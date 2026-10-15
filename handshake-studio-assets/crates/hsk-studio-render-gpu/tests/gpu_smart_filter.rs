//! Smart-filter stacks on the GPU backend (lane SMB-PS-SELECT-LIQUIFY-2): there is no GPU pass, so
//! the envelope operator `smart_filter_stack` is declared Unsupported and shared admission refuses
//! the plan under that key before any device work or sink call, on any machine (no adapter
//! needed). The CPU backend runs the stack (render-cpu tests/cpu_smart_filter.rs).
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::filter_math::LayerFilter;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BlendSpace, ColourTag, Exactness, ExecuteRequest, ExtOperator, Group, Layer,
    LayerExtension, NoVector, OutputTarget, PlanExtensions, PlanNode, PlanNodeId, PlanSources, RenderBackend,
    RenderPlan, SampleFormat, SinkError, SourceKey, StudioBlendMode, TileInput, TilePlanes, TileSink,
};
use hsk_studio_pigment::smart_filter::{SMART_FILTER_OPERATOR, SmartFilterEntryOp, SmartFilterOp, StackFilter};
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_render_gpu::{DeviceOptions, GpuBackend};

const EXTENT: Rect = Rect { x: 0, y: 0, width: 8, height: 6 };

fn tag() -> ColourTag {
    ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::LinearLight }
}

struct Tile {
    colour: Vec<u8>,
    alpha: Vec<u8>,
}
impl PlanSources for Tile {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        (key.0 == 1).then(|| TilePlanes {
            width: 4,
            height: 3,
            colour: &self.colour,
            colour_stride_bytes: 48,
            alpha: &self.alpha,
            alpha_stride_bytes: 16,
            colour_sha256: hsk_studio_prism::profile_hash(&self.colour),
            alpha_sha256: hsk_studio_prism::profile_hash(&self.alpha),
            format: SampleFormat::RgbF32Le,
            alpha_convention: AlphaConvention::Straight,
            space: tag(),
        })
    }
    fn mask(&self, _key: SourceKey) -> Option<Mask<'_>> {
        None
    }
}

struct Calls(usize);
impl TileSink for Calls {
    fn accept(&mut self, _r: Rect, _colour: &[u8], _alpha: &[u8]) -> Result<(), SinkError> {
        self.0 += 1;
        Ok(())
    }
}

#[test]
fn gpu_smart_filter_stack_is_refused_before_any_device_work() {
    let colour: Vec<u8> = (0..12).flat_map(|i| [0.25f32 + i as f32 / 48.0; 3].into_iter().flat_map(f32::to_le_bytes)).collect();
    let alpha: Vec<u8> = (0..12).flat_map(|_| 1.0f32.to_le_bytes()).collect();
    let tile = Tile { colour, alpha };
    let plan: RenderPlan<NoVector> = RenderPlan {
        revision: 5,
        target: OutputTarget { extent: EXTENT, format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight, space: tag() },
        nodes: vec![
            PlanNode::Solid { rect: EXTENT, rgba: [0.9, 0.7, 0.5, 1.0] },
            PlanNode::Tile(TileInput {
                key: SourceKey(1),
                placement: Rect { x: 2, y: 1, width: 4, height: 3 },
                format: SampleFormat::RgbF32Le,
                alpha: AlphaConvention::Straight,
                colour_sha256: hsk_studio_prism::profile_hash(&tile.colour),
                alpha_sha256: hsk_studio_prism::profile_hash(&tile.alpha),
            }),
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![
                    Layer { input: PlanNodeId(0), mode: StudioBlendMode::Normal, opacity: 1.0, fill_opacity: 1.0, mask: None },
                    Layer { input: PlanNodeId(1), mode: StudioBlendMode::Normal, opacity: 0.8, fill_opacity: 1.0, mask: None },
                ],
            }),
        ],
        root: PlanNodeId(2),
    };
    let op = SmartFilterOp {
        entries: vec![SmartFilterEntryOp { filter: StackFilter::Filter(LayerFilter::GaussianBlur { radius_px: 1.0 }), enabled: true, opacity: 1.0, mask: None }],
        stack_mask: None,
    };
    let ext = PlanExtensions::from_layers(vec![LayerExtension::new(PlanNodeId(2), 1).with_smart_filters(op)]);
    let backend = GpuBackend::new(DeviceOptions::default());
    assert_eq!(backend.extension_exactness(ExtOperator::SmartFilter), Exactness::Unsupported);
    let mut sink = Calls(0);
    let request = ExecuteRequest { expected_revision: 5, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 30 };
    let result = backend.execute_extended(&plan, &ext, &request, &tile, &CancellationToken::default(), &mut sink);
    assert_eq!(result.map(|_| ()), Err(BackendError::Unsupported { operator: SMART_FILTER_OPERATOR, exactness: Exactness::Unsupported }));
    assert_eq!(sink.0, 0);
}
