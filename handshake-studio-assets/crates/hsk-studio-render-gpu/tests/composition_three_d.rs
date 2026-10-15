//! A 3D run on the GPU backend (lane SMB-AE-3D): the same rank-matted projective layers and the same
//! independent ray-cast oracle as render-cpu `tests/composition_three_d.rs`, within the established
//! render-gpu tolerance. GPU-execution proofs follow `tests/gpu-proof.toml` (V4-03).
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::{SceneContext, SceneLimits, render_scene};
use hsk_studio_pigment::render_plan::{Exactness, ExecuteRequest};
use hsk_studio_render_gpu::{DeviceOptions, GpuBackend, Reason};
use std::sync::OnceLock;

#[path = "../../hsk-studio-composite/tests/support/composition_scenes.rs"]
mod scenes;
#[path = "../../hsk-studio-composite/tests/support/three_d_scene.rs"]
mod three_d;

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
fn gpu_three_d_run_matches_the_ray_cast_oracle() {
    if !gate("gpu_three_d_run_matches_the_ray_cast_oracle") {
        return;
    }
    let pixels = scenes::Pixels::fixture();
    let scene = three_d::three_d_scene(&pixels);
    let (want, crossing) = three_d::three_d_oracle(&pixels);
    assert!(crossing[0] > 0 && crossing[1] > 0, "{crossing:?}");
    let ctx = SceneContext {
        backend: backend(),
        pixels: &pixels,
        request: ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 },
        space: scenes::space(),
        limits: SceneLimits::default(),
    };
    let got = render_scene(&scene, &ctx, &CancellationToken::default()).expect("frame renders");
    let delta = scenes::max_delta(&got, &want);
    println!("three_d: gpu max |delta| vs oracle {delta:e}");
    assert!(delta <= GPU_TOLERANCE, "{delta}");
}

#[test]
fn gpu_lit_three_d_run_matches_the_shading_oracle() {
    if !gate("gpu_lit_three_d_run_matches_the_shading_oracle") {
        return;
    }
    let pixels = scenes::Pixels::fixture();
    let scene = three_d::lit_scene(&pixels);
    let (want, shadowed) = three_d::lit_oracle(&pixels);
    assert!(shadowed > 0);
    let ctx = SceneContext {
        backend: backend(),
        pixels: &pixels,
        request: ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 },
        space: scenes::space(),
        limits: SceneLimits::default(),
    };
    let got = render_scene(&scene, &ctx, &CancellationToken::default()).expect("frame renders");
    let delta = scenes::max_delta(&got, &want);
    println!("three_d lit: gpu max |delta| vs oracle {delta:e}");
    assert!(delta <= GPU_TOLERANCE, "{delta}");
}

#[test]
fn gpu_depth_of_field_matches_the_defocus_oracle() {
    if !gate("gpu_depth_of_field_matches_the_defocus_oracle") {
        return;
    }
    let pixels = scenes::Pixels::fixture();
    let scene = three_d::dof_scene(&pixels);
    let (want, _) = three_d::dof_oracle(&pixels);
    let ctx = SceneContext {
        backend: backend(),
        pixels: &pixels,
        request: ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 },
        space: scenes::space(),
        limits: SceneLimits::default(),
    };
    let got = render_scene(&scene, &ctx, &CancellationToken::default()).expect("frame renders");
    let delta = scenes::max_delta(&got, &want);
    println!("three_d dof: gpu max |delta| vs oracle {delta:e}");
    assert!(delta <= GPU_TOLERANCE, "{delta}");
}

#[test]
fn gpu_parented_three_d_run_matches_the_ray_cast_oracle() {
    // Lane SMB-AE-3D-2: the parented run of render-cpu's test against the same oracle.
    if !gate("gpu_parented_three_d_run_matches_the_ray_cast_oracle") {
        return;
    }
    let pixels = scenes::Pixels::fixture();
    let scene = three_d::parented_scene(&pixels);
    let (want, _) = three_d::parented_oracle(&pixels);
    let ctx = SceneContext {
        backend: backend(),
        pixels: &pixels,
        request: ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 },
        space: scenes::space(),
        limits: SceneLimits::default(),
    };
    let got = render_scene(&scene, &ctx, &CancellationToken::default()).expect("frame renders");
    let delta = scenes::max_delta(&got, &want);
    println!("three_d parented: gpu max |delta| vs oracle {delta:e}");
    assert!(delta <= GPU_TOLERANCE, "{delta}");
}

#[test]
fn gpu_masked_and_matted_three_d_run_matches_the_oracle() {
    // Lane SMB-AE-3D-2: render-cpu's masked / matted run against the same oracle.
    if !gate("gpu_masked_and_matted_three_d_run_matches_the_oracle") {
        return;
    }
    let pixels = scenes::Pixels::fixture();
    let scene = three_d::masked_scene(&pixels);
    let want = three_d::masked_oracle(&pixels);
    let ctx = SceneContext {
        backend: backend(),
        pixels: &pixels,
        request: ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 },
        space: scenes::space(),
        limits: SceneLimits::default(),
    };
    let got = render_scene(&scene, &ctx, &CancellationToken::default()).expect("frame renders");
    let delta = scenes::max_delta(&got, &want);
    println!("three_d masked: gpu max |delta| vs oracle {delta:e}");
    assert!(delta <= GPU_TOLERANCE, "{delta}");
}

#[test]
fn gpu_equirectangular_plane_to_sphere_matches_the_oracle() {
    // Lane SMB-AE-3D-2: the plane contents come from the GPU backend; the spherical ray cast and
    // the composite run in composite, so the frame stays within the render-gpu tolerance.
    if !gate("gpu_equirectangular_plane_to_sphere_matches_the_oracle") {
        return;
    }
    let pixels = scenes::Pixels::fixture();
    let ctx = SceneContext {
        backend: backend(),
        pixels: &pixels,
        request: ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 },
        space: scenes::space(),
        limits: SceneLimits::default(),
    };
    let frame = hsk_studio_composite::composition::render_equirect(
        &three_d::immersive_scene(&pixels),
        hsk_studio_pigment::transform_math::ResampleFilter::Bilinear,
        &ctx,
        &CancellationToken::default(),
    )
    .expect("frame");
    let delta = three_d::max_delta_px(&three_d::surface_pixels(&frame), &three_d::immersive_oracle(&pixels));
    println!("three_d equirect: gpu max |delta| vs oracle {delta:e}");
    assert!(delta <= GPU_TOLERANCE, "{delta}");
}
