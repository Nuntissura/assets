//! Effects together with Blend-If on one layer, and effects nested in the content of a layer with
//! effects (decision: work-folder `diagnostics/adjustments-advanced-blending.md`, topic
//! `effects-ops`): pigment's shared admission decides both for every backend, so no backend
//! refuses them locally after admission. Node and layer indexes and expected operators are derived
//! by hand from the plans built in this file.
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::coverage_math::{BlendChannel, BlendRangeEntry, IDENTITY_RANGE};
use hsk_studio_pigment::effect_math::{DropShadowOp, EffectKind, EffectOp};
use hsk_studio_pigment::render_plan::*;
use hsk_studio_pigment::{Mask, Rect};

const R: Rect = Rect {
    x: 0,
    y: 0,
    width: 2,
    height: 1,
};

fn layer(input: u32) -> Layer {
    Layer {
        input: PlanNodeId(input),
        mode: StudioBlendMode::Normal,
        opacity: 1.0,
        fill_opacity: 1.0,
        mask: None,
    }
}
fn solid() -> PlanNode {
    PlanNode::Solid {
        rect: R,
        rgba: [0.2, 0.4, 0.6, 1.0],
    }
}
fn group(layers: Vec<Layer>) -> PlanNode {
    PlanNode::Group(Group {
        isolated: true,
        layers,
    })
}
fn plan(nodes: Vec<PlanNode>) -> RenderPlan {
    let root = u32::try_from(nodes.len() - 1).unwrap();
    RenderPlan {
        revision: 3,
        target: OutputTarget {
            extent: R,
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space: ColourTag {
                profile_sha256: [7; 32],
                blend_space: BlendSpace::Encoded,
            },
        },
        nodes,
        root: PlanNodeId(root),
    }
}
fn shadow() -> Vec<EffectOp> {
    vec![EffectOp::DropShadow(DropShadowOp::new(
        [0.0; 3],
        StudioBlendMode::Normal,
        0.5,
    ))]
}
fn ramp() -> Vec<BlendRangeEntry> {
    vec![BlendRangeEntry {
        channel: BlendChannel::Gray,
        source: [0.2, 0.4, 0.6, 1.0],
        destination: IDENTITY_RANGE,
    }]
}
fn fx(group: u32, layer: u32) -> LayerExtension {
    LayerExtension::new(PlanNodeId(group), layer).with_effects(shadow())
}

const DROP_SHADOW: ExtOperator = ExtOperator::Effect(EffectKind::DropShadow);

/// Claims the effect kinds at `effects`, `EffectsNested` at `nested` and every other envelope
/// operator at `rest`. `Claims::all(Exact)` is the most permissive backend that can exist: the
/// refusal of an intrinsically unsupported operator must not depend on what a backend claims.
struct Claims {
    effects: Exactness,
    nested: Exactness,
    rest: Exactness,
}
impl Claims {
    fn all(class: Exactness) -> Self {
        Self {
            effects: class,
            nested: class,
            rest: class,
        }
    }
}
impl RenderBackend for Claims {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            name: "claims",
            version: "0",
            device: DeviceClass::Cpu,
            adapter: String::new(),
            generation: 0,
        }
    }
    fn exactness(&self, _: Operator) -> Exactness {
        Exactness::Exact
    }
    fn extension_exactness(&self, op: ExtOperator) -> Exactness {
        match op {
            ExtOperator::Effect(_) => self.effects,
            ExtOperator::EffectsNested => self.nested,
            _ => self.rest,
        }
    }
    fn execute(
        &self,
        _: &RenderPlan,
        _: &ExecuteRequest,
        _: &dyn PlanSources,
        _: &CancellationToken,
        _: &mut dyn TileSink,
    ) -> Result<RenderReceipt, BackendError> {
        Err(BackendError::Unavailable("claims_execute"))
    }
}
struct NoSources;
impl PlanSources for NoSources {
    fn tile(&self, _: SourceKey) -> Option<TilePlanes<'_>> {
        None
    }
    fn mask(&self, _: SourceKey) -> Option<Mask<'_>> {
        None
    }
}
struct CountingSink(u32);
impl TileSink for CountingSink {
    fn accept(&mut self, _: Rect, _: &[u8], _: &[u8]) -> Result<(), SinkError> {
        self.0 += 1;
        Ok(())
    }
}
fn req() -> ExecuteRequest {
    ExecuteRequest {
        expected_revision: 3,
        min_exactness: Exactness::Approximate,
        deadline: None,
        max_working_bytes: 1 << 20,
    }
}
fn refused(key: &'static str) -> Result<(), BackendError> {
    Err(BackendError::Unsupported {
        operator: key,
        exactness: Exactness::Unsupported,
    })
}
fn admit(
    p: &RenderPlan,
    ext: &PlanExtensions,
    backend: &Claims,
) -> Result<ExactnessLedger, BackendError> {
    admit_extended(p, ext, backend, &req(), &CancellationToken::default())
}
/// The refusal is the same at the default `execute_extended` route and reaches no sink.
fn refused_before_any_sink_call(
    p: &RenderPlan,
    ext: &PlanExtensions,
    backend: &Claims,
    key: &'static str,
) {
    let mut sink = CountingSink(0);
    let run = backend.execute_extended(
        p,
        ext,
        &req(),
        &NoSources,
        &CancellationToken::default(),
        &mut sink,
    );
    assert_eq!(run.map(|_| ()), refused(key));
    assert_eq!(sink.0, 0);
}

/// Nodes: 0 solid | 1 group {layer 0: node 0, layer 1: node 0} (root).
fn flat() -> RenderPlan {
    plan(vec![solid(), group(vec![layer(0), layer(0)])])
}

#[test]
fn keys_and_classes_of_the_new_operators() {
    assert_eq!(
        ExtOperator::EffectsWithBlendIf.key(),
        "effects_with_blend_if"
    );
    assert_eq!(
        ExtOperator::EffectsWithBlendIf.intrinsic(),
        Exactness::Unsupported
    );
    assert_eq!(ExtOperator::EffectsNested.key(), "effects_nested");
    // Classed like the effects themselves.
    assert_eq!(
        ExtOperator::EffectsNested.intrinsic(),
        DROP_SHADOW.intrinsic()
    );
    assert_eq!(
        ExtOperator::EffectsNested.intrinsic(),
        Exactness::Approximate
    );
}

#[test]
fn effects_with_blend_if_is_reported_by_validate_instead_of_blend_if() {
    let p = flat();
    // Layer 0: effects and a non-identity Blend-If -> the combination, not `blend_if`.
    let both = PlanExtensions::from_layers(vec![fx(1, 0).with_blend_if(ramp())]);
    assert_eq!(
        both.validate(&p),
        Ok(vec![DROP_SHADOW, ExtOperator::EffectsWithBlendIf])
    );
    // Counterfactuals: each feature alone, and both on different layers, are not the combination.
    let effects_only = PlanExtensions::from_layers(vec![fx(1, 0)]);
    assert_eq!(effects_only.validate(&p), Ok(vec![DROP_SHADOW]));
    let blend_if_only = PlanExtensions::from_layers(vec![
        LayerExtension::new(PlanNodeId(1), 0).with_blend_if(ramp()),
    ]);
    assert_eq!(blend_if_only.validate(&p), Ok(vec![ExtOperator::BlendIf]));
    let apart = PlanExtensions::from_layers(vec![
        fx(1, 0),
        LayerExtension::new(PlanNodeId(1), 1).with_blend_if(ramp()),
    ]);
    assert_eq!(
        apart.validate(&p),
        Ok(vec![DROP_SHADOW, ExtOperator::BlendIf])
    );
    // All-identity ranges are not Blend-If.
    let identity = PlanExtensions::from_layers(vec![
        fx(1, 0).with_blend_if(vec![BlendRangeEntry::identity(BlendChannel::Gray)]),
    ]);
    assert_eq!(identity.validate(&p), Ok(vec![DROP_SHADOW]));
}

#[test]
fn effects_with_blend_if_is_refused_for_every_backend_before_the_sink() {
    let p = flat();
    let both = PlanExtensions::from_layers(vec![fx(1, 0).with_blend_if(ramp())]);
    // An all-Exact backend, an all-Approximate one, and one that claims only effects and
    // Blend-If at their natural class: the same typed refusal (the claim is irrelevant because
    // pigment rates the combination Unsupported).
    for backend in [
        Claims::all(Exactness::Exact),
        Claims::all(Exactness::Approximate),
        Claims {
            effects: Exactness::Approximate,
            nested: Exactness::Unsupported,
            rest: Exactness::Approximate,
        },
    ] {
        assert_eq!(
            admit(&p, &both, &backend).map(|_| ()),
            refused("effects_with_blend_if")
        );
        refused_before_any_sink_call(&p, &both, &backend, "effects_with_blend_if");
    }
    // Counterfactual: the same backend admits the effects alone (Approximate) and Blend-If alone
    // on the other layer.
    let exact = Claims::all(Exactness::Exact);
    let apart = PlanExtensions::from_layers(vec![
        fx(1, 0),
        LayerExtension::new(PlanNodeId(1), 1).with_blend_if(ramp()),
    ]);
    let ledger = admit(&p, &apart, &exact).unwrap();
    assert_eq!(ledger.lowest, Exactness::Approximate);
    for key in ["drop_shadow", "blend_if"] {
        assert!(
            ledger
                .operators
                .iter()
                .any(|o| o.key == key && o.exactness == Exactness::Approximate),
            "{key}"
        );
    }
    assert!(
        ledger
            .operators
            .iter()
            .all(|o| o.key != "effects_with_blend_if")
    );
}

/// Nodes: 0 solid | 1 inner group {0} | 2 outer group {layer 0: node 1} | 3 outer group of a
/// chain {layer 0: node 2} | 4 sibling group {layer 0: node 1, layer 1: node 0} | 5 the root, whose
/// layers reference nodes 2, 3 and 4, so every group is reached from the root (the envelope
/// entries of a group no layer reaches ask nothing of a backend, PLR-069; this fixture's root was
/// node 4 before that rule, so nodes 2 and 3 were never reached).
fn nested() -> RenderPlan {
    plan(vec![
        solid(),
        group(vec![layer(0)]),
        group(vec![layer(1)]),
        group(vec![layer(2)]),
        group(vec![layer(1), layer(0)]),
        group(vec![layer(2), layer(3), layer(4)]),
    ])
}

#[test]
fn effects_nested_is_reported_when_the_content_holds_effects() {
    let p = nested();
    // Effects on the inner layer (1,0) and on the layer that references the inner group (2,0).
    let both = PlanExtensions::from_layers(vec![fx(1, 0), fx(2, 0)]);
    assert_eq!(
        both.validate(&p),
        Ok(vec![DROP_SHADOW, ExtOperator::EffectsNested])
    );
    // Through a middle group without effects (3,0) -> 2 -> 1: still nested.
    let chain = PlanExtensions::from_layers(vec![fx(1, 0), fx(3, 0)]);
    assert_eq!(
        chain.validate(&p),
        Ok(vec![DROP_SHADOW, ExtOperator::EffectsNested])
    );
    // Counterfactuals: only the inner layer, only the outer layer, and an outer layer whose
    // content has no effects anywhere are not nested.
    for ext in [vec![fx(1, 0)], vec![fx(2, 0)], vec![fx(3, 0)]] {
        assert_eq!(
            PlanExtensions::from_layers(ext).validate(&p),
            Ok(vec![DROP_SHADOW])
        );
    }
    // A sibling layer with effects over a plain tile does not make its neighbour's content nested:
    // (4,1) has effects over the solid (node 0); (4,0) references the inner group but has none.
    let sibling = PlanExtensions::from_layers(vec![fx(4, 1)]);
    assert_eq!(sibling.validate(&p), Ok(vec![DROP_SHADOW]));
    // ... while effects on both the inner group's layer and the sibling referencing it are nested.
    let sibling_nested = PlanExtensions::from_layers(vec![fx(1, 0), fx(4, 0)]);
    assert_eq!(
        sibling_nested.validate(&p),
        Ok(vec![DROP_SHADOW, ExtOperator::EffectsNested])
    );
    // An input that is not an earlier node is never followed (the plan itself is invalid; the
    // envelope validator must neither panic nor report nesting through it).
    let hostile = plan(vec![solid(), group(vec![layer(2)]), group(vec![layer(0)])]);
    let ext = PlanExtensions::from_layers(vec![fx(1, 0), fx(2, 0)]);
    assert_eq!(ext.validate(&hostile), Ok(vec![DROP_SHADOW]));
}

#[test]
fn effects_nested_is_refused_for_a_backend_that_declares_it_unsupported() {
    let p = nested();
    let ext = PlanExtensions::from_layers(vec![fx(1, 0), fx(2, 0)]);
    // Effects supported, nesting not declared: the render-cpu shape.
    let no_nesting = Claims {
        effects: Exactness::Approximate,
        nested: Exactness::Unsupported,
        rest: Exactness::Unsupported,
    };
    assert_eq!(
        admit(&p, &ext, &no_nesting).map(|_| ()),
        refused("effects_nested")
    );
    refused_before_any_sink_call(&p, &ext, &no_nesting, "effects_nested");
    // The same backend admits the effects without nesting.
    let flat_effects = PlanExtensions::from_layers(vec![fx(1, 0)]);
    let ledger = admit(&p, &flat_effects, &no_nesting).unwrap();
    assert!(ledger.operators.iter().all(|o| o.key != "effects_nested"));
}

#[test]
fn effects_nested_is_admitted_for_a_backend_that_declares_it_approximate() {
    let p = nested();
    let ext = PlanExtensions::from_layers(vec![fx(1, 0), fx(2, 0)]);
    // The render-gpu shape: nesting declared at Approximate.
    let nesting = Claims {
        effects: Exactness::Approximate,
        nested: Exactness::Approximate,
        rest: Exactness::Unsupported,
    };
    let ledger = admit(&p, &ext, &nesting).unwrap();
    assert_eq!(ledger.lowest, Exactness::Approximate);
    assert!(ledger.operators.contains(&OperatorExactness {
        key: "effects_nested",
        exactness: Exactness::Approximate
    }));
    // A backend cannot claim a better class than pigment's for the nesting (same rule as effects).
    let exact = Claims::all(Exactness::Exact);
    let ledger = admit(&p, &ext, &exact).unwrap();
    assert!(ledger.operators.contains(&OperatorExactness {
        key: "effects_nested",
        exactness: Exactness::Approximate
    }));
    // A request that demands better than Approximate refuses the plan (the first operator over
    // the floor is the effect itself, at class Approximate).
    let strict = ExecuteRequest {
        min_exactness: Exactness::Fitted,
        ..req()
    };
    assert!(matches!(
        admit_extended(&p, &ext, &nesting, &strict, &CancellationToken::default()),
        Err(BackendError::Unsupported {
            exactness: Exactness::Approximate,
            ..
        })
    ));
}
