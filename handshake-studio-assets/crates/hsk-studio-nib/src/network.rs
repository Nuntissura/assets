//! `StudioVectorNetwork` (lane SMB-ILLUSTRATOR-PEN-2 slice 5; STU-VEC-001, STU-VEC-004, STU-VEC-108,
//! STU-VEC-110): a graph of vertices joined by cubic segments where a vertex may join any number of
//! segments, plus regions (closed loops of segment indices) with their own winding rule and fill
//! state, so one network holds filled and unfilled regions at once.
//!
//! Contract (STU-VEC-108): three arrays, `vertices[]` (`x`, `y`, optional `stroke_cap`,
//! `stroke_join`, `corner_radius`, `handle_mirroring`; absent = inherit from the object),
//! `segments[]` (`start`, `end` vertex indices, optional `tangent_start` / `tangent_end` vectors
//! relative to their endpoint; both absent or zero = a straight line) and `regions[]`
//! (`winding_rule` NONZERO or EVENODD — a region never carries NONE — `loops[][]` of segment
//! indices, the REQUIRED fill state `fills`, optional `fill_style_id`). An out-of-range index is a
//! validation error, never clamped ([`NetworkError::IndexOutOfBounds`]). Replacing a network is
//! atomic: the caller builds a new value and [`VectorNetwork::validate`]s it whole.
//!
//! Operations: lossless promotion of an anchor path ([`VectorNetwork::from_path`], STU-VEC-001:
//! every anchor a vertex with its mirroring, every segment with its tangents, every closed contour a
//! filled region with the path's winding); the explicit, lossy flatten back to paths
//! ([`VectorNetwork::fill_geometry`] per filled region, [`VectorNetwork::stroke_geometry`] as maximal
//! chains through degree-2 vertices; both the derived read-only path sets of STU-VEC-108); the
//! paint-bucket region operation ([`VectorNetwork::paint_bucket`], STU-VEC-004, topology untouched);
//! and face detection ([`VectorNetwork::detect_regions`]): the bounded faces of the planar
//! embedding, found by walking half-edges in tangent order around every vertex.
//!
//! Limits and Studio choices: segments are assumed not to cross except at shared vertices (face
//! detection does not split crossing segments; UNVERIFIED against Figma's network semantics, D5);
//! tangent order uses an exact pseudo-angle (no trigonometry); face area is measured on 16 samples
//! per segment. Budgets are checked before anything is walked or allocated.
#![deny(clippy::arithmetic_side_effects)]
use crate::anchor_path::{AnchorPath, Contour, EditAnchor};
use crate::owned_path::{COORDINATE_LIMIT, FillRule, PathPoint};
use crate::path_contract::Mirroring;
use crate::stroke::{LineCap, LineJoin};

/// Most vertices or segments of one network.
pub const MAX_NETWORK_ELEMENTS: usize = 65_536;
/// Most regions of one network.
pub const MAX_REGIONS: usize = 4096;
/// Most segment references over all region loops.
pub const MAX_LOOP_REFERENCES: usize = 262_144;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkError {
    /// A vertex, segment or region index outside its array (STU-VEC-108: never clamped).
    IndexOutOfBounds,
    NonFinite,
    OutOfRange,
    BudgetExceeded,
    /// A segment from a vertex to itself without tangents, or a loop that does not close.
    Degenerate,
    /// A region's loop whose consecutive segments do not share a vertex.
    OpenLoop,
    /// A region with no loop, or a style id outside the key grammar.
    InvalidRegion,
}

impl NetworkError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::IndexOutOfBounds => "index_out_of_bounds",
            Self::NonFinite => "non_finite",
            Self::OutOfRange => "out_of_range",
            Self::BudgetExceeded => "budget_exceeded",
            Self::Degenerate => "degenerate",
            Self::OpenLoop => "open_loop",
            Self::InvalidRegion => "invalid_region",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NetworkVertex {
    pub x: f64,
    pub y: f64,
    pub stroke_cap: Option<LineCap>,
    pub stroke_join: Option<LineJoin>,
    pub corner_radius: Option<f64>,
    pub handle_mirroring: Option<Mirroring>,
}

impl NetworkVertex {
    pub const fn at(x: f64, y: f64) -> Self {
        Self { x, y, stroke_cap: None, stroke_join: None, corner_radius: None, handle_mirroring: None }
    }
    fn point(&self) -> PathPoint {
        PathPoint::new(self.x, self.y)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NetworkSegment {
    pub start: usize,
    pub end: usize,
    pub tangent_start: Option<PathPoint>,
    pub tangent_end: Option<PathPoint>,
}

impl NetworkSegment {
    pub const fn line(start: usize, end: usize) -> Self {
        Self { start, end, tangent_start: None, tangent_end: None }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NetworkRegion {
    /// NONZERO or EVENODD (a region never carries NONE, STU-VEC-110).
    pub winding_rule: FillRule,
    pub loops: Vec<Vec<usize>>,
    /// Per-region fill state (STU-VEC-004 / STU-VEC-108: required).
    pub fills: bool,
    pub fill_style_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct VectorNetwork {
    pub vertices: Vec<NetworkVertex>,
    pub segments: Vec<NetworkSegment>,
    pub regions: Vec<NetworkRegion>,
}

fn finite(p: PathPoint) -> Result<(), NetworkError> {
    if !(p.x.is_finite() && p.y.is_finite()) {
        return Err(NetworkError::NonFinite);
    }
    if p.x.abs() > COORDINATE_LIMIT || p.y.abs() > COORDINATE_LIMIT {
        return Err(NetworkError::OutOfRange);
    }
    Ok(())
}

fn zero(t: Option<PathPoint>) -> bool {
    t.is_none_or(|t| t.x == 0.0 && t.y == 0.0)
}

/// Exact monotone pseudo-angle of a direction in `[0, 4)` (counter-clockwise with y up).
fn pseudo_angle(dx: f64, dy: f64) -> f64 {
    let p = dx / (dx.abs() + dy.abs());
    if dy >= 0.0 { 1.0 - p } else { 3.0 + p }
}

impl VectorNetwork {
    /// Budgets first, then values and indices (STU-VEC-108: out-of-range is an error), segments
    /// non-degenerate, every region loop closed (consecutive segments share a vertex, the last
    /// returns to the first), style ids in the key grammar.
    pub fn validate(&self) -> Result<(), NetworkError> {
        if self.vertices.len() > MAX_NETWORK_ELEMENTS || self.segments.len() > MAX_NETWORK_ELEMENTS || self.regions.len() > MAX_REGIONS {
            return Err(NetworkError::BudgetExceeded);
        }
        let refs = self.regions.iter().flat_map(|r| r.loops.iter()).fold(0_usize, |n, l| n.saturating_add(l.len()));
        if refs > MAX_LOOP_REFERENCES {
            return Err(NetworkError::BudgetExceeded);
        }
        for v in &self.vertices {
            finite(v.point())?;
            if v.corner_radius.is_some_and(|r| !r.is_finite() || r < 0.0) {
                return Err(NetworkError::OutOfRange);
            }
        }
        for s in &self.segments {
            let (a, b) = (self.vertex(s.start)?, self.vertex(s.end)?);
            for (t, at) in [(s.tangent_start, a), (s.tangent_end, b)] {
                if let Some(t) = t {
                    finite(t)?;
                    finite(PathPoint::new(at.x + t.x, at.y + t.y))?;
                }
            }
            if s.start == s.end && zero(s.tangent_start) && zero(s.tangent_end) {
                return Err(NetworkError::Degenerate);
            }
        }
        for r in &self.regions {
            if r.loops.is_empty() || r.fill_style_id.as_ref().is_some_and(|id| id.is_empty() || id.len() > 128 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')) {
                return Err(NetworkError::InvalidRegion);
            }
            for l in &r.loops {
                self.loop_vertices(l)?;
            }
        }
        Ok(())
    }

    fn vertex(&self, i: usize) -> Result<&NetworkVertex, NetworkError> {
        self.vertices.get(i).ok_or(NetworkError::IndexOutOfBounds)
    }

    fn segment(&self, i: usize) -> Result<&NetworkSegment, NetworkError> {
        self.segments.get(i).ok_or(NetworkError::IndexOutOfBounds)
    }

    /// The vertex walk of a closed loop of segments: for each segment the vertex it is entered at
    /// and whether it is traversed reversed.
    fn loop_vertices(&self, l: &[usize]) -> Result<Vec<(usize, bool)>, NetworkError> {
        let first = self.segment(*l.first().ok_or(NetworkError::OpenLoop)?)?;
        // Choose the start vertex so that the walk closes: try both orientations of the first segment.
        for start in [first.start, first.end] {
            let mut at = start;
            let mut walk = Vec::with_capacity(l.len());
            let mut ok = true;
            for si in l {
                let s = self.segment(*si)?;
                if s.start == at {
                    walk.push((at, false));
                    at = s.end;
                } else if s.end == at {
                    walk.push((at, true));
                    at = s.start;
                } else {
                    ok = false;
                    break;
                }
            }
            if ok && at == start {
                return Ok(walk);
            }
        }
        Err(NetworkError::OpenLoop)
    }

    /// The cubic control points of segment `i`, traversed forward or reversed.
    fn controls(&self, i: usize, reversed: bool) -> Result<[PathPoint; 4], NetworkError> {
        let s = self.segment(i)?;
        let (a, b) = (self.vertex(s.start)?.point(), self.vertex(s.end)?.point());
        let ts = s.tangent_start.unwrap_or(PathPoint::new(0.0, 0.0));
        let te = s.tangent_end.unwrap_or(PathPoint::new(0.0, 0.0));
        let c = [a, PathPoint::new(a.x + ts.x, a.y + ts.y), PathPoint::new(b.x + te.x, b.y + te.y), b];
        Ok(if reversed { [c[3], c[2], c[1], c[0]] } else { c })
    }

    /// Lossless promotion of an anchor path (STU-VEC-001): every anchor a vertex (its mirroring kept),
    /// every segment a network segment (its handles as tangents), every closed contour one filled
    /// region with the path's winding rule.
    pub fn from_path(path: &AnchorPath) -> Result<Self, NetworkError> {
        path.validate().map_err(|_| NetworkError::Degenerate)?;
        let mut net = Self::default();
        for c in &path.contours {
            let base = net.vertices.len();
            net.vertices.extend(c.anchors.iter().map(|a| NetworkVertex { handle_mirroring: Some(a.mirroring), ..NetworkVertex::at(a.position.x, a.position.y) }));
            let n = c.anchors.len();
            let mut ring = Vec::with_capacity(c.segment_count());
            for i in 0..c.segment_count() {
                let j = i.checked_add(1).and_then(|j| j.checked_rem(n)).ok_or(NetworkError::Degenerate)?;
                let (a, b) = (c.anchors.get(i).ok_or(NetworkError::Degenerate)?, c.anchors.get(j).ok_or(NetworkError::Degenerate)?);
                let t = |p: PathPoint| (p.x != 0.0 || p.y != 0.0).then_some(p);
                ring.push(net.segments.len());
                net.segments.push(NetworkSegment {
                    start: base.checked_add(i).ok_or(NetworkError::BudgetExceeded)?,
                    end: base.checked_add(j).ok_or(NetworkError::BudgetExceeded)?,
                    tangent_start: t(a.outgoing),
                    tangent_end: t(b.incoming),
                });
            }
            if c.closed {
                net.regions.push(NetworkRegion { winding_rule: path.fill_rule, loops: vec![ring], fills: true, fill_style_id: None });
            }
        }
        net.validate()?;
        Ok(net)
    }

    /// STU-VEC-108 derived `fill_geometry`: one anchor path per filled region (its loops as closed
    /// contours, its winding rule). Unfilled regions draw nothing.
    pub fn fill_geometry(&self) -> Result<Vec<AnchorPath>, NetworkError> {
        self.validate()?;
        let mut out = Vec::new();
        for r in self.regions.iter().filter(|r| r.fills) {
            let mut path = AnchorPath::new(r.winding_rule);
            for l in &r.loops {
                path.contours.push(self.loop_contour(l)?);
            }
            out.push(path);
        }
        Ok(out)
    }

    fn loop_contour(&self, l: &[usize]) -> Result<Contour, NetworkError> {
        let walk = self.loop_vertices(l)?;
        let m = walk.len();
        let mut anchors = walk
            .iter()
            .map(|(v, _)| {
                let vx = self.vertex(*v)?;
                Ok(EditAnchor { mirroring: vx.handle_mirroring.unwrap_or(Mirroring::None), ..EditAnchor::corner(vx.x, vx.y) })
            })
            .collect::<Result<Vec<_>, NetworkError>>()?;
        for (k, (si, (_, reversed))) in l.iter().zip(&walk).enumerate() {
            let c = self.controls(*si, *reversed)?;
            anchors.get_mut(k).ok_or(NetworkError::Degenerate)?.outgoing = PathPoint::new(c[1].x - c[0].x, c[1].y - c[0].y);
            let next = k.checked_add(1).and_then(|n| n.checked_rem(m)).ok_or(NetworkError::Degenerate)?;
            anchors.get_mut(next).ok_or(NetworkError::Degenerate)?.incoming = PathPoint::new(c[2].x - c[3].x, c[2].y - c[3].y);
        }
        Ok(Contour { anchors, closed: true })
    }

    /// STU-VEC-108 derived `stroke_geometry`: every segment once, as maximal chains through vertices
    /// of degree 2 (open contours; a cycle of degree-2 vertices is a closed contour).
    pub fn stroke_geometry(&self) -> Result<AnchorPath, NetworkError> {
        self.validate()?;
        let mut degree = vec![0_usize; self.vertices.len()];
        for s in &self.segments {
            for v in [s.start, s.end] {
                let d = degree.get_mut(v).ok_or(NetworkError::IndexOutOfBounds)?;
                *d = d.saturating_add(1);
            }
        }
        let mut used = vec![false; self.segments.len()];
        let mut path = AnchorPath::new(FillRule::NonZero);
        // Chains start at vertices of degree != 2 first, then the remaining cycles.
        let starts: Vec<usize> = (0..self.vertices.len()).filter(|v| degree.get(*v) != Some(&2)).chain(0..self.vertices.len()).collect();
        for start in starts {
            while let Some(first) = (0..self.segments.len()).find(|i| !used[*i] && (self.segments[*i].start == start || self.segments[*i].end == start)) {
                let mut chain: Vec<(usize, bool)> = Vec::new();
                let mut at = start;
                let mut si = first;
                loop {
                    used[si] = true;
                    let s = self.segments[si];
                    let reversed = s.end == at && s.start != at;
                    chain.push((si, reversed));
                    at = if reversed { s.start } else { s.end };
                    if at == start || degree.get(at) != Some(&2) {
                        break;
                    }
                    match (0..self.segments.len()).find(|i| !used[*i] && (self.segments[*i].start == at || self.segments[*i].end == at)) {
                        Some(n) => si = n,
                        None => break,
                    }
                }
                let closed = at == start && degree.get(start) == Some(&2);
                let mut anchors: Vec<EditAnchor> = Vec::with_capacity(chain.len().saturating_add(1));
                for (k, (si, reversed)) in chain.iter().enumerate() {
                    let c = self.controls(*si, *reversed)?;
                    if k == 0 {
                        anchors.push(EditAnchor::corner(c[0].x, c[0].y));
                    }
                    if let Some(last) = anchors.last_mut() {
                        last.outgoing = PathPoint::new(c[1].x - c[0].x, c[1].y - c[0].y);
                    }
                    anchors.push(EditAnchor { incoming: PathPoint::new(c[2].x - c[3].x, c[2].y - c[3].y), ..EditAnchor::corner(c[3].x, c[3].y) });
                }
                if closed && let Some(last) = anchors.pop() && let Some(first) = anchors.first_mut() {
                    first.incoming = last.incoming;
                }
                path.contours.push(Contour { anchors, closed });
            }
        }
        path.validate().map_err(|_| NetworkError::Degenerate)?;
        Ok(path)
    }

    /// Paint bucket (STU-VEC-004): fill or clear one region; topology untouched.
    pub fn paint_bucket(&self, region: usize, fill: bool) -> Result<Self, NetworkError> {
        self.validate()?;
        let mut out = self.clone();
        out.regions.get_mut(region).ok_or(NetworkError::IndexOutOfBounds)?.fills = fill;
        Ok(out)
    }

    /// The bounded faces of the planar embedding (segments meet only at shared vertices): each face
    /// a loop of segment indices with positive orientation (counter-clockwise, y up). The unbounded
    /// face is dropped. Faces are new regions with the given fill state and NONZERO winding.
    pub fn detect_regions(&self, fills: bool) -> Result<Vec<NetworkRegion>, NetworkError> {
        self.validate()?;
        let n = self.segments.len();
        // Half-edge h = 2 * s (forward) or 2 * s + 1 (reversed); outgoing half-edges per vertex
        // sorted by the pseudo-angle of their leaving tangent.
        let mut out_edges: Vec<Vec<(f64, usize)>> = vec![Vec::new(); self.vertices.len()];
        for s in 0..n {
            for reversed in [false, true] {
                let c = self.controls(s, reversed)?;
                let mut d = PathPoint::new(c[1].x - c[0].x, c[1].y - c[0].y);
                if d.x == 0.0 && d.y == 0.0 {
                    d = PathPoint::new(c[2].x - c[0].x, c[2].y - c[0].y);
                }
                if d.x == 0.0 && d.y == 0.0 {
                    d = PathPoint::new(c[3].x - c[0].x, c[3].y - c[0].y);
                }
                let h = s.checked_mul(2).and_then(|h| h.checked_add(usize::from(reversed))).ok_or(NetworkError::BudgetExceeded)?;
                let from = if reversed { self.segments[s].end } else { self.segments[s].start };
                out_edges.get_mut(from).ok_or(NetworkError::IndexOutOfBounds)?.push((pseudo_angle(d.x, -d.y), h));
            }
        }
        for list in &mut out_edges {
            list.sort_by(|a, b| a.0.total_cmp(&b.0));
        }
        let half = n.checked_mul(2).ok_or(NetworkError::BudgetExceeded)?;
        let mut visited = vec![false; half];
        let mut faces = Vec::new();
        for h0 in 0..half {
            if visited[h0] {
                continue;
            }
            let mut face: Vec<usize> = Vec::new();
            let mut h = h0;
            let mut steps = 0_usize;
            while !visited[h] {
                visited[h] = true;
                face.push(h);
                steps = steps.saturating_add(1);
                if steps > half {
                    return Err(NetworkError::Degenerate);
                }
                // Arrive at the head vertex; take the twin, then the next outgoing edge clockwise.
                let s = h / 2;
                let reversed = h % 2 == 1;
                let head = if reversed { self.segments[s].start } else { self.segments[s].end };
                let twin = h ^ 1;
                let list = out_edges.get(head).ok_or(NetworkError::IndexOutOfBounds)?;
                let at = list.iter().position(|(_, e)| *e == twin).ok_or(NetworkError::Degenerate)?;
                let next = if at == 0 { list.len().saturating_sub(1) } else { at.saturating_sub(1) };
                h = list.get(next).ok_or(NetworkError::Degenerate)?.1;
            }
            // Signed area on 16 samples per segment.
            let mut area = 0.0;
            let mut pts: Vec<PathPoint> = Vec::new();
            for e in &face {
                let c = self.controls(e / 2, e % 2 == 1)?;
                for k in 0..16_u32 {
                    let t = f64::from(k) / 16.0;
                    let u = 1.0 - t;
                    let (a, b, cc, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                    pts.push(PathPoint::new(a * c[0].x + b * c[1].x + cc * c[2].x + d * c[3].x, a * c[0].y + b * c[1].y + cc * c[2].y + d * c[3].y));
                }
            }
            // Shoelace in the y-up frame (y negated): bounded faces walk counter-clockwise there.
            for (i, p) in pts.iter().enumerate() {
                let q = pts.get(i.saturating_add(1).checked_rem(pts.len()).unwrap_or(0)).copied().unwrap_or(*p);
                area += p.x * (-q.y) - q.x * (-p.y);
            }
            if area > 0.0 {
                faces.push(NetworkRegion { winding_rule: FillRule::NonZero, loops: vec![face.iter().map(|e| e / 2).collect()], fills, fill_style_id: None });
            }
        }
        Ok(faces)
    }
}
