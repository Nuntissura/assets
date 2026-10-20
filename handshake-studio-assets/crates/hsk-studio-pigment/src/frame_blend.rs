//! Frame blending pixels (lane SMB-AE-ROTO-TIMEREMAP slice 1; parity AE-10): the two
//! [STU-MOT-007a] methods that synthesise a frame between two source frames under a non-integer
//! time remap or stretch ([STU-MOT-045] contract 6). Motion decides WHICH frames and the weight
//! (`hsk_studio_motion::frame_sample`); this module owns the pixels (CX-CAP-001).
//!
//! - [`frame_mix`]: weighted cross-fade of the two frames in premultiplied binary64,
//!   `P = (1 - w) A_premul + w B_premul`, colour un-premultiplied once and rounded once to binary32.
//! - [`pixel_motion`]: optical-flow interpolation. Flow is estimated both ways by [`optical_flow`]
//!   (clean-room from the papers, no code read): Horn and Schunck, "Determining Optical Flow",
//!   Artificial Intelligence 17 (1981) 185-203 (brightness constancy plus quadratic smoothness,
//!   Jacobi update `U = Ubar - Ix (Ix (Ubar - u0) + Iy (Vbar - v0) + It) / (alpha^2 + Ix^2 + Iy^2)`
//!   with the paper's 3x3 average, 1/6 edge and 1/12 corner neighbours), run coarse to fine on a
//!   2x2-box pyramid with repeated warping of the second frame (Papenberg, Bruhn, Brox, Didas and
//!   Weickert, "Highly Accurate Optic Flow Computation with Theoretically Justified Warping", IJCV
//!   67 (2006)). The intermediate frame at weight `t` samples both frames along the intermediate
//!   flow approximation of Jiang et al., "Super SloMo", CVPR 2018, eq. 4 (the flow formula only,
//!   no learned model): `F_t0 = -(1 - t) t F_01 + t^2 F_10`, `F_t1 = (1 - t)^2 F_01 - t (1 - t)
//!   F_10`, `I_t = (1 - t) A(p + F_t0) + t B(p + F_t1)` premultiplied, bilinear with clamped edges.
//!   No occlusion reasoning (Studio reading).
//!
//! After Effects' Frame Mix / Pixel Motion formulas are UNVERIFIED (D5): class Approximate.
//! Determinism: fixed loop order, IEEE binary64 `+ - * /` only, so two runs give the same bits
//! (test `frame_mix_hand_values_and_hostile_inputs`).
//! Hostile inputs (V4-01/02): shapes, weights and parameters are checked (never clamped); bytes
//! and work of the whole operation are computed with checked arithmetic and refused against
//! [`BlendLimits`] before allocation.
#![deny(clippy::arithmetic_side_effects)]
use crate::Rect;
use crate::blend_math::Exactness;
use crate::retouch::RgbaPlane;
use hsk_studio_accord::CancellationToken;
use std::fmt;

/// Class of both blend methods (module docs).
pub const FRAME_BLEND_EXACTNESS: Exactness = Exactness::Approximate;
/// Largest pyramid depth.
pub const MAX_FLOW_LEVELS: u32 = 8;
/// Largest warp count per level.
pub const MAX_FLOW_WARPS: u32 = 16;
/// Largest Jacobi iteration count per warp.
pub const MAX_FLOW_ITERATIONS: u32 = 1000;

/// Byte and work budget (work = sample visits).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlendLimits {
    pub max_bytes: u64,
    pub max_work: u64,
}
impl BlendLimits {
    /// 1 GiB, 2e10 sample visits.
    pub const DEFAULT: Self = Self {
        max_bytes: 1 << 30,
        max_work: 20_000_000_000,
    };
}

/// Typed failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlendError {
    /// Frame rectangles differ, a buffer length disagrees with its rectangle, or a frame is empty.
    Shape,
    /// A weight or flow parameter is non-finite or outside its domain.
    Parameter,
    /// A sample is non-finite or outside `[0, 1]`.
    OutOfDomain,
    /// Bytes or work above the limits (refused before allocation).
    OverBudget { needed: u64, limit: u64 },
    Overflow,
    Canceled,
}
impl BlendError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Shape => "frame_blend_shape",
            Self::Parameter => "frame_blend_parameter",
            Self::OutOfDomain => "frame_blend_out_of_domain",
            Self::OverBudget { .. } => "frame_blend_over_budget",
            Self::Overflow => "frame_blend_overflow",
            Self::Canceled => "canceled",
        }
    }
}
impl fmt::Display for BlendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for BlendError {}

/// Horn-Schunck pyramid parameters (module docs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlowParams {
    /// Pyramid levels `1..=MAX_FLOW_LEVELS` (fewer are used when a level would be under 8 px).
    pub levels: u32,
    /// Warps per level `1..=MAX_FLOW_WARPS`.
    pub warps: u32,
    /// Jacobi iterations per warp `1..=MAX_FLOW_ITERATIONS`.
    pub iterations: u32,
    /// Smoothness weight `alpha > 0` on intensities in `[0, 1]`.
    pub alpha: f64,
}
impl FlowParams {
    pub const DEFAULT: Self = Self {
        levels: 4,
        warps: 3,
        iterations: 80,
        alpha: 0.05,
    };
    fn validate(&self) -> Result<(), BlendError> {
        if !(1..=MAX_FLOW_LEVELS).contains(&self.levels)
            || !(1..=MAX_FLOW_WARPS).contains(&self.warps)
            || !(1..=MAX_FLOW_ITERATIONS).contains(&self.iterations)
            || !self.alpha.is_finite()
            || self.alpha <= 0.0
        {
            return Err(BlendError::Parameter);
        }
        Ok(())
    }
}
impl Default for FlowParams {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A dense flow field: pixel `p` of the first frame corresponds to `p + (u, v)` of the second.
#[derive(Clone, Debug, PartialEq)]
pub struct FlowField {
    pub width: usize,
    pub height: usize,
    pub u: Vec<f64>,
    pub v: Vec<f64>,
}
impl FlowField {
    /// Flow at pixel `(x, y)` (zero outside).
    pub fn at(&self, x: usize, y: usize) -> [f64; 2] {
        let i = y.checked_mul(self.width).and_then(|r| r.checked_add(x));
        match i {
            Some(i) if x < self.width && y < self.height => [
                self.u.get(i).copied().unwrap_or(0.0),
                self.v.get(i).copied().unwrap_or(0.0),
            ],
            _ => [0.0, 0.0],
        }
    }
}

/// A single-channel working image.
#[derive(Clone, Debug)]
pub(crate) struct Gray {
    pub(crate) w: usize,
    pub(crate) h: usize,
    pub(crate) px: Vec<f64>,
}
impl Gray {
    fn get(&self, x: usize, y: usize) -> f64 {
        y.checked_mul(self.w)
            .and_then(|r| r.checked_add(x))
            .and_then(|i| self.px.get(i))
            .copied()
            .unwrap_or(0.0)
    }
    /// Bilinear sample at real coordinates, edges clamped.
    pub(crate) fn sample(&self, x: f64, y: f64) -> f64 {
        bilinear(self.w, self.h, x, y, |i| self.px.get(i).copied().unwrap_or(0.0))
    }
}

/// Bilinear interpolation of `get(index)` at `(x, y)` over a `w x h` grid, coordinates clamped to
/// the pixel centres `[0, w - 1] x [0, h - 1]`.
pub(crate) fn bilinear(w: usize, h: usize, x: f64, y: f64, get: impl Fn(usize) -> f64) -> f64 {
    if w == 0 || h == 0 {
        return 0.0;
    }
    let xm = w.saturating_sub(1) as f64;
    let ym = h.saturating_sub(1) as f64;
    let xc = if x.is_finite() { x.clamp(0.0, xm) } else { 0.0 };
    let yc = if y.is_finite() { y.clamp(0.0, ym) } else { 0.0 };
    let x0 = xc.floor() as usize;
    let y0 = yc.floor() as usize;
    let x1 = x0.saturating_add(1).min(w.saturating_sub(1));
    let y1 = y0.saturating_add(1).min(h.saturating_sub(1));
    let fx = xc - x0 as f64;
    let fy = yc - y0 as f64;
    let at = |xx: usize, yy: usize| yy.checked_mul(w).and_then(|r| r.checked_add(xx)).map(&get).unwrap_or(0.0);
    let top = at(x0, y0) * (1.0 - fx) + at(x1, y0) * fx;
    let bottom = at(x0, y1) * (1.0 - fx) + at(x1, y1) * fx;
    top * (1.0 - fy) + bottom * fy
}

fn dims(a: &RgbaPlane, b: &RgbaPlane) -> Result<(usize, usize), BlendError> {
    if a.bounds != b.bounds || a.bounds.width == 0 || a.bounds.height == 0 {
        return Err(BlendError::Shape);
    }
    for p in [a, b] {
        let n = usize::try_from(p.bounds.area()).map_err(|_| BlendError::Overflow)?;
        if p.data.len() != n {
            return Err(BlendError::Shape);
        }
        if p.data.iter().flatten().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v)) {
            return Err(BlendError::OutOfDomain);
        }
    }
    Ok((a.bounds.width as usize, a.bounds.height as usize))
}

fn weight_ok(w: f64) -> Result<f64, BlendError> {
    if w.is_finite() && (0.0..=1.0).contains(&w) { Ok(w) } else { Err(BlendError::Parameter) }
}

fn admit(needed: u64, limit: u64) -> Result<(), BlendError> {
    if needed > limit {
        return Err(BlendError::OverBudget { needed, limit });
    }
    Ok(())
}

fn premul(p: [f32; 4]) -> [f64; 4] {
    let a = f64::from(p[3]);
    [f64::from(p[0]) * a, f64::from(p[1]) * a, f64::from(p[2]) * a, a]
}

fn straight(p: [f64; 4]) -> [f32; 4] {
    let a = p[3].clamp(0.0, 1.0);
    if a <= 0.0 {
        return [0.0; 4];
    }
    let c = |v: f64| (v / a).clamp(0.0, 1.0) as f32;
    [c(p[0]), c(p[1]), c(p[2]), a as f32]
}

/// Frame Mix: `(1 - weight_b) A + weight_b B` in premultiplied binary64 (module docs).
pub fn frame_mix(a: &RgbaPlane, b: &RgbaPlane, weight_b: f64, limits: &BlendLimits) -> Result<RgbaPlane, BlendError> {
    let w = weight_ok(weight_b)?;
    dims(a, b)?;
    let bytes = a.bounds.area().checked_mul(16).ok_or(BlendError::Overflow)?;
    admit(bytes, limits.max_bytes)?;
    let data = a
        .data
        .iter()
        .zip(&b.data)
        .map(|(pa, pb)| {
            let (x, y) = (premul(*pa), premul(*pb));
            straight([0, 1, 2, 3].map(|c| (1.0 - w) * x[c] + w * y[c]))
        })
        .collect();
    Ok(RgbaPlane { bounds: a.bounds, data })
}

/// BT.709 luma of the premultiplied colour.
pub(crate) fn luma(p: &RgbaPlane, w: usize, h: usize) -> Gray {
    let px = p
        .data
        .iter()
        .map(|s| {
            let q = premul(*s);
            0.2126 * q[0] + 0.7152 * q[1] + 0.0722 * q[2]
        })
        .collect();
    Gray { w, h, px }
}

fn half(v: usize) -> usize {
    v.checked_div(2).unwrap_or(0)
}

fn down(g: &Gray) -> Gray {
    let (w, h) = (half(g.w).max(1), half(g.h).max(1));
    let mut px = Vec::with_capacity(w.saturating_mul(h));
    for y in 0..h {
        for x in 0..w {
            let (x0, y0) = (x.saturating_mul(2), y.saturating_mul(2));
            let (x1, y1) = (x0.saturating_add(1).min(g.w.saturating_sub(1)), y0.saturating_add(1).min(g.h.saturating_sub(1)));
            px.push((g.get(x0, y0) + g.get(x1, y0) + g.get(x0, y1) + g.get(x1, y1)) / 4.0);
        }
    }
    Gray { w, h, px }
}

/// Levels actually used: at most `levels`, coarsest side at least 8 px.
fn level_count(w: usize, h: usize, levels: u32) -> u32 {
    let (mut w, mut h, mut n) = (w, h, 1u32);
    while n < levels && half(w) >= 8 && half(h) >= 8 {
        w = half(w);
        h = half(h);
        n = n.saturating_add(1);
    }
    n
}

/// Bytes and sample visits of one flow estimate (`n` = finest pixel count).
fn flow_cost(n: u64, p: &FlowParams) -> Result<(u64, u64), BlendError> {
    // Pyramid of two frames (4/3 n each), flow, warped frame, gradients, update planes: ~16 f64.
    let bytes = n.checked_mul(256).ok_or(BlendError::Overflow)?;
    let per_level = u64::from(p.warps)
        .checked_mul(u64::from(p.iterations).checked_add(4).ok_or(BlendError::Overflow)?)
        .and_then(|v| v.checked_mul(24))
        .ok_or(BlendError::Overflow)?;
    // Sum over levels of n / 4^k is below 4/3 n; charge 2 n per level stack to stay conservative.
    let work = n.checked_mul(2).and_then(|v| v.checked_mul(per_level)).ok_or(BlendError::Overflow)?;
    Ok((bytes, work))
}

/// Admission of `steps` flow estimates over `area` pixels: parameters, per-estimate bytes and
/// the aggregate work of all steps (refused before any allocation).
pub(crate) fn flow_admit(area: u64, p: &FlowParams, steps: u64, limits: &BlendLimits) -> Result<(), BlendError> {
    p.validate()?;
    let (bytes, work) = flow_cost(area, p)?;
    admit(bytes, limits.max_bytes)?;
    admit(work.checked_mul(steps).ok_or(BlendError::Overflow)?, limits.max_work)
}

/// Horn-Schunck at one level, refining `(u, v)` in place (module docs).
fn refine_level(a: &Gray, b: &Gray, u: &mut [f64], v: &mut [f64], p: &FlowParams, cancel: &CancellationToken) -> Result<(), BlendError> {
    let (w, h) = (a.w, a.h);
    let n = w.saturating_mul(h);
    let a2 = p.alpha * p.alpha;
    let idx = |x: usize, y: usize| y.saturating_mul(w).saturating_add(x);
    for _ in 0..p.warps {
        cancel.check().map_err(|_| BlendError::Canceled)?;
        // Warp B toward A with the current flow; gradients of the mean of A and warped B.
        let mut bw = vec![0.0f64; n];
        for y in 0..h {
            for x in 0..w {
                let i = idx(x, y);
                let (fu, fv) = (u.get(i).copied().unwrap_or(0.0), v.get(i).copied().unwrap_or(0.0));
                if let Some(s) = bw.get_mut(i) {
                    *s = b.sample(x as f64 + fu, y as f64 + fv);
                }
            }
        }
        let m = Gray {
            w,
            h,
            px: a.px.iter().zip(&bw).map(|(p, q)| (p + q) / 2.0).collect(),
        };
        let (mut ix, mut iy, mut it) = (vec![0.0f64; n], vec![0.0f64; n], vec![0.0f64; n]);
        for y in 0..h {
            for x in 0..w {
                let i = idx(x, y);
                let (xl, xr) = (x.saturating_sub(1), x.saturating_add(1).min(w.saturating_sub(1)));
                let (yu, yd) = (y.saturating_sub(1), y.saturating_add(1).min(h.saturating_sub(1)));
                let dx = if xr > xl { (m.get(xr, y) - m.get(xl, y)) / xr.saturating_sub(xl) as f64 } else { 0.0 };
                let dy = if yd > yu { (m.get(x, yd) - m.get(x, yu)) / yd.saturating_sub(yu) as f64 } else { 0.0 };
                if let (Some(gx), Some(gy), Some(gt)) = (ix.get_mut(i), iy.get_mut(i), it.get_mut(i)) {
                    *gx = dx;
                    *gy = dy;
                    *gt = bw.get(i).copied().unwrap_or(0.0) - a.px.get(i).copied().unwrap_or(0.0);
                }
            }
        }
        let (u0, v0) = (u.to_vec(), v.to_vec());
        let avg = |f: &[f64], x: usize, y: usize| -> f64 {
            let g = |xx: usize, yy: usize| f.get(idx(xx, yy)).copied().unwrap_or(0.0);
            let (xl, xr) = (x.saturating_sub(1), x.saturating_add(1).min(w.saturating_sub(1)));
            let (yu, yd) = (y.saturating_sub(1), y.saturating_add(1).min(h.saturating_sub(1)));
            (g(xl, y) + g(xr, y) + g(x, yu) + g(x, yd)) / 6.0 + (g(xl, yu) + g(xr, yu) + g(xl, yd) + g(xr, yd)) / 12.0
        };
        for _ in 0..p.iterations {
            let (cu, cv) = (u.to_vec(), v.to_vec());
            for y in 0..h {
                for x in 0..w {
                    let i = idx(x, y);
                    let (ub, vb) = (avg(&cu, x, y), avg(&cv, x, y));
                    let (gx, gy, gt) = (
                        ix.get(i).copied().unwrap_or(0.0),
                        iy.get(i).copied().unwrap_or(0.0),
                        it.get(i).copied().unwrap_or(0.0),
                    );
                    let (bu, bv) = (u0.get(i).copied().unwrap_or(0.0), v0.get(i).copied().unwrap_or(0.0));
                    let k = (gx * (ub - bu) + gy * (vb - bv) + gt) / (a2 + gx * gx + gy * gy);
                    if let (Some(su), Some(sv)) = (u.get_mut(i), v.get_mut(i)) {
                        *su = ub - gx * k;
                        *sv = vb - gy * k;
                    }
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn flow_gray(a: &Gray, b: &Gray, p: &FlowParams, cancel: &CancellationToken) -> Result<FlowField, BlendError> {
    let levels = level_count(a.w, a.h, p.levels);
    let (mut pa, mut pb) = (vec![a.clone()], vec![b.clone()]);
    for _ in 1..levels {
        let (na, nb) = (
            down(pa.last().ok_or(BlendError::Shape)?),
            down(pb.last().ok_or(BlendError::Shape)?),
        );
        pa.push(na);
        pb.push(nb);
    }
    let mut u: Vec<f64> = Vec::new();
    let mut v: Vec<f64> = Vec::new();
    let (mut pw, mut ph) = (0usize, 0usize);
    for (ga, gb) in pa.iter().zip(&pb).rev() {
        let n = ga.w.saturating_mul(ga.h);
        // Upsample the coarser flow (nearest, values doubled) or start at zero.
        let (mut nu, mut nv) = (vec![0.0f64; n], vec![0.0f64; n]);
        if pw > 0 && ph > 0 {
            for y in 0..ga.h {
                for x in 0..ga.w {
                    let (sx, sy) = (half(x).min(pw.saturating_sub(1)), half(y).min(ph.saturating_sub(1)));
                    let j = sy.saturating_mul(pw).saturating_add(sx);
                    let i = y.saturating_mul(ga.w).saturating_add(x);
                    if let (Some(a1), Some(b1)) = (nu.get_mut(i), nv.get_mut(i)) {
                        *a1 = u.get(j).copied().unwrap_or(0.0) * 2.0;
                        *b1 = v.get(j).copied().unwrap_or(0.0) * 2.0;
                    }
                }
            }
        }
        refine_level(ga, gb, &mut nu, &mut nv, p, cancel)?;
        u = nu;
        v = nv;
        pw = ga.w;
        ph = ga.h;
    }
    Ok(FlowField {
        width: a.w,
        height: a.h,
        u,
        v,
    })
}

/// Dense flow from `a` to `b` on BT.709 luma (module docs). Budget-checked before allocation.
pub fn optical_flow(
    a: &RgbaPlane,
    b: &RgbaPlane,
    params: &FlowParams,
    limits: &BlendLimits,
    cancel: &CancellationToken,
) -> Result<FlowField, BlendError> {
    params.validate()?;
    let (w, h) = dims(a, b)?;
    let (bytes, work) = flow_cost(a.bounds.area(), params)?;
    admit(bytes, limits.max_bytes)?;
    admit(work, limits.max_work)?;
    cancel.check().map_err(|_| BlendError::Canceled)?;
    flow_gray(&luma(a, w, h), &luma(b, w, h), params, cancel)
}

/// Pixel Motion: the frame at `weight_b` between `a` (0) and `b` (1) by bidirectional optical
/// flow (module docs). `weight_b` 0 or 1 returns that frame unchanged (no flow is estimated).
pub fn pixel_motion(
    a: &RgbaPlane,
    b: &RgbaPlane,
    weight_b: f64,
    params: &FlowParams,
    limits: &BlendLimits,
    cancel: &CancellationToken,
) -> Result<RgbaPlane, BlendError> {
    let t = weight_ok(weight_b)?;
    params.validate()?;
    let (w, h) = dims(a, b)?;
    let (bytes, work) = flow_cost(a.bounds.area(), params)?;
    let bytes = bytes.checked_mul(2).and_then(|v| v.checked_add(a.bounds.area().checked_mul(64)?)).ok_or(BlendError::Overflow)?;
    admit(bytes, limits.max_bytes)?;
    admit(work.checked_mul(2).ok_or(BlendError::Overflow)?, limits.max_work)?;
    cancel.check().map_err(|_| BlendError::Canceled)?;
    if t == 0.0 {
        return Ok(a.clone());
    }
    if t == 1.0 {
        return Ok(b.clone());
    }
    let (la, lb) = (luma(a, w, h), luma(b, w, h));
    let f01 = flow_gray(&la, &lb, params, cancel)?;
    let f10 = flow_gray(&lb, &la, params, cancel)?;
    let pa: Vec<[f64; 4]> = a.data.iter().map(|p| premul(*p)).collect();
    let pb: Vec<[f64; 4]> = b.data.iter().map(|p| premul(*p)).collect();
    let chan = |plane: &[[f64; 4]], c: usize, x: f64, y: f64| -> f64 {
        bilinear(w, h, x, y, |i| plane.get(i).map(|p| p[c]).unwrap_or(0.0))
    };
    let mut data = Vec::with_capacity(pa.len());
    for y in 0..h {
        for x in 0..w {
            let [u01, v01] = f01.at(x, y);
            let [u10, v10] = f10.at(x, y);
            let (t0u, t0v) = (-(1.0 - t) * t * u01 + t * t * u10, -(1.0 - t) * t * v01 + t * t * v10);
            let (t1u, t1v) = (
                (1.0 - t) * (1.0 - t) * u01 - t * (1.0 - t) * u10,
                (1.0 - t) * (1.0 - t) * v01 - t * (1.0 - t) * v10,
            );
            let (xf, yf) = (x as f64, y as f64);
            let px = [0, 1, 2, 3].map(|c| {
                (1.0 - t) * chan(&pa, c, xf + t0u, yf + t0v) + t * chan(&pb, c, xf + t1u, yf + t1v)
            });
            data.push(straight(px));
        }
    }
    Ok(RgbaPlane { bounds: a.bounds, data })
}

/// Rectangle helper for callers building frames.
pub fn frame_rect(width: u32, height: u32) -> Rect {
    Rect { x: 0, y: 0, width, height }
}
