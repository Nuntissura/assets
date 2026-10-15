//! Lane SMB-COMPOSITE-DOCGRAPH-MASKS on the GPU backend (decision B1): the document-graph frame and
//! the oracle of render-cpu `tests/composition_docgraph_masks.rs`
//! (`hsk-studio-composite/tests/support/docgraph_scenes.rs`: vector clip, inverted VECTOR mask with
//! density and feather, a clipped layer over that masked base, an unlinked ALPHA mask, a LUMINANCE
//! mask with reveal_outside); CPU/GPU parity = both within tolerance of one oracle. The scene path
//! of the same composition is rendered on the GPU too and compared with the document-graph frame.
//! Gated by `tests/gpu-proof.toml` (V4-03).
use hsk_studio_render_gpu::{DeviceOptions, GpuBackend, Reason};
use std::sync::OnceLock;

#[path = "../../hsk-studio-composite/tests/support/docgraph_scenes.rs"]
mod fixture;

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
fn gpu_document_graph_frame_with_vector_clips_and_masks_matches_the_oracle() {
    if !gate("gpu_document_graph_frame_with_vector_clips_and_masks_matches_the_oracle") {
        return;
    }
    let pixels = fixture::scenes::Pixels::fixture();
    let frame = fixture::document_frame(&pixels);
    let oracle = fixture::oracle(&pixels);
    let got = fixture::render_frame(backend(), &frame, &pixels).expect("frame renders");
    let delta = fixture::max_delta(&got, &oracle);
    println!("gpu document-graph frame: max |delta| vs oracle {delta:e}");
    assert!(delta <= GPU_TOLERANCE, "document graph vs oracle: {delta}");
    let scene = fixture::render_scene_px(backend(), &fixture::scene(&pixels), &pixels);
    let between = fixture::max_delta(&scene, &got);
    println!("gpu scene path vs document graph: max |delta| {between:e}");
    assert!(between <= GPU_TOLERANCE, "scene path vs document graph: {between}");
}

/// Lane SMB-COMPOSITE-DOCGRAPH-2 goal 2 on the GPU: the moved variant's document-graph frame
/// (`fixture::moved_frame`: bilinear tile transforms, a vector clip and a linked LUMINANCE mask
/// resampled with their content, an unlinked ALPHA mask in place, a Folio clipped layer rotated
/// over its masked base) within the GPU tolerance of `fixture::moved_oracle`, which render-cpu
/// matches bit for bit.
#[test]
fn gpu_moved_document_graph_frame_matches_the_oracle() {
    if !gate("gpu_moved_document_graph_frame_matches_the_oracle") {
        return;
    }
    let pixels = fixture::scenes::Pixels::fixture();
    let frame = fixture::moved_frame(&pixels);
    let oracle = fixture::moved_oracle(&pixels);
    let got = fixture::render_frame(backend(), &frame, &pixels).expect("frame renders");
    let delta = fixture::max_delta(&got, &oracle);
    println!("gpu moved frame: max |delta| vs oracle {delta:e}");
    assert!(delta <= GPU_TOLERANCE, "moved document graph vs oracle: {delta}");
}
