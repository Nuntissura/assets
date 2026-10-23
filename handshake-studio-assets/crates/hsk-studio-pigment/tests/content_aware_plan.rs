//! Lane SMB-PIGMENT-FILTER-DESCRIPTOR slice 2: content-aware fill as a typed render-plan envelope
//! operator (LayerExtension.content_aware_fill, ExtOperator::ContentAwareFill). Validation,
//! combination and parameter refusals, admission refusal for a backend that does not declare it
//! (the trait default, which is what render-cpu and render-gpu answer for unlisted operators), and
//! the digest staying unchanged for entries without it.
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blur_math::{BlurScope, LayerBlur};
use hsk_studio_pigment::content_aware::{ColourAdaptation, ContentAwareOptions};
use hsk_studio_pigment::filter_math::LayerFilter;
use hsk_studio_pigment::render_plan::*;

fn rect(x: i64, y: i64, width: u32, height: u32) -> Rect {
    Rect { x, y, width, height }
}

fn plan(mode: StudioBlendMode) -> RenderPlan {
    let r = rect(0, 0, 4, 4);
    let layer = |input: u32, mode: StudioBlendMode| Layer { input: PlanNodeId(input), mode, opacity: 1.0, fill_opacity: 1.0, mask: None };
    RenderPlan {
        revision: 1,
        target: OutputTarget {
            extent: r,
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded },
        },
        nodes: vec![
            PlanNode::Solid { rect: r, rgba: [0.2, 0.4, 0.6, 1.0] },
            PlanNode::Solid { rect: rect(1, 1, 2, 2), rgba: [1.0, 1.0, 1.0, 0.5] },
            PlanNode::Group(Group { isolated: true, layers: vec![layer(0, StudioBlendMode::Normal), layer(1, mode)] }),
        ],
        root: PlanNodeId(2),
    }
}

fn hole() -> MaskInput {
    MaskInput { key: SourceKey(9), placement: rect(1, 1, 2, 2), format: MaskFormat::CoverageU8, sha256: [3; 32], inverted: false, density: 1.0 }
}

struct Backend {
    declares: bool,
}
impl RenderBackend for Backend {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity { name: "content-aware-plan-test", version: "0", device: DeviceClass::Cpu, adapter: String::new(), generation: 0 }
    }
    fn exactness(&self, _: Operator) -> Exactness {
        Exactness::Exact
    }
    fn extension_exactness(&self, op: ExtOperator) -> Exactness {
        if self.declares { op.intrinsic() } else { Exactness::Unsupported }
    }
    fn execute(&self, _: &RenderPlan, _: &ExecuteRequest, _: &dyn PlanSources, _: &CancellationToken, _: &mut dyn TileSink) -> Result<RenderReceipt, BackendError> {
        Err(BackendError::Unavailable("content_aware_plan_test_execute"))
    }
}

/// A backend that keeps the trait's default `extension_exactness` (declares nothing).
struct Plain;
impl RenderBackend for Plain {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity { name: "plain", version: "0", device: DeviceClass::Cpu, adapter: String::new(), generation: 0 }
    }
    fn exactness(&self, _: Operator) -> Exactness {
        Exactness::Exact
    }
    fn execute(&self, _: &RenderPlan, _: &ExecuteRequest, _: &dyn PlanSources, _: &CancellationToken, _: &mut dyn TileSink) -> Result<RenderReceipt, BackendError> {
        Err(BackendError::Unavailable("plain_execute"))
    }
}

#[test]
fn content_aware_fill_is_a_typed_plan_operator() {
    let p = plan(StudioBlendMode::Normal);
    let op = ContentAwareFillOp::new(hole());
    let entry = || LayerExtension::new(PlanNodeId(2), 1).with_content_aware_fill(op);
    let ext = PlanExtensions::from_layers(vec![entry()]);
    assert_eq!(ext.validate(&p), Ok(vec![ExtOperator::ContentAwareFill]));
    assert_eq!(ExtOperator::ContentAwareFill.key(), "content_aware_fill");
    assert_eq!(ExtOperator::ContentAwareFill.intrinsic(), Exactness::Approximate);
    // Admission: undeclared (the trait default, and the stub) is refused under the key before any
    // sink call; a backend that declares it admits with the Approximate class.
    let cancel = CancellationToken::default();
    let req = ExecuteRequest { expected_revision: 1, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 20 };
    let refused = Err(BackendError::Unsupported { operator: "content_aware_fill", exactness: Exactness::Unsupported });
    assert_eq!(admit_extended(&p, &ext, &Plain, &req, &cancel).map(|_| ()), refused);
    assert_eq!(admit_extended(&p, &ext, &Backend { declares: false }, &req, &cancel).map(|_| ()), refused);
    let ledger = admit_extended(&p, &ext, &Backend { declares: true }, &req, &cancel).unwrap();
    assert_eq!(ledger.lowest, Exactness::Approximate);
    assert!(ledger.operators.iter().any(|o| o.key == "content_aware_fill"));
    // Digest: entries without the fill serialise without the member; the thread count is not
    // part of the digest, every other option is.
    let plain = serde_json::to_string(&LayerExtension::new(PlanNodeId(2), 1)).unwrap();
    assert!(!plain.contains("content_aware_fill"), "{plain}");
    let with = serde_json::to_string(&entry()).unwrap();
    assert!(with.contains("\"content_aware_fill\"") && with.contains("\"colour_adaptation\":\"default\"") && !with.contains("threads"), "{with}");
    let d = |o: ContentAwareOptions| {
        let e = PlanExtensions::from_layers(vec![LayerExtension::new(PlanNodeId(2), 1).with_content_aware_fill(ContentAwareFillOp { options: o, ..op })]);
        e.digest_with(&p).unwrap()
    };
    let base = ContentAwareOptions::default();
    assert_eq!(d(base), d(ContentAwareOptions { threads: 8, ..base }));
    assert_ne!(d(base), d(ContentAwareOptions { colour_adaptation: ColourAdaptation::High, ..base }));
}

#[test]
fn content_aware_fill_parameters_and_combinations_are_refused() {
    let p = plan(StudioBlendMode::Normal);
    let op = ContentAwareFillOp::new(hole());
    let at = |e: LayerExtension| PlanExtensions::from_layers(vec![e]).validate(&p);
    let base = || LayerExtension::new(PlanNodeId(2), 1);
    let param = Err(PlanError::ContentAwareParameter { node: 2 });
    // Options out of range (never clamped), clip-to-base or density-scaled holes, bad rectangles.
    for bad in [
        ContentAwareFillOp { options: ContentAwareOptions { patch_radius: 0, ..op.options }, ..op },
        ContentAwareFillOp { options: ContentAwareOptions { threads: 65, ..op.options }, ..op },
        ContentAwareFillOp { hole: MaskInput::clip_to_base(), ..op },
        ContentAwareFillOp { hole: MaskInput { density: 0.5, ..hole() }, ..op },
        ContentAwareFillOp { hole: MaskInput { density: f32::NAN, ..hole() }, ..op },
        ContentAwareFillOp { hole: MaskInput { placement: rect(i64::MAX, 0, 2, 2), ..hole() }, ..op },
        ContentAwareFillOp { sampling: Some(rect(0, 0, 0, 4)), ..op },
        ContentAwareFillOp { sampling: Some(rect(i64::MAX - 1, 0, 4, 4)), ..op },
    ] {
        assert_eq!(at(base().with_content_aware_fill(bad)), param, "{bad:?}");
    }
    assert_eq!(PlanError::ContentAwareParameter { node: 2 }.code(), "plan_content_aware_parameter");
    // Combinations on one layer.
    let combo = Err(PlanError::ContentAwareCombination { node: 2 });
    let filled = || base().with_content_aware_fill(op);
    assert_eq!(at(filled().with_blur(LayerBlur::new(BlurScope::Layer, 2.0))), combo);
    assert_eq!(at(filled().with_filters(vec![LayerFilter::Median { radius_px: 1 }])), combo);
    assert_eq!(PlanError::ContentAwareCombination { node: 2 }.code(), "plan_content_aware_combination");
    // A pass-through layer.
    let pt = plan(StudioBlendMode::PassThrough);
    assert_eq!(PlanExtensions::from_layers(vec![filled()]).validate(&pt), combo);
    // A rectangular sampling area is a valid parameter.
    assert_eq!(at(base().with_content_aware_fill(ContentAwareFillOp { sampling: Some(rect(0, 0, 4, 4)), ..op })), Ok(vec![ExtOperator::ContentAwareFill]));
}
