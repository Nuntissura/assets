//! Envelope operations in an extended-range plan on render-cpu (lane SMB-PIGMENT-EXTENDED-RANGE-2):
//! exposure adjustment layer, gaussian blur filter, drop shadow and content-aware fill, each
//! against pigment's `*_in(SampleRange::Extended, ..)` maths or a hand value, for every chunking
//! and thread count; hostile overflow is typed.
#![deny(clippy::arithmetic_side_effects)]
mod common;
use common::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::adjust_math::{AdjustmentOp, Exposure, composite_adjustment_in};
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over_extended};
use hsk_studio_pigment::content_aware::{ColourAdaptation, ContentAwareOptions};
use hsk_studio_pigment::effect_math::{DropShadowOp, EffectOp};
use hsk_studio_pigment::filter_math::{FilterLimits, LayerFilter, filter_window_in};
use hsk_studio_pigment::render_plan::*;
use hsk_studio_render_cpu::{RenderError, Renderer};

const BUDGET: u64 = 1 << 26;
const CONFIGS: [(u32, usize); 6] = [(1, 1), (3, 1), (5, 4), (64, 1), (4, 8), (7, 2)];
const W: u32 = 8;
const H: u32 = 6;

fn scene(x: u32, y: u32) -> [f32; 4] {
    let (x, y) = (x as f32, y as f32);
    [0.5 + x * 0.5, 3.0 - y * 0.25, 0.25, 1.0]
}
fn ext(layers: Vec<LayerExtension>) -> PlanExtensions {
    envelope(layers).with_output(OutputStage::new(SampleRange::Extended, None))
}
fn run(chunk: u32, threads: usize, plan: &RenderPlan, e: &PlanExtensions, src: &dyn PlanSources) -> (Result<RenderReceipt, RenderError>, Grid) {
    let mut grid = Grid::new(plan.target.extent);
    let r = Renderer::with_threads(chunk, threads).unwrap().execute_extended_detailed(
        plan,
        e,
        &req(Exactness::Approximate, BUDGET),
        src,
        &CancellationToken::default(),
        &mut grid,
    );
    (r, grid)
}
/// Every configuration renders, gives the same bits as the first, and the bits of `want` when given.
fn check_all(plan: &RenderPlan, e: &PlanExtensions, src: &dyn PlanSources, key: &str, want: Option<&dyn Fn(i64, i64) -> [f32; 4]>) -> Grid {
    let mut first: Option<Grid> = None;
    for (chunk, threads) in CONFIGS {
        let (r, g) = run(chunk, threads, plan, e, src);
        let receipt = r.unwrap_or_else(|err| panic!("{key} ({chunk},{threads}): {err}"));
        assert!(receipt.exactness.operators.iter().any(|o| o.key == key), "{key}: {:?}", receipt.exactness);
        for y in 0..i64::from(H) {
            for x in 0..i64::from(W) {
                assert!(g.at(x, y).iter().all(|v| v.is_finite()));
                if let Some(f) = want {
                    assert_eq!(g.at(x, y).map(f32::to_bits), f(x, y).map(f32::to_bits), "{key} ({chunk},{threads}) ({x},{y})");
                }
            }
        }
        match &first {
            None => first = Some(g),
            Some(f) => assert_eq!(f.px.iter().map(|p| p.map(f32::to_bits)).collect::<Vec<_>>(), g.px.iter().map(|p| p.map(f32::to_bits)).collect::<Vec<_>>()),
        }
    }
    first.unwrap()
}

/// Exposure (+1 stop, offset 0.1) as an adjustment layer over the scene equals pigment's
/// `composite_adjustment_in(Extended)` bit for bit; a gaussian blur filter on the scene layer equals
/// pigment's `filter_window_in(Extended)` over the whole layer composited by
/// `composite_over_extended`; a drop shadow (multiply, black 0.5 grey, offset 2 px right) on a 2x2
/// block over a 3.0 backdrop gives the hand value 1.5 where only the shadow lies and keeps the block
/// colour 4.0 on the block; a content-aware fill (no colour adaptation) of a 4x4 hole in a 32x32 plane of 3.0
/// fills it with 3.0 (within 1e-5). All four are chunk- and thread-invariant and name their
/// `sample_range_extended_*` operator in the receipt. Hostile: an exposure that overflows binary32
/// is the typed `sample_range_outside_unit` (pigment `OutOfDomain`).
#[test]
fn extended_envelope_operations_render_unclamped_for_every_chunking() {
    let extent = rect(0, 0, W, H);
    let base = TileData::new(W, H, scene);
    let src = Sources { tiles: vec![(1, base.planes())], masks: Vec::new() };

    // Exposure adjustment layer.
    let exposure = AdjustmentOp::Exposure(Exposure { exposure: 1.0, offset: 0.1, gamma: 1.0, linearize_gamma: 1.0 });
    let adj_plan = plan(extent, vec![PlanNode::Tile(base.input(1)), placeholder(extent), group(vec![layer(0, StudioBlendMode::Normal, 1.0), layer(1, StudioBlendMode::Normal, 1.0)])], 2);
    let prepared = exposure.prepare().unwrap();
    let want_adj = |x: i64, y: i64| {
        let p = scene(u32::try_from(x).unwrap(), u32::try_from(y).unwrap());
        let site = PixelSite { x, y, seed: 0 };
        let b = composite_over_extended(StudioBlendMode::Normal, Rgba::TRANSPARENT, Rgba::new([p[0], p[1], p[2]], p[3]), 1.0, site).unwrap();
        let o = composite_adjustment_in(SampleRange::Extended, StudioBlendMode::Normal, b, &prepared, 1.0).unwrap();
        [o.rgb[0], o.rgb[1], o.rgb[2], o.a]
    };
    assert!(want_adj(7, 0)[0] > 8.0);
    check_all(&adj_plan, &ext(vec![one_layer(2, 1).with_adjustment(exposure)]), &src, "sample_range_extended_exposure", Some(&want_adj));

    // Gaussian blur filter on the scene layer (whole extent), over a transparent backdrop.
    let filter = LayerFilter::GaussianBlur { radius_px: 2.0 };
    let blur_plan = plan(extent, vec![PlanNode::Tile(base.input(1)), group(vec![layer(0, StudioBlendMode::Normal, 1.0)])], 1);
    let mut whole = Vec::new();
    for y in 0..H {
        for x in 0..W {
            whole.push(scene(x, y));
        }
    }
    let limits = FilterLimits { max_bytes: 1 << 30, max_work: 1 << 40 };
    let blurred = filter_window_in(SampleRange::Extended, &whole, extent, extent, &filter, limits).unwrap();
    let want_blur = |x: i64, y: i64| {
        let i = usize::try_from(y).unwrap().wrapping_mul(W as usize).wrapping_add(usize::try_from(x).unwrap());
        let p = blurred[i];
        let o = composite_over_extended(StudioBlendMode::Normal, Rgba::TRANSPARENT, Rgba::new([p[0], p[1], p[2]], p[3]), 1.0, PixelSite { x, y, seed: 0 }).unwrap();
        [o.rgb[0], o.rgb[1], o.rgb[2], o.a]
    };
    assert!(want_blur(4, 0)[1] > 1.0);
    check_all(&blur_plan, &ext(vec![one_layer(1, 0).with_filters(vec![filter])]), &src, "sample_range_extended_filter_gaussian_blur", Some(&want_blur));

    // Drop shadow: a 2x2 block of 4.0 at (1,1) over a 3.0 backdrop, shadow 2 px to the right.
    let block = TileData::new(2, 2, |_, _| [4.0, 4.0, 4.0, 1.0]);
    let fx_plan = plan(extent, vec![solid(extent, [3.0, 3.0, 3.0, 1.0]), PlanNode::Tile(block.input_at(2, 1, 1)), group(vec![layer(0, StudioBlendMode::Normal, 1.0), layer(1, StudioBlendMode::Normal, 1.0)])], 2);
    let mut shadow = DropShadowOp::new([0.5, 0.5, 0.5], StudioBlendMode::Multiply, 1.0);
    shadow.angle_deg = 180.0;
    shadow.distance_px = 2.0;
    let fx_src = Sources { tiles: vec![(2, block.planes())], masks: Vec::new() };
    let g = check_all(&fx_plan, &ext(vec![one_layer(2, 1).with_effects(vec![EffectOp::DropShadow(shadow)])]), &fx_src, "sample_range_extended_effects", None);
    assert_eq!(g.at(1, 1), [4.0, 4.0, 4.0, 1.0]);
    assert_eq!(g.at(0, 4), [3.0, 3.0, 3.0, 1.0]);
    let shadowed: Vec<[f32; 4]> = (0..i64::from(W)).flat_map(|x| (0..i64::from(H)).map(move |y| (x, y))).map(|(x, y)| g.at(x, y)).filter(|p| p[0] == 1.5).collect();
    assert!(!shadowed.is_empty(), "no shadow-only pixel at 3.0 x 0.5");

    // Content-aware fill without colour adaptation in a 32x32 plane of 3.0 (a 7x7 patch fits
    // beside the 4x4 hole).
    let ca_extent = rect(0, 0, 32, 32);
    let flat = TileData::new(32, 32, |_, _| [3.0, 2.0, 0.5, 1.0]);
    let hole = MaskData::u8(32, 32, |x, y| if (14..18).contains(&x) && (14..18).contains(&y) { 255 } else { 0 });
    let ca_plan = plan(ca_extent, vec![solid(ca_extent, [0.0, 0.0, 0.0, 1.0]), PlanNode::Tile(flat.input(2)), group(vec![layer(0, StudioBlendMode::Normal, 1.0), layer(1, StudioBlendMode::Normal, 1.0)])], 2);
    let options = ContentAwareOptions { colour_adaptation: ColourAdaptation::None, ..ContentAwareOptions::default() };
    let op = ContentAwareFillOp { hole: hole.input(9, false, 1.0), sampling: None, options };
    let ca_src = Sources { tiles: vec![(2, flat.planes())], masks: vec![(9, &hole)] };
    let mut grid = Grid::new(ca_extent);
    let r = Renderer::with_threads(5, 3).unwrap().execute_extended_detailed(&ca_plan, &ext(vec![one_layer(2, 1).with_content_aware_fill(op)]), &req(Exactness::Approximate, BUDGET), &ca_src, &CancellationToken::default(), &mut grid);
    let receipt = r.unwrap();
    assert!(receipt.exactness.operators.iter().any(|o| o.key == "sample_range_extended_content_aware_fill"));
    for p in &grid.px {
        assert!((p[0] - 3.0).abs() < 1e-5 && (p[1] - 2.0).abs() < 1e-5, "{p:?}");
    }

    // Hostile: +20 stops of 3e38 overflows binary32.
    let huge = TileData::new(W, H, |_, _| [3.0e38, 1.0, 1.0, 1.0]);
    let huge_src = Sources { tiles: vec![(1, huge.planes())], masks: Vec::new() };
    let boom = AdjustmentOp::Exposure(Exposure { exposure: 20.0, offset: 0.0, gamma: 1.0, linearize_gamma: 1.0 });
    let huge_plan = plan(extent, vec![PlanNode::Tile(huge.input(1)), placeholder(extent), group(vec![layer(0, StudioBlendMode::Normal, 1.0), layer(1, StudioBlendMode::Normal, 1.0)])], 2);
    let (r, _) = run(4, 2, &huge_plan, &ext(vec![one_layer(2, 1).with_adjustment(boom)]), &huge_src);
    assert_eq!(
        r.unwrap_err(),
        RenderError::Backend(BackendError::Unsupported { operator: SAMPLE_RANGE_OPERATOR, exactness: Exactness::Unsupported })
    );
}
