//! Key pipelines on the CPU backend (lane SMB-AE-KEY-TRACK-2): the envelope entry
//! `LayerExtension::keys` (pigment `keying::KeyStage`, STU-CMP-032/033). The keying maths is
//! pigment's single owner `keying::run_key_pipeline` (CX-CAP-001): render-cpu calls it once per
//! keyed layer and keeps no keyer, matte operator or despill of its own.
//!
//! Evaluation. Choke, spread, feather and edge-aware smoothing read neighbourhoods whose edge rule
//! is the plane's border, so a key is evaluated over the whole layer, not per chunk: before the
//! first chunk `run_keys` renders each keyed layer's content (its input node, WITHOUT the layer
//! mask, transparent beyond the content) over the content bounds inside the plan extent, hands
//! that plane to `run_key_pipeline`, and stores pigment's output colour plane (despilled, alpha
//! multiplied by the matte) as a node of `Prepared::extra` that the layer reads instead of its
//! input. The chunked compositor then applies the layer mask, Blend-If, mode and
//! `opacity * fill` as for any layer, so the bytes are pigment's key of the whole content
//! composited by `blend_math::composite_over`, for every chunking and thread count
//! (tests/cpu_keying.rs).
//!
//! Resource rules (V4-01/V4-02): the content plane, its group levels and feather scratch, and
//! every stored output plane are charged to `max_working_bytes` (after the chunk scratch and the
//! content-aware fills) before they are allocated; pigment's own pipeline bytes and work are
//! admitted by pigment against `KeyLimits` (the remaining budget, pigment's default work bound)
//! before it allocates. Refusals are typed before the first sink call: a keyed layer whose content
//! holds a layer with effects, a blur, filters, a content-aware fill or another key is
//! [`KEY_NESTED`]; pigment's refusals map to `InvalidPlan(KeyParameter)` (a parameter),
//! `BudgetExceeded` (bytes or work) and the sample-range key (a sample outside `[0, 1]`).
#![deny(clippy::arithmetic_side_effects)]
use super::extended::{ExtPrep, Fx, LayerFx};
use super::{Eval, PLayer, PNode, Prepared, RenderError, unsupported};
use crate::px::{area, height_of, overflow, width_of};
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::keying::{KeyError, KeyLimits, KeyStage, key_pipeline_cost, run_key_pipeline};
use hsk_studio_pigment::render_plan::{BackendError, PlanError, SAMPLE_RANGE_OPERATOR, StudioBlendMode};
use hsk_studio_pigment::retouch::RgbaPlane;

/// Refusal key: a keyed layer whose content holds a layer with effects, a blur, filters, a
/// content-aware fill or another key (the content is rendered once over the whole layer).
pub(super) const KEY_NESTED: &str = "key_nested";
/// Bytes of one content / output pixel (straight RGBA `f32`).
const PIXEL_BYTES: u64 = 16;
/// Bytes of one feather scratch sample (`f32`).
const FEATHER_SAMPLE_BYTES: u64 = 4;

/// One layer to key before the first chunk.
pub(super) struct KeyJob {
    /// Index of the layer's output node in `Prepared::extra`.
    pub(super) extra: usize,
    /// The layer's content node.
    pub(super) input: usize,
    /// Content bounds inside the plan extent (the plane handed to pigment).
    pub(super) bounds: Rect,
    pub(super) stages: Vec<KeyStage>,
    pub(super) node: u32,
}

fn key_error(error: KeyError, node: u32) -> RenderError {
    match error {
        KeyError::Parameter | KeyError::MissingCleanPlate => {
            RenderError::Backend(BackendError::InvalidPlan(PlanError::KeyParameter { node }))
        }
        KeyError::OutOfDomain => unsupported(SAMPLE_RANGE_OPERATOR),
        KeyError::OverBudget { needed, limit } => {
            RenderError::Backend(BackendError::BudgetExceeded { requested: needed, limit })
        }
        KeyError::Overflow => overflow(),
        other => unsupported(other.code()),
    }
}

/// `(transient, stored)` bytes of one job: the content plane, its group levels and feather scratch
/// (freed after the key), and the output plane kept for the run.
fn job_bytes(prepared: &Prepared<'_>, ext: &ExtPrep, job: &KeyJob) -> Result<(u64, u64), RenderError> {
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
    let transient = plane
        .checked_mul(levels)
        .and_then(|b| b.checked_add(feather))
        .ok_or_else(overflow)?;
    Ok((transient, plane))
}

/// Bytes the keys of a run need next to the chunk scratch: every stored output plane plus the
/// largest transient set (the jobs run one after the other). Zero without keys.
pub(super) fn key_bytes(prepared: &Prepared<'_>) -> Result<u64, RenderError> {
    let (mut transient, mut stored) = (0_u64, 0_u64);
    for job in &prepared.keys {
        let (t, s) = job_bytes(prepared, &prepared.ext, job)?;
        transient = transient.max(t);
        stored = stored.checked_add(s).ok_or_else(overflow)?;
    }
    transient.checked_add(stored).ok_or_else(overflow)
}

/// Pigment's predicted peak bytes over the jobs (refused against the budget before any job runs).
pub(super) fn pigment_bytes(prepared: &Prepared<'_>) -> Result<u64, RenderError> {
    let mut peak = 0_u64;
    for job in &prepared.keys {
        let n = u64::try_from(area(job.bounds)?).map_err(|_| overflow())?;
        let (bytes, _) = key_pipeline_cost(n, &job.stages).map_err(|e| key_error(e, job.node))?;
        peak = peak.max(bytes);
    }
    Ok(peak)
}

/// Runs every key of `prepared` (pigment `run_key_pipeline` with `pigment_limit` bytes) and stores
/// each output plane in its `extra` node. Cancel is observed before each key.
pub(super) fn run_keys(
    prepared: &mut Prepared<'_>,
    pigment_limit: u64,
    cancel: &CancellationToken,
) -> Result<(), RenderError> {
    let limits = KeyLimits {
        max_bytes: pigment_limit,
        max_work: KeyLimits::DEFAULT.max_work,
    };
    let mut outputs = Vec::with_capacity(prepared.keys.len());
    {
        let eval = Eval { prepared: &*prepared };
        for job in &prepared.keys {
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
            let plane = RgbaPlane { bounds: job.bounds, data: content };
            let keyed = run_key_pipeline(&plane, &job.stages, None, &limits).map_err(|e| key_error(e, job.node))?;
            if keyed.colour.bounds != job.bounds || keyed.colour.data.len() != n {
                return Err(unsupported("key_shape"));
            }
            outputs.push((job.extra, keyed.colour));
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
