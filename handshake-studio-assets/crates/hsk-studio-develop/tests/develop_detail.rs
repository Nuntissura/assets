//! Lane SMB-DEVELOP-CONTROLS-2 slice B: texture, dehaze, sharpening, luminance / colour noise
//! reduction. Lightroom-matching values are UNVERIFIED (D5); these tests prove this crate's
//! documented methods (see src/detail.rs), identity, determinism, refusals and corpus digest pins.
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_develop::detail::{dehaze, reduce_noise, sharpen, texture};
use hsk_studio_develop::*;
use sha2::{Digest, Sha256};

const LUMA: [f64; 3] = adjust::REC709_LUMA;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn profile() -> ProfileBinding {
    ProfileBinding {
        profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(),
        sha256: [7; 32],
    }
}

/// xorshift in [0, 1).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn grey_image(w: usize, h: usize, mut f: impl FnMut(usize, usize) -> f32) -> Vec<f32> {
    let mut out = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let v = f(x, y);
            out.extend_from_slice(&[v, v, v]);
        }
    }
    out
}

fn px(img: &[f32], w: usize, x: usize, y: usize) -> f32 {
    img[(y * w + x) * 3 + 1]
}

fn variance(values: impl Iterator<Item = f32>) -> f64 {
    let v: Vec<f64> = values.map(f64::from).collect();
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / v.len() as f64
}

#[test]
fn texture_and_sharpening_act_on_detail_and_edges() {
    let (w, h) = (96usize, 64usize);
    let flat = grey_image(w, h, |_, _| 0.2);
    let step = grey_image(w, h, |x, _| if x < 48 { 0.1 } else { 0.4 });
    let mut noise_rng = Rng(0x1234_5678_9abc_def1);
    let fine = grey_image(w, h, |_, _| 0.2 + 0.03 * (noise_rng.next() as f32 - 0.5));
    let run = |img: &[f32], f: &dyn Fn(&mut [f32]) -> Result<(), DevelopError>| {
        let mut out = img.to_vec();
        f(&mut out).unwrap();
        out
    };
    // Identity at 0: bit-exact.
    assert_eq!(run(&step, &|r| texture(r, w as u32, h as u32, 0.0, LUMA)), step);
    assert_eq!(run(&step, &|r| sharpen(r, w as u32, h as u32, &Sharpen::default(), LUMA)), step);
    // Flat frames are fixed points (up to the encode/decode round trip of an untouched offset 0).
    for amount in [-100.0, 100.0] {
        assert_eq!(run(&flat, &|r| texture(r, w as u32, h as u32, amount, LUMA)), flat);
    }
    assert_eq!(run(&flat, &|r| sharpen(r, w as u32, h as u32, &Sharpen { amount: 150.0, ..Sharpen::spec_defaults() }, LUMA)), flat);
    // Texture: fine detail variance grows with +, shrinks with -.
    let var = |img: &[f32]| variance(img.chunks_exact(3).map(|p| p[1]));
    let base = var(&fine);
    let up = var(&run(&fine, &|r| texture(r, w as u32, h as u32, 100.0, LUMA)));
    let down = var(&run(&fine, &|r| texture(r, w as u32, h as u32, -100.0, LUMA)));
    assert!(up > base * 1.5 && down < base * 0.5, "{base} {up} {down}");
    // Sharpening overshoots both sides of an edge; more amount = more overshoot; mean is preserved.
    let sharp = |s: Sharpen| run(&step, &|r| sharpen(r, w as u32, h as u32, &s, LUMA));
    let s1 = sharp(Sharpen { amount: 60.0, radius: 1.0, detail: 100.0, masking: 0.0 });
    let s2 = sharp(Sharpen { amount: 120.0, radius: 1.0, detail: 100.0, masking: 0.0 });
    assert!(px(&s1, w, 46, 10) < 0.1 && px(&s1, w, 49, 10) > 0.4, "{} {}", px(&s1, w, 46, 10), px(&s1, w, 49, 10));
    assert!(px(&s2, w, 49, 10) > px(&s1, w, 49, 10) && px(&s2, w, 46, 10) < px(&s1, w, 46, 10));
    assert_eq!(px(&s1, w, 5, 10), 0.1, "far from the edge nothing changes");
    // Detail slider limits halos: low detail overshoots less at a hard edge.
    let hard = grey_image(w, h, |x, _| if x < 48 { 0.02 } else { 0.9 });
    let halo = |detail: f64| {
        let o = run(&hard, &|r| sharpen(r, w as u32, h as u32, &Sharpen { amount: 100.0, radius: 1.5, detail, masking: 0.0 }, LUMA));
        px(&o, w, 49, 10) - 0.9
    };
    assert!(halo(100.0) > halo(0.0) && halo(0.0) >= 0.0, "{} {}", halo(100.0), halo(0.0));
    // Radius widens the affected band: the pixel 4 px from the edge moves only with the larger radius.
    let wide = run(&step, &|r| sharpen(r, w as u32, h as u32, &Sharpen { amount: 100.0, radius: 3.0, detail: 100.0, masking: 0.0 }, LUMA));
    let narrow = run(&step, &|r| sharpen(r, w as u32, h as u32, &Sharpen { amount: 100.0, radius: 0.5, detail: 100.0, masking: 0.0 }, LUMA));
    assert!((px(&wide, w, 52, 10) - 0.4).abs() > (px(&narrow, w, 52, 10) - 0.4).abs());
    // Edge masking: a weak-gradient texture is left alone at masking 100 while the strong edge still sharpens.
    let mut rng = Rng(7);
    let mixed = grey_image(w, h, |x, _| (if x < 48 { 0.1 } else { 0.4 }) + 0.004 * (rng.next() as f32 - 0.5));
    let unmasked = run(&mixed, &|r| sharpen(r, w as u32, h as u32, &Sharpen { amount: 100.0, radius: 1.0, detail: 100.0, masking: 0.0 }, LUMA));
    let masked = run(&mixed, &|r| sharpen(r, w as u32, h as u32, &Sharpen { amount: 100.0, radius: 1.0, detail: 100.0, masking: 100.0 }, LUMA));
    let flat_region = |img: &[f32]| variance((0..40).flat_map(|x| (0..h).map(move |y| (x, y))).map(|(x, y)| px(img, w, x, y)));
    assert!(flat_region(&masked) < flat_region(&unmasked) * 0.8, "{} {}", flat_region(&masked), flat_region(&unmasked));
    assert!(px(&masked, w, 49, 10) > 0.4, "the strong edge is still sharpened");
    // Hostile.
    let bad = |s: Sharpen, want: DevelopError| {
        assert_eq!(sharpen(&mut step.clone(), w as u32, h as u32, &s, LUMA).err(), Some(want), "{s:?}");
    };
    bad(Sharpen { amount: 150.5, ..Sharpen::default() }, DevelopError::InvalidAdjustment);
    bad(Sharpen { amount: -1.0, ..Sharpen::default() }, DevelopError::InvalidAdjustment);
    bad(Sharpen { radius: 0.49, ..Sharpen::default() }, DevelopError::InvalidAdjustment);
    bad(Sharpen { radius: 3.01, ..Sharpen::default() }, DevelopError::InvalidAdjustment);
    bad(Sharpen { detail: 101.0, ..Sharpen::default() }, DevelopError::InvalidAdjustment);
    bad(Sharpen { masking: f64::NAN, ..Sharpen::default() }, DevelopError::NonFinite);
    assert_eq!(texture(&mut step.clone(), w as u32, h as u32, 100.5, LUMA).err(), Some(DevelopError::InvalidAdjustment));
    assert_eq!(texture(&mut step.clone(), w as u32 + 1, h as u32, 10.0, LUMA).err(), Some(DevelopError::InvalidDimensions));
    assert_eq!(texture(&mut step.clone(), w as u32, h as u32, 10.0, [f64::NAN, 0.5, 0.5]).err(), Some(DevelopError::InvalidAdjustment));
    let mut poisoned = step.clone();
    poisoned[10] = f32::INFINITY;
    assert_eq!(texture(&mut poisoned, w as u32, h as u32, 10.0, LUMA).err(), Some(DevelopError::NonFinite));
    assert_eq!(texture(&mut [], 0, 0, 10.0, LUMA).err(), Some(DevelopError::InvalidDimensions));
}

#[test]
fn dehaze_follows_the_dark_channel_prior() {
    let (w, h) = (96usize, 72usize);
    // Scene radiance: flat shapes with deep darks (dark channel near 0), then veiled with airlight.
    let mut rng = Rng(99);
    let scene: Vec<[f32; 3]> = (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let base = if (x / 12 + y / 12) % 2 == 0 { [0.02, 0.03, 0.05] } else { [0.5, 0.08, 0.04] };
            base.map(|c| c + 0.01 * rng.next() as f32)
        })
        .collect();
    let veil = |t: f32, a: f32| -> Vec<f32> { scene.iter().flat_map(|p| p.map(|c| c * t + a * (1.0 - t))).collect() };
    let hazy = veil(0.55, 0.62);
    let contrast = |img: &[f32]| variance(img.chunks_exact(3).map(|p| p[0] + p[1] + p[2]));
    let darkest = |img: &[f32]| img.chunks_exact(3).map(|p| p[0].min(p[1]).min(p[2])).fold(f32::INFINITY, f32::min);
    let mut out = hazy.clone();
    dehaze(&mut out, w as u32, h as u32, 100.0).unwrap();
    assert!(out.iter().all(|v| v.is_finite()));
    assert!(contrast(&out) > contrast(&hazy) * 1.3, "{} {}", contrast(&out), contrast(&hazy));
    assert!(darkest(&out) < darkest(&hazy) * 0.75, "{} {}", darkest(&out), darkest(&hazy));
    // Strength is monotone: 50 sits between 0 and 100.
    let mut half = hazy.clone();
    dehaze(&mut half, w as u32, h as u32, 50.0).unwrap();
    assert!(contrast(&hazy) < contrast(&half) && contrast(&half) < contrast(&out));
    // Negative amounts add haze: contrast drops and the darkest value rises toward the airlight.
    let mut foggy = scene.iter().flat_map(|p| *p).collect::<Vec<f32>>();
    let clear_contrast = contrast(&foggy);
    dehaze(&mut foggy, w as u32, h as u32, -100.0).unwrap();
    assert!(contrast(&foggy) < clear_contrast * 0.5, "{} {clear_contrast}", contrast(&foggy));
    // Identity at 0 and determinism; a bright haze-free-looking flat frame stays finite.
    let mut zero = hazy.clone();
    dehaze(&mut zero, w as u32, h as u32, 0.0).unwrap();
    assert_eq!(zero, hazy);
    let mut again = hazy.clone();
    dehaze(&mut again, w as u32, h as u32, 100.0).unwrap();
    assert_eq!(again, out);
    let mut white = vec![1.0f32; w * h * 3];
    dehaze(&mut white, w as u32, h as u32, 100.0).unwrap();
    assert!(white.iter().all(|v| v.is_finite()));
    let mut black = vec![0.0f32; w * h * 3];
    dehaze(&mut black, w as u32, h as u32, 100.0).unwrap();
    assert!(black.iter().all(|v| v.is_finite() && *v >= 0.0));
    // Hostile.
    assert_eq!(dehaze(&mut hazy.clone(), w as u32, h as u32, 100.1).err(), Some(DevelopError::InvalidAdjustment));
    assert_eq!(dehaze(&mut hazy.clone(), w as u32, h as u32, f64::NAN).err(), Some(DevelopError::NonFinite));
    assert_eq!(dehaze(&mut hazy.clone(), w as u32 - 1, h as u32, 10.0).err(), Some(DevelopError::InvalidDimensions));
    let mut nan = hazy.clone();
    nan[3] = f32::NAN;
    assert_eq!(dehaze(&mut nan, w as u32, h as u32, 10.0).err(), Some(DevelopError::NonFinite));
    // 1 x 1 and 1 x n frames do not panic.
    for (ww, hh) in [(1usize, 1usize), (1, 5), (7, 1)] {
        let mut tiny = vec![0.3f32; ww * hh * 3];
        dehaze(&mut tiny, ww as u32, hh as u32, 80.0).unwrap();
        dehaze(&mut tiny, ww as u32, hh as u32, -80.0).unwrap();
        assert!(tiny.iter().all(|v| v.is_finite()));
    }
}

#[test]
fn noise_reduction_smooths_noise_and_keeps_edges() {
    let (w, h) = (96usize, 64usize);
    let mut rng = Rng(0xfeed_beef);
    // Luminance noise on a two-level frame.
    let noisy = grey_image(w, h, |x, _| (if x < 48 { 0.15 } else { 0.5 }) + 0.02 * (rng.next() as f32 - 0.5));
    let lum = |l: f64| {
        let mut out = noisy.clone();
        reduce_noise(&mut out, w as u32, h as u32, &NoiseReduction { luminance: l, colour: 0.0 }, LUMA).unwrap();
        out
    };
    let region = |img: &[f32]| variance((5..40).flat_map(|x| (5..h - 5).map(move |y| (x, y))).map(|(x, y)| px(img, w, x, y)));
    let (v0, v50, v100) = (region(&noisy), region(&lum(50.0)), region(&lum(100.0)));
    assert!(v100 < v50 && v50 < v0 * 0.7 && v100 < v0 * 0.2, "{v0} {v50} {v100}");
    // The edge survives (bilateral): the step between the two levels keeps most of its height.
    let smoothed = lum(100.0);
    let gap = px(&smoothed, w, 52, 20) - px(&smoothed, w, 44, 20);
    assert!(gap > 0.3, "edge height {gap}");
    // Colour noise: random chroma on a flat luma. Chroma spread shrinks, luma is unchanged.
    let mut rng = Rng(42);
    let tint: Vec<f32> = (0..w * h)
        .flat_map(|_| {
            let (a, b) = (0.06 * (rng.next() as f32 - 0.5), 0.06 * (rng.next() as f32 - 0.5));
            [0.3 + a, 0.3, 0.3 + b]
        })
        .collect();
    let chroma_spread = |img: &[f32]| variance(img.chunks_exact(3).map(|p| p[0] - p[2]));
    let luma_of = |img: &[f32]| -> Vec<f64> {
        img.chunks_exact(3).map(|p| 0.2126729 * f64::from(p[0]) + 0.7151522 * f64::from(p[1]) + 0.072175 * f64::from(p[2])).collect()
    };
    let mut cleaned = tint.clone();
    reduce_noise(&mut cleaned, w as u32, h as u32, &NoiseReduction { luminance: 0.0, colour: 100.0 }, LUMA).unwrap();
    assert!(chroma_spread(&cleaned) < chroma_spread(&tint) * 0.3, "{} {}", chroma_spread(&cleaned), chroma_spread(&tint));
    let max_dev = luma_of(&cleaned).iter().zip(luma_of(&tint)).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
    assert!(max_dev < 0.02, "colour NR keeps luma within the nonlinear round trip: {max_dev}");
    // Identity at 0, determinism, flat frames unchanged.
    let mut zero = noisy.clone();
    reduce_noise(&mut zero, w as u32, h as u32, &NoiseReduction::default(), LUMA).unwrap();
    assert_eq!(zero, noisy);
    assert_eq!(lum(70.0), lum(70.0));
    let mut flat = vec![0.25f32; w * h * 3];
    reduce_noise(&mut flat, w as u32, h as u32, &NoiseReduction { luminance: 100.0, colour: 100.0 }, LUMA).unwrap();
    assert!(flat.iter().all(|v| (v - 0.25).abs() < 1e-5));
    // Spec defaults are named, not default.
    assert_eq!(NoiseReduction::default(), NoiseReduction { luminance: 0.0, colour: 0.0 });
    assert_eq!(NoiseReduction::spec_defaults().colour, 25.0);
    assert_eq!(Sharpen::spec_defaults().amount, 40.0);
    assert_eq!(Sharpen::default().amount, 0.0);
    // Hostile.
    let bad = |nr: NoiseReduction, want: DevelopError| {
        assert_eq!(reduce_noise(&mut noisy.clone(), w as u32, h as u32, &nr, LUMA).err(), Some(want));
    };
    bad(NoiseReduction { luminance: 100.5, colour: 0.0 }, DevelopError::InvalidAdjustment);
    bad(NoiseReduction { luminance: 0.0, colour: -0.5 }, DevelopError::InvalidAdjustment);
    bad(NoiseReduction { luminance: f64::NAN, colour: 0.0 }, DevelopError::NonFinite);
    assert_eq!(
        reduce_noise(&mut noisy.clone(), w as u32 + 2, h as u32, &NoiseReduction { luminance: 10.0, colour: 0.0 }, LUMA).err(),
        Some(DevelopError::InvalidDimensions)
    );
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

fn native() -> DevelopRecipe {
    DevelopRecipe::native([1.0; 3], [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], profile())
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
fn recipe_gating_tile_api_refusals_and_hostile_fields() {
    let plane = plane_from(48, 40, |x, y| 800 + (30 * x + 17 * y) as u16 % 2400);
    let render = |r: &DevelopRecipe| develop_to_image(&plane, r, 1, &CancellationToken::default());
    let plain = render(&native()).unwrap().0;
    type Poke = fn(&mut Detail);
    let pokes: [(&str, Poke, &str); 6] = [
        ("texture", |d| d.texture = 50.0, "texture_whole_frame"),
        ("dehaze", |d| d.dehaze = 50.0, "dehaze_whole_frame"),
        ("sharpen", |d| d.sharpen.amount = 80.0, "sharpening_whole_frame"),
        ("luminance nr", |d| d.noise.luminance = 40.0, "noise_reduction_whole_frame"),
        ("colour nr", |d| d.noise.colour = 40.0, "noise_reduction_whole_frame"),
        ("sharpen radius only", |d| d.sharpen.radius = 2.0, ""),
    ];
    for (name, poke, tile_name) in pokes {
        let mut r = native();
        poke(&mut r.adjust.detail);
        let before = r.clone();
        assert_eq!(render(&r).unwrap().0, plain, "{name}: retained but ignored while enable_detail is off");
        assert_eq!(r, before);
        r.enables.enable_detail = true;
        if tile_name.is_empty() {
            assert_eq!(render(&r).unwrap().0, plain, "{name}: radius alone is not a stage");
            continue;
        }
        assert_ne!(render(&r).unwrap().0, plain, "{name}: runs once enabled");
        let mut calls = 0;
        let refused = develop(
            &DevelopRequest {
                plane: &plane,
                recipe: &r,
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
        assert_eq!(refused.err(), Some(DevelopError::UnsupportedContribution(tile_name)), "{name}");
        assert_eq!(calls, 0);
    }
    // Every field is validated even while the group is off.
    type Hostile = (&'static str, fn(&mut Detail), DevelopError);
    let hostile: [Hostile; 8] = [
        ("texture", |d| d.texture = 100.5, DevelopError::InvalidAdjustment),
        ("dehaze", |d| d.dehaze = f64::NAN, DevelopError::NonFinite),
        ("amount", |d| d.sharpen.amount = 151.0, DevelopError::InvalidAdjustment),
        ("radius", |d| d.sharpen.radius = 3.5, DevelopError::InvalidAdjustment),
        ("detail", |d| d.sharpen.detail = -1.0, DevelopError::InvalidAdjustment),
        ("masking", |d| d.sharpen.masking = f64::INFINITY, DevelopError::NonFinite),
        ("luminance", |d| d.noise.luminance = 101.0, DevelopError::InvalidAdjustment),
        ("colour", |d| d.noise.colour = -5.0, DevelopError::InvalidAdjustment),
    ];
    for (name, poke, want) in hostile {
        let mut r = native();
        poke(&mut r.adjust.detail);
        assert_eq!(r.validate().err(), Some(want), "{name}");
        assert_eq!(render(&r).err(), Some(want), "{name}");
    }
    // The stage order is fixed (noise, dehaze, texture, clarity, sharpening): all together is stable,
    // deterministic across thread counts and distinct from any single stage.
    let mut all = native();
    all.enables.enable_detail = true;
    all.adjust.detail = Detail {
        texture: 30.0,
        dehaze: 20.0,
        sharpen: Sharpen { amount: 60.0, radius: 1.2, detail: 40.0, masking: 20.0 },
        noise: NoiseReduction { luminance: 30.0, colour: 30.0 },
    };
    all.adjust.presence.clarity = 25.0;
    let one = develop_to_image_threads(&plane, &all, 1, Threads::single(), &CancellationToken::default()).unwrap().0;
    let many = develop_to_image_threads(&plane, &all, 1, Threads::new(6).unwrap(), &CancellationToken::default()).unwrap().0;
    assert_eq!(one, many);
    assert!(one.rgb.iter().all(|v| v.is_finite()));
    assert_ne!(one, plain);
}

#[test]
fn corpus_dng_detail_stack_is_pinned() {
    const TEST: &str = "corpus_dng_detail_stack_is_pinned";
    let Some(bytes) = corpus_file(TEST, "canon-eos-5d3-mlv-14bit-lossless.dng", "1d77dcc6cdb839b10c1948f0b4471984ab48cf15e973fb77520584cee72b603e") else {
        return;
    };
    let raw = decode_container(&bytes).unwrap();
    // A 640 x 480 crop of the frame keeps the debug-build run short (the stages are serial).
    let full = raw.default_window();
    let window = PlaneWindow { x: full.x + 600, y: full.y + 100, width: 640, height: 480 };
    let (plane, _) = raw.cfa_plane(window, RangePolicy::ClampReported, 1, Limits::LARGE).unwrap();
    let (mut recipe, _) = raw
        .recipe(&color::xyz_d50_to_linear_srgb().unwrap(), profile(), BaselineExposure::Ignore)
        .unwrap();
    recipe.exposure_stops = 1.0;
    recipe.enables.enable_detail = true;
    recipe.adjust.detail = Detail {
        texture: 35.0,
        dehaze: 30.0,
        sharpen: Sharpen { amount: 70.0, radius: 1.1, detail: 35.0, masking: 25.0 },
        noise: NoiseReduction { luminance: 35.0, colour: 45.0 },
    };
    recipe.adjust.presence.clarity = 20.0;
    let a = develop_to_image_threads(&plane, &recipe, 1, Threads::single(), &CancellationToken::default()).unwrap().0;
    let b = develop_to_image_threads(&plane, &recipe, 1, Threads::auto(), &CancellationToken::default()).unwrap().0;
    assert_eq!(a, b);
    recipe.enables.enable_detail = false;
    let plain = develop_to_image(&plane, &recipe, 1, &CancellationToken::default()).unwrap().0;
    assert_ne!(a.rgb, plain.rgb);
    assert!(a.rgb.iter().all(|v| v.is_finite()));
    let mut hasher = Sha256::new();
    for v in &a.rgb {
        hasher.update(v.to_le_bytes());
    }
    let digest = hex(&hasher.finalize());
    println!("corpus detail-stack {}x{} digest={digest}", a.width, a.height);
    assert_eq!(digest, PIN_DETAIL);
}

const PIN_DETAIL: &str = "4f94d078074b387b8616ef7c7684400b21d824fa898b95cc6d3594b3e1c3728b";
