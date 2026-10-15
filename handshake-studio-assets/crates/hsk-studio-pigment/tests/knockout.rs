//! Lane SMB-PS-ADJUST-STYLES slice 3: the shallow-knockout rule (`knockout_math::knockout_over`),
//! hand values from the documented formula: `S = composite_over(mode, initial, source, opacity)`,
//! then `lerp(running, S, shape)` on premultiplied colour and alpha. Photoshop parity UNVERIFIED (D5).
use hsk_studio_pigment::blend_math::{BlendError, Exactness, PixelSite, Rgba, StudioBlendMode};
use hsk_studio_pigment::knockout_math::{KNOCKOUT_EXACTNESS, knockout_over};
use hsk_studio_pigment::render_plan::{ExtOperator, Knockout};

const SITE: PixelSite = PixelSite { x: 0, y: 0, seed: 0 };

fn ko(opacity: f32, shape: f32) -> Rgba {
    knockout_over(
        StudioBlendMode::Normal,
        Rgba::new([0.0, 1.0, 0.0], 1.0),
        Rgba::TRANSPARENT,
        Rgba::new([0.0, 0.0, 1.0], 1.0),
        opacity,
        shape,
        SITE,
    )
    .unwrap()
}
fn close(a: Rgba, rgb: [f32; 3], alpha: f32) -> bool {
    (0..3).all(|i| (a.rgb[i] - rgb[i]).abs() <= 1e-6) && (a.a - alpha).abs() <= 1e-6
}

#[test]
fn shallow_knockout_hand_values() {
    // Fill 0 (opacity x fill = 0), full shape over opaque green: S is the transparent initial
    // backdrop, so the layer punches a fully transparent hole.
    assert_eq!(ko(0.0, 1.0), Rgba::TRANSPARENT);
    // Opacity 1, full shape: the blue content over the initial backdrop replaces the green.
    assert!(close(ko(1.0, 1.0), [0.0, 0.0, 1.0], 1.0));
    // Layer opacity 0.5 x fill 1, shape 0.5: S = blue at alpha 0.5; alpha 1 + (0.5 - 1) 0.5 = 0.75;
    // premultiplied (0, 1, 0) + ((0, 0, 0.5) - (0, 1, 0)) 0.5 = (0, 0.5, 0.25) -> / 0.75 =
    // (0, 0.6666667, 0.3333333).
    assert!(close(ko(0.5, 0.5), [0.0, 0.666_666_7, 0.333_333_3], 0.75));
    // Fill 0 with half shape: half the green knocked out, colour unchanged.
    assert!(close(ko(0.0, 0.5), [0.0, 1.0, 0.0], 0.5));
    // Shape 0 leaves the running result as it is.
    assert!(close(ko(1.0, 0.0), [0.0, 1.0, 0.0], 1.0));
    // Hostile shape values are typed refusals.
    for bad in [f32::NAN, -0.1, 1.5] {
        assert_eq!(
            knockout_over(StudioBlendMode::Normal, Rgba::TRANSPARENT, Rgba::TRANSPARENT, Rgba::TRANSPARENT, 1.0, bad, SITE),
            Err(BlendError::OutOfDomain)
        );
    }
    // Class: shallow is pigment's Approximate rule, deep stays Unsupported (no Studio definition).
    assert_eq!(KNOCKOUT_EXACTNESS, Exactness::Approximate);
    assert_eq!(ExtOperator::Knockout(Knockout::Shallow).intrinsic(), Exactness::Approximate);
    assert_eq!(ExtOperator::Knockout(Knockout::Deep).intrinsic(), Exactness::Unsupported);
}
