//! Composition frames on the CPU backend (lane SMB-COMPOSITE-COMPOSITION-ADAPTER; decision B1:
//! cross-crate render proofs live in renderer test files). The scenes come from
//! `hsk-studio-composite/tests/support/composition_scenes.rs`, where composite's
//! `tests/composition_adapter.rs` proves the stored Folio compositions evaluate to them. Each
//! frame is rendered by `composite::render_scene` through this backend and compared, pixel by
//! pixel, with the shared oracle (pigment reference sampler + `composite_over`); the frame digest
//! must equal the recorded golden digest and be identical for every chunk size and thread count.
//! render-gpu's `tests/composition_frames.rs` compares the same frames with the same oracle
//! (CPU/GPU parity).
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::{RenderedScene, SceneComposition, SceneContext, SceneLimits, render_scene};
use hsk_studio_pigment::render_plan::{Exactness, ExecuteRequest};
use hsk_studio_render_cpu::Renderer;

#[path = "../../hsk-studio-composite/tests/support/composition_scenes.rs"]
mod scenes;

/// The CPU executes pigment's binary64 sampler and binary32 blend functions; the oracle calls the
/// same functions per pixel, so the frames agree to float rounding of the composition order.
const CPU_TOLERANCE: f32 = 1e-6;
/// Frame digests (colour plane then alpha plane, SHA-256) of every `scenes::cases` entry,
/// recorded when the case was added; any change to a frame's pixels shows here.
const GOLDEN: [(&str, &str); 17] = [
    ("comp1/frame0", "80952af10847e9206fc169b6d71ba416136135d5a6c92fd6df58ef22a695f1c3"),
    ("comp1/frame5", "17c0727e2320ba849bd7b70c829b62ce2cf7ffa080797d9d78ab32e1da2b41e6"),
    ("comp1/frame10", "b88a69fc89e656aa9c1b4f2732cf17ed35ff7d2586822cf08288e7c4f083d5c5"),
    ("comp1/frame25", "01cec491147e59ed5928d24858e45b33510ed28d7d91ca827afd01a02758ff64"),
    ("comp2/frame0", "10ce5462a698d61fe7b306244b81ac6d7ad2c7f762f1afb27d0e0f43fe700abf"),
    ("comp2/frame5", "4209af188370aab1afda1cfdd2201f9e9d93cfdb76dea253c53bc2d7a1ddb0ea"),
    ("comp2/frame10", "b1bfe92b4d96a8371a87c97efb9493c4f5129e9041760fb8f438099eefdca548"),
    ("comp3/frame0", "e7db49c40019dbf54679d502694c8d2cda0131906cab33c2945c348578304eab"),
    ("comp3/frame6", "df069a9dfb244bc42238884578f8e0a21d579beae8e6cd467c47dc21377b6092"),
    ("comp3/frame12", "476faede688b6943c9a3a6055651a866a26f350bb484ff117fbdd2e788d2194e"),
    ("comp3/frame22", "622146a4e65879880e89b68ce2eb73c16034d4b48ed6a4505684a64f0c492513"),
    // The nested composition is outside its duration: only the background (same as frame 0).
    ("comp3/frame30", "e7db49c40019dbf54679d502694c8d2cda0131906cab33c2945c348578304eab"),
    ("comp5/frame0", "41a302d58cbaaca5ed5ef2ae38e0878ed001083ac748c3b9bc61bb9aa7f158c3"),
    ("comp5/frame5", "728b77afbe7d167a9ae8ba788e8f0775061e99f40c08e091cfec324687463ee9"),
    ("comp5/frame10", "0b6dffa54759a1c535cb80f8bda687416fe8a8dd60993c3ef6343a66574eeb63"),
    ("comp6/frame0", "422cb1b81f3cc3e555e313c7c26d13c59bee6185f5c061b678e89d09067d5fc6"),
    ("comp6/frame7", "ae86ccf7cf0edc238e66cfcf38c1a7e1597fcc7154bf4efb6b132ad4c0bdcc48"),
];

fn render(renderer: &Renderer, scene: &SceneComposition, pixels: &scenes::Pixels) -> RenderedScene {
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
    let cases = scenes::cases(&pixels);
    let mut digests = Vec::new();
    for (label, scene) in &cases {
        let want = scenes::oracle(scene, &pixels);
        let base = render(&Renderer::new(64).unwrap(), scene, &pixels);
        let delta = scenes::max_delta(&base, &want);
        let digest = scenes::hex(&base.digest());
        println!("{label}: cpu digest {digest} max |delta| vs oracle {delta:e}");
        assert!(delta <= CPU_TOLERANCE, "{label}: {delta}");
        for (chunk, threads) in [(5, 1), (7, 4), (4, 8), (64, 2)] {
            let other = render(&Renderer::with_threads(chunk, threads).unwrap(), scene, &pixels);
            assert_eq!(other.digest(), base.digest(), "{label} chunk {chunk} threads {threads}");
        }
        // A frame resamples with the composition's declared filter exactly when one of its
        // layers (precompositions and mattes included) has a non-integer matrix.
        let keys: Vec<_> = base.exactness.operators.iter().map(|o| o.key).collect();
        assert_eq!(keys.contains(&"transform_bilinear"), scenes::scene_resamples(scene), "{label}: {keys:?}");
        assert_eq!(keys.contains(&"track_matte"), label.starts_with("comp5/"), "{label}: {keys:?}");
        digests.push((label.clone(), digest));
    }
    let golden: Vec<(String, String)> =
        GOLDEN.iter().map(|(l, d)| ((*l).to_owned(), (*d).to_owned())).collect();
    assert_eq!(digests, golden, "every case has its recorded golden digest");
}
