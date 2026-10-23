//! Mask paths on the CPU backend (lane SMB-AE-KEY-TRACK-3): a layer cut by an `add` and a
//! `subtract` mask path (the composition-space matte of composite `mask_paths`) renders the oracle
//! of `hsk-studio-composite/tests/support/masked_scene.rs`, identically for every chunking.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::{RenderedScene, SceneComposition, SceneContext, SceneLimits, render_scene};
use hsk_studio_pigment::render_plan::{Exactness, ExecuteRequest};
use hsk_studio_render_cpu::Renderer;

#[path = "../../hsk-studio-composite/tests/support/composition_scenes.rs"]
mod scenes;
#[path = "../../hsk-studio-composite/tests/support/masked_scene.rs"]
mod masked;

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
fn masked_layer_matches_the_oracle_on_every_chunking() {
    let pixels = scenes::Pixels::fixture();
    let scene = masked::masked_scene(&pixels);
    let want = masked::masked_oracle(&pixels);
    let base = render(&Renderer::new(64).unwrap(), &scene, &pixels);
    let delta = scenes::max_delta(&base, &want);
    println!("masked: cpu max |delta| vs oracle {delta:e}");
    assert!(delta <= 1e-6, "{delta}");
    for (chunk, threads) in [(5, 1), (7, 4), (64, 2)] {
        assert_eq!(render(&Renderer::with_threads(chunk, threads).unwrap(), &scene, &pixels).digest(), base.digest());
    }
}
