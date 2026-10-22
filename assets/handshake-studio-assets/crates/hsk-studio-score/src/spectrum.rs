use crate::block::all_finite;
use crate::error::ScoreError;
use hsk_studio_accord::CancellationToken;
use std::f64::consts::PI;

/// Smallest and largest transform sizes (powers of two; authored bounds).
pub const MIN_FFT_SIZE: usize = 8;
pub const MAX_FFT_SIZE: usize = 1 << 16;

/// Magnitude-spectrum analyser: periodic Hann window, in-place radix-2 FFT in `f64`, all buffers
/// allocated at construction (`analyze` allocates nothing).
///
/// Scaling: bin `k` reads the amplitude of a sinusoid centred on it. Interior bins are
/// `2 |X[k]| / sum(w)`; DC and Nyquist are `|X[k]| / sum(w)`, so a constant `A` reads `A` in bin 0
/// and a bin-centred sine of amplitude `A` reads `A` (its Hann neighbours read `A / 2`).
#[derive(Clone, Debug)]
pub struct SpectrumAnalyzer {
    size: usize,
    window: Vec<f64>,
    window_sum: f64,
    twiddle: Vec<(f64, f64)>,
    re: Vec<f64>,
    im: Vec<f64>,
    bins: Vec<f32>,
}

impl SpectrumAnalyzer {
    pub fn new(size: usize) -> Result<Self, ScoreError> {
        if !size.is_power_of_two() || !(MIN_FFT_SIZE..=MAX_FFT_SIZE).contains(&size) {
            return Err(ScoreError::OutOfRange);
        }
        let window: Vec<f64> = (0..size)
            .map(|n| 0.5 - 0.5 * (2.0 * PI * n as f64 / size as f64).cos())
            .collect();
        Ok(Self {
            size,
            window_sum: window.iter().sum(),
            window,
            twiddle: (0..size / 2)
                .map(|k| {
                    let angle = -2.0 * PI * k as f64 / size as f64;
                    (angle.cos(), angle.sin())
                })
                .collect(),
            re: vec![0.0; size],
            im: vec![0.0; size],
            bins: vec![0.0; size / 2 + 1],
        })
    }
    pub fn size(&self) -> usize {
        self.size
    }
    pub fn bin_count(&self) -> usize {
        self.size / 2 + 1
    }
    /// Centre frequency of `bin` at `sample_rate_hz`.
    pub fn bin_frequency_hz(&self, bin: usize, sample_rate_hz: u32) -> f64 {
        bin as f64 * f64::from(sample_rate_hz) / self.size as f64
    }

    /// Amplitude spectrum of exactly `size` samples; the returned slice has `size / 2 + 1` bins.
    pub fn analyze(
        &mut self,
        samples: &[f32],
        cancel: &CancellationToken,
    ) -> Result<&[f32], ScoreError> {
        cancel.check().map_err(|_| ScoreError::Canceled)?;
        if samples.len() != self.size {
            return Err(ScoreError::LengthMismatch);
        }
        if samples.iter().any(|s| !s.is_finite()) {
            return Err(ScoreError::NotFinite);
        }
        let bits = self.size.trailing_zeros();
        for (n, (s, w)) in samples.iter().zip(&self.window).enumerate() {
            let target = n.reverse_bits() >> (usize::BITS - bits);
            self.re[target] = f64::from(*s) * w;
            self.im[target] = 0.0;
        }
        let mut half = 1;
        while half < self.size {
            let stride = self.size / (2 * half);
            for start in (0..self.size).step_by(2 * half) {
                for k in 0..half {
                    let (wr, wi) = self.twiddle[k * stride];
                    let (a, b) = (start + k, start + k + half);
                    let tr = self.re[b] * wr - self.im[b] * wi;
                    let ti = self.re[b] * wi + self.im[b] * wr;
                    (self.re[b], self.im[b]) = (self.re[a] - tr, self.im[a] - ti);
                    (self.re[a], self.im[a]) = (self.re[a] + tr, self.im[a] + ti);
                }
            }
            half *= 2;
        }
        let last = self.size / 2;
        for (k, bin) in self.bins.iter_mut().enumerate() {
            let magnitude = self.re[k].hypot(self.im[k]);
            let scale = if k == 0 || k == last { 1.0 } else { 2.0 };
            *bin = (scale * magnitude / self.window_sum) as f32;
        }
        // Finite samples can still read past `f32::MAX` in a bin (a full-scale square wave reads
        // about 1.21 times its amplitude); an overflowing spectrum is refused, not returned.
        if !all_finite(&self.bins) {
            return Err(ScoreError::NotFinite);
        }
        Ok(&self.bins)
    }
}
