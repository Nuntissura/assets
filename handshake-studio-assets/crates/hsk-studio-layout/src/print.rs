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
//! ink manager. The spec counts 109 captured properties (105 on the preset object) but names only the
//! 83 grouped fields and the 12 screen parameters carried here; the unnamed rest is not invented.
#![deny(clippy::arithmetic_side_effects)]
use crate::LayoutError;

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
}
impl PrintUnit {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Percent => "percent",
            Self::Dpi => "dpi",
            Self::Deg => "deg",
            Self::LinesPerInch => "count",
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
