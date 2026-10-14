//! Layer filters on the CPU backend (lane SMB-RENDER-FILTERS slice 1). The expected pixels are
//! composed from pigment's single owners (`filter_math::filter_window` applied stage by stage to
//! the whole layer, `blend_math::composite_over`), never by the code under test, and must match
//! bit for bit for every chunking and thread count (the windows are uncut). Photoshop parity is
//! NOT_PROVEN (class Approximate).
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over};
use hsk_studio_pigment::blur_math::{BlurScope, LayerBlur};
use hsk_studio_pigment::filter_math::*;
use hsk_studio_pigment::render_plan::*;
use hsk_studio_render_cpu::Renderer;

mod common;
use common::*;

const NORMAL: StudioBlendMode = StudioBlendMode::Normal;
const WIDE: FilterLimits = FilterLimits { max_bytes: 1 << 30, max_work: 1 << 40 };
const W: u32 = 20;
const H: u32 = 16;

fn tile_px(x: u32, y: u32) -> [f32; 4] {
    let hole = (x * 3 + y * 5).is_multiple_of(7);
    [
        (x as f32 / 8.0).min(1.0),
        (y as f32 / 6.0).min(1.0),
        ((x + y) % 3) as f32 / 2.0,
        if hole { 0.0 } else { ((x + y) % 4 + 1) as f32 / 4.0 },
    ]
}

/// 0: opaque backdrop; 1: the 9x7 tile at (4, 3); 2: a half-transparent white solid; 3: root group
/// [backdrop, tile (multiply 0.9), glass].
fn filter_plan(tile: &TileData) -> RenderPlan {
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

fn tile_plane() -> (Vec<[f32; 4]>, Rect) {
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

/// The filter chain of the whole tile, stage by stage, over a window far larger than the extent.
fn chain(filters: &[LayerFilter]) -> (Vec<[f32; 4]>, Rect) {
    let big = rect(-40, -40, 100, 96);
    let (mut data, mut at) = tile_plane();
    for f in filters {
        data = filter_window(&data, at, big, f, WIDE).unwrap();
        at = big;
    }
    (data, at)
}

/// The expected image; `mask` is a per-pixel coverage byte plane over the extent.
fn expected(filters: &[LayerFilter], mask: Option<&dyn Fn(i64, i64) -> u8>) -> Vec<[u32; 4]> {
    let (data, at) = chain(filters);
    let mut want = Vec::new();
    for y in 0..H as i64 {
        for x in 0..W as i64 {
            let b = over(NORMAL, [0.0; 4], [0.2, 0.5, 0.8, 1.0], 1.0, x, y);
            let inside = x >= at.x && y >= at.y && x < at.x + at.width as i64 && y < at.y + at.height as i64;
            let s = if inside { data[((y - at.y) * at.width as i64 + (x - at.x)) as usize] } else { [0.0; 4] };
            let cov = mask.map_or(1.0, |m| (f64::from(m(x, y)) / 255.0) as f32);
            let b = over(StudioBlendMode::Multiply, b, s, 0.9 * 1.0 * cov, x, y);
            want.push(over(NORMAL, b, glass(x, y), 0.8, x, y).map(f32::to_bits));
        }
    }
    want
}

fn bits(grid: &Grid) -> Vec<[u32; 4]> {
    grid.px.iter().map(|p| p.map(f32::to_bits)).collect()
}

const CHUNKINGS: [(u32, usize); 5] = [(1, 1), (3, 1), (5, 4), (64, 1), (4, 8)];

fn each_kind() -> Vec<(&'static str, Vec<LayerFilter>)> {
    vec![
        ("gaussian", vec![LayerFilter::GaussianBlur { radius_px: 2.5 }]),
        ("unsharp", vec![LayerFilter::UnsharpMask { amount_pct: 150.0, radius_px: 1.5, threshold_levels: 3 }]),
        ("high_pass", vec![LayerFilter::HighPass { radius_px: 2.0 }]),
        ("median", vec![LayerFilter::Median { radius_px: 2 }]),
        (
            "noise_uniform",
            vec![LayerFilter::AddNoise { amount_pct: 25.0, distribution: NoiseDistribution::Uniform, monochromatic: false, seed: 77 }],
        ),
        (
            "noise_gaussian_mono",
            vec![LayerFilter::AddNoise { amount_pct: 40.0, distribution: NoiseDistribution::Gaussian, monochromatic: true, seed: 9 }],
        ),
        (
            "chain",
            vec![
                LayerFilter::Median { radius_px: 1 },
                LayerFilter::GaussianBlur { radius_px: 1.5 },
                LayerFilter::UnsharpMask { amount_pct: 200.0, radius_px: 1.0, threshold_levels: 0 },
                LayerFilter::AddNoise { amount_pct: 10.0, distribution: NoiseDistribution::Gaussian, monochromatic: false, seed: 5 },
            ],
        ),
    ]
}

#[test]
fn every_filter_matches_pigment_for_every_chunking_and_thread_count() {
    let tile = TileData::new(9, 7, tile_px);
    let sources = Sources { tiles: vec![(1, tile.planes())], masks: vec![] };
    let plan = filter_plan(&tile);
    for (name, filters) in each_kind() {
        let ext = envelope(vec![one_layer(3, 1).with_filters(filters.clone())]);
        let want = expected(&filters, None);
        for (chunk, threads) in CHUNKINGS {
            let renderer = Renderer::with_threads(chunk, threads).unwrap();
            let (result, grid) = run_ext(&renderer, &plan, &ext, Exactness::Approximate, 1 << 26, &sources);
            let receipt = result.unwrap();
            assert_eq!(receipt.exactness.lowest, Exactness::Approximate);
            assert_eq!(bits(&grid), want, "{name} chunk {chunk} x{threads}");
        }
    }
    // A filter changes the image (no silent identity): the blur differs from the plain layer.
    let plain = expected(&[], None);
    assert_ne!(plain, expected(&each_kind()[0].1, None));
    // The seed changes the noise.
    let a = LayerFilter::AddNoise { amount_pct: 25.0, distribution: NoiseDistribution::Uniform, monochromatic: false, seed: 1 };
    let b = LayerFilter::AddNoise { amount_pct: 25.0, distribution: NoiseDistribution::Uniform, monochromatic: false, seed: 2 };
    assert_ne!(expected(&[a], None), expected(&[b], None));
}

#[test]
fn filters_apply_before_the_layer_mask() {
    let mask_fn = |x: i64, y: i64| -> u8 { ((x * 17 + y * 29) % 256) as u8 };
    let tile = TileData::new(9, 7, tile_px);
    let mask = MaskData::u8(W, H, |x, y| mask_fn(i64::from(x), i64::from(y)));
    let sources = Sources { tiles: vec![(1, tile.planes())], masks: vec![(9, &mask)] };
    let mut plan = filter_plan(&tile);
    if let PlanNode::Group(g) = &mut plan.nodes[3] {
        g.layers[1].mask = Some(mask.input(9, false, 1.0));
    }
    for (name, filters) in each_kind() {
        let ext = envelope(vec![one_layer(3, 1).with_filters(filters.clone())]);
        let want = expected(&filters, Some(&mask_fn));
        for (chunk, threads) in [(3_u32, 1_usize), (64, 2), (5, 4)] {
            let renderer = Renderer::with_threads(chunk, threads).unwrap();
            let (result, grid) = run_ext(&renderer, &plan, &ext, Exactness::Approximate, 1 << 26, &sources);
            result.unwrap();
            assert_eq!(bits(&grid), want, "masked {name} chunk {chunk} x{threads}");
        }
    }
}

#[test]
fn hostile_radii_and_parameters_are_typed_before_any_sink_call() {
    let tile = TileData::new(9, 7, tile_px);
    let sources = Sources { tiles: vec![(1, tile.planes())], masks: vec![] };
    let plan = filter_plan(&tile);
    let renderer = Renderer::with_threads(8, 1).unwrap();
    // Radius 0 blur: the bytes of the pigment reference (no growth, no tap).
    let zero = vec![LayerFilter::GaussianBlur { radius_px: 0.0 }];
    let (result, grid) = run_ext(&renderer, &plan, &envelope(vec![one_layer(3, 1).with_filters(zero.clone())]), Exactness::Approximate, 1 << 26, &sources);
    result.unwrap();
    assert_eq!(bits(&grid), expected(&zero, None));
    // Out-of-range parameters and too many filters: pigment's admission, typed, no sink call.
    for bad in [
        LayerFilter::Median { radius_px: 0 },
        LayerFilter::Median { radius_px: 501 },
        LayerFilter::UnsharpMask { amount_pct: f32::NAN, radius_px: 1.0, threshold_levels: 0 },
        LayerFilter::HighPass { radius_px: -1.0 },
        LayerFilter::GaussianBlur { radius_px: f32::INFINITY },
        LayerFilter::AddNoise { amount_pct: 0.0, distribution: NoiseDistribution::Uniform, monochromatic: false, seed: 0 },
    ] {
        let ext = envelope(vec![one_layer(3, 1).with_filters(vec![bad])]);
        let (result, grid) = run_ext(&renderer, &plan, &ext, Exactness::Approximate, 1 << 26, &sources);
        assert_eq!(result.unwrap_err(), BackendError::InvalidPlan(PlanError::FilterParameter { node: 3 }), "{bad:?}");
        assert_eq!(grid.calls, 0);
    }
    let many = vec![LayerFilter::HighPass { radius_px: 1.0 }; MAX_FILTERS_PER_LAYER + 1];
    let (result, grid) = run_ext(&renderer, &plan, &envelope(vec![one_layer(3, 1).with_filters(many)]), Exactness::Approximate, 1 << 26, &sources);
    assert_eq!(result.unwrap_err(), BackendError::InvalidPlan(PlanError::FilterParameter { node: 3 }));
    assert_eq!(grid.calls, 0);
    // The largest admitted median (radius 500): predicted work above the window limit.
    let huge = envelope(vec![one_layer(3, 1).with_filters(vec![LayerFilter::Median { radius_px: 500 }])]);
    // 20 x 16 window = 320 px x 785349 disc samples x 4 channels = 1.005e9 > MAX_EFFECT_WINDOW_WORK.
    let (result, grid) = run_ext(&Renderer::with_threads(64, 1).unwrap(), &plan, &huge, Exactness::Approximate, 1 << 40, &sources);
    assert!(matches!(result, Err(BackendError::Unsupported { operator: "filter_work_limit", .. })), "{result:?}");
    assert_eq!(grid.calls, 0);
    // The widest blur (radius 1000) on a point filter: the output stays inside the content, so it
    // renders and equals the reference; one byte less than its peak is BudgetExceeded.
    let wide_filters = vec![LayerFilter::HighPass { radius_px: 1000.0 }];
    let wide = envelope(vec![one_layer(3, 1).with_filters(wide_filters.clone())]);
    let (result, grid) = run_ext(&renderer, &plan, &wide, Exactness::Approximate, 1 << 26, &sources);
    let peak = result.unwrap().peak_working_bytes;
    assert_eq!(bits(&grid), expected(&wide_filters, None));
    let (result, grid) = run_ext(&renderer, &plan, &wide, Exactness::Approximate, peak - 1, &sources);
    assert!(matches!(result, Err(BackendError::BudgetExceeded { .. })), "{result:?}");
    assert_eq!(grid.calls, 0);
}

#[test]
fn refusals_budget_and_nesting_are_typed_before_any_sink_call() {
    let tile = TileData::new(9, 7, tile_px);
    let sources = Sources { tiles: vec![(1, tile.planes())], masks: vec![] };
    let plan = filter_plan(&tile);
    let renderer = Renderer::with_threads(8, 1).unwrap();
    let chain_ext = envelope(vec![one_layer(3, 1).with_filters(each_kind()[6].1.clone())]);
    // Below the class Approximate each kind is refused under its key.
    for (key, f) in [
        ("filter_gaussian_blur", LayerFilter::GaussianBlur { radius_px: 2.0 }),
        ("filter_unsharp_mask", LayerFilter::UnsharpMask { amount_pct: 100.0, radius_px: 1.0, threshold_levels: 0 }),
        ("filter_high_pass", LayerFilter::HighPass { radius_px: 1.0 }),
        ("filter_median", LayerFilter::Median { radius_px: 1 }),
        ("filter_add_noise", LayerFilter::AddNoise { amount_pct: 5.0, distribution: NoiseDistribution::Uniform, monochromatic: false, seed: 0 }),
    ] {
        let ext = envelope(vec![one_layer(3, 1).with_filters(vec![f])]);
        let (result, grid) = run_ext(&renderer, &plan, &ext, Exactness::Exact, 1 << 26, &sources);
        assert!(matches!(result, Err(BackendError::Unsupported { operator, .. }) if operator == key), "{key}: {result:?}");
        assert_eq!(grid.calls, 0);
    }
    // Filters together with a blur on one layer: pigment's combination rule.
    let both = envelope(vec![one_layer(3, 1)
        .with_filters(vec![LayerFilter::HighPass { radius_px: 1.0 }])
        .with_blur(LayerBlur::new(BlurScope::Layer, 2.0))]);
    let (result, grid) = run_ext(&renderer, &plan, &both, Exactness::Approximate, 1 << 26, &sources);
    assert_eq!(result.unwrap_err(), BackendError::InvalidPlan(PlanError::FilterCombination { node: 3 }));
    assert_eq!(grid.calls, 0);
    // The frame and pigment's predicted stage bytes are charged before the first sink call.
    let (ok, _) = run_ext(&renderer, &plan, &chain_ext, Exactness::Approximate, 1 << 26, &sources);
    let peak = ok.unwrap().peak_working_bytes;
    let (result, grid) = run_ext(&renderer, &plan, &chain_ext, Exactness::Approximate, peak - 1, &sources);
    assert!(matches!(result, Err(BackendError::BudgetExceeded { .. })), "{result:?}");
    assert_eq!(grid.calls, 0);
    // A filtered layer inside the content of a filtered layer has no frame left.
    let mut nested = filter_plan(&tile);
    nested.nodes.push(group(vec![layer(3, NORMAL, 1.0)]));
    nested.root = PlanNodeId(4);
    let ext = envelope(vec![
        one_layer(3, 1).with_filters(vec![LayerFilter::HighPass { radius_px: 1.0 }]),
        one_layer(4, 0).with_filters(vec![LayerFilter::HighPass { radius_px: 1.0 }]),
    ]);
    let (result, grid) = run_ext(&renderer, &nested, &ext, Exactness::Approximate, 1 << 26, &sources);
    assert!(matches!(result, Err(BackendError::Unsupported { operator: "filter_nested", .. })), "{result:?}");
    assert_eq!(grid.calls, 0);
}
