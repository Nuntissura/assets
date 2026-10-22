//! Editable anchor path model (lane SMB-ILLUSTRATOR-PEN slice 1): the `StudioVectorPath` anchor
//! form of STU-VEC-001/002 that pen, node-select and shape tools edit, as pure functions.
//!
//! An [`AnchorPath`] is an ordered list of [`Contour`]s, each an ordered list of [`EditAnchor`]s and
//! an open/closed flag, plus the path's fill rule (STU-VEC-004/110). An anchor carries its position,
//! an incoming and an outgoing tangent handle (vectors relative to the anchor, document units) and
//! the STU-VEC-109 handle mirroring (`NONE`, `ANGLE`, `ANGLE_AND_LENGTH`); the two-member point type
//! (`smooth` = 1, `corner` = 2) is a derived read-only projection ([`EditAnchor::point_type`]).
//! Segment `i` of a contour runs from anchor `i` to anchor `i + 1` (a closed contour's last segment
//! returns to anchor 0) and is the cubic bounded by the outgoing handle of its start and the
//! incoming handle of its end; both zero = a straight segment (STU-VEC-003).
//!
//! Every operation validates its input, never mutates it, checks the anchor budget before it
//! allocates and returns a new validated path or a typed [`AnchorError`] whose STU-VEC-133 members
//! (`path_index_out_of_bounds`, `point_index_out_of_bounds`, `cannot_join_path_points`) are
//! distinct codes. Floating-point work uses IEEE-exact operations only (+ - * / sqrt), so results are
//! reproducible byte for byte.
//!
//! Studio choices, not verified Adobe behaviour (Illustrator parity UNVERIFIED, D5): a converted
//! smooth anchor without handles gets handles parallel to the chord of its neighbours with one third
//! of each neighbour distance; inserting an anchor on a curve gives it `ANGLE` mirroring; joining
//! coincident ends merges them into one `NONE` anchor, other ends are connected by a straight
//! segment; drag-to-bend moves both governing handles in proportion `(1 - t) : t`; deleting an
//! anchor keeps the outer handles (no refit).
#![deny(clippy::arithmetic_side_effects)]
use crate::owned_path::{COORDINATE_LIMIT, FillRule, OwnedPath, PathElement, PathLimits, PathPoint};
use crate::path_contract::Mirroring;

/// Most anchors one path holds (all contours).
pub const MAX_ANCHORS: usize = 65_536;
/// Most contours one path holds.
pub const MAX_CONTOURS: usize = 4096;
/// Default distance under which two ends are coincident for [`join`].
pub const DEFAULT_MERGE_DISTANCE: f64 = 1.0e-6;

/// Typed refusals. `code()` is the stable wire string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnchorError {
    /// STU-VEC-133 "path index out of bounds": no such contour.
    PathIndexOutOfBounds,
    /// STU-VEC-133 "point index out of bounds": no such anchor or segment.
    PointIndexOutOfBounds,
    /// STU-VEC-133 "cannot join path points": closed contour, the same end twice, or one anchor.
    CannotJoinPathPoints,
    /// A NaN or infinite coordinate, handle or parameter.
    NonFinite,
    /// A coordinate (or anchor + handle) beyond [`COORDINATE_LIMIT`].
    OutOfRange,
    /// More than [`MAX_ANCHORS`] anchors or [`MAX_CONTOURS`] contours.
    BudgetExceeded,
    /// A contour with no anchor, or a closed contour with fewer than two.
    Degenerate,
    /// A parameter outside its domain (segment parameter outside `0 < t < 1`, split at an end, ...).
    InvalidParameter,
    /// The open contour is already closed (pen extension of a closed contour).
    ContourClosed,
    /// The result would hold no anchor (STU-VEC-048 empty result, fail closed).
    EmptyResult,
    /// An owned path nib cannot express as anchors (quadratics are raised; this is for invalid input).
    InvalidGeometry,
}

impl AnchorError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::PathIndexOutOfBounds => "path_index_out_of_bounds",
            Self::PointIndexOutOfBounds => "point_index_out_of_bounds",
            Self::CannotJoinPathPoints => "cannot_join_path_points",
            Self::NonFinite => "non_finite",
            Self::OutOfRange => "out_of_range",
            Self::BudgetExceeded => "budget_exceeded",
            Self::Degenerate => "degenerate_contour",
            Self::InvalidParameter => "invalid_parameter",
            Self::ContourClosed => "contour_closed",
            Self::EmptyResult => "empty_result",
            Self::InvalidGeometry => "invalid_geometry",
        }
    }
}

/// The derived STU-VEC-109 point type (captured values `smooth` = 1, `corner` = 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointType {
    Smooth = 1,
    Corner = 2,
}

/// The STU-VEC-116 anchor selection state (captured values 1..=5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnchorSelection {
    None = 1,
    Anchor = 2,
    InHandle = 3,
    OutHandle = 4,
    BothHandles = 5,
}

/// One anchor: position and handles relative to it, document units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EditAnchor {
    pub position: PathPoint,
    pub incoming: PathPoint,
    pub outgoing: PathPoint,
    pub mirroring: Mirroring,
}

const ZERO: PathPoint = PathPoint { x: 0.0, y: 0.0 };

impl EditAnchor {
    /// A corner anchor without handles.
    pub const fn corner(x: f64, y: f64) -> Self {
        Self { position: PathPoint { x, y }, incoming: ZERO, outgoing: ZERO, mirroring: Mirroring::None }
    }
    /// A smooth anchor whose outgoing handle is (`dx`, `dy`) and incoming handle its mirror (pen drag).
    pub const fn smooth(x: f64, y: f64, dx: f64, dy: f64) -> Self {
        Self {
            position: PathPoint { x, y },
            incoming: PathPoint { x: -dx, y: -dy },
            outgoing: PathPoint { x: dx, y: dy },
            mirroring: Mirroring::AngleAndLength,
        }
    }
    /// STU-VEC-109 projection: `NONE` or zero-length handles are `corner`, the other members `smooth`.
    pub fn point_type(&self) -> PointType {
        if self.mirroring == Mirroring::None || (is_zero(self.incoming) && is_zero(self.outgoing)) {
            PointType::Corner
        } else {
            PointType::Smooth
        }
    }
    /// Absolute position of a handle end.
    pub fn handle_point(&self, side: HandleSide) -> PathPoint {
        let h = match side {
            HandleSide::In => self.incoming,
            HandleSide::Out => self.outgoing,
        };
        add(self.position, h)
    }
}

/// One open or closed contour.
#[derive(Clone, Debug, PartialEq)]
pub struct Contour {
    pub anchors: Vec<EditAnchor>,
    pub closed: bool,
}

impl Contour {
    /// Number of segments: `n - 1` open, `n` closed.
    pub fn segment_count(&self) -> usize {
        if self.closed { self.anchors.len() } else { self.anchors.len().saturating_sub(1) }
    }
}

/// The editable path.
#[derive(Clone, Debug, PartialEq)]
pub struct AnchorPath {
    pub contours: Vec<Contour>,
    pub fill_rule: FillRule,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnchorAddr {
    pub contour: usize,
    pub anchor: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleSide {
    In,
    Out,
}

/// An end of an open contour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    Start,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EndAddr {
    pub contour: usize,
    pub end: End,
}

/// Drag constraint (Shift): `Angle45` snaps the drag direction to a multiple of 45 degrees and keeps
/// its length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Constraint {
    Free,
    Angle45,
}

/// A hit on an anchor part (node-select tool).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnchorHit {
    pub at: AnchorAddr,
    /// `Anchor`, `InHandle` or `OutHandle`.
    pub part: AnchorSelection,
}

/// A hit on a segment: contour, segment index and curve parameter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SegmentHit {
    pub contour: usize,
    pub segment: usize,
    pub t: f64,
    pub distance: f64,
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
/// Exact quadratic-to-cubic handle: `2 v / 3` (multiply first, so integral inputs stay exact).
fn two_thirds(v: PathPoint) -> PathPoint {
    PathPoint::new(v.x * 2.0 / 3.0, v.y * 2.0 / 3.0)
}
fn neg(a: PathPoint) -> PathPoint {
    PathPoint::new(-a.x, -a.y)
}
fn len(a: PathPoint) -> f64 {
    (a.x * a.x + a.y * a.y).sqrt()
}
fn is_zero(a: PathPoint) -> bool {
    a.x == 0.0 && a.y == 0.0
}
fn lerp(a: PathPoint, b: PathPoint, t: f64) -> PathPoint {
    PathPoint::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}
/// `-0` written as `0` so equal geometry compares and serialises equal.
fn clean(p: PathPoint) -> PathPoint {
    let z = |v: f64| if v == 0.0 { 0.0 } else { v };
    PathPoint::new(z(p.x), z(p.y))
}

/// Snaps vector `v` to the nearest multiple of 45 degrees, keeping its length. The eight unit
/// directions are exact (the diagonal is `sqrt(0.5)`), and the nearest one is the one with the
/// largest dot product, so no trigonometry is involved.
pub fn snap45(v: PathPoint) -> PathPoint {
    let l = len(v);
    if l == 0.0 {
        return v;
    }
    let d = 0.5_f64.sqrt();
    let dirs = [(1.0, 0.0), (d, d), (0.0, 1.0), (-d, d), (-1.0, 0.0), (-d, -d), (0.0, -1.0), (d, -d)];
    let mut best = dirs[0];
    let mut best_dot = f64::NEG_INFINITY;
    for (x, y) in dirs {
        let dot = v.x * x + v.y * y;
        if dot > best_dot {
            best_dot = dot;
            best = (x, y);
        }
    }
    clean(PathPoint::new(best.0 * l, best.1 * l))
}

fn constrain(v: PathPoint, c: Constraint) -> PathPoint {
    match c {
        Constraint::Free => v,
        Constraint::Angle45 => snap45(v),
    }
}

fn finite_point(p: PathPoint) -> Result<(), AnchorError> {
    if !(p.x.is_finite() && p.y.is_finite()) {
        return Err(AnchorError::NonFinite);
    }
    if p.x.abs() > COORDINATE_LIMIT || p.y.abs() > COORDINATE_LIMIT {
        return Err(AnchorError::OutOfRange);
    }
    Ok(())
}

fn check_anchor(a: &EditAnchor) -> Result<(), AnchorError> {
    finite_point(a.position)?;
    finite_point(a.incoming)?;
    finite_point(a.outgoing)?;
    // The control points the owned path carries must be in range too.
    finite_point(add(a.position, a.incoming))?;
    finite_point(add(a.position, a.outgoing))
}

// ---- model --------------------------------------------------------------------------------------

impl AnchorPath {
    pub const fn new(fill_rule: FillRule) -> Self {
        Self { contours: Vec::new(), fill_rule }
    }

    /// Anchors over all contours (saturating; validation bounds it).
    pub fn anchor_count(&self) -> usize {
        self.contours.iter().fold(0_usize, |n, c| n.saturating_add(c.anchors.len()))
    }

    /// Budgets first ([`MAX_CONTOURS`], [`MAX_ANCHORS`]), then structure (every contour has an
    /// anchor, a closed contour at least two) and values (finite, within [`COORDINATE_LIMIT`],
    /// anchor + handle included).
    pub fn validate(&self) -> Result<(), AnchorError> {
        if self.contours.len() > MAX_CONTOURS || self.anchor_count() > MAX_ANCHORS {
            return Err(AnchorError::BudgetExceeded);
        }
        for c in &self.contours {
            if c.anchors.is_empty() || (c.closed && c.anchors.len() < 2) {
                return Err(AnchorError::Degenerate);
            }
            c.anchors.iter().try_for_each(check_anchor)?;
        }
        Ok(())
    }

    pub fn contour(&self, i: usize) -> Result<&Contour, AnchorError> {
        self.contours.get(i).ok_or(AnchorError::PathIndexOutOfBounds)
    }

    pub fn anchor(&self, at: AnchorAddr) -> Result<&EditAnchor, AnchorError> {
        self.contour(at.contour)?.anchors.get(at.anchor).ok_or(AnchorError::PointIndexOutOfBounds)
    }

    /// The four control points of segment `segment` of contour `contour`.
    pub fn segment(&self, contour: usize, segment: usize) -> Result<[PathPoint; 4], AnchorError> {
        let c = self.contour(contour)?;
        if segment >= c.segment_count() {
            return Err(AnchorError::PointIndexOutOfBounds);
        }
        let a = c.anchors.get(segment).ok_or(AnchorError::PointIndexOutOfBounds)?;
        let next = segment.checked_add(1).ok_or(AnchorError::PointIndexOutOfBounds)?;
        let b = c.anchors.get(next.checked_rem(c.anchors.len()).ok_or(AnchorError::Degenerate)?).ok_or(AnchorError::PointIndexOutOfBounds)?;
        Ok([a.position, a.handle_point(HandleSide::Out), b.handle_point(HandleSide::In), b.position])
    }

    /// The owned (element) form nib's boolean, stroke, flatten and raster functions take: one
    /// `MoveTo` per contour, a `LineTo` for a straight segment, else a `CubicTo`; a closed contour
    /// ends in `Close` (its straight closing segment is the implicit one). A one-anchor contour is a
    /// lone `MoveTo` (draws nothing).
    pub fn to_owned_path(&self) -> Result<OwnedPath, AnchorError> {
        self.validate()?;
        let mut elements = Vec::with_capacity(self.anchor_count().saturating_add(self.contours.len().saturating_mul(2)));
        for c in &self.contours {
            let first = c.anchors.first().ok_or(AnchorError::Degenerate)?;
            elements.push(PathElement::MoveTo(first.position));
            let n = c.segment_count();
            for i in 0..n {
                let a = c.anchors.get(i).ok_or(AnchorError::Degenerate)?;
                let b = c.anchors.get(i.checked_add(1).and_then(|j| j.checked_rem(c.anchors.len())).ok_or(AnchorError::Degenerate)?).ok_or(AnchorError::Degenerate)?;
                let last = i.checked_add(1) == Some(n);
                if is_zero(a.outgoing) && is_zero(b.incoming) {
                    if !(c.closed && last) {
                        elements.push(PathElement::LineTo(b.position));
                    }
                } else {
                    elements.push(PathElement::CubicTo(a.handle_point(HandleSide::Out), b.handle_point(HandleSide::In), b.position));
                }
            }
            if c.closed {
                elements.push(PathElement::Close);
            }
        }
        let path = OwnedPath { elements, fill_rule: self.fill_rule };
        path.validate(&PathLimits::default()).map_err(|_| AnchorError::InvalidGeometry)?;
        Ok(path)
    }

    /// Anchor form of an owned path (validated with nib's default limits first). Quadratics are
    /// raised exactly to cubics; a `Close` whose subpath already returned to its start merges the
    /// end anchor into the start anchor. Mirroring is inferred: two non-zero handles that point in
    /// opposite directions are `ANGLE_AND_LENGTH` when equally long, else `ANGLE`; everything else
    /// is `NONE`.
    pub fn from_owned_path(path: &OwnedPath) -> Result<Self, AnchorError> {
        path.validate(&PathLimits::default()).map_err(|_| AnchorError::InvalidGeometry)?;
        if path.elements.len() > MAX_ANCHORS {
            return Err(AnchorError::BudgetExceeded);
        }
        let mut out = AnchorPath::new(path.fill_rule);
        for e in &path.elements {
            match *e {
                PathElement::MoveTo(p) => {
                    if out.contours.len() >= MAX_CONTOURS {
                        return Err(AnchorError::BudgetExceeded);
                    }
                    out.contours.push(Contour { anchors: vec![EditAnchor::corner(p.x, p.y)], closed: false });
                }
                PathElement::LineTo(p) => last_contour(&mut out)?.anchors.push(EditAnchor::corner(p.x, p.y)),
                PathElement::QuadTo(c, p) => {
                    let contour = last_contour(&mut out)?;
                    let a = contour.anchors.last_mut().ok_or(AnchorError::InvalidGeometry)?;
                    let start = a.position;
                    a.outgoing = clean(two_thirds(sub(c, start)));
                    let mut b = EditAnchor::corner(p.x, p.y);
                    b.incoming = clean(two_thirds(sub(c, p)));
                    contour.anchors.push(b);
                }
                PathElement::CubicTo(c1, c2, p) => {
                    let contour = last_contour(&mut out)?;
                    let a = contour.anchors.last_mut().ok_or(AnchorError::InvalidGeometry)?;
                    a.outgoing = clean(sub(c1, a.position));
                    let mut b = EditAnchor::corner(p.x, p.y);
                    b.incoming = clean(sub(c2, p));
                    contour.anchors.push(b);
                }
                PathElement::Close => {
                    let contour = last_contour(&mut out)?;
                    if contour.anchors.len() > 1
                        && let (Some(first), Some(last)) = (contour.anchors.first().copied(), contour.anchors.last().copied())
                        && first.position == last.position
                    {
                        contour.anchors.pop();
                        if let Some(f) = contour.anchors.first_mut() {
                            f.incoming = last.incoming;
                        }
                    }
                    contour.closed = contour.anchors.len() >= 2;
                }
            }
        }
        for c in &mut out.contours {
            for a in &mut c.anchors {
                a.mirroring = infer_mirroring(a.incoming, a.outgoing);
            }
        }
        out.validate()?;
        Ok(out)
    }
}

fn last_contour(p: &mut AnchorPath) -> Result<&mut Contour, AnchorError> {
    let c = p.contours.last_mut().ok_or(AnchorError::InvalidGeometry)?;
    if c.anchors.len() >= MAX_ANCHORS {
        return Err(AnchorError::BudgetExceeded);
    }
    Ok(c)
}

/// The mirroring two handles exhibit (relative tolerance 1e-9).
pub fn infer_mirroring(incoming: PathPoint, outgoing: PathPoint) -> Mirroring {
    let (li, lo) = (len(incoming), len(outgoing));
    if li == 0.0 || lo == 0.0 {
        return Mirroring::None;
    }
    let cross = incoming.x * outgoing.y - incoming.y * outgoing.x;
    let dot = incoming.x * outgoing.x + incoming.y * outgoing.y;
    let eps = 1.0e-9;
    if dot >= 0.0 || cross.abs() > eps * li * lo {
        return Mirroring::None;
    }
    if (li - lo).abs() <= eps * li.max(lo) { Mirroring::AngleAndLength } else { Mirroring::Angle }
}

fn checked(path: &AnchorPath) -> Result<AnchorPath, AnchorError> {
    path.validate()?;
    Ok(path.clone())
}

fn finish(path: AnchorPath) -> Result<AnchorPath, AnchorError> {
    path.validate()?;
    Ok(path)
}

fn contour_mut(p: &mut AnchorPath, i: usize) -> Result<&mut Contour, AnchorError> {
    p.contours.get_mut(i).ok_or(AnchorError::PathIndexOutOfBounds)
}

fn room_for(path: &AnchorPath, more: usize) -> Result<(), AnchorError> {
    if path.anchor_count().checked_add(more).is_none_or(|n| n > MAX_ANCHORS) {
        return Err(AnchorError::BudgetExceeded);
    }
    Ok(())
}

// ---- pen ------------------------------------------------------------------------------------------

/// Pen: a new open contour holding `anchor` (appended after the existing contours).
pub fn start_contour(path: &AnchorPath, anchor: EditAnchor) -> Result<AnchorPath, AnchorError> {
    path.validate()?;
    check_anchor(&anchor)?;
    room_for(path, 1)?;
    if path.contours.len() >= MAX_CONTOURS {
        return Err(AnchorError::BudgetExceeded);
    }
    let mut out = path.clone();
    out.contours.push(Contour { anchors: vec![anchor], closed: false });
    finish(out)
}

/// Pen: appends `anchor` to the end of open contour `contour`.
pub fn append_anchor(path: &AnchorPath, contour: usize, anchor: EditAnchor) -> Result<AnchorPath, AnchorError> {
    path.validate()?;
    check_anchor(&anchor)?;
    if path.contour(contour)?.closed {
        return Err(AnchorError::ContourClosed);
    }
    room_for(path, 1)?;
    let mut out = path.clone();
    contour_mut(&mut out, contour)?.anchors.push(anchor);
    finish(out)
}

/// Pen: closes open contour `contour` (a click on its first anchor); `incoming` becomes the first
/// anchor's incoming handle (a drag on close), `None` keeps it.
pub fn close_contour(path: &AnchorPath, contour: usize, incoming: Option<PathPoint>) -> Result<AnchorPath, AnchorError> {
    let mut out = checked(path)?;
    let c = contour_mut(&mut out, contour)?;
    if c.closed {
        return Err(AnchorError::ContourClosed);
    }
    if c.anchors.len() < 2 {
        return Err(AnchorError::CannotJoinPathPoints);
    }
    c.closed = true;
    if let (Some(h), Some(first)) = (incoming, c.anchors.first_mut()) {
        first.incoming = clean(h);
        first.mirroring = infer_mirroring(first.incoming, first.outgoing);
    }
    finish(out)
}

// ---- anchor editing ---------------------------------------------------------------------------------

/// Inserts an anchor on segment `segment` at parameter `t` (exclusive `0 < t < 1`) by de Casteljau:
/// the curve is unchanged; the new anchor gets index `segment + 1`. A straight segment gains a
/// corner anchor, a curve an `ANGLE` anchor whose handles are the split control polygon.
pub fn insert_anchor(path: &AnchorPath, contour: usize, segment: usize, t: f64) -> Result<AnchorPath, AnchorError> {
    if !t.is_finite() {
        return Err(AnchorError::NonFinite);
    }
    if t <= 0.0 || t >= 1.0 {
        return Err(AnchorError::InvalidParameter);
    }
    path.validate()?;
    let [p0, p1, p2, p3] = path.segment(contour, segment)?;
    room_for(path, 1)?;
    let mut out = path.clone();
    let c = contour_mut(&mut out, contour)?;
    let n = c.anchors.len();
    let next = segment.checked_add(1).and_then(|j| j.checked_rem(n)).ok_or(AnchorError::PointIndexOutOfBounds)?;
    let straight = p1 == p0 && p2 == p3;
    let new = if straight {
        let m = lerp(p0, p3, t);
        EditAnchor::corner(m.x, m.y)
    } else {
        let (p01, p12, p23) = (lerp(p0, p1, t), lerp(p1, p2, t), lerp(p2, p3, t));
        let (p012, p123) = (lerp(p01, p12, t), lerp(p12, p23, t));
        let m = lerp(p012, p123, t);
        if let Some(a) = c.anchors.get_mut(segment) {
            a.outgoing = clean(sub(p01, p0));
        }
        if let Some(b) = c.anchors.get_mut(next) {
            b.incoming = clean(sub(p23, p3));
        }
        EditAnchor { position: m, incoming: clean(sub(p012, m)), outgoing: clean(sub(p123, m)), mirroring: Mirroring::Angle }
    };
    c.anchors.insert(segment.checked_add(1).ok_or(AnchorError::PointIndexOutOfBounds)?, new);
    finish(out)
}

/// Deletes one anchor (the neighbours keep their handles). A closed contour left with one anchor
/// opens; an emptied contour is removed; a path left without anchors is [`AnchorError::EmptyResult`].
pub fn delete_anchor(path: &AnchorPath, at: AnchorAddr) -> Result<AnchorPath, AnchorError> {
    path.anchor(at)?;
    let mut out = checked(path)?;
    let c = contour_mut(&mut out, at.contour)?;
    c.anchors.remove(at.anchor);
    if c.anchors.len() < 2 {
        c.closed = false;
    }
    if c.anchors.is_empty() {
        out.contours.remove(at.contour);
    }
    if out.contours.is_empty() {
        return Err(AnchorError::EmptyResult);
    }
    finish(out)
}

/// Anchor convert (STU-VEC-109): `NONE` retracts both handles (corner); `ANGLE` /
/// `ANGLE_AND_LENGTH` makes the handles collinear (and equally long for the latter), synthesising
/// handles parallel to the neighbour chord when the anchor has none.
pub fn convert_anchor(path: &AnchorPath, at: AnchorAddr, to: Mirroring) -> Result<AnchorPath, AnchorError> {
    path.anchor(at)?;
    let mut out = checked(path)?;
    let c = contour_mut(&mut out, at.contour)?;
    let n = c.anchors.len();
    let prev = if at.anchor > 0 { Some(at.anchor.saturating_sub(1)) } else if c.closed { Some(n.saturating_sub(1)) } else { None };
    let next = if at.anchor.saturating_add(1) < n { Some(at.anchor.saturating_add(1)) } else if c.closed { Some(0) } else { None };
    let prev_p = prev.and_then(|i| c.anchors.get(i)).map(|a| a.position);
    let next_p = next.and_then(|i| c.anchors.get(i)).map(|a| a.position);
    let a = c.anchors.get_mut(at.anchor).ok_or(AnchorError::PointIndexOutOfBounds)?;
    match to {
        Mirroring::None => {
            a.incoming = ZERO;
            a.outgoing = ZERO;
        }
        Mirroring::Angle | Mirroring::AngleAndLength => {
            let (li, lo) = (len(a.incoming), len(a.outgoing));
            // Direction of the outgoing tangent.
            let dir = if lo > 0.0 && li > 0.0 {
                let d = sub(scale(a.outgoing, 1.0 / lo), scale(a.incoming, 1.0 / li));
                let l = len(d);
                if l > 0.0 { scale(d, 1.0 / l) } else { scale(a.outgoing, 1.0 / lo) }
            } else if lo > 0.0 {
                scale(a.outgoing, 1.0 / lo)
            } else if li > 0.0 {
                neg(scale(a.incoming, 1.0 / li))
            } else {
                let chord = match (prev_p, next_p) {
                    (Some(p), Some(q)) => sub(q, p),
                    (None, Some(q)) => sub(q, a.position),
                    (Some(p), None) => sub(a.position, p),
                    (None, None) => ZERO,
                };
                let l = len(chord);
                if l == 0.0 {
                    return Err(AnchorError::Degenerate);
                }
                scale(chord, 1.0 / l)
            };
            let (mut li, mut lo) = (li, lo);
            if li == 0.0 && lo == 0.0 {
                li = prev_p.map_or(0.0, |p| len(sub(a.position, p)) / 3.0);
                lo = next_p.map_or(0.0, |q| len(sub(q, a.position)) / 3.0);
            }
            if to == Mirroring::AngleAndLength {
                let m = if li > 0.0 && lo > 0.0 { (li + lo) / 2.0 } else { li.max(lo) };
                li = m;
                lo = m;
            }
            a.outgoing = clean(scale(dir, lo));
            a.incoming = clean(scale(dir, -li));
        }
    }
    a.mirroring = to;
    finish(out)
}

/// Moves an anchor (with its handles) by (`dx`, `dy`), constrained per [`Constraint`].
pub fn move_anchor(path: &AnchorPath, at: AnchorAddr, dx: f64, dy: f64, constraint: Constraint) -> Result<AnchorPath, AnchorError> {
    if !(dx.is_finite() && dy.is_finite()) {
        return Err(AnchorError::NonFinite);
    }
    path.anchor(at)?;
    let d = constrain(PathPoint::new(dx, dy), constraint);
    let mut out = checked(path)?;
    let a = contour_mut(&mut out, at.contour)?.anchors.get_mut(at.anchor).ok_or(AnchorError::PointIndexOutOfBounds)?;
    a.position = clean(add(a.position, d));
    finish(out)
}

/// Sets one handle to `to` (a vector relative to the anchor; Shift constrains its direction). The
/// opposite handle follows the anchor's mirroring: `ANGLE_AND_LENGTH` mirrors it exactly, `ANGLE`
/// points it opposite keeping its own length, `NONE` leaves it; `break_mirroring` (Alt) sets the
/// anchor to `NONE` and leaves the opposite handle.
pub fn move_handle(path: &AnchorPath, at: AnchorAddr, side: HandleSide, to: PathPoint, constraint: Constraint, break_mirroring: bool) -> Result<AnchorPath, AnchorError> {
    finite_point(to)?;
    path.anchor(at)?;
    let mut out = checked(path)?;
    let a = contour_mut(&mut out, at.contour)?.anchors.get_mut(at.anchor).ok_or(AnchorError::PointIndexOutOfBounds)?;
    set_handle(a, side, constrain(to, constraint), break_mirroring);
    finish(out)
}

fn set_handle(a: &mut EditAnchor, side: HandleSide, to: PathPoint, break_mirroring: bool) {
    let to = clean(to);
    let other = match side {
        HandleSide::In => a.outgoing,
        HandleSide::Out => a.incoming,
    };
    if break_mirroring {
        a.mirroring = Mirroring::None;
    }
    let other = match a.mirroring {
        Mirroring::None => other,
        Mirroring::AngleAndLength => clean(neg(to)),
        Mirroring::Angle => {
            let (l, lo) = (len(to), len(other));
            if l == 0.0 { other } else { clean(scale(to, -lo / l)) }
        }
    };
    match side {
        HandleSide::In => {
            a.incoming = to;
            a.outgoing = other;
        }
        HandleSide::Out => {
            a.outgoing = to;
            a.incoming = other;
        }
    }
}

/// Drag-to-bend (STU-VEC-003): moves the point at parameter `t` (`0 < t < 1`) of a segment by
/// (`dx`, `dy`) by moving its two governing handles in proportion `(1 - t) : t`, so the dragged
/// point lands exactly on target. A straight segment becomes a curve (handles synthesised); the
/// opposite handles of mirrored end anchors follow their mirroring.
pub fn bend_segment(path: &AnchorPath, contour: usize, segment: usize, t: f64, dx: f64, dy: f64) -> Result<AnchorPath, AnchorError> {
    if !(t.is_finite() && dx.is_finite() && dy.is_finite()) {
        return Err(AnchorError::NonFinite);
    }
    if t <= 0.0 || t >= 1.0 {
        return Err(AnchorError::InvalidParameter);
    }
    path.validate()?;
    let [p0, p1, p2, p3] = path.segment(contour, segment)?;
    let mut out = path.clone();
    let c = contour_mut(&mut out, contour)?;
    let next = segment.checked_add(1).and_then(|j| j.checked_rem(c.anchors.len())).ok_or(AnchorError::PointIndexOutOfBounds)?;
    let u = 1.0 - t;
    // B(t) moves by 3 u t (u a + t b) d with a = u k, b = t k: k = 1 / (3 u t (u^2 + t^2)).
    let k = 1.0 / (3.0 * u * t * (u * u + t * t));
    let d = PathPoint::new(dx, dy);
    let new_out = sub(add(p1, scale(d, u * k)), p0);
    let new_in = sub(add(p2, scale(d, t * k)), p3);
    if let Some(a) = c.anchors.get_mut(segment) {
        set_handle(a, HandleSide::Out, new_out, false);
    }
    if let Some(b) = c.anchors.get_mut(next) {
        set_handle(b, HandleSide::In, new_in, false);
    }
    finish(out)
}

fn end_point(c: &Contour, e: End) -> Option<&EditAnchor> {
    match e {
        End::Start => c.anchors.first(),
        End::End => c.anchors.last(),
    }
}

fn reversed(c: &Contour) -> Contour {
    let swap = |a: &EditAnchor| EditAnchor { incoming: a.outgoing, outgoing: a.incoming, ..*a };
    let mut anchors: Vec<EditAnchor> = Vec::with_capacity(c.anchors.len());
    if c.closed {
        // Keep the start anchor: [a0, a(n-1), ..., a1].
        anchors.extend(c.anchors.first().map(swap));
        anchors.extend(c.anchors.iter().skip(1).rev().map(swap));
    } else {
        anchors.extend(c.anchors.iter().rev().map(swap));
    }
    Contour { anchors, closed: c.closed }
}

/// Reverses the direction of one contour (`Some`) or of every contour (`None`) (STU-VEC-043): anchor
/// order reversed and in/out handles swapped; a closed contour keeps its start anchor.
pub fn reverse(path: &AnchorPath, contour: Option<usize>) -> Result<AnchorPath, AnchorError> {
    if let Some(i) = contour {
        path.contour(i)?;
    }
    path.validate()?;
    let contours = path
        .contours
        .iter()
        .enumerate()
        .map(|(i, c)| if contour.is_none_or(|k| k == i) { reversed(c) } else { c.clone() })
        .collect();
    finish(AnchorPath { contours, fill_rule: path.fill_rule })
}

/// Join (STU-VEC-006/007): two ends of open contours. The two ends of one contour close it; ends of
/// two contours concatenate them into the first contour's slot (the second is removed). Ends closer
/// than `merge_distance` merge into one `NONE` anchor (incoming of the first, outgoing of the
/// second); otherwise a straight segment connects them.
pub fn join(path: &AnchorPath, a: EndAddr, b: EndAddr, merge_distance: f64) -> Result<AnchorPath, AnchorError> {
    if !merge_distance.is_finite() || merge_distance < 0.0 {
        return Err(AnchorError::InvalidParameter);
    }
    let (ca, cb) = (path.contour(a.contour)?, path.contour(b.contour)?);
    path.validate()?;
    if ca.closed || cb.closed || a == b {
        return Err(AnchorError::CannotJoinPathPoints);
    }
    let (pa, pb) = (end_point(ca, a.end).ok_or(AnchorError::Degenerate)?, end_point(cb, b.end).ok_or(AnchorError::Degenerate)?);
    let coincident = len(sub(pa.position, pb.position)) <= merge_distance;
    if a.contour == b.contour {
        if ca.anchors.len() < 2 {
            return Err(AnchorError::CannotJoinPathPoints);
        }
        let mut out = path.clone();
        let c = contour_mut(&mut out, a.contour)?;
        if coincident && c.anchors.len() > 2 {
            let last = c.anchors.pop().ok_or(AnchorError::Degenerate)?;
            if let Some(f) = c.anchors.first_mut() {
                f.incoming = last.incoming;
                f.mirroring = Mirroring::None;
            }
        }
        c.closed = true;
        return finish(out);
    }
    // Orient: the first contour ends at `a`, the second starts at `b`.
    let first = if a.end == End::Start { reversed(ca) } else { ca.clone() };
    let second = if b.end == End::End { reversed(cb) } else { cb.clone() };
    let mut anchors = first.anchors;
    let mut rest = second.anchors.into_iter();
    if coincident
        && let (Some(last), Some(head)) = (anchors.last_mut(), rest.next())
    {
        last.outgoing = head.outgoing;
        last.mirroring = Mirroring::None;
    } else if coincident {
        return Err(AnchorError::Degenerate);
    }
    anchors.extend(rest);
    let mut out = path.clone();
    *contour_mut(&mut out, a.contour)? = Contour { anchors, closed: false };
    out.contours.remove(b.contour);
    finish(out)
}

/// Scissors at an anchor (STU-VEC-048): an interior anchor of an open contour splits it into two open
/// contours whose ends coincide; any anchor of a closed contour opens it there (the anchor becomes
/// both ends). Splitting an open contour at an end is [`AnchorError::InvalidParameter`].
pub fn split_at_anchor(path: &AnchorPath, at: AnchorAddr) -> Result<AnchorPath, AnchorError> {
    path.anchor(at)?;
    path.validate()?;
    room_for(path, 1)?;
    let c = path.contour(at.contour)?;
    let n = c.anchors.len();
    let k = at.anchor;
    let pivot = *c.anchors.get(k).ok_or(AnchorError::PointIndexOutOfBounds)?;
    let end_copy = EditAnchor { outgoing: ZERO, mirroring: Mirroring::None, ..pivot };
    let start_copy = EditAnchor { incoming: ZERO, mirroring: Mirroring::None, ..pivot };
    let mut out = path.clone();
    if c.closed {
        let mut anchors = Vec::with_capacity(n.saturating_add(1));
        anchors.push(start_copy);
        anchors.extend(c.anchors.iter().skip(k.saturating_add(1)));
        anchors.extend(c.anchors.iter().take(k));
        anchors.push(end_copy);
        *contour_mut(&mut out, at.contour)? = Contour { anchors, closed: false };
        return finish(out);
    }
    if k == 0 || k.saturating_add(1) >= n {
        return Err(AnchorError::InvalidParameter);
    }
    if out.contours.len() >= MAX_CONTOURS {
        return Err(AnchorError::BudgetExceeded);
    }
    let mut head: Vec<EditAnchor> = c.anchors.iter().take(k).copied().collect();
    head.push(end_copy);
    let mut tail = vec![start_copy];
    tail.extend(c.anchors.iter().skip(k.saturating_add(1)));
    *contour_mut(&mut out, at.contour)? = Contour { anchors: head, closed: false };
    out.contours.insert(at.contour.saturating_add(1), Contour { anchors: tail, closed: false });
    finish(out)
}

/// Scissors at a parametric point (STU-VEC-048): [`insert_anchor`] then [`split_at_anchor`].
pub fn split_at(path: &AnchorPath, contour: usize, segment: usize, t: f64) -> Result<AnchorPath, AnchorError> {
    let inserted = insert_anchor(path, contour, segment, t)?;
    split_at_anchor(&inserted, AnchorAddr { contour, anchor: segment.saturating_add(1) })
}

// ---- hit testing (node-select tool) -------------------------------------------------------------------

/// The anchor part nearest to `p` within `radius` (handles before anchors on ties, so a retracted
/// handle never hides its anchor: zero-length handles are not hit targets).
pub fn hit_anchor(path: &AnchorPath, p: PathPoint, radius: f64) -> Option<AnchorHit> {
    let mut best: Option<(f64, AnchorHit)> = None;
    for (ci, c) in path.contours.iter().enumerate() {
        for (ai, a) in c.anchors.iter().enumerate() {
            let at = AnchorAddr { contour: ci, anchor: ai };
            let mut consider = |q: PathPoint, part: AnchorSelection| {
                let d = len(sub(q, p));
                if d <= radius && best.as_ref().is_none_or(|(bd, _)| d < *bd) {
                    best = Some((d, AnchorHit { at, part }));
                }
            };
            consider(a.position, AnchorSelection::Anchor);
            if !is_zero(a.incoming) {
                consider(a.handle_point(HandleSide::In), AnchorSelection::InHandle);
            }
            if !is_zero(a.outgoing) {
                consider(a.handle_point(HandleSide::Out), AnchorSelection::OutHandle);
            }
        }
    }
    best.map(|(_, h)| h)
}

fn bezier(s: &[PathPoint; 4], t: f64) -> PathPoint {
    let u = 1.0 - t;
    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    PathPoint::new(a * s[0].x + b * s[1].x + c * s[2].x + d * s[3].x, a * s[0].y + b * s[1].y + c * s[2].y + d * s[3].y)
}

/// The segment point nearest to `p` within `tolerance`: 64 uniform samples per segment, refined by
/// ternary search on the best interval; `t` is strictly inside `0..1`.
pub fn hit_segment(path: &AnchorPath, p: PathPoint, tolerance: f64) -> Option<SegmentHit> {
    const SAMPLES: u32 = 64;
    let mut best: Option<SegmentHit> = None;
    for (ci, c) in path.contours.iter().enumerate() {
        for si in 0..c.segment_count() {
            let Ok(s) = path.segment(ci, si) else { continue };
            let dist = |t: f64| len(sub(bezier(&s, t), p));
            let mut bt = 0.0;
            let mut bd = f64::INFINITY;
            for k in 0..=SAMPLES {
                let t = f64::from(k) / f64::from(SAMPLES);
                let d = dist(t);
                if d < bd {
                    bd = d;
                    bt = t;
                }
            }
            let step = 1.0 / f64::from(SAMPLES);
            let (mut lo, mut hi) = ((bt - step).max(0.0), (bt + step).min(1.0));
            for _ in 0..40 {
                let (m1, m2) = (lo + (hi - lo) / 3.0, hi - (hi - lo) / 3.0);
                if dist(m1) < dist(m2) { hi = m2 } else { lo = m1 }
            }
            let t = ((lo + hi) / 2.0).clamp(1.0e-6, 1.0 - 1.0e-6);
            let d = dist(t);
            if d <= tolerance && best.as_ref().is_none_or(|b| d < b.distance) {
                best = Some(SegmentHit { contour: ci, segment: si, t, distance: d });
            }
        }
    }
    best
}

/// Point of segment `segment` of `contour` at parameter `t`.
pub fn point_at(path: &AnchorPath, contour: usize, segment: usize, t: f64) -> Result<PathPoint, AnchorError> {
    if !t.is_finite() {
        return Err(AnchorError::NonFinite);
    }
    Ok(bezier(&path.segment(contour, segment)?, t.clamp(0.0, 1.0)))
}
