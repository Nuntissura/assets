//! The declared Studio laws v1 of the live-effect operators (parent module docs).
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_accord::CancellationToken;

use super::{EffectRecord, LiveEffectError, PT_PER_INCH, FREE_DISTORT_MAX_DEPTH, FREE_DISTORT_TOLERANCE_PT, REFERENCE, Result, check_cancel};
use crate::arrange::{Bounds, ReferencePoint, union_bounds, unit_random};
use crate::owned_path::{COORDINATE_LIMIT, OwnedPath, PathPoint};
use crate::path_contract::Error;
use crate::raster::Affine;
use crate::shape::sin_cos_deg;
use crate::shape_ops::{
    MAX_OP_ELEMENTS, MAX_POINTS_PER_SEGMENT, Seg, Sub, affine_mul, emit, polyline_sub, pucker_bloat,
    spaced_points, subpaths, transform_path, zig_zag,
};

/// Operator errors after the sources were validated: results beyond range or budget.
fn result_error(e: Error) -> LiveEffectError {
    match e {
        Error::BudgetExceeded | Error::Overflow => LiveEffectError::OutputOverBudget,
        _ => LiveEffectError::BeyondEngineRange,
    }
}

fn in_range(v: f64) -> Result<f64> {
    if v.is_finite() && v.abs() <= COORDINATE_LIMIT { Ok(v) } else { Err(LiveEffectError::BeyondEngineRange) }
}

fn bounds_of(sources: &[OwnedPath]) -> Option<Bounds> {
    union_bounds(sources)
}

fn diagonal(b: Option<Bounds>) -> f64 {
    b.map_or(0.0, |b| (b.width() * b.width() + b.height() * b.height()).sqrt())
}

fn relative(mode: &str) -> bool {
    mode == "mode.relative"
}

fn smooth(points: &str) -> bool {
    points == "points.smooth"
}

fn pt(x: f64, y: f64) -> PathPoint {
    PathPoint::new(x, y)
}

fn all_subpaths(sources: &[OwnedPath]) -> Result<Vec<Vec<Sub>>> {
    sources.iter().map(|s| subpaths(s).map_err(|_| LiveEffectError::InvalidGeometry)).collect()
}

/// Uniform in `[-1, 1)`.
fn signed_random(seed: u64, stream: u64, k: u64) -> f64 {
    2.0 * unit_random(seed, stream, k) - 1.0
}

fn roughen_pieces(seg: &Seg, detail: f64) -> u32 {
    let n = (detail * seg.length() / PT_PER_INCH).ceil();
    if n.is_nan() || n < 1.0 {
        1
    } else if n > MAX_OP_ELEMENTS as f64 {
        u32::MAX
    } else {
        n as u32
    }
}

pub(super) fn roughen(sources: &[OwnedPath], r: &EffectRecord, cancel: &CancellationToken) -> Result<Vec<OwnedPath>> {
    let rel = relative(r.token("roughen_mode")?);
    let raw = r.real(if rel { "roughen_size_relative" } else { "roughen_size_absolute" })?;
    let detail = r.real("roughen_detail")?;
    let smooth = smooth(r.token("roughen_points")?);
    let seed = r.seed()?;
    let size = in_range(if rel { raw / 100.0 * diagonal(bounds_of(sources)) } else { raw })?;
    let all = all_subpaths(sources)?;
    // Running aggregate of output points before any sampling (V4-02).
    let mut points = 0_u64;
    for s in all.iter().flatten() {
        for seg in &s.segs {
            points = points.saturating_add(u64::from(roughen_pieces(seg, detail)));
        }
        points = points.saturating_add(1);
    }
    if points > MAX_OP_ELEMENTS as u64 {
        return Err(LiveEffectError::OutputOverBudget);
    }
    let mut out = Vec::with_capacity(sources.len());
    let mut stream = 0_u64;
    for (path, subs) in sources.iter().zip(&all) {
        check_cancel(cancel)?;
        let mut moved_subs = Vec::with_capacity(subs.len());
        for s in subs {
            let mut pts: Vec<(PathPoint, PathPoint)> = Vec::new();
            for seg in &s.segs {
                pts.extend(spaced_points(seg, roughen_pieces(seg, detail)));
            }
            if !s.closed && let Some(last) = s.segs.last() {
                pts.push((last.end(), pt(1.0, 0.0)));
            }
            let last = pts.len().saturating_sub(1);
            let moved: Vec<PathPoint> = pts
                .iter()
                .enumerate()
                .map(|(k, (q, t))| {
                    if !s.closed && (k == 0 || k == last) {
                        return *q;
                    }
                    let sign = if k.is_multiple_of(2) { 1.0 } else { -1.0 };
                    let d = size * sign * unit_random(seed, stream, k as u64);
                    pt(q.x - t.y * d, q.y + t.x * d)
                })
                .collect();
            if !moved.is_empty() {
                moved_subs.push(polyline_sub(moved, s.closed, smooth));
            }
            stream = stream.wrapping_add(1);
        }
        out.push(emit(&moved_subs, path.fill_rule).map_err(result_error)?);
    }
    Ok(out)
}

fn segment_count(all: &[Vec<Sub>]) -> u64 {
    all.iter().flatten().map(|s| s.segs.len() as u64).fold(0, u64::saturating_add)
}

pub(super) fn zig_zag_live(sources: &[OwnedPath], r: &EffectRecord, cancel: &CancellationToken) -> Result<Vec<OwnedPath>> {
    let rel = relative(r.token("zigzag_mode")?);
    let raw = r.real("zigzag_size")?;
    let ridges = r.real("zigzag_ridges_per_segment")?;
    let smooth = smooth(r.token("zigzag_points")?);
    if ridges > f64::from(MAX_POINTS_PER_SEGMENT) {
        return Err(LiveEffectError::OutputOverBudget);
    }
    let ridges = ridges as u32;
    let size = in_range(if rel { raw / 100.0 * diagonal(bounds_of(sources)) } else { raw })?;
    let all = all_subpaths(sources)?;
    if segment_count(&all).saturating_mul(u64::from(ridges).saturating_add(1)) > MAX_OP_ELEMENTS as u64 {
        return Err(LiveEffectError::OutputOverBudget);
    }
    let mut out = Vec::with_capacity(sources.len());
    for path in sources {
        check_cancel(cancel)?;
        out.push(zig_zag(path, size, ridges, smooth).map_err(result_error)?);
    }
    Ok(out)
}

pub(super) fn pucker_bloat_live(sources: &[OwnedPath], r: &EffectRecord, cancel: &CancellationToken) -> Result<Vec<OwnedPath>> {
    let factor = in_range(r.real("pucker_bloat_factor")?)?;
    let mut out = Vec::with_capacity(sources.len());
    for path in sources {
        check_cancel(cancel)?;
        out.push(pucker_bloat(path, factor).map_err(result_error)?);
    }
    Ok(out)
}

fn raise(seg: &Seg) -> Seg {
    match *seg {
        Seg::Line(a, b) => {
            let at = |t: f64| pt(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);
            Seg::Cubic(a, at(1.0 / 3.0), at(2.0 / 3.0), b)
        }
        cubic => cubic,
    }
}

pub(super) fn twist_live(sources: &[OwnedPath], r: &EffectRecord, cancel: &CancellationToken) -> Result<Vec<OwnedPath>> {
    let angle = r.real("twist_angle")?;
    let Some(b) = bounds_of(sources) else {
        return Ok(sources.to_vec());
    };
    let c = b.centre();
    let all = all_subpaths(sources)?;
    let dist = |q: PathPoint| ((q.x - c[0]) * (q.x - c[0]) + (q.y - c[1]) * (q.y - c[1])).sqrt();
    let r_max = all
        .iter()
        .flatten()
        .flat_map(|s| s.segs.iter())
        .map(|g| dist(g.start()).max(dist(g.end())))
        .fold(0.0, f64::max);
    if r_max.is_nan() || r_max <= 0.0 {
        return Err(LiveEffectError::DegenerateExtent);
    }
    let f = |q: PathPoint| {
        let theta = angle * (1.0 - dist(q) / r_max).max(0.0);
        let (s, co) = sin_cos_deg(theta);
        let (vx, vy) = (q.x - c[0], q.y - c[1]);
        pt(c[0] + vx * co - vy * s, c[1] + vx * s + vy * co)
    };
    let mut out = Vec::with_capacity(sources.len());
    for (path, subs) in sources.iter().zip(&all) {
        check_cancel(cancel)?;
        let mapped: Vec<Sub> = subs
            .iter()
            .map(|s| {
                let segs: Vec<Seg> = s
                    .segs
                    .iter()
                    .map(|g| {
                        let (a, b, cc, d) = raise(g).as_cubic();
                        Seg::Cubic(f(a), f(b), f(cc), f(d))
                    })
                    .collect();
                Sub { start: f(s.start), segs, closed: s.closed }
            })
            .collect();
        out.push(emit(&mapped, path.fill_rule).map_err(result_error)?);
    }
    Ok(out)
}

pub(super) fn tweak(sources: &[OwnedPath], r: &EffectRecord, cancel: &CancellationToken) -> Result<Vec<OwnedPath>> {
    let rel = relative(r.token("tweak_mode")?);
    let (h, v) = (r.real("tweak_horizontal")?, r.real("tweak_vertical")?);
    let anchors_on = r.flag("modify_anchor_points")?;
    let in_on = r.flag("modify_in_control_points")?;
    let out_on = r.flag("modify_out_control_points")?;
    let seed = r.seed()?;
    let b = bounds_of(sources);
    let (w, ht) = b.map_or((0.0, 0.0), |b| (b.width(), b.height()));
    let (hx, vy) = if rel { (h / 100.0 * w, v / 100.0 * ht) } else { (h, v) };
    let (hx, vy) = (in_range(hx)?, in_range(vy)?);
    let all = all_subpaths(sources)?;
    let mut out = Vec::with_capacity(sources.len());
    let mut sub_index = 0_u64;
    for (path, subs) in sources.iter().zip(&all) {
        check_cancel(cancel)?;
        let mut moved = Vec::with_capacity(subs.len());
        for s in subs {
            let base = sub_index.wrapping_mul(8);
            let offset = |on: bool, c: u64, k: usize| {
                if on {
                    let k = k as u64;
                    pt(hx * signed_random(seed, base.wrapping_add(c), k), vy * signed_random(seed, base.wrapping_add(c).wrapping_add(1), k))
                } else {
                    pt(0.0, 0.0)
                }
            };
            let n = s.segs.len();
            let mut segs = Vec::with_capacity(n);
            for (i, seg) in s.segs.iter().enumerate() {
                let next = i.saturating_add(1);
                let kb = if s.closed && next == n { 0 } else { next };
                let (da, db) = (offset(anchors_on, 0, i), offset(anchors_on, 0, kb));
                let (a, c1, c2, e) = seg.as_cubic();
                let shift = |q: PathPoint, d: PathPoint| pt(q.x + d.x, q.y + d.y);
                let (a2, e2) = (shift(a, da), shift(e, db));
                let straight = matches!(seg, Seg::Line(..)) && !in_on && !out_on;
                segs.push(if straight {
                    Seg::Line(a2, e2)
                } else {
                    Seg::Cubic(a2, shift(shift(c1, da), offset(out_on, 4, i)), shift(shift(c2, db), offset(in_on, 2, kb)), e2)
                });
            }
            let start = segs.first().map_or(s.start, Seg::start);
            moved.push(Sub { segs, start, closed: s.closed });
            sub_index = sub_index.wrapping_add(1);
        }
        out.push(emit(&moved, path.fill_rule).map_err(result_error)?);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Free distort

fn cross(o: PathPoint, a: PathPoint, b: PathPoint) -> f64 {
    (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x)
}

fn segments_cross(a: PathPoint, b: PathPoint, c: PathPoint, d: PathPoint) -> bool {
    let (d1, d2) = (cross(a, b, c), cross(a, b, d));
    let (d3, d4) = (cross(c, d, a), cross(c, d, b));
    ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0)) && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
}

/// Degenerate (repeated corner, collinear triple), self-crossing and non-convex quads are refused.
fn check_quad(q: &[PathPoint; 4]) -> Result<()> {
    let len = |a: PathPoint, b: PathPoint| ((b.x - a.x) * (b.x - a.x) + (b.y - a.y) * (b.y - a.y)).sqrt();
    let mut turns = [0.0; 4];
    for (turn, [a, b, c]) in turns.iter_mut().zip([[q[0], q[1], q[2]], [q[1], q[2], q[3]], [q[2], q[3], q[0]], [q[3], q[0], q[1]]]) {
        let (l1, l2) = (len(a, b), len(b, c));
        let z = cross(a, b, c);
        if !(l1 > 0.0 && l2 > 0.0) || z.abs() <= 1e-12 * l1 * l2 {
            return Err(LiveEffectError::DegenerateQuad);
        }
        *turn = z;
    }
    if segments_cross(q[0], q[1], q[2], q[3]) || segments_cross(q[1], q[2], q[3], q[0]) {
        return Err(LiveEffectError::SelfCrossingQuad);
    }
    if !(turns.iter().all(|t| *t > 0.0) || turns.iter().all(|t| *t < 0.0)) {
        return Err(LiveEffectError::QuadNotConvex);
    }
    Ok(())
}

type Mat3 = [f64; 9];

/// Unit square `(0,0) (1,0) (1,1) (0,1)` onto `q0..q3` (Heckbert), row-major on `(u, v, 1)`.
fn square_to_quad(q: &[PathPoint; 4]) -> Mat3 {
    let [p0, p1, p2, p3] = *q;
    let (dx1, dx2, dx3) = (p1.x - p2.x, p3.x - p2.x, p0.x - p1.x + p2.x - p3.x);
    let (dy1, dy2, dy3) = (p1.y - p2.y, p3.y - p2.y, p0.y - p1.y + p2.y - p3.y);
    let den = dx1 * dy2 - dx2 * dy1;
    let g = (dx3 * dy2 - dx2 * dy3) / den;
    let h = (dx1 * dy3 - dx3 * dy1) / den;
    [
        p1.x - p0.x + g * p1.x, p3.x - p0.x + h * p3.x, p0.x,
        p1.y - p0.y + g * p1.y, p3.y - p0.y + h * p3.y, p0.y,
        g, h, 1.0,
    ]
}

fn adjugate(m: &Mat3) -> Mat3 {
    let [a, b, c, d, e, f, g, h, i] = *m;
    [
        e * i - f * h, c * h - b * i, b * f - c * e,
        f * g - d * i, a * i - c * g, c * d - a * f,
        d * h - e * g, b * g - a * h, a * e - b * d,
    ]
}

fn mul3(a: &Mat3, b: &Mat3) -> Mat3 {
    let [a0, a1, a2, a3, a4, a5, a6, a7, a8] = *a;
    let [b0, b1, b2, b3, b4, b5, b6, b7, b8] = *b;
    [
        a0 * b0 + a1 * b3 + a2 * b6, a0 * b1 + a1 * b4 + a2 * b7, a0 * b2 + a1 * b5 + a2 * b8,
        a3 * b0 + a4 * b3 + a5 * b6, a3 * b1 + a4 * b4 + a5 * b7, a3 * b2 + a4 * b5 + a5 * b8,
        a6 * b0 + a7 * b3 + a8 * b6, a6 * b1 + a7 * b4 + a8 * b7, a6 * b2 + a7 * b5 + a8 * b8,
    ]
}

struct Homography(Mat3);

impl Homography {
    fn new(src: &[PathPoint; 4], dst: &[PathPoint; 4]) -> Result<Self> {
        let mut m = mul3(&square_to_quad(dst), &adjugate(&square_to_quad(src)));
        if m.iter().any(|v| !v.is_finite()) {
            return Err(LiveEffectError::DegenerateQuad);
        }
        // Points inside the source quad map with a positive weight.
        let cx = (src[0].x + src[1].x + src[2].x + src[3].x) / 4.0;
        let cy = (src[0].y + src[1].y + src[2].y + src[3].y) / 4.0;
        if m[6] * cx + m[7] * cy + m[8] < 0.0 {
            m.iter_mut().for_each(|v| *v = -*v);
        }
        Ok(Self(m))
    }
    fn map(&self, q: PathPoint) -> Result<PathPoint> {
        let m = &self.0;
        let w = m[6] * q.x + m[7] * q.y + m[8];
        if w.is_nan() || w <= 0.0 {
            return Err(LiveEffectError::PointBeyondHorizon);
        }
        Ok(pt(in_range((m[0] * q.x + m[1] * q.y + m[2]) / w)?, in_range((m[3] * q.x + m[4] * q.y + m[5]) / w)?))
    }
}

type Cubic = (PathPoint, PathPoint, PathPoint, PathPoint);

fn halves(c: Cubic) -> (Cubic, Cubic) {
    let mid = |a: PathPoint, b: PathPoint| pt((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
    let (a, b, cc, d) = c;
    let (ab, bc, cd) = (mid(a, b), mid(b, cc), mid(cc, d));
    let (abc, bcd) = (mid(ab, bc), mid(bc, cd));
    let m = mid(abc, bcd);
    ((a, ab, abc, m), (m, bcd, cd, d))
}

fn map_cubic(h: &Homography, c: Cubic, depth: u32, out: &mut Vec<Seg>, elements: &mut usize) -> Result<()> {
    let (a, b, cc, d) = c;
    let mapped = Seg::Cubic(h.map(a)?, h.map(b)?, h.map(cc)?, h.map(d)?);
    let mut close = depth >= FREE_DISTORT_MAX_DEPTH;
    if !close {
        close = true;
        let source = Seg::Cubic(a, b, cc, d);
        for t in [0.25, 0.5, 0.75] {
            let exact = h.map(source.at(t))?;
            let approx = mapped.at(t);
            let (dx, dy) = (exact.x - approx.x, exact.y - approx.y);
            if (dx * dx + dy * dy).sqrt() > FREE_DISTORT_TOLERANCE_PT {
                close = false;
                break;
            }
        }
    }
    if close {
        *elements = elements.saturating_add(1);
        if *elements > MAX_OP_ELEMENTS {
            return Err(LiveEffectError::OutputOverBudget);
        }
        out.push(mapped);
        return Ok(());
    }
    let (left, right) = halves(c);
    map_cubic(h, left, depth.saturating_add(1), out, elements)?;
    map_cubic(h, right, depth.saturating_add(1), out, elements)
}

pub(super) fn free_distort(sources: &[OwnedPath], r: &EffectRecord, cancel: &CancellationToken) -> Result<Vec<OwnedPath>> {
    let (src, dst) = (r.quad(0)?, r.quad(1)?);
    check_quad(&src)?;
    check_quad(&dst)?;
    if src == dst {
        return Ok(sources.to_vec());
    }
    let h = Homography::new(&src, &dst)?;
    let all = all_subpaths(sources)?;
    let mut elements = 0_usize;
    let mut out = Vec::with_capacity(sources.len());
    for (path, subs) in sources.iter().zip(&all) {
        check_cancel(cancel)?;
        let mut mapped = Vec::with_capacity(subs.len());
        for s in subs {
            let mut segs = Vec::with_capacity(s.segs.len());
            for seg in &s.segs {
                match *seg {
                    Seg::Line(a, b) => {
                        elements = elements.saturating_add(1);
                        if elements > MAX_OP_ELEMENTS {
                            return Err(LiveEffectError::OutputOverBudget);
                        }
                        segs.push(Seg::Line(h.map(a)?, h.map(b)?));
                    }
                    Seg::Cubic(a, b, c, d) => map_cubic(&h, (a, b, c, d), 0, &mut segs, &mut elements)?,
                }
            }
            mapped.push(Sub { start: h.map(s.start)?, segs, closed: s.closed });
        }
        out.push(emit(&mapped, path.fill_rule).map_err(result_error)?);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Transform (step-repeat)

fn reference(token: &str) -> Result<ReferencePoint> {
    let index = REFERENCE.iter().position(|m| m.id == token).ok_or(LiveEffectError::UnknownEnumMember)?;
    let wire = u8::try_from(index.saturating_add(1)).map_err(|_| LiveEffectError::UnknownEnumMember)?;
    ReferencePoint::from_wire(wire).ok_or(LiveEffectError::UnknownEnumMember)
}

struct StepParams {
    move_h: f64,
    move_v: f64,
    scale_h: f64,
    scale_v: f64,
    rotate: f64,
    reflect_x: bool,
    reflect_y: bool,
    random: bool,
    seed: u64,
}

impl StepParams {
    /// `translate(move) . about(anchor, rotate . scale . reflect)` for copy `k`.
    fn step(&self, anchor: [f64; 2], k: u64) -> Affine {
        let amount = |stream: u64, identity: f64, value: f64| {
            if self.random { identity + (value - identity) * unit_random(self.seed, stream, k) } else { value }
        };
        let sx = amount(1, 100.0, self.scale_h) / 100.0 * if self.reflect_x { -1.0 } else { 1.0 };
        let sy = amount(2, 100.0, self.scale_v) / 100.0 * if self.reflect_y { -1.0 } else { 1.0 };
        let (s, c) = sin_cos_deg(amount(3, 0.0, self.rotate));
        let m = [c * sx, s * sx, -s * sy, c * sy];
        let [ax, ay] = anchor;
        [
            m[0],
            m[1],
            m[2],
            m[3],
            ax - (m[0] * ax + m[2] * ay) + amount(4, 0.0, self.move_h),
            ay - (m[1] * ax + m[3] * ay) + amount(5, 0.0, self.move_v),
        ]
    }
}

pub(super) fn step_repeat(sources: &[OwnedPath], r: &EffectRecord, cancel: &CancellationToken) -> Result<Vec<OwnedPath>> {
    let p = StepParams {
        move_h: r.real("move_horizontal")?,
        move_v: r.real("move_vertical")?,
        scale_h: r.real("scale_horizontal")?,
        scale_v: r.real("scale_vertical")?,
        rotate: r.real("rotate")?,
        reflect_x: r.flag("reflect_x")?,
        reflect_y: r.flag("reflect_y")?,
        // STU-VEC-157 spells the key `randomize` (lane SMB-AI-LIVE-EFFECTS-1).
        random: r.flag("randomize")?,
        seed: r.seed()?,
    };
    let reference = reference(r.token("reference_point")?)?;
    let copies = u64::try_from(r.int("copies")?).map_err(|_| LiveEffectError::NotACount)?;
    // Copies are counted against the element budget before any allocation (V4-02).
    let per_copy: u64 = sources.iter().map(|s| s.elements.len() as u64).fold(0, u64::saturating_add).saturating_add(sources.len() as u64);
    if copies.saturating_add(1).saturating_mul(per_copy) > MAX_OP_ELEMENTS as u64 {
        return Err(LiveEffectError::OutputOverBudget);
    }
    let anchor = bounds_of(sources).map_or([0.0, 0.0], |b| reference.of(&b));
    let capacity = usize::try_from(copies.saturating_add(1).saturating_mul(sources.len() as u64)).map_err(|_| LiveEffectError::OutputOverBudget)?;
    let mut out = Vec::with_capacity(capacity);
    out.extend(sources.iter().cloned());
    let mut m: Affine = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mut k = 0_u64;
    while k < copies {
        check_cancel(cancel)?;
        k = k.saturating_add(1);
        m = affine_mul(&p.step(anchor, k), &m);
        for path in sources {
            out.push(transform_path(path, &m).map_err(result_error)?);
        }
    }
    Ok(out)
}
