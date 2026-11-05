//! Track display state per sequence (lane SMB-PR-TRACK-DISPLAY-1; STU-VID-036).
//!
//! [`StudioTrackDisplayRecords`] (`hsk.studio.track_display@1`) holds one entry per sequence, beside
//! the document, the placement of the sequence settings records (lead decision, spec-owner item
//! SD-47). An entry names its sequence and copies no other document value, so editing a sequence's
//! tracks or frame rate cannot make it stale (test `the_records_hold_no_copy_of_a_document_value`).
//! A sequence with no entry resolves to the declared defaults ([`resolve_track_displays`], which also
//! says whether the state is stored), so nothing else has to report it.
//!
//! Display state is a view preference and blocks no document edit (lead decision on this lane). An
//! entry naming a sequence the document does not hold is inert: valid, kept and listed by
//! [`inert_track_display_entries`], and live again when the sequence comes back, as an undo brings it.
//! That is this record set's one difference from the other per-sequence records, which refuse a
//! dangling reference. The clause is silent on a removed sequence; vendor parity is UNVERIFIED (D5).
//!
//! The declared defaults, bounds, command ids and the height unit (logical pixels) are Studio's: the
//! clause states none of them. Vendor parity is UNVERIFIED (D5). Audio and video fade handles and
//! audio and video keyframes are one switch per medium, a Studio reading of the clause's list.
//!
//! [`StudioTrackDisplayCommand`] covers the switches, the height of one track kind, expand-all,
//! minimize-all, and saving and recalling a named height preset. Each takes the caller's
//! `CommandContext` and returns the next records and a report of codes and counters; it holds no
//! document, so the revision and the validation of a commit stay with the caller.
//!
//! The records arrive from untrusted wire JSON: integer arithmetic is denied (workflow V4-01), every
//! list is counted before it is walked and charged against the [`Budget`], and a parse refuses bytes
//! over [`MAX_TRACK_DISPLAY_INPUT_BYTES`] whatever the budget allows.
#![deny(clippy::arithmetic_side_effects)]
use super::{
    Budget, Code, Diagnostic, MAX_SEQUENCES, Result, StudioDocument, TrackKind, Unresolved, err, id, key,
    sequence_preset::parse_wire,
    values::{Ctx, label},
};
use hsk_studio_accord::{CancellationToken, CommandContext};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

schema_token!(TrackDisplaySchema, "hsk.studio.track_display@1");

/// Largest track height in logical pixels (Studio hostile-input bound).
pub const MAX_TRACK_HEIGHT: u32 = 4096;
/// Smallest track height, and the height minimize-all sets.
pub const MIN_TRACK_HEIGHT: u32 = 16;
/// The height expand-all sets.
pub const EXPANDED_TRACK_HEIGHT: u32 = 200;
/// The height of every track kind of a sequence with no entry.
pub const DEFAULT_TRACK_HEIGHT: u32 = 48;
/// Entries one record set may hold, inert ones included.
pub const MAX_TRACK_DISPLAY_ENTRIES: usize = MAX_SEQUENCES;
/// Named height presets one sequence may hold.
pub const MAX_TRACK_HEIGHT_PRESETS: usize = 64;
/// Largest wire form of a record set in bytes, whatever the caller's budget allows.
pub const MAX_TRACK_DISPLAY_INPUT_BYTES: usize = 1_048_576;

/// STU-VID-036 video thumbnails.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioVideoThumbnails {
    Off,
    HeadOnly,
    HeadAndTail,
    Continuous,
}

/// STU-VID-036 audio waveform scaling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioWaveformScaling {
    Logarithmic,
    Dynamic,
}

/// STU-VID-036 audio waveforms: shown or not, with their scaling and the rectified option.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioAudioWaveforms {
    pub shown: bool,
    pub scaling: StudioWaveformScaling,
    pub rectified: bool,
}

/// The STU-VID-036 display switches, every one required on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioTrackDisplaySwitches {
    pub video_thumbnails: StudioVideoThumbnails,
    pub clip_names: bool,
    pub fx_badges: bool,
    pub proxy_badges: bool,
    pub content_credential_badges: bool,
    pub duplicate_frame_markers: bool,
    pub clip_markers: bool,
    pub source_clip_name_and_label: bool,
    pub through_edits: bool,
    pub audio_waveforms: StudioAudioWaveforms,
    pub audio_names: bool,
    pub audio_channel_labels: bool,
    pub audio_type_badges: bool,
    pub audio_fade_handles: bool,
    pub video_fade_handles: bool,
    pub audio_keyframes: bool,
    pub video_keyframes: bool,
    pub audio_clip_headers_on_small_tracks: bool,
}
impl StudioTrackDisplaySwitches {
    /// The declared defaults (a Studio reading; vendor parity UNVERIFIED, D5): nothing hidden, head
    /// thumbnails, logarithmic waveforms that are not rectified.
    pub const STUDIO_DEFAULT: Self = Self {
        video_thumbnails: StudioVideoThumbnails::HeadOnly,
        clip_names: true,
        fx_badges: true,
        proxy_badges: true,
        content_credential_badges: true,
        duplicate_frame_markers: true,
        clip_markers: true,
        source_clip_name_and_label: true,
        through_edits: true,
        audio_waveforms: StudioAudioWaveforms { shown: true, scaling: StudioWaveformScaling::Logarithmic, rectified: false },
        audio_names: true,
        audio_channel_labels: true,
        audio_type_badges: true,
        audio_fade_handles: true,
        video_fade_handles: true,
        audio_keyframes: true,
        video_keyframes: true,
        audio_clip_headers_on_small_tracks: true,
    };
}

fn valid_height(height: &u32) -> bool {
    (MIN_TRACK_HEIGHT..=MAX_TRACK_HEIGHT).contains(height)
}

fn valid_preset_name(name: &str) -> bool {
    !name.trim().is_empty() && label(name)
}

/// One height per track kind, in logical pixels, each `MIN_TRACK_HEIGHT..=MAX_TRACK_HEIGHT`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioTrackHeights {
    pub video: u32,
    pub audio: u32,
    pub caption: u32,
    pub submix: u32,
}
impl StudioTrackHeights {
    /// `height` for every track kind.
    pub const fn all(height: u32) -> Self {
        Self { video: height, audio: height, caption: height, submix: height }
    }
    /// The declared defaults.
    pub const STUDIO_DEFAULT: Self = Self::all(DEFAULT_TRACK_HEIGHT);
    /// The height of `kind`.
    pub const fn of(&self, kind: TrackKind) -> u32 {
        match kind {
            TrackKind::Video => self.video,
            TrackKind::Audio => self.audio,
            TrackKind::Caption => self.caption,
            TrackKind::Submix => self.submix,
        }
    }
    /// Every height is in bounds.
    fn valid(&self) -> bool {
        [self.video, self.audio, self.caption, self.submix].iter().all(valid_height)
    }
    fn set(&mut self, kind: TrackKind, height: u32) {
        match kind {
            TrackKind::Video => self.video = height,
            TrackKind::Audio => self.audio = height,
            TrackKind::Caption => self.caption = height,
            TrackKind::Submix => self.submix = height,
        }
    }
}

/// A named set of track heights of one sequence, saved and recalled by `preset_id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioTrackHeightPreset {
    /// Unique within the entry (key grammar).
    pub preset_id: String,
    pub name: String,
    pub heights: StudioTrackHeights,
}

/// The track display state of one document sequence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioTrackDisplayEntry {
    /// A sequence of the document.
    pub sequence_id: String,
    pub switches: StudioTrackDisplaySwitches,
    pub heights: StudioTrackHeights,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub height_presets: Vec<StudioTrackHeightPreset>,
}

/// The per-document track display record set, one per `document_id`, beside the document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioTrackDisplayRecords {
    pub schema_id: TrackDisplaySchema,
    /// `SDOC` id of the annotated document.
    pub document_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sequences: Vec<StudioTrackDisplayEntry>,
}
impl StudioTrackDisplayRecords {
    /// An empty set for `document_id`.
    pub fn new(document_id: impl Into<String>) -> Self {
        Self { schema_id: TrackDisplaySchema::V1, document_id: document_id.into(), sequences: Vec::new() }
    }
    /// The entry of sequence `sequence_id`.
    pub fn entry(&self, sequence_id: &str) -> Option<&StudioTrackDisplayEntry> {
        self.sequences.iter().find(|e| e.sequence_id == sequence_id)
    }
}

/// The display state a sequence resolves to: its entry's, or the declared defaults.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StudioResolvedTrackDisplay {
    pub sequence_id: String,
    /// False when the sequence has no entry and these are the declared defaults.
    pub stored: bool,
    pub switches: StudioTrackDisplaySwitches,
    pub heights: StudioTrackHeights,
    pub height_presets: Vec<StudioTrackHeightPreset>,
}

/// Every document sequence with the display state it resolves to, in document order. `records` must
/// already be valid against `d`.
pub fn resolve_track_displays(records: Option<&StudioTrackDisplayRecords>, d: &StudioDocument) -> Vec<StudioResolvedTrackDisplay> {
    let resolve = |sequence_id: &String| match records.and_then(|r| r.entry(sequence_id)) {
        Some(e) => StudioResolvedTrackDisplay {
            sequence_id: sequence_id.clone(),
            stored: true,
            switches: e.switches,
            heights: e.heights,
            height_presets: e.height_presets.clone(),
        },
        None => StudioResolvedTrackDisplay {
            sequence_id: sequence_id.clone(),
            stored: false,
            switches: StudioTrackDisplaySwitches::STUDIO_DEFAULT,
            heights: StudioTrackHeights::STUDIO_DEFAULT,
            height_presets: Vec::new(),
        },
    };
    d.sequences.iter().map(|s| resolve(&s.sequence_id)).collect()
}

/// The entries that name a sequence the document does not hold, in stored order. Nothing removes one
/// on its own; a caller writes records without it.
pub fn inert_track_display_entries<'a>(records: &'a StudioTrackDisplayRecords, d: &StudioDocument) -> Vec<&'a StudioTrackDisplayEntry> {
    records.sequences.iter().filter(|e| !d.sequences.iter().any(|s| s.sequence_id == e.sequence_id)).collect()
}

/// Validates the record set against the document: `SDOC` id equal to the document's; at most
/// `MAX_TRACK_DISPLAY_ENTRIES` entries, inert ones included, and `MAX_TRACK_HEIGHT_PRESETS` presets per
/// entry (each list counted before it is walked, then charged against `Budget::nodes`, so the two count
/// together); each entry names a sequence id of the key grammar (`InvalidId`) once
/// (`DuplicateIdentity`), a sequence the document holds or an inert one; heights in bounds
/// (`InvalidField`); preset ids of the key grammar (`InvalidId`), unique in the entry
/// (`DuplicateIdentity`); preset names non-blank, at most 128 bytes, no control characters
/// (`InvalidField`, charged against `Budget::payload_bytes`).
pub fn validate_track_display_records(r: &StudioTrackDisplayRecords, d: &StudioDocument, b: Budget, token: &CancellationToken) -> Result<()> {
    let mut cx = Ctx::new(b, token, &Unresolved);
    cx.tick()?;
    id(&r.document_id, "SDOC", "document_id")?;
    if r.document_id != d.document_id {
        return Err(err(Code::InvalidField, "document_id"));
    }
    if r.sequences.len() > MAX_TRACK_DISPLAY_ENTRIES {
        return Err(err(Code::BudgetExceeded, "track_display"));
    }
    cx.nodes(r.sequences.len(), "track_display")?;
    let mut seen = BTreeSet::new();
    for e in &r.sequences {
        cx.tick()?;
        let t = e.sequence_id.as_str();
        if !key(t) {
            return Err(err(Code::InvalidId, "track_display/sequence_id"));
        }
        if !seen.insert(t) {
            return Err(err(Code::DuplicateIdentity, t));
        }
        if !e.heights.valid() {
            return Err(err(Code::InvalidField, format!("{t}/heights")));
        }
        if e.height_presets.len() > MAX_TRACK_HEIGHT_PRESETS {
            return Err(err(Code::BudgetExceeded, format!("{t}/height_presets")));
        }
        cx.nodes(e.height_presets.len(), t)?;
        let mut presets = BTreeSet::new();
        for p in &e.height_presets {
            cx.tick()?;
            if !key(&p.preset_id) {
                return Err(err(Code::InvalidId, format!("{t}/height_presets/preset_id")));
            }
            if !presets.insert(p.preset_id.as_str()) {
                return Err(err(Code::DuplicateIdentity, format!("{t}/height_presets/{}", p.preset_id)));
            }
            if !valid_preset_name(&p.name) {
                return Err(err(Code::InvalidField, format!("{t}/height_presets/{}/name", p.preset_id)));
            }
            if !p.heights.valid() {
                return Err(err(Code::InvalidField, format!("{t}/height_presets/{}/heights", p.preset_id)));
            }
            cx.text(&p.name, t)?;
        }
    }
    Ok(())
}

/// JSON schema (draft 2020-12) of the track display record set.
pub fn track_display_records_schema() -> schemars::Schema {
    schemars::generate::SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<StudioTrackDisplayRecords>()
}

/// Parses wire JSON (bounded by [`MAX_TRACK_DISPLAY_INPUT_BYTES`], `Budget::input_bytes` and depth
/// before any parse; duplicate keys `DuplicateField`, unknown fields, a missing field and wrong types
/// `InvalidJson`) and validates it against the document.
pub fn parse_track_display_records(input: &[u8], d: &StudioDocument, b: Budget, token: &CancellationToken) -> Result<StudioTrackDisplayRecords> {
    if input.len() > MAX_TRACK_DISPLAY_INPUT_BYTES {
        return Err(err(Code::BudgetExceeded, "track_display"));
    }
    let r: StudioTrackDisplayRecords = parse_wire(input, b, token, "track_display")?;
    validate_track_display_records(&r, d, b, token)?;
    Ok(r)
}

/// Command ids, in [`StudioTrackDisplayCommand`] variant order.
pub const TRACK_DISPLAY_COMMAND_IDS: [&str; 6] = [
    "track_display.switches.set",
    "track_display.height.set",
    "track_display.expand_all",
    "track_display.minimize_all",
    "track_display.height_preset.save",
    "track_display.height_preset.recall",
];

/// A typed track display command (STU-VID-036). A sequence without an entry starts from the declared
/// defaults, and gains an entry only when the command changes what it resolves to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StudioTrackDisplayCommand {
    /// Writes every switch of `sequence_id`.
    SetSwitches { sequence_id: String, switches: StudioTrackDisplaySwitches },
    /// Sets the height of one track kind.
    SetHeight { sequence_id: String, kind: TrackKind, height: u32 },
    /// Sets every track kind to [`EXPANDED_TRACK_HEIGHT`].
    ExpandAll { sequence_id: String },
    /// Sets every track kind to [`MIN_TRACK_HEIGHT`].
    MinimizeAll { sequence_id: String },
    /// Saves the sequence's current heights as the new preset `preset_id`.
    SaveHeightPreset { sequence_id: String, preset_id: String, name: String },
    /// Sets the sequence's heights to those of its preset `preset_id`.
    RecallHeightPreset { sequence_id: String, preset_id: String },
}
impl StudioTrackDisplayCommand {
    /// The command id the caller's context must carry.
    pub const fn command_id(&self) -> &'static str {
        match self {
            Self::SetSwitches { .. } => TRACK_DISPLAY_COMMAND_IDS[0],
            Self::SetHeight { .. } => TRACK_DISPLAY_COMMAND_IDS[1],
            Self::ExpandAll { .. } => TRACK_DISPLAY_COMMAND_IDS[2],
            Self::MinimizeAll { .. } => TRACK_DISPLAY_COMMAND_IDS[3],
            Self::SaveHeightPreset { .. } => TRACK_DISPLAY_COMMAND_IDS[4],
            Self::RecallHeightPreset { .. } => TRACK_DISPLAY_COMMAND_IDS[5],
        }
    }
}

/// A typed refusal; nothing changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StudioTrackDisplayError {
    /// The context carries no command id.
    CommandMissing,
    /// The context's command id is not this command's.
    CommandMismatch,
    StaleRevision { expected: u64, current: u64 },
    /// The records fail their own validation; the Folio diagnostic code.
    InvalidInput(Code),
    SequenceMissing,
    PresetMissing,
    DuplicatePreset,
    InvalidPresetId,
    InvalidName,
    InvalidHeight,
    BudgetExceeded,
    Canceled,
}
impl StudioTrackDisplayError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::CommandMissing => "command_missing",
            Self::CommandMismatch => "command_mismatch",
            Self::StaleRevision { .. } => "stale_revision",
            Self::InvalidInput(_) => "invalid_input",
            Self::SequenceMissing => "sequence_missing",
            Self::PresetMissing => "preset_missing",
            Self::DuplicatePreset => "duplicate_preset",
            Self::InvalidPresetId => "invalid_preset_id",
            Self::InvalidName => "invalid_name",
            Self::InvalidHeight => "invalid_height",
            Self::BudgetExceeded => "budget_exceeded",
            Self::Canceled => "canceled",
        }
    }
}
impl std::fmt::Display for StudioTrackDisplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for StudioTrackDisplayError {}

/// What a host emits through Observe for one applied command: codes, counters and the opaque
/// correlation only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StudioTrackDisplayReport {
    pub command: &'static str,
    pub base_revision: u64,
    pub correlation: u64,
    /// Entries of the resulting record set.
    pub entries: usize,
    /// Height presets of the command's sequence after it.
    pub height_presets: usize,
    /// Whether the records differ from the input.
    pub changed: bool,
}

/// An applied command: the resulting records and the report. The caller's input is the undo state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StudioTrackDisplayOutcome {
    pub records: StudioTrackDisplayRecords,
    pub report: StudioTrackDisplayReport,
}

/// Applies one command at `revision` (the caller's current document revision; the context's base
/// revision must equal it and its command id must be `cmd.command_id()`). The records are validated
/// first (`InvalidInput`), the command runs on a copy, and the result is validated again and must fit
/// what a parse under `b` accepts (`BudgetExceeded`). A refusal returns the typed error.
pub fn apply_track_display_command(
    document: &StudioDocument,
    records: &StudioTrackDisplayRecords,
    ctx: &CommandContext,
    revision: u64,
    cmd: &StudioTrackDisplayCommand,
    b: Budget,
    token: &CancellationToken,
) -> std::result::Result<StudioTrackDisplayOutcome, StudioTrackDisplayError> {
    use StudioTrackDisplayCommand as C;
    use StudioTrackDisplayError as E;
    let c = ctx.command().ok_or(E::CommandMissing)?;
    if c.as_str() != cmd.command_id() {
        return Err(E::CommandMismatch);
    }
    if ctx.base_revision() != revision {
        return Err(E::StaleRevision { expected: ctx.base_revision(), current: revision });
    }
    let invalid = |d: Diagnostic| match d.code {
        Code::Canceled => E::Canceled,
        Code::BudgetExceeded => E::BudgetExceeded,
        code => E::InvalidInput(code),
    };
    validate_track_display_records(records, document, b, token).map_err(invalid)?;
    let sequence_id = match cmd {
        C::SetSwitches { sequence_id, .. }
        | C::SetHeight { sequence_id, .. }
        | C::ExpandAll { sequence_id }
        | C::MinimizeAll { sequence_id }
        | C::SaveHeightPreset { sequence_id, .. }
        | C::RecallHeightPreset { sequence_id, .. } => sequence_id,
    };
    if !document.sequences.iter().any(|s| &s.sequence_id == sequence_id) {
        return Err(E::SequenceMissing);
    }
    let before = records.entry(sequence_id).cloned().unwrap_or_else(|| StudioTrackDisplayEntry {
        sequence_id: sequence_id.clone(),
        switches: StudioTrackDisplaySwitches::STUDIO_DEFAULT,
        heights: StudioTrackHeights::STUDIO_DEFAULT,
        height_presets: Vec::new(),
    });
    let mut state = before.clone();
    match cmd {
        C::SetSwitches { switches, .. } => state.switches = *switches,
        C::SetHeight { kind, height, .. } => {
            if !valid_height(height) {
                return Err(E::InvalidHeight);
            }
            state.heights.set(*kind, *height);
        }
        C::ExpandAll { .. } => state.heights = StudioTrackHeights::all(EXPANDED_TRACK_HEIGHT),
        C::MinimizeAll { .. } => state.heights = StudioTrackHeights::all(MIN_TRACK_HEIGHT),
        C::SaveHeightPreset { preset_id, name, .. } => {
            if !key(preset_id) {
                return Err(E::InvalidPresetId);
            }
            if state.height_presets.iter().any(|p| &p.preset_id == preset_id) {
                return Err(E::DuplicatePreset);
            }
            if !valid_preset_name(name) {
                return Err(E::InvalidName);
            }
            if state.height_presets.len() >= MAX_TRACK_HEIGHT_PRESETS {
                return Err(E::BudgetExceeded);
            }
            state.height_presets.push(StudioTrackHeightPreset { preset_id: preset_id.clone(), name: name.clone(), heights: state.heights });
        }
        C::RecallHeightPreset { preset_id, .. } => {
            state.heights = state.height_presets.iter().find(|p| &p.preset_id == preset_id).ok_or(E::PresetMissing)?.heights;
        }
    }
    let changed = state != before;
    let mut next = records.clone();
    let height_presets = state.height_presets.len();
    if changed {
        match next.sequences.iter_mut().find(|e| &e.sequence_id == sequence_id) {
            Some(e) => *e = state,
            None => next.sequences.push(state),
        }
        validate_track_display_records(&next, document, b, token).map_err(invalid)?;
        let bytes = serde_json::to_vec(&next).map_err(|_| E::InvalidInput(Code::InvalidJson))?;
        if bytes.len() > MAX_TRACK_DISPLAY_INPUT_BYTES.min(b.input_bytes) {
            return Err(E::BudgetExceeded);
        }
    }
    let report = StudioTrackDisplayReport {
        command: cmd.command_id(),
        base_revision: ctx.base_revision(),
        correlation: ctx.correlation().value(),
        entries: next.sequences.len(),
        height_presets,
        changed,
    };
    Ok(StudioTrackDisplayOutcome { records: next, report })
}
