//! Lane SMB-REFUSAL-KEYS (plausible register PLR-006): the refusals this backend re-decides as
//! defence in depth carry pigment's keys, so they cannot drift from shared admission.
//!
//! Claim, per rule below: the error `prepare` returns on its own (admission bypassed) is equal to
//! the error pigment's `admit` returns for the same plan on this backend, and that error is built
//! from pigment's exported constants and operator enums, never a literal spelled here. The test
//! needs no adapter: `prepare` is host-side and the backend is constructed with the explicit
//! no-device switch, so it proves its claim on every machine (no `NOT_PROVEN`). Backend-owned keys
//! (`plan_shape`, `effect_cost`, ...) are out of scope: pigment's admission does not define those
//! rules. Lane SMB-FX-OPS-ADOPT adds the rules pigment took over in SMB-PIGMENT-FX-OPS and
//! SMB-PIGMENT-ADMIT-PARITY (`mask_on_pass_through`, `alpha_premultiplied`,
//! `effects_with_blend_if`) and the declarations of `effects_nested` and `alpha_premultiplied`.
use super::*;
use crate::device::BackendChoice;
use hsk_studio_pigment::Mask;
use hsk_studio_pigment::coverage_math::{BlendChannel, BlendRangeEntry, IDENTITY_RANGE};
use hsk_studio_pigment::effect_math::{
    ColourOverlayOp, EffectFeature, EffectOp, GlowFill, GlowTechnique, OuterGlowOp,
};
use hsk_studio_pigment::render_plan::{
    BlendSpace, Layer, LayerExtension, MaskInput, OperatorExactness, OutputTarget,
};

struct NoSources;
impl PlanSources for NoSources {
    fn tile(&self, _key: SourceKey) -> Option<TilePlanes<'_>> {
        None
    }
    fn mask(&self, _key: SourceKey) -> Option<Mask<'_>> {
        None
    }
}

const SIDE: Rect = Rect {
    x: 0,
    y: 0,
    width: 4,
    height: 4,
};

fn mk_plan(nodes: Vec<PlanNode>, root: u32) -> RenderPlan<NoVector> {
    RenderPlan {
        revision: 7,
        target: OutputTarget {
            extent: SIDE,
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space: ColourTag {
                profile_sha256: [1; 32],
                blend_space: BlendSpace::LinearLight,
            },
        },
        nodes,
        root: PlanNodeId(root),
    }
}
fn mk_layer(input: u32, mode: StudioBlendMode, opacity: f32, fill_opacity: f32) -> Layer {
    Layer {
        input: PlanNodeId(input),
        mode,
        opacity,
        fill_opacity,
        mask: None,
    }
}
fn mk_group(isolated: bool, layers: Vec<Layer>) -> PlanNode {
    PlanNode::Group(Group { isolated, layers })
}
fn white() -> PlanNode {
    PlanNode::Solid {
        rect: SIDE,
        rgba: [1.0; 4],
    }
}
/// `[solid, non-isolated group over it, isolated root with one layer over the group]`.
fn pass_through_plan(
    mode: StudioBlendMode,
    opacity: f32,
    fill_opacity: f32,
) -> RenderPlan<NoVector> {
    mk_plan(
        vec![
            white(),
            mk_group(false, vec![mk_layer(0, StudioBlendMode::Normal, 1.0, 1.0)]),
            mk_group(true, vec![mk_layer(1, mode, opacity, fill_opacity)]),
        ],
        2,
    )
}
/// `[solid, isolated root with one layer]` where the layer is what the caller varies.
fn one_layer_plan(solid: PlanNode, layer: Layer) -> RenderPlan<NoVector> {
    mk_plan(vec![solid, mk_group(true, vec![layer])], 1)
}

fn request() -> ExecuteRequest {
    ExecuteRequest {
        expected_revision: 7,
        min_exactness: Exactness::Approximate,
        deadline: None,
        max_working_bytes: 1 << 20,
    }
}

/// What shared admission says about `plan` on this backend, and what `prepare` says on its own.
fn admission_and_prepare(plan: &RenderPlan<NoVector>) -> (BackendError, GpuError) {
    let backend = GpuBackend::new(DeviceOptions {
        backend: BackendChoice::None,
        ..DeviceOptions::default()
    });
    let admitted = admit(plan, &backend, &request(), &CancellationToken::default())
        .expect_err("shared admission refuses this plan");
    let prepared = prepare(plan, &NoSources, None, 1 << 20)
        .err()
        .expect("prepare refuses this plan on its own");
    (admitted, prepared)
}
fn refusal(operator: &'static str) -> BackendError {
    BackendError::Unsupported {
        operator,
        exactness: Exactness::Unsupported,
    }
}

#[test]
fn defence_in_depth_refusal_keys_are_pigments() {
    let density_mask = MaskInput {
        key: SourceKey(1),
        placement: SIDE,
        format: MaskFormat::CoverageU8,
        sha256: [0; 32],
        inverted: false,
        density: 0.5,
    };
    let full_mask = MaskInput {
        density: 1.0,
        ..density_mask
    };
    let clipped = Layer {
        mask: Some(MaskInput::clip_to_base()),
        ..mk_layer(0, StudioBlendMode::Normal, 1.0, 1.0)
    };
    // (rule, plan, the refusal pigment names for it).
    let cases: Vec<(&str, RenderPlan<NoVector>, BackendError)> = vec![
        (
            "pass-through layer below full opacity",
            pass_through_plan(StudioBlendMode::PassThrough, 0.5, 1.0),
            refusal(Operator::PassThroughOpacity.key()),
        ),
        (
            // The backend used to report this one under the opacity key.
            "pass-through layer below full fill",
            pass_through_plan(StudioBlendMode::PassThrough, 1.0, 0.5),
            refusal(PASS_THROUGH_FILL_OPERATOR),
        ),
        (
            "non-isolated group composited with a blend mode",
            pass_through_plan(StudioBlendMode::Multiply, 1.0, 1.0),
            refusal(Operator::NonIsolatedBlendedGroup.key()),
        ),
        (
            "blend mode without a compositing function",
            one_layer_plan(white(), mk_layer(0, StudioBlendMode::Average, 1.0, 1.0)),
            refusal(Operator::Blend(StudioBlendMode::Average).key()),
        ),
        (
            "coverage mask with density below 1 on the v1 path",
            one_layer_plan(
                white(),
                Layer {
                    mask: Some(density_mask),
                    ..mk_layer(0, StudioBlendMode::Normal, 1.0, 1.0)
                },
            ),
            refusal(MASK_DENSITY_OPERATOR),
        ),
        (
            "solid colour outside [0, 1]",
            one_layer_plan(
                PlanNode::Solid {
                    rect: SIDE,
                    rgba: [2.0, 0.0, 0.0, 1.0],
                },
                mk_layer(0, StudioBlendMode::Normal, 1.0, 1.0),
            ),
            refusal(SAMPLE_RANGE_OPERATOR),
        ),
        (
            "clipping run whose base is a pass-through group",
            mk_plan(
                vec![
                    white(),
                    mk_group(false, vec![mk_layer(0, StudioBlendMode::Normal, 1.0, 1.0)]),
                    mk_group(
                        true,
                        vec![mk_layer(1, StudioBlendMode::PassThrough, 1.0, 1.0), clipped],
                    ),
                ],
                2,
            ),
            // Pigment's validation owns this rule: its own error value, node = the group node.
            BackendError::InvalidPlan(PlanError::ClipBasePassThrough { node: 2 }),
        ),
        (
            // SMB-PIGMENT-ADMIT-PARITY: pigment defines no mask on a pass-through layer.
            "coverage mask on a pass-through layer",
            mk_plan(
                vec![
                    white(),
                    mk_group(false, vec![mk_layer(0, StudioBlendMode::Normal, 1.0, 1.0)]),
                    mk_group(
                        true,
                        vec![Layer {
                            mask: Some(full_mask),
                            ..mk_layer(1, StudioBlendMode::PassThrough, 1.0, 1.0)
                        }],
                    ),
                ],
                2,
            ),
            refusal(PASS_THROUGH_MASK_OPERATOR),
        ),
        (
            // SMB-PIGMENT-ADMIT-PARITY: this backend declares the capability Unsupported.
            "premultiplied alpha at the target",
            {
                let mut plan =
                    one_layer_plan(white(), mk_layer(0, StudioBlendMode::Normal, 1.0, 1.0));
                plan.target.alpha = AlphaConvention::Premultiplied;
                plan
            },
            refusal(ExtOperator::AlphaPremultiplied.key()),
        ),
    ];
    for (rule, plan, pigment_says) in cases {
        let (admitted, prepared) = admission_and_prepare(&plan);
        assert_eq!(admitted, pigment_says, "pigment admission, {rule}");
        assert_eq!(
            prepared,
            GpuError::Backend(admitted),
            "prepare equals admission, {rule}"
        );
    }

    // Features pigment recognises and refuses in effects: compiling the effect on its own reports
    // the key `admit_extended` reports for the same envelope (`EffectFeature::key`).
    let glow = || OuterGlowOp::new([0.0; 3], StudioBlendMode::Normal, 1.0);
    let features = [
        (
            EffectFeature::GlowGradient,
            glow().with_fill(GlowFill::Gradient),
        ),
        (
            EffectFeature::GlowTechniquePrecise,
            glow().with_technique(GlowTechnique::Precise),
        ),
        (EffectFeature::GlowRange, glow().with_range(0.5)),
        (EffectFeature::GlowJitter, glow().with_jitter(0.5)),
    ];
    let glow_plan = one_layer_plan(white(), mk_layer(0, StudioBlendMode::Normal, 1.0, 1.0));
    let backend = GpuBackend::new(DeviceOptions {
        backend: BackendChoice::None,
        ..DeviceOptions::default()
    });
    for (feature, op) in features {
        let envelope = PlanExtensions::from_layers(vec![
            LayerExtension::new(PlanNodeId(1), 0).with_effects(vec![EffectOp::OuterGlow(op)]),
        ]);
        let admitted = admit_extended(
            &glow_plan,
            &envelope,
            &backend,
            &request(),
            &CancellationToken::default(),
        )
        .expect_err("shared admission refuses the feature");
        assert_eq!(
            admitted,
            refusal(feature.key()),
            "pigment admission, {feature:?}"
        );
        let prepared = prepare(&glow_plan, &NoSources, Some(&envelope), 1 << 20)
            .err()
            .expect("effect compilation refuses the feature on its own");
        assert_eq!(
            prepared,
            GpuError::Backend(admitted),
            "preparation equals admission, {feature:?}"
        );
    }
}

fn overlay_fx(group: u32, layer: u32) -> LayerExtension {
    LayerExtension::new(PlanNodeId(group), layer).with_effects(vec![EffectOp::ColourOverlay(
        ColourOverlayOp::new([1.0, 0.0, 0.0], StudioBlendMode::Normal, 0.5),
    )])
}
fn headless_backend() -> GpuBackend {
    GpuBackend::new(DeviceOptions {
        backend: BackendChoice::None,
        ..DeviceOptions::default()
    })
}

/// SMB-FX-OPS-ADOPT: the declarations for the three operators pigment added, and the exactness
/// ledger of a nested-effects plan equal what pigment's `validate` reports for the same envelope.
#[test]
fn fx_ops_declarations_and_nested_ledger_equal_pigments() {
    let backend = headless_backend();
    // Nesting is evaluated here (test fx_stack_mask_group_and_nesting_parity) at pigment's class.
    assert_eq!(
        backend.extension_exactness(ExtOperator::EffectsNested),
        Exactness::Approximate
    );
    assert_eq!(
        backend.extension_exactness(ExtOperator::EffectsNested),
        ExtOperator::EffectsNested
            .intrinsic()
            .lowest_of(Exactness::Fitted)
    );
    // Straight alpha only: undeclared would also be Unsupported, the declaration is explicit.
    assert_eq!(
        backend.extension_exactness(ExtOperator::AlphaPremultiplied),
        Exactness::Unsupported
    );
    assert_eq!(
        backend.extension_exactness(ExtOperator::EffectsWithBlendIf),
        Exactness::Unsupported
    );

    // [solid, group{0}, root group{1}] with effects on (1, 0) and on the layer that references
    // the inner group (2, 0): nested.
    let plan = mk_plan(
        vec![
            white(),
            mk_group(true, vec![mk_layer(0, StudioBlendMode::Normal, 1.0, 1.0)]),
            mk_group(true, vec![mk_layer(1, StudioBlendMode::Normal, 1.0, 1.0)]),
        ],
        2,
    );
    let nested = PlanExtensions::from_layers(vec![overlay_fx(1, 0), overlay_fx(2, 0)]);
    let reported = nested
        .validate(&plan)
        .expect("pigment validates the envelope");
    assert!(reported.contains(&ExtOperator::EffectsNested));
    let ledger = admit_extended(
        &plan,
        &nested,
        &backend,
        &request(),
        &CancellationToken::default(),
    )
    .expect("this backend declares nesting, so shared admission admits it");
    for op in reported {
        let class = op.intrinsic().lowest_of(backend.extension_exactness(op));
        assert!(
            ledger.operators.contains(&OperatorExactness {
                key: op.key(),
                exactness: class
            }),
            "ledger records {} at {class:?}",
            op.key()
        );
    }
    // Counterfactual: effects on the inner layer only are not nested.
    let flat = PlanExtensions::from_layers(vec![overlay_fx(1, 0)]);
    let ledger = admit_extended(
        &plan,
        &flat,
        &backend,
        &request(),
        &CancellationToken::default(),
    )
    .expect("flat effects are admitted");
    assert!(ledger.operators.iter().all(|o| o.key != "effects_nested"));
}

/// SMB-FX-OPS-ADOPT: effects together with Blend-If on one layer are refused by shared admission
/// as pigment's `EffectsWithBlendIf`, and the local check in `prepare` carries the same key.
#[test]
fn effects_with_blend_if_refusal_is_pigments_in_admission_and_prepare() {
    let backend = headless_backend();
    let plan = one_layer_plan(white(), mk_layer(0, StudioBlendMode::Normal, 1.0, 1.0));
    let both = PlanExtensions::from_layers(vec![overlay_fx(1, 0).with_blend_if(vec![
        BlendRangeEntry {
            channel: BlendChannel::Gray,
            source: [0.2, 0.4, 0.6, 0.8],
            destination: IDENTITY_RANGE,
        },
    ])]);
    let admitted = admit_extended(
        &plan,
        &both,
        &backend,
        &request(),
        &CancellationToken::default(),
    )
    .expect_err("shared admission refuses the combination");
    assert_eq!(admitted, refusal(ExtOperator::EffectsWithBlendIf.key()));
    let prepared = prepare(&plan, &NoSources, Some(&both), 1 << 20)
        .err()
        .expect("prepare refuses the combination on its own");
    assert_eq!(prepared, GpuError::Backend(admitted));
}
