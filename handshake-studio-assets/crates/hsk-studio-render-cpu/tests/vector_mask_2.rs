//! Lane SMB-COMPOSITE-VECTOR-MASK-2 on the CPU backend (decision B1): every case of
//! `hsk-studio-composite/tests/support/mask_scenes.rs` `lane2_cases` against the shared oracle
//! (the unmasked layer oracle cut by pigment's effective mask, feather then invert and density):
//! density 40 % + feather 2 px on a VECTOR mask, Illustrator's `reveal_outside` LUMINANCE, a
//! caller-rendered raster ALPHA source (`resolve_mask_source`, feathered), pigment's
//! `isolate_masked_pass_through` (refused as `mask_on_pass_through` before, the isolated W3C
//! reading after) and an unlinked mask in a composition (`unlinked_mask_matte`: the layer moves by
//! (5, 3), its mask stays). render-gpu `tests/vector_mask_2.rs` runs the same cases on the GPU.
use hsk_studio_render_cpu::Renderer;

#[path = "../../hsk-studio-composite/tests/support/mask_scenes.rs"]
mod masks;

const CPU_TOLERANCE: f32 = 1e-6;

#[test]
fn mask_parameters_rendered_sources_pass_through_isolation_and_unlinked_mattes_match_the_oracle() {
    for (chunk, threads) in [(64, 1), (7, 4)] {
        let cases = masks::lane2_cases(&Renderer::with_threads(chunk, threads).unwrap());
        assert_eq!(cases.len(), 5);
        for (case, delta) in cases {
            println!("{case} (chunk {chunk}, threads {threads}): cpu max |delta| vs oracle {delta:e}");
            assert!(delta <= CPU_TOLERANCE, "{case}: {delta}");
        }
    }
}
