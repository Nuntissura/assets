//! Timeline audio -> score mixdown (PR-23 timeline side) and the playback audio path (PR-19).
//!
//! [`build_timeline_mix`] turns the sequence's audio tracks into one `hsk_studio_score::Session`:
//! - tracks: enabled, not muted, and when any audio track is soloed only the soloed ones
//!   (STU-VID-021 `enabled`/`muted`/`soloed`); the Folio track `volume` (linear) and `pan`
//!   (STU-FX-137) become the score fader (dB) and pan of every clip on the track; a volume of 0 is
//!   silent and contributes nothing;
//! - clips: every enabled clip is cut from its source audio (an [`AudioSources`] port: planar,
//!   at the mix rate, sample 0 = source tick 0) by `TrackSpec::from_source` (unit speed) or
//!   `TrackSpec::from_source_at_speed` (other speeds, score's band-limited resampler);
//! - transitions: an `audio_crossfade` held as a clip's tail becomes score's fade pair over the
//!   transition span: the outgoing clip runs `after_cut` samples into its tail handle with a fade
//!   out over the whole span, the incoming clip starts `before_cut` samples early from its head
//!   handle with a fade in over the span (score: progress k/frames, `Linear` = `(1 - p) a + p b`,
//!   `EqualPower` = cos/sin). The curve is the transition parameter `curve` (`linear` |
//!   `equal_power`, default `equal_power`; Studio key, the shipped default's curve is
//!   UNVERIFIED). A crossfade on a clip with a non-unit speed is `audio_unsupported`.
//!
//! Timeline positions must sit on the audio sample grid (`not_frame_aligned` otherwise). Score
//! mixes at most `hsk_studio_score::MAX_TRACKS` clips per session; more is `limit_exceeded`,
//! refused before any sample is copied. The session is the master [`AudioClock`] of playback:
//! its `position()` is the next sample it renders; a timeline with nothing audible mixes one
//! silent sample so the clock still runs.
#![deny(clippy::arithmetic_side_effects)]

use crate::error::{PulseError, Result};
use crate::playback::AudioClock;
use crate::time::samples_from_ticks_exact;
use crate::timeline_edit::SourceExtents;
use crate::transition::{AUDIO_CROSSFADE, check_at};
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{ClipSource, StudioSequence, TrackKind, TransitionParameterValue};
use hsk_studio_score::{
    AutomationLane, ClipSpeed, Fade, FadeCurve, MAX_TRACKS, ScoreError, Session, TrackSpec,
};

/// Parameter key of the crossfade curve (`linear` | `equal_power`).
pub const CROSSFADE_CURVE: &str = "curve";

/// Typed port: decoded planar audio (1 or 2 channels) of a clip source at the mix sample rate.
pub trait AudioSources {
    fn audio(&self, source: &ClipSource) -> Option<&[Vec<f32>]>;
}

impl AudioClock for Session {
    fn sample_rate_hz(&self) -> u32 {
        Session::sample_rate_hz(self)
    }
    fn sample_position(&self) -> u64 {
        self.position()
    }
}

fn score_error(e: ScoreError) -> PulseError {
    match e {
        ScoreError::Canceled => PulseError::Canceled,
        ScoreError::OutOfRange => PulseError::PastSourceEnd,
        ScoreError::CapacityExceeded => PulseError::LimitExceeded,
        ScoreError::InvalidRate => PulseError::UnsupportedSampleRate,
        _ => PulseError::AudioRejected,
    }
}

fn samples(ticks: Ticks, rate_hz: u32) -> Result<usize> {
    usize::try_from(samples_from_ticks_exact(ticks, rate_hz)?).map_err(|_| PulseError::Overflow)
}

/// One score clip before construction.
struct Cut {
    track: usize,
    clip: usize,
    source_in: usize,
    frames: usize,
    start: u64,
    fade_in: Option<Fade>,
    fade_out: Option<Fade>,
}

/// Builds the score mixdown of `sequence` at `sample_rate_hz` (see the module docs).
pub fn build_timeline_mix(
    sequence: &StudioSequence,
    sample_rate_hz: u32,
    max_block_frames: usize,
    sources: &dyn AudioSources,
    extents: &dyn SourceExtents,
    cancel: &CancellationToken,
) -> Result<Session> {
    cancel.check()?;
    let solo = sequence
        .tracks
        .iter()
        .any(|t| t.kind == TrackKind::Audio && t.soloed);
    let audible: Vec<usize> = (0..sequence.tracks.len())
        .filter(|&t| {
            let tr = &sequence.tracks[t];
            tr.kind == TrackKind::Audio
                && tr.enabled
                && !tr.muted
                && (!solo || tr.soloed)
                && tr.volume > 0.0
        })
        .collect();
    let count = audible.iter().fold(0usize, |n, &t| {
        n.saturating_add(
            sequence.tracks[t]
                .clips
                .iter()
                .filter(|c| c.enabled)
                .count(),
        )
    });
    if count > MAX_TRACKS {
        return Err(PulseError::LimitExceeded);
    }
    let mut cuts: Vec<Cut> = Vec::with_capacity(count);
    for &t in &audible {
        let clips = &sequence.tracks[t].clips;
        let first_of_track = cuts.len();
        for (c, clip) in clips.iter().enumerate() {
            cancel.check()?;
            if !clip.enabled {
                continue;
            }
            cuts.push(Cut {
                track: t,
                clip: c,
                source_in: samples(clip.source_in, sample_rate_hz)?,
                frames: samples(clip.duration, sample_rate_hz)?,
                start: samples_from_ticks_exact(clip.timeline_start, sample_rate_hz)?,
                fade_in: None,
                fade_out: None,
            });
        }
        // Crossfades between abutting enabled clips of this track.
        for i in first_of_track..cuts.len() {
            let (c, clip) = (cuts[i].clip, &clips[cuts[i].clip]);
            let Some(tr) = clip.transitions.tail.as_ref() else {
                continue;
            };
            if tr.transition_kind != AUDIO_CROSSFADE {
                continue;
            }
            let next = i
                .checked_add(1)
                .filter(|j| *j < cuts.len() && cuts[*j].clip == c.saturating_add(1));
            let Some(j) = next else { continue };
            let placement = check_at(sequence, t, c, extents)?;
            if clip.speed.numerator != 1 || clip.speed.denominator != 1 {
                return Err(PulseError::AudioUnsupported);
            }
            let incoming = &clips[cuts[j].clip];
            if incoming.speed.numerator != 1 || incoming.speed.denominator != 1 {
                return Err(PulseError::AudioUnsupported);
            }
            let before = samples(placement.before_cut, sample_rate_hz)?;
            let after = samples(placement.after_cut, sample_rate_hz)?;
            let span = u64::try_from(before.checked_add(after).ok_or(PulseError::Overflow)?)
                .map_err(|_| PulseError::Overflow)?;
            let curve = match tr
                .parameters
                .iter()
                .find(|p| p.param_key == CROSSFADE_CURVE)
            {
                None => FadeCurve::EqualPower,
                Some(p) => match &p.param_value {
                    TransitionParameterValue::Enumeration(value) if value == "linear" => {
                        FadeCurve::Linear
                    }
                    TransitionParameterValue::Enumeration(value) if value == "equal_power" => {
                        FadeCurve::EqualPower
                    }
                    _ => return Err(PulseError::InvalidInput),
                },
            };
            let fade = Fade {
                frames: span,
                curve,
            };
            cuts[i].frames = cuts[i]
                .frames
                .checked_add(after)
                .ok_or(PulseError::Overflow)?;
            cuts[i].fade_out = Some(fade);
            let early = u64::try_from(before).map_err(|_| PulseError::Overflow)?;
            cuts[j].source_in = cuts[j]
                .source_in
                .checked_sub(before)
                .ok_or(PulseError::PastSourceStart)?;
            cuts[j].start = cuts[j]
                .start
                .checked_sub(early)
                .ok_or(PulseError::Underflow)?;
            cuts[j].frames = cuts[j]
                .frames
                .checked_add(before)
                .ok_or(PulseError::Overflow)?;
            cuts[j].fade_in = Some(fade);
        }
    }
    let mut specs = Vec::with_capacity(cuts.len());
    for cut in &cuts {
        cancel.check()?;
        let track = &sequence.tracks[cut.track];
        let clip = &track.clips[cut.clip];
        let source = sources
            .audio(&clip.source_ref)
            .ok_or(PulseError::SourceExtentUnknown)?;
        let mut spec = if clip.speed.numerator == 1 && clip.speed.denominator == 1 {
            TrackSpec::from_source(source, cut.source_in, cut.frames, cut.start)
        } else {
            let source_frames =
                samples(clip.source_out.checked_sub(clip.source_in)?, sample_rate_hz)?;
            let speed = ClipSpeed::new(
                clip.speed.numerator.unsigned_abs(),
                clip.speed.denominator,
                clip.speed.is_reversed(),
            )
            .map_err(score_error)?;
            TrackSpec::from_source_at_speed(source, cut.source_in, source_frames, speed, cut.start)
        }
        .map_err(score_error)?;
        spec.volume_db = AutomationLane::constant(20.0 * track.volume.log10());
        spec.pan = AutomationLane::constant(track.pan);
        spec.fade_in = cut.fade_in;
        spec.fade_out = cut.fade_out;
        specs.push(spec);
    }
    if specs.is_empty() {
        // Nothing audible: one silent sample keeps the session running as the playback clock.
        specs.push(TrackSpec::new(vec![vec![0.0]], 0));
    }
    Session::new(sample_rate_hz, max_block_frames, specs).map_err(|e| match e {
        ScoreError::OutOfRange => PulseError::InvalidInput,
        other => score_error(other),
    })
}
