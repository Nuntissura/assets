//! Monotone Hermite tone curves (STU-CON-044 `resolved_curves`). Points are authored in
//! `levels_0_255`; evaluation happens in normalised binary64 with separately written operations
//! (no FMA, no LUT). Authored points are never sorted, deduplicated or repaired.
use crate::DevelopError;

pub const MIN_POINTS: usize = 2;
pub const MAX_POINTS: usize = 32;

#[derive(Clone, Debug, PartialEq)]
pub struct Curve {
    points: Vec<(f64, f64)>,
}

impl Curve {
    pub fn identity() -> Self {
        Self {
            points: vec![(0.0, 0.0), (255.0, 255.0)],
        }
    }

    /// 2..32 finite points in `levels_0_255`, strictly ascending x, nondecreasing y,
    /// endpoints exactly (0,0) and (255,255).
    pub fn new(points: Vec<(f64, f64)>) -> Result<Self, DevelopError> {
        if !(MIN_POINTS..=MAX_POINTS).contains(&points.len()) {
            return Err(DevelopError::InvalidCurve);
        }
        if points
            .iter()
            .any(|(x, y)| !x.is_finite() || !y.is_finite() || !(0.0..=255.0).contains(x) || !(0.0..=255.0).contains(y))
        {
            return Err(DevelopError::InvalidCurve);
        }
        if points[0] != (0.0, 0.0) || points[points.len() - 1] != (255.0, 255.0) {
            return Err(DevelopError::InvalidCurve);
        }
        if points.windows(2).any(|w| w[1].0 <= w[0].0 || w[1].1 < w[0].1) {
            return Err(DevelopError::InvalidCurve);
        }
        Ok(Self { points })
    }

    pub fn points(&self) -> &[(f64, f64)] {
        &self.points
    }

    /// Identity exactly when every authored point has x == y; the points stay retained.
    pub fn is_identity(&self) -> bool {
        self.points.iter().all(|(x, y)| x == y)
    }

    pub fn prepare(&self) -> Result<PreparedCurve, DevelopError> {
        let n = self.points.len();
        let xs: Vec<f64> = self.points.iter().map(|p| p.0 / 255.0).collect();
        let ys: Vec<f64> = self.points.iter().map(|p| p.1 / 255.0).collect();
        let mut d = vec![0.0f64; n - 1];
        for i in 0..n - 1 {
            d[i] = (ys[i + 1] - ys[i]) / (xs[i + 1] - xs[i]);
            if !d[i].is_finite() {
                return Err(DevelopError::InvalidCurve);
            }
        }
        let mut m = vec![0.0f64; n];
        m[0] = d[0];
        m[n - 1] = d[n - 2];
        for i in 1..n - 1 {
            m[i] = if d[i - 1] * d[i] <= 0.0 {
                0.0
            } else {
                (d[i - 1] + d[i]) / 2.0
            };
        }
        for i in 0..n - 1 {
            if d[i] == 0.0 {
                m[i] = 0.0;
                m[i + 1] = 0.0;
            } else {
                let a = m[i] / d[i];
                let b = m[i + 1] / d[i];
                let s = a * a + b * b;
                if !s.is_finite() {
                    return Err(DevelopError::InvalidCurve);
                }
                if s > 9.0 {
                    let q = 3.0 / s.sqrt();
                    m[i] = (q * a) * d[i];
                    m[i + 1] = (q * b) * d[i];
                }
            }
        }
        if m.iter().any(|v| !v.is_finite()) {
            return Err(DevelopError::InvalidCurve);
        }
        Ok(PreparedCurve { xs, ys, m })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PreparedCurve {
    xs: Vec<f64>,
    ys: Vec<f64>,
    m: Vec<f64>,
}

impl PreparedCurve {
    /// Evaluate at a normalised input in 0..=1; anything outside refuses (never clipped).
    pub fn eval(&self, v: f64) -> Result<f64, DevelopError> {
        if !v.is_finite() || !(0.0..=1.0).contains(&v) {
            return Err(DevelopError::CurveDomain);
        }
        let n = self.xs.len();
        if let Some(i) = self.xs.iter().position(|x| *x == v) {
            return Ok(self.ys[i]);
        }
        let i = (0..n - 1)
            .find(|i| self.xs[*i] < v && v < self.xs[i + 1])
            .ok_or(DevelopError::CurveDomain)?;
        let h = self.xs[i + 1] - self.xs[i];
        let t = (v - self.xs[i]) / h;
        let t2 = t * t;
        let t3 = (t * t) * t;
        let a = (2.0 * t3 - 3.0 * t2 + 1.0) * self.ys[i];
        let b = ((t3 - 2.0 * t2 + t) * h) * self.m[i];
        let c = (-2.0 * t3 + 3.0 * t2) * self.ys[i + 1];
        let d = ((t3 - t2) * h) * self.m[i + 1];
        let out = ((a + b) + c) + d;
        if out.is_finite() {
            Ok(out)
        } else {
            Err(DevelopError::InvalidCurve)
        }
    }
}
