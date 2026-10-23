//! Corner-pinned composition layers on the CPU backend (lane SMB-AE-KEY-TRACK-3): `render_scene`
//! lowers a `SceneContent::Pinned` layer to pigment's projective layer transform (pin, then the
//! layer matrix); every pixel must match the shared oracle and the frame must be identical for every
//! chunk size and thread count. render-gpu's `tests/composition_corner_pin.rs` renders the same
//! scene against the same oracle.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::{RenderedScene, SceneComposition, SceneContent, SceneContext, SceneLimits, render_scene};
use hsk_studio_pigment::render_plan::{Exactness, ExecuteRequest};
use hsk_studio_render_cpu::Renderer;

#[path = "../../hsk-studio-composite/tests/support/composition_scenes.rs"]
mod scenes;
#[path = "../../hsk-studio-composite/tests/support/pinned_scene.rs"]
mod pinned;

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
fn corner_pinned_layers_match_the_oracle_on_every_chunking() {
    let pixels = scenes::Pixels::fixture();
    let scene = pinned::pinned_scene(&pixels);
    let want = pinned::pinned_oracle(&scene, &pixels);
    let base = render(&Renderer::new(64).unwrap(), &scene, &pixels);
    let delta = scenes::max_delta(&base, &want);
    println!("pinned: cpu digest {} max |delta| vs oracle {delta:e}", scenes::hex(&base.digest()));
    assert!(delta <= 1e-6, "{delta}");
    for (chunk, threads) in [(5, 1), (7, 4), (4, 8), (64, 2)] {
        let other = render(&Renderer::with_threads(chunk, threads).unwrap(), &scene, &pixels);
        assert_eq!(other.digest(), base.digest(), "chunk {chunk} threads {threads}");
    }
    // The pin matters: the same layers without it render differently.
    let mut plain = scene.clone();
    for l in &mut plain.layers {
        if let SceneContent::Pinned(p) = &l.content {
            l.content = p.content.clone();
        }
    }
    let unpinned = render(&Renderer::new(64).unwrap(), &plain, &pixels);
    assert_ne!(unpinned.digest(), base.digest());
    // A pin on content without pixels of its own is refused typed.
    let mut bad = scene.clone();
    if let SceneContent::Pinned(p) = &mut bad.layers[0].content {
        p.content = SceneContent::Pinned(Box::new((**p).clone()));
    }
    let ctx = SceneContext {
        backend: &Renderer::new(64).unwrap(),
        pixels: &pixels,
        request: ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 },
        space: scenes::space(),
        limits: SceneLimits::default(),
    };
    let e = render_scene(&bad, &ctx, &CancellationToken::default()).unwrap_err();
    assert!(matches!(e, hsk_studio_composite::TemporalError::Unsupported { feature: "composition_corner_pin_content", .. }), "{e:?}");
}
