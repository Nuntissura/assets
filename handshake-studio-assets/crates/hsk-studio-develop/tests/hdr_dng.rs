//! Lane SMB-HDR-MERGE-2: the merged HDR frame as a floating-point LinearRaw DNG (Adobe DNG
//! specification 1.4.0.0: SampleFormat 3, BitsPerSample 32, WhiteLevel default 1.0 for floating
//! point, DNGBackwardVersion at least 1.4.0.0), read back by develop's own reader; integer DNG output
//! unchanged. Brackets are synthesised DNGs (as in tests/hdr_merge.rs): real brackets NOT_PROVEN.
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_develop::color::{self, Mat3};
use hsk_studio_develop::hdr_merge::{RawBracket, RawHdrMerged, RawHdrOptions, merge_raw_brackets};
use hsk_studio_develop::*;
use hsk_studio_interop_tiff::{self as tiff, ChildIfds, Container, FieldValue, IfdSpec};
use hsk_studio_pigment::hdr_merge::HdrLimits;
use sha2::{Digest, Sha256};

/// sRGB -> XYZ D50 colorants (literal, independent of the crate).
const SRGB_D50: Mat3 = [0.4360747, 0.3850649, 0.1430804, 0.2225045, 0.7168786, 0.0606169, 0.0139322, 0.0971045, 0.7141733];
const BLACK: u32 = 64;
const WHITE: u32 = 4095;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn srat(v: &[f64]) -> FieldValue {
    FieldValue::SRational(v.iter().map(|x| ((x * 10_000_000.0).round() as i32, 10_000_000)).collect())
}

fn rat(v: &[f64]) -> FieldValue {
    FieldValue::Rational(v.iter().map(|x| ((x * 1_000_000.0).round() as u32, 1_000_000)).collect())
}

fn short(v: u16) -> FieldValue {
    FieldValue::Short(vec![v])
}

fn long(v: u32) -> FieldValue {
    FieldValue::Long(vec![v])
}

/// Scene radiance (grey, linear in x + y) at the reference exposure; 1.0 = sensor saturation.
fn scene(x: u32, y: u32) -> f64 {
    0.02 + 2.98 * f64::from(x + y) / f64::from(64 + 48 - 2)
}

/// A 64x48 RGGB DNG of the scene at `scale` light and exposure time `exposure`, with an Exif IFD.
fn build(scale: f64, exposure: (u32, u32)) -> Vec<u8> {
    let (w, h) = (64u32, 48u32);
    let span = f64::from(WHITE - BLACK);
    let samples: Vec<u16> = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| (f64::from(BLACK) + ((scene(x, y) * scale).min(1.0) * span).round()) as u16)
        .collect();
    let cm1 = color::invert(&SRGB_D50).unwrap();
    let mut cm2 = cm1;
    cm2[0] *= 1.05;
    cm2[8] *= 0.95;
    let entries: Vec<(u16, FieldValue)> = vec![
        (256, long(w)),
        (257, long(h)),
        (258, short(16)),
        (259, short(1)),
        (262, short(32803)),
        (271, FieldValue::Ascii("StudioMake".into())),
        (272, FieldValue::Ascii("Studio Test Camera".into())),
        (277, short(1)),
        (278, long(h)),
        (306, FieldValue::Ascii("2026:10:30 09:08:07".into())),
        (33421, FieldValue::Short(vec![2, 2])),
        (33422, FieldValue::Byte(vec![0, 1, 1, 2])),
        (50706, FieldValue::Byte(vec![1, 4, 0, 0])),
        (50707, FieldValue::Byte(vec![1, 1, 0, 0])),
        (50708, FieldValue::Ascii("Studio Test Camera".into())),
        (50713, FieldValue::Short(vec![1, 1])),
        (50714, FieldValue::Rational(vec![(BLACK * 4, 4)])),
        (50717, long(WHITE)),
        (50721, srat(&cm1)),
        (50722, srat(&cm2)),
        (50778, short(17)),
        (50779, short(21)),
        (50964, srat(&SRGB_D50)),
        (50965, srat(&SRGB_D50)),
        (50727, rat(&[1.0, 1.0, 1.0])),
        (50728, rat(&[0.5, 1.0, 0.8])),
        (50730, srat(&[0.25])),
    ];
    let exif = vec![
        (33434, FieldValue::Rational(vec![exposure])),
        (33437, FieldValue::Rational(vec![(80, 10)])),
        (34855, short(100)),
    ];
    let children = vec![ChildIfds {
        tag: 34665,
        ifds: vec![IfdSpec {
            entries: exif,
            ..IfdSpec::default()
        }],
    }];
    let strip: Vec<u8> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
    let spec = IfdSpec {
        entries,
        strips: vec![strip],
        children,
    };
    tiff::write_container(&spec, 1 << 28).unwrap()
}

/// SHA-256 of `dng_from_raw` output (uncompressed, no preview, with Exif) for the fixed synthetic
/// raw, captured on the unchanged base b64650e8 before this lane touched src/dng_write.rs or
/// src/dng.rs (baseline run log lane-check-smb-hdr-merge-2-baseline-b64650e8.log).
const INTEGER_PINS: [(DngCompression, &str); 1] =
    [(DngCompression::None, "c3a0726ff37080a1c6532634c49921487dcf7a9f9901e354eb579373dd5a230e")];

#[test]
fn integer_dng_output_is_byte_identical() {
    let bytes = build(1.0, (1, 100));
    let raw = decode_container(&bytes).unwrap();
    let exif = exif_subset(&bytes);
    for (compression, pin) in INTEGER_PINS {
        let settings = DngExportSettings {
            compression,
            preview: DngPreview::None,
            ..DngExportSettings::default()
        };
        let parts = DngParts {
            exif: Some(&exif),
            ..DngParts::default()
        };
        let out = dng_from_raw(&raw, &settings, &parts).unwrap();
        let digest = hex(&Sha256::digest(&out.bytes));
        println!("integer_dng {compression:?} bytes={} sha256={digest}", out.bytes.len());
        assert_eq!(digest, pin, "integer DNG output changed ({compression:?})");
    }
}

fn profile() -> ProfileBinding {
    ProfileBinding {
        profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(),
        sha256: [7; 32],
    }
}

struct Decoded {
    bytes: Vec<u8>,
    raw: DngRaw,
    exif: ExifSubset,
}

fn decoded(scale: f64, exposure: (u32, u32)) -> Decoded {
    let bytes = build(scale, exposure);
    Decoded {
        raw: decode_container(&bytes).unwrap(),
        exif: exif_subset(&bytes),
        bytes,
    }
}

fn merged(d: &[Decoded]) -> RawHdrMerged {
    let brackets: Vec<RawBracket<'_>> = d.iter().map(|d| RawBracket { raw: &d.raw, exif: &d.exif }).collect();
    merge_raw_brackets(
        &brackets,
        &color::xyz_d50_to_linear_srgb().unwrap(),
        profile(),
        &RawHdrOptions::default(),
        &HdrLimits::DEFAULT,
        &CancellationToken::default(),
    )
    .unwrap()
}

fn plain() -> DngExportSettings {
    DngExportSettings {
        preview: DngPreview::None,
        ..DngExportSettings::default()
    }
}

#[test]
fn hdr_dng_round_trips_bit_exact_through_develops_reader() {
    let d = [decoded(0.25, (1, 400)), decoded(1.0, (1, 100)), decoded(4.0, (1, 25))];
    let m = merged(&d);
    let reference = &d[m.camera.reference];
    let parts = DngParts {
        exif: Some(&reference.exif),
        ..DngParts::default()
    };
    let out = dng_from_hdr(&m, &reference.raw, &plain(), &parts).unwrap();
    assert_eq!((out.width, out.height), (64, 48));
    // The merge reaches about 3.0 (beyond display range): two stops down fit DNG's 0..1 clip.
    assert_eq!(out.scale_stops, 2);
    assert_eq!(out.baseline_exposure, 0.25 + 2.0);
    assert_eq!(out.exif_fields, vec!["make", "model", "date_time", "exposure_time", "f_number", "iso"]);
    assert_eq!(out.dng_sha256.as_slice(), Sha256::digest(&out.bytes).as_slice());

    // Develop's own reader: bit-exact samples, and 2^scale_stops restores the merged radiance exactly.
    let back = decode_linear_float(&out.bytes).unwrap();
    assert_eq!((back.width, back.height), (64, 48));
    assert_eq!(back.samples.len(), 64 * 48 * 3);
    let restore = (1u32 << out.scale_stops) as f32;
    let mut above_one = 0;
    for (px, stored) in m.camera.frame.data.iter().zip(back.samples.chunks_exact(3)) {
        for (s, v) in stored.iter().zip(&px[..3]) {
            assert_eq!((s * restore).to_bits(), v.to_bits(), "radiance restored bit for bit");
            assert!((0.0..=1.0).contains(s));
        }
        if px[1] > 1.0 {
            above_one += 1;
        }
    }
    assert!(above_one > 100, "the file carries radiance beyond display range");
    let le: Vec<u8> = back.samples.iter().flat_map(|v| v.to_le_bytes()).collect();
    assert_eq!(out.samples_sha256.as_slice(), Sha256::digest(&le).as_slice());
    assert_eq!((back.black_level.clone(), back.white_level.clone()), (vec![0.0], vec![1.0]));
    assert_eq!(back.backward_version, DNG_VERSION);
    assert_eq!(back.unique_camera_model.as_deref(), Some("Studio Test Camera"));
    assert_eq!(back.color.baseline_exposure, 2.25);
    let near = |a: &[f64], b: &[f64]| a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() <= 1e-6);
    assert!(near(&back.color.as_shot_neutral.unwrap(), &reference.raw.color.as_shot_neutral.unwrap()));
    let (got, want) = (&back.color, &reference.raw.color);
    for (a, b) in got.color_matrix.iter().chain(&got.forward_matrix).zip(want.color_matrix.iter().chain(&want.forward_matrix)) {
        assert!(near(&a.unwrap(), &b.unwrap()));
    }

    // The tags DNG 1.4 requires for floating point, read by the independent interop-tiff facade.
    let c = Container::parse(&out.bytes, &tiff::Limits::default()).unwrap();
    let ifd0 = c.ifd(c.first_ifd_offset()).unwrap();
    let u = |t: u16| c.uints(ifd0.find(t).unwrap_or_else(|| panic!("tag {t} missing"))).unwrap();
    assert_eq!((u(254), u(262), u(277), u(259), u(284)), (vec![0], vec![34892], vec![3], vec![1], vec![1]));
    assert_eq!((u(258), u(339)), (vec![32, 32, 32], vec![3, 3, 3]));
    assert_eq!(c.bytes(ifd0.find(50706).unwrap()).unwrap(), [1, 4, 0, 0]);
    assert_eq!(c.bytes(ifd0.find(50707).unwrap()).unwrap(), [1, 4, 0, 0]);
    assert!(ifd0.find(50717).is_none(), "no WhiteLevel: the floating-point default 1.0 applies");
    assert!(ifd0.find(33422).is_none(), "LinearRaw carries no CFA pattern");

    // The CFA reader keeps refusing LinearRaw; the float reader refuses CFA files.
    assert_eq!(decode_container(&out.bytes).unwrap_err(), DevelopError::UnsupportedContainer);
    assert_eq!(decode_linear_float(&reference.bytes).unwrap_err(), DevelopError::UnsupportedContainer);
    println!("NOT_PROVEN: Lightroom / Adobe DNG Converter reading of this float DNG (no D5 oracle)");
    println!("NOT_PROVEN: real camera brackets (synthesised DNG brackets of one linear scene)");
}

#[test]
fn hdr_dng_refuses_unsupported_settings_and_hostile_files() {
    let d = [decoded(0.25, (1, 400)), decoded(1.0, (1, 100))];
    let m = merged(&d);
    let source = &d[m.camera.reference].raw;
    let none = DngParts::default();
    let code = |r: Result<HdrDngExport, ExportError>| r.unwrap_err().code();
    let lossless = DngExportSettings {
        compression: DngCompression::LosslessJpeg,
        ..plain()
    };
    assert_eq!(code(dng_from_hdr(&m, source, &lossless, &none)), "hdr_compression_unsupported");
    assert_eq!(code(dng_from_hdr(&m, source, &DngExportSettings::default(), &none)), "hdr_preview_unsupported");
    let mut wrong_size = m.clone();
    wrong_size.camera.frame.data.pop();
    assert_eq!(code(dng_from_hdr(&wrong_size, source, &plain(), &none)), "frame_length_mismatch");
    let mut nan = m.clone();
    nan.camera.frame.data[5][1] = f32::NAN;
    assert_eq!(code(dng_from_hdr(&nan, source, &plain(), &none)), DevelopError::NonFinite.code());
    let mut moved = m.clone();
    moved.window.width = 62;
    assert_eq!(code(dng_from_hdr(&moved, source, &plain(), &none)), "hdr_source_mismatch");

    // A frame already inside 0..1 is written unscaled.
    let mut dim = m.clone();
    for px in &mut dim.camera.frame.data {
        for v in &mut px[..3] {
            *v = (*v / 8.0).min(1.0);
        }
    }
    let out = dng_from_hdr(&dim, source, &plain(), &none).unwrap();
    assert_eq!((out.scale_stops, out.baseline_exposure), (0, 0.25));

    // Hostile files: truncation and a forged sample format are refused, never read past the end.
    let good = dng_from_hdr(&m, source, &plain(), &none).unwrap().bytes;
    for cut in [8usize, 64, good.len() / 2, good.len() - 4] {
        assert!(decode_linear_float(&good[..cut]).is_err(), "truncated at {cut}");
    }
    let c = Container::parse(&good, &tiff::Limits::default()).unwrap();
    let ifd0 = c.ifd(c.first_ifd_offset()).unwrap();
    let entry = ifd0.find(339).unwrap();
    // SampleFormat (3 SHORTs) does not fit the 4-byte entry, so it sits at its value offset.
    let at = usize::try_from(entry.value_offset).unwrap();
    let mut forged = good.clone();
    forged[at..at + 2].copy_from_slice(&1u16.to_le_bytes());
    assert_eq!(decode_linear_float(&forged).unwrap_err(), DevelopError::UnsupportedContainer);
}
