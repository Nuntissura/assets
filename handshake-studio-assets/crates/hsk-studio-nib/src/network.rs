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
            // Degenerate walks (a dangling edge out and back) have zero area up to rounding: they are
            // not faces. The threshold is relative to the face's extent.
            let extent = pts.iter().fold(1.0_f64, |m, q| m.max(q.x.abs()).max(q.y.abs()));
            if area > 1e-9 * extent * extent {
                faces.push(NetworkRegion { winding_rule: FillRule::NonZero, loops: vec![face.iter().map(|e| e / 2).collect()], fills, fill_style_id: None });
            }
        }
        Ok(faces)
    }
}

// ---- lane SMB-ILLUSTRATOR-PEN-3: planarize (crossings and T-junctions) ------------------------------

/// Samples per curved segment used to find crossings.
const CURVE_SAMPLES: u32 = 64;
/// Most candidate sub-segment comparisons of one planarize call.
pub const MAX_PLANARIZE_WORK: u64 = 1 << 26;

fn lerp_pt(a: PathPoint, b: PathPoint, t: f64) -> PathPoint {
    PathPoint::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

/// `(left, right)` of cubic `c` split at `t` by de Casteljau.
fn split_cubic(c: [PathPoint; 4], t: f64) -> ([PathPoint; 4], [PathPoint; 4]) {
    let (p01, p12, p23) = (lerp_pt(c[0], c[1], t), lerp_pt(c[1], c[2], t), lerp_pt(c[2], c[3], t));
    let (p012, p123) = (lerp_pt(p01, p12, t), lerp_pt(p12, p23, t));
    let m = lerp_pt(p012, p123, t);
    ([c[0], p01, p012, m], [m, p123, p23, c[3]])
}

fn bez(c: &[PathPoint; 4], t: f64) -> PathPoint {
    let u = 1.0 - t;
    let (a, b, cc, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    PathPoint::new(a * c[0].x + b * c[1].x + cc * c[2].x + d * c[3].x, a * c[0].y + b * c[1].y + cc * c[2].y + d * c[3].y)
}

/// Polyline of a segment: `(point, parameter)`; a straight segment is its two ends.
fn polyline(net: &VectorNetwork, s: usize) -> Result<Vec<(PathPoint, f64)>, NetworkError> {
    let seg = net.segment(s)?;
    let c = net.controls(s, false)?;
    if zero(seg.tangent_start) && zero(seg.tangent_end) {
        return Ok(vec![(c[0], 0.0), (c[3], 1.0)]);
    }
    Ok((0..=CURVE_SAMPLES).map(|k| {
        let t = f64::from(k) / f64::from(CURVE_SAMPLES);
        (bez(&c, t), t)
    }).collect())
}

/// Parameters `(u, v)` in `[0, 1]` where segments `a0-a1` and `b0-b1` meet, if they do (not parallel).
fn hit(a0: PathPoint, a1: PathPoint, b0: PathPoint, b1: PathPoint) -> Option<(f64, f64)> {
    let (r, s) = (PathPoint::new(a1.x - a0.x, a1.y - a0.y), PathPoint::new(b1.x - b0.x, b1.y - b0.y));
    let den = r.x * s.y - r.y * s.x;
    if den == 0.0 {
        return None;
    }
    let q = PathPoint::new(b0.x - a0.x, b0.y - a0.y);
    let u = (q.x * s.y - q.y * s.x) / den;
    let v = (q.x * r.y - q.y * r.x) / den;
    ((0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v)).then_some((u, v))
}

/// Distance from `p` to segment `a-b` and the parameter of the nearest point.
fn nearest(p: PathPoint, a: PathPoint, b: PathPoint) -> (f64, f64) {
    let d = PathPoint::new(b.x - a.x, b.y - a.y);
    let l2 = d.x * d.x + d.y * d.y;
    let t = if l2 == 0.0 { 0.0 } else { (((p.x - a.x) * d.x + (p.y - a.y) * d.y) / l2).clamp(0.0, 1.0) };
    let q = lerp_pt(a, b, t);
    (((p.x - q.x).powi(2) + (p.y - q.y).powi(2)).sqrt(), t)
}

impl VectorNetwork {
    /// Planarize (lane SMB-ILLUSTRATOR-PEN-3): splits every segment where it crosses another
    /// segment (a new vertex shared by both) or where a vertex lies on its interior within
    /// `tolerance` (a T-junction: split at the existing vertex), so [`VectorNetwork::detect_regions`]
    /// finds faces without segments having to meet at shared vertices. Curves are located on 64
    /// samples (crossing parameters are approximate to the sampling; the split vertex is the found
    /// point and the sub-curves are exact de Casteljau pieces moved to it). Region loops are remapped
    /// onto the sub-segments in walk order, so fill state is kept. Split points closer than
    /// `tolerance` merge. Bounded by [`MAX_PLANARIZE_WORK`] comparisons before splitting.
    pub fn planarize(&self, tolerance: f64) -> Result<Self, NetworkError> {
        if !tolerance.is_finite() {
            return Err(NetworkError::NonFinite);
        }
        if tolerance <= 0.0 {
            return Err(NetworkError::OutOfRange);
        }
        self.validate()?;
        let n = self.segments.len();
        // Pair and vertex-segment scans are bounded before they run.
        let pairs = u64::try_from(n).unwrap_or(u64::MAX).saturating_mul(u64::try_from(n).unwrap_or(u64::MAX));
        let vs = u64::try_from(self.vertices.len()).unwrap_or(u64::MAX).saturating_mul(u64::try_from(n).unwrap_or(u64::MAX));
        if pairs > MAX_PLANARIZE_WORK || vs > MAX_PLANARIZE_WORK {
            return Err(NetworkError::BudgetExceeded);
        }
        let lines = (0..n).map(|s| polyline(self, s)).collect::<Result<Vec<_>, _>>()?;
        let bbox = |l: &[(PathPoint, f64)]| {
            l.iter().fold([f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY], |b, (p, _)| [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)])
        };
        let boxes: Vec<[f64; 4]> = lines.iter().map(|l| bbox(l)).collect();
        let mut vertices = self.vertices.clone();
        // Split requests: (segment, parameter, vertex).
        let mut splits: Vec<(usize, f64, usize)> = Vec::new();
        let vertex_at = |p: PathPoint, vertices: &mut Vec<NetworkVertex>| -> Result<usize, NetworkError> {
            if let Some(i) = vertices.iter().position(|v| (v.x - p.x).abs() <= tolerance && (v.y - p.y).abs() <= tolerance) {
                return Ok(i);
            }
            if vertices.len() >= MAX_NETWORK_ELEMENTS {
                return Err(NetworkError::BudgetExceeded);
            }
            vertices.push(NetworkVertex::at(p.x, p.y));
            Ok(vertices.len().saturating_sub(1))
        };
        let mut work = 0_u64;
        let interior = |t: f64| t > 1e-9 && t < 1.0 - 1e-9;
        for i in 0..n {
            for j in i.saturating_add(1)..n {
                let (a, b) = (boxes[i], boxes[j]);
                if a[2] + tolerance < b[0] || b[2] + tolerance < a[0] || a[3] + tolerance < b[1] || b[3] + tolerance < a[1] {
                    continue;
                }
                let (li, lj) = (&lines[i], &lines[j]);
                work = work.saturating_add(u64::try_from(li.len().saturating_mul(lj.len())).unwrap_or(u64::MAX));
                if work > MAX_PLANARIZE_WORK {
                    return Err(NetworkError::BudgetExceeded);
                }
                for wi in li.windows(2) {
                    for wj in lj.windows(2) {
                        let Some((u, v)) = hit(wi[0].0, wi[1].0, wj[0].0, wj[1].0) else { continue };
                        let ti = wi[0].1 + (wi[1].1 - wi[0].1) * u;
                        let tj = wj[0].1 + (wj[1].1 - wj[0].1) * v;
                        if !interior(ti) && !interior(tj) {
                            continue; // meeting at endpoints: already a shared or separate vertex
                        }
                        let p = lerp_pt(wi[0].0, wi[1].0, u);
                        let vtx = vertex_at(p, &mut vertices)?;
                        if interior(ti) {
                            splits.push((i, ti, vtx));
                        }
                        if interior(tj) {
                            splits.push((j, tj, vtx));
                        }
                    }
                }
            }
        }
        // Near-miss T-junctions: a vertex within tolerance of a segment's interior.
        for (s, line) in lines.iter().enumerate() {
            let seg = self.segment(s)?;
            for (vi, v) in self.vertices.iter().enumerate() {
                if vi == seg.start || vi == seg.end {
                    continue;
                }
                let b = boxes[s];
                if v.x < b[0] - tolerance || v.x > b[2] + tolerance || v.y < b[1] - tolerance || v.y > b[3] + tolerance {
                    continue;
                }
                work = work.saturating_add(u64::try_from(line.len()).unwrap_or(u64::MAX));
                if work > MAX_PLANARIZE_WORK {
                    return Err(NetworkError::BudgetExceeded);
                }
                for w in line.windows(2) {
                    let (d, u) = nearest(v.point(), w[0].0, w[1].0);
                    let t = w[0].1 + (w[1].1 - w[0].1) * u;
                    if d <= tolerance && interior(t) && !splits.iter().any(|(ss, _, sv)| *ss == s && *sv == vi) {
                        splits.push((s, t, vi));
                        break;
                    }
                }
            }
        }
        // Rebuild segments: each original segment becomes a chain of sub-segments.
        let mut segments = Vec::new();
        let mut chains: Vec<Vec<usize>> = Vec::with_capacity(n);
        for s in 0..n {
            let seg = *self.segment(s)?;
            let mut cuts: Vec<(f64, usize)> = splits.iter().filter(|(ss, _, _)| *ss == s).map(|(_, t, v)| (*t, *v)).collect();
            cuts.sort_by(|a, b| a.0.total_cmp(&b.0));
            cuts.dedup_by(|a, b| a.1 == b.1 || (a.0 - b.0).abs() <= 1e-12);
            let straight = zero(seg.tangent_start) && zero(seg.tangent_end);
            let mut rest = self.controls(s, false)?;
            let (mut from_t, mut from_v) = (0.0_f64, seg.start);
            let mut chain = Vec::with_capacity(cuts.len().saturating_add(1));
            for (t, v) in cuts.iter().copied().chain(std::iter::once((1.0, seg.end))) {
                let (piece, right) = if t >= 1.0 { (rest, rest) } else { split_cubic(rest, (t - from_t) / (1.0 - from_t)) };
                rest = right;
                if v == from_v {
                    from_t = t;
                    continue;
                }
                let (pa, pb) = (vertices.get(from_v).ok_or(NetworkError::IndexOutOfBounds)?.point(), vertices.get(v).ok_or(NetworkError::IndexOutOfBounds)?.point());
                let tangent = |h: PathPoint, at: PathPoint| PathPoint::new(h.x - at.x, h.y - at.y);
                chain.push(segments.len());
                segments.push(if straight {
                    NetworkSegment::line(from_v, v)
                } else {
                    NetworkSegment { start: from_v, end: v, tangent_start: Some(tangent(piece[1], pa)), tangent_end: Some(tangent(piece[2], pb)) }
                });
                if segments.len() > MAX_NETWORK_ELEMENTS {
                    return Err(NetworkError::BudgetExceeded);
                }
                from_t = t;
                from_v = v;
            }
            chains.push(chain);
        }
        // Remap region loops onto the chains in walk order.
        let mut regions = Vec::with_capacity(self.regions.len());
        for r in &self.regions {
            let mut loops = Vec::with_capacity(r.loops.len());
            for l in &r.loops {
                let walk = self.loop_vertices(l)?;
                let mut out = Vec::new();
                for (s, (_, reversed)) in l.iter().zip(&walk) {
                    let chain = chains.get(*s).ok_or(NetworkError::IndexOutOfBounds)?;
                    if *reversed {
                        out.extend(chain.iter().rev().copied());
                    } else {
                        out.extend(chain.iter().copied());
                    }
                }
                loops.push(out);
            }
            regions.push(NetworkRegion { loops, ..r.clone() });
        }
        let out = Self { vertices, segments, regions };
        out.validate()?;
        Ok(out)
    }
}
