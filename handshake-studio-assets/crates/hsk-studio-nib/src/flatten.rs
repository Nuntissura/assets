//! Path flattening with a proven chord-error bound (lane SMB-NIB-VECTOR-RENDER slice 1).
//!
//! Each quadratic or cubic segment is cut into `n` equal parameter steps and the curve is replaced by
//! the chords between the step points (direct Bernstein evaluation, no recursion, so the output is a
//! pure function of the input bits). `n` is the smallest count whose chord error is within the
//! caller tolerance by the linear-interpolation bound `|B(t) - chord(t)| <= h^2/8 * max|B''|` over a
//! parameter step `h = 1/n` (integral form of the remainder, valid for vector-valued curves):
//! quadratic `B'' = 2 (p0 - 2 p1 + p2)` gives `n = ceil(sqrt(|p0 - 2 p1 + p2| / (4 tol)))`; cubic
//! `|B''| <= 6 max(|p0 - 2 p1 + p2|, |p1 - 2 p2 + p3|)` gives `n = ceil(sqrt(3 M / (4 tol)))`.
//! The test `flatten_error_within_tolerance` measures the bound on dense samples.
//!
//! Hostile sizes (V4-01/02): the path is validated first (finite, bounded coordinates, element
//! budget); every step count is checked against the remaining point budget before any point is
//! pushed, so the output never exceeds `max_points`.
#![deny(clippy::arithmetic_side_effects)]
use crate::owned_path::{OwnedPath, PathElement, PathLimits, PathPoint};
use crate::path_contract::Error;

/// Smallest accepted chord tolerance (unit of the path coordinates).
pub const MIN_TOLERANCE: f64 = 1.0e-6;
/// Largest accepted chord tolerance.
pub const MAX_TOLERANCE: f64 = 1.0e6;
/// Default point budget of [`FlattenOptions`].
pub const DEFAULT_MAX_POINTS: usize = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlattenOptions {
    /// Largest distance between the true curve and its chords, in path units.
    pub tolerance: f64,
    /// Largest number of output points over all polylines.
    pub max_points: usize,
    /// Structural limits applied to the source path before flattening.
    pub path_limits: PathLimits,
}
impl Default for FlattenOptions {
    fn default() -> Self {
        Self {
            tolerance: 0.1,
            max_points: DEFAULT_MAX_POINTS,
            path_limits: PathLimits::default(),
        }
    }
}
impl FlattenOptions {
    pub(crate) fn check(&self) -> Result<(), Error> {
        if !self.tolerance.is_finite()
            || !(MIN_TOLERANCE..=MAX_TOLERANCE).contains(&self.tolerance)
        {
            return Err(Error::UnsupportedOptions);
        }
        if self.max_points == 0 {
            return Err(Error::UnsupportedOptions);
        }
        Ok(())
    }
}

/// One flattened subpath. `points` never repeats a point twice in a row and a closed polyline
/// does not repeat its first point at the end. `anchors[i]` is true when `points[i]` is an original
/// path anchor (an element end point or the subpath start) rather than an interior chord point; the
/// stroker uses it to apply the caller's join only at real corners.
#[derive(Clone, Debug, PartialEq)]
pub struct Polyline {
    pub points: Vec<PathPoint>,
    pub anchors: Vec<bool>,
    pub closed: bool,
}

impl Polyline {
    /// Arc length of the polyline (closing chord included when closed).
    pub fn length(&self) -> f64 {
        let mut total = 0.0;
        for pair in self.points.windows(2) {
            total += distance(pair[0], pair[1]);
        }
        if self.closed
            && let (Some(first), Some(last)) = (self.points.first(), self.points.last())
        {
            total += distance(*last, *first);
        }
        total
    }
}

/// Euclidean norm through `sqrt` only: IEEE 754 requires `sqrt` to be correctly rounded, whereas
/// `hypot`, `sin`, `cos` and `acos` come from the platform maths library and may differ between
/// hosts (STU-VEC-179 byte-identical geometry). Inputs are bounded (|v| <= 4e9), so no overflow.
pub(crate) fn norm(x: f64, y: f64) -> f64 {
    (x * x + y * y).sqrt()
}

pub(crate) fn distance(a: PathPoint, b: PathPoint) -> f64 {
    norm(b.x - a.x, b.y - a.y)
}

/// Flattens `path` into polylines (one per subpath with at least one point). Open subpaths stay open
/// (a fill closes them implicitly; a stroke does not).
pub fn flatten(path: &OwnedPath, options: &FlattenOptions) -> Result<Vec<Polyline>, Error> {
    options.check()?;
    path.validate(&options.path_limits)?;
    let mut out = Vec::new();
    let mut current: Option<Polyline> = None;
    let mut budget = Budget {
        used: 0,
        max: options.max_points,
    };
    let mut last = PathPoint::new(0.0, 0.0);
    for element in &path.elements {
        match *element {
            PathElement::MoveTo(p) => {
                if let Some(done) = current.take() {
                    finish(done, &mut out);
                }
                budget.take(1)?;
                current = Some(Polyline {
                    points: vec![p],
                    anchors: vec![true],
                    closed: false,
                });
                last = p;
            }
            PathElement::LineTo(p) => {
                let line = current.as_mut().ok_or(Error::InvalidGeometry)?;
                budget.take(1)?;
                push(line, p, true);
                last = p;
            }
            PathElement::QuadTo(c, p) => {
                let line = current.as_mut().ok_or(Error::InvalidGeometry)?;
                let d = second_difference(last, c, p);
                let n = steps(d / 4.0, options.tolerance, &mut budget)?;
                for i in 1..=n {
                    let t = f64::from(i) / f64::from(n);
                    let point = if i == n { p } else { quad_at(last, c, p, t) };
                    push(line, point, i == n);
                }
                last = p;
            }
            PathElement::CubicTo(c1, c2, p) => {
                let line = current.as_mut().ok_or(Error::InvalidGeometry)?;
                let m = second_difference(last, c1, c2).max(second_difference(c1, c2, p));
                let n = steps(3.0 * m / 4.0, options.tolerance, &mut budget)?;
                for i in 1..=n {
                    let t = f64::from(i) / f64::from(n);
                    let point = if i == n {
                        p
                    } else {
                        cubic_at(last, c1, c2, p, t)
                    };
                    push(line, point, i == n);
                }
                last = p;
            }
            PathElement::Close => {
                let mut done = current.take().ok_or(Error::InvalidGeometry)?;
                done.closed = true;
                if let Some(first) = done.points.first().copied() {
                    last = first;
                }
                finish(done, &mut out);
            }
        }
    }
    if let Some(done) = current.take() {
        finish(done, &mut out);
    }
    Ok(out)
}

struct Budget {
    used: usize,
    max: usize,
}
impl Budget {
    fn take(&mut self, count: usize) -> Result<(), Error> {
        let next = self.used.checked_add(count).ok_or(Error::BudgetExceeded)?;
        if next > self.max {
            return Err(Error::BudgetExceeded);
        }
        self.used = next;
        Ok(())
    }
}

/// Step count for a curve whose chord error at `n` steps is at most `scaled / n^2`; the budget is
/// charged before any point is produced.
fn steps(scaled: f64, tolerance: f64, budget: &mut Budget) -> Result<u32, Error> {
    let raw = (scaled / tolerance).sqrt().ceil().max(1.0);
    if !raw.is_finite() || raw > f64::from(u32::MAX) {
        return Err(Error::BudgetExceeded);
    }
    let remaining = budget.max.saturating_sub(budget.used);
    if raw > remaining as f64 {
        return Err(Error::BudgetExceeded);
    }
    // In range by the two checks above.
    let n = raw as u32;
    budget.take(n as usize)?;
    Ok(n)
}

fn second_difference(a: PathPoint, b: PathPoint, c: PathPoint) -> f64 {
    norm(a.x - 2.0 * b.x + c.x, a.y - 2.0 * b.y + c.y)
}

pub(crate) fn quad_at(p0: PathPoint, p1: PathPoint, p2: PathPoint, t: f64) -> PathPoint {
    let s = 1.0 - t;
    let (a, b, c) = (s * s, 2.0 * s * t, t * t);
    PathPoint::new(
        a * p0.x + b * p1.x + c * p2.x,
        a * p0.y + b * p1.y + c * p2.y,
    )
}

pub(crate) fn cubic_at(
    p0: PathPoint,
    p1: PathPoint,
    p2: PathPoint,
    p3: PathPoint,
    t: f64,
) -> PathPoint {
    let s = 1.0 - t;
    let (a, b, c, d) = (s * s * s, 3.0 * s * s * t, 3.0 * s * t * t, t * t * t);
    PathPoint::new(
        a * p0.x + b * p1.x + c * p2.x + d * p3.x,
        a * p0.y + b * p1.y + c * p2.y + d * p3.y,
    )
}

fn push(line: &mut Polyline, point: PathPoint, anchor: bool) {
    if line.points.last() == Some(&point) {
        if anchor && let Some(flag) = line.anchors.last_mut() {
            *flag = true;
        }
        return;
    }
    line.points.push(point);
    line.anchors.push(anchor);
}

fn finish(mut line: Polyline, out: &mut Vec<Polyline>) {
    if line.closed && line.points.len() > 1 && line.points.first() == line.points.last() {
        line.points.pop();
        line.anchors.pop();
        if let Some(flag) = line.anchors.first_mut() {
            *flag = true;
        }
    }
    out.push(line);
}
