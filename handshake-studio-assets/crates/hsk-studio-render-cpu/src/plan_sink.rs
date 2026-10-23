//! CPU `RenderBackend` for the shared pigment render plan: admission, source verification, then
//! bounded `chunk x chunk` evaluation into a caller `TileSink`.
//!
//! The executor receives buffers already in the plan's colour tag and materialised data only
//! (STU-COL-163): it never resolves a profile, converts a space or calls a colour engine. Blend
//! arithmetic is the single pigment owner `blend_math::composite_over`; this module adds chunking,
//! groups, masks, vector coverage, budgets and cancellation around it.
//!
//! Clip-to-base (`MaskFormat::BaseAlpha`, pigment's documented run semantics, exactness
//! Approximate, Photoshop rule UNVERIFIED): a run `[base, c1..cn]` is evaluated per chunk as an
//! isolated unit `G` held in one scratch buffer. `G` = base content with the base's own mask (no
//! opacity, fill or mode); each `ci` is blended onto `G` with its own mode, opacity and fill and a
//! source alpha multiplied by the base alpha, then `G`'s alpha is held at the base alpha; `G` is
//! composited onto the running backdrop with the BASE layer's mode, opacity and fill. The base
//! alpha is `G`'s own alpha (it never changes during the run), so a clipped layer needs no plane.
//!
//! Envelope operators (`RenderBackend::execute_extended`: adjustment layers, Blend-If, mask density
//! and feather) live in the `extended` submodule and layer effects (waves 1 and 2) in `effects`; both call
//! pigment's maths. Blend-If is applied to unclipped layers only: on a clip base or a clipped layer
//! pigment defines none and shared admission refuses it before any sink call. A plan without an
//! envelope takes the unchanged v1 path.
//!
//! Workflow v4 V4-01: this module and `px` deny unchecked integer arithmetic; plan-derived sizes
//! and coordinates go through the checked helpers of `crate::px`.
#![deny(clippy::arithmetic_side_effects)]
mod blur;
mod content_aware;
mod display;
mod effects;
mod extended;
mod filter;
mod key;
mod knockout;
mod smart_filter;
#[cfg(test)]
mod refusal_keys;

use crate::coverage::PreparedPath;
use crate::px::{
    add_u, area, end, grow_rect, index_in, intersect, mul_u, offset, overflow, to_i64, union,
};
use crate::vector::VectorSource;
pub use display::{DISPLAY_CLIP_EXACTNESS, DISPLAY_CLIP_OPERATOR, DisplayTransform};
use display::{over_fill, sample_ok};
pub use extended::MAX_EFFECT_WINDOW_WORK;
use extended::{ExtPrep, FrameShape, Fx, LayerFx, blend_weight, coverage_error};
use hsk_studio_accord::CancellationToken;
use hsk_studio_nib::PathLimits;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::{BlendError, PixelSite, Rgba, pass_through_mix};
use hsk_studio_pigment::blur_math::BlurScope;
use hsk_studio_pigment::coverage_math::mask_coverage;
use hsk_studio_pigment::effect_math::EffectKind;
use hsk_studio_pigment::transform_math::{LayerTransform, PreparedTransform};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, Applicability, BackendError, BackendIdentity, DeviceClass, Exactness,
    ExactnessLedger, ExecuteRequest, ExtOperator, Layer, MASK_DENSITY_OPERATOR, MaskFormat,
    MaskInput, Operator, PASS_THROUGH_FILL_OPERATOR, PASS_THROUGH_MASK_OPERATOR, PlanError,
    PlanExtensions, PlanLimits, PlanNode, PlanSources, RenderBackend, RenderPlan, RenderReceipt,
    SAMPLE_RANGE_OPERATOR, SampleFormat, SourceKey, StudioBlendMode, TileInput, TilePlanes,
    TileSink, admit_extended_with_limits, admit_with_limits, quantise_8bit,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

pub const RENDERER_NAME: &str = "hsk-studio-render-cpu";
pub const RENDERER_VERSION: &str = "0.1";
pub use hsk_studio_nib::MAX_PATH_ELEMENTS;
pub const MAX_CHUNK_SIDE: u32 = 4096;
pub const MAX_THREADS: usize = 64;
pub const DEFAULT_CHUNK_SIDE: u32 = 64;
/// Scratch charged per chunk pixel: 16 working (RGBA f32) + 12 colour out + 4 alpha out.
pub const SCRATCH_BYTES_PER_PIXEL: u64 = 32;
/// Extra scratch per chunk pixel for each level of nested isolated group (one RGBA f32 buffer).
pub const GROUP_SCRATCH_BYTES_PER_PIXEL: u64 = 16;

/// Why a source was refused; every case maps to `BackendError::SourceMismatch(key)` at the port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceFault {
    /// Resolved planes differ from what the plan declared (size, hashes, format, alpha, tag).
    Declared,
    MalformedStride,
    HashMismatch,
    Nonfinite,
    AlphaRange,
    /// Mask plane does not contain every pixel its layer can touch inside the target.
    MaskGap,
}
impl SourceFault {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Declared => "source_declared_mismatch",
            Self::MalformedStride => "source_malformed_stride",
            Self::HashMismatch => "source_hash_mismatch",
            Self::Nonfinite => "source_nonfinite",
            Self::AlphaRange => "source_alpha_range",
            Self::MaskGap => "mask_coverage_gap",
        }
    }
}

/// Detailed internal error; `From<RenderError> for BackendError` is the port mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderError {
    Backend(BackendError),
    Source { key: SourceKey, fault: SourceFault },
    InvalidConfig(&'static str),
}
impl From<BackendError> for RenderError {
    fn from(error: BackendError) -> Self {
        Self::Backend(error)
    }
}
impl From<RenderError> for BackendError {
    fn from(error: RenderError) -> Self {
        match error {
            RenderError::Backend(e) => e,
            RenderError::Source { key, .. } => BackendError::SourceMismatch(key),
            RenderError::InvalidConfig(_) => BackendError::Unavailable("invalid_config"),
        }
    }
}
impl RenderError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Backend(e) => e.code(),
            Self::Source { fault, .. } => fault.code(),
            Self::InvalidConfig(_) => "invalid_config",
        }
    }
}
impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Source { key, fault } => write!(f, "{}(key {})", fault.code(), key.0),
            Self::InvalidConfig(what) => write!(f, "invalid_config({what})"),
            Self::Backend(e) => write!(f, "{}", e.code()),
        }
    }
}
impl std::error::Error for RenderError {}

pub(crate) fn unsupported(operator: &'static str) -> RenderError {
    RenderError::Backend(BackendError::Unsupported {
        operator,
        exactness: Exactness::Unsupported,
    })
}
fn source(key: SourceKey, fault: SourceFault) -> RenderError {
    RenderError::Source { key, fault }
}
/// Typed mapping of a blend failure; every key is pigment's (PLR-006, test
/// `defence_in_depth_refusal_keys_are_pigments`): samples outside `[0, 1]` are
/// [`SAMPLE_RANGE_OPERATOR`], a mode this backend cannot evaluate is `Operator::Blend(mode).key()`
/// (what shared admission reports for it), any other failure is its own `BlendError::code`.
fn blend_error(error: BlendError) -> RenderError {
    match error {
        BlendError::OutOfDomain => unsupported(SAMPLE_RANGE_OPERATOR),
        BlendError::GroupOnly(mode)
        | BlendError::ToolOnly(mode)
        | BlendError::Unsupported(mode) => unsupported(Operator::Blend(mode).key()),
        other => unsupported(other.code()),
    }
}

/// True when this backend can evaluate `mode` as a layer mode (STU-RAS-155): layer-applicable
/// with a recovered function. `pass_through` (group-only) is a group operator, not a blend.
pub fn blend_supported(mode: StudioBlendMode) -> bool {
    let facts = mode.facts();
    facts.applicability == Applicability::Layer && facts.exactness != Exactness::Unsupported
}

/// Bounded CPU executor. Scratch is charged against the request's `max_working_bytes` before it is
/// allocated and released on every exit path by an RAII lease; `live_scratch_bytes` is 0 whenever
/// no render runs.
pub struct Renderer {
    chunk: u32,
    threads: usize,
    /// Plan budgets handed to shared admission: the maxima unless the host tightened them with
    /// [`Renderer::with_plan_limits`].
    limits: PlanLimits,
    /// Deprecated alias (`with_display_transform`): the output stage a plan without one runs with.
    display: Option<DisplayTransform>,
    live: AtomicU64,
    peak: AtomicU64,
}
impl Default for Renderer {
    fn default() -> Self {
        Self::with_chunk(DEFAULT_CHUNK_SIDE)
    }
}

struct ScratchLease<'a> {
    owner: &'a Renderer,
    bytes: u64,
}
impl Drop for ScratchLease<'_> {
    fn drop(&mut self) {
        self.owner.live.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

// ---- prepared (verified) plan -------------------------------------------------------------

#[derive(Clone, Copy)]
struct MaskView<'a> {
    bounds: Rect,
    wide: bool,
    bytes: &'a [u8],
    stride: usize,
    inverted: bool,
    /// `MaskInput.density`; below 1 only on the envelope path (`ExtPrep::allow_density`).
    density: f32,
    /// A linked mask of a transformed layer (`LayerTransform::transform_mask`): the plane is
    /// resampled through the layer transform by pigment's reference sampler.
    xform: Option<PreparedTransform>,
}
impl MaskView<'_> {
    /// Stored coverage at plan pixel `(x, y)`: [`MaskView::raw`], or for a transformed mask
    /// pigment's `PreparedTransform::sample_coverage` over the plane (zero outside it).
    fn stored(&self, x: i64, y: i64) -> Result<f32, RenderError> {
        let Some(t) = &self.xform else {
            return self.raw(x, y);
        };
        let (bx, by) = (self.bounds.x, self.bounds.y);
        t.sample_coverage(x, y, |i, j| {
            let px = bx.checked_add(i64::from(i)).ok_or_else(overflow)?;
            let py = by.checked_add(i64::from(j)).ok_or_else(overflow)?;
            self.raw(px, py)
        })
    }
    /// Stored coverage at plan pixel `(x, y)` (before invert and density); a pixel outside the
    /// plane is a typed `Overflow` (prepare checked that every pixel a layer touches is inside).
    fn raw(&self, x: i64, y: i64) -> Result<f32, RenderError> {
        let row = offset(y, self.bounds.y)?
            .checked_mul(self.stride)
            .ok_or_else(overflow)?;
        let column = offset(x, self.bounds.x)?;
        if self.wide {
            let at = column
                .checked_mul(2)
                .and_then(|c| row.checked_add(c))
                .ok_or_else(overflow)?;
            let end = at.checked_add(2).ok_or_else(overflow)?;
            let Some(&[lo, hi]) = self.bytes.get(at..end) else {
                return Err(overflow());
            };
            Ok((f64::from(u16::from_le_bytes([lo, hi])) / 65535.0) as f32)
        } else {
            let at = row.checked_add(column).ok_or_else(overflow)?;
            let byte = *self.bytes.get(at).ok_or_else(overflow)?;
            Ok((f64::from(byte) / 255.0) as f32)
        }
    }
    /// [`MaskView::raw`] with the pixel clamped into the plane (feather taps clamp at the border).
    fn raw_clamped(&self, x: i64, y: i64) -> Result<f32, RenderError> {
        let (x1, y1) = end(self.bounds)?;
        self.raw(
            x.max(self.bounds.x).min(x1.saturating_sub(1)),
            y.max(self.bounds.y).min(y1.saturating_sub(1)),
        )
    }
    /// Effective coverage from stored (possibly feathered) coverage `raw`: invert, then density.
    /// At density 1 this is the v1 inversion (bytes of every v1 plan stay identical); below 1 it is
    /// `coverage_math::mask_coverage`, pigment's single owner of the density formula.
    fn effective(&self, raw: f32) -> Result<f32, RenderError> {
        if self.density == 1.0 {
            Ok(if self.inverted { 1.0 - raw } else { raw })
        } else {
            mask_coverage(raw, self.inverted, self.density).map_err(coverage_error)
        }
    }
}

/// Mask coverage of `layer` at `(x, y)`: 1 without a mask; `plane` is the feathered chunk plane
/// (indexed by `at`) when the layer has a feather, else the stored plane is read directly.
fn mask_value(
    layer: &PLayer<'_>,
    plane: Option<&[f32]>,
    x: i64,
    y: i64,
    at: usize,
) -> Result<f32, RenderError> {
    let Some(mask) = layer.mask else {
        return Ok(1.0);
    };
    let raw = match plane {
        Some(plane) => *plane.get(at).ok_or_else(overflow)?,
        None => mask.stored(x, y)?,
    };
    mask.effective(raw)
}

/// [`mask_value`] for an effect frame: a frame reaches beyond the mask plane (the layer bounds plus
/// the effect margin), where the border coverage continues, as for the feather taps.
fn mask_value_clamped(
    layer: &PLayer<'_>,
    plane: Option<&[f32]>,
    x: i64,
    y: i64,
    at: usize,
) -> Result<f32, RenderError> {
    let Some(mask) = layer.mask else {
        return Ok(1.0);
    };
    let raw = match plane {
        Some(plane) => *plane.get(at).ok_or_else(overflow)?,
        None => mask.raw_clamped(x, y)?,
    };
    mask.effective(raw)
}

struct PLayer<'a> {
    input: usize,
    mode: StudioBlendMode,
    /// `opacity * fill_opacity`; fill is folded in as a product (class Approximate, see
    /// `Operator::FillOpacity`) until the Photoshop fill oracle exists.
    opacity: f32,
    mask: Option<MaskView<'a>>,
    /// Clip-to-base layer: its coverage is the alpha of the run's base (no plane, `mask` is None).
    clipped: bool,
    /// Envelope state (adjustment layer, Blend-If, feather, effects); default on a v1 layer.
    fx: LayerFx,
    /// `Layer.opacity` and `Layer.fill_opacity` as given (`opacity` above is their product); the
    /// effects composition takes them separately.
    layer_opacity: f32,
    fill_opacity: f32,
}
fn is_clipped(layer: &Layer) -> bool {
    layer.mask.as_ref().is_some_and(MaskInput::is_clip_to_base)
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
    Vector {
        path: PreparedPath,
        rgba: [f32; 4],
    },
    Group {
        layers: Vec<PLayer<'a>>,
        isolated: bool,
    },
    /// A tile resampled through a layer transform (envelope `LayerExtension::transform`): a node of
    /// [`Prepared::extra`], read with pigment's reference sampler (bytes are pigment's).
    Transformed {
        planes: TilePlanes<'a>,
        prep: PreparedTransform,
    },
    /// The output of a content-aware fill (envelope `LayerExtension::content_aware_fill`): a node
    /// of [`Prepared::extra`] holding pigment's filled plane (straight RGBA over `rect`), written
    /// by `content_aware::run_fills` before the first chunk (empty until then).
    Filled {
        rect: Rect,
        data: Vec<[f32; 4]>,
    },
}
/// Straight RGBA at plan pixel `(x, y)` of a transformed tile: pigment's
/// `PreparedTransform::sample_rgba` over the verified planes.
fn transformed_at(
    range: hsk_studio_pigment::render_plan::SampleRange,
    planes: &TilePlanes<'_>,
    prep: &PreparedTransform,
    x: i64,
    y: i64,
) -> Result<[f32; 4], RenderError> {
    prep.sample_rgba_in(range, x, y, |i, j| {
        tile_pixel(
            planes,
            crate::px::to_usize(u64::from(i))?,
            crate::px::to_usize(u64::from(j))?,
        )
    })
}
fn transform_invalid(node: u32, error: hsk_studio_pigment::transform_math::TransformError) -> RenderError {
    RenderError::Backend(BackendError::InvalidPlan(PlanError::TransformMatrix { node, error }))
}
struct Prepared<'a> {
    nodes: Vec<PNode<'a>>,
    bounds: Vec<Option<Rect>>,
    /// Transformed-tile nodes, addressed as `nodes.len() + k` by the layers that use them.
    extra: Vec<PNode<'a>>,
    extra_bounds: Vec<Option<Rect>>,
    /// Scratch buffers an evaluation of the node needs below its own destination.
    need: Vec<usize>,
    root: usize,
    reachable: u64,
    seed: u64,
    /// Envelope tables (adjustments, Blend-If sets, feather kernels); empty on the v1 path.
    ext: ExtPrep,
    /// Content-aware fills to run before the first chunk (lane SMB-RENDER-CONTENT-AWARE).
    fills: Vec<content_aware::FillJob<'a>>,
    /// Key pipelines to run before the first chunk, after the fills (lane SMB-AE-KEY-TRACK-2).
    keys: Vec<key::KeyJob>,
    /// Smart-filter stacks to run before the first chunk, after the keys (lane
    /// SMB-PS-SELECT-LIQUIFY-2).
    smart: Vec<smart_filter::SmartJob<'a>>,
}

impl<'a> Prepared<'a> {
    /// Node `index` of the plan, or transformed-tile node `index - nodes.len()`.
    fn node(&self, index: usize) -> Result<&PNode<'a>, RenderError> {
        match index.checked_sub(self.nodes.len()) {
            None => self.nodes.get(index),
            Some(k) => self.extra.get(k),
        }
        .ok_or_else(|| unsupported("plan_shape"))
    }
    /// Bounds of [`Prepared::node`] `index` (`None` when empty or out of range).
    fn bound(&self, index: usize) -> Option<Rect> {
        match index.checked_sub(self.bounds.len()) {
            None => self.bounds.get(index).copied().flatten(),
            Some(k) => self.extra_bounds.get(k).copied().flatten(),
        }
    }
}

/// Solid / vector fill colour in the plan's sample range (alpha always `[0, 1]`).
fn unit_rgb(rgba: [f32; 4], extended: bool) -> Result<(), RenderError> {
    if rgba[..3].iter().all(|c| sample_ok(extended, *c))
        && rgba[3].is_finite()
        && (0.0..=1.0).contains(&rgba[3])
    {
        Ok(())
    } else {
        Err(unsupported(SAMPLE_RANGE_OPERATOR))
    }
}

fn plane_ok(
    bytes: &[u8],
    stride: u64,
    width: u32,
    height: u32,
    bpp: u64,
) -> Result<(), SourceFault> {
    let Some(row) = u64::from(width).checked_mul(bpp) else {
        return Err(SourceFault::MalformedStride);
    };
    let size = stride.checked_mul(u64::from(height));
    if stride < row || !stride.is_multiple_of(4) || u64::try_from(bytes.len()).ok() != size {
        return Err(SourceFault::MalformedStride);
    }
    Ok(())
}
fn f32_at(bytes: &[u8], at: usize) -> Result<f32, RenderError> {
    let end = at.checked_add(4).ok_or_else(overflow)?;
    let Some(&[a, b, c, d]) = bytes.get(at..end) else {
        return Err(overflow());
    };
    Ok(f32::from_le_bytes([a, b, c, d]))
}
/// Straight RGBA of tile pixel `(column, row)`.
fn tile_pixel(p: &TilePlanes<'_>, column: usize, row: usize) -> Result<[f32; 4], RenderError> {
    let at = |stride: u64, bytes_per_pixel: usize| -> Result<usize, RenderError> {
        mul_u(row, crate::px::to_usize(stride)?)
            .and_then(|r| add_u(r, mul_u(column, bytes_per_pixel)?))
    };
    let c = at(p.colour_stride_bytes, 12)?;
    let a = at(p.alpha_stride_bytes, 4)?;
    Ok([
        f32_at(p.colour, c)?,
        f32_at(p.colour, add_u(c, 4)?)?,
        f32_at(p.colour, add_u(c, 8)?)?,
        f32_at(p.alpha, a)?,
    ])
}
/// Straight RGBA at plan pixel `(x, y)` of a tile placed at `rect`.
fn tile_at(p: &TilePlanes<'_>, rect: Rect, x: i64, y: i64) -> Result<[f32; 4], RenderError> {
    tile_pixel(p, offset(x, rect.x)?, offset(y, rect.y)?)
}

/// Shape, plane hashes and values of a resolved tile against the plan's declaration. A colour
/// sample outside `[0, 1]` is refused unless the plan's output stage is `extended` (any finite
/// colour, `display.rs`).
fn verify_tile(
    input: &TileInput,
    planes: &TilePlanes<'_>,
    plan_space: hsk_studio_pigment::render_plan::ColourTag,
    extended: bool,
) -> Result<(), RenderError> {
    let key = input.key;
    // Defence in depth: this backend declares `ExtOperator::AlphaPremultiplied` Unsupported, so
    // shared admission refuses a premultiplied convention first (key is pigment's).
    if input.alpha != AlphaConvention::Straight {
        return Err(unsupported(ExtOperator::AlphaPremultiplied.key()));
    }
    if !planes.matches(input, plan_space) {
        return Err(source(key, SourceFault::Declared));
    }
    plane_ok(
        planes.colour,
        planes.colour_stride_bytes,
        planes.width,
        planes.height,
        12,
    )
    .and_then(|()| {
        plane_ok(
            planes.alpha,
            planes.alpha_stride_bytes,
            planes.width,
            planes.height,
            4,
        )
    })
    .map_err(|f| source(key, f))?;
    if hsk_studio_prism::profile_hash(planes.colour) != planes.colour_sha256
        || hsk_studio_prism::profile_hash(planes.alpha) != planes.alpha_sha256
    {
        return Err(source(key, SourceFault::HashMismatch));
    }
    let mut out_of_unit = false;
    for y in 0..planes.height {
        for x in 0..planes.width {
            let [r, g, b, a] = tile_pixel(planes, x as usize, y as usize)?;
            if ![r, g, b, a].iter().all(|v| v.is_finite()) {
                return Err(source(key, SourceFault::Nonfinite));
            }
            if !(0.0..=1.0).contains(&a) {
                return Err(source(key, SourceFault::AlphaRange));
            }
            out_of_unit |= ![r, g, b].iter().all(|v| sample_ok(extended, *v));
        }
    }
    if out_of_unit {
        return Err(unsupported(SAMPLE_RANGE_OPERATOR));
    }
    Ok(())
}

/// `reach[i]`: node `i` is the root or the input of a layer of a reachable group. Both callers
/// run after shared admission, whose `RenderPlan::validate` refuses a root or an input out of
/// range and an input that is not an earlier node as a typed `InvalidPlan` before this sweep runs
/// (test `tests/cpu_envelope_reach.rs` `invalid_plan_indexes_are_refused_before_the_reachable_sweep`),
/// so the indexing below is in range and one reverse sweep visits each group after all the groups
/// that reference it. This is the set `prepare` executes and the set the envelope entries are
/// prepared for.
fn reachable<V>(plan: &RenderPlan<V>) -> Vec<bool> {
    let n = plan.nodes.len();
    let mut reach = vec![false; n];
    reach[plan.root.0 as usize] = true;
    for i in (0..n).rev() {
        if reach[i]
            && let PlanNode::Group(g) = &plan.nodes[i]
        {
            for l in &g.layers {
                reach[l.input.0 as usize] = true;
            }
        }
    }
    reach
}

fn prepare<'a, V: VectorSource>(
    plan: &RenderPlan<V>,
    sources: &'a dyn PlanSources,
    mut ext: ExtPrep,
) -> Result<Prepared<'a>, RenderError> {
    let extended = ext.output.is_extended();
    let target = plan.target;
    if target.alpha != AlphaConvention::Straight {
        return Err(unsupported(ExtOperator::AlphaPremultiplied.key()));
    }
    if target.format != SampleFormat::RgbF32Le {
        return Err(unsupported("sample_format"));
    }
    // Pattern overlays (lane SMB-PS-ADJUST-STYLES-2): every pattern an effect stack reads is a plan
    // tile source, verified like a tile input (shape, hashes, unit values) and decoded once into
    // straight RGBA (its bytes were charged by `ExtPrep::build`).
    for r in ext.pattern_refs.clone() {
        let input = TileInput {
            key: r.key,
            placement: Rect { x: 0, y: 0, width: r.width, height: r.height },
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: r.colour_sha256,
            alpha_sha256: r.alpha_sha256,
        };
        let planes = sources.tile(r.key).ok_or(BackendError::UnresolvedSource(r.key))?;
        verify_tile(&input, &planes, target.space, false)?;
        let (w, h) = (
            crate::px::to_usize(u64::from(r.width))?,
            crate::px::to_usize(u64::from(r.height))?,
        );
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(mul_u(w, h)?)
            .map_err(|_| BackendError::Unavailable("pattern_allocation"))?;
        for row in 0..h {
            for col in 0..w {
                pixels.push(tile_pixel(&planes, col, row)?);
            }
        }
        ext.pattern_pixels.insert(r.key, pixels);
    }
    let n = plan.nodes.len();
    let root = plan.root.0 as usize;
    let reach = reachable(plan);
    let mut nodes: Vec<PNode<'a>> = Vec::with_capacity(n);
    let mut bounds: Vec<Option<Rect>> = Vec::with_capacity(n);
    let mut need: Vec<usize> = Vec::with_capacity(n);
    // Does the node's subtree hold a layer with effects? (An effect layer's content may not.)
    let mut has_effects: Vec<bool> = Vec::with_capacity(n);
    let mut extra: Vec<PNode<'a>> = Vec::new();
    let mut extra_bounds: Vec<Option<Rect>> = Vec::new();
    // Does the node's subtree hold a content-aware fill? (A filled layer's content may not.)
    let mut has_fill: Vec<bool> = Vec::with_capacity(n);
    let mut fills: Vec<content_aware::FillJob<'a>> = Vec::new();
    let mut key_jobs: Vec<key::KeyJob> = Vec::new();
    let mut smart_jobs: Vec<smart_filter::SmartJob<'a>> = Vec::new();
    for (i, node) in plan.nodes.iter().enumerate() {
        let mut group_has_effects = false;
        let mut group_has_fill = false;
        if !reach[i] {
            nodes.push(PNode::Unused);
            bounds.push(None);
            need.push(0);
            has_effects.push(false);
            has_fill.push(false);
            continue;
        }
        let (pnode, node_bounds, node_need) = match node {
            PlanNode::Tile(t) => {
                let planes = sources
                    .tile(t.key)
                    .ok_or(BackendError::UnresolvedSource(t.key))?;
                verify_tile(t, &planes, target.space, extended)?;
                end(t.placement)?;
                (
                    PNode::Tile {
                        planes,
                        rect: t.placement,
                    },
                    Some(t.placement),
                    0,
                )
            }
            PlanNode::Solid { rect, rgba } => {
                unit_rgb(*rgba, extended)?;
                end(*rect)?;
                (PNode::Solid { rgba: *rgba }, Some(*rect), 0)
            }
            PlanNode::Vector(v) => {
                let fill = v.fill();
                unit_rgb(fill.rgba, extended)?;
                let path =
                    PreparedPath::prepare(&fill.path, fill.aa_samples, &PathLimits::default())?;
                end(path.bounds)?;
                let bounds = path.bounds;
                (
                    PNode::Vector {
                        path,
                        rgba: fill.rgba,
                    },
                    Some(bounds),
                    0,
                )
            }
            PlanNode::Group(g) => {
                let mut layers = Vec::with_capacity(g.layers.len());
                let mut group_bounds = None;
                let mut group_need = 0;
                let mut group_effects = false;
                // Scratch beneath the compositing buffer of the clip run being read (None outside
                // a run): the base evaluates straight into the run buffer, clipped members after.
                let mut run_need: Option<usize> = None;
                for (li, l) in g.layers.iter().enumerate() {
                    let input = l.input.0 as usize;
                    let child = &plan.nodes[input];
                    let pass_through = l.mode == StudioBlendMode::PassThrough;
                    let clipped = is_clipped(l);
                    let starts_run =
                        !clipped && g.layers.get(li.saturating_add(1)).is_some_and(is_clipped);
                    // A pass-through group has no isolated content to take a base alpha from.
                    // Pigment validation refuses it first (`ClipBasePassThrough`); kept as defence
                    // in depth so `prepare` stays safe on its own, and refused with pigment's own
                    // error value for the node, never a local spelling (PLR-006).
                    if starts_run && pass_through {
                        return Err(RenderError::Backend(BackendError::InvalidPlan(
                            PlanError::ClipBasePassThrough {
                                node: u32::try_from(i).map_err(|_| overflow())?,
                            },
                        )));
                    }
                    let child_isolated = match child {
                        PlanNode::Group(c) => Some(c.isolated),
                        _ => None,
                    };
                    // Structural validation already guaranteed pass_through <-> non-isolated group.
                    // Every refusal key below is pigment's operator key (`Operator::key`), so the
                    // defence in depth reports exactly what shared admission reports. A
                    // pass-through layer with opacity below 1 or a mask is evaluated (lane
                    // SMB-PS-ORACLE-PARITY-2, pigment `pass_through_mix`, `Eval::layer`).
                    // A non-isolated group composites straight onto the running backdrop, so there
                    // is no group result to scale: fill is as undefined as opacity here. Pigment's
                    // `admit` refuses it first for every backend (`PASS_THROUGH_FILL_OPERATOR`,
                    // intrinsically Unsupported); kept as defence in depth under the same key,
                    // typed and before the first sink call, never rendering the group at full
                    // strength while the receipt claims fill was applied.
                    if pass_through && l.fill_opacity < 1.0 {
                        return Err(unsupported(PASS_THROUGH_FILL_OPERATOR));
                    }
                    if !pass_through {
                        if child_isolated == Some(false) {
                            return Err(unsupported(Operator::NonIsolatedBlendedGroup.key()));
                        }
                        if !blend_supported(l.mode) {
                            return Err(unsupported(Operator::Blend(l.mode).key()));
                        }
                    }
                    let layer_fx = ext.layer(i, li);
                    let node_id = u32::try_from(i).map_err(|_| overflow())?;
                    // Layer transform: the layer reads a transformed-tile node of `extra` instead
                    // of its tile (pigment validated the tile input, the combinations and the
                    // matrix; kept as defence in depth with pigment's error values).
                    let mut mask_transform: Option<LayerTransform> = None;
                    let (source_index, content_bounds) = match layer_fx.transform {
                        None => (input, bounds[input]),
                        Some(index) => {
                            let t = ext.transform(index).ok_or_else(|| unsupported("plan_shape"))?;
                            let PNode::Tile { planes, rect } = &nodes[input] else {
                                return Err(RenderError::Backend(BackendError::InvalidPlan(
                                    PlanError::TransformTarget { node: node_id },
                                )));
                            };
                            if layer_fx.effect.is_some()
                                || (t.transform_mask && layer_fx.feather.is_some())
                            {
                                return Err(RenderError::Backend(BackendError::InvalidPlan(
                                    PlanError::TransformCombination { node: node_id },
                                )));
                            }
                            let prep = PreparedTransform::new(&t, *rect)
                                .map_err(|error| transform_invalid(node_id, error))?;
                            if t.transform_mask {
                                mask_transform = Some(t);
                            }
                            let at = add_u(n, extra.len())?;
                            extra.push(PNode::Transformed {
                                planes: *planes,
                                prep,
                            });
                            extra_bounds.push(Some(prep.bounds()));
                            (at, Some(prep.bounds()))
                        }
                    };
                    // Layer effects (waves 1 and 2): the layer is evaluated per chunk window (the content
                    // bounds grown by the stack margin, clipped to the chunk) and composed by
                    // pigment's `render_layer_window`.
                    let mut layer_bounds = content_bounds;
                    if let Some(index) = layer_fx.effect {
                        let margin = ext
                            .effect(index)
                            .map(|e| e.margin)
                            .ok_or_else(|| unsupported("plan_shape"))?;
                        // Defence in depth: pigment's shared admission decides both rules first
                        // (`ExtOperator::EffectsWithBlendIf` is intrinsically Unsupported;
                        // `ExtOperator::EffectsNested` is declared Unsupported by this backend),
                        // so the keys are pigment's.
                        if layer_fx.blend_if.is_some() {
                            return Err(unsupported(ExtOperator::EffectsWithBlendIf.key()));
                        }
                        if has_effects[input] {
                            return Err(unsupported(ExtOperator::EffectsNested.key()));
                        }
                        // The shadow and stroke reach beyond the content bounds by the margin.
                        layer_bounds = layer_bounds.map(|b| grow_rect(b, margin)).transpose()?;
                        ext.frame_depth = ext.frame_depth.max(need[input]);
                        ext.set_effect_bounds(index, content_bounds);
                    }
                    // Layer blur / backdrop blur (pigment `blur_math`): evaluated per chunk window
                    // on the chunk-level frame like the effects (see `blur`).
                    if let Some(index) = layer_fx.blur {
                        let (scope, support) = ext
                            .blur(index)
                            .map(|b| (b.blur.scope, b.support))
                            .ok_or_else(|| unsupported("plan_shape"))?;
                        // The frame is the chunk-level scratch: a blur or effects inside the
                        // content of a blurred layer have none left (typed, before any sink call).
                        if has_effects[input] {
                            return Err(unsupported(blur::BLUR_NESTED));
                        }
                        match scope {
                            BlurScope::Layer => {
                                layer_bounds =
                                    layer_bounds.map(|b| grow_rect(b, support)).transpose()?;
                                ext.frame_depth = ext.frame_depth.max(need[input]);
                            }
                            BlurScope::Backdrop => {
                                // Defence in depth for pigment's `BlurCombination`: the backdrop
                                // is the isolated group's own result below the layer.
                                if !g.isolated {
                                    return Err(RenderError::Backend(BackendError::InvalidPlan(
                                        PlanError::BlurCombination { node: node_id },
                                    )));
                                }
                                // The backdrop is re-evaluated over the grown window on the frame:
                                // layers below with effects or a blur would need a second frame.
                                if group_effects {
                                    return Err(unsupported(blur::BLUR_NESTED));
                                }
                                let prefix = run_need
                                    .map_or(group_need, |r| group_need.max(r.saturating_add(1)));
                                ext.frame_depth = ext.frame_depth.max(prefix).max(need[input]);
                            }
                        }
                        ext.set_blur_bounds(index, content_bounds);
                    }
                    // Layer filters (pigment `filter_math`): the layer is evaluated per window of the
                    // chunk-level frame like a layer blur (see `filter`). Pigment validated the
                    // combinations (`FilterCombination`); kept as defence in depth with its error.
                    if let Some(index) = layer_fx.filter {
                        if layer_fx.effect.is_some()
                            || layer_fx.blur.is_some()
                            || layer_fx.transform.is_some()
                            || layer_fx.adjustment.is_some()
                            || pass_through
                            || clipped
                        {
                            return Err(RenderError::Backend(BackendError::InvalidPlan(
                                PlanError::FilterCombination { node: node_id },
                            )));
                        }
                        if has_effects[input] {
                            return Err(unsupported(filter::FILTER_NESTED));
                        }
                        let growth = ext
                            .filter(index)
                            .ok_or_else(|| unsupported("plan_shape"))?
                            .total_growth()?;
                        layer_bounds = layer_bounds.map(|b| grow_rect(b, growth)).transpose()?;
                        ext.frame_depth = ext.frame_depth.max(need[input]);
                        ext.set_filter_bounds(index, content_bounds);
                    }
                    // Content-aware fill (pigment `content_aware::fill`): the layer reads a filled
                    // node of `extra`, written before the first chunk (see `content_aware`).
                    // Pigment validated the combinations (`ContentAwareCombination`); kept as
                    // defence in depth with its error.
                    let source_index = match layer_fx.content_aware {
                        None => source_index,
                        Some(index) => {
                            if layer_fx.effect.is_some()
                                || layer_fx.blur.is_some()
                                || layer_fx.filter.is_some()
                                || layer_fx.transform.is_some()
                                || layer_fx.adjustment.is_some()
                                || pass_through
                                || clipped
                                || starts_run
                            {
                                return Err(RenderError::Backend(BackendError::InvalidPlan(
                                    PlanError::ContentAwareCombination { node: node_id },
                                )));
                            }
                            if has_effects[input] || has_fill[input] {
                                return Err(unsupported(content_aware::CONTENT_AWARE_NESTED));
                            }
                            let op = ext
                                .content_aware(index)
                                .ok_or_else(|| unsupported("plan_shape"))?;
                            let hole = content_aware::hole_view(sources, &op.hole, node_id)?;
                            // The plane handed to pigment: the content inside the plan extent.
                            let plane = content_bounds
                                .map(|b| intersect(b, target.extent))
                                .transpose()?
                                .flatten();
                            let slot = extra.len();
                            let at = add_u(n, slot)?;
                            extra.push(PNode::Filled {
                                rect: plane.unwrap_or(target.extent),
                                data: Vec::new(),
                            });
                            extra_bounds.push(plane);
                            if let Some(bounds) = plane {
                                fills.push(content_aware::FillJob {
                                    extra: slot,
                                    input,
                                    bounds,
                                    hole,
                                    op,
                                    node: node_id,
                                });
                            }
                            layer_bounds = plane;
                            group_has_fill = true;
                            at
                        }
                    };
                    // Key pipeline (pigment `keying::run_key_pipeline`): the layer reads a keyed
                    // node of `extra`, written before the first chunk (see `key`). Pigment
                    // validated the combinations (`KeyCombination`); kept as defence in depth.
                    let source_index = match layer_fx.key {
                        None => source_index,
                        Some(index) => {
                            if layer_fx.effect.is_some()
                                || layer_fx.blur.is_some()
                                || layer_fx.filter.is_some()
                                || layer_fx.transform.is_some()
                                || layer_fx.adjustment.is_some()
                                || layer_fx.content_aware.is_some()
                                || pass_through
                                || clipped
                                || starts_run
                            {
                                return Err(RenderError::Backend(BackendError::InvalidPlan(
                                    PlanError::KeyCombination { node: node_id },
                                )));
                            }
                            if has_effects[input] || has_fill[input] {
                                return Err(unsupported(key::KEY_NESTED));
                            }
                            let stages = ext.key(index).ok_or_else(|| unsupported("plan_shape"))?.to_vec();
                            let plane = content_bounds
                                .map(|b| intersect(b, target.extent))
                                .transpose()?
                                .flatten();
                            let slot = extra.len();
                            let at = add_u(n, slot)?;
                            extra.push(PNode::Filled {
                                rect: plane.unwrap_or(target.extent),
                                data: Vec::new(),
                            });
                            extra_bounds.push(plane);
                            if let Some(bounds) = plane {
                                key_jobs.push(key::KeyJob { extra: slot, input, bounds, stages, node: node_id });
                            }
                            layer_bounds = plane;
                            group_has_fill = true;
                            at
                        }
                    };
                    // Smart-filter stack (pigment `smart_filter::SmartFilterStack::render`): the
                    // layer reads a filtered node of `extra`, written before the first chunk (see
                    // `smart_filter`). Pigment validated the combinations; kept as defence in depth.
                    let source_index = match layer_fx.smart {
                        None => source_index,
                        Some(index) => {
                            if layer_fx.effect.is_some()
                                || layer_fx.blur.is_some()
                                || layer_fx.filter.is_some()
                                || layer_fx.transform.is_some()
                                || layer_fx.adjustment.is_some()
                                || layer_fx.content_aware.is_some()
                                || layer_fx.key.is_some()
                                || pass_through
                                || clipped
                                || starts_run
                            {
                                return Err(RenderError::Backend(BackendError::InvalidPlan(
                                    PlanError::SmartFilterCombination { node: node_id },
                                )));
                            }
                            if has_effects[input] || has_fill[input] {
                                return Err(unsupported(smart_filter::SMART_FILTER_NESTED));
                            }
                            let op = ext.smart(index).ok_or_else(|| unsupported("plan_shape"))?.clone();
                            let mut masks = Vec::with_capacity(op.entries.len());
                            for e in &op.entries {
                                masks.push(match &e.mask {
                                    None => None,
                                    Some(m) => Some(smart_filter::mask_view(sources, m, node_id)?),
                                });
                            }
                            let stack_mask = match &op.stack_mask {
                                None => None,
                                Some(m) => Some(smart_filter::mask_view(sources, m, node_id)?),
                            };
                            let plane = content_bounds
                                .map(|b| intersect(b, target.extent))
                                .transpose()?
                                .flatten();
                            let slot = extra.len();
                            let at = add_u(n, slot)?;
                            extra.push(PNode::Filled {
                                rect: plane.unwrap_or(target.extent),
                                data: Vec::new(),
                            });
                            extra_bounds.push(plane);
                            if let Some(bounds) = plane {
                                smart_jobs.push(smart_filter::SmartJob { extra: slot, input, bounds, op, masks, stack_mask, node: node_id });
                            }
                            layer_bounds = plane;
                            group_has_fill = true;
                            at
                        }
                    };
                    group_has_fill |= has_fill[input];
                    group_effects |= layer_fx.effect.is_some()
                        || layer_fx.blur.is_some()
                        || layer_fx.filter.is_some()
                        || has_effects[input];
                    let mask = match &l.mask {
                        None => None,
                        // Canonical clip-to-base encoding (pigment validated): no plane to resolve.
                        Some(_) if clipped => None,
                        Some(m) => {
                            // Density below 1 is an envelope operator: the v1 path refuses it (and
                            // pigment's `admit` already did, as `MASK_DENSITY_OPERATOR`), the
                            // envelope path evaluates it.
                            if m.density != 1.0 && !ext.allow_density {
                                return Err(unsupported(MASK_DENSITY_OPERATOR));
                            }
                            // Defence in depth: pigment defines no mask on a pass-through layer
                            // and shared admission refuses it first (its constant).
                            if pass_through {
                                return Err(unsupported(PASS_THROUGH_MASK_OPERATOR));
                            }
                            let plane = sources
                                .mask(m.key)
                                .ok_or(BackendError::UnresolvedSource(m.key))?;
                            let wide = match m.format {
                                MaskFormat::CoverageU8 => false,
                                MaskFormat::CoverageU16Le => true,
                                // A format this backend has no reader for: pigment's operator
                                // key for it (`Operator::Mask(format).key()`). Defence in depth:
                                // `exactness` names the formats with a reader and answers
                                // Unsupported for any other, so shared admission refuses first.
                                other => return Err(unsupported(Operator::Mask(other).key())),
                            };
                            if plane.sample_format != m.format.key()
                                || plane.sha256 != m.sha256
                                || plane.bounds != m.placement
                            {
                                return Err(source(m.key, SourceFault::Declared));
                            }
                            plane.validate(plane.bounds).map_err(|e| {
                                source(
                                    m.key,
                                    if e == hsk_studio_pigment::Error::HashMismatch {
                                        SourceFault::HashMismatch
                                    } else {
                                        SourceFault::MalformedStride
                                    },
                                )
                            })?;
                            // A transformed (linked) mask has zero coverage outside its resampled
                            // plane by definition, so it has no gap to check.
                            if mask_transform.is_none()
                                && let Some(region) = content_bounds
                                    .and_then(|b| intersect(b, target.extent).ok().flatten())
                                && !plane
                                    .bounds
                                    .contains(region)
                                    .map_err(|_| BackendError::Overflow)?
                            {
                                return Err(source(m.key, SourceFault::MaskGap));
                            }
                            Some(MaskView {
                                bounds: plane.bounds,
                                wide,
                                bytes: plane.bytes,
                                stride: plane.stride_bytes as usize,
                                inverted: m.inverted,
                                density: m.density,
                                xform: mask_transform
                                    .map(|t| PreparedTransform::new(&t, plane.bounds))
                                    .transpose()
                                    .map_err(|error| transform_invalid(node_id, error))?,
                            })
                        }
                    };
                    let child_need = match child_isolated {
                        Some(true) => need[input].saturating_add(1),
                        // a pass-through layer with opacity keeps the backdrop before the group
                        // in one scratch level (pigment `pass_through_mix`)
                        Some(false) if l.opacity < 1.0 => {
                            need[input].saturating_add(1)
                        }
                        _ => need[input],
                    };
                    if clipped {
                        // Clipped content never extends beyond its base: no group extent.
                        run_need = Some(run_need.map_or(child_need, |r| r.max(child_need)));
                    } else {
                        if let Some(r) = run_need.take() {
                            group_need = group_need.max(r.saturating_add(1));
                        }
                        group_bounds = union(group_bounds, layer_bounds)?;
                        if starts_run {
                            run_need = Some(need[input]);
                        } else {
                            group_need = group_need.max(child_need);
                        }
                    }
                    layers.push(PLayer {
                        input: source_index,
                        mode: l.mode,
                        opacity: l.opacity * l.fill_opacity,
                        mask,
                        clipped,
                        fx: layer_fx,
                        layer_opacity: l.opacity,
                        fill_opacity: l.fill_opacity,
                    });
                }
                if let Some(r) = run_need {
                    group_need = group_need.max(r.saturating_add(1));
                }
                group_has_effects = group_effects;
                (
                    PNode::Group {
                        layers,
                        isolated: g.isolated,
                    },
                    group_bounds,
                    group_need,
                )
            }
        };
        nodes.push(pnode);
        bounds.push(node_bounds);
        need.push(node_need);
        has_effects.push(group_has_effects);
        has_fill.push(group_has_fill);
    }
    Ok(Prepared {
        nodes,
        bounds,
        extra,
        extra_bounds,
        need,
        root,
        reachable: reach.iter().filter(|r| **r).count() as u64,
        seed: plan.revision,
        ext,
        fills,
        keys: key_jobs,
        smart: smart_jobs,
    })
}

// ---- evaluation ------------------------------------------------------------------------------

struct Eval<'p, 'a> {
    prepared: &'p Prepared<'a>,
}

impl Eval<'_, '_> {
    /// Layers of group `idx`, bottom-first, composited into `dst` (a chunk-sized buffer).
    /// `spare` holds one scratch buffer per remaining level of isolated-group nesting.
    fn group(
        &self,
        idx: usize,
        chunk: Rect,
        dst: &mut [[f32; 4]],
        spare: &mut [Vec<[f32; 4]>],
        fx: &mut Fx,
    ) -> Result<(), RenderError> {
        self.group_upto(idx, usize::MAX, chunk, dst, spare, fx)
    }

    /// [`Eval::group`] over the first `upto` layers only (the backdrop of layer `upto`).
    fn group_upto(
        &self,
        idx: usize,
        upto: usize,
        chunk: Rect,
        dst: &mut [[f32; 4]],
        spare: &mut [Vec<[f32; 4]>],
        fx: &mut Fx,
    ) -> Result<(), RenderError> {
        let PNode::Group { layers, .. } = &self.prepared.nodes[idx] else {
            return Err(unsupported("plan_shape"));
        };
        let mut rest: &[PLayer<'_>] = layers.get(..upto).unwrap_or(layers);
        while let Some((base, tail)) = rest.split_first() {
            // Pigment validation guarantees a clipped layer follows an unclipped one.
            if base.clipped {
                return Err(unsupported("plan_shape"));
            }
            let clipped = tail.iter().take_while(|l| l.clipped).count();
            let (run, next) = tail.split_at(clipped);
            if run.is_empty() {
                // A backdrop blur changes the backdrop inside the layer before the layer itself
                // composites (pigment `blur_math`); validation keeps it out of clipping runs.
                if base.fx.blur.is_some() {
                    let position = layers
                        .len()
                        .checked_sub(rest.len())
                        .ok_or_else(overflow)?;
                    self.backdrop_blur(idx, position, base, chunk, dst, fx)?;
                }
                self.layer(base, chunk, dst, spare, fx)?;
            } else {
                self.clip_run(base, run, chunk, dst, spare, fx)?;
            }
            // the root's last layer is the delivered result, not a stored intermediate
            if idx != self.prepared.root || !next.is_empty() {
                self.store_depth(chunk, dst)?;
            }
            rest = next;
        }
        Ok(())
    }

    /// The working depth after a layer (lane SMB-PS-ORACLE-PARITY-2): in an 8-bit document the
    /// running result a later layer reads is stored in 8-bit levels, pigment
    /// `render_plan::quantise_8bit` on every straight channel and on alpha (D5 PS-C28 `nomask`,
    /// PS-C29 `fill100`); the root's final result is delivered unquantised (the sink owns the output
    /// depth). Float: nothing.
    fn store_depth(&self, chunk: Rect, dst: &mut [[f32; 4]]) -> Result<(), RenderError> {
        if !self.prepared.ext.output.is_eight_bit() {
            return Ok(());
        }
        let count = area(chunk)?;
        for px in dst.get_mut(..count).ok_or_else(overflow)? {
            *px = px.map(quantise_8bit);
        }
        Ok(())
    }

    /// One clipping run: `base` plus the layers clipped to it, through the run buffer `G` (see the
    /// module docs). Everything is per pixel, so the result is independent of chunking.
    fn clip_run(
        &self,
        base: &PLayer<'_>,
        clipped: &[PLayer<'_>],
        chunk: Rect,
        dst: &mut [[f32; 4]],
        spare: &mut [Vec<[f32; 4]>],
        fx: &mut Fx,
    ) -> Result<(), RenderError> {
        // Outside the base nothing of the run is visible.
        let Some(region) = self.prepared.bound(base.input)
            .map(|b| intersect(b, chunk))
            .transpose()?
            .flatten()
        else {
            return Ok(());
        };
        let count = area(chunk)?;
        let (buffer, deeper) = spare
            .split_first_mut()
            .ok_or(BackendError::Unavailable("group_scratch"))?;
        let run = &mut buffer[..count];
        run.fill([0.0; 4]);
        self.clip_base(base, region, chunk, run, deeper, fx)?;
        for layer in clipped {
            self.clip_layer(layer, chunk, run, deeper, fx)?;
        }
        // The base's own mask is already in `G`; opacity, fill and mode are the base layer's. Its
        // Blend-If is not applied: pigment defines none for a clip base and `admit_extended`
        // refuses it (`blend_if_clip_base`) before any sink call (test cpu_extended
        // `blend_if_on_clip_roles_is_refused_before_any_sink_call`).
        let group = PLayer {
            input: base.input,
            mode: base.mode,
            opacity: base.opacity,
            mask: None,
            clipped: false,
            fx: LayerFx::default(),
            layer_opacity: base.layer_opacity,
            fill_opacity: base.fill_opacity,
        };
        let result: &[[f32; 4]] = run;
        self.composite(&group, region, chunk, dst, fx, |_, _, at| {
            Ok((*result.get(at).ok_or_else(overflow)?, 1.0))
        })
    }

    /// `G` := base content with the base's own mask (alpha scaled), without opacity, fill or mode.
    fn clip_base(
        &self,
        base: &PLayer<'_>,
        region: Rect,
        chunk: Rect,
        run: &mut [[f32; 4]],
        deeper: &mut [Vec<[f32; 4]>],
        fx: &mut Fx,
    ) -> Result<(), RenderError> {
        match self.prepared.node(base.input)? {
            PNode::Group { isolated: true, .. } => {
                self.group(base.input, chunk, run, deeper, fx)?;
                self.seal_base(base, region, chunk, run, fx, |_, _, current| {
                    Ok((current, 1.0))
                })?;
            }
            PNode::Tile { planes, rect } => {
                self.seal_base(base, region, chunk, run, fx, |x, y, _| {
                    Ok((tile_at(planes, *rect, x, y)?, 1.0))
                })?;
            }
            PNode::Transformed { planes, prep } => {
                self.seal_base(base, region, chunk, run, fx, |x, y, _| {
                    Ok((transformed_at(self.prepared.ext.output.sample_range, planes, prep, x, y)?, 1.0))
                })?;
            }
            PNode::Solid { rgba } => {
                self.seal_base(base, region, chunk, run, fx, |_, _, _| Ok((*rgba, 1.0)))?;
            }
            PNode::Vector { path, rgba } => {
                self.seal_base(base, region, chunk, run, fx, |x, y, _| {
                    Ok((*rgba, path.coverage(x, y)))
                })?;
            }
            // A pass-through base is refused by `prepare`, a filled clip base by pigment validation
            // and `prepare` (`ContentAwareCombination`).
            PNode::Group {
                isolated: false, ..
            }
            | PNode::Filled { .. }
            | PNode::Unused => return Err(unsupported("plan_shape")),
        }
        Ok(())
    }

    /// Blend one clipped layer onto `G`.
    fn clip_layer(
        &self,
        layer: &PLayer<'_>,
        chunk: Rect,
        run: &mut [[f32; 4]],
        spare: &mut [Vec<[f32; 4]>],
        fx: &mut Fx,
    ) -> Result<(), RenderError> {
        let Some(region) = self.prepared.bound(layer.input)
            .map(|b| intersect(b, chunk))
            .transpose()?
            .flatten()
        else {
            return Ok(());
        };
        // An adjustment layer inside a run adjusts `G` only, so it never reaches beyond its base.
        if layer.fx.adjustment.is_some() {
            return self.adjust(layer, region, chunk, run, fx);
        }
        match self.prepared.node(layer.input)? {
            PNode::Group { isolated: true, .. } => {
                let count = area(chunk)?;
                let (buffer, deeper) = spare
                    .split_first_mut()
                    .ok_or(BackendError::Unavailable("group_scratch"))?;
                let buffer = &mut buffer[..count];
                buffer.fill([0.0; 4]);
                self.group(layer.input, chunk, buffer, deeper, fx)?;
                let result: &[[f32; 4]] = buffer;
                self.composite_clipped(layer, region, chunk, run, |_, _, at| {
                    Ok((*result.get(at).ok_or_else(overflow)?, 1.0))
                })
            }
            PNode::Tile { planes, rect } => {
                self.composite_clipped(layer, region, chunk, run, |x, y, _| {
                    Ok((tile_at(planes, *rect, x, y)?, 1.0))
                })
            }
            PNode::Transformed { planes, prep } => {
                self.composite_clipped(layer, region, chunk, run, |x, y, _| {
                    Ok((transformed_at(self.prepared.ext.output.sample_range, planes, prep, x, y)?, 1.0))
                })
            }
            PNode::Solid { rgba } => {
                self.composite_clipped(layer, region, chunk, run, |_, _, _| Ok((*rgba, 1.0)))
            }
            PNode::Vector { path, rgba } => {
                self.composite_clipped(layer, region, chunk, run, |x, y, _| {
                    Ok((*rgba, path.coverage(x, y)))
                })
            }
            // Pass-through clipped layers are refused by pigment validation, non-isolated
            // blended groups by `prepare`, filled clipped layers by both.
            PNode::Group {
                isolated: false, ..
            }
            | PNode::Filled { .. }
            | PNode::Unused => Err(unsupported("plan_shape")),
        }
    }

    /// Like [`Eval::composite`] for a clipped layer onto `G`: the source alpha is multiplied by the
    /// base alpha (`G`'s own alpha), and `G`'s alpha is held at the base alpha afterwards. A clipped
    /// layer's Blend-If is not applied: pigment defines none and `admit_extended` refuses it
    /// (`blend_if_clipped_layer`) before any sink call.
    fn composite_clipped(
        &self,
        layer: &PLayer<'_>,
        region: Rect,
        chunk: Rect,
        run: &mut [[f32; 4]],
        mut source: impl FnMut(i64, i64, usize) -> Result<([f32; 4], f32), RenderError>,
    ) -> Result<(), RenderError> {
        let (x1, y1) = end(region)?;
        for y in region.y..y1 {
            for x in region.x..x1 {
                let at = index_in(chunk, x, y)?;
                let b = *run.get(at).ok_or_else(overflow)?;
                let base_alpha = b[3];
                if base_alpha <= 0.0 {
                    continue;
                }
                let (rgba, coverage) = source(x, y, at)?;
                let opacity = layer.opacity * coverage;
                let source_alpha = rgba[3] * base_alpha;
                if opacity <= 0.0 || source_alpha <= 0.0 {
                    continue;
                }
                let out = over_fill(
                    self.prepared.ext.output.is_extended(),
                    layer.mode,
                    Rgba::new([b[0], b[1], b[2]], base_alpha),
                    Rgba::new([rgba[0], rgba[1], rgba[2]], source_alpha),
                    opacity,
                    layer.layer_opacity * coverage,
                    layer.fill_opacity,
                    PixelSite {
                        x,
                        y,
                        seed: self.prepared.seed,
                    },
                    self.prepared.ext.output.is_eight_bit(),
                )
                .map_err(blend_error)?;
                *run.get_mut(at).ok_or_else(overflow)? =
                    [out.rgb[0], out.rgb[1], out.rgb[2], base_alpha];
            }
        }
        Ok(())
    }

    fn layer(
        &self,
        layer: &PLayer<'_>,
        chunk: Rect,
        dst: &mut [[f32; 4]],
        spare: &mut [Vec<[f32; 4]>],
        fx: &mut Fx,
    ) -> Result<(), RenderError> {
        if let Some(depth) = layer.fx.knockout {
            return self.knockout_layer(layer, depth, chunk, dst, spare, fx);
        }
        // A layer with effects is evaluated per tile of an effect frame (see `effects`), a layer
        // with a layer blur per window of a blur frame (see `blur`).
        if layer.fx.effect.is_some() {
            return self.effect_layer(layer, chunk, dst, fx);
        }
        if self.is_layer_blur(layer) {
            return self.blur_layer(layer, chunk, dst, fx);
        }
        if self.is_filtered(layer) {
            return self.filter_layer(layer, chunk, dst, fx);
        }
        let Some(region) = self.prepared.bound(layer.input)
            .map(|b| intersect(b, chunk))
            .transpose()?
            .flatten()
        else {
            return Ok(());
        };
        // An adjustment layer applies `f(backdrop)` to the running buffer (its input is only the
        // placeholder rect that bounds it).
        if layer.fx.adjustment.is_some() {
            return self.adjust(layer, region, chunk, dst, fx);
        }
        // Knockout contexts (lane SMB-PS-ADJUST-STYLES-2): only plans with a knockout push them.
        let ext = &self.prepared.ext;
        let knockouts = ext.knockout_groups > 0 || ext.knockout_deep;
        match self.prepared.node(layer.input)? {
            PNode::Group {
                isolated: false,
                layers,
            } => {
                // a pass-through group holding a shallow knockout: its start is the running result
                let snapshot = knockouts
                    && layers.iter().any(|l| {
                        l.fx.knockout == Some(hsk_studio_pigment::render_plan::Knockout::Shallow)
                    });
                if snapshot {
                    fx.ko.push_snapshot(dst, layer.input)?;
                }
                let result = if layer.layer_opacity < 1.0 {
                    self.pass_through_weighed(layer, region, chunk, dst, spare, fx)
                } else {
                    self.group(layer.input, chunk, dst, spare, fx)
                };
                if snapshot {
                    fx.ko.pop();
                }
                result
            }
            PNode::Group { isolated: true, .. } => {
                let count = area(chunk)?;
                let (buffer, deeper) = spare
                    .split_first_mut()
                    .ok_or(BackendError::Unavailable("group_scratch"))?;
                let buffer = &mut buffer[..count];
                buffer.fill([0.0; 4]);
                if knockouts {
                    fx.ko.push_isolated(ext.knockout_deep, count);
                }
                let evaluated = self.group(layer.input, chunk, buffer, deeper, fx);
                if knockouts {
                    // deep knockouts inside clear this backdrop before the group composites
                    let cleared = fx.ko.pop_isolated(dst);
                    evaluated?;
                    cleared?;
                } else {
                    evaluated?;
                }
                let result: &[[f32; 4]] = buffer;
                self.composite(layer, region, chunk, dst, fx, |_, _, at| {
                    Ok((*result.get(at).ok_or_else(overflow)?, 1.0))
                })
            }
            PNode::Tile { planes, rect } => {
                self.composite(layer, region, chunk, dst, fx, |x, y, _| {
                    Ok((tile_at(planes, *rect, x, y)?, 1.0))
                })
            }
            PNode::Transformed { planes, prep } => {
                self.composite(layer, region, chunk, dst, fx, |x, y, _| {
                    Ok((transformed_at(self.prepared.ext.output.sample_range, planes, prep, x, y)?, 1.0))
                })
            }
            PNode::Filled { rect, data } => {
                self.composite(layer, region, chunk, dst, fx, |x, y, _| {
                    Ok((content_aware::filled_at(*rect, data, x, y)?, 1.0))
                })
            }
            PNode::Solid { rgba } => {
                self.composite(layer, region, chunk, dst, fx, |_, _, _| Ok((*rgba, 1.0)))
            }
            PNode::Vector { path, rgba } => {
                self.composite(layer, region, chunk, dst, fx, |x, y, _| {
                    Ok((*rgba, path.coverage(x, y)))
                })
            }
            PNode::Unused => Err(unsupported("plan_shape")),
        }
    }

    /// A pass-through layer with opacity below 1 (lane SMB-PS-ORACLE-PARITY-2): the running result
    /// is kept in one scratch level, the group's children composite onto `dst`, and every pixel of
    /// `region` becomes pigment `blend_math::pass_through_mix(before, after, opacity)` (D5 PS-C28
    /// `opacity50_nomask`). A mask on a pass-through layer is refused before this point
    /// (`PASS_THROUGH_MASK_OPERATOR`). Per pixel, so the result is independent of chunking.
    fn pass_through_weighed(
        &self,
        layer: &PLayer<'_>,
        region: Rect,
        chunk: Rect,
        dst: &mut [[f32; 4]],
        spare: &mut [Vec<[f32; 4]>],
        fx: &mut Fx,
    ) -> Result<(), RenderError> {
        let count = area(chunk)?;
        let (before, deeper) = spare
            .split_first_mut()
            .ok_or(BackendError::Unavailable("group_scratch"))?;
        let before = before.get_mut(..count).ok_or_else(overflow)?;
        before.copy_from_slice(dst.get(..count).ok_or_else(overflow)?);
        self.group(layer.input, chunk, dst, deeper, fx)?;
        let weight = layer.layer_opacity;
        let (x1, y1) = end(region)?;
        for y in region.y..y1 {
            for x in region.x..x1 {
                let at = index_in(chunk, x, y)?;
                let b = *before.get(at).ok_or_else(overflow)?;
                let a = *dst.get(at).ok_or_else(overflow)?;
                let out = pass_through_mix(
                    Rgba::new([b[0], b[1], b[2]], b[3]),
                    Rgba::new([a[0], a[1], a[2]], a[3]),
                    weight,
                );
                *dst.get_mut(at).ok_or_else(overflow)? =
                    [out.rgb[0], out.rgb[1], out.rgb[2], out.a.min(1.0)];
            }
        }
        Ok(())
    }

    /// A knockout layer: every pixel of its bounds is pigment `knockout_math::knockout_over` with
    /// the initial backdrop of its placement (lane SMB-PS-ADJUST-STYLES shallow in an isolated
    /// group; lane SMB-PS-ADJUST-STYLES-2 shallow at the root and in a pass-through group, from the
    /// knockout context, and deep: the transparent document base, the shape recorded for the
    /// enclosing buffers), shape = content alpha x coverage x mask x LAYER opacity, the content
    /// composited at opacity x fill. Per pixel, so the result is independent of chunking.
    fn knockout_layer(
        &self,
        layer: &PLayer<'_>,
        depth: hsk_studio_pigment::render_plan::Knockout,
        chunk: Rect,
        dst: &mut [[f32; 4]],
        spare: &mut [Vec<[f32; 4]>],
        fx: &mut Fx,
    ) -> Result<(), RenderError> {
        let Some(region) = self.prepared.bound(layer.input)
            .map(|b| intersect(b, chunk))
            .transpose()?
            .flatten()
        else {
            return Ok(());
        };
        // The content of an isolated-group input, evaluated once per chunk.
        let group_result = match self.prepared.node(layer.input)? {
            PNode::Group { isolated: true, .. } => {
                let count = area(chunk)?;
                let (buffer, deeper) = spare
                    .split_first_mut()
                    .ok_or(BackendError::Unavailable("group_scratch"))?;
                let buffer = &mut buffer[..count];
                buffer.fill([0.0; 4]);
                self.group(layer.input, chunk, buffer, deeper, fx)?;
                Some(&*buffer)
            }
            PNode::Group { isolated: false, .. } | PNode::Unused => {
                return Err(unsupported(ExtOperator::Knockout(
                    hsk_studio_pigment::render_plan::Knockout::Shallow,
                ).key()));
            }
            _ => None,
        };
        let feathered = self.feather(layer, region, chunk, fx)?;
        let plane = feathered.then_some(fx.plane.as_slice());
        let (x1, y1) = end(region)?;
        for y in region.y..y1 {
            for x in region.x..x1 {
                let at = index_in(chunk, x, y)?;
                let mask = mask_value(layer, plane, x, y, at)?;
                let (rgba, coverage) = match (group_result, self.prepared.node(layer.input)?) {
                    (Some(g), _) => (*g.get(at).ok_or_else(overflow)?, 1.0),
                    (None, PNode::Tile { planes, rect }) => (tile_at(planes, *rect, x, y)?, 1.0),
                    (None, PNode::Transformed { planes, prep }) => (
                        transformed_at(self.prepared.ext.output.sample_range, planes, prep, x, y)?,
                        1.0,
                    ),
                    (None, PNode::Filled { rect, data }) => {
                        (content_aware::filled_at(*rect, data, x, y)?, 1.0)
                    }
                    (None, PNode::Solid { rgba }) => (*rgba, 1.0),
                    (None, PNode::Vector { path, rgba }) => (*rgba, path.coverage(x, y)),
                    _ => return Err(unsupported("plan_shape")),
                };
                let shape = (rgba[3] * coverage * mask * layer.layer_opacity).clamp(0.0, 1.0);
                if shape <= 0.0 {
                    continue;
                }
                let initial = match depth {
                    hsk_studio_pigment::render_plan::Knockout::Deep => {
                        fx.ko.record_deep(at, shape)?;
                        Rgba::TRANSPARENT
                    }
                    _ => fx.ko.shallow_initial(layer.fx.group, at)?,
                };
                let b = *dst.get(at).ok_or_else(overflow)?;
                let out = hsk_studio_pigment::knockout_math::knockout_over(
                    layer.mode,
                    Rgba::new([b[0], b[1], b[2]], b[3]),
                    initial,
                    Rgba::new([rgba[0], rgba[1], rgba[2]], rgba[3]),
                    layer.opacity,
                    shape,
                    PixelSite {
                        x,
                        y,
                        seed: self.prepared.seed,
                    },
                )
                .map_err(blend_error)?;
                *dst.get_mut(at).ok_or_else(overflow)? = [out.rgb[0], out.rgb[1], out.rgb[2], out.a];
            }
        }
        Ok(())
    }

    /// Blend `source(x, y, index) = (straight RGBA, coverage)` into `dst` over `region`. The layer's
    /// mask (feathered over the chunk first when it has a feather, then invert and density) scales
    /// the opacity; its Blend-If weight scales the source alpha.
    fn composite(
        &self,
        layer: &PLayer<'_>,
        region: Rect,
        chunk: Rect,
        dst: &mut [[f32; 4]],
        fx: &mut Fx,
        mut source: impl FnMut(i64, i64, usize) -> Result<([f32; 4], f32), RenderError>,
    ) -> Result<(), RenderError> {
        let blend_if = self.blend_set(layer);
        let feathered = self.feather(layer, region, chunk, fx)?;
        let plane = feathered.then_some(fx.plane.as_slice());
        let (x1, y1) = end(region)?;
        for y in region.y..y1 {
            for x in region.x..x1 {
                let at = index_in(chunk, x, y)?;
                let mask = mask_value(layer, plane, x, y, at)?;
                if mask <= 0.0 {
                    continue;
                }
                let (rgba, coverage) = source(x, y, at)?;
                let opacity = layer.opacity * coverage * mask;
                if opacity <= 0.0 || rgba[3] <= 0.0 {
                    continue;
                }
                let b = *dst.get(at).ok_or_else(overflow)?;
                let mut source_alpha = rgba[3];
                if !blend_if.is_empty() {
                    source_alpha *=
                        blend_weight(blend_if, [rgba[0], rgba[1], rgba[2]], [b[0], b[1], b[2]], b[3])?;
                    if source_alpha <= 0.0 {
                        continue;
                    }
                }
                let out = over_fill(
                    self.prepared.ext.output.is_extended(),
                    layer.mode,
                    Rgba::new([b[0], b[1], b[2]], b[3]),
                    Rgba::new([rgba[0], rgba[1], rgba[2]], source_alpha),
                    opacity,
                    layer.layer_opacity * coverage * mask,
                    layer.fill_opacity,
                    PixelSite {
                        x,
                        y,
                        seed: self.prepared.seed,
                    },
                    self.prepared.ext.output.is_eight_bit(),
                )
                .map_err(blend_error)?;
                // Alpha can round 1 ulp above 1; keep the working buffer inside the blend domain.
                *dst.get_mut(at).ok_or_else(overflow)? =
                    [out.rgb[0], out.rgb[1], out.rgb[2], out.a.min(1.0)];
            }
        }
        Ok(())
    }

    /// Write the clip base into `run` over `region`: `source(x, y, current)` gives the straight RGBA
    /// and coverage (`current` is what `run` already holds, for an isolated-group base); the base's
    /// own mask scales the alpha. Fully transparent pixels are stored as `[0; 4]`.
    fn seal_base(
        &self,
        base: &PLayer<'_>,
        region: Rect,
        chunk: Rect,
        run: &mut [[f32; 4]],
        fx: &mut Fx,
        source: impl Fn(i64, i64, [f32; 4]) -> Result<([f32; 4], f32), RenderError>,
    ) -> Result<(), RenderError> {
        let feathered = self.feather(base, region, chunk, fx)?;
        let plane = feathered.then_some(fx.plane.as_slice());
        let (x1, y1) = end(region)?;
        for y in region.y..y1 {
            for x in region.x..x1 {
                let at = index_in(chunk, x, y)?;
                let mask = mask_value(base, plane, x, y, at)?;
                let current = *run.get(at).ok_or_else(overflow)?;
                let (rgba, coverage) = source(x, y, current)?;
                let alpha = rgba[3] * coverage * mask;
                *run.get_mut(at).ok_or_else(overflow)? = if alpha > 0.0 {
                    [rgba[0], rgba[1], rgba[2], alpha]
                } else {
                    [0.0; 4]
                };
            }
        }
        Ok(())
    }
}

/// Sizes of one worker's scratch: chunk pixels, levels of isolated-group nesting, and the feather
/// scratch (coverage plane and horizontal-pass buffer, in samples; zero without a feathered layer).
#[derive(Clone, Copy)]
struct ScratchShape {
    pixels: usize,
    depth: usize,
    fx_plane: usize,
    fx_tmp: usize,
    /// Effect frame (layers with effects); `None` on the v1 path.
    frame: Option<FrameShape>,
    /// Knockout snapshots and deep shape planes (lane SMB-PS-ADJUST-STYLES-2).
    ko: knockout::KnockoutShape,
}
impl ScratchShape {
    /// v1 shape: no feather scratch.
    #[cfg(test)]
    const fn plain(pixels: usize, depth: usize) -> Self {
        Self {
            pixels,
            depth,
            fx_plane: 0,
            fx_tmp: 0,
            frame: None,
            ko: knockout::KnockoutShape {
                snapshots: 0,
                shapes: 0,
            },
        }
    }
}

/// Per-worker buffers for one chunk: working RGBA, one buffer per level of isolated-group nesting,
/// the feather scratch and the two output planes.
struct Scratch {
    work: Vec<[f32; 4]>,
    spare: Vec<Vec<[f32; 4]>>,
    fx: Fx,
    colour: Vec<u8>,
    alpha: Vec<u8>,
}
impl Scratch {
    fn new(shape: ScratchShape) -> Self {
        let pixels = shape.pixels;
        // Capacity hints only; the scratch charge bounds `pixels` long before this.
        let bytes = |per_pixel: usize| pixels.checked_mul(per_pixel).unwrap_or(0);
        Self {
            work: vec![[0.0; 4]; pixels],
            spare: (0..shape.depth).map(|_| vec![[0.0; 4]; pixels]).collect(),
            fx: {
                let mut fx = Fx::new(shape.fx_plane, shape.fx_tmp, shape.frame);
                fx.ko = knockout::KnockoutScratch::new(shape.ko, pixels);
                fx
            },
            colour: Vec::with_capacity(bytes(12)),
            alpha: Vec::with_capacity(bytes(4)),
        }
    }
}

/// Where chunk `index` (row-major) sits in the extent.
#[derive(Clone, Copy)]
struct Geometry {
    extent: Rect,
    side: i64,
    columns: usize,
    total: usize,
}
impl Geometry {
    fn new(extent: Rect, side: u32) -> Result<Self, RenderError> {
        let side = i64::from(side);
        let count = |length: u32| -> Result<usize, RenderError> {
            // ceil(length / side)
            i64::from(length)
                .checked_add(side)
                .and_then(|n| n.checked_sub(1))
                .and_then(|n| n.checked_div(side))
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(overflow)
        };
        let (columns, rows) = (count(extent.width)?, count(extent.height)?);
        Ok(Self {
            extent,
            side,
            columns,
            total: mul_u(columns, rows)?,
        })
    }
    fn rect(&self, index: usize) -> Result<Rect, RenderError> {
        let column = index.checked_rem(self.columns).ok_or_else(overflow)?;
        let row = index.checked_div(self.columns).ok_or_else(overflow)?;
        let origin = |start: i64, step: usize| -> Result<i64, RenderError> {
            to_i64(step)?
                .checked_mul(self.side)
                .and_then(|n| start.checked_add(n))
                .ok_or_else(overflow)
        };
        let (x, y) = (origin(self.extent.x, column)?, origin(self.extent.y, row)?);
        let (right, bottom) = end(self.extent)?;
        let span = |far: i64, near: i64| -> Result<u32, RenderError> {
            far.checked_sub(near)
                .map(|n| n.min(self.side))
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(overflow)
        };
        Ok(Rect {
            x,
            y,
            width: span(right, x)?,
            height: span(bottom, y)?,
        })
    }
}

/// Evaluate the plan for one chunk into `scratch.colour` / `scratch.alpha`.
fn compute_chunk(
    eval: &Eval<'_, '_>,
    rect: Rect,
    scratch: &mut Scratch,
) -> Result<(), RenderError> {
    let prepared = eval.prepared;
    let count = area(rect)?;
    let work = scratch
        .work
        .get_mut(..count)
        .ok_or_else(|| unsupported("chunk_scratch"))?;
    work.fill([0.0; 4]);
    if matches!(prepared.nodes[prepared.root], PNode::Group { .. }) {
        // The root backdrop is transparent, so isolated and not are the same here.
        eval.group(
            prepared.root,
            rect,
            work,
            &mut scratch.spare,
            &mut scratch.fx,
        )?;
    } else {
        // A leaf root is one Normal, opacity-1 layer over transparent black.
        let root_layer = PLayer {
            input: prepared.root,
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            mask: None,
            clipped: false,
            fx: LayerFx::default(),
            layer_opacity: 1.0,
            fill_opacity: 1.0,
        };
        eval.layer(&root_layer, rect, work, &mut scratch.spare, &mut scratch.fx)?;
    }
    scratch.colour.clear();
    scratch.alpha.clear();
    for [r, g, b, a] in &scratch.work[..count] {
        scratch.colour.extend_from_slice(&r.to_le_bytes());
        scratch.colour.extend_from_slice(&g.to_le_bytes());
        scratch.colour.extend_from_slice(&b.to_le_bytes());
        scratch.alpha.extend_from_slice(&a.to_le_bytes());
    }
    Ok(())
}

/// Cancel, then deadline, as observed before every chunk.
fn between_chunks(
    cancel: &CancellationToken,
    deadline: Option<Instant>,
) -> Result<(), BackendError> {
    cancel.check().map_err(|_| BackendError::Canceled)?;
    if deadline.is_some_and(|d| Instant::now() >= d) {
        return Err(BackendError::DeadlineExceeded);
    }
    Ok(())
}

/// Run `body`; a panic inside it becomes the typed `Unavailable(what)` instead of an unwind (a
/// worker or sink that panics must neither hang a parallel run nor tear down the caller).
fn caught<T>(
    what: &'static str,
    body: impl FnOnce() -> Result<T, RenderError>,
) -> Result<T, RenderError> {
    catch_unwind(AssertUnwindSafe(body))
        .unwrap_or_else(|_| Err(RenderError::Backend(BackendError::Unavailable(what))))
}
/// The only place the caller's sink is invoked: sink errors keep their typed mapping, a sink panic
/// is `Unavailable("sink_panic")`.
fn deliver(
    sink: &mut dyn TileSink,
    rect: Rect,
    colour: &[u8],
    alpha: &[u8],
) -> Result<(), RenderError> {
    caught("sink_panic", || {
        sink.accept(rect, colour, alpha)
            .map_err(|e| RenderError::Backend(BackendError::Sink(e)))
    })
}

/// Evaluates one chunk of the plan into the scratch output planes.
type ChunkFn<'f> = dyn Fn(Rect, &mut Scratch) -> Result<(), RenderError> + Sync + 'f;

enum Message {
    Chunk(usize, Vec<u8>, Vec<u8>),
    Failed(RenderError),
}

/// Stops the workers when dropped, on every exit of the delivering thread including an unwind: a
/// parked worker (waiting for the delivered prefix to advance) would otherwise never be released
/// and `thread::scope` would never return.
struct StopWorkers<'a> {
    abort: &'a AtomicBool,
    gate: &'a (Mutex<usize>, Condvar),
}
impl Drop for StopWorkers<'_> {
    fn drop(&mut self) {
        self.abort.store(true, Ordering::Release);
        self.gate.1.notify_all();
    }
}

impl Renderer {
    pub fn new(chunk: u32) -> Result<Self, RenderError> {
        Self::with_threads(chunk, 1)
    }
    /// `threads > 1` evaluates chunks on that many scoped worker threads. Output bytes and the
    /// sink call order (row-major chunks, cancel and deadline observed before each delivery) are
    /// identical to the single-threaded run; only the scratch charge grows (see
    /// [`Renderer::scratch_bytes`]).
    pub fn with_threads(chunk: u32, threads: usize) -> Result<Self, RenderError> {
        if chunk == 0 || chunk > MAX_CHUNK_SIDE {
            return Err(RenderError::InvalidConfig("chunk"));
        }
        if threads == 0 || threads > MAX_THREADS {
            return Err(RenderError::InvalidConfig("threads"));
        }
        Ok(Self {
            chunk,
            threads,
            limits: PlanLimits::default(),
            display: None,
            live: AtomicU64::new(0),
            peak: AtomicU64::new(0),
        })
    }
    fn with_chunk(chunk: u32) -> Self {
        Self {
            chunk,
            threads: 1,
            limits: PlanLimits::default(),
            display: None,
            live: AtomicU64::new(0),
            peak: AtomicU64::new(0),
        }
    }
    /// Tightens the plan budgets (nodes, layers, nesting depth) that shared admission enforces for
    /// every plan this renderer executes: [`Renderer::execute_detailed`],
    /// [`Renderer::execute_extended_detailed`] and the `RenderBackend` methods that call them hand
    /// `limits` to pigment's `admit_with_limits` / `admit_extended_with_limits`, so a plan above a
    /// tightened budget is refused as `InvalidPlan` before any source lookup or sink call. Limits
    /// only tighten: a field above [`PlanLimits::MAXIMUM`] is `InvalidConfig("plan_limits")`, never
    /// clamped. A renderer that was not given limits uses the maxima, exactly as before (tests
    /// `tests/cpu_plan_limits.rs` `tightened_plan_limits_refuse_before_any_sink_call` and
    /// `default_plan_limits_are_unchanged`).
    pub fn with_plan_limits(mut self, limits: PlanLimits) -> Result<Self, RenderError> {
        limits
            .within_maximum()
            .map_err(|_| RenderError::InvalidConfig("plan_limits"))?;
        self.limits = limits;
        Ok(self)
    }
    /// Deprecated alias (lane SMB-RENDER-SCENE-DISPLAY; the output stage now lives in the plan,
    /// pigment `PlanExtensions::with_output`): a plan executed by this renderer whose envelope has
    /// the default output stage runs as if it declared `OutputStage { Extended, Some(transform) }`
    /// (extended-range composite, then the stage-6a clip); a plan that declares its own stage is
    /// unchanged. The receipt digest is that of the envelope executed (tests
    /// `tests/cpu_scene_display.rs`).
    #[deprecated(note = "declare the output stage in the plan: PlanExtensions::with_output")]
    pub fn with_display_transform(mut self, transform: DisplayTransform) -> Self {
        self.display = Some(transform);
        self
    }
    /// The deprecated alias's display transform (`None` = plans run with their own stage).
    pub fn display_transform(&self) -> Option<DisplayTransform> {
        self.display
    }
    /// The plan budgets this renderer hands to shared admission.
    pub fn plan_limits(&self) -> PlanLimits {
        self.limits
    }
    pub fn chunk(&self) -> u32 {
        self.chunk
    }
    pub fn threads(&self) -> usize {
        self.threads
    }
    /// Scratch bytes currently charged; 0 when no render is running.
    pub fn live_scratch_bytes(&self) -> u64 {
        self.live.load(Ordering::Acquire)
    }
    /// Highest simultaneous charge over this renderer's lifetime.
    pub fn peak_scratch_bytes(&self) -> u64 {
        self.peak.load(Ordering::Acquire)
    }
    /// Scratch charged for chunks of at most `pixels` pixels, isolated-group nesting `depth` and
    /// `workers` workers: every worker owns `32 + 16 * depth` bytes per pixel, and a parallel run
    /// also holds up to `2 * workers` finished chunks (`16` bytes per pixel) awaiting in-order
    /// delivery.
    pub fn scratch_bytes(pixels: u64, depth: u64, workers: u64) -> Option<u64> {
        let per_worker = GROUP_SCRATCH_BYTES_PER_PIXEL
            .checked_mul(depth)?
            .checked_add(SCRATCH_BYTES_PER_PIXEL)?;
        // `2 * workers` finished chunks of 16 bytes per pixel.
        let in_flight = if workers > 1 {
            workers.checked_mul(32)?
        } else {
            0
        };
        pixels.checked_mul(workers.checked_mul(per_worker)?.checked_add(in_flight)?)
    }

    fn reserve(&self, bytes: u64, limit: u64) -> Result<ScratchLease<'_>, RenderError> {
        if bytes > limit {
            return Err(BackendError::BudgetExceeded {
                requested: bytes,
                limit,
            }
            .into());
        }
        let live = self
            .live
            .fetch_add(bytes, Ordering::AcqRel)
            .saturating_add(bytes);
        self.peak.fetch_max(live, Ordering::AcqRel);
        Ok(ScratchLease { owner: self, bytes })
    }

    /// Execute with the detailed internal error. Order: admission (cancel, stale revision, plan
    /// validation, per-operator exactness), source verification (so a bad plan or source never
    /// reaches the sink), then chunks. Cancel and deadline are observed before every chunk. A
    /// receipt exists only after the last chunk was accepted. A panic in chunk evaluation or in the
    /// sink is returned as `Unavailable("worker_panic" | "sink_panic")` on every thread count: it
    /// never hangs the run and never unwinds into the caller.
    pub fn execute_detailed<V: VectorSource + Serialize + Send + Sync>(
        &self,
        plan: &RenderPlan<V>,
        request: &ExecuteRequest,
        sources: &dyn PlanSources,
        cancel: &CancellationToken,
        sink: &mut dyn TileSink,
    ) -> Result<RenderReceipt, RenderError> {
        if self.display.is_some() {
            return self.execute_extended_detailed(
                plan,
                &PlanExtensions::default(),
                request,
                sources,
                cancel,
                sink,
            );
        }
        let ledger = admit_with_limits::<V>(plan, &self.limits, self, request, cancel)?;
        self.run(
            plan,
            ExtPrep::default(),
            ledger,
            &|| plan.digest(),
            request,
            sources,
            cancel,
            sink,
        )
    }

    /// [`Renderer::execute_detailed`] for a plan with its envelope: adjustment layers, Blend-If,
    /// mask feather and mask density (`tests/cpu_extended.rs`). Order: `admit_extended` (shared
    /// admission; knockout and unsupported adjustments are refused before any work), envelope
    /// preparation (resident state charged to `max_working_bytes` as a running aggregate, for the
    /// entries of groups the root reaches only: an entry on any other group renders nothing and
    /// asks nothing, `tests/cpu_envelope_reach.rs`
    /// `unreachable_envelope_entries_are_neither_admitted_nor_prepared`), source
    /// verification, then the same chunked loop. Admission uses the plan budgets of
    /// [`Renderer::with_plan_limits`] (the maxima by default). An empty envelope is the plain v1 plan and takes
    /// the unchanged v1 path, except that a coverage mask with `density < 1` (no v1 operator) goes
    /// through the envelope admission, which admits it as `mask_density`.
    pub fn execute_extended_detailed<V: VectorSource + Serialize + Send + Sync>(
        &self,
        plan: &RenderPlan<V>,
        extensions: &PlanExtensions,
        request: &ExecuteRequest,
        sources: &dyn PlanSources,
        cancel: &CancellationToken,
        sink: &mut dyn TileSink,
    ) -> Result<RenderReceipt, RenderError> {
        let alias = display::aliased(extensions, self.display);
        let extensions = alias.as_ref().unwrap_or(extensions);
        if extensions.is_empty() && !plan.has_mask_density() {
            return self.execute_detailed(plan, request, sources, cancel, sink);
        }
        let ledger = admit_extended_with_limits::<V, Self>(
            plan,
            extensions,
            &self.limits,
            self,
            request,
            cancel,
        )?;
        // The plan passed admission, so its indexes are valid. Only the entries of groups the root
        // reaches are prepared, like the layers `prepare` visits: an entry on any other group
        // renders nothing, so it charges no resident bytes and cannot be refused here.
        let reach = reachable(plan);
        let ext = ExtPrep::build(extensions, &reach, request.max_working_bytes)?;
        self.run(
            plan,
            ext,
            ledger,
            &|| extensions.digest_with(plan),
            request,
            sources,
            cancel,
            sink,
        )
    }

    /// Prepared-plan execution shared by the v1 and the envelope path (admission already done).
    #[allow(clippy::too_many_arguments)]
    fn run<V: VectorSource>(
        &self,
        plan: &RenderPlan<V>,
        ext: ExtPrep,
        ledger: ExactnessLedger,
        digest: &dyn Fn() -> Result<[u8; 32], hsk_studio_pigment::Error>,
        request: &ExecuteRequest,
        sources: &dyn PlanSources,
        cancel: &CancellationToken,
        sink: &mut dyn TileSink,
    ) -> Result<RenderReceipt, RenderError> {
        let mut prepared = prepare(plan, sources, ext)?;
        let digest = digest().map_err(|_| BackendError::Unavailable("plan_digest"))?;

        let extent = plan.target.extent;
        end(extent)?;
        let geometry = Geometry::new(extent, self.chunk)?;
        let workers = self.threads.min(geometry.total);
        let (side_x, side_y) = (self.chunk.min(extent.width), self.chunk.min(extent.height));
        let max_pixels = u64::from(side_x)
            .checked_mul(u64::from(side_y))
            .ok_or_else(overflow)?;
        let depth = prepared.need[prepared.root];
        let base = Self::scratch_bytes(max_pixels, depth as u64, workers as u64)
            .ok_or(BackendError::Overflow)?;
        // Envelope charge: the resident tables once, the feather scratch (chunk plane plus the
        // horizontal pass with its `feather_support` margin rows) per worker. Zero on the v1 path.
        let (columns, rows) = (side_len(side_x)?, side_len(side_y)?);
        let (fx_plane, fx_tmp) = prepared.ext.feather_dims(columns, rows)?;
        let frame = prepared.ext.frame_shape(columns, rows)?;
        let frame_bytes = match &frame {
            Some(shape) => prepared.ext.frame_scratch_bytes(shape)?,
            None => 0,
        };
        // Knockout scratch (lane SMB-PS-ADJUST-STYLES-2): one start snapshot per group holding a
        // shallow knockout (an upper bound of the pass-through nesting) and, with a deep knockout,
        // one shape plane per level of isolated-group nesting, per worker.
        let ko = knockout::KnockoutShape {
            snapshots: prepared.ext.knockout_groups,
            shapes: if prepared.ext.knockout_deep {
                depth.saturating_add(1)
            } else {
                0
            },
        };
        let ko_bytes = ko.bytes(max_pixels)?;
        let scratch_bytes = prepared
            .ext
            .feather_scratch_bytes(columns, rows)?
            .checked_add(frame_bytes)
            .and_then(|b| b.checked_add(ko_bytes))
            .and_then(|per_worker| per_worker.checked_mul(workers as u64))
            .and_then(|fx| fx.checked_add(prepared.ext.resident_bytes))
            .and_then(|fx| fx.checked_add(base))
            .ok_or(BackendError::Overflow)?;
        let _lease = self.reserve(scratch_bytes, request.max_working_bytes)?;
        // Content-aware fills (lane SMB-RENDER-CONTENT-AWARE): their planes are charged next to the
        // chunk scratch, pigment gets what remains of the budget; all before the first sink call.
        let fill_bytes = content_aware::fill_bytes(&prepared)?;
        let remaining = request
            .max_working_bytes
            .saturating_sub(scratch_bytes);
        let _fill_lease = self.reserve(fill_bytes, remaining)?;
        if !prepared.fills.is_empty() {
            content_aware::run_fills(&mut prepared, remaining.saturating_sub(fill_bytes), cancel)?;
        }
        let scratch_bytes = scratch_bytes.checked_add(fill_bytes).ok_or(BackendError::Overflow)?;
        // Key pipelines (lane SMB-AE-KEY-TRACK-2): their planes and pigment's predicted pipeline
        // bytes are charged next to the scratch and fills, all before the first sink call.
        let key_bytes = key::key_bytes(&prepared)?;
        let key_pigment = key::pigment_bytes(&prepared)?;
        let remaining = request.max_working_bytes.saturating_sub(scratch_bytes);
        let key_total = key_bytes.checked_add(key_pigment).ok_or(BackendError::Overflow)?;
        let _key_lease = self.reserve(key_total, remaining)?;
        if !prepared.keys.is_empty() {
            key::run_keys(&mut prepared, key_pigment, cancel)?;
        }
        let scratch_bytes = scratch_bytes.checked_add(key_total).ok_or(BackendError::Overflow)?;
        // Smart-filter stacks (lane SMB-PS-SELECT-LIQUIFY-2): their planes are charged next to the
        // scratch, fills and keys before the first sink call; pigment gets what remains.
        let smart_bytes = smart_filter::smart_bytes(&prepared)?;
        let remaining = request.max_working_bytes.saturating_sub(scratch_bytes);
        let _smart_lease = self.reserve(smart_bytes, remaining)?;
        if !prepared.smart.is_empty() {
            smart_filter::run_smart(&mut prepared, remaining.saturating_sub(smart_bytes), cancel)?;
        }
        let scratch_bytes = scratch_bytes.checked_add(smart_bytes).ok_or(BackendError::Overflow)?;
        let eval = Eval {
            prepared: &prepared,
        };
        // Stage 6a (output stage `display: Some(Clip)`): the composite's colour clamped into [0, 1]
        // before the chunk reaches the sink.
        let clip = prepared.ext.output.display == Some(DisplayTransform::Clip);
        // Stage 6a view transform (output stage `display: Some(View(id))`, lane
        // SMB-PRISM-VIEW-TRANSFORM): Prism's per-pixel view on the composite's colour.
        let view = prepared.ext.output.display.and_then(|d| d.view());
        let compute = |rect: Rect, scratch: &mut Scratch| {
            compute_chunk(&eval, rect, scratch)?;
            if clip {
                display::clip_chunk(&mut scratch.colour)?;
            }
            if let Some(id) = view {
                display::view_chunk(&mut scratch.colour, id)?;
            }
            Ok(())
        };
        let shape = ScratchShape {
            pixels: max_pixels as usize,
            depth,
            fx_plane,
            fx_tmp,
            frame,
            ko,
        };

        let chunks = if workers == 1 {
            let mut scratch = Scratch::new(shape);
            for index in 0..geometry.total {
                between_chunks(cancel, request.deadline)?;
                let rect = geometry.rect(index)?;
                caught("worker_panic", || compute(rect, &mut scratch))?;
                deliver(sink, rect, &scratch.colour, &scratch.alpha)?;
            }
            geometry.total as u64
        } else {
            run_parallel(
                &compute,
                geometry,
                workers,
                shape,
                request.deadline,
                cancel,
                sink,
            )?
        };
        Ok(RenderReceipt {
            plan_revision: plan.revision,
            plan_digest: digest,
            backend: identity(),
            region: extent,
            target: plan.target,
            exactness: ledger,
            chunks,
            nodes_executed: prepared.reachable,
            peak_working_bytes: scratch_bytes,
        })
    }
}

/// A chunk side (at most `MAX_CHUNK_SIDE`) as a sample count.
fn side_len(side: u32) -> Result<usize, RenderError> {
    usize::try_from(side).map_err(|_| overflow())
}

/// Workers claim chunk indices in increasing order and may run at most `2 * workers` chunks ahead
/// of delivery; the calling thread reorders results and is the only one that touches `sink`, so the
/// sink sees row-major chunks exactly as in the single-threaded run. A panicking worker is reported
/// as `Message::Failed`, a panicking sink as a typed error, and `StopWorkers` releases the parked
/// workers however the delivering side exits, so the run always terminates.
#[allow(clippy::too_many_arguments)]
fn run_parallel(
    compute: &ChunkFn<'_>,
    geometry: Geometry,
    workers: usize,
    shape: ScratchShape,
    deadline: Option<Instant>,
    cancel: &CancellationToken,
    sink: &mut dyn TileSink,
) -> Result<u64, RenderError> {
    let window = workers.saturating_mul(2);
    // Capacity hints only; the scratch charge bounds `shape.pixels` long before this.
    let hint = |per_pixel: usize| shape.pixels.checked_mul(per_pixel).unwrap_or(0);
    let next = AtomicUsize::new(0);
    let abort = AtomicBool::new(false);
    let gate = (Mutex::new(0_usize), Condvar::new());
    let (tx, rx) = mpsc::channel::<Message>();
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let tx = tx.clone();
            let (next, abort, gate) = (&next, &abort, &gate);
            scope.spawn(move || {
                let crashed = catch_unwind(AssertUnwindSafe(|| {
                    let mut scratch = Scratch::new(shape);
                    loop {
                        let index = next.fetch_add(1, Ordering::AcqRel);
                        if index >= geometry.total || abort.load(Ordering::Acquire) {
                            return;
                        }
                        // Stay within the reorder window of the delivered prefix.
                        {
                            let (delivered, wake) = gate;
                            let mut guard =
                                delivered.lock().unwrap_or_else(PoisonError::into_inner);
                            while index >= guard.saturating_add(window)
                                && !abort.load(Ordering::Acquire)
                            {
                                guard = wake
                                    .wait_timeout(guard, Duration::from_millis(20))
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .0;
                            }
                        }
                        if abort.load(Ordering::Acquire) {
                            return;
                        }
                        let outcome = between_chunks(cancel, deadline)
                            .map_err(RenderError::from)
                            .and_then(|()| compute(geometry.rect(index)?, &mut scratch));
                        let message = match outcome {
                            Ok(()) => Message::Chunk(
                                index,
                                std::mem::replace(
                                    &mut scratch.colour,
                                    Vec::with_capacity(hint(12)),
                                ),
                                std::mem::replace(&mut scratch.alpha, Vec::with_capacity(hint(4))),
                            ),
                            Err(error) => Message::Failed(error),
                        };
                        let failed = matches!(message, Message::Failed(_));
                        if tx.send(message).is_err() || failed {
                            return;
                        }
                    }
                }))
                .is_err();
                if crashed {
                    // Its chunk will never arrive: say so, and release the parked workers.
                    abort.store(true, Ordering::Release);
                    gate.1.notify_all();
                    let _ = tx.send(Message::Failed(RenderError::Backend(
                        BackendError::Unavailable("worker_panic"),
                    )));
                }
            });
        }
        drop(tx);
        let _stop = StopWorkers {
            abort: &abort,
            gate: &gate,
        };

        let mut pending: BTreeMap<usize, (Vec<u8>, Vec<u8>)> = BTreeMap::new();
        let mut delivered = 0_usize;
        'deliver: loop {
            if delivered == geometry.total {
                break Ok(delivered as u64);
            }
            match rx.recv() {
                Ok(Message::Chunk(index, colour, alpha)) => {
                    pending.insert(index, (colour, alpha));
                    while let Some((colour, alpha)) = pending.remove(&delivered) {
                        if let Err(error) = between_chunks(cancel, deadline) {
                            break 'deliver Err(error.into());
                        }
                        let rect = match geometry.rect(delivered) {
                            Ok(rect) => rect,
                            Err(error) => break 'deliver Err(error),
                        };
                        if let Err(error) = deliver(sink, rect, &colour, &alpha) {
                            break 'deliver Err(error);
                        }
                        delivered = delivered.saturating_add(1);
                        *gate.0.lock().unwrap_or_else(PoisonError::into_inner) = delivered;
                        gate.1.notify_all();
                    }
                }
                Ok(Message::Failed(error)) => break Err(error),
                Err(_) => break Err(BackendError::Unavailable("worker_lost").into()),
            }
        }
    })
}

fn identity() -> BackendIdentity {
    BackendIdentity {
        name: RENDERER_NAME,
        version: RENDERER_VERSION,
        device: DeviceClass::Cpu,
        adapter: String::new(),
        generation: 0,
    }
}

impl<V: VectorSource + Serialize + Send + Sync> RenderBackend<V> for Renderer {
    fn identity(&self) -> BackendIdentity {
        identity()
    }
    fn exactness(&self, operator: Operator) -> Exactness {
        match operator {
            // Pigment's run semantics are implemented as documented; the Photoshop rule is
            // UNVERIFIED, so the class stays Approximate.
            Operator::Mask(MaskFormat::BaseAlpha) => Exactness::Approximate,
            // The coverage planes `prepare` has a reader for (the match on `m.format` there). The
            // formats are named, not covered by a wildcard: a format pigment adds later has no
            // reader here, so it must be Unsupported at shared admission (the last arm) instead of
            // Exact there and refused afterwards by the local mask-format check (R8-review-a;
            // pigment tripwire `every_enum_a_backend_refuses_on_has_an_admission_decision`).
            Operator::Tile
            | Operator::Solid
            | Operator::IsolatedGroup
            | Operator::PassThroughGroup
            | Operator::Mask(MaskFormat::CoverageU8 | MaskFormat::CoverageU16Le) => {
                Exactness::Exact
            }
            Operator::Mask(_) => Exactness::Unsupported,
            // Supersampled coverage approximates area coverage; exact only on sample-aligned edges.
            Operator::Vector | Operator::FillOpacity => Exactness::Approximate,
            Operator::Blend(mode) => {
                if blend_supported(mode) {
                    mode.facts().exactness
                } else {
                    Exactness::Unsupported
                }
            }
            // Lane SMB-PS-ORACLE-PARITY-2: pigment `pass_through_mix` (`Eval::pass_through_weighed`).
            Operator::PassThroughOpacity => operator.intrinsic(),
            Operator::NonIsolatedBlendedGroup => Exactness::Unsupported,
        }
    }
    fn supports_clip_to_base(&self) -> bool {
        true
    }
    /// The envelope operators implement pigment's formulas verbatim (`adjust_math`,
    /// `coverage_math`, `effect_math`) and pigment owns their exactness (CX-CAP-001): a supported
    /// operator reports exactly pigment's declared class (`ExtOperator::intrinsic`, Approximate for
    /// everything but Invert), not a better one (tests cpu_extended
    /// `refusals_stay_typed_and_the_v1_path_is_unchanged`, cpu_effects
    /// `effect_refusals_and_scratch_charge_stay_typed`, cpu_effects_fx2
    /// `wave2_exactness_refusals_and_budget`); bit-identity with pigment's reference is a separate
    /// fact (the parity tests). Knockout, Blend-If on a clip base or a clipped layer
    /// (`BlendIfClipBase`, `BlendIfClippedLayer`: no definition exists, test cpu_extended
    /// `blend_if_on_clip_roles_is_refused_before_any_sink_call`), effect kinds pigment adds after
    /// waves 1 and 2 and any operator this build does not know are Unsupported (typed refusal at
    /// admission).
    fn extension_exactness(&self, operator: ExtOperator) -> Exactness {
        match operator {
            ExtOperator::AdjustmentLayer
            | ExtOperator::Adjustment(_)
            | ExtOperator::BlendIf
            | ExtOperator::MaskDensity
            | ExtOperator::MaskFeather
            | ExtOperator::Effect(
                EffectKind::DropShadow
                | EffectKind::ColourOverlay
                | EffectKind::Stroke
                | EffectKind::InnerShadow
                | EffectKind::OuterGlow
                | EffectKind::InnerGlow
                // Lane SMB-PS-ADJUST-STYLES: evaluated by pigment's windowed style functions like
                // every other kind (tests/cpu_layer_styles.rs).
                | EffectKind::Satin
                | EffectKind::BevelEmboss
                | EffectKind::GradientOverlay
                // Lane SMB-PS-ADJUST-STYLES-2: the pattern pixels are a plan tile source decoded
                // in `prepare` (tests/cpu_pattern_overlay.rs).
                | EffectKind::PatternOverlay,
            )
            // Knockout: pigment `knockout_over` per pixel (`Eval::knockout_layer`); shallow lane
            // SMB-PS-ADJUST-STYLES (tests/cpu_knockout.rs), deep and the root / pass-through
            // placements lane SMB-PS-ADJUST-STYLES-2 (`knockout` module, tests/cpu_knockout_2.rs).
            | ExtOperator::Knockout(_) => operator.intrinsic(),
            // Declared Unsupported on purpose (lane SMB-FX-OPS-ADOPT), so shared admission refuses
            // them under pigment's keys before any sink call: a layer with effects whose content
            // holds a layer with effects (the chunked compositor has one effect frame per chunk
            // and refuses the nesting), and a premultiplied alpha convention at the target or a
            // tile input (this backend works on straight alpha and does not convert).
            ExtOperator::EffectsNested | ExtOperator::AlphaPremultiplied => Exactness::Unsupported,
            // Layer transforms (lane SMB-PIGMENT-TRANSFORM): every filter is evaluated by pigment's
            // binary64 reference sampler, so the bytes are pigment's (tests/cpu_transform.rs).
            ExtOperator::Transform(_) => operator.intrinsic(),
            // Layer and backdrop blur (lane SMB-COMPOSITE-NEW-KINDS): pigment's `blur_window` and
            // `backdrop_mix` evaluate them, so the bytes are pigment's (tests/cpu_blur.rs).
            ExtOperator::Blur(_) => operator.intrinsic(),
            // Layer filters (lane SMB-RENDER-FILTERS): pigment's `filter_window` evaluates every
            // kind, so the bytes are pigment's (tests/cpu_filters.rs).
            ExtOperator::Filter(_) => operator.intrinsic(),
            // Content-aware fill (lane SMB-RENDER-CONTENT-AWARE): pigment's `content_aware::fill`
            // synthesizes the whole layer content once, so the bytes are pigment's
            // (tests/cpu_content_aware.rs).
            ExtOperator::ContentAwareFill => operator.intrinsic(),
            // Key pipelines (lane SMB-AE-KEY-TRACK-2): pigment's `keying::run_key_pipeline` keys
            // the whole layer content once, so the bytes are pigment's (tests/cpu_keying.rs).
            ExtOperator::Key(_) => operator.intrinsic(),
            // Smart-filter stacks (lane SMB-PS-SELECT-LIQUIFY-2): pigment's
            // `SmartFilterStack::render` filters the whole layer content once, so the bytes are
            // pigment's (tests/cpu_smart_filter.rs).
            ExtOperator::SmartFilter => operator.intrinsic(),
            // Output stage (lane SMB-PIGMENT-EXTENDED-RANGE): extended range composites through
            // pigment's `composite_over_extended`, the clip is `f32::clamp` on the delivered chunk
            // (tests/cpu_extended_range.rs).
            ExtOperator::ExtendedRange | ExtOperator::DisplayClip => operator.intrinsic(),
            // Stage-6a view transform (lane SMB-PRISM-VIEW-TRANSFORM): Prism's
            // `ViewTransformId::apply` on the delivered chunk (tests/cpu_view_transform.rs).
            ExtOperator::DisplayView => operator.intrinsic(),
            // 8-bit working depth (lane SMB-PS-ORACLE-PARITY-2): `Eval::store_depth` after every
            // layer and pigment's 8-bit Levels (`PreparedAdjustment::with_eight_bit`).
            ExtOperator::WorkingDepthEight => operator.intrinsic(),
            // Envelope operations in an extended-range plan (lane SMB-PIGMENT-EXTENDED-RANGE-2):
            // the defined ones run through pigment's `*_in(SampleRange::Extended, ..)` functions
            // (tests/cpu_extended_envelope.rs); the undefined ones are intrinsically unsupported.
            ExtOperator::ExtendedEnvelope(op) if op.defined() => operator.intrinsic(),
            // Effect kinds added after wave 2 are not evaluated or tested here yet.
            _ => Exactness::Unsupported,
        }
    }
    fn execute_extended(
        &self,
        plan: &RenderPlan<V>,
        extensions: &PlanExtensions,
        request: &ExecuteRequest,
        sources: &dyn PlanSources,
        cancel: &CancellationToken,
        sink: &mut dyn TileSink,
    ) -> Result<RenderReceipt, BackendError> {
        self.execute_extended_detailed(plan, extensions, request, sources, cancel, sink)
            .map_err(BackendError::from)
    }
    fn execute(
        &self,
        plan: &RenderPlan<V>,
        request: &ExecuteRequest,
        sources: &dyn PlanSources,
        cancel: &CancellationToken,
        sink: &mut dyn TileSink,
    ) -> Result<RenderReceipt, BackendError> {
        self.execute_detailed(plan, request, sources, cancel, sink)
            .map_err(BackendError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hsk_studio_pigment::render_plan::SinkError;
    use std::sync::mpsc::RecvTimeoutError;

    struct Rects(Vec<Rect>);
    impl TileSink for Rects {
        fn accept(&mut self, rect: Rect, _colour: &[u8], _alpha: &[u8]) -> Result<(), SinkError> {
            self.0.push(rect);
            Ok(())
        }
    }

    /// R3: a worker that panics never sends its chunk, so the delivered prefix stopped advancing,
    /// the other workers parked at the reorder window and the receiver waited forever. The panic
    /// is now caught in the worker and reported; the run returns the typed error.
    #[test]
    fn panicking_worker_is_a_typed_error_and_never_hangs() {
        for workers in [2_usize, 3, 4] {
            let (done, result) = mpsc::channel();
            std::thread::spawn(move || {
                // 16 one-pixel chunks; chunk 3 panics.
                let geometry = Geometry::new(
                    Rect {
                        x: 0,
                        y: 0,
                        width: 16,
                        height: 1,
                    },
                    1,
                )
                .unwrap();
                let compute = |rect: Rect, scratch: &mut Scratch| {
                    assert_ne!(rect.x, 3, "injected worker panic");
                    scratch.colour = vec![0; 12];
                    scratch.alpha = vec![0; 4];
                    Ok(())
                };
                let mut sink = Rects(Vec::new());
                let outcome = run_parallel(
                    &compute,
                    geometry,
                    workers,
                    ScratchShape::plain(1, 0),
                    None,
                    &CancellationToken::default(),
                    &mut sink,
                );
                let _ = done.send((outcome, sink.0));
            });
            let (outcome, delivered) = match result.recv_timeout(Duration::from_secs(20)) {
                Ok(r) => r,
                Err(RecvTimeoutError::Timeout) => panic!("{workers} workers: run hung"),
                Err(RecvTimeoutError::Disconnected) => panic!("{workers} workers: run unwound"),
            };
            assert_eq!(
                outcome,
                Err(RenderError::Backend(BackendError::Unavailable(
                    "worker_panic"
                ))),
                "{workers} workers"
            );
            // Chunk 3 never completed, so only the in-order prefix 0..3 can have been delivered.
            assert!(delivered.len() <= 3, "{delivered:?}");
            for (i, rect) in delivered.iter().enumerate() {
                assert_eq!(rect.x, i as i64, "in-order prefix");
            }
        }
    }
}
