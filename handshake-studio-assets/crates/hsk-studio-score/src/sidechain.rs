//! Dynamics with a lookahead delay and an optional external sidechain (key) input.
//!
//! [`SidechainDynamics`] reuses the [`Dynamics`] gain computer unchanged (Giannoulis, Massberg
//! and Reiss, JAES 60(6), 2012: soft-knee static curve, peak or RMS detector, one-pole
//! attack/release in dB, expander hold and range) and adds two things:
//!
//! * **Key source.** [`KeySource::Internal`] detects on the processed channels themselves;
//!   [`KeySource::External`] detects on `k` extra key channels appended after the `n` main
//!   channels of the input block (input layout = `n` main labels then `k` key labels, output
//!   layout = the `n` main labels). Linked gain: one gain driven by the loudest key channel.
//!   Unlinked gain: key channel `i` drives main channel `i`, so `k` must equal `n`.
//! * **Lookahead.** The main signal is delayed by `L = round(lookahead_ms * rate / 1000)` samples
//!   while the detector runs on the undelayed key, so the gain starts to move `L` samples before
//!   the event reaches the output. The reported latency is `L`. Precisely: output frame `m`
//!   carries main input frame `m - L` scaled by the gain after the detector has seen key frames
//!   `0..=m`.
//!
//! With `KeySource::Internal` and `lookahead_ms = 0` the output is bit-identical to
//! [`Dynamics`] (tested in `tests/sidechain.rs`). All state is allocated at construction;
//! `process` allocates nothing and is sample-sequential, so any block partition gives the same
//! output. Ranges: lookahead 0..500 follows the spec `Dynamics` sheet row `look_ahead_time`
//! (STU-FX-134a contract table, hard 0..500; the sheet declares no unit, milliseconds is
//! authored). Adobe Audition side-chain behaviour is UNVERIFIED (D5 oracle session).
use crate::block::{InputBlock, OutputBlock};
use crate::dsp::{db_to_linear, poll};
use crate::dynamics::{Dynamics, DynamicsParams};
use crate::engine::Processor;
use crate::error::ScoreError;
use crate::layout::{CANCEL_CHECK_FRAMES, ChannelLayout, MAX_CHANNELS, ticks_per_sample};
use hsk_studio_accord::CancellationToken;

/// Lookahead range in ms (spec `Dynamics` sheet `look_ahead_time` hard range 0..500).
pub const SIDECHAIN_LOOKAHEAD_MS: (f64, f64) = (0.0, 500.0);

/// Where the level detector reads its signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeySource {
    /// The processed (main) channels.
    Internal,
    /// `channels` key channels appended after the main channels of every input block.
    External { channels: usize },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SidechainParams {
    pub dynamics: DynamicsParams,
    pub key: KeySource,
    pub lookahead_ms: f64,
}

impl SidechainParams {
    pub fn validate(&self) -> Result<(), ScoreError> {
        self.dynamics.validate()?;
        if !self.lookahead_ms.is_finite() {
            return Err(ScoreError::NotFinite);
        }
        if !(SIDECHAIN_LOOKAHEAD_MS.0..=SIDECHAIN_LOOKAHEAD_MS.1).contains(&self.lookahead_ms) {
            return Err(ScoreError::OutOfRange);
        }
        Ok(())
    }
}

/// Compressor / expander / gate with lookahead and an optional external key. See the module docs.
#[derive(Debug)]
pub struct SidechainDynamics {
    sample_rate_hz: u32,
    main: usize,
    key: usize,
    external: bool,
    params: SidechainParams,
    computer: Dynamics,
    lookahead: usize,
    delay_line: Vec<f32>,
    delay_pos: usize,
    identity: String,
}

impl SidechainDynamics {
    /// `main_channels` processed channels. Refusals (before any allocation of PCM state):
    /// rate not admitted -> `InvalidRate`; channel counts out of `1..=MAX_CHANNELS`, a total
    /// input width above `MAX_CHANNELS`, or an unlinked external key whose width differs from
    /// the main width -> `InvalidLayout`; NaN/inf -> `NotFinite`; out of range -> `OutOfRange`.
    pub fn new(
        sample_rate_hz: u32,
        main_channels: usize,
        params: SidechainParams,
    ) -> Result<Self, ScoreError> {
        ticks_per_sample(sample_rate_hz)?;
        if !(1..=MAX_CHANNELS).contains(&main_channels) {
            return Err(ScoreError::InvalidLayout);
        }
        let (key, external) = match params.key {
            KeySource::Internal => (main_channels, false),
            KeySource::External { channels } => {
                if channels == 0
                    || main_channels.checked_add(channels).is_none_or(|t| t > MAX_CHANNELS)
                    || (!params.dynamics.link && channels != main_channels)
                {
                    return Err(ScoreError::InvalidLayout);
                }
                (channels, true)
            }
        };
        params.validate()?;
        let rate = f64::from(sample_rate_hz);
        // <= 500 ms * 768 kHz = 384_000 samples per channel; times 16 channels fits easily.
        let lookahead = (params.lookahead_ms * rate / 1000.0).round() as usize;
        let computer = Dynamics::new(sample_rate_hz, main_channels.max(key), params.dynamics)?;
        let identity = format!(
            "sidechain_dynamics:main={main_channels};key={:?};lookahead={:016x};{}",
            params.key,
            params.lookahead_ms.to_bits(),
            computer.identity()
        );
        Ok(Self {
            sample_rate_hz,
            main: main_channels,
            key,
            external,
            params,
            computer,
            lookahead,
            delay_line: vec![0.0; main_channels * lookahead],
            delay_pos: 0,
            identity,
        })
    }
    pub fn params(&self) -> &SidechainParams {
        &self.params
    }
    /// Lookahead (and latency) in samples.
    pub fn lookahead_samples(&self) -> usize {
        self.lookahead
    }
    /// Current smoothed gain reduction in dB for gain state `index` (0 when linked).
    pub fn gain_reduction_db(&self, index: usize) -> Option<f64> {
        self.computer.gain_reduction_db(index)
    }
}

impl Processor for SidechainDynamics {
    fn identity(&self) -> String {
        self.identity.clone()
    }
    fn latency_samples(&self) -> u32 {
        self.lookahead as u32
    }
    fn accepts(&self, input: &ChannelLayout, output: &ChannelLayout) -> bool {
        if self.external {
            input.channels() == self.main + self.key
                && output.channels() == self.main
                && input.labels()[..self.main] == *output.labels()
        } else {
            input == output && input.channels() == self.main
        }
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
        let key_offset = if self.external { self.main } else { 0 };
        let link = self.params.dynamics.link;
        let makeup = self.computer.makeup_linear();
        let mut at = 0;
        while at < frames {
            poll(cancel)?;
            let end = (at + CANCEL_CHECK_FRAMES).min(frames);
            for n in at..end {
                let mut levels = [0.0_f64; MAX_CHANNELS];
                for (k, level) in levels.iter_mut().enumerate().take(self.key) {
                    let x = f64::from(input.channel(key_offset + k)[n]);
                    *level = self.computer.detect(k, x);
                }
                let mut gains = [1.0_f64; MAX_CHANNELS];
                if link {
                    let loudest = levels[..self.key].iter().fold(0.0_f64, |a, b| a.max(*b));
                    let g = db_to_linear(self.computer.step(0, loudest)) * makeup;
                    gains[..self.main].fill(g);
                } else {
                    for (ch, gain) in gains.iter_mut().enumerate().take(self.main) {
                        *gain = db_to_linear(self.computer.step(ch, levels[ch])) * makeup;
                    }
                }
                for (ch, dst) in out.channels_mut().enumerate().take(self.main) {
                    let x = input.channel(ch)[n];
                    let delayed = if self.lookahead == 0 {
                        x
                    } else {
                        let slot = ch * self.lookahead + self.delay_pos;
                        std::mem::replace(&mut self.delay_line[slot], x)
                    };
                    dst[n] = (f64::from(delayed) * gains[ch]) as f32;
                }
                if self.lookahead > 0 {
                    self.delay_pos += 1;
                    if self.delay_pos == self.lookahead {
                        self.delay_pos = 0;
                    }
                }
            }
            at = end;
        }
        Ok(())
    }
    fn reset(&mut self) {
        self.computer.reset();
        self.delay_line.fill(0.0);
        self.delay_pos = 0;
    }
}
