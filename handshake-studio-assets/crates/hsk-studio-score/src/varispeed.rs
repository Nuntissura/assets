//! Clip speed other than 1 (varispeed: duration and pitch change together) by band-limited
//! windowed-sinc interpolation.
//!
//! Output frame `m` of a clip played at speed `r = numerator / denominator` reads the source at the
//! exact rational position `p(m) = m r` (integer part and remainder kept in `u128`, so positions
//! never drift) and evaluates the band-limited source there:
//!
//! ```text
//! y(m) = sum_n x[n] h(p(m) - n),   h(t) = nu_c K(nu_c t),   K(u) = sinc(u) w(u / Z)
//! ```
//!
//! with `sinc(u) = sin(pi u)/(pi u)`, `w` the Kaiser window `I0(beta sqrt(1 - x^2)) / I0(beta)`
//! (J. F. Kaiser, "Nonrecursive digital filter design using the I0-sinh window function", 1974),
//! `Z = 64` zero crossings each side and `beta = 0.1102 (A - 8.7)` for a design stopband of
//! `A = 100 dB`. The cutoff is `nu_c = 0.95 min(1, 1/r)` of the source Nyquist frequency: by the
//! Kaiser length formula the transition band of this kernel is `0.1 nu_c` wide and centred on
//! `nu_c`, so the passband reaches `0.9025 min(1, 1/r)` and the stopband starts at
//! `0.9976 min(1, 1/r)`, below the Nyquist frequency of the output. Speeding up (`r > 1`) thus
//! removes everything that would alias, and slowing down (`r < 1`) suppresses the interpolation
//! images; both are measured in `tests/varispeed.rs`. The kernel is read from a table of `L = 4096`
//! points per zero crossing with linear interpolation (relative error about 1e-7, below the design
//! stopband). The method is the band-limited interpolation of J. O. Smith and P. Gossett, "A
//! flexible sampling-rate conversion method", ICASSP 1984 (re-derived here; no source copied).
//!
//! `reverse` plays the window backwards (the forward rule applied to the time-reversed source).
//! Source samples outside the clip window are used as handles when the source has them, else they
//! are zero. Speed 1 forward is an exact copy (no filtering).
use crate::error::ScoreError;
use std::sync::OnceLock;

/// Kernel zero crossings each side.
pub const VARISPEED_ZERO_CROSSINGS: usize = 64;
/// Design stopband attenuation in dB.
pub const VARISPEED_STOPBAND_DB: f64 = 100.0;
/// Cutoff as a fraction of the lower of the source and output Nyquist frequencies.
pub const VARISPEED_CUTOFF: f64 = 0.95;
/// Speed range (inclusive): 1/16 .. 16; numerator and denominator in 1 ..= 65536.
pub const VARISPEED_MAX_RATIO: u64 = 16;
pub const VARISPEED_MAX_TERM: u64 = 65_536;
const TABLE_RESOLUTION: usize = 4096;

/// Rational clip speed; `reverse` plays backwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClipSpeed {
    numerator: u64,
    denominator: u64,
    reverse: bool,
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

impl ClipSpeed {
    pub const UNITY: Self = Self { numerator: 1, denominator: 1, reverse: false };
    /// Reduced `numerator / denominator`. Refusals: a zero term or one above 65536, or a ratio
    /// outside 1/16 ..= 16 -> `OutOfRange`.
    pub fn new(numerator: u64, denominator: u64, reverse: bool) -> Result<Self, ScoreError> {
        if !(1..=VARISPEED_MAX_TERM).contains(&numerator) || !(1..=VARISPEED_MAX_TERM).contains(&denominator) {
            return Err(ScoreError::OutOfRange);
        }
        if numerator > VARISPEED_MAX_RATIO * denominator || denominator > VARISPEED_MAX_RATIO * numerator {
            return Err(ScoreError::OutOfRange);
        }
        let g = gcd(numerator, denominator);
        Ok(Self { numerator: numerator / g, denominator: denominator / g, reverse })
    }
    pub fn numerator(&self) -> u64 {
        self.numerator
    }
    pub fn denominator(&self) -> u64 {
        self.denominator
    }
    pub fn reverse(&self) -> bool {
        self.reverse
    }
    pub fn ratio(&self) -> f64 {
        self.numerator as f64 / self.denominator as f64
    }
    /// Output frames for a source window of `source_frames`: `ceil(source_frames / r)`, so every
    /// output position lies inside the window.
    pub fn output_frames(&self, source_frames: u64) -> u64 {
        let scaled = u128::from(source_frames) * u128::from(self.denominator);
        scaled.div_ceil(u128::from(self.numerator)) as u64
    }
    /// Cutoff of the band-limiting kernel as a fraction of the source Nyquist frequency.
    pub fn cutoff(&self) -> f64 {
        VARISPEED_CUTOFF * (self.denominator as f64 / self.numerator as f64).min(1.0)
    }
}

fn bessel_i0(x: f64) -> f64 {
    // Power series sum_k ((x/2)^k / k!)^2, to full f64 precision for the beta used here.
    let (mut sum, mut term, half) = (1.0, 1.0, x / 2.0);
    for k in 1..200 {
        term *= half / k as f64;
        let t2 = term * term;
        sum += t2;
        if t2 < sum * 1e-18 {
            break;
        }
    }
    sum
}

/// `K(u)` sampled at `u = i / L`, `i = 0 ..= Z L` (plus one zero guard entry).
fn kernel_table() -> &'static [f64] {
    static TABLE: OnceLock<Vec<f64>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let z = VARISPEED_ZERO_CROSSINGS as f64;
        let beta = 0.1102 * (VARISPEED_STOPBAND_DB - 8.7);
        let norm = bessel_i0(beta);
        let n = VARISPEED_ZERO_CROSSINGS * TABLE_RESOLUTION;
        let mut t = Vec::with_capacity(n + 2);
        for i in 0..=n {
            let u = i as f64 / TABLE_RESOLUTION as f64;
            let sinc = if i == 0 { 1.0 } else { (std::f64::consts::PI * u).sin() / (std::f64::consts::PI * u) };
            let x = u / z;
            let w = bessel_i0(beta * (1.0 - x * x).max(0.0).sqrt()) / norm;
            t.push(sinc * w);
        }
        t.push(0.0);
        t
    })
}

#[inline]
fn kernel(table: &[f64], u: f64) -> f64 {
    let pos = u.abs() * TABLE_RESOLUTION as f64;
    let i = pos as usize;
    if i + 1 >= table.len() {
        return 0.0;
    }
    let frac = pos - i as f64;
    table[i] + frac * (table[i + 1] - table[i])
}

/// Renders a clip window at `speed`: `out[m]` for `m in 0..out.len()` with
/// `out.len() == speed.output_frames(window_frames)`. `source` is one channel; samples of
/// `source` outside `window_start .. window_start + window_frames` serve as handles. Refusals: a
/// window past the end of `source` or an empty window -> `OutOfRange`; a wrong output length ->
/// `LengthMismatch`; a non-finite source sample -> `NotFinite`.
pub fn render_at_speed(
    source: &[f32],
    window_start: usize,
    window_frames: usize,
    speed: ClipSpeed,
    out: &mut [f32],
) -> Result<(), ScoreError> {
    let end = window_start.checked_add(window_frames).ok_or(ScoreError::OutOfRange)?;
    if window_frames == 0 || end > source.len() {
        return Err(ScoreError::OutOfRange);
    }
    if out.len() as u64 != speed.output_frames(window_frames as u64) {
        return Err(ScoreError::LengthMismatch);
    }
    if source.iter().any(|v| !v.is_finite()) {
        return Err(ScoreError::NotFinite);
    }
    // Source sample at window-relative index `i` (reversed when playing backwards); handles
    // outside the source are zero.
    let at = |i: i64| -> f64 {
        let abs = if speed.reverse { end as i64 - 1 - i } else { window_start as i64 + i };
        if abs < 0 || abs >= source.len() as i64 { 0.0 } else { f64::from(source[abs as usize]) }
    };
    if speed.numerator == 1 && speed.denominator == 1 {
        for (m, o) in out.iter_mut().enumerate() {
            *o = at(m as i64) as f32;
        }
        return Ok(());
    }
    let table = kernel_table();
    let nu = speed.cutoff();
    let reach = (VARISPEED_ZERO_CROSSINGS as f64 / nu).ceil() as i64;
    let (num, den) = (u128::from(speed.numerator), u128::from(speed.denominator));
    for (m, o) in out.iter_mut().enumerate() {
        let scaled = m as u128 * num;
        let whole = (scaled / den) as i64;
        let frac = (scaled % den) as f64 / den as f64;
        let mut acc = 0.0;
        for n in whole - reach..=whole + reach + 1 {
            let t = (whole - n) as f64 + frac;
            let h = kernel(table, nu * t);
            if h != 0.0 {
                acc += at(n) * h;
            }
        }
        *o = (acc * nu) as f32;
    }
    Ok(())
}
