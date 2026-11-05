//! Link, group and sync-lock operations (STU-VID-029) and merge clips / synchronize (STU-VID-029a),
//! lane SMB-PR-MERGE-SYNC-1. Every planner returns one ordinary [`EditProposal`] through the same
//! path as the other timeline edits: one revision step, one Folio validation, one
//! [`crate::TimelineInverse`]. There is no new primitive: a merged or synchronized unit is clips
//! that share an ordinary `link_group`. Pulse mints no ids and commits nothing.
//!
//! The three relationships stay separate (STU-VID-029):
//! - link: `StudioClip.link_group`. [`plan_unlink`] dissolves the whole group and writes nothing
//!   else. [`plan_timeline_edit_with_linked_selection`] with [`LinkedSelection::Suspended`] is the
//!   planner side of the linked-selection master toggle: the edit acts on the named clip alone and
//!   every group id stays (a razor's right half is unlinked: a group holds one clip per track).
//!   The toggle's state and "selecting one selects all" are selection state and belong to the host.
//! - group: [`plan_move_clips`] moves an arbitrary caller-supplied set by one delta and shifts
//!   nothing else, whatever the sync locks say. A stored group identity needs a Folio record field
//!   this lane does not own; the set comes from the caller.
//! - sync lock: `StudioTrack.sync_locked`, read only by the shifting edits in
//!   [`crate::plan_timeline_edit`] and [`crate::plan_assembly_edit`]; nothing here reads or writes it.
//!
//! Merge and synchronize share one reading (declared Studio reading, STU-VID-029a gives the bases and
//! no arithmetic): each clip has a reference position on the timeline, the anchor stays, every other
//! clip moves so its reference lands on the anchor's. [`SyncPoint::InPoint`] is the clip's timeline
//! start, [`SyncPoint::OutPoint`] its timeline end, [`SyncPoint::Marker`] the position of the clip
//! marker of that name inside the clip's playing window (through the clip's speed, as
//! `marker_span`; a position between ticks rounds down), and [`SyncPoint::Timecode`] aligns the
//! source timecode of the clips through the [`SourceTimecodes`] port: equal labels land on one tick.
//! Timecode refuses a changed speed (two rates have no single alignment), a different rate or
//! drop-frame mode (the labels are not comparable) and a clip sharing no timecode span with the
//! anchor (nonmatching timecode). Merge's anchor is its one video clip, so the picture keeps its
//! frame grid; synchronize names its anchor. A clip moves within its own track, later clips do not
//! ripple and sync-locked tracks do not shift. [`SyncPoint::AudioWaveform`] is refused
//! `sync_waveform_unavailable`: no correlator exists in a crate pulse may depend on.
//!
//! Hostile input (V4-01/02): the selection and the group id are budgeted before any work; every tick
//! sum is checked; a missing, hidden or ambiguous marker and a missing timecode are per-clip refusals
//! ([`MergeSyncError::clip_id`]), with no fallback position. Premiere parity (merge and synchronize
//! dialogs, the sync reference, cross-rate timecode) is UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]

use crate::annotate::source_to_timeline;
use crate::error::{PulseError, Result};
use crate::range::{TickDelta, TickRange, offset, validate_lane};
use crate::time::{Timecode, timecode_to_ticks};
use crate::timeline_edit::{
    EditContext, EditProposal, Footprint, TimelineEdit, apply_edit, grid_of, lane_of, propose, rate_of,
};
use hsk_studio_accord::FrameRate;
use hsk_studio_folio::{
    ClipSource, MAX_CLIPS_PER_SEQUENCE, MAX_TRACKS_PER_SEQUENCE, Snapshot, Speed, StudioClip,
    StudioSequence, StudioTrack, TrackKind,
};
use hsk_studio_observe::FailureCode;
use std::collections::{BTreeMap, BTreeSet};

/// Largest selection for merge and synchronize: a link group holds one clip per track.
pub const MAX_LINKED_SELECTION: usize = MAX_TRACKS_PER_SEQUENCE;
/// Largest set for [`plan_move_clips`].
pub const MAX_MOVE_SELECTION: usize = MAX_CLIPS_PER_SEQUENCE;
/// Longest link-group id (Folio's key limit); longer is refused before it is copied per clip.
pub const MAX_LINK_GROUP_BYTES: usize = 128;

/// The planner side of the linked-selection master toggle (STU-VID-029). The toggle's state is the
/// host's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LinkedSelection {
    /// An edit of one link-group member acts on the whole group.
    #[default]
    On,
    /// The edit acts on the named clip alone; every group stays as it is.
    Suspended,
}

/// An arbitrary set moved together (STU-VID-029 "Group"): every clip moves by `delta` on its track.
/// With [`LinkedSelection::On`] a member's link partners move with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveClips {
    pub clip_ids: Vec<String>,
    pub delta: TickDelta,
    pub linked: LinkedSelection,
}

/// What the clips align by (STU-VID-029a).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncPoint {
    /// The clip's timeline start.
    InPoint,
    /// The clip's timeline end.
    OutPoint,
    /// The source timecode, from the [`SourceTimecodes`] port.
    Timecode,
    /// The clip marker with this name, inside the clip's playing window.
    Marker { name: String },
    /// Refused `sync_waveform_unavailable`.
    AudioWaveform,
}

/// The timecode label of a source's first tick and the rate that label counts in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceTimecode {
    pub start: Timecode,
    pub rate: FrameRate,
}

/// Typed port: the start timecode of a clip source (Folio stores none; the host reads it from the
/// media, for example reel's `tmcd` fields). `None` = the source has none, a per-clip refusal.
pub trait SourceTimecodes {
    fn timecode(&self, source: &ClipSource) -> Option<SourceTimecode>;
}

/// No source has a timecode: for callers that do not align by timecode.
pub struct NoSourceTimecodes;
impl SourceTimecodes for NoSourceTimecodes {
    fn timecode(&self, _source: &ClipSource) -> Option<SourceTimecode> {
        None
    }
}

/// `merge clips`: one video clip and one or more audio clips become one linked unit, where they
/// stand (`align: None`) or aligned to the video clip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeClips {
    pub clip_ids: Vec<String>,
    /// A group id not yet in use in the sequence.
    pub link_group: String,
    pub align: Option<SyncPoint>,
}

/// `synchronize`: the clips, one per track, align to `anchor_clip_id` and become one linked unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Synchronize {
    pub clip_ids: Vec<String>,
    /// One of `clip_ids`; it stays where it is.
    pub anchor_clip_id: String,
    pub point: SyncPoint,
    /// A group id not yet in use in the sequence.
    pub link_group: String,
}

/// How far one clip moved on the timeline; source windows do not change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipAlignment {
    pub clip_id: String,
    pub offset: TickDelta,
}

/// The one proposal of a merge or synchronize plus its provenance: the basis it aligned by (`None`:
/// linked in place) and the offset of every selected clip, in request order.
#[derive(Clone, Debug)]
pub struct MergeSyncProposal {
    proposal: EditProposal,
    link_group: String,
    basis: Option<SyncPoint>,
    alignment: Vec<ClipAlignment>,
}
impl MergeSyncProposal {
    /// The ordinary proposal; its `inverse()` goes to [`crate::plan_timeline_revert`].
    pub fn proposal(&self) -> &EditProposal {
        &self.proposal
    }
    pub fn link_group(&self) -> &str {
        &self.link_group
    }
    pub fn basis(&self) -> Option<&SyncPoint> {
        self.basis.as_ref()
    }
    pub fn alignment(&self) -> &[ClipAlignment] {
        &self.alignment
    }
}

/// Typed refusal: pulse's own vocabulary plus the link codes. The code carries no id; the clip a
/// refusal is about is the separate typed field, for the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergeSyncError {
    Pulse(PulseError),
    /// The clip is already in a link group.
    ClipAlreadyLinked { clip_id: String },
    /// The clip shares a track with an earlier selected clip.
    LinkSameTrack { clip_id: String },
    /// A merge needs exactly one video clip and at least one audio clip, and nothing else.
    MergeNoAvPair,
    /// A synchronize needs at least two clips.
    SyncTooFewClips,
    /// No audio correlator in a crate pulse may depend on.
    SyncWaveformUnavailable,
    /// The clip's source has no timecode.
    SyncTimecodeMissing { clip_id: String },
    /// The clip's timecode counts in another rate or drop-frame mode than the anchor's.
    SyncTimecodeRateMismatch { clip_id: String },
    /// The clip plays at a changed speed, which has no single timecode alignment.
    SyncTimecodeSpeed { clip_id: String },
    /// The clip shares no timecode span with the anchor.
    SyncTimecodeNoOverlap { clip_id: String },
    /// The clip has no marker of that name inside its playing window.
    SyncMarkerMissing { clip_id: String },
    /// The clip has more than one marker of that name.
    SyncMarkerAmbiguous { clip_id: String },
}
impl MergeSyncError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Pulse(e) => e.code(),
            Self::ClipAlreadyLinked { .. } => "clip_already_linked",
            Self::LinkSameTrack { .. } => "link_same_track",
            Self::MergeNoAvPair => "merge_no_av_pair",
            Self::SyncTooFewClips => "sync_too_few_clips",
            Self::SyncWaveformUnavailable => "sync_waveform_unavailable",
            Self::SyncTimecodeMissing { .. } => "sync_timecode_missing",
            Self::SyncTimecodeRateMismatch { .. } => "sync_timecode_rate_mismatch",
            Self::SyncTimecodeSpeed { .. } => "sync_timecode_speed",
            Self::SyncTimecodeNoOverlap { .. } => "sync_timecode_no_overlap",
            Self::SyncMarkerMissing { .. } => "sync_marker_missing",
            Self::SyncMarkerAmbiguous { .. } => "sync_marker_ambiguous",
        }
    }
    /// The clip this refusal is about, when it is about one.
    pub fn clip_id(&self) -> Option<&str> {
        match self {
            Self::ClipAlreadyLinked { clip_id }
            | Self::LinkSameTrack { clip_id }
            | Self::SyncTimecodeMissing { clip_id }
            | Self::SyncTimecodeRateMismatch { clip_id }
            | Self::SyncTimecodeSpeed { clip_id }
            | Self::SyncTimecodeNoOverlap { clip_id }
            | Self::SyncMarkerMissing { clip_id }
            | Self::SyncMarkerAmbiguous { clip_id } => Some(clip_id),
            Self::Pulse(_) | Self::MergeNoAvPair | Self::SyncTooFewClips | Self::SyncWaveformUnavailable => None,
        }
    }
    /// `None` means the terminal outcome is `Canceled`, as [`PulseError::failure_code`].
    pub fn failure_code(&self) -> Option<FailureCode> {
        match self {
            Self::Pulse(e) => e.failure_code(),
            Self::SyncWaveformUnavailable | Self::SyncTimecodeSpeed { .. } => Some(FailureCode::Unsupported),
            _ => Some(FailureCode::Validation),
        }
    }
}
impl From<PulseError> for MergeSyncError {
    fn from(e: PulseError) -> Self {
        Self::Pulse(e)
    }
}
impl From<hsk_studio_accord::ValidationError> for MergeSyncError {
    fn from(e: hsk_studio_accord::ValidationError) -> Self {
        Self::Pulse(e.into())
    }
}
impl std::fmt::Display for MergeSyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for MergeSyncError {}
pub type MergeSyncResult<T> = std::result::Result<T, MergeSyncError>;

/// [`crate::plan_timeline_edit`] under the linked-selection master toggle. `On` is that function;
/// `Suspended` plans the edit with the link relationships masked and puts them back by clip id, so
/// the edit acts on the named clip alone and no group dissolves. A new clip (a razor's right half, a
/// range cut's second piece) has no link; a razor passes `linked: None`.
pub fn plan_timeline_edit_with_linked_selection(
    snapshot: &Snapshot,
    sequence_id: &str,
    expected_revision: u64,
    edit: &TimelineEdit,
    linked: LinkedSelection,
    ctx: &EditContext,
) -> Result<EditProposal> {
    propose(snapshot, sequence_id, expected_revision, ctx, true, |old, reads| {
        if linked == LinkedSelection::On {
            return apply_edit(old, edit, ctx, reads);
        }
        let mut masked = old.clone();
        let mut groups = BTreeMap::new();
        for clip in masked.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()) {
            if let Some(group) = clip.link_group.take() {
                groups.insert(clip.clip_id.clone(), group);
            }
        }
        let mut out = apply_edit(&masked, edit, ctx, reads)?;
        for clip in out.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()) {
            if let Some(group) = groups.remove(&clip.clip_id) {
                clip.link_group = Some(group);
            }
        }
        Ok(out)
    })
}

/// Unlink (STU-VID-029): dissolves the clip's whole link group and writes only `link_group`. An
/// unlinked clip is `no_change`. A locked member refuses. A group that breaks the one-clip-per-track
/// reading can be dissolved here; every other linked edit refuses it `linked_edit_unsupported`.
pub fn plan_unlink(
    snapshot: &Snapshot,
    sequence_id: &str,
    expected_revision: u64,
    clip_id: &str,
    ctx: &EditContext,
) -> Result<EditProposal> {
    propose(snapshot, sequence_id, expected_revision, ctx, false, |old, reads| {
        let group = old
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .find(|c| c.clip_id == clip_id)
            .ok_or(PulseError::UnknownClip)?
            .link_group
            .as_deref();
        let mut out = old.clone();
        let Some(group) = group else {
            return Ok(out);
        };
        for track in &mut out.tracks {
            ctx.cancel.check()?;
            for clip in &mut track.clips {
                if clip.link_group.as_deref() == Some(group) {
                    reads.add(&track.track_id);
                    reads.add(&clip.clip_id);
                    if track.locked {
                        return Err(PulseError::TrackLocked);
                    }
                    clip.link_group = None;
                }
            }
        }
        Ok(out)
    })
}

/// Orders a track whose clips moved and refuses a collision (`overlap`) or a tick overflow.
fn settle(track: &mut StudioTrack, ctx: &EditContext) -> Result<()> {
    track.clips.sort_by_key(|c| c.timeline_start);
    validate_lane(&lane_of(track)?, ctx.cancel)
}

fn move_clips(
    seq: &StudioSequence,
    request: &MoveClips,
    ctx: &EditContext,
    reads: &mut Footprint,
) -> Result<StudioSequence> {
    if request.clip_ids.is_empty() {
        return Err(PulseError::InvalidInput);
    }
    let mut wanted = BTreeSet::new();
    for id in &request.clip_ids {
        if !wanted.insert(id.as_str()) {
            return Err(PulseError::DuplicateId);
        }
    }
    let selected: Vec<&StudioClip> = seq
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .filter(|c| wanted.contains(c.clip_id.as_str()))
        .collect();
    if selected.len() != wanted.len() {
        return Err(PulseError::UnknownClip);
    }
    let groups: BTreeSet<&str> = if request.linked == LinkedSelection::On {
        selected.iter().filter_map(|c| c.link_group.as_deref()).collect()
    } else {
        BTreeSet::new()
    };
    let moves = |c: &StudioClip| {
        wanted.contains(c.clip_id.as_str()) || c.link_group.as_deref().is_some_and(|g| groups.contains(g))
    };
    let rate = rate_of(seq)?;
    let mut out = seq.clone();
    for (t, track) in seq.tracks.iter().enumerate() {
        ctx.cancel.check()?;
        if !track.clips.iter().any(moves) {
            continue;
        }
        reads.add(&track.track_id);
        if track.locked {
            return Err(PulseError::TrackLocked);
        }
        let grid = grid_of(track, rate);
        for clip in out.tracks[t].clips.iter_mut().filter(|c| moves(c)) {
            reads.add(&clip.clip_id);
            let start = offset(clip.timeline_start, request.delta)?;
            grid.check_position(start)?;
            clip.timeline_start = start;
        }
        settle(&mut out.tracks[t], ctx)?;
    }
    Ok(out)
}

/// Group move (STU-VID-029): every clip of the set moves by `delta` on its own track; nothing else
/// moves and no clip is trimmed, whatever the sync locks say. Refusals: `limit_exceeded` before any
/// work, `invalid_input` (empty), `duplicate_id`, `unknown_clip`, `track_locked`, `underflow`,
/// `overlap`, `not_frame_aligned` on a video track, `no_change` for a zero delta.
pub fn plan_move_clips(
    snapshot: &Snapshot,
    sequence_id: &str,
    expected_revision: u64,
    request: &MoveClips,
    ctx: &EditContext,
) -> Result<EditProposal> {
    if request.clip_ids.len() > MAX_MOVE_SELECTION {
        return Err(PulseError::LimitExceeded);
    }
    propose(snapshot, sequence_id, expected_revision, ctx, true, |old, reads| {
        move_clips(old, request, ctx, reads)
    })
}

#[derive(Clone, Copy)]
struct Member {
    track: usize,
    clip: usize,
}

struct Unit<'a> {
    clip_ids: &'a [String],
    link_group: &'a str,
    point: Option<&'a SyncPoint>,
    /// `None` = a merge, whose anchor is its one video clip.
    anchor: Option<&'a str>,
}

fn members_of(
    seq: &StudioSequence,
    clip_ids: &[String],
    reads: &mut Footprint,
) -> MergeSyncResult<Vec<Member>> {
    let mut wanted = BTreeSet::new();
    for id in clip_ids {
        if !wanted.insert(id.as_str()) {
            return Err(PulseError::DuplicateId.into());
        }
    }
    let mut found = BTreeMap::new();
    for (t, track) in seq.tracks.iter().enumerate() {
        for (c, clip) in track.clips.iter().enumerate() {
            if let Some(id) = wanted.get(clip.clip_id.as_str()) {
                found.insert(*id, Member { track: t, clip: c });
            }
        }
    }
    let mut members = Vec::with_capacity(clip_ids.len());
    let mut tracks = BTreeSet::new();
    for id in clip_ids {
        let member = *found.get(id.as_str()).ok_or(PulseError::UnknownClip)?;
        let track = &seq.tracks[member.track];
        reads.add(&track.track_id);
        reads.add(id);
        if track.clips[member.clip].link_group.is_some() {
            return Err(MergeSyncError::ClipAlreadyLinked { clip_id: id.clone() });
        }
        if !tracks.insert(member.track) {
            return Err(MergeSyncError::LinkSameTrack { clip_id: id.clone() });
        }
        if track.locked {
            return Err(PulseError::TrackLocked.into());
        }
        members.push(member);
    }
    Ok(members)
}

fn merge_anchor(seq: &StudioSequence, members: &[Member]) -> MergeSyncResult<Member> {
    let kind = |m: &Member| seq.tracks[m.track].kind;
    let mut video = members.iter().filter(|m| kind(m) == TrackKind::Video);
    let (first, second) = (video.next(), video.next());
    let has_audio = members.iter().any(|m| kind(m) == TrackKind::Audio);
    let only_av = members.iter().all(|m| matches!(kind(m), TrackKind::Video | TrackKind::Audio));
    match (first, second) {
        (Some(v), None) if has_audio && only_av => Ok(*v),
        _ => Err(MergeSyncError::MergeNoAvPair),
    }
}

/// What a clip's source timecode says about the clip: the label's rate and mode, and the span its
/// playing window covers in timecode ticks.
struct TimecodeSpan {
    format: (FrameRate, bool),
    span: TickRange,
}

fn timecode_span(clip: &StudioClip, timecodes: &dyn SourceTimecodes) -> MergeSyncResult<TimecodeSpan> {
    let clip_id = || clip.clip_id.clone();
    let tc = timecodes
        .timecode(&clip.source_ref)
        .ok_or_else(|| MergeSyncError::SyncTimecodeMissing { clip_id: clip_id() })?;
    if clip.speed != Speed::UNIT {
        return Err(MergeSyncError::SyncTimecodeSpeed { clip_id: clip_id() });
    }
    let start = timecode_to_ticks(tc.start, tc.rate, tc.start.drop_frame)?;
    let span = TickRange::from_start_duration(start.checked_add(clip.source_in)?, clip.duration)?;
    Ok(TimecodeSpan { format: (tc.rate, tc.start.drop_frame), span })
}

fn marker_position(clip: &StudioClip, name: &str) -> MergeSyncResult<i128> {
    let missing = || MergeSyncError::SyncMarkerMissing { clip_id: clip.clip_id.clone() };
    let mut named = clip.markers.iter().filter(|m| m.name == name);
    let marker = named.next().ok_or_else(missing)?;
    if named.next().is_some() {
        return Err(MergeSyncError::SyncMarkerAmbiguous { clip_id: clip.clip_id.clone() });
    }
    let at = source_to_timeline(clip, marker.start, marker.start)?.ok_or_else(missing)?;
    Ok(i128::from(at.start().value()))
}

/// The clip's reference position on the timeline. For timecode it is where timecode zero would lie:
/// a difference of two of these is the move that puts equal labels on one tick.
fn reference(clip: &StudioClip, point: &SyncPoint, tc: Option<&TimecodeSpan>) -> MergeSyncResult<i128> {
    let start = i128::from(clip.timeline_start.value());
    match (point, tc) {
        (SyncPoint::InPoint, _) => Ok(start),
        (SyncPoint::OutPoint, _) => {
            Ok(i128::from(clip.timeline_end().ok_or(PulseError::Overflow)?.value()))
        }
        (SyncPoint::Marker { name }, _) => marker_position(clip, name),
        (SyncPoint::Timecode, Some(tc)) => start
            .checked_sub(i128::from(tc.span.start().value()))
            .ok_or_else(|| PulseError::Overflow.into()),
        (SyncPoint::Timecode, None) => Err(PulseError::InvalidInput.into()),
        (SyncPoint::AudioWaveform, _) => Err(MergeSyncError::SyncWaveformUnavailable),
    }
}

fn alignment_offsets(
    seq: &StudioSequence,
    members: &[Member],
    anchor: Member,
    point: &SyncPoint,
    timecodes: &dyn SourceTimecodes,
    ctx: &EditContext,
) -> MergeSyncResult<Vec<TickDelta>> {
    let clip_at = |m: Member| &seq.tracks[m.track].clips[m.clip];
    let by_timecode = matches!(point, SyncPoint::Timecode);
    let anchor_tc = if by_timecode { Some(timecode_span(clip_at(anchor), timecodes)?) } else { None };
    let anchor_at = reference(clip_at(anchor), point, anchor_tc.as_ref())?;
    let mut offsets = Vec::with_capacity(members.len());
    for &member in members {
        ctx.cancel.check()?;
        let clip = clip_at(member);
        let tc = if by_timecode { Some(timecode_span(clip, timecodes)?) } else { None };
        if let (Some(tc), Some(anchor_tc)) = (&tc, &anchor_tc) {
            if tc.format != anchor_tc.format {
                return Err(MergeSyncError::SyncTimecodeRateMismatch { clip_id: clip.clip_id.clone() });
            }
            if !tc.span.overlaps(anchor_tc.span) {
                return Err(MergeSyncError::SyncTimecodeNoOverlap { clip_id: clip.clip_id.clone() });
            }
        }
        let at = reference(clip, point, tc.as_ref())?;
        offsets.push(TickDelta::new(anchor_at.checked_sub(at).ok_or(PulseError::Overflow)?)?);
    }
    Ok(offsets)
}

fn link_unit(
    seq: &StudioSequence,
    unit: &Unit<'_>,
    timecodes: &dyn SourceTimecodes,
    ctx: &EditContext,
    reads: &mut Footprint,
    alignment: &mut Vec<ClipAlignment>,
) -> MergeSyncResult<StudioSequence> {
    let members = members_of(seq, unit.clip_ids, reads)?;
    let group_in_use = seq
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .any(|c| c.link_group.as_deref() == Some(unit.link_group));
    if group_in_use {
        return Err(PulseError::DuplicateId.into());
    }
    let anchor = match unit.anchor {
        None => merge_anchor(seq, &members)?,
        Some(id) => {
            if members.len() < 2 {
                return Err(MergeSyncError::SyncTooFewClips);
            }
            *members
                .iter()
                .find(|m| seq.tracks[m.track].clips[m.clip].clip_id == id)
                .ok_or(PulseError::InvalidInput)?
        }
    };
    let offsets = match unit.point {
        None => vec![TickDelta::ZERO; members.len()],
        Some(point) => alignment_offsets(seq, &members, anchor, point, timecodes, ctx)?,
    };
    let rate = rate_of(seq)?;
    let mut out = seq.clone();
    for (member, delta) in members.iter().zip(offsets) {
        ctx.cancel.check()?;
        let track = &seq.tracks[member.track];
        let clip = &track.clips[member.clip];
        alignment.push(ClipAlignment { clip_id: clip.clip_id.clone(), offset: delta });
        let written = &mut out.tracks[member.track];
        written.clips[member.clip].link_group = Some(unit.link_group.to_owned());
        if delta != TickDelta::ZERO {
            let start = offset(clip.timeline_start, delta)?;
            grid_of(track, rate).check_position(start)?;
            written.clips[member.clip].timeline_start = start;
            settle(written, ctx)?;
        }
    }
    Ok(out)
}

/// One step around [`propose`] keeping the per-clip refusals typed. Only an aligning unit can end a
/// transition; a link written in place leaves the transitions alone.
fn plan_unit(
    snapshot: &Snapshot,
    sequence_id: &str,
    expected_revision: u64,
    unit: &Unit<'_>,
    timecodes: &dyn SourceTimecodes,
    ctx: &EditContext,
) -> MergeSyncResult<MergeSyncProposal> {
    if unit.clip_ids.len() > MAX_LINKED_SELECTION || unit.link_group.len() > MAX_LINK_GROUP_BYTES {
        return Err(PulseError::LimitExceeded.into());
    }
    if matches!(unit.point, Some(SyncPoint::AudioWaveform)) {
        return Err(MergeSyncError::SyncWaveformUnavailable);
    }
    let mut refusal = None;
    let mut alignment = Vec::new();
    let carry = unit.point.is_some();
    let result = propose(snapshot, sequence_id, expected_revision, ctx, carry, |old, reads| {
        link_unit(old, unit, timecodes, ctx, reads, &mut alignment).map_err(|e| {
            let code = match &e {
                MergeSyncError::Pulse(p) => *p,
                _ => PulseError::InvalidInput,
            };
            refusal = Some(e);
            code
        })
    });
    match (result, refusal) {
        (Ok(proposal), _) => Ok(MergeSyncProposal {
            proposal,
            link_group: unit.link_group.to_owned(),
            basis: unit.point.cloned(),
            alignment,
        }),
        (Err(_), Some(r)) => Err(r),
        (Err(e), None) => Err(e.into()),
    }
}

/// `merge clips` (STU-VID-029a): one video clip and its separately recorded audio clips become one
/// ordinary link group, in place or aligned to the video clip. Refusals: module docs and
/// [`MergeSyncError`].
pub fn plan_merge_clips(
    snapshot: &Snapshot,
    sequence_id: &str,
    expected_revision: u64,
    request: &MergeClips,
    timecodes: &dyn SourceTimecodes,
    ctx: &EditContext,
) -> MergeSyncResult<MergeSyncProposal> {
    let unit = Unit {
        clip_ids: &request.clip_ids,
        link_group: &request.link_group,
        point: request.align.as_ref(),
        anchor: None,
    };
    plan_unit(snapshot, sequence_id, expected_revision, &unit, timecodes, ctx)
}

/// `synchronize` (STU-VID-029a): the clips, one per track, align to the anchor by in point, out
/// point, timecode or marker and become one ordinary link group. Audio waveform is refused
/// `sync_waveform_unavailable`.
pub fn plan_synchronize(
    snapshot: &Snapshot,
    sequence_id: &str,
    expected_revision: u64,
    request: &Synchronize,
    timecodes: &dyn SourceTimecodes,
    ctx: &EditContext,
) -> MergeSyncResult<MergeSyncProposal> {
    let unit = Unit {
        clip_ids: &request.clip_ids,
        link_group: &request.link_group,
        point: Some(&request.point),
        anchor: Some(&request.anchor_clip_id),
    };
    plan_unit(snapshot, sequence_id, expected_revision, &unit, timecodes, ctx)
}
