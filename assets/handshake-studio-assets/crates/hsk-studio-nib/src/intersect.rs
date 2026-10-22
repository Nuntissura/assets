//! Curve-curve crossing detection by Bézier subdivision with an interval bound (lane
//! SMB-COMPOSITE-STROKE-GEOMETRY slice 2). A cubic lies inside the convex hull of its control
//! points, so it lies inside their bounding box: two curves can only meet where the boxes of their
//! pieces overlap. [`cubic_crossings`] repeatedly halves (de Casteljau, exact at `t = 1/2`) the piece
//! with the larger box of every overlapping pair and drops pairs whose boxes are disjoint; when both
//! boxes are within `eps` the pair is a crossing, reported at the midpoints of the two parameter
//! intervals. Every true crossing lies in a reported pair's boxes, so its location error is at most
//! `eps` per axis (the interval bound) and its parameters are within the pair's parameter intervals;
//! near-duplicate reports (one crossing found through neighbouring pieces) are merged.
//!
//! Straight segments are exact lines: [`line_cubic`] gives a straight segment uniform-parameter
//! control points, and two straight segments meet by the exact line formula. Coincident overlapping
//! curves have infinitely many crossings: the work is bounded ([`MAX_CROSSING_WORK`] pairs) and runs
//! out as a typed `BudgetExceeded`, never a hang.
#![deny(clippy::arithmetic_side_effects)]
use crate::owned_path::PathPoint;
use crate::path_contract::Error;

/// Most box pairs one [`cubic_crossings`] call examines.
pub const MAX_CROSSING_WORK: u64 = 1 << 20;

pub type Cubic = [PathPoint; 4];

fn lerp(a: PathPoint, b: PathPoint, t: f64) -> PathPoint {
    PathPoint::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

/// Control points of a straight segment with a uniform parameter (thirds).
pub fn line_cubic(a: PathPoint, b: PathPoint) -> Cubic {
    [a, lerp(a, b, 1.0 / 3.0), lerp(a, b, 2.0 / 3.0), b]
}

/// True when the control points are collinear (relative 1e-12) and in order along the chord (a
/// straight segment).
pub fn is_straight(c: &Cubic) -> bool {
    let d = PathPoint::new(c[3].x - c[0].x, c[3].y - c[0].y);
    let l2 = d.x * d.x + d.y * d.y;
    if l2 == 0.0 {
        return c.iter().all(|p| *p == c[0]);
    }
    [c[1], c[2]].iter().all(|p| {
        let v = PathPoint::new(p.x - c[0].x, p.y - c[0].y);
        let cross = v.x * d.y - v.y * d.x;
        let along = (v.x * d.x + v.y * d.y) / l2;
        // Collinear up to the rounding of `line_cubic`'s thirds (relative 1e-12).
        cross.abs() <= 1e-12 * l2 && (0.0..=1.0).contains(&along)
    })
}

fn bbox(c: &Cubic) -> [f64; 4] {
    c.iter().fold([f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY], |b, p| [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)])
}

fn halves(c: &Cubic) -> (Cubic, Cubic) {
    let (p01, p12, p23) = (lerp(c[0], c[1], 0.5), lerp(c[1], c[2], 0.5), lerp(c[2], c[3], 0.5));
    let (p012, p123) = (lerp(p01, p12, 0.5), lerp(p12, p23, 0.5));
    let m = lerp(p012, p123, 0.5);
    ([c[0], p01, p012, m], [m, p123, p23, c[3]])
}

/// Exact parameters where segment `a0-a1` meets `b0-b1` (inclusive), if they meet once.
pub fn line_line(a0: PathPoint, a1: PathPoint, b0: PathPoint, b1: PathPoint) -> Option<(f64, f64)> {
    let (r, s) = (PathPoint::new(a1.x - a0.x, a1.y - a0.y), PathPoint::new(b1.x - b0.x, b1.y - b0.y));
    let den = r.x * s.y - r.y * s.x;
    if den == 0.0 {
        return None;
    }
    let q = PathPoint::new(b0.x - a0.x, b0.y - a0.y);
    let u = (q.x * s.y - q.y * s.x) / den;
    let v = (q.x * r.y - q.y * r.x) / den;
    ((0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v)).then_some((u, v))
}

/// Every crossing of cubics `a` and `b` as parameter pairs `(ta, tb)`, sorted by `ta`; `eps` is the
/// spatial interval bound (finite, `> 0`).
pub fn cubic_crossings(a: &Cubic, b: &Cubic, eps: f64) -> Result<Vec<(f64, f64)>, Error> {
    if !(eps.is_finite() && eps > 0.0) {
        return Err(Error::UnsupportedOptions);
    }
    if a.iter().chain(b).any(|p| !(p.x.is_finite() && p.y.is_finite())) {
        return Err(Error::InvalidGeometry);
    }
    if is_straight(a) && is_straight(b) {
        return Ok(line_line(a[0], a[3], b[0], b[3]).into_iter().collect());
    }
    let mut out: Vec<(f64, f64)> = Vec::new();
    let mut stack: Vec<(Cubic, f64, f64, Cubic, f64, f64)> = vec![(*a, 0.0, 1.0, *b, 0.0, 1.0)];
    let mut work = 0_u64;
    while let Some((ca, a0, a1, cb, b0, b1)) = stack.pop() {
        work = work.saturating_add(1);
        if work > MAX_CROSSING_WORK {
            return Err(Error::BudgetExceeded);
        }
        let (ba, bb) = (bbox(&ca), bbox(&cb));
        if ba[2] < bb[0] || bb[2] < ba[0] || ba[3] < bb[1] || bb[3] < ba[1] {
            continue;
        }
        let (sa, sb) = ((ba[2] - ba[0]).max(ba[3] - ba[1]), (bb[2] - bb[0]).max(bb[3] - bb[1]));
        if sa <= eps && sb <= eps {
            out.push(((a0 + a1) / 2.0, (b0 + b1) / 2.0));
            continue;
        }
        if sa >= sb {
            let (l, r) = halves(&ca);
            let m = (a0 + a1) / 2.0;
            stack.push((r, m, a1, cb, b0, b1));
            stack.push((l, a0, m, cb, b0, b1));
        } else {
            let (l, r) = halves(&cb);
            let m = (b0 + b1) / 2.0;
            stack.push((ca, a0, a1, r, m, b1));
            stack.push((ca, a0, a1, l, b0, m));
        }
    }
    // Merge reports of one crossing found through neighbouring pieces.
    out.sort_by(|x, y| x.0.total_cmp(&y.0));
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for c in out {
        let close = |p: &(f64, f64)| {
            let (pa, pb) = (bez(a, p.0), bez(a, c.0));
            let (qa, qb) = (bez(b, p.1), bez(b, c.1));
            (pa.x - pb.x).abs() <= 4.0 * eps && (pa.y - pb.y).abs() <= 4.0 * eps && (qa.x - qb.x).abs() <= 4.0 * eps && (qa.y - qb.y).abs() <= 4.0 * eps
        };
        if !merged.last().is_some_and(close) {
            merged.push(c);
        }
    }
    Ok(merged)
}

/// Point of cubic `c` at `t`.
pub fn bez(c: &Cubic, t: f64) -> PathPoint {
    let u = 1.0 - t;
    let (a, b, cc, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    PathPoint::new(a * c[0].x + b * c[1].x + cc * c[2].x + d * c[3].x, a * c[0].y + b * c[1].y + cc * c[2].y + d * c[3].y)
}
