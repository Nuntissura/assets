//! Coverage rasterisation and paint evaluation (lane SMB-NIB-VECTOR-RENDER slice 3; STU-VEC-179,
//! STU-VEC-134..138). Produces render-ready planes; blending them into a document is the compositor's
//! job (composite/render lanes lower these planes; nib draws no layers).
//!
//! Coverage: the path is first resolved under its own fill rule into simple, non-overlapping polygons
//! ([`crate::boolean::normalise_path`], so non-zero and even-odd are handled the same way), then
//! every polygon edge is accumulated as exact signed area into a per-row cell buffer and prefix-summed
//! (the signed-area accumulation of font-rs / stb_truetype, written here from the published idea, no
//! code copied). For non-overlapping polygons this is the exact area of the pixel square covered by
//! the polygon (analytic anti-aliasing, no supersampling); the result is `round(255 * min(|area|,
//! 1))`. Only `+ - * /`, `floor`, `round` and `sqrt` are used, in a fixed order, so planes are
//! byte-identical on every host (STU-VEC-179). Error: flatten tolerance + grid quantum (receipt).
//!
//! Paint: solid, linear and radial (two-circle conical, the canvas/SVG 2 construction) gradients and
//! tiled raster patterns are evaluated at pixel centres and multiplied by coverage into a
//! premultiplied RGBA `f32` plane. Gradient stops interpolate straight (non-premultiplied) components
//! (`classic`, STU-VEC-137); a stop midpoint (STU-VEC-136, 13..=87 percent) skews the ramp piecewise
//! linearly. Adobe's exact midpoint curve, perceptual interpolation and dither are UNVERIFIED (D5):
//! `perceptual` and `dither = true` are refused, not approximated.
//!
//! Hostile sizes (V4-01/02): the pixel count, the cell buffer and the edge work are all bounded
//! before allocation; gradients, stops, transforms and tiles are validated first.
#![deny(clippy::arithmetic_side_effects)]
use crate::boolean::{BooleanOptions, GeometryReceipt, normalise_path};
use crate::owned_path::{OwnedPath, PathElement, PathPoint};
use crate::path_contract::Error;
use sha2::{Digest, Sha256};

/// Default pixel budget of one plane (64 Mpx).
pub const DEFAULT_MAX_PIXELS: u64 = 1 << 26;
/// Most stops per gradient (same ceiling as Folio's MAX_GRADIENT_STOPS).
pub const MAX_STOPS: usize = 1024;
/// Largest pattern tile edge in pixels.
pub const MAX_TILE_EDGE: u32 = 4096;

/// Integer pixel window of a plane; path coordinates are pixels, y down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelFrame {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}
impl PixelFrame {
    fn pixels(&self, max: u64) -> Result<usize, Error> {
        let n = u64::from(self.width)
            .checked_mul(u64::from(self.height))
            .ok_or(Error::BudgetExceeded)?;
        if n > max {
            return Err(Error::BudgetExceeded);
        }
        usize::try_from(n).map_err(|_| Error::BudgetExceeded)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterOptions {
    pub geometry: BooleanOptions,
    pub max_pixels: u64,
    /// Bound on edge work units (rows plus columns visited per edge).
    pub max_work: u64,
}
impl Default for RasterOptions {
    fn default() -> Self {
        Self {
            geometry: BooleanOptions::default(),
            max_pixels: DEFAULT_MAX_PIXELS,
            max_work: 1 << 28,
        }
    }
}

/// 8-bit coverage, row-major, `frame.width * frame.height` bytes; 255 = fully covered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoveragePlane {
    pub frame: PixelFrame,
    pub data: Vec<u8>,
}

impl CoveragePlane {
    pub fn get(&self, x: u32, y: u32) -> Option<u8> {
        if x >= self.frame.width || y >= self.frame.height {
            return None;
        }
        let i = (y as usize)
            .checked_mul(self.frame.width as usize)?
            .checked_add(x as usize)?;
        self.data.get(i).copied()
    }
    /// SHA-256 over the frame (little-endian x, y, width, height) and the bytes.
    pub fn digest(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(self.frame.x.to_le_bytes());
        h.update(self.frame.y.to_le_bytes());
        h.update(self.frame.width.to_le_bytes());
        h.update(self.frame.height.to_le_bytes());
        h.update(&self.data);
        h.finalize().into()
    }
    /// Coverage product (`round(a * b / 255)`) of two planes on the same frame: a clipping mask
    /// applied at coverage level. Geometry-exact clipping is `boolean(Intersect)` before rasterising.
    pub fn intersect(&self, other: &CoveragePlane) -> Result<CoveragePlane, Error> {
        if self.frame != other.frame || self.data.len() != other.data.len() {
            return Err(Error::InvalidRequest);
        }
        let data = self
            .data
            .iter()
            .zip(&other.data)
            .map(|(a, b)| {
                let p = u32::from(*a).saturating_mul(u32::from(*b));
                // round(p / 255) for p <= 65025, exact integer form.
                u8::try_from(p.saturating_add(127) / 255).unwrap_or(u8::MAX)
            })
            .collect();
        Ok(CoveragePlane {
            frame: self.frame,
            data,
        })
    }
}

/// Coverage of `path` (its own fill rule) over `frame`.
pub fn rasterize(
    path: &OwnedPath,
    frame: PixelFrame,
    options: &RasterOptions,
) -> Result<(CoveragePlane, Option<GeometryReceipt>), Error> {
    let pixels = frame.pixels(options.max_pixels)?;
    let normal = match normalise_path(path, &options.geometry) {
        Ok(r) => Some(r),
        Err(Error::EmptyPathfinderResult) => None,
        Err(e) => return Err(e),
    };
    let (w, h) = (frame.width as usize, frame.height as usize);
    let Some(normal) = normal else {
        return Ok((
            CoveragePlane {
                frame,
                data: vec![0; pixels],
            },
            None,
        ));
    };
    let edges = edges_of(&normal.path, frame);
    // Work bound before the cell buffer exists: rows + clipped columns per edge.
    let mut work = 0u64;
    for e in &edges {
        let rows = (e.1.y.min(f64::from(frame.height)) - e.0.y.max(0.0)).max(0.0) + 2.0;
        let cols = (e.0.x.max(e.1.x) - e.0.x.min(e.1.x)).min(f64::from(frame.width)) + rows;
        work = work.saturating_add((rows + cols) as u64);
        if work > options.max_work {
            return Err(Error::BudgetExceeded);
        }
    }
    let stride = w.checked_add(1).ok_or(Error::BudgetExceeded)?;
    let cells = stride.checked_mul(h).ok_or(Error::BudgetExceeded)?;
    let mut acc = vec![0.0f64; cells];
    for &(a, b, dir) in &edges {
        accumulate(&mut acc, stride, w, h, a, b, dir);
    }
    let mut data = Vec::with_capacity(pixels);
    for row in acc.chunks(stride) {
        let mut sum = 0.0f64;
        for cell in row.iter().take(w) {
            sum += cell;
            let c = sum.abs().min(1.0);
            // In 0..=255 by the clamp.
            data.push((c * 255.0).round() as u8);
        }
    }
    Ok((CoveragePlane { frame, data }, Some(normal.receipt)))
}

/// Polygon edges in frame-local pixels, ordered top to bottom, with their winding direction.
fn edges_of(path: &OwnedPath, frame: PixelFrame) -> Vec<(PathPoint, PathPoint, f64)> {
    let (ox, oy) = (f64::from(frame.x), f64::from(frame.y));
    let local = |p: PathPoint| PathPoint::new(p.x - ox, p.y - oy);
    let mut out = Vec::new();
    let mut start = None;
    let mut last = None;
    let mut push = |a: PathPoint, b: PathPoint| {
        if a.y < b.y {
            out.push((a, b, 1.0));
        } else if a.y > b.y {
            out.push((b, a, -1.0));
        }
    };
    for e in &path.elements {
        match *e {
            PathElement::MoveTo(p) => {
                start = Some(local(p));
                last = start;
            }
            PathElement::LineTo(p) => {
                let q = local(p);
                if let Some(l) = last {
                    push(l, q);
                }
                last = Some(q);
            }
            PathElement::Close => {
                if let (Some(l), Some(s)) = (last, start) {
                    push(l, s);
                }
                last = start;
            }
            // Normalised paths are polygonal.
            PathElement::QuadTo(..) | PathElement::CubicTo(..) => {}
        }
    }
    out
}

/// Adds one edge (`a.y < b.y`) as signed area to the cell rows it crosses.
fn accumulate(acc: &mut [f64], stride: usize, w: usize, h: usize, a: PathPoint, b: PathPoint, dir: f64) {
    let hf = h as f64;
    if b.y <= 0.0 || a.y >= hf {
        return;
    }
    let dxdy = (b.x - a.x) / (b.y - a.y);
    let y0 = a.y.max(0.0);
    let y1 = b.y.min(hf);
    // Rows are in 0..h by the clip above.
    let first = y0.floor() as usize;
    let last = (y1.ceil() as usize).min(h);
    for row in first..last {
        let top = y0.max(row as f64);
        let bottom = y1.min(row as f64 + 1.0);
        if bottom <= top {
            continue;
        }
        let xt = a.x + (top - a.y) * dxdy;
        let xb = a.x + (bottom - a.y) * dxdy;
        let Some(start) = row.checked_mul(stride) else {
            return;
        };
        let Some(cells) = start.checked_add(stride).and_then(|end| acc.get_mut(start..end)) else {
            return;
        };
        span(cells, w, xt, xb, (bottom - top) * dir);
    }
}

/// Distributes signed height `dy` of a straight piece from `x0` to `x1` over the cells of one row:
/// cell `i` gets the area right of the piece inside the cell, cell `i + 1` the rest, so the row's
/// prefix sum is the covered area. Parts left of the row add their full height to cell 0; parts at or
/// right of `w` add nothing visible.
fn span(cells: &mut [f64], w: usize, x0: f64, x1: f64, dy: f64) {
    let wf = w as f64;
    let (lo, hi) = if x0 <= x1 { (x0, x1) } else { (x1, x0) };
    if hi <= 0.0 {
        add(cells, 0, dy);
        return;
    }
    if lo >= wf {
        return;
    }
    let width = hi - lo;
    if width == 0.0 {
        cell_piece(cells, lo, lo, dy);
        return;
    }
    // Part left of 0.
    if lo < 0.0 {
        add(cells, 0, dy * (0.0 - lo) / width);
    }
    let mut x = lo.max(0.0);
    let end = hi.min(wf);
    while x < end {
        let next = (x.floor() + 1.0).min(end);
        cell_piece(cells, x, next, dy * (next - x) / width);
        x = next;
    }
}

fn cell_piece(cells: &mut [f64], xa: f64, xb: f64, dy: f64) {
    let column = xa.min(xb).floor();
    let mid = (xa + xb) * 0.5 - column;
    // Columns are within 0..w here (callers clip), cells has w + 1 entries.
    let i = column.max(0.0) as usize;
    add(cells, i, dy * (1.0 - mid));
    add(cells, i.saturating_add(1), dy * mid);
}

fn add(cells: &mut [f64], i: usize, v: f64) {
    if let Some(c) = cells.get_mut(i) {
        *c += v;
    }
}

// ---------------------------------------------------------------------------------------------
// Paint

/// Straight (non-premultiplied) RGBA, each component in `0..=1`.
pub type Rgba = [f32; 4];

/// `[a, b, c, d, e, f]`: point = `(a x + c y + e, b x + d y + f)` (Folio/SVG order).
pub type Affine = [f64; 6];
pub const IDENTITY: Affine = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spread {
    Pad,
    Reflect,
    Repeat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interpolation {
    Classic,
    Perceptual,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GradientStop {
    /// Ramp position `0..=1`.
    pub offset: f64,
    pub colour: Rgba,
    /// Skew toward the next stop as a fraction (`0.13..=0.87`); `None` = 0.5.
    pub midpoint: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum GradientGeometry {
    Linear { x1: f64, y1: f64, x2: f64, y2: f64 },
    /// End circle `(cx, cy, r)`, focal (start) circle `(fx, fy, fr)`.
    Radial { cx: f64, cy: f64, r: f64, fx: f64, fy: f64, fr: f64 },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Gradient {
    pub geometry: GradientGeometry,
    /// Gradient space to pixel space.
    pub transform: Affine,
    pub spread: Spread,
    pub interpolation: Interpolation,
    pub dither: bool,
    pub stops: Vec<GradientStop>,
}

/// A raster pattern tile, premultiplied RGBA, row-major, repeated over the plane.
#[derive(Clone, Debug, PartialEq)]
pub struct PatternTile {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<Rgba>,
    /// Tile space (pixel units of the tile) to plane pixel space.
    pub transform: Affine,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Paint {
    Solid(Rgba),
    Gradient(Gradient),
    Pattern(PatternTile),
}

/// Premultiplied RGBA `f32`, row-major over `frame`.
#[derive(Clone, Debug, PartialEq)]
pub struct PaintedPlane {
    pub frame: PixelFrame,
    pub data: Vec<Rgba>,
}

/// Evaluates `paint` at every pixel centre and multiplies it by the coverage.
pub fn paint_coverage(coverage: &CoveragePlane, paint: &Paint) -> Result<PaintedPlane, Error> {
    let frame = coverage.frame;
    if coverage.data.len() != frame.pixels(u64::MAX)? {
        return Err(Error::InvalidRequest);
    }
    let shader = Shader::new(paint)?;
    let mut data = Vec::with_capacity(coverage.data.len());
    let (w, ox, oy) = (frame.width as usize, f64::from(frame.x), f64::from(frame.y));
    for (i, c) in coverage.data.iter().enumerate() {
        if *c == 0 {
            data.push([0.0; 4]);
            continue;
        }
        let (row, col) = (i.checked_div(w).unwrap_or(0), i.checked_rem(w).unwrap_or(0));
        let x = ox + col as f64 + 0.5;
        let y = oy + row as f64 + 0.5;
        let v = shader.eval(x, y);
        let k = f32::from(*c) / 255.0;
        data.push([v[0] * k, v[1] * k, v[2] * k, v[3] * k]);
    }
    Ok(PaintedPlane { frame, data })
}

enum Shader<'a> {
    Solid(Rgba),
    Linear {
        inverse: Affine,
        p: (f64, f64),
        d: (f64, f64),
        len2: f64,
        ramp: Ramp<'a>,
    },
    Radial {
        inverse: Affine,
        f: (f64, f64),
        fr: f64,
        d: (f64, f64),
        dr: f64,
        ramp: Ramp<'a>,
    },
    Pattern {
        inverse: Affine,
        tile: &'a PatternTile,
    },
}

struct Ramp<'a> {
    stops: &'a [GradientStop],
    spread: Spread,
}

fn finite_rgba(c: &Rgba) -> bool {
    c.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v))
}

fn invert(m: &Affine) -> Result<Affine, Error> {
    if m.iter().any(|v| !v.is_finite()) {
        return Err(Error::UnsupportedOptions);
    }
    let [a, b, c, d, e, f] = *m;
    let det = a * d - b * c;
    if !det.is_finite() || det.abs() < 1e-12 {
        return Err(Error::UnsupportedOptions);
    }
    let inv = [
        d / det,
        -b / det,
        -c / det,
        a / det,
        (c * f - d * e) / det,
        (b * e - a * f) / det,
    ];
    if inv.iter().all(|v| v.is_finite()) {
        Ok(inv)
    } else {
        Err(Error::UnsupportedOptions)
    }
}

fn apply(m: &Affine, x: f64, y: f64) -> (f64, f64) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

fn check_gradient(g: &Gradient) -> Result<(), Error> {
    if g.interpolation != Interpolation::Classic || g.dither {
        return Err(Error::UnsupportedOptions);
    }
    if g.stops.is_empty() || g.stops.len() > MAX_STOPS {
        return Err(Error::UnsupportedOptions);
    }
    let mut previous = 0.0;
    for s in &g.stops {
        if !s.offset.is_finite() || !(0.0..=1.0).contains(&s.offset) || s.offset < previous {
            return Err(Error::UnsupportedOptions);
        }
        previous = s.offset;
        if !finite_rgba(&s.colour) {
            return Err(Error::UnsupportedOptions);
        }
        if let Some(m) = s.midpoint
            && (!m.is_finite() || !(0.13..=0.87).contains(&m))
        {
            return Err(Error::UnsupportedOptions);
        }
    }
    let bounded = |v: f64| v.is_finite() && v.abs() <= crate::owned_path::COORDINATE_LIMIT;
    let ok = match g.geometry {
        GradientGeometry::Linear { x1, y1, x2, y2 } => [x1, y1, x2, y2].into_iter().all(bounded),
        GradientGeometry::Radial { cx, cy, r, fx, fy, fr } => {
            [cx, cy, r, fx, fy, fr].into_iter().all(bounded) && r >= 0.0 && fr >= 0.0
        }
    };
    if ok { Ok(()) } else { Err(Error::UnsupportedOptions) }
}

impl<'a> Shader<'a> {
    fn new(paint: &'a Paint) -> Result<Self, Error> {
        match paint {
            Paint::Solid(c) => {
                if !finite_rgba(c) {
                    return Err(Error::UnsupportedOptions);
                }
                Ok(Self::Solid(premultiply(*c)))
            }
            Paint::Gradient(g) => {
                check_gradient(g)?;
                let inverse = invert(&g.transform)?;
                let ramp = Ramp {
                    stops: &g.stops,
                    spread: g.spread,
                };
                Ok(match g.geometry {
                    GradientGeometry::Linear { x1, y1, x2, y2 } => {
                        let d = (x2 - x1, y2 - y1);
                        Self::Linear {
                            inverse,
                            p: (x1, y1),
                            d,
                            len2: d.0 * d.0 + d.1 * d.1,
                            ramp,
                        }
                    }
                    GradientGeometry::Radial { cx, cy, r, fx, fy, fr } => Self::Radial {
                        inverse,
                        f: (fx, fy),
                        fr,
                        d: (cx - fx, cy - fy),
                        dr: r - fr,
                        ramp,
                    },
                })
            }
            Paint::Pattern(tile) => {
                if tile.width == 0
                    || tile.height == 0
                    || tile.width > MAX_TILE_EDGE
                    || tile.height > MAX_TILE_EDGE
                {
                    return Err(Error::UnsupportedOptions);
                }
                let n = (tile.width as usize).saturating_mul(tile.height as usize);
                if tile.pixels.len() != n || !tile.pixels.iter().all(finite_rgba) {
                    return Err(Error::UnsupportedOptions);
                }
                if tile.pixels.iter().any(|p| p[0] > p[3] || p[1] > p[3] || p[2] > p[3]) {
                    return Err(Error::UnsupportedOptions); // not premultiplied
                }
                Ok(Self::Pattern {
                    inverse: invert(&tile.transform)?,
                    tile,
                })
            }
        }
    }

    /// Premultiplied colour at pixel-space point `(x, y)`.
    fn eval(&self, x: f64, y: f64) -> Rgba {
        match self {
            Self::Solid(c) => *c,
            Self::Linear {
                inverse,
                p,
                d,
                len2,
                ramp,
            } => {
                let (u, v) = apply(inverse, x, y);
                // A zero-length axis paints the last stop (SVG rule).
                let t = if *len2 > 0.0 {
                    ((u - p.0) * d.0 + (v - p.1) * d.1) / len2
                } else {
                    1.0
                };
                ramp.at(t)
            }
            Self::Radial {
                inverse,
                f,
                fr,
                d,
                dr,
                ramp,
            } => {
                let (u, v) = apply(inverse, x, y);
                match conical_t((u - f.0, v - f.1), *d, *fr, *dr) {
                    Some(t) => ramp.at(t),
                    None => [0.0; 4],
                }
            }
            Self::Pattern { inverse, tile } => {
                let (u, v) = apply(inverse, x, y);
                sample_tile(tile, u, v)
            }
        }
    }
}

/// Largest `t` with `fr + t dr >= 0` such that `q` lies on the circle centred `t d` (relative to the
/// focal centre) with radius `fr + t dr` (two-circle conical gradient). `None` = not painted.
fn conical_t(q: (f64, f64), d: (f64, f64), fr: f64, dr: f64) -> Option<f64> {
    let a = d.0 * d.0 + d.1 * d.1 - dr * dr;
    let b = q.0 * d.0 + q.1 * d.1 + fr * dr;
    let c = q.0 * q.0 + q.1 * q.1 - fr * fr;
    let valid = |t: f64| t.is_finite() && fr + t * dr >= 0.0;
    if a == 0.0 {
        if b == 0.0 {
            return None;
        }
        let t = c / (2.0 * b);
        return valid(t).then_some(t);
    }
    let disc = b * b - a * c;
    if disc < 0.0 {
        return None;
    }
    let root = disc.sqrt();
    let (t1, t2) = ((b + root) / a, (b - root) / a);
    let (hi, lo) = if t1 >= t2 { (t1, t2) } else { (t2, t1) };
    if valid(hi) {
        Some(hi)
    } else if valid(lo) {
        Some(lo)
    } else {
        None
    }
}

impl Ramp<'_> {
    fn at(&self, t: f64) -> Rgba {
        let t = match self.spread {
            Spread::Pad => t.clamp(0.0, 1.0),
            Spread::Repeat => t - t.floor(),
            Spread::Reflect => {
                let m = t - 2.0 * (t * 0.5).floor();
                if m > 1.0 { 2.0 - m } else { m }
            }
        };
        let stops = self.stops;
        let (Some(first), Some(last)) = (stops.first(), stops.last()) else {
            return [0.0; 4];
        };
        if t <= first.offset {
            return premultiply(first.colour);
        }
        if t >= last.offset {
            return premultiply(last.colour);
        }
        for pair in stops.windows(2) {
            let (s0, s1) = (&pair[0], &pair[1]);
            if t >= s0.offset && t < s1.offset {
                let span = s1.offset - s0.offset;
                let u = if span > 0.0 { (t - s0.offset) / span } else { 1.0 };
                let m = s0.midpoint.unwrap_or(0.5);
                let k = if u <= m {
                    0.5 * u / m
                } else {
                    0.5 + 0.5 * (u - m) / (1.0 - m)
                } as f32;
                let mut out = [0.0f32; 4];
                for (o, (a, b)) in out.iter_mut().zip(s0.colour.iter().zip(&s1.colour)) {
                    *o = a + (b - a) * k;
                }
                return premultiply(out);
            }
        }
        premultiply(last.colour)
    }
}

fn premultiply(c: Rgba) -> Rgba {
    [c[0] * c[3], c[1] * c[3], c[2] * c[3], c[3]]
}

/// Bilinear sample with periodic wrap; texel centres at `i + 0.5`.
fn sample_tile(tile: &PatternTile, u: f64, v: f64) -> Rgba {
    let (w, h) = (f64::from(tile.width), f64::from(tile.height));
    let wrap = |x: f64, n: f64| x - n * (x / n).floor();
    let x = wrap(u - 0.5, w);
    let y = wrap(v - 0.5, h);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = ((x - x0) as f32, (y - y0) as f32);
    let index = |xi: f64, yi: f64| -> Rgba {
        let xi = wrap(xi, w) as usize;
        let yi = wrap(yi, h) as usize;
        yi.checked_mul(tile.width as usize)
            .and_then(|r| r.checked_add(xi))
            .and_then(|i| tile.pixels.get(i))
            .copied()
            .unwrap_or([0.0; 4])
    };
    let (a, b) = (index(x0, y0), index(x0 + 1.0, y0));
    let (c, d) = (index(x0, y0 + 1.0), index(x0 + 1.0, y0 + 1.0));
    let mut out = [0.0f32; 4];
    for (k, o) in out.iter_mut().enumerate() {
        let top = a[k] + (b[k] - a[k]) * fx;
        let bottom = c[k] + (d[k] - c[k]) * fx;
        *o = top + (bottom - top) * fy;
    }
    out
}
