//! Simplify (lane SMB-AI-SIMPLIFY-1, STU-VEC-114) and the smooth tool's standalone smoothing
//! (STU-VEC-007), as pure functions over the anchor model.
//!
//! [`simplify`] takes the four-parameter STU-VEC-114 table ([`SimplifyParams`]) and works on every
//! contour, open or closed; [`simplify_owned`] is the same through [`AnchorPath::from_owned_path`]
//! for composite and interop callers; [`smooth`] is the smooth tool's operation with the shared
//! `smoothing` parameter ([`SMOOTH_TOOL_DEFAULT`] = 20, captured). Floating-point work uses
//! IEEE-exact operations only (+ - * / sqrt, round, min/max); angles are compared with dot products
//! against [`crate::shape::sin_cos_deg`] (no platform trigonometry), so results are byte-identical
//! across platforms.
//!
//! Studio readings, not verified Adobe behaviour (Illustrator Simplify parity UNVERIFIED, D5):
//! - Parameters are carried at precision 2: `round(v * 100) / 100`, halves away from zero; a value
//!   whose magnitude is at least 1e13 is carried as given (its hundredths are not representable).
//!   The carried values are echoed in [`SimplifyReport`], never clamped (STU-VEC-100: the hard
//!   bounds are unknown, so every finite value is accepted; STU-VEC-102: 0..100 / 0..180 are UI
//!   ranges). NaN or infinite parameters are [`AnchorError::NonFinite`].
//! - Tolerance per contour: `(100 - curve_precision) / 100 * diagonal * TOLERANCE_FRACTION`, with
//!   `diagonal` the bounding-box diagonal of the contour's samples ([`SAMPLES_PER_SEGMENT`] per
//!   segment). Above 100 behaves as 100 (tolerance 0); below 0 the line continues (a larger
//!   tolerance); an overflowing tolerance saturates at `f64::MAX`.
//! - Tolerance 0 (`curve_precision` >= 100) removes only exactly redundant anchors: an anchor on a
//!   zero-length straight segment (a duplicate), an anchor between two straight segments that lies on
//!   the chord of its neighbours within [`EXACT_TOLERANCE`] (no backtracking), and a smooth anchor
//!   that splits one cubic (the merged cubic reproduces both pieces within [`EXACT_TOLERANCE`]).
//!   A path without such anchors is returned unchanged.
//! - Tolerance > 0: the kept anchors are a subset of the original anchors (the count never grows).
//!   The ends of an open contour, anchor 0 of a closed contour (plus its farthest anchor when nothing
//!   else is kept), protected corners and every [`MAX_RUN_SEGMENTS`]-th anchor of a long run are
//!   kept; each run between kept anchors is refit with one cubic whose end tangents are the original
//!   one-sided tangents (least squares on the samples, chord-length parameters, Newton
//!   reparameterisation); a run whose sample error exceeds the tolerance is split at the original
//!   anchor nearest its worst sample, and a one-segment run keeps its original segment. Every
//!   sample of the original lies within the reported tolerance of the result
//!   ([`ContourReport::max_error`] <= `tolerance`). The exact pass above then runs on the result.
//! - `angle_threshold` (deg): an anchor whose interior angle (between the reversed incoming and the
//!   outgoing tangent; 180 = straight) is below the threshold is a protected corner, kept with its
//!   anchor record unchanged. Thresholds <= 0 protect nothing, thresholds above 180 protect every
//!   anchor with two defined tangents; raising the threshold protects a superset.
//! - `convert_to_straight_lines`: runs are fit with straight chords and every output anchor has zero
//!   handles and `NONE` mirroring (protected corners included); smoothing is not applied (it would
//!   create handles). A one-segment curved run cannot be split further, so `max_error` may then
//!   exceed the tolerance; it is reported.
//! - `smoothing` (percent) is applied after the fit: the handles of every anchor that is not a
//!   protected corner move toward the curvature tool's Catmull-Rom tangent
//!   ([`crate::freeform::smooth_curvature`]: `(next - prev) / 6` mirrored; zero at open ends and
//!   on contours of fewer than three anchors) by the fraction `smoothing / 100`; values outside 0..100 extrapolate.
//!   Smoothing can move the result beyond the fit tolerance. [`smooth`] treats `NONE` anchors as
//!   the corners (as `smooth_curvature` does).
//! - `show_original_path` and `preview` are display-only and are never read (STU-VEC-114).
//! - A kept anchor that was a corner (`NONE`) stays a corner; a smooth anchor stays smooth
//!   (`ANGLE_AND_LENGTH` when its new handles are equal, else `ANGLE`), STU-VEC-044.
//! - A result coordinate beyond the coordinate limit (an extrapolated smoothing, say) is
//!   [`AnchorError::OutOfRange`]; input budgets are those of [`AnchorPath::validate`].
use crate::anchor_path::{AnchorError, AnchorPath, Contour, EditAnchor, infer_mirroring};
use crate::owned_path::{OwnedPath, PathPoint};
use crate::path_contract::Mirroring;
use crate::shape::sin_cos_deg;

/// Samples per original segment that the fit measures (and the error bound holds at).
pub const SAMPLES_PER_SEGMENT: u32 = 32;
/// Longest run (in original segments) refit as one piece; longer runs are cut at every
/// `MAX_RUN_SEGMENTS`-th anchor first (bounds the work per run).
pub const MAX_RUN_SEGMENTS: usize = 64;
/// Tolerance at `curve_precision` 0, as a fraction of the contour's bounding-box diagonal.
pub const TOLERANCE_FRACTION: f64 = 0.05;
/// Distance under which a removal at tolerance 0 counts as reproducing the curve.
pub const EXACT_TOLERANCE: f64 = 1.0e-9;
/// The smooth tool's captured `smoothing` default (STU-VEC-114 / STU-VEC-007), percent.
pub const SMOOTH_TOOL_DEFAULT: f64 = 20.0;
/// Newton reparameterisation passes per run fit.
const NEWTON_PASSES: u32 = 4;
/// Magnitude from which a parameter is carried as given.
const CARRY_LIMIT: f64 = 1.0e13;

const ZERO: PathPoint = PathPoint { x: 0.0, y: 0.0 };

/// The STU-VEC-114 parameter table. `Default` is the captured defaults: 0, 0, 0, false, false, true.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SimplifyParams {
    /// Percent, precision 2; 100 = exact (only redundant anchors removed).
    pub curve_precision: f64,
    /// Degrees, precision 2; anchors with a smaller interior angle are kept as corners.
    pub angle_threshold: f64,
    /// Percent, precision 2; applied after the fit.
    pub smoothing: f64,
    pub convert_to_straight_lines: bool,
    /// Display-only; never changes the result.
    pub show_original_path: bool,
    /// Display-only; never changes the result.
    pub preview: bool,
}

impl Default for SimplifyParams {
    fn default() -> Self {
        Self { curve_precision: 0.0, angle_threshold: 0.0, smoothing: 0.0, convert_to_straight_lines: false, show_original_path: false, preview: true }
    }
}

/// What simplify did to one contour.
#[derive(Clone, Debug, PartialEq)]
pub struct ContourReport {
    pub anchors_before: usize,
    pub anchors_after: usize,
    /// The effective tolerance (document units); 0 = exact mode.
    pub tolerance: f64,
    /// Largest measured distance of an original sample from the fit (before smoothing).
    pub max_error: f64,
    /// Original indices of the protected corners.
    pub protected: Vec<usize>,
}

/// The carried (rounded, unclamped) parameters and the per-contour reports.
#[derive(Clone, Debug, PartialEq)]
pub struct SimplifyReport {
    pub curve_precision: f64,
    pub angle_threshold: f64,
    pub smoothing: f64,
    pub contours: Vec<ContourReport>,
}

/// A parameter carried at precision 2 (see the module rules).
pub fn carry(v: f64) -> f64 {
    if !v.is_finite() || v.abs() >= CARRY_LIMIT {
        return v;
    }
    let r = (v * 100.0).round() / 100.0;
    if r == 0.0 { 0.0 } else { r }
}

/// The effective tolerance for a carried `curve_precision` and a bounding-box diagonal.
pub fn tolerance_for(curve_precision: f64, diagonal: f64) -> f64 {
    if curve_precision >= 100.0 {
        return 0.0;
    }
    let t = (100.0 - curve_precision) / 100.0 * diagonal * TOLERANCE_FRACTION;
    if t.is_finite() { t } else { f64::MAX }
}

// ---- vector helpers (IEEE-exact) ----------------------------------------------------------------

fn add(a: PathPoint, b: PathPoint) -> PathPoint {
    PathPoint::new(a.x + b.x, a.y + b.y)
}
fn sub(a: PathPoint, b: PathPoint) -> PathPoint {
    PathPoint::new(a.x - b.x, a.y - b.y)
}
fn scale(a: PathPoint, k: f64) -> PathPoint {
    PathPoint::new(a.x * k, a.y * k)
}
fn dot(a: PathPoint, b: PathPoint) -> f64 {
    a.x * b.x + a.y * b.y
}
fn len(a: PathPoint) -> f64 {
    dot(a, a).sqrt()
}
fn is_zero(a: PathPoint) -> bool {
    a.x == 0.0 && a.y == 0.0
}
fn unit(a: PathPoint) -> Option<PathPoint> {
    let l = len(a);
    (l > 0.0 && l.is_finite()).then(|| scale(a, 1.0 / l))
}
fn clean(p: PathPoint) -> PathPoint {
    let z = |v: f64| if v == 0.0 { 0.0 } else { v };
    PathPoint::new(z(p.x), z(p.y))
}

fn bernstein(t: f64) -> [f64; 4] {
    let u = 1.0 - t;
    [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t]
}

fn cubic_at(c: &[PathPoint; 4], t: f64) -> PathPoint {
    let b = bernstein(t);
    PathPoint::new(b[0] * c[0].x + b[1] * c[1].x + b[2] * c[2].x + b[3] * c[3].x, b[0] * c[0].y + b[1] * c[1].y + b[2] * c[2].y + b[3] * c[3].y)
}

fn cubic_d1(c: &[PathPoint; 4], t: f64) -> PathPoint {
    let u = 1.0 - t;
    let (a, b, d) = (sub(c[1], c[0]), sub(c[2], c[1]), sub(c[3], c[2]));
    scale(add(add(scale(a, u * u), scale(b, 2.0 * u * t)), scale(d, t * t)), 3.0)
}

fn cubic_d2(c: &[PathPoint; 4], t: f64) -> PathPoint {
    let u = 1.0 - t;
    let a = add(sub(c[2], scale(c[1], 2.0)), c[0]);
    let b = add(sub(c[3], scale(c[2], 2.0)), c[1]);
    scale(add(scale(a, u), scale(b, t)), 6.0)
}

/// Distance from `p` to the segment `a`..`b`.
fn segment_distance(p: PathPoint, a: PathPoint, b: PathPoint) -> f64 {
    let d = sub(b, a);
    let dd = dot(d, d);
    if dd == 0.0 {
        return len(sub(p, a));
    }
    let t = (dot(sub(p, a), d) / dd).clamp(0.0, 1.0);
    len(sub(p, add(a, scale(d, t))))
}

/// The direction the cubic leaves its start in (first control point that differs).
fn start_tangent(c: &[PathPoint; 4]) -> Option<PathPoint> {
    [c[1], c[2], c[3]].into_iter().find(|q| *q != c[0]).and_then(|q| unit(sub(q, c[0])))
}

/// The direction the cubic arrives at its end in.
fn end_tangent(c: &[PathPoint; 4]) -> Option<PathPoint> {
    [c[2], c[1], c[0]].into_iter().find(|q| *q != c[3]).and_then(|q| unit(sub(c[3], q)))
}

fn wrap(i: usize, n: usize) -> usize {
    i.checked_rem(n).unwrap_or(0)
}

/// Control points of segment `i` of a contour (`i < segment_count`).
fn seg(c: &Contour, i: usize) -> Result<[PathPoint; 4], AnchorError> {
    let n = c.anchors.len();
    let a = c.anchors.get(i).ok_or(AnchorError::PointIndexOutOfBounds)?;
    let b = c.anchors.get(wrap(i.saturating_add(1), n)).ok_or(AnchorError::PointIndexOutOfBounds)?;
    Ok([a.position, add(a.position, a.outgoing), add(b.position, b.incoming), b.position])
}

fn sample_param(s: u32) -> f64 {
    f64::from(s) / f64::from(SAMPLES_PER_SEGMENT)
}

/// Mirroring of a kept anchor whose handles changed: a corner stays a corner, a smooth anchor
/// stays smooth (STU-VEC-044).
fn remirror(orig: Mirroring, incoming: PathPoint, outgoing: PathPoint) -> Mirroring {
    if orig == Mirroring::None {
        return Mirroring::None;
    }
    match infer_mirroring(incoming, outgoing) {
        Mirroring::AngleAndLength => Mirroring::AngleAndLength,
        _ => Mirroring::Angle,
    }
}

fn with_handles(base: &EditAnchor, incoming: PathPoint, outgoing: PathPoint) -> EditAnchor {
    if incoming == base.incoming && outgoing == base.outgoing {
        return *base;
    }
    EditAnchor { position: base.position, incoming, outgoing, mirroring: remirror(base.mirroring, incoming, outgoing) }
}

// ---- corners ------------------------------------------------------------------------------------

/// Anchors whose interior angle is below `threshold` degrees (carried value).
fn protected_mask(c: &Contour, threshold: f64) -> Result<Vec<bool>, AnchorError> {
    let n = c.anchors.len();
    let mut out = vec![false; n];
    if threshold <= 0.0 || n < 2 {
        return Ok(out);
    }
    let all = threshold > 180.0;
    let cos_t = sin_cos_deg(threshold).1;
    for (i, slot) in out.iter_mut().enumerate() {
        let has_in = c.closed || i > 0;
        let has_out = c.closed || i.saturating_add(1) < n;
        if !(has_in && has_out) {
            continue;
        }
        let prev = if i > 0 { i.saturating_sub(1) } else { n.saturating_sub(1) };
        let (Some(tin), Some(tout)) = (end_tangent(&seg(c, prev)?), start_tangent(&seg(c, i)?)) else {
            continue;
        };
        // Interior angle between -tin and tout: cos = -tin . tout; angle < threshold <=> cos > cos(threshold).
        let cos_angle = -dot(tin, tout);
        *slot = all || cos_angle > cos_t;
    }
    Ok(out)
}

// ---- exact pass (tolerance 0) -------------------------------------------------------------------

/// The cubic that `prev -> cur -> next` split at `cur` came from, if it reproduces both pieces
/// within [`EXACT_TOLERANCE`]: the new outgoing handle of `prev` and incoming handle of `next`.
fn merge_split(prev: &EditAnchor, cur: &EditAnchor, next: &EditAnchor) -> Option<(PathPoint, PathPoint)> {
    let (lb, lc) = (len(cur.incoming), len(cur.outgoing));
    if lb == 0.0 || lc == 0.0 {
        return None;
    }
    let t = lb / (lb + lc);
    if !(t > 0.0 && t < 1.0) {
        return None;
    }
    let h_out = clean(scale(prev.outgoing, 1.0 / t));
    let h_in = clean(scale(next.incoming, 1.0 / (1.0 - t)));
    let (p0, m, p3) = (prev.position, cur.position, next.position);
    let q = [p0, add(p0, h_out), add(p3, h_in), p3];
    let left = [p0, add(p0, prev.outgoing), add(m, cur.incoming), m];
    let right = [m, add(m, cur.outgoing), add(p3, next.incoming), p3];
    for s in 1..=SAMPLES_PER_SEGMENT {
        let u = sample_param(s);
        if len(sub(cubic_at(&left, u), cubic_at(&q, t * u))) > EXACT_TOLERANCE {
            return None;
        }
        if len(sub(cubic_at(&right, u), cubic_at(&q, t + (1.0 - t) * u))) > EXACT_TOLERANCE {
            return None;
        }
    }
    Some((h_out, h_in))
}

/// `p` lies on the chord `a`..`b` within [`EXACT_TOLERANCE`], between its ends.
fn on_chord(a: PathPoint, p: PathPoint, b: PathPoint) -> bool {
    let d = sub(b, a);
    let l = len(d);
    if l == 0.0 {
        return false;
    }
    let v = sub(p, a);
    (v.x * d.y - v.y * d.x).abs() / l <= EXACT_TOLERANCE && dot(v, sub(b, p)) >= 0.0
}

/// Removes exactly redundant anchors (module rules). `fixed` anchors are never removed. Returns the
/// contour and the input index of every kept anchor.
fn exact_pass(c: &Contour, fixed: &[bool]) -> (Contour, Vec<usize>) {
    let n = c.anchors.len();
    if n < 3 {
        return (c.clone(), (0..n).collect());
    }
    let mut work = c.anchors.clone();
    let mut out: Vec<EditAnchor> = Vec::with_capacity(n);
    let mut idx: Vec<usize> = Vec::with_capacity(n);
    let is_fixed = |i: usize| fixed.get(i).copied().unwrap_or(true);
    for i in 0..n {
        let Some(cur) = work.get(i).copied() else { break };
        let Some(prev) = out.last().copied() else {
            out.push(cur);
            idx.push(i);
            continue;
        };
        let can_drop = out.len().saturating_add(n.saturating_sub(i).saturating_sub(1)) >= 2;
        let zero_seg = prev.position == cur.position && is_zero(prev.outgoing) && is_zero(cur.incoming);
        if zero_seg && can_drop {
            if !is_fixed(i) {
                if let Some(last) = out.last_mut() {
                    *last = with_handles(&prev, prev.incoming, cur.outgoing);
                }
                continue;
            }
            if let Some(&pi) = idx.last()
                && !is_fixed(pi)
            {
                out.pop();
                idx.pop();
                out.push(with_handles(&cur, prev.incoming, cur.outgoing));
                idx.push(i);
                continue;
            }
        }
        if is_fixed(i) || !can_drop {
            out.push(cur);
            idx.push(i);
            continue;
        }
        let next = if i.saturating_add(1) < n { work.get(i.saturating_add(1)).copied() } else if c.closed { out.first().copied() } else { None };
        let Some(next) = next else {
            out.push(cur);
            idx.push(i);
            continue;
        };
        let straight = is_zero(prev.outgoing) && is_zero(cur.incoming) && is_zero(cur.outgoing) && is_zero(next.incoming);
        if straight && on_chord(prev.position, cur.position, next.position) {
            continue;
        }
        if let Some((h_out, h_in)) = merge_split(&prev, &cur, &next) {
            if let Some(last) = out.last_mut() {
                *last = with_handles(&prev, prev.incoming, h_out);
            }
            let target = if i.saturating_add(1) < n { work.get_mut(i.saturating_add(1)) } else { out.first_mut() };
            if let Some(t) = target {
                *t = with_handles(t, h_in, t.outgoing);
            }
            continue;
        }
        out.push(cur);
        idx.push(i);
    }
    // The closing segment of a closed contour.
    if c.closed
        && out.len() > 2
        && let (Some(last), Some(first), Some(&li)) = (out.last().copied(), out.first().copied(), idx.last())
        && last.position == first.position
        && is_zero(last.outgoing)
        && is_zero(first.incoming)
        && !is_fixed(li)
    {
        out.pop();
        idx.pop();
        if let Some(f) = out.first_mut() {
            *f = with_handles(&first, last.incoming, first.outgoing);
        }
    }
    (Contour { anchors: out, closed: c.closed }, idx)
}

// ---- fit (tolerance > 0) ------------------------------------------------------------------------

struct Fit {
    outgoing: PathPoint,
    incoming: PathPoint,
    error: f64,
    /// Run-relative anchor offset nearest the worst sample.
    worst_anchor: usize,
}

struct Sample {
    p: PathPoint,
    seg: usize,
    s: u32,
}

fn run_samples(c: &Contour, start: usize, k: usize) -> Result<Vec<Sample>, AnchorError> {
    let first = c.anchors.get(start).ok_or(AnchorError::PointIndexOutOfBounds)?;
    let per = usize::try_from(SAMPLES_PER_SEGMENT).unwrap_or(32);
    let mut out = Vec::with_capacity(k.saturating_mul(per).saturating_add(1));
    out.push(Sample { p: first.position, seg: 0, s: 0 });
    for j in 0..k {
        let ctl = seg(c, start.saturating_add(j))?;
        for s in 1..=SAMPLES_PER_SEGMENT {
            out.push(Sample { p: if s == SAMPLES_PER_SEGMENT { ctl[3] } else { cubic_at(&ctl, sample_param(s)) }, seg: j, s });
        }
    }
    Ok(out)
}

fn worst_offset(sample: &Sample, k: usize) -> usize {
    let off = if sample.s.saturating_mul(2) <= SAMPLES_PER_SEGMENT { sample.seg } else { sample.seg.saturating_add(1) };
    off.clamp(1, k.saturating_sub(1).max(1))
}

/// Fits run `start .. start + k` (segments) with one cubic (or a chord when `lines`).
fn fit_run(c: &Contour, start: usize, k: usize, fixed_out: Option<PathPoint>, fixed_in: Option<PathPoint>, lines: bool) -> Result<Fit, AnchorError> {
    let n = c.anchors.len();
    let a = *c.anchors.get(start).ok_or(AnchorError::PointIndexOutOfBounds)?;
    let b = *c.anchors.get(wrap(start.saturating_add(k), n)).ok_or(AnchorError::PointIndexOutOfBounds)?;
    if k == 1 && !lines {
        return Ok(Fit { outgoing: a.outgoing, incoming: b.incoming, error: 0.0, worst_anchor: 1 });
    }
    let samples = run_samples(c, start, k)?;
    let (p0, p3) = (a.position, b.position);
    if lines {
        let mut best = (0.0_f64, 0_usize);
        for (i, sm) in samples.iter().enumerate() {
            let d = segment_distance(sm.p, p0, p3);
            if d > best.0 {
                best = (d, i);
            }
        }
        let worst = samples.get(best.1).map_or(1, |sm| worst_offset(sm, k));
        return Ok(Fit { outgoing: ZERO, incoming: ZERO, error: best.0, worst_anchor: worst });
    }
    let chord = sub(p3, p0);
    let t0 = start_tangent(&seg(c, start)?).or_else(|| unit(chord)).unwrap_or(ZERO);
    let t1 = end_tangent(&seg(c, start.saturating_add(k).saturating_sub(1))?).or_else(|| unit(chord)).unwrap_or(ZERO);
    // Chord-length parameters.
    let mut u: Vec<f64> = Vec::with_capacity(samples.len());
    let mut total = 0.0;
    let mut prev = p0;
    for sm in &samples {
        total += len(sub(sm.p, prev));
        u.push(total);
        prev = sm.p;
    }
    let last = u.len().saturating_sub(1);
    let denom = f64::from(u32::try_from(last).unwrap_or(u32::MAX)).max(1.0);
    for (i, v) in u.iter_mut().enumerate() {
        *v = if total > 0.0 { *v / total } else { f64::from(u32::try_from(i).unwrap_or(u32::MAX)) / denom };
    }
    if let Some(v) = u.last_mut() {
        *v = 1.0;
    }
    let fallback = if len(chord) > 0.0 { len(chord) / 3.0 } else { total / 3.0 };
    let dir_a = if fixed_out.is_some() { ZERO } else { t0 };
    let dir_b = if fixed_in.is_some() { ZERO } else { scale(t1, -1.0) };
    let c1 = add(p0, fixed_out.unwrap_or(ZERO));
    let c2 = add(p3, fixed_in.unwrap_or(ZERO));
    let mut best: Option<Fit> = None;
    for pass in 0..=NEWTON_PASSES {
        let (mut c11, mut c12, mut c22, mut x1, mut x2) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for (sm, &t) in samples.iter().zip(&u) {
            let bb = bernstein(t);
            let base = add(add(scale(p0, bb[0]), scale(c1, bb[1])), add(scale(c2, bb[2]), scale(p3, bb[3])));
            let r = sub(sm.p, base);
            let (va, vb) = (scale(dir_a, bb[1]), scale(dir_b, bb[2]));
            c11 += dot(va, va);
            c12 += dot(va, vb);
            c22 += dot(vb, vb);
            x1 += dot(va, r);
            x2 += dot(vb, r);
        }
        let (free_a, free_b) = (fixed_out.is_none(), fixed_in.is_none());
        let (mut al, mut be) = (0.0, 0.0);
        let mut ok = true;
        if free_a && free_b {
            let det = c11 * c22 - c12 * c12;
            if c11 > 0.0 && c22 > 0.0 && det > 1.0e-12 * c11 * c22 {
                al = (x1 * c22 - x2 * c12) / det;
                be = (c11 * x2 - c12 * x1) / det;
            } else {
                ok = false;
            }
        } else if free_a {
            if c11 > 0.0 { al = x1 / c11 } else { ok = false }
        } else if free_b {
            if c22 > 0.0 { be = x2 / c22 } else { ok = false }
        }
        let bad = |v: f64, free: bool| free && !(v.is_finite() && v > 1.0e-6 * total && v <= 4.0 * total);
        if !ok || bad(al, free_a) || bad(be, free_b) {
            al = fallback;
            be = fallback;
        }
        let outgoing = fixed_out.unwrap_or_else(|| clean(scale(t0, al)));
        let incoming = fixed_in.unwrap_or_else(|| clean(scale(t1, -be)));
        let ctl = [p0, add(p0, outgoing), add(p3, incoming), p3];
        let mut worst = (0.0_f64, 0_usize);
        for (i, (sm, &t)) in samples.iter().zip(&u).enumerate() {
            let d = len(sub(sm.p, cubic_at(&ctl, t)));
            if d > worst.0 {
                worst = (d, i);
            }
        }
        if best.as_ref().is_none_or(|f| worst.0 < f.error) {
            let worst_anchor = samples.get(worst.1).map_or(1, |sm| worst_offset(sm, k));
            best = Some(Fit { outgoing, incoming, error: worst.0, worst_anchor });
        }
        if pass < NEWTON_PASSES {
            for (i, (sm, t)) in samples.iter().zip(u.iter_mut()).enumerate() {
                if i == 0 || i == last {
                    continue;
                }
                let q = sub(cubic_at(&ctl, *t), sm.p);
                let d1 = cubic_d1(&ctl, *t);
                let den = dot(d1, d1) + dot(q, cubic_d2(&ctl, *t));
                if den != 0.0 && den.is_finite() {
                    let nt = (*t - dot(q, d1) / den).clamp(0.0, 1.0);
                    if nt.is_finite() {
                        *t = nt;
                    }
                }
            }
        }
    }
    best.ok_or(AnchorError::Degenerate)
}

/// Refits contour `c` at `tol` > 0 (module rules). Returns the contour, the input index of every
/// kept anchor and the largest sample error.
fn fit_contour(c: &Contour, prot: &[bool], tol: f64, lines: bool) -> Result<(Contour, Vec<usize>, f64), AnchorError> {
    let n = c.anchors.len();
    if n < 2 {
        let mut out = c.clone();
        if lines {
            for a in &mut out.anchors {
                *a = EditAnchor { mirroring: Mirroring::None, ..EditAnchor::corner(a.position.x, a.position.y) };
            }
        }
        return Ok((out, (0..n).collect(), 0.0));
    }
    let last = n.saturating_sub(1);
    let mut kept = vec![false; n];
    for (i, k) in kept.iter_mut().enumerate() {
        *k = i == 0 || (!c.closed && i == last) || prot.get(i).copied().unwrap_or(false);
    }
    if c.closed && kept.iter().filter(|k| **k).count() < 2 {
        let p0 = c.anchors.first().map_or(ZERO, |a| a.position);
        let mut far = (f64::NEG_INFINITY, 1_usize);
        for (i, a) in c.anchors.iter().enumerate().skip(1) {
            let d = dot(sub(a.position, p0), sub(a.position, p0));
            if d > far.0 {
                far = (d, i);
            }
        }
        if let Some(k) = kept.get_mut(far.1) {
            *k = true;
        }
    }
    // Cut long runs.
    let end = if c.closed { n } else { last };
    let mut from = 0_usize;
    for i in 1..=end {
        let slot = wrap(i, n);
        if kept.get(slot).copied().unwrap_or(false) || i.saturating_sub(from) >= MAX_RUN_SEGMENTS {
            if let Some(k) = kept.get_mut(slot) {
                *k = true;
            }
            from = i;
        }
    }
    let ks: Vec<usize> = (0..n).filter(|i| kept.get(*i).copied().unwrap_or(false)).collect();
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for (j, &s) in ks.iter().enumerate() {
        let e = match ks.get(j.saturating_add(1)) {
            Some(&e) => e,
            None if c.closed => n,
            None => break,
        };
        stack.push((s, e.saturating_sub(s)));
    }
    stack.reverse();
    let mut accepted: Vec<(usize, usize, PathPoint, PathPoint)> = Vec::new();
    let mut max_error = 0.0_f64;
    while let Some((s, k)) = stack.pop() {
        let e = wrap(s.saturating_add(k), n);
        let fixed_out = (!lines && prot.get(s).copied().unwrap_or(false)).then(|| c.anchors.get(s).map_or(ZERO, |a| a.outgoing));
        let fixed_in = (!lines && prot.get(e).copied().unwrap_or(false)).then(|| c.anchors.get(e).map_or(ZERO, |a| a.incoming));
        let fit = fit_run(c, s, k, fixed_out, fixed_in, lines)?;
        if fit.error <= tol || k <= 1 {
            max_error = max_error.max(fit.error);
            accepted.push((s, k, fit.outgoing, fit.incoming));
        } else {
            let m = fit.worst_anchor.clamp(1, k.saturating_sub(1));
            stack.push((s.saturating_add(m), k.saturating_sub(m)));
            stack.push((s, m));
        }
    }
    accepted.sort_by_key(|r| r.0);
    let mut outs: Vec<Option<PathPoint>> = vec![None; n];
    let mut ins: Vec<Option<PathPoint>> = vec![None; n];
    for &(s, k, o, i) in &accepted {
        if let Some(slot) = outs.get_mut(s) {
            *slot = Some(o);
        }
        if let Some(slot) = ins.get_mut(wrap(s.saturating_add(k), n)) {
            *slot = Some(i);
        }
    }
    let mut idx: Vec<usize> = accepted.iter().map(|r| r.0).collect();
    if !c.closed {
        idx.push(last);
    }
    let mut anchors = Vec::with_capacity(idx.len());
    for &i in &idx {
        let base = c.anchors.get(i).ok_or(AnchorError::PointIndexOutOfBounds)?;
        let a = if lines {
            EditAnchor { mirroring: Mirroring::None, ..EditAnchor::corner(base.position.x, base.position.y) }
        } else if prot.get(i).copied().unwrap_or(false) {
            *base
        } else {
            let incoming = ins.get(i).copied().flatten().unwrap_or(base.incoming);
            let outgoing = outs.get(i).copied().flatten().unwrap_or(base.outgoing);
            with_handles(base, incoming, outgoing)
        };
        anchors.push(a);
    }
    Ok((Contour { anchors, closed: c.closed }, idx, max_error))
}

// ---- smoothing ----------------------------------------------------------------------------------

fn blend(h: PathPoint, t: PathPoint, f: f64) -> PathPoint {
    let g = 1.0 - f;
    clean(PathPoint::new(h.x * g + t.x * f, h.y * g + t.y * f))
}

/// Moves the handles of every non-`corner` anchor toward the Catmull-Rom tangent by fraction `f`.
fn smooth_contour(c: &Contour, corner: &[bool], f: f64) -> Contour {
    let mut out = c.clone();
    if f == 0.0 {
        return out;
    }
    let n = c.anchors.len();
    for (i, a) in out.anchors.iter_mut().enumerate() {
        if corner.get(i).copied().unwrap_or(true) {
            continue;
        }
        let interior = c.closed || (i > 0 && i.saturating_add(1) < n);
        let t = if !interior || n < 3 {
            ZERO
        } else {
            let prev = c.anchors.get(if i > 0 { i.saturating_sub(1) } else { n.saturating_sub(1) }).map_or(a.position, |p| p.position);
            let next = c.anchors.get(wrap(i.saturating_add(1), n)).map_or(a.position, |p| p.position);
            scale(sub(next, prev), 1.0 / 6.0)
        };
        let outgoing = blend(a.outgoing, t, f);
        let incoming = blend(a.incoming, scale(t, -1.0), f);
        let m = infer_mirroring(incoming, outgoing);
        let mirroring = if m == Mirroring::None && a.mirroring != Mirroring::None { a.mirroring } else { m };
        *a = EditAnchor { position: a.position, incoming, outgoing, mirroring };
    }
    out
}

/// A result validated; a non-finite value from finite inputs is an overflow (`OutOfRange`).
fn finish(path: AnchorPath) -> Result<AnchorPath, AnchorError> {
    match path.validate() {
        Ok(()) => Ok(path),
        Err(AnchorError::NonFinite) => Err(AnchorError::OutOfRange),
        Err(e) => Err(e),
    }
}

/// Smooth tool (STU-VEC-007 with the STU-VEC-114 `smoothing` parameter, percent, precision 2):
/// the handles of every non-`NONE` anchor of contour `contour` move toward
/// [`crate::freeform::smooth_curvature`]'s tangent by `smoothing / 100`; `NONE` anchors (corners)
/// are untouched. 0 is the identity, 100 gives `smooth_curvature`'s handles; values outside 0..100
/// are accepted and extrapolate (STU-VEC-100).
pub fn smooth(path: &AnchorPath, contour: usize, smoothing: f64) -> Result<AnchorPath, AnchorError> {
    if !smoothing.is_finite() {
        return Err(AnchorError::NonFinite);
    }
    path.validate()?;
    let c = path.contour(contour)?;
    let corner: Vec<bool> = c.anchors.iter().map(|a| a.mirroring == Mirroring::None).collect();
    let next = smooth_contour(c, &corner, carry(smoothing) / 100.0);
    let mut out = path.clone();
    if let Some(slot) = out.contours.get_mut(contour) {
        *slot = next;
    }
    finish(out)
}

// ---- simplify -----------------------------------------------------------------------------------

fn sample_diagonal(c: &Contour) -> Result<f64, AnchorError> {
    let Some(first) = c.anchors.first() else { return Ok(0.0) };
    let (mut lo, mut hi) = (first.position, first.position);
    for i in 0..c.segment_count() {
        let ctl = seg(c, i)?;
        for s in 1..=SAMPLES_PER_SEGMENT {
            let p = cubic_at(&ctl, sample_param(s));
            lo = PathPoint::new(lo.x.min(p.x), lo.y.min(p.y));
            hi = PathPoint::new(hi.x.max(p.x), hi.y.max(p.y));
        }
    }
    Ok(len(sub(hi, lo)))
}

/// Simplify (STU-VEC-114) over every contour; see the module rules for the Studio readings.
pub fn simplify(path: &AnchorPath, params: &SimplifyParams) -> Result<(AnchorPath, SimplifyReport), AnchorError> {
    if !(params.curve_precision.is_finite() && params.angle_threshold.is_finite() && params.smoothing.is_finite()) {
        return Err(AnchorError::NonFinite);
    }
    path.validate()?;
    let precision = carry(params.curve_precision);
    let threshold = carry(params.angle_threshold);
    let smoothing = carry(params.smoothing);
    let lines = params.convert_to_straight_lines;
    let mut out = AnchorPath { contours: Vec::with_capacity(path.contours.len()), fill_rule: path.fill_rule };
    let mut reports = Vec::with_capacity(path.contours.len());
    for c in &path.contours {
        let n = c.anchors.len();
        let prot = protected_mask(c, threshold)?;
        let tol = tolerance_for(precision, sample_diagonal(c)?);
        let fixed_of = |i: usize| i == 0 || (!c.closed && i.saturating_add(1) == n) || prot.get(i).copied().unwrap_or(false);
        let (fitted, idx, max_error) = if tol == 0.0 && !lines {
            let fixed: Vec<bool> = (0..n).map(fixed_of).collect();
            let (r, idx) = exact_pass(c, &fixed);
            (r, idx, 0.0)
        } else {
            let (r, idx, err) = fit_contour(c, &prot, tol, lines)?;
            let fixed: Vec<bool> = idx.iter().map(|&i| fixed_of(i)).collect();
            let (r2, idx2) = exact_pass(&r, &fixed);
            let composed = idx2.iter().map(|&j| idx.get(j).copied().unwrap_or(0)).collect();
            (r2, composed, err)
        };
        let corner: Vec<bool> = idx.iter().map(|&i| prot.get(i).copied().unwrap_or(false)).collect();
        let result = if lines { fitted } else { smooth_contour(&fitted, &corner, smoothing / 100.0) };
        reports.push(ContourReport {
            anchors_before: n,
            anchors_after: result.anchors.len(),
            tolerance: tol,
            max_error,
            protected: prot.iter().enumerate().filter(|(_, p)| **p).map(|(i, _)| i).collect(),
        });
        out.contours.push(result);
    }
    let out = finish(out)?;
    Ok((out, SimplifyReport { curve_precision: precision, angle_threshold: threshold, smoothing, contours: reports }))
}

/// [`simplify`] on an owned path through [`AnchorPath::from_owned_path`] (quadratics are raised
/// exactly) and back through [`AnchorPath::to_owned_path`].
pub fn simplify_owned(path: &OwnedPath, params: &SimplifyParams) -> Result<(OwnedPath, SimplifyReport), AnchorError> {
    let anchors = AnchorPath::from_owned_path(path)?;
    let (simplified, report) = simplify(&anchors, params)?;
    Ok((simplified.to_owned_path()?, report))
}
