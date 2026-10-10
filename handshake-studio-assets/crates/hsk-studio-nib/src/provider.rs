//! Exact axis-line specialization of the pinned Vectorcraft/linesweeper leaf.
//! See SOURCE_PROVENANCE.json for retained algorithms and deliberately removed states.
//! All scratch and returned geometry are inline; this module requests no heap allocation.
use crate::{Error, Limits, Operation, Path, Point, Winding, WorkMeter};
use hsk_studio_accord::DomainId;

const EDGES: usize = 8;
const CUTS: usize = 16;
const VERTICES: usize = 64;
const PIECES: usize = 120;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    start: usize,
    end: usize,
}
impl Segment {
    pub fn start(&self) -> usize {
        self.start
    }
    pub fn end(&self) -> usize {
        self.end
    }
    pub fn incoming_tangent(&self) -> Option<Point> {
        None
    }
    pub fn outgoing_tangent(&self) -> Option<Point> {
        None
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Loop {
    start: usize,
    len: usize,
    outer: bool,
}
impl Loop {
    pub fn is_outer(&self) -> bool {
        self.outer
    }
    pub fn segment_range(&self) -> std::ops::Range<usize> {
        self.start..self.start + self.len
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Region<'a> {
    start: usize,
    len: usize,
    style: &'a DomainId,
}
impl<'a> Region<'a> {
    pub fn loop_range(&self) -> std::ops::Range<usize> {
        self.start..self.start + self.len
    }
    pub fn fill_style_id(&self) -> &'a DomainId {
        self.style
    }
    pub fn winding_rule(&self) -> Winding {
        Winding::NonZero
    }
}
/// Immutable, identity-free canonical geometry. Shared style identity is borrowed.
#[derive(Debug)]
pub struct Network<'a> {
    vertices: [Point; VERTICES],
    vertex_len: usize,
    segments: [Segment; PIECES],
    segment_len: usize,
    loops: [Loop; PIECES],
    loop_len: usize,
    refs: [usize; PIECES],
    ref_len: usize,
    regions: [Region<'a>; PIECES],
    region_len: usize,
    intersections: u64,
    sweep_events: u64,
}
impl<'a> Network<'a> {
    pub fn vertices(&self) -> &[Point] {
        &self.vertices[..self.vertex_len]
    }
    pub fn segments(&self) -> &[Segment] {
        &self.segments[..self.segment_len]
    }
    pub fn loops(&self) -> &[Loop] {
        &self.loops[..self.loop_len]
    }
    pub fn regions(&self) -> &[Region<'a>] {
        &self.regions[..self.region_len]
    }
    pub fn loop_segment_indices(&self, index: usize) -> Option<&[usize]> {
        self.loops()
            .get(index)
            .map(|v| &self.refs[v.segment_range()])
    }
    pub fn winding_rule(&self) -> Winding {
        Winding::NonZero
    }
    pub const fn requested_allocation_bytes(&self) -> u64 {
        0
    }
    pub fn intersections(&self) -> u64 {
        self.intersections
    }
    pub fn sweep_events(&self) -> u64 {
        self.sweep_events
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct P {
    x: f64,
    y: f64,
}
impl P {
    fn read(p: Point) -> Result<Self, Error> {
        if p.x.unit().as_str() != "pt" || p.y.unit().as_str() != "pt" {
            return Err(Error::InvalidGeometry);
        }
        let x = p.x.value();
        let y = p.y.value();
        if !x.is_finite() || !y.is_finite() {
            return Err(Error::InvalidGeometry);
        }
        Ok(Self {
            x: if x == 0.0 { 0.0 } else { x },
            y: if y == 0.0 { 0.0 } else { y },
        })
    }
    fn point(self) -> Result<Point, Error> {
        Point::pt(self.x, self.y)
    }
    fn cmp(self, other: Self) -> std::cmp::Ordering {
        self.x.total_cmp(&other.x).then(self.y.total_cmp(&other.y))
    }
}
#[derive(Clone, Copy)]
struct Edge {
    a: P,
    b: P,
    owner: usize,
}
impl Edge {
    fn vertical(self) -> bool {
        self.a.x == self.b.x
    }
    fn contains(self, p: P) -> bool {
        if self.vertical() {
            p.x == self.a.x && p.y >= self.a.y.min(self.b.y) && p.y <= self.a.y.max(self.b.y)
        } else {
            p.y == self.a.y && p.x >= self.a.x.min(self.b.x) && p.x <= self.a.x.max(self.b.x)
        }
    }
}
#[derive(Clone, Copy, Default)]
struct BinaryWinding {
    a: i32,
    b: i32,
}
impl BinaryWinding {
    // linesweeper BinaryWindingNumber::single/AddAssign, specialized to two tags.
    fn add(&mut self, owner: usize, sign: i32) -> Result<(), Error> {
        let value = if owner == 0 { &mut self.a } else { &mut self.b };
        *value = value.checked_add(sign).ok_or(Error::Overflow)?;
        Ok(())
    }
}
fn inside(rule: Winding, value: i32) -> bool {
    match rule {
        Winding::NonZero => value != 0,
        Winding::EvenOdd => value % 2 != 0,
        Winding::None => false,
    }
}
fn selected(w: BinaryWinding, paths: [&Path<'_>; 2], op: Operation, bottom: usize) -> bool {
    let a = inside(paths[0].winding_rule, w.a);
    let b = inside(paths[1].winding_rule, w.b);
    let back = if bottom == 0 { a } else { b };
    let front = if bottom == 0 { b } else { a };
    // Vectorcraft BoolOp::Union/Difference::apply.
    match op {
        Operation::Unite => a || b,
        Operation::BackMinusFront => back && !front,
        Operation::FrontMinusBack => front && !back,
    }
}
fn bound(value: usize, limit: u64) -> Result<(), Error> {
    if u64::try_from(value).map_err(|_| Error::Overflow)? > limit {
        Err(Error::BudgetExceeded)
    } else {
        Ok(())
    }
}
fn rect(
    path: &Path<'_>,
    owner: usize,
    out: &mut [Edge; EDGES],
    work: &mut WorkMeter<'_>,
) -> Result<(), Error> {
    if !path.closed {
        return Err(Error::InvalidGeometry);
    }
    if path.anchors.len() != 4 {
        return Err(Error::UnsupportedApproximation);
    }
    let mut points = [out[0].a; 4];
    for (i, anchor) in path.anchors.iter().enumerate() {
        work.step()?;
        points[i] = P::read(anchor.position)?;
        for handle in [anchor.incoming, anchor.outgoing] {
            let h = P::read(handle)?;
            if h.x != 0.0 || h.y != 0.0 {
                return Err(Error::UnsupportedApproximation);
            }
        }
    }
    for i in 0..4 {
        work.step()?;
        let a = points[i];
        let b = points[(i + 1) % 4];
        if a == b || (a.x != b.x && a.y != b.y) {
            return Err(Error::UnsupportedApproximation);
        }
        let c = points[(i + 2) % 4];
        if (a.x == b.x) == (b.x == c.x) {
            return Err(Error::UnsupportedApproximation);
        }
        if points[(i + 2) % 4] == a {
            return Err(Error::InvalidGeometry);
        }
        out[owner * 4 + i] = Edge { a, b, owner };
    }
    if points[0].x != points[3].x && points[0].y != points[3].y {
        return Err(Error::InvalidGeometry);
    }
    Ok(())
}
fn cut_insert(
    list: &mut [P; CUTS],
    len: &mut usize,
    point: P,
    work: &mut WorkMeter<'_>,
) -> Result<(), Error> {
    for current in &list[..*len] {
        work.step()?;
        if *current == point {
            return Ok(());
        }
    }
    work.step()?;
    if *len == CUTS {
        return Err(Error::ProviderRefused);
    }
    list[*len] = point;
    *len += 1;
    Ok(())
}
fn cuts_pair(a: Edge, b: Edge, points: &mut [P; 2]) -> usize {
    if a.vertical() != b.vertical() {
        let (v, h) = if a.vertical() { (a, b) } else { (b, a) };
        let p = P { x: v.a.x, y: h.a.y };
        if v.contains(p) && h.contains(p) {
            points[0] = p;
            1
        } else {
            0
        }
    } else {
        let aligned = if a.vertical() {
            a.a.x == b.a.x
        } else {
            a.a.y == b.a.y
        };
        if !aligned {
            return 0;
        }
        let (a0, a1) = if a.a.cmp(a.b).is_le() {
            (a.a, a.b)
        } else {
            (a.b, a.a)
        };
        let (b0, b1) = if b.a.cmp(b.b).is_le() {
            (b.a, b.b)
        } else {
            (b.b, b.a)
        };
        let lo = if a0.cmp(b0).is_ge() { a0 } else { b0 };
        let hi = if a1.cmp(b1).is_le() { a1 } else { b1 };
        if lo.cmp(hi).is_gt() {
            0
        } else if lo == hi {
            points[0] = lo;
            1
        } else {
            points[0] = lo;
            points[1] = hi;
            2
        }
    }
}
/// Source windings on the two sides of a canonical (east/north) atomic line.
/// Formal open-interval predicates avoid midpoint rounding and all new coordinates.
fn side_windings(
    a: P,
    b: P,
    edges: &[Edge; EDGES],
    work: &mut WorkMeter<'_>,
) -> Result<(BinaryWinding, BinaryWinding), Error> {
    let mut left = BinaryWinding::default();
    let mut right = BinaryWinding::default();
    let mut order = [0, 1, 2, 3, 4, 5, 6, 7];
    for i in 1..EDGES {
        let mut j = i;
        while j > 0 {
            work.step()?;
            let x = edges[order[j]].a.x.total_cmp(&edges[order[j - 1]].a.x);
            if x.is_gt() || (x.is_eq() && order[j] > order[j - 1]) {
                break;
            }
            order.swap(j, j - 1);
            j -= 1;
        }
    }
    for index in order {
        work.step()?;
        let e = edges[index];
        if !e.vertical() {
            continue;
        }
        let lo = e.a.y.min(e.b.y);
        let hi = e.a.y.max(e.b.y);
        let sign = if e.b.y > e.a.y { -1 } else { 1 };
        if a.x == b.x {
            if lo <= a.y && hi >= b.y {
                if e.a.x < a.x {
                    left.add(e.owner, sign)?;
                }
                if e.a.x <= a.x {
                    right.add(e.owner, sign)?;
                }
            }
        } else if e.a.x <= a.x {
            // For an east-directed line left=north, right=south.
            if lo <= a.y && a.y < hi {
                left.add(e.owner, sign)?;
            }
            if lo < a.y && a.y <= hi {
                right.add(e.owner, sign)?;
            }
        }
    }
    Ok((left, right))
}
fn vertex(
    net: &mut Network<'_>,
    p: P,
    limits: &Limits,
    work: &mut WorkMeter<'_>,
) -> Result<usize, Error> {
    for (i, v) in net.vertices().iter().enumerate() {
        work.step()?;
        if P::read(*v)? == p {
            return Ok(i);
        }
    }
    work.step()?;
    bound(net.vertex_len + 1, limits.output_vertices)?;
    if net.vertex_len == VERTICES {
        return Err(Error::ProviderRefused);
    }
    let i = net.vertex_len;
    net.vertices[i] = p.point()?;
    net.vertex_len += 1;
    Ok(i)
}
fn direction(a: P, b: P) -> Result<usize, Error> {
    if a.y == b.y && a.x < b.x {
        Ok(0)
    } else if a.x == b.x && a.y < b.y {
        Ok(1)
    } else if a.y == b.y && a.x > b.x {
        Ok(2)
    } else if a.x == b.x && a.y > b.y {
        Ok(3)
    } else {
        Err(Error::ProviderRefused)
    }
}
fn successor(net: &Network<'_>, edge: usize, work: &mut WorkMeter<'_>) -> Result<usize, Error> {
    let s = net.segments[edge];
    let end = P::read(net.vertices[s.end])?;
    let reverse = (direction(P::read(net.vertices[s.start])?, end)? + 2) % 4;
    let mut best = None;
    let mut turn = 5;
    // linesweeper contour walk: around the endpoint, find the next visible halfedge.
    for (i, candidate) in net.segments().iter().enumerate() {
        work.step()?;
        if candidate.start != s.end {
            continue;
        }
        let d = direction(end, P::read(net.vertices[candidate.end])?)?;
        let t = (reverse + 4 - d) % 4;
        if t == 0 {
            return Err(Error::ProviderRefused);
        }
        if t < turn {
            turn = t;
            best = Some(i);
        } else if t == turn {
            return Err(Error::ProviderRefused);
        }
    }
    best.ok_or(Error::ProviderRefused)
}
fn loop_cmp(
    net: &Network<'_>,
    a: Loop,
    b: Loop,
    work: &mut WorkMeter<'_>,
) -> Result<std::cmp::Ordering, Error> {
    for i in 0..a.len.min(b.len) {
        work.step()?;
        let pa = P::read(net.vertices[net.segments[net.refs[a.start + i]].start])?;
        let pb = P::read(net.vertices[net.segments[net.refs[b.start + i]].start])?;
        let order = pa.cmp(pb);
        if !order.is_eq() {
            return Ok(order);
        }
    }
    Ok(a.len.cmp(&b.len))
}
fn normalize_loop(
    net: &mut Network<'_>,
    index: usize,
    work: &mut WorkMeter<'_>,
) -> Result<(), Error> {
    let l = net.loops[index];
    let mut best = 0;
    for i in 1..l.len {
        for offset in 0..l.len {
            work.step()?;
            let pa = P::read(
                net.vertices[net.segments[net.refs[l.start + (i + offset) % l.len]].start],
            )?;
            let pb = P::read(
                net.vertices[net.segments[net.refs[l.start + (best + offset) % l.len]].start],
            )?;
            let order = pa.cmp(pb);
            if order.is_lt() {
                best = i;
                break;
            }
            if order.is_gt() {
                break;
            }
        }
    }
    // Bounded in-place rotation; no std sorting scratch or temporary Vec.
    for _ in 0..best {
        let first = net.refs[l.start];
        for i in 1..l.len {
            work.step()?;
            net.refs[l.start + i - 1] = net.refs[l.start + i];
        }
        net.refs[l.start + l.len - 1] = first;
    }
    Ok(())
}
fn contains_loop(
    net: &Network<'_>,
    l: Loop,
    p: P,
    work: &mut WorkMeter<'_>,
) -> Result<bool, Error> {
    let mut winding = 0_i32;
    for &idx in &net.refs[l.segment_range()] {
        work.step()?;
        let s = net.segments[idx];
        let a = P::read(net.vertices[s.start])?;
        let b = P::read(net.vertices[s.end])?;
        if a.x == b.x && a.x < p.x && a.y.min(b.y) <= p.y && p.y < a.y.max(b.y) {
            winding = winding
                .checked_add(if b.y > a.y { -1 } else { 1 })
                .ok_or(Error::Overflow)?;
        }
    }
    Ok(winding != 0)
}
fn contours(net: &mut Network<'_>, limits: &Limits, work: &mut WorkMeter<'_>) -> Result<(), Error> {
    let mut visited = [false; PIECES];
    for start in 0..net.segment_len {
        work.step()?;
        if visited[start] {
            continue;
        }
        bound(net.loop_len + 1, limits.output_loops)?;
        if net.loop_len == PIECES {
            return Err(Error::ProviderRefused);
        }
        let first_ref = net.ref_len;
        let mut next = start;
        let mut turns = 0_i32;
        loop {
            work.step()?;
            if visited[next] || net.ref_len == PIECES {
                return Err(Error::ProviderRefused);
            }
            visited[next] = true;
            net.refs[net.ref_len] = next;
            net.ref_len += 1;
            let s = net.segments[next];
            let a = P::read(net.vertices[s.start])?;
            let b = P::read(net.vertices[s.end])?;
            let following = successor(net, next, work)?;
            let end = P::read(net.vertices[net.segments[following].end])?;
            let change = (direction(b, end)? + 4 - direction(a, b)?) % 4;
            turns += match change {
                0 => 0,
                1 => 1,
                3 => -1,
                _ => return Err(Error::ProviderRefused),
            };
            next = following;
            if next == start {
                break;
            }
        }
        // For a simple orthogonal cycle total turn +/-4 has the same orientation
        // as signed shoelace area, without floating multiplication overflow/underflow.
        if turns != 4 && turns != -4 {
            return Err(Error::ProviderRefused);
        }
        let i = net.loop_len;
        net.loops[i] = Loop {
            start: first_ref,
            len: net.ref_len - first_ref,
            outer: turns > 0,
        };
        net.loop_len += 1;
        normalize_loop(net, i, work)?;
    }
    // Stable loop order and source scan-west parent relation, with flat ownership.
    let mut order = [0; PIECES];
    let mut parents = [usize::MAX; PIECES];
    for (i, slot) in order[..net.loop_len].iter_mut().enumerate() {
        *slot = i;
    }
    for i in 1..net.loop_len {
        let mut j = i;
        while j > 0 {
            work.step()?;
            if !loop_cmp(net, net.loops[order[j]], net.loops[order[j - 1]], work)?.is_lt() {
                break;
            }
            order.swap(j, j - 1);
            j -= 1;
        }
    }
    for (i, parent_slot) in parents[..net.loop_len].iter_mut().enumerate() {
        work.step()?;
        if net.loops[i].outer {
            continue;
        }
        let l = net.loops[i];
        let p = P::read(net.vertices[net.segments[net.refs[l.start]].start])?;
        let mut west = f64::NEG_INFINITY;
        for j in 0..net.loop_len {
            work.step()?;
            if !net.loops[j].outer || !contains_loop(net, net.loops[j], p, work)? {
                continue;
            }
            for &idx in &net.refs[net.loops[j].segment_range()] {
                work.step()?;
                let s = net.segments[idx];
                let a = P::read(net.vertices[s.start])?;
                let b = P::read(net.vertices[s.end])?;
                if a.x == b.x
                    && a.x < p.x
                    && a.y.min(b.y) <= p.y
                    && p.y < a.y.max(b.y)
                    && a.x > west
                {
                    west = a.x;
                    *parent_slot = j;
                }
            }
        }
        if *parent_slot == usize::MAX {
            return Err(Error::ProviderRefused);
        }
    }
    let mut ordered_loops = [net.loops[0]; PIECES];
    let mut ordered_refs = [0; PIECES];
    let mut count = 0;
    let mut refs = 0;
    for &outer in &order[..net.loop_len] {
        work.step()?;
        if !net.loops[outer].outer {
            continue;
        }
        bound(net.region_len + 1, limits.output_regions)?;
        if net.region_len == PIECES {
            return Err(Error::ProviderRefused);
        }
        let region_start = count;
        for phase in 0..=net.loop_len {
            work.step()?;
            let idx = if phase == 0 { outer } else { order[phase - 1] };
            if phase != 0 && parents[idx] != outer {
                continue;
            }
            if count == PIECES {
                return Err(Error::ProviderRefused);
            }
            let l = net.loops[idx];
            ordered_loops[count] = Loop {
                start: refs,
                len: l.len,
                outer: l.outer,
            };
            for &edge in &net.refs[l.segment_range()] {
                work.step()?;
                if refs == PIECES {
                    return Err(Error::ProviderRefused);
                }
                ordered_refs[refs] = edge;
                refs += 1;
            }
            count += 1;
        }
        net.regions[net.region_len].start = region_start;
        net.regions[net.region_len].len = count - region_start;
        net.region_len += 1;
    }
    if count != net.loop_len || refs != net.ref_len {
        return Err(Error::ProviderRefused);
    }
    net.loops = ordered_loops;
    net.refs = ordered_refs;
    Ok(())
}
fn canonical_indices(net: &mut Network<'_>, work: &mut WorkMeter<'_>) -> Result<(), Error> {
    let mut order = [0; VERTICES];
    let mut inverse = [0; VERTICES];
    for (i, slot) in order[..net.vertex_len].iter_mut().enumerate() {
        *slot = i;
    }
    for i in 1..net.vertex_len {
        let mut j = i;
        while j > 0 {
            work.step()?;
            if !P::read(net.vertices[order[j]])?
                .cmp(P::read(net.vertices[order[j - 1]])?)
                .is_lt()
            {
                break;
            }
            order.swap(j, j - 1);
            j -= 1;
        }
    }
    let mut vertices = net.vertices;
    for (i, old) in order[..net.vertex_len].iter().copied().enumerate() {
        work.step()?;
        vertices[i] = net.vertices[old];
        inverse[old] = i;
    }
    net.vertices = vertices;
    for s in &mut net.segments[..net.segment_len] {
        work.step()?;
        s.start = inverse[s.start];
        s.end = inverse[s.end];
    }
    let mut segments = net.segments;
    let mut remap = [0; PIECES];
    // Canonical segment indices follow normalized component/loop traversal.
    for (i, old) in net.refs[..net.ref_len].iter().copied().enumerate() {
        work.step()?;
        segments[i] = net.segments[old];
        remap[old] = i;
    }
    for r in &mut net.refs[..net.ref_len] {
        work.step()?;
        *r = remap[*r];
    }
    net.segments = segments;
    Ok(())
}

pub fn evaluate<'a>(
    paths: [&Path<'_>; 2],
    operation: Operation,
    bottom_index: usize,
    result_style: &'a DomainId,
    limits: &Limits,
    work: &mut WorkMeter<'_>,
) -> Result<Network<'a>, Error> {
    work.check()?;
    if bottom_index > 1 || result_style.prefix() != "SSTY" {
        return Err(Error::InvalidGeometry);
    }
    bound(2, limits.operands)?;
    bound(8, limits.anchors)?;
    bound(8, limits.segments)?;
    let p = P::read(
        paths[0]
            .anchors
            .first()
            .ok_or(Error::InvalidGeometry)?
            .position,
    )?;
    let mut edges = [Edge {
        a: p,
        b: p,
        owner: 0,
    }; EDGES];
    rect(paths[0], 0, &mut edges, work)?;
    rect(paths[1], 1, &mut edges, work)?;
    let mut cuts = [[p; CUTS]; EDGES];
    let mut lens = [0; EDGES];
    let mut intersections = 0;
    for i in 0..EDGES {
        cut_insert(&mut cuts[i], &mut lens[i], edges[i].a, work)?;
        cut_insert(&mut cuts[i], &mut lens[i], edges[i].b, work)?;
    }
    for i in 0..EDGES {
        for j in i + 1..EDGES {
            work.step()?;
            let mut points = [p; 2];
            let count = cuts_pair(edges[i], edges[j], &mut points);
            for point in &points[..count] {
                work.step()?;
                intersections += 1;
                bound(intersections, limits.intersections)?;
                work.record_intersection()?;
                cut_insert(&mut cuts[i], &mut lens[i], *point, work)?;
                cut_insert(&mut cuts[j], &mut lens[j], *point, work)?;
            }
        }
    }
    let dummy = p.point()?;
    let mut net = Network {
        vertices: [dummy; VERTICES],
        vertex_len: 0,
        segments: [Segment { start: 0, end: 0 }; PIECES],
        segment_len: 0,
        loops: [Loop {
            start: 0,
            len: 0,
            outer: false,
        }; PIECES],
        loop_len: 0,
        refs: [0; PIECES],
        ref_len: 0,
        regions: [Region {
            start: 0,
            len: 0,
            style: result_style,
        }; PIECES],
        region_len: 0,
        intersections: 0,
        sweep_events: 0,
    };
    let mut admitted_pieces = 0;
    let mut events = 0;
    for i in 0..EDGES {
        for k in 1..lens[i] {
            let mut j = k;
            while j > 0 {
                work.step()?;
                if !cuts[i][j].cmp(cuts[i][j - 1]).is_lt() {
                    break;
                }
                cuts[i].swap(j, j - 1);
                j -= 1;
            }
        }
        for k in 1..lens[i] {
            work.step()?;
            let a = cuts[i][k - 1];
            let b = cuts[i][k];
            if a == b {
                continue;
            }
            admitted_pieces += 1;
            if admitted_pieces > PIECES {
                return Err(Error::ProviderRefused);
            }
            events += 2;
            bound(events, limits.sweep_events)?;
            work.record_sweep_events(2)?;
            let (left, right) = side_windings(a, b, &edges, work)?;
            let l = selected(left, paths, operation, bottom_index);
            let r = selected(right, paths, operation, bottom_index);
            if l == r {
                continue;
            }
            let (a, b) = if l { (a, b) } else { (b, a) };
            let mut duplicate = false;
            for s in net.segments() {
                work.step()?;
                if P::read(net.vertices[s.start])? == a && P::read(net.vertices[s.end])? == b {
                    duplicate = true;
                    break;
                }
            }
            if duplicate {
                continue;
            }
            bound(net.segment_len + 1, limits.output_segments)?;
            if net.segment_len == PIECES {
                return Err(Error::ProviderRefused);
            }
            let start = vertex(&mut net, a, limits, work)?;
            let end = vertex(&mut net, b, limits, work)?;
            net.segments[net.segment_len] = Segment { start, end };
            net.segment_len += 1;
        }
    }
    if net.segment_len == 0 {
        return Err(Error::EmptyPathfinderResult);
    }
    contours(&mut net, limits, work)?;
    canonical_indices(&mut net, work)?;
    net.intersections = work.intersections();
    net.sweep_events = work.sweep_events();
    work.check()?;
    Ok(net)
}
