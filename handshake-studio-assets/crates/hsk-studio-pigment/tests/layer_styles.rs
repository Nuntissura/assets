//! Lane SMB-PS-ADJUST-STYLES slice 2: satin, bevel & emboss (smooth) and gradient overlay
//! (`effect_math::style`). Hand values are derived in the comments from the formulas documented in
//! `style.rs`; the windowed evaluator is checked bit for bit against the reference. Photoshop
//! parity is UNVERIFIED (D5).
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::StudioBlendMode;
use hsk_studio_pigment::effect_math::window::{
    AlphaPlaneRef, AlphaSource, WindowBudget, WindowError, WindowGeometry, WindowLimits, WindowPlane,
    layer_window_cost, render_layer_window, window_cost, window_matte,
};
use hsk_studio_pigment::effect_math::{
    BevelEmbossOp, BevelPart, BevelShape, BevelStyle, BevelTechnique, EffectError,
    EffectFeature, EffectKind, EffectOp, EffectPlane, GradientOverlayOp, GradientStopOp,
    GradientStyle, LayerParams, SatinOp, effect_support_px, reference_matte,
    render_layer_reference, render_reference,
};

const NORMAL: StudioBlendMode = StudioBlendMode::Normal;
const WHITE: [f32; 3] = [1.0; 3];
const LIMITS: WindowLimits = WindowLimits::new(256 << 20, 4_000_000_000);

fn plane(w: u32, h: u32, alpha: Vec<f32>) -> EffectPlane {
    let n = alpha.len();
    EffectPlane::new(w, h, vec![[0.2, 0.4, 0.6]; n], alpha).unwrap()
}
/// 12 wide x 10 high, columns 0..=6 opaque. Rows 4..=6 and columns 2..=11 around the probes see
/// the full kernel inside the plane, so the vertical gradient at row 5 is 0 and no plane edge
/// enters the hand values.
fn half_plane() -> EffectPlane {
    let alpha = (0..120).map(|i| if i % 12 <= 6 { 1.0 } else { 0.0 }).collect();
    plane(12, 10, alpha)
}
fn bevel(part: BevelPart, style: BevelStyle, angle: f32) -> EffectOp {
    EffectOp::BevelEmboss(BevelEmbossOp::new(
        part,
        WHITE,
        NORMAL,
        1.0,
        BevelShape {
            style,
            size_px: 2.0,
            angle_deg: angle,
            altitude_deg: 30.0,
            ..BevelShape::DEFAULT
        },
    ))
}
fn at(m: &[f32], x: usize, y: usize, w: usize) -> f32 {
    m[y * w + x]
}

#[test]
fn bevel_hand_values() {
    // Size 2 -> sigma 1 kernel (taps k0 0.3990503, k1 0.2420362, k2 0.0540056, k3 0.0044330).
    // Row 5 of the blurred alpha: H(5) = 1 - k2 - k3 = 0.9415614, H(7) = k1 + k2 + k3 =
    // 0.3004749, so dH/dx at x = 6 is (0.3004749 - 0.9415614) / 2 = -0.3205433; height scale
    // depth 100 % x size 2 = 2 -> N = (0.6410865, 0, 1) / 1.1878518. Light angle 0 at altitude 30:
    // L = (0.8660254, 0, 0.5), s = (0.5551975 + 0.5) / 1.1878518 = 0.8883239, flat s0 = 0.5,
    // highlight (s - s0) / (1 - s0) = 0.7766479. Light from 180: s = -0.0464681, shadow
    // (0.5 + 0.0464681) / 0.5 > 1 -> 1.
    let p = half_plane();
    let hi = reference_matte(&p, &bevel(BevelPart::Highlight, BevelStyle::InnerBevel, 0.0)).unwrap();
    assert!((at(&hi, 6, 5, 12) - 0.776_647_9).abs() < 2e-5, "{}", at(&hi, 6, 5, 12));
    let sh = reference_matte(&p, &bevel(BevelPart::Shadow, BevelStyle::InnerBevel, 0.0)).unwrap();
    assert_eq!(at(&sh, 6, 5, 12), 0.0, "lit side has no shadow");
    let sh = reference_matte(&p, &bevel(BevelPart::Shadow, BevelStyle::InnerBevel, 180.0)).unwrap();
    assert_eq!(at(&sh, 6, 5, 12), 1.0);
    // At x = 7 the slope is the same (H(8) - H(6) = 0.0584386 - 0.6995251, /2 = -0.3205433):
    // an inner bevel draws nothing outside the shape, an outer bevel and an emboss draw 0.7766.
    assert_eq!(at(&hi, 7, 5, 12), 0.0);
    for style in [BevelStyle::OuterBevel, BevelStyle::Emboss] {
        let m = reference_matte(&p, &bevel(BevelPart::Highlight, style, 0.0)).unwrap();
        assert!((at(&m, 7, 5, 12) - 0.776_647_9).abs() < 2e-5, "{style:?}");
    }
    // Pillow emboss outside the shape uses the negated height: lit side becomes shadow.
    let m = reference_matte(&p, &bevel(BevelPart::Highlight, BevelStyle::PillowEmboss, 0.0)).unwrap();
    assert_eq!(at(&m, 7, 5, 12), 0.0);
    // Direction down flips the lighting.
    let down = EffectOp::BevelEmboss(BevelEmbossOp::new(
        BevelPart::Shadow,
        WHITE,
        NORMAL,
        1.0,
        BevelShape {
            up: false,
            size_px: 2.0,
            angle_deg: 0.0,
            altitude_deg: 30.0,
            ..BevelShape::DEFAULT
        },
    ));
    let m = reference_matte(&p, &down).unwrap();
    assert_eq!(at(&m, 6, 5, 12), 1.0);
}

#[test]
fn satin_and_gradient_overlay_hand_values() {
    // Satin size 0 (no blur), distance 1, angle 0: offset (-1, 0), so m(x) = |a(x + 1) - a(x - 1)|
    // x a(x). Row alpha 0 0 1 1 1 1 0 0 -> 0 0 1 0 0 1 0 0; inverted (1 - m) x a -> 0 0 0 1 1 0 0 0.
    let row = plane(8, 1, vec![0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0]);
    let satin = |invert| EffectOp::Satin(SatinOp::new(WHITE, NORMAL, 1.0, 0.0, 1.0, 0.0, invert));
    assert_eq!(reference_matte(&row, &satin(false)).unwrap(), vec![0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    assert_eq!(reference_matte(&row, &satin(true)).unwrap(), vec![0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0]);

    // Gradient overlay, linear 0 deg, black -> white over a 4 x 1 opaque layer: projection length
    // 4 x |cos 0| + 1 x |sin 0| = 4, pixel centres -1.5, -0.5, 0.5, 1.5 from the centre ->
    // t = 0.125, 0.375, 0.625, 0.875; at opacity 1 over the content the result is the gradient.
    let stop = |position, c: f32, opacity| GradientStopOp { position, colour: [c; 3], opacity };
    let solid = plane(4, 1, vec![1.0; 4]);
    let lin = GradientOverlayOp::new(NORMAL, 1.0, GradientStyle::Linear, 0.0, &[stop(0.0, 0.0, 1.0), stop(1.0, 1.0, 1.0)]).unwrap();
    let out = render_reference(&solid, &EffectOp::GradientOverlay(lin)).unwrap();
    let got: Vec<f32> = (0..4).map(|x| out.pixel(x, 0).unwrap().rgb[0]).collect();
    assert_eq!(got, vec![0.125, 0.375, 0.625, 0.875]);
    // Reverse: 1 - t.
    let out = render_reference(&solid, &EffectOp::GradientOverlay(lin.with_reverse(true))).unwrap();
    assert_eq!(out.pixel(0, 0).unwrap().rgb[0], 0.875);
    // Stop opacity 0 -> 1 becomes the matte: alpha x t.
    let fade = GradientOverlayOp::new(NORMAL, 1.0, GradientStyle::Linear, 0.0, &[stop(0.0, 1.0, 0.0), stop(1.0, 1.0, 1.0)]).unwrap();
    assert_eq!(reference_matte(&solid, &EffectOp::GradientOverlay(fade)).unwrap(), vec![0.125, 0.375, 0.625, 0.875]);
    // Reflected: |t - 0.5| x 2 -> 0.75, 0.25, 0.25, 0.75. Radial on 4 x 1: radius 0.5 x 4 = 2,
    // distances 1.5, 0.5, 0.5, 1.5 -> 0.75, 0.25, 0.25, 0.75 too.
    for style in [GradientStyle::Reflected, GradientStyle::Radial] {
        let g = GradientOverlayOp::new(NORMAL, 1.0, style, 0.0, &[stop(0.0, 0.0, 1.0), stop(1.0, 1.0, 1.0)]).unwrap();
        let out = render_reference(&solid, &EffectOp::GradientOverlay(g)).unwrap();
        let got: Vec<f32> = (0..4).map(|x| out.pixel(x, 0).unwrap().rgb[0]).collect();
        assert_eq!(got, vec![0.75, 0.25, 0.25, 0.75], "{style:?}");
    }
}

/// A 40 x 30 alpha with an opaque block, a soft disc and scattered partial samples.
fn field() -> Vec<f32> {
    let (w, h) = (40u32, 30u32);
    let mut v = Vec::new();
    let mut s = 0x9e37_79b9_7f4a_7c15u64;
    for y in 0..h {
        for x in 0..w {
            let mut a: f32 = if (10..30).contains(&x) && (8..22).contains(&y) { 1.0 } else { 0.0 };
            let d = ((x as f32 - 32.0).powi(2) + (y as f32 - 9.0).powi(2)).sqrt();
            if d < 6.0 {
                a = a.max(1.0 - d / 6.0);
            }
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            if s % 19 == 0 {
                a = a.max((s >> 40) as f32 / 16_777_216.0);
            }
            v.push(a);
        }
    }
    v
}

#[test]
fn windows_are_bit_identical_to_the_reference_and_charged_up_front() {
    let alpha = field();
    let layer = Rect { x: 5, y: -3, width: 40, height: 30 };
    let reference = plane(40, 30, alpha.clone());
    let src = AlphaPlaneRef::new(layer, &alpha).unwrap();
    let stops = [
        GradientStopOp { position: 0.1, colour: [0.9, 0.1, 0.2], opacity: 1.0 },
        GradientStopOp { position: 0.6, colour: [0.1, 0.8, 0.3], opacity: 0.4 },
        GradientStopOp { position: 1.0, colour: [0.2, 0.3, 1.0], opacity: 0.9 },
    ];
    let mut ops = vec![
        EffectOp::Satin(SatinOp::new([0.1, 0.2, 0.3], StudioBlendMode::Multiply, 0.6, 37.0, 4.0, 3.0, true)),
        EffectOp::Satin(SatinOp::new(WHITE, NORMAL, 1.0, -120.0, 9.0, 0.0, false)),
    ];
    for style in [BevelStyle::InnerBevel, BevelStyle::OuterBevel, BevelStyle::Emboss, BevelStyle::PillowEmboss] {
        for (part, angle) in [(BevelPart::Highlight, 120.0), (BevelPart::Shadow, -45.0)] {
            ops.push(EffectOp::BevelEmboss(BevelEmbossOp::new(
                part,
                [0.9, 0.8, 0.2],
                StudioBlendMode::Screen,
                0.75,
                BevelShape { style, size_px: 4.0, depth: 250.0, angle_deg: angle, altitude_deg: 40.0, ..BevelShape::DEFAULT },
            )));
        }
    }
    for style in [GradientStyle::Linear, GradientStyle::Radial, GradientStyle::Angle, GradientStyle::Reflected, GradientStyle::Diamond] {
        let g = GradientOverlayOp::new(StudioBlendMode::Screen, 0.8, style, 30.0, &stops).unwrap();
        ops.push(EffectOp::GradientOverlay(g.with_scale(0.7).with_offset([0.1, -0.2])));
    }
    let windows = [
        Rect { x: 5, y: -3, width: 40, height: 30 },
        Rect { x: 12, y: 4, width: 9, height: 7 },
        Rect { x: 40, y: 20, width: 5, height: 7 },
    ];
    for op in &ops {
        let full = reference_matte(&reference, op).unwrap();
        for w in windows {
            let got = window_matte(&src, &WindowGeometry::plane(layer, w), op, LIMITS).unwrap();
            let lw = layer.width as i64;
            for (i, v) in got.coverage.iter().enumerate() {
                let (wx, wy) = (i as i64 % w.width as i64, i as i64 / w.width as i64);
                let (lx, ly) = (w.x + wx - layer.x, w.y + wy - layer.y);
                let want = full[(ly * lw + lx) as usize];
                assert_eq!(v.to_bits(), want.to_bits(), "{op:?} window {w:?} at ({lx}, {ly})");
            }
            let cost = window_cost(op, w).unwrap();
            assert!(got.receipt.peak_bytes <= cost.peak_bytes && got.receipt.work <= cost.work, "{op:?}: {:?} vs {cost:?}", got.receipt);
        }
    }
    // The composed layer (backdrop, content, every style) equals the reference composition.
    let backdrop_rgb: Vec<[f32; 3]> = (0..1200).map(|i| [(i % 7) as f32 / 7.0, 0.5, (i % 5) as f32 / 5.0]).collect();
    let backdrop_alpha: Vec<f32> = (0..1200).map(|i| (i % 3) as f32 / 2.0).collect();
    let content = EffectPlane::new(40, 30, vec![[0.7, 0.3, 0.1]; 1200], alpha.clone()).unwrap();
    let back = EffectPlane::new(40, 30, backdrop_rgb.clone(), backdrop_alpha.clone()).unwrap();
    let params = LayerParams { mode: NORMAL, opacity: 0.9, fill_opacity: 0.5 };
    let stack = [ops[0], ops[2], ops[3], ops[ops.len() - 1]];
    let want = render_layer_reference(&back, &content, params, &stack).unwrap();
    let whole = Rect { x: 5, y: -3, width: 40, height: 30 };
    let got = render_layer_window(
        &src,
        &WindowGeometry::plane(layer, whole),
        &vec![[0.7, 0.3, 0.1]; 1200],
        &WindowPlane::new(whole, backdrop_rgb, backdrop_alpha).unwrap(),
        params,
        &stack,
        LIMITS,
    )
    .unwrap();
    for i in 0..1200 {
        let (x, y) = ((i % 40) as u32, (i / 40) as u32);
        let px = want.pixel(x, y).unwrap();
        assert_eq!(got.plane.rgb()[i].map(f32::to_bits), px.rgb.map(f32::to_bits), "pixel {i}");
        assert_eq!(got.plane.alpha()[i].to_bits(), px.a.to_bits());
    }
    let cost = layer_window_cost(&stack, whole).unwrap();
    assert!(got.receipt.peak_bytes <= cost.peak_bytes && got.receipt.work <= cost.work);
    // A byte budget below the prediction refuses before any alpha is read.
    let small = WindowLimits::new(cost.peak_bytes - 1, u64::MAX);
    let read = std::cell::Cell::new(0usize);
    let counting = |r: Rect, out: &mut [f32]| {
        read.set(read.get() + 1);
        src.read_alpha(r, out)
    };
    let err = render_layer_window(
        &counting,
        &WindowGeometry::plane(layer, whole),
        &vec![[0.7, 0.3, 0.1]; 1200],
        &WindowPlane::new(whole, vec![[0.0; 3]; 1200], vec![0.0; 1200]).unwrap(),
        params,
        &stack,
        small,
    )
    .unwrap_err();
    assert!(matches!(err, WindowError::OverBudget { budget: WindowBudget::Bytes, .. }), "{err:?}");
    assert_eq!(read.get(), 0, "refused before any read");
}

#[test]
fn styles_refuse_typed_and_report_their_kind() {
    let shape = |f: fn(&mut BevelShape)| {
        let mut s = BevelShape::DEFAULT;
        f(&mut s);
        EffectOp::BevelEmboss(BevelEmbossOp::new(BevelPart::Highlight, WHITE, NORMAL, 1.0, s)).validate()
    };
    assert_eq!(shape(|s| s.technique = BevelTechnique::ChiselHard), Err(EffectError::Unsupported(EffectFeature::BevelChisel)));
    assert_eq!(shape(|s| s.style = BevelStyle::StrokeEmboss), Err(EffectError::Unsupported(EffectFeature::BevelStrokeEmboss)));
    assert_eq!(shape(|s| s.soften_px = 2.0), Err(EffectError::Unsupported(EffectFeature::BevelSoften)));
    assert_eq!(shape(|s| s.size_px = f32::NAN), Err(EffectError::OutOfDomain));
    assert_eq!(shape(|s| s.size_px = 5000.0), Err(EffectError::SizeOutOfRange));
    assert_eq!(shape(|s| s.altitude_deg = 91.0), Err(EffectError::OutOfDomain));
    assert_eq!(shape(|s| s.depth = 0.5), Err(EffectError::OutOfDomain));
    assert_eq!(
        EffectOp::Satin(SatinOp::new(WHITE, NORMAL, 1.0, 0.0, 40_000.0, 3.0, false)).validate(),
        Err(EffectError::DistanceOutOfRange)
    );
    assert_eq!(
        EffectOp::Satin(SatinOp::new(WHITE, NORMAL, 1.0, 200.0, 4.0, 3.0, false)).validate(),
        Err(EffectError::AngleOutOfRange)
    );
    let stop = GradientStopOp { position: 0.0, colour: WHITE, opacity: 1.0 };
    assert_eq!(
        GradientOverlayOp::new(NORMAL, 1.0, GradientStyle::Linear, 0.0, &[stop; 17]).unwrap_err(),
        EffectError::Unsupported(EffectFeature::GradientStops)
    );
    let one = GradientOverlayOp::new(NORMAL, 1.0, GradientStyle::Linear, 0.0, &[stop]).unwrap();
    assert_eq!(EffectOp::GradientOverlay(one).validate(), Err(EffectError::OutOfDomain));
    let two = GradientOverlayOp::new(NORMAL, 1.0, GradientStyle::Linear, 0.0, &[stop, stop]).unwrap();
    assert_eq!(EffectOp::GradientOverlay(two.with_scale(2.0)).validate(), Err(EffectError::OutOfDomain));
    // Kinds, keys, placement, support.
    assert_eq!(EffectOp::GradientOverlay(two).kind(), EffectKind::GradientOverlay);
    assert_eq!(EffectKind::Satin.key(), "satin");
    assert_eq!(EffectKind::BevelEmboss.facts().exactness, hsk_studio_pigment::blend_math::Exactness::Approximate);
    // Satin support: offset round(9 x cos 60) = 5 / round(9 x sin 60) = 8 -> 8, + blur support of
    // size 3 (sigma 1.5, ceil 4.5) = 5 -> 13. Bevel: blur support of size 5 (sigma 2.5, ceil 7.5
    // = 8) + 1 for the central difference = 9. Gradient: 0.
    assert_eq!(effect_support_px(&EffectOp::Satin(SatinOp::new(WHITE, NORMAL, 1.0, 60.0, 9.0, 3.0, false))), Ok(13));
    assert_eq!(effect_support_px(&EffectOp::BevelEmboss(BevelEmbossOp::new(BevelPart::Shadow, WHITE, NORMAL, 1.0, BevelShape::DEFAULT))), Ok(9));
    assert_eq!(effect_support_px(&EffectOp::GradientOverlay(two)), Ok(0));
}
