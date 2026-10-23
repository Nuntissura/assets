//! Layer effects wave 1 of the CPU backend (drop shadow, colour overlay, solid stroke) through
//! `RenderBackend::execute_extended`. The matte and the composition order are pigment's
//! `effect_math`; the backend evaluates them per chunk window (`effect_math::window`, see
//! tests/cpu_window.rs), so each test compares the rendered plan with `render_layer_reference` on
//! the whole plane (bit-identical) and states hand-derived values. Photoshop parity of the effects
//! is NOT_PROVEN (D5 cases 12-14).
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::coverage_math::{BlendChannel, BlendRangeEntry, IDENTITY_RANGE};
use hsk_studio_pigment::effect_math::window::layer_window_cost;
use hsk_studio_pigment::effect_math::{
    ColourOverlayOp, DropShadowOp, EffectKind, EffectOp, EffectPlane, LayerParams, StrokeOp,
    StrokePosition, render_layer_reference,
};
use hsk_studio_pigment::render_plan::*;
use hsk_studio_render_cpu::Renderer;

mod common;
use common::*;

const BLACK: [f32; 3] = [0.0; 3];
const RED: [f32; 3] = [1.0, 0.0, 0.0];
const BLUE: [f32; 3] = [0.0, 0.0, 1.0];
const NORMAL: StudioBlendMode = StudioBlendMode::Normal;

fn plane_of(w: u32, h: u32, f: impl Fn(u32, u32) -> ([f32; 3], f32)) -> EffectPlane {
    let (mut rgb, mut alpha) = (Vec::new(), Vec::new());
    for y in 0..h {
        for x in 0..w {
            let (c, a) = f(x, y);
            rgb.push(c);
            alpha.push(a);
        }
    }
    EffectPlane::new(w, h, rgb, alpha).unwrap()
}
/// A `w x h` plane that is `colour` (opaque) inside `[x0, x0 + rw) x [y0, y0 + rh)` and clear outside.
fn block(w: u32, h: u32, at: (u32, u32), size: (u32, u32), colour: [f32; 3]) -> EffectPlane {
    plane_of(w, h, |x, y| {
        let inside = x >= at.0
            && y >= at.1
            && x < at.0.wrapping_add(size.0)
            && y < at.1.wrapping_add(size.1);
        if inside { (colour, 1.0) } else { (BLACK, 0.0) }
    })
}
fn clear(w: u32, h: u32) -> EffectPlane {
    EffectPlane::transparent(w, h).unwrap()
}
fn opaque(w: u32, h: u32, colour: [f32; 3]) -> EffectPlane {
    plane_of(w, h, |_, _| (colour, 1.0))
}

/// Isolated group: layer 0 is `backdrop` over the extent, layer 1 the `content` node with the given
/// mode, opacity and fill. Effects ride on `(group 2, layer 1)`.
fn effect_plan(
    extent: Rect,
    backdrop: [f32; 4],
    content: PlanNode,
    mode: StudioBlendMode,
    opacity: f32,
    fill: f32,
) -> RenderPlan {
    plan(
        extent,
        vec![
            solid(extent, backdrop),
            content,
            group(vec![
                layer(0, NORMAL, 1.0),
                Layer {
                    fill_opacity: fill,
                    ..layer(1, mode, opacity)
                },
            ]),
        ],
        2,
    )
}
fn with_effects(ops: &[EffectOp]) -> PlanExtensions {
    envelope(vec![one_layer(2, 1).with_effects(ops.to_vec())])
}
fn render(
    chunk: u32,
    threads: usize,
    plan: &RenderPlan,
    ext: &PlanExtensions,
) -> (Result<RenderReceipt, BackendError>, Grid) {
    run_ext(
        &Renderer::with_threads(chunk, threads).unwrap(),
        plan,
        ext,
        Exactness::Approximate,
        1 << 20,
        &no_sources(),
    )
}
/// Every pixel of `grid` equals pigment's `render_layer_reference` of the same layer, bit for bit.
fn assert_reference(
    grid: &Grid,
    backdrop: &EffectPlane,
    content: &EffectPlane,
    params: LayerParams,
    ops: &[EffectOp],
) -> usize {
    let want = render_layer_reference(backdrop, content, params, ops).unwrap();
    let mut compared = 0_usize;
    for y in 0..content.height() {
        for x in 0..content.width() {
            let p = want.pixel(x, y).unwrap();
            assert_eq!(
                grid.at(i64::from(x), i64::from(y)).map(f32::to_bits),
                [p.rgb[0], p.rgb[1], p.rgb[2], p.a].map(f32::to_bits),
                "pixel ({x}, {y})"
            );
            compared = compared.saturating_add(1);
        }
    }
    compared
}
fn params(opacity: f32, fill: f32) -> LayerParams {
    LayerParams {
        mode: NORMAL,
        opacity,
        fill_opacity: fill,
    }
}

// ---- 1. drop shadow --------------------------------------------------------------------------

#[test]
fn drop_shadow_hand_values_and_reference_parity() {
    // One opaque red pixel, shadow black Normal 0.5, distance 0, spread 0, size 0: the matte is the
    // alpha. At fill 1 the shadow sits under the opaque content (red, alpha 1); at fill 0 only the
    // shadow shows at the effect opacity (0, 0, 0, 0.5); the layer opacity fades it (0.5 * 0.5);
    // knock-out removes it where the layer is.
    let one = rect(0, 0, 1, 1);
    let shadow = |knock: bool| {
        EffectOp::DropShadow(DropShadowOp::new(BLACK, NORMAL, 0.5).with_layer_knocks_out(knock))
    };
    let single = |opacity: f32, fill: f32, knock: bool| {
        pixel(
            &effect_plan(
                one,
                [0.0; 4],
                solid(one, [1.0, 0.0, 0.0, 1.0]),
                NORMAL,
                opacity,
                fill,
            ),
            &with_effects(&[shadow(knock)]),
            &no_sources(),
        )
    };
    near(single(1.0, 1.0, false), [1.0, 0.0, 0.0, 1.0], 1e-6);
    near(single(1.0, 0.0, false), [0.0, 0.0, 0.0, 0.5], 1e-6);
    near(single(0.5, 0.0, false), [0.0, 0.0, 0.0, 0.25], 1e-6);
    near(single(1.0, 0.0, true), [0.0, 0.0, 0.0, 0.0], 1e-6);

    // Offset: angle 180, distance 2 moves the matte by (+2, 0) (pigment's convention (-cos a, sin a)
    // * d, D5 case 12): a pixel at x = 2 in an 8 x 1 row, fill 0, shadow opacity 0.75, is visible
    // at x = 4 only.
    let row = rect(0, 0, 8, 1);
    let moved = effect_plan(
        row,
        [0.0; 4],
        solid(rect(2, 0, 1, 1), [1.0, 0.0, 0.0, 1.0]),
        NORMAL,
        1.0,
        0.0,
    );
    let ops = [EffectOp::DropShadow(
        DropShadowOp::new(BLACK, NORMAL, 0.75).with_offset(180.0, 2.0),
    )];
    let (result, grid) = render(3, 1, &moved, &with_effects(&ops));
    result.unwrap();
    for x in 0..8 {
        let want = if x == 4 { 0.75 } else { 0.0 };
        near(grid.at(x, 0), [0.0, 0.0, 0.0, want], 1e-6);
    }

    // Blur: a one-pixel-wide full-height bar (x = 7 of 15 x 15), fill 0, shadow opacity 1, size 2:
    // Photoshop's tent kernel (cross-lane re-pin by SMB-PS-ORACLE-PARITY-1: the size-2 shadow of
    // the D5 oracle PS-C12 square_s2_sp0_* is exact with it), support 1, taps w0 = 0.5, w1 = 0.25,
    // w2 = w3 = 0. Away from the bar ends the vertical pass sums to 1, so row 7 is
    // [w3, w2, w1, w0, w1, w2, w3] for x = 4 .. 10. Chunks of 8 make tiles of 8 with a margin of
    // 1, so the rows and columns cross tile borders; the whole plane equals the reference.
    let (w0, w1, w2, w3) = (0.5_f32, 0.25_f32, 0.0_f32, 0.0_f32);
    let extent = rect(0, 0, 15, 15);
    let bar = effect_plan(
        extent,
        [0.0; 4],
        solid(rect(7, 0, 1, 15), [1.0, 0.0, 0.0, 1.0]),
        NORMAL,
        1.0,
        0.0,
    );
    let ops = [EffectOp::DropShadow(
        DropShadowOp::new(BLACK, NORMAL, 1.0).with_size(2.0),
    )];
    let (result, grid) = render(8, 1, &bar, &with_effects(&ops));
    let receipt = result.unwrap();
    for (i, want) in [w3, w2, w1, w0, w1, w2, w3].into_iter().enumerate() {
        let x = i64::try_from(i).unwrap().wrapping_add(4);
        near(grid.at(x, 7), [0.0, 0.0, 0.0, want], 2e-6);
    }
    near(grid.at(1, 7), [0.0, 0.0, 0.0, 0.0], 0.0);
    let compared = assert_reference(
        &grid,
        &clear(15, 15),
        &block(15, 15, (7, 0), (1, 15), RED),
        params(1.0, 0.0),
        &ops,
    );
    assert_eq!(compared, 225);
    let class_of = |key: &str| {
        receipt
            .exactness
            .operators
            .iter()
            .find(|o| o.key == key)
            .map(|o| o.exactness)
    };
    assert_eq!(class_of("drop_shadow"), Some(Exactness::Approximate));
    assert_eq!(receipt.exactness.lowest, Exactness::Approximate);
    // Spread + size + offset + knock-out on a block, other chunking: still the reference.
    let blocky = effect_plan(
        extent,
        [0.0; 4],
        solid(rect(4, 4, 6, 5), [1.0, 0.0, 0.0, 1.0]),
        NORMAL,
        1.0,
        1.0,
    );
    let ops = [EffectOp::DropShadow(
        DropShadowOp::new([0.1, 0.2, 0.3], NORMAL, 0.8)
            .with_offset(-45.0, 2.0)
            .with_spread(0.5)
            .with_size(4.0)
            .with_layer_knocks_out(true),
    )];
    let (result, grid) = render(5, 2, &blocky, &with_effects(&ops));
    result.unwrap();
    let compared = assert_reference(
        &grid,
        &clear(15, 15),
        &block(15, 15, (4, 4), (6, 5), RED),
        params(1.0, 1.0),
        &ops,
    );
    assert_eq!(compared, 225);
}

// ---- 2. colour overlay -----------------------------------------------------------------------

#[test]
fn colour_overlay_hand_values_clip_and_full_strength() {
    // A 2 x 1 tile: pixel 0 opaque blue, pixel 1 fully transparent (blue, alpha 0). Red Normal 50%
    // overlay: over the blue pixel (0.5, 0, 0.5, 1); over the transparent pixel nothing (the matte
    // is the layer alpha, 0). At fill 0 the content vanishes but the overlay keeps its full
    // strength: (1, 0, 0, 0.5) on pixel 0 (STU-RAS-115 "regardless of fill").
    let wide = rect(0, 0, 2, 1);
    let tile = TileData::new(2, 1, |x, _| [0.0, 0.0, 1.0, if x == 0 { 1.0 } else { 0.0 }]);
    let sources = Sources {
        tiles: vec![(1, tile.planes())],
        masks: vec![],
    };
    let overlay = [EffectOp::ColourOverlay(ColourOverlayOp::new(
        RED, NORMAL, 0.5,
    ))];
    let render_with = |fill: f32| {
        let p = effect_plan(
            wide,
            [0.0; 4],
            PlanNode::Tile(tile.input(1)),
            NORMAL,
            1.0,
            fill,
        );
        let (result, grid) = run_ext(
            &Renderer::new(2).unwrap(),
            &p,
            &with_effects(&overlay),
            Exactness::Approximate,
            1 << 20,
            &sources,
        );
        result.unwrap();
        grid
    };
    let full = render_with(1.0);
    near(full.at(0, 0), [0.5, 0.0, 0.5, 1.0], 1e-6);
    near(full.at(1, 0), [0.0, 0.0, 0.0, 0.0], 0.0);
    let none = render_with(0.0);
    near(none.at(0, 0), [1.0, 0.0, 0.0, 0.5], 1e-6);
    near(none.at(1, 0), [0.0, 0.0, 0.0, 0.0], 0.0);
    // Reference parity at both fills, on the same two-pixel plane.
    let content = plane_of(2, 1, |x, _| (BLUE, if x == 0 { 1.0 } else { 0.0 }));
    for (grid, fill) in [(&full, 1.0), (&none, 0.0)] {
        let compared = assert_reference(grid, &clear(2, 1), &content, params(1.0, fill), &overlay);
        assert_eq!(compared, 2);
    }
}

// ---- 3. stroke -------------------------------------------------------------------------------

#[test]
fn stroke_rings_hand_values_and_reference_parity() {
    // A 3 x 3 opaque red square at (2, 2) of a 7 x 7 plane, black Normal opaque stroke of size 1.
    // Pigment's grow `clamp(r + 1 - d)` with the 5 x 5 chamfer d (lane SMB-PS-ORACLE-PARITY-2, D5
    // PS-C13): an orthogonal neighbour of the square has d = w1 -> 1.0, a diagonal neighbour
    // d = w2 -> 2 - 1.41366 = 0.58634, two pixels away along an axis the fringe 2 - 2 w1 = 0.00076,
    // a knight step away 0. Outside: that ring behind the content over transparent; inside: all
    // eight border pixels are 1.0 and the centre keeps the fringe (red times 1 - 0.00076); the
    // square is untouched outside the ring.
    let extent = rect(0, 0, 7, 7);
    let square = || solid(rect(2, 2, 3, 3), [1.0, 0.0, 0.0, 1.0]);
    let content = block(7, 7, (2, 2), (3, 3), RED);
    let stroke = |size: f32, position: StrokePosition| {
        [EffectOp::Stroke(StrokeOp::new(
            BLACK, NORMAL, 1.0, size, position,
        ))]
    };
    let run = |ops: &[EffectOp]| {
        let p = effect_plan(extent, [0.0; 4], square(), NORMAL, 1.0, 1.0);
        let (result, grid) = render(4, 1, &p, &with_effects(ops));
        result.unwrap();
        grid
    };
    let outside = run(&stroke(1.0, StrokePosition::Outside));
    near(outside.at(1, 3), [0.0, 0.0, 0.0, 1.0], 1e-6);
    near(outside.at(3, 5), [0.0, 0.0, 0.0, 1.0], 1e-6);
    near(outside.at(1, 1), [0.0, 0.0, 0.0, 0.586_343], 1e-6);
    near(outside.at(5, 5), [0.0, 0.0, 0.0, 0.586_343], 1e-6);
    near(outside.at(0, 3), [0.0, 0.0, 0.0, 0.000_759], 1e-6);
    near(outside.at(1, 0), [0.0, 0.0, 0.0, 0.0], 0.0);
    near(outside.at(3, 3), [1.0, 0.0, 0.0, 1.0], 1e-6);
    near(outside.at(2, 2), [1.0, 0.0, 0.0, 1.0], 1e-6);
    let inside = run(&stroke(1.0, StrokePosition::Inside));
    for (x, y) in [
        (2, 2),
        (3, 2),
        (4, 2),
        (2, 3),
        (4, 3),
        (2, 4),
        (3, 4),
        (4, 4),
    ] {
        near(inside.at(x, y), [0.0, 0.0, 0.0, 1.0], 1e-6);
    }
    near(inside.at(3, 3), [0.999_241, 0.0, 0.0, 1.0], 1e-6);
    near(inside.at(1, 3), [0.0, 0.0, 0.0, 0.0], 0.0);
    let mut compared = 0_usize;
    for (grid, size, position) in [
        (&outside, 1.0, StrokePosition::Outside),
        (&inside, 1.0, StrokePosition::Inside),
    ] {
        compared = compared.saturating_add(assert_reference(
            grid,
            &clear(7, 7),
            &content,
            params(1.0, 1.0),
            &stroke(size, position),
        ));
    }
    // Centre of size 2 covers both bands; a wider stroke and a 2-chunk split still match.
    for (size, position) in [
        (2.0, StrokePosition::Centre),
        (3.0, StrokePosition::Outside),
    ] {
        let grid = run(&stroke(size, position));
        compared = compared.saturating_add(assert_reference(
            &grid,
            &clear(7, 7),
            &content,
            params(1.0, 1.0),
            &stroke(size, position),
        ));
    }
    assert_eq!(compared, 196);
}

// ---- 4. composition, chunking and fades ------------------------------------------------------

#[test]
fn effect_stack_matches_reference_for_every_chunking() {
    // Layer Normal at opacity 0.8 and fill 0.5 over an opaque backdrop with a shadow (Multiply,
    // behind the content), a colour overlay and two strokes (over the content, list order).
    // Pigment's rule (effect_math module doc): effects fade with the layer opacity and not with
    // the fill; the reference defines the pixels. Chunks 40 (one tile), 13 and 9 with one and
    // three workers must all equal it, bit for bit, and each other. The reference plane holds the
    // layer plus its margin (9 px; the content has 11 px around it): pigment's reference reads
    // zero beyond its plane, so a shadow that shifts in from outside the plane would be cut.
    let extent = rect(0, 0, 40, 36);
    let backdrop = [0.2, 0.4, 0.6, 1.0];
    let colour = [0.9, 0.3, 0.1];
    let ops = [
        EffectOp::DropShadow(
            DropShadowOp::new(BLACK, StudioBlendMode::Multiply, 0.6)
                .with_offset(120.0, 3.0)
                .with_spread(0.25)
                .with_size(4.0),
        ),
        EffectOp::ColourOverlay(ColourOverlayOp::new([0.2, 0.9, 0.4], NORMAL, 0.3)),
        EffectOp::Stroke(StrokeOp::new(
            [1.0, 1.0, 0.0],
            NORMAL,
            0.8,
            2.0,
            StrokePosition::Outside,
        )),
        EffectOp::Stroke(StrokeOp::new(
            BLUE,
            NORMAL,
            0.5,
            1.0,
            StrokePosition::Inside,
        )),
    ];
    let p = effect_plan(
        extent,
        backdrop,
        solid(rect(11, 11, 9, 8), [colour[0], colour[1], colour[2], 1.0]),
        NORMAL,
        0.8,
        0.5,
    );
    let ext = with_effects(&ops);
    let want_backdrop = opaque(40, 36, [backdrop[0], backdrop[1], backdrop[2]]);
    let want_content = block(40, 36, (11, 11), (9, 8), colour);
    let mut first: Option<Vec<[u32; 4]>> = None;
    let mut compared = 0_usize;
    for (chunk, threads) in [(40, 1), (13, 1), (9, 3)] {
        let (result, grid) = render(chunk, threads, &p, &ext);
        result.unwrap();
        compared = compared.saturating_add(assert_reference(
            &grid,
            &want_backdrop,
            &want_content,
            params(0.8, 0.5),
            &ops,
        ));
        let bits: Vec<[u32; 4]> = grid.px.iter().map(|p| p.map(f32::to_bits)).collect();
        assert_eq!(first.get_or_insert_with(|| bits.clone()), &bits);
    }
    assert_eq!(compared, 4320);
}

// ---- 5. refusals and budget ------------------------------------------------------------------

#[test]
fn effect_refusals_and_scratch_charge_stay_typed() {
    let one = rect(0, 0, 1, 1);
    // Capability. Changed with SMB-RENDER-CPU-FX2: the wave 2 kinds (inner shadow, outer glow,
    // inner glow) are implemented too, so every kind of EffectKind::ALL reports pigment's declared
    // class (Approximate, D5 12-17), not a better one: pigment owns the exactness (CX-CAP-001).
    // (Before FX2 this loop asserted Unsupported for the three wave 2 kinds.) A kind pigment adds
    // later is refused at admission until this backend evaluates and tests it
    // (tests/cpu_effects_fx2.rs checks all six).
    let backend: &dyn RenderBackend = &Renderer::new(1).unwrap();
    for kind in EffectKind::ALL {
        assert_eq!(
            backend.extension_exactness(ExtOperator::Effect(kind)),
            ExtOperator::Effect(kind).intrinsic(),
            "{kind:?}"
        );
        assert_eq!(
            backend.extension_exactness(ExtOperator::Effect(kind)),
            Exactness::Approximate
        );
    }
    let refused = |p: &RenderPlan, ext: &PlanExtensions, min: Exactness| {
        let (result, grid) = run_ext(
            &Renderer::new(1).unwrap(),
            p,
            ext,
            min,
            1 << 20,
            &no_sources(),
        );
        assert_eq!(grid.calls, 0, "refusal happens before the first sink call");
        result
    };
    let shadow = |size: f32| {
        [EffectOp::DropShadow(
            DropShadowOp::new(BLACK, NORMAL, 0.5).with_size(size),
        )]
    };
    let basic = effect_plan(
        one,
        [0.0; 4],
        solid(one, [1.0, 0.0, 0.0, 1.0]),
        NORMAL,
        1.0,
        1.0,
    );
    let unsupported = |operator: &'static str| {
        Err(BackendError::Unsupported {
            operator,
            exactness: Exactness::Unsupported,
        })
    };
    // An Exact floor refuses the Approximate effect class, naming the effect.
    assert_eq!(
        refused(&basic, &with_effects(&shadow(0.0)), Exactness::Exact),
        Err(BackendError::Unsupported {
            operator: "drop_shadow",
            exactness: Exactness::Approximate
        })
    );
    // Changed with SMB-RENDER-CPU-WINDOW: a margin above the 24 px limit of the 64-pixel reference
    // planes (size 30: sigma 15, support 45) used to be the typed refusal `effect_margin`; the
    // windowed evaluation renders it. The shadow of one opaque pixel is the product of two centre
    // taps of the blur kernel (tests/cpu_window.rs checks the 200 px case against the oracle).
    let (result, grid) = render(1, 1, &basic, &with_effects(&shadow(30.0)));
    result.unwrap();
    assert_eq!(grid.calls, 1);
    near(grid.at(0, 0), [1.0, 0.0, 0.0, 1.0], 1e-6);
    // Effects and Blend-If on one layer, and effects in the content of a layer with effects.
    let both = envelope(vec![
        one_layer(2, 1)
            .with_effects(shadow(0.0).to_vec())
            .with_blend_if(vec![BlendRangeEntry {
                channel: BlendChannel::Gray,
                source: [0.0, 0.1, 0.9, 1.0],
                destination: IDENTITY_RANGE,
            }]),
    ]);
    assert_eq!(
        refused(&basic, &both, Exactness::Approximate),
        unsupported("effects_with_blend_if")
    );
    let nested = plan(
        one,
        vec![
            solid(one, [1.0, 0.0, 0.0, 1.0]),
            group(vec![layer(0, NORMAL, 1.0)]),
            group(vec![layer(1, NORMAL, 1.0)]),
        ],
        2,
    );
    let both_levels = envelope(vec![
        one_layer(1, 0).with_effects(shadow(0.0).to_vec()),
        one_layer(2, 0).with_effects(shadow(0.0).to_vec()),
    ]);
    assert_eq!(
        refused(&nested, &both_levels, Exactness::Approximate),
        unsupported("effects_nested")
    );
    // A single level is fine (the control for the nested refusal).
    let inner_only = envelope(vec![one_layer(1, 0).with_effects(shadow(0.0).to_vec())]);
    let (result, grid) = render(1, 1, &nested, &inner_only);
    result.unwrap();
    near(grid.at(0, 0), [1.0, 0.0, 0.0, 1.0], 1e-6);

    // Charge (hand-derived, one worker, 1 x 1 chunk). Changed with SMB-RENDER-CPU-WINDOW: the old
    // model charged a (1 + 2 * margin)^2 reference frame (19700 bytes for this case); the windowed
    // evaluation charges the content frame clipped to the 1 x 1 layer (1 pixel * 16 bytes), the two
    // window input arrays (1 pixel * 28 bytes) and pigment's predicted peak for a 1 x 1 window
    // (`layer_window_cost`; the shadow has margin feather_support(4) = 6). Total: v1 scratch 32 +
    // one effect record 64 + 16 + 28 + peak. One byte less is BudgetExceeded before any sink call.
    let ext = with_effects(&shadow(4.0));
    let peak = layer_window_cost(&shadow(4.0), rect(0, 0, 1, 1))
        .unwrap()
        .peak_bytes;
    let need = 140_u64.wrapping_add(peak);
    let (result, _) = run_ext(
        &Renderer::new(1).unwrap(),
        &basic,
        &ext,
        Exactness::Approximate,
        1 << 20,
        &no_sources(),
    );
    assert_eq!(result.unwrap().peak_working_bytes, need);
    let (result, grid) = run_ext(
        &Renderer::new(1).unwrap(),
        &basic,
        &ext,
        Exactness::Approximate,
        need.wrapping_sub(1),
        &no_sources(),
    );
    assert_eq!(
        result,
        Err(BackendError::BudgetExceeded {
            requested: need,
            limit: need.wrapping_sub(1)
        })
    );
    assert_eq!(grid.calls, 0);
}
