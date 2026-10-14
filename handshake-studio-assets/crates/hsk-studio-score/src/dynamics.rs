//! Dynamics processors: compressor, downward expander and gate (one [`Dynamics`] processor) and a
//! lookahead limiter with an optional true-peak detector ([`LookaheadLimiter`]).
//!
//! Gain computer: the soft-knee static curves of Giannoulis, Massberg and Reiss, "Digital Dynamic
//! Range Compressor Design - A Tutorial and Analysis", JAES 60(6), 2012 (published algorithm,
//! re-derived here; no third-party source copied). Smoothing is a one-pole filter on the gain in
//! dB with the attack/release time constants measured to 63 percent of a step.
use crate::block::{InputBlock, OutputBlock};
use crate::dsp::{db_to_linear, poll};
use crate::engine::Processor;
use crate::error::ScoreError;
use crate::layout::{CANCEL_CHECK_FRAMES, ChannelLayout, MAX_CHANNELS, ticks_per_sample};
use crate::truepeak::{TRUE_PEAK_DELAY, TruePeakFilter};
use hsk_studio_accord::CancellationToken;
use std::collections::VecDeque;

/// Authored ranges (the spec declares none for these processors; UNVERIFIED against Adobe).
pub const DYNAMICS_THRESHOLD_DB: (f64, f64) = (-100.0, 0.0);
pub const DYNAMICS_RATIO: (f64, f64) = (1.0, 100.0);
pub const DYNAMICS_KNEE_DB: (f64, f64) = (0.0, 48.0);
pub const DYNAMICS_ATTACK_MS: (f64, f64) = (0.0, 1000.0);
pub const DYNAMICS_RELEASE_MS: (f64, f64) = (1.0, 5000.0);
pub const DYNAMICS_HOLD_MS: (f64, f64) = (0.0, 5000.0);
pub const DYNAMICS_RANGE_DB: (f64, f64) = (0.0, 120.0);
pub const DYNAMICS_MAKEUP_DB: (f64, f64) = (-48.0, 48.0);
pub const DYNAMICS_RMS_WINDOW_MS: (f64, f64) = (1.0, 1000.0);
pub const LIMITER_CEILING_DB: (f64, f64) = (-60.0, 0.0);
pub const LIMITER_LOOKAHEAD_MS: (f64, f64) = (0.1, 20.0);
pub const LIMITER_RELEASE_MS: (f64, f64) = (1.0, 2000.0);

/// Level below which a detector reading is treated as digital silence (-200 dB).
const SILENCE_FLOOR: f64 = 1e-10;

/// Level detector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detector {
    /// Absolute sample value.
    Peak,
    /// Square root of an exponentially weighted mean square (`rms_window_ms` time constant).
    Rms,
}

/// Direction of the static curve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DynamicsMode {
    /// Downward compression: above the threshold the output level rises `1/ratio` dB per dB.
    Compressor,
    /// Downward expansion: below the threshold the output level falls `ratio` dB per dB; the gain
    /// reduction is limited to `range_db`. A gate is an expander with a large ratio and hold.
    Expander,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DynamicsParams {
    pub mode: DynamicsMode,
    pub threshold_db: f64,
    pub ratio: f64,
    /// Total knee width in dB, centred on the threshold; 0 is a hard knee.
    pub knee_db: f64,
    /// Compressor: time to follow increasing gain reduction. Expander: time to open.
    pub attack_ms: f64,
    /// Compressor: time to recover. Expander: time to close (after `hold_ms`).
    pub release_ms: f64,
    /// Expander only: the gain stays open this long after the level falls below the threshold.
    pub hold_ms: f64,
    /// Expander only: largest gain reduction in dB.
    pub range_db: f64,
    pub makeup_db: f64,
    pub detector: Detector,
    pub rms_window_ms: f64,
    /// `true`: one gain for all channels driven by the loudest channel; `false`: independent.
    pub link: bool,
}

impl DynamicsParams {
    /// Compressor with knee 6 dB, attack 10 ms, release 100 ms, no make-up, peak detector, linked.
    pub fn compressor(threshold_db: f64, ratio: f64) -> Self {
        Self {
            mode: DynamicsMode::Compressor,
            threshold_db,
            ratio,
            knee_db: 6.0,
            attack_ms: 10.0,
            release_ms: 100.0,
            hold_ms: 0.0,
            range_db: 120.0,
            makeup_db: 0.0,
            detector: Detector::Peak,
            rms_window_ms: 50.0,
            link: true,
        }
    }
    /// Downward expander with a hard knee, range 60 dB, attack 5 ms, release 100 ms.
    pub fn expander(threshold_db: f64, ratio: f64) -> Self {
        Self {
            mode: DynamicsMode::Expander,
            threshold_db,
            ratio,
            knee_db: 0.0,
            attack_ms: 5.0,
            release_ms: 100.0,
            hold_ms: 0.0,
            range_db: 60.0,
            makeup_db: 0.0,
            detector: Detector::Peak,
            rms_window_ms: 50.0,
            link: true,
        }
    }
    /// Gate: expander with ratio 100, range 80 dB, attack 1 ms, hold 50 ms, release 100 ms.
    pub fn gate(threshold_db: f64) -> Self {
        Self {
            ratio: 100.0,
            range_db: 80.0,
            attack_ms: 1.0,
            hold_ms: 50.0,
            ..Self::expander(threshold_db, 100.0)
        }
    }

    pub fn validate(&self) -> Result<(), ScoreError> {
        let checks = [
            (self.threshold_db, DYNAMICS_THRESHOLD_DB),
            (self.ratio, DYNAMICS_RATIO),
            (self.knee_db, DYNAMICS_KNEE_DB),
            (self.attack_ms, DYNAMICS_ATTACK_MS),
            (self.release_ms, DYNAMICS_RELEASE_MS),
            (self.hold_ms, DYNAMICS_HOLD_MS),
            (self.range_db, DYNAMICS_RANGE_DB),
            (self.makeup_db, DYNAMICS_MAKEUP_DB),
            (self.rms_window_ms, DYNAMICS_RMS_WINDOW_MS),
        ];
        if checks.iter().any(|(v, _)| !v.is_finite()) {
            return Err(ScoreError::NotFinite);
        }
        if checks.iter().any(|(v, (lo, hi))| v < lo || v > hi) {
            return Err(ScoreError::OutOfRange);
        }
        Ok(())
    }

    /// Static gain reduction in dB (`<= 0`) at detector level `level_db`, before smoothing and
    /// make-up. Compressor (hard knee): `(x - T) * (1/R - 1)` above `T`. Expander (hard knee):
    /// `(x - T) * (R - 1)` below `T`, floored at `-range_db`. Soft knee: quadratic blend over
    /// `T +/- knee/2`.
    pub fn static_reduction_db(&self, level_db: f64) -> f64 {
        let (t, r, w) = (self.threshold_db, self.ratio, self.knee_db);
        let x = level_db;
        match self.mode {
            DynamicsMode::Compressor => {
                let slope = 1.0 / r - 1.0;
                if 2.0 * (x - t) < -w {
                    0.0
                } else if w > 0.0 && 2.0 * (x - t).abs() <= w {
                    slope * (x - t + w / 2.0).powi(2) / (2.0 * w)
                } else {
                    slope * (x - t)
                }
            }
            DynamicsMode::Expander => {
                let slope = r - 1.0;
                let gr = if 2.0 * (x - t) > w {
                    0.0
                } else if w > 0.0 && 2.0 * (x - t).abs() <= w {
                    -slope * (x - t - w / 2.0).powi(2) / (2.0 * w)
                } else {
                    slope * (x - t)
                };
                gr.max(-self.range_db)
            }
        }
    }
}

fn level_db(level: f64) -> f64 {
    if level < SILENCE_FLOOR {
        -200.0
    } else {
        20.0 * level.log10()
    }
}

fn coefficient(ms: f64, rate: f64) -> f64 {
    if ms <= 0.0 {
        0.0
    } else {
        (-1.0 / (ms * 1e-3 * rate)).exp()
    }
}

/// Compressor / expander / gate. Latency 0 (no lookahead); all state is allocated at
/// construction and `process` allocates nothing. Block processing is sample-sequential, so the
/// output is bit-identical for every block partition (tested).
#[derive(Debug)]
pub struct Dynamics {
    sample_rate_hz: u32,
    channels: usize,
    params: DynamicsParams,
    attack: f64,
    release: f64,
    rms_coeff: f64,
    hold_samples: u32,
    makeup: f64,
    mean_square: Vec<f64>,
    gain_db: Vec<f64>,
    hold: Vec<u32>,
    identity: String,
}

impl Dynamics {
    pub fn new(
        sample_rate_hz: u32,
        channels: usize,
        params: DynamicsParams,
    ) -> Result<Self, ScoreError> {
        ticks_per_sample(sample_rate_hz)?;
        if !(1..=MAX_CHANNELS).contains(&channels) {
            return Err(ScoreError::InvalidLayout);
        }
        params.validate()?;
        let rate = f64::from(sample_rate_hz);
        let states = if params.link { 1 } else { channels };
        let identity = format!(
            "dynamics:rate={sample_rate_hz};channels={channels};mode={:?};det={:?};link={};p={:016x},{:016x},{:016x},{:016x},{:016x},{:016x},{:016x},{:016x},{:016x}",
            params.mode,
            params.detector,
            params.link,
            params.threshold_db.to_bits(),
            params.ratio.to_bits(),
            params.knee_db.to_bits(),
            params.attack_ms.to_bits(),
            params.release_ms.to_bits(),
            params.hold_ms.to_bits(),
            params.range_db.to_bits(),
            params.makeup_db.to_bits(),
            params.rms_window_ms.to_bits(),
        );
        Ok(Self {
            sample_rate_hz,
            channels,
            params,
            attack: coefficient(params.attack_ms, rate),
            release: coefficient(params.release_ms, rate),
            rms_coeff: coefficient(params.rms_window_ms, rate),
            hold_samples: (params.hold_ms * 1e-3 * rate).round() as u32,
            makeup: db_to_linear(params.makeup_db),
            mean_square: vec![0.0; channels],
            gain_db: vec![0.0; states],
            hold: vec![0; states],
            identity,
        })
    }
    pub fn params(&self) -> &DynamicsParams {
        &self.params
    }
    /// Current smoothed gain reduction in dB for gain state `index` (0 when linked).
    pub fn gain_reduction_db(&self, index: usize) -> Option<f64> {
        self.gain_db.get(index).copied()
    }

    /// One smoothing step for gain state `s` toward the static target for `level`.
    fn step(&mut self, s: usize, level: f64) -> f64 {
        let target = self.params.static_reduction_db(level_db(level));
        let g = self.gain_db[s];
        let next = match self.params.mode {
            DynamicsMode::Compressor => {
                let c = if target < g { self.attack } else { self.release };
                c * g + (1.0 - c) * target
            }
            DynamicsMode::Expander => {
                if target >= g {
                    self.hold[s] = self.hold_samples;
                    self.attack * g + (1.0 - self.attack) * target
                } else if self.hold[s] > 0 {
                    self.hold[s] -= 1;
                    g
                } else {
                    self.release * g + (1.0 - self.release) * target
                }
            }
        };
        self.gain_db[s] = next;
        next
    }
}

impl Processor for Dynamics {
    fn identity(&self) -> String {
        self.identity.clone()
    }
    fn latency_samples(&self) -> u32 {
        0
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
        let mut at = 0;
        while at < frames {
            poll(cancel)?;
            let end = (at + CANCEL_CHECK_FRAMES).min(frames);
            for n in at..end {
                let mut levels = [0.0_f64; MAX_CHANNELS];
                for (ch, level) in levels.iter_mut().enumerate().take(self.channels) {
                    let x = f64::from(input.channel(ch)[n]);
                    *level = match self.params.detector {
                        Detector::Peak => x.abs(),
                        Detector::Rms => {
                            let ms = self.rms_coeff * self.mean_square[ch]
                                + (1.0 - self.rms_coeff) * x * x;
                            self.mean_square[ch] = ms;
                            ms.sqrt()
                        }
                    };
                }
                let mut gains = [1.0_f64; MAX_CHANNELS];
                if self.params.link {
                    let loudest = levels[..self.channels].iter().fold(0.0_f64, |a, b| a.max(*b));
                    let g = db_to_linear(self.step(0, loudest)) * self.makeup;
                    gains[..self.channels].fill(g);
                } else {
                    for ch in 0..self.channels {
                        gains[ch] = db_to_linear(self.step(ch, levels[ch])) * self.makeup;
                    }
                }
                for (ch, dst) in out.channels_mut().enumerate() {
                    dst[n] = (f64::from(input.channel(ch)[n]) * gains[ch]) as f32;
                }
            }
            at = end;
        }
        Ok(())
    }
    fn reset(&mut self) {
        self.mean_square.fill(0.0);
        self.gain_db.fill(0.0);
        self.hold.fill(0);
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LimiterParams {
    /// Output ceiling in dBFS (dBTP when `true_peak`).
    pub ceiling_db: f64,
    pub lookahead_ms: f64,
    pub release_ms: f64,
    /// Detect on the 4x oversampled signal (adds 6 samples of latency).
    pub true_peak: bool,
}

impl Default for LimiterParams {
    /// -1 dB true-peak ceiling, 5 ms lookahead, 100 ms release.
    fn default() -> Self {
        Self {
            ceiling_db: -1.0,
            lookahead_ms: 5.0,
            release_ms: 100.0,
            true_peak: true,
        }
    }
}

impl LimiterParams {
    pub fn validate(&self) -> Result<(), ScoreError> {
        let checks = [
            (self.ceiling_db, LIMITER_CEILING_DB),
            (self.lookahead_ms, LIMITER_LOOKAHEAD_MS),
            (self.release_ms, LIMITER_RELEASE_MS),
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

/// Linked-channel lookahead brickwall limiter with an optional true-peak detector.
///
/// Let `L = max(1, round(lookahead_ms * rate / 1000))` and `o = 6` samples when `true_peak`
/// (the interpolator delay), else `0`. Per frame: detector peak over all channels (sample peak,
/// or the largest of the four 4x oversampled values) -> requirement
/// `r = min(1, ceiling / peak)` -> sliding minimum of the newest `L` requirements -> boxcar mean
/// over `L` values -> one-pole release (instant on falling gain). The result is applied to the
/// input delayed by `D = o + L - 1` samples, which is the reported latency. Every averaged term
/// is at most the requirement of the sample being output, so the gain never exceeds `r` at that
/// sample; a final sample clamp makes the sample ceiling unconditional (`clamp_events` counts
/// clamps larger than 1e-6 relative, which only f32 rounding can cause). With `true_peak` the
/// oversampled ceiling holds to within the interpolation accuracy measured in
/// `tests/dynamics.rs` (UNVERIFIED beyond those signals).
#[derive(Debug)]
pub struct LookaheadLimiter {
    sample_rate_hz: u32,
    channels: usize,
    params: LimiterParams,
    ceiling: f64,
    ceiling_f32: f32,
    window_len: usize,
    delay: usize,
    release_coeff: f64,
    filters: Vec<TruePeakFilter>,
    delay_line: Vec<f32>,
    delay_pos: usize,
    window: VecDeque<(u64, f64)>,
    boxcar: Vec<f64>,
    box_pos: usize,
    box_sum: f64,
    index: u64,
    release_prev: f64,
    clamp_events: u64,
    identity: String,
}

impl LookaheadLimiter {
    pub fn new(
        sample_rate_hz: u32,
        channels: usize,
        params: LimiterParams,
    ) -> Result<Self, ScoreError> {
        ticks_per_sample(sample_rate_hz)?;
        if !(1..=MAX_CHANNELS).contains(&channels) {
            return Err(ScoreError::InvalidLayout);
        }
        params.validate()?;
        let rate = f64::from(sample_rate_hz);
        let window_len = ((params.lookahead_ms * rate / 1000.0).round() as usize).max(1);
        let detect_offset = if params.true_peak { TRUE_PEAK_DELAY } else { 0 };
        let delay = detect_offset + window_len - 1;
        let ceiling = db_to_linear(params.ceiling_db);
        let identity = format!(
            "lookahead_limiter:rate={sample_rate_hz};channels={channels};tp={};p={:016x},{:016x},{:016x}",
            params.true_peak,
            params.ceiling_db.to_bits(),
            params.lookahead_ms.to_bits(),
            params.release_ms.to_bits()
        );
        Ok(Self {
            sample_rate_hz,
            channels,
            params,
            ceiling,
            ceiling_f32: ceiling as f32,
            window_len,
            delay,
            release_coeff: coefficient(params.release_ms, rate),
            filters: vec![TruePeakFilter::new(); channels],
            delay_line: vec![0.0; channels * delay],
            delay_pos: 0,
            window: VecDeque::with_capacity(window_len + 1),
            boxcar: vec![1.0; window_len],
            box_pos: 0,
            box_sum: window_len as f64,
            index: 0,
            release_prev: 1.0,
            clamp_events: 0,
            identity,
        })
    }
    pub fn params(&self) -> &LimiterParams {
        &self.params
    }
    /// Frames where the final sample clamp acted by more than f32 rounding.
    pub fn clamp_events(&self) -> u64 {
        self.clamp_events
    }
    pub fn lookahead_samples(&self) -> usize {
        self.window_len
    }
}

impl Processor for LookaheadLimiter {
    fn identity(&self) -> String {
        self.identity.clone()
    }
    fn latency_samples(&self) -> u32 {
        self.delay as u32
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
        let mut at = 0;
        while at < frames {
            poll(cancel)?;
            let end = (at + CANCEL_CHECK_FRAMES).min(frames);
            for n in at..end {
                let mut peak = 0.0_f64;
                for ch in 0..self.channels {
                    let x = f64::from(input.channel(ch)[n]);
                    let p = if self.params.true_peak {
                        self.filters[ch].push(x)
                    } else {
                        x.abs()
                    };
                    peak = peak.max(p);
                }
                let required = if peak > self.ceiling {
                    self.ceiling / peak
                } else {
                    1.0
                };
                // Sliding minimum of the newest `window_len` requirements.
                while self.window.back().is_some_and(|(_, v)| *v >= required) {
                    self.window.pop_back();
                }
                self.window.push_back((self.index, required));
                while self
                    .window
                    .front()
                    .is_some_and(|(i, _)| *i + self.window_len as u64 <= self.index)
                {
                    self.window.pop_front();
                }
                let minimum = self.window.front().map_or(1.0, |(_, v)| *v);
                // Boxcar mean; the running sum is recomputed exactly once per window to bound drift.
                self.box_sum += minimum - self.boxcar[self.box_pos];
                self.boxcar[self.box_pos] = minimum;
                self.box_pos += 1;
                if self.box_pos == self.window_len {
                    self.box_pos = 0;
                    self.box_sum = self.boxcar.iter().sum();
                }
                let mean = self.box_sum / self.window_len as f64;
                let gain = if mean < self.release_prev {
                    mean
                } else {
                    self.release_coeff * self.release_prev + (1.0 - self.release_coeff) * mean
                };
                self.release_prev = gain;
                for (ch, dst) in out.channels_mut().enumerate() {
                    let x = input.channel(ch)[n];
                    let delayed = if self.delay == 0 {
                        x
                    } else {
                        let slot = ch * self.delay + self.delay_pos;
                        let old = self.delay_line[slot];
                        self.delay_line[slot] = x;
                        old
                    };
                    let mut y = (f64::from(delayed) * gain) as f32;
                    let limit = self.ceiling_f32;
                    if y.abs() > limit {
                        if f64::from(y.abs()) > self.ceiling * (1.0 + 1e-6) {
                            self.clamp_events += 1;
                        }
                        y = y.clamp(-limit, limit);
                    }
                    dst[n] = y;
                }
                if self.delay > 0 {
                    self.delay_pos = (self.delay_pos + 1) % self.delay;
                }
                self.index += 1;
            }
            at = end;
        }
        Ok(())
    }
    fn reset(&mut self) {
        for f in &mut self.filters {
            f.reset();
        }
        self.delay_line.fill(0.0);
        self.delay_pos = 0;
        self.window.clear();
        self.boxcar.fill(1.0);
        self.box_pos = 0;
        self.box_sum = self.window_len as f64;
        self.index = 0;
        self.release_prev = 1.0;
        self.clamp_events = 0;
    }
}
