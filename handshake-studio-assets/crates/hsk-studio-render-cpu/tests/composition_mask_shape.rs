//! Shaped mask paths on the CPU backend (lane SMB-AE-MASK-FEATHER-1): a layer under a feathered
//! `add` mask and an expanded `subtract` mask at opacity 0.75 (the composition-space matte of
//! composite `mask_paths::mask_coverage_shaped`) renders the coverage oracle of
//! `hsk-studio-composite/tests/support/mask_shape_scene.rs`, identically for every chunking and
//! thread count.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::{RenderedScene, SceneComposition, SceneContext, SceneLimits, render_scene};
use hsk_studio_pigment::render_plan::{Exactness, ExecuteRequest};
use hsk_studio_render_cpu::Renderer;

#[path = "../../hsk-studio-composite/tests/support/composition_scenes.rs"]
mod scenes;
#[path = "../../hsk-studio-composite/tests/support/mask_shape_scene.rs"]
mod shaped;

fn render(renderer: &Renderer, scene: &SceneComposition, pixels: &scenes::Pixels) -> RenderedScene {
    let ctx = SceneContext {
        backend: renderer,
        pixels,
        request: ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 },
        space: scenes::space(),
        limits: SceneLimits::default(),
    };
    render_scene(scene, &ctx, &CancellationToken::default()).expect("frame renders")
}

#[test]
fn shaped_masks_match_the_coverage_oracle_on_every_chunking() {
    let pixels = scenes::Pixels::fixture();
    let scene = shaped::mask_shape_scene(&pixels);
    let want = shaped::mask_shape_oracle(&pixels);
    // The fixture exercises fractional coverage (feather ramp and the 0.75 subtract).
    let cov = shaped::shaped_coverage();
    assert!(cov.iter().filter(|c| **c > 0.0 && **c < 1.0).count() > 50, "fractional coverage present");
    let base = render(&Renderer::new(64).unwrap(), &scene, &pixels);
    let delta = scenes::max_delta(&base, &want);
    println!("shaped masks: cpu max |delta| vs coverage oracle {delta:e}");
    assert!(delta <= 1e-6, "{delta}");
    for (chunk, threads) in [(5, 1), (7, 4), (4, 8), (64, 2)] {
        assert_eq!(render(&Renderer::with_threads(chunk, threads).unwrap(), &scene, &pixels).digest(), base.digest(), "chunk {chunk} threads {threads}");
    }
    println!("NOT_PROVEN: GPU render of shaped masks (render-gpu not run by this lane; both backends consume the same coverage_u16le matte plane)");
    println!("NOT_PROVEN: After Effects mask feather / opacity / expansion pixel parity (no D5 oracle case for After Effects masks)");
}
