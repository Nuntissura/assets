//! Algorithmic reverb: Schroeder-Moorer parallel damped feedback combs into series allpasses,
//! with the Freeverb delay-length set (Jezar at Dreampoint, public domain; tunings scaled from
//! 44.1 kHz). Design, per channel bank:
//!
//! * 8 parallel feedback combs, delays `{1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617} * rate / 44100`
//!   samples. Comb `i` has loop gain `g_i = 10^(-3 d_i / (RT60 * rate))`, so every comb decays by
//!   exactly 60 dB in `RT60` seconds at DC (Schroeder's relation). Damping is a one-pole low-pass
//!   (`y = (1 - d) x + d y_prev`, unity DC gain) in the feedback path, which shortens the decay
//!   of high frequencies only.
//! * 4 series allpasses (delays `{556, 441, 341, 225}`, gain 0.5), unity magnitude response.
//! * Stereo: the right bank's delays are longer by 23 samples (at 44.1 kHz); the input is the
//!   channel mean; `width` cross-mixes the two banks as in Freeverb. Mono uses the left bank.
//! * Optional pre-delay (bounded to 200 ms) before the banks.
//!
//! Latency is 0; the reverb tail is not latency, see [`Reverb::tail_frames`]. State is stored in
//! `f64` and flushed to zero below 1e-30, so a silent tail never reaches denormals.
use crate::block::{InputBlock, OutputBlock};
use crate::dsp::poll;
use crate::engine::Processor;
use crate::error::ScoreError;
use crate::layout::{CANCEL_CHECK_FRAMES, ChannelLayout, ticks_per_sample};
use hsk_studio_accord::CancellationToken;

pub const REVERB_RT60_S: (f64, f64) = (0.1, 30.0);
pub const REVERB_DAMPING: (f64, f64) = (0.0, 1.0);
pub const REVERB_PRE_DELAY_MS: (f64, f64) = (0.0, 200.0);
pub const REVERB_WIDTH: (f64, f64) = (0.0, 1.0);
pub const REVERB_LEVEL: (f64, f64) = (0.0, 1.0);

const COMB_TUNING: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASS_TUNING: [usize; 4] = [556, 441, 341, 225];
const STEREO_SPREAD: usize = 23;
const ALLPASS_GAIN: f64 = 0.5;
/// Input scale: 8 combs sum, so 1/8 keeps the first-echo sum at unity.
const INPUT_GAIN: f64 = 0.125;
const FLUSH: f64 = 1e-30;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReverbParams {
    /// Time for the tail to fall 60 dB (at DC; damped high frequencies fall faster).
    pub rt60_s: f64,
    /// High-frequency damping in the feedback loops, `0` (none) to `1`.
    pub damping: f64,
    pub pre_delay_ms: f64,
    /// Stereo spread of the wet signal, `0` mono to `1` full.
    pub width: f64,
    /// Linear wet level.
    pub wet: f64,
    /// Linear dry level.
    pub dry: f64,
}

impl Default for ReverbParams {
    fn default() -> Self {
        Self { rt60_s: 1.5, damping: 0.3, pre_delay_ms: 0.0, width: 1.0, wet: 0.3, dry: 1.0 }
    }
}

impl ReverbParams {
    pub fn validate(&self) -> Result<(), ScoreError> {
        let checks = [
            (self.rt60_s, REVERB_RT60_S),
            (self.damping, REVERB_DAMPING),
            (self.pre_delay_ms, REVERB_PRE_DELAY_MS),
            (self.width, REVERB_WIDTH),
            (self.wet, REVERB_LEVEL),
            (self.dry, REVERB_LEVEL),
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

#[derive(Debug)]
struct Comb {
    buf: Vec<f64>,
    pos: usize,
    feedback: f64,
    store: f64,
}

impl Comb {
    fn step(&mut self, x: f64, damping: f64) -> f64 {
        let out = self.buf[self.pos];
        self.store = out * (1.0 - damping) + self.store * damping;
        if self.store.abs() < FLUSH {
            self.store = 0.0;
        }
        let v = x + self.store * self.feedback;
        self.buf[self.pos] = if v.abs() < FLUSH { 0.0 } else { v };
        self.pos += 1;
        if self.pos == self.buf.len() {
            self.pos = 0;
        }
        out
    }
}

#[derive(Debug)]
struct Allpass {
    buf: Vec<f64>,
    pos: usize,
}

impl Allpass {
    fn step(&mut self, x: f64) -> f64 {
        let held = self.buf[self.pos];
        let v = x + held * ALLPASS_GAIN;
        self.buf[self.pos] = if v.abs() < FLUSH { 0.0 } else { v };
        self.pos += 1;
        if self.pos == self.buf.len() {
            self.pos = 0;
        }
        held - x
    }
}

#[derive(Debug)]
struct Bank {
    combs: Vec<Comb>,
    allpasses: Vec<Allpass>,
}

impl Bank {
    fn new(rate: u32, rt60_s: f64, spread: usize) -> Self {
        let scale = |t: usize| ((t + spread) as f64 * f64::from(rate) / 44_100.0).round().max(1.0) as usize;
        Self {
            combs: COMB_TUNING
                .iter()
                .map(|t| {
                    let len = scale(*t);
                    Comb {
                        buf: vec![0.0; len],
                        pos: 0,
                        feedback: 10f64.powf(-3.0 * len as f64 / (rt60_s * f64::from(rate))),
                        store: 0.0,
                    }
                })
                .collect(),
            allpasses: ALLPASS_TUNING
                .iter()
                .map(|t| Allpass { buf: vec![0.0; scale(*t)], pos: 0 })
                .collect(),
        }
    }
    fn step(&mut self, x: f64, damping: f64) -> f64 {
        let mut sum = 0.0;
        for comb in &mut self.combs {
            sum += comb.step(x, damping);
        }
        for allpass in &mut self.allpasses {
            sum = allpass.step(sum);
        }
        sum
    }
    fn clear(&mut self) {
        for c in &mut self.combs {
            c.buf.fill(0.0);
            c.pos = 0;
            c.store = 0.0;
        }
        for a in &mut self.allpasses {
            a.buf.fill(0.0);
            a.pos = 0;
        }
    }
}

/// Mono or stereo algorithmic reverb (see the module docs for the design).
#[derive(Debug)]
pub struct Reverb {
    sample_rate_hz: u32,
    channels: usize,
    params: ReverbParams,
    banks: Vec<Bank>,
    pre: Vec<f64>,
    pre_pos: usize,
    wet1: f64,
    wet2: f64,
    identity: String,
}

impl Reverb {
    /// `channels` is 1 or 2.
    pub fn new(sample_rate_hz: u32, channels: usize, params: ReverbParams) -> Result<Self, ScoreError> {
        ticks_per_sample(sample_rate_hz)?;
        if !(1..=2).contains(&channels) {
            return Err(ScoreError::InvalidLayout);
        }
        params.validate()?;
        let banks = (0..channels)
            .map(|c| Bank::new(sample_rate_hz, params.rt60_s, c * STEREO_SPREAD))
            .collect();
        let pre_len = (params.pre_delay_ms * f64::from(sample_rate_hz) / 1000.0).round() as usize;
        let identity = format!(
            "reverb:rate={sample_rate_hz};channels={channels};p={:016x},{:016x},{:016x},{:016x},{:016x},{:016x}",
            params.rt60_s.to_bits(),
            params.damping.to_bits(),
            params.pre_delay_ms.to_bits(),
            params.width.to_bits(),
            params.wet.to_bits(),
            params.dry.to_bits()
        );
        Ok(Self {
            sample_rate_hz,
            channels,
            params,
            banks,
            pre: vec![0.0; pre_len],
            pre_pos: 0,
            wet1: params.wet * (1.0 + params.width) / 2.0,
            wet2: params.wet * (1.0 - params.width) / 2.0,
            identity,
        })
    }
    pub fn params(&self) -> &ReverbParams {
        &self.params
    }
    /// Frames after the input ends until the tail has fallen 60 dB at DC (`RT60` plus pre-delay).
    /// A bounce that must contain the whole tail renders this many extra frames.
    pub fn tail_frames(&self) -> u64 {
        (self.params.rt60_s * f64::from(self.sample_rate_hz)).ceil() as u64 + self.pre.len() as u64
    }
}

impl Processor for Reverb {
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
                let mut mean = 0.0;
                for ch in 0..self.channels {
                    mean += f64::from(input.channel(ch)[n]);
                }
                mean /= self.channels as f64;
                let fed = if self.pre.is_empty() {
                    mean
                } else {
                    let old = self.pre[self.pre_pos];
                    self.pre[self.pre_pos] = mean;
                    self.pre_pos = (self.pre_pos + 1) % self.pre.len();
                    old
                } * INPUT_GAIN;
                let damping = self.params.damping;
                let left = self.banks[0].step(fed, damping);
                if self.channels == 1 {
                    let y = f64::from(input.channel(0)[n]) * self.params.dry + left * self.params.wet;
                    out.channel_mut(0).expect("channel 0")[n] = y as f32;
                } else {
                    let right = self.banks[1].step(fed, damping);
                    let l = f64::from(input.channel(0)[n]) * self.params.dry + left * self.wet1 + right * self.wet2;
                    let r = f64::from(input.channel(1)[n]) * self.params.dry + right * self.wet1 + left * self.wet2;
                    let (dl, dr) = out.pair_mut().expect("stereo output");
                    dl[n] = l as f32;
                    dr[n] = r as f32;
                }
            }
            at = end;
        }
        Ok(())
    }
    fn reset(&mut self) {
        for bank in &mut self.banks {
            bank.clear();
        }
        self.pre.fill(0.0);
        self.pre_pos = 0;
    }
}

const REVERB_AUTOMATION: &[crate::session::AutomationParam] = &[
    crate::session::AutomationParam { name: "rt60_s", min: REVERB_RT60_S.0, max: REVERB_RT60_S.1 },
    crate::session::AutomationParam { name: "damping", min: REVERB_DAMPING.0, max: REVERB_DAMPING.1 },
    crate::session::AutomationParam { name: "width", min: REVERB_WIDTH.0, max: REVERB_WIDTH.1 },
    crate::session::AutomationParam { name: "wet", min: REVERB_LEVEL.0, max: REVERB_LEVEL.1 },
    crate::session::AutomationParam { name: "dry", min: REVERB_LEVEL.0, max: REVERB_LEVEL.1 },
];

/// Automatable: 0 = `rt60_s` (comb loop gains recomputed), 1 = `damping`, 2 = `width`, 3 = `wet`,
/// 4 = `dry`; the pre-delay (line length) is fixed. Delay lines keep their content.
impl crate::session::Automatable for Reverb {
    fn automation_parameters(&self) -> Vec<crate::session::AutomationParam> {
        REVERB_AUTOMATION.to_vec()
    }
    fn set_automated(&mut self, index: usize, value: f64) -> Result<(), ScoreError> {
        crate::session::check_automated(REVERB_AUTOMATION, index, value)?;
        match index {
            0 => {
                self.params.rt60_s = value;
                let rate = f64::from(self.sample_rate_hz);
                for bank in &mut self.banks {
                    for comb in &mut bank.combs {
                        comb.feedback = 10f64.powf(-3.0 * comb.buf.len() as f64 / (value * rate));
                    }
                }
            }
            1 => self.params.damping = value,
            2 => self.params.width = value,
            3 => self.params.wet = value,
            _ => self.params.dry = value,
        }
        self.wet1 = self.params.wet * (1.0 + self.params.width) / 2.0;
        self.wet2 = self.params.wet * (1.0 - self.params.width) / 2.0;
        Ok(())
    }
}
