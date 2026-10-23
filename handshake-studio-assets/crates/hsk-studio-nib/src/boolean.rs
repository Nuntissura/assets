//! Boolean operations on arbitrary paths (lane SMB-NIB-VECTOR-RENDER slice 1; STU-VEC-012/130).
//!
//! Pipeline, all deterministic: every operand is flattened ([`crate::flatten`], chord error at most
//! `flatten.tolerance`), then all points of the operation are snapped onto ONE integer grid whose
//! quantum is a power of two (`Grid`), each operand is normalised under its own fill rule
//! (self-intersections and overlapping subpaths resolved), and the operation runs on the integer
//! contours with i_overlay 4.0.7 (MIT, iShape-Rust; integer sweep with snapping, single-threaded:
//! the `allow_multithreading` feature is off). Results are polygons: outer contours have positive
//! signed shoelace area (counter-clockwise with y up), holes negative, fill rule non-zero.
//!
//! Error budget reported per result ([`GeometryReceipt`]): flatten tolerance plus the grid quantum;
//! i_overlay may additionally snap intersection vertices within a few grid quanta (its snap radius
//! starts at one quantum and doubles per pass; the final radius is not exposed: UNVERIFIED bound).
//!
//! Operand order is the explicit z-order, bottom first (as the rectangle provider): `SubtractFront`
//! (`backMinusFrontCommand`) removes every upper operand from the bottom one, `SubtractBack`
//! (`frontMinusBackCommand`) removes every lower operand from the top one. `Exclude` keeps areas
//! covered by an odd number of operands. An empty result is the typed `EmptyPathfinderResult`.
//! Adobe's own pathfinder output (point placement, curve refit) is not reproduced: UNVERIFIED (D5).
//!
//! Hostile sizes (V4-01/02): flatten budget, operand budget, and a candidate-pair guard that bounds
//! the possible intersection count (bounding-box sweep, aborted at `max_candidate_pairs` pairs or
//! `max_sweep_steps` comparisons) all run before i_overlay allocates its graph.
#![deny(clippy::arithmetic_side_effects)]
use crate::flatten::{FlattenOptions, Polyline, flatten};
use crate::owned_path::{FillRule, OwnedPath, PathElement, PathPoint};
use crate::path_contract::Error;
use i_overlay::core::fill_rule::FillRule as IFill;
use i_overlay::core::overlay::{Overlay, ShapeType};
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::i_float::int::point::IntPoint;
use i_overlay::i_shape::int::shape::{IntContour, IntShapes};

/// Half extent of the integer grid (the same headroom i_float's own adapter uses).
const GRID_HALF_EXTENT: f64 = (1u64 << 29) as f64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BooleanOp {
    Union,
    Intersect,
    Exclude,
    SubtractFront,
    SubtractBack,
}
impl BooleanOp {
    /// Captured operation token (STU-VEC-130).
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Union => "uniteCommand",
            Self::Intersect => "intersectCommand",
            Self::Exclude => "excludeCommand",
            Self::SubtractFront => "backMinusFrontCommand",
            Self::SubtractBack => "frontMinusBackCommand",
        }
    }
    pub fn parse(text: &str) -> Result<Self, Error> {
        match text {
            "uniteCommand" => Ok(Self::Union),
            "intersectCommand" => Ok(Self::Intersect),
            "excludeCommand" => Ok(Self::Exclude),
            "backMinusFrontCommand" => Ok(Self::SubtractFront),
            "frontMinusBackCommand" => Ok(Self::SubtractBack),
            _ => Err(Error::UnsupportedOperation),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BooleanOptions {
    pub flatten: FlattenOptions,
    pub max_operands: usize,
    /// Bound on segment pairs whose bounding boxes overlap (a superset of intersecting pairs).
    pub max_candidate_pairs: u64,
    /// Bound on bounding-box comparisons of the guard sweep itself.
    pub max_sweep_steps: u64,
}
impl Default for BooleanOptions {
    fn default() -> Self {
        Self {
            flatten: FlattenOptions {
                max_points: 1 << 18,
                ..FlattenOptions::default()
            },
            max_operands: 1024,
            max_candidate_pairs: 1 << 22,
            max_sweep_steps: 1 << 26,
        }
    }
}

/// Precision and size receipt of one geometry result.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeometryReceipt {
    pub flatten_tolerance: f64,
    /// Grid quantum (a power of two); output coordinates are `origin + k * quantum`.
    pub grid_quantum: f64,
    pub input_points: u64,
    pub candidate_pairs: u64,
    pub output_points: u64,
    pub output_contours: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GeometryResult {
    /// Polygon path, fill rule non-zero, one `MoveTo .. Close` per contour.
    pub path: OwnedPath,
    pub receipt: GeometryReceipt,
}

/// Runs `op` over `operands` (bottom to top, at least two).
pub fn boolean(
    operands: &[OwnedPath],
    op: BooleanOp,
    options: &BooleanOptions,
) -> Result<GeometryResult, Error> {
    if operands.len() < 2 {
        return Err(Error::InvalidRequest);
    }
    if operands.len() > options.max_operands {
        return Err(Error::BudgetExceeded);
    }
    let mut regions = Vec::with_capacity(operands.len());
    let mut flatten_options = options.flatten;
    for operand in operands {
        let lines = flatten(operand, &flatten_options)?;
        let used = count_points(&lines);
        flatten_options.max_points = flatten_options.max_points.saturating_sub(used);
        regions.push(Region {
            lines,
            fill_rule: operand.fill_rule,
        });
    }
    let grid = Grid::covering(regions.iter().flat_map(|r| r.lines.iter()))?;
    let pairs = candidate_guard(
        regions.iter().flat_map(|r| r.lines.iter()),
        &grid,
        options,
    )?;
    let normalised: Vec<IntShapes> = regions
        .iter()
        .map(|r| normalise(&grid.contours(&r.lines), r.fill_rule))
        .collect();
    let shapes = combine(op, &normalised);
    finish(shapes, &grid, options.flatten.tolerance, &regions, pairs)
}

/// Resolves one path under its own fill rule into simple non-overlapping polygons (outer positive,
/// holes negative, non-zero). Used before rasterising and by the stroker.
pub fn normalise_path(path: &OwnedPath, options: &BooleanOptions) -> Result<GeometryResult, Error> {
    let region = Region {
        lines: flatten(path, &options.flatten)?,
        fill_rule: path.fill_rule,
    };
    let regions = [region];
    let grid = Grid::covering(regions[0].lines.iter())?;
    let pairs = candidate_guard(regions[0].lines.iter(), &grid, options)?;
    let shapes = normalise(&grid.contours(&regions[0].lines), path.fill_rule);
    finish(shapes, &grid, options.flatten.tolerance, &regions, pairs)
}

pub(crate) struct Region {
    pub(crate) lines: Vec<Polyline>,
    pub(crate) fill_rule: FillRule,
}

pub(crate) fn count_points(lines: &[Polyline]) -> usize {
    lines
        .iter()
        .fold(0usize, |n, l| n.saturating_add(l.points.len()))
}

fn ifill(rule: FillRule) -> IFill {
    match rule {
        FillRule::NonZero => IFill::NonZero,
        FillRule::EvenOdd => IFill::EvenOdd,
    }
}

pub(crate) fn normalise(contours: &[IntContour], rule: FillRule) -> IntShapes {
    if contours.is_empty() {
        return Vec::new();
    }
    Overlay::with_contours(contours, &[]).overlay(OverlayRule::Subject, ifill(rule))
}

/// Union of already normalised shape sets (non-zero over consistently oriented contours).
pub(crate) fn union_all(sets: &[IntShapes]) -> IntShapes {
    let mut overlay = Overlay::new(0);
    for set in sets {
        overlay.add_shapes(set, ShapeType::Subject);
    }
    overlay.overlay(OverlayRule::Subject, IFill::NonZero)
}

pub(crate) fn binary(a: &IntShapes, b: &IntShapes, rule: OverlayRule) -> IntShapes {
    Overlay::with_shapes(a, b).overlay(rule, IFill::NonZero)
}

fn combine(op: BooleanOp, sets: &[IntShapes]) -> IntShapes {
    match op {
        BooleanOp::Union => union_all(sets),
        BooleanOp::Intersect | BooleanOp::Exclude => {
            let rule = if op == BooleanOp::Intersect {
                OverlayRule::Intersect
            } else {
                OverlayRule::Xor
            };
            let mut iter = sets.iter();
            let mut acc = iter.next().cloned().unwrap_or_default();
            for set in iter {
                if acc.is_empty() && rule == OverlayRule::Intersect {
                    break;
                }
                acc = binary(&acc, set, rule);
            }
            acc
        }
        BooleanOp::SubtractFront | BooleanOp::SubtractBack => {
            let (keep, rest) = if op == BooleanOp::SubtractFront {
                match sets.split_first() {
                    Some((first, rest)) => (first, rest),
                    None => return Vec::new(),
                }
            } else {
                match sets.split_last() {
                    Some((last, rest)) => (last, rest),
                    None => return Vec::new(),
                }
            };
            let mut overlay = Overlay::new(0);
            overlay.add_shapes(keep, ShapeType::Subject);
            for set in rest {
                overlay.add_shapes(set, ShapeType::Clip);
            }
            overlay.overlay(OverlayRule::Difference, IFill::NonZero)
        }
    }
}

pub(crate) fn finish(
    shapes: IntShapes,
    grid: &Grid,
    tolerance: f64,
    regions: &[Region],
    candidate_pairs: u64,
) -> Result<GeometryResult, Error> {
    let path = grid.path_of(&shapes)?;
    if path.elements.is_empty() {
        return Err(Error::EmptyPathfinderResult);
    }
    let input_points = regions
        .iter()
        .fold(0u64, |n, r| n.saturating_add(count_points(&r.lines) as u64));
    let (mut points, mut contours) = (0u64, 0u64);
    for contour in shapes.iter().flatten() {
        points = points.saturating_add(contour.len() as u64);
        contours = contours.saturating_add(1);
    }
    Ok(GeometryResult {
        path,
        receipt: GeometryReceipt {
            flatten_tolerance: tolerance,
            grid_quantum: grid.quantum,
            input_points,
            candidate_pairs,
            output_points: points,
            output_contours: contours,
        },
    })
}

/// Power-of-two integer grid centred on the operation's bounds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Grid {
    pub(crate) origin_x: f64,
    pub(crate) origin_y: f64,
    pub(crate) scale: f64,
    pub(crate) quantum: f64,
}

impl Grid {
    pub(crate) fn covering<'a>(lines: impl Iterator<Item = &'a Polyline>) -> Result<Self, Error> {
        let (mut x0, mut y0, mut x1, mut y1) = (
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        );
        for p in lines.flat_map(|l| l.points.iter()) {
            x0 = x0.min(p.x);
            y0 = y0.min(p.y);
            x1 = x1.max(p.x);
            y1 = y1.max(p.y);
        }
        if !x0.is_finite() {
            return Ok(Self {
                origin_x: 0.0,
                origin_y: 0.0,
                scale: 1.0,
                quantum: 1.0,
            });
        }
        Ok(Self::for_bounds(x0, y0, x1, y1))
    }

    pub(crate) fn for_bounds(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        let origin_x = x0 * 0.5 + x1 * 0.5;
        let origin_y = y0 * 0.5 + y1 * 0.5;
        let half = (x1 - x0).max(y1 - y0) * 0.5;
        let mut scale = 1.0f64;
        // Bounded loops: coordinates are within +-1e9 (validated), so at most ~70 halvings; the
        // doubling stops at 2^64.
        while half * scale > GRID_HALF_EXTENT {
            scale *= 0.5;
        }
        while half * scale * 2.0 <= GRID_HALF_EXTENT && scale < 1.8446744073709552e19 {
            scale *= 2.0;
        }
        Self {
            origin_x,
            origin_y,
            scale,
            quantum: 1.0 / scale,
        }
    }

    pub(crate) fn int_of(&self, p: PathPoint) -> IntPoint {
        let limit = GRID_HALF_EXTENT * 2.0;
        let x = ((p.x - self.origin_x) * self.scale)
            .round()
            .clamp(-limit, limit);
        let y = ((p.y - self.origin_y) * self.scale)
            .round()
            .clamp(-limit, limit);
        // In i32 range by the clamp (2^30 < 2^31).
        IntPoint::new(x as i32, y as i32)
    }

    pub(crate) fn point_of(&self, p: IntPoint) -> PathPoint {
        let x = f64::from(p.x) * self.quantum + self.origin_x;
        let y = f64::from(p.y) * self.quantum + self.origin_y;
        PathPoint::new(if x == 0.0 { 0.0 } else { x }, if y == 0.0 { 0.0 } else { y })
    }

    /// Integer contours of the polylines as filled regions (open polylines close implicitly);
    /// consecutive duplicates after snapping are dropped, degenerate contours skipped.
    pub(crate) fn contours(&self, lines: &[Polyline]) -> Vec<IntContour> {
        let mut out = Vec::with_capacity(lines.len());
        for line in lines {
            let mut contour: IntContour = Vec::with_capacity(line.points.len());
            for p in &line.points {
                let q = self.int_of(*p);
                if contour.last() != Some(&q) {
                    contour.push(q);
                }
            }
            while contour.len() > 1 && contour.first() == contour.last() {
                contour.pop();
            }
            if contour.len() >= 3 {
                out.push(contour);
            }
        }
        out
    }

    pub(crate) fn path_of(&self, shapes: &IntShapes) -> Result<OwnedPath, Error> {
        let mut path = OwnedPath::new(FillRule::NonZero);
        for contour in shapes.iter().flatten() {
            let mut points = contour.iter();
            let Some(first) = points.next() else {
                continue;
            };
            path.elements.push(PathElement::MoveTo(self.point_of(*first)));
            for p in points {
                path.elements.push(PathElement::LineTo(self.point_of(*p)));
            }
            path.elements.push(PathElement::Close);
        }
        Ok(path)
    }
}

/// Bounding-box sweep over all segments: counts segment pairs whose boxes overlap (every
/// intersecting pair is among them) and refuses before i_overlay runs when the count or the sweep
/// work exceeds its budget.
pub(crate) fn candidate_guard<'a>(
    lines: impl Iterator<Item = &'a Polyline>,
    grid: &Grid,
    options: &BooleanOptions,
) -> Result<u64, Error> {
    let mut boxes: Vec<[i32; 4]> = Vec::new();
    for line in lines {
        if line.points.len() < 2 {
            continue;
        }
        let next = line.points.iter().cycle().skip(1);
        for (p, q) in line.points.iter().zip(next) {
            let (a, b) = (grid.int_of(*p), grid.int_of(*q));
            boxes.push([a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y)]);
        }
    }
    boxes.sort_unstable();
    let (mut pairs, mut steps) = (0u64, 0u64);
    let mut active: Vec<[i32; 4]> = Vec::new();
    for b in boxes {
        active.retain(|a| a[2] >= b[0]);
        for a in &active {
            steps = steps.saturating_add(1);
            if a[1] <= b[3] && b[1] <= a[3] {
                pairs = pairs.saturating_add(1);
            }
        }
        if pairs > options.max_candidate_pairs || steps > options.max_sweep_steps {
            return Err(Error::BudgetExceeded);
        }
        steps = steps.saturating_add(active.len() as u64);
        active.push(b);
    }
    Ok(pairs)
}
