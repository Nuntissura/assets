//! Per-layer effect stacks on the CPU backend (lane SMB-AE-EFFECT-STACK-1; decision B1:
//! cross-crate render proofs live in renderer test files). Scenes come from
//! `hsk-studio-composite/tests/support/effect_scenes.rs`, where composite's
//! `tests/composition_effects.rs` pins the kernels and proves the Folio records lower to these
//! contents. The oracle of each frame is composite's `apply_effects` on the premultiplied source
//! (the stack runs on the host after the backend renders the layer), so a CPU frame equals it up
//! to the rounding of the final placement, and must be identical for every chunk size and thread
//! count.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::composition::{EFFECT_STACK_OPERATOR, SceneEffectEntry, apply_effects};
use hsk_studio_composite::{RenderedScene, SceneComposition, SceneContext, SceneLimits, render_scene};
use hsk_studio_pigment::render_plan::{Exactness, ExecuteRequest};
use hsk_studio_render_cpu::Renderer;

#[path = "../../hsk-studio-composite/tests/support/effect_scenes.rs"]
mod scenes;

const CPU_TOLERANCE: f32 = 1e-6;

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
fn oracle(effects: &[SceneEffectEntry]) -> Vec<[f32; 4]> {
    let mut b = scenes::premultiplied_source();
    apply_effects(scenes::W, scenes::H, &mut b, effects, &CancellationToken::default()).expect("stack runs");
    scenes::straight(&b)
}
fn f32_at(b: &[u8], i: usize) -> f32 {
    f32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

#[test]
fn effect_stacks_render_on_the_cpu_backend_like_the_host_kernels() {
    let pixels = scenes::Pixels::fixture();
    for (label, effects) in scenes::golden_cases() {
        let want = oracle(&effects);
        let scene = scenes::effected_scene(&pixels, effects, None);
        let base = render(&Renderer::new(64).unwrap(), &scene, &pixels);
        let mut delta = 0.0f32;
        for (i, w) in want.iter().enumerate() {
            let got = [f32_at(&base.colour, i * 12), f32_at(&base.colour, i * 12 + 4), f32_at(&base.colour, i * 12 + 8), f32_at(&base.alpha, i * 4)];
            for (g, x) in got.iter().zip(w) {
                delta = delta.max((g - x).abs());
            }
        }
        println!("{label}: max |delta| vs host kernels {delta:e}");
        assert!(delta <= CPU_TOLERANCE, "{label}: {delta}");
        for (chunk, threads) in [(5, 1), (7, 4), (4, 8)] {
            let other = render(&Renderer::with_threads(chunk, threads).unwrap(), &scene, &pixels);
            assert_eq!(other.digest(), base.digest(), "{label} chunk {chunk} threads {threads}");
        }
        assert!(base.exactness.operators.iter().any(|o| o.key == EFFECT_STACK_OPERATOR), "{label}");
        assert_eq!(base.exactness.lowest, Exactness::Approximate, "{label}");
    }
}
