//! Lane SMB-PS-ADJUST-STYLES-2: pattern overlay on the CPU backend. The pattern pixels are a plan
//! tile source (verified like a tile input, decoded once in `prepare`, charged up front); the
//! layer renders through pigment's windowed evaluator, bit-identical to pigment's
//! `render_layer_reference_with` for every chunking and thread count. Unresolved, mismatching and
//! over-budget patterns are typed refusals before any sink call. Photoshop parity UNVERIFIED (D5).
use hsk_studio_pigment::effect_math::{
    EffectOp, EffectPlane, LayerParams, PatternOverlayOp, PatternPixels, PatternRef, render_layer_reference_with,
};
use hsk_studio_pigment::render_plan::*;
use hsk_studio_render_cpu::Renderer;

mod common;
use common::*;

const NORMAL: StudioBlendMode = StudioBlendMode::Normal;

fn content_px(x: u32, y: u32) -> [f32; 4] {
    [0.2, 0.3 + 0.01 * (x % 7) as f32, 0.4, ((x + 2 * y) % 5) as f32 / 4.0]
}
fn pattern_px(x: u32, y: u32) -> [f32; 4] {
    [x as f32 / 4.0, y as f32 / 3.0, 0.5, if (x + y).is_multiple_of(3) { 0.5 } else { 1.0 }]
}
struct Px(Vec<[f32; 4]>);
impl PatternPixels for Px {
    fn pattern(&self, key: SourceKey) -> Option<&[[f32; 4]]> {
        (key == SourceKey(9)).then_some(self.0.as_slice())
    }
}

#[test]
fn pattern_overlay_matches_pigment_for_every_chunking() {
    let extent = rect(0, 0, 13, 9);
    let content = TileData::new(13, 9, content_px);
    let pattern = TileData::new(4, 3, pattern_px);
    let pp = pattern.planes();
    let r = PatternRef { key: SourceKey(9), width: 4, height: 3, colour_sha256: pp.colour_sha256, alpha_sha256: pp.alpha_sha256 };
    let p = plan(
        extent,
        vec![solid(extent, [0.9, 0.8, 0.1, 1.0]), PlanNode::Tile(content.input(1)), group(vec![layer(0, NORMAL, 1.0), layer(1, StudioBlendMode::Multiply, 0.9)])],
        2,
    );
    let sources = Sources { tiles: vec![(1, content.planes()), (9, pattern.planes())], masks: vec![] };
    let ops = [
        EffectOp::PatternOverlay(PatternOverlayOp::new(StudioBlendMode::Screen, 0.7, r).with_scale(1.5).with_phase([1.0, -2.0])),
        EffectOp::PatternOverlay(PatternOverlayOp::new(NORMAL, 0.5, r).with_link(false)),
    ];
    let ext = envelope(vec![one_layer(2, 1).with_effects(ops.to_vec())]);
    // pigment's reference on the extent plane (the layer covers the extent: plane and layer bounds
    // coincide)
    let back = EffectPlane::new(13, 9, vec![[0.9, 0.8, 0.1]; 117], vec![1.0; 117]).unwrap();
    let cp: Vec<[f32; 4]> = (0..117).map(|i| content_px(i % 13, i / 13)).collect();
    let cplane = EffectPlane::new(13, 9, cp.iter().map(|p| [p[0], p[1], p[2]]).collect(), cp.iter().map(|p| p[3]).collect()).unwrap();
    let pixels: Vec<[f32; 4]> = (0..12).map(|i| pattern_px(i % 4, i / 4)).collect();
    let want = render_layer_reference_with(&back, &cplane, LayerParams { mode: StudioBlendMode::Multiply, opacity: 0.9, fill_opacity: 1.0 }, &ops, &Px(pixels)).unwrap();
    for (chunk, threads) in [(13, 1), (4, 1), (3, 2)] {
        let (result, grid) = run_ext(&Renderer::with_threads(chunk, threads).unwrap(), &p, &ext, Exactness::Approximate, 1 << 24, &sources);
        result.unwrap_or_else(|e| panic!("chunk {chunk}: {e:?}"));
        for y in 0..9u32 {
            for x in 0..13u32 {
                let w = want.pixel(x, y).unwrap();
                let g = grid.at(i64::from(x), i64::from(y));
                assert_eq!([g[0], g[1], g[2], g[3]].map(f32::to_bits), [w.rgb[0], w.rgb[1], w.rgb[2], w.a.min(1.0)].map(f32::to_bits), "({x}, {y}) chunk {chunk}");
            }
        }
    }
    let backend: &dyn RenderBackend = &Renderer::new(4).unwrap();
    assert_eq!(backend.extension_exactness(ExtOperator::Effect(hsk_studio_pigment::effect_math::EffectKind::PatternOverlay)), Exactness::Approximate);
    // source verification and charging (one test per slice budget: run from here)
    pattern_sources_are_verified_and_charged_before_any_sink_call();
}

fn pattern_sources_are_verified_and_charged_before_any_sink_call() {
    let extent = rect(0, 0, 6, 4);
    let content = TileData::new(6, 4, content_px);
    let pattern = TileData::new(4, 3, pattern_px);
    let pp = pattern.planes();
    let good = PatternRef { key: SourceKey(9), width: 4, height: 3, colour_sha256: pp.colour_sha256, alpha_sha256: pp.alpha_sha256 };
    let p = plan(extent, vec![PlanNode::Tile(content.input(1)), group(vec![layer(0, NORMAL, 1.0)])], 1);
    let run = |r: PatternRef, sources: &Sources<'_>, budget: u64| {
        let ext = envelope(vec![one_layer(1, 0).with_effects(vec![EffectOp::PatternOverlay(PatternOverlayOp::new(NORMAL, 1.0, r))])]);
        let (result, grid) = run_ext(&Renderer::new(4).unwrap(), &p, &ext, Exactness::Approximate, budget, sources);
        (result, grid.calls)
    };
    let with = Sources { tiles: vec![(1, content.planes()), (9, pattern.planes())], masks: vec![] };
    let without = Sources { tiles: vec![(1, content.planes())], masks: vec![] };
    assert!(run(good, &with, 1 << 24).0.is_ok());
    assert_eq!(run(good, &without, 1 << 24), (Err(BackendError::UnresolvedSource(SourceKey(9))), 0));
    // a declared size or hash that disagrees with the source
    let (r, calls) = run(PatternRef { width: 5, ..good }, &with, 1 << 24);
    assert!(matches!(r, Err(BackendError::SourceMismatch(_))) && calls == 0, "{r:?}");
    let (r, calls) = run(PatternRef { colour_sha256: [0; 32], ..good }, &with, 1 << 24);
    assert!(matches!(r, Err(BackendError::SourceMismatch(_))) && calls == 0, "{r:?}");
    // a hostile declared size is charged before anything is read: 4096 x 4096 x 16 bytes
    let (r, calls) = run(PatternRef { width: 4096, height: 4096, ..good }, &with, 1 << 24);
    assert!(matches!(r, Err(BackendError::BudgetExceeded { .. })) && calls == 0, "{r:?}");
}
