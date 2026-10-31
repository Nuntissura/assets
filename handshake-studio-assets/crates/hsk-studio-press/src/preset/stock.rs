//! Stock PDF export presets (lane SMB-PDF-STOCK-PRESETS-1, STU-LAY-159, MT-30841): the six captured
//! presets as a static, read-only catalogue on the [`PdfPreset`] contract.
//!
//! - Identity: a stable id (`hsk.studio.pdf_preset.<snake>`), the name as the STU-LAY-159 column
//!   states it, and the compliance token as its own field ([`StockPreset::compliance`]); nothing
//!   parses one from the other (the preset named `PDF/X-4` declares `PDF/X-4:2007`).
//! - Values: the 19 table rows ([`STOCK_ROWS`]) and the common defaults exactly; every other setting
//!   carries per-value provenance `studio_declared` (STU-IO-125: the shipped set is Studio's own),
//!   numeric ones from the STU-IO-129 `default` column where it has one ([`stock_provenance`]).
//! - The JPEG-2000 tile and quality have no setting home (SD-17): the record carries them
//!   ([`STOCK_JPEG2000`]) and the provenance receipt marks them `not_representable`.
//! - [`logical_count`] is 6; the seven captured locale variants are not materialised, their
//!   identities were not captured ([`StockGap::LocaleVariantsNotCaptured`]).
//! - [`StockPreset::to_preset`] builds a `shipped` (read-only) preset for the three presets that
//!   assert no standard; the three PDF/X presets refuse `stock_output_intent_not_captured`
//!   (STU-IO-130 needs a profile, none was captured: SD-18). Behind that, SD-13 still refuses them
//!   at resolution (1.3 `compatibility_level_not_writable`, X-4 `standard_level_conflict`).
//!
//! Studio-declared values, the descriptions and the 1:1 chroma encoding (`HSamples`/`VSamples` all
//! 1) are Studio's own; vendor preset values and Acrobat behaviour are UNVERIFIED (D5).

use super::{
    AutoFilterStrategy, AutoRotatePages, Binding, CannotEmbedFontPolicy, ColourConversionStrategy, CompatibilityLevel, ComplianceCheck,
    CompressObjects, DefaultRenderingIntent, DownsampleType, EnumValue, ImageFilter, MinimumResolutionPolicy, OverprintMode, PdfGroup,
    PdfPreset, PresetDraft, PresetError, QualityEntry, QualityValue, SettingRef, SettingSpec, SettingValue, SourceInput,
    TransferFunctionInfo, UcrAndBlackGeneration, SETTINGS,
};

/// The captured stock set is six logical presets (STU-LAY-159).
pub const STOCK_LOGICAL_COUNT: usize = 6;
/// The captured preset FILE count is a different number: six plus seven locale variants.
pub const STOCK_LOCALE_VARIANTS: usize = 7;
/// The locale every stock description is keyed by (studio-declared).
pub const STOCK_DESCRIPTION_LOCALE: &str = "en";

pub const HIGH_QUALITY_PRINT: &str = "hsk.studio.pdf_preset.high_quality_print";
pub const PRESS_QUALITY: &str = "hsk.studio.pdf_preset.press_quality";
pub const SMALLEST_FILE_SIZE: &str = "hsk.studio.pdf_preset.smallest_file_size";
pub const PDF_X_1A_2001: &str = "hsk.studio.pdf_preset.pdf_x_1a_2001";
pub const PDF_X_3_2002: &str = "hsk.studio.pdf_preset.pdf_x_3_2002";
pub const PDF_X_4: &str = "hsk.studio.pdf_preset.pdf_x_4";

/// One STU-LAY-159 table row: the row label as the spec states it and the setting it fills.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StockRow {
    pub row: &'static str,
    pub setting: &'static str,
}
const fn row(row: &'static str, setting: &'static str) -> StockRow {
    StockRow { row, setting }
}
/// The 19 rows, in table order.
pub const STOCK_ROWS: [StockRow; 19] = [
    row("PDF version", "general.compatibility_level"),
    row("Colour conversion", "colour_management.colour_conversion_strategy"),
    row("Colour image resolution", "colour_images.target_resolution"),
    row("Colour downsample threshold", "colour_images.downsample_threshold"),
    row("Greyscale resolution", "greyscale_images.target_resolution"),
    row("Monochrome resolution", "monochrome_images.target_resolution"),
    row("Colour image filter", "colour_images.filter"),
    row("Auto-filter colour images", "colour_images.auto_filter"),
    row("Embed all fonts", "fonts.embed_all_fonts"),
    row("Subset fonts", "fonts.subset_fonts"),
    row("Max subset percent", "fonts.max_subset_percent"),
    row("Optimize", "general.optimisation"),
    row("Compress objects", "general.compress_objects"),
    row("Auto-rotate pages", "general.auto_rotate_pages"),
    row("Allow transparency", "general.allow_transparency"),
    row("Cannot-embed-font policy", "fonts.cannot_embed_font_policy"),
    row("Preserve overprint", "colour_management.preserve_overprint"),
    row("Compliance check", "standards_compliance.compliance_check"),
    row("PDF/X compliant only", "standards_compliance.standard_compliant_only"),
];

/// The STU-LAY-159 column values of one preset, except the compliance check, which is the
/// record's own field ([`StockPreset::compliance`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StockRows {
    pub pdf_version: CompatibilityLevel,
    pub colour_conversion: ColourConversionStrategy,
    pub colour_image_resolution: i64,
    pub colour_downsample_threshold: f64,
    pub greyscale_resolution: i64,
    pub monochrome_resolution: i64,
    pub colour_image_filter: ImageFilter,
    pub auto_filter_colour_images: bool,
    pub embed_all_fonts: bool,
    pub subset_fonts: bool,
    pub max_subset_percent: i64,
    pub optimize: bool,
    pub compress_objects: CompressObjects,
    pub auto_rotate_pages: AutoRotatePages,
    pub allow_transparency: bool,
    pub cannot_embed_font_policy: CannotEmbedFontPolicy,
    pub preserve_overprint: bool,
    pub pdfx_compliant_only: bool,
}

/// One stock preset record.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StockPreset {
    pub id: &'static str,
    /// The name as the STU-LAY-159 column states it.
    pub name: &'static str,
    /// The compliance token, carried apart from [`Self::name`] (AC-003).
    pub compliance: ComplianceCheck,
    pub description: &'static str,
    pub rows: StockRows,
}

/// The JPEG-2000 common default (STU-LAY-159) that has no setting home (SD-17).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Jpeg2000Defaults {
    pub tile_width: u32,
    pub tile_height: u32,
    pub quality: u32,
}
pub const STOCK_JPEG2000: Jpeg2000Defaults = Jpeg2000Defaults { tile_width: 256, tile_height: 256, quality: 30 };
/// ACS image dictionary QFactor (common default).
pub const STOCK_ACS_QFACTOR: f64 = 0.15;
/// 1:1 chroma sampling, as `HSamples` / `VSamples` (encoding studio-declared, UNVERIFIED).
pub const STOCK_ACS_SAMPLES: [f64; 4] = [1.0; 4];
/// Image memory in bytes (common default).
pub const STOCK_IMAGE_MEMORY: i64 = 1_048_576;
/// Start page (common default); the end page is nullable ([`PdfPreset::page_range`]).
pub const STOCK_START_PAGE: i64 = 1;

const fn rows(
    pdf_version: CompatibilityLevel,
    colour_conversion: ColourConversionStrategy,
    resolutions: [i64; 3],
    embed_all_fonts: bool,
    general: (bool, CompressObjects, AutoRotatePages, bool),
    cannot_embed_font_policy: CannotEmbedFontPolicy,
    pdfx_compliant_only: bool,
) -> StockRows {
    let (optimize, compress_objects, auto_rotate_pages, allow_transparency) = general;
    StockRows {
        pdf_version,
        colour_conversion,
        colour_image_resolution: resolutions[0],
        colour_downsample_threshold: 1.5,
        greyscale_resolution: resolutions[1],
        monochrome_resolution: resolutions[2],
        colour_image_filter: ImageFilter::DctEncode,
        auto_filter_colour_images: true,
        embed_all_fonts,
        subset_fonts: true,
        max_subset_percent: 100,
        optimize,
        compress_objects,
        auto_rotate_pages,
        allow_transparency,
        cannot_embed_font_policy,
        preserve_overprint: true,
        pdfx_compliant_only,
    }
}

use AutoRotatePages as Rot;
use CannotEmbedFontPolicy as Cef;
use ColourConversionStrategy as Cc;
use CompatibilityLevel as Lv;
use CompressObjects as Co;

/// The six stock presets, in STU-LAY-159 column order.
pub static STOCK_PRESETS: [StockPreset; STOCK_LOGICAL_COUNT] = [
    StockPreset {
        id: HIGH_QUALITY_PRINT,
        name: "High Quality Print",
        compliance: ComplianceCheck::None,
        description: "Studio stock preset for high-quality desktop printing.",
        rows: rows(Lv::V1_4, Cc::LeaveColorUnchanged, [300, 300, 1200], true, (true, Co::Tags, Rot::All, false), Cef::Warning, false),
    },
    StockPreset {
        id: PRESS_QUALITY,
        name: "Press Quality",
        compliance: ComplianceCheck::None,
        description: "Studio stock preset for commercial press output.",
        rows: rows(Lv::V1_4, Cc::Cmyk, [300, 300, 1200], true, (true, Co::Tags, Rot::None, false), Cef::Error, false),
    },
    StockPreset {
        id: SMALLEST_FILE_SIZE,
        name: "Smallest File Size",
        compliance: ComplianceCheck::None,
        description: "Studio stock preset for the smallest file for screen and web.",
        rows: rows(Lv::V1_5, Cc::Srgb, [100, 150, 300], false, (true, Co::All, Rot::All, false), Cef::Warning, false),
    },
    StockPreset {
        id: PDF_X_1A_2001,
        name: "PDF/X-1a:2001",
        compliance: ComplianceCheck::PdfX1a2001,
        description: "Studio stock preset for PDF/X-1a:2001 exchange.",
        rows: rows(Lv::V1_3, Cc::Cmyk, [300, 300, 1200], true, (false, Co::Off, Rot::None, false), Cef::Error, true),
    },
    StockPreset {
        id: PDF_X_3_2002,
        name: "PDF/X-3:2002",
        compliance: ComplianceCheck::PdfX3_2002,
        description: "Studio stock preset for PDF/X-3:2002 exchange.",
        rows: rows(Lv::V1_3, Cc::UseDeviceIndependentColor, [300, 300, 1200], true, (false, Co::Off, Rot::None, false), Cef::Error, true),
    },
    StockPreset {
        id: PDF_X_4,
        name: "PDF/X-4",
        compliance: ComplianceCheck::PdfX4_2007,
        description: "Studio stock preset for PDF/X-4 exchange with live transparency.",
        rows: rows(Lv::V1_4, Cc::UseDeviceIndependentColor, [300, 300, 1200], true, (false, Co::Off, Rot::None, true), Cef::Error, true),
    },
];

/// The six logical stock presets (the seven locale variants are not materialised).
pub fn logical_count() -> usize {
    STOCK_PRESETS.len()
}
pub fn stock_presets() -> &'static [StockPreset] {
    &STOCK_PRESETS
}
/// The stock preset with this exact id.
pub fn stock_preset(id: &str) -> Option<&'static StockPreset> {
    STOCK_PRESETS.iter().find(|p| p.id == id)
}

/// Where one stock value comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StockValueSource {
    /// A STU-LAY-159 table row (the row label).
    Captured { row: &'static str },
    /// The STU-LAY-159 defaults common to all six.
    CommonDefault,
    /// The spec gives no stock value: Studio's own (STU-IO-125).
    StudioDeclared,
}
impl StockValueSource {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Captured { .. } => "captured",
            Self::CommonDefault => "common_default",
            Self::StudioDeclared => "studio_declared",
        }
    }
}

/// One setting of a stock preset with its value and provenance. `default_column` marks a
/// studio-declared numeric value taken from the STU-IO-129 `default` column.
#[derive(Clone, Debug, PartialEq)]
pub struct StockValue {
    pub setting: SettingRef,
    pub value: SettingValue,
    pub source: StockValueSource,
    pub default_column: bool,
}

/// A captured stock value with no setting home; the receipt marks it `not_representable`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotRepresentable {
    pub name: &'static str,
    pub value: u32,
    pub code: &'static str,
}
pub const NOT_REPRESENTABLE: &str = "not_representable";

/// The per-value provenance receipt of one stock preset.
#[derive(Clone, Debug, PartialEq)]
pub struct StockProvenance {
    pub preset_id: &'static str,
    /// One entry per [`SETTINGS`] entry, same order: `(setting, source, default_column)`.
    pub entries: Vec<(SettingRef, StockValueSource, bool)>,
    /// The description text and locale are studio-declared.
    pub description: StockValueSource,
    pub not_representable: [NotRepresentable; 3],
    pub gaps: &'static [StockGap],
}
impl StockProvenance {
    /// The provenance of `group.name`.
    pub fn source(&self, key: &str) -> Option<(StockValueSource, bool)> {
        self.entries.iter().find(|(r, _, _)| r.key() == key).map(|(_, s, d)| (*s, *d))
    }
}

/// An open spec gap or spec conflict the stock catalogue carries (STU-LAY-168: one per gap).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StockGap {
    /// The seven locale-variant identities were not captured; they are not materialised.
    LocaleVariantsNotCaptured,
    /// SD-18: no output-intent profile was captured for the three PDF/X presets.
    OutputIntentNotCaptured,
    /// SD-17: the JPEG-2000 tile and quality have no setting home.
    Jpeg2000NotRepresentable,
    /// SD-14: STU-LAY-159 names CCITT; STU-IO-128 lists DCTEncode and FlateEncode only.
    CcittOutsideFilterDomain,
    /// SD-15: STU-LAY-159 says a nullable end page; STU-IO-129 stores -1.
    EndPageSentinel,
    /// SD-16: STU-LAY-159 shows `Error` captured; STU-IO-128 labels it studio-declared.
    CannotEmbedErrorProvenance,
}
impl StockGap {
    pub const ALL: [StockGap; 6] = [
        Self::LocaleVariantsNotCaptured,
        Self::OutputIntentNotCaptured,
        Self::Jpeg2000NotRepresentable,
        Self::CcittOutsideFilterDomain,
        Self::EndPageSentinel,
        Self::CannotEmbedErrorProvenance,
    ];
    pub const fn code(self) -> &'static str {
        match self {
            Self::LocaleVariantsNotCaptured => "locale_variants_not_captured",
            Self::OutputIntentNotCaptured => "stock_output_intent_not_captured",
            Self::Jpeg2000NotRepresentable => "jpeg2000_not_representable",
            Self::CcittOutsideFilterDomain => "ccitt_outside_image_filter_domain",
            Self::EndPageSentinel => "end_page_sentinel_conflict",
            Self::CannotEmbedErrorProvenance => "cannot_embed_error_provenance_conflict",
        }
    }
    /// The spec-delta id the gap is filed under, if any.
    pub const fn spec_delta(self) -> Option<&'static str> {
        match self {
            Self::LocaleVariantsNotCaptured => None,
            Self::OutputIntentNotCaptured => Some("SD-18"),
            Self::Jpeg2000NotRepresentable => Some("SD-17"),
            Self::CcittOutsideFilterDomain => Some("SD-14"),
            Self::EndPageSentinel => Some("SD-15"),
            Self::CannotEmbedErrorProvenance => Some("SD-16"),
        }
    }
}

fn acs_dictionary() -> SettingValue {
    SettingValue::Quality(vec![
        QualityEntry { key: "QFactor".to_owned(), value: QualityValue::Number(STOCK_ACS_QFACTOR) },
        QualityEntry { key: "HSamples".to_owned(), value: QualityValue::Numbers(STOCK_ACS_SAMPLES.to_vec()) },
        QualityEntry { key: "VSamples".to_owned(), value: QualityValue::Numbers(STOCK_ACS_SAMPLES.to_vec()) },
    ])
}

impl StockPreset {
    /// Every setting of [`SETTINGS`] with its stock value and provenance, in table order.
    pub fn values(&self) -> Vec<StockValue> {
        SETTINGS
            .iter()
            .filter_map(|spec| {
                self.value_of(spec).map(|(value, source, default_column)| StockValue { setting: spec.setting(), value, source, default_column })
            })
            .collect()
    }

    /// The stock value of `group.name`.
    pub fn value(&self, key: &str) -> Option<SettingValue> {
        let spec = SETTINGS.iter().find(|s| s.setting().key() == key)?;
        self.value_of(spec).map(|(v, _, _)| v)
    }

    fn value_of(&self, spec: &SettingSpec) -> Option<(SettingValue, StockValueSource, bool)> {
        use PdfGroup as G;
        use SettingValue::{Bool, Integer};
        use StockValueSource::{CommonDefault as Common, StudioDeclared as Decl};
        let cap = |row: &'static str| StockValueSource::Captured { row };
        let r = &self.rows;
        let e = SettingValue::Enum;
        if spec.group.is_image() {
            return self.image_value(spec.group, spec.name);
        }
        Some(match (spec.group, spec.name) {
            (G::General, "compatibility_level") => (e(EnumValue::CompatibilityLevel(r.pdf_version)), cap("PDF version"), false),
            (G::General, "start_page") => (Integer(STOCK_START_PAGE), Common, false),
            (G::General, "end_page") => (Integer(-1), Common, false),
            (G::General, "thumbnails") => (Bool(false), Decl, false),
            (G::General, "optimisation") => (Bool(r.optimize), cap("Optimize"), false),
            (G::General, "binding") => (e(EnumValue::Binding(Binding::Left)), Decl, false),
            (G::General, "auto_rotate_pages") => (e(EnumValue::AutoRotatePages(r.auto_rotate_pages)), cap("Auto-rotate pages"), false),
            (G::General, "compress_objects") => (e(EnumValue::CompressObjects(r.compress_objects)), cap("Compress objects"), false),
            (G::General, "image_memory") => (Integer(STOCK_IMAGE_MEMORY), Common, false),
            (G::General, "allow_transparency") => (Bool(r.allow_transparency), cap("Allow transparency"), false),
            (G::Fonts, "embed_all_fonts") => (Bool(r.embed_all_fonts), cap("Embed all fonts"), false),
            (G::Fonts, "subset_fonts") => (Bool(r.subset_fonts), cap("Subset fonts"), false),
            (G::Fonts, "max_subset_percent") => (Integer(r.max_subset_percent), cap("Max subset percent"), false),
            (G::Fonts, "embed_open_type") => (Bool(false), Decl, false),
            (G::Fonts, "always_embed" | "never_embed") => (SettingValue::FontList(Vec::new()), Decl, false),
            (G::Fonts, "cannot_embed_font_policy") => {
                (e(EnumValue::CannotEmbedFontPolicy(r.cannot_embed_font_policy)), cap("Cannot-embed-font policy"), false)
            }
            (G::ColourManagement, "colour_conversion_strategy") => {
                (e(EnumValue::ColourConversionStrategy(r.colour_conversion)), cap("Colour conversion"), false)
            }
            (G::ColourManagement, "transfer_function_info") => (e(EnumValue::TransferFunctionInfo(TransferFunctionInfo::Apply)), Decl, false),
            (G::ColourManagement, "ucr_and_black_generation") => {
                (e(EnumValue::UcrAndBlackGeneration(UcrAndBlackGeneration::Preserve)), Decl, false)
            }
            (G::ColourManagement, "default_rendering_intent") => {
                (e(EnumValue::DefaultRenderingIntent(DefaultRenderingIntent::Default)), Decl, false)
            }
            (G::ColourManagement, "overprint_mode") => (e(EnumValue::OverprintMode(OverprintMode::One)), Decl, false),
            (G::ColourManagement, "preserve_overprint") => (Bool(r.preserve_overprint), cap("Preserve overprint"), false),
            (G::OutputIntent, "output_intent_profile") => (SettingValue::Text(String::new()), Decl, false),
            (G::StandardsCompliance, "standard_check_1a" | "standard_check_3") => (Bool(false), Decl, false),
            (G::StandardsCompliance, "standard_compliant_only") => (Bool(r.pdfx_compliant_only), cap("PDF/X compliant only"), false),
            (G::StandardsCompliance, "compliance_check") => (e(EnumValue::ComplianceCheck(self.compliance)), cap("Compliance check"), false),
            (G::PostscriptAndDsc, "dsc_reporting_level") => (Integer(0), Decl, true),
            _ => return None,
        })
    }

    fn image_value(&self, g: PdfGroup, name: &str) -> Option<(SettingValue, StockValueSource, bool)> {
        use PdfGroup as G;
        use SettingValue::{Bool, Integer, Real};
        use StockValueSource::{CommonDefault as Common, StudioDeclared as Decl};
        let cap = |row: &'static str| StockValueSource::Captured { row };
        let r = &self.rows;
        let e = SettingValue::Enum;
        let colour = g == G::ColourImages;
        let mono = g == G::MonochromeImages;
        Some(match name {
            "anti_alias" | "crop_to_frame" => (Bool(false), Decl, false),
            "minimum_resolution" => (Integer(if mono { 1200 } else { 150 }), Decl, false),
            "minimum_resolution_policy" => (e(EnumValue::MinimumResolutionPolicy(MinimumResolutionPolicy::Ok)), Decl, false),
            "downsample" => (Bool(true), Decl, false),
            "downsample_type" => (e(EnumValue::DownsampleType(DownsampleType::Bicubic)), Decl, false),
            "target_resolution" => match g {
                G::ColourImages => (Integer(r.colour_image_resolution), cap("Colour image resolution"), false),
                G::GreyscaleImages => (Integer(r.greyscale_resolution), cap("Greyscale resolution"), false),
                _ => (Integer(r.monochrome_resolution), cap("Monochrome resolution"), false),
            },
            "bit_depth" => (Integer(-1), Decl, true),
            "minimum_downsample_depth" => (Integer(if g == G::GreyscaleImages { 2 } else { 1 }), Decl, false),
            "downsample_threshold" if colour => (Real(r.colour_downsample_threshold), cap("Colour downsample threshold"), false),
            "downsample_threshold" => (Real(1.5), Decl, true),
            "encode" => (Bool(true), Decl, false),
            "filter" if colour => (e(EnumValue::ImageFilter(r.colour_image_filter)), cap("Colour image filter"), false),
            "filter" if mono => (e(EnumValue::ImageFilter(ImageFilter::CcittFaxEncode)), Common, false),
            "filter" => (e(EnumValue::ImageFilter(ImageFilter::DctEncode)), Decl, false),
            "auto_filter" if colour => (Bool(r.auto_filter_colour_images), cap("Auto-filter colour images"), false),
            "auto_filter" => (Bool(!mono), Decl, false),
            "auto_filter_strategy" => (e(EnumValue::AutoFilterStrategy(AutoFilterStrategy::Jpeg)), Decl, false),
            "base_quality_dictionary" => (SettingValue::Quality(Vec::new()), Decl, false),
            "auto_colour_space_quality_dictionary" if !mono => (acs_dictionary(), Common, false),
            "auto_colour_space_quality_dictionary" => (SettingValue::Quality(Vec::new()), Decl, false),
            _ => return None,
        })
    }

    /// The per-value provenance receipt.
    pub fn provenance(&self) -> StockProvenance {
        let not_rep = |name: &'static str, value: u32| NotRepresentable { name, value, code: NOT_REPRESENTABLE };
        StockProvenance {
            preset_id: self.id,
            entries: self.values().into_iter().map(|v| (v.setting, v.source, v.default_column)).collect(),
            description: StockValueSource::StudioDeclared,
            not_representable: [
                not_rep("jpeg2000_tile_width", STOCK_JPEG2000.tile_width),
                not_rep("jpeg2000_tile_height", STOCK_JPEG2000.tile_height),
                not_rep("jpeg2000_quality", STOCK_JPEG2000.quality),
            ],
            gaps: &StockGap::ALL,
        }
    }

    /// Builds the read-only `shipped` [`PdfPreset`]. A preset asserting a standard refuses
    /// `stock_output_intent_not_captured` (SD-18); `edit_copy` gives an operator preset.
    pub fn to_preset(&self) -> Result<PdfPreset, PresetError> {
        if self.compliance != ComplianceCheck::None {
            return Err(PresetError::StockOutputIntentNotCaptured);
        }
        let values: Vec<(String, SettingValue)> = self.values().into_iter().map(|v| (v.setting.key(), v.value)).collect();
        let descriptions = [(STOCK_DESCRIPTION_LOCALE, self.description)];
        let head = PresetDraft {
            id: self.id,
            name: self.name,
            descriptions: &descriptions,
            default_locale: STOCK_DESCRIPTION_LOCALE,
            source: SourceInput::Shipped,
            settings: &[],
        };
        PdfPreset::from_values(&head, &values)
    }
}

/// The per-value provenance receipt of the stock preset `id`.
pub fn stock_provenance(id: &str) -> Option<StockProvenance> {
    stock_preset(id).map(StockPreset::provenance)
}
