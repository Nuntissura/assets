//! Smart-filter stacks on the CPU backend (lane SMB-PS-SELECT-LIQUIFY-2): the envelope entry
//! `LayerExtension::smart_filters` (pigment `smart_filter::SmartFilterOp`, STU-RAS-037 /
//! STU-FX-011a / STU-FX-024). The stack maths is pigment's single owner
//! `smart_filter::SmartFilterStack::render` (CX-CAP-001): render-cpu resolves the plan mask
//! sources into coverage planes and calls it once per layer; it keeps no filter, liquify or mix
//! of its own.
//!
//! Evaluation (same shape as key pipelines): before the first chunk each smart-filtered layer's
//! content (its input node, WITHOUT the layer mask, transparent beyond the content) is rendered
//! over the content bounds inside the plan extent, the stack renders that plane, and pigment's
//! output is stored as a node of `Prepared::extra` that the layer reads instead of its input. The
//! chunked compositor applies the layer mask, Blend-If, mode and opacity as for any layer, so the
//! bytes are pigment's stack of the whole content composited by `blend_math::composite_over`,
//! for every chunking and thread count (tests/cpu_smart_filter.rs). A liquify mesh must cover
//! exactly that plane (else `plan_smart_filter_parameter`).
//!
//! Resource rules (V4-01/V4-02): the content plane, its group levels and feather scratch, the
//! resolved mask planes and every stored output plane are charged to `max_working_bytes` before
//! they are allocated; pigment admits its filter windows and liquify output against the remaining
//! budget. Refusals are typed before the first sink call.
#![deny(clippy::arithmetic_side_effects)]
use super::extended::{ExtPrep, Fx, LayerFx};
use super::{Eval, MaskView, PLayer, PNode, Prepared, RenderError, SourceFault, source, unsupported};
use crate::px::{area, height_of, mul_u, overflow, width_of};
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::filter_math::{FilterError, FilterLimits};
use hsk_studio_pigment::liquify::{LiquifyError, LiquifyLimits};
use hsk_studio_pigment::render_plan::{BackendError, MaskFormat, MaskInput, PlanError, PlanSources, StudioBlendMode};
use hsk_studio_pigment::retouch::RgbaPlane;
use hsk_studio_pigment::selection::CoveragePlane;
use hsk_studio_pigment::smart_filter::{SmartFilterOp, SmartFilterStack, StackEntry, StackError, StackLimits};

/// Refusal key: a smart-filtered layer whose content holds a layer with effects, a blur, filters,
/// a content-aware fill, a key or another stack (the content is rendered once over the layer).
pub(super) const SMART_FILTER_NESTED: &str = "smart_filter_nested";
const PIXEL_BYTES: u64 = 16;
const FEATHER_SAMPLE_BYTES: u64 = 4;
const COVERAGE_SAMPLE_BYTES: u64 = 2;

/// One layer to filter before the first chunk.
pub(super) struct SmartJob<'a> {
    pub(super) extra: usize,
    pub(super) input: usize,
    pub(super) bounds: Rect,
    pub(super) op: SmartFilterOp,
    pub(super) masks: Vec<Option<MaskView<'a>>>,
    pub(super) stack_mask: Option<MaskView<'a>>,
    pub(super) node: u32,
}

fn parameter(node: u32) -> RenderError {
    RenderError::Backend(BackendError::InvalidPlan(PlanError::SmartFilterParameter { node }))
}

/// A mask source as a view (declared format, hash, bounds, stride checked like a layer mask).
pub(super) fn mask_view<'a>(sources: &'a dyn PlanSources, m: &MaskInput, node: u32) -> Result<MaskView<'a>, RenderError> {
    let wide = match m.format {
        MaskFormat::CoverageU8 => false,
        MaskFormat::CoverageU16Le => true,
        _ => return Err(parameter(node)),
    };
    if m.density != 1.0 {
        return Err(parameter(node));
    }
    let plane = sources.mask(m.key).ok_or(BackendError::UnresolvedSource(m.key))?;
    if plane.sample_format != m.format.key() || plane.sha256 != m.sha256 || plane.bounds != m.placement {
        return Err(source(m.key, SourceFault::Declared));
    }
    plane.validate(plane.bounds).map_err(|e| {
        source(
            m.key,
            if e == hsk_studio_pigment::Error::HashMismatch { SourceFault::HashMismatch } else { SourceFault::MalformedStride },
        )
    })?;
    Ok(MaskView {
        bounds: plane.bounds,
        wide,
        bytes: plane.bytes,
        stride: usize::try_from(plane.stride_bytes).map_err(|_| overflow())?,
        inverted: m.inverted,
        density: 1.0,
        xform: None,
    })
}

/// A view as pigment's coverage plane (`u8 * 257`, inverted `65535 - c`).
fn coverage(view: &MaskView<'_>) -> Result<CoveragePlane, RenderError> {
    let (w, h) = (width_of(view.bounds)?, height_of(view.bounds)?);
    let mut data = Vec::with_capacity(mul_u(w, h)?);
    let size = if view.wide { 2 } else { 1 };
    for row in 0..h {
        let start = mul_u(row, view.stride)?;
        for column in 0..w {
            let at = mul_u(column, size).and_then(|c| crate::px::add_u(start, c))?;
            let stored = if view.wide {
                let end = at.checked_add(2).ok_or_else(overflow)?;
                let Some(&[lo, hi]) = view.bytes.get(at..end) else {
                    return Err(overflow());
                };
                u16::from_le_bytes([lo, hi])
            } else {
                u16::from(*view.bytes.get(at).ok_or_else(overflow)?).checked_mul(257).ok_or_else(overflow)?
            };
            data.push(if view.inverted { u16::MAX.saturating_sub(stored) } else { stored });
        }
    }
    Ok(CoveragePlane { bounds: view.bounds, data })
}

fn stack_error(error: StackError, node: u32, limit: u64) -> RenderError {
    match error {
        StackError::Filter(FilterError::OverBudget { needed, limit }) => {
            RenderError::Backend(BackendError::BudgetExceeded { requested: needed, limit })
        }
        StackError::Liquify(LiquifyError::Pigment(hsk_studio_pigment::Error::BudgetExceeded))
        | StackError::Pigment(hsk_studio_pigment::Error::BudgetExceeded) => {
            RenderError::Backend(BackendError::BudgetExceeded { requested: limit.saturating_add(1), limit })
        }
        StackError::Filter(FilterError::Overflow) | StackError::Pigment(hsk_studio_pigment::Error::Overflow) => overflow(),
        StackError::Filter(FilterError::Parameter)
        | StackError::Opacity
        | StackError::TooManyEntries
        | StackError::Liquify(_)
        | StackError::Pigment(hsk_studio_pigment::Error::InvalidInput) => parameter(node),
        other => unsupported(other.code()),
    }
}

fn job_bytes(prepared: &Prepared<'_>, ext: &ExtPrep, job: &SmartJob<'_>) -> Result<(u64, u64), RenderError> {
    let to_u64 = |n: usize| u64::try_from(n).map_err(|_| overflow());
    let pixels = to_u64(area(job.bounds)?)?;
    let plane = pixels.checked_mul(PIXEL_BYTES).ok_or_else(overflow)?;
    let levels = to_u64(prepared.need.get(job.input).copied().ok_or_else(overflow)?)?
        .checked_add(1)
        .ok_or_else(overflow)?;
    let (fp, ft) = ext.feather_dims(width_of(job.bounds)?, height_of(job.bounds)?)?;
    let feather = to_u64(crate::px::add_u(fp, ft)?)?.checked_mul(FEATHER_SAMPLE_BYTES).ok_or_else(overflow)?;
    let mut masks = 0_u64;
    for v in job.masks.iter().flatten().chain(job.stack_mask.iter()) {
        let b = to_u64(area(v.bounds)?)?.checked_mul(COVERAGE_SAMPLE_BYTES).ok_or_else(overflow)?;
        masks = masks.checked_add(b).ok_or_else(overflow)?;
    }
    // The stack keeps the current plane and one filtered plane (pigment's own window bytes are
    // admitted by pigment against the remaining budget).
    let transient = plane
        .checked_mul(levels.checked_add(2).ok_or_else(overflow)?)
        .and_then(|b| b.checked_add(feather))
        .and_then(|b| b.checked_add(masks))
        .ok_or_else(overflow)?;
    Ok((transient, plane))
}

/// Bytes the stacks of a run need next to the chunk scratch (zero without stacks).
pub(super) fn smart_bytes(prepared: &Prepared<'_>) -> Result<u64, RenderError> {
    let (mut transient, mut stored) = (0_u64, 0_u64);
    for job in &prepared.smart {
        let (t, s) = job_bytes(prepared, &prepared.ext, job)?;
        transient = transient.max(t);
        stored = stored.checked_add(s).ok_or_else(overflow)?;
    }
    transient.checked_add(stored).ok_or_else(overflow)
}

/// Runs every stack (pigment `SmartFilterStack::render`, `pigment_limit` bytes for its filter
/// windows and liquify output) and stores each output plane in its `extra` node.
pub(super) fn run_smart(prepared: &mut Prepared<'_>, pigment_limit: u64, cancel: &CancellationToken) -> Result<(), RenderError> {
    let limits = StackLimits {
        filter: FilterLimits { max_bytes: pigment_limit, max_work: StackLimits::default().filter.max_work },
        liquify: LiquifyLimits { max_output_bytes: pigment_limit, ..LiquifyLimits::default() },
        ..StackLimits::default()
    };
    let mut outputs = Vec::with_capacity(prepared.smart.len());
    {
        let eval = Eval { prepared: &*prepared };
        for job in &prepared.smart {
            cancel.check().map_err(|_| BackendError::Canceled)?;
            let n = area(job.bounds)?;
            let depth = prepared.need.get(job.input).copied().ok_or_else(overflow)?;
            let (fp, ft) = prepared.ext.feather_dims(width_of(job.bounds)?, height_of(job.bounds)?)?;
            let mut content = vec![[0.0_f32; 4]; n];
            {
                let mut spare: Vec<Vec<[f32; 4]>> = (0..depth).map(|_| vec![[0.0; 4]; n]).collect();
                let mut fx = Fx::new(fp, ft, None);
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
            let mut entries = Vec::with_capacity(job.op.entries.len());
            for (e, view) in job.op.entries.iter().zip(&job.masks) {
                entries.push(StackEntry {
                    filter: e.filter.clone(),
                    enabled: e.enabled,
                    opacity: e.opacity,
                    mask: view.as_ref().map(coverage).transpose()?,
                });
            }
            let stack = SmartFilterStack { entries, stack_mask: job.stack_mask.as_ref().map(coverage).transpose()? };
            let plane = RgbaPlane { bounds: job.bounds, data: content };
            let out = stack.render(&plane, &limits).map_err(|e| stack_error(e, job.node, pigment_limit))?;
            if out.bounds != job.bounds || out.data.len() != n {
                return Err(unsupported("smart_filter_shape"));
            }
            outputs.push((job.extra, out));
        }
    }
    for (extra, plane) in outputs {
        let slot = prepared.extra.get_mut(extra).ok_or_else(overflow)?;
        *slot = PNode::Filled { rect: plane.bounds, data: plane.data };
    }
    Ok(())
}
