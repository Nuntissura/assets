//! Multitrack mixdown with sample-accurate automation (STU-FX-135 effect stacks, STU-FX-137
//! tracks and buses).
//!
//! A [`Session`] owns mono or stereo clips placed at sample positions on one timeline, each with an
//! ordered effect stack ([`EffectSlot`]s of [`Automatable`] processors), a volume lane (dB) and a
//! pan lane, and renders a stereo mix block by block in timeline order.
//!
//! **Automation lanes** ([`AutomationLane`]) hold strictly increasing breakpoints `(sample,
//! value)`. Before the first breakpoint the value is the first value, after the last the last
//! value. Between breakpoints `Step` holds the earlier value and `Linear` interpolates
//! `v0 + (v1 - v0) (t - t0) / (t1 - t0)` in `f64` (exact at breakpoints). Every lane is evaluated
//! at every sample: volume and pan per sample at the output timeline position; an effect
//! parameter is set before each sample at which its value changes (inside a linear ramp every
//! sample, otherwise only at breakpoints), evaluated at the timeline position of the audio that
//! stage is reading, and the stage runs on the sub-blocks between those changes, so automation is
//! sample-accurate.
//!
//! **Latency compensation.** A track's latency `L` is the sum of its stages' latencies. Clips are
//! in memory, so the stack is fed `L` samples ahead: the first render primes it with timeline
//! samples `0..L` (outputs discarded) and output frame `t` then carries the stack's result for
//! timeline sample `t`, aligned sample-exact with every other track; the session itself reports
//! no latency. Stage `k` reads the audio of timeline time `n - (latency of stages before k)` when
//! the stack is fed timeline sample `n`; its lanes are evaluated there (clamped at 0 during the
//! priming pre-roll).
//!
//! **Per track and sample `t`:** `x` = clip sample at `t - start` times the clip fades ([`Fade`],
//! applied before the stack; 0 outside the clip, so effect tails continue), `y` = stack(x) aligned
//! to `t`, gain `g = 10^(volume_db(t)/20)`. Mono:
//! equal-power pan `(L, R) = g y (cos a, sin a)`, `a = (pan + 1) pi/4` (the [`Pan`](crate::Pan)
//! law). Stereo: linear balance `L = g y_L min(1, 1 - pan)`, `R = g y_R min(1, 1 + pan)` (centre =
//! unity). The bus is the sum over tracks in track order, accumulated in `f64` and rounded once.
//!
//! All buffers are allocated in [`Session::new`]; `render` allocates nothing. Blocks must be
//! rendered contiguously from sample 0; a render canceled during its work or with a non-finite
//! result resets the session to sample 0 (nothing partial is kept).
use crate::block::{InputBlock, OutputBlock};
use crate::dsp::{db_to_linear, poll};
use crate::engine::Processor;
use crate::error::ScoreError;
use crate::layout::{BlockSpec, ChannelLayout, MAX_BLOCK_FRAMES, ticks_per_sample};
use hsk_studio_accord::CancellationToken;
use std::f64::consts::FRAC_PI_4;

/// Widest track list (authored bound).
pub const MAX_TRACKS: usize = 64;
/// Most breakpoints per lane (authored bound).
pub const MAX_AUTOMATION_POINTS: usize = 4096;
/// Volume lane range in dB (authored).
pub const TRACK_VOLUME_DB: (f64, f64) = (-96.0, 12.0);
/// Pan lane range.
pub const TRACK_PAN: (f64, f64) = (-1.0, 1.0);
/// Longest clip in frames (authored bound: 2^31).
pub const MAX_CLIP_FRAMES: usize = 1 << 31;
/// Most effect stages per track (authored bound).
pub const MAX_TRACK_EFFECTS: usize = 16;
/// Largest compensated track latency in samples (authored bound: 2^24).
pub const MAX_TRACK_LATENCY: u64 = 1 << 24;

/// One automatable parameter of a processor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AutomationParam {
    pub name: &'static str,
    pub min: f64,
    pub max: f64,
}

/// A processor whose parameters can change between samples without resetting its state.
pub trait Automatable: Processor {
    /// Parameters in index order (ranges may depend on the construction, e.g. the rate). Called
    /// outside the audio path (it may allocate).
    fn automation_parameters(&self) -> Vec<AutomationParam>;
    /// Sets parameter `index` (refuses NaN/inf with `NotFinite`, out of range or an unknown index
    /// with `OutOfRange`). Allocation-free.
    fn set_automated(&mut self, index: usize, value: f64) -> Result<(), ScoreError>;
}

pub(crate) fn check_automated(params: &[AutomationParam], index: usize, value: f64) -> Result<(), ScoreError> {
    let p = params.get(index).ok_or(ScoreError::OutOfRange)?;
    if !value.is_finite() {
        return Err(ScoreError::NotFinite);
    }
    if !(p.min..=p.max).contains(&value) {
        return Err(ScoreError::OutOfRange);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interpolation {
    Step,
    Linear,
}

/// Breakpoint automation; see the module docs for the evaluation rule.
#[derive(Clone, Debug, PartialEq)]
pub struct AutomationLane {
    points: Vec<(u64, f64)>,
    interpolation: Interpolation,
}

impl AutomationLane {
    /// Refusals: no points or more than [`MAX_AUTOMATION_POINTS`] -> `CapacityExceeded`
    /// (empty: `OutOfRange`); positions not strictly increasing or a value outside `range` ->
    /// `OutOfRange`; NaN/inf -> `NotFinite`.
    pub fn new(points: &[(u64, f64)], interpolation: Interpolation, range: (f64, f64)) -> Result<Self, ScoreError> {
        if points.is_empty() {
            return Err(ScoreError::OutOfRange);
        }
        if points.len() > MAX_AUTOMATION_POINTS {
            return Err(ScoreError::CapacityExceeded);
        }
        if points.iter().any(|(_, v)| !v.is_finite()) {
            return Err(ScoreError::NotFinite);
        }
        if points.iter().any(|(_, v)| *v < range.0 || *v > range.1) || points.windows(2).any(|w| w[0].0 >= w[1].0) {
            return Err(ScoreError::OutOfRange);
        }
        Ok(Self { points: points.to_vec(), interpolation })
    }
    /// A lane holding `value` everywhere (no range check; the session checks it).
    pub fn constant(value: f64) -> Self {
        Self { points: vec![(0, value)], interpolation: Interpolation::Step }
    }
    pub fn points(&self) -> &[(u64, f64)] {
        &self.points
    }
    /// Index of the last breakpoint at or before `t`, if any.
    fn segment(&self, t: u64) -> Option<usize> {
        self.points.partition_point(|(p, _)| *p <= t).checked_sub(1)
    }
    pub fn value_at(&self, t: u64) -> f64 {
        match self.segment(t) {
            None => self.points[0].1,
            Some(i) if i + 1 == self.points.len() => self.points[i].1,
            Some(i) => {
                let ((t0, v0), (t1, v1)) = (self.points[i], self.points[i + 1]);
                match self.interpolation {
                    Interpolation::Step => v0,
                    Interpolation::Linear => v0 + (v1 - v0) * ((t - t0) as f64 / (t1 - t0) as f64),
                }
            }
        }
    }
    /// First sample after `t` at which the value may differ from the value at `t`.
    fn next_change(&self, t: u64) -> u64 {
        match self.segment(t) {
            None => self.points[0].0,
            Some(i) if i + 1 == self.points.len() => u64::MAX,
            Some(i) => {
                let ((_, v0), (t1, v1)) = (self.points[i], self.points[i + 1]);
                if self.interpolation == Interpolation::Linear && v0 != v1 { t + 1 } else { t1 }
            }
        }
    }
    fn within(&self, range: (f64, f64)) -> Result<(), ScoreError> {
        if self.points.iter().any(|(_, v)| !v.is_finite()) {
            return Err(ScoreError::NotFinite);
        }
        if self.points.iter().any(|(_, v)| *v < range.0 || *v > range.1) {
            return Err(ScoreError::OutOfRange);
        }
        Ok(())
    }
}

/// One effect stage of a track: a processor and its `(parameter index, lane)` automation.
pub struct EffectSlot {
    pub processor: Box<dyn Automatable + Send>,
    pub lanes: Vec<(usize, AutomationLane)>,
}

impl EffectSlot {
    pub fn new(processor: Box<dyn Automatable + Send>) -> Self {
        Self { processor, lanes: Vec::new() }
    }
    /// Adds an automation lane for parameter `index`.
    pub fn with_lane(mut self, index: usize, lane: AutomationLane) -> Self {
        self.lanes.push((index, lane));
        self
    }
}

/// One track as given to [`Session::new`]. Build it with [`TrackSpec::new`] and set the public
/// fields (the struct is non-exhaustive so later fields are not breaking).
#[non_exhaustive]
pub struct TrackSpec {
    /// Planar clip, 1 or 2 channels of equal length.
    pub clip: Vec<Vec<f32>>,
    pub start_sample: u64,
    pub volume_db: AutomationLane,
    pub pan: AutomationLane,
    /// Ordered effect stack (stage 0 first).
    pub effects: Vec<EffectSlot>,
    /// Clip fade in over its first frames (applied to the clip samples, before the stack).
    pub fade_in: Option<Fade>,
    /// Clip fade out over its last frames.
    pub fade_out: Option<Fade>,
}

impl TrackSpec {
    /// Unity volume (0 dB), centre pan, no effects, no fades.
    pub fn new(clip: Vec<Vec<f32>>, start_sample: u64) -> Self {
        Self {
            clip,
            start_sample,
            volume_db: AutomationLane::constant(0.0),
            pan: AutomationLane::constant(0.0),
            effects: Vec::new(),
            fade_in: None,
            fade_out: None,
        }
    }
    /// A clip cut from planar `source` audio: frames `source_in .. source_in + frames` placed at
    /// `start_sample` (a timeline clip with a source in point). Refusals: no channel or more than
    /// 2 -> `InvalidLayout`; unequal source channels -> `LengthMismatch`; an empty window or one
    /// past the source end -> `OutOfRange`.
    pub fn from_source(source: &[Vec<f32>], source_in: usize, frames: usize, start_sample: u64) -> Result<Self, ScoreError> {
        if !(1..=2).contains(&source.len()) {
            return Err(ScoreError::InvalidLayout);
        }
        let len = source[0].len();
        if source.iter().any(|c| c.len() != len) {
            return Err(ScoreError::LengthMismatch);
        }
        let end = source_in.checked_add(frames).ok_or(ScoreError::OutOfRange)?;
        if frames == 0 || end > len {
            return Err(ScoreError::OutOfRange);
        }
        Ok(Self::new(source.iter().map(|c| c[source_in..end].to_vec()).collect(), start_sample))
    }
    /// A clip cut from `source` frames `source_in .. source_in + source_frames` and played at
    /// `speed` (band-limited windowed-sinc, see [`render_at_speed`](crate::render_at_speed); source
    /// samples outside the window serve as handles). The clip lasts
    /// `speed.output_frames(source_frames)` frames on the timeline. Refusals as
    /// [`TrackSpec::from_source`], plus `NotFinite` for a non-finite source sample and
    /// `CapacityExceeded` for a result longer than [`MAX_CLIP_FRAMES`].
    pub fn from_source_at_speed(
        source: &[Vec<f32>],
        source_in: usize,
        source_frames: usize,
        speed: crate::varispeed::ClipSpeed,
        start_sample: u64,
    ) -> Result<Self, ScoreError> {
        Self::from_source(&source[..source.len().min(2)], source_in, source_frames, start_sample)?;
        if source.len() > 2 {
            return Err(ScoreError::InvalidLayout);
        }
        let frames = speed.output_frames(source_frames as u64);
        if frames > MAX_CLIP_FRAMES as u64 {
            return Err(ScoreError::CapacityExceeded);
        }
        let mut clip = Vec::with_capacity(source.len());
        for channel in source {
            let mut out = vec![0.0_f32; frames as usize];
            crate::varispeed::render_at_speed(channel, source_in, source_frames, speed, &mut out)?;
            clip.push(out);
        }
        Ok(Self::new(clip, start_sample))
    }
}

/// Fade shape. With progress `p = k / frames` at the `k`-th frame of the fade: `Linear` gains
/// `p` (in) and `1 - p` (out), so an abutting out/in pair over the same span is the linear
/// crossfade `(1 - p) a + p b`; `EqualPower` gains `sin(p pi/2)` (in) and `cos(p pi/2)` (out),
/// whose squares sum to 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FadeCurve {
    Linear,
    EqualPower,
}

/// A clip fade of `frames` samples (1 ..= clip length).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fade {
    pub frames: u64,
    pub curve: FadeCurve,
}

impl Fade {
    /// Gain of a fade in at progress `k / frames`.
    pub fn in_gain(&self, k: u64) -> f64 {
        let p = k as f64 / self.frames as f64;
        match self.curve {
            FadeCurve::Linear => p,
            FadeCurve::EqualPower => (p * std::f64::consts::FRAC_PI_2).sin(),
        }
    }
    /// Gain of a fade out at progress `k / frames`.
    pub fn out_gain(&self, k: u64) -> f64 {
        let p = k as f64 / self.frames as f64;
        match self.curve {
            FadeCurve::Linear => 1.0 - p,
            FadeCurve::EqualPower => (p * std::f64::consts::FRAC_PI_2).cos(),
        }
    }
}

struct Stage {
    slot: EffectSlot,
    /// Latency of the stages before this one.
    offset: u64,
    block: BlockSpec,
    out: OutputBlock,
    last: Vec<f64>,
}

struct Track {
    clip: Vec<Vec<f32>>,
    start: u64,
    end: u64,
    volume_db: AutomationLane,
    pan: AutomationLane,
    stages: Vec<Stage>,
    latency: u64,
    input: Vec<Vec<f32>>,
    wet: Vec<Vec<f32>>,
    primed: bool,
    fade_in: Option<Fade>,
    fade_out: Option<Fade>,
}

impl Track {
    /// Clip sample of channel `c` at timeline `t` with the fades applied (0 outside the clip).
    #[inline]
    fn clip_sample(&self, c: usize, t: u64) -> f32 {
        if t < self.start || t >= self.end {
            return 0.0;
        }
        let x = self.clip[c][(t - self.start) as usize];
        let mut g = 1.0_f64;
        if let Some(f) = self.fade_in {
            let k = t - self.start;
            if k < f.frames {
                g *= f.in_gain(k);
            }
        }
        if let Some(f) = self.fade_out {
            let from = self.end - f.frames;
            if t >= from {
                g *= f.out_gain(t - from);
            }
        }
        if g == 1.0 { x } else { (f64::from(x) * g) as f32 }
    }
    /// Feeds timeline samples `[feed, feed + len)` (`len <= max_block_frames`) through the stack;
    /// the last stage's output goes to `wet[..len]` when `keep`.
    fn run_stack(&mut self, feed: u64, len: usize, keep: bool, cancel: &CancellationToken) -> Result<(), ScoreError> {
        let channels = self.input.len();
        let end = feed + len as u64;
        let mut at = 0;
        while at < len {
            poll(cancel)?;
            let n = feed + at as u64;
            let mut stop = end;
            for stage in &mut self.stages {
                let u = n.saturating_sub(stage.offset);
                for (slot, (index, lane)) in stage.slot.lanes.iter().enumerate() {
                    let v = lane.value_at(u);
                    if v.to_bits() != stage.last[slot].to_bits() {
                        stage.slot.processor.set_automated(*index, v)?;
                        stage.last[slot] = v;
                    }
                    stop = stop.min(stage.offset.saturating_add(lane.next_change(u)));
                }
            }
            let sub = (stop - n) as usize;
            for c in 0..channels {
                for k in 0..sub {
                    let v = self.clip_sample(c, n + k as u64);
                    self.input[c][k] = v;
                }
            }
            for k in 0..self.stages.len() {
                let (before, rest) = self.stages.split_at_mut(k);
                let stage = &mut rest[0];
                stage.block.frames = sub;
                stage.block.start_sample = n.saturating_sub(stage.offset);
                let slices: [&[f32]; 2] = match before.last() {
                    None => [&self.input[0][..sub], &self.input[channels - 1][..sub]],
                    Some(prev) => {
                        let cap = prev.out.capacity();
                        let b = prev.out.backing();
                        [&b[..sub], &b[(channels - 1) * cap..(channels - 1) * cap + sub]]
                    }
                };
                let input = InputBlock::new(&stage.block, &slices[..channels]);
                stage.slot.processor.process(&input, &mut stage.out, cancel)?;
            }
            if keep {
                match self.stages.last() {
                    Some(last) => {
                        let cap = last.out.capacity();
                        for (c, w) in self.wet.iter_mut().enumerate() {
                            w[at..at + sub].copy_from_slice(&last.out.backing()[c * cap..c * cap + sub]);
                        }
                    }
                    None => {
                        for (w, i) in self.wet.iter_mut().zip(&self.input) {
                            w[at..at + sub].copy_from_slice(&i[..sub]);
                        }
                    }
                }
            }
            at += sub;
        }
        Ok(())
    }
    fn reset(&mut self) {
        for stage in &mut self.stages {
            stage.slot.processor.reset();
            stage.last.fill(f64::NAN);
        }
        self.primed = self.latency == 0;
    }
}

/// Multitrack renderer; see the module docs.
pub struct Session {
    sample_rate_hz: u32,
    max_block_frames: usize,
    tracks: Vec<Track>,
    position: u64,
    length: u64,
    bus: [Vec<f64>; 2],
}

impl Session {
    /// Refusals (all before any rendering): rate not admitted -> `InvalidRate`; no tracks,
    /// `max_block_frames` 0 -> `OutOfRange`; more than [`MAX_TRACKS`] tracks, more than
    /// [`MAX_TRACK_EFFECTS`] stages, a stack latency above [`MAX_TRACK_LATENCY`] or a block above
    /// `MAX_BLOCK_FRAMES` -> `CapacityExceeded`; a clip with 0 or more than 2 channels ->
    /// `InvalidLayout`; unequal channel lengths -> `LengthMismatch`; empty or too long clip ->
    /// `CapacityExceeded`; end position overflow -> `PositionOverflow`; NaN/inf sample or lane
    /// value -> `NotFinite`; lane value outside its range, an unknown effect parameter or a stage
    /// that cannot take `max_block_frames` -> `OutOfRange`; a stage bound to another rate ->
    /// `InvalidRate`; a stage not mapping the clip layout to itself -> `ChannelMismatch`.
    pub fn new(sample_rate_hz: u32, max_block_frames: usize, tracks: Vec<TrackSpec>) -> Result<Self, ScoreError> {
        ticks_per_sample(sample_rate_hz)?;
        if tracks.is_empty() || max_block_frames == 0 {
            return Err(ScoreError::OutOfRange);
        }
        if tracks.len() > MAX_TRACKS || max_block_frames > MAX_BLOCK_FRAMES {
            return Err(ScoreError::CapacityExceeded);
        }
        let mut built = Vec::with_capacity(tracks.len());
        let mut length = 0u64;
        for spec in tracks {
            let channels = spec.clip.len();
            if !(1..=2).contains(&channels) {
                return Err(ScoreError::InvalidLayout);
            }
            let frames = spec.clip[0].len();
            if spec.clip.iter().any(|c| c.len() != frames) {
                return Err(ScoreError::LengthMismatch);
            }
            if frames == 0 || frames > MAX_CLIP_FRAMES || spec.effects.len() > MAX_TRACK_EFFECTS {
                return Err(ScoreError::CapacityExceeded);
            }
            if spec.clip.iter().any(|c| c.iter().any(|v| !v.is_finite())) {
                return Err(ScoreError::NotFinite);
            }
            let end = spec.start_sample.checked_add(frames as u64).ok_or(ScoreError::PositionOverflow)?;
            spec.volume_db.within(TRACK_VOLUME_DB)?;
            spec.pan.within(TRACK_PAN)?;
            if [spec.fade_in, spec.fade_out].iter().flatten().any(|f| f.frames == 0 || f.frames > frames as u64) {
                return Err(ScoreError::OutOfRange);
            }
            let layout = if channels == 1 { ChannelLayout::mono(1) } else { ChannelLayout::stereo() };
            let mut stages = Vec::with_capacity(spec.effects.len());
            let mut offset = 0u64;
            for slot in spec.effects {
                let p = &slot.processor;
                if p.required_sample_rate_hz().is_some_and(|r| r != sample_rate_hz) {
                    return Err(ScoreError::InvalidRate);
                }
                if !p.accepts(&layout, &layout) {
                    return Err(ScoreError::ChannelMismatch);
                }
                if p.max_frames().is_some_and(|m| m < max_block_frames) {
                    return Err(ScoreError::OutOfRange);
                }
                let params = p.automation_parameters();
                for (index, lane) in &slot.lanes {
                    let param = params.get(*index).ok_or(ScoreError::OutOfRange)?;
                    lane.within((param.min, param.max))?;
                }
                let latency = u64::from(p.latency_samples());
                stages.push(Stage {
                    last: vec![f64::NAN; slot.lanes.len()],
                    slot,
                    offset,
                    block: BlockSpec { sample_rate_hz, layout: layout.clone(), frames: 1, start_sample: 0 },
                    out: OutputBlock::with_capacity(layout.clone(), max_block_frames)?,
                });
                offset += latency;
                if offset > MAX_TRACK_LATENCY {
                    return Err(ScoreError::CapacityExceeded);
                }
            }
            length = length.max(end);
            built.push(Track {
                clip: spec.clip,
                start: spec.start_sample,
                end,
                volume_db: spec.volume_db,
                pan: spec.pan,
                stages,
                latency: offset,
                input: vec![vec![0.0; max_block_frames]; channels],
                wet: vec![vec![0.0; max_block_frames]; channels],
                primed: offset == 0,
                fade_in: spec.fade_in,
                fade_out: spec.fade_out,
            });
        }
        Ok(Self {
            sample_rate_hz,
            max_block_frames,
            tracks: built,
            position: 0,
            length,
            bus: [vec![0.0; max_block_frames], vec![0.0; max_block_frames]],
        })
    }
    pub fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }
    /// Last clip end on the timeline (effect tails may continue past it).
    pub fn length_samples(&self) -> u64 {
        self.length
    }
    /// Next sample `render` expects.
    pub fn position(&self) -> u64 {
        self.position
    }
    /// Compensated latency of track `index` (sum of its stage latencies), if it exists.
    pub fn track_latency(&self, index: usize) -> Option<u64> {
        self.tracks.get(index).map(|t| t.latency)
    }
    /// Returns to sample 0 and resets every effect.
    pub fn reset(&mut self) {
        self.position = 0;
        for t in &mut self.tracks {
            t.reset();
        }
    }

    /// Renders `left.len()` frames starting at `start_sample` into the stereo bus.
    /// `TimelineMismatch` unless `start_sample == position()`; `LengthMismatch` for unequal
    /// outputs; `OutOfRange` for 0 frames; `CapacityExceeded` above `max_block_frames`; all of these
    /// and a cancel observed before any work leave the session unchanged. A cancel during the work
    /// or a non-finite result resets the session to sample 0; outputs are then unspecified.
    pub fn render(&mut self, start_sample: u64, left: &mut [f32], right: &mut [f32], cancel: &CancellationToken) -> Result<(), ScoreError> {
        cancel.check().map_err(|_| ScoreError::Canceled)?;
        if start_sample != self.position {
            return Err(ScoreError::TimelineMismatch);
        }
        let frames = left.len();
        if right.len() != frames {
            return Err(ScoreError::LengthMismatch);
        }
        if frames == 0 {
            return Err(ScoreError::OutOfRange);
        }
        if frames > self.max_block_frames {
            return Err(ScoreError::CapacityExceeded);
        }
        start_sample.checked_add(frames as u64 + MAX_TRACK_LATENCY).ok_or(ScoreError::PositionOverflow)?;
        let result = self.render_inner(start_sample, frames, left, right, cancel);
        match result {
            Ok(()) => {
                self.position = start_sample + frames as u64;
                Ok(())
            }
            Err(e) => {
                self.reset();
                Err(e)
            }
        }
    }

    fn render_inner(&mut self, start: u64, frames: usize, left: &mut [f32], right: &mut [f32], cancel: &CancellationToken) -> Result<(), ScoreError> {
        self.bus[0][..frames].fill(0.0);
        self.bus[1][..frames].fill(0.0);
        let max_block = self.max_block_frames;
        for track in &mut self.tracks {
            poll(cancel)?;
            if !track.primed {
                // Pre-roll: feed timeline samples 0..L, discarding the outputs.
                let mut fed = 0u64;
                while fed < track.latency {
                    let len = (track.latency - fed).min(max_block as u64) as usize;
                    track.run_stack(fed, len, false, cancel)?;
                    fed += len as u64;
                }
                track.primed = true;
            }
            track.run_stack(start + track.latency, frames, true, cancel)?;
            let channels = track.wet.len();
            // Fader and pan, per sample.
            for k in 0..frames {
                let t = start + k as u64;
                let g = db_to_linear(track.volume_db.value_at(t));
                let pan = track.pan.value_at(t);
                let (l, r) = if channels == 1 {
                    let y = f64::from(track.wet[0][k]) * g;
                    let (gl, gr) = pan_gains(pan);
                    (y * gl, y * gr)
                } else {
                    (
                        f64::from(track.wet[0][k]) * g * (1.0 - pan).min(1.0),
                        f64::from(track.wet[1][k]) * g * (1.0 + pan).min(1.0),
                    )
                };
                self.bus[0][k] += l;
                self.bus[1][k] += r;
            }
        }
        for (dst, src) in left.iter_mut().zip(&self.bus[0][..frames]) {
            *dst = *src as f32;
        }
        for (dst, src) in right.iter_mut().zip(&self.bus[1][..frames]) {
            *dst = *src as f32;
        }
        if left.iter().chain(right.iter()).any(|v| !v.is_finite()) {
            return Err(ScoreError::NotFinite);
        }
        Ok(())
    }
}

/// Equal-power pan gains `(cos a, sin a)`, `a = (pan + 1) pi/4`; endpoints exact.
pub fn pan_gains(pan: f64) -> (f64, f64) {
    if pan <= -1.0 {
        (1.0, 0.0)
    } else if pan >= 1.0 {
        (0.0, 1.0)
    } else {
        let a = (pan + 1.0) * FRAC_PI_4;
        (a.cos(), a.sin())
    }
}
