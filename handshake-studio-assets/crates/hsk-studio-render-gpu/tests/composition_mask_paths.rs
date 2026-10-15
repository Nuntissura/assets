//! Mask paths on the GPU backend (lane SMB-AE-KEY-TRACK-3): the same scene
//! and oracle as render-cpu `tests/composition_mask_paths.rs`, within the established render-gpu
//! tolerance. GPU-execution proofs follow `tests/gpu-proof.toml` (V4-03).
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::{SceneContext, SceneLimits, render_scene};
use hsk_studio_pigment::render_plan::{Exactness, ExecuteRequest};
use hsk_studio_render_gpu::{DeviceOptions, GpuBackend, Reason};
use std::sync::OnceLock;

#[path = "../../hsk-studio-composite/tests/support/composition_scenes.rs"]
mod scenes;
#[path = "../../hsk-studio-composite/tests/support/masked_scene.rs"]
mod masked;

/// Established render-gpu tolerance against pigment's reference (2e-6), on alpha and
/// premultiplied colour.
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

#[test]
fn gpu_masked_layer_matches_the_oracle() {
    if !gate("gpu_masked_layer_matches_the_oracle") {
        return;
    }
    let pixels = scenes::Pixels::fixture();
    let scene = masked::masked_scene(&pixels);
    let want = masked::masked_oracle(&pixels);
    let ctx = SceneContext {
        backend: backend(),
        pixels: &pixels,
        request: ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 },
        space: scenes::space(),
        limits: SceneLimits::default(),
    };
    let got = render_scene(&scene, &ctx, &CancellationToken::default()).expect("frame renders");
    let delta = scenes::max_delta(&got, &want);
    println!("masked: gpu max |delta| vs oracle {delta:e}");
    assert!(delta <= GPU_TOLERANCE, "{delta}");
}
