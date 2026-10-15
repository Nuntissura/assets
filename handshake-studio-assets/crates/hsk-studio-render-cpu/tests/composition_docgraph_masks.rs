//! Lane SMB-COMPOSITE-DOCGRAPH-MASKS on the CPU backend (decision B1): the document-graph frame of
//! `hsk-studio-composite/tests/support/docgraph_scenes.rs` (vector clip, inverted VECTOR mask with
//! density and feather, a layer clipped to that masked base, an unlinked ALPHA mask that stays
//! while its layer moves, a LUMINANCE mask with reveal_outside), which composite's
//! `tests/composition_docgraph_masks.rs` proves is exactly what the Folio records lower to on the
//! document-graph path. Every pixel is compared with the shared oracle; the frame digest is pinned
//! and identical on every chunk size and thread count; the scene path of the same composition
//! (`render_scene` of the fixture scene, which composite proves equals `scene_at`) renders the
//! same pixels. render-gpu `tests/composition_docgraph_masks.rs` runs the same frame on the GPU.
use hsk_studio_render_cpu::Renderer;

#[path = "../../hsk-studio-composite/tests/support/docgraph_scenes.rs"]
mod fixture;

/// Digest (colour plane then alpha plane, SHA-256) of the CPU frame, recorded when the case was
/// added.
const GOLDEN: &str = "pending";

#[test]
fn document_graph_frame_with_vector_clips_and_masks_matches_the_oracle_and_the_scene_path() {
    let pixels = fixture::scenes::Pixels::fixture();
    let frame = fixture::document_frame(&pixels);
    let oracle = fixture::oracle(&pixels);
    let base = fixture::render_frame(&Renderer::new(64).unwrap(), &frame, &pixels).expect("frame renders");
    let delta = fixture::max_delta(&base, &oracle);
    let off: Vec<usize> = (0..base.len()).filter(|i| base[*i] != oracle[*i]).collect();
    let digest = fixture::digest(&base);
    println!("cpu document-graph frame: digest {digest}, max |delta| vs oracle {delta:e}, {} of {} pixels not bit-identical", off.len(), base.len());
    for i in off.iter().take(8) {
        println!("  ({}, {}): cpu {:?} oracle {:?}", i % 32, i / 32, base[*i], oracle[*i]);
    }
    for (chunk, threads) in [(5, 1), (7, 4), (4, 8), (64, 2)] {
        let other = fixture::render_frame(&Renderer::with_threads(chunk, threads).unwrap(), &frame, &pixels).unwrap();
        assert_eq!(fixture::digest(&other), digest, "chunk {chunk} threads {threads}");
    }
    let scene = fixture::render_scene_px(&Renderer::new(64).unwrap(), &fixture::scene(&pixels), &pixels);
    let scene_off = (0..base.len()).filter(|i| base[*i] != scene[*i]).count();
    println!("cpu scene path vs document graph: max |delta| {:e}, {scene_off} pixels not bit-identical", fixture::max_delta(&scene, &base));
    assert!(off.is_empty(), "bit for bit against the oracle");
    assert_eq!(digest, GOLDEN, "golden digest");
}
