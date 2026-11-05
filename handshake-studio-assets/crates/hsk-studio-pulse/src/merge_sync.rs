//! Red slice of lane SMB-PR-MERGE-SYNC-1: the public shape only, every planner refuses.
#![deny(clippy::arithmetic_side_effects)]

use crate::error::{PulseError, Result};
use crate::range::TickDelta;
use crate::time::Timecode;
use crate::timeline_edit::{EditContext, EditProposal, TimelineEdit};
use hsk_studio_accord::FrameRate;
use hsk_studio_folio::{ClipSource, MAX_CLIPS_PER_SEQUENCE, MAX_TRACKS_PER_SEQUENCE, Snapshot};
use hsk_studio_observe::FailureCode;

pub const MAX_LINKED_SELECTION: usize = MAX_TRACKS_PER_SEQUENCE;
pub const MAX_MOVE_SELECTION: usize = MAX_CLIPS_PER_SEQUENCE;
pub const MAX_LINK_GROUP_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LinkedSelection {
    #[default]
    On,
    Suspended,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveClips {
    pub clip_ids: Vec<String>,
    pub delta: TickDelta,
    pub linked: LinkedSelection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncPoint {
    InPoint,
    OutPoint,
    Timecode,
    Marker { name: String },
    AudioWaveform,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceTimecode {
    pub start: Timecode,
    pub rate: FrameRate,
}

pub trait SourceTimecodes {
    fn timecode(&self, source: &ClipSource) -> Option<SourceTimecode>;
}

pub struct NoSourceTimecodes;
impl SourceTimecodes for NoSourceTimecodes {
    fn timecode(&self, _source: &ClipSource) -> Option<SourceTimecode> {
        None
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeClips {
    pub clip_ids: Vec<String>,
    pub link_group: String,
    pub align: Option<SyncPoint>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Synchronize {
    pub clip_ids: Vec<String>,
    pub anchor_clip_id: String,
    pub point: SyncPoint,
    pub link_group: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipAlignment {
    pub clip_id: String,
    pub offset: TickDelta,
}

#[derive(Clone, Debug)]
pub struct MergeSyncProposal {
    proposal: EditProposal,
    link_group: String,
    basis: Option<SyncPoint>,
    alignment: Vec<ClipAlignment>,
}
impl MergeSyncProposal {
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergeSyncError {
    Pulse(PulseError),
}
impl MergeSyncError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Pulse(e) => e.code(),
        }
    }
    pub fn clip_id(&self) -> Option<&str> {
        None
    }
    pub fn failure_code(&self) -> Option<FailureCode> {
        match self {
            Self::Pulse(e) => e.failure_code(),
        }
    }
}
impl std::fmt::Display for MergeSyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for MergeSyncError {}
pub type MergeSyncResult<T> = std::result::Result<T, MergeSyncError>;

pub fn plan_timeline_edit_with_linked_selection(
    _snapshot: &Snapshot,
    _sequence_id: &str,
    _expected_revision: u64,
    _edit: &TimelineEdit,
    _linked: LinkedSelection,
    _ctx: &EditContext,
) -> Result<EditProposal> {
    Err(PulseError::InvalidInput)
}

pub fn plan_unlink(
    _snapshot: &Snapshot,
    _sequence_id: &str,
    _expected_revision: u64,
    _clip_id: &str,
    _ctx: &EditContext,
) -> Result<EditProposal> {
    Err(PulseError::InvalidInput)
}

pub fn plan_move_clips(
    _snapshot: &Snapshot,
    _sequence_id: &str,
    _expected_revision: u64,
    _request: &MoveClips,
    _ctx: &EditContext,
) -> Result<EditProposal> {
    Err(PulseError::InvalidInput)
}

pub fn plan_merge_clips(
    _snapshot: &Snapshot,
    _sequence_id: &str,
    _expected_revision: u64,
    _request: &MergeClips,
    _timecodes: &dyn SourceTimecodes,
    _ctx: &EditContext,
) -> MergeSyncResult<MergeSyncProposal> {
    Err(MergeSyncError::Pulse(PulseError::InvalidInput))
}

pub fn plan_synchronize(
    _snapshot: &Snapshot,
    _sequence_id: &str,
    _expected_revision: u64,
    _request: &Synchronize,
    _timecodes: &dyn SourceTimecodes,
    _ctx: &EditContext,
) -> MergeSyncResult<MergeSyncProposal> {
    Err(MergeSyncError::Pulse(PulseError::InvalidInput))
}
