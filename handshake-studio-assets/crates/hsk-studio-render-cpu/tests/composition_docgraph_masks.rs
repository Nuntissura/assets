//! Lane SMB-COMPOSITE-DOCGRAPH-MASKS on the CPU backend (decision B1): the document-graph frame of
//! `hsk-studio-composite/tests/support/docgraph_scenes.rs` (vector clip, inverted VECTOR mask with
//! density and feather, a layer clipped to that masked base, an unlinked ALPHA mask that stays
//! while its layer moves, a LUMINANCE mask with reveal_outside), which composite's
//! `tests/composition_docgraph_masks.rs` proves is exactly what the Folio records lower to on the
//! document-graph path. Every pixel is compared with the shared oracle; the frame digest is pinned
//! and identical on every chunk size and thread count; the scene path of the same composition
//! (`render_scene` of the fixture scene, which composite proves equals `scene_at`) renders the
//! same pixels (`path_agreement`: alpha bit-identical, colour within 2^-22 under a surfaced layer).
//! render-gpu `tests/composition_docgraph_masks.rs` runs the same frame on the GPU.
use hsk_studio_render_cpu::Renderer;

#[path = "../../hsk-studio-composite/tests/support/docgraph_scenes.rs"]
mod fixture;

/// Digest (colour plane then alpha plane, SHA-256) of the CPU frame, recorded when the case was
/// added.
const GOLDEN: &str = "424d4e27d452383a1d5f02dbf985671c424ca44733cb833461392d6210c92b8d";

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
    println!("cpu scene path vs document graph: max |delta| {:e}", fixture::max_delta(&scene, &base));
    match fixture::path_agreement(&base, &scene) {
        Ok(ulp) => println!("  {} of {} pixels bit-identical, {ulp} within the surface rounding under a surfaced layer", base.len() - ulp, base.len()),
        Err(e) => panic!("{}", e.join("\n")),
    }
    assert!(off.is_empty(), "bit for bit against the oracle");
    assert_eq!(digest, GOLDEN, "golden digest");
}

/// Digest of the CPU frame of the moved variant (lane SMB-COMPOSITE-DOCGRAPH-2), recorded when the
/// case was added.
const MOVED_GOLDEN: &str = "ee603289d737b97dd7bf4f0fa648d72d7804943ff0611773b006b5878fc42899";

/// Lane SMB-COMPOSITE-DOCGRAPH-2 goal 2: fractional, rotated and scaled moves of clipped and masked
/// layers on the document graph (`fixture::moved_frame`, which composite's
/// `fractional_and_rotated_moves_of_clipped_and_masked_layers` proves is what the Folio records
/// lower to): bilinear tile transforms, a vector clip and a linked LUMINANCE reveal_outside mask
/// resampled with their content, an unlinked ALPHA mask in place, a Folio clipped layer rotated
/// over its masked base. Every pixel equals `fixture::moved_oracle` (pigment's reference sampler
/// and `composite_over`, independent of the plan) bit for bit, except at most one binary32 ULP of
/// colour (5.96e-8) inside the rotated Folio clipped layer's destination bounds (measured: 2 of 768
/// pixels; render-cpu's clip run over a resampled clipped layer rounds there, the composite
/// reference executor matches the oracle bit for bit); the digest is pinned and identical on every
/// chunk size and thread count.
#[test]
fn moved_document_graph_frame_matches_the_oracle() {
    let pixels = fixture::scenes::Pixels::fixture();
    let frame = fixture::moved_frame(&pixels);
    let oracle = fixture::moved_oracle(&pixels);
    let base = fixture::render_frame(&Renderer::new(64).unwrap(), &frame, &pixels).expect("frame renders");
    let off: Vec<usize> = (0..base.len()).filter(|i| base[*i] != oracle[*i]).collect();
    let digest = fixture::digest(&base);
    println!("cpu moved frame: digest {digest}, max |delta| vs oracle {:e}, {} of {} pixels not bit-identical",
        fixture::max_delta(&base, &oracle), off.len(), base.len());
    for i in off.iter().take(8) {
        println!("  ({}, {}): cpu {:?} oracle {:?}", i % 32, i / 32, base[*i], oracle[*i]);
    }
    for (chunk, threads) in [(5, 1), (7, 4), (4, 8), (64, 2)] {
        let other = fixture::render_frame(&Renderer::with_threads(chunk, threads).unwrap(), &frame, &pixels).unwrap();
        assert_eq!(fixture::digest(&other), digest, "chunk {chunk} threads {threads}");
    }
    let l12 = fixture::spec(12);
    let t = fixture::layer_transform(l12).expect("layer 12 resamples");
    let rotated = hsk_studio_pigment::transform_math::PreparedTransform::new(&t, fixture::moved_tile_rect(l12)).unwrap().bounds();
    for i in &off {
        let (x, y) = ((i % 32) as i64, (i / 32) as i64);
        let inside = x >= rotated.x && y >= rotated.y && x < rotated.x + i64::from(rotated.width) && y < rotated.y + i64::from(rotated.height);
        assert!(inside, "({x}, {y}) differs outside the rotated clipped layer");
        assert_eq!(base[*i][3], oracle[*i][3], "({x}, {y}): alpha bit for bit");
    }
    assert!(fixture::max_delta(&base, &oracle) <= 5.97e-8, "at most one ULP");
    assert!(off.len() <= 2, "{} pixels off", off.len());
    assert_eq!(digest, MOVED_GOLDEN, "golden digest");
}
