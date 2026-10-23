//! Content-aware fill on the GPU backend (lane SMB-RENDER-CONTENT-AWARE): not implemented yet, so
//! `ExtOperator::ContentAwareFill` is declared Unsupported and shared admission refuses it typed
//! before any device work. Hardware-independent: on the `none` backend the refusal is
//! `Unsupported { content_aware_fill }`, not `Unavailable` (admission precedes the device check);
//! on a live adapter the refusal is the same, with no sink call and no device residency.
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::content_aware::ContentAwareOptions;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BlendSpace, ColourTag, ContentAwareFillOp, Exactness,
    ExecuteRequest, ExtOperator, Group, Layer, LayerExtension, MaskFormat, MaskInput, NoVector,
    OutputTarget, PlanExtensions, PlanNode, PlanNodeId, PlanSources, RenderBackend, RenderPlan,
    SampleFormat, SinkError, SourceKey, StudioBlendMode, TilePlanes, TileSink,
};
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_render_gpu::{BackendChoice, DeviceOptions, GpuBackend};

fn rect(x: i64, y: i64, width: u32, height: u32) -> Rect {
    Rect { x, y, width, height }
}
fn plan() -> RenderPlan<NoVector> {
    let r = rect(0, 0, 8, 8);
    let layer = |input: u32| Layer {
        input: PlanNodeId(input),
        mode: StudioBlendMode::Normal,
        opacity: 1.0,
        fill_opacity: 1.0,
        mask: None,
    };
    RenderPlan {
        revision: 4,
        target: OutputTarget {
            extent: r,
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::LinearLight },
        },
        nodes: vec![
            PlanNode::Solid { rect: r, rgba: [0.2, 0.4, 0.6, 1.0] },
            PlanNode::Solid { rect: rect(2, 2, 4, 4), rgba: [0.9, 0.1, 0.1, 1.0] },
            PlanNode::Group(Group { isolated: true, layers: vec![layer(0), layer(1)] }),
        ],
        root: PlanNodeId(2),
    }
}
fn envelope() -> PlanExtensions {
    let hole = MaskInput {
        key: SourceKey(9),
        placement: rect(3, 3, 2, 2),
        format: MaskFormat::CoverageU8,
        sha256: [3; 32],
        inverted: false,
        density: 1.0,
    };
    PlanExtensions::from_layers(vec![LayerExtension::new(PlanNodeId(2), 1).with_content_aware_fill(
        ContentAwareFillOp { hole, sampling: None, options: ContentAwareOptions::default() },
    )])
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
#[derive(Default)]
struct Count(usize);
impl TileSink for Count {
    fn accept(&mut self, _: Rect, _: &[u8], _: &[u8]) -> Result<(), SinkError> {
        self.0 += 1;
        Ok(())
    }
}

/// The GPU backend declares no content-aware fill: the refusal is `Unsupported(content_aware_fill)`
/// on the `none` backend (so it precedes the device check) and on the platform backend (with or
/// without an adapter), with no sink call and nothing resident; the same plan without the fill is
/// not refused for that reason.
#[test]
fn gpu_content_aware_fill_is_refused_before_any_device_work() {
    let refusal = Err(BackendError::Unsupported {
        operator: "content_aware_fill",
        exactness: Exactness::Unsupported,
    });
    for choice in [BackendChoice::None, BackendChoice::Auto] {
        let backend = GpuBackend::new(DeviceOptions { backend: choice, ..DeviceOptions::default() });
        assert_eq!(backend.extension_exactness(ExtOperator::ContentAwareFill), Exactness::Unsupported);
        let request = ExecuteRequest {
            expected_revision: 4,
            min_exactness: Exactness::Approximate,
            deadline: None,
            max_working_bytes: 1 << 26,
        };
        let mut sink = Count::default();
        let result = backend.execute_extended(
            &plan(),
            &envelope(),
            &request,
            &NoSources,
            &CancellationToken::default(),
            &mut sink,
        );
        assert_eq!(result.map(|_| ()), refusal, "{choice:?}");
        assert_eq!(sink.0, 0, "{choice:?}: sink touched");
        assert_eq!(backend.resident_bytes(), 0, "{choice:?}");
        eprintln!("{choice:?}: refused before device work (device {:?})", backend.device_info().map(|i| i.text()));
    }
    // Control: without the fill, the none backend reaches the device check (Unavailable).
    let none = GpuBackend::new(DeviceOptions { backend: BackendChoice::None, ..DeviceOptions::default() });
    let request = ExecuteRequest { expected_revision: 4, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 26 };
    let mut sink = Count::default();
    let plain = none.execute(&plan(), &request, &NoSources, &CancellationToken::default(), &mut sink);
    assert_eq!(plain.map(|_| ()), Err(BackendError::Unavailable("no_adapter")));
}
