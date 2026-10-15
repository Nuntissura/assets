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
#![allow(dead_code)]
use crate::scenes::{BACKGROUND, Pixels, space, tile_id};
use hsk_studio_composite::three_d::{Plane3d, Transform3d, camera, expand_run, layer_rotation};
use hsk_studio_composite::{SceneComposition, SceneContent};
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
}
fn specs() -> [Spec; 3] {
    [
        Spec { tile: "tg", layer: 12, at: (0, 0), anchor: [4.0, 4.0, 0.0], position: [20.0, 10.0, 25.0], scale: [1.5, 1.25], orientation: [0.0, 0.0, 0.0], rot: [0.0, 0.0, 20.0], opacity: 1.0 },
        Spec { tile: "tf", layer: 11, at: (3, 2), anchor: [10.0, 7.0, 0.0], position: [16.0, 12.0, 0.0], scale: [1.0, 1.0], orientation: [0.0, 0.0, 0.0], rot: [0.0, 35.0, 0.0], opacity: 0.9 },
        Spec { tile: "te", layer: 10, at: (0, 0), anchor: [5.0, 4.0, 1.5], position: [15.0, 13.0, 1.0], scale: [1.4, 1.4], orientation: [0.0, 0.0, 10.0], rot: [-40.0, 0.0, 0.0], opacity: 1.0 },
    ]
}

/// The run's planes, bottom first.
pub fn planes(pixels: &Pixels) -> Vec<Plane3d> {
    specs()
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
                },
                mode: StudioBlendMode::Normal,
                opacity: s.opacity,
                fill_opacity: 1.0,
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
/// World point of layer point (x, y).
fn world(s: &Spec, x: f64, y: f64) -> V {
    add(s.position, rotate(s, [s.scale[0] * (x - s.anchor[0]), s.scale[1] * (y - s.anchor[1]), -s.anchor[2]]))
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
    let (r, d, f) = basis();
    let specs = specs();
    let mut draws = Vec::new();
    for s in &specs {
        let t = pixels.get(&tile_id(s.layer, s.tile)).unwrap();
        let p = t.scene_tile(s.at.0, s.at.1).placement;
        let (x0, y0) = (p.x as f64, p.y as f64);
        let (x1, y1) = (x0 + f64::from(p.width), y0 + f64::from(p.height));
        let src = [[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
        let dst = src.map(|c| project(world(s, c[0] - s.at.0 as f64, c[1] - s.at.1 as f64)));
        let map = Projective2::from_quads(src, dst).expect("projected quad");
        let prep = PreparedTransform::new(&LayerTransform::projective(map, ResampleFilter::Bilinear), p).unwrap();
        draws.push((t, prep, p));
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
                let c = inv(sub(CAMERA_POSITION, s.position));
                let k = inv(dir);
                let t = (-s.anchor[2] - c[2]) / k[2];
                if t.is_nan() || t <= 1e-3 {
                    continue;
                }
                let u = add(c, scale(k, t));
                let lx = u[0] / s.scale[0] + s.anchor[0] + s.at.0 as f64;
                let ly = u[1] / s.scale[1] + s.anchor[1] + s.at.1 as f64;
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
                let s = prep.sample_rgba(x, y, |u, v| Ok::<_, ()>(tile.at(u, v))).unwrap();
                acc = composite_over(StudioBlendMode::Normal, acc, Rgba::new([s[0], s[1], s[2]], s[3]), specs[i].opacity as f32, PixelSite { x, y, seed: 0 }).unwrap();
            }
            out.push([acc.rgb[0], acc.rgb[1], acc.rgb[2], acc.a]);
        }
    }
    (out, crossing)
}
