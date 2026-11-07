//! `StudioExportRecipe` (lane SMB-PR-EXPORT-RECIPE-1; STU-IO-101, 103, 108, 141..147, 175..185, STU-VID-060,
//! 062, 063): every output is a typed parameter set over the export parameter dictionary
//! ([`crate::export_parameters`]), and reel's export path consumes it ([`job_from_recipe`],
//! [`StudioExportRecipe::resolve`]). A shipped preset is the same record as a user recipe
//! ([`crate::presets::ExportPreset::studio_recipe`]).
//!
//! The record: `recipe_id`, `name`, `target_format` (the exporter identity of STU-IO-141 / STU-IO-108:
//! `(exporter_class_id, exporter_file_type)`; reel's own is [`MUXER_ID`] and the container token),
//! `parameters` (dictionary identifier to typed value, ordered by identifier bytes), `source_scope`,
//! `output_targets`, `does_video` / `does_audio`, `metadata_export_option`, `standard_filters`, plus reel's
//! typed codec selection ([`RecipeVideoCodec`], [`PcmChoice`]): the dictionary's codec integers are
//! exporter-local (STU-IO-108) and carry no Studio meaning, so reel does not read them.
//!
//! Every parameter is checked when it is set, never clamped ([`StudioExportRecipe::set_parameter`]):
//! unknown identifiers (not echoed), credential rows (STU-IO-147 / 185: named by field, never by value),
//! structure rows that store no value (type codes 1, 7, 8, 11), the value kind, non-finite numbers and
//! `-0.0`, declared usable hard bounds (`EFFECT_PARAM_OUT_OF_RANGE` naming the identifier, the value and both
//! bounds, STU-FX-104; one-sided rows enforce their known side; rows with no declared bound accept any value
//! unclamped, STU-IO-103; [`crate::export_parameters::DEGENERATE_BOUNDS`] rows are not range-enforced),
//! codec integers without an exporter identity (STU-IO-177), frame rate as anything but integer ticks
//! (STU-IO-178), and numeric values whose unit is undeclared and not declared by reel's writer binding
//! (STU-IO-175). Tests: tests/export_recipe.rs.
//!
//! Consumption ([`REEL_WRITER_BINDING`]): a closed list of six identifiers, each with the unit Studio declares
//! for it (`studio_declared`, recorded here and not in the dictionary row). Any other parameter, and any
//! record field the writer does not consume (`output_targets`, `standard_filters`), is a typed
//! `export_parameter_not_consumed` before the first frame, never ignored (STU-VID-062). An H.264 / HEVC
//! recipe without `ADBEVideoTargetBitrate` is `export_preset_incomplete` (STU-IO-179). The six shipped
//! presets are complete against these six consumed identifiers only, not against the 453.
//!
//! Hostile-input bounds, checked before anything is stored (V4-01/02; lead-overrulable): identifier <= 128
//! bytes, parameter count <= 453, text and path values <= 4096 bytes, `name` <= 256 bytes, `recipe_id` 1..=128
//! printable ASCII, exporter ids 1..=64 printable ASCII, `output_targets` <= 64 of 1..=255 bytes with no
//! `/`, `\`, `:`, NUL or `..` and not `.`, `standard_filters` <= 64 of 1..=256 bytes. Lookup is a binary
//! search. [`StudioExportRecipe::to_json`] is deterministic and hand-written (no clock, random id or locale,
//! STU-IO-163). Persistence is the host's (STU-IO-166); no JSON parser here.
#![deny(clippy::arithmetic_side_effects)]

use crate::error::ReelError;
use crate::export::{
    Av1Settings, ExportAudioSource, ExportJob, ExportLimits, ExportRecipe, ExportVideoSource, PcmChoice,
    SourceSpan, VideoCodecChoice,
};
use crate::export_parameters::{
    CODEC_SELECTION, CREDENTIAL_OUTSIDE_PUBLISHING, DeclaredValue, EXPORT_PARAMETER_COUNT, ExportFacet,
    ExportParameter, ExportTypeCode, ExportUnit, export_parameter,
};
use crate::mux::{Container, MUXER_ID};
use crate::os_encode::{H264Profile, OsVideoSettings};
use crate::presets::{ExportPreset, PresetVideo};
use crate::vc3::encode::Vc3Profile;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const MAX_IDENTIFIER_BYTES: usize = 128;
pub const MAX_TEXT_BYTES: usize = 4096;
pub const MAX_NAME_BYTES: usize = 256;
pub const MAX_RECIPE_ID_BYTES: usize = 128;
pub const MAX_EXPORTER_ID_BYTES: usize = 64;
pub const MAX_OUTPUT_TARGETS: usize = 64;
pub const MAX_OUTPUT_TARGET_BYTES: usize = 255;
pub const MAX_STANDARD_FILTERS: usize = 64;
pub const MAX_STANDARD_FILTER_BYTES: usize = 256;

const WIDTH: &str = "ADBEVideoWidth";
const HEIGHT: &str = "ADBEVideoHeight";
const FPS: &str = "ADBEVideoFPS";
const RATE: &str = "ADBEAudioRatePerSecond";
const CHANNELS: &str = "ADBEAudioNumChannels";
const BITRATE: &str = "ADBEVideoTargetBitrate";

/// A dictionary identifier reel's writer consumes, with the unit Studio declares for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsumedParameter {
    pub identifier: &'static str,
    /// `studio_declared` unit (the dictionary row's unit stays undeclared).
    pub studio_declared_unit: &'static str,
    pub consumed_as: &'static str,
}

/// Reel's writer binding: the closed list of consumed identifiers, sorted by identifier.
pub const REEL_WRITER_BINDING: [ConsumedParameter; 6] = [
    ConsumedParameter {
        identifier: CHANNELS,
        studio_declared_unit: "channels",
        consumed_as: "equals the audio source's channels(); a negative value is refused, its meaning is not established",
    },
    ConsumedParameter { identifier: RATE, studio_declared_unit: "hertz", consumed_as: "equals the audio source's rate()" },
    ConsumedParameter {
        identifier: FPS,
        studio_declared_unit: "ticks_per_frame",
        consumed_as: "equals the video source's ticks_per_frame() (STU-IO-142, tick base 254016000000 per second)",
    },
    ConsumedParameter { identifier: HEIGHT, studio_declared_unit: "pixels", consumed_as: "equals the video source's format().height" },
    ConsumedParameter {
        identifier: BITRATE,
        studio_declared_unit: "megabits_per_second",
        consumed_as: "H.264 / HEVC OsVideoSettings.bitrate in bits per second (STU-IO-143)",
    },
    ConsumedParameter { identifier: WIDTH, studio_declared_unit: "pixels", consumed_as: "equals the video source's format().width" },
];

/// The unit reel's writer binding declares for `identifier`, if it consumes it.
pub fn studio_declared_unit(identifier: &str) -> Option<&'static str> {
    REEL_WRITER_BINDING.iter().find(|c| c.identifier == identifier).map(|c| c.studio_declared_unit)
}

/// A typed parameter value.
#[derive(Debug, Clone, PartialEq)]
pub enum ExportValue {
    Integer(i64),
    Float(f64),
    Bool(bool),
    Text(String),
}

/// The exporter identity of a recipe (STU-IO-141): `(exporter_class_id, exporter_file_type)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportTargetFormat {
    pub exporter_class_id: String,
    pub exporter_file_type: String,
}

impl ExportTargetFormat {
    /// Reel's own exporter for `container`: class id [`MUXER_ID`], file type [`Container::as_str`].
    pub fn reel(container: Container) -> Self {
        Self { exporter_class_id: MUXER_ID.to_owned(), exporter_file_type: container.as_str().to_owned() }
    }
}

/// The part of the source a recipe exports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceScope {
    /// The span the host gives the job.
    EntireSource,
    /// Exactly this span; a job over another span is refused.
    Span(SourceSpan),
}

/// Metadata export of a recipe. Reel's writer exports no metadata; other options are not modelled (no spec
/// member list; vendor options UNVERIFIED, D5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataExportOption {
    None,
}

/// Reel's typed video codec selection. H.264 / HEVC take their bitrate from `ADBEVideoTargetBitrate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecipeVideoCodec {
    Vc3(Vc3Profile),
    Av1(Av1Settings),
    H264(H264Profile),
    /// HEVC Main (the OS encoder ignores the H.264 profile field for HEVC, `os_encode`).
    Hevc,
}

/// A typed refusal of a recipe or of its consumption; `code()` is the stable token.
#[derive(Debug, Clone, PartialEq)]
pub enum ExportRecipeError {
    /// STU-FX-104: outside a declared usable hard bound (both bounds as declared).
    OutOfRange { identifier: &'static str, value: String, hard_min: DeclaredValue, hard_max: DeclaredValue },
    NonFinite { identifier: &'static str },
    NegativeZero { identifier: &'static str },
    WrongKind { identifier: &'static str, expected: &'static str },
    /// Type codes 1, 7, 8, 11 store no value (STU-IO-104).
    TakesNoValue { identifier: &'static str },
    /// A colour value needs a `StudioColorProfile` reference, which this record does not model.
    KindUnsupported { identifier: &'static str },
    /// STU-IO-175.
    MissingUnit { identifier: &'static str },
    /// STU-IO-177.
    CodecIntegerWithoutExporter { recipe_id: String, identifier: &'static str },
    /// STU-IO-178.
    FrameRateStoredAsFloat { recipe_id: String, identifier: &'static str },
    /// STU-IO-185: names the recipe and the field, never the value.
    CredentialInRecipe { recipe_id: String, field: &'static str },
    /// An identifier that is not in the dictionary or longer than 128 bytes; never echoed.
    UnknownParameter { reason: &'static str },
    Limit { field: &'static str, limit: usize },
    InvalidField { field: &'static str },
    /// STU-IO-108 / 141: fail closed naming the exporter.
    ExporterUnavailable { exporter: String },
    /// A valid parameter or record field reel's writer does not consume (STU-VID-062).
    NotConsumed { identifier: &'static str },
    /// STU-IO-179.
    PresetIncomplete { recipe_id: String, missing: &'static str },
    SourceMismatch { identifier: &'static str, declared: String, source: String },
    MeaningUnestablished { identifier: &'static str, value: i64 },
    Conversion { identifier: &'static str },
    /// A refusal of reel's writer (`MEDIA_CODEC_UNAVAILABLE` and the like).
    Writer(ReelError),
}

impl ExportRecipeError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::OutOfRange { .. } => "EFFECT_PARAM_OUT_OF_RANGE",
            Self::NonFinite { .. } => "export_parameter_non_finite",
            Self::NegativeZero { .. } => "export_parameter_negative_zero",
            Self::WrongKind { .. } => "export_parameter_wrong_kind",
            Self::TakesNoValue { .. } => "export_parameter_takes_no_value",
            Self::KindUnsupported { .. } => "export_parameter_kind_unsupported",
            Self::MissingUnit { .. } => "export_parameter_missing_unit",
            Self::CodecIntegerWithoutExporter { .. } => "export_codec_integer_without_exporter",
            Self::FrameRateStoredAsFloat { .. } => "export_frame_rate_stored_as_float",
            Self::CredentialInRecipe { .. } => "export_credential_in_recipe_record",
            Self::UnknownParameter { .. } => "export_parameter_unknown",
            Self::Limit { .. } => "export_recipe_limit",
            Self::InvalidField { .. } => "export_recipe_field_invalid",
            Self::ExporterUnavailable { .. } => "export_exporter_unavailable",
            Self::NotConsumed { .. } => "export_parameter_not_consumed",
            Self::PresetIncomplete { .. } => "export_preset_incomplete",
            Self::SourceMismatch { .. } => "export_parameter_source_mismatch",
            Self::MeaningUnestablished { .. } => "export_parameter_meaning_unestablished",
            Self::Conversion { .. } => "export_parameter_conversion",
            Self::Writer(e) => e.code(),
        }
    }

    /// As reel's own error, for callers typed on [`ReelError`] (`ExportPreset::recipe`): a writer refusal is
    /// itself, any other refusal `MEDIA_UNSUPPORTED` naming this code.
    pub fn into_reel_error(self) -> ReelError {
        match self {
            Self::Writer(e) => e,
            other => ReelError::Unsupported { what: other.code() },
        }
    }
}

impl std::fmt::Display for ExportRecipeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let code = self.code();
        match self {
            Self::OutOfRange { identifier, value, hard_min, hard_max } => {
                write!(f, "{code} identifier={identifier} value={value} hard_min={hard_min} hard_max={hard_max}")
            }
            Self::NonFinite { identifier }
            | Self::NegativeZero { identifier }
            | Self::TakesNoValue { identifier }
            | Self::KindUnsupported { identifier }
            | Self::MissingUnit { identifier }
            | Self::NotConsumed { identifier }
            | Self::Conversion { identifier } => write!(f, "{code} identifier={identifier}"),
            Self::WrongKind { identifier, expected } => write!(f, "{code} identifier={identifier} expected={expected}"),
            Self::CodecIntegerWithoutExporter { recipe_id, identifier }
            | Self::FrameRateStoredAsFloat { recipe_id, identifier } => write!(f, "{code} recipe={recipe_id} identifier={identifier}"),
            Self::CredentialInRecipe { recipe_id, field } => write!(f, "{code} recipe={recipe_id} field={field}"),
            Self::UnknownParameter { reason } => write!(f, "{code} {reason}"),
            Self::Limit { field, limit } => write!(f, "{code} field={field} limit={limit}"),
            Self::InvalidField { field } => write!(f, "{code} field={field}"),
            Self::ExporterUnavailable { exporter } => write!(f, "{code} exporter={exporter}"),
            Self::PresetIncomplete { recipe_id, missing } => write!(f, "{code} recipe={recipe_id} missing={missing}"),
            Self::SourceMismatch { identifier, declared, source } => {
                write!(f, "{code} identifier={identifier} declared={declared} source={source}")
            }
            Self::MeaningUnestablished { identifier, value } => write!(f, "{code} identifier={identifier} value={value}"),
            Self::Writer(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ExportRecipeError {}

impl From<ReelError> for ExportRecipeError {
    fn from(e: ReelError) -> Self {
        Self::Writer(e)
    }
}

/// A recipe record (STU-IO-101, STU-VID-063). Fields are private: every value got in through a checked setter.
#[derive(Debug, Clone, PartialEq)]
pub struct StudioExportRecipe {
    recipe_id: String,
    name: String,
    target_format: Option<ExportTargetFormat>,
    parameters: BTreeMap<&'static str, ExportValue>,
    source_scope: SourceScope,
    output_targets: Vec<String>,
    video: Option<RecipeVideoCodec>,
    audio: Option<PcmChoice>,
    metadata_export_option: MetadataExportOption,
    standard_filters: Vec<String>,
}

fn printable_ascii(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && s.bytes().all(|b| b.is_ascii_graphic())
}

#[derive(Clone, Copy)]
enum Num {
    I(i64),
    F(f64),
}

impl Num {
    fn cmp_bound(self, bound: DeclaredValue) -> Option<Ordering> {
        match (self, bound) {
            (Self::I(v), DeclaredValue::Integer(b)) => Some(v.cmp(&b)),
            (Self::I(v), DeclaredValue::Decimal(b)) => (v as f64).partial_cmp(&b),
            (Self::F(v), DeclaredValue::Integer(b)) => v.partial_cmp(&(b as f64)),
            (Self::F(v), DeclaredValue::Decimal(b)) => v.partial_cmp(&b),
            _ => None,
        }
    }

    fn render(self) -> String {
        match self {
            Self::I(v) => v.to_string(),
            Self::F(v) => v.to_string(),
        }
    }
}

/// Checks one value against its dictionary row (module docs, in this order).
fn check_parameter(recipe_id: &str, has_exporter: bool, row: &ExportParameter, value: &ExportValue) -> Result<(), ExportRecipeError> {
    use ExportRecipeError as E;
    use ExportTypeCode as T;
    let identifier = row.identifier;
    if row.facet == ExportFacet::PublishingDestination || CREDENTIAL_OUTSIDE_PUBLISHING.contains(&identifier) {
        return Err(E::CredentialInRecipe { recipe_id: recipe_id.to_owned(), field: identifier });
    }
    if !row.type_code.stores_value() {
        return Err(E::TakesNoValue { identifier });
    }
    let wrong = |expected| Err(E::WrongKind { identifier, expected });
    let number = if identifier == FPS {
        match value {
            ExportValue::Integer(v) => Some(Num::I(*v)),
            ExportValue::Float(_) => return Err(E::FrameRateStoredAsFloat { recipe_id: recipe_id.to_owned(), identifier }),
            _ => return wrong("integer ticks per frame"),
        }
    } else {
        match (row.type_code, value) {
            (T::SignedInteger | T::EnumeratedInteger, ExportValue::Integer(v)) => Some(Num::I(*v)),
            (T::SignedInteger | T::EnumeratedInteger, _) => return wrong("integer"),
            (T::FloatingPoint, ExportValue::Float(v)) => {
                if !v.is_finite() {
                    return Err(E::NonFinite { identifier });
                }
                if *v == 0.0 && v.is_sign_negative() {
                    return Err(E::NegativeZero { identifier });
                }
                Some(Num::F(*v))
            }
            (T::FloatingPoint, _) => return wrong("float"),
            (T::Boolean, ExportValue::Bool(_)) => None,
            (T::Boolean, _) => return wrong("boolean"),
            (T::String | T::Path | T::Opaque, ExportValue::Text(t)) => {
                if t.len() > MAX_TEXT_BYTES {
                    return Err(E::Limit { field: "parameter_text", limit: MAX_TEXT_BYTES });
                }
                None
            }
            (T::String | T::Path | T::Opaque, _) => return wrong("text"),
            (T::Colour, _) => return Err(E::KindUnsupported { identifier }),
            (T::GroupHeader | T::Action | T::TabGroup | T::RepeatingGroup, _) => return Err(E::TakesNoValue { identifier }),
        }
    };
    let Some(number) = number else { return Ok(()) };
    let (min, max) = row.usable_hard_bounds();
    let below = min.and_then(|b| number.cmp_bound(b)) == Some(Ordering::Less);
    let above = max.and_then(|b| number.cmp_bound(b)) == Some(Ordering::Greater);
    if below || above {
        return Err(E::OutOfRange { identifier, value: number.render(), hard_min: row.hard_min, hard_max: row.hard_max });
    }
    if CODEC_SELECTION.contains(&identifier) && !has_exporter {
        return Err(E::CodecIntegerWithoutExporter { recipe_id: recipe_id.to_owned(), identifier });
    }
    if row.unit == ExportUnit::Undeclared && studio_declared_unit(identifier).is_none() {
        return Err(E::MissingUnit { identifier });
    }
    Ok(())
}

/// Megabits per second to whole bits per second (rounded), when it is 1 ..= u32::MAX.
pub fn megabits_to_bits(mbps: f64) -> Option<u32> {
    let bits = (mbps * 1_000_000.0).round();
    (bits.is_finite() && bits >= 1.0 && bits <= f64::from(u32::MAX)).then_some(bits as u32)
}

impl StudioExportRecipe {
    /// A recipe with no parameters, no target format, whole-source scope and no codecs. `recipe_id`: 1..=128
    /// printable ASCII; `name`: <= 256 bytes.
    pub fn new(recipe_id: &str, name: &str) -> Result<Self, ExportRecipeError> {
        if !printable_ascii(recipe_id, MAX_RECIPE_ID_BYTES) {
            return Err(ExportRecipeError::InvalidField { field: "recipe_id" });
        }
        if name.len() > MAX_NAME_BYTES {
            return Err(ExportRecipeError::Limit { field: "name", limit: MAX_NAME_BYTES });
        }
        Ok(Self {
            recipe_id: recipe_id.to_owned(),
            name: name.to_owned(),
            target_format: None,
            parameters: BTreeMap::new(),
            source_scope: SourceScope::EntireSource,
            output_targets: Vec::new(),
            video: None,
            audio: None,
            metadata_export_option: MetadataExportOption::None,
            standard_filters: Vec::new(),
        })
    }

    /// Sets the exporter identity (each part 1..=64 printable ASCII).
    pub fn with_target_format(mut self, target: ExportTargetFormat) -> Result<Self, ExportRecipeError> {
        if !printable_ascii(&target.exporter_class_id, MAX_EXPORTER_ID_BYTES) {
            return Err(ExportRecipeError::InvalidField { field: "exporter_class_id" });
        }
        if !printable_ascii(&target.exporter_file_type, MAX_EXPORTER_ID_BYTES) {
            return Err(ExportRecipeError::InvalidField { field: "exporter_file_type" });
        }
        self.target_format = Some(target);
        Ok(self)
    }

    pub fn with_video(mut self, video: Option<RecipeVideoCodec>) -> Self {
        self.video = video;
        self
    }

    pub fn with_audio(mut self, audio: Option<PcmChoice>) -> Self {
        self.audio = audio;
        self
    }

    pub fn with_source_scope(mut self, scope: SourceScope) -> Result<Self, ExportRecipeError> {
        if let SourceScope::Span(s) = scope
            && s.end_ticks < s.start_ticks
        {
            return Err(ExportRecipeError::InvalidField { field: "source_scope" });
        }
        self.source_scope = scope;
        Ok(self)
    }

    /// Adds an output target name (a file name, no directory).
    pub fn add_output_target(&mut self, name: &str) -> Result<(), ExportRecipeError> {
        if self.output_targets.len() >= MAX_OUTPUT_TARGETS {
            return Err(ExportRecipeError::Limit { field: "output_targets", limit: MAX_OUTPUT_TARGETS });
        }
        let bad = name.is_empty()
            || name.len() > MAX_OUTPUT_TARGET_BYTES
            || name == "."
            || name.contains("..")
            || name.bytes().any(|b| matches!(b, b'/' | b'\\' | b':' | 0));
        if bad {
            return Err(ExportRecipeError::InvalidField { field: "output_targets" });
        }
        self.output_targets.push(name.to_owned());
        Ok(())
    }

    pub fn add_standard_filter(&mut self, filter: &str) -> Result<(), ExportRecipeError> {
        if self.standard_filters.len() >= MAX_STANDARD_FILTERS {
            return Err(ExportRecipeError::Limit { field: "standard_filters", limit: MAX_STANDARD_FILTERS });
        }
        if filter.is_empty() || filter.len() > MAX_STANDARD_FILTER_BYTES {
            return Err(ExportRecipeError::InvalidField { field: "standard_filters" });
        }
        self.standard_filters.push(filter.to_owned());
        Ok(())
    }

    /// Sets one parameter after every check of the module docs; a refusal stores nothing. Setting an
    /// identifier again replaces its value.
    pub fn set_parameter(&mut self, identifier: &str, value: ExportValue) -> Result<(), ExportRecipeError> {
        if identifier.len() > MAX_IDENTIFIER_BYTES {
            return Err(ExportRecipeError::UnknownParameter { reason: "identifier_too_long" });
        }
        let row = export_parameter(identifier).ok_or(ExportRecipeError::UnknownParameter { reason: "not_in_dictionary" })?;
        if !self.parameters.contains_key(row.identifier) && self.parameters.len() >= EXPORT_PARAMETER_COUNT {
            return Err(ExportRecipeError::Limit { field: "parameters", limit: EXPORT_PARAMETER_COUNT });
        }
        check_parameter(&self.recipe_id, self.target_format.is_some(), row, &value)?;
        self.parameters.insert(row.identifier, value);
        Ok(())
    }

    pub fn recipe_id(&self) -> &str {
        &self.recipe_id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn target_format(&self) -> Option<&ExportTargetFormat> {
        self.target_format.as_ref()
    }
    pub fn parameters(&self) -> &BTreeMap<&'static str, ExportValue> {
        &self.parameters
    }
    pub fn source_scope(&self) -> SourceScope {
        self.source_scope
    }
    pub fn output_targets(&self) -> &[String] {
        &self.output_targets
    }
    pub fn video(&self) -> Option<RecipeVideoCodec> {
        self.video
    }
    pub fn audio(&self) -> Option<PcmChoice> {
        self.audio
    }
    /// STU-VID-063.
    pub fn does_video(&self) -> bool {
        self.video.is_some()
    }
    pub fn does_audio(&self) -> bool {
        self.audio.is_some()
    }
    pub fn metadata_export_option(&self) -> MetadataExportOption {
        self.metadata_export_option
    }
    pub fn standard_filters(&self) -> &[String] {
        &self.standard_filters
    }

    /// Reel's container when the target format names reel's exporter, else `export_exporter_unavailable`.
    fn reel_container(&self) -> Result<Container, ExportRecipeError> {
        let Some(t) = &self.target_format else {
            return Err(ExportRecipeError::ExporterUnavailable { exporter: "none".to_owned() });
        };
        let container = match t.exporter_file_type.as_str() {
            "mp4" => Some(Container::Mp4),
            "mov" => Some(Container::Mov),
            _ => None,
        };
        match container {
            Some(c) if t.exporter_class_id == MUXER_ID => Ok(c),
            _ => Err(ExportRecipeError::ExporterUnavailable { exporter: format!("{}/{}", t.exporter_class_id, t.exporter_file_type) }),
        }
    }

    fn os_bitrate(&self) -> Result<u32, ExportRecipeError> {
        match self.parameters.get(BITRATE) {
            None => Err(ExportRecipeError::PresetIncomplete { recipe_id: self.recipe_id.clone(), missing: BITRATE }),
            Some(ExportValue::Float(mbps)) => megabits_to_bits(*mbps).ok_or(ExportRecipeError::Conversion { identifier: BITRATE }),
            Some(_) => Err(ExportRecipeError::WrongKind { identifier: BITRATE, expected: "float" }),
        }
    }

    /// The writer settings reel's export queue runs (`os_encoder`: an OS encoder is registered on the queue
    /// the job goes to): the exporter must be reel's, every parameter and record field must be consumed, an
    /// H.264 / HEVC recipe needs its target bitrate, and the codec must be available (`MEDIA_CODEC_UNAVAILABLE`
    /// as [`ExportRecipeError::Writer`]). Source facts are checked by [`job_from_recipe`].
    pub fn resolve(&self, os_encoder: bool) -> Result<ExportRecipe, ExportRecipeError> {
        let container = self.reel_container()?;
        if !self.output_targets.is_empty() {
            return Err(ExportRecipeError::NotConsumed { identifier: "output_targets" });
        }
        if !self.standard_filters.is_empty() {
            return Err(ExportRecipeError::NotConsumed { identifier: "standard_filters" });
        }
        match self.metadata_export_option {
            MetadataExportOption::None => {}
        }
        for identifier in self.parameters.keys() {
            let consumed = match *identifier {
                WIDTH | HEIGHT | FPS => self.video.is_some(),
                RATE | CHANNELS => self.audio.is_some(),
                BITRATE => matches!(self.video, Some(RecipeVideoCodec::H264(_) | RecipeVideoCodec::Hevc)),
                _ => false,
            };
            if !consumed {
                return Err(ExportRecipeError::NotConsumed { identifier });
            }
        }
        let unavailable = |codec: &str| ExportRecipeError::Writer(ReelError::CodecUnavailable { codec: codec.to_owned() });
        let video = match self.video {
            None => None,
            Some(RecipeVideoCodec::Vc3(p)) => Some(VideoCodecChoice::Vc3(p)),
            Some(RecipeVideoCodec::Av1(s)) if cfg!(feature = "av1-encode") => Some(VideoCodecChoice::Av1(s)),
            Some(RecipeVideoCodec::Av1(_)) => return Err(unavailable("av1")),
            Some(RecipeVideoCodec::H264(profile)) => {
                let bitrate = self.os_bitrate()?;
                if !os_encoder {
                    return Err(unavailable("h264"));
                }
                Some(VideoCodecChoice::H264(OsVideoSettings { bitrate, profile }))
            }
            Some(RecipeVideoCodec::Hevc) => {
                let bitrate = self.os_bitrate()?;
                if !os_encoder {
                    return Err(unavailable("hevc"));
                }
                Some(VideoCodecChoice::Hevc(OsVideoSettings { bitrate, profile: H264Profile::High }))
            }
        };
        Ok(ExportRecipe { recipe_id: self.recipe_id.clone(), container, video, audio: self.audio })
    }

    /// Deterministic JSON (hand-written; parameters in identifier byte order; no clock, id or locale).
    pub fn to_json(&self) -> String {
        let mut o = String::new();
        o.push_str("{\"recipe_id\":");
        json_str(&mut o, &self.recipe_id);
        o.push_str(",\"name\":");
        json_str(&mut o, &self.name);
        o.push_str(",\"target_format\":");
        match &self.target_format {
            Some(t) => {
                o.push_str("{\"exporter_class_id\":");
                json_str(&mut o, &t.exporter_class_id);
                o.push_str(",\"exporter_file_type\":");
                json_str(&mut o, &t.exporter_file_type);
                o.push('}');
            }
            None => o.push_str("null"),
        }
        o.push_str(",\"parameters\":{");
        for (i, (id, value)) in self.parameters.iter().enumerate() {
            if i > 0 {
                o.push(',');
            }
            json_str(&mut o, id);
            o.push(':');
            match value {
                ExportValue::Integer(v) => o.push_str(&v.to_string()),
                ExportValue::Float(v) => o.push_str(&json_f64(*v)),
                ExportValue::Bool(v) => o.push_str(if *v { "true" } else { "false" }),
                ExportValue::Text(t) => json_str(&mut o, t),
            }
        }
        o.push_str("},\"source_scope\":");
        match self.source_scope {
            SourceScope::EntireSource => o.push_str("{\"kind\":\"entire_source\"}"),
            SourceScope::Span(s) => {
                o.push_str(&format!("{{\"kind\":\"span\",\"start_ticks\":{},\"end_ticks\":{}}}", s.start_ticks, s.end_ticks))
            }
        }
        o.push_str(",\"output_targets\":");
        json_list(&mut o, &self.output_targets);
        o.push_str(&format!(",\"does_video\":{},\"does_audio\":{},\"video\":", self.does_video(), self.does_audio()));
        match self.video {
            None => o.push_str("null"),
            Some(RecipeVideoCodec::Vc3(p)) => o.push_str(&format!("{{\"codec\":\"vc3\",\"profile\":\"{p:?}\"}}")),
            Some(RecipeVideoCodec::Av1(s)) => o.push_str(&format!(
                "{{\"codec\":\"av1\",\"speed\":{},\"quantizer\":{},\"bitrate\":{},\"max_keyframe_interval\":{}}}",
                s.speed,
                s.quantizer,
                s.bitrate.map_or("null".to_owned(), |b| b.to_string()),
                s.max_keyframe_interval
            )),
            Some(RecipeVideoCodec::H264(p)) => o.push_str(&format!("{{\"codec\":\"h264\",\"profile_idc\":{}}}", p.profile_idc())),
            Some(RecipeVideoCodec::Hevc) => o.push_str("{\"codec\":\"hevc\"}"),
        }
        o.push_str(",\"audio\":");
        match self.audio {
            None => o.push_str("null"),
            Some(a) => o.push_str(&format!("{{\"pcm_bits\":{},\"float\":{}}}", a.bits, a.float)),
        }
        o.push_str(",\"metadata_export_option\":\"none\",\"standard_filters\":");
        json_list(&mut o, &self.standard_filters);
        o.push('}');
        o
    }
}

fn json_str(o: &mut String, s: &str) {
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if u32::from(c) < 0x20 => o.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => o.push(c),
        }
    }
    o.push('"');
}

fn json_list(o: &mut String, items: &[String]) {
    o.push('[');
    for (i, s) in items.iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        json_str(o, s);
    }
    o.push(']');
}

/// A finite float as JSON that never reads back as an integer (`20` is written `20.0`).
fn json_f64(v: f64) -> String {
    let s = v.to_string();
    if s.contains('.') { s } else { format!("{s}.0") }
}

/// The record of a shipped preset (a user recipe is the same record, STU-VID-063): reel's exporter for the
/// preset's container, its codec selection, and for H.264 / HEVC its bitrate as `ADBEVideoTargetBitrate`.
pub(crate) fn preset_recipe(preset: &ExportPreset) -> StudioExportRecipe {
    let mut parameters = BTreeMap::new();
    let mut bitrate = |s: OsVideoSettings| {
        parameters.insert(BITRATE, ExportValue::Float(f64::from(s.bitrate) / 1_000_000.0));
    };
    let video = match preset.video {
        PresetVideo::Vc3(p) => RecipeVideoCodec::Vc3(p),
        PresetVideo::Av1(s) => RecipeVideoCodec::Av1(s),
        PresetVideo::H264(s) => {
            bitrate(s);
            RecipeVideoCodec::H264(s.profile)
        }
        PresetVideo::Hevc(s) => {
            bitrate(s);
            RecipeVideoCodec::Hevc
        }
    };
    StudioExportRecipe {
        recipe_id: preset.preset_id.to_owned(),
        name: preset.name.to_owned(),
        target_format: Some(ExportTargetFormat::reel(preset.container)),
        parameters,
        source_scope: SourceScope::EntireSource,
        output_targets: Vec::new(),
        video: Some(video),
        audio: preset.audio,
        metadata_export_option: MetadataExportOption::None,
        standard_filters: Vec::new(),
    }
}

/// What the host supplies for a job besides the recipe (the fields of [`ExportJob`] a recipe does not carry).
pub struct RecipeJobInputs {
    pub video: Option<Box<dyn ExportVideoSource>>,
    pub audio: Option<Box<dyn ExportAudioSource>>,
    pub span: SourceSpan,
    pub output: PathBuf,
    pub overwrite: bool,
    pub artifact: String,
    pub limits: ExportLimits,
}

/// An [`ExportJob`] from a recipe (lane proposal name, not a spec name), admitted by the host like any job.
/// Reads only source facts (`format`, `ticks_per_frame`, `rate`, `channels`), never a frame: every consumed
/// parameter must equal its source fact, a negative channel count is refused, and a `Span` scope must equal
/// the job's span. Refusals: [`StudioExportRecipe::resolve`]'s, `export_parameter_source_mismatch`,
/// `export_parameter_meaning_unestablished`. Writer bounds (e.g. the muxer's largest side) stay the queue's.
pub fn job_from_recipe(recipe: &StudioExportRecipe, inputs: RecipeJobInputs, os_encoder: bool) -> Result<ExportJob, ExportRecipeError> {
    let reel = recipe.resolve(os_encoder)?;
    if let SourceScope::Span(span) = recipe.source_scope
        && span != inputs.span
    {
        return Err(ExportRecipeError::SourceMismatch {
            identifier: "source_scope",
            declared: format!("{}..{}", span.start_ticks, span.end_ticks),
            source: format!("{}..{}", inputs.span.start_ticks, inputs.span.end_ticks),
        });
    }
    for (identifier, value) in &recipe.parameters {
        let ExportValue::Integer(declared) = value else { continue };
        let video = inputs.video.as_deref();
        let audio = inputs.audio.as_deref();
        let source: Option<i128> = match *identifier {
            WIDTH => video.map(|v| i128::from(v.format().width)),
            HEIGHT => video.map(|v| i128::from(v.format().height)),
            FPS => video.map(|v| i128::from(v.ticks_per_frame())),
            RATE => audio.map(|a| i128::from(a.rate())),
            CHANNELS => {
                if *declared < 0 {
                    return Err(ExportRecipeError::MeaningUnestablished { identifier, value: *declared });
                }
                audio.map(|a| i128::from(a.channels()))
            }
            _ => continue,
        };
        if source != Some(i128::from(*declared)) {
            return Err(ExportRecipeError::SourceMismatch {
                identifier,
                declared: declared.to_string(),
                source: source.map_or("none".to_owned(), |s| s.to_string()),
            });
        }
    }
    Ok(ExportJob {
        recipe: reel,
        video: inputs.video,
        audio: inputs.audio,
        span: inputs.span,
        output: inputs.output,
        overwrite: inputs.overwrite,
        artifact: inputs.artifact,
        limits: inputs.limits,
    })
}
