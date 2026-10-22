//! Key pipelines on the CPU backend (lane SMB-AE-KEY-TRACK-2). The expected pixels are composed
//! from pigment's single owners (`keying::run_key_pipeline` called directly on the layer content,
//! `blend_math::composite_over`), never by the code under test, and must match bit for bit for
//! every chunking and thread count. After Effects parity is UNVERIFIED (class Approximate).
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over};
use hsk_studio_pigment::blur_math::{BlurScope, LayerBlur};
use hsk_studio_pigment::filter_math::LayerFilter;
use hsk_studio_pigment::keying::*;
use hsk_studio_pigment::render_plan::*;
use hsk_studio_pigment::retouch::RgbaPlane;
use hsk_studio_render_cpu::Renderer;

mod common;
use common::*;

const NORMAL: StudioBlendMode = StudioBlendMode::Normal;
const W: u32 = 24;
const H: u32 = 20;
const BACKDROP: [f32; 4] = [0.8, 0.3, 0.6, 1.0];
const OPACITY: f32 = 0.85;
const TILE_AT: (i64, i64) = (2, 2);
const TILE_W: u32 = 20;
const TILE_H: u32 = 16;
const CHUNKINGS: [(u32, usize); 6] = [(1, 1), (3, 1), (5, 4), (64, 1), (4, 8), (7, 2)];

/// A green screen with a warm subject disc, green spill on the subject's rim and a soft edge.
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

fn stages() -> Vec<KeyStage> {
    vec![
        KeyStage::Chroma(
            ChromaKey { key: [0.1, 0.85, 0.12], space: ChromaSpace::YCbCr, tolerance: 0.05, softness: 0.15, luma_weight: 0.2 },
            MatteCombine::Replace,
        ),
        KeyStage::ChokeSpread { radius_px: -1 },
        KeyStage::EdgeSmooth { radius_px: 1, colour_sigma: 0.3 },
        KeyStage::Feather { radius_px: 1.0 },
        KeyStage::Despill(Despill { screen: SpillScreen::Green, limit: SpillLimit::Average, amount: 1.0, preserve_luminance: true }),
    ]
}

/// 0: opaque backdrop; 1: the tile; 2: root group [backdrop, tile (normal, 0.85)].
fn key_plan(tile: &TileData) -> RenderPlan {
    let extent = rect(0, 0, W, H);
    plan(
        extent,
        vec![
            solid(extent, BACKDROP),
            PlanNode::Tile(tile.input_at(1, TILE_AT.0, TILE_AT.1)),
            group(vec![layer(0, NORMAL, 1.0), layer(1, NORMAL, OPACITY)]),
        ],
        2,
    )
}

fn over(b: [f32; 4], s: [f32; 4], opacity: f32, x: i64, y: i64) -> [f32; 4] {
    if opacity <= 0.0 || s[3] <= 0.0 {
        return b;
    }
    let out = composite_over(NORMAL, Rgba::new([b[0], b[1], b[2]], b[3]), Rgba::new([s[0], s[1], s[2]], s[3]), opacity, PixelSite { x, y, seed: 7 })
        .unwrap();
    [out.rgb[0], out.rgb[1], out.rgb[2], out.a.min(1.0)]
}

/// Pigment's key of the tile plane composited by pigment's `composite_over`, the layer mask after
/// the key.
fn expected(stages: &[KeyStage], layer_mask: Option<fn(u32, u32) -> u8>) -> Vec<[u32; 4]> {
    let mut data = Vec::new();
    for y in 0..TILE_H {
        for x in 0..TILE_W {
            data.push(tile_px(x, y));
        }
    }
    let content = rect(TILE_AT.0, TILE_AT.1, TILE_W, TILE_H);
    let keyed = run_key_pipeline(&RgbaPlane::new(content, data).unwrap(), stages, None, &KeyLimits::DEFAULT).unwrap().colour;
    let mut want = Vec::new();
    for y in 0..i64::from(H) {
        for x in 0..i64::from(W) {
            let b = over([0.0; 4], BACKDROP, 1.0, x, y);
            let inside = x >= content.x && y >= content.y && x < content.x + i64::from(TILE_W) && y < content.y + i64::from(TILE_H);
            let s = if inside { keyed.data[((y - content.y) * i64::from(TILE_W) + (x - content.x)) as usize] } else { [0.0; 4] };
            let cov = layer_mask.map_or(1.0, |m| (f64::from(m(x as u32, y as u32)) / 255.0) as f32);
            want.push(over(b, s, OPACITY * cov, x, y).map(f32::to_bits));
        }
    }
    want
}

fn bits(grid: &Grid) -> Vec<[u32; 4]> {
    grid.px.iter().map(|p| p.map(f32::to_bits)).collect()
}

/// Byte identity with a direct pigment call for every chunking and thread count, with and without
/// a layer mask applied after the key; the key changes the image.
#[test]
fn key_matches_pigment_for_every_chunking_and_thread_count() {
    let tile = TileData::new(TILE_W, TILE_H, tile_px);
    let lmask_fn: fn(u32, u32) -> u8 = |x, y| ((x * 23 + y * 31) % 256) as u8;
    let lmask = MaskData::u8(W, H, lmask_fn);
    for masked in [false, true] {
        let sources = Sources { tiles: vec![(1, tile.planes())], masks: if masked { vec![(8, &lmask)] } else { vec![] } };
        let mut plan = key_plan(&tile);
        if let (true, PlanNode::Group(g)) = (masked, &mut plan.nodes[2]) {
            g.layers[1].mask = Some(lmask.input(8, false, 1.0));
        }
        let ext = envelope(vec![one_layer(2, 1).with_keys(stages())]);
        let want = expected(&stages(), masked.then_some(lmask_fn));
        for (chunk, threads) in CHUNKINGS {
            let renderer = Renderer::with_threads(chunk, threads).unwrap();
            let (result, grid) = run_ext(&renderer, &plan, &ext, Exactness::Approximate, 1 << 28, &sources);
            let receipt = result.unwrap_or_else(|e| panic!("masked {masked} chunk {chunk} x{threads}: {e:?}"));
            assert_eq!(receipt.exactness.lowest, Exactness::Approximate);
            assert_eq!(bits(&grid), want, "masked {masked} chunk {chunk} x{threads}");
        }
        let (plain, grid) = run_ext(&Renderer::with_threads(8, 1).unwrap(), &plan, &envelope(vec![]), Exactness::Approximate, 1 << 28, &sources);
        plain.unwrap();
        assert_ne!(bits(&grid), want, "the key changed nothing");
        // The screen is keyed out: the backdrop shows through at a screen pixel.
        let (_, grid) = run_ext(&Renderer::with_threads(8, 1).unwrap(), &plan, &ext, Exactness::Approximate, 1 << 28, &sources);
        assert_eq!(grid.at(3, 3), over([0.0; 4], BACKDROP, 1.0, 3, 3));
    }
}

/// Typed refusals before any sink call: pigment's admission (difference matte, parameter out of
/// range, combinations, extended range, an Approximate key under a Fitted floor), render-cpu's own
/// (a keyed layer whose content holds another keyed layer) and the budget charge.
#[test]
fn key_refusals_and_budgets_are_typed_before_any_sink_call() {
    let tile = TileData::new(TILE_W, TILE_H, tile_px);
    let sources = Sources { tiles: vec![(1, tile.planes())], masks: vec![] };
    let plan = key_plan(&tile);
    let renderer = Renderer::with_threads(5, 2).unwrap();
    let check = |what: &str, plan: &RenderPlan, ext: PlanExtensions, min: Exactness, budget: u64, want: Result<(), BackendError>| {
        let (result, grid) = run_ext(&renderer, plan, &ext, min, budget, &sources);
        assert_eq!(result.map(|_| ()), want, "{what}");
        assert_eq!(grid.calls, 0, "{what}: sink touched");
    };
    let a = Exactness::Approximate;
    let big = 1 << 28;
    let param = Err(BackendError::InvalidPlan(PlanError::KeyParameter { node: 2 }));
    let combo = Err(BackendError::InvalidPlan(PlanError::KeyCombination { node: 2 }));
    let entry = |keys: Vec<KeyStage>| one_layer(2, 1).with_keys(keys);
    let diff = KeyStage::Difference(DifferenceMatte { tolerance: 0.1, softness: 0.1, blur_px: 0.0 }, MatteCombine::Replace);
    check("difference has no clean plate", &plan, envelope(vec![entry(vec![diff])]), a, big, param);
    check("NaN", &plan, envelope(vec![entry(vec![KeyStage::Feather { radius_px: f32::NAN }])]), a, big, param);
    check("too many", &plan, envelope(vec![entry(vec![KeyStage::Levels { black: 0.0, white: 1.0 }; MAX_KEY_STAGES + 1])]), a, big, param);
    check("with filters", &plan, envelope(vec![entry(stages()).with_filters(vec![LayerFilter::Median { radius_px: 1 }])]), a, big, combo);
    check("with blur", &plan, envelope(vec![entry(stages()).with_blur(LayerBlur::new(BlurScope::Layer, 2.0))]), a, big, combo);
    let extended = envelope(vec![entry(stages())]).with_output(OutputStage::new(SampleRange::Extended, None));
    check("extended range", &plan, extended, a, big, combo);
    check(
        "fitted floor",
        &plan,
        envelope(vec![entry(stages())]),
        Exactness::Fitted,
        big,
        Err(BackendError::Unsupported { operator: "key_chroma", exactness: Exactness::Approximate }),
    );
    // A keyed layer whose content is a group holding a keyed layer.
    let extent = rect(0, 0, W, H);
    let nested = plan_nodes(extent, &tile);
    let ext = envelope(vec![one_layer(2, 0).with_keys(stages()), one_layer(3, 1).with_keys(stages())]);
    check("nested", &nested, ext, a, big, Err(BackendError::Unsupported { operator: "key_nested", exactness: Exactness::Unsupported }));
    // Budget: the charge is content + stored output + pigment's predicted pipeline bytes.
    let ext = envelope(vec![entry(stages())]);
    let (ok, _) = run_ext(&renderer, &plan, &ext, a, big, &sources);
    let peak = ok.unwrap().peak_working_bytes;
    let (plain, _) = run_ext(&renderer, &plan, &envelope(vec![]), a, big, &sources);
    let plain_peak = plain.unwrap().peak_working_bytes;
    let n: u64 = 20 * 16;
    let pigment = key_pipeline_cost(n, &stages()).unwrap().0;
    assert_eq!(peak - plain_peak, n * 16 + n * 16 + pigment + 5 * 64, "key charge");
    let (under, grid) = run_ext(&renderer, &plan, &ext, a, peak - 1, &sources);
    assert!(matches!(under, Err(BackendError::BudgetExceeded { .. })), "{under:?}");
    assert_eq!(grid.calls, 0);
}

/// 0: backdrop; 1: tile; 2: inner group [tile keyed]; 3: root [backdrop, inner group keyed].
fn plan_nodes(extent: hsk_studio_pigment::Rect, tile: &TileData) -> RenderPlan {
    plan(
        extent,
        vec![
            solid(extent, BACKDROP),
            PlanNode::Tile(tile.input_at(1, TILE_AT.0, TILE_AT.1)),
            group(vec![layer(1, NORMAL, 1.0)]),
            group(vec![layer(0, NORMAL, 1.0), layer(2, NORMAL, 1.0)]),
        ],
        3,
    )
}
