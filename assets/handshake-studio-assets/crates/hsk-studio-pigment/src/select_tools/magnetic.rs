//! Magnetic lasso (STU-RAS-018 row "Magnetic lasso | Edge-snapping selection with width,
//! contrast, and frequency controls"), lane SMB-PS-SELECT-LIQUIFY-2.
//!
//! Clean-room from Mortensen and Barrett, "Intelligent Scissors for Image Composition"
//! (SIGGRAPH 1995; no code read): local link cost
//! `l(p, q) = wZ fZ(q) + wG fG(q) + wD fD(p, q)` with `wZ = 0.43`, `wG = 0.43`, `wD = 0.14`;
//! `fZ` = 0 on a Laplacian zero crossing else 1; `fG = 1 - G / max G` (scaled by `1 / sqrt 2`
//! for 4-neighbour links, the paper's distance weighting); `fD = 2 / (3 pi) (acos dp + acos dq)`
//! from the unit edge directions perpendicular to the gradient; optimal paths by Dijkstra over
//! the 8-connected pixel graph from each anchor ("live wire").
//!
//! Studio readings (Photoshop's magnetic lasso is UNVERIFIED, D5): the gradient is the largest
//! Sobel magnitude over the premultiplied RGB channels, the Laplacian is taken on the channel
//! mean; `width` snaps each traced sample to the strongest edge within that radius when its
//! gradient reaches `contrast` percent of the ROI maximum; `frequency` (`> 0`) places an anchor
//! every `max(2, 200 / frequency)` pixels of traced length; the wire closes back to the first
//! anchor and fills even-odd through the pixel centres.
#![deny(clippy::arithmetic_side_effects)]
use super::{Composite, RgbaImage, ToolError, ToolLimits, admit_plane, index_in};
use crate::selection::{self, ColourView, CoveragePlane, FillRule, SelectionContext};
use crate::{Error, Rect};
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// Magnetic lasso controls (Studio defaults; no clamping, out-of-domain values refuse).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MagneticOptions {
    /// Edge-detection width in pixels (`>= 0`).
    pub width: f64,
    /// Edge contrast in percent of the ROI's strongest gradient (`>= 0`).
    pub contrast: f64,
    /// Anchor frequency (`> 0`).
    pub frequency: f64,
    pub sample_merged: bool,
    /// ROI growth around the trace in pixels.
    pub margin_px: u32,
}
impl Default for MagneticOptions {
    fn default() -> Self {
        Self {
            width: 10.0,
            contrast: 10.0,
            frequency: 57.0,
            sample_merged: false,
            margin_px: 16,
        }
    }
}

/// Working bytes per ROI pixel: image (32) + gradient, laplacian, direction (32) + distance,
/// predecessor and done flag (13) + heap share (8 links x 16).
pub const WORK_BYTES_PER_PIXEL: u64 = RgbaImage::BYTES_PER_PIXEL + 32 + 13 + 128;

const W_Z: f64 = 0.43;
const W_G: f64 = 0.43;
const W_D: f64 = 0.14;
const NEIGHBOURS: [(i64, i64); 8] = [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)];

/// Cost maps of an ROI (the "live wire" graph).
pub struct LiveWire {
    bounds: Rect,
    grad: Vec<f64>,
    zero: Vec<bool>,
    dir: Vec<[f64; 2]>,
    max_grad: f64,
}

impl LiveWire {
    /// Builds the cost maps of `layers` over `roi` (budget admitted before allocation).
    pub fn new(src: &Composite<'_>, roi: Rect, limits: &ToolLimits) -> Result<Self, ToolError> {
        limits.admit(roi.area(), WORK_BYTES_PER_PIXEL)?;
        let img = RgbaImage::from_composite(src, roi, limits)?;
        let (w, h) = (roi.width as usize, roi.height as usize);
        let n = img.px.len();
        let at = |x: usize, y: usize, c: usize| -> f64 {
            let xi = x.min(w.saturating_sub(1));
            let yi = y.min(h.saturating_sub(1));
            img.px.get(yi.saturating_mul(w).saturating_add(xi)).map_or(0.0, |p| p[c])
        };
        let mut grad = vec![0.0f64; n];
        let mut dir = vec![[0.0f64; 2]; n];
        let mut lap = vec![0.0f64; n];
        for y in 0..h {
            for x in 0..w {
                let (xm, xp, ym, yp) = (x.saturating_sub(1), x.saturating_add(1), y.saturating_sub(1), y.saturating_add(1));
                let mut best = (0.0f64, 0.0f64, 0.0f64);
                for c in 0..3 {
                    let gx = (at(xp, ym, c) + 2.0 * at(xp, y, c) + at(xp, yp, c)) - (at(xm, ym, c) + 2.0 * at(xm, y, c) + at(xm, yp, c));
                    let gy = (at(xm, yp, c) + 2.0 * at(x, yp, c) + at(xp, yp, c)) - (at(xm, ym, c) + 2.0 * at(x, ym, c) + at(xp, ym, c));
                    let m = gx.hypot(gy) / 8.0;
                    if m > best.0 {
                        best = (m, gx, gy);
                    }
                }
                let i = y.saturating_mul(w).saturating_add(x);
                let mean = |xx: usize, yy: usize| (at(xx, yy, 0) + at(xx, yy, 1) + at(xx, yy, 2)) / 3.0;
                if let (Some(g), Some(d), Some(l)) = (grad.get_mut(i), dir.get_mut(i), lap.get_mut(i)) {
                    *g = best.0;
                    let len = best.1.hypot(best.2);
                    // Edge direction: the gradient turned 90 degrees.
                    *d = if len > 0.0 { [best.2 / len, -best.1 / len] } else { [0.0, 0.0] };
                    *l = mean(xm, y) + mean(xp, y) + mean(x, ym) + mean(x, yp) - 4.0 * mean(x, y);
                }
            }
        }
        let max_grad = grad.iter().copied().fold(0.0f64, f64::max);
        let mut zero = vec![false; n];
        for y in 0..h {
            for x in 0..w {
                let i = y.saturating_mul(w).saturating_add(x);
                let li = lap.get(i).copied().unwrap_or(0.0);
                let mut z = false;
                for (dx, dy) in [(1usize, 0usize), (0, 1)] {
                    let (nx, ny) = (x.saturating_add(dx), y.saturating_add(dy));
                    if nx >= w || ny >= h {
                        continue;
                    }
                    let lj = lap.get(ny.saturating_mul(w).saturating_add(nx)).copied().unwrap_or(0.0);
                    // Zero crossing: opposite signs, the pixel nearer zero carries it.
                    if li * lj < 0.0 && li.abs() <= lj.abs() {
                        z = true;
                    }
                }
                if let Some(slot) = zero.get_mut(i) {
                    *slot |= z;
                }
                for (dx, dy) in [(1usize, 0usize), (0, 1)] {
                    let (nx, ny) = (x.saturating_add(dx), y.saturating_add(dy));
                    if nx >= w || ny >= h {
                        continue;
                    }
                    let j = ny.saturating_mul(w).saturating_add(nx);
                    let lj = lap.get(j).copied().unwrap_or(0.0);
                    if li * lj < 0.0 && lj.abs() < li.abs()
                        && let Some(slot) = zero.get_mut(j)
                    {
                        *slot = true;
                    }
                }
            }
        }
        Ok(Self { bounds: roi, grad, zero, dir, max_grad })
    }

    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    /// Gradient magnitude at a document pixel (0 outside the ROI).
    pub fn gradient(&self, x: i64, y: i64) -> f64 {
        index_in(self.bounds, x, y).and_then(|i| self.grad.get(i)).copied().unwrap_or(0.0)
    }

    fn link_cost(&self, p: usize, q: usize, diagonal: bool) -> f64 {
        let fz = if self.zero.get(q) == Some(&true) { 0.0 } else { 1.0 };
        let g = self.grad.get(q).copied().unwrap_or(0.0);
        let mut fg = if self.max_grad > 0.0 { 1.0 - g / self.max_grad } else { 1.0 };
        if !diagonal {
            fg /= std::f64::consts::SQRT_2;
        }
        let w = (self.bounds.width as usize).max(1);
        let (px, py) = (p.checked_rem(w).unwrap_or(0) as f64, p.checked_div(w).unwrap_or(0) as f64);
        let (qx, qy) = (q.checked_rem(w).unwrap_or(0) as f64, q.checked_div(w).unwrap_or(0) as f64);
        let dp = self.dir.get(p).copied().unwrap_or([0.0; 2]);
        let dq = self.dir.get(q).copied().unwrap_or([0.0; 2]);
        let (mut lx, mut ly) = (qx - px, qy - py);
        if dp[0] * lx + dp[1] * ly < 0.0 {
            lx = -lx;
            ly = -ly;
        }
        let len = lx.hypot(ly).max(1e-12);
        let (lx, ly) = (lx / len, ly / len);
        let a = (dp[0] * lx + dp[1] * ly).clamp(-1.0, 1.0).acos();
        let b = (lx * dq[0] + ly * dq[1]).clamp(-1.0, 1.0).acos();
        let fd = 2.0 / (3.0 * std::f64::consts::PI) * (a + b);
        W_Z * fz + W_G * fg + W_D * fd
    }

    /// Minimum-cost 8-connected path from `from` to `to` (both inside the ROI), endpoints
    /// included.
    pub fn path(&self, from: (i64, i64), to: (i64, i64)) -> Result<Vec<(i64, i64)>, ToolError> {
        let s = index_in(self.bounds, from.0, from.1).ok_or(Error::InvalidInput)?;
        let t = index_in(self.bounds, to.0, to.1).ok_or(Error::InvalidInput)?;
        let n = self.grad.len();
        let mut dist = vec![f64::INFINITY; n];
        let mut prev = vec![u32::MAX; n];
        let mut done = vec![false; n];
        let mut heap = BinaryHeap::new();
        if let Some(d) = dist.get_mut(s) {
            *d = 0.0;
        }
        heap.push(Reverse((0u64, s)));
        let w = self.bounds.width as usize;
        while let Some(Reverse((bits, u))) = heap.pop() {
            if done.get(u) == Some(&true) {
                continue;
            }
            if let Some(f) = done.get_mut(u) {
                *f = true;
            }
            if u == t {
                break;
            }
            let du = f64::from_bits(bits);
            let (ux, uy) = (u.checked_rem(w.max(1)).unwrap_or(0) as i64, u.checked_div(w.max(1)).unwrap_or(0) as i64);
            for (dx, dy) in NEIGHBOURS {
                let Some(v) = index_in(
                    Rect { x: 0, y: 0, width: self.bounds.width, height: self.bounds.height },
                    ux.saturating_add(dx),
                    uy.saturating_add(dy),
                ) else {
                    continue;
                };
                if done.get(v) == Some(&true) {
                    continue;
                }
                let nd = du + self.link_cost(u, v, dx != 0 && dy != 0);
                if nd < dist.get(v).copied().unwrap_or(f64::INFINITY) {
                    if let Some(d) = dist.get_mut(v) {
                        *d = nd;
                    }
                    if let Some(p) = prev.get_mut(v) {
                        *p = u32::try_from(u).map_err(|_| Error::Overflow)?;
                    }
                    // Non-negative f64 order equals its bit order.
                    heap.push(Reverse((nd.to_bits(), v)));
                }
            }
        }
        let mut out = Vec::new();
        let mut cur = t;
        loop {
            let (x, y) = super::coords_of(self.bounds, cur);
            out.push((x, y));
            if cur == s {
                break;
            }
            let p = prev.get(cur).copied().unwrap_or(u32::MAX);
            if p == u32::MAX || out.len() > n {
                return Err(Error::InvalidInput.into());
            }
            cur = p as usize;
        }
        out.reverse();
        Ok(out)
    }

    /// The strongest-gradient pixel within `radius` of `p` when it reaches `contrast` percent of
    /// the ROI maximum, else `p` itself (clamped into the ROI).
    pub fn snap(&self, p: (i64, i64), radius: f64, contrast: f64) -> (i64, i64) {
        let b = self.bounds;
        let (ex, ey) = (b.x.saturating_add(i64::from(b.width)).saturating_sub(1), b.y.saturating_add(i64::from(b.height)).saturating_sub(1));
        let p = (p.0.clamp(b.x, ex), p.1.clamp(b.y, ey));
        let r = radius.floor().min(1.0e6) as i64;
        let mut best = (self.gradient(p.0, p.1), p);
        for dy in r.saturating_neg()..=r {
            for dx in r.saturating_neg()..=r {
                let (x, y) = (p.0.saturating_add(dx), p.1.saturating_add(dy));
                if dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy)) as f64 > radius * radius {
                    continue;
                }
                let g = self.gradient(x, y);
                if g > best.0 {
                    best = (g, (x, y));
                }
            }
        }
        if best.0 >= self.max_grad * contrast / 100.0 && best.0 > 0.0 { best.1 } else { p }
    }
}

/// Magnetic lasso from a traced cursor path (document pixel coordinates): samples snap to edges
/// within `width`, anchors are placed by `frequency`, live-wire paths join consecutive anchors and
/// close back to the first; the closed wire fills even-odd through pixel centres.
pub fn magnetic_lasso(
    layers: &[ColourView<'_>],
    active: usize,
    trace: &[[f64; 2]],
    opts: &MagneticOptions,
    ctx: &SelectionContext,
    limits: &ToolLimits,
) -> Result<CoveragePlane, ToolError> {
    for v in [opts.width, opts.contrast, opts.frequency] {
        if !v.is_finite() {
            return Err(Error::Nonfinite.into());
        }
    }
    if opts.width < 0.0 || opts.contrast < 0.0 || opts.frequency <= 0.0 {
        return Err(ToolError::OutOfRange);
    }
    if trace.len() > ctx.limits.max_points {
        return Err(Error::BudgetExceeded.into());
    }
    if trace.iter().any(|p| !p[0].is_finite() || !p[1].is_finite()) {
        return Err(Error::Nonfinite.into());
    }
    if trace.len() < 3 {
        return Err(Error::InvalidInput.into());
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for p in trace {
        x0 = x0.min(p[0]);
        y0 = y0.min(p[1]);
        x1 = x1.max(p[0]);
        y1 = y1.max(p[1]);
    }
    let grow = opts.width.ceil() + f64::from(opts.margin_px);
    let (gx0, gy0, gx1, gy1) = ((x0 - grow).floor(), (y0 - grow).floor(), (x1 + grow).ceil(), (y1 + grow).ceil());
    if [gx0, gy0, gx1, gy1].iter().any(|v| v.abs() > 9.0e15) {
        return Err(Error::Overflow.into());
    }
    let span = Rect {
        x: gx0 as i64,
        y: gy0 as i64,
        width: u32::try_from((gx1 - gx0) as i64).map_err(|_| Error::Overflow)?,
        height: u32::try_from((gy1 - gy0) as i64).map_err(|_| Error::Overflow)?,
    };
    let roi = span.intersect(ctx.canvas)?.ok_or(Error::InvalidInput)?;
    admit_plane(roi, &ctx.limits)?;
    let src = Composite::select(layers, active, opts.sample_merged)?;
    let wire = LiveWire::new(&src, roi, limits)?;
    // Anchors: snapped samples every `spacing` pixels of traced length.
    let spacing = (200.0 / opts.frequency).max(2.0);
    let snap = |p: &[f64; 2]| wire.snap((p[0].floor() as i64, p[1].floor() as i64), opts.width, opts.contrast);
    let mut anchors = vec![snap(&trace[0])];
    let mut run = 0.0f64;
    for w in trace.windows(2) {
        run += (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]);
        if run >= spacing {
            let a = snap(&w[1]);
            if anchors.last() != Some(&a) {
                anchors.push(a);
            }
            run = 0.0;
        }
    }
    if anchors.len() < 3 {
        return Err(Error::InvalidInput.into());
    }
    let mut points: Vec<[f64; 2]> = Vec::new();
    for (i, a) in anchors.iter().enumerate() {
        let b = anchors.get(i.saturating_add(1)).unwrap_or(&anchors[0]);
        let seg = wire.path(*a, *b)?;
        for (x, y) in seg.iter().skip(usize::from(!points.is_empty())) {
            points.push([*x as f64 + 0.5, *y as f64 + 0.5]);
        }
    }
    Ok(selection::polygon(&points, FillRule::EvenOdd, ctx)?)
}
