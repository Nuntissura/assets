//! Raster filter kernels on the CPU backend (lane SMB-PIGMENT-FILTER-KERNELS-1): the expected pixels
//! come from pigment's `SmartFilterStack::render` of the layer content composited by pigment, never
//! from the code under test, and must match bit for bit for every chunking and thread count.
//! Vendor parity UNVERIFIED (class Approximate).
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over};
use hsk_studio_pigment::blur_math::RadialBlurKind;
use hsk_studio_pigment::filter_kernels::{BoxDimensions, FilterKernel as K, MorphPreserve};
use hsk_studio_pigment::render_plan::*;
use hsk_studio_pigment::retouch::RgbaPlane;
use hsk_studio_pigment::smart_filter::*;
use hsk_studio_render_cpu::Renderer;

mod common;
use common::*;

const NORMAL: StudioBlendMode = StudioBlendMode::Normal;
const W: u32 = 24;
const H: u32 = 20;
const BACKDROP: [f32; 4] = [0.8, 0.3, 0.6, 1.0];
const TILE_AT: (i64, i64) = (2, 2);
const TILE_W: u32 = 20;
const TILE_H: u32 = 16;
const CHUNKINGS: [(u32, usize); 6] = [(1, 1), (3, 1), (5, 4), (64, 1), (4, 8), (7, 2)];

fn tile_px(x: u32, y: u32) -> [f32; 4] {
    let check = (x / 3 + y / 3).is_multiple_of(2);
    if check { [0.9, 0.25 + 0.01 * (x % 5) as f32, 0.1, 1.0] } else { [0.1, 0.4, 0.85, 0.7] }
}

fn kernels() -> Vec<K> {
    let mut weights = [0; 25];
    weights[12] = 5;
    weights[6] = -1;
    vec![
        K::BoxBlur { radius_px: 2.0, iterations: 2, dimensions: BoxDimensions::HorizontalAndVertical },
        K::MotionBlur { angle_deg: -30.0, length_px: 6.0 },
        K::RadialBlur { centre_x_px: 10.0, centre_y_px: 9.0, amount: 40.0, kind: RadialBlurKind::Spin },
        K::FindEdges { invert: true },
        K::CustomConvolution { weights, scale: 4, offset: 3 },
        K::Sharpen,
        K::SharpenMore,
        K::BasicBlur,
        K::BlurMore,
        K::SharpenEdges,
        K::Despeckle,
        K::Emboss { angle_deg: 135.0, height_px: 1.5, amount_pct: 100.0 },
        K::Solarize { threshold_levels: 128 },
        K::Mosaic { horizontal_blocks: 4, vertical_blocks: 3 },
        K::Maximum { radius_px: 1, preserve: MorphPreserve::Squareness },
        K::Minimum { radius_px: 1, preserve: MorphPreserve::Roundness },
        K::DustAndScratches { radius_px: 1, threshold: 0.05 },
        K::SurfaceBlur { radius_px: 2, threshold_levels: 30.0 },
        K::BilateralBlur { radius_px: 2, range_sigma: 0.2 },
    ]
}

fn entry(k: K) -> SmartFilterEntryOp {
    SmartFilterEntryOp { filter: StackFilter::Kernel(k), enabled: true, opacity: 0.75, mask: None }
}

fn plan_for(tile: &TileData) -> RenderPlan {
    let extent = rect(0, 0, W, H);
    plan(extent, vec![solid(extent, BACKDROP), PlanNode::Tile(tile.input_at(1, TILE_AT.0, TILE_AT.1)), group(vec![layer(0, NORMAL, 1.0), layer(1, NORMAL, 1.0)])], 2)
}

fn over(b: [f32; 4], s: [f32; 4], x: i64, y: i64) -> [f32; 4] {
    if s[3] <= 0.0 {
        return b;
    }
    let out = composite_over(NORMAL, Rgba::new([b[0], b[1], b[2]], b[3]), Rgba::new([s[0], s[1], s[2]], s[3]), 1.0, PixelSite { x, y, seed: 7 }).unwrap();
    [out.rgb[0], out.rgb[1], out.rgb[2], out.a.min(1.0)]
}

fn expected(o: &SmartFilterOp) -> Vec<[u32; 4]> {
    let mut data = Vec::new();
    for y in 0..TILE_H {
        for x in 0..TILE_W {
            data.push(tile_px(x, y));
        }
    }
    let content = rect(TILE_AT.0, TILE_AT.1, TILE_W, TILE_H);
    let stack = SmartFilterStack {
        entries: o.entries.iter().map(|e| StackEntry { filter: e.filter.clone(), enabled: e.enabled, opacity: e.opacity, mask: None }).collect(),
        stack_mask: None,
    };
    let filtered = stack.render(&RgbaPlane::new(content, data).unwrap(), &StackLimits::default()).unwrap();
    let mut want = Vec::new();
    for y in 0..i64::from(H) {
        for x in 0..i64::from(W) {
            let b = over([0.0; 4], BACKDROP, x, y);
            let inside = x >= content.x && y >= content.y && x < content.x + i64::from(TILE_W) && y < content.y + i64::from(TILE_H);
            let s = if inside { filtered.data[((y - content.y) * i64::from(TILE_W) + (x - content.x)) as usize] } else { [0.0; 4] };
            want.push(over(b, s, x, y).map(f32::to_bits));
        }
    }
    want
}

fn bits(grid: &Grid) -> Vec<[u32; 4]> {
    grid.px.iter().map(|p| p.map(f32::to_bits)).collect()
}

#[test]
fn filter_kernels_match_pigment_for_every_chunking_and_thread_count() {
    let tile = TileData::new(TILE_W, TILE_H, tile_px);
    let sources = Sources { tiles: vec![(1, tile.planes())], masks: vec![] };
    let plan = plan_for(&tile);
    let (plain, plain_grid) = run_ext(&Renderer::with_threads(8, 1).unwrap(), &plan, &envelope(vec![]), Exactness::Approximate, 1 << 28, &sources);
    plain.unwrap();
    // A full stack (at most 16 entries), then each kind alone.
    let full = SmartFilterOp { entries: kernels().into_iter().take(16).map(entry).collect(), stack_mask: None };
    let alone = kernels().into_iter().map(|k| SmartFilterOp { entries: vec![entry(k)], stack_mask: None });
    for o in std::iter::once(full).chain(alone) {
        let want = expected(&o);
        assert_ne!(bits(&plain_grid), want, "the stack changed nothing");
        let ext = envelope(vec![one_layer(2, 1).with_smart_filters(o)]);
        for (chunk, threads) in CHUNKINGS {
            let renderer = Renderer::with_threads(chunk, threads).unwrap();
            let (result, grid) = run_ext(&renderer, &plan, &ext, Exactness::Approximate, 1 << 28, &sources);
            let receipt = result.unwrap_or_else(|e| panic!("chunk {chunk} x{threads}: {e:?}"));
            assert_eq!(receipt.exactness.lowest, Exactness::Approximate);
            assert_eq!(bits(&grid), want, "chunk {chunk} x{threads}");
        }
    }
}

#[test]
fn filter_kernel_refusals_are_typed_before_any_sink_call() {
    let tile = TileData::new(TILE_W, TILE_H, tile_px);
    let sources = Sources { tiles: vec![(1, tile.planes())], masks: vec![] };
    let plan = plan_for(&tile);
    let renderer = Renderer::with_threads(5, 2).unwrap();
    let param = Err(BackendError::InvalidPlan(PlanError::SmartFilterParameter { node: 2 }));
    for bad in [
        K::Solarize { threshold_levels: 255 },
        K::CustomConvolution { weights: [0; 25], scale: 0, offset: 0 },
        K::MotionBlur { angle_deg: f32::NAN, length_px: 1.0 },
        K::Maximum { radius_px: 32_001, preserve: MorphPreserve::Squareness },
    ] {
        let o = SmartFilterOp { entries: vec![SmartFilterEntryOp { filter: StackFilter::Kernel(bad.clone()), enabled: true, opacity: 1.0, mask: None }], stack_mask: None };
        let ext = envelope(vec![one_layer(2, 1).with_smart_filters(o)]);
        let (result, grid) = run_ext(&renderer, &plan, &ext, Exactness::Approximate, 1 << 28, &sources);
        assert_eq!(result.map(|_| ()), param, "{bad:?}");
        assert_eq!(grid.calls, 0, "{bad:?}");
    }
    // Over the work budget, inside the bounds: typed, no sink call.
    let o = SmartFilterOp {
        entries: vec![SmartFilterEntryOp { filter: StackFilter::Kernel(K::SurfaceBlur { radius_px: u32::MAX, threshold_levels: 1.0 }), enabled: true, opacity: 1.0, mask: None }],
        stack_mask: None,
    };
    let (result, grid) = run_ext(&renderer, &plan, &envelope(vec![one_layer(2, 1).with_smart_filters(o)]), Exactness::Approximate, 1 << 28, &sources);
    assert!(matches!(result, Err(BackendError::BudgetExceeded { .. })), "{:?}", result.map(|_| ()));
    assert_eq!(grid.calls, 0);
}
