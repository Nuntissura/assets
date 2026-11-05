//! RED STUB of lane SMB-PR-RETIME-HOLD-1: the types and signatures only, every body refuses.
#![deny(clippy::arithmetic_side_effects)]

use crate::error::PulseError;
use crate::range::TickRange;
use crate::timeline_edit::{EditContext, EditProposal, SourceExtents};
use hsk_studio_accord::{FrameRate, Ticks};
pub use hsk_studio_folio::{ClipFrameHold, ClipRetime, FrameHoldAnchor, FrameInterpolation};
use hsk_studio_folio::{Snapshot, StudioClip, StudioMulticamRecords};

/// The mark any diagnostic carries when it compares an `optical_flow` frame to a source frame.
pub const ESTIMATION_MARK: &str = "estimation";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceFrameGrid {
    ticks_per_frame: u64,
}
impl SourceFrameGrid {
    pub fn new(ticks_per_frame: u64) -> crate::Result<Self> {
        Ok(Self { ticks_per_frame })
    }
    pub fn from_rate(rate: FrameRate) -> Self {
        Self { ticks_per_frame: rate.ticks_per_frame() }
    }
    pub fn ticks_per_frame(self) -> u64 {
        self.ticks_per_frame
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceFrameRef {
    pub index: u64,
    pub start: Ticks,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameWeight {
    numerator: u128,
    denominator: u128,
}
impl FrameWeight {
    pub const ZERO: FrameWeight = FrameWeight { numerator: 0, denominator: 1 };
    pub fn numerator(self) -> u128 {
        self.numerator
    }
    pub fn denominator(self) -> u128 {
        self.denominator
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceFrameAnswer {
    Sampled(SourceFrameRef),
    Blended { from: SourceFrameRef, to: SourceFrameRef, weight: FrameWeight },
    Estimated { from: SourceFrameRef, to: SourceFrameRef, weight: FrameWeight },
    Held(SourceFrameRef),
}
impl SourceFrameAnswer {
    pub fn is_estimation(&self) -> bool {
        false
    }
    pub fn estimation_mark(&self) -> Option<&'static str> {
        None
    }
}

pub fn resolve_source_frames(_clip: &StudioClip, _tick: Ticks, _grid: SourceFrameGrid, _extents: &dyn SourceExtents) -> crate::Result<SourceFrameAnswer> {
    Err(PulseError::InvalidInput)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HoldAt {
    Playhead(Ticks),
    ClipMarker(String),
    InPoint,
    OutPoint,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetFrameHold {
    pub clip_id: String,
    pub at: HoldAt,
    pub grid: SourceFrameGrid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InsertFrameHoldSegment {
    pub clip_id: String,
    pub span: TickRange,
    pub at: HoldAt,
    pub grid: SourceFrameGrid,
    pub fresh_clip_ids: Vec<String>,
    pub fresh_link_groups: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetimeOptions {
    pub clip_id: String,
    pub frame_interpolation: Option<FrameInterpolation>,
    pub maintain_audio_pitch: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetimeError {
    Pulse(PulseError),
    HoldOutsideSourceWindow,
    HoldNotVideo,
    HoldMulticam,
}
impl RetimeError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Pulse(e) => e.code(),
            Self::HoldOutsideSourceWindow => "frame_hold_outside_source_window",
            Self::HoldNotVideo => "frame_hold_not_video",
            Self::HoldMulticam => "frame_hold_multicam",
        }
    }
}
impl From<PulseError> for RetimeError {
    fn from(e: PulseError) -> Self {
        Self::Pulse(e)
    }
}
impl std::fmt::Display for RetimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for RetimeError {}

pub type RetimeResult<T> = std::result::Result<T, RetimeError>;

pub fn plan_set_frame_hold(_snapshot: &Snapshot, _sequence_id: &str, _expected_revision: u64, _edit: &SetFrameHold, _multicam: Option<&StudioMulticamRecords>, _ctx: &EditContext) -> RetimeResult<EditProposal> {
    Err(PulseError::InvalidInput.into())
}

pub fn plan_clear_frame_hold(_snapshot: &Snapshot, _sequence_id: &str, _expected_revision: u64, _clip_id: &str, _ctx: &EditContext) -> RetimeResult<EditProposal> {
    Err(PulseError::InvalidInput.into())
}

pub fn plan_insert_frame_hold_segment(_snapshot: &Snapshot, _sequence_id: &str, _expected_revision: u64, _edit: &InsertFrameHoldSegment, _multicam: Option<&StudioMulticamRecords>, _ctx: &EditContext) -> RetimeResult<EditProposal> {
    Err(PulseError::InvalidInput.into())
}

pub fn plan_set_retime_options(_snapshot: &Snapshot, _sequence_id: &str, _expected_revision: u64, _edit: &RetimeOptions, _ctx: &EditContext) -> RetimeResult<EditProposal> {
    Err(PulseError::InvalidInput.into())
}
