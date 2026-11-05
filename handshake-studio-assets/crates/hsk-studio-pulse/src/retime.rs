//! Clip retime choices, source-frame resolution and frame hold (lane SMB-PR-RETIME-HOLD-1).
//! [STU-VID-031]: "`Maintain audio pitch` is an independent boolean. The frame-interpolation choice
//! governs what happens between source frames and is normative". [STU-VID-031a]: "`Frame hold`
//! freezes a clip at a chosen source frame -- at the playhead, at a clip marker, at the in point, or
//! at the out point -- as an explicit property, and `insert frame hold segment` splits out a held
//! span as its own clip". The record is Folio's `StudioClip.retime` ([`ClipRetime`]): a field on the
//! clip, so razor, nest and every other clone carry it (test `edits_carry_the_retime_record`).
//!
//! **Source-frame resolution** ([`resolve_source_frames`]): one timeline tick of a clip to a typed
//! [`SourceFrameAnswer`], in exact integer arithmetic (no float anywhere in the mapping). The caller
//! states the source's frame grid ([`SourceFrameGrid`]: pulse holds no source frame rate). The
//! position is `source_in + offset * |speed|`; reversed it is `L - offset * |speed|` where `L` is the
//! start of the last frame of the window, so the head of a reversed clip shows the last frame and
//! unit reverse is the exact mirror of forward (test `reverse_speed_resolves_exactly`).
//! - `frame_sampling`: the nearest source frame; a tie goes to the later source frame.
//! - `frame_blending`: the frame at or before the position, the next one, and the exact reduced
//!   share of the later one (0 exactly on a frame).
//! - `optical_flow`: the same two frames and weight, marked as an estimation
//!   ([`SourceFrameAnswer::estimation_mark`]): any diagnostic comparing the frame to a source frame
//!   carries that mark ([STU-VID-031] "MUST be marked as an estimation"). The motion estimator is
//!   `hsk_studio_pigment::frame_blend::{optical_flow, pixel_motion}`; pulse has no edge to pigment
//!   and renders nothing, so wiring this answer to it is the renderer's work.
//! - Only frames of the source window are named: a position past the last frame (or below the first
//!   when reversed) stays on it, with weight 0.
//!
//! **Frame hold**: [`plan_set_frame_hold`], [`plan_clear_frame_hold`] and
//! [`plan_insert_frame_hold_segment`], each ONE proposal (one revision step, one Folio validation,
//! one [`crate::TimelineInverse`] that [`crate::plan_timeline_revert`] undoes), like every other
//! pulse edit ([STU-VID-040]). Pulse commits nothing. A held clip answers its one frame at every
//! timeline tick, in [`resolve_source_frames`] and in [`crate::clip_source_at`], so the program
//! monitor reads it through `frame_layers` (test `the_program_monitor_path_reads_the_hold`).
//! [`plan_set_retime_options`] sets the interpolation choice and `Maintain audio pitch` the same way.
//!
//! Declared readings (the spec is silent on each; Premiere's behaviour is UNVERIFIED, D5):
//! - The held frame is resolved once, when the hold is set, and stored as the first tick of that
//!   frame. It does not follow later edits: a razor or a trim leaves pieces whose window no longer
//!   holds it and they still show it (Folio therefore does not bind it to the window; the planners
//!   refuse a frame outside the window when it is chosen, `frame_hold_outside_source_window`).
//! - In point and out point are the clip's source bounds whatever the speed's sign. The window is
//!   half open, so the frame at the out point is the last frame starting before `source_out`.
//! - The playhead picks the frame `frame_sampling` shows there, whatever the clip's interpolation.
//! - Audio: a frame hold freezes the picture only. A clip on a non-video track cannot be held
//!   (`frame_hold_not_video`, Folio refuses it too); a linked audio partner keeps playing its own
//!   window through the held span, and `insert frame hold segment` cuts it at the same ticks so each
//!   link group stays one clip per track (test `a_held_clip_leaves_its_linked_audio_playing`).
//! - A hold applies to the named clip, not its link group; the same goes for the retime options.
//! - `insert frame hold segment` holds a span of the clip in place: nothing moves and the outer
//!   pieces keep their windows and frames (test `insert_frame_hold_segment_splits_out_a_held_span`).
//!
//! A hold on a multicam clip or a clip of a multicam source sequence is refused
//! `frame_hold_multicam`, as [`crate::plan_rate_stretch`] refuses reverse there. Clearing stays open.
//! The other way round, multicam refuses a clip it would read that already holds a frame
//! (`multicam_hold_unsupported`, `multicam.rs` module docs). A set, a clear or an options change
//! leaves the clip window unchanged: [`crate::EditProposal::changes`] lists it as
//! [`crate::ClipChange::RetimeRecord`] and the clip is in the write footprint (test
//! `set_and_clear_are_one_step_with_one_inverse`); session commits whole sequences, so the record
//! reaches the document either way (session test `retime_commit`).
//!
//! Not here: `Time remapping` ([STU-VID-031a] keyframed speed), pixels, and the pitch-preserving
//! mix (`maintain_audio_pitch` is stored; `build_timeline_mix` does not read it yet).
#![deny(clippy::arithmetic_side_effects)]

use crate::error::PulseError;
use crate::range::{TickDelta, TickRange};
use crate::rate::{RateError, members, multicam_reads};
use crate::timeline_edit::{
    EditContext, EditProposal, Footprint, SourceExtents, grid_of, propose, rate_of, split_clip,
};
use crate::transition::locate;
use hsk_studio_accord::{FrameRate, Ticks};
pub use hsk_studio_folio::{ClipFrameHold, ClipRetime, FrameHoldAnchor, FrameInterpolation};
use hsk_studio_folio::{ClipSource, Snapshot, StudioClip, StudioMulticamRecords, StudioSequence, TrackKind};
use std::collections::BTreeSet;

/// The mark any diagnostic carries when it compares an `optical_flow` frame to a source frame.
pub const ESTIMATION_MARK: &str = "estimation";

/// Code of the typed outcome of a path that cannot render a clip's interpolation choice. It never
/// shows a floored frame instead.
pub const FRAME_INTERPOLATION_UNAVAILABLE: &str = "frame_interpolation_unavailable";

/// Typed port: the frame grid of a source, from the media owner. `None`: not stated; the layer is
/// then read on its sequence's own frame grid (a declared Studio reading).
pub trait SourceGrids {
    fn grid(&self, source: &ClipSource) -> Option<SourceFrameGrid>;
}

/// States no grid: every layer is read on its sequence's frame grid.
pub struct SequenceGrid;
impl SourceGrids for SequenceGrid {
    fn grid(&self, _source: &ClipSource) -> Option<SourceFrameGrid> {
        None
    }
}

/// The frame grid of a clip's source, stated by the caller from the media's metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceFrameGrid {
    ticks_per_frame: u64,
}

impl SourceFrameGrid {
    /// `invalid_input` for zero.
    pub fn new(ticks_per_frame: u64) -> crate::Result<Self> {
        if ticks_per_frame == 0 {
            return Err(PulseError::InvalidInput);
        }
        Ok(Self { ticks_per_frame })
    }
    pub fn from_rate(rate: FrameRate) -> Self {
        Self {
            ticks_per_frame: rate.ticks_per_frame(),
        }
    }
    pub fn ticks_per_frame(self) -> u64 {
        self.ticks_per_frame
    }
    fn index_of(self, tick: Ticks) -> crate::Result<u64> {
        tick.value()
            .checked_div(self.ticks_per_frame)
            .ok_or(PulseError::InvalidInput)
    }
    fn frame(self, index: u64) -> crate::Result<SourceFrameRef> {
        let start = index
            .checked_mul(self.ticks_per_frame)
            .ok_or(PulseError::Overflow)?;
        Ok(SourceFrameRef {
            index,
            start: Ticks::new(start),
        })
    }
}

/// One source frame: its number on the grid and its first SOURCE tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceFrameRef {
    pub index: u64,
    pub start: Ticks,
}

/// The exact reduced share of the later frame of a pair, in `[0, 1)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameWeight {
    numerator: u128,
    denominator: u128,
}

impl FrameWeight {
    pub const ZERO: FrameWeight = FrameWeight {
        numerator: 0,
        denominator: 1,
    };
    pub fn numerator(self) -> u128 {
        self.numerator
    }
    pub fn denominator(self) -> u128 {
        self.denominator
    }
    /// The share as a float, for the renderer's blend weight.
    #[allow(clippy::cast_precision_loss)]
    pub fn as_f64(self) -> f64 {
        self.numerator as f64 / self.denominator as f64
    }
    fn reduced(numerator: u128, denominator: u128) -> Self {
        let (mut a, mut b) = (numerator, denominator);
        while b != 0 {
            (a, b) = (b, a.checked_rem(b).unwrap_or(0));
        }
        match (numerator.checked_div(a), denominator.checked_div(a)) {
            (Some(numerator), Some(denominator)) => Self {
                numerator,
                denominator,
            },
            _ => Self::ZERO,
        }
    }
}

/// What a clip shows at one timeline tick (module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceFrameAnswer {
    /// `frame_sampling`: the nearest source frame.
    Sampled(SourceFrameRef),
    /// `frame_blending`: `weight` is the share of `to`.
    Blended {
        from: SourceFrameRef,
        to: SourceFrameRef,
        weight: FrameWeight,
    },
    /// `optical_flow`: the frame is synthesised between `from` and `to` at `weight`; an estimation.
    Estimated {
        from: SourceFrameRef,
        to: SourceFrameRef,
        weight: FrameWeight,
    },
    /// A frame hold: this one source frame, whatever the interpolation choice.
    Held(SourceFrameRef),
}

impl SourceFrameAnswer {
    /// The frame shown alone, or the earlier frame of a pair.
    pub fn frame(&self) -> SourceFrameRef {
        match *self {
            Self::Sampled(frame) | Self::Held(frame) => frame,
            Self::Blended { from, .. } | Self::Estimated { from, .. } => from,
        }
    }
    /// The two frames and the share of the later one when the answer needs both. A zero share is
    /// exactly the earlier frame, so it needs one decode and no blend.
    pub fn pair(&self) -> Option<(SourceFrameRef, SourceFrameRef, FrameWeight)> {
        match *self {
            Self::Blended { from, to, weight } | Self::Estimated { from, to, weight }
                if weight.numerator != 0 =>
            {
                Some((from, to, weight))
            }
            _ => None,
        }
    }
    pub fn is_estimation(&self) -> bool {
        matches!(self, Self::Estimated { .. })
    }
    /// [`ESTIMATION_MARK`] for an estimated frame: a diagnostic comparing it to a source frame must
    /// carry it ([STU-VID-031]).
    pub fn estimation_mark(&self) -> Option<&'static str> {
        self.is_estimation().then_some(ESTIMATION_MARK)
    }
}

/// The frame pair around a source position, inside the window (module docs).
struct Neighbours {
    from: u64,
    to: u64,
    /// The position's distance past `from`, over `modulus` (one source frame).
    remainder: u128,
    modulus: u128,
}

impl Neighbours {
    fn on(frame: u64) -> Self {
        Self {
            from: frame,
            to: frame,
            remainder: 0,
            modulus: 1,
        }
    }
    /// A tie goes to the later source frame.
    fn nearest(&self) -> u64 {
        if self.remainder >= self.modulus.saturating_sub(self.remainder) {
            self.to
        } else {
            self.from
        }
    }
}

/// First and last source frame of the clip's window (`source_out` is exclusive).
fn window_frames(clip: &StudioClip, grid: SourceFrameGrid) -> crate::Result<(u64, u64)> {
    if clip.source_in >= clip.source_out {
        return Err(PulseError::InvalidRange);
    }
    let last_tick = clip.source_out.checked_sub(Ticks::new(1))?;
    Ok((grid.index_of(clip.source_in)?, grid.index_of(last_tick)?))
}

fn neighbours(clip: &StudioClip, tick: Ticks, grid: SourceFrameGrid) -> crate::Result<Neighbours> {
    if clip.speed.numerator == 0 {
        return Err(PulseError::ZeroSpeed);
    }
    if clip.speed.denominator == 0 {
        return Err(PulseError::InvalidSpeed);
    }
    let (first, last) = window_frames(clip, grid)?;
    let den = u128::from(clip.speed.denominator);
    let ticks_per_frame = u128::from(grid.ticks_per_frame);
    let offset = tick.checked_sub(clip.timeline_start)?;
    let moved = u128::from(offset.value())
        .checked_mul(u128::from(clip.speed.numerator.unsigned_abs()))
        .ok_or(PulseError::Overflow)?;
    // The position in units of 1 / den tick.
    let position = if clip.speed.is_reversed() {
        let anchor = u128::from(last)
            .checked_mul(ticks_per_frame)
            .and_then(|v| v.checked_mul(den))
            .ok_or(PulseError::Overflow)?;
        anchor.checked_sub(moved)
    } else {
        let anchor = u128::from(clip.source_in.value())
            .checked_mul(den)
            .ok_or(PulseError::Overflow)?;
        Some(anchor.checked_add(moved).ok_or(PulseError::Overflow)?)
    };
    let Some(position) = position else {
        return Ok(Neighbours::on(first));
    };
    let modulus = den
        .checked_mul(ticks_per_frame)
        .ok_or(PulseError::Overflow)?;
    let index = position.checked_div(modulus).ok_or(PulseError::InvalidInput)?;
    let remainder = position.checked_rem(modulus).ok_or(PulseError::InvalidInput)?;
    if index >= u128::from(last) {
        return Ok(Neighbours::on(last));
    }
    if index < u128::from(first) {
        return Ok(Neighbours::on(first));
    }
    let from = u64::try_from(index).map_err(|_| PulseError::Overflow)?;
    let to = from.checked_add(1).ok_or(PulseError::Overflow)?;
    Ok(Neighbours {
        from,
        to,
        remainder,
        modulus,
    })
}

/// What `clip` shows at timeline `tick` (module docs). `outside_range` for a tick off the clip;
/// `past_source_end` for a stored hold at or past a known source length.
pub fn resolve_source_frames(
    clip: &StudioClip,
    tick: Ticks,
    grid: SourceFrameGrid,
    extents: &dyn SourceExtents,
) -> crate::Result<SourceFrameAnswer> {
    let end = clip.timeline_end().ok_or(PulseError::Overflow)?;
    if tick < clip.timeline_start || tick >= end {
        return Err(PulseError::OutsideRange);
    }
    if let Some(hold) = &clip.retime.frame_hold {
        if extents
            .extent(&clip.source_ref)
            .is_some_and(|e| hold.source_tick >= e)
        {
            return Err(PulseError::PastSourceEnd);
        }
        return Ok(SourceFrameAnswer::Held(
            grid.frame(grid.index_of(hold.source_tick)?)?,
        ));
    }
    let pair = neighbours(clip, tick, grid)?;
    let (from, to) = (grid.frame(pair.from)?, grid.frame(pair.to)?);
    let weight = FrameWeight::reduced(pair.remainder, pair.modulus);
    Ok(match clip.retime.frame_interpolation {
        FrameInterpolation::FrameSampling => SourceFrameAnswer::Sampled(grid.frame(pair.nearest())?),
        FrameInterpolation::FrameBlending => SourceFrameAnswer::Blended { from, to, weight },
        FrameInterpolation::OpticalFlow => SourceFrameAnswer::Estimated { from, to, weight },
    })
}

/// Where a frame hold takes its source frame ([STU-VID-031a]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HoldAt {
    /// The frame `frame_sampling` shows at this timeline tick.
    Playhead(Ticks),
    /// The frame holding the start of this marker of the clip.
    ClipMarker(String),
    InPoint,
    OutPoint,
}

/// Freezes `clip_id` (only that clip; module docs) at the frame `at` names on `grid`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetFrameHold {
    pub clip_id: String,
    pub at: HoldAt,
    pub grid: SourceFrameGrid,
}

/// Holds `span` (timeline ticks inside `clip_id`) as its own clip. The clip and each linked partner
/// are cut at the span's edges that lie inside the clip. Pulse mints no ids: each new piece takes
/// the next of `fresh_clip_ids` (clip first, then each partner in track order, left to right); a
/// linked clip's pieces after the first take the next of `fresh_link_groups`, one group per piece
/// position. Extra ids are unused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InsertFrameHoldSegment {
    pub clip_id: String,
    pub span: TickRange,
    pub at: HoldAt,
    pub grid: SourceFrameGrid,
    pub fresh_clip_ids: Vec<String>,
    pub fresh_link_groups: Vec<String>,
}

/// Sets what is `Some` and keeps the rest of the clip's retime record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetimeOptions {
    pub clip_id: String,
    pub frame_interpolation: Option<FrameInterpolation>,
    pub maintain_audio_pitch: Option<bool>,
}

/// Typed retime refusal: pulse's own vocabulary plus the hold-specific codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetimeError {
    Pulse(PulseError),
    /// The chosen frame is outside the clip's source window.
    HoldOutsideSourceWindow,
    /// Only a clip on a video track has a frame to hold.
    HoldNotVideo,
    /// A multicam clip or a clip of a multicam source sequence.
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

/// Shared one-step transaction around [`propose`] keeping the retime refusal typed.
fn plan_typed(
    snapshot: &Snapshot,
    sequence_id: &str,
    expected_revision: u64,
    ctx: &EditContext,
    edit: impl FnOnce(&StudioSequence, &mut Footprint) -> RetimeResult<StudioSequence>,
) -> RetimeResult<EditProposal> {
    let mut refusal: Option<RetimeError> = None;
    let result = propose(
        snapshot,
        sequence_id,
        expected_revision,
        ctx,
        true,
        |old, reads| {
            edit(old, reads).map_err(|e| {
                let code = match e {
                    RetimeError::Pulse(p) => p,
                    _ => PulseError::InvalidInput,
                };
                refusal = Some(e);
                code
            })
        },
    );
    match (result, refusal) {
        (Ok(p), _) => Ok(p),
        (Err(_), Some(r)) => Err(r),
        (Err(e), None) => Err(e.into()),
    }
}

/// The clip on an unlocked track, as (track, clip) indices, recorded in the read footprint.
fn editable(
    seq: &StudioSequence,
    clip_id: &str,
    reads: &mut Footprint,
) -> RetimeResult<(usize, usize)> {
    let (t, c) = locate(seq, clip_id).ok_or(PulseError::UnknownClip)?;
    reads.add(&seq.tracks[t].track_id);
    reads.add(clip_id);
    if seq.tracks[t].locked {
        return Err(PulseError::TrackLocked.into());
    }
    Ok((t, c))
}

fn require_holdable(
    seq: &StudioSequence,
    t: usize,
    clip_id: &str,
    records: Option<&StudioMulticamRecords>,
) -> RetimeResult<()> {
    if seq.tracks[t].kind != TrackKind::Video {
        return Err(RetimeError::HoldNotVideo);
    }
    if multicam_reads(seq, clip_id, records) {
        return Err(RetimeError::HoldMulticam);
    }
    Ok(())
}

/// The stored hold for `at` on `clip` (module docs: the frame is resolved here, once).
fn resolve_hold(
    clip: &StudioClip,
    at: &HoldAt,
    grid: SourceFrameGrid,
    extents: &dyn SourceExtents,
) -> RetimeResult<ClipFrameHold> {
    let (first, last) = window_frames(clip, grid)?;
    let (index, anchor) = match at {
        HoldAt::Playhead(tick) => {
            let end = clip.timeline_end().ok_or(PulseError::Overflow)?;
            if *tick < clip.timeline_start || *tick >= end {
                return Err(RetimeError::HoldOutsideSourceWindow);
            }
            (
                neighbours(clip, *tick, grid)?.nearest(),
                FrameHoldAnchor::Playhead,
            )
        }
        HoldAt::ClipMarker(marker_id) => {
            let marker = clip
                .markers
                .iter()
                .find(|m| m.marker_id == *marker_id)
                .ok_or(PulseError::UnknownMarker)?;
            if marker.start < clip.source_in || marker.start >= clip.source_out {
                return Err(RetimeError::HoldOutsideSourceWindow);
            }
            (grid.index_of(marker.start)?, FrameHoldAnchor::ClipMarker)
        }
        HoldAt::InPoint => (first, FrameHoldAnchor::InPoint),
        HoldAt::OutPoint => (last, FrameHoldAnchor::OutPoint),
    };
    let frame = grid.frame(index)?;
    if extents
        .extent(&clip.source_ref)
        .is_some_and(|e| frame.start >= e)
    {
        return Err(PulseError::PastSourceEnd.into());
    }
    Ok(ClipFrameHold {
        source_tick: frame.start,
        anchor,
    })
}

/// Plans a frame hold on `edit.clip_id` against `snapshot` at exactly `expected_revision` (module
/// docs). `multicam` is the document's multicam record set when it has one.
pub fn plan_set_frame_hold(
    snapshot: &Snapshot,
    sequence_id: &str,
    expected_revision: u64,
    edit: &SetFrameHold,
    multicam: Option<&StudioMulticamRecords>,
    ctx: &EditContext,
) -> RetimeResult<EditProposal> {
    plan_typed(snapshot, sequence_id, expected_revision, ctx, |old, reads| {
        let (t, c) = editable(old, &edit.clip_id, reads)?;
        require_holdable(old, t, &edit.clip_id, multicam)?;
        let hold = resolve_hold(&old.tracks[t].clips[c], &edit.at, edit.grid, ctx.extents)?;
        let mut out = old.clone();
        out.tracks[t].clips[c].retime.frame_hold = Some(hold);
        Ok(out)
    })
}

/// Plans the removal of the frame hold of `clip_id`: the clip follows its window again. `no_change`
/// on a clip without a hold.
pub fn plan_clear_frame_hold(
    snapshot: &Snapshot,
    sequence_id: &str,
    expected_revision: u64,
    clip_id: &str,
    ctx: &EditContext,
) -> RetimeResult<EditProposal> {
    plan_typed(snapshot, sequence_id, expected_revision, ctx, |old, reads| {
        let (t, c) = editable(old, clip_id, reads)?;
        let mut out = old.clone();
        out.tracks[t].clips[c].retime.frame_hold = None;
        Ok(out)
    })
}

/// The piece link groups of a linked clip: `count` fresh ones, distinct and not in use.
fn fresh_groups<'a>(
    seq: &StudioSequence,
    edit: &'a InsertFrameHoldSegment,
    count: usize,
) -> RetimeResult<Vec<&'a str>> {
    let groups: Vec<&str> = edit
        .fresh_link_groups
        .iter()
        .take(count)
        .map(String::as_str)
        .collect();
    if groups.len() < count {
        return Err(PulseError::InsufficientIds.into());
    }
    let distinct: BTreeSet<&str> = groups.iter().copied().collect();
    let in_use = seq
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .any(|k| k.link_group.as_deref().is_some_and(|g| distinct.contains(g)));
    if distinct.len() != groups.len() || in_use {
        return Err(PulseError::DuplicateId.into());
    }
    Ok(groups)
}

fn insert_segment(
    seq: &StudioSequence,
    edit: &InsertFrameHoldSegment,
    records: Option<&StudioMulticamRecords>,
    ctx: &EditContext,
    reads: &mut Footprint,
) -> RetimeResult<StudioSequence> {
    let rate = rate_of(seq)?;
    let list = members(seq, &edit.clip_id).map_err(|e| match e {
        RateError::Pulse(p) => RetimeError::Pulse(p),
        _ => RetimeError::Pulse(PulseError::InvalidInput),
    })?;
    let primary = *list.first().ok_or(PulseError::UnknownClip)?;
    require_holdable(seq, primary.0, &edit.clip_id, records)?;
    let lead = &seq.tracks[primary.0].clips[primary.1];
    let whole = TickRange::from_start_duration(lead.timeline_start, lead.duration)?;
    let span = edit.span;
    if span.start() >= span.end() || span.start() < whole.start() || span.end() > whole.end() {
        return Err(PulseError::OutsideRange.into());
    }
    let hold = resolve_hold(lead, &edit.at, edit.grid, ctx.extents)?;
    let has_left = span.start() > whole.start();
    let cuts: Vec<Ticks> = [has_left.then_some(span.start()), (span.end() < whole.end()).then_some(span.end())]
        .into_iter()
        .flatten()
        .collect();
    let groups = if lead.link_group.is_some() {
        fresh_groups(seq, edit, cuts.len())?
    } else {
        Vec::new()
    };
    let taken: BTreeSet<&str> = seq
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .map(|k| k.clip_id.as_str())
        .collect();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut ids = edit.fresh_clip_ids.iter();
    let mut out = seq.clone();
    for &(t, c) in &list {
        ctx.cancel.check().map_err(PulseError::from)?;
        let track = &seq.tracks[t];
        let clip = &track.clips[c];
        reads.add(&track.track_id);
        reads.add(&clip.clip_id);
        let grid = grid_of(track, rate);
        let mut rest = clip.clone();
        let mut pieces = Vec::with_capacity(cuts.len().saturating_add(1));
        for (i, &cut) in cuts.iter().enumerate() {
            grid.check_delta(TickDelta::between(clip.timeline_start, cut))?;
            let id = ids.next().ok_or(PulseError::InsufficientIds)?;
            if taken.contains(id.as_str()) || !seen.insert(id.as_str()) {
                return Err(PulseError::DuplicateId.into());
            }
            let (left, mut right) = split_clip(&rest, cut, id)?;
            reads.splits.push((clip.clip_id.clone(), id.clone()));
            if let Some(group) = groups.get(i) {
                right.link_group = Some((*group).to_owned());
            }
            pieces.push(left);
            rest = right;
        }
        pieces.push(rest);
        if (t, c) == primary {
            let held = pieces
                .get_mut(usize::from(has_left))
                .ok_or(PulseError::InvalidInput)?;
            held.retime.frame_hold = Some(hold);
        }
        out.tracks[t].clips.splice(c..=c, pieces);
    }
    Ok(out)
}

/// Plans `insert frame hold segment` against `snapshot` at exactly `expected_revision` (module
/// docs). Cuts on a video track are on whole frames (`not_frame_aligned`); a span off the clip is
/// `outside_range`, and so is a cut a linked partner does not span.
pub fn plan_insert_frame_hold_segment(
    snapshot: &Snapshot,
    sequence_id: &str,
    expected_revision: u64,
    edit: &InsertFrameHoldSegment,
    multicam: Option<&StudioMulticamRecords>,
    ctx: &EditContext,
) -> RetimeResult<EditProposal> {
    plan_typed(snapshot, sequence_id, expected_revision, ctx, |old, reads| {
        insert_segment(old, edit, multicam, ctx, reads)
    })
}

/// Plans a change of the clip's interpolation choice and `Maintain audio pitch` (module docs).
pub fn plan_set_retime_options(
    snapshot: &Snapshot,
    sequence_id: &str,
    expected_revision: u64,
    edit: &RetimeOptions,
    ctx: &EditContext,
) -> RetimeResult<EditProposal> {
    plan_typed(snapshot, sequence_id, expected_revision, ctx, |old, reads| {
        let (t, c) = editable(old, &edit.clip_id, reads)?;
        let mut out = old.clone();
        let retime = &mut out.tracks[t].clips[c].retime;
        if let Some(method) = edit.frame_interpolation {
            retime.frame_interpolation = method;
        }
        if let Some(pitch) = edit.maintain_audio_pitch {
            retime.maintain_audio_pitch = pitch;
        }
        Ok(out)
    })
}
