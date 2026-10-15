//! Transitions as typed timeline records (STU-VID-032/032a): a timed blend AT the edit point
//! between two abutting clips on one track. Pulse owns the record, its placement around the cut,
//! the handle (source beyond the trimmed bounds) accounting and the per-tick evaluation
//! ([`transition_frame`]: which source tick of each clip plays and the blend progress). Pixels are
//! composite's: a cross dissolve at progress `p` is the incoming frame at opacity `p` composited
//! normally over the opaque outgoing frame, which equals `(1 - p) * out + p * in` (test
//! `cross_dissolve_frame_matches_hand_computed_blend`). For non-opaque outgoing frames that over
//! composite is not a pure mix; a mix operator is a composite follow-up. Premiere Pro's frame
//! sampling of a dissolve (which progress the first frame shows) is UNVERIFIED (D5): here frame
//! `k` of an `n`-frame dissolve has progress `k / n`.
#![deny(clippy::arithmetic_side_effects)]

use crate::error::{PulseError, Result};
use crate::range::{Grid, TickDelta, TickRange};
use crate::rational::SpeedMath;
use crate::timeline_edit::{SourceExtents, rate_of};
use hsk_studio_accord::{CancellationToken, FrameRate, Ticks};
use hsk_studio_folio::{StudioClip, StudioSequence, TrackKind};

/// Most transitions one annotation set holds.
pub const MAX_TRANSITIONS: usize = 65_536;

/// The shipped defaults (STU-VID-032a): a cross dissolve for video, a crossfade for audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransitionKind {
    CrossDissolve,
    AudioCrossfade,
}

/// STU-VID-032 `alignment`; `Custom` places the centre at `cut + center_offset`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransitionAlignment {
    CenteredOnCut,
    StartAtCut,
    EndAtCut,
    Custom,
}

/// One transition at the cut between `outgoing_clip_id` and the clip abutting it,
/// `incoming_clip_id`, on the same track.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transition {
    pub transition_id: String,
    pub kind: TransitionKind,
    pub outgoing_clip_id: String,
    pub incoming_clip_id: String,
    pub duration: Ticks,
    pub alignment: TransitionAlignment,
    /// Signed ticks from the cut to the transition centre; non-zero only with `Custom`.
    pub center_offset: i64,
    /// The declared shortfall choice "repeat the end frame": a clip without enough handle holds
    /// its last available source tick instead of the transition being refused.
    pub repeat_end_frame: bool,
}

/// Where a transition sits: `span = [cut - before_cut, cut + after_cut)` on `track_id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransitionPlacement {
    pub track_id: String,
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
    pub kind: TransitionKind,
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

pub(crate) fn locate<'a>(
    sequence: &'a StudioSequence,
    clip_id: &str,
) -> Option<(usize, usize, &'a StudioClip)> {
    sequence.tracks.iter().enumerate().find_map(|(t, track)| {
        track
            .clips
            .iter()
            .position(|c| c.clip_id == clip_id)
            .map(|c| (t, c, &track.clips[c]))
    })
}

fn whole_frames(ticks: Ticks, rate: FrameRate) -> Result<()> {
    Grid::Frames(rate).check_delta(TickDelta::between(Ticks::new(0), ticks))
}

/// Places `transition` in `sequence`: both clips on one track, abutting, kind matching the
/// track kind, whole frames on video, inside both clips.
pub fn transition_placement(
    sequence: &StudioSequence,
    transition: &Transition,
) -> Result<TransitionPlacement> {
    let (t_out, c_out, out) =
        locate(sequence, &transition.outgoing_clip_id).ok_or(PulseError::UnknownClip)?;
    let (t_in, c_in, inc) =
        locate(sequence, &transition.incoming_clip_id).ok_or(PulseError::UnknownClip)?;
    let track = &sequence.tracks[t_out];
    let kind_ok = matches!(
        (transition.kind, track.kind),
        (TransitionKind::CrossDissolve, TrackKind::Video)
            | (TransitionKind::AudioCrossfade, TrackKind::Audio)
    );
    if t_in != t_out || !kind_ok {
        return Err(PulseError::InvalidInput);
    }
    let cut = out.timeline_end().ok_or(PulseError::Overflow)?;
    if c_out.checked_add(1) != Some(c_in) || inc.timeline_start != cut {
        return Err(PulseError::NotAdjacent);
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
    let before = match transition.alignment {
        TransitionAlignment::CenteredOnCut => i128::from(half),
        TransitionAlignment::StartAtCut => 0,
        TransitionAlignment::EndAtCut => i128::from(d.value()),
        TransitionAlignment::Custom => {
            i128::from(half).saturating_sub(i128::from(transition.center_offset))
        }
    };
    if transition.alignment != TransitionAlignment::Custom && transition.center_offset != 0 {
        return Err(PulseError::InvalidInput);
    }
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
        cut,
        span: TickRange::new(cut.checked_sub(before)?, cut.checked_add(after)?)?,
        before_cut: before,
        after_cut: after,
    })
}

/// Source available beyond the clip's playing tail (`tail`) or before its head.
fn available(clip: &StudioClip, tail: bool, extents: &dyn SourceExtents) -> Option<Ticks> {
    // Forward plays source_in -> source_out; reversed plays source_out -> source_in.
    let beyond_out = tail != clip.speed.is_reversed();
    if beyond_out {
        extents
            .extent(&clip.source_ref)
            .map(|e| Ticks::new(e.value().saturating_sub(clip.source_out.value())))
    } else {
        Some(clip.source_in)
    }
}

/// Handle accounting for a placed transition (STU-VID-032).
pub fn transition_handles(
    sequence: &StudioSequence,
    transition: &Transition,
    extents: &dyn SourceExtents,
) -> Result<HandleReport> {
    let placement = transition_placement(sequence, transition)?;
    let (_, _, out) =
        locate(sequence, &transition.outgoing_clip_id).ok_or(PulseError::UnknownClip)?;
    let (_, _, inc) =
        locate(sequence, &transition.incoming_clip_id).ok_or(PulseError::UnknownClip)?;
    Ok(HandleReport {
        outgoing_required: out.speed.span_for_duration(placement.after_cut)?,
        outgoing_available: available(out, true, extents),
        incoming_required: inc.speed.span_for_duration(placement.before_cut)?,
        incoming_available: available(inc, false, extents),
    })
}

/// Refuses a transition the sequence cannot play: placement errors, and a handle shortfall
/// (`insufficient_handle`, or `source_extent_unknown` when a length is unknown) unless the record
/// declares `repeat_end_frame`.
pub(crate) fn check_transition(
    sequence: &StudioSequence,
    transition: &Transition,
    extents: &dyn SourceExtents,
) -> Result<TransitionPlacement> {
    let placement = transition_placement(sequence, transition)?;
    let report = transition_handles(sequence, transition, extents)?;
    if !transition.repeat_end_frame {
        match report.shortfall() {
            (None, _) | (_, None) => return Err(PulseError::SourceExtentUnknown),
            _ if !report.sufficient() => return Err(PulseError::InsufficientHandle),
            _ => {}
        }
    }
    Ok(placement)
}

/// Source tick `clip` plays at timeline `tick` (which may lie outside the clip: a handle), with
/// the hold rule of `repeat_end_frame` at the available bounds `[0, extent]`.
fn source_at(
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

/// Evaluates the transition covering `tick` on `track_id`, if any.
pub fn transition_frame<'a>(
    sequence: &StudioSequence,
    transitions: impl IntoIterator<Item = &'a Transition>,
    track_id: &str,
    tick: Ticks,
    extents: &dyn SourceExtents,
    cancel: &CancellationToken,
) -> Result<Option<TransitionFrame>> {
    for (i, transition) in transitions.into_iter().enumerate() {
        if i.is_multiple_of(256) {
            cancel.check()?;
        }
        let placement = transition_placement(sequence, transition)?;
        if placement.track_id != track_id || !placement.span.contains(tick) {
            continue;
        }
        let (_, _, out) =
            locate(sequence, &transition.outgoing_clip_id).ok_or(PulseError::UnknownClip)?;
        let (_, _, inc) =
            locate(sequence, &transition.incoming_clip_id).ok_or(PulseError::UnknownClip)?;
        let hold = transition.repeat_end_frame;
        let (outgoing_source, outgoing_held) = source_at(out, tick, extents, hold)?;
        let (incoming_source, incoming_held) = source_at(inc, tick, extents, hold)?;
        return Ok(Some(TransitionFrame {
            transition_id: transition.transition_id.clone(),
            kind: transition.kind,
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
