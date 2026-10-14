//! 4x oversampling true-peak estimator shared by the lookahead limiter and the loudness meter.
//!
//! Design (own, no third-party coefficients): a 49-tap Kaiser-windowed sinc low-pass
//! (`h[n] = sinc(n / 4) * kaiser(n / 24, beta = 7)`, `n = -24..=24`) split into four polyphase
//! branches. Branch 0 has a single non-zero tap (the centre), so the original samples pass
//! through bit-exactly; branches 1..=3 have 12 taps each and are normalised to unit DC gain.
//! Group delay is [`TRUE_PEAK_DELAY`] original samples. ITU-R BS.1770-4 Annex 2 only requires
//! oversampling and offers a 48-tap filter as an example; this filter is a different, documented
//! design, and its accuracy is measured in `tests/loudness.rs`, not claimed identical.
use std::f64::consts::PI;
use std::sync::OnceLock;

/// Oversampling factor.
pub(crate) const OVERSAMPLE: usize = 4;
/// Taps per interpolating polyphase branch.
pub(crate) const BRANCH_TAPS: usize = 12;
/// Delay of the interpolator in original-rate samples: a value pushed at time `m` describes the
/// signal region `[m - 6, m - 5)`.
pub(crate) const TRUE_PEAK_DELAY: usize = 6;

const HALF_SPAN: i32 = 24;
const KAISER_BETA: f64 = 7.0;

fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0_f64, 1.0_f64);
    let q = x * x / 4.0;
    for k in 1..60 {
        term *= q / (f64::from(k) * f64::from(k));
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

fn sinc(x: f64) -> f64 {
    if x == 0.0 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    }
}

/// Branches 1..=3 (index 0..=2), tap `k` multiplies `x[m - k]`.
fn branches() -> &'static [[f64; BRANCH_TAPS]; 3] {
    static TABLE: OnceLock<[[f64; BRANCH_TAPS]; 3]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let denom = bessel_i0(KAISER_BETA);
        let h = |i: usize| -> f64 {
            let n = i as i32 - HALF_SPAN;
            let r = f64::from(n) / f64::from(HALF_SPAN);
            let window = bessel_i0(KAISER_BETA * (1.0 - r * r).max(0.0).sqrt()) / denom;
            sinc(f64::from(n) / OVERSAMPLE as f64) * window
        };
        let mut table = [[0.0; BRANCH_TAPS]; 3];
        for (b, row) in table.iter_mut().enumerate() {
            let phase = b + 1;
            for (k, tap) in row.iter_mut().enumerate() {
                *tap = h(OVERSAMPLE * k + phase);
            }
            let sum: f64 = row.iter().sum();
            for tap in row.iter_mut() {
                *tap /= sum;
            }
        }
        table
    })
}

/// Per-channel streaming estimator. `push` returns the largest magnitude among the four
/// oversampled values that describe the region `[m - 6, m - 5)`.
#[derive(Clone, Debug)]
pub(crate) struct TruePeakFilter {
    history: [f64; BRANCH_TAPS],
}

impl TruePeakFilter {
    pub(crate) fn new() -> Self {
        Self {
            history: [0.0; BRANCH_TAPS],
        }
    }
    pub(crate) fn reset(&mut self) {
        self.history = [0.0; BRANCH_TAPS];
    }
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        self.history.copy_within(0..BRANCH_TAPS - 1, 1);
        self.history[0] = x;
        // Branch 0 is the centre tap: x[m - 6].
        let mut peak = self.history[TRUE_PEAK_DELAY].abs();
        for row in branches() {
            let mut acc = 0.0;
            for (tap, h) in row.iter().zip(&self.history) {
                acc += tap * h;
            }
            peak = peak.max(acc.abs());
        }
        peak
    }
}
