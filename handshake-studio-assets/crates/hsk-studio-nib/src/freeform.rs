//! Freeform tools and live corners (lane SMB-ILLUSTRATOR-PEN-2 slice 3): the curvature tool's
//! smoothing, pencil fitting with smoothing, live corners on any anchor (round, inverted round,
//! chamfer), the knife and the shape builder, as pure functions over the anchor model and Nib's
//! boolean grid.
//!
//! - [`smooth_curvature`] (curvature tool, STU-VEC-007 "Draw and edit smooth curves by placed
//!   points ... click toggles corner/smooth"): every smooth interior anchor of a contour gets the
//!   uniform Catmull-Rom tangent `(next - prev) / 6` as mirrored handles, so the curve passes
//!   through every placed point; corner anchors and open ends keep no handles.
//! - [`fit_freehand`] (pencil, STU-VEC-119 `fidelity` / smoothing): the samples are reduced by
//!   Ramer-Douglas-Peucker at `tolerance` (iterative, no recursion), then smoothed like the
//!   curvature tool; `close_within` closes the contour when its ends are that close.
//! - [`apply_corner`] (live corners, STU-VEC-005/112 round, inverted round, chamfer): the anchor
//!   is replaced by two anchors set back `r` along its two incident directions, `r = min(radius,
//!   shorter incident chord / 2)` (STU-VEC-066 clamp, returned). Round = the circular arc tangent
//!   to both directions, inverted round = the concave arc centred on the corner, chamfer = a
//!   straight cut. Arc handle lengths use `4/3 tan(angle / 4)` from half-angle formulas (sqrt
//!   only).
//! - [`knife`] (STU-VEC-048): the cut polyline is stroked with a `kerf` wide butt-capped strip, the
//!   strip is removed from the path and every remaining connected region is a separate closed piece
//!   (the regions are separated by the kerf). A cut that leaves nothing is `EmptyPathfinderResult`.
//! - [`shape_builder`] (STU-VEC-007 shape builder): the operands are divided into faces; the faces
//!   the gesture touches are united into one piece (merge) or removed (delete); the other faces
//!   stay separate pieces.
//!
//! Studio choices, Illustrator parity UNVERIFIED (D5): the Catmull-Rom tangent, the RDP reduction,
//! the setback measured along the incident tangents (curved neighbours keep their outer handles),
//! the knife kerf, and face classification by gesture sample points.
#![deny(clippy::arithmetic_side_effects)]
use crate::anchor_path::{AnchorAddr, AnchorError, AnchorPath, Contour, EditAnchor, HandleSide};
use crate::boolean::{BooleanOp, BooleanOptions, boolean};
use crate::owned_path::{COORDINATE_LIMIT, FillRule, OwnedPath, PathElement, PathPoint};
use crate::path_contract::{Error, Mirroring};
use crate::pathfinder::{Piece, PathfinderOp, pathfinder};
use crate::stroke::{LineCap, StrokeOptions, StrokeStyle, outline_stroke};

/// Most freehand samples or cut / gesture points one call takes.
pub const MAX_SAMPLES: usize = 65_536;
/// Points sampled per gesture segment by [`shape_builder`].
const GESTURE_STEPS: u32 = 16;

fn sub(a: PathPoint, b: PathPoint) -> PathPoint {
    PathPoint::new(a.x - b.x, a.y - b.y)
}
fn add(a: PathPoint, b: PathPoint) -> PathPoint {
    PathPoint::new(a.x + b.x, a.y + b.y)
}
fn scale(a: PathPoint, k: f64) -> PathPoint {
    PathPoint::new(a.x * k, a.y * k)
}
fn len(a: PathPoint) -> f64 {
    (a.x * a.x + a.y * a.y).sqrt()
}
fn dot(a: PathPoint, b: PathPoint) -> f64 {
    a.x * b.x + a.y * b.y
}
fn unit(a: PathPoint) -> Option<PathPoint> {
    let l = len(a);
    (l > 0.0 && l.is_finite()).then(|| scale(a, 1.0 / l))
}

fn check_points(points: &[PathPoint]) -> Result<(), AnchorError> {
    if points.len() > MAX_SAMPLES {
        return Err(AnchorError::BudgetExceeded);
    }
    for p in points {
        if !(p.x.is_finite() && p.y.is_finite()) {
            return Err(AnchorError::NonFinite);
        }
        if p.x.abs() > COORDINATE_LIMIT || p.y.abs() > COORDINATE_LIMIT {
            return Err(AnchorError::OutOfRange);
        }
    }
    Ok(())
}

/// Curvature smoothing of contour `contour`: smooth (non-`NONE`) interior anchors get mirrored
/// Catmull-Rom handles `(next - prev) / 6`; corner anchors and the ends of an open contour get no
/// handles.
pub fn smooth_curvature(path: &AnchorPath, contour: usize) -> Result<AnchorPath, AnchorError> {
    path.validate()?;
    let c = path.contour(contour)?;
    let n = c.anchors.len();
    let mut anchors = c.anchors.clone();
    for (i, a) in anchors.iter_mut().enumerate() {
        let interior = c.closed || (i > 0 && i.saturating_add(1) < n);
        if a.mirroring == Mirroring::None || !interior || n < 3 {
            a.incoming = PathPoint::new(0.0, 0.0);
            a.outgoing = PathPoint::new(0.0, 0.0);
            continue;
        }
        let prev = c.anchors.get(i.checked_sub(1).unwrap_or(n.saturating_sub(1))).ok_or(AnchorError::Degenerate)?;
        let next = c.anchors.get(i.saturating_add(1).checked_rem(n).ok_or(AnchorError::Degenerate)?).ok_or(AnchorError::Degenerate)?;
        let t = scale(sub(next.position, prev.position), 1.0 / 6.0);
        a.outgoing = t;
        a.incoming = scale(t, -1.0);
        a.mirroring = Mirroring::AngleAndLength;
    }
    let mut out = path.clone();
    if let Some(slot) = out.contours.get_mut(contour) {
        slot.anchors = anchors;
    }
    out.validate()?;
    Ok(out)
}

/// Pencil: one contour through the samples reduced at `tolerance` (document units, `> 0`) and
/// smoothed; closed when `close_within` is given and the two ends are at most that far apart.
pub fn fit_freehand(samples: &[PathPoint], tolerance: f64, close_within: Option<f64>) -> Result<AnchorPath, AnchorError> {
    check_points(samples)?;
    if !tolerance.is_finite() || close_within.is_some_and(|d| !d.is_finite()) {
        return Err(AnchorError::NonFinite);
    }
    if tolerance <= 0.0 || close_within.is_some_and(|d| d < 0.0) {
        return Err(AnchorError::InvalidParameter);
    }
    let mut pts: Vec<PathPoint> = Vec::with_capacity(samples.len());
    for p in samples {
        if pts.last() != Some(p) {
            pts.push(*p);
        }
    }
    if pts.len() < 2 {
        return Err(AnchorError::Degenerate);
    }
    // Ramer-Douglas-Peucker with an explicit stack.
    let last = pts.len().saturating_sub(1);
    let mut keep = vec![false; pts.len()];
    keep[0] = true;
    keep[last] = true;
    let mut stack = vec![(0_usize, last)];
    while let Some((a, b)) = stack.pop() {
        if b <= a.saturating_add(1) {
            continue;
        }
        let (pa, pb) = (pts[a], pts[b]);
        let d = sub(pb, pa);
        let l = len(d);
        let mut best = (0.0_f64, a);
        for (i, p) in pts.iter().enumerate().take(b).skip(a.saturating_add(1)) {
            let v = sub(*p, pa);
            let dist = if l == 0.0 { len(v) } else { (v.x * d.y - v.y * d.x).abs() / l };
            if dist > best.0 {
                best = (dist, i);
            }
        }
        if best.0 > tolerance {
            keep[best.1] = true;
            stack.push((a, best.1));
            stack.push((best.1, b));
        }
    }
    let mut kept: Vec<PathPoint> = pts.iter().zip(&keep).filter(|(_, k)| **k).map(|(p, _)| *p).collect();
    let closed = match (close_within, kept.first(), kept.last()) {
        (Some(d), Some(f), Some(l)) if kept.len() >= 3 && len(sub(*f, *l)) <= d => {
            kept.pop();
            true
        }
        _ => false,
    };
    let anchors = kept.iter().map(|p| EditAnchor { mirroring: Mirroring::AngleAndLength, ..EditAnchor::corner(p.x, p.y) }).collect();
    let path = AnchorPath { contours: vec![Contour { anchors, closed }], fill_rule: FillRule::NonZero };
    smooth_curvature(&path, 0)
}

/// Live corner treatments of STU-VEC-005 / STU-VEC-112 that have specified geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CornerTreatment {
    Round,
    InvertedRound,
    Chamfer,
}

/// `tan(phi / 4)` and `tan(phi / 2)` from `cos(phi)` with square roots only.
fn tans(cos_phi: f64) -> (f64, f64) {
    let c = cos_phi.clamp(-1.0, 1.0);
    let (half_cos, half_sin) = (((1.0 + c) / 2.0).sqrt(), ((1.0 - c) / 2.0).sqrt());
    let quarter = half_sin / (1.0 + half_cos);
    let half = if half_cos == 0.0 { f64::INFINITY } else { half_sin / half_cos };
    (quarter, half)
}

/// The point the curve leaves `at` towards on side `side` (its handle end, else the neighbour's
/// facing handle end, else the neighbour itself) and the neighbour's position.
fn incident(c: &Contour, i: usize, side: HandleSide) -> Option<(PathPoint, PathPoint)> {
    let n = c.anchors.len();
    let a = c.anchors.get(i)?;
    let j = match side {
        HandleSide::In => {
            if i > 0 {
                i.checked_sub(1)?
            } else if c.closed {
                n.checked_sub(1)?
            } else {
                return None;
            }
        }
        HandleSide::Out => {
            if i.checked_add(1)? < n {
                i.checked_add(1)?
            } else if c.closed {
                0
            } else {
                return None;
            }
        }
    };
    let b = c.anchors.get(j)?;
    let own = a.handle_point(side);
    let towards = if own != a.position {
        own
    } else {
        let facing = b.handle_point(match side {
            HandleSide::In => HandleSide::Out,
            HandleSide::Out => HandleSide::In,
        });
        if facing != a.position { facing } else { b.position }
    };
    Some((towards, b.position))
}

/// Applies a live corner to anchor `at` (an anchor with two incident segments): returns the path and
/// the applied radius (STU-VEC-066 clamp).
pub fn apply_corner(path: &AnchorPath, at: AnchorAddr, kind: CornerTreatment, radius: f64) -> Result<(AnchorPath, f64), AnchorError> {
    if !radius.is_finite() {
        return Err(AnchorError::NonFinite);
    }
    if radius <= 0.0 {
        return Err(AnchorError::InvalidParameter);
    }
    path.validate()?;
    let corner = *path.anchor(at)?;
    let c = path.contour(at.contour)?;
    let (Some((tin, nin)), Some((tout, nout))) = (incident(c, at.anchor, HandleSide::In), incident(c, at.anchor, HandleSide::Out)) else {
        return Err(AnchorError::InvalidParameter);
    };
    let p = corner.position;
    // Directions: arriving along din, leaving along dout.
    let (din, dout) = match (unit(sub(p, tin)), unit(sub(tout, p))) {
        (Some(a), Some(b)) => (a, b),
        _ => return Err(AnchorError::Degenerate),
    };
    let cos_turn = dot(din, dout);
    if cos_turn >= 1.0 - 1e-12 || cos_turn <= -1.0 + 1e-12 {
        // No corner (straight through) or a cusp.
        return Err(AnchorError::InvalidParameter);
    }
    let r = radius.min(len(sub(p, nin)).min(len(sub(nout, p))) / 2.0);
    if r <= 0.0 {
        return Err(AnchorError::Degenerate);
    }
    let p1 = sub(p, scale(din, r));
    let p2 = add(p, scale(dout, r));
    let mut a1 = EditAnchor { position: p1, incoming: corner.incoming, outgoing: PathPoint::new(0.0, 0.0), mirroring: Mirroring::None };
    let mut a2 = EditAnchor { position: p2, incoming: PathPoint::new(0.0, 0.0), outgoing: corner.outgoing, mirroring: Mirroring::None };
    // The setback anchors keep the outer handles only when they point the incident way.
    if dot(a1.incoming, din) > 0.0 {
        a1.incoming = PathPoint::new(0.0, 0.0);
    }
    if dot(a2.outgoing, dout) < 0.0 {
        a2.outgoing = PathPoint::new(0.0, 0.0);
    }
    match kind {
        CornerTreatment::Chamfer => {}
        CornerTreatment::Round => {
            // Arc tangent to both directions, turning angle theta: R = r / tan(theta / 2).
            let (quarter, half) = tans(cos_turn);
            let big_r = r / half;
            let h = 4.0 / 3.0 * quarter * big_r;
            a1.outgoing = scale(din, h);
            a2.incoming = scale(dout, -h);
        }
        CornerTreatment::InvertedRound => {
            // Arc of radius r centred on the corner from p1 to p2 (angle phi between u and v).
            let (u, v) = (scale(din, -1.0), dout);
            let cos_phi = dot(u, v);
            let (quarter, _) = tans(cos_phi);
            let h = 4.0 / 3.0 * quarter * r;
            let (Some(t1), Some(t2)) = (unit(sub(v, scale(u, cos_phi))), unit(sub(u, scale(v, cos_phi)))) else {
                return Err(AnchorError::Degenerate);
            };
            a1.outgoing = scale(t1, h);
            a2.incoming = scale(t2, h);
        }
    }
    if path.anchor_count() >= crate::anchor_path::MAX_ANCHORS {
        return Err(AnchorError::BudgetExceeded);
    }
    let mut out = path.clone();
    let slot = out.contours.get_mut(at.contour).ok_or(AnchorError::PathIndexOutOfBounds)?;
    slot.anchors.splice(at.anchor..=at.anchor, [a1, a2]);
    out.validate()?;
    Ok((out, r))
}

/// Knife (STU-VEC-048): the regions of `path` left after removing the `kerf`-wide strip along the
/// cut polyline, each a separate closed polygon. Typed: fewer than two cut points
/// `InvalidRequest`, non-finite / huge input `InvalidGeometry` / `BudgetExceeded`, nothing left
/// `EmptyPathfinderResult`.
pub fn knife(path: &OwnedPath, cut: &[PathPoint], kerf: f64, options: &BooleanOptions) -> Result<Vec<OwnedPath>, Error> {
    if cut.len() < 2 {
        return Err(Error::InvalidRequest);
    }
    check_points(cut).map_err(|e| if e == AnchorError::BudgetExceeded { Error::BudgetExceeded } else { Error::InvalidGeometry })?;
    if !(kerf.is_finite() && kerf > 0.0) {
        return Err(Error::UnsupportedOptions);
    }
    let mut line = OwnedPath::new(FillRule::NonZero);
    line.elements.push(PathElement::MoveTo(cut[0]));
    line.elements.extend(cut.iter().skip(1).map(|p| PathElement::LineTo(*p)));
    let mut style = StrokeStyle::new(kerf);
    style.cap = LineCap::Butt;
    let strip = outline_stroke(&line, &style, &StrokeOptions { geometry: *options, ..StrokeOptions::default() })?;
    let faces = pathfinder(PathfinderOp::Divide, &[path.clone(), strip.path], None, options)?;
    let pieces: Vec<OwnedPath> = faces.pieces.into_iter().filter(|p| p.sources == [0]).map(|p| p.path).collect();
    if pieces.is_empty() {
        return Err(Error::EmptyPathfinderResult);
    }
    Ok(pieces)
}

/// Non-zero winding of polygon `path` (line elements; curves count by their chord) around `p`.
pub fn polygon_contains(path: &OwnedPath, p: PathPoint) -> bool {
    let mut winding = 0_i64;
    let mut start = None;
    let mut prev = None;
    let edge = |a: PathPoint, b: PathPoint, w: &mut i64| {
        if a.y <= p.y {
            if b.y > p.y && (b.x - a.x) * (p.y - a.y) - (p.x - a.x) * (b.y - a.y) > 0.0 {
                *w = w.saturating_add(1);
            }
        } else if b.y <= p.y && (b.x - a.x) * (p.y - a.y) - (p.x - a.x) * (b.y - a.y) < 0.0 {
            *w = w.saturating_sub(1);
        }
    };
    for e in &path.elements {
        match *e {
            PathElement::MoveTo(q) => {
                if let (Some(s), Some(l)) = (start, prev) {
                    edge(l, s, &mut winding);
                }
                start = Some(q);
                prev = Some(q);
            }
            PathElement::LineTo(q) | PathElement::QuadTo(_, q) | PathElement::CubicTo(_, _, q) => {
                if let Some(l) = prev {
                    edge(l, q, &mut winding);
                }
                prev = Some(q);
            }
            PathElement::Close => {
                if let (Some(s), Some(l)) = (start, prev) {
                    edge(l, s, &mut winding);
                }
                prev = start;
                start = None;
            }
        }
    }
    if let (Some(s), Some(l)) = (start, prev) {
        edge(l, s, &mut winding);
    }
    winding != 0
}

/// Shape builder: the operands (bottom first) divided into faces; faces touched by the gesture
/// polyline are united into one piece (`delete` = false) or removed (`delete` = true). Typed:
/// nothing touched `InvalidRequest`; nothing left `EmptyPathfinderResult`.
pub fn shape_builder(operands: &[OwnedPath], gesture: &[PathPoint], delete: bool, options: &BooleanOptions) -> Result<Vec<Piece>, Error> {
    if gesture.is_empty() {
        return Err(Error::InvalidRequest);
    }
    check_points(gesture).map_err(|e| if e == AnchorError::BudgetExceeded { Error::BudgetExceeded } else { Error::InvalidGeometry })?;
    if gesture.len().saturating_mul(usize::try_from(GESTURE_STEPS).unwrap_or(16)) > MAX_SAMPLES {
        return Err(Error::BudgetExceeded);
    }
    let faces = if operands.len() == 1 {
        vec![Piece { path: crate::boolean::normalise_path(&operands[0], options)?.path, sources: vec![0] }]
    } else {
        pathfinder(PathfinderOp::Divide, operands, None, options)?.pieces
    };
    let mut samples = vec![gesture[0]];
    for w in gesture.windows(2) {
        for k in 1..=GESTURE_STEPS {
            let t = f64::from(k) / f64::from(GESTURE_STEPS);
            samples.push(PathPoint::new(w[0].x + (w[1].x - w[0].x) * t, w[0].y + (w[1].y - w[0].y) * t));
        }
    }
    let hit: Vec<bool> = faces.iter().map(|f| samples.iter().any(|s| polygon_contains(&f.path, *s))).collect();
    if !hit.iter().any(|h| *h) {
        return Err(Error::InvalidRequest);
    }
    let mut out = Vec::new();
    let mut merged: Option<usize> = None;
    let hit_paths: Vec<OwnedPath> = faces.iter().zip(&hit).filter(|(_, h)| **h).map(|(f, _)| f.path.clone()).collect();
    let mut hit_sources: Vec<usize> = faces.iter().zip(&hit).filter(|(_, h)| **h).flat_map(|(f, _)| f.sources.clone()).collect();
    hit_sources.sort_unstable();
    hit_sources.dedup();
    for (face, h) in faces.into_iter().zip(hit) {
        if !h {
            out.push(face);
        } else if !delete && merged.is_none() {
            merged = Some(out.len());
            let path = if hit_paths.len() == 1 { face.path } else { boolean(&hit_paths, BooleanOp::Union, options)?.path };
            out.push(Piece { path, sources: hit_sources.clone() });
        }
    }
    if out.is_empty() {
        return Err(Error::EmptyPathfinderResult);
    }
    Ok(out)
}

// ---- lane SMB-ILLUSTRATOR-PEN-3: the knife on open paths ------------------------------------------

/// Samples per curved segment when the knife looks for crossings.
const KNIFE_CURVE_SAMPLES: u32 = 64;

fn lerp(a: PathPoint, b: PathPoint, t: f64) -> PathPoint {
    PathPoint::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

fn cubic_at(c: &[PathPoint; 4], t: f64) -> PathPoint {
    let u = 1.0 - t;
    let (a, b, cc, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    PathPoint::new(a * c[0].x + b * c[1].x + cc * c[2].x + d * c[3].x, a * c[0].y + b * c[1].y + cc * c[2].y + d * c[3].y)
}

fn split(c: [PathPoint; 4], t: f64) -> ([PathPoint; 4], [PathPoint; 4]) {
    let (p01, p12, p23) = (lerp(c[0], c[1], t), lerp(c[1], c[2], t), lerp(c[2], c[3], t));
    let (p012, p123) = (lerp(p01, p12, t), lerp(p12, p23, t));
    let m = lerp(p012, p123, t);
    ([c[0], p01, p012, m], [m, p123, p23, c[3]])
}

/// Parameter on `a0-a1` where it crosses `b0-b1` (both inclusive), if it does.
fn crossing(a0: PathPoint, a1: PathPoint, b0: PathPoint, b1: PathPoint) -> Option<f64> {
    let (r, s) = (sub(a1, a0), sub(b1, b0));
    let den = r.x * s.y - r.y * s.x;
    if den == 0.0 {
        return None;
    }
    let q = sub(b0, a0);
    let u = (q.x * s.y - q.y * s.x) / den;
    let v = (q.x * r.y - q.y * r.x) / den;
    ((0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v)).then_some(u)
}

/// Knife on strokes (STU-VEC-048 "open endpoints where it crosses strokes only"): every contour of
/// `path` (open or closed) is cut where the polyline `cut` crosses it; each resulting piece is an
/// open contour of its own path (same fill rule), its cut ends `NONE` anchors sharing the crossing
/// point. A contour the cut misses is returned unchanged. Crossings on curves are located on 64
/// samples per segment (the split itself is exact de Casteljau at the found parameter).
pub fn knife_strokes(path: &AnchorPath, cut: &[PathPoint]) -> Result<Vec<AnchorPath>, AnchorError> {
    if cut.len() < 2 {
        return Err(AnchorError::InvalidParameter);
    }
    check_points(cut)?;
    path.validate()?;
    let work = path.anchor_count().saturating_mul(usize::try_from(KNIFE_CURVE_SAMPLES).unwrap_or(64)).saturating_mul(cut.len());
    if work > MAX_SAMPLES.saturating_mul(1024) {
        return Err(AnchorError::BudgetExceeded);
    }
    let mut out = Vec::new();
    for (ci, c) in path.contours.iter().enumerate() {
        // The segments as cubics and their sorted interior cut parameters.
        let mut pieces: Vec<([PathPoint; 4], bool, Mirroring)> = Vec::new(); // (cubic, break after, start mirroring)
        for si in 0..c.segment_count() {
            let ctl = path.segment(ci, si)?;
            let straight = ctl[1] == ctl[0] && ctl[2] == ctl[3];
            let poly: Vec<(PathPoint, f64)> = if straight {
                vec![(ctl[0], 0.0), (ctl[3], 1.0)]
            } else {
                (0..=KNIFE_CURVE_SAMPLES).map(|k| {
                    let t = f64::from(k) / f64::from(KNIFE_CURVE_SAMPLES);
                    (cubic_at(&ctl, t), t)
                }).collect()
            };
            let mut ts: Vec<f64> = Vec::new();
            for w in poly.windows(2) {
                for k in cut.windows(2) {
                    if let Some(u) = crossing(w[0].0, w[1].0, k[0], k[1]) {
                        let t = w[0].1 + (w[1].1 - w[0].1) * u;
                        if t > 1e-9 && t < 1.0 - 1e-9 {
                            ts.push(t);
                        }
                    }
                }
            }
            ts.sort_by(f64::total_cmp);
            ts.dedup_by(|a, b| (*a - *b).abs() <= 1e-9);
            let start_mirroring = c.anchors.get(si).map_or(Mirroring::None, |a| a.mirroring);
            let mut rest = ctl;
            let mut from = 0.0;
            let mut first = true;
            for t in ts {
                let local = (t - from) / (1.0 - from);
                let (left, right) = if straight {
                    let m = lerp(rest[0], rest[3], local);
                    ([rest[0], rest[0], m, m], [m, m, rest[3], rest[3]])
                } else {
                    split(rest, local)
                };
                pieces.push((left, true, if first { start_mirroring } else { Mirroring::None }));
                first = false;
                rest = right;
                from = t;
            }
            pieces.push((rest, false, if first { start_mirroring } else { Mirroring::None }));
        }
        let breaks = pieces.iter().filter(|p| p.1).count();
        if breaks == 0 {
            out.push(AnchorPath { contours: vec![c.clone()], fill_rule: path.fill_rule });
            continue;
        }
        // A closed contour starts after its first break and wraps around.
        if c.closed {
            let first_break = pieces.iter().position(|p| p.1).unwrap_or(0);
            pieces.rotate_left(first_break.saturating_add(1));
        }
        let mut current: Vec<([PathPoint; 4], Mirroring)> = Vec::new();
        let flush = |run: &mut Vec<([PathPoint; 4], Mirroring)>, out: &mut Vec<AnchorPath>| {
            if run.is_empty() {
                return;
            }
            let mut anchors: Vec<EditAnchor> = Vec::with_capacity(run.len().saturating_add(1));
            for (k, (cub, m)) in run.iter().enumerate() {
                if k == 0 {
                    anchors.push(EditAnchor::corner(cub[0].x, cub[0].y));
                } else if let Some(last) = anchors.last_mut() {
                    last.mirroring = *m;
                }
                if let Some(last) = anchors.last_mut() {
                    last.outgoing = sub(cub[1], cub[0]);
                }
                anchors.push(EditAnchor { incoming: sub(cub[2], cub[3]), ..EditAnchor::corner(cub[3].x, cub[3].y) });
            }
            out.push(AnchorPath { contours: vec![Contour { anchors, closed: false }], fill_rule: path.fill_rule });
            run.clear();
        };
        for (cub, brk, m) in pieces {
            current.push((cub, m));
            if brk {
                flush(&mut current, &mut out);
            }
        }
        flush(&mut current, &mut out);
    }
    for p in &out {
        p.validate()?;
    }
    Ok(out)
}

// ---- lane SMB-ILLUSTRATOR-PEN-3: reshape and blob brush --------------------------------------------

/// Reshape (STU-VEC-007 "Adjust a path region while preserving overall curve continuity"): the point
/// at parameter `t` of segment `segment` of contour `contour` is dragged by (`dx`, `dy`). Every
/// anchor of the contour moves by the drag times `(1 - d / radius)^2` (`d` = its arc-length distance
/// from the grabbed point along the contour, 16 samples per segment; anchors beyond `radius` stay),
/// carrying its handles, so tangent directions (continuity) are kept; then the grabbed segment is
/// bent so the grabbed point lands exactly on target. Studio falloff, UNVERIFIED (D5).
#[allow(clippy::too_many_arguments)]
pub fn reshape(path: &AnchorPath, contour: usize, segment: usize, t: f64, dx: f64, dy: f64, radius: f64) -> Result<AnchorPath, AnchorError> {
    if !(t.is_finite() && dx.is_finite() && dy.is_finite() && radius.is_finite()) {
        return Err(AnchorError::NonFinite);
    }
    if t <= 0.0 || t >= 1.0 || radius <= 0.0 {
        return Err(AnchorError::InvalidParameter);
    }
    path.validate()?;
    let c = path.contour(contour)?;
    let segs = c.segment_count();
    if segment >= segs {
        return Err(AnchorError::PointIndexOutOfBounds);
    }
    let target = {
        let p = crate::anchor_path::point_at(path, contour, segment, t)?;
        PathPoint::new(p.x + dx, p.y + dy)
    };
    // Arc length of every segment and of the grabbed point.
    let mut seg_len = Vec::with_capacity(segs);
    let mut grab_len = 0.0;
    for si in 0..segs {
        let ctl = path.segment(contour, si)?;
        let mut l = 0.0;
        let mut prev = ctl[0];
        for k in 1..=16_u32 {
            let tk = f64::from(k) / 16.0;
            let q = cubic_at(&ctl, tk);
            let step = len(sub(q, prev));
            if si == segment && tk <= t {
                grab_len += step;
            }
            l += step;
            prev = q;
        }
        seg_len.push(l);
    }
    let before: f64 = seg_len.iter().take(segment).sum();
    let grab = before + grab_len;
    let total: f64 = seg_len.iter().sum();
    let mut out = path.clone();
    let slot = out.contours.get_mut(contour).ok_or(AnchorError::PathIndexOutOfBounds)?;
    let mut at = 0.0;
    for (i, a) in slot.anchors.iter_mut().enumerate() {
        let mut d = (at - grab).abs();
        if c.closed {
            d = d.min(total - d);
        }
        let w = (1.0 - d / radius).max(0.0);
        let w = w * w;
        a.position = PathPoint::new(a.position.x + dx * w, a.position.y + dy * w);
        at += seg_len.get(i).copied().unwrap_or(0.0);
    }
    out.validate()?;
    let now = crate::anchor_path::point_at(&out, contour, segment, t)?;
    let rest = sub(target, now);
    if rest.x == 0.0 && rest.y == 0.0 {
        return Ok(out);
    }
    crate::anchor_path::bend_segment(&out, contour, segment, t, rest.x, rest.y)
}

/// Blob brush (STU-VEC-007 "Paint filled unified vector shapes that merge with same-attribute
/// geometry"): the filled outline of the brush stroke through `samples` (round caps and joins,
/// `diameter` wide), united with every path of `merge_with` it overlaps. Returns the united outline
/// and the indices of the merged paths.
pub fn blob_brush(samples: &[PathPoint], diameter: f64, merge_with: &[OwnedPath], options: &BooleanOptions) -> Result<(OwnedPath, Vec<usize>), Error> {
    if samples.is_empty() {
        return Err(Error::InvalidRequest);
    }
    check_points(samples).map_err(|e| if e == AnchorError::BudgetExceeded { Error::BudgetExceeded } else { Error::InvalidGeometry })?;
    if !(diameter.is_finite() && diameter > 0.0) {
        return Err(Error::UnsupportedOptions);
    }
    let mut line = OwnedPath::new(FillRule::NonZero);
    line.elements.push(PathElement::MoveTo(samples[0]));
    if samples.len() == 1 {
        // A dab: a zero-length stroke with round caps is a disc.
        line.elements.push(PathElement::LineTo(samples[0]));
    }
    line.elements.extend(samples.iter().skip(1).map(|p| PathElement::LineTo(*p)));
    let mut style = StrokeStyle::new(diameter);
    style.cap = LineCap::Round;
    style.join = crate::stroke::LineJoin::Round;
    let stroke = outline_stroke(&line, &style, &StrokeOptions { geometry: *options, ..StrokeOptions::default() })?;
    let mut merged = Vec::new();
    let mut operands = vec![stroke.path.clone()];
    for (i, other) in merge_with.iter().enumerate() {
        match boolean(&[stroke.path.clone(), other.clone()], BooleanOp::Intersect, options) {
            Ok(_) => {
                merged.push(i);
                operands.push(other.clone());
            }
            Err(Error::EmptyPathfinderResult) => {}
            Err(e) => return Err(e),
        }
    }
    let united = if operands.len() == 1 { crate::boolean::normalise_path(&stroke.path, options)?.path } else { boolean(&operands, BooleanOp::Union, options)?.path };
    Ok((united, merged))
}
