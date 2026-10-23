//! Lane SMB-DEVELOP-CONTROLS slice 1 and 2: global Basic panel, colour mixer, parametric regions.
//! Lightroom-matching values are UNVERIFIED (D5); these tests prove this crate's documented maths,
//! monotonicity, identity, refusals and a corpus digest pin, never parity.
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_develop::*;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn profile() -> ProfileBinding {
    ProfileBinding {
        profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(),
        sha256: [7; 32],
    }
}

fn sweep() -> Vec<f64> {
    // From deep negative (below the black knot) through HDR highlights, on a fine grid.
    (-40..=4000).map(|i| f64::from(i) / 1000.0).collect()
}

fn grey(v: f64) -> [f64; 3] {
    [v, v, v]
}

fn tone(set: impl FnOnce(&mut BasicTone)) -> Adjustments {
    let mut a = Adjustments::default();
    set(&mut a.tone);
    a
}

fn y(a: &Adjustments, v: f64) -> f64 {
    apply_pixel(a, grey(v)).unwrap()[1]
}

#[test]
fn tone_controls_are_monotone_directional_and_identity_at_zero() {
    // Identity: all zero is exact on the whole sweep.
    let id = Adjustments::default();
    for v in sweep() {
        assert_eq!(apply_pixel(&id, grey(v)).unwrap(), grey(v), "identity at {v}");
    }
    // Every control at +-100 and a worst-case stack keeps the response monotone nondecreasing.
    type Setter = fn(&mut BasicTone, f64);
    let setters: [(&str, Setter); 5] = [
        ("contrast", |t, v| t.contrast = v),
        ("highlights", |t, v| t.highlights = v),
        ("shadows", |t, v| t.shadows = v),
        ("whites", |t, v| t.whites = v),
        ("blacks", |t, v| t.blacks = v),
    ];
    for (name, set) in setters {
        for amount in [-100.0, -37.0, 37.0, 100.0] {
            let a = tone(|t| set(t, amount));
            let mut last = f64::NEG_INFINITY;
            for v in sweep() {
                let out = y(&a, v);
                assert!(out.is_finite() && out >= last - 1e-12, "{name} {amount} not monotone at {v}: {out} < {last}");
                last = out;
            }
        }
    }
    for signs in [[1.0, 1.0, 1.0, 1.0, 1.0], [-1.0, -1.0, -1.0, -1.0, -1.0], [1.0, -1.0, 1.0, -1.0, 1.0], [-1.0, 1.0, -1.0, 1.0, -1.0]] {
        let a = tone(|t| {
            t.contrast = 100.0 * signs[0];
            t.highlights = 100.0 * signs[1];
            t.shadows = 100.0 * signs[2];
            t.whites = 100.0 * signs[3];
            t.blacks = 100.0 * signs[4];
        });
        let mut last = f64::NEG_INFINITY;
        for v in sweep() {
            let out = y(&a, v);
            assert!(out >= last - 1e-12, "stack {signs:?} not monotone at {v}");
            last = out;
        }
    }
    // Direction (region by region): linear 0.05 is deep shadow, 0.18 mid-dark, 0.9 highlight.
    assert!(y(&tone(|t| t.contrast = 100.0), 0.05) < 0.05 && y(&tone(|t| t.contrast = 100.0), 0.9) > 0.9);
    assert!(y(&tone(|t| t.contrast = -100.0), 0.05) > 0.05 && y(&tone(|t| t.contrast = -100.0), 0.9) < 0.9);
    assert!(y(&tone(|t| t.shadows = 100.0), 0.05) > 0.05, "shadows lift the shadows");
    assert!(y(&tone(|t| t.shadows = -100.0), 0.05) < 0.05);
    assert!(y(&tone(|t| t.highlights = -100.0), 0.7) < 0.7, "highlights recover the highlights");
    assert!(y(&tone(|t| t.highlights = 100.0), 0.7) > 0.7);
    assert!(y(&tone(|t| t.blacks = 100.0), 0.0) > 0.0, "blacks +100 lifts the black point");
    assert!(y(&tone(|t| t.blacks = -100.0), 0.0) < 0.0, "blacks -100 crushes below zero (clipped at export)");
    assert!(y(&tone(|t| t.whites = 100.0), 1.0) > 1.0 && y(&tone(|t| t.whites = -100.0), 1.0) < 1.0);
    // A region control barely moves the opposite end: whites leave deep shadows alone.
    assert!((y(&tone(|t| t.whites = 100.0), 0.01) - 0.01).abs() < 1e-3);
    // Parametric regions: direction, identity of the splits, ordering refusal.
    let mut p = Adjustments::default();
    p.parametric.shadows = 100.0;
    assert!(y(&p, 0.05) > 0.05 && (y(&p, 0.9) - 0.9).abs() < 1e-3);
    let mut p = Adjustments::default();
    p.parametric.highlights = -100.0;
    p.parametric.lights = -100.0;
    assert!(y(&p, 0.7) < 0.7);
    let mut last = f64::NEG_INFINITY;
    for v in sweep() {
        let out = y(&p, v);
        assert!(out >= last - 1e-12);
        last = out;
    }
    let mut bad = Adjustments::default();
    bad.parametric.shadow_split = 60.0; // above the midtone split
    assert_eq!(apply_pixel(&bad, grey(0.5)), Err(DevelopError::InvalidAdjustment));
}

#[test]
fn white_balance_shift_adapts_neutral_and_planck_locus_matches_references() {
    // Kim et al. spline vs published Planckian chromaticities (CIE illuminant A = 2856 K Planck:
    // 0.4476, 0.4074; 6500 K: 0.3135, 0.3237) and a round trip through the Robertson CCT solver.
    let a = planck_xy(2856.0).unwrap();
    assert!((a[0] - 0.4476).abs() < 2e-3 && (a[1] - 0.4074).abs() < 2e-3, "{a:?}");
    let d = planck_xy(6500.0).unwrap();
    assert!((d[0] - 0.3135).abs() < 2e-3 && (d[1] - 0.3237).abs() < 2e-3, "{d:?}");
    for k in [3000.0, 4000.0, 5003.0, 6500.0, 10000.0] {
        let cct = color::cct_from_xy(planck_xy(k).unwrap()).unwrap();
        assert!((cct - k).abs() / k < 0.01, "{k} -> {cct}");
    }
    assert_eq!(planck_xy(1000.0), Err(DevelopError::InvalidAdjustment));
    assert_eq!(planck_xy(f64::NAN), Err(DevelopError::InvalidAdjustment));

    let neutral = grey(0.5);
    let shifted = |t: f64, tint: f64| {
        let a = Adjustments { white_balance: WhiteBalanceShift { temperature: t, tint }, ..Adjustments::default() };
        apply_pixel(&a, neutral).unwrap()
    };
    assert_eq!(shifted(0.0, 0.0), neutral);
    // Warmer: red up, blue down, green nearly steady; cooler: the reverse.
    let warm = shifted(60.0, 0.0);
    assert!(warm[0] > 0.5 && warm[2] < 0.5, "{warm:?}");
    let cool = shifted(-60.0, 0.0);
    assert!(cool[0] < 0.5 && cool[2] > 0.5, "{cool:?}");
    // Monotone in the slider.
    let mut last = f64::NEG_INFINITY;
    for t in (-100..=100).step_by(10) {
        let r = shifted(f64::from(t), 0.0);
        assert!(r[0] > last, "red must rise with temperature, {t}");
        last = r[0];
    }
    // Tint +: magenta (green down relative to red/blue); tint -: green up.
    let magenta = shifted(0.0, 60.0);
    assert!(magenta[1] < magenta[0] && magenta[1] < magenta[2], "{magenta:?}");
    let green = shifted(0.0, -60.0);
    assert!(green[1] > green[0] && green[1] > green[2], "{green:?}");
    // The adaptation is a single matrix: it is linear (scaling the input scales the output) and
    // keeps black black.
    let a = Adjustments { white_balance: WhiteBalanceShift { temperature: 40.0, tint: -20.0 }, ..Adjustments::default() };
    let one = apply_pixel(&a, [0.2, 0.4, 0.1]).unwrap();
    let two = apply_pixel(&a, [0.4, 0.8, 0.2]).unwrap();
    for i in 0..3 {
        assert!((two[i] - 2.0 * one[i]).abs() < 1e-9 * two[i].abs().max(1.0));
    }
    assert_eq!(apply_pixel(&a, [0.0; 3]).unwrap(), [0.0; 3]);
}

#[test]
fn vibrance_saturation_and_mixer_bands() {
    let sat = |s: f64, v: f64, rgb: [f64; 3]| {
        let mut a = Adjustments::default();
        a.presence.saturation = s;
        a.presence.vibrance = v;
        apply_pixel(&a, rgb).unwrap()
    };
    let colour = [0.40, 0.18, 0.08];
    // saturation -100 collapses to a grey of the encoded luma; +100 widens the channel spread.
    let flat = sat(-100.0, 0.0, colour);
    assert!((flat[0] - flat[1]).abs() < 1e-9 && (flat[1] - flat[2]).abs() < 1e-9, "{flat:?}");
    let wide = sat(100.0, 0.0, colour);
    assert!(wide[0] - wide[2] > colour[0] - colour[2]);
    // Greys are fixed points of every colour control.
    for v in [-100.0, 50.0, 100.0] {
        for g in [0.0, 0.02, 0.18, 0.5, 1.0, 3.0] {
            let out = sat(v, v, grey(g));
            assert!((out[0] - g).abs() < 1e-12 * g.max(1.0) && (out[1] - g).abs() < 1e-12 * g.max(1.0), "{out:?} {g}");
        }
    }
    // Vibrance helps a muted colour more than a saturated one (same slider).
    let muted = [0.30, 0.26, 0.22];
    let vivid = [0.60, 0.05, 0.02];
    let spread = |rgb: [f64; 3]| rgb[0].max(rgb[1]).max(rgb[2]) - rgb[0].min(rgb[1]).min(rgb[2]);
    let gain_muted = spread(sat(0.0, 80.0, muted)) / spread(muted);
    let gain_vivid = spread(sat(0.0, 80.0, vivid)) / spread(vivid);
    assert!(gain_muted > gain_vivid && gain_vivid >= 1.0, "{gain_muted} {gain_vivid}");
    // Negative colour values skip the colour stage (hue undefined) and stay finite.
    assert_eq!(sat(100.0, 100.0, [0.4, -0.1, 0.2]), [0.4, -0.1, 0.2]);

    // Mixer: eight named bands, saturation on the green band moves green and leaves red/blue alone.
    assert_eq!(BAND_NAMES.len(), 8);
    assert_eq!(BAND_NAMES[3], "green");
    let mixer = |band: usize, hue: f64, saturation: f64, luminance: f64, rgb: [f64; 3]| {
        let mut a = Adjustments::default();
        a.mixer.bands[band] = HslBand { hue, saturation, luminance };
        apply_pixel(&a, rgb).unwrap()
    };
    let green = [0.10, 0.40, 0.12];
    let red = [0.50, 0.08, 0.06];
    let blue = [0.06, 0.10, 0.50];
    let more = mixer(3, 0.0, 100.0, 0.0, green);
    assert!(more[1] - more[0] > green[1] - green[0], "green band saturation raises green chroma");
    let near = |a: [f64; 3], b: [f64; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9);
    assert!(near(mixer(3, 0.0, 100.0, 0.0, red), red), "an unrelated band leaves red alone");
    assert!(near(mixer(3, 100.0, -100.0, 50.0, blue), blue), "and blue");
    // Luminance on the red band brightens red, darkens with a negative slider.
    assert!(mixer(0, 0.0, 0.0, 100.0, red)[0] > red[0] && mixer(0, 0.0, 0.0, -100.0, red)[0] < red[0]);
    // Hue shift rotates by BAND_HUE_DEGREES at 100: red 0 degrees moves toward orange (+hue).
    let rotated = mixer(0, 100.0, 0.0, 0.0, [0.5, 0.02, 0.02]);
    assert!(rotated[1] > 0.02, "red hue +100 gains green: {rotated:?}");
    // The eight band weights partition unity at any hue: the response is continuous across all
    // band centres (no jump between neighbouring hues).
    let mut a = Adjustments::default();
    for (i, b) in a.mixer.bands.iter_mut().enumerate() {
        b.saturation = if i % 2 == 0 { 100.0 } else { -100.0 };
    }
    let mut last: Option<[f64; 3]> = None;
    for deg in 0..=720 {
        let h = f64::from(deg) / 2.0;
        let rad = h.to_radians();
        let rgb = [0.3 + 0.2 * rad.cos(), 0.3 + 0.2 * (rad - 2.094_395).cos(), 0.3 + 0.2 * (rad + 2.094_395).cos()];
        let out = apply_pixel(&a, rgb).unwrap();
        if let Some(prev) = last {
            for i in 0..3 {
                assert!((out[i] - prev[i]).abs() < 0.08, "jump at hue {h}: {prev:?} -> {out:?}");
            }
        }
        last = Some(out);
    }
}

fn plane(width: u32, height: u32, f: impl Fn(u32, u32) -> u16) -> CfaPlane {
    let samples: Vec<u8> = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .flat_map(|(x, y)| f(x, y).to_le_bytes())
        .collect();
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
            bytes: samples,
        },
        Limits::LARGE,
    )
    .unwrap()
}

fn base_recipe() -> DevelopRecipe {
    DevelopRecipe::native([1.0; 3], [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], profile())
}

fn image(plane: &CfaPlane, recipe: &DevelopRecipe) -> DevelopedImage {
    develop_to_image(plane, recipe, 1, &CancellationToken::default()).unwrap().0
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
    let path: PathBuf = root.join("dng").join(file);
    let Ok(bytes) = std::fs::read(&path) else {
        return reason("file_absent".into());
    };
    let got = hex(&Sha256::digest(&bytes));
    if got != sha256 {
        return reason(format!("sha256_mismatch got={got}"));
    }
    Some(bytes)
}

fn f32_digest(rgb: &[f32]) -> String {
    let mut h = Sha256::new();
    for v in rgb {
        h.update(v.to_le_bytes());
    }
    hex(&h.finalize())
}

#[test]
fn clarity_is_whole_frame_local_contrast_and_pinned_on_the_corpus_dng() {
    // Flat frame: nothing to enhance, output unchanged for any clarity.
    let flat = plane(64, 48, |_, _| 1500);
    let mut r = base_recipe();
    r.enables.enable_detail = true;
    let reference = image(&flat, &base_recipe());
    for c in [-100.0, 100.0] {
        r.adjust.presence.clarity = c;
        assert_eq!(image(&flat, &r).rgb.iter().map(|v| v.to_bits()).collect::<Vec<_>>(), reference.rgb.iter().map(|v| v.to_bits()).collect::<Vec<_>>());
    }
    // Step edge: positive clarity pushes the two sides apart, negative pulls them together.
    let step = plane(64, 48, |x, _| if x < 32 { 1200 } else { 1800 });
    let plain = image(&step, &base_recipe());
    let at = |im: &DevelopedImage, x: usize, y: usize| im.rgb[(y * im.width as usize + x) * 3 + 1];
    r.adjust.presence.clarity = 100.0;
    let sharp = image(&step, &r);
    r.adjust.presence.clarity = -100.0;
    let soft = image(&step, &r);
    let (left, right) = (30, 33);
    let gap = |im: &DevelopedImage| at(im, right, 24) - at(im, left, 24);
    assert!(gap(&sharp) > gap(&plain) && gap(&soft) < gap(&plain), "{} {} {}", gap(&sharp), gap(&plain), gap(&soft));
    // Disabled group: values retained but ignored (bytes equal the plain render).
    let mut off = base_recipe();
    off.adjust.presence.clarity = 100.0;
    assert_eq!(image(&step, &off), plain);
    // The tile API cannot run a whole-frame stage: typed refusal, no tile delivered.
    let mut calls = 0;
    let refused = develop(
        &DevelopRequest {
            plane: &step,
            recipe: &{
                let mut c = base_recipe();
                c.enables.enable_detail = true;
                c.adjust.presence.clarity = 10.0;
                c
            },
            recipe_revision: 1,
            expected_source_revision: 1,
            grid: hsk_studio_pigment::Grid { origin_x: 0, origin_y: 0, tile_width: 32, tile_height: 32 },
            output_origin: (0, 0),
            execution_hint: ExecutionHint::Scalar,
        },
        &CancellationToken::default(),
        &mut |_| {
            calls += 1;
            Ok(())
        },
    );
    assert_eq!(refused.err(), Some(DevelopError::UnsupportedContribution("clarity_whole_frame")));
    assert_eq!(calls, 0);
    // Direct function: wrong length and non-finite input refuse; the buffer is untouched.
    let mut buf = vec![0.5f32; 3 * 4 * 4];
    assert_eq!(clarity(&mut buf, 4, 4, 100.0, adjust::REC709_LUMA), Ok(()));
    assert_eq!(clarity(&mut buf[..10], 4, 4, 100.0, adjust::REC709_LUMA), Err(DevelopError::InvalidDimensions));
    buf[7] = f32::NAN;
    assert_eq!(clarity(&mut buf, 4, 4, 100.0, adjust::REC709_LUMA), Err(DevelopError::NonFinite));
    assert_eq!(clarity(&mut [0.5; 3], 1, 1, 101.0, adjust::REC709_LUMA), Err(DevelopError::InvalidAdjustment));

    // Corpus DNG (Canon EOS 5D Mark III MLV export, CC0): full Basic stack, digest pinned.
    const TEST: &str = "clarity_is_whole_frame_local_contrast_and_pinned_on_the_corpus_dng";
    let Some(bytes) = corpus_file(TEST, "canon-eos-5d3-mlv-14bit-lossless.dng", "1d77dcc6cdb839b10c1948f0b4471984ab48cf15e973fb77520584cee72b603e") else {
        return;
    };
    let raw = decode_container(&bytes).unwrap();
    let window = raw.default_window();
    let (cfa, _) = raw.cfa_plane(window, RangePolicy::ClampReported, 1, Limits::LARGE).unwrap();
    let (mut recipe, _) = raw
        .recipe(&color::xyz_d50_to_linear_srgb().unwrap(), profile(), BaselineExposure::Ignore)
        .unwrap();
    recipe.enables.enable_colour_adjustments = true;
    recipe.enables.enable_detail = true;
    recipe.enables.enable_tone_curve = true;
    recipe.exposure_stops = 0.5;
    recipe.adjust.white_balance = WhiteBalanceShift { temperature: 25.0, tint: -10.0 };
    recipe.adjust.tone = BasicTone { contrast: 30.0, highlights: -40.0, shadows: 35.0, whites: 10.0, blacks: -15.0 };
    recipe.adjust.presence = Presence { vibrance: 30.0, saturation: 10.0, clarity: 40.0 };
    recipe.adjust.mixer.bands[3] = HslBand { hue: 10.0, saturation: 25.0, luminance: 10.0 };
    recipe.adjust.parametric.darks = 20.0;
    let plain = image(&cfa, &{
        let mut p = recipe.clone();
        p.adjust = Adjustments::default();
        p.exposure_stops = 0.5;
        p
    });
    let developed = image(&cfa, &recipe);
    let again = develop_to_image_threads(&cfa, &recipe, 1, Threads::single(), &CancellationToken::default()).unwrap().0;
    assert_eq!(developed, again, "thread count does not change the controls output");
    assert_ne!(developed.rgb, plain.rgb);
    assert!(developed.rgb.iter().all(|v| v.is_finite()));
    let digest = f32_digest(&developed.rgb);
    println!("corpus basic-stack {}x{} digest={digest}", developed.width, developed.height);
    assert_eq!(digest, PIN_BASIC_STACK, "corpus digest pin");
}

const PIN_BASIC_STACK: &str = "46f8f086caf475b03a2e651c3b20416df5ffb51356b56ab589fa29d8eb0355fc";

#[test]
fn identity_default_gating_and_hostile_parameters() {
    let step = plane(32, 32, |x, y| 800 + 40 * (x as u16) + 20 * (y as u16));
    let baseline = image(&step, &base_recipe());
    // Authored values stay in the recipe while their group is off; the output equals the baseline
    // for colour (enable_colour_adjustments off) and regions (enable_tone_curve off).
    let mut r = base_recipe();
    r.adjust.presence.vibrance = 80.0;
    r.adjust.presence.saturation = -50.0;
    r.adjust.mixer.bands[0].hue = 40.0;
    r.adjust.parametric.shadows = 70.0;
    let before = r.clone();
    assert_eq!(image(&step, &r), baseline);
    assert_eq!(r, before, "develop never rewrites the recipe");
    r.enables.enable_colour_adjustments = true;
    assert_ne!(image(&step, &r), baseline);
    // Always-on controls (white balance shift and basic tone) need no group flag.
    let mut t = base_recipe();
    t.adjust.tone.contrast = 50.0;
    assert_ne!(image(&step, &t), baseline);

    // Hostile parameters: every slider, NaN / infinity / just past the soft range, each typed.
    type Poke = fn(&mut Adjustments, f64);
    let pokes: Vec<(&str, Poke)> = vec![
        ("temperature", |a, v| a.white_balance.temperature = v),
        ("tint", |a, v| a.white_balance.tint = v),
        ("contrast", |a, v| a.tone.contrast = v),
        ("highlights", |a, v| a.tone.highlights = v),
        ("shadows", |a, v| a.tone.shadows = v),
        ("whites", |a, v| a.tone.whites = v),
        ("blacks", |a, v| a.tone.blacks = v),
        ("vibrance", |a, v| a.presence.vibrance = v),
        ("saturation", |a, v| a.presence.saturation = v),
        ("clarity", |a, v| a.presence.clarity = v),
        ("band hue", |a, v| a.mixer.bands[7].hue = v),
        ("band sat", |a, v| a.mixer.bands[2].saturation = v),
        ("band lum", |a, v| a.mixer.bands[5].luminance = v),
        ("parametric", |a, v| a.parametric.lights = v),
    ];
    for (name, poke) in pokes {
        for (bad, want) in [
            (f64::NAN, DevelopError::NonFinite),
            (f64::INFINITY, DevelopError::NonFinite),
            (100.000_001, DevelopError::InvalidAdjustment),
            (-1.0e300, DevelopError::InvalidAdjustment),
        ] {
            let mut rec = base_recipe();
            poke(&mut rec.adjust, bad);
            assert_eq!(rec.validate().err(), Some(want), "{name} = {bad}");
            assert_eq!(apply_pixel(&rec.adjust, grey(0.5)).err(), Some(want), "{name} = {bad}");
            // a refused recipe delivers no tile
            let mut calls = 0;
            let result = develop(
                &DevelopRequest {
                    plane: &step,
                    recipe: &rec,
                    recipe_revision: 1,
                    expected_source_revision: 1,
                    grid: hsk_studio_pigment::Grid { origin_x: 0, origin_y: 0, tile_width: 16, tile_height: 16 },
                    output_origin: (0, 0),
                    execution_hint: ExecutionHint::Scalar,
                },
                &CancellationToken::default(),
                &mut |_| {
                    calls += 1;
                    Ok(())
                },
            );
            assert_eq!(result.err(), Some(want));
            assert_eq!(calls, 0);
        }
        let mut edge = base_recipe();
        poke(&mut edge.adjust, 100.0);
        edge.validate().unwrap();
        poke(&mut edge.adjust, -100.0);
        edge.validate().unwrap();
    }
    // Structural refusals: version, luma, working matrix, non-finite pixel.
    let mut v = base_recipe();
    v.adjust.version = (9, 9);
    assert_eq!(v.validate().err(), Some(DevelopError::InvalidAdjustment));
    let mut l = base_recipe();
    l.adjust.luma = [0.5, 0.5, 0.5];
    assert_eq!(l.validate().err(), Some(DevelopError::InvalidAdjustment));
    let mut m = base_recipe();
    m.adjust.white_balance.temperature = 10.0;
    m.adjust.working_to_xyz_d50 = [0.0; 9];
    assert_eq!(m.validate().err(), Some(DevelopError::NotInvertibleMatrix));
    assert_eq!(apply_pixel(&Adjustments::default(), [f64::NAN, 0.0, 0.0]), Err(DevelopError::NonFinite));
    assert_eq!(DevelopError::InvalidAdjustment.code(), "invalid_adjustment");
}
