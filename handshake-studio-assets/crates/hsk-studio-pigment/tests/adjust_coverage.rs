//! Hand-computed single-pixel oracle values for `adjust_math` and `coverage_math` (decision:
//! work-folder `diagnostics/adjustments-advanced-blending.md`). Every expected number below is
//! derived by hand in the comment next to it, not produced by the code under test.
use hsk_studio_pigment::adjust_math::{
    AdjustError, AdjustmentKind, AdjustmentOp, BrightnessContrast, Curves, Exposure, HueSaturation,
    Levels, LevelsRecord, composite_adjustment,
};
use hsk_studio_pigment::blend_math::{Exactness, PixelSite, Rgba, StudioBlendMode, composite_over};
use hsk_studio_pigment::coverage_math::{
    BlendChannel, BlendRangeEntry, CoverageError, IDENTITY_RANGE, blend_if_weight, feather_kernel,
    feather_sigma, mask_coverage, range_weight,
};

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-5
}
fn close3(a: [f32; 3], b: [f32; 3]) -> bool {
    (0..3).all(|i| close(a[i], b[i]))
}

#[test]
fn mask_density_invert_feather_and_blend_if() {
    // 1 - 0.5 * (1 - 0.25) = 0.625; inverted m' = 0.75 -> 1 - 0.5 * 0.25 = 0.875.
    assert!(close(mask_coverage(0.25, false, 0.5).unwrap(), 0.625));
    assert!(close(mask_coverage(0.25, true, 0.5).unwrap(), 0.875));
    assert!(close(mask_coverage(0.25, false, 1.0).unwrap(), 0.25));
    assert!(close(mask_coverage(0.25, false, 0.0).unwrap(), 1.0));
    assert_eq!(mask_coverage(1.5, false, 1.0), Err(CoverageError::OutOfDomain));

    // Feather radius 2 -> sigma 1, support 3, centre tap 1 / (1 + 2 (e^-0.5 + e^-2 + e^-4.5))
    // = 1 / 2.5059499 = 0.3990503.
    assert_eq!(feather_kernel(0.0).unwrap(), vec![1.0]);
    let k = feather_kernel(2.0).unwrap();
    assert_eq!(k.len(), 7);
    assert!(close(k[3], 0.399_050_3));
    assert!(close(k.iter().sum::<f32>(), 1.0));
    assert_eq!(feather_sigma(10_001.0), Err(CoverageError::FeatherOutOfRange));

    // Range [0.2, 0.4, 0.6, 1.0]: 0.1 -> 0; 0.3 -> (0.3-0.2)/0.2 = 0.5; 0.5 -> 1;
    // 0.8 -> (1.0-0.8)/0.4 = 0.5. Collapsed black pair 0.5: 0.49 -> 0, 0.5 -> 1.
    let r = [0.2, 0.4, 0.6, 1.0];
    assert!(close(range_weight(0.1, r).unwrap(), 0.0));
    assert!(close(range_weight(0.3, r).unwrap(), 0.5));
    assert!(close(range_weight(0.5, r).unwrap(), 1.0));
    assert!(close(range_weight(0.8, r).unwrap(), 0.5));
    assert!(close(range_weight(0.49, [0.5, 0.5, 1.0, 1.0]).unwrap(), 0.0));
    assert!(close(range_weight(0.5, [0.5, 0.5, 1.0, 1.0]).unwrap(), 1.0));
    assert!(close(range_weight(1.0, IDENTITY_RANGE).unwrap(), 1.0));
    assert_eq!(range_weight(0.5, [0.4, 0.3, 1.0, 1.0]), Err(CoverageError::RangeOrder));

    // Gray source range on lum([0.3; 3]) = 0.3 -> 0.5; red destination [0, 0, 0.5, 0.5]:
    // backdrop red 0.4 -> 1 (product 0.5), backdrop red 0.6 -> 0 (product 0).
    let entries = [
        BlendRangeEntry {
            channel: BlendChannel::Gray,
            source: r,
            destination: IDENTITY_RANGE,
        },
        BlendRangeEntry {
            channel: BlendChannel::Red,
            source: IDENTITY_RANGE,
            destination: [0.0, 0.0, 0.5, 0.5],
        },
    ];
    assert!(close(blend_if_weight(&entries, [0.3; 3], [0.4, 0.9, 0.1]).unwrap(), 0.5));
    assert!(close(blend_if_weight(&entries, [0.3; 3], [0.6, 0.9, 0.1]).unwrap(), 0.0));
    assert!(close(blend_if_weight(&[], [0.3; 3], [0.6, 0.9, 0.1]).unwrap(), 1.0));
}

#[test]
fn levels_curves_and_point_adjustments() {
    // Levels (0.2, 0.8, gamma 2, 0.1, 0.9) at 0.5: v = 0.5, sqrt = 0.70710678,
    // 0.1 + 0.70710678 * 0.8 = 0.66568542.
    let rec = LevelsRecord {
        input_black: 0.2,
        input_white: 0.8,
        gamma: 2.0,
        output_black: 0.1,
        output_white: 0.9,
    };
    let id = LevelsRecord::IDENTITY;
    let lv = |composite, red| AdjustmentOp::Levels(Levels { composite, red, green: id, blue: id });
    assert!(close3(lv(rec, id).apply([0.5, 0.1, 0.9]).unwrap(), [0.665_685_4, 0.1, 0.9]));
    // Channel record first, then composite: red record (0, 0.5) maps 0.1 -> 0.2; the inverting
    // composite (output 1 -> 0) gives 0.8; green 0.1 -> 0.9. Composite-first would give red 1.0.
    let invert_out = LevelsRecord {
        output_black: 1.0,
        output_white: 0.0,
        ..id
    };
    let red_half = LevelsRecord {
        input_white: 0.5,
        ..id
    };
    assert!(close3(lv(invert_out, red_half).apply([0.1, 0.1, 0.1]).unwrap(), [0.8, 0.9, 0.9]));
    let bad = LevelsRecord { gamma: 0.0, ..id };
    assert_eq!(
        lv(bad, id).apply([0.5; 3]),
        Err(AdjustError::InvalidParameter("levels.gamma"))
    );

    // Curves: two points (0,1),(1,0) are linear: 0.25 -> 0.75. Natural spline through
    // (0,0),(0.5,0.75),(1,1): interior second derivative m1 = -3 (6 * (0.5 - 1.5) / 2); at 0.25,
    // a = b = 0.5: 0.375 + (0.125 - 0.5) * (-3) * 0.25 / 6 = 0.421875; at a knot 0.5 -> 0.75.
    let cv = |composite: Vec<[f32; 2]>| {
        AdjustmentOp::Curves(Curves { composite, red: vec![], green: vec![], blue: vec![] })
    };
    assert!(close3(cv(vec![[0.0, 1.0], [1.0, 0.0]]).apply([0.25; 3]).unwrap(), [0.75; 3]));
    let s = cv(vec![[0.0, 0.0], [0.5, 0.75], [1.0, 1.0]]);
    assert!(close3(s.apply([0.25, 0.5, 1.0]).unwrap(), [0.421_875, 0.75, 1.0]));
    assert_eq!(
        cv(vec![[0.5, 0.0], [0.5, 1.0]]).apply([0.2; 3]),
        Err(AdjustError::InvalidParameter("curves.points"))
    );

    // Exposure +1 EV: 0.25 -> 0.5. Offset -0.1, gamma 2: sqrt(0.4) = 0.63245553.
    let ex = |exposure, offset, gamma| AdjustmentOp::Exposure(Exposure { exposure, offset, gamma });
    assert!(close3(ex(1.0, 0.0, 1.0).apply([0.25; 3]).unwrap(), [0.5; 3]));
    assert!(close3(ex(0.0, -0.1, 2.0).apply([0.5; 3]).unwrap(), [0.632_455_5; 3]));

    // Invert: 0.25 -> 0.75; 8-bit 5 -> 250 (Adobe help example).
    let inv = AdjustmentOp::Invert.apply([0.25, 5.0 / 255.0, 1.0]).unwrap();
    assert!(close3(inv, [0.75, 250.0 / 255.0, 0.0]));
    assert_eq!(AdjustmentKind::Invert.facts().exactness, Exactness::Exact);

    // Threshold 0.5: lum([0.6;3]) = 0.6 -> white; lum([1,0,0]) = 0.3 -> black.
    let th = AdjustmentOp::Threshold { level: 0.5 };
    assert_eq!(th.apply([0.6; 3]).unwrap(), [1.0; 3]);
    assert_eq!(th.apply([1.0, 0.0, 0.0]).unwrap(), [0.0; 3]);

    // Posterize 4 (n - 1 = 3): 0.4 -> round(1.2) / 3 = 1/3; 0.5 -> round(1.5) / 3 = 2/3.
    let po = AdjustmentOp::Posterize { levels: 4 };
    assert!(close3(po.apply([0.4, 0.5, 1.0]).unwrap(), [1.0 / 3.0, 2.0 / 3.0, 1.0]));
    assert_eq!(
        AdjustmentOp::Posterize { levels: 1 }.apply([0.5; 3]),
        Err(AdjustError::InvalidParameter("posterize.levels"))
    );
}

#[test]
fn hue_saturation_master_and_refusals() {
    let hs = |hue, saturation, lightness| {
        AdjustmentOp::HueSaturation(HueSaturation { hue, saturation, lightness, colorize: false })
    };
    // Red (h 0, s 1, l 0.5) rotated +120 deg is green; saturation -100 is gray 0.5; lightness
    // +100 is white; lightness -50: l 0.25, m2 0.5, m1 0 -> [0.5, 0, 0].
    assert!(close3(hs(120.0, 0.0, 0.0).apply([1.0, 0.0, 0.0]).unwrap(), [0.0, 1.0, 0.0]));
    assert!(close3(hs(0.0, -100.0, 0.0).apply([1.0, 0.0, 0.0]).unwrap(), [0.5; 3]));
    assert!(close3(hs(0.0, 0.0, 100.0).apply([1.0, 0.0, 0.0]).unwrap(), [1.0; 3]));
    assert!(close3(hs(0.0, 0.0, -50.0).apply([1.0, 0.0, 0.0]).unwrap(), [0.5, 0.0, 0.0]));
    let colorize = AdjustmentOp::HueSaturation(HueSaturation {
        hue: 0.0,
        saturation: 0.0,
        lightness: 0.0,
        colorize: true,
    });
    assert_eq!(colorize.apply([0.5; 3]), Err(AdjustError::Unsupported("hue_saturation.colorize")));
    let bc = AdjustmentOp::BrightnessContrast(BrightnessContrast {
        brightness: 500.0,
        contrast: 0.0,
        use_legacy: false,
    });
    assert_eq!(
        bc.apply([0.5; 3]),
        Err(AdjustError::InvalidParameter("brightness_contrast.brightness"))
    );
    assert_eq!(AdjustmentOp::Invert.apply([1.5, 0.0, 0.0]), Err(AdjustError::OutOfDomain));
}

#[test]
fn adjustment_layer_rule() {
    // Invert over (0.2, 0.4, 0.6) alpha 0.5, Normal, coverage 0.5: f = (0.8, 0.6, 0.4),
    // C = Cb + (f - Cb) * 0.5 = (0.5, 0.5, 0.5), alpha stays 0.5.
    let inv = AdjustmentOp::Invert.prepare().unwrap();
    let b = Rgba::new([0.2, 0.4, 0.6], 0.5);
    let out = composite_adjustment(StudioBlendMode::Normal, b, &inv, 0.5).unwrap();
    assert!(close3(out.rgb, [0.5; 3]) && close(out.a, 0.5));
    // Multiply, coverage 1: Cb * f = (0.16, 0.24, 0.24), alpha 0.5.
    let out = composite_adjustment(StudioBlendMode::Multiply, b, &inv, 1.0).unwrap();
    assert!(close3(out.rgb, [0.16, 0.24, 0.24]) && close(out.a, 0.5));
    // Transparent backdrop stays transparent.
    let t = composite_adjustment(StudioBlendMode::Normal, Rgba::TRANSPARENT, &inv, 1.0).unwrap();
    assert_eq!(t, Rgba::TRANSPARENT);
    // On an opaque backdrop the rule equals W3C source-over of f(Cb) at alpha k (blend_math).
    let opaque = Rgba::new([0.2, 0.4, 0.6], 1.0);
    let k = 0.3;
    let rule = composite_adjustment(StudioBlendMode::Multiply, opaque, &inv, k).unwrap();
    let f = inv.apply(opaque.rgb).unwrap();
    let over = composite_over(
        StudioBlendMode::Multiply,
        opaque,
        Rgba::new(f, 1.0),
        k,
        PixelSite::default(),
    )
    .unwrap();
    assert!(close3(rule.rgb, over.rgb) && close(rule.a, over.a));
}
