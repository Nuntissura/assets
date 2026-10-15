//! Shape-layer path operators (lane SMB-AE-MOTION-TOOLS slice 3; STU-MOT-100..104: the shape
//! geometry engine is nib, operators get no private geometry elsewhere). Every operator maps owned
//! paths to owned paths (or transforms); composite's shape-layer evaluator orders them.
//!
//! Generators: [`rect_path`] (centre, size, roundness = nib `rectangle` round corners),
//! [`ellipse_path`] (nib `ellipse`), [`polystar_path`] (star or polygon, fractional rotation,
//! inner/outer radius and roundness), all with an optional reversed direction.
//!
//! Modifiers (Studio's stated rules; After Effects parity UNVERIFIED, D5; where After Effects
//! behaviour is documented by the MIT lottie-web reimplementation the rule follows it):
//! * [`trim_path`]: start / end in percent of the arc length, offset in degrees (360 = one full
//!   length); `start > end` swaps; the range is shifted into `[0, 1)`; a range past the end wraps
//!   (one continuous contour on a closed subpath, two pieces on an open one); `Simultaneously` trims
//!   every subpath by its own length, `Individually` trims the concatenation of all subpaths.
//!   Arc length: lines exact; cubics by 16-point Gauss-Legendre on 8 equal parameter pieces; the
//!   cut parameter by 60 bisection steps; sub-curves by de Casteljau.
//! * [`repeater`]: copy `i` (`0 <= i < ceil(copies)`) gets `T^(i + offset)`, `T` =
//!   `translate(anchor + position) . rotate(rotation) . scale(scale) . translate(-anchor)` (cumulative, so a rotated
//!   position walks a curve), a fractional power applies the fraction of position and rotation and
//!   `scale^f`; opacity `start + (end - start) * i / (n - 1)`; a fractional copy count fades the last
//!   copy by its fraction.
//! * [`offset_path`]: closed subpaths grow by the union with (shrink by the difference of) their own
//!   stroke outline of width `2 |amount|` with the given join; open subpaths become that outline
//!   (`amount > 0`) or vanish; `copies` concentric results at `amount * k`, `k = 1..=copies`.
//! * [`merge_paths`]: `Merge` concatenates (compound path), `Add` / `Subtract` (first minus the
//!   rest) / `Intersect` / `ExcludeIntersections` are nib booleans.
//! * [`pucker_bloat`]: lottie-web's rule: vertices move toward the vertex centroid by `a = amount /
//!   100`, Bezier control points away from it by `a` (a straight segment's control points are its
//!   end points): bloat (`a > 0`) bulges the edges, pucker (`a < 0`) pinches them.
//! * [`round_corners`]: every vertex between two straight segments (closed: all; open: interior)
//!   with turning angle `phi` is cut back `d = min(radius, half of each adjacent segment)` and
//!   replaced by the cubic arc of radius `d / tan(phi / 2)` (handle `4/3 tan(phi / 4)` of it; a
//!   right angle gives the circle constant), computed with `sqrt` only.
//! * [`zig_zag`]: each segment is cut into `ridges + 1` equal-length pieces; interior points move
//!   along the normal `(-t_y, t_x)` by `+size, -size, ...`; corner = polyline, smooth = Catmull-Rom
//!   cubics through the same points. End anchors stay.
//! * [`wiggle_path`]: each segment is cut into `detail + 1` equal-length pieces; every point moves by
//!   `size * (N_x, N_y)`, `N` seeded smoothstep value noise at `time * frequency + temporal_phase /
//!   360 + point * spatial_phase / 360`, `correlation` mixing one common stream with each point's.
//! * [`twist`]: every anchor and control point rotates about `center` by `angle * (1 - d / R)`, `d`
//!   its distance to the centre, `R` the largest anchor distance (centre rotates fully, the
//!   outermost anchor stays).
//!
//! Hostile sizes (V4-01/02): every count (copies, ridges, detail points, output elements) is
//! bounded before allocation and every parameter is finite.
#![deny(clippy::arithmetic_side_effects)]
use crate::anchor_path::AnchorPath;
use crate::boolean::{BooleanOp, BooleanOptions, boolean, normalise_path};
use crate::owned_path::{COORDINATE_LIMIT, FillRule, OwnedPath, PathElement, PathLimits, PathPoint};
use crate::path_contract::Error;
use crate::raster::Affine;
use crate::shape::{CornerKind, EllipseSpec, RectSpec, ellipse, rectangle, sin_cos_deg};
use crate::stroke::{LineJoin, StrokeOptions, StrokeStyle, outline_stroke};

/// Largest element count of one operator result.
pub const MAX_OP_ELEMENTS: usize = 1 << 18;
/// Largest repeater copy count.
pub const MAX_REPEATER_COPIES: f64 = 1024.0;
/// Largest zig-zag ridge or wiggle detail count per segment.
pub const MAX_POINTS_PER_SEGMENT: u32 = 1024;
/// Largest polystar point count.
pub const MAX_POLYSTAR_POINTS: f64 = 1000.0;
/// Largest offset-paths copy count.
pub const MAX_OFFSET_COPIES: u32 = 64;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Seg {
    Line(PathPoint, PathPoint),
    Cubic(PathPoint, PathPoint, PathPoint, PathPoint),
}

#[derive(Clone, Debug, PartialEq)]
struct Sub {
    segs: Vec<Seg>,
    start: PathPoint,
    closed: bool,
}

fn p(x: f64, y: f64) -> PathPoint {
    PathPoint::new(x, y)
}
fn add(a: PathPoint, b: PathPoint) -> PathPoint {
    p(a.x + b.x, a.y + b.y)
}
fn sub(a: PathPoint, b: PathPoint) -> PathPoint {
    p(a.x - b.x, a.y - b.y)
}
fn mul(a: PathPoint, k: f64) -> PathPoint {
    p(a.x * k, a.y * k)
}
fn lerp(a: PathPoint, b: PathPoint, t: f64) -> PathPoint {
    p(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}
fn norm(a: PathPoint) -> f64 {
    (a.x * a.x + a.y * a.y).sqrt()
}

fn param(name: &'static str, v: f64) -> Result<f64, Error> {
    let _ = name;
    if v.is_finite() && v.abs() <= COORDINATE_LIMIT {
        Ok(v)
    } else {
        Err(Error::InvalidRequest)
    }
}

impl Seg {
    fn start(&self) -> PathPoint {
        match *self {
            Seg::Line(a, _) | Seg::Cubic(a, _, _, _) => a,
        }
    }
    fn end(&self) -> PathPoint {
        match *self {
            Seg::Line(_, b) | Seg::Cubic(_, _, _, b) => b,
        }
    }
    fn at(&self, t: f64) -> PathPoint {
        match *self {
            Seg::Line(a, b) => lerp(a, b, t),
            Seg::Cubic(a, b, c, d) => {
                let (ab, bc, cd) = (lerp(a, b, t), lerp(b, c, t), lerp(c, d, t));
                lerp(lerp(ab, bc, t), lerp(bc, cd, t), t)
            }
        }
    }
    fn derivative(&self, t: f64) -> PathPoint {
        match *self {
            Seg::Line(a, b) => sub(b, a),
            Seg::Cubic(a, b, c, d) => {
                let m = 1.0 - t;
                add(add(mul(sub(b, a), 3.0 * m * m), mul(sub(c, b), 6.0 * m * t)), mul(sub(d, c), 3.0 * t * t))
            }
        }
    }
    /// Arc length from 0 to `t`.
    fn length_to(&self, t: f64) -> f64 {
        match *self {
            Seg::Line(a, b) => norm(sub(b, a)) * t,
            Seg::Cubic(..) => {
                // 16-point Gauss-Legendre on 8 equal pieces of [0, t].
                const X: [f64; 8] = [
                    0.095_012_509_837_637_44, 0.281_603_550_779_258_9, 0.458_016_777_657_227_4, 0.617_876_244_402_643_8,
                    0.755_404_408_355_003, 0.865_631_202_387_831_8, 0.944_575_023_073_232_6, 0.989_400_934_991_649_9,
                ];
                const W: [f64; 8] = [
                    0.189_450_610_455_068_5, 0.182_603_415_044_923_6, 0.169_156_519_395_002_5, 0.149_595_988_816_576_7,
                    0.124_628_971_255_533_9, 0.095_158_511_682_492_8, 0.062_253_523_938_647_9, 0.027_152_459_411_754_1,
                ];
                let pieces = 8.0;
                let h = t / pieces;
                let mut total = 0.0;
                for k in 0..8_u32 {
                    let mid = h * (f64::from(k) + 0.5);
                    for (x, w) in X.iter().zip(W) {
                        for s in [-1.0, 1.0] {
                            total += w * norm(self.derivative(mid + s * x * h / 2.0)) * h / 2.0;
                        }
                    }
                }
                total
            }
        }
    }
    fn length(&self) -> f64 {
        self.length_to(1.0)
    }
    /// Parameter at arc length `len` (clamped to the segment).
    fn t_at(&self, len: f64, total: f64) -> f64 {
        if total <= 0.0 || len <= 0.0 {
            return 0.0;
        }
        if len >= total {
            return 1.0;
        }
        if let Seg::Line(..) = self {
            return len / total;
        }
        let (mut lo, mut hi) = (0.0, 1.0);
        for _ in 0..60 {
            let mid = 0.5 * (lo + hi);
            if self.length_to(mid) < len {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        0.5 * (lo + hi)
    }
    /// The piece of the segment between parameters `t0 <= t1`.
    fn piece(&self, t0: f64, t1: f64) -> Seg {
        match *self {
            Seg::Line(..) => Seg::Line(self.at(t0), self.at(t1)),
            Seg::Cubic(a, b, c, d) => {
                // Split at t1, keep the left part, then split that at t0 / t1.
                let split = |a: PathPoint, b: PathPoint, c: PathPoint, d: PathPoint, t: f64| {
                    let (ab, bc, cd) = (lerp(a, b, t), lerp(b, c, t), lerp(c, d, t));
                    let (abc, bcd) = (lerp(ab, bc, t), lerp(bc, cd, t));
                    let m = lerp(abc, bcd, t);
                    ((a, ab, abc, m), (m, bcd, cd, d))
                };
                let (left, _) = split(a, b, c, d, t1);
                let u = if t1 > 0.0 { t0 / t1 } else { 0.0 };
                let (_, right) = split(left.0, left.1, left.2, left.3, u);
                Seg::Cubic(right.0, right.1, right.2, right.3)
            }
        }
    }
    fn map(&self, f: impl Fn(PathPoint) -> PathPoint) -> Seg {
        match *self {
            Seg::Line(a, b) => Seg::Line(f(a), f(b)),
            Seg::Cubic(a, b, c, d) => Seg::Cubic(f(a), f(b), f(c), f(d)),
        }
    }
    fn as_cubic(&self) -> (PathPoint, PathPoint, PathPoint, PathPoint) {
        match *self {
            Seg::Line(a, b) => (a, a, b, b),
            Seg::Cubic(a, b, c, d) => (a, b, c, d),
        }
    }
}

fn subpaths(path: &OwnedPath) -> Result<Vec<Sub>, Error> {
    path.validate(&PathLimits { max_elements: MAX_OP_ELEMENTS, require_closed: false })?;
    let mut out: Vec<Sub> = Vec::new();
    let mut cur: Option<Sub> = None;
    let mut pen = p(0.0, 0.0);
    for e in &path.elements {
        match *e {
            PathElement::MoveTo(q) => {
                if let Some(s) = cur.take() {
                    out.push(s);
                }
                cur = Some(Sub { segs: Vec::new(), start: q, closed: false });
                pen = q;
            }
            PathElement::LineTo(q) => {
                cur.as_mut().ok_or(Error::InvalidGeometry)?.segs.push(Seg::Line(pen, q));
                pen = q;
            }
            PathElement::QuadTo(c, q) => {
                let c1 = lerp(pen, c, 2.0 / 3.0);
                let c2 = lerp(q, c, 2.0 / 3.0);
                cur.as_mut().ok_or(Error::InvalidGeometry)?.segs.push(Seg::Cubic(pen, c1, c2, q));
                pen = q;
            }
            PathElement::CubicTo(a, b, q) => {
                cur.as_mut().ok_or(Error::InvalidGeometry)?.segs.push(Seg::Cubic(pen, a, b, q));
                pen = q;
            }
            PathElement::Close => {
                let mut s = cur.take().ok_or(Error::InvalidGeometry)?;
                if pen != s.start {
                    s.segs.push(Seg::Line(pen, s.start));
                }
                s.closed = true;
                pen = s.start;
                out.push(s);
            }
        }
    }
    if let Some(s) = cur.take() {
        out.push(s);
    }
    Ok(out)
}

fn emit(subs: &[Sub], fill_rule: FillRule) -> Result<OwnedPath, Error> {
    let mut path = OwnedPath::new(fill_rule);
    for s in subs {
        if s.segs.is_empty() {
            continue;
        }
        path.elements.push(PathElement::MoveTo(s.segs[0].start()));
        let n = s.segs.len();
        for (i, seg) in s.segs.iter().enumerate() {
            let last = i.checked_add(1) == Some(n);
            match *seg {
                Seg::Line(_, b) => {
                    if !(s.closed && last && b == s.segs[0].start()) {
                        path.elements.push(PathElement::LineTo(b));
                    }
                }
                Seg::Cubic(_, b, c, d) => path.elements.push(PathElement::CubicTo(b, c, d)),
            }
        }
        if s.closed {
            path.elements.push(PathElement::Close);
        }
        if path.elements.len() > MAX_OP_ELEMENTS {
            return Err(Error::BudgetExceeded);
        }
    }
    path.validate(&PathLimits { max_elements: MAX_OP_ELEMENTS, require_closed: false })?;
    Ok(path)
}

/// Reverses every subpath's direction.
pub fn reverse_path(path: &OwnedPath) -> Result<OwnedPath, Error> {
    let subs: Vec<Sub> = subpaths(path)?
        .into_iter()
        .map(|s| {
            let segs: Vec<Seg> = s
                .segs
                .iter()
                .rev()
                .map(|g| match *g {
                    Seg::Line(a, b) => Seg::Line(b, a),
                    Seg::Cubic(a, b, c, d) => Seg::Cubic(d, c, b, a),
                })
                .collect();
            let start = segs.first().map_or(s.start, Seg::start);
            Sub { segs, start, closed: s.closed }
        })
        .collect();
    emit(&subs, path.fill_rule)
}

/// `[a, b, c, d, e, f]` (SVG order) applied to every point.
pub fn transform_path(path: &OwnedPath, m: &Affine) -> Result<OwnedPath, Error> {
    if m.iter().any(|v| !v.is_finite()) {
        return Err(Error::InvalidRequest);
    }
    let f = |q: PathPoint| p(m[0] * q.x + m[2] * q.y + m[4], m[1] * q.x + m[3] * q.y + m[5]);
    let mut out = OwnedPath::new(path.fill_rule);
    for e in &path.elements {
        out.elements.push(match *e {
            PathElement::MoveTo(q) => PathElement::MoveTo(f(q)),
            PathElement::LineTo(q) => PathElement::LineTo(f(q)),
            PathElement::QuadTo(a, q) => PathElement::QuadTo(f(a), f(q)),
            PathElement::CubicTo(a, b, q) => PathElement::CubicTo(f(a), f(b), f(q)),
            PathElement::Close => PathElement::Close,
        });
    }
    out.validate(&PathLimits { max_elements: MAX_OP_ELEMENTS, require_closed: false })?;
    Ok(out)
}

/// `a . b` (apply `b` first).
pub fn affine_mul(a: &Affine, b: &Affine) -> Affine {
    [
        a[0] * b[0] + a[2] * b[1],
        a[1] * b[0] + a[3] * b[1],
        a[0] * b[2] + a[2] * b[3],
        a[1] * b[2] + a[3] * b[3],
        a[0] * b[4] + a[2] * b[5] + a[4],
        a[1] * b[4] + a[3] * b[5] + a[5],
    ]
}

/// Inverse of an affine (`None` when singular).
pub fn affine_inverse(m: &Affine) -> Option<Affine> {
    let det = m[0] * m[3] - m[1] * m[2];
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    let (a, b, c, d) = (m[3] / det, -m[1] / det, -m[2] / det, m[0] / det);
    Some([a, b, c, d, -(a * m[4] + c * m[5]), -(b * m[4] + d * m[5])])
}

/// translate(t) . rotate(deg, clockwise on y-down) . scale(s) about `anchor`.
pub fn anchored_transform(anchor: [f64; 2], translate: [f64; 2], rotation_deg: f64, scale: [f64; 2]) -> Affine {
    let (s, c) = sin_cos_deg(rotation_deg);
    let rs: Affine = [c * scale[0], s * scale[0], -s * scale[1], c * scale[1], 0.0, 0.0];
    let to_origin: Affine = [1.0, 0.0, 0.0, 1.0, -anchor[0], -anchor[1]];
    let back: Affine = [1.0, 0.0, 0.0, 1.0, anchor[0] + translate[0], anchor[1] + translate[1]];
    affine_mul(&back, &affine_mul(&rs, &to_origin))
}

/// Total arc length of every subpath.
pub fn path_length(path: &OwnedPath) -> Result<f64, Error> {
    Ok(subpaths(path)?.iter().flat_map(|s| s.segs.iter()).map(Seg::length).sum())
}

// ---------------------------------------------------------------------------------------------
// Generators

fn finish_generator(a: AnchorPath, reversed: bool) -> Result<OwnedPath, Error> {
    let path = a.to_owned_path().map_err(|_| Error::InvalidGeometry)?;
    if reversed { reverse_path(&path) } else { Ok(path) }
}

/// Rectangle path: centre `position`, `size`, corner `roundness` (radius, clamped to half the
/// shorter side as nib's rectangle reports).
pub fn rect_path(position: [f64; 2], size: [f64; 2], roundness: f64, reversed: bool) -> Result<OwnedPath, Error> {
    let (w, h) = (param("size", size[0])?, param("size", size[1])?);
    let r = param("roundness", roundness)?;
    let spec = RectSpec {
        x: param("position", position[0])? - w / 2.0,
        y: param("position", position[1])? - h / 2.0,
        width: w,
        height: h,
        corner_radius: [r.max(0.0); 4],
        corner_kind: if r > 0.0 { CornerKind::Round } else { CornerKind::None },
        corner_smoothing: 0.0,
    };
    let (a, _) = rectangle(&spec).map_err(|_| Error::InvalidRequest)?;
    finish_generator(a, reversed)
}

/// Ellipse path: centre `position`, `size` (diameters).
pub fn ellipse_path(position: [f64; 2], size: [f64; 2], reversed: bool) -> Result<OwnedPath, Error> {
    let spec = EllipseSpec::full(
        param("position", position[0])?,
        param("position", position[1])?,
        param("size", size[0])? / 2.0,
        param("size", size[1])? / 2.0,
    );
    finish_generator(ellipse(&spec).map_err(|_| Error::InvalidRequest)?, reversed)
}

/// Star or polygon.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Polystar {
    pub star: bool,
    /// Point count `3..=1000` (integer part used).
    pub points: f64,
    pub position: [f64; 2],
    /// Degrees; 0 puts the first outer point straight up.
    pub rotation: f64,
    pub outer_radius: f64,
    pub inner_radius: f64,
    /// Percent; handle length `roundness / 100 * pi * r / points` perpendicular to the radius
    /// (Studio rule, UNVERIFIED).
    pub outer_roundness: f64,
    pub inner_roundness: f64,
    pub reversed: bool,
}

/// Polystar path (struct docs).
pub fn polystar_path(s: &Polystar) -> Result<OwnedPath, Error> {
    let n = param("points", s.points)?.floor();
    if !(3.0..=MAX_POLYSTAR_POINTS).contains(&n) {
        return Err(Error::InvalidRequest);
    }
    let (cx, cy) = (param("position", s.position[0])?, param("position", s.position[1])?);
    let ro = param("outer_radius", s.outer_radius)?;
    let ri = param("inner_radius", s.inner_radius)?;
    for v in [s.rotation, s.outer_roundness, s.inner_roundness] {
        param("polystar", v)?;
    }
    if ro < 0.0 || ri < 0.0 {
        return Err(Error::InvalidRequest);
    }
    let count = if s.star { n * 2.0 } else { n };
    let step = 360.0 / count;
    let mut verts = Vec::new();
    let mut i = 0.0;
    while i < count {
        let outer = !s.star || (i % 2.0) == 0.0;
        let (r, round) = if outer { (ro, s.outer_roundness) } else { (ri, s.inner_roundness) };
        let (sn, cs) = sin_cos_deg(-90.0 + s.rotation + i * step);
        let v = p(cx + r * cs, cy + r * sn);
        let h = round / 100.0 * std::f64::consts::PI * r / n;
        let tangent = p(-sn * h, cs * h);
        verts.push((v, tangent));
        i += 1.0;
    }
    let mut segs = Vec::with_capacity(verts.len());
    for (k, (v, t)) in verts.iter().enumerate() {
        let (w, u) = verts[k.checked_add(1).and_then(|j| j.checked_rem(verts.len())).ok_or(Error::Overflow)?];
        if norm(*t) == 0.0 && norm(u) == 0.0 {
            segs.push(Seg::Line(*v, w));
        } else {
            segs.push(Seg::Cubic(*v, add(*v, *t), sub(w, u), w));
        }
    }
    let path = emit(&[Sub { start: verts[0].0, segs, closed: true }], FillRule::NonZero)?;
    if s.reversed { reverse_path(&path) } else { Ok(path) }
}

// ---------------------------------------------------------------------------------------------
// Trim paths

/// How trim paths treats several subpaths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrimMode {
    Simultaneously,
    Individually,
}

/// Normalised `[s, e]` ranges in `[0, 1]` (module docs); empty = nothing.
fn trim_ranges(start: f64, end: f64, offset_deg: f64) -> Vec<(f64, f64)> {
    let (mut s, mut e) = (start / 100.0, end / 100.0);
    if s > e {
        std::mem::swap(&mut s, &mut e);
    }
    let o = offset_deg / 360.0;
    s += o;
    e += o;
    if e - s >= 1.0 {
        return vec![(0.0, 1.0)];
    }
    if e <= s {
        return Vec::new();
    }
    let k = s.floor();
    s -= k;
    e -= k;
    if e <= 1.0 { vec![(s, e)] } else { vec![(s, 1.0), (0.0, e - 1.0)] }
}

/// Cut `[a, b]` (arc length) of one subpath into pieces (segment parts in order).
fn cut(sub: &Sub, lengths: &[f64], a: f64, b: f64) -> Vec<Seg> {
    let mut out = Vec::new();
    let mut at = 0.0;
    for (seg, len) in sub.segs.iter().zip(lengths) {
        let (s0, s1) = (at, at + len);
        at = s1;
        let lo = a.max(s0);
        let hi = b.min(s1);
        if hi <= lo || *len <= 0.0 {
            continue;
        }
        let t0 = seg.t_at(lo - s0, *len);
        let t1 = seg.t_at(hi - s0, *len);
        out.push(seg.piece(t0, t1));
    }
    out
}

fn trim_sub(sub: &Sub, ranges: &[(f64, f64)], total: f64, base: f64, all: f64) -> Vec<Sub> {
    // `ranges` are fractions of `all` starting at `base` (Individually) or of `total` (base 0).
    let lengths: Vec<f64> = sub.segs.iter().map(Seg::length).collect();
    let mut pieces: Vec<Vec<Seg>> = Vec::new();
    for (s, e) in ranges {
        let a = s * all - base;
        let b = e * all - base;
        let (a, b) = (a.max(0.0), b.min(total));
        if b > a {
            pieces.push(cut(sub, &lengths, a, b));
        }
    }
    // A closed subpath whose kept range wraps past its end continues from its start.
    if sub.closed && pieces.len() == 2 && ranges.len() == 2 && base == 0.0 {
        let mut joined = pieces[0].clone();
        joined.extend(pieces[1].iter().copied());
        return vec![Sub { start: joined.first().map_or(sub.start, Seg::start), segs: joined, closed: false }];
    }
    pieces
        .into_iter()
        .filter(|s| !s.is_empty())
        .map(|segs| Sub { start: segs[0].start(), segs, closed: false })
        .collect()
}

/// Trim paths (module docs) over one path (its subpaths).
pub fn trim_path(path: &OwnedPath, start: f64, end: f64, offset_deg: f64, mode: TrimMode) -> Result<OwnedPath, Error> {
    let mut out = trim_many(std::slice::from_ref(path), start, end, offset_deg, mode)?;
    Ok(out.pop().unwrap_or_else(|| OwnedPath::new(path.fill_rule)))
}

/// Trim paths over several paths (a shape group's paths, in order): `Simultaneously` trims every
/// subpath of every path by its own length, `Individually` trims the concatenation of all
/// subpaths of all paths. One result per input path.
pub fn trim_many(paths: &[OwnedPath], start: f64, end: f64, offset_deg: f64, mode: TrimMode) -> Result<Vec<OwnedPath>, Error> {
    let (start, end, offset) = (param("start", start)?, param("end", end)?, param("offset", offset_deg)?);
    let ranges = trim_ranges(start, end, offset);
    if ranges == [(0.0, 1.0)] {
        return Ok(paths.to_vec());
    }
    let all_subs: Vec<Vec<Sub>> = paths.iter().map(subpaths).collect::<Result<_, _>>()?;
    let all: f64 = all_subs.iter().flatten().flat_map(|s| s.segs.iter()).map(Seg::length).sum();
    let mut base = 0.0;
    let mut results = Vec::with_capacity(paths.len());
    for (path, subs) in paths.iter().zip(&all_subs) {
        let mut out = Vec::new();
        for s in subs {
            let total: f64 = s.segs.iter().map(Seg::length).sum();
            match mode {
                TrimMode::Simultaneously => out.extend(trim_sub(s, &ranges, total, 0.0, total)),
                TrimMode::Individually => {
                    out.extend(trim_sub(s, &ranges, total, base, all));
                    base += total;
                }
            }
        }
        results.push(emit(&out, path.fill_rule)?);
    }
    Ok(results)
}

// ---------------------------------------------------------------------------------------------
// Repeater

/// Repeater transform (module docs). Scale and opacities in percent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RepeaterTransform {
    pub anchor: [f64; 2],
    pub position: [f64; 2],
    pub scale: [f64; 2],
    pub rotation: f64,
    pub start_opacity: f64,
    pub end_opacity: f64,
}

fn step_power(t: &RepeaterTransform, k: f64) -> Affine {
    anchored_transform(
        t.anchor,
        [t.position[0] * k, t.position[1] * k],
        t.rotation * k,
        [(t.scale[0] / 100.0).powf(k), (t.scale[1] / 100.0).powf(k)],
    )
}

/// Per-copy transform and opacity factor (module docs), copy 0 first.
pub fn repeater(copies: f64, offset: f64, t: &RepeaterTransform) -> Result<Vec<(Affine, f64)>, Error> {
    let copies = param("copies", copies)?;
    let offset = param("offset", offset)?;
    if !(0.0..=MAX_REPEATER_COPIES).contains(&copies) || offset.abs() > MAX_REPEATER_COPIES {
        return Err(Error::BudgetExceeded);
    }
    for v in [t.anchor[0], t.anchor[1], t.position[0], t.position[1], t.scale[0], t.scale[1], t.rotation] {
        param("repeater", v)?;
    }
    if t.scale.iter().any(|s| *s <= 0.0)
        || ![t.start_opacity, t.end_opacity].iter().all(|o| (0.0..=100.0).contains(o))
    {
        return Err(Error::InvalidRequest);
    }
    let n = copies.ceil();
    let frac = copies - copies.floor();
    let step = step_power(t, 1.0);
    let inverse = affine_inverse(&step).ok_or(Error::InvalidRequest)?;
    let mut out = Vec::new();
    let mut i = 0.0;
    while i < n {
        let k = i + offset;
        let whole = k.floor();
        let mut m = step_power(t, k - whole);
        let unit = if whole >= 0.0 { &step } else { &inverse };
        let mut w = 0.0;
        while w < whole.abs() {
            m = affine_mul(unit, &m);
            w += 1.0;
        }
        let ramp = if n > 1.0 { i / (n - 1.0) } else { 0.0 };
        let mut opacity = (t.start_opacity + (t.end_opacity - t.start_opacity) * ramp) / 100.0;
        if frac > 0.0 && i == n - 1.0 {
            opacity *= frac;
        }
        out.push((m, opacity));
        i += 1.0;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Offset and merge

/// Offset paths (module docs).
pub fn offset_path(
    path: &OwnedPath,
    amount: f64,
    join: LineJoin,
    miter_limit: f64,
    copies: u32,
    options: &StrokeOptions,
) -> Result<OwnedPath, Error> {
    let amount = param("amount", amount)?;
    if copies == 0 || copies > MAX_OFFSET_COPIES {
        return Err(Error::BudgetExceeded);
    }
    let subs = subpaths(path)?;
    let closed: Vec<Sub> = subs.iter().filter(|s| s.closed).cloned().collect();
    let open: Vec<Sub> = subs.iter().filter(|s| !s.closed).cloned().collect();
    let closed_path = emit(&closed, path.fill_rule)?;
    let open_path = emit(&open, path.fill_rule)?;
    let mut parts: Vec<OwnedPath> = Vec::new();
    for k in 1..=copies {
        let a = amount * f64::from(k);
        if a == 0.0 {
            parts.push(path.clone());
            continue;
        }
        let style = StrokeStyle { join, miter_limit, ..StrokeStyle::new(2.0 * a.abs()) };
        if !closed_path.elements.is_empty() {
            let ring = outline_stroke(&closed_path, &style, options)?.path;
            let op = if a > 0.0 { BooleanOp::Union } else { BooleanOp::SubtractFront };
            match boolean(&[closed_path.clone(), ring], op, &options.geometry) {
                Ok(r) => parts.push(r.path),
                Err(Error::EmptyPathfinderResult) => {}
                Err(e) => return Err(e),
            }
        }
        if a > 0.0 && !open_path.elements.is_empty() {
            parts.push(outline_stroke(&open_path, &style, options)?.path);
        }
    }
    let mut out = OwnedPath::new(FillRule::NonZero);
    for part in parts {
        out.elements.extend(part.elements);
        if out.elements.len() > MAX_OP_ELEMENTS {
            return Err(Error::BudgetExceeded);
        }
    }
    Ok(out)
}

/// Merge-paths mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeMode {
    Merge,
    Add,
    Subtract,
    Intersect,
    ExcludeIntersections,
}

/// Merge paths (module docs); `paths` in accumulation order (the first is the minuend).
pub fn merge_paths(paths: &[OwnedPath], mode: MergeMode, options: &BooleanOptions) -> Result<OwnedPath, Error> {
    if mode == MergeMode::Merge || paths.len() < 2 {
        let mut out = OwnedPath::new(paths.first().map_or(FillRule::NonZero, |p| p.fill_rule));
        for path in paths {
            out.elements.extend(path.elements.iter().copied());
            if out.elements.len() > MAX_OP_ELEMENTS {
                return Err(Error::BudgetExceeded);
            }
        }
        if mode != MergeMode::Merge && paths.len() == 1 {
            return Ok(normalise_path(&out, options)?.path);
        }
        return Ok(out);
    }
    let op = match mode {
        MergeMode::Add | MergeMode::Merge => BooleanOp::Union,
        MergeMode::Subtract => BooleanOp::SubtractFront,
        MergeMode::Intersect => BooleanOp::Intersect,
        MergeMode::ExcludeIntersections => BooleanOp::Exclude,
    };
    match boolean(paths, op, options) {
        Ok(r) => Ok(r.path),
        Err(Error::EmptyPathfinderResult) => Ok(OwnedPath::new(FillRule::NonZero)),
        Err(e) => Err(e),
    }
}

// ---------------------------------------------------------------------------------------------
// Point modifiers

fn centroid(sub: &Sub) -> PathPoint {
    let n = sub.segs.len().max(1) as f64;
    let sum = sub.segs.iter().fold(p(0.0, 0.0), |acc, s| add(acc, s.start()));
    mul(sum, 1.0 / n)
}

/// Pucker & bloat (module docs), `amount` percent.
pub fn pucker_bloat(path: &OwnedPath, amount: f64) -> Result<OwnedPath, Error> {
    let a = param("amount", amount)? / 100.0;
    let subs: Vec<Sub> = subpaths(path)?
        .into_iter()
        .map(|s| {
            let c = centroid(&s);
            let vertex = |q: PathPoint| add(q, mul(sub(c, q), a));
            let control = |q: PathPoint| add(q, mul(sub(c, q), -a));
            let segs: Vec<Seg> = s
                .segs
                .iter()
                .map(|g| {
                    let (p0, p1, p2, p3) = g.as_cubic();
                    Seg::Cubic(vertex(p0), control(p1), control(p2), vertex(p3))
                })
                .collect();
            Sub { start: vertex(s.start), segs, closed: s.closed }
        })
        .collect();
    emit(&subs, path.fill_rule)
}

/// Round corners (module docs).
pub fn round_corners(path: &OwnedPath, radius: f64) -> Result<OwnedPath, Error> {
    let radius = param("radius", radius)?;
    if radius < 0.0 {
        return Err(Error::InvalidRequest);
    }
    let mut out = Vec::new();
    for s in subpaths(path)? {
        let n = s.segs.len();
        if radius == 0.0 || n < 2 {
            out.push(s);
            continue;
        }
        // Cut amounts at each vertex i (between seg i-1 and seg i).
        let mut cut_in = vec![0.0; n];
        let mut cut_out = vec![0.0; n];
        let mut handle = vec![0.0; n];
        for i in 0..n {
            if i == 0 && !s.closed {
                continue;
            }
            let prev = s.segs[wrap_prev(i, n)];
            let next = s.segs[i];
            let (Seg::Line(a, v), Seg::Line(_, b)) = (prev, next) else { continue };
            let (lin, lout) = (norm(sub(v, a)), norm(sub(b, v)));
            if lin <= 0.0 || lout <= 0.0 {
                continue;
            }
            let (ui, uo) = (mul(sub(v, a), 1.0 / lin), mul(sub(b, v), 1.0 / lout));
            let cos_phi = (ui.x * uo.x + ui.y * uo.y).clamp(-1.0, 1.0);
            if cos_phi >= 1.0 - 1e-12 || cos_phi <= -1.0 + 1e-12 {
                continue;
            }
            let d = radius.min(lin / 2.0).min(lout / 2.0);
            let tan_half = ((1.0 - cos_phi) / (1.0 + cos_phi)).sqrt();
            let r = d / tan_half;
            let (s_half, c_half) = (((1.0 - cos_phi) / 2.0).sqrt(), ((1.0 + cos_phi) / 2.0).sqrt());
            let tan_quarter = s_half / (1.0 + c_half);
            cut_in[i] = d;
            cut_out[i] = d;
            handle[i] = 4.0 / 3.0 * tan_quarter * r;
        }
        let mut segs = Vec::new();
        for i in 0..n {
            let seg = s.segs[i];
            let j = wrap_next(i, n);
            let (a, b) = (seg.start(), seg.end());
            let a2 = if cut_out[i] > 0.0 { add(a, mul(sub(b, a), cut_out[i] / norm(sub(b, a)))) } else { a };
            let b2 = if (j != 0 || s.closed) && cut_in[j] > 0.0 { add(b, mul(sub(a, b), cut_in[j] / norm(sub(b, a)))) } else { b };
            match seg {
                Seg::Line(..) => segs.push(Seg::Line(a2, b2)),
                cubic => segs.push(cubic),
            }
            if (j != 0 || s.closed) && cut_in[j] > 0.0 {
                let next = s.segs[j];
                let (nb, nc) = (next.start(), next.end());
                let ui = mul(sub(b, a), 1.0 / norm(sub(b, a)));
                let uo = mul(sub(nc, nb), 1.0 / norm(sub(nc, nb)));
                let c0 = b2;
                let c3 = add(nb, mul(uo, cut_out[j]));
                segs.push(Seg::Cubic(c0, add(c0, mul(ui, handle[j])), sub(c3, mul(uo, handle[j])), c3));
            }
        }
        // A closed path whose first vertex was rounded starts at the cut point.
        let start = segs.first().map_or(s.start, Seg::start);
        out.push(Sub { segs, start, closed: s.closed });
    }
    emit(&out, path.fill_rule)
}

fn wrap_prev(i: usize, n: usize) -> usize {
    if i == 0 { n.saturating_sub(1) } else { i.saturating_sub(1) }
}

fn wrap_next(i: usize, n: usize) -> usize {
    let j = i.saturating_add(1);
    if j >= n { 0 } else { j }
}

/// Points at `pieces` equal arc-length steps along a segment (end point excluded).
fn spaced_points(seg: &Seg, pieces: u32) -> Vec<(PathPoint, PathPoint)> {
    let total = seg.length();
    (0..pieces)
        .map(|j| {
            let t = seg.t_at(total * f64::from(j) / f64::from(pieces), total);
            let d = seg.derivative(t);
            let l = norm(d);
            let tangent = if l > 0.0 { mul(d, 1.0 / l) } else { p(1.0, 0.0) };
            (seg.at(t), tangent)
        })
        .collect()
}

/// Catmull-Rom cubics through `pts` (closed wraps).
fn catmull(pts: &[PathPoint], closed: bool) -> Vec<Seg> {
    let n = pts.len();
    let count = if closed { n } else { n.saturating_sub(1) };
    let last = n.saturating_sub(1);
    (0..count)
        .map(|i| {
            let prev = if i == 0 { if closed { last } else { 0 } } else { i.saturating_sub(1) };
            let next = |k: usize| {
                let j = k.saturating_add(1);
                if j <= last { j } else if closed { 0 } else { last }
            };
            let (i1, i2) = (i, next(i));
            let i3 = next(i2);
            let (p0, p1, p2, p3) = (pts[prev], pts[i1], pts[i2], pts[i3]);
            Seg::Cubic(p1, add(p1, mul(sub(p2, p0), 1.0 / 6.0)), sub(p2, mul(sub(p3, p1), 1.0 / 6.0)), p2)
        })
        .collect()
}

fn polyline_sub(pts: Vec<PathPoint>, closed: bool, smooth: bool) -> Sub {
    let segs = if smooth {
        catmull(&pts, closed)
    } else {
        let mut v: Vec<Seg> = pts.windows(2).map(|w| Seg::Line(w[0], w[1])).collect();
        if closed && pts.len() > 1 {
            v.push(Seg::Line(pts[pts.len().saturating_sub(1)], pts[0]));
        }
        v
    };
    Sub { start: pts.first().copied().unwrap_or(p(0.0, 0.0)), segs, closed }
}

fn budget(subs: &[Sub], per_segment: u32) -> Result<(), Error> {
    let segs: usize = subs.iter().map(|s| s.segs.len()).sum();
    let points = (segs as u64).saturating_mul(u64::from(per_segment).saturating_add(1));
    if points > MAX_OP_ELEMENTS as u64 {
        return Err(Error::BudgetExceeded);
    }
    Ok(())
}

/// Zig zag (module docs).
pub fn zig_zag(path: &OwnedPath, size: f64, ridges: u32, smooth: bool) -> Result<OwnedPath, Error> {
    let size = param("size", size)?;
    if ridges > MAX_POINTS_PER_SEGMENT {
        return Err(Error::BudgetExceeded);
    }
    let subs = subpaths(path)?;
    budget(&subs, ridges)?;
    let mut out = Vec::new();
    for s in subs {
        let mut pts = Vec::new();
        for seg in &s.segs {
            for (j, (q, t)) in spaced_points(seg, ridges.saturating_add(1)).into_iter().enumerate() {
                if j == 0 {
                    pts.push(q);
                } else {
                    let sign = if j.is_multiple_of(2) { -1.0 } else { 1.0 };
                    pts.push(add(q, mul(p(-t.y, t.x), size * sign)));
                }
            }
        }
        if !s.closed && let Some(last) = s.segs.last() {
            pts.push(last.end());
        }
        out.push(polyline_sub(pts, s.closed, smooth));
    }
    emit(&out, path.fill_rule)
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Seeded smoothstep value noise in `-1..=1` (lattice values from splitmix64).
pub fn path_noise(seed: u64, stream: u64, x: f64) -> f64 {
    let x0 = x.floor();
    let f = x - x0;
    let k = x0 as i64 as u64;
    let lattice = |k: u64| {
        let h = splitmix64(seed ^ splitmix64(stream ^ splitmix64(k)));
        (h >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    };
    let (a, b) = (lattice(k), lattice(k.wrapping_add(1)));
    a + (b - a) * (f * f * (3.0 - 2.0 * f))
}

/// Wiggle paths parameters (module docs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WiggleParams {
    pub size: f64,
    pub detail: u32,
    pub smooth: bool,
    pub time_s: f64,
    pub wiggles_per_second: f64,
    /// Percent 0..=100.
    pub correlation: f64,
    pub temporal_phase: f64,
    pub spatial_phase: f64,
    pub seed: u64,
}

/// Wiggle paths (module docs).
pub fn wiggle_path(path: &OwnedPath, w: &WiggleParams) -> Result<OwnedPath, Error> {
    for v in [w.size, w.time_s, w.wiggles_per_second, w.correlation, w.temporal_phase, w.spatial_phase] {
        param("wiggle", v)?;
    }
    if w.detail > MAX_POINTS_PER_SEGMENT || !(0.0..=100.0).contains(&w.correlation) {
        return Err(Error::InvalidRequest);
    }
    let subs = subpaths(path)?;
    budget(&subs, w.detail)?;
    let x = w.time_s * w.wiggles_per_second + w.temporal_phase / 360.0;
    let c = w.correlation / 100.0;
    let mut out = Vec::new();
    let mut index = 0u64;
    for s in subs {
        let mut pts = Vec::new();
        let mut raw: Vec<PathPoint> = Vec::new();
        for seg in &s.segs {
            raw.extend(spaced_points(seg, w.detail.saturating_add(1)).into_iter().map(|(q, _)| q));
        }
        if !s.closed && let Some(last) = s.segs.last() {
            raw.push(last.end());
        }
        for q in raw {
            let own_x = x + index as f64 * w.spatial_phase / 360.0;
            let stream = index.wrapping_mul(2).wrapping_add(2);
            let nx = c * path_noise(w.seed, 0, x) + (1.0 - c) * path_noise(w.seed, stream, own_x);
            let ny = c * path_noise(w.seed, 1, x) + (1.0 - c) * path_noise(w.seed, stream.wrapping_add(1), own_x);
            pts.push(add(q, p(nx * w.size, ny * w.size)));
            index = index.wrapping_add(1);
        }
        out.push(polyline_sub(pts, s.closed, w.smooth));
    }
    emit(&out, path.fill_rule)
}

/// Twist (module docs), `angle` degrees.
pub fn twist(path: &OwnedPath, angle: f64, center: [f64; 2]) -> Result<OwnedPath, Error> {
    let angle = param("angle", angle)?;
    let c = p(param("center", center[0])?, param("center", center[1])?);
    let subs = subpaths(path)?;
    let r_max = subs
        .iter()
        .flat_map(|s| s.segs.iter())
        .map(|g| norm(sub(g.start(), c)).max(norm(sub(g.end(), c))))
        .fold(0.0, f64::max);
    if r_max <= 0.0 {
        return Ok(path.clone());
    }
    let f = |q: PathPoint| {
        let d = norm(sub(q, c));
        let theta = angle * (1.0 - d / r_max).max(0.0);
        let (s, co) = sin_cos_deg(theta);
        let v = sub(q, c);
        add(c, p(v.x * co - v.y * s, v.x * s + v.y * co))
    };
    let out: Vec<Sub> = subs
        .into_iter()
        .map(|s| {
            let segs: Vec<Seg> = s.segs.iter().map(|g| {
                let (a, b, cc, d) = g.as_cubic();
                Seg::Cubic(a, b, cc, d).map(f)
            }).collect();
            Sub { start: f(s.start), segs, closed: s.closed }
        })
        .collect();
    emit(&out, path.fill_rule)
}
