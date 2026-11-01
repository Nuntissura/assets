//! Print preferences (lane SMB-ID-BOOKLET-1; STU-LAY-059, STU-LAY-156): a typed record of the print
//! option surface, its declared numeric bounds and named printer presets. Engine only: nothing here
//! prints, renders, writes PostScript or EPS, or talks to a device (those stay open, MT-30819 AC-003).
//!
//! Field kinds. The spec declares four enumerations (`print_layers`, `print_page_orientation`,
//! `trapping`, `send_image_data`), the preset type enumeration and fifteen numeric parameters with
//! bounds; those are typed here exactly as tabled. Every other field is named by the spec without a
//! kind, so it carries an untyped captured [`PrintValue`] (bool, finite number or bounded text); no
//! kind is invented. `None` means unset: the spec declares no default for any field, so none is
//! supplied.
//!
//! Numeric parameters store hard_min, hard_max, soft_min, soft_max, default, unit and precision as
//! separate values; a value the spec marks unknown is `None` and is never mirrored from its
//! opposite bound. A set value must be finite, inside [hard_min, hard_max] and, where a precision is
//! declared, carry no more decimal places than it (bitmap_resolution: whole dpi).
//!
//! Captured typed errors (spec :7064): the spec names no field for "a half-gap value greater than the
//! crossover", so no refusal is guessed for it (UNVERIFIED, D5); ink aliasing belongs to press-write's
//! ink manager. Transparency flattener presets (STU-LAY-161) are typed records stored in
//! [`FlattenerPresets`], and `flattener_preset_name` resolves against them (see the flattener section).
//! The spec counts 109 captured properties (105 on the preset object) but names only the
//! 83 grouped fields and the 12 screen parameters carried here; the unnamed rest is not invented.
#![deny(clippy::arithmetic_side_effects)]
use crate::LayoutError;
use hsk_studio_accord::{CancellationToken, CommandContext};
use hsk_studio_folio::{
    StudioDocument, StudioTrapEnd, StudioTrapImagePlacement, StudioTrapPreset, StudioTrapPresetRecords, StudioTrapPresetRef,
};
use hsk_studio_observe::FailureCode;

/// Bytes of a captured text value (a Studio bound for hostile input; the spec declares none).
pub const MAX_PRINT_TEXT_BYTES: usize = 4096;
/// Bytes of a printer-preset name (a Studio bound for hostile input; the spec declares none).
pub const MAX_PRESET_NAME_BYTES: usize = 256;

/// Unit of a declared numeric parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrintUnit {
    Percent,
    Dpi,
    Deg,
    /// `count` in lines per inch.
    LinesPerInch,
    /// Points, the canonical layout length unit (STU-LAY-100/101); trap widths (STU-LAY-152).
    Pt,
    /// A dimensionless ratio (STU-LAY-152 `black_density`).
    Ratio,
}
impl PrintUnit {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Percent => "percent",
            Self::Dpi => "dpi",
            Self::Deg => "deg",
            Self::LinesPerInch => "count",
            Self::Pt => "pt",
            Self::Ratio => "ratio",
        }
    }
}

/// The declared bound set of one numeric parameter (spec :7036-7042), each field separate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NumericBounds {
    pub hard_min: f64,
    pub hard_max: f64,
    /// `None` = unknown in the spec.
    pub soft_min: Option<f64>,
    pub soft_max: Option<f64>,
    pub default: Option<f64>,
    pub unit: PrintUnit,
    /// Decimal places; `None` = unknown in the spec.
    pub precision: Option<u32>,
}

/// The fifteen numeric parameters of the STU-LAY-156 bound table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrintParameter {
    ScaleWidth,
    ScaleHeight,
    BitmapResolution,
    BlackAngle,
    CyanAngle,
    MagentaAngle,
    YellowAngle,
    SpotAngle,
    CompositeAngle,
    BlackFrequency,
    CyanFrequency,
    MagentaFrequency,
    YellowFrequency,
    SpotFrequency,
    CompositeFrequency,
}
impl PrintParameter {
    pub const ALL: [Self; 15] = [
        Self::ScaleWidth,
        Self::ScaleHeight,
        Self::BitmapResolution,
        Self::BlackAngle,
        Self::CyanAngle,
        Self::MagentaAngle,
        Self::YellowAngle,
        Self::SpotAngle,
        Self::CompositeAngle,
        Self::BlackFrequency,
        Self::CyanFrequency,
        Self::MagentaFrequency,
        Self::YellowFrequency,
        Self::SpotFrequency,
        Self::CompositeFrequency,
    ];
    pub const fn name(self) -> &'static str {
        match self {
            Self::ScaleWidth => "scale_width",
            Self::ScaleHeight => "scale_height",
            Self::BitmapResolution => "bitmap_resolution",
            Self::BlackAngle => "black_angle",
            Self::CyanAngle => "cyan_angle",
            Self::MagentaAngle => "magenta_angle",
            Self::YellowAngle => "yellow_angle",
            Self::SpotAngle => "spot_angle",
            Self::CompositeAngle => "composite_angle",
            Self::BlackFrequency => "black_frequency",
            Self::CyanFrequency => "cyan_frequency",
            Self::MagentaFrequency => "magenta_frequency",
            Self::YellowFrequency => "yellow_frequency",
            Self::SpotFrequency => "spot_frequency",
            Self::CompositeFrequency => "composite_frequency",
        }
    }
    /// The bound set exactly as tabled; unknown fields are `None`.
    pub const fn bounds(self) -> NumericBounds {
        let (hard_min, hard_max, unit, precision) = match self {
            Self::ScaleWidth | Self::ScaleHeight => (0.0, 1000.0, PrintUnit::Percent, None),
            Self::BitmapResolution => (72.0, 1200.0, PrintUnit::Dpi, Some(0)),
            Self::BlackAngle
            | Self::CyanAngle
            | Self::MagentaAngle
            | Self::YellowAngle
            | Self::SpotAngle
            | Self::CompositeAngle => (0.0, 360.0, PrintUnit::Deg, None),
            Self::BlackFrequency
            | Self::CyanFrequency
            | Self::MagentaFrequency
            | Self::YellowFrequency
            | Self::SpotFrequency
            | Self::CompositeFrequency => (1.0, 500.0, PrintUnit::LinesPerInch, None),
        };
        NumericBounds { hard_min, hard_max, soft_min: None, soft_max: None, default: None, unit, precision }
    }
    /// Admit `value` for this parameter: finite, inside the hard bounds (inclusive), and within the
    /// declared precision. Soft bounds and defaults are unknown and never applied.
    pub fn check(self, value: f64) -> Result<f64, PrintError> {
        if !value.is_finite() {
            return Err(PrintError::ParameterNotFinite(self));
        }
        let b = self.bounds();
        if value < b.hard_min || value > b.hard_max {
            return Err(PrintError::ParameterOutOfBounds(self));
        }
        if let Some(places) = b.precision {
            let scale = 10f64.powi(i32::try_from(places).unwrap_or(i32::MAX));
            let scaled = value * scale;
            if scaled.fract() != 0.0 {
                return Err(PrintError::ParameterPrecision(self));
            }
        }
        Ok(value)
    }
}

macro_rules! print_enum {
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
            pub fn from_code(code: &str) -> Result<Self, PrintError> {
                match code {
                    $($code => Ok(Self::$variant),)+
                    _ => Err(PrintError::UnknownMember(stringify!($name))),
                }
            }
        }
    };
}

print_enum!(
    /// `print_layers` (General).
    PrintLayers {
        All => "print_layers.all",
        Visible => "print_layers.visible",
        VisiblePrintable => "print_layers.visible_printable",
    }
);
print_enum!(
    /// `print_page_orientation` (Setup).
    PrintPageOrientation {
        Portrait => "portrait",
        Landscape => "landscape",
        ReversePortrait => "reverse_portrait",
        ReverseLandscape => "reverse_landscape",
    }
);
print_enum!(
    /// `trapping` (Output).
    Trapping {
        Off => "trapping.off",
        ApplicationBuiltin => "trapping.application_builtin",
        InRip => "trapping.in_rip",
    }
);
print_enum!(
    /// `send_image_data` (Graphics).
    SendImageData {
        All => "image_data.all",
        OptimizedSubsampling => "image_data.optimized_subsampling",
        Proxy => "image_data.proxy",
        NoImageData => "image_data.none",
    }
);
print_enum!(
    /// `printer_preset_types` of the printer-preset object.
    PresetType {
        Default => "preset_type.default",
        Custom => "preset_type.custom",
    }
);

/// A captured value of a field whose kind the spec does not declare.
#[derive(Clone, Debug, PartialEq)]
pub enum PrintValue {
    Bool(bool),
    /// Must be finite.
    Number(f64),
    /// At most [`MAX_PRINT_TEXT_BYTES`] bytes.
    Text(String),
}

/// Typed refusals of print records. Codes only; no text is carried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrintError {
    ParameterNotFinite(PrintParameter),
    ParameterOutOfBounds(PrintParameter),
    ParameterPrecision(PrintParameter),
    /// A member code that is not a member of the named enumeration.
    UnknownMember(&'static str),
    /// A captured number that is NaN or infinite (field name).
    FieldNotFinite(&'static str),
    /// A captured text over [`MAX_PRINT_TEXT_BYTES`] (field name).
    TextTooLong(&'static str),
    PresetNameEmpty,
    PresetNameTooLong,
}
impl PrintError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::ParameterNotFinite(_) => "print_parameter_not_finite",
            Self::ParameterOutOfBounds(_) => "print_parameter_out_of_bounds",
            Self::ParameterPrecision(_) => "print_parameter_precision",
            Self::UnknownMember(_) => "print_enum_member_unknown",
            Self::FieldNotFinite(_) => "print_field_not_finite",
            Self::TextTooLong(_) => "print_text_too_long",
            Self::PresetNameEmpty => "print_preset_name_empty",
            Self::PresetNameTooLong => "print_preset_name_too_long",
        }
    }
    /// The layout refusal class for `report` (one Observe terminal outcome, codes only).
    pub const fn layout_error(self) -> LayoutError {
        match self {
            Self::TextTooLong(_) | Self::PresetNameTooLong => LayoutError::BudgetExceeded,
            _ => LayoutError::InvalidInput,
        }
    }
}
impl std::fmt::Display for PrintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for PrintError {}

/// The eight option groups of spec :7048-7057.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrintGroup {
    General,
    Setup,
    MarksAndBleed,
    Output,
    Graphics,
    ColourManagement,
    Advanced,
    Device,
}
impl PrintGroup {
    pub const ALL: [Self; 8] = [
        Self::General,
        Self::Setup,
        Self::MarksAndBleed,
        Self::Output,
        Self::Graphics,
        Self::ColourManagement,
        Self::Advanced,
        Self::Device,
    ];
    pub const fn code(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Setup => "setup",
            Self::MarksAndBleed => "marks_and_bleed",
            Self::Output => "output",
            Self::Graphics => "graphics",
            Self::ColourManagement => "colour_management",
            Self::Advanced => "advanced",
            Self::Device => "device",
        }
    }
    /// The group's field names in spec order.
    pub const fn fields(self) -> &'static [&'static str] {
        match self {
            Self::General => &[
                "page_range", "print_spreads", "copies", "collating", "reverse_order", "print_layers",
                "print_parent_pages", "print_nonprinting", "print_blank_pages", "print_guides_grids",
            ],
            Self::Setup => &[
                "paper_size", "paper_width", "paper_height", "paper_offset", "paper_gap", "paper_transverse",
                "print_page_orientation", "scale_width", "scale_height", "scale_proportional", "scale_mode",
                "page_position", "thumbnails", "thumbnails_per_page", "tile", "tiling_type", "tiling_overlap",
            ],
            Self::MarksAndBleed => &[
                "all_printer_marks", "crop_marks", "bleed_marks", "registration_marks", "color_bars",
                "page_information_marks", "mark_type", "mark_offset", "mark_line_weight", "bleed_top",
                "bleed_bottom", "bleed_inside", "bleed_outside", "bleed_chain", "use_document_bleed_to_print",
                "include_slug_to_print",
            ],
            Self::Output => &[
                "color_output", "trapping", "text_as_black", "flip", "negative", "screening",
                "simulate_overprint", "print_black", "print_cyan", "print_magenta", "print_yellow",
                "separation_screening", "composite_screening", "preserve_color_numbers",
            ],
            Self::Graphics => &[
                "send_image_data", "font_downloading", "download_ppd_fonts", "postscript_level", "data_format",
                "omit_bitmaps", "omit_eps", "omit_pdf", "opi_image_replacement",
            ],
            Self::ColourManagement => &["source_space", "intent", "profile", "crd"],
            Self::Advanced => &[
                "flattener_preset_name", "ignore_spread_overrides", "bitmap_printing", "bitmap_resolution",
                "pdf_passthrough",
            ],
            Self::Device => &[
                "device_type", "printer", "print_to", "print_to_disk", "print_file", "ppd", "ppd_file",
                "print_record",
            ],
        }
    }
}

/// One field as visited for validation and catalogue checks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PrintField<'a> {
    /// An untyped captured value.
    Value(Option<&'a PrintValue>),
    /// A declared enumeration, as its member code.
    Member(Option<&'static str>),
    /// A declared numeric parameter.
    Parameter(PrintParameter, Option<f64>),
}

type Visit<'v, 'a> = &'v mut dyn FnMut(&'static str, PrintField<'a>);

fn v(value: &Option<PrintValue>) -> PrintField<'_> {
    PrintField::Value(value.as_ref())
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GeneralOptions {
    pub page_range: Option<PrintValue>,
    pub print_spreads: Option<PrintValue>,
    pub copies: Option<PrintValue>,
    pub collating: Option<PrintValue>,
    pub reverse_order: Option<PrintValue>,
    pub print_layers: Option<PrintLayers>,
    pub print_parent_pages: Option<PrintValue>,
    pub print_nonprinting: Option<PrintValue>,
    pub print_blank_pages: Option<PrintValue>,
    pub print_guides_grids: Option<PrintValue>,
}
impl GeneralOptions {
    pub fn visit<'a>(&'a self, f: Visit<'_, 'a>) {
        f("page_range", v(&self.page_range));
        f("print_spreads", v(&self.print_spreads));
        f("copies", v(&self.copies));
        f("collating", v(&self.collating));
        f("reverse_order", v(&self.reverse_order));
        f("print_layers", PrintField::Member(self.print_layers.map(PrintLayers::code)));
        f("print_parent_pages", v(&self.print_parent_pages));
        f("print_nonprinting", v(&self.print_nonprinting));
        f("print_blank_pages", v(&self.print_blank_pages));
        f("print_guides_grids", v(&self.print_guides_grids));
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SetupOptions {
    pub paper_size: Option<PrintValue>,
    pub paper_width: Option<PrintValue>,
    pub paper_height: Option<PrintValue>,
    pub paper_offset: Option<PrintValue>,
    pub paper_gap: Option<PrintValue>,
    pub paper_transverse: Option<PrintValue>,
    pub print_page_orientation: Option<PrintPageOrientation>,
    /// Percent, hard 0..=1000.
    pub scale_width: Option<f64>,
    /// Percent, hard 0..=1000.
    pub scale_height: Option<f64>,
    pub scale_proportional: Option<PrintValue>,
    pub scale_mode: Option<PrintValue>,
    pub page_position: Option<PrintValue>,
    pub thumbnails: Option<PrintValue>,
    pub thumbnails_per_page: Option<PrintValue>,
    pub tile: Option<PrintValue>,
    pub tiling_type: Option<PrintValue>,
    pub tiling_overlap: Option<PrintValue>,
}
impl SetupOptions {
    pub fn visit<'a>(&'a self, f: Visit<'_, 'a>) {
        f("paper_size", v(&self.paper_size));
        f("paper_width", v(&self.paper_width));
        f("paper_height", v(&self.paper_height));
        f("paper_offset", v(&self.paper_offset));
        f("paper_gap", v(&self.paper_gap));
        f("paper_transverse", v(&self.paper_transverse));
        f("print_page_orientation", PrintField::Member(self.print_page_orientation.map(PrintPageOrientation::code)));
        f("scale_width", PrintField::Parameter(PrintParameter::ScaleWidth, self.scale_width));
        f("scale_height", PrintField::Parameter(PrintParameter::ScaleHeight, self.scale_height));
        f("scale_proportional", v(&self.scale_proportional));
        f("scale_mode", v(&self.scale_mode));
        f("page_position", v(&self.page_position));
        f("thumbnails", v(&self.thumbnails));
        f("thumbnails_per_page", v(&self.thumbnails_per_page));
        f("tile", v(&self.tile));
        f("tiling_type", v(&self.tiling_type));
        f("tiling_overlap", v(&self.tiling_overlap));
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MarksAndBleedOptions {
    pub all_printer_marks: Option<PrintValue>,
    pub crop_marks: Option<PrintValue>,
    pub bleed_marks: Option<PrintValue>,
    pub registration_marks: Option<PrintValue>,
    pub color_bars: Option<PrintValue>,
    pub page_information_marks: Option<PrintValue>,
    pub mark_type: Option<PrintValue>,
    pub mark_offset: Option<PrintValue>,
    pub mark_line_weight: Option<PrintValue>,
    pub bleed_top: Option<PrintValue>,
    pub bleed_bottom: Option<PrintValue>,
    pub bleed_inside: Option<PrintValue>,
    pub bleed_outside: Option<PrintValue>,
    pub bleed_chain: Option<PrintValue>,
    pub use_document_bleed_to_print: Option<PrintValue>,
    pub include_slug_to_print: Option<PrintValue>,
}
impl MarksAndBleedOptions {
    pub fn visit<'a>(&'a self, f: Visit<'_, 'a>) {
        f("all_printer_marks", v(&self.all_printer_marks));
        f("crop_marks", v(&self.crop_marks));
        f("bleed_marks", v(&self.bleed_marks));
        f("registration_marks", v(&self.registration_marks));
        f("color_bars", v(&self.color_bars));
        f("page_information_marks", v(&self.page_information_marks));
        f("mark_type", v(&self.mark_type));
        f("mark_offset", v(&self.mark_offset));
        f("mark_line_weight", v(&self.mark_line_weight));
        f("bleed_top", v(&self.bleed_top));
        f("bleed_bottom", v(&self.bleed_bottom));
        f("bleed_inside", v(&self.bleed_inside));
        f("bleed_outside", v(&self.bleed_outside));
        f("bleed_chain", v(&self.bleed_chain));
        f("use_document_bleed_to_print", v(&self.use_document_bleed_to_print));
        f("include_slug_to_print", v(&self.include_slug_to_print));
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct OutputOptions {
    pub color_output: Option<PrintValue>,
    pub trapping: Option<Trapping>,
    pub text_as_black: Option<PrintValue>,
    pub flip: Option<PrintValue>,
    pub negative: Option<PrintValue>,
    pub screening: Option<PrintValue>,
    pub simulate_overprint: Option<PrintValue>,
    pub print_black: Option<PrintValue>,
    pub print_cyan: Option<PrintValue>,
    pub print_magenta: Option<PrintValue>,
    pub print_yellow: Option<PrintValue>,
    pub separation_screening: Option<PrintValue>,
    pub composite_screening: Option<PrintValue>,
    pub preserve_color_numbers: Option<PrintValue>,
}
impl OutputOptions {
    pub fn visit<'a>(&'a self, f: Visit<'_, 'a>) {
        f("color_output", v(&self.color_output));
        f("trapping", PrintField::Member(self.trapping.map(Trapping::code)));
        f("text_as_black", v(&self.text_as_black));
        f("flip", v(&self.flip));
        f("negative", v(&self.negative));
        f("screening", v(&self.screening));
        f("simulate_overprint", v(&self.simulate_overprint));
        f("print_black", v(&self.print_black));
        f("print_cyan", v(&self.print_cyan));
        f("print_magenta", v(&self.print_magenta));
        f("print_yellow", v(&self.print_yellow));
        f("separation_screening", v(&self.separation_screening));
        f("composite_screening", v(&self.composite_screening));
        f("preserve_color_numbers", v(&self.preserve_color_numbers));
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphicsOptions {
    pub send_image_data: Option<SendImageData>,
    pub font_downloading: Option<PrintValue>,
    pub download_ppd_fonts: Option<PrintValue>,
    pub postscript_level: Option<PrintValue>,
    pub data_format: Option<PrintValue>,
    pub omit_bitmaps: Option<PrintValue>,
    pub omit_eps: Option<PrintValue>,
    pub omit_pdf: Option<PrintValue>,
    pub opi_image_replacement: Option<PrintValue>,
}
impl GraphicsOptions {
    pub fn visit<'a>(&'a self, f: Visit<'_, 'a>) {
        f("send_image_data", PrintField::Member(self.send_image_data.map(SendImageData::code)));
        f("font_downloading", v(&self.font_downloading));
        f("download_ppd_fonts", v(&self.download_ppd_fonts));
        f("postscript_level", v(&self.postscript_level));
        f("data_format", v(&self.data_format));
        f("omit_bitmaps", v(&self.omit_bitmaps));
        f("omit_eps", v(&self.omit_eps));
        f("omit_pdf", v(&self.omit_pdf));
        f("opi_image_replacement", v(&self.opi_image_replacement));
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ColourManagementOptions {
    pub source_space: Option<PrintValue>,
    pub intent: Option<PrintValue>,
    pub profile: Option<PrintValue>,
    pub crd: Option<PrintValue>,
}
impl ColourManagementOptions {
    pub fn visit<'a>(&'a self, f: Visit<'_, 'a>) {
        f("source_space", v(&self.source_space));
        f("intent", v(&self.intent));
        f("profile", v(&self.profile));
        f("crd", v(&self.crd));
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AdvancedOptions {
    pub flattener_preset_name: Option<PrintValue>,
    pub ignore_spread_overrides: Option<PrintValue>,
    /// Print-as-bitmap switch (MT-30819 AC-002 field; bitmap output itself is not in this lane).
    pub bitmap_printing: Option<PrintValue>,
    /// dpi, hard 72..=1200, precision 0.
    pub bitmap_resolution: Option<f64>,
    pub pdf_passthrough: Option<PrintValue>,
}
impl AdvancedOptions {
    pub fn visit<'a>(&'a self, f: Visit<'_, 'a>) {
        f("flattener_preset_name", v(&self.flattener_preset_name));
        f("ignore_spread_overrides", v(&self.ignore_spread_overrides));
        f("bitmap_printing", v(&self.bitmap_printing));
        f("bitmap_resolution", PrintField::Parameter(PrintParameter::BitmapResolution, self.bitmap_resolution));
        f("pdf_passthrough", v(&self.pdf_passthrough));
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeviceOptions {
    pub device_type: Option<PrintValue>,
    pub printer: Option<PrintValue>,
    pub print_to: Option<PrintValue>,
    pub print_to_disk: Option<PrintValue>,
    pub print_file: Option<PrintValue>,
    pub ppd: Option<PrintValue>,
    pub ppd_file: Option<PrintValue>,
    pub print_record: Option<PrintValue>,
}
impl DeviceOptions {
    pub fn visit<'a>(&'a self, f: Visit<'_, 'a>) {
        f("device_type", v(&self.device_type));
        f("printer", v(&self.printer));
        f("print_to", v(&self.print_to));
        f("print_to_disk", v(&self.print_to_disk));
        f("print_file", v(&self.print_file));
        f("ppd", v(&self.ppd));
        f("ppd_file", v(&self.ppd_file));
        f("print_record", v(&self.print_record));
    }
}

/// Angle (deg, hard 0..=360) and frequency (lines per inch, hard 1..=500) of the six screens. The
/// spec's group table names no group for them, so they sit beside the groups.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScreenParameters {
    pub black_angle: Option<f64>,
    pub cyan_angle: Option<f64>,
    pub magenta_angle: Option<f64>,
    pub yellow_angle: Option<f64>,
    pub spot_angle: Option<f64>,
    pub composite_angle: Option<f64>,
    pub black_frequency: Option<f64>,
    pub cyan_frequency: Option<f64>,
    pub magenta_frequency: Option<f64>,
    pub yellow_frequency: Option<f64>,
    pub spot_frequency: Option<f64>,
    pub composite_frequency: Option<f64>,
}
impl ScreenParameters {
    pub fn visit<'a>(&'a self, f: Visit<'_, 'a>) {
        use PrintParameter as P;
        for (p, value) in [
            (P::BlackAngle, self.black_angle),
            (P::CyanAngle, self.cyan_angle),
            (P::MagentaAngle, self.magenta_angle),
            (P::YellowAngle, self.yellow_angle),
            (P::SpotAngle, self.spot_angle),
            (P::CompositeAngle, self.composite_angle),
            (P::BlackFrequency, self.black_frequency),
            (P::CyanFrequency, self.cyan_frequency),
            (P::MagentaFrequency, self.magenta_frequency),
            (P::YellowFrequency, self.yellow_frequency),
            (P::SpotFrequency, self.spot_frequency),
            (P::CompositeFrequency, self.composite_frequency),
        ] {
            f(p.name(), PrintField::Parameter(p, value));
        }
    }
}

/// The print-preference record: the eight groups plus the screen parameters. `Default` is the
/// all-unset record (no spec default exists to supply).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PrintPreferences {
    pub general: GeneralOptions,
    pub setup: SetupOptions,
    pub marks_and_bleed: MarksAndBleedOptions,
    pub output: OutputOptions,
    pub graphics: GraphicsOptions,
    pub colour_management: ColourManagementOptions,
    pub advanced: AdvancedOptions,
    pub device: DeviceOptions,
    pub screens: ScreenParameters,
}
impl PrintPreferences {
    /// Visit the fields of one group in spec order.
    pub fn visit_group<'a>(&'a self, group: PrintGroup, f: Visit<'_, 'a>) {
        match group {
            PrintGroup::General => self.general.visit(f),
            PrintGroup::Setup => self.setup.visit(f),
            PrintGroup::MarksAndBleed => self.marks_and_bleed.visit(f),
            PrintGroup::Output => self.output.visit(f),
            PrintGroup::Graphics => self.graphics.visit(f),
            PrintGroup::ColourManagement => self.colour_management.visit(f),
            PrintGroup::Advanced => self.advanced.visit(f),
            PrintGroup::Device => self.device.visit(f),
        }
    }
    /// Check every set field: numeric parameters against their declared bounds, captured numbers
    /// finite, captured text bounded. The first refusal in group order (then screens) is returned.
    pub fn validate(&self) -> Result<(), PrintError> {
        let mut first: Option<PrintError> = None;
        let mut check = |name: &'static str, field: PrintField<'_>| {
            if first.is_some() {
                return;
            }
            let result = match field {
                PrintField::Parameter(p, Some(value)) => p.check(value).map(|_| ()),
                PrintField::Value(Some(PrintValue::Number(n))) if !n.is_finite() => Err(PrintError::FieldNotFinite(name)),
                PrintField::Value(Some(PrintValue::Text(t))) if t.len() > MAX_PRINT_TEXT_BYTES => {
                    Err(PrintError::TextTooLong(name))
                }
                _ => Ok(()),
            };
            if let Err(e) = result {
                first = Some(e);
            }
        };
        for group in PrintGroup::ALL {
            self.visit_group(group, &mut check);
        }
        self.screens.visit(&mut check);
        first.map_or(Ok(()), Err)
    }
}

/// A named printer preset carrying a complete print state (spec :7059-7061).
#[derive(Clone, Debug, PartialEq)]
pub struct PrinterPreset {
    pub name: String,
    pub preset_type: PresetType,
    pub preferences: PrintPreferences,
}
impl PrinterPreset {
    /// Build a preset from a complete state; the name must be non-empty (after trimming white space)
    /// and at most [`MAX_PRESET_NAME_BYTES`] bytes, and the state must validate.
    pub fn new(name: &str, preset_type: PresetType, preferences: PrintPreferences) -> Result<Self, PrintError> {
        if name.len() > MAX_PRESET_NAME_BYTES {
            return Err(PrintError::PresetNameTooLong);
        }
        if name.trim().is_empty() {
            return Err(PrintError::PresetNameEmpty);
        }
        preferences.validate()?;
        Ok(Self { name: name.to_owned(), preset_type, preferences })
    }
    /// The complete state this preset applies (a full copy; the preset is unchanged).
    pub fn apply(&self) -> PrintPreferences {
        self.preferences.clone()
    }
}

// ---- Trap presets (lane SMB-ID-TOC-INDEX-1; STU-LAY-152, STU-LAY-100, STU-LAY-103) ----
//
// A trap preset is a named record of the seven tabled parameters plus the two enumerations. Each
// parameter's bound set is the same seven-field [`NumericBounds`] the print parameters use: hard and
// soft bounds separate, `None` for every field the spec marks unknown (never mirrored from the other
// range), no default supplied. `black_density` carries two captures of its hard maximum (10 and
// 12.0); per STU-LAY-103 both are recorded ([`TrapParameter::captured_hard_max`]) and the normative
// Studio hard maximum is 12.0. The two widths are stored in `pt` (canonical, STU-LAY-100); the
// per-unit ranges of the spec are conversions of the point bound ([`TrapParameter::hard_bounds_in`]),
// never stored constants. Persistence (lane SMB-ID-TOC-PERSIST-1): Folio stores presets by value in
// `hsk.studio.trap_preset_records@1` ([`TrapPreset::from_folio`] / [`TrapPreset::to_folio`],
// lossless both ways; the name is the preset's identity) and the page's `applied_trap_preset`
// (STU-LAY-107) names one; [`assign_trap_preset`] sets it on a page range under the caller's
// `CommandContext`. Vendor parity UNVERIFIED (D5).

/// Bytes of a trap-preset name: Folio's bound (one rule for the stored record and the contract).
pub use hsk_studio_folio::MAX_TRAP_PRESET_NAME_BYTES;

/// The seven numeric parameters of the STU-LAY-152 table, in table order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrapParameter {
    DefaultTrapWidth,
    BlackWidth,
    BlackColorThreshold,
    BlackDensity,
    SlidingTrapThreshold,
    StepThreshold,
    ColorReduction,
}
impl TrapParameter {
    pub const ALL: [Self; 7] = [
        Self::DefaultTrapWidth,
        Self::BlackWidth,
        Self::BlackColorThreshold,
        Self::BlackDensity,
        Self::SlidingTrapThreshold,
        Self::StepThreshold,
        Self::ColorReduction,
    ];
    pub const fn name(self) -> &'static str {
        match self {
            Self::DefaultTrapWidth => "default_trap_width",
            Self::BlackWidth => "black_width",
            Self::BlackColorThreshold => "black_color_threshold",
            Self::BlackDensity => "black_density",
            Self::SlidingTrapThreshold => "sliding_trap_threshold",
            Self::StepThreshold => "step_threshold",
            Self::ColorReduction => "color_reduction",
        }
    }
    /// The bound set as tabled, with the normative hard maximum of `black_density` (12.0, the wider
    /// capture per STU-LAY-103); unknown fields are `None`.
    pub const fn bounds(self) -> NumericBounds {
        let (hard_min, hard_max, unit, precision) = match self {
            Self::DefaultTrapWidth | Self::BlackWidth => (0.0, 8.0, PrintUnit::Pt, None),
            Self::BlackColorThreshold | Self::SlidingTrapThreshold | Self::ColorReduction => {
                (0.0, 100.0, PrintUnit::Percent, None)
            }
            Self::BlackDensity => (0.001, 12.0, PrintUnit::Ratio, Some(3)),
            Self::StepThreshold => (1.0, 100.0, PrintUnit::Percent, None),
        };
        NumericBounds { hard_min, hard_max, soft_min: None, soft_max: None, default: None, unit, precision }
    }
    /// Every captured hard maximum (STU-LAY-103: a capture conflict records both). One entry when
    /// the captures agree; `black_density` has the table's 10 and the second capture's 12.0.
    pub const fn captured_hard_max(self) -> &'static [f64] {
        match self {
            Self::DefaultTrapWidth | Self::BlackWidth => &[8.0],
            Self::BlackColorThreshold | Self::SlidingTrapThreshold | Self::StepThreshold | Self::ColorReduction => {
                &[100.0]
            }
            Self::BlackDensity => &[10.0, 12.0],
        }
    }
    /// The hard bounds of a width parameter in another measurement system, derived from the
    /// canonical point bounds (STU-LAY-100: a converter, no per-unit constants). `None` for a
    /// parameter that is not a length, or a system without a positive finite size.
    pub fn hard_bounds_in(self, system: crate::MeasureSystem) -> Option<(f64, f64)> {
        let b = self.bounds();
        if b.unit != PrintUnit::Pt {
            return None;
        }
        let per = crate::to_points(system);
        if !per.is_finite() || per <= 0.0 {
            return None;
        }
        Some((b.hard_min / per, b.hard_max / per))
    }
    /// Admit `value`: finite, inside the hard bounds (inclusive) and, where a precision is declared,
    /// with no more decimal places than it. Soft bounds and defaults are unknown and never applied.
    pub fn check(self, value: f64) -> Result<f64, TrapPresetError> {
        if !value.is_finite() {
            return Err(TrapPresetError::ParameterNotFinite(self));
        }
        let b = self.bounds();
        if value < b.hard_min || value > b.hard_max {
            return Err(TrapPresetError::ParameterOutOfBounds(self));
        }
        if let Some(places) = b.precision {
            let scaled = value * 10f64.powi(i32::try_from(places).unwrap_or(i32::MAX));
            // Tolerance for the binary form of a decimal (0.123 * 1000 is not exact).
            let tolerance = 1e-9 * scaled.abs().max(1.0);
            if (scaled - scaled.round()).abs() > tolerance {
                return Err(TrapPresetError::ParameterPrecision(self));
            }
        }
        Ok(value)
    }
}

/// STU-LAY-152 `trap_end_type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrapEndType {
    Miter,
    Overlap,
}
impl TrapEndType {
    pub const ALL: [Self; 2] = [Self::Miter, Self::Overlap];
    pub const fn code(self) -> &'static str {
        match self {
            Self::Miter => "trap_end.miter",
            Self::Overlap => "trap_end.overlap",
        }
    }
    /// Exact member code only: no case folding, trimming or aliasing.
    pub fn from_code(code: &str) -> Result<Self, TrapPresetError> {
        Self::ALL.into_iter().find(|m| m.code() == code).ok_or(TrapPresetError::UnknownMember("trap_end_type"))
    }
}

/// STU-LAY-152 `trap_image_placement`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrapImagePlacement {
    CenterEdges,
    Choke,
    ImageNeutralDensity,
    ImagesOverSpread,
}
impl TrapImagePlacement {
    pub const ALL: [Self; 4] = [Self::CenterEdges, Self::Choke, Self::ImageNeutralDensity, Self::ImagesOverSpread];
    pub const fn code(self) -> &'static str {
        match self {
            Self::CenterEdges => "trap_image.center_edges",
            Self::Choke => "trap_image.choke",
            Self::ImageNeutralDensity => "trap_image.image_neutral_density",
            Self::ImagesOverSpread => "trap_image.images_over_spread",
        }
    }
    /// Exact member code only: no case folding, trimming or aliasing.
    pub fn from_code(code: &str) -> Result<Self, TrapPresetError> {
        Self::ALL
            .into_iter()
            .find(|m| m.code() == code)
            .ok_or(TrapPresetError::UnknownMember("trap_image_placement"))
    }
}

/// Typed refusals of trap presets. Codes only; no text is carried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrapPresetError {
    ParameterNotFinite(TrapParameter),
    ParameterOutOfBounds(TrapParameter),
    ParameterPrecision(TrapParameter),
    /// A member code that is not a member of the named enumeration.
    UnknownMember(&'static str),
    NameEmpty,
    NameTooLong,
}
impl TrapPresetError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::ParameterNotFinite(_) => "trap_parameter_not_finite",
            Self::ParameterOutOfBounds(_) => "trap_parameter_out_of_bounds",
            Self::ParameterPrecision(_) => "trap_parameter_precision",
            Self::UnknownMember(_) => "trap_enum_member_unknown",
            Self::NameEmpty => "trap_preset_name_empty",
            Self::NameTooLong => "trap_preset_name_too_long",
        }
    }
    /// The layout refusal class for `report` (one Observe terminal outcome, codes only).
    pub const fn layout_error(self) -> LayoutError {
        match self {
            Self::NameTooLong => LayoutError::BudgetExceeded,
            _ => LayoutError::InvalidInput,
        }
    }
}
impl std::fmt::Display for TrapPresetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for TrapPresetError {}

/// The values of a trap preset. `None` = unset: the spec declares no default for any field, so
/// none is supplied. `Default` is the all-unset record.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrapPresetValues {
    /// `pt`, hard 0..=8.
    pub default_trap_width: Option<f64>,
    /// `pt`, hard 0..=8.
    pub black_width: Option<f64>,
    /// Percent, hard 0..=100.
    pub black_color_threshold: Option<f64>,
    /// Ratio, hard 0.001..=12.0, precision 3.
    pub black_density: Option<f64>,
    /// Percent, hard 0..=100.
    pub sliding_trap_threshold: Option<f64>,
    /// Percent, hard 1..=100.
    pub step_threshold: Option<f64>,
    /// Percent, hard 0..=100.
    pub color_reduction: Option<f64>,
    pub trap_end_type: Option<TrapEndType>,
    pub trap_image_placement: Option<TrapImagePlacement>,
}
impl TrapPresetValues {
    /// The value of one parameter.
    pub const fn get(&self, p: TrapParameter) -> Option<f64> {
        match p {
            TrapParameter::DefaultTrapWidth => self.default_trap_width,
            TrapParameter::BlackWidth => self.black_width,
            TrapParameter::BlackColorThreshold => self.black_color_threshold,
            TrapParameter::BlackDensity => self.black_density,
            TrapParameter::SlidingTrapThreshold => self.sliding_trap_threshold,
            TrapParameter::StepThreshold => self.step_threshold,
            TrapParameter::ColorReduction => self.color_reduction,
        }
    }
    /// Set one parameter after [`TrapParameter::check`]; a refused value leaves the record unchanged.
    pub fn set(&mut self, p: TrapParameter, value: f64) -> Result<(), TrapPresetError> {
        let value = p.check(value)?;
        let slot = match p {
            TrapParameter::DefaultTrapWidth => &mut self.default_trap_width,
            TrapParameter::BlackWidth => &mut self.black_width,
            TrapParameter::BlackColorThreshold => &mut self.black_color_threshold,
            TrapParameter::BlackDensity => &mut self.black_density,
            TrapParameter::SlidingTrapThreshold => &mut self.sliding_trap_threshold,
            TrapParameter::StepThreshold => &mut self.step_threshold,
            TrapParameter::ColorReduction => &mut self.color_reduction,
        };
        *slot = Some(value);
        Ok(())
    }
    /// Check every set parameter; the first refusal in table order.
    pub fn validate(&self) -> Result<(), TrapPresetError> {
        for p in TrapParameter::ALL {
            if let Some(v) = self.get(p) {
                p.check(v)?;
            }
        }
        Ok(())
    }
}

/// A named trap preset (STU-LAY-152).
#[derive(Clone, Debug, PartialEq)]
pub struct TrapPreset {
    pub name: String,
    pub values: TrapPresetValues,
}
impl TrapPreset {
    /// The name must be non-empty after trimming white space and at most
    /// [`MAX_TRAP_PRESET_NAME_BYTES`] bytes; every set value must validate.
    pub fn new(name: &str, values: TrapPresetValues) -> Result<Self, TrapPresetError> {
        if name.len() > MAX_TRAP_PRESET_NAME_BYTES {
            return Err(TrapPresetError::NameTooLong);
        }
        if name.trim().is_empty() {
            return Err(TrapPresetError::NameEmpty);
        }
        values.validate()?;
        Ok(Self { name: name.to_owned(), values })
    }
}

impl From<StudioTrapEnd> for TrapEndType {
    fn from(v: StudioTrapEnd) -> Self {
        match v {
            StudioTrapEnd::Miter => Self::Miter,
            StudioTrapEnd::Overlap => Self::Overlap,
        }
    }
}
impl From<TrapEndType> for StudioTrapEnd {
    fn from(v: TrapEndType) -> Self {
        match v {
            TrapEndType::Miter => Self::Miter,
            TrapEndType::Overlap => Self::Overlap,
        }
    }
}
impl From<StudioTrapImagePlacement> for TrapImagePlacement {
    fn from(v: StudioTrapImagePlacement) -> Self {
        match v {
            StudioTrapImagePlacement::CenterEdges => Self::CenterEdges,
            StudioTrapImagePlacement::Choke => Self::Choke,
            StudioTrapImagePlacement::ImageNeutralDensity => Self::ImageNeutralDensity,
            StudioTrapImagePlacement::ImagesOverSpread => Self::ImagesOverSpread,
        }
    }
}
impl From<TrapImagePlacement> for StudioTrapImagePlacement {
    fn from(v: TrapImagePlacement) -> Self {
        match v {
            TrapImagePlacement::CenterEdges => Self::CenterEdges,
            TrapImagePlacement::Choke => Self::Choke,
            TrapImagePlacement::ImageNeutralDensity => Self::ImageNeutralDensity,
            TrapImagePlacement::ImagesOverSpread => Self::ImagesOverSpread,
        }
    }
}

impl TrapPreset {
    /// A stored Folio preset admitted as this record: the name rule and every set value against its
    /// bound set ([`TrapPreset::new`]); unset values stay unset.
    pub fn from_folio(p: &StudioTrapPreset) -> Result<Self, TrapPresetError> {
        let values = TrapPresetValues {
            default_trap_width: p.default_trap_width,
            black_width: p.black_width,
            black_color_threshold: p.black_color_threshold,
            black_density: p.black_density,
            sliding_trap_threshold: p.sliding_trap_threshold,
            step_threshold: p.step_threshold,
            color_reduction: p.color_reduction,
            trap_end_type: p.trap_end_type.map(Into::into),
            trap_image_placement: p.trap_image_placement.map(Into::into),
        };
        Self::new(&p.name, values)
    }
    /// This record as a stored Folio preset (inverse of [`TrapPreset::from_folio`]).
    pub fn to_folio(&self) -> StudioTrapPreset {
        let v = &self.values;
        StudioTrapPreset {
            name: self.name.clone(),
            default_trap_width: v.default_trap_width,
            black_width: v.black_width,
            black_color_threshold: v.black_color_threshold,
            black_density: v.black_density,
            sliding_trap_threshold: v.sliding_trap_threshold,
            step_threshold: v.step_threshold,
            color_reduction: v.color_reduction,
            trap_end_type: v.trap_end_type.map(Into::into),
            trap_image_placement: v.trap_image_placement.map(Into::into),
        }
    }
}

/// An inclusive page range in document page order, by page key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrapPageRange<'a> {
    pub first: &'a str,
    pub last: &'a str,
}

/// Typed refusals of [`assign_trap_preset`]. Codes only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrapAssignError {
    /// The command context carries no command id.
    CommandMissing,
    StaleRevision { expected: u64, current: u64 },
    /// The preset records belong to another document.
    DocumentMismatch,
    /// The reference names no stored preset.
    PresetMissing(String),
    /// A range end names no page of the document.
    PageMissing(String),
    /// The range's last page precedes its first page.
    PageRangeOrder,
    RevisionOverflow,
    Canceled,
}
impl TrapAssignError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::CommandMissing => "command_id_missing",
            Self::StaleRevision { .. } => "stale_revision",
            Self::DocumentMismatch => "trap_document_mismatch",
            Self::PresetMissing(_) => "trap_preset_missing",
            Self::PageMissing(_) => "trap_page_missing",
            Self::PageRangeOrder => "trap_page_range_order",
            Self::RevisionOverflow => "revision_overflow",
            Self::Canceled => "canceled",
        }
    }
    /// The Observe failure class; `None` for cancellation.
    pub fn failure_code(&self) -> Option<FailureCode> {
        match self {
            Self::Canceled => None,
            Self::StaleRevision { .. } => LayoutError::StaleRevision { expected: 0, current: 0 }.failure_code(),
            _ => Some(FailureCode::Validation),
        }
    }
}
impl std::fmt::Display for TrapAssignError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for TrapAssignError {}

/// The edited document of an assignment and the pages it changed, in document order.
#[derive(Clone, Debug, PartialEq)]
pub struct TrapAssignment {
    /// The input document with every page of the range carrying `preset`, at revision base + 1.
    pub document: StudioDocument,
    pub pages: Vec<String>,
}

/// Applies a stored trap preset (or, with `None`, clears it) on every page of `range` (STU-LAY-107
/// `applied_trap_preset`; STU-LAY-152 assignment to a page range): each page carries the same
/// reference. A mutating document edit attributed to the caller's `CommandContext` (command id
/// required, base revision = document revision); the result is a new document at revision + 1 for
/// the caller to validate and commit; the input is unchanged.
pub fn assign_trap_preset(
    ctx: &CommandContext,
    document: &StudioDocument,
    presets: &StudioTrapPresetRecords,
    range: TrapPageRange<'_>,
    preset: Option<&StudioTrapPresetRef>,
    cancel: &CancellationToken,
) -> Result<TrapAssignment, TrapAssignError> {
    cancel.check().map_err(|_| TrapAssignError::Canceled)?;
    if ctx.command().is_none() {
        return Err(TrapAssignError::CommandMissing);
    }
    if ctx.base_revision() != document.revision {
        return Err(TrapAssignError::StaleRevision { expected: ctx.base_revision(), current: document.revision });
    }
    if presets.document_id != document.document_id {
        return Err(TrapAssignError::DocumentMismatch);
    }
    if let Some(p) = preset
        && presets.preset(p).is_none()
    {
        return Err(TrapAssignError::PresetMissing(p.as_str().to_owned()));
    }
    let position = |key: &str| document.page_spreads.iter().flat_map(|s| s.pages.iter()).position(|p| p.page_id == key);
    let first = position(range.first).ok_or_else(|| TrapAssignError::PageMissing(range.first.to_owned()))?;
    let last = position(range.last).ok_or_else(|| TrapAssignError::PageMissing(range.last.to_owned()))?;
    if last < first {
        return Err(TrapAssignError::PageRangeOrder);
    }
    let revision = document.revision.checked_add(1).ok_or(TrapAssignError::RevisionOverflow)?;
    let mut out = document.clone();
    out.revision = revision;
    let mut pages = Vec::new();
    for (k, page) in out.page_spreads.iter_mut().flat_map(|s| s.pages.iter_mut()).enumerate() {
        if k.is_multiple_of(256) {
            cancel.check().map_err(|_| TrapAssignError::Canceled)?;
        }
        if (first..=last).contains(&k) {
            page.applied_trap_preset = preset.cloned();
            pages.push(page.page_id.clone());
        }
    }
    Ok(TrapAssignment { document: out, pages })
}

// ---- Transparency flattener presets (lane SMB-PRESS-INK-MANAGER-1; STU-LAY-161, STU-LAY-135,
// STU-LAY-100) ----
//
// A flattener preset is a named record of the two tabled resolutions, the raster-vector balance, the
// three outline / clip switches and the five-member flattener level of STU-LAY-135. The two
// resolutions use the seven-field bound set ([`FlattenerParameter::bounds`], hard and soft bounds
// separate, `None` for every field the spec marks unknown, nothing mirrored or invented). The
// raster-vector balance has no recovered bounds (declared SPEC GAP): every one of its seven fields is
// unknown, so a stored balance is checked only for being finite and is never range-checked, clamped
// or defaulted. Stored presets live in a [`FlattenerPresets`] collection edited by attributed
// commands ([`FlattenerPresets::apply`], the caller's `CommandContext`); `flattener_preset_name`
// (print, [`AdvancedOptions::resolve_flattener_preset`]) and `applied_flattener_preset` (PDF export,
// `AdvancedPanel::resolve_flattener_preset`) resolve against it by exact name, and a name with no
// stored preset is a typed refusal. Not here: the flattener PREVIEW and flattening itself (follow-up;
// press-write's coverage stage `transparency_flattening` stays unperformed), Folio persistence of
// the collection, and any mapping from a flattener level to parameter values (none is captured).
// Vendor parity UNVERIFIED (D5).

/// Most stored flattener presets (a Studio bound for hostile input; the spec declares none).
pub const MAX_FLATTENER_PRESETS: usize = 1024;
/// Bytes of a flattener-preset name: the bound of the PDF export panel's `applied_flattener_preset`
/// name, so every storable name can be referenced.
pub const MAX_FLATTENER_PRESET_NAME_BYTES: usize = crate::pdf_export_prefs::MAX_PDF_NAME_BYTES;

/// The seven fields of one flattener parameter. Unlike [`NumericBounds`] the hard bounds and unit
/// may be unknown too (the raster-vector balance: SPEC GAP).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlattenerBounds {
    pub hard_min: Option<f64>,
    pub hard_max: Option<f64>,
    pub soft_min: Option<f64>,
    pub soft_max: Option<f64>,
    pub default: Option<f64>,
    pub unit: Option<PrintUnit>,
    pub precision: Option<u32>,
}

/// The numeric parameters of a flattener preset: the two rows of the STU-LAY-161 table and the
/// raster-vector balance the clause names without bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlattenerParameter {
    LineArtAndTextResolution,
    GradientAndMeshResolution,
    RasterVectorBalance,
}
impl FlattenerParameter {
    pub const ALL: [Self; 3] = [Self::LineArtAndTextResolution, Self::GradientAndMeshResolution, Self::RasterVectorBalance];
    pub const fn name(self) -> &'static str {
        match self {
            Self::LineArtAndTextResolution => "line_art_and_text_resolution",
            Self::GradientAndMeshResolution => "gradient_and_mesh_resolution",
            Self::RasterVectorBalance => "raster_vector_balance",
        }
    }
    /// The bound set exactly as tabled; every field the spec does not declare is `None`.
    pub const fn bounds(self) -> FlattenerBounds {
        let (hard_min, hard_max, unit) = match self {
            Self::LineArtAndTextResolution => (Some(1.0), Some(9600.0), Some(PrintUnit::Dpi)),
            Self::GradientAndMeshResolution => (Some(0.0), Some(1200.0), Some(PrintUnit::Dpi)),
            Self::RasterVectorBalance => (None, None, None),
        };
        FlattenerBounds { hard_min, hard_max, soft_min: None, soft_max: None, default: None, unit, precision: None }
    }
    /// True for a parameter whose bounds the spec declares as not recovered (SPEC GAP).
    pub const fn bounds_undeclared(self) -> bool {
        matches!(self, Self::RasterVectorBalance)
    }
    /// Admit `value`: finite and inside the declared hard bounds (inclusive). Undeclared bounds are
    /// never invented, so the balance is checked for being finite only; nothing is clamped.
    pub fn check(self, value: f64) -> Result<f64, FlattenerPresetError> {
        if !value.is_finite() {
            return Err(FlattenerPresetError::ParameterNotFinite(self));
        }
        let b = self.bounds();
        if b.hard_min.is_some_and(|lo| value < lo) || b.hard_max.is_some_and(|hi| value > hi) {
            return Err(FlattenerPresetError::ParameterOutOfBounds(self));
        }
        Ok(value)
    }
}

/// The five-member flattener level of STU-LAY-135 (`spread_flattener_override`), the one
/// enumeration for spread overrides and flattener presets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlattenerLevel {
    Low,
    MediumLow,
    Medium,
    MediumHigh,
    High,
}
impl FlattenerLevel {
    pub const ALL: [Self; 5] = [Self::Low, Self::MediumLow, Self::Medium, Self::MediumHigh, Self::High];
    pub const fn code(self) -> &'static str {
        match self {
            Self::Low => "flattener.low",
            Self::MediumLow => "flattener.medium_low",
            Self::Medium => "flattener.medium",
            Self::MediumHigh => "flattener.medium_high",
            Self::High => "flattener.high",
        }
    }
    /// Exact member code only: no case folding, trimming or aliasing.
    pub fn from_code(code: &str) -> Result<Self, FlattenerPresetError> {
        Self::ALL.into_iter().find(|m| m.code() == code).ok_or(FlattenerPresetError::UnknownMember("flattener_level"))
    }
}

/// Typed refusals of flattener presets, their store and name resolution. Codes only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlattenerPresetError {
    ParameterNotFinite(FlattenerParameter),
    ParameterOutOfBounds(FlattenerParameter),
    /// A member code that is not a member of the named enumeration.
    UnknownMember(&'static str),
    NameEmpty,
    NameTooLong,
    /// A name with a control character.
    NameInvalid,
    /// More than [`MAX_FLATTENER_PRESETS`] stored presets.
    TooManyPresets,
    /// A referenced name has no stored preset (dangling `flattener_preset_name` or
    /// `applied_flattener_preset`, or a removal of a preset that is not stored).
    PresetMissing(String),
    /// `flattener_preset_name` holds a captured value that is not text, so it names nothing.
    NameNotText,
    /// The command context carries no command id.
    CommandMissing,
    StaleRevision { expected: u64, current: u64 },
    RevisionOverflow,
    Canceled,
}
impl FlattenerPresetError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ParameterNotFinite(_) => "flattener_parameter_not_finite",
            Self::ParameterOutOfBounds(_) => "flattener_parameter_out_of_bounds",
            Self::UnknownMember(_) => "flattener_enum_member_unknown",
            Self::NameEmpty => "flattener_preset_name_empty",
            Self::NameTooLong => "flattener_preset_name_too_long",
            Self::NameInvalid => "flattener_preset_name_invalid",
            Self::TooManyPresets => "flattener_too_many_presets",
            Self::PresetMissing(_) => "flattener_preset_missing",
            Self::NameNotText => "flattener_preset_name_not_text",
            Self::CommandMissing => "command_id_missing",
            Self::StaleRevision { .. } => "stale_revision",
            Self::RevisionOverflow => "revision_overflow",
            Self::Canceled => "canceled",
        }
    }
    /// The Observe failure class; `None` for cancellation.
    pub fn failure_code(&self) -> Option<FailureCode> {
        match self {
            Self::Canceled => None,
            Self::StaleRevision { .. } => LayoutError::StaleRevision { expected: 0, current: 0 }.failure_code(),
            _ => Some(FailureCode::Validation),
        }
    }
}
impl std::fmt::Display for FlattenerPresetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for FlattenerPresetError {}

/// The values of a flattener preset. `None` = unset: the spec declares no default, so none is
/// supplied. `Default` is the all-unset record.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FlattenerPresetValues {
    /// dpi, hard 1..=9600.
    pub line_art_and_text_resolution: Option<f64>,
    /// dpi, hard 0..=1200.
    pub gradient_and_mesh_resolution: Option<f64>,
    /// Bounds not recovered (SPEC GAP): finite only.
    pub raster_vector_balance: Option<f64>,
    pub convert_all_text_to_outlines: Option<bool>,
    pub convert_all_strokes_to_outlines: Option<bool>,
    pub clip_complex_regions: Option<bool>,
    /// The STU-LAY-135 flattener level.
    pub level: Option<FlattenerLevel>,
}
impl FlattenerPresetValues {
    /// The value of one numeric parameter.
    pub const fn get(&self, p: FlattenerParameter) -> Option<f64> {
        match p {
            FlattenerParameter::LineArtAndTextResolution => self.line_art_and_text_resolution,
            FlattenerParameter::GradientAndMeshResolution => self.gradient_and_mesh_resolution,
            FlattenerParameter::RasterVectorBalance => self.raster_vector_balance,
        }
    }
    /// Set one numeric parameter after [`FlattenerParameter::check`]; a refused value leaves the
    /// record unchanged.
    pub fn set(&mut self, p: FlattenerParameter, value: f64) -> Result<(), FlattenerPresetError> {
        let value = p.check(value)?;
        let slot = match p {
            FlattenerParameter::LineArtAndTextResolution => &mut self.line_art_and_text_resolution,
            FlattenerParameter::GradientAndMeshResolution => &mut self.gradient_and_mesh_resolution,
            FlattenerParameter::RasterVectorBalance => &mut self.raster_vector_balance,
        };
        *slot = Some(value);
        Ok(())
    }
    /// Check every set numeric parameter; the first refusal in table order.
    pub fn validate(&self) -> Result<(), FlattenerPresetError> {
        for p in FlattenerParameter::ALL {
            if let Some(v) = self.get(p) {
                p.check(v)?;
            }
        }
        Ok(())
    }
}

/// A named transparency flattener preset (STU-LAY-161).
#[derive(Clone, Debug, PartialEq)]
pub struct FlattenerPreset {
    pub name: String,
    pub values: FlattenerPresetValues,
}
impl FlattenerPreset {
    /// The name must be non-empty after trimming white space, at most
    /// [`MAX_FLATTENER_PRESET_NAME_BYTES`] bytes (checked before it is copied) and free of control
    /// characters; every set value must validate.
    pub fn new(name: &str, values: FlattenerPresetValues) -> Result<Self, FlattenerPresetError> {
        check_flattener_name(name)?;
        values.validate()?;
        Ok(Self { name: name.to_owned(), values })
    }
}

fn check_flattener_name(name: &str) -> Result<(), FlattenerPresetError> {
    if name.len() > MAX_FLATTENER_PRESET_NAME_BYTES {
        return Err(FlattenerPresetError::NameTooLong);
    }
    if name.trim().is_empty() {
        return Err(FlattenerPresetError::NameEmpty);
    }
    if name.chars().any(char::is_control) {
        return Err(FlattenerPresetError::NameInvalid);
    }
    Ok(())
}

/// One mutating edit of the stored flattener presets.
#[derive(Clone, Debug, PartialEq)]
pub enum FlattenerPresetEdit {
    /// Store `preset`, replacing a stored preset of the same name.
    Put(FlattenerPreset),
    /// Remove the stored preset of this name.
    Remove(String),
}

/// The stored flattener presets of a document, by unique exact name, with their own revision.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FlattenerPresets {
    revision: u64,
    presets: Vec<FlattenerPreset>,
}
impl FlattenerPresets {
    pub fn revision(&self) -> u64 {
        self.revision
    }
    /// The stored presets in insertion order.
    pub fn presets(&self) -> &[FlattenerPreset] {
        &self.presets
    }
    /// The stored preset of exactly this name.
    pub fn get(&self, name: &str) -> Option<&FlattenerPreset> {
        self.presets.iter().find(|p| p.name == name)
    }
    /// The stored preset of exactly this name, or `flattener_preset_missing` (a dangling reference).
    pub fn resolve(&self, name: &str) -> Result<&FlattenerPreset, FlattenerPresetError> {
        self.get(name).ok_or_else(|| FlattenerPresetError::PresetMissing(name.to_owned()))
    }
    /// Applies one edit attributed to the caller's `CommandContext` (command id required, base
    /// revision = this store's revision). The result is a new store at revision + 1; the input is
    /// unchanged and a refused edit changes nothing. A put is re-validated ([`FlattenerPreset::new`]
    /// rules) so a record built by field writes cannot bypass the bounds.
    pub fn apply(&self, ctx: &CommandContext, edit: &FlattenerPresetEdit, cancel: &CancellationToken) -> Result<Self, FlattenerPresetError> {
        cancel.check().map_err(|_| FlattenerPresetError::Canceled)?;
        if ctx.command().is_none() {
            return Err(FlattenerPresetError::CommandMissing);
        }
        if ctx.base_revision() != self.revision {
            return Err(FlattenerPresetError::StaleRevision { expected: ctx.base_revision(), current: self.revision });
        }
        let revision = self.revision.checked_add(1).ok_or(FlattenerPresetError::RevisionOverflow)?;
        let mut presets = self.presets.clone();
        match edit {
            FlattenerPresetEdit::Put(p) => {
                check_flattener_name(&p.name)?;
                p.values.validate()?;
                match presets.iter().position(|q| q.name == p.name) {
                    Some(k) => presets[k] = p.clone(),
                    None if presets.len() >= MAX_FLATTENER_PRESETS => return Err(FlattenerPresetError::TooManyPresets),
                    None => presets.push(p.clone()),
                }
            }
            FlattenerPresetEdit::Remove(name) => {
                let k = presets.iter().position(|q| &q.name == name).ok_or_else(|| FlattenerPresetError::PresetMissing(name.clone()))?;
                presets.remove(k);
            }
        }
        Ok(Self { revision, presets })
    }
}

impl AdvancedOptions {
    /// Resolves `flattener_preset_name` against the stored presets: unset is `Ok(None)`; a text value
    /// names a stored preset exactly or is `flattener_preset_missing`; a captured value that is not
    /// text is `flattener_preset_name_not_text`.
    pub fn resolve_flattener_preset<'p>(&self, presets: &'p FlattenerPresets) -> Result<Option<&'p FlattenerPreset>, FlattenerPresetError> {
        match &self.flattener_preset_name {
            None => Ok(None),
            Some(PrintValue::Text(name)) => presets.resolve(name).map(Some),
            Some(_) => Err(FlattenerPresetError::NameNotText),
        }
    }
}
