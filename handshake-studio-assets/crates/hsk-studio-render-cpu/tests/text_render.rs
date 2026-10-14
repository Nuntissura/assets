//! SMB-COMPOSITE-TEXT (decision B1: render-cpu dev-depends on composite): render-cpu executes the
//! plan composite lowers for a text layer (`text_layer_render_plan`, equal byte for byte to the
//! document lowering, composite test text_layer_lowers_with_pinned_digest). Glyphs: Inter "Hi"
//! shaped by Type at 12 pt and rasterised at 192 ppi (fixtures of hsk-studio-type, V4-03).
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::{
    PreparedText, TextLayerInput, TextRenderLowering, TextResolution, TextRun, prepare_text,
    text_layer_render_plan,
};
use hsk_studio_pigment::render_plan::*;
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_prism::profile_hash;
use hsk_studio_render_cpu::*;
use hsk_studio_type::FontBytes;

#[path = "../../hsk-studio-composite/tests/common/text_shaping.rs"]
mod shaping;
use shaping::{FontFile, Seg, shape};

const LAYER: &str = "SLYR-019abcde-0000-7000-8000-000000000003";
const FILL: [f32; 4] = [0.8, 0.1, 0.2, 1.0];
/// SHA-256 of the rendered colour plane followed by the alpha plane (rgb_f32le, f32le straight).
const CPU_DIGEST: &str = "UNSET";

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
fn prepared() -> PreparedText {
    let files = [FontFile::inter()];
    let segs = [Seg { start: 0, end: 2, font: 0, script: *b"Latn", language: "en", axes: vec![] }];
    shape(&files, "Hi", &segs, false, 12.0, |p| {
        let run = &p.paragraphs()[0].runs()[0];
        let runs = [TextRun { run, font: FontBytes { bytes: &files[0].bytes, resource: run.resolved() }, origin_px: [10.25, 40.0] }];
        prepare_text(
            &[TextLayerInput { layer_id: LAYER, runs: &runs, fill_rgba: FILL }],
            &TextResolution::at_ppi(192.0),
            &CancellationToken::default(),
        )
        .unwrap()
    })
}
fn target() -> OutputTarget {
    OutputTarget {
        extent: Rect { x: 0, y: 0, width: 64, height: 64 },
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        space: ColourTag { profile_sha256: profile_hash(b"working-profile"), blend_space: BlendSpace::Encoded },
    }
}
struct Sources<'a> {
    low: &'a TextRenderLowering,
    text: &'a PreparedText,
}
impl PlanSources for Sources<'_> {
    fn tile(&self, _: SourceKey) -> Option<TilePlanes<'_>> {
        None
    }
    fn mask(&self, key: SourceKey) -> Option<Mask<'_>> {
        self.low.text_mask(key, self.text)
    }
}
struct Planes {
    extent: Rect,
    colour: Vec<u8>,
    alpha: Vec<u8>,
}
impl TileSink for Planes {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let w = r.width as usize;
        for row in 0..r.height as usize {
            let at = (r.y as usize + row) * self.extent.width as usize + r.x as usize;
            self.colour[at * 12..(at + w) * 12].copy_from_slice(&colour[row * w * 12..(row + 1) * w * 12]);
            self.alpha[at * 4..(at + w) * 4].copy_from_slice(&alpha[row * w * 4..(row + 1) * w * 4]);
        }
        Ok(())
    }
}
fn render(renderer: &Renderer, low: &TextRenderLowering, text: &PreparedText) -> Planes {
    let extent = low.lowering.plan.target.extent;
    let n = extent.width as usize * extent.height as usize;
    let mut sink = Planes { extent, colour: vec![0xAB; n * 12], alpha: vec![0xAB; n * 4] };
    let request = ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Exact, deadline: None, max_working_bytes: 64 << 20 };
    renderer
        .execute(&low.lowering.plan, &request, &Sources { low, text }, &CancellationToken::default(), &mut sink)
        .unwrap();
    sink
}
fn f32_at(b: &[u8], i: usize) -> f32 {
    f32::from_le_bytes([b[i * 4], b[i * 4 + 1], b[i * 4 + 2], b[i * 4 + 3]])
}

#[test]
fn cpu_renders_lowered_text_with_pinned_digest() {
    let text = prepared();
    let low = text_layer_render_plan(&text, LAYER, 7, target()).unwrap();
    let planes = render(&Renderer::new(16).unwrap(), &low, &text);
    // Independent expectation: Solid(fill) masked by Type's coverage, Normal over transparent:
    // alpha = fill alpha * coverage / 255, colour = fill where alpha > 0.
    let plane = text.plane(LAYER).unwrap();
    let p = plane.placement.unwrap();
    let mut max_err = 0.0f32;
    let mut ink = 0usize;
    for y in 0..64i64 {
        for x in 0..64i64 {
            let i = (y * 64 + x) as usize;
            let inside = x >= p.x && y >= p.y && x < p.x + i64::from(p.width) && y < p.y + i64::from(p.height);
            let cov = if inside {
                f32::from(plane.coverage[((y - p.y) * i64::from(p.width) + (x - p.x)) as usize]) / 255.0
            } else {
                0.0
            };
            let alpha = f32_at(&planes.alpha, i);
            max_err = max_err.max((alpha - FILL[3] * cov).abs());
            if cov > 0.0 {
                ink += 1;
                for c in 0..3 {
                    max_err = max_err.max((f32_at(&planes.colour, i * 3 + c) - FILL[c]).abs());
                }
            }
        }
    }
    println!("text_cpu max_err {max_err:e} over {ink} inked pixels");
    assert!(ink > 0);
    assert!(max_err <= 1.0e-6, "CPU vs coverage expectation {max_err:e}");
    // Same bytes for every chunking and thread count.
    for renderer in [Renderer::new(7).unwrap(), Renderer::with_threads(16, 4).unwrap()] {
        let other = render(&renderer, &low, &text);
        assert!(other.colour == planes.colour && other.alpha == planes.alpha, "chunking/threads changed bytes");
    }
    let mut bytes = planes.colour.clone();
    bytes.extend_from_slice(&planes.alpha);
    let digest = hex(&profile_hash(&bytes));
    println!("text_cpu digest={digest}");
    assert_eq!(digest, CPU_DIGEST, "CPU text render anchor");
}
