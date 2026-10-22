//! Lane SMB-PS-FILL-LAYERS: the plan shape composite lowers a gradient or pattern fill layer to
//! (composite `tests/fill_layer_lowering.rs`: an opaque white `Solid` over the fill rectangle, a
//! plan layer in Normal mode with fill opacity 0 that keeps the layer's opacity, and one gradient
//! or pattern overlay effect carrying the layer's blend mode and fill opacity) rendered on the CPU
//! backend. The result must equal pigment's single-owner reference bit for bit for every chunking
//! and thread count, equal the value derived by hand from the gradient definition, and be one
//! stable digest. Photoshop parity UNVERIFIED (no D5 case covers fill layers).
use hsk_studio_pigment::effect_math::{
    EffectOp, EffectPlane, GradientOverlayOp, GradientStopOp, GradientStyle, LayerParams, NoPatterns, PatternOverlayOp,
    PatternPixels, PatternRef, render_layer_reference_with,
};
use hsk_studio_pigment::render_plan::*;
use hsk_studio_render_cpu::Renderer;

mod common;
use common::*;

const W: u32 = 13;
const H: u32 = 9;
const BACKDROP: [f32; 4] = [0.9, 0.8, 0.1, 1.0];

/// Backdrop layer, then the fill layer exactly as composite lowers it: white shape, Normal, opacity
/// `opacity`, fill 0, the overlay `op` in the envelope.
fn fill_plan(opacity: f32) -> RenderPlan {
    let extent = rect(0, 0, W, H);
    plan(
        extent,
        vec![
            solid(extent, BACKDROP),
            solid(extent, [1.0; 4]),
            group(vec![
                layer(0, StudioBlendMode::Normal, 1.0),
                Layer { fill_opacity: 0.0, ..layer(1, StudioBlendMode::Normal, opacity) },
            ]),
        ],
        2,
    )
}
fn backdrop_plane() -> EffectPlane {
    let n = (W * H) as usize;
    EffectPlane::new(W, H, vec![[BACKDROP[0], BACKDROP[1], BACKDROP[2]]; n], vec![1.0; n]).unwrap()
}
fn shape_plane() -> EffectPlane {
    let n = (W * H) as usize;
    EffectPlane::new(W, H, vec![[1.0; 3]; n], vec![1.0; n]).unwrap()
}
fn digest(grid: &Grid) -> String {
    let mut bytes = Vec::new();
    for y in 0..i64::from(H) {
        for x in 0..i64::from(W) {
            for c in grid.at(x, y) {
                bytes.extend_from_slice(&c.to_bits().to_le_bytes());
            }
        }
    }
    hsk_studio_prism::hash_hex(&hsk_studio_prism::profile_hash(&bytes))
}
fn render_all(p: &RenderPlan, ext: &PlanExtensions, sources: &Sources<'_>) -> Vec<(Grid, u32, usize)> {
    [(13u32, 1usize), (4, 1), (3, 2)]
        .into_iter()
        .map(|(chunk, threads)| {
            let (result, grid) = run_ext(&Renderer::with_threads(chunk, threads).unwrap(), p, ext, Exactness::Approximate, 1 << 24, sources);
            result.unwrap_or_else(|e| panic!("chunk {chunk}: {e:?}"));
            (grid, chunk, threads)
        })
        .collect()
}
fn assert_reference(grids: &[(Grid, u32, usize)], want: &EffectPlane) {
    for (grid, chunk, _) in grids {
        for y in 0..H {
            for x in 0..W {
                let w = want.pixel(x, y).unwrap();
                let g = grid.at(i64::from(x), i64::from(y));
                assert_eq!(
                    [g[0], g[1], g[2], g[3]].map(f32::to_bits),
                    [w.rgb[0], w.rgb[1], w.rgb[2], w.a.min(1.0)].map(f32::to_bits),
                    "({x}, {y}) chunk {chunk}"
                );
            }
        }
    }
    let first = digest(&grids[0].0);
    assert!(grids.iter().all(|(g, _, _)| digest(g) == first), "digest differs between chunkings");
    println!("FILL_LAYER_DIGEST {first}");
}

#[test]
fn gradient_fill_renders_bit_identical_to_the_reference_and_to_the_hand_value() {
    let stops = [
        GradientStopOp { position: 0.0, colour: [0.0; 3], opacity: 1.0 },
        GradientStopOp { position: 1.0, colour: [1.0; 3], opacity: 0.5 },
    ];
    // Layer: opacity 0.8; effect: Multiply at fill opacity 0.6 (the layer's mode and fill).
    let op = GradientOverlayOp::new(StudioBlendMode::Multiply, 0.6, GradientStyle::Linear, 0.0, &stops).unwrap();
    let ops = [EffectOp::GradientOverlay(op)];
    let p = fill_plan(0.8);
    let ext = envelope(vec![one_layer(2, 1).with_effects(ops.to_vec())]);
    let grids = render_all(&p, &ext, &no_sources());
    // The reference: layer 0 over nothing is the backdrop; then the fill layer from the plan alone.
    let want = render_layer_reference_with(
        &backdrop_plane(),
        &shape_plane(),
        LayerParams { mode: StudioBlendMode::Normal, opacity: 0.8, fill_opacity: 0.0 },
        &ops,
        &NoPatterns,
    )
    .unwrap();
    assert_reference(&grids, &want);
    // Hand value: linear angle 0 is t = (x + 0.5) / W, stop opacity 1 - 0.5 t, coverage = effect
    // opacity * layer opacity * stop opacity, Multiply over an opaque backdrop:
    // result = Cb * (1 - a + a * t).
    for x in [0i64, 4, 12] {
        let t = (x as f32 + 0.5) / W as f32;
        let a = 0.6 * 0.8 * (1.0 - 0.5 * t);
        let got = grids[0].0.at(x, 3);
        for c in 0..3 {
            let want = BACKDROP[c] * (1.0 - a + a * t);
            assert!((got[c] - want).abs() < 2e-6, "x {x} channel {c}: got {} want {want}", got[c]);
        }
        assert_eq!(got[3], 1.0);
    }
    // Capability: pigment's class, never a better one.
    let backend: &dyn RenderBackend = &Renderer::new(4).unwrap();
    assert_eq!(
        backend.extension_exactness(ExtOperator::Effect(hsk_studio_pigment::effect_math::EffectKind::GradientOverlay)),
        Exactness::Approximate
    );
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
fn pattern_fill_renders_bit_identical_to_the_reference() {
    let pattern = TileData::new(4, 3, pattern_px);
    let pp = pattern.planes();
    let r = PatternRef { key: SourceKey(9), width: 4, height: 3, colour_sha256: pp.colour_sha256, alpha_sha256: pp.alpha_sha256 };
    let op = PatternOverlayOp::new(StudioBlendMode::Normal, 0.7, r).with_scale(1.5).with_phase([1.0, -2.0]);
    let ops = [EffectOp::PatternOverlay(op)];
    let p = fill_plan(0.9);
    let ext = envelope(vec![one_layer(2, 1).with_effects(ops.to_vec())]);
    let sources = Sources { tiles: vec![(9, pattern.planes())], masks: vec![] };
    let grids = render_all(&p, &ext, &sources);
    let pixels: Vec<[f32; 4]> = (0..12).map(|i| pattern_px(i % 4, i / 4)).collect();
    let want = render_layer_reference_with(
        &backdrop_plane(),
        &shape_plane(),
        LayerParams { mode: StudioBlendMode::Normal, opacity: 0.9, fill_opacity: 0.0 },
        &ops,
        &Px(pixels.clone()),
    )
    .unwrap();
    assert_reference(&grids, &want);
    // Hand value: the pattern pixel at floor((x + 0.5 - phase) / scale) wrapped, with its own alpha
    // multiplying the coverage 0.7 * 0.9 (Normal over an opaque backdrop).
    let (x, y) = (6i64, 4i64);
    let u = ((x as f64 + 0.5 - 1.0) / 1.5).floor().rem_euclid(4.0) as u32;
    let v = ((y as f64 + 0.5 + 2.0) / 1.5).floor().rem_euclid(3.0) as u32;
    let px = pattern_px(u, v);
    let a = 0.7 * 0.9 * px[3];
    let got = grids[0].0.at(x, y);
    for c in 0..3 {
        let want = BACKDROP[c] * (1.0 - a) + px[c] * a;
        assert!((got[c] - want).abs() < 2e-6, "channel {c}: got {} want {want}", got[c]);
    }
}
