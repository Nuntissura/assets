//! Lane SMB-PIGMENT-FOLLOWUPS: PLR-069 (an envelope entry on a group the root does not reach asks
//! nothing of a backend, like the nodes of `RenderPlan::validate`), PLR-072 (the depth budget is
//! compared for every plan shape), PLR-073 (a source tripwire for the readers of plan nodes and
//! envelope entries), PLR-074 (the docs and DESCRIPTOR state the rule exactly) and PLR-075 (the
//! clip-base search of `validate` is linear). Node indexes are derived by hand from the plans built
//! in this file. Only API that exists before the lane is used, so this file also compiles on the
//! old tree and the tests that guard the fix are red there.
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::adjust_math::{AdjustmentKind, AdjustmentOp, BrightnessContrast};
use hsk_studio_pigment::coverage_math::{BlendChannel, BlendRangeEntry, IDENTITY_RANGE};
use hsk_studio_pigment::effect_math::{
    DropShadowOp, EffectFeature, EffectKind, EffectOp, GlowFill, OuterGlowOp,
};
use hsk_studio_pigment::render_plan::*;
use std::time::{Duration, Instant};

const R: Rect = Rect {
    x: 0,
    y: 0,
    width: 2,
    height: 1,
};
const NORMAL: StudioBlendMode = StudioBlendMode::Normal;

fn layer(input: u32, mode: StudioBlendMode) -> Layer {
    Layer {
        input: PlanNodeId(input),
        mode,
        opacity: 1.0,
        fill_opacity: 1.0,
        mask: None,
    }
}
fn clipped(input: u32, mode: StudioBlendMode) -> Layer {
    Layer {
        mask: Some(MaskInput::clip_to_base()),
        ..layer(input, mode)
    }
}
fn masked(input: u32) -> Layer {
    Layer {
        mask: Some(MaskInput {
            key: SourceKey(9),
            placement: R,
            format: MaskFormat::CoverageU8,
            sha256: [5; 32],
            inverted: false,
            density: 1.0,
        }),
        ..layer(input, NORMAL)
    }
}
fn solid() -> PlanNode {
    PlanNode::Solid {
        rect: R,
        rgba: [0.2, 0.4, 0.6, 1.0],
    }
}
/// The canonical adjustment-layer input: a transparent solid.
fn placeholder() -> PlanNode {
    PlanNode::Solid {
        rect: R,
        rgba: [0.0; 4],
    }
}
fn group(isolated: bool, layers: Vec<Layer>) -> PlanNode {
    PlanNode::Group(Group { isolated, layers })
}
fn plan_of(nodes: Vec<PlanNode>, root: u32) -> RenderPlan {
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
fn request() -> ExecuteRequest {
    ExecuteRequest {
        expected_revision: 3,
        min_exactness: Exactness::Approximate,
        deadline: None,
        max_working_bytes: 1 << 20,
    }
}

/// `0` opaque solid | `1` transparent placeholder | `extra` (nodes `2..`) | the root: an isolated
/// group {Normal over 0}, plus {Normal over the last extra node} when `reach` is set. Without
/// `reach` nothing references the extra nodes.
fn plan_with(extra: Vec<PlanNode>, reach: bool) -> RenderPlan {
    let mut nodes = vec![solid(), placeholder()];
    let entry = u32::try_from(nodes.len() + extra.len() - 1).unwrap();
    nodes.extend(extra);
    let mut layers = vec![layer(0, NORMAL)];
    if reach {
        layers.push(layer(entry, NORMAL));
    }
    nodes.push(group(true, layers));
    let root = u32::try_from(nodes.len() - 1).unwrap();
    plan_of(nodes, root)
}

/// Exact for everything except the one v1 operator and the one envelope operator it refuses; it
/// supports the clip route.
struct Refuses {
    ext: Option<ExtOperator>,
    v1: Option<Operator>,
}
impl Refuses {
    const NOTHING: Self = Self {
        ext: None,
        v1: None,
    };
}
impl RenderBackend for Refuses {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            name: "refuses",
            version: "0",
            device: DeviceClass::Cpu,
            adapter: String::new(),
            generation: 0,
        }
    }
    fn exactness(&self, op: Operator) -> Exactness {
        if Some(op) == self.v1 {
            Exactness::Unsupported
        } else {
            Exactness::Exact
        }
    }
    fn supports_clip_to_base(&self) -> bool {
        true
    }
    fn extension_exactness(&self, op: ExtOperator) -> Exactness {
        if Some(op) == self.ext {
            Exactness::Unsupported
        } else {
            Exactness::Exact
        }
    }
    fn execute(
        &self,
        plan: &RenderPlan,
        request: &ExecuteRequest,
        _: &dyn PlanSources,
        cancel: &CancellationToken,
        _: &mut dyn TileSink,
    ) -> Result<RenderReceipt, BackendError> {
        admit(plan, self, request, cancel)?;
        Err(BackendError::Unavailable("refuses_execute"))
    }
}
fn refusal(key: &'static str) -> Result<(), BackendError> {
    Err(BackendError::Unsupported {
        operator: key,
        exactness: Exactness::Unsupported,
    })
}
fn admitted(
    plan: &RenderPlan,
    ext: &PlanExtensions,
    backend: &Refuses,
) -> Result<ExactnessLedger, BackendError> {
    admit_extended(
        plan,
        ext,
        backend,
        &request(),
        &CancellationToken::default(),
    )
}

fn shadow(mode: StudioBlendMode) -> EffectOp {
    EffectOp::DropShadow(DropShadowOp::new([0.0; 3], mode, 0.5))
}
fn blend_if() -> Vec<BlendRangeEntry> {
    vec![BlendRangeEntry {
        channel: BlendChannel::Gray,
        source: [0.2, 0.4, 0.6, 1.0],
        destination: IDENTITY_RANGE,
    }]
}
fn at(group: u32, layer: u32) -> LayerExtension {
    LayerExtension::new(PlanNodeId(group), layer)
}

/// One envelope feature on the last extra node `g` of `plan_with`.
struct Case {
    what: &'static str,
    extra: fn() -> Vec<PlanNode>,
    entries: fn(u32) -> Vec<LayerExtension>,
    refuse_ext: Option<ExtOperator>,
    refuse_v1: Option<Operator>,
    /// What `PlanExtensions::validate` reports when the group is reachable, in first-use order.
    want: fn() -> Vec<ExtOperator>,
    /// The key of the refusal (for an operator no backend refuses: its own key).
    key: fn() -> &'static str,
}
fn cases() -> Vec<Case> {
    vec![
        Case {
            what: "adjustment layer",
            // 2 isolated {Normal over 0, Normal over the placeholder (the adjustment)}
            extra: || vec![group(true, vec![layer(0, NORMAL), layer(1, NORMAL)])],
            entries: |g| vec![at(g, 1).with_adjustment(AdjustmentOp::Invert)],
            refuse_ext: Some(ExtOperator::AdjustmentLayer),
            refuse_v1: None,
            want: || {
                vec![
                    ExtOperator::AdjustmentLayer,
                    ExtOperator::Adjustment(AdjustmentKind::Invert),
                ]
            },
            key: || ExtOperator::AdjustmentLayer.key(),
        },
        Case {
            what: "adjustment kind",
            extra: || vec![group(true, vec![layer(0, NORMAL), layer(1, NORMAL)])],
            entries: |g| vec![at(g, 1).with_adjustment(AdjustmentOp::Invert)],
            refuse_ext: Some(ExtOperator::Adjustment(AdjustmentKind::Invert)),
            refuse_v1: None,
            want: || {
                vec![
                    ExtOperator::AdjustmentLayer,
                    ExtOperator::Adjustment(AdjustmentKind::Invert),
                ]
            },
            key: || AdjustmentKind::Invert.key(),
        },
        Case {
            what: "layer effect",
            extra: || vec![group(true, vec![layer(0, NORMAL)])],
            entries: |g| vec![at(g, 0).with_effects(vec![shadow(NORMAL)])],
            refuse_ext: Some(ExtOperator::Effect(EffectKind::DropShadow)),
            refuse_v1: None,
            want: || vec![ExtOperator::Effect(EffectKind::DropShadow)],
            key: || EffectKind::DropShadow.key(),
        },
        Case {
            what: "effect blend mode",
            extra: || vec![group(true, vec![layer(0, NORMAL)])],
            entries: |g| vec![at(g, 0).with_effects(vec![shadow(StudioBlendMode::Multiply)])],
            refuse_ext: None,
            refuse_v1: Some(Operator::Blend(StudioBlendMode::Multiply)),
            want: || vec![ExtOperator::Effect(EffectKind::DropShadow)],
            key: || Operator::Blend(StudioBlendMode::Multiply).key(),
        },
        Case {
            what: "effects nested",
            // 2 isolated {Normal over 0} (inner) | 3 isolated {Normal over 2} (outer), effects on both
            extra: || {
                vec![
                    group(true, vec![layer(0, NORMAL)]),
                    group(true, vec![layer(2, NORMAL)]),
                ]
            },
            entries: |g| {
                vec![
                    at(g - 1, 0).with_effects(vec![shadow(NORMAL)]),
                    at(g, 0).with_effects(vec![shadow(NORMAL)]),
                ]
            },
            refuse_ext: Some(ExtOperator::EffectsNested),
            refuse_v1: None,
            want: || {
                vec![
                    ExtOperator::Effect(EffectKind::DropShadow),
                    ExtOperator::EffectsNested,
                ]
            },
            key: || ExtOperator::EffectsNested.key(),
        },
        Case {
            what: "blend-if",
            extra: || vec![group(true, vec![layer(0, NORMAL)])],
            entries: |g| vec![at(g, 0).with_blend_if(blend_if())],
            refuse_ext: Some(ExtOperator::BlendIf),
            refuse_v1: None,
            want: || vec![ExtOperator::BlendIf],
            key: || ExtOperator::BlendIf.key(),
        },
        Case {
            what: "blend-if on a clip base (intrinsically unsupported)",
            extra: || vec![group(true, vec![layer(0, NORMAL), clipped(0, NORMAL)])],
            entries: |g| vec![at(g, 0).with_blend_if(blend_if())],
            refuse_ext: None,
            refuse_v1: None,
            want: || vec![ExtOperator::BlendIfClipBase],
            key: || ExtOperator::BlendIfClipBase.key(),
        },
        Case {
            what: "blend-if on a clipped layer (intrinsically unsupported)",
            extra: || vec![group(true, vec![layer(0, NORMAL), clipped(0, NORMAL)])],
            entries: |g| vec![at(g, 1).with_blend_if(blend_if())],
            refuse_ext: None,
            refuse_v1: None,
            want: || vec![ExtOperator::BlendIfClippedLayer],
            key: || ExtOperator::BlendIfClippedLayer.key(),
        },
        Case {
            what: "effects with blend-if (intrinsically unsupported)",
            extra: || vec![group(true, vec![layer(0, NORMAL)])],
            entries: |g| {
                vec![
                    at(g, 0)
                        .with_effects(vec![shadow(NORMAL)])
                        .with_blend_if(blend_if()),
                ]
            },
            refuse_ext: None,
            refuse_v1: None,
            want: || {
                vec![
                    ExtOperator::Effect(EffectKind::DropShadow),
                    ExtOperator::EffectsWithBlendIf,
                ]
            },
            key: || ExtOperator::EffectsWithBlendIf.key(),
        },
        Case {
            what: "mask feather",
            extra: || vec![group(true, vec![masked(0)])],
            entries: |g| vec![at(g, 0).with_mask_feather(2.0)],
            refuse_ext: Some(ExtOperator::MaskFeather),
            refuse_v1: None,
            want: || vec![ExtOperator::MaskFeather],
            key: || ExtOperator::MaskFeather.key(),
        },
    ]
}

/// PLR-069: an envelope operator (adjustment, Blend-If, feather, effect, nested effects, an effect's
/// blend mode) used only by entries on a group the root does not reach admits through `validate`
/// and `admit_extended` and is not in the ledger; the same entries on a reachable group are
/// refused with the same key and `validate` reports the operators in first-use order. The
/// v1-only parts of the plans admit either way, so the difference is the entry alone.
#[test]
fn unreachable_envelope_entry_asks_for_nothing_reachable_one_is_refused() {
    // Every case is judged before the test fails, so a failing run names all the cases that differ.
    let mut failures: Vec<String> = Vec::new();
    for case in cases() {
        let backend = Refuses {
            ext: case.refuse_ext,
            v1: case.refuse_v1,
        };
        let group_id = u32::try_from((case.extra)().len() + 1).unwrap();
        let ext = PlanExtensions::from_layers((case.entries)(group_id));
        let key = (case.key)();

        let unreachable = plan_with((case.extra)(), false);
        let reported = ext.validate(&unreachable);
        if reported != Ok(vec![]) {
            failures.push(format!(
                "{}: validate of the unreachable entry = {reported:?}",
                case.what
            ));
        }
        match admitted(&unreachable, &ext, &backend) {
            Ok(ledger) if ledger.operators.iter().any(|o| o.key == key) => failures.push(format!(
                "{}: the unreachable entry is in the ledger {:?}",
                case.what, ledger.operators
            )),
            Ok(_) => {}
            Err(e) => failures.push(format!("{}: unreachable entry refused: {e:?}", case.what)),
        }

        let reachable = plan_with((case.extra)(), true);
        let reported = ext.validate(&reachable);
        if reported != Ok((case.want)()) {
            failures.push(format!(
                "{}: validate of the reachable entry = {reported:?}",
                case.what
            ));
        }
        let refused = admitted(&reachable, &ext, &backend).map(|_| ());
        if refused != refusal(key) {
            failures.push(format!(
                "{}: reachable entry admission = {refused:?}",
                case.what
            ));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// PLR-069, nesting across the reachability line: the inner group is reached by the root, the
/// outer group (which holds it) is not. The outer entry is not executed, so it does not make the
/// plan "nested": only the inner entry's effect is reported, and a backend that cannot evaluate
/// nested effects still admits the plan.
#[test]
fn unreachable_outer_effect_entry_does_not_report_nesting() {
    // 0 solid | 1 placeholder | 2 inner isolated {Normal over 0} | 3 outer isolated {Normal over 2}
    // | 4 root {Normal over 0, Normal over 2}: node 3 is referenced by nobody.
    let plan = plan_of(
        vec![
            solid(),
            placeholder(),
            group(true, vec![layer(0, NORMAL)]),
            group(true, vec![layer(2, NORMAL)]),
            group(true, vec![layer(0, NORMAL), layer(2, NORMAL)]),
        ],
        4,
    );
    let ext = PlanExtensions::from_layers(vec![
        at(2, 0).with_effects(vec![shadow(NORMAL)]),
        at(3, 0).with_effects(vec![shadow(NORMAL)]),
    ]);
    assert_eq!(
        ext.validate(&plan),
        Ok(vec![ExtOperator::Effect(EffectKind::DropShadow)])
    );
    let backend = Refuses {
        ext: Some(ExtOperator::EffectsNested),
        v1: None,
    };
    assert!(admitted(&plan, &ext, &backend).is_ok());

    // The same outer entry on a root that does reach node 3 is nested.
    let reached = plan_of(
        vec![
            solid(),
            placeholder(),
            group(true, vec![layer(0, NORMAL)]),
            group(true, vec![layer(2, NORMAL)]),
            group(true, vec![layer(0, NORMAL), layer(3, NORMAL)]),
        ],
        4,
    );
    assert_eq!(
        ext.validate(&reached),
        Ok(vec![
            ExtOperator::Effect(EffectKind::DropShadow),
            ExtOperator::EffectsNested
        ])
    );
    assert_eq!(
        admitted(&reached, &ext, &backend).map(|_| ()),
        refusal(ExtOperator::EffectsNested.key())
    );
}

/// One envelope entry (or two) on the last extra node `g` and the typed error pigment gives it.
struct Row {
    what: &'static str,
    entries: fn(u32) -> Vec<LayerExtension>,
    error: PlanError,
    /// The refusal key of the same entry at admission when the error is a feature refusal (an
    /// `Unsupported`); `None` when it is an `InvalidPlan`.
    key: Option<&'static str>,
}

/// Runs every row on an unreachable and on a reachable group: the same typed error from
/// `validate`, and the matching refusal from `admit_extended` (any backend).
fn rows_are_refused_on_every_entry(rows: Vec<Row>) {
    for row in rows {
        let ext = PlanExtensions::from_layers((row.entries)(2));
        for reach in [false, true] {
            let plan = plan_with(
                vec![group(true, vec![layer(0, NORMAL), layer(1, NORMAL)])],
                reach,
            );
            assert_eq!(
                ext.validate(&plan),
                Err(row.error),
                "{} (reachable: {reach})",
                row.what
            );
            let expected = match row.key {
                Some(key) => refusal(key),
                None => Err(BackendError::InvalidPlan(row.error)),
            };
            assert_eq!(
                admitted(&plan, &ext, &Refuses::NOTHING).map(|_| ()),
                expected,
                "{} (reachable: {reach})",
                row.what
            );
        }
    }
}

/// PLR-069 guard (green before and after): a knockout, an adjustment kind and an effect feature
/// that pigment itself refuses are typed errors on every entry, reachable or not, and admission
/// reports the feature key either way. They are not a capability a backend declares.
#[test]
fn intrinsic_envelope_refusals_stay_on_every_entry() {
    rows_are_refused_on_every_entry(vec![
        Row {
            what: "knockout deep",
            entries: |g| vec![at(g, 0).with_knockout(Knockout::Deep)],
            error: PlanError::KnockoutUnsupported {
                node: 2,
                depth: Knockout::Deep,
            },
            key: Some("knockout_deep"),
        },
        Row {
            what: "adjustment kind without a formula",
            entries: |g| {
                vec![at(g, 1).with_adjustment(AdjustmentOp::BrightnessContrast(
                    BrightnessContrast {
                        brightness: 10.0,
                        contrast: 0.0,
                        use_legacy: false,
                    },
                ))]
            },
            error: PlanError::AdjustmentUnsupported {
                node: 2,
                kind: AdjustmentKind::BrightnessContrast,
            },
            key: Some(AdjustmentKind::BrightnessContrast.key()),
        },
        Row {
            what: "effect feature pigment refuses",
            entries: |g| {
                vec![at(g, 0).with_effects(vec![EffectOp::OuterGlow(
                    OuterGlowOp::new([0.0; 3], NORMAL, 0.5).with_fill(GlowFill::Gradient),
                )])]
            },
            error: PlanError::EffectUnsupported {
                node: 2,
                feature: EffectFeature::GlowGradient,
            },
            key: Some(EffectFeature::GlowGradient.key()),
        },
    ]);
}

/// PLR-069 guard (green before and after): the structure of an envelope entry is checked on every
/// entry; a malformed entry on an unreachable group is the same typed error as on a reachable one.
#[test]
fn envelope_structure_stays_checked_on_unreachable_entries() {
    rows_are_refused_on_every_entry(vec![
        Row {
            what: "entry on a solid node",
            entries: |_| vec![at(0, 0)],
            error: PlanError::ExtensionTarget { node: 0 },
            key: None,
        },
        Row {
            what: "layer index out of range",
            entries: |g| vec![at(g, 9)],
            error: PlanError::ExtensionTarget { node: 2 },
            key: None,
        },
        Row {
            what: "duplicate entry",
            entries: |g| vec![at(g, 0), at(g, 0)],
            error: PlanError::ExtensionDuplicate { node: 2 },
            key: None,
        },
        Row {
            what: "adjustment over a content layer",
            entries: |g| vec![at(g, 0).with_adjustment(AdjustmentOp::Invert)],
            error: PlanError::AdjustmentInput { node: 2 },
            key: None,
        },
        Row {
            what: "adjustment parameter",
            entries: |g| vec![at(g, 1).with_adjustment(AdjustmentOp::Posterize { levels: 1 })],
            error: PlanError::AdjustmentParameter { node: 2 },
            key: None,
        },
        Row {
            what: "feather without a coverage mask",
            entries: |g| vec![at(g, 0).with_mask_feather(4.0)],
            error: PlanError::FeatherWithoutMask { node: 2 },
            key: None,
        },
        Row {
            what: "feather not finite",
            entries: |g| vec![at(g, 0).with_mask_feather(f32::NAN)],
            error: PlanError::NonFinite { node: 2 },
            key: None,
        },
        Row {
            what: "blend-if range out of order",
            entries: |g| {
                vec![at(g, 0).with_blend_if(vec![BlendRangeEntry {
                    channel: BlendChannel::Gray,
                    source: [0.4, 0.2, 0.6, 1.0],
                    destination: IDENTITY_RANGE,
                }])]
            },
            error: PlanError::BlendIfRange { node: 2 },
            key: None,
        },
        Row {
            what: "effect size above the limit",
            entries: |g| {
                vec![at(g, 0).with_effects(vec![EffectOp::DropShadow(
                    DropShadowOp::new([0.0; 3], NORMAL, 0.5).with_size(1.0e9),
                )])]
            },
            error: PlanError::EffectLimit { node: 2 },
            key: None,
        },
    ]);
}

/// PLR-072: the depth budget is compared for every node, so `max_depth` below a plan's depth
/// refuses it whatever its shape, including a lone leaf and a group without layers (depth 1);
/// `max_depth = 0` refuses every plan. Same typed error on `validate` and on both admissions.
#[test]
fn depth_limit_applies_to_every_plan_shape() {
    let tile = PlanNode::Tile(TileInput {
        key: SourceKey(1),
        placement: R,
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        colour_sha256: [1; 32],
        alpha_sha256: [2; 32],
    });
    // (shape, plan, depth)
    let shapes: Vec<(&str, RenderPlan, usize)> = vec![
        ("lone solid", plan_of(vec![solid()], 0), 1),
        ("lone tile", plan_of(vec![tile], 0), 1),
        (
            "group without layers",
            plan_of(vec![group(true, vec![])], 0),
            1,
        ),
        (
            "group over a solid",
            plan_of(vec![solid(), group(true, vec![layer(0, NORMAL)])], 1),
            2,
        ),
        (
            "group over a layerless group",
            plan_of(
                vec![group(true, vec![]), group(true, vec![layer(0, NORMAL)])],
                1,
            ),
            2,
        ),
        (
            "three levels",
            plan_of(
                vec![
                    solid(),
                    group(true, vec![layer(0, NORMAL)]),
                    group(true, vec![layer(1, NORMAL)]),
                ],
                2,
            ),
            3,
        ),
    ];
    let cancel = CancellationToken::default();
    for (what, plan, depth) in shapes {
        // The defaults admit it (a leaf root needs the backend's Normal class, which is Exact).
        assert!(plan.validate(&PlanLimits::default()).is_ok(), "{what}");
        for max_depth in 0..=depth + 1 {
            let limits = PlanLimits {
                max_depth,
                ..PlanLimits::default()
            };
            let fits = max_depth >= depth;
            let v1 = admit_with_limits(&plan, &limits, &Refuses::NOTHING, &request(), &cancel);
            let ext = admit_extended_with_limits(
                &plan,
                &PlanExtensions::default(),
                &limits,
                &Refuses::NOTHING,
                &request(),
                &cancel,
            );
            if fits {
                assert!(
                    plan.validate(&limits).is_ok(),
                    "{what}, max_depth {max_depth}"
                );
                assert!(v1.is_ok(), "{what}, max_depth {max_depth}");
                assert!(ext.is_ok(), "{what}, max_depth {max_depth}");
            } else {
                assert_eq!(
                    plan.validate(&limits),
                    Err(PlanError::TooDeep),
                    "{what}, max_depth {max_depth}"
                );
                let refused = Err(BackendError::InvalidPlan(PlanError::TooDeep));
                assert_eq!(v1.map(|_| ()), refused, "{what}, max_depth {max_depth}");
                assert_eq!(ext.map(|_| ()), refused, "{what}, max_depth {max_depth}");
            }
        }
    }
    // The budgets keep their order: an empty plan and the node budget are decided first.
    let zero = PlanLimits {
        max_nodes: 0,
        max_layers: 0,
        max_depth: 0,
    };
    assert_eq!(plan_of(vec![], 0).validate(&zero), Err(PlanError::Empty));
    assert_eq!(
        plan_of(vec![solid()], 0).validate(&zero),
        Err(PlanError::BudgetExceeded)
    );
    // A structural defect of the first node is reported before the depth of that node.
    let empty_rect = PlanNode::Solid {
        rect: Rect {
            x: 0,
            y: 0,
            width: 0,
            height: 1,
        },
        rgba: [0.0, 0.0, 0.0, 1.0],
    };
    let depth0 = PlanLimits {
        max_depth: 0,
        ..PlanLimits::default()
    };
    assert_eq!(
        plan_of(vec![empty_rect], 0).validate(&depth0),
        Err(PlanError::EmptyRect { node: 0 })
    );
}

/// An isolated group over node 0 whose layer carries a coverage mask edited by `edit`.
fn masked_group(edit: fn(&mut MaskInput)) -> PlanNode {
    let mut m = MaskInput {
        key: SourceKey(9),
        placement: R,
        format: MaskFormat::CoverageU8,
        sha256: [5; 32],
        inverted: false,
        density: 1.0,
    };
    edit(&mut m);
    group(
        true,
        vec![Layer {
            mask: Some(m),
            ..layer(0, NORMAL)
        }],
    )
}

/// PLAN-R12-2 guard (green before and after): the structure branches of `validate` that no earlier
/// test put on an unreachable node stay checked there too, with the same typed error on `validate`,
/// `admit` and `admit_extended`: a tile placement, a solid alpha, a mask placement and density,
/// non-canonical clip fields, a clipped pass-through layer and a pass-through clip base. The extra
/// nodes start at node 2 (see `plan_with`) and nothing references them.
#[test]
fn structure_branches_stay_checked_on_unreachable_nodes() {
    type Extra = fn() -> Vec<PlanNode>;
    struct Branch {
        what: &'static str,
        extra: Extra,
        error: PlanError,
    }
    let cases: Vec<Branch> = vec![
        Branch {
            what: "tile placement with no area",
            extra: || {
                vec![PlanNode::Tile(TileInput {
                    key: SourceKey(1),
                    placement: Rect {
                        x: 0,
                        y: 0,
                        width: 0,
                        height: 1,
                    },
                    format: SampleFormat::RgbF32Le,
                    alpha: AlphaConvention::Straight,
                    colour_sha256: [1; 32],
                    alpha_sha256: [2; 32],
                })]
            },
            error: PlanError::EmptyRect { node: 2 },
        },
        Branch {
            what: "solid alpha above one",
            extra: || {
                vec![PlanNode::Solid {
                    rect: R,
                    rgba: [0.0, 0.0, 0.0, 2.0],
                }]
            },
            error: PlanError::OutOfRange { node: 2 },
        },
        Branch {
            what: "mask density not finite",
            extra: || vec![masked_group(|m| m.density = f32::NAN)],
            error: PlanError::NonFinite { node: 2 },
        },
        Branch {
            what: "mask density above one",
            extra: || vec![masked_group(|m| m.density = 2.0)],
            error: PlanError::OutOfRange { node: 2 },
        },
        Branch {
            what: "mask placement with no area",
            extra: || {
                vec![masked_group(|m| {
                    m.placement = Rect {
                        x: 0,
                        y: 0,
                        width: 0,
                        height: 1,
                    }
                })]
            },
            error: PlanError::EmptyRect { node: 2 },
        },
        Branch {
            what: "clip fields not canonical",
            extra: || {
                vec![group(
                    true,
                    vec![
                        layer(0, NORMAL),
                        Layer {
                            mask: Some(MaskInput {
                                inverted: true,
                                ..MaskInput::clip_to_base()
                            }),
                            ..layer(0, NORMAL)
                        },
                    ],
                )]
            },
            error: PlanError::ClipFields { node: 2 },
        },
        Branch {
            what: "clipped pass-through layer",
            // 2 non-isolated {Normal over 0} | 3 isolated {Normal over 0 (base), PassThrough over 2 clipped}
            extra: || {
                vec![
                    group(false, vec![layer(0, NORMAL)]),
                    group(
                        true,
                        vec![layer(0, NORMAL), clipped(2, StudioBlendMode::PassThrough)],
                    ),
                ]
            },
            error: PlanError::ClipPassThrough { node: 3 },
        },
        Branch {
            what: "pass-through clip base",
            // 2 non-isolated {Normal over 0} | 3 isolated {PassThrough over 2 (base), Normal over 0 clipped}
            extra: || {
                vec![
                    group(false, vec![layer(0, NORMAL)]),
                    group(
                        true,
                        vec![layer(2, StudioBlendMode::PassThrough), clipped(0, NORMAL)],
                    ),
                ]
            },
            error: PlanError::ClipBasePassThrough { node: 3 },
        },
    ];
    let cancel = CancellationToken::default();
    for case in cases {
        let plan = plan_with((case.extra)(), false);
        let expected = Err(BackendError::InvalidPlan(case.error));
        assert_eq!(
            plan.validate(&PlanLimits::default()),
            Err(case.error),
            "{}",
            case.what
        );
        assert_eq!(
            admit(&plan, &Refuses::NOTHING, &request(), &cancel).map(|_| ()),
            expected,
            "{}",
            case.what
        );
        assert_eq!(
            admitted(&plan, &PlanExtensions::default(), &Refuses::NOTHING).map(|_| ()),
            expected,
            "{}",
            case.what
        );
    }

    // The budgets count unreachable groups: three layers in a group nobody references, and a chain
    // of two unreachable groups deeper than the reachable part.
    // 2 isolated {3 layers over 0} | 3 isolated {Normal over 2}, root {Normal over 0}.
    let plan = plan_with(
        vec![
            group(
                true,
                vec![layer(0, NORMAL), layer(0, NORMAL), layer(0, NORMAL)],
            ),
            group(true, vec![layer(2, NORMAL)]),
        ],
        false,
    );
    let (layers, depth) = (5usize, 3usize);
    assert!(plan.validate(&PlanLimits::default()).is_ok());
    for (what, limits, error) in [
        (
            "layers",
            PlanLimits {
                max_layers: layers - 1,
                ..PlanLimits::default()
            },
            PlanError::BudgetExceeded,
        ),
        (
            "depth",
            PlanLimits {
                max_depth: depth - 1,
                ..PlanLimits::default()
            },
            PlanError::TooDeep,
        ),
    ] {
        assert_eq!(plan.validate(&limits), Err(error), "{what}");
        assert_eq!(
            admit_with_limits(&plan, &limits, &Refuses::NOTHING, &request(), &cancel).map(|_| ()),
            Err(BackendError::InvalidPlan(error)),
            "{what}"
        );
    }
    let exact = PlanLimits {
        max_nodes: 5,
        max_layers: layers,
        max_depth: depth,
    };
    assert!(plan.validate(&exact).is_ok());
}

/// PLR-075: a group of `max_layers` layers whose first layer is unclipped and every other layer is
/// clipped to it validates in a linear walk. The backwards base search made each clipped layer scan
/// back to layer 0 (about 2.1e9 steps at the default maximum); the bound below is 100 times what the
/// linear walk takes in a debug build, and the quadratic search exceeds it by a wide margin.
#[test]
fn clip_run_validation_is_linear() {
    let count = PlanLimits::MAXIMUM.max_layers;
    let mut layers = vec![layer(0, NORMAL)];
    layers.extend((1..count).map(|_| clipped(0, NORMAL)));
    let plan = plan_of(vec![solid(), group(true, layers)], 1);
    let started = Instant::now();
    let ops = plan.validate(&PlanLimits::default()).unwrap();
    let elapsed = started.elapsed();
    assert!(ops.contains(&Operator::Mask(MaskFormat::BaseAlpha)));
    assert!(
        elapsed < Duration::from_secs(5),
        "validate of {count} clipped layers took {elapsed:?}"
    );
}

/// PLR-075 guard (green before and after): the clip-run rules give the same typed error as the
/// reference rule (the base is the nearest earlier unclipped layer, found by a plain backwards
/// scan here) for every sequence of up to six layers drawn from: unclipped Normal (U), clipped
/// Normal (C), unclipped pass-through (P) and clipped pass-through (Q).
#[test]
fn clip_run_errors_match_the_reference_rule() {
    #[derive(Clone, Copy, PartialEq)]
    enum Kind {
        U,
        C,
        P,
        Q,
    }
    const KINDS: [Kind; 4] = [Kind::U, Kind::C, Kind::P, Kind::Q];
    let is_clipped = |k: Kind| matches!(k, Kind::C | Kind::Q);
    let is_pass_through = |k: Kind| matches!(k, Kind::P | Kind::Q);
    let build = |k: Kind| match k {
        Kind::U => layer(0, NORMAL),
        Kind::C => clipped(0, NORMAL),
        Kind::P => layer(1, StudioBlendMode::PassThrough),
        Kind::Q => clipped(1, StudioBlendMode::PassThrough),
    };
    // The reference: the first clipped layer with a problem, in layer order.
    let reference = |seq: &[Kind]| -> Result<(), PlanError> {
        for (li, k) in seq.iter().enumerate() {
            if !is_clipped(*k) {
                continue;
            }
            let base = (0..li).rev().find(|j| !is_clipped(seq[*j]));
            let Some(base) = base else {
                return Err(PlanError::ClipWithoutBase { node: 2 });
            };
            if is_pass_through(*k) {
                return Err(PlanError::ClipPassThrough { node: 2 });
            }
            if is_pass_through(seq[base]) {
                return Err(PlanError::ClipBasePassThrough { node: 2 });
            }
        }
        Ok(())
    };
    let mut checked = 0usize;
    let mut seq: Vec<Kind> = Vec::new();
    // Counting in base 4 over every length from 1 to 6.
    for length in 1..=6u32 {
        for code in 0..4usize.pow(length) {
            seq.clear();
            let mut rest = code;
            for _ in 0..length {
                seq.push(KINDS[rest % 4]);
                rest /= 4;
            }
            // 0 solid | 1 non-isolated group over 0 | 2 root: the sequence.
            let plan = plan_of(
                vec![
                    solid(),
                    group(false, vec![layer(0, NORMAL)]),
                    group(true, seq.iter().map(|k| build(*k)).collect()),
                ],
                2,
            );
            let got = plan.validate(&PlanLimits::default()).map(|_| ());
            assert_eq!(got, reference(&seq), "sequence #{code} of length {length}");
            checked += 1;
        }
    }
    assert_eq!(checked, 4 + 16 + 64 + 256 + 1024 + 4096);
}

/// PLR-074: the docs and the DESCRIPTOR of `render_plan` say what the code does. The `validate`
/// and `plan_limits` topics state the envelope rule and the depth rule, and every test they cite
/// exists in `tests/` under that name.
#[test]
fn descriptor_topics_state_the_rule_and_cite_existing_tests() {
    let descriptor: serde_json::Value =
        serde_json::from_str(DESCRIPTOR).expect("DESCRIPTOR is JSON");
    let topic = |name: &str| -> String {
        descriptor[name]
            .as_str()
            .unwrap_or_else(|| panic!("DESCRIPTOR topic {name} is missing"))
            .to_string()
    };
    let validate = topic("validate");
    let plan_limits = topic("plan_limits");
    for needle in [
        "PlanExtensions::validate",
        "unreachable group",
        "reachable from the root only",
        "no backend could run it",
    ] {
        assert!(validate.contains(needle), "validate topic lacks {needle:?}");
    }
    assert!(
        plan_limits.contains("max_depth 0 every plan"),
        "plan_limits topic lacks the depth rule"
    );

    let cited = [
        (
            &validate,
            [
                "unreachable_unsupported_operator_admits_reachable_one_is_refused",
                "structure_stays_checked_on_unreachable_nodes",
                "unreachable_envelope_entry_asks_for_nothing_reachable_one_is_refused",
                "intrinsic_envelope_refusals_stay_on_every_entry",
            ]
            .as_slice(),
        ),
        (
            &plan_limits,
            [
                "tightened_limits_refuse_what_the_defaults_admit",
                "loosening_past_the_maxima_is_a_typed_error_everywhere",
                "defaults_are_the_maxima_and_unchanged",
                "depth_limit_applies_to_every_plan_shape",
            ]
            .as_slice(),
        ),
    ];
    let mut sources = String::new();
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests");
    for entry in std::fs::read_dir(dir).expect("tests directory") {
        let path = entry.expect("tests entry").path();
        if path.extension().is_some_and(|e| e == "rs") {
            sources.push_str(&std::fs::read_to_string(&path).expect("test source"));
        }
    }
    for (text, names) in cited {
        for name in names {
            assert!(text.contains(name), "topic does not cite {name}");
            assert!(
                sources.contains(&format!("fn {name}(")),
                "cited test {name} does not exist"
            );
        }
    }
}

/// How a reader of plan nodes or envelope entries is classified.
#[derive(Clone, Copy)]
enum Reads {
    /// Structure or a lookup: decided over every node or entry by design.
    Every,
    /// Capability: must go through the reachable set; the strings must all occur in the code.
    Reachable(&'static [&'static str]),
}
/// One scanned source file: its name, its text and the classification of its readers.
type Scanned = (&'static str, &'static str, &'static [(&'static str, Reads)]);

/// PLR-073: every function of the plan IR that reads `plan.nodes`, `self.nodes` or the envelope
/// `self.layers` is classified here. A new reader fails this test until it is classified, and a
/// capability reader must go through the reachable set that both backends execute. (The V4-06
/// regexes of the lane notes find the same sites by text.)
#[test]
fn admission_readers_of_nodes_and_entries_are_classified() {
    let files: [Scanned; 2] = [
        (
            "render_plan.rs",
            include_str!("../src/render_plan.rs"),
            &[
                ("validate", Reads::Reachable(&["reachable_nodes()"])),
                ("reachable_nodes", Reads::Every),
                // `Group::clip_role`: the layers of one group, by index.
                ("clip_role", Reads::Every),
                ("has_leaf_root", Reads::Every),
                (
                    "has_pass_through_fill",
                    Reads::Reachable(&["reachable_nodes()"]),
                ),
                (
                    "has_pass_through_mask",
                    Reads::Reachable(&["reachable_nodes()"]),
                ),
                (
                    "uses_premultiplied_alpha",
                    Reads::Reachable(&["reachable_nodes()"]),
                ),
                (
                    "has_solid_outside_unit",
                    Reads::Reachable(&["reachable_nodes()"]),
                ),
                ("has_mask_density", Reads::Reachable(&["reachable_nodes()"])),
            ],
        ),
        (
            "render_plan/extensions.rs",
            include_str!("../src/render_plan/extensions.rs"),
            &[
                ("is_empty", Reads::Every),
                ("extension", Reads::Every),
                ("effect_modes", Reads::Every),
                ("effect_modes_in", Reads::Reachable(&["reachable_nodes()"])),
                (
                    "nodes_holding_effects",
                    Reads::Reachable(&["reachable.get("]),
                ),
                (
                    "validate",
                    Reads::Reachable(&["reachable_nodes()", "if counts &&"]),
                ),
            ],
        ),
    ];
    let mut seen = 0usize;
    for (file, source, table) in files {
        let code = source.split("#[cfg(test)]").next().unwrap();
        let mut chunks: Vec<(String, String)> = Vec::new();
        for line in code.lines() {
            let trimmed = line.trim_start();
            let declared = ["pub(crate) fn ", "pub(super) fn ", "pub fn ", "fn "]
                .iter()
                .find_map(|p| trimmed.strip_prefix(p));
            if let Some(rest) = declared {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                chunks.push((name, String::new()));
            }
            if trimmed.starts_with("//") {
                continue;
            }
            if let Some((_, body)) = chunks.last_mut() {
                body.push_str(line);
                body.push('\n');
            }
        }
        for (name, body) in &chunks {
            let reads = ["self.nodes", "plan.nodes", "self.layers"]
                .iter()
                .any(|p| body.contains(p));
            if !reads {
                continue;
            }
            seen += 1;
            let Some((_, class)) = table.iter().find(|(n, _)| n == name) else {
                panic!(
                    "{file}: fn {name} reads plan nodes or envelope entries and is not classified"
                );
            };
            if let Reads::Reachable(needles) = class {
                for needle in *needles {
                    assert!(
                        body.contains(needle),
                        "{file}: fn {name} is a capability reader and lacks {needle:?}"
                    );
                }
            }
        }
        // The admission entry points do not call the all-entries helper.
        for (name, body) in &chunks {
            if name.starts_with("admit") {
                assert!(
                    !body.contains(".effect_modes("),
                    "{file}: fn {name} asks for the effect modes of every entry"
                );
            }
        }
    }
    assert!(seen >= 12, "the scan found only {seen} readers");
}
