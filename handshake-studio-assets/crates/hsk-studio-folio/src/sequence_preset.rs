//! Sequence settings, sequence presets and editing modes (lane SMB-PR-SEQUENCE-PRESETS-1;
//! STU-VID-011, 012a, 016, 016a, 017, 017a, 018, 019).
//!
//! * [`StudioSequenceSettings`]: the STU-VID-011 normative settings record, all 17 fields, typed as
//!   the table states (`frame_rate` in ticks per frame, `pixel_aspect_ratio` a rational, the
//!   STU-VID-012a time-display enumeration [`StudioTimeDisplay`], STU-VID-018 `immersive` as the
//!   composition's [`StudioImmersiveSettings`], STU-VID-019 [`StudioPreviewRenderFormat`]).
//! * [`StudioSequenceSettingsRecords`] (`hsk.studio.sequence_settings@1`): one settings record and
//!   one editing mode per document sequence, beside the document like the multicam and audio
//!   routing record sets. Placement reading: STU-VID-011/018/019 make these settings part of the
//!   sequence record; `StudioSequence` gains no field here because every `StudioSequence` struct
//!   literal across the dependent crates would break (lane notes: a move onto the sequence is a
//!   lead decision). The entry is keyed by `sequence_id`, its `frame_rate` must equal the
//!   sequence's, and a sequence without an entry is an earlier-shape sequence that loads unchanged.
//! * [`StudioSequencePresetSet`]: the sequence-preset section of the `StudioStyleRegistry`
//!   (`hsk.studio.style_registry@1`, as the audio and motion preset sections), STU-VID-016a. A
//!   preset is one complete settings record with a STU-FX-143 category path; it holds no path,
//!   asset or machine-local value, so the library is portable data (STU-FX-039a).
//!   [`shipped_sequence_presets`] holds the 391 complete rows of the 016a table; the one row with no
//!   declared frame rate is listed in [`SHIPPED_PRESET_EXCLUSIONS`] and never shipped (STU-VID-016:
//!   there is no partial preset; test `studiosequencepreset_is_a_named_complete_settings_record_mt_33695`). Fields the table does not give are filled by Studio and
//!   named in the preset's `defaulted_fields`: STU-VID-011 Studio defaults where the clause states
//!   one, Studio readings where it does not (`ShippedPresetRow::preset` docs).
//! * [`StudioEditingMode`]: STU-VID-017; [`shipped_editing_modes`] holds the 53 rows of the 017a
//!   table with `constraints_known = false` and empty allow-lists (gap STU-VID-080).
//! * [`StudioSequenceSettingsCommand`]: save a sequence's settings as a new preset (STU-VID-016a,
//!   "a typed command, not a UI-only affordance"), apply a preset to a sequence, set a sequence's
//!   editing mode. Each takes the caller's `CommandContext` (actor, client session, command id,
//!   base revision, correlation), is atomic and returns a report of codes and counters only; Folio
//!   has no Observe edge, so the host emits it.
//!
//! The records arrive from untrusted wire JSON: integer arithmetic is denied (workflow V4-01), every
//! list is counted before it is walked and charged against the [`Budget`]. Vendor parity of the
//! command behaviour is UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
mod editing_mode;
mod settings;
mod shipped;
pub use editing_mode::*;
pub use settings::*;
pub use shipped::SHIPPED_PRESET_EXCLUSIONS;

use super::{
    Budget, Code, Result, SequenceFrameRate, StudioDocument, StyleRegistrySchema, Unresolved, err, key,
    values::{Ctx, label},
};
use hsk_studio_accord::{CancellationToken, CommandContext};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Presets one set may hold (Studio bound; 391 ship).
pub const MAX_SEQUENCE_PRESETS: usize = 4096;
/// Category path depth (Studio bound; the shipped paths are at most three deep).
pub const MAX_SEQUENCE_PRESET_CATEGORY_DEPTH: usize = 8;
/// Longest preset name or category segment in bytes.
pub const MAX_SEQUENCE_PRESET_TEXT: usize = 128;

/// Why a shipped preset field differs from what its table row declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SequencePresetWarning {
    /// The row declares time-display code 113 (gap STU-VID-081); the frame-rate-matched display was
    /// stored instead (STU-VID-012b; for 48 fps no timecode member exists, so `frames`, as Pulse's
    /// `TimeDisplay::matching`).
    TimeDisplayCode113,
}

/// STU-VID-016 `StudioSequencePreset`: a named, complete sequence settings record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioSequencePreset {
    /// Unique within the set (key grammar).
    pub preset_id: String,
    pub name: String,
    /// Browsable category path, outermost first (STU-FX-143); empty = top level.
    pub category_path: Vec<String>,
    pub settings: StudioSequenceSettings,
    /// Settings fields the source did not declare, so Studio filled them (shipped presets only;
    /// omitted when empty): `working_color_space`, `use_maximum_bit_depth`,
    /// `use_maximum_render_quality`, `allow_linear_compositing` and `initial_audio_track_count` hold
    /// their STU-VID-011 Studio default; `auto_tone_map`, `preview_render_format` and `immersive` have
    /// no declared default and hold a Studio reading (`ShippedPresetRow::preset` docs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub defaulted_fields: Vec<SequenceSettingsField>,
    /// Source values that could not be stored as declared (omitted when empty).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<SequencePresetWarning>,
}

/// The sequence-preset entries of a style registry (`hsk.studio.style_registry@1`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioSequencePresetSet {
    pub schema_id: StyleRegistrySchema,
    pub presets: Vec<StudioSequencePreset>,
}
impl StudioSequencePresetSet {
    /// An empty set.
    pub fn new() -> Self {
        Self { schema_id: StyleRegistrySchema::V1, presets: Vec::new() }
    }
    /// The preset `preset_id`.
    pub fn preset(&self, preset_id: &str) -> Option<&StudioSequencePreset> {
        self.presets.iter().find(|p| p.preset_id == preset_id)
    }
}
impl Default for StudioSequencePresetSet {
    fn default() -> Self {
        Self::new()
    }
}

fn text(s: &str) -> bool {
    !s.trim().is_empty() && s.len() <= MAX_SEQUENCE_PRESET_TEXT && label(s)
}

fn category_ok(path: &[String]) -> bool {
    path.len() <= MAX_SEQUENCE_PRESET_CATEGORY_DEPTH && path.iter().all(|c| text(c))
}

/// Validates a preset set: at most `MAX_SEQUENCE_PRESETS` presets (counted before any is walked,
/// then charged against `Budget::nodes`); unique key-grammar preset ids (`InvalidId` /
/// `DuplicateIdentity`); names and category segments non-blank, at most
/// `MAX_SEQUENCE_PRESET_TEXT` bytes, no control characters, depth at most
/// `MAX_SEQUENCE_PRESET_CATEGORY_DEPTH` (`InvalidField`); settings valid ([`StudioSequenceSettings`]
/// rules, `InvalidField` at `sequence_presets/<id>/<field>`); `defaulted_fields` and `warnings`
/// listed once each. Every text counts against `Budget::payload_bytes`.
pub fn validate_sequence_presets(set: &StudioSequencePresetSet, b: Budget, token: &CancellationToken) -> Result<()> {
    let mut cx = Ctx::new(b, token, &Unresolved);
    cx.tick()?;
    if set.presets.len() > MAX_SEQUENCE_PRESETS {
        return Err(err(Code::BudgetExceeded, "sequence_presets"));
    }
    cx.nodes(set.presets.len(), "sequence_presets")?;
    let mut ids = BTreeSet::new();
    for p in &set.presets {
        cx.tick()?;
        if !key(&p.preset_id) {
            return Err(err(Code::InvalidId, "sequence_presets/preset_id"));
        }
        if !ids.insert(p.preset_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, format!("sequence_presets/{}", p.preset_id)));
        }
        let t = format!("sequence_presets/{}", p.preset_id);
        if !text(&p.name) {
            return Err(err(Code::InvalidField, format!("{t}/name")));
        }
        if !category_ok(&p.category_path) {
            return Err(err(Code::InvalidField, format!("{t}/category_path")));
        }
        cx.text(&p.name, &t)?;
        for c in &p.category_path {
            cx.text(c, &t)?;
        }
        validate_settings(&p.settings, &t, &mut cx)?;
        let fields: BTreeSet<_> = p.defaulted_fields.iter().collect();
        let warnings: BTreeSet<_> = p.warnings.iter().collect();
        if fields.len() != p.defaulted_fields.len() || warnings.len() != p.warnings.len() {
            return Err(err(Code::InvalidField, format!("{t}/defaulted_fields")));
        }
    }
    Ok(())
}

/// JSON schema (draft 2020-12) of the sequence-preset section.
pub fn sequence_presets_schema() -> schemars::Schema {
    schemars::generate::SchemaSettings::draft2020_12().into_generator().into_root_schema_for::<StudioSequencePresetSet>()
}

/// Parses a preset section from wire JSON (shared Folio preflight; duplicate keys
/// `DuplicateField`, unknown fields and wrong types `InvalidJson`) and validates it.
pub fn parse_sequence_presets(input: &[u8], b: Budget, token: &CancellationToken) -> Result<StudioSequencePresetSet> {
    let set: StudioSequencePresetSet = parse_wire(input, b, token, "sequence_presets")?;
    validate_sequence_presets(&set, b, token)?;
    Ok(set)
}

pub(crate) fn parse_wire<T: serde::de::DeserializeOwned>(input: &[u8], b: Budget, token: &CancellationToken, target: &str) -> Result<T> {
    super::preflight(input, b, token)?;
    let value = serde_json::from_slice::<super::UniqueValue>(input)
        .map_err(|e| err(if e.to_string().contains("duplicate_field") { Code::DuplicateField } else { Code::InvalidJson }, target))?
        .0;
    serde_json::from_value(value).map_err(|_| err(Code::InvalidJson, target))
}

// ---------------------------------------------------------------------------------------------
// Shipped data (rows generated from the spec tables into `shipped.rs`).

/// STU-VID-013 canonical ticks per frame of the rates the 016a table uses.
mod frame_rates {
    pub(super) const TPF_23_976: u64 = 10_594_584_000;
    pub(super) const TPF_24: u64 = 10_584_000_000;
    pub(super) const TPF_25: u64 = 10_160_640_000;
    pub(super) const TPF_29_97: u64 = 8_475_667_200;
    pub(super) const TPF_30: u64 = 8_467_200_000;
    pub(super) const TPF_48: u64 = 5_292_000_000;
    pub(super) const TPF_50: u64 = 5_080_320_000;
    pub(super) const TPF_59_94: u64 = 4_237_833_600;
    pub(super) const TPF_60: u64 = 4_233_600_000;
}

/// 016a "Working colour space" column: declared BT.709, declared BT.2100 HLG, or `--`.
#[derive(Clone, Copy)]
enum ShippedCs {
    B709,
    B2100,
    N,
}
/// 016a "Field order" column.
#[derive(Clone, Copy)]
enum ShippedField {
    P,
    U,
    L,
}
/// 016a "Audio" column channel part: `stereo` or `multichannel / adaptive`.
#[derive(Clone, Copy)]
enum ShippedAudio {
    S,
    M,
}

/// One 016a row: name, group, Studio preset id, width, height, PAR terms, ticks per frame, field
/// order, time display (`None` = code 113), sample rate, channels, video tracks, colour space.
struct ShippedPresetRow(
    &'static str,
    &'static str,
    &'static str,
    u32,
    u32,
    u32,
    u32,
    u64,
    ShippedField,
    Option<StudioTimeDisplay>,
    u32,
    ShippedAudio,
    u8,
    ShippedCs,
);

impl ShippedPresetRow {
    /// The complete settings record of the row. Declared by the row: frame size, PAR, frame rate,
    /// field order, video time display, sample rate and master channels, video track count and (for
    /// 27 rows) the working colour space; `audio_time_display` is `audio_samples` for all 392
    /// (STU-VID-011, declared); `auto_tone_map` is true for the BT.2100 HLG (HDR-capable) rows
    /// (declared). Everything else is named in `defaulted_fields`. STU-VID-011 Studio defaults:
    /// working colour space `BT.709 RGB Full` where the row has `--`; maximum bit depth and render
    /// quality false; linear compositing true; 4 audio tracks (the clause ties 4 to the stereo
    /// configurations; applying it to the multichannel rows is a Studio reading). Studio readings, no
    /// default declared: `auto_tone_map` false outside the HDR rows; the preview format at the
    /// sequence frame size with the preview tier's codec (null); immersive null (flat).
    fn preset(&self) -> StudioSequencePreset {
        let Self(name, group, preset_id, width, height, pn, pd, tpf, field, display, rate, audio, video_tracks, cs) = *self;
        let frame_size = SequenceFrameSize { width, height };
        let hdr = matches!(cs, ShippedCs::B2100);
        let mut defaulted = Vec::new();
        let color_space = match cs {
            ShippedCs::B709 => SequenceColorSpace::Bt709RgbFull,
            ShippedCs::B2100 => SequenceColorSpace::Bt2100HlgRgbFull,
            ShippedCs::N => {
                defaulted.push(SequenceSettingsField::WorkingColorSpace);
                SequenceColorSpace::Bt709RgbFull
            }
        };
        if !hdr {
            defaulted.push(SequenceSettingsField::AutoToneMap);
        }
        defaulted.extend([
            SequenceSettingsField::UseMaximumBitDepth,
            SequenceSettingsField::UseMaximumRenderQuality,
            SequenceSettingsField::AllowLinearCompositing,
            SequenceSettingsField::InitialAudioTrackCount,
            SequenceSettingsField::PreviewRenderFormat,
            SequenceSettingsField::Immersive,
        ]);
        defaulted.sort();
        let (video_time_display, warnings) = match display {
            Some(d) => (d, Vec::new()),
            None => (StudioTimeDisplay::Frames, vec![SequencePresetWarning::TimeDisplayCode113]),
        };
        StudioSequencePreset {
            preset_id: preset_id.into(),
            name: name.into(),
            category_path: group.split('/').map(str::to_owned).collect(),
            settings: StudioSequenceSettings {
                frame_size,
                pixel_aspect_ratio: SequencePixelAspectRatio { numerator: pn, denominator: pd },
                frame_rate: SequenceFrameRate { ticks_per_frame: tpf },
                field_order: match field {
                    ShippedField::P => SequenceFieldOrder::Progressive,
                    ShippedField::U => SequenceFieldOrder::UpperFieldFirst,
                    ShippedField::L => SequenceFieldOrder::LowerFieldFirst,
                },
                video_time_display,
                audio_time_display: StudioTimeDisplay::AudioSamples,
                audio_sample_rate_hz: rate,
                audio_master_channel_type: match audio {
                    ShippedAudio::S => AudioMasterChannelType::Stereo,
                    ShippedAudio::M => AudioMasterChannelType::MultichannelAdaptive,
                },
                working_color_space: SequenceWorkingColorSpace { color_space, is_linearized: false },
                auto_tone_map: hdr,
                use_maximum_bit_depth: false,
                use_maximum_render_quality: false,
                allow_linear_compositing: true,
                initial_video_track_count: video_tracks,
                initial_audio_track_count: 4,
                preview_render_format: StudioPreviewRenderFormat { codec: None, frame_size },
                immersive: None,
            },
            defaulted_fields: defaulted,
            warnings,
        }
    }
}

/// One 017a row: Studio mode id, name, Windows GUID, platforms, slot counts.
struct ShippedModeRow(&'static str, Option<&'static str>, Option<&'static str>, &'static [EditingModePlatform], [u16; 4]);

impl ShippedModeRow {
    fn mode(&self) -> StudioEditingMode {
        let [frame_rate, frame_rect, pixel_aspect, field_type] = self.4;
        StudioEditingMode {
            mode_id: self.0.into(),
            name: self.1.map(str::to_owned),
            windows_guid: self.2.map(str::to_owned),
            platforms: self.3.to_vec(),
            slots: StudioEditingModeSlots { frame_rate, frame_rect, pixel_aspect, field_type },
            constraints_known: false,
            constraints: StudioEditingModeConstraints::default(),
        }
    }
}

/// The shipped sequence-preset library: the 391 complete rows of the STU-VID-016a table, in table
/// order (category path = the group column split at `/`).
pub fn shipped_sequence_presets() -> StudioSequencePresetSet {
    StudioSequencePresetSet {
        schema_id: StyleRegistrySchema::V1,
        presets: shipped::SHIPPED_PRESET_ROWS.iter().map(ShippedPresetRow::preset).collect(),
    }
}

// ---------------------------------------------------------------------------------------------
// Commands.

/// Command ids, in [`StudioSequenceSettingsCommand`] variant order.
pub const SEQUENCE_SETTINGS_COMMAND_IDS: [&str; 3] =
    ["sequence.preset.save_from_sequence", "sequence.preset.apply", "sequence.editing_mode.set"];

/// A typed sequence-settings command (STU-VID-016a, STU-VID-017).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StudioSequenceSettingsCommand {
    /// Saves the current settings of `sequence_id` as the new preset `preset_id` (STU-VID-016a).
    /// The sequence must have a settings entry: an earlier-shape sequence has no complete record.
    SavePresetFromSequence { sequence_id: String, preset_id: String, name: String, category_path: Vec<String> },
    /// Writes the settings of `preset_id` to `sequence_id`, including its frame rate (set on the
    /// sequence too). A sequence without an entry gains one with the `custom` editing mode; an
    /// existing entry keeps its mode, whose allow-lists the settings must meet.
    ApplyPreset { preset_id: String, sequence_id: String },
    /// Sets the editing mode of `sequence_id` (a shipped mode whose allow-lists the settings meet).
    SetEditingMode { sequence_id: String, mode_id: String },
}
impl StudioSequenceSettingsCommand {
    /// The command id the caller's context must carry.
    pub const fn command_id(&self) -> &'static str {
        match self {
            Self::SavePresetFromSequence { .. } => SEQUENCE_SETTINGS_COMMAND_IDS[0],
            Self::ApplyPreset { .. } => SEQUENCE_SETTINGS_COMMAND_IDS[1],
            Self::SetEditingMode { .. } => SEQUENCE_SETTINGS_COMMAND_IDS[2],
        }
    }
}

/// A typed refusal; nothing changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StudioSequenceSettingsError {
    /// The context carries no command id.
    CommandMissing,
    /// The context's command id is not this command's.
    CommandMismatch,
    StaleRevision { expected: u64, current: u64 },
    /// The inputs fail their own validation; the Folio diagnostic code. Cancellation and budget
    /// overruns are not bad input: they are [`Self::Canceled`] and [`Self::BudgetExceeded`] (test
    /// `command_codes_tell_cancellation_and_budget_from_bad_input_r55_f6`).
    InvalidInput(Code),
    SequenceMissing,
    /// The sequence has no settings entry (an earlier-shape sequence).
    SettingsMissing,
    PresetMissing,
    DuplicatePreset,
    InvalidPresetId,
    InvalidName,
    InvalidCategory,
    ModeUnknown,
    /// The settings fall outside the editing mode's allow-lists.
    ModeViolation,
    BudgetExceeded,
    /// The cancellation token was set while the inputs or results were validated.
    Canceled,
}
impl StudioSequenceSettingsError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::CommandMissing => "command_missing",
            Self::CommandMismatch => "command_mismatch",
            Self::StaleRevision { .. } => "stale_revision",
            Self::InvalidInput(_) => "invalid_input",
            Self::SequenceMissing => "sequence_missing",
            Self::SettingsMissing => "settings_missing",
            Self::PresetMissing => "preset_missing",
            Self::DuplicatePreset => "duplicate_preset",
            Self::InvalidPresetId => "invalid_preset_id",
            Self::InvalidName => "invalid_name",
            Self::InvalidCategory => "invalid_category",
            Self::ModeUnknown => "mode_unknown",
            Self::ModeViolation => "mode_violation",
            Self::BudgetExceeded => "budget_exceeded",
            Self::Canceled => "canceled",
        }
    }
}
impl std::fmt::Display for StudioSequenceSettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for StudioSequenceSettingsError {}

/// What a host emits through Observe for one applied command: codes, counters and the opaque
/// correlation only (no names, ids or settings values).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StudioSequenceSettingsReport {
    pub command: &'static str,
    pub base_revision: u64,
    pub correlation: u64,
    /// Presets of the resulting preset set.
    pub presets: usize,
    /// Settings entries of the resulting record set.
    pub settings_entries: usize,
    /// Whether the sequence's frame rate changed.
    pub frame_rate_changed: bool,
}

/// An applied command: the resulting document (only a sequence frame rate can change), settings
/// records and preset set, and the report. The caller's inputs are the undo state.
#[derive(Clone, Debug, PartialEq)]
pub struct StudioSequenceSettingsOutcome {
    pub document: StudioDocument,
    pub records: StudioSequenceSettingsRecords,
    pub presets: StudioSequencePresetSet,
    pub report: StudioSequenceSettingsReport,
}

/// Applies one command at `revision` (the caller's current document revision; the context's base
/// revision must equal it and its command id must be `cmd.command_id()`). The inputs are validated
/// first (`InvalidInput`), then the command runs on copies and the results are validated again;
/// a refusal returns the typed error and changes nothing.
#[allow(clippy::too_many_arguments)]
pub fn apply_sequence_settings_command(
    document: &StudioDocument,
    records: &StudioSequenceSettingsRecords,
    presets: &StudioSequencePresetSet,
    ctx: &CommandContext,
    revision: u64,
    cmd: &StudioSequenceSettingsCommand,
    b: Budget,
    token: &CancellationToken,
) -> std::result::Result<StudioSequenceSettingsOutcome, StudioSequenceSettingsError> {
    use StudioSequenceSettingsError as E;
    let c = ctx.command().ok_or(E::CommandMissing)?;
    if c.as_str() != cmd.command_id() {
        return Err(E::CommandMismatch);
    }
    if ctx.base_revision() != revision {
        return Err(E::StaleRevision { expected: ctx.base_revision(), current: revision });
    }
    let invalid = |d: super::Diagnostic| match d.code {
        Code::Canceled => E::Canceled,
        Code::BudgetExceeded => E::BudgetExceeded,
        code => E::InvalidInput(code),
    };
    validate_sequence_settings_records(records, document, b, token).map_err(invalid)?;
    validate_sequence_presets(presets, b, token).map_err(invalid)?;
    let mut document = document.clone();
    let mut records = records.clone();
    let mut presets = presets.clone();
    let mut frame_rate_changed = false;
    let sequence = |document: &StudioDocument, id: &str| document.sequences.iter().position(|s| s.sequence_id == id).ok_or(E::SequenceMissing);
    match cmd {
        StudioSequenceSettingsCommand::SavePresetFromSequence { sequence_id, preset_id, name, category_path } => {
            sequence(&document, sequence_id)?;
            let settings = records.entry(sequence_id).ok_or(E::SettingsMissing)?.settings.clone();
            if !key(preset_id) {
                return Err(E::InvalidPresetId);
            }
            if presets.preset(preset_id).is_some() {
                return Err(E::DuplicatePreset);
            }
            if !text(name) {
                return Err(E::InvalidName);
            }
            if !category_ok(category_path) {
                return Err(E::InvalidCategory);
            }
            if presets.presets.len() >= MAX_SEQUENCE_PRESETS {
                return Err(E::BudgetExceeded);
            }
            presets.presets.push(StudioSequencePreset {
                preset_id: preset_id.clone(),
                name: name.clone(),
                category_path: category_path.clone(),
                settings,
                defaulted_fields: Vec::new(),
                warnings: Vec::new(),
            });
        }
        StudioSequenceSettingsCommand::ApplyPreset { preset_id, sequence_id } => {
            let i = sequence(&document, sequence_id)?;
            let settings = presets.preset(preset_id).ok_or(E::PresetMissing)?.settings.clone();
            let mode_id = records.entry(sequence_id).map_or(CUSTOM_EDITING_MODE.to_owned(), |e| e.editing_mode.clone());
            let mode = shipped_editing_mode(&mode_id).ok_or(E::ModeUnknown)?;
            if !editing_mode_violations(&mode, &settings).is_empty() {
                return Err(E::ModeViolation);
            }
            let seq = &mut document.sequences[i];
            frame_rate_changed = seq.frame_rate != settings.frame_rate;
            seq.frame_rate = settings.frame_rate;
            match records.sequences.iter_mut().find(|e| &e.sequence_id == sequence_id) {
                Some(e) => e.settings = settings,
                None => records.sequences.push(StudioSequenceSettingsEntry {
                    sequence_id: sequence_id.clone(),
                    editing_mode: mode_id,
                    settings,
                }),
            }
        }
        StudioSequenceSettingsCommand::SetEditingMode { sequence_id, mode_id } => {
            sequence(&document, sequence_id)?;
            let mode = shipped_editing_mode(mode_id).ok_or(E::ModeUnknown)?;
            let entry = records.sequences.iter_mut().find(|e| &e.sequence_id == sequence_id).ok_or(E::SettingsMissing)?;
            if !editing_mode_violations(&mode, &entry.settings).is_empty() {
                return Err(E::ModeViolation);
            }
            entry.editing_mode = mode_id.clone();
        }
    }
    // Only a sequence frame rate changed in the document; the records check re-proves it is a
    // canonical rate equal to the entry's.
    validate_sequence_settings_records(&records, &document, b, token).map_err(invalid)?;
    validate_sequence_presets(&presets, b, token).map_err(invalid)?;
    let report = StudioSequenceSettingsReport {
        command: cmd.command_id(),
        base_revision: ctx.base_revision(),
        correlation: ctx.correlation().value(),
        presets: presets.presets.len(),
        settings_entries: records.sequences.len(),
        frame_rate_changed,
    };
    Ok(StudioSequenceSettingsOutcome { document, records, presets, report })
}
