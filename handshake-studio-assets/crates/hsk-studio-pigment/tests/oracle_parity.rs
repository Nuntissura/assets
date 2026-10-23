//! Lane SMB-PS-ORACLE-PARITY-1: hand values of the Photoshop rules measured on the D5 oracle
//! (Photoshop 27.11, `<oracle corpus root>/adobe-session/photoshop`; the whole-image proof is the
//! harness suite `tests/d5_ps_compare.rs`). Every expected value below is a sample of an oracle
//! output (8-bit codes or the 15-bit scale of a 16-bit document), cited by case and variant.
use hsk_studio_pigment::adjust_math::levels_gamma;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, StudioBlendMode, composite_over_fill};
use hsk_studio_pigment::effect_math::{
    effect_blur_kernel, effect_blur_support, interior_mix, knockout_coverage, shadow_offset,
};

/// The effect blur is a tent of half-width `max(size, 2)` and the shadow offset rounds the half
/// of `5 cos 120` away from zero (PS-C12 square_s2/s10 spread 0 reproduce exactly; `*_a120_d5`
/// moves by (3, 4); `fractional_d2.5_a30` by (-2, 1)).
#[test]
fn effect_tent_kernel_and_offset_rounding() {
    assert_eq!(effect_blur_kernel(0.0).unwrap(), vec![1.0]);
    assert_eq!(effect_blur_kernel(2.0).unwrap(), vec![0.25, 0.5, 0.25]);
    // size 1 (the blur part of PS-C12 square_s2_sp50: 64 / 191 at the grown edge) = [1, 2, 1] / 4
    assert_eq!(effect_blur_kernel(1.0).unwrap(), vec![0.25, 0.5, 0.25]);
    let k10 = effect_blur_kernel(10.0).unwrap();
    assert_eq!(k10.len(), 19);
    assert!((k10[9] - 0.1).abs() < 1e-7 && (k10[0] - 0.01).abs() < 1e-7);
    assert_eq!(effect_blur_support(10.0), Ok(9));
    assert_eq!(effect_blur_support(40.0), Ok(39));
    assert_eq!(shadow_offset(120.0, 5.0), Ok((3, 4)));
    assert_eq!(shadow_offset(-60.0, 5.0), Ok((-3, -4)));
    assert_eq!(shadow_offset(30.0, 2.5), Ok((-2, 1)));
    assert_eq!(shadow_offset(0.0, 5.0), Ok((-5, 0)));
}

/// Knockout composites disjointly: PS-C12 soft_fill50_knocks at a rim pixel with shape alpha
/// 0.541 and shadow matte 0.102 stores alpha 0.318 (81 / 255); fill 0 stores `m (1 - a)`.
#[test]
fn knockout_coverage_and_interior_mix() {
    let (a, m) = (0.541_f32, 0.102_f32);
    let knocked = m * (1.0 - a);
    let cov = knockout_coverage(knocked, a, 0.5);
    let alpha = a * 0.5 + cov * (1.0 - a * 0.5);
    assert!((alpha * 255.0 - 81.0).abs() <= 1.0, "{}", alpha * 255.0);
    assert_eq!(knockout_coverage(knocked, a, 0.0), knocked);
    assert_eq!(knockout_coverage(0.0, 1.0, 1.0), 0.0);
    // Interior unit on an anti-aliased rim over transparency keeps the shape alpha (PS-C15 circle
    // rims: alpha 22 stays 22, the colour is the interior mix).
    let below = Rgba::TRANSPARENT;
    let inside = Rgba::new([0.5, 0.5, 0.5], 1.0);
    let out = interior_mix(below, inside, 22.0 / 255.0, true);
    assert!((out.a - 22.0 / 255.0).abs() < 1e-7);
    assert_eq!(out.rgb, [0.5, 0.5, 0.5]);
    assert_eq!(interior_mix(below, inside, 1.0, true), inside);
    assert_eq!(interior_mix(below, inside, 0.0, true), below);
}

/// Fill inside the blend for the special eight (PS-B01 d8_o100_f50_*, backdrop level 128 under
/// source level 128): colour dodge 171, colour burn 85, hard mix 129, difference 64 (8-bit codes,
/// within 1).
#[test]
fn special_eight_fill_inside_the_blend() {
    let v = 128.0 / 255.0;
    let b = Rgba::new([v; 3], 1.0);
    let s = Rgba::new([v; 3], 1.0);
    for (mode, want) in [
        (StudioBlendMode::ColourDodge, 171.0),
        (StudioBlendMode::ColourBurn, 85.0),
        (StudioBlendMode::HardMix, 129.0),
        (StudioBlendMode::Difference, 64.0),
    ] {
        let out = composite_over_fill(mode, b, s, 1.0, 0.5, PixelSite::default()).unwrap();
        let code = (out.rgb[0] * 255.0).round();
        assert!((code - want).abs() <= 1.0, "{mode:?} {code}");
        assert_eq!(out.a, 1.0);
    }
    // Every other mode keeps the product fold, bit for bit.
    let n = composite_over_fill(StudioBlendMode::Multiply, b, s, 0.8, 0.5, PixelSite::default()).unwrap();
    let p = hsk_studio_pigment::blend_math::composite_over(StudioBlendMode::Multiply, b, s, 0.8 * 0.5, PixelSite::default()).unwrap();
    assert_eq!(n, p);
}

/// Levels gamma 2 near black: PS-C02 16bit_comp_g2 stores 512.01, 1995.03, 8205.13 and 11608.18
/// (15-bit scale) at the ramp levels 1, 4, 19 and 32 (inputs on the 15-bit grid); within 4 steps.
#[test]
fn levels_gamma_toe_near_black() {
    for (level, want) in [(1.0_f32, 512.01_f32), (4.0, 1995.03), (19.0, 8205.13), (32.0, 11608.18)] {
        let x = (level * 32768.0 / 255.0).round() / 32768.0;
        let got = levels_gamma(x, 2.0) * 32768.0;
        assert!((got - want).abs() <= 4.0, "level {level}: {got} vs {want}");
    }
    // gamma at most 1 is the plain power (PS-C02 g0.5 matches it)
    assert_eq!(levels_gamma(0.25, 0.5), 0.25f32.powf(2.0));
    assert_eq!(levels_gamma(0.3, 1.0), 0.3);
}
