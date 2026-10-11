//! Lower an evaluated composite plan into pigment's shared render-plan IR (decision record
//! `diagnostics/render-plan-ir.md`). Composite owns graph semantics and authored ids; the IR is
//! plain, folio-free data that render-cpu / render-gpu execute. The key -> record tables stay on
//! the composite side (`SourceTable`, `MaskTable`), because the IR addresses sources by content
//! hash and `SourceKey` only.
//!
//! Mapping: a raster layer becomes one `Tile` node (one tile) or an isolated group of `Tile`
//! nodes (several tiles, Normal, disjoint grid cells); a composite step becomes a `Group` whose
//! layers follow the authored `input_ordinal` order, bottom-first. Each referencing `Layer`
//! carries the referenced layer's authored appearance (STU-RAS-006): `StudioBlendMode`, opacity
//! and fill opacity as fractions, and an enabled mask as a `MaskInput` over the union of its
//! coverage tiles (density 1, not inverted: Folio does not store either yet). A group follows
//! its mode: pass_through = non-isolated, any other mode = isolated.
//!
//! Clipping (Folio `clipped`): a clipped layer clips to the nearest unclipped layer below it in
//! the same group and lowers to pigment's clip-to-base route (`MaskInput::clip_to_base`, Approximate:
//! the Photoshop rule is UNVERIFIED). Hidden base = clipped layers dropped with it. Never silently
//! ignored: a clipped layer with no base, with its own mask (one mask slot) or referencing a
//! pass-through group, a pass-through group serving as the clip base (`clip_base_pass_through`,
//! Unsupported in the evaluation ledger, consistent with the backends; Photoshop's semantics for it
//! are UNVERIFIED pending the oracle), and an enabled mask without coverage, are `Unavailable`. A
//! hidden or empty pass-through base drops its clipped layers instead. A disabled mask is
//! an authored "no mask" and is omitted. Hidden and empty layers are not emitted.
//!
//! The requested root is rendered regardless of its own visibility flag. Its own appearance is
//! honoured by wrapping it in one synthetic isolated group when it is not neutral over a
//! transparent backdrop (opacity or fill below 1, or an enabled mask); the blend mode itself is
//! irrelevant over a transparent backdrop (W3C `ab = 0` gives `Cs' = Cs`).
use crate::{
    CompositeError,
    evaluate_static::{EvalReport, StepStatus, tile_rects, union},
    lower_plan::{LoweredPlan, NodeAddr, SourceValue, Step, StepAppearance},
};
use hsk_studio_folio::{Payload, Snapshot, StudioLayer, TileRef};
use hsk_studio_pigment::blend_math::{ExactnessLedger, StudioBlendMode};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, Group, Layer, MaskFormat, MaskInput, OutputTarget, PlanError, PlanLimits,
    PlanNode, PlanNodeId, RenderPlan, SampleFormat, SourceKey, TileInput,
};
use std::collections::{BTreeMap, BTreeSet};

/// What the caller knows about one resolved tile that Folio's authored `TileRef` cannot state:
/// the plane hashes and the declared sample/alpha convention of the resolved bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileInfo {
    pub format: SampleFormat,
    pub alpha: AlphaConvention,
    pub colour_sha256: [u8; 32],
    pub alpha_sha256: [u8; 32],
}

/// Caller-owned lookup (host artifact store / resolved tiles). `None` = not resolvable, which
/// makes the lowering `Unavailable` instead of inventing hashes.
pub trait TileInfoSource {
    fn tile_info(&self, tile: &TileRef) -> Option<TileInfo>;
    /// SHA-256 of the single coverage plane the caller resolves from ALL of one mask's tiles
    /// (stitched over the union of their placements). Default: unresolvable.
    fn mask_sha256(&self, _tiles: &[TileRef]) -> Option<[u8; 32]> {
        None
    }
}

impl<F: Fn(&TileRef) -> Option<TileInfo>> TileInfoSource for F {
    fn tile_info(&self, tile: &TileRef) -> Option<TileInfo> {
        self(tile)
    }
}

/// `SourceKey` -> authored tile record, in key order.
pub type SourceTable = Vec<(SourceKey, TileRef)>;
/// `SourceKey` -> the authored coverage tiles that make up one mask plane.
pub type MaskTable = Vec<(SourceKey, Vec<TileRef>)>;

#[derive(Clone, Debug, PartialEq)]
pub struct RenderLowering {
    pub plan: RenderPlan,
    pub sources: SourceTable,
    pub mask_sources: MaskTable,
    /// Intrinsic class of every operator the validated plan uses (formula provenance only; a
    /// backend can lower it further at admission).
    pub exactness: ExactnessLedger,
}

struct Emitter<'a> {
    nodes: Vec<PlanNode>,
    sources: SourceTable,
    mask_sources: MaskTable,
    next_key: u32,
    info: &'a dyn TileInfoSource,
}

impl Emitter<'_> {
    fn push(&mut self, node: PlanNode) -> Result<PlanNodeId, CompositeError> {
        let id = u32::try_from(self.nodes.len()).map_err(|_| CompositeError::Overflow)?;
        self.nodes.push(node);
        Ok(PlanNodeId(id))
    }
    fn key(&mut self) -> Result<SourceKey, CompositeError> {
        let key = SourceKey(self.next_key);
        self.next_key = self.next_key.checked_add(1).ok_or(CompositeError::Overflow)?;
        Ok(key)
    }
    /// The referencing layer: the referenced layer's authored appearance plus its mask. A clipped
    /// layer carries pigment's clip-to-base route in its single mask slot, so a clipped layer
    /// with its own mask is refused (evaluation already marks it unavailable).
    fn layer(
        &mut self,
        input: PlanNodeId,
        appearance: &StepAppearance,
        layer: &StudioLayer,
    ) -> Result<Layer, CompositeError> {
        let mask = match (appearance.clipped, appearance.mask) {
            (true, true) => {
                return Err(CompositeError::Unavailable(format!(
                    "{}:clipped_with_mask_unsupported",
                    layer.layer_id
                )));
            }
            (true, false) => Some(MaskInput::clip_to_base()),
            (false, true) => Some(self.mask(layer)?),
            (false, false) => None,
        };
        Ok(Layer {
            input,
            mode: appearance.mode,
            opacity: appearance.opacity,
            fill_opacity: appearance.fill_opacity,
            mask,
        })
    }
    fn mask(&mut self, layer: &StudioLayer) -> Result<MaskInput, CompositeError> {
        let tiles = layer
            .appearance
            .as_ref()
            .and_then(|a| a.mask.as_ref())
            .map(|m| m.tiles.as_slice())
            .filter(|t| !t.is_empty())
            .ok_or_else(|| {
                CompositeError::Unavailable(format!("{}:mask_empty", layer.layer_id))
            })?;
        let format = mask_format(tiles)?;
        let mut placement = None;
        for rect in tile_rects(tiles)? {
            placement = union(placement, rect)?;
        }
        let placement = placement.ok_or(CompositeError::Plan(PlanError::Empty))?;
        let sha256 = self.info.mask_sha256(tiles).ok_or_else(|| {
            CompositeError::Unavailable(format!("{}/mask:mask_hash", layer.layer_id))
        })?;
        let key = self.key()?;
        self.mask_sources.push((key, tiles.to_vec()));
        Ok(MaskInput {
            key,
            placement,
            format,
            sha256,
            inverted: false,
            density: 1.0,
        })
    }
}

fn mask_format(tiles: &[TileRef]) -> Result<MaskFormat, CompositeError> {
    let format = |t: &TileRef| match t.format.as_str() {
        "coverage_u8" => Some(MaskFormat::CoverageU8),
        "coverage_u16le" => Some(MaskFormat::CoverageU16Le),
        _ => None,
    };
    let first = tiles.first().and_then(format);
    match first {
        Some(f) if tiles.iter().all(|t| format(t) == Some(f)) => Ok(f),
        _ => Err(CompositeError::UnsupportedDescriptor("mask_format".into())),
    }
}

/// Lower `root` of an evaluated plan. The result has passed `RenderPlan::validate`.
///
/// Fails `Unavailable` when the blending profile is unbound, when any step reachable from `root`
/// is unavailable (including clipped layers and empty masks), when a reachable layer has an
/// unresolved document issue, or when `info` cannot describe a reachable tile or mask;
/// `ColourMismatch` when the target's declared blend space differs from the plan's;
/// `StaleRevision` when plan, report and snapshot revisions differ.
pub fn lower_to_render_plan(
    plan: &LoweredPlan,
    report: &EvalReport,
    snap: &Snapshot,
    root: &NodeAddr,
    target: OutputTarget,
    info: &dyn TileInfoSource,
) -> Result<RenderLowering, CompositeError> {
    let doc = snap.document();
    for revision in [plan.revision, report.receipt.revision] {
        if revision != doc.revision {
            return Err(CompositeError::StaleRevision {
                expected: revision,
                actual: doc.revision,
            });
        }
    }
    if !plan.blend_profile_bound {
        return Err(CompositeError::Unavailable(
            "blending_space:unassigned_profile".into(),
        ));
    }
    if target.space.blend_space != plan.blend_space {
        return Err(CompositeError::ColourMismatch);
    }
    let index: BTreeMap<&NodeAddr, usize> = plan
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.addr(), i))
        .collect();
    if index.len() != plan.steps.len() || report.steps.len() != plan.steps.len() {
        return Err(CompositeError::UnsupportedDescriptor(
            "plan_report_mismatch".into(),
        ));
    }
    let root_index = *index
        .get(root)
        .ok_or_else(|| CompositeError::DanglingInput(root.text()))?;

    if plan.steps[root_index].appearance().clipped {
        // A root has no siblings, hence no clip base.
        return Err(CompositeError::Unavailable(format!(
            "{}:clip_without_base",
            root.text()
        )));
    }

    // Phase 1: steps reachable from the root through visible inputs. A clipped input clips to the
    // nearest unclipped input below it; with a hidden base it is not part of the render.
    let mut reachable = BTreeSet::new();
    let mut stack = vec![root_index];
    while let Some(i) = stack.pop() {
        if !reachable.insert(i) {
            continue;
        }
        if let Step::Composite { inputs, .. } = &plan.steps[i] {
            let mut base_visible: Option<bool> = None;
            for input in inputs {
                let child = *index
                    .get(input)
                    .ok_or_else(|| CompositeError::DanglingInput(input.text()))?;
                if child >= i {
                    return Err(CompositeError::Cycle(input.text()));
                }
                let step = &plan.steps[child];
                if step.appearance().clipped {
                    match base_visible {
                        None => {
                            return Err(CompositeError::Unavailable(format!(
                                "{}:clip_without_base",
                                input.text()
                            )));
                        }
                        Some(false) => continue,
                        Some(true) => {}
                    }
                } else {
                    base_visible = Some(step.visible());
                }
                if step.visible() {
                    stack.push(child);
                }
            }
        }
    }
    for i in &reachable {
        let eval = &report.steps[*i];
        if eval.addr != *plan.steps[*i].addr() {
            return Err(CompositeError::UnsupportedDescriptor(
                "plan_report_mismatch".into(),
            ));
        }
        if let StepStatus::Unavailable(reason) = &eval.status {
            return Err(CompositeError::Unavailable(format!(
                "{}:{reason}",
                eval.addr.text()
            )));
        }
        for u in &report.unavailable {
            if u.target.starts_with(&eval.addr.layer_id) {
                return Err(CompositeError::Unavailable(format!(
                    "{}:{}",
                    u.target, u.reason
                )));
            }
        }
    }

    // Phase 2: emit nodes in step order (every reference points to an earlier node).
    let layers: BTreeMap<&str, &StudioLayer> = doc
        .layers
        .iter()
        .map(|l| (l.layer_id.as_str(), l))
        .collect();
    let mut out = Emitter {
        nodes: Vec::new(),
        sources: Vec::new(),
        mask_sources: Vec::new(),
        next_key: 0,
        info,
    };
    let mut node_of: BTreeMap<usize, PlanNodeId> = BTreeMap::new();
    let root_neutral = plan.steps[root_index]
        .appearance()
        .is_neutral_over_transparent();
    for (i, step) in plan.steps.iter().enumerate() {
        if !reachable.contains(&i) {
            continue;
        }
        match step {
            Step::Source { addr, value, .. } => {
                let (SourceValue::Image, Some(Payload::Raster { tiles: records })) = (
                    value,
                    layers.get(addr.layer_id.as_str()).map(|l| &l.payload),
                ) else {
                    return Err(CompositeError::Unavailable(format!(
                        "{}:unsupported_payload",
                        addr.text()
                    )));
                };
                let rects = tile_rects(records)?;
                let mut tile_nodes = Vec::with_capacity(records.len());
                for (record, rect) in records.iter().zip(rects) {
                    let found = out.info.tile_info(record).ok_or_else(|| {
                        CompositeError::Unavailable(format!(
                            "{}/tile/{}:tile_info",
                            record.layer_id, record.object_key
                        ))
                    })?;
                    let key = out.key()?;
                    out.sources.push((key, record.clone()));
                    let id = out.push(PlanNode::Tile(TileInput {
                        key,
                        placement: rect,
                        format: found.format,
                        alpha: found.alpha,
                        colour_sha256: found.colour_sha256,
                        alpha_sha256: found.alpha_sha256,
                    }))?;
                    tile_nodes.push(id);
                }
                match tile_nodes.as_slice() {
                    [] => {}
                    [only] => {
                        node_of.insert(i, *only);
                    }
                    many => {
                        let mut group_layers = Vec::with_capacity(many.len());
                        for id in many {
                            group_layers.push(Layer {
                                input: *id,
                                mode: StudioBlendMode::Normal,
                                opacity: 1.0,
                                fill_opacity: 1.0,
                                mask: None,
                            });
                        }
                        let id = out.push(PlanNode::Group(Group {
                            isolated: true,
                            layers: group_layers,
                        }))?;
                        node_of.insert(i, id);
                    }
                }
            }
            Step::Composite {
                inputs, appearance, ..
            } => {
                let mut group_layers = Vec::with_capacity(inputs.len());
                // Was the nearest unclipped input below emitted? (`None`: no unclipped input yet.)
                let mut base_emitted: Option<bool> = None;
                // Is that base a pass-through group (no isolated content to take a base alpha from)?
                let mut base_pass_through = false;
                for input in inputs {
                    let child = index[input];
                    let emitted = if plan.steps[child].visible() {
                        node_of.get(&child).copied()
                    } else {
                        None
                    };
                    if plan.steps[child].appearance().clipped {
                        // Emitted clipped layer on a pass-through base: backends refuse it, and
                        // Photoshop's semantics are UNVERIFIED (oracle pending), so never guess.
                        if base_pass_through && emitted.is_some() {
                            return Err(CompositeError::Unavailable(format!(
                                "{}:clip_base_pass_through",
                                input.text()
                            )));
                        }
                        match base_emitted {
                            None => {
                                return Err(CompositeError::Unavailable(format!(
                                    "{}:clip_without_base",
                                    input.text()
                                )));
                            }
                            // Nothing to clip to: the clipped layer contributes nothing.
                            Some(false) => continue,
                            Some(true) => {}
                        }
                    } else {
                        base_emitted = Some(emitted.is_some());
                        base_pass_through = emitted.is_some()
                            && plan.steps[child].appearance().mode == StudioBlendMode::PassThrough;
                    }
                    let Some(id) = emitted.as_ref() else {
                        continue;
                    };
                    let layer = layers
                        .get(input.layer_id.as_str())
                        .ok_or_else(|| CompositeError::DanglingInput(input.text()))?;
                    group_layers.push(out.layer(*id, plan.steps[child].appearance(), layer)?);
                }
                let is_root = i == root_index;
                if group_layers.is_empty() && !is_root {
                    continue;
                }
                let group = Group {
                    // A neutral root renders over a transparent backdrop, where isolated and
                    // pass-through are equivalent; every other group follows its mode.
                    isolated: (is_root && root_neutral)
                        || appearance.mode
                            != StudioBlendMode::PassThrough,
                    layers: group_layers,
                };
                let id = out.push(PlanNode::Group(group))?;
                node_of.insert(i, id);
            }
        }
    }
    let mut root_node = *node_of
        .get(&root_index)
        .ok_or(CompositeError::Plan(PlanError::Empty))?;
    if !root_neutral {
        // The root's own opacity / fill / mask apply: one synthetic isolated wrapper layer.
        let root_layer = layers
            .get(root.layer_id.as_str())
            .ok_or_else(|| CompositeError::DanglingInput(root.text()))?;
        let layer = out.layer(root_node, plan.steps[root_index].appearance(), root_layer)?;
        root_node = out.push(PlanNode::Group(Group {
            isolated: true,
            layers: vec![layer],
        }))?;
    }
    let render = RenderPlan {
        revision: plan.revision,
        target,
        nodes: out.nodes,
        root: root_node,
    };
    let operators = render
        .validate(&PlanLimits::default())
        .map_err(CompositeError::Plan)?;
    let mut exactness = ExactnessLedger::default();
    for op in operators {
        exactness.record(op.key(), op.intrinsic());
    }
    Ok(RenderLowering {
        plan: render,
        sources: out.sources,
        mask_sources: out.mask_sources,
        exactness,
    })
}
