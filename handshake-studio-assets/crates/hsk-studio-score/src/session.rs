//! Multitrack mixdown with sample-accurate automation (STU-FX-135 effect stacks, STU-FX-137
//! tracks and buses).
//!
//! A [`Session`] owns mono or stereo clips placed at sample positions on one timeline, each with an
//! optional effect ([`Automatable`] processor), a volume lane (dB) and a pan lane, and renders a
//! stereo mix block by block in timeline order.
//!
//! **Automation lanes** ([`AutomationLane`]) hold strictly increasing breakpoints `(sample,
//! value)`. Before the first breakpoint the value is the first value, after the last the last
//! value. Between breakpoints `Step` holds the earlier value and `Linear` interpolates
//! `v0 + (v1 - v0) (t - t0) / (t1 - t0)` in `f64` (exact at breakpoints). Every lane is evaluated
//! at every sample: volume and pan per sample; effect parameters are set before each sample at
//! which their value changes (inside a linear ramp every sample, otherwise only at breakpoints),
//! and the effect runs on the sub-blocks between those changes, so automation is sample-accurate.
//!
//! **Per track and sample `t`:** `x` = clip sample at `t - start` (0 outside the clip, so effect
//! tails continue), `y` = effect(x), gain `g = 10^(volume_db(t)/20)`. Mono: equal-power pan
//! `(L, R) = g y (cos a, sin a)`, `a = (pan + 1) pi/4` (the [`Pan`](crate::Pan) law). Stereo:
//! linear balance `L = g y_L min(1, 1 - pan)`, `R = g y_R min(1, 1 + pan)` (centre = unity). The
//! bus is the sum over tracks in track order, accumulated in `f64` and rounded once.
//!
//! Effects must have latency 0 (no plugin-delay compensation yet; a latent effect is refused with
//! `OutOfRange`). All buffers are allocated in [`Session::new`]; `render` allocates nothing.
//! Blocks must be rendered contiguously from sample 0; a render canceled during its work or with a
//! non-finite result resets the session to sample 0 (nothing partial is kept).
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

/// One automatable parameter of a processor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AutomationParam {
    pub name: &'static str,
    pub min: f64,
    pub max: f64,
}

/// A processor whose parameters can change between samples without resetting its state.
pub trait Automatable: Processor {
    fn automation_parameters(&self) -> &'static [AutomationParam];
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

/// One track as given to [`Session::new`].
pub struct TrackSpec {
    /// Planar clip, 1 or 2 channels of equal length.
    pub clip: Vec<Vec<f32>>,
    pub start_sample: u64,
    pub volume_db: AutomationLane,
    pub pan: AutomationLane,
    pub effect: Option<Box<dyn Automatable + Send>>,
    /// `(parameter index, lane)` pairs for the effect.
    pub effect_lanes: Vec<(usize, AutomationLane)>,
}

struct Track {
    spec: TrackSpec,
    end_sample: u64,
    block: BlockSpec,
    input: Vec<Vec<f32>>,
    wet: Vec<Vec<f32>>,
    out: OutputBlock,
    last_param: Vec<f64>,
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
    /// `max_block_frames` 0, or more than [`MAX_TRACKS`] / `MAX_BLOCK_FRAMES` -> `OutOfRange` /
    /// `CapacityExceeded`; a clip with 0 or more than 2 channels -> `InvalidLayout`; unequal
    /// channel lengths -> `LengthMismatch`; empty or too long clip, end position overflow ->
    /// `CapacityExceeded` / `PositionOverflow`; NaN/inf sample or lane value -> `NotFinite`; lane
    /// value out of range, unknown effect parameter or an effect with latency -> `OutOfRange`;
    /// effect bound to another rate -> `InvalidRate`; effect not accepting the clip layout ->
    /// `ChannelMismatch`.
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
        for mut spec in tracks {
            let channels = spec.clip.len();
            if !(1..=2).contains(&channels) {
                return Err(ScoreError::InvalidLayout);
            }
            let frames = spec.clip[0].len();
            if spec.clip.iter().any(|c| c.len() != frames) {
                return Err(ScoreError::LengthMismatch);
            }
            if frames == 0 || frames > MAX_CLIP_FRAMES {
                return Err(ScoreError::CapacityExceeded);
            }
            if spec.clip.iter().any(|c| c.iter().any(|v| !v.is_finite())) {
                return Err(ScoreError::NotFinite);
            }
            let end_sample = spec.start_sample.checked_add(frames as u64).ok_or(ScoreError::PositionOverflow)?;
            spec.volume_db.within(TRACK_VOLUME_DB)?;
            spec.pan.within(TRACK_PAN)?;
            let layout = if channels == 1 { ChannelLayout::mono(1) } else { ChannelLayout::stereo() };
            let mut last_param = Vec::new();
            match spec.effect.as_mut() {
                Some(effect) => {
                    if effect.required_sample_rate_hz().is_some_and(|r| r != sample_rate_hz) {
                        return Err(ScoreError::InvalidRate);
                    }
                    if !effect.accepts(&layout, &layout) {
                        return Err(ScoreError::ChannelMismatch);
                    }
                    if effect.latency_samples() != 0 || effect.max_frames().is_some_and(|m| m < max_block_frames) {
                        return Err(ScoreError::OutOfRange);
                    }
                    let params = effect.automation_parameters();
                    for (index, lane) in &spec.effect_lanes {
                        let p = params.get(*index).ok_or(ScoreError::OutOfRange)?;
                        lane.within((p.min, p.max))?;
                    }
                    last_param = vec![f64::NAN; spec.effect_lanes.len()];
                }
                None if !spec.effect_lanes.is_empty() => return Err(ScoreError::OutOfRange),
                None => {}
            }
            length = length.max(end_sample);
            built.push(Track {
                spec,
                end_sample,
                block: BlockSpec { sample_rate_hz, layout: layout.clone(), frames: 1, start_sample: 0 },
                input: vec![vec![0.0; max_block_frames]; channels],
                wet: vec![vec![0.0; max_block_frames]; channels],
                out: OutputBlock::with_capacity(layout, max_block_frames)?,
                last_param,
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
    /// Returns to sample 0 and resets every effect.
    pub fn reset(&mut self) {
        self.position = 0;
        for t in &mut self.tracks {
            if let Some(e) = t.spec.effect.as_mut() {
                e.reset();
            }
            t.last_param.fill(f64::NAN);
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
        start_sample.checked_add(frames as u64).ok_or(ScoreError::PositionOverflow)?;
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
        for track in &mut self.tracks {
            poll(cancel)?;
            let channels = track.input.len();
            // Clip samples on this block (zeros outside the clip).
            for (c, buf) in track.input.iter_mut().enumerate() {
                for (k, v) in buf[..frames].iter_mut().enumerate() {
                    let t = start + k as u64;
                    *v = if t >= track.spec.start_sample && t < track.end_sample {
                        track.spec.clip[c][(t - track.spec.start_sample) as usize]
                    } else {
                        0.0
                    };
                }
            }
            match track.spec.effect.as_mut() {
                None => {
                    for (w, i) in track.wet.iter_mut().zip(&track.input) {
                        w[..frames].copy_from_slice(&i[..frames]);
                    }
                }
                Some(effect) => {
                    let mut at = 0;
                    while at < frames {
                        let t = start + at as u64;
                        let mut end = start + frames as u64;
                        for (slot, (index, lane)) in track.spec.effect_lanes.iter().enumerate() {
                            let v = lane.value_at(t);
                            if v.to_bits() != track.last_param[slot].to_bits() {
                                effect.set_automated(*index, v)?;
                                track.last_param[slot] = v;
                            }
                            end = end.min(lane.next_change(t));
                        }
                        let len = ((end - t) as usize).min(frames - at);
                        track.block.frames = len;
                        track.block.start_sample = t;
                        let slices: [&[f32]; 2] = [
                            &track.input[0][at..at + len],
                            &track.input[channels - 1][at..at + len],
                        ];
                        let input = InputBlock::new(&track.block, &slices[..channels]);
                        effect.process(&input, &mut track.out, cancel)?;
                        for (c, w) in track.wet.iter_mut().enumerate() {
                            let src = &track.out.backing()[c * track.out.capacity()..c * track.out.capacity() + len];
                            w[at..at + len].copy_from_slice(src);
                        }
                        at += len;
                    }
                }
            }
            // Fader and pan, per sample.
            for k in 0..frames {
                let t = start + k as u64;
                let g = db_to_linear(track.spec.volume_db.value_at(t));
                let pan = track.spec.pan.value_at(t);
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
