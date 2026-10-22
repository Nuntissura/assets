//! Stroke rows with their own geometry on the CPU backend (lane SMB-COMPOSITE-STROKE-GEOMETRY): the
//! layer of `hsk-studio-composite/tests/support/stroke_geometry_scene.rs` (a fill rectangle and two
//! stroke rows whose outlines differ from the fill geometry, as a vector network or a variable-width
//! stroke) is prepared by composite, lowered by `vector_layer_render_plan` and rendered here; every
//! pixel is compared with the shared oracle (`vector_scenes.rs`: pigment `composite_over` over nib's
//! planes), bit-identical for every chunk and thread count, and the frame digest is pinned.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::{VectorCoverage, VectorResolution, prepare_vector_layers, vector_layer_render_plan};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, Exactness, ExecuteRequest, OutputTarget, RenderBackend, SampleFormat, SinkError, TileSink,
};
use hsk_studio_render_cpu::Renderer;

#[path = "../../hsk-studio-composite/tests/support/vector_scenes.rs"]
mod scenes;
#[path = "../../hsk-studio-composite/tests/support/stroke_geometry_scene.rs"]
mod stroke_scene;

const CPU_TOLERANCE: f32 = 1e-6;
const GOLDEN: &str = "73a616465d6dab7c3c18a0fde6e0da230957f6088c879806a07e4790e31f4bb9";

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

#[test]
fn stroke_geometry_layer_matches_the_oracle_on_every_chunking_and_thread_count() {
    let cancel = CancellationToken::default();
    let res = VectorResolution { coverage: VectorCoverage::Exact, ..VectorResolution::default() };
    let vectors = prepare_vector_layers(&[stroke_scene::spec()], &res, &cancel).unwrap();
    let plane = vectors.plane(scenes::LAYER).unwrap();
    let target = OutputTarget {
        extent: Rect { x: 0, y: 0, width: scenes::W, height: scenes::H },
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        space: scenes::space(),
    };
    let low = vector_layer_render_plan(&vectors, scenes::LAYER, 7, target).unwrap();
    let sources = scenes::Sources { low: &low, vectors: &vectors };
    let request = ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 };
    let run = |r: &Renderer| {
        let mut frame = Frame(vec![[0.0; 4]; (scenes::W * scenes::H) as usize]);
        r.execute_extended(&low.plan, &low.extensions, &request, &sources, &cancel, &mut frame).expect("renders");
        frame.0
    };
    let base = run(&Renderer::new(64).unwrap());
    let want = scenes::oracle(plane);
    let delta = scenes::max_delta(&base, &want);
    let d = digest(&base);
    println!("cpu stroke-geometry digest {d} max |delta| vs oracle {delta:e}");
    assert!(delta <= CPU_TOLERANCE, "{delta}");
    // The stroke outline paints where the fill geometry has no coverage (outside the rectangle).
    let px = |x: usize, y: usize| base[y * scenes::W as usize + x];
    assert!(px(28, 7)[3] > 0.8, "band outside the fill is painted: {:?}", px(28, 7));
    assert_eq!(px(1, 1)[3], 0.0);
    for (chunk, threads) in [(5, 1), (7, 4), (4, 8), (64, 2)] {
        assert_eq!(digest(&run(&Renderer::with_threads(chunk, threads).unwrap())), d, "chunk {chunk} threads {threads}");
    }
    assert_eq!(d, GOLDEN, "pinned frame digest");
}
