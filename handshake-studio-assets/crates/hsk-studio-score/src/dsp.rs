use crate::block::{InputBlock, OutputBlock};
use crate::engine::Processor;
use crate::error::ScoreError;
use crate::layout::{CANCEL_CHECK_FRAMES, ChannelLayout, MAX_CHANNELS, MAX_SAMPLE_RATE_HZ};
use hsk_studio_accord::CancellationToken;
use std::collections::VecDeque;
use std::f64::consts::FRAC_PI_4;

/// Affine normalised-to-value mapping `value = n * (max - min) + min` (STU-FX-136).
pub fn normalised_to_value(normalised: f64, min: f64, max: f64) -> f64 {
    normalised * (max - min) + min
}

/// Inverse of [`normalised_to_value`].
pub fn value_to_normalised(value: f64, min: f64, max: f64) -> f64 {
    (value - min) / (max - min)
}

pub fn db_to_linear(db: f64) -> f64 {
    10.0_f64.powf(db / 20.0)
}

pub fn linear_to_db(linear: f64) -> f64 {
    20.0 * linear.log10()
}

pub(crate) fn poll(cancel: &CancellationToken) -> Result<(), ScoreError> {
    cancel.check().map_err(|_| ScoreError::Canceled)
}

/// Runs `kernel(channel, src, dst)` over matching channels in cancel-polled chunks.
fn per_channel(
    input: &InputBlock<'_>,
    out: &mut OutputBlock,
    cancel: &CancellationToken,
    mut kernel: impl FnMut(usize, &[f32], &mut [f32]),
) -> Result<(), ScoreError> {
    let frames = input.frames();
    for (ch, (src, dst)) in input.channels().iter().zip(out.channels_mut()).enumerate() {
        let mut at = 0;
        while at < frames {
            poll(cancel)?;
            let end = (at + CANCEL_CHECK_FRAMES).min(frames);
            kernel(ch, &src[at..end], &mut dst[at..end]);
            at = end;
        }
    }
    Ok(())
}

/// Bit-exact copy; latency 0.
#[derive(Clone, Copy, Debug, Default)]
pub struct Unity;

impl Processor for Unity {
    fn latency_samples(&self) -> u32 {
        0
    }
    fn accepts(&self, input: &ChannelLayout, output: &ChannelLayout) -> bool {
        input == output
    }
    fn process(
        &mut self,
        input: &InputBlock<'_>,
        out: &mut OutputBlock,
        cancel: &CancellationToken,
    ) -> Result<(), ScoreError> {
        per_channel(input, out, cancel, |_, src, dst| dst.copy_from_slice(src))
    }
}

/// Linear gain on every channel (`out = in * linear`); STU-FX-137 track `volume`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gain {
    linear: f32,
}

impl Gain {
    pub fn new(linear: f32) -> Result<Self, ScoreError> {
        if linear.is_finite() {
            Ok(Self { linear })
        } else {
            Err(ScoreError::NotFinite)
        }
    }
    pub fn from_db(db: f64) -> Result<Self, ScoreError> {
        if !db.is_finite() {
            return Err(ScoreError::NotFinite);
        }
        Self::new(db_to_linear(db) as f32)
    }
    pub fn linear(&self) -> f32 {
        self.linear
    }
}

impl Processor for Gain {
    fn latency_samples(&self) -> u32 {
        0
    }
    fn accepts(&self, input: &ChannelLayout, output: &ChannelLayout) -> bool {
        input == output
    }
    fn process(
        &mut self,
        input: &InputBlock<'_>,
        out: &mut OutputBlock,
        cancel: &CancellationToken,
    ) -> Result<(), ScoreError> {
        let linear = self.linear;
        per_channel(input, out, cancel, |_, src, dst| {
            for (d, s) in dst.iter_mut().zip(src) {
                *d = s * linear;
            }
        })
    }
}

/// Equal-power pan of one mono source into two channels: `L = cos(a)`, `R = sin(a)`,
/// `a = (pan + 1) * pi/4`. `pan` in `-1.0..=1.0`, default 0 (STU-FX-137); the endpoints are
/// exactly `(1, 0)` and `(0, 1)`, the centre is `1/sqrt(2)` per side (-3 dB).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pan {
    pan: f32,
    left: f32,
    right: f32,
}

impl Pan {
    pub fn new(pan: f32) -> Result<Self, ScoreError> {
        if !pan.is_finite() {
            return Err(ScoreError::NotFinite);
        }
        if !(-1.0..=1.0).contains(&pan) {
            return Err(ScoreError::OutOfRange);
        }
        let (left, right) = if pan <= -1.0 {
            (1.0, 0.0)
        } else if pan >= 1.0 {
            (0.0, 1.0)
        } else {
            let angle = (f64::from(pan) + 1.0) * FRAC_PI_4;
            (angle.cos(), angle.sin())
        };
        Ok(Self {
            pan,
            left: left as f32,
            right: right as f32,
        })
    }
    pub fn pan(&self) -> f32 {
        self.pan
    }
    /// `(left, right)` linear gains.
    pub fn gains(&self) -> (f32, f32) {
        (self.left, self.right)
    }
}

impl Processor for Pan {
    fn latency_samples(&self) -> u32 {
        0
    }
    fn accepts(&self, input: &ChannelLayout, output: &ChannelLayout) -> bool {
        input.channels() == 1 && output.channels() == 2
    }
    fn process(
        &mut self,
        input: &InputBlock<'_>,
        out: &mut OutputBlock,
        cancel: &CancellationToken,
    ) -> Result<(), ScoreError> {
        let frames = input.frames();
        let src = input.channel(0);
        let (left, right) = out.pair_mut().ok_or(ScoreError::ChannelMismatch)?;
        let mut at = 0;
        while at < frames {
            poll(cancel)?;
            let end = (at + CANCEL_CHECK_FRAMES).min(frames);
            for ((s, l), r) in src[at..end]
                .iter()
                .zip(&mut left[at..end])
                .zip(&mut right[at..end])
            {
                *l = s * self.left;
                *r = s * self.right;
            }
            at = end;
        }
        Ok(())
    }
}

/// Linear channel matrix: `out[o] = sum_i coefficients[o * inputs + i] * in[i]`, accumulated in
/// ascending `i` order (deterministic). Covers down-mix, up-mix and ordered sends to a submix.
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelMix {
    inputs: usize,
    outputs: usize,
    coefficients: Vec<f32>,
}

impl ChannelMix {
    pub fn new(inputs: usize, outputs: usize, coefficients: &[f32]) -> Result<Self, ScoreError> {
        if !(1..=MAX_CHANNELS).contains(&inputs) || !(1..=MAX_CHANNELS).contains(&outputs) {
            return Err(ScoreError::InvalidLayout);
        }
        if coefficients.len() != inputs * outputs {
            return Err(ScoreError::LengthMismatch);
        }
        if coefficients.iter().any(|c| !c.is_finite()) {
            return Err(ScoreError::NotFinite);
        }
        Ok(Self {
            inputs,
            outputs,
            coefficients: coefficients.to_vec(),
        })
    }
    /// `mono = 0.5 * L + 0.5 * R`.
    pub fn stereo_to_mono() -> Self {
        Self {
            inputs: 2,
            outputs: 1,
            coefficients: vec![0.5, 0.5],
        }
    }
    /// `L = R = mono`.
    pub fn mono_to_stereo() -> Self {
        Self {
            inputs: 1,
            outputs: 2,
            coefficients: vec![1.0, 1.0],
        }
    }
}

impl Processor for ChannelMix {
    fn latency_samples(&self) -> u32 {
        0
    }
    fn accepts(&self, input: &ChannelLayout, output: &ChannelLayout) -> bool {
        input.channels() == self.inputs && output.channels() == self.outputs
    }
    fn process(
        &mut self,
        input: &InputBlock<'_>,
        out: &mut OutputBlock,
        cancel: &CancellationToken,
    ) -> Result<(), ScoreError> {
        let frames = input.frames();
        for (o, dst) in out.channels_mut().enumerate() {
            let row = &self.coefficients[o * self.inputs..(o + 1) * self.inputs];
            let mut at = 0;
            while at < frames {
                poll(cancel)?;
                let end = (at + CANCEL_CHECK_FRAMES).min(frames);
                for (k, d) in dst[at..end].iter_mut().enumerate() {
                    let n = at + k;
                    let mut acc = 0.0_f32;
                    for (i, c) in row.iter().enumerate() {
                        acc += c * input.channel(i)[n];
                    }
                    *d = acc;
                }
                at = end;
            }
        }
        Ok(())
    }
}

/// Hard Limiter contract ranges (STU-FX-136a). The sheets declare no units (136b); authored
/// units: `max_amp` dB, `input_boost` dB, `lookahead_time` ms, `release_time` ms.
pub const HARD_LIMITER_MAX_AMP_DB: (f64, f64) = (-100.0, 0.0);
pub const HARD_LIMITER_INPUT_BOOST_DB: (f64, f64) = (-100.0, 50.0);
pub const HARD_LIMITER_LOOKAHEAD_MS: (f64, f64) = (5.0, 20.0);
pub const HARD_LIMITER_RELEASE_MS: (f64, f64) = (40.0, 200.0);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HardLimiterParams {
    pub max_amp_db: f64,
    pub input_boost_db: f64,
    pub lookahead_ms: f64,
    pub release_ms: f64,
}

impl Default for HardLimiterParams {
    /// Contract defaults: -50, 20, 7.1, 100 (normalised 0.5, 0.8, 0.14, 0.375).
    fn default() -> Self {
        Self {
            max_amp_db: -50.0,
            input_boost_db: 20.0,
            lookahead_ms: 7.1,
            release_ms: 100.0,
        }
    }
}

impl HardLimiterParams {
    /// Builds from the four normalised slots `in0..in3` (max_amp, input_boost, lookahead, release).
    pub fn from_normalised(slots: [f64; 4]) -> Result<Self, ScoreError> {
        if slots.iter().any(|n| !n.is_finite()) {
            return Err(ScoreError::NotFinite);
        }
        if slots.iter().any(|n| !(0.0..=1.0).contains(n)) {
            return Err(ScoreError::OutOfRange);
        }
        let map = |n: f64, (lo, hi): (f64, f64)| normalised_to_value(n, lo, hi);
        let params = Self {
            max_amp_db: map(slots[0], HARD_LIMITER_MAX_AMP_DB),
            input_boost_db: map(slots[1], HARD_LIMITER_INPUT_BOOST_DB),
            lookahead_ms: map(slots[2], HARD_LIMITER_LOOKAHEAD_MS),
            release_ms: map(slots[3], HARD_LIMITER_RELEASE_MS),
        };
        params.validate()?;
        Ok(params)
    }
    /// Rejects non-finite values and values outside the hard ranges.
    pub fn validate(&self) -> Result<(), ScoreError> {
        let checks = [
            (self.max_amp_db, HARD_LIMITER_MAX_AMP_DB),
            (self.input_boost_db, HARD_LIMITER_INPUT_BOOST_DB),
            (self.lookahead_ms, HARD_LIMITER_LOOKAHEAD_MS),
            (self.release_ms, HARD_LIMITER_RELEASE_MS),
        ];
        if checks.iter().any(|(v, _)| !v.is_finite()) {
            return Err(ScoreError::NotFinite);
        }
        if checks.iter().any(|(v, (lo, hi))| v < lo || v > hi) {
            return Err(ScoreError::OutOfRange);
        }
        Ok(())
    }
}

/// Linked-channel lookahead brickwall limiter.
///
/// Per frame: boosted peak over all channels -> required gain `req = min(1, ceiling / peak)` ->
/// sliding minimum over the `L + 1` newest requirements (lookahead) -> one-pole release ->
/// boxcar mean over `L + 1` values (attack smoothing) -> applied to the input delayed by
/// `L = round(lookahead_ms * rate / 1000)` samples. Because every averaged term is at most the
/// requirement of the sample being output, no output exceeds the ceiling; a final clamp makes
/// that unconditional and `overshoot_events` counts any frame where the clamp had to act.
/// All buffers are allocated at construction; `process` allocates nothing.
#[derive(Debug)]
pub struct HardLimiter {
    sample_rate_hz: u32,
    channels: usize,
    ceiling: f32,
    boost: f32,
    release_coeff: f32,
    latency: usize,
    delay: Vec<f32>,
    delay_pos: usize,
    gains: Vec<f32>,
    window: VecDeque<(u64, f32)>,
    index: u64,
    smooth: Vec<f32>,
    smooth_pos: usize,
    smooth_sum: f64,
    release_prev: f32,
    overshoot_events: u64,
}

impl HardLimiter {
    pub fn new(
        sample_rate_hz: u32,
        channels: usize,
        params: HardLimiterParams,
    ) -> Result<Self, ScoreError> {
        if sample_rate_hz == 0 || sample_rate_hz > MAX_SAMPLE_RATE_HZ {
            return Err(ScoreError::InvalidRate);
        }
        if !(1..=MAX_CHANNELS).contains(&channels) {
            return Err(ScoreError::InvalidLayout);
        }
        params.validate()?;
        let rate = f64::from(sample_rate_hz);
        let latency = (params.lookahead_ms * rate / 1000.0).round() as usize;
        let release_samples = (params.release_ms * rate / 1000.0).max(1.0);
        let mut limiter = Self {
            sample_rate_hz,
            channels,
            ceiling: db_to_linear(params.max_amp_db) as f32,
            boost: db_to_linear(params.input_boost_db) as f32,
            release_coeff: (-1.0 / release_samples).exp() as f32,
            latency,
            delay: vec![0.0; channels * latency],
            delay_pos: 0,
            gains: vec![1.0; CANCEL_CHECK_FRAMES],
            window: VecDeque::with_capacity(latency + 2),
            index: 0,
            smooth: vec![1.0; latency + 1],
            smooth_pos: 0,
            smooth_sum: 0.0,
            release_prev: 1.0,
            overshoot_events: 0,
        };
        limiter.reset();
        Ok(limiter)
    }
    /// Linear ceiling `10^(max_amp / 20)` actually enforced.
    pub fn ceiling(&self) -> f32 {
        self.ceiling
    }
    /// Frames where the final clamp had to act (above the ceiling by more than 1e-6 relative).
    pub fn overshoot_events(&self) -> u64 {
        self.overshoot_events
    }

    fn boosted(sample: f32, boost: f32) -> f32 {
        (sample * boost).clamp(-f32::MAX, f32::MAX)
    }

    fn advance_gain(&mut self, peak: f32) -> f32 {
        let required = if peak > self.ceiling {
            self.ceiling / peak
        } else {
            1.0
        };
        while self.window.back().is_some_and(|&(_, v)| v >= required) {
            self.window.pop_back();
        }
        self.window.push_back((self.index, required));
        let oldest_kept = self.index.saturating_sub(self.latency as u64);
        while self.window.front().is_some_and(|&(i, _)| i < oldest_kept) {
            self.window.pop_front();
        }
        let minimum = self.window.front().map_or(1.0, |&(_, v)| v);
        let released = if minimum < self.release_prev {
            minimum
        } else {
            minimum + self.release_coeff * (self.release_prev - minimum)
        };
        self.release_prev = released;
        let slot = &mut self.smooth[self.smooth_pos];
        self.smooth_sum += f64::from(released) - f64::from(*slot);
        *slot = released;
        self.smooth_pos += 1;
        if self.smooth_pos == self.smooth.len() {
            self.smooth_pos = 0;
        }
        self.index += 1;
        (self.smooth_sum / self.smooth.len() as f64).max(0.0) as f32
    }
}

impl Processor for HardLimiter {
    fn latency_samples(&self) -> u32 {
        self.latency as u32
    }
    fn accepts(&self, input: &ChannelLayout, output: &ChannelLayout) -> bool {
        input == output && input.channels() == self.channels
    }
    fn required_sample_rate_hz(&self) -> Option<u32> {
        Some(self.sample_rate_hz)
    }
    fn process(
        &mut self,
        input: &InputBlock<'_>,
        out: &mut OutputBlock,
        cancel: &CancellationToken,
    ) -> Result<(), ScoreError> {
        let frames = input.frames();
        let (boost, ceiling, latency) = (self.boost, self.ceiling, self.latency);
        let mut at = 0;
        while at < frames {
            poll(cancel)?;
            let end = (at + CANCEL_CHECK_FRAMES).min(frames);
            let span = end - at;
            for k in 0..span {
                let mut peak = 0.0_f32;
                for c in 0..self.channels {
                    peak = peak.max(Self::boosted(input.channel(c)[at + k], boost).abs());
                }
                let gain = self.advance_gain(peak);
                self.gains[k] = gain;
            }
            for c in 0..self.channels {
                let src = &input.channel(c)[at..end];
                let dst = &mut out.channel_mut(c).ok_or(ScoreError::ChannelMismatch)?[at..end];
                let gains = &self.gains[..span];
                if latency == 0 {
                    for ((d, s), g) in dst.iter_mut().zip(src).zip(gains) {
                        let y = Self::boosted(*s, boost) * g;
                        self.overshoot_events +=
                            u64::from(y.abs() > ceiling * 1.000_001);
                        *d = y.clamp(-ceiling, ceiling);
                    }
                } else {
                    let ring = &mut self.delay[c * latency..(c + 1) * latency];
                    let mut pos = self.delay_pos;
                    for ((d, s), g) in dst.iter_mut().zip(src).zip(gains) {
                        let delayed = std::mem::replace(&mut ring[pos], Self::boosted(*s, boost));
                        pos += 1;
                        if pos == latency {
                            pos = 0;
                        }
                        let y = delayed * g;
                        self.overshoot_events +=
                            u64::from(y.abs() > ceiling * 1.000_001);
                        *d = y.clamp(-ceiling, ceiling);
                    }
                }
            }
            if latency > 0 {
                self.delay_pos = (self.delay_pos + span) % latency;
            }
            at = end;
        }
        Ok(())
    }
    fn reset(&mut self) {
        self.delay.fill(0.0);
        self.delay_pos = 0;
        self.window.clear();
        self.index = 0;
        self.smooth.fill(1.0);
        self.smooth_pos = 0;
        self.smooth_sum = self.smooth.len() as f64;
        self.release_prev = 1.0;
        self.overshoot_events = 0;
    }
}
