//! Blend-If on a clip base and on a clipped layer (decision: work-folder
//! `diagnostics/adjustments-advanced-blending.md`, topic `blend-if-clip-roles`): no public source
//! defines either, so pigment's shared admission refuses both identically for every backend.
//! Layer indexes and expected operators are derived by hand from the plan built in `plan()`.
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::adjust_math::AdjustmentOp;
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
fn clipped(input: u32) -> Layer {
    Layer {
        mask: Some(MaskInput::clip_to_base()),
        ..layer(input)
    }
}
fn solid(rgba: [f32; 4]) -> PlanNode {
    PlanNode::Solid { rect: R, rgba }
}

/// Nodes 0..=2: opaque solids; 3: transparent adjustment placeholder; 4: isolated root group whose
/// layers (bottom first) are
/// 0 base A | 1 c1 clipped | 2 c2 clipped | 3 adjustment clipped | 4 D | 5 E | 6 adjustment.
/// Clip roles: A = Base; 1, 2, 3 = Clipped; D, E and the last adjustment layer = Unclipped.
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
            solid([0.8, 0.1, 0.1, 1.0]),
            solid([0.1, 0.8, 0.1, 1.0]),
            solid([0.1, 0.1, 0.8, 1.0]),
            solid([0.0; 4]),
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![
                    layer(0),
                    clipped(1),
                    clipped(2),
                    clipped(3),
                    layer(0),
                    layer(1),
                    layer(3),
                ],
            }),
        ],
        root: PlanNodeId(4),
    }
}
const GROUP: PlanNodeId = PlanNodeId(4);

fn ramp() -> Vec<BlendRangeEntry> {
    vec![BlendRangeEntry {
        channel: BlendChannel::Gray,
        source: [0.2, 0.4, 0.6, 1.0],
        destination: IDENTITY_RANGE,
    }]
}
fn on(layer: u32) -> PlanExtensions {
    PlanExtensions::from_layers(vec![
        LayerExtension::new(GROUP, layer).with_blend_if(ramp()),
    ])
}
fn on_adjustment(layer: u32) -> PlanExtensions {
    PlanExtensions::from_layers(vec![
        LayerExtension::new(GROUP, layer)
            .with_adjustment(AdjustmentOp::Invert)
            .with_blend_if(ramp()),
    ])
}

/// Claims every envelope operator at class `ext` and the clip-to-base route (the most permissive
/// backend that can exist: the refusal must not depend on what a backend claims).
struct Claims {
    ext: Exactness,
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
    fn supports_clip_to_base(&self) -> bool {
        true
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
        Err(BackendError::Unavailable("claims_execute"))
    }
}
/// Supports clipping but no envelope operator at all (trait defaults).
struct Bare;
impl RenderBackend for Bare {
    fn identity(&self) -> BackendIdentity {
        Claims {
            ext: Exactness::Exact,
        }
        .identity()
    }
    fn exactness(&self, _: Operator) -> Exactness {
        Exactness::Exact
    }
    fn supports_clip_to_base(&self) -> bool {
        true
    }
    fn execute(
        &self,
        _: &RenderPlan,
        _: &ExecuteRequest,
        _: &dyn PlanSources,
        _: &CancellationToken,
        _: &mut dyn TileSink,
    ) -> Result<RenderReceipt, BackendError> {
        Err(BackendError::Unavailable("bare_execute"))
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
fn req() -> ExecuteRequest {
    ExecuteRequest {
        expected_revision: 3,
        min_exactness: Exactness::Approximate,
        deadline: None,
        max_working_bytes: 1 << 20,
    }
}
fn refused(key: &'static str) -> Result<ExactnessLedger, BackendError> {
    Err(BackendError::Unsupported {
        operator: key,
        exactness: Exactness::Unsupported,
    })
}

#[test]
fn clip_roles_classify_blend_if_in_validate() {
    let p = plan();
    let PlanNode::Group(g) = &p.nodes[4] else {
        unreachable!()
    };
    let roles: Vec<_> = (0..8).map(|i| g.clip_role(i)).collect();
    assert_eq!(
        roles,
        vec![
            Some(ClipRole::Base),
            Some(ClipRole::Clipped),
            Some(ClipRole::Clipped),
            Some(ClipRole::Clipped),
            Some(ClipRole::Unclipped),
            Some(ClipRole::Unclipped),
            Some(ClipRole::Unclipped),
            None,
        ]
    );

    // The base and the clipped layers get their own operators INSTEAD of `blend_if`.
    assert_eq!(on(0).validate(&p), Ok(vec![ExtOperator::BlendIfClipBase]));
    assert_eq!(on(1).validate(&p), Ok(vec![ExtOperator::BlendIfClippedLayer]));
    assert_eq!(on(2).validate(&p), Ok(vec![ExtOperator::BlendIfClippedLayer]));
    // Unclipped layers, before or after a run, keep the plain operator.
    assert_eq!(on(4).validate(&p), Ok(vec![ExtOperator::BlendIf]));
    assert_eq!(on(5).validate(&p), Ok(vec![ExtOperator::BlendIf]));
    // A clipped adjustment layer is a clipped layer; an unclipped one is not a clip role.
    assert_eq!(
        on_adjustment(3).validate(&p),
        Ok(vec![
            ExtOperator::AdjustmentLayer,
            ExtOperator::Adjustment(hsk_studio_pigment::adjust_math::AdjustmentKind::Invert),
            ExtOperator::BlendIfClippedLayer
        ])
    );
    assert_eq!(
        on_adjustment(6).validate(&p),
        Ok(vec![
            ExtOperator::AdjustmentLayer,
            ExtOperator::Adjustment(hsk_studio_pigment::adjust_math::AdjustmentKind::Invert),
            ExtOperator::BlendIf
        ])
    );
    // First-use order across layers, one operator per role.
    let all = PlanExtensions::from_layers(vec![
        LayerExtension::new(GROUP, 0).with_blend_if(ramp()),
        LayerExtension::new(GROUP, 1).with_blend_if(ramp()),
        LayerExtension::new(GROUP, 2).with_blend_if(ramp()),
        LayerExtension::new(GROUP, 4).with_blend_if(ramp()),
    ]);
    assert_eq!(
        all.validate(&p),
        Ok(vec![
            ExtOperator::BlendIfClipBase,
            ExtOperator::BlendIfClippedLayer,
            ExtOperator::BlendIf
        ])
    );
    // Identity ranges change nothing wherever they sit.
    let identity = PlanExtensions::from_layers(vec![
        LayerExtension::new(GROUP, 0).with_blend_if(vec![BlendRangeEntry::identity(
            BlendChannel::Gray,
        )]),
        LayerExtension::new(GROUP, 1).with_blend_if(vec![BlendRangeEntry::identity(
            BlendChannel::Red,
        )]),
    ]);
    assert_eq!(identity.validate(&p), Ok(vec![]));
}

#[test]
fn clip_roles_refuse_blend_if_uniformly_for_every_backend() {
    let p = plan();
    let cancel = CancellationToken::default();
    let cases: [(PlanExtensions, &'static str); 4] = [
        (on(0), "blend_if_clip_base"),
        (on(1), "blend_if_clipped_layer"),
        (on(2), "blend_if_clipped_layer"),
        (on_adjustment(3), "blend_if_clipped_layer"),
    ];
    let exact = Claims {
        ext: Exactness::Exact,
    };
    let approx = Claims {
        ext: Exactness::Approximate,
    };
    for (i, (ext, _)) in cases.iter().enumerate() {
        let key: &'static str = cases[i].1;
        // The same typed refusal for a backend claiming everything Exact and one claiming
        // Approximate (counterfactual: the claim is irrelevant because pigment rates the operator
        // Unsupported).
        assert_eq!(admit_extended(&p, ext, &exact, &req(), &cancel), refused(key));
        assert_eq!(admit_extended(&p, ext, &approx, &req(), &cancel), refused(key));
        // A backend with no envelope support refuses a bare Blend-If layer with the clip-role key
        // too; an adjustment layer is refused earlier, as `adjustment_layer`, by such a backend.
        if i < 3 {
            assert_eq!(admit_extended(&p, ext, &Bare, &req(), &cancel), refused(key));
        } else {
            assert_eq!(
                admit_extended(&p, ext, &Bare, &req(), &cancel),
                refused("adjustment_layer")
            );
        }
        // The refusal happens at admission, before the backend's own execute path.
        let run = exact.execute_extended(&p, ext, &req(), &NoSources, &cancel, &mut NoSink);
        assert_eq!(
            run,
            Err(BackendError::Unsupported {
                operator: key,
                exactness: Exactness::Unsupported
            })
        );
    }

    // Counterfactual: the same ranges on an unclipped layer ARE admitted by a backend that claims
    // `blend_if`, as Approximate, and refused as plain `blend_if` by one that does not.
    for layer in [4, 5] {
        let ledger = admit_extended(&p, &on(layer), &exact, &req(), &cancel).unwrap();
        assert_eq!(ledger.lowest, Exactness::Approximate);
        assert!(ledger.operators.contains(&OperatorExactness {
            key: "blend_if",
            exactness: Exactness::Approximate
        }));
        assert_eq!(
            admit_extended(&p, &on(layer), &Bare, &req(), &cancel),
            refused("blend_if")
        );
    }
    let ledger = admit_extended(&p, &on_adjustment(6), &exact, &req(), &cancel).unwrap();
    assert!(
        ledger
            .operators
            .iter()
            .all(|o| !o.key.starts_with("blend_if_"))
    );
}

#[test]
fn identity_ranges_on_clip_roles_are_not_refused() {
    let p = plan();
    let cancel = CancellationToken::default();
    let ext = PlanExtensions::from_layers(vec![
        LayerExtension::new(GROUP, 0)
            .with_blend_if(vec![BlendRangeEntry::identity(BlendChannel::Gray)]),
        LayerExtension::new(GROUP, 2)
            .with_blend_if(vec![BlendRangeEntry::identity(BlendChannel::Blue)]),
    ]);
    let full = Claims {
        ext: Exactness::Exact,
    };
    let ledger = admit_extended(&p, &ext, &full, &req(), &cancel).unwrap();
    assert!(ledger.operators.iter().all(|o| !o.key.starts_with("blend_if")));
    // A clip-role entry next to an admitted plain one still refuses the whole plan.
    let mixed = PlanExtensions::from_layers(vec![
        LayerExtension::new(GROUP, 4).with_blend_if(ramp()),
        LayerExtension::new(GROUP, 0).with_blend_if(ramp()),
    ]);
    assert_eq!(
        admit_extended(&p, &mixed, &full, &req(), &cancel),
        refused("blend_if_clip_base")
    );
}

#[test]
fn clip_role_operator_keys_classes_and_descriptor_claim() {
    assert_eq!(ExtOperator::BlendIfClipBase.key(), "blend_if_clip_base");
    assert_eq!(ExtOperator::BlendIfClippedLayer.key(), "blend_if_clipped_layer");
    assert_eq!(ExtOperator::BlendIfClipBase.intrinsic(), Exactness::Unsupported);
    assert_eq!(
        ExtOperator::BlendIfClippedLayer.intrinsic(),
        Exactness::Unsupported
    );
    // The plain operator stays Approximate (unchanged).
    assert_eq!(ExtOperator::BlendIf.intrinsic(), Exactness::Approximate);
    // The DESCRIPTOR claim cites this test file (V4-07).
    assert!(DESCRIPTOR.contains("blend_if_clip_base"));
    assert!(DESCRIPTOR.contains("blend_if_clipped_layer"));
}
