//! Vector masks on the CPU backend (lane SMB-COMPOSITE-VECTOR-MASK; decision B1). The nib layer of
//! `hsk-studio-composite/tests/support/vector_scenes.rs` masked four ways
//! (`tests/support/mask_scenes.rs`: VECTOR, VECTOR inverted, ALPHA and LUMINANCE inverted in a
//! region of a group source) is lowered by `vector_layer_render_plan` and rendered here; every pixel
//! is compared with the shared oracle and the bytes must be identical for every chunk and thread
//! count. Linked vs unlinked: a vector mask is anchored to its source's document geometry (moving the
//! masked layer's own geometry leaves the mask plane in place), while a composition moves a masked
//! static subtree, mask included, with its layer matrix. render-gpu `tests/vector_mask.rs` compares
//! the same cases with the same oracle (CPU/GPU parity).
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::{
    InputsLowering, OwnedMask, PreparedVectors, SceneComposition, SceneContent, SceneContext,
    SceneLayer, SceneLimits, ScenePixels, StaticContent, StudioBlendMode, Surface,
    render_scene, vector_layer_render_plan,
};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, Exactness, ExecuteRequest, OutputTarget, RenderBackend, SampleFormat, SinkError,
    TilePlanes, TileSink,
};
use hsk_studio_pigment::transform_math::{Affine2, ResampleFilter};
use hsk_studio_render_cpu::Renderer;
use std::collections::BTreeMap;

#[path = "../../hsk-studio-composite/tests/support/mask_scenes.rs"]
mod masks;
use masks::base as scenes;

const CPU_TOLERANCE: f32 = 1e-6;
/// Frame digests of `masks::CASES`, in order. VECTOR equals render-cpu `tests/vector_nib.rs`
/// `GOLDEN_CLIPPED`: a VECTOR mask of the diamond and a vector clip of the diamond are one frame.
const GOLDENS: [&str; 4] = [
    "cd06d6dc9b66868a3baa5fdd2a19c04487976121b6ee2ea3e2af0e33f48fb557",
    "32003f5af9d7f756bf47ada9608631f63b016a97a10dc9fa97d5d2952440c1f8",
    "bfb5e372c9304a683d19308d3e6cf7fbeb7ad081b9c36d1f55036bf5b38c847d",
    "1bc5ead349730894ce5ff53d3cc74d2b271bf64689e7fa192859c0aad966fd5b",
];

struct Frame(Vec<[f32; 4]>);
impl TileSink for Frame {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let f = |b: &[u8], i: usize| f32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        for row in 0..r.height as usize {
            for col in 0..r.width as usize {
                let i = row * r.width as usize + col;
                let at = (r.y as usize + row) * scenes::W as usize + r.x as usize + col;
                self.0[at] = [f(colour, i * 12), f(colour, i * 12 + 4), f(colour, i * 12 + 8), f(alpha, i * 4)];
            }
        }
        Ok(())
    }
}
fn digest(px: &[[f32; 4]]) -> String {
    let mut bytes = Vec::new();
    for p in px {
        for v in p {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
    }
    hsk_studio_prism::profile_hash(&bytes).iter().map(|b| format!("{b:02x}")).collect()
}
fn target() -> OutputTarget {
    OutputTarget {
        extent: Rect { x: 0, y: 0, width: scenes::W, height: scenes::H },
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        space: scenes::space(),
    }
}
fn request() -> ExecuteRequest {
    ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 }
}
fn run(r: &Renderer, low: &InputsLowering, vectors: &PreparedVectors) -> Vec<[f32; 4]> {
    let mut frame = Frame(vec![[0.0; 4]; (scenes::W * scenes::H) as usize]);
    let sources = scenes::Sources { low, vectors };
    r.execute_extended(&low.plan, &low.extensions, &request(), &sources, &CancellationToken::default(), &mut frame)
        .expect("renders");
    frame.0
}

#[test]
fn masked_nib_layer_matches_the_oracle_for_every_mask_kind_and_chunking() {
    let mut digests = Vec::new();
    for (case, golden) in masks::CASES.into_iter().zip(GOLDENS) {
        let vectors = masks::masked(case);
        let plane = vectors.plane(scenes::LAYER).unwrap();
        let low = vector_layer_render_plan(&vectors, scenes::LAYER, 7, target()).unwrap();
        let base = run(&Renderer::new(64).unwrap(), &low, &vectors);
        let delta = scenes::max_delta(&base, &masks::oracle_masked(plane, &low));
        let d = digest(&base);
        println!("{case}: cpu digest {d} max |delta| vs oracle {delta:e}");
        assert!(delta <= CPU_TOLERANCE, "{case}: {delta}");
        assert_ne!(d, digest(&scenes::oracle(plane)), "{case}: the mask changes the frame");
        for (chunk, threads) in [(5, 1), (7, 4), (64, 2)] {
            assert_eq!(digest(&run(&Renderer::with_threads(chunk, threads).unwrap(), &low, &vectors)), d, "{case}: chunk {chunk} threads {threads}");
        }
        assert_eq!(d, golden, "{case}: pinned frame digest");
        digests.push(d);
    }
    digests.sort();
    digests.dedup();
    assert_eq!(digests.len(), masks::CASES.len(), "every mask kind renders differently");
    // Inverted VECTOR = the unmasked frame minus the VECTOR-masked one (pigment invert, full outside).
    let vectors = masks::masked("vector_inverted");
    let low = vector_layer_render_plan(&vectors, scenes::LAYER, 7, target()).unwrap();
    assert!((0..scenes::W as i64).all(|x| masks::mask_at(&low, x, 0) == 1.0), "inverted: full outside the diamond");
}

/// Caller pixels of a static subtree that owns everything it reads.
struct NoPixels;
impl ScenePixels for NoPixels {
    fn tile(&self, _: &str) -> Option<TilePlanes<'_>> {
        None
    }
}

#[test]
fn vector_masks_are_document_anchored_and_move_with_a_composition_layer() {
    // Unlinked from the masked layer's own geometry: the mask plane is the source's, in document
    // space, wherever the masked layer's paths are.
    let moved = {
        let v = masks::prepared_moved(3.0, 2.0);
        let m = masks::mask("alpha", &v);
        v.with_mask(scenes::LAYER, m)
    };
    let still = masks::masked("alpha");
    let low_moved = vector_layer_render_plan(&moved, scenes::LAYER, 7, target()).unwrap();
    let low_still = vector_layer_render_plan(&still, scenes::LAYER, 7, target()).unwrap();
    assert_ne!(moved.plane(scenes::LAYER).unwrap().placement, still.plane(scenes::LAYER).unwrap().placement);
    for y in 0..scenes::H as i64 {
        for x in 0..scenes::W as i64 {
            let (a, b) = (masks::mask_at(&low_moved, x, y), masks::mask_at(&low_still, x, y));
            // Both planes hold the source content; outside one plane's placement it reads zero.
            assert!(a == b || a == 0.0 || b == 0.0, "({x},{y}) {a} vs {b}");
        }
    }
    let frame = run(&Renderer::new(64).unwrap(), &low_moved, &moved);
    let delta = scenes::max_delta(&frame, &masks::oracle_masked(moved.plane(scenes::LAYER).unwrap(), &low_moved));
    assert!(delta <= CPU_TOLERANCE, "moved layer: {delta}");

    // Linked in a composition: the masked subtree (mask plane included) moves with the layer matrix.
    let low = low_still;
    let vectors = &still;
    let own = |m: hsk_studio_pigment::Mask<'_>| OwnedMask {
        bounds: m.bounds,
        sample_format: m.sample_format.to_owned(),
        bytes: m.bytes.to_vec(),
        stride_bytes: m.stride_bytes,
        sha256: m.sha256,
    };
    let mut owned_masks = BTreeMap::new();
    let mut owned_tiles = BTreeMap::new();
    let keys = low.vector_masks.iter().map(|(k, _)| *k)
        .chain(low.vector_rows.iter().map(|(k, _, _)| *k))
        .chain(low.clip_planes.iter().map(|(k, _)| *k));
    for key in keys {
        if let Some(m) = low.vector_mask(key, vectors) {
            owned_masks.insert(key, own(m));
        } else if let Some(t) = low.vector_paint(key, vectors, scenes::space()) {
            owned_tiles.insert(key, Surface {
                width: t.width,
                height: t.height,
                colour: t.colour.to_vec(),
                alpha: t.alpha.to_vec(),
                colour_sha256: t.colour_sha256,
                alpha_sha256: t.alpha_sha256,
            });
        }
    }
    let content = StaticContent {
        plan: low.plan.clone(),
        extensions: low.extensions.clone(),
        rect: target().extent,
        tiles: BTreeMap::new(),
        masks: BTreeMap::new(),
        owned_masks,
        owned_tiles,
    };
    let (dx, dy) = (5i64, 3i64);
    let scene = SceneComposition {
        composition_id: "masked".into(),
        width: scenes::W + 8,
        height: scenes::H + 6,
        background: None,
        filter: ResampleFilter::Bilinear,
        layers: vec![SceneLayer {
            layer_id: scenes::LAYER.into(),
            content: SceneContent::Static(Box::new(content)),
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            fill_opacity: 1.0,
            matrix: Affine2 { m: [1.0, 0.0, dx as f64, 0.0, 1.0, dy as f64] },
            matte: None,
            clipped: false,
        }],
    };
    let renderer = Renderer::new(64).unwrap();
    let ctx = SceneContext {
        backend: &renderer,
        pixels: &NoPixels,
        request: request(),
        space: scenes::space(),
        limits: SceneLimits::default(),
    };
    let rendered = render_scene(&scene, &ctx, &CancellationToken::default()).expect("scene renders");
    let still_frame = run(&renderer, &low, vectors);
    let f = |b: &[u8], i: usize| f32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    let sw = scene.width as i64;
    let mut worst = 0.0f32;
    for y in 0..scene.height as i64 {
        for x in 0..sw {
            let i = (y * sw + x) as usize;
            let got = [f(&rendered.colour, i * 12), f(&rendered.colour, i * 12 + 4), f(&rendered.colour, i * 12 + 8), f(&rendered.alpha, i * 4)];
            let (sx, sy) = (x - dx, y - dy);
            let want = if sx >= 0 && sy >= 0 && sx < scenes::W as i64 && sy < scenes::H as i64 {
                still_frame[(sy * scenes::W as i64 + sx) as usize]
            } else {
                [0.0; 4]
            };
            worst = worst.max(scenes::max_delta(&[got], &[want]));
        }
    }
    println!("composition (translate {dx},{dy}) vs moved static frame: max |delta| {worst:e}");
    assert!(worst <= CPU_TOLERANCE, "{worst}");
}
