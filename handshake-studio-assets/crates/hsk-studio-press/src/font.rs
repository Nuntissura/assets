//! Fonts as a viewer draws them, read once and shared by the redaction selector (`redaction.rs`) and
//! the proof's determination pass (`show_text.rs`), so both read the same codes, text, writing mode
//! and glyph metrics (lead decision 2026-10-11T15:00:20Z, R10-F4/F6):
//! - codes: a simple font draws one glyph per byte; a composite font's string is split into codes by
//!   the codespace ranges of its ENCODING CMap (`determination::encoding_cmap`, `cmap::read_code`),
//!   never by its `/ToUnicode` map; a code outside every range, or a font whose codespace is not
//!   positively known, has no reading;
//! - text: a simple font through its encoding (`encoding.rs`) with `/ToUnicode` for codes the encoding
//!   leaves open; a composite font through `/ToUnicode` looked up by each code's numeric value;
//! - advance widths in glyph space (1/1000 em), as an interval `[lo, hi]` (exact when equal):
//!   a simple font's `/Widths` (`/FirstChar`; a code outside the array or a non-number entry takes the
//!   descriptor's `/MissingWidth`, default 0: ISO 32000-2 9.6.2.1, pdf.js `extractWidths`); without
//!   `/Widths`, one of the 14 standard fonts (exact `/BaseFont` name, not embedded) by the advance
//!   width of the glyph its encoding names in Adobe's AFM metrics (`std14_data.rs`; a name the AFM
//!   lacks reads `[0, widest glyph]`); any other non-embedded font is drawn by viewers with a
//!   substitute standard font, so `[0, widest standard glyph]`; an embedded simple font without
//!   `/Widths` has unknown metrics ([`Font::metrics_known`], `RedactionClass::UnknownGlyphMetrics`); a
//!   composite font's widths come from its CIDFont's `/W` and `/DW` (default 1000) by CID (ISO
//!   32000-2 9.7.4.3), the CID from the encoding CMap (identity, or the embedded `cidrange` /
//!   `cidchar` entries); a CID that is not known, overlapping `/W` entries or a malformed `/W` read
//!   `[least, greatest]` of every width the font declares;
//! - ink extent: the descriptor's `/FontBBox` (the AFM box for a standard font) UNITED with the union of
//!   the 14 standard fonts' boxes `[-200, -293, 1116, 1010]` ([`STD_UNION_BOX`]), so a missing,
//!   degenerate or too small declared box never shrinks the region test below what a viewer's
//!   substitute font draws. Glyph outlines a font program draws outside both boxes are not covered
//!   (DESCRIPTOR `proof.not_covered`).
//!
//! Tests: `composite_geometry_uses_w_dw_and_signed_size`, `standard_fonts_use_their_afm_widths`,
//! `composite_codes_follow_the_encoding_codespace` and the unit tests below.
#![deny(clippy::arithmetic_side_effects)]
use crate::cmap::{Cmap, Space, code_bytes, parse_to_unicode, read_code};
use crate::determination::{CidMap, EncodingCmap, WMode, encoding_cmap};
use crate::encoding::{Base, GlyphSource, SimpleEncoding, glyph_unicode, is_composite};
use crate::encoding_data::{MAC_ROMAN, STANDARD, WIN_ANSI};
use crate::error::{Limits, PressError, Result};
use crate::filters;
use crate::redaction::res;
use crate::show_text::Objs;
use crate::std14_data::{FONTS as STD14, Std14};
use crate::syntax::{Dict, Object};
use crate::text_codec::pdfdoc_char;

/// Union of the `FontBBox` of the 14 standard fonts (Times-BoldItalic -200, Symbol -293 and 1010,
/// Helvetica-Oblique 1116; `std14_data.rs`, unit test `std14_union_box_is_derived_from_the_afm_files`).
pub(crate) const STD_UNION_BOX: [f64; 4] = [-200.0, -293.0, 1116.0, 1010.0];
/// Widest glyph advance of the 14 standard fonts (Symbol `arrowboth` and `arrowdblboth`, 1042).
pub(crate) const STD_MAX_WIDTH: f64 = 1042.0;
/// Most `/W` elements read; a longer array reads as malformed (every CID `[least, greatest]`).
const MAX_W_ELEMENTS: usize = 1 << 20;
/// Decoded-byte budget one `/W` element costs.
const W_ELEMENT_COST: usize = 16;

/// A width interval in glyph space (1/1000 em).
pub(crate) type Width = (f64, f64);

/// One code of a shown string.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Glyph {
    pub code: u32,
    /// Byte length of the code.
    pub len: usize,
    /// Advance width interval (`None`: the font's metrics are unknown).
    pub width: Option<Width>,
    /// Word spacing applies: the single-byte code 32 (ISO 32000-2 9.3.3).
    pub space: bool,
}

/// A composite font's CIDFont widths.
struct CidWidths {
    /// `(first CID, last CID, width)`, sorted by first CID, not overlapping.
    entries: Vec<(u32, u32, f64)>,
    dw: f64,
    /// Every declared width and `/DW`: the interval of a CID whose width is not known exactly.
    bound: Width,
    /// `/W` overlapped itself or was malformed: no CID width is exact.
    loose: bool,
}

impl CidWidths {
    fn width(&self, cid: Option<u32>) -> Width {
        let Some(cid) = cid.filter(|_| !self.loose) else { return self.bound };
        let i = self.entries.partition_point(|(first, _, _)| *first <= cid);
        match i.checked_sub(1).and_then(|k| self.entries.get(k)) {
            Some((_, last, w)) if cid <= *last => (*w, *w),
            _ => (self.dw, self.dw),
        }
    }
}

enum Metrics {
    /// Per byte code.
    Simple(Vec<Width>),
    Cid(CidWidths),
    Unknown,
}

pub(crate) enum Kind {
    Simple(SimpleEncoding),
    /// Codespace (`None`: not positively known) and code -> CID map of the encoding CMap.
    Composite { space: Option<Vec<Space>>, cid: CidMap },
}

/// A font dictionary read for drawing (module docs).
pub(crate) struct Font {
    pub kind: Kind,
    pub tounicode: Option<Cmap>,
    pub wmode: WMode,
    pub type3: bool,
    /// Ink box in glyph space (1/1000 em), `[llx, lly, urx, ury]`.
    pub bbox: [f64; 4],
    metrics: Metrics,
}

/// The text of one shown string.
pub(crate) struct Reading {
    /// The text the font draws (a guess of Latin-1 bytes when `complete` is false and nothing reads).
    pub text: Vec<char>,
    /// A simple font's `/ToUnicode` reading (what text extraction may report).
    pub alt: Option<Vec<char>>,
    /// Every code was read.
    pub complete: bool,
}

fn latin1(s: &[u8]) -> Vec<char> {
    s.iter().map(|b| char::from(*b)).collect()
}

fn number(objs: &Objs, v: Option<&Object>) -> Option<f64> {
    v.map(|x| res(objs, x)).and_then(Object::as_f64)
}

fn descriptor<'a>(objs: &'a Objs, fd: &'a Dict) -> Option<&'a Dict> {
    fd.get("FontDescriptor").map(|d| res(objs, d)).and_then(Object::as_dict)
}

fn embedded(desc: Option<&Dict>) -> bool {
    desc.is_some_and(|d| ["FontFile", "FontFile2", "FontFile3"].iter().any(|k| d.get(k).is_some()))
}

/// The descriptor's `/FontBBox` when it is a box with area.
fn declared_box(objs: &Objs, desc: Option<&Dict>) -> Option<[f64; 4]> {
    let a = desc?.get("FontBBox").map(|b| res(objs, b)).and_then(Object::as_array)?;
    if a.len() != 4 {
        return None;
    }
    let mut v = [0.0f64; 4];
    for (slot, x) in v.iter_mut().zip(a) {
        *slot = res(objs, x).as_f64()?;
    }
    let b = [v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3])];
    (b[2] > b[0] && b[3] > b[1]).then_some(b)
}

fn union(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

/// The standard font a `/BaseFont` names exactly (ISO 32000-2 9.6.2.2).
fn std14(objs: &Objs, fd: &Dict) -> Option<&'static Std14> {
    let name = fd.get("BaseFont").map(|b| res(objs, b)).and_then(Object::as_name)?;
    STD14.iter().find(|f| f.name.as_bytes() == name)
}

/// Advance width of the glyph `name` in a standard font.
fn afm_by_name(f: &Std14, name: &[u8]) -> Option<f64> {
    f.glyphs.binary_search_by(|(n, _, _)| n.as_bytes().cmp(name)).ok().and_then(|i| f.glyphs.get(i)).map(|(_, _, w)| f64::from(*w))
}

/// Advance width of the glyph at `code` of the font's own encoding (StandardEncoding for the Latin
/// fonts, the built-in encoding of Symbol and ZapfDingbats).
fn afm_by_code(f: &Std14, code: u8) -> Option<f64> {
    f.glyphs.iter().find(|(_, c, _)| *c == i16::from(code)).map(|(_, _, w)| f64::from(*w))
}

/// Advance width of the glyph whose Adobe Glyph List text is `c`.
fn afm_by_char(f: &Std14, c: char) -> Option<f64> {
    let mut found = f.glyphs.iter().filter(|(n, _, _)| glyph_unicode(n.as_bytes()).is_some_and(|t| t.chars().eq(std::iter::once(c))));
    let (_, _, w) = found.next()?;
    // Two glyphs for one character would make the width ambiguous.
    found.next().is_none().then(|| f64::from(*w))
}

fn base_char(base: Base, code: u8) -> Option<char> {
    let v = match base {
        Base::Standard => STANDARD.get(usize::from(code)).copied(),
        Base::WinAnsi => WIN_ANSI.get(usize::from(code)).copied(),
        Base::MacRoman => MAC_ROMAN.get(usize::from(code)).copied(),
        Base::PdfDoc => return Some(pdfdoc_char(code)).filter(|c| *c != '\0'),
        Base::Empty | Base::BuiltIn | Base::Other => None,
    }?;
    char::from_u32(u32::from(v)).filter(|_| v != 0)
}

/// Width of `code` in a standard font with encoding `enc` (module docs).
fn std14_width(f: &Std14, enc: &SimpleEncoding, code: u8) -> Width {
    let latin = !matches!(f.name, "Symbol" | "ZapfDingbats");
    let exact = match enc.glyph_source(code) {
        GlyphSource::Name(n) => afm_by_name(f, n),
        // The font's own encoding: StandardEncoding for the Latin fonts, built-in for the symbol fonts.
        GlyphSource::Base(Base::Standard, c) if latin => afm_by_code(f, c),
        GlyphSource::Base(Base::BuiltIn | Base::Empty, c) => afm_by_code(f, c),
        GlyphSource::Base(base, c) => base_char(base, c).and_then(|ch| afm_by_char(f, ch)),
    };
    exact.map_or((0.0, f64::from(f.max_width)), |w| (w, w))
}

/// Reads a font dictionary (module docs). Decoding is charged to `budget`; a budget overrun or
/// cancellation is returned, never read as "no map".
pub(crate) fn read_font(objs: &Objs, fd: &Dict, limits: &Limits, budget: &mut usize) -> Result<Font> {
    let mut tounicode = None;
    if let Some(Object::Stream(tu)) = fd.get("ToUnicode").map(|v| res(objs, v))
        && filters::is_decodable(&tu.dict)
    {
        match filters::decode_stream(tu, limits, budget) {
            Ok(data) => tounicode = parse_to_unicode(&data),
            Err(e @ (PressError::InputLimit(_) | PressError::Canceled)) => return Err(e),
            Err(_) => {}
        }
    }
    if is_composite(objs, fd) {
        let EncodingCmap { wmode, space, cid } = encoding_cmap(objs, fd.get("Encoding"), limits, budget, 0)?;
        let desc_font = fd
            .get("DescendantFonts")
            .map(|d| res(objs, d))
            .and_then(Object::as_array)
            .and_then(|a| a.first())
            .map(|d| res(objs, d))
            .and_then(Object::as_dict);
        let cidfont = desc_font.unwrap_or(fd);
        let widths = cid_widths(objs, cidfont, budget)?;
        let bbox = declared_box(objs, descriptor(objs, cidfont)).map_or(STD_UNION_BOX, |b| union(b, STD_UNION_BOX));
        return Ok(Font { kind: Kind::Composite { space, cid }, tounicode, wmode, type3: false, bbox, metrics: Metrics::Cid(widths) });
    }
    let type3 = fd.get("Subtype").map(|t| res(objs, t)).is_some_and(|t| t.is_name("Type3"));
    let enc = SimpleEncoding::of_font(objs, fd, budget)?;
    let desc = descriptor(objs, fd);
    let std = std14(objs, fd).filter(|_| !embedded(desc));
    let metrics = match fd.get("Widths").map(|w| res(objs, w)) {
        Some(Object::Array(a)) => {
            let first = number(objs, fd.get("FirstChar")).filter(|f| f.fract() == 0.0).unwrap_or(0.0);
            let missing = number(objs, desc.and_then(|d| d.get("MissingWidth"))).unwrap_or(0.0);
            let mut v = vec![(missing, missing); 256];
            for (i, w) in a.iter().enumerate() {
                let code = first + i as f64;
                if (0.0..256.0).contains(&code)
                    && let Some(w) = res(objs, w).as_f64()
                    && let Some(slot) = v.get_mut(code as usize)
                {
                    *slot = (w, w);
                }
            }
            Metrics::Simple(v)
        }
        _ if type3 => Metrics::Unknown,
        _ => match std {
            Some(f) => Metrics::Simple((0..=255u8).map(|c| std14_width(f, &enc, c)).collect()),
            None if !embedded(desc) => Metrics::Simple(vec![(0.0, STD_MAX_WIDTH); 256]),
            None => Metrics::Unknown,
        },
    };
    let declared = declared_box(objs, desc).or_else(|| std.map(|f| f.bbox.map(f64::from)));
    let bbox = declared.map_or(STD_UNION_BOX, |b| union(b, STD_UNION_BOX));
    Ok(Font { kind: Kind::Simple(enc), tounicode, wmode: WMode::Horizontal, type3, bbox, metrics })
}

/// A CIDFont's `/W` and `/DW` (module docs; pdf.js `extractWidths` reads `/W` the same way).
fn cid_widths(objs: &Objs, cidfont: &Dict, budget: &mut usize) -> Result<CidWidths> {
    let dw = number(objs, cidfont.get("DW")).unwrap_or(1000.0);
    let mut entries: Vec<(u32, u32, f64)> = Vec::new();
    let mut loose = false;
    if let Some(w) = cidfont.get("W").map(|w| res(objs, w)) {
        match w.as_array() {
            Some(items) if items.len() <= MAX_W_ELEMENTS => {
                *budget = budget.checked_sub(items.len().saturating_add(16).saturating_mul(W_ELEMENT_COST)).ok_or(PressError::InputLimit("decoded_total"))?;
                let cid = |o: &Object| res(objs, o).as_int().and_then(|v| u32::try_from(v).ok());
                let mut i = 0usize;
                while let Some(start) = items.get(i) {
                    let Some(first) = cid(start) else {
                        loose = true;
                        break;
                    };
                    match items.get(i.saturating_add(1)).map(|x| res(objs, x)) {
                        Some(Object::Array(ws)) => {
                            *budget = budget.checked_sub(ws.len().saturating_mul(W_ELEMENT_COST)).ok_or(PressError::InputLimit("decoded_total"))?;
                            for (k, wv) in ws.iter().enumerate() {
                                match (res(objs, wv).as_f64(), u32::try_from(k).ok().and_then(|k| first.checked_add(k))) {
                                    (Some(w), Some(c)) => entries.push((c, c, w)),
                                    _ => loose = true,
                                }
                            }
                            i = i.saturating_add(2);
                        }
                        Some(o) => match (o.as_int().and_then(|v| u32::try_from(v).ok()), items.get(i.saturating_add(2)).and_then(|x| res(objs, x).as_f64())) {
                            (Some(last), Some(w)) if last >= first => {
                                entries.push((first, last, w));
                                i = i.saturating_add(3);
                            }
                            _ => {
                                loose = true;
                                break;
                            }
                        },
                        None => {
                            loose = true;
                            break;
                        }
                    }
                    if entries.len() > MAX_W_ELEMENTS {
                        loose = true;
                        break;
                    }
                }
            }
            _ => loose = true,
        }
    }
    entries.sort_by_key(|(first, _, _)| *first);
    if entries.windows(2).any(|p| p[1].0 <= p[0].1) {
        loose = true;
    }
    let bound = entries.iter().fold((dw, dw), |(lo, hi), (_, _, w)| (lo.min(*w), hi.max(*w)));
    Ok(CidWidths { entries, dw, bound, loose })
}

impl Font {
    pub(crate) fn composite(&self) -> bool {
        matches!(self.kind, Kind::Composite { .. })
    }

    /// The glyph metrics are known (exactly or as a bound): not an embedded simple font without
    /// `/Widths` (Type 3 geometry is not computed in any case).
    pub(crate) fn metrics_known(&self) -> bool {
        !matches!(self.metrics, Metrics::Unknown)
    }

    /// The codes of `s` with their widths (`None`: a composite code cannot be split; module docs).
    pub(crate) fn glyphs(&self, s: &[u8]) -> Option<Vec<Glyph>> {
        let mut out = Vec::with_capacity(s.len());
        match &self.kind {
            Kind::Simple(_) => {
                for b in s {
                    let width = match &self.metrics {
                        Metrics::Simple(v) => v.get(usize::from(*b)).copied(),
                        _ => None,
                    };
                    out.push(Glyph { code: u32::from(*b), len: 1, width, space: *b == 32 });
                }
            }
            Kind::Composite { space, cid } => {
                let space = space.as_deref()?;
                let mut pos = 0usize;
                while pos < s.len() {
                    let (code, len) = read_code(space, s, pos)?;
                    let width = match &self.metrics {
                        Metrics::Cid(w) => Some(w.width(cid.cid(code))),
                        _ => None,
                    };
                    out.push(Glyph { code, len, width, space: len == 1 && code == 32 });
                    pos = pos.saturating_add(len);
                }
            }
        }
        Some(out)
    }

    /// The text `s` draws (module docs).
    pub(crate) fn read(&self, s: &[u8]) -> Reading {
        match &self.kind {
            Kind::Simple(enc) => {
                let r = enc.read(s, self.tounicode.as_ref());
                Reading { text: r.text, alt: self.tounicode.as_ref().map(|c| c.read_bytes(s)), complete: !r.opaque }
            }
            Kind::Composite { .. } => {
                let (Some(tu), Some(glyphs)) = (&self.tounicode, self.glyphs(s)) else {
                    return Reading { text: latin1(s), alt: None, complete: s.is_empty() };
                };
                let mut text = Vec::new();
                let mut complete = true;
                for g in glyphs {
                    match tu.lookup(g.code) {
                        Some(t) => text.extend(t.chars()),
                        None => complete = false,
                    }
                }
                Reading { text, alt: None, complete }
            }
        }
    }

    /// The codes under which this font draws `text`, one code per character: through its encoding
    /// (simple fonts) and through its `/ToUnicode` map (code value -> bytes by the codespace).
    pub(crate) fn encode(&self, text: &[char]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        if let Kind::Simple(enc) = &self.kind
            && let Some(c) = enc.encode(text)
        {
            out.push(c);
        }
        if let Some(tu) = &self.tounicode {
            let bytes: Option<Vec<u8>> = text
                .iter()
                .map(|c| {
                    let v = tu.inverse(*c)?;
                    match &self.kind {
                        Kind::Simple(_) => u8::try_from(v).ok().map(|b| vec![b]),
                        Kind::Composite { space, .. } => code_bytes(space.as_deref()?, v),
                    }
                })
                .collect::<Option<Vec<Vec<u8>>>>()
                .map(|v| v.concat());
            out.extend(bytes);
        }
        out
    }

    /// The reading a removed string carries as needle text (R10-D1): a simple font's complete
    /// encoding reading whenever it differs AT ALL from the bytes read as PDFDocEncoding text (a
    /// `/Differences` or base-encoding quote, dash or space remap included), else its `/ToUnicode`
    /// reading; a composite font's `/ToUnicode` reading. `None`: the bytes are the text, or nothing
    /// reads them. The flag says whether the reading is complete.
    pub(crate) fn needle_text(&self, s: &[u8]) -> (Option<String>, bool) {
        match &self.kind {
            Kind::Composite { .. } => {
                if self.tounicode.is_none() {
                    return (None, s.is_empty());
                }
                let r = self.read(s);
                (Some(r.text.into_iter().collect()), r.complete)
            }
            Kind::Simple(enc) => {
                let read = enc.read(s, self.tounicode.as_ref());
                let doc: Vec<char> = s.iter().map(|b| pdfdoc_char(*b)).collect();
                if !read.opaque && !read.text.is_empty() && read.text != doc {
                    return (Some(read.text.into_iter().collect()), true);
                }
                (self.tounicode.as_ref().map(|c| c.read_bytes(s).into_iter().collect()), !read.opaque)
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use crate::reader::Entry;
    use crate::syntax::Lexer;
    use std::collections::BTreeMap;

    fn font(extra: &[(u32, &str)], dict: &str) -> Font {
        let mut m: Objs = BTreeMap::new();
        for (n, text) in extra {
            let obj = Lexer::new(text.as_bytes(), 0, 32).parse_object(0, true).unwrap();
            m.insert(*n, Entry { generation: 0, obj, span: None, data_at: None });
        }
        let fd = Lexer::new(dict.as_bytes(), 0, 32).parse_object(0, true).unwrap();
        let mut budget = usize::MAX;
        read_font(&m, fd.as_dict().unwrap(), &Limits::default(), &mut budget).unwrap()
    }

    fn widths(f: &Font, s: &[u8]) -> Vec<Width> {
        f.glyphs(s).unwrap().into_iter().map(|g| g.width.unwrap()).collect()
    }

    #[test]
    fn std14_union_box_is_derived_from_the_afm_files() {
        let u = STD14.iter().fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |b, f| union(b, f.bbox.map(f64::from)));
        assert_eq!(u, STD_UNION_BOX);
        assert_eq!(STD14.iter().map(|f| f64::from(f.max_width)).fold(0.0, f64::max), STD_MAX_WIDTH);
        assert!(STD14.iter().all(|f| f.glyphs.windows(2).all(|w| w[0].0.as_bytes() < w[1].0.as_bytes())), "sorted for the binary search");
    }

    #[test]
    fn simple_widths_follow_widths_afm_and_substitutes() {
        // Helvetica without /Widths: AFM widths through StandardEncoding (@ = 1015, A = 667).
        let h = font(&[], "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        assert_eq!(widths(&h, b"@A "), vec![(1015.0, 1015.0), (667.0, 667.0), (278.0, 278.0)]);
        assert_eq!(h.bbox, union([-166.0, -225.0, 1000.0, 931.0], STD_UNION_BOX));
        // WinAnsi base by character: 0x80 the Euro (556), 0x93 quotedblleft (333); 0x81 is unassigned:
        // a bound, not a guess.
        let w = font(&[], "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
        assert_eq!(widths(&w, b"\x80\x93\x81"), vec![(556.0, 556.0), (333.0, 333.0), (0.0, 1015.0)]);
        // /Differences names: by name.
        let d = font(&[], "<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman /Encoding << /Differences [65 /W /fi] >> >>");
        assert_eq!(widths(&d, b"AB"), vec![(944.0, 944.0), (556.0, 556.0)]);
        // Symbol: its own encoding (arrowboth 0xAB = 1042).
        assert_eq!(widths(&font(&[], "<< /Type /Font /Subtype /Type1 /BaseFont /Symbol >>"), b"\xAB"), vec![(1042.0, 1042.0)]);
        // /Widths win; outside the array /MissingWidth.
        let x = font(&[(5, "<< /MissingWidth 77 /FontBBox [0 0 0 0] >>")], "<< /Type /Font /Subtype /TrueType /BaseFont /Arial /FirstChar 65 /Widths [500 (x)] /FontDescriptor 5 0 R >>");
        assert_eq!(widths(&x, b"ABC"), vec![(500.0, 500.0), (77.0, 77.0), (77.0, 77.0)]);
        assert_eq!(x.bbox, STD_UNION_BOX, "a degenerate declared box falls back to the union");
        // Non-embedded, not standard, no /Widths: a substitute; embedded: unknown.
        assert_eq!(widths(&font(&[], "<< /Type /Font /Subtype /TrueType /BaseFont /Arial >>"), b"A"), vec![(0.0, STD_MAX_WIDTH)]);
        let e = font(&[(5, "<< /FontFile2 9 0 R >>")], "<< /Type /Font /Subtype /TrueType /BaseFont /ABCDEF+Arial /FontDescriptor 5 0 R >>");
        assert!(!e.metrics_known() && e.glyphs(b"A").unwrap()[0].width.is_none());
    }

    #[test]
    fn cid_widths_follow_w_and_dw() {
        let f = font(&[(21, "<< /Subtype /CIDFontType2 /DW 3000 /W [10 [100 200] 20 30 400] >>")], "<< /Subtype /Type0 /Encoding /Identity-H /DescendantFonts [21 0 R] >>");
        assert_eq!(widths(&f, &[0, 10, 0, 11, 0, 25, 0, 1]), vec![(100.0, 100.0), (200.0, 200.0), (400.0, 400.0), (3000.0, 3000.0)]);
        assert!(f.glyphs(&[0, 10, 0]).is_none(), "a partial code is no code");
        // Overlapping entries and a CMap whose CIDs are unknown read the declared bound.
        let o = font(&[(21, "<< /Subtype /CIDFontType2 /W [1 5 100 3 [900]] >>")], "<< /Subtype /Type0 /Encoding /Identity-H /DescendantFonts [21 0 R] >>");
        assert_eq!(widths(&o, &[0, 9]), vec![(100.0, 1000.0)]);
        let u = font(&[(21, "<< /Subtype /CIDFontType2 /W [1 [50]] >>")], "<< /Subtype /Type0 /Encoding /UniJIS-UCS2-H /DescendantFonts [21 0 R] >>");
        assert_eq!(widths(&u, &[0, 1]), vec![(50.0, 1000.0)]);
        // Indirect /Subtype (R10-F3): still composite.
        let i = font(&[(31, "/Type0"), (21, "<< /Subtype /CIDFontType2 >>")], "<< /Subtype 31 0 R /Encoding /Identity-V /DescendantFonts [21 0 R] >>");
        assert!(i.composite() && i.wmode == WMode::Vertical);
    }

    /// V4-02 for the /W reader: a range to the last CID allocates nothing (dims_u32_max), and every
    /// element is charged to the operation budget (aggregate_over_budget).
    #[test]
    fn w_arrays_are_bounded() {
        let f = font(&[(21, "<< /Subtype /CIDFontType2 /W [0 4294967295 500] >>")], "<< /Subtype /Type0 /Encoding /Identity-H /DescendantFonts [21 0 R] >>");
        assert_eq!(widths(&f, &[0xFF, 0xFF]), vec![(500.0, 500.0)]);
        let many: String = (0..20_000).map(|i| format!("{i} [{}] ", i % 900)).collect();
        let m: Objs = {
            let mut m = BTreeMap::new();
            let obj = Lexer::new(format!("<< /Subtype /CIDFontType2 /W [{many}] >>").as_bytes(), 0, 32).parse_object(0, true).unwrap();
            m.insert(21, Entry { generation: 0, obj, span: None, data_at: None });
            m
        };
        let fd = Lexer::new(b"<< /Subtype /Type0 /Encoding /Identity-H /DescendantFonts [21 0 R] >>", 0, 32).parse_object(0, true).unwrap();
        let mut budget = 100_000usize;
        assert!(matches!(read_font(&m, fd.as_dict().unwrap(), &Limits::default(), &mut budget), Err(PressError::InputLimit("decoded_total"))));
        let mut budget = usize::MAX;
        let f = read_font(&m, fd.as_dict().unwrap(), &Limits::default(), &mut budget).unwrap();
        assert_eq!(widths(&f, &[0x4E, 0x1F]), vec![(199.0, 199.0)], "CID 19999 = 0x4E1F: width 19999 mod 900");
    }
}
