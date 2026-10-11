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
//! Not covered (the proof still reports them as hits, never hides them): named-destination names,
//! action URIs, field options, tooltips, structure-tree text, text drawn inside nested appearance
//! forms that use composite-font codes.
use crate::content::parse_content;
use crate::error::{Limits, PressError, Result};
use crate::filters;
use crate::pdf_edit::PdfDoc;
use crate::reader::{Entry, check_cancel};
use crate::redaction::res;
use crate::sanitized_save::{PayloadProof, variants_text, verify_with_variants};
use crate::syntax::{Dict, ObjRef, Object, Stream};
use crate::text_codec::{Enc, decode_text, encode_text};
use hsk_studio_accord::CancellationToken;
use std::collections::{BTreeMap, BTreeSet};

type Objs = BTreeMap<u32, Entry>;

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
    /// Nothing was kept by policy: every matching slot in the enabled classes was redacted.
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

/// Text shown by the show operators of a content stream, in order, one space between operators;
/// strings are read byte-for-byte (the proof's raw-byte view). Unparsable content falls back to
/// its raw bytes.
fn content_text(data: &[u8], limits: &Limits) -> Vec<char> {
    fn push(o: &Object, out: &mut Vec<char>) {
        match o {
            Object::Str(s) => out.extend(s.iter().map(|b| char::from(*b))),
            Object::Array(a) => a.iter().for_each(|x| push(x, out)),
            _ => {}
        }
    }
    let Ok(ops) = parse_content(data, limits) else {
        return data.iter().map(|b| char::from(*b)).collect();
    };
    let mut out = Vec::new();
    for op in ops.iter().filter(|op| matches!(op.name.as_slice(), b"Tj" | b"TJ" | b"'" | b"\"")) {
        op.operands.iter().for_each(|o| push(o, &mut out));
        out.push(' ');
    }
    out
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
    /// `/RC`: text string or XHTML stream; any match drops the whole key.
    Rich,
    /// `/V`, `/DV`: Plain, or an array of text strings.
    Multi,
}

struct Slot {
    key: &'static str,
    role: TextRole,
    kind: SlotKind,
    enabled: bool,
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
            Object::Array(items) if kind == SlotKind::Multi => {
                let (mut out, mut total, mut changed, mut dropped) = (Vec::with_capacity(items.len()), 0u32, false, false);
                for it in items {
                    let (chars, enc) = match res(objs, it) {
                        Object::Str(s) => decode_text(s),
                        _ => {
                            out.push(it.clone());
                            continue;
                        }
                    };
                    match self.plain(&chars, enc) {
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
                        _ => out.push(it.clone()),
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
            _ => Rewrite::NotText,
        })
    }

    fn slot(&mut self, objs: &Objs, d: &mut Dict, s: &Slot, loc: Loc, out: &mut Vec<TextFieldOutcome>) -> Result<Applied> {
        let Some(v) = d.get(s.key) else { return Ok(Applied::Nothing) };
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
                d.set(s.key, value);
                push(if matches!(self.policy.mode, TextMode::Remove) { TextOutcomeKind::Removed } else { TextOutcomeKind::Replaced });
            }
            _ => {
                d.remove(s.key);
                push(TextOutcomeKind::Dropped);
            }
        }
        Ok(Applied::Edited)
    }

    /// `(matches in show operands, some appearance stream was unreadable)`.
    fn appearance_scan(&mut self, objs: &Objs, d: &Dict) -> Result<(u32, bool)> {
        let (mut matches, mut unreadable) = (0u32, false);
        for n in appearance_streams_of(objs, d) {
            let Some(Entry { obj: Object::Stream(st), .. }) = objs.get(&n) else { continue };
            match self.stream_bytes(st)? {
                None => unreadable = true,
                Some(data) => {
                    matches = matches.saturating_add(self.matcher.find(&content_text(&data, self.limits)).len() as u32);
                }
            }
        }
        Ok((matches, unreadable))
    }

    /// Processes one dictionary reached as outline item, annotation and/or form field.
    fn dict(&mut self, objs: &Objs, d: &mut Dict, t: Target, loc: Loc, out: &mut Vec<TextFieldOutcome>) -> Result<bool> {
        let p = self.policy;
        let widget_like = t.field || d.get("FT").is_some() || d.get("Subtype").is_some_and(|s| s.is_name("Widget"));
        let mut slots: Vec<Slot> = Vec::new();
        if t.outline {
            slots.push(Slot { key: "Title", role: TextRole::OutlineTitle, kind: SlotKind::Plain, enabled: p.outline_titles });
        }
        if t.annot {
            slots.push(Slot { key: "Contents", role: TextRole::AnnotContents, kind: SlotKind::Plain, enabled: p.annotation_text });
            slots.push(Slot { key: "Subj", role: TextRole::AnnotSubject, kind: SlotKind::Plain, enabled: p.annotation_text });
            slots.push(Slot { key: "RC", role: TextRole::AnnotRichText, kind: SlotKind::Rich, enabled: p.annotation_text });
        }
        if t.annot || t.field {
            if widget_like {
                slots.push(Slot { key: "T", role: TextRole::FieldName, kind: SlotKind::Plain, enabled: false });
            } else {
                slots.push(Slot { key: "T", role: TextRole::AnnotTitle, kind: SlotKind::Plain, enabled: p.annotation_text });
            }
        }
        if widget_like {
            slots.push(Slot { key: "V", role: TextRole::FieldValue, kind: SlotKind::Multi, enabled: p.widget_values });
            slots.push(Slot { key: "DV", role: TextRole::FieldDefaultValue, kind: SlotKind::Multi, enabled: p.widget_values });
        }
        let (mut changed, mut edited) = (false, false);
        for s in &slots {
            match self.slot(objs, d, s, loc, out)? {
                Applied::Edited => {
                    changed = true;
                    edited = true;
                }
                Applied::Kept | Applied::Nothing => {}
            }
        }
        if t.annot && (p.annotation_text || (widget_like && p.widget_values)) {
            let (matches, unreadable) = self.appearance_scan(objs, d)?;
            let enabled = !widget_like || p.widget_values;
            let outcome = |kind, unreadable| TextFieldOutcome { object: loc.holder, direct: loc.direct, role: TextRole::Appearance, kind, matches, unreadable };
            if enabled && (matches > 0 || (unreadable && edited)) {
                // The text of this annotation was redacted but its rendering still shows it (or
                // cannot be shown to be free of it): drop the appearance; viewers regenerate it.
                d.remove("AP");
                changed = true;
                out.push(outcome(TextOutcomeKind::Dropped, matches == 0));
            } else if !enabled && matches > 0 {
                out.push(outcome(TextOutcomeKind::KeptByPolicy, false));
            }
        }
        Ok(changed)
    }
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
    /// Redacts `needles` (Unicode text) from outline titles and annotation text per `policy`, as
    /// one atomic in-memory change (the document is untouched on error). Call `save_full` (or use
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
        report.matches_redacted = report
            .outcomes
            .iter()
            .filter(|o| o.kind != TextOutcomeKind::KeptByPolicy)
            .fold(0u32, |a, o| a.saturating_add(o.matches));
        report.kept_by_policy = report.outcomes.iter().filter(|o| o.kind == TextOutcomeKind::KeptByPolicy).count() as u32;
        report.complete = report.kept_by_policy == 0;
        self.objects = staged;
        Ok(report)
    }
}

/// Redacts `needles` from outline titles and annotation text, full-saves, and verifies the saved
/// bytes with the independent payload proof. On error the document is restored.
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
