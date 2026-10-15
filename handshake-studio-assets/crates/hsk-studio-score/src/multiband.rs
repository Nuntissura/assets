//! Linkwitz-Riley 4th-order crossover and a 2..4 band compressor built on it.
//!
//! **Crossover.** Each split at `f_k` is a Linkwitz-Riley 4th-order pair: low = two cascaded
//! Butterworth (Q = 1/sqrt 2) low-pass sections, high = two cascaded Butterworth high-pass
//! sections (S. Linkwitz, "Active Crossover Networks for Noncoincident Drivers", JAES 24(1),
//! 1976). In the analogue prototype `LP^2 + HP^2 = (1 + s^4) / (s^2 + sqrt2 s + 1)^2 =
//! (s^2 - sqrt2 s + 1) / (s^2 + sqrt2 s + 1)`, the second-order allpass with the same `f_k` and
//! Q; the bilinear transform is a rational substitution, so the digital sections (same prewarp)
//! keep the identity exactly. Bands are taken as a tree (band `j` = high-passes `0..j`, then the
//! low-pass `j`), and every band except the top one is passed through the allpasses of all higher
//! crossovers, so the band sum is `AP_0 AP_1 ... AP_{n-2}`: magnitude exactly 1 at every
//! frequency (flat), with the allpass phase. Proven to 1e-9 in `tests/multiband.rs`.
//!
//! **Compressor.** Each band runs its own [`Dynamics`] gain computer (Giannoulis soft knee,
//! peak/RMS, attack/release, hold, range, makeup, link) on the band signal; the output is the sum
//! of the gained bands. With every band below its threshold the processor is exactly the allpass
//! chain. Latency 0; all filter and gain state is allocated at construction; `process` allocates
//! nothing and is sample-sequential (any block partition gives the same output). Ranges and the
//! band count are authored: the spec row `Multiband Compressor` (STU-FX-134a) declares 0
//! parameters. Adobe Audition multiband parity is UNVERIFIED (D5).
use crate::block::{InputBlock, OutputBlock};
use crate::dsp::{db_to_linear, poll};
use crate::dynamics::{Dynamics, DynamicsParams};
use crate::engine::Processor;
use crate::error::ScoreError;
use crate::filters::{BUTTERWORTH_Q, Sos, SosKind, SosState};
use crate::layout::{CANCEL_CHECK_FRAMES, ChannelLayout, MAX_CHANNELS, ticks_per_sample};
use hsk_studio_accord::CancellationToken;

/// Band count range.
pub const MULTIBAND_BANDS: (usize, usize) = (2, 4);
/// Lowest crossover frequency; the highest must be below `rate / 2`.
pub const CROSSOVER_MIN_HZ: f64 = 20.0;
/// Widest band list ([`MULTIBAND_BANDS`] upper bound).
pub const MAX_BANDS: usize = 4;
const MAX_SPLITS: usize = MAX_BANDS - 1;

/// One Linkwitz-Riley split (coefficients only).
#[derive(Clone, Copy, Debug)]
struct Split {
    lp: Sos,
    hp: Sos,
    ap: Sos,
}

/// Per-channel state of a whole crossover tree.
#[derive(Clone, Copy, Debug, Default)]
struct TreeState {
    lp: [[SosState; 2]; MAX_SPLITS],
    hp: [[SosState; 2]; MAX_SPLITS],
    /// `comp[b][k]`: allpass of split `k` applied to band `b` (used for `k > b`).
    comp: [[SosState; MAX_SPLITS]; MAX_BANDS],
}

/// Validates a crossover list: 1..=3 strictly ascending finite frequencies in `20..rate/2`.
fn validate_crossovers(crossovers_hz: &[f64], rate: f64) -> Result<(), ScoreError> {
    if crossovers_hz.is_empty() || crossovers_hz.len() > MAX_SPLITS {
        return Err(ScoreError::OutOfRange);
    }
    if crossovers_hz.iter().any(|f| !f.is_finite()) {
        return Err(ScoreError::NotFinite);
    }
    let in_range = crossovers_hz.iter().all(|f| *f >= CROSSOVER_MIN_HZ && *f < rate / 2.0);
    let ascending = crossovers_hz.windows(2).all(|w| w[0] < w[1]);
    if !in_range || !ascending {
        return Err(ScoreError::OutOfRange);
    }
    Ok(())
}

/// Single-channel Linkwitz-Riley 4th-order crossover tree with allpass phase compensation (see the
/// module docs). Exposed for analysis and tests; [`MultibandCompressor`] runs one per channel.
#[derive(Clone, Debug)]
pub struct Crossover {
    rate: f64,
    splits: [Option<Split>; MAX_SPLITS],
    count: usize,
    state: TreeState,
}

impl Crossover {
    /// `crossovers_hz`: 1..=3 strictly ascending frequencies in `20..rate/2` (2..=4 bands).
    pub fn new(sample_rate_hz: u32, crossovers_hz: &[f64]) -> Result<Self, ScoreError> {
        ticks_per_sample(sample_rate_hz)?;
        let rate = f64::from(sample_rate_hz);
        validate_crossovers(crossovers_hz, rate)?;
        let mut splits = [None; MAX_SPLITS];
        for (slot, f) in splits.iter_mut().zip(crossovers_hz) {
            *slot = Some(Split {
                lp: Sos::new(SosKind::LowPass, *f, BUTTERWORTH_Q, rate),
                hp: Sos::new(SosKind::HighPass, *f, BUTTERWORTH_Q, rate),
                ap: Sos::new(SosKind::AllPass, *f, BUTTERWORTH_Q, rate),
            });
        }
        Ok(Self { rate, splits, count: crossovers_hz.len(), state: TreeState::default() })
    }
    pub fn bands(&self) -> usize {
        self.count + 1
    }
    /// Splits one sample; writes `bands()` values into `out[..bands()]`. Their sum is the input
    /// through the allpass chain (magnitude 1).
    pub fn split(&mut self, x: f64, out: &mut [f64; MAX_BANDS]) {
        split_tree(&self.splits, self.count, &mut self.state, x, out);
    }
    /// Analytic magnitude of band `band` at `frequency_hz` (LR4 sections; allpasses are 1).
    pub fn band_magnitude(&self, band: usize, frequency_hz: f64) -> f64 {
        let mut m = 1.0;
        for (k, split) in self.splits.iter().take(self.count).enumerate() {
            let s = split.expect("split present below count");
            if k < band {
                m *= s.hp.magnitude(frequency_hz, self.rate).powi(2);
            } else if k == band {
                m *= s.lp.magnitude(frequency_hz, self.rate).powi(2);
            }
        }
        m
    }
    pub fn reset(&mut self) {
        self.state = TreeState::default();
    }
}

#[inline]
fn split_tree(
    splits: &[Option<Split>; MAX_SPLITS],
    count: usize,
    state: &mut TreeState,
    x: f64,
    out: &mut [f64; MAX_BANDS],
) {
    let mut rest = x;
    for k in 0..count {
        let s = splits[k].as_ref().expect("split present below count");
        let [lp0, lp1] = &mut state.lp[k];
        let low = lp1.run(&s.lp, lp0.run(&s.lp, rest));
        let [hp0, hp1] = &mut state.hp[k];
        rest = hp1.run(&s.hp, hp0.run(&s.hp, rest));
        let mut band = low;
        for (j, sj) in splits.iter().enumerate().take(count).skip(k + 1) {
            let sj = sj.as_ref().expect("split present below count");
            band = state.comp[k][j].run(&sj.ap, band);
        }
        out[k] = band;
    }
    out[count] = rest;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MultibandParams {
    /// 2..=4.
    pub bands: usize,
    /// First `bands - 1` entries are used: strictly ascending, `20..rate/2` Hz.
    pub crossovers_hz: [f64; MAX_SPLITS],
    /// First `bands` entries are used; band 0 is the lowest.
    pub band_params: [DynamicsParams; MAX_BANDS],
}

impl MultibandParams {
    /// Three bands split at 200 Hz and 2 kHz, each a 4:1 compressor at -20 dB.
    pub fn three_band() -> Self {
        Self {
            bands: 3,
            crossovers_hz: [200.0, 2000.0, 8000.0],
            band_params: [DynamicsParams::compressor(-20.0, 4.0); MAX_BANDS],
        }
    }
}

/// 2..4 band compressor over a Linkwitz-Riley 4th-order crossover. See the module docs.
#[derive(Debug)]
pub struct MultibandCompressor {
    sample_rate_hz: u32,
    channels: usize,
    params: MultibandParams,
    splits: [Option<Split>; MAX_SPLITS],
    trees: Vec<TreeState>,
    computers: Vec<Dynamics>,
    identity: String,
}

impl MultibandCompressor {
    /// Refusals: rate not admitted -> `InvalidRate`; channels out of `1..=MAX_CHANNELS` ->
    /// `InvalidLayout`; band count outside 2..=4 -> `OutOfRange`; NaN/inf -> `NotFinite`; a
    /// crossover out of range or not strictly ascending, or a band parameter out of range ->
    /// `OutOfRange`.
    pub fn new(sample_rate_hz: u32, channels: usize, params: MultibandParams) -> Result<Self, ScoreError> {
        ticks_per_sample(sample_rate_hz)?;
        if !(1..=MAX_CHANNELS).contains(&channels) {
            return Err(ScoreError::InvalidLayout);
        }
        if !(MULTIBAND_BANDS.0..=MULTIBAND_BANDS.1).contains(&params.bands) {
            return Err(ScoreError::OutOfRange);
        }
        let splits_used = &params.crossovers_hz[..params.bands - 1];
        let crossover = Crossover::new(sample_rate_hz, splits_used)?;
        let mut computers = Vec::with_capacity(params.bands);
        for p in &params.band_params[..params.bands] {
            computers.push(Dynamics::new(sample_rate_hz, channels, *p)?);
        }
        let mut identity = format!("multiband:rate={sample_rate_hz};channels={channels};bands={};x=", params.bands);
        for f in splits_used {
            identity.push_str(&format!("{:016x},", f.to_bits()));
        }
        for c in &computers {
            identity.push('[');
            identity.push_str(&c.identity());
            identity.push(']');
        }
        Ok(Self {
            sample_rate_hz,
            channels,
            params,
            splits: crossover.splits,
            trees: vec![TreeState::default(); channels],
            computers,
            identity,
        })
    }
    pub fn params(&self) -> &MultibandParams {
        &self.params
    }
    /// Current smoothed gain reduction of `band` in dB for gain state `index` (0 when linked).
    pub fn gain_reduction_db(&self, band: usize, index: usize) -> Option<f64> {
        self.computers.get(band)?.gain_reduction_db(index)
    }
}

impl Processor for MultibandCompressor {
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
    fn process(
        &mut self,
        input: &InputBlock<'_>,
        out: &mut OutputBlock,
        cancel: &CancellationToken,
    ) -> Result<(), ScoreError> {
        let frames = input.frames();
        let bands = self.params.bands;
        let splits = bands - 1;
        let mut at = 0;
        while at < frames {
            poll(cancel)?;
            let end = (at + CANCEL_CHECK_FRAMES).min(frames);
            for n in at..end {
                let mut split = [[0.0_f64; MAX_BANDS]; MAX_CHANNELS];
                for (ch, values) in split.iter_mut().enumerate().take(self.channels) {
                    let x = f64::from(input.channel(ch)[n]);
                    split_tree(&self.splits, splits, &mut self.trees[ch], x, values);
                }
                let mut sum = [0.0_f64; MAX_CHANNELS];
                for (b, computer) in self.computers.iter_mut().enumerate() {
                    let link = computer.params().link;
                    let makeup = computer.makeup_linear();
                    let mut levels = [0.0_f64; MAX_CHANNELS];
                    for ch in 0..self.channels {
                        levels[ch] = computer.detect(ch, split[ch][b]);
                    }
                    if link {
                        let loudest = levels[..self.channels].iter().fold(0.0_f64, |a, v| a.max(*v));
                        let g = db_to_linear(computer.step(0, loudest)) * makeup;
                        for ch in 0..self.channels {
                            sum[ch] += g * split[ch][b];
                        }
                    } else {
                        for ch in 0..self.channels {
                            let g = db_to_linear(computer.step(ch, levels[ch])) * makeup;
                            sum[ch] += g * split[ch][b];
                        }
                    }
                }
                for (ch, dst) in out.channels_mut().enumerate().take(self.channels) {
                    dst[n] = sum[ch] as f32;
                }
            }
            at = end;
        }
        Ok(())
    }
    fn reset(&mut self) {
        self.trees.fill(TreeState::default());
        for c in &mut self.computers {
            c.reset();
        }
    }
}
