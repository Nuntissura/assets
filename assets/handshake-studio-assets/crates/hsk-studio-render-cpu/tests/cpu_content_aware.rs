//! Content-aware fill on the CPU backend (lane SMB-RENDER-CONTENT-AWARE). The expected pixels are
//! composed from pigment's single owners (`content_aware::fill` called directly on the layer
//! content, `blend_math::composite_over`), never by the code under test, and must match bit for bit
//! for every chunking and thread count. Photoshop parity is NOT_PROVEN (class Approximate).
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over};
use hsk_studio_pigment::blur_math::{BlurScope, LayerBlur};
use hsk_studio_pigment::content_aware::{ColourAdaptation, ContentAwareOptions, SamplingArea, fill};
use hsk_studio_pigment::filter_math::LayerFilter;
use hsk_studio_pigment::render_plan::*;
use hsk_studio_pigment::retouch::{RetouchLimits, RgbaPlane};
use hsk_studio_pigment::selection::CoveragePlane;
use hsk_studio_render_cpu::Renderer;

mod common;
use common::*;

const NORMAL: StudioBlendMode = StudioBlendMode::Normal;
const W: u32 = 24;
const H: u32 = 20;
const BACKDROP: [f32; 4] = [0.2, 0.5, 0.8, 1.0];
const OPACITY: f32 = 0.9;
/// The tile: 20 x 16 at (2, 2), vertical stripes with a diagonal ramp (texture for the patches).
const TILE_AT: (i64, i64) = (2, 2);
const TILE_W: u32 = 20;
const TILE_H: u32 = 16;
const CHUNKINGS: [(u32, usize); 6] = [(1, 1), (3, 1), (5, 4), (64, 1), (4, 8), (7, 2)];

fn tile_px(x: u32, y: u32) -> [f32; 4] {
    let stripe = if (x / 2).is_multiple_of(2) { 0.9 } else { 0.15 };
    [stripe, ((x + y) % 5) as f32 / 4.0, (y as f32 / 15.0).min(1.0), if (x + 2 * y).is_multiple_of(9) { 0.5 } else { 1.0 }]
}

/// 0: opaque backdrop; 1: the tile; 2: root group [backdrop, tile (normal, 0.9)].
fn ca_plan(tile: &TileData) -> RenderPlan {
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
    let out = composite_over(
        NORMAL,
        Rgba::new([b[0], b[1], b[2]], b[3]),
        Rgba::new([s[0], s[1], s[2]], s[3]),
        opacity,
        PixelSite { x, y, seed: 7 },
    )
    .unwrap();
    [out.rgb[0], out.rgb[1], out.rgb[2], out.a.min(1.0)]
}

/// One case: hole bytes (u8 over the extent), inverted, sampling, options, optional layer mask.
struct Case {
    name: &'static str,
    hole: fn(u32, u32) -> u8,
    inverted: bool,
    sampling: Option<Rect>,
    options: ContentAwareOptions,
    layer_mask: Option<fn(u32, u32) -> u8>,
}

fn options(seed: u64, colour: ColourAdaptation, threads: u32) -> ContentAwareOptions {
    ContentAwareOptions {
        patch_radius: 2,
        em_iterations: 2,
        search_iterations: 2,
        max_levels: 3,
        seed,
        threads,
        colour_adaptation: colour,
        ..ContentAwareOptions::default()
    }
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "block_auto_default",
            hole: |x, y| if (9..14).contains(&x) && (7..11).contains(&y) { 255 } else { 0 },
            inverted: false,
            sampling: None,
            options: options(3, ColourAdaptation::Default, 1),
            layer_mask: None,
        },
        Case {
            name: "partial_rect_none_masked",
            hole: |x, y| if (13..17).contains(&x) && (9..13).contains(&y) { 128 } else { 0 },
            inverted: false,
            sampling: Some(rect(2, 2, 10, 16)),
            options: options(11, ColourAdaptation::None, 2),
            layer_mask: Some(|x, y| ((x * 17 + y * 29) % 256) as u8),
        },
        Case {
            name: "inverted_threads",
            hole: |x, y| if (5..9).contains(&x) && (12..15).contains(&y) { 0 } else { 255 },
            inverted: true,
            sampling: None,
            options: options(5, ColourAdaptation::Default, 3),
            layer_mask: None,
        },
    ]
}

/// The expected image: pigment's fill of the tile plane (direct call, threads 1) composited by
/// pigment's `composite_over` with the layer mask after the fill.
fn expected(case: &Case) -> Vec<[u32; 4]> {
    let mut data = Vec::new();
    for y in 0..TILE_H {
        for x in 0..TILE_W {
            data.push(tile_px(x, y));
        }
    }
    let content = rect(TILE_AT.0, TILE_AT.1, TILE_W, TILE_H);
    let target = RgbaPlane::new(content, data).unwrap();
    let mut hole = Vec::new();
    for y in 0..H {
        for x in 0..W {
            let c = u16::from((case.hole)(x, y)) * 257;
            hole.push(if case.inverted { u16::MAX - c } else { c });
        }
    }
    let hole = CoveragePlane { bounds: rect(0, 0, W, H), data: hole };
    let sampling = case.sampling.map_or(SamplingArea::Auto, SamplingArea::Rectangular);
    let direct = ContentAwareOptions { threads: 1, ..case.options };
    let limits = RetouchLimits { max_bytes: 1 << 30, max_work: RetouchLimits::default().max_work };
    let filled = fill(&target, &hole, sampling, &direct, &limits).unwrap().result.plane;
    let mut want = Vec::new();
    for y in 0..i64::from(H) {
        for x in 0..i64::from(W) {
            let b = over([0.0; 4], BACKDROP, 1.0, x, y);
            let inside = x >= content.x && y >= content.y && x < content.x + i64::from(TILE_W) && y < content.y + i64::from(TILE_H);
            let s = if inside {
                filled.data[((y - content.y) * i64::from(TILE_W) + (x - content.x)) as usize]
            } else {
                [0.0; 4]
            };
            let cov = case.layer_mask.map_or(1.0, |m| (f64::from(m(x as u32, y as u32)) / 255.0) as f32);
            want.push(over(b, s, OPACITY * cov, x, y).map(f32::to_bits));
        }
    }
    want
}

fn bits(grid: &Grid) -> Vec<[u32; 4]> {
    grid.px.iter().map(|p| p.map(f32::to_bits)).collect()
}

fn hole_op(case: &Case, hole: &MaskData) -> ContentAwareFillOp {
    ContentAwareFillOp { hole: hole.input(9, case.inverted, 1.0), sampling: case.sampling, options: case.options }
}

/// Byte identity with a direct pigment call for every chunking and thread count (auto and
/// rectangular sampling, default and none colour adaptation, partial and inverted holes, a layer
/// mask applied after the fill, pigment `threads` 1..3), and the fill changes the image.
#[test]
fn fill_matches_pigment_for_every_chunking_and_thread_count() {
    let tile = TileData::new(TILE_W, TILE_H, tile_px);
    for case in cases() {
        let hole = MaskData::u8(W, H, case.hole);
        let lmask = case.layer_mask.map(|m| MaskData::u8(W, H, m));
        let mut masks = vec![(9, &hole)];
        if let Some(m) = &lmask {
            masks.push((8, m));
        }
        let sources = Sources { tiles: vec![(1, tile.planes())], masks };
        let mut plan = ca_plan(&tile);
        if let (Some(m), PlanNode::Group(g)) = (&lmask, &mut plan.nodes[2]) {
            g.layers[1].mask = Some(m.input(8, false, 1.0));
        }
        let ext = envelope(vec![one_layer(2, 1).with_content_aware_fill(hole_op(&case, &hole))]);
        let want = expected(&case);
        for (chunk, threads) in CHUNKINGS {
            let renderer = Renderer::with_threads(chunk, threads).unwrap();
            let (result, grid) = run_ext(&renderer, &plan, &ext, Exactness::Approximate, 1 << 28, &sources);
            let receipt = result.unwrap_or_else(|e| panic!("{} chunk {chunk} x{threads}: {e:?}", case.name));
            assert_eq!(receipt.exactness.lowest, Exactness::Approximate);
            assert_eq!(bits(&grid), want, "{} chunk {chunk} x{threads}", case.name);
        }
        // No silent identity: the plain layer differs inside the hole.
        let (plain, grid) = run_ext(&Renderer::with_threads(8, 1).unwrap(), &plan, &envelope(vec![]), Exactness::Approximate, 1 << 28, &sources);
        plain.unwrap();
        assert_ne!(bits(&grid), want, "{}: the fill changed nothing", case.name);
    }
}

/// Budgets are charged before allocation and before the first sink call: one byte under the
/// render-cpu charge (chunk scratch + content, levels, hole and stored output) is `BudgetExceeded`
/// from render-cpu; exactly that charge leaves pigment no bytes and its predicted pyramid is
/// refused as `BudgetExceeded`; a hole above `max_hole_pixels` is pigment's typed refusal; the
/// receipt's peak includes the fill planes.
#[test]
fn budgets_are_charged_before_any_sink_call() {
    let tile = TileData::new(TILE_W, TILE_H, tile_px);
    let case = &cases()[0];
    let hole = MaskData::u8(W, H, case.hole);
    let sources = Sources { tiles: vec![(1, tile.planes())], masks: vec![(9, &hole)] };
    let plan = ca_plan(&tile);
    let ext = envelope(vec![one_layer(2, 1).with_content_aware_fill(hole_op(case, &hole))]);
    let renderer = Renderer::with_threads(8, 1).unwrap();
    let (ok, _) = run_ext(&renderer, &plan, &ext, Exactness::Approximate, 1 << 28, &sources);
    let peak = ok.unwrap().peak_working_bytes;
    let (plain, _) = run_ext(&renderer, &plan, &envelope(vec![]), Exactness::Approximate, 1 << 28, &sources);
    let plain_peak = plain.unwrap().peak_working_bytes;
    // content 20x16x16 + hole 24x20x2 + stored output 20x16x16 (the tile needs no group level)
    assert_eq!(peak - plain_peak, 20 * 16 * 16 + 24 * 20 * 2 + 20 * 16 * 16 + 256, "fill charge");
    let (under, grid) = run_ext(&renderer, &plan, &ext, Exactness::Approximate, peak - 1, &sources);
    assert!(matches!(under, Err(BackendError::BudgetExceeded { .. })), "{under:?}");
    assert_eq!(grid.calls, 0);
    let (pigment, grid) = run_ext(&renderer, &plan, &ext, Exactness::Approximate, peak, &sources);
    assert_eq!(pigment, Err(BackendError::BudgetExceeded { requested: 1, limit: 0 }));
    assert_eq!(grid.calls, 0);
    let small = ContentAwareFillOp { options: ContentAwareOptions { max_hole_pixels: 3, ..case.options }, ..hole_op(case, &hole) };
    let (big_hole, grid) = run_ext(&renderer, &plan, &envelope(vec![one_layer(2, 1).with_content_aware_fill(small)]), Exactness::Approximate, 1 << 28, &sources);
    assert_eq!(big_hole, Err(BackendError::Unsupported { operator: "content_aware_hole_too_large", exactness: Exactness::Unsupported }));
    assert_eq!(grid.calls, 0);
}

/// Unknown or invalid parameter combinations are typed refusals before any sink call: pigment's
/// admission (combination with filters, a blur, an adjustment or a clip role; options out of range;
/// a clip-to-base, density-scaled or misplaced hole; an empty sampling rectangle; an Approximate
/// operator under a Fitted floor) and render-cpu's own (a fill whose content holds another fill or
/// a filtered layer, an unresolved hole source, no sampleable source).
#[test]
fn parameter_and_combination_refusals_are_typed() {
    let tile = TileData::new(TILE_W, TILE_H, tile_px);
    let case = &cases()[0];
    let hole = MaskData::u8(W, H, case.hole);
    let sources = Sources { tiles: vec![(1, tile.planes())], masks: vec![(9, &hole)] };
    let plan = ca_plan(&tile);
    let renderer = Renderer::with_threads(5, 2).unwrap();
    let op = hole_op(case, &hole);
    let entry = || one_layer(2, 1).with_content_aware_fill(op);
    let param = Err(BackendError::InvalidPlan(PlanError::ContentAwareParameter { node: 2 }));
    let combo = Err(BackendError::InvalidPlan(PlanError::ContentAwareCombination { node: 2 }));
    let check = |what: &str, plan: &RenderPlan, ext: PlanExtensions, min: Exactness, want: Result<(), BackendError>| {
        let (result, grid) = run_ext(&renderer, plan, &ext, min, 1 << 28, &sources);
        assert_eq!(result.map(|_| ()), want, "{what}");
        assert_eq!(grid.calls, 0, "{what}: sink touched");
    };
    let a = Exactness::Approximate;
    check("with filters", &plan, envelope(vec![entry().with_filters(vec![LayerFilter::Median { radius_px: 1 }])]), a, combo);
    check("with blur", &plan, envelope(vec![entry().with_blur(LayerBlur::new(BlurScope::Layer, 2.0))]), a, combo);
    for bad in [
        ContentAwareOptions { patch_radius: 0, ..op.options },
        ContentAwareOptions { em_iterations: 65, ..op.options },
        ContentAwareOptions { threads: 0, ..op.options },
    ] {
        let ext = envelope(vec![one_layer(2, 1).with_content_aware_fill(ContentAwareFillOp { options: bad, ..op })]);
        check("options", &plan, ext, a, param);
    }
    let holes = [
        MaskInput { density: 0.5, ..op.hole },
        MaskInput::clip_to_base(),
    ];
    for h in holes {
        let ext = envelope(vec![one_layer(2, 1).with_content_aware_fill(ContentAwareFillOp { hole: h, ..op })]);
        check("hole", &plan, ext, a, param);
    }
    let empty = ContentAwareFillOp { sampling: Some(rect(3, 3, 0, 4)), ..op };
    check("empty sampling", &plan, envelope(vec![one_layer(2, 1).with_content_aware_fill(empty)]), a, param);
    check(
        "fitted floor",
        &plan,
        envelope(vec![entry()]),
        Exactness::Fitted,
        Err(BackendError::Unsupported { operator: "content_aware_fill", exactness: Exactness::Approximate }),
    );
    // Nested: an outer group whose layer content is the root group holding a filled layer.
    let mut nested = ca_plan(&tile);
    nested.nodes.push(group(vec![layer(2, NORMAL, 1.0)]));
    nested.root = PlanNodeId(3);
    check(
        "fill inside a fill",
        &nested,
        envelope(vec![entry(), one_layer(3, 0).with_content_aware_fill(op)]),
        a,
        Err(BackendError::Unsupported { operator: "content_aware_nested", exactness: Exactness::Unsupported }),
    );
    check(
        "filter inside a fill",
        &nested,
        envelope(vec![one_layer(2, 1).with_filters(vec![LayerFilter::Median { radius_px: 1 }]), one_layer(3, 0).with_content_aware_fill(op)]),
        a,
        Err(BackendError::Unsupported { operator: "content_aware_nested", exactness: Exactness::Unsupported }),
    );
    // Unresolved hole source (render-cpu), and a sampling area entirely inside the hole (pigment).
    let missing = ContentAwareFillOp { hole: MaskInput { key: SourceKey(77), ..op.hole }, ..op };
    check("missing hole", &plan, envelope(vec![one_layer(2, 1).with_content_aware_fill(missing)]), a, Err(BackendError::UnresolvedSource(SourceKey(77))));
    let no_source = ContentAwareFillOp { sampling: Some(rect(9, 7, 5, 4)), ..op };
    check(
        "no source",
        &plan,
        envelope(vec![one_layer(2, 1).with_content_aware_fill(no_source)]),
        a,
        Err(BackendError::Unsupported { operator: "content_aware_no_source", exactness: Exactness::Unsupported }),
    );
}
