//! PDF export preferences (lane SMB-ID-PDF-EXPORT-PREFS-1; STU-LAY-158, STU-LAY-168, lead placement
//! for STU-LAY-159): a typed record of the six PDF export panels, the nine declared numeric bound
//! sets, the three partner-relative compression thresholds, requested vs effective read-back and the
//! typed password error. Engine only: nothing here exports, renders or writes a PDF, and no operator
//! surface exists (the tooltip half of each panel MT waits for the GUI, D7). Tagged and accessible
//! output (MT-30833 AC-004) needs the exporter and stays open.
//!
//! Numeric contracts (spec :7098-7108). Six rows are absolute: the three `*_bitmap_sampling_dpi`
//! (9..=2400 dpi, precision 0), `subset_fonts_below` (0..=100 percent, precision 0) and the two tile
//! sizes (128..=2048 px, precision 0). hard_min, hard_max, soft_min, soft_max, default, unit and
//! precision are stored separately ([`PdfParameter::bounds`]); a value the spec marks unknown is
//! `None` and never mirrored from its opposite bound (test `pdf_numeric_relative_bound_metadata`).
//!
//! Relative bounds (spec :7110-7112, STU-LAY-168 :7585). The three `threshold_to_compress_*` rows
//! are a [`PdfRelativeBounds`]: `1x .. 10x` of the named partner sampling dpi. No absolute maximum is
//! stored anywhere; a threshold is admitted only against the partner's live value, and every edit of
//! a partner re-validates its threshold atomically ([`CompressionPanel::set_sampling_dpi`] refuses
//! and keeps the old state; [`CompressionPanel::set_sampling_dpi_and_threshold`] accepts both or
//! neither). A threshold cannot be set while its partner is unset, and a partner cannot be cleared
//! while its threshold is set.
//!
//! Unit conflict (reported, not resolved; UNVERIFIED, D5): the table states unit `ratio` for the
//! thresholds while their bounds are multiples of a dpi partner. Studio STORES the threshold in dpi
//! (the stored unit, [`PdfRelativeBounds::stored_unit`]) and keeps the declared unit `ratio` beside
//! it ([`PdfRelativeBounds::declared_unit`]); [`ImageClassCompression::threshold_ratio`] reports the
//! multiple of the partner. The STU-LAY-159 stock rows state the downsample threshold as 1.5 (a
//! ratio); mapping either onto the press preset is not this lane.
//!
//! Field kinds. The tabled enumerations are typed exactly as tabled (`acrobat_compatibility` five,
//! `export_layers` three, `pdf_display_title` two, quality tiers seven, `compression_type` three,
//! `printer_mark_weight` nine, `pdf_color_space` six); `from_code` admits exact codes only.
//! `pdf_magnification` is declared ten-member but its members are unnamed, and
//! `interactive_elements_option`, `pdf_page_layout`, the per-class sampling and compression methods
//! and `pdf_mark_type` name no members: they are bounded [`PdfToken`]s whose domain completeness is
//! `undeclared` ([`PdfTokenField`]); no member is invented. `standards_compliance` is a `partial`
//! domain admitting the four STU-LAY-159 compliance tokens (`none`, `PDF/X-1a:2001`,
//! `PDF/X-3:2002`, `PDF/X-4:2007`, spec :7137-7175) as known members plus any other bounded token.
//! Every other field the spec names without a kind carries an untyped captured [`PrintValue`]
//! (shared with print preferences). Studio-declared kinds: `use_security` and the nine `disallow_*`
//! permission flags are switches (`Option<bool>`, independent of each other); the four bleeds are
//! finite numbers >= 0 in pt (as the booklet numerics); `page_range` uses the booklet grammar
//! ([`crate::parse_page_range`]) validated against [`crate::MAX_IMPOSITION_PAGES`] pages and resolved
//! against a document with [`PdfPageRange::resolve`].
//!
//! Read-back (spec :7127-7129). The four `effective_*` values are a separate read-only type
//! ([`PdfEffectiveReadBack`]): `None` until an exporter records them through
//! [`PdfExportPreferences::record_effective`]; no requested value is copied into one (test
//! `pdf_output_effective_colour_readback`).
//!
//! Security (spec :7130-7131). Reading or writing `open_document_password` or
//! `change_security_password` while `use_security` is not `Some(true)` returns
//! [`PdfExportError::PasswordSecurityOff`], never an empty string (test
//! `pdf_security_password_permission_bits`). Turning security off clears both
//! passwords (Studio reading, UNVERIFIED). [`PdfPassword`] redacts its value in `Debug`, has no
//! `Display`, and no error, observe or receipt path carries it (errors are codes; `report` emits
//! codes only; test `pdf_security_password_permission_bits_redacted_everywhere`).
//!
//! Preset reference (STU-LAY-159 lead placement). `pdf_export_preset_id` is a bounded id string
//! (`studio_declared` grammar: ASCII lower-case letters, digits, `.`, `_`, `-`); it resolves nothing,
//! because the stock catalogue is press's and layout has no press edge.
//!
//! Hostile input (V4-01/02): every text is length-checked before it is copied; refusals are typed
//! codes. InDesign PDF export behaviour is UNVERIFIED (D5, no oracle case).
#![deny(clippy::arithmetic_side_effects)]
use crate::booklet::{BookletError, MAX_IMPOSITION_PAGES, parse_page_range};
use crate::print::{MAX_PRINT_TEXT_BYTES, PrintValue};
use crate::LayoutError;

/// Bytes of a bounded token (a Studio bound for hostile input; the spec declares none).
pub const MAX_PDF_TOKEN_BYTES: usize = 128;
/// Bytes of a name (flattener preset, PDF export preset name; a Studio bound).
pub const MAX_PDF_NAME_BYTES: usize = 256;
/// Bytes of a password (a Studio bound; the spec declares none).
pub const MAX_PDF_PASSWORD_BYTES: usize = 256;
/// Bytes of a PDF export preset id (a Studio bound).
pub const MAX_PDF_PRESET_ID_BYTES: usize = 128;

/// Unit of a declared numeric parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdfUnit {
    Dpi,
    Percent,
    Px,
    Ratio,
}
impl PdfUnit {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Dpi => "dpi",
            Self::Percent => "percent",
            Self::Px => "px",
            Self::Ratio => "ratio",
        }
    }
}

/// The declared bound set of an absolute row, each field separate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdfNumericBounds {
    pub hard_min: f64,
    pub hard_max: f64,
    /// `None` = unknown in the spec.
    pub soft_min: Option<f64>,
    pub soft_max: Option<f64>,
    pub default: Option<f64>,
    pub unit: PdfUnit,
    /// Decimal places; `None` = unknown in the spec.
    pub precision: Option<u32>,
}

/// The declared bound set of a relative row: multiples of the partner's live value. No absolute
/// maximum exists in this type.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdfRelativeBounds {
    pub partner: PdfParameter,
    /// Lower bound as a multiple of the partner (1x).
    pub min_multiple: f64,
    /// Upper bound as a multiple of the partner (10x).
    pub max_multiple: f64,
    pub soft_min: Option<f64>,
    pub soft_max: Option<f64>,
    pub default: Option<f64>,
    /// The unit the spec table states (`ratio`).
    pub declared_unit: PdfUnit,
    /// The unit Studio stores (`dpi`, the partner's unit; UNVERIFIED, D5).
    pub stored_unit: PdfUnit,
    pub precision: Option<u32>,
}
impl PdfRelativeBounds {
    /// The live (min, max) in the stored unit for a partner value; nothing is stored.
    pub fn live(&self, partner_value: f64) -> (f64, f64) {
        (partner_value * self.min_multiple, partner_value * self.max_multiple)
    }
}

/// The bound set of one row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PdfBounds {
    Absolute(PdfNumericBounds),
    Relative(PdfRelativeBounds),
}

/// The nine numeric rows of the STU-LAY-158 bound table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdfParameter {
    ColorBitmapSamplingDpi,
    GrayscaleBitmapSamplingDpi,
    MonochromeBitmapSamplingDpi,
    ThresholdToCompressColor,
    ThresholdToCompressGray,
    ThresholdToCompressMonochrome,
    SubsetFontsBelow,
    ColorTileSize,
    GrayTileSize,
}
impl PdfParameter {
    pub const ALL: [Self; 9] = [
        Self::ColorBitmapSamplingDpi,
        Self::GrayscaleBitmapSamplingDpi,
        Self::MonochromeBitmapSamplingDpi,
        Self::ThresholdToCompressColor,
        Self::ThresholdToCompressGray,
        Self::ThresholdToCompressMonochrome,
        Self::SubsetFontsBelow,
        Self::ColorTileSize,
        Self::GrayTileSize,
    ];
    pub const fn name(self) -> &'static str {
        match self {
            Self::ColorBitmapSamplingDpi => "color_bitmap_sampling_dpi",
            Self::GrayscaleBitmapSamplingDpi => "grayscale_bitmap_sampling_dpi",
            Self::MonochromeBitmapSamplingDpi => "monochrome_bitmap_sampling_dpi",
            Self::ThresholdToCompressColor => "threshold_to_compress_color",
            Self::ThresholdToCompressGray => "threshold_to_compress_gray",
            Self::ThresholdToCompressMonochrome => "threshold_to_compress_monochrome",
            Self::SubsetFontsBelow => "subset_fonts_below",
            Self::ColorTileSize => "color_tile_size",
            Self::GrayTileSize => "gray_tile_size",
        }
    }
    /// The bound set exactly as tabled; unknown fields are `None`.
    pub const fn bounds(self) -> PdfBounds {
        const fn abs(hard_min: f64, hard_max: f64, unit: PdfUnit) -> PdfBounds {
            PdfBounds::Absolute(PdfNumericBounds {
                hard_min,
                hard_max,
                soft_min: None,
                soft_max: None,
                default: None,
                unit,
                precision: Some(0),
            })
        }
        const fn rel(partner: PdfParameter) -> PdfBounds {
            PdfBounds::Relative(PdfRelativeBounds {
                partner,
                min_multiple: 1.0,
                max_multiple: 10.0,
                soft_min: None,
                soft_max: None,
                default: None,
                declared_unit: PdfUnit::Ratio,
                stored_unit: PdfUnit::Dpi,
                precision: None,
            })
        }
        match self {
            Self::ColorBitmapSamplingDpi | Self::GrayscaleBitmapSamplingDpi | Self::MonochromeBitmapSamplingDpi => {
                abs(9.0, 2400.0, PdfUnit::Dpi)
            }
            Self::ThresholdToCompressColor => rel(Self::ColorBitmapSamplingDpi),
            Self::ThresholdToCompressGray => rel(Self::GrayscaleBitmapSamplingDpi),
            Self::ThresholdToCompressMonochrome => rel(Self::MonochromeBitmapSamplingDpi),
            Self::SubsetFontsBelow => abs(0.0, 100.0, PdfUnit::Percent),
            Self::ColorTileSize | Self::GrayTileSize => abs(128.0, 2048.0, PdfUnit::Px),
        }
    }
    /// Admit `value` for an ABSOLUTE row: finite, inside the hard bounds (inclusive), within the
    /// declared precision. A relative row is refused here (it needs its partner's live value).
    pub fn check(self, value: f64) -> Result<f64, PdfExportError> {
        let PdfBounds::Absolute(b) = self.bounds() else {
            return Err(PdfExportError::ThresholdPartnerUnset(self));
        };
        if !value.is_finite() {
            return Err(PdfExportError::ParameterNotFinite(self));
        }
        if value < b.hard_min || value > b.hard_max {
            return Err(PdfExportError::ParameterOutOfBounds(self));
        }
        if let Some(places) = b.precision {
            let scale = 10f64.powi(i32::try_from(places).unwrap_or(i32::MAX));
            if (value * scale).fract() != 0.0 {
                return Err(PdfExportError::ParameterPrecision(self));
            }
        }
        Ok(value)
    }
    /// Admit `threshold` for a RELATIVE row against the partner's live value.
    pub fn check_relative(self, threshold: f64, partner_value: Option<f64>) -> Result<f64, PdfExportError> {
        let PdfBounds::Relative(b) = self.bounds() else {
            return self.check(threshold);
        };
        if !threshold.is_finite() {
            return Err(PdfExportError::ParameterNotFinite(self));
        }
        let partner_value = partner_value.ok_or(PdfExportError::ThresholdPartnerUnset(self))?;
        let (lo, hi) = b.live(partner_value);
        if threshold < lo || threshold > hi {
            return Err(PdfExportError::ThresholdOutOfRelativeBounds(self));
        }
        Ok(threshold)
    }
}

macro_rules! pdf_enum {
    ($(#[$m:meta])* $name:ident { $($variant:ident => $code:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum $name { $($variant),+ }
        impl $name {
            pub const ALL: &[Self] = &[$(Self::$variant),+];
            pub const fn code(self) -> &'static str {
                match self { $(Self::$variant => $code),+ }
            }
            /// Exact member code only: no case folding, trimming or aliasing.
            pub fn from_code(code: &str) -> Result<Self, PdfExportError> {
                match code {
                    $($code => Ok(Self::$variant),)+
                    _ => Err(PdfExportError::UnknownMember(stringify!($name))),
                }
            }
        }
    };
}

pdf_enum!(
    /// `acrobat_compatibility` (General).
    AcrobatCompatibility {
        Acrobat4 => "acrobat_4",
        Acrobat5 => "acrobat_5",
        Acrobat6 => "acrobat_6",
        Acrobat7 => "acrobat_7",
        Acrobat8 => "acrobat_8",
    }
);
pdf_enum!(
    /// `export_layers` (General).
    ExportLayers {
        All => "layers.all",
        Visible => "layers.visible",
        VisiblePrintable => "layers.visible_printable",
    }
);
pdf_enum!(
    /// `pdf_display_title` (General).
    PdfDisplayTitle {
        FileName => "display.file_name",
        DocumentTitle => "display.document_title",
    }
);
pdf_enum!(
    /// The per-class quality tier (Compression).
    CompressionQuality {
        Minimum => "compression_quality.minimum",
        Low => "compression_quality.low",
        Medium => "compression_quality.medium",
        High => "compression_quality.high",
        Maximum => "compression_quality.maximum",
        FourBit => "compression_quality.four_bit",
        EightBit => "compression_quality.eight_bit",
    }
);
pdf_enum!(
    /// `compression_type` (Compression).
    CompressionType {
        NoCompression => "compress.none",
        Structure => "compress.structure",
        Objects => "compress.objects",
    }
);
pdf_enum!(
    /// `pdf_color_space` (Output).
    PdfColorSpace {
        Rgb => "pdf_color.rgb",
        Cmyk => "pdf_color.cmyk",
        Unchanged => "pdf_color.unchanged",
        RepurposeRgb => "pdf_color.repurpose_rgb",
        RepurposeCmyk => "pdf_color.repurpose_cmyk",
        Gray => "pdf_color.gray",
    }
);
pdf_enum!(
    /// `printer_mark_weight` (Marks and bleeds): nine fixed weights, each kept in its own unit.
    PrinterMarkWeight {
        Pt0125 => "0.125pt",
        Pt025 => "0.25pt",
        Pt050 => "0.50pt",
        Mm005 => "0.05mm",
        Mm007 => "0.07mm",
        Mm010 => "0.10mm",
        Mm015 => "0.15mm",
        Mm020 => "0.20mm",
        Mm030 => "0.30mm",
    }
);

/// Unit a printer-mark weight is declared in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkWeightUnit {
    Pt,
    Mm,
}
impl PrinterMarkWeight {
    /// The declared magnitude as an exact fraction (numerator, denominator) in [`Self::unit`].
    pub const fn declared(self) -> (u32, u32) {
        match self {
            Self::Pt0125 => (1, 8),
            Self::Pt025 => (1, 4),
            Self::Pt050 => (1, 2),
            Self::Mm005 => (1, 20),
            Self::Mm007 => (7, 100),
            Self::Mm010 => (1, 10),
            Self::Mm015 => (3, 20),
            Self::Mm020 => (1, 5),
            Self::Mm030 => (3, 10),
        }
    }
    pub const fn unit(self) -> MarkWeightUnit {
        match self {
            Self::Pt0125 | Self::Pt025 | Self::Pt050 => MarkWeightUnit::Pt,
            _ => MarkWeightUnit::Mm,
        }
    }
    /// The exact weight in points as a fraction in lowest terms (1 mm = 72 / 25.4 pt = 360 / 127 pt).
    pub const fn points_exact(self) -> (u32, u32) {
        match self {
            Self::Pt0125 => (1, 8),
            Self::Pt025 => (1, 4),
            Self::Pt050 => (1, 2),
            Self::Mm005 => (18, 127),
            Self::Mm007 => (126, 635),
            Self::Mm010 => (36, 127),
            Self::Mm015 => (54, 127),
            Self::Mm020 => (72, 127),
            Self::Mm030 => (108, 127),
        }
    }
    /// The weight in points (the exact fraction evaluated in f64).
    pub fn points(self) -> f64 {
        let (n, d) = self.points_exact();
        f64::from(n) / f64::from(d)
    }
}

/// How complete a token field's member domain is in the spec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DomainCompleteness {
    /// The spec names no members (or declares a count without naming them).
    Undeclared,
    /// Some members are named elsewhere in the spec; others may exist.
    Partial,
}
impl DomainCompleteness {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Undeclared => "undeclared",
            Self::Partial => "partial",
        }
    }
}

/// The token fields: enumerations whose members the spec does not (fully) name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdfTokenField {
    StandardsCompliance,
    InteractiveElementsOption,
    PdfMagnification,
    PdfPageLayout,
    ColorSamplingMethod,
    GrayscaleSamplingMethod,
    MonochromeSamplingMethod,
    ColorCompressionMethod,
    GrayscaleCompressionMethod,
    MonochromeCompressionMethod,
    PdfMarkType,
}
/// The four STU-LAY-159 compliance tokens (spec :7137-7175), the partial domain of
/// `standards_compliance`.
pub const STANDARDS_COMPLIANCE_KNOWN: [&str; 4] = ["none", "PDF/X-1a:2001", "PDF/X-3:2002", "PDF/X-4:2007"];
impl PdfTokenField {
    pub const ALL: [Self; 11] = [
        Self::StandardsCompliance,
        Self::InteractiveElementsOption,
        Self::PdfMagnification,
        Self::PdfPageLayout,
        Self::ColorSamplingMethod,
        Self::GrayscaleSamplingMethod,
        Self::MonochromeSamplingMethod,
        Self::ColorCompressionMethod,
        Self::GrayscaleCompressionMethod,
        Self::MonochromeCompressionMethod,
        Self::PdfMarkType,
    ];
    pub const fn name(self) -> &'static str {
        match self {
            Self::StandardsCompliance => "standards_compliance",
            Self::InteractiveElementsOption => "interactive_elements_option",
            Self::PdfMagnification => "pdf_magnification",
            Self::PdfPageLayout => "pdf_page_layout",
            Self::ColorSamplingMethod => "color_sampling_method",
            Self::GrayscaleSamplingMethod => "grayscale_sampling_method",
            Self::MonochromeSamplingMethod => "monochrome_sampling_method",
            Self::ColorCompressionMethod => "color_compression_method",
            Self::GrayscaleCompressionMethod => "grayscale_compression_method",
            Self::MonochromeCompressionMethod => "monochrome_compression_method",
            Self::PdfMarkType => "pdf_mark_type",
        }
    }
    pub const fn domain_completeness(self) -> DomainCompleteness {
        match self {
            Self::StandardsCompliance => DomainCompleteness::Partial,
            _ => DomainCompleteness::Undeclared,
        }
    }
    /// The member count the spec declares without naming (only `pdf_magnification`: ten).
    pub const fn declared_member_count(self) -> Option<u32> {
        match self {
            Self::PdfMagnification => Some(10),
            _ => None,
        }
    }
    /// Members the spec names for this field (empty for an undeclared domain).
    pub const fn known_members(self) -> &'static [&'static str] {
        match self {
            Self::StandardsCompliance => &STANDARDS_COMPLIANCE_KNOWN,
            _ => &[],
        }
    }
}

fn bounded_copy(text: &str, max: usize, too_long: PdfExportError, empty: PdfExportError) -> Result<String, PdfExportError> {
    if text.len() > max {
        return Err(too_long);
    }
    if text.trim().is_empty() {
        return Err(empty);
    }
    Ok(text.to_owned())
}

/// A bounded token of a field whose domain is undeclared or partial: 1..=[`MAX_PDF_TOKEN_BYTES`]
/// bytes, no control characters, not only white space; kept exactly as given (no case folding).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PdfToken(String);
impl PdfToken {
    pub fn new(token: &str) -> Result<Self, PdfExportError> {
        let t = bounded_copy(token, MAX_PDF_TOKEN_BYTES, PdfExportError::TokenTooLong, PdfExportError::TokenEmpty)?;
        if t.chars().any(char::is_control) {
            return Err(PdfExportError::TokenInvalid);
        }
        Ok(Self(t))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// Whether this token is a member the spec names for `field` (false for undeclared domains).
    pub fn is_known_member_of(&self, field: PdfTokenField) -> bool {
        field.known_members().contains(&self.0.as_str())
    }
}

/// A bounded name (`applied_flattener_preset`, a PDF export preset name): non-empty after trimming,
/// at most [`MAX_PDF_NAME_BYTES`] bytes, no control characters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PdfName(String);
impl PdfName {
    pub fn new(name: &str) -> Result<Self, PdfExportError> {
        let n = bounded_copy(name, MAX_PDF_NAME_BYTES, PdfExportError::NameTooLong, PdfExportError::NameEmpty)?;
        if n.chars().any(char::is_control) {
            return Err(PdfExportError::NameInvalid);
        }
        Ok(Self(n))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A PDF export preset id (`studio_declared` grammar: 1..=[`MAX_PDF_PRESET_ID_BYTES`] bytes of
/// ASCII lower-case letters, digits, `.`, `_`, `-`). It resolves nothing in layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PdfPresetId(String);
impl PdfPresetId {
    pub fn new(id: &str) -> Result<Self, PdfExportError> {
        if id.len() > MAX_PDF_PRESET_ID_BYTES {
            return Err(PdfExportError::PresetIdTooLong);
        }
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')) {
            return Err(PdfExportError::PresetIdInvalid);
        }
        Ok(Self(id.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A password value. `Debug` prints no value and there is no `Display`; [`Self::expose`] is the only
/// way to read it (for an exporter).
#[derive(Clone, PartialEq, Eq)]
pub struct PdfPassword(String);
impl PdfPassword {
    /// Non-empty (no password is `None`), at most [`MAX_PDF_PASSWORD_BYTES`] bytes, checked before
    /// the value is copied.
    pub fn new(password: &str) -> Result<Self, PdfExportError> {
        if password.len() > MAX_PDF_PASSWORD_BYTES {
            return Err(PdfExportError::PasswordTooLong);
        }
        if password.is_empty() {
            return Err(PdfExportError::PasswordEmpty);
        }
        Ok(Self(password.to_owned()))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for PdfPassword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PdfPassword(<redacted>)")
    }
}

/// A validated `page_range` text (booklet grammar), resolved against a document on demand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PdfPageRange(String);
impl PdfPageRange {
    /// Validate the grammar against [`MAX_IMPOSITION_PAGES`] pages (the text is length-checked first
    /// by `parse_page_range`).
    pub fn new(range: &str) -> Result<Self, PdfExportError> {
        parse_page_range(range, MAX_IMPOSITION_PAGES).map_err(PdfExportError::PageRange)?;
        Ok(Self(range.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// The selected page ordinals of a document of `document_pages` pages, in written order.
    pub fn resolve(&self, document_pages: u32) -> Result<Vec<u32>, PdfExportError> {
        parse_page_range(&self.0, document_pages).map_err(PdfExportError::PageRange)
    }
}

/// Typed refusals of PDF export records. Codes only; no text and no password is carried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdfExportError {
    ParameterNotFinite(PdfParameter),
    ParameterOutOfBounds(PdfParameter),
    ParameterPrecision(PdfParameter),
    /// A threshold set (or checked) while its partner sampling dpi is unset, or a partner cleared
    /// while its threshold is set.
    ThresholdPartnerUnset(PdfParameter),
    /// A threshold outside 1x..10x of its partner's live value.
    ThresholdOutOfRelativeBounds(PdfParameter),
    /// A partner edit that would leave its threshold outside 1x..10x (refused; old state kept).
    PartnerChangeInvalidatesThreshold(PdfParameter),
    UnknownMember(&'static str),
    TokenEmpty,
    TokenTooLong,
    TokenInvalid,
    NameEmpty,
    NameTooLong,
    NameInvalid,
    PresetIdInvalid,
    PresetIdTooLong,
    PasswordEmpty,
    PasswordTooLong,
    /// A password read or written while `use_security` is not on (spec :7130-7131).
    PasswordSecurityOff,
    /// A captured number that is NaN or infinite (field name).
    FieldNotFinite(&'static str),
    /// A captured text over [`MAX_PRINT_TEXT_BYTES`] (field name).
    TextTooLong(&'static str),
    /// A bleed that is NaN, infinite or negative (field name).
    BleedInvalid(&'static str),
    PageRange(BookletError),
}
impl PdfExportError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::ParameterNotFinite(_) => "pdf_parameter_not_finite",
            Self::ParameterOutOfBounds(_) => "pdf_parameter_out_of_bounds",
            Self::ParameterPrecision(_) => "pdf_parameter_precision",
            Self::ThresholdPartnerUnset(_) => "pdf_threshold_partner_unset",
            Self::ThresholdOutOfRelativeBounds(_) => "pdf_threshold_out_of_relative_bounds",
            Self::PartnerChangeInvalidatesThreshold(_) => "pdf_partner_change_invalidates_threshold",
            Self::UnknownMember(_) => "pdf_enum_member_unknown",
            Self::TokenEmpty => "pdf_token_empty",
            Self::TokenTooLong => "pdf_token_too_long",
            Self::TokenInvalid => "pdf_token_invalid",
            Self::NameEmpty => "pdf_name_empty",
            Self::NameTooLong => "pdf_name_too_long",
            Self::NameInvalid => "pdf_name_invalid",
            Self::PresetIdInvalid => "pdf_preset_id_invalid",
            Self::PresetIdTooLong => "pdf_preset_id_too_long",
            Self::PasswordEmpty => "pdf_password_empty",
            Self::PasswordTooLong => "pdf_password_too_long",
            Self::PasswordSecurityOff => "pdf_password_security_off",
            Self::FieldNotFinite(_) => "pdf_field_not_finite",
            Self::TextTooLong(_) => "pdf_text_too_long",
            Self::BleedInvalid(_) => "pdf_bleed_invalid",
            Self::PageRange(e) => e.code(),
        }
    }
    /// The layout refusal class for `report` (one Observe terminal outcome, codes only).
    pub const fn layout_error(self) -> LayoutError {
        match self {
            Self::TokenTooLong
            | Self::NameTooLong
            | Self::PresetIdTooLong
            | Self::PasswordTooLong
            | Self::TextTooLong(_) => LayoutError::BudgetExceeded,
            Self::PageRange(e) => e.layout_error(),
            _ => LayoutError::InvalidInput,
        }
    }
}
impl std::fmt::Display for PdfExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for PdfExportError {}

/// The six panels of spec :7120-7125.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdfPanel {
    General,
    Compression,
    MarksAndBleeds,
    Output,
    Advanced,
    Security,
}
impl PdfPanel {
    pub const ALL: [Self; 6] = [
        Self::General,
        Self::Compression,
        Self::MarksAndBleeds,
        Self::Output,
        Self::Advanced,
        Self::Security,
    ];
    pub const fn code(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Compression => "compression",
            Self::MarksAndBleeds => "marks_and_bleeds",
            Self::Output => "output",
            Self::Advanced => "advanced",
            Self::Security => "security",
        }
    }
    /// The panel's field names in spec order. Compression's per-class fields carry Studio names
    /// (`<class>_sampling_method`, `<class>_compression_method`, `<class>_compression_quality`)
    /// beside the tabled `*_bitmap_sampling_dpi` and `threshold_to_compress_*` names. The Output
    /// panel's four `effective_*` read-back fields are listed by [`PdfEffectiveField`], not here.
    pub const fn fields(self) -> &'static [&'static str] {
        match self {
            Self::General => &[
                "acrobat_compatibility", "standards_compliance", "page_range", "export_reader_spreads",
                "export_as_single_pages", "single_pages_pdf_suffix", "export_hidden_spread", "export_layers",
                "include_bookmarks", "include_hyperlinks", "include_structure", "export_nonprinting_objects",
                "export_guides_and_grids", "interactive_elements_option", "pdf_magnification", "pdf_page_layout",
                "open_in_full_screen", "pdf_display_title", "default_document_language", "generate_thumbnails",
                "optimize_pdf", "view_pdf", "preserve_editing_capabilities",
            ],
            Self::Compression => &[
                "color_sampling_method", "color_bitmap_sampling_dpi", "color_compression_method",
                "color_compression_quality", "threshold_to_compress_color",
                "grayscale_sampling_method", "grayscale_bitmap_sampling_dpi", "grayscale_compression_method",
                "grayscale_compression_quality", "threshold_to_compress_gray",
                "monochrome_sampling_method", "monochrome_bitmap_sampling_dpi", "monochrome_compression_method",
                "monochrome_compression_quality", "threshold_to_compress_monochrome",
                "compress_text_and_line_art", "crop_images_to_frames", "compression_type", "color_tile_size",
                "gray_tile_size",
            ],
            Self::MarksAndBleeds => &[
                "crop_marks", "bleed_marks", "registration_marks", "color_bars", "page_information_marks",
                "page_marks_offset", "printer_mark_weight", "pdf_mark_type", "bleed_top", "bleed_bottom",
                "bleed_inside", "bleed_outside", "use_document_bleed_with_pdf", "include_slug_with_pdf",
            ],
            Self::Output => &[
                "pdf_color_space", "pdf_destination_profile", "include_icc_profiles", "pdf_x_profile",
                "output_condition", "output_condition_name", "oc_registry", "simulate_overprint",
            ],
            Self::Advanced => &[
                "subset_fonts_below", "omit_bitmaps", "omit_eps", "omit_pdf", "applied_flattener_preset",
                "ignore_spread_overrides",
            ],
            Self::Security => &[
                "use_security", "open_document_password", "change_security_password", "disallow_printing",
                "disallow_hi_res_printing", "disallow_changing", "disallow_copying",
                "disallow_extraction_for_accessibility", "disallow_document_assembly", "disallow_notes",
                "disallow_form_fill_in", "disallow_plaintext_metadata",
            ],
        }
    }
}

fn check_value(name: &'static str, value: &Option<PrintValue>) -> Result<(), PdfExportError> {
    match value {
        Some(PrintValue::Number(n)) if !n.is_finite() => Err(PdfExportError::FieldNotFinite(name)),
        Some(PrintValue::Text(t)) if t.len() > MAX_PRINT_TEXT_BYTES => Err(PdfExportError::TextTooLong(name)),
        _ => Ok(()),
    }
}

fn check_absolute(p: PdfParameter, value: Option<f64>) -> Result<(), PdfExportError> {
    value.map_or(Ok(()), |v| p.check(v).map(|_| ()))
}

/// General panel.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GeneralPanel {
    pub acrobat_compatibility: Option<AcrobatCompatibility>,
    /// Partial domain: [`STANDARDS_COMPLIANCE_KNOWN`] plus any other bounded token.
    pub standards_compliance: Option<PdfToken>,
    pub page_range: Option<PdfPageRange>,
    pub export_reader_spreads: Option<PrintValue>,
    pub export_as_single_pages: Option<PrintValue>,
    pub single_pages_pdf_suffix: Option<PrintValue>,
    pub export_hidden_spread: Option<PrintValue>,
    pub export_layers: Option<ExportLayers>,
    pub include_bookmarks: Option<PrintValue>,
    pub include_hyperlinks: Option<PrintValue>,
    pub include_structure: Option<PrintValue>,
    pub export_nonprinting_objects: Option<PrintValue>,
    pub export_guides_and_grids: Option<PrintValue>,
    /// Undeclared domain.
    pub interactive_elements_option: Option<PdfToken>,
    /// Declared ten-member, members unnamed: undeclared domain.
    pub pdf_magnification: Option<PdfToken>,
    /// Undeclared domain.
    pub pdf_page_layout: Option<PdfToken>,
    pub open_in_full_screen: Option<PrintValue>,
    pub pdf_display_title: Option<PdfDisplayTitle>,
    pub default_document_language: Option<PrintValue>,
    pub generate_thumbnails: Option<PrintValue>,
    pub optimize_pdf: Option<PrintValue>,
    pub view_pdf: Option<PrintValue>,
    pub preserve_editing_capabilities: Option<PrintValue>,
}
impl GeneralPanel {
    fn validate(&self) -> Result<(), PdfExportError> {
        for (name, value) in [
            ("export_reader_spreads", &self.export_reader_spreads),
            ("export_as_single_pages", &self.export_as_single_pages),
            ("single_pages_pdf_suffix", &self.single_pages_pdf_suffix),
            ("export_hidden_spread", &self.export_hidden_spread),
            ("include_bookmarks", &self.include_bookmarks),
            ("include_hyperlinks", &self.include_hyperlinks),
            ("include_structure", &self.include_structure),
            ("export_nonprinting_objects", &self.export_nonprinting_objects),
            ("export_guides_and_grids", &self.export_guides_and_grids),
            ("open_in_full_screen", &self.open_in_full_screen),
            ("default_document_language", &self.default_document_language),
            ("generate_thumbnails", &self.generate_thumbnails),
            ("optimize_pdf", &self.optimize_pdf),
            ("view_pdf", &self.view_pdf),
            ("preserve_editing_capabilities", &self.preserve_editing_capabilities),
        ] {
            check_value(name, value)?;
        }
        Ok(())
    }
}

/// The three image classes of the Compression panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageClass {
    Color,
    Grayscale,
    Monochrome,
}
impl ImageClass {
    pub const ALL: [Self; 3] = [Self::Color, Self::Grayscale, Self::Monochrome];
    pub const fn sampling_parameter(self) -> PdfParameter {
        match self {
            Self::Color => PdfParameter::ColorBitmapSamplingDpi,
            Self::Grayscale => PdfParameter::GrayscaleBitmapSamplingDpi,
            Self::Monochrome => PdfParameter::MonochromeBitmapSamplingDpi,
        }
    }
    pub const fn threshold_parameter(self) -> PdfParameter {
        match self {
            Self::Color => PdfParameter::ThresholdToCompressColor,
            Self::Grayscale => PdfParameter::ThresholdToCompressGray,
            Self::Monochrome => PdfParameter::ThresholdToCompressMonochrome,
        }
    }
}

/// One image class of the Compression panel. The sampling dpi and its threshold are private: they
/// change only through [`CompressionPanel`]'s atomic setters, so a stored threshold lies in
/// 1x..10x of its stored partner (the bounds are the same for every class).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImageClassCompression {
    /// Undeclared domain.
    pub sampling_method: Option<PdfToken>,
    sampling_dpi: Option<f64>,
    /// Undeclared domain.
    pub compression_method: Option<PdfToken>,
    pub quality: Option<CompressionQuality>,
    threshold_dpi: Option<f64>,
}
impl ImageClassCompression {
    pub fn sampling_dpi(&self) -> Option<f64> {
        self.sampling_dpi
    }
    /// The threshold in the stored unit (dpi).
    pub fn threshold_dpi(&self) -> Option<f64> {
        self.threshold_dpi
    }
    /// The threshold as a multiple of the partner (the declared unit `ratio`).
    pub fn threshold_ratio(&self) -> Option<f64> {
        match (self.threshold_dpi, self.sampling_dpi) {
            (Some(t), Some(p)) => Some(t / p),
            _ => None,
        }
    }
}

/// Compression panel.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CompressionPanel {
    pub color: ImageClassCompression,
    pub grayscale: ImageClassCompression,
    pub monochrome: ImageClassCompression,
    pub compress_text_and_line_art: Option<PrintValue>,
    pub crop_images_to_frames: Option<PrintValue>,
    pub compression_type: Option<CompressionType>,
    /// px, hard 128..=2048, precision 0.
    pub color_tile_size: Option<f64>,
    /// px, hard 128..=2048, precision 0.
    pub gray_tile_size: Option<f64>,
}
impl CompressionPanel {
    pub fn class(&self, class: ImageClass) -> &ImageClassCompression {
        match class {
            ImageClass::Color => &self.color,
            ImageClass::Grayscale => &self.grayscale,
            ImageClass::Monochrome => &self.monochrome,
        }
    }
    fn class_mut(&mut self, class: ImageClass) -> &mut ImageClassCompression {
        match class {
            ImageClass::Color => &mut self.color,
            ImageClass::Grayscale => &mut self.grayscale,
            ImageClass::Monochrome => &mut self.monochrome,
        }
    }
    /// The live threshold bounds (dpi) of `class`, computed from the partner; `None` while the
    /// partner is unset. Nothing absolute is stored.
    pub fn threshold_bounds(&self, class: ImageClass) -> Option<(f64, f64)> {
        let PdfBounds::Relative(b) = class.threshold_parameter().bounds() else {
            return None;
        };
        self.class(class).sampling_dpi.map(|p| b.live(p))
    }
    /// Set (or clear) a sampling dpi, re-validating its threshold against the new value. On any
    /// refusal the panel is unchanged.
    pub fn set_sampling_dpi(&mut self, class: ImageClass, dpi: Option<f64>) -> Result<(), PdfExportError> {
        let threshold = self.class(class).threshold_dpi;
        Self::admit(class, dpi, threshold).map_err(|e| match e {
            PdfExportError::ThresholdOutOfRelativeBounds(p) | PdfExportError::ThresholdPartnerUnset(p)
                if p == class.threshold_parameter() =>
            {
                PdfExportError::PartnerChangeInvalidatesThreshold(p)
            }
            other => other,
        })?;
        self.class_mut(class).sampling_dpi = dpi;
        Ok(())
    }
    /// Set (or clear) a threshold (dpi) against the partner's live value. On a refusal the panel is
    /// unchanged.
    pub fn set_threshold(&mut self, class: ImageClass, threshold_dpi: Option<f64>) -> Result<(), PdfExportError> {
        let dpi = self.class(class).sampling_dpi;
        Self::admit(class, dpi, threshold_dpi)?;
        self.class_mut(class).threshold_dpi = threshold_dpi;
        Ok(())
    }
    /// Set a sampling dpi and its threshold together: both are stored or neither.
    pub fn set_sampling_dpi_and_threshold(
        &mut self,
        class: ImageClass,
        dpi: Option<f64>,
        threshold_dpi: Option<f64>,
    ) -> Result<(), PdfExportError> {
        Self::admit(class, dpi, threshold_dpi)?;
        let c = self.class_mut(class);
        c.sampling_dpi = dpi;
        c.threshold_dpi = threshold_dpi;
        Ok(())
    }
    fn admit(class: ImageClass, dpi: Option<f64>, threshold: Option<f64>) -> Result<(), PdfExportError> {
        check_absolute(class.sampling_parameter(), dpi)?;
        if let Some(t) = threshold {
            class.threshold_parameter().check_relative(t, dpi)?;
        }
        Ok(())
    }
    fn validate(&self) -> Result<(), PdfExportError> {
        for class in ImageClass::ALL {
            let c = self.class(class);
            Self::admit(class, c.sampling_dpi, c.threshold_dpi)?;
        }
        check_value("compress_text_and_line_art", &self.compress_text_and_line_art)?;
        check_value("crop_images_to_frames", &self.crop_images_to_frames)?;
        check_absolute(PdfParameter::ColorTileSize, self.color_tile_size)?;
        check_absolute(PdfParameter::GrayTileSize, self.gray_tile_size)
    }
}

/// Marks and bleeds panel.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MarksAndBleedsPanel {
    pub crop_marks: Option<PrintValue>,
    pub bleed_marks: Option<PrintValue>,
    pub registration_marks: Option<PrintValue>,
    pub color_bars: Option<PrintValue>,
    pub page_information_marks: Option<PrintValue>,
    pub page_marks_offset: Option<PrintValue>,
    pub printer_mark_weight: Option<PrinterMarkWeight>,
    /// Undeclared domain.
    pub pdf_mark_type: Option<PdfToken>,
    /// pt, finite >= 0 (Studio-declared admission).
    pub bleed_top: Option<f64>,
    pub bleed_bottom: Option<f64>,
    pub bleed_inside: Option<f64>,
    pub bleed_outside: Option<f64>,
    pub use_document_bleed_with_pdf: Option<PrintValue>,
    pub include_slug_with_pdf: Option<PrintValue>,
}
impl MarksAndBleedsPanel {
    fn validate(&self) -> Result<(), PdfExportError> {
        for (name, value) in [
            ("crop_marks", &self.crop_marks),
            ("bleed_marks", &self.bleed_marks),
            ("registration_marks", &self.registration_marks),
            ("color_bars", &self.color_bars),
            ("page_information_marks", &self.page_information_marks),
            ("page_marks_offset", &self.page_marks_offset),
            ("use_document_bleed_with_pdf", &self.use_document_bleed_with_pdf),
            ("include_slug_with_pdf", &self.include_slug_with_pdf),
        ] {
            check_value(name, value)?;
        }
        for (name, bleed) in [
            ("bleed_top", self.bleed_top),
            ("bleed_bottom", self.bleed_bottom),
            ("bleed_inside", self.bleed_inside),
            ("bleed_outside", self.bleed_outside),
        ] {
            if let Some(b) = bleed
                && (!b.is_finite() || b < 0.0)
            {
                return Err(PdfExportError::BleedInvalid(name));
            }
        }
        Ok(())
    }
}

/// Output panel: the requested values only. The `effective_*` read-back is [`PdfEffectiveReadBack`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OutputPanel {
    pub pdf_color_space: Option<PdfColorSpace>,
    pub pdf_destination_profile: Option<PrintValue>,
    pub include_icc_profiles: Option<PrintValue>,
    pub pdf_x_profile: Option<PrintValue>,
    pub output_condition: Option<PrintValue>,
    pub output_condition_name: Option<PrintValue>,
    pub oc_registry: Option<PrintValue>,
    pub simulate_overprint: Option<PrintValue>,
}
impl OutputPanel {
    fn validate(&self) -> Result<(), PdfExportError> {
        for (name, value) in [
            ("pdf_destination_profile", &self.pdf_destination_profile),
            ("include_icc_profiles", &self.include_icc_profiles),
            ("pdf_x_profile", &self.pdf_x_profile),
            ("output_condition", &self.output_condition),
            ("output_condition_name", &self.output_condition_name),
            ("oc_registry", &self.oc_registry),
            ("simulate_overprint", &self.simulate_overprint),
        ] {
            check_value(name, value)?;
        }
        Ok(())
    }
}

/// The four read-back fields (spec :7127-7129).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdfEffectiveField {
    DestinationProfile,
    OutputCondition,
    OcRegistry,
    PdfXProfile,
}
impl PdfEffectiveField {
    pub const ALL: [Self; 4] = [Self::DestinationProfile, Self::OutputCondition, Self::OcRegistry, Self::PdfXProfile];
    pub const fn name(self) -> &'static str {
        match self {
            Self::DestinationProfile => "effective_destination_profile",
            Self::OutputCondition => "effective_output_condition",
            Self::OcRegistry => "effective_oc_registry",
            Self::PdfXProfile => "effective_pdf_x_profile",
        }
    }
}

/// What an export actually resolved. Read-only to everyone but an exporter's
/// [`PdfExportPreferences::record_effective`]; `None` until recorded.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PdfEffectiveReadBack {
    destination_profile: Option<String>,
    output_condition: Option<String>,
    oc_registry: Option<String>,
    pdf_x_profile: Option<String>,
}
impl PdfEffectiveReadBack {
    pub fn get(&self, field: PdfEffectiveField) -> Option<&str> {
        match field {
            PdfEffectiveField::DestinationProfile => self.destination_profile.as_deref(),
            PdfEffectiveField::OutputCondition => self.output_condition.as_deref(),
            PdfEffectiveField::OcRegistry => self.oc_registry.as_deref(),
            PdfEffectiveField::PdfXProfile => self.pdf_x_profile.as_deref(),
        }
    }
    fn slot(&mut self, field: PdfEffectiveField) -> &mut Option<String> {
        match field {
            PdfEffectiveField::DestinationProfile => &mut self.destination_profile,
            PdfEffectiveField::OutputCondition => &mut self.output_condition,
            PdfEffectiveField::OcRegistry => &mut self.oc_registry,
            PdfEffectiveField::PdfXProfile => &mut self.pdf_x_profile,
        }
    }
    /// Whether nothing has been recorded.
    pub fn is_empty(&self) -> bool {
        PdfEffectiveField::ALL.iter().all(|f| self.get(*f).is_none())
    }
}

/// Advanced panel.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AdvancedPanel {
    /// percent, hard 0..=100, precision 0.
    pub subset_fonts_below: Option<f64>,
    pub omit_bitmaps: Option<PrintValue>,
    pub omit_eps: Option<PrintValue>,
    pub omit_pdf: Option<PrintValue>,
    pub applied_flattener_preset: Option<PdfName>,
    pub ignore_spread_overrides: Option<PrintValue>,
}
impl AdvancedPanel {
    fn validate(&self) -> Result<(), PdfExportError> {
        check_absolute(PdfParameter::SubsetFontsBelow, self.subset_fonts_below)?;
        for (name, value) in [
            ("omit_bitmaps", &self.omit_bitmaps),
            ("omit_eps", &self.omit_eps),
            ("omit_pdf", &self.omit_pdf),
            ("ignore_spread_overrides", &self.ignore_spread_overrides),
        ] {
            check_value(name, value)?;
        }
        Ok(())
    }
}

/// The nine permission flags of the Security panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdfPermission {
    Printing,
    HiResPrinting,
    Changing,
    Copying,
    ExtractionForAccessibility,
    DocumentAssembly,
    Notes,
    FormFillIn,
    PlaintextMetadata,
}
impl PdfPermission {
    pub const ALL: [Self; 9] = [
        Self::Printing,
        Self::HiResPrinting,
        Self::Changing,
        Self::Copying,
        Self::ExtractionForAccessibility,
        Self::DocumentAssembly,
        Self::Notes,
        Self::FormFillIn,
        Self::PlaintextMetadata,
    ];
    pub const fn name(self) -> &'static str {
        match self {
            Self::Printing => "disallow_printing",
            Self::HiResPrinting => "disallow_hi_res_printing",
            Self::Changing => "disallow_changing",
            Self::Copying => "disallow_copying",
            Self::ExtractionForAccessibility => "disallow_extraction_for_accessibility",
            Self::DocumentAssembly => "disallow_document_assembly",
            Self::Notes => "disallow_notes",
            Self::FormFillIn => "disallow_form_fill_in",
            Self::PlaintextMetadata => "disallow_plaintext_metadata",
        }
    }
}

/// Which password field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdfPasswordField {
    OpenDocument,
    ChangeSecurity,
}
impl PdfPasswordField {
    pub const fn name(self) -> &'static str {
        match self {
            Self::OpenDocument => "open_document_password",
            Self::ChangeSecurity => "change_security_password",
        }
    }
}

/// Security panel. The passwords are private: they are read and written only while security is on.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SecurityPanel {
    use_security: Option<bool>,
    open_document_password: Option<PdfPassword>,
    change_security_password: Option<PdfPassword>,
    pub disallow_printing: Option<bool>,
    pub disallow_hi_res_printing: Option<bool>,
    pub disallow_changing: Option<bool>,
    pub disallow_copying: Option<bool>,
    pub disallow_extraction_for_accessibility: Option<bool>,
    pub disallow_document_assembly: Option<bool>,
    pub disallow_notes: Option<bool>,
    pub disallow_form_fill_in: Option<bool>,
    pub disallow_plaintext_metadata: Option<bool>,
}
impl SecurityPanel {
    pub fn use_security(&self) -> Option<bool> {
        self.use_security
    }
    fn security_on(&self) -> bool {
        self.use_security == Some(true)
    }
    /// Set `use_security`; anything but `Some(true)` clears both passwords (Studio reading).
    pub fn set_use_security(&mut self, use_security: Option<bool>) {
        self.use_security = use_security;
        if !self.security_on() {
            self.open_document_password = None;
            self.change_security_password = None;
        }
    }
    /// Read a password: [`PdfExportError::PasswordSecurityOff`] while security is off (test `pdf_security_password_permission_bits`), never an
    /// empty string; `Ok(None)` when security is on and no password is set.
    pub fn password(&self, field: PdfPasswordField) -> Result<Option<&PdfPassword>, PdfExportError> {
        if !self.security_on() {
            return Err(PdfExportError::PasswordSecurityOff);
        }
        Ok(match field {
            PdfPasswordField::OpenDocument => self.open_document_password.as_ref(),
            PdfPasswordField::ChangeSecurity => self.change_security_password.as_ref(),
        })
    }
    /// Write a password; refused while security is off (nothing changes).
    pub fn set_password(&mut self, field: PdfPasswordField, password: Option<PdfPassword>) -> Result<(), PdfExportError> {
        if !self.security_on() {
            return Err(PdfExportError::PasswordSecurityOff);
        }
        match field {
            PdfPasswordField::OpenDocument => self.open_document_password = password,
            PdfPasswordField::ChangeSecurity => self.change_security_password = password,
        }
        Ok(())
    }
    pub fn permission(&self, p: PdfPermission) -> Option<bool> {
        match p {
            PdfPermission::Printing => self.disallow_printing,
            PdfPermission::HiResPrinting => self.disallow_hi_res_printing,
            PdfPermission::Changing => self.disallow_changing,
            PdfPermission::Copying => self.disallow_copying,
            PdfPermission::ExtractionForAccessibility => self.disallow_extraction_for_accessibility,
            PdfPermission::DocumentAssembly => self.disallow_document_assembly,
            PdfPermission::Notes => self.disallow_notes,
            PdfPermission::FormFillIn => self.disallow_form_fill_in,
            PdfPermission::PlaintextMetadata => self.disallow_plaintext_metadata,
        }
    }
    /// Set one permission flag; no other flag changes.
    pub fn set_permission(&mut self, p: PdfPermission, disallow: Option<bool>) {
        let slot = match p {
            PdfPermission::Printing => &mut self.disallow_printing,
            PdfPermission::HiResPrinting => &mut self.disallow_hi_res_printing,
            PdfPermission::Changing => &mut self.disallow_changing,
            PdfPermission::Copying => &mut self.disallow_copying,
            PdfPermission::ExtractionForAccessibility => &mut self.disallow_extraction_for_accessibility,
            PdfPermission::DocumentAssembly => &mut self.disallow_document_assembly,
            PdfPermission::Notes => &mut self.disallow_notes,
            PdfPermission::FormFillIn => &mut self.disallow_form_fill_in,
            PdfPermission::PlaintextMetadata => &mut self.disallow_plaintext_metadata,
        };
        *slot = disallow;
    }
}

/// The PDF export preference record: the six panels, the effective read-back and an optional preset
/// reference. `Default` is the all-unset record (the spec declares no defaults).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PdfExportPreferences {
    pub general: GeneralPanel,
    pub compression: CompressionPanel,
    pub marks_and_bleeds: MarksAndBleedsPanel,
    pub output: OutputPanel,
    pub advanced: AdvancedPanel,
    pub security: SecurityPanel,
    /// A stock or user preset by stable id; resolves nothing in layout (press owns the catalogue).
    pub pdf_export_preset_id: Option<PdfPresetId>,
    effective: PdfEffectiveReadBack,
}
impl PdfExportPreferences {
    /// The read-back of the last export recorded on this record.
    pub fn effective(&self) -> &PdfEffectiveReadBack {
        &self.effective
    }
    /// The exporter's typed setter for one read-back value (bounded before copying). Requested
    /// values are not touched (test `pdf_output_effective_colour_readback`).
    pub fn record_effective(&mut self, field: PdfEffectiveField, value: &str) -> Result<(), PdfExportError> {
        if value.len() > MAX_PRINT_TEXT_BYTES {
            return Err(PdfExportError::TextTooLong(field.name()));
        }
        *self.effective.slot(field) = Some(value.to_owned());
        Ok(())
    }
    /// The requested state only (read-back cleared), for a preset or a fresh export.
    pub fn requested_state(&self) -> Self {
        Self { effective: PdfEffectiveReadBack::default(), ..self.clone() }
    }
    /// Check every set field: absolute rows against their bounds, thresholds against their live
    /// partner, captured numbers finite, captured text bounded, bleeds finite >= 0. The first
    /// refusal in panel order is returned.
    pub fn validate(&self) -> Result<(), PdfExportError> {
        self.general.validate()?;
        self.compression.validate()?;
        self.marks_and_bleeds.validate()?;
        self.output.validate()?;
        self.advanced.validate()
    }
}

/// A named PDF export preset holding a complete requested state (no read-back).
#[derive(Clone, Debug, PartialEq)]
pub struct PdfExportPreset {
    pub name: PdfName,
    preferences: PdfExportPreferences,
}
impl PdfExportPreset {
    /// Build a preset from a complete state; the state must validate and its read-back is dropped.
    pub fn new(name: &str, preferences: &PdfExportPreferences) -> Result<Self, PdfExportError> {
        let name = PdfName::new(name)?;
        preferences.validate()?;
        Ok(Self { name, preferences: preferences.requested_state() })
    }
    pub fn preferences(&self) -> &PdfExportPreferences {
        &self.preferences
    }
    /// The complete state this preset applies (a full copy; the preset is unchanged).
    pub fn apply(&self) -> PdfExportPreferences {
        self.preferences.clone()
    }
}
