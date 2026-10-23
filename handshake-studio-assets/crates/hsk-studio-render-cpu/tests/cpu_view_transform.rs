//! Stage-6a view transform on render-cpu (lane SMB-PRISM-VIEW-TRANSFORM): an extended-range plan
//! whose output stage declares `DisplayTransform::View(Aces1SdrVideo)` delivers Prism's ACES SDR
//! view of pigment's extended composite, pixel for pixel, for every chunking and thread count.
#![deny(clippy::arithmetic_side_effects)]
mod common;
use common::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over_extended};
use hsk_studio_pigment::render_plan::*;
use hsk_studio_prism::ViewTransformId;
use hsk_studio_render_cpu::Renderer;

const VIEW: ViewTransformId = ViewTransformId::Aces1SdrVideo;
const CONFIGS: [(u32, usize); 4] = [(1, 1), (3, 1), (5, 4), (64, 2)];

/// Scene-referred values: 0 .. 16 in x (log steps), three hues in y; alpha 1 except row 2.
fn scene_pixel(x: u32, y: u32) -> [f32; 4] {
    let k = 2f32.powf(x as f32 - 4.0);
    match y {
        0 => [k, k, k, 1.0],
        1 => [k, 0.4 * k, 0.1 * k, 1.0],
        _ => [0.05 * k, 0.3 * k, k, 0.5],
    }
}

/// End to end: tile (scene-referred, up to 16.0) under a Multiply layer, Extended range, view
/// stage. Every delivered colour equals `ViewTransformId::apply` of pigment's extended composite
/// bit for bit, alpha is the composite's; the receipt digest is `digest_with` and differs from the
/// clip stage's; the unit stage with the view admits and delivers the view of the unit composite.
#[test]
fn view_stage_delivers_prisms_view_of_the_extended_composite_for_every_chunking() {
    let base = TileData::new(9, 3, scene_pixel);
    let top = TileData::new(4, 3, |_, _| [0.8, 0.9, 1.0, 1.0]);
    let p = plan(
        rect(0, 0, 9, 3),
        vec![
            PlanNode::Tile(base.input(1)),
            PlanNode::Tile(top.input_at(2, 5, 0)),
            group(vec![layer(0, StudioBlendMode::Normal, 1.0), layer(1, StudioBlendMode::Multiply, 0.75)]),
        ],
        2,
    );
    let src = Sources { tiles: vec![(1, base.planes()), (2, top.planes())], masks: Vec::new() };
    let oracle = |x: i64, y: i64| -> [f32; 4] {
        let site = PixelSite { x, y, seed: 0 };
        let s = scene_pixel(u32::try_from(x).unwrap(), u32::try_from(y).unwrap());
        let mut out =
            composite_over_extended(StudioBlendMode::Normal, Rgba::TRANSPARENT, Rgba::new([s[0], s[1], s[2]], s[3]), 1.0, site)
                .unwrap();
        if x >= 5 {
            out = composite_over_extended(StudioBlendMode::Multiply, out, Rgba::new([0.8, 0.9, 1.0], 1.0), 0.75, site)
                .unwrap();
        }
        let v = VIEW.apply(out.rgb).unwrap();
        [v[0], v[1], v[2], out.a]
    };
    let view = PlanExtensions::default()
        .with_output(OutputStage::new(SampleRange::Extended, Some(DisplayTransform::View(VIEW))));
    let clip = PlanExtensions::default()
        .with_output(OutputStage::new(SampleRange::Extended, Some(DisplayTransform::Clip)));
    assert_ne!(view.digest_with(&p).unwrap(), clip.digest_with(&p).unwrap());
    // Hand anchor: (8, 0) is the 16.0 neutral, above the ODT white point (c5(0.18 * 2^6.5) at
    // about 16.29 ACES lands just under it; 16.0 is not above it), checked as <= 1.
    assert!(oracle(8, 0)[..3].iter().all(|v| *v > 0.9 && *v <= 1.0));
    let request = req(Exactness::Approximate, 1 << 24);
    let mut seen_mid = false;
    for (chunk, threads) in CONFIGS {
        let renderer = Renderer::with_threads(chunk, threads).unwrap();
        let mut grid = Grid::new(p.target.extent);
        let receipt = renderer
            .execute_extended_detailed(&p, &view, &request, &src, &CancellationToken::default(), &mut grid)
            .unwrap_or_else(|e| panic!("({chunk},{threads}): {e}"));
        assert_eq!(receipt.plan_digest, view.digest_with(&p).unwrap());
        for y in 0..3i64 {
            for x in 0..9i64 {
                let want = oracle(x, y);
                let got = grid.at(x, y);
                assert_eq!(got.map(f32::to_bits), want.map(f32::to_bits), "({chunk},{threads}) ({x},{y})");
                assert!(got[..3].iter().all(|v| (0.0..=1.0).contains(v)));
                seen_mid |= got[..3].iter().any(|v| *v > 0.05 && *v < 0.95);
            }
        }
        assert_eq!(renderer.live_scratch_bytes(), 0);
    }
    assert!(seen_mid);
    // Unit range with the view: admitted; the view of the unit composite.
    let unit_base = TileData::new(9, 3, |x, y| scene_pixel(x, y).map(|v| v.clamp(0.0, 1.0)));
    let up = plan(rect(0, 0, 9, 3), vec![PlanNode::Tile(unit_base.input(1))], 0);
    let usrc = Sources { tiles: vec![(1, unit_base.planes())], masks: Vec::new() };
    let unit_view = PlanExtensions::default()
        .with_output(OutputStage::new(SampleRange::Unit, Some(DisplayTransform::View(VIEW))));
    let mut grid = Grid::new(up.target.extent);
    Renderer::with_threads(4, 2)
        .unwrap()
        .execute_extended_detailed(&up, &unit_view, &request, &usrc, &CancellationToken::default(), &mut grid)
        .unwrap();
    let s = scene_pixel(3, 1).map(|v| v.clamp(0.0, 1.0));
    let v = VIEW.apply([s[0], s[1], s[2]]).unwrap();
    assert_eq!(grid.at(3, 1)[..3].iter().map(|f| f.to_bits()).collect::<Vec<_>>(), v.map(f32::to_bits).to_vec());
}
