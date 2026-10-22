//! Key pipelines on the GPU backend (lane SMB-AE-KEY-TRACK-3): `key.wgsl` (binary32) against
//! pigment's `keying::run_key_pipeline` (binary64) composited with `blend_math::composite_over`,
//! which render-cpu reproduces bit for bit (render-cpu tests/cpu_keying.rs). Tolerance: 1e-4 on
//! alpha and premultiplied colour (ramps divide by a softness, so the binary32 distance error is
//! amplified by up to 1/softness); measured values are printed. GPU-execution proofs follow
//! `tests/gpu-proof.toml` (V4-03); the refusal test needs no adapter.
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over};
use hsk_studio_pigment::keying::*;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BlendSpace, ColourTag, Exactness, ExecuteRequest, ExtOperator, Group, Layer,
    LayerExtension, NoVector, OutputTarget, PlanExtensions, PlanNode, PlanNodeId, PlanSources, RenderBackend, RenderPlan,
    RenderReceipt, SampleFormat, SinkError, SourceKey, StudioBlendMode, TileInput, TilePlanes, TileSink,
};
use hsk_studio_pigment::retouch::RgbaPlane;
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_render_gpu::{DeviceOptions, GpuBackend, Reason};
use std::sync::OnceLock;

const TOLERANCE: f32 = 1e-4;
const W: u32 = 24;
const H: u32 = 20;
const BACKDROP: [f32; 4] = [0.8, 0.3, 0.6, 1.0];
const NORMAL: StudioBlendMode = StudioBlendMode::Normal;
const TILE_W: u32 = 20;
const TILE_H: u32 = 16;

fn rect(x: i64, y: i64, width: u32, height: u32) -> Rect {
    Rect { x, y, width, height }
}
fn tag() -> ColourTag {
    ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::LinearLight }
}
/// A green screen with a warm subject disc and spill on its rim (render-cpu cpu_keying fixture).
fn tile_px(x: u32, y: u32) -> [f32; 4] {
    let (dx, dy) = (x as f32 - 9.5, y as f32 - 7.5);
    let d = (dx * dx + dy * dy).sqrt();
    if d < 4.5 {
        [0.85, 0.55 + 0.02 * (x % 3) as f32, 0.4, 1.0]
    } else if d < 6.0 {
        [0.5, 0.75, 0.35, 1.0]
    } else {
        [0.1 + 0.01 * (y % 4) as f32, 0.85, 0.12, 1.0]
    }
}
struct Tile {
    colour: Vec<u8>,
    alpha: Vec<u8>,
}
impl Tile {
    fn new() -> Self {
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for y in 0..TILE_H {
            for x in 0..TILE_W {
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
            width: TILE_W,
            height: TILE_H,
            colour: &self.colour,
            colour_stride_bytes: u64::from(TILE_W) * 12,
            alpha: &self.alpha,
            alpha_stride_bytes: u64::from(TILE_W) * 4,
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
fn layer(input: u32, opacity: f32) -> Layer {
    Layer { input: PlanNodeId(input), mode: NORMAL, opacity, fill_opacity: 1.0, mask: None }
}
/// 0: backdrop; 1: the tile at (2, 2); 2: root [backdrop, tile 0.85].
fn plan(tile: &Tile) -> RenderPlan<NoVector> {
    let extent = rect(0, 0, W, H);
    RenderPlan {
        revision: 7,
        target: OutputTarget { extent, format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight, space: tag() },
        nodes: vec![
            PlanNode::Solid { rect: extent, rgba: BACKDROP },
            PlanNode::Tile(TileInput {
                key: SourceKey(1),
                placement: rect(2, 2, TILE_W, TILE_H),
                format: SampleFormat::RgbF32Le,
                alpha: AlphaConvention::Straight,
                colour_sha256: hsk_studio_prism::profile_hash(&tile.colour),
                alpha_sha256: hsk_studio_prism::profile_hash(&tile.alpha),
            }),
            PlanNode::Group(Group { isolated: true, layers: vec![layer(0, 1.0), layer(1, 0.85)] }),
        ],
        root: PlanNodeId(2),
    }
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
        Some(Reason::NoAdapter) if strict => panic!("NOT_PROVEN: no adapter; {what} requires a GPU (tests/gpu-proof.toml)"),
        Some(Reason::NoAdapter) => {
            eprintln!("NOT_PROVEN: no adapter; {what} not run");
            false
        }
        other => panic!("{what}: no live device ({other:?})"),
    }
}
fn execute(plan: &RenderPlan<NoVector>, ext: &PlanExtensions, tile: &Tile) -> (Result<RenderReceipt, BackendError>, Image) {
    let mut image = Image { px: vec![[f32::NAN; 4]; (W * H) as usize], calls: 0 };
    let request = ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 30 };
    let r = backend().execute_extended(plan, ext, &request, tile, &CancellationToken::default(), &mut image);
    (r, image)
}
fn over(b: [f32; 4], s: [f32; 4], opacity: f32, x: i64, y: i64) -> [f32; 4] {
    if opacity <= 0.0 || s[3] <= 0.0 {
        return b;
    }
    let o = composite_over(NORMAL, Rgba::new([b[0], b[1], b[2]], b[3]), Rgba::new([s[0], s[1], s[2]], s[3]), opacity, PixelSite { x, y, seed: 7 }).unwrap();
    [o.rgb[0], o.rgb[1], o.rgb[2], o.a.min(1.0)]
}
/// Pigment's key of the tile composited by pigment (render-cpu's bytes).
fn expected(stages: &[KeyStage]) -> Vec<[f32; 4]> {
    let mut data = Vec::new();
    for y in 0..TILE_H {
        for x in 0..TILE_W {
            data.push(tile_px(x, y));
        }
    }
    let keyed = run_key_pipeline(&RgbaPlane::new(rect(2, 2, TILE_W, TILE_H), data).unwrap(), stages, None, &KeyLimits::DEFAULT).unwrap().colour;
    let mut want = Vec::new();
    for y in 0..i64::from(H) {
        for x in 0..i64::from(W) {
            let b = over([0.0; 4], BACKDROP, 1.0, x, y);
            let inside = (2..2 + i64::from(TILE_W)).contains(&x) && (2..2 + i64::from(TILE_H)).contains(&y);
            let s = if inside { keyed.data[((y - 2) * i64::from(TILE_W) + (x - 2)) as usize] } else { [0.0; 4] };
            want.push(over(b, s, 0.85, x, y));
        }
    }
    want
}
fn deviation(got: &[[f32; 4]], want: &[[f32; 4]]) -> f32 {
    let pre = |p: [f32; 4]| [p[0] * p[3], p[1] * p[3], p[2] * p[3], p[3]];
    got.iter().zip(want).fold(0.0f32, |m, (g, w)| {
        let (g, w) = (pre(*g), pre(*w));
        (0..4).fold(m, |m, c| m.max((g[c] - w[c]).abs()))
    })
}

fn chains() -> Vec<(&'static str, Vec<KeyStage>)> {
    let r = MatteCombine::Replace;
    vec![
        (
            "screen",
            vec![
                KeyStage::Chroma(ChromaKey { key: [0.1, 0.85, 0.12], space: ChromaSpace::YCbCr, tolerance: 0.05, softness: 0.15, luma_weight: 0.2 }, r),
                KeyStage::ChokeSpread { radius_px: -1 },
                KeyStage::EdgeSmooth { radius_px: 1, colour_sigma: 0.3 },
                KeyStage::Feather { radius_px: 1.0 },
                KeyStage::Despill(Despill { screen: SpillScreen::Green, limit: SpillLimit::Average, amount: 1.0, preserve_luminance: true }),
            ],
        ),
        (
            "lab_range_levels_spread",
            vec![
                KeyStage::Chroma(ChromaKey { key: [0.1, 0.85, 0.12], space: ChromaSpace::Lab, tolerance: 0.1, softness: 0.4, luma_weight: 0.5 }, r),
                KeyStage::ColourRange(
                    ColourRangeKey { space: RangeSpace::Lab, add: vec![[0.5, 0.75, 0.35]], subtract: vec![[0.85, 0.56, 0.4]], fuzziness: 0.3 },
                    MatteCombine::Min,
                ),
                KeyStage::Levels { black: 0.1, white: 0.9 },
                KeyStage::ChokeSpread { radius_px: 2 },
                KeyStage::Despill(Despill { screen: SpillScreen::Green, limit: SpillLimit::Max, amount: 0.5, preserve_luminance: false }),
            ],
        ),
        (
            "extractors",
            vec![
                KeyStage::ColourDifference(ColourDifferenceKey { screen: SpillScreen::Green, a_black: 0.05, a_white: 0.6, b_black: 0.1, b_white: 0.7 }, r),
                KeyStage::LinearColour(LinearColourKey { key: [0.1, 0.85, 0.12], matching: LinearMatch::Hue, tolerance: 0.02, softness: 0.2, keep: false }, MatteCombine::Multiply),
                KeyStage::LinearColour(LinearColourKey { key: [0.1, 0.85, 0.12], matching: LinearMatch::Chroma, tolerance: 0.02, softness: 0.3, keep: false }, MatteCombine::Max),
                KeyStage::Extract(Extract { channel: ExtractChannel::Luminance, black: 0.2, white: 0.9, black_softness: 0.1, white_softness: 0.05, invert: false }, MatteCombine::Multiply),
                KeyStage::Luma(LumaKey { kind: LumaKeyType::KeyOutSimilar, threshold: 0.66, tolerance: 0.01, softness: 0.2 }, MatteCombine::Max),
                KeyStage::ColourKey(ColourKey { key: [0.5, 0.75, 0.35], tolerance: 0.01 }, MatteCombine::Min),
                KeyStage::Unmult,
            ],
        ),
    ]
}

#[test]
fn gpu_key_pipelines_match_pigment_within_tolerance() {
    if !gate("gpu_key_pipelines_match_pigment_within_tolerance") {
        return;
    }
    let tile = Tile::new();
    let plan = plan(&tile);
    let mut worst = 0.0f32;
    for (name, stages) in chains() {
        let ext = PlanExtensions::from_layers(vec![LayerExtension::new(PlanNodeId(2), 1).with_keys(stages.clone())]);
        let (result, image) = execute(&plan, &ext, &tile);
        let receipt = result.unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!(receipt.exactness.lowest, Exactness::Approximate);
        assert_eq!(receipt.plan_digest, ext.digest_with(&plan).unwrap(), "{name}: the receipt binds the original plan");
        let d = deviation(&image.px, &expected(&stages));
        println!("gpu key {name}: max |delta| (alpha, premultiplied) {d:e}");
        worst = worst.max(d);
        assert!(d <= TOLERANCE, "{name}: {d:e}");
    }
    println!("gpu key worst: {worst:e}");
}

#[test]
fn gpu_key_refusals_are_typed_before_any_device_work() {
    let tile = Tile::new();
    let plan = plan(&tile);
    let refused = |stages: Vec<KeyStage>, operator: &str| {
        let ext = PlanExtensions::from_layers(vec![LayerExtension::new(PlanNodeId(2), 1).with_keys(stages)]);
        let (result, image) = execute(&plan, &ext, &tile);
        assert!(matches!(&result, Err(BackendError::Unsupported { operator: o, .. }) if *o == operator), "{operator}: {result:?}");
        assert_eq!(image.calls, 0);
    };
    let path = vec![[0.0, 0.0], [9.0, 0.0], [9.0, 9.0]];
    refused(vec![KeyStage::InnerOuter(InnerOuterKey { inner: path.clone(), outer: path }, MatteCombine::Replace)], "key_inner_outer");
    refused(vec![KeyStage::KeyCleaner(KeyCleaner { edge_radius_px: 2, alpha_contrast: 1.0 })], "matte_key_cleaner");
    refused(vec![KeyStage::ChokeSpread { radius_px: 40 }], "key_gpu_radius");
    refused(vec![KeyStage::EdgeSmooth { radius_px: 17, colour_sigma: 0.2 }], "key_gpu_radius");
    // A keyed layer whose input is not a tile.
    let mut solid = plan.clone();
    solid.nodes[1] = PlanNode::Solid { rect: rect(2, 2, 4, 4), rgba: [0.1, 0.8, 0.1, 1.0] };
    let ext = PlanExtensions::from_layers(vec![LayerExtension::new(PlanNodeId(2), 1).with_keys(vec![KeyStage::Levels { black: 0.1, white: 0.9 }])]);
    let (result, image) = execute(&solid, &ext, &tile);
    assert!(matches!(&result, Err(BackendError::Unsupported { operator: "key_gpu_input", .. })), "{result:?}");
    assert_eq!(image.calls, 0);
    for kind in [KeyKind::Chroma, KeyKind::Feather, KeyKind::Unmult] {
        assert_eq!(backend().extension_exactness(ExtOperator::Key(kind)), Exactness::Approximate);
    }
    assert_eq!(backend().extension_exactness(ExtOperator::Key(KeyKind::WireRemoval)), Exactness::Unsupported);
}
