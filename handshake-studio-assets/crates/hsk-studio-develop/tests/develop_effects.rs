//! Lane SMB-DEVELOP-LENS-PRESETS slice 3: grain, post-crop vignette (STU-RAW-119) and calibration
//! (STU-RAW-120). Lightroom-matching values are UNVERIFIED (D5); these tests prove this crate's
//! documented maths (src/effects.rs) against hand-computed values and statistics.
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_develop::*;

fn profile() -> ProfileBinding {
    ProfileBinding { profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(), sha256: [7; 32] }
}

fn flat(width: u32, height: u32, sample: u16) -> CfaPlane {
    CfaPlane::with_limits(
        PlaneInit {
            width,
            height,
            stride_bytes: width * 2,
            sensor_origin: (0, 0),
            cfa: Cfa { pattern: CfaPattern::Bayer(Bayer::Rggb), phase_x: 0, phase_y: 0 },
            black: [0; 3],
            white: [4095; 3],
            source_revision: 1,
            bytes: (0..width * height).flat_map(|_| sample.to_le_bytes()).collect(),
        },
        Limits::LARGE,
    )
    .unwrap()
}

fn recipe() -> DevelopRecipe {
    DevelopRecipe::native([1.0; 3], [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], profile())
}

fn enc(c: f64) -> f64 {
    if c <= 0.003_130_8 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

fn smoothstep(t: f64) -> f64 {
    let c = t.clamp(0.0, 1.0);
    c * c * (3.0 - 2.0 * c)
}

fn image(plane: &CfaPlane, r: &DevelopRecipe) -> DevelopedImage {
    develop_to_image(plane, r, 1, &CancellationToken::default()).unwrap().0
}

fn autocorrelation(field: &[f64], w: usize) -> f64 {
    let (mut num, mut den) = (0.0, 0.0);
    for (i, v) in field.iter().enumerate() {
        den += v * v;
        if i % w + 1 < w {
            num += v * field[i + 1];
        }
    }
    num / den
}

#[test]
fn grain_statistics_match_hand_values() {
    // The field is exactly zero-mean, unit-deviation; size and roughness set its correlation.
    let (w, h) = (96usize, 64usize);
    let coarse = grain_field(w, h, 1.0, 0.0, 7);
    let mean = coarse.iter().sum::<f64>() / coarse.len() as f64;
    let sd = (coarse.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / coarse.len() as f64).sqrt();
    assert!(mean.abs() < 1e-12 && (sd - 1.0).abs() < 1e-12);
    let fine = grain_field(w, h, 0.0, 1.0, 7);
    assert!(autocorrelation(&coarse, w) > 0.8 && autocorrelation(&fine, w).abs() < 0.1);
    assert_eq!(grain_field(w, h, 0.25, 0.5, 7), grain_field(w, h, 0.25, 0.5, 7));
    assert_ne!(grain_field(w, h, 0.25, 0.5, 7), grain_field(w, h, 0.25, 0.5, 8));
    // Through develop on a flat frame: the encoded offset has deviation 0.12 * amount * m(b) and mean 0.
    let plane = flat(96, 64, 1000);
    let base = image(&plane, &recipe());
    let mut r = recipe();
    r.enables.enable_effects = true;
    r.adjust.effects.grain = Grain { amount: 50.0, size: 25.0, frequency: 50.0, seed: 397_038_266 };
    let grained = image(&plane, &r);
    let v = f64::from(base.rgb[1]);
    let b = enc(v);
    let m = 0.25 + 0.75 * 4.0 * b * (1.0 - b);
    let diffs: Vec<f64> = grained.rgb.chunks_exact(3).map(|p| enc(f64::from(p[1])) - b).collect();
    let dm = diffs.iter().sum::<f64>() / diffs.len() as f64;
    let dsd = (diffs.iter().map(|d| (d - dm).powi(2)).sum::<f64>() / diffs.len() as f64).sqrt();
    assert!(dm.abs() < 1e-5 && (dsd - 0.12 * 0.5 * m).abs() < 1e-5, "mean {dm} sd {dsd} want {}", 0.06 * m);
    // Monochrome grain: the three channels move together; replayable across thread counts.
    assert!(grained.rgb.chunks_exact(3).all(|p| p[0] == p[1] && p[1] == p[2]));
    let threads = develop_to_image_threads(&plane, &r, 1, Threads::new(4).unwrap(), &CancellationToken::default()).unwrap().0;
    assert_eq!(threads, grained);
}

#[test]
fn post_crop_vignette_matches_hand_values() {
    let plane = flat(64, 48, 1500);
    let base = image(&plane, &recipe());
    let v = base.rgb[1];
    let mut r = recipe();
    r.enables.enable_effects = true;
    r.adjust.effects.vignette = PostCropVignette { amount: -100.0, midpoint: 50.0, roundness: 0.0, feather: 50.0, highlight_contrast: 0.0, style: VignetteStyle::HighlightPriority };
    let img = image(&plane, &r);
    let at = |im: &DevelopedImage, x: usize, y: usize| im.rgb[(y * 64 + x) * 3 + 1];
    assert_eq!(at(&img, 32, 24), v, "the centre is untouched");
    // Corner (0, 0): ellipse distance d with half axes 32 x 24, b = 1.0, a = 1.0 - 0.5 = 0.5.
    let d = ((31.5f64 / 32.0).powi(2) + (23.5f64 / 24.0).powi(2)).sqrt();
    let t = smoothstep((d - 0.5) / 0.5);
    let want = f64::from(v) * (-2.0 * t).exp2();
    assert!((f64::from(at(&img, 0, 0)) - want).abs() < 1e-6, "{} vs {want}", at(&img, 0, 0));
    // Colour priority, positive: corners move toward white in the encoded domain.
    r.adjust.effects.vignette.style = VignetteStyle::ColourPriority;
    r.adjust.effects.vignette.amount = 100.0;
    let img = image(&plane, &r);
    let e = enc(f64::from(v));
    let want = e + t * (1.0 - e);
    assert!((enc(f64::from(at(&img, 0, 0))) - want).abs() < 1e-5);
    // Identity at amount 0 (bit-exact versus the group off).
    r.adjust.effects.vignette.amount = 0.0;
    assert_eq!(image(&plane, &r), base);
}

#[test]
fn calibration_moves_primaries_and_keeps_white() {
    let with = |f: &dyn Fn(&mut Calibration)| {
        let mut a = Adjustments::default();
        f(&mut a.calibration);
        a
    };
    // Red saturation -100: the red primary lands on the white point's chromaticity (neutral).
    let desat = with(&|c| c.red_saturation = -100.0);
    let red = apply_pixel(&desat, [1.0, 0.0, 0.0]).unwrap();
    assert!((red[0] - red[1]).abs() < 1e-9 && (red[1] - red[2]).abs() < 1e-9, "{red:?}");
    // White is a fixed point of every primary move.
    let mixed = with(&|c| {
        c.red_hue = 60.0;
        c.green_saturation = 40.0;
        c.blue_hue = -30.0;
    });
    let white = apply_pixel(&mixed, [1.0, 1.0, 1.0]).unwrap();
    assert!(white.iter().all(|v| (v - 1.0).abs() < 1e-12), "{white:?}");
    assert_ne!(apply_pixel(&mixed, [0.8, 0.2, 0.1]).unwrap(), [0.8, 0.2, 0.1]);
    // Shadow tint +100 on a dark grey: green x 2^(-0.25 w), red and blue x 2^(0.125 w).
    let tint = with(&|c| c.shadow_tint = 100.0);
    let g = 0.01;
    let w = 1.0 - smoothstep(enc(g) / 0.5);
    let out = apply_pixel(&tint, [g; 3]).unwrap();
    assert!((out[1] - g * (-0.25 * w).exp2()).abs() < 1e-9 && (out[0] - g * (0.125 * w).exp2()).abs() < 1e-9);
    assert_eq!(apply_pixel(&tint, [1.0; 3]).unwrap(), [1.0; 3], "highlights untouched (w = 0)");
    // Calibration is per pixel: the tile API runs it and matches the whole-frame image.
    let plane = flat(32, 32, 100); // dark (encoded ~0.17), inside the shadow-tint window
    let mut r = recipe();
    r.enables.enable_calibration = true;
    r.adjust.calibration = Calibration { shadow_tint: -40.0, ..Calibration::default() };
    let img = image(&plane, &r);
    let mut tiles = Vec::new();
    develop(
        &DevelopRequest {
            plane: &plane,
            recipe: &r,
            recipe_revision: 1,
            expected_source_revision: 1,
            grid: hsk_studio_pigment::Grid { origin_x: 0, origin_y: 0, tile_width: 32, tile_height: 32 },
            output_origin: (0, 0),
            execution_hint: ExecutionHint::Auto,
        },
        &CancellationToken::default(),
        &mut |t| {
            tiles.push(t);
            Ok(())
        },
    )
    .unwrap();
    let first = f32::from_le_bytes(tiles[0].colour[4..8].try_into().unwrap());
    assert_eq!(first, img.rgb[1]);
    assert_ne!(img, image(&plane, &recipe()));
}

#[test]
fn effects_are_whole_frame_and_identity_is_exact() {
    let plane = flat(40, 30, 900);
    let base = image(&plane, &recipe());
    let mut r = recipe();
    r.enables.enable_effects = true;
    r.enables.enable_calibration = true;
    assert_eq!(image(&plane, &r), base, "zero amounts are the identity");
    r.adjust.effects.grain.amount = 10.0;
    let err = develop(
        &DevelopRequest {
            plane: &plane,
            recipe: &r,
            recipe_revision: 1,
            expected_source_revision: 1,
            grid: hsk_studio_pigment::Grid { origin_x: 0, origin_y: 0, tile_width: 16, tile_height: 16 },
            output_origin: (0, 0),
            execution_hint: ExecutionHint::Auto,
        },
        &CancellationToken::default(),
        &mut |_| Ok(()),
    )
    .err();
    assert_eq!(err, Some(DevelopError::UnsupportedContribution("effects_whole_frame")));
    // Grain runs after the crop: same seed + same crop size = same grain on a shifted crop.
    r.crop.right = 0.5;
    let left = image(&plane, &r);
    r.crop.left = 0.5;
    r.crop.right = 1.0;
    let right = image(&plane, &r);
    assert_eq!(left, right);
}

#[test]
fn hostile_effect_and_calibration_values_are_typed() {
    let check = |f: &dyn Fn(&mut DevelopRecipe), want: DevelopError| {
        let mut r = recipe();
        r.enables.enable_effects = true;
        r.enables.enable_calibration = true;
        f(&mut r);
        assert_eq!(r.validate().err(), Some(want));
    };
    check(&|r| r.adjust.effects.grain.amount = 101.0, DevelopError::InvalidAdjustment);
    check(&|r| r.adjust.effects.grain.size = f64::NAN, DevelopError::NonFinite);
    check(&|r| r.adjust.effects.grain.frequency = -1.0, DevelopError::InvalidAdjustment);
    check(&|r| r.adjust.effects.vignette.midpoint = -1.0, DevelopError::InvalidAdjustment);
    check(&|r| r.adjust.effects.vignette.roundness = 200.0, DevelopError::InvalidAdjustment);
    check(&|r| r.adjust.effects.vignette.amount = f64::INFINITY, DevelopError::NonFinite);
    check(&|r| r.adjust.calibration.red_hue = 150.0, DevelopError::InvalidAdjustment);
    check(&|r| r.adjust.calibration.shadow_tint = f64::NAN, DevelopError::NonFinite);
    // A degenerate working matrix cannot carry calibration.
    check(
        &|r| {
            r.adjust.calibration.green_hue = 10.0;
            r.adjust.working_to_xyz_d50 = [0.0; 9];
        },
        DevelopError::InvalidAdjustment,
    );
    // Validated even while the groups are off (retained parameters must stay valid).
    let mut r = recipe();
    r.adjust.effects.grain.amount = -5.0;
    assert_eq!(r.validate().err(), Some(DevelopError::InvalidAdjustment));
    // The extreme seed is a valid parameter.
    let mut r = recipe();
    r.enables.enable_effects = true;
    r.adjust.effects.grain = Grain { amount: 100.0, size: 100.0, frequency: 0.0, seed: u32::MAX };
    assert!(develop_to_image(&flat(16, 16, 800), &r, 1, &CancellationToken::default()).is_ok());
}
