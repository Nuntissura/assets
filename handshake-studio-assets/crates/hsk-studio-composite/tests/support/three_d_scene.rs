//! A 3D run (lane SMB-AE-3D) and its independent oracle, shared by the renderer tests (render-cpu
//! and render-gpu `tests/composition_three_d.rs`). Three textured planes under a two-node camera:
//! "tf" turned about Y and "te" tilted about X cross each other (an intersection line); "tg" lies
//! further back, rotated and scaled. The oracle does not use `three_d`: per pixel centre it casts
//! the camera ray (its own look-at basis and rotations, `f64` sin/cos), intersects every plane,
//! keeps the hits inside the content rectangle grown by one pixel, orders them farthest first and
//! composites each plane's sample there. The sample is pigment's reference sampler through the
//! homography fitted to the four projected content corners (`Projective2::from_quads`), so the
//! renderers' plane homography is checked as well. Include next to `composition_scenes.rs` as
//! module `scenes`.
//!
//! The lit variant ([`lit_scene`] / [`lit_oracle`]) adds an ambient, a point (shadow casting,
//! smooth falloff), a spot and a parallel light and per-plane materials; the oracle shades every
//! content texel with its own implementation of the documented model (`three_d::shade`: Lambert
//! diffuse, Phong specular with exponent 1 + shininess, metal tint, hard shadows through the other
//! planes' nearest-texel alpha), then samples and composites as above.
#![allow(dead_code)]
use crate::scenes::{BACKGROUND, Pixels, space, tile_id};
use hsk_studio_composite::three_d::{Affine3, DepthOfField, Falloff, Light, LightKind, Material, Plane3d, Transform3d, camera, expand_run, expand_run_lit, layer_rotation};
use hsk_studio_composite::{SceneComposition, SceneContent, SceneLayer, SceneMatte, SceneMatteMode};
use hsk_studio_pigment::transform_math::Affine2;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, StudioBlendMode, composite_over};
use hsk_studio_pigment::transform_math::{LayerTransform, PreparedTransform, Projective2, ResampleFilter};

pub const W: u32 = 32;
pub const H: u32 = 24;
pub const CAMERA_POSITION: [f64; 3] = [10.0, 4.0, -60.0];
pub const CAMERA_POI: [f64; 3] = [16.0, 12.0, 0.0];
pub const ZOOM: f64 = 50.0;

/// One plane: tile, its layer number and placement, anchor (layer space), position, scale,
/// orientation, X/Y/Z rotation (degrees) and opacity.
struct Spec {
    tile: &'static str,
    layer: u8,
    at: (i64, i64),
    anchor: [f64; 3],
    position: [f64; 3],
    scale: [f64; 2],
    orientation: [f64; 3],
    rot: [f64; 3],
    opacity: f64,
    /// Lane SMB-AE-3D-2: the parent (pivot = anchor = position, X/Y/Z rotation, X/Y scale).
    parent: Option<ParentSpec>,
}
/// A parent layer: `pivot + R S (v - pivot)`, `R` the X, Y, Z turns, `S = diag(sx, sy, 1)`.
#[derive(Clone, Copy)]
struct ParentSpec {
    pivot: [f64; 3],
    rot: [f64; 3],
    scale: [f64; 2],
}
fn specs() -> [Spec; 3] {
    [
        Spec { tile: "tg", layer: 12, at: (0, 0), anchor: [4.0, 4.0, 0.0], position: [20.0, 10.0, 25.0], scale: [1.5, 1.25], orientation: [0.0, 0.0, 0.0], rot: [0.0, 0.0, 20.0], opacity: 1.0, parent: None },
        Spec { tile: "tf", layer: 11, at: (3, 2), anchor: [10.0, 7.0, 0.0], position: [16.0, 12.0, 0.0], scale: [1.0, 1.0], orientation: [0.0, 0.0, 0.0], rot: [0.0, 35.0, 0.0], opacity: 0.9, parent: None },
        Spec { tile: "te", layer: 10, at: (0, 0), anchor: [5.0, 4.0, 1.5], position: [15.0, 13.0, 1.0], scale: [1.4, 1.4], orientation: [0.0, 0.0, 10.0], rot: [-40.0, 0.0, 0.0], opacity: 1.0, parent: None },
    ]
}

/// Lane SMB-AE-3D-2: "tf" under a 3D parent (turned about X, Y and Z, scaled non-uniformly, so the
/// child plane is sheared) and "te" under a 2D parent (Z turn and scale at Z 0).
fn parented_specs() -> [Spec; 3] {
    let [tg, tf, te] = specs();
    [
        tg,
        Spec { parent: Some(ParentSpec { pivot: [16.0, 12.0, 0.0], rot: [12.0, -15.0, 6.0], scale: [1.15, 0.9] }), ..tf },
        Spec { parent: Some(ParentSpec { pivot: [15.0, 13.0, 0.0], rot: [0.0, 0.0, 10.0], scale: [0.95, 1.1] }), ..te },
    ]
}

/// The run's planes, bottom first.
pub fn planes(pixels: &Pixels) -> Vec<Plane3d> {
    planes_of(pixels, &specs())
}
fn planes_of(pixels: &Pixels, specs: &[Spec]) -> Vec<Plane3d> {
    specs
        .iter()
        .map(|s| {
            let t = pixels.get(&tile_id(s.layer, s.tile)).expect("tile");
            let tile = t.scene_tile(s.at.0, s.at.1);
            Plane3d {
                layer_id: format!("SLYR-3d-{}", s.tile),
                bounds: tile.placement,
                origin: [s.at.0 as f64, s.at.1 as f64],
                content: SceneContent::Tiles(vec![tile]),
                transform: Transform3d {
                    anchor: s.anchor,
                    position: s.position,
                    scale: s.scale,
                    rotation: layer_rotation(s.orientation, s.rot[0], s.rot[1], s.rot[2]),
                    parent: s.parent.map_or(Affine3::IDENTITY, |p| Affine3::layer(p.pivot, p.pivot, p.scale, &layer_rotation([0.0; 3], p.rot[0], p.rot[1], p.rot[2]))),
                },
                mode: StudioBlendMode::Normal,
                opacity: s.opacity,
                fill_opacity: 1.0,
                layer_mask: None,
                track_matte: None,
            }
        })
        .collect()
}

/// The 3D run expanded into a 2D scene.
pub fn three_d_scene(pixels: &Pixels) -> SceneComposition {
    let cam = camera(CAMERA_POSITION, [0.0; 3], 0.0, 0.0, 0.0, Some(CAMERA_POI), ZOOM, W, H).expect("camera");
    SceneComposition {
        composition_id: "three-d".to_owned(),
        width: W,
        height: H,
        background: Some(BACKGROUND),
        filter: ResampleFilter::Bilinear,
        layers: expand_run(&planes(pixels), &cam, W, H, space(), 7).expect("run expands"),
    }
}

type V = [f64; 3];
fn sub(a: V, b: V) -> V {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: V, b: V) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V, b: V) -> V {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn unit(a: V) -> V {
    let l = dot(a, a).sqrt();
    [a[0] / l, a[1] / l, a[2] / l]
}
fn scale(a: V, k: f64) -> V {
    [a[0] * k, a[1] * k, a[2] * k]
}
fn add(a: V, b: V) -> V {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
/// Rotate `v` about axis `axis` (0 = X, 1 = Y, 2 = Z) by `deg` (Y down, Z away; the same sense as
/// the 2D rotation about Z).
fn turn(v: V, axis: usize, deg: f64) -> V {
    let (s, c) = deg.to_radians().sin_cos();
    match axis {
        0 => [v[0], c * v[1] - s * v[2], s * v[1] + c * v[2]],
        1 => [c * v[0] + s * v[2], v[1], -s * v[0] + c * v[2]],
        _ => [c * v[0] - s * v[1], s * v[0] + c * v[1], v[2]],
    }
}
/// Orientation (X, Y, Z) first, then the X, Y and Z rotations.
fn rotate(s: &Spec, v: V) -> V {
    let mut v = v;
    for (axis, deg) in [(0, s.orientation[0]), (1, s.orientation[1]), (2, s.orientation[2]), (0, s.rot[0]), (1, s.rot[1]), (2, s.rot[2])] {
        v = turn(v, axis, deg);
    }
    v
}
/// World point of layer point (x, y): the layer's own transform, then its parent's.
fn world(s: &Spec, x: f64, y: f64) -> V {
    let own = add(s.position, rotate(s, [s.scale[0] * (x - s.anchor[0]), s.scale[1] * (y - s.anchor[1]), -s.anchor[2]]));
    match s.parent {
        None => own,
        Some(p) => {
            let mut v = sub(own, p.pivot);
            v = [v[0] * p.scale[0], v[1] * p.scale[1], v[2]];
            for (axis, deg) in [(0, p.rot[0]), (1, p.rot[1]), (2, p.rot[2])] {
                v = turn(v, axis, deg);
            }
            add(p.pivot, v)
        }
    }
}
/// Ray `c + t u` against the plane of `s` (any affine parent): `(t, x, y)` with `(x, y)` the layer
/// point, by Cramer's rule on `world(0,0) + x ex + y ey = c + t u`.
fn hit_any(s: &Spec, c: V, u: V) -> Option<(f64, f64, f64)> {
    let w0 = world(s, 0.0, 0.0);
    let ex = sub(world(s, 1.0, 0.0), w0);
    let ey = sub(world(s, 0.0, 1.0), w0);
    let nu = scale(u, -1.0);
    let rhs = sub(c, w0);
    let det3 = |a: V, b: V, d: V| dot(a, cross(b, d));
    let den = det3(ex, ey, nu);
    if den.abs() < 1e-12 {
        return None;
    }
    Some((det3(ex, ey, rhs) / den, det3(rhs, ey, nu) / den, det3(ex, rhs, nu) / den))
}
/// Camera basis: right, down, forward (world).
fn basis() -> (V, V, V) {
    let f = unit(sub(CAMERA_POI, CAMERA_POSITION));
    let d = unit(sub([0.0, 1.0, 0.0], scale(f, f[1])));
    (cross(d, f), d, f)
}
fn project(p: V) -> [f64; 2] {
    let (r, d, f) = basis();
    let q = sub(p, CAMERA_POSITION);
    let z = dot(q, f);
    [f64::from(W) / 2.0 + ZOOM * dot(q, r) / z, f64::from(H) / 2.0 + ZOOM * dot(q, d) / z]
}

/// Straight RGBA of every pixel of [`three_d_scene`], row-major, plus the count of pixels where
/// "tf" is in front of "te" and the count where "te" is in front of "tf" (both must be non-zero:
/// the planes intersect inside the frame).
pub fn three_d_oracle(pixels: &Pixels) -> (Vec<[f32; 4]>, [usize; 2]) {
    let (out, crossing, _) = oracle(pixels, false, false);
    (out, crossing)
}

/// Lane SMB-AE-3D-2: [`three_d_scene`] with "tf" under a 3D parent and "te" under a 2D parent
/// (`parented_specs`).
pub fn parented_scene(pixels: &Pixels) -> SceneComposition {
    let cam = camera(CAMERA_POSITION, [0.0; 3], 0.0, 0.0, 0.0, Some(CAMERA_POI), ZOOM, W, H).expect("camera");
    SceneComposition {
        composition_id: "three-d-parented".to_owned(),
        width: W,
        height: H,
        background: Some(BACKGROUND),
        filter: ResampleFilter::Bilinear,
        layers: expand_run(&planes_of(pixels, &parented_specs()), &cam, W, H, space(), 7).expect("run expands"),
    }
}
/// The ray-cast oracle of [`parented_scene`] (the parent applied by the oracle's own turns, the
/// ray solved against the sheared plane by Cramer's rule), plus the crossing counts.
pub fn parented_oracle(pixels: &Pixels) -> (Vec<[f32; 4]>, [usize; 2]) {
    let (out, crossing, _) = oracle_of(pixels, &parented_specs(), false, false);
    (out, crossing)
}

/// Lane SMB-AE-3D-2: "tf" carries a layer-space mask (texels `4..14 x 2..10`, applied before the
/// projection, so it follows the plane in perspective) and "te" a screen-space alpha track matte
/// (tile "ta" placed at (12, 8), rendered alone).
pub fn masked_scene(pixels: &Pixels) -> SceneComposition {
    let cam = camera(CAMERA_POSITION, [0.0; 3], 0.0, 0.0, 0.0, Some(CAMERA_POI), ZOOM, W, H).expect("camera");
    let mut planes = planes(pixels);
    planes[1].layer_mask = Some(tf_mask());
    let ta = pixels.get(&tile_id(2, "ta")).unwrap();
    planes[2].track_matte = Some(SceneMatte {
        layer: Some(Box::new(SceneLayer {
            layer_id: "SLYR-matte".to_owned(),
            content: SceneContent::Tiles(vec![ta.scene_tile(MATTE_AT.0, MATTE_AT.1)]),
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            fill_opacity: 1.0,
            matrix: Affine2::IDENTITY,
            matte: None,
            clipped: false,
        })),
        mode: SceneMatteMode::Alpha,
        inverted: false,
    });
    SceneComposition {
        composition_id: "three-d-masked".to_owned(),
        width: W,
        height: H,
        background: Some(BACKGROUND),
        filter: ResampleFilter::Bilinear,
        layers: expand_run(&planes, &cam, W, H, space(), 7).expect("run expands"),
    }
}
const MATTE_AT: (i64, i64) = (12, 8);
fn tf_mask() -> Vec<f32> {
    (0..14).flat_map(|k| (0..20).map(move |i| if (4..14).contains(&i) && (2..10).contains(&k) { 1.0 } else { 0.0 })).collect()
}
/// The oracle of [`masked_scene`]: tf's texels masked by its own texel test; te's opacity times the
/// matte tile's alpha at the pixel, quantised to 16 bits as the matte plane carries it.
pub fn masked_oracle(pixels: &Pixels) -> Vec<[f32; 4]> {
    let ta = pixels.get(&tile_id(2, "ta")).unwrap();
    let mut matte = Vec::new();
    for y in 0..i64::from(H) {
        for x in 0..i64::from(W) {
            let (u, v) = (x - MATTE_AT.0, y - MATTE_AT.1);
            let a = if u >= 0 && v >= 0 && u < i64::from(ta.width) && v < i64::from(ta.height) { ta.at(u as u32, v as u32)[3] } else { 0.0 };
            matte.push(((f64::from(a) * 65535.0).round() / 65535.0) as f32);
        }
    }
    oracle_full(pixels, &specs(), false, false, &[None, Some(tf_mask()), None], &[None, None, Some(matte)], &[]).0
}

/// Lane SMB-AE-3D-3: the corner pin of "tg" (its 8 x 8 content corners moved; content plan).
pub fn tg_pin() -> Projective2 {
    Projective2::from_quads([[0.0, 0.0], [8.0, 0.0], [8.0, 8.0], [0.0, 8.0]], [[1.0, -1.0], [9.5, 1.0], [8.0, 9.0], [-1.5, 7.0]]).unwrap()
}
/// Lane SMB-AE-3D-3: [`three_d_scene`] with "tg" corner-pinned (its plane textures the pinned
/// content over `three_d::pinned_bounds`) and "te" clipped to "tf": te's clip matte is tf drawn
/// alone (opacity 1) through the camera, as the composition adapter builds it.
pub fn pinned_clipped_scene(pixels: &Pixels) -> SceneComposition {
    let cam = camera(CAMERA_POSITION, [0.0; 3], 0.0, 0.0, 0.0, Some(CAMERA_POI), ZOOM, W, H).expect("camera");
    let mut planes = planes(pixels);
    let (pin, bounds) = hsk_studio_composite::three_d::pinned_bounds(&tg_pin(), planes[0].bounds).expect("pin");
    planes[0].content = SceneContent::Pinned(Box::new(hsk_studio_composite::composition::PinnedContent { content: planes[0].content.clone(), pin }));
    planes[0].bounds = bounds;
    let base = Plane3d { opacity: 1.0, ..planes[1].clone() };
    planes[2].track_matte = Some(SceneMatte {
        layer: expand_run(&[base], &cam, W, H, space(), 7).expect("base").into_iter().next().map(Box::new),
        mode: SceneMatteMode::Alpha,
        inverted: false,
    });
    SceneComposition {
        composition_id: "three-d-pinned-clipped".to_owned(),
        width: W,
        height: H,
        background: Some(BACKGROUND),
        filter: ResampleFilter::Bilinear,
        layers: expand_run(&planes, &cam, W, H, space(), 7).expect("run expands"),
    }
}
/// The oracle of [`pinned_clipped_scene`]: tg sampled through its pin then its own projection,
/// footprint = the oracle's integer cover of the pinned corners; te's opacity times tf's own
/// bilinear alpha at the pixel (in front of the camera, inside tf's grown rectangle), quantised to
/// 16 bits as the matte plane carries it. Also returns the count of pixels the clip matte covers
/// partly (0 < coverage < 1).
pub fn pinned_clipped_oracle(pixels: &Pixels) -> (Vec<[f32; 4]>, usize) {
    let specs = specs();
    let (r, d, f) = basis();
    let s = &specs[1];
    let t = pixels.get(&tile_id(s.layer, s.tile)).unwrap();
    let p = t.scene_tile(s.at.0, s.at.1).placement;
    let (x0, y0) = (p.x as f64, p.y as f64);
    let (x1, y1) = (x0 + f64::from(p.width), y0 + f64::from(p.height));
    let src = [[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
    let dst = src.map(|c| project(world(s, c[0] - s.at.0 as f64, c[1] - s.at.1 as f64)));
    let prep = PreparedTransform::new(&LayerTransform::projective(Projective2::from_quads(src, dst).unwrap(), ResampleFilter::Bilinear), p).unwrap();
    let mut matte = Vec::new();
    let mut partial = 0;
    for y in 0..i64::from(H) {
        for x in 0..i64::from(W) {
            let (sx, sy) = (x as f64 + 0.5, y as f64 + 0.5);
            let dir = add(add(scale(r, (sx - f64::from(W) / 2.0) / ZOOM), scale(d, (sy - f64::from(H) / 2.0) / ZOOM)), f);
            let a = match hit_any(s, CAMERA_POSITION, dir) {
                Some((tt, lx, ly)) if tt > 1e-3 && lx + x0 >= x0 - 1.0 && ly + y0 >= y0 - 1.0 && lx + x0 < x1 + 1.0 && ly + y0 < y1 + 1.0 => {
                    prep.sample_rgba(x, y, |u, v| Ok::<_, ()>(t.at(u, v))).unwrap()[3]
                }
                _ => 0.0,
            };
            let q = ((f64::from(a) * 65535.0).round() / 65535.0) as f32;
            if q > 0.0 && q < 1.0 {
                partial += 1;
            }
            matte.push(q);
        }
    }
    let out = oracle_full(pixels, &specs, false, false, &[], &[None, None, Some(matte)], &[Some(tg_pin()), None, None]).0;
    (out, partial)
}

/// Lane SMB-AE-3D-3 (motion blur): the X offsets of "tf" at the four shutter samples.
pub const BLUR_SHIFTS: [f64; 4] = [-1.5, -0.5, 0.5, 1.5];
fn shifted_specs(dx: f64) -> [Spec; 3] {
    let [tg, tf, te] = specs();
    let position = [tf.position[0] + dx, tf.position[1], tf.position[2]];
    [tg, Spec { position, ..tf }, te]
}
/// Lane SMB-AE-3D-3: the run with "tf" moving along X across the shutter, as the adapter emits a
/// motion-blurred run: one layer averaging the run expanded at each sample.
pub fn motion_blur_scene(pixels: &Pixels) -> SceneComposition {
    let cam = camera(CAMERA_POSITION, [0.0; 3], 0.0, 0.0, 0.0, Some(CAMERA_POI), ZOOM, W, H).expect("camera");
    let samples = BLUR_SHIFTS
        .iter()
        .enumerate()
        .map(|(k, dx)| SceneComposition {
            composition_id: format!("three-d-blur/{k}"),
            width: W,
            height: H,
            background: None,
            filter: ResampleFilter::Bilinear,
            layers: expand_run(&planes_of(pixels, &shifted_specs(*dx)), &cam, W, H, space(), 7).expect("run expands"),
        })
        .collect();
    SceneComposition {
        composition_id: "three-d-blur".to_owned(),
        width: W,
        height: H,
        background: Some(BACKGROUND),
        filter: ResampleFilter::Bilinear,
        layers: vec![SceneLayer {
            layer_id: "SLYR-3d-tg/motion_blur".to_owned(),
            content: SceneContent::Accumulated(Box::new(hsk_studio_composite::composition::AccumulatedContent { samples })),
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            fill_opacity: 1.0,
            matrix: Affine2::IDENTITY,
            matte: None,
            clipped: false,
        }],
    }
}
/// The oracle of [`motion_blur_scene`]: the mean of the ray-cast oracle frames of the four shifted
/// runs (each over the opaque background, so the mean of the straight colours is the mean of the
/// premultiplied ones); also the count of pixels the motion changes.
pub fn motion_blur_oracle(pixels: &Pixels) -> (Vec<[f32; 4]>, usize) {
    let frames: Vec<Vec<[f32; 4]>> = BLUR_SHIFTS.iter().map(|dx| oracle_of(pixels, &shifted_specs(*dx), false, false).0).collect();
    let n = frames[0].len();
    let mut moved = 0;
    let out = (0..n)
        .map(|i| {
            if frames.iter().any(|f| (0..4).any(|c| (f[i][c] - frames[0][i][c]).abs() > 1e-4)) {
                moved += 1;
            }
            let mut m = [0.0f64; 4];
            for f in &frames {
                for c in 0..3 {
                    m[c] += f64::from(f[i][c]) * f64::from(f[i][3]);
                }
                m[3] += f64::from(f[i][3]);
            }
            let a = m[3] / frames.len() as f64;
            [(m[0] / frames.len() as f64 / a) as f32, (m[1] / frames.len() as f64 / a) as f32, (m[2] / frames.len() as f64 / a) as f32, a as f32]
        })
        .collect();
    (out, moved)
}

/// The lens of [`dof_scene`]: focus near "tf" / "te", "tg" behind it out of focus.
pub const LENS: DepthOfField = DepthOfField { focus_distance: 61.0, aperture: 30.0, blur_level: 1.0 };
/// The unlit run under a camera with [`LENS`].
pub fn dof_scene(pixels: &Pixels) -> SceneComposition {
    let mut cam = camera(CAMERA_POSITION, [0.0; 3], 0.0, 0.0, 0.0, Some(CAMERA_POI), ZOOM, W, H).expect("camera");
    cam.dof = Some(LENS);
    SceneComposition {
        composition_id: "three-d-dof".to_owned(),
        width: W,
        height: H,
        background: Some(BACKGROUND),
        filter: ResampleFilter::Bilinear,
        layers: expand_run_lit(&planes(pixels), &[], &[], &cam, W, H, space(), 7).expect("run expands"),
    }
}
/// The oracle of [`dof_scene`], plus the largest layer-space defocus radius.
pub fn dof_oracle(pixels: &Pixels) -> (Vec<[f32; 4]>, f64) {
    let (out, _, r) = oracle(pixels, false, true);
    (out, r)
}

/// Layer-space defocus radius of plane `s` at layer point (x, y): screen circle of confusion over
/// the local screen scale, from the analytic Jacobian of the pinhole projection.
fn radius_at(s: &Spec, x: f64, y: f64) -> f64 {
    let (r, d, f) = basis();
    let q = sub(world(s, x, y), CAMERA_POSITION);
    let (qx, qy, qz) = (dot(q, r), dot(q, d), dot(q, f));
    let ex = scale(rotate(s, [1.0, 0.0, 0.0]), s.scale[0]);
    let ey = scale(rotate(s, [0.0, 1.0, 0.0]), s.scale[1]);
    let j = |e: V| -> (f64, f64) {
        let (dx, dy, dz) = (dot(e, r), dot(e, d), dot(e, f));
        (ZOOM * (dx * qz - qx * dz) / (qz * qz), ZOOM * (dy * qz - qy * dz) / (qz * qz))
    };
    let (a, c) = j(ex);
    let (b, e) = j(ey);
    let screen_scale = (a * e - b * c).abs().sqrt();
    let coc = LENS.blur_level * LENS.aperture * ZOOM * (qz - LENS.focus_distance).abs() / (2.0 * qz * LENS.focus_distance);
    coc / screen_scale
}
/// Fast-rectangle gather blur of one plane's texels (straight in, straight out).
fn blur_plane(tex: &[[f32; 4]], w: usize, h: usize, radius: &dyn Fn(usize, usize) -> f64) -> Vec<[f32; 4]> {
    let cov = |d: f64, r: f64| (r + 0.5 - d).clamp(0.0, 1.0);
    let mut out = Vec::with_capacity(tex.len());
    for y in 0..h {
        for x in 0..w {
            let r = radius(x, y);
            let reach = (r + 0.5).ceil() as i64;
            let (mut sw, mut sa, mut sc) = (0.0f64, 0.0f64, [0.0f64; 3]);
            for dy in -reach..=reach {
                for dx in -reach..=reach {
                    let wt = cov(dx.abs() as f64, r) * cov(dy.abs() as f64, r);
                    if wt == 0.0 {
                        continue;
                    }
                    sw += wt;
                    let (xx, yy) = (x as i64 + dx, y as i64 + dy);
                    if xx < 0 || yy < 0 || xx >= w as i64 || yy >= h as i64 {
                        continue;
                    }
                    let t = tex[yy as usize * w + xx as usize];
                    let a = f64::from(t[3]);
                    sa += wt * a;
                    for c in 0..3 {
                        sc[c] += wt * a * f64::from(t[c]);
                    }
                }
            }
            let rgb = |c: usize| if sa > 0.0 { (sc[c] / sa) as f32 } else { 0.0 };
            out.push([rgb(0), rgb(1), rgb(2), (sa / sw) as f32]);
        }
    }
    out
}

/// The lit run's materials (bottom first: tg, tf, te).
pub fn materials() -> [Material; 3] {
    let base = Material { accepts_shadows: true, ..Material::DEFAULT };
    [
        base,
        Material { casts_shadows: true, diffuse: 0.8, specular_intensity: 0.6, specular_shininess: 20.0, metal: 0.3, ..base },
        Material { casts_shadows: true, light_transmission: 0.25, ambient: 0.7, ..base },
    ]
}
fn unit3(v: [f64; 3]) -> [f64; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    [v[0] / l, v[1] / l, v[2] / l]
}
/// Ambient, point (shadows), spot and parallel lights.
pub fn lights() -> Vec<Light> {
    let l = |id: &str, kind: LightKind, colour: [f64; 3], intensity: f64, position: [f64; 3], target: [f64; 3]| Light {
        layer_id: id.to_owned(),
        kind,
        colour,
        intensity,
        position,
        direction: unit3(sub(target, position)),
        cone_angle: 60.0,
        cone_feather: 0.5,
        falloff: Falloff::None,
        falloff_start: 0.0,
        falloff_distance: 500.0,
        casts_shadows: false,
        shadow_darkness: 1.0,
    };
    vec![
        l("SLYR-ambient", LightKind::Ambient, [1.0, 0.9, 0.8], 0.3, [0.0; 3], [0.0, 0.0, 1.0]),
        Light { falloff: Falloff::Smooth, falloff_start: 20.0, falloff_distance: 80.0, casts_shadows: true, shadow_darkness: 0.8, ..l("SLYR-point", LightKind::Point, [1.0, 1.0, 1.0], 1.2, [14.0, 6.0, -30.0], [16.0, 12.0, 0.0]) },
        l("SLYR-spot", LightKind::Spot, [0.6, 0.8, 1.0], 0.9, [30.0, 0.0, -40.0], [16.0, 12.0, 0.0]),
        l("SLYR-parallel", LightKind::Parallel, [1.0, 1.0, 1.0], 0.4, [0.0, 0.0, 0.0], [0.3, 0.5, 1.0]),
    ]
}
/// The run with [`lights`] and [`materials`].
pub fn lit_scene(pixels: &Pixels) -> SceneComposition {
    let cam = camera(CAMERA_POSITION, [0.0; 3], 0.0, 0.0, 0.0, Some(CAMERA_POI), ZOOM, W, H).expect("camera");
    SceneComposition {
        composition_id: "three-d-lit".to_owned(),
        width: W,
        height: H,
        background: Some(BACKGROUND),
        filter: ResampleFilter::Bilinear,
        layers: expand_run_lit(&planes(pixels), &materials(), &lights(), &cam, W, H, space(), 7).expect("run expands"),
    }
}
/// [`three_d_oracle`] of [`lit_scene`], plus the count of texels a shadow darkens.
pub fn lit_oracle(pixels: &Pixels) -> (Vec<[f32; 4]>, usize) {
    let (out, _, _) = oracle(pixels, true, false);
    (out, shadowed_texels(pixels))
}

fn smooth(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
/// Alpha of plane `j` at layer point (lx, ly), nearest texel.
fn alpha_at(pixels: &Pixels, specs: &[Spec], j: usize, lx: f64, ly: f64) -> f64 {
    let s = &specs[j];
    let t = pixels.get(&tile_id(s.layer, s.tile)).unwrap();
    let (i, k) = ((lx + s.at.0 as f64).floor() - s.at.0 as f64, (ly + s.at.1 as f64).floor() - s.at.1 as f64);
    if i < 0.0 || k < 0.0 || i >= f64::from(t.width) || k >= f64::from(t.height) {
        return 0.0;
    }
    f64::from(t.at(i as u32, k as u32)[3])
}
/// Layer point of world point `w` on plane `s`, and the ray parameter of `p + t u` hitting it.
fn hit(s: &Spec, p: V, u: V) -> Option<(f64, f64, f64)> {
    let ex = rotate(s, [1.0, 0.0, 0.0]);
    let ey = rotate(s, [0.0, 1.0, 0.0]);
    let ez = rotate(s, [0.0, 0.0, 1.0]);
    let c = sub(p, s.position);
    let den = dot(u, ez);
    if den.abs() < 1e-12 {
        return None;
    }
    let t = (-s.anchor[2] - dot(c, ez)) / den;
    let q = add(c, scale(u, t));
    Some((t, dot(q, ex) / s.scale[0] + s.anchor[0], dot(q, ey) / s.scale[1] + s.anchor[1]))
}
/// Shaded straight RGB of plane `i` at layer point (x, y) with albedo `a`; `shadowed` set when a
/// shadow darkens it.
fn shade_texel(pixels: &Pixels, specs: &[Spec], i: usize, x: f64, y: f64, a: [f32; 3], shadowed: &mut bool) -> [f32; 3] {
    let s = &specs[i];
    let m = materials()[i];
    let p = world(s, x, y);
    let mut n = rotate(s, [0.0, 0.0, 1.0]);
    let e = sub(CAMERA_POSITION, p);
    if dot(n, e) < 0.0 {
        n = scale(n, -1.0);
    }
    let v = unit(e);
    let mut amb = [0.0; 3];
    let mut dif = [0.0; 3];
    let mut spe = [0.0; 3];
    for l in lights() {
        let c = scale(l.colour, l.intensity);
        if l.kind == LightKind::Ambient {
            amb = add(amb, c);
            continue;
        }
        let (lv, dist) = if l.kind == LightKind::Parallel {
            (scale(l.direction, -1.0), f64::INFINITY)
        } else {
            let d = sub(l.position, p);
            (unit(d), dot(d, d).sqrt())
        };
        let att = match (l.kind, l.falloff) {
            (LightKind::Parallel, _) | (_, Falloff::None) => 1.0,
            (_, Falloff::Smooth) => {
                if dist <= l.falloff_start { 1.0 } else { 1.0 - smooth((dist - l.falloff_start) / l.falloff_distance) }
            }
            (_, Falloff::InverseSquareClamped) => {
                if dist <= l.falloff_start { 1.0 } else { (l.falloff_start / dist).powi(2) }
            }
        };
        let cone = if l.kind == LightKind::Spot {
            let ang = dot(scale(lv, -1.0), l.direction).clamp(-1.0, 1.0).acos().to_degrees();
            let half = l.cone_angle / 2.0;
            let inner = half * (1.0 - l.cone_feather);
            if ang <= inner { 1.0 } else if ang >= half { 0.0 } else { smooth((half - ang) / (half - inner)) }
        } else {
            1.0
        };
        let ndl = dot(n, lv);
        if ndl <= 0.0 || att * cone == 0.0 {
            continue;
        }
        let mut sh = 1.0;
        if l.casts_shadows && m.accepts_shadows {
            for j in 0..specs.len() {
                if j == i || !materials()[j].casts_shadows {
                    continue;
                }
                let Some((t, lx, ly)) = hit(&specs[j], p, lv) else { continue };
                if !(t > 1e-9 && t < dist) {
                    continue;
                }
                let al = alpha_at(pixels, specs, j, lx, ly);
                if al > 0.0 {
                    *shadowed = true;
                }
                sh *= 1.0 - l.shadow_darkness * al * specs[j].opacity * (1.0 - materials()[j].light_transmission);
            }
        }
        let k = att * cone * sh;
        let r = sub(scale(n, 2.0 * ndl), lv);
        let sp = dot(r, v).max(0.0).powf(1.0 + m.specular_shininess);
        dif = add(dif, scale(c, k * ndl));
        spe = add(spe, scale(c, k * sp));
    }
    let mut out = [0.0f32; 3];
    for ch in 0..3 {
        let al = f64::from(a[ch]);
        out[ch] = (al * (m.ambient * amb[ch] + m.diffuse * dif[ch]) + m.specular_intensity * spe[ch] * (1.0 + m.metal * (al - 1.0))).clamp(0.0, 1.0) as f32;
    }
    out
}
/// Texels of every plane, shaded or not: `[plane][row-major texel]`.
fn texels(pixels: &Pixels, specs: &[Spec], lit: bool) -> Vec<Vec<[f32; 4]>> {
    specs
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let t = pixels.get(&tile_id(s.layer, s.tile)).unwrap();
            let mut v = Vec::new();
            for k in 0..t.height {
                for j in 0..t.width {
                    let c = t.at(j, k);
                    if !lit {
                        v.push(c);
                        continue;
                    }
                    let mut sh = false;
                    let rgb = shade_texel(pixels, specs, i, f64::from(j) + 0.5, f64::from(k) + 0.5, [c[0], c[1], c[2]], &mut sh);
                    v.push([rgb[0], rgb[1], rgb[2], c[3]]);
                }
            }
            v
        })
        .collect()
}
fn shadowed_texels(pixels: &Pixels) -> usize {
    let specs = specs();
    let mut n = 0;
    for (i, s) in specs.iter().enumerate() {
        let t = pixels.get(&tile_id(s.layer, s.tile)).unwrap();
        for k in 0..t.height {
            for j in 0..t.width {
                let mut sh = false;
                let c = t.at(j, k);
                shade_texel(pixels, &specs, i, f64::from(j) + 0.5, f64::from(k) + 0.5, [c[0], c[1], c[2]], &mut sh);
                n += usize::from(sh && c[3] > 0.0);
            }
        }
    }
    n
}

fn oracle(pixels: &Pixels, lit: bool, dof: bool) -> (Vec<[f32; 4]>, [usize; 2], f64) {
    oracle_of(pixels, &specs(), lit, dof)
}
fn oracle_of(pixels: &Pixels, specs: &[Spec], lit: bool, dof: bool) -> (Vec<[f32; 4]>, [usize; 2], f64) {
    oracle_full(pixels, specs, lit, dof, &[], &[], &[])
}
/// `masks[i]`: plane `i`'s layer-space texel coverage (multiplied into its texel alpha before the
/// sample); `mattes[i]`: plane `i`'s screen coverage (multiplied into its opacity per pixel).
/// `pins[i]` (lane SMB-AE-3D-3): plane `i`'s corner pin (content plan -> content plan); its texture
/// is the pinned content and its footprint the oracle's own integer cover of the pinned corners.
fn oracle_full(pixels: &Pixels, specs: &[Spec], lit: bool, dof: bool, masks: &[Option<Vec<f32>>], mattes: &[Option<Vec<f32>>], pins: &[Option<Projective2>]) -> (Vec<[f32; 4]>, [usize; 2], f64) {
    let (r, d, f) = basis();
    let mut shaded = texels(pixels, specs, lit);
    for (i, m) in masks.iter().enumerate() {
        if let Some(m) = m {
            for (t, c) in shaded[i].iter_mut().zip(m) {
                t[3] *= c;
            }
        }
    }
    let mut max_radius = 0.0f64;
    if dof {
        for (i, s) in specs.iter().enumerate() {
            let t = pixels.get(&tile_id(s.layer, s.tile)).unwrap();
            let radius = |x: usize, y: usize| radius_at(s, x as f64 + 0.5, y as f64 + 0.5);
            for y in 0..t.height as usize {
                for x in 0..t.width as usize {
                    max_radius = max_radius.max(radius(x, y));
                }
            }
            shaded[i] = blur_plane(&shaded[i], t.width as usize, t.height as usize, &radius);
        }
    }
    let mut draws = Vec::new();
    for s in specs {
        let t = pixels.get(&tile_id(s.layer, s.tile)).unwrap();
        let p = t.scene_tile(s.at.0, s.at.1).placement;
        let (x0, y0) = (p.x as f64, p.y as f64);
        let (x1, y1) = (x0 + f64::from(p.width), y0 + f64::from(p.height));
        let src = [[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
        let dst = src.map(|c| project(world(s, c[0] - s.at.0 as f64, c[1] - s.at.1 as f64)));
        let mut map = Projective2::from_quads(src, dst).expect("projected quad");
        let mut foot = p;
        if let Some(Some(pin)) = pins.get(draws.len()) {
            map = pin.then(&map);
            let c = src.map(|c| {
                let (x, y, w) = pin.apply_h(c[0], c[1]);
                [x / w, y / w]
            });
            let lo = [c.iter().map(|v| v[0]).fold(f64::INFINITY, f64::min).floor(), c.iter().map(|v| v[1]).fold(f64::INFINITY, f64::min).floor()];
            let hi = [c.iter().map(|v| v[0]).fold(f64::NEG_INFINITY, f64::max).ceil(), c.iter().map(|v| v[1]).fold(f64::NEG_INFINITY, f64::max).ceil()];
            foot = hsk_studio_pigment::Rect { x: lo[0] as i64, y: lo[1] as i64, width: (hi[0] - lo[0]) as u32, height: (hi[1] - lo[1]) as u32 };
        }
        let prep = PreparedTransform::new(&LayerTransform::projective(map, ResampleFilter::Bilinear), p).unwrap();
        draws.push((t, prep, foot));
    }
    let mut out = Vec::new();
    let mut crossing = [0usize; 2];
    for y in 0..i64::from(H) {
        for x in 0..i64::from(W) {
            let (sx, sy) = (x as f64 + 0.5, y as f64 + 0.5);
            let dir = add(add(scale(r, (sx - f64::from(W) / 2.0) / ZOOM), scale(d, (sy - f64::from(H) / 2.0) / ZOOM)), f);
            let mut hits: Vec<(f64, usize)> = Vec::new();
            for (i, s) in specs.iter().enumerate() {
                // Layer frame: u = R^T (p - position); the plane is u.z = -anchor_z.
                let inv = |v: V| -> V {
                    let ex = rotate(s, [1.0, 0.0, 0.0]);
                    let ey = rotate(s, [0.0, 1.0, 0.0]);
                    let ez = rotate(s, [0.0, 0.0, 1.0]);
                    [dot(v, ex), dot(v, ey), dot(v, ez)]
                };
                let (t, lx, ly) = if s.parent.is_some() {
                    // Lane SMB-AE-3D-2: a parented (possibly sheared) plane.
                    let Some((t, x, y)) = hit_any(s, CAMERA_POSITION, dir) else { continue };
                    (t, x + s.at.0 as f64, y + s.at.1 as f64)
                } else {
                    let c = inv(sub(CAMERA_POSITION, s.position));
                    let k = inv(dir);
                    let t = (-s.anchor[2] - c[2]) / k[2];
                    let u = add(c, scale(k, t));
                    (t, u[0] / s.scale[0] + s.anchor[0] + s.at.0 as f64, u[1] / s.scale[1] + s.anchor[1] + s.at.1 as f64)
                };
                if t.is_nan() || t <= 1e-3 {
                    continue;
                }
                let p = draws[i].2;
                let inside = lx >= p.x as f64 - 1.0 && ly >= p.y as f64 - 1.0 && lx < p.x as f64 + f64::from(p.width) + 1.0 && ly < p.y as f64 + f64::from(p.height) + 1.0;
                if inside {
                    // The camera depth of the hit is `t` (the ray's forward component is 1).
                    hits.push((t, i));
                }
            }
            hits.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
            let order: Vec<usize> = hits.iter().map(|h| h.1).collect();
            if let (Some(a), Some(b)) = (order.iter().position(|i| *i == 1), order.iter().position(|i| *i == 2)) {
                crossing[usize::from(a < b)] += 1;
            }
            let b = BACKGROUND;
            let mut acc = composite_over(StudioBlendMode::Normal, Rgba::TRANSPARENT, Rgba::new([b[0], b[1], b[2]], b[3]), 1.0, PixelSite::default()).unwrap();
            for i in order {
                let (tile, prep, _) = &draws[i];
                let tex = &shaded[i];
                let s = prep.sample_rgba(x, y, |u, v| Ok::<_, ()>(tex[(v * tile.width + u) as usize])).unwrap();
                let cov = mattes.get(i).and_then(|m| m.as_ref()).map_or(1.0, |m| m[(y * i64::from(W) + x) as usize]);
                acc = composite_over(StudioBlendMode::Normal, acc, Rgba::new([s[0], s[1], s[2]], s[3]), specs[i].opacity as f32 * cov, PixelSite { x, y, seed: 0 }).unwrap();
            }
            out.push([acc.rgb[0], acc.rgb[1], acc.rgb[2], acc.a]);
        }
    }
    (out, crossing, max_radius)
}

/// Lane SMB-AE-3D-2 (STU-CMP-050): the planes and camera of [`three_d_scene`].
pub fn aux_inputs(pixels: &Pixels) -> (Vec<Plane3d>, hsk_studio_composite::three_d::Camera) {
    (planes(pixels), camera(CAMERA_POSITION, [0.0; 3], 0.0, 0.0, 0.0, Some(CAMERA_POI), ZOOM, W, H).expect("camera"))
}
/// Lane SMB-AE-3D-3: every plane's content alpha of [`aux_inputs`] read straight from the fixture
/// tiles (no backend), row-major over the plane's rectangle.
pub fn tile_alphas(pixels: &Pixels) -> Vec<Vec<f32>> {
    specs()
        .iter()
        .map(|s| {
            let t = pixels.get(&tile_id(s.layer, s.tile)).unwrap();
            (0..t.height).flat_map(|v| (0..t.width).map(move |u| (u, v))).map(|(u, v)| t.at(u, v)[3]).collect()
        })
        .collect()
}
/// Per pixel (row-major): the nearest plane whose nearest texel under the camera ray has
/// `alpha x opacity > 0` (exact content rectangle): its camera depth, its camera-space unit normal
/// facing the camera, and its object id (3, 2, 1 for tg, tf, te); `(inf, 0, 0)` where none.
pub fn aux_oracle(pixels: &Pixels) -> Vec<(f64, [f64; 3], f32)> {
    let (r, d, f) = basis();
    let specs = specs();
    let mut out = Vec::new();
    for y in 0..H {
        for x in 0..W {
            let (sx, sy) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
            let dir = add(add(scale(r, (sx - f64::from(W) / 2.0) / ZOOM), scale(d, (sy - f64::from(H) / 2.0) / ZOOM)), f);
            let mut best: Option<(f64, usize)> = None;
            for (i, s) in specs.iter().enumerate() {
                let Some((t, lx, ly)) = hit_any(s, CAMERA_POSITION, dir) else { continue };
                let t3 = pixels.get(&tile_id(s.layer, s.tile)).unwrap();
                if !(t > 1e-3 && lx >= 0.0 && ly >= 0.0 && lx < f64::from(t3.width) && ly < f64::from(t3.height)) {
                    continue;
                }
                if f64::from(t3.at(lx.floor() as u32, ly.floor() as u32)[3]) * s.opacity <= 0.0 {
                    continue;
                }
                if best.is_none_or(|(b, _)| t <= b) {
                    best = Some((t, i));
                }
            }
            out.push(match best {
                None => (f64::INFINITY, [0.0; 3], 0.0),
                Some((t, i)) => {
                    let n = rotate(&specs[i], [0.0, 0.0, 1.0]);
                    let mut nc = [dot(n, r), dot(n, d), dot(n, f)];
                    let qc = [t * (sx - f64::from(W) / 2.0) / ZOOM, t * (sy - f64::from(H) / 2.0) / ZOOM, t];
                    if dot(nc, qc) > 0.0 {
                        nc = scale(nc, -1.0);
                    }
                    (t, unit(nc), (3 - i) as f32)
                }
            });
        }
    }
    out
}
/// An independent minimal OpenEXR reader for the writer's subset (single part, scanline,
/// uncompressed, FLOAT channels): parses the header attribute by attribute and returns every
/// channel's plane.
pub fn read_exr_f32(b: &[u8]) -> Vec<(String, Vec<f32>)> {
    let i32_at = |o: usize| i32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    assert_eq!(i32_at(0), 20000630, "magic");
    assert_eq!(i32_at(4), 2, "version 2, single part scanline");
    let cstr = |o: usize| -> (String, usize) {
        let e = o + b[o..].iter().position(|c| *c == 0).unwrap();
        (String::from_utf8(b[o..e].to_vec()).unwrap(), e + 1)
    };
    let (mut o, mut names, mut window) = (8usize, Vec::new(), [0i32; 4]);
    loop {
        if b[o] == 0 {
            o += 1;
            break;
        }
        let (name, n) = cstr(o);
        let (kind, n) = cstr(n);
        let size = i32_at(n) as usize;
        let v = n + 4;
        match (name.as_str(), kind.as_str()) {
            ("channels", "chlist") => {
                let mut c = v;
                while b[c] != 0 {
                    let (ch, n) = cstr(c);
                    assert_eq!(i32_at(n), 2, "FLOAT");
                    assert_eq!((i32_at(n + 8), i32_at(n + 12)), (1, 1), "sampling");
                    names.push(ch);
                    c = n + 16;
                }
            }
            ("compression", _) => assert_eq!(b[v], 0, "uncompressed"),
            ("dataWindow", "box2i") => window = [i32_at(v), i32_at(v + 4), i32_at(v + 8), i32_at(v + 12)],
            _ => {}
        }
        o = v + size;
    }
    let (w, h) = ((window[2] - window[0] + 1) as usize, (window[3] - window[1] + 1) as usize);
    let mut planes: Vec<(String, Vec<f32>)> = names.iter().map(|n| (n.clone(), Vec::new())).collect();
    for y in 0..h {
        let at = u64::from_le_bytes(b[o + 8 * y..o + 8 * y + 8].try_into().unwrap()) as usize;
        assert_eq!(i32_at(at), y as i32);
        assert_eq!(i32_at(at + 4) as usize, w * 4 * names.len());
        let mut p = at + 8;
        for plane in &mut planes {
            for _ in 0..w {
                plane.1.push(f32::from_le_bytes(b[p..p + 4].try_into().unwrap()));
                p += 4;
            }
        }
    }
    planes
}

/// Lane SMB-AE-3D-2 (STU-CMP-055 "plane to sphere"): the equirectangular frame size of
/// [`immersive_scene`] (one degree per pixel).
pub const EW: u32 = 360;
pub const EH: u32 = 180;
/// The run's planes, unlit, under the two-node camera, as one immersive scene.
pub fn immersive_scene(pixels: &Pixels) -> hsk_studio_composite::three_d::immersive::ImmersiveScene {
    hsk_studio_composite::three_d::immersive::ImmersiveScene {
        width: EW,
        height: EH,
        background: Some(BACKGROUND),
        planes: planes(pixels),
        materials: Vec::new(),
        lights: Vec::new(),
        camera: camera(CAMERA_POSITION, [0.0; 3], 0.0, 0.0, 0.0, Some(CAMERA_POI), ZOOM, W, H).expect("camera"),
    }
}
/// The oracle of [`immersive_scene`]: per output pixel its own longitude / latitude direction in
/// the oracle's camera basis, the ray solved against every plane (`hit_any`), the tile sampled
/// bilinearly in premultiplied colour at texel centres (transparent outside), the hits
/// composited farthest first over the background.
pub fn immersive_oracle(pixels: &Pixels) -> Vec<[f32; 4]> {
    let (r, d, f) = basis();
    let specs = specs();
    let mut out = Vec::new();
    for y in 0..EH {
        for x in 0..EW {
            let lon = 2.0 * std::f64::consts::PI * ((f64::from(x) + 0.5) / f64::from(EW) - 0.5);
            let lat = std::f64::consts::PI * (0.5 - (f64::from(y) + 0.5) / f64::from(EH));
            let c = [lat.cos() * lon.sin(), -lat.sin(), lat.cos() * lon.cos()];
            let dir = add(add(scale(r, c[0]), scale(d, c[1])), scale(f, c[2]));
            let mut hits: Vec<(f64, usize, [f64; 4])> = Vec::new();
            for (i, s) in specs.iter().enumerate() {
                let Some((t, lx, ly)) = hit_any(s, CAMERA_POSITION, dir) else { continue };
                if t <= 1e-3 {
                    continue;
                }
                let tile = pixels.get(&tile_id(s.layer, s.tile)).unwrap();
                let (fx, fy) = (lx - 0.5, ly - 0.5);
                let (x0, y0) = (fx.floor(), fy.floor());
                let mut acc = [0.0f64; 4];
                for (dy, wy) in [(0.0, 1.0 - (fy - y0)), (1.0, fy - y0)] {
                    for (dx, wx) in [(0.0, 1.0 - (fx - x0)), (1.0, fx - x0)] {
                        let (ix, iy) = (x0 + dx, y0 + dy);
                        if ix < 0.0 || iy < 0.0 || ix >= f64::from(tile.width) || iy >= f64::from(tile.height) {
                            continue;
                        }
                        let t4 = tile.at(ix as u32, iy as u32);
                        let a = f64::from(t4[3]);
                        for k in 0..3 {
                            acc[k] += wx * wy * a * f64::from(t4[k]);
                        }
                        acc[3] += wx * wy * a;
                    }
                }
                if acc[3] > 0.0 {
                    hits.push((t, i, acc));
                }
            }
            hits.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
            let b = BACKGROUND;
            let mut acc = composite_over(StudioBlendMode::Normal, Rgba::TRANSPARENT, Rgba::new([b[0], b[1], b[2]], b[3]), 1.0, PixelSite { x: i64::from(x), y: i64::from(y), seed: 0 }).unwrap();
            for (_, i, v) in hits {
                let src = Rgba::new([(v[0] / v[3]) as f32, (v[1] / v[3]) as f32, (v[2] / v[3]) as f32], v[3] as f32);
                acc = composite_over(StudioBlendMode::Normal, acc, src, specs[i].opacity as f32, PixelSite { x: i64::from(x), y: i64::from(y), seed: 0 }).unwrap();
            }
            out.push([acc.rgb[0], acc.rgb[1], acc.rgb[2], acc.a]);
        }
    }
    out
}
/// Straight RGBA of a composite `Surface` (row-major).
pub fn surface_pixels(s: &hsk_studio_composite::Surface) -> Vec<[f32; 4]> {
    let f = |b: &[u8], i: usize| f32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap());
    (0..(s.width * s.height) as usize).map(|i| [f(&s.colour, i * 3), f(&s.colour, i * 3 + 1), f(&s.colour, i * 3 + 2), f(&s.alpha, i)]).collect()
}
/// Largest difference on alpha and premultiplied colour.
pub fn max_delta_px(a: &[[f32; 4]], b: &[[f32; 4]]) -> f32 {
    a.iter().zip(b).map(|(p, q)| (0..4).map(|k| if k == 3 { (p[3] - q[3]).abs() } else { (p[k] * p[3] - q[k] * q[3]).abs() }).fold(0.0, f32::max)).fold(0.0, f32::max)
}
/// The sphere-rotate oracle: output direction `d` reads the source at `R^T d` (`R` the X, then
/// Y, then Z turns), bilinear in premultiplied colour, longitude wrapped, latitude clamped.
pub fn sphere_rotate_oracle(src: &[[f32; 4]], w: u32, h: u32, rot: [f64; 3]) -> Vec<[f32; 4]> {
    let inv = |v: V| -> V {
        let mut v = v;
        for (axis, deg) in [(2, -rot[2]), (1, -rot[1]), (0, -rot[0])] {
            v = turn(v, axis, deg);
        }
        v
    };
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let lon = 2.0 * std::f64::consts::PI * ((f64::from(x) + 0.5) / f64::from(w) - 0.5);
            let lat = std::f64::consts::PI * (0.5 - (f64::from(y) + 0.5) / f64::from(h));
            let s = inv([lat.cos() * lon.sin(), -lat.sin(), lat.cos() * lon.cos()]);
            let (slon, slat) = (s[0].atan2(s[2]), (-s[1]).clamp(-1.0, 1.0).asin());
            let u = (slon / (2.0 * std::f64::consts::PI) + 0.5) * f64::from(w) - 0.5;
            let v = (0.5 - slat / std::f64::consts::PI) * f64::from(h) - 0.5;
            let (u0, v0) = (u.floor(), v.floor());
            let mut acc = [0.0f64; 4];
            for (dy, wy) in [(0i64, 1.0 - (v - v0)), (1, v - v0)] {
                for (dx, wx) in [(0i64, 1.0 - (u - u0)), (1, u - u0)] {
                    let ix = (u0 as i64 + dx).rem_euclid(i64::from(w));
                    let iy = (v0 as i64 + dy).clamp(0, i64::from(h) - 1);
                    let p = src[(iy * i64::from(w) + ix) as usize];
                    let a = f64::from(p[3]);
                    for k in 0..3 {
                        acc[k] += wx * wy * a * f64::from(p[k]);
                    }
                    acc[3] += wx * wy * a;
                }
            }
            let rgb = |k: usize| if acc[3] > 0.0 { (acc[k] / acc[3]) as f32 } else { 0.0 };
            out.push([rgb(0), rgb(1), rgb(2), acc[3] as f32]);
        }
    }
    out
}
