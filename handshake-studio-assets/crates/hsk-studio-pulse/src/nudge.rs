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
use crate::merge_sync::LinkedSelection;
use crate::timeline_edit::{EditContext, EditProposal};
use hsk_studio_folio::Snapshot;
use hsk_studio_observe::FailureCode;

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
impl std::fmt::Display for NudgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for NudgeError {}
pub type NudgeResult<T> = std::result::Result<T, NudgeError>;

/// Red commit: refuses everything; the planner comes with the next commit of this lane.
pub fn plan_nudge_track(_: &Snapshot, _: &str, _: u64, _: &NudgeTrack, _: &EditContext) -> NudgeResult<EditProposal> {
    Err(PulseError::InvalidInput.into())
}
