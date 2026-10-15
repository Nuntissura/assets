//! Liquify (mesh push) as a re-editable displacement mesh (STU-RAS-140 row "Liquify", STU-RAS-142
//! row "Liquify" brush `size`, `density`, `pressure`, `rate`, STU-FX-024 forward warp, pucker,
//! bloat, twirl, freeze / thaw mask, reconstruct / smooth, "distortion is stored as a re-editable
//! mesh"). Lane SMB-PS-SELECT-LIQUIFY.
//!
//! Model: a regular vertex grid over the layer bounds stores a backward displacement `D(v)`; the
//! output pixel whose centre is `p` shows the source at `p + D(p)`, with `D` bilinear between
//! vertices. Rendering goes through pigment's reference resampler
//! ([`crate::transform_math::PreparedTransform`], one translation per output pixel, so the
//! nearest / bilinear / Catmull-Rom rules and rule 1 "exact centre" are the transform lane's): a
//! zero displacement samples the texel itself, so an identity mesh, [`LiquifyMesh::reconstruct_all`]
//! and a full-strength reconstruct brush give byte-identical pixels.
//!
//! Brush tools compose with the existing warp (`M'(p) = M(q)`, `D'(p) = q + D(q) - p`):
//! forward warp `q = p - w d` (d = drag step), push left = forward warp along the drag direction
//! turned 90 degrees to the left, twirl `q = c + R(-+theta w)(p - c)`, pucker
//! `q = c + (1 + k w)(p - c)`, bloat `q = c + (1 - k w)(p - c)`, reconstruct `D' = (1 - k w) D`
//! (exactly 0 when `k w >= 1`), smooth `D' = D + k w (mean of the 4 neighbours - D)`. Dabs come
//! from the one brush engine ([`crate::brush::place_dabs`], 10 % spacing). Weight
//! `w = ((1 - density) (1 - (r/R)^2)^2 + density) (1 - freeze)` inside the radius `R`, 0 outside.
//! The step constants (twirl 0.1 rad, pucker / bloat 0.05 per dab at rate 100 % and pressure
//! 100 %) and the falloff are Studio readings; Photoshop's are UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use crate::brush::{BrushDynamics, BrushError, BrushLimits, BrushTip, Dab, StrokeSample, place_dabs};
use crate::selection::{ColourView, CoveragePlane};
use crate::transform_math::{Affine2, EdgeMode, LayerTransform, PreparedTransform, ResampleFilter, TransformError};
use crate::{Error, MAX_BYTES, Rect};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Typed failure of a liquify operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiquifyError {
    Pigment(Error),
    Brush(BrushError),
    Transform(TransformError),
    /// A brush or mesh parameter outside its formula's domain (never clamped).
    OutOfRange,
}
impl LiquifyError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Pigment(e) => e.code(),
            Self::Brush(e) => e.code(),
            Self::Transform(e) => e.code(),
            Self::OutOfRange => "liquify_out_of_range",
        }
    }
}
impl From<Error> for LiquifyError {
    fn from(e: Error) -> Self {
        Self::Pigment(e)
    }
}
impl From<BrushError> for LiquifyError {
    fn from(e: BrushError) -> Self {
        Self::Brush(e)
    }
}
impl From<TransformError> for LiquifyError {
    fn from(e: TransformError) -> Self {
        Self::Transform(e)
    }
}
impl fmt::Display for LiquifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for LiquifyError {}

/// Budgets checked before allocation or work (V4-02).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiquifyLimits {
    /// Mesh vertices (a mesh bomb refuses before allocating).
    pub max_vertices: u64,
    /// Dabs per stroke.
    pub max_dabs: usize,
    /// Dab x affected-vertex updates per stroke.
    pub max_work: u64,
    /// Rendered output bytes (16 per pixel: rgb_f32le + alpha f32le).
    pub max_output_bytes: u64,
}
impl Default for LiquifyLimits {
    fn default() -> Self {
        Self {
            max_vertices: 1 << 22,
            max_dabs: 100_000,
            max_work: 1 << 32,
            max_output_bytes: 16 * MAX_BYTES,
        }
    }
}

/// The liquify tools of STU-FX-024 and STU-RAS-140 (turbulence and mesh clone: lane
/// SMB-PS-SELECT-LIQUIFY-2).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum LiquifyTool {
    ForwardWarp,
    PushLeft,
    Twirl { clockwise: bool },
    Pucker,
    Bloat,
    Reconstruct,
    Smooth,
    /// Adds a smooth seeded value-noise displacement: per dab
    /// `D += 0.05 size rate pressure w n(v)`, `n` bilinear between lattice values in `[-1, 1]`
    /// (SplitMix64 of `(seed, cell, axis)`) on cells of `size / 2` pixels.
    Turbulence { seed: u64 },
    /// Copies the distortion found at `v + (dx, dy)`: `D' = D + k w (D(v + offset) - D)`,
    /// `k = min(1, rate pressure)`.
    MeshClone { dx: f64, dy: f64 },
}

fn splitmix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Lattice value in `[-1, 1]` of cell `(i, j)` and `axis`.
fn lattice(seed: u64, i: i64, j: i64, axis: u64) -> f64 {
    let h = splitmix(seed ^ splitmix((i as u64).wrapping_mul(0x632B_E59B_D9B4_E019) ^ splitmix((j as u64) ^ axis.wrapping_mul(0x8CB9_2BA7_2F3D_8DD7))));
    (h >> 11) as f64 / 9_007_199_254_740_992.0 * 2.0 - 1.0
}

/// Smooth value noise (bilinear lattice, smoothstep weights) at `(x, y)` with cell `cell`.
fn value_noise(seed: u64, x: f64, y: f64, cell: f64, axis: u64) -> f64 {
    let (u, v) = (x / cell, y / cell);
    let (i, j) = (u.floor(), v.floor());
    let (tx, ty) = (u - i, v - j);
    let (sx, sy) = (tx * tx * (3.0 - 2.0 * tx), ty * ty * (3.0 - 2.0 * ty));
    let (i, j) = (i as i64, j as i64);
    let (i1, j1) = (i.saturating_add(1), j.saturating_add(1));
    let a = lattice(seed, i, j, axis);
    let b = lattice(seed, i1, j, axis);
    let c = lattice(seed, i, j1, axis);
    let d = lattice(seed, i1, j1, axis);
    let top = a + (b - a) * sx;
    let bottom = c + (d - c) * sx;
    top + (bottom - top) * sy
}

/// STU-RAS-142 liquify brush. No bound is declared: only out-of-domain values refuse (size must
/// be positive; density, pressure, rate non-negative; all finite). Percent values above 100 are
/// admitted and scale linearly. `size` is a diameter; the brush engine's tip range (1..=5000 px)
/// applies.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiquifyBrush {
    pub size: f64,
    pub density: f64,
    pub pressure: f64,
    pub rate: f64,
}
impl Default for LiquifyBrush {
    /// Studio defaults (Photoshop's UNVERIFIED).
    fn default() -> Self {
        Self {
            size: 100.0,
            density: 50.0,
            pressure: 100.0,
            rate: 80.0,
        }
    }
}

/// Re-editable liquify mesh: vertex `(i, j)` sits at pixel centre
/// `(bounds.x + 0.5 + i spacing, bounds.y + 0.5 + j spacing)`; `disp` and `freeze` are row-major
/// per vertex.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiquifyMesh {
    pub bounds: Rect,
    pub spacing: u32,
    pub cols: u32,
    pub rows: u32,
    pub disp: Vec<[f64; 2]>,
    /// Freeze per vertex in `[0, 1]` (1 = fully protected).
    pub freeze: Vec<f32>,
}

fn grid_count(len: u32, spacing: u32) -> Result<u32, Error> {
    // ceil((len - 1) / spacing) + 1 vertices cover pixel centres 0.5 .. len - 0.5.
    let span = len.saturating_sub(1);
    span.div_ceil(spacing).checked_add(1).ok_or(Error::Overflow)
}

fn finite(v: f64) -> Result<f64, LiquifyError> {
    if v.is_finite() { Ok(v) } else { Err(Error::Nonfinite.into()) }
}

impl LiquifyMesh {
    /// Identity mesh over `bounds` with vertex `spacing` (pixels, `>= 1`).
    pub fn new(bounds: Rect, spacing: u32, limits: &LiquifyLimits) -> Result<Self, LiquifyError> {
        if spacing == 0 || bounds.width == 0 || bounds.height == 0 {
            return Err(LiquifyError::OutOfRange);
        }
        bounds.end()?;
        let cols = grid_count(bounds.width, spacing)?;
        let rows = grid_count(bounds.height, spacing)?;
        let n = u64::from(cols).checked_mul(u64::from(rows)).ok_or(Error::Overflow)?;
        if n > limits.max_vertices {
            return Err(Error::BudgetExceeded.into());
        }
        let n = usize::try_from(n).map_err(|_| Error::Overflow)?;
        Ok(Self {
            bounds,
            spacing,
            cols,
            rows,
            disp: vec![[0.0; 2]; n],
            freeze: vec![0.0; n],
        })
    }

    /// Structural check of a deserialized mesh (lengths, finiteness, freeze range, budget).
    pub fn validate(&self, limits: &LiquifyLimits) -> Result<(), LiquifyError> {
        let fresh = Self::new(self.bounds, self.spacing, limits)?;
        if fresh.cols != self.cols || fresh.rows != self.rows || self.disp.len() != fresh.disp.len() || self.freeze.len() != fresh.freeze.len() {
            return Err(Error::InvalidInput.into());
        }
        if self.disp.iter().any(|d| !d[0].is_finite() || !d[1].is_finite()) || self.freeze.iter().any(|f| !f.is_finite()) {
            return Err(Error::Nonfinite.into());
        }
        if self.freeze.iter().any(|f| !(0.0..=1.0).contains(f)) {
            return Err(Error::InvalidInput.into());
        }
        Ok(())
    }

    /// True when every displacement is exactly zero.
    pub fn is_identity(&self) -> bool {
        self.disp.iter().all(|d| d[0] == 0.0 && d[1] == 0.0)
    }

    /// Reconstruct everything: the exact identity.
    pub fn reconstruct_all(&mut self) {
        self.disp.fill([0.0; 2]);
    }

    /// Freeze mask from a coverage plane (a `StudioMask`-style selection): each vertex takes the
    /// coverage of the pixel it sits on. Thaw = a plane combined by `diminish`, or [`Self::thaw_all`].
    pub fn set_freeze(&mut self, plane: &CoveragePlane) {
        for (k, f) in self.freeze.iter_mut().enumerate() {
            let (x, y) = vertex_pixel(self.bounds, self.spacing, self.cols, k);
            let (ex, ey) = (
                self.bounds.x.saturating_add(i64::from(self.bounds.width.saturating_sub(1))),
                self.bounds.y.saturating_add(i64::from(self.bounds.height.saturating_sub(1))),
            );
            *f = f32::from(plane.at(x.min(ex), y.min(ey))) / 65535.0;
        }
    }

    pub fn thaw_all(&mut self) {
        self.freeze.fill(0.0);
    }

    fn vertex_pos(&self, k: usize) -> (f64, f64) {
        let (x, y) = vertex_pixel(self.bounds, self.spacing, self.cols, k);
        (x as f64 + 0.5, y as f64 + 0.5)
    }

    fn at(&self, i: u32, j: u32) -> [f64; 2] {
        let k = (j as usize).saturating_mul(self.cols as usize).saturating_add(i as usize);
        self.disp.get(k).copied().unwrap_or([0.0; 2])
    }

    /// Bilinear displacement at pixel-space point `(px, py)` (clamped to the mesh).
    pub fn displacement_at(&self, px: f64, py: f64) -> [f64; 2] {
        let s = f64::from(self.spacing);
        let u = ((px - self.bounds.x as f64 - 0.5) / s).clamp(0.0, f64::from(self.cols.saturating_sub(1)));
        let v = ((py - self.bounds.y as f64 - 0.5) / s).clamp(0.0, f64::from(self.rows.saturating_sub(1)));
        let (i0, j0) = (u.floor() as u32, v.floor() as u32);
        let (i1, j1) = (i0.saturating_add(1).min(self.cols.saturating_sub(1)), j0.saturating_add(1).min(self.rows.saturating_sub(1)));
        let (tx, ty) = (u - f64::from(i0), v - f64::from(j0));
        let (a, b, c, d) = (self.at(i0, j0), self.at(i1, j0), self.at(i0, j1), self.at(i1, j1));
        if tx == 0.0 && ty == 0.0 {
            return a;
        }
        [0, 1].map(|k| {
            let top = a[k] + (b[k] - a[k]) * tx;
            let bottom = c[k] + (d[k] - c[k]) * tx;
            top + (bottom - top) * ty
        })
    }

    /// Apply one brush stroke of `tool`. All-or-nothing: every budget and parameter is checked
    /// before the mesh changes.
    pub fn apply_stroke(
        &mut self,
        tool: LiquifyTool,
        samples: &[StrokeSample],
        brush: &LiquifyBrush,
        limits: &LiquifyLimits,
    ) -> Result<(), LiquifyError> {
        for v in [brush.size, brush.density, brush.pressure, brush.rate] {
            finite(v)?;
        }
        if let LiquifyTool::MeshClone { dx, dy } = tool {
            finite(dx)?;
            finite(dy)?;
        }
        if brush.size <= 0.0 || brush.density < 0.0 || brush.pressure < 0.0 || brush.rate < 0.0 {
            return Err(LiquifyError::OutOfRange);
        }
        let tip = BrushTip::round(brush.size, 100.0, 10.0);
        let blim = BrushLimits {
            max_dabs: limits.max_dabs,
            max_samples: limits.max_dabs,
            ..BrushLimits::default()
        };
        let mut dabs = place_dabs(&tip, &BrushDynamics::default(), samples, &blim)?;
        let still = samples.windows(2).all(|w| w[0].x == w[1].x && w[0].y == w[1].y);
        if still && samples.len() > 1 {
            // A held pen: one stationary application per sample.
            let first = dabs.first().copied();
            dabs = first.map(|d| vec![d; samples.len()]).unwrap_or_default();
        }
        if dabs.is_empty() {
            return Ok(());
        }
        let radius = brush.size / 2.0;
        let s = f64::from(self.spacing);
        let side = (2.0 * radius / s).ceil() + 2.0;
        let per_dab = (side * side).min(self.disp.len() as f64);
        if dabs.len() as f64 * per_dab > limits.max_work as f64 {
            return Err(Error::BudgetExceeded.into());
        }
        let density = brush.density / 100.0;
        let strength = brush.pressure / 100.0;
        let rate = brush.rate / 100.0;
        let mut updates: Vec<(usize, [f64; 2])> = Vec::new();
        let mut prev: Option<Dab> = None;
        for dab in &dabs {
            let (cx, cy) = (dab.x, dab.y);
            let step = prev.map_or([0.0, 0.0], |p| [cx - p.x, cy - p.y]);
            prev = Some(*dab);
            updates.clear();
            let (i0, i1) = self.vertex_span(cx, radius, true);
            let (j0, j1) = self.vertex_span(cy, radius, false);
            for j in j0..=j1 {
                for i in i0..=i1 {
                    let k = (j as usize).saturating_mul(self.cols as usize).saturating_add(i as usize);
                    let (vx, vy) = self.vertex_pos(k);
                    let r = (vx - cx).hypot(vy - cy);
                    if r >= radius {
                        continue;
                    }
                    let t = r / radius;
                    let fall = (1.0 - t * t) * (1.0 - t * t);
                    let freeze = f64::from(self.freeze.get(k).copied().unwrap_or(0.0));
                    let w = ((1.0 - density) * fall + density) * (1.0 - freeze);
                    if w <= 0.0 {
                        continue;
                    }
                    let d = self.disp.get(k).copied().unwrap_or([0.0; 2]);
                    let compose = |qx: f64, qy: f64| -> [f64; 2] {
                        let dq = self.displacement_at(qx, qy);
                        [qx + dq[0] - vx, qy + dq[1] - vy]
                    };
                    let new = match tool {
                        LiquifyTool::ForwardWarp => {
                            let (mx, my) = (w * strength * step[0], w * strength * step[1]);
                            compose(vx - mx, vy - my)
                        }
                        LiquifyTool::PushLeft => {
                            // Left of the drag direction in y-down pixel space.
                            let (lx, ly) = (step[1], -step[0]);
                            let (mx, my) = (w * strength * lx, w * strength * ly);
                            compose(vx - mx, vy - my)
                        }
                        LiquifyTool::Twirl { clockwise } => {
                            let theta = if clockwise { 1.0 } else { -1.0 } * 0.1 * rate * strength * w;
                            let (sn, cs) = (-theta).sin_cos();
                            let (px, py) = (vx - cx, vy - cy);
                            compose(cx + cs * px - sn * py, cy + sn * px + cs * py)
                        }
                        LiquifyTool::Pucker | LiquifyTool::Bloat => {
                            let k = 0.05 * rate * strength * w;
                            let f = if tool == LiquifyTool::Pucker { 1.0 + k } else { 1.0 - k };
                            compose(cx + f * (vx - cx), cy + f * (vy - cy))
                        }
                        LiquifyTool::Reconstruct => {
                            let k = rate * strength * w;
                            if k >= 1.0 { [0.0, 0.0] } else { [d[0] * (1.0 - k), d[1] * (1.0 - k)] }
                        }
                        LiquifyTool::Turbulence { seed } => {
                            let amp = 0.05 * brush.size * rate * strength * w;
                            let cell = (brush.size / 2.0).max(1.0);
                            [d[0] + amp * value_noise(seed, vx, vy, cell, 0), d[1] + amp * value_noise(seed, vx, vy, cell, 1)]
                        }
                        LiquifyTool::MeshClone { dx, dy } => {
                            let k = (rate * strength).min(1.0) * w;
                            let src = self.displacement_at(vx + dx, vy + dy);
                            [d[0] + k * (src[0] - d[0]), d[1] + k * (src[1] - d[1])]
                        }
                        LiquifyTool::Smooth => {
                            let k = (rate * strength * w).min(1.0);
                            let n = [
                                self.at(i.saturating_sub(1), j),
                                self.at(i.saturating_add(1).min(self.cols.saturating_sub(1)), j),
                                self.at(i, j.saturating_sub(1)),
                                self.at(i, j.saturating_add(1).min(self.rows.saturating_sub(1))),
                            ];
                            [0, 1].map(|c| {
                                let mean = n.iter().map(|v| v[c]).sum::<f64>() / 4.0;
                                d[c] + k * (mean - d[c])
                            })
                        }
                    };
                    updates.push((k, self.keep_near(vx, vy, new)));
                }
            }
            for (k, v) in &updates {
                if let Some(slot) = self.disp.get_mut(*k) {
                    *slot = *v;
                }
            }
        }
        Ok(())
    }

    /// Clamp a displacement so the sample point stays on a pixel centre inside the bounds (edge
    /// pixels repeat beyond the layer; Studio reading).
    fn keep_inside(&self, vx: f64, vy: f64, d: [f64; 2]) -> [f64; 2] {
        let (x0, y0) = (self.bounds.x as f64 + 0.5, self.bounds.y as f64 + 0.5);
        let x1 = x0 + f64::from(self.bounds.width.saturating_sub(1));
        let y1 = y0 + f64::from(self.bounds.height.saturating_sub(1));
        let fix = |v: f64| if v.is_finite() { v } else { 0.0 };
        [fix((vx + d[0]).clamp(x0, x1) - vx), fix((vy + d[1]).clamp(y0, y1) - vy)]
    }

    /// Mesh-time guard: the sample point stays within one vertex spacing of the bounds (render
    /// applies the strict [`Self::keep_inside`]); a zero displacement is kept exactly.
    fn keep_near(&self, vx: f64, vy: f64, d: [f64; 2]) -> [f64; 2] {
        let s = f64::from(self.spacing);
        let (x0, y0) = (self.bounds.x as f64 + 0.5 - s, self.bounds.y as f64 + 0.5 - s);
        let x1 = self.bounds.x as f64 + f64::from(self.bounds.width) - 0.5 + s;
        let y1 = self.bounds.y as f64 + f64::from(self.bounds.height) - 0.5 + s;
        let fix = |v: f64| if v.is_finite() { v } else { 0.0 };
        let tx = vx + d[0];
        let ty = vy + d[1];
        [
            if (x0..=x1).contains(&tx) { fix(d[0]) } else { fix(tx.clamp(x0, x1) - vx) },
            if (y0..=y1).contains(&ty) { fix(d[1]) } else { fix(ty.clamp(y0, y1) - vy) },
        ]
    }

    /// Inclusive vertex index range whose positions lie within `radius` of `c` on one axis.
    fn vertex_span(&self, c: f64, radius: f64, x_axis: bool) -> (u32, u32) {
        let (origin, count) = if x_axis {
            (self.bounds.x as f64 + 0.5, self.cols)
        } else {
            (self.bounds.y as f64 + 0.5, self.rows)
        };
        let s = f64::from(self.spacing);
        let max = f64::from(count.saturating_sub(1));
        let lo = ((c - radius - origin) / s).floor().clamp(0.0, max) as u32;
        let hi = ((c + radius - origin) / s).ceil().clamp(0.0, max) as u32;
        (lo, hi)
    }

    /// Render `src` (whose bounds must equal the mesh bounds) through the mesh with pigment's
    /// reference resampler. Output is `rgb_f32le` + `f32le` alpha over the same bounds, the
    /// format of the input view.
    pub fn render(
        &self,
        src: &ColourView<'_>,
        filter: ResampleFilter,
        limits: &LiquifyLimits,
    ) -> Result<(Vec<u8>, Vec<u8>), LiquifyError> {
        if src.bounds != self.bounds {
            return Err(Error::InvalidInput.into());
        }
        let pixels = self.bounds.area();
        let bytes = pixels.checked_mul(16).ok_or(Error::Overflow)?;
        if bytes > limits.max_output_bytes {
            return Err(Error::BudgetExceeded.into());
        }
        let n = usize::try_from(pixels).map_err(|_| Error::Overflow)?;
        if n.checked_mul(12) != Some(src.colour.len()) || n.checked_mul(4) != Some(src.alpha.len()) {
            return Err(Error::MalformedStride.into());
        }
        let read = |b: &[u8], i: usize| -> f32 {
            let at = i.saturating_mul(4);
            b.get(at..at.saturating_add(4))
                .and_then(|s| s.try_into().ok())
                .map(f32::from_le_bytes)
                .unwrap_or(f32::NAN)
        };
        for i in 0..n {
            let a = read(src.alpha, i);
            if !a.is_finite() || (0..3).any(|c| !read(src.colour, i.saturating_mul(3).saturating_add(c)).is_finite()) {
                return Err(Error::Nonfinite.into());
            }
            if !(0.0..=1.0).contains(&a) {
                return Err(Error::AlphaRange.into());
            }
        }
        let w = self.bounds.width as usize;
        let texel = |i: u32, j: u32| -> Result<[f32; 4], TransformError> {
            let k = (j as usize).saturating_mul(w).saturating_add(i as usize);
            let c = k.saturating_mul(3);
            Ok([
                read(src.colour, c),
                read(src.colour, c.saturating_add(1)),
                read(src.colour, c.saturating_add(2)),
                read(src.alpha, k),
            ])
        };
        let (ex, ey) = self.bounds.end()?;
        let mut colour = Vec::with_capacity(n.saturating_mul(12));
        let mut alpha = Vec::with_capacity(n.saturating_mul(4));
        for y in self.bounds.y..ey {
            for x in self.bounds.x..ex {
                let d = self.displacement_at(x as f64 + 0.5, y as f64 + 0.5);
                let d = self.keep_inside(x as f64 + 0.5, y as f64 + 0.5, d);
                // Destination = source - D, so the inverse samples at p + D.
                let t = LayerTransform::new(Affine2::translate(-d[0], -d[1]), filter).with_edge(EdgeMode::Clamp);
                let p = PreparedTransform::new(&t, self.bounds)?;
                let px = p.sample_rgba(x, y, texel)?;
                for c in &px[..3] {
                    colour.extend_from_slice(&c.to_le_bytes());
                }
                alpha.extend_from_slice(&px[3].to_le_bytes());
            }
        }
        Ok((colour, alpha))
    }
}

/// Nominal pixel of vertex `k` (the last row / column may lie up to `spacing - 1` pixels past the
/// bounds).
fn vertex_pixel(bounds: Rect, spacing: u32, cols: u32, k: usize) -> (i64, i64) {
    let c = (cols as usize).max(1);
    let (i, j) = (k.checked_rem(c).unwrap_or(0), k.checked_div(c).unwrap_or(0));
    let s = i64::from(spacing);
    (
        bounds.x.saturating_add((i as i64).saturating_mul(s)),
        bounds.y.saturating_add((j as i64).saturating_mul(s)),
    )
}

/// Liquify surface description (Studio readings; Photoshop NOT_PROVEN, UNVERIFIED, D5).
pub const DESCRIPTOR: &str = r#"{"module":"STUDIO-MODULE-PIGMENT","surface":"liquify","version":1,"lane":"SMB-PS-SELECT-LIQUIFY","spec_basis":["STU-RAS-140 Liquify","STU-RAS-142 Liquify brush size/density/pressure/rate, freeze/thaw masks are StudioMask values","STU-FX-024"],"mesh":"LiquifyMesh{bounds, spacing, cols, rows, disp backward displacement per vertex, freeze}; serde (re-editable); bilinear between vertices","tools":["forward_warp","push_left","twirl cw|ccw","pucker","bloat","reconstruct","smooth","turbulence(seed): smooth seeded value noise, 0.05 size rate pressure w per dab, cells size/2","mesh_clone(dx,dy): D += min(1, rate pressure) w (D(v+offset) - D)"],"compose":"D'(p) = q + D(q) - p","render":"per pixel translation through transform_math::PreparedTransform (nearest|bilinear|bicubic, rule 1 exact centre): identity / reconstruct_all = byte copy","freeze":"set_freeze(CoveragePlane) per-vertex; thaw_all","limits":{"max_vertices":4194304,"max_dabs":100000,"max_work":4294967296},"not_implemented":["face-aware sliders","GPU path"],"parity":"NOT_PROVEN"}"#;
