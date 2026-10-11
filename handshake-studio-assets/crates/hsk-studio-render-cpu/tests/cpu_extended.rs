//! Envelope operators of the CPU backend (`RenderBackend::execute_extended`). Five focused tests:
//! adjustment layers (hand values, single-pixel parity with pigment's stack oracle, clip run),
//! mask density, Blend-If ramps, mask feather (hand-convolved values, chunk and thread
//! independence, budget charge) and the refusals plus the unchanged v1 path. Photoshop parity of
//! every operator is NOT_PROVEN (oracle list D5 cases 1, 9, 10): expected values are derived by
//! hand from pigment's documented formulas, never from a Photoshop render.
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::adjust_math::{
    AdjustmentKind, AdjustmentOp, BrightnessContrast, Curves, Exposure, HueSaturation, Levels,
    LevelsRecord,
};
use hsk_studio_pigment::blend_math::{BlendContext, PixelSite, Rgba, StackNode, evaluate_stack};
use hsk_studio_pigment::coverage_math::{BlendChannel, BlendRangeEntry, IDENTITY_RANGE};
use hsk_studio_pigment::render_plan::*;
use hsk_studio_render_cpu::Renderer;

mod common;
use common::*;

// ---- 1. adjustment layers --------------------------------------------------------------------

/// Backdrop solid (isolated group layer 0, Normal) then an adjustment layer (layer 1).
fn adjustment_plan(backdrop: [f32; 4], mode: StudioBlendMode, opacity: f32) -> RenderPlan {
    let one = rect(0, 0, 1, 1);
    plan(
        one,
        vec![
            solid(one, backdrop),
            placeholder(one),
            group(vec![
                layer(0, StudioBlendMode::Normal, 1.0),
                layer(1, mode, opacity),
            ]),
        ],
        2,
    )
}

#[test]
fn adjustment_layer_hand_values_pigment_parity_and_clip_run() {
    // Hand values (pigment rule: colour lerp(Cb, B(Cb, f(Cb)), k), alpha unchanged). Invert over
    // opaque (0.2, 0.4, 0.6) at opacity 0.5: 0.2 + (0.8 - 0.2) * 0.5 = 0.5 per channel.
    let invert = envelope(vec![one_layer(2, 1).with_adjustment(AdjustmentOp::Invert)]);
    let opaque = adjustment_plan([0.2, 0.4, 0.6, 1.0], StudioBlendMode::Normal, 0.5);
    near(
        pixel(&opaque, &invert, &no_sources()),
        [0.5, 0.5, 0.5, 1.0],
        1e-6,
    );
    // Over a half-transparent backdrop the alpha stays 0.5.
    let thin = adjustment_plan([0.2, 0.4, 0.6, 0.5], StudioBlendMode::Normal, 0.5);
    near(
        pixel(&thin, &invert, &no_sources()),
        [0.5, 0.5, 0.5, 0.5],
        1e-6,
    );
    // Receipt: the envelope digest, pigment's classes (rule Approximate, invert Exact).
    let (result, _) = run_ext(
        &Renderer::new(1).unwrap(),
        &opaque,
        &invert,
        Exactness::Approximate,
        1 << 20,
        &no_sources(),
    );
    let receipt = result.unwrap();
    assert_eq!(receipt.plan_digest, invert.digest_with(&opaque).unwrap());
    assert_ne!(receipt.plan_digest, opaque.digest().unwrap());
    assert_eq!(receipt.exactness.lowest, Exactness::Approximate);
    let class_of = |key: &str| {
        receipt
            .exactness
            .operators
            .iter()
            .find(|o| o.key == key)
            .map(|o| o.exactness)
    };
    assert_eq!(class_of("adjustment_layer"), Some(Exactness::Approximate));
    assert_eq!(class_of("invert"), Some(Exactness::Exact));

    // Single-pixel parity with pigment's stack oracle (`evaluate_stack`), every implemented kind,
    // opaque and half-transparent backdrops: the backend calls the same functions, so the result
    // is bit-identical.
    let record = |input_black, input_white, gamma, output_black, output_white| LevelsRecord {
        input_black,
        input_white,
        gamma,
        output_black,
        output_white,
    };
    let ops = [
        AdjustmentOp::Levels(Levels {
            composite: record(0.1, 0.9, 1.5, 0.05, 0.95),
            red: LevelsRecord::IDENTITY,
            green: record(0.0, 0.8, 1.0, 0.0, 1.0),
            blue: LevelsRecord::IDENTITY,
        }),
        AdjustmentOp::Curves(Curves {
            composite: vec![[0.0, 0.0], [0.25, 0.4], [0.75, 0.6], [1.0, 1.0]],
            red: vec![],
            green: vec![],
            blue: vec![],
        }),
        AdjustmentOp::HueSaturation(HueSaturation {
            hue: 60.0,
            saturation: -30.0,
            lightness: 10.0,
            colorize: false,
        }),
        AdjustmentOp::BrightnessContrast(BrightnessContrast {
            brightness: 20.0,
            contrast: 30.0,
            use_legacy: true,
        }),
        AdjustmentOp::Exposure(Exposure {
            exposure: 1.0,
            offset: 0.1,
            gamma: 1.5,
            linearize_gamma: 1.0,
        }),
        AdjustmentOp::Invert,
        AdjustmentOp::Threshold { level: 0.5 },
        AdjustmentOp::Posterize { levels: 4 },
    ];
    let ctx = BlendContext {
        space: BlendSpace::LinearLight,
        site: PixelSite {
            x: 0,
            y: 0,
            seed: 7,
        },
    };
    let mut compared = 0_u32;
    for (i, op) in ops.iter().enumerate() {
        let mode = [StudioBlendMode::Normal, StudioBlendMode::Multiply][i % 2];
        let opacity = [0.5, 0.75, 1.0][i % 3];
        for backdrop in [[0.2, 0.4, 0.6, 1.0], [0.7, 0.3, 0.1, 0.5]] {
            let plan = adjustment_plan(backdrop, mode, opacity);
            let ext = envelope(vec![one_layer(2, 1).with_adjustment(op.clone())]);
            let got = pixel(&plan, &ext, &no_sources());
            let (want, _) = evaluate_stack(
                &[
                    StackNode::Leaf {
                        source: Rgba::new([backdrop[0], backdrop[1], backdrop[2]], backdrop[3]),
                        mode: StudioBlendMode::Normal,
                        opacity: 1.0,
                    },
                    StackNode::Adjustment {
                        op: op.clone(),
                        mode,
                        opacity,
                    },
                ],
                Rgba::TRANSPARENT,
                &ctx,
            )
            .unwrap();
            let want = [want.rgb[0], want.rgb[1], want.rgb[2], want.a];
            assert_eq!(
                got.map(f32::to_bits),
                want.map(f32::to_bits),
                "{:?} {mode:?} {opacity} over {backdrop:?}",
                op.kind()
            );
            compared = compared.saturating_add(1);
        }
    }
    assert_eq!(compared, 16);

    // Clip run: an adjustment layer clipped to a base adjusts only the base. B = gray 0.2 across
    // two pixels, base A = red on the left pixel, invert clipped to A: the left pixel is the
    // inverted A (0, 1, 1) and the right pixel keeps B (0.2) instead of being inverted to 0.8.
    let wide = rect(0, 0, 2, 1);
    let clip_plan = plan(
        wide,
        vec![
            solid(wide, [0.2, 0.2, 0.2, 1.0]),
            solid(rect(0, 0, 1, 1), [1.0, 0.0, 0.0, 1.0]),
            placeholder(wide),
            group(vec![
                layer(0, StudioBlendMode::Normal, 1.0),
                layer(1, StudioBlendMode::Normal, 1.0),
                Layer {
                    mask: Some(MaskInput::clip_to_base()),
                    ..layer(2, StudioBlendMode::Normal, 1.0)
                },
            ]),
        ],
        3,
    );
    let clip_ext = envelope(vec![one_layer(3, 2).with_adjustment(AdjustmentOp::Invert)]);
    let (result, grid) = run_ext(
        &Renderer::new(2).unwrap(),
        &clip_plan,
        &clip_ext,
        Exactness::Approximate,
        1 << 20,
        &no_sources(),
    );
    result.unwrap();
    near(grid.at(0, 0), [0.0, 1.0, 1.0, 1.0], 1e-6);
    near(grid.at(1, 0), [0.2, 0.2, 0.2, 1.0], 1e-6);
}

// ---- 2. mask density -------------------------------------------------------------------------

#[test]
fn mask_density_and_invert_hand_values() {
    // Black, then a Normal red layer through a 16-bit mask of 16384/65535 = 0.25000381 coverage.
    // Effective coverage `1 - density * (1 - m)`: density 0.5 -> 0.5 + 0.5 * 0.25000381 =
    // 0.62500191 (the brief's 0.25 -> 0.625); density 0 -> 1; inverted at density 0.5 ->
    // 1 - 0.5 * 0.25000381 = 0.87499809. The red layer over opaque black has red = coverage.
    let one = rect(0, 0, 1, 1);
    let mask = MaskData::u16(1, 1, 16384);
    let sources = Sources {
        tiles: vec![],
        masks: vec![(1, &mask)],
    };
    let masked = |inverted: bool, density: f32| {
        plan(
            one,
            vec![
                solid(one, [0.0, 0.0, 0.0, 1.0]),
                solid(one, [1.0, 0.0, 0.0, 1.0]),
                group(vec![
                    layer(0, StudioBlendMode::Normal, 1.0),
                    Layer {
                        mask: Some(mask.input(1, inverted, density)),
                        ..layer(1, StudioBlendMode::Normal, 1.0)
                    },
                ]),
            ],
            2,
        )
    };
    let empty = PlanExtensions::default();
    for (inverted, density, want) in [
        (false, 0.5, 0.625_001_9),
        (false, 0.0, 1.0),
        (true, 0.5, 0.874_998_1),
    ] {
        let p = masked(inverted, density);
        // A density mask needs no envelope entry: the empty envelope still takes the envelope
        // admission, which reports `mask_density` (Approximate).
        let (result, grid) = run_ext(
            &Renderer::new(1).unwrap(),
            &p,
            &empty,
            Exactness::Approximate,
            1 << 20,
            &sources,
        );
        let receipt = result.unwrap();
        near(grid.at(0, 0), [want, 0.0, 0.0, 1.0], 1e-6);
        assert!(
            receipt
                .exactness
                .operators
                .iter()
                .any(|o| { o.key == "mask_density" && o.exactness == Exactness::Approximate })
        );
    }
    // The v1 path keeps today's refusal, before any sink call.
    let p = masked(false, 0.5);
    let mut grid = Grid::new(one);
    let refused = Renderer::new(1).unwrap().execute(
        &p,
        &req(Exactness::Approximate, 1 << 20),
        &sources,
        &CancellationToken::default(),
        &mut grid,
    );
    assert_eq!(
        refused,
        Err(BackendError::Unsupported {
            operator: "mask_density",
            exactness: Exactness::Unsupported
        })
    );
    assert_eq!(grid.calls, 0);
}

// ---- 3. Blend-If -----------------------------------------------------------------------------

#[test]
fn blend_if_ramps_hand_values() {
    // Source gray ramp over opaque black, Normal, opacity 1: out = weight * value. Source Gray
    // range [0.2, 0.4, 0.6, 1.0]: 0 below 0.2, linear up to 0.4, 1 up to 0.6, linear down to 0
    // at 1.0. Weights for 0.1 .. 1.0: 0, 0, 0.5, 1, 1, 0.5, 0 (0.3 -> (0.3 - 0.2) / 0.2 = 0.5,
    // 0.8 -> (1.0 - 0.8) / 0.4 = 0.5).
    let values = [0.1_f32, 0.2, 0.3, 0.4, 0.5, 0.8, 1.0];
    let weights = [0.0_f32, 0.0, 0.5, 1.0, 1.0, 0.5, 0.0];
    let wide = rect(0, 0, 7, 1);
    let ramp = TileData::new(7, 1, |x, _| {
        let v = values[usize::try_from(x).unwrap()];
        [v, v, v, 1.0]
    });
    let sources = Sources {
        tiles: vec![(1, ramp.planes())],
        masks: vec![],
    };
    let ramp_plan = plan(
        wide,
        vec![
            solid(wide, [0.0, 0.0, 0.0, 1.0]),
            PlanNode::Tile(ramp.input(1)),
            group(vec![
                layer(0, StudioBlendMode::Normal, 1.0),
                layer(1, StudioBlendMode::Normal, 1.0),
            ]),
        ],
        2,
    );
    let gray = BlendRangeEntry {
        channel: BlendChannel::Gray,
        source: [0.2, 0.4, 0.6, 1.0],
        destination: IDENTITY_RANGE,
    };
    let ext = envelope(vec![one_layer(2, 1).with_blend_if(vec![gray])]);
    let (result, grid) = run_ext(
        &Renderer::new(3).unwrap(),
        &ramp_plan,
        &ext,
        Exactness::Approximate,
        1 << 20,
        &sources,
    );
    let receipt = result.unwrap();
    for (i, (v, w)) in values.iter().zip(weights).enumerate() {
        let want = v * w;
        near(
            grid.at(i64::try_from(i).unwrap(), 0),
            [want, want, want, 1.0],
            1e-6,
        );
    }
    assert!(
        receipt
            .exactness
            .operators
            .iter()
            .any(|o| { o.key == "blend_if" && o.exactness == Exactness::Approximate })
    );

    // Source weight 0.5 (gray 0.3) times underlying weight 0.5 (backdrop red 0.25 under Red
    // destination [0, 0.5, 1, 1]: (0.25 - 0) / (0.5 - 0)) is 0.25 of the source alpha:
    // out = 0.25 * 0.3 + 0.75 * backdrop = (0.2625, 0.075, 0.075).
    let one = rect(0, 0, 1, 1);
    let both = plan(
        one,
        vec![
            solid(one, [0.25, 0.0, 0.0, 1.0]),
            solid(one, [0.3, 0.3, 0.3, 1.0]),
            group(vec![
                layer(0, StudioBlendMode::Normal, 1.0),
                layer(1, StudioBlendMode::Normal, 1.0),
            ]),
        ],
        2,
    );
    let ext = envelope(vec![one_layer(2, 1).with_blend_if(vec![
        gray,
        BlendRangeEntry {
            channel: BlendChannel::Red,
            source: IDENTITY_RANGE,
            destination: [0.0, 0.5, 1.0, 1.0],
        },
    ])]);
    near(
        pixel(&both, &ext, &no_sources()),
        [0.2625, 0.075, 0.075, 1.0],
        1e-6,
    );

    // The Blend-If source of an adjustment layer is its own content f(backdrop): invert over gray
    // 0.2 gives content gray 0.8; White pair [0.5, 0.9] weighs it (0.9 - 0.8) / 0.4 = 0.25, so
    // out = 0.2 + (0.8 - 0.2) * 0.25 = 0.35 (reading the backdrop 0.2 instead would give 0.8).
    let adjusted = adjustment_plan([0.2, 0.2, 0.2, 1.0], StudioBlendMode::Normal, 1.0);
    let ext = envelope(vec![
        one_layer(2, 1)
            .with_adjustment(AdjustmentOp::Invert)
            .with_blend_if(vec![BlendRangeEntry {
                channel: BlendChannel::Gray,
                source: [0.0, 0.0, 0.5, 0.9],
                destination: IDENTITY_RANGE,
            }]),
    ]);
    near(
        pixel(&adjusted, &ext, &no_sources()),
        [0.35, 0.35, 0.35, 1.0],
        1e-6,
    );
}

// ---- 4. mask feather -------------------------------------------------------------------------

#[test]
fn mask_feather_hand_values_budget_and_aggregate() {
    // Hard vertical edge at x = 6 (left coverage 0, right 1) on a 12 x 3 plane, red layer over
    // black, feather 2 px: sigma = 1, support ceil(3) = 3, taps exp(-i^2 / 2) / S with
    // S = 1 + 2 (e^-0.5 + e^-2 + e^-4.5) = 2.5059499: w0 = 0.39905028, w1 = 0.24203623,
    // w2 = 0.05400558, w3 = 0.00443305 (they sum to 1). Hand-convolved coverage at x = 2 .. 9:
    // 0, w3, w2 + w3, w1 + w2 + w3, 1 - (w1 + w2 + w3), 1 - (w2 + w3), 1 - w3, 1.
    let (w0, w1, w2, w3) = (
        0.399_050_28_f32,
        0.242_036_23_f32,
        0.054_005_58_f32,
        0.004_433_05_f32,
    );
    assert!((w0 + 2.0 * (w1 + w2 + w3) - 1.0).abs() < 1e-6);
    let want_cover = [
        0.0,
        w3,
        w2 + w3,
        w1 + w2 + w3,
        1.0 - (w1 + w2 + w3),
        1.0 - (w2 + w3),
        1.0 - w3,
        1.0,
    ];
    let wide = rect(0, 0, 12, 3);
    let mask = MaskData::u8(12, 3, |x, _| if x >= 6 { 255 } else { 0 });
    let sources = Sources {
        tiles: vec![],
        masks: vec![(1, &mask)],
    };
    let feathered = |nodes_extra: usize| {
        // Layer 1 (red) and, for the aggregate case, layer 2 (green) share the mask plane.
        let mut layers = vec![
            layer(0, StudioBlendMode::Normal, 1.0),
            Layer {
                mask: Some(mask.input(1, false, 1.0)),
                ..layer(1, StudioBlendMode::Normal, 1.0)
            },
        ];
        let mut nodes = vec![
            solid(wide, [0.0, 0.0, 0.0, 1.0]),
            solid(wide, [1.0, 0.0, 0.0, 1.0]),
        ];
        if nodes_extra > 0 {
            nodes.push(solid(wide, [0.0, 1.0, 0.0, 1.0]));
            layers.push(Layer {
                mask: Some(mask.input(1, false, 1.0)),
                ..layer(2, StudioBlendMode::Normal, 1.0)
            });
        }
        let root = u32::try_from(nodes.len()).unwrap();
        nodes.push(group(layers));
        plan(wide, nodes, root)
    };
    let single = feathered(0);
    let ext = envelope(vec![one_layer(2, 1).with_mask_feather(2.0)]);
    let render = |chunk: u32, threads: usize, budget: u64| {
        run_ext(
            &Renderer::with_threads(chunk, threads).unwrap(),
            &single,
            &ext,
            Exactness::Approximate,
            budget,
            &sources,
        )
    };
    let (result, grid) = render(12, 1, 1 << 20);
    let receipt = result.unwrap();
    for y in 0..3 {
        for (i, want) in want_cover.iter().enumerate() {
            let x = i64::try_from(i).unwrap().wrapping_add(2);
            near(grid.at(x, y), [*want, 0.0, 0.0, 1.0], 2e-6);
        }
    }
    assert!(
        receipt
            .exactness
            .operators
            .iter()
            .any(|o| { o.key == "mask_feather" && o.exactness == Exactness::Approximate })
    );
    // The margin crosses chunk borders: chunk 5 and three workers give the same bytes.
    for (chunk, threads) in [(5, 1), (5, 3), (4, 2)] {
        let (result, other) = render(chunk, threads, 1 << 20);
        result.unwrap();
        assert!(
            other
                .px
                .iter()
                .zip(&grid.px)
                .all(|(a, b)| a.map(f32::to_bits) == b.map(f32::to_bits)),
            "chunk {chunk} threads {threads}"
        );
    }

    // Charge (hand-derived, one worker, chunk 12 x 3 = 36 px, no group nesting): v1 scratch
    // 36 * 32 = 1152; kernel 7 taps * 4 = 28; feather scratch (chunk plane 36 + horizontal pass
    // 12 * (3 + 2 * 3) = 108 samples) * 4 = 576; total 1756. One byte less is refused with
    // BudgetExceeded before any sink call.
    assert_eq!(receipt.peak_working_bytes, 1756);
    let (result, grid) = render(12, 1, 1755);
    assert_eq!(
        result,
        Err(BackendError::BudgetExceeded {
            requested: 1756,
            limit: 1755
        })
    );
    assert_eq!(grid.calls, 0);
    assert!(render(12, 1, 1756).0.is_ok());
    // A feather far wider than the budget (10000 px: support 15000 rows of margin) is refused.
    let huge = envelope(vec![one_layer(2, 1).with_mask_feather(10_000.0)]);
    let (result, grid) = run_ext(
        &Renderer::new(12).unwrap(),
        &single,
        &huge,
        Exactness::Approximate,
        1 << 20,
        &sources,
    );
    assert!(
        matches!(result, Err(BackendError::BudgetExceeded { .. })),
        "{result:?}"
    );
    assert_eq!(grid.calls, 0);

    // Aggregate over budget: two layers with distinct radii (2 px: 7 taps = 28 bytes, 3 px:
    // support 5, 11 taps = 44 bytes) are charged as a running sum before the second kernel is
    // built: 28 then 72 > 50. Each alone fits.
    let double = feathered(1);
    let both = envelope(vec![
        one_layer(3, 1).with_mask_feather(2.0),
        one_layer(3, 2).with_mask_feather(3.0),
    ]);
    let (result, grid) = run_ext(
        &Renderer::new(12).unwrap(),
        &double,
        &both,
        Exactness::Approximate,
        50,
        &sources,
    );
    assert_eq!(
        result,
        Err(BackendError::BudgetExceeded {
            requested: 72,
            limit: 50
        })
    );
    assert_eq!(grid.calls, 0);
    // With room for both it renders; the scratch is sized by the wider margin (support 5):
    // 1152 + 72 + (36 + 12 * (3 + 10)) * 4 = 1152 + 72 + 768 = 1992.
    let (result, _) = run_ext(
        &Renderer::new(12).unwrap(),
        &double,
        &both,
        Exactness::Approximate,
        1992,
        &sources,
    );
    assert_eq!(result.unwrap().peak_working_bytes, 1992);
}

// ---- 5. refusals and the unchanged v1 path ---------------------------------------------------

#[test]
fn refusals_stay_typed_and_the_v1_path_is_unchanged() {
    let renderer = Renderer::new(1).unwrap();
    // Capability classes: pigment owns the exactness of every operator (CX-CAP-001), so a
    // supported operator reports exactly pigment's declared class, never a better one (invert is
    // Exact there, the rest Approximate); knockout is refused.
    let backend: &dyn RenderBackend = &renderer;
    let supported = [
        ExtOperator::AdjustmentLayer,
        ExtOperator::BlendIf,
        ExtOperator::MaskDensity,
        ExtOperator::MaskFeather,
    ]
    .into_iter()
    .chain(AdjustmentKind::ALL.map(ExtOperator::Adjustment));
    for op in supported {
        assert_eq!(backend.extension_exactness(op), op.intrinsic(), "{op:?}");
    }
    assert_eq!(
        backend.extension_exactness(ExtOperator::Adjustment(AdjustmentKind::Invert)),
        Exactness::Exact
    );
    assert_eq!(
        backend.extension_exactness(ExtOperator::MaskFeather),
        Exactness::Approximate
    );
    for depth in [Knockout::Shallow, Knockout::Deep] {
        assert_eq!(
            backend.extension_exactness(ExtOperator::Knockout(depth)),
            Exactness::Unsupported
        );
    }

    let p = adjustment_plan([0.2, 0.4, 0.6, 1.0], StudioBlendMode::Normal, 0.5);
    let refused = |ext: &PlanExtensions, min: Exactness| {
        let (result, grid) = run_ext(&renderer, &p, ext, min, 1 << 20, &no_sources());
        assert_eq!(grid.calls, 0, "refusal happens before the first sink call");
        result
    };
    // Knockout (shallow and deep) before any sink call.
    for (depth, key) in [
        (Knockout::Shallow, "knockout_shallow"),
        (Knockout::Deep, "knockout_deep"),
    ] {
        assert_eq!(
            refused(
                &envelope(vec![one_layer(2, 1).with_knockout(depth)]),
                Exactness::Approximate
            ),
            Err(BackendError::Unsupported {
                operator: key,
                exactness: Exactness::Unsupported
            })
        );
    }
    // The modern Brightness/Contrast transfer has no documented formula: typed refusal.
    let modern = AdjustmentOp::BrightnessContrast(BrightnessContrast {
        brightness: 10.0,
        contrast: 10.0,
        use_legacy: false,
    });
    assert_eq!(
        refused(
            &envelope(vec![one_layer(2, 1).with_adjustment(modern)]),
            Exactness::Approximate
        ),
        Err(BackendError::Unsupported {
            operator: "brightness_contrast",
            exactness: Exactness::Unsupported
        })
    );
    // An Exact floor refuses the Approximate adjustment-layer rule.
    assert_eq!(
        refused(
            &envelope(vec![one_layer(2, 1).with_adjustment(AdjustmentOp::Invert)]),
            Exactness::Exact
        ),
        Err(BackendError::Unsupported {
            operator: "adjustment_layer",
            exactness: Exactness::Approximate
        })
    );
    // An adjustment whose input is not the placeholder is an invalid plan, not a render.
    let bad = envelope(vec![one_layer(2, 0).with_adjustment(AdjustmentOp::Invert)]);
    assert_eq!(
        refused(&bad, Exactness::Approximate),
        Err(BackendError::InvalidPlan(PlanError::AdjustmentInput {
            node: 2
        }))
    );

    // v1 path: a plan with an empty envelope is the v1 plan. Backdrop (0.2, 0.4, 0.6) with a red
    // layer at opacity 0.5 is (0.6, 0.2, 0.3, 1.0) by hand; `execute_extended` with an empty
    // envelope and `execute` give identical bytes and receipts on a larger chunked, threaded
    // plan with an inverted 8-bit mask.
    let one = rect(0, 0, 1, 1);
    let known = plan(
        one,
        vec![
            solid(one, [0.2, 0.4, 0.6, 1.0]),
            solid(one, [1.0, 0.0, 0.0, 1.0]),
            group(vec![
                layer(0, StudioBlendMode::Normal, 1.0),
                layer(1, StudioBlendMode::Normal, 0.5),
            ]),
        ],
        2,
    );
    near(
        pixel(&known, &PlanExtensions::default(), &no_sources()),
        [0.6, 0.2, 0.3, 1.0],
        1e-6,
    );
    let wide = rect(0, 0, 9, 4);
    let tile = TileData::new(9, 4, |x, y| {
        let t = f32::from(u8::try_from(x.wrapping_add(y)).unwrap()) / 16.0;
        [t, 1.0 - t, 0.5, 0.75]
    });
    let mask = MaskData::u8(9, 4, |x, y| {
        u8::try_from(x.wrapping_mul(25).wrapping_add(y)).unwrap()
    });
    let sources = Sources {
        tiles: vec![(1, tile.planes())],
        masks: vec![(2, &mask)],
    };
    let busy = plan(
        wide,
        vec![
            solid(wide, [0.1, 0.2, 0.3, 1.0]),
            PlanNode::Tile(tile.input(1)),
            group(vec![
                layer(0, StudioBlendMode::Normal, 1.0),
                Layer {
                    mask: Some(mask.input(2, true, 1.0)),
                    ..layer(1, StudioBlendMode::Multiply, 0.8)
                },
            ]),
        ],
        2,
    );
    let threaded = Renderer::with_threads(4, 3).unwrap();
    let mut v1 = Grid::new(wide);
    let a = threaded
        .execute(
            &busy,
            &req(Exactness::Approximate, 1 << 20),
            &sources,
            &CancellationToken::default(),
            &mut v1,
        )
        .unwrap();
    let (b, ext) = run_ext(
        &threaded,
        &busy,
        &PlanExtensions::default(),
        Exactness::Approximate,
        1 << 20,
        &sources,
    );
    let b = b.unwrap();
    assert!(
        v1.px
            .iter()
            .zip(&ext.px)
            .all(|(x, y)| x.map(f32::to_bits) == y.map(f32::to_bits))
    );
    assert_eq!(a.plan_digest, b.plan_digest);
    assert_eq!(a.exactness, b.exactness);
    assert_eq!(a.peak_working_bytes, b.peak_working_bytes);
    assert_eq!((a.chunks, a.nodes_executed), (b.chunks, b.nodes_executed));
}
