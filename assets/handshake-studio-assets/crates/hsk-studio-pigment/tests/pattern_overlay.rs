//! Lane SMB-PS-ADJUST-STYLES-2: pattern overlay (`effect_math::style::PatternOverlayOp`): hand
//! values of the tiling (origin, phase, scale, wrap), the matte (alpha x pattern alpha), the
//! windowed evaluator bit for bit against the reference, and the typed refusals (no pixels, wrong
//! length, hostile size and scale). Photoshop parity UNVERIFIED (D5).
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::StudioBlendMode;
use hsk_studio_pigment::effect_math::window::{
    AlphaPlaneRef, WindowGeometry, WindowLimits, WindowPlane, render_layer_window_patterned_in, window_matte_with,
};
use hsk_studio_pigment::effect_math::{
    EffectError, EffectFeature, EffectKind, EffectOp, EffectPlane, LayerParams, NoPatterns, PatternOverlayOp,
    PatternPixels, PatternRef, reference_matte, reference_matte_with, render_layer_reference_with,
};
use hsk_studio_pigment::render_plan::{SampleRange, SourceKey};

const LIMITS: WindowLimits = WindowLimits::new(256 << 20, 4_000_000_000);

/// A 3 x 2 pattern: colours by index, alpha 1 except pixel (2, 1) at 0.5.
fn pattern_pixels() -> Vec<[f32; 4]> {
    (0..6).map(|i| [i as f32 / 10.0, 0.5, 1.0 - i as f32 / 10.0, if i == 5 { 0.5 } else { 1.0 }]).collect()
}
struct One(Vec<[f32; 4]>);
impl PatternPixels for One {
    fn pattern(&self, key: SourceKey) -> Option<&[[f32; 4]]> {
        (key == SourceKey(7)).then_some(self.0.as_slice())
    }
}
fn pattern_ref() -> PatternRef {
    PatternRef { key: SourceKey(7), width: 3, height: 2, colour_sha256: [1; 32], alpha_sha256: [2; 32] }
}
fn op() -> PatternOverlayOp {
    PatternOverlayOp::new(StudioBlendMode::Normal, 0.8, pattern_ref())
}

#[test]
fn tiling_hand_values_and_matte() {
    let px = pattern_pixels();
    let layer = Rect { x: 10, y: 20, width: 8, height: 8 };
    // Linked, scale 1, no phase: pixel (10 + u, 20 + v) reads (u mod 3, v mod 2).
    assert_eq!(op().sample_at(10, 20, layer, &px), Some(px[0]));
    assert_eq!(op().sample_at(12, 21, layer, &px), Some(px[5]));
    assert_eq!(op().sample_at(13, 22, layer, &px), Some(px[0]), "wraps in both directions");
    assert_eq!(op().sample_at(9, 20, layer, &px), Some(px[2]), "left of the origin wraps to the last column");
    // Unlinked: anchored at the document origin; phase (1, 0) shifts right by one pixel.
    assert_eq!(op().with_link(false).sample_at(10, 20, layer, &px), Some(px[1]));
    assert_eq!(op().with_phase([1.0, 0.0]).sample_at(10, 20, layer, &px), Some(px[2]));
    // Scale 2: each pattern pixel covers 2 x 2 (centre (11.5 - 10) / 2 = 0.75 -> column 0).
    assert_eq!(op().with_scale(2.0).sample_at(11, 20, layer, &px), Some(px[0]));
    assert_eq!(op().with_scale(2.0).sample_at(12, 20, layer, &px), Some(px[1]));
    // A pixel list of the wrong length is no sample.
    assert_eq!(op().sample_at(10, 20, layer, &px[..5]), None);

    // Matte over a 4 x 2 plane, alpha 0.5 everywhere: alpha x pattern alpha (pixel (2, 1) has
    // pattern alpha 0.5 -> 0.25).
    let plane = EffectPlane::new(4, 2, vec![[0.3; 3]; 8], vec![0.5; 8]).unwrap();
    let m = reference_matte_with(&plane, &EffectOp::PatternOverlay(op()), &One(px.clone())).unwrap();
    assert_eq!(m, vec![0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.25, 0.5]);
    // The composed reference: colour = pattern colour at alpha 0.5 x 0.8 over a transparent
    // backdrop.
    let back = EffectPlane::transparent(4, 2).unwrap();
    let out = render_layer_reference_with(&back, &plane, LayerParams { mode: StudioBlendMode::Normal, opacity: 1.0, fill_opacity: 0.0 }, &[EffectOp::PatternOverlay(op())], &One(px.clone())).unwrap();
    let p = out.pixel(1, 0).unwrap();
    assert!((p.rgb[0] - 0.1).abs() < 1e-6 && (p.a - 0.4).abs() < 1e-6, "{p:?}");
    assert_eq!(EffectKind::PatternOverlay.key(), "pattern_overlay");
    // typed refusals (one test per slice budget: run from here)
    pattern_refusals_are_typed();
}

#[test]
fn windows_are_bit_identical_to_the_reference() {
    let px = pattern_pixels();
    let alpha: Vec<f32> = (0..600).map(|i| (i % 7) as f32 / 6.0).collect();
    // The reference plane's origin is the document origin, so the layer sits at (0, 0) here (the
    // unlinked anchor would otherwise differ by the layer offset; offsets: tiling_hand_values).
    let layer = Rect { x: 0, y: 0, width: 30, height: 20 };
    let src = AlphaPlaneRef::new(layer, &alpha).unwrap();
    let reference = EffectPlane::new(30, 20, vec![[0.4, 0.2, 0.1]; 600], alpha.clone()).unwrap();
    let ops = [
        op(),
        op().with_scale(1.7).with_phase([0.4, -3.2]),
        op().with_link(false).with_scale(0.6),
    ];
    for o in ops {
        let e = EffectOp::PatternOverlay(o);
        let full = reference_matte_with(&reference, &e, &One(px.clone())).unwrap();
        for w in [layer, Rect { x: 6, y: 6, width: 7, height: 5 }] {
            let got = window_matte_with(&src, &WindowGeometry::plane(layer, w), &e, LIMITS, &One(px.clone())).unwrap();
            for (i, v) in got.coverage.iter().enumerate() {
                let (wx, wy) = (i as i64 % i64::from(w.width), i as i64 / i64::from(w.width));
                let (lx, ly) = (w.x + wx - layer.x, w.y + wy - layer.y);
                assert_eq!(v.to_bits(), full[(ly * 30 + lx) as usize].to_bits(), "{o:?} {w:?}");
            }
        }
        // The composed window equals the composed reference.
        let back_rgb: Vec<[f32; 3]> = (0..600).map(|i| [(i % 5) as f32 / 5.0, 0.3, 0.6]).collect();
        let back_alpha: Vec<f32> = vec![1.0; 600];
        let params = LayerParams { mode: StudioBlendMode::Normal, opacity: 0.9, fill_opacity: 0.6 };
        let want = render_layer_reference_with(
            &EffectPlane::new(30, 20, back_rgb.clone(), back_alpha.clone()).unwrap(),
            &reference,
            params,
            &[e],
            &One(px.clone()),
        )
        .unwrap();
        let got = render_layer_window_patterned_in(
            SampleRange::Unit,
            &src,
            &WindowGeometry::plane(layer, layer),
            &vec![[0.4, 0.2, 0.1]; 600],
            &WindowPlane::new(layer, back_rgb, back_alpha).unwrap(),
            params,
            &[e],
            LIMITS,
            &One(px.clone()),
        )
        .unwrap();
        for i in 0..600u32 {
            let p = want.pixel(i % 30, i / 30).unwrap();
            assert_eq!(got.plane.rgb()[i as usize].map(f32::to_bits), p.rgb.map(f32::to_bits));
            assert_eq!(got.plane.alpha()[i as usize].to_bits(), p.a.to_bits());
        }
    }
}

fn pattern_refusals_are_typed() {
    let plane = EffectPlane::new(2, 2, vec![[0.3; 3]; 4], vec![1.0; 4]).unwrap();
    let e = EffectOp::PatternOverlay(op());
    // no pixels: the plain reference (and every caller without a pattern source)
    assert_eq!(reference_matte(&plane, &e), Err(EffectError::Unsupported(EffectFeature::PatternUnresolved)));
    assert_eq!(reference_matte_with(&plane, &e, &NoPatterns), Err(EffectError::Unsupported(EffectFeature::PatternUnresolved)));
    // wrong length and out-of-range samples
    assert_eq!(reference_matte_with(&plane, &e, &One(pattern_pixels()[..4].to_vec())), Err(EffectError::PlaneShape));
    let mut bad = pattern_pixels();
    bad[0][0] = f32::NAN;
    assert_eq!(reference_matte_with(&plane, &e, &One(bad)), Err(EffectError::PlaneShape));
    // hostile parameters: size 0 or beyond the ceiling, scale 0 / NaN / 11, phase infinite
    let with_ref = |r: PatternRef| EffectOp::PatternOverlay(PatternOverlayOp::new(StudioBlendMode::Normal, 1.0, r)).validate();
    assert_eq!(with_ref(PatternRef { width: 0, ..pattern_ref() }), Err(EffectError::SizeOutOfRange));
    assert_eq!(with_ref(PatternRef { height: 4097, ..pattern_ref() }), Err(EffectError::SizeOutOfRange));
    for s in [0.0, f32::NAN, 11.0] {
        assert_eq!(EffectOp::PatternOverlay(op().with_scale(s)).validate(), Err(EffectError::OutOfDomain), "{s}");
    }
    assert_eq!(EffectOp::PatternOverlay(op().with_phase([f32::INFINITY, 0.0])).validate(), Err(EffectError::OutOfDomain));
}
