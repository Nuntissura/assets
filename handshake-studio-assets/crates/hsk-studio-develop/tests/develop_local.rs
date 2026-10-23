//! Lane SMB-DEVELOP-CONTROLS slice 3: local adjustments (linear / radial gradient and brush masks).
//! Lightroom-matching values are UNVERIFIED (D5); these tests prove this crate's documented maths,
//! mask algebra, resolution independence, determinism, refusals and a corpus digest pin.
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

fn recipe(corrections: Vec<Correction>) -> DevelopRecipe {
    let mut r = DevelopRecipe::native([1.0; 3], [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], profile());
    r.enables.enable_mask_group_based_corrections = true;
    r.adjust.local = corrections;
    r
}

fn render(plane: &CfaPlane, recipe: &DevelopRecipe, threads: Threads) -> Result<DevelopedImage, DevelopError> {
    develop_to_image_threads(plane, recipe, 1, threads, &CancellationToken::default()).map(|r| r.0)
}

fn g(im: &DevelopedImage, x: usize, y: usize) -> f32 {
    im.rgb[(y * im.width as usize + x) * 3 + 1]
}

fn bits(im: &DevelopedImage) -> Vec<u32> {
    im.rgb.iter().map(|v| v.to_bits()).collect()
}

fn mask(source: MaskSource) -> Mask {
    Mask { id: "m".into(), active: true, inverted: false, value: 1.0, blend: MaskBlend::Add, source }
}

/// +1 stop of local exposure (4 stops at +-1, so 0.25).
fn one_stop() -> LocalStack {
    LocalStack { exposure: 0.25, ..LocalStack::default() }
}

fn vertical_ramp(flipped: bool) -> Mask {
    mask(MaskSource::LinearGradient(LinearGradient { zero: (0.5, 0.25), full: (0.5, 0.75), flipped }))
}

#[test]
fn linear_gradient_ramps_exposure_and_flip_is_the_complement() {
    let plane = flat(64, 64, 1000);
    let plain = render(&plane, &recipe(Vec::new()), Threads::single()).unwrap();
    let base = g(&plain, 5, 5);
    let img = render(&plane, &recipe(vec![Correction::new("c", vec![vertical_ramp(false)], one_stop())]), Threads::single()).unwrap();
    // Above the zero line: untouched bytes; below the full line: exactly +1 stop; between: monotone.
    for y in 0..16 {
        assert_eq!(g(&img, 9, y), base, "row {y} above zero");
    }
    for y in 48..64 {
        assert_eq!(g(&img, 9, y), base * 2.0, "row {y} below full");
    }
    let mut last = base;
    for y in 16..=48 {
        let v = g(&img, 9, y);
        assert!(v >= last && v <= base * 2.0, "row {y}: {v}");
        last = v;
    }
    assert!(g(&img, 9, 32) > base * 1.4 && g(&img, 9, 32) < base * 1.6, "midpoint blends half-way: {}", g(&img, 9, 32));
    // Flipped is the complement coverage: top fully lit, bottom untouched.
    let fl = render(&plane, &recipe(vec![Correction::new("c", vec![vertical_ramp(true)], one_stop())]), Threads::single()).unwrap();
    assert_eq!(g(&fl, 9, 2), base * 2.0);
    assert_eq!(g(&fl, 9, 61), base);
    for y in 0..64 {
        let (a, b) = (f64::from(g(&img, 9, y) - base), f64::from(g(&fl, 9, y) - base));
        assert!((a + b - f64::from(base)).abs() < 1e-6, "row {y}: coverage must sum to 1");
    }
    // Inverted equals flipped; mask value scales the coverage; amount is a gain on the slider.
    let mut inv = vertical_ramp(false);
    inv.inverted = true;
    let inverted = render(&plane, &recipe(vec![Correction::new("c", vec![inv], one_stop())]), Threads::single()).unwrap();
    assert_eq!(bits(&inverted), bits(&fl));
    let mut half = vertical_ramp(false);
    half.value = 0.5;
    let halved = render(&plane, &recipe(vec![Correction::new("c", vec![half], one_stop())]), Threads::single()).unwrap();
    assert!((g(&halved, 9, 60) - base * 1.5).abs() < 1e-6);
    let mut double = Correction::new("c", vec![vertical_ramp(false)], LocalStack { exposure: 0.125, ..LocalStack::default() });
    double.amount = 2.0;
    let gained = render(&plane, &recipe(vec![double]), Threads::single()).unwrap();
    assert_eq!(g(&gained, 9, 60), g(&img, 9, 60), "amount 2 x slider 0.125 == slider 0.25");
}

#[test]
fn radial_gradient_geometry_feather_rotation_roundness() {
    let plane = flat(80, 80, 1000);
    let base = g(&render(&plane, &recipe(Vec::new()), Threads::single()).unwrap(), 1, 1);
    #[allow(clippy::too_many_arguments)]
    let radial = |left, top, right, bottom, angle, roundness, feather, flipped| {
        mask(MaskSource::RadialGradient(RadialGradient { left, top, right, bottom, angle, midpoint: 50.0, roundness, feather, flipped }))
    };
    let run = |m: Mask| render(&plane, &recipe(vec![Correction::new("c", vec![m], one_stop())]), Threads::single()).unwrap();
    // Hard ellipse (feather 0): centre lit, corner untouched, boundary respected.
    let hard = run(radial(0.25, 0.25, 0.75, 0.75, 0.0, 0.0, 0.0, false));
    assert_eq!(g(&hard, 40, 40), base * 2.0);
    assert_eq!(g(&hard, 1, 1), base);
    assert_eq!(g(&hard, 40, 22), base * 2.0, "inside the vertical radius (20 px)");
    assert_eq!(g(&hard, 40, 10), base, "outside it");
    // Feather widens the soft zone: monotone falloff along a radius, bounded by 1 stop.
    let soft = run(radial(0.25, 0.25, 0.75, 0.75, 0.0, 0.0, 100.0, false));
    let mut last = f32::INFINITY;
    for x in 40..60 {
        let v = g(&soft, x, 40);
        assert!(v <= last && v >= base && v <= base * 2.0, "x {x}: {v}");
        last = v;
    }
    assert!(g(&soft, 40, 40) > g(&soft, 52, 40) && g(&soft, 52, 40) > base);
    // Rotation: a wide ellipse turned 90 degrees swaps its long axis.
    let wide = run(radial(0.1, 0.35, 0.9, 0.65, 0.0, 0.0, 0.0, false));
    let turned = run(radial(0.1, 0.35, 0.9, 0.65, 90.0, 0.0, 0.0, false));
    assert_eq!(g(&wide, 70, 40), base * 2.0);
    assert_eq!(g(&wide, 40, 70), base);
    assert_eq!(g(&turned, 70, 40), base);
    assert_eq!(g(&turned, 40, 70), base * 2.0);
    // Roundness: +100 (squarish) covers the diagonal corner of the box, -100 (diamond) does not.
    let square = run(radial(0.25, 0.25, 0.75, 0.75, 0.0, 100.0, 0.0, false));
    let diamond = run(radial(0.25, 0.25, 0.75, 0.75, 0.0, -100.0, 0.0, false));
    assert_eq!(g(&square, 55, 55), base * 2.0);
    assert_eq!(g(&diamond, 55, 55), base);
    // Flipped: complement.
    let flipped = run(radial(0.25, 0.25, 0.75, 0.75, 0.0, 0.0, 0.0, true));
    assert_eq!(g(&flipped, 40, 40), base);
    assert_eq!(g(&flipped, 1, 1), base * 2.0);
}

#[test]
fn brush_dabs_accumulate_and_render_at_any_resolution() {
    let brush = |dabs: Vec<(f64, f64)>, radius, flow, feather| mask(MaskSource::Brush(Brush { dabs, radius, flow, feather }));
    let at = |size: u32, m: Mask| {
        let plane = flat(size, size, 1000);
        let base = g(&render(&plane, &recipe(Vec::new()), Threads::single()).unwrap(), 1, 1);
        let img = render(&plane, &recipe(vec![Correction::new("c", vec![m], one_stop())]), Threads::single()).unwrap();
        (img, base)
    };
    let (one, base) = at(100, brush(vec![(0.5, 0.5)], 0.2, 1.0, 0.0));
    assert_eq!(g(&one, 50, 50), base * 2.0, "full flow at the dab centre");
    assert_eq!(g(&one, 5, 5), base, "far away untouched");
    assert_eq!(g(&one, 50, 75), base, "outside the 20 px radius");
    // Flow scales one dab; two overlapping dabs accumulate (1 - (1 - f)^2).
    let (half, _) = at(100, brush(vec![(0.5, 0.5)], 0.2, 0.5, 0.0));
    assert!((g(&half, 50, 50) - base * 1.5).abs() < 1e-6);
    let (two, _) = at(100, brush(vec![(0.5, 0.5), (0.5, 0.5)], 0.2, 0.5, 0.0));
    assert!((g(&two, 50, 50) - base * 1.75).abs() < 1e-6);
    // Feather: coverage falls from the centre outward without increasing.
    let (soft, _) = at(100, brush(vec![(0.5, 0.5)], 0.3, 1.0, 1.0));
    let mut last = f32::INFINITY;
    for x in 50..80 {
        let v = g(&soft, x, 50);
        assert!(v <= last + 1e-6 && v >= base, "x {x}");
        last = v;
    }
    // Resolution independence: the same dab list at 100 px and 200 px agrees at corresponding
    // normalised positions.
    let list = vec![(0.3, 0.4), (0.35, 0.45), (0.7, 0.6)];
    let (small, b1) = at(100, brush(list.clone(), 0.12, 1.0, 0.5));
    let (large, b2) = at(200, brush(list, 0.12, 1.0, 0.5));
    for (nx, ny) in [(0.3, 0.4), (0.33, 0.43), (0.7, 0.6), (0.5, 0.5), (0.1, 0.9), (0.7, 0.68), (0.62, 0.6)] {
        let a = f64::from(g(&small, (nx * 100.0) as usize, (ny * 100.0) as usize) / b1);
        let b = f64::from(g(&large, (nx * 200.0) as usize, (ny * 200.0) as usize) / b2);
        assert!((a - b).abs() < 0.12, "({nx},{ny}): {a} vs {b}");
    }
    // Dab parsing of the STU-RAW-121 "d x y" record.
    assert_eq!(parse_dab("d 0.25 0.75"), Ok((0.25, 0.75)));
    assert_eq!(parse_dab("  d  -0.1   1.2  "), Ok((-0.1, 1.2)));
}

#[test]
fn mask_algebra_local_stack_and_thread_invariance_on_the_corpus_dng() {
    // Mask algebra on a flat frame: add (union), subtract, inactive pieces, empty mask list.
    let plane = flat(64, 64, 1000);
    let plain = render(&plane, &recipe(Vec::new()), Threads::single()).unwrap();
    let base = g(&plain, 1, 1);
    let top = mask(MaskSource::LinearGradient(LinearGradient { zero: (0.5, 0.5), full: (0.5, 0.25), flipped: false }));
    let ramp = vertical_ramp(false);
    let c = |masks: Vec<Mask>| render(&plane, &recipe(vec![Correction::new("c", masks, one_stop())]), Threads::single()).unwrap();
    let union = c(vec![top.clone(), ramp.clone()]);
    assert_eq!(g(&union, 9, 2), base * 2.0);
    assert_eq!(g(&union, 9, 62), base * 2.0);
    assert_eq!(g(&union, 9, 32), g(&c(vec![ramp.clone()]), 9, 32), "the top gradient adds nothing at the middle");
    let mut minus = ramp.clone();
    minus.blend = MaskBlend::Subtract;
    let cut = c(vec![top.clone(), minus]);
    assert_eq!(g(&cut, 9, 2), base * 2.0, "subtracting a mask that is 0 there changes nothing");
    let mut sub_all = top.clone();
    sub_all.blend = MaskBlend::Subtract;
    let none = c(vec![top.clone(), sub_all]);
    assert_eq!(g(&none, 9, 2), base, "add then subtract the same mask cancels");
    let mut inactive = ramp.clone();
    inactive.active = false;
    assert_eq!(c(vec![inactive]), plain);
    assert_eq!(c(Vec::new()), plain);
    let mut off = Correction::new("c", vec![ramp.clone()], one_stop());
    off.active = false;
    assert_eq!(render(&plane, &recipe(vec![off]), Threads::single()).unwrap(), plain);
    // Disabled group: authored corrections retained, output equals the plain render.
    let mut disabled = recipe(vec![Correction::new("c", vec![ramp.clone()], one_stop())]);
    disabled.enables.enable_mask_group_based_corrections = false;
    let before = disabled.clone();
    assert_eq!(render(&plane, &disabled, Threads::single()).unwrap(), plain);
    assert_eq!(disabled, before);
    // The local stack reaches the Basic controls: shadows lift, temperature warms, contrast pulls.
    let local = |stack: LocalStack, sample: u16| {
        let p = flat(32, 32, sample);
        let full = mask(MaskSource::LinearGradient(LinearGradient { zero: (0.5, -1.0), full: (0.5, -0.5), flipped: false }));
        let plain = render(&p, &recipe(Vec::new()), Threads::single()).unwrap();
        let img = render(&p, &recipe(vec![Correction::new("c", vec![full], stack)]), Threads::single()).unwrap();
        let px = |im: &DevelopedImage| {
            let i = (16 * 32 + 16) * 3;
            [im.rgb[i], im.rgb[i + 1], im.rgb[i + 2]]
        };
        (px(&plain), px(&img))
    };
    let (p, l) = local(LocalStack { shadows: 1.0, ..LocalStack::default() }, 200);
    assert!(l[1] > p[1], "local shadows +1 lifts a dark pixel: {p:?} {l:?}");
    let (p, l) = local(LocalStack { temperature: 0.6, ..LocalStack::default() }, 1500);
    assert!(l[0] > p[0] && l[2] < p[2], "local temperature +0.6 warms");
    let (p, l) = local(LocalStack { contrast: -1.0, ..LocalStack::default() }, 3500);
    assert!(l[1] < p[1], "local contrast -1 pulls a bright pixel down");

    // Corpus DNG (Canon EOS 5D Mark III MLV export, CC0): gradient + radial + brush, threads 1 vs auto, digest pin.
    const TEST: &str = "mask_algebra_local_stack_and_thread_invariance_on_the_corpus_dng";
    let Some(bytes) = corpus_file(TEST, "canon-eos-5d3-mlv-14bit-lossless.dng", "1d77dcc6cdb839b10c1948f0b4471984ab48cf15e973fb77520584cee72b603e") else {
        return;
    };
    let raw = decode_container(&bytes).unwrap();
    let (cfa, _) = raw.cfa_plane(raw.default_window(), RangePolicy::ClampReported, 1, Limits::LARGE).unwrap();
    let (mut rec, _) = raw.recipe(&color::xyz_d50_to_linear_srgb().unwrap(), profile(), BaselineExposure::Ignore).unwrap();
    rec.enables.enable_mask_group_based_corrections = true;
    rec.adjust.local = vec![
        Correction::new(
            "sky",
            vec![mask(MaskSource::LinearGradient(LinearGradient { zero: (0.5, 0.45), full: (0.5, 0.0), flipped: false }))],
            LocalStack { exposure: -0.12, highlights: -0.5, temperature: -0.3, saturation: 0.2, ..LocalStack::default() },
        ),
        Correction::new(
            "vignette",
            vec![mask(MaskSource::RadialGradient(RadialGradient {
                left: 0.2,
                top: 0.15,
                right: 0.8,
                bottom: 0.85,
                angle: 20.0,
                midpoint: 50.0,
                roundness: 10.0,
                feather: 60.0,
                flipped: true,
            }))],
            LocalStack { exposure: -0.1, contrast: 0.2, ..LocalStack::default() },
        ),
        Correction::new(
            "dodge",
            vec![mask(MaskSource::Brush(Brush {
                dabs: (0..24).map(|i| (0.3 + 0.02 * f64::from(i), 0.5 + 0.005 * f64::from(i))).collect(),
                radius: 0.08,
                flow: 0.4,
                feather: 0.7,
            }))],
            LocalStack { exposure: 0.1, clarity: 0.4, shadows: 0.3, ..LocalStack::default() },
        ),
    ];
    let a = render(&cfa, &rec, Threads::single()).unwrap();
    let b = render(&cfa, &rec, Threads::new(7).unwrap()).unwrap();
    let auto = render(&cfa, &rec, Threads::auto()).unwrap();
    assert_eq!(a, b, "thread count does not change local corrections");
    assert_eq!(a, auto);
    rec.adjust.local.clear();
    let no_local = render(&cfa, &rec, Threads::auto()).unwrap();
    assert_ne!(a.rgb, no_local.rgb);
    assert!(a.rgb.iter().all(|v| v.is_finite()));
    let mut h = Sha256::new();
    for v in &a.rgb {
        h.update(v.to_le_bytes());
    }
    let digest = hex(&h.finalize());
    println!("corpus local-stack {}x{} digest={digest}", a.width, a.height);
    assert_eq!(digest, PIN_LOCAL, "corpus digest pin");
}

const PIN_LOCAL: &str = "c0168c479df37269452fc73a0d74a76b667456140f4b32436af3e6959a6b3028";

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
fn hostile_masks_and_corrections_refuse_typed() {
    let plane = flat(32, 32, 1000);
    let ok = || Correction::new("c", vec![vertical_ramp(false)], one_stop());
    let refuse = |mutate: &dyn Fn(&mut Correction), want: DevelopError| {
        let mut c = ok();
        mutate(&mut c);
        let r = recipe(vec![c]);
        assert_eq!(r.validate().err(), Some(want));
        assert_eq!(render(&plane, &r, Threads::single()).err(), Some(want));
    };
    refuse(&|c| c.amount = f64::NAN, DevelopError::NonFinite);
    refuse(&|c| c.amount = -0.1, DevelopError::InvalidMask);
    refuse(&|c| c.amount = 4.5, DevelopError::InvalidMask);
    refuse(&|c| c.stack.exposure = f64::INFINITY, DevelopError::NonFinite);
    refuse(&|c| c.stack.shadows = 1.01, DevelopError::InvalidAdjustment);
    refuse(
        &|c| {
            c.amount = 2.0;
            c.stack.contrast = 0.6;
        },
        DevelopError::InvalidAdjustment,
    );
    refuse(&|c| c.masks[0].value = 1.5, DevelopError::InvalidMask);
    refuse(&|c| c.masks[0].value = f64::NAN, DevelopError::NonFinite);
    refuse(
        &|c| c.masks[0].source = MaskSource::LinearGradient(LinearGradient { zero: (0.5, 0.5), full: (0.5, 0.5), flipped: false }),
        DevelopError::InvalidMask,
    );
    refuse(
        &|c| c.masks[0].source = MaskSource::LinearGradient(LinearGradient { zero: (f64::NAN, 0.0), full: (1.0, 1.0), flipped: false }),
        DevelopError::NonFinite,
    );
    #[allow(clippy::too_many_arguments)]
    let radial = |l, t, r, b, a, m, ro, f| {
        MaskSource::RadialGradient(RadialGradient { left: l, top: t, right: r, bottom: b, angle: a, midpoint: m, roundness: ro, feather: f, flipped: false })
    };
    refuse(&|c| c.masks[0].source = radial(0.6, 0.2, 0.4, 0.8, 0.0, 50.0, 0.0, 0.0), DevelopError::InvalidMask);
    refuse(&|c| c.masks[0].source = radial(0.2, 0.2, 0.8, 0.8, 0.0, 101.0, 0.0, 0.0), DevelopError::InvalidMask);
    refuse(&|c| c.masks[0].source = radial(0.2, 0.2, 0.8, 0.8, 0.0, 50.0, -101.0, 0.0), DevelopError::InvalidMask);
    refuse(&|c| c.masks[0].source = radial(0.2, 0.2, 0.8, 0.8, 0.0, 50.0, 0.0, 100.5), DevelopError::InvalidMask);
    refuse(&|c| c.masks[0].source = radial(0.2, 0.2, 0.8, 0.8, f64::INFINITY, 50.0, 0.0, 0.0), DevelopError::NonFinite);
    let brush = |dabs, radius, flow, feather| MaskSource::Brush(Brush { dabs, radius, flow, feather });
    refuse(&|c| c.masks[0].source = brush(vec![(0.5, 0.5)], 0.0, 1.0, 0.0), DevelopError::InvalidMask);
    refuse(&|c| c.masks[0].source = brush(vec![(0.5, 0.5)], 1.5, 1.0, 0.0), DevelopError::InvalidMask);
    refuse(&|c| c.masks[0].source = brush(vec![(0.5, 0.5)], 0.1, 2.0, 0.0), DevelopError::InvalidMask);
    refuse(&|c| c.masks[0].source = brush(vec![(f64::NAN, 0.5)], 0.1, 1.0, 0.0), DevelopError::NonFinite);
    refuse(&|c| c.masks[0].source = brush(vec![(1.0e9, 0.5)], 0.1, 1.0, 0.0), DevelopError::InvalidMask);
    refuse(&|c| c.masks[0].source = brush(vec![(0.5, 0.5); 65_537], 0.1, 1.0, 0.0), DevelopError::InvalidMask);
    // Rasterisation budget: a maximum list of maximum-radius dabs is refused before any work.
    let big = recipe(vec![Correction::new("c", vec![mask(brush(vec![(0.5, 0.5); 65_536], 1.0, 1.0, 0.0))], one_stop())]);
    assert_eq!(render(&flat(2048, 2048, 1000), &big, Threads::single()).err(), Some(DevelopError::InvalidMask));
    // Too many corrections / masks.
    assert_eq!(recipe(vec![ok(); 65]).validate().err(), Some(DevelopError::InvalidMask));
    let mut many = ok();
    many.masks = vec![vertical_ramp(false); 17];
    assert_eq!(recipe(vec![many]).validate().err(), Some(DevelopError::InvalidMask));
    // An invalid record is refused even while its group is disabled.
    let mut bad = ok();
    bad.amount = f64::NAN;
    let mut off = recipe(vec![bad]);
    off.enables.enable_mask_group_based_corrections = false;
    assert_eq!(off.validate().err(), Some(DevelopError::NonFinite));
    // Dab record parsing.
    for bad in ["", "d", "d 1", "d 1 2 3", "x 1 2", "d a b", "d nan 1", "d 1 inf", "D 1 2"] {
        assert_eq!(parse_dab(bad), Err(DevelopError::InvalidMask), "{bad:?}");
    }
    // The tile API cannot run a whole-frame stage: typed refusal, no tile delivered.
    let mut calls = 0;
    let refused = develop(
        &DevelopRequest {
            plane: &plane,
            recipe: &recipe(vec![ok()]),
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
    assert_eq!(refused.err(), Some(DevelopError::UnsupportedContribution("mask_group_whole_frame")));
    assert_eq!(calls, 0);
    assert_eq!(DevelopError::InvalidMask.code(), "invalid_mask");
}
