//! Layer blur and backdrop blur on the GPU backend (lane SMB-COMPOSITE-NEW-KINDS slice 1): the
//! `blur.wgsl` passes (binary32) against the composition of pigment's single owners
//! (`blur_math::blur_window` binary64 per pass, `blur_math::backdrop_mix`,
//! `blend_math::composite_over`), which render-cpu reproduces bit for bit (render-cpu
//! tests/cpu_blur.rs). GPU-execution proofs follow `tests/gpu-proof.toml` (V4-03): without an adapter
//! they FAIL with `NOT_PROVEN: no adapter` while `require_gpu = true`. The admission test is
//! hardware-independent.
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over};
use hsk_studio_pigment::blur_math::*;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BlendSpace, ColourTag, Exactness, ExecuteRequest, ExtOperator,
    Group, Layer, LayerExtension, NoVector, OutputTarget, PlanError, PlanExtensions, PlanNode,
    PlanNodeId, PlanSources, RenderBackend, RenderPlan, RenderReceipt, SampleFormat, SinkError,
    SourceKey, StudioBlendMode, TileInput, TilePlanes, TileSink,
};
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_render_gpu::{DeviceOptions, GpuBackend, Reason};
use std::sync::OnceLock;

/// Tolerance on alpha and premultiplied colour: the blur sums up to 3001 taps per pass in binary32
/// where pigment sums in binary64 (measured values are printed by the tests).
const TOLERANCE: f32 = 1e-5;
const NORMAL: StudioBlendMode = StudioBlendMode::Normal;
const WIDE: BlurLimits = BlurLimits { max_bytes: 1 << 30, max_work: 1 << 40 };
const W: u32 = 20;
const H: u32 = 16;

fn rect(x: i64, y: i64, width: u32, height: u32) -> Rect {
    Rect { x, y, width, height }
}
fn tag() -> ColourTag {
    ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::LinearLight }
}
/// The render-cpu cpu_blur fixture tile (9x7 at (4, 3)).
fn tile_px(x: u32, y: u32) -> [f32; 4] {
    let hole = (x * 3 + y * 5).is_multiple_of(7);
    [
        (x as f32 / 8.0).min(1.0),
        (y as f32 / 6.0).min(1.0),
        ((x + y) % 3) as f32 / 2.0,
        if hole { 0.0 } else { ((x + y) % 4 + 1) as f32 / 4.0 },
    ]
}
struct Tile {
    colour: Vec<u8>,
    alpha: Vec<u8>,
}
impl Tile {
    fn new() -> Self {
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for y in 0..7 {
            for x in 0..9 {
                let p = tile_px(x, y);
                for c in &p[..3] {
                    colour.extend_from_slice(&c.to_le_bytes());
                }
                alpha.extend_from_slice(&p[3].to_le_bytes());
            }
        }
        Self { colour, alpha }
    }
}
impl PlanSources for Tile {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        (key.0 == 1).then(|| TilePlanes {
            width: 9,
            height: 7,
            colour: &self.colour,
            colour_stride_bytes: 9 * 12,
            alpha: &self.alpha,
            alpha_stride_bytes: 9 * 4,
            colour_sha256: hsk_studio_prism::profile_hash(&self.colour),
            alpha_sha256: hsk_studio_prism::profile_hash(&self.alpha),
            format: SampleFormat::RgbF32Le,
            alpha_convention: AlphaConvention::Straight,
            space: tag(),
        })
    }
    fn mask(&self, _: SourceKey) -> Option<Mask<'_>> {
        None
    }
}
struct Image {
    px: Vec<[f32; 4]>,
    calls: usize,
}
impl TileSink for Image {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let f = |b: &[u8], at: usize| f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        for y in 0..r.height as usize {
            for x in 0..r.width as usize {
                let i = y * r.width as usize + x;
                let at = (r.y as usize + y) * W as usize + r.x as usize + x;
                self.px[at] = [f(colour, i * 12), f(colour, i * 12 + 4), f(colour, i * 12 + 8), f(alpha, i * 4)];
            }
        }
        self.calls += 1;
        Ok(())
    }
}
fn layer(input: u32, mode: StudioBlendMode, opacity: f32) -> Layer {
    Layer { input: PlanNodeId(input), mode, opacity, fill_opacity: 1.0, mask: None }
}
/// The render-cpu cpu_blur plan: root [backdrop, tile (multiply 0.9), glass (0.8)].
fn blur_plan(tile: &Tile, isolated: bool) -> RenderPlan<NoVector> {
    let extent = rect(0, 0, W, H);
    let mut nodes = vec![
        PlanNode::Solid { rect: extent, rgba: [0.2, 0.5, 0.8, 1.0] },
        PlanNode::Tile(TileInput {
            key: SourceKey(1),
            placement: rect(4, 3, 9, 7),
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: hsk_studio_prism::profile_hash(&tile.colour),
            alpha_sha256: hsk_studio_prism::profile_hash(&tile.alpha),
        }),
        PlanNode::Solid { rect: rect(6, 5, 8, 6), rgba: [1.0, 1.0, 1.0, 0.5] },
        PlanNode::Group(Group {
            isolated,
            layers: vec![
                layer(0, NORMAL, 1.0),
                layer(1, StudioBlendMode::Multiply, 0.9),
                layer(2, NORMAL, 0.8),
            ],
        }),
    ];
    let mut root = 3;
    if !isolated {
        nodes.push(PlanNode::Group(Group {
            isolated: true,
            layers: vec![layer(3, StudioBlendMode::PassThrough, 1.0)],
        }));
        root = 4;
    }
    RenderPlan {
        revision: 7,
        target: OutputTarget { extent, format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight, space: tag() },
        nodes,
        root: PlanNodeId(root),
    }
}

fn over(mode: StudioBlendMode, b: [f32; 4], s: [f32; 4], opacity: f32, x: i64, y: i64) -> [f32; 4] {
    if opacity <= 0.0 || s[3] <= 0.0 {
        return b;
    }
    let out = composite_over(
        mode,
        Rgba::new([b[0], b[1], b[2]], b[3]),
        Rgba::new([s[0], s[1], s[2]], s[3]),
        opacity,
        PixelSite { x, y, seed: 7 },
    )
    .unwrap();
    [out.rgb[0], out.rgb[1], out.rgb[2], out.a.min(1.0)]
}

/// Pigment's composition (what render-cpu renders bit for bit).
fn expected(layer_radius: Option<f32>, backdrop_radius: Option<f32>) -> Vec<[f32; 4]> {
    let extent = rect(0, 0, W, H);
    let mut plane = Vec::new();
    for y in 0..7 {
        for x in 0..9 {
            plane.push(tile_px(x, y));
        }
    }
    let tile_blur = layer_radius.map(|r| {
        let taps = LayerBlur::new(BlurScope::Layer, r).taps().unwrap();
        blur_window(&plane, rect(4, 3, 9, 7), extent, &taps, WIDE).unwrap()
    });
    let mut below = Vec::new();
    for y in 0..H as i64 {
        for x in 0..W as i64 {
            let b = over(NORMAL, [0.0; 4], [0.2, 0.5, 0.8, 1.0], 1.0, x, y);
            let s = match &tile_blur {
                Some(t) => t[(y * W as i64 + x) as usize],
                None if (4..13).contains(&x) && (3..10).contains(&y) => tile_px((x - 4) as u32, (y - 3) as u32),
                None => [0.0; 4],
            };
            below.push(over(StudioBlendMode::Multiply, b, s, 0.9, x, y));
        }
    }
    let back = backdrop_radius.map(|r| {
        let taps = LayerBlur::new(BlurScope::Backdrop, r).taps().unwrap();
        blur_window(&below, extent, extent, &taps, WIDE).unwrap()
    });
    let mut want = Vec::new();
    for y in 0..H as i64 {
        for x in 0..W as i64 {
            let i = (y * W as i64 + x) as usize;
            let glass = if (6..14).contains(&x) && (5..11).contains(&y) { [1.0, 1.0, 1.0, 0.5] } else { [0.0; 4] };
            let mut b = below[i];
            if let Some(blurred) = &back
                && glass[3] > 0.0
            {
                b = backdrop_mix(b, blurred[i], glass[3] * 0.8).unwrap();
            }
            want.push(over(NORMAL, b, glass, 0.8, x, y));
        }
    }
    want
}

fn backend() -> &'static GpuBackend {
    static SHARED: OnceLock<GpuBackend> = OnceLock::new();
    SHARED.get_or_init(|| GpuBackend::new(DeviceOptions { chunk_side: 8, ..DeviceOptions::default() }))
}
fn gate(what: &str) -> bool {
    let b = backend();
    if b.device_info().is_some() {
        return true;
    }
    if let Some(failure) = b.failure() {
        panic!("{what}: adapter present but the backend failed: {failure:?}");
    }
    let strict = include_str!("gpu-proof.toml").lines().any(|l| l.trim() == "require_gpu = true")
        || std::env::var("HSK_STUDIO_TEST_REQUIRE_GPU").as_deref() == Ok("1");
    match b.unavailable_reason() {
        Some(Reason::NoAdapter) if strict => {
            panic!("NOT_PROVEN: no adapter; {what} requires a GPU (tests/gpu-proof.toml)")
        }
        Some(Reason::NoAdapter) => {
            eprintln!("NOT_PROVEN: no adapter; {what} not run");
            false
        }
        other => panic!("{what}: no live device ({other:?})"),
    }
}
fn request(min: Exactness) -> ExecuteRequest {
    ExecuteRequest { expected_revision: 7, min_exactness: min, deadline: None, max_working_bytes: 1 << 30 }
}
fn execute(plan: &RenderPlan<NoVector>, ext: &PlanExtensions, tile: &Tile, min: Exactness) -> (Result<RenderReceipt, BackendError>, Image) {
    let mut image = Image { px: vec![[f32::NAN; 4]; (W * H) as usize], calls: 0 };
    let result = backend().execute_extended(plan, ext, &request(min), tile, &CancellationToken::default(), &mut image);
    (result, image)
}
fn entry(layer: u32, scope: BlurScope, r: f32) -> LayerExtension {
    LayerExtension::new(PlanNodeId(3), layer).with_blur(LayerBlur::new(scope, r))
}
/// Largest |delta| of alpha and of premultiplied colour.
fn deviation(got: &[[f32; 4]], want: &[[f32; 4]]) -> f32 {
    let pre = |p: [f32; 4]| [p[0] * p[3], p[1] * p[3], p[2] * p[3], p[3]];
    got.iter()
        .zip(want)
        .flat_map(|(g, w)| {
            let (g, w) = (pre(*g), pre(*w));
            (0..4).map(move |c| (g[c] - w[c]).abs())
        })
        .fold(0.0, f32::max)
}

#[test]
fn gpu_layer_and_backdrop_blur_match_pigment_within_tolerance() {
    if !gate("gpu blur parity") {
        return;
    }
    let tile = Tile::new();
    let plan = blur_plan(&tile, true);
    type Case = (&'static str, Vec<LayerExtension>, Option<f32>, Option<f32>);
    let cases: [Case; 4] = [
        ("layer r1", vec![entry(1, BlurScope::Layer, 1.0)], Some(1.0), None),
        ("layer r6", vec![entry(1, BlurScope::Layer, 6.0)], Some(6.0), None),
        ("backdrop r1.5", vec![entry(2, BlurScope::Backdrop, 1.5)], None, Some(1.5)),
        ("backdrop r4", vec![entry(2, BlurScope::Backdrop, 4.0)], None, Some(4.0)),
    ];
    for (label, entries, layer_r, back_r) in cases {
        let ext = PlanExtensions::from_layers(entries);
        let (result, image) = execute(&plan, &ext, &tile, Exactness::Approximate);
        let receipt = result.unwrap_or_else(|e| panic!("{label}: {e:?}"));
        assert_eq!(receipt.exactness.lowest, Exactness::Approximate, "{label}");
        let want = expected(layer_r, back_r);
        let dev = deviation(&image.px, &want);
        println!("{label}: max |delta| alpha/premultiplied colour = {dev:e} (tolerance {TOLERANCE:e})");
        assert!(dev <= TOLERANCE, "{label}: deviation {dev:e}");
        // The blur really acts: the unblurred plan differs well beyond the tolerance.
        assert!(deviation(&image.px, &expected(None, None)) > 1e-3, "{label}: blur had no effect");
    }
}

#[test]
fn gpu_blur_admission_and_refusals_are_typed() {
    let tile = Tile::new();
    // Declared with pigment's class (lowered to Fitted like every float operator of this backend).
    assert_eq!(
        backend().extension_exactness(ExtOperator::Blur(BlurScope::Layer)),
        Exactness::Approximate
    );
    // A backdrop blur in a pass-through group: pigment's validation refuses it (no sink call).
    let pass = blur_plan(&tile, false);
    let ext = PlanExtensions::from_layers(vec![entry(2, BlurScope::Backdrop, 2.0)]);
    let (result, image) = execute(&pass, &ext, &tile, Exactness::Approximate);
    assert_eq!(result.unwrap_err(), BackendError::InvalidPlan(PlanError::BlurCombination { node: 3 }));
    assert_eq!(image.calls, 0);
    // Below the class Approximate the operator is refused under its key, before any device work.
    let plan = blur_plan(&tile, true);
    let ext = PlanExtensions::from_layers(vec![entry(1, BlurScope::Layer, 2.0)]);
    let (result, image) = execute(&plan, &ext, &tile, Exactness::Fitted);
    assert!(matches!(result, Err(BackendError::Unsupported { operator: "blur_layer", .. })), "{result:?}");
    assert_eq!(image.calls, 0);
}
