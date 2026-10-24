//! Multicam records (lane SMB-PR-MULTICAM-1): the per-document record set `StudioMulticamRecords`
//! (schema `hsk.studio.multicam_records@1`) beside the document like the clip finishing record set
//! (`StudioDocument`, `StudioSequence`, `StudioTrack` and `StudioClip` gain no fields and
//! `ClipSource` gains no variant, so every existing document and clip literal is unchanged).
//!
//! Spec basis: STU-VID-033 "Multicam is a clip mode, not a separate editor. A multicam source
//! sequence holds N synchronised angles; a clip referencing it exposes an `active_angle` that is
//! switchable over time, producing a cut list"; "Angle count is at least 16 in the addressable
//! command surface; Studio imposes no fixed maximum beyond a document performance budget";
//! "`audio_follows_video` is a boolean"; "A selection-order option (top-down versus source order)
//! decides angle numbering". STU-VID-030a "multicam clips MAY be set to follow or ignore the nest
//! setting. Both are explicit booleans on the nested clip, never implicit."
//!
//! Stored (data only; switching, record passes, navigation and flatten are Pulse's planners):
//! `multicam_sequences` declares which document sequences are multicam source sequences and their
//! angle numbering; `clips` holds the multicam mode of every clip that references one (the clip's
//! existing `source_ref` `ClipSource::Sequence { sequence_id }` is the reference; a cut list is the
//! ordered run of such clips on a track, each with its own active angle).
//!
//! Studio readings, declared (STU-VID-033 does not state them; lane notes `spec_deltas`):
//! - one angle = one VIDEO track of the source sequence; the audio of angle n is the audio track
//!   whose per-kind `index` equals the `index` of angle n's video track (none = a silent angle);
//! - numbering: `source_order` numbers the video tracks by ascending `index` (the order the angles
//!   were added), `top_down` by descending `index` (the top of the video stack is angle 1);
//!   `angle_order` is that numbering written out and validated, never free;
//! - `audio_angle` is the angle whose audio a clip plays; while `audio_follows_video` it equals
//!   `active_angle` (an angle switch moves both); without it a switch moves the picture only;
//! - a clip on a video track shows `active_angle`, a clip on an audio track plays `audio_angle`;
//! - `enabled` false (command `cmd.clip.multicam.toggle`) plays the source sequence as an ordinary
//!   nest and keeps the stored angles.
//!
//! Refusals: out-of-range angles and a wrong `angle_order` `InvalidField`; an unknown sequence, track
//! or clip, or a clip whose `source_ref` is not a declared multicam source `DanglingReference`;
//! duplicate sequence or clip entries `DuplicateIdentity`; counts beyond the hard ceilings or the
//! caller node budget `BudgetExceeded`. The records arrive from untrusted wire JSON: integer
//! arithmetic is denied (workflow V4-01) and every list is counted before it is walked.
#![deny(clippy::arithmetic_side_effects)]
use super::{
    Budget, ClipSource, Code, Result, StudioDocument, StudioSequence, TrackKind, err, id, key, values::Ctx,
};
use hsk_studio_accord::CancellationToken;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

schema_token!(MulticamRecordsSchema, "hsk.studio.multicam_records@1");

/// Multicam source sequences in one record set (a document holds at most `MAX_SEQUENCES`).
pub const MAX_MULTICAM_SEQUENCES: usize = super::MAX_SEQUENCES;
/// Angles of one multicam source sequence (one per video track; STU-VID-033 sets no maximum, the
/// document's per-sequence track ceiling is the performance budget).
pub const MAX_MULTICAM_ANGLES: usize = super::MAX_TRACKS_PER_SEQUENCE;
/// Smallest angle count the command surface must address (STU-VID-033 "at least 16").
pub const MIN_ADDRESSABLE_ANGLES: usize = 16;
/// Multicam clips in one record set.
pub const MAX_MULTICAM_CLIPS: usize = super::MAX_CLIPS_PER_SEQUENCE;

/// STU-VID-033 selection order: decides angle numbering (module docs for the Studio reading).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MulticamSelectionOrder {
    /// Angle 1 is the video track with the highest `index` (the top of the stack).
    TopDown,
    /// Angle 1 is the video track with `index` 0 (the order the angles were added).
    SourceOrder,
}

/// One multicam source sequence and its angle numbering.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioMulticamSequence {
    /// The document sequence holding the synchronised angles.
    pub sequence_id: String,
    pub selection_order: MulticamSelectionOrder,
    /// Angle n (1-based) is the video track `angle_order[n - 1]`; exactly every video track of the
    /// sequence, in the order `selection_order` gives.
    pub angle_order: Vec<String>,
}

/// The multicam mode of one clip whose `source_ref` names a multicam source sequence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioMulticamClip {
    pub clip_id: String,
    /// 1-based angle the clip shows.
    pub active_angle: u32,
    /// 1-based angle whose audio the clip plays; equals `active_angle` while `audio_follows_video`.
    pub audio_angle: u32,
    /// STU-VID-033: when true an angle switch also switches the audio.
    pub audio_follows_video: bool,
    /// STU-VID-030a: explicit follow-nest-setting boolean on the nested clip.
    pub follows_nest_setting: bool,
    /// Multicam mode on (`cmd.clip.multicam.toggle`); off plays the source as an ordinary nest.
    pub enabled: bool,
}

/// The per-document multicam record set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioMulticamRecords {
    pub schema_id: MulticamRecordsSchema,
    /// `SDOC` id of the annotated document.
    pub document_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub multicam_sequences: Vec<StudioMulticamSequence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clips: Vec<StudioMulticamClip>,
}
impl StudioMulticamRecords {
    /// An empty set for `document_id`.
    pub fn new(document_id: impl Into<String>) -> Self {
        Self {
            schema_id: MulticamRecordsSchema::V1,
            document_id: document_id.into(),
            multicam_sequences: Vec::new(),
            clips: Vec::new(),
        }
    }
    /// The declared multicam source sequence `sequence_id`.
    pub fn multicam_sequence(&self, sequence_id: &str) -> Option<&StudioMulticamSequence> {
        self.multicam_sequences.iter().find(|s| s.sequence_id == sequence_id)
    }
    /// The multicam mode of clip `clip_id`.
    pub fn clip(&self, clip_id: &str) -> Option<&StudioMulticamClip> {
        self.clips.iter().find(|c| c.clip_id == clip_id)
    }
    /// Mutable multicam mode of clip `clip_id`.
    pub fn clip_mut(&mut self, clip_id: &str) -> Option<&mut StudioMulticamClip> {
        self.clips.iter_mut().find(|c| c.clip_id == clip_id)
    }
}

/// The angle numbering `order` gives the video tracks of `sequence` (module docs): track ids, angle 1
/// first. Pulse and the validator share it, so a stored `angle_order` is never free.
pub fn multicam_angle_order(sequence: &StudioSequence, order: MulticamSelectionOrder) -> Vec<String> {
    let mut video: Vec<(u32, &str)> = sequence
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video)
        .map(|t| (t.index, t.track_id.as_str()))
        .collect();
    video.sort_unstable();
    if order == MulticamSelectionOrder::TopDown {
        video.reverse();
    }
    video.into_iter().map(|(_, t)| t.to_owned()).collect()
}

/// The audio track paired with video track `video_track_id` of `sequence`: the audio track with the
/// same per-kind `index` (`None` = a silent angle).
pub fn multicam_paired_audio_track<'a>(sequence: &'a StudioSequence, video_track_id: &str) -> Option<&'a str> {
    let index = sequence
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video && t.track_id == video_track_id)?
        .index;
    sequence
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Audio && t.index == index)
        .map(|t| t.track_id.as_str())
}

/// JSON schema (draft 2020-12) of the multicam record set.
pub fn multicam_records_schema() -> schemars::Schema {
    schemars::generate::SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<StudioMulticamRecords>()
}

/// Parses wire JSON (bounded by `Budget::input_bytes` and depth before any parse; duplicate keys
/// refused) and validates it against the document.
pub fn parse_multicam_records(
    input: &[u8],
    d: &StudioDocument,
    b: Budget,
    token: &CancellationToken,
) -> Result<StudioMulticamRecords> {
    super::preflight(input, b, token)?;
    let value = serde_json::from_slice::<super::UniqueValue>(input)
        .map_err(|e| {
            err(
                if e.to_string().contains("duplicate_field") { Code::DuplicateField } else { Code::InvalidJson },
                "multicam_records",
            )
        })?
        .0;
    let r: StudioMulticamRecords =
        serde_json::from_value(value).map_err(|_| err(Code::InvalidJson, "multicam_records"))?;
    validate_multicam_records(&r, d, b, token)?;
    Ok(r)
}

/// Validates the record set against the document (module docs).
pub fn validate_multicam_records(
    r: &StudioMulticamRecords,
    d: &StudioDocument,
    b: Budget,
    token: &CancellationToken,
) -> Result<()> {
    let mut cx = Ctx::new(b, token, &super::Unresolved);
    cx.tick()?;
    id(&r.document_id, "SDOC", "document_id")?;
    if r.document_id != d.document_id {
        return Err(err(Code::InvalidField, "document_id"));
    }
    if r.multicam_sequences.len() > MAX_MULTICAM_SEQUENCES {
        return Err(err(Code::BudgetExceeded, "multicam_sequences"));
    }
    if r.clips.len() > MAX_MULTICAM_CLIPS {
        return Err(err(Code::BudgetExceeded, "clips"));
    }
    cx.nodes(r.multicam_sequences.len(), "multicam_sequences")?;
    cx.nodes(r.clips.len(), "clips")?;
    // Declared source sequence -> angle count.
    let mut angles: BTreeMap<&str, usize> = BTreeMap::new();
    for m in &r.multicam_sequences {
        cx.tick()?;
        let t = m.sequence_id.as_str();
        if !key(t) {
            return Err(err(Code::InvalidId, "multicam_sequences/sequence_id"));
        }
        let Some(seq) = d.sequences.iter().find(|s| s.sequence_id == t) else {
            return Err(err(Code::DanglingReference, t));
        };
        if angles.contains_key(t) {
            return Err(err(Code::DuplicateIdentity, t));
        }
        if m.angle_order.len() > MAX_MULTICAM_ANGLES {
            return Err(err(Code::BudgetExceeded, format!("{t}/angle_order")));
        }
        cx.nodes(m.angle_order.len(), t)?;
        for track_id in &m.angle_order {
            if !seq.tracks.iter().any(|k| k.kind == TrackKind::Video && &k.track_id == track_id) {
                return Err(err(Code::DanglingReference, format!("{t}/angle_order")));
            }
        }
        if m.angle_order.is_empty() || m.angle_order != multicam_angle_order(seq, m.selection_order) {
            return Err(err(Code::InvalidField, format!("{t}/angle_order")));
        }
        angles.insert(t, m.angle_order.len());
    }
    // Clip id -> (owning sequence id, clip source).
    let mut owners: BTreeMap<&str, (&str, &ClipSource)> = BTreeMap::new();
    for s in &d.sequences {
        for c in s.tracks.iter().flat_map(|t| t.clips.iter()) {
            owners.insert(c.clip_id.as_str(), (s.sequence_id.as_str(), &c.source_ref));
        }
    }
    let mut seen = BTreeSet::new();
    for c in &r.clips {
        cx.tick()?;
        let t = c.clip_id.as_str();
        id(t, "SCLP", "clips/clip_id")?;
        let Some((owner, source)) = owners.get(t) else {
            return Err(err(Code::DanglingReference, t));
        };
        if !seen.insert(t) {
            return Err(err(Code::DuplicateIdentity, t));
        }
        let ClipSource::Sequence { sequence_id } = source else {
            return Err(err(Code::DanglingReference, format!("{t}/source_ref")));
        };
        if sequence_id == owner {
            return Err(err(Code::InvalidField, format!("{t}/source_ref")));
        }
        let Some(count) = angles.get(sequence_id.as_str()) else {
            return Err(err(Code::DanglingReference, format!("{t}/source_ref")));
        };
        let in_range = |a: u32| usize::try_from(a).is_ok_and(|a| (1..=*count).contains(&a));
        if !in_range(c.active_angle) {
            return Err(err(Code::InvalidField, format!("{t}/active_angle")));
        }
        if !in_range(c.audio_angle) || (c.audio_follows_video && c.audio_angle != c.active_angle) {
            return Err(err(Code::InvalidField, format!("{t}/audio_angle")));
        }
    }
    Ok(())
}
