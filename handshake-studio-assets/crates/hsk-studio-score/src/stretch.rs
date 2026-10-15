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
    let mut engine = FrameEngine::new(n, params.phase_locking);
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
        let ha = if k == k_first { None } else { Some((ca - centre(k - 1)) as f64) };
        engine.frame(&sample, ca, ha);
        let cs = k * hs as i64; // output centre
        for i in 0..n {
            let at = cs - half + i as i64 + half; // shifted by the N/2 margin
            if at >= 0 && (at as usize) < ola.len() {
                ola[at as usize] += engine.out[i];
            }
        }
    }
    Ok(ola[n / 2..n / 2 + out_len].iter().map(|v| (v / 1.5) as f32).collect())
}

/// One phase-vocoder frame: analysis at a centre, phase propagation (or reset), synthesis into
/// `out` (inverse FFT times the window, before the 1/1.5 overlap-add normalisation). Shared by
/// [`time_stretch`] and [`StreamingStretch`], so both compute identical frames.
struct FrameEngine {
    n: usize,
    phase_locking: bool,
    fft: Fft,
    window: Vec<f64>,
    re: Vec<f64>,
    im: Vec<f64>,
    mag: Vec<f64>,
    prev_mag: Vec<f64>,
    phase: Vec<f64>,
    prev_phase: Vec<f64>,
    synth: Vec<f64>,
    peaks: Vec<usize>,
    out: Vec<f64>,
}

impl FrameEngine {
    fn new(n: usize, phase_locking: bool) -> Self {
        let bins = n / 2 + 1;
        Self {
            n,
            phase_locking,
            fft: Fft::new(n),
            window: (0..n).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos()).collect(),
            re: vec![0.0; n],
            im: vec![0.0; n],
            mag: vec![0.0; bins],
            prev_mag: vec![0.0; bins],
            phase: vec![0.0; bins],
            prev_phase: vec![0.0; bins],
            synth: vec![0.0; bins],
            peaks: Vec::with_capacity(bins),
            out: vec![0.0; n],
        }
    }

    /// Analyses the frame centred on input sample `ca` and returns the positive spectral flux
    /// `sum max(0, |X_k| - |X_{k-1}|) / sum |X_k|` against the previous frame (0 for silence).
    fn analyse(&mut self, sample: &dyn Fn(i64) -> f64, ca: i64) -> f64 {
        let (n, half) = (self.n, (self.n / 2) as i64);
        for i in 0..n {
            self.re[i] = sample(ca - half + i as i64) * self.window[i];
            self.im[i] = 0.0;
        }
        self.fft.transform(&mut self.re, &mut self.im, false);
        self.prev_mag.copy_from_slice(&self.mag);
        let (mut rise, mut total) = (0.0, 0.0);
        for b in 0..n / 2 + 1 {
            let (r, i) = (self.re[b], self.im[b]);
            self.mag[b] = (r * r + i * i).sqrt();
            self.phase[b] = i.atan2(r);
            rise += (self.mag[b] - self.prev_mag[b]).max(0.0);
            total += self.mag[b];
        }
        if total > 0.0 { rise / total } else { 0.0 }
    }

    /// Propagates phases with analysis hop `ha` (`None` = phase reset: synthesis phase = analysis
    /// phase) and synthesises into `out`.
    fn synthesise(&mut self, ha: Option<f64>) {
        let (n, bins, hs) = (self.n, self.n / 2 + 1, (self.n / 4) as f64);
        match ha {
            None => self.synth.copy_from_slice(&self.phase),
            Some(ha) => {
                let (phase, prev) = (&self.phase, &self.prev_phase);
                let advance = |b: usize| {
                    let w = 2.0 * PI * b as f64 / n as f64;
                    let inst = if ha > 0.0 { w + princarg(phase[b] - prev[b] - w * ha) / ha } else { w };
                    inst * hs
                };
                if self.phase_locking {
                    self.peaks.clear();
                    for b in 0..bins {
                        let lo = b.saturating_sub(2);
                        let hi = (b + 2).min(bins - 1);
                        if (lo..=hi).all(|j| j == b || self.mag[b] > self.mag[j]) {
                            self.peaks.push(b);
                        }
                    }
                    if self.peaks.is_empty() {
                        for (b, value) in self.synth.iter_mut().enumerate() {
                            *value = princarg(*value + advance(b));
                        }
                    } else {
                        // New peak phases first (they need the previous synthesis phase of their
                        // bin); `re` is free scratch until the synthesis spectrum is built.
                        for &p in &self.peaks {
                            self.re[p] = princarg(self.synth[p] + advance(p));
                        }
                        // Regions: bins between two peaks split at the lowest magnitude between them.
                        let mut start = 0;
                        for (i, &p) in self.peaks.iter().enumerate() {
                            let end = match self.peaks.get(i + 1) {
                                Some(&q) => (p..=q).min_by(|a, b| self.mag[*a].total_cmp(&self.mag[*b])).unwrap_or(q),
                                None => bins,
                            };
                            let psi_p = self.re[p];
                            for b in start..end {
                                self.synth[b] = psi_p + phase[b] - phase[p];
                            }
                            start = end;
                        }
                    }
                } else {
                    for (b, value) in self.synth.iter_mut().enumerate() {
                        *value = princarg(*value + advance(b));
                    }
                }
            }
        }
        self.prev_phase.copy_from_slice(&self.phase);
        // Synthesis spectrum (Hermitian) and inverse transform.
        for b in 0..bins {
            let (s, c) = self.synth[b].sin_cos();
            self.re[b] = self.mag[b] * c;
            self.im[b] = self.mag[b] * s;
        }
        self.im[0] = 0.0;
        self.im[n / 2] = 0.0;
        for b in 1..n / 2 {
            self.re[n - b] = self.re[b];
            self.im[n - b] = -self.im[b];
        }
        self.fft.transform(&mut self.re, &mut self.im, true);
        for i in 0..n {
            self.out[i] = self.re[i] * self.window[i];
        }
    }

    fn frame(&mut self, sample: &dyn Fn(i64) -> f64, ca: i64, ha: Option<f64>) -> f64 {
        let flux = self.analyse(sample, ca);
        self.synthesise(ha);
        flux
    }
}

/// Onset threshold on the positive spectral flux (authored).
pub const TRANSIENT_FLUX_THRESHOLD: f64 = 0.3;

/// Streaming (push/pull) time-stretch with transient preservation.
///
/// The same phase vocoder as [`time_stretch`] (frames centred on output `k H_s` and analysed at
/// input `round(k H_s / factor)`), run incrementally: [`push`](Self::push) accepts input into a
/// bounded ring, [`pull`](Self::pull) processes every frame whose input is available and returns
/// finished output, [`finish`](Self::finish) declares the end of the input (zeros after it) and the
/// output then ends at exactly `round(total_input * factor)` samples. All buffers are allocated in
/// [`StreamingStretch::new`]; push and pull allocate nothing. With `transients` off the output is
/// bit-identical to [`time_stretch`] for any push/pull partition (tested).
///
/// **Transient preservation** (phase reset at onsets: C. Duxbury, M. Davies, M. Sandler, "Improved
/// time-scaling of musical audio using phase locking at transients", AES 112, 2002; A. Roebel, "A
/// new approach to transient processing in the phase vocoder", DAFx 2003; re-derived). A frame
/// whose positive spectral flux exceeds [`TRANSIENT_FLUX_THRESHOLD`] is an onset: its synthesis
/// phases are reset to its analysis phases, and the next four frames (up to the first whose window
/// starts past the onset) advance the input by exactly `H_s`, so the onset region is resynthesised
/// one to one instead of being smeared over the window. Later frames then move the
/// analysis position back toward its target `round(k H_s / factor)` by at most twice the nominal
/// hop per frame (and never backwards), so the long-term ratio is kept.
pub struct StreamingStretch {
    factor: f64,
    hs: usize,
    n: usize,
    transients: bool,
    engine: FrameEngine,
    input: Vec<f64>,
    in_count: u64,
    finished: bool,
    output: Vec<f64>,
    emitted: u64,
    ready: u64,
    next_k: i64,
    prev_centre: i64,
    transient_left: u32,
    onsets: u64,
}

impl StreamingStretch {
    /// Refusals as [`time_stretch`] (rate, factor, FFT size).
    pub fn new(sample_rate_hz: u32, factor: f64, params: StretchParams, transients: bool) -> Result<Self, ScoreError> {
        ticks_per_sample(sample_rate_hz)?;
        if !factor.is_finite() {
            return Err(ScoreError::NotFinite);
        }
        let n = params.fft_size;
        if !(STRETCH_FACTOR.0..=STRETCH_FACTOR.1).contains(&factor) || !n.is_power_of_two() || !(STRETCH_FFT.0..=STRETCH_FFT.1).contains(&n) {
            return Err(ScoreError::OutOfRange);
        }
        let hs = n / 4;
        // Input ring: the N samples of a frame plus room for the largest catch-up hops.
        let max_hop = 2 * ((hs as f64 / factor).ceil() as usize) + hs;
        let in_cap = (2 * n + 4 * max_hop).next_power_of_two();
        Ok(Self {
            factor,
            hs,
            n,
            transients,
            engine: FrameEngine::new(n, params.phase_locking),
            input: vec![0.0; in_cap],
            in_count: 0,
            finished: false,
            output: vec![0.0; (2 * n).next_power_of_two()],
            emitted: 0,
            ready: 0,
            next_k: -1,
            prev_centre: 0,
            transient_left: 0,
            onsets: 0,
        })
    }
    /// Onsets detected so far.
    pub fn onsets(&self) -> u64 {
        self.onsets
    }
    /// Output samples that exist in total once the input is finished.
    fn total_output(&self) -> Option<u64> {
        self.finished.then(|| ((self.in_count as f64 * self.factor).round() as u64).max(1))
    }
    fn target(&self, k: i64) -> i64 {
        (k as f64 * self.hs as f64 / self.factor).round() as i64
    }
    /// Analysis centre of the next frame.
    fn next_centre(&self) -> i64 {
        let k = self.next_k;
        if k == -1 {
            return self.target(-1);
        }
        if self.transient_left > 0 {
            return self.prev_centre + self.hs as i64;
        }
        let nominal = (self.hs as f64 / self.factor).ceil() as i64;
        let step = (self.target(k) - self.prev_centre).clamp(0, 2 * nominal);
        if self.transients { self.prev_centre + step } else { self.target(k) }
    }
    /// Oldest input sample still needed.
    fn oldest_needed(&self) -> i64 {
        self.next_centre() - (self.n / 2) as i64
    }
    /// Accepts input samples while the ring has room; returns how many were taken. Refuses
    /// NaN/inf with `NotFinite` (nothing taken) and pushes after `finish` with `OutOfRange`.
    pub fn push(&mut self, samples: &[f32]) -> Result<usize, ScoreError> {
        if self.finished {
            return Err(ScoreError::OutOfRange);
        }
        if samples.iter().any(|v| !v.is_finite()) {
            return Err(ScoreError::NotFinite);
        }
        let cap = self.input.len() as i64;
        let oldest = self.oldest_needed().max(0);
        let room = (oldest + cap - self.in_count as i64).max(0) as usize;
        let take = room.min(samples.len());
        for v in &samples[..take] {
            let slot = (self.in_count % cap as u64) as usize;
            self.input[slot] = f64::from(*v);
            self.in_count += 1;
        }
        Ok(take)
    }
    /// Declares the end of the input.
    pub fn finish(&mut self) {
        self.finished = true;
    }
    /// Processes every frame that can be processed and copies finished output into `out`;
    /// returns the number of samples written (0 when more input is needed or the output ended).
    pub fn pull(&mut self, out: &mut [f32], cancel: &CancellationToken) -> Result<usize, ScoreError> {
        let (n, hs) = (self.n, self.hs as i64);
        let half = (n / 2) as i64;
        let out_cap = self.output.len() as i64;
        loop {
            let available = self.ready - self.emitted;
            if available as usize >= out.len() {
                break;
            }
            if self.total_output().is_some_and(|total| self.ready >= total) {
                break;
            }
            let ca = self.next_centre();
            let k = self.next_k;
            if !self.finished && ca + half > self.in_count as i64 {
                break; // needs more input
            }
            // The frame writes output samples k Hs - N/2 .. k Hs + N/2.
            if k * hs + half - self.emitted as i64 > out_cap {
                break;
            }
            cancel.check().map_err(|_| ScoreError::Canceled)?;
            let (cap, count, finished, input) = (self.input.len() as i64, self.in_count as i64, self.finished, &self.input);
            let sample = |i: i64| {
                if i < 0 || i >= count {
                    0.0
                } else {
                    debug_assert!(finished || i >= count - cap);
                    input[(i % cap) as usize]
                }
            };
            let flux = self.engine.analyse(&sample, ca);
            let onset = self.transients && k > -1 && self.transient_left == 0 && flux > TRANSIENT_FLUX_THRESHOLD;
            let ha = if k == -1 || onset { None } else { Some((ca - self.prev_centre) as f64) };
            if onset {
                self.onsets += 1;
                // Frames k+1 ..= k+4 also advance by H_s: frame k+4 is the first whose window
                // starts past the onset region of frame k (centre + 4 H_s - N/2 = centre + N/2).
                self.transient_left = 5;
            }
            self.engine.synthesise(ha);
            for i in 0..n as i64 {
                let at = k * hs - half + i;
                if at >= self.emitted as i64 {
                    self.output[(at % out_cap) as usize] += self.engine.out[i as usize];
                }
            }
            self.transient_left = self.transient_left.saturating_sub(1);
            self.prev_centre = ca;
            self.next_k += 1;
            self.ready = ((k + 1) * hs - half).max(0) as u64;
        }
        let mut available = self.ready - self.emitted;
        if let Some(total) = self.total_output() {
            available = available.min(total.saturating_sub(self.emitted));
        }
        let count = (available as usize).min(out.len());
        for o in out.iter_mut().take(count) {
            let slot = (self.emitted % out_cap as u64) as usize;
            *o = (self.output[slot] / 1.5) as f32;
            self.output[slot] = 0.0;
            self.emitted += 1;
        }
        Ok(count)
    }
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
