//! Tracker command surface (lane SMB-AE-KEY-TRACK-2): a typed session a model drives without the
//! on-canvas UI (STU-CMP-065a: "Tracking is a long-running, cancellable, observable analysis ...
//! fully drivable by typed command so a model can track a shot without the on-canvas UI. 18
//! tracker commands are recovered as the operation-surface target").
//!
//! The 18 recovered MotionTracker commands are [`TrackerCommand::MoveTrackPoint`] (16: up, down,
//! left, right by 1 or 10 screen pixels, with the attach point or, `keep_attach`, without it) and
//! `TrackerCommand::SetPointsBarIn {}` / `TrackerCommand::SetPointsBarOut {}` (the in and out point
//! of the "Motion Tracker Points" bar, set to the current frame). The rest of the enum is the
//! typed surface that makes the analysis drivable: add and remove points, set the current frame
//! and the view zoom (screen pixels per layer pixel, which the screen-pixel nudges divide by),
//! place a point on a frame (manual correction) and analyse forward or backward over the bar from
//! the current frame (pigment's tracker; re-analysing from a corrected frame is the correction
//! path of STU-CMP-065). [`TrackerSession::motion_track`] hands the result to
//! [`crate::track_apply`] (apply to target, stabilise).
//!
//! Every command is validated before it changes anything (a refused command leaves the session as
//! it was); analysis observes the caller's cancel token and reports determinate progress.
//! Recovered default bindings (arrow keys, Shift for 10 pixels, Alt to keep the attach point,
//! Alt+[ / Alt+] for the bar) are the caller's keymap and not part of this surface.
#![deny(clippy::arithmetic_side_effects)]

use crate::track_apply::{MotionTrack, TrackApplyError, TrackFrame};
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::track_math::{
    LumaFrame, TrackDirection, TrackError, TrackLimits, TrackParams, TrackPointSpec, TrackSample,
    TrackStatus, track_points,
};
use std::fmt;

/// Track points one session may hold.
pub const MAX_SESSION_POINTS: usize = 64;
/// Frames one session may span.
pub const MAX_SESSION_FRAMES: usize = 100_000;
/// Zoom range (screen pixels per layer pixel).
pub const MIN_ZOOM: f64 = 1.0 / 64.0;
pub const MAX_ZOOM: f64 = 64.0;

/// The command vocabulary lives in pigment (`track_math`, the tracking owner, which has serde), so
/// a command has one serialisable definition (lane SMB-AE-KEY-TRACK-3) without serde in composite.
pub use hsk_studio_pigment::track_math::{Nudge, NudgeStep, TrackerCommand};

#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackerError {
    /// A point index, frame, radius, zoom or position out of range, or a point without a position
    /// where the command needs one.
    Parameter,
    /// The bar's in point would follow its out point.
    BarOrder,
    /// The analysis refused (pigment's typed error, including cancellation).
    Track(TrackError),
    Overflow,
}
impl TrackerError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Parameter => "tracker_parameter",
            Self::BarOrder => "tracker_bar_order",
            Self::Track(e) => e.code(),
            Self::Overflow => "tracker_overflow",
        }
    }
}
impl fmt::Display for TrackerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for TrackerError {}

/// What one command did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackerReceipt {
    pub command: &'static str,
    /// Frames written by an analysis (0 otherwise).
    pub frames_analysed: usize,
    pub points: usize,
}

/// One track point of a session.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionPoint {
    pub feature_radius: u32,
    pub search_radius: u32,
    /// Attach point minus feature centre (layer pixels).
    pub attach_offset: [f64; 2],
    /// Per frame of the session: the analysed or placed sample.
    pub samples: Vec<Option<TrackSample>>,
}

/// A tracker session over `frame_count` frames.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackerSession {
    frame_count: usize,
    current: usize,
    bar_in: usize,
    bar_out: usize,
    zoom: f64,
    points: Vec<SessionPoint>,
}

fn finite2(p: [f64; 2]) -> bool {
    p.iter().all(|v| v.is_finite())
}

impl TrackerSession {
    /// A session at frame 0, bar over every frame, zoom 1.
    pub fn new(frame_count: usize) -> Result<Self, TrackerError> {
        if frame_count == 0 || frame_count > MAX_SESSION_FRAMES {
            return Err(TrackerError::Parameter);
        }
        Ok(Self {
            frame_count,
            current: 0,
            bar_in: 0,
            bar_out: frame_count.checked_sub(1).ok_or(TrackerError::Overflow)?,
            zoom: 1.0,
            points: Vec::new(),
        })
    }
    pub fn current(&self) -> usize {
        self.current
    }
    pub fn bar(&self) -> (usize, usize) {
        (self.bar_in, self.bar_out)
    }
    pub fn points(&self) -> &[SessionPoint] {
        &self.points
    }

    /// The analysed / placed samples of `point` in frame order.
    pub fn samples(&self, point: usize) -> Option<Vec<TrackSample>> {
        self.points.get(point).map(|p| p.samples.iter().flatten().copied().collect())
    }

    /// `point`'s attach-point motion (rotation 0, scale 1) for [`crate::track_apply`].
    pub fn motion_track(&self, point: usize) -> Result<MotionTrack, TrackApplyError> {
        let p = self.points.get(point).ok_or(TrackApplyError::Parameter)?;
        MotionTrack::new(
            p.samples
                .iter()
                .flatten()
                .filter(|s| s.status.is_tracked())
                .map(|s| TrackFrame {
                    frame: s.frame,
                    attach: [s.position[0] + p.attach_offset[0], s.position[1] + p.attach_offset[1]],
                    rotation_deg: 0.0,
                    scale: 1.0,
                })
                .collect(),
        )
    }

    /// Runs one command (validated first: a refused command changes nothing). `frames` are the
    /// session's frames (needed by `Analyze` only; any slice for the others).
    #[allow(clippy::too_many_arguments)]
    pub fn apply(
        &mut self,
        command: TrackerCommand,
        frames: &[LumaFrame],
        params: &TrackParams,
        limits: &TrackLimits,
        cancel: &CancellationToken,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<TrackerReceipt, TrackerError> {
        cancel.check().map_err(|_| TrackerError::Track(TrackError::Canceled))?;
        let mut analysed = 0usize;
        match command {
            TrackerCommand::MoveTrackPoint { point, direction, step, keep_attach } => {
                let px = match step {
                    NudgeStep::One => 1.0,
                    NudgeStep::Ten => 10.0,
                } / self.zoom;
                let (dx, dy) = match direction {
                    Nudge::Up => (0.0, -px),
                    Nudge::Down => (0.0, px),
                    Nudge::Left => (-px, 0.0),
                    Nudge::Right => (px, 0.0),
                };
                let current = self.current;
                let p = self.points.get_mut(point).ok_or(TrackerError::Parameter)?;
                let s = p.samples.get_mut(current).and_then(Option::as_mut).ok_or(TrackerError::Parameter)?;
                s.position = [s.position[0] + dx, s.position[1] + dy];
                s.status = TrackStatus::Tracked;
                s.confidence = 1.0;
                if keep_attach {
                    p.attach_offset = [p.attach_offset[0] - dx, p.attach_offset[1] - dy];
                }
            }
            TrackerCommand::SetPointsBarIn {} => {
                if self.current > self.bar_out {
                    return Err(TrackerError::BarOrder);
                }
                self.bar_in = self.current;
            }
            TrackerCommand::SetPointsBarOut {} => {
                if self.current < self.bar_in {
                    return Err(TrackerError::BarOrder);
                }
                self.bar_out = self.current;
            }
            TrackerCommand::AddPoint { position, feature_radius, search_radius, attach_offset } => {
                if self.points.len() >= MAX_SESSION_POINTS
                    || !finite2(position)
                    || !finite2(attach_offset)
                    || feature_radius == 0
                    || feature_radius > limits.max_feature_radius
                    || search_radius == 0
                    || search_radius > limits.max_search_radius
                {
                    return Err(TrackerError::Parameter);
                }
                let mut samples = vec![None; self.frame_count];
                if let Some(slot) = samples.get_mut(self.current) {
                    *slot = Some(TrackSample { frame: self.current, position, confidence: 1.0, status: TrackStatus::Tracked });
                }
                self.points.push(SessionPoint { feature_radius, search_radius, attach_offset, samples });
            }
            TrackerCommand::RemovePoint { point } => {
                if point >= self.points.len() {
                    return Err(TrackerError::Parameter);
                }
                self.points.remove(point);
            }
            TrackerCommand::SetCurrentFrame { frame } => {
                if frame >= self.frame_count {
                    return Err(TrackerError::Parameter);
                }
                self.current = frame;
            }
            TrackerCommand::SetZoom { screen_px_per_layer_px } => {
                if !(screen_px_per_layer_px.is_finite() && (MIN_ZOOM..=MAX_ZOOM).contains(&screen_px_per_layer_px)) {
                    return Err(TrackerError::Parameter);
                }
                self.zoom = screen_px_per_layer_px;
            }
            TrackerCommand::PlacePoint { point, frame, position } => {
                if frame >= self.frame_count || !finite2(position) {
                    return Err(TrackerError::Parameter);
                }
                let p = self.points.get_mut(point).ok_or(TrackerError::Parameter)?;
                let slot = p.samples.get_mut(frame).ok_or(TrackerError::Parameter)?;
                *slot = Some(TrackSample { frame, position, confidence: 1.0, status: TrackStatus::Tracked });
            }
            TrackerCommand::Analyze { direction } => {
                analysed = self.analyze(direction, frames, params, limits, cancel, progress)?;
            }
        }
        Ok(TrackerReceipt { command: command.key(), frames_analysed: analysed, points: self.points.len() })
    }

    #[allow(clippy::too_many_arguments)]
    fn analyze(
        &mut self,
        direction: TrackDirection,
        frames: &[LumaFrame],
        params: &TrackParams,
        limits: &TrackLimits,
        cancel: &CancellationToken,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<usize, TrackerError> {
        if frames.len() != self.frame_count || self.current < self.bar_in || self.current > self.bar_out {
            return Err(TrackerError::Parameter);
        }
        let bar_end = self.bar_out.checked_add(1).ok_or(TrackerError::Overflow)?;
        let window = frames.get(self.bar_in..bar_end).ok_or(TrackerError::Parameter)?;
        let start = self.current.checked_sub(self.bar_in).ok_or(TrackerError::Overflow)?;
        let current = self.current;
        let mut owners = Vec::new();
        let mut specs = Vec::new();
        for (i, p) in self.points.iter().enumerate() {
            if let Some(Some(s)) = p.samples.get(current) {
                owners.push(i);
                specs.push(TrackPointSpec { position: s.position, feature_radius: p.feature_radius, search_radius: p.search_radius });
            }
        }
        if specs.is_empty() {
            return Err(TrackerError::Parameter);
        }
        let tracks = track_points(window, start, direction, &specs, params, limits, cancel, progress).map_err(TrackerError::Track)?;
        let mut written = 0usize;
        for (owner, track) in owners.iter().zip(tracks) {
            let p = self.points.get_mut(*owner).ok_or(TrackerError::Overflow)?;
            for s in track {
                let frame = s.frame.checked_add(self.bar_in).ok_or(TrackerError::Overflow)?;
                if frame == current {
                    continue;
                }
                if let Some(slot) = p.samples.get_mut(frame) {
                    *slot = Some(TrackSample { frame, ..s });
                    written = written.saturating_add(1);
                }
            }
        }
        Ok(written)
    }
}
