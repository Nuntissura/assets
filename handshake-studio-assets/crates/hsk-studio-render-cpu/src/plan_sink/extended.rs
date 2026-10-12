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
//! (waves 1 and 2) keep their tables here and are evaluated in the `effects` submodule.
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
use hsk_studio_pigment::effect_math::window::{
    WindowBudget, WindowError, WindowLimits, layer_window_cost, stack_support_px,
};
use hsk_studio_pigment::effect_math::{EffectError, EffectOp};
use hsk_studio_pigment::render_plan::{
    BackendError, ExtOperator, PlanExtensions, SAMPLE_RANGE_OPERATOR,
};
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
/// Bytes the window arrays of one effect call hold next to pigment's own peak: the backdrop plane
/// (colour 12 + alpha 4 per pixel) and the content colour (12 per pixel).
const WINDOW_INPUT_BYTES_PER_PIXEL: u64 = 28;
/// Bytes of one content frame pixel (straight RGBA `f32`).
const FRAME_PIXEL_BYTES: u64 = 16;
/// Largest predicted work (inner-loop visits, `window_cost`) of one effect window. Cancel and
/// deadline are observed between chunks only, so the work of one window is bounded here: a layer
/// whose largest window is predicted above it is a typed refusal (`effect_work_limit`) before the
/// first sink call; a smaller chunk size shrinks the window (test `work_limit_and_hostile_geometry_stay_typed`).
pub const MAX_EFFECT_WINDOW_WORK: u64 = 1_000_000_000;

/// Envelope state of one layer: indices into the tables of [`ExtPrep`].
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct LayerFx {
    pub(super) adjustment: Option<usize>,
    pub(super) blend_if: Option<usize>,
    pub(super) feather: Option<usize>,
    pub(super) effect: Option<usize>,
}

/// The effects of one layer (list order), the margin around its content they read
/// (`stack_support_px`) and the content bounds (set by `prepare`; `None` until then or when the
/// content is absent).
pub(super) struct EffectLayer {
    pub(super) ops: Vec<EffectOp>,
    pub(super) margin: usize,
    bounds: Option<Rect>,
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
    /// Deepest isolated-group nesting under an effect layer's content (set by `prepare`).
    pub(super) frame_depth: usize,
    /// Bytes of the tables above, reserved for the whole run.
    pub(super) resident_bytes: u64,
    /// Widest kernel support over all feathered layers (the margin the scratch must hold).
    max_support: usize,
    /// The envelope path admits `MaskInput.density < 1`; the v1 path refuses it.
    pub(super) allow_density: bool,
}

/// Typed mapping of a coverage failure; the keys are pigment's (PLR-006): [`SAMPLE_RANGE_OPERATOR`]
/// and the envelope operators' own `ExtOperator::key`, what `admit_extended` reports for them.
pub(super) fn coverage_error(error: CoverageError) -> RenderError {
    match error {
        CoverageError::OutOfDomain => unsupported(SAMPLE_RANGE_OPERATOR),
        CoverageError::FeatherOutOfRange => unsupported(ExtOperator::MaskFeather.key()),
        CoverageError::RangeOrder => unsupported(ExtOperator::BlendIf.key()),
    }
}
fn adjust_error(error: AdjustError) -> RenderError {
    match error {
        AdjustError::OutOfDomain => unsupported(SAMPLE_RANGE_OPERATOR),
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
        EffectError::PlaneShape => unsupported(EffectError::PlaneShape.code()),
        EffectError::OverBudget => unsupported(EffectError::OverBudget.code()),
        // A feature pigment recognises and refuses (gradient glow, glow range, jitter, Precise):
        // its own key, what `admit_extended` reports for it.
        EffectError::Unsupported(feature) => unsupported(feature.key()),
        EffectError::Overflow => overflow(),
        EffectError::Blend(e) => blend_error(e),
        _ => unsupported("effect_parameter"),
    }
}

/// Typed mapping of a windowed-evaluation failure: a byte limit is `BudgetExceeded` with pigment's
/// predicted need, a work limit is the refusal `effect_work_limit`.
pub(super) fn window_error(error: WindowError) -> RenderError {
    match error {
        WindowError::Effect(e) => effect_error(e),
        WindowError::OverBudget {
            budget: WindowBudget::Bytes,
            needed,
            limit,
        } => RenderError::Backend(BackendError::BudgetExceeded {
            requested: needed,
            limit,
        }),
        WindowError::OverBudget { .. } => unsupported("effect_work_limit"),
        WindowError::Overflow => overflow(),
        WindowError::AllocationFailed => {
            RenderError::Backend(BackendError::Unavailable("effect_allocation"))
        }
        WindowError::TooManyEffects => unsupported("effect_limit"),
        WindowError::WindowShape | WindowError::Geometry => {
            unsupported(EffectError::PlaneShape.code())
        }
        WindowError::OutsideDomain => unsupported("effect_window_domain"),
        WindowError::Source => unsupported("effect_source"),
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
    /// Only the entries of groups in `reach` (the nodes the root reaches, as `prepare` computes
    /// them) are prepared: an entry on any other group renders nothing and charges nothing to
    /// `limit` (test `tests/cpu_envelope_reach.rs`
    /// `unreachable_envelope_entries_are_neither_admitted_nor_prepared`: the pixel and
    /// `peak_working_bytes` equal the plain plan's); pigment's admission skips its operators the
    /// same way.
    pub(super) fn build(
        extensions: &PlanExtensions,
        reach: &[bool],
        limit: u64,
    ) -> Result<Self, RenderError> {
        let mut prep = Self {
            allow_density: true,
            ..Self::default()
        };
        let mut kernel_of: BTreeMap<u32, usize> = BTreeMap::new();
        for e in &extensions.layers {
            if !usize::try_from(e.group.0)
                .ok()
                .and_then(|group| reach.get(group))
                .copied()
                .unwrap_or(false)
            {
                continue;
            }
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
                // The margin the stack reads around the layer content: the shared domain of a
                // windowed evaluation (`WindowGeometry::uncut`). Any size is admitted here; the
                // memory and work of a window are charged and bounded in `frame_shape`.
                let margin = to_usize(u64::from(
                    stack_support_px(&e.effects).map_err(effect_error)?,
                ))?;
                let bytes = u64::try_from(e.effects.len())
                    .ok()
                    .and_then(|n| n.checked_mul(EFFECT_OP_BYTES))
                    .ok_or_else(overflow)?;
                prep.charge(bytes, limit)?;
                fx.effect = Some(prep.effects.len());
                prep.effects.push(EffectLayer {
                    ops: e.effects.clone(),
                    margin,
                    bounds: None,
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
                        return Err(unsupported(ExtOperator::MaskFeather.key()));
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

/// Sizes of the per-worker effect frame: the layer content over the part of the layer that the
/// alpha reads of one window can reach (the window grown by the stack margin, clipped to the layer
/// bounds), the group levels beneath it, the frame's feather scratch, and the transient memory of
/// one windowed call.
#[derive(Clone, Copy, Debug)]
pub(super) struct FrameShape {
    pub(super) pixels: usize,
    /// Levels of isolated-group nesting the layer content needs.
    pub(super) depth: usize,
    /// Feather scratch for the frame (zero without a feathered layer).
    pub(super) fx_plane: usize,
    pub(super) fx_tmp: usize,
    /// Transient bytes of one window call: the backdrop and content colour arrays plus pigment's
    /// predicted peak (`layer_window_cost`), largest over the effect layers.
    pub(super) window_bytes: u64,
    /// Limits handed to every window call: pigment's predicted peak of the largest window and
    /// [`MAX_EFFECT_WINDOW_WORK`].
    pub(super) limits: WindowLimits,
}

impl ExtPrep {
    /// Records the content bounds of effect layer `index` (the layer's node bounds, before the
    /// margin is added); called by `prepare`.
    pub(super) fn set_effect_bounds(&mut self, index: usize, bounds: Option<Rect>) {
        if let Some(layer) = self.effects.get_mut(index) {
            layer.bounds = bounds;
        }
    }

    /// The frame shape for chunks of at most `chunk_width x chunk_height`; `None` without effect
    /// layers. For every effect layer with content the largest window is the chunk clipped to the
    /// layer grown by its margin; `layer_window_cost` of that window gives pigment's predicted peak
    /// bytes and work (before any read or allocation). A predicted work above
    /// [`MAX_EFFECT_WINDOW_WORK`] is the typed refusal `effect_work_limit`.
    pub(super) fn frame_shape(
        &self,
        chunk_width: usize,
        chunk_height: usize,
    ) -> Result<Option<FrameShape>, RenderError> {
        if self.effects.is_empty() {
            return Ok(None);
        }
        let (mut pixels, mut fx_plane, mut fx_tmp) = (0_usize, 0_usize, 0_usize);
        let (mut window_bytes, mut peak) = (0_u64, 0_u64);
        for layer in &self.effects {
            let Some(bounds) = layer.bounds else {
                continue;
            };
            let (bounds_width, bounds_height) = (width_of(bounds)?, height_of(bounds)?);
            if bounds_width == 0 || bounds_height == 0 {
                continue;
            }
            let twice = mul_u(layer.margin, 2)?;
            let window_width = chunk_width.min(add_u(bounds_width, twice)?);
            let window_height = chunk_height.min(add_u(bounds_height, twice)?);
            let frame_width = bounds_width.min(add_u(window_width, twice)?);
            let frame_height = bounds_height.min(add_u(window_height, twice)?);
            pixels = pixels.max(mul_u(frame_width, frame_height)?);
            let (plane, tmp) = self.feather_dims(frame_width, frame_height)?;
            fx_plane = fx_plane.max(plane);
            fx_tmp = fx_tmp.max(tmp);
            let window = Rect {
                x: 0,
                y: 0,
                width: u32::try_from(window_width).map_err(|_| overflow())?,
                height: u32::try_from(window_height).map_err(|_| overflow())?,
            };
            let cost = layer_window_cost(&layer.ops, window).map_err(window_error)?;
            if cost.work > MAX_EFFECT_WINDOW_WORK {
                return Err(unsupported("effect_work_limit"));
            }
            let inputs = u64::try_from(mul_u(window_width, window_height)?)
                .ok()
                .and_then(|n| n.checked_mul(WINDOW_INPUT_BYTES_PER_PIXEL))
                .ok_or_else(overflow)?;
            window_bytes =
                window_bytes.max(inputs.checked_add(cost.peak_bytes).ok_or_else(overflow)?);
            peak = peak.max(cost.peak_bytes);
        }
        Ok(Some(FrameShape {
            pixels,
            depth: self.frame_depth,
            fx_plane,
            fx_tmp,
            window_bytes,
            limits: WindowLimits::new(peak, MAX_EFFECT_WINDOW_WORK),
        }))
    }

    /// One worker's effect scratch in bytes: the content frame and its group levels (16 bytes per
    /// pixel each), the frame's feather scratch, and the transient memory of one window call
    /// (`FrameShape::window_bytes`, pigment's own prediction plus the two input arrays).
    pub(super) fn frame_scratch_bytes(&self, shape: &FrameShape) -> Result<u64, RenderError> {
        let to_u64 = |n: usize| u64::try_from(n).map_err(|_| overflow());
        let levels = to_u64(shape.depth)?.checked_add(1).ok_or_else(overflow)?;
        let frame = to_u64(shape.pixels)?
            .checked_mul(FRAME_PIXEL_BYTES)
            .and_then(|b| b.checked_mul(levels))
            .ok_or_else(overflow)?;
        let fx = to_u64(add_u(shape.fx_plane, shape.fx_tmp)?)?
            .checked_mul(SAMPLE_BYTES)
            .ok_or_else(overflow)?;
        frame
            .checked_add(fx)
            .and_then(|b| b.checked_add(shape.window_bytes))
            .ok_or_else(overflow)
    }

    pub(super) fn effect(&self, index: usize) -> Option<&EffectLayer> {
        self.effects.get(index)
    }
}

/// Per-worker feather scratch: the feathered coverage of the layer being composited (chunk-indexed)
/// and the horizontal-pass buffer. Sized once and charged with the rest of the scratch; the length is not changed afterwards.
/// `frame` is the effect frame (only on the chunk-level scratch of a worker, `None` inside it, so a
/// layer with effects inside the content of a layer with effects is a typed refusal).
pub(super) struct Fx {
    pub(super) plane: Vec<f32>,
    tmp: Vec<f32>,
    pub(super) frame: Option<Box<Frame>>,
}
/// Working memory of one effect window (see `effects.rs`): the layer content over the frame, the
/// group levels beneath it, a frame-sized feather scratch for the layer's mask, and the limits of
/// the windowed call.
pub(super) struct Frame {
    pub(super) content: Vec<[f32; 4]>,
    pub(super) spare: Vec<Vec<[f32; 4]>>,
    pub(super) fx: Fx,
    pub(super) limits: WindowLimits,
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
                    limits: s.limits,
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
