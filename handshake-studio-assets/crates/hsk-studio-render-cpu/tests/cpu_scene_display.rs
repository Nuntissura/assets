//! Deprecated renderer alias `with_display_transform` (lane SMB-RENDER-SCENE-DISPLAY, moved onto
//! the plan's output stage by lane SMB-PIGMENT-EXTENDED-RANGE): a plan without its own stage runs as
//! if it declared `OutputStage { Extended, Some(Clip) }`.
//!
//! Scene: a 7x5 developed-raw-like tile whose colour runs from -0.5 to 4.0 (alpha 1), with an
//! in-range 4x3 tile composited on top in Multiply at opacity 0.8 and alpha 0.5. The oracle is
//! pigment's `composite_over_extended` (the single blend owner) of the unclipped sources, then the
//! stage-6a clamp, computed here independently of render-cpu.
#![deny(clippy::arithmetic_side_effects)]
#![allow(deprecated)]
mod common;
use common::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over_extended};
use hsk_studio_pigment::render_plan::*;
use hsk_studio_render_cpu::{DisplayTransform, RenderError, Renderer, SourceFault};

const BUDGET: u64 = 1 << 24;

fn scene_pixel(x: u32, y: u32) -> [f32; 4] {
    let (x, y) = (x as f32, y as f32);
    [x * 0.5 - 0.5, 1.0 + y * 0.75, 0.3, 1.0]
}
fn top_pixel(_: u32, _: u32) -> [f32; 4] {
    [0.6, 0.2, 0.9, 0.5]
}
fn scene_plan(base: &TileData, top: &TileData) -> RenderPlan {
    plan(
        rect(0, 0, 7, 5),
        vec![
            PlanNode::Tile(base.input(1)),
            PlanNode::Tile(top.input_at(2, 2, 1)),
            group(vec![
                layer(0, StudioBlendMode::Normal, 1.0),
                layer(1, StudioBlendMode::Multiply, 0.8),
            ]),
        ],
        2,
    )
}
fn sources<'a>(base: &'a TileData, top: &'a TileData) -> Sources<'a> {
    Sources {
        tiles: vec![(1, base.planes()), (2, top.planes())],
        masks: Vec::new(),
    }
}
fn rgba(p: [f32; 4]) -> Rgba {
    Rgba {
        rgb: [p[0], p[1], p[2]],
        a: p[3],
    }
}
/// Pigment extended composite of the sources at `(x, y)`, then the stage-6a clamp.
fn oracle(x: i64, y: i64) -> [f32; 4] {
    let site = PixelSite { x, y, seed: 0 };
    let base = scene_pixel(u32::try_from(x).unwrap(), u32::try_from(y).unwrap());
    let mut out =
        composite_over_extended(StudioBlendMode::Normal, Rgba::TRANSPARENT, rgba(base), 1.0, site)
            .unwrap();
    if (2..6).contains(&x) && (1..4).contains(&y) {
        out = composite_over_extended(StudioBlendMode::Multiply, out, rgba(top_pixel(0, 0)), 0.8, site)
            .unwrap();
    }
    let c = |v: f32| v.clamp(0.0, 1.0);
    [c(out.rgb[0]), c(out.rgb[1]), c(out.rgb[2]), out.a]
}
fn run(
    renderer: &Renderer,
    plan: &RenderPlan,
    min: Exactness,
    budget: u64,
    sources: &dyn PlanSources,
) -> (Result<RenderReceipt, RenderError>, Grid) {
    let mut grid = Grid::new(plan.target.extent);
    let result = renderer.execute_detailed(
        plan,
        &req(min, budget),
        sources,
        &CancellationToken::default(),
        &mut grid,
    );
    (result, grid)
}
fn clip_renderer(chunk: u32, threads: usize) -> Renderer {
    Renderer::with_threads(chunk, threads)
        .unwrap()
        .with_display_transform(DisplayTransform::Clip)
}
fn ledger_class(receipt: &RenderReceipt, key: &str) -> Option<Exactness> {
    receipt
        .exactness
        .operators
        .iter()
        .find(|o| o.key == key)
        .map(|o| o.exactness)
}

/// The alias on known inputs: every pixel equals pigment's extended composite then the clamp, bit
/// for bit (hand values: (6,4) is [2.5, 4.0, 0.3] -> [1, 1, 0.3]; (3,2) is [1.0, 2.5, 0.3] under
/// Multiply 0.8 x alpha 0.5 = 0.4 of [0.6, 0.2, 0.9] -> [0.84, 1.7, 0.288] -> [0.84, 1, 0.288]
/// within 1 ulp of the f32 chain), for every chunking and thread count; the receipt records
/// `sample_range_extended` Approximate and `display_clip` Exact, and its digest is that of the
/// envelope the alias executes.
#[test]
fn clip_matches_the_pigment_composite_of_clipped_sources_for_every_chunking_and_thread_count() {
    let (base, top) = (TileData::new(7, 5, scene_pixel), TileData::new(4, 3, top_pixel));
    let plan = scene_plan(&base, &top);
    let src = sources(&base, &top);
    assert_eq!(oracle(6, 4), [1.0, 1.0, 0.3, 1.0]);
    let hand = oracle(3, 2);
    for (got, want) in hand.iter().zip([0.84f32, 1.0, 0.288, 1.0]) {
        assert!((got - want).abs() <= 2.0 * f32::EPSILON, "{hand:?}");
    }
    let declared = PlanExtensions::default().with_output(OutputStage::new(
        SampleRange::Extended,
        Some(DisplayTransform::Clip),
    ));
    for (chunk, threads) in [(1, 1), (3, 1), (5, 4), (64, 1), (4, 8), (7, 2)] {
        let renderer = clip_renderer(chunk, threads);
        let (result, grid) = run(&renderer, &plan, Exactness::Approximate, BUDGET, &src);
        let receipt = result.unwrap_or_else(|e| panic!("({chunk},{threads}): {e}"));
        for y in 0..5i64 {
            for x in 0..7i64 {
                let (got, want) = (grid.at(x, y), oracle(x, y));
                assert_eq!(
                    got.map(f32::to_bits),
                    want.map(f32::to_bits),
                    "({chunk},{threads}) pixel ({x},{y}): {got:?} vs {want:?}"
                );
            }
        }
        assert_eq!(ledger_class(&receipt, EXTENDED_RANGE_OPERATOR), Some(Exactness::Approximate));
        assert_eq!(ledger_class(&receipt, DISPLAY_CLIP_OPERATOR), Some(Exactness::Exact));
        assert_eq!(receipt.plan_digest, declared.digest_with(&plan).unwrap());
        assert_eq!(renderer.live_scratch_bytes(), 0);
    }
}

/// Without the alias or a plan stage the refusal is unchanged; under the alias a request floor
/// above Approximate is refused with `sample_range_extended`, before the first sink call.
#[test]
fn undeclared_keeps_the_refusal_and_declared_clip_refusals_are_typed() {
    let (base, top) = (TileData::new(7, 5, scene_pixel), TileData::new(4, 3, top_pixel));
    let plan = scene_plan(&base, &top);
    let src = sources(&base, &top);

    let plain = Renderer::new(64).unwrap();
    assert_eq!(plain.display_transform(), None);
    let (result, grid) = run(&plain, &plan, Exactness::Approximate, BUDGET, &src);
    assert_eq!(
        result.unwrap_err(),
        RenderError::Backend(BackendError::Unsupported {
            operator: SAMPLE_RANGE_OPERATOR,
            exactness: Exactness::Unsupported,
        })
    );
    assert_eq!(grid.calls, 0);
    let mut grid = Grid::new(plan.target.extent);
    let port = plain.execute(
        &plan,
        &req(Exactness::Approximate, BUDGET),
        &src,
        &CancellationToken::default(),
        &mut grid,
    );
    assert_eq!(port.unwrap_err().code(), "unsupported_operator");
    assert_eq!(grid.calls, 0);

    let clip = clip_renderer(64, 1);
    assert_eq!(clip.display_transform(), Some(DisplayTransform::Clip));
    for floor in [Exactness::Exact, Exactness::Fitted] {
        let (result, grid) = run(&clip, &plan, floor, BUDGET, &src);
        assert_eq!(
            result.unwrap_err(),
            RenderError::Backend(BackendError::Unsupported {
                operator: EXTENDED_RANGE_OPERATOR,
                exactness: Exactness::Approximate,
            })
        );
        assert_eq!(grid.calls, 0);
    }
    assert_eq!(clip.live_scratch_bytes(), 0);
}

/// Hostile samples under the alias: NaN, +inf and -inf keep `source_nonfinite`, alpha above 1
/// keeps `source_alpha_range`, a colour plane that does not hash to its declaration keeps
/// `source_hash_mismatch`; finite negative, -0.0, f32::MAX and subnormal samples pass the extended
/// range and leave the stage-6a clip as 0, 0, 1 and unchanged. No refusal reaches the sink.
#[test]
fn hostile_samples_under_a_declared_clip_stay_typed_or_clip() {
    let clip = clip_renderer(2, 3);
    for (bad, fault) in [
        (f32::NAN, SourceFault::Nonfinite),
        (f32::INFINITY, SourceFault::Nonfinite),
        (f32::NEG_INFINITY, SourceFault::Nonfinite),
    ] {
        let tile = TileData::new(3, 2, |x, y| if (x, y) == (1, 1) { [5.0, bad, 0.2, 1.0] } else { [5.0, 0.1, 0.2, 1.0] });
        let p = plan(rect(0, 0, 3, 2), vec![PlanNode::Tile(tile.input(4))], 0);
        let src = Sources { tiles: vec![(4, tile.planes())], masks: Vec::new() };
        let (result, grid) = run(&clip, &p, Exactness::Approximate, BUDGET, &src);
        assert_eq!(result.unwrap_err(), RenderError::Source { key: SourceKey(4), fault });
        assert_eq!(grid.calls, 0);
    }
    let alpha = TileData::new(3, 2, |_, _| [5.0, 0.1, 0.2, 1.5]);
    let p = plan(rect(0, 0, 3, 2), vec![PlanNode::Tile(alpha.input(4))], 0);
    let src = Sources { tiles: vec![(4, alpha.planes())], masks: Vec::new() };
    let (result, grid) = run(&clip, &p, Exactness::Approximate, BUDGET, &src);
    assert_eq!(result.unwrap_err(), RenderError::Source { key: SourceKey(4), fault: SourceFault::AlphaRange });
    assert_eq!(grid.calls, 0);

    let declared = TileData::new(3, 2, |_, _| [0.5, 0.5, 0.5, 1.0]);
    let actual = TileData::new(3, 2, |_, _| [7.0, 0.5, 0.5, 1.0]);
    let p = plan(rect(0, 0, 3, 2), vec![PlanNode::Tile(declared.input(4))], 0);
    let mut forged = actual.planes();
    forged.colour_sha256 = declared.planes().colour_sha256;
    let src = Sources { tiles: vec![(4, forged)], masks: Vec::new() };
    let (result, grid) = run(&clip, &p, Exactness::Approximate, BUDGET, &src);
    assert_eq!(result.unwrap_err(), RenderError::Source { key: SourceKey(4), fault: SourceFault::HashMismatch });
    assert_eq!(grid.calls, 0);
    assert_eq!(clip.live_scratch_bytes(), 0);

    let tiny = f32::from_bits(1); // smallest positive subnormal
    let values = [[-3.0, -0.0, f32::MAX, 1.0], [tiny, -1e-30, 1.0000001, 1.0]];
    let tile = TileData::new(2, 1, |x, _| values[x as usize]);
    let p = plan(rect(0, 0, 2, 1), vec![PlanNode::Tile(tile.input(4))], 0);
    let src = Sources { tiles: vec![(4, tile.planes())], masks: Vec::new() };
    let (result, grid) = run(&clip, &p, Exactness::Approximate, BUDGET, &src);
    let receipt = result.unwrap();
    assert_eq!(ledger_class(&receipt, DISPLAY_CLIP_OPERATOR), Some(Exactness::Exact));
    for (x, v) in values.iter().enumerate() {
        let want: Vec<f32> = v[..3].iter().map(|c| c.clamp(0.0, 1.0)).collect();
        let got = grid.at(i64::try_from(x).unwrap(), 0);
        assert_eq!(&got[..3], &want[..], "pixel {x}");
        assert_eq!(got[3], 1.0);
    }
    assert_eq!(grid.at(0, 0)[..3], [0.0, 0.0, 1.0]);
    assert_eq!(grid.at(1, 0)[0].to_bits(), tiny.to_bits());
}
