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
//! 2. Detection: `|e[n]| > threshold * sigma_n`; `sigma` is the robust scale `1.4826 * median|e|`
//!    of each 256-sample segment, `sigma_n` the largest of the segment of `n` and its two
//!    neighbours (floored at 1e-5 of the window RMS so numerically perfect fits do not flag
//!    rounding noise). Flags closer than `p` samples are merged into one gap `[first, last]`.
//!    Then, twice: the AR model is refitted by the covariance method over the rows that touch no
//!    detected sample, the residual is recomputed and detection repeats with the scale taken from
//!    those clean rows only (a burst would otherwise inflate its own segment's scale).
//! 3. Interpolation (LSAR): the gap samples `x_u` minimise `sum_n e[n]^2` over every residual
//!    that touches them, i.e. `(A_u^T A_u) x_u = -A_u^T A_k x_k`; `A_u^T A_u` is the Toeplitz
//!    matrix of the coefficient autocorrelation, solved by Cholesky, with the final clean-row AR
//!    model, so the corruption does not bias the model.
//!    Gaps longer than `max_gap_ms` are left untouched and counted in `skipped_gaps`.
//!
//! 4. Digital-silence edges are not clicks: a cluster right after a run of at least `p` exact
//!    zeros (a signal starting from silence) or a cluster of zeros followed by `p` zeros (a signal
//!    stopping into silence) is left untouched and counted in `silence_edges`. The same holds on
//!    a noisy or dithered floor: a cluster where the RMS level of the 256 samples after it is at
//!    least 20 dB above the 256 before it (onset) or 20 dB below (offset) is an edge, not a click;
//!    a click leaves the level around it unchanged and is repaired.
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
/// Level window (samples each side of a detection) for the floor-edge rule.
pub const CLICK_EDGE_LEVEL_WINDOW: usize = 256;
/// Level change that marks an onset or offset rather than a click.
pub const CLICK_EDGE_RATIO_DB: f64 = 20.0;
/// Residual-scale segment length (the scale at a sample is the largest of its own and the two
/// neighbouring segments' robust scales).
const SEGMENT: usize = 256;
const SEGMENTS: usize = CLICK_WINDOW / SEGMENT;
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
    masked: Vec<bool>,
    phi: Vec<f64>,
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
    silence_edges: u64,
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
                masked: vec![false; CLICK_WINDOW],
                phi: vec![0.0; (p + 1) * (p + 1)],
                matrix: vec![0.0; CLICK_MAX_GAP_SAMPLES * CLICK_MAX_GAP_SAMPLES],
                rhs: vec![0.0; CLICK_MAX_GAP_SAMPLES],
                rows: vec![0.0; CLICK_MAX_GAP_SAMPLES + p],
            },
            repaired_gaps: 0,
            skipped_gaps: 0,
            silence_edges: 0,
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
    /// Detections at the edge of silence (a start after or a stop into exact zeros, or a 20 dB level
    /// step from or to a noisy or dithered floor), left untouched.
    pub fn silence_edges(&self) -> u64 {
        self.silence_edges
    }

    /// Repairs `scratch.window` in place; returns (repaired, skipped, silence edge) gap counts.
    fn repair_window(s: &mut Scratch, p: usize, threshold: f64, max_gap: usize) -> (u64, u64, u64) {
        let w = CLICK_WINDOW;
        s.original.copy_from_slice(&s.window);
        if !fit_ar(&s.window, &mut s.r, &mut s.a, &mut s.tmp) {
            return (0, 0, 0);
        }
        let floor = 1e-5 * (s.r[0] / w as f64).sqrt();
        // Stage 1: autocorrelation model, scale from every residual row.
        residual(s, p);
        s.masked.fill(false);
        detect(s, p, threshold, floor);
        if s.gaps.is_empty() {
            return (0, 0, 0);
        }
        // Stage 2 (twice): refit by the covariance method over rows that touch no detected
        // sample (Godsill and Rayner: the model must not see the corruption), recompute the
        // residual and detect again with the scale taken from clean rows only. A burst inflates
        // the stage-1 scale of its own segment, which leaves its edge samples undetected
        // (measured: an 80-sample burst kept 6 corrupted edge samples and the interpolation
        // diverged).
        for _ in 0..2 {
            mask(s);
            fit_ar_clean(s, p);
            residual(s, p);
            detect(s, p, threshold, floor);
        }
        mask(s);
        fit_ar_clean(s, p);
        // Counts cover only gaps that start in this window's centre, so a click seen by two
        // overlapping windows is counted once.
        let starts_in_centre = |first: usize| (CLICK_CONTEXT..CLICK_CONTEXT + CLICK_HOP).contains(&first);
        let (mut skipped, mut edges) = (0, 0);
        // Keep only gaps with p known samples each side and LSAR rows inside the window, and never
        // "repair" the edge of digital silence: a signal starting after silence or stopping into
        // it is a legitimate discontinuity, not a click (the AR model predicts zeros, or a
        // continuation, there).
        s.gaps.retain(|&(first, last)| {
            let len = last - first + 1;
            let fits = first >= p && last + 1 + p <= w;
            if silence_edge(&s.original, first, last, p) {
                if starts_in_centre(first) {
                    edges += 1;
                }
                return false;
            }
            if len > max_gap && starts_in_centre(first) {
                skipped += 1;
            }
            fits && len <= max_gap
        });
        if !s.gaps.is_empty() {
            for (k, ra) in s.ra.iter_mut().enumerate() {
                *ra = (0..=p - k).map(|j| s.a[j] * s.a[j + k]).sum();
            }
            for gi in 0..s.gaps.len() {
                let (first, last) = s.gaps[gi];
                lsar(s, p, first, last + 1 - first);
            }
        }
        let repaired = s.gaps.iter().filter(|(first, _)| starts_in_centre(*first)).count() as u64;
        (repaired, skipped, edges)
    }
}

/// True when cluster `[first, last]` is the edge of digital silence: onset = a run of at least `p`
/// exact zeros ends within the `p` samples before `first`; offset = the cluster and the `p`
/// samples after it are exact zeros (the residual there is the model predicting a continuation).
///
/// Level rule (noisy or dithered floors): with `before` the RMS of up to
/// [`CLICK_EDGE_LEVEL_WINDOW`] samples before `first` and `after` the RMS of up to as many samples
/// after `last` (each needing at least `p` samples), the cluster is an onset when `after` exceeds
/// `before` by [`CLICK_EDGE_RATIO_DB`] or more and an offset when `before` exceeds `after` by as
/// much. A click leaves the level around it unchanged, so it is never an edge by this rule.
fn silence_edge(x: &[f64], first: usize, last: usize, p: usize) -> bool {
    let zero_run_ending_at = |j: usize| j + 1 >= p && x[j + 1 - p..=j].iter().all(|v| *v == 0.0);
    let onset = (first.saturating_sub(p)..first).any(zero_run_ending_at);
    let offset = last + 1 + p <= x.len() && x[first..last + 1 + p].iter().all(|v| *v == 0.0);
    if onset || offset {
        return true;
    }
    let lo = first.saturating_sub(CLICK_EDGE_LEVEL_WINDOW);
    let hi = (last + 1 + CLICK_EDGE_LEVEL_WINDOW).min(x.len());
    if first - lo < p || hi - (last + 1) < p {
        return false;
    }
    let mean_square = |s: &[f64]| s.iter().map(|v| v * v).sum::<f64>() / s.len() as f64;
    let (before, after) = (mean_square(&x[lo..first]), mean_square(&x[last + 1..hi]));
    let ratio = 10f64.powf(CLICK_EDGE_RATIO_DB / 10.0);
    after >= ratio * before || before >= ratio * after
}

/// `s.residual[n] = sum_k a_k original[n - k]` for `n >= p`.
fn residual(s: &mut Scratch, p: usize) {
    for n in p..CLICK_WINDOW {
        s.residual[n] = (0..=p).map(|k| s.a[k] * s.original[n - k]).sum();
    }
}

/// Marks every sample of every cluster in `s.gaps`.
fn mask(s: &mut Scratch) {
    s.masked.fill(false);
    for &(first, last) in &s.gaps {
        s.masked[first..=last].fill(true);
    }
}

/// Flags `|e[n]| > threshold * sigma_n` and merges flags closer than `p` into `s.gaps`.
/// `sigma` per 256-sample segment is `1.4826 * median|e|` over the segment's rows whose span
/// `[n - p, n]` holds no masked sample (segments with fewer than 32 such rows borrow from the
/// neighbours); `sigma_n` is the largest of the own and neighbouring segments, at least `floor`.
fn detect(s: &mut Scratch, p: usize, threshold: f64, floor: f64) {
    let mut seg_sigma = [f64::NAN; SEGMENTS];
    for (i, sigma) in seg_sigma.iter_mut().enumerate() {
        let lo = (i * SEGMENT).max(p);
        let hi = (i + 1) * SEGMENT;
        let mut count = 0;
        for n in lo..hi {
            if !s.masked[n - p..=n].contains(&true) {
                s.abs[count] = s.residual[n].abs();
                count += 1;
            }
        }
        if count >= 32 {
            let (_, median, _) = s.abs[..count].select_nth_unstable_by(count / 2, f64::total_cmp);
            *sigma = 1.4826 * *median;
        }
    }
    s.gaps.clear();
    let mut current: Option<(usize, usize)> = None;
    for n in p..CLICK_WINDOW {
        let seg = n / SEGMENT;
        let local = seg_sigma[seg.saturating_sub(1)..(seg + 2).min(SEGMENTS)]
            .iter()
            .filter(|v| !v.is_nan())
            .fold(floor, |a, b| a.max(*b));
        if s.residual[n].abs() > threshold * local {
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
    if !cholesky_solve(&mut s.matrix, &mut s.rhs, g) {
        return; // numerically singular: leave the gap as it is
    }
    s.window[m..m + g].copy_from_slice(&s.rhs[..g]);
}

/// Solves `M x = b` for symmetric positive definite `M` (row-major `n x n` in `m`, overwritten by
/// its Cholesky factor); `b` is replaced by `x`. Returns false when `M` is not numerically
/// positive definite (then `b` is unspecified).
fn cholesky_solve(m: &mut [f64], b: &mut [f64], n: usize) -> bool {
    for j in 0..n {
        let mut d = m[j * n + j];
        for k in 0..j {
            d -= m[j * n + k] * m[j * n + k];
        }
        if d <= 0.0 || !d.is_finite() {
            return false;
        }
        let d = d.sqrt();
        m[j * n + j] = d;
        for i in j + 1..n {
            let mut v = m[i * n + j];
            for k in 0..j {
                v -= m[i * n + k] * m[j * n + k];
            }
            m[i * n + j] = v / d;
        }
    }
    for i in 0..n {
        let mut v = b[i];
        for k in 0..i {
            v -= m[i * n + k] * b[k];
        }
        b[i] = v / m[i * n + i];
    }
    for i in (0..n).rev() {
        let mut v = b[i];
        for k in i + 1..n {
            v -= m[k * n + i] * b[k];
        }
        b[i] = v / m[i * n + i];
    }
    true
}

/// Covariance-method AR(p) fit over the rows `n` whose span `[n - p, n]` holds no masked
/// sample; writes `s.a` only on success.
fn fit_ar_clean(s: &mut Scratch, p: usize) {
    let w = CLICK_WINDOW;
    let dim = p + 1;
    s.phi[..dim * dim].fill(0.0);
    let mut masked_in_span = s.masked[..p].iter().filter(|m| **m).count();
    let mut rows = 0usize;
    for n in p..w {
        if s.masked[n] {
            masked_in_span += 1;
        }
        if masked_in_span == 0 {
            rows += 1;
            for i in 0..dim {
                let xi = s.original[n - i];
                for j in i..dim {
                    s.phi[i * dim + j] += xi * s.original[n - j];
                }
            }
        }
        if s.masked[n - p] {
            masked_in_span -= 1;
        }
    }
    if rows < 4 * dim {
        return;
    }
    // Normal equations for a_1..a_p: Phi[1.., 1..] a = -Phi[1.., 0] (tiny diagonal loading).
    for i in 0..p {
        for j in 0..p {
            let (r, c) = if i <= j { (i + 1, j + 1) } else { (j + 1, i + 1) };
            s.matrix[i * p + j] = s.phi[r * dim + c];
        }
        s.matrix[i * p + i] *= 1.0 + 1e-9;
        s.rhs[i] = -s.phi[i + 1];
    }
    if cholesky_solve(&mut s.matrix, &mut s.rhs, p) && s.rhs[..p].iter().all(|v| v.is_finite()) {
        s.a[0] = 1.0;
        s.a[1..=p].copy_from_slice(&s.rhs[..p]);
    }
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
                        let (rep, skip, edge) = Self::repair_window(&mut self.scratch, p, threshold, max_gap);
                        self.silence_edges += edge;
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
        self.silence_edges = 0;
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

/// Automatable: 0 = `threshold` (robust sigmas, takes effect from the next analysis window).
/// AR order, window and maximum gap are fixed at construction.
const CLICK_AUTOMATION: &[crate::session::AutomationParam] =
    &[crate::session::AutomationParam { name: "threshold", min: CLICK_THRESHOLD.0, max: CLICK_THRESHOLD.1 }];

impl crate::session::Automatable for ClickRepair {
    fn automation_parameters(&self) -> Vec<crate::session::AutomationParam> {
        CLICK_AUTOMATION.to_vec()
    }
    fn set_automated(&mut self, index: usize, value: f64) -> Result<(), ScoreError> {
        crate::session::check_automated(CLICK_AUTOMATION, index, value)?;
        self.params.threshold = value;
        Ok(())
    }
}

/// Automatable: 0 = `fundamental_hz`, 1 = `bandwidth_hz`; the notches are re-placed with the same
/// rule as [`HumRemover::new`] (harmonics at or above 0.95 Nyquist dropped), filter state is kept.
impl crate::session::Automatable for HumRemover {
    fn automation_parameters(&self) -> Vec<crate::session::AutomationParam> {
        vec![
            crate::session::AutomationParam { name: "fundamental_hz", min: HUM_FUNDAMENTAL_HZ.0, max: HUM_FUNDAMENTAL_HZ.1 },
            crate::session::AutomationParam { name: "bandwidth_hz", min: HUM_BANDWIDTH_HZ.0, max: HUM_BANDWIDTH_HZ.1 },
        ]
    }
    fn set_automated(&mut self, index: usize, value: f64) -> Result<(), ScoreError> {
        let range = match index {
            0 => HUM_FUNDAMENTAL_HZ,
            1 => HUM_BANDWIDTH_HZ,
            _ => return Err(ScoreError::OutOfRange),
        };
        if !value.is_finite() {
            return Err(ScoreError::NotFinite);
        }
        if !(range.0..=range.1).contains(&value) {
            return Err(ScoreError::OutOfRange);
        }
        if index == 0 {
            self.params.fundamental_hz = value;
        } else {
            self.params.bandwidth_hz = value;
        }
        let rate = f64::from(self.sample_rate_hz);
        self.used = 0;
        self.sections = [None; MAX_HUM_HARMONICS];
        for k in 1..=self.params.harmonics {
            let f = self.params.fundamental_hz * k as f64;
            if f >= 0.95 * rate / 2.0 {
                break;
            }
            self.sections[self.used] = Some(Sos::new(SosKind::Notch, f, f / self.params.bandwidth_hz, rate));
            self.used += 1;
        }
        Ok(())
    }
}
