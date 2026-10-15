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
//! /Identity`, `/W` from the caller, `/FontFile2` subset to the used glyphs with ids preserved (`subset.rs`;
//! `ExportOptions::subset_fonts`, test `subset_font_keeps_ids_and_outlines`), `/ToUnicode` from the caller so
//! text stays extractable), an optional output intent (`/DestOutputProfile`) and `/Info /Title`.
//! Deterministic: no dates, the trailer `/ID` is a hash of the objects (test
//! `export_is_deterministic`).
//!
//! Not in this slice: CFF / OpenType-CFF embedding (refused
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
mod cff;
mod pdfx;
mod render;
mod sfnt;
pub use pdfx::{PDFA2B_CONFORMANCE, PDFA2B_PART, PDFX4_VERSION, PdfXReport, PdfXRule, check_pdfx4};
mod subset;
mod verdict;
pub use verdict::{CONFORMANCE_PDFA2B_VERAPDF, MAX_REPORT_BYTES, VERAPDF_PDFA2B_PROFILE};
pub use render::{RenderedPage, render_page};
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
/// Most stops of one gradient (Folio's ceiling).
pub const MAX_GRADIENT_STOPS: usize = 1024;
/// Deepest nesting of open transparency groups on one page.
const MAX_GROUP_DEPTH: usize = 32;
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
    /// An alpha or opacity outside 0..=1.
    AlphaOutOfRange,
    /// A gradient with no stops, more than [`MAX_GRADIENT_STOPS`], offsets outside 0..=1 or decreasing, a negative
    /// radius or a singular matrix.
    InvalidGradient,
    /// A PDF/X-4 requirement the request cannot meet (`what` names it).
    PdfXRequirement(&'static str),
    /// A PDF/A-2b requirement the request cannot meet (`what` names it).
    PdfARequirement(&'static str),
    /// [`ExportReceipt::with_verapdf_report`] refused the report (`why` names the reason).
    ReportRefused(&'static str),
    /// `end_group` without an open group, `finish` with one open, or groups deeper than 32.
    UnbalancedGroup,
    /// A soft mask image that is not `DeviceGray` without alpha.
    InvalidMask,
    /// `render_page`: the canvas at the requested scale is over `Limits::max_flatten_pixels`.
    RenderOverBudget,
    /// `render_page`: the interpreter could not open the file or stopped (panic).
    RenderRefused,
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
            Self::AlphaOutOfRange => "alpha_out_of_range",
            Self::InvalidGradient => "invalid_gradient",
            Self::PdfXRequirement(_) => "pdfx_requirement",
            Self::PdfARequirement(_) => "pdfa_requirement",
            Self::ReportRefused(_) => "report_refused",
            Self::UnbalancedGroup => "unbalanced_group",
            Self::InvalidMask => "invalid_mask",
            Self::RenderOverBudget => "render_over_budget",
            Self::RenderRefused => "render_refused",
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
    /// Embed only the glyphs used (glyph ids preserved, `subset.rs`; default true). `false` embeds
    /// the whole font program.
    pub subset_fonts: bool,
    /// Write the PDF/X-4 identification this crate implements (`pdfx.rs`; conformance stays
    /// `not_proven`). Requires a `GtsPdfx` output intent, version 1.6 or later and a TrimBox on
    /// every page.
    pub pdfx4: Option<PdfX4<'a>>,
    /// Write the PDF/A-2b identification (XMP `pdfaid` part 2 conformance B, the XMP extension
    /// schemas for the non-predefined properties, a `GTS_PDFA1` output intent sharing the
    /// destination profile). Requires an output intent and a 1.x header. Conformance stays
    /// `not_proven` until a passing veraPDF report is attached ([`ExportReceipt::with_verapdf_report`]).
    pub pdfa2b: bool,
}

/// PDF/X-4 identification values (no clock is read: dates come from the caller).
#[derive(Clone, Copy, Debug)]
pub struct PdfX4<'a> {
    /// PDF date strings (`D:20261015000000Z`).
    pub created: &'a str,
    pub modified: &'a str,
    /// `/Trapped`: whether the document has been trapped.
    pub trapped: bool,
}
impl Default for ExportOptions<'_> {
    fn default() -> Self {
        Self { version: "1.7", output_intent: None, info_title: None, subset_fonts: true, pdfx4: None, pdfa2b: false }
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
    /// [`EXPORT_CONFORMANCE`] (`not_proven`; test `two_page_export_reads_back`), or
    /// [`CONFORMANCE_PDFA2B_VERAPDF`] once a passing veraPDF report is attached
    /// ([`ExportReceipt::with_verapdf_report`]).
    pub conformance: &'static str,
    /// sha256 of the validator report behind `conformance` (`None` while `not_proven`).
    pub conformance_report_sha256: Option<String>,
    /// Validator and profile of that report (`veraPDF 1.30.3: PDF/A-2b validation profile`).
    pub conformance_validator: Option<String>,
}

/// PDF blend modes (ISO 32000-2 11.3.5, Tables 134 and 135).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BlendMode {
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
}
impl BlendMode {
    /// The `/BM` name.
    pub const fn pdf_name(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Multiply => "Multiply",
            Self::Screen => "Screen",
            Self::Overlay => "Overlay",
            Self::Darken => "Darken",
            Self::Lighten => "Lighten",
            Self::ColorDodge => "ColorDodge",
            Self::ColorBurn => "ColorBurn",
            Self::HardLight => "HardLight",
            Self::SoftLight => "SoftLight",
            Self::Difference => "Difference",
            Self::Exclusion => "Exclusion",
            Self::Hue => "Hue",
            Self::Saturation => "Saturation",
            Self::Color => "Color",
            Self::Luminosity => "Luminosity",
        }
    }
}

/// Axial or radial shading geometry in gradient space (mapped to user space by `Gradient::matrix`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GradientGeometry {
    Axial { from: [f64; 2], to: [f64; 2] },
    Radial { from: [f64; 2], from_radius: f64, to: [f64; 2], to_radius: f64 },
}

/// One colour stop: offset 0..=1 along the gradient, colour components 0..=1, opacity 0..=1 and
/// an optional midpoint (0 < m < 1: the fraction of the way to the next stop where the colour is
/// the even mix; the piece is the power curve x^N with m^N = 1/2).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GradientStop {
    pub offset: f64,
    pub colour: [f64; 3],
    pub opacity: f64,
    pub midpoint: Option<f64>,
}
impl GradientStop {
    /// An opaque stop without a midpoint.
    pub const fn new(offset: f64, colour: [f64; 3]) -> Self {
        Self { offset, colour, opacity: 1.0, midpoint: None }
    }
}

/// What a gradient does past its first and last stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GradientSpread {
    /// The end colours continue (`/Extend` as given).
    Pad,
    /// The stops repeat.
    Repeat,
    /// The stops repeat, every other cycle mirrored.
    Reflect,
}

/// A gradient fill: geometry, stops (non-decreasing offsets), `/Extend` (used by `Pad`), spread,
/// the gradient-to-user matrix and the colour space (`None` = DeviceRGB, else a 3-component ICC
/// profile). Stop opacity below 1 becomes a luminosity soft mask of the same geometry (a gray
/// shading of the opacities).
#[derive(Clone, Copy, Debug)]
pub struct Gradient<'a> {
    pub geometry: GradientGeometry,
    pub stops: &'a [GradientStop],
    pub extend: [bool; 2],
    pub spread: GradientSpread,
    pub matrix: [f64; 6],
    pub profile: Option<IccHandle>,
}

/// One run of a text clip: font, size and `(glyph id, x, y)` placements in points.
pub type TextClipRun<'a> = (FontHandle, f64, &'a [(u16, f64, f64)]);

/// Most repeat / reflect cycles one gradient fill expands to.
pub const MAX_GRADIENT_CYCLES: i64 = 256;
/// Most linear pieces of one gradient function after expansion.
pub const MAX_GRADIENT_PIECES: usize = 4096;

/// Constant alpha and blend mode of the drawings that follow on a page (an `/ExtGState` with
/// `/ca`, `/CA`, `/BM` set after each drawing's `q`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawState {
    pub fill_alpha: f64,
    pub stroke_alpha: f64,
    pub blend: BlendMode,
}
impl Default for DrawState {
    fn default() -> Self {
        Self { fill_alpha: 1.0, stroke_alpha: 1.0, blend: BlendMode::Normal }
    }
}

/// One image of a soft-mask group, drawn through `placement`.
#[derive(Clone, Copy, Debug)]
pub struct MaskImage<'a> {
    pub image: ImageSpec<'a>,
    pub placement: [f64; 6],
}

/// A soft mask (ISO 32000-1 11.6.5): a transparency group drawing `images` (with their own alpha,
/// any colour space); luminosity masks read the group's luminosity over `/BC`, alpha masks its
/// alpha. Outside the images a luminosity mask is the backdrop and an alpha mask 0.
#[derive(Clone, Copy, Debug)]
pub struct SoftMask<'a> {
    pub images: &'a [MaskImage<'a>],
    /// `/S`: luminosity (default) or alpha (the mask group's alpha).
    pub alpha: bool,
    /// `/BC` gray backdrop of a luminosity mask (0 = black, the ISO default).
    pub backdrop: f64,
    /// `/TR` as an exponential function (type 2, C0 0, C1 1) with this exponent; `None` = identity.
    pub transfer_exponent: Option<f64>,
}
impl<'a> SoftMask<'a> {
    /// A luminosity mask on a black backdrop with no transfer function.
    pub fn luminosity(images: &'a [MaskImage<'a>]) -> Self {
        Self { images, alpha: false, backdrop: 0.0, transfer_exponent: None }
    }
    /// An alpha mask (the images' alpha, 0 outside them).
    pub fn alpha(images: &'a [MaskImage<'a>]) -> Self {
        Self { images, alpha: true, backdrop: 0.0, transfer_exponent: None }
    }
}

/// Most images one soft mask may draw.
pub const MAX_MASK_IMAGES: usize = 256;

/// How a closed group is painted: constant opacity, blend mode, isolated / knockout flags of the
/// transparency group and an optional soft mask.
#[derive(Clone, Copy, Debug)]
pub struct GroupStyle<'a> {
    pub opacity: f64,
    pub blend: BlendMode,
    pub isolated: bool,
    pub knockout: bool,
    pub soft_mask: Option<SoftMask<'a>>,
}

/// Content of one page or of one open transparency group, with the resources it uses.
#[derive(Default)]
struct Level {
    ops: Vec<Op>,
    fonts: BTreeSet<usize>,
    profiles: BTreeSet<usize>,
    /// (resource name, XObject number) of images and forms.
    xobjects: Vec<(String, u32)>,
    gstates: Vec<(String, u32)>,
    shadings: Vec<(String, u32)>,
    /// Open `push_clip` levels.
    clips: usize,
    state: DrawState,
}

struct PageBuild {
    spec: PageSpec,
    page_obj: u32,
    content_obj: u32,
    /// `levels[0]` is the page; the others are open groups, innermost last.
    levels: Vec<Level>,
}

struct FontBuild {
    type0: u32,
    num_glyphs: u16,
    /// CID of each glyph id for a CID-keyed CFF font (codes are CIDs); `None`: codes are glyph ids.
    gid_to_cid: Option<std::rc::Rc<Vec<u16>>>,
    /// Index into `PdfExport::subset_files` when subsetting.
    file: Option<usize>,
}

/// A font program subset at `finish`: its bytes, the glyphs used by every font sharing it, its
/// object number and the (descriptor, CIDFont, Type0, base name) objects whose names get the tag.
struct SubsetFile {
    /// A CID-keyed CFF program (`cff::subset_cid`, `/Subtype /CIDFontType0C`) instead of TrueType.
    cff: bool,
    bytes: Vec<u8>,
    num: u32,
    used: BTreeSet<u16>,
    members: Vec<(u32, u32, u32, String)>,
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
    subset: bool,
    pdfx4: Option<(String, String, bool, String)>,
    /// PDF/A-2b identification requested; the title for the XMP `dc:title`.
    pdfa2b: bool,
    title: Option<String>,
    shadings: usize,
    subset_files: Vec<SubsetFile>,
    subset_by_hash: BTreeMap<[u8; 32], usize>,
    profiles: Vec<IccBuild>,
    images: usize,
    forms: usize,
    /// Shared `/ExtGState` objects of draw states: (fill alpha bits, stroke alpha bits, blend) ->
    /// (resource name, object).
    gstates: BTreeMap<(u64, u64, BlendMode), (String, u32)>,
    unique_gstates: usize,
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
fn name_obj(n: &str) -> Object {
    Object::Name(n.as_bytes().to_vec())
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
            subset: options.subset_fonts,
            pdfx4: None,
            pdfa2b: options.pdfa2b,
            title: options.info_title.map(str::to_owned),
            shadings: 0,
            subset_files: Vec::new(),
            subset_by_hash: BTreeMap::new(),
            profiles: Vec::new(),
            images: 0,
            forms: 0,
            gstates: BTreeMap::new(),
            unique_gstates: 0,
            raw_total: 0,
            written_total: 0,
        };
        ex.reserve(4)?;
        ex.catalog = ex.alloc();
        ex.pages_root = ex.alloc();
        if let Some(x) = &options.pdfx4 {
            let ok_version = options.version.split_once('.').and_then(|(a, b)| Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?))).is_some_and(|(a, b)| a > 1 || b >= 6);
            if !ok_version {
                return Err(ExportError::PdfXRequirement("version_1_6"));
            }
            if !options.output_intent.is_some_and(|o| o.subtype == OutputIntentSubtype::GtsPdfx) {
                return Err(ExportError::PdfXRequirement("gts_pdfx_output_intent"));
            }
            if !(pdfx::valid_date(x.created) && pdfx::valid_date(x.modified)) {
                return Err(ExportError::PdfXRequirement("pdf_dates"));
            }
        }
        if options.pdfa2b {
            if options.output_intent.is_none() {
                return Err(ExportError::PdfARequirement("output_intent"));
            }
            if !options.version.starts_with("1.") {
                return Err(ExportError::PdfARequirement("version_1_x"));
            }
        }
        if options.info_title.is_some() || options.pdfx4.is_some() || options.pdfa2b {
            let mut info = Dict::default();
            if let Some(title) = options.info_title {
                info.set("Title", text_bytes(title)?);
            }
            info.set("Producer", Object::Str(b"hsk-studio-press".to_vec()));
            if let Some(x) = &options.pdfx4 {
                info.set("GTS_PDFXVersion", Object::Str(pdfx::PDFX4_VERSION.as_bytes().to_vec()));
                info.set("CreationDate", Object::Str(x.created.as_bytes().to_vec()));
                info.set("ModDate", Object::Str(x.modified.as_bytes().to_vec()));
                info.set("Trapped", name(if x.trapped { "True" } else { "False" }));
                ex.pdfx4 = Some((x.created.to_owned(), x.modified.to_owned(), x.trapped, options.info_title.unwrap_or("").to_owned()));
            }
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

    /// The innermost open level (group or page) of `page`.
    fn top(&self, page: PageHandle) -> Result<&Level> {
        self.page(page)?.levels.last().ok_or(ExportError::NoSuchPage)
    }
    fn top_mut(&mut self, page: PageHandle) -> Result<&mut Level> {
        self.pages.get_mut(page.0).and_then(|p| p.levels.last_mut()).ok_or(ExportError::NoSuchPage)
    }

    fn push_ops(&mut self, page: PageHandle, ops: Vec<Op>) -> Result<()> {
        let max = self.limits.max_content_ops;
        let l = self.top_mut(page)?;
        match l.ops.len().checked_add(ops.len()) {
            Some(n) if n <= max => {
                l.ops.extend(ops);
                Ok(())
            }
            _ => Err(ExportError::TooManyOperations),
        }
    }

    /// The shared `/ExtGState` of a draw state (created once).
    fn gstate_for(&mut self, st: DrawState) -> Result<(String, u32)> {
        let key = (st.fill_alpha.to_bits(), st.stroke_alpha.to_bits(), st.blend);
        if let Some(v) = self.gstates.get(&key) {
            return Ok(v.clone());
        }
        self.reserve(1)?;
        let mut d = Dict::default();
        d.set("Type", name("ExtGState"));
        d.set("ca", real(st.fill_alpha));
        d.set("CA", real(st.stroke_alpha));
        d.set("BM", name(st.blend.pdf_name()));
        let num = self.alloc();
        self.insert(num, Object::Dict(d));
        let v = (format!("GS{}", self.gstates.len()), num);
        self.gstates.insert(key, v.clone());
        Ok(v)
    }

    /// Alpha and blend mode for the drawings that follow on `page` (in the innermost open group).
    pub fn set_draw_state(&mut self, page: PageHandle, state: DrawState) -> Result<()> {
        finite(&[state.fill_alpha, state.stroke_alpha])?;
        if !((0.0..=1.0).contains(&state.fill_alpha) && (0.0..=1.0).contains(&state.stroke_alpha)) {
            return Err(ExportError::AlphaOutOfRange);
        }
        self.top_mut(page)?.state = state;
        Ok(())
    }

    /// Opens a transparency group on `page`: what is drawn until the matching [`Self::end_group`]
    /// is composited as one (a form XObject with `/Group /S /Transparency`).
    pub fn begin_group(&mut self, page: PageHandle) -> Result<()> {
        let p = self.pages.get_mut(page.0).ok_or(ExportError::NoSuchPage)?;
        if p.levels.len() > MAX_GROUP_DEPTH {
            return Err(ExportError::UnbalancedGroup);
        }
        p.levels.push(Level::default());
        Ok(())
    }

    /// Closes the innermost group of `page` and paints it with `style` in the enclosing level. An
    /// empty group writes nothing.
    pub fn end_group(&mut self, page: PageHandle, style: GroupStyle<'_>) -> Result<()> {
        let p = self.page(page)?;
        if p.levels.len() < 2 {
            return Err(ExportError::UnbalancedGroup);
        }
        finite(&[style.opacity])?;
        if !(0.0..=1.0).contains(&style.opacity) {
            return Err(ExportError::AlphaOutOfRange);
        }
        let media = p.spec.media_box_pt;
        let mask = match style.soft_mask {
            None => None,
            Some(m) => {
                if m.images.is_empty() || m.images.len() > MAX_MASK_IMAGES {
                    return Err(ExportError::InvalidMask);
                }
                finite(&[m.backdrop])?;
                if !(0.0..=1.0).contains(&m.backdrop) {
                    return Err(ExportError::ColourOutOfRange);
                }
                if let Some(n) = m.transfer_exponent {
                    finite(&[n])?;
                    if n <= 0.0 {
                        return Err(ExportError::ValueOutOfRange);
                    }
                }
                let mut imgs = Vec::with_capacity(m.images.len());
                for mi in m.images {
                    finite(&mi.placement)?;
                    imgs.push((self.image_objects(mi.image)?, mi.placement));
                }
                Some((m, imgs))
            }
        };
        let level = self.top(page)?;
        if level.clips != 0 {
            return Err(ExportError::UnbalancedGroup);
        }
        if level.ops.is_empty() {
            if let Some(p) = self.pages.get_mut(page.0) {
                p.levels.pop();
            }
            return Ok(());
        }
        let content = write_ops(&level.ops, &[])?;
        let resources = self.resources(level)?;
        let (form_stream, form_len) = {
            let mut d = Dict::default();
            d.set("Type", name("XObject"));
            d.set("Subtype", name("Form"));
            d.set("BBox", rect(&media));
            let mut g = Dict::default();
            g.set("Type", name("Group"));
            g.set("S", name("Transparency"));
            g.set("I", Object::Bool(style.isolated));
            g.set("K", Object::Bool(style.knockout));
            d.set("Group", Object::Dict(g));
            d.set("Resources", Object::Dict(resources));
            flate_stream(d, &content)
        };
        // Mask group content: every mask image through its placement.
        let mask_content = match &mask {
            Some((_, imgs)) => {
                let mut ops = Vec::with_capacity(imgs.len().saturating_mul(4));
                for (i, (_, placement)) in imgs.iter().enumerate() {
                    ops.push(op("q", vec![]));
                    ops.push(op("cm", placement.iter().map(|v| real(*v)).collect()));
                    ops.push(op("Do", vec![name_obj(&format!("MIm{i}"))]));
                    ops.push(op("Q", vec![]));
                }
                Some(write_ops(&ops, &[])?)
            }
            None => None,
        };
        // Parent level must take q / gs / Do / Q.
        let parent_ops = self.page(page)?.levels.iter().rev().nth(1).map_or(0, |l| l.ops.len());
        match parent_ops.checked_add(4) {
            Some(n) if n <= self.limits.max_content_ops => {}
            _ => return Err(ExportError::TooManyOperations),
        }
        let image_objects: u32 = mask.as_ref().map_or(0, |(_, imgs)| imgs.iter().map(|((_, sm, _, _), _)| if sm.is_some() { 2u32 } else { 1 }).fold(0u32, |a, b| a.saturating_add(b)));
        let objects = 2u32.saturating_add(if mask.is_some() { 1 } else { 0 }).saturating_add(image_objects);
        self.reserve(objects)?;
        let mut raw = content.len();
        let mut written = form_len;
        if let Some((_, imgs)) = &mask {
            for ((_, _, img_raw, img_written), _) in imgs {
                raw = raw.checked_add(*img_raw).ok_or(ExportError::AggregateOverBudget)?;
                written = written.checked_add(*img_written).ok_or(ExportError::AggregateOverBudget)?;
            }
        }
        let mask_form = mask_content.map(|c| {
            let mut d = Dict::default();
            d.set("Type", name("XObject"));
            d.set("Subtype", name("Form"));
            d.set("BBox", rect(&media));
            let mut g = Dict::default();
            g.set("Type", name("Group"));
            g.set("S", name("Transparency"));
            g.set("CS", name("DeviceGray"));
            d.set("Group", Object::Dict(g));
            let len = c.len();
            (flate_stream(d, &c), len)
        });
        if let Some(((_, l), c)) = &mask_form {
            raw = raw.checked_add(*c).ok_or(ExportError::AggregateOverBudget)?;
            written = written.checked_add(*l).ok_or(ExportError::AggregateOverBudget)?;
        }
        self.charge(raw, written)?;
        // Everything is checked and charged: build objects.
        let form = self.alloc();
        self.insert(form, form_stream);
        let mut gs = Dict::default();
        gs.set("Type", name("ExtGState"));
        gs.set("ca", real(style.opacity));
        gs.set("CA", real(style.opacity));
        gs.set("BM", name(style.blend.pdf_name()));
        if let (Some((m, imgs)), Some(((mut mform, _), _))) = (mask, mask_form) {
            let mut x = Dict::default();
            for (i, ((img, smask, _, _), _)) in imgs.into_iter().enumerate() {
                let mut img = img;
                if let Some(sm) = smask {
                    let snum = self.alloc();
                    self.insert(snum, sm);
                    if let Object::Stream(st) = &mut img {
                        st.dict.set("SMask", rf(snum));
                    }
                }
                let inum = self.alloc();
                self.insert(inum, img);
                x.set_key(format!("MIm{i}").as_bytes(), rf(inum));
            }
            if let Object::Stream(st) = &mut mform {
                let mut r = Dict::default();
                r.set("XObject", Object::Dict(x));
                st.dict.set("Resources", Object::Dict(r));
            }
            let mnum = self.alloc();
            self.insert(mnum, mform);
            gs.set("SMask", Object::Dict(mask_dict(mnum, m.alpha, m.backdrop, m.transfer_exponent)));
        }
        let gnum = self.alloc();
        self.insert(gnum, Object::Dict(gs));
        let gname = format!("GSu{}", self.unique_gstates);
        self.unique_gstates = self.unique_gstates.saturating_add(1);
        let fname = format!("Fm{}", self.forms);
        self.forms = self.forms.saturating_add(1);
        if let Some(p) = self.pages.get_mut(page.0) {
            p.levels.pop();
        }
        let parent = self.top_mut(page)?;
        parent.ops.extend([
            op("q", vec![]),
            op("gs", vec![name_obj(&gname)]),
            op("Do", vec![name_obj(&fname)]),
            op("Q", vec![]),
        ]);
        parent.gstates.push((gname, gnum));
        parent.xobjects.push((fname, form));
        Ok(())
    }

    /// The resource dictionary of one level.
    fn resources(&self, l: &Level) -> Result<Dict> {
        let mut res = Dict::default();
        if !l.fonts.is_empty() {
            let mut d = Dict::default();
            for f in &l.fonts {
                let num = self.fonts.get(*f).ok_or(ExportError::NoSuchFont)?.type0;
                d.set_key(format!("F{f}").as_bytes(), rf(num));
            }
            res.set("Font", Object::Dict(d));
        }
        if !l.xobjects.is_empty() {
            let mut d = Dict::default();
            for (n, num) in &l.xobjects {
                d.set_key(n.as_bytes(), rf(*num));
            }
            res.set("XObject", Object::Dict(d));
        }
        if !l.profiles.is_empty() {
            let mut d = Dict::default();
            for h in &l.profiles {
                let space = self.profiles.get(*h).ok_or(ExportError::NoSuchProfile)?.space;
                d.set_key(format!("CS{h}").as_bytes(), rf(space));
            }
            res.set("ColorSpace", Object::Dict(d));
        }
        if !l.shadings.is_empty() {
            let mut d = Dict::default();
            for (n, num) in &l.shadings {
                d.set_key(n.as_bytes(), rf(*num));
            }
            res.set("Shading", Object::Dict(d));
        }
        if !l.gstates.is_empty() {
            let mut d = Dict::default();
            for (n, num) in &l.gstates {
                d.set_key(n.as_bytes(), rf(*num));
            }
            res.set("ExtGState", Object::Dict(d));
        }
        Ok(res)
    }

    /// Adds a page; boxes must be finite with positive size, trim and bleed inside the media box.
    pub fn add_page(&mut self, spec: PageSpec) -> Result<PageHandle> {
        check_box(&spec.media_box_pt)?;
        if self.pdfx4.is_some() && spec.trim_box_pt.is_none() {
            return Err(ExportError::PdfXRequirement("page_trim_box"));
        }
        if let (Some(t), Some(b)) = (spec.trim_box_pt, spec.bleed_box_pt)
            && !inside(&t, &b)
        {
            return Err(ExportError::BoxOutsideMedia);
        }
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
            levels: vec![Level::default()],
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
        let state = self.top(page)?.state;
        let gs = if state == DrawState::default() { None } else { Some(self.gstate_for(state)?) };
        let mut ops = Vec::with_capacity(body.len().saturating_add(3));
        ops.push(op("q", vec![]));
        if let Some((n, _)) = &gs {
            ops.push(op("gs", vec![name_obj(n)]));
        }
        ops.extend(body);
        ops.push(op("Q", vec![]));
        self.push_ops(page, ops)?;
        let l = self.top_mut(page)?;
        if let Some(i) = colour {
            l.profiles.insert(i);
        }
        if let Some(g) = gs
            && !l.gstates.contains(&g)
        {
            l.gstates.push(g);
        }
        Ok(())
    }

    /// Clips the drawings that follow on `page` (innermost open group) to `path` until the matching
    /// [`Self::pop_clip`] (`q path W n`; ISO 32000-1 8.5.4). Depth is capped at 32.
    pub fn push_clip(&mut self, page: PageHandle, path: &[PathCommand], rule: FillRule) -> Result<()> {
        let mut ops = vec![op("q", vec![])];
        ops.extend(Self::path_ops(path)?);
        ops.push(op(if rule == FillRule::EvenOdd { "W*" } else { "W" }, vec![]));
        ops.push(op("n", vec![]));
        if self.top(page)?.clips >= MAX_GROUP_DEPTH {
            return Err(ExportError::UnbalancedGroup);
        }
        self.push_ops(page, ops)?;
        let l = self.top_mut(page)?;
        l.clips = l.clips.saturating_add(1);
        Ok(())
    }

    /// Ends the innermost [`Self::push_clip`] of `page`.
    pub fn pop_clip(&mut self, page: PageHandle) -> Result<()> {
        if self.top(page)?.clips == 0 {
            return Err(ExportError::UnbalancedGroup);
        }
        self.push_ops(page, vec![op("Q", vec![])])?;
        let l = self.top_mut(page)?;
        l.clips = l.clips.saturating_sub(1);
        Ok(())
    }

    /// Fills `path` with an axial (type 2) or radial (type 3) shading (ISO 32000-1 8.7.4.5.3/4):
    /// `q [mask gs] path W n cm /Sh sh Q`. The stops become a stitching function (type 3) of type-2
    /// pieces (exponent from the stop's midpoint, else linear), padded to offsets 0 and 1 with the end
    /// colours. `Repeat` / `Reflect` expand the function over the integer cycles the path's bounds
    /// reach in gradient space (at most [`MAX_GRADIENT_CYCLES`]) and move the shading's end points to
    /// the first and last cycle; stop opacity below 1 adds a luminosity soft mask whose group draws the
    /// same shading geometry in DeviceGray with the opacities.
    pub fn fill_path_gradient(&mut self, page: PageHandle, path: &[PathCommand], rule: FillRule, g: &Gradient<'_>) -> Result<()> {
        self.page(page)?;
        finite(&g.matrix)?;
        let [a, b, c, d, e, f] = g.matrix;
        let det = a * d - b * c;
        if det.abs() < 1e-12 {
            return Err(ExportError::InvalidGradient);
        }
        if g.stops.is_empty() || g.stops.len() > MAX_GRADIENT_STOPS {
            return Err(ExportError::InvalidGradient);
        }
        for st in g.stops {
            finite(&[st.offset, st.opacity])?;
            finite(&st.colour)?;
            if let Some(m) = st.midpoint {
                finite(&[m])?;
                if !(m > 0.0 && m < 1.0) {
                    return Err(ExportError::InvalidGradient);
                }
            }
            if !(0.0..=1.0).contains(&st.offset) || !(0.0..=1.0).contains(&st.opacity) || !st.colour.iter().all(|v| (0.0..=1.0).contains(v)) {
                return Err(ExportError::InvalidGradient);
            }
        }
        if g.stops.windows(2).any(|w| w[1].offset < w[0].offset) {
            return Err(ExportError::InvalidGradient);
        }
        let (p0, r0, p1, r1, kind) = match g.geometry {
            GradientGeometry::Axial { from, to } => {
                finite(&[from[0], from[1], to[0], to[1]])?;
                (from, 0.0, to, 0.0, 2)
            }
            GradientGeometry::Radial { from, from_radius, to, to_radius } => {
                finite(&[from[0], from[1], from_radius, to[0], to[1], to_radius])?;
                if from_radius < 0.0 || to_radius < 0.0 {
                    return Err(ExportError::InvalidGradient);
                }
                (from, from_radius, to, to_radius, 3)
            }
        };
        let path_ops = Self::path_ops(path)?;
        // Cycles: the parameter range the path's bounds reach in gradient space.
        let (k0, k1) = if g.spread == GradientSpread::Pad {
            (0i64, 1i64)
        } else {
            let pts: Vec<(f64, f64)> = path
                .iter()
                .flat_map(|cmd| match *cmd {
                    PathCommand::MoveTo(x, y) | PathCommand::LineTo(x, y) => vec![(x, y)],
                    PathCommand::CubicTo(x1, y1, x2, y2, x3, y3) => vec![(x1, y1), (x2, y2), (x3, y3)],
                    PathCommand::Close => vec![],
                })
                .collect();
            let (mut lo, mut hi) = (0.0f64, 1.0f64);
            // Corners of the bounds, mapped back to gradient space.
            let xs = pts.iter().map(|p| p.0);
            let ys = pts.iter().map(|p| p.1);
            let (x0, x1) = (xs.clone().fold(f64::MAX, f64::min), xs.fold(f64::MIN, f64::max));
            let (y0, y1) = (ys.clone().fold(f64::MAX, f64::min), ys.fold(f64::MIN, f64::max));
            for (ux, uy) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
                let (tx, ty) = (ux - e, uy - f);
                let gx = (d * tx - c * ty) / det;
                let gy = (-b * tx + a * ty) / det;
                let sv = if kind == 2 {
                    let (dx, dy) = (p1[0] - p0[0], p1[1] - p0[1]);
                    let len2 = dx * dx + dy * dy;
                    if len2 <= 0.0 { 0.0 } else { ((gx - p0[0]) * dx + (gy - p0[1]) * dy) / len2 }
                } else {
                    // Largest s with the point on circle s (no clipping to [0, 1]).
                    let (cdx, cdy, dr) = (p1[0] - p0[0], p1[1] - p0[1], r1 - r0);
                    let (pdx, pdy) = (gx - p0[0], gy - p0[1]);
                    let qa = cdx * cdx + cdy * cdy - dr * dr;
                    let qb = pdx * cdx + pdy * cdy + r0 * dr;
                    let qc = pdx * pdx + pdy * pdy - r0 * r0;
                    if qa.abs() < 1e-12 {
                        if qb.abs() > 1e-12 { qc / (2.0 * qb) } else { 1.0 }
                    } else {
                        let disc = (qb * qb - qa * qc).max(0.0);
                        ((qb + disc.sqrt()) / qa).max((qb - disc.sqrt()) / qa)
                    }
                };
                if sv.is_finite() {
                    lo = lo.min(sv);
                    hi = hi.max(sv);
                }
            }
            if kind == 3 {
                // Radii must stay non-negative: no cycle below the parameter where r(s) = 0.
                let dr = r1 - r0;
                if dr > 0.0 {
                    lo = lo.max(-r0 / dr);
                }
            }
            let (k0, k1) = (lo.floor(), hi.ceil());
            if !(k0.is_finite() && k1.is_finite()) || k1 - k0 > MAX_GRADIENT_CYCLES as f64 {
                return Err(ExportError::InvalidGradient);
            }
            (k0 as i64, k1 as i64)
        };
        let cycles = k1.saturating_sub(k0).max(1);
        let mut stops: Vec<GradientStop> = g.stops.to_vec();
        if let Some(first) = stops.first().copied()
            && first.offset > 0.0
        {
            stops.insert(0, GradientStop { offset: 0.0, midpoint: None, ..first });
        }
        if let Some(last) = stops.last().copied()
            && last.offset < 1.0
        {
            stops.push(GradientStop { offset: 1.0, midpoint: None, ..last });
        }
        if stops.len() == 1 {
            stops.push(stops[0]);
        }
        let pieces_per_cycle = stops.len().saturating_sub(1);
        if usize::try_from(cycles).unwrap_or(usize::MAX).saturating_mul(pieces_per_cycle) > MAX_GRADIENT_PIECES {
            return Err(ExportError::InvalidGradient);
        }
        let opacity = stops.iter().any(|st| st.opacity < 1.0);
        // One function over t in [0, 1] covering cycles k0..k1; `value` picks colour or opacity.
        let build = |value: &dyn Fn(&GradientStop) -> Vec<f64>| -> Object {
            let piece = |c0: Vec<f64>, c1: Vec<f64>, n: f64| {
                let mut fd = Dict::default();
                fd.set("FunctionType", int(2));
                fd.set("Domain", Object::Array(vec![real(0.0), real(1.0)]));
                fd.set("C0", Object::Array(c0.iter().map(|v| real(*v)).collect()));
                fd.set("C1", Object::Array(c1.iter().map(|v| real(*v)).collect()));
                fd.set("N", real(n));
                Object::Dict(fd)
            };
            let exponent = |m: Option<f64>| m.map_or(1.0, |m| 0.5f64.ln() / m.ln());
            let mut functions = Vec::new();
            let mut bounds = Vec::new();
            let span = cycles as f64;
            for cyc in 0..cycles {
                let reversed = g.spread == GradientSpread::Reflect && (k0.saturating_add(cyc)).rem_euclid(2) == 1;
                for i in 0..pieces_per_cycle {
                    let (sa, sb) = (stops[i], stops[i.saturating_add(1)]);
                    let (from_off, to_off, c_from, c_to, n) = if reversed {
                        // Mirrored: walk the stops backwards; the midpoint is measured from the other end.
                        let (ra, rb) = (stops[pieces_per_cycle.saturating_sub(i)], stops[pieces_per_cycle.saturating_sub(i).saturating_sub(1)]);
                        (1.0 - ra.offset, 1.0 - rb.offset, value(&ra), value(&rb), exponent(rb.midpoint.map(|m| 1.0 - m)))
                    } else {
                        (sa.offset, sb.offset, value(&sa), value(&sb), exponent(sa.midpoint))
                    };
                    let _ = from_off;
                    functions.push(piece(c_from, c_to, n));
                    let is_last = cyc.saturating_add(1) == cycles && i.saturating_add(1) == pieces_per_cycle;
                    if !is_last {
                        bounds.push(real((cyc as f64 + to_off) / span));
                    }
                }
            }
            if functions.len() == 1 {
                return functions.pop().unwrap_or(Object::Null);
            }
            let mut fd = Dict::default();
            fd.set("FunctionType", int(3));
            fd.set("Domain", Object::Array(vec![real(0.0), real(1.0)]));
            let count = functions.len();
            fd.set("Functions", Object::Array(functions));
            fd.set("Bounds", Object::Array(bounds));
            fd.set("Encode", Object::Array((0..count).flat_map(|_| [real(0.0), real(1.0)]).collect()));
            Object::Dict(fd)
        };
        let at = |k: i64| -> ([f64; 2], f64) {
            let k = k as f64;
            ([p0[0] + k * (p1[0] - p0[0]), p0[1] + k * (p1[1] - p0[1])], r0 + k * (r1 - r0))
        };
        let ((q0, qr0), (q1, qr1)) = (at(k0), at(k1));
        let coords: Vec<f64> = if kind == 2 { vec![q0[0], q0[1], q1[0], q1[1]] } else { vec![q0[0], q0[1], qr0.max(0.0), q1[0], q1[1], qr1.max(0.0)] };
        finite(&coords)?;
        let extend = if g.spread == GradientSpread::Pad { g.extend } else { [true, true] };
        let shading = |space: Object, function: Object| {
            let mut sd = Dict::default();
            sd.set("ShadingType", int(kind));
            sd.set("ColorSpace", space);
            sd.set("Coords", Object::Array(coords.iter().map(|v| real(*v)).collect()));
            sd.set("Extend", Object::Array(vec![Object::Bool(extend[0]), Object::Bool(extend[1])]));
            sd.set("Function", function);
            Object::Dict(sd)
        };
        let space = match g.profile {
            Some(h) => {
                let p = self.profiles.get(h.0).ok_or(ExportError::NoSuchProfile)?;
                if p.components != 3 {
                    return Err(ExportError::IccComponentsMismatch);
                }
                rf(p.space)
            }
            None => name("DeviceRGB"),
        };
        let colour_shading = shading(space, build(&|st: &GradientStop| st.colour.to_vec()));
        let sh_name = format!("Sh{}", self.shadings);
        let mut body = Vec::new();
        let mut mask_objects = None;
        if opacity {
            let gray = shading(name("DeviceGray"), build(&|st: &GradientStop| vec![st.opacity]));
            let mut mops = vec![op("q", vec![])];
            mops.extend(path_ops.iter().cloned());
            mops.push(op(if rule == FillRule::EvenOdd { "W*" } else { "W" }, vec![]));
            mops.push(op("n", vec![]));
            mops.push(op("cm", g.matrix.iter().map(|v| real(*v)).collect()));
            mops.push(op("sh", vec![name_obj("MSh0")]));
            mops.push(op("Q", vec![]));
            let content = write_ops(&mops, &[])?;
            let media = self.page(page)?.spec.media_box_pt;
            mask_objects = Some((gray, content, media));
            body.push(op("gs", vec![name_obj(&format!("GSm{}", self.shadings))]));
        }
        body.extend(path_ops);
        body.push(op(if rule == FillRule::EvenOdd { "W*" } else { "W" }, vec![]));
        body.push(op("n", vec![]));
        body.push(op("cm", g.matrix.iter().map(|v| real(*v)).collect()));
        body.push(op("sh", vec![name_obj(&sh_name)]));
        match self.top(page)?.ops.len().checked_add(body.len()).and_then(|n| n.checked_add(4)) {
            Some(n) if n <= self.limits.max_content_ops => {}
            _ => return Err(ExportError::TooManyOperations),
        }
        self.reserve(if mask_objects.is_some() { 4 } else { 1 })?;
        let mask_gs = match mask_objects {
            None => None,
            Some((gray, content, media)) => {
                let len = content.len();
                let mut fd = Dict::default();
                fd.set("Type", name("XObject"));
                fd.set("Subtype", name("Form"));
                fd.set("BBox", rect(&media));
                let mut grp = Dict::default();
                grp.set("Type", name("Group"));
                grp.set("S", name("Transparency"));
                grp.set("CS", name("DeviceGray"));
                fd.set("Group", Object::Dict(grp));
                let (stream, written) = flate_stream(fd, &content);
                self.charge(len, written)?;
                let gray_num = self.alloc();
                self.insert(gray_num, gray);
                let mut stream = stream;
                if let Object::Stream(st) = &mut stream {
                    let mut shd = Dict::default();
                    shd.set("MSh0", rf(gray_num));
                    let mut r = Dict::default();
                    r.set("Shading", Object::Dict(shd));
                    st.dict.set("Resources", Object::Dict(r));
                }
                let form = self.alloc();
                self.insert(form, stream);
                let mut gs = Dict::default();
                gs.set("Type", name("ExtGState"));
                gs.set("SMask", Object::Dict(mask_dict(form, false, 0.0, None)));
                let gnum = self.alloc();
                self.insert(gnum, Object::Dict(gs));
                Some((format!("GSm{}", self.shadings), gnum))
            }
        };
        let num = self.alloc();
        self.insert(num, colour_shading);
        self.shadings = self.shadings.saturating_add(1);
        let icc = g.profile.map(|h| h.0);
        self.draw(page, body, icc)?;
        let l = self.top_mut(page)?;
        l.shadings.push((sh_name, num));
        if let Some(gsm) = mask_gs {
            l.gstates.push(gsm);
        }
        Ok(())
    }

    /// Clips the drawings that follow on `page` to the union of glyph outlines (text rendering mode
    /// 7, ISO 32000-1 9.3.6): one text object holding every run `(font, size, [(glyph id, x, y)])`;
    /// ended by [`Self::pop_clip`].
    pub fn push_clip_text(&mut self, page: PageHandle, runs: &[TextClipRun<'_>]) -> Result<()> {
        let mut ops = vec![op("q", vec![]), op("BT", vec![]), op("Tr", vec![int(7)])];
        let mut used = Vec::new();
        for (font, size, glyphs) in runs {
            let fb = self.fonts.get(font.0).ok_or(ExportError::NoSuchFont)?;
            finite(&[*size])?;
            if *size <= 0.0 {
                return Err(ExportError::InvalidSize);
            }
            if glyphs.iter().any(|(gid, ..)| *gid >= fb.num_glyphs) {
                return Err(ExportError::FontGlyphOutOfRange);
            }
            let map = fb.gid_to_cid.clone();
            ops.push(op("Tf", vec![Object::Name(format!("F{}", font.0).into_bytes()), real(*size)]));
            for (gid, x, y) in glyphs.iter() {
                finite(&[*x, *y])?;
                let code = map.as_ref().and_then(|m| m.get(usize::from(*gid)).copied()).unwrap_or(*gid);
                ops.push(op("Tm", vec![int(1), int(0), int(0), int(1), real(*x), real(*y)]));
                ops.push(op("Tj", vec![Object::Str(code.to_be_bytes().to_vec())]));
            }
            used.push(font.0);
        }
        ops.push(op("ET", vec![]));
        if self.top(page)?.clips >= MAX_GROUP_DEPTH {
            return Err(ExportError::UnbalancedGroup);
        }
        self.push_ops(page, ops)?;
        let l = self.top_mut(page)?;
        l.clips = l.clips.saturating_add(1);
        l.fonts.extend(used);
        for (font, _, glyphs) in runs {
            if let Some(i) = self.fonts.get(font.0).and_then(|fb| fb.file)
                && let Some(file) = self.subset_files.get_mut(i)
            {
                file.used.extend(glyphs.iter().map(|(g, ..)| *g));
            }
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

    /// The image XObject and optional SMask of `image` (checked, not yet charged or inserted) with
    /// its raw and written byte counts.
    fn image_objects(&self, image: ImageSpec<'_>) -> Result<(Object, Option<Object>, usize, usize)> {
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
        Ok((stream, mask.map(|m| m.0), raw, written))
    }

    /// Draws `image` through the `cm` matrix `placement` (`[a b c d e f]`: the unit square to points).
    pub fn draw_image(&mut self, page: PageHandle, image: ImageSpec<'_>, placement: [f64; 6]) -> Result<()> {
        self.page(page)?;
        finite(&placement)?;
        let (stream, mask, raw, written) = self.image_objects(image)?;
        let res = format!("Im{}", self.images);
        let ops = vec![op("cm", placement.iter().map(|v| real(*v)).collect()), op("Do", vec![name_obj(&res)])];
        // Count the q/gs/cm/Do/Q operations before charging anything.
        match self.top(page)?.ops.len().checked_add(5) {
            Some(n) if n <= self.limits.max_content_ops => {}
            _ => return Err(ExportError::TooManyOperations),
        }
        self.reserve(if mask.is_some() { 3 } else { 2 })?;
        self.charge(raw, written)?;
        let mut stream = stream;
        if let Some(m) = mask {
            let mnum = self.alloc();
            self.insert(mnum, m);
            if let Object::Stream(s) = &mut stream {
                s.dict.set("SMask", rf(mnum));
            }
        }
        let num = self.alloc();
        self.insert(num, stream);
        self.draw(page, ops, None)?;
        self.top_mut(page)?.xobjects.push((res, num));
        self.images = self.images.saturating_add(1);
        Ok(())
    }

    /// Embeds a TrueType-outline font as Type0 / CIDFontType2 (`/Identity-H`, `/CIDToGIDMap
    /// /Identity`, the whole font as `/FontFile2`; a second font with the same bytes shares the
    /// font file). `widths` are `(glyph id, advance in 1/1000 em)` for `/W`; `to_unicode` maps glyph
    /// ids to their text for `/ToUnicode` (a later entry for the same glyph wins).
    pub fn embed_truetype(&mut self, bytes: &[u8], face_index: u32, widths: &[(u16, u16)], to_unicode: &[(u16, &str)]) -> Result<FontHandle> {
        let facts = sfnt::read(bytes, face_index)?;
        if facts.cff.is_some() {
            return Err(ExportError::FontCffNotEmbedded);
        }
        let n = facts.num_glyphs;
        if widths.iter().any(|(g, _)| *g >= n) || to_unicode.iter().any(|(g, _)| *g >= n) {
            return Err(ExportError::FontGlyphOutOfRange);
        }
        if to_unicode.iter().any(|(_, t)| t.is_empty() || t.chars().count() > MAX_GLYPH_TEXT) {
            return Err(ExportError::ToUnicodeInvalid);
        }
        let sha: [u8; 32] = Sha256::digest(bytes).into();
        let cmap = to_unicode_cmap(to_unicode);
        let (tu_stream, tu_len) = flate_stream(Dict::default(), cmap.as_bytes());
        let (file_num, subset_index) = if self.subset {
            let shared = self.subset_by_hash.get(&sha).copied();
            if shared.is_none() {
                // Structure the subsetter needs (loca/glyf) is checked now, not at finish.
                subset::closure(bytes, &BTreeSet::new())?;
                self.precheck_raw(bytes.len())?;
            }
            self.reserve(if shared.is_some() { 4 } else { 5 })?;
            let raw = cmap.len().checked_add(if shared.is_some() { 0 } else { bytes.len() }).ok_or(ExportError::AggregateOverBudget)?;
            self.charge(raw, tu_len)?;
            match shared {
                Some(i) => (self.subset_files.get(i).map_or(0, |f| f.num), Some(i)),
                None => {
                    let num = self.alloc();
                    self.subset_files.push(SubsetFile { cff: false, bytes: bytes.to_vec(), num, used: BTreeSet::new(), members: Vec::new() });
                    let i = self.subset_files.len().saturating_sub(1);
                    self.subset_by_hash.insert(sha, i);
                    (num, Some(i))
                }
            }
        } else {
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
            let written = tu_len.checked_add(font_file.1.as_ref().map_or(0, |f| f.1)).ok_or(ExportError::AggregateOverBudget)?;
            let raw = cmap.len().checked_add(if shared.is_some() { 0 } else { bytes.len() }).ok_or(ExportError::AggregateOverBudget)?;
            self.charge(raw, written)?;
            let num = match font_file {
                (num, None) => num,
                (_, Some((s, _))) => {
                    let num = self.alloc();
                    self.insert(num, s);
                    self.font_files.insert(sha, num);
                    num
                }
            };
            (num, None)
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
        if let Some(f) = subset_index.and_then(|i| self.subset_files.get_mut(i)) {
            f.used.extend(widths.iter().map(|(g, _)| *g));
            f.used.extend(to_unicode.iter().map(|(g, _)| *g));
            f.members.push((fd_num, cid_num, type0, base.clone()));
        }
        self.fonts.push(FontBuild { type0, num_glyphs: n, gid_to_cid: None, file: subset_index });
        Ok(FontHandle(self.fonts.len().saturating_sub(1)))
    }

    /// Embeds a TrueType-outline font (as [`Self::embed_truetype`]) or an OpenType-CFF font as Type0
    /// / CIDFontType0 (`/Identity-H`): a CID-keyed CFF (Top DICT `ROS`) is embedded as the bare CFF
    /// program (`/FontFile3 /Subtype /CIDFontType0C`), codes are CIDs from the CFF charset and
    /// `/W` / `/ToUnicode` are keyed by CID; a name-keyed CFF is embedded as the whole OpenType font
    /// (`/FontFile3 /Subtype /OpenType`) with codes = glyph ids (ISO 32000-2 9.7.4.2 and 9.9; wording
    /// UNVERIFIED). CID-keyed CFF programs are subset at finish (`cff::subset_cid`, glyph ids kept)
    /// when `ExportOptions::subset_fonts` is on; name-keyed OpenType-CFF is embedded whole.
    /// `widths` and `to_unicode` take glyph ids in both cases.
    pub fn embed_font(&mut self, bytes: &[u8], face_index: u32, widths: &[(u16, u16)], to_unicode: &[(u16, &str)]) -> Result<FontHandle> {
        let facts = sfnt::read(bytes, face_index)?;
        let Some((cs, ce)) = facts.cff else {
            return self.embed_truetype(bytes, face_index, widths, to_unicode);
        };
        let program = bytes.get(cs..ce).ok_or(ExportError::FontUnreadable)?;
        let cf = cff::read(program)?;
        let n = facts.num_glyphs.min(cf.num_glyphs);
        if widths.iter().any(|(g, _)| *g >= n) || to_unicode.iter().any(|(g, _)| *g >= n) {
            return Err(ExportError::FontGlyphOutOfRange);
        }
        if to_unicode.iter().any(|(_, t)| t.is_empty() || t.chars().count() > MAX_GLYPH_TEXT) {
            return Err(ExportError::ToUnicodeInvalid);
        }
        let map = cf.cid_keyed.then(|| std::rc::Rc::new(cf.gid_to_cid.clone()));
        let code = |g: u16| map.as_ref().and_then(|m| m.get(usize::from(g)).copied()).unwrap_or(g);
        let cid_widths: Vec<(u16, u16)> = widths.iter().map(|(g, w)| (code(*g), *w)).collect();
        let cid_text: Vec<(u16, &str)> = to_unicode.iter().map(|(g, t)| (code(*g), *t)).collect();
        let embedded: &[u8] = if cf.cid_keyed { program } else { bytes };
        // CID-keyed CFF programs are subset at finish (cff::subset_cid) when subsetting is on.
        let deferred = self.subset && cf.cid_keyed;
        if deferred {
            cff::subset_cid(program, &BTreeSet::new())?;
        }
        self.precheck_raw(embedded.len())?;
        self.reserve(5)?;
        let cmap = to_unicode_cmap(&cid_text);
        let (tu_stream, tu_len) = flate_stream(Dict::default(), cmap.as_bytes());
        let ff = if deferred {
            None
        } else {
            let mut ffd = Dict::default();
            ffd.set("Subtype", name(if cf.cid_keyed { "CIDFontType0C" } else { "OpenType" }));
            Some(flate_stream(ffd, embedded))
        };
        let raw = cmap.len().checked_add(embedded.len()).ok_or(ExportError::AggregateOverBudget)?;
        let written = tu_len.checked_add(ff.as_ref().map_or(0, |f| f.1)).ok_or(ExportError::AggregateOverBudget)?;
        self.charge(raw, written)?;
        let base = facts.ps_name.clone().unwrap_or_else(|| format!("HskFont-{}", &sha256_hex(bytes)[..12]));
        let scale = |v: i16| f64::from(v) * 1000.0 / f64::from(facts.units_per_em);
        let (ff_num, fd_num, cid_num, tu_num, type0) = (self.alloc(), self.alloc(), self.alloc(), self.alloc(), self.alloc());
        let mut fd = Dict::default();
        fd.set("Type", name("FontDescriptor"));
        fd.set("FontName", name(&base));
        fd.set("Flags", int(4));
        fd.set("FontBBox", Object::Array(facts.bbox.iter().map(|v| real(scale(*v))).collect()));
        fd.set("ItalicAngle", int(0));
        fd.set("Ascent", real(scale(facts.ascent)));
        fd.set("Descent", real(scale(facts.descent)));
        fd.set("CapHeight", real(scale(facts.cap_height.unwrap_or(facts.ascent))));
        fd.set("StemV", int(80));
        fd.set("FontFile3", rf(ff_num));
        let mut sys = Dict::default();
        sys.set("Registry", Object::Str(cf.registry.clone().unwrap_or_else(|| "Adobe".into()).into_bytes()));
        sys.set("Ordering", Object::Str(cf.ordering.clone().unwrap_or_else(|| "Identity".into()).into_bytes()));
        sys.set("Supplement", int(cf.supplement));
        let mut cid = Dict::default();
        cid.set("Type", name("Font"));
        cid.set("Subtype", name("CIDFontType0"));
        cid.set("BaseFont", name(&base));
        cid.set("CIDSystemInfo", Object::Dict(sys));
        cid.set("FontDescriptor", rf(fd_num));
        cid.set("W", w_array(&cid_widths));
        let mut t0 = Dict::default();
        t0.set("Type", name("Font"));
        t0.set("Subtype", name("Type0"));
        t0.set("BaseFont", name(&base));
        t0.set("Encoding", name("Identity-H"));
        t0.set("DescendantFonts", Object::Array(vec![rf(cid_num)]));
        t0.set("ToUnicode", rf(tu_num));
        let file = match ff {
            Some((ff_stream, _)) => {
                self.insert(ff_num, ff_stream);
                None
            }
            None => {
                let mut used: BTreeSet<u16> = widths.iter().map(|(g, _)| *g).collect();
                used.extend(to_unicode.iter().map(|(g, _)| *g));
                self.subset_files.push(SubsetFile { cff: true, bytes: program.to_vec(), num: ff_num, used, members: vec![(fd_num, cid_num, type0, base.clone())] });
                Some(self.subset_files.len().saturating_sub(1))
            }
        };
        self.insert(fd_num, Object::Dict(fd));
        self.insert(cid_num, Object::Dict(cid));
        self.insert(tu_num, tu_stream);
        self.insert(type0, Object::Dict(t0));
        self.fonts.push(FontBuild { type0, num_glyphs: n, gid_to_cid: map, file });
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
        if let Some(file) = f.file.and_then(|i| self.subset_files.get_mut(i)) {
            file.used.extend(glyphs.iter().map(|(g, ..)| *g));
        }
        if glyphs.is_empty() {
            return Ok(());
        }
        let map = self.fonts.get(font.0).and_then(|f| f.gid_to_cid.clone());
        let (mut body, icc) = self.colour_ops(&fill, false)?;
        body.push(op("BT", vec![]));
        body.push(op("Tf", vec![Object::Name(format!("F{}", font.0).into_bytes()), real(size_pt)]));
        for (g, x, y) in glyphs {
            let (gx, gy) = (origin_pt[0] + x, origin_pt[1] + y);
            finite(&[gx, gy])?;
            body.push(op("Tm", vec![int(1), int(0), int(0), int(1), real(gx), real(gy)]));
            let code = map.as_ref().and_then(|m| m.get(usize::from(*g)).copied()).unwrap_or(*g);
            body.push(op("Tj", vec![Object::Str(code.to_be_bytes().to_vec())]));
        }
        body.push(op("ET", vec![]));
        self.draw(page, body, icc)?;
        self.top_mut(page)?.fonts.insert(font.0);
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
            if p.levels.len() != 1 || p.levels.first().is_some_and(|l| l.clips != 0) {
                return Err(ExportError::UnbalancedGroup);
            }
            let base = p.levels.first().ok_or(ExportError::UnbalancedGroup)?;
            let content = write_ops(&base.ops, &[])?;
            let (stream, len) = flate_stream(Dict::default(), &content);
            self.charge(content.len(), len)?;
            self.insert(p.content_obj, stream);
            let res = self.resources(base)?;
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
        self.write_subsets()?;
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
            let mut intents = vec![Object::Dict(oi.clone())];
            // PDF/A-2 6.2.3: a GTS_PDFA1 intent beside GTS_PDFX, same DestOutputProfile reference.
            if self.pdfa2b && oi.get("S") == Some(&name("GTS_PDFX")) {
                let mut a = oi;
                a.set("S", name("GTS_PDFA1"));
                intents.push(Object::Dict(a));
            }
            cat.set("OutputIntents", Object::Array(intents));
        }
        if self.pdfx4.is_some() || self.pdfa2b {
            let x4 = self.pdfx4.as_ref().map(|(c, m, t, _)| (c.as_str(), m.as_str(), *t));
            let xmp = pdfx::xmp_packet(self.title.as_deref(), x4, self.pdfa2b);
            self.reserve(1)?;
            self.charge(xmp.len(), xmp.len())?;
            let mut md = Dict::default();
            md.set("Type", name("Metadata"));
            md.set("Subtype", name("XML"));
            let num = self.alloc();
            self.insert(num, Object::Stream(Box::new(Stream { dict: md, data: xmp.into_bytes() })));
            cat.set("Metadata", rf(num));
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

    /// Writes every subset font program (`subset.rs`) and prefixes the names of the fonts that use
    /// it with the subset tag (`ABCDEF+Name`, from a hash of the font and the kept glyph ids).
    fn write_subsets(&mut self) -> Result<()> {
        let files = std::mem::take(&mut self.subset_files);
        for f in &files {
            let (keep, data) = if f.cff {
                let mut keep = f.used.clone();
                keep.insert(0);
                let data = cff::subset_cid(&f.bytes, &keep)?;
                (keep, data)
            } else {
                let keep = subset::closure(&f.bytes, &f.used)?;
                let data = subset::subset(&f.bytes, &keep)?;
                (keep, data)
            };
            let mut h = Sha256::new();
            h.update(Sha256::digest(&f.bytes));
            for g in &keep {
                h.update(g.to_be_bytes());
            }
            let tag = subset::tag(&h.finalize().into());
            let mut d = Dict::default();
            if f.cff {
                d.set("Subtype", name("CIDFontType0C"));
            } else {
                d.set("Length1", int(i64::try_from(data.len()).map_err(|_| ExportError::StreamOverBudget)?));
            }
            let (stream, len) = flate_stream(d, &data);
            // Raw bytes were charged for the whole font at embedding; the written bytes now.
            self.charge(0, len)?;
            self.insert(f.num, stream);
            for (fd, cid, t0, base) in &f.members {
                let named = name(&format!("{tag}+{base}"));
                for (num, key) in [(fd, "FontName"), (cid, "BaseFont"), (t0, "BaseFont")] {
                    if let Some(Entry { obj: Object::Dict(d), .. }) = self.objects.get_mut(num) {
                        d.set(key, named.clone());
                    }
                }
            }
        }
        self.subset_files = files;
        Ok(())
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
            effective_pdfx_profile: doc
                .info_ref()
                .and_then(|r| doc.dict_of(r.num))
                .and_then(|i| match i.get("GTS_PDFXVersion").map(|o| doc.resolve(o)) {
                    Some(Object::Str(s)) => Some(text_string(s)),
                    _ => None,
                }),
            conformance: EXPORT_CONFORMANCE,
            conformance_report_sha256: None,
            conformance_validator: None,
        })
    }
}

/// A soft-mask dictionary over the transparency group `group` (ISO 32000-1 11.6.5.2).
fn mask_dict(group: u32, alpha: bool, backdrop: f64, transfer_exponent: Option<f64>) -> Dict {
    let mut sm = Dict::default();
    sm.set("Type", name("Mask"));
    sm.set("S", name(if alpha { "Alpha" } else { "Luminosity" }));
    sm.set("G", rf(group));
    if !alpha {
        sm.set("BC", Object::Array(vec![real(backdrop)]));
    }
    if let Some(n) = transfer_exponent {
        let mut f = Dict::default();
        f.set("FunctionType", int(2));
        f.set("Domain", Object::Array(vec![real(0.0), real(1.0)]));
        f.set("C0", Object::Array(vec![real(0.0)]));
        f.set("C1", Object::Array(vec![real(1.0)]));
        f.set("N", real(n));
        sm.set("TR", Object::Dict(f));
    }
    sm
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
