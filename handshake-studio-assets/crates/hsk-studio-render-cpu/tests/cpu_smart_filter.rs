//! Smart-filter stacks on the CPU backend (lane SMB-PS-SELECT-LIQUIFY-2). The expected pixels are
//! composed from pigment's single owners (`smart_filter::SmartFilterStack::render` called directly
//! on the layer content with the masks as coverage planes, `blend_math::composite_over`), never by
//! the code under test, and must match bit for bit for every chunking and thread count.
//! Photoshop parity is UNVERIFIED (class Approximate).
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over};
use hsk_studio_pigment::brush::StrokeSample;
use hsk_studio_pigment::filter_math::{LayerFilter, NoiseDistribution};
use hsk_studio_pigment::liquify::{LiquifyBrush, LiquifyLimits, LiquifyMesh, LiquifyTool};
use hsk_studio_pigment::render_plan::*;
use hsk_studio_pigment::retouch::RgbaPlane;
use hsk_studio_pigment::selection::CoveragePlane;
use hsk_studio_pigment::smart_filter::*;
use hsk_studio_pigment::transform_math::ResampleFilter;
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

fn tile_px(x: u32, y: u32) -> [f32; 4] {
    let check = (x / 3 + y / 3).is_multiple_of(2);
    if check { [0.9, 0.25 + 0.01 * (x % 5) as f32, 0.1, 1.0] } else { [0.1, 0.4, 0.85, 0.7] }
}

fn content_rect() -> hsk_studio_pigment::Rect {
    rect(TILE_AT.0, TILE_AT.1, TILE_W, TILE_H)
}

fn mesh() -> LiquifyMesh {
    let lim = LiquifyLimits::default();
    let mut m = LiquifyMesh::new(content_rect(), 4, &lim).unwrap();
    let s = |x: f64, y: f64| StrokeSample { x, y, pressure: None, time_ms: None };
    m.apply_stroke(LiquifyTool::Twirl { clockwise: true }, &[s(12.0, 10.0); 3], &LiquifyBrush { size: 12.0, ..LiquifyBrush::default() }, &lim).unwrap();
    m
}

fn entry_mask_fn(x: u32, y: u32) -> u8 {
    ((x * 11 + y * 7) % 256) as u8
}
fn stack_mask_fn(x: u32, _y: u32) -> u8 {
    if x < 14 { 255 } else { 96 }
}

/// Blur (masked) -> liquify (opacity 0.6) -> noise, under a stack mask.
fn op(entry_mask: &MaskData, stack_mask: &MaskData) -> SmartFilterOp {
    SmartFilterOp {
        entries: vec![
            SmartFilterEntryOp { filter: StackFilter::Filter(LayerFilter::GaussianBlur { radius_px: 2.0 }), enabled: true, opacity: 1.0, mask: Some(entry_mask.input(8, false, 1.0)) },
            SmartFilterEntryOp { filter: StackFilter::Liquify { mesh: mesh(), resample: ResampleFilter::Bilinear }, enabled: true, opacity: 0.6, mask: None },
            SmartFilterEntryOp { filter: StackFilter::Filter(LayerFilter::Median { radius_px: 1 }), enabled: false, opacity: 1.0, mask: None },
            SmartFilterEntryOp {
                filter: StackFilter::Filter(LayerFilter::AddNoise { amount_pct: 10.0, distribution: NoiseDistribution::Uniform, monochromatic: false, seed: 3 }),
                enabled: true,
                opacity: 1.0,
                mask: None,
            },
        ],
        stack_mask: Some(stack_mask.input(9, true, 1.0)),
    }
}

fn plan_for(tile: &TileData) -> RenderPlan {
    let extent = rect(0, 0, W, H);
    plan(extent, vec![solid(extent, BACKDROP), PlanNode::Tile(tile.input_at(1, TILE_AT.0, TILE_AT.1)), group(vec![layer(0, NORMAL, 1.0), layer(1, NORMAL, OPACITY)])], 2)
}

fn over(b: [f32; 4], s: [f32; 4], opacity: f32, x: i64, y: i64) -> [f32; 4] {
    if opacity <= 0.0 || s[3] <= 0.0 {
        return b;
    }
    let out = composite_over(NORMAL, Rgba::new([b[0], b[1], b[2]], b[3]), Rgba::new([s[0], s[1], s[2]], s[3]), opacity, PixelSite { x, y, seed: 7 }).unwrap();
    [out.rgb[0], out.rgb[1], out.rgb[2], out.a.min(1.0)]
}

fn plane_of(f: fn(u32, u32) -> u8, inverted: bool) -> CoveragePlane {
    let mut data = Vec::new();
    for y in 0..H {
        for x in 0..W {
            let c = u16::from(f(x, y)) * 257;
            data.push(if inverted { u16::MAX - c } else { c });
        }
    }
    CoveragePlane { bounds: rect(0, 0, W, H), data }
}

/// Pigment's stack of the content plane, composited by pigment.
fn expected() -> Vec<[u32; 4]> {
    let mut data = Vec::new();
    for y in 0..TILE_H {
        for x in 0..TILE_W {
            data.push(tile_px(x, y));
        }
    }
    let content = content_rect();
    let em = MaskData::u8(W, H, entry_mask_fn);
    let sm = MaskData::u8(W, H, stack_mask_fn);
    let o = op(&em, &sm);
    let stack = SmartFilterStack {
        entries: o
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| StackEntry { filter: e.filter.clone(), enabled: e.enabled, opacity: e.opacity, mask: (i == 0).then(|| plane_of(entry_mask_fn, false)) })
            .collect(),
        stack_mask: Some(plane_of(stack_mask_fn, true)),
    };
    let filtered = stack.render(&RgbaPlane::new(content, data).unwrap(), &StackLimits::default()).unwrap();
    let mut want = Vec::new();
    for y in 0..i64::from(H) {
        for x in 0..i64::from(W) {
            let b = over([0.0; 4], BACKDROP, 1.0, x, y);
            let inside = x >= content.x && y >= content.y && x < content.x + i64::from(TILE_W) && y < content.y + i64::from(TILE_H);
            let s = if inside { filtered.data[((y - content.y) * i64::from(TILE_W) + (x - content.x)) as usize] } else { [0.0; 4] };
            want.push(over(b, s, OPACITY, x, y).map(f32::to_bits));
        }
    }
    want
}

fn bits(grid: &Grid) -> Vec<[u32; 4]> {
    grid.px.iter().map(|p| p.map(f32::to_bits)).collect()
}

#[test]
fn smart_filter_stack_matches_pigment_for_every_chunking_and_thread_count() {
    let tile = TileData::new(TILE_W, TILE_H, tile_px);
    let em = MaskData::u8(W, H, entry_mask_fn);
    let sm = MaskData::u8(W, H, stack_mask_fn);
    let sources = Sources { tiles: vec![(1, tile.planes())], masks: vec![(8, &em), (9, &sm)] };
    let plan = plan_for(&tile);
    let ext = envelope(vec![one_layer(2, 1).with_smart_filters(op(&em, &sm))]);
    let want = expected();
    for (chunk, threads) in CHUNKINGS {
        let renderer = Renderer::with_threads(chunk, threads).unwrap();
        let (result, grid) = run_ext(&renderer, &plan, &ext, Exactness::Approximate, 1 << 28, &sources);
        let receipt = result.unwrap_or_else(|e| panic!("chunk {chunk} x{threads}: {e:?}"));
        assert_eq!(receipt.exactness.lowest, Exactness::Approximate);
        assert_eq!(bits(&grid), want, "chunk {chunk} x{threads}");
    }
    let (plain, grid) = run_ext(&Renderer::with_threads(8, 1).unwrap(), &plan, &envelope(vec![]), Exactness::Approximate, 1 << 28, &sources);
    plain.unwrap();
    assert_ne!(bits(&grid), want, "the stack changed nothing");
    // An all-disabled stack renders the plain image bit for bit.
    let mut off = op(&em, &sm);
    for e in &mut off.entries {
        e.enabled = false;
    }
    let (r, g2) = run_ext(&Renderer::with_threads(8, 1).unwrap(), &plan, &envelope(vec![one_layer(2, 1).with_smart_filters(off)]), Exactness::Approximate, 1 << 28, &sources);
    r.unwrap();
    assert_eq!(bits(&g2), bits(&grid));
}

#[test]
fn smart_filter_refusals_and_budgets_are_typed_before_any_sink_call() {
    let tile = TileData::new(TILE_W, TILE_H, tile_px);
    let em = MaskData::u8(W, H, entry_mask_fn);
    let sm = MaskData::u8(W, H, stack_mask_fn);
    let sources = Sources { tiles: vec![(1, tile.planes())], masks: vec![(8, &em), (9, &sm)] };
    let plan = plan_for(&tile);
    let renderer = Renderer::with_threads(5, 2).unwrap();
    let check = |what: &str, ext: PlanExtensions, min: Exactness, budget: u64, want: Result<(), BackendError>| {
        let (result, grid) = run_ext(&renderer, &plan, &ext, min, budget, &sources);
        assert_eq!(result.map(|_| ()), want, "{what}");
        assert_eq!(grid.calls, 0, "{what}: sink touched");
    };
    let a = Exactness::Approximate;
    let big = 1 << 28;
    let param = Err(BackendError::InvalidPlan(PlanError::SmartFilterParameter { node: 2 }));
    let combo = Err(BackendError::InvalidPlan(PlanError::SmartFilterCombination { node: 2 }));
    let with = |o: SmartFilterOp| envelope(vec![one_layer(2, 1).with_smart_filters(o)]);
    let mut bad = op(&em, &sm);
    bad.entries[0].opacity = 1.5;
    check("opacity", with(bad), a, big, param);
    let mut bad = op(&em, &sm);
    bad.entries[0].filter = StackFilter::Filter(LayerFilter::GaussianBlur { radius_px: f32::NAN });
    check("NaN radius", with(bad), a, big, param);
    let mut bad = op(&em, &sm);
    bad.entries[0].mask = Some(em.input(8, false, 0.5));
    check("mask density", with(bad), a, big, param);
    let mut bad = op(&em, &sm);
    bad.entries = vec![bad.entries[3].clone(); 17];
    check("too many", with(bad), a, big, param);
    let mut bad = op(&em, &sm);
    if let StackFilter::Liquify { mesh, .. } = &mut bad.entries[1].filter {
        mesh.disp.pop();
    }
    check("broken mesh", with(bad), a, big, param);
    // A mesh over other bounds than the content plane: refused by pigment at execution.
    let mut bad = op(&em, &sm);
    bad.entries[1].filter = StackFilter::Liquify { mesh: LiquifyMesh::new(rect(0, 0, 8, 8), 4, &LiquifyLimits::default()).unwrap(), resample: ResampleFilter::Bilinear };
    check("mesh bounds", with(bad), a, big, param);
    check(
        "with filters",
        envelope(vec![one_layer(2, 1).with_smart_filters(op(&em, &sm)).with_filters(vec![LayerFilter::Median { radius_px: 1 }])]),
        a,
        big,
        combo,
    );
    let extended = with(op(&em, &sm)).with_output(OutputStage::new(SampleRange::Extended, None));
    check("extended range", extended, a, big, combo);
    check("fitted floor", with(op(&em, &sm)), Exactness::Fitted, big, Err(BackendError::Unsupported { operator: "smart_filter_stack", exactness: Exactness::Approximate }));
    let missing = Sources { tiles: vec![(1, tile.planes())], masks: vec![(9, &sm)] };
    let (r, grid) = run_ext(&renderer, &plan, &with(op(&em, &sm)), a, big, &missing);
    assert_eq!(r.map(|_| ()), Err(BackendError::UnresolvedSource(SourceKey(8))));
    assert_eq!(grid.calls, 0);
    // Budget: the charge is refused before the first sink call when one byte short.
    let (ok, _) = run_ext(&renderer, &plan, &with(op(&em, &sm)), a, big, &sources);
    let peak = ok.unwrap().peak_working_bytes;
    let (plain, _) = run_ext(&renderer, &plan, &envelope(vec![]), a, big, &sources);
    let plain_peak = plain.unwrap().peak_working_bytes;
    println!("smart filter peak {peak} vs plain {plain_peak}");
    assert!(peak > plain_peak);
    let (under, grid) = run_ext(&renderer, &plan, &with(op(&em, &sm)), a, plain_peak, &sources);
    assert!(matches!(under, Err(BackendError::BudgetExceeded { .. })), "{under:?}");
    assert_eq!(grid.calls, 0);
}
