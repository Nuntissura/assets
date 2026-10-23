//! A composition with corner-pinned layers (lane SMB-AE-KEY-TRACK-3) and its oracle, shared by the
//! renderer tests (render-cpu and render-gpu `tests/composition_corner_pin.rs`). The oracle samples
//! each pinned tile with pigment's reference sampler through `pin` then the layer matrix
//! (`LayerTransform::projective`) and composites with pigment's `composite_over`, the conventions of
//! `composition_scenes::oracle`. Include next to `composition_scenes.rs` as module `scenes`.
#![allow(dead_code)]
use crate::scenes::{BACKGROUND, Pixels, tile_id};
use hsk_studio_composite::{PinnedContent, SceneComposition, SceneContent, SceneLayer};
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, StudioBlendMode, composite_over};
use hsk_studio_pigment::transform_math::{Affine2, LayerTransform, PreparedTransform, Projective2, ResampleFilter};

/// Two pinned tiles: tile "ta" (8x6 at the origin) pinned onto a perspective quad and then rotated,
/// scaled and moved by the layer matrix; tile "tb" (6x6 at (2, 2)) pinned with an identity matrix
/// (an integer placement that the pin turns into a resample).
pub fn pinned_scene(pixels: &Pixels) -> SceneComposition {
    let ta = pixels.get(&tile_id(2, "ta")).expect("tile ta").scene_tile(0, 0);
    let tb = pixels.get(&tile_id(3, "tb")).expect("tile tb").scene_tile(2, 2);
    let pin_a = Projective2::from_quads(
        [[0.0, 0.0], [8.0, 0.0], [8.0, 6.0], [0.0, 6.0]],
        [[1.0, 0.5], [9.5, 1.5], [8.75, 7.0], [0.5, 5.5]],
    )
    .unwrap();
    let pin_b = Projective2::from_quads(
        [[2.0, 2.0], [8.0, 2.0], [8.0, 8.0], [2.0, 8.0]],
        [[18.0, 12.0], [29.0, 13.5], [27.5, 22.0], [17.25, 20.0]],
    )
    .unwrap();
    let layer = |id: &str, content: SceneContent, matrix: Affine2, opacity: f64| SceneLayer {
        layer_id: id.to_owned(),
        content,
        mode: StudioBlendMode::Normal,
        opacity,
        fill_opacity: 1.0,
        matrix,
        matte: None,
        clipped: false,
    };
    SceneComposition {
        composition_id: "pinned".to_owned(),
        width: 32,
        height: 24,
        background: Some(BACKGROUND),
        filter: ResampleFilter::Bilinear,
        layers: vec![
            layer(
                "SLYR-pin-a",
                SceneContent::Pinned(Box::new(PinnedContent { content: SceneContent::Tiles(vec![ta]), pin: pin_a })),
                Affine2::from_parts(4.0, 3.0, 1.5, 1.5, 12.0, 6.0, 5.0),
                0.9,
            ),
            layer(
                "SLYR-pin-b",
                SceneContent::Pinned(Box::new(PinnedContent { content: SceneContent::Tiles(vec![tb]), pin: pin_b })),
                Affine2::IDENTITY,
                1.0,
            ),
        ],
    }
}

/// Straight RGBA of every pixel of [`pinned_scene`], row-major.
pub fn pinned_oracle(scene: &SceneComposition, pixels: &Pixels) -> Vec<[f32; 4]> {
    let mut draws = Vec::new();
    for l in &scene.layers {
        let SceneContent::Pinned(p) = &l.content else { panic!("pinned fixture") };
        let SceneContent::Tiles(tiles) = &p.content else { panic!("tiles") };
        let t = &tiles[0];
        let map = p.pin.then(&Projective2::from_affine(&l.matrix));
        let prep = PreparedTransform::new(&LayerTransform::projective(map, scene.filter), t.placement).unwrap();
        draws.push((l, pixels.get(&t.tile_id).unwrap(), prep));
    }
    let mut out = Vec::new();
    for y in 0..i64::from(scene.height) {
        for x in 0..i64::from(scene.width) {
            let b = scene.background.unwrap();
            let mut acc = composite_over(StudioBlendMode::Normal, Rgba::TRANSPARENT, Rgba::new([b[0], b[1], b[2]], b[3]), 1.0, PixelSite::default()).unwrap();
            for (l, tile, prep) in &draws {
                let s = prep.sample_rgba(x, y, |i, j| Ok::<_, ()>(tile.at(i, j))).unwrap();
                acc = composite_over(l.mode, acc, Rgba::new([s[0], s[1], s[2]], s[3]), (l.opacity * l.fill_opacity) as f32, PixelSite { x, y, seed: 0 }).unwrap();
            }
            out.push([acc.rgb[0], acc.rgb[1], acc.rgb[2], acc.a]);
        }
    }
    out
}
