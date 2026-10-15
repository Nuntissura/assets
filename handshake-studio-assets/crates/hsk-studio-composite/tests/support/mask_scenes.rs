//! Shared vector-mask fixture of lane SMB-COMPOSITE-VECTOR-MASK (folio-free). Included with
//! `#[path]` by render-cpu / render-gpu `tests/vector_mask.rs` (decision B1): the nib layer of
//! `vector_scenes.rs` masked four ways, lowered by `vector_layer_render_plan`, every pixel compared
//! with [`oracle_masked`].
//!
//! Mask source (rendered alone, crate::vector_mask): a group (opacity 0.9) of `A` (yellow square,
//! normal 60 %) and `B` (blue triangle, multiply). Cases: VECTOR (the diamond of
//! `vector_scenes::clip_outline`), VECTOR inverted (pigment's mask invert), ALPHA of the group, and
//! LUMINANCE of the group inverted inside a fractional region (baked on the host).
//!
//! The oracle is the unmasked layer oracle (`vector_scenes::oracle`) cut by the lowered mask plane
//! (`coverage_u16le / 65535`, inverted by the plan flag, zero outside the plane); the mask values
//! themselves are proven against nib and pigment by composite `tests/vector_mask.rs`.
#![allow(dead_code)]
#[path = "vector_scenes.rs"]
pub mod base;

use hsk_studio_composite::vector::{FillRule, OwnedPath, PathElement, PathPoint};
use hsk_studio_composite::{
    FillPaint, FillSpec, InputsLowering, MaskContentMode, MaskNode, PreparedVectors, VectorCoverage,
    VectorGeometry, VectorLayerSpec, VectorMask, VectorMaskSource, VectorPlane, VectorResolution,
    mask_content, prepare_vector_layers,
};
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, StudioBlendMode, composite_over};
use hsk_studio_pigment::render_plan::PlanNode;

pub const MASK_A: &str = "SLYR-019abcde-0000-7000-8000-00000000000b";
pub const MASK_B: &str = "SLYR-019abcde-0000-7000-8000-00000000000c";
pub const CASES: [&str; 4] = ["vector", "vector_inverted", "alpha", "luminance_inverted_region"];

fn poly(points: &[(f64, f64)]) -> OwnedPath {
    let mut elements = vec![PathElement::MoveTo(PathPoint::new(points[0].0, points[0].1))];
    elements.extend(points[1..].iter().map(|p| PathElement::LineTo(PathPoint::new(p.0, p.1))));
    elements.push(PathElement::Close);
    OwnedPath { elements, fill_rule: FillRule::NonZero }
}
fn solid(id: &str, path: OwnedPath, rgba: [f32; 4], mode: StudioBlendMode) -> VectorLayerSpec {
    VectorLayerSpec {
        layer_id: id.to_owned(),
        geometry: VectorGeometry::Paths(vec![path]),
        fills: vec![FillSpec { paint: FillPaint::Solid(rgba), mode, opacity: 1.0 }],
    }
}

/// The nib layer of `vector_scenes` (optionally moved by `(dx, dy)`) and the two mask-source layers.
pub fn prepared_moved(dx: f64, dy: f64) -> PreparedVectors {
    let mut layer = base::spec();
    if let VectorGeometry::Paths(paths) = &mut layer.geometry {
        for p in paths {
            for e in &mut p.elements {
                let mut mv = |q: &mut PathPoint| *q = PathPoint::new(q.x + dx, q.y + dy);
                match e {
                    PathElement::MoveTo(q) | PathElement::LineTo(q) => mv(q),
                    PathElement::QuadTo(c, q) => {
                        mv(c);
                        mv(q);
                    }
                    PathElement::CubicTo(c1, c2, q) => {
                        mv(c1);
                        mv(c2);
                        mv(q);
                    }
                    PathElement::Close => {}
                }
            }
        }
    }
    let a = solid(MASK_A, poly(&[(6.0, 3.5), (22.5, 3.5), (22.5, 18.0), (6.0, 18.0)]), [0.95, 0.85, 0.1, 1.0], StudioBlendMode::Normal);
    let b = solid(MASK_B, poly(&[(12.0, 8.0), (31.0, 12.5), (15.5, 23.0)]), [0.1, 0.3, 0.9, 0.8], StudioBlendMode::Normal);
    let res = VectorResolution { coverage: VectorCoverage::Exact, ..VectorResolution::default() };
    prepare_vector_layers(&[layer, a, b], &res, &CancellationToken::default()).unwrap()
}
pub fn prepared() -> PreparedVectors {
    prepared_moved(0.0, 0.0)
}

/// The mask source group: opacity 0.9, `A` normal 60 %, `B` multiply.
pub fn source() -> MaskNode {
    MaskNode::Group {
        mode: StudioBlendMode::Normal,
        opacity: 0.9,
        children: vec![
            MaskNode::Leaf { layer_id: MASK_A.into(), mode: StudioBlendMode::Normal, opacity: 0.6 },
            MaskNode::Leaf { layer_id: MASK_B.into(), mode: StudioBlendMode::Multiply, opacity: 1.0 },
        ],
    }
}

/// The mask of case `name` (one of [`CASES`]) over `vectors`.
pub fn mask(name: &str, vectors: &PreparedVectors) -> VectorMask {
    let content = |mode| mask_content(vectors, &source(), mode, &mut (1 << 24)).unwrap();
    match name {
        "vector" => VectorMask { source: VectorMaskSource::Outline(Some(base::clip_outline())), invert: false, region: None },
        "vector_inverted" => VectorMask { source: VectorMaskSource::Outline(Some(base::clip_outline())), invert: true, region: None },
        "alpha" => VectorMask { source: content(MaskContentMode::Alpha), invert: false, region: None },
        "luminance_inverted_region" => VectorMask {
            source: content(MaskContentMode::Luminance),
            invert: true,
            region: Some([3.25, 2.0, 26.5, 20.75]),
        },
        other => panic!("case {other}"),
    }
}

/// `prepared()` with `base::LAYER` masked by case `name`.
pub fn masked(name: &str) -> PreparedVectors {
    let v = prepared();
    let m = mask(name, &v);
    v.with_mask(base::LAYER, m)
}

/// The lowered mask plane's value at `(x, y)` (inverted by the plan flag; zero outside the plane).
pub fn mask_at(low: &InputsLowering, x: i64, y: i64) -> f32 {
    let PlanNode::Group(root) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!("wrapped root") };
    let m = root.layers[0].mask.expect("mask on the wrapper");
    let [(_, plane)] = low.clip_planes.as_slice() else { panic!("one plane") };
    assert!(plane.wide);
    let r = plane.placement;
    let inside = x >= r.x && y >= r.y && x < r.x + i64::from(r.width) && y < r.y + i64::from(r.height);
    let v = if inside {
        let i = (((y - r.y) * i64::from(r.width) + (x - r.x)) * 2) as usize;
        (f64::from(u16::from_le_bytes([plane.coverage[i], plane.coverage[i + 1]])) / 65535.0) as f32
    } else {
        0.0
    };
    if m.inverted { 1.0 - v } else { v }
}

/// [`base::oracle`] of `plane` cut by the lowered mask of `low`.
pub fn oracle_masked(plane: &VectorPlane, low: &InputsLowering) -> Vec<[f32; 4]> {
    base::oracle(plane)
        .into_iter()
        .enumerate()
        .map(|(i, p)| {
            let (x, y) = ((i % base::W as usize) as i64, (i / base::W as usize) as i64);
            let o = composite_over(StudioBlendMode::Normal, Rgba::TRANSPARENT, Rgba::new([p[0], p[1], p[2]], p[3]),
                mask_at(low, x, y), PixelSite { x, y, seed: 0 }).unwrap();
            [o.rgb[0], o.rgb[1], o.rgb[2], o.a]
        })
        .collect()
}
