//! Deterministic glyph rasterisation (STU-TYP-127/131). After one f64 transform per outline point
//! (IEEE basic operations only, no FMA, no SIMD), every step is integer: 24.8 fixed-point points,
//! integer curve flattening (tolerance 1/16 px, at most 256 steps per curve), exact signed-area
//! accumulation per cell, non-zero winding with |winding| clamped to full coverage, unhinted.
//! The plane is row-major `coverage_u8` with stride = width (pigment `Mask` byte layout).
#![deny(clippy::arithmetic_side_effects)]
use crate::{Error, GlyphError, GlyphOutline, OutlineCommand, Run};
use hsk_studio_accord::CancellationToken;

/// Bump when rasteriser output bytes can change; cached planes keyed by it are invalidated.
pub const RASTER_VERSION: &str = "hsk-type-raster-1";

/// Subpixel units per pixel (24.8 fixed point).
const ONE: i64 = 256;
/// Accumulator value of one fully covered pixel: dy (256) times doubled x weight (512).
const FULL: i64 = 131_072;
const HALF_FULL: i64 = 65_536;
/// Doubled x weight of a full pixel width.
const TWO_ONE: i64 = 512;
/// Flattening tolerance 1/16 px in 24.8 units.
const TOLERANCE_X4: i64 = 64;
const MAX_CURVE_STEPS: i64 = 256;
/// Largest absolute pixel coordinate admitted (2^22 px); keeps 24.8 values within 2^30.
const MAX_ABS_PX: f64 = 4_194_304.0;

#[derive(Clone, Copy, Debug)]
pub struct RasterLimits {
    pub max_width: u32,
    pub max_height: u32,
    /// Plane pixels (aggregate over the run), checked before allocation.
    pub max_pixels: u64,
    /// Flattened line segments over the whole call.
    pub max_segments: u64,
    pub max_work_units: u64,
}
impl Default for RasterLimits {
    fn default() -> Self {
        Self {
            max_width: 16_384,
            max_height: 16_384,
            max_pixels: 64 << 20,
            max_segments: 1 << 20,
            max_work_units: 1 << 30,
        }
    }
}

/// Coverage plane in device pixels: top-left `(x, y)`, row-major, stride = width, 0..=255.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlyphCoverage {
    x: i64,
    y: i64,
    width: u32,
    height: u32,
    data: Vec<u8>,
}
impl GlyphCoverage {
    pub fn x(&self) -> i64 {
        self.x
    }
    pub fn y(&self) -> i64 {
        self.y
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn stride(&self) -> u32 {
        self.width
    }
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

/// Rasterises one outline: font units scaled by `px_per_unit`, origin at `origin_px` (baseline,
/// y down in the raster). Fractional origins are honoured (subpixel positioning).
pub fn rasterize_glyph(
    outline: &GlyphOutline,
    origin_px: [f64; 2],
    px_per_unit: f64,
    limits: &RasterLimits,
    cancel: &CancellationToken,
) -> Result<GlyphCoverage, GlyphError> {
    if !(px_per_unit.is_finite() && px_per_unit > 0.0) {
        return Err(Error::InvalidInput.into());
    }
    finite_origin(origin_px)?;
    raster(&[(outline, origin_px)], px_per_unit, limits, cancel)
}

/// Rasterises a shaped run: pen from the run's advances/offsets (pt -> px by `px_per_pt`),
/// scale = `font_size_pt * px_per_pt / units_per_em`, glyph coverages summed with saturation.
/// `outlines` must come from the run's resolved font (content hash and units per em checked).
pub fn rasterize_run(
    run: &Run<'_>,
    outlines: &[GlyphOutline],
    origin_px: [f64; 2],
    px_per_pt: f64,
    limits: &RasterLimits,
    cancel: &CancellationToken,
) -> Result<GlyphCoverage, GlyphError> {
    cancel.check().map_err(|_| Error::Canceled)?;
    if !(px_per_pt.is_finite() && px_per_pt > 0.0) {
        return Err(Error::InvalidInput.into());
    }
    finite_origin(origin_px)?;
    if run.axes().iter().any(|axis| !axis.defaulted) {
        return Err(GlyphError::VariableOutlineUnsupported);
    }
    let units_per_em = run.units_per_em();
    let size = run.font_size_pt();
    if units_per_em == 0 || !(size.is_finite() && size > 0.0) {
        return Err(Error::InvalidInput.into());
    }
    let hash = run.resolved().content_hash;
    for outline in outlines {
        if outline.font_hash() != hash {
            return Err(Error::HashMismatch.into());
        }
        if u32::from(outline.units_per_em()) != units_per_em {
            return Err(Error::InvalidInput.into());
        }
    }
    let scale = size * px_per_pt / f64::from(units_per_em);
    if !(scale.is_finite() && scale > 0.0) {
        return Err(GlyphError::RasterOverLimit);
    }
    let glyphs = run.glyphs();
    let mut index: Vec<(u32, usize)> = Vec::with_capacity(outlines.len());
    for (i, outline) in outlines.iter().enumerate() {
        index.push((outline.glyph_id(), i));
    }
    index.sort_unstable();
    let mut placements = Vec::with_capacity(glyphs.len());
    let (mut pen_x, mut pen_y) = (0.0f64, 0.0f64);
    for glyph in glyphs {
        let at = index
            .binary_search_by(|(id, _)| id.cmp(&glyph.glyph_id))
            .map_err(|_| GlyphError::GlyphOutlineUnavailable)?;
        let outline = &outlines[index[at].1];
        let x = origin_px[0] + pen_x + glyph.x_offset_pt * px_per_pt;
        let y = origin_px[1] - (pen_y + glyph.y_offset_pt * px_per_pt);
        if !(x.is_finite() && y.is_finite()) || x.abs() > MAX_ABS_PX || y.abs() > MAX_ABS_PX {
            return Err(GlyphError::RasterOverLimit);
        }
        placements.push((outline, [x, y]));
        pen_x += glyph.x_advance_pt * px_per_pt;
        pen_y += glyph.y_advance_pt * px_per_pt;
    }
    raster(&placements, scale, limits, cancel)
}

fn finite_origin(origin_px: [f64; 2]) -> Result<(), GlyphError> {
    if origin_px.iter().all(|v| v.is_finite()) {
        Ok(())
    } else {
        Err(Error::InvalidInput.into())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct P {
    x: i64,
    y: i64,
}
#[derive(Clone, Copy, Debug)]
struct Line {
    a: P,
    b: P,
}

fn over() -> GlyphError {
    GlyphError::RasterOverLimit
}
fn add(a: i64, b: i64) -> Result<i64, GlyphError> {
    a.checked_add(b).ok_or_else(over)
}
fn sub(a: i64, b: i64) -> Result<i64, GlyphError> {
    a.checked_sub(b).ok_or_else(over)
}
fn mul(a: i64, b: i64) -> Result<i64, GlyphError> {
    a.checked_mul(b).ok_or_else(over)
}
fn floor_div(a: i64, b: i64) -> Result<i64, GlyphError> {
    a.checked_div_euclid(b).ok_or_else(over)
}
fn ceil_div(a: i64, b: i64) -> Result<i64, GlyphError> {
    floor_div(add(a, sub(b, 1)?)?, b)
}
/// Round-half-up integer division for `den != 0` (sign normalised first).
fn round_div(num: i128, den: i128) -> Result<i64, GlyphError> {
    let (num, den) = if den < 0 {
        (num.checked_neg().ok_or_else(over)?, den.checked_neg().ok_or_else(over)?)
    } else {
        (num, den)
    };
    if den == 0 {
        return Err(Error::InternalInvariant.into());
    }
    let twice = num.checked_mul(2).ok_or_else(over)?;
    let q = twice
        .checked_add(den)
        .ok_or_else(over)?
        .checked_div_euclid(den.checked_mul(2).ok_or_else(over)?)
        .ok_or_else(over)?;
    i64::try_from(q).map_err(|_| over())
}
fn ceil_sqrt(v: i64) -> i64 {
    let v = v.max(0).unsigned_abs();
    let s = v.isqrt();
    let s = if s.checked_mul(s).is_some_and(|sq| sq < v) {
        s.saturating_add(1)
    } else {
        s
    };
    i64::try_from(s).unwrap_or(i64::MAX)
}

struct Work {
    used: u64,
    limit: u64,
}
impl Work {
    fn step(&mut self, units: u64) -> Result<(), GlyphError> {
        self.used = self.used.checked_add(units).ok_or(Error::Overflow)?;
        if self.used > self.limit {
            return Err(Error::Budget.into());
        }
        Ok(())
    }
}

struct Flattener<'a> {
    lines: &'a mut Vec<Line>,
    cap: u64,
}
impl Flattener<'_> {
    fn line(&mut self, a: P, b: P) -> Result<(), GlyphError> {
        if a.y == b.y {
            return Ok(());
        }
        if self.lines.len() as u64 >= self.cap {
            return Err(GlyphError::RasterOverLimit);
        }
        self.lines.push(Line { a, b });
        Ok(())
    }
    fn steps(dd: i64) -> Result<i64, GlyphError> {
        Ok(ceil_sqrt(ceil_div(dd, TOLERANCE_X4)?).clamp(1, MAX_CURVE_STEPS))
    }
    fn quad(&mut self, p0: P, p1: P, p2: P) -> Result<P, GlyphError> {
        let ddx = add(sub(p0.x, mul(p1.x, 2)?)?, p2.x)?;
        let ddy = add(sub(p0.y, mul(p1.y, 2)?)?, p2.y)?;
        // Chord error of n uniform steps <= |p0 - 2p1 + p2| / (4 n^2) (L1 bounds Euclidean).
        let n = Self::steps(add(ddx.checked_abs().ok_or_else(over)?, ddy.checked_abs().ok_or_else(over)?)?)?;
        let n2 = i128::from(mul(n, n)?);
        let mut prev = p0;
        for i in 1..=n {
            let j = i128::from(sub(n, i)?);
            let i = i128::from(i);
            let w0 = j.checked_mul(j).ok_or_else(over)?;
            let w1 = i.checked_mul(j).and_then(|v| v.checked_mul(2)).ok_or_else(over)?;
            let w2 = i.checked_mul(i).ok_or_else(over)?;
            let eval = |a: i64, b: i64, c: i64| -> Result<i64, GlyphError> {
                let s = i128::from(a)
                    .checked_mul(w0)
                    .and_then(|v| v.checked_add(i128::from(b).checked_mul(w1)?))
                    .and_then(|v| v.checked_add(i128::from(c).checked_mul(w2)?))
                    .ok_or_else(over)?;
                round_div(s, n2)
            };
            let next = P {
                x: eval(p0.x, p1.x, p2.x)?,
                y: eval(p0.y, p1.y, p2.y)?,
            };
            self.line(prev, next)?;
            prev = next;
        }
        Ok(p2)
    }
    fn cubic(&mut self, p0: P, p1: P, p2: P, p3: P) -> Result<P, GlyphError> {
        let l1 = |a: P, b: P, c: P| -> Result<i64, GlyphError> {
            let x = add(sub(a.x, mul(b.x, 2)?)?, c.x)?;
            let y = add(sub(a.y, mul(b.y, 2)?)?, c.y)?;
            add(x.checked_abs().ok_or_else(over)?, y.checked_abs().ok_or_else(over)?)
        };
        // Chord error of n uniform steps <= 3 * max second difference / (4 n^2).
        let dd = l1(p0, p1, p2)?.max(l1(p1, p2, p3)?);
        let n = Self::steps(mul(dd, 3)?)?;
        let n3 = i128::from(mul(mul(n, n)?, n)?);
        let mut prev = p0;
        for i in 1..=n {
            let j = i128::from(sub(n, i)?);
            let i = i128::from(i);
            let w0 = j.checked_mul(j).and_then(|v| v.checked_mul(j)).ok_or_else(over)?;
            let w1 = j
                .checked_mul(j)
                .and_then(|v| v.checked_mul(i))
                .and_then(|v| v.checked_mul(3))
                .ok_or_else(over)?;
            let w2 = j
                .checked_mul(i)
                .and_then(|v| v.checked_mul(i))
                .and_then(|v| v.checked_mul(3))
                .ok_or_else(over)?;
            let w3 = i.checked_mul(i).and_then(|v| v.checked_mul(i)).ok_or_else(over)?;
            let eval = |a: i64, b: i64, c: i64, d: i64| -> Result<i64, GlyphError> {
                let s = i128::from(a)
                    .checked_mul(w0)
                    .and_then(|v| v.checked_add(i128::from(b).checked_mul(w1)?))
                    .and_then(|v| v.checked_add(i128::from(c).checked_mul(w2)?))
                    .and_then(|v| v.checked_add(i128::from(d).checked_mul(w3)?))
                    .ok_or_else(over)?;
                round_div(s, n3)
            };
            let next = P {
                x: eval(p0.x, p1.x, p2.x, p3.x)?,
                y: eval(p0.y, p1.y, p2.y, p3.y)?,
            };
            self.line(prev, next)?;
            prev = next;
        }
        Ok(p3)
    }
}

/// font units -> 24.8 device units: one f64 multiply and add per axis, rounded once.
fn point(p: [f32; 2], origin: [f64; 2], scale: f64) -> Result<P, GlyphError> {
    if !(p[0].is_finite() && p[1].is_finite()) {
        return Err(Error::InvalidInput.into());
    }
    let x = origin[0] + f64::from(p[0]) * scale;
    let y = origin[1] - f64::from(p[1]) * scale;
    if !(x.is_finite() && y.is_finite()) || x.abs() > MAX_ABS_PX || y.abs() > MAX_ABS_PX {
        return Err(GlyphError::RasterOverLimit);
    }
    // |value| <= 2^30, exactly representable; the cast cannot saturate.
    Ok(P {
        x: (x * 256.0).round() as i64,
        y: (y * 256.0).round() as i64,
    })
}

fn flatten(
    outline: &GlyphOutline,
    origin: [f64; 2],
    scale: f64,
    out: &mut Flattener<'_>,
    work: &mut Work,
) -> Result<(), GlyphError> {
    let mut first: Option<P> = None;
    let mut cur: Option<P> = None;
    for command in outline.commands() {
        work.step(1)?;
        match *command {
            OutlineCommand::MoveTo(p) => {
                if let (Some(c), Some(f)) = (cur, first) {
                    out.line(c, f)?;
                }
                let p = point(p, origin, scale)?;
                first = Some(p);
                cur = Some(p);
            }
            OutlineCommand::LineTo(p) => {
                let c = cur.ok_or(Error::CorruptFont)?;
                let p = point(p, origin, scale)?;
                out.line(c, p)?;
                cur = Some(p);
            }
            OutlineCommand::QuadTo(c1, p) => {
                let c = cur.ok_or(Error::CorruptFont)?;
                cur = Some(out.quad(c, point(c1, origin, scale)?, point(p, origin, scale)?)?);
            }
            OutlineCommand::CurveTo(c1, c2, p) => {
                let c = cur.ok_or(Error::CorruptFont)?;
                cur = Some(out.cubic(
                    c,
                    point(c1, origin, scale)?,
                    point(c2, origin, scale)?,
                    point(p, origin, scale)?,
                )?);
            }
            OutlineCommand::Close => {
                if let (Some(c), Some(f)) = (cur, first) {
                    out.line(c, f)?;
                }
                cur = first;
            }
        }
    }
    // Fill closes open contours implicitly.
    if let (Some(c), Some(f)) = (cur, first) {
        out.line(c, f)?;
    }
    Ok(())
}

fn bbox(lines: &[Line]) -> Option<(i64, i64, i64, i64)> {
    let mut it = lines.iter();
    let l = it.next()?;
    let mut b = (
        l.a.x.min(l.b.x),
        l.a.y.min(l.b.y),
        l.a.x.max(l.b.x),
        l.a.y.max(l.b.y),
    );
    for l in it {
        b.0 = b.0.min(l.a.x).min(l.b.x);
        b.1 = b.1.min(l.a.y).min(l.b.y);
        b.2 = b.2.max(l.a.x).max(l.b.x);
        b.3 = b.3.max(l.a.y).max(l.b.y);
    }
    Some(b)
}

struct Cells<'a> {
    acc: &'a mut [i64],
    stride: i64,
}
impl Cells<'_> {
    fn piece(&mut self, row: i64, col: i64, dy: i64, xmid2: i64) -> Result<(), GlyphError> {
        let at = add(mul(row, self.stride)?, col)?;
        let at = usize::try_from(at).map_err(|_| GlyphError::Type(Error::InternalInvariant))?;
        let left = mul(dy, sub(TWO_ONE, xmid2)?)?;
        let right = mul(dy, xmid2)?;
        let next = at.checked_add(1).ok_or_else(over)?;
        let cell = self.acc.get_mut(at).ok_or(Error::InternalInvariant)?;
        *cell = add(*cell, left)?;
        let cell = self.acc.get_mut(next).ok_or(Error::InternalInvariant)?;
        *cell = add(*cell, right)?;
        Ok(())
    }
    /// One row band from (xs, ys) to (xe, ye), ys < ye, split at pixel column boundaries.
    fn row(&mut self, row: i64, start: P, end: P, sign: i64, work: &mut Work) -> Result<(), GlyphError> {
        let (xs, ys, xe, ye) = (start.x, start.y, end.x, end.y);
        let lo = xs.min(xe);
        let hi = xs.max(xe);
        let col_lo = floor_div(lo, ONE)?;
        if xs == xe || col_lo == floor_div(sub(hi, 1)?, ONE)? {
            work.step(1)?;
            let xmid2 = sub(add(xs, xe)?, mul(mul(col_lo, ONE)?, 2)?)?;
            return self.piece(row, col_lo, mul(sign, sub(ye, ys)?)?, xmid2);
        }
        let col_hi = floor_div(sub(hi, 1)?, ONE)?;
        let (mut px, mut py) = (xs, ys);
        let mut k = if xs < xe { add(col_lo, 1)? } else { col_hi };
        loop {
            work.step(1)?;
            let bx = mul(k, ONE)?;
            let by = add(
                ys,
                round_div(
                    i128::from(sub(ye, ys)?)
                        .checked_mul(i128::from(sub(bx, xs)?))
                        .ok_or_else(over)?,
                    i128::from(sub(xe, xs)?),
                )?,
            )?;
            let col = floor_div(px.min(bx), ONE)?;
            let xmid2 = sub(add(px, bx)?, mul(mul(col, ONE)?, 2)?)?;
            self.piece(row, col, mul(sign, sub(by, py)?)?, xmid2)?;
            px = bx;
            py = by;
            if xs < xe {
                if k == col_hi {
                    break;
                }
                k = add(k, 1)?;
            } else {
                if k == add(col_lo, 1)? {
                    break;
                }
                k = sub(k, 1)?;
            }
        }
        let col = floor_div(px.min(xe), ONE)?;
        let xmid2 = sub(add(px, xe)?, mul(mul(col, ONE)?, 2)?)?;
        self.piece(row, col, mul(sign, sub(ye, py)?)?, xmid2)
    }
    fn line(&mut self, l: Line, work: &mut Work, cancel: &CancellationToken) -> Result<(), GlyphError> {
        let (sign, a, b) = if l.a.y < l.b.y { (1, l.a, l.b) } else { (-1, l.b, l.a) };
        let dy = sub(b.y, a.y)?;
        let dx = sub(b.x, a.x)?;
        let first = floor_div(a.y, ONE)?;
        let last = floor_div(sub(b.y, 1)?, ONE)?;
        let x_at = |y: i64| -> Result<i64, GlyphError> {
            add(
                a.x,
                round_div(
                    i128::from(dx)
                        .checked_mul(i128::from(sub(y, a.y)?))
                        .ok_or_else(over)?,
                    i128::from(dy),
                )?,
            )
        };
        let mut row = first;
        while row <= last {
            cancel.check().map_err(|_| Error::Canceled)?;
            let top = a.y.max(mul(row, ONE)?);
            let bottom = b.y.min(mul(add(row, 1)?, ONE)?);
            if top < bottom {
                let start = P { x: x_at(top)?, y: top };
                let end = P { x: x_at(bottom)?, y: bottom };
                self.row(row, start, end, sign, work)?;
            }
            row = add(row, 1)?;
        }
        Ok(())
    }
}

fn raster(
    placements: &[(&GlyphOutline, [f64; 2])],
    scale: f64,
    limits: &RasterLimits,
    cancel: &CancellationToken,
) -> Result<GlyphCoverage, GlyphError> {
    cancel.check().map_err(|_| Error::Canceled)?;
    if limits.max_width == 0
        || limits.max_height == 0
        || limits.max_pixels == 0
        || limits.max_segments == 0
        || limits.max_work_units == 0
    {
        return Err(Error::InvalidInput.into());
    }
    let mut work = Work {
        used: 0,
        limit: limits.max_work_units,
    };
    let mut lines: Vec<Line> = Vec::new();
    let mut ranges = Vec::with_capacity(placements.len());
    for (outline, origin) in placements {
        cancel.check().map_err(|_| Error::Canceled)?;
        let start = lines.len();
        let mut flattener = Flattener {
            lines: &mut lines,
            cap: limits.max_segments,
        };
        flatten(outline, *origin, scale, &mut flattener, &mut work)?;
        ranges.push(start..lines.len());
    }
    let Some((min_x, min_y, max_x, max_y)) = bbox(&lines) else {
        return Ok(GlyphCoverage {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            data: Vec::new(),
        });
    };
    let x0 = floor_div(min_x, ONE)?;
    let y0 = floor_div(min_y, ONE)?;
    let width = sub(ceil_div(max_x, ONE)?, x0)?;
    let height = sub(ceil_div(max_y, ONE)?, y0)?;
    let width_u = u32::try_from(width).map_err(|_| over())?;
    let height_u = u32::try_from(height).map_err(|_| over())?;
    let pixels = u64::from(width_u)
        .checked_mul(u64::from(height_u))
        .ok_or_else(over)?;
    if width_u > limits.max_width || height_u > limits.max_height || pixels > limits.max_pixels {
        return Err(GlyphError::RasterOverLimit);
    }
    let mut data = vec![0u8; usize::try_from(pixels).map_err(|_| over())?];
    let mut acc: Vec<i64> = Vec::new();
    for range in ranges {
        let glyph = lines.get(range).ok_or(Error::InternalInvariant)?;
        let Some((gx_min, gy_min, gx_max, gy_max)) = bbox(glyph) else {
            continue;
        };
        let gx0 = floor_div(gx_min, ONE)?;
        let gy0 = floor_div(gy_min, ONE)?;
        let gw = sub(ceil_div(gx_max, ONE)?, gx0)?;
        let gh = sub(ceil_div(gy_max, ONE)?, gy0)?;
        let stride = add(gw, 2)?;
        let cells = usize::try_from(mul(stride, gh)?).map_err(|_| over())?;
        work.step(cells as u64)?;
        acc.clear();
        acc.resize(cells, 0);
        let mut grid = Cells {
            acc: &mut acc,
            stride,
        };
        let (ox, oy) = (mul(gx0, ONE)?, mul(gy0, ONE)?);
        for l in glyph {
            let local = Line {
                a: P {
                    x: sub(l.a.x, ox)?,
                    y: sub(l.a.y, oy)?,
                },
                b: P {
                    x: sub(l.b.x, ox)?,
                    y: sub(l.b.y, oy)?,
                },
            };
            grid.line(local, &mut work, cancel)?;
        }
        for row in 0..gh {
            cancel.check().map_err(|_| Error::Canceled)?;
            let mut sum: i64 = 0;
            let plane_row = sub(add(gy0, row)?, y0)?;
            for col in 0..gw {
                let at = usize::try_from(add(mul(row, stride)?, col)?).map_err(|_| over())?;
                sum = add(sum, *acc.get(at).ok_or(Error::InternalInvariant)?)?;
                let cover = sum.checked_abs().ok_or_else(over)?.min(FULL);
                let value = floor_div(add(mul(cover, 255)?, HALF_FULL)?, FULL)?;
                let value = u8::try_from(value).map_err(|_| GlyphError::Type(Error::InternalInvariant))?;
                if value == 0 {
                    continue;
                }
                let plane_col = sub(add(gx0, col)?, x0)?;
                let index = usize::try_from(add(mul(plane_row, width)?, plane_col)?)
                    .map_err(|_| over())?;
                let pixel = data.get_mut(index).ok_or(Error::InternalInvariant)?;
                *pixel = pixel.saturating_add(value);
            }
        }
    }
    Ok(GlyphCoverage {
        x: x0,
        y: y0,
        width: width_u,
        height: height_u,
        data,
    })
}
