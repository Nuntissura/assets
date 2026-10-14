//! Composition frames on the CPU backend (lane SMB-COMPOSITE-COMPOSITION-ADAPTER; decision B1:
//! cross-crate render proofs live in renderer test files). The scenes come from
//! `hsk-studio-composite/tests/support/composition_scenes.rs`, where composite's
//! `tests/composition_adapter.rs` proves the stored Folio composition evaluates to them. Each
//! frame is rendered by `composite::render_scene` through this backend and compared, pixel by
//! pixel, with the shared oracle (pigment reference sampler + `composite_over`); the frame digest
//! must be identical for every chunk size and thread count. render-gpu's
//! `tests/composition_frames.rs` compares the same frames with the same oracle (CPU/GPU parity).
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::{SceneContext, SceneLimits, render_scene};
use hsk_studio_pigment::render_plan::{Exactness, ExecuteRequest};
use hsk_studio_render_cpu::Renderer;

#[path = "../../hsk-studio-composite/tests/support/composition_scenes.rs"]
mod scenes;

/// The CPU executes pigment's binary64 sampler and binary32 blend functions; the oracle calls the
/// same functions per pixel, so the frames agree to float rounding of the composition order.
const CPU_TOLERANCE: f32 = 1e-6;
/// Frame digests (colour plane then alpha plane, SHA-256) of `scenes::FRAMES`, recorded at the
/// slice 1 commit; any change to the frames' pixels shows here.
const GOLDEN: [&str; 4] = [
    "80952af10847e9206fc169b6d71ba416136135d5a6c92fd6df58ef22a695f1c3",
    "17c0727e2320ba849bd7b70c829b62ce2cf7ffa080797d9d78ab32e1da2b41e6",
    "b88a69fc89e656aa9c1b4f2732cf17ed35ff7d2586822cf08288e7c4f083d5c5",
    "01cec491147e59ed5928d24858e45b33510ed28d7d91ca827afd01a02758ff64",
];

fn render(renderer: &Renderer, scene: &hsk_studio_composite::SceneComposition, pixels: &scenes::Pixels) -> hsk_studio_composite::RenderedScene {
    let ctx = SceneContext {
        backend: renderer,
        pixels,
        request: ExecuteRequest {
            expected_revision: 7,
            min_exactness: Exactness::Approximate,
            deadline: None,
            max_working_bytes: 1 << 28,
        },
        space: scenes::space(),
        limits: SceneLimits::default(),
    };
    render_scene(scene, &ctx, &CancellationToken::default()).expect("frame renders")
}

#[test]
fn composition_frames_match_the_oracle_on_every_chunking_and_thread_count() {
    let pixels = scenes::Pixels::fixture();
    for (frame, golden) in scenes::FRAMES.into_iter().zip(GOLDEN) {
        let scene = scenes::expected_scene(frame, &pixels);
        let want = scenes::oracle(&scene, &pixels);
        let base = render(&Renderer::new(64).unwrap(), &scene, &pixels);
        let delta = scenes::max_delta(&base, &want);
        println!("frame {frame}: cpu digest {} max |delta| vs oracle {delta:e}", scenes::hex(&base.digest()));
        assert!(delta <= CPU_TOLERANCE, "frame {frame}: {delta}");
        assert_eq!(scenes::hex(&base.digest()), golden, "frame {frame} digest");
        for (chunk, threads) in [(5, 1), (7, 4), (4, 8), (64, 2)] {
            let other = render(&Renderer::with_threads(chunk, threads).unwrap(), &scene, &pixels);
            assert_eq!(other.digest(), base.digest(), "frame {frame} chunk {chunk} threads {threads}");
        }
        // The animated frames resample with the composition's declared filter.
        let keys: Vec<_> = base.exactness.operators.iter().map(|o| o.key).collect();
        assert_eq!(keys.contains(&"transform_bilinear"), frame != 0, "{keys:?}");
    }
}
