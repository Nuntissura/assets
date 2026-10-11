//! Windowed layer effects of the CPU backend (SMB-RENDER-CPU-WINDOW): `execute_extended` evaluates
//! an effect layer per chunk window with pigment's `effect_math::window` API instead of per 64 x 64
//! reference tile. Parity with pigment's `render_layer_reference` is compared on `f32::to_bits`; a
//! margin the reference planes cannot hold (a 200 px blur) is compared with the uncut windowed
//! oracle, which these tests call directly (one window over the whole extent, an `AlphaPlaneRef`
//! over a hand-built alpha plane), independent of the backend's chunking and frame code. Photoshop
//! parity of the effects stays NOT_PROVEN (D5 cases 12-14).
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::coverage_math::feather_kernel;
use hsk_studio_pigment::effect_math::window::{
    AlphaPlaneRef, WindowGeometry, WindowLimits, WindowPlane, layer_window_cost,
    render_layer_window, stack_support_px,
};
use hsk_studio_pigment::effect_math::{
    ColourOverlayOp, DropShadowOp, EffectOp, EffectPlane, LayerParams, StrokeOp, StrokePosition,
    render_layer_reference,
};
use hsk_studio_pigment::render_plan::*;
use hsk_studio_render_cpu::{MAX_EFFECT_WINDOW_WORK, Renderer};

mod common;
use common::*;

const BLACK: [f32; 3] = [0.0; 3];
const RED: [f32; 3] = [1.0, 0.0, 0.0];
const BLUE: [f32; 3] = [0.0, 0.0, 1.0];
const NORMAL: StudioBlendMode = StudioBlendMode::Normal;
const MULTIPLY: StudioBlendMode = StudioBlendMode::Multiply;

/// Straight RGBA of tile pixel `(x, y)` of a `w x h` tile: a colour ramp and a non-binary alpha
/// with transparent holes (colour kept where alpha is 0).
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

/// One effect layer over a backdrop: group `[backdrop solid, content]`; effects ride on `(g, 1)`.
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
fn rendered(
    chunk: u32,
    threads: usize,
    plan: &RenderPlan,
    ext: &PlanExtensions,
    budget: u64,
    sources: &dyn PlanSources,
) -> (Result<RenderReceipt, BackendError>, Grid) {
    run_ext(
        &Renderer::with_threads(chunk, threads).unwrap(),
        plan,
        ext,
        Exactness::Approximate,
        budget,
        sources,
    )
}
fn bits(p: [f32; 4]) -> [u32; 4] {
    p.map(f32::to_bits)
}

// ---- 1. parity with the reference, every chunking and thread count ---------------------------

#[test]
fn parity_with_reference_across_chunkings_and_threads() {
    // A 17 x 13 tile with partial alpha and holes at (14, 14) of a 48 x 44 plane (the reference
    // plane holds the layer plus its 9 px margin): layer Multiply at opacity 0.8 / fill 0.5 with a
    // Multiply drop shadow (offset, spread, blur), a Dissolve colour overlay and two strokes. Every
    // pixel of the reference equals the rendered pixel bit for bit for chunks 48, 17, 7, 5, 11 on
    // 1-4 workers (dissolve included: it reads plan coordinates, not chunk coordinates).
    let extent = rect(0, 0, 48, 44);
    let tile = TileData::new(17, 13, |x, y| tile_px(17, 13, x, y));
    let sources = Sources {
        tiles: vec![(1, tile.planes())],
        masks: vec![],
    };
    let ops = [
        EffectOp::DropShadow(
            DropShadowOp::new([0.1, 0.2, 0.3], MULTIPLY, 0.6)
                .with_offset(120.0, 3.0)
                .with_spread(0.25)
                .with_size(4.0),
        ),
        EffectOp::ColourOverlay(ColourOverlayOp::new(
            [0.2, 0.9, 0.4],
            StudioBlendMode::Dissolve,
            0.5,
        )),
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
    // Margin 9 = shadow offset 3 + spread 1 + blur support 5; the layer bounds (14, 14, 17, 13)
    // grown by it are the region the effect layer rewrites: x 5..40, y 5..36.
    assert_eq!(stack_support_px(&ops).unwrap(), 9);
    let content = plane_of(48, 44, |x, y| {
        let (lx, ly) = (i64::from(x).wrapping_sub(14), i64::from(y).wrapping_sub(14));
        match (u32::try_from(lx), u32::try_from(ly)) {
            (Ok(tx), Ok(ty)) if tx < 17 && ty < 13 => {
                let p = tile_px(17, 13, tx, ty);
                ([p[0], p[1], p[2]], p[3])
            }
            _ => (BLACK, 0.0),
        }
    });
    let params = LayerParams {
        mode: MULTIPLY,
        opacity: 0.8,
        fill_opacity: 0.5,
    };
    let ext = with_effects(&ops);
    let content_node = PlanNode::Tile(tile.input_at(1, 14, 14));
    let mut compared = 0_usize;
    for backdrop in [[0.2_f32, 0.4, 0.6, 1.0], [0.2, 0.4, 0.6, 0.7]] {
        // The running buffer under the effect layer: the backdrop layer alone, as rendered.
        let under = plan(
            extent,
            vec![solid(extent, backdrop), group(vec![layer(0, NORMAL, 1.0)])],
            1,
        );
        let (result, below) = rendered(48, 1, &under, &envelope(vec![]), 1 << 24, &no_sources());
        result.unwrap();
        let want_backdrop = plane_of(48, 44, |x, y| {
            let p = below.at(i64::from(x), i64::from(y));
            ([p[0], p[1], p[2]], p[3])
        });
        let want = render_layer_reference(&want_backdrop, &content, params, &ops).unwrap();
        let opaque = backdrop[3] == 1.0;
        let p = effect_plan(extent, backdrop, content_node.clone(), MULTIPLY, 0.8, 0.5);
        let mut first: Option<Vec<[u32; 4]>> = None;
        for (chunk, threads) in [(48, 1), (17, 1), (7, 2), (5, 3), (11, 4)] {
            let (result, grid) = rendered(chunk, threads, &p, &ext, 1 << 24, &sources);
            let receipt = result.unwrap();
            assert_eq!(receipt.exactness.lowest, Exactness::Approximate);
            for y in 0..44_u32 {
                for x in 0..48_u32 {
                    let got = grid.at(i64::from(x), i64::from(y));
                    let inside = (5..40).contains(&x) && (5..36).contains(&y);
                    if inside || opaque {
                        let w = want.pixel(x, y).unwrap();
                        assert_eq!(
                            bits(got),
                            bits([w.rgb[0], w.rgb[1], w.rgb[2], w.a]),
                            "chunk {chunk} threads {threads} pixel ({x}, {y}) backdrop {backdrop:?}"
                        );
                        compared = compared.saturating_add(1);
                    } else {
                        // Outside the region the effect layer does not touch the running buffer.
                        assert_eq!(bits(got), bits(below.at(i64::from(x), i64::from(y))));
                    }
                }
            }
            let all: Vec<[u32; 4]> = grid.px.iter().copied().map(bits).collect();
            assert_eq!(first.get_or_insert_with(|| all.clone()), &all);
        }
    }
    // 5 chunkings x (2112 opaque + 1085 inside the region of the translucent run).
    assert_eq!(compared, 5 * (2112 + 35 * 31));
}

// ---- 2. a margin the reference cannot hold ------------------------------------------------------

/// Centre tap of the blur kernel of `size` px (separable: the shadow of one opaque pixel is
/// `w0 * w0` at the pixel).
fn centre_tap(size: f32) -> f32 {
    let taps = feather_kernel(size).unwrap();
    taps[taps.len() / 2]
}

#[test]
fn large_margin_renders_and_matches_the_uncut_windowed_oracle() {
    // Hand value: one opaque pixel, fill 0, black shadow opacity 1, blur size 200 (sigma 100,
    // support 300 px: the former 24 px margin limit refused it). The shadow at the pixel is
    // the product of two centre taps w0 * w0.
    let one = rect(0, 0, 1, 1);
    let blur = |size: f32| {
        [EffectOp::DropShadow(
            DropShadowOp::new(BLACK, NORMAL, 1.0).with_size(size),
        )]
    };
    let tiny = effect_plan(
        one,
        [0.0; 4],
        solid(one, [1.0, 0.0, 0.0, 1.0]),
        NORMAL,
        1.0,
        0.0,
    );
    for size in [200.0_f32, 1000.0] {
        let ops = blur(size);
        assert!(stack_support_px(&ops).unwrap() > 24, "size {size}");
        let (result, grid) = rendered(1, 1, &tiny, &with_effects(&ops), 1 << 28, &no_sources());
        result.unwrap();
        let w0 = f64::from(centre_tap(size));
        near(grid.at(0, 0), [0.0, 0.0, 0.0, (w0 * w0) as f32], 1e-9);
    }

    // Oracle: a 20 x 16 tile at (10, 7) of a 40 x 30 plan, shadow (Multiply, offset, blur 200,
    // margin 305) + outside stroke + overlay, layer opacity 0.9 / fill 0.8. One window over the
    // whole extent with `WindowGeometry::uncut`, the tile alpha as an `AlphaPlaneRef`.
    let extent = rect(0, 0, 40, 30);
    let layer_rect = rect(10, 7, 20, 16);
    let tile = TileData::new(20, 16, |x, y| tile_px(20, 16, x, y));
    let sources = Sources {
        tiles: vec![(1, tile.planes())],
        masks: vec![],
    };
    let ops = [
        EffectOp::DropShadow(
            DropShadowOp::new([0.05, 0.1, 0.2], MULTIPLY, 0.7)
                .with_offset(45.0, 5.0)
                .with_size(200.0),
        ),
        EffectOp::ColourOverlay(ColourOverlayOp::new(RED, NORMAL, 0.25)),
        EffectOp::Stroke(StrokeOp::new(
            BLUE,
            NORMAL,
            0.8,
            3.0,
            StrokePosition::Outside,
        )),
    ];
    let support = stack_support_px(&ops).unwrap();
    assert_eq!(support, 305, "5 offset + 300 blur support");
    let mut alpha = Vec::new();
    let mut content_rgb = Vec::new();
    for y in 0..30_u32 {
        for x in 0..40_u32 {
            let (tx, ty) = (i64::from(x).wrapping_sub(10), i64::from(y).wrapping_sub(7));
            let p = match (u32::try_from(tx), u32::try_from(ty)) {
                (Ok(u), Ok(v)) if u < 20 && v < 16 => tile_px(20, 16, u, v),
                _ => [0.0; 4],
            };
            content_rgb.push([p[0], p[1], p[2]]);
            if (10..30).contains(&x) && (7..23).contains(&y) {
                alpha.push(p[3]);
            }
        }
    }
    let backdrop = [0.2_f32, 0.4, 0.6, 1.0];
    let backdrop_plane = WindowPlane::new(
        extent,
        vec![[backdrop[0], backdrop[1], backdrop[2]]; 1200],
        vec![backdrop[3]; 1200],
    )
    .unwrap();
    let geometry = WindowGeometry::uncut(layer_rect, support, extent)
        .unwrap()
        .with_site_origin((0, 0));
    let want = render_layer_window(
        &AlphaPlaneRef::new(layer_rect, &alpha).unwrap(),
        &geometry,
        &content_rgb,
        &backdrop_plane,
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
        NORMAL,
        0.9,
        0.8,
    );
    for (chunk, threads) in [(64, 1), (13, 3)] {
        let (result, grid) = rendered(chunk, threads, &p, &with_effects(&ops), 1 << 26, &sources);
        result.unwrap();
        let mut visible_shadow = 0_usize;
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
                // The blurred shadow of the tile reaches pixels far outside it.
                visible_shadow = visible_shadow.saturating_add(usize::from(
                    (got[0] - backdrop[0]).abs() > 1e-7 && !(7..23).contains(&y),
                ));
            }
        }
        assert!(
            visible_shadow > 100,
            "{visible_shadow} pixels carry the blur"
        );
    }
}

// ---- 3. the window cost is charged ---------------------------------------------------------------

#[test]
fn window_cost_is_charged_and_limit_minus_one_is_refused_before_any_sink_call() {
    // 12 x 10 plan, chunk 8 (largest chunk 8 x 8), one worker, a 4 x 4 red layer at (4, 3) with a
    // size 30 shadow (margin feather_support = 45, refused before the window API existed).
    // Charge, hand-derived: v1 scratch 8 * 8 * 32 = 2048; one effect record 64; the content frame
    // (layer 4 x 4 clipped from the window grown by 45) is 16 pixels * 16 bytes = 256; the window
    // arrays (8 * 8 pixels * 28 bytes = 1792) plus pigment's predicted peak for an 8 x 8 window.
    let extent = rect(0, 0, 12, 10);
    let ops = [EffectOp::DropShadow(
        DropShadowOp::new(BLACK, NORMAL, 0.5).with_size(30.0),
    )];
    assert_eq!(stack_support_px(&ops).unwrap(), 45);
    let peak = layer_window_cost(&ops, rect(0, 0, 8, 8))
        .unwrap()
        .peak_bytes;
    assert!(peak > 0);
    let fixed: u64 = 2048 + 64 + 256 + 1792;
    let need = fixed.wrapping_add(peak);
    let p = effect_plan(
        extent,
        [0.0, 0.0, 0.0, 1.0],
        solid(rect(4, 3, 4, 4), [1.0, 0.0, 0.0, 1.0]),
        NORMAL,
        1.0,
        1.0,
    );
    let ext = with_effects(&ops);
    let (result, grid) = rendered(8, 1, &p, &ext, need, &no_sources());
    let receipt = result.unwrap();
    assert_eq!(receipt.peak_working_bytes, need);
    assert_eq!(receipt.chunks, 4);
    near(grid.at(5, 4), [1.0, 0.0, 0.0, 1.0], 1e-6);
    // The shadow (black on black) is invisible; the layer pixel and the backdrop are exact.
    near(grid.at(0, 0), [0.0, 0.0, 0.0, 1.0], 1e-6);
    let below = need.wrapping_sub(1);
    let (result, refused) = rendered(8, 1, &p, &ext, below, &no_sources());
    assert_eq!(
        result,
        Err(BackendError::BudgetExceeded {
            requested: need,
            limit: below
        })
    );
    assert_eq!(refused.calls, 0, "refused before the first sink call");
    // Two workers: v1 scratch 64 pixels * (2 * 32 + 2 * 32 in flight) = 8192, the effect record
    // once (64), and the frame + window part (256 + 1792 + peak) once per worker.
    let (result, _) = rendered(8, 2, &p, &ext, 1 << 20, &no_sources());
    let per_worker = 256_u64.wrapping_add(1792).wrapping_add(peak);
    let two = 8192_u64
        .wrapping_add(64)
        .wrapping_add(per_worker.wrapping_mul(2));
    assert_eq!(result.unwrap().peak_working_bytes, two);
}

// ---- 4. work limit and hostile geometry -----------------------------------------------------------

#[test]
fn work_limit_and_hostile_geometry_stay_typed() {
    // A size 1000 shadow on a 512 x 512 layer with one 512 x 512 window is predicted above the
    // work limit: refused as `effect_work_limit` before the first sink call.
    let extent = rect(0, 0, 512, 512);
    let ops = [EffectOp::DropShadow(
        DropShadowOp::new(BLACK, NORMAL, 0.5).with_size(1000.0),
    )];
    let cost = layer_window_cost(&ops, extent).unwrap();
    assert!(cost.work > MAX_EFFECT_WINDOW_WORK, "{cost:?}");
    let big = effect_plan(
        extent,
        [0.0; 4],
        solid(extent, [1.0, 0.0, 0.0, 1.0]),
        NORMAL,
        1.0,
        1.0,
    );
    let (result, grid) = rendered(512, 1, &big, &with_effects(&ops), 1 << 32, &no_sources());
    assert_eq!(
        result,
        Err(BackendError::Unsupported {
            operator: "effect_work_limit",
            exactness: Exactness::Unsupported
        })
    );
    assert_eq!(grid.calls, 0);
    // The smaller window of an 8 x 8 chunk is within the limit.
    assert!(layer_window_cost(&ops, rect(0, 0, 8, 8)).unwrap().work <= MAX_EFFECT_WINDOW_WORK);

    // Distance 30000 (the pigment maximum): the margin is 30000 px, the frame stays the 4 x 4
    // layer and the shadow lies far outside the plan, so the render is the plain layer.
    let small = rect(0, 0, 8, 8);
    let far = [EffectOp::DropShadow(
        DropShadowOp::new(BLACK, NORMAL, 1.0).with_offset(0.0, 30_000.0),
    )];
    assert_eq!(stack_support_px(&far).unwrap(), 30_000);
    let p = effect_plan(
        small,
        [0.25, 0.5, 0.75, 1.0],
        solid(rect(2, 2, 4, 4), [1.0, 0.0, 0.0, 1.0]),
        NORMAL,
        1.0,
        1.0,
    );
    let (result, grid) = rendered(4, 2, &p, &with_effects(&far), 1 << 20, &no_sources());
    result.unwrap();
    for y in 0..8_i64 {
        for x in 0..8_i64 {
            let inside = (2..6).contains(&x) && (2..6).contains(&y);
            let want = if inside {
                [1.0, 0.0, 0.0, 1.0]
            } else {
                [0.25, 0.5, 0.75, 1.0]
            };
            near(grid.at(x, y), want, 1e-6);
        }
    }
}

// ---- 5. group content and a feathered mask ----------------------------------------------------------

#[test]
fn group_content_and_feathered_mask_frames_match_the_windowed_oracle() {
    // The layer is an isolated group of two solids with a u8 mask (feather 2 px, opacity 0.85,
    // fill 0.7) and a shadow + stroke. The oracle renders the same content alone (group + feathered
    // mask over transparent, no effects) and feeds its alpha and colour to pigment's windowed
    // composition over one whole-extent window. The content-only render re-derives straight colour
    // from premultiplied arithmetic, so colours are compared within 1e-5; the alpha of the layer
    // shape is the same product in both.
    let extent = rect(0, 0, 30, 26);
    let mask = MaskData::u8(30, 26, |x, y| {
        u8::try_from(x.wrapping_mul(9).wrapping_add(y.wrapping_mul(7)) % 256).unwrap()
    });
    let sources = Sources {
        tiles: vec![],
        masks: vec![(1, &mask)],
    };
    let a = solid(rect(8, 6, 14, 12), [0.9, 0.2, 0.1, 1.0]);
    let b = solid(rect(12, 10, 10, 10), [0.1, 0.8, 0.3, 0.75]);
    let inner = group(vec![layer(1, NORMAL, 1.0), layer(2, MULTIPLY, 0.9)]);
    let ops = [
        EffectOp::DropShadow(
            DropShadowOp::new([0.0, 0.1, 0.2], MULTIPLY, 0.7)
                .with_offset(30.0, 2.0)
                .with_size(6.0),
        ),
        EffectOp::Stroke(StrokeOp::new(
            BLUE,
            NORMAL,
            0.8,
            2.0,
            StrokePosition::Outside,
        )),
    ];
    let masked = |opacity: f32, fill: f32| Layer {
        fill_opacity: fill,
        mask: Some(mask.input(1, false, 1.0)),
        ..layer(3, NORMAL, opacity)
    };
    let backdrop = [0.3_f32, 0.5, 0.7, 1.0];
    let with_fx = plan(
        extent,
        vec![
            solid(extent, backdrop),
            a.clone(),
            b.clone(),
            inner,
            group(vec![layer(0, NORMAL, 1.0), masked(0.85, 0.7)]),
        ],
        4,
    );
    let ext = envelope(vec![
        one_layer(4, 1)
            .with_effects(ops.to_vec())
            .with_mask_feather(2.0),
    ]);
    // Content only: the same group and mask, feather 2, over a transparent target (the two solids
    // are nodes 0 and 1 here, nodes 1 and 2 above).
    let alone = plan(
        extent,
        vec![
            a,
            b,
            group(vec![layer(0, NORMAL, 1.0), layer(1, MULTIPLY, 0.9)]),
            group(vec![Layer {
                mask: Some(mask.input(1, false, 1.0)),
                ..layer(2, NORMAL, 1.0)
            }]),
        ],
        3,
    );
    let (result, shape) = rendered(
        30,
        1,
        &alone,
        &envelope(vec![one_layer(3, 0).with_mask_feather(2.0)]),
        1 << 20,
        &sources,
    );
    result.unwrap();
    let layer_rect = rect(8, 6, 14, 14);
    let mut alpha = Vec::new();
    let mut content_rgb = Vec::new();
    for y in 0..26_i64 {
        for x in 0..30_i64 {
            let p = shape.at(x, y);
            content_rgb.push([p[0], p[1], p[2]]);
            if (8..22).contains(&x) && (6..20).contains(&y) {
                alpha.push(p[3]);
            }
        }
    }
    let support = stack_support_px(&ops).unwrap();
    let backdrop_plane = WindowPlane::new(
        extent,
        vec![[backdrop[0], backdrop[1], backdrop[2]]; 780],
        vec![backdrop[3]; 780],
    )
    .unwrap();
    let want = render_layer_window(
        &AlphaPlaneRef::new(layer_rect, &alpha).unwrap(),
        &WindowGeometry::uncut(layer_rect, support, extent)
            .unwrap()
            .with_site_origin((0, 0)),
        &content_rgb,
        &backdrop_plane,
        LayerParams {
            mode: NORMAL,
            opacity: 0.85,
            fill_opacity: 0.7,
        },
        &ops,
        WindowLimits::new(16 << 20, 1_000_000_000),
    )
    .unwrap();
    let mut first: Option<Vec<[u32; 4]>> = None;
    for (chunk, threads) in [(30, 1), (11, 1), (7, 3)] {
        let (result, grid) = rendered(chunk, threads, &with_fx, &ext, 1 << 22, &sources);
        result.unwrap();
        for y in 0..26_u32 {
            for x in 0..30_u32 {
                let at = usize::try_from(y.wrapping_mul(30).wrapping_add(x)).unwrap();
                let w = [
                    want.plane.rgb()[at][0],
                    want.plane.rgb()[at][1],
                    want.plane.rgb()[at][2],
                    want.plane.alpha()[at],
                ];
                near(grid.at(i64::from(x), i64::from(y)), w, 1e-5);
            }
        }
        let all: Vec<[u32; 4]> = grid.px.iter().copied().map(bits).collect();
        assert_eq!(
            first.get_or_insert_with(|| all.clone()),
            &all,
            "chunk {chunk}"
        );
    }
}
