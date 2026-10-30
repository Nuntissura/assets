//! A composition layer under shaped masks (lane SMB-AE-MASK-FEATHER-1: feather, opacity,
//! expansion) and its coverage oracle, for the renderer tests (render-cpu
//! `tests/composition_mask_shape.rs`). Include next to `composition_scenes.rs` as `scenes`.
//!
//! Tile "tf" (20x14 at (4, 3)) at opacity 0.8 under an `add` mask (6, 5)-(22, 16) feathered
//! [2.5, 1.5] px, then a `subtract` mask (10, 8)-(14, 12) expanded by 1 px (round corners) at
//! opacity 0.75. The oracle composites the tile over the background with the layer opacity times
//! the combined coverage of `mask_coverage_shaped` quantised to 16 bits (the `coverage_u16le`
//! plane of `coverage_matte`), so the frame proves the renderer carries the shaped coverage
//! unchanged, for every chunking.
#![allow(dead_code)]
use crate::scenes::{BACKGROUND, Pixels, space, tile_id};
use hsk_studio_composite::mask_paths::{MaskAtTick, MaskShape, StudioCompositionMaskMode, coverage_matte, mask_coverage_shaped};
use hsk_studio_composite::{SceneComposition, SceneContent, SceneLayer};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, StudioBlendMode, composite_over};
use hsk_studio_pigment::transform_math::{Affine2, ResampleFilter};

pub const OPACITY: f64 = 0.8;
const RECT: Rect = Rect { x: 0, y: 0, width: 32, height: 24 };

fn rect_path(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<f64> {
    vec![x0, y0, 0.0, 0.0, 0.0, 0.0, x1, y0, 0.0, 0.0, 0.0, 0.0, x1, y1, 0.0, 0.0, 0.0, 0.0, x0, y1, 0.0, 0.0, 0.0, 0.0]
}

/// The shaped masks of the scene.
pub fn shaped_masks() -> Vec<(MaskAtTick, MaskShape)> {
    vec![
        (
            MaskAtTick { mode: StudioCompositionMaskMode::Add, inverted: false, closed: true, path: rect_path(6.0, 5.0, 22.0, 16.0) },
            MaskShape { feather_px: [2.5, 1.5], ..MaskShape::default() },
        ),
        (
            MaskAtTick { mode: StudioCompositionMaskMode::Subtract, inverted: false, closed: true, path: rect_path(10.0, 8.0, 14.0, 12.0) },
            MaskShape { opacity: 0.75, expansion_px: 1.0, ..MaskShape::default() },
        ),
    ]
}

/// The combined coverage over the composition rect, row-major (`0..=1`, f32).
pub fn shaped_coverage() -> Vec<f32> {
    mask_coverage_shaped(&shaped_masks(), &Affine2::IDENTITY, RECT, 1 << 20).unwrap().unwrap()
}

/// Module docs.
pub fn mask_shape_scene(pixels: &Pixels) -> SceneComposition {
    let matte = coverage_matte(&shaped_coverage(), RECT, "SLYR-shaped", space(), 7).unwrap();
    let tile = pixels.get(&tile_id(11, "tf")).expect("tile tf").scene_tile(4, 3);
    SceneComposition {
        composition_id: "shaped".to_owned(),
        width: RECT.width,
        height: RECT.height,
        background: Some(BACKGROUND),
        filter: ResampleFilter::Bilinear,
        layers: vec![SceneLayer {
            layer_id: "SLYR-shaped".to_owned(),
            content: SceneContent::Tiles(vec![tile]),
            mode: StudioBlendMode::Normal,
            opacity: OPACITY,
            fill_opacity: 1.0,
            matrix: Affine2::IDENTITY,
            matte: Some(matte),
            clipped: false,
        }],
    }
}

/// Straight RGBA of every pixel of [`mask_shape_scene`], row-major (module docs).
pub fn mask_shape_oracle(pixels: &Pixels) -> Vec<[f32; 4]> {
    let tile = pixels.get(&tile_id(11, "tf")).unwrap();
    let coverage = shaped_coverage();
    let mut out = Vec::new();
    for y in 0..i64::from(RECT.height) {
        for x in 0..i64::from(RECT.width) {
            let b = BACKGROUND;
            let mut acc = composite_over(StudioBlendMode::Normal, Rgba::TRANSPARENT, Rgba::new([b[0], b[1], b[2]], b[3]), 1.0, PixelSite::default()).unwrap();
            let c = coverage[(y * i64::from(RECT.width) + x) as usize];
            // Quantised exactly as `coverage_matte` stores the plane.
            let q = (f64::from(c.clamp(0.0, 1.0)) * 65535.0).round() as u16;
            let cov = (f64::from(q) / 65535.0) as f32;
            let on_tile = (4..24).contains(&x) && (3..17).contains(&y);
            if on_tile && cov > 0.0 {
                let s = tile.at((x - 4) as u32, (y - 3) as u32);
                if s[3] > 0.0 {
                    acc = composite_over(StudioBlendMode::Normal, acc, Rgba::new([s[0], s[1], s[2]], s[3]), OPACITY as f32 * cov, PixelSite { x, y, seed: 0 }).unwrap();
                }
            }
            out.push([acc.rgb[0], acc.rgb[1], acc.rgb[2], acc.a]);
        }
    }
    out
}
