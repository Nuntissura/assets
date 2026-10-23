//! Pixel selection engine (STU-RAS-017 / 018 / 118 / 119): coverage planes, the four closed combine
//! modes, rectangle / ellipse / polygon (lasso) producers with anti-aliasing, feather through the
//! existing [`coverage_math::feather_kernel`], colour range and invert. Pure compute: no ports,
//! no leases, no Folio writes. A [`CoveragePlane`] converts to the bytes [`crate::Mask::validate`]
//! accepts, so a selection feeds [`crate::render_plan::MaskInput`] and `masked_replace` unchanged.
//!
//! Every formula here is a documented Studio reading of Adobe help text, class Approximate
//! (oracle list D5): the combine maths, the colour-range fuzziness falloff, the default even-odd
//! lasso fill rule and the 4x4 anti-alias sample count are UNVERIFIED against Photoshop.
#![deny(clippy::arithmetic_side_effects)]
use crate::coverage_math::{self, CoverageError};
use crate::render_plan::MaskFormat;
use crate::{Error, MAX_BYTES, Rect};
use std::fmt;

/// Typed failure of a selection operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionError {
    /// Pigment-level refusal (`Overflow`, `BudgetExceeded`, `Nonfinite`, `InvalidInput`, ...).
    Pigment(Error),
    /// Feather radius refusal from `coverage_math`.
    Coverage(CoverageError),
}
impl SelectionError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Pigment(e) => e.code(),
            Self::Coverage(e) => e.code(),
        }
    }
}
impl From<Error> for SelectionError {
    fn from(e: Error) -> Self {
        Self::Pigment(e)
    }
}
impl From<CoverageError> for SelectionError {
    fn from(e: CoverageError) -> Self {
        Self::Coverage(e)
    }
}
impl fmt::Display for SelectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for SelectionError {}

/// Budgets checked BEFORE any allocation (V4-02).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionLimits {
    /// Largest plane in bytes (2 bytes per sample).
    pub max_bytes: u64,
    /// Largest polygon vertex count.
    pub max_points: usize,
}
impl Default for SelectionLimits {
    fn default() -> Self {
        Self {
            max_bytes: MAX_BYTES,
            max_points: 1_000_000,
        }
    }
}

/// Shared inputs of every producer: the canvas the result is clipped to, the budgets and the
/// document anti-alias toggle (STU-RAS-119; true = 4x4 pixel-centred samples, the render-cpu
/// rule; false = the pixel centre only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionContext {
    pub canvas: Rect,
    pub limits: SelectionLimits,
    pub anti_alias: bool,
}
impl SelectionContext {
    pub fn new(canvas: Rect) -> Self {
        Self {
            canvas,
            limits: SelectionLimits::default(),
            anti_alias: true,
        }
    }
    fn samples(&self) -> u32 {
        if self.anti_alias { 4 } else { 1 }
    }
}

/// STU-RAS-118 closed combine set; discriminants are the spec values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CombineMode {
    Replace = 1,
    Extend = 2,
    Diminish = 3,
    Intersect = 4,
}
impl CombineMode {
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Replace),
            2 => Some(Self::Extend),
            3 => Some(Self::Diminish),
            4 => Some(Self::Intersect),
            _ => None,
        }
    }
}

/// Polygon interior rule. Default even-odd is a Studio reading (Photoshop rule UNVERIFIED, D5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FillRule {
    #[default]
    EvenOdd,
    NonZero,
}

/// Rectangle in f64 pixel space (top-left origin).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RectF {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Coverage plane: u16 coverage / 65535, row-major, `bounds.area()` samples; zero outside.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoveragePlane {
    pub bounds: Rect,
    pub data: Vec<u16>,
}

const FULL: u16 = u16::MAX;

fn check_area(bounds: Rect, limits: &SelectionLimits) -> Result<usize, Error> {
    let bytes = bounds.area().checked_mul(2).ok_or(Error::Overflow)?;
    if bytes > limits.max_bytes {
        return Err(Error::BudgetExceeded);
    }
    usize::try_from(bounds.area()).map_err(|_| Error::Overflow)
}

fn empty_at(x: i64, y: i64) -> CoveragePlane {
    CoveragePlane {
        bounds: Rect {
            x,
            y,
            width: 0,
            height: 0,
        },
        data: Vec::new(),
    }
}

impl CoveragePlane {
    /// A plane of one constant value; the byte budget is checked before allocating.
    pub fn filled(bounds: Rect, value: u16, limits: &SelectionLimits) -> Result<Self, Error> {
        let n = check_area(bounds, limits)?;
        Ok(Self {
            bounds,
            data: vec![value; n],
        })
    }

    /// Coverage at a pixel; 0 outside the bounds.
    pub fn at(&self, x: i64, y: i64) -> u16 {
        let (Some(dx), Some(dy)) = (x.checked_sub(self.bounds.x), y.checked_sub(self.bounds.y))
        else {
            return 0;
        };
        let (Ok(dx), Ok(dy)) = (u32::try_from(dx), u32::try_from(dy)) else {
            return 0;
        };
        if dx >= self.bounds.width || dy >= self.bounds.height {
            return 0;
        }
        (dy as usize)
            .checked_mul(self.bounds.width as usize)
            .and_then(|v| v.checked_add(dx as usize))
            .and_then(|i| self.data.get(i))
            .copied()
            .unwrap_or(0)
    }

    /// Sum of all coverage as a fraction of one pixel (test and area helper).
    pub fn area_sum(&self) -> f64 {
        self.data.iter().map(|v| f64::from(*v)).sum::<f64>() / f64::from(FULL)
    }

    /// Bytes exactly as [`crate::Mask::validate`] accepts (tight stride): `coverage_u8` rounds
    /// `v * 255 / 65535`, `coverage_u16le` is little-endian.
    pub fn to_mask_bytes(&self, format: MaskFormat) -> Result<Vec<u8>, Error> {
        match format {
            MaskFormat::CoverageU8 => Ok(self
                .data
                .iter()
                .map(|v| {
                    let x = u32::from(*v).saturating_mul(255).saturating_add(32767) / 65535;
                    u8::try_from(x).unwrap_or(u8::MAX)
                })
                .collect()),
            MaskFormat::CoverageU16Le => Ok(self.data.iter().flat_map(|v| v.to_le_bytes()).collect()),
            _ => Err(Error::Unsupported),
        }
    }
}

/// Fixed-point `round(a * b / 65535)`.
fn mul_unit(a: u16, b: u16) -> u16 {
    let p = u64::from(a).saturating_mul(u64::from(b));
    let r = p.saturating_mul(2).saturating_add(65535) / 131_070;
    u16::try_from(r).unwrap_or(FULL)
}

fn union_rect(a: Rect, b: Rect) -> Result<Rect, Error> {
    let (ae, af) = a.end()?;
    let (be, bf) = b.end()?;
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    Ok(Rect {
        x,
        y,
        width: u32::try_from(ae.max(be).checked_sub(x).ok_or(Error::Overflow)?)
            .map_err(|_| Error::Overflow)?,
        height: u32::try_from(af.max(bf).checked_sub(y).ok_or(Error::Overflow)?)
            .map_err(|_| Error::Overflow)?,
    })
}

/// STU-RAS-118 combine. Studio formulas (class Approximate): extend `a + b - a*b`, diminish
/// `a * (1 - b)`, intersect `a * b`, replace `b`, in 16-bit fixed point with round-half-up.
/// Result bounds: replace = `new`; extend = union; diminish = `base`; intersect = bounds
/// intersection. A missing base is an empty selection.
pub fn combine(
    base: Option<&CoveragePlane>,
    new: &CoveragePlane,
    mode: CombineMode,
    limits: &SelectionLimits,
) -> Result<CoveragePlane, SelectionError> {
    let empty;
    let base_plane = match base {
        Some(b) => b,
        None => {
            empty = empty_at(new.bounds.x, new.bounds.y);
            &empty
        }
    };
    let bounds = match mode {
        CombineMode::Replace => new.bounds,
        CombineMode::Extend => {
            if base.is_none() {
                new.bounds
            } else {
                union_rect(base_plane.bounds, new.bounds)?
            }
        }
        CombineMode::Diminish => base_plane.bounds,
        CombineMode::Intersect => base_plane
            .bounds
            .intersect(new.bounds)?
            .unwrap_or(Rect {
                x: base_plane.bounds.x,
                y: base_plane.bounds.y,
                width: 0,
                height: 0,
            }),
    };
    let n = check_area(bounds, limits)?;
    let (ex, ey) = bounds.end()?;
    let mut data = Vec::with_capacity(n);
    for y in bounds.y..ey {
        for x in bounds.x..ex {
            let a = base_plane.at(x, y);
            let b = new.at(x, y);
            data.push(match mode {
                CombineMode::Replace => b,
                CombineMode::Extend => {
                    let sum = u32::from(a).saturating_add(u32::from(b));
                    u16::try_from(sum.saturating_sub(u32::from(mul_unit(a, b)))).unwrap_or(FULL)
                }
                CombineMode::Diminish => a.saturating_sub(mul_unit(a, b)),
                CombineMode::Intersect => mul_unit(a, b),
            });
        }
    }
    Ok(CoveragePlane { bounds, data })
}

/// `1 - coverage` over the whole canvas.
pub fn invert(
    plane: &CoveragePlane,
    ctx: &SelectionContext,
) -> Result<CoveragePlane, SelectionError> {
    let bounds = ctx.canvas;
    let n = check_area(bounds, &ctx.limits)?;
    let (ex, ey) = bounds.end()?;
    let mut data = Vec::with_capacity(n);
    for y in bounds.y..ey {
        for x in bounds.x..ex {
            data.push(FULL.saturating_sub(plane.at(x, y)));
        }
    }
    Ok(CoveragePlane { bounds, data })
}

fn finite(v: f64) -> Result<f64, Error> {
    // Bound keeps the later float-to-integer conversions inside i64 with headroom.
    if v.is_finite() && v.abs() < 1.0e15 {
        Ok(v)
    } else if v.is_finite() {
        Err(Error::Overflow)
    } else {
        Err(Error::Nonfinite)
    }
}

/// Plane bounds of a shape's f64 bounding box clipped to the canvas (zero-size when disjoint).
fn clipped_bounds(
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
    canvas: Rect,
) -> Result<Rect, Error> {
    let x0 = min_x.floor() as i64;
    let y0 = min_y.floor() as i64;
    let x1 = max_x.ceil() as i64;
    let y1 = max_y.ceil() as i64;
    let w = x1.checked_sub(x0).ok_or(Error::Overflow)?.max(0);
    let h = y1.checked_sub(y0).ok_or(Error::Overflow)?.max(0);
    let bbox = Rect {
        x: x0,
        y: y0,
        width: u32::try_from(w).unwrap_or(u32::MAX),
        height: u32::try_from(h).unwrap_or(u32::MAX),
    };
    Ok(bbox.intersect(canvas)?.unwrap_or(Rect {
        x: canvas.x,
        y: canvas.y,
        width: 0,
        height: 0,
    }))
}

/// Scanline coverage: `spans(y, out)` pushes the non-overlapping inside intervals `[x0, x1)` of
/// the horizontal line at `y` (pixel space). `n` x `n` pixel-centred samples per pixel.
fn rasterize<F: FnMut(f64, &mut Vec<(f64, f64)>)>(
    bounds: Rect,
    n: u32,
    ctx: &SelectionContext,
    mut spans: F,
) -> Result<CoveragePlane, SelectionError> {
    let cells = check_area(bounds, &ctx.limits)?;
    let w = bounds.width as usize;
    let nn = n as usize;
    let sw = w.checked_mul(nn).ok_or(Error::Overflow)?;
    let mut data = vec![0u16; cells];
    if cells == 0 {
        return Ok(CoveragePlane { bounds, data });
    }
    let mut diff = vec![0i32; sw.checked_add(1).ok_or(Error::Overflow)?];
    let mut buf = Vec::new();
    let nf = f64::from(n);
    for (r, row) in data.chunks_exact_mut(w).enumerate() {
        for sub in 0..n {
            let y = bounds.y as f64 + r as f64 + (f64::from(sub) + 0.5) / nf;
            buf.clear();
            spans(y, &mut buf);
            diff.fill(0);
            for &(x0, x1) in &buf {
                let lo = (((x0 - bounds.x as f64) * nf - 0.5).ceil()).clamp(0.0, sw as f64) as usize;
                let hi = (((x1 - bounds.x as f64) * nf - 0.5).ceil()).clamp(0.0, sw as f64) as usize;
                if hi > lo {
                    if let Some(d) = diff.get_mut(lo) {
                        *d = d.saturating_add(1);
                    }
                    if let Some(d) = diff.get_mut(hi) {
                        *d = d.saturating_sub(1);
                    }
                }
            }
            let mut run = 0i32;
            for (k, d) in diff.iter().take(sw).enumerate() {
                run = run.saturating_add(*d);
                if run > 0
                    && let Some(cell) = k.checked_div(nn).and_then(|i| row.get_mut(i))
                {
                    *cell = cell.saturating_add(1);
                }
            }
        }
    }
    let total = u64::from(n).saturating_mul(u64::from(n));
    for v in &mut data {
        let c = u64::from(*v);
        let scaled = c
            .saturating_mul(u64::from(FULL))
            .saturating_mul(2)
            .saturating_add(total)
            .checked_div(total.saturating_mul(2))
            .unwrap_or(0);
        *v = u16::try_from(scaled).unwrap_or(FULL);
    }
    Ok(CoveragePlane { bounds, data })
}

/// Rectangular marquee; a single row or column is a rectangle of height or width 1.
pub fn rect(r: RectF, ctx: &SelectionContext) -> Result<CoveragePlane, SelectionError> {
    let (x, y, w, h) = (finite(r.x)?, finite(r.y)?, finite(r.width)?, finite(r.height)?);
    if w < 0.0 || h < 0.0 {
        return Err(Error::InvalidInput.into());
    }
    let (x1, y1) = (x + w, y + h);
    let bounds = clipped_bounds(x, y, x1, y1, ctx.canvas)?;
    rasterize(bounds, ctx.samples(), ctx, |sy, out| {
        if sy >= y && sy < y1 {
            out.push((x, x1));
        }
    })
}

/// Elliptical marquee inscribed in the rectangle.
pub fn ellipse(r: RectF, ctx: &SelectionContext) -> Result<CoveragePlane, SelectionError> {
    let (x, y, w, h) = (finite(r.x)?, finite(r.y)?, finite(r.width)?, finite(r.height)?);
    if w < 0.0 || h < 0.0 {
        return Err(Error::InvalidInput.into());
    }
    let (a, b) = (w / 2.0, h / 2.0);
    let (cx, cy) = (x + a, y + b);
    let bounds = clipped_bounds(x, y, x + w, y + h, ctx.canvas)?;
    rasterize(bounds, ctx.samples(), ctx, |sy, out| {
        if b > 0.0 {
            let t = (sy - cy) / b;
            let q = 1.0 - t * t;
            if q > 0.0 {
                let half = a * q.sqrt();
                out.push((cx - half, cx + half));
            }
        }
    })
}

/// Polygonal or freehand lasso (straight segments, implicitly closed). The point cap is checked
/// before any work.
pub fn polygon(
    points: &[[f64; 2]],
    rule: FillRule,
    ctx: &SelectionContext,
) -> Result<CoveragePlane, SelectionError> {
    if points.len() > ctx.limits.max_points {
        return Err(Error::BudgetExceeded.into());
    }
    let mut min = [f64::MAX; 2];
    let mut max = [f64::MIN; 2];
    for p in points {
        for axis in 0..2 {
            let v = finite(p[axis])?;
            min[axis] = min[axis].min(v);
            max[axis] = max[axis].max(v);
        }
    }
    if points.len() < 3 {
        return Ok(empty_at(ctx.canvas.x, ctx.canvas.y));
    }
    let bounds = clipped_bounds(min[0], min[1], max[0], max[1], ctx.canvas)?;
    let mut crossings: Vec<(f64, i32)> = Vec::new();
    rasterize(bounds, ctx.samples(), ctx, |sy, out| {
        crossings.clear();
        for (i, p) in points.iter().enumerate() {
            let q = points[i.checked_add(1).filter(|j| *j < points.len()).unwrap_or(0)];
            let dir = if p[1] <= sy && q[1] > sy {
                1
            } else if q[1] <= sy && p[1] > sy {
                -1
            } else {
                continue;
            };
            let t = (sy - p[1]) / (q[1] - p[1]);
            crossings.push((p[0] + t * (q[0] - p[0]), dir));
        }
        crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
        match rule {
            FillRule::EvenOdd => {
                for pair in crossings.chunks_exact(2) {
                    out.push((pair[0].0, pair[1].0));
                }
            }
            FillRule::NonZero => {
                let mut wind = 0i32;
                let mut start = 0.0;
                for &(x, d) in crossings.iter() {
                    let before = wind;
                    wind = wind.saturating_add(d);
                    if before == 0 && wind != 0 {
                        start = x;
                    } else if before != 0 && wind == 0 {
                        out.push((start, x));
                    }
                }
            }
        }
    })
}

/// Gaussian feather through [`coverage_math::feather_kernel`] (separable, zero outside the
/// plane). Bounds grow by [`coverage_math::feather_support`] and are clipped to the canvas.
pub fn feather(
    plane: &CoveragePlane,
    radius_px: f32,
    ctx: &SelectionContext,
) -> Result<CoveragePlane, SelectionError> {
    let kernel = coverage_math::feather_kernel(radius_px)?;
    let support = i64::from(coverage_math::feather_support(radius_px)?);
    if radius_px == 0.0 {
        return Ok(plane.clone());
    }
    let b = plane.bounds;
    let (_, ey) = b.end()?;
    let grown_x = Rect {
        x: b.x.checked_sub(support).ok_or(Error::Overflow)?,
        y: b.y,
        width: u32::try_from(
            i64::from(b.width)
                .checked_add(support.checked_mul(2).ok_or(Error::Overflow)?)
                .ok_or(Error::Overflow)?,
        )
        .map_err(|_| Error::Overflow)?,
        height: b.height,
    };
    let grown = Rect {
        x: grown_x.x,
        y: b.y.checked_sub(support).ok_or(Error::Overflow)?,
        width: grown_x.width,
        height: u32::try_from(
            i64::from(b.height)
                .checked_add(support.checked_mul(2).ok_or(Error::Overflow)?)
                .ok_or(Error::Overflow)?,
        )
        .map_err(|_| Error::Overflow)?,
    };
    let out_bounds = grown.intersect(ctx.canvas)?.unwrap_or(Rect {
        x: ctx.canvas.x,
        y: ctx.canvas.y,
        width: 0,
        height: 0,
    });
    let hx = Rect {
        x: out_bounds.x,
        y: b.y,
        width: out_bounds.width,
        height: b.height,
    };
    // Stage-1 buffer is f64 (8 bytes per sample): budget it as four times the u16 plane.
    let stage_bytes = hx.area().checked_mul(8).ok_or(Error::Overflow)?;
    if stage_bytes > ctx.limits.max_bytes.saturating_mul(4) {
        return Err(Error::BudgetExceeded.into());
    }
    let stage = check_area(hx, &ctx.limits)?;
    check_area(out_bounds, &ctx.limits)?;
    let taps = |i: usize| f64::from(kernel[i]);
    // Horizontal pass.
    let mut mid = Vec::with_capacity(stage);
    let (hex, _) = hx.end()?;
    for y in b.y..ey {
        for x in hx.x..hex {
            let mut acc = 0.0f64;
            for (i, _) in kernel.iter().enumerate() {
                let off = i64::try_from(i).map_err(|_| Error::Overflow)?.checked_sub(support).ok_or(Error::Overflow)?;
                let sx = x.checked_add(off).ok_or(Error::Overflow)?;
                acc += taps(i) * f64::from(plane.at(sx, y));
            }
            mid.push(acc);
        }
    }
    // Vertical pass.
    let (oex, oey) = out_bounds.end()?;
    let mut data = Vec::with_capacity(check_area(out_bounds, &ctx.limits)?);
    let mw = hx.width as usize;
    for y in out_bounds.y..oey {
        for x in out_bounds.x..oex {
            let col = usize::try_from(x.checked_sub(hx.x).ok_or(Error::Overflow)?)
                .map_err(|_| Error::Overflow)?;
            let mut acc = 0.0f64;
            for (i, _) in kernel.iter().enumerate() {
                let off = i64::try_from(i).map_err(|_| Error::Overflow)?.checked_sub(support).ok_or(Error::Overflow)?;
                let sy = y.checked_add(off).ok_or(Error::Overflow)?;
                let Some(row) = sy.checked_sub(b.y).and_then(|r| usize::try_from(r).ok()) else {
                    continue;
                };
                if row >= b.height as usize {
                    continue;
                }
                let idx = row
                    .checked_mul(mw)
                    .and_then(|v| v.checked_add(col))
                    .ok_or(Error::Overflow)?;
                acc += taps(i) * mid.get(idx).copied().unwrap_or(0.0);
            }
            data.push(acc.round().clamp(0.0, f64::from(FULL)) as u16);
        }
    }
    Ok(CoveragePlane {
        bounds: out_bounds,
        data,
    })
}

/// Resolved colour view for [`colour_range`]: tight `rgb_f32le` (12 bytes per pixel) and tight
/// straight `f32le` alpha (4 bytes per pixel) over `bounds`.
#[derive(Clone, Copy, Debug)]
pub struct ColourView<'a> {
    pub bounds: Rect,
    pub colour: &'a [u8],
    pub alpha: &'a [u8],
}

fn f32_at(bytes: &[u8], index: usize) -> Option<f32> {
    let at = index.checked_mul(4)?;
    let end = at.checked_add(4)?;
    Some(f32::from_le_bytes(bytes.get(at..end)?.try_into().ok()?))
}

/// Colour-range select: coverage `clamp(1 - d / fuzziness, 0, 1)` with `d` the Euclidean distance
/// between the pixel and `sample` in the view's encoded RGB; alpha 0 pixels get 0. Fuzziness 0
/// selects exact matches only. Studio reading, class Approximate (D5).
pub fn colour_range(
    view: &ColourView<'_>,
    sample: [f32; 3],
    fuzziness: f32,
    ctx: &SelectionContext,
) -> Result<CoveragePlane, SelectionError> {
    if !fuzziness.is_finite() || sample.iter().any(|v| !v.is_finite()) {
        return Err(Error::Nonfinite.into());
    }
    if fuzziness < 0.0 {
        return Err(Error::InvalidInput.into());
    }
    let pixels = usize::try_from(view.bounds.area()).map_err(|_| Error::Overflow)?;
    if pixels.checked_mul(12) != Some(view.colour.len()) || pixels.checked_mul(4) != Some(view.alpha.len())
    {
        return Err(Error::MalformedStride.into());
    }
    let bounds = view.bounds.intersect(ctx.canvas)?.unwrap_or(Rect {
        x: ctx.canvas.x,
        y: ctx.canvas.y,
        width: 0,
        height: 0,
    });
    let n = check_area(bounds, &ctx.limits)?;
    let (ex, ey) = bounds.end()?;
    let mut data = Vec::with_capacity(n);
    for y in bounds.y..ey {
        for x in bounds.x..ex {
            let dx = usize::try_from(x.checked_sub(view.bounds.x).ok_or(Error::Overflow)?)
                .map_err(|_| Error::Overflow)?;
            let dy = usize::try_from(y.checked_sub(view.bounds.y).ok_or(Error::Overflow)?)
                .map_err(|_| Error::Overflow)?;
            let px = dy
                .checked_mul(view.bounds.width as usize)
                .and_then(|v| v.checked_add(dx))
                .ok_or(Error::Overflow)?;
            let a = f32_at(view.alpha, px).ok_or(Error::MalformedStride)?;
            if !a.is_finite() {
                return Err(Error::Nonfinite.into());
            }
            let mut d2 = 0.0f64;
            for (c, s) in sample.iter().enumerate() {
                let i = px
                    .checked_mul(3)
                    .and_then(|v| v.checked_add(c))
                    .ok_or(Error::Overflow)?;
                let v = f32_at(view.colour, i).ok_or(Error::MalformedStride)?;
                if !v.is_finite() {
                    return Err(Error::Nonfinite.into());
                }
                let diff = f64::from(v) - f64::from(*s);
                d2 += diff * diff;
            }
            let cov = if a <= 0.0 {
                0.0
            } else if fuzziness == 0.0 {
                if d2 == 0.0 { 1.0 } else { 0.0 }
            } else {
                (1.0 - d2.sqrt() / f64::from(fuzziness)).clamp(0.0, 1.0)
            };
            data.push((cov * f64::from(FULL)).round() as u16);
        }
    }
    Ok(CoveragePlane { bounds, data })
}

/// Selection surface description (Studio readings, parity NOT_PROVEN).
pub const DESCRIPTOR: &str = r#"{"module":"STUDIO-MODULE-PIGMENT","surface":"selection","version":1,"plane":"CoveragePlane{bounds, u16 coverage/65535}; to_mask_bytes emits coverage_u8|coverage_u16le accepted by Mask::validate and MaskInput","combine":"closed set replace=1 extend=2 diminish=3 intersect=4 (STU-RAS-118); extend a+b-ab, diminish a(1-b), intersect ab, 16-bit fixed point round-half-up; Studio reading, Approximate, D5","producers":"rect, ellipse, polygon(FillRule EvenOdd default|NonZero), colour_range(Euclidean encoded RGB, fuzziness falloff), invert, feather (coverage_math::feather_kernel, separable)","anti_alias":"true = 4x4 pixel-centred samples (render-cpu rule), false = centre sample","limits":{"max_bytes":16777216,"max_points":1000000},"parity":"NOT_PROVEN (Photoshop combine maths, fill rule, fuzziness, sample count unverified)","pending":["flood select","modify ops STU-RAS-120","vector membership","StudioSelectionSet record"]}"#;
