//! Up and down by one track (STU-VID-035a), lane SMB-PR-NUDGE-1. [`plan_nudge_track`] returns one
//! ordinary [`EditProposal`] through the same path as the other timeline edits: one revision step,
//! one Folio validation, one [`crate::TimelineInverse`]. Pulse mints no ids and commits nothing.
//!
//! A clip keeps its time and moves whole to the next track of its own kind, so no field of it is
//! set or copied one by one: retime, speed, markers, `enabled`, the link group and the window arrive
//! as they left (test `a_track_nudge_changes_only_the_track_of_a_clip`). The diff that reports the
//! move destructures the clip with no `..` (`ClipChange::TrackMove`).
//!
//! Studio readings, vendor behaviour UNVERIFIED (D5):
//! - Up and down are the rows the Viewer draws: on caption and video tracks up is the next higher
//!   index, on audio and submix tracks up is the next lower index (harness-view `track_order`;
//!   test `up_lands_on_the_row_above_in_the_drawn_order` in its `viewer_timeline_nudge`).
//! - An occupied range refuses [`NudgeError::RangeOccupied`], naming both clips. A vertical nudge
//!   never overwrites, trims or shifts anything, whatever the sync locks say. Touching is free. A
//!   range another clip of the same request is leaving counts as free: the whole selection moves as
//!   one step, and any refusal leaves nothing.
//! - Link partners follow merge-sync's [`LinkedSelection`]. `On`: the whole group moves. A group
//!   takes one index step, the one the first named clip of the group asks for, and each partner
//!   takes that step in its own kind's stack, so V1 + A1 go to V2 + A2 and a pair stays a pair (the
//!   audio row goes down when the video row goes up). A partner with no track that way, on a locked
//!   track or facing an occupied range refuses the whole edit, naming the partner. `Suspended`: the
//!   named clip moves alone and keeps its group id; landing on a track that holds a partner would
//!   put two members on one track and refuses `linked_edit_unsupported`, as does any member landing
//!   beside another member of its group.
//! - A transition stays when it still plays where the clip lands. One that no longer plays is
//!   removed and listed in [`EditProposal::dropped_transitions`], as for every clip edit.
//!
//! Hostile input (V4-01/02): the selection is budgeted before any work ([`crate::MAX_MOVE_SELECTION`]);
//! destination tracks are found once per track, so the work is linear in the Folio-bounded document.
#![deny(clippy::arithmetic_side_effects)]

use crate::error::PulseError;
use crate::merge_sync::{LinkedSelection, MAX_MOVE_SELECTION};
use crate::range::validate_lane;
use crate::timeline_edit::{EditContext, EditProposal, Footprint, lane_of, propose};
use hsk_studio_folio::{Snapshot, StudioSequence, TrackKind};
use hsk_studio_observe::FailureCode;
use std::collections::{BTreeMap, BTreeSet};

/// Which way a clip goes, in the rows the timeline draws (module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackNudge {
    Up,
    Down,
}

/// `cmd.timeline.nudge.up` / `cmd.timeline.nudge.down`: every named clip goes one track that way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NudgeTrack {
    pub clip_ids: Vec<String>,
    pub direction: TrackNudge,
    pub linked: LinkedSelection,
}

/// Typed refusal: pulse's own vocabulary plus the three a track nudge adds. A code carries no id; the
/// clips and the track a refusal is about are separate typed fields, for the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NudgeError {
    Pulse(PulseError),
    /// The clip's kind has no further track that way.
    NoTrackThatWay { clip_id: String },
    /// The track the clip would leave or land on is locked.
    TrackLocked { clip_id: String, track_id: String },
    /// `occupied_by` already plays in the range the clip would land on.
    RangeOccupied { clip_id: String, occupied_by: String },
}
impl NudgeError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Pulse(e) => e.code(),
            Self::NoTrackThatWay { .. } => "nudge_no_track",
            Self::TrackLocked { .. } => "track_locked",
            Self::RangeOccupied { .. } => "nudge_range_occupied",
        }
    }
    /// The clip this refusal is about, when it is about one.
    pub fn clip_id(&self) -> Option<&str> {
        match self {
            Self::NoTrackThatWay { clip_id } | Self::TrackLocked { clip_id, .. } | Self::RangeOccupied { clip_id, .. } => Some(clip_id),
            Self::Pulse(_) => None,
        }
    }
    /// `None` means the terminal outcome is `Canceled`, as [`PulseError::failure_code`].
    pub fn failure_code(&self) -> Option<FailureCode> {
        match self {
            Self::Pulse(e) => e.failure_code(),
            _ => Some(FailureCode::Validation),
        }
    }
}
impl From<PulseError> for NudgeError {
    fn from(e: PulseError) -> Self {
        Self::Pulse(e)
    }
}
impl From<hsk_studio_accord::ValidationError> for NudgeError {
    fn from(e: hsk_studio_accord::ValidationError) -> Self {
        Self::Pulse(e.into())
    }
}
impl std::fmt::Display for NudgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for NudgeError {}
pub type NudgeResult<T> = std::result::Result<T, NudgeError>;

/// One place in a kind's stack, by index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Higher,
    Lower,
}

fn step_of(kind: TrackKind, direction: TrackNudge) -> Step {
    let drawn_by_descending_index = match kind {
        TrackKind::Caption | TrackKind::Video => true,
        TrackKind::Audio | TrackKind::Submix => false,
    };
    if drawn_by_descending_index == (direction == TrackNudge::Up) { Step::Higher } else { Step::Lower }
}

/// For each track, the next track of its kind with a lower and with a higher index. Folio holds a
/// kind's tracks in index order.
fn stack_neighbours(seq: &StudioSequence) -> Vec<(Option<usize>, Option<usize>)> {
    let mut neighbours: Vec<(Option<usize>, Option<usize>)> = vec![(None, None); seq.tracks.len()];
    let mut last: [Option<usize>; 4] = [None; 4];
    for (t, track) in seq.tracks.iter().enumerate() {
        let slot = match track.kind {
            TrackKind::Video => 0,
            TrackKind::Audio => 1,
            TrackKind::Caption => 2,
            TrackKind::Submix => 3,
        };
        if let Some(previous) = last[slot] {
            neighbours[previous].1 = Some(t);
            neighbours[t].0 = Some(previous);
        }
        last[slot] = Some(t);
    }
    neighbours
}

/// The step each moving clip takes, as the module docs read link partners.
fn steps<'a>(seq: &'a StudioSequence, request: &'a NudgeTrack, ctx: &EditContext) -> NudgeResult<(BTreeSet<&'a str>, BTreeMap<&'a str, Step>)> {
    if request.clip_ids.is_empty() {
        return Err(PulseError::InvalidInput.into());
    }
    let mut wanted = BTreeSet::new();
    for id in &request.clip_ids {
        if !wanted.insert(id.as_str()) {
            return Err(PulseError::DuplicateId.into());
        }
    }
    let mut named: BTreeMap<&str, (TrackKind, Option<&str>)> = BTreeMap::new();
    for track in &seq.tracks {
        ctx.cancel.check()?;
        for clip in track.clips.iter().filter(|c| wanted.contains(c.clip_id.as_str())) {
            named.insert(clip.clip_id.as_str(), (track.kind, clip.link_group.as_deref()));
        }
    }
    let mut groups = BTreeMap::new();
    for id in &request.clip_ids {
        let (kind, group) = *named.get(id.as_str()).ok_or(PulseError::UnknownClip)?;
        if let (LinkedSelection::On, Some(group)) = (request.linked, group) {
            groups.entry(group).or_insert_with(|| step_of(kind, request.direction));
        }
    }
    Ok((wanted, groups))
}

fn nudge_tracks(seq: &StudioSequence, request: &NudgeTrack, ctx: &EditContext, reads: &mut Footprint) -> NudgeResult<StudioSequence> {
    let (wanted, groups) = steps(seq, request, ctx)?;
    let neighbours = stack_neighbours(seq);
    let mut destination: BTreeMap<&str, usize> = BTreeMap::new();
    let mut moving_groups = BTreeSet::new();
    for (t, track) in seq.tracks.iter().enumerate() {
        ctx.cancel.check()?;
        for clip in &track.clips {
            let group = clip.link_group.as_deref();
            let step = match group.and_then(|g| groups.get(g)) {
                Some(step) => *step,
                None if wanted.contains(clip.clip_id.as_str()) => step_of(track.kind, request.direction),
                None => continue,
            };
            reads.add(&track.track_id);
            reads.add(&clip.clip_id);
            let next = if step == Step::Higher { neighbours[t].1 } else { neighbours[t].0 };
            let d = next.ok_or_else(|| NudgeError::NoTrackThatWay { clip_id: clip.clip_id.clone() })?;
            if let Some(locked) = [track, &seq.tracks[d]].into_iter().find(|x| x.locked) {
                return Err(NudgeError::TrackLocked { clip_id: clip.clip_id.clone(), track_id: locked.track_id.clone() });
            }
            destination.insert(clip.clip_id.as_str(), d);
            moving_groups.extend(group);
        }
    }
    let mut out = seq.clone();
    let mut going = Vec::with_capacity(destination.len());
    for track in &mut out.tracks {
        let (leaving, staying) = std::mem::take(&mut track.clips).into_iter().partition::<Vec<_>, _>(|c| destination.contains_key(c.clip_id.as_str()));
        track.clips = staying;
        going.extend(leaving);
    }
    let mut landing = BTreeSet::new();
    for clip in going {
        let d = *destination.get(clip.clip_id.as_str()).ok_or(PulseError::UnknownClip)?;
        landing.insert(d);
        out.tracks[d].clips.push(clip);
    }
    for d in landing {
        ctx.cancel.check()?;
        let track = &mut out.tracks[d];
        track.clips.sort_by_key(|c| c.timeline_start);
        reads.add(&track.track_id);
        let mut members = BTreeSet::new();
        for clip in &track.clips {
            reads.add(&clip.clip_id);
            if let Some(group) = clip.link_group.as_deref().filter(|g| moving_groups.contains(g))
                && !members.insert(group)
            {
                return Err(PulseError::LinkedEditUnsupported.into());
            }
        }
        for pair in track.clips.windows(2) {
            let end = pair[0].timeline_end().ok_or(PulseError::Overflow)?;
            if end > pair[1].timeline_start {
                let (clip, other) = if destination.contains_key(pair[1].clip_id.as_str()) { (&pair[1], &pair[0]) } else { (&pair[0], &pair[1]) };
                return Err(NudgeError::RangeOccupied { clip_id: clip.clip_id.clone(), occupied_by: other.clip_id.clone() });
            }
        }
        validate_lane(&lane_of(track)?, ctx.cancel)?;
    }
    Ok(out)
}

/// Up or down by one track (STU-VID-035a): every named clip keeps its time and moves to the next track
/// of its kind; nothing else moves. Refusals, nothing produced: `limit_exceeded` before any work,
/// `invalid_input` (empty), `duplicate_id`, `unknown_clip`, [`NudgeError`], `linked_edit_unsupported`
/// (two members of a group on one track), `stale_revision`, `canceled`.
pub fn plan_nudge_track(snapshot: &Snapshot, sequence_id: &str, expected_revision: u64, request: &NudgeTrack, ctx: &EditContext) -> NudgeResult<EditProposal> {
    if request.clip_ids.len() > MAX_MOVE_SELECTION {
        return Err(PulseError::LimitExceeded.into());
    }
    let mut refusal = None;
    let result = propose(snapshot, sequence_id, expected_revision, ctx, true, |old, reads| {
        nudge_tracks(old, request, ctx, reads).map_err(|e| {
            let code = match &e {
                NudgeError::Pulse(p) => *p,
                _ => PulseError::InvalidInput,
            };
            refusal = Some(e);
            code
        })
    });
    match (result, refusal) {
        (Ok(proposal), _) => Ok(proposal),
        (Err(_), Some(r)) => Err(r),
        (Err(e), None) => Err(e.into()),
    }
}
