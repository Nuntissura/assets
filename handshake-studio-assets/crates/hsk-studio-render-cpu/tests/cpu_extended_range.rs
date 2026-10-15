//! Plan output stage on render-cpu (lane SMB-PIGMENT-EXTENDED-RANGE): extended-range compositing
//! through pigment `composite_over_extended` and the stage-6a clip, against an oracle computed here
//! from pigment's functions; the unit default is byte-identical to the v1 path.
#![deny(clippy::arithmetic_side_effects)]
mod common;
use common::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over_extended};
use hsk_studio_pigment::render_plan::*;
use hsk_studio_render_cpu::{RenderError, Renderer, SourceFault};

const BUDGET: u64 = 1 << 24;
const CONFIGS: [(u32, usize); 6] = [(1, 1), (3, 1), (5, 4), (64, 1), (4, 8), (7, 2)];
/// Layers above the scene tile, bottom first: (mode, opacity, rect origin, colour + alpha).
type Top = (StudioBlendMode, f32, (i64, i64), [f32; 4]);
const TOPS: [Top; 3] = [
    (StudioBlendMode::LinearDodgeAdd, 0.7, (1, 0), [0.5, 1.25, 0.2, 0.8]),
    (StudioBlendMode::Difference, 1.0, (3, 2), [2.0, 0.25, 0.75, 0.6]),
    (StudioBlendMode::Lighten, 0.5, (0, 3), [3.5, 0.1, 6.0, 1.0]),
];

fn scene_pixel(x: u32, y: u32) -> [f32; 4] {
    let (x, y) = (x as f32, y as f32);
    [x * 0.5 - 0.5, 1.0 + y * 0.75, 0.3, 1.0]
}
fn scene(base: &TileData, tops: &[TileData]) -> RenderPlan {
    let mut nodes = vec![PlanNode::Tile(base.input(1))];
    let mut layers = vec![layer(0, StudioBlendMode::Normal, 1.0)];
    for (i, ((mode, opacity, (x, y), _), tile)) in TOPS.iter().zip(tops).enumerate() {
        let key = u32::try_from(i).unwrap().wrapping_add(2);
        nodes.push(PlanNode::Tile(tile.input_at(key, *x, *y)));
        layers.push(layer(key.wrapping_sub(1), *mode, *opacity));
    }
    nodes.push(group(layers));
    plan(rect(0, 0, 7, 5), nodes, 4)
}
fn top_tiles() -> Vec<TileData> {
    TOPS.iter().map(|t| TileData::new(4, 2, move |_, _| t.3)).collect()
}
fn sources<'a>(base: &'a TileData, tops: &'a [TileData]) -> Sources<'a> {
    let mut tiles = vec![(1, base.planes())];
    for (i, t) in tops.iter().enumerate() {
        tiles.push((u32::try_from(i).unwrap().wrapping_add(2), t.planes()));
    }
    Sources { tiles, masks: Vec::new() }
}
/// Pigment's extended composite of the scene at `(x, y)`.
fn oracle(x: i64, y: i64) -> [f32; 4] {
    let site = PixelSite { x, y, seed: 0 };
    let p = scene_pixel(u32::try_from(x).unwrap(), u32::try_from(y).unwrap());
    let mut out = composite_over_extended(
        StudioBlendMode::Normal,
        Rgba::TRANSPARENT,
        Rgba::new([p[0], p[1], p[2]], p[3]),
        1.0,
        site,
    )
    .unwrap();
    for (mode, opacity, (ox, oy), c) in TOPS {
        if (ox..ox.wrapping_add(4)).contains(&x) && (oy..oy.wrapping_add(2)).contains(&y) {
            out = composite_over_extended(mode, out, Rgba::new([c[0], c[1], c[2]], c[3]), opacity, site)
                .unwrap();
        }
    }
    [out.rgb[0], out.rgb[1], out.rgb[2], out.a]
}
fn run(
    renderer: &Renderer,
    plan: &RenderPlan,
    ext: Option<&PlanExtensions>,
    sources: &dyn PlanSources,
) -> (Result<RenderReceipt, RenderError>, Grid) {
    let mut grid = Grid::new(plan.target.extent);
    let request = req(Exactness::Approximate, BUDGET);
    let cancel = CancellationToken::default();
    let result = match ext {
        None => renderer.execute_detailed(plan, &request, sources, &cancel, &mut grid),
        Some(e) => renderer.execute_extended_detailed(plan, e, &request, sources, &cancel, &mut grid),
    };
    (result, grid)
}
fn bits(grid: &Grid) -> Vec<[u32; 4]> {
    grid.px.iter().map(|p| p.map(f32::to_bits)).collect()
}
fn stage(range: SampleRange, display: Option<DisplayTransform>) -> PlanExtensions {
    PlanExtensions::default().with_output(OutputStage::new(range, display))
}

/// Unit default: the v1 call, an empty envelope and an envelope with the default output stage give
/// the same bytes and the same receipt (digest, ledger, peak bytes). Extended: every pixel equals
/// pigment's extended composite bit for bit, values above 1 kept (hand value: (6,4) keeps
/// [2.5, 4.0, 0.3] untouched by the layers above), for every chunking and thread count; with the
/// clip the same composite clamped into [0, 1].
#[test]
fn unit_default_is_byte_identical_and_extended_stage_matches_pigment_for_every_chunking() {
    let tops = top_tiles();
    let in_range = TileData::new(7, 5, |x, y| scene_pixel(x, y).map(|v| v.clamp(0.0, 1.0)));
    let unit_tops: Vec<TileData> = TOPS
        .iter()
        .map(|t| TileData::new(4, 2, move |_, _| t.3.map(|v| v.clamp(0.0, 1.0))))
        .collect();
    let unit_plan = scene(&in_range, &unit_tops);
    let unit_src = sources(&in_range, &unit_tops);
    let renderer = Renderer::with_threads(5, 4).unwrap();
    let (v1, v1_grid) = run(&renderer, &unit_plan, None, &unit_src);
    let v1 = v1.unwrap();
    for env in [PlanExtensions::default(), stage(SampleRange::Unit, None)] {
        let (r, g) = run(&renderer, &unit_plan, Some(&env), &unit_src);
        assert_eq!(r.unwrap(), v1);
        assert_eq!(bits(&g), bits(&v1_grid));
    }

    let base = TileData::new(7, 5, scene_pixel);
    let plan = scene(&base, &tops);
    let src = sources(&base, &tops);
    assert_eq!(oracle(6, 4), [2.5, 4.0, 0.3, 1.0]);
    let extended = stage(SampleRange::Extended, None);
    let clipped = stage(SampleRange::Extended, Some(DisplayTransform::Clip));
    let mut above_one = 0;
    for (chunk, threads) in CONFIGS {
        let renderer = Renderer::with_threads(chunk, threads).unwrap();
        let (r, g) = run(&renderer, &plan, Some(&extended), &src);
        let receipt = r.unwrap_or_else(|e| panic!("({chunk},{threads}): {e}"));
        assert_eq!(receipt.plan_digest, extended.digest_with(&plan).unwrap());
        let (rc, gc) = run(&renderer, &plan, Some(&clipped), &src);
        rc.unwrap();
        for y in 0..5i64 {
            for x in 0..7i64 {
                let want = oracle(x, y);
                assert_eq!(g.at(x, y).map(f32::to_bits), want.map(f32::to_bits), "({chunk},{threads}) ({x},{y})");
                let clip = [want[0].clamp(0.0, 1.0), want[1].clamp(0.0, 1.0), want[2].clamp(0.0, 1.0), want[3]];
                assert_eq!(gc.at(x, y).map(f32::to_bits), clip.map(f32::to_bits), "clip ({chunk},{threads}) ({x},{y})");
                above_one += usize::from(want[..3].iter().any(|v| *v > 1.0));
            }
        }
        assert_eq!(renderer.live_scratch_bytes(), 0);
    }
    assert!(above_one > 0);
}

/// Hostile values in an extended plan: NaN and +-inf tile samples keep `source_nonfinite`, a NaN or
/// infinite solid is an invalid plan, a mode undefined above 1 (screen) and an envelope layer
/// entry are refused before the first sink call, and an add that overflows binary32 is the typed
/// `sample_range_outside_unit` (pigment `OutOfDomain`), never an infinite pixel.
#[test]
fn hostile_values_in_an_extended_plan_are_typed() {
    let renderer = Renderer::with_threads(2, 3).unwrap();
    let extended = stage(SampleRange::Extended, Some(DisplayTransform::Clip));
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let tile = TileData::new(3, 2, |x, _| if x == 1 { [5.0, bad, 0.2, 1.0] } else { [5.0, 0.1, 0.2, 1.0] });
        let p = plan(rect(0, 0, 3, 2), vec![PlanNode::Tile(tile.input(4))], 0);
        let src = Sources { tiles: vec![(4, tile.planes())], masks: Vec::new() };
        let (result, grid) = run(&renderer, &p, Some(&extended), &src);
        assert_eq!(result.unwrap_err(), RenderError::Source { key: SourceKey(4), fault: SourceFault::Nonfinite });
        assert_eq!(grid.calls, 0);
        let p = plan(rect(0, 0, 3, 2), vec![solid(rect(0, 0, 3, 2), [bad, 0.5, 0.5, 1.0])], 0);
        let (result, grid) = run(&renderer, &p, Some(&extended), &no_sources());
        assert!(matches!(result, Err(RenderError::Backend(BackendError::InvalidPlan(_)))), "{result:?}");
        assert_eq!(grid.calls, 0);
    }
    let two = |mode: StudioBlendMode, top: f32| {
        plan(
            rect(0, 0, 2, 1),
            vec![
                solid(rect(0, 0, 2, 1), [3.0e38, 2.0, 0.5, 1.0]),
                solid(rect(0, 0, 2, 1), [top, 0.25, 0.5, 1.0]),
                group(vec![layer(0, StudioBlendMode::Normal, 1.0), layer(1, mode, 1.0)]),
            ],
            2,
        )
    };
    let (result, grid) = run(&renderer, &two(StudioBlendMode::Screen, 1.0), Some(&extended), &no_sources());
    assert_eq!(
        result.unwrap_err(),
        RenderError::Backend(BackendError::Unsupported {
            operator: EXTENDED_RANGE_MODE_OPERATOR,
            exactness: Exactness::Unsupported,
        })
    );
    assert_eq!(grid.calls, 0);
    let entry = PlanExtensions::from_layers(vec![one_layer(2, 1).with_mask_feather(0.0)])
        .with_output(OutputStage::new(SampleRange::Extended, None));
    let (result, grid) = run(&renderer, &two(StudioBlendMode::Multiply, 1.0), Some(&entry), &no_sources());
    assert_eq!(
        result.unwrap_err(),
        RenderError::Backend(BackendError::Unsupported {
            operator: EXTENDED_RANGE_COMBINATION_OPERATOR,
            exactness: Exactness::Unsupported,
        })
    );
    assert_eq!(grid.calls, 0);
    let (result, _) = run(&renderer, &two(StudioBlendMode::LinearDodgeAdd, 3.0e38), Some(&extended), &no_sources());
    assert_eq!(
        result.unwrap_err(),
        RenderError::Backend(BackendError::Unsupported {
            operator: SAMPLE_RANGE_OPERATOR,
            exactness: Exactness::Unsupported,
        })
    );
    assert_eq!(renderer.live_scratch_bytes(), 0);
}
