//! Frame blending and roto-matte contents on the CPU backend (lane SMB-AE-ROTO-WIRING; decision
//! B1: cross-crate render proofs live in renderer test files). Scenes and oracles come from
//! `hsk-studio-composite/tests/support/roto_scenes.rs`, where composite's
//! `tests/composition_roto.rs` proves the Folio records evaluate to these contents. The oracle of
//! a blended frame is Pigment's `frame_mix` / `pixel_motion` on the hand-written source frames; of
//! a roto frame, the source pixel with alpha times coverage and the matte colour. Each frame must
//! match its oracle and be identical for every chunk size and thread count; render-gpu's
//! `tests/composition_roto_frames.rs` renders the same frames on the GPU.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::composition::FrameBlendMethod;
use hsk_studio_composite::{RenderedScene, SceneComposition, SceneContext, SceneLimits, render_scene};
use hsk_studio_pigment::frame_blend::{BlendLimits, FlowParams, frame_mix, pixel_motion};
use hsk_studio_pigment::render_plan::{Exactness, ExecuteRequest};
use hsk_studio_pigment::retouch::RgbaPlane;
use hsk_studio_render_cpu::Renderer;

#[path = "../../hsk-studio-composite/tests/support/roto_scenes.rs"]
mod scenes;

/// The two source frames are rendered by the backend and then blended on the host by Pigment, so
/// a CPU frame equals the oracle up to the rounding of the final placement.
const CPU_TOLERANCE: f32 = 1e-6;

fn cases(pixels: &scenes::Pixels) -> Vec<(String, SceneComposition, RgbaPlane)> {
    let cancel = CancellationToken::default();
    let mut out = Vec::new();
    for (a, b, w) in [(0u64, 1u64, 0.5f64), (2, 3, 0.25), (6, 7, 0.75)] {
        out.push((
            format!("frame_mix/{a}-{b}@{w}"),
            scenes::blended_scene(a, b, w, FrameBlendMethod::FrameMix, pixels),
            frame_mix(&scenes::nested_plane(a), &scenes::nested_plane(b), w, &BlendLimits::DEFAULT).unwrap(),
        ));
        out.push((
            format!("pixel_motion/{a}-{b}@{w}"),
            scenes::blended_scene(a, b, w, FrameBlendMethod::PixelMotion, pixels),
            pixel_motion(&scenes::nested_plane(a), &scenes::nested_plane(b), w, &FlowParams::DEFAULT, &BlendLimits::DEFAULT, &cancel).unwrap(),
        ));
    }
    for k in [0u64, 3, 7] {
        let with = scenes::disc_matte(k);
        let mut plain = scenes::disc_matte(k);
        plain.colours = None;
        out.push((format!("roto/{k}/decontaminated"), scenes::roto_scene(k, with.clone(), pixels), scenes::roto_oracle(k, &with)));
        out.push((format!("roto/{k}/plain"), scenes::roto_scene(k, plain.clone(), pixels), scenes::roto_oracle(k, &plain)));
    }
    out
}

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
fn roto_and_frame_blend_frames_match_the_oracle_on_every_chunking() {
    let pixels = scenes::Pixels::fixture();
    for (label, scene, want) in cases(&pixels) {
        let base = render(&Renderer::new(64).unwrap(), &scene, &pixels);
        let delta = scenes::max_delta(&base, &want);
        println!("{label}: cpu digest {} max |delta| vs oracle {delta:e}", scenes::hex(&base.digest()));
        assert!(delta <= CPU_TOLERANCE, "{label}: {delta}");
        for (chunk, threads) in [(5, 1), (7, 4), (4, 8)] {
            let other = render(&Renderer::with_threads(chunk, threads).unwrap(), &scene, &pixels);
            assert_eq!(other.digest(), base.digest(), "{label} chunk {chunk} threads {threads}");
        }
        let keys: Vec<_> = base.exactness.operators.iter().map(|o| o.key).collect();
        let key = if label.starts_with("roto/") { "roto_matte" } else { "frame_blend" };
        assert!(keys.contains(&key), "{label}: {keys:?}");
        assert_eq!(base.exactness.lowest, Exactness::Approximate, "{label}");
    }
}
