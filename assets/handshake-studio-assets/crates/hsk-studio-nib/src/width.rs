//! Variable-width strokes (lane SMB-ILLUSTRATOR-PEN-3; STU-VEC-007 width tool "Add/move/remove
//! width points along a stroke to author a variable-width profile", STU-VEC-122): a width profile is
//! a list of width points `(t, width)` with `t` the arc-length fraction `0..=1` of each contour;
//! between points the width is linear, beyond the ends constant. [`outline_variable`] flattens the
//! path, offsets every polyline vertex by half the local width along the averaged unit normal, builds
//! the left side forward and the right side back (an open contour, butt or round caps) or two rings
//! (a closed contour), and resolves self-overlap with Nib's boolean normalisation into a filled
//! outline (non-zero polygons). A uniform profile reproduces a constant stroke up to the join style
//! (joins are the averaged-normal joins of the flattened polyline: Studio choice, UNVERIFIED);
//! round caps are polygons whose chord error stays within the flatten tolerance.
#![deny(clippy::arithmetic_side_effects)]
use crate::boolean::{BooleanOptions, GeometryResult, normalise_path};
use crate::flatten::flatten;
use crate::owned_path::{FillRule, OwnedPath, PathElement, PathPoint};
use crate::path_contract::Error;
use crate::shape::sin_cos_deg;

/// Most width points of one profile.
pub const MAX_WIDTH_POINTS: usize = 256;
/// Largest width.
pub const MAX_WIDTH: f64 = 1.0e6;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WidthPoint {
    /// Arc-length fraction of the contour, `0..=1`.
    pub t: f64,
    pub width: f64,
}

/// A width profile: points with strictly increasing `t`.
#[derive(Clone, Debug, PartialEq)]
pub struct WidthProfile {
    pub points: Vec<WidthPoint>,
}

impl WidthProfile {
    pub fn uniform(width: f64) -> Self {
        Self { points: vec![WidthPoint { t: 0.0, width }, WidthPoint { t: 1.0, width }] }
    }

    /// 1..=[`MAX_WIDTH_POINTS`] points, `t` finite in `0..=1` strictly increasing, widths finite in
    /// `0..=`[`MAX_WIDTH`].
    pub fn validate(&self) -> Result<(), Error> {
        if self.points.is_empty() || self.points.len() > MAX_WIDTH_POINTS {
            return Err(Error::UnsupportedOptions);
        }
        let mut last = f64::NEG_INFINITY;
        for p in &self.points {
            if !(p.t.is_finite() && (0.0..=1.0).contains(&p.t) && p.t > last && p.width.is_finite() && (0.0..=MAX_WIDTH).contains(&p.width)) {
                return Err(Error::UnsupportedOptions);
            }
            last = p.t;
        }
        Ok(())
    }

    /// Width at arc-length fraction `t` (linear between points, constant beyond the ends).
    pub fn width_at(&self, t: f64) -> f64 {
        let (Some(first), Some(last)) = (self.points.first(), self.points.last()) else { return 0.0 };
        if t <= first.t {
            return first.width;
        }
        if t >= last.t {
            return last.width;
        }
        for w in self.points.windows(2) {
            if t <= w[1].t {
                let f = (t - w[0].t) / (w[1].t - w[0].t);
                return w[0].width + (w[1].width - w[0].width) * f;
            }
        }
        last.width
    }

    /// Adds a point, or replaces the point within `1e-6` of `t`.
    pub fn with_point(&self, t: f64, width: f64) -> Result<Self, Error> {
        let mut points: Vec<WidthPoint> = self.points.iter().copied().filter(|p| (p.t - t).abs() > 1e-6).collect();
        points.push(WidthPoint { t, width });
        points.sort_by(|a, b| a.t.total_cmp(&b.t));
        let out = Self { points };
        out.validate()?;
        Ok(out)
    }
}

/// Line cap of a variable-width outline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidthCap {
    Butt,
    Round,
}

fn unit_normal(a: PathPoint, b: PathPoint) -> Option<PathPoint> {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let l = (dx * dx + dy * dy).sqrt();
    (l > 0.0).then(|| PathPoint::new(-dy / l, dx / l))
}

/// Filled outline of `path` stroked with the variable `profile`.
pub fn outline_variable(path: &OwnedPath, profile: &WidthProfile, cap: WidthCap, options: &BooleanOptions) -> Result<GeometryResult, Error> {
    profile.validate()?;
    let lines = flatten(path, &options.flatten)?;
    let mut out = OwnedPath::new(FillRule::NonZero);
    for line in &lines {
        let mut pts: Vec<PathPoint> = Vec::with_capacity(line.points.len());
        for p in &line.points {
            if pts.last() != Some(p) {
                pts.push(*p);
            }
        }
        if line.closed && pts.len() > 1 && pts.first() == pts.last() {
            pts.pop();
        }
        if pts.len() < 2 {
            continue;
        }
        let n = pts.len();
        // Arc-length fractions.
        let mut acc = vec![0.0_f64; n];
        for i in 1..n {
            let (a, b) = (pts[i.saturating_sub(1)], pts[i]);
            acc[i] = acc[i.saturating_sub(1)] + ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
        }
        let closing = if line.closed {
            let (a, b) = (pts[n.saturating_sub(1)], pts[0]);
            ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt()
        } else {
            0.0
        };
        let total = acc[n.saturating_sub(1)] + closing;
        if total <= 0.0 {
            continue;
        }
        // Every interior width point becomes a polyline vertex (the width is linear between width
        // points, so the outline is exact between vertices of straight runs).
        let (pts, acc) = {
            let mut out_p = Vec::with_capacity(n.saturating_add(profile.points.len()));
            let mut out_a = Vec::with_capacity(n.saturating_add(profile.points.len()));
            let mut marks = profile.points.iter().map(|w| w.t * total).filter(|m| *m > 0.0 && *m < total).peekable();
            let segments = if line.closed { n } else { n.saturating_sub(1) };
            for i in 0..segments {
                let (a, b) = (pts[i], pts[i.saturating_add(1).checked_rem(n).unwrap_or(0)]);
                let (la, lb) = (acc[i], if i.saturating_add(1) < n { acc[i.saturating_add(1)] } else { total });
                out_p.push(a);
                out_a.push(la);
                while let Some(m) = marks.peek().copied() {
                    if m <= la {
                        marks.next();
                        continue;
                    }
                    if m >= lb {
                        break;
                    }
                    let f = (m - la) / (lb - la);
                    out_p.push(PathPoint::new(a.x + (b.x - a.x) * f, a.y + (b.y - a.y) * f));
                    out_a.push(m);
                    marks.next();
                }
            }
            if !line.closed {
                out_p.push(pts[n.saturating_sub(1)]);
                out_a.push(acc[n.saturating_sub(1)]);
            }
            (out_p, out_a)
        };
        let n = pts.len();
        let mut left = Vec::with_capacity(n);
        let mut right = Vec::with_capacity(n);
        for i in 0..n {
            let prev = if i > 0 { Some(pts[i.saturating_sub(1)]) } else if line.closed { Some(pts[n.saturating_sub(1)]) } else { None };
            let next = if i.saturating_add(1) < n { Some(pts[i.saturating_add(1)]) } else if line.closed { Some(pts[0]) } else { None };
            let n1 = prev.and_then(|p| unit_normal(p, pts[i]));
            let n2 = next.and_then(|q| unit_normal(pts[i], q));
            let normal = match (n1, n2) {
                (Some(a), Some(b)) => {
                    let s = PathPoint::new(a.x + b.x, a.y + b.y);
                    let l = (s.x * s.x + s.y * s.y).sqrt();
                    if l > 1e-12 { PathPoint::new(s.x / l, s.y / l) } else { a }
                }
                (Some(a), None) | (None, Some(a)) => a,
                (None, None) => continue,
            };
            let half = profile.width_at(acc[i] / total) / 2.0;
            left.push(PathPoint::new(pts[i].x + normal.x * half, pts[i].y + normal.y * half));
            right.push(PathPoint::new(pts[i].x - normal.x * half, pts[i].y - normal.y * half));
        }
        if left.len() < 2 {
            continue;
        }
        if line.closed {
            for ring in [left, right.into_iter().rev().collect::<Vec<_>>()] {
                out.elements.push(PathElement::MoveTo(ring[0]));
                out.elements.extend(ring.iter().skip(1).map(|p| PathElement::LineTo(*p)));
                out.elements.push(PathElement::Close);
            }
            continue;
        }
        let mut ring: Vec<PathPoint> = left.clone();
        let tolerance = options.flatten.tolerance;
        let end_cap = |centre: PathPoint, from: PathPoint, ring: &mut Vec<PathPoint>| {
            // Half circle from `from` around `centre`, clockwise on screen, with chords whose sagitta
            // stays within the flatten tolerance: step angle ~ 2 sqrt(2 tol / r) (sqrt only), 4..=1024.
            let (vx, vy) = (from.x - centre.x, from.y - centre.y);
            let r = (vx * vx + vy * vy).sqrt();
            let step = if r > 0.0 { 2.0 * (2.0 * tolerance / r).sqrt() } else { std::f64::consts::PI };
            let steps = (std::f64::consts::PI / step).ceil().clamp(4.0, 1024.0);
            // In 4..=1024 after the clamp.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let steps = steps as u32;
            for k in 1..steps {
                let (s, c) = sin_cos_deg(-180.0 * f64::from(k) / f64::from(steps));
                ring.push(PathPoint::new(centre.x + vx * c - vy * s, centre.y + vx * s + vy * c));
            }
        };
        if cap == WidthCap::Round {
            end_cap(pts[n.saturating_sub(1)], left[left.len().saturating_sub(1)], &mut ring);
        }
        ring.extend(right.iter().rev().copied());
        if cap == WidthCap::Round {
            end_cap(pts[0], right[0], &mut ring);
        }
        out.elements.push(PathElement::MoveTo(ring[0]));
        out.elements.extend(ring.iter().skip(1).map(|p| PathElement::LineTo(*p)));
        out.elements.push(PathElement::Close);
    }
    if out.elements.is_empty() {
        return Err(Error::EmptyPathfinderResult);
    }
    normalise_path(&out, options)
}
