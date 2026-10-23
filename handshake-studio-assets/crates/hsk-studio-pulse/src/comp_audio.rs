//! Composition audio (lane SMB-AE-PARTICLES-AUDIO-PRESETS slice 2; STU-MOT-016 "The audio group
//! carries `audio_levels`, a keyframable stereo-pair level stream ... there is no separate audio
//! automation format ([STU-FX-135])").
//!
//! **Layers.** A [`CompAudioLayer`] is an audio-bearing composition layer: planar source audio at the
//! mix rate (1 or 2 channels; source sample 0 sits at the layer's `start`, which may be negative,
//! the composition tick of the layer's source start), an audible span `[in_point, out_point)` in
//! composition ticks, the layer's audio switch and solo, and its `audio_levels` keyframes
//! ([`LevelKey`]: a stereo pair in dB, linear in dB to the next key or held). Composition ticks map
//! to samples by floor (`samples_floor`, so 29.97 fps key times land on the sample at or before
//! them); source start must sit on the sample grid.
//!
//! **Mix through score.** Every audible layer (audio switch on; when any audible layer is soloed,
//! only soloed layers) becomes TWO mono score tracks: the left side (source channel 0) panned hard
//! left and the right side (channel 1, or channel 0 of a mono source) panned hard right. Score's
//! equal-power law is exactly `(1, 0)` / `(0, 1)` at the ends, so each side's `volume_db` lane IS
//! that side's level stream: the stereo pair is independent per side, sample accurate, linear in
//! dB between keys (a held key is a breakpoint pair one sample before the next key). Before the
//! first key the first value holds, after the last the last (score's lane rule). A layer with no
//! keys is 0 dB. One score `Session` holds at most [`MAX_COMP_AUDIO_LAYERS`] layers (score
//! `MAX_TRACKS` = 64 = 2 per layer); more is `limit_exceeded` before any sample is copied.
//!
//! **Preview (PR-19 clock).** [`CompAudioStream`] renders the mix block by block and is an
//! [`AudioClock`] (position = next composition sample), so pulse's [`PlaybackScheduler`] clocks the
//! composition's frames off the audio master ([`composition_scheduler`]). `seek` rebuilds the score
//! session with its origin at the new composition sample (sources cut at the origin, level lanes
//! re-anchored at their value there), so a scrub costs one rebuild, never a replay from 0.
//!
//! **Convert audio to keyframes.** [`audio_amplitude`] renders the composition mix once and reports,
//! per composition frame `f` (samples `[floor(f tpf / tps), floor((f + 1) tpf / tps))`), the RMS of
//! each side times 100 and their mean ("both channels"): the per-frame series that motion turns into
//! the expression-readable `audio_amplitude` slider keyframes (`hsk_studio_motion::audio_amplitude`).
//! The scale (RMS x 100) is a Studio rule; After Effects' Convert Audio to Keyframes scale, its
//! level interpolation and its mono-layer pan are UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use crate::error::{PulseError, Result};
use crate::playback::{AudioClock, DropPolicy, PlaybackScheduler};
use crate::time::{samples_floor, samples_from_ticks_exact};
use hsk_studio_accord::{CancellationToken, TICKS_PER_SECOND, Ticks};
use hsk_studio_folio::{SequenceFrameRate, SequenceSchema, StudioSequence};
use hsk_studio_score::{AutomationLane, Interpolation, ScoreError, Session, TRACK_VOLUME_DB, TrackSpec};
use std::sync::Arc;

/// Most audible layers of one composition mix (two score tracks each).
pub const MAX_COMP_AUDIO_LAYERS: usize = 32;
/// Most level keys of one layer (score lanes hold 4096 points; a held key may take two).
pub const MAX_LEVEL_KEYS: usize = 2048;
/// Level range in dB (score's fader range).
pub const LEVELS_DB: (f64, f64) = TRACK_VOLUME_DB;
/// Most frames one [`audio_amplitude`] call reports.
pub const MAX_AMPLITUDE_FRAMES: u64 = 1 << 20;

/// One `audio_levels` keyframe: a stereo pair in dB at a composition tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LevelKey {
    pub tick: Ticks,
    /// `[left, right]` dB within [`LEVELS_DB`].
    pub db: [f64; 2],
    /// Hold to the next key instead of linear.
    pub hold: bool,
}

/// One audio-bearing composition layer (module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct CompAudioLayer {
    pub layer_id: String,
    /// Planar source audio at the mix rate, 1 or 2 equal-length channels.
    pub source: Arc<Vec<Vec<f32>>>,
    /// Composition tick of source sample 0 (signed: a layer may start before 0).
    pub start: i64,
    pub in_point: Ticks,
    pub out_point: Ticks,
    pub levels: Vec<LevelKey>,
    /// The layer's audio switch.
    pub audio_enabled: bool,
    pub solo: bool,
}

impl CompAudioLayer {
    /// A layer at unity level playing its whole source from `start_tick` (`start_tick >= 0`).
    pub fn new(layer_id: &str, source: Vec<Vec<f32>>, start_tick: u64, rate_hz: u32) -> Result<Self> {
        let frames = source.first().map_or(0, Vec::len) as u64;
        let len = crate::time::ticks_from_samples(frames, rate_hz)?;
        let start = i64::try_from(start_tick).map_err(|_| PulseError::Overflow)?;
        Ok(Self {
            layer_id: layer_id.to_owned(),
            source: Arc::new(source),
            start,
            in_point: Ticks::new(start_tick),
            out_point: Ticks::new(start_tick.checked_add(len.value()).ok_or(PulseError::Overflow)?),
            levels: Vec::new(),
            audio_enabled: true,
            solo: false,
        })
    }
}

/// A layer resolved to composition samples.
#[derive(Clone, Debug)]
struct Prepared {
    source: Arc<Vec<Vec<f32>>>,
    /// Composition sample of source sample 0.
    start: i128,
    /// Audible composition samples `[a, b)` (already cut to the source).
    a: u64,
    b: u64,
    /// Per side: strictly increasing `(composition sample, dB)` breakpoints (linear lane).
    lanes: [Vec<(u64, f64)>; 2],
}

fn score_error(e: ScoreError) -> PulseError {
    match e {
        ScoreError::Canceled => PulseError::Canceled,
        ScoreError::CapacityExceeded => PulseError::LimitExceeded,
        ScoreError::InvalidRate => PulseError::UnsupportedSampleRate,
        _ => PulseError::AudioRejected,
    }
}

/// Linear value of `lane` at `t` with constant ends (score's `AutomationLane::value_at` rule).
fn lane_value(lane: &[(u64, f64)], t: u64) -> f64 {
    let Some(&(t0, v0)) = lane.first() else { return 0.0 };
    if t <= t0 {
        return v0;
    }
    let i = lane.partition_point(|(k, _)| *k <= t);
    if i >= lane.len() {
        return lane[lane.len().saturating_sub(1)].1;
    }
    let (ta, va) = lane[i.saturating_sub(1)];
    let (tb, vb) = lane[i];
    va + (vb - va) * (t.abs_diff(ta) as f64) / (tb.abs_diff(ta) as f64)
}

fn prepare(layer: &CompAudioLayer, rate: u32) -> Result<Option<Prepared>> {
    let ch = layer.source.len();
    if !(1..=2).contains(&ch) {
        return Err(PulseError::AudioUnsupported);
    }
    let frames = layer.source[0].len();
    if frames == 0 || layer.source.iter().any(|c| c.len() != frames) {
        return Err(PulseError::InvalidInput);
    }
    if layer.source.iter().flatten().any(|v| !v.is_finite()) {
        return Err(PulseError::InvalidInput);
    }
    if layer.levels.len() > MAX_LEVEL_KEYS {
        return Err(PulseError::LimitExceeded);
    }
    let per = i128::from(crate::time::ticks_from_samples(1, rate)?.value());
    let start_ticks = i128::from(layer.start);
    if start_ticks.checked_rem(per) != Some(0) {
        return Err(PulseError::NotFrameAligned);
    }
    let start = start_ticks.checked_div(per).ok_or(PulseError::Overflow)?;
    let (i, o) = (samples_floor(layer.in_point, rate)?, samples_floor(layer.out_point, rate)?);
    if o < i {
        return Err(PulseError::InvalidRange);
    }
    let src_end = start.checked_add(frames as i128).ok_or(PulseError::Overflow)?;
    let a = i128::from(i).max(start).max(0);
    let b = i128::from(o).min(src_end);
    // Level lanes (validated even for a silent layer: hostile keys are refused, never ignored).
    let mut lanes: [Vec<(u64, f64)>; 2] = [Vec::new(), Vec::new()];
    let mut prev: Option<u64> = None;
    for (k, key) in layer.levels.iter().enumerate() {
        for v in key.db {
            if !v.is_finite() || !(LEVELS_DB.0..=LEVELS_DB.1).contains(&v) {
                return Err(PulseError::InvalidInput);
            }
        }
        let s = samples_floor(key.tick, rate)?;
        if prev.is_some_and(|p| s <= p) {
            return Err(PulseError::UnorderedRanges);
        }
        prev = Some(s);
        for (lane, v) in lanes.iter_mut().zip(key.db) {
            lane.push((s, v));
        }
        if key.hold
            && let Some(next) = layer.levels.get(k.saturating_add(1))
            && let Some(before) = samples_floor(next.tick, rate)?.checked_sub(1).filter(|b| *b > s)
        {
            for (lane, v) in lanes.iter_mut().zip(key.db) {
                lane.push((before, v));
            }
        }
    }
    if b <= a || !layer.audio_enabled {
        return Ok(None);
    }
    Ok(Some(Prepared {
        source: layer.source.clone(),
        start,
        a: u64::try_from(a).map_err(|_| PulseError::Overflow)?,
        b: u64::try_from(b).map_err(|_| PulseError::Overflow)?,
        lanes,
    }))
}

/// The score tracks of the prepared layers with the session origin at composition sample `origin`.
fn tracks_at(layers: &[Prepared], origin: u64) -> Result<Vec<TrackSpec>> {
    let mut out = Vec::with_capacity(layers.len().saturating_mul(2));
    for l in layers {
        let a = l.a.max(origin);
        if a >= l.b {
            continue;
        }
        let frames = usize::try_from(l.b.abs_diff(a)).map_err(|_| PulseError::Overflow)?;
        let source_in = usize::try_from(i128::from(a).checked_sub(l.start).ok_or(PulseError::Overflow)?)
            .map_err(|_| PulseError::Overflow)?;
        let at = a.abs_diff(origin);
        for side in 0..2usize {
            let ch = side.min(l.source.len().saturating_sub(1));
            let mut t = TrackSpec::from_source(std::slice::from_ref(&l.source[ch]), source_in, frames, at)
                .map_err(score_error)?;
            t.pan = AutomationLane::constant(if side == 0 { -1.0 } else { 1.0 });
            let lane = &l.lanes[side];
            if !lane.is_empty() {
                let mut points = vec![(0u64, lane_value(lane, origin))];
                points.extend(lane.iter().filter(|(k, _)| *k > origin).map(|(k, v)| (k.abs_diff(origin), *v)));
                t.volume_db = AutomationLane::new(&points, Interpolation::Linear, LEVELS_DB).map_err(score_error)?;
            }
            out.push(t);
        }
    }
    Ok(out)
}

/// The composition mix as a block renderer and audio master clock (module docs).
pub struct CompAudioStream {
    rate: u32,
    max_block: usize,
    layers: Vec<Prepared>,
    session: Option<Session>,
    origin: u64,
    position: u64,
    length: u64,
}

impl CompAudioStream {
    /// Validates every layer (sources, spans, level keys) and builds the mix at sample 0.
    /// Refusals: a source with 0 or more than 2 channels (`audio_unsupported`); empty, ragged or
    /// non-finite source, level outside [`LEVELS_DB`] or non-finite (`invalid_input`); key times
    /// not strictly increasing in samples (`unordered_ranges`); a start off the sample grid
    /// (`not_frame_aligned`); out before in (`invalid_range`); more than [`MAX_LEVEL_KEYS`] keys or
    /// [`MAX_COMP_AUDIO_LAYERS`] audible layers (`limit_exceeded`); score refusals (rate, block).
    pub fn new(layers: &[CompAudioLayer], rate_hz: u32, max_block_frames: usize, cancel: &CancellationToken) -> Result<Self> {
        cancel.check().map_err(|_| PulseError::Canceled)?;
        let mut prepared = Vec::new();
        let any_solo = layers.iter().any(|l| l.solo && l.audio_enabled);
        for l in layers {
            if let Some(p) = prepare(l, rate_hz)?
                && (!any_solo || l.solo)
            {
                prepared.push(p);
            }
        }
        if prepared.len() > MAX_COMP_AUDIO_LAYERS {
            return Err(PulseError::LimitExceeded);
        }
        let length = prepared.iter().map(|p| p.b).max().unwrap_or(0);
        let mut s = Self { rate: rate_hz, max_block: max_block_frames, layers: prepared, session: None, origin: 0, position: 0, length };
        s.rebuild(0)?;
        Ok(s)
    }

    fn rebuild(&mut self, origin: u64) -> Result<()> {
        let tracks = tracks_at(&self.layers, origin)?;
        self.session = if tracks.is_empty() {
            crate::time::ticks_from_samples(1, self.rate)?;
            if self.max_block == 0 {
                return Err(PulseError::InvalidInput);
            }
            None
        } else {
            Some(Session::new(self.rate, self.max_block, tracks).map_err(score_error)?)
        };
        self.origin = origin;
        self.position = origin;
        Ok(())
    }

    /// Moves the stream to composition sample `sample` (one session rebuild).
    pub fn seek(&mut self, sample: u64) -> Result<()> {
        self.rebuild(sample)
    }

    /// Renders the next `left.len()` composition samples (the clock advances).
    pub fn render(&mut self, left: &mut [f32], right: &mut [f32], cancel: &CancellationToken) -> Result<()> {
        cancel.check().map_err(|_| PulseError::Canceled)?;
        if left.len() != right.len() || left.is_empty() || left.len() > self.max_block {
            return Err(PulseError::InvalidInput);
        }
        let n = left.len() as u64;
        let next = self.position.checked_add(n).ok_or(PulseError::Overflow)?;
        match &mut self.session {
            Some(s) => s.render(self.position.abs_diff(self.origin), left, right, cancel).map_err(score_error)?,
            None => {
                left.fill(0.0);
                right.fill(0.0);
            }
        }
        self.position = next;
        Ok(())
    }

    /// Next composition sample `render` produces.
    pub fn position(&self) -> u64 {
        self.position
    }
    /// End of the last audible layer in composition samples.
    pub fn length_samples(&self) -> u64 {
        self.length
    }
    /// Audible layers in the mix.
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }
}

impl AudioClock for CompAudioStream {
    fn sample_rate_hz(&self) -> u32 {
        self.rate
    }
    fn sample_position(&self) -> u64 {
        self.position
    }
}

/// A playback scheduler over a composition's frame grid `[0, duration)` (an in-memory sequence
/// projection with no tracks; it is never written to Folio). Clock it with a [`CompAudioStream`].
pub fn composition_scheduler(composition_id: &str, ticks_per_frame: u64, duration: Ticks, policy: DropPolicy) -> Result<PlaybackScheduler> {
    let seq = StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: composition_id.to_owned(),
        name: composition_id.to_owned(),
        frame_rate: SequenceFrameRate { ticks_per_frame },
        duration,
        tracks: Vec::new(),
        markers: Vec::new(),
        work_area: None,
    };
    PlaybackScheduler::new(&seq, policy)
}

/// Per-frame amplitude of the composition mix (module docs): `left`, `right` = RMS x 100 of each
/// side over the frame's samples, `both` = their mean; frame `f` starts at tick `f * ticks_per_frame`.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioAmplitude {
    pub ticks_per_frame: u64,
    pub left: Vec<f64>,
    pub right: Vec<f64>,
    pub both: Vec<f64>,
}

/// Renders frames `0..frames` of the mix from sample 0 (the stream is left at the end) and measures
/// them. Refusals: 0 or more than [`MAX_AMPLITUDE_FRAMES`] frames, `ticks_per_frame` 0 or shorter
/// than one sample (`invalid_input` / `limit_exceeded`).
pub fn audio_amplitude(stream: &mut CompAudioStream, ticks_per_frame: u64, frames: u64, cancel: &CancellationToken) -> Result<AudioAmplitude> {
    if frames == 0 || ticks_per_frame == 0 {
        return Err(PulseError::InvalidInput);
    }
    if frames > MAX_AMPLITUDE_FRAMES {
        return Err(PulseError::LimitExceeded);
    }
    let rate = stream.rate;
    let edge = |f: u64| -> Result<u64> {
        let t = u128::from(f).checked_mul(u128::from(ticks_per_frame)).ok_or(PulseError::Overflow)?;
        let s = t.checked_mul(u128::from(rate)).ok_or(PulseError::Overflow)?.checked_div(u128::from(TICKS_PER_SECOND)).ok_or(PulseError::Overflow)?;
        u64::try_from(s).map_err(|_| PulseError::Overflow)
    };
    if edge(1)? == 0 {
        return Err(PulseError::InvalidInput);
    }
    stream.seek(0)?;
    let block = stream.max_block;
    let (mut l, mut r) = (vec![0.0f32; block], vec![0.0f32; block]);
    let cap = usize::try_from(frames).map_err(|_| PulseError::Overflow)?;
    let mut out = AudioAmplitude { ticks_per_frame, left: Vec::with_capacity(cap), right: Vec::with_capacity(cap), both: Vec::with_capacity(cap) };
    for f in 0..frames {
        let (s0, s1) = (edge(f)?, edge(f.checked_add(1).ok_or(PulseError::Overflow)?)?);
        let n = s1.abs_diff(s0);
        let (mut sl, mut sr) = (0.0f64, 0.0f64);
        let mut left_n = n;
        while left_n > 0 {
            let k = usize::try_from(left_n.min(block as u64)).map_err(|_| PulseError::Overflow)?;
            stream.render(&mut l[..k], &mut r[..k], cancel)?;
            sl += l[..k].iter().map(|v| f64::from(*v) * f64::from(*v)).sum::<f64>();
            sr += r[..k].iter().map(|v| f64::from(*v) * f64::from(*v)).sum::<f64>();
            left_n = left_n.saturating_sub(k as u64);
        }
        let (al, ar) = (100.0 * (sl / n as f64).sqrt(), 100.0 * (sr / n as f64).sqrt());
        out.left.push(al);
        out.right.push(ar);
        out.both.push(0.5 * (al + ar));
    }
    Ok(out)
}

/// Exact sample of a composition tick (for callers placing sources on the grid).
pub fn composition_sample(tick: Ticks, rate_hz: u32) -> Result<u64> {
    samples_from_ticks_exact(tick, rate_hz)
}
