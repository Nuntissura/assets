//! Prism-owned ICC evaluation for the wide path (SMB-PRISM-ENG-6).
//!
//! moxcms 0.9.1 only PARSES profiles here; every number of a wide conversion is computed in this file in
//! f64 from IEEE basic operations (+, -, *, /, comparisons, rounding): no libm transcendental and no fused
//! multiply-add, so a result is meant to be identical on every IEEE-754 host (this host's digests are pinned by
//! tests/wide_determinism.rs; a second-host comparison of its DIGESTS line is not done yet). Why not moxcms transforms: its pure-gamma `curv`
//! path uses a pow without a zero guard (gamma 1.8 at 0.0 gives -1.3e8), a type-0 parametric curve maps
//! negatives to 0, curve tables are looked up with a truncated 14-bit index, table inverses are 16-bit,
//! and table profiles are re-baked onto 33^3 / 17^4 grids (diagnostics/prism-eng6.md).
//!
//! Semantics (ICC.1:2022 clause 10 tag types; out-of-domain rules as LittleCMS 2 float evaluation):
//! * curves: `curv` count 0 identity, count 1 gamma (u8Fixed8), count n table with linear interpolation;
//!   `para` types 0-4. Table curves clamp their input to 0..1; parametric curves are evaluated unbounded.
//! * CLUT: inputs clamped to 0..1; 1 input linear, 2 bilinear, 3 tetrahedral, more inputs linear in the
//!   first input over the remaining ones (the lcms2 `Eval4Inputs` scheme). A PCS -> device table indexed by
//!   Lab interpolates its 3D CLUT trilinearly, as LittleCMS 2 does for every Lab-PCS output table
//!   (`_cmsReadOutputLUT` -> `ChangeInterpolationToTrilinear`; measured on the 13^3 synthetic CMYK B2A1:
//!   trilinear reproduces lcms2 to 1e-6, tetrahedral differs by 0.12 in magenta near black).
//! * `mft1`/`mft2`: matrix (3 inputs, non-identity only), input tables, CLUT, output tables. `mAB `: A
//!   curves, CLUT, M curves, matrix + offset, B curves. `mBA `: the reverse order.
//! * PCS encodings: XYZ u1Fixed15 (1.0 = 32768/65535), Lab v4 (`mAB `/`mBA `/`mft1`), Lab legacy v2
//!   (`mft2`: L 100 = 0xFF00, a/b 0 = 0x8000).
//! * Tag precedence per ICC.1:2022 8.10.2: the intent's table, else table 0, else matrix/TRC.
//!
//! The wide path carries PCS XYZ normalised by the D50 white (`n = XYZ / D50`), unclamped.
#![deny(clippy::arithmetic_side_effects)] // V4-01: hostile sizes, explicit checked/saturating/wrapping math
use crate::Intent;
use crate::wide::{DeviceSpace, WideError, WideProfileDescriptor};
use moxcms::{ColorProfile, DataColorSpace, LutStore, LutWarehouse, ToneReprCurve};

pub(crate) const D50: [f64; 3] = [0.9642, 1.0, 0.8249];

// ------------------------------------------------------------------ deterministic elementary functions
// Table-driven ln / exp. Every table is computed by const evaluation (IEEE-exact, same on every host)
// from slowly converging but exact series; the run-time polynomials are evaluated in a fixed order.
/// ln 2 split (fdlibm): the high part has 21 trailing zero bits, so `n * LN2_HI` is exact for |n| < 2^21.
const LN2_HI: f64 = f64::from_bits(0x3fe6_2e42_fee0_0000);
const LN2_LO: f64 = f64::from_bits(0x3dea_39ef_3579_3c76);
const LOG2E: f64 = std::f64::consts::LOG2_E;
const TAB: usize = 256;

/// ln(c) for c in [1, 2) by the atanh series (|s| <= 1/3, 40 terms: truncation < 1e-40).
const fn ln_series(c: f64) -> f64 {
    let s = (c - 1.0) / (c + 1.0);
    let s2 = s * s;
    let mut p = 0.0;
    let mut k: u32 = 40;
    while k > 0 {
        k = k.saturating_sub(1);
        p = p * s2 + 2.0 / k.saturating_mul(2).saturating_add(1) as f64;
    }
    s * p
}
/// e^r for |r| < 0.7 by Taylor (30 terms).
const fn exp_series(r: f64) -> f64 {
    let mut p = 1.0;
    let mut k: u32 = 30;
    while k > 0 {
        p = 1.0 + p * r / k as f64;
        k = k.saturating_sub(1);
    }
    p
}
/// (c_j, 1 / c_j, ln c_j) with c_j = 1 + (j + 1/2) / 256, the centre of each mantissa bucket.
const LN_TAB: [(f64, f64, f64); TAB] = {
    let mut t = [(0.0, 0.0, 0.0); TAB];
    let mut j = 0;
    while j < TAB {
        let c = 1.0 + (2 * j + 1) as f64 / (2 * TAB) as f64;
        t[j] = (c, 1.0 / c, ln_series(c));
        j += 1;
    }
    t
};
/// 2^(j / 256).
const EXP_TAB: [f64; TAB] = {
    let mut t = [0.0; TAB];
    let mut j = 0;
    while j < TAB {
        t[j] = exp_series(j as f64 * (LN2_HI + LN2_LO) / TAB as f64);
        j += 1;
    }
    t
};

/// Natural logarithm of a finite x > 0 (absolute error about 2e-16 plus |ln x| * 1e-16).
#[inline]
fn ln(x: f64) -> f64 {
    let mut bits = x.to_bits();
    let mut e = ((bits >> 52) & 0x7ff) as i32;
    if e == 0 {
        // Subnormal: scale by 2^54 first.
        bits = (x * 18_014_398_509_481_984.0).to_bits();
        // e in 0..=2047 (11-bit field): no overflow.
        e = (((bits >> 52) & 0x7ff) as i32).saturating_sub(54);
    }
    let m = f64::from_bits((bits & 0x000f_ffff_ffff_ffff) | 0x3ff0_0000_0000_0000);
    let (c, inv, lc) = LN_TAB[((bits >> 44) & 0xff) as usize];
    // |r| <= 1/513; m - c is exact (same binade, c has 9 significant bits).
    let r = (m - c) * inv;
    // log1p(r) to degree 6 (truncation < 4e-19), Estrin order.
    let r2 = r * r;
    let a = r - 0.5 * r2;
    let b = (1.0 / 3.0) - 0.25 * r;
    let d = 0.2 - (1.0 / 6.0) * r;
    let p = a + r2 * r * (b + r2 * d);
    let kf = f64::from(e.saturating_sub(1023));
    (kf * LN2_HI + lc) + (kf * LN2_LO + p)
}

/// 2^n as an exact scale factor.
#[inline]
fn scale2(v: f64, n: i32) -> f64 {
    // k is clamped to -1022..=1023 by every caller below, so the biased exponent is 1..=2046.
    let pow2 = |k: i32| f64::from_bits((k.clamp(-1022, 1023).saturating_add(1023) as u64) << 52);
    if n > 1023 {
        v * pow2(1023) * pow2(n.saturating_sub(1023).min(1023))
    } else if n >= -1022 {
        v * pow2(n)
    } else {
        v * pow2(-1022) * pow2(n.saturating_add(1022).max(-1022))
    }
}

#[inline]
fn exp(z: f64) -> f64 {
    if z.is_nan() {
        return z;
    }
    if z > 709.79 {
        return f64::INFINITY;
    }
    if z < -745.2 {
        return 0.0;
    }
    // z = n ln2 / 256 + r, |r| <= ln2 / 512.
    let n = (z * (TAB as f64 * LOG2E)).round();
    let r = (z - n * (LN2_HI / TAB as f64)) - n * (LN2_LO / TAB as f64);
    // e^r to degree 5 (truncation < 1e-18), Estrin order.
    let r2 = r * r;
    let p = (1.0 + r) + r2 * ((0.5 + r * (1.0 / 6.0)) + r2 * ((1.0 / 24.0) + r * (1.0 / 120.0)));
    let ni = n as i32;
    scale2(EXP_TAB[ni.rem_euclid(TAB as i32) as usize] * p, ni.div_euclid(TAB as i32))
}

/// x^y for x >= 0 (NaN for negative or NaN input). Relative error about |y ln x| * 2e-16.
#[inline]
pub(crate) fn pow(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() || x < 0.0 {
        return f64::NAN;
    }
    if y == 0.0 || x == 1.0 {
        return 1.0;
    }
    if x == 0.0 {
        return if y > 0.0 { 0.0 } else { f64::INFINITY };
    }
    if y == 1.0 {
        return x;
    }
    if x == f64::INFINITY {
        return if y > 0.0 { f64::INFINITY } else { 0.0 };
    }
    exp(y * ln(x))
}

pub(crate) fn cbrt(x: f64) -> f64 {
    if x < 0.0 { -pow(-x, 1.0 / 3.0) } else { pow(x, 1.0 / 3.0) }
}

// ------------------------------------------------------------------ PCS helpers (CIE, ICC D50)
pub(crate) fn lab_to_xyz(l: f64, a: f64, b: f64) -> [f64; 3] {
    let fy = (l + 16.0) / 116.0;
    let (fx, fz) = (fy + a / 500.0, fy - b / 200.0);
    let inv = |t: f64| {
        if t > 6.0 / 29.0 {
            t * t * t
        } else {
            3.0 * (6.0 / 29.0) * (6.0 / 29.0) * (t - 4.0 / 29.0)
        }
    };
    [D50[0] * inv(fx), D50[1] * inv(fy), D50[2] * inv(fz)]
}
pub(crate) fn xyz_to_lab(xyz: [f64; 3]) -> [f64; 3] {
    let f = |t: f64| {
        if t > 216.0 / 24389.0 {
            cbrt(t)
        } else {
            (24389.0 / 27.0 * t + 16.0) / 116.0
        }
    };
    let (fx, fy, fz) = (f(xyz[0] / D50[0]), f(xyz[1] / D50[1]), f(xyz[2] / D50[2]));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

// ------------------------------------------------------------------ curves
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Curve {
    Identity,
    /// `curv` count 1 or `para` type 0: y = x^g.
    Gamma(f64),
    /// `curv` table normalised to 0..1, at least 2 entries.
    Table(Vec<f64>),
    /// `para` types 1-4: [g, a, b, c, d, e, f] with unused entries 0.
    Param { kind: u8, p: [f64; 7] },
}

impl Curve {
    pub(crate) fn from_mox(c: &ToneReprCurve) -> Result<Self, WideError> {
        match c {
            ToneReprCurve::Lut(v) if v.is_empty() => Ok(Self::Identity),
            ToneReprCurve::Lut(v) if v.len() == 1 => {
                let g = f64::from(v[0]) / 256.0;
                Ok(if g == 1.0 { Self::Identity } else { Self::Gamma(g) })
            }
            ToneReprCurve::Lut(v) => Ok(Self::table(v.iter().map(|x| f64::from(*x) / 65535.0).collect())),
            ToneReprCurve::Parametric(params) => {
                let q: Vec<f64> = params.iter().map(|v| f64::from(*v)).collect();
                if q.iter().any(|v| !v.is_finite()) {
                    return Err(WideError::MalformedProfile);
                }
                let mut p = [0.0; 7];
                p[..q.len().min(7)].copy_from_slice(&q[..q.len().min(7)]);
                let kind = match q.len() {
                    1 => return Ok(if q[0] == 1.0 { Self::Identity } else { Self::Gamma(q[0]) }),
                    3 => 1,
                    4 => 2,
                    5 => 3,
                    7 => 4,
                    _ => return Err(WideError::MalformedProfile),
                };
                if p[1] == 0.0 {
                    return Err(WideError::MalformedProfile);
                }
                Ok(Self::Param { kind, p })
            }
        }
    }
    /// A table that is exactly the identity ramp is the identity (no clamp needed in table pipelines,
    /// where a CLUT or a 0..1 producer follows or precedes it).
    fn table(t: Vec<f64>) -> Self {
        let n = t.len().saturating_sub(1) as f64;
        if t.iter().enumerate().all(|(i, v)| (v - i as f64 / n).abs() < 1e-12) {
            Self::Identity
        } else {
            Self::Table(t)
        }
    }
    /// Device -> linear (forward curve). Table input clamps to 0..1; parametric curves are unbounded and
    /// follow the ICC formulas, with 0 (types 0-2) where the power base is not positive.
    pub(crate) fn eval(&self, x: f64) -> f64 {
        match self {
            Self::Identity => x,
            Self::Gamma(g) => {
                if x > 0.0 {
                    pow(x, *g)
                } else {
                    0.0
                }
            }
            Self::Table(t) => {
                // t.len() >= 2 (Curve::from_mox and split_tables).
                let x = if x > 0.0 { x.min(1.0) } else { 0.0 };
                let pos = x * t.len().saturating_sub(1) as f64;
                let i = (pos as usize).min(t.len().saturating_sub(2));
                let f = pos - i as f64;
                let j = i.saturating_add(1);
                t[i] + f * (t[j] - t[i])
            }
            Self::Param { kind, p } => {
                let [g, a, b, c, d, e, f] = *p;
                let power = |x: f64| {
                    let base = a * x + b;
                    if base > 0.0 { pow(base, g) } else { 0.0 }
                };
                match kind {
                    1 => {
                        if x >= -b / a { power(x) } else { 0.0 }
                    }
                    2 => {
                        if x >= -b / a { power(x) + c } else { c }
                    }
                    3 => {
                        if x >= d { power(x) } else { c * x }
                    }
                    _ => {
                        if x >= d { power(x) + e } else { c * x + f }
                    }
                }
            }
        }
    }

    /// Linear -> device (inverse curve) for matrix/TRC and Gray destinations, extended outside 0..1:
    /// power branches mirror through the origin of the power function, linear toes extend linearly, a
    /// table extrapolates its end segments. Out-of-gamut values therefore keep their sign.
    pub(crate) fn inverse(&self, y: f64) -> f64 {
        let root = |v: f64, g: f64| if v >= 0.0 { pow(v, 1.0 / g) } else { -pow(-v, 1.0 / g) };
        match self {
            Self::Identity => y,
            Self::Gamma(g) => root(y, *g),
            Self::Table(t) => table_inverse(t, y),
            Self::Param { kind, p } => {
                let [g, a, b, c, d, e, f] = *p;
                match kind {
                    1 => (root(y, g) - b) / a,
                    2 => (root(y - c, g) - b) / a,
                    3 => {
                        let base = a * d + b;
                        let yd = if base > 0.0 { pow(base, g) } else { 0.0 };
                        if y >= yd {
                            (root(y, g) - b) / a
                        } else if c != 0.0 {
                            y / c
                        } else {
                            d
                        }
                    }
                    _ => {
                        let base = a * d + b;
                        let yd = if base > 0.0 { pow(base, g) } else { 0.0 } + e;
                        if y >= yd {
                            (root(y - e, g) - b) / a
                        } else if c != 0.0 {
                            (y - f) / c
                        } else {
                            d
                        }
                    }
                }
            }
        }
    }
}

/// Exact inverse of a monotonic piecewise-linear table (entries at i / (n - 1)); `t.len() >= 2`
/// (`Curve::from_mox` and `split_tables` refuse shorter tables).
fn table_inverse(t: &[f64], y: f64) -> f64 {
    let n = t.len();
    let last = n.saturating_sub(1);
    let step = 1.0 / last as f64;
    let ascending = t[last] >= t[0];
    let at = |i: usize| if ascending { t[i] } else { -t[i] };
    let v = if ascending { y } else { -y };
    let seg = |i: usize| {
        let (lo, hi) = (at(i), at(i.saturating_add(1).min(last)));
        let base = i as f64 * step;
        if hi > lo { base + (v - lo) / (hi - lo) * step } else { base }
    };
    if v <= at(0) {
        return seg(0).min(0.0);
    }
    if v >= at(last) {
        return seg(n.saturating_sub(2)).max(1.0);
    }
    // Largest index with at(i) <= v.
    let (mut lo, mut hi) = (0usize, last);
    while hi.saturating_sub(lo) > 1 {
        let mid = lo.saturating_add(hi.saturating_sub(lo) / 2);
        if at(mid) <= v {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    seg(lo)
}

// ------------------------------------------------------------------ CLUT
const MAX_CH: usize = 16;

#[derive(Clone, Debug)]
pub(crate) struct Clut {
    inputs: usize,
    outputs: usize,
    grid: Vec<usize>,
    strides: Vec<usize>,
    data: Vec<f64>,
    trilinear: bool,
}

fn store_values(store: &LutStore) -> Vec<f64> {
    match store {
        LutStore::Store8(v) => v.iter().map(|x| f64::from(*x) / 255.0).collect(),
        LutStore::Store16(v) => v.iter().map(|x| f64::from(*x) / 65535.0).collect(),
    }
}

/// Clamp to 0..1; NaN reads as 0 (as lcms2 `fclamp`).
fn unit(v: f64) -> f64 {
    if v > 0.0 { v.min(1.0) } else { 0.0 }
}
/// Index arithmetic of a validated CLUT. `Clut::new` checks with checked arithmetic that every
/// `grid * stride` product fits and that the largest reachable node index is below `data.len()`, so these
/// wrapping operations cannot wrap (oversized grids are refused: tests/wide_hostile.rs).
#[inline]
fn ix(a: usize, b: usize) -> usize {
    a.wrapping_add(b)
}
#[inline]
fn ixm(a: usize, b: usize) -> usize {
    a.wrapping_mul(b)
}
/// Cell index and fraction of `v` on a grid of `n >= 2` points.
fn locate(v: f64, n: usize) -> (usize, f64) {
    let pos = unit(v) * n.wrapping_sub(1) as f64;
    let i = (pos as usize).min(n.wrapping_sub(2));
    (i, pos - i as f64)
}

#[allow(clippy::needless_range_loop)] // k indexes the output and the CLUT node offset together
impl Clut {
    fn new(store: &LutStore, grid: &[usize], outputs: usize) -> Result<Self, WideError> {
        let inputs = grid.len();
        if inputs == 0 || inputs > 8 || outputs == 0 || outputs > MAX_CH || grid.iter().any(|g| *g < 2) {
            return Err(WideError::UnsupportedProfile);
        }
        // strides[i] = outputs * product(grid[i + 1..]), all checked.
        let mut strides = vec![outputs; inputs];
        for i in (1..inputs).rev() {
            strides[i.saturating_sub(1)] = strides[i].checked_mul(grid[i]).ok_or(WideError::MalformedProfile)?;
        }
        let total = strides[0].checked_mul(grid[0]).ok_or(WideError::MalformedProfile)?;
        // Largest reachable index: sum((grid - 1) * stride) + outputs - 1 (= total - 1), checked.
        let mut last = outputs.checked_sub(1).ok_or(WideError::MalformedProfile)?;
        for (g, st) in grid.iter().zip(&strides) {
            let step = g.checked_sub(1).and_then(|v| v.checked_mul(*st)).ok_or(WideError::MalformedProfile)?;
            last = last.checked_add(step).ok_or(WideError::MalformedProfile)?;
        }
        let data = store_values(store);
        if data.len() != total || last >= total {
            return Err(WideError::MalformedProfile);
        }
        Ok(Self {
            inputs,
            outputs,
            grid: grid.to_vec(),
            strides,
            data,
            trilinear: false,
        })
    }

    fn eval(&self, input: &[f64], out: &mut [f64]) {
        self.eval_from(0, 0, input, out);
    }

    fn eval_from(&self, dim: usize, base: usize, input: &[f64], out: &mut [f64]) {
        let o = self.outputs;
        let next = ix(dim, 1);
        match self.inputs.wrapping_sub(dim) {
            1 => {
                let (i, f) = locate(input[dim], self.grid[dim]);
                let p0 = ix(base, ixm(i, self.strides[dim]));
                let p1 = ix(p0, self.strides[dim]);
                for k in 0..o {
                    let (a, b) = (self.data[ix(p0, k)], self.data[ix(p1, k)]);
                    out[k] = a + f * (b - a);
                }
            }
            2 => {
                let (i, fx) = locate(input[dim], self.grid[dim]);
                let (j, fy) = locate(input[next], self.grid[next]);
                let (sx, sy) = (self.strides[dim], self.strides[next]);
                let p = ix(base, ix(ixm(i, sx), ixm(j, sy)));
                for k in 0..o {
                    let d = |off: usize| self.data[ix(ix(p, off), k)];
                    let a = d(0) + fy * (d(sy) - d(0));
                    let b = d(sx) + fy * (d(ix(sx, sy)) - d(sx));
                    out[k] = a + fx * (b - a);
                }
            }
            3 if self.trilinear => self.trilinear3(dim, base, input, out),
            3 => self.tetrahedral(dim, base, input, out),
            _ => {
                let (i, f) = locate(input[dim], self.grid[dim]);
                let (mut a, mut b) = ([0.0; MAX_CH], [0.0; MAX_CH]);
                let p0 = ix(base, ixm(i, self.strides[dim]));
                self.eval_from(next, p0, input, &mut a);
                self.eval_from(next, ix(p0, self.strides[dim]), input, &mut b);
                for k in 0..o {
                    out[k] = a[k] + f * (b[k] - a[k]);
                }
            }
        }
    }

    /// Cell origin, strides and fractions of the last three inputs starting at `dim`.
    fn cell3(&self, dim: usize, base: usize, input: &[f64]) -> (usize, [usize; 3], [f64; 3]) {
        let (d1, d2) = (ix(dim, 1), ix(dim, 2));
        let (x0, fx) = locate(input[dim], self.grid[dim]);
        let (y0, fy) = locate(input[d1], self.grid[d1]);
        let (z0, fz) = locate(input[d2], self.grid[d2]);
        let s = [self.strides[dim], self.strides[d1], self.strides[d2]];
        let p0 = ix(base, ix(ixm(x0, s[0]), ix(ixm(y0, s[1]), ixm(z0, s[2]))));
        (p0, s, [fx, fy, fz])
    }

    /// Trilinear interpolation over the last three inputs.
    fn trilinear3(&self, dim: usize, base: usize, input: &[f64], out: &mut [f64]) {
        let (p0, [sx, sy, sz], [fx, fy, fz]) = self.cell3(dim, base, input);
        let lerp = |a: f64, b: f64, t: f64| a + t * (b - a);
        for k in 0..self.outputs {
            let d = |off: usize| self.data[ix(ix(p0, off), k)];
            let x00 = lerp(d(0), d(sx), fx);
            let x10 = lerp(d(sy), d(ix(sx, sy)), fx);
            let x01 = lerp(d(sz), d(ix(sx, sz)), fx);
            let x11 = lerp(d(ix(sy, sz)), d(ix(ix(sx, sy), sz)), fx);
            out[k] = lerp(lerp(x00, x10, fy), lerp(x01, x11, fy), fz);
        }
    }

    /// Tetrahedral interpolation over the last three inputs (same tetrahedra as lcms2).
    fn tetrahedral(&self, dim: usize, base: usize, input: &[f64], out: &mut [f64]) {
        let (p0, [sx, sy, sz], [rx, ry, rz]) = self.cell3(dim, base, input);
        let (xy, xz, yz) = (ix(sx, sy), ix(sx, sz), ix(sy, sz));
        let xyz = ix(xy, sz);
        // (c1 = a - b, c2 = c - d, c3 = e - f) as offsets from p0.
        let t: [usize; 6] = if rx >= ry && ry >= rz {
            [sx, 0, xy, sx, xyz, xy]
        } else if rx >= rz && rz >= ry {
            [sx, 0, xyz, xz, xz, sx]
        } else if rz >= rx && rx >= ry {
            [xz, sz, xyz, xz, sz, 0]
        } else if ry >= rx && rx >= rz {
            [xy, sy, sy, 0, xyz, xy]
        } else if ry >= rz && rz >= rx {
            [xyz, yz, sy, 0, yz, sy]
        } else if rz >= ry && ry >= rx {
            [xyz, yz, yz, sz, sz, 0]
        } else {
            [0; 6]
        };
        for k in 0..self.outputs {
            let d = |off: usize| self.data[ix(ix(p0, off), k)];
            let c0 = d(0);
            out[k] = c0 + (d(t[0]) - d(t[1])) * rx + (d(t[2]) - d(t[3])) * ry + (d(t[4]) - d(t[5])) * rz;
        }
    }
}

// ------------------------------------------------------------------ table pipelines
type Mat3 = [[f64; 3]; 3];

fn is_identity(m: &Mat3) -> bool {
    (0..3).all(|r| (0..3).all(|c| m[r][c] == if r == c { 1.0 } else { 0.0 }))
}
fn mul(m: &Mat3, v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PcsEncoding {
    Xyz,
    /// ICC v4 Lab (also every 8-bit Lab): L = 100 v, a = 255 v - 128.
    Lab,
    /// ICC v2 legacy 16-bit Lab of `mft2`: L 100 = 0xFF00, a/b 0 = 0x8000.
    LabLegacy,
}
impl PcsEncoding {
    /// Encoded table value -> normalised PCS XYZ / D50.
    fn decode(self, v: [f64; 3]) -> [f64; 3] {
        let xyz = match self {
            Self::Xyz => [v[0] * (65535.0 / 32768.0), v[1] * (65535.0 / 32768.0), v[2] * (65535.0 / 32768.0)],
            Self::Lab => lab_to_xyz(100.0 * v[0], 255.0 * v[1] - 128.0, 255.0 * v[2] - 128.0),
            Self::LabLegacy => {
                let k = 65535.0 / 65280.0;
                lab_to_xyz(100.0 * v[0] * k, 255.0 * v[1] * k - 128.0, 255.0 * v[2] * k - 128.0)
            }
        };
        [xyz[0] / D50[0], xyz[1] / D50[1], xyz[2] / D50[2]]
    }
    fn encode(self, n: [f64; 3]) -> [f64; 3] {
        let xyz = [n[0] * D50[0], n[1] * D50[1], n[2] * D50[2]];
        match self {
            Self::Xyz => [xyz[0] * (32768.0 / 65535.0), xyz[1] * (32768.0 / 65535.0), xyz[2] * (32768.0 / 65535.0)],
            Self::Lab => {
                let l = xyz_to_lab(xyz);
                [l[0] / 100.0, (l[1] + 128.0) / 255.0, (l[2] + 128.0) / 255.0]
            }
            Self::LabLegacy => {
                let l = xyz_to_lab(xyz);
                let k = 65280.0 / 65535.0;
                [l[0] / 100.0 * k, (l[1] + 128.0) / 255.0 * k, (l[2] + 128.0) / 255.0 * k]
            }
        }
    }
}

#[derive(Clone, Debug)]
enum Pipe {
    /// `mft1`/`mft2`.
    Legacy { matrix: Option<Mat3>, input: Vec<Curve>, clut: Clut, output: Vec<Curve> },
    /// `mAB ` (to_pcs = true) or `mBA `.
    Multi { to_pcs: bool, a: Vec<Curve>, clut: Option<Clut>, m: Vec<Curve>, matrix: Option<(Mat3, [f64; 3])>, b: Vec<Curve> },
}

fn curves(list: &[ToneReprCurve]) -> Result<Vec<Curve>, WideError> {
    list.iter().map(Curve::from_mox).collect()
}
fn split_tables(store: &LutStore, channels: usize, entries: usize) -> Result<Vec<Curve>, WideError> {
    let values = store_values(store);
    if entries < 2 || Some(values.len()) != channels.checked_mul(entries) {
        return Err(WideError::MalformedProfile);
    }
    Ok(values.chunks_exact(entries).map(|c| Curve::table(c.to_vec())).collect())
}
fn apply(curves: &[Curve], v: &mut [f64]) {
    for (c, x) in curves.iter().zip(v.iter_mut()) {
        *x = c.eval(*x);
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Table {
    pipe: Pipe,
    pub(crate) inputs: usize,
    pub(crate) outputs: usize,
}

impl Table {
    /// `lab_indexed`: a PCS -> device table of a Lab-PCS profile (3D CLUT interpolated trilinearly).
    fn new(w: &LutWarehouse, to_pcs: bool, lab_indexed: bool) -> Result<Self, WideError> {
        let mut t = Self::build(w, to_pcs)?;
        if lab_indexed {
            match &mut t.pipe {
                Pipe::Legacy { clut, .. } => clut.trilinear = true,
                Pipe::Multi { clut: Some(clut), .. } => clut.trilinear = true,
                Pipe::Multi { clut: None, .. } => {}
            }
        }
        Ok(t)
    }
    fn build(w: &LutWarehouse, to_pcs: bool) -> Result<Self, WideError> {
        match w {
            LutWarehouse::Lut(l) => {
                let (inputs, outputs) = (usize::from(l.num_input_channels), usize::from(l.num_output_channels));
                let grid = vec![usize::from(l.num_clut_grid_points); inputs];
                let m = l.matrix.v;
                Ok(Self {
                    pipe: Pipe::Legacy {
                        matrix: (inputs == 3 && !is_identity(&m)).then_some(m),
                        input: split_tables(&l.input_table, inputs, usize::from(l.num_input_table_entries))?,
                        clut: Clut::new(&l.clut_table, &grid, outputs)?,
                        output: split_tables(&l.output_table, outputs, usize::from(l.num_output_table_entries))?,
                    },
                    inputs,
                    outputs,
                })
            }
            LutWarehouse::Multidimensional(t) => {
                let (inputs, outputs) = (usize::from(t.num_input_channels), usize::from(t.num_output_channels));
                let clut = match &t.clut {
                    Some(store) => {
                        let grid: Vec<usize> = t.grid_points[..inputs].iter().map(|g| usize::from(*g)).collect();
                        Some(Clut::new(store, &grid, outputs)?)
                    }
                    None if inputs == outputs => None,
                    None => return Err(WideError::MalformedProfile),
                };
                let (a, m, b) = (curves(&t.a_curves)?, curves(&t.m_curves)?, curves(&t.b_curves)?);
                // Channel counts on each side of the CLUT (A curves sit on the device side).
                let (device, pcs) = if to_pcs { (inputs, outputs) } else { (outputs, inputs) };
                if (!a.is_empty() && a.len() != device) || (!m.is_empty() && m.len() != pcs) || (!b.is_empty() && b.len() != pcs) {
                    return Err(WideError::MalformedProfile);
                }
                let matrix = (!is_identity(&t.matrix.v) || t.bias.v != [0.0; 3]).then_some((t.matrix.v, t.bias.v));
                if matrix.is_some() && pcs != 3 {
                    return Err(WideError::MalformedProfile);
                }
                Ok(Self {
                    pipe: Pipe::Multi { to_pcs, a, clut, m, matrix, b },
                    inputs,
                    outputs,
                })
            }
        }
    }

    /// Evaluate one pixel: `v[..inputs]` in, `v[..outputs]` out (encoded table domain).
    fn eval(&self, v: &mut [f64; MAX_CH]) {
        let mut tmp = [0.0; MAX_CH];
        match &self.pipe {
            Pipe::Legacy { matrix, input, clut, output } => {
                if let Some(m) = matrix {
                    let r = mul(m, [v[0], v[1], v[2]]);
                    v[..3].copy_from_slice(&r);
                }
                apply(input, &mut v[..self.inputs]);
                clut.eval(&v[..self.inputs], &mut tmp);
                v[..self.outputs].copy_from_slice(&tmp[..self.outputs]);
                apply(output, &mut v[..self.outputs]);
            }
            Pipe::Multi { to_pcs, a, clut, m, matrix, b } => {
                let clut_stage = |v: &mut [f64; MAX_CH], tmp: &mut [f64; MAX_CH]| {
                    if let Some(c) = clut {
                        c.eval(&v[..self.inputs], tmp);
                        v[..self.outputs].copy_from_slice(&tmp[..self.outputs]);
                    }
                };
                let matrix_stage = |v: &mut [f64; MAX_CH]| {
                    if let Some((mat, bias)) = matrix {
                        let r = mul(mat, [v[0], v[1], v[2]]);
                        for i in 0..3 {
                            v[i] = r[i] + bias[i];
                        }
                    }
                };
                if *to_pcs {
                    apply(a, &mut v[..self.inputs]);
                    clut_stage(v, &mut tmp);
                    apply(m, &mut v[..self.outputs]);
                    matrix_stage(v);
                    apply(b, &mut v[..self.outputs]);
                } else {
                    apply(b, &mut v[..self.inputs]);
                    matrix_stage(v);
                    apply(m, &mut v[..self.inputs]);
                    clut_stage(v, &mut tmp);
                    apply(a, &mut v[..self.outputs]);
                }
            }
        }
    }
}

// ------------------------------------------------------------------ stages
/// Device -> normalised PCS.
#[derive(Clone, Debug)]
pub(crate) enum ToPcs {
    /// `n = m * trc(device)`; `m` already divides rows by D50.
    Matrix { trc: [Curve; 3], m: Mat3 },
    /// `n = (Y, Y, Y)` with `Y = trc(gray)`.
    Gray(Curve),
    Table { table: Table, enc: PcsEncoding },
}
/// Normalised PCS -> device.
#[derive(Clone, Debug)]
pub(crate) enum FromPcs {
    /// `linear = inv * n` (inverse colourants with D50 folded into the columns), then inverse TRC.
    Matrix { inv: Mat3, trc: [Curve; 3] },
    /// `linear = Y = n[1]`, then inverse kTRC.
    Gray(Curve),
    Table { table: Table, enc: PcsEncoding },
}

impl ToPcs {
    pub(crate) fn eval(&self, device: &[f32]) -> [f64; 3] {
        match self {
            Self::Matrix { trc, m } => mul(
                m,
                [trc[0].eval(f64::from(device[0])), trc[1].eval(f64::from(device[1])), trc[2].eval(f64::from(device[2]))],
            ),
            Self::Gray(c) => {
                let y = c.eval(f64::from(device[0]));
                [y, y, y]
            }
            Self::Table { table, enc } => {
                let mut v = [0.0; MAX_CH];
                for (o, d) in v.iter_mut().zip(device) {
                    *o = f64::from(*d);
                }
                table.eval(&mut v);
                enc.decode([v[0], v[1], v[2]])
            }
        }
    }
}

impl FromPcs {
    /// Linear device values of a matrix/Gray destination (`None` for tables): what clip detection scans.
    pub(crate) fn linear(&self, n: [f64; 3]) -> Option<[f64; 3]> {
        match self {
            Self::Matrix { inv, .. } => Some(mul(inv, n)),
            Self::Gray(_) => Some([n[1], 0.0, 0.0]),
            Self::Table { .. } => None,
        }
    }
    /// Encode linear values (from `linear`) to device values.
    pub(crate) fn encode_linear(&self, lin: [f64; 3], out: &mut [f32]) {
        match self {
            Self::Matrix { trc, .. } => {
                for c in 0..3 {
                    out[c] = trc[c].inverse(lin[c]) as f32;
                }
            }
            Self::Gray(c) => out[0] = c.inverse(lin[0]) as f32,
            Self::Table { .. } => unreachable!("table destinations have no linear stage"),
        }
    }
    pub(crate) fn eval(&self, n: [f64; 3], out: &mut [f32]) {
        match self {
            Self::Table { table, enc } => {
                let mut v = [0.0; MAX_CH];
                v[..3].copy_from_slice(&enc.encode(n));
                table.eval(&mut v);
                for (o, x) in out.iter_mut().zip(&v[..table.outputs]) {
                    *o = *x as f32;
                }
            }
            _ => {
                let lin = self.linear(n).expect("matrix or gray destination");
                self.encode_linear(lin, out);
            }
        }
    }
}

// ------------------------------------------------------------------ stage construction (ICC 8.10.2)
fn table_index(intent: Intent) -> usize {
    match intent {
        Intent::Perceptual => 0,
        Intent::RelativeColorimetric | Intent::AbsoluteColorimetric => 1,
        Intent::Saturation => 2,
    }
}
fn pick(tables: [&Option<LutWarehouse>; 3], intent: Intent) -> Option<&LutWarehouse> {
    tables[table_index(intent)].as_ref().or(tables[0].as_ref())
}
fn encoding(w: &LutWarehouse, pcs: DataColorSpace) -> PcsEncoding {
    match (pcs, w) {
        (DataColorSpace::Xyz, _) => PcsEncoding::Xyz,
        (_, LutWarehouse::Lut(l)) if l.lut_type == moxcms::LutType::Lut16 => PcsEncoding::LabLegacy,
        _ => PcsEncoding::Lab,
    }
}
fn trc3(p: &ColorProfile) -> Option<[Curve; 3]> {
    let get = |c: &Option<ToneReprCurve>| c.as_ref().map(Curve::from_mox);
    match (get(&p.red_trc), get(&p.green_trc), get(&p.blue_trc)) {
        (Some(Ok(r)), Some(Ok(g)), Some(Ok(b))) => Some([r, g, b]),
        _ => None,
    }
}
fn matrix_capable(p: &ColorProfile, d: &WideProfileDescriptor) -> bool {
    p.pcs == DataColorSpace::Xyz
        && match d.device {
            DeviceSpace::Rgb => trc3(p).is_some(),
            DeviceSpace::Gray => p.gray_trc.is_some(),
            DeviceSpace::Cmyk => false,
        }
}
fn colorants_over_d50(p: &ColorProfile) -> Mat3 {
    let m = p.colorant_matrix().v;
    let mut out = [[0.0; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            out[r][c] = m[r][c] / D50[r];
        }
    }
    out
}
pub(crate) fn inverse3(m: Mat3) -> Option<Mat3> {
    let c = |r: usize, k: usize| {
        const NEXT: [usize; 3] = [1, 2, 0];
        let (r1, r2, k1, k2) = (NEXT[r], NEXT[NEXT[r]], NEXT[k], NEXT[NEXT[k]]);
        m[r1][k1] * m[r2][k2] - m[r1][k2] * m[r2][k1]
    };
    let det = m[0][0] * c(0, 0) + m[0][1] * c(0, 1) + m[0][2] * c(0, 2);
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let mut out = [[0.0; 3]; 3];
    for (r, row) in out.iter_mut().enumerate() {
        for (k, v) in row.iter_mut().enumerate() {
            *v = c(k, r) / det;
        }
    }
    Some(out)
}

/// Device -> PCS stage of `p` for `intent` (ICC.1:2022 8.10.2 precedence).
pub(crate) fn to_pcs(p: &ColorProfile, d: &WideProfileDescriptor, intent: Intent) -> Result<ToPcs, WideError> {
    if let Some(w) = pick([&p.lut_a_to_b_perceptual, &p.lut_a_to_b_colorimetric, &p.lut_a_to_b_saturation], intent) {
        let table = Table::new(w, true, false)?;
        if table.inputs != d.device.channels() || table.outputs != 3 {
            return Err(WideError::MalformedProfile);
        }
        return Ok(ToPcs::Table { table, enc: encoding(w, p.pcs) });
    }
    if !matrix_capable(p, d) {
        return Err(WideError::MissingIntentTable);
    }
    Ok(match d.device {
        DeviceSpace::Gray => ToPcs::Gray(Curve::from_mox(p.gray_trc.as_ref().expect("checked"))?),
        _ => ToPcs::Matrix { trc: trc3(p).expect("checked"), m: colorants_over_d50(p) },
    })
}

/// PCS -> device stage of `p` for `intent` (ICC.1:2022 8.10.2 precedence).
pub(crate) fn from_pcs(p: &ColorProfile, d: &WideProfileDescriptor, intent: Intent) -> Result<FromPcs, WideError> {
    if let Some(w) = pick([&p.lut_b_to_a_perceptual, &p.lut_b_to_a_colorimetric, &p.lut_b_to_a_saturation], intent) {
        let table = Table::new(w, false, p.pcs == DataColorSpace::Lab)?;
        if table.inputs != 3 || table.outputs != d.device.channels() {
            return Err(WideError::MalformedProfile);
        }
        return Ok(FromPcs::Table { table, enc: encoding(w, p.pcs) });
    }
    if !matrix_capable(p, d) {
        return Err(WideError::MissingIntentTable);
    }
    Ok(match d.device {
        DeviceSpace::Gray => FromPcs::Gray(Curve::from_mox(p.gray_trc.as_ref().expect("checked"))?),
        _ => {
            let mut inv = inverse3(p.colorant_matrix().v).ok_or(WideError::SingularMatrix)?;
            for row in &mut inv {
                for (v, w) in row.iter_mut().zip(D50) {
                    *v *= w;
                }
            }
            FromPcs::Matrix { inv, trc: trc3(p).expect("checked") }
        }
    })
}

// ------------------------------------------------------------------ fused matrix/Gray pairs
/// Matrix/TRC or Gray source and destination: one affine 3x3 between the curves,
/// `linear_dst = a * linear_src + b`, with `a = D * diag(k) * S` and `b = D * o` for the PCS map `n' = k n + o`.
#[derive(Clone, Debug)]
pub(crate) struct Fused {
    pub(crate) src: Vec<Curve>,
    pub(crate) a: Mat3,
    pub(crate) b: [f64; 3],
    pub(crate) dst: Vec<Curve>,
}
impl Fused {
    pub(crate) fn build(source: &ToPcs, scale: [f64; 3], offset: [f64; 3], dest: &FromPcs) -> Option<Self> {
        let (src, s): (Vec<Curve>, Mat3) = match source {
            ToPcs::Matrix { trc, m } => (trc.to_vec(), *m),
            // Gray: linear source vector (Y, Y, Y), n = identity * (Y, Y, Y).
            ToPcs::Gray(c) => (vec![c.clone()], [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]),
            ToPcs::Table { .. } => return None,
        };
        let (dst, dm): (Vec<Curve>, Mat3) = match dest {
            FromPcs::Matrix { inv, trc } => (trc.to_vec(), *inv),
            FromPcs::Gray(c) => (vec![c.clone()], [[0.0, 1.0, 0.0], [0.0; 3], [0.0; 3]]),
            FromPcs::Table { .. } => return None,
        };
        let mut a = [[0.0; 3]; 3];
        for r in 0..3 {
            for c in 0..3 {
                a[r][c] = (0..3).map(|j| dm[r][j] * scale[j] * s[j][c]).fold(0.0, |acc, v| acc + v);
            }
        }
        // A Gray source feeds (Y, Y, Y): fold the three columns into the first.
        if src.len() == 1 {
            for row in &mut a {
                row[0] = row[0] + row[1] + row[2];
                row[1] = 0.0;
                row[2] = 0.0;
            }
        }
        let b = mul(&dm, offset);
        Some(Self { src, a, b, dst })
    }
    /// Linear destination values for one device pixel.
    pub(crate) fn linear(&self, px: &[f32]) -> [f64; 3] {
        let l = if self.src.len() == 1 {
            [self.src[0].eval(f64::from(px[0])), 0.0, 0.0]
        } else {
            [self.src[0].eval(f64::from(px[0])), self.src[1].eval(f64::from(px[1])), self.src[2].eval(f64::from(px[2]))]
        };
        let r = mul(&self.a, l);
        [r[0] + self.b[0], r[1] + self.b[1], r[2] + self.b[2]]
    }
    pub(crate) fn encode(&self, lin: [f64; 3], out: &mut [f32]) {
        for (c, curve) in self.dst.iter().enumerate() {
            out[c] = curve.inverse(lin[c]) as f32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_pow_matches_closed_forms() {
        // Relative error bound ~ |y ln x| * 2^-52.
        for (x, y, want) in [(2.0, 10.0, 1024.0), (0.25, 0.5, 0.5), (8.0, 1.0 / 3.0, 2.0), (1e-300, 0.5, 1e-150), (10.0, -2.0, 0.01)] {
            let got = pow(x, y);
            assert!(((got - want) / want).abs() < 1e-12, "{x}^{y} = {got}, want {want}");
        }
        assert_eq!(pow(0.0, 1.8), 0.0);
        assert_eq!(pow(1.0, 2.4), 1.0);
        let e = exp(1.0);
        assert!((e - std::f64::consts::E).abs() < 1e-15, "{e}");
        assert!((ln(std::f64::consts::E) - 1.0).abs() < 1e-15);
    }
}
