//! Content-aware fill on the CPU backend (lane SMB-RENDER-CONTENT-AWARE): the envelope operator
//! `LayerExtension::content_aware_fill` (pigment `ContentAwareFillOp`, STU-RAS-143). The synthesis
//! is pigment's single owner `content_aware::fill` (CX-CAP-001): render-cpu calls it once per layer
//! and keeps no patch search, voting, pyramid or blend of its own.
//!
//! Evaluation. A content-aware fill is global: every hole pixel may take its patches from anywhere
//! in the sampling area, so it cannot be evaluated per chunk. Before the first chunk, `run` renders
//! each filled layer's content (its input node, WITHOUT the layer mask, transparent beyond the
//! content) over the content bounds inside the plan extent `B`, hands that plane with the hole
//! (the `hole` mask source as a coverage plane) and the sampling area (`None` = auto, `Some(rect)` =
//! rectangular) to `content_aware::fill`, and stores pigment's output plane as a node of
//! `Prepared::extra` that the layer reads instead of its input. The chunked compositor then
//! applies the layer mask, Blend-If, mode and `opacity * fill` as for any layer. So the bytes are
//! pigment's fill of the whole content composited by `blend_math::composite_over`, for every
//! chunking and thread count (tests/cpu_content_aware.rs); pigment's bytes do not depend on its own
//! `threads` option either.
//!
//! Hole coverage (a render-cpu reading, no pigment conversion exists): a `coverage_u16le` sample is
//! taken as is, a `coverage_u8` sample `v` as `v * 257` (0 -> 0, 255 -> 65535); `inverted` gives
//! `65535 - c`. Pigment clips the hole to the content plane.
//!
//! Resource rules (V4-01/V4-02): the content plane, its group levels and feather scratch, the hole
//! plane and every stored output plane are charged to `max_working_bytes` (after the chunk scratch)
//! before they are allocated; the remaining budget is pigment's `RetouchLimits::max_bytes`, which
//! pigment charges with its predicted pyramid bytes before allocating, and its work bound is
//! pigment's default `RetouchLimits::max_work`. Refusals are typed before the first sink call: a
//! filled layer whose content holds a layer with effects, a blur, filters or another fill is
//! [`CONTENT_AWARE_NESTED`]; pigment's refusals keep pigment's codes (hole too large, no source,
//! option unsupported), its option range error is `InvalidPlan(ContentAwareParameter)` and its
//! budget error is `BudgetExceeded`.
#![deny(clippy::arithmetic_side_effects)]
use super::extended::{LayerFx, ExtPrep, Fx};
use super::{Eval, MaskView, PLayer, PNode, Prepared, RenderError, SourceFault, source, unsupported};
use crate::px::{area, end, height_of, mul_u, overflow, width_of};
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::content_aware::{SamplingArea, fill_in};
use hsk_studio_pigment::render_plan::{
    BackendError, ContentAwareFillOp, MaskFormat, MaskInput, PlanError, PlanSources,
    SAMPLE_RANGE_OPERATOR, StudioBlendMode,
};
use hsk_studio_pigment::retouch::{RetouchError, RetouchLimits, RgbaPlane};
use hsk_studio_pigment::selection::CoveragePlane;

/// Refusal key: a filled layer whose content holds a layer with effects, a blur, filters or
/// another content-aware fill (the content is rendered once over the whole layer, without the
/// chunk-level frame those need).
pub(super) const CONTENT_AWARE_NESTED: &str = "content_aware_nested";
/// Bytes of one content / output pixel (straight RGBA `f32`).
const PIXEL_BYTES: u64 = 16;
/// Bytes of one hole coverage sample (`u16`).
const HOLE_SAMPLE_BYTES: u64 = 2;
/// Bytes of one feather scratch sample (`f32`).
const FEATHER_SAMPLE_BYTES: u64 = 4;

/// One layer to fill before the first chunk.
pub(super) struct FillJob<'a> {
    /// Index of the layer's output node in the `Prepared::extra` vector (the layer reads it as
    /// node `nodes.len() + extra`).
    pub(super) extra: usize,
    /// The layer's content node.
    pub(super) input: usize,
    /// Content bounds inside the plan extent (the plane handed to pigment).
    pub(super) bounds: Rect,
    pub(super) hole: MaskView<'a>,
    pub(super) op: ContentAwareFillOp,
    pub(super) node: u32,
}

/// The `hole` mask source as a view (same checks as a layer mask: declared format, hash, bounds,
/// stride). Pigment validated density 1 and the coverage formats.
pub(super) fn hole_view<'a>(
    sources: &'a dyn PlanSources,
    hole: &MaskInput,
    node: u32,
) -> Result<MaskView<'a>, RenderError> {
    let wide = match hole.format {
        MaskFormat::CoverageU8 => false,
        MaskFormat::CoverageU16Le => true,
        _ => return Err(parameter(node)),
    };
    if hole.density != 1.0 {
        return Err(parameter(node));
    }
    let plane = sources
        .mask(hole.key)
        .ok_or(BackendError::UnresolvedSource(hole.key))?;
    if plane.sample_format != hole.format.key()
        || plane.sha256 != hole.sha256
        || plane.bounds != hole.placement
    {
        return Err(source(hole.key, SourceFault::Declared));
    }
    plane.validate(plane.bounds).map_err(|e| {
        source(
            hole.key,
            if e == hsk_studio_pigment::Error::HashMismatch {
                SourceFault::HashMismatch
            } else {
                SourceFault::MalformedStride
            },
        )
    })?;
    Ok(MaskView {
        bounds: plane.bounds,
        wide,
        bytes: plane.bytes,
        stride: usize::try_from(plane.stride_bytes).map_err(|_| overflow())?,
        inverted: hole.inverted,
        density: 1.0,
        xform: None,
    })
}

fn parameter(node: u32) -> RenderError {
    RenderError::Backend(BackendError::InvalidPlan(PlanError::ContentAwareParameter { node }))
}

/// Typed mapping of a pigment fill refusal (pigment's codes; `BudgetExceeded` for its byte or work
/// budget, with `limit` = the bytes pigment was given and `requested` one above it: pigment does
/// not report its prediction).
fn fill_error(error: RetouchError, node: u32, limit: u64) -> RenderError {
    match error {
        RetouchError::OutOfRange => parameter(node),
        RetouchError::OutOfDomain => unsupported(SAMPLE_RANGE_OPERATOR),
        RetouchError::Pigment(hsk_studio_pigment::Error::BudgetExceeded) => {
            RenderError::Backend(BackendError::BudgetExceeded {
                requested: limit.saturating_add(1),
                limit,
            })
        }
        RetouchError::Pigment(hsk_studio_pigment::Error::Overflow) => overflow(),
        other => unsupported(other.code()),
    }
}

/// The hole view as pigment's coverage plane (module docs: `u8 * 257`, inverted `65535 - c`).
fn hole_plane(hole: &MaskView<'_>) -> Result<CoveragePlane, RenderError> {
    let (w, h) = (width_of(hole.bounds)?, height_of(hole.bounds)?);
    let mut data = Vec::with_capacity(mul_u(w, h)?);
    let size = if hole.wide { 2 } else { 1 };
    for row in 0..h {
        let start = mul_u(row, hole.stride)?;
        for column in 0..w {
            let at = mul_u(column, size).and_then(|c| crate::px::add_u(start, c))?;
            let stored = if hole.wide {
                let end = at.checked_add(2).ok_or_else(overflow)?;
                let Some(&[lo, hi]) = hole.bytes.get(at..end) else {
                    return Err(overflow());
                };
                u16::from_le_bytes([lo, hi])
            } else {
                let byte = *hole.bytes.get(at).ok_or_else(overflow)?;
                u16::from(byte).checked_mul(257).ok_or_else(overflow)?
            };
            data.push(if hole.inverted { u16::MAX.saturating_sub(stored) } else { stored });
        }
    }
    Ok(CoveragePlane { bounds: hole.bounds, data })
}

/// Bytes of one job: `(transient, stored)`: the content plane, its group levels and feather
/// scratch and the hole plane (freed after the fill), and the output plane kept for the run.
fn job_bytes(prepared: &Prepared<'_>, ext: &ExtPrep, job: &FillJob<'_>) -> Result<(u64, u64), RenderError> {
    let to_u64 = |n: usize| u64::try_from(n).map_err(|_| overflow());
    let pixels = to_u64(area(job.bounds)?)?;
    let plane = pixels.checked_mul(PIXEL_BYTES).ok_or_else(overflow)?;
    let levels = to_u64(prepared.need.get(job.input).copied().ok_or_else(overflow)?)?
        .checked_add(1)
        .ok_or_else(overflow)?;
    let (fp, ft) = ext.feather_dims(width_of(job.bounds)?, height_of(job.bounds)?)?;
    let feather = to_u64(crate::px::add_u(fp, ft)?)?
        .checked_mul(FEATHER_SAMPLE_BYTES)
        .ok_or_else(overflow)?;
    let hole = to_u64(area(job.hole.bounds)?)?
        .checked_mul(HOLE_SAMPLE_BYTES)
        .ok_or_else(overflow)?;
    let transient = plane
        .checked_mul(levels)
        .and_then(|b| b.checked_add(feather))
        .and_then(|b| b.checked_add(hole))
        .ok_or_else(overflow)?;
    Ok((transient, plane))
}

/// Bytes the fills of a run need next to the chunk scratch: every stored output plane plus the
/// largest transient set (the jobs run one after the other). Zero without fills.
pub(super) fn fill_bytes(prepared: &Prepared<'_>) -> Result<u64, RenderError> {
    let (mut transient, mut stored) = (0_u64, 0_u64);
    for job in &prepared.fills {
        let (t, s) = job_bytes(prepared, &prepared.ext, job)?;
        transient = transient.max(t);
        stored = stored.checked_add(s).ok_or_else(overflow)?;
    }
    transient.checked_add(stored).ok_or_else(overflow)
}

/// Runs every fill of `prepared` (pigment `content_aware::fill`, with `pigment_bytes` as its byte
/// budget) and stores each output plane in its `extra` node. Cancel is observed before each fill.
pub(super) fn run_fills(
    prepared: &mut Prepared<'_>,
    pigment_bytes: u64,
    cancel: &CancellationToken,
) -> Result<(), RenderError> {
    let limits = RetouchLimits {
        max_bytes: pigment_bytes,
        max_work: RetouchLimits::default().max_work,
    };
    let mut outputs = Vec::with_capacity(prepared.fills.len());
    {
        let eval = Eval { prepared: &*prepared };
        for job in &prepared.fills {
            cancel.check().map_err(|_| BackendError::Canceled)?;
            let n = area(job.bounds)?;
            let depth = prepared.need.get(job.input).copied().ok_or_else(overflow)?;
            let (fp, ft) = prepared
                .ext
                .feather_dims(width_of(job.bounds)?, height_of(job.bounds)?)?;
            let mut content = vec![[0.0_f32; 4]; n];
            {
                let mut spare: Vec<Vec<[f32; 4]>> = (0..depth).map(|_| vec![[0.0; 4]; n]).collect();
                let mut fx = Fx::new(fp, ft, None);
                // The content alone: its input node over the bounds, before the layer mask.
                let layer = PLayer {
                    input: job.input,
                    mode: StudioBlendMode::Normal,
                    opacity: 1.0,
                    mask: None,
                    clipped: false,
                    fx: LayerFx::default(),
                    layer_opacity: 1.0,
                    fill_opacity: 1.0,
                };
                eval.frame_content_raw(&layer, job.bounds, &mut content, &mut spare, &mut fx)?;
            }
            let range = eval.prepared.ext.output.sample_range;
            let target = RgbaPlane {
                bounds: job.bounds,
                data: content,
            };
            target
                .validate_in(range)
                .map_err(|e| fill_error(e, job.node, pigment_bytes))?;
            let hole = hole_plane(&job.hole)?;
            let sampling = match job.op.sampling {
                None => SamplingArea::Auto,
                Some(rect) => SamplingArea::Rectangular(rect),
            };
            let filled = fill_in(range, &target, &hole, sampling, &job.op.options, &limits)
                .map_err(|e| fill_error(e, job.node, pigment_bytes))?;
            let plane = filled.result.plane;
            if plane.bounds != job.bounds || plane.data.len() != n {
                return Err(unsupported("content_aware_shape"));
            }
            outputs.push((job.extra, plane));
        }
    }
    for (extra, plane) in outputs {
        let slot = prepared.extra.get_mut(extra).ok_or_else(overflow)?;
        *slot = PNode::Filled {
            rect: plane.bounds,
            data: plane.data,
        };
    }
    Ok(())
}

/// Straight RGBA at `(x, y)` of a filled node (transparent outside its plane).
pub(super) fn filled_at(rect: Rect, data: &[[f32; 4]], x: i64, y: i64) -> Result<[f32; 4], RenderError> {
    let (x1, y1) = end(rect)?;
    if x < rect.x || y < rect.y || x >= x1 || y >= y1 {
        return Ok([0.0; 4]);
    }
    data.get(crate::px::index_in(rect, x, y)?)
        .copied()
        .ok_or_else(|| unsupported("content_aware_shape"))
}
