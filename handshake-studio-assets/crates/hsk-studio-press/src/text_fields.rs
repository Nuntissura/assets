//! Slice 3: redaction of outline (bookmark) titles and annotation text, with per-object outcomes.
//!
//! Covered text slots (ISO 32000-2): outline item `/Title` (12.3.3, Table 151), annotation
//! `/Contents`, `/T`, `/Subj`, `/RC` (12.5.2 Table 166, 12.5.6.2 Table 170; `/RC` is a text string or
//! a text stream holding XHTML), and, only when `widget_values` is set, form field `/V` and `/DV`
//! (12.7.4.1 Table 226: text string, text stream, or array of text strings for choice fields).
//! `/T` of a widget or field is the field NAME (not annotation text): never rewritten, but a match
//! is reported as kept. The appearance streams of an annotation (`/AP`, 12.5.5) carry a rendering of
//! the same text, so an appearance whose show operands match is dropped (viewers regenerate it).
//!
//! Proof rule (same as slices 1-2, nothing is silently kept): every slot is either rewritten or
//! reported as `KeptByPolicy`; the independent check is `verify_payload_absent` over the saved
//! bytes (annotation appearance streams count as annotation text there, not as kept page text).
//! Outcomes never carry document text.
//!
//! Matching works on decoded text (PDFDocEncoding, UTF-16BE/LE, UTF-8 with mark), not on bytes, so
//! one needle matches all spellings. Defaults are fail-closed: case-insensitive, substring.
//! Slice 4 closes the remaining text carriers, each with its own outcomes: named destinations
//! (`/Dests`, `/Names /Dests`: names are identifiers, so a matching name is renamed to a unique opaque
//! `HskRdN` in the tree and in every `/Dest`, `/OpenAction` and GoTo `/D` reference; the name tree
//! is rebuilt as one sorted leaf), action URIs (`/S /URI` actions anywhere, plus the catalog
//! `/URI /Base`), form `/TU` and `/Opt`, structure-element `/Alt /ActualText /E /T`, trailer `/Info`
//! strings and the catalog XMP `/Metadata` stream, and annotations reached only through `/Popup`,
//! `/IRT` or a popup's `/Parent`. Appearance streams are read through the font's `/ToUnicode`
//! CMap; composite-font text that cannot be decoded drops the appearance of an edited annotation
//! and is otherwise reported `Unverifiable` (kept, never silent).
//! Not covered (the proof still reports them as hits, never hides them): `/GoToR` and `/Launch`
//! targets, other name trees (embedded file names), page/image `/Metadata`, structure elements that
//! are direct dictionaries without `/P`, text only decodable through a `/ToUnicode` that the proof
//! (a byte scan) cannot read.
use crate::cmap::{Cmap, parse_to_unicode};
use crate::content::parse_content;
use crate::error::{Limits, PressError, Result};
use crate::filters;
use crate::pdf_edit::PdfDoc;
use crate::reader::{Entry, check_cancel};
use crate::redaction::res;
use crate::sanitized_save::{PayloadProof, variants_text, verify_with_variants};
use crate::syntax::{Dict, ObjRef, Object, Stream};
use crate::text_codec::{Enc, decode_text, encode_text};
use crate::writer::{reachable, trailer_roots};
use hsk_studio_accord::CancellationToken;
use std::collections::{BTreeMap, BTreeSet};

type Objs = BTreeMap<u32, Entry>;
/// Name-tree or legacy-dictionary entries `(name bytes, value)`.
type NamePairs = Vec<(Vec<u8>, Object)>;

const MAX_NEEDLES: usize = 4096;
const MAX_NEEDLE_CHARS: usize = 1024;
const MAX_PASSES: usize = 8;
const MAX_AP_FORMS: usize = 256;

/// What replaces a matched span.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextMode {
    /// Each matched span becomes this marker (it must not itself match a needle).
    Replace(String),
    /// Each matched span is deleted.
    Remove,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextFieldPolicy {
    /// Outline item `/Title`.
    pub outline_titles: bool,
    /// Annotation `/Contents`, `/T`, `/Subj`, `/RC` and annotation appearance streams.
    pub annotation_text: bool,
    /// EXPLICIT option: form field `/V` and `/DV` (and the widget appearance). Off by default:
    /// matches there are reported as `KeptByPolicy`.
    pub widget_values: bool,
    /// Named destination names (`/Dests`, `/Names /Dests`) and every reference to them.
    pub destinations: bool,
    /// `/URI` of URI actions (`/A`, `/AA`, `/OpenAction`, `/Next` chains) and the catalog `/URI /Base`.
    pub action_uris: bool,
    /// Form field `/TU` (tooltip) and `/Opt` (choice options).
    pub form_labels: bool,
    /// Structure element `/Alt`, `/ActualText`, `/E`, `/T`.
    pub structure_text: bool,
    /// Trailer `/Info` string entries and the catalog `/Metadata` (XMP) stream.
    pub info_fields: bool,
    pub mode: TextMode,
    /// Simple case folding (one lowercase char per char). Default true (fail closed).
    pub case_insensitive: bool,
    /// The proof's whole-token rule: a needle edge that is an ASCII letter/digit must not touch
    /// another ASCII letter/digit. Default false (substring; over-removal is safe).
    pub whole_token: bool,
}

impl Default for TextFieldPolicy {
    fn default() -> Self {
        Self {
            outline_titles: true,
            annotation_text: true,
            widget_values: false,
            destinations: true,
            action_uris: true,
            form_labels: true,
            structure_text: true,
            info_fields: true,
            mode: TextMode::Replace("[REDACTED]".into()),
            case_insensitive: true,
            whole_token: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextRole {
    OutlineTitle,
    AnnotContents,
    AnnotTitle,
    AnnotRichText,
    AnnotSubject,
    FieldValue,
    FieldDefaultValue,
    /// `/T` of a form field or widget: a name, never rewritten.
    FieldName,
    /// `/AP` appearance streams of an annotation.
    Appearance,
    /// A named destination name (tree key, legacy `/Dests` key) or a reference to one.
    DestName,
    /// `/URI` of a URI action (or the catalog `/URI /Base`).
    ActionUri,
    /// Form field `/TU`.
    FieldTooltip,
    /// Form field `/Opt`.
    FieldOption,
    StructAlt,
    StructActualText,
    StructExpansion,
    StructTitle,
    /// A string entry of the trailer `/Info` dictionary.
    InfoField,
    /// The catalog `/Metadata` (XMP) stream.
    Xmp,
}

impl TextRole {
    pub const fn key(self) -> &'static str {
        match self {
            Self::OutlineTitle => "Title",
            Self::AnnotContents => "Contents",
            Self::AnnotTitle | Self::FieldName => "T",
            Self::AnnotRichText => "RC",
            Self::AnnotSubject => "Subj",
            Self::FieldValue => "V",
            Self::FieldDefaultValue => "DV",
            Self::Appearance => "AP",
            Self::DestName => "Dests",
            Self::ActionUri => "URI",
            Self::FieldTooltip => "TU",
            Self::FieldOption => "Opt",
            Self::StructAlt => "Alt",
            Self::StructActualText => "ActualText",
            Self::StructExpansion => "E",
            Self::StructTitle => "T",
            Self::InfoField => "Info",
            Self::Xmp => "Metadata",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextOutcomeKind {
    /// Matched spans were replaced by the marker.
    Replaced,
    /// Matched spans were deleted.
    Removed,
    /// The whole key was removed: rich text or an appearance that contains a match, or text that
    /// could not be read or rewritten (`unreadable`).
    Dropped,
    /// A match exists and the policy forbids rewriting this slot: the text is KEPT.
    KeptByPolicy,
    /// An appearance whose text cannot be decoded (composite font without a usable `/ToUnicode`,
    /// undecodable stream) on an annotation that was not edited: KEPT, and the report is not
    /// `complete`.
    Unverifiable,
}

/// One rewritten (or deliberately kept) text slot. Carries no document text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextFieldOutcome {
    /// The object that holds the slot (for `direct` annotation dictionaries: the page or array
    /// object that holds the dictionary).
    pub object: ObjRef,
    pub direct: bool,
    pub role: TextRole,
    pub kind: TextOutcomeKind,
    /// Matched spans (0 when the slot could not be read).
    pub matches: u32,
    /// The slot's text could not be decoded or kept stable, so the key was dropped fail-closed.
    pub unreadable: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextFieldReport {
    pub outcomes: Vec<TextFieldOutcome>,
    pub outline_items_scanned: u32,
    pub annotations_scanned: u32,
    pub fields_scanned: u32,
    /// Matched spans removed or replaced, plus dropped slots' matches.
    pub matches_redacted: u32,
    pub kept_by_policy: u32,
    /// Appearances kept although their text could not be decoded.
    pub unverifiable: u32,
    /// Nothing was kept by policy and nothing was unverifiable: every matching slot in the enabled
    /// classes was redacted.
    pub complete: bool,
}

/// Output of [`redact_text_fields_and_save`].
#[derive(Clone, Debug, PartialEq)]
pub struct TextRedactedOutput {
    pub bytes: Vec<u8>,
    pub report: TextFieldReport,
    /// Independent proof over the saved bytes for the needles (ASCII needles also by their
    /// UTF-16BE spelling; other needles by PDFDocEncoding, UTF-16BE and UTF-8 spellings).
    pub proof: PayloadProof,
}

impl TextRedactedOutput {
    /// Nothing kept by policy, no leak hit anywhere, one xref section, no unreachable or unlisted
    /// objects.
    pub fn is_proven(&self) -> bool {
        self.report.complete && self.proof.is_clean()
    }
}

// ------------------------------------------------------------------ matching

fn fold(c: char, ci: bool) -> char {
    if ci { c.to_lowercase().next().unwrap_or(c) } else { c }
}

struct Matcher {
    /// Folded needles, longest first.
    needles: Vec<Vec<char>>,
    ci: bool,
    token: bool,
}

enum Rw {
    Clean,
    Changed(Vec<char>, u32),
    /// Still matching after `MAX_PASSES` rewrite passes.
    Unstable(u32),
}

fn boundary(hay: &[char], start: usize, end: usize, needle: &[char]) -> bool {
    let before = start == 0 || !hay[start - 1].is_ascii_alphanumeric() || !needle[0].is_ascii_alphanumeric();
    let after = end >= hay.len() || !hay[end].is_ascii_alphanumeric() || !needle[needle.len() - 1].is_ascii_alphanumeric();
    before && after
}

impl Matcher {
    fn new(needles: &[String], ci: bool, token: bool) -> Result<Self> {
        if needles.len() > MAX_NEEDLES {
            return Err(PressError::InputLimit("needles"));
        }
        let mut set: BTreeSet<Vec<char>> = BTreeSet::new();
        for n in needles {
            let folded: Vec<char> = n.trim().chars().map(|c| fold(c, ci)).collect();
            if folded.is_empty() {
                continue;
            }
            if folded.len() > MAX_NEEDLE_CHARS {
                return Err(PressError::InputLimit("needle_chars"));
            }
            set.insert(folded);
        }
        let mut needles: Vec<Vec<char>> = set.into_iter().collect();
        needles.sort_by_key(|n| std::cmp::Reverse(n.len()));
        Ok(Self { needles, ci, token })
    }

    /// Non-overlapping, leftmost, longest-first spans `[start, end)`.
    fn find(&self, hay: &[char]) -> Vec<(usize, usize)> {
        if self.needles.is_empty() {
            return Vec::new();
        }
        let folded: Vec<char> = hay.iter().map(|c| fold(*c, self.ci)).collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < folded.len() {
            let hit = self.needles.iter().find(|n| {
                let end = i + n.len();
                end <= folded.len() && folded[i..end] == n[..] && (!self.token || boundary(hay, i, end, n))
            });
            match hit {
                Some(n) => {
                    out.push((i, i + n.len()));
                    i += n.len();
                }
                None => i += 1,
            }
        }
        out
    }

    fn rewrite(&self, hay: &[char], mode: &TextMode) -> Rw {
        let mut cur = hay.to_vec();
        let mut total = 0u32;
        for _ in 0..MAX_PASSES {
            let found = self.find(&cur);
            if found.is_empty() {
                return if total == 0 { Rw::Clean } else { Rw::Changed(cur, total) };
            }
            total = total.saturating_add(found.len() as u32);
            let mut next = Vec::with_capacity(cur.len());
            let mut at = 0;
            for (s, e) in found {
                next.extend_from_slice(&cur[at..s]);
                if let TextMode::Replace(marker) = mode {
                    next.extend(marker.chars());
                }
                at = e;
            }
            next.extend_from_slice(&cur[at..]);
            cur = next;
        }
        Rw::Unstable(total)
    }

    /// Matches in XHTML-ish rich text, judged on three spellings so markup cannot split a needle:
    /// the raw text, the text with tags removed, and the text with tags replaced by a space.
    fn rich_matches(&self, chars: &[char]) -> u32 {
        let spellings = [
            collapse_ws(&decode_entities(chars)),
            collapse_ws(&decode_entities(&strip_markup(chars, false))),
            collapse_ws(&decode_entities(&strip_markup(chars, true))),
        ];
        spellings.iter().map(|v| self.find(v).len() as u32).max().unwrap_or(0)
    }
}

fn strip_markup(chars: &[char], gap: bool) -> Vec<char> {
    let mut out = Vec::with_capacity(chars.len());
    let mut in_tag = false;
    for &c in chars {
        match (in_tag, c) {
            (false, '<') => in_tag = true,
            (true, '>') => {
                in_tag = false;
                if gap {
                    out.push(' ');
                }
            }
            (true, _) => {}
            (false, _) => out.push(c),
        }
    }
    out
}

fn decode_entities(chars: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '&'
            && let Some(len) = chars[i + 1..].iter().take(12).position(|c| *c == ';')
        {
            let name: String = chars[i + 1..i + 1 + len].iter().collect();
            let decoded = match name.as_str() {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some('\u{A0}'),
                n => n
                    .strip_prefix("#x")
                    .or_else(|| n.strip_prefix("#X"))
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                    .or_else(|| n.strip_prefix('#').and_then(|d| d.parse::<u32>().ok()))
                    .and_then(char::from_u32),
            };
            if let Some(c) = decoded {
                out.push(c);
                i += len + 2;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn collapse_ws(chars: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(chars.len());
    let mut space = false;
    for &c in chars {
        if c.is_whitespace() {
            space = true;
        } else {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.push(c);
        }
    }
    out
}

/// Rich-text bytes: byte-order marks decide, otherwise UTF-8 (XML default).
fn decode_rich(bytes: &[u8]) -> Vec<char> {
    if bytes.starts_with(&[0xFE, 0xFF]) || bytes.starts_with(&[0xFF, 0xFE]) {
        decode_text(bytes).0
    } else {
        String::from_utf8_lossy(bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)).chars().collect()
    }
}

// ---------------------------------------------------------------- appearance

/// Appearance streams of one annotation dictionary: `/AP` `/N` `/R` `/D`, each a stream or a
/// dictionary of state streams (12.5.5), plus form XObjects those streams draw (bounded).
pub(crate) fn appearance_streams_of(objs: &Objs, d: &Dict) -> BTreeSet<u32> {
    fn state_streams(objs: &Objs, states: &Dict, out: &mut BTreeSet<u32>) {
        for (_, v) in &states.entries {
            if let Object::Ref(r) = v
                && matches!(objs.get(&r.num), Some(Entry { obj: Object::Stream(_), .. }))
            {
                out.insert(r.num);
            }
        }
    }
    let mut out = BTreeSet::new();
    let Some(ap) = d.get("AP").map(|a| res(objs, a)).and_then(Object::as_dict) else {
        return out;
    };
    for key in ["N", "R", "D"] {
        match ap.get(key) {
            Some(Object::Ref(r)) => match objs.get(&r.num).map(|e| &e.obj) {
                Some(Object::Stream(_)) => {
                    out.insert(r.num);
                }
                Some(Object::Dict(states)) => state_streams(objs, states, &mut out),
                _ => {}
            },
            Some(Object::Dict(states)) => state_streams(objs, states, &mut out),
            _ => {}
        }
    }
    let mut work: Vec<u32> = out.iter().copied().collect();
    while let Some(n) = work.pop() {
        let Some(Entry { obj: Object::Stream(s), .. }) = objs.get(&n) else { continue };
        let forms = s.dict.get("Resources").map(|r| res(objs, r)).and_then(Object::as_dict).and_then(|r| r.get("XObject")).map(|x| res(objs, x));
        if let Some(Object::Dict(xo)) = forms {
            for (_, v) in &xo.entries {
                if let Object::Ref(r) = v
                    && out.len() < MAX_AP_FORMS
                    && matches!(objs.get(&r.num), Some(Entry { obj: Object::Stream(s), .. }) if s.dict.get("Subtype").is_some_and(|t| t.is_name("Form")))
                    && out.insert(r.num)
                {
                    work.push(r.num);
                }
            }
        }
    }
    out
}

/// Every stream that is (part of) an annotation appearance, over all objects. The payload proof
/// judges these as annotation text, not as kept page text.
pub(crate) fn appearance_stream_nums(objs: &Objs) -> BTreeSet<u32> {
    let mut out = BTreeSet::new();
    for entry in objs.values() {
        let mut stack: Vec<&Object> = vec![&entry.obj];
        while let Some(o) = stack.pop() {
            match o {
                Object::Array(a) => stack.extend(a.iter()),
                Object::Dict(d) => {
                    if d.get("AP").is_some() {
                        out.extend(appearance_streams_of(objs, d));
                    }
                    stack.extend(d.entries.iter().map(|(_, v)| v));
                }
                Object::Stream(s) => {
                    if s.dict.get("AP").is_some() {
                        out.extend(appearance_streams_of(objs, &s.dict));
                    }
                    stack.extend(s.dict.entries.iter().map(|(_, v)| v));
                }
                _ => {}
            }
        }
    }
    out
}

// ------------------------------------------------------------------- engine

enum Rewrite {
    /// Not a text value (null, number, dictionary, missing).
    NotText,
    Clean,
    /// Replacement as a DIRECT value (shared string objects are never edited in place).
    Changed { value: Object, matches: u32 },
    Dropped { matches: u32, unreadable: bool },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotKind {
    /// Text string, or text stream.
    Plain,
    /// `/RC`, `/Metadata`: text string or markup stream; any match drops the whole key.
    Rich,
    /// `/V`, `/DV`, `/Opt`: Plain, or an array of text strings (arrays of arrays one level deep).
    Multi,
}

struct Slot {
    key: String,
    role: TextRole,
    kind: SlotKind,
    enabled: bool,
}

impl Slot {
    fn new(key: &str, role: TextRole, kind: SlotKind, enabled: bool) -> Self {
        Self { key: key.to_owned(), role, kind, enabled }
    }
}

#[derive(Clone, Copy)]
struct Loc {
    holder: ObjRef,
    direct: bool,
}

enum Applied {
    Nothing,
    Kept,
    Edited,
}

/// Which roles reached an object.
#[derive(Default, Clone, Copy)]
struct Target {
    outline: bool,
    annot: bool,
    field: bool,
    info: bool,
    catalog: bool,
}

/// How the show-operator strings of one font are read.
enum FontText {
    /// Single-byte codes read as Latin-1 (what the byte-level proof sees).
    Simple,
    /// `/ToUnicode` decodes the codes; non-composite fonts are also read as Latin-1.
    Cmap { cmap: Cmap, composite: bool },
    /// Composite font without a usable `/ToUnicode`: the codes cannot be read.
    Opaque,
}

/// Unique opaque replacements for named-destination names, shared by the tree, the legacy
/// dictionary and every reference.
#[derive(Default)]
struct DestRename {
    strs: BTreeMap<Vec<u8>, Vec<u8>>,
    names: BTreeMap<Vec<u8>, Vec<u8>>,
    /// Names that must not be issued (existing tree and dictionary keys, issued names).
    taken: BTreeSet<Vec<u8>>,
    counter: u32,
}

impl DestRename {
    fn rename(&mut self, old: &[u8], is_name: bool) -> Vec<u8> {
        if let Some(n) = if is_name { self.names.get(old) } else { self.strs.get(old) } {
            return n.clone();
        }
        let new = loop {
            let c = format!("HskRd{}", self.counter).into_bytes();
            self.counter += 1;
            if !self.taken.contains(&c) {
                break c;
            }
        };
        self.taken.insert(new.clone());
        let map = if is_name { &mut self.names } else { &mut self.strs };
        map.insert(old.to_vec(), new.clone());
        new
    }
}

/// Text of a destination name: a name object is UTF-8 bytes, a string is a PDF text string.
fn name_text(bytes: &[u8], is_name: bool) -> Vec<char> {
    if is_name { String::from_utf8_lossy(bytes).chars().collect() } else { decode_text(bytes).0 }
}

struct Ctx<'a> {
    matcher: &'a Matcher,
    policy: &'a TextFieldPolicy,
    limits: &'a Limits,
    budget: usize,
}

impl Ctx<'_> {
    fn stream_bytes(&mut self, st: &Stream) -> Result<Option<Vec<u8>>> {
        if !filters::is_decodable(&st.dict) {
            return Ok(None);
        }
        match filters::decode_stream(st, self.limits, &mut self.budget) {
            Ok(b) => Ok(Some(b)),
            Err(e @ (PressError::InputLimit(_) | PressError::Canceled)) => Err(e),
            Err(_) => Ok(None),
        }
    }

    fn plain(&self, chars: &[char], enc: Enc) -> Rewrite {
        match self.matcher.rewrite(chars, &self.policy.mode) {
            Rw::Clean => Rewrite::Clean,
            Rw::Changed(c, matches) => Rewrite::Changed { value: Object::Str(encode_text(&c, enc)), matches },
            Rw::Unstable(matches) => Rewrite::Dropped { matches, unreadable: true },
        }
    }

    fn rich(&self, chars: &[char]) -> Rewrite {
        match self.matcher.rich_matches(chars) {
            0 => Rewrite::Clean,
            matches => Rewrite::Dropped { matches, unreadable: false },
        }
    }

    fn rewrite_items(&self, objs: &Objs, items: &[Object], depth: usize) -> Rewrite {
        let (mut out, mut total, mut changed, mut dropped) = (Vec::with_capacity(items.len()), 0u32, false, false);
        for it in items {
            let part = match res(objs, it) {
                Object::Str(s) => {
                    let (chars, enc) = decode_text(s);
                    self.plain(&chars, enc)
                }
                Object::Array(inner) if depth < 2 => self.rewrite_items(objs, inner, depth + 1),
                _ => Rewrite::NotText,
            };
            match part {
                Rewrite::Changed { value, matches } => {
                    out.push(value);
                    total = total.saturating_add(matches);
                    changed = true;
                }
                Rewrite::Dropped { matches, .. } => {
                    total = total.saturating_add(matches);
                    dropped = true;
                    out.push(it.clone());
                }
                Rewrite::NotText | Rewrite::Clean => out.push(it.clone()),
            }
        }
        if dropped {
            Rewrite::Dropped { matches: total, unreadable: true }
        } else if changed {
            Rewrite::Changed { value: Object::Array(out), matches: total }
        } else {
            Rewrite::Clean
        }
    }

    fn rewrite_value(&mut self, objs: &Objs, v: &Object, kind: SlotKind) -> Result<Rewrite> {
        Ok(match res(objs, v) {
            Object::Str(s) => {
                if kind == SlotKind::Rich {
                    self.rich(&decode_text(s).0)
                } else {
                    let (chars, enc) = decode_text(s);
                    self.plain(&chars, enc)
                }
            }
            Object::Stream(st) => match self.stream_bytes(st)? {
                None => Rewrite::Dropped { matches: 0, unreadable: true },
                Some(data) if kind == SlotKind::Rich => self.rich(&decode_rich(&data)),
                Some(data) => {
                    let (chars, enc) = decode_text(&data);
                    self.plain(&chars, enc)
                }
            },
            Object::Array(items) if kind == SlotKind::Multi => self.rewrite_items(objs, items, 0),
            _ => Rewrite::NotText,
        })
    }

    fn slot(&mut self, objs: &Objs, d: &mut Dict, s: &Slot, loc: Loc, out: &mut Vec<TextFieldOutcome>) -> Result<Applied> {
        let Some(v) = d.get(&s.key) else { return Ok(Applied::Nothing) };
        let rw = self.rewrite_value(objs, v, s.kind)?;
        let (matches, unreadable) = match &rw {
            Rewrite::Changed { matches, .. } => (*matches, false),
            Rewrite::Dropped { matches, unreadable } => (*matches, *unreadable),
            Rewrite::NotText | Rewrite::Clean => return Ok(Applied::Nothing),
        };
        let mut push = |kind| out.push(TextFieldOutcome { object: loc.holder, direct: loc.direct, role: s.role, kind, matches, unreadable });
        if !s.enabled {
            if matches > 0 {
                push(TextOutcomeKind::KeptByPolicy);
                return Ok(Applied::Kept);
            }
            return Ok(Applied::Nothing);
        }
        match rw {
            Rewrite::Changed { value, .. } => {
                d.set(&s.key, value);
                push(if matches!(self.policy.mode, TextMode::Remove) { TextOutcomeKind::Removed } else { TextOutcomeKind::Replaced });
            }
            _ => {
                d.remove(&s.key);
                push(TextOutcomeKind::Dropped);
            }
        }
        Ok(Applied::Edited)
    }

    fn font_text(&mut self, objs: &Objs, fonts: Option<&Dict>, name: &[u8]) -> Result<FontText> {
        let Some(fd) = fonts.and_then(|f| f.get_key(name)).map(|v| res(objs, v)).and_then(Object::as_dict) else {
            return Ok(FontText::Simple);
        };
        let composite = fd.get("Subtype").is_some_and(|s| s.is_name("Type0"));
        if let Some(Object::Stream(tu)) = fd.get("ToUnicode").map(|v| res(objs, v))
            && let Some(data) = self.stream_bytes(tu)?
            && let Some(cmap) = parse_to_unicode(&data)
        {
            return Ok(FontText::Cmap { cmap, composite });
        }
        Ok(if composite { FontText::Opaque } else { FontText::Simple })
    }

    /// Text shown by the show operators of a form/content stream, in order, one space between
    /// operators; codes are read through the font's `/ToUnicode` where there is one. The flag is set
    /// when text drawn with a composite font that cannot be decoded was seen.
    fn content_text(&mut self, objs: &Objs, st: &Stream, data: &[u8]) -> Result<(Vec<char>, bool)> {
        fn strings<'o>(o: &'o Object, out: &mut Vec<&'o [u8]>) {
            match o {
                Object::Str(s) => out.push(s),
                Object::Array(a) => a.iter().for_each(|x| strings(x, out)),
                _ => {}
            }
        }
        let latin1 = |s: &[u8]| s.iter().map(|b| char::from(*b)).collect::<Vec<char>>();
        let Ok(ops) = parse_content(data, self.limits) else {
            return Ok((latin1(data), false));
        };
        let fonts = st
            .dict
            .get("Resources")
            .map(|r| res(objs, r))
            .and_then(Object::as_dict)
            .and_then(|r| r.get("Font"))
            .map(|f| res(objs, f))
            .and_then(Object::as_dict);
        let mut cache: BTreeMap<Vec<u8>, FontText> = BTreeMap::new();
        let (mut out, mut opaque, mut cur) = (Vec::new(), false, None::<Vec<u8>>);
        for op in &ops {
            match op.name.as_slice() {
                b"Tf" => cur = op.operands.first().and_then(Object::as_name).map(<[u8]>::to_vec),
                b"Tj" | b"TJ" | b"'" | b"\"" => {
                    if let Some(n) = &cur
                        && !cache.contains_key(n)
                    {
                        let f = self.font_text(objs, fonts, n)?;
                        cache.insert(n.clone(), f);
                    }
                    let font = cur.as_ref().and_then(|n| cache.get(n));
                    let mut strs = Vec::new();
                    op.operands.iter().for_each(|o| strings(o, &mut strs));
                    for s in strs {
                        match font {
                            Some(FontText::Cmap { cmap, composite }) => {
                                out.extend(cmap.decode(s));
                                if !composite {
                                    out.push(' ');
                                    out.extend(latin1(s));
                                }
                            }
                            Some(FontText::Opaque) => {
                                opaque |= !s.is_empty();
                                out.extend(latin1(s));
                            }
                            _ => out.extend(latin1(s)),
                        }
                    }
                    out.push(' ');
                }
                _ => {}
            }
        }
        Ok((out, opaque))
    }

    /// `(matches in appearance text, some appearance could not be read)`.
    fn appearance_scan(&mut self, objs: &Objs, d: &Dict) -> Result<(u32, bool)> {
        let (mut matches, mut undecodable) = (0u32, false);
        for n in appearance_streams_of(objs, d) {
            let Some(Entry { obj: Object::Stream(st), .. }) = objs.get(&n) else { continue };
            match self.stream_bytes(st)? {
                None => undecodable = true,
                Some(data) => {
                    let (text, opaque) = self.content_text(objs, st, &data)?;
                    undecodable |= opaque;
                    matches = matches.saturating_add(self.matcher.find(&text).len() as u32);
                }
            }
        }
        Ok((matches, undecodable))
    }

    /// Processes one dictionary reached as outline item, annotation, form field, `/Info` and/or
    /// catalog.
    fn dict(&mut self, objs: &Objs, d: &mut Dict, t: Target, loc: Loc, out: &mut Vec<TextFieldOutcome>) -> Result<bool> {
        let p = self.policy;
        let widget_like = t.field || d.get("FT").is_some() || d.get("Subtype").is_some_and(|s| s.is_name("Widget"));
        let mut slots: Vec<Slot> = Vec::new();
        if t.outline {
            slots.push(Slot::new("Title", TextRole::OutlineTitle, SlotKind::Plain, p.outline_titles));
        }
        if t.annot {
            slots.push(Slot::new("Contents", TextRole::AnnotContents, SlotKind::Plain, p.annotation_text));
            slots.push(Slot::new("Subj", TextRole::AnnotSubject, SlotKind::Plain, p.annotation_text));
            slots.push(Slot::new("RC", TextRole::AnnotRichText, SlotKind::Rich, p.annotation_text));
        }
        if t.annot || t.field {
            if widget_like {
                slots.push(Slot::new("T", TextRole::FieldName, SlotKind::Plain, false));
            } else {
                slots.push(Slot::new("T", TextRole::AnnotTitle, SlotKind::Plain, p.annotation_text));
            }
        }
        if widget_like {
            slots.push(Slot::new("V", TextRole::FieldValue, SlotKind::Multi, p.widget_values));
            slots.push(Slot::new("DV", TextRole::FieldDefaultValue, SlotKind::Multi, p.widget_values));
            slots.push(Slot::new("TU", TextRole::FieldTooltip, SlotKind::Plain, p.form_labels));
            slots.push(Slot::new("Opt", TextRole::FieldOption, SlotKind::Multi, p.form_labels));
        }
        if t.info {
            for (k, v) in &d.entries {
                if let (Ok(key), Object::Str(_)) = (std::str::from_utf8(k), res(objs, v)) {
                    slots.push(Slot::new(key, TextRole::InfoField, SlotKind::Plain, p.info_fields));
                }
            }
        }
        if t.catalog {
            slots.push(Slot::new("Metadata", TextRole::Xmp, SlotKind::Rich, p.info_fields));
        }
        let (mut changed, mut edited) = (false, false);
        for s in &slots {
            if let Applied::Edited = self.slot(objs, d, s, loc, out)? {
                changed = true;
                edited = true;
            }
        }
        if t.annot && (p.annotation_text || (widget_like && p.widget_values)) {
            let (matches, undecodable) = self.appearance_scan(objs, d)?;
            let enabled = !widget_like || p.widget_values;
            let outcome = |kind, unreadable| TextFieldOutcome { object: loc.holder, direct: loc.direct, role: TextRole::Appearance, kind, matches, unreadable };
            if enabled && (matches > 0 || (undecodable && edited)) {
                // The text of this annotation was redacted but its rendering still shows it (or
                // cannot be shown to be free of it): drop the appearance; viewers regenerate it.
                d.remove("AP");
                changed = true;
                out.push(outcome(TextOutcomeKind::Dropped, matches == 0));
            } else if enabled && undecodable {
                out.push(outcome(TextOutcomeKind::Unverifiable, true));
            } else if !enabled && matches > 0 {
                out.push(outcome(TextOutcomeKind::KeptByPolicy, false));
            }
        }
        Ok(changed)
    }

    /// Renames a named-destination reference (`/Dest`, `/OpenAction`, GoTo `/D`) that matches.
    fn rename_ref(&mut self, rn: &mut DestRename, d: &mut Dict, key: &str, loc: Loc, out: &mut Vec<TextFieldOutcome>) -> bool {
        let (bytes, is_name) = match d.get(key) {
            Some(Object::Str(s)) => (s.clone(), false),
            Some(Object::Name(n)) => (n.clone(), true),
            _ => return false,
        };
        let matches = self.matcher.find(&name_text(&bytes, is_name)).len() as u32;
        if matches == 0 {
            return false;
        }
        let mut push = |kind| out.push(TextFieldOutcome { object: loc.holder, direct: loc.direct, role: TextRole::DestName, kind, matches, unreadable: false });
        if !self.policy.destinations {
            push(TextOutcomeKind::KeptByPolicy);
            return false;
        }
        let new = rn.rename(&bytes, is_name);
        d.set(key, if is_name { Object::Name(new) } else { Object::Str(new) });
        push(TextOutcomeKind::Replaced);
        true
    }

    /// Slice 4 sweep over one reachable dictionary: destination references, URI actions, the
    /// catalog base URI and structure elements.
    fn sweep(&mut self, objs: &Objs, rn: &mut DestRename, d: &mut Dict, loc: Loc, out: &mut Vec<TextFieldOutcome>) -> Result<bool> {
        let p = self.policy;
        let mut changed = false;
        let s_name = d.get("S").and_then(Object::as_name).map(<[u8]>::to_vec);
        for key in ["Dest", "OpenAction"] {
            changed |= self.rename_ref(rn, d, key, loc, out);
        }
        if s_name.as_deref() == Some(b"GoTo") {
            changed |= self.rename_ref(rn, d, "D", loc, out);
        }
        if s_name.as_deref() == Some(b"URI") {
            let s = Slot::new("URI", TextRole::ActionUri, SlotKind::Plain, p.action_uris);
            changed |= matches!(self.slot(objs, d, &s, loc, out)?, Applied::Edited);
        }
        if d.get("Type").is_some_and(|t| t.is_name("Catalog"))
            && let Some(Object::Dict(u)) = d.get_mut("URI")
        {
            let s = Slot::new("Base", TextRole::ActionUri, SlotKind::Plain, p.action_uris);
            changed |= matches!(self.slot(objs, u, &s, loc, out)?, Applied::Edited);
        }
        if s_name.is_some() && (matches!(d.get("P"), Some(Object::Ref(_))) || d.get("Type").is_some_and(|t| t.is_name("StructElem"))) {
            for (key, role) in [
                ("Alt", TextRole::StructAlt),
                ("ActualText", TextRole::StructActualText),
                ("E", TextRole::StructExpansion),
                ("T", TextRole::StructTitle),
            ] {
                let s = Slot::new(key, role, SlotKind::Plain, p.structure_text);
                changed |= matches!(self.slot(objs, d, &s, loc, out)?, Applied::Edited);
            }
        }
        Ok(changed)
    }
}

/// Applies `f` to every dictionary nested in `o` (depth 0 = the object's own dictionary).
fn walk_dicts(o: &mut Object, depth: usize, f: &mut dyn FnMut(&mut Dict, usize) -> Result<bool>) -> Result<bool> {
    if depth > 64 {
        return Ok(false);
    }
    let mut changed = false;
    match o {
        Object::Dict(d) => {
            changed |= f(d, depth)?;
            for (_, v) in &mut d.entries {
                changed |= walk_dicts(v, depth + 1, f)?;
            }
        }
        Object::Stream(s) => {
            changed |= f(&mut s.dict, depth)?;
            for (_, v) in &mut s.dict.entries {
                changed |= walk_dicts(v, depth + 1, f)?;
            }
        }
        Object::Array(a) => {
            for v in a {
                changed |= walk_dicts(v, depth + 1, f)?;
            }
        }
        _ => {}
    }
    Ok(changed)
}

/// Applies `f` to the value reached from object `num` through dictionary keys `path`, following one
/// indirect reference per hop (an indirect value is replaced inside its own object). Returns the
/// number of the object that now holds the edited value.
fn edit_at(staged: &mut Objs, num: u32, path: &[&str], f: &mut dyn FnMut(&mut Object)) -> Option<u32> {
    let mut entry = staged.remove(&num)?;
    let (mut next_hop, mut done) = (None::<(u32, usize)>, false);
    {
        let mut cur: &mut Object = &mut entry.obj;
        let mut idx = 0;
        loop {
            let Some(key) = path.get(idx) else {
                f(cur);
                done = true;
                break;
            };
            let Object::Dict(d) = cur else { break };
            let Some(next) = d.get_mut(key) else { break };
            if let Object::Ref(r) = next {
                next_hop = Some((r.num, idx + 1));
                break;
            }
            cur = next;
            idx += 1;
        }
    }
    let holder = done.then_some(num);
    if done {
        entry.span = None;
    }
    staged.insert(num, entry);
    match next_hop {
        Some((n, idx)) => edit_at(staged, n, &path[idx..], f).or(holder),
        None => holder,
    }
}

/// Entries `(key, value)` of the catalog `/Names /Dests` name tree (every leaf, any depth) and the
/// number of the object that holds its root.
fn read_dest_tree(objs: &Objs, root_num: u32, max: usize) -> Result<Option<(u32, NamePairs)>> {
    let Some(cat) = objs.get(&root_num).and_then(|e| e.obj.as_dict()) else { return Ok(None) };
    let Some(names_v) = cat.get("Names") else { return Ok(None) };
    let Some(root_v) = res(objs, names_v).as_dict().and_then(|n| n.get("Dests")) else { return Ok(None) };
    let holder = match (root_v, names_v) {
        (Object::Ref(r), _) | (_, Object::Ref(r)) => r.num,
        _ => root_num,
    };
    let (mut pairs, mut seen) = (Vec::new(), BTreeSet::new());
    let mut stack: Vec<&Object> = vec![root_v];
    while let Some(v) = stack.pop() {
        if let Object::Ref(r) = v
            && !seen.insert(r.num)
        {
            continue;
        }
        if seen.len() > max {
            return Err(PressError::InputLimit("max_objects"));
        }
        let Some(node) = res(objs, v).as_dict() else { continue };
        if let Some(Object::Array(items)) = node.get("Names").map(|a| res(objs, a)) {
            for pair in items.chunks(2) {
                if let [k, val] = pair
                    && let Object::Str(ks) = res(objs, k)
                {
                    pairs.push((ks.clone(), val.clone()));
                }
            }
            if pairs.len() > max {
                return Err(PressError::InputLimit("max_objects"));
            }
        }
        if let Some(Object::Array(kids)) = node.get("Kids").map(|a| res(objs, a)) {
            stack.extend(kids.iter().rev());
        }
    }
    Ok(Some((holder, pairs)))
}

/// Entries of the legacy catalog `/Dests` dictionary and the number of the object that holds it.
fn read_legacy_dests(objs: &Objs, root_num: u32) -> Option<(u32, NamePairs)> {
    let cat = objs.get(&root_num).and_then(|e| e.obj.as_dict())?;
    let v = cat.get("Dests")?;
    let d = res(objs, v).as_dict()?;
    let holder = if let Object::Ref(r) = v { r.num } else { root_num };
    Some((holder, d.entries.clone()))
}

/// Renames matching destination names in the name tree (rebuilt as one sorted leaf) and in the
/// legacy dictionary. References are renamed by the sweep through the same `DestRename`.
fn redact_dest_names(ctx: &Ctx, staged: &mut Objs, root_num: u32, rn: &mut DestRename, out: &mut Vec<TextFieldOutcome>) -> Result<()> {
    let tree = read_dest_tree(staged, root_num, ctx.limits.max_objects)?;
    let legacy = read_legacy_dests(staged, root_num);
    let count = |bytes: &[u8], is_name: bool| ctx.matcher.find(&name_text(bytes, is_name)).len() as u32;
    let outcome = |holder: u32, kind, matches| TextFieldOutcome {
        object: ObjRef { num: holder, generation: 0 },
        direct: false,
        role: TextRole::DestName,
        kind,
        matches,
        unreadable: false,
    };
    if let Some((_, pairs)) = &tree {
        rn.taken.extend(pairs.iter().map(|(k, _)| k.clone()));
    }
    if let Some((_, entries)) = &legacy {
        rn.taken.extend(entries.iter().map(|(k, _)| k.clone()));
    }
    if let Some((holder, pairs)) = tree {
        let hits: u32 = pairs.iter().map(|(k, _)| count(k, false)).sum();
        if hits > 0 && !ctx.policy.destinations {
            out.push(outcome(holder, TextOutcomeKind::KeptByPolicy, hits));
        } else if hits > 0 {
            let mut renamed: Vec<(Vec<u8>, Object)> =
                pairs.into_iter().map(|(k, v)| if count(&k, false) > 0 { (rn.rename(&k, false), v) } else { (k, v) }).collect();
            renamed.sort_by(|a, b| a.0.cmp(&b.0));
            renamed.dedup_by(|later, first| later.0 == first.0);
            let arr: Vec<Object> = renamed.into_iter().flat_map(|(k, v)| [Object::Str(k), v]).collect();
            let mut root = Dict::default();
            root.set("Names", Object::Array(arr));
            let new_root = Object::Dict(root);
            let at = edit_at(staged, root_num, &["Names", "Dests"], &mut |o| *o = new_root.clone()).unwrap_or(holder);
            out.push(outcome(at, TextOutcomeKind::Replaced, hits));
        }
    }
    if let Some((holder, entries)) = legacy {
        let hits: u32 = entries.iter().map(|(k, _)| count(k, true)).sum();
        if hits > 0 && !ctx.policy.destinations {
            out.push(outcome(holder, TextOutcomeKind::KeptByPolicy, hits));
        } else if hits > 0 {
            let renamed: Vec<(Vec<u8>, Object)> =
                entries.into_iter().map(|(k, v)| if count(&k, true) > 0 { (rn.rename(&k, true), v) } else { (k, v) }).collect();
            let new_dict = Object::Dict(Dict { entries: renamed });
            let at = edit_at(staged, root_num, &["Dests"], &mut |o| *o = new_dict.clone()).unwrap_or(holder);
            out.push(outcome(at, TextOutcomeKind::Replaced, hits));
        }
    }
    Ok(())
}

/// Outline items: everything reachable from `/Outlines` through `/First /Last /Next /Prev` (the
/// tree may be inconsistent; every linked node is judged). `/Parent` is never followed.
fn outline_nums(doc: &PdfDoc, cancel: &CancellationToken) -> Result<BTreeSet<u32>> {
    let mut stack: Vec<u32> = match doc.catalog()?.get("Outlines") {
        Some(Object::Ref(r)) => vec![r.num],
        Some(Object::Dict(d)) => ["First", "Last"].iter().filter_map(|k| d.get(k).and_then(Object::as_ref)).map(|r| r.num).collect(),
        _ => Vec::new(),
    };
    let mut seen = BTreeSet::new();
    while let Some(n) = stack.pop() {
        if !seen.insert(n) {
            continue;
        }
        if seen.len() % 256 == 0 {
            check_cancel(cancel)?;
        }
        if seen.len() > doc.limits.max_objects {
            return Err(PressError::InputLimit("max_objects"));
        }
        let Some(d) = doc.dict_of(n) else { continue };
        for k in ["First", "Last", "Next", "Prev"] {
            if let Some(Object::Ref(r)) = d.get(k) {
                stack.push(r.num);
            }
        }
    }
    Ok(seen)
}

/// The array of annotations held by a page (`direct`: the page dictionary holds the array) or by
/// an array object.
fn annot_items(obj: &mut Object, page_holder: bool) -> Option<&mut Vec<Object>> {
    match obj {
        Object::Array(items) if !page_holder => Some(items),
        Object::Dict(d) if page_holder => match d.get_mut("Annots") {
            Some(Object::Array(items)) => Some(items),
            _ => None,
        },
        _ => None,
    }
}

impl PdfDoc {
    /// Redacts `needles` (Unicode text) from outline titles, annotation text, named destinations,
    /// URI actions, form labels, structure text and document information per `policy`, as one
    /// atomic in-memory change (the document is untouched on error). Call `save_full` (or use
    /// [`redact_text_fields_and_save`]) afterwards; nothing is proven until the saved bytes pass
    /// `verify_payload_absent`.
    pub fn redact_text_fields(&mut self, needles: &[String], policy: &TextFieldPolicy, cancel: &CancellationToken) -> Result<TextFieldReport> {
        self.guard_mutation(cancel)?;
        let matcher = Matcher::new(needles, policy.case_insensitive, policy.whole_token)?;
        if let TextMode::Replace(marker) = &policy.mode {
            let m: Vec<char> = marker.chars().collect();
            if !matcher.find(&m).is_empty() {
                return Err(PressError::Malformed("text_marker_matches_needle"));
            }
        }

        // 1. Collect targets (read-only).
        let mut targets: BTreeMap<u32, Target> = BTreeMap::new();
        for n in outline_nums(self, cancel)? {
            targets.entry(n).or_default().outline = true;
        }
        // holder object number -> true when the holder is a page dictionary
        let mut direct_holders: BTreeMap<u32, bool> = BTreeMap::new();
        for page in &self.pages {
            check_cancel(cancel)?;
            let Some(av) = self.dict_of(*page).and_then(|d| d.get("Annots")) else { continue };
            let (holder, items) = match (av, self.resolve(av)) {
                (Object::Array(a), _) => ((*page, true), a),
                (Object::Ref(r), Object::Array(a)) => ((r.num, false), a),
                _ => continue,
            };
            for item in items {
                match item {
                    Object::Ref(r) => targets.entry(r.num).or_default().annot = true,
                    Object::Dict(_) => {
                        direct_holders.insert(holder.0, holder.1);
                    }
                    _ => {}
                }
            }
        }
        // Annotations reached only through /Popup, /IRT or a popup's /Parent.
        let mut queue: Vec<u32> = targets.iter().filter(|(_, t)| t.annot).map(|(n, _)| *n).collect();
        while let Some(n) = queue.pop() {
            let Some(d) = self.dict_of(n) else { continue };
            for k in ["Popup", "IRT", "Parent"] {
                let Some(Object::Ref(r)) = d.get(k) else { continue };
                let is_annot = self.dict_of(r.num).is_some_and(|x| x.get("Subtype").is_some());
                if is_annot && !targets.get(&r.num).is_some_and(|t| t.annot) {
                    targets.entry(r.num).or_default().annot = true;
                    queue.push(r.num);
                    if targets.len() > self.limits.max_objects {
                        return Err(PressError::InputLimit("max_objects"));
                    }
                }
            }
        }
        let form = self.catalog()?.get("AcroForm").map(|a| self.resolve(a)).and_then(Object::as_dict);
        if let Some(Object::Array(roots)) = form.and_then(|f| f.get("Fields")).map(|f| self.resolve(f)) {
            let mut stack: Vec<(u32, usize)> = roots.iter().filter_map(Object::as_ref).map(|r| (r.num, 1)).collect();
            let mut seen = BTreeSet::new();
            while let Some((n, depth)) = stack.pop() {
                if depth > self.limits.max_depth {
                    return Err(PressError::InputLimit("depth"));
                }
                if !seen.insert(n) {
                    continue;
                }
                if seen.len() > self.limits.max_objects {
                    return Err(PressError::InputLimit("max_objects"));
                }
                targets.entry(n).or_default().field = true;
                if let Some(Object::Array(kids)) = self.dict_of(n).and_then(|d| d.get("Kids")).map(|k| self.resolve(k)) {
                    stack.extend(kids.iter().filter_map(Object::as_ref).map(|r| (r.num, depth + 1)));
                }
            }
        }
        let root_num = self.trailer.get("Root").and_then(Object::as_ref).ok_or(PressError::Malformed("no_root"))?.num;
        targets.entry(root_num).or_default().catalog = true;
        if let Some(info) = self.trailer.get("Info").and_then(Object::as_ref) {
            targets.entry(info.num).or_default().info = true;
        }

        // 2. Rewrite on a staged copy; commit at the end.
        let mut staged = self.objects.clone();
        let mut ctx = Ctx { matcher: &matcher, policy, limits: &self.limits, budget: self.limits.max_decoded_total };
        let mut report = TextFieldReport::default();
        for (i, (num, t)) in targets.into_iter().enumerate() {
            if i % 256 == 0 {
                check_cancel(cancel)?;
            }
            let Some(mut entry) = staged.remove(&num) else { continue };
            let loc = Loc { holder: ObjRef { num, generation: entry.generation }, direct: false };
            let changed = match &mut entry.obj {
                Object::Dict(d) => {
                    report.outline_items_scanned += u32::from(t.outline);
                    report.annotations_scanned += u32::from(t.annot);
                    report.fields_scanned += u32::from(t.field);
                    ctx.dict(&staged, d, t, loc, &mut report.outcomes)?
                }
                _ => false,
            };
            if changed {
                entry.span = None;
            }
            staged.insert(num, entry);
        }
        for (holder, is_page) in direct_holders {
            check_cancel(cancel)?;
            let Some(mut entry) = staged.remove(&holder) else { continue };
            let loc = Loc { holder: ObjRef { num: holder, generation: entry.generation }, direct: true };
            let mut changed = false;
            if let Some(items) = annot_items(&mut entry.obj, is_page) {
                for item in items.iter_mut() {
                    if let Object::Dict(d) = item {
                        report.annotations_scanned += 1;
                        changed |= ctx.dict(&staged, d, Target { annot: true, ..Target::default() }, loc, &mut report.outcomes)?;
                    }
                }
            }
            if changed {
                entry.span = None;
            }
            staged.insert(holder, entry);
        }

        // 3. Named destinations: the tree and the legacy dictionary, then every reference below.
        let mut rn = DestRename::default();
        redact_dest_names(&ctx, &mut staged, root_num, &mut rn, &mut report.outcomes)?;

        // 4. Sweep every reachable object: destination references, URI actions, structure text.
        let live = reachable(&staged, &trailer_roots(&self.trailer));
        for (i, num) in live.into_iter().enumerate() {
            if i % 256 == 0 {
                check_cancel(cancel)?;
            }
            let Some(mut entry) = staged.remove(&num) else { continue };
            let generation = entry.generation;
            let changed = walk_dicts(&mut entry.obj, 0, &mut |d, depth| {
                let loc = Loc { holder: ObjRef { num, generation }, direct: depth > 0 };
                ctx.sweep(&staged, &mut rn, d, loc, &mut report.outcomes)
            })?;
            if changed {
                entry.span = None;
            }
            staged.insert(num, entry);
        }

        let count = |k: TextOutcomeKind| report.outcomes.iter().filter(|o| o.kind == k).count() as u32;
        report.kept_by_policy = count(TextOutcomeKind::KeptByPolicy);
        report.unverifiable = count(TextOutcomeKind::Unverifiable);
        report.matches_redacted = report
            .outcomes
            .iter()
            .filter(|o| !matches!(o.kind, TextOutcomeKind::KeptByPolicy | TextOutcomeKind::Unverifiable))
            .fold(0u32, |a, o| a.saturating_add(o.matches));
        report.complete = report.kept_by_policy == 0 && report.unverifiable == 0;
        self.objects = staged;
        Ok(report)
    }
}

/// Redacts `needles` from outline titles, annotation text and the other covered text carriers,
/// full-saves, and verifies the saved bytes with the independent payload proof. On error the
/// document is restored.
pub fn redact_text_fields_and_save(
    doc: &mut PdfDoc,
    needles: &[String],
    policy: &TextFieldPolicy,
    cancel: &CancellationToken,
) -> Result<TextRedactedOutput> {
    let backup = doc.objects.clone();
    let result = (|| -> Result<TextRedactedOutput> {
        let report = doc.redact_text_fields(needles, policy, cancel)?;
        let bytes = doc.save_full(cancel)?;
        let proof = verify_with_variants(&bytes, variants_text(needles), &doc.limits, cancel)?;
        Ok(TextRedactedOutput { bytes, report, proof })
    })();
    if result.is_err() {
        doc.objects = backup;
    }
    result
}
