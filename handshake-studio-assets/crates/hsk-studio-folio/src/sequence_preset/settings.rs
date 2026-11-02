//! The STU-VID-011 sequence settings record and the per-document record set that binds one to each
//! sequence (module docs of `sequence_preset` for the placement reading).
#![deny(clippy::arithmetic_side_effects)]
use super::super::{
    Budget, Code, MAX_COMPOSITION_SIDE, MAX_SEQUENCES, MIN_COMPOSITION_SIDE, Result, SequenceFrameRate,
    StudioDocument, StudioImmersiveSettings, err, id, key, required, validate_immersive, values::Ctx,
};
use super::editing_mode::{editing_mode_violations, shipped_editing_mode};
use hsk_studio_accord::CancellationToken;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

schema_token!(SequenceSettingsSchema, "hsk.studio.sequence_settings@1");

/// Largest audio sample rate a settings record may declare (Studio hostile-input bound, the Reel
/// decoder ceiling; STU-VID-011 declares 48000 and 32000 only).
pub const MAX_SEQUENCE_SAMPLE_RATE_HZ: u32 = 768_000;
/// Longest preview codec token in bytes (a key: a fourcc such as `apcs`, or a Studio codec key).
pub const MAX_PREVIEW_CODEC_BYTES: usize = 128;

/// STU-VID-011 `frame_size`: composition raster size in pixels. Each side is
/// `MIN_COMPOSITION_SIDE..=MAX_COMPOSITION_SIDE` (Studio bounds shared with compositions; the
/// shipped sizes run from 512x256 to 8192x8192).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SequenceFrameSize {
    pub width: u32,
    pub height: u32,
}
impl SequenceFrameSize {
    pub(crate) fn valid(&self) -> bool {
        let side = MIN_COMPOSITION_SIDE..=MAX_COMPOSITION_SIDE;
        side.contains(&self.width) && side.contains(&self.height)
    }
}

/// STU-VID-011 `pixel_aspect_ratio`: a rational, never a float (a float is refused on the wire, test
/// `the_normative_sequence_settings_record_mt_33686`). Stored as declared (the shipped
/// `1920:1440` and `1920:1920` are not reduced), so only a zero term is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SequencePixelAspectRatio {
    pub numerator: u32,
    pub denominator: u32,
}
impl SequencePixelAspectRatio {
    pub const SQUARE: Self = Self { numerator: 1, denominator: 1 };
    pub(crate) fn valid(&self) -> bool {
        self.numerator != 0 && self.denominator != 0
    }
}

/// STU-VID-011 `field_order`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SequenceFieldOrder {
    Progressive,
    UpperFieldFirst,
    LowerFieldFirst,
}

/// The STU-VID-012a time-display enumeration, all 15 members with their normative codes. Pulse
/// holds the same enumeration as `hsk_studio_pulse::display::TimeDisplay` (same members, same
/// codes); Folio may depend only on Accord, so the members are repeated here with the same codes
/// and the code is the conversion (`code` / `from_code`). Code 113 has no member (STU-VID-012b,
/// gap STU-VID-081).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub enum StudioTimeDisplay {
    #[serde(rename = "timecode_24")]
    Timecode24,
    #[serde(rename = "timecode_25")]
    Timecode25,
    #[serde(rename = "timecode_2997_drop")]
    Timecode2997Drop,
    #[serde(rename = "timecode_2997_non_drop")]
    Timecode2997NonDrop,
    #[serde(rename = "timecode_30")]
    Timecode30,
    #[serde(rename = "timecode_50")]
    Timecode50,
    #[serde(rename = "timecode_5994_drop")]
    Timecode5994Drop,
    #[serde(rename = "timecode_5994_non_drop")]
    Timecode5994NonDrop,
    #[serde(rename = "timecode_60")]
    Timecode60,
    #[serde(rename = "frames")]
    Frames,
    #[serde(rename = "timecode_23976")]
    Timecode23976,
    #[serde(rename = "feet_frames_16mm")]
    FeetFrames16mm,
    #[serde(rename = "feet_frames_35mm")]
    FeetFrames35mm,
    #[serde(rename = "audio_samples")]
    AudioSamples,
    #[serde(rename = "milliseconds")]
    Milliseconds,
}
impl StudioTimeDisplay {
    /// Every member, in code order.
    pub const ALL: [Self; 15] = [
        Self::Timecode24,
        Self::Timecode25,
        Self::Timecode2997Drop,
        Self::Timecode2997NonDrop,
        Self::Timecode30,
        Self::Timecode50,
        Self::Timecode5994Drop,
        Self::Timecode5994NonDrop,
        Self::Timecode60,
        Self::Frames,
        Self::Timecode23976,
        Self::FeetFrames16mm,
        Self::FeetFrames35mm,
        Self::AudioSamples,
        Self::Milliseconds,
    ];
    /// The STU-VID-012a code.
    pub const fn code(self) -> u16 {
        match self {
            Self::Timecode24 => 100,
            Self::Timecode25 => 101,
            Self::Timecode2997Drop => 102,
            Self::Timecode2997NonDrop => 103,
            Self::Timecode30 => 104,
            Self::Timecode50 => 105,
            Self::Timecode5994Drop => 106,
            Self::Timecode5994NonDrop => 107,
            Self::Timecode60 => 108,
            Self::Frames => 109,
            Self::Timecode23976 => 110,
            Self::FeetFrames16mm => 111,
            Self::FeetFrames35mm => 112,
            Self::AudioSamples => 200,
            Self::Milliseconds => 201,
        }
    }
    /// The member of a STU-VID-012a code; `None` for any other code, 113 included.
    pub fn from_code(code: u16) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.code() == code)
    }
    /// A video display (codes 100..=112): valid for `video_time_display`.
    pub const fn is_video(self) -> bool {
        self.code() < 200
    }
    /// An audio display (`audio_samples` or `milliseconds`): valid for `audio_time_display`.
    pub const fn is_audio(self) -> bool {
        !self.is_video()
    }
}

/// STU-FX-137 master channel configuration (`audio_master_channel_type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum AudioMasterChannelType {
    #[serde(rename = "mono")]
    Mono,
    #[serde(rename = "stereo")]
    Stereo,
    #[serde(rename = "5.1")]
    FivePointOne,
    #[serde(rename = "multichannel_adaptive")]
    MultichannelAdaptive,
    #[serde(rename = "sixteen_channel")]
    SixteenChannel,
}

/// The working colour spaces STU-VID-011 declares in the shipped set (a `StudioColorProfile`
/// reference by its declared name; other spaces need a profile binding Folio does not add here).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SequenceColorSpace {
    #[serde(rename = "BT.709 RGB Full")]
    Bt709RgbFull,
    #[serde(rename = "BT.2100 HLG RGB Full")]
    Bt2100HlgRgbFull,
}

/// STU-VID-011 `working_color_space` with its `is_linearized` flag (declared 0 in every shipped
/// configuration read).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SequenceWorkingColorSpace {
    pub color_space: SequenceColorSpace,
    pub is_linearized: bool,
}
impl SequenceWorkingColorSpace {
    /// The STU-VID-011 Studio default: `BT.709 RGB Full`, not linearized.
    pub const STUDIO_DEFAULT: Self = Self { color_space: SequenceColorSpace::Bt709RgbFull, is_linearized: false };
}

/// STU-VID-019 preview render settings: the codec and frame size of rendered timeline previews,
/// per sequence and independent of the export recipe; the frame size MAY differ from the sequence
/// frame size. `codec` is a key (a fourcc such as `apcs`); explicit null = the codec of Studio's
/// preview tier, an implementation decision (STU-VID-019, STU-VID-051).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioPreviewRenderFormat {
    #[serde(deserialize_with = "required")]
    #[schemars(required)]
    pub codec: Option<String>,
    pub frame_size: SequenceFrameSize,
}

/// The STU-VID-011 normative sequence settings record: every field required, in the table order.
/// `immersive` is the STU-VID-018 record (the composition record shape, STU-CMP-055a); explicit null
/// = a flat sequence with no immersive frame parameters (the composition reading).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioSequenceSettings {
    pub frame_size: SequenceFrameSize,
    pub pixel_aspect_ratio: SequencePixelAspectRatio,
    pub frame_rate: SequenceFrameRate,
    pub field_order: SequenceFieldOrder,
    pub video_time_display: StudioTimeDisplay,
    pub audio_time_display: StudioTimeDisplay,
    pub audio_sample_rate_hz: u32,
    pub audio_master_channel_type: AudioMasterChannelType,
    pub working_color_space: SequenceWorkingColorSpace,
    pub auto_tone_map: bool,
    pub use_maximum_bit_depth: bool,
    pub use_maximum_render_quality: bool,
    pub allow_linear_compositing: bool,
    pub initial_video_track_count: u8,
    pub initial_audio_track_count: u8,
    pub preview_render_format: StudioPreviewRenderFormat,
    #[serde(deserialize_with = "required")]
    #[schemars(required)]
    pub immersive: Option<StudioImmersiveSettings>,
}

/// The STU-VID-011 field names, in table order (the `defaulted_fields` vocabulary of a preset).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SequenceSettingsField {
    FrameSize,
    PixelAspectRatio,
    FrameRate,
    FieldOrder,
    VideoTimeDisplay,
    AudioTimeDisplay,
    AudioSampleRateHz,
    AudioMasterChannelType,
    WorkingColorSpace,
    AutoToneMap,
    UseMaximumBitDepth,
    UseMaximumRenderQuality,
    AllowLinearCompositing,
    InitialVideoTrackCount,
    InitialAudioTrackCount,
    PreviewRenderFormat,
    Immersive,
}

fn check(ok: bool, target: &str, field: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(err(Code::InvalidField, format!("{target}/{field}"))) }
}

/// One settings record: canonical frame rate (an STU-VID-013 table value), sizes in bounds, non-zero
/// aspect terms, a video display in `video_time_display` and an audio display in
/// `audio_time_display`, a sample rate `1..=MAX_SEQUENCE_SAMPLE_RATE_HZ`, a key preview codec, and
/// valid immersive parameters. Failures are `InvalidField` at `<target>/<field>`.
pub(crate) fn validate_settings(s: &StudioSequenceSettings, target: &str, cx: &mut Ctx) -> Result<()> {
    cx.tick()?;
    check(s.frame_size.valid(), target, "frame_size")?;
    check(s.pixel_aspect_ratio.valid(), target, "pixel_aspect_ratio")?;
    check(s.frame_rate.accord().is_some(), target, "frame_rate")?;
    check(s.video_time_display.is_video(), target, "video_time_display")?;
    check(s.audio_time_display.is_audio(), target, "audio_time_display")?;
    check((1..=MAX_SEQUENCE_SAMPLE_RATE_HZ).contains(&s.audio_sample_rate_hz), target, "audio_sample_rate_hz")?;
    let preview = &s.preview_render_format;
    check(preview.frame_size.valid(), target, "preview_render_format")?;
    if let Some(codec) = &preview.codec {
        check(key(codec) && codec.len() <= MAX_PREVIEW_CODEC_BYTES, target, "preview_render_format")?;
        cx.text(codec, target)?;
    }
    if let Some(immersive) = &s.immersive {
        validate_immersive(immersive, &format!("{target}/immersive"), cx)?;
    }
    Ok(())
}

/// The settings record and editing mode of one document sequence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioSequenceSettingsEntry {
    /// A sequence of the document.
    pub sequence_id: String,
    /// The one editing mode the sequence references (STU-VID-017): a shipped mode id
    /// ([`shipped_editing_modes`](super::shipped_editing_modes)); `custom` declares no constraints.
    pub editing_mode: String,
    pub settings: StudioSequenceSettings,
}

/// The per-document sequence settings record set (`hsk.studio.sequence_settings@1`), one per
/// `document_id`, beside the document. A sequence without an entry is an earlier-shape sequence
/// whose settings were not recorded (its frame rate lives on the sequence); an empty set is not
/// encoded beyond its header.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioSequenceSettingsRecords {
    pub schema_id: SequenceSettingsSchema,
    /// `SDOC` id of the annotated document.
    pub document_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sequences: Vec<StudioSequenceSettingsEntry>,
}
impl StudioSequenceSettingsRecords {
    /// An empty set for `document_id`.
    pub fn new(document_id: impl Into<String>) -> Self {
        Self { schema_id: SequenceSettingsSchema::V1, document_id: document_id.into(), sequences: Vec::new() }
    }
    /// The entry of sequence `sequence_id`.
    pub fn entry(&self, sequence_id: &str) -> Option<&StudioSequenceSettingsEntry> {
        self.sequences.iter().find(|e| e.sequence_id == sequence_id)
    }
}

/// Validates the record set against the document: `SDOC` id equal to the document's; at most
/// `MAX_SEQUENCES` entries (counted before any is walked, then charged against `Budget::nodes`);
/// each names a document sequence (`DanglingReference`) once (`DuplicateIdentity`); its editing mode
/// is a shipped mode (`DanglingReference` at `<sequence>/editing_mode`) whose constraints the
/// settings meet (`InvalidField` at `<sequence>/editing_mode`; shipped modes declare none); the
/// settings are valid ([`validate_settings`]) and their `frame_rate` equals the sequence's
/// (`InvalidField` at `<sequence>/frame_rate`).
pub fn validate_sequence_settings_records(
    r: &StudioSequenceSettingsRecords,
    d: &StudioDocument,
    b: Budget,
    token: &CancellationToken,
) -> Result<()> {
    let mut cx = Ctx::new(b, token, &super::super::Unresolved);
    cx.tick()?;
    id(&r.document_id, "SDOC", "document_id")?;
    if r.document_id != d.document_id {
        return Err(err(Code::InvalidField, "document_id"));
    }
    if r.sequences.len() > MAX_SEQUENCES {
        return Err(err(Code::BudgetExceeded, "sequence_settings"));
    }
    cx.nodes(r.sequences.len(), "sequence_settings")?;
    let mut seen = BTreeSet::new();
    for e in &r.sequences {
        cx.tick()?;
        let t = e.sequence_id.as_str();
        if !key(t) {
            return Err(err(Code::InvalidId, "sequence_settings/sequence_id"));
        }
        let Some(seq) = d.sequences.iter().find(|s| s.sequence_id == t) else {
            return Err(err(Code::DanglingReference, t));
        };
        if !seen.insert(t) {
            return Err(err(Code::DuplicateIdentity, t));
        }
        let Some(mode) = shipped_editing_mode(&e.editing_mode) else {
            return Err(err(Code::DanglingReference, format!("{t}/editing_mode")));
        };
        validate_settings(&e.settings, t, &mut cx)?;
        check(e.settings.frame_rate == seq.frame_rate, t, "frame_rate")?;
        check(editing_mode_violations(&mode, &e.settings).is_empty(), t, "editing_mode")?;
    }
    Ok(())
}

/// JSON schema (draft 2020-12) of the sequence settings record set.
pub fn sequence_settings_records_schema() -> schemars::Schema {
    schemars::generate::SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<StudioSequenceSettingsRecords>()
}

/// Parses wire JSON (bounded by `Budget::input_bytes` and depth before any parse; duplicate keys
/// `DuplicateField`, unknown fields and wrong types `InvalidJson`) and validates it against the
/// document.
pub fn parse_sequence_settings_records(
    input: &[u8],
    d: &StudioDocument,
    b: Budget,
    token: &CancellationToken,
) -> Result<StudioSequenceSettingsRecords> {
    let r: StudioSequenceSettingsRecords = super::parse_wire(input, b, token, "sequence_settings")?;
    validate_sequence_settings_records(&r, d, b, token)?;
    Ok(r)
}
