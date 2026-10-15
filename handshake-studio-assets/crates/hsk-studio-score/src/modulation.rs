//! Modulation effects: [`Flanger`] and [`Chorus`] (LFO-modulated fractional delay lines) and
//! [`Phaser`] (LFO-swept first-order allpass stages).
//!
//! **LFO.** A sine on a phase accumulator in cycles: `phase_c(n) = frac(phase0_c + n * rate_hz /
//! sample_rate)` (accumulated sample by sample in `f64`, wrapped at 1). Channel `c > 0` starts at
//! `stereo_phase_deg / 360`. The sweep shape is the raised cosine `s(n) = 0.5 - 0.5 cos(2 pi
//! phase_c(n))` (0 at phase 0, 1 at phase 0.5).
//!
//! **Fractional delay.** Third-order Lagrange interpolation over four neighbouring delay-line
//! samples (T. I. Laakso et al., "Splitting the Unit Delay", IEEE Signal Processing Magazine 13(1),
//! 1996). The line is read before the current sample is written (so a feedback path is causal),
//! which makes one sample the shortest delay: a requested delay below one sample reads at one
//! sample ([`Flanger::delay_samples`] reports the delay actually used). For a delay `d` the nodes
//! are delays `b..b+3` with `b = max(floor(d) - 1, 1)` and the weights are the Lagrange basis at
//! `t = d - b`; an integer `d >= 2` reads one stored sample exactly.
//!
//! * Flanger: `d(n) = initial + (final - initial) s(n)` ms; `line = x + feedback * tap`;
//!   `out = (1 - mix) x + mix tap`.
//! * Chorus: `voices` taps, voice `v` with delay `delay_ms + depth_ms sin(2 pi (phase + v/voices))`;
//!   wet = mean of the taps (no feedback); `out = (1 - mix) x + mix wet`.
//! * Phaser: `stages` first-order allpasses `y = c x + x[n-1] - c y[n-1]` with
//!   `c = (tan(pi f_b/rate) - 1)/(tan(pi f_b/rate) + 1)` (-90 degrees at `f_b`), all at the break
//!   frequency `f_b(n) = min_hz (max_hz/min_hz)^s(n)` (exponential sweep); chain input
//!   `x + feedback * previous chain output`; `out = (1 - mix) x + mix chain`.
//!
//! Ranges: Flanger `initial`/`final` 0..20, `phase` 0..360 and `mix`/`feedback` (as fractions of
//! the spec's 0..100) follow the spec Flanger sheet (STU-FX-134a); the spec rows `cycles`, `beats`,
//! the Chorus/Flanger sheet and the Phaser `intensity`/`depth` rows have no declared meaning and
//! are refused by [`bind_spec_row`](crate::bind_spec_row). LFO rate, chorus and phaser ranges are
//! authored. Latency 0 for all three; state allocated at construction; `process` allocates
//! nothing and is sample-sequential (partition invariant). Adobe parity UNVERIFIED (D5).
use crate::block::{InputBlock, OutputBlock};
use crate::dsp::poll;
use crate::engine::Processor;
use crate::error::ScoreError;
use crate::layout::{CANCEL_CHECK_FRAMES, ChannelLayout, MAX_CHANNELS, ticks_per_sample};
use hsk_studio_accord::CancellationToken;
use std::f64::consts::PI;

/// LFO rate in Hz (authored).
pub const LFO_RATE_HZ: (f64, f64) = (0.001, 20.0);
/// Flanger initial/final delay in ms (spec Flanger sheet hard range).
pub const FLANGER_DELAY_MS: (f64, f64) = (0.0, 20.0);
/// Stereo LFO phase offset in degrees (spec Flanger `phase`).
pub const STEREO_PHASE_DEG: (f64, f64) = (0.0, 360.0);
/// Feedback (authored; the spec's 0..100 percent maps to 0..1, and 1 is refused as unstable).
pub const MODULATION_FEEDBACK: (f64, f64) = (-0.99, 0.99);
/// Chorus centre delay in ms (authored); depth is `0..=delay_ms`.
pub const CHORUS_DELAY_MS: (f64, f64) = (1.0, 50.0);
pub const MAX_CHORUS_VOICES: usize = 4;
/// Phaser stage count: even, 2..=12 (authored).
pub const PHASER_STAGES: (usize, usize) = (2, 12);
/// Phaser sweep frequencies: `20 <= min_hz <= max_hz < rate/2` (authored).
pub const PHASER_MIN_HZ: f64 = 20.0;

const FLUSH: f64 = 1e-30;

fn check(values: &[(f64, (f64, f64))]) -> Result<(), ScoreError> {
    if values.iter().any(|(v, _)| !v.is_finite()) {
        return Err(ScoreError::NotFinite);
    }
    if values.iter().any(|(v, (lo, hi))| v < lo || v > hi) {
        return Err(ScoreError::OutOfRange);
    }
    Ok(())
}

fn common_new(sample_rate_hz: u32, channels: usize) -> Result<f64, ScoreError> {
    ticks_per_sample(sample_rate_hz)?;
    if !(1..=MAX_CHANNELS).contains(&channels) {
        return Err(ScoreError::InvalidLayout);
    }
    Ok(f64::from(sample_rate_hz))
}

/// Per-channel LFO phase accumulators.
#[derive(Clone, Debug)]
struct Lfo {
    increment: f64,
    start: [f64; MAX_CHANNELS],
    phase: [f64; MAX_CHANNELS],
}

impl Lfo {
    fn new(rate_hz: f64, sample_rate: f64, stereo_phase_deg: f64) -> Self {
        let mut start = [0.0; MAX_CHANNELS];
        let offset = (stereo_phase_deg / 360.0).fract();
        for s in start.iter_mut().skip(1) {
            *s = offset;
        }
        Self { increment: rate_hz / sample_rate, start, phase: start }
    }
    /// Current phase of channel `c` in cycles, then advances it by one sample.
    #[inline]
    fn next(&mut self, c: usize) -> f64 {
        let p = self.phase[c];
        let mut q = p + self.increment;
        if q >= 1.0 {
            q -= 1.0;
        }
        self.phase[c] = q;
        p
    }
    fn reset(&mut self) {
        self.phase = self.start;
    }
}

#[inline]
fn raised_cosine(phase: f64) -> f64 {
    0.5 - 0.5 * (2.0 * PI * phase).cos()
}

/// Planar ring of `channels` delay lines, read before write.
#[derive(Clone, Debug)]
struct FracLines {
    len: usize,
    buf: Vec<f64>,
    pos: usize,
}

impl FracLines {
    /// Lines able to read any delay up to `max_delay` samples.
    fn new(channels: usize, max_delay: f64) -> Self {
        let len = max_delay.ceil() as usize + 5;
        Self { len, buf: vec![0.0; channels * len], pos: 0 }
    }
    #[inline]
    fn at(&self, c: usize, k: usize) -> f64 {
        // Delay k >= 1 is w[n - k]; pos is where w[n] will be written.
        self.buf[c * self.len + (self.pos + self.len - k) % self.len]
    }
    /// Lagrange-3 read at delay `d` (clamped to at least one sample).
    #[inline]
    fn read(&self, c: usize, d: f64) -> f64 {
        let d = d.max(1.0);
        let base = (d.floor() as usize).saturating_sub(1).max(1);
        let t = d - base as f64;
        let h0 = -(t - 1.0) * (t - 2.0) * (t - 3.0) / 6.0;
        let h1 = t * (t - 2.0) * (t - 3.0) / 2.0;
        let h2 = -t * (t - 1.0) * (t - 3.0) / 2.0;
        let h3 = t * (t - 1.0) * (t - 2.0) / 6.0;
        h0 * self.at(c, base)
            + h1 * self.at(c, base + 1)
            + h2 * self.at(c, base + 2)
            + h3 * self.at(c, base + 3)
    }
    #[inline]
    fn write(&mut self, c: usize, v: f64) {
        self.buf[c * self.len + self.pos] = if v.abs() < FLUSH { 0.0 } else { v };
    }
    #[inline]
    fn advance(&mut self) {
        self.pos += 1;
        if self.pos == self.len {
            self.pos = 0;
        }
    }
    fn reset(&mut self) {
        self.buf.fill(0.0);
        self.pos = 0;
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlangerParams {
    pub initial_ms: f64,
    pub final_ms: f64,
    pub rate_hz: f64,
    pub stereo_phase_deg: f64,
    pub feedback: f64,
    pub mix: f64,
}

impl Default for FlangerParams {
    /// 1 -> 5 ms sweep at 0.25 Hz, 90 degree stereo phase, feedback 0.5, mix 0.5.
    fn default() -> Self {
        Self { initial_ms: 1.0, final_ms: 5.0, rate_hz: 0.25, stereo_phase_deg: 90.0, feedback: 0.5, mix: 0.5 }
    }
}

impl FlangerParams {
    pub fn validate(&self) -> Result<(), ScoreError> {
        check(&[
            (self.initial_ms, FLANGER_DELAY_MS),
            (self.final_ms, FLANGER_DELAY_MS),
            (self.rate_hz, LFO_RATE_HZ),
            (self.stereo_phase_deg, STEREO_PHASE_DEG),
            (self.feedback, MODULATION_FEEDBACK),
            (self.mix, (0.0, 1.0)),
        ])
    }
}

/// Flanger: one LFO-swept fractional delay with feedback. See the module docs.
#[derive(Debug)]
pub struct Flanger {
    sample_rate_hz: u32,
    channels: usize,
    params: FlangerParams,
    initial: f64,
    span: f64,
    lfo: Lfo,
    lines: FracLines,
    last_delay: [f64; MAX_CHANNELS],
    identity: String,
}

impl Flanger {
    pub fn new(sample_rate_hz: u32, channels: usize, params: FlangerParams) -> Result<Self, ScoreError> {
        let rate = common_new(sample_rate_hz, channels)?;
        params.validate()?;
        let ms = rate / 1000.0;
        let identity = format!(
            "flanger:rate={sample_rate_hz};channels={channels};p={:016x},{:016x},{:016x},{:016x},{:016x},{:016x}",
            params.initial_ms.to_bits(),
            params.final_ms.to_bits(),
            params.rate_hz.to_bits(),
            params.stereo_phase_deg.to_bits(),
            params.feedback.to_bits(),
            params.mix.to_bits()
        );
        Ok(Self {
            sample_rate_hz,
            channels,
            params,
            initial: params.initial_ms * ms,
            span: (params.final_ms - params.initial_ms) * ms,
            lfo: Lfo::new(params.rate_hz, rate, params.stereo_phase_deg),
            lines: FracLines::new(channels, params.initial_ms.max(params.final_ms) * ms),
            last_delay: [0.0; MAX_CHANNELS],
            identity,
        })
    }
    pub fn params(&self) -> &FlangerParams {
        &self.params
    }
    /// Delay in samples used for the most recent frame of channel `c` (after the one-sample floor).
    pub fn delay_samples(&self, c: usize) -> Option<f64> {
        (c < self.channels).then(|| self.last_delay[c])
    }
}

impl Processor for Flanger {
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
    fn process(&mut self, input: &InputBlock<'_>, out: &mut OutputBlock, cancel: &CancellationToken) -> Result<(), ScoreError> {
        let frames = input.frames();
        let (fb, mix) = (self.params.feedback, self.params.mix);
        let mut at = 0;
        while at < frames {
            poll(cancel)?;
            let end = (at + CANCEL_CHECK_FRAMES).min(frames);
            for n in at..end {
                for (c, dst) in out.channels_mut().enumerate().take(self.channels) {
                    let x = f64::from(input.channel(c)[n]);
                    let d = (self.initial + self.span * raised_cosine(self.lfo.next(c))).max(1.0);
                    self.last_delay[c] = d;
                    let tap = self.lines.read(c, d);
                    self.lines.write(c, x + fb * tap);
                    dst[n] = ((1.0 - mix) * x + mix * tap) as f32;
                }
                self.lines.advance();
            }
            at = end;
        }
        Ok(())
    }
    fn reset(&mut self) {
        self.lfo.reset();
        self.lines.reset();
        self.last_delay = [0.0; MAX_CHANNELS];
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChorusParams {
    pub voices: usize,
    pub delay_ms: f64,
    pub depth_ms: f64,
    pub rate_hz: f64,
    pub stereo_phase_deg: f64,
    pub mix: f64,
}

impl Default for ChorusParams {
    /// 3 voices, 20 ms +/- 3 ms at 0.8 Hz, 90 degree stereo phase, mix 0.5.
    fn default() -> Self {
        Self { voices: 3, delay_ms: 20.0, depth_ms: 3.0, rate_hz: 0.8, stereo_phase_deg: 90.0, mix: 0.5 }
    }
}

impl ChorusParams {
    pub fn validate(&self) -> Result<(), ScoreError> {
        check(&[
            (self.delay_ms, CHORUS_DELAY_MS),
            (self.depth_ms, (0.0, self.delay_ms.max(0.0))),
            (self.rate_hz, LFO_RATE_HZ),
            (self.stereo_phase_deg, STEREO_PHASE_DEG),
            (self.mix, (0.0, 1.0)),
        ])?;
        if !(1..=MAX_CHORUS_VOICES).contains(&self.voices) {
            return Err(ScoreError::OutOfRange);
        }
        Ok(())
    }
}

/// Chorus: `voices` LFO-modulated taps on one delay line per channel. See the module docs.
#[derive(Debug)]
pub struct Chorus {
    sample_rate_hz: u32,
    channels: usize,
    params: ChorusParams,
    centre: f64,
    depth: f64,
    lfo: Lfo,
    lines: FracLines,
    identity: String,
}

impl Chorus {
    pub fn new(sample_rate_hz: u32, channels: usize, params: ChorusParams) -> Result<Self, ScoreError> {
        let rate = common_new(sample_rate_hz, channels)?;
        params.validate()?;
        let ms = rate / 1000.0;
        let identity = format!(
            "chorus:rate={sample_rate_hz};channels={channels};voices={};p={:016x},{:016x},{:016x},{:016x},{:016x}",
            params.voices,
            params.delay_ms.to_bits(),
            params.depth_ms.to_bits(),
            params.rate_hz.to_bits(),
            params.stereo_phase_deg.to_bits(),
            params.mix.to_bits()
        );
        Ok(Self {
            sample_rate_hz,
            channels,
            params,
            centre: params.delay_ms * ms,
            depth: params.depth_ms * ms,
            lfo: Lfo::new(params.rate_hz, rate, params.stereo_phase_deg),
            lines: FracLines::new(channels, (params.delay_ms + params.depth_ms) * ms),
            identity,
        })
    }
    pub fn params(&self) -> &ChorusParams {
        &self.params
    }
}

impl Processor for Chorus {
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
    fn process(&mut self, input: &InputBlock<'_>, out: &mut OutputBlock, cancel: &CancellationToken) -> Result<(), ScoreError> {
        let frames = input.frames();
        let voices = self.params.voices;
        let mix = self.params.mix;
        let mut at = 0;
        while at < frames {
            poll(cancel)?;
            let end = (at + CANCEL_CHECK_FRAMES).min(frames);
            for n in at..end {
                for (c, dst) in out.channels_mut().enumerate().take(self.channels) {
                    let x = f64::from(input.channel(c)[n]);
                    let phase = self.lfo.next(c);
                    let mut wet = 0.0;
                    for v in 0..voices {
                        let p = phase + v as f64 / voices as f64;
                        let d = self.centre + self.depth * (2.0 * PI * p).sin();
                        wet += self.lines.read(c, d);
                    }
                    wet /= voices as f64;
                    self.lines.write(c, x);
                    dst[n] = ((1.0 - mix) * x + mix * wet) as f32;
                }
                self.lines.advance();
            }
            at = end;
        }
        Ok(())
    }
    fn reset(&mut self) {
        self.lfo.reset();
        self.lines.reset();
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhaserParams {
    pub stages: usize,
    pub min_hz: f64,
    pub max_hz: f64,
    pub rate_hz: f64,
    pub stereo_phase_deg: f64,
    pub feedback: f64,
    pub mix: f64,
}

impl Default for PhaserParams {
    /// 4 stages sweeping 300 Hz .. 3 kHz at 0.5 Hz, 90 degree stereo phase, feedback 0.3, mix 0.5.
    fn default() -> Self {
        Self { stages: 4, min_hz: 300.0, max_hz: 3000.0, rate_hz: 0.5, stereo_phase_deg: 90.0, feedback: 0.3, mix: 0.5 }
    }
}

/// Phaser: `stages` LFO-swept first-order allpasses with feedback. See the module docs.
#[derive(Debug)]
pub struct Phaser {
    sample_rate_hz: u32,
    channels: usize,
    params: PhaserParams,
    rate: f64,
    lfo: Lfo,
    x1: [[f64; PHASER_STAGES.1]; MAX_CHANNELS],
    y1: [[f64; PHASER_STAGES.1]; MAX_CHANNELS],
    last: [f64; MAX_CHANNELS],
    last_break: [f64; MAX_CHANNELS],
    identity: String,
}

impl Phaser {
    /// Refusals: `InvalidRate`, `InvalidLayout` as for every processor; NaN/inf -> `NotFinite`;
    /// odd or out-of-range stage count, `min_hz < 20`, `max_hz < min_hz`, `max_hz >= rate/2` or
    /// another range violation -> `OutOfRange`.
    pub fn new(sample_rate_hz: u32, channels: usize, params: PhaserParams) -> Result<Self, ScoreError> {
        let rate = common_new(sample_rate_hz, channels)?;
        check(&[
            (params.min_hz, (PHASER_MIN_HZ, f64::MAX)),
            (params.max_hz, (PHASER_MIN_HZ, f64::MAX)),
            (params.rate_hz, LFO_RATE_HZ),
            (params.stereo_phase_deg, STEREO_PHASE_DEG),
            (params.feedback, MODULATION_FEEDBACK),
            (params.mix, (0.0, 1.0)),
        ])?;
        if !(PHASER_STAGES.0..=PHASER_STAGES.1).contains(&params.stages)
            || !params.stages.is_multiple_of(2)
            || params.max_hz < params.min_hz
            || params.max_hz >= rate / 2.0
        {
            return Err(ScoreError::OutOfRange);
        }
        let identity = format!(
            "phaser:rate={sample_rate_hz};channels={channels};stages={};p={:016x},{:016x},{:016x},{:016x},{:016x},{:016x}",
            params.stages,
            params.min_hz.to_bits(),
            params.max_hz.to_bits(),
            params.rate_hz.to_bits(),
            params.stereo_phase_deg.to_bits(),
            params.feedback.to_bits(),
            params.mix.to_bits()
        );
        Ok(Self {
            sample_rate_hz,
            channels,
            params,
            rate,
            lfo: Lfo::new(params.rate_hz, rate, params.stereo_phase_deg),
            x1: [[0.0; PHASER_STAGES.1]; MAX_CHANNELS],
            y1: [[0.0; PHASER_STAGES.1]; MAX_CHANNELS],
            last: [0.0; MAX_CHANNELS],
            last_break: [0.0; MAX_CHANNELS],
            identity,
        })
    }
    pub fn params(&self) -> &PhaserParams {
        &self.params
    }
    /// Break frequency used for the most recent frame of channel `c`.
    pub fn break_hz(&self, c: usize) -> Option<f64> {
        (c < self.channels).then(|| self.last_break[c])
    }
}

impl Processor for Phaser {
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
    fn process(&mut self, input: &InputBlock<'_>, out: &mut OutputBlock, cancel: &CancellationToken) -> Result<(), ScoreError> {
        let frames = input.frames();
        let p = self.params;
        let ratio = p.max_hz / p.min_hz;
        let mut at = 0;
        while at < frames {
            poll(cancel)?;
            let end = (at + CANCEL_CHECK_FRAMES).min(frames);
            for n in at..end {
                for (c, dst) in out.channels_mut().enumerate().take(self.channels) {
                    let x = f64::from(input.channel(c)[n]);
                    let f = p.min_hz * ratio.powf(raised_cosine(self.lfo.next(c)));
                    self.last_break[c] = f;
                    let t = (PI * f / self.rate).tan();
                    let k = (t - 1.0) / (t + 1.0);
                    let mut v = x + p.feedback * self.last[c];
                    for s in 0..p.stages {
                        let y = k * v + self.x1[c][s] - k * self.y1[c][s];
                        self.x1[c][s] = v;
                        self.y1[c][s] = if y.abs() < FLUSH { 0.0 } else { y };
                        v = y;
                    }
                    self.last[c] = v;
                    dst[n] = ((1.0 - p.mix) * x + p.mix * v) as f32;
                }
            }
            at = end;
        }
        Ok(())
    }
    fn reset(&mut self) {
        self.lfo.reset();
        self.x1 = [[0.0; PHASER_STAGES.1]; MAX_CHANNELS];
        self.y1 = [[0.0; PHASER_STAGES.1]; MAX_CHANNELS];
        self.last = [0.0; MAX_CHANNELS];
        self.last_break = [0.0; MAX_CHANNELS];
    }
}
