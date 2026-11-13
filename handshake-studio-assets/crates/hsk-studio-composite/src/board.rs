//! Board drawing (lane SMB-BOARD-1; [STU-WB-104] to [STU-WB-109], [STU-WB-115]): the geometry a board
//! node draws with, as Nib paths in document units, from its Folio board record. Pure functions: a host
//! stores the result as the node's vector path, so a board renders, exports and reopens through the
//! ordinary vector path of [`crate::prepare_vectors`] (no board-private renderer).
//!
//! - Shapes ([`shape_outline`]): the twelve `studio_declared` natives and the `figma_plugin_api` tokens
//!   that map to a native construction; any other token is `shape_type_geometry_not_available` (the
//!   token is kept by Folio, never relabelled: [STU-WB-105]).
//! - Connectors: [`anchor`] resolves a magnet on a node's bounds (`AUTO` picks the side facing the other
//!   end), [`route`] draws `STRAIGHT` and `ELBOWED` (`CURVED` is `connector_line_type_not_available`), and
//!   [`connector_drawing`] turns the route and both caps into one even-odd path meant to be filled and
//!   stroked in one colour: each route segment is its own two-point open contour (no fill area), a filled
//!   cap is a closed contour, a hollow ring is two contours.
//!
//! Studio readings, declared (FigJam geometry UNVERIFIED, D5): shape proportions, the elbow route (one or
//! two bends, no obstacle avoidance), the cap size (`max(6 x stroke width, 8)`) and every cap drawing.
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_folio::{StudioConnectorCap, StudioConnectorLineType, StudioConnectorMagnet, StudioShapeType, StudioShapeVocabulary};
use hsk_studio_nib::{FillRule, OwnedPath, PathElement, PathPoint};

/// The cubic circle constant.
const KAPPA: f64 = 0.552_284_749_830_793_4;

/// A typed refusal of the board drawing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoardGeometryError {
    /// `CURVED` routing is not drawn in this build.
    LineTypeNotAvailable,
    /// The shape token has no native construction in this build.
    ShapeGeometryNotAvailable,
    /// A non-finite or non-positive size, or a degenerate route.
    Invalid,
}

impl BoardGeometryError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::LineTypeNotAvailable => "connector_line_type_not_available",
            Self::ShapeGeometryNotAvailable => "shape_type_geometry_not_available",
            Self::Invalid => "board_geometry_invalid",
        }
    }
}

/// An axis-aligned rectangle, document units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoardRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl BoardRect {
    pub fn center(&self) -> [f64; 2] {
        [self.x + self.w / 2.0, self.y + self.h / 2.0]
    }

    fn valid(&self) -> bool {
        [self.x, self.y, self.w, self.h].iter().all(|v| v.is_finite()) && self.w > 0.0 && self.h > 0.0
    }

    /// The point at fractions `(u, v)` of the rectangle.
    fn at(&self, u: f64, v: f64) -> PathPoint {
        PathPoint::new(self.x + self.w * u, self.y + self.h * v)
    }
}

/// The side an endpoint leaves its node from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoardSide {
    Top,
    Right,
    Bottom,
    Left,
}

impl BoardSide {
    fn horizontal(self) -> bool {
        matches!(self, Self::Left | Self::Right)
    }

    pub const fn token(self) -> &'static str {
        match self {
            Self::Top => "TOP",
            Self::Right => "RIGHT",
            Self::Bottom => "BOTTOM",
            Self::Left => "LEFT",
        }
    }
}

/// A resolved connector end: its point and, when it sits on a node side, that side.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoardEnd {
    pub point: [f64; 2],
    pub side: Option<BoardSide>,
}

/// The magnet `magnet` of a node with bounds `rect`; `AUTO` takes the side facing `toward`.
pub fn anchor(rect: &BoardRect, magnet: StudioConnectorMagnet, toward: [f64; 2]) -> BoardEnd {
    let [cx, cy] = rect.center();
    let side = match magnet {
        StudioConnectorMagnet::Top => BoardSide::Top,
        StudioConnectorMagnet::Right => BoardSide::Right,
        StudioConnectorMagnet::Bottom => BoardSide::Bottom,
        StudioConnectorMagnet::Left => BoardSide::Left,
        StudioConnectorMagnet::Center => return BoardEnd { point: [cx, cy], side: None },
        StudioConnectorMagnet::Auto => {
            let (dx, dy) = (toward[0] - cx, toward[1] - cy);
            if dx.abs() * rect.h >= dy.abs() * rect.w {
                if dx >= 0.0 { BoardSide::Right } else { BoardSide::Left }
            } else if dy >= 0.0 {
                BoardSide::Bottom
            } else {
                BoardSide::Top
            }
        }
    };
    let point = match side {
        BoardSide::Top => [cx, rect.y],
        BoardSide::Right => [rect.x + rect.w, cy],
        BoardSide::Bottom => [cx, rect.y + rect.h],
        BoardSide::Left => [rect.x, cy],
    };
    BoardEnd { point, side: Some(side) }
}

/// The route of a connector from `a` to `b`: `STRAIGHT` one segment, `ELBOWED` axis-parallel segments
/// leaving each end along its side (an end without a side leaves along the larger offset).
pub fn route(a: BoardEnd, b: BoardEnd, line: StudioConnectorLineType) -> Result<Vec<[f64; 2]>, BoardGeometryError> {
    if !a.point.iter().chain(b.point.iter()).all(|v| v.is_finite()) {
        return Err(BoardGeometryError::Invalid);
    }
    let [ax, ay] = a.point;
    let [bx, by] = b.point;
    let points = match line {
        StudioConnectorLineType::Curved => return Err(BoardGeometryError::LineTypeNotAvailable),
        StudioConnectorLineType::Straight => vec![a.point, b.point],
        StudioConnectorLineType::Elbowed => {
            let inferred = (bx - ax).abs() >= (by - ay).abs();
            let ha = a.side.map_or(inferred, BoardSide::horizontal);
            let hb = b.side.map_or(inferred, BoardSide::horizontal);
            match (ha, hb) {
                (true, true) => {
                    let mx = (ax + bx) / 2.0;
                    vec![a.point, [mx, ay], [mx, by], b.point]
                }
                (false, false) => {
                    let my = (ay + by) / 2.0;
                    vec![a.point, [ax, my], [bx, my], b.point]
                }
                (true, false) => vec![a.point, [bx, ay], b.point],
                (false, true) => vec![a.point, [ax, by], b.point],
            }
        }
    };
    Ok(simplify(points))
}

/// Drops repeated points and middle points on a straight run.
fn simplify(points: Vec<[f64; 2]>) -> Vec<[f64; 2]> {
    let mut out: Vec<[f64; 2]> = Vec::with_capacity(points.len());
    for p in points {
        if out.last() == Some(&p) {
            continue;
        }
        if let [.., a, b] = out.as_slice() {
            let cross = (b[0] - a[0]) * (p[1] - b[1]) - (b[1] - a[1]) * (p[0] - b[0]);
            let forward = (b[0] - a[0]) * (p[0] - b[0]) + (b[1] - a[1]) * (p[1] - b[1]);
            if cross == 0.0 && forward >= 0.0 {
                out.pop();
            }
        }
        out.push(p);
    }
    out
}

fn pt(p: [f64; 2]) -> PathPoint {
    PathPoint::new(p[0], p[1])
}

fn polygon(path: &mut OwnedPath, points: &[PathPoint]) {
    let mut it = points.iter();
    let Some(first) = it.next() else { return };
    path.elements.push(PathElement::MoveTo(*first));
    path.elements.extend(it.map(|p| PathElement::LineTo(*p)));
    path.elements.push(PathElement::Close);
}

fn line(path: &mut OwnedPath, a: PathPoint, b: PathPoint) {
    path.elements.push(PathElement::MoveTo(a));
    path.elements.push(PathElement::LineTo(b));
}

/// A closed ellipse with center `c` and radii `rx`, `ry` (four cubics, clockwise on screen).
fn ellipse(path: &mut OwnedPath, c: [f64; 2], rx: f64, ry: f64) {
    let (kx, ky) = (rx * KAPPA, ry * KAPPA);
    let [x, y] = c;
    path.elements.push(PathElement::MoveTo(PathPoint::new(x + rx, y)));
    path.elements.push(PathElement::CubicTo(PathPoint::new(x + rx, y + ky), PathPoint::new(x + kx, y + ry), PathPoint::new(x, y + ry)));
    path.elements.push(PathElement::CubicTo(PathPoint::new(x - kx, y + ry), PathPoint::new(x - rx, y + ky), PathPoint::new(x - rx, y)));
    path.elements.push(PathElement::CubicTo(PathPoint::new(x - rx, y - ky), PathPoint::new(x - kx, y - ry), PathPoint::new(x, y - ry)));
    path.elements.push(PathElement::CubicTo(PathPoint::new(x + kx, y - ry), PathPoint::new(x + rx, y - ky), PathPoint::new(x + rx, y)));
    path.elements.push(PathElement::Close);
}

/// A closed rectangle with corner radius `r` (clamped to half the short side).
fn rounded(path: &mut OwnedPath, b: &BoardRect, r: f64) {
    let r = r.clamp(0.0, b.w.min(b.h) / 2.0);
    if r == 0.0 {
        polygon(path, &[b.at(0.0, 0.0), b.at(1.0, 0.0), b.at(1.0, 1.0), b.at(0.0, 1.0)]);
        return;
    }
    let k = r * (1.0 - KAPPA);
    let (x0, y0, x1, y1) = (b.x, b.y, b.x + b.w, b.y + b.h);
    let p = PathPoint::new;
    path.elements.push(PathElement::MoveTo(p(x0 + r, y0)));
    path.elements.push(PathElement::LineTo(p(x1 - r, y0)));
    path.elements.push(PathElement::CubicTo(p(x1 - k, y0), p(x1, y0 + k), p(x1, y0 + r)));
    path.elements.push(PathElement::LineTo(p(x1, y1 - r)));
    path.elements.push(PathElement::CubicTo(p(x1, y1 - k), p(x1 - k, y1), p(x1 - r, y1)));
    path.elements.push(PathElement::LineTo(p(x0 + r, y1)));
    path.elements.push(PathElement::CubicTo(p(x0 + k, y1), p(x0, y1 - k), p(x0, y1 - r)));
    path.elements.push(PathElement::LineTo(p(x0, y0 + r)));
    path.elements.push(PathElement::CubicTo(p(x0, y0 + k), p(x0 + k, y0), p(x0 + r, y0)));
    path.elements.push(PathElement::Close);
}

/// The native construction a shape token draws with.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Native {
    Rectangle,
    RoundedRectangle,
    Ellipse,
    Diamond,
    TriangleUp,
    TriangleDown,
    ParallelogramRight,
    ParallelogramLeft,
    Hexagon,
    Cylinder,
    Cloud,
    Document,
    PredefinedProcess,
    Terminator,
    Polygon(&'static [(f64, f64)]),
    InternalStorage,
    CircleCross,
}

const TRAPEZOID: &[(f64, f64)] = &[(0.2, 0.0), (0.8, 0.0), (1.0, 1.0), (0.0, 1.0)];
const PENTAGON: &[(f64, f64)] = &[(0.5, 0.0), (1.0, 0.38), (0.81, 1.0), (0.19, 1.0), (0.0, 0.38)];
const OCTAGON: &[(f64, f64)] = &[(0.29, 0.0), (0.71, 0.0), (1.0, 0.29), (1.0, 0.71), (0.71, 1.0), (0.29, 1.0), (0.0, 0.71), (0.0, 0.29)];
const STAR: &[(f64, f64)] = &[(0.5, 0.0), (0.61, 0.35), (0.98, 0.35), (0.68, 0.57), (0.79, 0.91), (0.5, 0.70), (0.21, 0.91), (0.32, 0.57), (0.02, 0.35), (0.39, 0.35)];
const PLUS: &[(f64, f64)] = &[(0.35, 0.0), (0.65, 0.0), (0.65, 0.35), (1.0, 0.35), (1.0, 0.65), (0.65, 0.65), (0.65, 1.0), (0.35, 1.0), (0.35, 0.65), (0.0, 0.65), (0.0, 0.35), (0.35, 0.35)];
const CHEVRON: &[(f64, f64)] = &[(0.0, 0.0), (0.75, 0.0), (1.0, 0.5), (0.75, 1.0), (0.0, 1.0), (0.25, 0.5)];
const ARROW_RIGHT: &[(f64, f64)] = &[(0.0, 0.25), (0.6, 0.25), (0.6, 0.0), (1.0, 0.5), (0.6, 1.0), (0.6, 0.75), (0.0, 0.75)];
const ARROW_LEFT: &[(f64, f64)] = &[(1.0, 0.25), (0.4, 0.25), (0.4, 0.0), (0.0, 0.5), (0.4, 1.0), (0.4, 0.75), (1.0, 0.75)];
const MANUAL_INPUT: &[(f64, f64)] = &[(0.0, 0.25), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];

fn native(shape: &StudioShapeType) -> Option<Native> {
    Some(match (shape.vocabulary, shape.token.as_str()) {
        (StudioShapeVocabulary::StudioDeclared, t) => match t {
            "rectangle" => Native::Rectangle,
            "rounded_rectangle" => Native::RoundedRectangle,
            "ellipse" => Native::Ellipse,
            "diamond" => Native::Diamond,
            "triangle" => Native::TriangleUp,
            "parallelogram" => Native::ParallelogramRight,
            "hexagon" => Native::Hexagon,
            "cylinder" => Native::Cylinder,
            "cloud" => Native::Cloud,
            "document" => Native::Document,
            "predefined_process" => Native::PredefinedProcess,
            "terminator" => Native::Terminator,
            _ => return None,
        },
        (StudioShapeVocabulary::FigmaPluginApi, t) => match t {
            "SQUARE" => Native::Rectangle,
            "ROUNDED_RECTANGLE" => Native::RoundedRectangle,
            "ELLIPSE" => Native::Ellipse,
            "DIAMOND" => Native::Diamond,
            "TRIANGLE_UP" => Native::TriangleUp,
            "TRIANGLE_DOWN" => Native::TriangleDown,
            "PARALLELOGRAM_RIGHT" => Native::ParallelogramRight,
            "PARALLELOGRAM_LEFT" => Native::ParallelogramLeft,
            "HEXAGON" => Native::Hexagon,
            "ENG_DATABASE" => Native::Cylinder,
            "DOCUMENT_SINGLE" => Native::Document,
            "PREDEFINED_PROCESS" => Native::PredefinedProcess,
            "TRAPEZOID" => Native::Polygon(TRAPEZOID),
            "PENTAGON" => Native::Polygon(PENTAGON),
            "OCTAGON" => Native::Polygon(OCTAGON),
            "STAR" => Native::Polygon(STAR),
            "PLUS" => Native::Polygon(PLUS),
            "CHEVRON" => Native::Polygon(CHEVRON),
            "ARROW_RIGHT" => Native::Polygon(ARROW_RIGHT),
            "ARROW_LEFT" => Native::Polygon(ARROW_LEFT),
            "MANUAL_INPUT" => Native::Polygon(MANUAL_INPUT),
            "INTERNAL_STORAGE" => Native::InternalStorage,
            "SUMMING_JUNCTION" | "OR" => Native::CircleCross,
            _ => return None,
        },
    })
}

/// How much wider and taller than its text a shape draws so the text sits inside it (1 for the
/// rectangular shapes; Studio-declared).
pub fn text_fit(shape: &StudioShapeType) -> f64 {
    match native(shape) {
        Some(Native::Ellipse | Native::Diamond | Native::Cloud | Native::CircleCross | Native::Polygon(_)) => 1.5,
        Some(Native::TriangleUp | Native::TriangleDown) => 2.0,
        Some(Native::Hexagon | Native::ParallelogramRight | Native::ParallelogramLeft | Native::Terminator) => 1.25,
        _ => 1.0,
    }
}

/// The outline of shape `shape` in `rect` ([STU-WB-105]), closed contours only (the node's text
/// container; interior lines are [`shape_details`]); `corner_radius` applies to the rounded rectangle
/// (default a sixth of the short side).
pub fn shape_outline(shape: &StudioShapeType, rect: &BoardRect, corner_radius: Option<f64>) -> Result<OwnedPath, BoardGeometryError> {
    if !rect.valid() || corner_radius.is_some_and(|r| !r.is_finite() || r < 0.0) {
        return Err(BoardGeometryError::Invalid);
    }
    let kind = native(shape).ok_or(BoardGeometryError::ShapeGeometryNotAvailable)?;
    let mut p = OwnedPath::new(FillRule::NonZero);
    let b = rect;
    let [cx, cy] = b.center();
    match kind {
        Native::Rectangle => rounded(&mut p, b, corner_radius.unwrap_or(0.0)),
        Native::RoundedRectangle => rounded(&mut p, b, corner_radius.unwrap_or(b.w.min(b.h) / 6.0)),
        Native::Terminator => rounded(&mut p, b, b.w.min(b.h) / 2.0),
        Native::Ellipse => ellipse(&mut p, [cx, cy], b.w / 2.0, b.h / 2.0),
        Native::Diamond => polygon(&mut p, &[b.at(0.5, 0.0), b.at(1.0, 0.5), b.at(0.5, 1.0), b.at(0.0, 0.5)]),
        Native::TriangleUp => polygon(&mut p, &[b.at(0.5, 0.0), b.at(1.0, 1.0), b.at(0.0, 1.0)]),
        Native::TriangleDown => polygon(&mut p, &[b.at(0.0, 0.0), b.at(1.0, 0.0), b.at(0.5, 1.0)]),
        Native::ParallelogramRight => polygon(&mut p, &[b.at(0.2, 0.0), b.at(1.0, 0.0), b.at(0.8, 1.0), b.at(0.0, 1.0)]),
        Native::ParallelogramLeft => polygon(&mut p, &[b.at(0.0, 0.0), b.at(0.8, 0.0), b.at(1.0, 1.0), b.at(0.2, 1.0)]),
        Native::Hexagon => polygon(&mut p, &[b.at(0.2, 0.0), b.at(0.8, 0.0), b.at(1.0, 0.5), b.at(0.8, 1.0), b.at(0.2, 1.0), b.at(0.0, 0.5)]),
        Native::Polygon(fractions) => polygon(&mut p, &fractions.iter().map(|(u, v)| b.at(*u, *v)).collect::<Vec<_>>()),
        Native::PredefinedProcess | Native::InternalStorage => rounded(&mut p, b, 0.0),
        Native::CircleCross => ellipse(&mut p, [cx, cy], b.w / 2.0, b.h / 2.0),
        Native::Cylinder => {
            let rx = b.w / 2.0;
            let ry = (b.h * 0.15).min(rx);
            let (kx, ky) = (rx * KAPPA, ry * KAPPA);
            let (x0, x1, top, bottom) = (b.x, b.x + b.w, b.y + ry, b.y + b.h - ry);
            let q = PathPoint::new;
            p.elements.push(PathElement::MoveTo(q(x0, top)));
            p.elements.push(PathElement::LineTo(q(x0, bottom)));
            p.elements.push(PathElement::CubicTo(q(x0, bottom + ky), q(cx - kx, bottom + ry), q(cx, bottom + ry)));
            p.elements.push(PathElement::CubicTo(q(cx + kx, bottom + ry), q(x1, bottom + ky), q(x1, bottom)));
            p.elements.push(PathElement::LineTo(q(x1, top)));
            p.elements.push(PathElement::CubicTo(q(x1, top - ky), q(cx + kx, top - ry), q(cx, top - ry)));
            p.elements.push(PathElement::CubicTo(q(cx - kx, top - ry), q(x0, top - ky), q(x0, top)));
            p.elements.push(PathElement::Close);
        }
        Native::Cloud => {
            // Eight scallops around an inset ellipse, each bulging outward.
            let (rx, ry) = (b.w * 0.42, b.h * 0.40);
            let ring: Vec<[f64; 2]> = (0..8_u32)
                .map(|i| {
                    let t = f64::from(i) * std::f64::consts::FRAC_PI_4;
                    [cx + rx * t.cos(), cy + ry * t.sin()]
                })
                .collect();
            p.elements.push(PathElement::MoveTo(pt(ring[0])));
            for (i, a) in ring.iter().enumerate() {
                let next = ring.get(i.saturating_add(1)).copied().unwrap_or(ring[0]);
                let mid = [(a[0] + next[0]) / 2.0, (a[1] + next[1]) / 2.0];
                let (nx, ny) = (mid[0] - cx, mid[1] - cy);
                let len = nx.hypot(ny).max(f64::MIN_POSITIVE);
                let bulge = (next[0] - a[0]).hypot(next[1] - a[1]) * 0.5;
                let (ox, oy) = (nx / len * bulge, ny / len * bulge);
                p.elements.push(PathElement::CubicTo(PathPoint::new(a[0] + ox, a[1] + oy), PathPoint::new(next[0] + ox, next[1] + oy), pt(next)));
            }
            p.elements.push(PathElement::Close);
        }
        Native::Document => {
            let q = PathPoint::new;
            let (x0, x1, y0, wave) = (b.x, b.x + b.w, b.y, b.y + b.h * 0.85);
            p.elements.push(PathElement::MoveTo(q(x0, y0)));
            p.elements.push(PathElement::LineTo(q(x1, y0)));
            p.elements.push(PathElement::LineTo(q(x1, wave)));
            p.elements.push(PathElement::CubicTo(q(x0 + b.w * 0.75, b.y + b.h * 0.7), q(x0 + b.w * 0.5, b.y + b.h * 1.1), q(x0, wave)));
            p.elements.push(PathElement::Close);
        }
    }
    Ok(p)
}

/// The interior lines of shape `shape` in `rect` that are not part of its outline (the cylinder's front
/// rim, the predefined process's bars, the internal storage's rules, the summing junction's cross):
/// open contours drawn in the node's colours over its outline. `None` for a shape without any. The
/// outline alone is the node's text container (area text needs closed contours only).
pub fn shape_details(shape: &StudioShapeType, rect: &BoardRect) -> Option<OwnedPath> {
    if !rect.valid() {
        return None;
    }
    let b = rect;
    let [cx, cy] = b.center();
    let mut p = OwnedPath::new(FillRule::NonZero);
    match native(shape)? {
        Native::PredefinedProcess => {
            line(&mut p, b.at(0.1, 0.0), b.at(0.1, 1.0));
            line(&mut p, b.at(0.9, 0.0), b.at(0.9, 1.0));
        }
        Native::InternalStorage => {
            line(&mut p, b.at(0.15, 0.0), b.at(0.15, 1.0));
            line(&mut p, b.at(0.0, 0.15), b.at(1.0, 0.15));
        }
        Native::CircleCross => {
            let (dx, dy) = (b.w * 0.3536, b.h * 0.3536);
            line(&mut p, PathPoint::new(cx - dx, cy - dy), PathPoint::new(cx + dx, cy + dy));
            line(&mut p, PathPoint::new(cx - dx, cy + dy), PathPoint::new(cx + dx, cy - dy));
        }
        Native::Cylinder => {
            // The front rim of the top: an open arc inside the filled body (its implied fill adds no area).
            let rx = b.w / 2.0;
            let ry = (b.h * 0.15).min(rx);
            let (kx, ky) = (rx * KAPPA, ry * KAPPA);
            let (x0, x1, top) = (b.x, b.x + b.w, b.y + ry);
            let q = PathPoint::new;
            p.elements.push(PathElement::MoveTo(q(x0, top)));
            p.elements.push(PathElement::CubicTo(q(x0, top + ky), q(cx - kx, top + ry), q(cx, top + ry)));
            p.elements.push(PathElement::CubicTo(q(cx + kx, top + ry), q(x1, top + ky), q(x1, top)));
        }
        _ => return None,
    }
    Some(p)
}

/// The card of a sticky or the frame of a section: a rectangle with corner radius `r`.
pub fn card_outline(rect: &BoardRect, r: f64) -> Result<OwnedPath, BoardGeometryError> {
    if !rect.valid() || !r.is_finite() || r < 0.0 {
        return Err(BoardGeometryError::Invalid);
    }
    let mut p = OwnedPath::new(FillRule::NonZero);
    rounded(&mut p, rect, r);
    Ok(p)
}

/// The cap size for a stroke `width` (Studio-declared).
pub fn cap_size(width: f64) -> f64 {
    (width * 6.0).max(8.0)
}

fn unit(from: [f64; 2], to: [f64; 2]) -> Option<[f64; 2]> {
    let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
    let len = dx.hypot(dy);
    (len > 0.0 && len.is_finite()).then(|| [dx / len, dy / len])
}

/// Draws cap `cap` with its tip at `t`, pointing along unit `d` (outward), stroke `width`.
fn cap(path: &mut OwnedPath, cap: StudioConnectorCap, t: [f64; 2], d: [f64; 2], width: f64) {
    let s = cap_size(width);
    let n = [-d[1], d[0]];
    let at = |back: f64, side: f64| PathPoint::new(t[0] - d[0] * back + n[0] * side, t[1] - d[1] * back + n[1] * side);
    let bar = |path: &mut OwnedPath, back: f64| line(path, at(back, s / 2.0), at(back, -s / 2.0));
    let foot = |path: &mut OwnedPath| {
        line(path, at(s, 0.0), at(0.0, s / 2.0));
        line(path, at(s, 0.0), at(0.0, 0.0));
        line(path, at(s, 0.0), at(0.0, -s / 2.0));
    };
    let ring = |path: &mut OwnedPath, back: f64| {
        let r = s * 0.3;
        let c = [t[0] - d[0] * back, t[1] - d[1] * back];
        ellipse(path, c, r, r);
        let inner = (r - width).max(r * 0.4);
        ellipse(path, c, inner, inner);
    };
    match cap {
        StudioConnectorCap::None => {}
        StudioConnectorCap::ArrowEquilateral | StudioConnectorCap::TriangleFilled => polygon(path, &[at(0.0, 0.0), at(s, s * 0.577), at(s, -s * 0.577)]),
        StudioConnectorCap::ArrowLines => {
            line(path, at(s, s * 0.577), at(0.0, 0.0));
            line(path, at(s, -s * 0.577), at(0.0, 0.0));
        }
        StudioConnectorCap::CircleFilled => ellipse(path, [t[0] - d[0] * s / 2.0, t[1] - d[1] * s / 2.0], s / 2.0, s / 2.0),
        StudioConnectorCap::DiamondFilled => polygon(path, &[at(0.0, 0.0), at(s / 2.0, s / 3.0), at(s, 0.0), at(s / 2.0, -s / 3.0)]),
        StudioConnectorCap::ErdOne => bar(path, s * 0.5),
        StudioConnectorCap::ErdExactlyOne => {
            bar(path, s * 0.4);
            bar(path, s * 0.7);
        }
        StudioConnectorCap::ErdMany => foot(path),
        StudioConnectorCap::ErdOneOrMore => {
            foot(path);
            bar(path, s * 1.2);
        }
        StudioConnectorCap::ErdZeroOrOne => {
            bar(path, s * 0.5);
            ring(path, s * 1.3);
        }
        StudioConnectorCap::ErdZeroOrMore => {
            foot(path);
            ring(path, s * 1.4);
        }
    }
}

/// The drawing of a connector with route `points` (module docs): an even-odd path to fill and stroke
/// in the connector's colour at `width`.
pub fn connector_drawing(points: &[[f64; 2]], start: StudioConnectorCap, end: StudioConnectorCap, width: f64) -> Result<OwnedPath, BoardGeometryError> {
    if points.len() < 2 || !width.is_finite() || width <= 0.0 || !points.iter().flatten().all(|v| v.is_finite()) {
        return Err(BoardGeometryError::Invalid);
    }
    let mut p = OwnedPath::new(FillRule::EvenOdd);
    for w in points.windows(2) {
        if w[0] != w[1] {
            line(&mut p, pt(w[0]), pt(w[1]));
        }
    }
    if p.elements.is_empty() {
        return Err(BoardGeometryError::Invalid);
    }
    let (first, second) = (points[0], points[1]);
    if let Some(d) = unit(second, first) {
        cap(&mut p, start, first, d, width);
    }
    if let [.., before, last] = points
        && let Some(d) = unit(*before, *last)
    {
        cap(&mut p, end, *last, d, width);
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> BoardRect {
        BoardRect { x: 0.0, y: 0.0, w: 100.0, h: 50.0 }
    }

    #[test]
    fn magnets_sit_on_the_named_side_and_auto_faces_the_other_end() {
        let r = rect();
        assert_eq!(anchor(&r, StudioConnectorMagnet::Right, [0.0, 0.0]).point, [100.0, 25.0]);
        assert_eq!(anchor(&r, StudioConnectorMagnet::Top, [0.0, 0.0]).point, [50.0, 0.0]);
        assert_eq!(anchor(&r, StudioConnectorMagnet::Center, [0.0, 0.0]).side, None);
        assert_eq!(anchor(&r, StudioConnectorMagnet::Auto, [500.0, 30.0]).side, Some(BoardSide::Right));
        assert_eq!(anchor(&r, StudioConnectorMagnet::Auto, [-500.0, 30.0]).side, Some(BoardSide::Left));
        assert_eq!(anchor(&r, StudioConnectorMagnet::Auto, [50.0, 900.0]).side, Some(BoardSide::Bottom));
    }

    #[test]
    fn elbowed_routes_are_axis_parallel_and_straight_is_one_segment() {
        let a = BoardEnd { point: [100.0, 25.0], side: Some(BoardSide::Right) };
        let b = BoardEnd { point: [300.0, 125.0], side: Some(BoardSide::Left) };
        let r = route(a, b, StudioConnectorLineType::Elbowed).unwrap();
        assert_eq!(r, vec![[100.0, 25.0], [200.0, 25.0], [200.0, 125.0], [300.0, 125.0]]);
        assert!(r.windows(2).all(|w| w[0][0] == w[1][0] || w[0][1] == w[1][1]));
        assert_eq!(route(a, b, StudioConnectorLineType::Straight).unwrap().len(), 2);
        assert_eq!(route(a, b, StudioConnectorLineType::Curved), Err(BoardGeometryError::LineTypeNotAvailable));
        // Aligned ends collapse to one segment.
        let c = BoardEnd { point: [300.0, 25.0], side: Some(BoardSide::Left) };
        assert_eq!(route(a, c, StudioConnectorLineType::Elbowed).unwrap(), vec![[100.0, 25.0], [300.0, 25.0]]);
    }

    #[test]
    fn every_studio_shape_and_every_cap_draws_a_valid_path() {
        for token in hsk_studio_folio::STUDIO_SHAPE_TOKENS {
            let shape = StudioShapeType::studio(token);
            let p = shape_outline(&shape, &rect(), None).unwrap();
            let outline = hsk_studio_nib::anchor_path::AnchorPath::from_owned_path(&p).unwrap_or_else(|e| panic!("{token}: {e:?}"));
            // The outline is an area-text container: closed contours only.
            assert!(!outline.contours.is_empty() && outline.contours.iter().all(|c| c.closed), "{token}");
            if let Some(d) = shape_details(&shape, &rect()) {
                hsk_studio_nib::anchor_path::AnchorPath::from_owned_path(&d).unwrap_or_else(|e| panic!("{token} details: {e:?}"));
            }
        }
        assert!(shape_details(&StudioShapeType::studio("cylinder"), &rect()).is_some());
        assert!(shape_details(&StudioShapeType::studio("rectangle"), &rect()).is_none());
        let unmapped = StudioShapeType { vocabulary: StudioShapeVocabulary::FigmaPluginApi, vocabulary_version: Some("1.141.0".into()), token: "SHIELD".into() };
        assert_eq!(shape_outline(&unmapped, &rect(), None), Err(BoardGeometryError::ShapeGeometryNotAvailable));
        assert_eq!(shape_outline(&StudioShapeType::studio("rectangle"), &BoardRect { w: f64::NAN, ..rect() }, None), Err(BoardGeometryError::Invalid));
        let caps = [
            StudioConnectorCap::None,
            StudioConnectorCap::ArrowEquilateral,
            StudioConnectorCap::ArrowLines,
            StudioConnectorCap::TriangleFilled,
            StudioConnectorCap::CircleFilled,
            StudioConnectorCap::DiamondFilled,
            StudioConnectorCap::ErdOne,
            StudioConnectorCap::ErdMany,
            StudioConnectorCap::ErdOneOrMore,
            StudioConnectorCap::ErdZeroOrOne,
            StudioConnectorCap::ErdZeroOrMore,
            StudioConnectorCap::ErdExactlyOne,
        ];
        for c in caps {
            let p = connector_drawing(&[[0.0, 0.0], [50.0, 0.0], [50.0, 80.0]], c, c, 2.0).unwrap();
            hsk_studio_nib::anchor_path::AnchorPath::from_owned_path(&p).unwrap_or_else(|e| panic!("{c:?}: {e:?}"));
        }
        assert_eq!(connector_drawing(&[[0.0, 0.0]], StudioConnectorCap::None, StudioConnectorCap::None, 2.0), Err(BoardGeometryError::Invalid));
        assert_eq!(connector_drawing(&[[0.0, 0.0], [0.0, 0.0]], StudioConnectorCap::None, StudioConnectorCap::None, 2.0), Err(BoardGeometryError::Invalid));
    }
}
