//! Lane SMB-PIGMENT-PLANLIMITS, item PLR-005 (lead decision WAIVE_WITH_STUB, implemented here):
//! `admit` validated with `PlanLimits::default()` and a host could not tighten the plan budgets.
//! `admit_with_limits` and `admit_extended_with_limits` take the caller's limits; limits can only
//! tighten, a field above `PlanLimits::MAXIMUM` is the typed error `PlanError::LimitAboveMaximum`.
//! Node, layer and depth counts are derived by hand from the plans built in this file.
use hsk_studio_accord::CancellationToken;
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
/// The last node is the root.
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
/// 3 nodes, 2 layers, depth 3: `0 solid | 1 group {Normal over 0} | 2 root {Normal over 1}`.
/// Depth: the solid is 1, node 1 is 2, the root is 3.
fn small() -> RenderPlan {
    plan(vec![solid(), group(vec![layer(0)]), group(vec![layer(1)])])
}
/// `count` nodes: `count - 1` solids and a root group over the first one (the solids after it are
/// unreachable, which structure still counts).
fn nodes_plan(count: usize) -> RenderPlan {
    let mut nodes = vec![solid(); count - 1];
    nodes.push(group(vec![layer(0)]));
    plan(nodes)
}
/// 2 nodes and `count` layers in the root group, all over the solid.
fn layers_plan(count: usize) -> RenderPlan {
    plan(vec![solid(), group(vec![layer(0); count])])
}
/// A chain: the solid (depth 1) and `groups` nested groups, the root at depth `groups + 1`.
fn chain_plan(groups: usize) -> RenderPlan {
    let mut nodes = vec![solid()];
    for k in 1..=groups {
        nodes.push(group(vec![layer(u32::try_from(k - 1).unwrap())]));
    }
    plan(nodes)
}

/// Exact for everything; `execute` admits with the host's limits and then delivers one chunk, the
/// way a backend adopting the new entry point would.
struct Limited {
    limits: PlanLimits,
}
impl RenderBackend for Limited {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            name: "limited",
            version: "0",
            device: DeviceClass::Cpu,
            adapter: String::new(),
            generation: 0,
        }
    }
    fn exactness(&self, _: Operator) -> Exactness {
        Exactness::Exact
    }
    fn supports_clip_to_base(&self) -> bool {
        true
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
        sink: &mut dyn TileSink,
    ) -> Result<RenderReceipt, BackendError> {
        admit_with_limits(plan, &self.limits, self, request, cancel)?;
        sink.accept(R, &[0; 24], &[0; 8])
            .map_err(BackendError::Sink)?;
        Err(BackendError::Unavailable("limited_execute"))
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
fn request() -> ExecuteRequest {
    ExecuteRequest {
        expected_revision: 3,
        min_exactness: Exactness::Approximate,
        deadline: None,
        max_working_bytes: 1 << 20,
    }
}
fn backend() -> Limited {
    Limited {
        limits: PlanLimits::default(),
    }
}
/// v1 admission with the caller's limits, then the envelope path: key-and-ledger results.
fn with_limits(
    plan: &RenderPlan,
    limits: &PlanLimits,
) -> [Result<ExactnessLedger, BackendError>; 2] {
    let cancel = CancellationToken::default();
    [
        admit_with_limits(plan, limits, &backend(), &request(), &cancel),
        admit_extended_with_limits(
            plan,
            &PlanExtensions::default(),
            limits,
            &backend(),
            &request(),
            &cancel,
        ),
    ]
}
fn invalid(error: PlanError) -> Result<ExactnessLedger, BackendError> {
    Err(BackendError::InvalidPlan(error))
}
fn defaults(plan: &RenderPlan) -> [Result<ExactnessLedger, BackendError>; 2] {
    let cancel = CancellationToken::default();
    [
        admit(plan, &backend(), &request(), &cancel),
        admit_extended(
            plan,
            &PlanExtensions::default(),
            &backend(),
            &request(),
            &cancel,
        ),
    ]
}

/// For each limit kind: a limit one below what the plan needs refuses a plan the defaults admit,
/// with the typed error `validate` reports, on every entry point and before any sink call; a limit
/// exactly at what the plan needs admits it with the ledger the defaults give.
#[test]
fn tightened_limits_refuse_what_the_defaults_admit() {
    let p = small();
    let d = PlanLimits::default();
    let ledger = defaults(&p);
    assert!(ledger[0].is_ok() && ledger[1].is_ok());
    // (name, one below, exactly enough, the refusal)
    let kinds = [
        (
            "max_nodes",
            PlanLimits { max_nodes: 2, ..d },
            PlanLimits { max_nodes: 3, ..d },
            PlanError::BudgetExceeded,
        ),
        (
            "max_layers",
            PlanLimits { max_layers: 1, ..d },
            PlanLimits { max_layers: 2, ..d },
            PlanError::BudgetExceeded,
        ),
        (
            "max_depth",
            PlanLimits { max_depth: 2, ..d },
            PlanLimits { max_depth: 3, ..d },
            PlanError::TooDeep,
        ),
        // Zero is the tightest limit: it refuses every plan that has a node, a layer or a group.
        (
            "max_nodes zero",
            PlanLimits { max_nodes: 0, ..d },
            PlanLimits { max_nodes: 3, ..d },
            PlanError::BudgetExceeded,
        ),
        (
            "max_layers zero",
            PlanLimits { max_layers: 0, ..d },
            PlanLimits { max_layers: 2, ..d },
            PlanError::BudgetExceeded,
        ),
        (
            "max_depth zero",
            PlanLimits { max_depth: 0, ..d },
            PlanLimits { max_depth: 3, ..d },
            PlanError::TooDeep,
        ),
    ];
    for (name, tight, enough, error) in kinds {
        assert_eq!(p.validate(&tight), Err(error), "{name}");
        assert_eq!(
            with_limits(&p, &tight),
            [invalid(error), invalid(error)],
            "{name}"
        );
        assert_eq!(with_limits(&p, &enough), ledger, "{name}");
        assert!(p.validate(&enough).is_ok(), "{name}");

        // A host backend that admits with its limits delivers nothing when it refuses.
        for (limits, chunks, refusal) in [
            (tight, 0, Err(BackendError::InvalidPlan(error))),
            (enough, 1, Err(BackendError::Unavailable("limited_execute"))),
        ] {
            let mut sink = CountingSink(0);
            let run = Limited { limits }
                .execute(
                    &p,
                    &request(),
                    &NoSources,
                    &CancellationToken::default(),
                    &mut sink,
                )
                .map(|_| ());
            assert_eq!((run, sink.0), (refusal, chunks), "{name}");
        }
    }
    // Each kind is its own: tightening one field leaves the others at the maximum.
    let nodes_only = PlanLimits { max_nodes: 3, ..d };
    assert_eq!(
        with_limits(&layers_plan(5), &nodes_only),
        defaults(&layers_plan(5))
    );
    assert_eq!(
        with_limits(&chain_plan(10), &nodes_only),
        [
            invalid(PlanError::BudgetExceeded),
            invalid(PlanError::BudgetExceeded)
        ]
    );
}

/// A limit above the maximum is a typed error on every entry point, for each field, at the first
/// value above the maximum and at `usize::MAX`; it is checked before the plan is read, so a
/// plan the maxima refuse is never admitted by loosening.
#[test]
fn loosening_past_the_maxima_is_a_typed_error_everywhere() {
    let max = PlanLimits::MAXIMUM;
    assert_eq!(max.within_maximum(), Ok(()));
    let huge = u64::try_from(usize::MAX).unwrap();
    // (kind, limits, requested, maximum)
    let loosened = [
        (
            PlanLimitKind::Nodes,
            PlanLimits {
                max_nodes: 65_537,
                ..max
            },
            65_537,
            65_536,
        ),
        (
            PlanLimitKind::Layers,
            PlanLimits {
                max_layers: 65_537,
                ..max
            },
            65_537,
            65_536,
        ),
        (
            PlanLimitKind::Depth,
            PlanLimits {
                max_depth: 65,
                ..max
            },
            65,
            64,
        ),
        (
            PlanLimitKind::Nodes,
            PlanLimits {
                max_nodes: usize::MAX,
                ..max
            },
            huge,
            65_536,
        ),
        (
            PlanLimitKind::Layers,
            PlanLimits {
                max_layers: usize::MAX,
                ..max
            },
            huge,
            65_536,
        ),
        (
            PlanLimitKind::Depth,
            PlanLimits {
                max_depth: usize::MAX,
                ..max
            },
            huge,
            64,
        ),
    ];
    for (kind, limits, requested, maximum) in loosened {
        let error = PlanError::LimitAboveMaximum {
            kind,
            requested,
            maximum,
        };
        assert_eq!(limits.within_maximum(), Err(error), "{kind:?}");
        // validate, admit and admit_extended: a small valid plan, an over-budget one and an empty
        // one all answer with the limit error, not with the plan's own verdict.
        for p in [
            small(),
            nodes_plan(65_537),
            RenderPlan {
                nodes: Vec::new(),
                ..small()
            },
        ] {
            assert_eq!(p.validate(&limits), Err(error), "{kind:?}");
            assert_eq!(
                with_limits(&p, &limits),
                [invalid(error), invalid(error)],
                "{kind:?}"
            );
        }
        assert_eq!(error.code(), "plan_limit_above_maximum");
        assert_eq!(
            BackendError::InvalidPlan(error).code(),
            "plan_limit_above_maximum"
        );
    }
    assert_eq!(PlanLimitKind::Nodes.key(), "max_nodes");
    assert_eq!(PlanLimitKind::Layers.key(), "max_layers");
    assert_eq!(PlanLimitKind::Depth.key(), "max_depth");

    // Several fields loosened: the first in the order nodes, layers, depth is named.
    let all = PlanLimits {
        max_nodes: 70_000,
        max_layers: 70_000,
        max_depth: 70,
    };
    assert_eq!(
        all.within_maximum(),
        Err(PlanError::LimitAboveMaximum {
            kind: PlanLimitKind::Nodes,
            requested: 70_000,
            maximum: 65_536
        })
    );
    let not_nodes = PlanLimits {
        max_nodes: 5,
        ..all
    };
    assert_eq!(
        not_nodes.within_maximum(),
        Err(PlanError::LimitAboveMaximum {
            kind: PlanLimitKind::Layers,
            requested: 70_000,
            maximum: 65_536
        })
    );

    // Loosening admits nothing the maxima refuse: 65 537 nodes are over budget with the maxima.
    let big = nodes_plan(65_537);
    assert_eq!(big.validate(&max), Err(PlanError::BudgetExceeded));
    assert_eq!(
        with_limits(&big, &max),
        [
            invalid(PlanError::BudgetExceeded),
            invalid(PlanError::BudgetExceeded)
        ]
    );

    // Order: cancel, then stale revision, then the limits, then the plan.
    let loose = PlanLimits {
        max_depth: 65,
        ..max
    };
    let p = small();
    let cancel = CancellationToken::default();
    let stale = ExecuteRequest {
        expected_revision: 99,
        ..request()
    };
    assert_eq!(
        admit_with_limits(&p, &loose, &backend(), &stale, &cancel).map(|_| ()),
        Err(BackendError::StaleRevision {
            expected: 99,
            found: 3
        })
    );
    cancel.cancel();
    assert_eq!(
        admit_with_limits(&p, &loose, &backend(), &request(), &cancel).map(|_| ()),
        Err(BackendError::Canceled)
    );
}

/// `PlanLimits::default()` is the maxima, with the values it always had, and `admit` and
/// `admit_extended` are the `_with_limits` functions with them: same ledger, same refusal, same
/// boundary (65 536 nodes or layers and depth 64 admit, one more is refused).
#[test]
fn defaults_are_the_maxima_and_unchanged() {
    assert_eq!(
        PlanLimits::default(),
        PlanLimits {
            max_nodes: 65_536,
            max_layers: 65_536,
            max_depth: 64
        }
    );
    assert_eq!(PlanLimits::default(), PlanLimits::MAXIMUM);
    assert_eq!(PlanLimits::default().within_maximum(), Ok(()));

    let d = PlanLimits::default();
    let cases: [(&str, RenderPlan, Result<(), PlanError>); 7] = [
        ("small", small(), Ok(())),
        ("65536 nodes", nodes_plan(65_536), Ok(())),
        (
            "65537 nodes",
            nodes_plan(65_537),
            Err(PlanError::BudgetExceeded),
        ),
        ("65536 layers", layers_plan(65_536), Ok(())),
        (
            "65537 layers",
            layers_plan(65_537),
            Err(PlanError::BudgetExceeded),
        ),
        ("depth 64", chain_plan(63), Ok(())),
        ("depth 65", chain_plan(64), Err(PlanError::TooDeep)),
    ];
    for (name, p, verdict) in cases {
        assert_eq!(p.validate(&d).map(|_| ()), verdict, "{name}");
        let by_default = defaults(&p);
        assert_eq!(with_limits(&p, &d), by_default, "{name}");
        assert_eq!(with_limits(&p, &PlanLimits::MAXIMUM), by_default, "{name}");
        match verdict {
            Ok(()) => assert!(by_default[0].is_ok() && by_default[1].is_ok(), "{name}"),
            Err(error) => assert_eq!(by_default, [invalid(error), invalid(error)], "{name}"),
        }
    }
    // The trait's own default route is the defaults too: a refused plan delivers nothing.
    let mut sink = CountingSink(0);
    let run = Limited { limits: d }
        .execute(
            &nodes_plan(65_537),
            &request(),
            &NoSources,
            &CancellationToken::default(),
            &mut sink,
        )
        .map(|_| ());
    assert_eq!(
        (run, sink.0),
        (Err(BackendError::InvalidPlan(PlanError::BudgetExceeded)), 0)
    );
}
