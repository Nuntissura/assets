//! Layer blur and backdrop blur on the CPU backend (lane SMB-COMPOSITE-NEW-KINDS slice 1). The
//! expected pixels are composed from pigment's single owners (`blur_math::blur_window` over the
//! whole layer, `blur_math::backdrop_mix`, `blend_math::composite_over`), never by the code under
//! test, and must match bit for bit for every chunking and thread count (the windows are uncut).
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over};
use hsk_studio_pigment::blur_math::*;
use hsk_studio_pigment::render_plan::*;
use hsk_studio_render_cpu::Renderer;

mod common;
use common::*;

const NORMAL: StudioBlendMode = StudioBlendMode::Normal;
const WIDE: BlurLimits = BlurLimits { max_bytes: 1 << 30, max_work: 1 << 40 };
const W: u32 = 20;
const H: u32 = 16;

/// A ramp with transparent holes and partial alpha (colour kept where alpha is 0).
fn tile_px(x: u32, y: u32) -> [f32; 4] {
    let hole = (x * 3 + y * 5).is_multiple_of(7);
    [
        (x as f32 / 8.0).min(1.0),
        (y as f32 / 6.0).min(1.0),
        ((x + y) % 3) as f32 / 2.0,
        if hole { 0.0 } else { ((x + y) % 4 + 1) as f32 / 4.0 },
    ]
}

/// 0: opaque backdrop solid over the extent; 1: the 9x7 tile at (4, 3); 2: a half-transparent
/// white solid at (6, 5) 8x6 (the "glass"); 3: root group [backdrop, tile (multiply 0.9), glass].
fn blur_plan(tile: &TileData) -> RenderPlan {
    let extent = rect(0, 0, W, H);
    plan(
        extent,
        vec![
            solid(extent, [0.2, 0.5, 0.8, 1.0]),
            PlanNode::Tile(tile.input_at(1, 4, 3)),
            solid(rect(6, 5, 8, 6), [1.0, 1.0, 1.0, 0.5]),
            group(vec![
                layer(0, NORMAL, 1.0),
                layer(1, StudioBlendMode::Multiply, 0.9),
                layer(2, NORMAL, 0.8),
            ]),
        ],
        3,
    )
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

fn tile_plane() -> (Vec<[f32; 4]>, hsk_studio_pigment::Rect) {
    let mut v = Vec::new();
    for y in 0..7 {
        for x in 0..9 {
            v.push(tile_px(x, y));
        }
    }
    (v, rect(4, 3, 9, 7))
}

fn glass(x: i64, y: i64) -> [f32; 4] {
    if (6..14).contains(&x) && (5..11).contains(&y) { [1.0, 1.0, 1.0, 0.5] } else { [0.0; 4] }
}

/// The expected image: `layer_blur` blurs the tile layer, `backdrop` gives the glass a backdrop
/// blur (pigment maths only).
fn expected(layer_radius: Option<f32>, backdrop_radius: Option<f32>) -> Vec<[u32; 4]> {
    let extent = rect(0, 0, W, H);
    let (plane, at) = tile_plane();
    let tile_blur = layer_radius.map(|r| {
        let taps = LayerBlur::new(BlurScope::Layer, r).taps().unwrap();
        blur_window(&plane, at, extent, &taps, WIDE).unwrap()
    });
    let mut below = Vec::new();
    for y in 0..H as i64 {
        for x in 0..W as i64 {
            let b = over(NORMAL, [0.0; 4], [0.2, 0.5, 0.8, 1.0], 1.0, x, y);
            let s = match &tile_blur {
                Some(blurred) => blurred[(y * W as i64 + x) as usize],
                None if (4..13).contains(&x) && (3..10).contains(&y) => tile_px((x - 4) as u32, (y - 3) as u32),
                None => [0.0; 4],
            };
            below.push(over(StudioBlendMode::Multiply, b, s, 0.9, x, y));
        }
    }
    let back_blur = backdrop_radius.map(|r| {
        let taps = LayerBlur::new(BlurScope::Backdrop, r).taps().unwrap();
        blur_window(&below, extent, extent, &taps, WIDE).unwrap()
    });
    let mut want = Vec::new();
    for y in 0..H as i64 {
        for x in 0..W as i64 {
            let i = (y * W as i64 + x) as usize;
            let mut b = below[i];
            let g = glass(x, y);
            if let Some(blurred) = &back_blur
                && g[3] > 0.0
            {
                b = backdrop_mix(b, blurred[i], g[3] * 0.8).unwrap();
            }
            want.push(over(NORMAL, b, g, 0.8, x, y).map(f32::to_bits));
        }
    }
    want
}

fn bits(grid: &Grid) -> Vec<[u32; 4]> {
    grid.px.iter().map(|p| p.map(f32::to_bits)).collect()
}

const CHUNKINGS: [(u32, usize); 5] = [(1, 1), (3, 1), (5, 4), (64, 1), (4, 8)];

#[test]
fn layer_blur_matches_pigment_for_every_chunking_and_thread_count() {
    let tile = TileData::new(9, 7, tile_px);
    let sources = Sources { tiles: vec![(1, tile.planes())], masks: vec![] };
    let plan = blur_plan(&tile);
    for r in [1.0_f32, 2.5, 6.0] {
        let ext = envelope(vec![one_layer(3, 1).with_blur(LayerBlur::new(BlurScope::Layer, r))]);
        let want = expected(Some(r), None);
        for (chunk, threads) in CHUNKINGS {
            let renderer = Renderer::with_threads(chunk, threads).unwrap();
            let (result, grid) = run_ext(&renderer, &plan, &ext, Exactness::Approximate, 1 << 26, &sources);
            let receipt = result.unwrap();
            assert_eq!(receipt.exactness.lowest, Exactness::Approximate);
            assert_eq!(bits(&grid), want, "radius {r} chunk {chunk} x{threads}");
        }
    }
    // Radius 0 is no blur: the bytes of the plain plan.
    let renderer = Renderer::with_threads(5, 2).unwrap();
    let zero = envelope(vec![one_layer(3, 1).with_blur(LayerBlur::new(BlurScope::Layer, 0.0))]);
    let (a, with_zero) = run_ext(&renderer, &plan, &zero, Exactness::Approximate, 1 << 26, &sources);
    a.unwrap();
    assert_eq!(bits(&with_zero), expected(None, None));
}

#[test]
fn backdrop_blur_matches_pigment_for_every_chunking_and_thread_count() {
    let tile = TileData::new(9, 7, tile_px);
    let sources = Sources { tiles: vec![(1, tile.planes())], masks: vec![] };
    let plan = blur_plan(&tile);
    for r in [1.5_f32, 4.0] {
        let ext = envelope(vec![one_layer(3, 2).with_blur(LayerBlur::new(BlurScope::Backdrop, r))]);
        let want = expected(None, Some(r));
        for (chunk, threads) in CHUNKINGS {
            let renderer = Renderer::with_threads(chunk, threads).unwrap();
            let (result, grid) = run_ext(&renderer, &plan, &ext, Exactness::Approximate, 1 << 26, &sources);
            result.unwrap();
            assert_eq!(bits(&grid), want, "radius {r} chunk {chunk} x{threads}");
        }
    }
    // Both at once: the backdrop of the glass is the blurred tile layer.
    let ext = envelope(vec![
        one_layer(3, 1).with_blur(LayerBlur::new(BlurScope::Layer, 2.0)),
        one_layer(3, 2).with_blur(LayerBlur::new(BlurScope::Backdrop, 3.0)),
    ]);
    let (result, _) = run_ext(&Renderer::with_threads(4, 2).unwrap(), &plan, &ext, Exactness::Approximate, 1 << 26, &sources);
    // A blurred layer below a backdrop blur needs a second frame: typed, before any sink call.
    assert!(matches!(result, Err(BackendError::Unsupported { operator: "blur_nested", .. })), "{result:?}");
}

#[test]
fn blur_refusals_and_budget_are_typed_before_any_sink_call() {
    let tile = TileData::new(9, 7, tile_px);
    let sources = Sources { tiles: vec![(1, tile.planes())], masks: vec![] };
    let plan = blur_plan(&tile);
    let renderer = Renderer::with_threads(8, 1).unwrap();
    // Backdrop blur in a pass-through group: pigment's validation refuses it at admission.
    let mut pass = blur_plan(&tile);
    pass.nodes[3] = PlanNode::Group(Group {
        isolated: false,
        layers: vec![layer(0, NORMAL, 1.0), layer(1, StudioBlendMode::Multiply, 0.9), layer(2, NORMAL, 0.8)],
    });
    pass.nodes.push(group(vec![layer(3, StudioBlendMode::PassThrough, 1.0)]));
    pass.root = PlanNodeId(4);
    let back = envelope(vec![one_layer(3, 2).with_blur(LayerBlur::new(BlurScope::Backdrop, 2.0))]);
    let (result, grid) = run_ext(&renderer, &pass, &back, Exactness::Approximate, 1 << 26, &sources);
    assert_eq!(result.unwrap_err(), BackendError::InvalidPlan(PlanError::BlurCombination { node: 3 }));
    assert_eq!(grid.calls, 0);
    // Below the class Approximate the operator is refused under its key.
    let layer_blur = envelope(vec![one_layer(3, 1).with_blur(LayerBlur::new(BlurScope::Layer, 3.0))]);
    let (result, grid) = run_ext(&renderer, &plan, &layer_blur, Exactness::Exact, 1 << 26, &sources);
    assert!(matches!(result, Err(BackendError::Unsupported { operator: "blur_layer", .. })), "{result:?}");
    assert_eq!(grid.calls, 0);
    // The frame and pigment's predicted blur bytes are charged before the first sink call: find
    // the charge of a run, then one byte less is BudgetExceeded with no sink call.
    let (ok, _) = run_ext(&renderer, &plan, &layer_blur, Exactness::Approximate, 1 << 26, &sources);
    let peak = ok.unwrap().peak_working_bytes;
    let (result, grid) = run_ext(&renderer, &plan, &layer_blur, Exactness::Approximate, peak - 1, &sources);
    assert!(matches!(result, Err(BackendError::BudgetExceeded { .. })), "{result:?}");
    assert_eq!(grid.calls, 0);
    // A blur inside the content of a blurred layer has no frame left: typed, no sink call.
    let mut nested = blur_plan(&tile);
    nested.nodes.push(group(vec![layer(3, NORMAL, 1.0)]));
    nested.root = PlanNodeId(4);
    let ext = envelope(vec![
        one_layer(3, 1).with_blur(LayerBlur::new(BlurScope::Layer, 2.0)),
        one_layer(4, 0).with_blur(LayerBlur::new(BlurScope::Layer, 2.0)),
    ]);
    let (result, grid) = run_ext(&renderer, &nested, &ext, Exactness::Approximate, 1 << 26, &sources);
    assert!(matches!(result, Err(BackendError::Unsupported { operator: "blur_nested", .. })), "{result:?}");
    assert_eq!(grid.calls, 0);
}
