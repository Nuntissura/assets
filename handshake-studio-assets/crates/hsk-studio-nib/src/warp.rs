//! Envelope distort and warp (lane SMB-ILLUSTRATOR-TOOLS-2; STU-VEC-055, STU-VEC-168, STU-VEC-169).
//!
//! An envelope maps the union bounds of the wrapped art onto a new shape. Art is flattened (chord
//! tolerance from `envelope_fidelity`: 0 % = 1 px, 100 % = 0.01 px), every chord is subdivided so
//! no piece is longer than `diagonal / (16 + 2.4 * fidelity)`, then each point is mapped; the
//! result is polylines (line segments, subpaths closed as in the source, fill rule kept). The
//! deviation from the exact image of the curves is bounded by the tolerance and the subdivision
//! (class Approximate).
//!
//! Kinds:
//! * [`Warp`]: one of the fifteen STU-VEC-168 styles with `bend`, horizontal and vertical
//!   distortion stored as RATIOS (-1..1; the authored percent / 100) and an axis. In normalised
//!   coordinates `x, y` in -1..1 across the bounds (y down), with `b` the bend and `s = 1 - x^2`:
//!   arc `y -= b s`, `x *= 1 + b y / 2`; arc_lower `y -= b s (1 + y) / 2`; arc_upper
//!   `y -= b s (1 - y) / 2`; arch `y -= b s`; bulge `y += b s y`; shell_lower `y += b s max(y, 0)`
//!   and `x *= 1 - b (1 + y) / 4`; shell_upper `y += b s min(y, 0)` and `x *= 1 - b (1 - y) / 4`;
//!   flag `y -= b sin(pi (x + 1)) (x + 1) / 2`; wave `y -= b sin(pi (x + 1))`; fish
//!   `y += b s y (x + 1) / 2`; rise `y -= b sin(pi x / 2)`; fisheye (r = |(x, y)| < 1)
//!   `(x, y) *= 1 + b (1 - r^2)`; inflate `(x, y) *= 1 + b (1 - r^2 / 2)`; squeeze
//!   `x *= 1 - b (1 - y^2) / 2`; twist: rotate by `b pi (1 - r)` for `r < 1`. The vertical axis
//!   swaps x and y around the style. Then horizontal distortion `y *= 1 + dh x` and vertical
//!   distortion `x *= 1 + dv y`. Sines use nib's polynomial `shape::sin_cos_deg` (no platform
//!   trigonometry). These formulas are Studio's (the captured integer mapping and
//!   Illustrator's formulas are not recorded: parity UNVERIFIED, D5).
//! * [`Mesh`]: a `(rows + 1) x (columns + 1)` grid of points (row-major from the top left) the
//!   unit square maps onto, bilinear in each cell.
//! * Top object ([`Envelope::Shape`]): a Coons patch over the first closed subpath of the top
//!   object: its four corners are the outline points nearest the bounds corners, the sides between
//!   them are parameterised by arc length.
#![deny(clippy::arithmetic_side_effects)]
use crate::arrange::{Bounds, union_bounds};
use crate::flatten::{FlattenOptions, flatten};
use crate::{Error, FillRule, OwnedPath, PathElement, PathLimits, PathPoint};
use crate::shape::sin_cos_deg;

/// Most output points of one envelope or brush call.
pub const MAX_MAPPED_POINTS: usize = 1 << 20;
/// Most mesh rows or columns.
pub const MAX_MESH_CELLS: usize = 64;

/// STU-VEC-168 warp styles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WarpStyle {
    Arc,
    ArcLower,
    ArcUpper,
    Arch,
    Bulge,
    ShellLower,
    ShellUpper,
    Flag,
    Wave,
    Fish,
    Rise,
    Fisheye,
    Inflate,
    Squeeze,
    Twist,
}

impl WarpStyle {
    pub const ALL: [WarpStyle; 15] = [
        Self::Arc, Self::ArcLower, Self::ArcUpper, Self::Arch, Self::Bulge, Self::ShellLower, Self::ShellUpper, Self::Flag, Self::Wave, Self::Fish, Self::Rise, Self::Fisheye, Self::Inflate, Self::Squeeze, Self::Twist,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WarpAxis {
    Horizontal,
    Vertical,
}

/// A warp (ratios, STU-VEC-168).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Warp {
    pub style: WarpStyle,
    pub axis: WarpAxis,
    pub bend: f64,
    pub distortion_horizontal: f64,
    pub distortion_vertical: f64,
}

/// A mesh envelope: `(rows + 1) * (columns + 1)` points, row-major from the top left.
#[derive(Clone, Debug, PartialEq)]
pub struct Mesh {
    pub rows: usize,
    pub columns: usize,
    pub points: Vec<PathPoint>,
}

impl Mesh {
    /// The undistorted grid over `b`.
    pub fn grid(b: &Bounds, rows: usize, columns: usize) -> Result<Self, Error> {
        if rows == 0 || columns == 0 || rows > MAX_MESH_CELLS || columns > MAX_MESH_CELLS {
            return Err(Error::InvalidRequest);
        }
        let mut points = Vec::new();
        for r in 0..=rows {
            for c in 0..=columns {
                let (u, v) = (c as f64 / columns as f64, r as f64 / rows as f64);
                points.push(PathPoint::new(b.min[0] + u * b.width(), b.min[1] + v * b.height()));
            }
        }
        Ok(Self { rows, columns, points })
    }
}

/// What the art is wrapped in.
#[derive(Clone, Debug, PartialEq)]
pub enum Envelope {
    Warp(Warp),
    Mesh(Mesh),
    /// The top object's outline.
    Shape(OwnedPath),
}

/// One warp style in normalised coordinates (horizontal axis).
fn style_map(style: WarpStyle, b: f64, x: f64, y: f64) -> (f64, f64) {
    let s = 1.0 - x * x;
    let r2 = x * x + y * y;
    match style {
        WarpStyle::Arc => (x * (1.0 + b * y / 2.0), y - b * s),
        WarpStyle::ArcLower => (x, y - b * s * (1.0 + y) / 2.0),
        WarpStyle::ArcUpper => (x, y - b * s * (1.0 - y) / 2.0),
        WarpStyle::Arch => (x, y - b * s),
        WarpStyle::Bulge => (x, y + b * s * y),
        WarpStyle::ShellLower => (x * (1.0 - b * (1.0 + y) / 4.0), y + b * s * y.max(0.0)),
        WarpStyle::ShellUpper => (x * (1.0 - b * (1.0 - y) / 4.0), y + b * s * y.min(0.0)),
        WarpStyle::Flag => (x, y - b * sin_cos_deg(180.0 * (x + 1.0)).0 * (x + 1.0) / 2.0),
        WarpStyle::Wave => (x, y - b * sin_cos_deg(180.0 * (x + 1.0)).0),
        WarpStyle::Fish => (x, y + b * s * y * (x + 1.0) / 2.0),
        WarpStyle::Rise => (x, y - b * sin_cos_deg(90.0 * x).0),
        WarpStyle::Fisheye => {
            let k = if r2 < 1.0 { 1.0 + b * (1.0 - r2) } else { 1.0 };
            (x * k, y * k)
        }
        WarpStyle::Inflate => {
            let k = 1.0 + b * (1.0 - r2 / 2.0);
            (x * k, y * k)
        }
        WarpStyle::Squeeze => (x * (1.0 - b * (1.0 - y * y) / 2.0), y),
        WarpStyle::Twist => {
            let r = r2.sqrt();
            if r < 1.0 {
                let (sn, cs) = sin_cos_deg(b * 180.0 * (1.0 - r));
                (x * cs - y * sn, x * sn + y * cs)
            } else {
                (x, y)
            }
        }
    }
}

/// The warp of normalised point (x, y).
pub fn warp_point(w: &Warp, x: f64, y: f64) -> (f64, f64) {
    let (x, y) = match w.axis {
        WarpAxis::Horizontal => style_map(w.style, w.bend, x, y),
        WarpAxis::Vertical => {
            let (a, b) = style_map(w.style, w.bend, y, x);
            (b, a)
        }
    };
    let y = y * (1.0 + w.distortion_horizontal * x);
    let x = x * (1.0 + w.distortion_vertical * y);
    (x, y)
}

fn lerp(a: PathPoint, b: PathPoint, t: f64) -> PathPoint {
    PathPoint::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

/// Point and arc-length table of a closed loop (for the Coons sides).
struct Side {
    points: Vec<PathPoint>,
    lengths: Vec<f64>,
}

impl Side {
    fn new(points: Vec<PathPoint>) -> Self {
        let mut lengths = vec![0.0];
        for w in points.windows(2) {
            let last = lengths.last().copied().unwrap_or(0.0);
            lengths.push(last + (sq(w[1].x - w[0].x) + sq(w[1].y - w[0].y)).sqrt());
        }
        Self { points, lengths }
    }
    fn at(&self, t: f64) -> PathPoint {
        let total = self.lengths.last().copied().unwrap_or(0.0);
        let want = t.clamp(0.0, 1.0) * total;
        let i = self.lengths.partition_point(|l| *l < want).clamp(1, self.points.len().saturating_sub(1).max(1));
        let (l0, l1) = (self.lengths[i.saturating_sub(1)], self.lengths[i]);
        let f = if l1 > l0 { (want - l0) / (l1 - l0) } else { 0.0 };
        match (self.points.get(i.saturating_sub(1)), self.points.get(i)) {
            (Some(a), Some(b)) => lerp(*a, *b, f),
            (Some(a), None) => *a,
            _ => PathPoint::new(0.0, 0.0),
        }
    }
}

struct Coons {
    top: Side,
    bottom: Side,
    left: Side,
    right: Side,
}

fn coons(shape: &OwnedPath) -> Result<Coons, Error> {
    let lines = flatten(shape, &FlattenOptions { tolerance: 0.01, ..FlattenOptions::default() })?;
    let ring = lines.into_iter().find(|l| l.closed && l.points.len() >= 3).ok_or(Error::InvalidGeometry)?;
    let b = crate::arrange::path_bounds(shape).ok_or(Error::InvalidGeometry)?;
    let pts = ring.points;
    let n = pts.len();
    let nearest = |c: [f64; 2]| -> usize {
        (0..n).min_by(|i, j| {
            let d = |k: usize| sq(pts[k].x - c[0]) + sq(pts[k].y - c[1]);
            d(*i).total_cmp(&d(*j))
        }).unwrap_or(0)
    };
    let corners = [nearest(b.min), nearest([b.max[0], b.min[1]]), nearest(b.max), nearest([b.min[0], b.max[1]])];
    // Orientation: walking forward from the top-left corner must reach top-right before bottom-left.
    let ahead = |from: usize, to: usize| to.wrapping_sub(from).wrapping_add(n).checked_rem(n).unwrap_or(0);
    let forward = ahead(corners[0], corners[1]) < ahead(corners[0], corners[3]);
    let walk = |from: usize, to: usize| -> Vec<PathPoint> {
        let mut out = vec![pts[from]];
        let mut i = from;
        let mut guard = 0_usize;
        while i != to && guard <= n {
            i = if forward { i.wrapping_add(1).checked_rem(n).unwrap_or(0) } else { i.wrapping_add(n).wrapping_sub(1).checked_rem(n).unwrap_or(0) };
            out.push(pts[i]);
            guard = guard.saturating_add(1);
        }
        out
    };
    let [tl, tr, br, bl] = corners;
    if [tl, tr, br, bl].windows(2).any(|w| w[0] == w[1]) {
        return Err(Error::InvalidGeometry);
    }
    let top = walk(tl, tr);
    let right = walk(tr, br);
    let mut bottom = walk(br, bl);
    bottom.reverse();
    let mut left = walk(bl, tl);
    left.reverse();
    Ok(Coons { top: Side::new(top), bottom: Side::new(bottom), left: Side::new(left), right: Side::new(right) })
}

impl Coons {
    fn at(&self, u: f64, v: f64) -> PathPoint {
        let (t, bo, l, r) = (self.top.at(u), self.bottom.at(u), self.left.at(v), self.right.at(v));
        let (c00, c10, c01, c11) = (self.top.at(0.0), self.top.at(1.0), self.bottom.at(0.0), self.bottom.at(1.0));
        let x = (1.0 - v) * t.x + v * bo.x + (1.0 - u) * l.x + u * r.x - ((1.0 - u) * (1.0 - v) * c00.x + u * (1.0 - v) * c10.x + (1.0 - u) * v * c01.x + u * v * c11.x);
        let y = (1.0 - v) * t.y + v * bo.y + (1.0 - u) * l.y + u * r.y - ((1.0 - u) * (1.0 - v) * c00.y + u * (1.0 - v) * c10.y + (1.0 - u) * v * c01.y + u * v * c11.y);
        PathPoint::new(x, y)
    }
}

fn mesh_at(m: &Mesh, u: f64, v: f64) -> PathPoint {
    let (fc, fr) = (u.clamp(0.0, 1.0) * m.columns as f64, v.clamp(0.0, 1.0) * m.rows as f64);
    let (c, r) = ((fc.floor() as usize).min(m.columns.saturating_sub(1)), (fr.floor() as usize).min(m.rows.saturating_sub(1)));
    let (fu, fv) = (fc - c as f64, fr - r as f64);
    let w = m.columns.saturating_add(1);
    let at = |rr: usize, cc: usize| m.points.get(rr.saturating_mul(w).saturating_add(cc)).copied().unwrap_or(PathPoint::new(0.0, 0.0));
    let r1 = r.saturating_add(1);
    let c1 = c.saturating_add(1);
    let top = lerp(at(r, c), at(r, c1), fu);
    let bottom = lerp(at(r1, c), at(r1, c1), fu);
    lerp(top, bottom, fv)
}

/// Flattens `paths`, subdivides chords to `max_segment`, maps every point through `f` and emits
/// polylines (closed subpaths stay closed; fill rule kept). Points are budgeted before mapping.
pub(crate) fn map_paths(paths: &[OwnedPath], tolerance: f64, max_segment: f64, f: &dyn Fn(PathPoint) -> PathPoint) -> Result<Vec<OwnedPath>, Error> {
    if !(max_segment.is_finite() && max_segment > 0.0) {
        return Err(Error::InvalidRequest);
    }
    let mut budget = MAX_MAPPED_POINTS;
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let lines = flatten(path, &FlattenOptions { tolerance, max_points: MAX_MAPPED_POINTS, path_limits: PathLimits::default() })?;
        let mut o = OwnedPath::new(path.fill_rule);
        for l in lines {
            let mut pts = l.points.clone();
            if l.closed && let Some(first) = pts.first().copied() {
                pts.push(first);
            }
            let mut first = true;
            for w in pts.windows(2) {
                let len = (sq(w[1].x - w[0].x) + sq(w[1].y - w[0].y)).sqrt();
                let pieces = (len / max_segment).ceil().clamp(1.0, MAX_MAPPED_POINTS as f64) as usize;
                budget = budget.checked_sub(pieces.saturating_add(1)).ok_or(Error::BudgetExceeded)?;
                if first {
                    o.elements.push(PathElement::MoveTo(f(w[0])));
                    first = false;
                }
                for k in 1..=pieces {
                    let q = f(lerp(w[0], w[1], k as f64 / pieces as f64));
                    if !(q.x.is_finite() && q.y.is_finite()) {
                        return Err(Error::InvalidGeometry);
                    }
                    o.elements.push(PathElement::LineTo(q));
                }
            }
            if first && let Some(p) = pts.first() {
                o.elements.push(PathElement::MoveTo(f(*p)));
            }
            if l.closed {
                o.elements.push(PathElement::Close);
            }
        }
        o.validate(&PathLimits { max_elements: MAX_MAPPED_POINTS.saturating_mul(2), require_closed: false })?;
        out.push(o);
    }
    Ok(out)
}

/// Chord tolerance and largest piece of an envelope at `fidelity` percent over `b`.
fn fidelity_steps(fidelity: f64, b: &Bounds) -> Result<(f64, f64), Error> {
    if !(fidelity.is_finite() && (0.0..=100.0).contains(&fidelity)) {
        return Err(Error::InvalidRequest);
    }
    let tolerance = 1.0 - 0.0099 * fidelity;
    let diagonal = (sq(b.width()) + sq(b.height())).sqrt().max(1.0);
    Ok((tolerance, diagonal / (16.0 + 2.4 * fidelity)))
}

/// The art `paths` wrapped in `envelope` (module docs); `fidelity` percent 0..=100.
pub fn envelope_paths(paths: &[OwnedPath], envelope: &Envelope, fidelity: f64) -> Result<Vec<OwnedPath>, Error> {
    let Some(b) = union_bounds(paths) else { return Ok(paths.to_vec()) };
    let (tol, seg) = fidelity_steps(fidelity, &b)?;
    let (cx, cy) = (b.centre()[0], b.centre()[1]);
    let (hw, hh) = ((b.width() / 2.0).max(f64::MIN_POSITIVE), (b.height() / 2.0).max(f64::MIN_POSITIVE));
    let unit = move |q: PathPoint| ((q.x - b.min[0]) / b.width().max(f64::MIN_POSITIVE), (q.y - b.min[1]) / b.height().max(f64::MIN_POSITIVE));
    match envelope {
        Envelope::Warp(w) => {
            if ![w.bend, w.distortion_horizontal, w.distortion_vertical].iter().all(|v| v.is_finite() && (-1.0..=1.0).contains(v)) {
                return Err(Error::InvalidRequest);
            }
            let f = |q: PathPoint| {
                let (x, y) = warp_point(w, (q.x - cx) / hw, (q.y - cy) / hh);
                PathPoint::new(cx + x * hw, cy + y * hh)
            };
            map_paths(paths, tol, seg, &f)
        }
        Envelope::Mesh(m) => {
            if m.rows == 0 || m.columns == 0 || m.rows > MAX_MESH_CELLS || m.columns > MAX_MESH_CELLS {
                return Err(Error::InvalidRequest);
            }
            let want = m.rows.saturating_add(1).saturating_mul(m.columns.saturating_add(1));
            if m.points.len() != want || m.points.iter().any(|q| !(q.x.is_finite() && q.y.is_finite())) {
                return Err(Error::InvalidRequest);
            }
            let f = |q: PathPoint| {
                let (u, v) = unit(q);
                mesh_at(m, u, v)
            };
            map_paths(paths, tol, seg, &f)
        }
        Envelope::Shape(shape) => {
            let c = coons(shape)?;
            let f = |q: PathPoint| {
                let (u, v) = unit(q);
                c.at(u, v)
            };
            map_paths(paths, tol, seg, &f)
        }
    }
}

/// A closed rectangle path (helper for callers and tests).
pub fn rect(b: &Bounds) -> OwnedPath {
    let mut o = OwnedPath::new(FillRule::NonZero);
    o.elements = vec![
        PathElement::MoveTo(PathPoint::new(b.min[0], b.min[1])),
        PathElement::LineTo(PathPoint::new(b.max[0], b.min[1])),
        PathElement::LineTo(PathPoint::new(b.max[0], b.max[1])),
        PathElement::LineTo(PathPoint::new(b.min[0], b.max[1])),
        PathElement::Close,
    ];
    o
}

fn sq(v: f64) -> f64 {
    v * v
}
