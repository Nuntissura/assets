//! New-PDF export builder (lane SMB-LAYOUT-PDF-EXPORT slice 1). Takes neutral, already-resolved page
//! content (paths in points, images as samples, glyph ids with positions and the font bytes the
//! shaper used) and writes ONE fresh PDF through the crate's own serialisers: `canon::write_ops`
//! for content streams, `writer::write_document` for objects, xref and trailer, Flate through the
//! same `miniz_oxide` call the redaction save uses. Press stays at its edges (no Folio, layout or
//! type types here): a caller lowers its document to this API (STU-TYP-127: glyph runs are
//! positioned by the caller; this module has no shaper).
//!
//! Output: `%PDF-<version>`, one page object per `add_page` (MediaBox, optional TrimBox / BleedBox),
//! fills and strokes in DeviceGray / DeviceRGB / DeviceCMYK / ICCBased colour, Flate image XObjects
//! with an optional `/SMask` alpha, Type0 / CIDFontType2 fonts (`/Identity-H`, `/CIDToGIDMap
//! /Identity`, `/W` from the caller, the WHOLE font as `/FontFile2`, `/ToUnicode` from the caller so
//! text stays extractable), an optional output intent (`/DestOutputProfile`) and `/Info /Title`.
//! Deterministic: no dates, the trailer `/ID` is a hash of the objects (test
//! `export_is_deterministic`).
//!
//! Not in this slice: font subsetting, CFF / OpenType-CFF embedding (refused
//! `font_cff_not_embedded`), Type 3 / colour fonts, transparency groups and blend modes (beyond the
//! image SMask), spot colours, overprint, marks, tagged structure, links, encryption, presets.
//! `ExportReceipt::conformance` is `not_proven` (test `two_page_export_reads_back`): an output intent does not make the file
//! PDF/X or PDF/A, and this crate's own reading is not an independent conformance check (STU-PDF-002).
//!
//! Hostile input (V4-01/02): every size is checked arithmetic; image sample bytes are computed and
//! compared with `Limits::max_decoded_bytes` before anything is compressed; the running aggregate of
//! raw stream bytes is bounded by `Limits::max_decoded_total` and of written stream bytes by
//! `Limits::max_bytes`; pages by `max_pages`, object numbers by `max_object_number`, operations per
//! page by `max_content_ops`. Every refusal is a typed [`ExportError`] and leaves the builder as it
//! was; `finish` returns bytes only on success (tests `hostile_sizes_are_typed_refusals`,
//! `hostile_fonts_are_typed_refusals`).
#![deny(clippy::arithmetic_side_effects)]
mod sfnt;
#[cfg(test)]
mod tests;

use crate::canon::write_ops;
use crate::content::Op;
use crate::error::{Limits, PressError};
use crate::pdf_edit::{PdfDoc, sha256_hex, text_string};
use crate::reader::{Entry, check_cancel};
use crate::syntax::{Dict, ObjRef, Object, Stream};
use crate::writer::{write_document, write_object};
use hsk_studio_accord::CancellationToken;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// The value of `ExportReceipt::conformance` in this slice (test `two_page_export_reads_back`).
pub const EXPORT_CONFORMANCE: &str = "not_proven";
/// Largest magnitude of a coordinate, size, width or matrix entry (points). Larger values are
/// refused (`value_out_of_range`); magnitudes below 1e-9 are written as 0, so every number fits the
/// canonical content writer's 64-character numbers.
pub const MAX_EXPORT_COORD: f64 = 1.0e9;
/// Longest text string (title, output condition fields) in chars.
pub const MAX_EXPORT_TEXT: usize = 4096;
/// Longest `/ToUnicode` text of one glyph in chars (ligatures map to a few chars).
const MAX_GLYPH_TEXT: usize = 16;
/// `bfchar` entries per block (Adobe TN 5014: at most 100).
const BFCHAR_BLOCK: usize = 100;
const VERSIONS: [&str; 5] = ["1.4", "1.5", "1.6", "1.7", "2.0"];

/// Typed refusal of the export builder. Stable snake_case [`ExportError::code`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportError {
    Canceled,
    /// `ExportOptions::version` is not one of 1.4, 1.5, 1.6, 1.7, 2.0.
    InvalidVersion,
    /// A coordinate, size, width or matrix entry is NaN or infinite.
    NonFinite,
    /// A coordinate, size, width or matrix entry is larger than [`MAX_EXPORT_COORD`] in magnitude.
    ValueOutOfRange,
    /// A box has no positive width and height.
    InvalidBox,
    /// A trim or bleed box is not inside the media box.
    BoxOutsideMedia,
    /// A colour component is outside 0..=1, or the component count does not match the space.
    ColourOutOfRange,
    /// A path is empty or does not start with `MoveTo`.
    InvalidPath,
    /// Negative stroke width or non-positive font size.
    InvalidSize,
    NoSuchPage,
    NoSuchFont,
    NoSuchProfile,
    /// Image width or height is zero.
    ImageDims,
    /// width x height x components x bytes per sample overflows (refused before any allocation).
    ImageSizeOverflow,
    /// `samples` / `alpha` length differs from the computed size.
    ImageSamplesLength,
    /// Bits per component other than 8 or 16.
    UnsupportedBits,
    /// One stream is larger than `Limits::max_decoded_bytes`.
    StreamOverBudget,
    /// The running total of stream bytes passed `max_decoded_total` (raw) or `max_bytes` (written).
    AggregateOverBudget,
    TooManyPages,
    TooManyObjects,
    /// More operations on one page than `Limits::max_content_ops`.
    TooManyOperations,
    /// `finish` without any page.
    NoPages,
    /// Not a readable single TrueType-outline font.
    FontUnreadable,
    /// CFF or CFF2 outlines (`OTTO`): slice 2 embeds them as `/FontFile3 /OpenType`.
    FontCffNotEmbedded,
    /// A font collection (`ttcf`) or a face index other than 0.
    FontCollectionNotEmbedded,
    /// `OS/2 fsType` forbids embedding (Restricted License or Bitmap embedding only).
    FontEmbeddingRestricted,
    /// A glyph id at or above the font's glyph count.
    FontGlyphOutOfRange,
    /// A `/ToUnicode` text that is empty or longer than 16 chars.
    ToUnicodeInvalid,
    /// An ICC profile without a valid header (size, `acsp` signature, data colour space).
    IccUnreadable,
    /// The ICC data colour space does not have the stated number of components (1, 3 or 4).
    IccComponentsMismatch,
    /// A text string longer than [`MAX_EXPORT_TEXT`] chars.
    TextTooLong,
    /// The crate's writer or reader refused (serialisation or the read-back check).
    Press(PressError),
}

impl ExportError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Canceled => "canceled",
            Self::InvalidVersion => "invalid_version",
            Self::NonFinite => "non_finite",
            Self::ValueOutOfRange => "value_out_of_range",
            Self::InvalidBox => "invalid_box",
            Self::BoxOutsideMedia => "box_outside_media",
            Self::ColourOutOfRange => "colour_out_of_range",
            Self::InvalidPath => "invalid_path",
            Self::InvalidSize => "invalid_size",
            Self::NoSuchPage => "no_such_page",
            Self::NoSuchFont => "no_such_font",
            Self::NoSuchProfile => "no_such_profile",
            Self::ImageDims => "image_dims",
            Self::ImageSizeOverflow => "image_size_overflow",
            Self::ImageSamplesLength => "image_samples_length",
            Self::UnsupportedBits => "unsupported_bits",
            Self::StreamOverBudget => "stream_over_budget",
            Self::AggregateOverBudget => "aggregate_over_budget",
            Self::TooManyPages => "too_many_pages",
            Self::TooManyObjects => "too_many_objects",
            Self::TooManyOperations => "too_many_operations",
            Self::NoPages => "no_pages",
            Self::FontUnreadable => "font_unreadable",
            Self::FontCffNotEmbedded => "font_cff_not_embedded",
            Self::FontCollectionNotEmbedded => "font_collection_not_embedded",
            Self::FontEmbeddingRestricted => "font_embedding_restricted",
            Self::FontGlyphOutOfRange => "font_glyph_out_of_range",
            Self::ToUnicodeInvalid => "to_unicode_invalid",
            Self::IccUnreadable => "icc_unreadable",
            Self::IccComponentsMismatch => "icc_components_mismatch",
            Self::TextTooLong => "text_too_long",
            Self::Press(_) => "press_refused",
        }
    }
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Press(e) => write!(f, "{}: {e}", self.code()),
            _ => f.write_str(self.code()),
        }
    }
}
impl std::error::Error for ExportError {}

impl From<PressError> for ExportError {
    fn from(e: PressError) -> Self {
        match e {
            PressError::Canceled => Self::Canceled,
            other => Self::Press(other),
        }
    }
}

type Result<T> = std::result::Result<T, ExportError>;

/// `/S` of the output intent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputIntentSubtype {
    /// `/GTS_PDFX`.
    GtsPdfx,
    /// `/GTS_PDFA1`.
    GtsPdfa1,
}

/// An output intent with its destination profile (STU-LAY-158 Output panel values).
#[derive(Clone, Copy, Debug)]
pub struct OutputIntent<'a> {
    pub icc: &'a [u8],
    /// 1 (gray), 3 (RGB) or 4 (CMYK); must match the profile's data colour space.
    pub components: u8,
    pub output_condition_identifier: &'a str,
    pub output_condition: Option<&'a str>,
    pub registry: Option<&'a str>,
    pub subtype: OutputIntentSubtype,
}

#[derive(Clone, Copy, Debug)]
pub struct ExportOptions<'a> {
    /// Header version: "1.4", "1.5", "1.6", "1.7" (default) or "2.0".
    pub version: &'a str,
    pub output_intent: Option<OutputIntent<'a>>,
    pub info_title: Option<&'a str>,
}
impl Default for ExportOptions<'_> {
    fn default() -> Self {
        Self { version: "1.7", output_intent: None, info_title: None }
    }
}

/// Page boxes in points, `[x0, y0, x1, y1]` with x0 < x1 and y0 < y1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageSpec {
    pub media_box_pt: [f64; 4],
    pub trim_box_pt: Option<[f64; 4]>,
    pub bleed_box_pt: Option<[f64; 4]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PageHandle(usize);
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FontHandle(usize);
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IccHandle(usize);

/// Path construction in points (PDF user space, y up).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathCommand {
    MoveTo(f64, f64),
    LineTo(f64, f64),
    CubicTo(f64, f64, f64, f64, f64, f64),
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

/// A colour value; components 0..=1. `IccBased` uses the first N of the four components, N = the
/// profile's component count.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Colour {
    DeviceGray(f64),
    DeviceRgb([f64; 3]),
    DeviceCmyk([f64; 4]),
    IccBased(IccHandle, [f64; 4]),
}

/// The colour space of image samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageColour {
    DeviceGray,
    DeviceRgb,
    DeviceCmyk,
    IccBased(IccHandle),
}

/// Image samples, rows top to bottom, components interleaved, big-endian 16-bit samples.
#[derive(Clone, Copy, Debug)]
pub struct ImageSpec<'a> {
    pub width: u32,
    pub height: u32,
    /// 8 or 16.
    pub bits: u8,
    pub colour: ImageColour,
    pub samples: &'a [u8],
    /// One alpha sample per pixel at the same bit depth, written as the image's `/SMask`.
    pub alpha: Option<&'a [u8]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineCap {
    Butt,
    Round,
    Square,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineJoin {
    Miter,
    Round,
    Bevel,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StrokeStyle {
    pub width_pt: f64,
    pub cap: LineCap,
    pub join: LineJoin,
}

/// What `finish` wrote, read back from the output by this crate's reader (STU-LAY-158 effective_*
/// read-back values: what the export resolved, not what was asked).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportReceipt {
    pub version: String,
    pub pages: u32,
    pub fonts_embedded: u32,
    pub images: u32,
    pub bytes: usize,
    pub sha256_hex: String,
    /// sha256 of the decoded `/DestOutputProfile` read back from the output.
    pub effective_destination_profile_sha256: Option<String>,
    /// `/OutputConditionIdentifier` read back from the output.
    pub effective_output_condition: Option<String>,
    /// `/RegistryName` read back from the output.
    pub effective_oc_registry: Option<String>,
    /// `None` in this slice: no PDF/X profile is claimed or proven (STU-PDF-002; test
    /// `two_page_export_reads_back`).
    pub effective_pdfx_profile: Option<String>,
    /// [`EXPORT_CONFORMANCE`] (`not_proven`; test `two_page_export_reads_back`).
    pub conformance: &'static str,
}

struct PageBuild {
    spec: PageSpec,
    page_obj: u32,
    content_obj: u32,
    ops: Vec<Op>,
    fonts: BTreeSet<usize>,
    profiles: BTreeSet<usize>,
    /// (resource index, XObject number).
    images: Vec<(usize, u32)>,
}

struct FontBuild {
    type0: u32,
    num_glyphs: u16,
}

struct IccBuild {
    space: u32,
    components: u8,
}

/// The builder. Every method either applies completely or refuses with nothing changed.
pub struct PdfExport {
    version: String,
    limits: Limits,
    objects: BTreeMap<u32, Entry>,
    next: u32,
    catalog: u32,
    pages_root: u32,
    info: Option<u32>,
    output_intent: Option<Dict>,
    pages: Vec<PageBuild>,
    fonts: Vec<FontBuild>,
    font_files: BTreeMap<[u8; 32], u32>,
    profiles: Vec<IccBuild>,
    images: usize,
    raw_total: usize,
    written_total: usize,
}

fn name(n: &str) -> Object {
    Object::Name(n.as_bytes().to_vec())
}
fn real(v: f64) -> Object {
    Object::Real(if v.abs() < 1.0e-9 { 0.0 } else { v })
}
fn rf(num: u32) -> Object {
    Object::Ref(ObjRef { num, generation: 0 })
}
fn int<T: Into<i64>>(v: T) -> Object {
    Object::Int(v.into())
}
fn op(n: &str, operands: Vec<Object>) -> Op {
    Op { name: n.as_bytes().to_vec(), operands, start: 0, end: 0, inline_data: None }
}
fn finite(vals: &[f64]) -> Result<()> {
    if !vals.iter().all(|v| v.is_finite()) {
        return Err(ExportError::NonFinite);
    }
    if vals.iter().all(|v| v.abs() <= MAX_EXPORT_COORD) { Ok(()) } else { Err(ExportError::ValueOutOfRange) }
}
fn check_box(b: &[f64; 4]) -> Result<()> {
    finite(b)?;
    if b[0] < b[2] && b[1] < b[3] { Ok(()) } else { Err(ExportError::InvalidBox) }
}
fn inside(inner: &[f64; 4], outer: &[f64; 4]) -> bool {
    inner[0] >= outer[0] && inner[1] >= outer[1] && inner[2] <= outer[2] && inner[3] <= outer[3]
}
fn rect(b: &[f64; 4]) -> Object {
    Object::Array(b.iter().map(|v| real(*v)).collect())
}
fn deflate(data: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(data, 6)
}
fn flate_stream(mut dict: Dict, raw: &[u8]) -> (Object, usize) {
    dict.set("Filter", name("FlateDecode"));
    let data = deflate(raw);
    let len = data.len();
    (Object::Stream(Box::new(Stream { dict, data })), len)
}

/// A text string: printable ASCII as is, anything else UTF-16BE with a byte-order mark (ISO 32000
/// 7.9.2.2). The writer escapes `(`, `)` and `\` and writes other bytes in octal.
fn text_bytes(s: &str) -> Result<Object> {
    if s.chars().count() > MAX_EXPORT_TEXT {
        return Err(ExportError::TextTooLong);
    }
    if s.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
        return Ok(Object::Str(s.as_bytes().to_vec()));
    }
    let mut out = vec![0xFE, 0xFF];
    for u in s.encode_utf16() {
        out.extend_from_slice(&u.to_be_bytes());
    }
    Ok(Object::Str(out))
}

/// ICC header check (ICC.1:2022 7.2): declared size within the data and at least the 128-byte header
/// plus the tag count, `acsp` at byte 36, data colour space at byte 16 with `components` channels.
fn check_icc(icc: &[u8], components: u8, limits: &Limits) -> Result<()> {
    if icc.len() > limits.max_decoded_bytes {
        return Err(ExportError::StreamOverBudget);
    }
    let declared = icc.get(0..4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]));
    let declared = declared.and_then(|d| usize::try_from(d).ok()).ok_or(ExportError::IccUnreadable)?;
    if icc.len() < 132 || declared < 132 || declared > icc.len() || icc.get(36..40) != Some(b"acsp") {
        return Err(ExportError::IccUnreadable);
    }
    let n = match icc.get(16..20) {
        Some(b"GRAY") => 1,
        Some(b"RGB ") => 3,
        Some(b"CMYK") => 4,
        _ => return Err(ExportError::IccUnreadable),
    };
    if n == components { Ok(()) } else { Err(ExportError::IccComponentsMismatch) }
}

impl PdfExport {
    pub fn new(options: ExportOptions<'_>, limits: &Limits) -> Result<Self> {
        if !VERSIONS.contains(&options.version) {
            return Err(ExportError::InvalidVersion);
        }
        let mut ex = Self {
            version: options.version.to_owned(),
            limits: *limits,
            objects: BTreeMap::new(),
            next: 1,
            catalog: 0,
            pages_root: 0,
            info: None,
            output_intent: None,
            pages: Vec::new(),
            fonts: Vec::new(),
            font_files: BTreeMap::new(),
            profiles: Vec::new(),
            images: 0,
            raw_total: 0,
            written_total: 0,
        };
        ex.reserve(4)?;
        ex.catalog = ex.alloc();
        ex.pages_root = ex.alloc();
        if let Some(title) = options.info_title {
            let mut info = Dict::default();
            info.set("Title", text_bytes(title)?);
            info.set("Producer", Object::Str(b"hsk-studio-press".to_vec()));
            let num = ex.alloc();
            ex.insert(num, Object::Dict(info));
            ex.info = Some(num);
        }
        if let Some(oi) = options.output_intent {
            check_icc(oi.icc, oi.components, limits)?;
            let mut d = Dict::default();
            d.set("Type", name("OutputIntent"));
            d.set(
                "S",
                name(match oi.subtype {
                    OutputIntentSubtype::GtsPdfx => "GTS_PDFX",
                    OutputIntentSubtype::GtsPdfa1 => "GTS_PDFA1",
                }),
            );
            d.set("OutputConditionIdentifier", text_bytes(oi.output_condition_identifier)?);
            if let Some(c) = oi.output_condition {
                d.set("OutputCondition", text_bytes(c)?);
            }
            if let Some(r) = oi.registry {
                d.set("RegistryName", text_bytes(r)?);
            }
            let mut sd = Dict::default();
            sd.set("N", int(oi.components));
            let (stream, len) = flate_stream(sd, oi.icc);
            ex.charge(oi.icc.len(), len)?;
            let num = ex.alloc();
            ex.insert(num, stream);
            d.set("DestOutputProfile", rf(num));
            ex.output_intent = Some(d);
        }
        Ok(ex)
    }

    /// Fails unless `n` more object numbers fit under `Limits::max_object_number`.
    fn reserve(&self, n: u32) -> Result<()> {
        let last = self.next.checked_add(n).and_then(|v| v.checked_sub(1)).ok_or(ExportError::TooManyObjects)?;
        if last > self.limits.max_object_number { Err(ExportError::TooManyObjects) } else { Ok(()) }
    }
    /// Call only after [`Self::reserve`].
    fn alloc(&mut self) -> u32 {
        let n = self.next;
        self.next = self.next.saturating_add(1);
        n
    }
    fn insert(&mut self, num: u32, obj: Object) {
        self.objects.insert(num, Entry { generation: 0, obj, span: None, data_at: None });
    }
    /// Checks the aggregates for `raw` stream bytes written as `written` bytes, then charges them.
    fn charge(&mut self, raw: usize, written: usize) -> Result<()> {
        let r = self.raw_total.checked_add(raw).filter(|t| *t <= self.limits.max_decoded_total);
        let w = self.written_total.checked_add(written).filter(|t| *t <= self.limits.max_bytes);
        match (r, w) {
            (Some(r), Some(w)) => {
                self.raw_total = r;
                self.written_total = w;
                Ok(())
            }
            _ => Err(ExportError::AggregateOverBudget),
        }
    }
    /// Raw bytes only (before compressing): refuses before the compressor allocates.
    fn precheck_raw(&self, raw: usize) -> Result<()> {
        if raw > self.limits.max_decoded_bytes {
            return Err(ExportError::StreamOverBudget);
        }
        match self.raw_total.checked_add(raw) {
            Some(t) if t <= self.limits.max_decoded_total => Ok(()),
            _ => Err(ExportError::AggregateOverBudget),
        }
    }

    fn page(&self, page: PageHandle) -> Result<&PageBuild> {
        self.pages.get(page.0).ok_or(ExportError::NoSuchPage)
    }

    fn push_ops(&mut self, page: PageHandle, ops: Vec<Op>) -> Result<()> {
        let p = self.pages.get_mut(page.0).ok_or(ExportError::NoSuchPage)?;
        match p.ops.len().checked_add(ops.len()) {
            Some(n) if n <= self.limits.max_content_ops => {
                p.ops.extend(ops);
                Ok(())
            }
            _ => Err(ExportError::TooManyOperations),
        }
    }

    /// Adds a page; boxes must be finite with positive size, trim and bleed inside the media box.
    pub fn add_page(&mut self, spec: PageSpec) -> Result<PageHandle> {
        check_box(&spec.media_box_pt)?;
        for b in [spec.trim_box_pt, spec.bleed_box_pt].into_iter().flatten() {
            check_box(&b)?;
            if !inside(&b, &spec.media_box_pt) {
                return Err(ExportError::BoxOutsideMedia);
            }
        }
        if self.pages.len() >= self.limits.max_pages {
            return Err(ExportError::TooManyPages);
        }
        self.reserve(2)?;
        let (page_obj, content_obj) = (self.alloc(), self.alloc());
        self.pages.push(PageBuild {
            spec,
            page_obj,
            content_obj,
            ops: Vec::new(),
            fonts: BTreeSet::new(),
            profiles: BTreeSet::new(),
            images: Vec::new(),
        });
        Ok(PageHandle(self.pages.len().saturating_sub(1)))
    }

    /// Registers an ICC profile for `Colour::IccBased` / `ImageColour::IccBased`.
    pub fn embed_icc(&mut self, icc: &[u8], components: u8) -> Result<IccHandle> {
        check_icc(icc, components, &self.limits)?;
        self.precheck_raw(icc.len())?;
        self.reserve(2)?;
        let mut sd = Dict::default();
        sd.set("N", int(components));
        let (stream, len) = flate_stream(sd, icc);
        self.charge(icc.len(), len)?;
        let (stream_num, space) = (self.alloc(), self.alloc());
        self.insert(stream_num, stream);
        self.insert(space, Object::Array(vec![name("ICCBased"), rf(stream_num)]));
        self.profiles.push(IccBuild { space, components });
        Ok(IccHandle(self.profiles.len().saturating_sub(1)))
    }

    /// Colour-setting operations for fill (`stroke = false`) or stroke.
    fn colour_ops(&self, c: &Colour, stroke: bool) -> Result<(Vec<Op>, Option<usize>)> {
        let comps = |v: &[f64]| -> Result<Vec<Object>> {
            finite(v)?;
            if v.iter().all(|x| (0.0..=1.0).contains(x)) {
                Ok(v.iter().map(|x| real(*x)).collect())
            } else {
                Err(ExportError::ColourOutOfRange)
            }
        };
        let pick = |fill: &str, strk: &str| if stroke { strk.to_owned() } else { fill.to_owned() };
        Ok(match c {
            Colour::DeviceGray(g) => (vec![op(&pick("g", "G"), comps(&[*g])?)], None),
            Colour::DeviceRgb(v) => (vec![op(&pick("rg", "RG"), comps(v)?)], None),
            Colour::DeviceCmyk(v) => (vec![op(&pick("k", "K"), comps(v)?)], None),
            Colour::IccBased(h, v) => {
                let p = self.profiles.get(h.0).ok_or(ExportError::NoSuchProfile)?;
                let vals = v.get(..usize::from(p.components)).ok_or(ExportError::ColourOutOfRange)?;
                let cs = Object::Name(format!("CS{}", h.0).into_bytes());
                (vec![op(&pick("cs", "CS"), vec![cs]), op(&pick("sc", "SC"), comps(vals)?)], Some(h.0))
            }
        })
    }

    fn path_ops(path: &[PathCommand]) -> Result<Vec<Op>> {
        if !matches!(path.first(), Some(PathCommand::MoveTo(..))) {
            return Err(ExportError::InvalidPath);
        }
        path.iter()
            .map(|c| {
                Ok(match *c {
                    PathCommand::MoveTo(x, y) => {
                        finite(&[x, y])?;
                        op("m", vec![real(x), real(y)])
                    }
                    PathCommand::LineTo(x, y) => {
                        finite(&[x, y])?;
                        op("l", vec![real(x), real(y)])
                    }
                    PathCommand::CubicTo(a, b, c, d, e, f) => {
                        finite(&[a, b, c, d, e, f])?;
                        op("c", vec![real(a), real(b), real(c), real(d), real(e), real(f)])
                    }
                    PathCommand::Close => op("h", vec![]),
                })
            })
            .collect()
    }

    fn draw(&mut self, page: PageHandle, body: Vec<Op>, colour: Option<usize>) -> Result<()> {
        self.page(page)?;
        let mut ops = Vec::with_capacity(body.len().saturating_add(2));
        ops.push(op("q", vec![]));
        ops.extend(body);
        ops.push(op("Q", vec![]));
        self.push_ops(page, ops)?;
        if let (Some(i), Some(p)) = (colour, self.pages.get_mut(page.0)) {
            p.profiles.insert(i);
        }
        Ok(())
    }

    /// Fills `path` (points) with `colour`.
    pub fn fill_path(&mut self, page: PageHandle, path: &[PathCommand], rule: FillRule, colour: Colour) -> Result<()> {
        let (mut body, icc) = self.colour_ops(&colour, false)?;
        body.extend(Self::path_ops(path)?);
        body.push(op(if rule == FillRule::EvenOdd { "f*" } else { "f" }, vec![]));
        self.draw(page, body, icc)
    }

    /// Strokes `path` (points) with `colour` and `style`.
    pub fn stroke_path(&mut self, page: PageHandle, path: &[PathCommand], colour: Colour, style: StrokeStyle) -> Result<()> {
        finite(&[style.width_pt])?;
        if style.width_pt < 0.0 {
            return Err(ExportError::InvalidSize);
        }
        let (mut body, icc) = self.colour_ops(&colour, true)?;
        let cap = match style.cap {
            LineCap::Butt => 0,
            LineCap::Round => 1,
            LineCap::Square => 2,
        };
        let join = match style.join {
            LineJoin::Miter => 0,
            LineJoin::Round => 1,
            LineJoin::Bevel => 2,
        };
        body.push(op("w", vec![real(style.width_pt)]));
        body.push(op("J", vec![int(cap)]));
        body.push(op("j", vec![int(join)]));
        body.extend(Self::path_ops(path)?);
        body.push(op("S", vec![]));
        self.draw(page, body, icc)
    }

    /// Draws `image` through the `cm` matrix `placement` (`[a b c d e f]`: the unit square to points).
    pub fn draw_image(&mut self, page: PageHandle, image: ImageSpec<'_>, placement: [f64; 6]) -> Result<()> {
        self.page(page)?;
        finite(&placement)?;
        let bytes_per = match image.bits {
            8 => 1usize,
            16 => 2,
            _ => return Err(ExportError::UnsupportedBits),
        };
        if image.width == 0 || image.height == 0 {
            return Err(ExportError::ImageDims);
        }
        let (space, comps): (Object, usize) = match image.colour {
            ImageColour::DeviceGray => (name("DeviceGray"), 1),
            ImageColour::DeviceRgb => (name("DeviceRGB"), 3),
            ImageColour::DeviceCmyk => (name("DeviceCMYK"), 4),
            ImageColour::IccBased(h) => {
                let p = self.profiles.get(h.0).ok_or(ExportError::NoSuchProfile)?;
                (rf(p.space), usize::from(p.components))
            }
        };
        let w = usize::try_from(image.width).map_err(|_| ExportError::ImageSizeOverflow)?;
        let h = usize::try_from(image.height).map_err(|_| ExportError::ImageSizeOverflow)?;
        let pixels = w.checked_mul(h).ok_or(ExportError::ImageSizeOverflow)?;
        let alpha_len = pixels.checked_mul(bytes_per).ok_or(ExportError::ImageSizeOverflow)?;
        let expected = alpha_len.checked_mul(comps).ok_or(ExportError::ImageSizeOverflow)?;
        let raw = match image.alpha {
            Some(_) => expected.checked_add(alpha_len).ok_or(ExportError::ImageSizeOverflow)?,
            None => expected,
        };
        if expected > self.limits.max_decoded_bytes || alpha_len > self.limits.max_decoded_bytes {
            return Err(ExportError::StreamOverBudget);
        }
        if image.samples.len() != expected || image.alpha.is_some_and(|a| a.len() != alpha_len) {
            return Err(ExportError::ImageSamplesLength);
        }
        self.precheck_raw(raw)?;
        self.reserve(if image.alpha.is_some() { 2 } else { 1 })?;
        let base = |cs: Object| {
            let mut d = Dict::default();
            d.set("Type", name("XObject"));
            d.set("Subtype", name("Image"));
            d.set("Width", int(image.width));
            d.set("Height", int(image.height));
            d.set("ColorSpace", cs);
            d.set("BitsPerComponent", int(image.bits));
            d
        };
        let mask = image.alpha.map(|a| flate_stream(base(name("DeviceGray")), a));
        let (stream, len) = flate_stream(base(space), image.samples);
        let written = len.checked_add(mask.as_ref().map_or(0, |m| m.1)).ok_or(ExportError::AggregateOverBudget)?;
        let mut ops = vec![op("cm", placement.iter().map(|v| real(*v)).collect())];
        let res = self.page(page)?.images.len();
        ops.push(op("Do", vec![Object::Name(format!("Im{res}").into_bytes())]));
        // Count the q/cm/Do/Q operations before charging anything.
        match self.page(page)?.ops.len().checked_add(4) {
            Some(n) if n <= self.limits.max_content_ops => {}
            _ => return Err(ExportError::TooManyOperations),
        }
        self.charge(raw, written)?;
        let mut stream = stream;
        if let Some((m, _)) = mask {
            let mnum = self.alloc();
            self.insert(mnum, m);
            if let Object::Stream(s) = &mut stream {
                s.dict.set("SMask", rf(mnum));
            }
        }
        let num = self.alloc();
        self.insert(num, stream);
        self.draw(page, ops, None)?;
        if let Some(p) = self.pages.get_mut(page.0) {
            p.images.push((res, num));
        }
        self.images = self.images.saturating_add(1);
        Ok(())
    }

    /// Embeds a TrueType-outline font as Type0 / CIDFontType2 (`/Identity-H`, `/CIDToGIDMap
    /// /Identity`, the whole font as `/FontFile2`; a second font with the same bytes shares the
    /// font file). `widths` are `(glyph id, advance in 1/1000 em)` for `/W`; `to_unicode` maps glyph
    /// ids to their text for `/ToUnicode` (a later entry for the same glyph wins).
    pub fn embed_truetype(&mut self, bytes: &[u8], face_index: u32, widths: &[(u16, u16)], to_unicode: &[(u16, &str)]) -> Result<FontHandle> {
        let facts = sfnt::read(bytes, face_index)?;
        let n = facts.num_glyphs;
        if widths.iter().any(|(g, _)| *g >= n) || to_unicode.iter().any(|(g, _)| *g >= n) {
            return Err(ExportError::FontGlyphOutOfRange);
        }
        if to_unicode.iter().any(|(_, t)| t.is_empty() || t.chars().count() > MAX_GLYPH_TEXT) {
            return Err(ExportError::ToUnicodeInvalid);
        }
        let sha: [u8; 32] = Sha256::digest(bytes).into();
        let shared = self.font_files.get(&sha).copied();
        if shared.is_none() {
            self.precheck_raw(bytes.len())?;
        }
        self.reserve(if shared.is_some() { 4 } else { 5 })?;
        let font_file = match shared {
            Some(num) => (num, None),
            None => {
                let mut d = Dict::default();
                d.set("Length1", int(i64::try_from(bytes.len()).map_err(|_| ExportError::StreamOverBudget)?));
                let (s, len) = flate_stream(d, bytes);
                (0, Some((s, len)))
            }
        };
        let cmap = to_unicode_cmap(to_unicode);
        let (tu_stream, tu_len) = flate_stream(Dict::default(), cmap.as_bytes());
        let written = tu_len.checked_add(font_file.1.as_ref().map_or(0, |f| f.1)).ok_or(ExportError::AggregateOverBudget)?;
        let raw = cmap.len().checked_add(if shared.is_some() { 0 } else { bytes.len() }).ok_or(ExportError::AggregateOverBudget)?;
        self.charge(raw, written)?;
        let file_num = match font_file {
            (num, None) => num,
            (_, Some((s, _))) => {
                let num = self.alloc();
                self.insert(num, s);
                self.font_files.insert(sha, num);
                num
            }
        };
        let base = facts.ps_name.clone().unwrap_or_else(|| format!("HskFont-{}", &sha256_hex(bytes)[..12]));
        let scale = |v: i16| f64::from(v) * 1000.0 / f64::from(facts.units_per_em);
        let mut fd = Dict::default();
        fd.set("Type", name("FontDescriptor"));
        fd.set("FontName", name(&base));
        // Bit 3 (Symbolic): the glyphs are addressed by id, not by a standard Latin set.
        fd.set("Flags", int(4));
        fd.set("FontBBox", Object::Array(facts.bbox.iter().map(|v| real(scale(*v))).collect()));
        fd.set("ItalicAngle", int(0));
        fd.set("Ascent", real(scale(facts.ascent)));
        fd.set("Descent", real(scale(facts.descent)));
        fd.set("CapHeight", real(scale(facts.cap_height.unwrap_or(facts.ascent))));
        // Required key without a TrueType source; 80 is a neutral value (viewers read the outlines).
        fd.set("StemV", int(80));
        fd.set("FontFile2", rf(file_num));
        let mut sys = Dict::default();
        sys.set("Registry", Object::Str(b"Adobe".to_vec()));
        sys.set("Ordering", Object::Str(b"Identity".to_vec()));
        sys.set("Supplement", int(0));
        let mut cid = Dict::default();
        cid.set("Type", name("Font"));
        cid.set("Subtype", name("CIDFontType2"));
        cid.set("BaseFont", name(&base));
        cid.set("CIDSystemInfo", Object::Dict(sys));
        let (fd_num, cid_num, tu_num, type0) = (self.alloc(), self.alloc(), self.alloc(), self.alloc());
        cid.set("FontDescriptor", rf(fd_num));
        cid.set("W", w_array(widths));
        cid.set("CIDToGIDMap", name("Identity"));
        let mut t0 = Dict::default();
        t0.set("Type", name("Font"));
        t0.set("Subtype", name("Type0"));
        t0.set("BaseFont", name(&base));
        t0.set("Encoding", name("Identity-H"));
        t0.set("DescendantFonts", Object::Array(vec![rf(cid_num)]));
        t0.set("ToUnicode", rf(tu_num));
        self.insert(fd_num, Object::Dict(fd));
        self.insert(cid_num, Object::Dict(cid));
        self.insert(tu_num, tu_stream);
        self.insert(type0, Object::Dict(t0));
        self.fonts.push(FontBuild { type0, num_glyphs: n });
        Ok(FontHandle(self.fonts.len().saturating_sub(1)))
    }

    /// Shows glyphs of `font` at `size_pt`: each `(glyph id, x, y)` is placed at `origin + (x, y)`
    /// (points) with its own text matrix, so the caller's positions are written as given (not
    /// re-shaped, STU-TYP-127). Codes are 2-byte CIDs equal to the glyph ids.
    pub fn show_glyphs(&mut self, page: PageHandle, font: FontHandle, size_pt: f64, origin_pt: [f64; 2], fill: Colour, glyphs: &[(u16, f64, f64)]) -> Result<()> {
        self.page(page)?;
        let f = self.fonts.get(font.0).ok_or(ExportError::NoSuchFont)?;
        finite(&[size_pt, origin_pt[0], origin_pt[1]])?;
        if size_pt <= 0.0 {
            return Err(ExportError::InvalidSize);
        }
        if glyphs.iter().any(|(g, ..)| *g >= f.num_glyphs) {
            return Err(ExportError::FontGlyphOutOfRange);
        }
        if glyphs.is_empty() {
            return Ok(());
        }
        let (mut body, icc) = self.colour_ops(&fill, false)?;
        body.push(op("BT", vec![]));
        body.push(op("Tf", vec![Object::Name(format!("F{}", font.0).into_bytes()), real(size_pt)]));
        for (g, x, y) in glyphs {
            let (gx, gy) = (origin_pt[0] + x, origin_pt[1] + y);
            finite(&[gx, gy])?;
            body.push(op("Tm", vec![int(1), int(0), int(0), int(1), real(gx), real(gy)]));
            body.push(op("Tj", vec![Object::Str(g.to_be_bytes().to_vec())]));
        }
        body.push(op("ET", vec![]));
        self.draw(page, body, icc)?;
        if let Some(p) = self.pages.get_mut(page.0) {
            p.fonts.insert(font.0);
        }
        Ok(())
    }

    /// Writes the file and reads it back with this crate's reader (`PdfDoc::open`) for the receipt.
    /// Nothing is returned on a refusal.
    pub fn finish(mut self, cancel: &CancellationToken) -> Result<(Vec<u8>, ExportReceipt)> {
        check_cancel(cancel)?;
        if self.pages.is_empty() {
            return Err(ExportError::NoPages);
        }
        let pages = std::mem::take(&mut self.pages);
        let mut kids = Vec::with_capacity(pages.len());
        for p in &pages {
            check_cancel(cancel)?;
            let content = write_ops(&p.ops, &[])?;
            let (stream, len) = flate_stream(Dict::default(), &content);
            self.charge(content.len(), len)?;
            self.insert(p.content_obj, stream);
            let mut res = Dict::default();
            if !p.fonts.is_empty() {
                let mut d = Dict::default();
                for f in &p.fonts {
                    let num = self.fonts.get(*f).ok_or(ExportError::NoSuchFont)?.type0;
                    d.set_key(format!("F{f}").as_bytes(), rf(num));
                }
                res.set("Font", Object::Dict(d));
            }
            if !p.images.is_empty() {
                let mut d = Dict::default();
                for (i, num) in &p.images {
                    d.set_key(format!("Im{i}").as_bytes(), rf(*num));
                }
                res.set("XObject", Object::Dict(d));
            }
            if !p.profiles.is_empty() {
                let mut d = Dict::default();
                for h in &p.profiles {
                    let space = self.profiles.get(*h).ok_or(ExportError::NoSuchProfile)?.space;
                    d.set_key(format!("CS{h}").as_bytes(), rf(space));
                }
                res.set("ColorSpace", Object::Dict(d));
            }
            let mut pd = Dict::default();
            pd.set("Type", name("Page"));
            pd.set("Parent", rf(self.pages_root));
            pd.set("MediaBox", rect(&p.spec.media_box_pt));
            if let Some(b) = &p.spec.trim_box_pt {
                pd.set("TrimBox", rect(b));
            }
            if let Some(b) = &p.spec.bleed_box_pt {
                pd.set("BleedBox", rect(b));
            }
            pd.set("Resources", Object::Dict(res));
            pd.set("Contents", rf(p.content_obj));
            self.insert(p.page_obj, Object::Dict(pd));
            kids.push(rf(p.page_obj));
        }
        let count = i64::try_from(kids.len()).map_err(|_| ExportError::TooManyPages)?;
        let mut root = Dict::default();
        root.set("Type", name("Pages"));
        root.set("Kids", Object::Array(kids));
        root.set("Count", Object::Int(count));
        self.insert(self.pages_root, Object::Dict(root));
        let mut cat = Dict::default();
        cat.set("Type", name("Catalog"));
        cat.set("Pages", rf(self.pages_root));
        if let Some(oi) = self.output_intent.take() {
            cat.set("OutputIntents", Object::Array(vec![Object::Dict(oi)]));
        }
        self.insert(self.catalog, Object::Dict(cat));
        // Deterministic file identifier: a hash of every object as written (ISO 32000 14.4).
        let mut hasher = Sha256::new();
        let mut buf = Vec::new();
        for (num, e) in &self.objects {
            buf.clear();
            buf.extend_from_slice(&num.to_be_bytes());
            write_object(&e.obj, &mut buf);
            hasher.update(&buf);
        }
        let digest: [u8; 32] = hasher.finalize().into();
        let id = Object::Str(digest[..16].to_vec());
        let mut trailer = Dict::default();
        trailer.set("Root", rf(self.catalog));
        if let Some(info) = self.info {
            trailer.set("Info", rf(info));
        }
        trailer.set("ID", Object::Array(vec![id.clone(), id]));
        let bytes = write_document(&self.version, &self.objects, &[], &trailer, &self.limits, cancel)?;
        let receipt = self.read_back(&bytes, cancel)?;
        Ok((bytes, receipt))
    }

    /// The receipt from the written bytes, through `PdfDoc::open` (the crate's strict reader).
    fn read_back(&self, bytes: &[u8], cancel: &CancellationToken) -> Result<ExportReceipt> {
        let doc = PdfDoc::open(bytes, &self.limits, cancel)?;
        let inspection = doc.inspect();
        let oi = doc
            .catalog()?
            .get("OutputIntents")
            .map(|o| doc.resolve(o))
            .and_then(Object::as_array)
            .and_then(|a| a.first())
            .map(|o| doc.resolve(o))
            .and_then(Object::as_dict);
        let text = |key: &str| -> Option<String> {
            match oi?.get(key).map(|o| doc.resolve(o)) {
                Some(Object::Str(s)) => Some(text_string(s)),
                _ => None,
            }
        };
        let profile = match oi.and_then(|d| d.get("DestOutputProfile")).map(|o| doc.resolve(o)) {
            Some(Object::Stream(s)) => {
                let mut budget = self.limits.max_decoded_total;
                Some(sha256_hex(&crate::filters::decode_stream(s, &self.limits, &mut budget)?))
            }
            _ => None,
        };
        Ok(ExportReceipt {
            version: inspection.version,
            pages: inspection.page_count,
            fonts_embedded: u32::try_from(self.fonts.len()).unwrap_or(u32::MAX),
            images: u32::try_from(self.images).unwrap_or(u32::MAX),
            bytes: bytes.len(),
            sha256_hex: inspection.source_sha256,
            effective_destination_profile_sha256: profile,
            effective_output_condition: text("OutputConditionIdentifier"),
            effective_oc_registry: text("RegistryName"),
            effective_pdfx_profile: None,
            conformance: EXPORT_CONFORMANCE,
        })
    }
}

/// `/W` with runs of consecutive glyph ids grouped: `[g [w1 w2 ...] ...]` (ISO 32000 9.7.4.3).
fn w_array(widths: &[(u16, u16)]) -> Object {
    let map: BTreeMap<u16, u16> = widths.iter().copied().collect();
    let mut out = Vec::new();
    let mut run: Option<(u16, Vec<Object>, u16)> = None;
    for (g, w) in map {
        match &mut run {
            Some((_, ws, last)) if last.checked_add(1) == Some(g) => {
                ws.push(int(w));
                *last = g;
            }
            _ => {
                if let Some((start, ws, _)) = run.take() {
                    out.push(int(start));
                    out.push(Object::Array(ws));
                }
                run = Some((g, vec![int(w)], g));
            }
        }
    }
    if let Some((start, ws, _)) = run {
        out.push(int(start));
        out.push(Object::Array(ws));
    }
    Object::Array(out)
}

/// A `/ToUnicode` CMap (Adobe TN 5411) with 2-byte codes and UTF-16BE destinations.
fn to_unicode_cmap(map: &[(u16, &str)]) -> String {
    let entries: BTreeMap<u16, &str> = map.iter().copied().collect();
    let mut s = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    let list: Vec<(u16, &str)> = entries.into_iter().collect();
    for chunk in list.chunks(BFCHAR_BLOCK) {
        s.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for (g, text) in chunk {
            s.push_str(&format!("<{g:04X}> <"));
            for u in text.encode_utf16() {
                s.push_str(&format!("{u:04X}"));
            }
            s.push_str(">\n");
        }
        s.push_str("endbfchar\n");
    }
    s.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    s
}
