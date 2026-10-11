//! Render-plan extension envelope (`PlanExtensions`): validation, shared admission, the default
//! `execute_extended` route and the single-pixel adjustment oracle node. Node ids and expected
//! errors are derived by hand from the plan built in `plan()`.
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::adjust_math::{AdjustmentKind, AdjustmentOp, BrightnessContrast};
use hsk_studio_pigment::blend_math::{BlendContext, PixelSite, Rgba, StackNode, evaluate_stack};
use hsk_studio_pigment::coverage_math::{BlendChannel, BlendRangeEntry, IDENTITY_RANGE};
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

/// 0: opaque solid backdrop; 1: transparent placeholder; 2: isolated group [0, 1] (root).
fn plan() -> RenderPlan {
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
        nodes: vec![
            PlanNode::Solid {
                rect: R,
                rgba: [0.2, 0.4, 0.6, 1.0],
            },
            PlanNode::Solid {
                rect: R,
                rgba: [0.0; 4],
            },
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![layer(0), layer(1)],
            }),
        ],
        root: PlanNodeId(2),
    }
}

fn adjustment(op: AdjustmentOp) -> PlanExtensions {
    PlanExtensions::from_layers(vec![
        LayerExtension::new(PlanNodeId(2), 1).with_adjustment(op),
    ])
}

struct Fake {
    ext: Exactness,
}
impl RenderBackend for Fake {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            name: "fake",
            version: "0",
            device: DeviceClass::Cpu,
            adapter: String::new(),
            generation: 0,
        }
    }
    fn exactness(&self, _: Operator) -> Exactness {
        Exactness::Exact
    }
    fn extension_exactness(&self, _: ExtOperator) -> Exactness {
        self.ext
    }
    fn execute(
        &self,
        _: &RenderPlan,
        _: &ExecuteRequest,
        _: &dyn PlanSources,
        _: &CancellationToken,
        _: &mut dyn TileSink,
    ) -> Result<RenderReceipt, BackendError> {
        Err(BackendError::Unavailable("fake_execute"))
    }
}
/// A backend written before the envelope existed (all trait defaults).
struct Legacy;
impl RenderBackend for Legacy {
    fn identity(&self) -> BackendIdentity {
        Fake {
            ext: Exactness::Exact,
        }
        .identity()
    }
    fn exactness(&self, _: Operator) -> Exactness {
        Exactness::Exact
    }
    fn execute(
        &self,
        _: &RenderPlan,
        _: &ExecuteRequest,
        _: &dyn PlanSources,
        _: &CancellationToken,
        _: &mut dyn TileSink,
    ) -> Result<RenderReceipt, BackendError> {
        Err(BackendError::Unavailable("legacy_execute"))
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
struct NoSink;
impl TileSink for NoSink {
    fn accept(&mut self, _: Rect, _: &[u8], _: &[u8]) -> Result<(), SinkError> {
        Err(SinkError::Rejected)
    }
}
fn req(min: Exactness) -> ExecuteRequest {
    ExecuteRequest {
        expected_revision: 3,
        min_exactness: min,
        deadline: None,
        max_working_bytes: 1 << 20,
    }
}

#[test]
fn envelope_validation_rules() {
    let p = plan();
    let ok = adjustment(AdjustmentOp::Invert);
    assert_eq!(
        ok.validate(&p),
        Ok(vec![
            ExtOperator::AdjustmentLayer,
            ExtOperator::Adjustment(AdjustmentKind::Invert)
        ])
    );
    assert_eq!(PlanExtensions::default().validate(&p), Ok(vec![]));

    // Node 0 is a Solid, not a group; layer 5 of group 2 does not exist.
    let target = PlanExtensions::from_layers(vec![LayerExtension::new(PlanNodeId(0), 0)]);
    assert_eq!(
        target.validate(&p),
        Err(PlanError::ExtensionTarget { node: 0 })
    );
    let index = PlanExtensions::from_layers(vec![LayerExtension::new(PlanNodeId(2), 5)]);
    assert_eq!(
        index.validate(&p),
        Err(PlanError::ExtensionTarget { node: 2 })
    );
    let dup = PlanExtensions::from_layers(vec![
        LayerExtension::new(PlanNodeId(2), 0),
        LayerExtension::new(PlanNodeId(2), 0),
    ]);
    assert_eq!(
        dup.validate(&p),
        Err(PlanError::ExtensionDuplicate { node: 2 })
    );

    // Layer 0's input (node 0) is opaque, not the transparent placeholder.
    let input = PlanExtensions::from_layers(vec![
        LayerExtension::new(PlanNodeId(2), 0).with_adjustment(AdjustmentOp::Invert),
    ]);
    assert_eq!(
        input.validate(&p),
        Err(PlanError::AdjustmentInput { node: 2 })
    );

    let modern = adjustment(AdjustmentOp::BrightnessContrast(BrightnessContrast {
        brightness: 10.0,
        contrast: 0.0,
        use_legacy: false,
    }));
    assert_eq!(
        modern.validate(&p),
        Err(PlanError::AdjustmentUnsupported {
            node: 2,
            kind: AdjustmentKind::BrightnessContrast
        })
    );
    assert_eq!(
        adjustment(AdjustmentOp::Posterize { levels: 1 }).validate(&p),
        Err(PlanError::AdjustmentParameter { node: 2 })
    );

    let deep = PlanExtensions::from_layers(vec![
        LayerExtension::new(PlanNodeId(2), 0).with_knockout(Knockout::Deep),
    ]);
    assert_eq!(
        deep.validate(&p),
        Err(PlanError::KnockoutUnsupported {
            node: 2,
            depth: Knockout::Deep
        })
    );

    let feather = PlanExtensions::from_layers(vec![
        LayerExtension::new(PlanNodeId(2), 0).with_mask_feather(4.0),
    ]);
    assert_eq!(
        feather.validate(&p),
        Err(PlanError::FeatherWithoutMask { node: 2 })
    );

    let entry = |source| BlendRangeEntry {
        channel: BlendChannel::Gray,
        source,
        destination: IDENTITY_RANGE,
    };
    let blend_if = |entries: Vec<BlendRangeEntry>| {
        PlanExtensions::from_layers(vec![
            LayerExtension::new(PlanNodeId(2), 0).with_blend_if(entries),
        ])
    };
    assert_eq!(
        blend_if(vec![entry([0.2, 0.4, 0.6, 1.0])]).validate(&p),
        Ok(vec![ExtOperator::BlendIf])
    );
    assert_eq!(
        blend_if(vec![entry(IDENTITY_RANGE)]).validate(&p),
        Ok(vec![])
    );
    assert_eq!(
        blend_if(vec![entry([0.4, 0.2, 0.6, 1.0])]).validate(&p),
        Err(PlanError::BlendIfRange { node: 2 })
    );
    assert_eq!(
        blend_if(vec![entry(IDENTITY_RANGE), entry(IDENTITY_RANGE)]).validate(&p),
        Err(PlanError::BlendIfRange { node: 2 })
    );

    // A clipped layer after the adjustment layer would make it a clip base.
    let mut clip = plan();
    let PlanNode::Group(g) = &mut clip.nodes[2] else {
        unreachable!()
    };
    g.layers.push(Layer {
        mask: Some(MaskInput::clip_to_base()),
        ..layer(0)
    });
    assert_eq!(
        adjustment(AdjustmentOp::Invert).validate(&clip),
        Err(PlanError::AdjustmentClipBase { node: 2 })
    );
}

#[test]
fn envelope_admission_and_default_route() {
    let p = plan();
    let cancel = CancellationToken::default();
    let ext = adjustment(AdjustmentOp::Invert);

    // A backend that never heard of the envelope refuses it at admission, naming the operator.
    assert_eq!(
        admit_extended(&p, &ext, &Legacy, &req(Exactness::Approximate), &cancel),
        Err(BackendError::Unsupported {
            operator: "adjustment_layer",
            exactness: Exactness::Unsupported
        })
    );
    let full = Fake {
        ext: Exactness::Exact,
    };
    let ledger = admit_extended(&p, &ext, &full, &req(Exactness::Approximate), &cancel).unwrap();
    assert_eq!(ledger.lowest, Exactness::Approximate);
    assert!(ledger.operators.contains(&OperatorExactness {
        key: "invert",
        exactness: Exactness::Exact
    }));
    assert!(ledger.operators.contains(&OperatorExactness {
        key: "adjustment_layer",
        exactness: Exactness::Approximate
    }));
    // An Exact floor refuses the Approximate adjustment-layer rule.
    assert_eq!(
        admit_extended(&p, &ext, &full, &req(Exactness::Exact), &cancel),
        Err(BackendError::Unsupported {
            operator: "adjustment_layer",
            exactness: Exactness::Approximate
        })
    );
    let deep = PlanExtensions::from_layers(vec![
        LayerExtension::new(PlanNodeId(2), 0).with_knockout(Knockout::Deep),
    ]);
    assert_eq!(
        admit_extended(&p, &deep, &full, &req(Exactness::Approximate), &cancel),
        Err(BackendError::Unsupported {
            operator: "knockout_deep",
            exactness: Exactness::Unsupported
        })
    );

    // Default execute_extended: empty envelope -> v1 execute; otherwise refuse, never ignore.
    let run = |b: &dyn RenderBackend, e: &PlanExtensions| {
        b.execute_extended(
            &p,
            e,
            &req(Exactness::Approximate),
            &NoSources,
            &cancel,
            &mut NoSink,
        )
    };
    assert_eq!(
        run(&Legacy, &PlanExtensions::default()),
        Err(BackendError::Unavailable("legacy_execute"))
    );
    assert_eq!(
        run(&Legacy, &ext),
        Err(BackendError::Unsupported {
            operator: "adjustment_layer",
            exactness: Exactness::Unsupported
        })
    );
    assert_eq!(
        run(&full, &ext),
        Err(BackendError::Unavailable(
            "execute_extended_not_implemented"
        ))
    );

    // Digest: an empty envelope digests like the plain plan; a non-empty one binds the envelope.
    assert_eq!(
        PlanExtensions::default().digest_with(&p).unwrap(),
        p.digest().unwrap()
    );
    assert_ne!(ext.digest_with(&p).unwrap(), p.digest().unwrap());
}

/// R5 (validation/R5-review-a.json, SMB-PIGMENT-ENG-1): a pass-through layer with fill < 1 was
/// rated `fill_opacity` Approximate by validate and refused by each backend with a different code.
/// Now validate does not rate it and admit refuses it uniformly, even for a backend claiming
/// every operator Exact.
#[test]
fn pass_through_fill_is_refused_uniformly_by_admit() {
    let cancel = CancellationToken::default();
    let full = Fake {
        ext: Exactness::Exact,
    };
    // 0: opaque solid; 1: non-isolated group [0]; 2: isolated root [1 as PassThrough, fill f].
    let pass = |mode: StudioBlendMode, fill: f32, isolated_child: bool| {
        let mut p = plan();
        p.nodes = vec![
            PlanNode::Solid {
                rect: R,
                rgba: [0.2, 0.4, 0.6, 1.0],
            },
            PlanNode::Group(Group {
                isolated: isolated_child,
                layers: vec![layer(0)],
            }),
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![Layer {
                    mode,
                    fill_opacity: fill,
                    ..layer(1)
                }],
            }),
        ];
        p
    };
    let refused = pass(StudioBlendMode::PassThrough, 0.5, false);
    let ops = refused.validate(&PlanLimits::default()).unwrap();
    assert!(ops.contains(&Operator::PassThroughGroup) && !ops.contains(&Operator::FillOpacity));
    assert!(refused.has_pass_through_fill());
    for floor in [Exactness::Exact, Exactness::Approximate] {
        assert_eq!(
            admit(&refused, &full, &req(floor), &cancel),
            Err(BackendError::Unsupported {
                operator: PASS_THROUGH_FILL_OPERATOR,
                exactness: Exactness::Unsupported
            })
        );
    }
    // Controls: fill 1 pass-through is admitted; a Multiply layer keeps its Approximate fill.
    assert!(
        admit(
            &pass(StudioBlendMode::PassThrough, 1.0, false),
            &full,
            &req(Exactness::Exact),
            &cancel
        )
        .is_ok()
    );
    let multiply = pass(StudioBlendMode::Multiply, 0.5, true);
    assert!(
        multiply
            .validate(&PlanLimits::default())
            .unwrap()
            .contains(&Operator::FillOpacity)
    );
    let ledger = admit(&multiply, &full, &req(Exactness::Approximate), &cancel).unwrap();
    assert_eq!(ledger.lowest, Exactness::Approximate);
}

#[test]
fn stack_oracle_adjustment_node() {
    // Invert over opaque (0.2, 0.4, 0.6), Normal, opacity 1 -> (0.8, 0.6, 0.4), alpha 1.
    let ctx = BlendContext {
        space: BlendSpace::Encoded,
        site: PixelSite::default(),
    };
    let nodes = [StackNode::Adjustment {
        op: AdjustmentOp::Invert,
        mode: StudioBlendMode::Normal,
        opacity: 1.0,
    }];
    let (out, receipt) = evaluate_stack(&nodes, Rgba::new([0.2, 0.4, 0.6], 1.0), &ctx).unwrap();
    assert!((0..3).all(|i| (out.rgb[i] - [0.8, 0.6, 0.4][i]).abs() <= 1e-6));
    assert_eq!(out.a, 1.0);
    assert_eq!(receipt.exactness.lowest, Exactness::Approximate);
    let keys: Vec<_> = receipt.exactness.operators.iter().map(|o| o.key).collect();
    assert_eq!(keys, ["adjustment_layer", "invert", "normal"]);
}
