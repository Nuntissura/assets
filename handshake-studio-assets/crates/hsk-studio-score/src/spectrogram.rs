//! Spectral frequency display data: an STFT magnitude grid, optionally with reassigned
//! time-frequency coordinates, computed tile by tile with bounded memory.
//!
//! **Grid.** Periodic Hann window of `N` samples (power of two, 256..=16384), hop `H` (1..=N).
//! Frame `k` is centred on sample `k H` and reads samples `k H - N/2 .. k H + N/2` (zero outside
//! the clip); a clip of `len` samples has `ceil(len / H)` frames and `N/2 + 1` bins, bin `b` at
//! `b rate / N` Hz. Cell value = `2 |X_b| / sum(w)` (`|X_b| / sum(w)` at DC and Nyquist), so a
//! bin-centred sine of amplitude `A` reads `A`.
//!
//! **Tiles.** [`Spectrogram::tile`] fills frames `first .. first + count` from a caller slice that
//! holds only the samples those frames read ([`Spectrogram::tile_input_range`]), so a long file
//! can be streamed through in chunks; every tile is bit-identical to the same frames of a whole
//! grid (tested). A tile is limited to [`MAX_TILE_CELLS`] cells, checked before any work; the
//! scratch (a few `N`-length buffers) is allocated once in [`Spectrogram::new`] and tiles
//! allocate nothing.
//!
//! **Reassignment** (F. Auger and P. Flandrin, "Improving the readability of time-frequency and
//! time-scale representations by the reassignment method", IEEE TSP 43(5), 1995; re-derived):
//! with the window `h`, `T h` (`t h(t)`, `t` relative to the frame centre) and `D h` (the
//! derivative of the continuous Hann), each cell gets the reassigned time
//! `t^ = kH + Re(X_Th conj(X_h)) / |X_h|^2` (samples) and frequency
//! `f^ = (b / N - Im(X_Dh conj(X_h)) / (2 pi |X_h|^2)) rate` (Hz). Cells with `|X_h|^2` below 1e-24
//! keep their grid coordinates. Deterministic (fixed operation order). Audition display parity
//! UNVERIFIED (D5).
//!
//! **Window choice** (lane SMB-AU-WAVEFORM-EDITOR): [`Spectrogram::with_window`] takes a
//! [`SpectralWindow`], a periodic cosine-sum window `w(i) = a0 - a1 cos(2 pi i/N) + a2 cos(4 pi i/N)
//! - a3 cos(6 pi i/N)` (Hann 0.5/0.5; Hamming 0.54/0.46; Blackman 0.42/0.5/0.08; 4-term
//! Blackman-Harris 0.35875/0.48829/0.14128/0.01168, F. J. Harris, Proc. IEEE 66(1), 1978) or
//! rectangular (`a0 = 1`). The calibration `2 |X_b| / sum(w)` holds for every window (a bin-centred
//! sine of amplitude `A` reads `A`); `D h` is the analytic derivative of the same cosine sum
//! (reassignment of the rectangular window, which has none, keeps the grid frequency). `new` is
//! `with_window(.., Hann)`, computed by the original expressions (bit-identical).
use crate::denoise::Fft;
use crate::error::ScoreError;
use crate::layout::ticks_per_sample;
use std::f64::consts::PI;

/// FFT size range (powers of two).
pub const SPECTROGRAM_FFT: (usize, usize) = (256, 16_384);
/// Largest tile in cells (frames x bins): 2^24 f32 = 64 MiB per output array.
pub const MAX_TILE_CELLS: usize = 1 << 24;

/// Analysis window of a [`Spectrogram`] (module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SpectralWindow {
    #[default]
    Hann,
    Hamming,
    Blackman,
    BlackmanHarris,
    Rectangular,
}

impl SpectralWindow {
    /// Cosine-sum coefficients `a0..a3`.
    pub const fn coefficients(self) -> [f64; 4] {
        match self {
            Self::Hann => [0.5, 0.5, 0.0, 0.0],
            Self::Hamming => [0.54, 0.46, 0.0, 0.0],
            Self::Blackman => [0.42, 0.5, 0.08, 0.0],
            Self::BlackmanHarris => [0.358_75, 0.488_29, 0.141_28, 0.011_68],
            Self::Rectangular => [1.0, 0.0, 0.0, 0.0],
        }
    }
    pub const fn name(self) -> &'static str {
        match self {
            Self::Hann => "hann",
            Self::Hamming => "hamming",
            Self::Blackman => "blackman",
            Self::BlackmanHarris => "blackman_harris",
            Self::Rectangular => "rectangular",
        }
    }
    /// Periodic window of `n` samples.
    pub fn samples(self, n: usize) -> Vec<f64> {
        let a = self.coefficients();
        (0..n)
            .map(|i| {
                let t = 2.0 * PI * i as f64 / n as f64;
                if self == Self::Hann { 0.5 - 0.5 * t.cos() } else { a[0] - a[1] * t.cos() + a[2] * (2.0 * t).cos() - a[3] * (3.0 * t).cos() }
            })
            .collect()
    }
    /// Derivative of the continuous window with respect to the sample index.
    fn derivative(self, n: usize) -> Vec<f64> {
        let a = self.coefficients();
        let step = 2.0 * PI / n as f64;
        (0..n)
            .map(|i| {
                let t = 2.0 * PI * i as f64 / n as f64;
                if self == Self::Hann {
                    PI / n as f64 * t.sin()
                } else {
                    step * (a[1] * t.sin() - 2.0 * a[2] * (2.0 * t).sin() + 3.0 * a[3] * (3.0 * t).sin())
                }
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpectrogramParams {
    pub fft_size: usize,
    pub hop: usize,
}

/// Reassigned coordinates of a tile (same layout as the magnitude grid).
pub struct ReassignedTile<'a> {
    pub magnitude: &'a mut [f32],
    /// Reassigned time in samples (absolute).
    pub time: &'a mut [f64],
    /// Reassigned frequency in Hz.
    pub frequency: &'a mut [f64],
}

/// Reusable STFT scratch for one parameter set.
pub struct Spectrogram {
    sample_rate_hz: u32,
    params: SpectrogramParams,
    window_kind: SpectralWindow,
    fft: Fft,
    window: Vec<f64>,
    window_t: Vec<f64>,
    window_d: Vec<f64>,
    scale: f64,
    re: Vec<f64>,
    im: Vec<f64>,
    re_t: Vec<f64>,
    im_t: Vec<f64>,
    re_d: Vec<f64>,
    im_d: Vec<f64>,
}

impl Spectrogram {
    /// Refusals: rate not admitted -> `InvalidRate`; FFT size not a power of two in
    /// [`SPECTROGRAM_FFT`] or hop outside `1..=fft_size` -> `OutOfRange`.
    pub fn new(sample_rate_hz: u32, params: SpectrogramParams) -> Result<Self, ScoreError> {
        Self::with_window(sample_rate_hz, params, SpectralWindow::Hann)
    }

    /// As [`new`](Self::new) with the analysis window `window` (module docs).
    pub fn with_window(sample_rate_hz: u32, params: SpectrogramParams, window_kind: SpectralWindow) -> Result<Self, ScoreError> {
        ticks_per_sample(sample_rate_hz)?;
        let n = params.fft_size;
        if !n.is_power_of_two() || !(SPECTROGRAM_FFT.0..=SPECTROGRAM_FFT.1).contains(&n) || !(1..=n).contains(&params.hop) {
            return Err(ScoreError::OutOfRange);
        }
        let window = window_kind.samples(n);
        let half = (n / 2) as f64;
        let window_t = window.iter().enumerate().map(|(i, w)| (i as f64 - half) * w).collect();
        let window_d = window_kind.derivative(n);
        let scale = 2.0 / window.iter().sum::<f64>();
        Ok(Self {
            sample_rate_hz,
            params,
            window_kind,
            fft: Fft::new(n),
            window,
            window_t,
            window_d,
            scale,
            re: vec![0.0; n],
            im: vec![0.0; n],
            re_t: vec![0.0; n],
            im_t: vec![0.0; n],
            re_d: vec![0.0; n],
            im_d: vec![0.0; n],
        })
    }
    pub fn params(&self) -> SpectrogramParams {
        self.params
    }
    pub fn window(&self) -> SpectralWindow {
        self.window_kind
    }
    pub fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }
    pub fn bins(&self) -> usize {
        self.params.fft_size / 2 + 1
    }
    /// Frames of a clip of `len` samples: `ceil(len / hop)`.
    pub fn frame_count(&self, len: u64) -> u64 {
        len.div_ceil(self.params.hop as u64)
    }
    /// Absolute sample range `[start, end)` (clipped to `0..len`) that frames `first .. first +
    /// count` read.
    pub fn tile_input_range(&self, len: u64, first: u64, count: u64) -> (u64, u64) {
        let half = (self.params.fft_size / 2) as u64;
        let hop = self.params.hop as u64;
        let start = (first * hop).saturating_sub(half);
        let end = ((first + count.max(1) - 1) * hop + half).min(len);
        (start.min(len), end)
    }

    fn check(&self, input: &[f32], input_start: u64, len: u64, first: u64, count: usize, out_len: usize) -> Result<(), ScoreError> {
        let cells = count.checked_mul(self.bins()).ok_or(ScoreError::CapacityExceeded)?;
        if cells > MAX_TILE_CELLS {
            return Err(ScoreError::CapacityExceeded);
        }
        if count == 0 || first.checked_add(count as u64).is_none_or(|e| e > self.frame_count(len)) {
            return Err(ScoreError::OutOfRange);
        }
        if out_len != cells {
            return Err(ScoreError::LengthMismatch);
        }
        let (start, end) = self.tile_input_range(len, first, count as u64);
        if input_start > start || input_start + (input.len() as u64) < end {
            return Err(ScoreError::OutOfRange);
        }
        let lo = (start - input_start) as usize;
        let hi = (end - input_start) as usize;
        if input[lo..hi].iter().any(|v| !v.is_finite()) {
            return Err(ScoreError::NotFinite);
        }
        Ok(())
    }

    /// Loads and transforms frame `k` (with the `T h` and `D h` windows too when `reassign`).
    fn frame(&mut self, input: &[f32], input_start: u64, len: u64, k: u64, reassign: bool) {
        let n = self.params.fft_size;
        let centre = (k * self.params.hop as u64) as i64;
        let base = centre - (n / 2) as i64;
        for i in 0..n {
            let abs = base + i as i64;
            let x = if abs < 0 || abs as u64 >= len { 0.0 } else { f64::from(input[(abs as u64 - input_start) as usize]) };
            self.re[i] = x * self.window[i];
            self.im[i] = 0.0;
            if reassign {
                self.re_t[i] = x * self.window_t[i];
                self.im_t[i] = 0.0;
                self.re_d[i] = x * self.window_d[i];
                self.im_d[i] = 0.0;
            }
        }
        self.fft.transform(&mut self.re, &mut self.im, false);
        if reassign {
            self.fft.transform(&mut self.re_t, &mut self.im_t, false);
            self.fft.transform(&mut self.re_d, &mut self.im_d, false);
        }
    }

    fn magnitude(&self, b: usize) -> f32 {
        let m = (self.re[b] * self.re[b] + self.im[b] * self.im[b]).sqrt() * self.scale;
        let edge = b == 0 || b == self.params.fft_size / 2;
        (if edge { m / 2.0 } else { m }) as f32
    }

    /// Magnitude grid of frames `first .. first + count` (row-major: frame, then bin) of a clip of
    /// `len` samples. `input` holds absolute samples `input_start ..`, covering at least
    /// [`tile_input_range`](Self::tile_input_range). Refusals: more than [`MAX_TILE_CELLS`] cells
    /// -> `CapacityExceeded`; no frames, frames past the clip or input not covering the range ->
    /// `OutOfRange`; `out.len() != count * bins` -> `LengthMismatch`; NaN/inf input -> `NotFinite`.
    pub fn tile(&mut self, input: &[f32], input_start: u64, len: u64, first: u64, count: usize, out: &mut [f32]) -> Result<(), ScoreError> {
        self.check(input, input_start, len, first, count, out.len())?;
        let bins = self.bins();
        for (j, row) in out.chunks_mut(bins).enumerate() {
            self.frame(input, input_start, len, first + j as u64, false);
            for (b, cell) in row.iter_mut().enumerate() {
                *cell = self.magnitude(b);
            }
        }
        Ok(())
    }

    /// As [`tile`](Self::tile), plus reassigned time and frequency per cell.
    pub fn tile_reassigned(&mut self, input: &[f32], input_start: u64, len: u64, first: u64, count: usize, out: ReassignedTile<'_>) -> Result<(), ScoreError> {
        self.check(input, input_start, len, first, count, out.magnitude.len())?;
        let bins = self.bins();
        if out.time.len() != out.magnitude.len() || out.frequency.len() != out.magnitude.len() {
            return Err(ScoreError::LengthMismatch);
        }
        let (n, rate, hop) = (self.params.fft_size as f64, f64::from(self.sample_rate_hz), self.params.hop as f64);
        for j in 0..count {
            let k = first + j as u64;
            self.frame(input, input_start, len, k, true);
            for b in 0..bins {
                let cell = j * bins + b;
                out.magnitude[cell] = self.magnitude(b);
                let (hr, hi) = (self.re[b], self.im[b]);
                let power = hr * hr + hi * hi;
                let centre = k as f64 * hop;
                if power < 1e-24 {
                    out.time[cell] = centre;
                    out.frequency[cell] = b as f64 * rate / n;
                    continue;
                }
                // Re(X_Th conj X_h) and Im(X_Dh conj X_h).
                let t_re = self.re_t[b] * hr + self.im_t[b] * hi;
                let d_im = self.im_d[b] * hr - self.re_d[b] * hi;
                out.time[cell] = centre + t_re / power;
                out.frequency[cell] = (b as f64 / n - d_im / (2.0 * PI * power)) * rate;
            }
        }
        Ok(())
    }
}
