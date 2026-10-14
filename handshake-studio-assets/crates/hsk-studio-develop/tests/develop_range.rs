//! Lane SMB-DEVELOP-CONTROLS-2 slice C: luminance and colour range masks in the local stack
//! (STU-RAW-121 `luminance_range` / `colour_range`). Lightroom-matching values are UNVERIFIED (D5).
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

fn enc(c: f64) -> f64 {
    if c <= 0.003_130_8 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
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

fn recipe(corrections: Vec<Correction>) -> DevelopRecipe {
    let mut r = DevelopRecipe::native([1.0; 3], [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], profile());
    r.enables.enable_mask_group_based_corrections = true;
    r.adjust.local = corrections;
    r
}

fn render(plane: &CfaPlane, r: &DevelopRecipe, threads: Threads) -> Result<DevelopedImage, DevelopError> {
    develop_to_image_threads(plane, r, 1, threads, &CancellationToken::default()).map(|v| v.0)
}

fn g(im: &DevelopedImage, x: usize, y: usize) -> f32 {
    im.rgb[(y * im.width as usize + x) * 3 + 1]
}

fn rgb(im: &DevelopedImage, x: usize, y: usize) -> [f32; 3] {
    let i = (y * im.width as usize + x) * 3;
    [im.rgb[i], im.rgb[i + 1], im.rgb[i + 2]]
}

fn mask(source: MaskSource) -> Mask {
    Mask { id: "m".into(), active: true, inverted: false, value: 1.0, blend: MaskBlend::Add, source }
}

fn one_stop() -> LocalStack {
    LocalStack { exposure: 0.25, ..LocalStack::default() }
}

/// Grey ramp: the raw sample rises with x, so encoded luma rises with x.
fn ramp() -> CfaPlane {
    plane_from(64, 16, |x, _| 200 + 56 * x as u16)
}

fn luma_at(plain: &DevelopedImage, x: usize) -> f64 {
    enc(f64::from(g(plain, x, 8)))
}

#[test]
fn luminance_range_selects_by_the_pixels_own_luma() {
    let plane = ramp();
    let plain = render(&plane, &recipe(Vec::new()), Threads::single()).unwrap();
    let range = |lo, hi, feather| mask(MaskSource::LuminanceRange(LuminanceRange { lo, hi, feather }));
    let run = |m: Mask| render(&plane, &recipe(vec![Correction::new("c", vec![m], one_stop())]), Threads::single()).unwrap();
    // Hard window: exactly the pixels whose encoded luma lies in [0.35, 0.65] get +1 stop.
    let hard = run(range(0.35, 0.65, 0.0));
    let (mut inside, mut outside) = (0, 0);
    for x in 0..64 {
        let l = luma_at(&plain, x);
        if (l - 0.35).abs() < 0.01 || (l - 0.65).abs() < 0.01 {
            continue; // the test measures luma from one channel; the mask uses the weighted luma
        }
        if (0.35..=0.65).contains(&l) {
            inside += 1;
            assert_eq!(g(&hard, x, 8), g(&plain, x, 8) * 2.0, "x {x} luma {l}");
        } else {
            outside += 1;
            assert_eq!(g(&hard, x, 8), g(&plain, x, 8), "x {x} luma {l}");
        }
    }
    assert!(inside > 5 && outside > 20, "{inside} {outside}");
    // Feather: soft edges, coverage monotone up into the window and down out of it, and bounded.
    let soft = run(range(0.35, 0.65, 1.0));
    let cover = |im: &DevelopedImage, x: usize| f64::from(g(im, x, 8) / g(&plain, x, 8)) - 1.0;
    let mut partial = 0;
    let mut previous = -1.0;
    let first_hi = (0..64).find(|x| luma_at(&plain, *x) > 0.5).unwrap();
    for x in 0..first_hi {
        let c = cover(&soft, x);
        assert!((-1e-6..=1.0 + 1e-6).contains(&c) && c >= previous - 1e-6, "rising edge x {x}: {c}");
        if c > 0.01 && c < 0.99 {
            partial += 1;
        }
        previous = c;
    }
    assert!(partial >= 2, "a feathered edge has intermediate coverage");
    // Inverted is the complement.
    let mut inv = range(0.35, 0.65, 0.0);
    inv.inverted = true;
    let inverted = run(inv);
    for x in 0..64 {
        let (a, b) = (cover(&hard, x), cover(&inverted, x));
        assert!((a + b - 1.0).abs() < 1e-6, "x {x}");
    }
    // Combined with a geometry mask by subtract: the window minus the left half.
    let left_half = mask(MaskSource::LinearGradient(LinearGradient { zero: (0.5, 0.0), full: (0.49, 0.0), flipped: false }));
    let mut minus = left_half;
    minus.blend = MaskBlend::Subtract;
    let cut = run_many(&plane, vec![range(0.0, 1.0, 0.0), minus]);
    assert_eq!(g(&cut, 2, 8), g(&plain, 2, 8), "left edge removed");
    assert_eq!(g(&cut, 60, 8), g(&plain, 60, 8) * 2.0, "right side kept");
    // Range masks read the image as it was BEFORE any correction ran: a first correction that
    // brightens everything by 2 stops does not change what the second one selects.
    let everything = mask(MaskSource::LinearGradient(LinearGradient { zero: (-1.0, 0.0), full: (-0.5, 0.0), flipped: false }));
    let lift = Correction::new("lift", vec![everything], LocalStack { exposure: 0.5, ..LocalStack::default() });
    let dark_only = Correction::new("dark", vec![range(0.0, 0.3, 0.0)], LocalStack { exposure: -0.25, ..LocalStack::default() });
    let both = render(&plane, &recipe(vec![lift.clone(), dark_only]), Threads::single()).unwrap();
    let lifted = render(&plane, &recipe(vec![lift]), Threads::single()).unwrap();
    for x in 0..64 {
        if (luma_at(&plain, x) - 0.3).abs() < 0.02 {
            continue;
        }
        let want = if luma_at(&plain, x) <= 0.3 { g(&lifted, x, 8) * 0.5 } else { g(&lifted, x, 8) };
        assert!((g(&both, x, 8) - want).abs() <= 1e-6 * want.abs().max(1.0), "x {x}: {} vs {want}", g(&both, x, 8));
    }
}

fn run_many(plane: &CfaPlane, masks: Vec<Mask>) -> DevelopedImage {
    render(plane, &recipe(vec![Correction::new("c", masks, one_stop())]), Threads::single()).unwrap()
}

/// Left half reddish, right half bluish (RGGB sites), so colour range masks have something to find.
fn two_colour() -> CfaPlane {
    plane_from(64, 32, |x, y| match (x % 2, y % 2, x < 32) {
        (0, 0, true) => 3000,
        (1, 1, true) => 700,
        (0, 0, false) => 700,
        (1, 1, false) => 3000,
        _ => 1500,
    })
}

#[test]
fn colour_range_selects_by_distance_to_the_samples() {
    let plane = two_colour();
    let plain = render(&plane, &recipe(Vec::new()), Threads::single()).unwrap();
    let encoded = |im: &DevelopedImage, x, y| rgb(im, x, y).map(|c| enc(f64::from(c)));
    let red = encoded(&plain, 16, 16);
    let blue = encoded(&plain, 48, 16);
    assert!(red[0] > red[2] + 0.2 && blue[2] > blue[0] + 0.2, "{red:?} {blue:?}");
    let col = |samples: Vec<[f64; 3]>, breadth: f64| mask(MaskSource::ColourRange(ColourRange { samples, breadth }));
    let run = |m: Mask| render(&plane, &recipe(vec![Correction::new("c", vec![m], one_stop())]), Threads::single()).unwrap();
    // A red sample lights the red half (interior, away from the seam) and not the blue half.
    let hit = run(col(vec![red], 0.2));
    assert_eq!(rgb(&hit, 16, 16).map(|c| c * 0.5), rgb(&plain, 16, 16).map(|c| c), "full +1 stop on the sampled colour");
    assert_eq!(rgb(&hit, 48, 16), rgb(&plain, 48, 16), "blue half untouched");
    // Two samples: both halves.
    let both = run(col(vec![red, blue], 0.2));
    assert_eq!(rgb(&both, 16, 16), rgb(&hit, 16, 16));
    assert_eq!(rgb(&both, 48, 16).map(|c| c * 0.5), rgb(&plain, 48, 16));
    // Breadth widens: a sample halfway between red and blue misses both at 0 and reaches them at 1.
    let between = [(red[0] + blue[0]) / 2.0, (red[1] + blue[1]) / 2.0, (red[2] + blue[2]) / 2.0];
    let narrow = run(col(vec![between], 0.0));
    let wide = run(col(vec![between], 1.0));
    assert_eq!(rgb(&narrow, 16, 16), rgb(&plain, 16, 16));
    assert!(g(&wide, 16, 16) > g(&plain, 16, 16) && g(&wide, 48, 16) > g(&plain, 48, 16));
    // Coverage falls off smoothly with distance (a ramp of samples around red).
    let mut last = f32::INFINITY;
    for step in 0..=10 {
        let t = f64::from(step) / 10.0;
        let sample = [red[0] * (1.0 - t) + between[0] * t, red[1] * (1.0 - t) + between[1] * t, red[2] * (1.0 - t) + between[2] * t];
        let v = g(&run(col(vec![sample], 0.3)), 16, 16);
        assert!(v <= last + 1e-6, "step {step}");
        last = v;
    }
    // Inverted = complement; the seam region stays finite.
    let mut inv = col(vec![red], 0.2);
    inv.inverted = true;
    let inverted = run(inv);
    assert_eq!(rgb(&inverted, 16, 16), rgb(&plain, 16, 16));
    assert_eq!(rgb(&inverted, 48, 16).map(|c| c * 0.5), rgb(&plain, 48, 16));
    assert!(hit.rgb.iter().chain(&wide.rgb).all(|v| v.is_finite()));
}

#[test]
fn hostile_range_masks_refuse_typed() {
    let plane = ramp();
    let refuse = |source: MaskSource, want: DevelopError| {
        let r = recipe(vec![Correction::new("c", vec![mask(source)], one_stop())]);
        assert_eq!(r.validate().err(), Some(want));
        assert_eq!(render(&plane, &r, Threads::single()).err(), Some(want));
    };
    let lum = |lo, hi, feather| MaskSource::LuminanceRange(LuminanceRange { lo, hi, feather });
    refuse(lum(0.5, 0.5, 0.0), DevelopError::InvalidMask);
    refuse(lum(0.7, 0.3, 0.0), DevelopError::InvalidMask);
    refuse(lum(-0.1, 0.5, 0.0), DevelopError::InvalidMask);
    refuse(lum(0.1, 1.1, 0.0), DevelopError::InvalidMask);
    refuse(lum(0.1, 0.5, 1.5), DevelopError::InvalidMask);
    refuse(lum(f64::NAN, 0.5, 0.0), DevelopError::NonFinite);
    refuse(lum(0.1, f64::INFINITY, 0.0), DevelopError::NonFinite);
    let colr = |samples, breadth| MaskSource::ColourRange(ColourRange { samples, breadth });
    refuse(colr(Vec::new(), 0.5), DevelopError::InvalidMask);
    refuse(colr(vec![[0.5; 3]; MAX_COLOUR_SAMPLES + 1], 0.5), DevelopError::InvalidMask);
    refuse(colr(vec![[0.5, 0.5, 1.01]], 0.5), DevelopError::InvalidMask);
    refuse(colr(vec![[-0.01, 0.5, 0.5]], 0.5), DevelopError::InvalidMask);
    refuse(colr(vec![[0.5, f64::NAN, 0.5]], 0.5), DevelopError::NonFinite);
    refuse(colr(vec![[0.5; 3]], 1.5), DevelopError::InvalidMask);
    refuse(colr(vec![[0.5; 3]], f64::NAN), DevelopError::NonFinite);
    // The group stays validated while disabled; an inactive mask is still validated.
    let mut off = recipe(vec![Correction::new("c", vec![mask(lum(0.9, 0.1, 0.0))], one_stop())]);
    off.enables.enable_mask_group_based_corrections = false;
    assert_eq!(off.validate().err(), Some(DevelopError::InvalidMask));
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

#[test]
fn corpus_dng_range_masks_are_thread_invariant_and_pinned() {
    const TEST: &str = "corpus_dng_range_masks_are_thread_invariant_and_pinned";
    let Some(bytes) = corpus_file(TEST, "canon-eos-5d3-mlv-14bit-lossless.dng", "1d77dcc6cdb839b10c1948f0b4471984ab48cf15e973fb77520584cee72b603e") else {
        return;
    };
    let raw = decode_container(&bytes).unwrap();
    let (plane, _) = raw.cfa_plane(raw.default_window(), RangePolicy::ClampReported, 1, Limits::LARGE).unwrap();
    let (mut rec, _) = raw
        .recipe(&color::xyz_d50_to_linear_srgb().unwrap(), profile(), BaselineExposure::Ignore)
        .unwrap();
    rec.enables.enable_mask_group_based_corrections = true;
    rec.adjust.local = vec![
        Correction::new(
            "shadows",
            vec![mask(MaskSource::LuminanceRange(LuminanceRange { lo: 0.0, hi: 0.35, feather: 0.6 }))],
            LocalStack { exposure: 0.1, shadows: 0.3, saturation: 0.1, ..LocalStack::default() },
        ),
        Correction::new(
            "skin",
            vec![mask(MaskSource::ColourRange(ColourRange { samples: vec![[0.62, 0.45, 0.38], [0.55, 0.40, 0.32]], breadth: 0.35 }))],
            LocalStack { temperature: -0.2, saturation: -0.15, ..LocalStack::default() },
        ),
    ];
    let a = render(&plane, &rec, Threads::single()).unwrap();
    let b = render(&plane, &rec, Threads::new(6).unwrap()).unwrap();
    assert_eq!(a, b, "thread count does not change range masks");
    let local = std::mem::take(&mut rec.adjust.local);
    let plain = render(&plane, &rec, Threads::auto()).unwrap();
    assert_ne!(a.rgb, plain.rgb);
    assert!(a.rgb.iter().all(|v| v.is_finite()));
    drop(local);
    let mut h = Sha256::new();
    for v in &a.rgb {
        h.update(v.to_le_bytes());
    }
    let digest = hex(&h.finalize());
    println!("corpus range-masks {}x{} digest={digest}", a.width, a.height);
    assert_eq!(digest, PIN_RANGE);
}

const PIN_RANGE: &str = "e6e29ebdb4993a633522f097c880bb16c9fd976311b7d5eec21d3f50e546d9c0";
