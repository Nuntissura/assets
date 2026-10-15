//! 3D compositing on the classic planar model (lane SMB-AE-3D; spec 14.27.3).
//!
//! Spec basis: STU-CMP-035 (`is_3d` promotes the transform: Z position, X and Y rotation, an
//! orientation triple, material options; a 3D layer is a flat PLANE positioned in 3D space unless
//! a geometry option extrudes it), STU-CMP-035a (contiguous runs of 3D layers are sorted by depth
//! and rendered together; a 2D layer, an adjustment layer or a blend mode needing an intermediate
//! raster breaks the run; layers of one run may intersect), STU-CMP-040 / 040a (camera: zoom in
//! pixels, angle of view the same value; one-node or two-node by the point of interest),
//! STU-MOT-013 (3D transform members), STU-CMP-060 (a renderer declares a capability set; a
//! property it does not honour is reported inactive with a reason, [`PLANAR_RENDERER`]).
//!
//! Geometry (Studio readings; After Effects' conventions are UNVERIFIED, D5). World space is
//! composition pixels with x right, y down and z away from the viewer. A 3D layer maps its layer
//! space point `p = (x, y, 0)` to `world = position + R S (p - anchor)`, `S = diag(sx, sy, 1)`,
//! `R = Rz(rz) Ry(ry) Rx(rx) Rz(oz) Ry(oy) Rx(ox)` (orientation first, then the X, Y and Z
//! rotations; degrees, quarter turns exact). The camera has position `C` and a rotation `Rc`
//! (camera to world; columns screen-right, screen-down, forward): a one-node camera uses its own
//! rotation, a two-node camera looks at its point of interest (forward towards it, screen-down
//! the world-down direction made orthogonal) and then applies its own rotation. Camera point
//! `q = Rc^T (world - C)`; the screen point is `centre + zoom (q.x / q.z, q.y / q.z)` and the depth
//! is `q.z`. Without a camera layer the composition uses [`default_camera`] (z = 0 maps 1:1). A
//! plane therefore maps to the screen by one homography `H` ([`plane_homography`]), resampled by
//! pigment's projective transform on both renderers (the corner-pin machinery).
//!
//! Depth sorting and intersection ([`expand_run`]): per screen pixel centre, every plane of the run
//! whose content rectangle (grown by one pixel for the filter footprint) the camera ray hits in
//! front of the camera gets its exact depth (`1 / w` of the inverse homography); the planes are
//! ranked farthest first (equal depth: stack order, bottom first). The run is emitted as one 2D
//! layer per (rank, plane) pair that occurs, matted by the hard 0/1 mask of the pixels where that
//! plane has that rank, ranks in order. Pieces of one rank are disjoint, so every pixel composites
//! its planes in its own depth order: intersecting planes cut each other along the intersection
//! line (per pixel, without anti-aliasing of that line). A plane that crosses the camera plane is
//! refused ([`COMPOSITION_3D_CROSSES_CAMERA`]); one wholly behind the camera renders nothing.
#![deny(clippy::arithmetic_side_effects)]

use crate::CompositeError;
use crate::composition::{PinnedContent, SceneContent, SceneLayer};
use crate::temporal::TemporalError;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::StudioBlendMode;
use hsk_studio_pigment::render_plan::ColourTag;
use hsk_studio_pigment::transform_math::{Affine2, Projective2, exact_sin_cos};

pub type Vec3 = [f64; 3];
/// Row-major 3x3.
pub type Mat3 = [[f64; 3]; 3];

/// A plane that crosses the camera plane (part in front, part behind).
pub const COMPOSITION_3D_CROSSES_CAMERA: &str = "composition_3d_plane_crosses_camera";
/// A two-node camera whose forward direction is parallel to world-down (no screen orientation).
pub const COMPOSITION_3D_CAMERA_DEGENERATE: &str = "composition_3d_camera_degenerate";
/// A 3D layer with content the planar model cannot place (none, adjustment, pinned).
pub const COMPOSITION_3D_CONTENT: &str = "composition_3d_content";
/// A parent on a 3D layer, or a 3D parent of a 2D layer (slice 1 of the planar renderer).
pub const COMPOSITION_3D_PARENT: &str = "composition_3d_parent";
/// A track matte, mask path, corner pin or clipping on a 3D layer (slice 1 of the planar renderer).
pub const COMPOSITION_3D_LAYER_OPTION: &str = "composition_3d_layer_option";
/// More planes in one run than [`MAX_RUN_PLANES`].
pub const COMPOSITION_3D_RUN_LIMIT: &str = "composition_3d_run_limit";
/// Most planes one 3D run may hold (the run emits up to planes x ranks layers).
pub const MAX_RUN_PLANES: usize = 32;
/// Smallest camera depth a visible plane point may have.
pub const NEAR_DEPTH: f64 = 1e-3;

/// The planar renderer's capability set (STU-CMP-060): what it honours, and every recognised 3D
/// property it does NOT honour with the stated reason (shown inactive, never hidden).
pub struct RendererCapabilities {
    pub name: &'static str,
    pub extrusion: bool,
    pub bevels: bool,
    pub environment_lighting: bool,
    pub reflections: bool,
    pub refraction: bool,
    pub ray_traced_shadows: bool,
    /// `(property, reason)` for every inactive property.
    pub inactive: &'static [(&'static str, &'static str)],
}
/// The classic planar renderer (STU-CMP-060 item 6: "layers can be positioned as planes in 3D
/// space").
pub const PLANAR_RENDERER: RendererCapabilities = RendererCapabilities {
    name: "planar",
    extrusion: false,
    bevels: false,
    environment_lighting: false,
    reflections: false,
    refraction: false,
    ray_traced_shadows: false,
    inactive: &[
        ("extrusion_depth", "planar renderer: layers are flat planes, no extruded geometry"),
        ("bevel_depth", "planar renderer: no bevels"),
        ("bevel_style", "planar renderer: no bevels"),
        ("bevel_direction", "planar renderer: no bevels"),
        ("plane_curvature", "planar renderer: planes are not curved"),
        ("plane_subdivision", "planar renderer: planes are not subdivided"),
        ("appears_in_reflections", "planar renderer: no reflections"),
        ("reflection", "planar renderer: no reflections"),
        ("glossiness", "planar renderer: no reflections"),
        ("fresnel", "planar renderer: no reflections"),
        ("index_of_refraction", "planar renderer: no refraction"),
        ("transparency", "planar renderer: no refraction"),
        ("transparency_rolloff", "planar renderer: no refraction"),
        ("environment_atom", "planar renderer: no environment lighting"),
        ("background_visible", "planar renderer: no environment lighting"),
        ("background_opacity", "planar renderer: no environment lighting"),
        ("background_blur", "planar renderer: no environment lighting"),
    ],
};

pub fn mat_mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut m = [[0.0; 3]; 3];
    for (r, row) in m.iter_mut().enumerate() {
        for (c, v) in row.iter_mut().enumerate() {
            *v = a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c];
        }
    }
    m
}
pub fn mat_vec(a: &Mat3, v: Vec3) -> Vec3 {
    [
        a[0][0] * v[0] + a[0][1] * v[1] + a[0][2] * v[2],
        a[1][0] * v[0] + a[1][1] * v[1] + a[1][2] * v[2],
        a[2][0] * v[0] + a[2][1] * v[1] + a[2][2] * v[2],
    ]
}
pub fn transpose(a: &Mat3) -> Mat3 {
    [[a[0][0], a[1][0], a[2][0]], [a[0][1], a[1][1], a[2][1]], [a[0][2], a[1][2], a[2][2]]]
}
fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn dot(a: Vec3, b: Vec3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn normalize(a: Vec3) -> Option<Vec3> {
    let l = dot(a, a).sqrt();
    (l.is_finite() && l > 1e-12).then(|| [a[0] / l, a[1] / l, a[2] / l])
}

/// Rotation about X by `deg` (y down, z away: positive turns +y towards +z).
pub fn rot_x(deg: f64) -> Mat3 {
    let (s, c) = exact_sin_cos(deg);
    [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]]
}
/// Rotation about Y by `deg` (positive turns +z towards +x).
pub fn rot_y(deg: f64) -> Mat3 {
    let (s, c) = exact_sin_cos(deg);
    [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]]
}
/// Rotation about Z by `deg` (positive = clockwise on screen, as the 2D rotation).
pub fn rot_z(deg: f64) -> Mat3 {
    let (s, c) = exact_sin_cos(deg);
    [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]]
}
/// `Rz(rz) Ry(ry) Rx(rx) Rz(oz) Ry(oy) Rx(ox)` (module docs).
pub fn layer_rotation(orientation: Vec3, rx: f64, ry: f64, rz: f64) -> Mat3 {
    let o = mat_mul(&rot_z(orientation[2]), &mat_mul(&rot_y(orientation[1]), &rot_x(orientation[0])));
    mat_mul(&rot_z(rz), &mat_mul(&rot_y(ry), &mat_mul(&rot_x(rx), &o)))
}

/// The 3D transform of one layer at a tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform3d {
    pub anchor: Vec3,
    pub position: Vec3,
    /// X and Y scale factors (1 = unscaled); a plane has no Z extent.
    pub scale: [f64; 2],
    pub rotation: Mat3,
}
impl Transform3d {
    /// World point of layer point `(x, y, 0)`.
    pub fn world(&self, x: f64, y: f64) -> Vec3 {
        let local = [self.scale[0] * (x - self.anchor[0]), self.scale[1] * (y - self.anchor[1]), -self.anchor[2]];
        let r = mat_vec(&self.rotation, local);
        [self.position[0] + r[0], self.position[1] + r[1], self.position[2] + r[2]]
    }
}

/// A camera at a tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub position: Vec3,
    /// Camera to world (columns: screen right, screen down, forward).
    pub rotation: Mat3,
    /// Focal distance in pixels (STU-CMP-040 `zoom`).
    pub zoom: f64,
    /// The composition centre in screen pixels.
    pub centre: [f64; 2],
}
/// Studio's default camera for a `width x height` composition: zoom for a 39.5978 degree horizontal
/// angle of view (a 50 mm lens on a 36 mm film back; UNVERIFIED as After Effects' default), placed at
/// `(w/2, h/2, -zoom)` looking along +z, so the plane z = 0 maps one to one.
pub fn default_camera(width: u32, height: u32) -> Camera {
    let (w, h) = (f64::from(width), f64::from(height));
    let zoom = hsk_studio_folio::camera_zoom_of_angle(39.5978, w);
    Camera { position: [w / 2.0, h / 2.0, -zoom], rotation: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], zoom, centre: [w / 2.0, h / 2.0] }
}
/// A two-node camera's base rotation: forward towards `poi`, screen-down along world-down made
/// orthogonal; `None` when forward is (anti)parallel to world-down or `poi == position`.
pub fn look_at(position: Vec3, poi: Vec3) -> Option<Mat3> {
    let f = normalize(sub(poi, position))?;
    let down = [0.0, 1.0, 0.0];
    let d = normalize(sub(down, [f[0] * dot(down, f), f[1] * dot(down, f), f[2] * dot(down, f)]))?;
    let r = cross(d, f);
    Some([[r[0], d[0], f[0]], [r[1], d[1], f[1]], [r[2], d[2], f[2]]])
}

/// A camera layer at a tick: position, own rotations, optional point of interest (two-node) and
/// zoom; `None` when a two-node camera is degenerate ([`look_at`]). STU-CMP-040a: one-node and
/// two-node are the same layer with the target enabled or not.
#[allow(clippy::too_many_arguments)]
pub fn camera(
    position: Vec3,
    orientation: Vec3,
    rx: f64,
    ry: f64,
    rz: f64,
    poi: Option<Vec3>,
    zoom: f64,
    width: u32,
    height: u32,
) -> Option<Camera> {
    let own = layer_rotation(orientation, rx, ry, rz);
    let rotation = match poi {
        None => own,
        Some(target) => mat_mul(&look_at(position, target)?, &own),
    };
    (zoom.is_finite() && zoom > 0.0).then_some(Camera {
        position,
        rotation,
        zoom,
        centre: [f64::from(width) / 2.0, f64::from(height) / 2.0],
    })
}

/// A 2D layer between 3D layers (STU-CMP-035a run break).
pub const RUN_BREAK_2D_LAYER: &str = "two_d_layer";
/// An adjustment layer between 3D layers (STU-CMP-035a run break).
pub const RUN_BREAK_ADJUSTMENT: &str = "adjustment_layer";
/// One run break: the breaking layer and why; layers on either side cannot intersect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunBreak {
    pub layer_id: String,
    pub reason: &'static str,
}

/// The homography layer space -> screen of a plane with transform `t` seen by `cam` (its third
/// row is the camera depth of the layer point).
pub fn plane_homography(t: &Transform3d, cam: &Camera) -> Projective2 {
    let rc_t = transpose(&cam.rotation);
    // Camera-space images of e_x, e_y and of the layer origin.
    let o = t.world(0.0, 0.0);
    let ex = sub(t.world(1.0, 0.0), o);
    let ey = sub(t.world(0.0, 1.0), o);
    let cx = mat_vec(&rc_t, ex);
    let cy = mat_vec(&rc_t, ey);
    let c0 = mat_vec(&rc_t, sub(o, cam.position));
    let k = |row: usize| -> [f64; 3] {
        let pick = |v: Vec3| match row {
            0 => cam.zoom * v[0] + cam.centre[0] * v[2],
            1 => cam.zoom * v[1] + cam.centre[1] * v[2],
            _ => v[2],
        };
        [pick(cx), pick(cy), pick(c0)]
    };
    let (r0, r1, r2) = (k(0), k(1), k(2));
    Projective2 { m: [r0[0], r0[1], r0[2], r1[0], r1[1], r1[2], r2[0], r2[1], r2[2]] }
}

/// One plane of a 3D run, in stack order (bottom first).
#[derive(Clone, Debug, PartialEq)]
pub struct Plane3d {
    pub layer_id: String,
    pub content: SceneContent,
    /// Content plan rectangle (layer space = content plan minus `origin`).
    pub bounds: Rect,
    pub origin: [f64; 2],
    pub transform: Transform3d,
    pub mode: StudioBlendMode,
    pub opacity: f64,
    pub fill_opacity: f64,
}

/// Expands one 3D run into 2D scene layers (module docs: depth ranks per pixel, one matted layer
/// per (rank, plane)) for a `width x height` composition seen by `cam`.
pub fn expand_run(
    planes: &[Plane3d],
    cam: &Camera,
    width: u32,
    height: u32,
    space: ColourTag,
    revision: u64,
) -> Result<Vec<SceneLayer>, TemporalError> {
    if planes.len() > MAX_RUN_PLANES {
        return Err(TemporalError::Unsupported { layer_id: planes.first().map(|p| p.layer_id.clone()).unwrap_or_default(), feature: COMPOSITION_3D_RUN_LIMIT });
    }
    let unsupported = |id: &str, feature: &'static str| TemporalError::Unsupported { layer_id: id.to_owned(), feature };
    // Per plane: content plan -> screen homography, its inverse, and the grown content rect.
    struct Prep {
        pin: Projective2,
        inverse: Projective2,
        lo: [f64; 2],
        hi: [f64; 2],
        visible: bool,
    }
    let mut preps = Vec::with_capacity(planes.len());
    for p in planes {
        if !matches!(p.content, SceneContent::Tiles(_) | SceneContent::Static(_) | SceneContent::Precomp(_)) {
            return Err(unsupported(&p.layer_id, COMPOSITION_3D_CONTENT));
        }
        let h = plane_homography(&p.transform, cam);
        let to_layer = Projective2::from_affine(&Affine2::translate(-p.origin[0], -p.origin[1]));
        let pin = to_layer.then(&h);
        let (x0, y0) = (p.bounds.x as f64 - 1.0, p.bounds.y as f64 - 1.0);
        let (x1, y1) = (x0 + f64::from(p.bounds.width) + 2.0, y0 + f64::from(p.bounds.height) + 2.0);
        let depths: Vec<f64> = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)].iter().map(|(x, y)| pin.apply_h(*x, *y).2).collect();
        let front = depths.iter().filter(|d| **d > NEAR_DEPTH).count();
        if front != 0 && front != 4 {
            return Err(unsupported(&p.layer_id, COMPOSITION_3D_CROSSES_CAMERA));
        }
        let inverse = pin.inverse().map_err(|_| unsupported(&p.layer_id, COMPOSITION_3D_CROSSES_CAMERA))?;
        preps.push(Prep { pin, inverse, lo: [x0, y0], hi: [x1, y1], visible: front == 4 });
    }
    let n = usize::try_from(u64::from(width).checked_mul(u64::from(height)).ok_or(CompositeError::Overflow)?).map_err(|_| CompositeError::Overflow)?;
    // rank[i][pixel] = rank of plane i at the pixel, or u8::MAX when absent.
    let mut rank = vec![vec![u8::MAX; n]; planes.len()];
    let mut order: Vec<(f64, usize)> = Vec::with_capacity(planes.len());
    for y in 0..height {
        for x in 0..width {
            let (sx, sy) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
            order.clear();
            for (i, pr) in preps.iter().enumerate() {
                if !pr.visible {
                    continue;
                }
                let (lx, ly, w) = pr.inverse.apply_h(sx, sy);
                if !(w.is_finite() && w > 0.0) {
                    continue;
                }
                let (ux, uy) = (lx / w, ly / w);
                if ux < pr.lo[0] || uy < pr.lo[1] || ux >= pr.hi[0] || uy >= pr.hi[1] {
                    continue;
                }
                let depth = 1.0 / w;
                if depth > NEAR_DEPTH {
                    order.push((depth, i));
                }
            }
            // Farthest first; equal depth: stack order (bottom first).
            order.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
            let at = usize::try_from(y).ok().and_then(|y| y.checked_mul(usize::try_from(width).ok()?)).and_then(|r| r.checked_add(usize::try_from(x).ok()?)).ok_or(CompositeError::Overflow)?;
            for (k, (_, i)) in order.iter().enumerate() {
                if let Some(slot) = rank.get_mut(*i).and_then(|r| r.get_mut(at)) {
                    *slot = u8::try_from(k).unwrap_or(u8::MAX);
                }
            }
        }
    }
    let rect = Rect { x: 0, y: 0, width, height };
    let mut out = Vec::new();
    for k in 0..planes.len() {
        let k8 = u8::try_from(k).map_err(|_| CompositeError::Overflow)?;
        for (i, (p, pr)) in planes.iter().zip(&preps).enumerate() {
            let Some(ranks) = rank.get(i) else { continue };
            if !ranks.contains(&k8) {
                continue;
            }
            let coverage: Vec<f32> = ranks.iter().map(|r| if *r == k8 { 1.0 } else { 0.0 }).collect();
            let matte = crate::mask_paths::coverage_matte(&coverage, rect, &p.layer_id, space, revision)?;
            out.push(SceneLayer {
                layer_id: p.layer_id.clone(),
                content: SceneContent::Pinned(Box::new(PinnedContent { content: p.content.clone(), pin: pr.pin })),
                mode: p.mode,
                opacity: p.opacity,
                fill_opacity: p.fill_opacity,
                matrix: Affine2::IDENTITY,
                matte: Some(matte),
                clipped: false,
            });
        }
    }
    Ok(out)
}
