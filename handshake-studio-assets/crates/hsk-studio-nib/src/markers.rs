//! Stroke end markers (arrowheads; lane SMB-AI-ARROWHEADS-1, STU-VEC-146, STU-VEC-043): filled
//! marker outlines at the ends of every open subpath of a stroked path, and the trimmed path the
//! stroke must use under `inside_path`. [`StrokeStyle`](crate::stroke::StrokeStyle) gains no field;
//! the caller unions the marker outlines with the stroke outline of [`MarkerOutlines::trimmed`]
//! (so a dashed stroke never dashes a marker).
//!
//! Frame: at an end, `d` is the unit direction pointing out of the path (the end tangent at the
//! end, the reversed start tangent at the start; STU-VEC-043: the path direction decides which end
//! is the start) and `n = (-d.y, d.x)`. A shape is drawn in local units `(u, v)` of the marker size
//! `s = stroke_width * scale / 100` with its tip at `u = 0` and its body at `u < 0`; a local point maps
//! to `end + (u + shift) s d + v s n`. `arrow_align.inside_path`: `shift = 0` (the tip sits at the
//! path end) and the path is trimmed by `trim * s` of arc length at that end. `arrow_align.outside_path`:
//! `shift = length` (the marker starts at the end, its tip lies `length * s` beyond it) and the path is
//! not trimmed. A degenerate end tangent uses the first non-zero derivative of that end segment
//! (cubic: `p3 - p2`, then `p3 - p1`, then `p3 - p0`; start mirrored); zero-length end segments are
//! skipped; a subpath with no direction at a marked end is refused (`marker_degenerate_tangent`).
//! Closed subpaths have no ends and get no marker; they are listed in
//! [`MarkerOutlines::closed_subpaths`]. An open subpath that its two trims consume entirely is left
//! out of the trimmed path and listed in [`MarkerOutlines::consumed_subpaths`].
//!
//! Shape table (STU-VEC-146 names the tokens only; every proportion below is a Studio reading,
//! UNVERIFIED (D5), as are Illustrator's default scale and its "align" semantics). Units of `s`;
//! `length` = extent behind the tip, `trim` = the inside-path trim; hollow shapes are even-odd.
//!
//! | token | shape (local `(u, v)`) | length | trim |
//! |---|---|---|---|
//! | `arrow.simple` | chevron `(0,0) (-3,1.5) (-3,0.5) (-1,0) (-3,-0.5) (-3,-1.5)` | 3 | 1 |
//! | `arrow.simple_wide` | chevron `(0,0) (-3,3) (-3,2) (-1,0) (-3,-2) (-3,-3)` | 3 | 1 |
//! | `arrow.triangle` | triangle `(0,0) (-3,1.5) (-3,-1.5)` | 3 | 3 |
//! | `arrow.triangle_wide` | triangle `(0,0) (-3,3) (-3,-3)` | 3 | 3 |
//! | `arrow.barbed` | `(0,0) (-3,1.5) (-2,0) (-3,-1.5)` | 3 | 2 |
//! | `arrow.curved` | barbed with quadratic flanks, controls `(-1.5,±0.25)` | 3 | 2 |
//! | `arrow.circle` | ring centre `(-1.5,0)`, radii 1.5 and 1 | 3 | 2.75 |
//! | `arrow.circle_solid` | disc centre `(-1.5,0)`, radius 1.5 | 3 | 1.5 |
//! | `arrow.square` | square `[-3,0] x [-1.5,1.5]` less `[-2.5,-0.5] x [-1,1]` | 3 | 2.75 |
//! | `arrow.square_solid` | square `[-3,0] x [-1.5,1.5]` | 3 | 1.5 |
//! | `arrow.bar` | bar `[-1,0] x [-2,2]` | 1 | 0.5 |
//! | `ROUND` | disc centre `(-0.5,0)`, radius 0.5 | 1 | 0.5 |
//! | `SQUARE` | square `[-1,0] x [-0.5,0.5]` | 1 | 0.5 |
//! | `ARROW_LINES` | chevron `(0,0) (-2.5,2.5) (-2.5,1.5) (-1,0) (-2.5,-1.5) (-2.5,-2.5)` | 2.5 | 1 |
//! | `ARROW_EQUILATERAL` | triangle `(0,0) (-1.5 sqrt 3, 1.5) (-1.5 sqrt 3, -1.5)` | 1.5 sqrt 3 | 1.5 sqrt 3 |
//! | `TRIANGLE_FILLED` | triangle `(0,0) (-2,1) (-2,-1)` | 2 | 2 |
//! | `DIAMOND_FILLED` | diamond `(0,0) (-1.5,1.5) (-3,0) (-1.5,-1.5)` | 3 | 1.5 |
//! | `CIRCLE_FILLED` | disc centre `(-1.5,0)`, radius 1.5 | 3 | 1.5 |
//!
//! `arrow.none` / `NONE` are no marker. The six `ERD_*` connector terminators are refused
//! (`marker_shape_unbuilt`); no substitute is drawn. Circles are four cubic quarter arcs (handle
//! factor `4 (sqrt 2 - 1) / 3`). Straight trims are exact; curve trims use kurbo's arc length
//! (`MarkerOptions::arclen_accuracy`).
#![deny(clippy::arithmetic_side_effects)]
use crate::owned_path::{COORDINATE_LIMIT, FillRule, OwnedPath, PathElement, PathLimits, PathPoint};
use kurbo::{CubicBez, ParamCurve, ParamCurveArclen, Point, QuadBez};

/// Smallest marker scale, percent (STU-VEC-146 hard minimum).
pub const MARKER_SCALE_MIN: f64 = 1.0;
/// Largest marker scale, percent (STU-VEC-146 hard maximum).
pub const MARKER_SCALE_MAX: f64 = 1000.0;
/// Handle factor of a cubic quarter circle.
const KAPPA: f64 = 0.552_284_749_830_793_4;
const SQRT_3: f64 = 1.732_050_807_568_877_2;

/// A typed marker refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerError {
    /// The path fails [`OwnedPath::validate`] (non-finite or out-of-limit points, structure).
    InvalidGeometry,
    /// The path has more elements than the limits allow.
    BudgetExceeded,
    /// Stroke width not finite, not positive or above [`COORDINATE_LIMIT`].
    InvalidWidth,
    /// A scale outside `1..=1000` percent, or not finite.
    ScaleOutOfRange,
    /// A token outside the STU-VEC-146 catalogue.
    UnknownToken,
    /// A catalogue token with no built shape (the `ERD_*` terminators).
    ShapeUnbuilt,
    /// A marked end of a subpath with no direction (zero-length subpath).
    DegenerateTangent,
    /// A marker point beyond [`COORDINATE_LIMIT`].
    OutOfRange,
    /// `arclen_accuracy` not finite or outside `1e-12..=1`.
    InvalidOptions,
}
impl MarkerError {
    /// Stable code.
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidGeometry => "invalid_geometry",
            Self::BudgetExceeded => "budget_exceeded",
            Self::InvalidWidth => "marker_stroke_width",
            Self::ScaleOutOfRange => "marker_scale_out_of_range",
            Self::UnknownToken => "marker_unknown_token",
            Self::ShapeUnbuilt => "marker_shape_unbuilt",
            Self::DegenerateTangent => "marker_degenerate_tangent",
            Self::OutOfRange => "marker_out_of_range",
            Self::InvalidOptions => "marker_options",
        }
    }
}

/// The drawn marker shapes: 11 of the layout suite and 7 of the collaborative suite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerShape {
    Simple,
    SimpleWide,
    Triangle,
    TriangleWide,
    Barbed,
    Curved,
    Circle,
    CircleSolid,
    Square,
    SquareSolid,
    Bar,
    CapRound,
    CapSquare,
    ArrowLines,
    ArrowEquilateral,
    TriangleFilled,
    DiamondFilled,
    CircleFilled,
}

/// Inside-path / outside-path (STU-VEC-146 `arrow_alignment`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerAlign {
    InsidePath,
    OutsidePath,
}

/// The six connector terminators: in the catalogue, not built.
const UNBUILT: [&str; 6] = [
    "ERD_ONE",
    "ERD_MANY",
    "ERD_ONE_OR_MORE",
    "ERD_ZERO_OR_MORE",
    "ERD_ZERO_OR_ONE",
    "ERD_EXACTLY_ONE",
];

impl MarkerShape {
    /// Every built shape.
    pub const ALL: [Self; 18] = [
        Self::Simple,
        Self::SimpleWide,
        Self::Triangle,
        Self::TriangleWide,
        Self::Barbed,
        Self::Curved,
        Self::Circle,
        Self::CircleSolid,
        Self::Square,
        Self::SquareSolid,
        Self::Bar,
        Self::CapRound,
        Self::CapSquare,
        Self::ArrowLines,
        Self::ArrowEquilateral,
        Self::TriangleFilled,
        Self::DiamondFilled,
        Self::CircleFilled,
    ];

    /// The STU-VEC-146 token of the shape.
    pub const fn token(self) -> &'static str {
        match self {
            Self::Simple => "arrow.simple",
            Self::SimpleWide => "arrow.simple_wide",
            Self::Triangle => "arrow.triangle",
            Self::TriangleWide => "arrow.triangle_wide",
            Self::Barbed => "arrow.barbed",
            Self::Curved => "arrow.curved",
            Self::Circle => "arrow.circle",
            Self::CircleSolid => "arrow.circle_solid",
            Self::Square => "arrow.square",
            Self::SquareSolid => "arrow.square_solid",
            Self::Bar => "arrow.bar",
            Self::CapRound => "ROUND",
            Self::CapSquare => "SQUARE",
            Self::ArrowLines => "ARROW_LINES",
            Self::ArrowEquilateral => "ARROW_EQUILATERAL",
            Self::TriangleFilled => "TRIANGLE_FILLED",
            Self::DiamondFilled => "DIAMOND_FILLED",
            Self::CircleFilled => "CIRCLE_FILLED",
        }
    }

    /// The shape of a catalogue token: `Ok(None)` for `arrow.none` / `NONE`,
    /// [`MarkerError::ShapeUnbuilt`] for an `ERD_*` terminator, [`MarkerError::UnknownToken`]
    /// otherwise.
    pub fn from_token(token: &str) -> Result<Option<Self>, MarkerError> {
        if token == "arrow.none" || token == "NONE" {
            return Ok(None);
        }
        if UNBUILT.contains(&token) {
            return Err(MarkerError::ShapeUnbuilt);
        }
        Self::ALL
            .into_iter()
            .find(|s| s.token() == token)
            .map(Some)
            .ok_or(MarkerError::UnknownToken)
    }

    /// Extent behind the tip, in units of the marker size.
    pub const fn length(self) -> f64 {
        match self {
            Self::Bar | Self::CapRound | Self::CapSquare => 1.0,
            Self::ArrowLines => 2.5,
            Self::ArrowEquilateral => 1.5 * SQRT_3,
            Self::TriangleFilled => 2.0,
            _ => 3.0,
        }
    }

    /// Inside-path trim, in units of the marker size.
    pub const fn trim(self) -> f64 {
        match self {
            Self::Simple | Self::SimpleWide | Self::ArrowLines => 1.0,
            Self::Triangle | Self::TriangleWide => 3.0,
            Self::Barbed | Self::Curved => 2.0,
            Self::Circle | Self::Square => 2.75,
            Self::CircleSolid | Self::SquareSolid | Self::DiamondFilled | Self::CircleFilled => 1.5,
            Self::Bar | Self::CapRound | Self::CapSquare => 0.5,
            Self::ArrowEquilateral => 1.5 * SQRT_3,
            Self::TriangleFilled => 2.0,
        }
    }

    /// The local outline (module table) and its fill rule.
    fn local(self) -> (Vec<Local>, FillRule) {
        use Local::{Close, Line, Move, Quad};
        let polygon = |pts: &[(f64, f64)]| -> Vec<Local> {
            let mut out = Vec::with_capacity(pts.len().saturating_add(1));
            for (i, &p) in pts.iter().enumerate() {
                out.push(if i == 0 { Move(p) } else { Line(p) });
            }
            out.push(Close);
            out
        };
        let rect = |u0: f64, u1: f64, v: f64| polygon(&[(u0, -v), (u1, -v), (u1, v), (u0, v)]);
        let even = FillRule::EvenOdd;
        let non = FillRule::NonZero;
        match self {
            Self::Simple => (polygon(&[(0.0, 0.0), (-3.0, 1.5), (-3.0, 0.5), (-1.0, 0.0), (-3.0, -0.5), (-3.0, -1.5)]), non),
            Self::SimpleWide => (polygon(&[(0.0, 0.0), (-3.0, 3.0), (-3.0, 2.0), (-1.0, 0.0), (-3.0, -2.0), (-3.0, -3.0)]), non),
            Self::Triangle => (polygon(&[(0.0, 0.0), (-3.0, 1.5), (-3.0, -1.5)]), non),
            Self::TriangleWide => (polygon(&[(0.0, 0.0), (-3.0, 3.0), (-3.0, -3.0)]), non),
            Self::Barbed => (polygon(&[(0.0, 0.0), (-3.0, 1.5), (-2.0, 0.0), (-3.0, -1.5)]), non),
            Self::Curved => (
                vec![
                    Move((0.0, 0.0)),
                    Quad((-1.5, 0.25), (-3.0, 1.5)),
                    Line((-2.0, 0.0)),
                    Line((-3.0, -1.5)),
                    Quad((-1.5, -0.25), (0.0, 0.0)),
                    Close,
                ],
                non,
            ),
            Self::Circle => {
                let mut c = circle((-1.5, 0.0), 1.5);
                c.extend(circle((-1.5, 0.0), 1.0));
                (c, even)
            }
            Self::CircleSolid | Self::CircleFilled => (circle((-1.5, 0.0), 1.5), non),
            Self::Square => {
                let mut c = rect(-3.0, 0.0, 1.5);
                c.extend(rect(-2.5, -0.5, 1.0));
                (c, even)
            }
            Self::SquareSolid => (rect(-3.0, 0.0, 1.5), non),
            Self::Bar => (rect(-1.0, 0.0, 2.0), non),
            Self::CapRound => (circle((-0.5, 0.0), 0.5), non),
            Self::CapSquare => (rect(-1.0, 0.0, 0.5), non),
            Self::ArrowLines => (polygon(&[(0.0, 0.0), (-2.5, 2.5), (-2.5, 1.5), (-1.0, 0.0), (-2.5, -1.5), (-2.5, -2.5)]), non),
            Self::ArrowEquilateral => {
                let l = 1.5 * SQRT_3;
                (polygon(&[(0.0, 0.0), (-l, 1.5), (-l, -1.5)]), non)
            }
            Self::TriangleFilled => (polygon(&[(0.0, 0.0), (-2.0, 1.0), (-2.0, -1.0)]), non),
            Self::DiamondFilled => (polygon(&[(0.0, 0.0), (-1.5, 1.5), (-3.0, 0.0), (-1.5, -1.5)]), non),
        }
    }
}

/// A local outline element (units of the marker size).
#[derive(Clone, Copy)]
enum Local {
    Move((f64, f64)),
    Line((f64, f64)),
    Quad((f64, f64), (f64, f64)),
    Cubic((f64, f64), (f64, f64), (f64, f64)),
    Close,
}

/// A full circle as four cubic quarter arcs, counter-clockwise from `(cu + r, cv)`.
fn circle(c: (f64, f64), r: f64) -> Vec<Local> {
    let k = KAPPA * r;
    let (x, y) = c;
    vec![
        Local::Move((x + r, y)),
        Local::Cubic((x + r, y + k), (x + k, y + r), (x, y + r)),
        Local::Cubic((x - k, y + r), (x - r, y + k), (x - r, y)),
        Local::Cubic((x - r, y - k), (x - k, y - r), (x, y - r)),
        Local::Cubic((x + k, y - r), (x + r, y - k), (x + r, y)),
        Local::Close,
    ]
}

/// One end's marker.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarkerEnd {
    pub shape: MarkerShape,
    /// Percent of the stroke width, `1..=1000`.
    pub scale: f64,
}

/// The markers of one stroke: independent start and end (STU-VEC-146), one alignment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarkerSpec {
    pub start: Option<MarkerEnd>,
    pub end: Option<MarkerEnd>,
    pub align: MarkerAlign,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarkerOptions {
    /// Structural limits the input path is validated with.
    pub limits: PathLimits,
    /// Arc-length accuracy of curve trims, path units, `1e-12..=1`.
    pub arclen_accuracy: f64,
}
impl Default for MarkerOptions {
    fn default() -> Self {
        Self {
            limits: PathLimits::default(),
            arclen_accuracy: 1e-9,
        }
    }
}

/// One drawn marker.
#[derive(Clone, Debug, PartialEq)]
pub struct MarkerPlacement {
    /// Index of the subpath (in `MoveTo` order).
    pub subpath: usize,
    /// `true` at the subpath start, `false` at its end.
    pub at_start: bool,
    pub shape: MarkerShape,
    /// Marker size `stroke_width * scale / 100`.
    pub size: f64,
    /// The path end the marker belongs to.
    pub end: PathPoint,
    /// Unit direction out of the path at that end.
    pub direction: (f64, f64),
    /// The filled outline (closed; even-odd for hollow shapes).
    pub outline: OwnedPath,
}

/// The markers of a path and the path the stroke must use.
#[derive(Clone, Debug, PartialEq)]
pub struct MarkerOutlines {
    pub markers: Vec<MarkerPlacement>,
    /// The input path with every inside-path end trimmed (unchanged under outside-path or without
    /// markers); same fill rule.
    pub trimmed: OwnedPath,
    /// Closed subpaths: no ends, no markers (reported, not silent).
    pub closed_subpaths: Vec<usize>,
    /// Open subpaths the trims consumed entirely (left out of `trimmed`).
    pub consumed_subpaths: Vec<usize>,
}
impl MarkerOutlines {
    /// Every marker outline.
    pub fn outlines(&self) -> Vec<OwnedPath> {
        self.markers.iter().map(|m| m.outline.clone()).collect()
    }
}

/// One segment of a subpath.
#[derive(Clone, Copy, Debug)]
enum Seg {
    Line(PathPoint, PathPoint),
    Quad(PathPoint, PathPoint, PathPoint),
    Cubic(PathPoint, PathPoint, PathPoint, PathPoint),
}

fn kp(p: PathPoint) -> Point {
    Point::new(p.x, p.y)
}
fn pp(p: Point) -> PathPoint {
    PathPoint::new(p.x, p.y)
}
fn norm(x: f64, y: f64) -> f64 {
    (x * x + y * y).sqrt()
}
fn direction(from: PathPoint, to: PathPoint) -> Option<(f64, f64)> {
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let l = norm(dx, dy);
    (l > 0.0 && l.is_finite()).then(|| (dx / l, dy / l))
}

impl Seg {
    fn end(self) -> PathPoint {
        match self {
            Self::Line(_, b) | Self::Quad(_, _, b) | Self::Cubic(_, _, _, b) => b,
        }
    }
    /// Direction of travel at the end: the first non-zero derivative.
    fn end_direction(self) -> Option<(f64, f64)> {
        match self {
            Self::Line(a, b) => direction(a, b),
            Self::Quad(a, c, b) => direction(c, b).or_else(|| direction(a, b)),
            Self::Cubic(a, c1, c2, b) => direction(c2, b)
                .or_else(|| direction(c1, b))
                .or_else(|| direction(a, b)),
        }
    }
    /// Direction of travel at the start: the first non-zero derivative.
    fn start_direction(self) -> Option<(f64, f64)> {
        match self {
            Self::Line(a, b) => direction(a, b),
            Self::Quad(a, c, b) => direction(a, c).or_else(|| direction(a, b)),
            Self::Cubic(a, c1, c2, b) => direction(a, c1)
                .or_else(|| direction(a, c2))
                .or_else(|| direction(a, b)),
        }
    }
    fn length(self, accuracy: f64) -> f64 {
        match self {
            Self::Line(a, b) => norm(b.x - a.x, b.y - a.y),
            Self::Quad(a, c, b) => QuadBez::new(kp(a), kp(c), kp(b)).arclen(accuracy),
            Self::Cubic(a, c1, c2, b) => {
                CubicBez::new(kp(a), kp(c1), kp(c2), kp(b)).arclen(accuracy)
            }
        }
    }
    /// The part between arc lengths `from` and `to` (`0 <= from <= to <= len`).
    fn part(self, from: f64, to: f64, len: f64, accuracy: f64) -> Self {
        if from <= 0.0 && to >= len {
            return self;
        }
        match self {
            Self::Line(a, b) => {
                let at = |s: f64| {
                    if s <= 0.0 {
                        a
                    } else if s >= len {
                        b
                    } else {
                        let t = s / len;
                        PathPoint::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
                    }
                };
                Self::Line(at(from), at(to))
            }
            Self::Quad(a, c, b) => {
                let q = QuadBez::new(kp(a), kp(c), kp(b));
                let t0 = if from <= 0.0 { 0.0 } else { q.inv_arclen(from, accuracy) };
                let t1 = if to >= len { 1.0 } else { q.inv_arclen(to, accuracy) };
                let s = q.subsegment(t0..t1);
                Self::Quad(pp(s.p0), pp(s.p1), pp(s.p2))
            }
            Self::Cubic(a, c1, c2, b) => {
                let q = CubicBez::new(kp(a), kp(c1), kp(c2), kp(b));
                let t0 = if from <= 0.0 { 0.0 } else { q.inv_arclen(from, accuracy) };
                let t1 = if to >= len { 1.0 } else { q.inv_arclen(to, accuracy) };
                let s = q.subsegment(t0..t1);
                Self::Cubic(pp(s.p0), pp(s.p1), pp(s.p2), pp(s.p3))
            }
        }
    }
    fn push(self, out: &mut Vec<PathElement>) {
        out.push(match self {
            Self::Line(_, b) => PathElement::LineTo(b),
            Self::Quad(_, c, b) => PathElement::QuadTo(c, b),
            Self::Cubic(_, c1, c2, b) => PathElement::CubicTo(c1, c2, b),
        });
    }
}

/// A subpath: its start, segments and whether it ends in `Close`.
struct Subpath {
    start: PathPoint,
    segs: Vec<Seg>,
    closed: bool,
}

fn subpaths(path: &OwnedPath) -> Vec<Subpath> {
    let mut out: Vec<Subpath> = Vec::new();
    let mut pen = PathPoint::new(0.0, 0.0);
    for e in &path.elements {
        let current = out.last_mut();
        match (*e, current) {
            (PathElement::MoveTo(p), _) => {
                out.push(Subpath { start: p, segs: Vec::new(), closed: false });
                pen = p;
            }
            (PathElement::LineTo(p), Some(s)) => {
                s.segs.push(Seg::Line(pen, p));
                pen = p;
            }
            (PathElement::QuadTo(c, p), Some(s)) => {
                s.segs.push(Seg::Quad(pen, c, p));
                pen = p;
            }
            (PathElement::CubicTo(c1, c2, p), Some(s)) => {
                s.segs.push(Seg::Cubic(pen, c1, c2, p));
                pen = p;
            }
            (PathElement::Close, Some(s)) => {
                s.closed = true;
                pen = s.start;
            }
            // `validate` guarantees a leading MoveTo.
            (_, None) => {}
        }
    }
    out
}

fn check_scale(end: Option<MarkerEnd>) -> Result<(), MarkerError> {
    match end {
        Some(e) if !e.scale.is_finite() || !(MARKER_SCALE_MIN..=MARKER_SCALE_MAX).contains(&e.scale) => {
            Err(MarkerError::ScaleOutOfRange)
        }
        _ => Ok(()),
    }
}

/// The marker outline of `end` at `p` with outward direction `d`.
fn place(
    end: MarkerEnd,
    p: PathPoint,
    d: (f64, f64),
    stroke_width: f64,
    align: MarkerAlign,
) -> Result<(OwnedPath, f64), MarkerError> {
    let s = stroke_width * end.scale / 100.0;
    let shift = match align {
        MarkerAlign::InsidePath => 0.0,
        MarkerAlign::OutsidePath => end.shape.length(),
    };
    let n = (-d.1, d.0);
    let map = |(u, v): (f64, f64)| -> Result<PathPoint, MarkerError> {
        let a = (u + shift) * s;
        let b = v * s;
        let q = PathPoint::new(p.x + a * d.0 + b * n.0, p.y + a * d.1 + b * n.1);
        if q.x.is_finite() && q.y.is_finite() && q.x.abs() <= COORDINATE_LIMIT && q.y.abs() <= COORDINATE_LIMIT {
            Ok(q)
        } else {
            Err(MarkerError::OutOfRange)
        }
    };
    let (local, fill_rule) = end.shape.local();
    let mut elements = Vec::with_capacity(local.len());
    for e in local {
        elements.push(match e {
            Local::Move(a) => PathElement::MoveTo(map(a)?),
            Local::Line(a) => PathElement::LineTo(map(a)?),
            Local::Quad(c, a) => PathElement::QuadTo(map(c)?, map(a)?),
            Local::Cubic(c1, c2, a) => PathElement::CubicTo(map(c1)?, map(c2)?, map(a)?),
            Local::Close => PathElement::Close,
        });
    }
    let trim = match align {
        MarkerAlign::InsidePath => end.shape.trim() * s,
        MarkerAlign::OutsidePath => 0.0,
    };
    Ok((OwnedPath { elements, fill_rule }, trim))
}

/// Marker outlines at the start and end of every open subpath of `path` (module docs), and the
/// trimmed path the stroke of width `stroke_width` must use.
pub fn marker_outlines(
    path: &OwnedPath,
    spec: &MarkerSpec,
    stroke_width: f64,
    options: &MarkerOptions,
) -> Result<MarkerOutlines, MarkerError> {
    let accuracy = options.arclen_accuracy;
    if !accuracy.is_finite() || !(1e-12..=1.0).contains(&accuracy) {
        return Err(MarkerError::InvalidOptions);
    }
    if path.elements.len() > options.limits.max_elements {
        return Err(MarkerError::BudgetExceeded);
    }
    path.validate(&options.limits)
        .map_err(|_| MarkerError::InvalidGeometry)?;
    if !stroke_width.is_finite() || stroke_width <= 0.0 || stroke_width > COORDINATE_LIMIT {
        return Err(MarkerError::InvalidWidth);
    }
    check_scale(spec.start)?;
    check_scale(spec.end)?;
    let mut out = MarkerOutlines {
        markers: Vec::new(),
        trimmed: OwnedPath::new(path.fill_rule),
        closed_subpaths: Vec::new(),
        consumed_subpaths: Vec::new(),
    };
    for (index, sub) in subpaths(path).into_iter().enumerate() {
        if sub.closed {
            out.closed_subpaths.push(index);
            out.trimmed.elements.push(PathElement::MoveTo(sub.start));
            for seg in &sub.segs {
                seg.push(&mut out.trimmed.elements);
            }
            out.trimmed.elements.push(PathElement::Close);
            continue;
        }
        let mut trims = [0.0_f64; 2];
        for (k, (end, at_start)) in [(spec.start, true), (spec.end, false)].into_iter().enumerate() {
            let Some(end) = end else { continue };
            let (point, dir) = if at_start {
                let d = sub
                    .segs
                    .iter()
                    .find_map(|s| s.start_direction())
                    .ok_or(MarkerError::DegenerateTangent)?;
                (sub.start, (-d.0, -d.1))
            } else {
                let d = sub
                    .segs
                    .iter()
                    .rev()
                    .find_map(|s| s.end_direction())
                    .ok_or(MarkerError::DegenerateTangent)?;
                (sub.segs.last().map_or(sub.start, |s| s.end()), d)
            };
            let (outline, trim) = place(end, point, dir, stroke_width, spec.align)?;
            if let Some(t) = trims.get_mut(k) {
                *t = trim;
            }
            out.markers.push(MarkerPlacement {
                subpath: index,
                at_start,
                shape: end.shape,
                size: stroke_width * end.scale / 100.0,
                end: point,
                direction: dir,
                outline,
            });
        }
        let [head, tail] = trims;
        if head <= 0.0 && tail <= 0.0 {
            out.trimmed.elements.push(PathElement::MoveTo(sub.start));
            for seg in &sub.segs {
                seg.push(&mut out.trimmed.elements);
            }
            continue;
        }
        let lengths: Vec<f64> = sub.segs.iter().map(|s| s.length(accuracy)).collect();
        let total: f64 = lengths.iter().sum();
        let (from, to) = (head, total - tail);
        if to <= from {
            out.consumed_subpaths.push(index);
            continue;
        }
        let mut acc = 0.0;
        let mut started = false;
        for (seg, &len) in sub.segs.iter().zip(&lengths) {
            let (a, b) = (acc, acc + len);
            acc = b;
            if b < from || a > to || (len > 0.0 && (b <= from || a >= to)) {
                continue;
            }
            let part = seg.part((from - a).max(0.0), (to - a).min(len), len, accuracy);
            if !started {
                let first = match part {
                    Seg::Line(p, _) | Seg::Quad(p, _, _) | Seg::Cubic(p, _, _, _) => p,
                };
                out.trimmed.elements.push(PathElement::MoveTo(first));
                started = true;
            }
            part.push(&mut out.trimmed.elements);
        }
        if !started {
            out.consumed_subpaths.push(index);
        }
    }
    out.trimmed
        .validate(&PathLimits { max_elements: usize::MAX, require_closed: false })
        .map_err(|_| MarkerError::OutOfRange)?;
    Ok(out)
}
