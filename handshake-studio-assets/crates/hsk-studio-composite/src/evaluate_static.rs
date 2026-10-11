//! Static evaluation of a lowered plan against the immutable snapshot it was lowered from.
//! Same semantics for previews and export: extents come from authored tile records only, hidden
//! layers never contribute to a parent, and anything without a verified route (vector/text to
//! image, unresolved or unsupported payloads, unassigned profiles) is reported `Unavailable`,
//! never faked, skipped or treated as Normal.
use crate::{
    CompositeError,
    lower_extensions::{
        ConvertedAdjustment, EffectContext, LayerFeatures, REFUSE_EFFECT_ON_CLIP_BASE,
        adjustment_for, is_adjustment_layer,
    },
    lower_plan::{LoweredPlan, NodeAddr, SourceValue, Step},
};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    Length, Payload, ProfileBinding, Resolution, Snapshot, StudioRasterRecords, TileRef,
};
use hsk_studio_pigment::adjust_math::AdjustError;
use hsk_studio_pigment::blend_math::{
    Applicability, BlendError, BlendSpace, Exactness, ExactnessLedger, StudioBlendMode,
};
use hsk_studio_pigment::render_plan::{
    ExtOperator, MaskFormat, Operator, PASS_THROUGH_FILL_OPERATOR, PlanError,
};
use hsk_studio_pigment::{Grid, Rect};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StepStatus {
    Ready,
    Unavailable(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepEval {
    pub addr: NodeAddr,
    pub visible: bool,
    /// Bounding union of tile / visible-child rectangles in pixels; `None` when empty or unavailable.
    pub extent: Option<Rect>,
    pub status: StepStatus,
}

impl StepEval {
    /// A step that evaluated fine and contributes nothing (an empty group, a raster without
    /// tiles). An unavailable step is NOT empty: it blocks its parent instead of vanishing.
    pub(crate) fn is_ready_and_empty(&self) -> bool {
        self.status == StepStatus::Ready && self.extent.is_none()
    }
}

/// A document-level or step-level reason the render cannot be complete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unavailable {
    pub target: String,
    pub reason: String,
}

/// Receipt of a static evaluation (STU-ARC-015 shape, minimal): revision, declared blend space,
/// whether the blending profile is bound, and the exactness ledger (lowest class + operators).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderReceipt {
    pub revision: u64,
    pub blend_profile_bound: bool,
    pub blend_space: BlendSpace,
    pub exactness: ExactnessLedger,
    /// Always true: this evaluator never pulls temporal, audio or native providers (STU-CMP-093).
    pub static_closure: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvalReport {
    pub steps: Vec<StepEval>,
    pub unavailable: Vec<Unavailable>,
    pub receipt: RenderReceipt,
}

impl EvalReport {
    /// `Ok` only when nothing is unavailable; otherwise the first reason as a typed error.
    pub fn require_ready(&self) -> Result<(), CompositeError> {
        if let Some(u) = self.unavailable.first() {
            return Err(CompositeError::Unavailable(format!("{}:{}", u.target, u.reason)));
        }
        for s in &self.steps {
            if let StepStatus::Unavailable(reason) = &s.status {
                return Err(CompositeError::Unavailable(format!("{}:{reason}", s.addr.text())));
            }
        }
        Ok(())
    }
    pub fn step(&self, addr: &NodeAddr) -> Option<&StepEval> {
        self.steps.iter().find(|s| &s.addr == addr)
    }
}

fn px(l: &Length) -> Option<u32> {
    let v = l.value;
    if v.is_finite() && v >= 1.0 && v.fract() == 0.0 && v <= f64::from(u32::MAX) {
        Some(v as u32)
    } else {
        None
    }
}

pub(crate) fn union(current: Option<Rect>, next: Rect) -> Result<Option<Rect>, CompositeError> {
    let Some(a) = current else {
        return Ok(Some(next));
    };
    let (ax, ay) = a.end().map_err(|_| CompositeError::Overflow)?;
    let (bx, by) = next.end().map_err(|_| CompositeError::Overflow)?;
    let x = a.x.min(next.x);
    let y = a.y.min(next.y);
    let width = ax.max(bx).checked_sub(x).and_then(|w| u32::try_from(w).ok());
    let height = ay.max(by).checked_sub(y).and_then(|h| u32::try_from(h).ok());
    match (width, height) {
        (Some(width), Some(height)) => Ok(Some(Rect {
            x,
            y,
            width,
            height,
        })),
        _ => Err(CompositeError::Overflow),
    }
}

/// Placement rectangle of every tile, in tile order. A tile (column, row) lies on a grid of the
/// layer's largest tile size; each tile keeps its own size. Shared by extents and plan lowering.
pub(crate) fn tile_rects(tiles: &[TileRef]) -> Result<Vec<Rect>, CompositeError> {
    let mut grid_w = 0u32;
    let mut grid_h = 0u32;
    for t in tiles {
        let (Some(w), Some(h)) = (px(&t.width), px(&t.height)) else {
            return Err(CompositeError::UnsupportedDescriptor("tile_size".into()));
        };
        grid_w = grid_w.max(w);
        grid_h = grid_h.max(h);
    }
    let grid = Grid {
        origin_x: 0,
        origin_y: 0,
        tile_width: grid_w,
        tile_height: grid_h,
    };
    let mut rects = Vec::with_capacity(tiles.len());
    for t in tiles {
        let (Some(width), Some(height)) = (px(&t.width), px(&t.height)) else {
            return Err(CompositeError::UnsupportedDescriptor("tile_size".into()));
        };
        let cell = grid
            .bounds(t.column, t.row)
            .map_err(|_| CompositeError::Overflow)?;
        rects.push(Rect {
            x: cell.x,
            y: cell.y,
            width,
            height,
        });
    }
    Ok(rects)
}

fn raster_extent(tiles: &[TileRef]) -> Result<Option<Rect>, CompositeError> {
    let mut extent = None;
    for rect in tile_rects(tiles)? {
        extent = union(extent, rect)?;
    }
    Ok(extent)
}

fn resolution_key(r: Resolution) -> &'static str {
    match r {
        Resolution::Available => "available",
        Resolution::Absent => "absent",
        Resolution::HashMismatch => "hash_mismatch",
        Resolution::Unauthorized => "unauthorized",
        Resolution::Unsupported => "unsupported",
    }
}

/// Typed capability check for the step's blend mode (STU-RAS-155): `pass_through` belongs to
/// groups, `behind`/`clear` are tool-only and unrecovered modes have no function.
fn check_applicability(mode: StudioBlendMode, composite: bool) -> Result<(), CompositeError> {
    match mode.facts().applicability {
        Applicability::Layer => Ok(()),
        Applicability::GroupOnly if composite => Ok(()),
        Applicability::GroupOnly => Err(BlendError::GroupOnly(mode).into()),
        Applicability::ToolOnly => Err(BlendError::ToolOnly(mode).into()),
        Applicability::Unspecified => Err(BlendError::Unsupported(mode).into()),
    }
}

/// Record a pigment operator: key and class both come from pigment (CX-CAP-001; R5 composite
/// literal drift; test `tests/render_plan_lowering.rs`
/// `evaluation_ledger_keys_and_classes_are_pigments`).
fn record_operator(ledger: &mut ExactnessLedger, op: Operator) {
    ledger.record(op.key(), op.intrinsic());
}
/// Same for an envelope operator.
fn record_ext(ledger: &mut ExactnessLedger, op: ExtOperator) {
    ledger.record(op.key(), op.intrinsic());
}

/// Everything a step uses, keyed like pigment's `Operator`s so ledgers read the same way. The only
/// keys composite owns are `source` (an evaluation concept; pigment names the same thing `tile`)
/// and the refusal key `clip_base_pass_through` (pigment has no operator for it).
fn record_appearance(ledger: &mut ExactnessLedger, step: &Step, adjustment: bool) {
    if matches!(step, Step::Source { .. }) && !adjustment {
        ledger.record("source", Exactness::Exact);
    }
    let a = step.appearance();
    let facts = a.mode.facts();
    ledger.record(facts.key, facts.exactness);
    if a.mode == StudioBlendMode::PassThrough {
        if a.fill_opacity < 1.0 {
            // Pigment: a pass-through layer has no group result for fill to scale, so shared
            // admission refuses it for every backend (`PASS_THROUGH_FILL_OPERATOR`).
            ledger.record(PASS_THROUGH_FILL_OPERATOR, Exactness::Unsupported);
        }
        if a.opacity < 1.0 {
            record_operator(ledger, Operator::PassThroughOpacity);
        }
    } else if a.fill_opacity < 1.0 && a.mode != StudioBlendMode::Normal {
        record_operator(ledger, Operator::FillOpacity);
    }
    if a.clipped {
        // Pigment's clip-to-base route; the Photoshop clipping-group rule is UNVERIFIED.
        record_operator(ledger, Operator::Mask(MaskFormat::BaseAlpha));
    }
}

/// Why the step cannot be rendered although its payload can. The render IR has one mask slot per
/// layer, so a clipped layer cannot also carry its own mask; a clipped layer cannot reference a
/// pass-through group; an enabled mask without coverage tiles has unspecified meaning. None of
/// these is guessed or ignored.
fn appearance_block(
    layers: &BTreeMap<&str, &hsk_studio_folio::StudioLayer>,
    step: &Step,
) -> Option<&'static str> {
    let a = step.appearance();
    if a.mode == StudioBlendMode::PassThrough && a.fill_opacity < 1.0 {
        return Some(PASS_THROUGH_FILL_OPERATOR);
    }
    if a.clipped && a.mask {
        return Some("clipped_with_mask_unsupported");
    }
    if a.clipped && a.mode == StudioBlendMode::PassThrough {
        return Some("clipped_pass_through_unsupported");
    }
    if a.mask {
        let tiles = layers
            .get(step.addr().layer_id.as_str())
            .and_then(|l| l.appearance.as_ref())
            .and_then(|ap| ap.mask.as_ref())
            .map_or(0, |m| m.tiles.len());
        if tiles == 0 {
            return Some("mask_empty");
        }
    }
    None
}

/// Evaluate `plan` against `snap`. Fails with `StaleRevision` when the plan and snapshot revisions
/// differ, `Cycle` / `DanglingInput` when the (possibly hand-built) plan is not a topologically
/// ordered closure, and `Canceled` when the token fires (checked per step).
pub fn evaluate(
    plan: &LoweredPlan,
    snap: &Snapshot,
    cancel: &CancellationToken,
) -> Result<EvalReport, CompositeError> {
    evaluate_with(plan, snap, None, cancel)
}

/// [`evaluate`] with the document's raster records (adjustment layers, mask parameters, Blend-If,
/// knockout, effect stacks). Every envelope operator a visible layer uses is recorded in the
/// ledger under pigment's `ExtOperator::key()` at its `intrinsic()` class; a feature the envelope
/// cannot carry makes the step `Unavailable` with the same code the lowering returns. Knockout and
/// modern brightness/contrast are carried, not blocked: pigment's admission refuses them and the
/// ledger says `Unsupported`. `records.document_id` must be the snapshot's document.
pub fn evaluate_extended(
    plan: &LoweredPlan,
    snap: &Snapshot,
    records: &StudioRasterRecords,
    cancel: &CancellationToken,
) -> Result<EvalReport, CompositeError> {
    if records.document_id != snap.document().document_id {
        return Err(CompositeError::UnsupportedDescriptor(
            "raster_records_document_mismatch".into(),
        ));
    }
    evaluate_with(plan, snap, Some(records), cancel)
}

fn evaluate_with(
    plan: &LoweredPlan,
    snap: &Snapshot,
    records: Option<&StudioRasterRecords>,
    cancel: &CancellationToken,
) -> Result<EvalReport, CompositeError> {
    cancel.check().map_err(|_| CompositeError::Canceled)?;
    let doc = snap.document();
    if plan.revision != doc.revision {
        return Err(CompositeError::StaleRevision {
            expected: plan.revision,
            actual: doc.revision,
        });
    }
    let layers: BTreeMap<&str, &hsk_studio_folio::StudioLayer> =
        doc.layers.iter().map(|l| (l.layer_id.as_str(), l)).collect();
    let all: BTreeSet<&NodeAddr> = plan.steps.iter().map(Step::addr).collect();
    if all.len() != plan.steps.len() {
        return Err(CompositeError::UnsupportedDescriptor("duplicate_step".into()));
    }

    let mut ledger = ExactnessLedger::default();
    let mut evals: Vec<StepEval> = Vec::with_capacity(plan.steps.len());
    // Index-aligned with `evals`: the step is an adjustment layer (it has no extent of its own and
    // applies to whatever lies below it, so it is never "empty").
    let mut adjustment_steps: Vec<bool> = Vec::with_capacity(plan.steps.len());
    // Index-aligned with `evals`: the step is visible and carries rendered effects (such a layer
    // cannot be the base of a clipping run; pigment refuses it).
    let mut effects_steps: Vec<bool> = Vec::with_capacity(plan.steps.len());
    let effect_cx = records.map(|r| EffectContext {
        global_light: r.global_light.as_ref(),
        blending: &doc.blending_space,
        space: plan.blend_space,
    });
    let mut seen: BTreeMap<&NodeAddr, usize> = BTreeMap::new();
    for step in &plan.steps {
        cancel.check().map_err(|_| CompositeError::Canceled)?;
        check_applicability(
            step.appearance().mode,
            matches!(step, Step::Composite { .. }),
        )?;
        let layer = layers.get(step.addr().layer_id.as_str()).copied();
        // An adjustment layer is a graph source whose payload is its own primitive reference; it
        // is only understood together with the raster records.
        let converted: Option<Result<ConvertedAdjustment, &'static str>> = match (records, layer, step)
        {
            (Some(recs), Some(l), Step::Source { value: SourceValue::Image, .. })
                if is_adjustment_layer(l) =>
            {
                Some(adjustment_for(recs, &l.layer_id, plan.blend_space))
            }
            _ => None,
        };
        record_appearance(&mut ledger, step, converted.is_some());
        let mut ext_block: Option<&'static str> = None;
        let mut step_effects = false;
        if let (Some(recs), Some(cx), Some(l), true) = (records, effect_cx.as_ref(), layer, step.visible())
        {
            if let Some(Ok(c)) = &converted {
                record_ext(&mut ledger, ExtOperator::AdjustmentLayer);
                record_ext(&mut ledger, ExtOperator::Adjustment(c.op.kind()));
                for (key, exactness, _) in &c.notes {
                    ledger.record(key, *exactness);
                }
                // Pigment decides what it cannot evaluate (modern brightness/contrast).
                if matches!(c.op.prepare(), Err(AdjustError::Unsupported(_))) {
                    ledger.record(c.op.kind().key(), Exactness::Unsupported);
                }
            }
            let features = LayerFeatures::of(&layers, recs, cx, &l.layer_id);
            let a = step.appearance();
            for op in features.operators(a.mask && !a.clipped) {
                record_ext(&mut ledger, op);
            }
            // An effect composites with its own blend mode: pigment's admission asks the backend
            // for that mode's v1 class, so the ledger lists it under the mode's pigment key.
            for mode in features.effect_modes() {
                record_operator(&mut ledger, Operator::Blend(mode));
            }
            for (key, exactness, _) in &features.notes {
                ledger.record(key, *exactness);
            }
            ext_block = features.refusal(a, converted.is_some());
            step_effects = features.has_effects();
        }
        adjustment_steps.push(converted.is_some());
        effects_steps.push(step_effects);
        let eval = match step {
            Step::Source {
                addr,
                value,
                visible,
                ..
            } => {
                let payload = layers.get(addr.layer_id.as_str()).map(|l| &l.payload);
                let (extent, status) = match (&converted, value, payload) {
                    (Some(Ok(_)), _, _) => (None, StepStatus::Ready),
                    (Some(Err(code)), _, _) => (None, StepStatus::Unavailable((*code).into())),
                    (None, SourceValue::Image, Some(Payload::Raster { tiles })) => {
                        (raster_extent(tiles)?, StepStatus::Ready)
                    }
                    (None, SourceValue::Vector, _) => (
                        None,
                        StepStatus::Unavailable("vector_to_image_conversion".into()),
                    ),
                    (None, SourceValue::Text, _) => (
                        None,
                        StepStatus::Unavailable("text_to_image_conversion".into()),
                    ),
                    _ => (None, StepStatus::Unavailable("unsupported_payload".into())),
                };
                StepEval {
                    addr: addr.clone(),
                    visible: *visible,
                    extent,
                    status,
                }
            }
            Step::Composite {
                addr,
                inputs,
                visible,
                ..
            } => {
                let mut extent = None;
                let mut blocked: Option<String> = None;
                let mut have_base = false;
                // The current clip base is a rendered (visible, non-empty) pass-through group.
                let mut pass_through_base = false;
                // The current clip base is hidden or empty: its clipped run is dropped with it
                // (the lowering rule), so those layers neither block the parent nor count.
                let mut base_dropped = false;
                // The current clip base is a visible adjustment layer (pigment refuses it).
                let mut adjustment_base = false;
                // The current clip base is a rendered layer with effects (pigment refuses it).
                let mut effects_base = false;
                for input in inputs {
                    let Some(&i) = seen.get(input) else {
                        return Err(if all.contains(input) {
                            CompositeError::Cycle(input.text())
                        } else {
                            CompositeError::DanglingInput(input.text())
                        });
                    };
                    let child = &evals[i];
                    // Clipping group: a clipped input clips to the nearest unclipped input below
                    // it (whatever its visibility), so it needs one, and its content never
                    // extends beyond that base: it adds no extent of its own.
                    let clipped = plan.steps[i].appearance().clipped;
                    if clipped && !have_base {
                        blocked.get_or_insert_with(|| {
                            format!("clip_without_base:{}", child.addr.text())
                        });
                    }
                    have_base |= !clipped;
                    // A pass-through group has no isolated content to take a base alpha from, and
                    // the backends refuse it; Photoshop's semantics for it are UNVERIFIED (oracle
                    // pending). A hidden or empty base (or clipped layer) is dropped, not refused.
                    // An adjustment layer has no extent of its own but is rendered (it acts on
                    // whatever lies below it), so it is neither empty nor a droppable base.
                    let adjustment = adjustment_steps[i];
                    let rendered = child.visible && (child.extent.is_some() || adjustment);
                    if !clipped {
                        pass_through_base =
                            rendered && plan.steps[i].appearance().mode == StudioBlendMode::PassThrough;
                        adjustment_base = child.visible && adjustment;
                        effects_base = rendered && effects_steps[i];
                        base_dropped = !child.visible || (child.is_ready_and_empty() && !adjustment);
                    } else if base_dropped {
                        // Dropped with its hidden or empty base: not refused, not unavailable, no
                        // extent (R3: it used to block the parent through the status match below).
                        continue;
                    } else if pass_through_base && rendered {
                        ledger.record("clip_base_pass_through", Exactness::Unsupported);
                        blocked.get_or_insert_with(|| {
                            format!("clip_base_pass_through:{}", child.addr.text())
                        });
                    } else if adjustment_base && rendered {
                        // Pigment: an adjustment layer has no base alpha to clip to
                        // (`PlanError::AdjustmentClipBase`), refused for every backend.
                        ledger.record(
                            PlanError::AdjustmentClipBase { node: 0 }.code(),
                            Exactness::Unsupported,
                        );
                        blocked.get_or_insert_with(|| {
                            format!("clip_base_adjustment:{}", child.addr.text())
                        });
                    } else if effects_base && rendered {
                        // Pigment: Photoshop's rule for effects inside a clipping group is
                        // UNVERIFIED, so effects on a clip base are refused for every backend.
                        ledger.record(
                            PlanError::EffectClipRun { node: 0 }.code(),
                            Exactness::Unsupported,
                        );
                        blocked.get_or_insert_with(|| {
                            format!("{REFUSE_EFFECT_ON_CLIP_BASE}:{}", child.addr.text())
                        });
                    }
                    if !child.visible {
                        continue;
                    }
                    match &child.status {
                        StepStatus::Ready => {
                            if !clipped && let Some(e) = child.extent {
                                extent = union(extent, e)?;
                            }
                        }
                        StepStatus::Unavailable(_) => {
                            blocked.get_or_insert_with(|| {
                                format!("input_unavailable:{}", child.addr.text())
                            });
                        }
                    }
                }
                match blocked {
                    Some(reason) => StepEval {
                        addr: addr.clone(),
                        visible: *visible,
                        extent: None,
                        status: StepStatus::Unavailable(reason),
                    },
                    None => StepEval {
                        addr: addr.clone(),
                        visible: *visible,
                        extent,
                        status: StepStatus::Ready,
                    },
                }
            }
        };
        // Appearance the renderer cannot honour yet makes the step unavailable (never ignored).
        let eval = match appearance_block(&layers, step).or(ext_block) {
            Some(reason) => StepEval {
                extent: None,
                status: StepStatus::Unavailable(reason.into()),
                ..eval
            },
            None => eval,
        };
        seen.insert(step.addr(), evals.len());
        evals.push(eval);
    }

    let mut unavailable = Vec::new();
    for issue in snap.resolution_issues() {
        unavailable.push(Unavailable {
            target: issue.target.clone(),
            reason: resolution_key(issue.disposition).into(),
        });
    }
    for (name, binding) in [
        ("working_space_rgb", &doc.working_space_rgb),
        ("working_space_cmyk", &doc.working_space_cmyk),
        ("working_space_gray", &doc.working_space_gray),
        ("working_space_spot", &doc.working_space_spot),
        ("blending_space", &doc.blending_space),
    ] {
        if matches!(binding, ProfileBinding::Unassigned(())) {
            unavailable.push(Unavailable {
                target: name.into(),
                reason: "unassigned_profile".into(),
            });
        }
    }
    Ok(EvalReport {
        steps: evals,
        unavailable,
        receipt: RenderReceipt {
            revision: plan.revision,
            blend_profile_bound: plan.blend_profile_bound,
            blend_space: plan.blend_space,
            exactness: ledger,
            static_closure: true,
        },
    })
}
