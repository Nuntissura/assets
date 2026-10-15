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
//!
//! Parenting (lane SMB-AE-3D-2; STU-MOT-008 "the child's transform is evaluated in the parent's
//! transform space", STU-CMP-010 step 5 "the parent chain"): a 3D layer's world point is
//! [`Transform3d::parent`] applied to its own transform, the parent chain being each ancestor's
//! [`Affine3::layer`] map (any affine: a non-uniform parent scale shears the child plane, whose
//! normal is then the cofactor image, [`Transform3d::normal`]). Mixing 2D and 3D (Studio reading;
//! the spec names no rule, After Effects behaviour UNVERIFIED, D5): a 2D ancestor of a 3D layer
//! acts as a 3D layer at Z 0 turned about Z only; a 2D layer under a 3D parent stays 2D and takes
//! the parent's 2D part only (X/Y position, Z rotation, X/Y scale, X/Y anchor; Z position, anchor
//! Z, X/Y rotation and orientation ignored). A camera or light under a parent ([`camera_in`]) moves
//! with the chain and turns by its rotation part. A camera or light as a parent is refused
//! ([`COMPOSITION_3D_PARENT`]); a singular chain is [`COMPOSITION_3D_PARENT_DEGENERATE`].
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
/// A camera or light layer as the parent of another layer (lane SMB-AE-3D-2 parents every other
/// layer kind, 2D or 3D, in 3D; a camera or light parent is not modelled).
pub const COMPOSITION_3D_PARENT: &str = "composition_3d_parent";
/// A parent chain whose linear part is singular (a zero scale) under a 3D layer, camera or light.
pub const COMPOSITION_3D_PARENT_DEGENERATE: &str = "composition_3d_parent_degenerate";
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
        ("shadow_diffusion", "planar renderer: hard shadows only"),
        ("iris_shape", "planar renderer: every iris renders as the fast rectangle"),
        ("iris_rotation", "planar renderer: fast rectangle iris only"),
        ("iris_roundness", "planar renderer: fast rectangle iris only"),
        ("iris_aspect_ratio", "planar renderer: fast rectangle iris only"),
        ("iris_diffraction_fringe", "planar renderer: no diffraction fringe"),
        ("highlight_gain", "planar renderer: no bokeh highlights"),
        ("highlight_threshold", "planar renderer: no bokeh highlights"),
        ("highlight_saturation", "planar renderer: no bokeh highlights"),
        ("focus_area_width", "planar renderer: thin-lens focus at one distance"),
        ("split_blur_level", "planar renderer: one blur level"),
        ("light_colour_profile", "planar renderer: light colour is used in the plan's colour space, unconverted"),
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

/// An affine map of 3D space, `p -> m p + t` (lane SMB-AE-3D-2: the parent chain of a 3D layer).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine3 {
    pub m: Mat3,
    pub t: Vec3,
}
impl Affine3 {
    pub const IDENTITY: Self = Self { m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], t: [0.0; 3] };
    pub fn apply(&self, p: Vec3) -> Vec3 {
        let r = mat_vec(&self.m, p);
        [r[0] + self.t[0], r[1] + self.t[1], r[2] + self.t[2]]
    }
    /// `outer` after `self`.
    pub fn then(&self, outer: &Affine3) -> Affine3 {
        Affine3 { m: mat_mul(&outer.m, &self.m), t: outer.apply(self.t) }
    }
    /// One layer's transform from its own layer space to its parent's: `position + R S (p -
    /// anchor)` with `S = diag(sx, sy, 1)` (a layer has no Z scale; Studio reading, UNVERIFIED).
    pub fn layer(anchor: Vec3, position: Vec3, scale: [f64; 2], rotation: &Mat3) -> Affine3 {
        let mut m = *rotation;
        for row in &mut m {
            row[0] *= scale[0];
            row[1] *= scale[1];
        }
        let a = mat_vec(&m, anchor);
        Affine3 { m, t: [position[0] - a[0], position[1] - a[1], position[2] - a[2]] }
    }
    /// The cofactor matrix `det(m) m^-T` (maps plane normals; never divides).
    pub fn cofactor(&self) -> Mat3 {
        let m = &self.m;
        let c = |r0: usize, r1: usize, c0: usize, c1: usize| m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
        [
            [c(1, 2, 1, 2), -c(1, 2, 0, 2), c(1, 2, 0, 1)],
            [-c(0, 2, 1, 2), c(0, 2, 0, 2), -c(0, 2, 0, 1)],
            [c(0, 1, 1, 2), -c(0, 1, 0, 2), c(0, 1, 0, 1)],
        ]
    }
    pub fn det(&self) -> f64 {
        let m = &self.m;
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0]) + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    }
    /// The inverse map, `None` when `m` is singular or not finite.
    pub fn inverse(&self) -> Option<Affine3> {
        let det = self.det();
        if !(det.is_finite() && det.abs() > 1e-300) {
            return None;
        }
        let cof = self.cofactor();
        let mut inv = transpose(&cof);
        for row in &mut inv {
            for v in row.iter_mut() {
                *v /= det;
            }
        }
        let t = mat_vec(&inv, self.t);
        Some(Affine3 { m: inv, t: [-t[0], -t[1], -t[2]] })
    }
}

/// The rotation part of a linear map (Gram-Schmidt on its columns, right-handed): what a camera
/// or light takes from a scaled or sheared parent chain (Studio reading, UNVERIFIED). `None` when
/// the first two columns are degenerate.
pub fn rotation_part(m: &Mat3) -> Option<Mat3> {
    let col = |c: usize| [m[0][c], m[1][c], m[2][c]];
    let u0 = normalize(col(0))?;
    let c1 = col(1);
    let u1 = normalize(sub(c1, scale(u0, dot(c1, u0))))?;
    let u2 = cross(u0, u1);
    Some([[u0[0], u1[0], u2[0]], [u0[1], u1[1], u2[1]], [u0[2], u1[2], u2[2]]])
}

/// The 3D transform of one layer at a tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform3d {
    pub anchor: Vec3,
    pub position: Vec3,
    /// X and Y scale factors (1 = unscaled); a plane has no Z extent.
    pub scale: [f64; 2],
    pub rotation: Mat3,
    /// The parent chain (STU-MOT-008), the layer's parent space -> world; [`Affine3::IDENTITY`]
    /// without a parent.
    pub parent: Affine3,
}
impl Transform3d {
    /// World point of layer point `(x, y, 0)`.
    pub fn world(&self, x: f64, y: f64) -> Vec3 {
        let local = [self.scale[0] * (x - self.anchor[0]), self.scale[1] * (y - self.anchor[1]), -self.anchor[2]];
        let r = mat_vec(&self.rotation, local);
        let own = [self.position[0] + r[0], self.position[1] + r[1], self.position[2] + r[2]];
        if self.parent == Affine3::IDENTITY { own } else { self.parent.apply(own) }
    }
    /// A world normal of the plane (not necessarily unit; either side).
    pub fn normal(&self) -> Vec3 {
        let own = mat_vec(&self.rotation, [0.0, 0.0, 1.0]);
        if self.parent == Affine3::IDENTITY { own } else { mat_vec(&self.parent.cofactor(), own) }
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
    /// Depth of field when the camera enables it (STU-CMP-040 `depth_of_field`).
    pub dof: Option<DepthOfField>,
}

/// Thin-lens depth of field of a camera (STU-CMP-040 `focus_distance`, `aperture`, `blur_level`),
/// in pixels and as a fraction. The circle of confusion of a point at camera depth `z` has the
/// screen radius `blur aperture zoom |z - focus| / (2 z focus)` (thin lens with the focal length
/// small against the distances; Studio reading, UNVERIFIED).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DepthOfField {
    pub focus_distance: f64,
    pub aperture: f64,
    pub blur_level: f64,
}
impl DepthOfField {
    /// Screen-space circle-of-confusion radius (pixels) at camera depth `z` for `zoom`.
    pub fn screen_radius(&self, zoom: f64, z: f64) -> f64 {
        self.blur_level * self.aperture * zoom * (z - self.focus_distance).abs() / (2.0 * z * self.focus_distance)
    }
}
/// Largest layer-space defocus radius (texels).
pub const MAX_DOF_RADIUS: f64 = 32.0;
/// Most kernel taps one defocused surface may take.
pub const MAX_DOF_TAPS: u64 = 1 << 26;
/// A defocus radius above [`MAX_DOF_RADIUS`] or a surface above [`MAX_DOF_TAPS`].
pub const COMPOSITION_3D_DOF_BUDGET: &str = "composition_3d_dof_budget";
/// Studio's default camera for a `width x height` composition: zoom for a 39.5978 degree horizontal
/// angle of view (a 50 mm lens on a 36 mm film back; UNVERIFIED as After Effects' default), placed at
/// `(w/2, h/2, -zoom)` looking along +z, so the plane z = 0 maps one to one.
pub fn default_camera(width: u32, height: u32) -> Camera {
    let (w, h) = (f64::from(width), f64::from(height));
    let zoom = hsk_studio_folio::camera_zoom_of_angle(39.5978, w);
    Camera { position: [w / 2.0, h / 2.0, -zoom], rotation: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], zoom, centre: [w / 2.0, h / 2.0], dof: None }
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
    camera_in(&Affine3::IDENTITY, position, orientation, rx, ry, rz, poi, zoom, width, height)
}

/// [`camera`] under a parent chain (lane SMB-AE-3D-2; Studio reading, UNVERIFIED): the position
/// and the point of interest are mapped by `parent`; a one-node camera turns by the rotation part
/// of the chain ([`rotation_part`], scale and shear dropped), a two-node camera looks at its mapped
/// point of interest. `None` when degenerate.
#[allow(clippy::too_many_arguments)]
pub fn camera_in(
    parent: &Affine3,
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
    let identity = *parent == Affine3::IDENTITY;
    let position = if identity { position } else { parent.apply(position) };
    let rotation = match poi {
        None if identity => own,
        None => mat_mul(&rotation_part(&parent.m)?, &own),
        Some(target) => mat_mul(&look_at(position, if identity { target } else { parent.apply(target) })?, &own),
    };
    (zoom.is_finite() && zoom > 0.0).then_some(Camera {
        position,
        rotation,
        zoom,
        centre: [f64::from(width) / 2.0, f64::from(height) / 2.0],
        dof: None,
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
    expand_run_lit(planes, &[], &[], cam, width, height, space, revision)
}

/// [`expand_run`] with lights (STU-CMP-045): with at least one light, every plane whose material
/// accepts lights becomes [`SceneContent::Lit`] (shaded per content texel by [`shade`] before the
/// projective resample), with the other planes of the run as shadow casters (STU-CMP-035a: planes
/// of other runs neither intersect nor shadow). `materials` is per plane (missing = default).
#[allow(clippy::too_many_arguments)]
pub fn expand_run_lit(
    planes: &[Plane3d],
    materials: &[Material],
    lights: &[Light],
    cam: &Camera,
    width: u32,
    height: u32,
    space: ColourTag,
    revision: u64,
) -> Result<Vec<SceneLayer>, TemporalError> {
    if lights.len() > MAX_LIGHTS {
        return Err(TemporalError::Unsupported { layer_id: lights.first().map(|l| l.layer_id.clone()).unwrap_or_default(), feature: COMPOSITION_3D_LIGHT_LIMIT });
    }
    let material = |i: usize| materials.get(i).copied().unwrap_or(Material::DEFAULT);
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
                content: SceneContent::Pinned(Box::new(PinnedContent { content: lit_content(planes, i, &material, lights, cam), pin: pr.pin })),
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

/// Most lights one run is shaded with.
pub const MAX_LIGHTS: usize = 16;
/// More active lights than [`MAX_LIGHTS`].
pub const COMPOSITION_3D_LIGHT_LIMIT: &str = "composition_3d_light_limit";
/// A 3D layer on the document-graph path (`lower_document_frame`), which has no 3D runs.
pub const COMPOSITION_3D_ON_DOCUMENT_GRAPH: &str = "composition_3d_on_document_graph";

/// Material options of one plane at a tick (STU-MOT-014), as fractions (percent / 100) except
/// `specular_shininess` (percent). Members the planar renderer does not honour are not here
/// ([`PLANAR_RENDERER`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Material {
    pub accepts_lights: bool,
    pub accepts_shadows: bool,
    pub casts_shadows: bool,
    pub ambient: f64,
    pub diffuse: f64,
    pub specular_intensity: f64,
    pub specular_shininess: f64,
    pub metal: f64,
    pub light_transmission: f64,
}
impl Material {
    /// The Folio defaults (`hsk_studio_folio::STUDIO_3D_BOUNDS`): ambient 100 %, diffuse 50 %,
    /// specular 50 %, shininess 5 %, metal 100 %, transmission 0; lit, no shadows.
    pub const DEFAULT: Self = Self {
        accepts_lights: true,
        accepts_shadows: false,
        casts_shadows: false,
        ambient: 1.0,
        diffuse: 0.5,
        specular_intensity: 0.5,
        specular_shininess: 5.0,
        metal: 1.0,
        light_transmission: 0.0,
    };
}

/// STU-CMP-045 light type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightKind {
    Parallel,
    Spot,
    Point,
    Ambient,
}
/// STU-CMP-045 falloff type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Falloff {
    None,
    Smooth,
    InverseSquareClamped,
}

/// One light at a tick, in world space.
#[derive(Clone, Debug, PartialEq)]
pub struct Light {
    pub layer_id: String,
    pub kind: LightKind,
    /// Plan-space colour (no conversion).
    pub colour: [f64; 3],
    /// Intensity as a fraction (100 % = 1); unbounded, may be negative (STU-CMP-045).
    pub intensity: f64,
    pub position: Vec3,
    /// Unit direction the light points (towards its point of interest).
    pub direction: Vec3,
    /// Full cone angle, degrees (spot).
    pub cone_angle: f64,
    /// Cone feather as a fraction of the half angle (spot).
    pub cone_feather: f64,
    pub falloff: Falloff,
    pub falloff_start: f64,
    pub falloff_distance: f64,
    pub casts_shadows: bool,
    /// Shadow darkness as a fraction.
    pub shadow_darkness: f64,
}

/// A plane of the run that may cast a shadow onto the lit plane.
#[derive(Clone, Debug, PartialEq)]
pub struct Caster {
    pub layer_id: String,
    pub content: SceneContent,
    pub bounds: Rect,
    pub origin: [f64; 2],
    pub transform: Transform3d,
    pub opacity: f64,
    pub light_transmission: f64,
}

/// Content shaded by lights before its projective resample ([`SceneContent::Lit`]).
#[derive(Clone, Debug, PartialEq)]
pub struct LitContent {
    /// Tiles, a static subtree or a precomposition.
    pub content: SceneContent,
    /// The content plan rectangle that is rendered and shaded.
    pub rect: Rect,
    pub origin: [f64; 2],
    pub transform: Transform3d,
    pub material: Material,
    pub lights: Vec<Light>,
    /// The camera position (view vector of the specular term).
    pub eye: Vec3,
    pub casters: Vec<Caster>,
    /// Depth of field: the plane's content-plan -> screen homography (its third row is the camera
    /// depth), the camera zoom and the lens (`None` = in focus everywhere).
    pub dof: Option<(Projective2, f64, DepthOfField)>,
}

fn lit_content(planes: &[Plane3d], i: usize, material: &dyn Fn(usize) -> Material, lights: &[Light], cam: &Camera) -> SceneContent {
    let Some(p) = planes.get(i) else {
        return SceneContent::None;
    };
    let m = material(i);
    let lit = !lights.is_empty() && m.accepts_lights;
    if !lit && cam.dof.is_none() {
        return p.content.clone();
    }
    let lights = if lit { lights } else { &[] };
    let to_layer = Projective2::from_affine(&Affine2::translate(-p.origin[0], -p.origin[1]));
    let dof = cam.dof.map(|d| (to_layer.then(&plane_homography(&p.transform, cam)), cam.zoom, d));
    let casters = planes
        .iter()
        .enumerate()
        .filter(|(j, _)| *j != i && material(*j).casts_shadows && m.accepts_shadows)
        .map(|(j, c)| Caster {
            layer_id: c.layer_id.clone(),
            content: c.content.clone(),
            bounds: c.bounds,
            origin: c.origin,
            transform: c.transform,
            opacity: c.opacity,
            light_transmission: material(j).light_transmission,
        })
        .collect();
    SceneContent::Lit(Box::new(LitContent {
        content: p.content.clone(),
        rect: p.bounds,
        origin: p.origin,
        transform: p.transform,
        material: m,
        lights: lights.to_vec(),
        eye: cam.position,
        casters,
        dof,
    }))
}

/// Layer-space defocus radius (texels) of the content texel centred at plan point `(x, y)`: the
/// screen circle-of-confusion radius divided by the local screen scale `sqrt(|det J|)` of the
/// content -> screen homography at that point.
pub fn defocus_radius(view: &Projective2, zoom: f64, dof: &DepthOfField, x: f64, y: f64) -> f64 {
    let h = &view.m;
    let (sx, sy, w) = view.apply_h(x, y);
    let w2 = w * w;
    let j00 = (h[0] * w - sx * h[6]) / w2;
    let j01 = (h[1] * w - sx * h[7]) / w2;
    let j10 = (h[3] * w - sy * h[6]) / w2;
    let j11 = (h[4] * w - sy * h[7]) / w2;
    let scale = (j00 * j11 - j01 * j10).abs().sqrt();
    dof.screen_radius(zoom, w) / scale
}

fn coverage(d: f64, r: f64) -> f64 {
    (r + 0.5 - d).clamp(0.0, 1.0)
}

/// Defocuses a straight RGB + alpha surface (`w x h`, row-major) with a per-texel radius: each
/// output texel gathers its neighbours under the fast-rectangle iris, a square of half-width `r`
/// with anti-aliased edges (`weight = c(|dx|) c(|dy|)`, `c(d) = clamp(r + 0.5 - d, 0, 1)`), in
/// premultiplied colour, normalised by the full kernel weight (texels outside the surface are
/// transparent). The blur stays inside the plane's footprint. Refuses (`Err(())`) a radius above
/// [`MAX_DOF_RADIUS`], a non-finite radius or more than [`MAX_DOF_TAPS`] taps, before allocating.
// Indices stay below `w * h` (checked) and `reach <= 33`.
#[allow(clippy::type_complexity, clippy::arithmetic_side_effects, clippy::result_unit_err)]
pub fn defocus(rgb: &[f32], alpha: &[f32], w: usize, h: usize, radii: &[f64]) -> Result<(Vec<f32>, Vec<f32>), ()> {
    let n = w.checked_mul(h).ok_or(())?;
    if rgb.len() != n.checked_mul(3).ok_or(())? || alpha.len() != n || radii.len() != n {
        return Err(());
    }
    let mut taps: u64 = 0;
    for r in radii {
        if !(r.is_finite() && *r >= 0.0 && *r <= MAX_DOF_RADIUS) {
            return Err(());
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let side = 2 * ((r + 0.5).ceil() as u64) + 1;
        taps = taps.checked_add(side.checked_mul(side).ok_or(())?).ok_or(())?;
    }
    if taps > MAX_DOF_TAPS {
        return Err(());
    }
    let mut out_rgb = vec![0.0f32; rgb.len()];
    let mut out_a = vec![0.0f32; n];
    for y in 0..h {
        for x in 0..w {
            let at = y * w + x;
            let r = radii[at];
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let reach = (r + 0.5).ceil() as usize;
            let (mut sw, mut sa, mut sc) = (0.0f64, 0.0f64, [0.0f64; 3]);
            for dy in 0..=2 * reach {
                let wy = coverage((dy as f64 - reach as f64).abs(), r);
                if wy == 0.0 {
                    continue;
                }
                for dx in 0..=2 * reach {
                    let wx = coverage((dx as f64 - reach as f64).abs(), r);
                    let wt = wx * wy;
                    if wt == 0.0 {
                        continue;
                    }
                    sw += wt;
                    let (Some(yy), Some(xx)) = ((y + dy).checked_sub(reach), (x + dx).checked_sub(reach)) else { continue };
                    if yy >= h || xx >= w {
                        continue;
                    }
                    let k = yy * w + xx;
                    let a = f64::from(alpha[k]);
                    sa += wt * a;
                    for c in 0..3 {
                        sc[c] += wt * a * f64::from(rgb[k * 3 + c]);
                    }
                }
            }
            let a = sa / sw;
            #[allow(clippy::cast_possible_truncation)]
            {
                out_a[at] = a as f32;
                for c in 0..3 {
                    out_rgb[at * 3 + c] = if sa > 0.0 { (sc[c] / sa) as f32 } else { 0.0 };
                }
            }
        }
    }
    Ok((out_rgb, out_a))
}

fn smoothstep(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
fn scale(a: Vec3, k: f64) -> Vec3 {
    [a[0] * k, a[1] * k, a[2] * k]
}

/// Distance attenuation (Studio reading of STU-CMP-045 falloff, UNVERIFIED): none = 1; smooth =
/// 1 up to `start`, smoothstep down to 0 over `distance`; inverse square clamped = 1 up to the
/// radius `start`, then `(start / d)^2`.
pub fn attenuation(falloff: Falloff, start: f64, distance: f64, d: f64) -> f64 {
    match falloff {
        Falloff::None => 1.0,
        Falloff::Smooth if d <= start => 1.0,
        Falloff::Smooth if distance <= 0.0 => 0.0,
        Falloff::Smooth => 1.0 - smoothstep((d - start) / distance),
        Falloff::InverseSquareClamped if d <= start => 1.0,
        Falloff::InverseSquareClamped => (start / d) * (start / d),
    }
}

/// Spot cone factor: 1 inside `half (1 - feather)`, smoothstep to 0 at the half angle.
pub fn spot_factor(cone_angle: f64, feather: f64, angle_from_axis: f64) -> f64 {
    let half = cone_angle / 2.0;
    let inner = half * (1.0 - feather.clamp(0.0, 1.0));
    if angle_from_axis <= inner {
        1.0
    } else if angle_from_axis >= half {
        0.0
    } else {
        smoothstep((half - angle_from_axis) / (half - inner))
    }
}

/// Straight alpha of a caster at world point `p` (its plane), nearest content texel; 0 outside.
pub type CasterAlpha<'a> = &'a dyn Fn(usize, f64, f64) -> f32;

/// Shades one straight RGB texel of a lit plane at layer point `(x, y)` (module docs of slice 2):
/// `albedo (ambient A + diffuse D) + specular S mix(1, albedo, metal)`, `A` the sum of ambient
/// lights, `D` the sum of `colour intensity attenuation spot shadow max(0, N.L)`, `S` the sum of
/// `colour intensity attenuation spot shadow max(0, R.V)^(1 + shininess)`; `N` faces the eye
/// (two-sided planes). Shadows: every caster the segment from the point towards the light crosses
/// in front of the light multiplies by `1 - darkness alpha opacity (1 - transmission)` (hard
/// shadows; `caster_alpha(i, x, y)` reads caster `i` at its plan point). The result is clamped
/// into [0, 1], the composition's unit sample range (the light intensities themselves are never
/// clamped). Studio reading of the classic renderer, UNVERIFIED (D5).
pub fn shade(lit: &LitContent, x: f64, y: f64, albedo: [f32; 3], caster_alpha: CasterAlpha<'_>) -> [f32; 3] {
    let p = lit.transform.world(x, y);
    let mut n = normalize(lit.transform.normal()).unwrap_or([0.0, 0.0, 1.0]);
    let to_eye = sub(lit.eye, p);
    if dot(n, to_eye) < 0.0 {
        n = scale(n, -1.0);
    }
    let v = normalize(to_eye).unwrap_or([0.0, 0.0, -1.0]);
    let m = &lit.material;
    let (mut amb, mut dif, mut spe) = ([0.0f64; 3], [0.0f64; 3], [0.0f64; 3]);
    for l in &lit.lights {
        let c = scale(l.colour, l.intensity);
        if l.kind == LightKind::Ambient {
            amb = [amb[0] + c[0], amb[1] + c[1], amb[2] + c[2]];
            continue;
        }
        let (dir_to_light, dist) = match l.kind {
            LightKind::Parallel => (scale(l.direction, -1.0), f64::INFINITY),
            _ => {
                let d = sub(l.position, p);
                let len = dot(d, d).sqrt();
                match normalize(d) {
                    Some(u) => (u, len),
                    None => continue,
                }
            }
        };
        let att = if l.kind == LightKind::Parallel { 1.0 } else { attenuation(l.falloff, l.falloff_start, l.falloff_distance, dist) };
        let spot = if l.kind == LightKind::Spot {
            let cos = (-dot(dir_to_light, l.direction)).clamp(-1.0, 1.0);
            spot_factor(l.cone_angle, l.cone_feather, cos.acos().to_degrees())
        } else {
            1.0
        };
        let ndl = dot(n, dir_to_light);
        if ndl <= 0.0 || att * spot == 0.0 {
            continue;
        }
        let mut shadow = 1.0;
        if l.casts_shadows && m.accepts_shadows {
            for (i, c) in lit.casters.iter().enumerate() {
                let Some(t) = segment_hit(&c.transform, p, dir_to_light, dist) else { continue };
                let hit = [p[0] + dir_to_light[0] * t, p[1] + dir_to_light[1] * t, p[2] + dir_to_light[2] * t];
                let Some((lx, ly)) = layer_point(&c.transform, hit) else { continue };
                let a = f64::from(caster_alpha(i, lx + c.origin[0], ly + c.origin[1]));
                shadow *= 1.0 - l.shadow_darkness * a * c.opacity * (1.0 - c.light_transmission);
            }
        }
        let k = att * spot * shadow;
        let r = sub(scale(n, 2.0 * ndl), dir_to_light);
        let rv = dot(r, v).max(0.0);
        let sp = rv.powf(1.0 + m.specular_shininess);
        for ch in 0..3 {
            dif[ch] += c[ch] * k * ndl;
            spe[ch] += c[ch] * k * sp;
        }
    }
    let mut out = [0.0f32; 3];
    for ch in 0..3 {
        let a = f64::from(albedo[ch]);
        let lit_v = a * (m.ambient * amb[ch] + m.diffuse * dif[ch]) + m.specular_intensity * spe[ch] * (1.0 + m.metal * (a - 1.0));
        #[allow(clippy::cast_possible_truncation)]
        let f = lit_v.clamp(0.0, 1.0) as f32;
        out[ch] = f;
    }
    out
}

/// Distance along the unit ray `p + t u` to the plane of `t3`, for `1e-9 < t < max` (a shadow
/// segment towards the light).
fn segment_hit(t3: &Transform3d, p: Vec3, u: Vec3, max: f64) -> Option<f64> {
    let n = t3.normal();
    let o = t3.world(t3.anchor[0], t3.anchor[1]);
    let den = dot(n, u);
    if den.abs() < 1e-12 {
        return None;
    }
    let t = dot(n, sub(o, p)) / den;
    (t > 1e-9 && t < max).then_some(t)
}

/// Layer point of world point `w` on the plane of `t3` (inverse of [`Transform3d::world`]).
pub fn layer_point(t3: &Transform3d, w: Vec3) -> Option<(f64, f64)> {
    let w = if t3.parent == Affine3::IDENTITY { w } else { t3.parent.inverse()?.apply(w) };
    let u = mat_vec(&transpose(&t3.rotation), sub(w, t3.position));
    (t3.scale[0] != 0.0 && t3.scale[1] != 0.0).then(|| (u[0] / t3.scale[0] + t3.anchor[0], u[1] / t3.scale[1] + t3.anchor[1]))
}

/// STU-CMP-045a: a shadow needs the light's `casts_shadows`, the caster's material
/// `casts_shadows` and the receiver's `accepts_shadows`; all default off.
pub const SHADOW_LIGHT_OFF: &str = "light_casts_shadows_off";
pub const SHADOW_CASTER_OFF: &str = "caster_casts_shadows_off";
pub const SHADOW_RECEIVER_OFF: &str = "receiver_accepts_shadows_off";
/// The determinate STU-CMP-045a diagnostic: which of the three switches are off (empty = the
/// shadow is cast), in that fixed order.
pub fn shadow_switches_off(light_casts: bool, caster_casts: bool, receiver_accepts: bool) -> Vec<&'static str> {
    let mut off = Vec::new();
    if !light_casts {
        off.push(SHADOW_LIGHT_OFF);
    }
    if !caster_casts {
        off.push(SHADOW_CASTER_OFF);
    }
    if !receiver_accepts {
        off.push(SHADOW_RECEIVER_OFF);
    }
    off
}
/// One (light, caster, receiver) triple with some but not all shadow switches on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShadowDiagnostic {
    pub light: String,
    pub caster: String,
    pub receiver: String,
    pub off: Vec<&'static str>,
}
