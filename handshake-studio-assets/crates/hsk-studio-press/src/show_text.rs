//! The determination pass (lead decision 2026-10-11T13:10:26Z, POSITIVE DETERMINATION): text drawn
//! by content streams (page content, form XObjects, Type 3 glyph procedures, annotation appearances,
//! any stream the object-table scan reaches), read through each font: the encoding of a simple font
//! (base encoding plus `/Differences`, glyph names through the Adobe Glyph List, `encoding.rs`) and/or
//! its `/ToUnicode` CMap.
//!
//! For EVERY text-showing operator (`Tj`, `TJ`, `'`, `"`) the walk records a [`ShowRec`]:
//! - its font, resolved by the graphics-state rules or not ([`FontState`]: no font selected, or a
//!   name that is no font dictionary of the resources in force);
//! - its writing mode ([`WMode`] from `determination`: horizontal, vertical or unresolved);
//! - whether its glyph geometry is known ([`ShowRec::geometry`]: Type 3 and vertical text are not
//!   computed by the redaction selector until lane SMB-PRESS-GEOMETRY);
//! - its reading per string ([`StrRead`]) and whether every code was read ([`ShowRec::unreadable`]).
//!
//! The payload proof computes `is_proven` / `region_cleared` from these records
//! (`sanitized_save.rs`): an operator whose font or writing mode is unresolved, or whose reading is
//! incomplete, is a typed `NotProven` (`TextCause`), and on a redaction region's page an operator
//! whose geometry is unknown makes the region incomplete (`RedactionClass`). Needles are matched in
//! the text each root draws in drawing order ([`Walker::root_texts`]): every string's reading joined
//! without separators (so `(SECRE) Tj (T-ALPHA) Tj` reads `SECRET-ALPHA`, across operators, text
//! objects and the forms a stream draws) and spaced (one space between strings, so kerning used as
//! the word space still reads as a space).
//!
//! One interpreter, [`Walker`], serves the payload proof and the text-field appearance scan. It
//! follows what a viewer draws, not what a dictionary lists:
//! - the font is part of the graphics state: `Tf`, an ExtGState `/Font` set by `gs`, saved by `q` and
//!   restored by `Q` (ISO 32000-2 8.4.1, 8.4.2, 8.4.5 Table 57, 9.3.1); a page's content streams are
//!   one stream, so the state carries from one to the next (7.8.2);
//! - a form XObject is entered by `Do` with the caller's graphics state (its font included) and with
//!   its own `/Resources`, or the caller's when it has none (7.8.3, 8.10.1), memoised per (form,
//!   resources, font, role), depth [`MAX_FORM_DEPTH`]; a cycle or deeper nesting is unreadable;
//! - anything it cannot follow (content that does not tokenize, operands split from their operator
//!   across a page's streams, `q` nesting over [`MAX_GSTATE`], an undecodable form) is reported in
//!   [`Walker::unreadable`] instead of being skipped (tests `font_state_follows_q_q_gs_and_do`,
//!   `unreadable_or_stray_content_never_reads_clean`).
//!
//! Bounded: a ToUnicode stream is decoded under the caller's decoded-byte budget (aggregate), parsed
//! by `cmap` (entry and range limits), and cached per font object; the walk interprets at most
//! `4 x Limits::max_content_ops` operators; the text of the roots is charged to
//! `Limits::max_decoded_total` characters ([`Walker::root_texts`]).
#![deny(clippy::arithmetic_side_effects)]
use crate::cmap::{Cmap, parse_to_unicode};
use crate::content::{Op, parse_content_ext};
use crate::determination::{WMode, writing_mode};
use crate::encoding::{SimpleEncoding, is_composite, same_words};
use crate::error::{Limits, PressError, RedactionClass, Result, TextCause};
use crate::filters;
use crate::reader::Entry;
use crate::redaction::res;
use crate::syntax::{Dict, Object};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

pub(crate) type Objs = BTreeMap<u32, Entry>;

pub(crate) enum FontText {
    /// One-byte codes: read through the font's encoding (base encoding, `/Differences`, glyph names)
    /// and through `/ToUnicode` where there is one. A code neither reads makes the text unreadable.
    /// Always horizontal; `type3`: a Type 3 font (glyph geometry not computed by the selector).
    Simple { enc: SimpleEncoding, cmap: Option<Cmap>, type3: bool },
    /// Composite font: `/ToUnicode` decodes the codes (`None`: no usable map, the codes cannot be
    /// read); `wmode` from its CMap (`determination::writing_mode`).
    Composite { cmap: Option<Cmap>, wmode: WMode },
}

impl FontText {
    fn wmode(&self) -> WMode {
        match self {
            Self::Simple { .. } => WMode::Horizontal,
            Self::Composite { wmode, .. } => *wmode,
        }
    }
    fn type3(&self) -> bool {
        matches!(self, Self::Simple { type3: true, .. })
    }
}

/// Fonts already read: by font object number, and (for font dictionaries written directly into a
/// resource dictionary) by the dictionary's address. The address entries are only valid while the
/// object map is not mutated, so every [`Walker`] starts with none (a hostile content stream with a
/// million `Tf` operators must not rebuild a direct font's encoding a million times).
#[derive(Default)]
pub(crate) struct FontCache {
    by_object: BTreeMap<u32, Rc<FontText>>,
    by_address: BTreeMap<usize, Rc<FontText>>,
}

/// The text a stream (and every form it draws) shows, for the text-field appearance scan: one entry
/// per reading kind, joined and spaced ([`Walker::root_texts`]), and whether any of it could not be
/// read or followed.
pub(crate) struct Shown {
    pub texts: Vec<Vec<char>>,
    pub opaque: bool,
}

fn latin1(s: &[u8]) -> Vec<char> {
    s.iter().map(|b| char::from(*b)).collect()
}

fn strings<'o>(o: &'o Object, out: &mut Vec<&'o [u8]>) {
    match o {
        Object::Str(s) => out.push(s),
        Object::Array(a) => a.iter().for_each(|x| strings(x, out)),
        _ => {}
    }
}

/// Text-showing operators.
pub(crate) fn is_show(name: &[u8]) -> bool {
    matches!(name, b"Tj" | b"TJ" | b"'" | b"\"")
}

/// String operands of a text-showing operator (`"` carries two numbers first).
pub(crate) fn show_strings(op: &Op) -> Vec<&[u8]> {
    let mut out = Vec::new();
    op.operands.iter().for_each(|o| strings(o, &mut out));
    out
}

fn read_font(objs: &Objs, fd: &Dict, limits: &Limits, budget: &mut usize) -> Result<FontText> {
    let mut cmap = None;
    if let Some(Object::Stream(tu)) = fd.get("ToUnicode").map(|v| res(objs, v))
        && filters::is_decodable(&tu.dict)
    {
        match filters::decode_stream(tu, limits, budget) {
            Ok(data) => cmap = parse_to_unicode(&data),
            Err(e @ (PressError::InputLimit(_) | PressError::Canceled)) => return Err(e),
            Err(_) => {}
        }
    }
    if is_composite(fd) {
        let wmode = writing_mode(objs, fd.get("Encoding"), limits, budget, 0)?;
        return Ok(FontText::Composite { cmap, wmode });
    }
    let type3 = fd.get("Subtype").map(|t| res(objs, t)).is_some_and(|t| t.is_name("Type3"));
    Ok(FontText::Simple { enc: SimpleEncoding::of_font(objs, fd, budget)?, cmap, type3 })
}

/// The font a font-dictionary value selects (`None`: not a font dictionary).
fn font_of(objs: &Objs, v: &Object, limits: &Limits, budget: &mut usize, cache: &mut FontCache) -> Result<Option<Rc<FontText>>> {
    let key = if let Object::Ref(r) = v { Some(r.num) } else { None };
    if let Some(k) = key
        && let Some(hit) = cache.by_object.get(&k)
    {
        return Ok(Some(Rc::clone(hit)));
    }
    let Some(fd) = res(objs, v).as_dict() else { return Ok(None) };
    let address = std::ptr::from_ref(fd) as usize;
    if key.is_none()
        && let Some(hit) = cache.by_address.get(&address)
    {
        return Ok(Some(Rc::clone(hit)));
    }
    let font = Rc::new(read_font(objs, fd, limits, budget)?);
    match key {
        Some(k) => cache.by_object.insert(k, Rc::clone(&font)),
        None => cache.by_address.insert(address, Rc::clone(&font)),
    };
    Ok(Some(font))
}

/// The font a resource name selects (`None`: not a font dictionary).
pub(crate) fn lookup_font(
    objs: &Objs,
    fonts: Option<&Dict>,
    name: &[u8],
    limits: &Limits,
    budget: &mut usize,
    cache: &mut FontCache,
) -> Result<Option<Rc<FontText>>> {
    match fonts.and_then(|f| f.get_key(name)) {
        Some(v) => font_of(objs, v, limits, budget, cache),
        None => Ok(None),
    }
}

/// What drew a content stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Role {
    /// Page content, or a form reached from page content by `Do`: the document's kept text.
    Kept,
    /// An annotation appearance, or a form it draws.
    Appearance,
    /// A content stream (form, tiling pattern, Type3 glyph procedure, stream with resources) that no
    /// page or appearance draws, read with its own resources; the content of a page object outside the
    /// page tree, read with that page's resources; any other stream that tokenizes as content, read
    /// with none. Not kept text; a font name it cannot resolve makes its text opaque (tests
    /// `stray_streams_and_hidden_payloads_are_not_kept_text`,
    /// `partial_tounicode_and_unreachable_pages_are_read_or_opaque`).
    Stray,
}

/// Font selected in the graphics state.
#[derive(Clone)]
enum FontSel {
    /// No `Tf` (or `gs` with `/Font`) yet.
    Unset,
    Known(Rc<FontText>),
    /// A `Tf` (or `gs /Font`) named a font the resources do not hold.
    Unresolved,
}

impl FontSel {
    fn key(&self) -> usize {
        match self {
            Self::Unset => 0,
            Self::Unresolved => 1,
            Self::Known(f) => Rc::as_ptr(f).cast::<u8>() as usize,
        }
    }
}

struct State {
    font: FontSel,
    stack: Vec<FontSel>,
}

/// How the font of a text-showing operator was determined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FontState {
    Known,
    /// No `Tf` or ExtGState `/Font` selected a font.
    NoFont,
    /// The selected name is no font dictionary of the resources in force (nor `/AcroForm /DR` for an
    /// appearance).
    Unresolved,
}

/// The readings of one string of a text-showing operator.
pub(crate) struct StrRead {
    /// The text the font draws: a simple font's encoding reading (codes it leaves open completed by
    /// `/ToUnicode`), a composite font's `/ToUnicode` reading (the codes it names). For a font that is
    /// not determined (or a composite font without a map) the bytes as Latin-1: a guess, and the
    /// operator is not positively determined.
    pub text: Vec<char>,
    /// A simple font's `/ToUnicode` reading, when it has a map (what text extraction may report).
    pub alt: Option<Vec<char>>,
    /// The bytes as Latin-1.
    pub raw: Vec<char>,
    /// The bytes are what the font draws (a simple font whose complete encoding reading has the same
    /// letters and digits as the bytes). Only such bytes may exempt a byte match in kept text (R8 P1;
    /// test `kept_text_needs_a_drawn_reading`).
    pub raw_drawn: bool,
}

/// One text-showing operator in one drawing context, positively determined or not.
pub(crate) struct ShowRec {
    /// The stream holding the operator (`None`: bytes handed to [`Walker::run_bytes`]).
    pub stream: Option<u32>,
    pub role: Role,
    /// Index of the operator in its stream's operations.
    pub op: usize,
    /// Per string of the operator ([`show_strings`] order).
    pub strings: Vec<StrRead>,
    pub font: FontState,
    pub wmode: WMode,
    pub type3: bool,
    /// Some code (or the content itself) could not be turned into text.
    pub unreadable: bool,
}

impl ShowRec {
    /// The operator's text is not completely known (its font is not determined, or a code is not read).
    pub(crate) fn opaque(&self) -> bool {
        self.font != FontState::Known || self.unreadable
    }

    /// Why this operator is not positively determined (empty: it is).
    pub(crate) fn causes(&self) -> Vec<TextCause> {
        let mut v = Vec::new();
        match self.font {
            FontState::NoFont => v.push(TextCause::NoFont),
            FontState::Unresolved => v.push(TextCause::UnresolvedFont),
            FontState::Known => {
                if self.wmode == WMode::Unresolved {
                    v.push(TextCause::UnresolvedWritingMode);
                }
            }
        }
        if self.unreadable {
            v.push(TextCause::UnreadableCode);
        }
        v
    }

    /// Why the redaction selector cannot compute this operator's glyph boxes (empty: it can).
    pub(crate) fn geometry(&self) -> Vec<RedactionClass> {
        if self.font != FontState::Known {
            return vec![RedactionClass::UnresolvedFont];
        }
        let mut v = Vec::new();
        if self.type3 {
            v.push(RedactionClass::Type3Text);
        }
        match self.wmode {
            WMode::Horizontal => {}
            WMode::Vertical => v.push(RedactionClass::VerticalText),
            WMode::Unresolved => v.push(RedactionClass::UnresolvedWritingMode),
        }
        v
    }
}

/// `q` nesting the walker follows (as redaction does).
pub(crate) const MAX_GSTATE: usize = 256;
/// Form nesting the walker follows.
pub(crate) const MAX_FORM_DEPTH: usize = 32;

pub(crate) type ParsedOps = Rc<(Vec<Op>, bool)>;

/// An entry of a drawing sequence: a text-showing operator, or a form drawn there by `Do`.
#[derive(Clone, Copy)]
enum Item {
    Rec(usize),
    Form(usize),
}

type MemoKey = (u32, usize, usize, Role);

/// Content interpreter for text (see the module docs).
pub(crate) struct Walker<'o> {
    objs: &'o Objs,
    limits: &'o Limits,
    pub budget: usize,
    pub fonts: FontCache,
    /// `/AcroForm /DR /Font`: where viewers look up fonts an appearance stream's own resources lack.
    dr_fonts: Option<&'o Dict>,
    /// Decoded stream bytes supplied by the caller (the payload proof decodes every stream once);
    /// `None` = decode on demand under `budget`.
    pre: Option<&'o BTreeMap<u32, Vec<u8>>>,
    parsed: BTreeMap<u32, Option<ParsedOps>>,
    done: BTreeSet<MemoKey>,
    ops_left: usize,
    pub records: Vec<ShowRec>,
    /// Drawing sequences: one per root and one per (form, resources, font, role) walked.
    seqs: Vec<Vec<Item>>,
    form_seq: BTreeMap<MemoKey, usize>,
    /// `(sequence, role)` of every root drawn on its own (page, appearance, stray stream, bytes).
    roots: Vec<(usize, Role)>,
    /// Forms each stream draws by `Do` (walked or memoised), for a page's closure.
    pub draws: BTreeMap<u32, BTreeSet<u32>>,
    /// Streams whose drawing could not be followed, with the reason.
    pub unreadable: BTreeMap<u32, &'static str>,
    /// Bytes handed to [`Walker::run_bytes`] could not be followed.
    pub root_unreadable: bool,
    /// Every role each stream was drawn in.
    pub roles: BTreeMap<u32, BTreeSet<Role>>,
    /// Resource dictionaries each stream was read with (for the code-spelling backstop).
    pub resources_used: BTreeMap<u32, Vec<&'o Dict>>,
    /// Every font selected during the walk stays alive, so the memo key (font address) can never be
    /// reused by a different font.
    keep: Vec<Rc<FontText>>,
}

/// Reading kinds of drawn text matched by the proof and the appearance scan: the font's text, the
/// `/ToUnicode` reading (where a simple font has one), the bytes.
pub(crate) const KINDS: usize = 3;
/// Provenance of a separator character (belongs to no string).
pub(crate) const SEP: u32 = u32::MAX;
/// Boundary in a joined text where a form drawn again by the same root is not repeated: no match
/// spans it.
const BOUNDARY: char = '\u{FFFF}';

/// Text of one reading kind of a root, `chars[i]` drawn by string `slot[i]` (index into
/// [`RootText::slots`]; [`SEP`] for separators).
pub(crate) struct Hay {
    pub chars: Vec<char>,
    pub slot: Vec<u32>,
}

/// What one root draws, in drawing order (module docs).
pub(crate) struct RootText {
    pub role: Role,
    /// `(record, string)` of every slot.
    pub slots: Vec<(usize, usize)>,
    /// Per reading kind: joined (no separator) then spaced (a space between strings).
    pub hays: Vec<Hay>,
}

impl<'o> Walker<'o> {
    pub(crate) fn new(objs: &'o Objs, limits: &'o Limits, budget: usize, mut fonts: FontCache) -> Self {
        fonts.by_address.clear();
        Self {
            objs,
            limits,
            budget,
            fonts,
            dr_fonts: dr_fonts(objs),
            pre: None,
            parsed: BTreeMap::new(),
            done: BTreeSet::new(),
            ops_left: limits.max_content_ops.saturating_mul(4),
            records: Vec::new(),
            seqs: Vec::new(),
            form_seq: BTreeMap::new(),
            roots: Vec::new(),
            draws: BTreeMap::new(),
            unreadable: BTreeMap::new(),
            root_unreadable: false,
            roles: BTreeMap::new(),
            resources_used: BTreeMap::new(),
            keep: Vec::new(),
        }
    }

    /// Uses already decoded stream bytes; a stream missing from `pre` is undecodable.
    pub(crate) fn with_decoded(mut self, pre: &'o BTreeMap<u32, Vec<u8>>) -> Self {
        self.pre = Some(pre);
        self
    }

    fn unread(&mut self, stream: Option<u32>, why: &'static str) {
        match stream {
            Some(n) => {
                self.unreadable.entry(n).or_insert(why);
            }
            None => self.root_unreadable = true,
        }
    }

    fn new_seq(&mut self) -> usize {
        self.seqs.push(Vec::new());
        self.seqs.len().saturating_sub(1)
    }

    fn push_item(&mut self, seq: usize, item: Item) {
        if let Some(s) = self.seqs.get_mut(seq) {
            s.push(item);
        }
    }

    /// Parsed operations of a stream object (cached). `None`: not a stream, not decodable, or does
    /// not tokenize.
    pub(crate) fn ops_of(&mut self, num: u32) -> Result<Option<ParsedOps>> {
        if let Some(p) = self.parsed.get(&num) {
            return Ok(p.clone());
        }
        let parsed = match self.pre {
            Some(pre) => pre.get(&num).and_then(|data| parse_content_ext(data, self.limits).ok()),
            None => match self.objs.get(&num).map(|e| &e.obj) {
                Some(Object::Stream(s)) => match filters::decode_stream(s, self.limits, &mut self.budget) {
                    Ok(data) => parse_content_ext(&data, self.limits).ok(),
                    Err(e @ (PressError::InputLimit(_) | PressError::Canceled)) => return Err(e),
                    Err(_) => None,
                },
                _ => None,
            },
        };
        let parsed = parsed.map(Rc::new);
        self.parsed.insert(num, parsed.clone());
        Ok(parsed)
    }

    fn note(&mut self, num: u32, role: Role, resources: Option<&'o Dict>) {
        self.roles.entry(num).or_default().insert(role);
        if let Some(r) = resources {
            let used = self.resources_used.entry(num).or_default();
            if !used.iter().any(|d| std::ptr::eq(*d, r)) {
                used.push(r);
            }
        }
    }

    /// A page: its content streams in order, one graphics state across them. `role` is
    /// [`Role::Kept`] for a page of the page tree and [`Role::Stray`] for a page object outside it
    /// ([`pages_outside_tree`]).
    pub(crate) fn run_page(&mut self, segments: &[u32], resources: Option<&'o Dict>, role: Role) -> Result<()> {
        let mut st = State { font: FontSel::Unset, stack: Vec::new() };
        let seq = self.new_seq();
        self.roots.push((seq, role));
        let mut broken = false;
        for (i, seg) in segments.iter().enumerate() {
            self.note(*seg, role, resources);
            if broken {
                // The state after an unreadable stream is unknown: what follows is not checkable.
                self.unread(Some(*seg), "after_unreadable_stream");
                st.font = FontSel::Unresolved;
            }
            let Some(parsed) = self.ops_of(*seg)? else {
                self.unread(Some(*seg), "content_unreadable");
                broken = true;
                continue;
            };
            if parsed.1 && i.saturating_add(1) < segments.len() {
                self.unread(Some(*seg), "operands_across_streams");
                broken = true;
            }
            let mut chain = vec![*seg];
            self.interp(Some(*seg), &parsed.0, resources, role, &mut st, 0, &mut chain, seq)?;
        }
        Ok(())
    }

    /// A stream no page, page object, appearance or content role reached (the object-table scan):
    /// read as stray content when it tokenizes, with no resources, so every font it names is
    /// unresolved and its text opaque (R8 F1; test
    /// `partial_tounicode_and_unreachable_pages_are_read_or_opaque`). A stream that does not tokenize is
    /// not content; its bytes are scanned in every spelling.
    pub(crate) fn run_unreached(&mut self, num: u32) -> Result<()> {
        if self.ops_of(num)?.is_none() {
            return Ok(());
        }
        self.run_stream(num, None, Role::Stray)
    }

    /// One stream drawn on its own (an appearance, or a stray content stream).
    pub(crate) fn run_stream(&mut self, num: u32, resources: Option<&'o Dict>, role: Role) -> Result<()> {
        self.note(num, role, resources);
        let key = (num, resources.map_or(0, |d| std::ptr::from_ref(d) as usize), FontSel::Unset.key(), role);
        if !self.done.insert(key) {
            return Ok(());
        }
        let Some(parsed) = self.ops_of(num)? else {
            self.unread(Some(num), "content_unreadable");
            return Ok(());
        };
        let mut st = State { font: FontSel::Unset, stack: Vec::new() };
        let mut chain = vec![num];
        let seq = self.new_seq();
        self.roots.push((seq, role));
        self.interp(Some(num), &parsed.0, resources, role, &mut st, 0, &mut chain, seq)
    }

    /// Bytes that are not (or not known as) a stream object, drawn on their own.
    pub(crate) fn run_bytes(&mut self, data: &[u8], resources: Option<&'o Dict>, role: Role) -> Result<()> {
        let seq = self.new_seq();
        self.roots.push((seq, role));
        let Ok((ops, _)) = parse_content_ext(data, self.limits) else {
            // Unreadable: the raw bytes still count as text for matching, and it reads as opaque.
            self.root_unreadable = true;
            let raw = latin1(data);
            let strings = vec![StrRead { text: raw.clone(), alt: None, raw, raw_drawn: false }];
            self.records.push(ShowRec { stream: None, role, op: 0, strings, font: FontState::Known, wmode: WMode::Horizontal, type3: false, unreadable: true });
            let idx = self.records.len().saturating_sub(1);
            self.push_item(seq, Item::Rec(idx));
            return Ok(());
        };
        let mut st = State { font: FontSel::Unset, stack: Vec::new() };
        let mut chain = Vec::new();
        self.interp(None, &ops, resources, role, &mut st, 0, &mut chain, seq)
    }

    fn font_named(&mut self, resources: Option<&'o Dict>, name: &[u8], role: Role) -> Result<FontSel> {
        let objs = self.objs;
        let fonts = resources.and_then(|r| r.get("Font")).map(|f| res(objs, f)).and_then(Object::as_dict);
        if let Some(f) = lookup_font(objs, fonts, name, self.limits, &mut self.budget, &mut self.fonts)? {
            return Ok(FontSel::Known(f));
        }
        if role == Role::Appearance
            && let Some(f) = lookup_font(objs, self.dr_fonts, name, self.limits, &mut self.budget, &mut self.fonts)?
        {
            return Ok(FontSel::Known(f));
        }
        Ok(FontSel::Unresolved)
    }

    /// The font an ExtGState sets (`None`: the state sets no font).
    fn gs_font(&mut self, resources: Option<&'o Dict>, name: &[u8]) -> Result<Option<FontSel>> {
        let objs = self.objs;
        let Some(gs) = resources
            .and_then(|r| r.get("ExtGState"))
            .map(|g| res(objs, g))
            .and_then(Object::as_dict)
            .and_then(|g| g.get_key(name))
            .map(|v| res(objs, v))
            .and_then(Object::as_dict)
        else {
            return Ok(None);
        };
        let Some(entry) = gs.get("Font").map(|f| res(objs, f)) else { return Ok(None) };
        let font = match entry.as_array().and_then(|a| a.first()) {
            Some(v) => font_of(objs, v, self.limits, &mut self.budget, &mut self.fonts)?,
            None => None,
        };
        Ok(Some(font.map_or(FontSel::Unresolved, FontSel::Known)))
    }

    /// The record of one text-showing operator drawn with `font`.
    fn determine(stream: Option<u32>, role: Role, op_index: usize, op: &Op, font: &FontSel) -> ShowRec {
        let (state, wmode, type3) = match font {
            FontSel::Known(f) => (FontState::Known, f.wmode(), f.type3()),
            FontSel::Unset => (FontState::NoFont, WMode::Unresolved, false),
            FontSel::Unresolved => (FontState::Unresolved, WMode::Unresolved, false),
        };
        let mut rec = ShowRec { stream, role, op: op_index, strings: Vec::new(), font: state, wmode, type3, unreadable: false };
        for s in show_strings(op) {
            let raw = latin1(s);
            let (text, alt, raw_drawn) = match font {
                FontSel::Known(f) => match f.as_ref() {
                    FontText::Composite { cmap: Some(cmap), .. } => {
                        // A code the map does not name is text this walk cannot read.
                        let (decoded, complete) = cmap.decode_checked(s);
                        rec.unreadable |= !complete;
                        (decoded, None, false)
                    }
                    FontText::Composite { cmap: None, .. } => {
                        rec.unreadable |= !s.is_empty();
                        (raw.clone(), None, false)
                    }
                    FontText::Simple { enc, cmap, .. } => {
                        let read = enc.read(s, cmap.as_ref());
                        rec.unreadable |= read.opaque;
                        let drawn = !read.opaque && same_words(&read.text, &raw);
                        (read.text, cmap.as_ref().map(|c| c.decode(s)), drawn)
                    }
                },
                // Not determined: what a viewer substitutes is unknown (the bytes are a guess).
                FontSel::Unset | FontSel::Unresolved => (raw.clone(), None, false),
            };
            rec.strings.push(StrRead { text, alt, raw, raw_drawn });
        }
        rec
    }

    #[allow(clippy::too_many_arguments)]
    fn interp(
        &mut self,
        stream: Option<u32>,
        ops: &[Op],
        resources: Option<&'o Dict>,
        role: Role,
        st: &mut State,
        depth: usize,
        chain: &mut Vec<u32>,
        seq: usize,
    ) -> Result<()> {
        let objs = self.objs;
        for (i, op) in ops.iter().enumerate() {
            self.ops_left = self.ops_left.checked_sub(1).ok_or(PressError::InputLimit("content_walk"))?;
            match op.name.as_slice() {
                b"q" => {
                    if st.stack.len() >= MAX_GSTATE {
                        self.unread(stream, "gstate_depth");
                    } else {
                        st.stack.push(st.font.clone());
                    }
                }
                b"Q" => {
                    if let Some(f) = st.stack.pop() {
                        st.font = f;
                    }
                }
                b"Tf" => {
                    st.font = match op.operands.first().and_then(Object::as_name) {
                        Some(n) => self.font_named(resources, n, role)?,
                        None => FontSel::Unresolved,
                    };
                    self.keep_alive(&st.font);
                }
                b"gs" => {
                    if let Some(n) = op.operands.first().and_then(Object::as_name)
                        && let Some(f) = self.gs_font(resources, n)?
                    {
                        st.font = f;
                        self.keep_alive(&st.font);
                    }
                }
                name if is_show(name) => {
                    if show_strings(op).iter().all(|s| s.is_empty()) {
                        continue;
                    }
                    let rec = Self::determine(stream, role, i, op, &st.font);
                    self.records.push(rec);
                    let idx = self.records.len().saturating_sub(1);
                    self.push_item(seq, Item::Rec(idx));
                }
                b"Do" => {
                    let Some(name) = op.operands.first().and_then(Object::as_name) else { continue };
                    let Some(Object::Ref(r)) = resources
                        .and_then(|rs| rs.get("XObject"))
                        .map(|x| res(objs, x))
                        .and_then(Object::as_dict)
                        .and_then(|x| x.get_key(name))
                    else {
                        continue;
                    };
                    let Some(Object::Stream(form)) = objs.get(&r.num).map(|e| &e.obj) else { continue };
                    if !form.dict.get("Subtype").is_some_and(|t| t.is_name("Form")) {
                        continue;
                    }
                    if let Some(s) = stream {
                        self.draws.entry(s).or_default().insert(r.num);
                    }
                    let own = form.dict.get("Resources").map(|x| res(objs, x)).and_then(Object::as_dict);
                    let active = own.or(resources);
                    self.note(r.num, role, active);
                    if chain.contains(&r.num) {
                        self.unread(Some(r.num), "form_cycle");
                        continue;
                    }
                    if depth >= MAX_FORM_DEPTH {
                        self.unread(Some(r.num), "form_depth");
                        continue;
                    }
                    let key = (r.num, active.map_or(0, |d| std::ptr::from_ref(d) as usize), st.font.key(), role);
                    if let Some(f) = self.form_seq.get(&key).copied() {
                        self.push_item(seq, Item::Form(f));
                        continue;
                    }
                    let Some(parsed) = self.ops_of(r.num)? else {
                        self.unread(Some(r.num), "content_unreadable");
                        continue;
                    };
                    let f = self.new_seq();
                    self.form_seq.insert(key, f);
                    self.push_item(seq, Item::Form(f));
                    // The form starts with the caller's graphics state and leaves it unchanged.
                    let mut inner = State { font: st.font.clone(), stack: Vec::new() };
                    chain.push(r.num);
                    let walked = self.interp(Some(r.num), &parsed.0, active, role, &mut inner, depth.saturating_add(1), chain, f);
                    chain.pop();
                    walked?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn keep_alive(&mut self, font: &FontSel) {
        if let FontSel::Known(f) = font
            && !self.keep.last().is_some_and(|k| Rc::ptr_eq(k, f))
        {
            self.keep.push(Rc::clone(f));
        }
    }

    /// The text every root draws in drawing order, the forms it draws inline (a form drawn again by
    /// the same root is not repeated: a boundary no match spans), as [`KINDS`] reading kinds, each
    /// joined and spaced. Every character is charged to `chars_left` (`InputLimit("text_runs")` when
    /// it is spent).
    pub(crate) fn root_texts(&self, chars_left: &mut usize) -> Result<Vec<RootText>> {
        let mut out = Vec::with_capacity(self.roots.len());
        for (seq, role) in &self.roots {
            let mut rt = RootText { role: *role, slots: Vec::new(), hays: (0..KINDS.saturating_mul(2)).map(|_| Hay { chars: Vec::new(), slot: Vec::new() }).collect() };
            let mut seen: BTreeSet<usize> = BTreeSet::new();
            let mut stack: Vec<(usize, usize)> = vec![(*seq, 0)];
            while let Some((s, pos)) = stack.pop() {
                let Some(item) = self.seqs.get(s).and_then(|v| v.get(pos)).copied() else { continue };
                stack.push((s, pos.saturating_add(1)));
                match item {
                    Item::Rec(r) => self.append(&mut rt, r, chars_left)?,
                    Item::Form(f) => {
                        if stack.len() <= MAX_FORM_DEPTH.saturating_add(1) && seen.insert(f) {
                            stack.push((f, 0));
                        } else {
                            for (k, hay) in rt.hays.iter_mut().enumerate() {
                                hay.chars.push(if k % 2 == 0 { BOUNDARY } else { ' ' });
                                hay.slot.push(SEP);
                            }
                        }
                    }
                }
            }
            out.push(rt);
        }
        Ok(out)
    }

    fn append(&self, rt: &mut RootText, rec: usize, chars_left: &mut usize) -> Result<()> {
        let Some(r) = self.records.get(rec) else { return Ok(()) };
        for (si, s) in r.strings.iter().enumerate() {
            let slot = u32::try_from(rt.slots.len()).map_err(|_| PressError::InputLimit("text_runs"))?;
            rt.slots.push((rec, si));
            let kinds: [&[char]; KINDS] = [&s.text, s.alt.as_deref().unwrap_or(&s.text), &s.raw];
            for (k, text) in kinds.into_iter().enumerate() {
                let cost = text.len().saturating_add(1).saturating_mul(2);
                *chars_left = chars_left.checked_sub(cost).ok_or(PressError::InputLimit("text_runs"))?;
                let joined = k.saturating_mul(2);
                if let Some(h) = rt.hays.get_mut(joined) {
                    h.chars.extend_from_slice(text);
                    h.slot.extend(std::iter::repeat_n(slot, text.len()));
                }
                if let Some(h) = rt.hays.get_mut(joined.saturating_add(1)) {
                    if !h.chars.is_empty() {
                        h.chars.push(' ');
                        h.slot.push(SEP);
                    }
                    h.chars.extend_from_slice(text);
                    h.slot.extend(std::iter::repeat_n(slot, text.len()));
                }
            }
        }
        Ok(())
    }

    /// The text-field view of what was walked (every root's reading kinds, joined and spaced).
    pub(crate) fn shown(&self) -> Result<Shown> {
        let mut chars_left = self.limits.max_decoded_total;
        let texts = self.root_texts(&mut chars_left)?.into_iter().flat_map(|rt| rt.hays.into_iter().map(|h| h.chars)).collect();
        let opaque = self.root_unreadable || !self.unreadable.is_empty() || self.records.iter().any(ShowRec::opaque);
        Ok(Shown { texts, opaque })
    }
}

/// `/AcroForm /DR /Font` of the document's catalog, if any.
fn dr_fonts(objs: &Objs) -> Option<&Dict> {
    objs.values().find_map(|e| {
        let c = e.obj.as_dict().filter(|d| d.get("Type").is_some_and(|t| t.is_name("Catalog")))?;
        c.get("AcroForm")
            .map(|a| res(objs, a))
            .and_then(Object::as_dict)
            .and_then(|a| a.get("DR"))
            .map(|d| res(objs, d))
            .and_then(Object::as_dict)
            .and_then(|d| d.get("Font"))
            .map(|f| res(objs, f))
            .and_then(Object::as_dict)
    })
}

/// Text an appearance stream (and every form it draws) shows, read as a viewer draws it; content
/// that cannot be followed or text that is not positively determined reads as `opaque`.
pub(crate) fn show_text(
    objs: &Objs,
    resources: Option<&Dict>,
    data: &[u8],
    limits: &Limits,
    budget: &mut usize,
    cache: &mut FontCache,
) -> Result<Shown> {
    let mut w = Walker::new(objs, limits, *budget, std::mem::take(cache));
    let run = w.run_bytes(data, resources, Role::Appearance);
    *budget = w.budget;
    let shown = w.shown();
    *cache = std::mem::take(&mut w.fonts);
    run?;
    shown
}

/// The `/Resources` that apply to a page: its own or the nearest ancestor's.
pub(crate) fn inherited_resources<'a>(objs: &'a Objs, page: &'a Dict) -> Option<&'a Dict> {
    let mut d = page;
    for _ in 0..32 {
        if let Some(r) = d.get("Resources").map(|r| res(objs, r)).and_then(Object::as_dict) {
            return Some(r);
        }
        d = d.get("Parent").map(|p| res(objs, p)).and_then(Object::as_dict)?;
    }
    None
}

/// Page-tree depth the page walk follows.
const PAGE_TREE_DEPTH: usize = 64;

/// The content streams a page dictionary draws, in order: `/Contents` as one stream, or an array of
/// stream references (`None`: no content stream, e.g. an annotation whose `/Contents` is a string).
fn content_streams_of(objs: &Objs, d: &Dict) -> Option<Vec<u32>> {
    let refs = |a: &[Object]| -> Vec<u32> { a.iter().filter_map(Object::as_ref).map(|x| x.num).collect() };
    let segments = match d.get("Contents")? {
        Object::Ref(rf) => match objs.get(&rf.num).map(|x| &x.obj) {
            Some(Object::Array(a)) => refs(a),
            Some(Object::Stream(_)) => vec![rf.num],
            _ => return None,
        },
        Object::Array(a) => refs(a),
        _ => return None,
    };
    segments.iter().any(|n| matches!(objs.get(n).map(|e| &e.obj), Some(Object::Stream(_)))).then_some(segments)
}

/// Page objects OUTSIDE the page tree (object-table scan, R8 F1): every indirect dictionary that is
/// not a page reached from `/Root /Pages` and whose `/Contents` is a content stream (or an array of
/// them), such as a template page under `/Names /Templates` (ISO 32000-2 12.7.6) or a page reached only
/// through a destination, `/OpenAction` or an annotation `/P`: `(content streams, inherited
/// resources)`. Read as stray content (never kept text; test
/// `partial_tounicode_and_unreachable_pages_are_read_or_opaque`).
pub(crate) fn pages_outside_tree<'a>(objs: &'a Objs, tree: &BTreeSet<u32>) -> Vec<(Vec<u32>, Option<&'a Dict>)> {
    objs.iter()
        .filter(|(n, _)| !tree.contains(n))
        .filter_map(|(_, e)| {
            let d = e.obj.as_dict()?;
            Some((content_streams_of(objs, d)?, inherited_resources(objs, d)))
        })
        .collect()
}

/// Every page reached from `/Root /Pages`: `(page object, content stream numbers in order, resources)`.
pub(crate) fn pages<'a>(objs: &'a Objs, trailer: &'a Dict) -> Vec<(u32, Vec<u32>, Option<&'a Dict>)> {
    let mut out = Vec::new();
    let Some(root) = trailer.get("Root").map(|r| res(objs, r)).and_then(Object::as_dict).and_then(|c| c.get("Pages")).and_then(Object::as_ref)
    else {
        return out;
    };
    let mut seen = BTreeSet::new();
    let mut stack = vec![(root.num, 1usize)];
    while let Some((n, depth)) = stack.pop() {
        if depth > PAGE_TREE_DEPTH || !seen.insert(n) {
            continue;
        }
        let Some(d) = objs.get(&n).and_then(|e| e.obj.as_dict()) else { continue };
        if let Some(Object::Array(kids)) = d.get("Kids").map(|k| res(objs, k)) {
            // Reversed so the stack pops pages in document order.
            stack.extend(kids.iter().rev().filter_map(Object::as_ref).map(|r| (r.num, depth.saturating_add(1))));
            continue;
        }
        let segments: Vec<u32> = match d.get("Contents") {
            Some(Object::Ref(rf)) => match objs.get(&rf.num).map(|x| &x.obj) {
                Some(Object::Array(a)) => a.iter().filter_map(Object::as_ref).map(|x| x.num).collect(),
                _ => vec![rf.num],
            },
            Some(Object::Array(a)) => a.iter().filter_map(Object::as_ref).map(|x| x.num).collect(),
            _ => Vec::new(),
        };
        out.push((n, segments, inherited_resources(objs, d)));
    }
    out
}

/// Nesting followed when looking for Type 3 font dictionaries inside objects.
const DICT_SEARCH_DEPTH: usize = 64;

/// Content streams that are content by their role, with the resources they are read with on their
/// own: form XObjects, tiling patterns, any other non-image stream that carries `/Resources`, and the
/// glyph procedures (`/CharProcs`) of EVERY Type 3 font dictionary, indirect or written directly into
/// a resource dictionary (R9-P1), read with the font's resources (test
/// `direct_type3_glyph_procedures_are_read`).
pub(crate) fn content_role_streams(objs: &Objs) -> BTreeMap<u32, Option<&Dict>> {
    let mut out = BTreeMap::new();
    for (num, e) in objs {
        if let Object::Stream(s) = &e.obj {
            let d = &s.dict;
            let own = d.get("Resources").map(|r| res(objs, r)).and_then(Object::as_dict);
            let form = d.get("Subtype").is_some_and(|t| t.is_name("Form"));
            let pattern = d.get("PatternType").and_then(Object::as_int) == Some(1);
            let image = d.get("Subtype").is_some_and(|t| t.is_name("Image"));
            if form || pattern || (own.is_some() && !image) {
                out.insert(*num, own);
            }
        }
    }
    for e in objs.values() {
        let mut stack: Vec<(&Object, usize)> = vec![(&e.obj, 0)];
        while let Some((o, depth)) = stack.pop() {
            let d = match o {
                Object::Dict(d) => d,
                Object::Stream(s) => &s.dict,
                Object::Array(a) => {
                    if depth < DICT_SEARCH_DEPTH {
                        stack.extend(a.iter().map(|x| (x, depth.saturating_add(1))));
                    }
                    continue;
                }
                _ => continue,
            };
            if d.get("Subtype").map(|t| res(objs, t)).is_some_and(|t| t.is_name("Type3"))
                && let Some(procs) = d.get("CharProcs").map(|c| res(objs, c)).and_then(Object::as_dict)
            {
                let own = d.get("Resources").map(|r| res(objs, r)).and_then(Object::as_dict);
                for (_, v) in &procs.entries {
                    if let Some(r) = v.as_ref() {
                        out.entry(r.num).or_insert(own);
                    }
                }
            }
            if depth < DICT_SEARCH_DEPTH {
                stack.extend(d.entries.iter().map(|(_, v)| (v, depth.saturating_add(1))));
            }
        }
    }
    out
}

/// Font objects of the `/Font` dictionaries in `resources` (for the code-spelling backstop).
pub(crate) fn fonts_in(objs: &Objs, resources: &[&Dict], limits: &Limits, budget: &mut usize, cache: &mut FontCache) -> Result<Vec<Rc<FontText>>> {
    let mut out: Vec<Rc<FontText>> = Vec::new();
    for r in resources {
        let Some(fonts) = r.get("Font").map(|f| res(objs, f)).and_then(Object::as_dict) else { continue };
        for (_, v) in &fonts.entries {
            if let Some(f) = font_of(objs, v, limits, budget, cache)?
                && !out.iter().any(|g| Rc::ptr_eq(g, &f))
            {
                out.push(f);
            }
        }
    }
    Ok(out)
}
