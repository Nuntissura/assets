//! Stage-6a views from an explicit scene input on render-cpu (lane SMB-PRISM-VIEW-TRANSFORM-2):
//! `DisplayTransform::ViewFrom(ViewTransform)` with ACEScg input, the Display P3 SDR view and a PQ
//! HDR view deliver Prism's view of pigment's extended composite bit for bit; the view and its
//! input are in the digest; pigment's DESCRIPTOR names the stage.
#![deny(clippy::arithmetic_side_effects)]
mod common;
use common::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over_extended};
use hsk_studio_pigment::render_plan::*;
use hsk_studio_prism::{SceneInput, ViewTransform, ViewTransformId};
use hsk_studio_render_cpu::Renderer;

fn scene_pixel(x: u32, y: u32) -> [f32; 4] {
    let k = 2f32.powf(x as f32 - 4.0);
    match y {
        0 => [k, k, k, 1.0],
        1 => [k, 0.4 * k, 0.1 * k, 1.0],
        _ => [0.05 * k, 0.3 * k, k, 0.5],
    }
}

#[test]
fn view_from_scene_inputs_and_new_views_match_prism_on_the_composite() {
    let base = TileData::new(9, 3, scene_pixel);
    let p = plan(rect(0, 0, 9, 3), vec![PlanNode::Tile(base.input(1)), group(vec![layer(0, StudioBlendMode::Normal, 1.0)])], 1);
    let src = Sources { tiles: vec![(1, base.planes())], masks: Vec::new() };
    let request = req(Exactness::Approximate, 1 << 24);
    let stage = |d: DisplayTransform| PlanExtensions::default().with_output(OutputStage::new(SampleRange::Extended, Some(d)));
    let views = [
        ViewTransform::new(ViewTransformId::Aces1SdrVideo, SceneInput::AcesCg),
        ViewTransform::new(ViewTransformId::Aces1SdrVideoDisplayP3, SceneInput::LinearSrgb),
        ViewTransform::new(ViewTransformId::Aces1HdrVideo1000Nits, SceneInput::AcesCg),
        ViewTransform::new(ViewTransformId::Aces1HdrVideo4000Nits, SceneInput::Aces2065_1),
    ];
    let mut digests = Vec::new();
    let mut above_one = 0usize;
    for vt in views {
        let env = stage(DisplayTransform::ViewFrom(vt));
        let digest = env.digest_with(&p).unwrap();
        assert!(!digests.contains(&digest), "{}@{} digest collides", vt.view.key(), vt.input.key());
        digests.push(digest);
        for (chunk, threads) in [(1u32, 1usize), (4, 3), (64, 2)] {
            let renderer = Renderer::with_threads(chunk, threads).unwrap();
            let mut grid = Grid::new(p.target.extent);
            let receipt = renderer
                .execute_extended_detailed(&p, &env, &request, &src, &CancellationToken::default(), &mut grid)
                .unwrap_or_else(|e| panic!("{} ({chunk},{threads}): {e}", vt.view.key()));
            assert_eq!(receipt.plan_digest, digest);
            assert!(receipt.exactness.operators.iter().any(|o| o.key == "display_view_transform"));
            for y in 0..3i64 {
                for x in 0..9i64 {
                    let s = scene_pixel(u32::try_from(x).unwrap(), u32::try_from(y).unwrap());
                    let c = composite_over_extended(
                        StudioBlendMode::Normal,
                        Rgba::TRANSPARENT,
                        Rgba::new([s[0], s[1], s[2]], s[3]),
                        1.0,
                        PixelSite { x, y, seed: 0 },
                    )
                    .unwrap();
                    let v = vt.apply(c.rgb).unwrap();
                    let want = [v[0], v[1], v[2], c.a];
                    let got = grid.at(x, y);
                    assert_eq!(got.map(f32::to_bits), want.map(f32::to_bits), "{} ({chunk},{threads}) ({x},{y})", vt.view.key());
                    above_one = above_one.saturating_add(usize::from(got[..3].iter().any(|v| *v > 1.0)));
                }
            }
        }
    }
    // HDR views deliver values above 1 (units of 100 cd/m2); SDR views stay in [0, 1].
    assert!(above_one > 0);
    // `View(id)` reads linear sRGB: the same pixels as `ViewFrom(id @ linear_srgb)`, a different
    // spelling in the digest.
    let id = ViewTransformId::Aces1SdrVideo;
    let a = stage(DisplayTransform::View(id));
    let b = stage(DisplayTransform::ViewFrom(ViewTransform::new(id, SceneInput::LinearSrgb)));
    assert_eq!(DisplayTransform::View(id).view(), DisplayTransform::ViewFrom(ViewTransform::new(id, SceneInput::LinearSrgb)).view());
    assert_eq!(DisplayTransform::Clip.view(), None);
    let run = |env: &PlanExtensions| {
        let mut grid = Grid::new(p.target.extent);
        Renderer::with_threads(4, 2)
            .unwrap()
            .execute_extended_detailed(&p, env, &request, &src, &CancellationToken::default(), &mut grid)
            .unwrap();
        grid.px.iter().map(|q| q.map(f32::to_bits)).collect::<Vec<_>>()
    };
    assert_eq!(run(&a), run(&b));
    assert_ne!(a.digest_with(&p).unwrap(), b.digest_with(&p).unwrap());
    // Pigment's DESCRIPTOR names the stage (item 4 of the lane).
    assert!(DESCRIPTOR.contains("display_view_transform") && DESCRIPTOR.contains("ViewFrom"));
}
