//! Lane SMB-DEVELOP-CONTROLS slice 4: develop-to-output (RGB buffer + ICC for the harness to export
//! as JPEG/PNG). Reference values are the IEC 61966-2-1 sRGB OETF evaluated by hand; Lightroom export
//! parity is UNVERIFIED (D5).
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_develop::*;
use sha2::{Digest, Sha256};

const SRGB_V4: &[u8] = include_bytes!("profiles/sRGB-v4.icc");
const SRGB_V4_SHA256: &str = "c56e1685d888f5edb92fe07f2750f387f8fe8e91b32ff8fb0b56bfbbb9458353";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn image(rgb: Vec<f32>, width: u32, height: u32) -> DevelopedImage {
    DevelopedImage { width, height, rgb }
}

#[test]
fn srgb_encoding_matches_hand_values_clamps_and_counts() {
    assert_eq!(hex(&Sha256::digest(SRGB_V4)), SRGB_V4_SHA256, "fixture is the CC0 profile");
    // Hand-evaluated IEC 61966-2-1 values (8-bit, round half up): 0 -> 0, 1 -> 255,
    // 0.0031308 -> 12.92 * it = 0.04045 -> 10.3 -> 10, 0.01 -> 0.09966 -> 25, 0.18 -> 0.46132 -> 118,
    // 0.25 -> 0.53710 -> 137, 0.5 -> 0.73536 -> 188, 0.75 -> 0.88082 -> 225.
    let linear = [0.0f32, 1.0, 0.003_130_8, 0.01, 0.18, 0.25, 0.5, 0.75];
    let want = [0u8, 255, 10, 25, 118, 137, 188, 225];
    let rgb: Vec<f32> = linear.iter().flat_map(|v| [*v; 3]).collect();
    let out = render_output(&image(rgb, 8, 1), OutputDepth::Eight, SRGB_V4, None).unwrap();
    assert_eq!((out.width, out.height, out.depth_bits, out.transfer), (8, 1, 8, "srgb_trc"));
    assert_eq!(out.data.len(), 8 * 3);
    for (i, w) in want.iter().enumerate() {
        assert_eq!(&out.data[i * 3..i * 3 + 3], &[*w; 3], "linear {}", linear[i]);
    }
    assert_eq!(out.clip, ClipReport::default());
    assert_eq!(out.icc, SRGB_V4);
    assert_eq!(hex(&out.icc_sha256), SRGB_V4_SHA256);
    // Clamping is counted per channel value, never silent; 16-bit is big-endian pairs.
    let hot = image(vec![-0.5, 0.0, 1.5, 1.0, 2.0, -1.0], 2, 1);
    let eight = render_output(&hot, OutputDepth::Eight, SRGB_V4, None).unwrap();
    assert_eq!(eight.data, vec![0, 0, 255, 255, 255, 0]);
    assert_eq!(eight.clip, ClipReport { below_zero: 2, above_one: 2 });
    let sixteen = render_output(&hot, OutputDepth::Sixteen, SRGB_V4, None).unwrap();
    assert_eq!(sixteen.data, vec![0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0]);
    assert_eq!(sixteen.clip, eight.clip);
    // Monotone and 8/16-bit consistent over a dense sweep (16-bit / 257 within one 8-bit step).
    let sweep: Vec<f32> = (0..=2000).map(|i| i as f32 / 2000.0).collect();
    let rgb: Vec<f32> = sweep.iter().flat_map(|v| [*v; 3]).collect();
    let n = sweep.len() as u32;
    let a = render_output(&image(rgb.clone(), n, 1), OutputDepth::Eight, SRGB_V4, None).unwrap();
    let b = render_output(&image(rgb, n, 1), OutputDepth::Sixteen, SRGB_V4, None).unwrap();
    let mut last = 0u8;
    for (i, v) in sweep.iter().enumerate() {
        let (s8, s16) = (a.data[i * 3], u16::from_be_bytes([b.data[i * 6], b.data[i * 6 + 1]]));
        assert!(s8 >= last, "monotone at {v}");
        assert!((i32::from(s8) - (i32::from(s16) + 128) / 257).abs() <= 1, "8/16 mismatch at {v}: {s8} {s16}");
        last = s8;
    }
}

#[test]
fn icc_header_and_recipe_hash_binding() {
    check_icc_header(SRGB_V4).unwrap();
    let img = image(vec![0.5; 3], 1, 1);
    let render = |icc: &[u8], expected: Option<[u8; 32]>| render_output(&img, OutputDepth::Eight, icc, expected);
    let bad = |mutate: &dyn Fn(&mut Vec<u8>)| {
        let mut icc = SRGB_V4.to_vec();
        mutate(&mut icc);
        assert_eq!(render(&icc, None).err(), Some(DevelopError::InvalidProfile));
    };
    bad(&|icc| icc.truncate(127));
    bad(&|icc| icc.truncate(400)); // size field no longer matches
    bad(&|icc| icc.push(0));
    bad(&|icc| icc[36..40].copy_from_slice(b"nope"));
    bad(&|icc| icc[16..20].copy_from_slice(b"GRAY"));
    bad(&|icc| icc[12..16].copy_from_slice(b"link"));
    bad(&|icc| icc[12..16].copy_from_slice(b"abst"));
    bad(&|icc| icc[8] = 1);
    assert_eq!(render(&[], None).err(), Some(DevelopError::InvalidProfile));
    // Hash binding: the recipe's output ProfileBinding.sha256 must equal the embedded bytes' hash.
    let good: [u8; 32] = Sha256::digest(SRGB_V4).into();
    assert!(render(SRGB_V4, Some(good)).is_ok());
    let mut other = good;
    other[0] ^= 1;
    assert_eq!(render(SRGB_V4, Some(other)).err(), Some(DevelopError::InvalidProfile));
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
fn corpus_dng_develops_to_an_exportable_buffer_with_its_profile() {
    const TEST: &str = "corpus_dng_develops_to_an_exportable_buffer_with_its_profile";
    let Some(bytes) = corpus_file(TEST, "canon-eos-5d3-mlv-14bit-lossless.dng", "1d77dcc6cdb839b10c1948f0b4471984ab48cf15e973fb77520584cee72b603e") else {
        return;
    };
    let raw = decode_container(&bytes).unwrap();
    let window = raw.default_window();
    let (plane, _) = raw.cfa_plane(window, RangePolicy::ClampReported, 1, Limits::LARGE).unwrap();
    let binding = ProfileBinding {
        profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(),
        sha256: Sha256::digest(SRGB_V4).into(),
    };
    let (mut recipe, _) = raw
        .recipe(&color::xyz_d50_to_linear_srgb().unwrap(), binding, BaselineExposure::Ignore)
        .unwrap();
    recipe.adjust.tone = BasicTone { contrast: 20.0, shadows: 25.0, ..BasicTone::default() };
    recipe.enables.enable_colour_adjustments = true;
    recipe.adjust.presence.vibrance = 20.0;
    let cancel = CancellationToken::default();
    let (out, receipt) = develop_to_output(&plane, &recipe, 1, OutputDepth::Eight, SRGB_V4, Threads::auto(), &cancel).unwrap();
    assert_eq!((out.width, out.height), (window.width, window.height));
    assert_eq!(out.data.len(), out.width as usize * out.height as usize * 3);
    assert_eq!(out.icc, SRGB_V4, "the profile travels with the pixels unchanged");
    assert_eq!(receipt.output.width, out.width);
    // A real photograph: wide tonal spread, not a constant frame.
    let (lo, hi) = out.data.iter().fold((255u8, 0u8), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
    assert!(hi > lo.saturating_add(100), "tonal spread {lo}..{hi}");
    let single = develop_to_output(&plane, &recipe, 1, OutputDepth::Eight, SRGB_V4, Threads::single(), &cancel).unwrap().0;
    assert_eq!(single, out, "thread count does not change the export buffer");
    let digest = hex(&Sha256::digest(&out.data));
    println!("corpus output {}x{} srgb8 digest={digest} clip={:?}", out.width, out.height, out.clip);
    assert_eq!(digest, PIN_OUTPUT, "corpus digest pin");
    // The recipe's output binding must match the profile bytes: a different profile refuses before
    // any pixel is developed.
    let mut other = recipe.clone();
    other.output_profile.sha256 = [9; 32];
    other.working_profile.sha256 = [9; 32];
    assert_eq!(
        develop_to_output(&plane, &other, 1, OutputDepth::Eight, SRGB_V4, Threads::single(), &cancel).err(),
        Some(DevelopError::InvalidProfile)
    );
}

const PIN_OUTPUT: &str = "845cfd01b8ff3e0b4a149c0be4e0d1826b68ed38f72556c4ead8f0e9bf464efc";

#[test]
fn hostile_output_inputs_refuse_typed() {
    let ok = |rgb: Vec<f32>, w, h| render_output(&image(rgb, w, h), OutputDepth::Eight, SRGB_V4, None);
    assert_eq!(ok(vec![0.0; 5], 1, 1).err(), Some(DevelopError::InvalidDimensions));
    assert_eq!(ok(vec![], 0, 0).err(), Some(DevelopError::InvalidDimensions));
    assert_eq!(ok(vec![0.0; 3], 0, 1).err(), Some(DevelopError::InvalidDimensions));
    assert_eq!(ok(vec![0.0; 3], u32::MAX, u32::MAX).err(), Some(DevelopError::InvalidDimensions));
    assert_eq!(ok(vec![0.5, f32::NAN, 0.5], 1, 1).err(), Some(DevelopError::NonFinite));
    assert_eq!(ok(vec![0.5, 0.5, f32::INFINITY], 1, 1).err(), Some(DevelopError::NonFinite));
    assert_eq!(ok(vec![f32::NEG_INFINITY, 0.5, 0.5], 1, 1).err(), Some(DevelopError::NonFinite));
    let oversized = vec![0u8; MAX_ICC_BYTES + 1];
    assert_eq!(check_icc_header(&oversized).err(), Some(DevelopError::InvalidProfile));
    // Extreme finite values clamp (counted) rather than wrap.
    let big = render_output(&image(vec![f32::MAX, f32::MIN, f32::MIN_POSITIVE], 1, 1), OutputDepth::Sixteen, SRGB_V4, None).unwrap();
    assert_eq!(big.data, vec![0xFF, 0xFF, 0, 0, 0, 0]);
    assert_eq!(big.clip, ClipReport { below_zero: 1, above_one: 1 });
}
