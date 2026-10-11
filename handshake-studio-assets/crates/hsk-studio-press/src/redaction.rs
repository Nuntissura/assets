//! Irreversible region redaction for content that can be proven removed:
//! text-showing operators whose glyph run may touch the region are deleted from the page content
//! stream (the page then points at a NEW stream object; the old stream is unreachable and dropped
//! by the full save), and annotations whose `/Rect` touches the region are removed together with
//! every form-field reference to them. Anything else painted in the region (images, vectors,
//! form XObjects, shadings, inline images) cannot be removed with proof in this slice: by default
//! the operation REFUSES (fail closed) with `UnsupportedRedactionClass`.
//!
//! Content is read with the strict parser (`content::parse_strict`): an unknown operator, an extra or
//! missing operand, an operand of the wrong type or an inline image whose end is not certain is a
//! `PressError::Malformed` refusal (lead decision 2026-10-11T15:00:20Z, R10-F1/F5; tests
//! `extra_operands_are_refused`, `inline_image_end_follows_the_data`). Kept operations are written back
//! in the canonical syntax (`canon.rs`), never copied as source bytes; `redact_and_save` then rewrites
//! every content stream of the document canonically.
//!
//! Geometry (R10-F6; module `font.rs`): a glyph run's box is the union of each glyph's ink box, the
//! font's glyph box (`/FontBBox` united with the union of the 14 standard fonts' boxes) placed at the
//! glyph's origin, transformed through the text rendering matrix of ISO 32000-2 9.4.4 (`Tfs x Th`,
//! `Tfs`, `Trise`, Tm, CTM) with the SIGNED font size, so text drawn with a negative size (rotated 180
//! degrees) is boxed where it is drawn. Origins advance by `(w0 x Tfs / 1000 + Tc + Tw) x Th` per glyph
//! (Tc per glyph, Tw for the single-byte code 32): `w0` from `/Widths` (simple fonts), the 14 standard
//! fonts' AFM widths, or `/W` and `/DW` by CID (composite fonts, codes split by the encoding CMap).
//! Where a width is only bounded (a substitute font, an unknown CID) the origins are intervals and every
//! later origin on the line carries that uncertainty until a line operator positions the text again;
//! the box covers every origin the interval allows (tests `composite_geometry_uses_w_dw_and_signed_size`,
//! `standard_fonts_use_their_afm_widths`). Whole strings are removed (a string straddling the region
//! edge is removed entirely). The font is graphics state as for a viewer (lead decision
//! 2026-10-11T13:10:26Z, R9-F2): `Tf` and an ExtGState `/Font` set by `gs` (font and size; ISO 32000-2
//! 8.4.5 Table 57) select it, `q`/`Q` save and restore it, and a form XObject drawn by `Do` starts with
//! the caller's resolved font whatever its own `/Resources` hold (8.10.1; test
//! `font_is_graphics_state_in_the_selector`). Not computed (lead decisions 2026-10-11T11:58:11Z and
//! 13:10:26Z; geometry is lane SMB-PRESS-GEOMETRY): Type 3 glyphs (advance through `/FontMatrix`,
//! procedures may draw anywhere), vertical writing, a writing mode that is not positively known
//! (`determination::encoding_cmap`), text whose font is not determined, and text whose glyph widths
//! are not known (an embedded simple font without `/Widths`, a composite string its codespace cannot
//! split). Text of any such kind anywhere on the page makes the region
//! [`RedactionClass::Type3Text`] / [`RedactionClass::VerticalText`] /
//! [`RedactionClass::UnresolvedWritingMode`] / [`RedactionClass::UnresolvedFont`] /
//! [`RedactionClass::UnknownGlyphMetrics`]: refused under [`UnsupportedPolicy::Refuse`], listed in
//! `unredacted` (never complete) under `ReportAndContinue` (tests
//! `type3_and_vertical_text_never_read_as_cleared`, `writing_mode_is_read_from_the_cmap`,
//! `unknown_cmap_names_are_unresolved_writing_modes`, `fonts_inherited_into_forms_are_resolved`,
//! `undetermined_fonts_never_read_as_cleared`). A glyph's ink outside both the declared and the
//! standard boxes (a font program that draws outside its box) is not covered.
//! Images (image XObjects and inline images) cannot be erased pixel by pixel here: under
//! `UnsupportedPolicy::RemoveImages` an image whose placement touches the region is removed whole
//! (its `Do` / `BI..EI` leaves the content, the resource entry goes when nothing else uses it), under
//! the other policies it is an unsupported class. No pixel codec is involved. An XObject's `/Subtype`
//! is resolved before it is read (R10-F3; test `indirect_subtypes_are_resolved`).
#![deny(clippy::arithmetic_side_effects)]
use crate::canon::write_op;
use crate::content::{Op, parse_strict};
use crate::determination::WMode;
use crate::error::{MAX_REF_HOPS, PressError, RedactionClass, Result};
use crate::font::{Font, STD_MAX_WIDTH, STD_UNION_BOX, read_font};
use crate::text_codec::pdfdoc_char;
use crate::filters;
use crate::pdf_edit::PdfDoc;
use crate::reader::{Entry, check_cancel};
use crate::show_text::resource_components;
use crate::syntax::{Dict, ObjRef, Object, Stream};
use crate::writer::{reachable, trailer_roots};
use hsk_studio_accord::CancellationToken;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

/// Redaction target: a rectangle `[x0, y0, x1, y1]` in the page's default user space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RedactionRegion {
    pub page_index: u32,
    pub rect: [f64; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum UnsupportedPolicy {
    /// Fail closed with `UnsupportedRedactionClass` (default).
    #[default]
    Refuse,
    /// Redact what can be proven and list the rest in `RedactionReport::unredacted`.
    ReportAndContinue,
    /// Images and inline images whose placement touches a region are removed whole (listed in
    /// `RedactionReport::images_removed` / `inline_images_removed`); every other unsupported class
    /// still fails closed.
    RemoveImages,
}

/// SENSITIVE: `removed_text` holds the removed string operands; route it only to the verifier.
#[derive(Clone, Debug, PartialEq)]
pub struct RedactionReport {
    pub page_index: u32,
    /// Removed string operands (elements of a `TJ` counted one by one).
    pub text_strings_removed: u32,
    /// The removed string operands, and for every run of two or more consecutive removed strings of
    /// one content stream in drawing order (across `Tj`, `TJ`, `'` and `"` operators, the elements of a
    /// `TJ` and text objects; ended by a kept non-empty string, a `Do` and the end of the content) that
    /// run joined: what a viewer may draw as one run (R8 F2, R9-F3; tests `kerned_tj_runs_are_read_whole`,
    /// `removed_runs_join_across_show_operators`).
    pub removed_text: Vec<Vec<u8>>,
    /// `removed_text[i]` as its font draws it: its simple font's complete encoding reading whenever it
    /// differs at all from the bytes read as PDFDocEncoding text (a quote, dash or space remap
    /// included, R10-D1), else its `/ToUnicode` reading (`None`: the bytes are the text, or nothing
    /// reads them; see `removed_unreadable`). The proof searches both this text and the bytes' text
    /// (test `remapped_punctuation_is_searched_as_drawn`).
    pub removed_unicode: Vec<Option<String>>,
    /// `removed_text[i]` has no complete reading: its font is not determined (no `Tf` or `gs /Font`
    /// selected one, or the name is not in the resources), is a composite font without a usable
    /// `/ToUnicode` or with codes the map does not name, or is a simple font with a code neither its
    /// encoding nor its `/ToUnicode` reads. The words it drew are unknown; any such string, whatever its
    /// length (one glyph per operator included), withholds `RedactedOutput::is_proven`
    /// (`NotProven::UnreadableRemovedText`, R8 F3/D3, R9-F3; tests
    /// `unreadable_removed_text_is_never_proven`, `glyph_by_glyph_unreadable_text_is_never_proven`).
    pub removed_unreadable: Vec<bool>,
    pub annotations_removed: Vec<ObjRef>,
    /// Image XObjects whose placement touched the region and was removed (`RemoveImages`).
    pub images_removed: Vec<ObjRef>,
    /// Inline images removed (`RemoveImages`).
    pub inline_images_removed: u32,
    pub form_fields_removed: u32,
    /// Classes present in the region that were NOT removed (only with `ReportAndContinue`).
    pub unredacted: Vec<RedactionClass>,
    /// A removed string's advance was not exactly known, so following text on its line may have moved.
    pub layout_shift_possible: bool,
    /// Every mark in the region was removed with proof (`unredacted` is empty).
    pub complete: bool,
    /// Classes in the region that only the flattening removes (`redact_and_save`, Operator decision
    /// D27): images, vectors, shadings and inline images the selector does not remove from the page
    /// content are painted over on the raster that replaces the page, so they are neither refused nor
    /// `unredacted` there (test `flattened_region_pixels_are_opaque_and_the_rest_is_rendered`). Always
    /// empty from [`PdfDoc::redact_region`], which does not flatten.
    pub painted_over: Vec<RedactionClass>,
}

type Mat = [f64; 6];
const IDENT: Mat = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

fn mul(a: &Mat, b: &Mat) -> Mat {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}
fn apply(m: &Mat, x: f64, y: f64) -> (f64, f64) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}
fn translate(tx: f64, ty: f64) -> Mat {
    [1.0, 0.0, 0.0, 1.0, tx, ty]
}
/// Box of `points`. A non-finite point (a CTM that overflowed) makes the box unbounded: `f64::min`
/// and `f64::max` would silently drop a NaN, and a mark whose position cannot be computed must read
/// as touching every region (fail closed; test `stray_streams_and_hidden_payloads_are_not_kept_text`).
fn bbox(points: &[(f64, f64)]) -> [f64; 4] {
    if points.iter().any(|(x, y)| !x.is_finite() || !y.is_finite()) {
        return [f64::NEG_INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::INFINITY];
    }
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for (x, y) in points {
        b[0] = b[0].min(*x);
        b[1] = b[1].min(*y);
        b[2] = b[2].max(*x);
        b[3] = b[3].max(*y);
    }
    b
}
fn touches(a: &[f64; 4], b: &[f64; 4]) -> bool {
    if a.iter().any(|v| v.is_nan()) {
        return true;
    }
    a[0] <= b[2] && a[2] >= b[0] && a[1] <= b[3] && a[3] >= b[1]
}

/// The font of the graphics state (R9-F2): resolved once where it is selected, then carried by value
/// (saved by `q`, restored by `Q`, inherited by forms).
#[derive(Clone)]
enum FontSel {
    /// No `Tf` or ExtGState `/Font` selected a font.
    Unset,
    Known(Rc<Font>),
    /// The name (or the ExtGState `/Font` entry) is not a font dictionary of the resources in force.
    Unresolved,
}

#[derive(Clone)]
struct Gfx {
    ctm: Mat,
    tc: f64,
    tw: f64,
    th: f64,
    tl: f64,
    ts: f64,
    fs: f64,
    font: FontSel,
    lw: f64,
}

/// Consecutive removed strings of one content stream (R9-F3): their bytes, and per piece its reading
/// (`None`: the bytes are the text) and whether that reading is complete.
#[derive(Default)]
struct Run {
    bytes: Vec<u8>,
    pieces: Vec<(Vec<u8>, Option<String>, bool)>,
}

struct Shown {
    hit: bool,
    /// Advance of the string in text space (scaling included), when exactly known.
    adv: Option<f64>,
}

struct Interp<'a> {
    doc: &'a PdfDoc,
    resources: Option<&'a Dict>,
    region: [f64; 4],
    g: Gfx,
    stack: Vec<Gfx>,
    tm: Mat,
    tlm: Mat,
    /// Uncertainty of the text position along the line `[lo, hi]` (text space, added to Tm's origin)
    /// since the last line positioning: a run of bounded but not exact widths moves it.
    slack: (f64, f64),
    path: Option<[f64; 4]>,
    /// Fonts already read, by the address of their (resolved) font dictionary: valid for the whole
    /// pass because the document's objects are not mutated while it runs.
    fonts: BTreeMap<usize, Rc<Font>>,
    unsupported: Vec<RedactionClass>,
    removed: Vec<Vec<u8>>,
    /// Removed string operands (`removed` also holds joined runs).
    strings_removed: u32,
    /// The current run of consecutive removed strings of the content stream.
    run: Run,
    /// `removed[i]` read through its font.
    removed_unicode: Vec<Option<String>>,
    /// `removed[i]` has no complete reading.
    removed_unreadable: Vec<bool>,
    layout_shift: bool,
    /// Remove images whose placement touches the region (`UnsupportedPolicy::RemoveImages`).
    remove_images: bool,
    images_removed: Vec<ObjRef>,
    inline_removed: u32,
    /// Remaining decoded-byte allowance for form-XObject content.
    budget: usize,
    next_num: u32,
    /// Copy-on-write form XObjects created by this pass.
    new_objects: Vec<(u32, Entry)>,
    name_counter: u32,
}

/// Result of interpreting one content level (page or form).
struct Pass {
    out: Vec<u8>,
    changed: bool,
    /// New `/XObject` resource names for rewritten `Do` uses.
    additions: Vec<(Vec<u8>, ObjRef)>,
    /// Resource names whose every use at this level was rewritten.
    removals: Vec<Vec<u8>>,
}

const MAX_FORM_DEPTH: usize = 8;

struct XObj<'a> {
    /// `Some((stream, matrix))` for a form XObject; `None` for an image.
    form: Option<(&'a Stream, Mat)>,
    bbox: [f64; 4],
}

fn num(op: &Op, i: usize) -> Result<f64> {
    op.operands.get(i).and_then(Object::as_f64).ok_or(PressError::Malformed("content_operands"))
}
fn matrix(op: &Op) -> Result<Mat> {
    Ok([num(op, 0)?, num(op, 1)?, num(op, 2)?, num(op, 3)?, num(op, 4)?, num(op, 5)?])
}

/// Writes `ops` as canonical operations (inline images in transport form, `canon::write_op`).
fn emit(name: &[u8], operands: Vec<Object>, out: &mut Vec<u8>) -> Result<()> {
    write_op(&Op { name: name.to_vec(), operands, start: 0, end: 0, inline_data: None }, &[], true, out)
}

impl<'a> Interp<'a> {
    fn new(doc: &'a PdfDoc, resources: Option<&'a Dict>, region: [f64; 4], budget: usize) -> Self {
        Self {
            budget,
            next_num: doc.objects.keys().next_back().copied().unwrap_or(0).saturating_add(1),
            new_objects: Vec::new(),
            name_counter: 0,
            doc,
            resources,
            region,
            g: Gfx { ctm: IDENT, tc: 0.0, tw: 0.0, th: 1.0, tl: 0.0, ts: 0.0, fs: 0.0, font: FontSel::Unset, lw: 1.0 },
            stack: Vec::new(),
            tm: IDENT,
            tlm: IDENT,
            slack: (0.0, 0.0),
            path: None,
            fonts: BTreeMap::new(),
            unsupported: Vec::new(),
            removed: Vec::new(),
            strings_removed: 0,
            run: Run::default(),
            removed_unicode: Vec::new(),
            removed_unreadable: Vec::new(),
            layout_shift: false,
            remove_images: false,
            images_removed: Vec::new(),
            inline_removed: 0,
        }
    }

    /// The font a font dictionary value selects (`None`: not a font dictionary), read once per
    /// dictionary (`font::read_font`; charged to the pass budget, an overrun is a refusal, R8 F3/D3).
    fn font_info(&mut self, value: &Object) -> Result<Option<Rc<Font>>> {
        let doc = self.doc;
        let Some(fd) = doc.resolve(value).as_dict() else { return Ok(None) };
        let address = std::ptr::from_ref(fd) as usize;
        if let Some(f) = self.fonts.get(&address) {
            return Ok(Some(Rc::clone(f)));
        }
        let info = Rc::new(read_font(&doc.objects, fd, &doc.limits, &mut self.budget)?);
        self.fonts.insert(address, Rc::clone(&info));
        Ok(Some(info))
    }

    /// `Tf`: the font a resource name selects in the resources in force.
    fn font_named(&mut self, name: &[u8]) -> Result<FontSel> {
        let doc = self.doc;
        let value = self
            .resources
            .and_then(|r| r.get("Font"))
            .map(|f| doc.resolve(f))
            .and_then(Object::as_dict)
            .and_then(|fonts| fonts.entries.iter().find(|(k, _)| k == name))
            .map(|(_, v)| v);
        Ok(match value {
            Some(v) => self.font_info(v)?.map_or(FontSel::Unresolved, FontSel::Known),
            None => FontSel::Unresolved,
        })
    }

    /// `gs`: an ExtGState sets the font and size with `/Font [font size]` (ISO 32000-2 8.4.5 Table 57;
    /// the operands of `Tf`, the font given as a reference) and the line width with `/LW`.
    fn ext_gstate(&mut self, name: &[u8]) -> Result<()> {
        let doc = self.doc;
        let Some(gs) = self
            .resources
            .and_then(|r| r.get("ExtGState"))
            .map(|g| doc.resolve(g))
            .and_then(Object::as_dict)
            .and_then(|g| g.entries.iter().find(|(k, _)| k == name))
            .map(|(_, v)| doc.resolve(v))
            .and_then(Object::as_dict)
        else {
            return Ok(());
        };
        if let Some(lw) = gs.get("LW").and_then(|w| doc.resolve(w).as_f64()) {
            self.g.lw = lw;
        }
        let Some(entry) = gs.get("Font").map(|f| doc.resolve(f)) else { return Ok(()) };
        let pair = entry.as_array().filter(|a| a.len() == 2);
        self.g.font = match pair.and_then(|a| a.first()) {
            Some(v) => self.font_info(v)?.map_or(FontSel::Unresolved, FontSel::Known),
            None => FontSel::Unresolved,
        };
        match pair.and_then(|a| a.get(1)).and_then(|s| doc.resolve(s).as_f64()) {
            Some(size) => self.g.fs = size,
            None => self.g.font = FontSel::Unresolved,
        }
        Ok(())
    }

    /// `s` as text the way a viewer draws it, and whether that reading is complete
    /// (`font::Font::needle_text`). Incomplete (`false`): no font is determined, a composite font has
    /// no usable map, a code its codespace or map does not name, or a simple font has a code nothing
    /// reads (R8 F3/D3).
    fn reading_of(&self, s: &[u8]) -> (Option<String>, bool) {
        match &self.g.font {
            FontSel::Known(font) => font.needle_text(s),
            FontSel::Unset | FontSel::Unresolved => (None, false),
        }
    }

    /// Reference of the image XObject a resource name selects.
    fn xobject_ref(&self, name: &[u8]) -> Option<ObjRef> {
        let doc = self.doc;
        let xo = self.resources?.get("XObject").map(|x| doc.resolve(x)).and_then(Object::as_dict)?;
        xo.entries.iter().find(|(k, _)| k == name).and_then(|(_, v)| v.as_ref())
    }

    /// Computes the glyph-run box for `s` at the current text position and advances Tm (module docs).
    /// Text whose glyph geometry is not computed here (Type 3, vertical, unresolved writing mode,
    /// undetermined font, unknown widths) marks the page's region.
    fn show(&mut self, s: &[u8]) -> Shown {
        let font = match &self.g.font {
            FontSel::Known(f) => Some(Rc::clone(f)),
            FontSel::Unset | FontSel::Unresolved => None,
        };
        let glyphs = font.as_ref().and_then(|f| f.glyphs(s));
        if !s.is_empty() {
            match &font {
                None => self.mark(RedactionClass::UnresolvedFont),
                Some(f) => {
                    if f.type3 {
                        self.mark(RedactionClass::Type3Text);
                    }
                    match f.wmode {
                        WMode::Horizontal if !f.type3 && (!f.metrics_known() || glyphs.is_none()) => self.mark(RedactionClass::UnknownGlyphMetrics),
                        WMode::Horizontal => {}
                        WMode::Vertical => self.mark(RedactionClass::VerticalText),
                        WMode::Unresolved => self.mark(RedactionClass::UnresolvedWritingMode),
                    }
                }
            }
        }
        // Without a split (undetermined font, unsplittable codes): one glyph per byte, bounded width.
        let widths: Vec<((f64, f64), bool)> = match &glyphs {
            Some(gl) => gl.iter().map(|g| (g.width.unwrap_or((0.0, STD_MAX_WIDTH)), g.space)).collect(),
            None => s.iter().map(|b| ((0.0, STD_MAX_WIDTH), *b == 32 && !font.as_ref().is_some_and(|f| f.composite()))).collect(),
        };
        let gb = font.as_ref().map_or(STD_UNION_BOX, |f| f.bbox);
        let (fs, th) = (self.g.fs, self.g.th);
        let (ya, yb) = (gb[1] / 1000.0 * fs + self.g.ts, gb[3] / 1000.0 * fs + self.g.ts);
        let (xa, xb) = (gb[0] / 1000.0 * fs * th, gb[2] / 1000.0 * fs * th);
        let (ink_lo, ink_hi) = (xa.min(xb), xa.max(xb));
        let mut o = self.slack;
        let (mut xmin, mut xmax) = (o.0, o.1);
        let mut adv = (0.0f64, 0.0f64);
        for ((lo, hi), space) in &widths {
            xmin = xmin.min(o.0 + ink_lo);
            xmax = xmax.max(o.1 + ink_hi);
            let extra = self.g.tc + if *space { self.g.tw } else { 0.0 };
            let (a1, a2) = ((lo / 1000.0 * fs + extra) * th, (hi / 1000.0 * fs + extra) * th);
            let (alo, ahi) = (a1.min(a2), a1.max(a2));
            o = (o.0 + alo, o.1 + ahi);
            adv = (adv.0 + alo, adv.1 + ahi);
        }
        let m = mul(&self.tm, &self.g.ctm);
        let (y0, y1) = (ya.min(yb), ya.max(yb));
        let pts = [apply(&m, xmin, y0), apply(&m, xmax, y0), apply(&m, xmin, y1), apply(&m, xmax, y1)];
        let hit = touches(&bbox(&pts), &self.region);
        self.tm = mul(&translate(adv.0, 0.0), &self.tm);
        self.slack = (self.slack.0, self.slack.1 + (adv.1 - adv.0));
        let exact = font.as_ref().is_some_and(|f| !f.type3) && adv.0 == adv.1;
        Shown { hit, adv: exact.then_some(adv.0) }
    }

    /// Records `s` (a removed string) with its reading and adds it to the current run.
    fn record_removed(&mut self, s: &[u8]) {
        let (u, complete) = self.reading_of(s);
        self.removed.push(s.to_vec());
        self.removed_unicode.push(u.clone());
        self.removed_unreadable.push(!complete);
        if !s.is_empty() {
            self.run.bytes.extend_from_slice(s);
            self.run.pieces.push((s.to_vec(), u, complete));
        }
    }

    /// Ends the run of consecutive removed strings of the content stream (at `Do`, a kept non-empty
    /// string and the end of the content): two or more are also recorded joined, the text a
    /// viewer draws as one run, so pieces shorter than a distinctive needle (kerned `TJ` elements, one
    /// `Tj` per syllable or glyph) are searched as the word they form, and its readability is judged on
    /// the whole run (R8 F2, R9-F3; tests `kerned_tj_runs_are_read_whole`, `removed_runs_join_across_show_operators`,
    /// `glyph_by_glyph_unreadable_text_is_never_proven`).
    fn flush_run(&mut self) {
        let Run { bytes, pieces } = std::mem::take(&mut self.run);
        if pieces.len() < 2 {
            return;
        }
        let complete = pieces.iter().all(|(_, _, c)| *c);
        let unicode = pieces.iter().any(|(_, u, _)| u.is_some()).then(|| {
            pieces
                .iter()
                .map(|(b, u, _)| match u {
                    Some(t) => t.clone(),
                    None => b.iter().map(|x| pdfdoc_char(*x)).collect(),
                })
                .collect::<String>()
        });
        self.removed.push(bytes);
        self.removed_unicode.push(unicode);
        self.removed_unreadable.push(!complete);
    }

    /// Kerning number that reproduces the advance of a removed string, when exactly known (and short
    /// enough to be written canonically).
    fn spacer(&mut self, shown: &Shown) -> Option<f64> {
        let scale = self.g.fs * self.g.th;
        let Some(a) = shown.adv else {
            self.layout_shift = true;
            return None;
        };
        if a == 0.0 {
            return None;
        }
        let n = -a * 1000.0 / scale;
        if scale != 0.0 && n.abs() < 1.0e9 {
            Some(n)
        } else {
            self.layout_shift = true;
            None
        }
    }

    fn line_start(&mut self) {
        self.tm = self.tlm;
        self.slack = (0.0, 0.0);
    }

    fn next_line(&mut self) {
        self.tlm = mul(&translate(0.0, -self.g.tl), &self.tlm);
        self.line_start();
    }

    fn add_point(&mut self, x: f64, y: f64) {
        let (px, py) = apply(&self.g.ctm, x, y);
        let b = self.path.get_or_insert([px, py, px, py]);
        b[0] = b[0].min(px);
        b[1] = b[1].min(py);
        b[2] = b[2].max(px);
        b[3] = b[3].max(py);
    }

    fn paint(&mut self, stroke: bool) {
        if let Some(mut b) = self.path.take() {
            if stroke {
                let det = (self.g.ctm[0] * self.g.ctm[3] - self.g.ctm[1] * self.g.ctm[2]).abs().sqrt();
                let grow = self.g.lw.abs().max(1.0) * det.max(1.0);
                b = [b[0] - grow, b[1] - grow, b[2] + grow, b[3] + grow];
            }
            if touches(&b, &self.region) {
                self.mark(RedactionClass::Vector);
            }
        }
    }

    fn mark(&mut self, class: RedactionClass) {
        if !self.unsupported.contains(&class) {
            self.unsupported.push(class);
        }
    }

    fn xobject(&self, name: &[u8]) -> Option<XObj<'a>> {
        let doc = self.doc;
        let xo = self.resources?.get("XObject").map(|x| doc.resolve(x)).and_then(Object::as_dict)?;
        let obj = xo.entries.iter().find(|(k, _)| k == name).map(|(_, v)| doc.resolve(v))?;
        let everything = [f64::MIN, f64::MIN, f64::MAX, f64::MAX];
        let d = obj.as_dict()?;
        // R10-F3: /Subtype may be an indirect object; viewers dereference it.
        let subtype = d.get("Subtype").map(|s| doc.resolve(s));
        if subtype.is_some_and(|s| s.is_name("Form")) {
            let Object::Stream(stream) = obj else { return None };
            let m: Mat = match d.get("Matrix").map(|m| doc.resolve(m)).and_then(Object::as_array) {
                Some(a) if a.len() == 6 => {
                    let mut out = IDENT;
                    for (slot, v) in out.iter_mut().zip(a) {
                        match doc.resolve(v).as_f64() {
                            Some(x) => *slot = x,
                            // Unreadable matrix: assume the form covers everything (fail closed).
                            None => return Some(XObj { form: Some((stream, IDENT)), bbox: everything }),
                        }
                    }
                    out
                }
                _ => IDENT,
            };
            let full = mul(&m, &self.g.ctm);
            let area = match doc.rect_of(d.get("BBox")) {
                Some(b) => bbox(&[apply(&full, b[0], b[1]), apply(&full, b[2], b[1]), apply(&full, b[0], b[3]), apply(&full, b[2], b[3])]),
                None => everything,
            };
            Some(XObj { form: Some((stream, m)), bbox: area })
        } else if subtype.is_some_and(|s| s.is_name("Image")) {
            let c = &self.g.ctm;
            Some(XObj { form: None, bbox: bbox(&[apply(c, 0.0, 0.0), apply(c, 1.0, 0.0), apply(c, 0.0, 1.0), apply(c, 1.0, 1.0)]) })
        } else {
            None
        }
    }

    fn alloc(&mut self) -> Result<u32> {
        let n = self.next_num;
        self.next_num = n.checked_add(1).ok_or(PressError::InputLimit("max_objects"))?;
        Ok(n)
    }

    fn fresh_name(&mut self, additions: &[(Vec<u8>, ObjRef)]) -> Vec<u8> {
        let doc = self.doc;
        loop {
            let candidate = format!("HskRd{}", self.name_counter).into_bytes();
            self.name_counter = self.name_counter.saturating_add(1);
            let in_resources = self
                .resources
                .and_then(|r| r.get("XObject"))
                .map(|x| doc.resolve(x))
                .and_then(Object::as_dict)
                .is_some_and(|x| x.get_key(&candidate).is_some());
            if !in_resources && !additions.iter().any(|(n, _)| *n == candidate) {
                return candidate;
            }
        }
    }

    /// Direct copy of `base` whose `/XObject` entry gains `additions` and loses `removals`.
    fn resources_with(&self, base: Option<&Dict>, additions: &[(Vec<u8>, ObjRef)], removals: &[Vec<u8>]) -> Dict {
        let mut res = base.cloned().unwrap_or_default();
        let mut xo = res.get("XObject").map(|x| self.doc.resolve(x)).and_then(Object::as_dict).cloned().unwrap_or_default();
        for name in removals {
            xo.remove_key(name);
        }
        for (name, r) in additions {
            xo.set_key(name, Object::Ref(*r));
        }
        res.set("XObject", Object::Dict(xo));
        res
    }

    /// Strict parse of content read with the resources in force (named inline-image colour spaces).
    fn parse(&mut self, data: &[u8]) -> Result<Vec<Op>> {
        let objs = &self.doc.objects;
        let resources = self.resources;
        let cs = move |n: &[u8]| resource_components(objs, resources, n);
        Ok(parse_strict(data, &self.doc.limits, &cs, &mut self.budget)?.ops)
    }

    /// Interprets a form XObject under the current CTM. Returns a copy-on-write replacement form
    /// when text inside it was removed; the original form is left as it is (test form_xobject_text_redaction_copy_on_write).
    fn run_form(&mut self, stream: &'a Stream, matrix: Mat, cancel: &CancellationToken, depth: usize) -> Result<Option<ObjRef>> {
        let data = filters::decode_stream(stream, &self.doc.limits, &mut self.budget)?;
        let doc = self.doc;
        let own_resources = stream.dict.get("Resources").map(|r| doc.resolve(r)).and_then(Object::as_dict);
        let parent_resources = self.resources;
        let saved = (self.g.clone(), std::mem::take(&mut self.stack), self.tm, self.tlm, self.path.take(), self.slack);
        self.g.ctm = mul(&matrix, &self.g.ctm);
        if own_resources.is_some() {
            self.resources = own_resources;
        }
        let pass = self.parse(&data).and_then(|ops| self.run(&ops, &data, cancel, depth));
        let effective = self.resources;
        (self.g, self.stack, self.tm, self.tlm, self.path, self.slack) = saved;
        self.resources = parent_resources;
        let pass = pass?;
        if !pass.changed {
            return Ok(None);
        }
        let mut dict = stream.dict.clone();
        for key in ["Filter", "DecodeParms", "Length", "DL"] {
            dict.remove(key);
        }
        dict.set("Filter", Object::Name(b"FlateDecode".to_vec()));
        if !pass.additions.is_empty() || !pass.removals.is_empty() {
            let rebuilt = self.resources_with(effective, &pass.additions, &pass.removals);
            dict.set("Resources", Object::Dict(rebuilt));
        }
        let data = miniz_oxide::deflate::compress_to_vec_zlib(&pass.out, 6);
        let num = self.alloc()?;
        self.new_objects.push((num, Entry { generation: 0, obj: Object::Stream(Box::new(Stream { dict, data })), span: None, data_at: None }));
        Ok(Some(ObjRef { num, generation: 0 }))
    }

    fn run(&mut self, ops: &[Op], data: &[u8], cancel: &CancellationToken, depth: usize) -> Result<Pass> {
        let mut out: Vec<u8> = Vec::with_capacity(data.len());
        let mut changed = false;
        let mut additions: Vec<(Vec<u8>, ObjRef)> = Vec::new();
        let mut rewritten: BTreeSet<Vec<u8>> = BTreeSet::new();
        let mut uses: BTreeMap<Vec<u8>, u32> = BTreeMap::new();
        for op in ops {
            if op.name == b"Do"
                && let Some(n) = op.operands.first().and_then(Object::as_name)
            {
                {
                    let c = uses.entry(n.to_vec()).or_default();
                    *c = c.saturating_add(1);
                }
            }
        }
        for (i, op) in ops.iter().enumerate() {
            if i % 1024 == 0 {
                check_cancel(cancel)?;
            }
            let mut keep = true;
            match op.name.as_slice() {
                b"q" => {
                    if self.stack.len() >= 256 {
                        return Err(PressError::InputLimit("gstate_depth"));
                    }
                    self.stack.push(self.g.clone());
                }
                b"Q" => {
                    if let Some(g) = self.stack.pop() {
                        self.g = g;
                    }
                }
                b"cm" => self.g.ctm = mul(&matrix(op)?, &self.g.ctm),
                b"w" => self.g.lw = num(op, 0)?,
                b"BT" => {
                    self.tm = IDENT;
                    self.tlm = IDENT;
                    self.slack = (0.0, 0.0);
                }
                b"Tf" => {
                    let name = op.operands.first().and_then(Object::as_name).ok_or(PressError::Malformed("content_operands"))?;
                    self.g.fs = num(op, 1)?;
                    self.g.font = self.font_named(name)?;
                }
                b"gs" => {
                    if let Some(name) = op.operands.first().and_then(Object::as_name) {
                        self.ext_gstate(name)?;
                    }
                }
                b"Tc" => self.g.tc = num(op, 0)?,
                b"Tw" => self.g.tw = num(op, 0)?,
                b"Tz" => self.g.th = num(op, 0)? / 100.0,
                b"TL" => self.g.tl = num(op, 0)?,
                b"Ts" => self.g.ts = num(op, 0)?,
                b"Td" | b"TD" => {
                    let (tx, ty) = (num(op, 0)?, num(op, 1)?);
                    if op.name == b"TD" {
                        self.g.tl = -ty;
                    }
                    self.tlm = mul(&translate(tx, ty), &self.tlm);
                    self.line_start();
                }
                b"Tm" => {
                    self.tlm = matrix(op)?;
                    self.line_start();
                }
                b"T*" => self.next_line(),
                b"Tj" | b"'" | b"\"" => {
                    let si = if op.name == b"\"" {
                        self.g.tw = num(op, 0)?;
                        self.g.tc = num(op, 1)?;
                        2
                    } else {
                        0
                    };
                    if op.name != b"Tj" {
                        self.next_line();
                    }
                    let Some(Object::Str(s)) = op.operands.get(si) else {
                        return Err(PressError::Malformed("content_operands"));
                    };
                    let shown = self.show(s);
                    if shown.hit {
                        self.strings_removed = self.strings_removed.saturating_add(1);
                        self.record_removed(s);
                        changed = true;
                        keep = false;
                        // The removed operator's other effects stay: `"` sets Tw and Tc, `'` and `"`
                        // move to the next line.
                        if op.name == b"\"" {
                            emit(b"Tw", op.operands.first().cloned().into_iter().collect(), &mut out)?;
                            emit(b"Tc", op.operands.get(1).cloned().into_iter().collect(), &mut out)?;
                        }
                        if op.name != b"Tj" {
                            emit(b"T*", Vec::new(), &mut out)?;
                        }
                        if let Some(n) = self.spacer(&shown) {
                            emit(b"TJ", vec![Object::Array(vec![Object::Real(n)])], &mut out)?;
                        }
                    } else if !s.is_empty() {
                        self.flush_run();
                    }
                }
                b"TJ" => {
                    let Some(Object::Array(items)) = op.operands.first() else {
                        return Err(PressError::Malformed("content_operands"));
                    };
                    let mut new_items: Vec<Object> = Vec::with_capacity(items.len());
                    let mut any = false;
                    // Adjacent removed elements extend the content stream's run (flush_run).
                    for item in items {
                        match item {
                            Object::Str(s) => {
                                let shown = self.show(s);
                                if shown.hit {
                                    any = true;
                                    self.strings_removed = self.strings_removed.saturating_add(1);
                                    self.record_removed(s);
                                    if let Some(n) = self.spacer(&shown) {
                                        new_items.push(Object::Real(n));
                                    }
                                } else {
                                    if !s.is_empty() {
                                        self.flush_run();
                                    }
                                    new_items.push(item.clone());
                                }
                            }
                            Object::Int(_) | Object::Real(_) => {
                                let n = item.as_f64().unwrap_or(0.0);
                                self.tm = mul(&translate(-n / 1000.0 * self.g.fs * self.g.th, 0.0), &self.tm);
                                new_items.push(item.clone());
                            }
                            _ => return Err(PressError::Malformed("tj_element")),
                        }
                    }
                    if any {
                        changed = true;
                        keep = false;
                        emit(b"TJ", vec![Object::Array(new_items)], &mut out)?;
                    }
                }
                b"m" | b"l" => {
                    let (x, y) = (num(op, 0)?, num(op, 1)?);
                    self.add_point(x, y);
                }
                b"c" => {
                    for k in 0..3usize {
                        let (x, y) = (num(op, k.saturating_mul(2))?, num(op, k.saturating_mul(2).saturating_add(1))?);
                        self.add_point(x, y);
                    }
                }
                b"v" | b"y" => {
                    for k in 0..2usize {
                        let (x, y) = (num(op, k.saturating_mul(2))?, num(op, k.saturating_mul(2).saturating_add(1))?);
                        self.add_point(x, y);
                    }
                }
                b"re" => {
                    let (x, y, w, h) = (num(op, 0)?, num(op, 1)?, num(op, 2)?, num(op, 3)?);
                    for (px, py) in [(x, y), (x + w, y), (x, y + h), (x + w, y + h)] {
                        self.add_point(px, py);
                    }
                }
                b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*" => self.paint(true),
                b"f" | b"F" | b"f*" => self.paint(false),
                b"n" => self.path = None,
                b"sh" => self.mark(RedactionClass::Shading),
                b"BI" => {
                    let c = self.g.ctm;
                    let b = bbox(&[apply(&c, 0.0, 0.0), apply(&c, 1.0, 0.0), apply(&c, 0.0, 1.0), apply(&c, 1.0, 1.0)]);
                    if touches(&b, &self.region) {
                        if self.remove_images {
                            self.inline_removed = self.inline_removed.saturating_add(1);
                            changed = true;
                            keep = false;
                        } else {
                            self.mark(RedactionClass::InlineImage);
                        }
                    }
                }
                b"Do" => {
                    self.flush_run();
                    let name = op.operands.first().and_then(Object::as_name).ok_or(PressError::Malformed("content_operands"))?;
                    if let Some(x) = self.xobject(name)
                        && touches(&x.bbox, &self.region)
                    {
                        match x.form {
                            None if self.remove_images => {
                                if let Some(r) = self.xobject_ref(name)
                                    && !self.images_removed.contains(&r)
                                {
                                    self.images_removed.push(r);
                                }
                                if let Some(count) = uses.get_mut(name) {
                                    *count = count.saturating_sub(1);
                                }
                                rewritten.insert(name.to_vec());
                                changed = true;
                                keep = false;
                            }
                            None => self.mark(RedactionClass::Image),
                            Some(_) if depth >= MAX_FORM_DEPTH => self.mark(RedactionClass::FormXObject),
                            Some((stream, matrix)) => {
                                if let Some(copy) = self.run_form(stream, matrix, cancel, depth.saturating_add(1))? {
                                    let fresh = self.fresh_name(&additions);
                                    emit(b"Do", vec![Object::Name(fresh.clone())], &mut out)?;
                                    if let Some(count) = uses.get_mut(name) {
                                        *count = count.saturating_sub(1);
                                    }
                                    rewritten.insert(name.to_vec());
                                    additions.push((fresh, copy));
                                    changed = true;
                                    keep = false;
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            if keep {
                write_op(op, data, true, &mut out)?;
            }
        }
        self.flush_run();
        let removals: Vec<Vec<u8>> = rewritten.into_iter().filter(|n| uses.get(n).copied().unwrap_or(0) == 0).collect();
        Ok(Pass { out, changed, additions, removals })
    }
}

pub(crate) fn res<'a>(m: &'a BTreeMap<u32, Entry>, mut o: &'a Object) -> &'a Object {
    static NULL: Object = Object::Null;
    for _ in 0..MAX_REF_HOPS {
        match o {
            Object::Ref(r) => match m.get(&r.num) {
                Some(e) => o = &e.obj,
                None => return &NULL,
            },
            _ => return o,
        }
    }
    &NULL
}

fn purge_list(
    m: &mut BTreeMap<u32, Entry>,
    items: Vec<Object>,
    removed: &mut BTreeSet<u32>,
    depth: usize,
    max_depth: usize,
    fields_removed: &mut u32,
) -> Result<Vec<Object>> {
    if depth > max_depth {
        return Err(PressError::InputLimit("depth"));
    }
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Some(r) = item.as_ref() else {
            out.push(item);
            continue;
        };
        if removed.contains(&r.num) {
            *fields_removed = fields_removed.saturating_add(1);
            continue;
        }
        let kids: Option<Vec<Object>> = m
            .get(&r.num)
            .and_then(|e| e.obj.as_dict())
            .and_then(|d| d.get("Kids"))
            .map(|k| res(m, k))
            .and_then(Object::as_array)
            .map(<[Object]>::to_vec);
        if let Some(kids) = kids {
            let before = kids.len();
            let kept = purge_list(m, kids, removed, depth.saturating_add(1), max_depth, fields_removed)?;
            if kept.len() < before {
                if kept.is_empty() {
                    removed.insert(r.num);
                    *fields_removed = fields_removed.saturating_add(1);
                    continue;
                }
                if let Some(Entry { obj: Object::Dict(d), span, .. }) = m.get_mut(&r.num) {
                    d.set("Kids", Object::Array(kept));
                    *span = None;
                }
            }
        }
        out.push(item);
    }
    Ok(out)
}

impl PdfDoc {
    fn page_content_streams(&self, page: u32) -> Result<Vec<u32>> {
        let d = self.dict_of(page).ok_or(PressError::NoSuchPage)?;
        let Some(c) = d.get("Contents") else { return Ok(Vec::new()) };
        let nums = match (c, self.resolve(c)) {
            (Object::Ref(r), Object::Stream(_)) => vec![r.num],
            (_, Object::Array(a)) => a.iter().filter_map(Object::as_ref).map(|r| r.num).collect(),
            (_, Object::Null) => Vec::new(),
            _ => return Err(PressError::Malformed("page_contents")),
        };
        Ok(nums)
    }

    /// Irreversibly removes the text-showing operators and annotations that touch `region` from
    /// the in-memory document. Call `save_full` (or `redact_and_save`) afterwards; nothing is
    /// proven until the saved bytes pass `verify_payload_absent`. This is the selector alone: it does
    /// NOT flatten the page (only `redact_and_save` / `redact_and_save_with` do, Operator decision D27),
    /// so images, vectors and text it cannot place stay on the page.
    pub fn redact_region(
        &mut self,
        region: &RedactionRegion,
        policy: UnsupportedPolicy,
        cancel: &CancellationToken,
    ) -> Result<RedactionReport> {
        self.select_region(region, policy, false, cancel)
    }

    /// The region selector. With `flattening` (the page is replaced by its raster afterwards) the
    /// painted classes it does not remove (images, vectors, shadings, inline images) are listed in
    /// `painted_over` instead of refused or `unredacted`; text it cannot place and forms nested past
    /// its depth still count, because the words they draw are the needles of the proof.
    pub(crate) fn select_region(
        &mut self,
        region: &RedactionRegion,
        policy: UnsupportedPolicy,
        flattening: bool,
        cancel: &CancellationToken,
    ) -> Result<RedactionReport> {
        self.guard_mutation(cancel)?;
        let page_num = *self.pages.get(region.page_index as usize).ok_or(PressError::NoSuchPage)?;
        let r = region.rect;
        if !r.iter().all(|v| v.is_finite()) {
            return Err(PressError::Malformed("region"));
        }
        let rect = [r[0].min(r[2]), r[1].min(r[3]), r[0].max(r[2]), r[1].max(r[3])];

        // 1. Page content: decode, tokenize, interpret, rewrite.
        let stream_nums = self.page_content_streams(page_num)?;
        let mut budget = self.limits.max_decoded_total;
        let mut data: Vec<u8> = Vec::new();
        for n in &stream_nums {
            check_cancel(cancel)?;
            let Some(Entry { obj: Object::Stream(s), .. }) = self.objects.get(n) else {
                return Err(PressError::Malformed("content_stream"));
            };
            data.extend_from_slice(&filters::decode_stream(s, &self.limits, &mut budget)?);
            data.push(b'\n');
        }
        let resources = self.inherited(page_num, "Resources").and_then(Object::as_dict);
        let mut interp = Interp::new(self, resources, rect, budget);
        interp.remove_images = policy == UnsupportedPolicy::RemoveImages;
        let ops = interp.parse(&data)?;
        let pass = interp.run(&ops, &data, cancel, 0)?;
        let page_resources = if pass.additions.is_empty() && pass.removals.is_empty() {
            None
        } else {
            Some(interp.resources_with(resources, &pass.additions, &pass.removals))
        };
        let Interp { mut unsupported, removed, strings_removed, removed_unicode, removed_unreadable, layout_shift, new_objects, next_num, images_removed, inline_removed, .. } = interp;
        let mut painted_over: Vec<RedactionClass> = Vec::new();
        if flattening {
            let painted = |c: &RedactionClass| matches!(c, RedactionClass::Image | RedactionClass::Vector | RedactionClass::Shading | RedactionClass::InlineImage);
            painted_over = unsupported.iter().copied().filter(painted).collect();
            painted_over.sort_by_key(|c| c.as_str());
            unsupported.retain(|c| !painted(c));
        }
        let (new_content, changed) = (pass.out, pass.changed);
        if policy != UnsupportedPolicy::ReportAndContinue
            && let Some(class) = unsupported.first()
        {
            return Err(PressError::UnsupportedRedactionClass(*class));
        }

        // 2. Stage every mutation on a copy; commit only after the reachability proof.
        let mut staged = self.objects.clone();
        staged.extend(new_objects);
        if changed {
            let new_num = next_num;
            let mut dict = Dict::default();
            dict.set("Filter", Object::Name(b"FlateDecode".to_vec()));
            let data = miniz_oxide::deflate::compress_to_vec_zlib(&new_content, 6);
            staged.insert(new_num, Entry { generation: 0, obj: Object::Stream(Box::new(Stream { dict, data })), span: None, data_at: None });
            let page = staged.get_mut(&page_num).ok_or(PressError::NoSuchPage)?;
            if let Object::Dict(d) = &mut page.obj {
                d.set("Contents", Object::Ref(ObjRef { num: new_num, generation: 0 }));
                if let Some(rebuilt) = page_resources {
                    d.set("Resources", Object::Dict(rebuilt));
                }
            }
            page.span = None;
        }

        // 3. Annotations touching the region (plus popups / children of removed annotations).
        let items: Vec<Object> = match self.dict_of(page_num).and_then(|d| d.get("Annots")).map(|a| self.resolve(a)) {
            Some(Object::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
        let mut removed_refs: BTreeSet<u32> = BTreeSet::new();
        for item in &items {
            if let Some(rf) = item.as_ref() {
                let hit = self
                    .dict_of(rf.num)
                    .and_then(|ad| self.rect_of(ad.get("Rect")))
                    .is_some_and(|ar| touches(&ar, &rect));
                if hit {
                    removed_refs.insert(rf.num);
                }
            }
        }
        loop {
            let before = removed_refs.len();
            for item in &items {
                let Some(rf) = item.as_ref() else { continue };
                let Some(ad) = self.dict_of(rf.num) else { continue };
                if removed_refs.contains(&rf.num)
                    && let Some(p) = ad.get("Popup").and_then(Object::as_ref)
                {
                    removed_refs.insert(p.num);
                }
                if ad.get("Parent").and_then(Object::as_ref).is_some_and(|p| removed_refs.contains(&p.num)) {
                    removed_refs.insert(rf.num);
                }
            }
            if removed_refs.len() == before {
                break;
            }
        }
        let kept: Vec<Object> = items
            .iter()
            .filter(|i| match i {
                Object::Ref(rf) => !removed_refs.contains(&rf.num),
                Object::Dict(d) => !self.rect_of(d.get("Rect")).is_some_and(|ar| touches(&ar, &rect)),
                _ => true,
            })
            .cloned()
            .collect();
        let mut annotations_removed: Vec<ObjRef> =
            removed_refs.iter().filter_map(|n| self.objects.get(n).map(|e| ObjRef { num: *n, generation: e.generation })).collect();
        annotations_removed.sort();
        let mut form_fields_removed = 0u32;
        if kept.len() < items.len() {
            let page = staged.get_mut(&page_num).ok_or(PressError::NoSuchPage)?;
            if let Object::Dict(d) = &mut page.obj {
                d.set("Annots", Object::Array(kept));
            }
            page.span = None;
            // 4. Drop every AcroForm field reference to the removed widgets (and emptied parents).
            let root = self.trailer.get("Root").and_then(Object::as_ref).ok_or(PressError::Malformed("no_root"))?.num;
            let form_target: Option<(u32, bool)> = match self.catalog()?.get("AcroForm") {
                Some(Object::Ref(r)) => Some((r.num, false)),
                Some(Object::Dict(_)) => Some((root, true)),
                _ => None,
            };
            if let Some((holder, nested)) = form_target {
                let fields: Option<Vec<Object>> = {
                    let form = if nested {
                        staged.get(&holder).and_then(|e| e.obj.as_dict()).and_then(|c| c.get("AcroForm")).and_then(Object::as_dict)
                    } else {
                        staged.get(&holder).and_then(|e| e.obj.as_dict())
                    };
                    form.and_then(|f| f.get("Fields")).map(|f| res(&staged, f)).and_then(Object::as_array).map(<[Object]>::to_vec)
                };
                if let Some(fields) = fields {
                    let before = fields.len();
                    let new_fields = purge_list(&mut staged, fields, &mut removed_refs, 1, self.limits.depth_cap(), &mut form_fields_removed)?;
                    if new_fields.len() < before
                        && let Some(e) = staged.get_mut(&holder)
                    {
                        let target = if nested {
                            match &mut e.obj {
                                Object::Dict(c) => c
                                    .entries
                                    .iter_mut()
                                    .find(|(k, _)| k == b"AcroForm")
                                    .and_then(|(_, v)| if let Object::Dict(d) = v { Some(d) } else { None }),
                                _ => None,
                            }
                        } else if let Object::Dict(d) = &mut e.obj {
                            Some(d)
                        } else {
                            None
                        };
                        if let Some(d) = target {
                            d.set("Fields", Object::Array(new_fields));
                        }
                        e.span = None;
                    }
                }
            }
        }

        // 5. Proof: no removed annotation or field object remains reachable from the trailer.
        let live = reachable(&staged, &trailer_roots(&self.trailer));
        if removed_refs.iter().any(|n| live.contains(n)) {
            return Err(PressError::RedactionIncomplete);
        }
        self.objects = staged;
        let mut unredacted = unsupported;
        unredacted.sort_by_key(|c| c.as_str());
        Ok(RedactionReport {
            page_index: region.page_index,
            text_strings_removed: strings_removed,
            removed_text: removed,
            removed_unicode,
            removed_unreadable,
            annotations_removed,
            images_removed,
            inline_images_removed: inline_removed,
            form_fields_removed,
            complete: unredacted.is_empty(),
            unredacted,
            layout_shift_possible: layout_shift,
            painted_over,
        })
    }
}
