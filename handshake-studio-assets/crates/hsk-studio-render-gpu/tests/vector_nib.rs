//! nib-drawn vector layer on the GPU backend (lane SMB-COMPOSITE-VECTOR-IN-GROUP slice 2; decision
//! B1): the same layer and oracle as render-cpu `tests/vector_nib.rs`
//! (`hsk-studio-composite/tests/support/vector_scenes.rs`); CPU/GPU parity = both within tolerance
//! of one oracle. Gated by `tests/gpu-proof.toml` (V4-03).
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::{VectorCoverage, VectorResolution, prepare_vector_layers, vector_layer_render_plan};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, Exactness, ExecuteRequest, OutputTarget, RenderBackend, SampleFormat, SinkError, TileSink,
};
use hsk_studio_render_gpu::{DeviceOptions, GpuBackend, Reason};
use std::sync::OnceLock;

#[path = "../../hsk-studio-composite/tests/support/vector_scenes.rs"]
mod scenes;

const GPU_TOLERANCE: f32 = 2e-6;

fn backend() -> &'static GpuBackend {
    static SHARED: OnceLock<GpuBackend> = OnceLock::new();
    SHARED.get_or_init(|| GpuBackend::new(DeviceOptions { chunk_side: 16, ..DeviceOptions::default() }))
}
fn gate(what: &str) -> bool {
    let b = backend();
    if b.device_info().is_some() {
        return true;
    }
    if let Some(failure) = b.failure() {
        panic!("{what}: adapter present but the backend failed: {failure:?}");
    }
    let strict = include_str!("gpu-proof.toml").lines().any(|l| l.trim() == "require_gpu = true")
        || std::env::var("HSK_STUDIO_TEST_REQUIRE_GPU").as_deref() == Ok("1");
    match b.unavailable_reason() {
        Some(Reason::NoAdapter) if strict => panic!("NOT_PROVEN: no adapter; {what} requires a GPU (tests/gpu-proof.toml)"),
        Some(Reason::NoAdapter) => {
            eprintln!("NOT_PROVEN: no adapter; {what} not run");
            false
        }
        other => panic!("{what}: no live device ({other:?})"),
    }
}

struct Frame(Vec<[f32; 4]>);
impl TileSink for Frame {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let f = |b: &[u8], i: usize| f32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        for row in 0..r.height as usize {
            for col in 0..r.width as usize {
                let i = row * r.width as usize + col;
                let at = (r.y as usize + row) * scenes::W as usize + r.x as usize + col;
                self.0[at] = [f(colour, i * 12), f(colour, i * 12 + 4), f(colour, i * 12 + 8), f(alpha, i * 4)];
            }
        }
        Ok(())
    }
}

#[test]
fn gpu_nib_vector_layer_matches_the_shared_oracle() {
    if !gate("gpu_nib_vector_layer_matches_the_shared_oracle") {
        return;
    }
    let cancel = CancellationToken::default();
    let res = VectorResolution { coverage: VectorCoverage::Exact, ..VectorResolution::default() };
    let vectors = prepare_vector_layers(&[scenes::spec()], &res, &cancel).unwrap();
    let plane = vectors.plane(scenes::LAYER).unwrap();
    let target = OutputTarget {
        extent: Rect { x: 0, y: 0, width: scenes::W, height: scenes::H },
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        space: scenes::space(),
    };
    let low = vector_layer_render_plan(&vectors, scenes::LAYER, 7, target).unwrap();
    let sources = scenes::Sources { low: &low, vectors: &vectors };
    let request = ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 };
    let mut frame = Frame(vec![[0.0; 4]; (scenes::W * scenes::H) as usize]);
    backend().execute_extended(&low.plan, &low.extensions, &request, &sources, &cancel, &mut frame).expect("renders");
    let delta = scenes::max_delta(&frame.0, &scenes::oracle(plane));
    println!("gpu max |delta| vs oracle {delta:e}");
    assert!(delta <= GPU_TOLERANCE, "{delta}");
}

#[test]
fn gpu_clipped_nib_vector_layer_matches_the_shared_oracle() {
    if !gate("gpu_clipped_nib_vector_layer_matches_the_shared_oracle") {
        return;
    }
    let cancel = CancellationToken::default();
    let res = VectorResolution { coverage: VectorCoverage::Exact, ..VectorResolution::default() };
    let vectors = prepare_vector_layers(&[scenes::spec()], &res, &cancel)
        .unwrap()
        .with_clip(scenes::LAYER, Some(scenes::clip_outline()));
    let plane = vectors.plane(scenes::LAYER).unwrap();
    let target = OutputTarget {
        extent: Rect { x: 0, y: 0, width: scenes::W, height: scenes::H },
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        space: scenes::space(),
    };
    let low = vector_layer_render_plan(&vectors, scenes::LAYER, 7, target).unwrap();
    let [(_, clip)] = low.clip_planes.as_slice() else { panic!("one clip plane") };
    let sources = scenes::Sources { low: &low, vectors: &vectors };
    let request = ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 };
    let mut frame = Frame(vec![[0.0; 4]; (scenes::W * scenes::H) as usize]);
    backend().execute_extended(&low.plan, &low.extensions, &request, &sources, &cancel, &mut frame).expect("renders");
    let delta = scenes::max_delta(&frame.0, &scenes::oracle_clipped(plane, clip));
    println!("clipped gpu max |delta| vs oracle {delta:e}");
    assert!(delta <= GPU_TOLERANCE, "{delta}");
}
