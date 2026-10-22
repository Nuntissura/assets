//! Lane SMB-DEVELOP-EXPORT-TIFF: TIFF export of a developed image (module 14: STU-IO-136 TIFF compression
//! domain None/LZW/ZIP; STU-COL embedded profile). Reads every exported file back through hsk-studio-interop-tiff
//! and compares pixels, profile bytes and metadata. Lightroom Classic export equivalence is UNVERIFIED (D5).
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_develop::*;
use hsk_studio_interop_tiff as tiff;
use sha2::{Digest, Sha256};

const SRGB_V4: &[u8] = include_bytes!("profiles/sRGB-v4.icc");
const P3_V4: &[u8] = include_bytes!("profiles/DisplayP3-v4.icc");
const P3_LINEAR: &[u8] = include_bytes!("profiles/DisplayP3-admitted-linear-v4.icc");

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn image(rgb: Vec<f32>, width: u32, height: u32) -> DevelopedImage {
    DevelopedImage { width, height, rgb }
}

/// A deterministic non-trivial frame: gradient plus a hot and a cold pixel (clip counted).
fn frame(w: u32, h: u32) -> DevelopedImage {
    let mut rgb = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let t = (x + y * w) as f32 / (w * h) as f32;
            rgb.extend_from_slice(&[t, (t * 7.0).fract(), 1.0 - t]);
        }
    }
    rgb[0] = -0.25;
    rgb[1] = 1.75;
    image(rgb, w, h)
}

fn decode(bytes: &[u8]) -> tiff::TiffFile {
    tiff::decode(bytes, &tiff::Limits::default(), &CancellationToken::default()).expect("decode")
}

fn settings(depth: OutputDepth, space: ExportColourSpace) -> TiffExportSettings {
    TiffExportSettings::new(depth, space)
}

fn samples_of(out: &OutputImage) -> tiff::Samples {
    if out.depth_bits == 8 {
        tiff::Samples::U8(out.data.clone())
    } else {
        tiff::Samples::U16(out.data.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect())
    }
}

/// Round trip for every depth x compression x predictor x byte order x container: pixels equal the rendered
/// buffer exactly, the embedded profile is byte-identical, the settings record is what the file says.
#[test]
fn tiff_export_round_trips_pixels_profile_and_metadata_for_every_setting() {
    let img = frame(41, 23);
    for depth in [OutputDepth::Eight, OutputDepth::Sixteen] {
        let out = render_output(&img, depth, SRGB_V4, None).unwrap();
        assert_eq!(out.clip, ClipReport { below_zero: 1, above_one: 1 });
        for compression in [TiffCompression::None, TiffCompression::Lzw, TiffCompression::Deflate] {
            for predictor in [false, true] {
                if predictor && compression == TiffCompression::None {
                    continue;
                }
                for byte_order in [ByteOrder::Little, ByteOrder::Big] {
                    for big_tiff in [false, true] {
                        let mut s = settings(depth, ExportColourSpace::Srgb);
                        s.compression = compression;
                        s.predictor = predictor;
                        s.byte_order = byte_order;
                        s.big_tiff = big_tiff;
                        s.dpi = Some(300);
                        s.metadata.software = Some("hsk-studio-develop test".into());
                        let tag = format!("{depth:?} {compression:?} {predictor} {byte_order:?} {big_tiff}");
                        let export = tiff_from_output(&out, &s).unwrap_or_else(|e| panic!("{tag}: {e:?}"));
                        assert_eq!(export.tiff_sha256.as_slice(), Sha256::digest(&export.bytes).as_slice(), "{tag}");
                        assert_eq!((export.width, export.height, export.depth_bits), (41, 23, depth.bits()), "{tag}");
                        assert_eq!(export.clip, out.clip, "{tag}");
                        assert_eq!(export.icc_sha256, out.icc_sha256, "{tag}");
                        assert_eq!(export, tiff_from_output(&out, &s).unwrap(), "deterministic {tag}");
                        let file = decode(&export.bytes);
                        assert_eq!(file.byte_order, byte_order, "{tag}");
                        assert_eq!(file.big_tiff, big_tiff, "{tag}");
                        let page = &file.pages[0];
                        assert_eq!(page.pixels(), &samples_of(&out), "pixels {tag}");
                        assert_eq!((page.bits_per_sample, page.samples_per_pixel), (depth.bits().into(), 3), "{tag}");
                        assert_eq!(page.icc.as_deref(), Some(SRGB_V4), "icc {tag}");
                        assert_eq!(page.resolution.map(|r| (r.x, r.y, r.unit)), Some(((300, 1), (300, 1), 2)), "{tag}");
                        assert_eq!(page.predictor, if predictor { 2 } else { 1 }, "{tag}");
                        let c = tiff::Container::parse(&export.bytes, &tiff::Limits::default()).unwrap();
                        let ifds = c.walk().unwrap();
                        let sw = ifds[0].find(305).expect("Software tag");
                        assert_eq!(c.ascii(sw).unwrap(), "hsk-studio-develop test", "{tag}");
                    }
                }
            }
        }
    }
}

/// 16-bit export keeps precision a 8-bit export cannot: a dense linear ramp has more than 256 distinct codes,
/// is monotone, and every file sample equals an independent f64 evaluation of the IEC 61966-2-1 OETF within
/// one code. The 8-bit file of the same ramp agrees with the 16-bit one within one 8-bit step.
#[test]
fn sixteen_bit_export_keeps_precision_and_matches_the_reference_curve() {
    let n = 4096_u32;
    let ramp: Vec<f32> = (0..n).flat_map(|i| [i as f32 / (n - 1) as f32; 3]).collect();
    let img = image(ramp.clone(), n, 1);
    let files: Vec<tiff::TiffFile> = [OutputDepth::Eight, OutputDepth::Sixteen]
        .into_iter()
        .map(|d| {
            let out = render_output(&img, d, SRGB_V4, None).unwrap();
            let mut s = settings(d, ExportColourSpace::Srgb);
            s.compression = TiffCompression::Deflate;
            s.predictor = true;
            decode(&tiff_from_output(&out, &s).unwrap().bytes)
        })
        .collect();
    let (tiff::Samples::U8(a), tiff::Samples::U16(b)) = (files[0].pages[0].pixels(), files[1].pages[0].pixels()) else {
        panic!("sample types");
    };
    let oetf = |v: f64| if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
    let mut distinct = std::collections::BTreeSet::new();
    let mut last = 0_u16;
    for i in 0..n as usize {
        let want16 = oetf(f64::from(ramp[i * 3])) * 65535.0;
        let got16 = b[i * 3];
        assert!((f64::from(got16) - want16).abs() <= 1.0, "16-bit at {i}: {got16} vs {want16}");
        assert!(got16 >= last, "monotone at {i}");
        last = got16;
        distinct.insert(got16);
        let want8 = oetf(f64::from(ramp[i * 3])) * 255.0;
        assert!((f64::from(a[i * 3]) - want8).abs() <= 1.0, "8-bit at {i}");
    }
    assert!(distinct.len() > 1000, "16-bit export keeps more than 8 bits: {} codes", distinct.len());
}

/// Colour space choice: sRGB and Display P3 embed their own profile (checked by colorants and curve);
/// a profile that is not the named space, a linear profile, Adobe RGB and ProPhoto are typed refusals.
#[test]
fn colour_space_choice_embeds_the_matching_profile_and_refuses_the_rest() {
    let img = frame(5, 4);
    let render = |icc: &[u8]| render_output(&img, OutputDepth::Sixteen, icc, None).unwrap();
    let srgb = render(SRGB_V4);
    let p3 = render(P3_V4);
    let ok = tiff_from_output(&p3, &settings(OutputDepth::Sixteen, ExportColourSpace::DisplayP3)).unwrap();
    assert_eq!(decode(&ok.bytes).pages[0].icc.as_deref(), Some(P3_V4));
    assert_eq!(ok.colour_space, ExportColourSpace::DisplayP3);
    let err = |out: &OutputImage, space| tiff_from_output(out, &settings(OutputDepth::Sixteen, space)).unwrap_err();
    let e = err(&srgb, ExportColourSpace::DisplayP3);
    assert_eq!((e.code(), e.detail()), ("profile_colour_space_mismatch", "colorants_differ"));
    let e = err(&p3, ExportColourSpace::Srgb);
    assert_eq!((e.code(), e.detail()), ("profile_colour_space_mismatch", "colorants_differ"));
    // A linear-light P3 profile has the right colorants but not the transfer the pixels were encoded with.
    let lin = render_output(&img, OutputDepth::Sixteen, P3_LINEAR, None).unwrap();
    let e = err(&lin, ExportColourSpace::DisplayP3);
    assert_eq!((e.code(), e.detail()), ("profile_colour_space_mismatch", "transfer_is_not_srgb_curve"));
    // No Adobe RGB / ProPhoto profile exists in the corpus: the refusal names the missing profile.
    for (space, name) in [(ExportColourSpace::AdobeRgb, "adobe_rgb_1998"), (ExportColourSpace::ProPhoto, "prophoto_rgb")] {
        let e = err(&srgb, space);
        assert_eq!((e.code(), e.detail()), ("colour_space_profile_missing", name));
    }
    // A header-valid profile with garbage tag table is a mismatch, never a panic.
    let mut junk = SRGB_V4.to_vec();
    junk[128..132].copy_from_slice(&u32::MAX.to_be_bytes());
    let mut out = srgb.clone();
    out.icc = junk;
    assert_eq!(tiff_from_output(&out, &settings(OutputDepth::Sixteen, ExportColourSpace::Srgb)).unwrap_err().code(), "profile_colour_space_mismatch");
}

/// Settings and image refusals, all before any sample buffer or file is built.
#[test]
fn settings_and_hostile_images_are_typed_refusals() {
    let out = render_output(&frame(3, 3), OutputDepth::Eight, SRGB_V4, None).unwrap();
    let base = settings(OutputDepth::Eight, ExportColourSpace::Srgb);
    let code = |s: &TiffExportSettings| tiff_from_output(&out, s).unwrap_err().code();
    let mut s = base.clone();
    s.predictor = true;
    assert_eq!(code(&s), "predictor_requires_compression");
    for dpi in [0, MAX_DPI + 1, u32::MAX] {
        let mut s = base.clone();
        s.dpi = Some(dpi);
        assert_eq!(code(&s), "invalid_resolution");
    }
    let mut s = base.clone();
    s.metadata.artist = Some("caf\u{e9}".into());
    assert_eq!(code(&s), "metadata_not_ascii");
    let s16 = settings(OutputDepth::Sixteen, ExportColourSpace::Srgb);
    assert_eq!(code(&s16), "depth_mismatch");
    // Dimensions that do not match the data are refused with checked arithmetic, never an allocation.
    let hostile = |w: u32, h: u32| {
        let mut o = out.clone();
        (o.width, o.height) = (w, h);
        tiff_from_output(&o, &base).unwrap_err().code()
    };
    assert_eq!(hostile(0, 3), "invalid_dimensions");
    assert_eq!(hostile(3, 0), "invalid_dimensions");
    assert_eq!(hostile(100_000, 100_000), "invalid_dimensions");
    assert_eq!(hostile(u32::MAX, u32::MAX), "dims_overflow");
    // A bad profile header is the develop code.
    let mut o = out.clone();
    o.icc.truncate(100);
    assert_eq!(tiff_from_output(&o, &base).unwrap_err().code(), DevelopError::InvalidProfile.code());
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

/// A corpus DNG developed end to end into 16-bit ZIP+predictor TIFF equals develop_to_output's buffer, for every
/// thread count, and a changed recipe changes the file (NOT_PROVEN line when the corpus is absent).
#[test]
fn corpus_dng_develops_to_a_tiff_equal_to_the_output_buffer() {
    const TEST: &str = "corpus_dng_develops_to_a_tiff_equal_to_the_output_buffer";
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
    let cancel = CancellationToken::default();
    let mut s = settings(OutputDepth::Sixteen, ExportColourSpace::Srgb);
    s.compression = TiffCompression::Deflate;
    s.predictor = true;
    s.dpi = Some(240);
    let (export, receipt) = develop_to_tiff(&plane, &recipe, 1, &s, SRGB_V4, Threads::auto(), &cancel).unwrap();
    let (reference, _) = develop_to_output(&plane, &recipe, 1, OutputDepth::Sixteen, SRGB_V4, Threads::single(), &cancel).unwrap();
    assert_eq!((export.width, export.height), (reference.width, reference.height));
    assert_eq!(receipt.output.width, export.width);
    let file = decode(&export.bytes);
    assert_eq!(file.pages[0].pixels(), &samples_of(&reference), "TIFF pixels equal develop_to_output");
    assert_eq!(file.pages[0].icc.as_deref(), Some(SRGB_V4));
    assert_eq!(export.clip, reference.clip);
    let single = develop_to_tiff(&plane, &recipe, 1, &s, SRGB_V4, Threads::single(), &cancel).unwrap().0;
    assert_eq!(single.bytes, export.bytes, "thread count does not change the file");
    let mut brighter = recipe.clone();
    brighter.exposure_stops = 1.0;
    let other = develop_to_tiff(&plane, &brighter, 1, &s, SRGB_V4, Threads::auto(), &cancel).unwrap().0;
    assert_ne!(other.tiff_sha256, export.tiff_sha256);
    // Refusals before development: wrong colour space for the profile, and Adobe RGB.
    let mut p3 = s.clone();
    p3.colour_space = ExportColourSpace::DisplayP3;
    assert_eq!(develop_to_tiff(&plane, &recipe, 1, &p3, SRGB_V4, Threads::single(), &cancel).unwrap_err().code(), "profile_colour_space_mismatch");
    let mut adobe = s.clone();
    adobe.colour_space = ExportColourSpace::AdobeRgb;
    let e = develop_to_tiff(&plane, &recipe, 1, &adobe, SRGB_V4, Threads::single(), &cancel).unwrap_err();
    assert_eq!((e.code(), e.detail()), ("colour_space_profile_missing", "adobe_rgb_1998"));
    println!("corpus tiff {}x{} bytes={} sha256={}", export.width, export.height, export.bytes.len(), hex(&export.tiff_sha256));
}
