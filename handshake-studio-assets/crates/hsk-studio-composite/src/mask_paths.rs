//! Composition-layer mask paths (lane SMB-AE-KEY-TRACK-3; STU-CMP-025 "A mask is a bezier path on a
//! layer that contributes to the layer's alpha, and a layer may carry many", STU-CMP-025a seven
//! modes combined in order, an independent `inverted` flag).
//!
//! Evaluation at one tick: every mask's path value (Folio `StudioCompositionMask`, six numbers per
//! vertex, layer space) becomes a closed cubic bezier ([`mask_path`]: segment `k` runs from vertex
//! `k` through `vertex_k + out_k` and `vertex_{k+1} + in_{k+1}` to vertex `k+1`), mapped to
//! composition pixels by the layer's matrix (an affine map keeps a cubic a cubic), and nib's exact
//! area coverage (8-bit) is taken over the composition rect. The masks combine in order
//! ([`combine`]); the result is the layer's alpha matte, carried as a composition-space
//! [`SceneMatte`] (a white plane under the coverage, the construction of the unlinked vector mask
//! matte), so the scene lowering and both renderers need nothing new.
//!
//! Combination (Studio reading; After Effects' arithmetic is UNVERIFIED, D5): the accumulator
//! starts at 0, or at 1 when the first contributing mask is `subtract` or `intersect` (a first
//! subtracting mask cuts from the whole layer); with `m` the mask's coverage (`1 - m` inverted):
//! add `a + m - a m`, subtract `a (1 - m)`, intersect `a m`, lighten `max(a, m)`, darken
//! `min(a, m)`, difference `|a - m|`; `none` contributes nothing (geometry only). No contributing
//! mask: no matte.
#![deny(clippy::arithmetic_side_effects)]

use crate::CompositeError;
use crate::composition::{OwnedMask, SceneContent, SceneLayer, SceneMatte, SceneMatteMode, StaticContent};
pub use hsk_studio_folio::StudioCompositionMaskMode;
use hsk_studio_nib::{FillRule, OwnedPath, PathElement, PathPoint};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::StudioBlendMode;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, ColourTag, Group, Layer, MaskFormat, MaskInput, OutputTarget, PlanExtensions, PlanLimits,
    PlanNode, PlanNodeId, RenderPlan, SampleFormat, SourceKey,
};
use hsk_studio_pigment::transform_math::Affine2;
use std::collections::BTreeMap;

/// One mask evaluated at a tick.
#[derive(Clone, Debug, PartialEq)]
pub struct MaskAtTick {
    pub mode: StudioCompositionMaskMode,
    pub inverted: bool,
    pub closed: bool,
    /// Six numbers per vertex (`x, y, in_dx, in_dy, out_dx, out_dy`), layer space.
    pub path: Vec<f64>,
}

/// The closed bezier of a mask path value through `matrix` (layer space -> composition pixels).
/// `None` for an empty or malformed value.
pub fn mask_path(path: &[f64], matrix: &Affine2) -> Option<OwnedPath> {
    let n = path.len().checked_div(6)?;
    if n == 0 || path.len().checked_rem(6) != Some(0) || !path.iter().all(|v| v.is_finite()) {
        return None;
    }
    let vertex = |k: usize| -> Option<[f64; 6]> {
        let s = path.get(k.checked_mul(6)?..k.checked_mul(6)?.checked_add(6)?)?;
        Some([s[0], s[1], s[2], s[3], s[4], s[5]])
    };
    let pt = |x: f64, y: f64| {
        let (a, b) = matrix.apply(x, y);
        PathPoint::new(a, b)
    };
    let first = vertex(0)?;
    let mut elements = vec![PathElement::MoveTo(pt(first[0], first[1]))];
    for k in 0..n {
        let a = vertex(k)?;
        let b = vertex(k.checked_add(1)?.checked_rem(n)?)?;
        elements.push(PathElement::CubicTo(pt(a[0] + a[4], a[1] + a[5]), pt(b[0] + b[2], b[1] + b[3]), pt(b[0], b[1])));
    }
    elements.push(PathElement::Close);
    Some(OwnedPath { elements, fill_rule: FillRule::NonZero })
}

/// Combines one mask's coverage `m` into the accumulator `a` (module docs).
pub fn combine(mode: StudioCompositionMaskMode, a: f32, m: f32) -> f32 {
    match mode {
        StudioCompositionMaskMode::None => a,
        StudioCompositionMaskMode::Add => a + m - a * m,
        StudioCompositionMaskMode::Subtract => a * (1.0 - m),
        StudioCompositionMaskMode::Intersect => a * m,
        StudioCompositionMaskMode::Lighten => a.max(m),
        StudioCompositionMaskMode::Darken => a.min(m),
        StudioCompositionMaskMode::Difference => (a - m).abs(),
    }
}

/// The combined mask coverage over `rect` (composition pixels), row-major, or `None` when no mask
/// contributes. Refusal codes are the nib coverage codes (`max_pixels` bounds the plane).
pub fn mask_coverage(
    masks: &[MaskAtTick],
    matrix: &Affine2,
    rect: Rect,
    max_pixels: u64,
) -> Result<Option<Vec<f32>>, &'static str> {
    let mut acc: Option<Vec<f32>> = None;
    for m in masks.iter().filter(|m| m.closed && m.mode != StudioCompositionMaskMode::None) {
        let path = mask_path(&m.path, matrix).ok_or("mask_path_shape")?;
        let cov = crate::vector_nib::coverage(Some(&path), rect, max_pixels)?;
        let a = acc.get_or_insert_with(|| {
            let start = match m.mode {
                StudioCompositionMaskMode::Subtract | StudioCompositionMaskMode::Intersect => 1.0,
                _ => 0.0,
            };
            vec![start; cov.len()]
        });
        for (v, c) in a.iter_mut().zip(cov) {
            let mut cv = f32::from(c) / 255.0;
            if m.inverted {
                cv = 1.0 - cv;
            }
            *v = combine(m.mode, *v, cv).clamp(0.0, 1.0);
        }
    }
    Ok(acc)
}

/// The composition-space alpha matte of a coverage plane over `rect` (a white plane under it).
pub fn coverage_matte(
    coverage: &[f32],
    rect: Rect,
    layer_id: &str,
    space: ColourTag,
    revision: u64,
) -> Result<SceneMatte, CompositeError> {
    let mut bytes = Vec::with_capacity(coverage.len().saturating_mul(2));
    for c in coverage {
        // Exact for 0 and 1; round to the nearest of 65536 levels in between.
        let v = (f64::from(c.clamp(0.0, 1.0)) * 65535.0).round() as u16;
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    let sha256 = hsk_studio_prism::profile_hash(&bytes);
    let stride_bytes = u64::from(rect.width).checked_mul(2).ok_or(CompositeError::Overflow)?;
    let key = SourceKey(0);
    let plan = RenderPlan {
        revision,
        target: OutputTarget { extent: rect, format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight, space },
        nodes: vec![
            PlanNode::Solid { rect, rgba: [1.0, 1.0, 1.0, 1.0] },
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![Layer {
                    input: PlanNodeId(0),
                    mode: StudioBlendMode::Normal,
                    opacity: 1.0,
                    fill_opacity: 1.0,
                    mask: Some(MaskInput {
                        key,
                        placement: rect,
                        format: MaskFormat::CoverageU16Le,
                        sha256,
                        inverted: false,
                        density: 1.0,
                    }),
                }],
            }),
        ],
        root: PlanNodeId(1),
    };
    plan.validate(&PlanLimits::default()).map_err(CompositeError::Plan)?;
    let mut owned_masks = BTreeMap::new();
    owned_masks.insert(
        key,
        OwnedMask { bounds: rect, sample_format: "coverage_u16le".into(), bytes, stride_bytes, sha256 },
    );
    let content = StaticContent {
        plan,
        extensions: PlanExtensions::default(),
        rect,
        tiles: BTreeMap::new(),
        masks: BTreeMap::new(),
        owned_masks,
        owned_tiles: BTreeMap::new(),
    };
    Ok(SceneMatte {
        layer: Some(Box::new(SceneLayer {
            layer_id: format!("{layer_id}/masks"),
            content: SceneContent::Static(Box::new(content)),
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            fill_opacity: 1.0,
            matrix: Affine2::IDENTITY,
            matte: None,
            clipped: false,
        })),
        mode: SceneMatteMode::Alpha,
        inverted: false,
    })
}

/// A mask on a layer without pixels of its own (null, adjustment) or a collapsed precomposition.
pub const COMPOSITION_MASK_PATH_CONTENT: &str = "composition_mask_path_content";
/// Masks together with a track matte or an unlinked vector mask matte on one layer (one matte per
/// layer in the scene model).
pub const COMPOSITION_MASK_PATH_WITH_MATTE: &str = "composition_mask_path_with_matte";
/// Masks together with a corner pin on one layer (the mask would have to follow the pin).
pub const COMPOSITION_MASK_PATH_WITH_CORNER_PIN: &str = "composition_mask_path_with_corner_pin";
/// A masked layer on the document-graph path (`lower_frame`).
pub const COMPOSITION_MASK_PATH_ON_DOCUMENT_GRAPH: &str = "composition_mask_path_on_document_graph";
