//! Feedback delay (echo) with optional ping-pong cross-feedback and a damping low-pass in the loop.
//!
//! Per channel `c`, with `D = round(time_ms * rate / 1000)` samples (at least 1):
//!
//! ```text
//! y_c[n]   = line_c[n - D]                         (the delay-line output, i.e. the wet signal)
//! s_c[n]   = y_c[n]            (normal)    or  y_{1-c}[n]   (ping-pong, stereo only)
//! f_c[n]   = s_c[n]            (no damping) or (1 - k) s_c[n] + k f_c[n - 1],  k = exp(-2 pi f_d / rate)
//! line_c[n] = x_c[n] + feedback * f_c[n]
//! out_c[n] = (1 - mix) x_c[n] + mix y_c[n]
//! ```
//!
//! So an impulse gives echoes at `m * D` with amplitude `feedback^(m-1)` (no damping), and with
//! damping every pass through the loop convolves with the one-pole low-pass (unity DC gain, so the
//! DC steady state is `(1 - mix) + mix / (1 - feedback)` either way). Ping-pong feeds each
//! channel's echo into the other channel's line, so echoes alternate sides. Latency is 0; the echo
//! tail is not latency, see [`FeedbackDelay::tail_frames`]. Loop state is `f64` and flushed to zero
//! below 1e-30 (no denormals). All state is allocated at construction (bounded by
//! [`MAX_DELAY_LINE_SAMPLES`], checked before allocating); `process` allocates nothing and is
//! sample-sequential, so every block partition gives the same output.
//!
//! The structure is the textbook feedback comb / tapped delay line (e.g. J. O. Smith,
//! "Physical Audio Signal Processing", feedback comb filter); ranges are authored because the spec
//! row `Analog Delay` (STU-FX-134a) declares 0 parameters. Adobe Audition delay parity is
//! UNVERIFIED (D5 oracle session).
use crate::block::{InputBlock, OutputBlock};
use crate::dsp::poll;
use crate::engine::Processor;
use crate::error::ScoreError;
use crate::layout::{CANCEL_CHECK_FRAMES, ChannelLayout, MAX_CHANNELS, ticks_per_sample};
use hsk_studio_accord::CancellationToken;
use std::f64::consts::PI;

/// Delay time in ms (authored); the rounded sample count must also be at least 1.
pub const DELAY_TIME_MS: (f64, f64) = (0.0, 5000.0);
/// Loop gain (authored); `|feedback| < 1` keeps the loop stable.
pub const DELAY_FEEDBACK: (f64, f64) = (-0.99, 0.99);
/// Wet fraction `0` (dry only) to `1` (wet only).
pub const DELAY_MIX: (f64, f64) = (0.0, 1.0);
/// Lowest damping cutoff; the highest is `rate / 2`.
pub const DELAY_DAMPING_MIN_HZ: f64 = 20.0;
/// Total delay-line samples over all channels (authored bound: 8 Mi `f64` = 64 MiB).
pub const MAX_DELAY_LINE_SAMPLES: usize = 1 << 23;

const FLUSH: f64 = 1e-30;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DelayParams {
    pub time_ms: f64,
    pub feedback: f64,
    /// One-pole low-pass cutoff in the feedback loop; `None` leaves the loop flat.
    pub damping_hz: Option<f64>,
    pub mix: f64,
    /// Stereo only: each channel's echo feeds the other channel's delay line.
    pub ping_pong: bool,
}

impl Default for DelayParams {
    /// 250 ms, feedback 0.4, no damping, mix 0.3, no ping-pong.
    fn default() -> Self {
        Self { time_ms: 250.0, feedback: 0.4, damping_hz: None, mix: 0.3, ping_pong: false }
    }
}

impl DelayParams {
    /// Range checks that do not depend on the rate (see [`FeedbackDelay::new`] for the rest).
    pub fn validate(&self) -> Result<(), ScoreError> {
        let checks = [
            (self.time_ms, DELAY_TIME_MS),
            (self.feedback, DELAY_FEEDBACK),
            (self.mix, DELAY_MIX),
        ];
        if checks.iter().any(|(v, _)| !v.is_finite())
            || self.damping_hz.is_some_and(|f| !f.is_finite())
        {
            return Err(ScoreError::NotFinite);
        }
        if checks.iter().any(|(v, (lo, hi))| v < lo || v > hi) {
            return Err(ScoreError::OutOfRange);
        }
        Ok(())
    }
}

/// Feedback delay. See the module docs for the exact recurrence.
#[derive(Debug)]
pub struct FeedbackDelay {
    sample_rate_hz: u32,
    channels: usize,
    params: DelayParams,
    delay: usize,
    damping: Option<f64>,
    line: Vec<f64>,
    pos: usize,
    lowpass: [f64; MAX_CHANNELS],
    identity: String,
}

impl FeedbackDelay {
    /// Refusals, all before allocating: rate not admitted -> `InvalidRate`; channels out of
    /// `1..=MAX_CHANNELS` or ping-pong on a non-stereo layout -> `InvalidLayout`; NaN/inf ->
    /// `NotFinite`; a range violation, a delay that rounds to 0 samples or a damping cutoff
    /// outside `20..=rate/2` -> `OutOfRange`; `channels * D > MAX_DELAY_LINE_SAMPLES` ->
    /// `CapacityExceeded`.
    pub fn new(sample_rate_hz: u32, channels: usize, params: DelayParams) -> Result<Self, ScoreError> {
        ticks_per_sample(sample_rate_hz)?;
        if !(1..=MAX_CHANNELS).contains(&channels) || (params.ping_pong && channels != 2) {
            return Err(ScoreError::InvalidLayout);
        }
        params.validate()?;
        let rate = f64::from(sample_rate_hz);
        if params
            .damping_hz
            .is_some_and(|f| !(DELAY_DAMPING_MIN_HZ..=rate / 2.0).contains(&f))
        {
            return Err(ScoreError::OutOfRange);
        }
        // time_ms <= 5000 and rate <= 768 kHz, so this is <= 3.84e6 and fits usize exactly.
        let delay = (params.time_ms * rate / 1000.0).round() as usize;
        if delay == 0 {
            return Err(ScoreError::OutOfRange);
        }
        let total = channels
            .checked_mul(delay)
            .filter(|t| *t <= MAX_DELAY_LINE_SAMPLES)
            .ok_or(ScoreError::CapacityExceeded)?;
        let identity = format!(
            "feedback_delay:rate={sample_rate_hz};channels={channels};ping_pong={};damping={};p={:016x},{:016x},{:016x},{:016x}",
            params.ping_pong,
            params.damping_hz.is_some(),
            params.time_ms.to_bits(),
            params.feedback.to_bits(),
            params.damping_hz.unwrap_or(0.0).to_bits(),
            params.mix.to_bits(),
        );
        Ok(Self {
            sample_rate_hz,
            channels,
            params,
            delay,
            damping: params.damping_hz.map(|f| (-2.0 * PI * f / rate).exp()),
            line: vec![0.0; total],
            pos: 0,
            lowpass: [0.0; MAX_CHANNELS],
            identity,
        })
    }
    pub fn params(&self) -> &DelayParams {
        &self.params
    }
    /// Delay in samples (`round(time_ms * rate / 1000)`).
    pub fn delay_samples(&self) -> usize {
        self.delay
    }
    /// Frames after the input ends until undamped echoes fall below 1e-6 (-120 dB) of the first
    /// echo: `D * (m + 1)` with `m = ceil(ln 1e-6 / ln |feedback|)` (`m = 0` without feedback).
    /// Damping only shortens the tail, so this is an upper bound for it.
    pub fn tail_frames(&self) -> u64 {
        let g = self.params.feedback.abs();
        let m = if g == 0.0 { 0.0 } else { (1e-6_f64.ln() / g.ln()).ceil() };
        self.delay as u64 * (m as u64 + 1)
    }
}

impl Processor for FeedbackDelay {
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
        let (fb, mix, d) = (self.params.feedback, self.params.mix, self.delay);
        let mut at = 0;
        while at < frames {
            poll(cancel)?;
            let end = (at + CANCEL_CHECK_FRAMES).min(frames);
            for n in at..end {
                let mut taps = [0.0_f64; MAX_CHANNELS];
                for (c, tap) in taps.iter_mut().enumerate().take(self.channels) {
                    *tap = self.line[c * d + self.pos];
                }
                for (c, dst) in out.channels_mut().enumerate().take(self.channels) {
                    let x = f64::from(input.channel(c)[n]);
                    let source = if self.params.ping_pong { taps[1 - c] } else { taps[c] };
                    let filtered = match self.damping {
                        Some(k) => {
                            let mut v = (1.0 - k) * source + k * self.lowpass[c];
                            if v.abs() < FLUSH {
                                v = 0.0;
                            }
                            self.lowpass[c] = v;
                            v
                        }
                        None => source,
                    };
                    let mut write = x + fb * filtered;
                    if write.abs() < FLUSH {
                        write = 0.0;
                    }
                    self.line[c * d + self.pos] = write;
                    dst[n] = ((1.0 - mix) * x + mix * taps[c]) as f32;
                }
                self.pos += 1;
                if self.pos == d {
                    self.pos = 0;
                }
            }
            at = end;
        }
        Ok(())
    }
    fn reset(&mut self) {
        self.line.fill(0.0);
        self.pos = 0;
        self.lowpass = [0.0; MAX_CHANNELS];
    }
}

const DELAY_AUTOMATION: &[crate::session::AutomationParam] = &[
    crate::session::AutomationParam { name: "feedback", min: DELAY_FEEDBACK.0, max: DELAY_FEEDBACK.1 },
    crate::session::AutomationParam { name: "mix", min: DELAY_MIX.0, max: DELAY_MIX.1 },
];

/// Automatable: 0 = `feedback`, 1 = `mix` (take effect from the next processed sample; the
/// delay time is fixed because the line is sized at construction). `identity` keeps the
/// construction-time configuration.
impl crate::session::Automatable for FeedbackDelay {
    fn automation_parameters(&self) -> Vec<crate::session::AutomationParam> {
        DELAY_AUTOMATION.to_vec()
    }
    fn set_automated(&mut self, index: usize, value: f64) -> Result<(), ScoreError> {
        crate::session::check_automated(DELAY_AUTOMATION, index, value)?;
        match index {
            0 => self.params.feedback = value,
            _ => self.params.mix = value,
        }
        Ok(())
    }
}
