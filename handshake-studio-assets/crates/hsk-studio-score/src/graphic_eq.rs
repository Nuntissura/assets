//! Graphic equalizer with 10, 20 or 30 bands (spec rows `GraphicEQ10Bands`, `GraphicEQ20Bands`,
//! `GraphicEQ30Bands`, STU-FX-136a; lane SMB-SCORE-EFFECTS-6).
//!
//! **Design.** One second-order peak section per band in `f64`, run as a cascade (transposed
//! direct form II, state below 1e-30 flushed to zero like `filters.rs`), after V. Valimaki and
//! J. Liski / J. Liski and V. Valimaki, "The Quest for the Best Graphic Equalizer", DAFx-17
//! (re-derived; no third-party code). Band `k` with gain `g` dB, linear `G = 10^(g/20)`, band-edge
//! gain `G_B = 10^(c g / 20)` and bandwidth `B`:
//!
//! ```text
//! H(z) = (1 + G b - 2 cos(w) z^-1 + (1 - G b) z^-2) / (1 + b - 2 cos(w) z^-1 + (1 - b) z^-2)
//! b    = sqrt(|G_B^2 - 1| / |G^2 - G_B^2|) tan(B / 2)
//! ```
//!
//! A band gain with `|g| < 1e-9` dB is the identity section (exactly `1`), so all-zero commands are
//! bit-exact identity. Interaction factor `c`: 0.3 at 10 bands and 0.4 at 30 bands (the paper);
//! 0.37 at 20 bands (not published: swept 0.30..0.40 in 0.01 steps over the error sets of
//! `tests/graphic_eq.rs`, best kept; lane notes). Bandwidth: at construction, bisection (60 steps)
//! chooses `B` so the prototype section at `g_p = 17` dB reads `c g_p` at its lower neighbour's
//! centre (band 0: its upper neighbour; a lone band: one authored spacing below).
//!
//! Gains: `M = 2N - 1` design points (band centres and the geometric means of neighbours), target
//! the command at a centre and the mean of the two commands between them. The interaction matrix
//! `A[m][k] = dB(H_k at w_m) / g_p` of the prototype sections is solved by least squares
//! (Householder QR), then rebuilt once from the gains of that first pass (a column with a gain of
//! `|g| < 1e-9` keeps its prototype) and solved again; the second solution is applied.
//!
//! Band centres are authored (the spec declares none; vendor bands UNVERIFIED, D5): 10 bands
//! `1000 * 2^k`, `k = -5..=4`; 20 bands `1000 * 2^(k/2)`, `k = -11..=8`; 30 bands `1000 * 2^(k/3)`,
//! `k = -16..=13`. A centre at or above 0.95 Nyquist is dropped (the rule of `HumRemover`); its
//! command stays stored, and [`GraphicEq::active_bands`] reports the count designed. With no active
//! band only the master `gain` applies.
//!
//! **Parameters** (spec sheets, units authored: dB): `gain` -64..64 is a master gain applied at
//! once; `amp1..ampN` -70..70 are the band commands. `accuracy` (10..8192) and `range` (1.5..120)
//! have no declared meaning (`bind_spec_row` refuses them): they are range checked, stored and
//! have no effect on the sound.
//!
//! **Automation cost.** A design is a matrix rebuild and two solves, too much per sample, so a band
//! command set by [`Automatable::set_automated`] takes effect at the next design hop: the processor
//! designs at most once per [`GRAPHIC_EQ_DESIGN_HOP`] frames, at stream positions that are
//! multiples of the hop (counted from the stream start), and only when a command changed. A set
//! before frame 0 designs at frame 0. All buffers are allocated at construction; `process` and a
//! design allocate nothing; latency 0; any block partition gives the same output.
//!
//! [`Automatable::set_automated`]: crate::Automatable::set_automated
#![allow(clippy::needless_range_loop)] // index loops mirror the matrix and band formulas
use crate::block::{InputBlock, OutputBlock};
use crate::dsp::{db_to_linear, normalised_to_value, poll};
use crate::engine::Processor;
use crate::error::ScoreError;
use crate::layout::{CANCEL_CHECK_FRAMES, ChannelLayout, MAX_CHANNELS, ticks_per_sample};
use crate::session::{AutomationParam, check_automated};
use hsk_studio_accord::CancellationToken;
use std::f64::consts::PI;

/// Band counts of the three spec rows.
pub const GRAPHIC_EQ_BANDS: [usize; 3] = [10, 20, 30];
pub const MAX_GRAPHIC_EQ_BANDS: usize = 30;
/// Spec hard ranges (STU-FX-136a); units authored (dB for `gain` and `amp*`).
pub const GRAPHIC_EQ_ACCURACY: (f64, f64) = (10.0, 8192.0);
pub const GRAPHIC_EQ_GAIN_DB: (f64, f64) = (-64.0, 64.0);
pub const GRAPHIC_EQ_RANGE: (f64, f64) = (1.5, 120.0);
pub const GRAPHIC_EQ_AMP_DB: (f64, f64) = (-70.0, 70.0);
/// Most frames between two designs of automated band commands (`layout::CANCEL_CHECK_FRAMES`).
pub const GRAPHIC_EQ_DESIGN_HOP: usize = CANCEL_CHECK_FRAMES;
/// Prototype gain of the interaction matrix (the paper's `g_p`).
pub const GRAPHIC_EQ_PROTOTYPE_DB: f64 = 17.0;

/// Band gains below this magnitude (dB) are the identity section.
const NEAR_ZERO_DB: f64 = 1e-9;
/// A solved band gain is kept inside this bound (a numerical guard for degenerate solves).
const SOLVED_GAIN_GUARD_DB: f64 = 200.0;
const BISECTION_STEPS: usize = 60;
const FLUSH: f64 = 1e-30;
const MAX_POINTS: usize = 2 * MAX_GRAPHIC_EQ_BANDS - 1;

const fn amp_param(name: &'static str) -> AutomationParam {
    AutomationParam { name, min: GRAPHIC_EQ_AMP_DB.0, max: GRAPHIC_EQ_AMP_DB.1 }
}

/// Automation order: 0 = `gain`, then `amp1 ..= ampN`.
const AUTOMATION: [AutomationParam; MAX_GRAPHIC_EQ_BANDS + 1] = [
    AutomationParam { name: "gain", min: GRAPHIC_EQ_GAIN_DB.0, max: GRAPHIC_EQ_GAIN_DB.1 },
    amp_param("amp1"),
    amp_param("amp2"),
    amp_param("amp3"),
    amp_param("amp4"),
    amp_param("amp5"),
    amp_param("amp6"),
    amp_param("amp7"),
    amp_param("amp8"),
    amp_param("amp9"),
    amp_param("amp10"),
    amp_param("amp11"),
    amp_param("amp12"),
    amp_param("amp13"),
    amp_param("amp14"),
    amp_param("amp15"),
    amp_param("amp16"),
    amp_param("amp17"),
    amp_param("amp18"),
    amp_param("amp19"),
    amp_param("amp20"),
    amp_param("amp21"),
    amp_param("amp22"),
    amp_param("amp23"),
    amp_param("amp24"),
    amp_param("amp25"),
    amp_param("amp26"),
    amp_param("amp27"),
    amp_param("amp28"),
    amp_param("amp29"),
    amp_param("amp30"),
];

/// Interaction factor `c` of a band count (module docs); `None` for an unsupported count.
pub fn graphic_eq_interaction(bands: usize) -> Option<f64> {
    match bands {
        10 => Some(0.3),
        20 => Some(0.37),
        30 => Some(0.4),
        _ => None,
    }
}

/// Authored centre of band `band` (0 = lowest) of a `bands` equalizer in Hz (module docs).
pub fn graphic_eq_centre_hz(bands: usize, band: usize) -> Option<f64> {
    if band >= bands {
        return None;
    }
    let k = band as f64;
    match bands {
        10 => Some(1000.0 * 2f64.powf(k - 5.0)),
        20 => Some(1000.0 * 2f64.powf((k - 11.0) / 2.0)),
        30 => Some(1000.0 * 2f64.powf((k - 16.0) / 3.0)),
        _ => None,
    }
}

/// Spacing ratio between neighbouring authored centres.
fn spacing(bands: usize) -> f64 {
    match bands {
        10 => 2.0,
        20 => std::f64::consts::SQRT_2,
        _ => 2f64.powf(1.0 / 3.0),
    }
}

/// The stored commands of one graphic equalizer (spec parameter names in the docs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GraphicEqParams {
    /// 10, 20 or 30.
    pub bands: usize,
    /// Spec `accuracy`: stored, no effect on the sound (no declared meaning).
    pub accuracy: f64,
    /// Spec `gain`: master gain in dB.
    pub gain_db: f64,
    /// Spec `range`: stored, no effect on the sound (no declared meaning).
    pub range: f64,
    /// Spec `amp1 ..= ampN` in dB; entries from `bands` on are unused.
    pub amps_db: [f64; MAX_GRAPHIC_EQ_BANDS],
}

impl GraphicEqParams {
    /// The spec defaults: `accuracy` 999.997454, `gain` 0, `range` 47.808615, every `amp` 0.
    pub fn flat(bands: usize) -> Self {
        Self { bands, accuracy: 999.997454, gain_db: 0.0, range: 47.808615, amps_db: [0.0; MAX_GRAPHIC_EQ_BANDS] }
    }
    /// `OutOfRange` for a band count other than 10, 20 or 30 or a value outside its hard range;
    /// `NotFinite` for NaN or infinity (the first `bands` commands are checked).
    pub fn validate(&self) -> Result<(), ScoreError> {
        if !GRAPHIC_EQ_BANDS.contains(&self.bands) {
            return Err(ScoreError::OutOfRange);
        }
        let fixed = [(self.accuracy, GRAPHIC_EQ_ACCURACY), (self.gain_db, GRAPHIC_EQ_GAIN_DB), (self.range, GRAPHIC_EQ_RANGE)];
        let amps = self.amps_db[..self.bands].iter().map(|a| (*a, GRAPHIC_EQ_AMP_DB));
        let all: Vec<(f64, (f64, f64))> = fixed.into_iter().chain(amps).collect();
        if all.iter().any(|(v, _)| !v.is_finite()) {
            return Err(ScoreError::NotFinite);
        }
        if all.iter().any(|(v, (lo, hi))| v < lo || v > hi) {
            return Err(ScoreError::OutOfRange);
        }
        Ok(())
    }
    /// Builds from the normalised slots in sheet order (`accuracy`, `gain`, `range`, `amp1 ..=
    /// ampN`, i.e. slots in0, in1, in2, in4 ..), each mapped by its row's affine expression
    /// ([`crate::spec_parameter_rows`]). `LengthMismatch` for a slot count other than `bands + 3`,
    /// `NotFinite`, `OutOfRange` outside `0..=1`.
    pub fn from_normalised(bands: usize, slots: &[f64]) -> Result<Self, ScoreError> {
        let rows = crate::spec_rows::graphic_eq_rows(bands).ok_or(ScoreError::OutOfRange)?;
        if slots.len() != rows.len() {
            return Err(ScoreError::LengthMismatch);
        }
        if slots.iter().any(|n| !n.is_finite()) {
            return Err(ScoreError::NotFinite);
        }
        if slots.iter().any(|n| !(0.0..=1.0).contains(n)) {
            return Err(ScoreError::OutOfRange);
        }
        let map = |i: usize| {
            let row = &rows[i];
            normalised_to_value(slots[i], row.hard_min.unwrap_or(0.0), row.hard_max.unwrap_or(1.0))
        };
        let mut params = Self::flat(bands);
        params.accuracy = map(0);
        params.gain_db = map(1);
        params.range = map(2);
        for k in 0..bands {
            params.amps_db[k] = map(k + 3);
        }
        params.validate()?;
        Ok(params)
    }
}

/// One normalised second-order section (`a0 = 1`).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Section {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Section {
    const IDENTITY: Self = Self { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 };

    /// Peak section at `w` (rad/sample) with bandwidth `bandwidth` (rad) and gain `gain_db`.
    fn peak(w: f64, bandwidth: f64, gain_db: f64, c: f64) -> Self {
        if gain_db.abs() < NEAR_ZERO_DB || !gain_db.is_finite() {
            return Self::IDENTITY;
        }
        let g = 10f64.powf(gain_db / 20.0);
        let gb = 10f64.powf(c * gain_db / 20.0);
        let beta = ((gb * gb - 1.0).abs() / (g * g - gb * gb).abs()).sqrt() * (bandwidth / 2.0).tan();
        let a0 = 1.0 + beta;
        let mid = -2.0 * w.cos() / a0;
        Self { b0: (1.0 + g * beta) / a0, b1: mid, b2: (1.0 - g * beta) / a0, a1: mid, a2: (1.0 - beta) / a0 }
    }

    /// `|H|` from `[cos w, sin w, cos 2w, sin 2w]`.
    fn magnitude(&self, trig: &[f64; 4]) -> f64 {
        let [c1, s1, c2, s2] = *trig;
        let num = ((self.b0 + self.b1 * c1 + self.b2 * c2).powi(2) + (self.b1 * s1 + self.b2 * s2).powi(2)).sqrt();
        let den = ((1.0 + self.a1 * c1 + self.a2 * c2).powi(2) + (self.a1 * s1 + self.a2 * s2).powi(2)).sqrt();
        num / den
    }

    fn db(&self, trig: &[f64; 4]) -> f64 {
        20.0 * self.magnitude(trig).log10()
    }

    /// Transposed direct form II step with the 1e-30 flush.
    #[inline]
    fn run(&self, z: &mut [f64; 2], x: f64) -> f64 {
        let y = self.b0 * x + z[0];
        z[0] = self.b1 * x - self.a1 * y + z[1];
        z[1] = self.b2 * x - self.a2 * y;
        if z[0].abs() < FLUSH {
            z[0] = 0.0;
        }
        if z[1].abs() < FLUSH {
            z[1] = 0.0;
        }
        y
    }
}

fn trig(w: f64) -> [f64; 4] {
    let (s1, c1) = w.sin_cos();
    let (s2, c2) = (2.0 * w).sin_cos();
    [c1, s1, c2, s2]
}

/// Least squares `min |A x - b|` by Householder QR, in place (`a` is `m x n` row-major, `m >= n`);
/// `v` is `m` scratch; a diagonal below 1e-12 of the largest gives `x_j = 0`. Allocation-free.
fn least_squares(a: &mut [f64], m: usize, n: usize, b: &mut [f64], v: &mut [f64], x: &mut [f64]) {
    for j in 0..n {
        let norm = (j..m).map(|i| a[i * n + j] * a[i * n + j]).sum::<f64>().sqrt();
        if norm == 0.0 {
            continue;
        }
        let alpha = if a[j * n + j] > 0.0 { -norm } else { norm };
        for i in j..m {
            v[i] = a[i * n + j];
        }
        v[j] -= alpha;
        let vv: f64 = (j..m).map(|i| v[i] * v[i]).sum();
        if vv == 0.0 {
            continue;
        }
        for col in j..n {
            let dot: f64 = (j..m).map(|i| v[i] * a[i * n + col]).sum();
            let f = 2.0 * dot / vv;
            for i in j..m {
                a[i * n + col] -= f * v[i];
            }
        }
        let dot: f64 = (j..m).map(|i| v[i] * b[i]).sum();
        let f = 2.0 * dot / vv;
        for i in j..m {
            b[i] -= f * v[i];
        }
    }
    let largest = (0..n).map(|j| a[j * n + j].abs()).fold(0.0_f64, f64::max);
    for j in (0..n).rev() {
        let mut s = b[j];
        for col in j + 1..n {
            s -= a[j * n + col] * x[col];
        }
        let d = a[j * n + j];
        x[j] = if d.abs() > 1e-12 * largest && d != 0.0 { s / d } else { 0.0 };
    }
}

/// Graphic equalizer (module docs).
#[derive(Clone, Debug)]
pub struct GraphicEq {
    sample_rate_hz: u32,
    channels: usize,
    params: GraphicEqParams,
    c: f64,
    active: usize,
    /// Band centres in rad/sample and their bisected bandwidths (active bands).
    w: [f64; MAX_GRAPHIC_EQ_BANDS],
    bandwidth: [f64; MAX_GRAPHIC_EQ_BANDS],
    /// Design-point trigonometry (`2 active - 1` entries).
    points: Vec<[f64; 4]>,
    /// Prototype interaction matrix, `points x active`, row-major.
    proto: Vec<f64>,
    work: Vec<f64>,
    target: Vec<f64>,
    rhs: Vec<f64>,
    scratch: Vec<f64>,
    first: Vec<f64>,
    solved: Vec<f64>,
    /// Applied sections and the gains they were designed for.
    sections: [Section; MAX_GRAPHIC_EQ_BANDS],
    band_gains: [f64; MAX_GRAPHIC_EQ_BANDS],
    master: f64,
    /// Filter memories, `channels x active`.
    memory: Vec<[f64; 2]>,
    position: u64,
    dirty: bool,
    designs: u64,
    identity: String,
}

impl GraphicEq {
    /// Refusals: `InvalidRate` (rate not admitted), `InvalidLayout` (channels outside
    /// `1..=MAX_CHANNELS`), then [`GraphicEqParams::validate`]. Designs once.
    pub fn new(sample_rate_hz: u32, channels: usize, params: GraphicEqParams) -> Result<Self, ScoreError> {
        if sample_rate_hz > 0 {
            return Err(ScoreError::OutOfRange); // red stub (lane SMB-SCORE-EFFECTS-6)
        }
        ticks_per_sample(sample_rate_hz)?;
        if !(1..=MAX_CHANNELS).contains(&channels) {
            return Err(ScoreError::InvalidLayout);
        }
        params.validate()?;
        let bands = params.bands;
        let c = graphic_eq_interaction(bands).ok_or(ScoreError::OutOfRange)?;
        let rate = f64::from(sample_rate_hz);
        let mut w = [0.0; MAX_GRAPHIC_EQ_BANDS];
        let mut active = 0;
        for k in 0..bands {
            let f = graphic_eq_centre_hz(bands, k).ok_or(ScoreError::OutOfRange)?;
            if f >= 0.95 * rate / 2.0 {
                break;
            }
            w[k] = 2.0 * PI * f / rate;
            active += 1;
        }
        let mut bandwidth = [0.0; MAX_GRAPHIC_EQ_BANDS];
        for k in 0..active {
            let neighbour = if k >= 1 {
                w[k - 1]
            } else if active >= 2 {
                w[1]
            } else {
                w[0] / spacing(bands)
            };
            bandwidth[k] = Self::bisect(w[k], &trig(neighbour), c);
        }
        let mut points = Vec::with_capacity(MAX_POINTS);
        for k in 0..active {
            points.push(trig(w[k]));
            if k + 1 < active {
                points.push(trig((w[k] * w[k + 1]).sqrt()));
            }
        }
        let m = points.len();
        let mut proto = vec![0.0; m * active];
        for k in 0..active {
            let s = Section::peak(w[k], bandwidth[k], GRAPHIC_EQ_PROTOTYPE_DB, c);
            for (row, p) in points.iter().enumerate() {
                proto[row * active + k] = s.db(p) / GRAPHIC_EQ_PROTOTYPE_DB;
            }
        }
        let mut amps = String::new();
        for a in &params.amps_db[..bands] {
            amps.push_str(&format!("{:016x},", a.to_bits()));
        }
        let identity = format!(
            "graphic_eq_{bands}_bands:rate={sample_rate_hz};channels={channels};accuracy={:016x};range={:016x};gain={:016x};amps={amps}",
            params.accuracy.to_bits(),
            params.range.to_bits(),
            params.gain_db.to_bits()
        );
        let mut eq = Self {
            sample_rate_hz,
            channels,
            params,
            c,
            active,
            w,
            bandwidth,
            points,
            work: vec![0.0; m * active],
            proto,
            target: vec![0.0; m],
            rhs: vec![0.0; m],
            scratch: vec![0.0; m],
            first: vec![0.0; active],
            solved: vec![0.0; active],
            sections: [Section::IDENTITY; MAX_GRAPHIC_EQ_BANDS],
            band_gains: [0.0; MAX_GRAPHIC_EQ_BANDS],
            master: db_to_linear(params.gain_db),
            memory: vec![[0.0; 2]; channels * active],
            position: 0,
            dirty: false,
            designs: 0,
            identity,
        };
        eq.design();
        Ok(eq)
    }

    /// Bandwidth whose prototype section reads `c g_p` at the neighbour (60 bisection steps).
    fn bisect(w: f64, neighbour: &[f64; 4], c: f64) -> f64 {
        let target = c * GRAPHIC_EQ_PROTOTYPE_DB;
        let (mut lo, mut hi) = (1e-9, PI - 1e-9);
        for _ in 0..BISECTION_STEPS {
            let mid = 0.5 * (lo + hi);
            if Section::peak(w, mid, GRAPHIC_EQ_PROTOTYPE_DB, c).db(neighbour) < target {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        0.5 * (lo + hi)
    }

    /// Two-pass least-squares design of the band gains from the current commands (module docs).
    fn design(&mut self) {
        let n = self.active;
        let m = self.points.len();
        self.designs = self.designs.saturating_add(1);
        self.dirty = false;
        if n == 0 {
            return;
        }
        let amps = &self.params.amps_db;
        for k in 0..n {
            self.target[2 * k] = amps[k];
            if k + 1 < n {
                self.target[2 * k + 1] = 0.5 * (amps[k] + amps[k + 1]);
            }
        }
        self.work.copy_from_slice(&self.proto);
        self.rhs.copy_from_slice(&self.target);
        least_squares(&mut self.work, m, n, &mut self.rhs, &mut self.scratch, &mut self.first);
        for k in 0..n {
            let g = self.first[k];
            if g.abs() < NEAR_ZERO_DB || !g.is_finite() {
                for row in 0..m {
                    self.work[row * n + k] = self.proto[row * n + k];
                }
            } else {
                let s = Section::peak(self.w[k], self.bandwidth[k], g, self.c);
                for (row, p) in self.points.iter().enumerate() {
                    self.work[row * n + k] = s.db(p) / g;
                }
            }
        }
        self.rhs.copy_from_slice(&self.target);
        least_squares(&mut self.work, m, n, &mut self.rhs, &mut self.scratch, &mut self.solved);
        for k in 0..n {
            let raw = self.solved[k];
            let g = if raw.is_finite() { raw.clamp(-SOLVED_GAIN_GUARD_DB, SOLVED_GAIN_GUARD_DB) } else { 0.0 };
            self.band_gains[k] = g;
            self.sections[k] = Section::peak(self.w[k], self.bandwidth[k], g, self.c);
        }
    }

    /// The stored commands.
    pub fn params(&self) -> &GraphicEqParams {
        &self.params
    }
    /// Bands below 0.95 Nyquist (designed); the rest are stored only.
    pub fn active_bands(&self) -> usize {
        self.active
    }
    /// Designs done so far (construction included).
    pub fn designs(&self) -> u64 {
        self.designs
    }
    /// The interaction factor `c` in use.
    pub fn interaction(&self) -> f64 {
        self.c
    }
    /// Gains in dB of the applied sections (active bands).
    pub fn band_gains_db(&self) -> &[f64] {
        &self.band_gains[..self.active]
    }
    /// Analytic magnitude in dB of the applied sections and the master gain at `frequency_hz`.
    pub fn magnitude_db(&self, frequency_hz: f64) -> f64 {
        let t = trig(2.0 * PI * frequency_hz / f64::from(self.sample_rate_hz));
        let linear = self.sections[..self.active].iter().fold(self.master, |acc, s| acc * s.magnitude(&t));
        20.0 * linear.log10()
    }
    /// Largest pole radius over the applied sections (stability: below 1).
    pub fn max_pole_radius(&self) -> f64 {
        self.sections[..self.active]
            .iter()
            .map(|s| {
                // Poles of z^2 + a1 z + a2: complex pair radius sqrt(a2), else the larger real root.
                let disc = s.a1 * s.a1 - 4.0 * s.a2;
                if disc < 0.0 {
                    s.a2.abs().sqrt()
                } else {
                    let r = disc.sqrt();
                    ((-s.a1 + r) / 2.0).abs().max(((-s.a1 - r) / 2.0).abs())
                }
            })
            .fold(0.0, f64::max)
    }
}

impl Processor for GraphicEq {
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
        let hop = GRAPHIC_EQ_DESIGN_HOP as u64;
        let n = self.active;
        let mut at = 0;
        while at < frames {
            poll(cancel)?;
            let end = (at + CANCEL_CHECK_FRAMES).min(frames);
            let mut seg = at;
            while seg < end {
                let phase = self.position % hop;
                if phase == 0 && self.dirty {
                    self.design();
                }
                let to_boundary = (hop - phase) as usize;
                let seg_end = end.min(seg + to_boundary);
                let master = self.master;
                for (ch, dst) in out.channels_mut().enumerate().take(self.channels) {
                    let src = &input.channel(ch)[seg..seg_end];
                    let memory = &mut self.memory[ch * n..(ch + 1) * n];
                    for (d, s) in dst[seg..seg_end].iter_mut().zip(src) {
                        let mut x = f64::from(*s);
                        for (section, z) in self.sections[..n].iter().zip(memory.iter_mut()) {
                            x = section.run(z, x);
                        }
                        *d = (x * master) as f32;
                    }
                }
                self.position += (seg_end - seg) as u64;
                seg = seg_end;
            }
            at = end;
        }
        Ok(())
    }
    /// Back to the stream start: memories cleared, position 0, and the current commands are
    /// designed again before the next sample.
    fn reset(&mut self) {
        self.memory.fill([0.0; 2]);
        self.position = 0;
        self.dirty = true;
    }
    /// Checkpoint: stream position, pending-design flag, applied gains and sections, memories.
    fn save_state(&self) -> Option<Vec<u8>> {
        let n = self.active;
        let mut coefficients = Vec::with_capacity(5 * n);
        for s in &self.sections[..n] {
            coefficients.extend_from_slice(&[s.b0, s.b1, s.b2, s.a1, s.a2]);
        }
        let memory: Vec<f64> = self.memory.iter().flatten().copied().collect();
        Some(
            crate::state::StateWriter::new()
                .u64(self.position)
                .u64(u64::from(self.dirty))
                .f64s(&self.band_gains[..n])
                .f64s(&coefficients)
                .f64s(&memory)
                .finish(),
        )
    }
    fn restore_state(&mut self, state: &[u8]) -> Result<(), ScoreError> {
        let n = self.active;
        let mut r = crate::state::StateReader::new(state);
        let position = r.u64()?;
        let dirty = match r.u64()? {
            0 => false,
            1 => true,
            _ => return Err(ScoreError::LengthMismatch),
        };
        let mut gains = vec![0.0; n];
        r.f64s(&mut gains)?;
        let mut coefficients = vec![0.0; 5 * n];
        r.f64s(&mut coefficients)?;
        let mut memory = vec![0.0; 2 * self.memory.len()];
        r.f64s(&mut memory)?;
        r.finish()?;
        self.position = position;
        self.dirty = dirty;
        self.band_gains[..n].copy_from_slice(&gains);
        for (s, c) in self.sections[..n].iter_mut().zip(coefficients.chunks_exact(5)) {
            *s = Section { b0: c[0], b1: c[1], b2: c[2], a1: c[3], a2: c[4] };
        }
        for (dst, src) in self.memory.iter_mut().zip(memory.chunks_exact(2)) {
            *dst = [src[0], src[1]];
        }
        Ok(())
    }
}

/// Automatable: 0 = `gain` (master dB, at once), `k` = `ampk` for `k = 1..=bands` (dB; designed at
/// the next design hop, module docs). `accuracy` and `range` are not automatable.
impl crate::session::Automatable for GraphicEq {
    fn automation_parameters(&self) -> Vec<AutomationParam> {
        AUTOMATION[..=self.params.bands].to_vec()
    }
    fn set_automated(&mut self, index: usize, value: f64) -> Result<(), ScoreError> {
        check_automated(&AUTOMATION[..=self.params.bands], index, value)?;
        if index == 0 {
            self.params.gain_db = value;
            self.master = db_to_linear(value);
        } else {
            self.params.amps_db[index - 1] = value;
            self.dirty = true;
        }
        Ok(())
    }
}
