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
//! - clip pan envelopes (lane SMB-AU-MULTITRACK-3, Folio `StudioClipPanEnvelope`, via
//!   [`TimelineMixStream::with_clip_envelopes`]): a clip with an envelope gets the score pan lane
//!   `clamp(track pan + pan, -1, 1)` at `timeline_start + offset_ticks`, evaluated at the points
//!   and linear between them, held outside them; score's pan law applies per clip (mono
//!   equal-power `(cos a, sin a)`, `a = (pan + 1) pi/4`; stereo balance `L min(1, 1 - pan)`,
//!   `R min(1, 1 + pan)`). Adding the track pan is a Studio reading (the bus mix renders each strip
//!   at track pan 0, so there the lane is the clip envelope itself; module `bus_mix`). Audition's
//!   clip pan behaviour is UNVERIFIED (D5).
//!
//! Timeline positions must sit on the audio sample grid (`not_frame_aligned` otherwise). One score
//! session mixes at most `hsk_studio_score::MAX_TRACKS` clips; `build_timeline_mix` refuses more
//! (`limit_exceeded`, before any sample is copied). The session is the master [`AudioClock`] of
//! playback: its `position()` is the next sample it renders; a timeline with nothing audible mixes
//! one silent sample so the clock still runs.
//!
//! [`TimelineMixStream`] lifts the cap (lane SMB-PULSE-PLAYBACK-2): the same planned clips, sorted
//! by start, are packed into groups of at most `MAX_TRACKS` consecutive clips; a group's score
//! session is built only when the play position reaches the group's span (clip samples are copied
//! then, not before) and dropped when the position passes it; every block sums the groups active
//! in it. Score sums tracks linearly in f64 and publishes f32, so the stream equals the
//! single-session mix within f32 rounding of the partial sums (test
//! `streamed_mix_of_more_than_64_clips_matches_the_hand_mix`). Resident clip samples are bounded
//! by [`StreamLimits::max_resident_frames`] (`limit_exceeded` before copying). Seeking drops every
//! session; a group rebuilt behind the position renders and discards its samples from its start
//! up to the position (bounded by the group's span, counted in [`StreamStats::discarded_frames`]).
#![deny(clippy::arithmetic_side_effects)]

use crate::error::{PulseError, Result};
use crate::playback::AudioClock;
use crate::time::samples_from_ticks_exact;
use crate::timeline_edit::SourceExtents;
use crate::transition::{AUDIO_CROSSFADE, check_at};
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{ClipSource, StudioClipGainEnvelope, StudioClipPanEnvelope, StudioSequence, TrackKind, TransitionParameterValue};
use hsk_studio_score::{
    AutomationLane, ChannelLayout, ClipSpeed, Fade, FadeCurve, MAX_TRACKS, ScoreError, Session,
    TrackSpec,
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

/// One planned timeline clip, independent of the sequence (no samples copied yet).
#[derive(Clone, Debug)]
struct PlannedClip {
    source: ClipSource,
    /// `None` = unit speed.
    speed: Option<ClipSpeed>,
    source_in: usize,
    /// Unit speed: frames cut; other speeds: source window frames.
    frames: usize,
    start: u64,
    /// Frames the clip lasts on the timeline.
    len: u64,
    fade_in: Option<Fade>,
    fade_out: Option<Fade>,
    volume_db: f64,
    pan: f64,
    /// Clip gain envelope (lane SMB-AUDITION-SESSION-2): `(absolute timeline sample, dB)`.
    gain: Vec<(u64, f64)>,
    /// Clip pan envelope (lane SMB-AU-MULTITRACK-3): `(absolute timeline sample, pan)`.
    pan_env: Vec<(u64, f64)>,
}

impl PlannedClip {
    fn end(&self) -> Result<u64> {
        self.start.checked_add(self.len).ok_or(PulseError::Overflow)
    }
    fn spec(&self, sources: &dyn AudioSources, origin: u64) -> Result<TrackSpec> {
        let source = sources
            .audio(&self.source)
            .ok_or(PulseError::SourceExtentUnknown)?;
        let start = self.start.checked_sub(origin).ok_or(PulseError::Underflow)?;
        let mut spec = match self.speed {
            None => TrackSpec::from_source(source, self.source_in, self.frames, start),
            Some(speed) => {
                TrackSpec::from_source_at_speed(source, self.source_in, self.frames, speed, start)
            }
        }
        .map_err(score_error)?;
        spec.volume_db = if self.gain.is_empty() {
            AutomationLane::constant(self.volume_db)
        } else {
            // Track fader plus clip gain, linear in dB between the envelope points; the sum is
            // floored at score's -96 dB fader bottom and refused above its +12 dB top.
            let (lo, hi) = hsk_studio_score::TRACK_VOLUME_DB;
            let mut points = Vec::with_capacity(self.gain.len());
            for (at, db) in &self.gain {
                let at = at.checked_sub(origin).ok_or(PulseError::Underflow)?;
                let v = (self.volume_db + db).max(lo);
                if !v.is_finite() || v > hi {
                    return Err(PulseError::InvalidInput);
                }
                points.push((at, v));
            }
            AutomationLane::new(&points, hsk_studio_score::Interpolation::Linear, (lo, hi)).map_err(score_error)?
        };
        spec.pan = if self.pan_env.is_empty() {
            AutomationLane::constant(self.pan)
        } else {
            // Track pan plus clip pan, clamped to score's pan range at each point.
            let (lo, hi) = hsk_studio_score::TRACK_PAN;
            let mut points = Vec::with_capacity(self.pan_env.len());
            for (at, pan) in &self.pan_env {
                let at = at.checked_sub(origin).ok_or(PulseError::Underflow)?;
                let v = self.pan + pan;
                if !v.is_finite() {
                    return Err(PulseError::InvalidInput);
                }
                points.push((at, v.clamp(lo, hi)));
            }
            AutomationLane::new(&points, hsk_studio_score::Interpolation::Linear, (lo, hi)).map_err(score_error)?
        };
        spec.fade_in = self.fade_in;
        spec.fade_out = self.fade_out;
        Ok(spec)
    }
}

fn audible_tracks(sequence: &StudioSequence) -> Vec<usize> {
    let solo = sequence
        .tracks
        .iter()
        .any(|t| t.kind == TrackKind::Audio && t.soloed);
    (0..sequence.tracks.len())
        .filter(|&t| {
            let tr = &sequence.tracks[t];
            tr.kind == TrackKind::Audio
                && tr.enabled
                && !tr.muted
                && (!solo || tr.soloed)
                && tr.volume > 0.0
        })
        .collect()
}

fn audible_clip_count(sequence: &StudioSequence, audible: &[usize]) -> usize {
    audible.iter().fold(0usize, |n, &t| {
        n.saturating_add(
            sequence.tracks[t]
                .clips
                .iter()
                .filter(|c| c.enabled)
                .count(),
        )
    })
}

/// Plans every audible clip (fades, handles, speeds, track gain) without copying samples; each
/// clip's source must be present in `sources`.
#[allow(clippy::too_many_arguments)]
fn plan_clips(
    sequence: &StudioSequence,
    sample_rate_hz: u32,
    audible: &[usize],
    sources: &dyn AudioSources,
    extents: &dyn SourceExtents,
    gains: &[StudioClipGainEnvelope],
    pans: &[StudioClipPanEnvelope],
    cancel: &CancellationToken,
) -> Result<Vec<PlannedClip>> {
    let mut cuts: Vec<Cut> = Vec::new();
    for &t in audible {
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
    let mut planned = Vec::with_capacity(cuts.len());
    for cut in &cuts {
        cancel.check()?;
        let track = &sequence.tracks[cut.track];
        let clip = &track.clips[cut.clip];
        sources
            .audio(&clip.source_ref)
            .ok_or(PulseError::SourceExtentUnknown)?;
        let (speed, frames, len) = if clip.speed.numerator == 1 && clip.speed.denominator == 1 {
            let len = u64::try_from(cut.frames).map_err(|_| PulseError::Overflow)?;
            (None, cut.frames, len)
        } else {
            let source_frames =
                samples(clip.source_out.checked_sub(clip.source_in)?, sample_rate_hz)?;
            let speed = ClipSpeed::new(
                clip.speed.numerator.unsigned_abs(),
                clip.speed.denominator,
                clip.speed.is_reversed(),
            )
            .map_err(score_error)?;
            let len = speed
                .output_frames(u64::try_from(source_frames).map_err(|_| PulseError::Overflow)?);
            (Some(speed), source_frames, len)
        };
        let mut gain = Vec::new();
        if let Some(env) = gains.iter().find(|g| g.clip_id == clip.clip_id) {
            let at = samples_from_ticks_exact(clip.timeline_start, sample_rate_hz)?;
            for point in &env.points {
                let offset = samples_from_ticks_exact(Ticks::new(point.offset_ticks), sample_rate_hz)?;
                gain.push((at.checked_add(offset).ok_or(PulseError::Overflow)?, point.gain_db));
            }
        }
        let mut pan_env = Vec::new();
        if let Some(env) = pans.iter().find(|g| g.clip_id == clip.clip_id) {
            let at = samples_from_ticks_exact(clip.timeline_start, sample_rate_hz)?;
            for point in &env.points {
                let offset = samples_from_ticks_exact(Ticks::new(point.offset_ticks), sample_rate_hz)?;
                pan_env.push((at.checked_add(offset).ok_or(PulseError::Overflow)?, point.pan));
            }
        }
        planned.push(PlannedClip {
            source: clip.source_ref.clone(),
            speed,
            source_in: cut.source_in,
            frames,
            start: cut.start,
            len,
            fade_in: cut.fade_in,
            fade_out: cut.fade_out,
            volume_db: 20.0 * track.volume.log10(),
            pan: track.pan,
            gain,
            pan_env,
        });
    }
    Ok(planned)
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
    let audible = audible_tracks(sequence);
    if audible_clip_count(sequence, &audible) > MAX_TRACKS {
        return Err(PulseError::LimitExceeded);
    }
    let planned = plan_clips(sequence, sample_rate_hz, &audible, sources, extents, &[], &[], cancel)?;
    let mut specs = Vec::with_capacity(planned.len());
    for p in &planned {
        cancel.check()?;
        specs.push(p.spec(sources, 0)?);
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

/// Bounds of a [`TimelineMixStream`] (authored values).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamLimits {
    /// Most audible clips in one timeline.
    pub max_clips: usize,
    /// Clips per score session (`1..=MAX_TRACKS`).
    pub group_clips: usize,
    /// Clip frames (summed timeline lengths) resident in the active sessions at once.
    pub max_resident_frames: u64,
}

impl Default for StreamLimits {
    fn default() -> Self {
        Self {
            max_clips: 65_536,
            group_clips: MAX_TRACKS,
            max_resident_frames: 1 << 28,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamStats {
    pub groups: usize,
    pub sessions_built: u64,
    pub sessions_dropped: u64,
    pub max_active_sessions: usize,
    pub max_resident_frames: u64,
    /// Frames rendered and thrown away to bring a rebuilt session to the position.
    pub discarded_frames: u64,
}

struct Group {
    first: usize,
    count: usize,
    start: u64,
    end: u64,
    frames: u64,
    session: Option<Session>,
}

/// Streaming mix of any number of clips (see the module docs); also the playback [`AudioClock`].
pub struct TimelineMixStream<S: AudioSources> {
    sources: S,
    rate: u32,
    block: usize,
    limits: StreamLimits,
    clips: Vec<PlannedClip>,
    groups: Vec<Group>,
    position: u64,
    length: u64,
    resident: u64,
    scratch: [Vec<f32>; 2],
    stats: StreamStats,
}

impl<S: AudioSources> TimelineMixStream<S> {
    /// Plans the mix; refusals as [`build_timeline_mix`] except the session cap, plus
    /// `limit_exceeded` above `limits.max_clips` (before planning) or for one group above
    /// `max_resident_frames`, and `invalid_input` for `group_clips` outside `1..=MAX_TRACKS` or
    /// `max_block_frames` 0.
    pub fn new(
        sequence: &StudioSequence,
        sample_rate_hz: u32,
        max_block_frames: usize,
        sources: S,
        extents: &dyn SourceExtents,
        limits: StreamLimits,
        cancel: &CancellationToken,
    ) -> Result<Self> {
        Self::with_clip_gains(sequence, sample_rate_hz, max_block_frames, sources, extents, limits, &[], cancel)
    }

    /// As [`TimelineMixStream::new`] with clip gain envelopes (lane SMB-AUDITION-SESSION-2,
    /// Folio `StudioClipGainEnvelope`): a clip with an envelope gets the score volume lane
    /// `track dB + gain_db` at `timeline_start + offset_ticks` (linear in dB between points, held
    /// outside them; floored at -96 dB, `invalid_input` above +12 dB; offsets off the sample grid
    /// are `not_frame_aligned`).
    #[allow(clippy::too_many_arguments)]
    pub fn with_clip_gains(
        sequence: &StudioSequence,
        sample_rate_hz: u32,
        max_block_frames: usize,
        sources: S,
        extents: &dyn SourceExtents,
        limits: StreamLimits,
        gains: &[StudioClipGainEnvelope],
        cancel: &CancellationToken,
    ) -> Result<Self> {
        Self::with_clip_envelopes(sequence, sample_rate_hz, max_block_frames, sources, extents, limits, gains, &[], cancel)
    }

    /// As [`TimelineMixStream::with_clip_gains`] with clip pan envelopes (lane SMB-AU-MULTITRACK-3,
    /// Folio `StudioClipPanEnvelope`): a clip with an envelope gets the score pan lane
    /// `clamp(track pan + pan, -1, 1)` at `timeline_start + offset_ticks` (linear between points,
    /// held outside them; offsets off the sample grid are `not_frame_aligned`; module docs).
    #[allow(clippy::too_many_arguments)]
    pub fn with_clip_envelopes(
        sequence: &StudioSequence,
        sample_rate_hz: u32,
        max_block_frames: usize,
        sources: S,
        extents: &dyn SourceExtents,
        limits: StreamLimits,
        gains: &[StudioClipGainEnvelope],
        pans: &[StudioClipPanEnvelope],
        cancel: &CancellationToken,
    ) -> Result<Self> {
        cancel.check()?;
        if !(1..=MAX_TRACKS).contains(&limits.group_clips) || max_block_frames == 0 {
            return Err(PulseError::InvalidInput);
        }
        crate::time::ticks_from_samples(1, sample_rate_hz)?;
        let audible = audible_tracks(sequence);
        if audible_clip_count(sequence, &audible) > limits.max_clips {
            return Err(PulseError::LimitExceeded);
        }
        let mut clips = plan_clips(sequence, sample_rate_hz, &audible, &sources, extents, gains, pans, cancel)?;
        clips.sort_by_key(|c| c.start);
        let mut groups = Vec::new();
        let mut length = 0u64;
        for (index, chunk) in clips.chunks(limits.group_clips).enumerate() {
            let first = index
                .checked_mul(limits.group_clips)
                .ok_or(PulseError::Overflow)?;
            let mut end = 0u64;
            let mut frames = 0u64;
            for c in chunk {
                end = end.max(c.end()?);
                frames = frames.checked_add(c.len).ok_or(PulseError::Overflow)?;
            }
            if frames > limits.max_resident_frames {
                return Err(PulseError::LimitExceeded);
            }
            length = length.max(end);
            groups.push(Group {
                first,
                count: chunk.len(),
                start: chunk.first().map_or(0, |c| c.start),
                end,
                frames,
                session: None,
            });
        }
        let stats = StreamStats {
            groups: groups.len(),
            ..StreamStats::default()
        };
        Ok(Self {
            sources,
            rate: sample_rate_hz,
            block: max_block_frames,
            limits,
            clips,
            groups,
            position: 0,
            length,
            resident: 0,
            scratch: [vec![0.0; max_block_frames], vec![0.0; max_block_frames]],
            stats,
        })
    }
    pub fn position(&self) -> u64 {
        self.position
    }
    /// Last clip end in samples.
    pub fn length_samples(&self) -> u64 {
        self.length
    }
    pub fn stats(&self) -> StreamStats {
        self.stats
    }
    pub fn clip_count(&self) -> usize {
        self.clips.len()
    }
    pub fn active_sessions(&self) -> usize {
        self.groups.iter().filter(|g| g.session.is_some()).count()
    }
    pub fn sources(&self) -> &S {
        &self.sources
    }
    pub fn max_block_frames(&self) -> usize {
        self.block
    }
    /// Output layout of the mix (score's stereo bus), for the device stream configuration.
    pub fn layout(&self) -> ChannelLayout {
        ChannelLayout::stereo()
    }

    /// Moves to `sample`; every session is dropped and rebuilt on demand.
    pub fn seek(&mut self, sample: u64) {
        for g in &mut self.groups {
            if g.session.take().is_some() {
                self.stats.sessions_dropped = self.stats.sessions_dropped.saturating_add(1);
            }
        }
        self.resident = 0;
        self.position = sample;
    }

    fn build(&mut self, g: usize, cancel: &CancellationToken) -> Result<()> {
        let resident = self
            .resident
            .checked_add(self.groups[g].frames)
            .ok_or(PulseError::Overflow)?;
        if resident > self.limits.max_resident_frames {
            return Err(PulseError::LimitExceeded);
        }
        let (first, count, origin) = (
            self.groups[g].first,
            self.groups[g].count,
            self.groups[g].start,
        );
        let last = first.checked_add(count).ok_or(PulseError::Overflow)?;
        let mut specs = Vec::with_capacity(count);
        for c in &self.clips[first..last] {
            cancel.check()?;
            specs.push(c.spec(&self.sources, origin)?);
        }
        let mut session = Session::new(self.rate, self.block, specs).map_err(score_error)?;
        // Behind the position (seek into the middle of the group): render and discard.
        let mut local = 0u64;
        let target = self.position.saturating_sub(origin);
        let block = u64::try_from(self.block).map_err(|_| PulseError::Overflow)?;
        while local < target {
            let step = target.saturating_sub(local).min(block);
            let n = usize::try_from(step).map_err(|_| PulseError::Overflow)?;
            let [l, r] = &mut self.scratch;
            session
                .render(local, &mut l[..n], &mut r[..n], cancel)
                .map_err(score_error)?;
            local = local.saturating_add(step);
            self.stats.discarded_frames = self.stats.discarded_frames.saturating_add(step);
        }
        self.groups[g].session = Some(session);
        self.resident = resident;
        self.stats.sessions_built = self.stats.sessions_built.saturating_add(1);
        self.stats.max_resident_frames = self.stats.max_resident_frames.max(resident);
        Ok(())
    }

    /// Renders the next `left.len()` frames (at most `max_block_frames`) at the position and
    /// advances it. `invalid_input` for unequal or empty outputs or a block above the maximum.
    pub fn render(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        cancel: &CancellationToken,
    ) -> Result<()> {
        cancel.check()?;
        let n = left.len();
        if n == 0 || n != right.len() || n > self.block {
            return Err(PulseError::InvalidInput);
        }
        left.fill(0.0);
        right.fill(0.0);
        let from = self.position;
        let to = from
            .checked_add(u64::try_from(n).map_err(|_| PulseError::Overflow)?)
            .ok_or(PulseError::Overflow)?;
        for g in 0..self.groups.len() {
            let (start, end) = (self.groups[g].start, self.groups[g].end);
            if end <= from {
                if self.groups[g].session.take().is_some() {
                    self.resident = self.resident.saturating_sub(self.groups[g].frames);
                    self.stats.sessions_dropped = self.stats.sessions_dropped.saturating_add(1);
                }
                continue;
            }
            if start >= to {
                continue;
            }
            if self.groups[g].session.is_none() {
                self.build(g, cancel)?;
            }
            // Part of the block this group covers.
            let a = start.max(from);
            let b = end.min(to);
            let offset =
                usize::try_from(a.saturating_sub(from)).map_err(|_| PulseError::Overflow)?;
            let len = usize::try_from(b.saturating_sub(a)).map_err(|_| PulseError::Overflow)?;
            let local = a.checked_sub(start).ok_or(PulseError::Underflow)?;
            let [l, r] = &mut self.scratch;
            let session = self.groups[g]
                .session
                .as_mut()
                .ok_or(PulseError::InvalidInput)?;
            session
                .render(local, &mut l[..len], &mut r[..len], cancel)
                .map_err(score_error)?;
            let end_at = offset.checked_add(len).ok_or(PulseError::Overflow)?;
            for (dst, src) in left[offset..end_at].iter_mut().zip(&l[..len]) {
                *dst += *src;
            }
            for (dst, src) in right[offset..end_at].iter_mut().zip(&r[..len]) {
                *dst += *src;
            }
        }
        self.stats.max_active_sessions = self
            .stats
            .max_active_sessions
            .max(self.active_sessions());
        self.position = to;
        Ok(())
    }
}

impl<S: AudioSources> AudioClock for TimelineMixStream<S> {
    fn sample_rate_hz(&self) -> u32 {
        self.rate
    }
    fn sample_position(&self) -> u64 {
        self.position
    }
}
