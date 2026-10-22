//! Gradient mesh geometry (lane SMB-AI-TYPE-PATH-RECOLOR; STU-VEC-034, STU-COL-036, STU-VEC-059).
//! Nib owns the mesh geometry; render-cpu rasterises it, Folio stores it.
//!
//! Model: `rows x columns` patches over a `(rows + 1) x (columns + 1)` grid of mesh points (row
//! major, row 0 at the top), each point with a straight RGBA colour (alpha = the point's
//! transparency, STU-COL-036). Every mesh line between two neighbouring points is a cubic Bezier:
//! horizontal edges `h_handles[r * columns + c]` (point `(r, c)` to `(r, c + 1)`), vertical edges
//! `v_handles[r * (columns + 1) + c]` (point `(r, c)` to `(r + 1, c)`), each holding its two inner
//! control points. A patch is a Coons patch of its four edges (the PDF type 6 surface), or a
//! tensor-product patch when `tensor` holds its four interior control points (PDF type 7; order
//! `p11, p12, p21, p22` with `p_ij` = u-index i, v-index j). Colour is bilinear in the patch
//! parameters `(u, v)` between its four corner colours, as in PDF shading types 6 and 7.
//!
//! Creation: [`GradientMesh::grid`] (a rectangle, straight lines) and [`GradientMesh::from_path`]
//! (a closed path: four segments become the patch boundary exactly; any other closed path is cut
//! at quarter arc lengths into four sides, each fitted by one cubic through its end points and end
//! tangents with handle length chord / 3, a Studio rule), then subdivided: interior points sit on
//! the Coons surface, edge handles come from the surface derivative (a Coons isocurve is a cubic,
//! so subdivided lines reproduce the master surface's isocurves exactly). Editing: colour and
//! position of a point (its handles move with it), insert a mesh line through a patch column or
//! row (de Casteljau split of the crossed edges, the new line on the patch isocurve), remove an
//! interior mesh line (the two edges either side merge into one cubic keeping the outer handles).
//!
//! [`GradientMesh::tessellate`] turns patches into triangles in painting order (patch row major,
//! later over earlier, PDF overlap rule for patches; folds inside a patch paint in parameter
//! order), the subdivision chosen from the patch's control-polygon size so a sub-quad spans at
//! most `max_step` units (capped at 64 x 64 per patch). Bounds (V4-01/02): grid size, point count
//! and triangle count are checked against [`MeshLimits`] before allocation; non-finite geometry and
//! colours outside `0..=1` are refused. Illustrator mesh parity (creation from a shape, line
//! insertion, colour interpolation) is UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use crate::{OwnedPath, PathElement};

/// A 2-D point in document units.
pub type MeshPt = [f64; 2];

/// One mesh point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeshPoint {
    pub pos: MeshPt,
    /// Straight RGBA, `0..=1`.
    pub colour: [f32; 4],
}

/// Caller bounds, checked before allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeshLimits {
    /// Most patch rows and most patch columns.
    pub max_lines: u32,
    /// Most patches.
    pub max_patches: usize,
    /// Most triangles one tessellation produces.
    pub max_triangles: usize,
}
impl Default for MeshLimits {
    fn default() -> Self {
        Self { max_lines: 256, max_patches: 16_384, max_triangles: 1 << 24 }
    }
}

/// Typed refusals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MeshError {
    InvalidInput(&'static str),
    /// The path has no area or cannot bound a mesh.
    DegeneratePath,
    OverLimit(&'static str),
}
impl MeshError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidInput(_) => "mesh_invalid_input",
            Self::DegeneratePath => "mesh_degenerate_path",
            Self::OverLimit(_) => "mesh_over_limit",
        }
    }
}

/// A gradient mesh (module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct GradientMesh {
    pub rows: u32,
    pub columns: u32,
    pub points: Vec<MeshPoint>,
    pub h_handles: Vec<[MeshPt; 2]>,
    pub v_handles: Vec<[MeshPt; 2]>,
    /// Per patch (row major) four interior control points, or empty for Coons patches.
    pub tensor: Vec<[MeshPt; 4]>,
}

/// One tessellation vertex: position, its patch (row major) and patch parameters `(u, v)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeshVertex {
    pub pos: MeshPt,
    pub patch: u32,
    pub uv: [f64; 2],
}

/// Triangles of a tessellated mesh in painting order. A rasteriser interpolates `uv` across a
/// triangle and evaluates the patch colour there ([`MeshTriangles::colour`]), so the colour is the
/// exact bilinear patch colour wherever the patch map is affine.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshTriangles {
    pub vertices: Vec<MeshVertex>,
    pub triangles: Vec<[u32; 3]>,
    /// Corner colours `[c00, c01, c10, c11]` per patch (row major).
    pub patch_colours: Vec<[[f32; 4]; 4]>,
}
impl MeshTriangles {
    /// The colour of patch `patch` at `(u, v)` (bilinear, parameters clamped to `0..=1`).
    pub fn colour(&self, patch: u32, u: f64, v: f64) -> [f32; 4] {
        self.patch_colours.get(patch as usize).map_or([0.0; 4], |c| bilinear_c(*c, u.clamp(0.0, 1.0), v.clamp(0.0, 1.0)))
    }
}

fn lerp(a: MeshPt, b: MeshPt, t: f64) -> MeshPt {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}
fn addp(a: MeshPt, b: MeshPt) -> MeshPt {
    [a[0] + b[0], a[1] + b[1]]
}
fn subp(a: MeshPt, b: MeshPt) -> MeshPt {
    [a[0] - b[0], a[1] - b[1]]
}
fn scale(a: MeshPt, k: f64) -> MeshPt {
    [a[0] * k, a[1] * k]
}
fn dist(a: MeshPt, b: MeshPt) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}
fn finite(p: MeshPt) -> bool {
    p[0].is_finite() && p[1].is_finite()
}

/// Cubic Bezier point.
fn bez(p: [MeshPt; 4], t: f64) -> MeshPt {
    let u = 1.0 - t;
    let (b0, b1, b2, b3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    [b0 * p[0][0] + b1 * p[1][0] + b2 * p[2][0] + b3 * p[3][0], b0 * p[0][1] + b1 * p[1][1] + b2 * p[2][1] + b3 * p[3][1]]
}
/// Cubic Bezier derivative.
fn bez_d(p: [MeshPt; 4], t: f64) -> MeshPt {
    let u = 1.0 - t;
    let (a, b, c) = (subp(p[1], p[0]), subp(p[2], p[1]), subp(p[3], p[2]));
    scale(addp(addp(scale(a, u * u), scale(b, 2.0 * u * t)), scale(c, t * t)), 3.0)
}
/// de Casteljau split at `t`: (left, right).
fn split(p: [MeshPt; 4], t: f64) -> ([MeshPt; 4], [MeshPt; 4]) {
    let (a, b, c) = (lerp(p[0], p[1], t), lerp(p[1], p[2], t), lerp(p[2], p[3], t));
    let (d, e) = (lerp(a, b, t), lerp(b, c, t));
    let m = lerp(d, e, t);
    ([p[0], a, d, m], [m, e, c, p[3]])
}
fn lerp_c(a: [f32; 4], b: [f32; 4], t: f64) -> [f32; 4] {
    let t = t as f32;
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t, a[3] + (b[3] - a[3]) * t]
}
fn bilinear_c(c: [[f32; 4]; 4], u: f64, v: f64) -> [f32; 4] {
    lerp_c(lerp_c(c[0], c[1], u), lerp_c(c[2], c[3], u), v)
}
fn check_colour(c: [f32; 4]) -> Result<(), MeshError> {
    if c.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) { Ok(()) } else { Err(MeshError::InvalidInput("colour")) }
}

/// The four boundary cubics of a patch: top (P00 -> P01), bottom (P10 -> P11), left (P00 -> P10),
/// right (P01 -> P11).
#[derive(Clone, Copy, Debug)]
struct Patch {
    top: [MeshPt; 4],
    bottom: [MeshPt; 4],
    left: [MeshPt; 4],
    right: [MeshPt; 4],
    tensor: Option<[MeshPt; 4]>,
    colours: [[f32; 4]; 4],
}

impl Patch {
    fn coons(&self, u: f64, v: f64) -> MeshPt {
        let (c0, c1, d0, d1) = (bez(self.top, u), bez(self.bottom, u), bez(self.left, v), bez(self.right, v));
        let (p00, p01, p10, p11) = (self.top[0], self.top[3], self.bottom[0], self.bottom[3]);
        let ruled = addp(addp(scale(c0, 1.0 - v), scale(c1, v)), addp(scale(d0, 1.0 - u), scale(d1, u)));
        let bilinear = addp(addp(scale(p00, (1.0 - u) * (1.0 - v)), scale(p01, u * (1.0 - v))), addp(scale(p10, (1.0 - u) * v), scale(p11, u * v)));
        subp(ruled, bilinear)
    }
    /// The 4 x 4 control net `net[i][j]` (u-index i, v-index j).
    fn net(&self, inner: [MeshPt; 4]) -> [[MeshPt; 4]; 4] {
        let mut n = [[[0.0; 2]; 4]; 4];
        for (k, ((t, b), (l, r))) in self.top.iter().zip(&self.bottom).zip(self.left.iter().zip(&self.right)).enumerate() {
            n[k][0] = *t;
            n[k][3] = *b;
            n[0][k] = *l;
            n[3][k] = *r;
        }
        n[1][1] = inner[0];
        n[1][2] = inner[1];
        n[2][1] = inner[2];
        n[2][2] = inner[3];
        n
    }
    fn point(&self, u: f64, v: f64) -> MeshPt {
        match self.tensor {
            None => self.coons(u, v),
            Some(inner) => {
                let n = self.net(inner);
                let cols: Vec<MeshPt> = n.iter().map(|col| bez(*col, v)).collect();
                bez([cols[0], cols[1], cols[2], cols[3]], u)
            }
        }
    }
    fn colour(&self, u: f64, v: f64) -> [f32; 4] {
        bilinear_c(self.colours, u, v)
    }
    /// Longest control-polygon edge of the boundary (units).
    fn extent(&self) -> f64 {
        [self.top, self.bottom, self.left, self.right].iter().map(|c| dist(c[0], c[1]) + dist(c[1], c[2]) + dist(c[2], c[3])).fold(0.0, f64::max)
    }
}

impl GradientMesh {
    fn idx(&self, r: u32, c: u32) -> usize {
        (r as usize).saturating_mul((self.columns as usize).saturating_add(1)).saturating_add(c as usize)
    }
    fn h_idx(&self, r: u32, c: u32) -> usize {
        (r as usize).saturating_mul(self.columns as usize).saturating_add(c as usize)
    }
    fn v_idx(&self, r: u32, c: u32) -> usize {
        self.idx(r, c)
    }

    /// Counts the arrays need for `rows x columns` patches, checked against `limits`.
    fn sizes(rows: u32, columns: u32, limits: &MeshLimits) -> Result<(usize, usize, usize, usize), MeshError> {
        if rows == 0 || columns == 0 {
            return Err(MeshError::InvalidInput("grid"));
        }
        if rows > limits.max_lines || columns > limits.max_lines {
            return Err(MeshError::OverLimit("max_lines"));
        }
        let (r, c) = (rows as usize, columns as usize);
        let patches = r.checked_mul(c).ok_or(MeshError::OverLimit("max_patches"))?;
        if patches > limits.max_patches {
            return Err(MeshError::OverLimit("max_patches"));
        }
        let points = r.saturating_add(1).saturating_mul(c.saturating_add(1));
        let h = r.saturating_add(1).saturating_mul(c);
        let v = r.saturating_mul(c.saturating_add(1));
        Ok((patches, points, h, v))
    }

    /// Checks array sizes, finiteness and colours.
    pub fn validate(&self, limits: &MeshLimits) -> Result<(), MeshError> {
        let (patches, points, h, v) = Self::sizes(self.rows, self.columns, limits)?;
        if self.points.len() != points || self.h_handles.len() != h || self.v_handles.len() != v || !(self.tensor.is_empty() || self.tensor.len() == patches) {
            return Err(MeshError::InvalidInput("array_sizes"));
        }
        for p in &self.points {
            if !finite(p.pos) {
                return Err(MeshError::InvalidInput("point"));
            }
            check_colour(p.colour)?;
        }
        let handles = self.h_handles.iter().chain(&self.v_handles).flat_map(|h| h.iter());
        if !handles.chain(self.tensor.iter().flat_map(|t| t.iter())).all(|p| finite(*p)) {
            return Err(MeshError::InvalidInput("handle"));
        }
        Ok(())
    }

    /// A `rows x columns` mesh over the rectangle `[x0, x1] x [y0, y1]` (straight lines), every
    /// point `colour`.
    pub fn grid(rect: [f64; 4], rows: u32, columns: u32, colour: [f32; 4], limits: &MeshLimits) -> Result<Self, MeshError> {
        let [x0, y0, x1, y1] = rect;
        if !(rect.iter().all(|v| v.is_finite()) && x1 > x0 && y1 > y0) {
            return Err(MeshError::DegeneratePath);
        }
        let side = |a: MeshPt, b: MeshPt| [a, lerp(a, b, 1.0 / 3.0), lerp(a, b, 2.0 / 3.0), b];
        let master = Patch {
            top: side([x0, y0], [x1, y0]),
            bottom: side([x0, y1], [x1, y1]),
            left: side([x0, y0], [x0, y1]),
            right: side([x1, y0], [x1, y1]),
            tensor: None,
            colours: [colour; 4],
        };
        Self::subdivide(&master, rows, columns, colour, limits)
    }

    /// A mesh conforming to the closed first subpath of `path` (module docs).
    pub fn from_path(path: &OwnedPath, rows: u32, columns: u32, colour: [f32; 4], limits: &MeshLimits) -> Result<Self, MeshError> {
        Self::sizes(rows, columns, limits)?;
        check_colour(colour)?;
        // Cubic segments of the first subpath (lines and quads raised).
        let mut segs: Vec<[MeshPt; 4]> = Vec::new();
        let (mut start, mut cur): (Option<MeshPt>, MeshPt) = (None, [0.0; 2]);
        for e in &path.elements {
            let line = |a: MeshPt, b: MeshPt| [a, lerp(a, b, 1.0 / 3.0), lerp(a, b, 2.0 / 3.0), b];
            match *e {
                PathElement::MoveTo(p) => {
                    if start.is_some() {
                        break;
                    }
                    cur = [p.x, p.y];
                    start = Some(cur);
                }
                PathElement::LineTo(p) => {
                    segs.push(line(cur, [p.x, p.y]));
                    cur = [p.x, p.y];
                }
                PathElement::QuadTo(q, p) => {
                    let (q, p) = ([q.x, q.y], [p.x, p.y]);
                    segs.push([cur, lerp(cur, q, 2.0 / 3.0), lerp(p, q, 2.0 / 3.0), p]);
                    cur = p;
                }
                PathElement::CubicTo(a, b, p) => {
                    segs.push([cur, [a.x, a.y], [b.x, b.y], [p.x, p.y]]);
                    cur = [p.x, p.y];
                }
                PathElement::Close => {
                    if let Some(s) = start
                        && dist(s, cur) > 1e-12
                    {
                        segs.push(line(cur, s));
                    }
                    break;
                }
            }
        }
        let Some(first) = start else { return Err(MeshError::DegeneratePath) };
        if dist(first, cur) > 1e-12 && segs.last().is_some_and(|l| dist(l[3], first) > 1e-12) {
            segs.push([cur, lerp(cur, first, 1.0 / 3.0), lerp(cur, first, 2.0 / 3.0), first]);
        }
        segs.retain(|s| s.iter().any(|p| dist(*p, s[0]) > 1e-12));
        if segs.iter().any(|s| !s.iter().all(|p| finite(*p))) {
            return Err(MeshError::InvalidInput("path"));
        }
        // Shoelace over the control polygon: no area, no mesh.
        let area: f64 = segs.iter().flat_map(|s| [(s[0], s[1]), (s[1], s[2]), (s[2], s[3])]).map(|(a, b)| a[0] * b[1] - b[0] * a[1]).sum::<f64>() / 2.0;
        if segs.len() < 2 || area.is_nan() || area.abs() <= 1e-9 {
            return Err(MeshError::DegeneratePath);
        }
        let sides: [[MeshPt; 4]; 4] = if segs.len() == 4 {
            [segs[0], segs[1], segs[2], segs[3]]
        } else {
            quarter_fit(&segs)?
        };
        // Sides run around the path: 0 = top (P00 -> P01), 1 = right (P01 -> P11),
        // 2 = bottom reversed (P11 -> P10), 3 = left reversed (P10 -> P00).
        let rev = |s: [MeshPt; 4]| [s[3], s[2], s[1], s[0]];
        let master = Patch { top: sides[0], right: sides[1], bottom: rev(sides[2]), left: rev(sides[3]), tensor: None, colours: [colour; 4] };
        Self::subdivide(&master, rows, columns, colour, limits)
    }

    /// Partial derivatives of a Coons patch at `(u, v)`.
    fn coons_du(p: &Patch, u: f64, v: f64) -> MeshPt {
        let (c0, c1) = (bez_d(p.top, u), bez_d(p.bottom, u));
        let (d0, d1) = (bez(p.left, v), bez(p.right, v));
        let (p00, p01, p10, p11) = (p.top[0], p.top[3], p.bottom[0], p.bottom[3]);
        let ruled = addp(addp(scale(c0, 1.0 - v), scale(c1, v)), subp(d1, d0));
        let bil = addp(scale(subp(p01, p00), 1.0 - v), scale(subp(p11, p10), v));
        subp(ruled, bil)
    }
    fn coons_dv(p: &Patch, u: f64, v: f64) -> MeshPt {
        let (c0, c1) = (bez(p.top, u), bez(p.bottom, u));
        let (d0, d1) = (bez_d(p.left, v), bez_d(p.right, v));
        let (p00, p01, p10, p11) = (p.top[0], p.top[3], p.bottom[0], p.bottom[3]);
        let ruled = addp(subp(c1, c0), addp(scale(d0, 1.0 - u), scale(d1, u)));
        let bil = addp(scale(subp(p10, p00), 1.0 - u), scale(subp(p11, p01), u));
        subp(ruled, bil)
    }

    /// Subdivides a master Coons patch into `rows x columns` (module docs).
    fn subdivide(master: &Patch, rows: u32, columns: u32, colour: [f32; 4], limits: &MeshLimits) -> Result<Self, MeshError> {
        let (_, points, h, v) = Self::sizes(rows, columns, limits)?;
        let mut mesh = Self { rows, columns, points: Vec::with_capacity(points), h_handles: Vec::with_capacity(h), v_handles: Vec::with_capacity(v), tensor: Vec::new() };
        let (fr, fc) = (f64::from(rows), f64::from(columns));
        for r in 0..=rows {
            for c in 0..=columns {
                mesh.points.push(MeshPoint { pos: master.coons(f64::from(c) / fc, f64::from(r) / fr), colour });
            }
        }
        let (du, dv) = (1.0 / fc, 1.0 / fr);
        for r in 0..=rows {
            for c in 0..columns {
                let (u0, u1, vv) = (f64::from(c) / fc, (f64::from(c) + 1.0) / fc, f64::from(r) / fr);
                let a = addp(master.coons(u0, vv), scale(Self::coons_du(master, u0, vv), du / 3.0));
                let b = subp(master.coons(u1, vv), scale(Self::coons_du(master, u1, vv), du / 3.0));
                mesh.h_handles.push([a, b]);
            }
        }
        for r in 0..rows {
            for c in 0..=columns {
                let (v0, v1, uu) = (f64::from(r) / fr, (f64::from(r) + 1.0) / fr, f64::from(c) / fc);
                let a = addp(master.coons(uu, v0), scale(Self::coons_dv(master, uu, v0), dv / 3.0));
                let b = subp(master.coons(uu, v1), scale(Self::coons_dv(master, uu, v1), dv / 3.0));
                mesh.v_handles.push([a, b]);
            }
        }
        mesh.validate(limits)?;
        Ok(mesh)
    }

    fn patch(&self, r: u32, c: u32) -> Patch {
        let p = |r: u32, c: u32| self.points[self.idx(r, c)];
        let (p00, p01, p10, p11) = (p(r, c), p(r, c.saturating_add(1)), p(r.saturating_add(1), c), p(r.saturating_add(1), c.saturating_add(1)));
        let h = |r: u32| self.h_handles[self.h_idx(r, c)];
        let v = |c: u32| self.v_handles[self.v_idx(r, c)];
        let (ht, hb, vl, vr) = (h(r), h(r.saturating_add(1)), v(c), v(c.saturating_add(1)));
        let pi = (r as usize).saturating_mul(self.columns as usize).saturating_add(c as usize);
        Patch {
            top: [p00.pos, ht[0], ht[1], p01.pos],
            bottom: [p10.pos, hb[0], hb[1], p11.pos],
            left: [p00.pos, vl[0], vl[1], p10.pos],
            right: [p01.pos, vr[0], vr[1], p11.pos],
            tensor: self.tensor.get(pi).copied(),
            colours: [p00.colour, p01.colour, p10.colour, p11.colour],
        }
    }

    /// Point and colour of patch `(r, c)` at `(u, v)`.
    pub fn evaluate(&self, r: u32, c: u32, u: f64, v: f64) -> Option<(MeshPt, [f32; 4])> {
        if r >= self.rows || c >= self.columns || !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
            return None;
        }
        let p = self.patch(r, c);
        Some((p.point(u, v), p.colour(u, v)))
    }

    /// The mesh point `(r, c)`.
    pub fn point(&self, r: u32, c: u32) -> Option<&MeshPoint> {
        (r <= self.rows && c <= self.columns).then(|| &self.points[self.idx(r, c)])
    }

    /// Sets the colour of point `(r, c)`.
    pub fn set_colour(&mut self, r: u32, c: u32, colour: [f32; 4]) -> Result<(), MeshError> {
        check_colour(colour)?;
        if r > self.rows || c > self.columns {
            return Err(MeshError::InvalidInput("point_index"));
        }
        let i = self.idx(r, c);
        self.points[i].colour = colour;
        Ok(())
    }

    /// Moves point `(r, c)` by `d`; the handles of its mesh lines next to it move with it.
    pub fn move_point(&mut self, r: u32, c: u32, d: MeshPt) -> Result<(), MeshError> {
        if !finite(d) || r > self.rows || c > self.columns {
            return Err(MeshError::InvalidInput("move"));
        }
        let i = self.idx(r, c);
        self.points[i].pos = addp(self.points[i].pos, d);
        if c < self.columns {
            let k = self.h_idx(r, c);
            self.h_handles[k][0] = addp(self.h_handles[k][0], d);
        }
        if c > 0 {
            let k = self.h_idx(r, c.saturating_sub(1));
            self.h_handles[k][1] = addp(self.h_handles[k][1], d);
        }
        if r < self.rows {
            let k = self.v_idx(r, c);
            self.v_handles[k][0] = addp(self.v_handles[k][0], d);
        }
        if r > 0 {
            let k = self.v_idx(r.saturating_sub(1), c);
            self.v_handles[k][1] = addp(self.v_handles[k][1], d);
        }
        if !finite(self.points[i].pos) {
            return Err(MeshError::InvalidInput("move"));
        }
        Ok(())
    }

    /// The isocurve `u = f` of patch `(r, c)` as a cubic from its top to its bottom edge (exact for
    /// Coons patches; the tensor control net's column interpolation for tensor patches).
    fn iso_u(&self, r: u32, c: u32, f: f64) -> [MeshPt; 4] {
        let p = self.patch(r, c);
        match p.tensor {
            Some(inner) => {
                let n = p.net(inner);
                let col = |j: usize| bez([n[0][j], n[1][j], n[2][j], n[3][j]], f);
                [col(0), col(1), col(2), col(3)]
            }
            None => {
                let (a, b) = (subp(bez(p.top, f), lerp(p.top[0], p.top[3], f)), subp(bez(p.bottom, f), lerp(p.bottom[0], p.bottom[3], f)));
                let lin = [a, lerp(a, b, 1.0 / 3.0), lerp(a, b, 2.0 / 3.0), b];
                let mut q = [[0.0; 2]; 4];
                for (k, item) in q.iter_mut().enumerate() {
                    *item = addp(lerp(p.left[k], p.right[k], f), lin[k]);
                }
                q
            }
        }
    }

    /// Inserts a vertical mesh line through column `c` at fraction `f` (`0 < f < 1`): new points
    /// on every horizontal edge of that column (colours interpolated), the new line on the patch
    /// isocurves. Tensor interior points of the split column are dropped (those patches become
    /// Coons patches).
    pub fn insert_column(&mut self, c: u32, f: f64, limits: &MeshLimits) -> Result<(), MeshError> {
        if c >= self.columns || !(f > 0.0 && f < 1.0) {
            return Err(MeshError::InvalidInput("insert_column"));
        }
        let columns = self.columns.checked_add(1).ok_or(MeshError::OverLimit("max_lines"))?;
        let (patches, points, h, v) = Self::sizes(self.rows, columns, limits)?;
        let mut out = Self { rows: self.rows, columns, points: Vec::with_capacity(points), h_handles: Vec::with_capacity(h), v_handles: Vec::with_capacity(v), tensor: Vec::new() };
        let isos: Vec<[MeshPt; 4]> = (0..self.rows).map(|r| self.iso_u(r, c, f)).collect();
        for r in 0..=self.rows {
            for k in 0..=self.columns {
                out.points.push(self.points[self.idx(r, k)]);
                if k == c {
                    let a = self.points[self.idx(r, k)];
                    let b = self.points[self.idx(r, k.saturating_add(1))];
                    let hh = self.h_handles[self.h_idx(r, k)];
                    out.points.push(MeshPoint { pos: bez([a.pos, hh[0], hh[1], b.pos], f), colour: lerp_c(a.colour, b.colour, f) });
                }
            }
            for k in 0..self.columns {
                let hh = self.h_handles[self.h_idx(r, k)];
                if k == c {
                    let (a, b) = (self.points[self.idx(r, k)].pos, self.points[self.idx(r, k.saturating_add(1))].pos);
                    let (left, right) = split([a, hh[0], hh[1], b], f);
                    out.h_handles.push([left[1], left[2]]);
                    out.h_handles.push([right[1], right[2]]);
                } else {
                    out.h_handles.push(hh);
                }
            }
        }
        for r in 0..self.rows {
            for k in 0..=self.columns {
                out.v_handles.push(self.v_handles[self.v_idx(r, k)]);
                if k == c {
                    let iso = isos[r as usize];
                    out.v_handles.push([iso[1], iso[2]]);
                }
            }
        }
        if !self.tensor.is_empty() {
            out.tensor = Vec::with_capacity(patches);
            // Split patches become Coons: use their Coons-equivalent interior points.
            for r in 0..self.rows {
                for k in 0..self.columns {
                    let t = self.tensor[(r as usize).saturating_mul(self.columns as usize).saturating_add(k as usize)];
                    if k == c {
                        out.tensor.push([[f64::NAN; 2]; 4]);
                        out.tensor.push([[f64::NAN; 2]; 4]);
                    } else {
                        out.tensor.push(t);
                    }
                }
            }
            out.fill_coons_interiors();
        }
        out.validate(limits)?;
        *self = out;
        Ok(())
    }

    /// Replaces NaN tensor entries by the Coons-equivalent interior points (PDF 32000-1 8.7.4.5.8).
    fn fill_coons_interiors(&mut self) {
        for r in 0..self.rows {
            for c in 0..self.columns {
                let pi = (r as usize).saturating_mul(self.columns as usize).saturating_add(c as usize);
                if self.tensor[pi].iter().all(|p| finite(*p)) {
                    continue;
                }
                let mut p = self.patch(r, c);
                p.tensor = Some([[0.0; 2]; 4]);
                let n = p.net([[0.0; 2]; 4]);
                let k = |terms: &[(f64, MeshPt)]| scale(terms.iter().fold([0.0, 0.0], |acc, (w, q)| addp(acc, scale(*q, *w))), 1.0 / 9.0);
                let p11 = k(&[(-4.0, n[0][0]), (6.0, n[0][1]), (6.0, n[1][0]), (-2.0, n[0][3]), (-2.0, n[3][0]), (3.0, n[3][1]), (3.0, n[1][3]), (-1.0, n[3][3])]);
                let p12 = k(&[(-4.0, n[0][3]), (6.0, n[0][2]), (6.0, n[1][3]), (-2.0, n[0][0]), (-2.0, n[3][3]), (3.0, n[3][2]), (3.0, n[1][0]), (-1.0, n[3][0])]);
                let p21 = k(&[(-4.0, n[3][0]), (6.0, n[3][1]), (6.0, n[2][0]), (-2.0, n[3][3]), (-2.0, n[0][0]), (3.0, n[0][1]), (3.0, n[2][3]), (-1.0, n[0][3])]);
                let p22 = k(&[(-4.0, n[3][3]), (6.0, n[3][2]), (6.0, n[2][3]), (-2.0, n[3][0]), (-2.0, n[0][3]), (3.0, n[0][2]), (3.0, n[2][0]), (-1.0, n[0][0])]);
                self.tensor[pi] = [p11, p12, p21, p22];
            }
        }
    }

    /// Transposed copy (rows <-> columns), so row operations reuse the column ones.
    fn transposed(&self) -> Self {
        let (rows, columns) = (self.columns, self.rows);
        let mut t = Self { rows, columns, points: Vec::with_capacity(self.points.len()), h_handles: Vec::with_capacity(self.v_handles.len()), v_handles: Vec::with_capacity(self.h_handles.len()), tensor: Vec::new() };
        for r in 0..=rows {
            for c in 0..=columns {
                t.points.push(self.points[self.idx(c, r)]);
            }
        }
        for r in 0..=rows {
            for c in 0..columns {
                t.h_handles.push(self.v_handles[self.v_idx(c, r)]);
            }
        }
        for r in 0..rows {
            for c in 0..=columns {
                t.v_handles.push(self.h_handles[self.h_idx(c, r)]);
            }
        }
        if !self.tensor.is_empty() {
            for r in 0..rows {
                for c in 0..columns {
                    let [a, b, cc, d] = self.tensor[(c as usize).saturating_mul(self.columns as usize).saturating_add(r as usize)];
                    t.tensor.push([a, cc, b, d]);
                }
            }
        }
        t
    }

    /// Inserts a horizontal mesh line through row `r` at fraction `f` (see [`Self::insert_column`]).
    pub fn insert_row(&mut self, r: u32, f: f64, limits: &MeshLimits) -> Result<(), MeshError> {
        let mut t = self.transposed();
        t.insert_column(r, f, limits)?;
        *self = t.transposed();
        Ok(())
    }

    /// Removes the interior vertical mesh line `c` (`1..columns`): the two edges either side of each
    /// removed point merge into one cubic keeping their outer handles; the merged patches become
    /// Coons patches.
    pub fn remove_column(&mut self, c: u32, limits: &MeshLimits) -> Result<(), MeshError> {
        if c == 0 || c >= self.columns {
            return Err(MeshError::InvalidInput("remove_column"));
        }
        let columns = self.columns.saturating_sub(1);
        let (_, points, h, v) = Self::sizes(self.rows, columns, limits)?;
        let mut out = Self { rows: self.rows, columns, points: Vec::with_capacity(points), h_handles: Vec::with_capacity(h), v_handles: Vec::with_capacity(v), tensor: Vec::new() };
        for r in 0..=self.rows {
            for k in 0..=self.columns {
                if k != c {
                    out.points.push(self.points[self.idx(r, k)]);
                }
            }
            for k in 0..self.columns {
                if k == c.saturating_sub(1) {
                    let (a, b) = (self.h_handles[self.h_idx(r, k)], self.h_handles[self.h_idx(r, c)]);
                    // Outer handles kept, lengths doubled toward the merged span (a Studio rule).
                    let (p0, p3) = (self.points[self.idx(r, k)].pos, self.points[self.idx(r, c.saturating_add(1))].pos);
                    out.h_handles.push([addp(p0, scale(subp(a[0], p0), 2.0)), addp(p3, scale(subp(b[1], p3), 2.0))]);
                } else if k != c {
                    out.h_handles.push(self.h_handles[self.h_idx(r, k)]);
                }
            }
        }
        for r in 0..self.rows {
            for k in 0..=self.columns {
                if k != c {
                    out.v_handles.push(self.v_handles[self.v_idx(r, k)]);
                }
            }
        }
        if !self.tensor.is_empty() {
            for r in 0..self.rows {
                for k in 0..self.columns {
                    if k == c {
                        continue;
                    }
                    let t = self.tensor[(r as usize).saturating_mul(self.columns as usize).saturating_add(k as usize)];
                    out.tensor.push(if k == c.saturating_sub(1) { [[f64::NAN; 2]; 4] } else { t });
                }
            }
            out.fill_coons_interiors();
        }
        out.validate(limits)?;
        *self = out;
        Ok(())
    }

    /// Removes the interior horizontal mesh line `r` (`1..rows`).
    pub fn remove_row(&mut self, r: u32, limits: &MeshLimits) -> Result<(), MeshError> {
        let mut t = self.transposed();
        t.remove_column(r, limits)?;
        *self = t.transposed();
        Ok(())
    }

    /// Triangles in painting order (module docs); `max_step` = largest sub-quad side in units.
    pub fn tessellate(&self, max_step: f64, limits: &MeshLimits) -> Result<MeshTriangles, MeshError> {
        self.validate(limits)?;
        if !(max_step.is_finite() && max_step > 0.0) {
            return Err(MeshError::InvalidInput("max_step"));
        }
        // Subdivision per patch, then the total, all before allocating.
        let mut steps = Vec::with_capacity((self.rows as usize).saturating_mul(self.columns as usize));
        let mut tris = 0_usize;
        let mut verts = 0_usize;
        for r in 0..self.rows {
            for c in 0..self.columns {
                let n = (self.patch(r, c).extent() / max_step).ceil().clamp(1.0, 64.0) as usize;
                steps.push(n);
                tris = n.checked_mul(n).and_then(|q| q.checked_mul(2)).and_then(|q| tris.checked_add(q)).ok_or(MeshError::OverLimit("max_triangles"))?;
                verts = n.saturating_add(1).checked_mul(n.saturating_add(1)).and_then(|q| verts.checked_add(q)).ok_or(MeshError::OverLimit("max_triangles"))?;
            }
        }
        if tris > limits.max_triangles || u32::try_from(verts).is_err() {
            return Err(MeshError::OverLimit("max_triangles"));
        }
        let patches = (self.rows as usize).saturating_mul(self.columns as usize);
        let mut out = MeshTriangles { vertices: Vec::with_capacity(verts), triangles: Vec::with_capacity(tris), patch_colours: Vec::with_capacity(patches) };
        let mut k = 0_usize;
        for r in 0..self.rows {
            for c in 0..self.columns {
                let n = steps[k];
                k = k.saturating_add(1);
                let p = self.patch(r, c);
                let patch = u32::try_from(out.patch_colours.len()).map_err(|_| MeshError::OverLimit("max_patches"))?;
                out.patch_colours.push(p.colours);
                let base = u32::try_from(out.vertices.len()).map_err(|_| MeshError::OverLimit("max_triangles"))?;
                let nf = n as f64;
                for j in 0..=n {
                    for i in 0..=n {
                        let (u, v) = (i as f64 / nf, j as f64 / nf);
                        out.vertices.push(MeshVertex { pos: p.point(u, v), patch, uv: [u, v] });
                    }
                }
                let w = u32::try_from(n.saturating_add(1)).map_err(|_| MeshError::OverLimit("max_triangles"))?;
                let n32 = w.saturating_sub(1);
                for j in 0..n32 {
                    for i in 0..n32 {
                        let a = base.saturating_add(j.saturating_mul(w)).saturating_add(i);
                        let (b, cc, d) = (a.saturating_add(1), a.saturating_add(w), a.saturating_add(w).saturating_add(1));
                        out.triangles.push([a, b, d]);
                        out.triangles.push([a, d, cc]);
                    }
                }
            }
        }
        Ok(out)
    }
}

/// Four cubic sides of a closed path cut at quarter arc lengths (module docs).
fn quarter_fit(segs: &[[MeshPt; 4]]) -> Result<[[MeshPt; 4]; 4], MeshError> {
    // Arc length by 16-step polyline per segment (ordering only; the fit uses exact points).
    const STEPS: usize = 16;
    let mut samples: Vec<(usize, f64, f64)> = Vec::with_capacity(segs.len().saturating_mul(STEPS).saturating_add(1));
    let mut acc = 0.0;
    samples.push((0, 0.0, 0.0));
    for (i, s) in segs.iter().enumerate() {
        let mut prev = s[0];
        for k in 1..=STEPS {
            let t = k as f64 / STEPS as f64;
            let p = bez(*s, t);
            acc += dist(prev, p);
            prev = p;
            samples.push((i, t, acc));
        }
    }
    if acc.is_nan() || acc <= 1e-9 {
        return Err(MeshError::DegeneratePath);
    }
    // Point and tangent at arc position `target`, interpolated between the bracketing samples;
    // `incoming` takes the segment arriving at a joint (the tangent before it), else the one
    // leaving it.
    let eps = acc * 1e-12;
    let at = |target: f64, incoming: bool| -> (MeshPt, MeshPt) {
        let k = if incoming { samples.partition_point(|s| s.2 < target - eps) } else { samples.partition_point(|s| s.2 <= target + eps) };
        let k = k.clamp(1, samples.len().saturating_sub(1));
        let (prev, cur) = (samples[k.saturating_sub(1)], samples[k]);
        let t0 = if prev.0 == cur.0 { prev.1 } else { 0.0 };
        let span = cur.2 - prev.2;
        let frac = if span > 0.0 { ((target - prev.2) / span).clamp(0.0, 1.0) } else { 0.0 };
        let (i, t) = (cur.0, t0 + frac * (cur.1 - t0));
        let s = segs[i];
        let d = bez_d(s, t);
        let d = if (d[0].hypot(d[1])) > 1e-12 { d } else { subp(s[3], s[0]) };
        (bez(s, t), d)
    };
    let starts: Vec<(MeshPt, MeshPt)> = (0..4).map(|q| at(acc * q as f64 / 4.0, false)).collect();
    let mut sides = [[[0.0; 2]; 4]; 4];
    for (q, side) in sides.iter_mut().enumerate() {
        let (a, da) = starts[q];
        let (b, db) = at(acc * (q as f64 + 1.0) / 4.0, true);
        // The last side closes on the first point exactly.
        let b = if q == 3 { starts[0].0 } else { b };
        let chord = dist(a, b) / 3.0;
        let unit = |d: MeshPt| {
            let n = d[0].hypot(d[1]);
            if n > 1e-12 { scale(d, 1.0 / n) } else { [0.0, 0.0] }
        };
        *side = [a, addp(a, scale(unit(da), chord)), subp(b, scale(unit(db), chord)), b];
    }
    Ok(sides)
}
