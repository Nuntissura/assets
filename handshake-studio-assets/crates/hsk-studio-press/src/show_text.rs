//! Text drawn by content streams (page content, form XObjects, annotation appearances), read
//! through each font: the encoding of a simple font (base encoding plus `/Differences`, glyph names
//! through the Adobe Glyph List, `encoding.rs`) and/or its `/ToUnicode` CMap. Two views of the same
//! strings: `all` is what a redaction scan matches (single-byte reading plus every other reading);
//! `mapped` is the text that ONLY a font's encoding or ToUnicode map reveals (composite-font codes,
//! custom encodings), which a byte scan cannot see and the payload proof therefore adds.
//! Font-decoding sites: [`read_font`] (font dictionary -> [`FontText`]) and the show branch of
//! [`Walker::interp`] (string -> readings); the redaction selector decodes with the same
//! `SimpleEncoding` (`redaction.rs`, `Interp::font` / `unicode_of`) and the proof's code spellings use
//! `FontText` (`sanitized_save.rs`, `code_variants`).
//!
//! One interpreter, [`Walker`], serves the payload proof and the text-field appearance scan. It
//! follows what a viewer draws, not what a dictionary lists:
//! - the font is part of the graphics state: `Tf`, an ExtGState `/Font` set by `gs`, saved by `q` and
//!   restored by `Q` (ISO 32000-2 8.4.1, 8.4.5, 9.3.1); a page's content streams are one stream, so
//!   the state carries from one to the next (7.8.2);
//! - a form XObject is entered by `Do` with the caller's graphics state (its font included) and with
//!   its own `/Resources`, or the caller's when it has none (8.10.1), memoised per (form, resources,
//!   font, role), depth [`MAX_FORM_DEPTH`]; a cycle or deeper nesting is unreadable;
//! - anything it cannot follow (content that does not tokenize, operands split from their operator
//!   across a page's streams, `q` nesting over [`MAX_GSTATE`], an undecodable form) is reported in
//!   [`Walker::unreadable`] instead of being skipped (tests `font_state_follows_q_q_gs_and_do`,
//!   `unreadable_or_stray_content_never_reads_clean`).
//!
//! Bounded: a ToUnicode stream is decoded under the caller's decoded-byte budget (aggregate), parsed
//! by `cmap` (entry and range limits), and cached per font object; the walk interprets at most
//! `4 x Limits::max_content_ops` operators.
#![deny(clippy::arithmetic_side_effects)]
use crate::cmap::{Cmap, parse_to_unicode};
use crate::content::{Op, parse_content_ext};
use crate::encoding::{SimpleEncoding, expand_ligatures, is_composite, same_words};
use crate::error::{Limits, PressError, Result};
use crate::filters;
use crate::reader::Entry;
use crate::redaction::res;
use crate::syntax::{Dict, Object};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

pub(crate) type Objs = BTreeMap<u32, Entry>;

pub(crate) enum FontText {
    /// One-byte codes: read through the font's encoding (base encoding, `/Differences`, glyph names),
    /// through `/ToUnicode` where there is one, and as Latin-1 (what the byte-level proof sees). A code
    /// none of them reads makes the text opaque.
    Simple { enc: SimpleEncoding, cmap: Option<Cmap> },
    /// Composite font: `/ToUnicode` decodes the codes.
    Composite { cmap: Cmap },
    /// Composite font without a usable `/ToUnicode`: the codes cannot be read.
    Opaque,
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

pub(crate) struct Shown {
    pub all: Vec<char>,
    /// The same text with every operator of two or more strings (a kerned `TJ`) read as one run:
    /// its bytes joined and every reading joined, no separator between the strings (R8 F2/D2).
    pub joined: Vec<char>,
    pub mapped: Vec<char>,
    /// Text was drawn with a font whose codes cannot be read (composite without a usable
    /// `/ToUnicode`), or the content could not be followed.
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
    let composite = is_composite(fd);
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
    Ok(match (composite, cmap) {
        (true, Some(cmap)) => FontText::Composite { cmap },
        (true, None) => FontText::Opaque,
        (false, cmap) => FontText::Simple { enc: SimpleEncoding::of_font(objs, fd, budget)?, cmap },
    })
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
    /// with none. Never kept text; a font name it cannot resolve makes its text opaque.
    Stray,
}

/// Font selected in the graphics state.
#[derive(Clone)]
enum FontSel {
    /// No `Tf` yet.
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

/// Text shown by one text-showing operator in one drawing context.
pub(crate) struct ShowRec {
    /// The stream holding the operator (`None`: bytes handed to [`Walker::run_bytes`]).
    pub stream: Option<u32>,
    pub role: Role,
    /// Index of the operator in its stream's operations.
    pub op: usize,
    pub all: Vec<char>,
    /// Font readings that differ from the bytes, one space after each string's reading.
    pub mapped: Vec<char>,
    /// Operator of two or more non-empty strings (a kerned `TJ`): each reading that differs from the
    /// bytes, joined over all strings with NO separator, as a viewer draws one run (R8 F2/D2; test
    /// `kerned_tj_runs_are_read_whole_and_kept_text_needs_a_drawn_reading`). Empty otherwise.
    pub joined: Vec<Vec<char>>,
    /// The string bytes of such an operator joined (empty for a single string).
    pub bytes: Vec<u8>,
    /// Per string of the operator ([`show_strings`] order): its bytes are what the font draws (a simple
    /// font whose complete encoding reading has the same letters and digits as the bytes). Only such
    /// bytes may exempt a byte match in kept text (R8 P1).
    pub raw_drawn: Vec<bool>,
    pub opaque: bool,
}

/// `q` nesting the walker follows (as redaction does).
pub(crate) const MAX_GSTATE: usize = 256;
/// Form nesting the walker follows.
pub(crate) const MAX_FORM_DEPTH: usize = 32;

pub(crate) type ParsedOps = Rc<(Vec<Op>, bool)>;

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
    done: BTreeSet<(u32, usize, usize, Role)>,
    ops_left: usize,
    pub records: Vec<ShowRec>,
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
            self.interp(Some(*seg), &parsed.0, resources, role, &mut st, 0, &mut chain)?;
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
        self.interp(Some(num), &parsed.0, resources, role, &mut st, 0, &mut chain)
    }

    /// Bytes that are not (or not known as) a stream object, drawn on their own.
    pub(crate) fn run_bytes(&mut self, data: &[u8], resources: Option<&'o Dict>, role: Role) -> Result<()> {
        let Ok((ops, _)) = parse_content_ext(data, self.limits) else {
            // Unreadable: the raw bytes still count as text for matching, and it reads as opaque.
            self.root_unreadable = true;
            let mut all = latin1(data);
            all.push(' ');
            self.records.push(ShowRec { stream: None, role, op: 0, all, mapped: Vec::new(), joined: Vec::new(), bytes: Vec::new(), raw_drawn: Vec::new(), opaque: true });
            return Ok(());
        };
        let mut st = State { font: FontSel::Unset, stack: Vec::new() };
        let mut chain = Vec::new();
        self.interp(None, &ops, resources, role, &mut st, 0, &mut chain)
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
                    let strs = show_strings(op);
                    if strs.iter().all(|s| s.is_empty()) {
                        continue;
                    }
                    // A kerned TJ draws one run: its readings are also matched joined (R8 F2/D2).
                    let multi = strs.iter().filter(|s| !s.is_empty()).count() > 1;
                    let mut rec = ShowRec {
                        stream,
                        role,
                        op: i,
                        all: Vec::new(),
                        mapped: Vec::new(),
                        joined: Vec::new(),
                        bytes: Vec::new(),
                        raw_drawn: Vec::with_capacity(strs.len()),
                        opaque: false,
                    };
                    let (mut enc_join, mut map_join): (Vec<char>, Vec<char>) = (Vec::new(), Vec::new());
                    for s in strs {
                        if multi {
                            rec.bytes.extend_from_slice(s);
                        }
                        let mut raw_drawn = false;
                        match &st.font {
                            FontSel::Known(f) => match f.as_ref() {
                                FontText::Composite { cmap } => {
                                    // A code the map does not name is text this walk cannot read.
                                    let (decoded, complete) = cmap.decode_checked(s);
                                    rec.opaque |= !complete;
                                    rec.all.extend(decoded.iter().copied());
                                    map_join.extend(decoded.iter().copied());
                                    rec.mapped.extend(decoded);
                                    rec.mapped.push(' ');
                                }
                                FontText::Opaque => {
                                    rec.opaque |= !s.is_empty();
                                    rec.all.extend(latin1(s));
                                }
                                FontText::Simple { enc, cmap } => {
                                    // Every reading of the string a viewer may draw: the encoding's (and
                                    // with ligature names spelled out), the font's /ToUnicode, and the
                                    // bytes as Latin-1. A reading that equals the bytes adds nothing.
                                    let raw = latin1(s);
                                    let read = enc.read(s, cmap.as_ref());
                                    rec.opaque |= read.opaque;
                                    raw_drawn = !read.opaque && same_words(&read.text, &raw);
                                    enc_join.extend(read.text.iter().copied());
                                    let mut readings: Vec<Vec<char>> = Vec::new();
                                    readings.extend(expand_ligatures(&read.text));
                                    readings.push(read.text);
                                    if let Some(c) = cmap.as_ref() {
                                        let d = c.decode(s);
                                        map_join.extend(d.iter().copied());
                                        readings.push(d);
                                    }
                                    for reading in readings.into_iter().filter(|r| !r.is_empty() && *r != raw) {
                                        rec.all.extend(reading.iter().copied());
                                        rec.all.push(' ');
                                        rec.mapped.extend(reading);
                                        rec.mapped.push(' ');
                                    }
                                    rec.all.extend(raw);
                                }
                            },
                            // A font name the resources do not hold: what a viewer substitutes is
                            // unknown, so page content and stray content (page objects outside the
                            // page tree, streams nothing draws: no drawing context to fall back on) are
                            // opaque (R8 F1). Appearance streams fall back to /DR first (see
                            // `font_named`) and then to a viewer default font (single-byte codes, the
                            // byte scan).
                            FontSel::Unresolved => {
                                rec.opaque |= matches!(role, Role::Kept | Role::Stray) && !s.is_empty();
                                rec.all.extend(latin1(s));
                            }
                            FontSel::Unset => rec.all.extend(latin1(s)),
                        }
                        rec.raw_drawn.push(raw_drawn);
                    }
                    if multi {
                        let raw = latin1(&rec.bytes);
                        let lig = expand_ligatures(&enc_join);
                        for reading in [Some(enc_join), lig, Some(map_join)].into_iter().flatten() {
                            if !reading.is_empty() && reading != raw && !rec.joined.contains(&reading) {
                                rec.joined.push(reading);
                            }
                        }
                    }
                    rec.all.push(' ');
                    self.records.push(rec);
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
                    if !self.done.insert(key) {
                        continue;
                    }
                    let Some(parsed) = self.ops_of(r.num)? else {
                        self.unread(Some(r.num), "content_unreadable");
                        continue;
                    };
                    // The form starts with the caller's graphics state and leaves it unchanged.
                    let mut inner = State { font: st.font.clone(), stack: Vec::new() };
                    chain.push(r.num);
                    let walked = self.interp(Some(r.num), &parsed.0, active, role, &mut inner, depth.saturating_add(1), chain);
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

    /// Records joined into one [`Shown`] (the text-field view of an appearance).
    pub(crate) fn shown(&self) -> Shown {
        let mut shown = Shown { all: Vec::new(), joined: Vec::new(), mapped: Vec::new(), opaque: self.root_unreadable || !self.unreadable.is_empty() };
        for r in &self.records {
            shown.all.extend(r.all.iter().copied());
            if r.bytes.is_empty() {
                shown.joined.extend(r.all.iter().copied());
            } else {
                shown.joined.extend(latin1(&r.bytes));
                shown.joined.push(' ');
                for j in &r.joined {
                    shown.joined.extend(j.iter().copied());
                    shown.joined.push(' ');
                }
            }
            shown.mapped.extend(r.mapped.iter().copied());
            shown.opaque |= r.opaque;
        }
        shown
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
/// that cannot be followed reads as `opaque`.
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
    Ok(shown)
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

/// Content streams that are content by their role, with the resources they are read with on their
/// own: form XObjects, tiling patterns, Type3 glyph procedures (with the font's resources) and any
/// other non-image stream that carries `/Resources`.
pub(crate) fn content_role_streams(objs: &Objs) -> BTreeMap<u32, Option<&Dict>> {
    let mut out = BTreeMap::new();
    for (num, e) in objs {
        match &e.obj {
            Object::Stream(s) => {
                let d = &s.dict;
                let own = d.get("Resources").map(|r| res(objs, r)).and_then(Object::as_dict);
                let form = d.get("Subtype").is_some_and(|t| t.is_name("Form"));
                let pattern = d.get("PatternType").and_then(Object::as_int) == Some(1);
                let image = d.get("Subtype").is_some_and(|t| t.is_name("Image"));
                if form || pattern || (own.is_some() && !image) {
                    out.insert(*num, own);
                }
            }
            Object::Dict(d) if d.get("Subtype").is_some_and(|t| t.is_name("Type3")) => {
                let own = d.get("Resources").map(|r| res(objs, r)).and_then(Object::as_dict);
                if let Some(procs) = d.get("CharProcs").map(|c| res(objs, c)).and_then(Object::as_dict) {
                    for (_, v) in &procs.entries {
                        if let Some(r) = v.as_ref() {
                            out.entry(r.num).or_insert(own);
                        }
                    }
                }
            }
            _ => {}
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
