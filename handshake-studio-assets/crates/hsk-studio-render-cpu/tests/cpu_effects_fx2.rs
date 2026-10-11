//! Layer effects wave 2 of the CPU backend (SMB-RENDER-CPU-FX2): inner shadow, outer glow and inner
//! glow through `execute_extended`, on the same windowed evaluation as wave 1 (`effect_math::window`,
//! tests/cpu_window.rs). Comparisons are on `f32::to_bits` against pigment's `render_layer_reference`
//! (planes that hold the layer plus its margin) and against the uncut windowed oracle called
//! directly (a 300 px margin). Photoshop parity of the effects stays NOT_PROVEN (D5 cases 15-17).
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::effect_math::window::{
    AlphaPlaneRef, WindowGeometry, WindowLimits, WindowPlane, layer_window_cost,
    render_layer_window, stack_support_px,
};
use hsk_studio_pigment::effect_math::{
    ColourOverlayOp, DropShadowOp, EffectKind, EffectOp, EffectPlane, GlowFill, GlowSource,
    GlowTechnique, InnerGlowOp, InnerShadowOp, LayerParams, OuterGlowOp, render_layer_reference,
};
use hsk_studio_pigment::render_plan::*;
use hsk_studio_render_cpu::Renderer;

mod common;
use common::*;

const BLACK: [f32; 3] = [0.0; 3];
const NORMAL: StudioBlendMode = StudioBlendMode::Normal;
const MULTIPLY: StudioBlendMode = StudioBlendMode::Multiply;
const SCREEN: StudioBlendMode = StudioBlendMode::Screen;

/// Straight RGBA of pixel `(x, y)` of a `w x h` tile: a colour ramp and a non-binary alpha with
/// transparent holes (colour kept where alpha is 0).
fn tile_px(w: u32, h: u32, x: u32, y: u32) -> [f32; 4] {
    let ratio = |n: u32, d: u32| {
        f32::from(u16::try_from(n).unwrap()) / f32::from(u16::try_from(d).unwrap())
    };
    let hole = x
        .wrapping_mul(3)
        .wrapping_add(y.wrapping_mul(5))
        .is_multiple_of(7);
    let steps = x.wrapping_add(y) % 5;
    let alpha = if hole {
        0.0
    } else {
        ratio(steps.wrapping_add(1), 5)
    };
    [
        ratio(x, w),
        ratio(y, h),
        0.25 + 0.25 * ratio(x.wrapping_add(y) % 4, 1),
        alpha,
    ]
}

/// Tile pixel at plan pixel `(x, y)` of a tile placed at `(ox, oy)`; transparent black elsewhere.
fn tile_at(w: u32, h: u32, at: (i64, i64), x: u32, y: u32) -> [f32; 4] {
    let (tx, ty) = (
        i64::from(x).wrapping_sub(at.0),
        i64::from(y).wrapping_sub(at.1),
    );
    match (u32::try_from(tx), u32::try_from(ty)) {
        (Ok(u), Ok(v)) if u < w && v < h => tile_px(w, h, u, v),
        _ => [0.0; 4],
    }
}

fn effect_plan(extent: Rect, backdrop: [f32; 4], content: PlanNode, fill: f32) -> RenderPlan {
    plan(
        extent,
        vec![
            solid(extent, backdrop),
            content,
            group(vec![
                layer(0, NORMAL, 1.0),
                Layer {
                    fill_opacity: fill,
                    ..layer(1, NORMAL, 0.9)
                },
            ]),
        ],
        2,
    )
}
fn with_effects(ops: &[EffectOp]) -> PlanExtensions {
    envelope(vec![one_layer(2, 1).with_effects(ops.to_vec())])
}
fn rendered(
    chunk: u32,
    threads: usize,
    plan: &RenderPlan,
    ext: &PlanExtensions,
    min: Exactness,
    budget: u64,
    sources: &dyn PlanSources,
) -> (Result<RenderReceipt, BackendError>, Grid) {
    run_ext(
        &Renderer::with_threads(chunk, threads).unwrap(),
        plan,
        ext,
        min,
        budget,
        sources,
    )
}
fn bits(p: [f32; 4]) -> [u32; 4] {
    p.map(f32::to_bits)
}

// ---- 1. parity with the reference, every chunking and worker count ---------------------------

#[test]
fn wave2_parity_with_reference_across_chunkings_and_workers() {
    // A 17 x 13 tile with partial alpha and holes at (14, 14) of a 48 x 44 plane, layer Normal at
    // opacity 0.9 / fill 0.6 over an opaque backdrop. All three wave 2 kinds (outer glow behind
    // the content; inner shadow with offset and choke, inner glow from the edge and from the
    // centre over it) plus a drop shadow and a Dissolve overlay of wave 1, in list order. Every
    // pixel of the reference plane (it holds the layer plus the margin) equals the rendered
    // pixel bit for bit, for chunks 48, 17, 7, 5 on 1-3 workers.
    let extent = rect(0, 0, 48, 44);
    let tile = TileData::new(17, 13, |x, y| tile_px(17, 13, x, y));
    let sources = Sources {
        tiles: vec![(1, tile.planes())],
        masks: vec![],
    };
    let ops = [
        EffectOp::OuterGlow(
            OuterGlowOp::new([1.0, 0.9, 0.2], SCREEN, 0.7)
                .with_spread(0.3)
                .with_size(5.0),
        ),
        EffectOp::DropShadow(
            DropShadowOp::new(BLACK, MULTIPLY, 0.5)
                .with_offset(120.0, 3.0)
                .with_size(3.0),
        ),
        EffectOp::InnerShadow(
            InnerShadowOp::new([0.1, 0.2, 0.3], MULTIPLY, 0.6)
                .with_offset(-60.0, 2.0)
                .with_choke(0.25)
                .with_size(4.0),
        ),
        EffectOp::ColourOverlay(ColourOverlayOp::new(
            [0.2, 0.9, 0.4],
            StudioBlendMode::Dissolve,
            0.4,
        )),
        EffectOp::InnerGlow(
            InnerGlowOp::new([0.2, 0.9, 1.0], NORMAL, 0.5)
                .with_choke(0.2)
                .with_size(4.0),
        ),
        EffectOp::InnerGlow(
            InnerGlowOp::new([1.0, 0.3, 0.3], NORMAL, 0.4)
                .with_source(GlowSource::Centre)
                .with_size(3.0),
        ),
    ];
    // The reference plane (48 x 44) holds the layer (14, 14, 17, 13) grown by the margin.
    let margin = stack_support_px(&ops).unwrap();
    assert!((8..=14).contains(&margin), "margin {margin}");
    let backdrop = [0.2_f32, 0.4, 0.6, 1.0];
    let want = render_layer_reference(
        &EffectPlane::new(48, 44, vec![[0.2, 0.4, 0.6]; 2112], vec![1.0; 2112]).unwrap(),
        &{
            let (mut rgb, mut alpha) = (Vec::new(), Vec::new());
            for y in 0..44_u32 {
                for x in 0..48_u32 {
                    let p = tile_at(17, 13, (14, 14), x, y);
                    rgb.push([p[0], p[1], p[2]]);
                    alpha.push(p[3]);
                }
            }
            EffectPlane::new(48, 44, rgb, alpha).unwrap()
        },
        LayerParams {
            mode: NORMAL,
            opacity: 0.9,
            fill_opacity: 0.6,
        },
        &ops,
    )
    .unwrap();
    let content_node = PlanNode::Tile(tile.input_at(1, 14, 14));
    let p = effect_plan(extent, backdrop, content_node, 0.6);
    // The same layer without effects, to show the wave 2 kinds change pixels.
    let (result, plain) = rendered(
        48,
        1,
        &p,
        &envelope(vec![]),
        Exactness::Approximate,
        1 << 24,
        &sources,
    );
    result.unwrap();
    let ext = with_effects(&ops);
    let mut first: Option<Vec<[u32; 4]>> = None;
    for (chunk, threads) in [(48, 1), (17, 1), (7, 2), (5, 3)] {
        let (result, grid) = rendered(
            chunk,
            threads,
            &p,
            &ext,
            Exactness::Approximate,
            1 << 24,
            &sources,
        );
        let receipt = result.unwrap();
        // Pigment's class, per kind: Approximate (the lowest of the receipt too).
        for key in ["inner_shadow", "outer_glow", "inner_glow", "drop_shadow"] {
            assert!(
                receipt
                    .exactness
                    .operators
                    .iter()
                    .any(|o| o.key == key && o.exactness == Exactness::Approximate),
                "{key}"
            );
        }
        assert_eq!(receipt.exactness.lowest, Exactness::Approximate);
        let mut changed = 0_usize;
        for y in 0..44_u32 {
            for x in 0..48_u32 {
                let got = grid.at(i64::from(x), i64::from(y));
                let w = want.pixel(x, y).unwrap();
                assert_eq!(
                    bits(got),
                    bits([w.rgb[0], w.rgb[1], w.rgb[2], w.a]),
                    "chunk {chunk} threads {threads} pixel ({x}, {y})"
                );
                changed = changed.saturating_add(usize::from(
                    bits(got) != bits(plain.at(i64::from(x), i64::from(y))),
                ));
            }
        }
        assert!(
            changed > 300,
            "{changed} pixels differ from the plain layer"
        );
        let all: Vec<[u32; 4]> = grid.px.iter().copied().map(bits).collect();
        assert_eq!(first.get_or_insert_with(|| all.clone()), &all);
    }
}

// ---- 2. a margin the reference cannot hold ------------------------------------------------------

#[test]
fn wave2_large_margin_matches_the_uncut_windowed_oracle() {
    // A 20 x 16 tile at (10, 7) of a 40 x 30 plan with an inner shadow (blur 200, offset 4), an
    // outer glow (blur 200) and a centre inner glow (blur 120): margin 304 px (the 24 px limit of
    // the reference planes refused every one of them). The oracle is pigment's
    // `render_layer_window` with `WindowGeometry::uncut` over one whole-extent window and an
    // `AlphaPlaneRef`, independent of the backend's frame and chunk code.
    let extent = rect(0, 0, 40, 30);
    let layer_rect = rect(10, 7, 20, 16);
    let tile = TileData::new(20, 16, |x, y| tile_px(20, 16, x, y));
    let sources = Sources {
        tiles: vec![(1, tile.planes())],
        masks: vec![],
    };
    let ops = [
        EffectOp::InnerShadow(
            InnerShadowOp::new([0.05, 0.1, 0.2], MULTIPLY, 0.7)
                .with_offset(45.0, 4.0)
                .with_size(200.0),
        ),
        EffectOp::OuterGlow(OuterGlowOp::new([1.0, 0.8, 0.2], SCREEN, 0.6).with_size(200.0)),
        EffectOp::InnerGlow(
            InnerGlowOp::new([0.2, 0.9, 1.0], NORMAL, 0.5)
                .with_source(GlowSource::Centre)
                .with_size(120.0),
        ),
    ];
    let support = stack_support_px(&ops).unwrap();
    assert_eq!(support, 304, "4 offset + 300 blur support");
    let mut alpha = Vec::new();
    let mut content_rgb = Vec::new();
    for y in 0..30_u32 {
        for x in 0..40_u32 {
            let p = tile_at(20, 16, (10, 7), x, y);
            content_rgb.push([p[0], p[1], p[2]]);
            if (10..30).contains(&x) && (7..23).contains(&y) {
                alpha.push(p[3]);
            }
        }
    }
    let backdrop = [0.2_f32, 0.4, 0.6, 1.0];
    let want = render_layer_window(
        &AlphaPlaneRef::new(layer_rect, &alpha).unwrap(),
        &WindowGeometry::uncut(layer_rect, support, extent)
            .unwrap()
            .with_site_origin((0, 0)),
        &content_rgb,
        &WindowPlane::new(
            extent,
            vec![[backdrop[0], backdrop[1], backdrop[2]]; 1200],
            vec![backdrop[3]; 1200],
        )
        .unwrap(),
        LayerParams {
            mode: NORMAL,
            opacity: 0.9,
            fill_opacity: 0.8,
        },
        &ops,
        WindowLimits::new(64 << 20, 4_000_000_000),
    )
    .unwrap();
    let p = effect_plan(
        extent,
        backdrop,
        PlanNode::Tile(tile.input_at(1, 10, 7)),
        0.8,
    );
    let (result, plain) = rendered(
        64,
        1,
        &p,
        &envelope(vec![]),
        Exactness::Approximate,
        1 << 24,
        &sources,
    );
    result.unwrap();
    for (chunk, threads) in [(64, 1), (13, 3)] {
        let (result, grid) = rendered(
            chunk,
            threads,
            &p,
            &with_effects(&ops),
            Exactness::Approximate,
            1 << 26,
            &sources,
        );
        result.unwrap();
        let mut changed = 0_usize;
        for y in 0..30_u32 {
            for x in 0..40_u32 {
                let at = usize::try_from(y.wrapping_mul(40).wrapping_add(x)).unwrap();
                let w = [
                    want.plane.rgb()[at][0],
                    want.plane.rgb()[at][1],
                    want.plane.rgb()[at][2],
                    want.plane.alpha()[at],
                ];
                let got = grid.at(i64::from(x), i64::from(y));
                assert_eq!(
                    bits(got),
                    bits(w),
                    "chunk {chunk} threads {threads} ({x}, {y})"
                );
                changed = changed.saturating_add(usize::from(
                    bits(got) != bits(plain.at(i64::from(x), i64::from(y))),
                ));
            }
        }
        assert!(changed > 600, "{changed} pixels carry a wave 2 effect");
    }
}

// ---- 3. exactness, typed refusals, budget ------------------------------------------------------

#[test]
fn wave2_exactness_refusals_and_budget() {
    // Every kind of pigment's waves 1 and 2 reports pigment's declared class (Approximate).
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
    let one = rect(0, 0, 1, 1);
    let basic = effect_plan(one, [0.0; 4], solid(one, [1.0, 0.0, 0.0, 1.0]), 1.0);
    let refused = |ops: &[EffectOp], min: Exactness| {
        let (result, grid) = rendered(
            1,
            1,
            &basic,
            &with_effects(ops),
            min,
            1 << 20,
            &no_sources(),
        );
        assert_eq!(grid.calls, 0, "refusal happens before the first sink call");
        result
    };
    let unsupported = |operator: &'static str, exactness: Exactness| {
        Err(BackendError::Unsupported {
            operator,
            exactness,
        })
    };
    // Features pigment refuses stay typed refusals named by their key, never a flat-colour glow.
    let outer = OuterGlowOp::new(BLACK, NORMAL, 0.5).with_size(2.0);
    let inner = InnerGlowOp::new(BLACK, NORMAL, 0.5).with_size(2.0);
    for (op, key) in [
        (
            EffectOp::OuterGlow(outer.with_fill(GlowFill::Gradient)),
            "glow_gradient",
        ),
        (
            EffectOp::InnerGlow(inner.with_technique(GlowTechnique::Precise)),
            "glow_technique_precise",
        ),
        (EffectOp::OuterGlow(outer.with_range(0.5)), "glow_range"),
        (EffectOp::InnerGlow(inner.with_jitter(0.2)), "glow_jitter"),
    ] {
        assert_eq!(
            refused(&[op], Exactness::Approximate),
            unsupported(key, Exactness::Unsupported),
            "{key}"
        );
    }
    // An Exact floor refuses the Approximate class of each wave 2 kind, naming it.
    for (op, key) in [
        (
            EffectOp::InnerShadow(InnerShadowOp::new(BLACK, NORMAL, 0.5)),
            "inner_shadow",
        ),
        (EffectOp::OuterGlow(outer), "outer_glow"),
        (EffectOp::InnerGlow(inner), "inner_glow"),
    ] {
        assert_eq!(
            refused(&[op], Exactness::Exact),
            unsupported(key, Exactness::Approximate),
            "{key}"
        );
    }
    // Charge, hand-derived (one worker, 1 x 1 chunk, a 1 x 1 layer): v1 scratch 32; three effect
    // records 3 * 64; the content frame 1 pixel * 16; the window arrays 1 pixel * 28; pigment's
    // predicted peak for a 1 x 1 window (`layer_window_cost`). One byte less is BudgetExceeded
    // before any sink call.
    let ops = [
        EffectOp::InnerShadow(InnerShadowOp::new(BLACK, NORMAL, 0.5).with_size(4.0)),
        EffectOp::OuterGlow(outer),
        EffectOp::InnerGlow(inner),
    ];
    let peak = layer_window_cost(&ops, rect(0, 0, 1, 1))
        .unwrap()
        .peak_bytes;
    let need = 268_u64.wrapping_add(peak);
    let (result, grid) = rendered(
        1,
        1,
        &basic,
        &with_effects(&ops),
        Exactness::Approximate,
        need,
        &no_sources(),
    );
    assert_eq!(result.unwrap().peak_working_bytes, need);
    assert_eq!(grid.calls, 1);
    let below = need.wrapping_sub(1);
    let (result, grid) = rendered(
        1,
        1,
        &basic,
        &with_effects(&ops),
        Exactness::Approximate,
        below,
        &no_sources(),
    );
    assert_eq!(
        result,
        Err(BackendError::BudgetExceeded {
            requested: need,
            limit: below
        })
    );
    assert_eq!(grid.calls, 0);
}
