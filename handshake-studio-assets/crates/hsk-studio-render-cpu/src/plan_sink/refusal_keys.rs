//! Lane SMB-REFUSAL-KEYS (plausible register PLR-006): the refusals this backend re-decides as
//! defence in depth carry pigment's keys, so they cannot drift from shared admission.
//!
//! Claim, per rule below: the error `prepare` returns on its own (admission bypassed) is equal to
//! the error pigment's `admit` returns for the same plan, and that error is built from pigment's
//! exported constants and operator enums, never a literal spelled here. The envelope mappers
//! (`coverage_error`, `effect_error`, `window_error`, `blend_error`) are checked against the same
//! pigment names, and the features pigment recognises and refuses in effects (gradient glow, glow
//! range, jitter, Precise technique) are checked against `admit_extended`. Backend-owned keys
//! (`plan_shape`, `effects_nested`, `effect_work_limit`, `effect_limit`, ...) are out of scope:
//! pigment's admission does not report those rules under these names.
use super::*;
use hsk_studio_pigment::Mask;
use hsk_studio_pigment::coverage_math::CoverageError;
use hsk_studio_pigment::effect_math::window::WindowError;
use hsk_studio_pigment::effect_math::{
    EffectError, EffectFeature, EffectOp, GlowFill, GlowTechnique, OuterGlowOp,
};
use hsk_studio_pigment::render_plan::{
    BlendSpace, ColourTag, Group, LayerExtension, NoVector, OutputTarget, PlanNodeId,
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
        let prepared = ExtPrep::build(&envelope, 1 << 20)
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
