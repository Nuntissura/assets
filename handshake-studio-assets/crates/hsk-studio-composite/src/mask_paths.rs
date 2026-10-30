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
//!
//! Shaped masks (lane SMB-AE-MASK-FEATHER-1; STU-CMP-025 mask feather, opacity and expansion, all
//! keyframable): [`mask_coverage_shaped`] takes a [`MaskShape`] per mask, whose `Default` is the
//! identity (bit-identical to [`mask_coverage`], test `tests/mask_shape.rs`
//! `identity_shape_is_bit_identical_to_mask_coverage`). Studio readings, each UNVERIFIED against
//! After Effects (order and arithmetic, D5; no D5 oracle case exists for After Effects masks):
//! - Order per mask: (1) expansion, (2) coverage over the rect grown by the feather support,
//!   (3) separable feather cropped back to the rect, (4) invert, (5) multiply by opacity,
//!   (6) [`combine`] in mask order (STU-CMP-025a).
//! - Expansion is applied in LAYER space, before the layer matrix: nib's `edit::offset_path` of
//!   the mask's bezier with a round join (STU-CMP-027 the same vector primitive, STU-VEC-113),
//!   flattened at [`MASK_EXPANSION_TOLERANCE_PX`]; the polygon result is then mapped by the matrix.
//!   Expansion 0 skips the offset (the exact curve is kept). An inset that collapses the region
//!   gives zero coverage for that mask, not an error.
//! - Feather is applied in the pixels of the coverage plane (composition pixels for a layer's
//!   composition-space matte, layer pixels for a 3D plane or an effect stack's content rect),
//!   along the plane's x and y axes; a layer scale or rotation does not scale or turn it. The
//!   feather value is used directly as the radius of pigment's `coverage_math` feather (the
//!   Photoshop vector-mask feather maths: `sigma = radius / 2`, `feather_kernel` taps over
//!   `feather_support = ceil(3 sigma)` pixels), x radius horizontally then y radius vertically.
//!   Coverage is taken over the rect grown by the support of each axis, so the cropped result
//!   reads no clamped border samples. Two axes are a Studio reading of the uniform After Effects
//!   "Mask Feather" (STU-MOT-095); the variable-width feather (`ADBE Mask Interp`) is a declared
//!   gap.
//! - Sampled values (keyframe interpolation may overshoot the stored ranges): opacity is clamped
//!   into `0..=1` and a negative feather to 0; a non-finite value, or a feather above pigment's
//!   `MAX_FEATHER_PX`, is a typed refusal naming the mask ([`MaskShapeRefusal`]).
#![deny(clippy::arithmetic_side_effects)]

use crate::CompositeError;
use crate::composition::{OwnedMask, SceneContent, SceneLayer, SceneMatte, SceneMatteMode, StaticContent};
pub use hsk_studio_folio::StudioCompositionMaskMode;
use hsk_studio_nib::boolean::BooleanOptions;
use hsk_studio_nib::flatten::FlattenOptions;
use hsk_studio_nib::stroke::LineJoin;
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

/// The feather, opacity and expansion of one mask at a tick (module docs). `Default` is the
/// identity: no feather, opacity 1, no expansion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaskShape {
    /// Feather radius in pixels of the coverage plane, `[x, y]`.
    pub feather_px: [f64; 2],
    /// Mask opacity, `0..=1` (Folio's stored form, STU-CMP-026).
    pub opacity: f64,
    /// Path offset in layer-space pixels; positive grows the region.
    pub expansion_px: f64,
}
impl Default for MaskShape {
    fn default() -> Self {
        Self { feather_px: [0.0, 0.0], opacity: 1.0, expansion_px: 0.0 }
    }
}

/// A refusal of [`mask_coverage_shaped`]: the index of the refused mask in the input slice and a
/// stable code (one of this module's `COMPOSITION_MASK_*` codes, `mask_path_shape`, or a nib
/// coverage code).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaskShapeRefusal {
    pub mask: usize,
    pub code: &'static str,
}

/// A feather that is non-finite or above pigment's `coverage_math::MAX_FEATHER_PX`.
pub const COMPOSITION_MASK_FEATHER_OUT_OF_RANGE: &str = "composition_mask_feather_out_of_range";
/// A non-finite mask opacity.
pub const COMPOSITION_MASK_OPACITY_NOT_FINITE: &str = "composition_mask_opacity_not_finite";
/// A non-finite expansion, or one nib's offset refuses (coordinate limit, geometry budgets).
pub const COMPOSITION_MASK_EXPANSION_GEOMETRY: &str = "composition_mask_expansion_geometry";
/// Flatten tolerance of an expanded mask path, layer-space pixels (nib's offset result is a
/// polygon; the default 0.1 would move a round corner's pixel coverage by more than 1/255).
pub const MASK_EXPANSION_TOLERANCE_PX: f64 = 1.0e-3;

/// The polygon of `path` (layer space) offset by `expansion` with a round join, mapped by
/// `matrix`; `None` when the inset collapses the region.
fn expanded_path(path: &[f64], expansion: f64, matrix: &Affine2) -> Result<Option<OwnedPath>, &'static str> {
    if !expansion.is_finite() {
        return Err(COMPOSITION_MASK_EXPANSION_GEOMETRY);
    }
    let layer = mask_path(path, &Affine2::IDENTITY).ok_or("mask_path_shape")?;
    let defaults = BooleanOptions::default();
    let options = BooleanOptions {
        flatten: FlattenOptions { tolerance: MASK_EXPANSION_TOLERANCE_PX, ..defaults.flatten },
        ..defaults
    };
    let offset = match hsk_studio_nib::edit::offset_path(&layer, expansion, LineJoin::Round, 4.0, &options) {
        Ok(r) => r.path,
        Err(hsk_studio_nib::Error::EmptyPathfinderResult) => return Ok(None),
        Err(_) => return Err(COMPOSITION_MASK_EXPANSION_GEOMETRY),
    };
    let pt = |p: PathPoint| {
        let (a, b) = matrix.apply(p.x, p.y);
        PathPoint::new(a, b)
    };
    let elements = offset
        .elements
        .iter()
        .map(|e| match *e {
            PathElement::MoveTo(p) => PathElement::MoveTo(pt(p)),
            PathElement::LineTo(p) => PathElement::LineTo(pt(p)),
            PathElement::QuadTo(c, p) => PathElement::QuadTo(pt(c), pt(p)),
            PathElement::CubicTo(c1, c2, p) => PathElement::CubicTo(pt(c1), pt(c2), pt(p)),
            PathElement::Close => PathElement::Close,
        })
        .collect();
    Ok(Some(OwnedPath { elements, fill_rule: offset.fill_rule }))
}

/// Pigment's feather taps and support for a sampled radius (a negative finite radius is 0).
fn feather_taps(radius: f64) -> Result<(Vec<f32>, u32), &'static str> {
    if !radius.is_finite() || radius > f64::from(hsk_studio_pigment::coverage_math::MAX_FEATHER_PX) {
        return Err(COMPOSITION_MASK_FEATHER_OUT_OF_RANGE);
    }
    // Finite and within 0..=MAX_FEATHER_PX (an f32 range) by the check above.
    #[allow(clippy::cast_possible_truncation)]
    let r = radius.max(0.0) as f32;
    let taps = hsk_studio_pigment::coverage_math::feather_kernel(r).map_err(|_| COMPOSITION_MASK_FEATHER_OUT_OF_RANGE)?;
    let support = hsk_studio_pigment::coverage_math::feather_support(r).map_err(|_| COMPOSITION_MASK_FEATHER_OUT_OF_RANGE)?;
    Ok((taps, support))
}

/// The tap-weighted sum of `values` (a weighted mean of values in `0..=1`; the taps sum to 1),
/// accumulated in f64 and stored f32 like the plane.
#[allow(clippy::cast_possible_truncation)]
fn dot(values: &mut dyn Iterator<Item = f32>, taps: &[f32]) -> f32 {
    values.zip(taps).map(|(v, t)| f64::from(v) * f64::from(*t)).sum::<f64>() as f32
}

/// `plane` (row-major over a rect `width` wide) blurred by `kx` along x then `ky` along y, keeping
/// only the samples every tap of which lies inside the plane: `width - (kx.len() - 1)` columns by
/// `rows - (ky.len() - 1)` rows.
fn separable_feather(plane: &[f32], width: usize, kx: &[f32], ky: &[f32]) -> Vec<f32> {
    let mut across = Vec::new();
    let mut out_width = 0usize;
    for row in plane.chunks_exact(width) {
        let before = across.len();
        across.extend(row.windows(kx.len()).map(|w| dot(&mut w.iter().copied(), kx)));
        out_width = across.len().saturating_sub(before);
    }
    if out_width == 0 {
        return Vec::new();
    }
    let rows: Vec<&[f32]> = across.chunks_exact(out_width).collect();
    let mut out = Vec::new();
    for window in rows.windows(ky.len()) {
        for i in 0..out_width {
            out.push(dot(&mut window.iter().map(|r| r[i]), ky));
        }
    }
    out
}

/// One mask's shaped coverage over `rect` (steps 1-3 of the module docs), before invert.
fn shaped_plane(m: &MaskAtTick, shape: &MaskShape, matrix: &Affine2, rect: Rect, max_pixels: u64) -> Result<Vec<f32>, &'static str> {
    let (kx, sx) = feather_taps(shape.feather_px[0])?;
    let (ky, sy) = feather_taps(shape.feather_px[1])?;
    let path = if shape.expansion_px == 0.0 {
        Some(mask_path(&m.path, matrix).ok_or("mask_path_shape")?)
    } else {
        expanded_path(&m.path, shape.expansion_px, matrix)?
    };
    let over = crate::vector::REFUSE_VECTOR_OVER_LIMIT;
    let grown = Rect {
        x: rect.x.checked_sub(i64::from(sx)).ok_or(over)?,
        y: rect.y.checked_sub(i64::from(sy)).ok_or(over)?,
        width: rect.width.checked_add(sx.checked_mul(2).ok_or(over)?).ok_or(over)?,
        height: rect.height.checked_add(sy.checked_mul(2).ok_or(over)?).ok_or(over)?,
    };
    let cov = crate::vector_nib::coverage(path.as_ref(), grown, max_pixels)?;
    let plane: Vec<f32> = cov.iter().map(|c| f32::from(*c) / 255.0).collect();
    if (sx == 0 && sy == 0) || rect.width == 0 || rect.height == 0 {
        return Ok(plane);
    }
    let width = usize::try_from(grown.width).map_err(|_| over)?;
    Ok(separable_feather(&plane, width, &kx, &ky))
}

/// The combined coverage of shaped masks over `rect`, row-major, or `None` when no mask
/// contributes (module docs). `max_pixels` bounds every coverage plane, including the rect grown
/// by a feather's support; it is checked before allocation. The identity shape gives exactly
/// [`mask_coverage`]'s result.
pub fn mask_coverage_shaped(
    masks: &[(MaskAtTick, MaskShape)],
    matrix: &Affine2,
    rect: Rect,
    max_pixels: u64,
) -> Result<Option<Vec<f32>>, MaskShapeRefusal> {
    let mut acc: Option<Vec<f32>> = None;
    for (index, (m, shape)) in masks.iter().enumerate() {
        if !m.closed || m.mode == StudioCompositionMaskMode::None {
            continue;
        }
        let refuse = |code: &'static str| MaskShapeRefusal { mask: index, code };
        if !shape.opacity.is_finite() {
            return Err(refuse(COMPOSITION_MASK_OPACITY_NOT_FINITE));
        }
        // Finite; clamped into 0..=1 (an f32 range).
        #[allow(clippy::cast_possible_truncation)]
        let opacity = shape.opacity.clamp(0.0, 1.0) as f32;
        let cov = shaped_plane(m, shape, matrix, rect, max_pixels).map_err(refuse)?;
        let a = acc.get_or_insert_with(|| {
            let start = match m.mode {
                StudioCompositionMaskMode::Subtract | StudioCompositionMaskMode::Intersect => 1.0,
                _ => 0.0,
            };
            vec![start; cov.len()]
        });
        for (v, c) in a.iter_mut().zip(cov) {
            let mut cv = c;
            if m.inverted {
                cv = 1.0 - cv;
            }
            *v = combine(m.mode, *v, cv * opacity).clamp(0.0, 1.0);
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
