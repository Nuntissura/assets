//! Envelope operators of the CPU backend (`RenderBackend::execute_extended`): adjustment layers,
//! Blend-If, mask density and mask feather. This module only wires them into the chunked
//! compositor. Every formula is pigment's single owner (CX-CAP-001): `adjust_math` (adjustment
//! maths and the adjustment-layer rule), `coverage_math` (mask density/invert, feather kernel,
//! Blend-If weight). render-cpu keeps no second copy of any of them.
//!
//! Resource rules (workflow v4 V4-01/V4-02): integer arithmetic is checked (denied by lint below);
//! the resident envelope state (prepared adjustments, Blend-If sets, feather kernels) is charged to
//! `ExecuteRequest::max_working_bytes` as a running aggregate BEFORE it is allocated, and the
//! per-worker feather scratch (a coverage plane for the chunk plus a horizontal-pass buffer that
//! also holds the `feather_support` margin rows) is part of the reserved scratch, so a feather wider
//! than the budget is refused with `BudgetExceeded` before the first sink call. Layer effects
//! (wave 1) keep their tables here and are evaluated in the `effects` submodule.
#![deny(clippy::arithmetic_side_effects)]
use super::{Eval, MaskView, PLayer, RenderError, blend_error, unsupported};
use crate::px::{
    add_u, area, end, height_of, index_in, mul_u, overflow, to_i64, to_usize, width_of,
};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::adjust_math::{
    AdjustError, AdjustLayerError, PreparedAdjustment, composite_adjustment,
};
use hsk_studio_pigment::blend_math::Rgba;
use hsk_studio_pigment::coverage_math::{
    BlendRangeEntry, CoverageError, IDENTITY_RANGE, blend_if_weight, feather_kernel,
    feather_support,
};
use hsk_studio_pigment::effect_math::{
    EffectError, EffectOp, MAX_REFERENCE_SIDE, effect_support_px,
};
use hsk_studio_pigment::render_plan::{BackendError, PlanExtensions};
use std::collections::BTreeMap;

/// Resident bound charged per prepared adjustment: four curve channels of at most 19 points with
/// three `f64` each is 1824 bytes, plus the record itself.
const ADJUSTMENT_STATE_BYTES: u64 = 2_048;
/// Resident bytes charged per Blend-If entry (channel plus two ranges).
const BLEND_ENTRY_BYTES: u64 = 40;
/// Bytes of one feather tap or scratch sample (`f32`).
const SAMPLE_BYTES: u64 = 4;
/// Resident bytes charged per effect instance (the `EffectOp` record).
const EFFECT_OP_BYTES: u64 = 64;
/// Pigment's reference planes have at most `MAX_REFERENCE_SIDE` (64) pixels per side, so a tile and
/// its margin must fit one plane. The reference recomputes the margin of every tile, so the cost of
/// a layer grows with `(64 / tile side)^2`; a margin of 24 px keeps tiles of at least 16 px. A
/// larger margin needs a windowed matte API in pigment and is refused as `effect_margin`.
pub(super) const MAX_EFFECT_MARGIN: usize = 24;

/// Envelope state of one layer: indices into the tables of [`ExtPrep`].
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct LayerFx {
    pub(super) adjustment: Option<usize>,
    pub(super) blend_if: Option<usize>,
    pub(super) feather: Option<usize>,
    pub(super) effect: Option<usize>,
}

/// The effects of one layer (list order) and the margin around its content they read.
pub(super) struct EffectLayer {
    pub(super) ops: Vec<EffectOp>,
    pub(super) margin: usize,
}

/// One normalised feather kernel: `2 * support + 1` taps.
struct Kernel {
    taps: Vec<f32>,
    support: usize,
}

/// Prepared envelope of a plan; `ExtPrep::default()` is the v1 plan (no envelope, density refused).
#[derive(Default)]
pub(super) struct ExtPrep {
    layers: BTreeMap<(u32, u32), LayerFx>,
    adjustments: Vec<PreparedAdjustment>,
    blend_sets: Vec<Vec<BlendRangeEntry>>,
    kernels: Vec<Kernel>,
    effects: Vec<EffectLayer>,
    /// Widest effect margin and longest effect list over all layers.
    max_margin: usize,
    max_effects: usize,
    /// Deepest isolated-group nesting under an effect layer's content (set by `prepare`).
    pub(super) frame_depth: usize,
    /// Bytes of the tables above, reserved for the whole run.
    pub(super) resident_bytes: u64,
    /// Widest kernel support over all feathered layers (the margin the scratch must hold).
    max_support: usize,
    /// The envelope path admits `MaskInput.density < 1`; the v1 path refuses it.
    pub(super) allow_density: bool,
}

pub(super) fn coverage_error(error: CoverageError) -> RenderError {
    match error {
        CoverageError::OutOfDomain => unsupported("sample_range_outside_unit"),
        CoverageError::FeatherOutOfRange => unsupported("mask_feather"),
        CoverageError::RangeOrder => unsupported("blend_if"),
    }
}
fn adjust_error(error: AdjustError) -> RenderError {
    match error {
        AdjustError::OutOfDomain => unsupported("sample_range_outside_unit"),
        AdjustError::InvalidParameter(what) | AdjustError::Unsupported(what) => unsupported(what),
    }
}
fn layer_error(error: AdjustLayerError) -> RenderError {
    match error {
        AdjustLayerError::Adjust(e) => adjust_error(e),
        AdjustLayerError::Blend(e) => blend_error(e),
    }
}

pub(super) fn effect_error(error: EffectError) -> RenderError {
    match error {
        EffectError::SizeOutOfRange | EffectError::DistanceOutOfRange => {
            unsupported("effect_limit")
        }
        EffectError::PlaneShape => unsupported("effect_plane_shape"),
        EffectError::OverBudget => unsupported("effect_over_budget"),
        EffectError::Overflow => overflow(),
        EffectError::Blend(e) => blend_error(e),
        _ => unsupported("effect_parameter"),
    }
}

/// Blend-If weight of one pixel (`coverage_math::blend_if_weight`, typed errors).
pub(super) fn blend_weight(
    entries: &[BlendRangeEntry],
    source: [f32; 3],
    backdrop: [f32; 3],
) -> Result<f32, RenderError> {
    blend_if_weight(entries, source, backdrop).map_err(coverage_error)
}

impl ExtPrep {
    /// Running-aggregate charge: refuses before the bytes are allocated.
    fn charge(&mut self, bytes: u64, limit: u64) -> Result<(), RenderError> {
        let total = self
            .resident_bytes
            .checked_add(bytes)
            .ok_or_else(overflow)?;
        if total > limit {
            return Err(RenderError::Backend(BackendError::BudgetExceeded {
                requested: total,
                limit,
            }));
        }
        self.resident_bytes = total;
        Ok(())
    }

    /// Prepares the envelope of a plan that `admit_extended` already validated: adjustments are
    /// prepared once per plan (`AdjustmentOp::prepare`), identical feather radii share a kernel.
    pub(super) fn build(extensions: &PlanExtensions, limit: u64) -> Result<Self, RenderError> {
        let mut prep = Self {
            allow_density: true,
            ..Self::default()
        };
        let mut kernel_of: BTreeMap<u32, usize> = BTreeMap::new();
        for e in &extensions.layers {
            let mut fx = LayerFx::default();
            if let Some(op) = &e.adjustment {
                prep.charge(ADJUSTMENT_STATE_BYTES, limit)?;
                let prepared = op.prepare().map_err(adjust_error)?;
                fx.adjustment = Some(prep.adjustments.len());
                prep.adjustments.push(prepared);
            }
            if e.blend_if
                .iter()
                .any(|x| x.source != IDENTITY_RANGE || x.destination != IDENTITY_RANGE)
            {
                let bytes = u64::try_from(e.blend_if.len())
                    .ok()
                    .and_then(|n| n.checked_mul(BLEND_ENTRY_BYTES))
                    .ok_or_else(overflow)?;
                prep.charge(bytes, limit)?;
                fx.blend_if = Some(prep.blend_sets.len());
                prep.blend_sets.push(e.blend_if.clone());
            }
            if !e.effects.is_empty() {
                // The margin an effect reads around the layer content: pigment's reference planes
                // hold at most MAX_REFERENCE_SIDE pixels per side, so a tile of the layer plus its
                // margin must fit one plane.
                let mut margin = 0_usize;
                for op in &e.effects {
                    let support = effect_support_px(op).map_err(effect_error)?;
                    margin = margin.max(to_usize(u64::from(support))?);
                }
                if margin > MAX_EFFECT_MARGIN {
                    return Err(unsupported("effect_margin"));
                }
                let bytes = u64::try_from(e.effects.len())
                    .ok()
                    .and_then(|n| n.checked_mul(EFFECT_OP_BYTES))
                    .ok_or_else(overflow)?;
                prep.charge(bytes, limit)?;
                prep.max_margin = prep.max_margin.max(margin);
                prep.max_effects = prep.max_effects.max(e.effects.len());
                fx.effect = Some(prep.effects.len());
                prep.effects.push(EffectLayer {
                    ops: e.effects.clone(),
                    margin,
                });
            }
            if e.mask_feather_px > 0.0 {
                let px = e.mask_feather_px;
                let index = if let Some(index) = kernel_of.get(&px.to_bits()) {
                    *index
                } else {
                    let support =
                        to_usize(u64::from(feather_support(px).map_err(coverage_error)?))?;
                    let taps = support
                        .checked_mul(2)
                        .and_then(|n| n.checked_add(1))
                        .ok_or_else(overflow)?;
                    let bytes = u64::try_from(taps)
                        .ok()
                        .and_then(|n| n.checked_mul(SAMPLE_BYTES))
                        .ok_or_else(overflow)?;
                    prep.charge(bytes, limit)?;
                    let weights = feather_kernel(px).map_err(coverage_error)?;
                    if weights.len() != taps {
                        return Err(unsupported("mask_feather"));
                    }
                    let index = prep.kernels.len();
                    prep.kernels.push(Kernel {
                        taps: weights,
                        support,
                    });
                    kernel_of.insert(px.to_bits(), index);
                    prep.max_support = prep.max_support.max(support);
                    index
                };
                fx.feather = Some(index);
            }
            prep.layers.insert((e.group.0, e.layer), fx);
        }
        Ok(prep)
    }

    /// Envelope state of layer `layer` of group node `group` (none when it has no entry).
    pub(super) fn layer(&self, group: usize, layer: usize) -> LayerFx {
        match (u32::try_from(group), u32::try_from(layer)) {
            (Ok(g), Ok(l)) => self.layers.get(&(g, l)).copied().unwrap_or_default(),
            _ => LayerFx::default(),
        }
    }

    /// `(plane, tmp)` sample counts of one worker's feather scratch for chunks of at most
    /// `width x height`: the chunk coverage plane and the horizontal pass over the chunk rows plus
    /// `max_support` margin rows above and below. `(0, 0)` without feathered layers.
    pub(super) fn feather_dims(
        &self,
        width: usize,
        height: usize,
    ) -> Result<(usize, usize), RenderError> {
        if self.kernels.is_empty() {
            return Ok((0, 0));
        }
        let rows = add_u(height, mul_u(self.max_support, 2)?)?;
        Ok((mul_u(width, height)?, mul_u(width, rows)?))
    }

    /// Bytes of one worker's feather scratch (see [`ExtPrep::feather_dims`]).
    pub(super) fn feather_scratch_bytes(
        &self,
        width: usize,
        height: usize,
    ) -> Result<u64, RenderError> {
        let (plane, tmp) = self.feather_dims(width, height)?;
        u64::try_from(add_u(plane, tmp)?)
            .ok()
            .and_then(|n| n.checked_mul(SAMPLE_BYTES))
            .ok_or_else(overflow)
    }
}

/// Sizes of the per-worker effect frame: the working rectangle of one effect tile (the tile plus
/// the effect margin on every side, at most `MAX_REFERENCE_SIDE` per side).
#[derive(Clone, Copy, Debug)]
pub(super) struct FrameShape {
    pub(super) pixels: usize,
    /// Levels of isolated-group nesting the layer content needs.
    pub(super) depth: usize,
    /// Feather scratch for the frame (zero without a feathered layer).
    pub(super) fx_plane: usize,
    pub(super) fx_tmp: usize,
    /// Largest tile side (the chunk side).
    pub(super) tile_cap: usize,
    /// Effects per layer (sizes the matte allocations of the reference).
    pub(super) effects: usize,
}

impl ExtPrep {
    /// The frame shape for chunks of side at most `chunk_side`; `None` without effect layers.
    pub(super) fn frame_shape(&self, chunk_side: usize) -> Result<Option<FrameShape>, RenderError> {
        if self.effects.is_empty() {
            return Ok(None);
        }
        let side = add_u(chunk_side, mul_u(self.max_margin, 2)?)?
            .min(usize::try_from(MAX_REFERENCE_SIDE).map_err(|_| overflow())?);
        let (fx_plane, fx_tmp) = self.feather_dims(side, side)?;
        Ok(Some(FrameShape {
            pixels: mul_u(side, side)?,
            depth: self.frame_depth,
            fx_plane,
            fx_tmp,
            tile_cap: chunk_side,
            effects: self.max_effects,
        }))
    }

    /// Upper bound of one worker's effect scratch in bytes: the content frame and its group levels
    /// (16 bytes per pixel each), the three reference planes (backdrop, content, result: 16 bytes
    /// per pixel each), the mattes and the reference's temporaries (4 bytes per pixel for each
    /// effect plus 12), and the frame's feather scratch. An estimate of the allocations of
    /// `render_layer_reference`, which allocates internally; not a measured value.
    pub(super) fn frame_scratch_bytes(&self, shape: &FrameShape) -> Result<u64, RenderError> {
        let to_u64 = |n: usize| u64::try_from(n).map_err(|_| overflow());
        let per_pixel = to_u64(shape.depth)?
            .checked_mul(16)
            .and_then(|d| d.checked_add(16 + 48))
            .and_then(|d| {
                to_u64(shape.effects.checked_add(12)?)
                    .ok()?
                    .checked_mul(4)?
                    .checked_add(d)
            })
            .ok_or_else(overflow)?;
        let fx = to_u64(add_u(shape.fx_plane, shape.fx_tmp)?)?
            .checked_mul(SAMPLE_BYTES)
            .ok_or_else(overflow)?;
        to_u64(shape.pixels)?
            .checked_mul(per_pixel)
            .and_then(|b| b.checked_add(fx))
            .ok_or_else(overflow)
    }

    pub(super) fn effect(&self, index: usize) -> Option<&EffectLayer> {
        self.effects.get(index)
    }
}

/// Per-worker feather scratch: the feathered coverage of the layer being composited (chunk-indexed)
/// and the horizontal-pass buffer. Sized once, charged with the rest of the scratch, never grown.
/// `frame` is the effect frame (only on the chunk-level scratch of a worker, `None` inside it, so a
/// layer with effects inside the content of a layer with effects is a typed refusal).
pub(super) struct Fx {
    pub(super) plane: Vec<f32>,
    tmp: Vec<f32>,
    pub(super) frame: Option<Box<Frame>>,
}
/// Working memory of one effect tile (see `effects.rs`): the layer content over the frame, the
/// group levels beneath it, and a frame-sized feather scratch for the layer's mask.
pub(super) struct Frame {
    pub(super) content: Vec<[f32; 4]>,
    pub(super) spare: Vec<Vec<[f32; 4]>>,
    pub(super) fx: Fx,
    pub(super) tile_cap: usize,
}
impl Fx {
    pub(super) fn new(plane: usize, tmp: usize, frame: Option<FrameShape>) -> Self {
        Self {
            plane: vec![0.0; plane],
            tmp: vec![0.0; tmp],
            frame: frame.map(|s| {
                Box::new(Frame {
                    content: vec![[0.0; 4]; s.pixels],
                    spare: (0..s.depth).map(|_| vec![[0.0; 4]; s.pixels]).collect(),
                    fx: Fx::new(s.fx_plane, s.fx_tmp, None),
                    tile_cap: s.tile_cap,
                })
            }),
        }
    }
}

/// Weighted sum of `values` by `taps` in binary64, rounded once.
fn convolve(
    taps: &[f32],
    mut value: impl FnMut(usize) -> Result<f32, RenderError>,
) -> Result<f32, RenderError> {
    let mut sum = 0.0_f64;
    for (t, w) in taps.iter().enumerate() {
        sum += f64::from(*w) * f64::from(value(t)?);
    }
    Ok(sum as f32)
}

impl Eval<'_, '_> {
    /// Blend-If entries of `layer` (empty: identity).
    pub(super) fn blend_set(&self, layer: &PLayer<'_>) -> &[BlendRangeEntry] {
        layer
            .fx
            .blend_if
            .and_then(|i| self.prepared.ext.blend_sets.get(i))
            .map_or(&[], Vec::as_slice)
    }

    /// When `layer` has a mask feather, fills `fx.plane` (chunk-indexed) with the feathered raw
    /// coverage over `region` and returns true: a separable Gaussian with
    /// `coverage_math::feather_kernel`, read with a `feather_support` margin and clamped at the
    /// mask plane border (horizontal pass over the region columns and margin rows, then vertical),
    /// applied before invert and density. Returns false for a layer without feather.
    pub(super) fn feather(
        &self,
        layer: &PLayer<'_>,
        region: Rect,
        chunk: Rect,
        fx: &mut Fx,
    ) -> Result<bool, RenderError> {
        let (Some(mask), Some(index)) = (layer.mask.as_ref(), layer.fx.feather) else {
            return Ok(false);
        };
        let kernel = self
            .prepared
            .ext
            .kernels
            .get(index)
            .ok_or_else(|| unsupported("plan_shape"))?;
        feather_plane(mask, kernel, region, chunk, fx)?;
        Ok(true)
    }

    /// Adjustment layer onto `buf` (a chunk-sized running buffer: the group backdrop, or the clip
    /// run buffer `G` when `clipped`): per pixel of `region`,
    /// `k = opacity * fill * mask * blend_if` and `adjust_math::composite_adjustment(mode, backdrop,
    /// prepared, k)`; alpha is unchanged, a transparent backdrop stays transparent. The Blend-If
    /// source colour of an adjustment layer is its own content `f(backdrop)` (Studio rule, Photoshop
    /// behaviour UNVERIFIED).
    pub(super) fn adjust(
        &self,
        layer: &PLayer<'_>,
        region: Rect,
        chunk: Rect,
        buf: &mut [[f32; 4]],
        fx: &mut Fx,
    ) -> Result<(), RenderError> {
        let adjustment = layer
            .fx
            .adjustment
            .and_then(|i| self.prepared.ext.adjustments.get(i))
            .ok_or_else(|| unsupported("plan_shape"))?;
        let blend_if = self.blend_set(layer);
        let feathered = self.feather(layer, region, chunk, fx)?;
        let plane = feathered.then_some(fx.plane.as_slice());
        let (x1, y1) = end(region)?;
        for y in region.y..y1 {
            for x in region.x..x1 {
                let at = index_in(chunk, x, y)?;
                let b = *buf.get(at).ok_or_else(overflow)?;
                if b[3] <= 0.0 {
                    continue;
                }
                let mask = super::mask_value(layer, plane, x, y, at)?;
                let mut k = layer.opacity * mask;
                if k <= 0.0 {
                    continue;
                }
                let backdrop = Rgba::new([b[0], b[1], b[2]], b[3]);
                if !blend_if.is_empty() {
                    let content = adjustment.apply(backdrop.rgb).map_err(adjust_error)?;
                    k *= blend_weight(blend_if, content, backdrop.rgb)?;
                    if k <= 0.0 {
                        continue;
                    }
                }
                let out = composite_adjustment(layer.mode, backdrop, adjustment, k)
                    .map_err(layer_error)?;
                *buf.get_mut(at).ok_or_else(overflow)? =
                    [out.rgb[0], out.rgb[1], out.rgb[2], out.a];
            }
        }
        Ok(())
    }
}

/// Feathered raw coverage of `mask` over `region` into `fx.plane` (indexed over `chunk`).
fn feather_plane(
    mask: &MaskView<'_>,
    kernel: &Kernel,
    region: Rect,
    chunk: Rect,
    fx: &mut Fx,
) -> Result<(), RenderError> {
    let (width, height) = (width_of(region)?, height_of(region)?);
    let rows = add_u(height, mul_u(kernel.support, 2)?)?;
    if fx.tmp.len() < mul_u(width, rows)? || fx.plane.len() < area(chunk)? {
        return Err(unsupported("feather_scratch"));
    }
    let support = to_i64(kernel.support)?;
    // Pass 1: horizontal, for the region columns over the margin rows; row `r` of the buffer is
    // plan row `region.y + r - support`.
    for r in 0..rows {
        let y = to_i64(r)?
            .checked_add(region.y)
            .and_then(|y| y.checked_sub(support))
            .ok_or_else(overflow)?;
        for c in 0..width {
            let x0 = to_i64(c)?.checked_add(region.x).ok_or_else(overflow)?;
            let value = convolve(&kernel.taps, |t| {
                let x = to_i64(t)?
                    .checked_add(x0)
                    .and_then(|x| x.checked_sub(support))
                    .ok_or_else(overflow)?;
                mask.raw_clamped(x, y)
            })?;
            let at = mul_u(r, width).and_then(|row| add_u(row, c))?;
            *fx.tmp.get_mut(at).ok_or_else(overflow)? = value;
        }
    }
    // Pass 2: vertical; output row `rr` reads buffer rows `rr ..= rr + 2 * support`.
    for rr in 0..height {
        for c in 0..width {
            let value = convolve(&kernel.taps, |t| {
                let at = add_u(rr, t)
                    .and_then(|r| mul_u(r, width))
                    .and_then(|row| add_u(row, c))?;
                fx.tmp.get(at).copied().ok_or_else(overflow)
            })?;
            let x = to_i64(c)?.checked_add(region.x).ok_or_else(overflow)?;
            let y = to_i64(rr)?.checked_add(region.y).ok_or_else(overflow)?;
            // A normalised kernel can leave the sum a rounding step outside [0, 1].
            *fx.plane
                .get_mut(index_in(chunk, x, y)?)
                .ok_or_else(overflow)? = value.clamp(0.0, 1.0);
        }
    }
    Ok(())
}
