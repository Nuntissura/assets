//! Additive extension envelope of the render-plan IR (decision: work-folder
//! `diagnostics/adjustments-advanced-blending.md`, topic additivity). The v1 types (`RenderPlan`,
//! `Layer`, `MaskInput`, `Operator`, `PlanNode`, `BackendError`) are built by struct literals or
//! matched exhaustively in composite, render-cpu and render-gpu, so features added after v1 travel
//! here, addressed by `(group node, layer index)`, next to an unchanged plan:
//! adjustment layers, Blend-If, mask feather, the mask-density capability query and knockout
//! (refused until implemented). [`admit_extended`] is the one shared admission for a plan with
//! its envelope; a backend opts in per operator through
//! [`RenderBackend::extension_exactness`] (default: nothing supported).
use super::{
    Applicability, BackendError, ExactnessLedger, ExecuteRequest, MaskFormat, Operator, PlanError,
    PlanNode, PlanNodeId, RenderBackend, RenderPlan, admit_core,
};
use crate::adjust_math::{ADJUSTMENT_LAYER_EXACTNESS, AdjustError, AdjustmentKind, AdjustmentOp};
use crate::blend_math::{Exactness, StudioBlendMode};
use crate::coverage_math::{
    BlendRangeEntry, COVERAGE_EXACTNESS, CoverageError, IDENTITY_RANGE, MAX_FEATHER_PX, check_range,
};
use crate::effect_math::{
    EFFECT_EXACTNESS, EffectError, EffectKind, EffectOp, MAX_EFFECTS_PER_LAYER,
    MAX_EFFECTS_PER_PLAN,
};
use hsk_studio_accord::CancellationToken;
use serde::Serialize;
use std::collections::BTreeSet;

/// Knockout depth of a layer (Photoshop advanced blending: none, shallow, deep). Only `None` is
/// admitted until a knockout implementation exists; the others are refused by
/// [`PlanExtensions::validate`] with [`PlanError::KnockoutUnsupported`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum Knockout {
    None,
    Shallow,
    Deep,
}
impl Knockout {
    pub const fn key(self) -> &'static str {
        match self {
            Self::None => "knockout_none",
            Self::Shallow => "knockout_shallow",
            Self::Deep => "knockout_deep",
        }
    }
}

/// Extension of one `Layer` of one `Group` node.
///
/// - `adjustment`: `Some` makes the layer an adjustment layer. Its `Layer.input` must be the
///   canonical placeholder `PlanNode::Solid { rect, rgba: [0.0; 4] }` (the rect bounds where the
///   adjustment applies). Content = `f(backdrop)`, where the backdrop is the group's running result
///   below this layer (transparent start in an isolated group, the parent backdrop in a
///   pass-through group, the run unit inside a clip-to-base run). Per pixel
///   `k = opacity * fill_opacity * mask * blend_if`, colour `lerp(Cb, B_mode(Cb, f(Cb)), k)`, alpha
///   unchanged ([`crate::adjust_math::composite_adjustment`]). It may not be a clip base.
/// - `blend_if`: Blend-If entries ([`crate::coverage_math::blend_if_weight`] multiplies the
///   source alpha). Empty or all-identity = no Blend-If. Refused on a pass-through layer.
/// - `mask_feather_px`: Gaussian feather radius of the layer's coverage mask
///   ([`crate::coverage_math::feather_kernel`], applied before density); `0` = none.
/// - `knockout`: only `Knockout::None` is admitted.
/// - `effects`: layer effects of wave 1 ([`crate::effect_math`]: drop shadow, colour overlay,
///   solid stroke), rendered in list order within each placement (behind the content, over the
///   content). Not allowed on an adjustment layer, a pass-through layer, a clipped layer or the
///   base of a clipping run. Empty = no effects (and absent from the digest).
///
/// `#[non_exhaustive]` so later envelope fields (effects wave) stay additive: build it with
/// [`LayerExtension::new`] and the `with_*` methods or field assignment.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LayerExtension {
    pub group: PlanNodeId,
    pub layer: u32,
    pub adjustment: Option<AdjustmentOp>,
    pub blend_if: Vec<BlendRangeEntry>,
    pub mask_feather_px: f32,
    pub knockout: Knockout,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<EffectOp>,
}
impl LayerExtension {
    /// An extension that changes nothing (fill in the fields you need).
    pub const fn new(group: PlanNodeId, layer: u32) -> Self {
        Self {
            group,
            layer,
            adjustment: None,
            blend_if: Vec::new(),
            mask_feather_px: 0.0,
            knockout: Knockout::None,
            effects: Vec::new(),
        }
    }
    pub fn with_adjustment(mut self, op: AdjustmentOp) -> Self {
        self.adjustment = Some(op);
        self
    }
    pub fn with_blend_if(mut self, entries: Vec<BlendRangeEntry>) -> Self {
        self.blend_if = entries;
        self
    }
    pub fn with_mask_feather(mut self, px: f32) -> Self {
        self.mask_feather_px = px;
        self
    }
    pub fn with_knockout(mut self, knockout: Knockout) -> Self {
        self.knockout = knockout;
        self
    }
    pub fn with_effects(mut self, effects: Vec<EffectOp>) -> Self {
        self.effects = effects;
        self
    }
}

/// The envelope: at most one entry per `(group, layer)`. Empty = the plain v1 plan.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct PlanExtensions {
    pub layers: Vec<LayerExtension>,
}

/// One envelope feature a plan uses; the unit of capability and exactness accounting next to the
/// v1 [`super::Operator`].
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExtOperator {
    /// The adjustment-layer compositing rule (alpha preserved).
    AdjustmentLayer,
    /// The per-pixel maths of one adjustment kind.
    Adjustment(AdjustmentKind),
    BlendIf,
    /// A coverage mask with `density < 1` (the v1 path keeps today's backend refusal).
    MaskDensity,
    MaskFeather,
    /// Not reported by `validate`, which refuses knockout (`tests/plan_extensions.rs`
    /// `envelope_validation_rules`); present for capability queries.
    Knockout(Knockout),
    /// One layer-effect kind (wave 1: drop shadow, colour overlay, stroke). The blend modes the
    /// effects use are admitted separately by [`admit_extended`] through the backend's v1
    /// [`Operator::Blend`] classes.
    Effect(EffectKind),
}
impl ExtOperator {
    pub const fn key(self) -> &'static str {
        match self {
            Self::AdjustmentLayer => "adjustment_layer",
            Self::Adjustment(k) => k.key(),
            Self::BlendIf => "blend_if",
            Self::MaskDensity => "mask_density",
            Self::MaskFeather => "mask_feather",
            Self::Knockout(k) => k.key(),
            Self::Effect(k) => k.key(),
        }
    }
    /// Class independent of any backend (formula provenance).
    pub const fn intrinsic(self) -> Exactness {
        match self {
            Self::AdjustmentLayer => ADJUSTMENT_LAYER_EXACTNESS,
            Self::Adjustment(k) => k.facts().exactness,
            Self::BlendIf | Self::MaskDensity | Self::MaskFeather => COVERAGE_EXACTNESS,
            Self::Knockout(Knockout::None) => Exactness::Exact,
            Self::Knockout(_) => Exactness::Unsupported,
            Self::Effect(_) => EFFECT_EXACTNESS,
        }
    }
}

fn is_clip(l: &super::Layer) -> bool {
    l.mask
        .as_ref()
        .is_some_and(super::MaskInput::is_clip_to_base)
}

impl PlanExtensions {
    /// An envelope holding `layers` (validated by [`PlanExtensions::validate`]).
    pub fn from_layers(layers: Vec<LayerExtension>) -> Self {
        Self { layers }
    }
    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }
    /// The extension of `(group, layer)`, if any.
    pub fn extension(&self, group: PlanNodeId, layer: u32) -> Option<&LayerExtension> {
        self.layers
            .iter()
            .find(|e| e.group == group && e.layer == layer)
    }

    /// Structural validation of the envelope against its plan (run after `plan.validate`).
    /// Returns the distinct envelope operators in first-use order.
    pub fn validate<V>(&self, plan: &RenderPlan<V>) -> Result<Vec<ExtOperator>, PlanError> {
        let mut ops: Vec<ExtOperator> = Vec::new();
        let mut add = |op: ExtOperator| {
            if !ops.contains(&op) {
                ops.push(op);
            }
        };
        let mut seen = BTreeSet::new();
        let mut total_effects = 0usize;
        for e in &self.layers {
            let node = e.group.0;
            let Some(PlanNode::Group(g)) = plan.nodes.get(node as usize) else {
                return Err(PlanError::ExtensionTarget { node });
            };
            let Some(layer) = g.layers.get(e.layer as usize) else {
                return Err(PlanError::ExtensionTarget { node });
            };
            if !seen.insert((node, e.layer)) {
                return Err(PlanError::ExtensionDuplicate { node });
            }
            if e.knockout != Knockout::None {
                return Err(PlanError::KnockoutUnsupported {
                    node,
                    depth: e.knockout,
                });
            }
            if let Some(op) = &e.adjustment {
                match plan.nodes.get(layer.input.0 as usize) {
                    Some(PlanNode::Solid { rgba, .. }) if *rgba == [0.0; 4] => {}
                    _ => return Err(PlanError::AdjustmentInput { node }),
                }
                if layer.mode.facts().applicability != Applicability::Layer {
                    return Err(PlanError::ModeNotApplicable {
                        node,
                        mode: layer.mode,
                    });
                }
                match op.prepare() {
                    Ok(_) => {}
                    Err(AdjustError::Unsupported(_)) => {
                        return Err(PlanError::AdjustmentUnsupported {
                            node,
                            kind: op.kind(),
                        });
                    }
                    Err(_) => return Err(PlanError::AdjustmentParameter { node }),
                }
                // A clipped layer's base is the nearest earlier unclipped layer: the next layer
                // being clipped makes this (unclipped) adjustment layer its base.
                let next_clipped = g.layers.get(e.layer as usize + 1).is_some_and(is_clip);
                if !is_clip(layer) && next_clipped {
                    return Err(PlanError::AdjustmentClipBase { node });
                }
                add(ExtOperator::AdjustmentLayer);
                add(ExtOperator::Adjustment(op.kind()));
            }
            if !e.effects.is_empty() {
                if e.adjustment.is_some() {
                    return Err(PlanError::EffectOnAdjustment { node });
                }
                if layer.mode == StudioBlendMode::PassThrough {
                    return Err(PlanError::EffectPassThrough { node });
                }
                if is_clip(layer) || g.layers.get(e.layer as usize + 1).is_some_and(is_clip) {
                    return Err(PlanError::EffectClipRun { node });
                }
                total_effects = total_effects.saturating_add(e.effects.len());
                if e.effects.len() > MAX_EFFECTS_PER_LAYER || total_effects > MAX_EFFECTS_PER_PLAN
                {
                    return Err(PlanError::EffectLimit { node });
                }
                for effect in &e.effects {
                    effect.validate().map_err(|err| match err {
                        EffectError::SizeOutOfRange | EffectError::DistanceOutOfRange => {
                            PlanError::EffectLimit { node }
                        }
                        EffectError::ModeNotApplicable(mode) => {
                            PlanError::ModeNotApplicable { node, mode }
                        }
                        _ => PlanError::EffectParameter { node },
                    })?;
                    add(ExtOperator::Effect(effect.kind()));
                }
            }
            if !e.blend_if.is_empty() {
                if layer.mode == StudioBlendMode::PassThrough {
                    return Err(PlanError::BlendIfPassThrough { node });
                }
                let mut channels = BTreeSet::new();
                for entry in &e.blend_if {
                    if !channels.insert(entry.channel.key()) {
                        return Err(PlanError::BlendIfRange { node });
                    }
                    for r in [entry.source, entry.destination] {
                        check_range(r).map_err(|e| match e {
                            CoverageError::OutOfDomain => PlanError::OutOfRange { node },
                            _ => PlanError::BlendIfRange { node },
                        })?;
                    }
                }
                if e.blend_if
                    .iter()
                    .any(|x| x.source != IDENTITY_RANGE || x.destination != IDENTITY_RANGE)
                {
                    add(ExtOperator::BlendIf);
                }
            }
            if !e.mask_feather_px.is_finite() {
                return Err(PlanError::NonFinite { node });
            }
            if !(0.0..=MAX_FEATHER_PX).contains(&e.mask_feather_px) {
                return Err(PlanError::OutOfRange { node });
            }
            if e.mask_feather_px > 0.0 {
                if !layer
                    .mask
                    .as_ref()
                    .is_some_and(|m| m.format != MaskFormat::BaseAlpha)
                {
                    return Err(PlanError::FeatherWithoutMask { node });
                }
                add(ExtOperator::MaskFeather);
            }
        }
        if plan.has_mask_density() {
            add(ExtOperator::MaskDensity);
        }
        Ok(ops)
    }

    /// The distinct blend modes the envelope's effects composite with, in first-use order
    /// ([`admit_extended`] asks the backend for each mode's v1 [`Operator::Blend`] class).
    pub fn effect_modes(&self) -> Vec<StudioBlendMode> {
        let mut modes: Vec<StudioBlendMode> = Vec::new();
        for effect in self.layers.iter().flat_map(|e| e.effects.iter()) {
            if !modes.contains(&effect.mode()) {
                modes.push(effect.mode());
            }
        }
        modes
    }

    /// SHA-256 binding a receipt to the plan and its envelope. An empty envelope digests exactly
    /// like the plain plan ([`RenderPlan::digest`]).
    pub fn digest_with<V: Serialize>(
        &self,
        plan: &RenderPlan<V>,
    ) -> Result<[u8; 32], crate::Error> {
        if self.is_empty() {
            return plan.digest();
        }
        let bytes = serde_json::to_vec(&(plan, self)).map_err(|_| crate::Error::InvalidInput)?;
        Ok(hsk_studio_prism::profile_hash(&bytes))
    }
}

/// Shared admission for a plan with its envelope: [`admit`](super::admit) for the v1 plan (except its v1-only
/// mask-density refusal: density is the envelope operator `MaskDensity` here), then envelope
/// validation, then per envelope operator class = intrinsic lowered by
/// [`RenderBackend::extension_exactness`]; refuses Unsupported or below `min_exactness` before any
/// sink call. Knockout and refused adjustment features map to `BackendError::Unsupported` naming
/// the feature; other envelope defects map to `BackendError::InvalidPlan`.
pub fn admit_extended<V, B: RenderBackend<V> + ?Sized>(
    plan: &RenderPlan<V>,
    extensions: &PlanExtensions,
    backend: &B,
    request: &ExecuteRequest,
    cancel: &CancellationToken,
) -> Result<ExactnessLedger, BackendError> {
    let mut ledger = admit_core(plan, backend, request, cancel)?;
    let operators = extensions.validate(plan).map_err(|e| match e {
        PlanError::KnockoutUnsupported { depth, .. } => BackendError::Unsupported {
            operator: depth.key(),
            exactness: Exactness::Unsupported,
        },
        PlanError::AdjustmentUnsupported { kind, .. } => BackendError::Unsupported {
            operator: kind.key(),
            exactness: Exactness::Unsupported,
        },
        other => BackendError::InvalidPlan(other),
    })?;
    for op in operators {
        let class = op.intrinsic().lowest_of(backend.extension_exactness(op));
        if class == Exactness::Unsupported || class.rank() > request.min_exactness.rank() {
            return Err(BackendError::Unsupported {
                operator: op.key(),
                exactness: class,
            });
        }
        ledger.record(op.key(), class);
    }
    // An effect composites with its own blend mode: the backend's v1 class for that mode applies
    // (an effect is refused when its mode is: `tests/effects.rs`
    // `effect_without_backend_support_is_refused_before_the_sink`).
    for mode in extensions.effect_modes() {
        let op = Operator::Blend(mode);
        let class = op.intrinsic().lowest_of(backend.exactness(op));
        if class == Exactness::Unsupported || class.rank() > request.min_exactness.rank() {
            return Err(BackendError::Unsupported {
                operator: op.key(),
                exactness: class,
            });
        }
        ledger.record(op.key(), class);
    }
    Ok(ledger)
}
