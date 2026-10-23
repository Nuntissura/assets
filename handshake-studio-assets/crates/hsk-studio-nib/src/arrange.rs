//! Arrange: object transforms, transform-each, align and distribute (lane SMB-ILLUSTRATOR-TOOLS-2;
//! STU-VEC-029, STU-VEC-157, STU-VEC-166). Nib owns the geometry; callers apply the returned
//! affines (`[a, b, c, d, e, f]`, SVG order: `x' = a x + c y + e`, `y' = b x + d y + f`, y down) or
//! translations to their own records.
//!
//! * Bounds are the exact geometric bounds of a path (line end points; cubic and quadratic
//!   extrema from the derivative roots), not the control-point hull.
//! * [`rotate_about`] turns clockwise on screen (y down) for a positive angle; [`reflect_about`]
//!   mirrors across the line through the centre at `axis_deg` (0 = horizontal axis: a vertical
//!   flip); [`shear_about`] shears by `angle_deg` along the axis at `axis_deg` (0 = horizontal:
//!   `x' = x + tan(angle) (y - cy)`). Angles use nib's polynomial `shape::sin_cos_deg` (exact at
//!   multiples of 90 degrees, no platform trigonometry).
//! * [`transform_each`] (STU-VEC-157): every object is scaled (percent), reflected, rotated and
//!   moved about its own reference point ([`ReferencePoint`], nine positions; the captured integer
//!   1..9 is read row-major from the top left: Studio's reading, the captured mapping is not
//!   recorded). `randomize` draws each copy's scale, move and rotation uniformly between the
//!   identity value and the given value (seeded, deterministic; Studio rule, Illustrator parity
//!   UNVERIFIED, D5).
//! * [`align`] / [`distribute`] (STU-VEC-166): targets selection (the union bounds), a key object
//!   (it never moves) or an artboard. Distribute keeps the two extreme objects (by the distributed
//!   coordinate; artboard: the artboard edges) and spaces the others evenly; distribute-spacing with
//!   an explicit gap packs objects from the key object outward (selection target: from the
//!   first object in coordinate order).
#![deny(clippy::arithmetic_side_effects)]
use crate::raster::Affine;
use crate::shape::sin_cos_deg;
use crate::{Error, OwnedPath, PathElement, PathPoint};

/// Most objects one arrange call takes.
pub const MAX_ARRANGE_OBJECTS: usize = 65_536;
/// Hard range of transform-each scale (percent, STU-VEC-157).
pub const TRANSFORM_EACH_SCALE: (f64, f64) = (-4000.0, 4000.0);
/// Hard range of transform-each move (document units, STU-VEC-157).
pub const TRANSFORM_EACH_MOVE: (f64, f64) = (-4000.0, 4000.0);

/// Axis-aligned bounds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub min: [f64; 2],
    pub max: [f64; 2],
}

impl Bounds {
    pub fn width(&self) -> f64 {
        self.max[0] - self.min[0]
    }
    pub fn height(&self) -> f64 {
        self.max[1] - self.min[1]
    }
    pub fn centre(&self) -> [f64; 2] {
        [(self.min[0] + self.max[0]) / 2.0, (self.min[1] + self.max[1]) / 2.0]
    }
    pub fn union(&self, o: &Bounds) -> Bounds {
        Bounds { min: [self.min[0].min(o.min[0]), self.min[1].min(o.min[1])], max: [self.max[0].max(o.max[0]), self.max[1].max(o.max[1])] }
    }
    fn finite(&self) -> bool {
        self.min.iter().chain(self.max.iter()).all(|v| v.is_finite()) && self.min[0] <= self.max[0] && self.min[1] <= self.max[1]
    }
    fn include(&mut self, q: PathPoint) {
        self.min = [self.min[0].min(q.x), self.min[1].min(q.y)];
        self.max = [self.max[0].max(q.x), self.max[1].max(q.y)];
    }
}

/// Roots in (0, 1) of `a t^2 + b t + c`.
fn unit_roots(a: f64, b: f64, c: f64) -> Vec<f64> {
    let mut out = Vec::new();
    if a.abs() < 1e-12 {
        if b.abs() > 1e-12 {
            out.push(-c / b);
        }
    } else {
        let disc = b * b - 4.0 * a * c;
        if disc >= 0.0 {
            let s = disc.sqrt();
            out.push((-b + s) / (2.0 * a));
            out.push((-b - s) / (2.0 * a));
        }
    }
    out.retain(|t| *t > 0.0 && *t < 1.0);
    out
}

fn cubic_at(p0: f64, p1: f64, p2: f64, p3: f64, t: f64) -> f64 {
    let u = 1.0 - t;
    u * u * u * p0 + 3.0 * u * u * t * p1 + 3.0 * u * t * t * p2 + t * t * t * p3
}

/// Exact geometric bounds of `path` (`None` for an empty path).
pub fn path_bounds(path: &OwnedPath) -> Option<Bounds> {
    let mut b: Option<Bounds> = None;
    let mut last = PathPoint::new(0.0, 0.0);
    let mut start = last;
    let add = |q: PathPoint, b: &mut Option<Bounds>| match b {
        Some(x) => x.include(q),
        None => *b = Some(Bounds { min: [q.x, q.y], max: [q.x, q.y] }),
    };
    for e in &path.elements {
        match *e {
            PathElement::MoveTo(q) => {
                add(q, &mut b);
                last = q;
                start = q;
            }
            PathElement::LineTo(q) => {
                add(q, &mut b);
                last = q;
            }
            PathElement::QuadTo(c, q) => {
                // Elevate to a cubic.
                let c1 = PathPoint::new(last.x + 2.0 / 3.0 * (c.x - last.x), last.y + 2.0 / 3.0 * (c.y - last.y));
                let c2 = PathPoint::new(q.x + 2.0 / 3.0 * (c.x - q.x), q.y + 2.0 / 3.0 * (c.y - q.y));
                cubic_bounds(last, c1, c2, q, &mut |p| add(p, &mut b));
                last = q;
            }
            PathElement::CubicTo(c1, c2, q) => {
                cubic_bounds(last, c1, c2, q, &mut |p| add(p, &mut b));
                last = q;
            }
            PathElement::Close => last = start,
        }
    }
    b
}

fn cubic_bounds(p0: PathPoint, p1: PathPoint, p2: PathPoint, p3: PathPoint, add: &mut dyn FnMut(PathPoint)) {
    add(p3);
    for axis in 0..2 {
        let g = |q: PathPoint| if axis == 0 { q.x } else { q.y };
        let (a0, a1, a2, a3) = (g(p0), g(p1), g(p2), g(p3));
        // Derivative / 3: (a1 - a0)(1-t)^2 + 2(a2 - a1)(1-t)t + (a3 - a2)t^2.
        let (d0, d1, d2) = (a1 - a0, a2 - a1, a3 - a2);
        for t in unit_roots(d0 - 2.0 * d1 + d2, 2.0 * (d1 - d0), d0) {
            add(PathPoint::new(cubic_at(p0.x, p1.x, p2.x, p3.x, t), cubic_at(p0.y, p1.y, p2.y, p3.y, t)));
        }
    }
}

/// Union bounds of several paths.
pub fn union_bounds(paths: &[OwnedPath]) -> Option<Bounds> {
    paths.iter().filter_map(path_bounds).reduce(|a, b| a.union(&b))
}

fn check(v: &[f64]) -> Result<(), Error> {
    if v.iter().all(|x| x.is_finite()) { Ok(()) } else { Err(Error::InvalidRequest) }
}

fn about(c: [f64; 2], m: [f64; 4]) -> Affine {
    [m[0], m[1], m[2], m[3], c[0] - m[0] * c[0] - m[2] * c[1], c[1] - m[1] * c[0] - m[3] * c[1]]
}

/// Rotation by `deg` (clockwise on screen) about `c`.
pub fn rotate_about(c: [f64; 2], deg: f64) -> Result<Affine, Error> {
    check(&[c[0], c[1], deg])?;
    let (s, k) = sin_cos_deg(deg);
    Ok(about(c, [k, s, -s, k]))
}

/// Scale by `sx`, `sy` (factors) about `c`.
pub fn scale_about(c: [f64; 2], sx: f64, sy: f64) -> Result<Affine, Error> {
    check(&[c[0], c[1], sx, sy])?;
    Ok(about(c, [sx, 0.0, 0.0, sy]))
}

/// Reflection across the line through `c` at `axis_deg` from the horizontal.
pub fn reflect_about(c: [f64; 2], axis_deg: f64) -> Result<Affine, Error> {
    check(&[c[0], c[1], axis_deg])?;
    let (s, k) = sin_cos_deg(2.0 * axis_deg);
    Ok(about(c, [k, s, s, -k]))
}

/// Shear by `angle_deg` (`|angle| < 90`) along the axis at `axis_deg` through `c`.
pub fn shear_about(c: [f64; 2], angle_deg: f64, axis_deg: f64) -> Result<Affine, Error> {
    check(&[c[0], c[1], angle_deg, axis_deg])?;
    if angle_deg.abs() >= 89.999 {
        return Err(Error::InvalidRequest);
    }
    let (ts, tc) = sin_cos_deg(angle_deg);
    let t = ts / tc;
    let (s, k) = sin_cos_deg(axis_deg);
    // R(axis) . [1 t; 0 1] . R(-axis)
    let m = [1.0 - t * s * k, -t * s * s, t * k * k, 1.0 + t * s * k];
    Ok(about(c, m))
}

/// Nine-position reference point (STU-VEC-157).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReferencePoint {
    TopLeft,
    Top,
    TopRight,
    Left,
    Centre,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl ReferencePoint {
    /// Studio's reading of the captured integer 1..9 (row-major from the top left).
    pub fn from_wire(v: u8) -> Option<Self> {
        use ReferencePoint::*;
        [TopLeft, Top, TopRight, Left, Centre, Right, BottomLeft, Bottom, BottomRight].get(usize::from(v).checked_sub(1)?).copied()
    }
    /// The point of `b`.
    pub fn of(self, b: &Bounds) -> [f64; 2] {
        use ReferencePoint::*;
        let x = match self {
            TopLeft | Left | BottomLeft => b.min[0],
            Top | Centre | Bottom => b.centre()[0],
            TopRight | Right | BottomRight => b.max[0],
        };
        let y = match self {
            TopLeft | Top | TopRight => b.min[1],
            Left | Centre | Right => b.centre()[1],
            BottomLeft | Bottom | BottomRight => b.max[1],
        };
        [x, y]
    }
}

/// The transform-each parameter set (STU-VEC-157): scale in percent, move in document units,
/// rotation in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransformEach {
    pub scale_horizontal: f64,
    pub scale_vertical: f64,
    pub move_horizontal: f64,
    pub move_vertical: f64,
    pub rotate: f64,
    pub reflect_x: bool,
    pub reflect_y: bool,
    pub reference: ReferencePoint,
    pub randomize: bool,
    pub seed: u64,
}

impl Default for TransformEach {
    fn default() -> Self {
        Self { scale_horizontal: 100.0, scale_vertical: 100.0, move_horizontal: 0.0, move_vertical: 0.0, rotate: 0.0, reflect_x: false, reflect_y: false, reference: ReferencePoint::Centre, randomize: false, seed: 0 }
    }
}

fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Deterministic uniform value in `[0, 1)` for (`seed`, `stream`, `index`).
pub fn unit_random(seed: u64, stream: u64, index: u64) -> f64 {
    let h = splitmix(seed ^ splitmix(stream.wrapping_mul(0x1000_0000_01B3) ^ splitmix(index)));
    (h >> 11) as f64 / (1_u64 << 53) as f64
}

/// One affine per object: `T = translate(move) . about(ref, rotate . reflect . scale)`.
pub fn transform_each(bounds: &[Bounds], p: &TransformEach) -> Result<Vec<Affine>, Error> {
    if bounds.len() > MAX_ARRANGE_OBJECTS {
        return Err(Error::BudgetExceeded);
    }
    check(&[p.scale_horizontal, p.scale_vertical, p.move_horizontal, p.move_vertical, p.rotate])?;
    let inside = |v: f64, r: (f64, f64)| (r.0..=r.1).contains(&v);
    if !inside(p.scale_horizontal, TRANSFORM_EACH_SCALE) || !inside(p.scale_vertical, TRANSFORM_EACH_SCALE) || !inside(p.move_horizontal, TRANSFORM_EACH_MOVE) || !inside(p.move_vertical, TRANSFORM_EACH_MOVE) {
        return Err(Error::InvalidRequest);
    }
    let mut out = Vec::with_capacity(bounds.len());
    for (i, b) in bounds.iter().enumerate() {
        if !b.finite() {
            return Err(Error::InvalidRequest);
        }
        let i = i as u64;
        let r = |stream: u64, from: f64, to: f64| if p.randomize { from + (to - from) * unit_random(p.seed, stream, i) } else { to };
        let sx = r(1, 100.0, p.scale_horizontal) / 100.0 * if p.reflect_x { -1.0 } else { 1.0 };
        let sy = r(2, 100.0, p.scale_vertical) / 100.0 * if p.reflect_y { -1.0 } else { 1.0 };
        let deg = r(3, 0.0, p.rotate);
        let (s, k) = sin_cos_deg(deg);
        // rotate . scale
        let m = [k * sx, s * sx, -s * sy, k * sy];
        let mut a = about(p.reference.of(b), m);
        a[4] += r(4, 0.0, p.move_horizontal);
        a[5] += r(5, 0.0, p.move_vertical);
        out.push(a);
    }
    Ok(out)
}

/// Align / distribute target (STU-VEC-166 `align_to`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AlignTo {
    Selection,
    KeyObject(usize),
    Artboard(Bounds),
}

/// Which edge or centre aligns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    CentreHorizontal,
    Right,
    Top,
    CentreVertical,
    Bottom,
}

impl Align {
    fn axis(self) -> usize {
        match self {
            Self::Left | Self::CentreHorizontal | Self::Right => 0,
            _ => 1,
        }
    }
    fn coord(self, b: &Bounds) -> f64 {
        match self {
            Self::Left => b.min[0],
            Self::Top => b.min[1],
            Self::Right => b.max[0],
            Self::Bottom => b.max[1],
            Self::CentreHorizontal => b.centre()[0],
            Self::CentreVertical => b.centre()[1],
        }
    }
}

fn target(bounds: &[Bounds], to: AlignTo) -> Result<Bounds, Error> {
    if bounds.len() > MAX_ARRANGE_OBJECTS || bounds.iter().any(|b| !b.finite()) {
        return Err(Error::InvalidRequest);
    }
    match to {
        AlignTo::Selection => bounds.iter().copied().reduce(|a, b| a.union(&b)).ok_or(Error::InvalidRequest),
        AlignTo::KeyObject(k) => bounds.get(k).copied().ok_or(Error::InvalidRequest),
        AlignTo::Artboard(b) if b.finite() => Ok(b),
        AlignTo::Artboard(_) => Err(Error::InvalidRequest),
    }
}

fn shift(axis: usize, d: f64) -> [f64; 2] {
    if axis == 0 { [d, 0.0] } else { [0.0, d] }
}

/// Translations that align every object's `how` edge or centre with the target's.
pub fn align(bounds: &[Bounds], to: AlignTo, how: Align) -> Result<Vec<[f64; 2]>, Error> {
    let t = how.coord(&target(bounds, to)?);
    Ok(bounds.iter().map(|b| shift(how.axis(), t - how.coord(b))).collect())
}

/// Distribution (STU-VEC-166): an edge or centre spaced evenly, or equal / explicit gaps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Distribute {
    Edge(Align),
    /// Equal gaps between objects along the axis (0 = horizontal), or the explicit `gap`.
    Spacing { axis: usize, gap: Option<f64> },
}

/// Translations distributing the objects.
pub fn distribute(bounds: &[Bounds], to: AlignTo, how: Distribute) -> Result<Vec<[f64; 2]>, Error> {
    target(bounds, to)?;
    let n = bounds.len();
    let mut out = vec![[0.0, 0.0]; n];
    if n == 0 || (n < 2 && !matches!(to, AlignTo::Artboard(_))) {
        return Ok(out);
    }
    match how {
        Distribute::Edge(a) => {
            let axis = a.axis();
            let mut order: Vec<usize> = (0..n).collect();
            order.sort_by(|x, y| a.coord(&bounds[*x]).total_cmp(&a.coord(&bounds[*y])));
            let (lo_i, hi_i) = (order[0], order[n.saturating_sub(1)]);
            let (first, last) = match to {
                // Inside the artboard: the extreme objects touch its edges.
                AlignTo::Artboard(f) => (f.min[axis] + (a.coord(&bounds[lo_i]) - bounds[lo_i].min[axis]), f.max[axis] - (bounds[hi_i].max[axis] - a.coord(&bounds[hi_i]))),
                _ => (a.coord(&bounds[lo_i]), a.coord(&bounds[hi_i])),
            };
            let steps = n.saturating_sub(1).max(1) as f64;
            for (k, i) in order.iter().enumerate() {
                let want = first + (last - first) * (k as f64) / steps;
                out[*i] = shift(axis, want - a.coord(&bounds[*i]));
            }
        }
        Distribute::Spacing { axis, gap } => {
            if axis > 1 || gap.is_some_and(|g| !g.is_finite()) {
                return Err(Error::InvalidRequest);
            }
            let lo = |b: &Bounds| b.min[axis];
            let size = |b: &Bounds| b.max[axis] - b.min[axis];
            let mut order: Vec<usize> = (0..n).collect();
            order.sort_by(|x, y| (lo(&bounds[*x]) + size(&bounds[*x]) / 2.0).total_cmp(&(lo(&bounds[*y]) + size(&bounds[*y]) / 2.0)));
            let total: f64 = bounds.iter().map(size).sum();
            match gap {
                None => {
                    let (start, end) = match to {
                        AlignTo::Artboard(f) => (f.min[axis], f.max[axis]),
                        _ => (lo(&bounds[order[0]]), bounds[order[n.saturating_sub(1)]].max[axis]),
                    };
                    let slots = n.saturating_sub(1).max(1) as f64;
                    let g = (end - start - total) / slots;
                    let mut at = start;
                    for i in &order {
                        out[*i] = shift(axis, at - lo(&bounds[*i]));
                        at += size(&bounds[*i]) + g;
                    }
                }
                Some(g) => {
                    let key = match to {
                        AlignTo::KeyObject(k) => order.iter().position(|i| *i == k).ok_or(Error::InvalidRequest)?,
                        _ => 0,
                    };
                    let mut at = bounds[order[key]].max[axis];
                    for i in order.iter().skip(key.saturating_add(1)) {
                        at += g;
                        out[*i] = shift(axis, at - lo(&bounds[*i]));
                        at += size(&bounds[*i]);
                    }
                    let mut at = lo(&bounds[order[key]]);
                    for i in order.iter().take(key).rev() {
                        at -= g + size(&bounds[*i]);
                        out[*i] = shift(axis, at - lo(&bounds[*i]));
                    }
                }
            }
        }
    }
    Ok(out)
}
