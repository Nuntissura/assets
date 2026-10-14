//! Composition frames on the GPU backend (lane SMB-COMPOSITE-COMPOSITION-ADAPTER; decision B1).
//! The same scenes and oracle as render-cpu `tests/composition_frames.rs`
//! (`hsk-studio-composite/tests/support/composition_scenes.rs`): CPU/GPU parity is both backends
//! within their tolerance of one oracle. GPU-execution proofs follow `tests/gpu-proof.toml`
//! (V4-03): without an adapter they FAIL with `NOT_PROVEN: no adapter` while `require_gpu = true`.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::{SceneContext, SceneLimits, render_scene};
use hsk_studio_pigment::render_plan::{Exactness, ExecuteRequest};
use hsk_studio_render_gpu::{DeviceOptions, GpuBackend, Reason};
use std::sync::OnceLock;

#[path = "../../hsk-studio-composite/tests/support/composition_scenes.rs"]
mod scenes;

/// Established render-gpu tolerance against pigment's reference (2e-6, blend and resample
/// kernels in binary32), applied to alpha and premultiplied colour.
const GPU_TOLERANCE: f32 = 2e-6;

fn backend() -> &'static GpuBackend {
    static SHARED: OnceLock<GpuBackend> = OnceLock::new();
    SHARED.get_or_init(|| {
        GpuBackend::new(DeviceOptions {
            chunk_side: 16,
            ..DeviceOptions::default()
        })
    })
}
fn gate(what: &str) -> bool {
    let b = backend();
    if b.device_info().is_some() {
        return true;
    }
    if let Some(failure) = b.failure() {
        panic!("{what}: adapter present but the backend failed: {failure:?}");
    }
    let strict = include_str!("gpu-proof.toml")
        .lines()
        .any(|l| l.trim() == "require_gpu = true")
        || std::env::var("HSK_STUDIO_TEST_REQUIRE_GPU").as_deref() == Ok("1");
    match b.unavailable_reason() {
        Some(Reason::NoAdapter) if strict => {
            panic!("NOT_PROVEN: no adapter; {what} requires a GPU (tests/gpu-proof.toml)")
        }
        Some(Reason::NoAdapter) => {
            eprintln!("NOT_PROVEN: no adapter; {what} not run");
            false
        }
        other => panic!("{what}: no live device ({other:?})"),
    }
}

#[test]
fn gpu_composition_frames_match_the_shared_oracle() {
    if !gate("gpu_composition_frames_match_the_shared_oracle") {
        return;
    }
    let pixels = scenes::Pixels::fixture();
    let ctx = SceneContext {
        backend: backend(),
        pixels: &pixels,
        request: ExecuteRequest {
            expected_revision: 7,
            min_exactness: Exactness::Approximate,
            deadline: None,
            max_working_bytes: 1 << 28,
        },
        space: scenes::space(),
        limits: SceneLimits::default(),
    };
    for frame in scenes::FRAMES {
        let scene = scenes::expected_scene(frame, &pixels);
        let want = scenes::oracle(&scene, &pixels);
        let got = render_scene(&scene, &ctx, &CancellationToken::default()).expect("frame renders");
        let delta = scenes::max_delta(&got, &want);
        println!("frame {frame}: gpu digest {} max |delta| vs oracle {delta:e}", scenes::hex(&got.digest()));
        assert!(delta <= GPU_TOLERANCE, "frame {frame}: {delta}");
        let again = render_scene(&scene, &ctx, &CancellationToken::default()).expect("frame renders");
        assert_eq!(again.digest(), got.digest(), "frame {frame}: GPU frames are repeatable");
    }
}
