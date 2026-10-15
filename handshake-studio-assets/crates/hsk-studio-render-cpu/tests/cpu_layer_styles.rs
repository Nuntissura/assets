//! Lane SMB-PS-ADJUST-STYLES slice 2b: the CPU backend renders satin, bevel & emboss and gradient
//! overlay through `execute_extended` bit-identical (`f32::to_bits`) to pigment's
//! `render_layer_reference`, for several chunkings and worker counts, and reports pigment's class.
//! Hand values: pigment `tests/layer_styles.rs`. Photoshop parity UNVERIFIED (D5).
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::effect_math::{
    BevelEmbossOp, BevelPart, BevelShape, BevelStyle, EffectKind, EffectOp, EffectPlane,
    GradientOverlayOp, GradientStopOp, GradientStyle, LayerParams, SatinOp,
    render_layer_reference,
};
use hsk_studio_pigment::render_plan::*;
use hsk_studio_render_cpu::Renderer;

mod common;
use common::*;

const NORMAL: StudioBlendMode = StudioBlendMode::Normal;

/// Straight RGBA of a tile pixel: a colour ramp and a partial alpha with transparent holes.
fn tile_px(x: u32, y: u32) -> [f32; 4] {
    let f = |n: u32, d: u32| f32::from(u16::try_from(n % d).unwrap()) / f32::from(u16::try_from(d).unwrap());
    let hole = x.wrapping_mul(3).wrapping_add(y.wrapping_mul(5)).is_multiple_of(11);
    let alpha = if hole { 0.0 } else { 0.2 + 0.8 * f(x.wrapping_add(y), 4) };
    [f(x, 13), f(y, 9), 0.5, alpha]
}

fn plan_with(extent: Rect, content: PlanNode) -> RenderPlan {
    plan(
        extent,
        vec![
            solid(extent, [0.2, 0.4, 0.6, 1.0]),
            content,
            group(vec![
                layer(0, NORMAL, 1.0),
                Layer {
                    fill_opacity: 0.6,
                    ..layer(1, NORMAL, 0.9)
                },
            ]),
        ],
        2,
    )
}

/// Renders `ops` on a `tw x th` tile placed at `at` in a `w x h` extent, compares every pixel with
/// pigment's reference over the whole extent, for several chunkings and worker counts.
fn check(ops: &[EffectOp], (w, h): (u32, u32), (tw, th): (u32, u32), at: (i64, i64)) {
    let tile = TileData::new(tw, th, tile_px);
    let sources = Sources { tiles: vec![(1, tile.planes())], masks: vec![] };
    let p = plan_with(rect(0, 0, w, h), PlanNode::Tile(tile.input_at(1, at.0, at.1)));
    let n = usize::try_from(w * h).unwrap();
    let (mut rgb, mut alpha) = (Vec::with_capacity(n), Vec::with_capacity(n));
    for y in 0..h {
        for x in 0..w {
            let (tx, ty) = (i64::from(x) - at.0, i64::from(y) - at.1);
            let px = match (u32::try_from(tx), u32::try_from(ty)) {
                (Ok(u), Ok(v)) if u < tw && v < th => tile_px(u, v),
                _ => [0.0; 4],
            };
            rgb.push([px[0], px[1], px[2]]);
            alpha.push(px[3]);
        }
    }
    let want = render_layer_reference(
        &EffectPlane::new(w, h, vec![[0.2, 0.4, 0.6]; n], vec![1.0; n]).unwrap(),
        &EffectPlane::new(w, h, rgb, alpha).unwrap(),
        LayerParams { mode: NORMAL, opacity: 0.9, fill_opacity: 0.6 },
        ops,
    )
    .unwrap();
    let ext = envelope(vec![one_layer(2, 1).with_effects(ops.to_vec())]);
    for (chunk, threads) in [(w, 1), (13, 1), (7, 2), (5, 3)] {
        let (result, grid) = run_ext(&Renderer::with_threads(chunk, threads).unwrap(), &p, &ext, Exactness::Approximate, 1 << 26, &sources);
        result.unwrap();
        for y in 0..h {
            for x in 0..w {
                let r = want.pixel(x, y).unwrap();
                let want_px = [r.rgb[0], r.rgb[1], r.rgb[2], r.a];
                assert_eq!(
                    grid.at(i64::from(x), i64::from(y)).map(f32::to_bits),
                    want_px.map(f32::to_bits),
                    "chunk {chunk} threads {threads} at ({x}, {y})"
                );
            }
        }
    }
}

#[test]
fn layer_styles_render_bit_identical_to_the_reference() {
    // Capability: pigment's class (Approximate), never a better one.
    let backend: &dyn RenderBackend = &Renderer::new(1).unwrap();
    for kind in [EffectKind::Satin, EffectKind::BevelEmboss, EffectKind::GradientOverlay] {
        assert_eq!(backend.extension_exactness(ExtOperator::Effect(kind)), Exactness::Approximate);
    }
    // Satin and both bevel halves on a 15 x 11 tile inside a 40 x 34 extent (the reference plane
    // holds the layer plus every support).
    let shape = BevelShape { size_px: 3.0, depth: 180.0, angle_deg: 135.0, altitude_deg: 35.0, ..BevelShape::DEFAULT };
    let ops = [
        EffectOp::Satin(SatinOp::new([0.1, 0.1, 0.4], StudioBlendMode::Multiply, 0.5, 19.0, 3.0, 2.0, true)),
        EffectOp::BevelEmboss(BevelEmbossOp::new(BevelPart::Shadow, [0.0; 3], StudioBlendMode::Multiply, 0.75, shape)),
        EffectOp::BevelEmboss(BevelEmbossOp::new(BevelPart::Highlight, [1.0; 3], StudioBlendMode::Screen, 0.75, shape)),
        EffectOp::BevelEmboss(BevelEmbossOp::new(
            BevelPart::Highlight,
            [1.0, 0.9, 0.6],
            NORMAL,
            0.5,
            BevelShape { style: BevelStyle::OuterBevel, ..shape },
        )),
    ];
    check(&ops, (40, 34), (15, 11), (12, 11));
    // Gradient overlay over a tile that covers the extent (its geometry follows the layer bounds,
    // which equal the reference plane there).
    let stops = [
        GradientStopOp { position: 0.0, colour: [1.0, 0.2, 0.1], opacity: 1.0 },
        GradientStopOp { position: 0.7, colour: [0.1, 0.3, 0.9], opacity: 0.5 },
        GradientStopOp { position: 1.0, colour: [0.9, 0.9, 0.2], opacity: 1.0 },
    ];
    for style in [GradientStyle::Linear, GradientStyle::Radial, GradientStyle::Angle, GradientStyle::Diamond] {
        let g = GradientOverlayOp::new(StudioBlendMode::Overlay, 0.8, style, 60.0, &stops).unwrap();
        check(&[EffectOp::GradientOverlay(g.with_offset([0.1, 0.0]))], (21, 17), (21, 17), (0, 0));
    }
}
