//! Lane SMB-REFUSAL-KEYS (plausible register PLR-006): the refusals this backend re-decides as
//! defence in depth carry pigment's keys, so they cannot drift from shared admission.
//!
//! Claim, per rule below: the error `prepare` returns on its own (admission bypassed) is equal to
//! the error pigment's `admit` returns for the same plan, and that error is built from pigment's
//! exported constants and operator enums, never a literal spelled here. The envelope mappers
//! (`coverage_error`, `effect_error`, `window_error`, `blend_error`) are checked against the same
//! pigment names, and the features pigment recognises and refuses in effects (gradient glow, glow
//! range, jitter, Precise technique) are checked against `admit_extended`. Lane SMB-FX-OPS-ADOPT
//! adds the rules pigment took over in SMB-PIGMENT-FX-OPS and SMB-PIGMENT-ADMIT-PARITY
//! (`mask_on_pass_through`, `alpha_premultiplied`, `effects_with_blend_if`, `effects_nested`).
//! Backend-owned keys (`plan_shape`, `effect_work_limit`, `effect_limit`, ...) are out of scope:
//! pigment's admission does not report those rules under these names.
use super::*;
use hsk_studio_pigment::Mask;
use hsk_studio_pigment::coverage_math::{
    BlendChannel, BlendRangeEntry, CoverageError, IDENTITY_RANGE,
};
use hsk_studio_pigment::effect_math::window::WindowError;
use hsk_studio_pigment::effect_math::{
    ColourOverlayOp, EffectError, EffectFeature, EffectOp, GlowFill, GlowTechnique, OuterGlowOp,
};
use hsk_studio_pigment::render_plan::{
    BlendSpace, ColourTag, Group, LayerExtension, NoVector, OutputTarget, PlanNodeId, admit,
    admit_extended,
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

fn mk_plan(nodes: Vec<PlanNode>, root: u32) -> RenderPlan {
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
fn pass_through_plan(mode: StudioBlendMode, opacity: f32, fill_opacity: f32) -> RenderPlan {
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
fn one_layer_plan(solid: PlanNode, layer: Layer) -> RenderPlan {
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
fn admission_and_prepare(plan: &RenderPlan) -> (BackendError, RenderError) {
    let renderer = Renderer::new(4).unwrap();
    let admitted = admit::<NoVector>(plan, &renderer, &request(), &CancellationToken::default())
        .expect_err("shared admission refuses this plan");
    let prepared = prepare(plan, &NoSources, ExtPrep::default())
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
    let cases: Vec<(&str, RenderPlan, BackendError)> = vec![
        (
            "pass-through layer below full opacity",
            pass_through_plan(StudioBlendMode::PassThrough, 0.5, 1.0),
            refusal(Operator::PassThroughOpacity.key()),
        ),
        (
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
            RenderError::Backend(admitted),
            "prepare equals admission, {rule}"
        );
    }

    // Features pigment recognises and refuses in effects: envelope preparation on its own reports
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
    for (feature, op) in features {
        let envelope = PlanExtensions::from_layers(vec![
            LayerExtension::new(PlanNodeId(1), 0).with_effects(vec![EffectOp::OuterGlow(op)]),
        ]);
        let renderer = Renderer::new(4).unwrap();
        let admitted = admit_extended::<NoVector, _>(
            &glow_plan,
            &envelope,
            &renderer,
            &request(),
            &CancellationToken::default(),
        )
        .expect_err("shared admission refuses the feature");
        assert_eq!(
            admitted,
            refusal(feature.key()),
            "pigment admission, {feature:?}"
        );
        let prepared = ExtPrep::build(&envelope, &reachable(&glow_plan), 1 << 20)
            .err()
            .expect("envelope preparation refuses the feature on its own");
        assert_eq!(
            prepared,
            RenderError::Backend(admitted),
            "preparation equals admission, {feature:?}"
        );
    }

    // Envelope and blend mappers: each key is the pigment name of the condition it reports.
    assert_eq!(
        coverage_error(CoverageError::OutOfDomain),
        unsupported(SAMPLE_RANGE_OPERATOR)
    );
    assert_eq!(
        coverage_error(CoverageError::FeatherOutOfRange),
        unsupported(ExtOperator::MaskFeather.key())
    );
    assert_eq!(
        coverage_error(CoverageError::RangeOrder),
        unsupported(ExtOperator::BlendIf.key())
    );
    assert_eq!(
        extended::effect_error(EffectError::PlaneShape),
        unsupported(EffectError::PlaneShape.code())
    );
    assert_eq!(
        extended::effect_error(EffectError::OverBudget),
        unsupported(EffectError::OverBudget.code())
    );
    for shape in [WindowError::WindowShape, WindowError::Geometry] {
        assert_eq!(
            extended::window_error(shape),
            unsupported(EffectError::PlaneShape.code())
        );
    }
    assert_eq!(
        blend_error(BlendError::OutOfDomain),
        unsupported(SAMPLE_RANGE_OPERATOR)
    );
    for mode in [
        StudioBlendMode::Average,
        StudioBlendMode::Behind,
        StudioBlendMode::PassThrough,
    ] {
        for error in [
            BlendError::Unsupported(mode),
            BlendError::ToolOnly(mode),
            BlendError::GroupOnly(mode),
        ] {
            assert_eq!(
                blend_error(error),
                unsupported(Operator::Blend(mode).key()),
                "{error:?}"
            );
        }
    }
    assert_eq!(
        blend_error(BlendError::TooDeep),
        unsupported(BlendError::TooDeep.code())
    );
}

/// SMB-PARITY-FOLLOWUP (R8-review-a): the mask formats this backend has a reader for are named in
/// `exactness`, so a format pigment adds later falls to Unsupported at shared admission instead of
/// being admitted as Exact and refused afterwards by the local mask-format check. Today's three
/// formats cannot tell the named arms from a wildcard; the fall-through itself is proven by the
/// pigment tripwire (compile time) and by the lane's red proof with a fourth variant.
#[test]
fn mask_formats_are_declared_by_name() {
    let renderer = Renderer::new(4).unwrap();
    let declared = |format| RenderBackend::<NoVector>::exactness(&renderer, Operator::Mask(format));
    // The coverage planes with a reader: Exact, pigment's own class for the operator.
    for format in [MaskFormat::CoverageU8, MaskFormat::CoverageU16Le] {
        assert_eq!(declared(format), Exactness::Exact, "{}", format.key());
        assert_eq!(Operator::Mask(format).intrinsic(), Exactness::Exact);
        // ... and a plan with such a mask is admitted by shared admission.
        let masked = Layer {
            mask: Some(MaskInput {
                key: SourceKey(1),
                placement: SIDE,
                format,
                sha256: [0; 32],
                inverted: false,
                density: 1.0,
            }),
            ..mk_layer(0, StudioBlendMode::Normal, 1.0, 1.0)
        };
        let plan = one_layer_plan(white(), masked);
        assert!(
            admit::<NoVector>(&plan, &renderer, &request(), &CancellationToken::default()).is_ok(),
            "{}",
            format.key()
        );
    }
    // The clip route keeps pigment's class (Approximate; the Photoshop rule is UNVERIFIED).
    assert_eq!(declared(MaskFormat::BaseAlpha), Exactness::Approximate);
    assert_eq!(
        declared(MaskFormat::BaseAlpha),
        Operator::Mask(MaskFormat::BaseAlpha).intrinsic()
    );
}

fn overlay_fx(group: u32, layer: u32) -> LayerExtension {
    LayerExtension::new(PlanNodeId(group), layer).with_effects(vec![EffectOp::ColourOverlay(
        ColourOverlayOp::new([1.0, 0.0, 0.0], StudioBlendMode::Normal, 0.5),
    )])
}

/// SMB-FX-OPS-ADOPT: the declarations for the operators pigment added, and the two effects rules
/// pigment now decides at shared admission: the error `prepare` returns on its own equals the one
/// `admit_extended` returns, and both carry pigment's keys.
#[test]
fn fx_ops_declarations_and_refusals_equal_pigments() {
    let renderer = Renderer::new(4).unwrap();
    let declared = |op| RenderBackend::<NoVector>::extension_exactness(&renderer, op);
    // Declared Unsupported on purpose: nesting (one effect frame per chunk) and premultiplied
    // alpha (straight only), so shared admission refuses them as pigment's keys.
    for op in [
        ExtOperator::EffectsNested,
        ExtOperator::AlphaPremultiplied,
        ExtOperator::EffectsWithBlendIf,
    ] {
        assert_eq!(declared(op), Exactness::Unsupported, "{}", op.key());
    }
    // Counterfactual: the effect kinds themselves are supported at pigment's class.
    assert_eq!(
        declared(ExtOperator::Effect(EffectKind::ColourOverlay)),
        ExtOperator::Effect(EffectKind::ColourOverlay).intrinsic()
    );

    // [solid, group{0}, root group{1}]: effects on (1, 0) and on the layer over the inner group.
    let nested_plan = mk_plan(
        vec![
            white(),
            mk_group(true, vec![mk_layer(0, StudioBlendMode::Normal, 1.0, 1.0)]),
            mk_group(true, vec![mk_layer(1, StudioBlendMode::Normal, 1.0, 1.0)]),
        ],
        2,
    );
    let nested = PlanExtensions::from_layers(vec![overlay_fx(1, 0), overlay_fx(2, 0)]);
    let flat_plan = one_layer_plan(white(), mk_layer(0, StudioBlendMode::Normal, 1.0, 1.0));
    let both = PlanExtensions::from_layers(vec![overlay_fx(1, 0).with_blend_if(vec![
        BlendRangeEntry {
            channel: BlendChannel::Gray,
            source: [0.2, 0.4, 0.6, 0.8],
            destination: IDENTITY_RANGE,
        },
    ])]);
    let cases = [
        (
            "effects nested in the content of a layer with effects",
            nested_plan,
            nested,
            ExtOperator::EffectsNested.key(),
        ),
        (
            "effects together with Blend-If on one layer",
            flat_plan,
            both,
            ExtOperator::EffectsWithBlendIf.key(),
        ),
    ];
    for (rule, plan, envelope, key) in cases {
        let admitted = admit_extended::<NoVector, _>(
            &plan,
            &envelope,
            &renderer,
            &request(),
            &CancellationToken::default(),
        )
        .expect_err("shared admission refuses the rule");
        assert_eq!(admitted, refusal(key), "pigment admission, {rule}");
        let ext =
            ExtPrep::build(&envelope, &reachable(&plan), 1 << 20).expect("envelope preparation");
        let prepared = prepare(&plan, &NoSources, ext)
            .err()
            .expect("prepare refuses the rule on its own");
        assert_eq!(
            prepared,
            RenderError::Backend(admitted),
            "prepare equals admission, {rule}"
        );
    }
}
