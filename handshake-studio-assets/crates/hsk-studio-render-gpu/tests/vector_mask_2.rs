//! Lane SMB-COMPOSITE-VECTOR-MASK-2 on the GPU backend (decision B1): the cases and the oracle of
//! render-cpu `tests/vector_mask_2.rs` (`hsk-studio-composite/tests/support/mask_scenes.rs`
//! `lane2_cases`); CPU/GPU parity = both within tolerance of one oracle. Gated by
//! `tests/gpu-proof.toml` (V4-03).
use hsk_studio_render_gpu::{DeviceOptions, GpuBackend, Reason};
use std::sync::OnceLock;

#[path = "../../hsk-studio-composite/tests/support/mask_scenes.rs"]
mod masks;

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
fn gpu_mask_parameters_rendered_sources_pass_through_isolation_and_unlinked_mattes() {
    if !gate("gpu_mask_parameters_rendered_sources_pass_through_isolation_and_unlinked_mattes") {
        return;
    }
    let cases = masks::lane2_cases(backend());
    assert_eq!(cases.len(), 5);
    for (case, delta) in cases {
        println!("{case}: gpu max |delta| vs oracle {delta:e}");
        assert!(delta <= GPU_TOLERANCE, "{case}: {delta}");
    }
}
