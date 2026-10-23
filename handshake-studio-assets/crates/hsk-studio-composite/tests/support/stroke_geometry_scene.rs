//! Shared fixture of lane SMB-COMPOSITE-STROKE-GEOMETRY (folio-free): a vector layer whose stroke
//! rows carry their own geometry ([`FillPaint::StrokeGeometry`]), as a vector network or a
//! variable-width stroke does. Included with `#[path]` next to `vector_scenes.rs` (module `scenes`)
//! by composite `tests/stroke_geometry.rs` and by render-cpu / render-gpu
//! `tests/vector_stroke_geometry.rs`; the pixels are compared with `scenes::oracle` (pigment
//! `composite_over` over nib's planes, independent of composite's lowering).
//!
//! Layer `scenes::LAYER` (32x24 plan): fill geometry = a rectangle (6, 5)-(20, 19); rows:
//! 0. solid fill, normal 100 %;
//! 1. stroke geometry = two overlapping bands (a tapered diagonal band and a slanted vertical band,
//!    united), solid, normal 90 %; the bands reach outside the fill rectangle;
//! 2. stroke geometry = a triangle, linear gradient paint, screen 80 %.
#![allow(dead_code)]
use super::scenes;
use hsk_studio_composite::vector::{FillRule, OwnedPath, PathElement, PathPoint};
use hsk_studio_composite::{FillPaint, FillSpec, VectorGeometry, VectorLayerSpec};
use hsk_studio_pigment::blend_math::StudioBlendMode;

pub fn poly(points: &[(f64, f64)]) -> OwnedPath {
    let mut elements = vec![PathElement::MoveTo(PathPoint::new(points[0].0, points[0].1))];
    for p in &points[1..] {
        elements.push(PathElement::LineTo(PathPoint::new(p.0, p.1)));
    }
    elements.push(PathElement::Close);
    OwnedPath { elements, fill_rule: FillRule::NonZero }
}

pub fn fill_rect() -> OwnedPath {
    poly(&[(6.0, 5.0), (20.0, 5.0), (20.0, 19.0), (6.0, 19.0)])
}

/// The two bands of stroke row 1 (a variable-width band and a crossing band).
pub fn bands() -> Vec<OwnedPath> {
    vec![poly(&[(2.0, 8.0), (30.0, 6.0), (30.0, 9.5), (2.0, 12.0)]), poly(&[(22.0, 1.5), (25.25, 1.5), (27.5, 22.0), (23.0, 22.0)])]
}

pub fn spec() -> VectorLayerSpec {
    VectorLayerSpec {
        layer_id: scenes::LAYER.to_owned(),
        geometry: VectorGeometry::Paths(vec![fill_rect()]),
        fills: vec![
            FillSpec { paint: FillPaint::Solid([0.2, 0.5, 0.8, 1.0]), mode: StudioBlendMode::Normal, opacity: 1.0 },
            FillSpec {
                paint: FillPaint::StrokeGeometry { outline: bands(), paint: Box::new(FillPaint::Solid([0.9, 0.3, 0.1, 1.0])) },
                mode: StudioBlendMode::Normal,
                opacity: 0.9,
            },
            FillSpec {
                paint: FillPaint::StrokeGeometry {
                    outline: vec![poly(&[(4.0, 21.5), (15.5, 14.25), (17.0, 23.0)])],
                    paint: Box::new(FillPaint::Gradient(scenes::gradient([0.0, 0.9, 0.9, 1.0], [0.9, 0.0, 0.9, 1.0], None))),
                },
                mode: StudioBlendMode::Screen,
                opacity: 0.8,
            },
        ],
    }
}
