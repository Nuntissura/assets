//! Lane SMB-DEVELOP-CONTROLS-2 slice A: absolute kelvin/tint white balance, point colour, grayscale
//! mix, colour grading. Lightroom-matching values are UNVERIFIED (D5); these tests prove this
//! crate's documented maths, identity, selectivity, refusals and corpus digest pins, never parity.
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_develop::*;
use sha2::{Digest, Sha256};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn profile() -> ProfileBinding {
    ProfileBinding {
        profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(),
        sha256: [7; 32],
    }
}

fn dec(e: f64) -> f64 {
    if e <= 0.040_45 { e / 12.92 } else { ((e + 0.055) / 1.055).powf(2.4) }
}

fn lin(e: [f64; 3]) -> [f64; 3] {
    [dec(e[0]), dec(e[1]), dec(e[2])]
}

fn hsv(rgb: [f64; 3]) -> (f64, f64, f64) {
    // Encoded-domain saturation and value of a linear pixel.
    let enc = |c: f64| if c <= 0.003_130_8 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    let e = [enc(rgb[0]), enc(rgb[1]), enc(rgb[2])];
    let mx = e[0].max(e[1]).max(e[2]);
    let mn = e[0].min(e[1]).min(e[2]);
    (0.0, if mx > 0.0 { (mx - mn) / mx } else { 0.0 }, mx)
}

fn synthetic_colour() -> DngColor {
    let mut c = DngColor::empty();
    // A linear-sRGB-primaried camera: XYZ(D50) -> camera is the inverse of the sRGB colorants.
    c.color_matrix[0] = Some(color::invert(&color::SRGB_TO_XYZ_D50).unwrap());
    c.calibration_illuminant[0] = Some(23);
    c.as_shot_neutral = Some([1.0, 1.0, 1.0]);
    c
}

fn binding_srgb() -> color::Mat3 {
    color::xyz_d50_to_linear_srgb().unwrap()
}

#[test]
fn absolute_white_balance_resolves_the_target_illuminant() {
    let colour = synthetic_colour();
    let at = |kelvin: f64, tint: f64| {
        recipe_for_white_point(&colour, &binding_srgb(), profile(), BaselineExposure::Ignore, kelvin, tint).unwrap()
    };
    // The resolved neutral is a white: matrix * (gain * neutral) = to_profile * neutral = (1,1,1).
    for (k, t) in [(2500.0, 0.0), (5003.0, 0.0), (6500.0, 40.0), (12000.0, -60.0), (25000.0, 150.0)] {
        let (recipe, solution) = at(k, t);
        let gain = recipe::as_shot_gain(solution.neutral).unwrap();
        let balanced = [gain[0] * solution.neutral[0], gain[1] * solution.neutral[1], gain[2] * solution.neutral[2]];
        let m = &recipe.camera_to_profile_rgb;
        for row in 0..3 {
            let v = m[row * 3] * balanced[0] + m[row * 3 + 1] * balanced[1] + m[row * 3 + 2] * balanced[2];
            assert!((v - 1.0).abs() < 1e-6, "{k} K tint {t}: row {row} = {v}");
        }
        assert_eq!(recipe.white_balance_mode, WbMode::Temperature);
        assert_eq!(recipe.adjust.absolute_white_balance, Some(AbsoluteWhiteBalance { kelvin: k, tint: t }));
        recipe.validate().unwrap();
        assert!((solution.cct - k).abs() / k < 0.05 || t != 0.0, "cct {} for {k}", solution.cct);
    }
    // A bluer illuminant means more blue in the camera neutral (relative to red): monotone in kelvin.
    let mut last = 0.0;
    for k in [2000.0, 2500.0, 3200.0, 4000.0, 5003.0, 6500.0, 9000.0, 15000.0, 25000.0] {
        let n = at(k, 0.0).1.neutral;
        let ratio = n[2] / n[0];
        assert!(ratio > last, "{k} K: blue/red {ratio} must rise");
        last = ratio;
    }
    // Positive tint = greener illuminant: the camera neutral gains green relative to red and blue.
    let share = |t: f64| {
        let n = at(6500.0, t).1.neutral;
        n[1] / (n[0] + n[2])
    };
    assert!(share(100.0) > share(0.0) && share(0.0) > share(-100.0), "{} {} {}", share(100.0), share(0.0), share(-100.0));
    // Refusals.
    for (k, t, want) in [
        (1999.0, 0.0, DevelopError::InvalidAdjustment),
        (25_000.5, 0.0, DevelopError::InvalidAdjustment),
        (6500.0, 150.5, DevelopError::InvalidAdjustment),
        (6500.0, -151.0, DevelopError::InvalidAdjustment),
        (f64::NAN, 0.0, DevelopError::NonFinite),
        (6500.0, f64::INFINITY, DevelopError::NonFinite),
    ] {
        let r = recipe_for_white_point(&colour, &binding_srgb(), profile(), BaselineExposure::Ignore, k, t);
        assert_eq!(r.err(), Some(want), "{k} {t}");
    }
    let (p, q) = (white_xy(6500.0, 0.0).unwrap(), planck_xy(6500.0).unwrap());
    assert!((p[0] - q[0]).abs() < 1e-9 && (p[1] - q[1]).abs() < 1e-9);
    // Mode / request consistency in the recipe.
    let mut r = at(6500.0, 0.0).0;
    r.adjust.absolute_white_balance = None;
    assert_eq!(r.validate().err(), Some(DevelopError::UnsupportedWhiteBalance));
    let mut r = at(6500.0, 0.0).0;
    r.white_balance_mode = WbMode::AsShot;
    assert_eq!(r.validate().err(), Some(DevelopError::InvalidAdjustment));
    let mut r = at(6500.0, 0.0).0;
    r.adjust.absolute_white_balance = Some(AbsoluteWhiteBalance { kelvin: 100.0, tint: 0.0 });
    assert_eq!(r.validate().err(), Some(DevelopError::InvalidAdjustment));
    // Corpus DNG (two profiles, forward matrices): an explicit white near the as-shot CCT lands near
    // the as-shot neutral (the as-shot white is not exactly on the Planckian locus, so a tolerance).
    const TEST: &str = "absolute_white_balance_resolves_the_target_illuminant";
    if let Some(bytes) = corpus_file(TEST, "canon-eos-5d3-mlv-14bit-lossless.dng", "1d77dcc6cdb839b10c1948f0b4471984ab48cf15e973fb77520584cee72b603e") {
        let raw = decode_container(&bytes).unwrap();
        let (_, shot) = raw.recipe(&binding_srgb(), profile(), BaselineExposure::Ignore).unwrap();
        let (recipe, solution) = raw.recipe_for_white(&binding_srgb(), profile(), BaselineExposure::Ignore, shot.cct, 0.0).unwrap();
        let (a, b) = (shot.neutral, solution.neutral);
        let (ra, rb) = (a[2] / a[0], b[2] / b[0]);
        println!("corpus as-shot {:.0} K neutral {:?}; explicit neutral {:?}", shot.cct, a, b);
        assert!((ra - rb).abs() / ra < 0.12, "blue/red as-shot {ra} vs explicit {rb}");
        recipe.validate().unwrap();
    }
}

fn corpus_file(test: &str, file: &str, sha256: &str) -> Option<Vec<u8>> {
    let ws = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).unwrap().to_path_buf();
    let reason = |why: String| {
        println!("NOT_PROVEN: {test} {file} reason={why}");
        None
    };
    let Ok(text) = std::fs::read_to_string(ws.join(".config").join("oracle-corpus.toml")) else {
        return reason("corpus_convention_absent".into());
    };
    let value = |key: &str| {
        text.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
        })
    };
    let (Some(search), Some(max)) = (value("search"), value("max-ancestors").and_then(|v| v.parse::<usize>().ok())) else {
        return reason("corpus_convention_malformed".into());
    };
    let Some(root) = ws.ancestors().take(max + 1).map(|a| a.join(&search)).find(|c| c.is_dir()) else {
        return reason("corpus_root_absent".into());
    };
    let Ok(bytes) = std::fs::read(root.join("dng").join(file)) else {
        return reason("file_absent".into());
    };
    let got = hex(&Sha256::digest(&bytes));
    if got != sha256 {
        return reason(format!("sha256_mismatch got={got}"));
    }
    Some(bytes)
}

fn with(f: impl FnOnce(&mut Adjustments)) -> Adjustments {
    let mut a = Adjustments::default();
    f(&mut a);
    a
}

fn near(a: [f64; 3], b: [f64; 3], tol: f64) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() <= tol)
}

#[test]
fn point_colour_selects_by_hue_saturation_and_value() {
    let green = lin([0.20, 0.60, 0.25]);
    let red = lin([0.65, 0.20, 0.18]);
    let source = [0.20, 0.60, 0.25];
    let point = |f: &dyn Fn(&mut PointColour)| {
        let mut p = PointColour::new(source);
        f(&mut p);
        with(|a| a.point_colours = vec![p])
    };
    // Zero sliders: identity (the point is dropped), whatever the ranges.
    assert_eq!(apply_pixel(&point(&|_| {}), green).unwrap(), green);
    assert_eq!(apply_pixel(&point(&|p| p.hue_range = 100.0), red).unwrap(), red);
    // Saturation on the sampled green raises its saturation; a red pixel is outside the hue range.
    let more = apply_pixel(&point(&|p| p.saturation = 100.0), green).unwrap();
    assert!(hsv(more).1 > hsv(green).1 + 0.05, "{more:?}");
    assert!(near(apply_pixel(&point(&|p| p.saturation = 100.0), red).unwrap(), red, 1e-9));
    let less = apply_pixel(&point(&|p| p.saturation = -100.0), green).unwrap();
    assert!(hsv(less).1 < hsv(green).1 - 0.05);
    // Luminance slider scales value of the selection; hue shift rotates it (green + moves toward cyan).
    assert!(hsv(apply_pixel(&point(&|p| p.luminance = 100.0), green).unwrap()).2 > hsv(green).2);
    assert!(hsv(apply_pixel(&point(&|p| p.luminance = -100.0), green).unwrap()).2 < hsv(green).2);
    let shifted = apply_pixel(&point(&|p| p.hue_shift = 100.0), green).unwrap();
    assert!(shifted[2] > green[2] && shifted[0] < green[0] + 1e-9, "{shifted:?}");
    // Range: a pixel 20 degrees away is untouched by a tight range and touched by a wide one.
    let near_green = lin([0.20, 0.60, 0.50]);
    let tight = apply_pixel(&point(&|p| { p.saturation = 100.0; p.hue_range = 0.0; }), near_green).unwrap();
    let wide = apply_pixel(&point(&|p| { p.saturation = 100.0; p.hue_range = 100.0; }), near_green).unwrap();
    assert!(near(tight, near_green, 1e-9) && !near(wide, near_green, 1e-6));
    // Greys and a negative pixel are fixed points; an achromatic source selects by value only.
    let grey_point = with(|a| {
        let mut p = PointColour::new([0.5, 0.5, 0.5]);
        p.saturation = 100.0;
        p.hue_shift = 100.0;
        p.luminance = 100.0;
        a.point_colours = vec![p];
    });
    for g in [0.0, 0.1, 0.214, 1.0] {
        let out = apply_pixel(&grey_point, [g; 3]).unwrap();
        assert!(near(out, [g; 3], 1e-12 * g.max(1.0)), "{g}: {out:?}");
    }
    assert_eq!(apply_pixel(&point(&|p| p.saturation = 100.0), [0.4, -0.1, 0.2]).unwrap(), [0.4, -0.1, 0.2]);
    // Several points combine; the list is bounded.
    let two = with(|a| {
        let mut g = PointColour::new(source);
        g.saturation = 100.0;
        let mut r = PointColour::new([0.65, 0.20, 0.18]);
        r.saturation = -100.0;
        a.point_colours = vec![g, r];
    });
    assert!(hsv(apply_pixel(&two, green).unwrap()).1 > hsv(green).1 && hsv(apply_pixel(&two, red).unwrap()).1 < hsv(red).1);
    let many = with(|a| a.point_colours = vec![PointColour::new(source); MAX_POINT_COLOURS + 1]);
    assert_eq!(apply_pixel(&many, green).err(), Some(DevelopError::InvalidAdjustment));
    // Hostile points.
    let bad = |f: &dyn Fn(&mut PointColour), want: DevelopError| {
        assert_eq!(apply_pixel(&point(f), green).err(), Some(want));
    };
    bad(&|p| p.source = [f64::NAN, 0.0, 0.0], DevelopError::NonFinite);
    bad(&|p| p.source = [1.01, 0.0, 0.0], DevelopError::InvalidAdjustment);
    bad(&|p| p.source = [0.0, -0.01, 0.0], DevelopError::InvalidAdjustment);
    bad(&|p| p.hue_shift = 100.5, DevelopError::InvalidAdjustment);
    bad(&|p| p.saturation = f64::INFINITY, DevelopError::NonFinite);
    bad(&|p| p.hue_range = 100.1, DevelopError::InvalidAdjustment);
    bad(&|p| p.saturation_range = -0.1, DevelopError::InvalidAdjustment);
    bad(&|p| p.luminance_range = f64::NAN, DevelopError::NonFinite);
}

#[test]
fn grayscale_mix_converts_and_weights_bands() {
    let mix = |convert: bool, f: &dyn Fn(&mut [f64; 8])| {
        with(|a| {
            a.grayscale.convert = convert;
            f(&mut a.grayscale.bands);
        })
    };
    let red = lin([0.70, 0.18, 0.15]);
    let blue = lin([0.15, 0.25, 0.70]);
    let green = lin([0.20, 0.60, 0.25]);
    // Not converting: bands are retained but ignored.
    assert_eq!(apply_pixel(&mix(false, &|b| b[0] = 100.0), red).unwrap(), red);
    // Converting: every channel equal; all-zero bands give the plain luma grey.
    for px in [red, blue, green] {
        let g = apply_pixel(&mix(true, &|_| {}), px).unwrap();
        assert!(g[0] == g[1] && g[1] == g[2], "{g:?}");
    }
    // The red band lightens/darkens a red pixel's grey, not a blue one's; blue band the reverse.
    let base_red = apply_pixel(&mix(true, &|_| {}), red).unwrap()[0];
    let base_blue = apply_pixel(&mix(true, &|_| {}), blue).unwrap()[0];
    assert!(apply_pixel(&mix(true, &|b| b[0] = 100.0), red).unwrap()[0] > base_red);
    assert!(apply_pixel(&mix(true, &|b| b[0] = -100.0), red).unwrap()[0] < base_red);
    assert!((apply_pixel(&mix(true, &|b| b[0] = 100.0), blue).unwrap()[0] - base_blue).abs() < 1e-9);
    assert!(apply_pixel(&mix(true, &|b| b[5] = 100.0), blue).unwrap()[0] > base_blue);
    // Greys are unaffected by any band; the result is never negative, even at -100 on every band.
    let all = mix(true, &|b| *b = [-100.0; 8]);
    for g in [0.0, 0.05, 0.5, 2.0] {
        let out = apply_pixel(&mix(true, &|b| *b = [100.0, -100.0, 100.0, -100.0, 100.0, -100.0, 100.0, -100.0]), [g; 3]).unwrap();
        assert!((out[0] - g).abs() < 1e-9 * g.max(1.0), "{g}: {out:?}");
        assert!(apply_pixel(&all, red).unwrap().iter().all(|v| *v >= 0.0));
    }
    // Continuity over the whole hue circle (band weights partition unity).
    let alt = mix(true, &|b| *b = [100.0, -100.0, 100.0, -100.0, 100.0, -100.0, 100.0, -100.0]);
    let mut last: Option<f64> = None;
    for deg in 0..=720 {
        let rad = (f64::from(deg) / 2.0).to_radians();
        let rgb = lin([0.5 + 0.3 * rad.cos(), 0.5 + 0.3 * (rad - 2.094_395).cos(), 0.5 + 0.3 * (rad + 2.094_395).cos()]);
        let v = apply_pixel(&alt, rgb).unwrap()[0];
        if let Some(prev) = last {
            assert!((v - prev).abs() < 0.06, "jump at {deg}: {prev} -> {v}");
        }
        last = Some(v);
    }
    // Hostile bands and group gating at recipe level.
    assert_eq!(apply_pixel(&mix(true, &|b| b[3] = 100.5), red).err(), Some(DevelopError::InvalidAdjustment));
    assert_eq!(apply_pixel(&mix(true, &|b| b[7] = f64::NAN), red).err(), Some(DevelopError::NonFinite));
    let mut r = DevelopRecipe::native([1.0; 3], [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], profile());
    r.adjust.grayscale.convert = true;
    let plane = flat_plane(16, 16, 1500);
    let colour_plane = tinted_plane(16, 16);
    let render = |r: &DevelopRecipe, p: &CfaPlane| develop_to_image(p, r, 1, &CancellationToken::default()).unwrap().0;
    let plain = render(&DevelopRecipe::native([1.0; 3], [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], profile()), &colour_plane);
    assert_eq!(render(&r, &colour_plane), plain, "enable_grayscale_mix off: the conversion is retained but not applied");
    r.enables.enable_grayscale_mix = true;
    let converted = render(&r, &colour_plane);
    assert!(converted.rgb.chunks_exact(3).all(|p| p[0] == p[1] && p[1] == p[2]));
    assert_ne!(converted, plain);
    let _ = plane;
}

fn flat_plane(w: u32, h: u32, sample: u16) -> CfaPlane {
    plane_from(w, h, |_, _| sample)
}

/// A frame whose R/G/B sites differ, so the developed image has colour.
fn tinted_plane(w: u32, h: u32) -> CfaPlane {
    plane_from(w, h, |x, y| match (x % 2, y % 2) {
        (0, 0) => 3000,
        (1, 1) => 800,
        _ => 1500,
    })
}

fn plane_from(w: u32, h: u32, f: impl Fn(u32, u32) -> u16) -> CfaPlane {
    let bytes: Vec<u8> = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).flat_map(|(x, y)| f(x, y).to_le_bytes()).collect();
    CfaPlane::with_limits(
        PlaneInit {
            width: w,
            height: h,
            stride_bytes: w * 2,
            sensor_origin: (0, 0),
            cfa: Cfa { pattern: CfaPattern::Bayer(Bayer::Rggb), phase_x: 0, phase_y: 0 },
            black: [0; 3],
            white: [4095; 3],
            source_revision: 1,
            bytes,
        },
        Limits::LARGE,
    )
    .unwrap()
}

#[test]
fn colour_grading_regions_blending_and_balance() {
    let wheel = |hue: f64, saturation: f64, luminance: f64| Wheel { hue, saturation, luminance };
    // Identity: wheels without saturation or luminance do nothing, whatever hue/blending/balance.
    let idle = with(|a| {
        a.grading.shadows = wheel(200.0, 0.0, 0.0);
        a.grading.highlights = wheel(40.0, 0.0, 0.0);
        a.grading.blending = 100.0;
        a.grading.balance = -100.0;
    });
    let px = lin([0.3, 0.5, 0.2]);
    assert_eq!(apply_pixel(&idle, px).unwrap(), px);
    let dark = lin([0.12, 0.12, 0.12]);
    let mid = lin([0.5, 0.5, 0.5]);
    let bright = lin([0.92, 0.92, 0.92]);
    let shift = |a: &Adjustments, p: [f64; 3]| {
        let out = apply_pixel(a, p).unwrap();
        // Encoded-domain red minus blue: a red tint raises it, a blue tint lowers it.
        let enc = |c: f64| if c <= 0.003_130_8 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
        enc(out[0]) - enc(out[2])
    };
    // Shadows wheel (red): strong in the dark, weak in the middle, absent in the highlights.
    let sh = with(|a| a.grading.shadows = wheel(0.0, 100.0, 0.0));
    let (s_dark, s_mid, s_bright) = (shift(&sh, dark), shift(&sh, mid), shift(&sh, bright));
    assert!(s_dark > 0.1 && s_dark > s_mid && s_mid >= s_bright && s_bright.abs() < 1e-6, "{s_dark} {s_mid} {s_bright}");
    // Highlights wheel (blue, hue 240): the mirror image.
    let hi = with(|a| a.grading.highlights = wheel(240.0, 100.0, 0.0));
    let (h_dark, h_bright) = (shift(&hi, dark), shift(&hi, bright));
    assert!(h_bright < -0.1 && h_dark.abs() < 1e-6, "{h_dark} {h_bright}");
    // Midtones wheel peaks in the middle; global touches every tone equally.
    let mt = with(|a| a.grading.midtones = wheel(0.0, 100.0, 0.0));
    assert!(shift(&mt, mid) > shift(&mt, dark) && shift(&mt, mid) > shift(&mt, bright));
    let gl = with(|a| a.grading.global = wheel(0.0, 100.0, 0.0));
    let g = [shift(&gl, dark), shift(&gl, mid), shift(&gl, bright)];
    assert!((g[0] - g[1]).abs() < 1e-9 && (g[1] - g[2]).abs() < 1e-9 && g[0] > 0.1, "{g:?}");
    // Luminance of a wheel lifts / lowers its region.
    let lift = with(|a| a.grading.shadows = wheel(0.0, 0.0, 100.0));
    assert!(apply_pixel(&lift, dark).unwrap()[1] > dark[1]);
    assert!((apply_pixel(&lift, bright).unwrap()[1] - bright[1]).abs() < 1e-9);
    // Balance toward highlights widens the shadow region; toward shadows narrows it.
    let probe = lin([0.55, 0.55, 0.55]);
    let balanced = |b: f64| shift(&with(|a| { a.grading.shadows = wheel(0.0, 100.0, 0.0); a.grading.balance = b; }), probe);
    assert!(balanced(100.0) > balanced(0.0) && balanced(0.0) > balanced(-100.0), "{} {} {}", balanced(100.0), balanced(0.0), balanced(-100.0));
    // Blending widens the overlap: at a tone between the regions, the shadow wheel reaches further.
    let blended = |b: f64| shift(&with(|a| { a.grading.shadows = wheel(0.0, 100.0, 0.0); a.grading.blending = b; }), lin([0.45, 0.45, 0.45]));
    assert!(blended(100.0) > blended(0.0), "{} {}", blended(100.0), blended(0.0));
    // Region weights are a partition of unity: a uniform global+three-region stack equals global alone.
    let all = with(|a| {
        a.grading.shadows = wheel(0.0, 100.0, 0.0);
        a.grading.midtones = wheel(0.0, 100.0, 0.0);
        a.grading.highlights = wheel(0.0, 100.0, 0.0);
    });
    for p in [dark, mid, bright, probe] {
        assert!((shift(&all, p) - shift(&gl, p)).abs() < 1e-9, "weights must sum to 1 at {p:?}");
    }
    // Hostile.
    let bad = |f: &dyn Fn(&mut ColourGrading), want: DevelopError| {
        let a = with(|a| f(&mut a.grading));
        assert_eq!(apply_pixel(&a, px).err(), Some(want));
    };
    bad(&|g| g.shadows.hue = 360.5, DevelopError::InvalidAdjustment);
    bad(&|g| g.shadows.hue = -1.0, DevelopError::InvalidAdjustment);
    bad(&|g| g.midtones.saturation = 100.5, DevelopError::InvalidAdjustment);
    bad(&|g| g.highlights.saturation = -0.5, DevelopError::InvalidAdjustment);
    bad(&|g| g.global.luminance = 101.0, DevelopError::InvalidAdjustment);
    bad(&|g| g.blending = 100.5, DevelopError::InvalidAdjustment);
    bad(&|g| g.balance = f64::NAN, DevelopError::NonFinite);
    bad(&|g| g.global.hue = f64::INFINITY, DevelopError::NonFinite);
    // Group gating at recipe level: colour grading needs enable_colour_adjustments.
    let mut r = DevelopRecipe::native([1.0; 3], [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], profile());
    r.adjust.grading.global = wheel(0.0, 100.0, 0.0);
    let plane = tinted_plane(16, 16);
    let render = |r: &DevelopRecipe| develop_to_image(&plane, r, 1, &CancellationToken::default()).unwrap().0;
    let plain = render(&DevelopRecipe::native([1.0; 3], [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], profile()));
    assert_eq!(render(&r), plain);
    r.enables.enable_colour_adjustments = true;
    assert_ne!(render(&r), plain);
}

#[test]
fn version_1_0_recipes_still_read_and_corpus_pin() {
    // A 1.0 recipe (no 1.1 families set) is accepted; an unknown version is refused.
    let mut r = DevelopRecipe::native([1.0; 3], [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], profile());
    assert_eq!(r.adjust.version, ADJUST_VERSION);
    r.adjust.version = ADJUST_MIN_VERSION;
    r.validate().unwrap();
    r.adjust.version = (1, 2);
    assert_eq!(r.validate().err(), Some(DevelopError::InvalidAdjustment));
    r.adjust.version = (0, 9);
    assert_eq!(r.validate().err(), Some(DevelopError::InvalidAdjustment));

    const TEST: &str = "version_1_0_recipes_still_read_and_corpus_pin";
    let Some(bytes) = corpus_file(TEST, "canon-eos-5d3-mlv-14bit-lossless.dng", "1d77dcc6cdb839b10c1948f0b4471984ab48cf15e973fb77520584cee72b603e") else {
        return;
    };
    let raw = decode_container(&bytes).unwrap();
    let (plane, _) = raw.cfa_plane(raw.default_window(), RangePolicy::ClampReported, 1, Limits::LARGE).unwrap();
    let (mut recipe, _) = raw
        .recipe_for_white(&binding_srgb(), profile(), BaselineExposure::Ignore, 4800.0, 12.0)
        .unwrap();
    recipe.enables.enable_colour_adjustments = true;
    recipe.enables.enable_grayscale_mix = false;
    recipe.adjust.point_colours = vec![{
        let mut p = PointColour::new([0.45, 0.55, 0.35]);
        p.saturation = 60.0;
        p.hue_shift = -20.0;
        p
    }];
    recipe.adjust.grading.shadows = Wheel { hue: 215.0, saturation: 35.0, luminance: -10.0 };
    recipe.adjust.grading.highlights = Wheel { hue: 40.0, saturation: 25.0, luminance: 5.0 };
    recipe.adjust.grading.blending = 60.0;
    recipe.adjust.grading.balance = -15.0;
    let a = develop_to_image_threads(&plane, &recipe, 1, Threads::single(), &CancellationToken::default()).unwrap().0;
    let b = develop_to_image_threads(&plane, &recipe, 1, Threads::new(5).unwrap(), &CancellationToken::default()).unwrap().0;
    assert_eq!(a, b, "thread count does not change the colour families");
    let mut gray = recipe.clone();
    gray.enables.enable_grayscale_mix = true;
    gray.adjust.grayscale = GrayscaleMix { convert: true, bands: [20.0, 10.0, -10.0, -30.0, 0.0, 40.0, 0.0, 0.0] };
    let g = develop_to_image(&plane, &gray, 1, &CancellationToken::default()).unwrap().0;
    assert!(a.rgb.iter().all(|v| v.is_finite()) && g.rgb.iter().all(|v| v.is_finite()));
    let digest = |im: &DevelopedImage| {
        let mut h = Sha256::new();
        for v in &im.rgb {
            h.update(v.to_le_bytes());
        }
        hex(&h.finalize())
    };
    println!("corpus colour2 colour digest={} gray digest={}", digest(&a), digest(&g));
    assert_eq!(digest(&a), PIN_COLOUR);
    assert_eq!(digest(&g), PIN_GRAY);
}

const PIN_COLOUR: &str = "";
const PIN_GRAY: &str = "";
