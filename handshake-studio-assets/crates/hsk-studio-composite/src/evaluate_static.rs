//! Static evaluation of a lowered plan against the immutable snapshot it was lowered from.
//! Same semantics for previews and export: extents come from authored tile records only, hidden
//! layers never contribute to a parent, and anything without a verified route (vector/text to
//! image, unresolved or unsupported payloads, unassigned profiles) is reported `Unavailable`,
//! never faked, skipped or treated as Normal.
use crate::{
    CompositeError,
    lower_extensions::{
        ConvertedAdjustment, EffectContext, LayerFeatures, REFUSE_BLUR_COMBINATION,
        REFUSE_CLIP_BASE_PASS_THROUGH,
        REFUSE_CLIP_GROUP_BLEND_OFF, REFUSE_EFFECT_ON_CLIP_BASE, adjustment_for,
        blend_if_operator, is_adjustment_layer,
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
    ClipRole, ExtOperator, MaskFormat, Operator, PASS_THROUGH_FILL_OPERATOR,
    PASS_THROUGH_MASK_OPERATOR, PlanError,
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

/// Status of a vector layer or boolean group: Ready with its plane as extent (an empty shape or no
/// fills: Ready and empty), its typed refusal, or `vector_layer_not_prepared`; the operators its
/// lowering uses are recorded when it is visible and inked.
fn vector_status(
    vectors: Option<&crate::vector::PreparedVectors>,
    layer_id: &str,
    visible: bool,
    ledger: &mut ExactnessLedger,
) -> (Option<Rect>, StepStatus) {
    let Some(v) = vectors else {
        return (None, StepStatus::Unavailable("vector_to_image_conversion".into()));
    };
    if let Some(code) = v.refusal(layer_id) {
        return (None, StepStatus::Unavailable(code.into()));
    }
    let Some(plane) = v.plane(layer_id) else {
        return (None, StepStatus::Unavailable("vector_layer_not_prepared".into()));
    };
    let extent = plane.placement.filter(|_| !plane.fills.is_empty());
    if extent.is_some() && visible {
        record_operator(ledger, Operator::IsolatedGroup);
        record_operator(ledger, Operator::Mask(MaskFormat::CoverageU8));
        for f in &plane.fills {
            let mode = f.mode.facts();
            ledger.record(mode.key, mode.exactness);
            match &f.paint {
                crate::vector::PreparedPaint::Solid(_) => record_operator(ledger, Operator::Solid),
                crate::vector::PreparedPaint::Painted(_) => record_operator(ledger, Operator::Tile),
                crate::vector::PreparedPaint::Image { .. } => {
                    record_operator(ledger, Operator::Tile);
                    record_ext(
                        ledger,
                        ExtOperator::Transform(crate::vector::IMAGE_FILTER),
                    );
                }
            }
        }
    }
    (extent, StepStatus::Ready)
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
/// key composite owns is `source` (an evaluation concept; pigment names the same thing `tile`);
/// every refusal key is pigment's: an operator key, an exported constant, or the code of the
/// `PlanError` pigment's validation refuses the rule with (PLR-006, test
/// `refusal_keys_are_pigments`).
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
        if a.mask && !a.clipped {
            // Pigment defines no mask on a pass-through layer (no group result to weigh), so
            // shared admission refuses it for every backend (`PASS_THROUGH_MASK_OPERATOR`).
            ledger.record(PASS_THROUGH_MASK_OPERATOR, Exactness::Unsupported);
        }
    } else if a.fill_opacity < 1.0 && a.mode != StudioBlendMode::Normal {
        record_operator(ledger, Operator::FillOpacity);
    }
    if a.clipped {
        // Pigment's clip-to-base route; the Photoshop clipping-group rule is UNVERIFIED.
        record_operator(ledger, Operator::Mask(MaskFormat::BaseAlpha));
    }
}

/// Why a vector-masked step (lane SMB-COMPOSITE-VECTOR-MASK, `crate::vector_mask`) cannot be
/// rendered: its mask's own refusal, a second mask in the one slot (raster mask or clip-to-base),
/// or an adjustment layer (no extent of its own to carry the plane). Never ignored.
fn vector_mask_block(
    vectors: Option<&crate::vector::PreparedVectors>,
    step: &Step,
    adjustment: bool,
) -> Option<&'static str> {
    if let Err(code) = vectors?.mask(&step.addr().layer_id)? {
        return Some(code);
    }
    let a = step.appearance();
    if a.clipped || a.mask {
        return Some(crate::vector_mask::REFUSE_MASK_WITH_MASK);
    }
    adjustment.then_some(crate::vector_mask::REFUSE_MASK_ADJUSTMENT)
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
    // Same standing as fill: a mask on a pass-through layer has no group result to weigh, pigment
    // refuses it for every backend, and a PSD with such a group imports with a typed loss. A clipped
    // layer's mask slot belongs to the clip route (refused below).
    if a.mode == StudioBlendMode::PassThrough && a.mask && !a.clipped {
        return Some(PASS_THROUGH_MASK_OPERATOR);
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

/// The clip roles a step has across the groups that composite it (a step shared by two groups can
/// be the base of a run in one and unclipped in the other).
#[derive(Clone, Copy, Default)]
struct RoleSet {
    unclipped: bool,
    base: bool,
    clipped: bool,
}

impl RoleSet {
    fn add(&mut self, role: ClipRole) {
        match role {
            ClipRole::Unclipped => self.unclipped = true,
            ClipRole::Base => self.base = true,
            ClipRole::Clipped => self.clipped = true,
        }
    }
    fn iter(self) -> impl Iterator<Item = ClipRole> {
        [
            (self.unclipped, ClipRole::Unclipped),
            (self.base, ClipRole::Base),
            (self.clipped, ClipRole::Clipped),
        ]
        .into_iter()
        .filter_map(|(on, role)| on.then_some(role))
    }
}

/// The clip role of every step in the groups that composite it, as the lowering emits it
/// (pigment's `Group::clip_role`: a clipped layer is `Clipped`, an unclipped layer with an emitted
/// clipped layer directly above it is `Base`, everything else `Unclipped`). A clipped layer under a
/// hidden or empty base, and a hidden or empty clipped layer, are not emitted and have no role in
/// that group, exactly like the lowering drops them. A step no group composites is `Unclipped`.
///
/// Also returns, per step, the inputs the lowering emits as layers of that step's group (visible and
/// not empty, a clipped layer only with a surviving base): the shape of the plan's layers, which the
/// nested-effects accounting reads.
fn clip_roles(
    plan: &LoweredPlan,
    evals: &[StepEval],
    adjustment_steps: &[bool],
    seen: &BTreeMap<&NodeAddr, usize>,
) -> (Vec<RoleSet>, Vec<Vec<usize>>) {
    let mut roles = vec![RoleSet::default(); plan.steps.len()];
    let mut emitted: Vec<Vec<usize>> = vec![Vec::new(); plan.steps.len()];
    let mut has_parent = vec![false; plan.steps.len()];
    for (at, step) in plan.steps.iter().enumerate() {
        let Step::Composite { inputs, .. } = step else {
            continue;
        };
        // (step index, role) in input order; `base_at` points at the current base in `local`.
        let mut local: Vec<(usize, ClipRole)> = Vec::with_capacity(inputs.len());
        let mut base_at: Option<usize> = None;
        let mut base_dropped = false;
        for input in inputs {
            let Some(&i) = seen.get(input) else {
                continue;
            };
            has_parent[i] = true;
            let child = &evals[i];
            let empty = child.is_ready_and_empty() && !adjustment_steps[i];
            if !plan.steps[i].appearance().clipped {
                base_dropped = !child.visible || empty;
                base_at = Some(local.len());
                local.push((i, ClipRole::Unclipped));
            } else if let Some(base) = base_at
                && !base_dropped
                && child.visible
                && !empty
            {
                if let Some(entry) = local.get_mut(base) {
                    entry.1 = ClipRole::Base;
                }
                local.push((i, ClipRole::Clipped));
            }
        }
        emitted[at] = local
            .iter()
            .map(|&(i, _)| i)
            .filter(|&i| evals[i].visible && !(evals[i].is_ready_and_empty() && !adjustment_steps[i]))
            .collect();
        for (i, role) in local {
            roles[i].add(role);
        }
    }
    for (i, parent) in has_parent.iter().enumerate() {
        if !parent {
            roles[i].add(ClipRole::Unclipped);
        }
    }
    (roles, emitted)
}

/// Evaluate `plan` against `snap`. Fails with `StaleRevision` when the plan and snapshot revisions
/// differ, `Cycle` / `DanglingInput` when the (possibly hand-built) plan is not a topologically
/// ordered closure, and `Canceled` when the token fires (checked per step).
pub fn evaluate(
    plan: &LoweredPlan,
    snap: &Snapshot,
    cancel: &CancellationToken,
) -> Result<EvalReport, CompositeError> {
    evaluate_with(plan, snap, None, None, None, cancel)
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
    evaluate_with(plan, snap, Some(records), None, None, cancel)
}

pub(crate) fn evaluate_with(
    plan: &LoweredPlan,
    snap: &Snapshot,
    records: Option<&StudioRasterRecords>,
    text: Option<&crate::text::PreparedText>,
    vectors: Option<&crate::vector::PreparedVectors>,
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
    // Index-aligned with `evals`: the step is visible and carries a non-identity Blend-If range.
    // Its operator depends on the clip role in the parent group, which is known only once every
    // step is evaluated (see `clip_roles`), so the ledger entry is added after the loop.
    let mut blend_if_steps: Vec<bool> = Vec::with_capacity(plan.steps.len());
    // Index-aligned with `evals`: the step is visible and has Blend Clipped Layers as Group
    // switched off (it matters on a clip base that renders a clipped layer).
    let mut group_blend_off_steps: Vec<bool> = Vec::with_capacity(plan.steps.len());
    // Index-aligned with `evals`: the step is visible and carries a backdrop blur, which needs an
    // isolated parent group (a pass-through group has no own result to be the backdrop).
    let mut backdrop_blur_steps: Vec<bool> = Vec::with_capacity(plan.steps.len());
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
        let mut step_blend_if = false;
        let mut step_group_blend_off = false;
        let mut step_backdrop_blur = false;
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
            // A blur on a clip base is refused like effects there (pigment `BlurCombination`).
            step_effects = features.has_effects() || features.has_blur();
            step_backdrop_blur = features.has_backdrop_blur();
            step_blend_if = !features.blend_if.is_empty();
            step_group_blend_off = features.clipped_group_blend_off;
        }
        adjustment_steps.push(converted.is_some());
        effects_steps.push(step_effects);
        blend_if_steps.push(step_blend_if);
        group_blend_off_steps.push(step_group_blend_off);
        backdrop_blur_steps.push(step_backdrop_blur);
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
                    // Vector layers drawn on the host (crate::vector) when prepared. A vector
                    // source whose `out` port is image-typed (Folio SMB-FOLIO-VECTOR-IMAGE-PORT:
                    // a rasterisable vector feeding a group's image inputs) is drawn the same way.
                    (
                        None,
                        SourceValue::Vector | SourceValue::Image,
                        Some(Payload::Vector { .. }),
                    ) if vectors.is_some() => {
                        vector_status(vectors, &addr.layer_id, *visible, &mut ledger)
                    }
                    (None, SourceValue::Vector, _)
                    | (None, SourceValue::Image, Some(Payload::Vector { .. })) => (
                        None,
                        StepStatus::Unavailable("vector_to_image_conversion".into()),
                    ),
                    // Text drawn by Type (crate::text); without prepared text it stays unavailable.
                    // A text source whose `out` port is image-typed (Folio G1: rasterised text that
                    // feeds a group's image inputs) is drawn the same way.
                    (None, SourceValue::Text | SourceValue::Image, Some(Payload::Text { .. }))
                        if text.is_some() =>
                    {
                        match text.and_then(|t| t.plane(&addr.layer_id)) {
                            Some(plane) => {
                                if plane.placement.is_some() && *visible {
                                    record_operator(&mut ledger, Operator::Solid);
                                    record_operator(
                                        &mut ledger,
                                        Operator::Mask(MaskFormat::CoverageU8),
                                    );
                                }
                                (plane.placement, StepStatus::Ready)
                            }
                            None => (
                                None,
                                StepStatus::Unavailable("text_layer_not_prepared".into()),
                            ),
                        }
                    }
                    (None, SourceValue::Text, _)
                    | (None, SourceValue::Image, Some(Payload::Text { .. })) => (
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
            // A live boolean group (crate::vector) is drawn as one shape: a leaf, whatever its
            // inputs.
            Step::Composite { addr, visible, .. }
                if vectors.is_some_and(|v| v.knows(&addr.layer_id)) =>
            {
                let (extent, status) = vector_status(vectors, &addr.layer_id, *visible, &mut ledger);
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
                // The current clip base has Blend Clipped Layers as Group switched off (the
                // switched-off reading is not modelled, so a rendered clipped layer is refused).
                let mut group_blend_off_base = false;
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
                        group_blend_off_base = rendered && group_blend_off_steps[i];
                        base_dropped = !child.visible || (child.is_ready_and_empty() && !adjustment);
                    } else if base_dropped {
                        // Dropped with its hidden or empty base: not refused, not unavailable, no
                        // extent (R3: it used to block the parent through the status match below).
                        continue;
                    } else if pass_through_base && rendered {
                        ledger.record(REFUSE_CLIP_BASE_PASS_THROUGH, Exactness::Unsupported);
                        blocked.get_or_insert_with(|| {
                            format!("{REFUSE_CLIP_BASE_PASS_THROUGH}:{}", child.addr.text())
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
                    } else if group_blend_off_base && rendered {
                        // Pigment's clip rule is the Blend Clipped Layers as Group ON reading; the
                        // file's OFF setting is not modelled and is never ignored.
                        ledger.record(REFUSE_CLIP_GROUP_BLEND_OFF, Exactness::Unsupported);
                        blocked.get_or_insert_with(|| {
                            format!("{REFUSE_CLIP_GROUP_BLEND_OFF}:{}", child.addr.text())
                        });
                    }
                    if !child.visible {
                        continue;
                    }
                    if backdrop_blur_steps[i]
                        && step.appearance().mode == StudioBlendMode::PassThrough
                    {
                        ledger.record(REFUSE_BLUR_COMBINATION, Exactness::Unsupported);
                        blocked.get_or_insert_with(|| {
                            format!("{REFUSE_BLUR_COMBINATION}:{}", child.addr.text())
                        });
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
        let mask_block = vector_mask_block(vectors, step, converted.is_some());
        let eval = match appearance_block(&layers, step).or(ext_block).or(mask_block) {
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

    // Blend-If: pigment reports `BlendIf`, `BlendIfClipBase` or `BlendIfClippedLayer` by the clip
    // role the layer has in its group, and `EffectsWithBlendIf` for an unclipped layer that also
    // has effects; the clip operators and the combination are Unsupported (no public definition),
    // so the ledger mirrors pigment's admission (tests/render_plan_lowering.rs
    // `blend_if_on_clip_roles_is_reported_and_refused_like_pigment`,
    // `effects_with_blend_if_and_nested_effects_are_reported_like_pigment`).
    if records.is_some() {
        let (roles, emitted) = clip_roles(plan, &evals, &adjustment_steps, &seen);
        for (i, has_blend_if) in blend_if_steps.iter().enumerate() {
            if *has_blend_if {
                for role in roles[i].iter() {
                    record_ext(&mut ledger, blend_if_operator(role, effects_steps[i]));
                }
            }
        }
        // Nested effects, as pigment's validate defines them: a layer with effects whose content
        // (the group it composites) holds, directly or through nested group layers, an emitted
        // layer with effects. Steps are in dependency order, so one forward pass suffices. The
        // class is pigment's (Approximate); each backend declares whether it evaluates the nesting.
        let mut holds_effects = vec![false; plan.steps.len()];
        for (i, inputs) in emitted.iter().enumerate() {
            holds_effects[i] = inputs.iter().any(|&j| effects_steps[j] || holds_effects[j]);
            if effects_steps[i] && holds_effects[i] {
                record_ext(&mut ledger, ExtOperator::EffectsNested);
            }
        }
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
