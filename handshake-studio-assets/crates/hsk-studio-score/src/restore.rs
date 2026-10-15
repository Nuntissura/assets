//! Restoration: click/crackle repair by autoregressive (AR) detection and least-squares AR
//! interpolation, and hum removal by a notch cascade at a fundamental and its harmonics.
//!
//! **ClickRepair** (S. J. Godsill and P. J. W. Rayner, "Digital Audio Restoration", Springer
//! 1998, ch. 5; S. V. Vaseghi and P. J. W. Rayner, "Detection and suppression of impulsive noise
//! in speech communication systems", IEE Proc. I 137(1), 1990). The stream is cut into analysis
//! windows of `W = 2048` samples advancing by `H = 1536`; each window repairs only its centre
//! `[C, C + H)` (`C = 256` samples of context each side), so every output sample is decided by
//! exactly one window, at fixed absolute positions (block partition cannot change the result).
//! Per window and channel, in `f64`:
//!
//! 1. AR(p) fit: autocorrelation `r[0..=p]` of the window, Levinson-Durbin recursion
//!    (`a_0 = 1`, `e[n] = sum_k a_k x[n-k]` is the prediction error).
//! 2. Detection: `|e[n]| > threshold * sigma`, `sigma = 1.4826 * median|e|` (robust scale; floored
//!    at 1e-5 of the window RMS so numerically perfect fits do not flag rounding noise). Flags
//!    closer than `p` samples are merged into one gap `[first, last]`.
//! 3. Interpolation (LSAR): the gap samples `x_u` minimise `sum_n e[n]^2` over every residual
//!    that touches them, i.e. `(A_u^T A_u) x_u = -A_u^T A_k x_k`; `A_u^T A_u` is the Toeplitz
//!    matrix of the coefficient autocorrelation, solved by Cholesky. A second pass refits the AR
//!    model on the repaired window and re-solves the same gaps (Godsill and Rayner's iteration).
//!    Gaps longer than `max_gap_ms` are left untouched and counted in `skipped_gaps`.
//!
//! Samples outside repaired gaps are passed through bit-exact. Latency `H + C = 1792` samples.
//! "Crackle" is dense small clicks: the same detector with a lower `threshold`.
//!
//! **HumRemover**: cookbook notches (W3C Audio EQ Cookbook) at `k f0` for `k = 1..=harmonics`
//! (harmonics at or above `0.95 rate/2` are dropped), each with bandwidth `bandwidth_hz`
//! (`Q = k f0 / bandwidth_hz`), cascaded in `f64`; latency 0; zero response exactly at each
//! `k f0` in steady state.
//!
//! Ranges are authored: the spec rows AutoClickRemover `inf_threshold`/`inf_complexity` have no
//! declared meaning (refused by [`bind_spec_row`](crate::bind_spec_row)) and DeHummer declares 0
//! parameters (STU-FX-134a). All state is allocated at construction; `process` allocates nothing.
//! Adobe Audition restoration parity UNVERIFIED (D5).
use crate::block::{InputBlock, OutputBlock};
use crate::dsp::poll;
use crate::engine::Processor;
use crate::error::ScoreError;
use crate::filters::{Sos, SosKind, SosState};
use crate::layout::{CANCEL_CHECK_FRAMES, ChannelLayout, MAX_CHANNELS, ticks_per_sample};
use hsk_studio_accord::CancellationToken;

/// Analysis window length in samples.
pub const CLICK_WINDOW: usize = 2048;
/// Context each side of a window's repaired centre.
pub const CLICK_CONTEXT: usize = 256;
/// Window advance (= repaired centre length).
pub const CLICK_HOP: usize = CLICK_WINDOW - 2 * CLICK_CONTEXT;
/// AR model order range.
pub const CLICK_AR_ORDER: (usize, usize) = (4, 64);
/// Detection threshold in robust standard deviations.
pub const CLICK_THRESHOLD: (f64, f64) = (2.0, 100.0);
/// Longest repaired gap in samples (after converting `max_gap_ms` at the rate).
pub const CLICK_MAX_GAP_SAMPLES: usize = 128;
/// Hum fundamental range in Hz.
pub const HUM_FUNDAMENTAL_HZ: (f64, f64) = (20.0, 1000.0);
/// Notch bandwidth range in Hz.
pub const HUM_BANDWIDTH_HZ: (f64, f64) = (0.1, 100.0);
pub const MAX_HUM_HARMONICS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClickRepairParams {
    pub ar_order: usize,
    pub threshold: f64,
    pub max_gap_ms: f64,
}

impl Default for ClickRepairParams {
    /// AR(24), 8 sigma, gaps up to 1 ms.
    fn default() -> Self {
        Self { ar_order: 24, threshold: 8.0, max_gap_ms: 1.0 }
    }
}

/// Scratch for one window solve (shared by all channels).
#[derive(Debug)]
struct Scratch {
    window: Vec<f64>,
    original: Vec<f64>,
    residual: Vec<f64>,
    abs: Vec<f64>,
    r: Vec<f64>,
    a: Vec<f64>,
    tmp: Vec<f64>,
    ra: Vec<f64>,
    gaps: Vec<(usize, usize)>,
    matrix: Vec<f64>,
    rhs: Vec<f64>,
    rows: Vec<f64>,
}

/// AR click/crackle repair with least-squares AR interpolation. See the module docs.
#[derive(Debug)]
pub struct ClickRepair {
    sample_rate_hz: u32,
    channels: usize,
    params: ClickRepairParams,
    max_gap: usize,
    hist: Vec<f64>,
    centre: Vec<f32>,
    fill: usize,
    scratch: Scratch,
    repaired_gaps: u64,
    skipped_gaps: u64,
    identity: String,
}

impl ClickRepair {
    /// Refusals: rate not admitted -> `InvalidRate`; channels out of `1..=MAX_CHANNELS` ->
    /// `InvalidLayout`; NaN/inf -> `NotFinite`; AR order, threshold out of range, or a
    /// `max_gap_ms` that rounds to 0 or more than [`CLICK_MAX_GAP_SAMPLES`] samples -> `OutOfRange`.
    pub fn new(sample_rate_hz: u32, channels: usize, params: ClickRepairParams) -> Result<Self, ScoreError> {
        ticks_per_sample(sample_rate_hz)?;
        if !(1..=MAX_CHANNELS).contains(&channels) {
            return Err(ScoreError::InvalidLayout);
        }
        if !params.threshold.is_finite() || !params.max_gap_ms.is_finite() {
            return Err(ScoreError::NotFinite);
        }
        if !(CLICK_AR_ORDER.0..=CLICK_AR_ORDER.1).contains(&params.ar_order)
            || !(CLICK_THRESHOLD.0..=CLICK_THRESHOLD.1).contains(&params.threshold)
            || !(0.0..=1000.0).contains(&params.max_gap_ms)
        {
            return Err(ScoreError::OutOfRange);
        }
        let max_gap = (params.max_gap_ms * f64::from(sample_rate_hz) / 1000.0).round() as usize;
        if !(1..=CLICK_MAX_GAP_SAMPLES).contains(&max_gap) {
            return Err(ScoreError::OutOfRange);
        }
        let p = params.ar_order;
        let identity = format!(
            "click_repair:rate={sample_rate_hz};channels={channels};order={p};max_gap={max_gap};threshold={:016x}",
            params.threshold.to_bits()
        );
        Ok(Self {
            sample_rate_hz,
            channels,
            params,
            max_gap,
            hist: vec![0.0; channels * CLICK_WINDOW],
            centre: vec![0.0; channels * CLICK_HOP],
            fill: CLICK_WINDOW - CLICK_HOP,
            scratch: Scratch {
                window: vec![0.0; CLICK_WINDOW],
                original: vec![0.0; CLICK_WINDOW],
                residual: vec![0.0; CLICK_WINDOW],
                abs: vec![0.0; CLICK_WINDOW],
                r: vec![0.0; p + 1],
                a: vec![0.0; p + 1],
                tmp: vec![0.0; p + 1],
                ra: vec![0.0; p + 1],
                gaps: Vec::with_capacity(CLICK_WINDOW),
                matrix: vec![0.0; CLICK_MAX_GAP_SAMPLES * CLICK_MAX_GAP_SAMPLES],
                rhs: vec![0.0; CLICK_MAX_GAP_SAMPLES],
                rows: vec![0.0; CLICK_MAX_GAP_SAMPLES + p],
            },
            repaired_gaps: 0,
            skipped_gaps: 0,
            identity,
        })
    }
    pub fn params(&self) -> &ClickRepairParams {
        &self.params
    }
    /// Longest repaired gap in samples.
    pub fn max_gap_samples(&self) -> usize {
        self.max_gap
    }
    /// Gaps interpolated so far (all channels).
    pub fn repaired_gaps(&self) -> u64 {
        self.repaired_gaps
    }
    /// Detected gaps longer than the maximum, left untouched.
    pub fn skipped_gaps(&self) -> u64 {
        self.skipped_gaps
    }

    /// Repairs `scratch.window` in place; returns (repaired, skipped) gap counts.
    fn repair_window(s: &mut Scratch, p: usize, threshold: f64, max_gap: usize) -> (u64, u64) {
        let w = CLICK_WINDOW;
        s.original.copy_from_slice(&s.window);
        if !fit_ar(&s.window, &mut s.r, &mut s.a, &mut s.tmp) {
            return (0, 0);
        }
        for n in p..w {
            s.residual[n] = (0..=p).map(|k| s.a[k] * s.window[n - k]).sum();
        }
        let count = w - p;
        for (dst, e) in s.abs[..count].iter_mut().zip(&s.residual[p..w]) {
            *dst = e.abs();
        }
        let (_, median, _) = s.abs[..count].select_nth_unstable_by(count / 2, f64::total_cmp);
        let rms = (s.r[0] / w as f64).sqrt();
        let sigma = (1.4826 * *median).max(1e-5 * rms);
        let limit = threshold * sigma;
        // Clusters of flags closer than p samples.
        s.gaps.clear();
        let mut current: Option<(usize, usize)> = None;
        for n in p..w {
            if s.residual[n].abs() > limit {
                current = match current {
                    Some((first, last)) if n - last < p => Some((first, n)),
                    Some(done) => {
                        s.gaps.push(done);
                        Some((n, n))
                    }
                    None => Some((n, n)),
                };
            }
        }
        if let Some(done) = current {
            s.gaps.push(done);
        }
        // Counts cover only gaps that touch this window's centre, so a click seen by two
        // overlapping windows is counted once.
        let in_centre = |first: usize, last: usize| last >= CLICK_CONTEXT && first < CLICK_CONTEXT + CLICK_HOP;
        let mut skipped = 0;
        // Keep only gaps with p known samples each side and LSAR rows inside the window.
        s.gaps.retain(|&(first, last)| {
            let len = last - first + 1;
            let fits = first >= p && last + 1 + p <= w;
            if len > max_gap && in_centre(first, last) {
                skipped += 1;
            }
            fits && len <= max_gap
        });
        for pass in 0..2 {
            if pass == 1 && !fit_ar(&s.window, &mut s.r, &mut s.a, &mut s.tmp) {
                break;
            }
            for (k, ra) in s.ra.iter_mut().enumerate() {
                *ra = (0..=p - k).map(|j| s.a[j] * s.a[j + k]).sum();
            }
            for gi in 0..s.gaps.len() {
                let (first, last) = s.gaps[gi];
                lsar(s, p, first, last + 1 - first);
            }
        }
        let repaired = s.gaps.iter().filter(|(first, last)| in_centre(*first, *last)).count() as u64;
        (repaired, skipped)
    }
}

/// Autocorrelation + Levinson-Durbin. Returns false for a silent or degenerate window.
fn fit_ar(x: &[f64], r: &mut [f64], a: &mut [f64], tmp: &mut [f64]) -> bool {
    let p = a.len() - 1;
    for (k, rk) in r.iter_mut().enumerate() {
        *rk = (k..x.len()).map(|n| x[n] * x[n - k]).sum();
    }
    if r[0] <= 0.0 {
        return false;
    }
    // White-noise correction keeps the recursion stable for near-singular (pure-tone) windows.
    let r0 = r[0] * (1.0 + 1e-9);
    a.fill(0.0);
    a[0] = 1.0;
    let mut err = r0;
    for i in 1..=p {
        let acc = r[i] + (1..i).map(|j| a[j] * r[i - j]).sum::<f64>();
        let k = -acc / err;
        tmp[..i].copy_from_slice(&a[..i]);
        for j in 1..i {
            a[j] = tmp[j] + k * tmp[i - j];
        }
        a[i] = k;
        err *= 1.0 - k * k;
        if err <= 0.0 || !err.is_finite() {
            return false;
        }
    }
    true
}

/// Least-squares AR interpolation of `x[m .. m + g]` in `s.window`, using `s.original` known samples.
fn lsar(s: &mut Scratch, p: usize, m: usize, g: usize) {
    // Known-sample part of every residual touching the gap.
    for (i, row) in s.rows[..g + p].iter_mut().enumerate() {
        let n = m + i;
        *row = (0..=p)
            .filter(|k| !(m..m + g).contains(&(n - k)))
            .map(|k| s.a[k] * s.original[n - k])
            .sum();
    }
    for i in 0..g {
        s.rhs[i] = -(0..=p).map(|k| s.a[k] * s.rows[i + k]).sum::<f64>();
        for j in 0..g {
            // Coefficient autocorrelation; zero beyond lag p.
            s.matrix[i * g + j] = s.ra.get(i.abs_diff(j)).copied().unwrap_or(0.0);
        }
    }
    // Cholesky (lower, in place) then forward/back substitution.
    for j in 0..g {
        let mut d = s.matrix[j * g + j];
        for k in 0..j {
            d -= s.matrix[j * g + k] * s.matrix[j * g + k];
        }
        if d <= 0.0 {
            return; // numerically singular: leave the gap as it is
        }
        let d = d.sqrt();
        s.matrix[j * g + j] = d;
        for i in j + 1..g {
            let mut v = s.matrix[i * g + j];
            for k in 0..j {
                v -= s.matrix[i * g + k] * s.matrix[j * g + k];
            }
            s.matrix[i * g + j] = v / d;
        }
    }
    for i in 0..g {
        let mut v = s.rhs[i];
        for k in 0..i {
            v -= s.matrix[i * g + k] * s.rhs[k];
        }
        s.rhs[i] = v / s.matrix[i * g + i];
    }
    for i in (0..g).rev() {
        let mut v = s.rhs[i];
        for k in i + 1..g {
            v -= s.matrix[k * g + i] * s.rhs[k];
        }
        s.rhs[i] = v / s.matrix[i * g + i];
    }
    s.window[m..m + g].copy_from_slice(&s.rhs[..g]);
}

impl Processor for ClickRepair {
    fn identity(&self) -> String {
        self.identity.clone()
    }
    fn latency_samples(&self) -> u32 {
        (CLICK_HOP + CLICK_CONTEXT) as u32
    }
    fn accepts(&self, input: &ChannelLayout, output: &ChannelLayout) -> bool {
        input == output && input.channels() == self.channels
    }
    fn required_sample_rate_hz(&self) -> Option<u32> {
        Some(self.sample_rate_hz)
    }
    fn process(&mut self, input: &InputBlock<'_>, out: &mut OutputBlock, cancel: &CancellationToken) -> Result<(), ScoreError> {
        let frames = input.frames();
        let (p, threshold, max_gap) = (self.params.ar_order, self.params.threshold, self.max_gap);
        let base = CLICK_WINDOW - CLICK_HOP;
        let mut at = 0;
        while at < frames {
            poll(cancel)?;
            let end = (at + CANCEL_CHECK_FRAMES).min(frames);
            for n in at..end {
                let pos = self.fill - base;
                for (c, dst) in out.channels_mut().enumerate().take(self.channels) {
                    dst[n] = self.centre[c * CLICK_HOP + pos];
                    self.hist[c * CLICK_WINDOW + self.fill] = f64::from(input.channel(c)[n]);
                }
                self.fill += 1;
                if self.fill == CLICK_WINDOW {
                    for c in 0..self.channels {
                        let h = &mut self.hist[c * CLICK_WINDOW..(c + 1) * CLICK_WINDOW];
                        self.scratch.window.copy_from_slice(h);
                        let (rep, skip) = Self::repair_window(&mut self.scratch, p, threshold, max_gap);
                        self.repaired_gaps += rep;
                        self.skipped_gaps += skip;
                        let centre = &self.scratch.window[CLICK_CONTEXT..CLICK_CONTEXT + CLICK_HOP];
                        for (dst, v) in self.centre[c * CLICK_HOP..(c + 1) * CLICK_HOP].iter_mut().zip(centre) {
                            *dst = *v as f32;
                        }
                        h.copy_within(CLICK_HOP.., 0);
                    }
                    self.fill = base;
                }
            }
            at = end;
        }
        Ok(())
    }
    fn reset(&mut self) {
        self.hist.fill(0.0);
        self.centre.fill(0.0);
        self.fill = CLICK_WINDOW - CLICK_HOP;
        self.repaired_gaps = 0;
        self.skipped_gaps = 0;
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HumParams {
    pub fundamental_hz: f64,
    /// Number of notches at `k * fundamental_hz`, `k = 1..=harmonics` (1..=16).
    pub harmonics: usize,
    pub bandwidth_hz: f64,
}

impl Default for HumParams {
    /// 50 Hz mains with 8 harmonics, 2 Hz notches.
    fn default() -> Self {
        Self { fundamental_hz: 50.0, harmonics: 8, bandwidth_hz: 2.0 }
    }
}

/// Notch cascade at a hum fundamental and its harmonics. See the module docs.
#[derive(Debug)]
pub struct HumRemover {
    sample_rate_hz: u32,
    channels: usize,
    params: HumParams,
    sections: [Option<Sos>; MAX_HUM_HARMONICS],
    used: usize,
    state: Vec<[SosState; MAX_HUM_HARMONICS]>,
    identity: String,
}

impl HumRemover {
    pub fn new(sample_rate_hz: u32, channels: usize, params: HumParams) -> Result<Self, ScoreError> {
        ticks_per_sample(sample_rate_hz)?;
        if !(1..=MAX_CHANNELS).contains(&channels) {
            return Err(ScoreError::InvalidLayout);
        }
        if !params.fundamental_hz.is_finite() || !params.bandwidth_hz.is_finite() {
            return Err(ScoreError::NotFinite);
        }
        let rate = f64::from(sample_rate_hz);
        if !(HUM_FUNDAMENTAL_HZ.0..=HUM_FUNDAMENTAL_HZ.1).contains(&params.fundamental_hz)
            || !(HUM_BANDWIDTH_HZ.0..=HUM_BANDWIDTH_HZ.1).contains(&params.bandwidth_hz)
            || !(1..=MAX_HUM_HARMONICS).contains(&params.harmonics)
        {
            return Err(ScoreError::OutOfRange);
        }
        let mut sections = [None; MAX_HUM_HARMONICS];
        let mut used = 0;
        for k in 1..=params.harmonics {
            let f = params.fundamental_hz * k as f64;
            if f >= 0.95 * rate / 2.0 {
                break;
            }
            sections[used] = Some(Sos::new(SosKind::Notch, f, f / params.bandwidth_hz, rate));
            used += 1;
        }
        let identity = format!(
            "hum_remover:rate={sample_rate_hz};channels={channels};notches={used};p={:016x},{:016x}",
            params.fundamental_hz.to_bits(),
            params.bandwidth_hz.to_bits()
        );
        Ok(Self {
            sample_rate_hz,
            channels,
            params,
            sections,
            used,
            state: vec![[SosState::default(); MAX_HUM_HARMONICS]; channels],
            identity,
        })
    }
    pub fn params(&self) -> &HumParams {
        &self.params
    }
    /// Notches actually placed (harmonics at or above 0.95 Nyquist are dropped).
    pub fn notches(&self) -> usize {
        self.used
    }
    /// Analytic magnitude of the cascade at `frequency_hz`.
    pub fn magnitude(&self, frequency_hz: f64) -> f64 {
        let rate = f64::from(self.sample_rate_hz);
        self.sections[..self.used].iter().flatten().map(|s| s.magnitude(frequency_hz, rate)).product()
    }
}

impl Processor for HumRemover {
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
    fn process(&mut self, input: &InputBlock<'_>, out: &mut OutputBlock, cancel: &CancellationToken) -> Result<(), ScoreError> {
        let frames = input.frames();
        let mut at = 0;
        while at < frames {
            poll(cancel)?;
            let end = (at + CANCEL_CHECK_FRAMES).min(frames);
            for (c, dst) in out.channels_mut().enumerate().take(self.channels) {
                let src = input.channel(c);
                let state = &mut self.state[c];
                for n in at..end {
                    let mut v = f64::from(src[n]);
                    for (st, sec) in state.iter_mut().zip(self.sections[..self.used].iter().flatten()) {
                        v = st.run(sec, v);
                    }
                    dst[n] = v as f32;
                }
            }
            at = end;
        }
        Ok(())
    }
    fn reset(&mut self) {
        self.state.fill([SosState::default(); MAX_HUM_HARMONICS]);
    }
}
