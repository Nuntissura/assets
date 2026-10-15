//! Transitions over Folio's stored records (STU-VID-032/032a; lane SMB-PULSE-PLAYBACK moved the
//! model into Folio: `StudioClip.transitions`, `TimelineTransition` = `hsk.studio.transition@1`).
//! A transition at the cut between a clip and the clip abutting it on its track is the OUTGOING
//! clip's `tail` (Folio's storage rule). Pulse owns its placement around the cut, the handle
//! accounting (source beyond the trimmed bounds) and the per-tick evaluation
//! ([`transition_frame`]: which source tick of each clip plays and the blend progress). A clip's
//! `head` transition (single-sided, no abutting outgoing clip) is stored by Folio but not evaluated
//! here yet.
//!
//! Pixels are composite's: a cross dissolve at progress `p` is the incoming frame at opacity `p`
//! composited normally over the opaque outgoing frame, which equals `(1 - p) * out + p * in` (test
//! `cross_dissolve_frame_matches_hand_computed_blend`); for a non-opaque outgoing frame that over
//! composite is not a pure mix. Premiere Pro's dissolve frame sampling is UNVERIFIED (D5): frame `k`
//! of an `n`-frame dissolve has progress `k / n`.
#![deny(clippy::arithmetic_side_effects)]

use crate::error::{PulseError, Result};
use crate::range::{Grid, TickDelta, TickRange};
use crate::rational::SpeedMath;
use crate::timeline_edit::{SourceExtents, rate_of};
use hsk_studio_accord::{CancellationToken, FrameRate, Ticks};
use hsk_studio_folio::{StudioClip, StudioSequence, TrackKind};
pub use hsk_studio_folio::{
    TimelineTransition, TransitionAlignment, TransitionParameter, TransitionParameterValue,
    TransitionSchema,
};

/// `transition_kind` of the shipped video default (STU-VID-032a). Studio key; the canonical
/// `filter_kind` spelling of STU-FX-126 is not verified (UNVERIFIED).
pub const CROSS_DISSOLVE: &str = "cross_dissolve";
/// `transition_kind` of the shipped audio default (STU-VID-032a); same caveat.
pub const AUDIO_CROSSFADE: &str = "audio_crossfade";

/// Where a transition sits: `span = [cut - before_cut, cut + after_cut)` on `track_id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransitionPlacement {
    pub track_id: String,
    pub outgoing_clip_id: String,
    pub incoming_clip_id: String,
    pub cut: Ticks,
    pub span: TickRange,
    pub before_cut: Ticks,
    pub after_cut: Ticks,
}

/// Handle accounting in SOURCE ticks (STU-VID-032): the outgoing clip plays `outgoing_required`
/// beyond its trimmed tail, the incoming clip `incoming_required` before its trimmed head.
/// `None` available = the source length is unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HandleReport {
    pub outgoing_required: Ticks,
    pub outgoing_available: Option<Ticks>,
    pub incoming_required: Ticks,
    pub incoming_available: Option<Ticks>,
}

impl HandleReport {
    /// Source ticks missing on each side `(outgoing, incoming)`; `None` = unknown length.
    pub fn shortfall(&self) -> (Option<Ticks>, Option<Ticks>) {
        let short = |need: Ticks, have: Option<Ticks>| {
            have.map(|h| Ticks::new(need.value().saturating_sub(h.value())))
        };
        (
            short(self.outgoing_required, self.outgoing_available),
            short(self.incoming_required, self.incoming_available),
        )
    }
    pub fn sufficient(&self) -> bool {
        matches!(self.shortfall(), (Some(a), Some(b)) if a.value() == 0 && b.value() == 0)
    }
}

/// One evaluated transition tick: the source tick each clip plays and the blend progress
/// `progress_numerator / progress_denominator` (0 = all outgoing).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransitionFrame {
    pub transition_id: String,
    pub transition_kind: String,
    pub outgoing_clip_id: String,
    pub incoming_clip_id: String,
    pub outgoing_source: Ticks,
    pub incoming_source: Ticks,
    /// The clip held its last available source tick (`repeat_end_frame`).
    pub outgoing_held: bool,
    pub incoming_held: bool,
    pub progress_numerator: u64,
    pub progress_denominator: u64,
}

impl TransitionFrame {
    /// Weight of the incoming clip (cross dissolve: its opacity over the outgoing frame).
    #[allow(clippy::cast_precision_loss)]
    pub fn incoming_weight(&self) -> f64 {
        self.progress_numerator as f64 / self.progress_denominator as f64
    }
}

pub(crate) fn locate(sequence: &StudioSequence, clip_id: &str) -> Option<(usize, usize)> {
    sequence.tracks.iter().enumerate().find_map(|(t, track)| {
        track
            .clips
            .iter()
            .position(|c| c.clip_id == clip_id)
            .map(|c| (t, c))
    })
}

fn whole_frames(ticks: Ticks, rate: FrameRate) -> Result<()> {
    Grid::Frames(rate).check_delta(TickDelta::between(Ticks::new(0), ticks))
}

/// Placement of the tail transition of clip `c` on track `t` (both clips abutting, kind matching
/// the track kind for the two shipped kinds, whole frames on video, inside both clips).
pub(crate) fn placement_at(
    sequence: &StudioSequence,
    t: usize,
    c: usize,
) -> Result<TransitionPlacement> {
    let track = sequence.tracks.get(t).ok_or(PulseError::UnknownTrack)?;
    let out = track.clips.get(c).ok_or(PulseError::UnknownClip)?;
    let transition = out
        .transitions
        .tail
        .as_ref()
        .ok_or(PulseError::UnknownTransition)?;
    let inc = c
        .checked_add(1)
        .and_then(|i| track.clips.get(i))
        .ok_or(PulseError::NotAdjacent)?;
    let cut = out.timeline_end().ok_or(PulseError::Overflow)?;
    if inc.timeline_start != cut {
        return Err(PulseError::NotAdjacent);
    }
    let kind_ok = match transition.transition_kind.as_str() {
        CROSS_DISSOLVE => track.kind == TrackKind::Video,
        AUDIO_CROSSFADE => track.kind == TrackKind::Audio,
        _ => true,
    };
    if !kind_ok {
        return Err(PulseError::InvalidInput);
    }
    let d = transition.duration;
    if d.value() == 0 {
        return Err(PulseError::ZeroDuration);
    }
    let video = track.kind == TrackKind::Video;
    let rate = rate_of(sequence)?;
    let half = if video {
        let tpf = rate.ticks_per_frame();
        let frames = d.value().checked_div(tpf).ok_or(PulseError::InvalidInput)?;
        frames.checked_div(2).and_then(|f| f.checked_mul(tpf))
    } else {
        d.value().checked_div(2)
    }
    .ok_or(PulseError::Overflow)?;
    if transition.alignment != TransitionAlignment::Custom && transition.center_offset != 0 {
        return Err(PulseError::InvalidInput);
    }
    let before = match transition.alignment {
        TransitionAlignment::CenteredOnCut => i128::from(half),
        TransitionAlignment::StartAtCut => 0,
        TransitionAlignment::EndAtCut => i128::from(d.value()),
        TransitionAlignment::Custom => {
            i128::from(half).saturating_sub(i128::from(transition.center_offset))
        }
    };
    let before = u64::try_from(before)
        .ok()
        .filter(|b| *b <= d.value())
        .map(Ticks::new)
        .ok_or(PulseError::OutsideRange)?;
    let after = d.checked_sub(before)?;
    if video {
        whole_frames(d, rate)?;
        whole_frames(before, rate)?;
    }
    if before > out.duration || after > inc.duration {
        return Err(PulseError::OutsideRange);
    }
    Ok(TransitionPlacement {
        track_id: track.track_id.clone(),
        outgoing_clip_id: out.clip_id.clone(),
        incoming_clip_id: inc.clip_id.clone(),
        cut,
        span: TickRange::new(cut.checked_sub(before)?, cut.checked_add(after)?)?,
        before_cut: before,
        after_cut: after,
    })
}

/// Placement of the transition held as `outgoing_clip_id`'s tail.
pub fn transition_placement(
    sequence: &StudioSequence,
    outgoing_clip_id: &str,
) -> Result<TransitionPlacement> {
    let (t, c) = locate(sequence, outgoing_clip_id).ok_or(PulseError::UnknownClip)?;
    placement_at(sequence, t, c)
}

/// Source available beyond the clip's playing tail (`tail`) or before its head.
fn available(clip: &StudioClip, tail: bool, extents: &dyn SourceExtents) -> Option<Ticks> {
    // Forward plays source_in -> source_out; reversed plays source_out -> source_in.
    if tail != clip.speed.is_reversed() {
        extents
            .extent(&clip.source_ref)
            .map(|e| Ticks::new(e.value().saturating_sub(clip.source_out.value())))
    } else {
        Some(clip.source_in)
    }
}

fn handles_at(
    sequence: &StudioSequence,
    t: usize,
    c: usize,
    placement: &TransitionPlacement,
    extents: &dyn SourceExtents,
) -> Result<HandleReport> {
    let clips = &sequence.tracks[t].clips;
    let out = &clips[c];
    let inc = clips
        .get(c.saturating_add(1))
        .ok_or(PulseError::NotAdjacent)?;
    Ok(HandleReport {
        outgoing_required: out.speed.span_for_duration(placement.after_cut)?,
        outgoing_available: available(out, true, extents),
        incoming_required: inc.speed.span_for_duration(placement.before_cut)?,
        incoming_available: available(inc, false, extents),
    })
}

/// Handle accounting for the transition held as `outgoing_clip_id`'s tail (STU-VID-032).
pub fn transition_handles(
    sequence: &StudioSequence,
    outgoing_clip_id: &str,
    extents: &dyn SourceExtents,
) -> Result<HandleReport> {
    let (t, c) = locate(sequence, outgoing_clip_id).ok_or(PulseError::UnknownClip)?;
    let placement = placement_at(sequence, t, c)?;
    handles_at(sequence, t, c, &placement, extents)
}

/// The tail transition of clip `c` on track `t` plays: placement errors, and a handle shortfall
/// (`insufficient_handle`, or `source_extent_unknown` for an unknown length) unless the record
/// declares `repeat_end_frame`.
pub(crate) fn check_at(
    sequence: &StudioSequence,
    t: usize,
    c: usize,
    extents: &dyn SourceExtents,
) -> Result<TransitionPlacement> {
    let placement = placement_at(sequence, t, c)?;
    let repeat = sequence.tracks[t].clips[c]
        .transitions
        .tail
        .as_ref()
        .is_some_and(|tr| tr.repeat_end_frame);
    let report = handles_at(sequence, t, c, &placement, extents)?;
    if !repeat {
        match report.shortfall() {
            (None, _) | (_, None) => return Err(PulseError::SourceExtentUnknown),
            _ if !report.sufficient() => return Err(PulseError::InsufficientHandle),
            _ => {}
        }
    }
    Ok(placement)
}

/// Source tick `clip` plays at timeline `tick` (which may lie outside the clip: a handle), with
/// the hold rule of `repeat_end_frame` at the available bounds `[0, extent]`. Positions between
/// ticks round toward the clip start.
pub fn clip_source_at(
    clip: &StudioClip,
    tick: Ticks,
    extents: &dyn SourceExtents,
    hold: bool,
) -> Result<(Ticks, bool)> {
    let offset = TickDelta::between(clip.timeline_start, tick).value();
    let magnitude = u64::try_from(offset.unsigned_abs()).map_err(|_| PulseError::Overflow)?;
    let num = u128::from(clip.speed.numerator.unsigned_abs());
    let span = u128::from(magnitude)
        .checked_mul(num)
        .and_then(|p| p.checked_div(u128::from(clip.speed.denominator)))
        .ok_or(PulseError::InvalidSpeed)?;
    let span = i128::try_from(span).map_err(|_| PulseError::Overflow)?;
    let signed = if offset < 0 {
        span.checked_neg()
    } else {
        Some(span)
    }
    .ok_or(PulseError::Overflow)?;
    let position = if clip.speed.is_reversed() {
        i128::from(clip.source_out.value()).checked_sub(signed)
    } else {
        i128::from(clip.source_in.value()).checked_add(signed)
    }
    .ok_or(PulseError::Overflow)?;
    // An unknown source length offers no handle past the trimmed out point.
    let limit = i128::from(
        extents
            .extent(&clip.source_ref)
            .unwrap_or(clip.source_out)
            .value(),
    );
    if (0..=limit).contains(&position) {
        return Ok((
            Ticks::new(u64::try_from(position).map_err(|_| PulseError::Overflow)?),
            false,
        ));
    }
    if !hold {
        return Err(PulseError::InsufficientHandle);
    }
    let held = position.clamp(0, limit);
    Ok((
        Ticks::new(u64::try_from(held).map_err(|_| PulseError::Overflow)?),
        true,
    ))
}

/// Evaluates the transition covering `tick` on `track_id`, if any: only the clip under `tick`
/// (as outgoing) and the clip before it (`tick` inside the incoming clip) can hold it, because a
/// placement stays inside both clips.
pub fn transition_frame(
    sequence: &StudioSequence,
    track_id: &str,
    tick: Ticks,
    extents: &dyn SourceExtents,
    cancel: &CancellationToken,
) -> Result<Option<TransitionFrame>> {
    cancel.check()?;
    let t = sequence
        .tracks
        .iter()
        .position(|tr| tr.track_id == track_id)
        .ok_or(PulseError::UnknownTrack)?;
    let clips = &sequence.tracks[t].clips;
    let under = clips.partition_point(|c| c.timeline_start <= tick);
    for c in [under.checked_sub(1), under.checked_sub(2)]
        .into_iter()
        .flatten()
    {
        let Some(transition) = clips[c].transitions.tail.as_ref() else {
            continue;
        };
        let Ok(placement) = placement_at(sequence, t, c) else {
            continue;
        };
        if !placement.span.contains(tick) {
            continue;
        }
        let (out, inc) = (&clips[c], &clips[c.saturating_add(1)]);
        let hold = transition.repeat_end_frame;
        let (outgoing_source, outgoing_held) = clip_source_at(out, tick, extents, hold)?;
        let (incoming_source, incoming_held) = clip_source_at(inc, tick, extents, hold)?;
        return Ok(Some(TransitionFrame {
            transition_id: transition.transition_id.clone(),
            transition_kind: transition.transition_kind.clone(),
            outgoing_clip_id: out.clip_id.clone(),
            incoming_clip_id: inc.clip_id.clone(),
            outgoing_source,
            incoming_source,
            outgoing_held,
            incoming_held,
            progress_numerator: tick.checked_sub(placement.span.start())?.value(),
            progress_denominator: transition.duration.value(),
        }));
    }
    Ok(None)
}

/// Every tail transition that still plays after an edit stays; the others are removed from
/// `sequence` and their ids returned (two overlapping on one track: the later is removed). Used
/// by every edit proposal, so a transition never outlives its cut silently.
pub(crate) fn prune_transitions(
    sequence: &mut StudioSequence,
    extents: &dyn SourceExtents,
    cancel: &CancellationToken,
) -> Result<Vec<String>> {
    let mut dropped = Vec::new();
    for t in 0..sequence.tracks.len() {
        cancel.check()?;
        let mut last_end: Option<Ticks> = None;
        for c in 0..sequence.tracks[t].clips.len() {
            if sequence.tracks[t].clips[c].transitions.tail.is_none() {
                continue;
            }
            let keep = match check_at(sequence, t, c, extents) {
                Ok(placement) if last_end.is_none_or(|end| placement.span.start() >= end) => {
                    last_end = Some(placement.span.end());
                    true
                }
                Ok(_) => false,
                Err(PulseError::Canceled) => return Err(PulseError::Canceled),
                Err(_) => false,
            };
            if !keep && let Some(tr) = sequence.tracks[t].clips[c].transitions.tail.take() {
                dropped.push(tr.transition_id);
            }
        }
    }
    Ok(dropped)
}
