//! Select and Mask / Refine Edge: the unified edge-refinement surface of STU-RAS-121 as an engine
//! operator. Controls `radius`, `smart_radius`, `smooth`, `feather`, `contrast`, `shift_edge`,
//! `decontaminate_colours`, `decontamination_amount` (STU-RAS-121 declares their units and
//! precision but no bounds or defaults: nothing is clamped, only values outside a formula's
//! domain refuse `tool_out_of_range`, and [`RefineOptions::default`] is a Studio default).
//!
//! Matting, clean-room from the papers (no code read):
//! - He, Sun and Tang, "Guided Image Filtering" (ECCV 2010 / TPAMI 2013): colour guided filter,
//!   `a_k = (Sigma_k + eps U)^-1 (mean(I p)_k - mu_k mean(p)_k)`, `b_k = mean(p)_k - a_k . mu_k`,
//!   `q_i = mean(a)_i . I_i + mean(b)_i`.
//! - Levin, Lischinski and Weiss, "A Closed-Form Solution to Natural Image Matting" (CVPR 2006 /
//!   TPAMI 2008): minimise `alpha^T L alpha + lambda (alpha - alpha_known)^T D (alpha -
//!   alpha_known)`; solved here by conjugate gradients with the matrix-free product of He, Sun and
//!   Tang, "Fast Matting Using Large Kernel Matting Laplacian Matrices" (CVPR 2010):
//!   `(L p)_i = sum_{k in w_i} (p_i - a_k . I_i - b_k)` with `eps / |w_k|` per window.
//!
//! Pipeline (Studio reading; Photoshop order and formulas UNVERIFIED, D5): binary selection
//! (coverage >= 0.5) -> `smooth` (majority vote over a `(2 round(smooth) + 1)^2` window) ->
//! edge band = pixels whose Euclidean distance to the boundary (Felzenszwalb-Huttenlocher exact
//! distance transform) is below the band radius -> matte in the band (guided filter or
//! closed-form) -> `feather` (Gaussian, [`crate::selection::feather`]) -> `contrast` ->
//! `shift_edge` -> optional colour decontamination.
#![deny(clippy::arithmetic_side_effects)]
use super::{Composite, RgbaImage, ToolError, ToolLimits, admit_plane, coords_of, index_in, tighten};
use crate::selection::{ColourView, CoveragePlane, SelectionContext, feather};
use crate::{Error, Rect};

/// Matte solver for the edge band.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MatteMethod {
    /// One guided-filter pass of the binary selection (fast).
    #[default]
    GuidedFilter,
    /// Closed-form matting, conjugate gradients on the matting Laplacian.
    ClosedForm,
}

/// STU-RAS-121 numeric controls plus solver settings. Defaults are Studio defaults (the spec
/// declares none).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RefineOptions {
    /// Edge-band radius in pixels (`>= 0`; 0 = no band, the hard selection).
    pub radius: f64,
    /// Adapt the band per pixel: narrow at hard edges, full at soft ones.
    pub smart_radius: bool,
    /// Unitless smoothing count (`>= 0`).
    pub smooth: f64,
    /// Gaussian feather radius in pixels (`>= 0`).
    pub feather: f64,
    /// Percent, signed; `>= 100` hardens to a step at 0.5.
    pub contrast: f64,
    /// Percent, signed: `alpha^(2^(-shift / 50))` (positive grows the selection).
    pub shift_edge: f64,
    pub decontaminate_colours: bool,
    /// Percent (`>= 0`): band colours move `amount * min(1, 4 (1 - alpha))` of the way to the
    /// `alpha^4`-weighted local mean colour (the nearby opaque foreground).
    pub decontamination_amount: f64,
    pub method: MatteMethod,
    /// Matting window radius (window side `2 r + 1`, `>= 1`).
    pub window_radius: u32,
    /// Regulariser `eps` (`> 0`), on encoded colour values in `[0, 1]`.
    pub epsilon: f64,
    /// Closed-form data weight `lambda` on the known pixels (`> 0`).
    pub lambda: f64,
    /// Closed-form conjugate-gradient iteration cap.
    pub max_iterations: u32,
    /// Arithmetic budget (pixel-operations) refused before work.
    pub max_ops: u64,
    pub sample_merged: bool,
}
impl Default for RefineOptions {
    fn default() -> Self {
        Self {
            radius: 4.0,
            smart_radius: false,
            smooth: 0.0,
            feather: 0.0,
            contrast: 0.0,
            shift_edge: 0.0,
            decontaminate_colours: false,
            decontamination_amount: 100.0,
            method: MatteMethod::GuidedFilter,
            window_radius: 2,
            epsilon: 1e-5,
            lambda: 100.0,
            max_iterations: 400,
            max_ops: 1 << 34,
            sample_merged: false,
        }
    }
}

/// Refined selection: the coverage plane, the band size and (with decontamination) straight RGB
/// colours over `coverage.bounds` (row-major) to write into the output layer.
#[derive(Clone, Debug, PartialEq)]
pub struct RefineOutput {
    pub coverage: CoveragePlane,
    /// Pixels the matte solver treated as unknown.
    pub band_pixels: u64,
    pub colours: Option<Vec<[f32; 3]>>,
}

/// Working bytes per pixel: image (32) + 13 window statistics and coefficient planes (~30 f64)
/// + matte, band, distance and conjugate-gradient vectors (~10 f64).
pub const WORK_BYTES_PER_PIXEL: u64 = RgbaImage::BYTES_PER_PIXEL + 40 * 8;

fn fin(v: f64) -> Result<f64, ToolError> {
    if v.is_finite() { Ok(v) } else { Err(Error::Nonfinite.into()) }
}

fn check(o: &RefineOptions) -> Result<(), ToolError> {
    for v in [o.radius, o.smooth, o.feather, o.contrast, o.shift_edge, o.decontamination_amount, o.epsilon, o.lambda] {
        fin(v)?;
    }
    if o.radius < 0.0
        || o.smooth < 0.0
        || o.feather < 0.0
        || o.decontamination_amount < 0.0
        || o.epsilon <= 0.0
        || o.lambda <= 0.0
        || o.window_radius == 0
    {
        return Err(ToolError::OutOfRange);
    }
    Ok(())
}

/// Grid of `w x h` for the separable filters.
#[derive(Clone, Copy)]
struct Dims {
    w: usize,
    h: usize,
}
impl Dims {
    fn n(self) -> usize {
        self.w.saturating_mul(self.h)
    }
}

/// Clipped box sums of `src` over `(2 r + 1)^2` windows (separable running sums).
fn box_sum(src: &[f64], d: Dims, r: usize) -> Vec<f64> {
    let mut tmp = vec![0.0f64; d.n()];
    for y in 0..d.h {
        let row = y.saturating_mul(d.w);
        let mut acc = 0.0f64;
        // Window [x - r, x + r] clipped; add the entering sample, drop the leaving one.
        for x in 0..r.min(d.w) {
            acc += src.get(row.saturating_add(x)).copied().unwrap_or(0.0);
        }
        for x in 0..d.w {
            if let Some(v) = x.checked_add(r).filter(|e| *e < d.w) {
                acc += src.get(row.saturating_add(v)).copied().unwrap_or(0.0);
            }
            if let Some(slot) = tmp.get_mut(row.saturating_add(x)) {
                *slot = acc;
            }
            if let Some(v) = x.checked_sub(r) {
                acc -= src.get(row.saturating_add(v)).copied().unwrap_or(0.0);
            }
        }
    }
    let mut out = vec![0.0f64; d.n()];
    for x in 0..d.w {
        let at = |y: usize| y.saturating_mul(d.w).saturating_add(x);
        let mut acc = 0.0f64;
        for y in 0..r.min(d.h) {
            acc += tmp.get(at(y)).copied().unwrap_or(0.0);
        }
        for y in 0..d.h {
            if let Some(v) = y.checked_add(r).filter(|e| *e < d.h) {
                acc += tmp.get(at(v)).copied().unwrap_or(0.0);
            }
            if let Some(slot) = out.get_mut(at(y)) {
                *slot = acc;
            }
            if let Some(v) = y.checked_sub(r) {
                acc -= tmp.get(at(v)).copied().unwrap_or(0.0);
            }
        }
    }
    out
}

/// Separable sliding max (`max = true`) or min of `src` over `(2 r + 1)^2` clipped windows.
fn box_extreme(src: &[f64], d: Dims, r: usize, max: bool) -> Vec<f64> {
    let pick = |a: f64, b: f64| if max { a.max(b) } else { a.min(b) };
    let init = if max { f64::NEG_INFINITY } else { f64::INFINITY };
    let pass = |input: &[f64], horizontal: bool| -> Vec<f64> {
        let mut out = vec![init; d.n()];
        let (outer, inner) = if horizontal { (d.h, d.w) } else { (d.w, d.h) };
        for o in 0..outer {
            for i in 0..inner {
                let lo = i.saturating_sub(r);
                let hi = i.saturating_add(r).min(inner.saturating_sub(1));
                let mut m = init;
                for j in lo..=hi {
                    let idx = if horizontal {
                        o.saturating_mul(d.w).saturating_add(j)
                    } else {
                        j.saturating_mul(d.w).saturating_add(o)
                    };
                    m = pick(m, input.get(idx).copied().unwrap_or(init));
                }
                let idx = if horizontal {
                    o.saturating_mul(d.w).saturating_add(i)
                } else {
                    i.saturating_mul(d.w).saturating_add(o)
                };
                if let Some(slot) = out.get_mut(idx) {
                    *slot = m;
                }
            }
        }
        out
    };
    let h = pass(src, true);
    pass(&h, false)
}

/// Felzenszwalb-Huttenlocher 1-D squared distance transform of `f` (lower envelope of parabolas).
fn edt_1d(f: &[f64], out: &mut [f64], v: &mut [usize], z: &mut [f64]) {
    let n = f.len();
    if n == 0 {
        return;
    }
    let mut k = 0usize;
    if let Some(v0) = v.first_mut() {
        *v0 = 0;
    }
    if let Some(z0) = z.first_mut() {
        *z0 = f64::NEG_INFINITY;
    }
    if let Some(z1) = z.get_mut(1) {
        *z1 = f64::INFINITY;
    }
    let fq = |q: usize| f.get(q).copied().unwrap_or(f64::INFINITY);
    for q in 1..n {
        let qf = q as f64;
        let mut s;
        loop {
            let vk = v.get(k).copied().unwrap_or(0);
            let vf = vk as f64;
            s = ((fq(q) + qf * qf) - (fq(vk) + vf * vf)) / (2.0 * qf - 2.0 * vf);
            if k > 0 && s <= z.get(k).copied().unwrap_or(f64::NEG_INFINITY) {
                k = k.saturating_sub(1);
                continue;
            }
            break;
        }
        k = k.saturating_add(1);
        if let Some(slot) = v.get_mut(k) {
            *slot = q;
        }
        if let Some(slot) = z.get_mut(k) {
            *slot = s;
        }
        if let Some(slot) = z.get_mut(k.saturating_add(1)) {
            *slot = f64::INFINITY;
        }
    }
    k = 0;
    for (q, o) in out.iter_mut().enumerate().take(n) {
        while z.get(k.saturating_add(1)).copied().unwrap_or(f64::INFINITY) < q as f64 {
            k = k.saturating_add(1);
        }
        let vk = v.get(k).copied().unwrap_or(0);
        let dq = q as f64 - vk as f64;
        *o = dq * dq + fq(vk);
    }
}

/// Squared Euclidean distance from every pixel to the nearest pixel where `target` holds.
fn edt(target: &[bool], d: Dims) -> Vec<f64> {
    const FAR: f64 = 1.0e20;
    let mut g: Vec<f64> = target.iter().map(|t| if *t { 0.0 } else { FAR }).collect();
    let len = d.w.max(d.h);
    let (mut f, mut out) = (vec![0.0f64; len], vec![0.0f64; len]);
    let (mut v, mut z) = (vec![0usize; len], vec![0.0f64; len.saturating_add(1)]);
    for x in 0..d.w {
        for y in 0..d.h {
            if let Some(s) = f.get_mut(y) {
                *s = g.get(y.saturating_mul(d.w).saturating_add(x)).copied().unwrap_or(FAR);
            }
        }
        edt_1d(f.get(..d.h).unwrap_or(&[]), out.get_mut(..d.h).unwrap_or(&mut []), &mut v, &mut z);
        for y in 0..d.h {
            if let Some(s) = g.get_mut(y.saturating_mul(d.w).saturating_add(x)) {
                *s = out.get(y).copied().unwrap_or(FAR);
            }
        }
    }
    for y in 0..d.h {
        let row = y.saturating_mul(d.w);
        for x in 0..d.w {
            if let Some(s) = f.get_mut(x) {
                *s = g.get(row.saturating_add(x)).copied().unwrap_or(FAR);
            }
        }
        edt_1d(f.get(..d.w).unwrap_or(&[]), out.get_mut(..d.w).unwrap_or(&mut []), &mut v, &mut z);
        for x in 0..d.w {
            if let Some(s) = g.get_mut(row.saturating_add(x)) {
                *s = out.get(x).copied().unwrap_or(FAR);
            }
        }
    }
    g
}

/// Window statistics of the guide: mean colour and `(Sigma_k + e_k U)^-1` per window centre.
struct Guide {
    d: Dims,
    r: usize,
    colour: Vec<[f64; 3]>,
    mean: Vec<[f64; 3]>,
    inv: Vec<[f64; 6]>,
    count: Vec<f64>,
}

/// Inverse of the symmetric 3x3 `[xx, xy, xz, yy, yz, zz]`.
fn inv3(m: [f64; 6]) -> [f64; 6] {
    let [a, b, c, d, e, f] = m;
    let (c00, c01, c02) = (d * f - e * e, c * e - b * f, b * e - c * d);
    let (c11, c12, c22) = (a * f - c * c, b * c - a * e, a * d - b * b);
    let det = a * c00 + b * c01 + c * c02;
    if det.abs() < 1e-300 {
        return [0.0; 6];
    }
    [c00 / det, c01 / det, c02 / det, c11 / det, c12 / det, c22 / det]
}

impl Guide {
    /// `laplacian`: regulariser `eps / |w_k|` (matting Laplacian) instead of `eps` (guided filter).
    fn new(colour: Vec<[f64; 3]>, d: Dims, r: usize, eps: f64, laplacian: bool) -> Self {
        let ones = vec![1.0f64; d.n()];
        let count = box_sum(&ones, d, r);
        let chan = |c: usize| -> Vec<f64> { colour.iter().map(|p| p[c]).collect() };
        let sums: Vec<Vec<f64>> = (0..3).map(|c| box_sum(&chan(c), d, r)).collect();
        let pairs = [(0, 0), (0, 1), (0, 2), (1, 1), (1, 2), (2, 2)];
        let prods: Vec<Vec<f64>> = pairs
            .iter()
            .map(|(i, j)| box_sum(&colour.iter().map(|p| p[*i] * p[*j]).collect::<Vec<_>>(), d, r))
            .collect();
        let mut mean = Vec::with_capacity(d.n());
        let mut inv = Vec::with_capacity(d.n());
        for k in 0..d.n() {
            let n = count.get(k).copied().unwrap_or(1.0).max(1.0);
            let mu = [0, 1, 2].map(|c| sums.get(c).and_then(|s| s.get(k)).copied().unwrap_or(0.0) / n);
            let e = if laplacian { eps / n } else { eps };
            let mut cov = [0.0f64; 6];
            for (slot, ((i, j), p)) in cov.iter_mut().zip(pairs.iter().zip(&prods)) {
                *slot = p.get(k).copied().unwrap_or(0.0) / n - mu[*i] * mu[*j];
            }
            cov[0] += e;
            cov[3] += e;
            cov[5] += e;
            mean.push(mu);
            inv.push(inv3(cov));
        }
        Self { d, r, colour, mean, inv, count }
    }

    /// Per-window coefficients `a_k`, `b_k` of `p`.
    fn coeffs(&self, p: &[f64]) -> (Vec<[f64; 3]>, Vec<f64>) {
        let sp = box_sum(p, self.d, self.r);
        let sip: Vec<Vec<f64>> = (0..3)
            .map(|c| {
                let v: Vec<f64> = self.colour.iter().zip(p).map(|(i, q)| i[c] * q).collect();
                box_sum(&v, self.d, self.r)
            })
            .collect();
        let mut a = Vec::with_capacity(self.d.n());
        let mut b = Vec::with_capacity(self.d.n());
        for k in 0..self.d.n() {
            let n = self.count.get(k).copied().unwrap_or(1.0).max(1.0);
            let pm = sp.get(k).copied().unwrap_or(0.0) / n;
            let mu = self.mean.get(k).copied().unwrap_or([0.0; 3]);
            let cv = [0, 1, 2].map(|c| sip.get(c).and_then(|s| s.get(k)).copied().unwrap_or(0.0) / n - mu[c] * pm);
            let m = self.inv.get(k).copied().unwrap_or([0.0; 6]);
            let ak = [
                m[0] * cv[0] + m[1] * cv[1] + m[2] * cv[2],
                m[1] * cv[0] + m[3] * cv[1] + m[4] * cv[2],
                m[2] * cv[0] + m[4] * cv[1] + m[5] * cv[2],
            ];
            b.push(pm - (ak[0] * mu[0] + ak[1] * mu[1] + ak[2] * mu[2]));
            a.push(ak);
        }
        (a, b)
    }

    /// Box sums of `a . I_i + b` contributions: returns `sum_{k in w_i} (a_k . I_i + b_k)`.
    fn gather(&self, a: &[[f64; 3]], b: &[f64]) -> Vec<f64> {
        let sa: Vec<Vec<f64>> = (0..3)
            .map(|c| box_sum(&a.iter().map(|v| v[c]).collect::<Vec<_>>(), self.d, self.r))
            .collect();
        let sb = box_sum(b, self.d, self.r);
        (0..self.d.n())
            .map(|i| {
                let px = self.colour.get(i).copied().unwrap_or([0.0; 3]);
                let s = |c: usize| sa.get(c).and_then(|v| v.get(i)).copied().unwrap_or(0.0);
                s(0) * px[0] + s(1) * px[1] + s(2) * px[2] + sb.get(i).copied().unwrap_or(0.0)
            })
            .collect()
    }

    /// Guided filter output `q_i = mean(a)_i . I_i + mean(b)_i`.
    fn filter(&self, p: &[f64]) -> Vec<f64> {
        let (a, b) = self.coeffs(p);
        let g = self.gather(&a, &b);
        g.iter()
            .zip(&self.count)
            .map(|(v, n)| v / n.max(1.0))
            .collect()
    }

    /// Matting Laplacian product `L p`.
    fn laplacian(&self, p: &[f64]) -> Vec<f64> {
        let (a, b) = self.coeffs(p);
        let g = self.gather(&a, &b);
        p.iter()
            .zip(&self.count)
            .zip(&g)
            .map(|((pi, n), gi)| n * pi - gi)
            .collect()
    }
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Closed-form matte: conjugate gradients on `(L + lambda D) x = lambda D known`.
fn closed_form(guide: &Guide, known: &[bool], target: &[f64], lambda: f64, iterations: u32) -> Vec<f64> {
    let apply = |p: &[f64]| -> Vec<f64> {
        let lp = guide.laplacian(p);
        lp.iter()
            .zip(p)
            .zip(known)
            .map(|((l, v), k)| l + if *k { lambda * v } else { 0.0 })
            .collect()
    };
    let rhs: Vec<f64> = target.iter().zip(known).map(|(t, k)| if *k { lambda * t } else { 0.0 }).collect();
    let mut x = target.to_vec();
    let ax = apply(&x);
    let mut r: Vec<f64> = rhs.iter().zip(&ax).map(|(b, a)| b - a).collect();
    let mut p = r.clone();
    let mut rs = dot(&r, &r);
    let stop = 1e-10 * dot(&rhs, &rhs).max(1.0);
    for _ in 0..iterations {
        if rs <= stop {
            break;
        }
        let ap = apply(&p);
        let pap = dot(&p, &ap);
        if pap <= 0.0 {
            break;
        }
        let alpha = rs / pap;
        for (xi, pi) in x.iter_mut().zip(&p) {
            *xi += alpha * pi;
        }
        for (ri, api) in r.iter_mut().zip(&ap) {
            *ri -= alpha * api;
        }
        let rs_new = dot(&r, &r);
        let beta = rs_new / rs;
        for (pi, ri) in p.iter_mut().zip(&r) {
            *pi = ri + beta * *pi;
        }
        rs = rs_new;
    }
    x
}

/// Refine `selection` against the image of `layers` (bottom-to-top; `active` unless
/// `sample_merged`). The work region is the selection grown by the band, window and feather
/// reach, clipped to the canvas; nothing outside it changes.
pub fn refine_edge(
    layers: &[ColourView<'_>],
    active: usize,
    selection: &CoveragePlane,
    opts: &RefineOptions,
    ctx: &SelectionContext,
    limits: &ToolLimits,
) -> Result<RefineOutput, ToolError> {
    check(opts)?;
    let reach_f = opts.radius.ceil() + opts.smooth.ceil() + f64::from(opts.window_radius) + 2.0;
    if reach_f > 1.0e9 {
        return Err(Error::Overflow.into());
    }
    let reach = reach_f as i64;
    let sel = selection.bounds;
    let (sx, sy) = sel.end()?;
    let (cx, cy) = ctx.canvas.end()?;
    let x0 = sel.x.saturating_sub(reach).max(ctx.canvas.x);
    let y0 = sel.y.saturating_sub(reach).max(ctx.canvas.y);
    let x1 = sx.saturating_add(reach).min(cx);
    let y1 = sy.saturating_add(reach).min(cy);
    if x1 <= x0 || y1 <= y0 || sel.area() == 0 {
        return Ok(RefineOutput {
            coverage: CoveragePlane {
                bounds: Rect { x: ctx.canvas.x, y: ctx.canvas.y, width: 0, height: 0 },
                data: Vec::new(),
            },
            band_pixels: 0,
            colours: None,
        });
    }
    let region = Rect {
        x: x0,
        y: y0,
        width: u32::try_from(x1.checked_sub(x0).ok_or(Error::Overflow)?).map_err(|_| Error::Overflow)?,
        height: u32::try_from(y1.checked_sub(y0).ok_or(Error::Overflow)?).map_err(|_| Error::Overflow)?,
    };
    admit_plane(region, &ctx.limits)?;
    limits.admit(region.area(), WORK_BYTES_PER_PIXEL)?;
    // Operation budget: per pixel ~40 box-filter passes per matte evaluation, the smart-radius
    // range filter (2 (2 R + 1) per channel) and the smoothing window.
    let n64 = region.area();
    let evals = match opts.method {
        MatteMethod::GuidedFilter => 1u64,
        MatteMethod::ClosedForm => u64::from(opts.max_iterations).saturating_add(2),
    };
    let band_r = opts.radius.ceil() as u64;
    let smart = if opts.smart_radius { band_r.saturating_mul(2).saturating_add(1).saturating_mul(14) } else { 0 };
    let per_px = evals.saturating_mul(40).saturating_add(smart).saturating_add(64);
    if n64.saturating_mul(per_px) > opts.max_ops {
        return Err(Error::BudgetExceeded.into());
    }
    let src = Composite::select(layers, active, opts.sample_merged)?;
    let img = RgbaImage::from_composite(&src, region, limits)?;
    let d = Dims { w: region.width as usize, h: region.height as usize };
    let n = d.n();

    // Binary selection, then smoothing by majority vote.
    let mut hard: Vec<bool> = (0..n)
        .map(|i| {
            let (x, y) = coords_of(region, i);
            selection.at(x, y) >= 32768
        })
        .collect();
    let s = opts.smooth.round() as usize;
    if s > 0 {
        let as_f: Vec<f64> = hard.iter().map(|h| if *h { 1.0 } else { 0.0 }).collect();
        let sums = box_sum(&as_f, d, s);
        let ones = box_sum(&vec![1.0; n], d, s);
        hard = sums.iter().zip(&ones).map(|(a, c)| *a * 2.0 >= *c).collect();
    }

    // Edge band from exact distances to the other side.
    let to_bg = edt(&hard.iter().map(|h| !h).collect::<Vec<_>>(), d);
    let to_fg = edt(&hard, d);
    let colour: Vec<[f64; 3]> = img.px.iter().map(|p| [p[0], p[1], p[2]]).collect();
    let band_radius: Vec<f64> = if opts.smart_radius && opts.radius > 0.0 {
        // Studio reading: sharpness = (largest 4-neighbour step within 3x3) / (colour range over
        // the band window), maximised over the band window; band = radius * (0.25 + 0.75 (1 -
        // sharpness)): a hard step narrows the band to a quarter, a soft ramp keeps it.
        let mut step = vec![0.0f64; n];
        for i in 0..n {
            let x = i.checked_rem(d.w.max(1)).unwrap_or(0);
            let c = colour.get(i).copied().unwrap_or([0.0; 3]);
            let mut m = 0.0f64;
            if x.saturating_add(1) < d.w {
                let q = colour.get(i.saturating_add(1)).copied().unwrap_or(c);
                m = m.max((0..3).map(|k| (c[k] - q[k]).abs()).fold(0.0, f64::max));
            }
            if i.saturating_add(d.w) < n {
                let q = colour.get(i.saturating_add(d.w)).copied().unwrap_or(c);
                m = m.max((0..3).map(|k| (c[k] - q[k]).abs()).fold(0.0, f64::max));
            }
            if let Some(slot) = step.get_mut(i) {
                *slot = m;
            }
        }
        let grad = box_extreme(&step, d, 1, true);
        let rr = band_r as usize;
        let mut range = vec![0.0f64; n];
        for k in 0..3 {
            let ch: Vec<f64> = colour.iter().map(|p| p[k]).collect();
            let hi = box_extreme(&ch, d, rr, true);
            let lo = box_extreme(&ch, d, rr, false);
            for ((slot, h), l) in range.iter_mut().zip(&hi).zip(&lo) {
                *slot = slot.max(h - l);
            }
        }
        // Sharpness is measured where the edge is and spread over the band window.
        let sharp: Vec<f64> = grad
            .iter()
            .zip(&range)
            .map(|(g, c)| if *c > 0.0 { (g / c).clamp(0.0, 1.0) } else { 0.0 })
            .collect();
        box_extreme(&sharp, d, rr, true)
            .iter()
            .map(|s| opts.radius * (0.25 + 0.75 * (1.0 - s)))
            .collect()
    } else {
        vec![opts.radius; n]
    };
    let unknown: Vec<bool> = (0..n)
        .map(|i| {
            let d2 = if hard.get(i) == Some(&true) { to_bg.get(i) } else { to_fg.get(i) };
            let dist = d2.copied().unwrap_or(f64::INFINITY).sqrt() - 0.5;
            dist < band_radius.get(i).copied().unwrap_or(0.0)
        })
        .collect();
    let band_pixels = unknown.iter().filter(|u| **u).count() as u64;
    let target: Vec<f64> = hard.iter().map(|h| if *h { 1.0 } else { 0.0 }).collect();
    let r = opts.window_radius as usize;
    let mut alpha = if band_pixels == 0 {
        target.clone()
    } else {
        match opts.method {
            MatteMethod::GuidedFilter => {
                let guide = Guide::new(colour.clone(), d, r, opts.epsilon, false);
                let q = guide.filter(&target);
                target
                    .iter()
                    .zip(&q)
                    .zip(&unknown)
                    .map(|((t, q), u)| if *u { q.clamp(0.0, 1.0) } else { *t })
                    .collect()
            }
            MatteMethod::ClosedForm => {
                let guide = Guide::new(colour.clone(), d, r, opts.epsilon, true);
                let known: Vec<bool> = unknown.iter().map(|u| !u).collect();
                closed_form(&guide, &known, &target, opts.lambda, opts.max_iterations)
                    .into_iter()
                    .zip(&target)
                    .zip(&unknown)
                    .map(|((a, t), u)| if *u { a.clamp(0.0, 1.0) } else { *t })
                    .collect()
            }
        }
    };

    // Global refinements: feather, contrast, shift edge.
    let to_plane = |a: &[f64]| CoveragePlane {
        bounds: region,
        data: a.iter().map(|v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16).collect(),
    };
    let mut plane = to_plane(&alpha);
    if opts.feather > 0.0 {
        let mut fctx = *ctx;
        fctx.canvas = region;
        plane = feather(&plane, opts.feather as f32, &fctx)?;
        alpha = (0..n)
            .map(|i| {
                let (x, y) = coords_of(region, i);
                f64::from(plane.at(x, y)) / 65535.0
            })
            .collect();
    }
    if opts.contrast != 0.0 || opts.shift_edge != 0.0 {
        let gamma = (-opts.shift_edge / 50.0).exp2();
        for a in alpha.iter_mut() {
            let mut v = *a;
            if opts.contrast >= 100.0 {
                v = if v >= 0.5 { 1.0 } else { 0.0 };
            } else if opts.contrast != 0.0 {
                v = (0.5 + (v - 0.5) * 100.0 / (100.0 - opts.contrast)).clamp(0.0, 1.0);
            }
            if opts.shift_edge != 0.0 && v > 0.0 {
                v = v.powf(gamma);
            }
            *a = v;
        }
        plane = to_plane(&alpha);
    }
    let coverage = tighten(plane)?;

    // Colour decontamination: pull band colours toward the nearby opaque foreground.
    let colours = if opts.decontaminate_colours {
        let rr = (opts.radius.ceil() as usize).max(2);
        let wgt: Vec<f64> = alpha.iter().map(|a| a.powi(4)).collect();
        let sw = box_sum(&wgt, d, rr);
        let sc: Vec<Vec<f64>> = (0..3)
            .map(|k| {
                let v: Vec<f64> = (0..n).map(|i| wgt.get(i).copied().unwrap_or(0.0) * img.straight(i)[k]).collect();
                box_sum(&v, d, rr)
            })
            .collect();
        let amount = opts.decontamination_amount / 100.0;
        let cb = coverage.bounds;
        let (ex, ey) = cb.end()?;
        let mut out = Vec::with_capacity(usize::try_from(cb.area()).map_err(|_| Error::Overflow)?);
        for y in cb.y..ey {
            for x in cb.x..ex {
                let i = index_in(region, x, y).ok_or(Error::Overflow)?;
                let c = img.straight(i);
                let a = alpha.get(i).copied().unwrap_or(0.0);
                let w = sw.get(i).copied().unwrap_or(0.0);
                let px = if w > 0.0 {
                    let t = amount * (4.0 * (1.0 - a)).min(1.0);
                    [0, 1, 2].map(|k| {
                        let f = sc.get(k).and_then(|v| v.get(i)).copied().unwrap_or(0.0) / w;
                        (c[k] + t * (f - c[k])).clamp(0.0, 1.0) as f32
                    })
                } else {
                    c.map(|v| v as f32)
                };
                out.push(px);
            }
        }
        Some(out)
    } else {
        None
    };
    Ok(RefineOutput { coverage, band_pixels, colours })
}

/// Refine surface description (Studio readings; Photoshop NOT_PROVEN, UNVERIFIED, D5).
pub const DESCRIPTOR: &str = r#"{"module":"STUDIO-MODULE-PIGMENT","surface":"select_tools.refine","version":1,"spec_basis":["STU-RAS-020","STU-RAS-121"],"controls":{"radius":"pixels, band = exact EDT distance - 0.5 < radius","smart_radius":"band radius * (0.25 + 0.75 (1 - s)), s = max over the band window of (largest 3x3 neighbour step / colour range over the band window)","smooth":"majority vote, window 2 round(smooth) + 1","feather":"Gaussian via selection::feather","contrast":"0.5 + (a - 0.5) * 100 / (100 - c); c >= 100 = step at 0.5","shift_edge":"a^(2^(-s/50))","decontaminate_colours":"colour += amount min(1, 4 (1 - a)) (alpha^4-weighted local mean colour within max(ceil(radius), 2) - colour)"},"bounds":"no clamping (STU-RAS-121: bounds undeclared); only out-of-domain values refuse tool_out_of_range","matte":{"guided_filter":"He et al. 2010 colour guided filter of the binary selection in the band","closed_form":"Levin et al. 2008 matting Laplacian, conjugate gradients, matrix-free product of He et al. 2010"},"not_implemented":["view_mode","output_to (host)","remember_settings","refinement brush set STU-RAS-122"],"parity":"NOT_PROVEN"}"#;
