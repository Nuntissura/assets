//! PDF export presets (lane SMB-PDF-PRESETS-1, STU-IO-125..133): a typed, COMPLETE parameter set and
//! its resolution onto press-write [`ExportOptions`].
//!
//! - Completeness (STU-IO-125): [`PdfPreset::from_draft`] refuses `preset_incomplete` naming the first
//!   missing setting; no setting is filled from an ambient default, and resolution builds every
//!   [`ExportOptions`] field from the preset or the caller's request (test `incomplete_draft_refuses`).
//! - Groups (STU-IO-126): every setting in [`SETTINGS`] declares one of the ten [`PdfGroup`]s; a group
//!   assignment the spec does not state is labelled [`Basis::StudioDeclared`]. The spec names no
//!   setting of the `security` group, so that group carries none (reported to the spec owner).
//! - Image family (STU-IO-127): each of the three image groups carries the same sixteen settings
//!   (eleven family settings, the three-setting codec family, two quality dictionaries).
//! - Domains (STU-IO-128): the fourteen enumerations exactly as tabled; `binding` `Right` and
//!   `cannot_embed_font_policy` `Error` / `OK` are studio-declared; single-observed-value rows carry
//!   [`DomainCompleteness::Partial`].
//! - Numeric contracts (STU-IO-129): [`NumericContract`] stores hard_min, hard_max, soft_min,
//!   soft_max, default, unit and precision separately; `unknown` and `absent` stay distinct
//!   [`Bound`]s. A value outside a hard bound refuses; outside a soft bound it is a typed
//!   [`PresetWarning`].
//! - Standards (STU-IO-130): asserting a standard check with an empty `output_intent_profile` refuses
//!   `standard_without_output_intent`.
//! - Fonts (STU-IO-131, STU-IO-012): [`PdfPreset::font_receipt`] names the font and the policy. The
//!   writer has no path that writes output without a font it cannot embed, so under every policy the
//!   receipt records the export refusal and the declared gap [`FONT_GAP`].
//! - Descriptions (STU-IO-132): a locale-keyed map rendered through [`DESCRIPTION_FALLBACK_CHAIN`].
//! - Provenance (STU-IO-133): `shipped` presets are read-only; [`PdfPreset::edit_copy`] yields an
//!   `operator` preset with the parent recorded.
//!
//! Spec delta SD-13: the `compatibility_level` domain is 1.3 / 1.4 / 1.5 while the writer writes
//! 1.4 .. 2.0 and PDF/X-4 only at 1.6 or later. Resolution refuses level 1.3
//! (`compatibility_level_not_writable`) and any PDF/X-4 request (`standard_level_conflict`); it
//! resolves neither side. Vendor preset values and Acrobat behaviour are UNVERIFIED (D5).
//!
//! Hostile input (V4-01/02): the draft's setting count, every list, the locale map, every name and
//! text are bounded and checked before anything is allocated; every refusal is a typed
//! [`PresetError`] (tests `hostile_sizes_refuse_before_allocation`, `hostile_values_refuse_typed`).
#![deny(clippy::arithmetic_side_effects)]

use hsk_studio_observe::{FailureCode, Outcome};
use hsk_studio_press_write::{ExportError, ExportOptions, OutputIntent, PdfX4};

/// Most `(key, value)` entries one draft may carry (the table has [`SETTINGS`]`.len()`).
pub const MAX_PRESET_ENTRIES: usize = 256;
/// Most names in `always_embed` or `never_embed`.
pub const MAX_FONT_LIST: usize = 512;
/// Longest font name in bytes.
pub const MAX_FONT_NAME_BYTES: usize = 255;
/// Longest preset id in bytes (ASCII letters, digits, `-`, `_`, `.`).
pub const MAX_PRESET_ID_BYTES: usize = 128;
/// Longest preset name in bytes.
pub const MAX_PRESET_NAME_BYTES: usize = 255;
/// Most locales in a description map.
pub const MAX_LOCALES: usize = 64;
/// Longest locale tag in bytes.
pub const MAX_LOCALE_BYTES: usize = 35;
/// Longest description text in bytes.
pub const MAX_DESCRIPTION_BYTES: usize = 4096;
/// Longest text setting (`output_intent_profile`) or import origin in bytes.
pub const MAX_TEXT_BYTES: usize = 1024;
/// Most entries of one quality dictionary.
pub const MAX_QUALITY_ENTRIES: usize = 32;
/// Longest quality-dictionary key or name value in bytes.
pub const MAX_QUALITY_KEY_BYTES: usize = 64;
/// Most numbers in one quality-dictionary array value.
pub const MAX_QUALITY_NUMBERS: usize = 16;
/// The declared description fallback chain (STU-IO-132).
pub const DESCRIPTION_FALLBACK_CHAIN: [&str; 3] = ["requested_locale", "language", "default_locale"];
/// The declared gap of a font the writer cannot embed: output without that font is not produced.
pub const FONT_GAP: &str = "output_without_font_not_implemented";

/// The ten PDF setting groups (STU-IO-126).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PdfGroup {
    General,
    Fonts,
    ColourImages,
    GreyscaleImages,
    MonochromeImages,
    ColourManagement,
    OutputIntent,
    StandardsCompliance,
    PostscriptAndDsc,
    Security,
}
impl PdfGroup {
    pub const ALL: [PdfGroup; 10] = [
        Self::General,
        Self::Fonts,
        Self::ColourImages,
        Self::GreyscaleImages,
        Self::MonochromeImages,
        Self::ColourManagement,
        Self::OutputIntent,
        Self::StandardsCompliance,
        Self::PostscriptAndDsc,
        Self::Security,
    ];
    /// The image groups carrying the STU-IO-127 family.
    pub const IMAGES: [PdfGroup; 3] = [Self::ColourImages, Self::GreyscaleImages, Self::MonochromeImages];
    pub const fn token(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Fonts => "fonts",
            Self::ColourImages => "colour_images",
            Self::GreyscaleImages => "greyscale_images",
            Self::MonochromeImages => "monochrome_images",
            Self::ColourManagement => "colour_management",
            Self::OutputIntent => "output_intent",
            Self::StandardsCompliance => "standards_compliance",
            Self::PostscriptAndDsc => "postscript_and_dsc",
            Self::Security => "security",
        }
    }
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|g| g.token() == token)
    }
    pub const fn is_image(self) -> bool {
        matches!(self, Self::ColourImages | Self::GreyscaleImages | Self::MonochromeImages)
    }
}

/// Whether a member, a group assignment or a name comes from the spec or is declared by Studio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basis {
    Spec,
    StudioDeclared,
}
impl Basis {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Spec => "spec",
            Self::StudioDeclared => "studio_declared",
        }
    }
}

/// `domain_completeness` of an enumeration (STU-IO-128 :21026).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DomainCompleteness {
    Complete,
    /// Only one value was observed; a later capture may extend the domain.
    Partial,
}
impl DomainCompleteness {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
        }
    }
}

/// One member of an enumeration with its basis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnumMember {
    pub token: &'static str,
    pub basis: Basis,
}

macro_rules! pdf_enum {
    ($(#[$m:meta])* $name:ident, $setting:literal, $completeness:ident, [$($variant:ident = $token:literal : $basis:ident),+ $(,)?]) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant),+
        }
        impl $name {
            /// The STU-IO-128 setting this domain belongs to.
            pub const SETTING: &'static str = $setting;
            pub const MEMBERS: &'static [EnumMember] = &[$(EnumMember { token: $token, basis: Basis::$basis }),+];
            pub const COMPLETENESS: DomainCompleteness = DomainCompleteness::$completeness;
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub const fn token(self) -> &'static str {
                match self {
                    $(Self::$variant => $token),+
                }
            }
            /// Exact, case-sensitive token match; nothing is mapped onto another member.
            pub fn from_token(token: &str) -> Option<Self> {
                match token {
                    $($token => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

pdf_enum!(CompatibilityLevel, "compatibility_level", Complete, [V1_3 = "1.3": Spec, V1_4 = "1.4": Spec, V1_5 = "1.5": Spec]);
pdf_enum!(ColourConversionStrategy, "colour_conversion_strategy", Complete, [
    LeaveColorUnchanged = "LeaveColorUnchanged": Spec,
    UseDeviceIndependentColor = "UseDeviceIndependentColor": Spec,
    Cmyk = "CMYK": Spec,
    Srgb = "sRGB": Spec,
]);
pdf_enum!(DownsampleType, "downsample_type", Complete, [Bicubic = "Bicubic": Spec, None = "None": Spec]);
pdf_enum!(ImageFilter, "image_filter", Complete, [DctEncode = "DCTEncode": Spec, FlateEncode = "FlateEncode": Spec]);
pdf_enum!(AutoFilterStrategy, "auto_filter_strategy", Complete, [Jpeg = "JPEG": Spec, Jpeg2000 = "JPEG2000": Spec]);
pdf_enum!(MinimumResolutionPolicy, "minimum_resolution_policy", Complete, [Ok = "OK": Spec, Warning = "Warning": Spec]);
pdf_enum!(TransferFunctionInfo, "transfer_function_info", Complete, [Apply = "Apply": Spec, Remove = "Remove": Spec]);
pdf_enum!(UcrAndBlackGeneration, "ucr_and_black_generation", Complete, [Preserve = "Preserve": Spec, Remove = "Remove": Spec]);
pdf_enum!(AutoRotatePages, "auto_rotate_pages", Complete, [All = "All": Spec, None = "None": Spec]);
pdf_enum!(Binding, "binding", Partial, [Left = "Left": Spec, Right = "Right": StudioDeclared]);
pdf_enum!(CompressObjects, "compress_objects", Complete, [Tags = "Tags": Spec, Off = "Off": Spec, All = "All": Spec]);
pdf_enum!(
    /// An integer domain; the token is the decimal integer, and [`SettingInput::Integer`] is accepted too.
    OverprintMode, "overprint_mode", Partial, [One = "1": Spec]
);
pdf_enum!(
    /// Only `Warning` was observed (Partial); `Error` and `OK` are studio-declared.
    CannotEmbedFontPolicy, "cannot_embed_font_policy", Partial, [
    Warning = "Warning": Spec,
    Error = "Error": StudioDeclared,
    Ok = "OK": StudioDeclared,
]);
pdf_enum!(DefaultRenderingIntent, "default_rendering_intent", Partial, [Default = "Default": Spec]);

macro_rules! domains {
    ($($t:ident),+ $(,)?) => {
        /// The fourteen STU-IO-128 enumerations.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum EnumDomain {
            $($t),+
        }
        impl EnumDomain {
            pub const ALL: &'static [EnumDomain] = &[$(Self::$t),+];
            pub const fn setting(self) -> &'static str {
                match self {
                    $(Self::$t => $t::SETTING),+
                }
            }
            pub const fn members(self) -> &'static [EnumMember] {
                match self {
                    $(Self::$t => $t::MEMBERS),+
                }
            }
            pub const fn completeness(self) -> DomainCompleteness {
                match self {
                    $(Self::$t => $t::COMPLETENESS),+
                }
            }
            pub fn parse(self, token: &str) -> Option<EnumValue> {
                match self {
                    $(Self::$t => $t::from_token(token).map(EnumValue::$t)),+
                }
            }
        }
        /// A typed member of one of the fourteen enumerations.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum EnumValue {
            $($t($t)),+
        }
        impl EnumValue {
            pub const fn token(self) -> &'static str {
                match self {
                    $(Self::$t(v) => v.token()),+
                }
            }
            pub const fn domain(self) -> EnumDomain {
                match self {
                    $(Self::$t(_) => EnumDomain::$t),+
                }
            }
        }
    };
}
domains!(
    CompatibilityLevel,
    ColourConversionStrategy,
    DownsampleType,
    ImageFilter,
    AutoFilterStrategy,
    MinimumResolutionPolicy,
    TransferFunctionInfo,
    UcrAndBlackGeneration,
    AutoRotatePages,
    Binding,
    CompressObjects,
    OverprintMode,
    CannotEmbedFontPolicy,
    DefaultRenderingIntent,
);

/// One field of a numeric contract: a value, `unknown` (the source declares a value, not a bound)
/// or `absent` (the field does not apply). Unknown is never replaced by the opposite bound.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Bound {
    Value(f64),
    Unknown,
    Absent,
}
impl Bound {
    const fn value(self) -> Option<f64> {
        match self {
            Self::Value(v) => Some(v),
            Self::Unknown | Self::Absent => None,
        }
    }
}

/// A numeric setting contract (STU-IO-129), seven separate fields. `spec_row` is false for a
/// setting the table has no row for: every field is then `Unknown`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NumericContract {
    pub name: &'static str,
    pub hard_min: Bound,
    pub hard_max: Bound,
    pub soft_min: Bound,
    pub soft_max: Bound,
    pub default: Bound,
    pub unit: &'static str,
    pub precision: u8,
    pub spec_row: bool,
}

enum Fit {
    Inside,
    OutsideSoft,
    OutsideHard,
}
impl NumericContract {
    fn fit(&self, v: f64) -> Fit {
        let below = |b: Bound| b.value().is_some_and(|m| v < m);
        let above = |b: Bound| b.value().is_some_and(|m| v > m);
        if below(self.hard_min) || above(self.hard_max) {
            Fit::OutsideHard
        } else if below(self.soft_min) || above(self.soft_max) {
            Fit::OutsideSoft
        } else {
            Fit::Inside
        }
    }
}

use Bound::{Absent, Unknown, Value as V};
const fn row(name: &'static str, b: [Bound; 5], unit: &'static str, precision: u8) -> NumericContract {
    NumericContract { name, hard_min: b[0], hard_max: b[1], soft_min: b[2], soft_max: b[3], default: b[4], unit, precision, spec_row: true }
}
const fn no_row(name: &'static str, unit: &'static str) -> NumericContract {
    NumericContract {
        name,
        hard_min: Unknown,
        hard_max: Unknown,
        soft_min: Unknown,
        soft_max: Unknown,
        default: Unknown,
        unit,
        precision: 0,
        spec_row: false,
    }
}
pub const COLOUR_IMAGE_RESOLUTION: NumericContract =
    row("colour_image_resolution", [V(1.0), Unknown, V(72.0), V(600.0), V(300.0)], "ppi", 0);
pub const GREYSCALE_IMAGE_RESOLUTION: NumericContract =
    row("greyscale_image_resolution", [V(1.0), Unknown, V(72.0), V(600.0), V(300.0)], "ppi", 0);
pub const MONOCHROME_IMAGE_RESOLUTION: NumericContract =
    row("monochrome_image_resolution", [V(1.0), Unknown, V(300.0), V(2400.0), V(1200.0)], "ppi", 0);
pub const DOWNSAMPLE_THRESHOLD: NumericContract = row("downsample_threshold", [V(1.0), Unknown, V(1.0), V(3.0), V(1.5)], "ratio", 5);
pub const MAX_SUBSET_PERCENT: NumericContract = row("max_subset_percent", [V(0.0), V(100.0), V(0.0), V(100.0), V(100.0)], "percent", 0);
pub const IMAGE_MEMORY: NumericContract = row("image_memory", [Unknown, Unknown, Absent, Absent, V(1_048_576.0)], "bytes", 0);
pub const START_PAGE: NumericContract = row("start_page", [V(1.0), Unknown, V(1.0), Unknown, V(1.0)], "page number", 0);
pub const END_PAGE: NumericContract = row("end_page", [V(-1.0), Unknown, V(-1.0), Unknown, V(-1.0)], "page number (-1 = last)", 0);
pub const DSC_REPORTING_LEVEL: NumericContract = row("dsc_reporting_level", [V(0.0), Unknown, V(0.0), V(3.0), V(0.0)], "level", 0);
pub const BIT_DEPTH: NumericContract = row("bit_depth", [V(-1.0), Unknown, V(-1.0), V(16.0), V(-1.0)], "bits (-1 = unchanged)", 0);
/// No STU-IO-129 row: every field unknown.
pub const MINIMUM_RESOLUTION: NumericContract = no_row("minimum_resolution", "ppi");
/// No STU-IO-129 row: every field unknown.
pub const MINIMUM_DOWNSAMPLE_DEPTH: NumericContract = no_row("minimum_downsample_depth", "bits");

/// The kind of value a setting takes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SettingKind {
    Bool,
    Integer(&'static NumericContract),
    Real(&'static NumericContract),
    Enum(EnumDomain),
    /// Ordered font-name list.
    FontList,
    /// Ordered `key -> scalar` dictionary.
    QualityDict,
    /// Bounded text (may be empty).
    Text,
}

/// One setting of the table: its name, group (and whether the spec states that group) and kind.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SettingSpec {
    pub group: PdfGroup,
    pub name: &'static str,
    pub group_basis: Basis,
    pub kind: SettingKind,
}
impl SettingSpec {
    pub const fn setting(&self) -> SettingRef {
        SettingRef { group: self.group, name: self.name }
    }
}

/// A setting identity: `group.name` (for example `colour_images.downsample`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SettingRef {
    pub group: PdfGroup,
    pub name: &'static str,
}
impl SettingRef {
    pub fn key(&self) -> String {
        format!("{}.{}", self.group.token(), self.name)
    }
}
impl std::fmt::Display for SettingRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.group.token(), self.name)
    }
}

const fn s(group: PdfGroup, name: &'static str, group_basis: Basis, kind: SettingKind) -> SettingSpec {
    SettingSpec { group, name, group_basis, kind }
}

macro_rules! settings_table {
    ([$($base:expr),* $(,)?] [$($g:ident : $res:ident),*] [$($tail:expr),* $(,)?]) => {
        &[
            $($base,)*
            $(
                s(PdfGroup::$g, "anti_alias", Basis::Spec, SettingKind::Bool),
                s(PdfGroup::$g, "crop_to_frame", Basis::Spec, SettingKind::Bool),
                s(PdfGroup::$g, "minimum_resolution", Basis::Spec, SettingKind::Integer(&MINIMUM_RESOLUTION)),
                s(PdfGroup::$g, "minimum_resolution_policy", Basis::Spec, SettingKind::Enum(EnumDomain::MinimumResolutionPolicy)),
                s(PdfGroup::$g, "downsample", Basis::Spec, SettingKind::Bool),
                s(PdfGroup::$g, "downsample_type", Basis::Spec, SettingKind::Enum(EnumDomain::DownsampleType)),
                s(PdfGroup::$g, "target_resolution", Basis::Spec, SettingKind::Integer(&$res)),
                s(PdfGroup::$g, "bit_depth", Basis::Spec, SettingKind::Integer(&BIT_DEPTH)),
                s(PdfGroup::$g, "minimum_downsample_depth", Basis::Spec, SettingKind::Integer(&MINIMUM_DOWNSAMPLE_DEPTH)),
                s(PdfGroup::$g, "downsample_threshold", Basis::Spec, SettingKind::Real(&DOWNSAMPLE_THRESHOLD)),
                s(PdfGroup::$g, "encode", Basis::Spec, SettingKind::Bool),
                s(PdfGroup::$g, "filter", Basis::Spec, SettingKind::Enum(EnumDomain::ImageFilter)),
                s(PdfGroup::$g, "auto_filter", Basis::Spec, SettingKind::Bool),
                s(PdfGroup::$g, "auto_filter_strategy", Basis::Spec, SettingKind::Enum(EnumDomain::AutoFilterStrategy)),
                s(PdfGroup::$g, "base_quality_dictionary", Basis::Spec, SettingKind::QualityDict),
                s(PdfGroup::$g, "auto_colour_space_quality_dictionary", Basis::Spec, SettingKind::QualityDict),
            )*
            $($tail,)*
        ]
    };
}

/// Every setting of a PDF preset, in receipt order. Each declares its group (STU-IO-126).
pub static SETTINGS: &[SettingSpec] = settings_table!(
    [
        s(PdfGroup::General, "compatibility_level", Basis::Spec, SettingKind::Enum(EnumDomain::CompatibilityLevel)),
        s(PdfGroup::General, "start_page", Basis::Spec, SettingKind::Integer(&START_PAGE)),
        s(PdfGroup::General, "end_page", Basis::Spec, SettingKind::Integer(&END_PAGE)),
        s(PdfGroup::General, "thumbnails", Basis::Spec, SettingKind::Bool),
        s(PdfGroup::General, "optimisation", Basis::Spec, SettingKind::Bool),
        s(PdfGroup::General, "binding", Basis::Spec, SettingKind::Enum(EnumDomain::Binding)),
        s(PdfGroup::General, "auto_rotate_pages", Basis::Spec, SettingKind::Enum(EnumDomain::AutoRotatePages)),
        s(PdfGroup::General, "compress_objects", Basis::Spec, SettingKind::Enum(EnumDomain::CompressObjects)),
        s(PdfGroup::General, "image_memory", Basis::StudioDeclared, SettingKind::Integer(&IMAGE_MEMORY)),
        s(PdfGroup::Fonts, "embed_all_fonts", Basis::Spec, SettingKind::Bool),
        s(PdfGroup::Fonts, "subset_fonts", Basis::Spec, SettingKind::Bool),
        s(PdfGroup::Fonts, "max_subset_percent", Basis::Spec, SettingKind::Integer(&MAX_SUBSET_PERCENT)),
        s(PdfGroup::Fonts, "embed_open_type", Basis::Spec, SettingKind::Bool),
        s(PdfGroup::Fonts, "always_embed", Basis::Spec, SettingKind::FontList),
        s(PdfGroup::Fonts, "never_embed", Basis::Spec, SettingKind::FontList),
        s(PdfGroup::Fonts, "cannot_embed_font_policy", Basis::Spec, SettingKind::Enum(EnumDomain::CannotEmbedFontPolicy)),
    ]
    [ColourImages: COLOUR_IMAGE_RESOLUTION, GreyscaleImages: GREYSCALE_IMAGE_RESOLUTION, MonochromeImages: MONOCHROME_IMAGE_RESOLUTION]
    [
        s(PdfGroup::ColourManagement, "colour_conversion_strategy", Basis::StudioDeclared, SettingKind::Enum(EnumDomain::ColourConversionStrategy)),
        s(PdfGroup::ColourManagement, "transfer_function_info", Basis::StudioDeclared, SettingKind::Enum(EnumDomain::TransferFunctionInfo)),
        s(PdfGroup::ColourManagement, "ucr_and_black_generation", Basis::StudioDeclared, SettingKind::Enum(EnumDomain::UcrAndBlackGeneration)),
        s(PdfGroup::ColourManagement, "default_rendering_intent", Basis::StudioDeclared, SettingKind::Enum(EnumDomain::DefaultRenderingIntent)),
        s(PdfGroup::ColourManagement, "overprint_mode", Basis::StudioDeclared, SettingKind::Enum(EnumDomain::OverprintMode)),
        s(PdfGroup::OutputIntent, "output_intent_profile", Basis::StudioDeclared, SettingKind::Text),
        s(PdfGroup::StandardsCompliance, "standard_check_1a", Basis::Spec, SettingKind::Bool),
        s(PdfGroup::StandardsCompliance, "standard_check_3", Basis::Spec, SettingKind::Bool),
        s(PdfGroup::StandardsCompliance, "standard_compliant_only", Basis::Spec, SettingKind::Bool),
        s(PdfGroup::PostscriptAndDsc, "dsc_reporting_level", Basis::StudioDeclared, SettingKind::Integer(&DSC_REPORTING_LEVEL)),
    ]
);

/// Index of `group.name` in [`SETTINGS`].
fn position(group: PdfGroup, name: &str) -> Option<usize> {
    SETTINGS.iter().position(|s| s.group == group && s.name == name)
}
/// Parses a `group.name` key into its table index.
fn key_position(key: &str) -> Option<usize> {
    let (group, name) = key.split_once('.')?;
    position(PdfGroup::from_token(group)?, name)
}

/// Typed refusal. Stable snake_case [`PresetError::code`]; no variant carries caller text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresetError {
    /// The draft carries more than [`MAX_PRESET_ENTRIES`] entries.
    TooManySettings,
    /// The entry at this draft position names no setting of [`SETTINGS`].
    UnknownSetting { position: usize },
    DuplicateSetting(SettingRef),
    /// STU-IO-125: the setting is missing; nothing is filled from a default.
    PresetIncomplete(SettingRef),
    WrongValueKind(SettingRef),
    /// The token is not a member of the setting's domain.
    UnknownMember(SettingRef),
    NonFinite(SettingRef),
    OutsideHardBounds(SettingRef),
    ListTooLong(SettingRef),
    FontNameInvalid(SettingRef),
    QualityDictionaryInvalid(SettingRef),
    TextInvalid(SettingRef),
    /// STU-IO-130: a standard check is asserted and `output_intent_profile` is empty.
    StandardWithoutOutputIntent,
    InvalidId,
    InvalidName,
    NoDescriptions,
    TooManyLocales,
    InvalidLocale { position: usize },
    DuplicateLocale { position: usize },
    DescriptionTooLong { position: usize },
    DefaultLocaleMissing,
    InvalidOrigin,
    /// STU-IO-133: a shipped preset is read-only; edit a copy ([`PdfPreset::edit_copy`]).
    ShippedReadOnly,
    /// SD-13: the writer does not write this compatibility level (1.3).
    CompatibilityLevelNotWritable(CompatibilityLevel),
    /// SD-13: PDF/X-4 needs version 1.6 or later; no `compatibility_level` member reaches it.
    StandardLevelConflict(CompatibilityLevel),
    /// `standard_compliant_only` with a standard check the writer does not implement.
    StandardComplianceNotProvable,
    /// The caller supplied an output intent the preset does not declare (`output_intent_profile` empty).
    OutputIntentNotDeclared,
}
impl PresetError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::TooManySettings => "too_many_settings",
            Self::UnknownSetting { .. } => "unknown_setting",
            Self::DuplicateSetting(_) => "duplicate_setting",
            Self::PresetIncomplete(_) => "preset_incomplete",
            Self::WrongValueKind(_) => "wrong_value_kind",
            Self::UnknownMember(_) => "unknown_member",
            Self::NonFinite(_) => "non_finite",
            Self::OutsideHardBounds(_) => "outside_hard_bounds",
            Self::ListTooLong(_) => "list_too_long",
            Self::FontNameInvalid(_) => "font_name_invalid",
            Self::QualityDictionaryInvalid(_) => "quality_dictionary_invalid",
            Self::TextInvalid(_) => "text_invalid",
            Self::StandardWithoutOutputIntent => "standard_without_output_intent",
            Self::InvalidId => "invalid_id",
            Self::InvalidName => "invalid_name",
            Self::NoDescriptions => "no_descriptions",
            Self::TooManyLocales => "too_many_locales",
            Self::InvalidLocale { .. } => "invalid_locale",
            Self::DuplicateLocale { .. } => "duplicate_locale",
            Self::DescriptionTooLong { .. } => "description_too_long",
            Self::DefaultLocaleMissing => "default_locale_missing",
            Self::InvalidOrigin => "invalid_origin",
            Self::ShippedReadOnly => "shipped_read_only",
            Self::CompatibilityLevelNotWritable(_) => "compatibility_level_not_writable",
            Self::StandardLevelConflict(_) => "standard_level_conflict",
            Self::StandardComplianceNotProvable => "standard_compliance_not_provable",
            Self::OutputIntentNotDeclared => "output_intent_not_declared",
        }
    }
    /// The setting a refusal names, if any.
    pub const fn setting(&self) -> Option<SettingRef> {
        match self {
            Self::DuplicateSetting(r)
            | Self::PresetIncomplete(r)
            | Self::WrongValueKind(r)
            | Self::UnknownMember(r)
            | Self::NonFinite(r)
            | Self::OutsideHardBounds(r)
            | Self::ListTooLong(r)
            | Self::FontNameInvalid(r)
            | Self::QualityDictionaryInvalid(r)
            | Self::TextInvalid(r) => Some(*r),
            _ => None,
        }
    }
}
impl std::fmt::Display for PresetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.setting() {
            Some(r) => write!(f, "{}: {r}", self.code()),
            None => f.write_str(self.code()),
        }
    }
}
impl std::error::Error for PresetError {}

/// Maps a preset result onto the closed Observe outcome vocabulary (codes only, no text); emit it
/// with [`crate::report`]. The SD-13 and standards refusals are `Unsupported`, the rest `Validation`.
pub fn preset_outcome_for<T>(result: &Result<T, PresetError>) -> Outcome {
    match result {
        Ok(_) => Outcome::Success,
        Err(
            PresetError::CompatibilityLevelNotWritable(_)
            | PresetError::StandardLevelConflict(_)
            | PresetError::StandardComplianceNotProvable,
        ) => Outcome::Failure(FailureCode::Unsupported),
        Err(_) => Outcome::Failure(FailureCode::Validation),
    }
}

/// A typed warning: the value lies inside the hard bounds and outside a soft bound.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PresetWarning {
    pub setting: SettingRef,
    pub value: f64,
    pub soft_min: Bound,
    pub soft_max: Bound,
}
impl PresetWarning {
    pub const CODE: &'static str = "outside_soft_bounds";
}

/// A quality-dictionary value (input form).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum QualityInput<'a> {
    Number(f64),
    Integer(i64),
    Bool(bool),
    Name(&'a str),
    Numbers(&'a [f64]),
}
/// A stored quality-dictionary value.
#[derive(Clone, Debug, PartialEq)]
pub enum QualityValue {
    Number(f64),
    Integer(i64),
    Bool(bool),
    Name(String),
    Numbers(Vec<f64>),
}
/// One ordered quality-dictionary entry.
#[derive(Clone, Debug, PartialEq)]
pub struct QualityEntry {
    pub key: String,
    pub value: QualityValue,
}

/// A setting value as a draft or edit supplies it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SettingInput<'a> {
    Bool(bool),
    Integer(i64),
    Real(f64),
    Token(&'a str),
    Names(&'a [&'a str]),
    Quality(&'a [(&'a str, QualityInput<'a>)]),
    Text(&'a str),
}

/// A stored, validated setting value.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingValue {
    Bool(bool),
    Integer(i64),
    Real(f64),
    Enum(EnumValue),
    FontList(Vec<String>),
    Quality(Vec<QualityEntry>),
    Text(String),
}

/// Provenance as a draft supplies it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceInput<'a> {
    Shipped,
    Operator { parent: Option<&'a str> },
    Imported { origin: &'a str },
}
/// `preset_source` (STU-IO-133).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PresetSource {
    Shipped,
    Operator { parent: Option<String> },
    Imported { origin: String },
}
impl PresetSource {
    pub const fn token(&self) -> &'static str {
        match self {
            Self::Shipped => "shipped",
            Self::Operator { .. } => "operator",
            Self::Imported { .. } => "imported",
        }
    }
    pub const fn is_read_only(&self) -> bool {
        matches!(self, Self::Shipped)
    }
}

/// The input of [`PdfPreset::from_draft`]: borrowed and validated before anything is owned.
#[derive(Clone, Copy, Debug)]
pub struct PresetDraft<'a> {
    pub id: &'a str,
    pub name: &'a str,
    /// `(locale, text)` pairs.
    pub descriptions: &'a [(&'a str, &'a str)],
    /// The last step of the fallback chain; must be a key of `descriptions`.
    pub default_locale: &'a str,
    pub source: SourceInput<'a>,
    /// `(group.name, value)` for every setting of [`SETTINGS`].
    pub settings: &'a [(&'a str, SettingInput<'a>)],
}

/// Which step of [`DESCRIPTION_FALLBACK_CHAIN`] produced a rendered description.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallbackStep {
    RequestedLocale,
    Language,
    DefaultLocale,
}
/// A rendered description: the locale used, its text and the fallback step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderedDescription<'p> {
    pub locale: &'p str,
    pub text: &'p str,
    pub step: FallbackStep,
}

/// The locale-keyed description map (STU-IO-132), never one flattened string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Descriptions {
    entries: Vec<(String, String)>,
    default_locale: String,
}
/// Locale tags compare ASCII-case-insensitively with `_` equal to `-`.
fn locale_eq(a: &str, b: &str) -> bool {
    let norm = |c: u8| if c == b'_' { b'-' } else { c.to_ascii_lowercase() };
    a.len() == b.len() && a.bytes().zip(b.bytes()).all(|(x, y)| norm(x) == norm(y))
}
fn language(tag: &str) -> &str {
    tag.split(['-', '_']).next().unwrap_or(tag)
}
fn valid_locale(tag: &str) -> bool {
    if !(2..=MAX_LOCALE_BYTES).contains(&tag.len()) {
        return false;
    }
    let mut parts = tag.split(['-', '_']);
    let first_ok = parts.next().is_some_and(|p| (2..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphabetic()));
    first_ok && parts.all(|p| (1..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric()))
}
impl Descriptions {
    fn new(pairs: &[(&str, &str)], default_locale: &str) -> Result<Self, PresetError> {
        if pairs.is_empty() {
            return Err(PresetError::NoDescriptions);
        }
        if pairs.len() > MAX_LOCALES {
            return Err(PresetError::TooManyLocales);
        }
        for (i, (locale, text)) in pairs.iter().enumerate() {
            if !valid_locale(locale) {
                return Err(PresetError::InvalidLocale { position: i });
            }
            if text.len() > MAX_DESCRIPTION_BYTES {
                return Err(PresetError::DescriptionTooLong { position: i });
            }
            if pairs.iter().take(i).any(|(earlier, _)| locale_eq(earlier, locale)) {
                return Err(PresetError::DuplicateLocale { position: i });
            }
        }
        let Some((default, _)) = pairs.iter().find(|(l, _)| locale_eq(l, default_locale)) else {
            return Err(PresetError::DefaultLocaleMissing);
        };
        Ok(Self {
            entries: pairs.iter().map(|(l, t)| ((*l).to_owned(), (*t).to_owned())).collect(),
            default_locale: (*default).to_owned(),
        })
    }
    pub fn default_locale(&self) -> &str {
        &self.default_locale
    }
    pub fn entries(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(l, t)| (l.as_str(), t.as_str()))
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn get(&self, locale: &str) -> Option<&str> {
        self.entries.iter().find(|(l, _)| locale_eq(l, locale)).map(|(_, t)| t.as_str())
    }
    /// Renders `requested` through [`DESCRIPTION_FALLBACK_CHAIN`]: the requested locale, then its
    /// language (the bare language tag, else the first entry in map order with that language),
    /// then the declared default locale.
    pub fn render(&self, requested: &str) -> RenderedDescription<'_> {
        fn hit(step: FallbackStep, (l, t): &(String, String)) -> RenderedDescription<'_> {
            RenderedDescription { locale: l.as_str(), text: t.as_str(), step }
        }
        if let Some(e) = self.entries.iter().find(|(l, _)| locale_eq(l, requested)) {
            return hit(FallbackStep::RequestedLocale, e);
        }
        let lang = language(requested);
        if valid_locale(lang) {
            let bare = self.entries.iter().find(|(l, _)| locale_eq(l, lang));
            if let Some(e) = bare.or_else(|| self.entries.iter().find(|(l, _)| language(l).eq_ignore_ascii_case(lang))) {
                return hit(FallbackStep::Language, e);
            }
        }
        let default = self.entries.iter().find(|(l, _)| *l == self.default_locale);
        match default {
            Some(e) => hit(FallbackStep::DefaultLocale, e),
            None => unreachable!("the default locale is a key of the map by construction"),
        }
    }
}

/// A complete, typed PDF export preset.
#[derive(Clone, Debug, PartialEq)]
pub struct PdfPreset {
    id: String,
    name: String,
    descriptions: Descriptions,
    source: PresetSource,
    /// One value per [`SETTINGS`] entry, same order.
    values: Vec<SettingValue>,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_PRESET_ID_BYTES && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}
fn valid_text(t: &str, max: usize) -> bool {
    t.len() <= max && !t.chars().any(char::is_control)
}
fn valid_name(t: &str, max: usize) -> bool {
    !t.is_empty() && valid_text(t, max)
}

fn parse_value(spec: &SettingSpec, input: SettingInput<'_>) -> Result<SettingValue, PresetError> {
    let r = spec.setting();
    let check = |c: &NumericContract, v: f64| match c.fit(v) {
        Fit::OutsideHard => Err(PresetError::OutsideHardBounds(r)),
        Fit::Inside | Fit::OutsideSoft => Ok(()),
    };
    match (spec.kind, input) {
        (SettingKind::Bool, SettingInput::Bool(b)) => Ok(SettingValue::Bool(b)),
        (SettingKind::Integer(c), SettingInput::Integer(n)) => {
            check(c, n as f64)?;
            Ok(SettingValue::Integer(n))
        }
        (SettingKind::Real(c), SettingInput::Real(x)) => {
            if !x.is_finite() {
                return Err(PresetError::NonFinite(r));
            }
            check(c, x)?;
            Ok(SettingValue::Real(x))
        }
        (SettingKind::Real(c), SettingInput::Integer(n)) => {
            check(c, n as f64)?;
            Ok(SettingValue::Real(n as f64))
        }
        (SettingKind::Enum(d), SettingInput::Token(t)) => d.parse(t).map(SettingValue::Enum).ok_or(PresetError::UnknownMember(r)),
        (SettingKind::Enum(EnumDomain::OverprintMode), SettingInput::Integer(n)) => match n {
            1 => Ok(SettingValue::Enum(EnumValue::OverprintMode(OverprintMode::One))),
            _ => Err(PresetError::UnknownMember(r)),
        },
        (SettingKind::FontList, SettingInput::Names(names)) => {
            if names.len() > MAX_FONT_LIST {
                return Err(PresetError::ListTooLong(r));
            }
            if !names.iter().all(|n| valid_name(n, MAX_FONT_NAME_BYTES)) {
                return Err(PresetError::FontNameInvalid(r));
            }
            Ok(SettingValue::FontList(names.iter().map(|n| (*n).to_owned()).collect()))
        }
        (SettingKind::QualityDict, SettingInput::Quality(entries)) => {
            if entries.len() > MAX_QUALITY_ENTRIES {
                return Err(PresetError::ListTooLong(r));
            }
            let key_ok = |k: &str| !k.is_empty() && k.len() <= MAX_QUALITY_KEY_BYTES && k.bytes().all(|b| b.is_ascii_graphic());
            let value_ok = |v: &QualityInput<'_>| match v {
                QualityInput::Number(x) => x.is_finite(),
                QualityInput::Integer(_) | QualityInput::Bool(_) => true,
                QualityInput::Name(n) => key_ok(n),
                QualityInput::Numbers(xs) => xs.len() <= MAX_QUALITY_NUMBERS && xs.iter().all(|x| x.is_finite()),
            };
            for (i, (k, v)) in entries.iter().enumerate() {
                if !key_ok(k) || !value_ok(v) || entries.iter().take(i).any(|(e, _)| e == k) {
                    return Err(PresetError::QualityDictionaryInvalid(r));
                }
            }
            Ok(SettingValue::Quality(
                entries
                    .iter()
                    .map(|(k, v)| QualityEntry {
                        key: (*k).to_owned(),
                        value: match v {
                            QualityInput::Number(x) => QualityValue::Number(*x),
                            QualityInput::Integer(n) => QualityValue::Integer(*n),
                            QualityInput::Bool(b) => QualityValue::Bool(*b),
                            QualityInput::Name(n) => QualityValue::Name((*n).to_owned()),
                            QualityInput::Numbers(xs) => QualityValue::Numbers(xs.to_vec()),
                        },
                    })
                    .collect(),
            ))
        }
        (SettingKind::Text, SettingInput::Text(t)) => {
            if !valid_text(t, MAX_TEXT_BYTES) {
                return Err(PresetError::TextInvalid(r));
            }
            Ok(SettingValue::Text(t.to_owned()))
        }
        _ => Err(PresetError::WrongValueKind(r)),
    }
}

fn soft_warning(spec: &SettingSpec, value: &SettingValue) -> Option<PresetWarning> {
    let (c, v) = match (spec.kind, value) {
        (SettingKind::Integer(c), SettingValue::Integer(n)) => (c, *n as f64),
        (SettingKind::Real(c), SettingValue::Real(x)) => (c, *x),
        _ => return None,
    };
    match c.fit(v) {
        Fit::OutsideSoft => Some(PresetWarning { setting: spec.setting(), value: v, soft_min: c.soft_min, soft_max: c.soft_max }),
        Fit::Inside | Fit::OutsideHard => None,
    }
}

/// The font settings of STU-IO-131, typed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FontSettings<'p> {
    pub embed_all_fonts: bool,
    pub subset_fonts: bool,
    pub max_subset_percent: i64,
    pub embed_open_type: bool,
    pub always_embed: &'p [String],
    pub never_embed: &'p [String],
    pub cannot_embed_font_policy: CannotEmbedFontPolicy,
}
/// One image group's STU-IO-127 family, typed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageFamily<'p> {
    pub anti_alias: bool,
    pub crop_to_frame: bool,
    pub minimum_resolution: i64,
    pub minimum_resolution_policy: MinimumResolutionPolicy,
    pub downsample: bool,
    pub downsample_type: DownsampleType,
    pub target_resolution: i64,
    pub bit_depth: i64,
    pub minimum_downsample_depth: i64,
    pub downsample_threshold: f64,
    pub encode: bool,
    pub filter: ImageFilter,
    pub auto_filter: bool,
    pub auto_filter_strategy: AutoFilterStrategy,
    pub base_quality_dictionary: &'p [QualityEntry],
    pub auto_colour_space_quality_dictionary: &'p [QualityEntry],
}
/// The standards-compliance settings of STU-IO-130, typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StandardsSettings<'p> {
    pub standard_check_1a: bool,
    pub standard_check_3: bool,
    pub standard_compliant_only: bool,
    pub output_intent_profile: &'p str,
}
impl StandardsSettings<'_> {
    /// Whether the preset asserts a standard (a standard check is on).
    pub const fn asserts_standard(&self) -> bool {
        self.standard_check_1a || self.standard_check_3
    }
}

macro_rules! enum_at {
    ($self:ident, $g:expr, $n:literal, $t:ident) => {
        match $self.at($g, $n) {
            SettingValue::Enum(EnumValue::$t(v)) => *v,
            _ => unreachable!("typed by construction"),
        }
    };
}

/// What the caller adds at resolution: values a preset does not hold (profile bytes, dates, title)
/// and the standards identification it requests from the writer.
#[derive(Clone, Copy, Debug, Default)]
pub struct ResolveRequest<'a> {
    pub output_intent: Option<OutputIntent<'a>>,
    pub info_title: Option<&'a str>,
    pub pdfx4: Option<PdfX4<'a>>,
    pub pdfa2b: bool,
}

/// Whether the exporter honours a setting. `NotApplied` carries the reason code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingStatus {
    Applied,
    NotApplied { code: &'static str },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingReceipt {
    pub setting: SettingRef,
    pub status: SettingStatus,
}
/// Every setting, applied or not, plus the soft-bound warnings. Conformance stays `not_proven`.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolveReceipt {
    pub preset_id: String,
    pub entries: Vec<SettingReceipt>,
    pub warnings: Vec<PresetWarning>,
    pub conformance: &'static str,
}
impl ResolveReceipt {
    pub fn status(&self, key: &str) -> Option<SettingStatus> {
        let i = key_position(key)?;
        self.entries.get(i).map(|e| e.status)
    }
}
/// A preset resolved onto the writer's options.
#[derive(Clone, Debug)]
pub struct Resolution<'a> {
    pub options: ExportOptions<'a>,
    pub receipt: ResolveReceipt,
}

/// The receipt of a font the writer cannot embed (STU-IO-131, STU-IO-012).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontReceipt {
    pub font: String,
    pub policy: CannotEmbedFontPolicy,
    /// The writer's refusal code (`font_cff_not_embedded`, `font_embedding_restricted`, ...).
    pub cause: &'static str,
    /// Always `export_refused`: the writer has no path that omits the font.
    pub outcome: &'static str,
    /// Always [`FONT_GAP`].
    pub gap: &'static str,
}

const fn when(applied: bool, code: &'static str) -> SettingStatus {
    if applied {
        SettingStatus::Applied
    } else {
        SettingStatus::NotApplied { code }
    }
}
const fn not_applied(code: &'static str) -> SettingStatus {
    SettingStatus::NotApplied { code }
}

impl PdfPreset {
    /// Builds a complete preset. Refuses typed: bounds first (before allocation), then identity,
    /// descriptions and provenance, then each setting in draft order, then the first missing setting
    /// in table order, then the STU-IO-130 standards rule.
    pub fn from_draft(draft: &PresetDraft<'_>) -> Result<Self, PresetError> {
        if draft.settings.len() > MAX_PRESET_ENTRIES {
            return Err(PresetError::TooManySettings);
        }
        if draft.descriptions.len() > MAX_LOCALES {
            return Err(PresetError::TooManyLocales);
        }
        if !valid_id(draft.id) {
            return Err(PresetError::InvalidId);
        }
        if !valid_name(draft.name, MAX_PRESET_NAME_BYTES) {
            return Err(PresetError::InvalidName);
        }
        let source = match draft.source {
            SourceInput::Shipped => PresetSource::Shipped,
            SourceInput::Operator { parent: None } => PresetSource::Operator { parent: None },
            SourceInput::Operator { parent: Some(p) } => {
                if !valid_id(p) {
                    return Err(PresetError::InvalidId);
                }
                PresetSource::Operator { parent: Some(p.to_owned()) }
            }
            SourceInput::Imported { origin } => {
                if !valid_name(origin, MAX_TEXT_BYTES) {
                    return Err(PresetError::InvalidOrigin);
                }
                PresetSource::Imported { origin: origin.to_owned() }
            }
        };
        let descriptions = Descriptions::new(draft.descriptions, draft.default_locale)?;
        let mut slots: Vec<Option<SettingValue>> = vec![None; SETTINGS.len()];
        for (pos, (key, input)) in draft.settings.iter().enumerate() {
            let Some(i) = key_position(key) else {
                return Err(PresetError::UnknownSetting { position: pos });
            };
            let (Some(spec), Some(slot)) = (SETTINGS.get(i), slots.get_mut(i)) else {
                return Err(PresetError::UnknownSetting { position: pos });
            };
            if slot.is_some() {
                return Err(PresetError::DuplicateSetting(spec.setting()));
            }
            *slot = Some(parse_value(spec, *input)?);
        }
        let mut values = Vec::with_capacity(SETTINGS.len());
        for (spec, slot) in SETTINGS.iter().zip(slots) {
            match slot {
                Some(v) => values.push(v),
                None => return Err(PresetError::PresetIncomplete(spec.setting())),
            }
        }
        let preset = Self { id: draft.id.to_owned(), name: draft.name.to_owned(), descriptions, source, values };
        preset.check_standards()?;
        Ok(preset)
    }

    fn check_standards(&self) -> Result<(), PresetError> {
        let s = self.standards();
        if s.asserts_standard() && s.output_intent_profile.is_empty() {
            return Err(PresetError::StandardWithoutOutputIntent);
        }
        Ok(())
    }

    fn at(&self, group: PdfGroup, name: &str) -> &SettingValue {
        match position(group, name).and_then(|i| self.values.get(i)) {
            Some(v) => v,
            None => unreachable!("every table setting has a value by construction"),
        }
    }
    fn bool_at(&self, group: PdfGroup, name: &str) -> bool {
        match self.at(group, name) {
            SettingValue::Bool(b) => *b,
            _ => unreachable!("typed by construction"),
        }
    }
    fn int_at(&self, group: PdfGroup, name: &str) -> i64 {
        match self.at(group, name) {
            SettingValue::Integer(n) => *n,
            _ => unreachable!("typed by construction"),
        }
    }
    fn real_at(&self, group: PdfGroup, name: &str) -> f64 {
        match self.at(group, name) {
            SettingValue::Real(x) => *x,
            _ => unreachable!("typed by construction"),
        }
    }
    fn fonts_at(&self, name: &str) -> &[String] {
        match self.at(PdfGroup::Fonts, name) {
            SettingValue::FontList(l) => l,
            _ => unreachable!("typed by construction"),
        }
    }
    fn quality_at(&self, group: PdfGroup, name: &str) -> &[QualityEntry] {
        match self.at(group, name) {
            SettingValue::Quality(q) => q,
            _ => unreachable!("typed by construction"),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn descriptions(&self) -> &Descriptions {
        &self.descriptions
    }
    pub fn source(&self) -> &PresetSource {
        &self.source
    }
    /// The stored value of `group.name`.
    pub fn value(&self, key: &str) -> Option<&SettingValue> {
        self.values.get(key_position(key)?)
    }
    /// Every setting with its value, in table order.
    pub fn settings(&self) -> impl Iterator<Item = (&'static SettingSpec, &SettingValue)> {
        SETTINGS.iter().zip(self.values.iter())
    }
    pub fn compatibility_level(&self) -> CompatibilityLevel {
        enum_at!(self, PdfGroup::General, "compatibility_level", CompatibilityLevel)
    }
    pub fn fonts(&self) -> FontSettings<'_> {
        FontSettings {
            embed_all_fonts: self.bool_at(PdfGroup::Fonts, "embed_all_fonts"),
            subset_fonts: self.bool_at(PdfGroup::Fonts, "subset_fonts"),
            max_subset_percent: self.int_at(PdfGroup::Fonts, "max_subset_percent"),
            embed_open_type: self.bool_at(PdfGroup::Fonts, "embed_open_type"),
            always_embed: self.fonts_at("always_embed"),
            never_embed: self.fonts_at("never_embed"),
            cannot_embed_font_policy: enum_at!(self, PdfGroup::Fonts, "cannot_embed_font_policy", CannotEmbedFontPolicy),
        }
    }
    /// The STU-IO-127 family of an image group; `None` for a group that is not an image group.
    pub fn image_family(&self, group: PdfGroup) -> Option<ImageFamily<'_>> {
        if !group.is_image() {
            return None;
        }
        Some(ImageFamily {
            anti_alias: self.bool_at(group, "anti_alias"),
            crop_to_frame: self.bool_at(group, "crop_to_frame"),
            minimum_resolution: self.int_at(group, "minimum_resolution"),
            minimum_resolution_policy: enum_at!(self, group, "minimum_resolution_policy", MinimumResolutionPolicy),
            downsample: self.bool_at(group, "downsample"),
            downsample_type: enum_at!(self, group, "downsample_type", DownsampleType),
            target_resolution: self.int_at(group, "target_resolution"),
            bit_depth: self.int_at(group, "bit_depth"),
            minimum_downsample_depth: self.int_at(group, "minimum_downsample_depth"),
            downsample_threshold: self.real_at(group, "downsample_threshold"),
            encode: self.bool_at(group, "encode"),
            filter: enum_at!(self, group, "filter", ImageFilter),
            auto_filter: self.bool_at(group, "auto_filter"),
            auto_filter_strategy: enum_at!(self, group, "auto_filter_strategy", AutoFilterStrategy),
            base_quality_dictionary: self.quality_at(group, "base_quality_dictionary"),
            auto_colour_space_quality_dictionary: self.quality_at(group, "auto_colour_space_quality_dictionary"),
        })
    }
    pub fn standards(&self) -> StandardsSettings<'_> {
        let profile = match self.at(PdfGroup::OutputIntent, "output_intent_profile") {
            SettingValue::Text(t) => t.as_str(),
            _ => unreachable!("typed by construction"),
        };
        StandardsSettings {
            standard_check_1a: self.bool_at(PdfGroup::StandardsCompliance, "standard_check_1a"),
            standard_check_3: self.bool_at(PdfGroup::StandardsCompliance, "standard_check_3"),
            standard_compliant_only: self.bool_at(PdfGroup::StandardsCompliance, "standard_compliant_only"),
            output_intent_profile: profile,
        }
    }
    /// Every numeric setting inside its hard bounds and outside a soft bound.
    pub fn warnings(&self) -> Vec<PresetWarning> {
        self.settings().filter_map(|(spec, v)| soft_warning(spec, v)).collect()
    }

    /// Changes one setting of an operator or imported preset. A shipped preset refuses
    /// `shipped_read_only`. On any refusal the preset is unchanged.
    pub fn set(&mut self, key: &str, input: SettingInput<'_>) -> Result<Option<PresetWarning>, PresetError> {
        if self.source.is_read_only() {
            return Err(PresetError::ShippedReadOnly);
        }
        let i = key_position(key).ok_or(PresetError::UnknownSetting { position: 0 })?;
        let spec = SETTINGS.get(i).ok_or(PresetError::UnknownSetting { position: 0 })?;
        let value = parse_value(spec, input)?;
        let warning = soft_warning(spec, &value);
        let Some(slot) = self.values.get_mut(i) else {
            return Err(PresetError::UnknownSetting { position: 0 });
        };
        let previous = std::mem::replace(slot, value);
        if let Err(e) = self.check_standards() {
            if let Some(slot) = self.values.get_mut(i) {
                *slot = previous;
            }
            return Err(e);
        }
        Ok(warning)
    }

    /// An operator copy of this preset with `parent` = this preset's id (STU-IO-133). This is how a
    /// shipped preset is edited; nothing is persisted here.
    pub fn edit_copy(&self, id: &str, name: &str) -> Result<Self, PresetError> {
        if !valid_id(id) {
            return Err(PresetError::InvalidId);
        }
        if !valid_name(name, MAX_PRESET_NAME_BYTES) {
            return Err(PresetError::InvalidName);
        }
        Ok(Self {
            id: id.to_owned(),
            name: name.to_owned(),
            descriptions: self.descriptions.clone(),
            source: PresetSource::Operator { parent: Some(self.id.clone()) },
            values: self.values.clone(),
        })
    }

    /// The receipt of a font the writer refused to embed, naming the font and the policy applied;
    /// `Ok(None)` when `error` is not a font-embedding refusal. Under every policy the outcome is the
    /// export refusal: the writer cannot write output without the font ([`FONT_GAP`]).
    pub fn font_receipt(&self, font: &str, error: &ExportError) -> Result<Option<FontReceipt>, PresetError> {
        if !valid_name(font, MAX_FONT_NAME_BYTES) {
            return Err(PresetError::FontNameInvalid(SettingRef { group: PdfGroup::Fonts, name: "cannot_embed_font_policy" }));
        }
        let cause = match error {
            ExportError::FontUnreadable
            | ExportError::FontCffNotEmbedded
            | ExportError::FontCollectionNotEmbedded
            | ExportError::FontEmbeddingRestricted => error.code(),
            _ => return Ok(None),
        };
        Ok(Some(FontReceipt {
            font: font.to_owned(),
            policy: self.fonts().cannot_embed_font_policy,
            cause,
            outcome: "export_refused",
            gap: FONT_GAP,
        }))
    }

    /// Resolves the preset onto press-write [`ExportOptions`]: every option field comes from the
    /// preset or `request` (never from `ExportOptions::default`), and the receipt lists every
    /// setting as applied or not applied with a code.
    ///
    /// Refusals, in order: `compatibility_level_not_writable` (1.3, SD-13), `standard_level_conflict`
    /// (PDF/X-4 requested; it needs 1.6+, SD-13), `standard_compliance_not_provable`
    /// (`standard_compliant_only` with a standard check the writer does not implement),
    /// `output_intent_not_declared` (an intent supplied while `output_intent_profile` is empty).
    pub fn resolve<'a>(&self, request: ResolveRequest<'a>) -> Result<Resolution<'a>, PresetError> {
        let level = self.compatibility_level();
        let version = match level {
            CompatibilityLevel::V1_3 => return Err(PresetError::CompatibilityLevelNotWritable(level)),
            CompatibilityLevel::V1_4 => "1.4",
            CompatibilityLevel::V1_5 => "1.5",
        };
        if request.pdfx4.is_some() {
            return Err(PresetError::StandardLevelConflict(level));
        }
        let standards = self.standards();
        if standards.standard_compliant_only && standards.asserts_standard() {
            return Err(PresetError::StandardComplianceNotProvable);
        }
        if standards.output_intent_profile.is_empty() && request.output_intent.is_some() {
            return Err(PresetError::OutputIntentNotDeclared);
        }
        let entries = self
            .settings()
            .map(|(spec, value)| SettingReceipt { setting: spec.setting(), status: self.status(spec, value, &request) })
            .collect();
        let options = ExportOptions {
            version,
            output_intent: request.output_intent,
            info_title: request.info_title,
            subset_fonts: self.fonts().subset_fonts,
            pdfx4: None,
            pdfa2b: request.pdfa2b,
        };
        let receipt = ResolveReceipt { preset_id: self.id.clone(), entries, warnings: self.warnings(), conformance: "not_proven" };
        Ok(Resolution { options, receipt })
    }

    fn status(&self, spec: &SettingSpec, value: &SettingValue, request: &ResolveRequest<'_>) -> SettingStatus {
        use SettingValue as Sv;
        let flag = |v: &SettingValue| matches!(v, Sv::Bool(true));
        if spec.group.is_image() {
            return match spec.name {
                "anti_alias" => when(!flag(value), "resampling_not_implemented"),
                "crop_to_frame" => when(!flag(value), "image_crop_not_implemented"),
                "minimum_resolution" | "minimum_resolution_policy" => not_applied("resolution_check_not_implemented"),
                "downsample" => when(!flag(value), "downsampling_not_implemented"),
                "downsample_type" => {
                    when(matches!(value, Sv::Enum(EnumValue::DownsampleType(DownsampleType::None))), "downsampling_not_implemented")
                }
                "bit_depth" => when(matches!(value, Sv::Integer(-1)), "bit_depth_conversion_not_implemented"),
                "encode" => when(flag(value), "writer_always_encodes"),
                "filter" => when(matches!(value, Sv::Enum(EnumValue::ImageFilter(ImageFilter::FlateEncode))), "recompression_not_implemented"),
                "auto_filter" => when(!flag(value), "recompression_not_implemented"),
                "auto_filter_strategy" | "base_quality_dictionary" | "auto_colour_space_quality_dictionary" => {
                    not_applied("recompression_not_implemented")
                }
                _ => not_applied("downsampling_not_implemented"),
            };
        }
        match spec.name {
            "compatibility_level" | "subset_fonts" | "always_embed" | "default_rendering_intent" => SettingStatus::Applied,
            "start_page" | "end_page" => not_applied("page_range_owned_by_caller"),
            "thumbnails" => when(!flag(value), "thumbnails_not_implemented"),
            "optimisation" => when(!flag(value), "optimisation_not_implemented"),
            "binding" => not_applied("binding_not_written"),
            "auto_rotate_pages" => {
                when(matches!(value, Sv::Enum(EnumValue::AutoRotatePages(AutoRotatePages::None))), "auto_rotate_not_implemented")
            }
            "compress_objects" => {
                when(matches!(value, Sv::Enum(EnumValue::CompressObjects(CompressObjects::Off))), "object_compression_not_implemented")
            }
            "image_memory" => not_applied("interpreter_setting_not_implemented"),
            "embed_all_fonts" | "embed_open_type" => when(flag(value), "writer_embeds_every_font"),
            "max_subset_percent" => not_applied("subset_threshold_not_implemented"),
            "never_embed" => when(matches!(value, Sv::FontList(l) if l.is_empty()), "writer_embeds_every_font"),
            "cannot_embed_font_policy" => when(
                matches!(value, Sv::Enum(EnumValue::CannotEmbedFontPolicy(CannotEmbedFontPolicy::Error))),
                "writer_refuses_unembeddable_font",
            ),
            "colour_conversion_strategy" => when(
                matches!(value, Sv::Enum(EnumValue::ColourConversionStrategy(ColourConversionStrategy::LeaveColorUnchanged))),
                "colour_conversion_not_implemented",
            ),
            "transfer_function_info" | "ucr_and_black_generation" => not_applied("postscript_colour_setting_not_implemented"),
            "overprint_mode" => not_applied("overprint_not_implemented"),
            "output_intent_profile" => {
                let declared = matches!(value, Sv::Text(t) if !t.is_empty());
                when(!declared || request.output_intent.is_some(), "output_intent_not_supplied")
            }
            "standard_check_1a" | "standard_check_3" => when(!flag(value), "standard_check_not_implemented"),
            "standard_compliant_only" => SettingStatus::Applied,
            _ => not_applied("postscript_setting_not_implemented"),
        }
    }
}
