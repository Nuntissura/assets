//! Gradient mesh rasterisation on the CPU (lane SMB-AI-TYPE-PATH-RECOLOR; STU-VEC-034,
//! STU-COL-036). Nib owns the mesh ([`hsk_studio_nib::mesh::GradientMesh`]) and its tessellation;
//! this module scan-converts the triangles.
//!
//! Method: the mesh is tessellated with sub-quads of at most 2 device pixels; every triangle is
//! sampled at pixel centres inside its bounding box (edge functions in f64, top-left fill rule, so
//! two triangles sharing an edge never both cover a centre on it). The patch parameters `(u, v)`
//! are interpolated barycentrically and the patch colour is evaluated there (bilinear between the
//! four corner colours, exact where the patch map is affine). Each sample composites source-over
//! (straight colour with its alpha, the point transparency) into a premultiplied RGBA f32 plane, in
//! the tessellation's painting order (later patches over earlier). One sample per pixel: mesh
//! outlines are not anti-aliased (a declared gap). The frame maps document units to pixels by
//! `px = (x - x0) * px_per_unit`. Bounds (V4-01/02): frame pixels are checked before allocation;
//! sampled bounding-box pixels are charged to `max_work` before each triangle is scanned. GPU
//! parity: render-gpu has no vector or triangle pipeline (compute-only plans; vector refused), so
//! a GPU mesh path is NOT_PROVEN and not claimed. Illustrator mesh appearance is UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_nib::mesh::{GradientMesh, MeshError, MeshLimits};

/// The pixel frame a mesh is drawn into.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeshFrame {
    /// Document position of the frame's top-left corner.
    pub x0: f64,
    pub y0: f64,
    pub width: u32,
    pub height: u32,
    pub px_per_unit: f64,
}

/// Caller bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeshRasterLimits {
    pub max_pixels: u64,
    /// Bounding-box pixels sampled over all triangles.
    pub max_work: u64,
    pub mesh: MeshLimits,
}
impl Default for MeshRasterLimits {
    fn default() -> Self {
        Self { max_pixels: 1 << 26, max_work: 1 << 32, mesh: MeshLimits::default() }
    }
}

/// Typed refusals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MeshRasterError {
    Mesh(MeshError),
    InvalidInput(&'static str),
    OverLimit(&'static str),
}
impl MeshRasterError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Mesh(e) => e.code(),
            Self::InvalidInput(_) => "mesh_raster_invalid_input",
            Self::OverLimit(_) => "mesh_raster_over_limit",
        }
    }
}
impl From<MeshError> for MeshRasterError {
    fn from(e: MeshError) -> Self {
        Self::Mesh(e)
    }
}

/// A rasterised mesh: premultiplied RGBA f32, row major, top row first.
#[derive(Clone, Debug, PartialEq)]
pub struct MeshRaster {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<f32>,
}
impl MeshRaster {
    /// Premultiplied RGBA of pixel `(x, y)`.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[f32; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = (y as usize).checked_mul(self.width as usize)?.checked_add(x as usize)?.checked_mul(4)?;
        let p = self.rgba.get(i..i.checked_add(4)?)?;
        Some([p[0], p[1], p[2], p[3]])
    }
}

/// Top or left edge of a triangle with positive area in y-down device space.
fn top_left(a: [f64; 2], b: [f64; 2]) -> bool {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    (dy == 0.0 && dx > 0.0) || dy < 0.0
}

/// Rasterises `mesh` into `frame` (module docs).
pub fn rasterize_gradient_mesh(mesh: &GradientMesh, frame: &MeshFrame, limits: &MeshRasterLimits) -> Result<MeshRaster, MeshRasterError> {
    let s = frame.px_per_unit;
    if !(s.is_finite() && s > 0.0 && frame.x0.is_finite() && frame.y0.is_finite() && frame.width > 0 && frame.height > 0) {
        return Err(MeshRasterError::InvalidInput("frame"));
    }
    let pixels = u64::from(frame.width).saturating_mul(u64::from(frame.height));
    if pixels > limits.max_pixels {
        return Err(MeshRasterError::OverLimit("max_pixels"));
    }
    let len = usize::try_from(pixels).ok().and_then(|p| p.checked_mul(4)).ok_or(MeshRasterError::OverLimit("max_pixels"))?;
    let tris = mesh.tessellate(2.0 / s, &limits.mesh)?;
    let mut rgba = vec![0.0_f32; len];
    let dev = |p: [f64; 2]| [(p[0] - frame.x0) * s, (p[1] - frame.y0) * s];
    let (w, h) = (f64::from(frame.width), f64::from(frame.height));
    let mut work = 0_u64;
    for t in &tris.triangles {
        let v = [tris.vertices[t[0] as usize], tris.vertices[t[1] as usize], tris.vertices[t[2] as usize]];
        let mut p = [dev(v[0].pos), dev(v[1].pos), dev(v[2].pos)];
        let mut uv = [v[0].uv, v[1].uv, v[2].uv];
        let mut area = (p[1][0] - p[0][0]) * (p[2][1] - p[0][1]) - (p[2][0] - p[0][0]) * (p[1][1] - p[0][1]);
        if area == 0.0 || !area.is_finite() {
            continue;
        }
        if area < 0.0 {
            p.swap(1, 2);
            uv.swap(1, 2);
            area = -area;
        }
        let lo = [p[0][0].min(p[1][0]).min(p[2][0]), p[0][1].min(p[1][1]).min(p[2][1])];
        let hi = [p[0][0].max(p[1][0]).max(p[2][0]), p[0][1].max(p[1][1]).max(p[2][1])];
        // Pixel centres c + 0.5 inside [lo, hi], clamped to the frame.
        let x0 = (lo[0] - 0.5).ceil().clamp(0.0, w);
        let x1 = (hi[0] - 0.5).floor().clamp(-1.0, w - 1.0);
        let y0 = (lo[1] - 0.5).ceil().clamp(0.0, h);
        let y1 = (hi[1] - 0.5).floor().clamp(-1.0, h - 1.0);
        if x1 < x0 || y1 < y0 {
            continue;
        }
        let span = ((x1 - x0 + 1.0) * (y1 - y0 + 1.0)) as u64;
        work = work.checked_add(span).filter(|n| *n <= limits.max_work).ok_or(MeshRasterError::OverLimit("max_work"))?;
        let edges = [(p[1], p[2]), (p[2], p[0]), (p[0], p[1])];
        let bias: [bool; 3] = [top_left(p[1], p[2]), top_left(p[2], p[0]), top_left(p[0], p[1])];
        let patch = v[0].patch;
        let (xs, xe, ys, ye) = (x0 as u32, x1 as u32, y0 as u32, y1 as u32);
        for py in ys..=ye {
            let cy = f64::from(py) + 0.5;
            for px in xs..=xe {
                let cx = f64::from(px) + 0.5;
                let mut b = [0.0; 3];
                let mut inside = true;
                for (k, (a, e)) in edges.iter().enumerate() {
                    let f = (e[0] - a[0]) * (cy - a[1]) - (e[1] - a[1]) * (cx - a[0]);
                    if f < 0.0 || (f == 0.0 && !bias[k]) {
                        inside = false;
                        break;
                    }
                    b[k] = f / area;
                }
                if !inside {
                    continue;
                }
                let u = b[0] * uv[0][0] + b[1] * uv[1][0] + b[2] * uv[2][0];
                let vv = b[0] * uv[0][1] + b[1] * uv[1][1] + b[2] * uv[2][1];
                let c = tris.colour(patch, u, vv);
                let i = (py as usize).saturating_mul(frame.width as usize).saturating_add(px as usize).saturating_mul(4);
                let Some(d) = rgba.get_mut(i..i.saturating_add(4)) else { continue };
                let a = c[3];
                let keep = 1.0 - a;
                d[0] = c[0] * a + d[0] * keep;
                d[1] = c[1] * a + d[1] * keep;
                d[2] = c[2] * a + d[2] * keep;
                d[3] = a + d[3] * keep;
            }
        }
    }
    Ok(MeshRaster { width: frame.width, height: frame.height, rgba })
}
