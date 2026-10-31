//! Extended range on render-gpu, second lane (SMB-PIGMENT-EXTENDED-RANGE-2): binary32 overflow is
//! the typed refusal render-cpu returns (`Unsupported { sample_range_outside_unit }`, render-cpu
//! tests/cpu_extended_range.rs `hostile_values_in_an_extended_plan_are_typed`), never an infinite
//! pixel, also when the overflow happens inside a nested group and a darken above it would hide an
//! infinity; screen and exclusion run the Nuke float formulas within TOLERANCE of pigment; an
//! undefined envelope operation is refused per operation before device work.
//! The execution half follows `tests/gpu-proof.toml` (V4-03).
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over_extended};
use hsk_studio_pigment::filter_math::LayerFilter;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BlendSpace, ColourTag, Exactness, ExecuteRequest, Group, Layer,
    LayerExtension, NoVector, OutputStage, OutputTarget, PlanExtensions, PlanNode, PlanNodeId,
    PlanSources, RenderPlan, SAMPLE_RANGE_OPERATOR, SampleFormat, SampleRange, SinkError,
    SourceKey, StudioBlendMode, TilePlanes, TileSink,
};
use hsk_studio_pigment::Mask;
use hsk_studio_render_gpu::{AdmissionRequest, DeviceOptions, GpuBackend, GpuError, Reason, UnavailablePort};
use std::sync::Arc;

const TOLERANCE: f32 = 1e-6;
const R: Rect = Rect { x: 0, y: 0, width: 4, height: 2 };

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
struct Grid {
    px: Vec<(Rect, Vec<f32>)>,
}
impl TileSink for Grid {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let f = |b: &[u8], i: usize| f32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        let mut v = Vec::new();
        for i in 0..(r.width * r.height) as usize {
            v.extend([f(colour, i * 12), f(colour, i * 12 + 4), f(colour, i * 12 + 8), f(alpha, i * 4)]);
        }
        self.px.push((r, v));
        Ok(())
    }
}
fn layer(input: u32, mode: StudioBlendMode, opacity: f32) -> Layer {
    Layer { input: PlanNodeId(input), mode, opacity, fill_opacity: 1.0, mask: None }
}
fn plan(nodes: Vec<PlanNode<NoVector>>) -> RenderPlan<NoVector> {
    let root = PlanNodeId(nodes.len() as u32 - 1);
    RenderPlan {
        revision: 3,
        target: OutputTarget {
            extent: R,
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space: ColourTag { profile_sha256: [5; 32], blend_space: BlendSpace::LinearLight },
        },
        nodes,
        root,
    }
}
fn solid(rgba: [f32; 4]) -> PlanNode<NoVector> {
    PlanNode::Solid { rect: R, rgba }
}
fn group(layers: Vec<Layer>) -> PlanNode<NoVector> {
    PlanNode::Group(Group { isolated: true, layers })
}
fn extended() -> PlanExtensions {
    PlanExtensions::default().with_output(OutputStage::new(SampleRange::Extended, None))
}
fn request() -> ExecuteRequest {
    ExecuteRequest { expected_revision: 3, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 }
}
fn strict() -> bool {
    include_str!("gpu-proof.toml").lines().any(|l| l.trim() == "require_gpu = true")
        || std::env::var("HSK_STUDIO_TEST_REQUIRE_GPU").as_deref() == Ok("1")
}
fn overflow_error() -> GpuError {
    GpuError::Backend(BackendError::Unsupported { operator: SAMPLE_RANGE_OPERATOR, exactness: Exactness::Unsupported })
}

/// none backend: a high pass in an extended plan is refused `sample_range_extended_filter_high_pass`
/// (undefined above 1, refused by pigment for every backend). Live adapter, chunk sides 2 and
/// 64: 3e38 + 3e38 under linear dodge is `Unsupported { sample_range_outside_unit }`, also when
/// the sum lies in a nested group that a darken layer of value 1 composites over it; screen of 2.5
/// over 3.0 (= max, 3.0), screen of 0.5 over 3.0 (= 2.0) and exclusion of 2.0 over 3.0 (= -7.0)
/// match pigment within TOLERANCE.
#[test]
fn gpu_overflow_is_the_cpu_refusal_and_nuke_modes_match_pigment() {
    let cancel = CancellationToken::default();
    let blurred = PlanExtensions::from_layers(vec![LayerExtension::new(PlanNodeId(2), 1).with_filters(vec![LayerFilter::HighPass { radius_px: 1.0 }])])
        .with_output(OutputStage::new(SampleRange::Extended, None));
    let simple = plan(vec![solid([2.0, 1.0, 1.0, 1.0]), solid([0.5, 0.5, 0.5, 1.0]), group(vec![layer(0, StudioBlendMode::Normal, 1.0), layer(1, StudioBlendMode::Normal, 1.0)])]);
    let none = GpuBackend::with_grant(Arc::new(UnavailablePort::new()), AdmissionRequest::asset_local(), DeviceOptions::default());
    let mut grid = Grid::default();
    assert_eq!(
        none.execute_extended_detailed(&simple, &blurred, &request(), &NoSources, &cancel, &mut grid).unwrap_err(),
        GpuError::Backend(BackendError::Unsupported { operator: "sample_range_extended_filter_high_pass", exactness: Exactness::Unsupported })
    );
    assert!(grid.px.is_empty());

    let sum = plan(vec![
        solid([3.0e38, 1.0, 1.0, 1.0]),
        solid([3.0e38, 0.5, 0.5, 1.0]),
        group(vec![layer(0, StudioBlendMode::Normal, 1.0), layer(1, StudioBlendMode::LinearDodgeAdd, 1.0)]),
    ]);
    let nested = plan(vec![
        solid([3.0e38, 1.0, 1.0, 1.0]),
        solid([3.0e38, 0.5, 0.5, 1.0]),
        group(vec![layer(0, StudioBlendMode::Normal, 1.0), layer(1, StudioBlendMode::LinearDodgeAdd, 1.0)]),
        solid([1.0, 1.0, 1.0, 1.0]),
        group(vec![layer(2, StudioBlendMode::Normal, 1.0), layer(3, StudioBlendMode::Darken, 1.0)]),
    ]);
    let modes = |mode: StudioBlendMode, b: [f32; 3], s: [f32; 3]| {
        plan(vec![solid([b[0], b[1], b[2], 1.0]), solid([s[0], s[1], s[2], 0.75]), group(vec![layer(0, StudioBlendMode::Normal, 1.0), layer(1, mode, 0.8)])])
    };
    let mut live = 0;
    for chunk_side in [2u32, 64] {
        let gpu = GpuBackend::new(DeviceOptions { chunk_side, ..DeviceOptions::default() });
        if gpu.device_info().is_none() {
            if let Some(failure) = gpu.failure() {
                panic!("adapter present but the backend failed: {failure:?}");
            }
            match gpu.unavailable_reason() {
                Some(Reason::NoAdapter) if strict() => panic!("NOT_PROVEN: no adapter; gpu overflow refusal requires a GPU (tests/gpu-proof.toml)"),
                Some(Reason::NoAdapter) => {
                    eprintln!("NOT_PROVEN: no adapter; gpu overflow refusal not run");
                    return;
                }
                other => panic!("no live device ({other:?})"),
            }
        }
        for p in [&sum, &nested] {
            let mut grid = Grid::default();
            assert_eq!(gpu.execute_extended_detailed(p, &extended(), &request(), &NoSources, &cancel, &mut grid).unwrap_err(), overflow_error());
        }
        for (mode, b, s) in [
            (StudioBlendMode::Screen, [3.0, 3.0, 0.2], [2.5, 0.5, 0.6]),
            (StudioBlendMode::Exclusion, [3.0, 0.5, 4.0], [2.0, 0.5, 1.5]),
        ] {
            let mut grid = Grid::default();
            gpu.execute_extended_detailed(&modes(mode, b, s), &extended(), &request(), &NoSources, &cancel, &mut grid).unwrap();
            let want = composite_over_extended(mode, Rgba::new(b, 1.0), Rgba::new(s, 0.75), 0.8, PixelSite::default()).unwrap();
            let mut max = 0f32;
            for (_, v) in &grid.px {
                for px in v.chunks_exact(4) {
                    for (got, w) in px.iter().zip(want.rgb) {
                        max = max.max((got - w).abs() / w.abs().max(1.0));
                    }
                    max = max.max((px[3] - want.a).abs());
                }
            }
            println!("chunk {chunk_side} {mode:?}: max relative |delta| vs pigment = {max:e} (tolerance {TOLERANCE:e}); pigment {:?}", want.rgb);
            assert!(max <= TOLERANCE, "{mode:?}: {max:e}");
        }
        live += 1;
    }
    assert_eq!(live, 2);
}
