//! Lane SMB-PIGMENT-PLANLIMITS, item PLR-058 (lead decision: structural well-formedness stays on
//! every node, capability collection reads the nodes reachable from the root only, like every
//! admission predicate and both backends). `RenderPlan::validate` returns the operators a backend
//! must support; a node no layer reaches is skipped by render-cpu and render-gpu before they look
//! at it, so an operator used only there asks for nothing, while a malformed unreachable node is
//! still a malformed plan. Node indexes are derived by hand from the plans built in this file.
//! Only API that exists before the lane is used, so this file also compiles on the old tree.
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::*;

const R: Rect = Rect {
    x: 0,
    y: 0,
    width: 2,
    height: 1,
};

fn layer(input: u32, mode: StudioBlendMode) -> Layer {
    Layer {
        input: PlanNodeId(input),
        mode,
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
fn tile() -> PlanNode {
    PlanNode::Tile(TileInput {
        key: SourceKey(1),
        placement: R,
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        colour_sha256: [1; 32],
        alpha_sha256: [2; 32],
    })
}
fn group(isolated: bool, layers: Vec<Layer>) -> PlanNode {
    PlanNode::Group(Group { isolated, layers })
}
fn coverage(format: MaskFormat) -> MaskInput {
    MaskInput {
        key: SourceKey(9),
        placement: R,
        format,
        sha256: [5; 32],
        inverted: false,
        density: 1.0,
    }
}

/// `0 solid | 1 solid | 2 isolated {Normal over 0}`, then `extra` (nodes 3..), then the root: an
/// isolated group {Normal over 2}, plus {Normal over the last extra node} when `reach` is set.
/// Without `reach` nothing references the extra nodes.
fn plan_with(extra: Vec<PlanNode>, reach: bool) -> RenderPlan {
    let mut nodes = vec![
        solid(),
        solid(),
        group(true, vec![layer(0, StudioBlendMode::Normal)]),
    ];
    let entry = u32::try_from(nodes.len() + extra.len() - 1).unwrap();
    nodes.extend(extra);
    let mut layers = vec![layer(2, StudioBlendMode::Normal)];
    if reach {
        layers.push(layer(entry, StudioBlendMode::Normal));
    }
    nodes.push(group(true, layers));
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
fn request() -> ExecuteRequest {
    ExecuteRequest {
        expected_revision: 3,
        min_exactness: Exactness::Approximate,
        deadline: None,
        max_working_bytes: 1 << 20,
    }
}

/// Exact for everything except the one operator it refuses, and the clip route when `clip` is off.
struct Refuses {
    operator: Option<Operator>,
    clip: bool,
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
        if Some(op) == self.operator {
            Exactness::Unsupported
        } else {
            Exactness::Exact
        }
    }
    fn supports_clip_to_base(&self) -> bool {
        self.clip
    }
    fn extension_exactness(&self, _: ExtOperator) -> Exactness {
        Exactness::Exact
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
/// v1 and envelope admission of `plan` on `backend`, as a pair of key-only results.
fn both(plan: &RenderPlan, backend: &Refuses) -> [Result<(), BackendError>; 2] {
    let cancel = CancellationToken::default();
    [
        admit(plan, backend, &request(), &cancel).map(|_| ()),
        admit_extended(
            plan,
            &PlanExtensions::default(),
            backend,
            &request(),
            &cancel,
        )
        .map(|_| ()),
    ]
}

/// One case: the extra nodes whose last node is the entry the root reaches when `reach` is set;
/// `op` is what the backend refuses (or `None` for the clip route, which the backend refuses by not
/// supporting it); `key` is the refusal key a reachable use gets.
struct Case {
    what: &'static str,
    extra: fn() -> Vec<PlanNode>,
    op: Option<Operator>,
    contains: Operator,
    key: &'static str,
}
fn cases() -> [Case; 5] {
    [
        Case {
            what: "blend mode in an unreachable layer",
            // 3 isolated {Multiply over 1}
            extra: || vec![group(true, vec![layer(1, StudioBlendMode::Multiply)])],
            op: Some(Operator::Blend(StudioBlendMode::Multiply)),
            contains: Operator::Blend(StudioBlendMode::Multiply),
            key: "multiply",
        },
        Case {
            what: "tile node nobody references",
            // 3 tile | 4 isolated {Normal over 3}
            extra: || vec![tile(), group(true, vec![layer(3, StudioBlendMode::Normal)])],
            op: Some(Operator::Tile),
            contains: Operator::Tile,
            key: "tile",
        },
        Case {
            what: "coverage mask format in an unreachable layer",
            // 3 isolated {Normal over 1, coverage_u8 mask}
            extra: || {
                vec![group(
                    true,
                    vec![Layer {
                        mask: Some(coverage(MaskFormat::CoverageU8)),
                        ..layer(1, StudioBlendMode::Normal)
                    }],
                )]
            },
            op: Some(Operator::Mask(MaskFormat::CoverageU8)),
            contains: Operator::Mask(MaskFormat::CoverageU8),
            key: "coverage_u8",
        },
        Case {
            what: "clip route in an unreachable group",
            // 3 isolated {Normal over 1 (base), Normal over 1 clipped to it}
            extra: || {
                vec![group(
                    true,
                    vec![
                        layer(1, StudioBlendMode::Normal),
                        Layer {
                            mask: Some(MaskInput::clip_to_base()),
                            ..layer(1, StudioBlendMode::Normal)
                        },
                    ],
                )]
            },
            op: None,
            contains: Operator::Mask(MaskFormat::BaseAlpha),
            key: "clip_to_base",
        },
        Case {
            what: "pass-through reference in an unreachable group",
            // 3 non-isolated {Normal over 1} | 4 isolated {PassThrough over 3}
            extra: || {
                vec![
                    group(false, vec![layer(1, StudioBlendMode::Normal)]),
                    group(true, vec![layer(3, StudioBlendMode::PassThrough)]),
                ]
            },
            op: Some(Operator::PassThroughGroup),
            contains: Operator::PassThroughGroup,
            key: "group_pass_through",
        },
    ]
}

/// PLR-058: an operator a backend refuses, used only by nodes the root does not reach, admits
/// (`validate`, `admit`, `admit_extended`); the same nodes reached from the root are refused with
/// the key the all-nodes collection gave, and `validate` then reports the operator.
#[test]
fn unreachable_unsupported_operator_admits_reachable_one_is_refused() {
    let limits = PlanLimits::default();
    for case in cases() {
        let backend = Refuses {
            operator: case.op,
            clip: case.op.is_some(),
        };
        let unreachable = plan_with((case.extra)(), false);
        let ops = unreachable.validate(&limits).expect(case.what);
        assert!(!ops.contains(&case.contains), "{}: {ops:?}", case.what);
        // The reached part of the plan still reports what it uses, in first-use (node) order:
        // node 0 the Solid, node 2 its Normal layer, the root an isolated group (node 2).
        assert_eq!(
            ops,
            vec![
                Operator::Solid,
                Operator::Blend(StudioBlendMode::Normal),
                Operator::IsolatedGroup,
            ],
            "{}",
            case.what
        );
        assert_eq!(
            both(&unreachable, &backend),
            [Ok(()), Ok(())],
            "{}",
            case.what
        );

        let reachable = plan_with((case.extra)(), true);
        let ops = reachable.validate(&limits).expect(case.what);
        assert!(ops.contains(&case.contains), "{}: {ops:?}", case.what);
        assert_eq!(
            both(&reachable, &backend),
            [refusal(case.key), refusal(case.key)],
            "{}",
            case.what
        );
        // Same key as the operator's own: nothing was renamed on the way.
        assert_eq!(case.contains.key(), case.key, "{}", case.what);
    }
}

/// An unreachable unsupported use does not hide a reachable one: the reachable refusal is
/// reported, whichever node comes first.
#[test]
fn reachable_refusal_is_reported_when_an_unreachable_node_comes_first() {
    // 3 isolated {Multiply over 1} (unreachable) | 4 isolated {Screen over 1} | root reaches 4.
    let nodes = || {
        vec![
            group(true, vec![layer(1, StudioBlendMode::Multiply)]),
            group(true, vec![layer(1, StudioBlendMode::Screen)]),
        ]
    };
    let plan = plan_with(nodes(), true);
    let ops = plan.validate(&PlanLimits::default()).unwrap();
    assert!(ops.contains(&Operator::Blend(StudioBlendMode::Screen)));
    assert!(!ops.contains(&Operator::Blend(StudioBlendMode::Multiply)));
    for refused in [StudioBlendMode::Multiply, StudioBlendMode::Screen] {
        let backend = Refuses {
            operator: Some(Operator::Blend(refused)),
            clip: true,
        };
        let expected = if refused == StudioBlendMode::Screen {
            [refusal("screen"), refusal("screen")]
        } else {
            [Ok(()), Ok(())]
        };
        assert_eq!(both(&plan, &backend), expected, "{refused:?}");
    }
}

/// Structure is checked over every node: a malformed node is refused whether or not a layer
/// reaches it, with the same typed error `validate` gave before (hostile indexes never reach the
/// capability collection, a cycle is a forward reference and cannot hang it).
#[test]
fn structure_stays_checked_on_unreachable_nodes() {
    let nan = f32::NAN;
    let empty = Rect {
        x: 0,
        y: 0,
        width: 0,
        height: 1,
    };
    // Each case: extra nodes starting at node 3 (the root is the node after them) and the error.
    let cases: Vec<(&str, Vec<PlanNode>, PlanError)> = vec![
        (
            "forward reference to the root",
            vec![group(true, vec![layer(4, StudioBlendMode::Normal)])],
            PlanError::ForwardReference { node: 3, input: 4 },
        ),
        (
            "self reference (a cycle)",
            vec![group(true, vec![layer(3, StudioBlendMode::Normal)])],
            PlanError::ForwardReference { node: 3, input: 3 },
        ),
        (
            "input far outside the plan",
            vec![group(true, vec![layer(u32::MAX, StudioBlendMode::Normal)])],
            PlanError::ForwardReference {
                node: 3,
                input: u32::MAX,
            },
        ),
        (
            "non-finite opacity",
            vec![group(
                true,
                vec![Layer {
                    opacity: nan,
                    ..layer(0, StudioBlendMode::Normal)
                }],
            )],
            PlanError::NonFinite { node: 3 },
        ),
        (
            "fill above one",
            vec![group(
                true,
                vec![Layer {
                    fill_opacity: 1.5,
                    ..layer(0, StudioBlendMode::Normal)
                }],
            )],
            PlanError::OutOfRange { node: 3 },
        ),
        (
            "tool-only mode",
            vec![group(true, vec![layer(0, StudioBlendMode::Behind)])],
            PlanError::ModeNotApplicable {
                node: 3,
                mode: StudioBlendMode::Behind,
            },
        ),
        (
            "pass-through over an isolated group",
            vec![group(true, vec![layer(2, StudioBlendMode::PassThrough)])],
            PlanError::PassThroughOnIsolated { node: 3 },
        ),
        (
            "empty rect",
            vec![PlanNode::Solid {
                rect: empty,
                rgba: [0.0, 0.0, 0.0, 1.0],
            }],
            PlanError::EmptyRect { node: 3 },
        ),
        (
            "non-finite colour",
            vec![PlanNode::Solid {
                rect: R,
                rgba: [nan, 0.0, 0.0, 1.0],
            }],
            PlanError::NonFinite { node: 3 },
        ),
        (
            "clipped layer without a base",
            vec![group(
                true,
                vec![Layer {
                    mask: Some(MaskInput::clip_to_base()),
                    ..layer(0, StudioBlendMode::Normal)
                }],
            )],
            PlanError::ClipWithoutBase { node: 3 },
        ),
    ];
    let backend = Refuses {
        operator: None,
        clip: true,
    };
    for (what, extra, error) in cases {
        let plan = plan_with(extra, false);
        assert_eq!(plan.validate(&PlanLimits::default()), Err(error), "{what}");
        assert_eq!(
            both(&plan, &backend),
            [
                Err(BackendError::InvalidPlan(error)),
                Err(BackendError::InvalidPlan(error))
            ],
            "{what}"
        );
    }
    // The predicates do not hang or panic on the cyclic, unvalidated plan either.
    let cyclic = plan_with(
        vec![group(true, vec![layer(3, StudioBlendMode::Normal)])],
        true,
    );
    assert!(!cyclic.has_pass_through_mask() && !cyclic.has_mask_density());
}
