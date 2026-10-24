//! Rate stretch and Speed/Duration planners (lane SMB-PR-RATE-STRETCH-1, [STU-VID-026] row "Rate
//! stretch": "Change a clip's `duration` by changing its `speed`, keeping the full source window";
//! [STU-VID-031]: "A clip's speed and duration are two views of one value: fixing either derives the
//! other. Reverse is a negative speed").
//!
//! Both planners keep the clip's source window (`source_in` / `source_out` never move) and derive
//! the speed from the source span and the new timeline duration through the assembly planner's
//! [`FourPointFit::ChangeSpeed`] rule (`speed = source span / sequence span`, sign kept, exact and
//! reduced; no copy of that arithmetic here). Each plan is ONE proposal (one revision step, one
//! Folio validation, one [`crate::TimelineInverse`] that [`crate::plan_timeline_revert`] undoes),
//! like every other pulse edit ([STU-VID-040]). Pulse commits nothing.
//!
//! - [`plan_rate_stretch`]: the dragged edge ([`RateEdge::Head`] / [`RateEdge::Tail`]) moves to a
//!   new timeline tick; the other edge stays (no ripple).
//! - [`plan_speed_duration`]: the Speed/Duration command (`cmd.clip.speed`, `Ctrl+R`): a speed
//!   magnitude or a duration plus the reverse flag; the head stays and the tail follows (no ripple).
//!
//! Limits (declared Studio readings: [STU-VID-031] declares no numeric speed limits, so the bound
//! of the [STU-VID-026] row is read as representability plus the timeline limits below):
//! - the new duration is at least one sequence frame (`rate_stretch_below_one_frame`);
//! - on a video track the moved edge stays on whole frames (`not_frame_aligned`);
//! - the speed is representable: `i64` numerator, reduced, the derived duration a whole number of
//!   ticks (`rate_stretch_unrepresentable`);
//! - the clip never overlaps a neighbour on its track (`rate_stretch_overlap`); a head moved before
//!   tick 0 is `outside_range`;
//! - linked partners (shared `link_group`, one clip per track, else `linked_edit_unsupported`)
//!   stretch together: each member's moved edge moves by the same delta and each keeps its own
//!   source window, so the group stays in sync; a locked member track is `track_locked`;
//! - reverse on a multicam clip, or on any clip of a declared multicam source sequence, is refused
//!   `rate_stretch_multicam_reverse` (lane SMB-PR-MULTICAM-2 refuses reverse speed in resolve and
//!   flatten, so this planner never creates state multicam refuses to read).
//!
//! Spec gaps (declared, not stored): [STU-VID-031]'s `Maintain audio pitch` boolean and the
//! frame-interpolation choice (`frame_sampling` / `frame_blending` / `optical_flow`) have no field
//! on `StudioClip`; these planners take neither and the GUI shows them as unavailable. A change of
//! only the reverse flag leaves the clip window unchanged, so [`crate::EditProposal::changes`] is
//! empty for it (the prospective sequence and the inverse still carry the new speed).
//! Premiere Pro's rate-stretch limits, cursor zones and Speed/Duration defaults are UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]

use crate::assemble::{AssemblyPoints, FourPointFit, resolve_points};
use crate::error::PulseError;
use crate::range::{TickDelta, offset};
use crate::rational::{SpeedMath, reduced_speed};
use crate::timeline_edit::{EditContext, EditProposal, grid_of, propose, rate_of};
use hsk_studio_accord::Ticks;
use hsk_studio_folio::{Snapshot, Speed, StudioMulticamRecords, StudioSequence};
use std::collections::BTreeSet;

/// STU-VID-041 command ids carried by the GUI (bindings belong to the GUI).
pub const CMD_CLIP_SPEED: &str = "cmd.clip.speed";
pub const CMD_TOOL_RATE_STRETCH: &str = "cmd.tools.05ratestretch";

/// The clip edge a rate stretch drags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateEdge {
    Head,
    Tail,
}

impl RateEdge {
    pub fn key(self) -> &'static str {
        match self {
            Self::Head => "head",
            Self::Tail => "tail",
        }
    }
}

/// Rate stretch: `edge` of `clip_id` moves to timeline tick `new_edge` (head: the new timeline
/// start; tail: the new exclusive end); the source window stays and the speed follows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RateStretch {
    pub clip_id: String,
    pub edge: RateEdge,
    pub new_edge: Ticks,
}

/// The value the Speed/Duration command fixes ([STU-VID-031]: the other is derived).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeedDurationTarget {
    /// Speed magnitude (positive numerator, reduced); the direction comes from `reverse`.
    Speed(Speed),
    /// New timeline duration in ticks.
    Duration(Ticks),
}

/// The Speed/Duration command (`cmd.clip.speed`): the head stays, the tail follows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeedDuration {
    pub clip_id: String,
    pub target: SpeedDurationTarget,
    /// `true` = reverse (negative speed), `false` = forward.
    pub reverse: bool,
}

/// Typed rate refusal: pulse's own vocabulary plus the rate-specific codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateError {
    Pulse(PulseError),
    /// The stretched clip would overlap a neighbour on its track.
    Overlap,
    /// The new duration is shorter than one sequence frame.
    BelowOneFrame,
    /// The speed (or the duration it derives) is not representable exactly.
    Unrepresentable,
    /// Reverse speed on a multicam clip or on a clip of a multicam source sequence.
    MulticamReverse,
}

impl RateError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Pulse(e) => e.code(),
            Self::Overlap => "rate_stretch_overlap",
            Self::BelowOneFrame => "rate_stretch_below_one_frame",
            Self::Unrepresentable => "rate_stretch_unrepresentable",
            Self::MulticamReverse => "rate_stretch_multicam_reverse",
        }
    }
}

impl From<PulseError> for RateError {
    fn from(e: PulseError) -> Self {
        Self::Pulse(e)
    }
}

impl std::fmt::Display for RateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for RateError {}

pub type RateResult<T> = std::result::Result<T, RateError>;

/// One member's retime: start and duration move by the same deltas for every link-group member.
struct Retime {
    start_delta: TickDelta,
    duration_delta: TickDelta,
    /// `Some(true)` reverse, `Some(false)` forward, `None` keep the direction.
    reverse: Option<bool>,
}

fn unrepresentable(e: PulseError) -> RateError {
    match e {
        PulseError::InvalidSpeed | PulseError::Inexact | PulseError::Overflow | PulseError::ZeroSpeed => RateError::Unrepresentable,
        other => RateError::Pulse(other),
    }
}

/// The clip, its whole link group (one clip per track), primary first, as (track, clip) indices.
fn members(seq: &StudioSequence, clip_id: &str) -> RateResult<Vec<(usize, usize)>> {
    let primary = seq
        .tracks
        .iter()
        .enumerate()
        .find_map(|(t, track)| track.clips.iter().position(|c| c.clip_id == clip_id).map(|c| (t, c)))
        .ok_or(PulseError::UnknownClip)?;
    let mut out = vec![primary];
    if let Some(group) = &seq.tracks[primary.0].clips[primary.1].link_group {
        for (t, track) in seq.tracks.iter().enumerate() {
            for (c, clip) in track.clips.iter().enumerate() {
                if (t, c) != primary && clip.link_group.as_ref() == Some(group) {
                    out.push((t, c));
                }
            }
        }
    }
    let tracks: BTreeSet<usize> = out.iter().map(|(t, _)| *t).collect();
    if tracks.len() != out.len() {
        return Err(PulseError::LinkedEditUnsupported.into());
    }
    if out.iter().any(|(t, _)| seq.tracks[*t].locked) {
        return Err(PulseError::TrackLocked.into());
    }
    Ok(out)
}

/// A reversed speed would be created on a clip multicam reads.
fn multicam_reads(seq: &StudioSequence, clip_id: &str, records: Option<&StudioMulticamRecords>) -> bool {
    records.is_some_and(|r| r.clip(clip_id).is_some() || r.multicam_sequence(&seq.sequence_id).is_some())
}

fn retime_members(seq: &StudioSequence, primary: &str, retime: &Retime, records: Option<&StudioMulticamRecords>, ctx: &EditContext) -> RateResult<StudioSequence> {
    let rate = rate_of(seq)?;
    let one_frame = rate.ticks_per_frame();
    let list = members(seq, primary)?;
    let mut out = seq.clone();
    for &(t, c) in &list {
        ctx.cancel.check().map_err(PulseError::from)?;
        let track = &seq.tracks[t];
        let clip = &track.clips[c];
        let grid = grid_of(track, rate);
        grid.check_delta(retime.start_delta)?;
        grid.check_delta(retime.duration_delta)?;
        let start = offset(clip.timeline_start, retime.start_delta).map_err(|e| if e == PulseError::Underflow { PulseError::OutsideRange } else { e })?;
        let duration = match offset(clip.duration, retime.duration_delta) {
            Ok(d) => d,
            Err(PulseError::Underflow) => return Err(RateError::BelowOneFrame),
            Err(e) => return Err(e.into()),
        };
        if duration.value() < one_frame {
            return Err(RateError::BelowOneFrame);
        }
        let end = start.checked_add(duration).map_err(|_| PulseError::Overflow)?;
        let previous_end = c.checked_sub(1).and_then(|i| track.clips.get(i)).map(|p| p.timeline_end().ok_or(PulseError::Overflow)).transpose()?;
        let next_start = c.checked_add(1).and_then(|i| track.clips.get(i)).map(|n| n.timeline_start);
        if previous_end.is_some_and(|p| p > start) || next_start.is_some_and(|n| n < end) {
            return Err(RateError::Overlap);
        }
        let reversed = retime.reverse.unwrap_or(clip.speed.is_reversed());
        if reversed && multicam_reads(seq, &clip.clip_id, records) {
            return Err(RateError::MulticamReverse);
        }
        let magnitude = clip.speed.numerator.checked_abs().ok_or(RateError::Unrepresentable)?;
        let signed = Speed { numerator: if reversed { magnitude.checked_neg().ok_or(RateError::Unrepresentable)? } else { magnitude }, denominator: clip.speed.denominator };
        let points = AssemblyPoints { source_in: Some(clip.source_in), source_out: Some(clip.source_out), sequence_in: Some(start), sequence_out: Some(end) };
        let resolved = resolve_points(points, signed, Some(FourPointFit::ChangeSpeed)).map_err(unrepresentable)?;
        if resolved.duration != duration {
            return Err(RateError::Unrepresentable);
        }
        let edited = &mut out.tracks[t].clips[c];
        edited.timeline_start = start;
        edited.duration = duration;
        edited.speed = resolved.speed;
    }
    Ok(out)
}

/// Shared one-step transaction around [`propose`] keeping the rate refusal typed.
fn plan_retime(snapshot: &Snapshot, sequence_id: &str, expected_revision: u64, ctx: &EditContext, edit: impl FnOnce(&StudioSequence) -> RateResult<StudioSequence>) -> RateResult<EditProposal> {
    let mut refusal: Option<RateError> = None;
    let result = propose(snapshot, sequence_id, expected_revision, ctx, true, |old, reads| {
        for track in &old.tracks {
            reads.add(&track.track_id);
        }
        edit(old).map_err(|e| {
            let code = match e {
                RateError::Pulse(p) => p,
                _ => PulseError::InvalidInput,
            };
            refusal = Some(e);
            code
        })
    });
    match (result, refusal) {
        (Ok(p), _) => Ok(p),
        (Err(_), Some(r)) => Err(r),
        (Err(e), None) => Err(e.into()),
    }
}

fn find<'a>(snapshot: &'a Snapshot, sequence_id: &str, clip_id: &str) -> RateResult<&'a hsk_studio_folio::StudioClip> {
    let seq = snapshot.document().sequences.iter().find(|s| s.sequence_id == sequence_id).ok_or(PulseError::UnknownSequence)?;
    seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.clip_id == clip_id).ok_or_else(|| PulseError::UnknownClip.into())
}

/// Plans a rate stretch of `edit.clip_id` against `snapshot` at exactly `expected_revision`
/// (module docs). `multicam` is the document's multicam record set when it has one.
pub fn plan_rate_stretch(snapshot: &Snapshot, sequence_id: &str, expected_revision: u64, edit: &RateStretch, multicam: Option<&StudioMulticamRecords>, ctx: &EditContext) -> RateResult<EditProposal> {
    let clip = find(snapshot, sequence_id, &edit.clip_id)?;
    let end = clip.timeline_end().ok_or(PulseError::Overflow)?;
    let retime = match edit.edge {
        RateEdge::Tail => Retime { start_delta: TickDelta::ZERO, duration_delta: TickDelta::between(end, edit.new_edge), reverse: None },
        RateEdge::Head => {
            let moved = TickDelta::between(clip.timeline_start, edit.new_edge);
            Retime { start_delta: moved, duration_delta: moved.negated(), reverse: None }
        }
    };
    plan_retime(snapshot, sequence_id, expected_revision, ctx, |old| retime_members(old, &edit.clip_id, &retime, multicam, ctx))
}

/// Plans the Speed/Duration command on `edit.clip_id` (module docs): a speed magnitude derives the
/// duration (`span * den / num`, exact, else `rate_stretch_unrepresentable`), a duration derives the
/// speed; `reverse` sets the direction.
pub fn plan_speed_duration(snapshot: &Snapshot, sequence_id: &str, expected_revision: u64, edit: &SpeedDuration, multicam: Option<&StudioMulticamRecords>, ctx: &EditContext) -> RateResult<EditProposal> {
    let clip = find(snapshot, sequence_id, &edit.clip_id)?;
    let duration = match edit.target {
        SpeedDurationTarget::Duration(d) => d,
        SpeedDurationTarget::Speed(s) => {
            if s.numerator <= 0 || reduced_speed(s.numerator, s.denominator).ok() != Some(s) {
                return Err(PulseError::InvalidSpeed.into());
            }
            let span = clip.source_out.checked_sub(clip.source_in).map_err(|_| PulseError::InvalidRange)?;
            s.duration_for_span(span).map_err(unrepresentable)?
        }
    };
    let retime = Retime { start_delta: TickDelta::ZERO, duration_delta: TickDelta::between(clip.duration, duration), reverse: Some(edit.reverse) };
    plan_retime(snapshot, sequence_id, expected_revision, ctx, |old| retime_members(old, &edit.clip_id, &retime, multicam, ctx))
}

/// The display percentage of `speed` (`100 * |num| / den`, two decimals, floor), e.g. `"50.00"`.
pub fn speed_percent_label(speed: Speed) -> String {
    let hundredths = u128::from(speed.numerator.unsigned_abs()).saturating_mul(10_000).checked_div(u128::from(speed.denominator)).unwrap_or(0);
    let whole = hundredths.checked_div(100).unwrap_or(0);
    let frac = hundredths.checked_rem(100).unwrap_or(0);
    format!("{}{whole}.{frac:02}", if speed.is_reversed() { "-" } else { "" })
}

/// Parses a decimal percentage (`"50"`, `"33.5"`, at most 6 decimals, no sign) into the exact
/// reduced speed magnitude `percent / 100`; `None` for zero, junk or an unrepresentable value.
pub fn speed_from_percent(text: &str) -> Option<Speed> {
    let text = text.trim().trim_end_matches('%').trim();
    let (whole, frac) = text.split_once('.').unwrap_or((text, ""));
    if (whole.is_empty() && frac.is_empty()) || frac.len() > 6 || !whole.bytes().chain(frac.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    let numerator: i64 = format!("{whole}{frac}").parse().ok()?;
    let scale = 10u64.checked_pow(u32::try_from(frac.len()).ok()?)?;
    let denominator = scale.checked_mul(100)?;
    reduced_speed(numerator, denominator).ok()
}
