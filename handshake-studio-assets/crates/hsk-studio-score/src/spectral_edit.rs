//! Spectral selection (time x frequency) with attenuate and heal.
//!
//! **STFT.** Periodic Hann analysis window `w` of `N` samples, hop `H = N/4`, frame `k` centred on
//! sample `k H`; synthesis window `w / 1.5`, so `sum_k w(n - kH) w(n - kH) / 1.5 = 1` and an
//! unmodified STFT resynthesises the input.
//!
//! **Selection.** Samples `start .. end` and frequencies `low_hz ..= high_hz`. A cell (frame `k`,
//! bin `b`) is selected when `k H` lies in `start .. end` and `b rate / N` lies in the band.
//!
//! **Edit as a difference.** Only the change `D_k(b) = X'_k(b) - X_k(b)` of selected cells is
//! resynthesised and added: `out[n] = in[n] + sum_k (w/1.5)(n - kH) IFFT(D_k)(n - kH)`. Outside
//! the samples `first H - N/2 .. last H + N/2` covered by edited frames the output is the input
//! bit for bit (it is copied); inside, unselected frequencies are disturbed only by the window
//! leakage of the edit (measured in `tests/spectral_edit.rs`).
//!
//! * **Attenuate** by `g` dB: `X' = 10^(g/20) X` (a full-band selection over whole frames gives
//!   exactly `10^(g/20) x` in the interior, by the overlap-add identity).
//! * **Heal**: per selected bin, the magnitude is interpolated linearly in log magnitude between
//!   the frames three hops outside the selection on each side (their windows do not reach the
//!   selected samples' centres), and the original phase is kept: `X' = M X / |X|`. This replaces
//!   a short time-frequency defect by its surroundings (the method of spectral interpolation
//!   repair; Audition's healing algorithm is UNVERIFIED, D5).
//!
//! Offline whole-clip function: scratch is allocated per call (a few `N` buffers plus the edited
//! span), bounded by [`MAX_SPECTRAL_EDIT_SPAN`] and checked first. Deterministic.
use crate::denoise::Fft;
use crate::error::ScoreError;
use crate::layout::ticks_per_sample;
use hsk_studio_accord::CancellationToken;
use std::f64::consts::PI;

/// Longest edited span in samples (authored bound).
pub const MAX_SPECTRAL_EDIT_SPAN: u64 = 1 << 25;
/// FFT size range (powers of two).
pub const SPECTRAL_EDIT_FFT: (usize, usize) = (256, 16_384);
/// Attenuation range in dB.
pub const SPECTRAL_ATTENUATION_DB: (f64, f64) = (-120.0, 0.0);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpectralSelection {
    pub start_sample: u64,
    pub end_sample: u64,
    pub low_hz: f64,
    pub high_hz: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SpectralOp {
    Attenuate { gain_db: f64 },
    Heal,
}

/// What an edit touched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpectralEditReport {
    /// Samples `[start, end)` that may differ from the input; all others are copied.
    pub affected_start: u64,
    pub affected_end: u64,
    pub frames: u64,
    pub cells: u64,
}

/// Applies `op` to `selection` of `input` (one channel) into `out` (same length). Refusals: rate
/// not admitted -> `InvalidRate`; NaN/inf -> `NotFinite`; empty or reversed selection, a band
/// outside `0..=rate/2`, an FFT size not a power of two in [`SPECTRAL_EDIT_FFT`], an attenuation
/// outside [`SPECTRAL_ATTENUATION_DB`] or a selection past the input -> `OutOfRange`; unequal
/// lengths -> `LengthMismatch`; an edited span above [`MAX_SPECTRAL_EDIT_SPAN`] ->
/// `CapacityExceeded`; cancellation (checked per frame) -> `Canceled`.
pub fn spectral_edit(
    input: &[f32],
    out: &mut [f32],
    sample_rate_hz: u32,
    fft_size: usize,
    selection: SpectralSelection,
    op: SpectralOp,
    cancel: &CancellationToken,
) -> Result<SpectralEditReport, ScoreError> {
    ticks_per_sample(sample_rate_hz)?;
    let n = fft_size;
    if !n.is_power_of_two() || !(SPECTRAL_EDIT_FFT.0..=SPECTRAL_EDIT_FFT.1).contains(&n) {
        return Err(ScoreError::OutOfRange);
    }
    if out.len() != input.len() {
        return Err(ScoreError::LengthMismatch);
    }
    let rate = f64::from(sample_rate_hz);
    let s = selection;
    if !s.low_hz.is_finite() || !s.high_hz.is_finite() {
        return Err(ScoreError::NotFinite);
    }
    if s.start_sample >= s.end_sample || s.end_sample > input.len() as u64 || s.low_hz < 0.0 || s.high_hz < s.low_hz || s.high_hz > rate / 2.0 {
        return Err(ScoreError::OutOfRange);
    }
    let gain = match op {
        SpectralOp::Attenuate { gain_db } => {
            if !gain_db.is_finite() {
                return Err(ScoreError::NotFinite);
            }
            if !(SPECTRAL_ATTENUATION_DB.0..=SPECTRAL_ATTENUATION_DB.1).contains(&gain_db) {
                return Err(ScoreError::OutOfRange);
            }
            Some(10f64.powf(gain_db / 20.0))
        }
        SpectralOp::Heal => None,
    };
    if input.iter().any(|v| !v.is_finite()) {
        return Err(ScoreError::NotFinite);
    }
    let hop = (n / 4) as u64;
    let half = (n / 2) as i64;
    let len = input.len() as u64;
    out.copy_from_slice(input);
    let first = s.start_sample.div_ceil(hop);
    let last_excl = s.end_sample.div_ceil(hop);
    let bins = n / 2 + 1;
    let band: Vec<usize> = (0..bins).filter(|b| (s.low_hz..=s.high_hz).contains(&(*b as f64 * rate / n as f64))).collect();
    if first >= last_excl || band.is_empty() {
        return Ok(SpectralEditReport { affected_start: 0, affected_end: 0, frames: 0, cells: 0 });
    }
    let affected_start = (first * hop).saturating_sub(half as u64);
    let affected_end = ((last_excl - 1) * hop + half as u64).min(len);
    if affected_end - affected_start > MAX_SPECTRAL_EDIT_SPAN {
        return Err(ScoreError::CapacityExceeded);
    }
    let fft = Fft::new(n);
    let window: Vec<f64> = (0..n).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos()).collect();
    let (mut re, mut im) = (vec![0.0; n], vec![0.0; n]);
    let analyse = |k: i64, re: &mut [f64], im: &mut [f64]| {
        for i in 0..n {
            let at = k * hop as i64 - half + i as i64;
            let x = if at < 0 || at >= len as i64 { 0.0 } else { f64::from(input[at as usize]) };
            re[i] = x * window[i];
            im[i] = 0.0;
        }
        fft.transform(re, im, false);
    };
    // Heal: log magnitudes of the boundary frames three hops outside the selection.
    let frame_count = len.div_ceil(hop) as i64;
    let (k_left, k_right) = ((first as i64 - 3).max(0), (last_excl as i64 + 2).min(frame_count.max(1) - 1));
    let mut left = vec![0.0; bins];
    let mut right = vec![0.0; bins];
    if gain.is_none() {
        for (k, store) in [(k_left, &mut left), (k_right, &mut right)] {
            analyse(k, &mut re, &mut im);
            for b in 0..bins {
                store[b] = (re[b] * re[b] + im[b] * im[b]).sqrt().max(1e-12).ln();
            }
        }
    }
    let mut delta = vec![0.0_f64; (affected_end - affected_start) as usize];
    let mut cells = 0u64;
    let (mut dre, mut dim) = (vec![0.0; n], vec![0.0; n]);
    for k in first..last_excl {
        cancel.check().map_err(|_| ScoreError::Canceled)?;
        analyse(k as i64, &mut re, &mut im);
        dre.fill(0.0);
        dim.fill(0.0);
        for &b in &band {
            let (xr, xi) = (re[b], im[b]);
            let (tr, ti) = match gain {
                Some(g) => (g * xr, g * xi),
                None => {
                    let t = (k as f64 - k_left as f64) / ((k_right - k_left).max(1) as f64);
                    let m = (left[b] + t * (right[b] - left[b])).exp();
                    let mag = (xr * xr + xi * xi).sqrt();
                    if mag > 0.0 { (m * xr / mag, m * xi / mag) } else { (m, 0.0) }
                }
            };
            dre[b] = tr - xr;
            dim[b] = ti - xi;
            cells += 1;
        }
        dim[0] = 0.0;
        dim[n / 2] = 0.0;
        for b in 1..n / 2 {
            dre[n - b] = dre[b];
            dim[n - b] = -dim[b];
        }
        fft.transform(&mut dre, &mut dim, true);
        for i in 0..n {
            let at = k as i64 * hop as i64 - half + i as i64;
            if at >= affected_start as i64 && at < affected_end as i64 {
                delta[(at as u64 - affected_start) as usize] += dre[i] * window[i] / 1.5;
            }
        }
    }
    for (o, d) in out[affected_start as usize..affected_end as usize].iter_mut().zip(&delta) {
        *o = (f64::from(*o) + d) as f32;
    }
    if out.iter().any(|v| !v.is_finite()) {
        out.copy_from_slice(input);
        return Err(ScoreError::NotFinite);
    }
    Ok(SpectralEditReport { affected_start, affected_end, frames: last_excl - first, cells })
}
