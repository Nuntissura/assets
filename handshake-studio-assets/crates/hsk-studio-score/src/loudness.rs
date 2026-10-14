//! Loudness metering per ITU-R BS.1770-4 and EBU R128 / Tech 3341 / Tech 3342.
//!
//! * K-weighting: pre-filter high shelf then RLB high-pass, both biquads derived for any admitted
//!   rate from the analogue prototypes by the bilinear transform. At 48 kHz the derived
//!   coefficients equal the BS.1770-4 table values to 1e-12 (asserted in `tests/loudness.rs`).
//! * Gating blocks: 400 ms windows every 100 ms (75 percent overlap). The 100 ms sub-block edges
//!   are `floor(k * rate / 10)`, so any admitted rate (also 11025 Hz) keeps exact integer sample
//!   counts and the result does not depend on the caller's block partition.
//! * Loudness `L = -0.691 + 10 log10(sum_i G_i z_i)`; channel weights `G_i` are supplied by the
//!   caller because the spec declares no channel-label semantics (UNVERIFIED), 1.0 for
//!   L/R/C, 1.41 for the surrounds, 0.0 for LFE.
//! * Integrated: absolute gate -70 LUFS, relative gate -10 LU. Loudness range: 3 s short-term
//!   values every 100 ms, absolute gate -70 LUFS, relative gate -20 LU, 10th to 95th percentile.
//!   Gating statistics live in fixed 0.01 LU histograms (bounded memory for any stream length);
//!   gate thresholds and percentiles are therefore quantised to 0.01 LU, block power sums are
//!   exact.
//! * True peak: 4x oversampling ([`crate::truepeak`]) of the unweighted signal.
use crate::block::{InputBlock, OutputBlock};
use crate::dsp::poll;
use crate::engine::Processor;
use crate::error::ScoreError;
use crate::layout::{CANCEL_CHECK_FRAMES, ChannelLayout, MAX_CHANNELS, ticks_per_sample};
use crate::truepeak::TruePeakFilter;
use hsk_studio_accord::CancellationToken;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// `-0.691` offset of BS.1770-4 equation (2).
const OFFSET: f64 = -0.691;
const HIST_LOW: f64 = -70.0;
const HIST_BIN: f64 = 0.01;
const HIST_BINS: usize = 10_000;
const SHORT_TERM_SUBBLOCKS: usize = 30;
const MOMENTARY_SUBBLOCKS: usize = 4;

/// Surround channel weight (+1.5 dB) of BS.1770-4.
pub const SURROUND_WEIGHT: f64 = 1.41;

fn lufs(z: f64) -> f64 {
    if z > 0.0 { OFFSET + 10.0 * z.log10() } else { f64::NEG_INFINITY }
}

#[derive(Clone, Copy, Debug)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
}

impl Biquad {
    fn pre_filter(rate: f64) -> Self {
        let (f0, gain_db, q) = (1_681.974_450_955_533, 3.999_843_853_973_347, 0.707_175_236_955_419_6);
        let k = (std::f64::consts::PI * f0 / rate).tan();
        let vh = 10f64.powf(gain_db / 20.0);
        let vb = vh.powf(0.499_666_774_154_541_6);
        let a0 = 1.0 + k / q + k * k;
        Self {
            b: [
                (vh + vb * k / q + k * k) / a0,
                2.0 * (k * k - vh) / a0,
                (vh - vb * k / q + k * k) / a0,
            ],
            a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        }
    }
    fn rlb(rate: f64) -> Self {
        let (f0, q) = (38.135_470_876_024_44, 0.500_327_037_323_877_3);
        let k = (std::f64::consts::PI * f0 / rate).tan();
        let a0 = 1.0 + k / q + k * k;
        Self {
            b: [1.0, -2.0, 1.0],
            a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        }
    }
}

/// K-weighting biquad coefficients `(shelf_b, shelf_a, rlb_b, rlb_a)` for `sample_rate_hz`
/// (`a` without the leading 1). Exposed so the 48 kHz table values can be checked.
pub fn k_weighting_coefficients(sample_rate_hz: u32) -> ([f64; 3], [f64; 2], [f64; 3], [f64; 2]) {
    let rate = f64::from(sample_rate_hz);
    let (s, r) = (Biquad::pre_filter(rate), Biquad::rlb(rate));
    (s.b, s.a, r.b, r.a)
}

#[derive(Clone, Copy, Debug, Default)]
struct KState {
    s: [f64; 2],
    r: [f64; 2],
}

impl KState {
    fn run(&mut self, shelf: &Biquad, rlb: &Biquad, x: f64) -> f64 {
        // Transposed direct form II.
        let y = shelf.b[0] * x + self.s[0];
        self.s[0] = shelf.b[1] * x - shelf.a[0] * y + self.s[1];
        self.s[1] = shelf.b[2] * x - shelf.a[1] * y;
        let z = rlb.b[0] * y + self.r[0];
        self.r[0] = rlb.b[1] * y - rlb.a[0] * z + self.r[1];
        self.r[1] = rlb.b[2] * y - rlb.a[1] * z;
        z
    }
}

#[derive(Clone, Debug)]
struct Histogram {
    counts: Vec<u64>,
    energy: Vec<f64>,
}

impl Histogram {
    fn new() -> Self {
        Self { counts: vec![0; HIST_BINS], energy: vec![0.0; HIST_BINS] }
    }
    fn clear(&mut self) {
        self.counts.fill(0);
        self.energy.fill(0.0);
    }
    fn add(&mut self, loudness: f64, z: f64) {
        let index = (((loudness - HIST_LOW) / HIST_BIN) as usize).min(HIST_BINS - 1);
        self.counts[index] += 1;
        self.energy[index] += z;
    }
    fn totals(&self, from: usize) -> (u64, f64) {
        (self.counts[from..].iter().sum(), self.energy[from..].iter().sum())
    }
    /// First bin strictly above `gate_lufs`.
    fn first_above(gate_lufs: f64) -> usize {
        if gate_lufs <= HIST_LOW { 0 } else { (((gate_lufs - HIST_LOW) / HIST_BIN).floor() as usize + 1).min(HIST_BINS) }
    }
    fn relative_threshold(&self, offset_lu: f64) -> Option<usize> {
        let (n, e) = self.totals(0);
        (n > 0).then(|| Self::first_above(lufs(e / n as f64) + offset_lu))
    }
}

/// One read-out of a meter. `None` means "not enough audio yet" (momentary needs 400 ms,
/// short-term 3 s, integrated one gated block, loudness range one gated short-term value);
/// `-inf` means digital silence.
#[derive(Clone, Debug, PartialEq)]
pub struct LoudnessSnapshot {
    pub frames: u64,
    pub momentary_lufs: Option<f64>,
    pub short_term_lufs: Option<f64>,
    pub integrated_lufs: Option<f64>,
    pub loudness_range_lu: Option<f64>,
    pub max_momentary_lufs: Option<f64>,
    pub max_short_term_lufs: Option<f64>,
    /// Per input channel, dBTP of the 4x oversampled signal (`-inf` for silence).
    pub true_peak_dbtp: Vec<f64>,
    /// Per input channel, dBFS sample peak.
    pub sample_peak_dbfs: Vec<f64>,
    /// Gating blocks above the absolute gate.
    pub gated_blocks: u64,
}

fn db_amp(x: f64) -> f64 {
    if x > 0.0 { 20.0 * x.log10() } else { f64::NEG_INFINITY }
}

#[derive(Debug)]
struct Shared {
    frames: AtomicU64,
    values: [AtomicU64; 6],
    gated_blocks: AtomicU64,
    true_peak: Vec<AtomicU64>,
    sample_peak: Vec<AtomicU64>,
}

fn store_opt(a: &AtomicU64, v: Option<f64>) {
    a.store(v.map_or(f64::NAN.to_bits(), f64::to_bits), Ordering::Release);
}
fn load_opt(a: &AtomicU64) -> Option<f64> {
    let v = f64::from_bits(a.load(Ordering::Acquire));
    (!v.is_nan()).then_some(v)
}

/// Cloneable, lock-free read handle of a [`LoudnessMeter`] that sits inside a [`Chain`](crate::Chain)
/// (the chain owns the meter; the readout is updated at the end of every processed block).
#[derive(Clone, Debug)]
pub struct LoudnessReadout {
    shared: Arc<Shared>,
}

impl LoudnessReadout {
    pub fn snapshot(&self) -> LoudnessSnapshot {
        let s = &self.shared;
        LoudnessSnapshot {
            frames: s.frames.load(Ordering::Acquire),
            momentary_lufs: load_opt(&s.values[0]),
            short_term_lufs: load_opt(&s.values[1]),
            integrated_lufs: load_opt(&s.values[2]),
            loudness_range_lu: load_opt(&s.values[3]),
            max_momentary_lufs: load_opt(&s.values[4]),
            max_short_term_lufs: load_opt(&s.values[5]),
            true_peak_dbtp: s.true_peak.iter().map(|a| f64::from_bits(a.load(Ordering::Acquire))).collect(),
            sample_peak_dbfs: s.sample_peak.iter().map(|a| f64::from_bits(a.load(Ordering::Acquire))).collect(),
            gated_blocks: s.gated_blocks.load(Ordering::Acquire),
        }
    }
}

/// BS.1770-4 / R128 meter and pass-through [`Processor`] (latency 0, output bit-identical to the
/// input). A block discarded by the engine (cancel, stale revision) calls `reset`, which clears
/// the whole measurement; the meter cannot un-measure one block.
#[derive(Debug)]
pub struct LoudnessMeter {
    sample_rate_hz: u32,
    weights: Vec<f64>,
    shelf: Biquad,
    rlb: Biquad,
    filters: Vec<KState>,
    peaks: Vec<TruePeakFilter>,
    true_peak: Vec<f64>,
    sample_peak: Vec<f64>,
    frames: u64,
    next_edge: u64,
    sub_index: u64,
    sub_energy: f64,
    sub_frames: u64,
    ring: VecDeque<(f64, u64)>,
    gating: Histogram,
    range: Histogram,
    momentary: Option<f64>,
    short_term: Option<f64>,
    max_momentary: Option<f64>,
    max_short_term: Option<f64>,
    integrated_cache: Option<f64>,
    range_cache: Option<f64>,
    shared: Arc<Shared>,
    identity: String,
}

impl LoudnessMeter {
    /// `weights[i]` is `G_i` of input channel `i` (one entry per channel, finite, `0..=4`).
    pub fn new(sample_rate_hz: u32, weights: &[f64]) -> Result<Self, ScoreError> {
        ticks_per_sample(sample_rate_hz)?;
        if !(1..=MAX_CHANNELS).contains(&weights.len()) {
            return Err(ScoreError::InvalidLayout);
        }
        if weights.iter().any(|w| !w.is_finite()) {
            return Err(ScoreError::NotFinite);
        }
        if weights.iter().any(|w| !(0.0..=4.0).contains(w)) {
            return Err(ScoreError::OutOfRange);
        }
        let channels = weights.len();
        let rate = f64::from(sample_rate_hz);
        let atomics = |n: usize| (0..n).map(|_| AtomicU64::new(f64::NEG_INFINITY.to_bits())).collect::<Vec<_>>();
        let shared = Arc::new(Shared {
            frames: AtomicU64::new(0),
            values: std::array::from_fn(|_| AtomicU64::new(f64::NAN.to_bits())),
            gated_blocks: AtomicU64::new(0),
            true_peak: atomics(channels),
            sample_peak: atomics(channels),
        });
        let identity = format!(
            "loudness_meter:rate={sample_rate_hz};weights={}",
            weights.iter().map(|w| format!("{:016x}", w.to_bits())).collect::<Vec<_>>().join(",")
        );
        Ok(Self {
            sample_rate_hz,
            weights: weights.to_vec(),
            shelf: Biquad::pre_filter(rate),
            rlb: Biquad::rlb(rate),
            filters: vec![KState::default(); channels],
            peaks: vec![TruePeakFilter::new(); channels],
            true_peak: vec![0.0; channels],
            sample_peak: vec![0.0; channels],
            frames: 0,
            next_edge: Self::edge(1, sample_rate_hz),
            sub_index: 0,
            sub_energy: 0.0,
            sub_frames: 0,
            ring: VecDeque::with_capacity(SHORT_TERM_SUBBLOCKS + 1),
            gating: Histogram::new(),
            range: Histogram::new(),
            momentary: None,
            short_term: None,
            max_momentary: None,
            max_short_term: None,
            integrated_cache: None,
            range_cache: None,
            shared,
            identity,
        })
    }
    /// Unit weights for `channels` channels (mono, stereo, or any layout of equal-weight channels).
    pub fn unweighted(sample_rate_hz: u32, channels: usize) -> Result<Self, ScoreError> {
        Self::new(sample_rate_hz, &vec![1.0; channels])
    }
    /// 5.1 in the order L, R, C, LFE, Ls, Rs: weights 1, 1, 1, 0, 1.41, 1.41 (BS.1770-4 Table 4).
    pub fn weights_5_1() -> [f64; 6] {
        [1.0, 1.0, 1.0, 0.0, SURROUND_WEIGHT, SURROUND_WEIGHT]
    }
    pub fn readout(&self) -> LoudnessReadout {
        LoudnessReadout { shared: Arc::clone(&self.shared) }
    }
    pub fn channels(&self) -> usize {
        self.weights.len()
    }

    fn edge(k: u64, rate: u32) -> u64 {
        (u128::from(k) * u128::from(rate) / 10) as u64
    }

    /// Feeds planar samples (every channel the same length, all finite).
    pub fn feed(&mut self, channels: &[&[f32]]) -> Result<(), ScoreError> {
        let none = CancellationToken::default();
        self.feed_cancellable(channels, &none)
    }

    fn feed_cancellable(&mut self, channels: &[&[f32]], cancel: &CancellationToken) -> Result<(), ScoreError> {
        if channels.len() != self.weights.len() {
            return Err(ScoreError::ChannelMismatch);
        }
        let frames = channels[0].len();
        if channels.iter().any(|c| c.len() != frames) {
            return Err(ScoreError::LengthMismatch);
        }
        if channels.iter().any(|c| c.iter().any(|s| !s.is_finite())) {
            return Err(ScoreError::NotFinite);
        }
        let mut at = 0;
        while at < frames {
            poll(cancel)?;
            let end = (at + CANCEL_CHECK_FRAMES).min(frames);
            for n in at..end {
                let mut energy = 0.0;
                for (ch, samples) in channels.iter().enumerate() {
                    let x = f64::from(samples[n]);
                    let y = self.filters[ch].run(&self.shelf, &self.rlb, x);
                    energy += self.weights[ch] * y * y;
                    self.sample_peak[ch] = self.sample_peak[ch].max(x.abs());
                    // The interpolator reports the region 6 samples back; its running maximum
                    // covers the whole stream once `finish_peaks` flushes the tail (see snapshot).
                    self.true_peak[ch] = self.true_peak[ch].max(self.peaks[ch].push(x));
                }
                self.sub_energy += energy;
                self.sub_frames += 1;
                self.frames += 1;
                if self.frames == self.next_edge {
                    self.close_subblock();
                }
            }
            at = end;
        }
        self.publish();
        Ok(())
    }

    fn close_subblock(&mut self) {
        if self.ring.len() == SHORT_TERM_SUBBLOCKS {
            self.ring.pop_front();
        }
        self.ring.push_back((self.sub_energy, self.sub_frames));
        self.sub_energy = 0.0;
        self.sub_frames = 0;
        self.sub_index += 1;
        self.next_edge = Self::edge(self.sub_index + 1, self.sample_rate_hz);
        let window = |count: usize| -> Option<f64> {
            (self.ring.len() >= count).then(|| {
                let (e, n) = self.ring.iter().rev().take(count).fold((0.0, 0u64), |a, b| (a.0 + b.0, a.1 + b.1));
                e / n as f64
            })
        };
        if let Some(z) = window(MOMENTARY_SUBBLOCKS) {
            let l = lufs(z);
            self.momentary = Some(l);
            self.max_momentary = Some(self.max_momentary.map_or(l, |m| m.max(l)));
            if l > HIST_LOW {
                self.gating.add(l, z);
            }
        }
        if let Some(z) = window(SHORT_TERM_SUBBLOCKS) {
            let l = lufs(z);
            self.short_term = Some(l);
            self.max_short_term = Some(self.max_short_term.map_or(l, |m| m.max(l)));
            if l > HIST_LOW {
                self.range.add(l, z);
            }
        }
        self.integrated_cache = self.integrated();
        self.range_cache = self.loudness_range();
    }

    fn integrated(&self) -> Option<f64> {
        let from = self.gating.relative_threshold(-10.0)?;
        let (n, e) = self.gating.totals(from);
        (n > 0).then(|| lufs(e / n as f64))
    }

    fn loudness_range(&self) -> Option<f64> {
        let from = self.range.relative_threshold(-20.0)?;
        let (n, _) = self.range.totals(from);
        if n == 0 {
            return None;
        }
        let at_rank = |p: f64| -> f64 {
            let rank = ((n - 1) as f64 * p + 0.5).floor() as u64;
            let mut seen = 0;
            for i in from..HIST_BINS {
                seen += self.range.counts[i];
                if seen > rank {
                    return HIST_LOW + (i as f64 + 0.5) * HIST_BIN;
                }
            }
            HIST_LOW + HIST_BINS as f64 * HIST_BIN
        };
        Some(at_rank(0.95) - at_rank(0.10))
    }

    /// Current measurement. True peak includes the interpolator tail: the 6 newest samples are
    /// flushed through a copy of the filter state, so a peak in the last samples is not missed.
    pub fn snapshot(&self) -> LoudnessSnapshot {
        let true_peak_dbtp = (0..self.weights.len())
            .map(|ch| {
                let mut tail = self.peaks[ch].clone();
                let mut peak = self.true_peak[ch];
                for _ in 0..crate::truepeak::TRUE_PEAK_DELAY {
                    peak = peak.max(tail.push(0.0));
                }
                db_amp(peak)
            })
            .collect();
        LoudnessSnapshot {
            frames: self.frames,
            momentary_lufs: self.momentary,
            short_term_lufs: self.short_term,
            integrated_lufs: self.integrated_cache,
            loudness_range_lu: self.range_cache,
            max_momentary_lufs: self.max_momentary,
            max_short_term_lufs: self.max_short_term,
            true_peak_dbtp,
            sample_peak_dbfs: self.sample_peak.iter().map(|p| db_amp(*p)).collect(),
            gated_blocks: self.gating.counts.iter().sum(),
        }
    }

    fn publish(&self) {
        let s = self.snapshot();
        let sh = &self.shared;
        sh.frames.store(s.frames, Ordering::Release);
        store_opt(&sh.values[0], s.momentary_lufs);
        store_opt(&sh.values[1], s.short_term_lufs);
        store_opt(&sh.values[2], s.integrated_lufs);
        store_opt(&sh.values[3], s.loudness_range_lu);
        store_opt(&sh.values[4], s.max_momentary_lufs);
        store_opt(&sh.values[5], s.max_short_term_lufs);
        sh.gated_blocks.store(s.gated_blocks, Ordering::Release);
        for (a, v) in sh.true_peak.iter().zip(&s.true_peak_dbtp) {
            a.store(v.to_bits(), Ordering::Release);
        }
        for (a, v) in sh.sample_peak.iter().zip(&s.sample_peak_dbfs) {
            a.store(v.to_bits(), Ordering::Release);
        }
    }

    fn clear(&mut self) {
        self.filters.fill(KState::default());
        for p in &mut self.peaks {
            p.reset();
        }
        self.true_peak.fill(0.0);
        self.sample_peak.fill(0.0);
        self.frames = 0;
        self.next_edge = Self::edge(1, self.sample_rate_hz);
        self.sub_index = 0;
        self.sub_energy = 0.0;
        self.sub_frames = 0;
        self.ring.clear();
        self.gating.clear();
        self.range.clear();
        self.momentary = None;
        self.short_term = None;
        self.max_momentary = None;
        self.max_short_term = None;
        self.integrated_cache = None;
        self.range_cache = None;
    }
}

impl Processor for LoudnessMeter {
    fn identity(&self) -> String {
        self.identity.clone()
    }
    fn latency_samples(&self) -> u32 {
        0
    }
    fn accepts(&self, input: &ChannelLayout, output: &ChannelLayout) -> bool {
        input == output && input.channels() == self.weights.len()
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
        for (src, dst) in input.channels().iter().zip(out.channels_mut()) {
            dst[..src.len()].copy_from_slice(src);
        }
        self.feed_cancellable(input.channels(), cancel)
    }
    fn reset(&mut self) {
        self.clear();
        self.publish();
    }
}
