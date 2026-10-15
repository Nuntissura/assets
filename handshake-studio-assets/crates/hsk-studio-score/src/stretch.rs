//! Time-stretch (duration changes, pitch kept) and pitch-shift (pitch changes, duration kept) by a
//! phase vocoder with identity phase locking.
//!
//! **Phase vocoder** (M. Dolson, "The phase vocoder: a tutorial", CMJ 10(4), 1986; J. Laroche and
//! M. Dolson, "Improved phase vocoder time-scale modification of audio", IEEE TSAP 7(3), 1999;
//! re-derived, no source copied). Periodic Hann window of `N` samples, synthesis hop
//! `H_s = N/4` (75 % overlap, where the squared-window overlap sum is exactly `1.5`). Synthesis
//! frame `k` is centred on output sample `k H_s`; it is analysed from the input centred on
//! `c_a(k) = round(k H_s / factor)`, so the analysis hop `H_a(k) = c_a(k) - c_a(k-1)` is an integer
//! and the output is exactly `round(len * factor)` samples (input outside its range reads zero).
//! For a peak bin `b` (larger than its two neighbours on each side) the phase advance is the
//! measured instantaneous frequency `w_b + princarg(phi_k - phi_{k-1} - w_b H_a) / H_a` times `H_s`.
//! **Identity phase locking**: every other bin takes the phase of the peak of its region (bins
//! split at the lowest magnitude between neighbouring peaks) plus its own analysis phase offset
//! from that peak, `psi_b = psi_p + phi_b - phi_p`, which keeps the shape of each sinusoid's lobe
//! (the cause of "phasiness" in the plain vocoder). With `phase_locking = false` every bin is
//! propagated on its own (the plain vocoder, kept for comparison). Synthesis is the inverse FFT
//! times the window, overlap-added and divided by `1.5`.
//!
//! **Pitch-shift** by `s` semitones: the ratio `2^(s/12)` is approximated by a rational `r` with
//! one term 65536 (relative error below 1.3e-4, under 0.23 cent; [`pitch_ratio`] returns it); the
//! input is stretched by `r` and then played at speed `r` through the band-limited resampler
//! ([`render_at_speed`](crate::render_at_speed)), and the result is cut or zero-padded to exactly
//! the input length.
//!
//! Both are offline whole-clip functions (their output length differs from the input, so they are
//! not block [`Processor`](crate::Processor)s); all scratch is allocated once per call and the
//! frame loop allocates nothing. Deterministic. Adobe parity UNVERIFIED (D5).
use crate::denoise::Fft;
use crate::error::ScoreError;
use crate::layout::ticks_per_sample;
use crate::varispeed::{ClipSpeed, render_at_speed};
use hsk_studio_accord::CancellationToken;
use std::f64::consts::PI;

/// Stretch factor range (output duration / input duration).
pub const STRETCH_FACTOR: (f64, f64) = (0.25, 4.0);
/// Pitch-shift range in semitones.
pub const PITCH_SEMITONES: (f64, f64) = (-24.0, 24.0);
/// FFT size range (powers of two).
pub const STRETCH_FFT: (usize, usize) = (256, 16_384);
/// Largest output in samples (authored bound, about 11.6 min at 48 kHz).
pub const MAX_STRETCH_OUTPUT: usize = 1 << 25;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StretchParams {
    pub fft_size: usize,
    pub phase_locking: bool,
}

impl StretchParams {
    /// About 43 ms of window (2048 at 44.1/48 kHz, 4096 at 96 kHz), phase locking on.
    pub fn for_rate(sample_rate_hz: u32) -> Self {
        let target = f64::from(sample_rate_hz) * 0.0427;
        let fft = (target.log2().round().exp2() as usize).clamp(STRETCH_FFT.0, STRETCH_FFT.1);
        Self { fft_size: fft, phase_locking: true }
    }
}

fn princarg(x: f64) -> f64 {
    x - 2.0 * PI * (x / (2.0 * PI)).round()
}

/// Stretches `input` (one channel at `sample_rate_hz`) to `round(len * factor)` samples. Refusals:
/// rate not admitted -> `InvalidRate`; NaN/inf factor or sample -> `NotFinite`; a factor outside
/// [`STRETCH_FACTOR`], an FFT size that is not a power of two in [`STRETCH_FFT`] or an empty input
/// -> `OutOfRange`; an output above [`MAX_STRETCH_OUTPUT`] -> `CapacityExceeded` (before
/// allocating); cancellation (checked every frame) -> `Canceled`.
pub fn time_stretch(
    input: &[f32],
    sample_rate_hz: u32,
    factor: f64,
    params: StretchParams,
    cancel: &CancellationToken,
) -> Result<Vec<f32>, ScoreError> {
    ticks_per_sample(sample_rate_hz)?;
    if !factor.is_finite() {
        return Err(ScoreError::NotFinite);
    }
    let n = params.fft_size;
    if !(STRETCH_FACTOR.0..=STRETCH_FACTOR.1).contains(&factor)
        || !n.is_power_of_two()
        || !(STRETCH_FFT.0..=STRETCH_FFT.1).contains(&n)
        || input.is_empty()
    {
        return Err(ScoreError::OutOfRange);
    }
    let out_len = (input.len() as f64 * factor).round() as usize;
    if out_len > MAX_STRETCH_OUTPUT || input.len() > MAX_STRETCH_OUTPUT {
        return Err(ScoreError::CapacityExceeded);
    }
    if input.iter().any(|v| !v.is_finite()) {
        return Err(ScoreError::NotFinite);
    }
    let out_len = out_len.max(1);
    let hs = n / 4;
    let bins = n / 2 + 1;
    let fft = Fft::new(n);
    let window: Vec<f64> = (0..n).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos()).collect();
    let (mut re, mut im) = (vec![0.0; n], vec![0.0; n]);
    let (mut mag, mut phase) = (vec![0.0; bins], vec![0.0; bins]);
    let (mut prev_phase, mut synth) = (vec![0.0; bins], vec![0.0; bins]);
    let mut peaks: Vec<usize> = Vec::with_capacity(bins);
    let half = (n / 2) as i64;
    // Output buffer with N/2 of margin on each side.
    let mut ola = vec![0.0_f64; out_len + n];
    let k_first: i64 = -1;
    let k_last = ((out_len + n / 2) / hs) as i64 + 1;
    let centre = |k: i64| (k as f64 * hs as f64 / factor).round() as i64;
    let sample = |i: i64| if i < 0 || i >= input.len() as i64 { 0.0 } else { f64::from(input[i as usize]) };
    for k in k_first..=k_last {
        cancel.check().map_err(|_| ScoreError::Canceled)?;
        let ca = centre(k);
        for i in 0..n {
            re[i] = sample(ca - half + i as i64) * window[i];
            im[i] = 0.0;
        }
        fft.transform(&mut re, &mut im, false);
        for b in 0..bins {
            mag[b] = (re[b] * re[b] + im[b] * im[b]).sqrt();
            phase[b] = im[b].atan2(re[b]);
        }
        if k == k_first {
            synth.copy_from_slice(&phase);
        } else {
            let ha = (ca - centre(k - 1)) as f64;
            let advance = |b: usize, phase: &[f64], prev: &[f64]| {
                let w = 2.0 * PI * b as f64 / n as f64;
                let inst = if ha > 0.0 { w + princarg(phase[b] - prev[b] - w * ha) / ha } else { w };
                inst * hs as f64
            };
            if params.phase_locking {
                peaks.clear();
                for b in 0..bins {
                    let lo = b.saturating_sub(2);
                    let hi = (b + 2).min(bins - 1);
                    if (lo..=hi).all(|j| j == b || mag[b] > mag[j]) {
                        peaks.push(b);
                    }
                }
                if peaks.is_empty() {
                    for (b, value) in synth.iter_mut().enumerate() {
                        *value = princarg(*value + advance(b, &phase, &prev_phase));
                    }
                } else {
                    // New peak phases first (they need the previous synthesis phase of their bin);
                    // `re` is free scratch until the synthesis spectrum is built.
                    for &p in &peaks {
                        re[p] = princarg(synth[p] + advance(p, &phase, &prev_phase));
                    }
                    // Regions: bins between two peaks split at the lowest magnitude between them.
                    let mut start = 0;
                    for (i, &p) in peaks.iter().enumerate() {
                        let end = match peaks.get(i + 1) {
                            Some(&q) => (p..=q).min_by(|a, b| mag[*a].total_cmp(&mag[*b])).unwrap_or(q),
                            None => bins,
                        };
                        let psi_p = re[p];
                        for b in start..end {
                            synth[b] = psi_p + phase[b] - phase[p];
                        }
                        start = end;
                    }
                }
            } else {
                for (b, value) in synth.iter_mut().enumerate() {
                    *value = princarg(*value + advance(b, &phase, &prev_phase));
                }
            }
        }
        prev_phase.copy_from_slice(&phase);
        // Synthesis spectrum (Hermitian) and inverse transform.
        for b in 0..bins {
            let (s, c) = synth[b].sin_cos();
            re[b] = mag[b] * c;
            im[b] = mag[b] * s;
        }
        im[0] = 0.0;
        im[n / 2] = 0.0;
        for b in 1..n / 2 {
            re[n - b] = re[b];
            im[n - b] = -im[b];
        }
        fft.transform(&mut re, &mut im, true);
        let cs = k * hs as i64; // output centre
        for i in 0..n {
            let at = cs - half + i as i64 + half; // shifted by the N/2 margin
            if at >= 0 && (at as usize) < ola.len() {
                ola[at as usize] += re[i] * window[i];
            }
        }
    }
    Ok(ola[n / 2..n / 2 + out_len].iter().map(|v| (v / 1.5) as f32).collect())
}

/// The rational pitch ratio used for `semitones` (one term 65536).
pub fn pitch_ratio(semitones: f64) -> Result<ClipSpeed, ScoreError> {
    if !semitones.is_finite() {
        return Err(ScoreError::NotFinite);
    }
    if !(PITCH_SEMITONES.0..=PITCH_SEMITONES.1).contains(&semitones) {
        return Err(ScoreError::OutOfRange);
    }
    let beta = (semitones / 12.0).exp2();
    let full = 65_536_u64;
    if beta >= 1.0 {
        ClipSpeed::new(full, ((full as f64 / beta).round() as u64).max(1), false)
    } else {
        ClipSpeed::new(((full as f64 * beta).round() as u64).max(1), full, false)
    }
}

/// Shifts pitch by `semitones` keeping the duration: output has exactly `input.len()` samples.
/// Refusals as [`time_stretch`] plus NaN/inf or out-of-range semitones.
pub fn pitch_shift(
    input: &[f32],
    sample_rate_hz: u32,
    semitones: f64,
    params: StretchParams,
    cancel: &CancellationToken,
) -> Result<Vec<f32>, ScoreError> {
    let speed = pitch_ratio(semitones)?;
    let stretched = time_stretch(input, sample_rate_hz, speed.ratio(), params, cancel)?;
    let mut played = vec![0.0_f32; speed.output_frames(stretched.len() as u64) as usize];
    render_at_speed(&stretched, 0, stretched.len(), speed, &mut played)?;
    played.resize(input.len(), 0.0);
    Ok(played)
}
