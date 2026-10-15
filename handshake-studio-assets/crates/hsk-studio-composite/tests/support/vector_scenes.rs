//! Shared nib-vector fixture of lane SMB-COMPOSITE-VECTOR-IN-GROUP slice 2 (folio-free). Included
//! with `#[path]` by composite `tests/vector_nib.rs` and by render-cpu / render-gpu
//! `tests/vector_nib.rs` (decision B1), which render [`spec`] through `vector_layer_render_plan` and
//! compare every pixel with [`oracle`].
//!
//! Layer `L` (32x24 plan): geometry = a triangle and an overlapping square (united; nib exact
//! coverage), rows bottom first:
//! 0. linear gradient fill (red -> blue, 25 % midpoint shift, pad), normal 100 %;
//! 1. 4x4 checker pattern fill, multiply 60 %;
//! 2. solid stroke 3 px, round joins and caps, normal 100 %;
//! 3. gradient stroke 1.5 px, dashed (4, 2), miter, screen 80 %.
//!
//! The oracle is independent of composite's lowering: per pixel the fill rows composite (pigment
//! `composite_over`) over transparent and are cut by the fill coverage; each stroke row composites
//! on top with its own coverage. Paint colours are nib's (`paint_coverage`) at the pixel centre and
//! the coverages are nib's planes, read from the prepared layer.
#![allow(dead_code)]
use hsk_studio_composite::vector::{
    DashPattern, FillRule, Gradient, GradientGeometry, GradientInterpolation, GradientStop, LineCap,
    LineJoin, OwnedPath, PathElement, PathPoint, PatternTile, Spread, StrokeAlign, StrokeStyle,
};
use hsk_studio_composite::{
    FillPaint, FillSpec, InputsLowering, PreparedPaint, PreparedVectors, VectorGeometry,
    VectorLayerSpec, VectorPlane,
};
use hsk_studio_pigment::Mask;
use hsk_studio_pigment::blend_math::{BlendSpace, PixelSite, Rgba, StudioBlendMode, composite_over};
use hsk_studio_pigment::render_plan::{ColourTag, PlanSources, SourceKey, TilePlanes};

pub const W: u32 = 32;
pub const H: u32 = 24;
pub const LAYER: &str = "SLYR-019abcde-0000-7000-8000-00000000000a";

pub fn space() -> ColourTag {
    ColourTag {
        profile_sha256: [7; 32],
        blend_space: BlendSpace::Encoded,
    }
}

fn poly(points: &[(f64, f64)]) -> OwnedPath {
    let mut elements = vec![PathElement::MoveTo(PathPoint::new(points[0].0, points[0].1))];
    for p in &points[1..] {
        elements.push(PathElement::LineTo(PathPoint::new(p.0, p.1)));
    }
    elements.push(PathElement::Close);
    OwnedPath {
        elements,
        fill_rule: FillRule::NonZero,
    }
}

pub fn geometry() -> VectorGeometry {
    VectorGeometry::Paths(vec![
        poly(&[(4.0, 18.5), (15.25, 3.0), (24.0, 19.0)]),
        poly(&[(14.0, 10.0), (27.5, 10.0), (27.5, 21.0), (14.0, 21.0)]),
    ])
}

pub fn gradient(c0: [f32; 4], c1: [f32; 4], midpoint: Option<f64>) -> Gradient {
    Gradient {
        geometry: GradientGeometry::Linear {
            x1: 4.0,
            y1: 3.0,
            x2: 27.5,
            y2: 21.0,
        },
        transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        spread: Spread::Pad,
        interpolation: GradientInterpolation::Classic,
        dither: false,
        stops: vec![
            GradientStop { offset: 0.0, colour: c0, midpoint },
            GradientStop { offset: 1.0, colour: c1, midpoint: None },
        ],
    }
}

pub fn checker() -> PatternTile {
    let mut pixels = Vec::new();
    for y in 0..4u32 {
        for x in 0..4u32 {
            let on = (x / 2 + y / 2) % 2 == 0;
            // Premultiplied.
            pixels.push(if on { [0.2, 0.7, 0.3, 1.0] } else { [0.45, 0.45, 0.0, 0.5] });
        }
    }
    PatternTile {
        width: 4,
        height: 4,
        pixels,
        transform: [1.0, 0.0, 0.0, 1.0, 1.0, 0.0],
    }
}

pub fn spec() -> VectorLayerSpec {
    VectorLayerSpec {
        layer_id: LAYER.to_owned(),
        geometry: geometry(),
        fills: vec![
            FillSpec {
                paint: FillPaint::Gradient(gradient([0.9, 0.1, 0.1, 1.0], [0.1, 0.2, 0.9, 0.8], Some(0.25))),
                mode: StudioBlendMode::Normal,
                opacity: 1.0,
            },
            FillSpec {
                paint: FillPaint::Pattern(checker()),
                mode: StudioBlendMode::Multiply,
                opacity: 0.6,
            },
            FillSpec {
                paint: FillPaint::Stroke {
                    style: StrokeStyle {
                        width: 3.0,
                        cap: LineCap::Round,
                        join: LineJoin::Round,
                        miter_limit: 4.0,
                        dash: None,
                        align: StrokeAlign::Center,
                    },
                    paint: Box::new(FillPaint::Solid([0.95, 0.85, 0.2, 1.0])),
                },
                mode: StudioBlendMode::Normal,
                opacity: 1.0,
            },
            FillSpec {
                paint: FillPaint::Stroke {
                    style: StrokeStyle {
                        width: 1.5,
                        cap: LineCap::Butt,
                        join: LineJoin::Miter,
                        miter_limit: 4.0,
                        dash: Some(DashPattern { lengths: vec![4.0, 2.0], phase: 0.0 }),
                        align: StrokeAlign::Center,
                    },
                    paint: Box::new(FillPaint::Gradient(gradient([0.0, 0.9, 0.9, 1.0], [0.9, 0.0, 0.9, 1.0], None))),
                },
                mode: StudioBlendMode::Screen,
                opacity: 0.8,
            },
        ],
    }
}

/// Plan sources of a lowered vector layer: painted tiles and coverage masks from the prepared layer.
pub struct Sources<'a> {
    pub low: &'a InputsLowering,
    pub vectors: &'a PreparedVectors,
}
impl PlanSources for Sources<'_> {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        self.low.vector_paint(key, self.vectors, space())
    }
    fn mask(&self, key: SourceKey) -> Option<Mask<'_>> {
        self.low.vector_mask(key, self.vectors)
    }
}

fn straight_at(plane: &VectorPlane, row: usize, x: i64, y: i64) -> [f32; 4] {
    let rect = plane.placement.unwrap();
    let i = ((y - rect.y) * i64::from(rect.width) + (x - rect.x)) as usize;
    match &plane.fills[row].paint {
        PreparedPaint::Solid(c) => *c,
        PreparedPaint::Painted(t) => {
            let f = |b: &[u8], at: usize| f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
            [f(&t.colour, i * 12), f(&t.colour, i * 12 + 4), f(&t.colour, i * 12 + 8), f(&t.alpha, i * 4)]
        }
        other => panic!("fixture row {other:?}"),
    }
}

/// Straight RGBA of every pixel of `(0, 0, W, H)`, row-major (module docs).
pub fn oracle(plane: &VectorPlane) -> Vec<[f32; 4]> {
    let rect = plane.placement.unwrap();
    let mut out = Vec::new();
    for y in 0..i64::from(H) {
        for x in 0..i64::from(W) {
            let inside = x >= rect.x && y >= rect.y && x < rect.x + i64::from(rect.width) && y < rect.y + i64::from(rect.height);
            if !inside {
                out.push([0.0; 4]);
                continue;
            }
            let i = ((y - rect.y) * i64::from(rect.width) + (x - rect.x)) as usize;
            let site = PixelSite { x, y, seed: 0 };
            let mut rows = Rgba::TRANSPARENT;
            for (row, fill) in plane.fills.iter().enumerate() {
                if plane.is_stroke(row) {
                    continue;
                }
                let c = straight_at(plane, row, x, y);
                rows = composite_over(fill.mode, rows, Rgba::new([c[0], c[1], c[2]], c[3]), fill.opacity, site).unwrap();
            }
            let cov = (f64::from(plane.coverage[i]) / 255.0) as f32;
            let mut acc = composite_over(StudioBlendMode::Normal, Rgba::TRANSPARENT, rows, cov, site).unwrap();
            for s in &plane.strokes {
                let fill = &plane.fills[s.row];
                let c = straight_at(plane, s.row, x, y);
                let k = (f64::from(s.coverage[i]) / 255.0) as f32;
                acc = composite_over(fill.mode, acc, Rgba::new([c[0], c[1], c[2]], c[3]), fill.opacity * k, site).unwrap();
            }
            out.push([acc.rgb[0], acc.rgb[1], acc.rgb[2], acc.a]);
        }
    }
    out
}

/// Largest |delta| of alpha and premultiplied colour.
pub fn max_delta(got: &[[f32; 4]], want: &[[f32; 4]]) -> f32 {
    let mut worst = 0.0f32;
    for (g, w) in got.iter().zip(want) {
        worst = worst.max((g[3] - w[3]).abs());
        for c in 0..3 {
            worst = worst.max((g[c] * g[3] - w[c] * w[3]).abs());
        }
    }
    worst
}
