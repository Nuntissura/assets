//! Path editing primitives as pure functions (lane SMB-NIB-VECTOR-RENDER slice 4; STU-VEC-006 offset
//! path and join/average, STU-VEC-009 anchor conversion, STU-VEC-048 scissors).
//!
//! Every function takes an [`OwnedPath`], validates it, returns a new validated path and never
//! mutates its input. Anchors are addressed per subpath ([`AnchorRef`]): in an open subpath anchor 0
//! is the start and anchor `k` the end of segment `k - 1`; in a closed subpath the implicit closing
//! line is an ordinary segment while editing (anchor 0 is the start, there are as many anchors as
//! segments) and is written back as `Close` when it is straight.
//!
//! Rules that are Studio choices, not verified Adobe behaviour (UNVERIFIED, D5): deleting an anchor
//! between two curves keeps the outer handles (no shape-preserving refit); a converted smooth anchor
//! gets handles parallel to the chord between its neighbours with one third of each neighbour
//! distance; joining coincident ends merges them, other ends are connected by a straight segment.
#![deny(clippy::arithmetic_side_effects)]
use crate::boolean::{BooleanOptions, GeometryResult, normalise_path};
use crate::flatten::norm;
use crate::owned_path::{COORDINATE_LIMIT, FillRule, OwnedPath, PathElement, PathLimits, PathPoint};
use crate::path_contract::Error;
use crate::stroke::{LineCap, LineJoin, StrokeAlign, StrokeOptions, StrokeStyle, outline_stroke};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnchorRef {
    pub subpath: usize,
    pub anchor: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnchorKind {
    Corner,
    Smooth,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    Start,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EndRef {
    pub subpath: usize,
    pub end: End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
    Both,
}

type P = PathPoint;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Seg {
    Line(P),
    Quad(P, P),
    Cubic(P, P, P),
}
impl Seg {
    fn end(self) -> P {
        match self {
            Self::Line(e) | Self::Quad(_, e) | Self::Cubic(_, _, e) => e,
        }
    }
    /// Exact cubic form starting at `a`.
    fn cubic(self, a: P) -> (P, P, P) {
        match self {
            Self::Line(b) => (lerp(a, b, 1.0 / 3.0), lerp(a, b, 2.0 / 3.0), b),
            Self::Quad(c, b) => (lerp(a, c, 2.0 / 3.0), lerp(b, c, 2.0 / 3.0), b),
            Self::Cubic(c1, c2, b) => (c1, c2, b),
        }
    }
    /// Editing handles starting at `a`: a line's handles are retracted onto its anchors; quads are
    /// raised exactly to cubics.
    fn handles(self, a: P) -> (P, P, P) {
        match self {
            Self::Line(b) => (a, b, b),
            _ => self.cubic(a),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Sub {
    start: P,
    segs: Vec<Seg>,
    closed: bool,
}
impl Sub {
    fn anchors(&self) -> usize {
        if self.closed {
            self.segs.len()
        } else {
            self.segs.len().saturating_add(1)
        }
    }
    fn anchor(&self, k: usize) -> Option<P> {
        if k == 0 {
            Some(self.start)
        } else {
            self.segs.get(k.checked_sub(1)?).map(|s| s.end())
        }
    }
    /// Start point of segment `i`.
    fn seg_start(&self, i: usize) -> Option<P> {
        self.anchor(i)
    }
    fn reversed(&self) -> Sub {
        let mut segs = Vec::with_capacity(self.segs.len());
        let mut points: Vec<P> = vec![self.start];
        points.extend(self.segs.iter().map(|s| s.end()));
        for (i, s) in self.segs.iter().enumerate().rev() {
            let a = points[i];
            segs.push(match *s {
                Seg::Line(_) => Seg::Line(a),
                Seg::Quad(c, _) => Seg::Quad(c, a),
                Seg::Cubic(c1, c2, _) => Seg::Cubic(c2, c1, a),
            });
        }
        Sub {
            start: points.last().copied().unwrap_or(self.start),
            segs,
            closed: self.closed,
        }
    }
}

fn lerp(a: P, b: P, t: f64) -> P {
    P::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

fn parse(path: &OwnedPath) -> Result<Vec<Sub>, Error> {
    path.validate(&PathLimits::default())?;
    let mut subs: Vec<Sub> = Vec::new();
    for e in &path.elements {
        match *e {
            PathElement::MoveTo(p) => subs.push(Sub {
                start: p,
                segs: Vec::new(),
                closed: false,
            }),
            PathElement::LineTo(p) => current(&mut subs)?.segs.push(Seg::Line(p)),
            PathElement::QuadTo(c, p) => current(&mut subs)?.segs.push(Seg::Quad(c, p)),
            PathElement::CubicTo(a, b, p) => current(&mut subs)?.segs.push(Seg::Cubic(a, b, p)),
            PathElement::Close => {
                let sub = current(&mut subs)?;
                let end = sub.segs.last().map(|s| s.end());
                if end != Some(sub.start) {
                    sub.segs.push(Seg::Line(sub.start));
                }
                sub.closed = true;
            }
        }
    }
    Ok(subs)
}

fn current(subs: &mut [Sub]) -> Result<&mut Sub, Error> {
    subs.last_mut().ok_or(Error::InvalidGeometry)
}

fn assemble(subs: &[Sub], fill_rule: FillRule) -> Result<OwnedPath, Error> {
    let mut path = OwnedPath::new(fill_rule);
    for sub in subs {
        path.elements.push(PathElement::MoveTo(sub.start));
        let mut segs: &[Seg] = &sub.segs;
        if sub.closed
            && let Some((Seg::Line(e), rest)) = segs.split_last()
            && *e == sub.start
            && !rest.is_empty()
        {
            segs = rest;
        }
        for s in segs {
            path.elements.push(match *s {
                Seg::Line(e) => PathElement::LineTo(e),
                Seg::Quad(c, e) => PathElement::QuadTo(c, e),
                Seg::Cubic(a, b, e) => PathElement::CubicTo(a, b, e),
            });
        }
        if sub.closed {
            path.elements.push(PathElement::Close);
        }
    }
    path.validate(&PathLimits::default())?;
    Ok(path)
}

fn sub_mut(subs: &mut [Sub], i: usize) -> Result<&mut Sub, Error> {
    subs.get_mut(i).ok_or(Error::InvalidRequest)
}

/// Splits segment `segment` of `subpath` at parameter `t` (exclusive `0..1`) by de Casteljau; the
/// curve is unchanged and gains one anchor (index `segment + 1`).
pub fn insert_anchor(path: &OwnedPath, subpath: usize, segment: usize, t: f64) -> Result<OwnedPath, Error> {
    if !t.is_finite() || t <= 0.0 || t >= 1.0 {
        return Err(Error::InvalidRequest);
    }
    let mut subs = parse(path)?;
    let sub = sub_mut(&mut subs, subpath)?;
    let a = sub.seg_start(segment).ok_or(Error::InvalidRequest)?;
    let seg = *sub.segs.get(segment).ok_or(Error::InvalidRequest)?;
    let (left, right) = match seg {
        Seg::Line(b) => {
            let m = lerp(a, b, t);
            (Seg::Line(m), Seg::Line(b))
        }
        Seg::Quad(c, b) => {
            let (ac, cb) = (lerp(a, c, t), lerp(c, b, t));
            let m = lerp(ac, cb, t);
            (Seg::Quad(ac, m), Seg::Quad(cb, b))
        }
        Seg::Cubic(c1, c2, b) => {
            let (p01, p12, p23) = (lerp(a, c1, t), lerp(c1, c2, t), lerp(c2, b, t));
            let (p012, p123) = (lerp(p01, p12, t), lerp(p12, p23, t));
            let m = lerp(p012, p123, t);
            (Seg::Cubic(p01, p012, m), Seg::Cubic(p123, p23, b))
        }
    };
    sub.segs[segment] = left;
    sub.segs.insert(segment.saturating_add(1), right);
    assemble(&subs, path.fill_rule)
}

/// Removes one anchor. An end anchor of an open subpath drops its segment; an interior anchor merges
/// its two segments (two lines stay a line, otherwise a cubic keeping the outer handles). At least
/// two anchors must remain.
pub fn delete_anchor(path: &OwnedPath, at: AnchorRef) -> Result<OwnedPath, Error> {
    let mut subs = parse(path)?;
    let sub = sub_mut(&mut subs, at.subpath)?;
    let n = sub.anchors();
    if at.anchor >= n {
        return Err(Error::InvalidRequest);
    }
    if n <= 2 {
        return Err(Error::InvalidGeometry);
    }
    if sub.closed && at.anchor == 0 {
        // Rotate so the anchor is interior: start at the previous anchor.
        let last = sub.segs.pop().ok_or(Error::InvalidGeometry)?;
        let new_start = sub.anchor(sub.segs.len()).ok_or(Error::InvalidGeometry)?;
        sub.segs.insert(0, last);
        sub.start = new_start;
        merge(sub, 1)?;
    } else if !sub.closed && at.anchor == 0 {
        let first = sub.segs.remove(0);
        sub.start = first.end();
    } else if !sub.closed && at.anchor.checked_add(1) == Some(n) {
        sub.segs.pop();
    } else {
        merge(sub, at.anchor)?;
    }
    assemble(&subs, path.fill_rule)
}

/// Merges segments `k - 1` and `k` (anchor `k` disappears).
fn merge(sub: &mut Sub, k: usize) -> Result<(), Error> {
    let i = k.checked_sub(1).ok_or(Error::InvalidRequest)?;
    let a = sub.seg_start(i).ok_or(Error::InvalidRequest)?;
    let mid = sub.anchor(k).ok_or(Error::InvalidRequest)?;
    let (s0, s1) = (
        *sub.segs.get(i).ok_or(Error::InvalidRequest)?,
        *sub.segs.get(k).ok_or(Error::InvalidRequest)?,
    );
    let merged = match (s0, s1) {
        (Seg::Line(_), Seg::Line(e)) => Seg::Line(e),
        _ => {
            let (c1, _, _) = s0.handles(a);
            let (_, c2, e) = s1.handles(mid);
            Seg::Cubic(c1, c2, e)
        }
    };
    sub.segs[i] = merged;
    sub.segs.remove(k);
    Ok(())
}

/// Segment indices entering and leaving anchor `k` (None at open ends).
fn around(sub: &Sub, k: usize) -> (Option<usize>, Option<usize>) {
    let n = sub.segs.len();
    if sub.closed {
        let incoming = if k == 0 { n.checked_sub(1) } else { k.checked_sub(1) };
        (incoming, (k < n).then_some(k))
    } else {
        (k.checked_sub(1), (k < n).then_some(k))
    }
}

/// Converts an anchor to a corner (handles retracted onto the anchor; a segment whose two handles
/// then sit on its end points becomes a line) or to a smooth anchor (collinear handles parallel to
/// the chord between the neighbouring anchors, each one third of the distance to that neighbour).
pub fn convert_anchor(path: &OwnedPath, at: AnchorRef, kind: AnchorKind) -> Result<OwnedPath, Error> {
    let mut subs = parse(path)?;
    let sub = sub_mut(&mut subs, at.subpath)?;
    if at.anchor >= sub.anchors() {
        return Err(Error::InvalidRequest);
    }
    let v = sub.anchor(at.anchor).ok_or(Error::InvalidRequest)?;
    let (incoming, outgoing) = around(sub, at.anchor);
    let prev = incoming.and_then(|i| sub.seg_start(i));
    let next = outgoing.and_then(|o| sub.segs.get(o)).map(|s| s.end());
    let (in_handle, out_handle) = match kind {
        AnchorKind::Corner => (v, v),
        AnchorKind::Smooth => {
            let (from, to) = (prev.unwrap_or(v), next.unwrap_or(v));
            let (dx, dy) = (to.x - from.x, to.y - from.y);
            let len = norm(dx, dy);
            if len <= 0.0 || !len.is_finite() {
                return Err(Error::InvalidGeometry);
            }
            let (ux, uy) = (dx / len, dy / len);
            let back = prev.map_or(0.0, |p| norm(v.x - p.x, v.y - p.y) / 3.0);
            let ahead = next.map_or(0.0, |q| norm(q.x - v.x, q.y - v.y) / 3.0);
            (
                P::new(v.x - ux * back, v.y - uy * back),
                P::new(v.x + ux * ahead, v.y + uy * ahead),
            )
        }
    };
    if let (Some(i), Some(a)) = (incoming, prev) {
        let (c1, _, e) = sub.segs[i].handles(a);
        sub.segs[i] = simplify(a, c1, in_handle, e);
    }
    if let Some(o) = outgoing {
        let (_, c2, e) = sub.segs[o].handles(v);
        sub.segs[o] = simplify(v, out_handle, c2, e);
    }
    assemble(&subs, path.fill_rule)
}

fn simplify(a: P, c1: P, c2: P, e: P) -> Seg {
    if c1 == a && c2 == e {
        Seg::Line(e)
    } else {
        Seg::Cubic(c1, c2, e)
    }
}

/// Scissors at an anchor: an open subpath splits into two open subpaths sharing the anchor position;
/// a closed subpath opens there (starts and ends at the anchor). Open end anchors cannot be split.
pub fn split_at_anchor(path: &OwnedPath, at: AnchorRef) -> Result<OwnedPath, Error> {
    let mut subs = parse(path)?;
    let sub = sub_mut(&mut subs, at.subpath)?.clone();
    if at.anchor >= sub.anchors() {
        return Err(Error::InvalidRequest);
    }
    let k = at.anchor;
    let replacement = if sub.closed {
        let start = sub.anchor(k).ok_or(Error::InvalidRequest)?;
        let mut segs = sub.segs[k..].to_vec();
        segs.extend_from_slice(&sub.segs[..k]);
        vec![Sub {
            start,
            segs,
            closed: false,
        }]
    } else {
        if k == 0 || k == sub.segs.len() {
            return Err(Error::InvalidRequest);
        }
        let mid = sub.anchor(k).ok_or(Error::InvalidRequest)?;
        vec![
            Sub {
                start: sub.start,
                segs: sub.segs[..k].to_vec(),
                closed: false,
            },
            Sub {
                start: mid,
                segs: sub.segs[k..].to_vec(),
                closed: false,
            },
        ]
    };
    subs.splice(at.subpath..=at.subpath, replacement);
    assemble(&subs, path.fill_rule)
}

/// Scissors at a parametric point of a segment (STU-VEC-048): insert, then split there.
pub fn split_at(path: &OwnedPath, subpath: usize, segment: usize, t: f64) -> Result<OwnedPath, Error> {
    let inserted = insert_anchor(path, subpath, segment, t)?;
    split_at_anchor(
        &inserted,
        AnchorRef {
            subpath,
            anchor: segment.saturating_add(1),
        },
    )
}

/// Joins two open ends. The same subpath's start and end close it. Two subpaths are oriented so `a`
/// is the end of the first and `b` the start of the second; ends within `merge_distance` merge
/// into one anchor at `a`'s position, others are connected by a straight segment. The joined
/// subpath takes `a`'s place.
pub fn join(path: &OwnedPath, a: EndRef, b: EndRef, merge_distance: f64) -> Result<OwnedPath, Error> {
    if !merge_distance.is_finite() || merge_distance < 0.0 {
        return Err(Error::InvalidRequest);
    }
    let mut subs = parse(path)?;
    let sa = subs.get(a.subpath).ok_or(Error::InvalidRequest)?.clone();
    let sb = subs.get(b.subpath).ok_or(Error::InvalidRequest)?.clone();
    if sa.closed || sb.closed || (sa.segs.is_empty() && a.subpath == b.subpath) {
        return Err(Error::InvalidRequest);
    }
    let close_enough = |p: P, q: P| norm(p.x - q.x, p.y - q.y) <= merge_distance;
    if a.subpath == b.subpath {
        if a.end == b.end {
            return Err(Error::InvalidRequest);
        }
        let sub = sub_mut(&mut subs, a.subpath)?;
        let end = sub.segs.last().map(|s| s.end()).ok_or(Error::InvalidGeometry)?;
        if close_enough(end, sub.start) && sub.segs.len() > 1 {
            // Merge the end anchor into the start: the last segment now ends at the start.
            if let Some(last) = sub.segs.last_mut() {
                *last = match *last {
                    Seg::Line(_) => Seg::Line(sub.start),
                    Seg::Quad(c, _) => Seg::Quad(c, sub.start),
                    Seg::Cubic(c1, c2, _) => Seg::Cubic(c1, c2, sub.start),
                };
            }
        } else {
            sub.segs.push(Seg::Line(sub.start));
        }
        sub.closed = true;
        return assemble(&subs, path.fill_rule);
    }
    let first = if a.end == End::Start { sa.reversed() } else { sa };
    let second = if b.end == End::End { sb.reversed() } else { sb };
    let mut joined = first;
    let end = joined.segs.last().map_or(joined.start, |s| s.end());
    if !close_enough(end, second.start) {
        joined.segs.push(Seg::Line(second.start));
        joined.segs.extend(second.segs);
    } else {
        // Merge: the second's first segment starts at the first's end point.
        joined.segs.extend(second.segs);
    }
    subs[a.subpath] = joined;
    subs.remove(b.subpath);
    assemble(&subs, path.fill_rule)
}

/// Moves the given anchors to their average position on `axis`; each anchor's handles move with it.
pub fn average_anchors(path: &OwnedPath, anchors: &[AnchorRef], axis: Axis) -> Result<OwnedPath, Error> {
    if anchors.is_empty() {
        return Err(Error::InvalidRequest);
    }
    let mut subs = parse(path)?;
    let (mut sx, mut sy) = (0.0, 0.0);
    for r in anchors {
        let p = subs
            .get(r.subpath)
            .filter(|s| r.anchor < s.anchors())
            .and_then(|s| s.anchor(r.anchor))
            .ok_or(Error::InvalidRequest)?;
        sx += p.x;
        sy += p.y;
    }
    let count = anchors.len() as f64;
    let (mx, my) = (sx / count, sy / count);
    for r in anchors {
        let sub = sub_mut(&mut subs, r.subpath)?;
        let p = sub.anchor(r.anchor).ok_or(Error::InvalidRequest)?;
        let target = match axis {
            Axis::Horizontal => P::new(p.x, my),
            Axis::Vertical => P::new(mx, p.y),
            Axis::Both => P::new(mx, my),
        };
        move_anchor(sub, r.anchor, target);
    }
    assemble(&subs, path.fill_rule)
}

fn move_anchor(sub: &mut Sub, k: usize, to: P) {
    let Some(from) = sub.anchor(k) else {
        return;
    };
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let shift = |p: P| P::new(p.x + dx, p.y + dy);
    let (incoming, outgoing) = around(sub, k);
    if let Some(i) = incoming {
        sub.segs[i] = match sub.segs[i] {
            Seg::Line(_) => Seg::Line(to),
            Seg::Quad(c, _) => Seg::Quad(c, to),
            Seg::Cubic(c1, c2, _) => Seg::Cubic(c1, shift(c2), to),
        };
    }
    if let Some(o) = outgoing
        && let Seg::Cubic(c1, c2, e) = sub.segs[o]
    {
        sub.segs[o] = Seg::Cubic(shift(c1), c2, e);
    }
    if k == 0 {
        sub.start = to;
    }
}

/// Offset path (STU-VEC-006, STU-VEC-113): the filled region (own fill rule, open subpaths closed)
/// grown by `offset` (`> 0`, outset) or shrunk (`< 0`, inset) with the given join at corners, as
/// polygons. Built as region union / minus the stroke of width `2 |offset|` along the region's
/// resolved boundary.
pub fn offset_path(
    path: &OwnedPath,
    offset: f64,
    join: LineJoin,
    miter_limit: f64,
    options: &BooleanOptions,
) -> Result<GeometryResult, Error> {
    if !offset.is_finite() || offset.abs() > COORDINATE_LIMIT {
        return Err(Error::UnsupportedOptions);
    }
    let region = normalise_path(path, options)?;
    if offset == 0.0 {
        return Ok(region);
    }
    let mut style = StrokeStyle::new(offset.abs() * 2.0);
    style.join = join;
    style.cap = LineCap::Butt;
    style.miter_limit = miter_limit;
    style.align = if offset > 0.0 {
        StrokeAlign::Outside
    } else {
        StrokeAlign::Inside
    };
    // Align halves the doubled width again: Outside/Inside keep only the band on one side.
    style.width = offset.abs();
    let stroke_options = StrokeOptions {
        geometry: *options,
        ..StrokeOptions::default()
    };
    let band = outline_stroke(&region.path, &style, &stroke_options)?;
    let operands = [region.path, band.path];
    let op = if offset > 0.0 {
        crate::boolean::BooleanOp::Union
    } else {
        crate::boolean::BooleanOp::SubtractFront
    };
    crate::boolean::boolean(&operands, op, options)
}
