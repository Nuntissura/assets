//! Frame blending and roto-matte contents on the GPU backend (lane SMB-AE-ROTO-WIRING; decision
//! B1). The same scenes and oracles as render-cpu `tests/composition_roto_frames.rs`
//! (`hsk-studio-composite/tests/support/roto_scenes.rs`): the GPU renders the two source frames
//! (or the matted content), the host step is shared, so CPU/GPU parity is both backends within
//! their tolerance of one oracle. GPU-execution proofs follow `tests/gpu-proof.toml` (V4-03):
//! without an adapter they FAIL with `NOT_PROVEN: no adapter` while `require_gpu = true`.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::composition::FrameBlendMethod;
use hsk_studio_composite::{SceneContext, SceneLimits, render_scene};
use hsk_studio_pigment::frame_blend::{BlendLimits, FlowParams, frame_mix, pixel_motion};
use hsk_studio_pigment::render_plan::{Exactness, ExecuteRequest};
use hsk_studio_render_gpu::{DeviceOptions, GpuBackend, Reason};
use std::sync::OnceLock;

#[path = "../../hsk-studio-composite/tests/support/roto_scenes.rs"]
mod scenes;

/// Established render-gpu tolerance against pigment's reference (binary32 kernels).
const GPU_TOLERANCE: f32 = 2e-6;
/// Pixel motion estimates flow on the GPU-rendered frames, so the frame-render rounding passes
/// through the flow solve (Studio tolerance for this content).
const GPU_PIXEL_MOTION_TOLERANCE: f32 = 1e-5;

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
fn gpu_roto_and_frame_blend_frames_match_the_shared_oracle() {
    if !gate("gpu_roto_and_frame_blend_frames_match_the_shared_oracle") {
        return;
    }
    let pixels = scenes::Pixels::fixture();
    let cancel = CancellationToken::default();
    let ctx = SceneContext {
        backend: backend(),
        pixels: &pixels,
        request: ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 },
        space: scenes::space(),
        limits: SceneLimits::default(),
    };
    let mut cases = Vec::new();
    for (a, b, w) in [(0u64, 1u64, 0.5f64), (2, 3, 0.25)] {
        cases.push((
            format!("frame_mix/{a}-{b}@{w}"),
            scenes::blended_scene(a, b, w, FrameBlendMethod::FrameMix, &pixels),
            frame_mix(&scenes::nested_plane(a), &scenes::nested_plane(b), w, &BlendLimits::DEFAULT).unwrap(),
            GPU_TOLERANCE,
        ));
        cases.push((
            format!("pixel_motion/{a}-{b}@{w}"),
            scenes::blended_scene(a, b, w, FrameBlendMethod::PixelMotion, &pixels),
            pixel_motion(&scenes::nested_plane(a), &scenes::nested_plane(b), w, &FlowParams::DEFAULT, &BlendLimits::DEFAULT, &cancel).unwrap(),
            GPU_PIXEL_MOTION_TOLERANCE,
        ));
    }
    for k in [0u64, 3, 7] {
        let m = scenes::disc_matte(k);
        cases.push((format!("roto/{k}"), scenes::roto_scene(k, m.clone(), &pixels), scenes::roto_oracle(k, &m), GPU_TOLERANCE));
    }
    for (label, scene, want, tolerance) in cases {
        let got = render_scene(&scene, &ctx, &cancel).expect("frame renders");
        let delta = scenes::max_delta(&got, &want);
        println!("{label}: gpu digest {} max |delta| vs oracle {delta:e}", scenes::hex(&got.digest()));
        assert!(delta <= tolerance, "{label}: {delta}");
        let again = render_scene(&scene, &ctx, &cancel).expect("frame renders");
        assert_eq!(again.digest(), got.digest(), "{label}: GPU frames are repeatable");
    }
}
