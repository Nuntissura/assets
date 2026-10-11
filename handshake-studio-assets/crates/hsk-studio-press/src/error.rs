//! Typed refusals and hostile-input limits. Messages are static strings and carry no document text.
#![deny(clippy::arithmetic_side_effects)]
use crate::flatten::FlattenCause;
use std::fmt;

/// Hostile-input limits. Every allocation the reader or writer makes is bounded by one of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Maximum input (and output) size in bytes.
    pub max_bytes: usize,
    /// Maximum number of in-use xref entries / loaded objects.
    pub max_objects: usize,
    /// Maximum nesting depth of arrays/dictionaries and of the page tree.
    pub max_depth: usize,
    /// Maximum decoded size of one stream.
    pub max_decoded_bytes: usize,
    /// Maximum decoded bytes summed over one operation (decompression-bomb bound).
    pub max_decoded_total: usize,
    /// Maximum number of cross-reference sections followed (`/Prev`, `/XRefStm`).
    pub max_xref_sections: usize,
    /// Maximum number of pages in the page tree.
    pub max_pages: usize,
    /// Maximum number of content-stream operations tokenised per page.
    pub max_content_ops: usize,
    /// Largest object number accepted from a cross-reference structure or written by a save
    /// (ISO 32000 Annex C, Table C.1: 8,388,607 indirect objects; text read through a secondary source).
    pub max_object_number: u32,
    /// Longest `/Filter` chain of one stream.
    pub max_filters: usize,
    /// Maximum parsed object nodes (every scalar, array element and dictionary value) over one document parse.
    pub max_nodes: usize,
    /// Resolution (dots per inch) of the raster that replaces a page carrying a redaction region
    /// (`flatten.rs`, Operator decision D27). 0 refuses every flattening.
    pub flatten_dpi: u32,
    /// Largest raster (width x height pixels) one flattened page may have; a larger page at
    /// `flatten_dpi` is a typed refusal (`PressError::FlattenRefused(FlattenCause::PixelBudget)`,
    /// checked before anything is allocated; test `flatten_pixel_budget_is_a_typed_refusal`).
    pub max_flatten_pixels: u64,
}

/// Hard ceiling for nesting depth and reference hops whatever `Limits` say: the tokenizer and the
/// writer recurse, so an enormous `max_depth` must not be able to exhaust the stack.
pub(crate) const HARD_MAX_DEPTH: usize = 256;

/// Hops followed along one chain of indirect references before it reads as null (a reference cycle
/// ends the same way); test `hostile_nesting_cycles_and_overlap`.
pub(crate) const MAX_REF_HOPS: usize = 16;

impl Limits {
    /// Nesting depth actually enforced (`max_depth` clamped to [`HARD_MAX_DEPTH`]).
    pub(crate) fn depth_cap(&self) -> usize {
        self.max_depth.min(HARD_MAX_DEPTH)
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 64 << 20,
            max_objects: 200_000,
            max_depth: 32,
            max_decoded_bytes: 64 << 20,
            max_decoded_total: 256 << 20,
            max_xref_sections: 64,
            max_pages: 10_000,
            max_content_ops: 2_000_000,
            max_object_number: 8_388_607,
            max_filters: 8,
            max_nodes: 16 << 20,
            flatten_dpi: 150,
            max_flatten_pixels: 1 << 24,
        }
    }
}

/// Content classes a region redaction cannot yet remove with proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RedactionClass {
    Image,
    Vector,
    FormXObject,
    Shading,
    InlineImage,
    /// Type 3 text anywhere on the page: its glyph advances go through `/FontMatrix` and its glyph
    /// procedures may draw outside the conservative box, which this selector does not compute, so a
    /// region cannot be proven clear (lead decision 2026-10-11T11:58:11Z, R8 F4/D4; geometry is lane
    /// SMB-PRESS-GEOMETRY; test `type3_and_vertical_text_never_read_as_cleared`).
    Type3Text,
    /// Vertical writing anywhere on the page (a composite font whose CMap is a vertical predefined
    /// CMap such as `/Identity-V` or `/V`, or an embedded CMap with `/WMode 1`, also through
    /// `/UseCMap` or `usecmap`): glyphs advance down the page, not along x (same decision; tests
    /// `type3_and_vertical_text_never_read_as_cleared`, `writing_mode_is_read_from_the_cmap`).
    VerticalText,
    /// Text on the page whose font is not determined (lead decision 2026-10-11T13:10:26Z, R9-F2): no
    /// `Tf` or ExtGState `/Font` selected one, or the name is not a font dictionary of the resources
    /// in force. Its glyph widths are unknown, so its box is unknown (test
    /// `undetermined_fonts_never_read_as_cleared`).
    UnresolvedFont,
    /// Text on the page drawn with a composite font whose CMap is neither a known horizontal nor a
    /// known vertical CMap (a predefined name outside ISO 32000-2 9.7.5.2 Table 116, an embedded CMap
    /// that cannot be read, a `/WMode` other than 0 or 1): its advance direction is unknown (R9-F1;
    /// test `unknown_cmap_names_are_unresolved_writing_modes`).
    UnresolvedWritingMode,
    /// Text on the page whose glyph advance widths are not known: an embedded simple font without
    /// `/Widths` (the widths live in the font program), or a composite font string its encoding CMap's
    /// codespace cannot split into codes (lead decision 2026-10-11T15:00:20Z, R10-F6; `font.rs`; test
    /// `composite_geometry_uses_w_dw_and_signed_size`).
    UnknownGlyphMetrics,
    /// An inline image on the page whose data holds `EI` and white space where pdf.js's look-ahead may
    /// end it early and draw the rest as operators (R11-F5): text such a viewer draws from it is unknown
    /// to the selector (test `prereview_region_pages_are_flattened`).
    AmbiguousInlineImage,
}

/// Why a text-showing operator of a content stream was not positively determined (lead decision
/// 2026-10-11T13:10:26Z): listed per content stream in `PayloadProof::undetermined_text`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TextCause {
    /// No `Tf` and no ExtGState `/Font` (through `gs`) selected a font before the operator.
    NoFont,
    /// The font name (`Tf`) or the ExtGState `/Font` entry is not a font dictionary of the resources
    /// in force (an appearance also looks in `/AcroForm /DR`).
    UnresolvedFont,
    /// A composite font whose writing mode is not known ([`RedactionClass::UnresolvedWritingMode`]).
    UnresolvedWritingMode,
    /// A code the font cannot turn into text (composite font without a usable `/ToUnicode` or with a
    /// code it does not name; simple-font code neither the encoding nor `/ToUnicode` reads).
    UnreadableCode,
}

impl TextCause {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoFont => "no_font",
            Self::UnresolvedFont => "unresolved_font",
            Self::UnresolvedWritingMode => "unresolved_writing_mode",
            Self::UnreadableCode => "unreadable_code",
        }
    }
}

impl RedactionClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Vector => "vector",
            Self::FormXObject => "form_xobject",
            Self::Shading => "shading",
            Self::InlineImage => "inline_image",
            Self::Type3Text => "type3_text",
            Self::VerticalText => "vertical_text",
            Self::UnresolvedFont => "unresolved_font",
            Self::UnresolvedWritingMode => "unresolved_writing_mode",
            Self::UnknownGlyphMetrics => "unknown_glyph_metrics",
            Self::AmbiguousInlineImage => "ambiguous_inline_image",
        }
    }
}

/// A sink-side fault (disk full, handle lost, ...). Carries no detail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SinkError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PressError {
    Canceled,
    /// A named hostile-input limit was exceeded.
    InputLimit(&'static str),
    /// Structurally invalid PDF input.
    Malformed(&'static str),
    /// `/Encrypt` present; this slice cannot decrypt, so nothing is edited, saved or redacted.
    Encrypted,
    /// `/Encrypt` present and the modify permission bit is clear.
    PermissionDenied,
    /// A byte-range signature exists; a full rewrite would invalidate it.
    SignedDocument,
    NoSuchObject,
    /// Target is not a dictionary object (streams, arrays, scalars).
    UnsupportedObject,
    /// `expected_source_sha256` differs from the opened input.
    StaleSource,
    KeyNotAllowed,
    /// A stream filter this slice does not implement (name is the PDF filter name).
    UnsupportedFilter(String),
    /// Content in the redaction region that cannot be removed with proof (fail closed).
    UnsupportedRedactionClass(RedactionClass),
    /// A removed object is still reachable from the output (internal proof failed).
    RedactionIncomplete,
    NoSuchPage,
    Sink(SinkError),
    /// A page carrying a redaction region could not be flattened (Operator decision D27): the
    /// redaction refuses; the original page is never kept instead.
    FlattenRefused(FlattenCause),
}

impl PressError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Canceled => "canceled",
            Self::InputLimit(_) => "input_limit",
            Self::Malformed(_) => "malformed",
            Self::Encrypted => "encrypted",
            Self::PermissionDenied => "permission_denied",
            Self::SignedDocument => "signed_document",
            Self::NoSuchObject => "no_such_object",
            Self::UnsupportedObject => "unsupported_object",
            Self::StaleSource => "stale_source",
            Self::KeyNotAllowed => "key_not_allowed",
            Self::UnsupportedFilter(_) => "unsupported_filter",
            Self::UnsupportedRedactionClass(_) => "unsupported_redaction_class",
            Self::RedactionIncomplete => "redaction_incomplete",
            Self::NoSuchPage => "no_such_page",
            Self::Sink(_) => "sink_fault",
            Self::FlattenRefused(_) => "flatten_refused",
        }
    }
}

impl fmt::Display for PressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputLimit(w) | Self::Malformed(w) => write!(f, "{}: {w}", self.code()),
            Self::UnsupportedFilter(n) => write!(f, "{}: {n}", self.code()),
            Self::UnsupportedRedactionClass(c) => write!(f, "{}: {}", self.code(), c.as_str()),
            Self::FlattenRefused(c) => write!(f, "{}: {}", self.code(), c.as_str()),
            _ => f.write_str(self.code()),
        }
    }
}

impl std::error::Error for PressError {}

pub(crate) type Result<T> = std::result::Result<T, PressError>;
