//! Spectral noise reduction: STFT with a captured noise profile and a Wiener-style
//! spectral-subtraction gain, smoothed in frequency and time to limit musical noise.
//!
//! Framework: sqrt-Hann (periodic) analysis and synthesis windows, hop `N / 4`, overlap-add
//! scaled by 1/2 (the sum of the periodic Hann `w^2` at 75 percent overlap is exactly 2), so unit
//! gain reconstructs the input exactly, delayed by [`NoiseReducer::latency_samples`] = `N - 1`
//! (tested). Noise profile: the mean `|X[k]|^2` over all full frames of a noise-only clip
//! ([`NoiseProfiler`]). Per frame and bin, with `P = |X|^2` and profile power `Nn`:
//! `G = max(floor, sqrt(max(0, 1 - alpha Nn / P)))` (power spectral subtraction with
//! over-subtraction `alpha`, expressed as a gain), `floor = 10^(-reduction_db / 20)`. Musical
//! noise (isolated bins that randomly clear the subtraction) is limited by (1) replacing the gain by the
//! median over `2m + 1` neighbouring bins (removes isolated passing bins, keeps plateaus of at
//! least `m + 1` bins such as a tone's main lobe) and (2) one-pole smoothing of each bin's gain across frames
//! with separate rise and fall time constants.
use crate::block::{InputBlock, OutputBlock};
use crate::digest::{sha256, to_hex};
use crate::dsp::poll;
use crate::engine::Processor;
use crate::error::ScoreError;
use crate::layout::{CANCEL_CHECK_FRAMES, ChannelLayout, MAX_CHANNELS, ticks_per_sample};
use hsk_studio_accord::CancellationToken;
use std::f64::consts::PI;

pub const DENOISE_MIN_FFT: usize = 256;
pub const DENOISE_MAX_FFT: usize = 8192;
pub const DENOISE_REDUCTION_DB: (f64, f64) = (0.0, 60.0);
pub const DENOISE_OVER_SUBTRACTION: (f64, f64) = (1.0, 8.0);
pub const DENOISE_SMOOTHING_MS: (f64, f64) = (0.0, 1000.0);
pub const DENOISE_FREQ_SMOOTHING_BINS: usize = 8;
/// Fewest full frames a noise profile must be built from.
pub const DENOISE_MIN_PROFILE_FRAMES: u64 = 8;

/// In-place radix-2 complex FFT (forward, and inverse scaled by 1/N).
pub(crate) struct Fft {
    size: usize,
    twiddle: Vec<(f64, f64)>,
}

impl Fft {
    pub(crate) fn new(size: usize) -> Self {
        Self {
            size,
            twiddle: (0..size / 2)
                .map(|k| {
                    let a = -2.0 * PI * k as f64 / size as f64;
                    (a.cos(), a.sin())
                })
                .collect(),
        }
    }
    pub(crate) fn transform(&self, re: &mut [f64], im: &mut [f64], inverse: bool) {
        let n = self.size;
        let bits = n.trailing_zeros();
        for i in 0..n {
            let j = i.reverse_bits() >> (usize::BITS - bits);
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut half = 1;
        while half < n {
            let stride = n / (2 * half);
            for start in (0..n).step_by(2 * half) {
                for k in 0..half {
                    let (wr, mut wi) = self.twiddle[k * stride];
                    if inverse {
                        wi = -wi;
                    }
                    let (a, b) = (start + k, start + k + half);
                    let tr = re[b] * wr - im[b] * wi;
                    let ti = re[b] * wi + im[b] * wr;
                    (re[b], im[b]) = (re[a] - tr, im[a] - ti);
                    (re[a], im[a]) = (re[a] + tr, im[a] + ti);
                }
            }
            half *= 2;
        }
        if inverse {
            let s = 1.0 / n as f64;
            re.iter_mut().for_each(|v| *v *= s);
            im.iter_mut().for_each(|v| *v *= s);
        }
    }
}

fn check_size(fft_size: usize) -> Result<(), ScoreError> {
    if fft_size.is_power_of_two() && (DENOISE_MIN_FFT..=DENOISE_MAX_FFT).contains(&fft_size) {
        Ok(())
    } else {
        Err(ScoreError::OutOfRange)
    }
}

fn sqrt_hann(size: usize) -> Vec<f64> {
    (0..size).map(|i| (0.5 - 0.5 * (2.0 * PI * i as f64 / size as f64).cos()).sqrt()).collect()
}

/// Mean noise power spectrum per channel (`fft_size / 2 + 1` bins), tied to a rate and FFT size.
#[derive(Clone, Debug, PartialEq)]
pub struct NoiseProfile {
    sample_rate_hz: u32,
    fft_size: usize,
    frames: u64,
    power: Vec<Vec<f64>>,
}

impl NoiseProfile {
    pub fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }
    pub fn fft_size(&self) -> usize {
        self.fft_size
    }
    pub fn channels(&self) -> usize {
        self.power.len()
    }
    /// Number of full analysis frames averaged.
    pub fn frames(&self) -> u64 {
        self.frames
    }
    /// Mean `|X[k]|^2` of channel `ch` under the analysis window and an unscaled forward FFT.
    pub fn power(&self, ch: usize) -> Option<&[f64]> {
        self.power.get(ch).map(Vec::as_slice)
    }
    /// SHA-256 over rate, size, frame count and the exact power bits (used in the processor identity).
    pub fn digest(&self) -> String {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&self.sample_rate_hz.to_le_bytes());
        bytes.extend_from_slice(&(self.fft_size as u64).to_le_bytes());
        bytes.extend_from_slice(&self.frames.to_le_bytes());
        for channel in &self.power {
            for p in channel {
                bytes.extend_from_slice(&p.to_bits().to_le_bytes());
            }
        }
        to_hex(&sha256(&bytes))
    }
}

/// Streaming noise-profile capture: push noise-only audio in any block partition, then `finish`.
pub struct NoiseProfiler {
    sample_rate_hz: u32,
    fft_size: usize,
    fft: Fft,
    window: Vec<f64>,
    rings: Vec<Vec<f64>>,
    sums: Vec<Vec<f64>>,
    re: Vec<f64>,
    im: Vec<f64>,
    count: u64,
    frames: u64,
}

impl NoiseProfiler {
    pub fn new(sample_rate_hz: u32, channels: usize, fft_size: usize) -> Result<Self, ScoreError> {
        ticks_per_sample(sample_rate_hz)?;
        if !(1..=MAX_CHANNELS).contains(&channels) {
            return Err(ScoreError::InvalidLayout);
        }
        check_size(fft_size)?;
        Ok(Self {
            sample_rate_hz,
            fft_size,
            fft: Fft::new(fft_size),
            window: sqrt_hann(fft_size),
            rings: vec![vec![0.0; fft_size]; channels],
            sums: vec![vec![0.0; fft_size / 2 + 1]; channels],
            re: vec![0.0; fft_size],
            im: vec![0.0; fft_size],
            count: 0,
            frames: 0,
        })
    }
    pub fn push(&mut self, channels: &[&[f32]]) -> Result<(), ScoreError> {
        if channels.len() != self.rings.len() {
            return Err(ScoreError::ChannelMismatch);
        }
        let frames = channels[0].len();
        if channels.iter().any(|c| c.len() != frames) {
            return Err(ScoreError::LengthMismatch);
        }
        if channels.iter().any(|c| c.iter().any(|s| !s.is_finite())) {
            return Err(ScoreError::NotFinite);
        }
        let (n, hop) = (self.fft_size, self.fft_size / 4);
        for i in 0..frames {
            let slot = (self.count % n as u64) as usize;
            for (ring, ch) in self.rings.iter_mut().zip(channels) {
                ring[slot] = f64::from(ch[i]);
            }
            self.count += 1;
            if self.count >= n as u64 && self.count.is_multiple_of(hop as u64) {
                let oldest = (self.count % n as u64) as usize;
                for (ring, sum) in self.rings.iter().zip(&mut self.sums) {
                    for j in 0..n {
                        self.re[j] = ring[(oldest + j) % n] * self.window[j];
                        self.im[j] = 0.0;
                    }
                    self.fft.transform(&mut self.re, &mut self.im, false);
                    for (k, s) in sum.iter_mut().enumerate() {
                        *s += self.re[k] * self.re[k] + self.im[k] * self.im[k];
                    }
                }
                self.frames += 1;
            }
        }
        Ok(())
    }
    pub fn frames(&self) -> u64 {
        self.frames
    }
    /// Fails with `OutOfRange` when fewer than [`DENOISE_MIN_PROFILE_FRAMES`] full frames were seen.
    pub fn finish(self) -> Result<NoiseProfile, ScoreError> {
        if self.frames < DENOISE_MIN_PROFILE_FRAMES {
            return Err(ScoreError::OutOfRange);
        }
        let frames = self.frames;
        Ok(NoiseProfile {
            sample_rate_hz: self.sample_rate_hz,
            fft_size: self.fft_size,
            frames,
            power: self
                .sums
                .into_iter()
                .map(|s| s.into_iter().map(|v| v / frames as f64).collect())
                .collect(),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoiseReductionParams {
    /// Largest attenuation per bin in dB (0 disables the reduction).
    pub reduction_db: f64,
    /// Profile power multiplier `alpha` (1 = plain subtraction).
    pub over_subtraction: f64,
    /// Time constant when a bin's gain rises (signal onset); larger values suppress musical noise.
    pub smoothing_up_ms: f64,
    /// Time constant when a bin's gain falls.
    pub smoothing_down_ms: f64,
    /// Gain replaced by its median over `2m + 1` bins; `m` in `0..=8` (`m` bins narrower than a
    /// signal's spectral peak keeps the peak).
    pub freq_smoothing_bins: usize,
}

impl Default for NoiseReductionParams {
    fn default() -> Self {
        Self { reduction_db: 24.0, over_subtraction: 2.0, smoothing_up_ms: 40.0, smoothing_down_ms: 5.0, freq_smoothing_bins: 1 }
    }
}

impl NoiseReductionParams {
    pub fn validate(&self) -> Result<(), ScoreError> {
        let checks = [
            (self.reduction_db, DENOISE_REDUCTION_DB),
            (self.over_subtraction, DENOISE_OVER_SUBTRACTION),
            (self.smoothing_up_ms, DENOISE_SMOOTHING_MS),
            (self.smoothing_down_ms, DENOISE_SMOOTHING_MS),
        ];
        if checks.iter().any(|(v, _)| !v.is_finite()) {
            return Err(ScoreError::NotFinite);
        }
        if checks.iter().any(|(v, (lo, hi))| v < lo || v > hi) || self.freq_smoothing_bins > DENOISE_FREQ_SMOOTHING_BINS {
            return Err(ScoreError::OutOfRange);
        }
        Ok(())
    }
}

/// Noise reducer processor. All buffers are allocated at construction. Latency `fft_size - 1`.
pub struct NoiseReducer {
    sample_rate_hz: u32,
    channels: usize,
    size: usize,
    params: NoiseReductionParams,
    profile: NoiseProfile,
    fft: Fft,
    window: Vec<f64>,
    floor: f64,
    up: f64,
    down: f64,
    rings: Vec<Vec<f64>>,
    acc: Vec<Vec<f64>>,
    gains: Vec<Vec<f64>>,
    raw: Vec<f64>,
    smooth: Vec<f64>,
    re: Vec<f64>,
    im: Vec<f64>,
    count: u64,
    identity: String,
}

impl NoiseReducer {
    pub fn new(profile: &NoiseProfile, channels: usize, params: NoiseReductionParams) -> Result<Self, ScoreError> {
        let rate = profile.sample_rate_hz;
        ticks_per_sample(rate)?;
        if !(1..=MAX_CHANNELS).contains(&channels) {
            return Err(ScoreError::InvalidLayout);
        }
        if profile.channels() != channels {
            return Err(ScoreError::ChannelMismatch);
        }
        params.validate()?;
        let size = profile.fft_size;
        check_size(size)?;
        let hop_s = (size / 4) as f64 / f64::from(rate);
        let coefficient = |ms: f64| if ms <= 0.0 { 0.0 } else { (-hop_s / (ms * 1e-3)).exp() };
        let bins = size / 2 + 1;
        let identity = format!(
            "noise_reducer:rate={rate};channels={channels};fft={size};profile={};p={:016x},{:016x},{:016x},{:016x},{}",
            profile.digest(),
            params.reduction_db.to_bits(),
            params.over_subtraction.to_bits(),
            params.smoothing_up_ms.to_bits(),
            params.smoothing_down_ms.to_bits(),
            params.freq_smoothing_bins
        );
        Ok(Self {
            sample_rate_hz: rate,
            channels,
            size,
            params,
            profile: profile.clone(),
            fft: Fft::new(size),
            window: sqrt_hann(size),
            floor: 10f64.powf(-params.reduction_db / 20.0),
            up: coefficient(params.smoothing_up_ms),
            down: coefficient(params.smoothing_down_ms),
            rings: vec![vec![0.0; size]; channels],
            acc: vec![vec![0.0; size]; channels],
            gains: vec![vec![1.0; bins]; channels],
            raw: vec![1.0; bins],
            smooth: vec![1.0; bins],
            re: vec![0.0; size],
            im: vec![0.0; size],
            count: 0,
            identity,
        })
    }
    pub fn params(&self) -> &NoiseReductionParams {
        &self.params
    }
    pub fn fft_size(&self) -> usize {
        self.size
    }
    /// Smoothed gains (linear, `fft_size / 2 + 1` bins) applied in the most recent frame of `ch`.
    pub fn last_gains(&self, ch: usize) -> Option<&[f64]> {
        self.gains.get(ch).map(Vec::as_slice)
    }

    #[allow(clippy::needless_range_loop)] // index loops mirror the per-bin formulas
    /// Analyses the frame ending at the newest sample of channel `ch` and overlap-adds it.
    fn frame(&mut self, ch: usize, newest: u64) {
        let n = self.size;
        let oldest = ((newest + 1) % n as u64) as usize;
        for j in 0..n {
            self.re[j] = self.rings[ch][(oldest + j) % n] * self.window[j];
            self.im[j] = 0.0;
        }
        self.fft.transform(&mut self.re, &mut self.im, false);
        let noise = &self.profile.power[ch];
        for k in 0..=n / 2 {
            let p = self.re[k] * self.re[k] + self.im[k] * self.im[k];
            let subtracted = if p > 0.0 { (1.0 - self.params.over_subtraction * noise[k] / p).max(0.0).sqrt() } else { 1.0 };
            self.raw[k] = subtracted.max(self.floor).min(1.0);
        }
        let m = self.params.freq_smoothing_bins;
        let last = n / 2;
        for k in 0..=last {
            let (lo, hi) = (k.saturating_sub(m), (k + m).min(last));
            let mut window = [0.0_f64; 2 * DENOISE_FREQ_SMOOTHING_BINS + 1];
            let len = hi - lo + 1;
            window[..len].copy_from_slice(&self.raw[lo..=hi]);
            window[..len].sort_by(f64::total_cmp);
            self.smooth[k] = window[len / 2];
        }
        let gains = &mut self.gains[ch];
        for k in 0..=last {
            let target = self.smooth[k];
            let c = if target > gains[k] { self.up } else { self.down };
            gains[k] = c * gains[k] + (1.0 - c) * target;
        }
        for k in 0..n {
            let g = gains[k.min(n - k)];
            self.re[k] *= g;
            self.im[k] *= g;
        }
        self.fft.transform(&mut self.re, &mut self.im, true);
        for j in 0..n {
            self.acc[ch][(oldest + j) % n] += self.re[j] * self.window[j] * 0.5;
        }
    }
}

impl Processor for NoiseReducer {
    fn identity(&self) -> String {
        self.identity.clone()
    }
    fn latency_samples(&self) -> u32 {
        (self.size - 1) as u32
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
        let (n, hop) = (self.size as u64, (self.size / 4) as u64);
        let mut at = 0;
        while at < frames {
            poll(cancel)?;
            let end = (at + CANCEL_CHECK_FRAMES).min(frames);
            for i in at..end {
                let slot = (self.count % n) as usize;
                for ch in 0..self.channels {
                    self.rings[ch][slot] = f64::from(input.channel(ch)[i]);
                }
                if (self.count + 1).is_multiple_of(hop) {
                    for ch in 0..self.channels {
                        self.frame(ch, self.count);
                    }
                }
                let read = ((self.count + 1) % n) as usize;
                for (ch, dst) in out.channels_mut().enumerate() {
                    dst[i] = self.acc[ch][read] as f32;
                    self.acc[ch][read] = 0.0;
                }
                self.count += 1;
            }
            at = end;
        }
        Ok(())
    }
    fn reset(&mut self) {
        for v in self.rings.iter_mut().chain(&mut self.acc) {
            v.fill(0.0);
        }
        for g in &mut self.gains {
            g.fill(1.0);
        }
        self.count = 0;
    }
}

const DENOISE_AUTOMATION: &[crate::session::AutomationParam] = &[
    crate::session::AutomationParam { name: "reduction_db", min: DENOISE_REDUCTION_DB.0, max: DENOISE_REDUCTION_DB.1 },
    crate::session::AutomationParam { name: "over_subtraction", min: DENOISE_OVER_SUBTRACTION.0, max: DENOISE_OVER_SUBTRACTION.1 },
    crate::session::AutomationParam { name: "smoothing_up_ms", min: DENOISE_SMOOTHING_MS.0, max: DENOISE_SMOOTHING_MS.1 },
    crate::session::AutomationParam { name: "smoothing_down_ms", min: DENOISE_SMOOTHING_MS.0, max: DENOISE_SMOOTHING_MS.1 },
];

/// Automatable: 0 = `reduction_db`, 1 = `over_subtraction`, 2 = `smoothing_up_ms`, 3 =
/// `smoothing_down_ms` (taking effect from the next STFT frame); FFT size, profile and frequency
/// smoothing are fixed.
impl crate::session::Automatable for NoiseReducer {
    fn automation_parameters(&self) -> Vec<crate::session::AutomationParam> {
        DENOISE_AUTOMATION.to_vec()
    }
    fn set_automated(&mut self, index: usize, value: f64) -> Result<(), ScoreError> {
        crate::session::check_automated(DENOISE_AUTOMATION, index, value)?;
        let hop_s = (self.size / 4) as f64 / f64::from(self.sample_rate_hz);
        let coefficient = |ms: f64| if ms <= 0.0 { 0.0 } else { (-hop_s / (ms * 1e-3)).exp() };
        match index {
            0 => {
                self.params.reduction_db = value;
                self.floor = 10f64.powf(-value / 20.0);
            }
            1 => self.params.over_subtraction = value,
            2 => {
                self.params.smoothing_up_ms = value;
                self.up = coefficient(value);
            }
            _ => {
                self.params.smoothing_down_ms = value;
                self.down = coefficient(value);
            }
        }
        Ok(())
    }
}
