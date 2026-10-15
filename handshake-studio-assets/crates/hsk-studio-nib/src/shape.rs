//! Parametric shape geometry (lane SMB-ILLUSTRATOR-PEN slice 2; STU-VEC-010, STU-VEC-127,
//! STU-VEC-066, STU-VEC-112): rectangle with four independent corner radii and corner kinds,
//! ellipse / arc (start and end angle, inner ratio: ring, donut, pie and segment are states of one
//! shape), polygon (side count, inset percentage), star (point count, inner radius ratio) and line
//! (length, angle), each as an [`AnchorPath`] in document units.
//!
//! Bounds are STU-VEC-127's where captured: rectangle width/height `0..=16348`, polygon
//! `side_count` `3..=100` (the normative hard range), `inset_percentage` `0..=100`, star
//! `point_count` `3..=50`. Bounds the spec marks `unknown` are checked for their physical domain only
//! (finite, radius and length non-negative, `inner_ratio` `0..1`, `inner_radius` positive). A
//! refusal names the parameter ([`ShapeError`]).
//!
//! Studio choices (Illustrator parity UNVERIFIED, D5): angles are degrees measured from +x towards
//! +y (clockwise on a y-down canvas); `end_angle - start_angle` a multiple of 360 is the full
//! ellipse; the first polygon and star vertex is at the top (-90 degrees); polygon inset places
//! inner vertices at `radius * cos(180 / n) * (1 - inset / 100)` so inset 0 is the plain polygon;
//! a rounded corner is a quarter-ellipse cubic with the circle constant `4 (sqrt 2 - 1) / 3`;
//! `fancy` corners and non-zero `corner_smoothing` are refused (their geometry is not specified).
//!
//! Trigonometry is a fixed polynomial over IEEE-exact operations ([`sin_cos_deg`]), so results are
//! identical on every platform; multiples of 90 degrees are exact.
#![deny(clippy::arithmetic_side_effects)]
use crate::anchor_path::{AnchorError, AnchorPath, Contour, EditAnchor};
use crate::owned_path::{COORDINATE_LIMIT, FillRule, PathPoint};
use crate::path_contract::Mirroring;

/// STU-VEC-127 rectangle width/height hard maximum.
pub const RECT_MAX: f64 = 16348.0;
/// STU-VEC-127 polygon side count hard range (normative 3..100; soft 3..20).
pub const POLYGON_SIDES: (u32, u32) = (3, 100);
/// STU-VEC-127 star point count hard range.
pub const STAR_POINTS: (u32, u32) = (3, 50);
/// Cubic circle constant `4 (sqrt 2 - 1) / 3`.
pub fn kappa() -> f64 {
    4.0 * (2.0_f64.sqrt() - 1.0) / 3.0
}

/// A refused shape parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShapeError {
    pub parameter: &'static str,
    pub error: AnchorError,
}

fn refuse(parameter: &'static str, error: AnchorError) -> ShapeError {
    ShapeError { parameter, error }
}

fn finite(parameter: &'static str, v: f64) -> Result<f64, ShapeError> {
    if !v.is_finite() {
        return Err(refuse(parameter, AnchorError::NonFinite));
    }
    if v.abs() > COORDINATE_LIMIT {
        return Err(refuse(parameter, AnchorError::OutOfRange));
    }
    Ok(v)
}

fn within(parameter: &'static str, v: f64, lo: f64, hi: f64) -> Result<f64, ShapeError> {
    let v = finite(parameter, v)?;
    if v < lo || v > hi {
        return Err(refuse(parameter, AnchorError::InvalidParameter));
    }
    Ok(v)
}

/// `(sin, cos)` of `deg` degrees: reduction to the nearest multiple of 90 (exact `%` and `round`),
/// then Taylor polynomials to degree 19 / 18 on `|x| <= pi / 4` (error below 1e-17).
pub fn sin_cos_deg(deg: f64) -> (f64, f64) {
    let mut d = deg % 360.0;
    if d < 0.0 {
        d += 360.0;
    }
    let q = (d / 90.0).round();
    let x = (d - q * 90.0) * (std::f64::consts::PI / 180.0);
    let x2 = x * x;
    let mut s = 1.0;
    let mut c = 1.0;
    // Horner from the highest term: sin = x (1 - x2/(2*3) (1 - x2/(4*5) (...))).
    for k in (1..=9_u32).rev() {
        let kf = f64::from(k);
        let (a, b) = (2.0 * kf, 2.0 * kf + 1.0);
        s = 1.0 - x2 / (a * b) * s;
        c = 1.0 - x2 / ((a - 1.0) * a) * c;
    }
    let s = x * s;
    // q is 0..=4 after the reduction; 4 is a full turn.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let quadrant = (q as u8) & 3;
    let z = |v: f64| if v == 0.0 { 0.0 } else { v };
    let (s, c) = match quadrant {
        0 => (s, c),
        1 => (c, -s),
        2 => (-s, -c),
        _ => (-c, s),
    };
    (z(s), z(c))
}

fn closed(anchors: Vec<EditAnchor>) -> AnchorPath {
    AnchorPath { contours: vec![Contour { anchors, closed: true }], fill_rule: FillRule::NonZero }
}

fn finish(path: AnchorPath) -> Result<AnchorPath, ShapeError> {
    path.validate().map_err(|e| refuse("geometry", e))?;
    Ok(path)
}

/// STU-VEC-112 corner kinds (captured values in [`CornerKind::capture_value`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CornerKind {
    None,
    Round,
    RoundInverted,
    Inset,
    Bevel,
    Fancy,
}

impl CornerKind {
    pub const fn capture_value(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Round => "rounded corner",
            Self::RoundInverted => "inverse rounded corner",
            Self::Inset => "inset corner",
            Self::Bevel => "bevel corner",
            Self::Fancy => "fancy corner",
        }
    }
    pub fn from_capture_value(v: &str) -> Option<Self> {
        [Self::None, Self::Round, Self::RoundInverted, Self::Inset, Self::Bevel, Self::Fancy].into_iter().find(|k| k.capture_value() == v)
    }
}

/// A rectangle: top-left `(x, y)`, size, per-corner radius `[top_left, top_right, bottom_right,
/// bottom_left]`, one corner kind, corner smoothing percent (only 0 is built).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RectSpec {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub corner_radius: [f64; 4],
    pub corner_kind: CornerKind,
    pub corner_smoothing: f64,
}

impl RectSpec {
    pub const fn plain(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self { x, y, width, height, corner_radius: [0.0; 4], corner_kind: CornerKind::None, corner_smoothing: 0.0 }
    }
}

/// The rectangle and the radii actually applied (STU-VEC-066: `min(requested, shorter incident
/// edge / 2)` per corner, reported back to the caller).
pub fn rectangle(spec: &RectSpec) -> Result<(AnchorPath, [f64; 4]), ShapeError> {
    let (x, y) = (finite("x", spec.x)?, finite("y", spec.y)?);
    let w = within("width", spec.width, 0.0, RECT_MAX)?;
    let h = within("height", spec.height, 0.0, RECT_MAX)?;
    if spec.corner_kind == CornerKind::Fancy {
        return Err(refuse("corner_kind", AnchorError::InvalidParameter));
    }
    if within("corner_smoothing", spec.corner_smoothing, 0.0, 100.0)? != 0.0 {
        return Err(refuse("corner_smoothing", AnchorError::InvalidParameter));
    }
    let half = w.min(h) / 2.0;
    let mut applied = [0.0; 4];
    for (a, r) in applied.iter_mut().zip(spec.corner_radius) {
        *a = within("corner_radius", r, 0.0, f64::MAX)?.min(half);
    }
    if spec.corner_kind == CornerKind::None {
        applied = [0.0; 4];
    }
    // Corners clockwise on a y-down canvas, each with its incoming and outgoing edge direction.
    let corners = [
        (PathPoint::new(x, y), PathPoint::new(0.0, -1.0), PathPoint::new(1.0, 0.0)),
        (PathPoint::new(x + w, y), PathPoint::new(1.0, 0.0), PathPoint::new(0.0, 1.0)),
        (PathPoint::new(x + w, y + h), PathPoint::new(0.0, 1.0), PathPoint::new(-1.0, 0.0)),
        (PathPoint::new(x, y + h), PathPoint::new(-1.0, 0.0), PathPoint::new(0.0, -1.0)),
    ];
    let k = kappa();
    let mut anchors = Vec::with_capacity(12);
    for ((c, din, dout), r) in corners.into_iter().zip(applied) {
        if r == 0.0 {
            anchors.push(EditAnchor::corner(c.x, c.y));
            continue;
        }
        let p1 = PathPoint::new(c.x - din.x * r, c.y - din.y * r);
        let p2 = PathPoint::new(c.x + dout.x * r, c.y + dout.y * r);
        let mut a1 = EditAnchor::corner(p1.x, p1.y);
        let mut a2 = EditAnchor::corner(p2.x, p2.y);
        match spec.corner_kind {
            CornerKind::Round => {
                a1.outgoing = PathPoint::new(din.x * r * k, din.y * r * k);
                a2.incoming = PathPoint::new(-dout.x * r * k, -dout.y * r * k);
                anchors.extend([a1, a2]);
            }
            CornerKind::RoundInverted => {
                a1.outgoing = PathPoint::new(dout.x * r * k, dout.y * r * k);
                a2.incoming = PathPoint::new(-din.x * r * k, -din.y * r * k);
                anchors.extend([a1, a2]);
            }
            CornerKind::Bevel => anchors.extend([a1, a2]),
            CornerKind::Inset => {
                let q = PathPoint::new(p1.x + dout.x * r, p1.y + dout.y * r);
                anchors.extend([a1, EditAnchor::corner(q.x, q.y), a2]);
            }
            CornerKind::None | CornerKind::Fancy => anchors.push(EditAnchor::corner(c.x, c.y)),
        }
    }
    Ok((finish(closed(anchors))?, applied))
}

/// An ellipse / arc: centre, radii, angles (degrees) and inner ratio `0..1`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EllipseSpec {
    pub cx: f64,
    pub cy: f64,
    pub rx: f64,
    pub ry: f64,
    pub start_angle: f64,
    pub end_angle: f64,
    pub inner_ratio: f64,
}

impl EllipseSpec {
    pub const fn full(cx: f64, cy: f64, rx: f64, ry: f64) -> Self {
        Self { cx, cy, rx, ry, start_angle: 0.0, end_angle: 360.0, inner_ratio: 0.0 }
    }
}

/// Anchors of the elliptical arc from `start` sweeping `sweep` degrees (|sweep| <= 360), split into
/// pieces of at most 90 degrees; handle length `4/3 tan(piece / 4)` of the tangent.
fn arc(cx: f64, cy: f64, rx: f64, ry: f64, start: f64, sweep: f64) -> Vec<EditAnchor> {
    let pieces = (sweep.abs() / 90.0).ceil().max(1.0);
    let step = sweep / pieces;
    let (s4, c4) = sin_cos_deg(step / 4.0);
    let k = 4.0 / 3.0 * (s4 / c4);
    // `pieces` is 1..=4.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = pieces as u8;
    let mut out = Vec::with_capacity(usize::from(n).saturating_add(1));
    for i in 0..=n {
        let a = start + step * f64::from(i);
        let (s, c) = sin_cos_deg(a);
        let tangent = PathPoint::new(-rx * s * k, ry * c * k);
        let mut anchor = EditAnchor::corner(cx + rx * c, cy + ry * s);
        if i > 0 {
            anchor.incoming = PathPoint::new(-tangent.x, -tangent.y);
        }
        if i < n {
            anchor.outgoing = tangent;
        }
        if i > 0 && i < n {
            anchor.mirroring = if rx == ry || s == 0.0 || c == 0.0 { Mirroring::AngleAndLength } else { Mirroring::Angle };
        }
        out.push(anchor);
    }
    out
}

/// The ellipse family: full ellipse (one contour), ring / donut (outer and reversed inner contour),
/// pie (arc and centre) and segment (outer arc and reversed inner arc).
pub fn ellipse(spec: &EllipseSpec) -> Result<AnchorPath, ShapeError> {
    let (cx, cy) = (finite("cx", spec.cx)?, finite("cy", spec.cy)?);
    let rx = within("rx", spec.rx, 0.0, COORDINATE_LIMIT)?;
    let ry = within("ry", spec.ry, 0.0, COORDINATE_LIMIT)?;
    let start = finite("start_angle", spec.start_angle)?;
    let end = finite("end_angle", spec.end_angle)?;
    let inner = finite("inner_ratio", spec.inner_ratio)?;
    if !(0.0..1.0).contains(&inner) {
        return Err(refuse("inner_ratio", AnchorError::InvalidParameter));
    }
    let sweep = end - start;
    let full = sweep % 360.0 == 0.0;
    let (irx, iry) = (rx * inner, ry * inner);
    let mut path = AnchorPath::new(FillRule::NonZero);
    if full {
        let mut outer = arc(cx, cy, rx, ry, start, 360.0);
        close_full(&mut outer);
        path.contours.push(Contour { anchors: outer, closed: true });
        if inner > 0.0 {
            let mut hole = arc(cx, cy, irx, iry, start, -360.0);
            close_full(&mut hole);
            path.contours.push(Contour { anchors: hole, closed: true });
        }
        return finish(path);
    }
    let sweep = sweep % 360.0;
    let mut anchors = arc(cx, cy, rx, ry, start, sweep);
    if inner > 0.0 {
        anchors.extend(arc(cx, cy, irx, iry, start + sweep, -sweep));
    } else {
        anchors.push(EditAnchor::corner(cx, cy));
    }
    path.contours.push(Contour { anchors, closed: true });
    finish(path)
}

/// A full arc repeats its first anchor at the end: merge it (incoming handle) and mark the first
/// anchor smooth.
fn close_full(anchors: &mut Vec<EditAnchor>) {
    if let Some(last) = anchors.pop()
        && let Some(first) = anchors.first_mut()
    {
        first.incoming = last.incoming;
        first.mirroring = crate::anchor_path::infer_mirroring(first.incoming, first.outgoing);
    }
}

fn vertex(cx: f64, cy: f64, r: f64, deg: f64) -> EditAnchor {
    let (s, c) = sin_cos_deg(deg);
    EditAnchor::corner(cx + r * c, cy + r * s)
}

/// A regular polygon of `side_count` sides (STU-VEC-127 hard 3..=100) inscribed in `radius`, first
/// vertex at the top; `inset_percentage` (0..=100) adds an inner vertex per side (star-ness).
pub fn polygon(cx: f64, cy: f64, radius: f64, side_count: u32, inset_percentage: f64) -> Result<AnchorPath, ShapeError> {
    let (cx, cy) = (finite("cx", cx)?, finite("cy", cy)?);
    let r = within("radius", radius, 0.0, COORDINATE_LIMIT)?;
    if !(POLYGON_SIDES.0..=POLYGON_SIDES.1).contains(&side_count) {
        return Err(refuse("side_count", AnchorError::InvalidParameter));
    }
    let inset = within("inset_percentage", inset_percentage, 0.0, 100.0)?;
    let n = f64::from(side_count);
    let (_, half_cos) = sin_cos_deg(180.0 / n);
    let inner = r * half_cos * (1.0 - inset / 100.0);
    let mut anchors = Vec::with_capacity(usize::try_from(side_count).unwrap_or(0).saturating_mul(2));
    for i in 0..side_count {
        let a = -90.0 + 360.0 * f64::from(i) / n;
        anchors.push(vertex(cx, cy, r, a));
        if inset > 0.0 {
            anchors.push(vertex(cx, cy, inner, a + 180.0 / n));
        }
    }
    finish(closed(anchors))
}

/// A star of `point_count` points (STU-VEC-127 hard 3..=50): outer radius = the object's size,
/// `inner_radius` a positive ratio of it.
pub fn star(cx: f64, cy: f64, radius: f64, point_count: u32, inner_radius: f64) -> Result<AnchorPath, ShapeError> {
    let (cx, cy) = (finite("cx", cx)?, finite("cy", cy)?);
    let r = within("radius", radius, 0.0, COORDINATE_LIMIT)?;
    if !(STAR_POINTS.0..=STAR_POINTS.1).contains(&point_count) {
        return Err(refuse("point_count", AnchorError::InvalidParameter));
    }
    let ratio = finite("inner_radius", inner_radius)?;
    if ratio <= 0.0 {
        return Err(refuse("inner_radius", AnchorError::InvalidParameter));
    }
    let n = f64::from(point_count);
    let mut anchors = Vec::with_capacity(usize::try_from(point_count).unwrap_or(0).saturating_mul(2));
    for i in 0..point_count {
        let a = -90.0 + 360.0 * f64::from(i) / n;
        anchors.push(vertex(cx, cy, r, a));
        anchors.push(vertex(cx, cy, r * ratio, a + 180.0 / n));
    }
    finish(closed(anchors))
}

/// A line segment from `(x, y)` of `length` (non-negative) at `angle` degrees: one open contour.
pub fn line(x: f64, y: f64, length: f64, angle: f64) -> Result<AnchorPath, ShapeError> {
    let (x, y) = (finite("x", x)?, finite("y", y)?);
    let l = within("length", length, 0.0, COORDINATE_LIMIT)?;
    let (s, c) = sin_cos_deg(finite("angle", angle)?);
    let path = AnchorPath {
        contours: vec![Contour { anchors: vec![EditAnchor::corner(x, y), EditAnchor::corner(x + l * c, y + l * s)], closed: false }],
        fill_rule: FillRule::NonZero,
    };
    finish(path)
}
