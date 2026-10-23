//! Crate-private second-order sections in `f64` (transposed direct form II) for the crossover,
//! phaser and hum filters. Coefficients follow the W3C "Audio EQ Cookbook" (R. Bristow-Johnson,
//! bilinear transform with frequency prewarping), the same derivation as `eq.rs`.
use std::f64::consts::PI;

/// Butterworth Q for a second-order section.
pub(crate) const BUTTERWORTH_Q: f64 = std::f64::consts::FRAC_1_SQRT_2;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Sos {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

#[derive(Clone, Copy)]
#[allow(clippy::enum_variant_names)] // cookbook names
pub(crate) enum SosKind {
    LowPass,
    HighPass,
    AllPass,
    Notch,
}

impl Sos {
    /// Section of `kind` at `frequency_hz` (caller validated `0 < f < rate/2`, `q > 0`).
    pub(crate) fn new(kind: SosKind, frequency_hz: f64, q: f64, rate: f64) -> Self {
        let w0 = 2.0 * PI * frequency_hz / rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let (b0, b1, b2) = match kind {
            SosKind::LowPass => ((1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0),
            SosKind::HighPass => ((1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0),
            SosKind::AllPass => (1.0 - alpha, -2.0 * cos, 1.0 + alpha),
            SosKind::Notch => (1.0, -2.0 * cos, 1.0),
        };
        let (a0, a1, a2) = (1.0 + alpha, -2.0 * cos, 1.0 - alpha);
        Self { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 }
    }

    /// `|H(e^{jw})|` at `frequency_hz`, evaluated from the transfer function.
    pub(crate) fn magnitude(&self, frequency_hz: f64, rate: f64) -> f64 {
        let w = 2.0 * PI * frequency_hz / rate;
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        let num = ((self.b0 + self.b1 * c1 + self.b2 * c2).powi(2)
            + (self.b1 * s1 + self.b2 * s2).powi(2))
        .sqrt();
        let den = ((1.0 + self.a1 * c1 + self.a2 * c2).powi(2)
            + (self.a1 * s1 + self.a2 * s2).powi(2))
        .sqrt();
        num / den
    }
}

/// Transposed direct form II state of one section.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SosState {
    z1: f64,
    z2: f64,
}

impl SosState {
    #[inline]
    pub(crate) fn run(&mut self, c: &Sos, x: f64) -> f64 {
        let y = c.b0 * x + self.z1;
        self.z1 = c.b1 * x - c.a1 * y + self.z2;
        self.z2 = c.b2 * x - c.a2 * y;
        // Flush values that cannot affect an f32 output, so silent tails never reach denormals.
        if self.z1.abs() < 1e-30 {
            self.z1 = 0.0;
        }
        if self.z2.abs() < 1e-30 {
            self.z2 = 0.0;
        }
        y
    }
}
