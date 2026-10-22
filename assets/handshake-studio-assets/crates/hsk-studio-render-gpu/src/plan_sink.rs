//! GPU `RenderBackend` for the shared pigment render plan: admission (`admit`, before any device
//! work), source verification, then bounded `chunk x chunk` execution of one WGSL compute pass per
//! layer (and per nested-group composite), read back into the caller's `TileSink`.
//!
//! Scope: `Solid` and `Tile` leaves; every layer-applicable separable, non-separable or
//! whole-pixel blend mode with a usable pigment formula (class Exact or Approximate) plus Normal (the list is derived
//! from pigment's mode facts in `kernel.rs`; an Approximate mode keeps pigment's class in the
//! receipt, the backend only lowers a class); layer opacity times fill
//! opacity; coverage masks (`coverage_u8` / `coverage_u16le`, inversion, density 1); isolated
//! groups (own transparent buffer per nesting level, then composited with the layer's mode,
//! opacity and mask) and pass-through groups (layers composited onto the parent's running
//! backdrop). Straight alpha, `rgb_f32le` in the plan's colour tag. The plan-extension envelope
//! (`execute_extended`) adds mask density and feather, Blend-If, adjustment layers and the layer
//! effects of pigment's `effect_math`, which render through `eval_effect_layer` and `fx.rs`.
//! Everything else (other modes,
//! vector, mask density, pass-through opacity, non-isolated blended groups,
//! premultiplied alpha) is refused by `admit` or source preparation as `Unsupported`, typed and
//! before any upload. There is no CPU fallback: no adapter is `Unavailable`, a lost device is
//! `DeviceLost`.
//!
//! Numerics: the shader reproduces `hsk_studio_pigment::blend_math::composite_over` in f32. GPU
//! fused multiply-add, division and square root may differ from the CPU reference in the last
//! ulps, so the backend reports class `Fitted` (tolerance-equal, not bit-exact) for the blend
//! operators. Mask coverages are computed on the host with render-cpu's exact conversion, so the
//! mask values the shader multiplies by are bit-identical to the CPU path's.
#![deny(clippy::arithmetic_side_effects)]
use crate::device::{DeviceInfo, DeviceOptions, GpuContext};
use crate::error::{GpuError, Reason};
use crate::pipeline::{ErrorScopes, PipelineFault};
use crate::ext::{
    AdjustGpu, BlendIfGpu, EXT_BYTES, FeatherError, FeatherPlane, MIN_LUT_BYTES,
    compile_adjustment, ext_bytes, feather_plane, opcode_of,
};
use crate::fx::{
    Buf, FX_PASS_MAX_OPS, FXP_BYTES, FxEffect, FxError, FxKernels, FxLayer, FxStep, compile_effect,
    margin_of, params_bytes, shape_alpha_step,
};
use crate::kernel::{
    BLUR_MAX_TAPS, BLURP_BYTES, BlurOp, FilterOp, LayerParams, MASK_TEXEL_BYTES, PARAMS_BYTES,
    RESAMPLE_PARAMS_BYTES, SITE_BYTES, SourceKind, TEXEL_BYTES, blur_params_bytes, filter_params_bytes,
    gpu_adjust_supported, gpu_blend_supported,
};
use hsk_studio_pigment::blur_math::{BlurScope, LayerBlur};
use hsk_studio_pigment::filter_math::{LayerFilter, MAX_FILTERS_PER_LAYER, NoiseDistribution};
use crate::residency::{Residency, ResidentKey};
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::effect_math::EffectKind;
use hsk_studio_pigment::transform_math::{
    EdgeMode, PreparedTransform, ResampleFilter, SampleSite, TransformError,
};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, ExtendedOp, OutputStage, BackendIdentity, ClipRole, ColourTag, DeviceClass, Exactness,
    ExactnessLedger, ExecuteRequest, ExtOperator, Group, Knockout, MASK_DENSITY_OPERATOR,
    MaskFormat, NoVector, Operator, PASS_THROUGH_FILL_OPERATOR, PASS_THROUGH_MASK_OPERATOR,
    PlanError, PlanExtensions, PlanNode, PlanNodeId, PlanSources, RenderBackend, RenderPlan,
    RenderReceipt, SAMPLE_RANGE_OPERATOR, SampleFormat, SourceKey, StudioBlendMode, TileInput,
    TilePlanes, TileSink, admit, admit_extended,
};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

pub const RENDERER_NAME: &str = "hsk-studio-render-gpu";
pub const RENDERER_VERSION: &str = "0.1";
/// Smallest chunk (texels) the executor will shrink to before reporting `BudgetExceeded`.
const MIN_CHUNK_TEXELS: u64 = 64;
/// Smallest output side of a chunk of a plan with layer effects before it is refused (the frame
/// around it adds the effect margin on every side).
const MIN_FX_SIDE: u32 = 8;
/// Bytes per frame texel of the three f32 matte planes.
const FX_PLANE_TEXEL_BYTES: u64 = 12;
const DEFAULT_POLL_LIMIT: Duration = Duration::from_secs(60);

/// Counters of one successful execution (device-side detail the shared receipt has no field for).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuStats {
    /// Tile regions plus mask coverages written to the device.
    pub bytes_uploaded: u64,
    pub bytes_read_back: u64,
    /// Highest simultaneous byte count charged to residency during the call.
    pub peak_resident_bytes: u64,
    pub evictions: u64,
    pub dispatches: u64,
    pub chunk_width: u32,
    pub chunk_height: u32,
    /// Chunk-sized destination buffers alive (1 + isolated-group nesting depth).
    pub levels: u32,
}

// ---- prepared (verified) plan -----------------------------------------------------------------

#[derive(Clone, Copy)]
struct MaskView<'a> {
    bounds: Rect,
    wide: bool,
    bytes: &'a [u8],
    stride: usize,
    inverted: bool,
    /// Mask density (`1.0` = plain mask; below 1 only through the plan-extension envelope).
    density: f32,
    /// Index into `Prepared::feathered` of the blurred coverage of this layer's region.
    feather: Option<usize>,
    /// A linked mask of a transformed layer: resampled on the host by pigment's binary64
    /// `PreparedTransform::sample_coverage` (bit-identical to render-cpu), zero outside the plane.
    xform: Option<PreparedTransform>,
}
impl MaskView<'_> {
    /// [`MaskView::raw`], or the resampled coverage of a transformed (linked) mask.
    fn stored(&self, x: i64, y: i64) -> f32 {
        let Some(t) = &self.xform else {
            return self.raw(x, y);
        };
        let (bx, by) = (self.bounds.x, self.bounds.y);
        t.sample_coverage(x, y, |i, j| {
            Ok::<f32, ()>(self.raw(
                bx.saturating_add(i64::from(i)),
                by.saturating_add(i64::from(j)),
            ))
        })
        .unwrap_or(0.0)
    }
    /// Stored coverage before feather, inversion and density. Same conversion as render-cpu: f64
    /// division, one rounding to f32. Inside a plain execution `(x, y)` stays inside the verified
    /// plane bounds. A plan with layer effects evaluates frames that reach beyond the target, and
    /// there a coordinate beyond the plane reads the nearest plane texel (render-cpu's clamped
    /// read); the plane's data beyond the target is unknown either way.
    fn raw(&self, x: i64, y: i64) -> f32 {
        let last_x = i64::from(self.bounds.width.saturating_sub(1));
        let last_y = i64::from(self.bounds.height.saturating_sub(1));
        let (Some(dx), Some(dy)) = (
            x.checked_sub(self.bounds.x).map(|v| v.clamp(0, last_x)),
            y.checked_sub(self.bounds.y).map(|v| v.clamp(0, last_y)),
        ) else {
            return 0.0;
        };
        let (Ok(dx), Ok(dy)) = (usize::try_from(dx), usize::try_from(dy)) else {
            return 0.0;
        };
        let Some(row) = dy.checked_mul(self.stride) else {
            return 0.0;
        };
        if self.wide {
            let at = dx.checked_mul(2).and_then(|c| row.checked_add(c));
            match at.and_then(|at| Some((self.bytes.get(at)?, self.bytes.get(at.checked_add(1)?)?)))
            {
                Some((lo, hi)) => (f64::from(u16::from_le_bytes([*lo, *hi])) / 65535.0) as f32,
                None => 0.0,
            }
        } else {
            match row.checked_add(dx).and_then(|at| self.bytes.get(at)) {
                Some(v) => (f64::from(*v) / 255.0) as f32,
                None => 0.0,
            }
        }
    }
    /// Coverage after feather (the envelope's blurred plane, else the stored value) and inversion;
    /// the density step runs in the shader.
    fn coverage(&self, feathered: &[FeatherPlane], x: i64, y: i64) -> f32 {
        let c = match self.feather.and_then(|i| feathered.get(i)) {
            Some(plane) => plane.at(x, y),
            None => self.stored(x, y),
        };
        if self.inverted { 1.0 - c } else { c }
    }
}

/// The weight of [`Exec::composite_mix`].
#[derive(Clone, Copy)]
enum MixWeight {
    /// The shape alpha already in the mask buffer (the interior unit).
    MaskBuffer,
    /// A uniform weight (the unit fade of a layer with effects, a pass-through layer's opacity).
    Uniform(f32),
}

/// A layer's opacity for one composite: the folded product (`opacity * fill`, every mode but the
/// special eight), and the opacity without the fill plus the fill (pigment `composite_over_fill`).
#[derive(Clone, Copy, Debug)]
struct Opac {
    folded: f32,
    unfilled: f32,
    fill: f32,
}
impl Opac {
    const fn plain(v: f32) -> Self {
        Self { folded: v, unfilled: v, fill: 1.0 }
    }
}
impl PLayer<'_> {
    const fn opac(&self) -> Opac {
        Opac { folded: self.opacity, unfilled: self.layer_opacity, fill: self.fill }
    }
}

struct PLayer<'a> {
    input: usize,
    /// Clipped to the nearest earlier unclipped layer of the group (pigment `BaseAlpha` route).
    clipped: bool,
    mode: StudioBlendMode,
    /// `opacity * fill_opacity` (fill folded in as a product, class Approximate, as render-cpu).
    opacity: f32,
    /// The layer opacity alone: the effects of a layer fade with it and not with the fill.
    layer_opacity: f32,
    /// The fill opacity alone (lane SMB-PS-ORACLE-PARITY-1: Photoshop's special-eight modes apply
    /// it inside the blend, pigment `composite_over_fill`).
    fill: f32,
    mask: Option<MaskView<'a>>,
    /// Blend-If entries of the plan-extension envelope (`None` = the v1 composite).
    blend_if: Option<BlendIfGpu>,
    /// Adjustment layer of the envelope: the layer's input is the transparent placeholder and the
    /// content is `f(running backdrop)`.
    adjust: Option<Box<AdjustGpu>>,
    /// Layer effects of the envelope (pigment `effect_math`), compiled into matte passes.
    fx: Option<Box<FxLayer>>,
    /// Layer or backdrop blur of the envelope (pigment `blur_math`, radius above 0).
    blur: Option<Box<BlurGpu>>,
    /// Layer filters of the envelope (pigment `filter_math`, lane SMB-RENDER-FILTERS).
    filter: Option<Box<FilterGpu>>,
    /// Smart-filter stack of the envelope (lane SMB-PS-SMART-FILTER-WIRING).
    smart: Option<Box<SmartGpu>>,
    /// Knockout of the envelope (lane SMB-PS-ADJUST-STYLES-3, `knockout` module).
    knockout: Knockout,
    /// The plan node of the group holding the layer (the knockout snapshot key).
    group: usize,
}

/// A smart-filter stack compiled for the filter passes: per enabled entry its filter chain,
/// opacity and mask (index into `masks`), the stack mask, the summed frame reach, and the plane it
/// renders (content bounds inside the extent).
struct SmartGpu {
    entries: Vec<(FilterGpu, f32, Option<usize>)>,
    /// Decoded masks of the stack (lane SMB-PS-ADJUST-STYLES-3, `smart_mask`).
    masks: Vec<smart_mask::CovPlane>,
    stack_mask: Option<usize>,
    reach: u32,
    plane: Option<Rect>,
}

/// Compiles a smart-filter stack; liquify entries have no GPU pass yet (typed refusal before any
/// device work). Masks (lane SMB-PS-ADJUST-STYLES-3) are checked and decoded like render-cpu does
/// for every entry (also the disabled ones, as render-cpu resolves them), charged to `room`; only
/// the enabled entries keep theirs.
fn compile_smart(
    op: &hsk_studio_pigment::smart_filter::SmartFilterOp,
    plane: Option<Rect>,
    node: u32,
    sources: &dyn PlanSources,
    room: &mut u64,
) -> Result<(SmartGpu, u64), GpuError> {
    use hsk_studio_pigment::smart_filter::StackFilter;
    if !op.check() {
        return Err(GpuError::Backend(BackendError::InvalidPlan(PlanError::SmartFilterParameter { node })));
    }
    let mut masks = Vec::new();
    let mut entry_masks = Vec::with_capacity(op.entries.len());
    for e in &op.entries {
        entry_masks.push(match &e.mask {
            Some(m) => {
                masks.push(smart_mask::resolve(sources, m, node, room)?);
                Some(masks.len().saturating_sub(1))
            }
            None => None,
        });
    }
    let stack_mask = match &op.stack_mask {
        Some(m) => {
            masks.push(smart_mask::resolve(sources, m, node, room)?);
            Some(masks.len().saturating_sub(1))
        }
        None => None,
    };
    let (mut entries, mut reach, mut ops) = (Vec::new(), 0u32, 1u64);
    for (e, mask) in op.entries.iter().zip(entry_masks).filter(|(e, _)| e.enabled && e.opacity > 0.0) {
        let f = match &e.filter {
            StackFilter::Filter(f) => f,
            StackFilter::Liquify { .. } => return Err(unsupported("smart_filter_liquify")),
        };
        let (compiled, pass_ops) = compile_filters(std::slice::from_ref(f), node)?;
        reach = reach.checked_add(compiled.reach).ok_or_else(overflow)?;
        ops = ops.max(pass_ops);
        entries.push((compiled, e.opacity, mask));
    }
    Ok((SmartGpu { entries, masks, stack_mask, reach, plane }, ops))
}

/// Largest median radius the shader sorts in registers (a disc of at most 149 samples per channel).
/// Pigment admits 1..=500; a larger radius is the typed refusal `filter_median_radius`.
const MEDIAN_GPU_MAX_RADIUS: u32 = 7;

/// A filter chain compiled for the passes of `filter.wgsl` and `blur.wgsl`.
struct FilterGpu {
    stages: Vec<FilterStageGpu>,
    /// Pixels the output of the whole chain reaches beyond the content bounds (blur support, median
    /// radius; point filters add none).
    growth: u32,
    /// Pixels the chain reads beyond a kept pixel: the frame margin it needs (sum over the stages).
    reach: u32,
}
enum FilterStageGpu {
    Gaussian(BlurGpu),
    Unsharp { blur: BlurGpu, amount: f32, threshold: f32 },
    HighPass { blur: BlurGpu },
    Median { radius: u32 },
    Noise { amount: f32, gaussian: bool, mono: bool, seed: u64 },
}

/// Compiles the filters of one layer; every refusal is typed and happens before any device work.
fn compile_filters(filters: &[LayerFilter], node: u32) -> Result<(FilterGpu, u64), GpuError> {
    let parameter = || {
        GpuError::Backend(BackendError::InvalidPlan(PlanError::FilterParameter { node }))
    };
    if filters.len() > MAX_FILTERS_PER_LAYER {
        return Err(parameter());
    }
    let blur_of = |radius_px: f32| -> Result<BlurGpu, GpuError> {
        let b = LayerBlur::new(BlurScope::Layer, radius_px);
        let taps = b.taps().map_err(|_| parameter())?;
        if taps.len() > BLUR_MAX_TAPS {
            return Err(unsupported("blur_parameter"));
        }
        let support = b.support_px().map_err(|_| parameter())?;
        Ok(BlurGpu {
            scope: BlurScope::Layer,
            taps,
            support,
        })
    };
    let (mut stages, mut growth, mut reach, mut pass_ops) = (Vec::new(), 0u32, 0u32, 1u64);
    let add = |total: u32, n: u32| total.checked_add(n).ok_or_else(overflow);
    for f in filters {
        f.validate().map_err(|_| parameter())?;
        let stage = match *f {
            LayerFilter::GaussianBlur { radius_px } => {
                let b = blur_of(radius_px)?;
                growth = add(growth, b.support)?;
                reach = add(reach, b.support)?;
                pass_ops = pass_ops.max(u64::try_from(b.taps.len()).map_err(|_| overflow())?);
                FilterStageGpu::Gaussian(b)
            }
            LayerFilter::UnsharpMask { amount_pct, radius_px, threshold_levels } => {
                let blur = blur_of(radius_px)?;
                reach = add(reach, blur.support)?;
                pass_ops = pass_ops.max(u64::try_from(blur.taps.len()).map_err(|_| overflow())?);
                FilterStageGpu::Unsharp {
                    blur,
                    amount: (f64::from(amount_pct) / 100.0) as f32,
                    threshold: f32::from(threshold_levels),
                }
            }
            LayerFilter::HighPass { radius_px } => {
                let blur = blur_of(radius_px)?;
                reach = add(reach, blur.support)?;
                pass_ops = pass_ops.max(u64::try_from(blur.taps.len()).map_err(|_| overflow())?);
                FilterStageGpu::HighPass { blur }
            }
            LayerFilter::Median { radius_px } => {
                if radius_px > MEDIAN_GPU_MAX_RADIUS {
                    return Err(unsupported("filter_median_radius"));
                }
                growth = add(growth, radius_px)?;
                reach = add(reach, radius_px)?;
                // Disc samples n; the insertion sort costs about n * n / 2 per channel.
                let side = u64::from(radius_px).saturating_mul(2).saturating_add(1);
                let n = side.saturating_mul(side);
                pass_ops = pass_ops.max(n.saturating_mul(n).saturating_mul(2));
                FilterStageGpu::Median { radius: radius_px }
            }
            LayerFilter::AddNoise { amount_pct, distribution, monochromatic, seed } => {
                FilterStageGpu::Noise {
                    amount: (f64::from(amount_pct) / 100.0 * 0.5) as f32,
                    gaussian: distribution == NoiseDistribution::Gaussian,
                    mono: monochromatic,
                    seed,
                }
            }
        };
        stages.push(stage);
    }
    Ok((FilterGpu { stages, growth, reach }, pass_ops))
}

/// A blur compiled for the passes of `blur.wgsl`: the scope, pigment's taps and their half width.
struct BlurGpu {
    scope: BlurScope,
    taps: Vec<f32>,
    support: u32,
}
enum PNode<'a> {
    Unused,
    Tile {
        planes: TilePlanes<'a>,
        rect: Rect,
    },
    Solid {
        rgba: [f32; 4],
    },
    Group {
        layers: Vec<PLayer<'a>>,
        isolated: bool,
    },
    /// A tile resampled through a layer transform (envelope `LayerExtension::transform`): a node of
    /// `Prepared::extra`, evaluated by the resample pass (`resample.wgsl`).
    Transformed {
        planes: TilePlanes<'a>,
        prep: PreparedTransform,
    },
}
struct Prepared<'a> {
    nodes: Vec<PNode<'a>>,
    bounds: Vec<Option<Rect>>,
    /// Transformed-tile nodes, addressed as `nodes.len() + k` by the layers that use them.
    extra: Vec<PNode<'a>>,
    extra_bounds: Vec<Option<Rect>>,
    /// Texels of the largest transformed tile: the resample source window is sized to hold it.
    transform_window_texels: u64,
    /// Extra destination levels an evaluation of the node needs below its own destination.
    need: Vec<usize>,
    root: usize,
    reachable: u64,
    has_tiles: bool,
    has_masks: bool,
    /// Blurred mask coverage of the envelope's feathered layers (`MaskView::feather` indexes it).
    feathered: Vec<FeatherPlane>,
    /// Some layer uses Blend-If or an adjustment: the envelope buffers and pipelines are needed.
    has_ext: bool,
    /// Largest table (curve samples, gradient-map stops, colour-lookup cube, selective-colour
    /// ranges, replace-colour samples) of any adjustment, in f32 values: the `lut` binding size.
    lut_floats: usize,
    /// Some reachable layer has effects: the frame, the matte planes and the effect pipeline exist.
    has_fx: bool,
    /// Blur kernels of all effects (charged to the working-byte limit while compiling).
    fx_kernels: FxKernels,
    /// Decoded pattern pixels of the pattern overlays (straight RGBA by plan source key).
    patterns: std::collections::BTreeMap<SourceKey, Vec<[f32; 4]>>,
    /// Largest per-pixel window cost of any effect pass (the chunk shrinks to bound a pass).
    fx_window_ops: u64,
    /// Margin in pixels every chunk is evaluated with beyond its own rectangle: the largest sum,
    /// along any chain of nested layers with effects, of the margins the effects read.
    frame_margin: u32,
    /// Some reachable layer has a blur: the blur pipeline, parameter block and two frame-sized
    /// scratch buffers exist.
    has_blur: bool,
    /// Most taps of any blur (a blur pass costs taps x frame pixels).
    blur_taps: u64,
    /// The plan's output stage (sample range, stage-6a display transform).
    output: OutputStage,
    /// Knockout layers, their snapshot groups and deep chains (lane SMB-PS-ADJUST-STYLES-3).
    ko: knockout::KoPlan,
}

impl<'a> Prepared<'a> {
    /// Node `index` of the plan, or transformed-tile node `index - nodes.len()`.
    fn node(&self, index: usize) -> Result<&PNode<'a>, GpuError> {
        match index.checked_sub(self.nodes.len()) {
            None => self.nodes.get(index),
            Some(k) => self.extra.get(k),
        }
        .ok_or_else(|| unsupported("plan_shape"))
    }
    /// Bounds of [`Prepared::node`] `index`.
    fn bound(&self, index: usize) -> Option<Rect> {
        match index.checked_sub(self.bounds.len()) {
            None => self.bounds.get(index).copied().flatten(),
            Some(k) => self.extra_bounds.get(k).copied().flatten(),
        }
    }
}
fn transform_invalid(node: u32, error: TransformError) -> GpuError {
    GpuError::Backend(BackendError::InvalidPlan(PlanError::TransformMatrix {
        node,
        error,
    }))
}

fn unsupported(operator: &'static str) -> GpuError {
    GpuError::Backend(BackendError::Unsupported {
        operator,
        exactness: Exactness::Unsupported,
    })
}
/// The refusal key of a non-identity Blend-If on `group.layers[index]` when its clip role has none
/// defined: the base of a clipping run or a clipped layer. Roles are pigment's
/// (`Group::clip_role`) and keys are pigment's (`ExtOperator::key`), so this backend and shared
/// admission cannot disagree. `None` for an unclipped layer (or an index outside the group).
fn blend_if_clip_refusal(group: &Group, index: usize) -> Option<&'static str> {
    match group.clip_role(index)? {
        ClipRole::Base => Some(ExtOperator::BlendIfClipBase.key()),
        ClipRole::Clipped => Some(ExtOperator::BlendIfClippedLayer.key()),
        ClipRole::Unclipped => None,
    }
}
fn mismatch(key: SourceKey) -> GpuError {
    GpuError::Backend(BackendError::SourceMismatch(key))
}
fn overflow() -> GpuError {
    GpuError::Backend(BackendError::Overflow)
}
/// `rect` grown by `margin` pixels on every side.
fn grow_rect(rect: Rect, margin: u32) -> Result<Rect, GpuError> {
    let m = i64::from(margin);
    let side = |v: u32| u32::try_from(i64::from(v).checked_add(m.checked_mul(2)?)?).ok();
    Ok(Rect {
        x: rect.x.checked_sub(m).ok_or_else(overflow)?,
        y: rect.y.checked_sub(m).ok_or_else(overflow)?,
        width: side(rect.width).ok_or_else(overflow)?,
        height: side(rect.height).ok_or_else(overflow)?,
    })
}
/// Typed refusal of an effect that could not be compiled.
fn fx_error(e: FxError, limit: u64) -> GpuError {
    match e {
        FxError::Overflow => overflow(),
        FxError::Unsupported(key) => unsupported(key),
        FxError::Budget { requested } => GpuError::BudgetExceeded {
            requested,
            budget: limit,
        },
    }
}
fn end(rect: Rect) -> Result<(i64, i64), GpuError> {
    rect.end().map_err(|_| overflow())
}
fn intersect(a: Rect, b: Rect) -> Result<Option<Rect>, GpuError> {
    a.intersect(b).map_err(|_| overflow())
}
fn union(a: Option<Rect>, b: Option<Rect>) -> Result<Option<Rect>, GpuError> {
    let (Some(a), Some(b)) = (a, b) else {
        return Ok(a.or(b));
    };
    let (ax1, ay1) = end(a)?;
    let (bx1, by1) = end(b)?;
    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
    Ok(Some(Rect {
        x,
        y,
        width: ax1
            .max(bx1)
            .checked_sub(x)
            .and_then(|w| u32::try_from(w).ok())
            .ok_or_else(overflow)?,
        height: ay1
            .max(by1)
            .checked_sub(y)
            .and_then(|h| u32::try_from(h).ok())
            .ok_or_else(overflow)?,
    }))
}
/// The little-endian `f32` at byte `at`; the planes' shape was verified by `plane_ok`, and a
/// position outside them reads as 0 instead of panicking.
fn f32_at(bytes: &[u8], at: usize) -> f32 {
    at.checked_add(4)
        .and_then(|end| bytes.get(at..end))
        .and_then(|word| <[u8; 4]>::try_from(word).ok())
        .map_or(0.0, f32::from_le_bytes)
}
fn tile_pixel(p: &TilePlanes<'_>, x: u32, y: u32) -> [f32; 4] {
    let (x, y) = (x as usize, y as usize);
    let c = y
        .saturating_mul(p.colour_stride_bytes as usize)
        .saturating_add(x.saturating_mul(12));
    let a = y
        .saturating_mul(p.alpha_stride_bytes as usize)
        .saturating_add(x.saturating_mul(4));
    [
        f32_at(p.colour, c),
        f32_at(p.colour, c.saturating_add(4)),
        f32_at(p.colour, c.saturating_add(8)),
        f32_at(p.alpha, a),
    ]
}
fn plane_ok(bytes: &[u8], stride: u64, width: u32, height: u32, bpp: u64) -> bool {
    let Some(row) = u64::from(width).checked_mul(bpp) else {
        return false;
    };
    stride >= row
        && stride.is_multiple_of(4)
        && stride
            .checked_mul(u64::from(height))
            .is_some_and(|size| u64::try_from(bytes.len()).ok() == Some(size))
}

/// Shape, plane hashes and values of a resolved tile against the plan's declaration. A colour
/// sample outside `[0, 1]` is refused unless the plan's output stage is `extended` (any finite
/// colour, `display.rs`).
fn verify_tile(
    input: &TileInput,
    planes: &TilePlanes<'_>,
    plan_space: ColourTag,
    extended: bool,
) -> Result<(), GpuError> {
    let key = input.key;
    // Defence in depth: shared admission refuses a premultiplied convention first, because this
    // backend declares `ExtOperator::AlphaPremultiplied` Unsupported (key is pigment's).
    if input.alpha != AlphaConvention::Straight {
        return Err(unsupported(ExtOperator::AlphaPremultiplied.key()));
    }
    if !planes.matches(input, plan_space)
        || !plane_ok(
            planes.colour,
            planes.colour_stride_bytes,
            planes.width,
            planes.height,
            12,
        )
        || !plane_ok(
            planes.alpha,
            planes.alpha_stride_bytes,
            planes.width,
            planes.height,
            4,
        )
    {
        return Err(mismatch(key));
    }
    if hsk_studio_prism::profile_hash(planes.colour) != planes.colour_sha256
        || hsk_studio_prism::profile_hash(planes.alpha) != planes.alpha_sha256
    {
        return Err(mismatch(key));
    }
    let mut out_of_unit = false;
    for y in 0..planes.height {
        for x in 0..planes.width {
            let [r, g, b, a] = tile_pixel(planes, x, y);
            if ![r, g, b, a].iter().all(|v| v.is_finite()) || !(0.0..=1.0).contains(&a) {
                return Err(mismatch(key));
            }
            out_of_unit |= ![r, g, b].iter().all(|v| sample_ok(extended, *v));
        }
    }
    if out_of_unit {
        return Err(unsupported(SAMPLE_RANGE_OPERATOR));
    }
    Ok(())
}

/// Map a feather failure to the typed refusal (before any device work).
fn feather_error(e: FeatherError, limit: u64) -> GpuError {
    match e {
        FeatherError::Radius => unsupported("mask_feather_radius"),
        FeatherError::Budget { requested } => GpuError::BudgetExceeded {
            requested,
            budget: limit,
        },
        FeatherError::Cost => unsupported("mask_feather_cost"),
        FeatherError::Overflow => overflow(),
    }
}

/// Verify the plan's sources and compile it for the device. `extensions` is the plan-extension
/// envelope of the extended path (`None` = the v1 path, which keeps today's refusal of a mask
/// density below 1); `scratch_limit` is the working-byte limit the host-side feather blurs are
/// charged to.
fn prepare<'a>(
    plan: &RenderPlan<NoVector>,
    sources: &'a dyn PlanSources,
    extensions: Option<&PlanExtensions>,
    scratch_limit: u64,
) -> Result<Prepared<'a>, GpuError> {
    let output = extensions.map(|e| e.output).unwrap_or_default();
    let extended = output.is_extended();
    let target = plan.target;
    if target.alpha != AlphaConvention::Straight {
        return Err(unsupported(ExtOperator::AlphaPremultiplied.key()));
    }
    if target.format != SampleFormat::RgbF32Le {
        return Err(unsupported("sample_format"));
    }
    let n = plan.nodes.len();
    let root = plan.root.0 as usize;
    let mut reach = vec![false; n];
    reach[root] = true;
    for i in (0..n).rev() {
        if reach[i]
            && let PlanNode::Group(g) = &plan.nodes[i]
        {
            for l in &g.layers {
                reach[l.input.0 as usize] = true;
            }
        }
    }
    let mut nodes: Vec<PNode<'a>> = Vec::with_capacity(n);
    let mut bounds: Vec<Option<Rect>> = Vec::with_capacity(n);
    let mut need: Vec<usize> = Vec::with_capacity(n);
    let (mut has_tiles, mut has_masks) = (false, false);
    let (mut has_ext, mut lut_floats) = (false, 0usize);
    let mut feathered: Vec<FeatherPlane> = Vec::new();
    let mut feather_bytes = 0u64;
    let mut has_fx = false;
    let mut fx_kernels = FxKernels::default();
    let mut patterns: std::collections::BTreeMap<SourceKey, Vec<[f32; 4]>> = std::collections::BTreeMap::new();
    let mut pattern_bytes = 0u64;
    let mut fx_window_ops = 1u64;
    // Per node: the margin a chunk must be evaluated with so the effects inside it read real data.
    let mut margins: Vec<u32> = Vec::with_capacity(n);
    let mut extra: Vec<PNode<'a>> = Vec::new();
    let mut extra_bounds: Vec<Option<Rect>> = Vec::new();
    let mut transform_window_texels = 0u64;
    let mut has_blur = false;
    let mut blur_taps = 0u64;
    let mut ko_layers: Vec<(usize, Knockout)> = Vec::new();
    for (i, node) in plan.nodes.iter().enumerate() {
        if !reach[i] {
            nodes.push(PNode::Unused);
            bounds.push(None);
            need.push(0);
            margins.push(0);
            continue;
        }
        let (pnode, node_bounds, node_need, node_margin) = match node {
            PlanNode::Tile(t) => {
                let planes = sources
                    .tile(t.key)
                    .ok_or(GpuError::Backend(BackendError::UnresolvedSource(t.key)))?;
                verify_tile(t, &planes, target.space, extended)?;
                end(t.placement)?;
                has_tiles = true;
                (
                    PNode::Tile {
                        planes,
                        rect: t.placement,
                    },
                    Some(t.placement),
                    0,
                    0,
                )
            }
            PlanNode::Solid { rect, rgba } => {
                if !(rgba[..3].iter().all(|c| sample_ok(extended, *c))
                    && rgba[3].is_finite()
                    && (0.0..=1.0).contains(&rgba[3]))
                {
                    return Err(unsupported(SAMPLE_RANGE_OPERATOR));
                }
                end(*rect)?;
                (PNode::Solid { rgba: *rgba }, Some(*rect), 0, 0)
            }
            PlanNode::Vector(never) => match *never {},
            PlanNode::Group(g) => {
                let mut layers = Vec::with_capacity(g.layers.len());
                let mut group_bounds = None;
                let mut group_need = 0usize;
                let mut group_margin = 0u32;
                for (layer_index, l) in g.layers.iter().enumerate() {
                    let input = l.input.0 as usize;
                    let pass_through = l.mode == StudioBlendMode::PassThrough;
                    let child_isolated = match &plan.nodes[input] {
                        PlanNode::Group(c) => Some(c.isolated),
                        _ => None,
                    };
                    // Structural validation already guaranteed pass_through <-> non-isolated group.
                    // Every refusal key below is pigment's (`Operator::key`, the exported
                    // `PASS_THROUGH_FILL_OPERATOR` / `MASK_DENSITY_OPERATOR`), in the order shared
                    // admission applies them (opacity, then fill), so the defence in depth reports
                    // exactly what `admit` reports (PLR-006, test
                    // `defence_in_depth_refusal_keys_are_pigments`).
                    if pass_through && l.fill_opacity < 1.0 {
                        return Err(unsupported(PASS_THROUGH_FILL_OPERATOR));
                    }
                    if !pass_through {
                        if child_isolated == Some(false) {
                            return Err(unsupported(Operator::NonIsolatedBlendedGroup.key()));
                        }
                        if !gpu_blend_supported(l.mode) {
                            return Err(unsupported(Operator::Blend(l.mode).key()));
                        }
                    }
                    // The envelope entry of this (group, layer), if the extended path has one.
                    let layer_ext = match (extensions, u32::try_from(i), u32::try_from(layer_index))
                    {
                        (Some(e), Ok(group), Ok(layer)) => e.extension(PlanNodeId(group), layer),
                        _ => None,
                    };
                    // Knockout (lane SMB-PS-ADJUST-STYLES-3): pigment admitted the layer and its
                    // placement; the `knockout` module evaluates it.
                    let knockout = layer_ext.map_or(Knockout::None, |x| x.knockout);
                    if knockout != Knockout::None {
                        ko_layers.push((i, knockout));
                    }
                    // Layer effects (pigment effect_math): compiled into matte passes and a
                    // margin. The compile charges the blur kernels to the working-byte limit.
                    let fx = match layer_ext {
                        Some(x) if !x.effects.is_empty() => {
                            let mut effects = Vec::with_capacity(x.effects.len());
                            let mut margin = 0u32;
                            for op in &x.effects {
                                margin = margin
                                    .max(margin_of(op).map_err(|e| fx_error(e, scratch_limit))?);
                                let compiled = compile_effect(
                                    op,
                                    &mut fx_kernels,
                                    scratch_limit.saturating_sub(feather_bytes),
                                    // the unit fade composes the stack at opacity 1 (pigment
                                    // `render_layer_reference`), so a knockout reads the fill
                                    l.fill_opacity,
                                )
                                .map_err(|e| fx_error(e, scratch_limit))?;
                                fx_window_ops = fx_window_ops.max(compiled.window_ops);
                                // a gradient or pattern overlay uploads its colours into the
                                // source region
                                has_tiles |= compiled.gradient.is_some() || compiled.pattern.is_some();
                                // Pattern pixels (lane SMB-PS-ADJUST-STYLES-2): a plan tile source,
                                // verified like a tile input and decoded once, charged to the
                                // working-byte limit before it is allocated.
                                if let Some(o) = &compiled.pattern
                                    && !patterns.contains_key(&o.pattern.key)
                                {
                                    let r = o.pattern;
                                    let bytes = u64::from(r.width)
                                        .checked_mul(u64::from(r.height))
                                        .and_then(|n| n.checked_mul(16))
                                        .ok_or_else(overflow)?;
                                    pattern_bytes = pattern_bytes.checked_add(bytes).ok_or_else(overflow)?;
                                    let used = feather_bytes.checked_add(pattern_bytes).ok_or_else(overflow)?;
                                    if used > scratch_limit {
                                        return Err(GpuError::BudgetExceeded {
                                            requested: used,
                                            budget: scratch_limit,
                                        });
                                    }
                                    let planes = sources
                                        .tile(r.key)
                                        .ok_or(GpuError::Backend(BackendError::UnresolvedSource(r.key)))?;
                                    let input = TileInput {
                                        key: r.key,
                                        placement: Rect { x: 0, y: 0, width: r.width, height: r.height },
                                        format: SampleFormat::RgbF32Le,
                                        alpha: AlphaConvention::Straight,
                                        colour_sha256: r.colour_sha256,
                                        alpha_sha256: r.alpha_sha256,
                                    };
                                    verify_tile(&input, &planes, target.space, false)?;
                                    let mut pixels = Vec::new();
                                    pixels
                                        .try_reserve_exact(
                                            usize::try_from(u64::from(r.width).saturating_mul(u64::from(r.height)))
                                                .map_err(|_| overflow())?,
                                        )
                                        .map_err(|_| unsupported("pattern_allocation"))?;
                                    for y in 0..r.height {
                                        for x in 0..r.width {
                                            pixels.push(tile_pixel(&planes, x, y));
                                        }
                                    }
                                    patterns.insert(r.key, pixels);
                                }
                                effects.push(compiled);
                            }
                            has_fx = true;
                            let interior = effects.iter().any(|e| e.interior);
                            Some(Box::new(FxLayer { effects, margin, interior }))
                        }
                        _ => None,
                    };
                    let clipped = l.mask.as_ref().is_some_and(|m| m.is_clip_to_base());
                    // Layer transform (envelope): the layer reads a transformed-tile node of
                    // `extra`. Pigment validated the tile input, the combinations and the matrix;
                    // kept as defence in depth with pigment's error values.
                    let node_id = u32::try_from(i).map_err(|_| overflow())?;
                    let mut mask_transform = None;
                    let (source_index, content_bounds) =
                        match layer_ext.and_then(|x| x.transform.as_ref()) {
                            None => (input, bounds[input]),
                            Some(t) => {
                                let PNode::Tile { planes, rect } = &nodes[input] else {
                                    return Err(GpuError::Backend(BackendError::InvalidPlan(
                                        PlanError::TransformTarget { node: node_id },
                                    )));
                                };
                                if fx.is_some()
                                    || (t.transform_mask
                                        && layer_ext.is_some_and(|x| x.mask_feather_px > 0.0))
                                {
                                    return Err(GpuError::Backend(BackendError::InvalidPlan(
                                        PlanError::TransformCombination { node: node_id },
                                    )));
                                }
                                let prep = PreparedTransform::new(t, *rect)
                                    .map_err(|e| transform_invalid(node_id, e))?;
                                if t.transform_mask {
                                    mask_transform = Some(*t);
                                }
                                transform_window_texels = transform_window_texels.max(
                                    u64::from(rect.width).saturating_mul(u64::from(rect.height)),
                                );
                                let at = n.checked_add(extra.len()).ok_or_else(overflow)?;
                                extra.push(PNode::Transformed {
                                    planes: *planes,
                                    prep,
                                });
                                extra_bounds.push(Some(prep.bounds()));
                                (at, Some(prep.bounds()))
                            }
                        };
                    let layer_region =
                        content_bounds.and_then(|b| intersect(b, target.extent).ok().flatten());
                    let mask = match &l.mask {
                        None => None,
                        // Clip-to-base carries no plane: the coverage is the base alpha.
                        Some(m) if m.is_clip_to_base() => None,
                        Some(m) => {
                            if m.density != 1.0 && extensions.is_none() {
                                return Err(unsupported(MASK_DENSITY_OPERATOR));
                            }
                            // Defence in depth: pigment defines no mask on a pass-through
                            // layer and shared admission refuses it first (its constant).
                            if pass_through {
                                return Err(unsupported(PASS_THROUGH_MASK_OPERATOR));
                            }
                            let wide = match m.format {
                                MaskFormat::CoverageU8 => false,
                                MaskFormat::CoverageU16Le => true,
                                // A format this backend has no reader for: pigment's operator
                                // key for it (`Operator::Mask(format).key()`).
                                other => return Err(unsupported(Operator::Mask(other).key())),
                            };
                            let plane = sources
                                .mask(m.key)
                                .ok_or(GpuError::Backend(BackendError::UnresolvedSource(m.key)))?;
                            if plane.sample_format != m.format.key()
                                || plane.sha256 != m.sha256
                                || plane.bounds != m.placement
                                || plane.validate(plane.bounds).is_err()
                            {
                                return Err(mismatch(m.key));
                            }
                            // A transformed (linked) mask has zero coverage outside its resampled
                            // plane by definition: no gap to check.
                            if mask_transform.is_none()
                                && let Some(region) = layer_region
                                && !plane.bounds.contains(region).map_err(|_| overflow())?
                            {
                                return Err(mismatch(m.key));
                            }
                            has_masks = true;
                            let mut view = MaskView {
                                bounds: plane.bounds,
                                wide,
                                bytes: plane.bytes,
                                stride: plane.stride_bytes as usize,
                                inverted: m.inverted,
                                density: m.density,
                                feather: None,
                                xform: mask_transform
                                    .map(|t| PreparedTransform::new(&t, plane.bounds))
                                    .transpose()
                                    .map_err(|e| transform_invalid(node_id, e))?,
                            };
                            // Mask feather (envelope): Gaussian blur of the stored coverage over the
                            // layer's region, on the host with pigment's taps, before inversion and
                            // density. Charged to the working-byte limit.
                            if let (Some(x), Some(region)) = (layer_ext, layer_region)
                                && x.mask_feather_px > 0.0
                            {
                                let remaining = scratch_limit.saturating_sub(feather_bytes);
                                let blurred = feather_plane(
                                    x.mask_feather_px,
                                    region,
                                    plane.bounds,
                                    remaining,
                                    |px, py| view.raw(px, py),
                                )
                                .map_err(|e| feather_error(e, scratch_limit))?;
                                feather_bytes = feather_bytes
                                    .checked_add(blurred.bytes())
                                    .ok_or_else(overflow)?;
                                view.feather = Some(feathered.len());
                                feathered.push(blurred);
                            }
                            Some(view)
                        }
                    };
                    // Blend-If and adjustment of the envelope.
                    let blend_if = match layer_ext {
                        Some(x) => BlendIfGpu::from_entries(&x.blend_if).map_err(unsupported)?,
                        None => None,
                    };
                    let adjust = match layer_ext.and_then(|x| x.adjustment.as_ref()) {
                        Some(op) => {
                            let kind = op.kind();
                            let opcode = opcode_of(kind)
                                .filter(|_| gpu_adjust_supported(kind))
                                .ok_or_else(|| unsupported(kind.key()))?;
                            let mut compiled =
                                compile_adjustment(op, opcode).map_err(unsupported)?;
                            // Levels on whole levels in an 8-bit document (lane
                            // SMB-PS-ORACLE-PARITY-2, pigment `LevelsRecord::map_value_8bit`): the y
                            // of each record's second vec4.
                            if output.is_eight_bit()
                                && matches!(op, hsk_studio_pigment::adjust_math::AdjustmentOp::Levels(_))
                            {
                                for hi in compiled.params.iter_mut().skip(1).step_by(2) {
                                    hi[1] = 1.0;
                                }
                            }
                            lut_floats = lut_floats.max(compiled.lut.as_ref().map_or(0, Vec::len));
                            Some(Box::new(compiled))
                        }
                        None => None,
                    };
                    // Blend-If with effects: the weight depends on the backdrop of the content
                    // composite and pigment does not define the combination, so shared admission
                    // refuses it for every backend (`ExtOperator::EffectsWithBlendIf`, intrinsically
                    // Unsupported) before this point. Kept as defence in depth under pigment's key.
                    if blend_if.is_some() && fx.is_some() {
                        return Err(unsupported(ExtOperator::EffectsWithBlendIf.key()));
                    }
                    // Blend-If on the base of a clipping run or on a clipped layer: no public source
                    // defines it, so pigment refuses it for every backend in `admit_extended`
                    // (`BlendIfClipBase`, `BlendIfClippedLayer`) before this point. Kept as defence
                    // in depth under the same keys (the roles and keys are pigment's, unit test
                    // `clip_role_blend_if_refusal_uses_pigments_roles_and_keys`; the public refusal
                    // is tested by fx_admission_and_refusals_run_without_adapter and
                    // ext_blend_if_parity).
                    if blend_if.is_some()
                        && let Some(key) = blend_if_clip_refusal(g, layer_index)
                    {
                        return Err(unsupported(key));
                    }
                    has_ext |= blend_if.is_some() || adjust.is_some();
                    // Layer / backdrop blur (pigment `blur_math`). Pigment validated the
                    // combinations; a backdrop blur outside an isolated group is refused again here
                    // with pigment's error value (defence in depth).
                    let blur = match layer_ext.and_then(|x| x.blur).filter(|b| b.radius_px > 0.0) {
                        Some(b) => {
                            if b.scope == BlurScope::Backdrop && !g.isolated {
                                return Err(GpuError::Backend(BackendError::InvalidPlan(
                                    PlanError::BlurCombination { node: node_id },
                                )));
                            }
                            let taps = b.taps().map_err(|_| unsupported("blur_parameter"))?;
                            if taps.len() > BLUR_MAX_TAPS {
                                return Err(unsupported("blur_parameter"));
                            }
                            let support = b.support_px().map_err(|_| unsupported("blur_parameter"))?;
                            has_blur = true;
                            blur_taps = blur_taps
                                .max(u64::try_from(taps.len()).map_err(|_| overflow())?);
                            Some(Box::new(BlurGpu {
                                scope: b.scope,
                                taps,
                                support,
                            }))
                        }
                        None => None,
                    };
                    // Layer filters (pigment `filter_math`). Pigment validated the combinations
                    // (`FilterCombination`); kept as defence in depth with pigment's error value.
                    let filter = match layer_ext {
                        Some(x) if !x.filters.is_empty() => {
                            if fx.is_some()
                                || blur.is_some()
                                || adjust.is_some()
                                || x.transform.is_some()
                                || pass_through
                                || clipped
                            {
                                return Err(GpuError::Backend(BackendError::InvalidPlan(
                                    PlanError::FilterCombination { node: node_id },
                                )));
                            }
                            let (compiled, pass_ops) = compile_filters(&x.filters, node_id)?;
                            has_blur = true;
                            blur_taps = blur_taps.max(pass_ops);
                            Some(Box::new(compiled))
                        }
                        _ => None,
                    };
                    // Smart-filter stack (pigment `smart_filter`): filter-only stacks without masks
                    // run as filter passes plus a mix pass per entry; pigment validated the
                    // combinations (`SmartFilterCombination`), kept as defence in depth.
                    let smart = match layer_ext.and_then(|x| x.smart_filters.as_ref()) {
                        Some(op) => {
                            if fx.is_some()
                                || blur.is_some()
                                || adjust.is_some()
                                || filter.is_some()
                                || layer_ext.is_some_and(|x| x.transform.is_some())
                                || pass_through
                                || clipped
                            {
                                return Err(GpuError::Backend(BackendError::InvalidPlan(
                                    PlanError::SmartFilterCombination { node: node_id },
                                )));
                            }
                            let plane = content_bounds.map(|b| intersect(b, target.extent)).transpose()?.flatten();
                            // Mask planes (lane SMB-PS-ADJUST-STYLES-3) are charged with the
                            // feather scratch against the working-byte limit.
                            let mut room = scratch_limit.saturating_sub(feather_bytes);
                            let before = room;
                            let (compiled, pass_ops) = compile_smart(op, plane, node_id, sources, &mut room)?;
                            feather_bytes = feather_bytes.checked_add(before.saturating_sub(room)).ok_or_else(overflow)?;
                            has_masks |= !compiled.masks.is_empty();
                            has_blur = true;
                            blur_taps = blur_taps.max(pass_ops);
                            Some(Box::new(compiled))
                        }
                        None => None,
                    };
                    // The layer's bounds grow by its effect margin (and a layer blur's support);
                    // the margin of the node is the largest chain sum of effect margins through
                    // nested groups. A backdrop blur reads the backdrop of the layers below up to
                    // its support away, so it needs their margin plus its support.
                    let layer_blur = blur
                        .as_ref()
                        .filter(|b| b.scope == BlurScope::Layer)
                        .map_or(0, |b| b.support);
                    let layer_margin = fx
                        .as_ref()
                        .map_or(0, |f| f.margin)
                        .max(layer_blur)
                        .max(filter.as_ref().map_or(0, |f| f.reach))
                        .max(smart.as_ref().map_or(0, |s| s.reach));
                    let layer_bounds = match content_bounds {
                        Some(b) if layer_margin > 0 => Some(grow_rect(b, layer_margin)?),
                        b => b,
                    };
                    let child_margin = match &plan.nodes[input] {
                        PlanNode::Group(_) => margins[input],
                        _ => 0,
                    };
                    if let Some(b) = blur.as_ref().filter(|b| b.scope == BlurScope::Backdrop) {
                        group_margin = group_margin
                            .checked_add(b.support)
                            .ok_or_else(overflow)?;
                    }
                    group_margin = group_margin.max(
                        layer_margin
                            .checked_add(child_margin)
                            .ok_or_else(overflow)?,
                    );
                    group_bounds = union(group_bounds, layer_bounds)?;
                    layers.push(PLayer {
                        input: source_index,
                        clipped,
                        mode: l.mode,
                        opacity: l.opacity * l.fill_opacity,
                        layer_opacity: l.opacity,
                        fill: l.fill_opacity,
                        mask,
                        blend_if,
                        adjust,
                        fx,
                        blur,
                        filter,
                        smart,
                        knockout,
                        group: i,
                    });
                }
                // Destination levels below this group's own: a plain layer needs its isolated child's
                // buffer (1 + the child's need); a clipping run needs the run buffer G (1) plus,
                // below G, each member's isolated child.
                // A layer with effects also holds its content frame one level below its own.
                let child_depth = |k: usize| {
                    // A transformed-tile node (index past the plan) is a leaf.
                    let input = layers[k].input;
                    let below = match plan.nodes.get(input) {
                        Some(PlanNode::Group(c)) if c.isolated => {
                            need.get(input).copied().unwrap_or(0).saturating_add(1)
                        }
                        // a pass-through layer with opacity runs one level down and is mixed back
                        // (pigment `pass_through_mix`, lane SMB-PS-ORACLE-PARITY-2)
                        Some(PlanNode::Group(_)) if layers[k].layer_opacity < 1.0 =>
                        {
                            need.get(input).copied().unwrap_or(0).saturating_add(1)
                        }
                        Some(PlanNode::Group(_)) => need.get(input).copied().unwrap_or(0),
                        _ => 0,
                    };
                    if layers[k].smart.is_some() {
                        // The running result and one filtered copy.
                        below.saturating_add(2)
                    } else if layers[k].fx.as_ref().is_some_and(|f| f.interior) {
                        // The interior unit (lane SMB-PS-ORACLE-PARITY-1) holds the unit one level
                        // below and the content copy two levels below; a layer opacity below 1
                        // adds the faded unit's level (lane SMB-PS-ORACLE-PARITY-2).
                        below
                            .max(1)
                            .saturating_add(1)
                            .saturating_add(usize::from(layers[k].layer_opacity < 1.0))
                    } else if layers[k].fx.is_some() {
                        below
                            .saturating_add(1)
                            .saturating_add(usize::from(layers[k].layer_opacity < 1.0))
                    } else if layers[k].blur.is_some()
                        || layers[k].filter.is_some()
                    {
                        below.saturating_add(1)
                    } else {
                        below
                    }
                };
                // The node index of this group, read before the run walk below shadows `i`.
                let group_node = u32::try_from(i).map_err(|_| overflow())?;
                let mut i = 0;
                while i < layers.len() {
                    let mut j = i.saturating_add(1);
                    while j < layers.len() && layers[j].clipped {
                        j = j.saturating_add(1);
                    }
                    if j == i.saturating_add(1) {
                        group_need = group_need.max(child_depth(i));
                    } else {
                        // Pigment validation refuses a pass-through clip base first
                        // (`ClipBasePassThrough`); defence in depth with pigment's own error value
                        // for the group node, never a local spelling (PLR-006).
                        if layers[i].mode == StudioBlendMode::PassThrough {
                            return Err(GpuError::Backend(BackendError::InvalidPlan(
                                PlanError::ClipBasePassThrough { node: group_node },
                            )));
                        }
                        group_need = group_need.max(
                            (i..j)
                                .map(|k| child_depth(k).saturating_add(1))
                                .max()
                                .unwrap_or(1),
                        );
                    }
                    i = j;
                }
                (
                    PNode::Group {
                        layers,
                        isolated: g.isolated,
                    },
                    group_bounds,
                    group_need,
                    group_margin,
                )
            }
        };
        nodes.push(pnode);
        bounds.push(node_bounds);
        need.push(node_need);
        margins.push(node_margin);
    }
    // Knockout contexts (lane SMB-PS-ADJUST-STYLES-3): the isolation and the group-layer parent of
    // every reachable group node.
    let ko = if ko_layers.is_empty() {
        knockout::KoPlan::default()
    } else {
        let mut isolated = std::collections::BTreeMap::new();
        let mut parent = std::collections::BTreeMap::new();
        for (gi, node) in plan.nodes.iter().enumerate() {
            if let (true, PlanNode::Group(g)) = (reach[gi], node) {
                isolated.insert(gi, g.isolated);
                for l in &g.layers {
                    parent.insert(l.input.0 as usize, gi);
                }
            }
        }
        knockout::KoPlan::new(&ko_layers, &isolated, &parent, root)
    };
    Ok(Prepared {
        nodes,
        bounds,
        extra,
        extra_bounds,
        transform_window_texels,
        need,
        root,
        reachable: reach.iter().filter(|r| **r).count() as u64,
        has_tiles,
        has_masks,
        feathered,
        has_ext,
        lut_floats,
        has_fx,
        fx_kernels,
        patterns,
        fx_window_ops,
        frame_margin: margins[root],
        has_blur,
        blur_taps,
        output,
        ko,
    })
}

// ---- backend ------------------------------------------------------------------------------------

enum State {
    Ready(Box<GpuContext>),
    Lost {
        generation: u64,
        adapter: String,
    },
    Unavailable(Reason),
    /// Adapter and device were acquired but the backend cannot run: the shader or pipeline failed
    /// validation (`GpuError::Pipeline`). A defect, not "no GPU": reported as that error.
    Failed(GpuError),
}
struct Inner {
    state: State,
    residency: Residency,
    /// Generation of the most recent device (0 = none yet); the next acquire is `last + 1`.
    last_generation: u64,
    stats: Option<GpuStats>,
    /// Detail of the most recent pipeline creation failure (acquire, reacquire or the test-faults
    /// probe); cleared by a successful (re)acquire.
    pipeline_fault: Option<PipelineFault>,
    /// Detail of the most recent execution that ended in a typed submit/read-back fault or a
    /// caught panic; cleared by the next execution (lane SMB-RENDER-GPU-SUBMIT-FAULT).
    submit_fault: Option<PipelineFault>,
}

/// GPU backend over the pigment `RenderBackend` port. Device use is serialised by one mutex.
pub struct GpuBackend {
    options: DeviceOptions,
    /// WGSL override for fault injection (`None` = the generated shader).
    shader: Option<String>,
    /// Deprecated alias (`with_display_transform`): the output stage a plan without one runs with.
    display: Option<DisplayTransform>,
    inner: Mutex<Inner>,
}

impl GpuBackend {
    /// Try to acquire a headless device now. Returns a backend, not a `Result`: when there is no
    /// device the backend exists in the `Unavailable` state and every `execute` reports it (after admission) as a typed error.
    pub fn new(options: DeviceOptions) -> Self {
        Self::acquire_into(options, None)
    }

    /// Fault-injection constructor (V4-04: compiled only with the `test-faults` feature): acquire
    /// with `wgsl` instead of the generated shader (also used by `reacquire`). A shader that fails
    /// validation leaves the backend in the typed pipeline failure state
    /// ([`GpuBackend::failure`]), not `Unavailable` (test
    /// pipeline_failure_is_typed_and_fails_the_gpu_gate).
    #[cfg(feature = "test-faults")]
    pub fn with_shader_source(options: DeviceOptions, wgsl: &str) -> Self {
        Self::acquire_into(options, Some(wgsl.to_owned()))
    }

    fn acquire_into(options: DeviceOptions, shader: Option<String>) -> Self {
        let acquired = GpuContext::acquire_with(
            &options,
            &CancellationToken::default(),
            0,
            shader.as_deref(),
        );
        let mut fault = None;
        let (state, last_generation) = match acquired {
            Ok(ctx) => {
                let generation = ctx.generation();
                (State::Ready(Box::new(ctx)), generation)
            }
            Err(failure) => match failure.error {
                GpuError::Unavailable(reason) => (State::Unavailable(reason), 0),
                // A device existed but the backend cannot run (shader/pipeline creation): kept as
                // its own typed failure, distinct from `Unavailable`, with its detail.
                error => {
                    fault = failure.fault.map(|f| *f);
                    (State::Failed(error), 0)
                }
            },
        };
        let mut backend = Self::with_state(options, state, last_generation);
        backend.shader = shader;
        backend.lock().pipeline_fault = fault;
        backend
    }

    /// Fault-injection constructor (V4-04: compiled only with the `test-faults` feature): a
    /// backend whose device of `generation` was already lost (the state the real device-lost
    /// callback latches). Lets tests exercise `DeviceLost` and `reacquire` without hardware.
    #[cfg(feature = "test-faults")]
    pub fn with_injected_loss(options: DeviceOptions, generation: u64) -> Self {
        let state = State::Lost {
            generation,
            adapter: String::from("injected"),
        };
        Self::with_state(options, state, generation)
    }

    fn with_state(options: DeviceOptions, state: State, last_generation: u64) -> Self {
        Self {
            options,
            shader: None,
            display: None,
            inner: Mutex::new(Inner {
                state,
                residency: Residency::new(options.residency_budget_bytes),
                last_generation,
                stats: None,
                pipeline_fault: None,
                submit_fault: None,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
    /// Lock and run the backend's own loss detection: poll a live device so wgpu delivers a pending
    /// device-lost callback, and move a latched device to `Lost`.
    fn lock_refreshed(&self) -> MutexGuard<'_, Inner> {
        let mut inner = self.lock();
        if let State::Ready(ctx) = &inner.state
            && ctx.poll_lost()
        {
            inner.state = State::Lost {
                generation: ctx.generation(),
                adapter: ctx.info().text(),
            };
        }
        inner
    }

    pub fn options(&self) -> DeviceOptions {
        self.options
    }
    /// Deprecated alias (lane SMB-RENDER-SCENE-DISPLAY; the output stage now lives in the plan,
    /// pigment `PlanExtensions::with_output`): a plan whose envelope has the default output stage
    /// runs as if it declared `OutputStage { Extended, Some(transform) }`; a plan that declares its
    /// own stage is unchanged (test `tests/gpu_scene_display.rs`).
    #[deprecated(note = "declare the output stage in the plan: PlanExtensions::with_output")]
    pub fn with_display_transform(mut self, transform: DisplayTransform) -> Self {
        self.display = Some(transform);
        self
    }
    /// The deprecated alias's display transform (`None` = plans run with their own stage).
    pub fn display_transform(&self) -> Option<DisplayTransform> {
        self.display
    }
    /// Description of the live device, `None` when unavailable or lost.
    pub fn device_info(&self) -> Option<DeviceInfo> {
        match &self.lock_refreshed().state {
            State::Ready(ctx) => Some(ctx.info().clone()),
            _ => None,
        }
    }
    /// Why no device is available (`None` while a device is live, after a loss, or after a
    /// pipeline failure).
    pub fn unavailable_reason(&self) -> Option<Reason> {
        match &self.lock().state {
            State::Unavailable(reason) => Some(*reason),
            _ => None,
        }
    }
    /// The typed failure of a backend whose adapter and device exist but whose shader or pipeline
    /// failed (`GpuError::Pipeline`); `None` in every other state. Distinct from `Unavailable` (test
    /// pipeline_failure_is_typed_and_fails_the_gpu_gate).
    pub fn failure(&self) -> Option<GpuError> {
        match &self.lock().state {
            State::Failed(error) => Some(*error),
            _ => None,
        }
    }
    /// Detail of the most recent shader-module or pipeline creation failure: the code also carried
    /// by `GpuError::Pipeline`, the failing object's label, the wgpu error class and the backend
    /// message (lane SMB-RENDER-GPU-PIPELINE-FAULT, tests/gpu_pipeline_fault.rs). `None` when the
    /// last (re)acquire built every pipeline.
    pub fn pipeline_fault(&self) -> Option<PipelineFault> {
        self.lock().pipeline_fault.clone()
    }
    /// Fault-injection probe (V4-04: compiled only with the `test-faults` feature): build one
    /// extra compute pipeline from `wgsl` (automatic layout) on the LIVE device, under the same
    /// guarded creation as the backend's own pipelines. A failure is the typed
    /// `GpuError::Pipeline` with its detail in [`GpuBackend::pipeline_fault`]; the backend state is
    /// unchanged, so the device keeps executing plans. An error that still reached the
    /// uncaptured-error latch is `GpuError::Gpu("uncaptured_error")` (test
    /// probe_failure_leaves_the_device_usable).
    #[cfg(feature = "test-faults")]
    pub fn probe_pipeline(&self, wgsl: &str, entry_point: &str) -> Result<(), GpuError> {
        let mut inner = self.lock_refreshed();
        let probed = match &inner.state {
            State::Ready(ctx) => {
                let probed = crate::pipeline::probe(&ctx.device, wgsl, entry_point);
                if ctx.take_fault() {
                    return Err(GpuError::Gpu("uncaptured_error"));
                }
                probed
            }
            State::Lost { generation, .. } => {
                return Err(GpuError::DeviceLost { generation: *generation });
            }
            State::Unavailable(reason) => return Err(GpuError::Unavailable(*reason)),
            State::Failed(error) => return Err(*error),
        };
        probed.map_err(|fault| {
            let error = fault.error();
            inner.pipeline_fault = Some(fault);
            error
        })
    }
    /// Detail of the most recent execution's typed fault: an error captured while a chunk's
    /// commands were encoded, submitted or read back (`submit_fault`, `readback_map_failed`,
    /// `submit_scope_timeout`, carried by `GpuError::Gpu(code)`) or a caught panic
    /// (`submit_panic`, the device is then latched lost), with the operation label, the label
    /// wgpu reports for the failing object, the class and the redacted backend message (lane
    /// SMB-RENDER-GPU-SUBMIT-FAULT, tests/gpu_submit_fault.rs). `None` after an execution without
    /// such a fault.
    pub fn submit_fault(&self) -> Option<PipelineFault> {
        self.lock().submit_fault.clone()
    }
    /// Fault-injection hook (V4-04: compiled only with the `test-faults` feature): arm one real
    /// execution fault on the live device for the next execution (`false` when there is no live
    /// device).
    #[cfg(feature = "test-faults")]
    pub fn inject_submit_fault(&self, fault: crate::pipeline::InjectedFault) -> bool {
        match &self.lock_refreshed().state {
            State::Ready(ctx) => {
                ctx.inject(fault);
                true
            }
            _ => false,
        }
    }
    /// Generation of the most recent device; 0 before any device existed.
    pub fn generation(&self) -> u64 {
        self.lock().last_generation
    }
    pub fn is_lost(&self) -> bool {
        matches!(self.lock_refreshed().state, State::Lost { .. })
    }
    /// Counters of the most recent successful execution.
    pub fn last_stats(&self) -> Option<GpuStats> {
        self.lock().stats
    }
    /// Bytes currently charged to residency (0 whenever no execution runs).
    pub fn resident_bytes(&self) -> u64 {
        self.lock().residency.used()
    }
    /// Destroy the live device through `wgpu::Device::destroy`. The loss is then detected by the
    /// backend itself (wgpu's device-lost callback, delivered by the poll before every execute and
    /// state query), so the next `execute` returns `DeviceLost{generation}`; nothing here sets the
    /// latch. Returns `false` when there was no live device. Compiled only with the `test-faults`
    /// feature (V4-04).
    #[cfg(feature = "test-faults")]
    pub fn simulate_device_loss(&self) -> bool {
        match &self.lock_refreshed().state {
            State::Ready(ctx) => {
                ctx.simulate_device_loss();
                true
            }
            _ => false,
        }
    }
    /// Explicit recovery: acquire a successor device (generation + 1) and drop all residency.
    /// `execute` does not call it (test device_destroy_is_detected_by_the_lost_callback: the
    /// next execute after a destroy is `DeviceLost`). On failure the previous state is kept (a lost device
    /// stays lost, so stale generations are still refused).
    pub fn reacquire(&self, cancel: &CancellationToken) -> Result<u64, GpuError> {
        let mut inner = self.lock();
        let ctx = match GpuContext::acquire_with(
            &self.options,
            cancel,
            inner.last_generation,
            self.shader.as_deref(),
        ) {
            Ok(ctx) => ctx,
            Err(failure) => {
                if failure.fault.is_some() {
                    inner.pipeline_fault = failure.fault.map(|f| *f);
                }
                return Err(failure.error);
            }
        };
        let generation = ctx.generation();
        inner.pipeline_fault = None;
        inner.state = State::Ready(Box::new(ctx));
        inner.last_generation = generation;
        inner.residency.clear();
        Ok(generation)
    }

    /// Execute with the detailed internal error and the device-side counters. Order: admission
    /// (cancel, stale revision, plan validation, per-operator class), source verification, device
    /// state, then chunks. A bad plan or source therefore does not reach the device (the none-backend tests
    /// admission_refusals_run_without_adapter and ext_refusals_and_capabilities_run_without_adapter
    /// assert no sink call and no residency), and a receipt
    /// exists only after the last chunk was accepted by the sink.
    pub fn execute_detailed(
        &self,
        plan: &RenderPlan<NoVector>,
        request: &ExecuteRequest,
        sources: &dyn PlanSources,
        cancel: &CancellationToken,
        sink: &mut dyn TileSink,
    ) -> Result<(RenderReceipt, GpuStats), GpuError> {
        self.execute_inner(plan, None, request, sources, cancel, sink, None)
    }

    /// [`GpuBackend::execute_detailed`] for a plan with its extension envelope (pigment
    /// `PlanExtensions`): shared envelope admission first (`admit_extended`: stale revision,
    /// validation, per-operator class, knockout and unsupported adjustments refused as typed
    /// `Unsupported`), then the same order as the v1 path. An empty envelope is the v1 plan except
    /// that a mask density below 1 is admitted (as `ExtOperator::MaskDensity`).
    pub fn execute_extended_detailed(
        &self,
        plan: &RenderPlan<NoVector>,
        extensions: &PlanExtensions,
        request: &ExecuteRequest,
        sources: &dyn PlanSources,
        cancel: &CancellationToken,
        sink: &mut dyn TileSink,
    ) -> Result<(RenderReceipt, GpuStats), GpuError> {
        self.execute_inner(plan, Some(extensions), request, sources, cancel, sink, None)
    }

    /// `texture`: the shared-device output (lane SMB-RENDER-GPU-SHARED-DEVICE); `None` = read back
    /// into `sink`.
    #[allow(clippy::too_many_arguments)]
    fn execute_inner(
        &self,
        plan: &RenderPlan<NoVector>,
        extensions: Option<&PlanExtensions>,
        request: &ExecuteRequest,
        sources: &dyn PlanSources,
        cancel: &CancellationToken,
        sink: &mut dyn TileSink,
        texture: Option<&TextureOut<'_>>,
    ) -> Result<(RenderReceipt, GpuStats), GpuError> {
        let alias = display::aliased(extensions, self.display);
        let extensions = alias.as_ref().or(extensions);
        // Stage-6a view transform (lanes SMB-PRISM-VIEW-TRANSFORM, -2): on the read-back path the
        // host applies it after the read-back; the shared-texture output runs Prism's WGSL form,
        // which exists for the SDR views only, so an HDR view is refused typed there, before
        // admission and before any device work.
        if texture.is_some()
            && extensions
                .and_then(|e| e.output.display)
                .and_then(|d| d.view())
                .is_some_and(|v| hsk_studio_prism::view_gpu_params(v).is_none())
        {
            return Err(unsupported(ExtOperator::DisplayView.key()));
        }
        let mut ledger = match extensions {
            None => admit::<NoVector>(plan, self, request, cancel)?,
            Some(e) => admit_extended(plan, e, self, request, cancel)?,
        };
        leaf_root_class(plan, self, &mut ledger);
        // Key pipelines (lane SMB-AE-KEY-TRACK-3): the host refusals run here, before the device
        // state is read; the keys run on the device below and the plan is prepared rewritten.
        let keyed_plan = key::has_keys(extensions);
        if let (true, Some(e)) = (keyed_plan, extensions) {
            key::check(plan, e)?;
        }
        let prepared_plain = if keyed_plan {
            None
        } else {
            Some(prepare(plan, sources, extensions, request.max_working_bytes)?)
        };
        let digest = match extensions {
            None => plan.digest(),
            Some(e) => e.digest_with(plan),
        }
        .map_err(|_| GpuError::Gpu("plan_digest"))?;

        let mut guard = self.lock_refreshed();
        let inner = &mut *guard;
        let ctx = match &inner.state {
            State::Ready(ctx) => ctx,
            State::Lost { generation, .. } => {
                return Err(GpuError::DeviceLost {
                    generation: *generation,
                });
            }
            State::Unavailable(reason) => return Err(GpuError::Unavailable(*reason)),
            State::Failed(error) => return Err(*error),
        };
        let keyed = match (keyed_plan, extensions) {
            (true, Some(e)) => Some(key::run(ctx, plan, e, sources, request, cancel)?),
            _ => None,
        };
        let keyed_sources = keyed.as_ref().map(|k| k.sources(sources));
        let prepared_keyed = match (&keyed, &keyed_sources) {
            (Some(k), Some(s)) => Some(prepare(&k.plan, s, Some(&k.ext), request.max_working_bytes)?),
            _ => None,
        };
        let (run_plan, prepared) = match (&keyed, &prepared_keyed, &prepared_plain) {
            (Some(k), Some(p), _) => (&k.plan, p),
            (_, _, Some(p)) => (plan, p),
            _ => return Err(unsupported("plan_shape")),
        };

        let residency = &mut inner.residency;
        let mark = residency.mark();
        residency.reset_peak();
        let evictions_before = residency.evictions();
        let mut stats = GpuStats::default();
        ctx.clear_fault_detail();
        // Panic guard: a panic inside the execution (wgpu's fatal paths such as
        // `Buffer::get_mapped_range` or `Device::poll`, or a defect here) is the typed
        // `submit_panic`; the chunk's error scopes are popped while unwinding. The device state
        // after such a panic is unknown, so the backend latches it as lost (reacquire recovers).
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run(
                ctx,
                residency,
                &self.options,
                prepared,
                run_plan,
                request,
                cancel,
                sink,
                &mut stats,
                texture,
            )
        }));
        let result = match result {
            Ok(result) => {
                // detail of a typed execution fault (None after a clean execution)
                let detail = ctx.fault_detail();
                (result, detail)
            }
            Err(payload) => {
                let fault = PipelineFault::new(
                    "submit_panic",
                    EXECUTE_LABEL,
                    "panic",
                    &crate::pipeline::panic_text(payload.as_ref()),
                );
                let error = fault.error();
                let generation = ctx.generation();
                let adapter = ctx.info().text();
                residency.release_since(mark);
                inner.state = State::Lost { generation, adapter };
                inner.submit_fault = Some(fault);
                return Err(error);
            }
        };
        let (result, detail) = result;
        inner.submit_fault = detail;
        stats.peak_resident_bytes = residency.peak_used();
        stats.evictions = residency.evictions().saturating_sub(evictions_before);
        residency.release_since(mark);
        let chunks = match result {
            Ok(chunks) => chunks,
            // A lost device is the root cause of any follow-up GPU failure.
            Err(GpuError::Gpu(_)) if ctx.is_lost() => {
                return Err(GpuError::DeviceLost {
                    generation: ctx.generation(),
                });
            }
            Err(error) => return Err(error),
        };
        let receipt = RenderReceipt {
            plan_revision: plan.revision,
            plan_digest: digest,
            backend: identity_of(&inner.state),
            region: plan.target.extent,
            target: plan.target,
            exactness: ledger,
            chunks,
            nodes_executed: prepared.reachable,
            peak_working_bytes: stats.peak_resident_bytes,
        };
        inner.stats = Some(stats);
        Ok((receipt, stats))
    }
}

/// A leaf root (`Solid`/`Tile`) has no layer, so the plan lists no blend operator, but `run`
/// composites it through the blend kernel (Normal, opacity 1, over transparent). That arithmetic
/// carries the GPU blend class (`Fitted`). Shared admission already checks that class against the
/// request floor (`RenderPlan::has_leaf_root`, pigment `admit_plan_rules`), so this only RECORDS it
/// in the receipt ledger; `Exact` is not claimed for a kernel pass (test
/// leaf_root_reports_the_blend_kernel_class).
fn leaf_root_class(
    plan: &RenderPlan<NoVector>,
    backend: &GpuBackend,
    ledger: &mut ExactnessLedger,
) {
    if !plan.has_leaf_root() {
        return;
    }
    let op = Operator::Blend(StudioBlendMode::Normal);
    ledger.record(op.key(), op.intrinsic().lowest_of(backend.exactness(op)));
}

fn identity_of(state: &State) -> BackendIdentity {
    let (adapter, generation) = match state {
        State::Ready(ctx) => (ctx.info().text(), ctx.generation()),
        State::Lost {
            generation,
            adapter,
        } => (adapter.clone(), *generation),
        State::Unavailable(_) | State::Failed(_) => (String::new(), 0),
    };
    BackendIdentity {
        name: RENDERER_NAME,
        version: RENDERER_VERSION,
        device: DeviceClass::Gpu,
        adapter,
        generation,
    }
}

impl RenderBackend<NoVector> for GpuBackend {
    /// Clip colour grade kernel (lane SMB-PR-SOURCE-MONITOR, module `grade`): always `Some`; no
    /// device or a lost one is the typed error, never a host fallback; a panic inside the device
    /// work is the typed `clip_grade_panic`.
    fn execute_grade(
        &self,
        program: &hsk_studio_prism::grade::GradeProgram,
        surface: &mut hsk_studio_pigment::render_plan::GradeSurface<'_>,
        cancel: &CancellationToken,
    ) -> Option<Result<(), BackendError>> {
        let mut guard = self.lock_refreshed();
        let (result, detail) = match &guard.state {
            State::Ready(ctx) => {
                ctx.clear_fault_detail();
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| grade::run(ctx, program, surface, cancel)))
                    .unwrap_or(Err(GpuError::Gpu("clip_grade_panic")));
                (r, ctx.fault_detail())
            }
            State::Lost { generation, .. } => (Err(GpuError::DeviceLost { generation: *generation }), None),
            State::Unavailable(reason) => (Err(GpuError::Unavailable(*reason)), None),
            State::Failed(error) => (Err(*error), None),
        };
        if result.is_err() {
            guard.submit_fault = detail;
        }
        Some(result.map_err(BackendError::from))
    }
    fn identity(&self) -> BackendIdentity {
        identity_of(&self.lock().state)
    }
    fn exactness(&self, operator: Operator) -> Exactness {
        match operator {
            // Pure data movement and structure: f32 bits survive upload and read-back unchanged,
            // group nesting and mask coverage conversion add no arithmetic of their own (the
            // blend operators carry the arithmetic class).
            Operator::Tile
            | Operator::Solid
            | Operator::IsolatedGroup
            | Operator::PassThroughGroup
            | Operator::Mask(MaskFormat::CoverageU8 | MaskFormat::CoverageU16Le) => {
                Exactness::Exact
            }
            // Same published formula as pigment; f32 GPU arithmetic is tolerance-equal, not
            // bit-exact (fused multiply-add, division rounding).
            Operator::Blend(mode) if gpu_blend_supported(mode) => Exactness::Fitted,
            Operator::FillOpacity => Exactness::Approximate,
            // Lane SMB-PS-ORACLE-PARITY-2: the pass-through mix (pigment `pass_through_mix`).
            Operator::PassThroughOpacity => Exactness::Approximate,
            // Pigment's documented clipping rule (unverified against Photoshop) as written; the
            // operator's intrinsic class is Approximate, which is what the receipt carries.
            Operator::Mask(MaskFormat::BaseAlpha) => Exactness::Approximate,
            _ => Exactness::Unsupported,
        }
    }
    fn supports_clip_to_base(&self) -> bool {
        true
    }
    /// Envelope operators this backend implements. A supported operator reports the lower class of
    /// pigment's declared class (`ExtOperator::intrinsic`) and `Fitted`: f32 GPU arithmetic is
    /// tolerance-equal to pigment's reference, not bit-exact (curves are sampled from a 4096-entry
    /// table, feather is a host blur with pigment's taps, effect blurs accumulate in binary32), and
    /// the reported class never exceeds pigment's (CX-CAP-001; test
    /// ext_refusals_and_capabilities_run_without_adapter). The adjustment kinds are those with a
    /// shader body (`gpu_adjust_supported`), the effect kinds are the six of pigment's waves 1 and
    /// 2; knockout runs every placement pigment admits (lane SMB-PS-ADJUST-STYLES-3), and Blend-If
    /// on a clip base or a clipped layer
    /// (`BlendIfClipBase`, `BlendIfClippedLayer`) is Unsupported because no definition exists (test
    /// ext_refusals_and_capabilities_run_without_adapter).
    ///
    /// Declared by lane SMB-FX-OPS-ADOPT: `EffectsNested` is supported (a layer with effects whose
    /// content holds a layer with effects is evaluated as a composition of per-layer effect stacks,
    /// test fx_stack_mask_group_and_nesting_parity; class Approximate like the effects);
    /// `AlphaPremultiplied` is Unsupported (this backend works on straight alpha and does not convert
    /// at the boundary); `EffectsWithBlendIf` is intrinsically Unsupported in pigment, so shared
    /// admission refuses it whatever is declared here.
    fn extension_exactness(&self, operator: ExtOperator) -> Exactness {
        let supported = match operator {
            // Output stage (lane SMB-PIGMENT-EXTENDED-RANGE): `EXTENDED_FLAG` dispatches and the
            // host-side clip (tests/gpu_extended_range.rs).
            ExtOperator::ExtendedRange | ExtOperator::DisplayClip => true,
            // Stage-6a view transform (lane SMB-PRISM-VIEW-TRANSFORM): Prism's view applied on
            // the host to the read-back (tests/gpu_view_transform.rs); the shared-texture output
            // runs Prism's WGSL form for the SDR views and refuses the HDR views (execute_inner).
            ExtOperator::DisplayView => true,
            // Envelope operations in the extended range (lane SMB-RENDER-GPU-EXTENDED-ENVELOPE):
            // every one pigment defines except content-aware fill, which this backend does not run
            // at all (tests/gpu_extended_envelope.rs).
            ExtOperator::ExtendedEnvelope(op) => op.defined() && op != ExtendedOp::ContentAwareFill,
            ExtOperator::AdjustmentLayer
            | ExtOperator::BlendIf
            | ExtOperator::MaskDensity
            | ExtOperator::MaskFeather
            | ExtOperator::EffectsNested
            | ExtOperator::Transform(_)
            | ExtOperator::Blur(_)
            | ExtOperator::Filter(_)
            // Smart-filter stacks (lane SMB-PS-SMART-FILTER-WIRING): filter-only stacks without
            // masks run as filter + mix passes within the filter tolerance (tests/gpu_smart_filter.rs);
            // liquify entries are a typed refusal at prepare; masks run as the masked mix (lane
            // SMB-PS-ADJUST-STYLES-3, tests/gpu_smart_filter_masks.rs).
            | ExtOperator::SmartFilter
            // Knockout (lane SMB-PS-ADJUST-STYLES-3): every placement pigment admits, as the
            // knockout passes (`knockout` module, tests/gpu_knockout.rs).
            | ExtOperator::Knockout(_) => true,
            ExtOperator::AlphaPremultiplied => false,
            // 8-bit working depth (lane SMB-PS-ORACLE-PARITY-2): `Exec::store_depth` after every
            // layer and the 8-bit Levels body of kernel.wgsl.
            ExtOperator::WorkingDepthEight => true,
            // Key pipelines (lane SMB-AE-KEY-TRACK-3): `key.wgsl` on keyed tile layers
            // (tests/gpu_keying.rs); inner/outer key, key cleaner and wire removal are not run.
            ExtOperator::Key(kind) => key::gpu_key_supported(kind),
            ExtOperator::Adjustment(kind) => gpu_adjust_supported(kind),
            ExtOperator::Effect(kind) => matches!(
                kind,
                EffectKind::DropShadow
                    | EffectKind::ColourOverlay
                    | EffectKind::Stroke
                    | EffectKind::InnerShadow
                    | EffectKind::OuterGlow
                    | EffectKind::InnerGlow
                    // lane SMB-PS-ADJUST-STYLES-2 (fx.rs satin / bevel passes, gradient colours
                    // from pigment; tests/gpu_layer_styles_2.rs)
                    | EffectKind::Satin
                    | EffectKind::BevelEmboss
                    | EffectKind::GradientOverlay
                    | EffectKind::PatternOverlay
            ),
            _ => false,
        };
        if supported {
            operator.intrinsic().lowest_of(Exactness::Fitted)
        } else {
            Exactness::Unsupported
        }
    }
    fn execute_extended(
        &self,
        plan: &RenderPlan<NoVector>,
        extensions: &PlanExtensions,
        request: &ExecuteRequest,
        sources: &dyn PlanSources,
        cancel: &CancellationToken,
        sink: &mut dyn TileSink,
    ) -> Result<RenderReceipt, BackendError> {
        // An empty envelope with no mask density below 1 is the v1 plan, byte for byte.
        if extensions.is_empty() && !plan.has_mask_density() {
            return self.execute(plan, request, sources, cancel, sink);
        }
        self.execute_extended_detailed(plan, extensions, request, sources, cancel, sink)
            .map(|(receipt, _)| receipt)
            .map_err(BackendError::from)
    }
    fn execute(
        &self,
        plan: &RenderPlan<NoVector>,
        request: &ExecuteRequest,
        sources: &dyn PlanSources,
        cancel: &CancellationToken,
        sink: &mut dyn TileSink,
    ) -> Result<RenderReceipt, BackendError> {
        self.execute_detailed(plan, request, sources, cancel, sink)
            .map(|(receipt, _)| receipt)
            .map_err(BackendError::from)
    }
}

// ---- device execution ---------------------------------------------------------------------------

/// Create device objects with validation and out-of-memory errors captured as typed results.
fn scoped<T>(
    device: &wgpu::Device,
    requested: u64,
    budget: u64,
    create: impl FnOnce() -> T,
) -> Result<T, GpuError> {
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let value = create();
    let oom = crate::device::block_on_bounded(device.pop_error_scope(), None);
    let validation = crate::device::block_on_bounded(device.pop_error_scope(), None);
    match (oom, validation) {
        (Some(None), Some(None)) => Ok(value),
        (Some(Some(_)), _) => Err(GpuError::BudgetExceeded { requested, budget }),
        _ => Err(GpuError::Gpu("device_validation")),
    }
}

/// Bytes of a `width x height` RGBA f32 region; saturates instead of wrapping (a saturated size is
/// refused by the budget checks).
fn texel_bytes(width: u32, height: u32) -> u64 {
    u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(TEXEL_BYTES)
}

/// Where one composite pass reads its source texels.
enum Src<'x, 'a> {
    Solid([f32; 4]),
    Tile(&'x TilePlanes<'a>, Rect),
    /// A tile resampled through a layer transform by the resample pass.
    Transformed(&'x TilePlanes<'a>, &'x PreparedTransform),
    /// The finished group buffer of the next nesting level.
    Chunk,
}

/// Device buffers of the resample pass (only when a layer is transformed): the parameter block,
/// the per-pixel sample sites (frame-sized), the source window (the largest transformed tile) and
/// the bind group that writes the result into `Exec::src`.
struct ResampleBuffers {
    params: wgpu::Buffer,
    sites: wgpu::Buffer,
    window: wgpu::Buffer,
    bind: wgpu::BindGroup,
}

/// Envelope device buffers and bind groups (only when a layer uses Blend-If or an adjustment).
struct ExtBuffers {
    ext: wgpu::Buffer,
    lut: wgpu::Buffer,
    /// Like `Exec::binds_tile` / `binds_chunk`, over the envelope layout (bindings 0-5).
    binds_tile: Vec<wgpu::BindGroup>,
    binds_chunk: Vec<wgpu::BindGroup>,
}

/// Device buffers of the effect passes (only when a layer has effects): the parameter block and
/// three f32 scratch planes of frame size. The matte itself is written into `Exec::mask`.
struct FxBuffers {
    params: wgpu::Buffer,
    planes: [wgpu::Buffer; 3],
}

/// Device buffers of the blur passes (only when a layer has a blur): the parameter block and two
/// frame-sized RGBA scratch planes.
struct BlurBuffers {
    params: wgpu::Buffer,
    a: wgpu::Buffer,
    b: wgpu::Buffer,
}

/// Device buffers and bind groups of one execution plus the state the layer recursion shares.
struct Exec<'e> {
    ctx: &'e GpuContext,
    cancel: &'e CancellationToken,
    deadline: Option<Instant>,
    /// Chunk-sized destinations; `levels[0]` is the root destination that is read back.
    levels: Vec<wgpu::Buffer>,
    src: wgpu::Buffer,
    mask: wgpu::Buffer,
    params: wgpu::Buffer,
    /// `binds_tile[l]`: dst = `levels[l]`, src = the upload buffer (also used for solid fills).
    binds_tile: Vec<wgpu::BindGroup>,
    /// `binds_chunk[l]`: dst = `levels[l]`, src = `levels[l + 1]` (an isolated group's result).
    binds_chunk: Vec<wgpu::BindGroup>,
    ext: Option<ExtBuffers>,
    fx: Option<FxBuffers>,
    resample: Option<ResampleBuffers>,
    blur: Option<BlurBuffers>,
    /// Blur kernels of the plan's effects (the passes index into it).
    kernels: &'e FxKernels,
    /// Decoded pattern pixels of the pattern overlays.
    patterns: &'e std::collections::BTreeMap<SourceKey, Vec<[f32; 4]>>,
    /// Blurred mask coverage of the envelope's feathered layers (`MaskView::feather` indexes it).
    feathered: &'e [FeatherPlane],
    upload: Vec<u8>,
    mask_upload: Vec<u8>,
    stats: &'e mut GpuStats,
    /// Extended-range output stage: every composite dispatch carries `EXTENDED_FLAG`.
    extended: bool,
    /// 8-bit working depth (lane SMB-PS-ORACLE-PARITY-2): the running result is stored in 8-bit
    /// levels after every layer (`Exec::store_depth`).
    eight_bit: bool,
    /// Knockout buffers and contexts (only when the plan has a knockout layer).
    ko: Option<knockout::KoBuffers>,
}

/// `a - b` as the unsigned offset of a region inside its chunk; a negative or oversized offset is
/// an overflow, not a wrapped index (`clippy::arithmetic_side_effects` is denied here).
fn offset(a: i64, b: i64) -> Result<u32, GpuError> {
    a.checked_sub(b)
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(overflow)
}

impl Exec<'_> {
    fn check(&self) -> Result<(), GpuError> {
        self.cancel
            .check()
            .map_err(|_| GpuError::Backend(BackendError::Canceled))?;
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(GpuError::Backend(BackendError::DeadlineExceeded));
        }
        self.ctx.check_alive()
    }

    /// Make the first `bytes` of a level fully transparent.
    /// Copy `levels[from]` into `levels[to]` (the first `bytes`).
    fn copy_level(&self, from: usize, to: usize, bytes: u64) {
        let mut encoder = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("hsk-studio-render-gpu copy level"),
            });
        encoder.copy_buffer_to_buffer(&self.levels[from], 0, &self.levels[to], 0, bytes);
        self.ctx.queue.submit(Some(encoder.finish()));
    }

    /// pigment `interior_mix` over `region`: `levels[level]` (below) and `levels[level + 1]` (the
    /// unit) by `weight`: the shape alpha already in the mask buffer (the interior unit) or a
    /// uniform opacity (the unit fade of a layer with effects; a pass-through layer, pigment
    /// `blend_math::pass_through_mix`).
    fn composite_mix(
        &mut self,
        region: Rect,
        chunk: Rect,
        level: usize,
        weight: MixWeight,
    ) -> Result<(), GpuError> {
        let ctx = self.ctx;
        let (has_mask, opacity) = match weight {
            MixWeight::MaskBuffer => (true, 1.0),
            MixWeight::Uniform(o) => (false, o),
        };
        let params = LayerParams {
            chunk_w: chunk.width,
            chunk_h: chunk.height,
            region: [
                offset(region.x, chunk.x)?,
                offset(region.y, chunk.y)?,
                region.width,
                region.height,
            ],
            mode: StudioBlendMode::Normal,
            source: SourceKind::Chunk,
            has_mask,
            opacity,
            unfilled: 1.0,
            fill: 1.0,
            replace: false,
            clip: false,
            opaque_source: false,
            mix: true,
            density: 1.0,
            extended: self.extended,
            quantise: false,
            eight_bit: false,
        };
        ctx.queue.write_buffer(&self.params, 0, &params.to_bytes());
        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("hsk-studio-render-gpu interior mix"),
            });
        ctx.kernel
            .dispatch(&mut encoder, &self.binds_chunk[level], region.width, region.height);
        ctx.queue.submit(Some(encoder.finish()));
        self.stats.dispatches = self.stats.dispatches.saturating_add(1);
        Ok(())
    }

    /// The working depth after a layer (lane SMB-PS-ORACLE-PARITY-2, pigment
    /// `render_plan::quantise_8bit`): in an 8-bit document every texel of `levels[level]` is stored
    /// in 8-bit levels (one dispatch over the chunk); float: nothing.
    fn store_depth(&mut self, chunk: Rect, level: usize) -> Result<(), GpuError> {
        if !self.eight_bit {
            return Ok(());
        }
        let ctx = self.ctx;
        let params = LayerParams {
            chunk_w: chunk.width,
            chunk_h: chunk.height,
            region: [0, 0, chunk.width, chunk.height],
            mode: StudioBlendMode::Normal,
            source: SourceKind::Solid([0.0; 4]),
            has_mask: false,
            opacity: 1.0,
            unfilled: 1.0,
            fill: 1.0,
            replace: false,
            clip: false,
            opaque_source: false,
            mix: false,
            density: 1.0,
            extended: self.extended,
            quantise: true,
            eight_bit: false,
        };
        ctx.queue.write_buffer(&self.params, 0, &params.to_bytes());
        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("hsk-studio-render-gpu working depth"),
            });
        ctx.kernel
            .dispatch(&mut encoder, &self.binds_tile[level], chunk.width, chunk.height);
        ctx.queue.submit(Some(encoder.finish()));
        self.stats.dispatches = self.stats.dispatches.saturating_add(1);
        Ok(())
    }

    fn clear(&self, level: usize, bytes: u64) {
        let mut encoder = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("hsk-studio-render-gpu clear"),
            });
        encoder.clear_buffer(&self.levels[level], 0, Some(bytes));
        self.ctx.queue.submit(Some(encoder.finish()));
    }

    /// Upload the coverage of `region` (feather and inversion applied on the host; the density step
    /// runs in the shader).
    fn upload_mask(&mut self, mask: &MaskView<'_>, region: Rect) {
        self.mask_upload.clear();
        for y in 0..i64::from(region.height) {
            for x in 0..i64::from(region.width) {
                let c = mask.coverage(
                    self.feathered,
                    region.x.saturating_add(x),
                    region.y.saturating_add(y),
                );
                self.mask_upload.extend_from_slice(&c.to_ne_bytes());
            }
        }
        self.ctx
            .queue
            .write_buffer(&self.mask, 0, &self.mask_upload);
        self.stats.bytes_uploaded = self
            .stats
            .bytes_uploaded
            .saturating_add(self.mask_upload.len() as u64);
    }

    /// Composite `source` over `region` (chunk-clipped) into `levels[level]` with `mode`, `opacity`
    /// and `mask`. One upload (tile region, mask coverage) plus one dispatch. `pass` selects the
    /// plain composite, the clip-run base initialisation or a clipped-layer composite; `blend_if`
    /// (envelope) multiplies the source alpha by the Blend-If weight in the shader.
    #[allow(clippy::too_many_arguments)]
    fn composite(
        &mut self,
        mode: StudioBlendMode,
        opacity: Opac,
        mask: Option<&MaskView<'_>>,
        blend_if: Option<&BlendIfGpu>,
        pass: Pass,
        region: Rect,
        chunk: Rect,
        level: usize,
        source: Src<'_, '_>,
    ) -> Result<(), GpuError> {
        let ctx = self.ctx;
        let kind = self.stage(source, region)?;
        if let Some(mask) = mask {
            self.upload_mask(mask, region);
        }
        let params = LayerParams {
            chunk_w: chunk.width,
            chunk_h: chunk.height,
            region: [
                offset(region.x, chunk.x)?,
                offset(region.y, chunk.y)?,
                region.width,
                region.height,
            ],
            mode,
            source: kind,
            has_mask: mask.is_some(),
            opacity: opacity.folded,
            unfilled: opacity.unfilled,
            fill: opacity.fill,
            replace: pass == Pass::Replace,
            clip: pass == Pass::Clip,
            opaque_source: pass == Pass::Opaque,
            mix: pass == Pass::Mix,
            density: mask.map_or(1.0, |m| m.density),
            extended: self.extended,
            quantise: false,
            eight_bit: self.eight_bit,
        };
        ctx.queue.write_buffer(&self.params, 0, &params.to_bytes());
        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("hsk-studio-render-gpu layer"),
            });
        match (blend_if, self.ext.as_ref()) {
            (Some(bi), Some(env)) => {
                ctx.queue
                    .write_buffer(&env.ext, 0, &ext_bytes(Some(bi), None));
                let bind = match kind {
                    SourceKind::Chunk => &env.binds_chunk[level],
                    _ => &env.binds_tile[level],
                };
                ctx.kernel
                    .dispatch_ext(&mut encoder, bind, region.width, region.height);
            }
            _ => {
                let bind = match kind {
                    SourceKind::Chunk => &self.binds_chunk[level],
                    _ => &self.binds_tile[level],
                };
                ctx.kernel
                    .dispatch(&mut encoder, bind, region.width, region.height);
            }
        }
        ctx.queue.submit(Some(encoder.finish()));
        self.stats.dispatches = self.stats.dispatches.saturating_add(1);
        Ok(())
    }

    /// Stage a composite source over `region`: a tile region uploaded into `src`, a transformed
    /// tile resampled into `src`, a solid colour, or the finished group buffer of the next level.
    fn stage(&mut self, source: Src<'_, '_>, region: Rect) -> Result<SourceKind, GpuError> {
        let ctx = self.ctx;
        Ok(match source {
            Src::Solid(rgba) => SourceKind::Solid(rgba),
            Src::Chunk => SourceKind::Chunk,
            Src::Transformed(planes, prep) => {
                self.resample(planes, prep, region)?;
                SourceKind::Region
            }
            Src::Tile(planes, placement) => {
                self.upload.clear();
                for y in 0..i64::from(region.height) {
                    for x in 0..i64::from(region.width) {
                        let px = offset(region.x.saturating_add(x), placement.x)?;
                        let py = offset(region.y.saturating_add(y), placement.y)?;
                        for v in tile_pixel(planes, px, py) {
                            self.upload.extend_from_slice(&v.to_ne_bytes());
                        }
                    }
                }
                ctx.queue.write_buffer(&self.src, 0, &self.upload);
                self.stats.bytes_uploaded = self
                    .stats
                    .bytes_uploaded
                    .saturating_add(self.upload.len() as u64);
                SourceKind::Region
            }
        })
    }

    /// Resample `region` of a transformed tile into `src` (region-sized, row-major): the sample
    /// sites come from pigment `PreparedTransform::sample_site` (binary64, the reference's geometry),
    /// the source window is the tile rect covering every tap of the region, and `resample.wgsl`
    /// evaluates the filter in binary32.
    fn resample(
        &mut self,
        planes: &TilePlanes<'_>,
        prep: &PreparedTransform,
        region: Rect,
    ) -> Result<(), GpuError> {
        let ctx = self.ctx;
        let Some(rs) = self.resample.as_ref() else {
            return Err(unsupported("plan_shape"));
        };
        let taps: i64 = match prep.filter() {
            ResampleFilter::Bicubic => 4,
            _ => 2,
        };
        let (tw, th) = (i64::from(planes.width), i64::from(planes.height));
        let clamp_edge = prep.edge() == EdgeMode::Clamp;
        let (mut lo_x, mut lo_y, mut hi_x, mut hi_y) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
        let mut sites: Vec<u8> = Vec::new();
        let last = |len: i64| len.saturating_sub(1);
        for y in 0..i64::from(region.height) {
            for x in 0..i64::from(region.width) {
                let site = prep.sample_site(region.x.saturating_add(x), region.y.saturating_add(y));
                let (c, r, tx, ty, span) = match site {
                    SampleSite::Transparent => (0, 0, -1.0_f32, 0.0_f32, 0),
                    SampleSite::Texel { column, row } => {
                        (i64::from(column), i64::from(row), -2.0, 0.0, 1)
                    }
                    SampleSite::Filtered {
                        first_column,
                        first_row,
                        tx,
                        ty,
                    } => (first_column, first_row, tx as f32, ty as f32, taps),
                };
                if span > 0 {
                    // The texels these taps read (clamped into the tile: a tap outside reads the
                    // edge texel in clamp mode and nothing in transparent mode).
                    let end = |v: i64| v.saturating_add(span).saturating_sub(1);
                    let (x0, x1) = (c.clamp(0, last(tw)), end(c).clamp(0, last(tw)));
                    let (y0, y1) = (r.clamp(0, last(th)), end(r).clamp(0, last(th)));
                    let inside_x = clamp_edge || (end(c) >= 0 && c < tw);
                    let inside_y = clamp_edge || (end(r) >= 0 && r < th);
                    if inside_x && inside_y {
                        lo_x = lo_x.min(x0);
                        hi_x = hi_x.max(x1);
                        lo_y = lo_y.min(y0);
                        hi_y = hi_y.max(y1);
                    }
                }
                let word = |v: i64| i32::try_from(v).map(i32::to_ne_bytes).map_err(|_| overflow());
                sites.extend_from_slice(&word(c)?);
                sites.extend_from_slice(&word(r)?);
                sites.extend_from_slice(&tx.to_ne_bytes());
                sites.extend_from_slice(&ty.to_ne_bytes());
            }
        }
        // The source window (empty when nothing in the region reads the tile).
        self.upload.clear();
        let (sx, sy, sw, sh) = if lo_x <= hi_x && lo_y <= hi_y {
            for row in lo_y..=hi_y {
                for column in lo_x..=hi_x {
                    let (cx, cy) = (
                        u32::try_from(column).map_err(|_| overflow())?,
                        u32::try_from(row).map_err(|_| overflow())?,
                    );
                    for v in tile_pixel(planes, cx, cy) {
                        self.upload.extend_from_slice(&v.to_ne_bytes());
                    }
                }
            }
            let side = |a: i64, b: i64| {
                b.checked_sub(a)
                    .and_then(|d| d.checked_add(1))
                    .and_then(|d| u32::try_from(d).ok())
                    .ok_or_else(overflow)
            };
            (lo_x, lo_y, side(lo_x, hi_x)?, side(lo_y, hi_y)?)
        } else {
            (0, 0, 0, 0)
        };
        if !self.upload.is_empty() {
            ctx.queue.write_buffer(&rs.window, 0, &self.upload);
        }
        ctx.queue.write_buffer(&rs.sites, 0, &sites);
        let filter: u32 = match prep.filter() {
            ResampleFilter::Bicubic => 2,
            _ => 1,
        };
        let word_i = |v: i64| i32::try_from(v).map(i32::to_ne_bytes).map_err(|_| overflow());
        let mut params: Vec<u8> = Vec::with_capacity(48);
        for w in [
            region.width.to_ne_bytes(),
            region.height.to_ne_bytes(),
            word_i(sx)?,
            word_i(sy)?,
            sw.to_ne_bytes(),
            sh.to_ne_bytes(),
            planes.width.to_ne_bytes(),
            planes.height.to_ne_bytes(),
            filter.to_ne_bytes(),
            u32::from(clamp_edge).to_ne_bytes(),
            u32::from(self.extended).to_ne_bytes(),
            0u32.to_ne_bytes(),
        ] {
            params.extend_from_slice(&w);
        }
        ctx.queue.write_buffer(&rs.params, 0, &params);
        self.stats.bytes_uploaded = self.stats.bytes_uploaded.saturating_add(
            u64::try_from(sites.len().saturating_add(self.upload.len())).unwrap_or(u64::MAX),
        );
        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("hsk-studio-render-gpu resample"),
            });
        ctx.kernel
            .dispatch_resample(&mut encoder, &rs.bind, region.width, region.height);
        ctx.queue.submit(Some(encoder.finish()));
        self.stats.dispatches = self.stats.dispatches.saturating_add(1);
        Ok(())
    }

    /// One matte pass over `region` of the frame `chunk`: reads the content frame `levels[content]`
    /// (its alpha is the layer's shape alpha) and the planes the step names, writes its output plane.
    fn fx_pass(
        &mut self,
        step: &FxStep,
        region: Rect,
        chunk: Rect,
        content: usize,
    ) -> Result<(), GpuError> {
        let ctx = self.ctx;
        let Some(fx) = self.fx.as_ref() else {
            return Err(unsupported("plan_shape"));
        };
        let bytes = params_bytes(
            step,
            self.kernels,
            chunk.width,
            [
                offset(region.x, chunk.x)?,
                offset(region.y, chunk.y)?,
                region.width,
                region.height,
            ],
        )
        .map_err(|e| fx_error(e, u64::MAX))?;
        ctx.queue.write_buffer(&fx.params, 0, &bytes);
        let plane = |b: Buf| match b {
            Buf::T0 => &fx.planes[0],
            Buf::T1 => &fx.planes[1],
            Buf::T2 => &fx.planes[2],
            Buf::Mask => &self.mask,
        };
        let bind = ctx.kernel.bind_fx(
            &ctx.device,
            &fx.params,
            [plane(step.fin), plane(step.fin2), plane(step.fout)],
            &self.levels[content],
        );
        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("hsk-studio-render-gpu effect pass"),
            });
        ctx.kernel
            .dispatch_fx(&mut encoder, &bind, region.width, region.height);
        ctx.queue.submit(Some(encoder.finish()));
        self.stats.dispatches = self.stats.dispatches.saturating_add(1);
        Ok(())
    }

    /// Composite the matte (in the mask buffer, region-sized) as a solid colour onto
    /// `levels[level]` with the effect's mode and `opacity` (effect opacity times layer opacity).
    ///
    /// A gradient overlay (lane SMB-PS-ADJUST-STYLES-2) composites a per-pixel source instead: its
    /// colour and stop opacity at every region pixel come from pigment's
    /// `GradientOverlayOp::sample_at` over the layer bounds `layer` (the windowed evaluator's
    /// geometry), uploaded as the source region; the matte in the mask buffer is the alpha.
    #[allow(clippy::too_many_arguments)]
    fn composite_matte(
        &mut self,
        effect: &FxEffect,
        opacity: f32,
        region: Rect,
        chunk: Rect,
        level: usize,
        layer: Rect,
    ) -> Result<(), GpuError> {
        let ctx = self.ctx;
        let [r, g, b] = effect.colour;
        let source = match &effect.gradient {
            Some(o) => {
                self.upload.clear();
                for y in 0..i64::from(region.height) {
                    for x in 0..i64::from(region.width) {
                        let (c, stop) = o.sample_at(
                            region.x.saturating_add(x),
                            region.y.saturating_add(y),
                            layer,
                        );
                        for v in [c[0], c[1], c[2], stop] {
                            self.upload.extend_from_slice(&v.to_ne_bytes());
                        }
                    }
                }
                ctx.queue.write_buffer(&self.src, 0, &self.upload);
                self.stats.bytes_uploaded = self
                    .stats
                    .bytes_uploaded
                    .saturating_add(self.upload.len() as u64);
                SourceKind::Region
            }
            None => match &effect.pattern {
                Some(o) => {
                    let pixels = self
                        .patterns
                        .get(&o.pattern.key)
                        .ok_or_else(|| unsupported("pattern_unresolved"))?;
                    self.upload.clear();
                    for y in 0..i64::from(region.height) {
                        for x in 0..i64::from(region.width) {
                            let p = o
                                .sample_at(region.x.saturating_add(x), region.y.saturating_add(y), layer, pixels)
                                .ok_or_else(|| unsupported("pattern_unresolved"))?;
                            for v in p {
                                self.upload.extend_from_slice(&v.to_ne_bytes());
                            }
                        }
                    }
                    ctx.queue.write_buffer(&self.src, 0, &self.upload);
                    self.stats.bytes_uploaded = self
                        .stats
                        .bytes_uploaded
                        .saturating_add(self.upload.len() as u64);
                    SourceKind::Region
                }
                None => SourceKind::Solid([r, g, b, 1.0]),
            },
        };
        let params = LayerParams {
            chunk_w: chunk.width,
            chunk_h: chunk.height,
            region: [
                offset(region.x, chunk.x)?,
                offset(region.y, chunk.y)?,
                region.width,
                region.height,
            ],
            mode: effect.mode,
            source,
            has_mask: true,
            opacity,
            unfilled: opacity,
            fill: 1.0,
            replace: false,
            clip: false,
            opaque_source: false,
            mix: false,
            density: 1.0,
            // Effect colour over the running frame (lane SMB-RENDER-GPU-EXTENDED-ENVELOPE).
            extended: self.extended,
            quantise: false,
            eight_bit: false,
        };
        ctx.queue.write_buffer(&self.params, 0, &params.to_bytes());
        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("hsk-studio-render-gpu effect composite"),
            });
        ctx.kernel.dispatch(
            &mut encoder,
            &self.binds_tile[level],
            region.width,
            region.height,
        );
        ctx.queue.submit(Some(encoder.finish()));
        self.stats.dispatches = self.stats.dispatches.saturating_add(1);
        Ok(())
    }

    /// Adjustment layer: `levels[level]` (the running backdrop of the layer's group, or the run
    /// unit of a clipping run) is adjusted in place over `region`. A clipped adjustment ignores a
    /// mask like every clipped layer.
    #[allow(clippy::too_many_arguments)]
    fn adjust(
        &mut self,
        layer: &PLayer<'_>,
        adjust: &AdjustGpu,
        pass: Pass,
        region: Rect,
        chunk: Rect,
        level: usize,
    ) -> Result<(), GpuError> {
        let ctx = self.ctx;
        let mask = if pass == Pass::Clip {
            None
        } else {
            layer.mask.as_ref()
        };
        if let Some(mask) = mask {
            self.upload_mask(mask, region);
        }
        let params = LayerParams {
            chunk_w: chunk.width,
            chunk_h: chunk.height,
            region: [
                offset(region.x, chunk.x)?,
                offset(region.y, chunk.y)?,
                region.width,
                region.height,
            ],
            mode: layer.mode,
            source: SourceKind::Solid([0.0; 4]),
            has_mask: mask.is_some(),
            opacity: layer.opacity,
            unfilled: layer.layer_opacity,
            fill: 1.0,
            replace: false,
            clip: false,
            opaque_source: false,
            mix: false,
            density: mask.map_or(1.0, |m| m.density),
            // Adjustment layer (lane SMB-RENDER-GPU-EXTENDED-ENVELOPE: linear exposure only).
            extended: self.extended,
            quantise: false,
            eight_bit: false,
        };
        let Some(env) = self.ext.as_ref() else {
            return Err(unsupported("plan_shape"));
        };
        ctx.queue.write_buffer(&self.params, 0, &params.to_bytes());
        ctx.queue.write_buffer(
            &env.ext,
            0,
            &ext_bytes(layer.blend_if.as_ref(), Some(adjust)),
        );
        if let Some(lut) = &adjust.lut {
            let mut bytes = Vec::with_capacity(lut.len().saturating_mul(4));
            for v in lut {
                bytes.extend_from_slice(&v.to_ne_bytes());
            }
            ctx.queue.write_buffer(&env.lut, 0, &bytes);
        }
        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("hsk-studio-render-gpu adjust"),
            });
        ctx.kernel.dispatch_adjust(
            &mut encoder,
            &env.binds_tile[level],
            region.width,
            region.height,
        );
        ctx.queue.submit(Some(encoder.finish()));
        self.stats.dispatches = self.stats.dispatches.saturating_add(1);
        Ok(())
    }
}

/// Which shader path a composite takes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pass {
    Normal,
    /// Initialise a cleared clip-run buffer from the base content (mask only; no opacity/mode).
    Replace,
    /// A clipped layer inside a run: source alpha times the run's alpha, alpha held.
    Clip,
    /// The content of an interior unit: the source counts as fully covering (alpha 1).
    Opaque,
    /// pigment `interior_mix`: the destination (below) and the source (the unit) by the mask.
    Mix,
}

/// Layers of group `idx`, bottom-first, composited into `levels[level]`. A layer followed by
/// clipped layers starts a clipping run, evaluated as one isolated unit.
fn eval_group(
    ex: &mut Exec<'_>,
    prep: &Prepared<'_>,
    idx: usize,
    chunk: Rect,
    level: usize,
) -> Result<(), GpuError> {
    let PNode::Group { layers, .. } = &prep.nodes[idx] else {
        return Err(unsupported("plan_shape"));
    };
    let mut i = 0;
    while i < layers.len() {
        let mut j = i.saturating_add(1);
        while j < layers.len() && layers[j].clipped {
            j = j.saturating_add(1);
        }
        if j == i.saturating_add(1) {
            if layers[i]
                .blur
                .as_ref()
                .is_some_and(|b| b.scope == BlurScope::Backdrop)
            {
                eval_backdrop_blur(ex, prep, &layers[i], chunk, level)?;
            }
            eval_layer(ex, prep, &layers[i], chunk, level)?;
        } else {
            eval_clip_run(ex, prep, &layers[i..j], chunk, level)?;
        }
        // the root's last layer is the delivered result, not a stored intermediate
        if idx != prep.root || j < layers.len() {
            ex.store_depth(chunk, level)?;
        }
        i = j;
    }
    Ok(())
}

/// Composite one layer's source (leaf or finished isolated group) into `levels[level]`.
fn source_pass(
    ex: &mut Exec<'_>,
    prep: &Prepared<'_>,
    layer: &PLayer<'_>,
    pass: Pass,
    region: Rect,
    chunk: Rect,
    level: usize,
) -> Result<(), GpuError> {
    // An adjustment layer's content is f(running backdrop): no source to composite.
    if let Some(adjust) = &layer.adjust {
        return ex.adjust(layer, adjust, pass, region, chunk, level);
    }
    let mask = if pass == Pass::Clip {
        None
    } else {
        layer.mask.as_ref()
    };
    // Initialising the run unit from the base never weighs it (a base with Blend-If is refused in
    // `prepare`).
    let blend_if = if pass == Pass::Replace {
        None
    } else {
        layer.blend_if.as_ref()
    };
    match prep.node(layer.input)? {
        // Isolated: own transparent buffer one level down, then composited as a source.
        PNode::Group { isolated: true, .. } => {
            ex.clear(
                level.saturating_add(1),
                texel_bytes(chunk.width, chunk.height),
            );
            // A deep knockout inside clears this backdrop by its shape before the group
            // composites (lane SMB-PS-ADJUST-STYLES-3).
            if prep.ko.deep_groups.contains(&layer.input) {
                ex.ko_push_isolated(level.saturating_add(1), chunk)?;
                let evaluated = eval_group(ex, prep, layer.input, chunk, level.saturating_add(1));
                let cleared = ex.ko_pop_isolated(level, chunk);
                evaluated?;
                cleared?;
            } else {
                eval_group(ex, prep, layer.input, chunk, level.saturating_add(1))?;
            }
            ex.composite(
                layer.mode,
                layer.opac(),
                mask,
                blend_if,
                pass,
                region,
                chunk,
                level,
                Src::Chunk,
            )
        }
        PNode::Tile { planes, rect } => ex.composite(
            layer.mode,
            layer.opac(),
            mask,
            blend_if,
            pass,
            region,
            chunk,
            level,
            Src::Tile(planes, *rect),
        ),
        PNode::Transformed { planes, prep } => ex.composite(
            layer.mode,
            layer.opac(),
            mask,
            blend_if,
            pass,
            region,
            chunk,
            level,
            Src::Transformed(planes, prep),
        ),
        PNode::Solid { rgba } if rgba[3] <= 0.0 => Ok(()),
        PNode::Solid { rgba } => ex.composite(
            layer.mode,
            layer.opac(),
            mask,
            blend_if,
            pass,
            region,
            chunk,
            level,
            Src::Solid(*rgba),
        ),
        // Pass-through groups are handled by the caller; a clipped or base layer never has one.
        PNode::Group {
            isolated: false, ..
        }
        | PNode::Unused => Err(unsupported("plan_shape")),
    }
}

fn eval_layer(
    ex: &mut Exec<'_>,
    prep: &Prepared<'_>,
    layer: &PLayer<'_>,
    chunk: Rect,
    level: usize,
) -> Result<(), GpuError> {
    ex.check()?;
    if let Some(fx) = &layer.fx {
        return eval_effect_layer(ex, prep, layer, fx, chunk, level);
    }
    if let Some(blur) = layer.blur.as_ref().filter(|b| b.scope == BlurScope::Layer) {
        return eval_blur_layer(ex, prep, layer, blur, chunk, level);
    }
    if let Some(filter) = &layer.filter {
        return eval_filter_layer(ex, prep, layer, filter, chunk, level);
    }
    if let Some(smart) = &layer.smart {
        return eval_smart_layer(ex, prep, layer, smart, chunk, level);
    }
    // Knockout (lane SMB-PS-ADJUST-STYLES-3): before the opacity test, a layer at fill 0 still
    // knocks out.
    if layer.knockout != Knockout::None {
        return knockout::eval_knockout_layer(ex, prep, layer, chunk, level);
    }
    // A group enclosing a deep knockout clears the backdrop even when it is invisible itself (as
    // render-cpu evaluates every layer).
    if layer.opacity <= 0.0 && !prep.ko.deep_groups.contains(&layer.input) {
        return Ok(());
    }
    let Some(region) = prep
        .bound(layer.input)
        .map(|b| intersect(b, chunk))
        .transpose()?
        .flatten()
    else {
        return Ok(());
    };
    // Pass-through: the child's layers composite onto this level's running backdrop.
    if matches!(
        prep.node(layer.input)?,
        PNode::Group {
            isolated: false,
            ..
        }
    ) {
        // A pass-through layer with opacity (lane SMB-PS-ORACLE-PARITY-2, pigment
        // `pass_through_mix`): the children composite onto a copy one level down, which is mixed
        // back by the opacity (a mask on a pass-through layer is refused in `prepare`).
        let weighed = layer.layer_opacity < 1.0;
        let target = if weighed {
            let unit = level.saturating_add(1);
            ex.copy_level(level, unit, texel_bytes(chunk.width, chunk.height));
            unit
        } else {
            level
        };
        // A shallow knockout inside reads the running result before the group (its snapshot).
        let evaluated = if !prep.ko.snapshot_groups.contains(&layer.input) {
            eval_group(ex, prep, layer.input, chunk, target)
        } else {
            ex.ko_push_snapshot(target, layer.input, chunk)?;
            let evaluated = eval_group(ex, prep, layer.input, chunk, target);
            ex.ko_pop();
            evaluated
        };
        evaluated?;
        if weighed {
            ex.composite_mix(region, chunk, level, MixWeight::Uniform(layer.layer_opacity))?;
        }
        return Ok(());
    }
    source_pass(ex, prep, layer, Pass::Normal, region, chunk, level)
}

/// A layer with effects (pigment `render_layer_reference` order): every behind-content effect in
/// list order, the content with `opacity * fill`, every over-content effect in list order. The
/// content (with the layer mask applied to its alpha: the shape the mattes derive from) is
/// rendered into the level below, each effect's matte is built from its alpha by the matte passes
/// into the mask buffer and composited as a solid colour with the effect's mode and
/// `effect opacity * layer opacity` (effects fade with the layer opacity, not with the fill).
/// A layer opacity below 1 fades the layer and its effects as one unit (lane
/// SMB-PS-ORACLE-PARITY-2, pigment `render_layer_reference`): the running result is copied one
/// level down, the stack runs there at opacity 1 (fill kept), and the unit is mixed back into this
/// level by the constant opacity (`interior_mix`).
fn eval_effect_layer(
    ex: &mut Exec<'_>,
    prep: &Prepared<'_>,
    layer: &PLayer<'_>,
    fx: &FxLayer,
    chunk: Rect,
    level: usize,
) -> Result<(), GpuError> {
    if layer.layer_opacity <= 0.0 {
        return Ok(());
    }
    if layer.layer_opacity >= 1.0 {
        return eval_effect_unit(ex, prep, layer, fx, chunk, level, layer.opac(), layer.layer_opacity);
    }
    let Some(bounds) = prep.bound(layer.input) else {
        return Ok(());
    };
    let Some(fx_region) = intersect(grow_rect(bounds, fx.margin)?, chunk)? else {
        return Ok(());
    };
    let unit = level.saturating_add(1);
    ex.copy_level(level, unit, texel_bytes(chunk.width, chunk.height));
    let whole = Opac { folded: layer.fill, unfilled: 1.0, fill: layer.fill };
    eval_effect_unit(ex, prep, layer, fx, chunk, unit, whole, 1.0)?;
    ex.composite_mix(fx_region, chunk, level, MixWeight::Uniform(layer.layer_opacity))
}

/// The stack of [`eval_effect_layer`] composed onto `level` with the content opacities `opac` and
/// the effects faded by `effect_scale` (the layer opacity, or 1 inside a unit fade).
#[allow(clippy::too_many_arguments)]
fn eval_effect_unit(
    ex: &mut Exec<'_>,
    prep: &Prepared<'_>,
    layer: &PLayer<'_>,
    fx: &FxLayer,
    chunk: Rect,
    level: usize,
    opac: Opac,
    effect_scale: f32,
) -> Result<(), GpuError> {
    // Content outside the frame is farther than the margin from every pixel that is kept.
    let Some(bounds) = prep.bound(layer.input) else {
        return Ok(());
    };
    let Some(content_region) = intersect(bounds, chunk)? else {
        return Ok(());
    };
    let Some(fx_region) = intersect(grow_rect(bounds, fx.margin)?, chunk)? else {
        return Ok(());
    };
    let content = level.saturating_add(1);
    ex.clear(content, texel_bytes(chunk.width, chunk.height));
    let mask = layer.mask.as_ref();
    match prep.node(layer.input)? {
        PNode::Group { isolated: true, .. } => {
            let below = content.saturating_add(1);
            ex.clear(below, texel_bytes(chunk.width, chunk.height));
            eval_group(ex, prep, layer.input, chunk, below)?;
            ex.composite(
                StudioBlendMode::Normal,
                Opac::plain(1.0),
                mask,
                None,
                Pass::Normal,
                content_region,
                chunk,
                content,
                Src::Chunk,
            )?;
        }
        PNode::Tile { planes, rect } => ex.composite(
            StudioBlendMode::Normal,
            Opac::plain(1.0),
            mask,
            None,
            Pass::Normal,
            content_region,
            chunk,
            content,
            Src::Tile(planes, *rect),
        )?,
        PNode::Transformed { planes, prep } => ex.composite(
            StudioBlendMode::Normal,
            Opac::plain(1.0),
            mask,
            None,
            Pass::Normal,
            content_region,
            chunk,
            content,
            Src::Transformed(planes, prep),
        )?,
        PNode::Solid { rgba } if rgba[3] <= 0.0 => {}
        PNode::Solid { rgba } => ex.composite(
            StudioBlendMode::Normal,
            Opac::plain(1.0),
            mask,
            None,
            Pass::Normal,
            content_region,
            chunk,
            content,
            Src::Solid(*rgba),
        )?,
        PNode::Group {
            isolated: false, ..
        }
        | PNode::Unused => return Err(unsupported("plan_shape")),
    }
    for behind in [true, false] {
        if !behind && fx.interior {
            // The interior unit (lane SMB-PS-ORACLE-PARITY-1, pigment `interior_mix`): the content
            // is copied two levels down (C), the result below one level down (U); the content
            // composites onto U as if it covered every pixel, the interior effects (mattes from C)
            // composite onto U, then U is mixed into this level by C's alpha.
            let bytes = texel_bytes(chunk.width, chunk.height);
            let copy = content.saturating_add(1);
            ex.copy_level(content, copy, bytes);
            ex.copy_level(level, content, bytes);
            ex.composite(
                layer.mode,
                opac,
                None,
                None,
                Pass::Opaque,
                content_region,
                chunk,
                content,
                Src::Chunk,
            )?;
            for effect in fx.effects.iter().filter(|e| e.interior) {
                ex.check()?;
                let opacity = effect.opacity * effect_scale;
                if effect.steps.is_empty() || opacity <= 0.0 {
                    continue;
                }
                for step in &effect.steps {
                    ex.fx_pass(step, fx_region, chunk, copy)?;
                }
                ex.composite_matte(effect, opacity, fx_region, chunk, content, bounds)?;
            }
            // the shape alpha into the mask, then the mix
            ex.fx_pass(&shape_alpha_step(), content_region, chunk, copy)?;
            ex.composite_mix(content_region, chunk, level, MixWeight::MaskBuffer)?;
        } else if !behind {
            // the content itself, mask already inside its alpha
            ex.composite(
                layer.mode,
                opac,
                None,
                None,
                Pass::Normal,
                content_region,
                chunk,
                level,
                Src::Chunk,
            )?;
        }
        // after the interior unit the content level holds the unit, the shape alpha lives in the
        // copy one level further down
        let shape_level = if fx.interior && !behind {
            content.saturating_add(1)
        } else {
            content
        };
        for effect in fx
            .effects
            .iter()
            .filter(|e| e.over_content != behind && !(fx.interior && e.interior))
        {
            ex.check()?;
            let opacity = effect.opacity * effect_scale;
            if effect.steps.is_empty() || opacity <= 0.0 {
                continue;
            }
            for step in &effect.steps {
                ex.fx_pass(step, fx_region, chunk, shape_level)?;
            }
            ex.composite_matte(effect, opacity, fx_region, chunk, level, bounds)?;
        }
    }
    Ok(())
}

/// `run = [base, clipped...]` evaluated as an isolated unit G one level down (pigment
/// `MaskFormat::BaseAlpha`): G starts as the base content with the base's own mask; each clipped
/// layer composites onto G with its own mode/opacity/fill, its source alpha multiplied by the base
/// alpha, and G's alpha is held at the base alpha; G then composites onto this level with the
/// BASE layer's mode, opacity and fill.
fn eval_clip_run(
    ex: &mut Exec<'_>,
    prep: &Prepared<'_>,
    run: &[PLayer<'_>],
    chunk: Rect,
    level: usize,
) -> Result<(), GpuError> {
    ex.check()?;
    let base = &run[0];
    if base.opacity <= 0.0 {
        return Ok(());
    }
    let Some(base_region) = prep
        .bound(base.input)
        .map(|b| intersect(b, chunk))
        .transpose()?
        .flatten()
    else {
        return Ok(());
    };
    let g = level.saturating_add(1);
    ex.clear(g, texel_bytes(chunk.width, chunk.height));
    source_pass(ex, prep, base, Pass::Replace, base_region, chunk, g)?;
    for clipped in &run[1..] {
        ex.check()?;
        if clipped.opacity <= 0.0 {
            continue;
        }
        // Content outside the base has zero alpha in G, so only the overlap can change.
        let Some(region) = prep
            .bound(clipped.input)
            .map(|b| intersect(b, base_region))
            .transpose()?
            .flatten()
        else {
            continue;
        };
        source_pass(ex, prep, clipped, Pass::Clip, region, chunk, g)?;
    }
    // A base with Blend-If is refused in `prepare` (pigment does not define where its ranges
    // apply), so this composite has no weight.
    ex.composite(
        base.mode,
        base.opac(),
        None,
        None,
        Pass::Normal,
        base_region,
        chunk,
        level,
        Src::Chunk,
    )
}

/// The layer's shape into the cleared `levels[content]` over `region`: its content composited
/// Normal at opacity 1 with its mask (an isolated group renders one level further down first),
/// exactly the content frame of [`eval_effect_layer`].
fn render_shape(
    ex: &mut Exec<'_>,
    prep: &Prepared<'_>,
    layer: &PLayer<'_>,
    region: Rect,
    chunk: Rect,
    content: usize,
) -> Result<(), GpuError> {
    render_content(ex, prep, layer, layer.mask.as_ref(), region, chunk, content)
}

/// [`render_shape`] with the mask chosen by the caller (`None` = the content before the mask).
fn render_content(
    ex: &mut Exec<'_>,
    prep: &Prepared<'_>,
    layer: &PLayer<'_>,
    mask: Option<&MaskView<'_>>,
    region: Rect,
    chunk: Rect,
    content: usize,
) -> Result<(), GpuError> {
    ex.clear(content, texel_bytes(chunk.width, chunk.height));
    let normal = |ex: &mut Exec<'_>, src: Src<'_, '_>| {
        ex.composite(
            StudioBlendMode::Normal,
            Opac::plain(1.0),
            mask,
            None,
            Pass::Normal,
            region,
            chunk,
            content,
            src,
        )
    };
    match prep.node(layer.input)? {
        PNode::Group { isolated: true, .. } => {
            let below = content.saturating_add(1);
            ex.clear(below, texel_bytes(chunk.width, chunk.height));
            eval_group(ex, prep, layer.input, chunk, below)?;
            normal(ex, Src::Chunk)
        }
        PNode::Tile { planes, rect } => normal(ex, Src::Tile(planes, *rect)),
        PNode::Transformed { planes, prep: t } => normal(ex, Src::Transformed(planes, t)),
        PNode::Solid { rgba } if rgba[3] <= 0.0 => Ok(()),
        PNode::Solid { rgba } => normal(ex, Src::Solid(*rgba)),
        PNode::Group {
            isolated: false, ..
        }
        | PNode::Unused => Err(unsupported("plan_shape")),
    }
}

/// A layer with a layer blur (pigment `blur_math`): the shape content one level below, blurred
/// horizontally into the scratch plane and vertically back into that level over the whole frame,
/// then composited with the layer's mode and `opacity * fill` over the layer bounds grown by the
/// support (the frame margin carries the support, so every kept pixel reads real content).
fn eval_blur_layer(
    ex: &mut Exec<'_>,
    prep: &Prepared<'_>,
    layer: &PLayer<'_>,
    blur: &BlurGpu,
    chunk: Rect,
    level: usize,
) -> Result<(), GpuError> {
    if layer.opacity <= 0.0 {
        return Ok(());
    }
    let Some(bounds) = prep.bound(layer.input) else {
        return Ok(());
    };
    let Some(content_region) = intersect(bounds, chunk)? else {
        return Ok(());
    };
    let Some(region) = intersect(grow_rect(bounds, blur.support)?, chunk)? else {
        return Ok(());
    };
    let content = level.saturating_add(1);
    render_shape(ex, prep, layer, content_region, chunk, content)?;
    ex.blur_pass(BlurOp::Horizontal, blur, 0.0, chunk, BlurIo::LevelToA(content))?;
    ex.blur_pass(BlurOp::Vertical, blur, 0.0, chunk, BlurIo::AToLevel(content))?;
    ex.composite(
        layer.mode,
        layer.opac(),
        None,
        None,
        Pass::Normal,
        region,
        chunk,
        level,
        Src::Chunk,
    )
}

/// A layer with filters (pigment `filter_math`): the content WITHOUT the layer mask one level
/// below, run through the filter passes over the whole frame (the frame margin carries the sum of
/// the stage reaches, so every kept pixel reads real content), then composited with the layer's
/// mask, Blend-If, mode and `opacity * fill` over the content bounds grown by the chain growth.
/// The composite covers only the mask plane (render-cpu does the same: the part of the filtered
/// result beyond the plane is not composited).
fn eval_filter_layer(
    ex: &mut Exec<'_>,
    prep: &Prepared<'_>,
    layer: &PLayer<'_>,
    filter: &FilterGpu,
    chunk: Rect,
    level: usize,
) -> Result<(), GpuError> {
    if layer.opacity <= 0.0 {
        return Ok(());
    }
    let Some(bounds) = prep.bound(layer.input) else {
        return Ok(());
    };
    let Some(content_region) = intersect(bounds, chunk)? else {
        return Ok(());
    };
    let Some(mut region) = intersect(grow_rect(bounds, filter.growth)?, chunk)? else {
        return Ok(());
    };
    if let Some(mask) = layer.mask.as_ref() {
        region = match intersect(region, mask.bounds)? {
            Some(r) => r,
            None => return Ok(()),
        };
    }
    let content = level.saturating_add(1);
    render_content(ex, prep, layer, None, content_region, chunk, content)?;
    for stage in &filter.stages {
        ex.check()?;
        ex.filter_stage(stage, chunk, content)?;
    }
    ex.composite(
        layer.mode,
        layer.opac(),
        layer.mask.as_ref(),
        layer.blend_if.as_ref(),
        Pass::Normal,
        region,
        chunk,
        level,
        Src::Chunk,
    )
}

/// A layer with a smart-filter stack (lane SMB-PS-SMART-FILTER-WIRING): the content WITHOUT the
/// layer mask over the stack plane (content bounds inside the extent; transparent beyond, as
/// pigment's `SmartFilterStack::render` sees it) one level below, then per enabled entry: copy to
/// two levels below, the entry's filter passes there, and the mix pass `lerp(running, filtered,
/// opacity)` back into the running level; finally composited with the layer's mask, Blend-If,
/// mode and opacity over the plane (the stack output is the plane, as on render-cpu).
fn eval_smart_layer(
    ex: &mut Exec<'_>,
    prep: &Prepared<'_>,
    layer: &PLayer<'_>,
    smart: &SmartGpu,
    chunk: Rect,
    level: usize,
) -> Result<(), GpuError> {
    if layer.opacity <= 0.0 {
        return Ok(());
    }
    let Some(plane) = smart.plane else {
        return Ok(());
    };
    let Some(content_region) = intersect(plane, chunk)? else {
        return Ok(());
    };
    let mut region = content_region;
    if let Some(mask) = layer.mask.as_ref() {
        region = match intersect(region, mask.bounds)? {
            Some(r) => r,
            None => return Ok(()),
        };
    }
    let running = level.saturating_add(1);
    let copy = level.saturating_add(2);
    render_content(ex, prep, layer, None, content_region, chunk, running)?;
    let rel = |v: i64, o: i64| i32::try_from(v.checked_sub(o).ok_or_else(overflow)?).map_err(|_| unsupported("filter_coordinates"));
    let (cx1, cy1) = end(content_region)?;
    let rect = (
        rel(content_region.x, chunk.x)?,
        rel(content_region.y, chunk.y)?,
        u32::try_from(rel(cx1, chunk.x)?).map_err(|_| overflow())?,
        u32::try_from(rel(cy1, chunk.y)?).map_err(|_| overflow())?,
    );
    let stack_mask = smart.stack_mask.and_then(|i| smart.masks.get(i));
    for (chain, opacity, mask) in &smart.entries {
        ex.check()?;
        ex.filter_pass(FilterOp::Copy, chunk, FilterBuf::Level(running), FilterBuf::Level(copy), FilterBuf::A)?;
        for stage in &chain.stages {
            ex.filter_stage(stage, chunk, copy)?;
        }
        // The mix also keeps nothing beyond the plane (render-cpu stores the plane only). With a
        // mask (lane SMB-PS-ADJUST-STYLES-3) the weight is per texel (`smart_mask`).
        let entry_mask = mask.and_then(|i| smart.masks.get(i));
        if entry_mask.is_some() || stack_mask.is_some() {
            ex.upload_weights(chunk, *opacity, entry_mask, stack_mask)?;
            ex.filter_mix_masked(chunk, rect, FilterBuf::Level(running), FilterBuf::A, FilterBuf::Level(copy))?;
        } else {
            ex.filter_pass(FilterOp::Mix { t: *opacity, rect }, chunk, FilterBuf::Level(running), FilterBuf::A, FilterBuf::Level(copy))?;
        }
        ex.filter_pass(FilterOp::Copy, chunk, FilterBuf::A, FilterBuf::Level(running), FilterBuf::B)?;
    }
    ex.composite(
        layer.mode,
        layer.opac(),
        layer.mask.as_ref(),
        layer.blend_if.as_ref(),
        Pass::Normal,
        region,
        chunk,
        level,
        Src::Chunk,
    )
}

/// Backdrop blur of `layer` before it composites: the running backdrop `levels[level]` is blurred
/// (horizontal into scratch A, vertical into scratch B) and mixed into the backdrop inside the
/// layer's shape (rendered one level below) with `k = shape alpha * layer opacity`.
fn eval_backdrop_blur(
    ex: &mut Exec<'_>,
    prep: &Prepared<'_>,
    layer: &PLayer<'_>,
    chunk: Rect,
    level: usize,
) -> Result<(), GpuError> {
    let Some(blur) = layer.blur.as_ref() else {
        return Ok(());
    };
    if layer.layer_opacity <= 0.0 {
        return Ok(());
    }
    let Some(region) = prep
        .bound(layer.input)
        .map(|b| intersect(b, chunk))
        .transpose()?
        .flatten()
    else {
        return Ok(());
    };
    ex.check()?;
    ex.blur_pass(BlurOp::Horizontal, blur, 0.0, chunk, BlurIo::LevelToA(level))?;
    ex.blur_pass(BlurOp::Vertical, blur, 0.0, chunk, BlurIo::AToB)?;
    let content = level.saturating_add(1);
    render_shape(ex, prep, layer, region, chunk, content)?;
    ex.blur_pass(
        BlurOp::Mix,
        blur,
        layer.layer_opacity,
        chunk,
        BlurIo::MixInto { level, shape: content },
    )
}

/// Buffers of one blur pass.
#[derive(Clone, Copy)]
enum BlurIo {
    /// `levels[l]` (straight) -> scratch A (premultiplied).
    LevelToA(usize),
    /// Scratch A -> `levels[l]` (straight).
    AToLevel(usize),
    /// Scratch A -> scratch B (straight).
    AToB,
    /// Scratch B mixed into `levels[level]` inside the shape alpha of `levels[shape]`.
    MixInto { level: usize, shape: usize },
}

/// A buffer of the filter passes.
#[derive(Clone, Copy)]
enum FilterBuf {
    Level(usize),
    A,
    B,
}

impl Exec<'_> {
    /// One stage of a filter chain over the whole frame `chunk`, in place on `levels[content]`.
    fn filter_stage(
        &mut self,
        stage: &FilterStageGpu,
        chunk: Rect,
        content: usize,
    ) -> Result<(), GpuError> {
        let level = FilterBuf::Level(content);
        match stage {
            FilterStageGpu::Gaussian(b) => {
                self.blur_pass(BlurOp::Horizontal, b, 0.0, chunk, BlurIo::LevelToA(content))?;
                self.blur_pass(BlurOp::Vertical, b, 0.0, chunk, BlurIo::AToLevel(content))
            }
            FilterStageGpu::Unsharp { blur, amount, threshold } => {
                self.blur_pass(BlurOp::Horizontal, blur, 0.0, chunk, BlurIo::LevelToA(content))?;
                self.blur_pass(BlurOp::Vertical, blur, 0.0, chunk, BlurIo::AToB)?;
                let op = FilterOp::Unsharp { amount: *amount, threshold: *threshold };
                self.filter_pass(op, chunk, level, FilterBuf::A, FilterBuf::B)?;
                self.filter_pass(FilterOp::Copy, chunk, FilterBuf::A, level, FilterBuf::B)
            }
            FilterStageGpu::HighPass { blur } => {
                self.blur_pass(BlurOp::Horizontal, blur, 0.0, chunk, BlurIo::LevelToA(content))?;
                self.blur_pass(BlurOp::Vertical, blur, 0.0, chunk, BlurIo::AToB)?;
                self.filter_pass(FilterOp::HighPass, chunk, level, FilterBuf::A, FilterBuf::B)?;
                self.filter_pass(FilterOp::Copy, chunk, FilterBuf::A, level, FilterBuf::B)
            }
            FilterStageGpu::Median { radius } => {
                let op = FilterOp::Median { radius: *radius };
                self.filter_pass(op, chunk, level, FilterBuf::A, FilterBuf::B)?;
                self.filter_pass(FilterOp::Copy, chunk, FilterBuf::A, level, FilterBuf::B)
            }
            FilterStageGpu::Noise { amount, gaussian, mono, seed } => {
                // The hash takes plan coordinates; the shader keeps them in i32.
                let end_x = chunk.x.checked_add(i64::from(chunk.width)).ok_or_else(overflow)?;
                let end_y = chunk.y.checked_add(i64::from(chunk.height)).ok_or_else(overflow)?;
                let origin = match (i32::try_from(chunk.x), i32::try_from(chunk.y)) {
                    (Ok(x), Ok(y)) if i32::try_from(end_x).is_ok() && i32::try_from(end_y).is_ok() => (x, y),
                    _ => return Err(unsupported("filter_coordinates")),
                };
                let op = FilterOp::Noise {
                    amount: *amount,
                    gaussian: *gaussian,
                    mono: *mono,
                    seed: *seed,
                    origin,
                };
                self.filter_pass(op, chunk, level, FilterBuf::A, FilterBuf::B)?;
                self.filter_pass(FilterOp::Copy, chunk, FilterBuf::A, level, FilterBuf::B)
            }
        }
    }

    /// One pass of `filter.wgsl` over the whole frame `chunk`; `src`, `dst` and `aux` are three
    /// distinct buffers (the output never aliases a read-only binding).
    fn filter_pass(
        &mut self,
        op: FilterOp,
        chunk: Rect,
        src: FilterBuf,
        dst: FilterBuf,
        aux: FilterBuf,
    ) -> Result<(), GpuError> {
        let ctx = self.ctx;
        let Some(bb) = self.blur.as_ref() else {
            return Err(unsupported("plan_shape"));
        };
        let pick = |b: FilterBuf| match b {
            FilterBuf::Level(l) => self.levels.get(l).ok_or_else(|| unsupported("plan_shape")),
            FilterBuf::A => Ok(&bb.a),
            FilterBuf::B => Ok(&bb.b),
        };
        let (src, dst, aux) = (pick(src)?, pick(dst)?, pick(aux)?);
        let bytes = filter_params_bytes(op, chunk.width, chunk.height, self.extended);
        ctx.queue.write_buffer(&bb.params, 0, &bytes);
        let bind = ctx.kernel.bind_blur(&ctx.device, &bb.params, src, dst, aux);
        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("hsk-studio-render-gpu filter pass"),
            });
        ctx.kernel
            .dispatch_filter(&mut encoder, &bind, chunk.width, chunk.height);
        ctx.queue.submit(Some(encoder.finish()));
        self.stats.dispatches = self.stats.dispatches.saturating_add(1);
        Ok(())
    }

    /// One pass of `blur.wgsl` over the whole frame `chunk`.
    fn blur_pass(
        &mut self,
        op: BlurOp,
        blur: &BlurGpu,
        opacity: f32,
        chunk: Rect,
        io: BlurIo,
    ) -> Result<(), GpuError> {
        let ctx = self.ctx;
        let Some(bb) = self.blur.as_ref() else {
            return Err(unsupported("plan_shape"));
        };
        let level = |l: usize| self.levels.get(l).ok_or_else(|| unsupported("plan_shape"));
        let (src, dst, aux) = match io {
            BlurIo::LevelToA(l) => (level(l)?, &bb.a, &bb.b),
            BlurIo::AToLevel(l) => (&bb.a, level(l)?, &bb.b),
            BlurIo::AToB => (&bb.a, &bb.b, level(0)?),
            BlurIo::MixInto { level: l, shape } => (&bb.b, level(l)?, level(shape)?),
        };
        let bytes = blur_params_bytes(op, chunk.width, chunk.height, &blur.taps, opacity, self.extended)
            .ok_or_else(|| unsupported("blur_parameter"))?;
        ctx.queue.write_buffer(&bb.params, 0, &bytes);
        let bind = ctx.kernel.bind_blur(&ctx.device, &bb.params, src, dst, aux);
        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("hsk-studio-render-gpu blur pass"),
            });
        ctx.kernel
            .dispatch_blur(&mut encoder, &bind, chunk.width, chunk.height);
        ctx.queue.submit(Some(encoder.finish()));
        self.stats.dispatches = self.stats.dispatches.saturating_add(1);
        Ok(())
    }
}

/// Sum of byte counts; an overflow is the typed `Overflow`, not a wrapped size.
fn sum_bytes(parts: &[u64]) -> Result<u64, GpuError> {
    parts
        .iter()
        .try_fold(0u64, |acc, part| acc.checked_add(*part))
        .ok_or_else(overflow)
}
fn mul_bytes(a: u64, b: u64) -> Result<u64, GpuError> {
    a.checked_mul(b).ok_or_else(overflow)
}

#[allow(clippy::too_many_arguments)]
fn run(
    ctx: &GpuContext,
    residency: &mut Residency,
    options: &DeviceOptions,
    prepared: &Prepared<'_>,
    plan: &RenderPlan<NoVector>,
    request: &ExecuteRequest,
    cancel: &CancellationToken,
    sink: &mut dyn TileSink,
    stats: &mut GpuStats,
    texture: Option<&TextureOut<'_>>,
) -> Result<u64, GpuError> {
    ctx.check_alive()?;
    let _ = ctx.take_fault();

    // ---- bounded sizing: the chunk shrinks until every device buffer fits both budgets ----
    let extent = plan.target.extent;
    let (extent_x1, extent_y1) = end(extent)?;
    let level_count = prepared.need[prepared.root].saturating_add(1);
    let levels_u64 = u64::try_from(level_count).map_err(|_| overflow())?;
    // Envelope buffers: the parameter block and the curve table (a placeholder binding without curves).
    let lut_bytes = u64::try_from(prepared.lut_floats)
        .ok()
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(overflow)?
        .max(MIN_LUT_BYTES);
    let ext_fixed = if prepared.has_ext {
        sum_bytes(&[EXT_BYTES, lut_bytes])?
    } else {
        0
    };
    // A plan with layer effects evaluates every chunk as a FRAME: the chunk grown by `margin`
    // pixels on each side (the effects read that far), so the level, source, mask and matte
    // buffers are frame-sized; only the central chunk is read back.
    let fx_on = prepared.has_fx;
    let margin = prepared.frame_margin;
    let margin2 = margin.checked_mul(2).ok_or_else(overflow)?;
    let masked = prepared.has_masks || fx_on;
    // Layer transforms: per frame texel one sample site, plus the parameter block and a source
    // window that holds the largest transformed tile.
    let transform_on = !prepared.extra.is_empty();
    let window_bytes = if transform_on {
        mul_bytes(prepared.transform_window_texels.max(1), TEXEL_BYTES)?
    } else {
        0
    };
    let blur_on = prepared.has_blur;
    // Knockout buffers (lane SMB-PS-ADJUST-STYLES-3): S, the shape, the snapshots and the deep
    // planes per frame texel, plus the `Ko` block and the placeholder.
    let (ko_per_texel, ko_fixed) = knockout::ko_bytes(&prepared.ko, level_count)?;
    let per_texel = sum_bytes(&[
        ko_per_texel,
        mul_bytes(
            TEXEL_BYTES,
            sum_bytes(&[
                levels_u64,
                1,
                u64::from(prepared.has_tiles),
                if blur_on { 2 } else { 0 },
            ])?,
        )?,
        if masked { MASK_TEXEL_BYTES } else { 0 },
        if fx_on { FX_PLANE_TEXEL_BYTES } else { 0 },
        if transform_on { SITE_BYTES } else { 0 },
    ])?;
    let fixed = sum_bytes(&[
        ko_fixed,
        PARAMS_BYTES,
        if prepared.has_tiles { 0 } else { TEXEL_BYTES },
        if masked { 0 } else { MASK_TEXEL_BYTES },
        ext_fixed,
        if fx_on { FXP_BYTES } else { 0 },
        if blur_on { BLURP_BYTES } else { 0 },
        if transform_on {
            sum_bytes(&[RESAMPLE_PARAMS_BYTES, window_bytes])?
        } else {
            0
        },
    ])?;
    let limit = request.max_working_bytes.min(residency.budget_bytes());
    let by_budget = limit
        .saturating_sub(fixed)
        .checked_div(per_texel)
        .unwrap_or(0);
    let by_buffer = ctx.max_buffer_bytes.checked_div(TEXEL_BYTES).unwrap_or(0);
    let max_texels = by_budget.min(by_buffer);
    if max_texels < MIN_CHUNK_TEXELS {
        return Err(GpuError::BudgetExceeded {
            requested: fixed.saturating_add(per_texel.saturating_mul(MIN_CHUNK_TEXELS)),
            budget: limit,
        });
    }
    let mut frame_side = u32::try_from(max_texels.isqrt())
        .unwrap_or(u32::MAX)
        .min(options.chunk_side.saturating_add(margin2))
        .max(1);
    // One effect pass costs `window x frame pixels`; the frame shrinks until a pass stays under the
    // cost bound (a pass that runs for seconds can hit the driver's watchdog).
    let mut cost_limited = false;
    if fx_on || blur_on {
        // A blur pass costs `taps x frame pixels`, like an effect pass costs its window.
        let pass_ops = prepared.fx_window_ops.max(prepared.blur_taps).max(1);
        let by_cost = u32::try_from(
            FX_PASS_MAX_OPS
                .checked_div(pass_ops)
                .unwrap_or(0)
                .isqrt(),
        )
        .unwrap_or(u32::MAX);
        if by_cost < frame_side {
            frame_side = by_cost;
            cost_limited = true;
        }
    }
    let side = frame_side.saturating_sub(margin2);
    let min_side = if fx_on || blur_on { MIN_FX_SIDE } else { 1 };
    if side < min_side {
        if cost_limited {
            return Err(unsupported("effect_cost"));
        }
        let frame_min = u64::from(min_side).saturating_add(u64::from(margin2));
        return Err(GpuError::BudgetExceeded {
            requested: fixed
                .saturating_add(per_texel.saturating_mul(frame_min.saturating_mul(frame_min))),
            budget: limit,
        });
    }
    let chunk_w = side.min(extent.width);
    let chunk_h = side.min(extent.height);
    let frame_w = chunk_w.checked_add(margin2).ok_or_else(overflow)?;
    let frame_h = chunk_h.checked_add(margin2).ok_or_else(overflow)?;
    let frame_bytes = texel_bytes(frame_w, frame_h);
    let chunk_bytes = texel_bytes(chunk_w, chunk_h);
    let frame_texels = mul_bytes(u64::from(frame_w), u64::from(frame_h))?;
    let src_bytes = if prepared.has_tiles {
        frame_bytes
    } else {
        TEXEL_BYTES
    };
    let mask_bytes = if masked {
        mul_bytes(frame_texels, MASK_TEXEL_BYTES)?
    } else {
        MASK_TEXEL_BYTES
    };
    // One f32 matte plane of the frame (three of them exist when a layer has effects).
    let plane_bytes = mul_bytes(frame_texels, 4)?;
    stats.chunk_width = chunk_w;
    stats.chunk_height = chunk_h;
    stats.levels = u32::try_from(level_count).map_err(|_| overflow())?;

    // ---- charge residency first, then create the buffers ----
    for level in 0..level_count {
        let level = u32::try_from(level).map_err(|_| overflow())?;
        residency.reserve(ResidentKey::Level(level), frame_bytes)?;
    }
    for (name, bytes) in [
        ("staging", chunk_bytes),
        ("src", src_bytes),
        ("mask", mask_bytes),
        ("params", PARAMS_BYTES),
    ] {
        residency.reserve(ResidentKey::Scratch(name), bytes)?;
    }
    if prepared.has_ext {
        residency.reserve(ResidentKey::Scratch("ext"), EXT_BYTES)?;
        residency.reserve(ResidentKey::Scratch("lut"), lut_bytes)?;
    }
    let fx_total = if fx_on {
        residency.reserve(ResidentKey::Scratch("fx_params"), FXP_BYTES)?;
        for name in ["fx_plane0", "fx_plane1", "fx_plane2"] {
            residency.reserve(ResidentKey::Scratch(name), plane_bytes)?;
        }
        sum_bytes(&[FXP_BYTES, mul_bytes(plane_bytes, 3)?])?
    } else {
        0
    };
    let blur_total = if blur_on {
        residency.reserve(ResidentKey::Scratch("blur_params"), BLURP_BYTES)?;
        residency.reserve(ResidentKey::Scratch("blur_a"), frame_bytes)?;
        residency.reserve(ResidentKey::Scratch("blur_b"), frame_bytes)?;
        sum_bytes(&[BLURP_BYTES, mul_bytes(frame_bytes, 2)?])?
    } else {
        0
    };
    let sites_bytes = mul_bytes(frame_texels, SITE_BYTES)?;
    let resample_total = if transform_on {
        residency.reserve(ResidentKey::Scratch("resample_params"), RESAMPLE_PARAMS_BYTES)?;
        residency.reserve(ResidentKey::Scratch("resample_sites"), sites_bytes)?;
        residency.reserve(ResidentKey::Scratch("resample_window"), window_bytes)?;
        sum_bytes(&[RESAMPLE_PARAMS_BYTES, sites_bytes, window_bytes])?
    } else {
        0
    };
    let ko_snaps = prepared.ko.snapshot_groups.len();
    let ko_planes = if prepared.ko.deep_groups.is_empty() {
        0
    } else {
        level_count.saturating_sub(1)
    };
    let ko_total = if prepared.ko.any {
        let snaps = mul_bytes(frame_bytes, u64::try_from(ko_snaps).map_err(|_| overflow())?)?;
        let planes = mul_bytes(plane_bytes, u64::try_from(ko_planes).map_err(|_| overflow())?)?;
        residency.reserve(ResidentKey::Scratch("ko_params"), crate::kernel::KO_PARAMS_BYTES)?;
        residency.reserve(ResidentKey::Scratch("ko_none"), 16)?;
        residency.reserve(ResidentKey::Scratch("ko_s"), frame_bytes)?;
        residency.reserve(ResidentKey::Scratch("ko_k"), plane_bytes)?;
        if snaps > 0 {
            residency.reserve(ResidentKey::Scratch("ko_snapshots"), snaps)?;
        }
        if planes > 0 {
            residency.reserve(ResidentKey::Scratch("ko_planes"), planes)?;
        }
        sum_bytes(&[crate::kernel::KO_PARAMS_BYTES, 16, frame_bytes, plane_bytes, snaps, planes])?
    } else {
        0
    };
    let total = sum_bytes(&[
        ko_total,
        mul_bytes(frame_bytes, levels_u64)?,
        chunk_bytes,
        src_bytes,
        mask_bytes,
        PARAMS_BYTES,
        ext_fixed,
        fx_total,
        resample_total,
        blur_total,
    ])?;
    let device = &ctx.device;
    let queue = &ctx.queue;
    let (
        levels,
        staging,
        src,
        mask,
        params,
        binds_tile,
        binds_chunk,
        ext_buffers,
        fx_buffers,
        resample_buffers,
        blur_buffers,
        ko_buffers,
    ) = scoped(device, total, limit, || {
            let buffer = |label, size, usage| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size,
                    usage,
                    mapped_at_creation: false,
                })
            };
            let levels: Vec<wgpu::Buffer> = (0..level_count)
                .map(|_| {
                    buffer(
                        "hsk-studio-render-gpu level",
                        frame_bytes,
                        wgpu::BufferUsages::STORAGE
                            | wgpu::BufferUsages::COPY_SRC
                            | wgpu::BufferUsages::COPY_DST,
                    )
                })
                .collect();
            let staging = buffer(
                "hsk-studio-render-gpu staging",
                chunk_bytes,
                wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            );
            let src = buffer(
                "hsk-studio-render-gpu src",
                src_bytes,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            );
            let mask = buffer(
                "hsk-studio-render-gpu mask",
                mask_bytes,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            );
            let params = buffer(
                "hsk-studio-render-gpu params",
                PARAMS_BYTES,
                wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            );
            let binds_tile: Vec<wgpu::BindGroup> = levels
                .iter()
                .map(|dst| ctx.kernel.bind(device, &params, &src, &mask, dst))
                .collect();
            let binds_chunk: Vec<wgpu::BindGroup> = levels
                .iter()
                .zip(levels.iter().skip(1))
                .map(|(dst, below)| ctx.kernel.bind(device, &params, below, &mask, dst))
                .collect();
            let fx_buffers = fx_on.then(|| FxBuffers {
                params: buffer(
                    "hsk-studio-render-gpu effect params",
                    FXP_BYTES,
                    wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                ),
                planes: [0, 1, 2].map(|_| {
                    buffer(
                        "hsk-studio-render-gpu effect plane",
                        plane_bytes,
                        wgpu::BufferUsages::STORAGE,
                    )
                }),
            });
            let ext_buffers = prepared.has_ext.then(|| {
                let ext = buffer(
                    "hsk-studio-render-gpu envelope params",
                    EXT_BYTES,
                    wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                );
                let lut = buffer(
                    "hsk-studio-render-gpu curve table",
                    lut_bytes,
                    wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                );
                let binds_tile = levels
                    .iter()
                    .map(|dst| {
                        ctx.kernel
                            .bind_ext(device, &params, &src, &mask, dst, &ext, &lut)
                    })
                    .collect();
                let binds_chunk = levels
                    .iter()
                    .zip(levels.iter().skip(1))
                    .map(|(dst, below)| {
                        ctx.kernel
                            .bind_ext(device, &params, below, &mask, dst, &ext, &lut)
                    })
                    .collect();
                ExtBuffers {
                    ext,
                    lut,
                    binds_tile,
                    binds_chunk,
                }
            });
            let blur_buffers = blur_on.then(|| BlurBuffers {
                params: buffer(
                    "hsk-studio-render-gpu blur params",
                    BLURP_BYTES,
                    wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                ),
                a: buffer("hsk-studio-render-gpu blur a", frame_bytes, wgpu::BufferUsages::STORAGE),
                b: buffer("hsk-studio-render-gpu blur b", frame_bytes, wgpu::BufferUsages::STORAGE),
            });
            let resample_buffers = transform_on.then(|| {
                let params = buffer(
                    "hsk-studio-render-gpu resample params",
                    RESAMPLE_PARAMS_BYTES,
                    wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                );
                let sites = buffer(
                    "hsk-studio-render-gpu resample sites",
                    sites_bytes,
                    wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                );
                let window = buffer(
                    "hsk-studio-render-gpu resample window",
                    window_bytes,
                    wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                );
                let bind = ctx
                    .kernel
                    .bind_resample(device, &params, &sites, &window, &src);
                ResampleBuffers {
                    params,
                    sites,
                    window,
                    bind,
                }
            });
            let ko_buffers = prepared.ko.any.then(|| {
                let storage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
                knockout::KoBuffers {
                    params: buffer(
                        "hsk-studio-render-gpu knockout params",
                        crate::kernel::KO_PARAMS_BYTES,
                        wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    ),
                    s: buffer("hsk-studio-render-gpu knockout s", frame_bytes, storage),
                    k: buffer("hsk-studio-render-gpu knockout shape", plane_bytes, wgpu::BufferUsages::STORAGE),
                    none: buffer("hsk-studio-render-gpu knockout none", 16, wgpu::BufferUsages::STORAGE),
                    snapshots: (0..ko_snaps)
                        .map(|_| {
                            buffer(
                                "hsk-studio-render-gpu knockout snapshot",
                                frame_bytes,
                                wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                            )
                        })
                        .collect(),
                    planes: (0..ko_planes)
                        .map(|_| buffer("hsk-studio-render-gpu knockout plane", plane_bytes, storage))
                        .collect(),
                    stack: Vec::new(),
                }
            });
            (
                levels,
                staging,
                src,
                mask,
                params,
                binds_tile,
                binds_chunk,
                ext_buffers,
                fx_buffers,
                resample_buffers,
                blur_buffers,
                ko_buffers,
            )
        })?;

    let clip = prepared.output.display == Some(DisplayTransform::Clip);
    let view_transform = prepared.output.display.and_then(|d| d.view());
    let extended = prepared.output.is_extended();
    let mut ex = Exec {
        ctx,
        cancel,
        deadline: request.deadline,
        levels,
        src,
        mask,
        params,
        binds_tile,
        binds_chunk,
        ext: ext_buffers,
        fx: fx_buffers,
        resample: resample_buffers,
        blur: blur_buffers,
        kernels: &prepared.fx_kernels,
        patterns: &prepared.patterns,
        feathered: &prepared.feathered,
        upload: Vec::new(),
        mask_upload: Vec::new(),
        stats,
        extended: prepared.output.is_extended(),
        eight_bit: prepared.output.is_eight_bit(),
        ko: ko_buffers,
    };
    // A leaf root is one Normal, opacity-1 layer over transparent black.
    let root_layer = PLayer {
        input: prepared.root,
        clipped: false,
        mode: StudioBlendMode::Normal,
        opacity: 1.0,
        layer_opacity: 1.0,
        fill: 1.0,
        mask: None,
        blend_if: None,
        adjust: None,
        fx: None,
        blur: None,
        filter: None,
        smart: None,
        knockout: Knockout::None,
        group: prepared.root,
    };
    let mut colour: Vec<u8> = Vec::new();
    let mut alpha: Vec<u8> = Vec::new();
    let mut chunks = 0_u64;
    let side_w = i64::from(chunk_w);
    let side_h = i64::from(chunk_h);
    let mut cy = extent.y;
    while cy < extent_y1 {
        let mut cx = extent.x;
        while cx < extent_x1 {
            ex.check()?;
            let rect = Rect {
                x: cx,
                y: cy,
                width: u32::try_from(side_w.min(extent_x1.saturating_sub(cx)))
                    .map_err(|_| overflow())?,
                height: u32::try_from(side_h.min(extent_y1.saturating_sub(cy)))
                    .map_err(|_| overflow())?,
            };
            let count = texel_bytes(rect.width, rect.height);
            // The frame: the chunk plus the effect margin (the chunk itself without effects).
            let frame = grow_rect(rect, margin)?;
            // Every device call of this chunk (passes, submits, read-back) runs under the
            // out-of-memory, internal and validation scopes: an error raised there is the typed
            // `submit_fault` with its detail, not the uncaptured latch (`device_fault`). The scopes
            // are popped below, or by `Drop` on an early return or an unwind (lane
            // SMB-RENDER-GPU-SUBMIT-FAULT).
            let scopes = ErrorScopes::push(device);
            let injected = ctx.take_injected();
            if injected.panic {
                panic!("injected execution panic (test-faults)");
            }

            // Transparent backdrop for this frame, then the root group (transparent backdrop, so
            // isolated and not are the same here) or the single leaf.
            ex.clear(0, texel_bytes(frame.width, frame.height));
            if matches!(prepared.nodes[prepared.root], PNode::Group { .. }) {
                eval_group(&mut ex, prepared, prepared.root, frame, 0)?;
            } else {
                eval_layer(&mut ex, prepared, &root_layer, frame, 0)?;
            }
            // The passes of this chunk are submitted: their errors are attributed to the chunk;
            // the output step gets its own scopes and label.
            if let Some(captured) = scopes.finish() {
                return Err(submit_fault(ctx, "submit_fault", CHUNK_LABEL, captured));
            }
            let scopes = ErrorScopes::push(device);
            // Shared-device output: the chunk goes straight into the caller's texture, no read-back.
            if let Some(out) = texture {
                out.write_chunk_in(&ex.levels[0], rect, frame.width, margin, extended)?;
                if let Some(captured) = scopes.finish() {
                    return Err(submit_fault(ctx, "submit_fault", OUTPUT_LABEL, captured));
                }
                chunks = chunks.saturating_add(1);
                cx = cx.checked_add(side_w).ok_or_else(overflow)?;
                continue;
            }

            // Read the finished chunk back (the central rows of the frame when it has a margin).
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("hsk-studio-render-gpu readback"),
            });
            if margin == 0 {
                encoder.copy_buffer_to_buffer(&ex.levels[0], 0, &staging, 0, count);
            } else {
                let row_bytes = texel_bytes(rect.width, 1);
                for row in 0..rect.height {
                    let first = u64::from(row)
                        .checked_add(u64::from(margin))
                        .and_then(|r| r.checked_mul(u64::from(frame.width)))
                        .and_then(|r| r.checked_add(u64::from(margin)))
                        .and_then(|r| r.checked_mul(TEXEL_BYTES))
                        .ok_or_else(overflow)?;
                    let to = u64::from(row).checked_mul(row_bytes).ok_or_else(overflow)?;
                    encoder.copy_buffer_to_buffer(&ex.levels[0], first, &staging, to, row_bytes);
                }
            }
            if injected.copy_out_of_bounds {
                let past_end = staging.size().saturating_add(16);
                encoder.copy_buffer_to_buffer(&ex.levels[0], 0, &staging, 0, past_end);
            }
            queue.submit(Some(encoder.finish()));
            if injected.staging_destroyed {
                staging.destroy();
            }
            let (tx, rx) = std::sync::mpsc::channel();
            staging
                .slice(0..count)
                .map_async(wgpu::MapMode::Read, move |outcome| {
                    let _ = tx.send(outcome);
                });
            let wait = request.deadline.map_or(DEFAULT_POLL_LIMIT, |d| {
                d.saturating_duration_since(Instant::now())
                    .max(Duration::from_millis(1))
            });
            let polled = device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(wait),
            });
            ctx.check_alive()?;
            if polled.is_err() {
                return Err(if request.deadline.is_some_and(|d| Instant::now() >= d) {
                    GpuError::Backend(BackendError::DeadlineExceeded)
                } else {
                    GpuError::Gpu("device_timeout")
                });
            }
            if !matches!(rx.try_recv(), Ok(Ok(()))) {
                // The scoped error (if any) is the cause of the failed mapping.
                let captured = scopes.finish().unwrap_or((
                    "map",
                    String::from("the read-back buffer could not be mapped"),
                ));
                return Err(submit_fault(ctx, "readback_map_failed", READBACK_LABEL, captured));
            }
            colour.clear();
            alpha.clear();
            {
                let view = staging.slice(0..count).get_mapped_range();
                for texel in view.chunks_exact(TEXEL_BYTES as usize) {
                    let mut values = [0.0_f32; 4];
                    for (value, word) in values.iter_mut().zip(texel.chunks_exact(4)) {
                        *value = f32::from_ne_bytes([word[0], word[1], word[2], word[3]]);
                    }
                    if extended
                        && (!values.iter().all(|v| v.is_finite()) || values[3] > 1.5)
                    {
                        // The kernel's overflow marker (or a non-finite texel): the typed refusal
                        // render-cpu returns for a non-finite extended composite.
                        return Err(unsupported(SAMPLE_RANGE_OPERATOR));
                    }
                    if clip {
                        // Stage 6a (output stage `display: Some(Clip)`), on the host.
                        for value in &mut values[..3] {
                            *value = value.clamp(0.0, 1.0);
                        }
                    }
                    if let Some(id) = view_transform {
                        // Stage 6a view transform (lane SMB-PRISM-VIEW-TRANSFORM), on the host:
                        // Prism's per-pixel view of the read-back colour; a non-finite composite
                        // (binary32 overflow on the device) is refused typed.
                        let out = id
                            .apply([values[0], values[1], values[2]])
                            .map_err(|_| unsupported(SAMPLE_RANGE_OPERATOR))?;
                        values[..3].copy_from_slice(&out);
                    }
                    for value in &values[..3] {
                        colour.extend_from_slice(&value.to_le_bytes());
                    }
                    alpha.extend_from_slice(&values[3].to_le_bytes());
                }
            }
            staging.unmap();
            ex.stats.bytes_read_back = ex.stats.bytes_read_back.saturating_add(count);
            if let Some(captured) = scopes.finish() {
                return Err(submit_fault(ctx, "submit_fault", READBACK_LABEL, captured));
            }
            if ctx.take_fault() {
                return Err(GpuError::Gpu("device_fault"));
            }
            sink.accept(rect, &colour, &alpha)
                .map_err(|e| GpuError::Backend(BackendError::Sink(e)))?;
            chunks = chunks.saturating_add(1);
            cx = cx.checked_add(side_w).ok_or_else(overflow)?;
        }
        cy = cy.checked_add(side_h).ok_or_else(overflow)?;
    }
    if let Some(out) = texture {
        // Extended range on the shared texture: the pass flags overflowed texels instead of
        // displaying them; the flag is read once here (lane SMB-RENDER-GPU-EXTENDED-ENVELOPE).
        let read = out.finish(extended)?;
        ex.stats.bytes_read_back = ex.stats.bytes_read_back.saturating_add(read);
    }
    Ok(chunks)
}

/// Operation labels of execution faults.
const CHUNK_LABEL: &str = "hsk-studio-render-gpu chunk";
const READBACK_LABEL: &str = "hsk-studio-render-gpu readback";
const OUTPUT_LABEL: &str = "hsk-studio-render-gpu shared output";
const EXECUTE_LABEL: &str = "hsk-studio-render-gpu execute";

/// Record the detail of an execution fault on `ctx` and return its typed error. A scope timeout
/// keeps its own code (`submit_scope_timeout`).
fn submit_fault(
    ctx: &GpuContext,
    code: &'static str,
    label: &'static str,
    captured: crate::pipeline::Captured,
) -> GpuError {
    let (class, message) = captured;
    let code = if class == "timeout" && code == "submit_fault" {
        "submit_scope_timeout"
    } else {
        code
    };
    let fault = PipelineFault::new(code, label, class, &message);
    let error = fault.error();
    ctx.record_fault(fault);
    error
}

mod display;
/// Clip colour grade kernel (lane SMB-PR-SOURCE-MONITOR).
mod grade;
mod key;
mod knockout;
mod smart_mask;
pub use display::{DISPLAY_CLIP_EXACTNESS, DISPLAY_CLIP_OPERATOR, DisplayTransform};
pub(crate) use display::EXTENDED_FLAG;
use display::sample_ok;
#[cfg(test)]
mod refusal_keys;

#[cfg(feature = "shared-device")]
pub mod shared;
#[cfg(feature = "shared-device")]
use shared::TextureOut;
/// Without the `shared-device` feature there is no texture output; the type keeps the private
/// `run` signature identical in both builds.
#[cfg(not(feature = "shared-device"))]
struct TextureOut<'a>(std::convert::Infallible, std::marker::PhantomData<&'a ()>);
#[cfg(not(feature = "shared-device"))]
impl TextureOut<'_> {
    fn write_chunk_in(&self, _: &wgpu::Buffer, _: Rect, _: u32, _: u32, _: bool) -> Result<(), GpuError> {
        match self.0 {}
    }
    fn finish(&self, _: bool) -> Result<u64, GpuError> {
        match self.0 {}
    }
}

/// Defence in depth for Blend-If on a clip role: `admit_extended` refuses it first, so `prepare`
/// cannot be reached with one through the public API; the roles and keys it uses are tested here.
#[cfg(test)]
mod tests {
    use super::*;
    use hsk_studio_pigment::render_plan::{Layer, MaskInput};

    fn plain(input: u32) -> Layer {
        Layer {
            input: PlanNodeId(input),
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            fill_opacity: 1.0,
            mask: None,
        }
    }
    fn clipped(input: u32) -> Layer {
        Layer {
            mask: Some(MaskInput::clip_to_base()),
            ..plain(input)
        }
    }

    #[test]
    fn clip_role_blend_if_refusal_uses_pigments_roles_and_keys() {
        // [unclipped, base, clipped, clipped, unclipped]; index 5 is outside the group.
        let group = Group {
            isolated: true,
            layers: vec![plain(0), plain(1), clipped(2), clipped(3), plain(4)],
        };
        assert_eq!(
            [0, 1, 2, 3, 4, 5].map(|i| blend_if_clip_refusal(&group, i)),
            [
                None,
                Some("blend_if_clip_base"),
                Some("blend_if_clipped_layer"),
                Some("blend_if_clipped_layer"),
                None,
                None
            ]
        );
        // The keys are pigment's operator keys, not a local spelling.
        assert_eq!(ExtOperator::BlendIfClipBase.key(), "blend_if_clip_base");
        assert_eq!(
            ExtOperator::BlendIfClippedLayer.key(),
            "blend_if_clipped_layer"
        );
    }
}
