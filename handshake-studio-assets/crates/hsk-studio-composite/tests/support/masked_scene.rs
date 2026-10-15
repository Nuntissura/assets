//! A composition layer cut by mask paths (lane SMB-AE-KEY-TRACK-3) and its oracle, shared by the
//! renderer tests (render-cpu and render-gpu `tests/composition_mask_paths.rs`). The masks are exact
//! integer rectangles (coverage 0 or 1), combined `add` then `subtract`, so the oracle is the tile
//! composited where the combined coverage is 1. Include next to `composition_scenes.rs` as `scenes`.
#![allow(dead_code)]
use crate::scenes::{BACKGROUND, Pixels, space, tile_id};
use hsk_studio_composite::mask_paths::{MaskAtTick, StudioCompositionMaskMode, coverage_matte, mask_coverage};
use hsk_studio_composite::{SceneComposition, SceneContent, SceneLayer};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, StudioBlendMode, composite_over};
use hsk_studio_pigment::transform_math::{Affine2, ResampleFilter};

fn rect_path(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<f64> {
    vec![x0, y0, 0.0, 0.0, 0.0, 0.0, x1, y0, 0.0, 0.0, 0.0, 0.0, x1, y1, 0.0, 0.0, 0.0, 0.0, x0, y1, 0.0, 0.0, 0.0, 0.0]
}

/// Inside the add rectangle and outside the subtract rectangle (composition pixels).
pub fn covered(x: i64, y: i64) -> bool {
    (6..22).contains(&x) && (5..16).contains(&y) && !((10..14).contains(&x) && (8..12).contains(&y))
}

/// Tile "tf" (20x14 at (4, 3)) under an `add` mask (6, 5)-(22, 16) and a `subtract` mask
/// (10, 8)-(14, 12), opacity 0.8.
pub fn masked_scene(pixels: &Pixels) -> SceneComposition {
    let rect = Rect { x: 0, y: 0, width: 32, height: 24 };
    let masks = vec![
        MaskAtTick { mode: StudioCompositionMaskMode::Add, inverted: false, closed: true, path: rect_path(6.0, 5.0, 22.0, 16.0) },
        MaskAtTick { mode: StudioCompositionMaskMode::Subtract, inverted: false, closed: true, path: rect_path(10.0, 8.0, 14.0, 12.0) },
    ];
    let cov = mask_coverage(&masks, &Affine2::IDENTITY, rect, 32 * 24).unwrap().unwrap();
    let matte = coverage_matte(&cov, rect, "SLYR-masked", space(), 7).unwrap();
    let tile = pixels.get(&tile_id(11, "tf")).expect("tile tf").scene_tile(4, 3);
    SceneComposition {
        composition_id: "masked".to_owned(),
        width: 32,
        height: 24,
        background: Some(BACKGROUND),
        filter: ResampleFilter::Bilinear,
        layers: vec![SceneLayer {
            layer_id: "SLYR-masked".to_owned(),
            content: SceneContent::Tiles(vec![tile]),
            mode: StudioBlendMode::Normal,
            opacity: 0.8,
            fill_opacity: 1.0,
            matrix: Affine2::IDENTITY,
            matte: Some(matte),
            clipped: false,
        }],
    }
}

/// Straight RGBA of every pixel of [`masked_scene`], row-major.
pub fn masked_oracle(pixels: &Pixels) -> Vec<[f32; 4]> {
    let tile = pixels.get(&tile_id(11, "tf")).unwrap();
    let mut out = Vec::new();
    for y in 0..24i64 {
        for x in 0..32i64 {
            let b = BACKGROUND;
            let mut acc = composite_over(StudioBlendMode::Normal, Rgba::TRANSPARENT, Rgba::new([b[0], b[1], b[2]], b[3]), 1.0, PixelSite::default()).unwrap();
            let on_tile = (4..24).contains(&x) && (3..17).contains(&y);
            if on_tile && covered(x, y) {
                let s = tile.at((x - 4) as u32, (y - 3) as u32);
                if s[3] > 0.0 {
                    acc = composite_over(StudioBlendMode::Normal, acc, Rgba::new([s[0], s[1], s[2]], s[3]), 0.8, PixelSite { x, y, seed: 0 }).unwrap();
                }
            }
            out.push([acc.rgb[0], acc.rgb[1], acc.rgb[2], acc.a]);
        }
    }
    out
}
