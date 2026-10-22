//! Lane SMB-PS-ADJUST-STYLES-2: bevel chisel hard / chisel soft, soften and stroke emboss
//! (`effect_math::style`). Hand values are derived in the comments from the formulas documented in
//! `style.rs`; soften is checked bit for bit against an independent re-computation of its reading
//! (separable Gaussian of the part matte, binary64 accumulation, zero beyond the plane); the
//! windowed evaluator is checked bit for bit against the reference. Photoshop parity UNVERIFIED (D5).
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::StudioBlendMode;
use hsk_studio_pigment::effect_math::window::{
    AlphaPlaneRef, WindowBudget, WindowError, WindowGeometry, WindowLimits, window_cost, window_matte,
};
use hsk_studio_pigment::effect_math::{
    BevelEmbossOp, BevelPart, BevelShape, BevelStroke, BevelStyle, BevelTechnique, EffectError, EffectOp,
    EffectPlane, StrokePosition, effect_support_px, reference_matte,
};

const WHITE: [f32; 3] = [1.0; 3];
const LIMITS: WindowLimits = WindowLimits::new(256 << 20, 4_000_000_000);

/// 40 x 40, the square 10..30 x 10..30 opaque.
fn square() -> EffectPlane {
    let alpha = (0..1600).map(|i| if (10..30).contains(&(i % 40)) && (10..30).contains(&(i / 40)) { 1.0 } else { 0.0 }).collect();
    EffectPlane::new(40, 40, vec![[0.5; 3]; 1600], alpha).unwrap()
}
fn op(part: BevelPart, shape: BevelShape) -> EffectOp {
    EffectOp::BevelEmboss(BevelEmbossOp::new(part, WHITE, StudioBlendMode::Normal, 1.0, shape))
}
fn chisel(style: BevelStyle, technique: BevelTechnique) -> BevelShape {
    BevelShape { style, technique, size_px: 4.0, depth: 100.0, angle_deg: 0.0, altitude_deg: 30.0, ..BevelShape::DEFAULT }
}

#[test]
fn chisel_hard_hand_values() {
    // Size 4, depth 100: field s / 4 with s = d - 0.5 capped at 4 (search radius ceil(4.5) = 5).
    // Row 20, right band of the square: d_in(26) = 4, d_in(27) = 3, d_in(28) = 2 (the outside column
    // 30), so the field reads 0.875, 0.625, 0.375 and gx(27) = (0.375 - 0.875) / 2 = -0.25; the rows
    // 19 and 21 read the same, gy = 0. Height scale k = depth / 100 x size = 4 -> N = (1, 0, 1) /
    // sqrt 2. Light angle 0 at altitude 30: L = (0.8660254, 0, 0.5); s = 1.3660254 / 1.4142136 =
    // 0.9659258, flat 0.5 -> highlight (s - 0.5) / 0.5 = 0.9318517. Mirror at x = 12 (left band):
    // N = (-1, 0, 1) / sqrt 2, s = -0.2588190 -> shadow (0.5 + 0.2588190) / 0.5 > 1 -> 1. The centre
    // (20, 20) is beyond the search radius: field 1 everywhere around it, flat -> 0.
    let p = square();
    let hi = reference_matte(&p, &op(BevelPart::Highlight, chisel(BevelStyle::InnerBevel, BevelTechnique::ChiselHard))).unwrap();
    let sh = reference_matte(&p, &op(BevelPart::Shadow, chisel(BevelStyle::InnerBevel, BevelTechnique::ChiselHard))).unwrap();
    let at = |m: &[f32], x: usize, y: usize| m[y * 40 + x];
    assert!((at(&hi, 27, 20) - 0.931_851_7).abs() < 1e-6, "{}", at(&hi, 27, 20));
    // the chisel slope is constant across the band: x = 28 sees the same gradient (d 3, 2, 1 ->
    // 0.625, 0.375, 0.125); x = 26 reaches the cap (d 5 -> s 4.5 capped at 4: gx = -0.1875)
    assert!((at(&hi, 28, 20) - 0.931_851_7).abs() < 1e-6, "{}", at(&hi, 28, 20));
    assert!(at(&hi, 26, 20) < at(&hi, 27, 20));
    assert_eq!(at(&sh, 27, 20), 0.0);
    assert_eq!(at(&sh, 12, 20), 1.0);
    assert_eq!((at(&hi, 20, 20), at(&sh, 20, 20)), (0.0, 0.0));
    // Outer bevel: outside the square the field is -min(d_out - 0.5, 4) / 4; at x = 32, d_out(31,
    // 32, 33) = 2, 3, 4 -> -0.375, -0.625, -0.875, gx = -0.25: the same normal, times 1 - alpha = 1.
    let outer = reference_matte(&p, &op(BevelPart::Highlight, chisel(BevelStyle::OuterBevel, BevelTechnique::ChiselHard))).unwrap();
    assert!((at(&outer, 32, 20) - 0.931_851_7).abs() < 1e-6, "{}", at(&outer, 32, 20));
    assert_eq!(at(&outer, 27, 20), 0.0, "outer bevel is drawn outside only");
    // Smooth reads a curved profile: its highlight differs from the chisel's constant slope.
    let smooth = reference_matte(&p, &op(BevelPart::Highlight, chisel(BevelStyle::InnerBevel, BevelTechnique::Smooth))).unwrap();
    assert!((at(&smooth, 27, 20) - at(&smooth, 26, 20)).abs() > 1e-3);
    // Chisel soft: the hard field blurred by size / 4 = 1 px; it differs from hard near the corners
    // of the ramp and is flat in the centre.
    let soft = reference_matte(&p, &op(BevelPart::Highlight, chisel(BevelStyle::InnerBevel, BevelTechnique::ChiselSoft))).unwrap();
    assert!(soft.iter().zip(&hi).any(|(a, b)| (a - b).abs() > 1e-3));
    assert_eq!(at(&soft, 20, 20), 0.0);
    // Support: chisel hard = ceil(4.5) + 1 = 6; soft adds the size-1 blur support (sigma 0.5,
    // ceil(1.5) = 2) = 8.
    assert_eq!(effect_support_px(&op(BevelPart::Highlight, chisel(BevelStyle::InnerBevel, BevelTechnique::ChiselHard))), Ok(6));
    assert_eq!(effect_support_px(&op(BevelPart::Highlight, chisel(BevelStyle::InnerBevel, BevelTechnique::ChiselSoft))), Ok(8));
}

#[test]
fn soften_is_the_blurred_part_matte_and_stroke_emboss_follows_the_stroke() {
    let p = square();
    let base = chisel(BevelStyle::Emboss, BevelTechnique::Smooth);
    let plain = reference_matte(&p, &op(BevelPart::Highlight, base)).unwrap();
    let softened = reference_matte(&p, &op(BevelPart::Highlight, BevelShape { soften_px: 3.0, ..base })).unwrap();
    // independent re-computation: separable effect tent kernel (effect_blur_kernel(3); the soften
    // blur follows the oracle-measured shadow/glow kernel since lane SMB-PS-ORACLE-PARITY-1, a
    // cross-lane re-pin, for soften UNVERIFIED), binary64 accumulation, zero beyond the plane,
    // horizontal then vertical, then clamped
    let taps = hsk_studio_pigment::effect_math::effect_blur_kernel(3.0).unwrap();
    let half = (taps.len() / 2) as i64;
    let pass = |src: &[f32], horizontal: bool| -> Vec<f32> {
        let mut out = vec![0.0f32; 1600];
        for y in 0..40i64 {
            for x in 0..40i64 {
                let mut acc = 0.0f64;
                for (i, t) in taps.iter().enumerate() {
                    let o = i as i64 - half;
                    let (sx, sy) = if horizontal { (x + o, y) } else { (x, y + o) };
                    let v = if (0..40).contains(&sx) && (0..40).contains(&sy) { src[(sy * 40 + sx) as usize] } else { 0.0 };
                    acc += f64::from(*t) * f64::from(v);
                }
                out[(y * 40 + x) as usize] = acc as f32;
            }
        }
        out
    };
    let want: Vec<f32> = pass(&pass(&plain, true), false).iter().map(|v| v.clamp(0.0, 1.0)).collect();
    assert_eq!(softened.iter().map(|v| v.to_bits()).collect::<Vec<_>>(), want.iter().map(|v| v.to_bits()).collect::<Vec<_>>());
    assert_ne!(softened, plain);

    // Stroke emboss without a stroke: nothing to emboss.
    let no_stroke = BevelShape { style: BevelStyle::StrokeEmboss, ..base };
    assert!(reference_matte(&p, &op(BevelPart::Highlight, no_stroke)).unwrap().iter().all(|v| *v == 0.0));
    // With an outside stroke of 3 px: drawn only where the stroke matte is (outside the square,
    // within 3.5 px of it), and some of the ring is lit.
    let stroked = BevelShape { stroke: Some(BevelStroke { size_px: 3.0, position: StrokePosition::Outside }), ..no_stroke };
    let m = reference_matte(&p, &op(BevelPart::Highlight, stroked)).unwrap();
    for y in 0..40usize {
        for x in 0..40usize {
            let inside = (10..30).contains(&x) && (10..30).contains(&y);
            let far = !(6..=33).contains(&x) || !(6..=33).contains(&y);
            if inside || far {
                assert_eq!(m[y * 40 + x], 0.0, "({x}, {y})");
            }
        }
    }
    assert!(m.iter().any(|v| *v > 0.1));
    // The emboss of the union shape: a centre stroke straddles the edge, so it is lit inside too.
    let centre = BevelShape { stroke: Some(BevelStroke { size_px: 4.0, position: StrokePosition::Centre }), ..no_stroke };
    let c = reference_matte(&p, &op(BevelPart::Highlight, centre)).unwrap();
    assert!((10..30).any(|x| c[20 * 40 + x] > 0.0));
}

#[test]
fn windows_are_bit_identical_for_the_new_techniques() {
    // a blob with soft edges inside a 40 x 30 layer
    let alpha: Vec<f32> = (0..1200)
        .map(|i| {
            let (x, y) = ((i % 40) as f32, (i / 40) as f32);
            let d = ((x - 19.5).powi(2) / 120.0 + (y - 14.0).powi(2) / 50.0).sqrt();
            (1.6 - d).clamp(0.0, 1.0)
        })
        .collect();
    let layer = Rect { x: 5, y: -3, width: 40, height: 30 };
    let reference = EffectPlane::new(40, 30, vec![[0.5; 3]; 1200], alpha.clone()).unwrap();
    let src = AlphaPlaneRef::new(layer, &alpha).unwrap();
    let mut shapes = Vec::new();
    for technique in [BevelTechnique::ChiselHard, BevelTechnique::ChiselSoft] {
        for style in [BevelStyle::InnerBevel, BevelStyle::OuterBevel, BevelStyle::Emboss, BevelStyle::PillowEmboss] {
            shapes.push(BevelShape { style, technique, size_px: 3.5, depth: 300.0, angle_deg: 120.0, altitude_deg: 35.0, ..BevelShape::DEFAULT });
        }
    }
    for position in [StrokePosition::Outside, StrokePosition::Inside, StrokePosition::Centre] {
        shapes.push(BevelShape { style: BevelStyle::StrokeEmboss, stroke: Some(BevelStroke { size_px: 2.5, position }), soften_px: 1.5, ..BevelShape::DEFAULT });
    }
    shapes.push(BevelShape { technique: BevelTechnique::ChiselHard, soften_px: 2.0, size_px: 2.0, ..BevelShape::DEFAULT });
    let windows = [layer, Rect { x: 12, y: 4, width: 9, height: 7 }, Rect { x: 38, y: 20, width: 7, height: 7 }];
    for shape in shapes {
        for part in [BevelPart::Highlight, BevelPart::Shadow] {
            let e = op(part, shape);
            let full = reference_matte(&reference, &e).unwrap();
            assert!(full.iter().any(|v| *v > 0.0), "{e:?} draws something");
            for w in windows {
                let got = window_matte(&src, &WindowGeometry::plane(layer, w), &e, LIMITS).unwrap();
                for (i, v) in got.coverage.iter().enumerate() {
                    let (wx, wy) = (i as i64 % i64::from(w.width), i as i64 / i64::from(w.width));
                    let (lx, ly) = (w.x + wx - layer.x, w.y + wy - layer.y);
                    assert_eq!(v.to_bits(), full[(ly * 40 + lx) as usize].to_bits(), "{e:?} window {w:?} at ({lx}, {ly})");
                }
                let cost = window_cost(&e, w).unwrap();
                assert!(got.receipt.work <= cost.work && got.receipt.peak_bytes <= cost.peak_bytes, "{:?} vs {cost:?}", got.receipt);
            }
        }
    }
}

/// Hostile sizes: the reference refuses a chisel search beyond its work budget before any pass, a
/// non-finite stroke or soften is a domain error, and the windowed evaluator refuses a work limit
/// below the chisel prediction before reading any alpha.
#[test]
fn chisel_and_stroke_budgets_refuse_typed() {
    let p = EffectPlane::transparent(64, 64).unwrap();
    // size 1000 (the ceiling): search radius 1001 -> 4096 x 2003^2 visits > 1e8
    let huge = op(BevelPart::Highlight, BevelShape { technique: BevelTechnique::ChiselHard, size_px: 1000.0, ..BevelShape::DEFAULT });
    assert_eq!(reference_matte(&p, &huge), Err(EffectError::OverBudget));
    let nan_stroke = BevelShape { style: BevelStyle::StrokeEmboss, stroke: Some(BevelStroke { size_px: f32::NAN, position: StrokePosition::Outside }), ..BevelShape::DEFAULT };
    assert_eq!(op(BevelPart::Highlight, nan_stroke).validate(), Err(EffectError::OutOfDomain));
    let big_soften = BevelShape { soften_px: 1e9, ..BevelShape::DEFAULT };
    assert_eq!(op(BevelPart::Highlight, big_soften).validate(), Err(EffectError::SizeOutOfRange));
    let alpha = vec![1.0f32; 400];
    let layer = Rect { x: 0, y: 0, width: 20, height: 20 };
    let read = std::cell::Cell::new(0usize);
    let src = AlphaPlaneRef::new(layer, &alpha).unwrap();
    let counting = |r: Rect, out: &mut [f32]| {
        read.set(read.get() + 1);
        hsk_studio_pigment::effect_math::window::AlphaSource::read_alpha(&src, r, out)
    };
    let e = op(BevelPart::Shadow, BevelShape { technique: BevelTechnique::ChiselHard, size_px: 6.0, ..BevelShape::DEFAULT });
    let cost = window_cost(&e, layer).unwrap();
    let err = window_matte(&counting, &WindowGeometry::plane(layer, layer), &e, WindowLimits::new(u64::MAX, cost.work - 1)).unwrap_err();
    assert!(matches!(err, WindowError::OverBudget { budget: WindowBudget::Work, .. }), "{err:?}");
    assert_eq!(read.get(), 0, "refused before any read");
}
