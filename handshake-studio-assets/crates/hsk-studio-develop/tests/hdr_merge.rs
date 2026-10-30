//! Lane SMB-HDR-MERGE-1 slice 3: bracketed raws merge through the develop stages and pigment's HDR
//! merge ([STU-RAS-033]). No real camera brackets exist in the repository: the brackets below are
//! synthesised DNGs (hsk-studio-interop-tiff's container writer, as in tests/develop_dng_write.rs)
//! of one linear scene at three exposure times, so real-bracket behaviour is NOT_PROVEN.
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_develop::color::{self, Mat3};
use hsk_studio_develop::hdr_merge::{RawBracket, RawHdrError, RawHdrOptions, merge_raw_brackets};
use hsk_studio_develop::*;
use hsk_studio_interop_tiff::{self as tiff, ChildIfds, FieldValue, IfdSpec};
use hsk_studio_pigment::hdr_merge::{HdrError, HdrLimits};

/// sRGB -> XYZ D50 colorants (literal, independent of the crate).
const SRGB_D50: Mat3 = [0.4360747, 0.3850649, 0.1430804, 0.2225045, 0.7168786, 0.0606169, 0.0139322, 0.0971045, 0.7141733];
const BLACK: u32 = 64;
const WHITE: u32 = 4095;

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

#[derive(Clone)]
struct Src {
    w: u32,
    h: u32,
    cfa: [u8; 4],
    model: &'static str,
    /// Light relative to the reference exposure.
    scale: f64,
    exposure: Option<(u32, u32)>,
    f_number: (u32, u32),
    iso: u16,
}

fn bracket(scale: f64, exposure: (u32, u32)) -> Src {
    Src {
        w: 64,
        h: 48,
        cfa: [0, 1, 1, 2],
        model: "Studio Test Camera",
        scale,
        exposure: Some(exposure),
        f_number: (80, 10),
        iso: 100,
    }
}

/// Scene radiance (grey, linear in x + y) at the reference exposure; 1.0 = sensor saturation.
fn scene(x: u32, y: u32) -> f64 {
    0.02 + 2.98 * f64::from(x + y) / f64::from(64 + 48 - 2)
}

/// A single-IFD CFA DNG (raw IFD = IFD0), uncompressed 16-bit, with an Exif IFD.
fn build(s: &Src) -> Vec<u8> {
    let span = f64::from(WHITE - BLACK);
    let samples: Vec<u16> = (0..s.h)
        .flat_map(|y| (0..s.w).map(move |x| (x, y)))
        .map(|(x, y)| (f64::from(BLACK) + ((scene(x, y) * s.scale).min(1.0) * span).round()) as u16)
        .collect();
    let cm1 = color::invert(&SRGB_D50).unwrap();
    let mut cm2 = cm1;
    cm2[0] *= 1.05;
    cm2[8] *= 0.95;
    let entries: Vec<(u16, FieldValue)> = vec![
        (256, long(s.w)),
        (257, long(s.h)),
        (258, short(16)),
        (259, short(1)),
        (262, short(32803)),
        (271, FieldValue::Ascii("StudioMake".into())),
        (272, FieldValue::Ascii(s.model.into())),
        (277, short(1)),
        (278, long(s.h)),
        (33421, FieldValue::Short(vec![2, 2])),
        (33422, FieldValue::Byte(s.cfa.to_vec())),
        (50706, FieldValue::Byte(vec![1, 4, 0, 0])),
        (50707, FieldValue::Byte(vec![1, 1, 0, 0])),
        (50708, FieldValue::Ascii(s.model.into())),
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
        (50728, rat(&[1.0, 1.0, 1.0])),
    ];
    let mut exif = vec![(33437, FieldValue::Rational(vec![s.f_number])), (34855, short(s.iso))];
    if let Some(t) = s.exposure {
        exif.push((33434, FieldValue::Rational(vec![t])));
    }
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

struct Decoded {
    raw: DngRaw,
    exif: ExifSubset,
}

fn decoded(s: &Src) -> Decoded {
    let bytes = build(s);
    Decoded {
        raw: decode_container(&bytes).unwrap(),
        exif: exif_subset(&bytes),
    }
}

fn brackets(d: &[Decoded]) -> Vec<RawBracket<'_>> {
    d.iter().map(|d| RawBracket { raw: &d.raw, exif: &d.exif }).collect()
}

fn profile() -> ProfileBinding {
    ProfileBinding {
        profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(),
        sha256: [7; 32],
    }
}

fn merge(d: &[Decoded]) -> Result<hsk_studio_develop::hdr_merge::RawHdrMerged, RawHdrError> {
    merge_raw_brackets(
        &brackets(d),
        &color::xyz_d50_to_linear_srgb().unwrap(),
        profile(),
        &RawHdrOptions::default(),
        &HdrLimits::DEFAULT,
        &CancellationToken::default(),
    )
}

/// The develop pipeline's own linear image of one bracket (native recipe: no curve, no output transform).
fn developed(d: &Decoded) -> (DevelopedImage, DevelopRecipe) {
    let (plane, _) = d
        .raw
        .cfa_plane(d.raw.default_window(), RangePolicy::ClampReported, 1, Limits::LARGE)
        .unwrap();
    let (recipe, _) = d
        .raw
        .recipe(&color::xyz_d50_to_linear_srgb().unwrap(), profile(), BaselineExposure::Ignore)
        .unwrap();
    let (image, _) = develop_to_image(&plane, &recipe, 1, &CancellationToken::default()).unwrap();
    (image, recipe)
}

#[test]
fn raw_brackets_merge_through_develop_to_linear_hdr() {
    let d = [
        decoded(&bracket(0.25, (1, 400))),
        decoded(&bracket(1.0, (1, 100))),
        decoded(&bracket(4.0, (1, 25))),
    ];
    assert_eq!(d[1].exif.exposure_time, Some((1, 100)), "EV source: EXIF ExposureTime the reader already reads");
    let merged = merge(&d).unwrap();
    assert_eq!(merged.camera.reference, 1);
    assert_eq!(merged.camera.depth, 32);
    for (got, want) in merged.camera.relative_exposure.iter().zip([0.25, 1.0, 4.0]) {
        assert!((got - want).abs() <= 1e-12, "{got} vs {want}");
    }
    let (reference, recipe) = developed(&d[1]);
    assert_eq!(merged.recipe, recipe, "the reference bracket's develop recipe");
    let (w, h) = (64u32, 48u32);
    let mut above_one = 0;
    for y in 2..h - 2 {
        for x in 2..w - 2 {
            let r = scene(x, y);
            if (0.9..=1.1).contains(&r) {
                continue; // demosaic mixes clipped and unclipped neighbours at the reference clip edge
            }
            let p = (y * w + x) as usize;
            let cam = merged.camera.frame.data[p];
            for v in &cam[..3] {
                let err = (f64::from(*v) - r).abs();
                assert!(err <= 3e-3 * r.max(1.0), "camera ({x},{y}): {v} vs scene {r}");
            }
            let work = merged.working.data[p];
            if r < 0.9 {
                // Unclipped in the reference: the develop pipeline's own linear frame.
                for (c, (got, want)) in work[..3].iter().zip(&reference.rgb[p * 3..p * 3 + 3]).enumerate() {
                    assert!((got - want).abs() <= 5e-3 * want.abs().max(1.0), "working ({x},{y}) {c}: {got} vs {want}");
                }
            } else if work[1] > 1.0 {
                above_one += 1;
            }
        }
    }
    assert!(above_one > 100, "linear scene-referred working frame keeps highlights above 1.0");
    println!("NOT_PROVEN: real camera brackets (synthesised DNG brackets of one linear scene)");
    println!("NOT_PROVEN: Lightroom Photo Merge > HDR parity (no D5 oracle)");
}

#[test]
fn identical_brackets_reproduce_the_develop_pipeline() {
    let one = decoded(&bracket(1.0, (1, 100)));
    let two = decoded(&bracket(1.0, (1, 100)));
    let merged = merge(&[one, two]).unwrap();
    let again = decoded(&bracket(1.0, (1, 100)));
    let (plane, _) = again
        .raw
        .cfa_plane(again.raw.default_window(), RangePolicy::ClampReported, 0, Limits::LARGE)
        .unwrap();
    let demosaiced = demosaic_normalized(&plane, &CancellationToken::default()).unwrap();
    for (got, want) in merged.camera.frame.data.iter().zip(&demosaiced) {
        assert_eq!(&got[..3], &want[..], "camera stage = develop's normalised demosaic");
        assert_eq!(got[3], 1.0);
    }
    let (image, _) = developed(&again);
    for (p, px) in merged.working.data.iter().enumerate() {
        for (c, (got, want)) in px[..3].iter().zip(&image.rgb[p * 3..p * 3 + 3]).enumerate() {
            assert!((got - want).abs() <= 1e-6 * want.abs().max(1.0), "working {p}/{c}: {got} vs {want}");
        }
    }
}

#[test]
fn raw_brackets_refuse_mismatched_camera_size_or_cfa() {
    let reference = || decoded(&bracket(1.0, (1, 100)));
    let with = |change: fn(&mut Src)| {
        let mut s = bracket(0.25, (1, 400));
        change(&mut s);
        decoded(&s)
    };
    let cases: Vec<(Decoded, RawHdrError)> = vec![
        (with(|s| s.model = "Other Camera"), RawHdrError::CameraMismatch { index: 1 }),
        (with(|s| s.w = 62), RawHdrError::SizeMismatch { index: 1 }),
        (with(|s| s.cfa = [1, 0, 2, 1]), RawHdrError::CfaMismatch { index: 1 }),
        (
            with(|s| s.f_number = (56, 10)),
            RawHdrError::SettingsDiffer {
                index: 1,
                field: "f_number",
            },
        ),
        (with(|s| s.iso = 400), RawHdrError::SettingsDiffer { index: 1, field: "iso" }),
        (with(|s| s.exposure = None), RawHdrError::Merge(HdrError::MissingExposure { index: 1 })),
        (with(|s| s.exposure = Some((0, 400))), RawHdrError::Merge(HdrError::InvalidExposure { index: 1 })),
    ];
    for (other, want) in cases {
        let err = merge(&[reference(), other]).unwrap_err();
        assert_eq!(err, want);
        assert!(!err.code().is_empty());
    }
    let err = merge(&[reference()]).unwrap_err();
    assert_eq!(err, RawHdrError::Merge(HdrError::TooFewFrames { count: 1 }));
    assert_eq!(err.code(), "hdr_too_few_frames");
    assert_eq!(
        RawHdrError::SettingsDiffer { index: 2, field: "iso" }.to_string(),
        "raw_hdr_settings_differ: bracket 2 iso"
    );
}
