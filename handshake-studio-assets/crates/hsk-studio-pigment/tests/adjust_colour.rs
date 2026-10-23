//! Lane SMB-PS-ADJUST-STYLES slice 1: hand-computed single-pixel values for the second STU-RAS-151
//! adjustment set (`adjust_colour`). Every expected number is derived by hand in the comment next
//! to it from the formula documented on the function, not produced by the code under test.
//! Photoshop parity of every kind is UNVERIFIED (D5); these tests prove the Studio formulas only.
use hsk_studio_pigment::adjust_math::{
    AdjustError, AdjustmentKind, AdjustmentOp, BlackAndWhite, ChannelMixer, ColourBalance,
    ColourLookup, GradientMap, GradientMapStop, MatchColour, PhotoFilter, ReplaceColour,
    SelectiveColour, SelectiveColourMethod, ShadowsHighlights, ToneCompression,
    ToneCompressionMethod, Vibrance, composite_adjustment,
};
use hsk_studio_pigment::blend_math::{Exactness, Rgba, StudioBlendMode};

fn close3(a: [f32; 3], b: [f32; 3]) -> bool {
    (0..3).all(|i| (a[i] - b[i]).abs() <= 1e-5)
}
fn check(op: AdjustmentOp, input: [f32; 3], want: [f32; 3]) {
    let got = op.apply(input).unwrap();
    assert!(close3(got, want), "{:?}: {got:?} != {want:?}", op.kind());
}

#[test]
fn vibrance_balance_filter_black_and_white_mixer() {
    // Vibrance 100 on (0.6, 0.4, 0.4): L = 0.18 + 0.236 + 0.044 = 0.46, chroma 0.2, factor
    // 1 + 1 (1 - 0.2) = 1.8 -> 0.46 + 0.14 * 1.8 = 0.712, 0.46 - 0.06 * 1.8 = 0.352.
    let vib = |vibrance, saturation| AdjustmentOp::Vibrance(Vibrance { vibrance, saturation });
    check(vib(100.0, 0.0), [0.6, 0.4, 0.4], [0.712, 0.352, 0.352]);
    // Saturation -100: chroma scaled by 0 -> all channels Lum = 0.46.
    check(vib(0.0, -100.0), [0.6, 0.4, 0.4], [0.46, 0.46, 0.46]);

    // Colour balance midtones cyan_red +50 on gray 0.5: HSL l = 0.5, w_m = clamp(0.167 / 0.25 +
    // 0.5) clamp(-0.167 / -0.25 + 0.5) 0.7 = 0.7, w_s = clamp(-0.668 + 0.5) = 0, w_h = 0 -> red
    // 0.5 + 50 * 0.7 / 100 = 0.85.
    let bal = ColourBalance {
        shadows: [0.0; 3],
        midtones: [50.0, 0.0, 0.0],
        highlights: [0.0; 3],
        preserve_luminosity: false,
    };
    check(AdjustmentOp::ColourBalance(bal), [0.5; 3], [0.85, 0.5, 0.5]);

    // Photo filter (1, 0.5, 0) at density 50 on gray 0.5: multiply (0.5, 0.25, 0), halfway
    // -> (0.5, 0.375, 0.25). Preserve luminosity: Lum = 0.15 + 0.22125 + 0.0275 = 0.39875, shift
    // 0.5 - 0.39875 = 0.10125 -> (0.60125, 0.47625, 0.35125), no clip.
    let pf = |preserve_luminosity| {
        AdjustmentOp::PhotoFilter(PhotoFilter {
            colour: [1.0, 0.5, 0.0],
            density: 50.0,
            preserve_luminosity,
        })
    };
    check(pf(false), [0.5; 3], [0.5, 0.375, 0.25]);
    check(pf(true), [0.5; 3], [0.60125, 0.47625, 0.35125]);

    // Black & white, weights (40, 60, 40, 60, 20, 80) on (0.8, 0.4, 0.2): max red (primary red
    // 0.4), mid green (secondary yellow 0.6) -> 0.2 + 0.4 * 0.4 + 0.2 * 0.6 = 0.48.
    let bw = BlackAndWhite {
        weights: [40.0, 60.0, 40.0, 60.0, 20.0, 80.0],
        tint: None,
    };
    check(AdjustmentOp::BlackAndWhite(bw), [0.8, 0.4, 0.2], [0.48; 3]);
    // Tint hue 0 saturation 100 at gray 0.48: HSL m2 = 0.48 * 2 = 0.96, m1 = 0 -> (0.96, 0, 0).
    let tinted = BlackAndWhite {
        tint: Some([0.0, 100.0]),
        ..bw
    };
    check(AdjustmentOp::BlackAndWhite(tinted), [0.8, 0.4, 0.2], [0.96, 0.0, 0.0]);

    // Channel mixer, red row (100, 50, 0, +10) on (0.5, 0.2, 0.1): 0.5 + 0.1 + 0 + 0.1 = 0.7;
    // identity green and blue rows. Monochrome repeats the red row.
    let mix = |monochrome| {
        AdjustmentOp::ChannelMixer(ChannelMixer {
            red: [100.0, 50.0, 0.0, 10.0],
            green: [0.0, 100.0, 0.0, 0.0],
            blue: [0.0, 0.0, 100.0, 0.0],
            monochrome,
        })
    };
    check(mix(false), [0.5, 0.2, 0.1], [0.7, 0.2, 0.1]);
    check(mix(true), [0.5, 0.2, 0.1], [0.7, 0.7, 0.7]);
}

#[test]
fn selective_colour_gradient_map_and_lookup() {
    // Pixel (0.8, 0.4, 0.2): reds membership max - mid = 0.4 (red is max), yellows mid - min = 0.2
    // (blue is min), neutrals 1 - (0.3 + 0.3) = 0.4; only the reds range carries values here.
    let sel = |method, cyan, black| {
        let mut ranges = [[0.0f32; 4]; 9];
        ranges[0] = [cyan, 0.0, 0.0, black];
        AdjustmentOp::SelectiveColour(SelectiveColour { method, ranges })
    };
    // Absolute cyan -25: red ink delta -0.25 x 0.4 = -0.1 -> red 0.8 + 0.1 = 0.9.
    check(
        sel(SelectiveColourMethod::Absolute, -25.0, 0.0),
        [0.8, 0.4, 0.2],
        [0.9, 0.4, 0.2],
    );
    // Relative cyan -25: red ink 0.2, delta -0.25 x 0.2 = -0.05, x 0.4 = -0.02 -> 0.82.
    check(
        sel(SelectiveColourMethod::Relative, -25.0, 0.0),
        [0.8, 0.4, 0.2],
        [0.82, 0.4, 0.2],
    );
    // Absolute black +50: every channel loses 0.5 x 0.4 = 0.2 -> (0.6, 0.2, 0.0).
    check(
        sel(SelectiveColourMethod::Absolute, 0.0, 50.0),
        [0.8, 0.4, 0.2],
        [0.6, 0.2, 0.0],
    );

    // Gradient map black -> red: (1, 1, 0) has Lum 0.3 + 0.59 = 0.89 -> (0.89, 0, 0); reversed
    // t = 0.11 -> (0.11, 0, 0). Three stops black, green at 0.5, white: gray 0.75 sits halfway
    // between green and white -> (0.5, 1, 0.5).
    let stop = |position, colour| GradientMapStop { position, colour };
    let two = |reverse| {
        AdjustmentOp::GradientMap(GradientMap {
            stops: vec![stop(0.0, [0.0; 3]), stop(1.0, [1.0, 0.0, 0.0])],
            reverse,
        })
    };
    check(two(false), [1.0, 1.0, 0.0], [0.89, 0.0, 0.0]);
    check(two(true), [1.0, 1.0, 0.0], [0.11, 0.0, 0.0]);
    let three = AdjustmentOp::GradientMap(GradientMap {
        stops: vec![
            stop(0.0, [0.0; 3]),
            stop(0.5, [0.0, 1.0, 0.0]),
            stop(1.0, [1.0; 3]),
        ],
        reverse: false,
    });
    check(three, [0.75; 3], [0.5, 1.0, 0.5]);

    // 2^3 inverting cube (entry r + 2 (g + 2 b) holds 1 - corner): trilinear reproduces 1 - c.
    let mut table = Vec::new();
    for b in 0..2 {
        for g in 0..2 {
            for r in 0..2 {
                table.push([1.0 - r as f32, 1.0 - g as f32, 1.0 - b as f32]);
            }
        }
    }
    let lut = ColourLookup { side: 2, table };
    check(
        AdjustmentOp::ColourLookup(lut.clone()),
        [0.25, 0.5, 1.0],
        [0.75, 0.5, 0.0],
    );
    // Hostile cubes: side above 65 is refused before the (empty) table is read; a short table
    // and a NaN entry are typed refusals too.
    let refused = |l: ColourLookup| AdjustmentOp::ColourLookup(l).prepare().unwrap_err();
    assert_eq!(
        refused(ColourLookup { side: u16::MAX, table: Vec::new() }),
        AdjustError::InvalidParameter("colour_lookup.side")
    );
    assert_eq!(
        refused(ColourLookup { side: 2, table: lut.table[..7].to_vec() }),
        AdjustError::InvalidParameter("colour_lookup.table_length")
    );
    let mut nan = lut.clone();
    nan.table[3] = [f32::NAN, 0.0, 0.0];
    assert_eq!(
        refused(nan),
        AdjustError::InvalidParameter("colour_lookup.table")
    );
}

#[test]
fn tone_match_and_replace() {
    // Shadows/highlights global form, shadow amount 50, tone width 50 on gray 0.2: ws = (1 -
    // 0.2 / 0.5)^2 = 0.36, highlight width 0 -> wh = 0; L1 = 0.2 + 0.5 x 0.36 x 0.8 = 0.344.
    let sh = ShadowsHighlights {
        shadow_amount: 50.0,
        shadow_tone_width: 50.0,
        shadow_radius: 0.0,
        highlight_amount: 0.0,
        highlight_tone_width: 0.0,
        highlight_radius: 0.0,
        colour_correction: 0.0,
        midtone_contrast: 0.0,
        black_clip: 0.01,
        white_clip: 0.01,
    };
    check(AdjustmentOp::ShadowsHighlights(sh), [0.2; 3], [0.344; 3]);
    // A radius needs a neighbourhood: typed refusal, not a silent global render.
    assert_eq!(
        AdjustmentOp::ShadowsHighlights(ShadowsHighlights {
            shadow_radius: 30.0,
            ..sh
        })
        .prepare()
        .unwrap_err(),
        AdjustError::Unsupported("shadows_highlights.radius")
    );

    // Tone compression: exposure-gamma e = 1, gamma 2 on 0.25 -> 0.5^(1/2) = 0.70710677;
    // highlight compression e = 1, gamma 1 on 0.5 -> 1 / (1 + 1) = 0.5.
    let tc = |method, exposure, gamma| {
        AdjustmentOp::ToneCompression(ToneCompression {
            method,
            exposure,
            gamma,
        })
    };
    check(
        tc(ToneCompressionMethod::ExposureGamma, 1.0, 2.0),
        [0.25; 3],
        [0.707_106_77; 3],
    );
    check(
        tc(ToneCompressionMethod::HighlightCompression, 1.0, 1.0),
        [0.5; 3],
        [0.5; 3],
    );
    assert_eq!(
        tc(ToneCompressionMethod::LocalAdaptation, 0.0, 1.0)
            .prepare()
            .unwrap_err(),
        AdjustError::Unsupported("tone_compression.local_adaptation")
    );

    // Match colour on gray 0.5, target mean 0.4 / std 0.1, source means (0.6, 0.5, 0.4) / std
    // 0.2: (0.5 - 0.4) x 2 + mean = (0.8, 0.7, 0.6). Fade 50 -> halfway to 0.5: (0.65, 0.6,
    // 0.55). Neutralize: source means -> Lum(0.6, 0.5, 0.4) = 0.18 + 0.295 + 0.044 = 0.519, so
    // every channel 0.2 + 0.519 = 0.719.
    let mc = |fade, neutralize| {
        AdjustmentOp::MatchColour(MatchColour {
            luminance: 100.0,
            colour_intensity: 100.0,
            fade,
            neutralize,
            source_mean: [0.6, 0.5, 0.4],
            source_std: [0.2; 3],
            target_mean: [0.4; 3],
            target_std: [0.1; 3],
        })
    };
    check(mc(0.0, false), [0.5; 3], [0.8, 0.7, 0.6]);
    check(mc(50.0, false), [0.5; 3], [0.65, 0.6, 0.55]);
    check(mc(0.0, true), [0.5; 3], [0.719; 3]);

    // Replace colour, sample red, fuzziness 40, hue +120. Red itself: membership 1, HSL hue 0 ->
    // 1/3 = green (0, 1, 0). (1, 20/255, 0): d = 20/255 -> membership 1 - 20 / 40 = 0.5; its HSL is
    // (h = (20/255) / 6 = 0.0130719, s 1, l 0.5), rotated h = 0.3464052 -> (0, 1, 6 h0 = 0.0784314),
    // halfway -> (0.5, 0.5392157, 0.0392157). Blue: distance sqrt(2), membership 0 -> unchanged.
    let rc = AdjustmentOp::ReplaceColour(ReplaceColour {
        samples: vec![[1.0, 0.0, 0.0]],
        fuzziness: 40.0,
        hue: 120.0,
        saturation: 0.0,
        lightness: 0.0,
    });
    check(rc.clone(), [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
    check(
        rc.clone(),
        [1.0, 20.0 / 255.0, 0.0],
        [0.5, 0.539_215_7, 0.039_215_7],
    );
    check(rc, [0.0, 0.0, 1.0], [0.0, 0.0, 1.0]);
}

#[test]
fn second_set_layer_rule_facts_and_hostile_parameters() {
    // Adjustment layer rule unchanged for the new kinds: monochrome mixer (0.7 gray) at coverage
    // 0.5 over opaque (0.5, 0.2, 0.1) -> (0.6, 0.45, 0.4), alpha 1.
    let mix = AdjustmentOp::ChannelMixer(ChannelMixer {
        red: [100.0, 50.0, 0.0, 10.0],
        green: [0.0, 100.0, 0.0, 0.0],
        blue: [0.0, 0.0, 100.0, 0.0],
        monochrome: true,
    })
    .prepare()
    .unwrap();
    let out = composite_adjustment(
        StudioBlendMode::Normal,
        Rgba::new([0.5, 0.2, 0.1], 1.0),
        &mix,
        0.5,
    )
    .unwrap();
    assert!(close3(out.rgb, [0.6, 0.45, 0.4]) && out.a == 1.0, "{out:?}");

    // Every second-set kind is in ALL after the first eight (GPU opcodes unchanged) and is
    // Approximate (Photoshop parity UNVERIFIED, D5).
    assert_eq!(AdjustmentKind::ALL.len(), 20);
    assert_eq!(AdjustmentKind::ALL[7], AdjustmentKind::Posterize);
    for kind in &AdjustmentKind::ALL[8..] {
        assert_eq!(kind.facts().exactness, Exactness::Approximate, "{kind:?}");
    }
    assert_eq!(AdjustmentKind::ReplaceColour.key(), "replace_colour");

    // Hostile parameters: NaN and out-of-range values are typed refusals before any work; a
    // gradient with one stop or with decreasing positions, and an empty sample set, refuse too.
    let err = |op: AdjustmentOp| op.prepare().unwrap_err();
    assert_eq!(
        err(AdjustmentOp::Vibrance(Vibrance {
            vibrance: f32::NAN,
            saturation: 0.0
        })),
        AdjustError::InvalidParameter("vibrance.vibrance")
    );
    assert_eq!(
        err(AdjustmentOp::PhotoFilter(PhotoFilter {
            colour: [1.0, 0.0, 0.0],
            density: 101.0,
            preserve_luminosity: false
        })),
        AdjustError::InvalidParameter("photo_filter.density")
    );
    let stop = |position| GradientMapStop {
        position,
        colour: [0.0; 3],
    };
    assert_eq!(
        err(AdjustmentOp::GradientMap(GradientMap {
            stops: vec![stop(0.0)],
            reverse: false
        })),
        AdjustError::InvalidParameter("gradient_map.stop_count")
    );
    assert_eq!(
        err(AdjustmentOp::GradientMap(GradientMap {
            stops: vec![stop(0.6), stop(0.4)],
            reverse: false
        })),
        AdjustError::InvalidParameter("gradient_map.stops")
    );
    assert_eq!(
        err(AdjustmentOp::ReplaceColour(ReplaceColour {
            samples: Vec::new(),
            fuzziness: 40.0,
            hue: 0.0,
            saturation: 0.0,
            lightness: 0.0
        })),
        AdjustError::InvalidParameter("replace_colour.samples")
    );
    assert_eq!(
        err(AdjustmentOp::BlackAndWhite(BlackAndWhite {
            weights: [40.0, 60.0, 40.0, 60.0, 20.0, f32::INFINITY],
            tint: None
        })),
        AdjustError::InvalidParameter("black_and_white.weights")
    );
    // A colour outside [0, 1] is OutOfDomain for the new kinds as for the first set.
    assert_eq!(
        AdjustmentOp::Vibrance(Vibrance {
            vibrance: 10.0,
            saturation: 0.0
        })
        .apply([1.5, 0.0, 0.0]),
        Err(AdjustError::OutOfDomain)
    );
}
