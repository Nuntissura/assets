//! Spectral selection editing with marquee and lasso regions (lane SMB-AU-WAVEFORM-EDITOR, parity
//! row AU-12 / AU-20): attenuate, remove or heal the time-frequency cells inside a region, then
//! resynthesise by inverse STFT with weighted overlap-add.
//!
//! **STFT.** Periodic analysis window `w` ([`SpectralWindow`], default Hann) of `N` samples, hop
//! `H = N/4`, frame `k >= 0` centred on sample `k H` (zero outside the clip), `N/2 + 1` bins, bin `b`
//! at `b rate / N` Hz.
//!
//! **Region.** A cell (frame `k`, bin `b`) is selected when the point `(k H, b rate / N)` (samples, Hz)
//! lies inside the region: a [`SpectralRegion::Marquee`] (`start <= kH < end`, `low <= f <= high`) or
//! a [`SpectralRegion::Lasso`] polygon of `(sample, Hz)` vertices, closed implicitly, by the even-odd
//! rule (crossing number of a ray towards +time; a point on a horizontal edge counts with the lower
//! vertex rule `(y_i > y) != (y_j > y)`).
//!
//! **Resynthesis.** The selected frames are `k_first ..= k_last`; every frame `k_first - 3 ..=
//! k_last + 3` (from 0) is analysed, modified where selected and resynthesised:
//! `y[n] = sum_k w(n - kH) IFFT(Y_k)(n - kH) / sum_k w(n - kH)^2` over the samples
//! `affected = [k_first H - N/2, k_last H + N/2)` (clipped to the clip) that edited frames reach;
//! every other sample is copied bit for bit. With every `Y_k = X_k` this weighted overlap-add is
//! the identity (`sum w^2 x / sum w^2 = x`): [`SpectralMaskOp::Identity`] measures the
//! reconstruction error (tested <= 1e-6 for every window).
//!
//! * **Attenuate** by `g` dB (`-120 ..= 0`): `Y = 10^(g/20) X`.
//! * **Remove**: `Y = 0`.
//! * **Heal**: per bin, each run of selected frames `a ..= z` takes its magnitude linearly
//!   interpolated in log magnitude between the unmodified frames `a - 3` and `z + 3` (whose windows
//!   do not reach the run's frame centres), keeping each cell's phase; at the clip start (no frame
//!   `a - 3`) the right anchor alone is used (counted in [`SpectralMaskReport::one_sided`]).
//!
//! Bounds checked before any allocation: affected span <= [`MAX_SPECTRAL_EDIT_SPAN`], analysed cells
//! <= [`MAX_SPECTRAL_MASK_CELLS`], lasso vertices 3 ..= [`MAX_LASSO_POINTS`]. Cancellation is
//! checked per frame; a cancel leaves `out` equal to the input. Audition's spectral healing and
//! lasso semantics are UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]

use crate::denoise::Fft;
use crate::error::ScoreError;
use crate::layout::ticks_per_sample;
use crate::spectral_edit::{MAX_SPECTRAL_EDIT_SPAN, SPECTRAL_ATTENUATION_DB, SPECTRAL_EDIT_FFT};
use crate::spectrogram::SpectralWindow;
use hsk_studio_accord::CancellationToken;

/// Most lasso vertices.
pub const MAX_LASSO_POINTS: usize = 4096;
/// Most analysed cells (frames x bins) of one edit: 2^23 complex f64 = 128 MiB.
pub const MAX_SPECTRAL_MASK_CELLS: usize = 1 << 23;

/// A time-frequency region (samples, Hz).
#[derive(Clone, Debug, PartialEq)]
pub enum SpectralRegion {
    Marquee { start_sample: u64, end_sample: u64, low_hz: f64, high_hz: f64 },
    /// Polygon vertices `(sample, Hz)`.
    Lasso(Vec<(f64, f64)>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SpectralMaskOp {
    /// Resynthesis without change (reconstruction check).
    Identity,
    Attenuate { gain_db: f64 },
    Remove,
    Heal,
}

/// What an edit touched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpectralMaskReport {
    /// Samples `[start, end)` that were resynthesised; all others are copied.
    pub affected_start: u64,
    pub affected_end: u64,
    /// Frames holding a selected cell, and selected cells.
    pub frames: u64,
    pub cells: u64,
    /// Heal runs that had only one anchor frame.
    pub one_sided: u64,
}

impl SpectralRegion {
    fn check(&self, len: u64, nyquist: f64) -> Result<(), ScoreError> {
        match self {
            Self::Marquee { start_sample, end_sample, low_hz, high_hz } => {
                if !low_hz.is_finite() || !high_hz.is_finite() {
                    return Err(ScoreError::NotFinite);
                }
                if start_sample >= end_sample || *end_sample > len || *low_hz < 0.0 || high_hz < low_hz || *high_hz > nyquist {
                    return Err(ScoreError::OutOfRange);
                }
            }
            Self::Lasso(points) => {
                if points.len() < 3 || points.len() > MAX_LASSO_POINTS {
                    return Err(ScoreError::OutOfRange);
                }
                if points.iter().any(|(t, f)| !t.is_finite() || !f.is_finite()) {
                    return Err(ScoreError::NotFinite);
                }
                if points.iter().any(|(t, f)| *t < 0.0 || *t > len as f64 || *f < 0.0 || *f > nyquist) {
                    return Err(ScoreError::OutOfRange);
                }
            }
        }
        Ok(())
    }

    /// Sample span `[min, max]` of the region.
    fn time_span(&self) -> (f64, f64) {
        match self {
            Self::Marquee { start_sample, end_sample, .. } => (*start_sample as f64, *end_sample as f64),
            Self::Lasso(p) => p.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), (t, _)| (lo.min(*t), hi.max(*t))),
        }
    }

    /// Whether the point `(t samples, f Hz)` is inside (module docs).
    pub fn contains(&self, t: f64, f: f64) -> bool {
        match self {
            Self::Marquee { start_sample, end_sample, low_hz, high_hz } => t >= *start_sample as f64 && t < *end_sample as f64 && f >= *low_hz && f <= *high_hz,
            Self::Lasso(p) => {
                let mut inside = false;
                let mut j = p.len().saturating_sub(1);
                for (i, (ti, fi)) in p.iter().enumerate() {
                    let (tj, fj) = p[j];
                    if (*fi > f) != (fj > f) && t < (tj - ti) * (f - fi) / (fj - fi) + ti {
                        inside = !inside;
                    }
                    j = i;
                }
                inside
            }
        }
    }
}

/// Applies `op` to the cells of `region` in `input` (one channel) into `out` (same length), module
/// docs. Refusals: rate not admitted -> `InvalidRate`; NaN/inf -> `NotFinite`; FFT size not a power
/// of two in `SPECTRAL_EDIT_FFT`, a region outside the clip or `0 ..= rate/2`, a lasso with fewer than
/// 3 or more than [`MAX_LASSO_POINTS`] vertices, an attenuation outside `SPECTRAL_ATTENUATION_DB`
/// -> `OutOfRange`; unequal lengths -> `LengthMismatch`; the span or cell bound exceeded ->
/// `CapacityExceeded`; cancellation -> `Canceled` (out = input).
#[allow(clippy::too_many_arguments)]
pub fn spectral_mask_edit(
    input: &[f32],
    out: &mut [f32],
    sample_rate_hz: u32,
    fft_size: usize,
    window: SpectralWindow,
    region: &SpectralRegion,
    op: SpectralMaskOp,
    cancel: &CancellationToken,
) -> Result<SpectralMaskReport, ScoreError> {
    ticks_per_sample(sample_rate_hz)?;
    let n = fft_size;
    if !n.is_power_of_two() || !(SPECTRAL_EDIT_FFT.0..=SPECTRAL_EDIT_FFT.1).contains(&n) {
        return Err(ScoreError::OutOfRange);
    }
    if out.len() != input.len() {
        return Err(ScoreError::LengthMismatch);
    }
    let rate = f64::from(sample_rate_hz);
    let len = input.len() as u64;
    region.check(len, rate / 2.0)?;
    let gain = match op {
        SpectralMaskOp::Attenuate { gain_db } => {
            if !gain_db.is_finite() {
                return Err(ScoreError::NotFinite);
            }
            if !(SPECTRAL_ATTENUATION_DB.0..=SPECTRAL_ATTENUATION_DB.1).contains(&gain_db) {
                return Err(ScoreError::OutOfRange);
            }
            Some(10f64.powf(gain_db / 20.0))
        }
        SpectralMaskOp::Remove => Some(0.0),
        SpectralMaskOp::Identity => Some(1.0),
        SpectralMaskOp::Heal => None,
    };
    if input.iter().any(|v| !v.is_finite()) {
        return Err(ScoreError::NotFinite);
    }
    out.copy_from_slice(input);
    let hop = (n / 4) as u64;
    let half = (n / 2) as u64;
    let bins = n.checked_div(2).and_then(|h| h.checked_add(1)).ok_or(ScoreError::OutOfRange)?;
    let bin_hz = rate / n as f64;
    // Candidate frames: centres within the region's time span.
    let (t0, t1) = region.time_span();
    let k_lo = (t0.max(0.0) / hop as f64).ceil() as u64;
    let k_hi = ((t1.max(0.0) / hop as f64).floor() as u64).min(len.div_ceil(hop));
    let empty = SpectralMaskReport { affected_start: 0, affected_end: 0, frames: 0, cells: 0, one_sided: 0 };
    if k_lo > k_hi {
        return Ok(empty);
    }
    // Selected cells per candidate frame (bounded: checked against the cell budget first).
    let candidates = usize::try_from(k_hi.saturating_sub(k_lo).saturating_add(1)).map_err(|_| ScoreError::CapacityExceeded)?;
    if candidates.checked_mul(bins).is_none_or(|c| c > MAX_SPECTRAL_MASK_CELLS) {
        return Err(ScoreError::CapacityExceeded);
    }
    let mut selected = vec![false; candidates.saturating_mul(bins)];
    let (mut first, mut last, mut cells) = (None::<u64>, 0u64, 0u64);
    for (j, row) in selected.chunks_mut(bins).enumerate() {
        let k = k_lo.saturating_add(j as u64);
        let t = k.saturating_mul(hop) as f64;
        for (b, cell) in row.iter_mut().enumerate() {
            if region.contains(t, b as f64 * bin_hz) {
                *cell = true;
                cells = cells.saturating_add(1);
                first.get_or_insert(k);
                last = k;
            }
        }
    }
    let Some(k_first) = first else { return Ok(empty) };
    let k_last = last;
    let affected_start = k_first.saturating_mul(hop).saturating_sub(half);
    let affected_end = k_last.saturating_mul(hop).saturating_add(half).min(len);
    if affected_end.saturating_sub(affected_start) > MAX_SPECTRAL_EDIT_SPAN {
        return Err(ScoreError::CapacityExceeded);
    }
    let a_first = k_first.saturating_sub(3);
    let a_last = k_last.saturating_add(3);
    let analysed = usize::try_from(a_last.saturating_sub(a_first).saturating_add(1)).map_err(|_| ScoreError::CapacityExceeded)?;
    if analysed.checked_mul(bins).is_none_or(|c| c > MAX_SPECTRAL_MASK_CELLS) {
        return Err(ScoreError::CapacityExceeded);
    }
    let is_selected = |k: u64, b: usize| -> bool {
        k.checked_sub(k_lo)
            .and_then(|j| usize::try_from(j).ok())
            .filter(|j| *j < candidates)
            .and_then(|j| j.checked_mul(bins)?.checked_add(b))
            .and_then(|i| selected.get(i).copied())
            .unwrap_or(false)
    };
    let fft = Fft::new(n);
    let w = window.samples(n);
    let (mut re, mut im) = (vec![0.0_f64; n], vec![0.0_f64; n]);
    let frame_start = |k: u64| -> i128 { i128::from(k).saturating_mul(i128::from(hop)).saturating_sub(i128::from(half)) };
    // Analysis of every frame in range (spectra kept: bins per frame).
    let mut spec_re = vec![0.0_f64; analysed.saturating_mul(bins)];
    let mut spec_im = vec![0.0_f64; analysed.saturating_mul(bins)];
    for j in 0..analysed {
        if cancel.check().is_err() {
            out.copy_from_slice(input);
            return Err(ScoreError::Canceled);
        }
        let base = frame_start(a_first.saturating_add(j as u64));
        for i in 0..n {
            let at = base.saturating_add(i as i128);
            let x = usize::try_from(at).ok().and_then(|a| input.get(a)).map_or(0.0, |v| f64::from(*v));
            re[i] = x * w[i];
            im[i] = 0.0;
        }
        fft.transform(&mut re, &mut im, false);
        let o = j.saturating_mul(bins);
        spec_re[o..o.saturating_add(bins)].copy_from_slice(&re[..bins]);
        spec_im[o..o.saturating_add(bins)].copy_from_slice(&im[..bins]);
    }
    // Modification of selected cells.
    let mut one_sided = 0u64;
    let index = |k: u64, b: usize| -> usize { usize::try_from(k.saturating_sub(a_first)).unwrap_or(usize::MAX).saturating_mul(bins).saturating_add(b) };
    match gain {
        Some(g) => {
            for k in k_first..=k_last {
                for b in 0..bins {
                    if is_selected(k, b) {
                        let i = index(k, b);
                        spec_re[i] *= g;
                        spec_im[i] *= g;
                    }
                }
            }
        }
        None => {
            let log_mag = |i: usize, re: &[f64], im: &[f64]| (re[i] * re[i] + im[i] * im[i]).sqrt().max(1e-12).ln();
            let (orig_re, orig_im) = (spec_re.clone(), spec_im.clone());
            for b in 0..bins {
                let mut k = k_first;
                while k <= k_last {
                    if !is_selected(k, b) {
                        k = k.saturating_add(1);
                        continue;
                    }
                    let a = k;
                    let mut z = k;
                    while z < k_last && is_selected(z.saturating_add(1), b) {
                        z = z.saturating_add(1);
                    }
                    let left = a.checked_sub(3).map(|l| (l, log_mag(index(l, b), &orig_re, &orig_im)));
                    let right_k = z.saturating_add(3);
                    let right = (right_k, log_mag(index(right_k, b), &orig_re, &orig_im));
                    if left.is_none() {
                        one_sided = one_sided.saturating_add(1);
                    }
                    for c in a..=z {
                        let target = match left {
                            Some((lk, lm)) => {
                                let t = c.saturating_sub(lk) as f64 / right.0.saturating_sub(lk) as f64;
                                lm + t * (right.1 - lm)
                            }
                            None => right.1,
                        }
                        .exp();
                        let i = index(c, b);
                        let mag = (orig_re[i] * orig_re[i] + orig_im[i] * orig_im[i]).sqrt();
                        if mag > 0.0 {
                            spec_re[i] = target * orig_re[i] / mag;
                            spec_im[i] = target * orig_im[i] / mag;
                        } else {
                            spec_re[i] = target;
                            spec_im[i] = 0.0;
                        }
                    }
                    k = z.saturating_add(1);
                }
            }
        }
    }
    // Weighted overlap-add over the affected samples.
    let span = usize::try_from(affected_end.saturating_sub(affected_start)).map_err(|_| ScoreError::CapacityExceeded)?;
    let mut acc = vec![0.0_f64; span];
    let mut wsum = vec![0.0_f64; span];
    for j in 0..analysed {
        if cancel.check().is_err() {
            out.copy_from_slice(input);
            return Err(ScoreError::Canceled);
        }
        let o = j.saturating_mul(bins);
        re[..bins].copy_from_slice(&spec_re[o..o.saturating_add(bins)]);
        im[..bins].copy_from_slice(&spec_im[o..o.saturating_add(bins)]);
        im[0] = 0.0;
        im[n / 2] = 0.0;
        for b in 1..n / 2 {
            re[n.saturating_sub(b)] = re[b];
            im[n.saturating_sub(b)] = -im[b];
        }
        fft.transform(&mut re, &mut im, true);
        let base = frame_start(a_first.saturating_add(j as u64));
        for i in 0..n {
            let at = base.saturating_add(i as i128).saturating_sub(i128::from(affected_start));
            if let Some(p) = usize::try_from(at).ok().filter(|p| *p < span) {
                acc[p] += re[i] * w[i];
                wsum[p] += w[i] * w[i];
            }
        }
    }
    let start = usize::try_from(affected_start).map_err(|_| ScoreError::CapacityExceeded)?;
    for (p, o) in out[start..start.saturating_add(span)].iter_mut().enumerate() {
        if wsum[p] > 0.0 {
            *o = (acc[p] / wsum[p]) as f32;
        }
    }
    if out.iter().any(|v| !v.is_finite()) {
        out.copy_from_slice(input);
        return Err(ScoreError::NotFinite);
    }
    Ok(SpectralMaskReport { affected_start, affected_end, frames: k_last.saturating_sub(k_first).saturating_add(1), cells, one_sided })
}
