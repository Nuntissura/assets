//! Lane SMB-DEVELOP-DNG-WRITER: DNG export of a decoded raw document (module 14: STU-RAW-017 raw-side export
//! facts; STU-RAW-103 interchange export). Source files are synthesized with hsk-studio-interop-tiff's container
//! writer (independent of dng_write.rs), the exported files are read back by develop's own DNG reader and by the
//! read-only interop-tiff container facade. Corpus cases print NOT_PROVEN when the corpus is absent (V4-03).
//! Lightroom Classic / Adobe DNG Converter equivalence is UNVERIFIED (no oracle, D5).
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_develop::color::{self, Mat3};
use hsk_studio_develop::dng_write::{MAX_TEXT_BYTES, MAX_XMP_BYTES};
use hsk_studio_develop::*;
use hsk_studio_interop_tiff::{self as tiff, ChildIfds, Container, FieldValue, IfdSpec};
use sha2::{Digest, Sha256};

const SRGB_V4: &[u8] = include_bytes!("profiles/sRGB-v4.icc");

/// sRGB -> XYZ D50 colorants (literal, independent of the crate).
const SRGB_D50: Mat3 = [0.4360747, 0.3850649, 0.1430804, 0.2225045, 0.7168786, 0.0606169, 0.0139322, 0.0971045, 0.7141733];

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

#[derive(Clone)]
struct Src {
    w: u32,
    h: u32,
    cfa: [u8; 4],
    white: u32,
    black: Vec<f64>,
    black_dim: (u16, u16),
    orientation: u16,
    opcode_list: bool,
    colour: bool,
    neutral: bool,
    model: Option<&'static str>,
    exif: bool,
    active: Option<[u32; 4]>,
    crop: Option<([u32; 2], [u32; 2])>,
    seed: u64,
}

fn base() -> Src {
    Src {
        w: 64,
        h: 48,
        cfa: [0, 1, 1, 2],
        white: 4095,
        black: vec![64.0],
        black_dim: (1, 1),
        orientation: 1,
        opcode_list: false,
        colour: true,
        neutral: true,
        model: Some("Studio Test Camera"),
        exif: false,
        active: None,
        crop: None,
        seed: 0x1234_5678_9ABC_DEF1,
    }
}

fn xorshift(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

/// Smooth gradient plus noise inside [black, white].
fn samples_of(s: &Src) -> Vec<u16> {
    let mut seed = s.seed;
    let span = s.white - 128;
    (0..s.h)
        .flat_map(|y| (0..s.w).map(move |x| (x, y)))
        .map(|(x, y)| {
            let smooth = 64 + (u64::from(x + y) * u64::from(span) / u64::from(s.w + s.h)) as u32;
            let noise = (xorshift(&mut seed) % 31) as u32;
            (smooth + noise).min(s.white) as u16
        })
        .collect()
}

fn exif_child() -> IfdSpec {
    IfdSpec {
        entries: vec![
            (33434, FieldValue::Rational(vec![(1, 125)])),
            (33437, FieldValue::Rational(vec![(28, 10)])),
            (34855, short(800)),
            (36867, FieldValue::Ascii("2026:10:21 09:08:07".into())),
            (37386, FieldValue::Rational(vec![(850, 10)])),
            (42036, FieldValue::Ascii("Studio Lens 24-70".into())),
        ],
        ..IfdSpec::default()
    }
}

/// A single-IFD CFA DNG (raw IFD = IFD0), uncompressed 16-bit.
fn build(s: &Src) -> (Vec<u8>, Vec<u16>) {
    let samples = samples_of(s);
    let mut e: Vec<(u16, FieldValue)> = vec![
        (256, long(s.w)),
        (257, long(s.h)),
        (258, short(16)),
        (259, short(1)),
        (262, short(32803)),
        (277, short(1)),
        (278, long(s.h)),
        (33421, FieldValue::Short(vec![2, 2])),
        (33422, FieldValue::Byte(s.cfa.to_vec())),
        (50706, FieldValue::Byte(vec![1, 4, 0, 0])),
        (50707, FieldValue::Byte(vec![1, 1, 0, 0])),
        (50713, FieldValue::Short(vec![s.black_dim.0, s.black_dim.1])),
        (50714, FieldValue::Rational(s.black.iter().map(|b| ((b * 4.0).round() as u32, 4)).collect())),
        (50717, long(s.white)),
        (274, short(s.orientation)),
    ];
    if let Some(m) = s.model {
        e.push((50708, FieldValue::Ascii(m.into())));
    }
    if s.colour {
        let cm1 = color::invert(&SRGB_D50).unwrap();
        let mut cm2 = cm1;
        cm2[0] *= 1.05;
        cm2[8] *= 0.95;
        e.push((50721, srat(&cm1)));
        e.push((50722, srat(&cm2)));
        e.push((50778, short(17)));
        e.push((50779, short(21)));
        e.push((50964, srat(&SRGB_D50)));
        e.push((50965, srat(&SRGB_D50)));
        e.push((50730, srat(&[0.25])));
        e.push((50727, rat(&[1.0, 1.0, 1.0])));
    }
    if s.neutral {
        e.push((50728, rat(&[1.0, 1.0, 1.0])));
    }
    if let Some(a) = s.active {
        e.push((50829, FieldValue::Long(a.to_vec())));
    }
    if let Some((o, z)) = s.crop {
        e.push((50719, FieldValue::Long(o.to_vec())));
        e.push((50720, FieldValue::Long(z.to_vec())));
    }
    if s.opcode_list {
        e.push((51009, FieldValue::Undefined(vec![0, 0, 0, 1, 0, 0, 0, 0])));
    }
    let mut children = Vec::new();
    if s.exif {
        e.push((271, FieldValue::Ascii("StudioMake".into())));
        e.push((272, FieldValue::Ascii("Studio Test Camera".into())));
        e.push((306, FieldValue::Ascii("2026:10:21 09:08:09".into())));
        children.push(ChildIfds { tag: 34665, ifds: vec![exif_child()] });
    }
    let strip: Vec<u8> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
    let spec = IfdSpec { entries: e, strips: vec![strip], children };
    (tiff::write_container(&spec, 1 << 28).unwrap(), samples)
}

fn raw_of(s: &Src) -> (DngRaw, Vec<u16>) {
    let (bytes, samples) = build(s);
    (decode_container(&bytes).unwrap(), samples)
}

fn no_preview() -> DngExportSettings {
    DngExportSettings { preview: DngPreview::None, ..DngExportSettings::default() }
}

fn export(raw: &DngRaw, settings: &DngExportSettings) -> DngExport {
    dng_from_raw(raw, settings, &DngParts::default()).unwrap()
}

fn code(r: Result<DngExport, ExportError>) -> &'static str {
    r.unwrap_err().code()
}

fn assert_same_document(a: &DngRaw, b: &DngRaw) {
    assert_eq!((a.width, a.height), (b.width, b.height));
    assert_eq!(a.cfa_pattern, b.cfa_pattern);
    assert_eq!(a.active_area, b.active_area);
    assert_eq!(a.default_window(), b.default_window());
    assert_eq!(a.white_level, b.white_level);
    assert_eq!(a.black_repeat, b.black_repeat);
    assert_eq!(a.black_level.len(), b.black_level.len());
    for (x, y) in a.black_level.iter().zip(&b.black_level) {
        assert!((x - y).abs() < 1e-6, "black {x} vs {y}");
    }
    assert_eq!(a.black_delta_h.len(), b.black_delta_h.len());
    assert_eq!(a.black_delta_v.len(), b.black_delta_v.len());
    assert_eq!(a.orientation, b.orientation);
    assert_eq!(a.unique_camera_model, b.unique_camera_model);
    assert_eq!(a.samples(), b.samples(), "raw data equals sample for sample");
    // Colour tags.
    for i in 0..2 {
        assert_eq!(a.color.calibration_illuminant[i], b.color.calibration_illuminant[i]);
        for (name, x, y) in [
            ("color_matrix", a.color.color_matrix[i], b.color.color_matrix[i]),
            ("forward_matrix", a.color.forward_matrix[i], b.color.forward_matrix[i]),
            ("camera_calibration", a.color.camera_calibration[i], b.color.camera_calibration[i]),
        ] {
            match (x, y) {
                (None, None) => {}
                (Some(x), Some(y)) => x.iter().zip(&y).for_each(|(p, q)| assert!((p - q).abs() < 1e-6, "{name}[{i}] {p} vs {q}")),
                _ => panic!("{name}[{i}] presence differs"),
            }
        }
    }
    for (x, y) in a.color.analog_balance.iter().zip(&b.color.analog_balance) {
        assert!((x - y).abs() < 1e-6);
    }
    assert_eq!(a.color.as_shot_neutral.is_some(), b.color.as_shot_neutral.is_some());
    if let (Some(x), Some(y)) = (a.color.as_shot_neutral, b.color.as_shot_neutral) {
        x.iter().zip(&y).for_each(|(p, q)| assert!((p - q).abs() < 1e-6));
    }
    assert!((a.color.baseline_exposure - b.color.baseline_exposure).abs() < 1e-6);
}

#[test]
fn uncompressed_dng_round_trips_samples_and_tags() {
    let mut s = base();
    s.black = vec![64.0, 64.5, 63.5, 64.0];
    s.black_dim = (2, 2);
    s.orientation = 6;
    s.active = Some([0, 0, 48, 64]);
    s.crop = Some(([2, 4], [58, 40]));
    let (raw, _) = raw_of(&s);
    assert_eq!(raw.unapplied, vec!["orientation"]);
    let out = export(&raw, &no_preview());
    assert_eq!((out.bits_per_sample, out.compression, out.preview), (16, DngCompression::None, None));
    let back = decode_container(&out.bytes).unwrap();
    assert_same_document(&raw, &back);
    assert_eq!(back.default_window(), raw.default_window());
    assert_eq!(back.orientation, 6);
    assert_eq!(out.dng_sha256.as_slice(), Sha256::digest(&out.bytes).as_slice());
    // The reported frame digest is the digest of the source samples as little-endian u16.
    let le: Vec<u8> = raw.samples().iter().flat_map(|v| v.to_le_bytes()).collect();
    assert_eq!(out.samples_sha256.as_slice(), Sha256::digest(&le).as_slice());
    // Deterministic bytes.
    assert_eq!(export(&raw, &no_preview()).bytes, out.bytes);
}

#[test]
fn lossless_jpeg_dng_round_trips_exactly_and_is_smaller() {
    let (raw, _) = raw_of(&Src { w: 256, h: 128, ..base() });
    let none = export(&raw, &no_preview());
    let settings = DngExportSettings { compression: DngCompression::LosslessJpeg, ..no_preview() };
    let ljpeg = export(&raw, &settings);
    assert_eq!(ljpeg.compression, DngCompression::LosslessJpeg);
    assert_eq!(ljpeg.samples_sha256, none.samples_sha256);
    let back = decode_container(&ljpeg.bytes).unwrap();
    assert_same_document(&raw, &back);
    assert!(ljpeg.bytes.len() < none.bytes.len(), "lossless JPEG {} < uncompressed {}", ljpeg.bytes.len(), none.bytes.len());
    // The raw IFD says compression 7 and the SOF3 stream precision.
    let c = Container::parse(&ljpeg.bytes, &tiff::Limits::default()).unwrap();
    let ifds = c.walk().unwrap();
    assert_eq!(c.uints(ifds[0].find(259).unwrap()).unwrap(), vec![7]);
    assert_eq!(c.uints(ifds[0].find(258).unwrap()).unwrap(), vec![u64::from(ljpeg.bits_per_sample)]);
}

#[test]
fn file_structure_has_the_dng_profile_tags() {
    let mut s = base();
    s.exif = true;
    let (raw, _) = raw_of(&s);
    let exif = exif_subset(&build(&s).0);
    assert_eq!(exif.skipped, None);
    let xmp = b"<?xpacket begin=\"\xEF\xBB\xBF\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description xmlns:crs=\"http://ns.adobe.com/camera-raw-settings/1.0/\" crs:Exposure2012=\"+0.50\" crs:Contrast2012=\"+10\"/></rdf:RDF></x:xmpmeta>\n<?xpacket end=\"w\"?>";
    let preview = PreviewImage { width: 8, height: 6, rgb: (0..8 * 6 * 3).map(|i| (i * 5) as u8).collect() };
    let settings = DngExportSettings { software: Some("Handshake Studio develop".into()), ..DngExportSettings::default() };
    let out = dng_from_raw(&raw, &settings, &DngParts { xmp: Some(xmp), exif: Some(&exif), preview: Some(&preview) }).unwrap();
    assert_eq!(out.preview, Some((8, 6)));
    assert_eq!(out.xmp_bytes, xmp.len());
    assert_eq!(out.exif_fields, vec!["make", "model", "date_time", "exposure_time", "f_number", "iso", "date_time_original", "focal_length", "lens_model"]);
    let c = Container::parse(&out.bytes, &tiff::Limits::default()).unwrap();
    let ifds = c.walk().unwrap();
    assert_eq!(ifds.len(), 2, "IFD0 (preview) and the raw SubIFD");
    let (ifd0, raw_ifd) = (&ifds[0], &ifds[1]);
    let u = |ifd: &tiff::Ifd, t: u16| c.uints(ifd.find(t).unwrap_or_else(|| panic!("tag {t} missing"))).unwrap();
    // IFD0: reduced-resolution RGB preview with the DNG tags.
    assert_eq!((u(ifd0, 254), u(ifd0, 262), u(ifd0, 277), u(ifd0, 259)), (vec![1], vec![2], vec![3], vec![1]));
    assert_eq!((u(ifd0, 256), u(ifd0, 257), u(ifd0, 258)), (vec![8], vec![6], vec![8, 8, 8]));
    assert_eq!(c.bytes(ifd0.find(50706).unwrap()).unwrap(), DNG_VERSION);
    assert_eq!(c.bytes(ifd0.find(50707).unwrap()).unwrap(), DNG_VERSION);
    assert_eq!(c.ascii(ifd0.find(50708).unwrap()).unwrap(), "Studio Test Camera");
    assert_eq!(c.ascii(ifd0.find(271).unwrap()).unwrap(), "StudioMake");
    assert_eq!(c.ascii(ifd0.find(305).unwrap()).unwrap(), "Handshake Studio develop");
    assert_eq!((u(ifd0, 274), u(ifd0, 50970), u(ifd0, 50778), u(ifd0, 50779)), (vec![1], vec![1], vec![17], vec![21]));
    assert_eq!(c.f64s(ifd0.find(50721).unwrap()).unwrap().len(), 9);
    assert_eq!(c.f64s(ifd0.find(50722).unwrap()).unwrap().len(), 9);
    assert_eq!(c.f64s(ifd0.find(50964).unwrap()).unwrap().len(), 9);
    assert_eq!(c.f64s(ifd0.find(50728).unwrap()).unwrap(), vec![1.0, 1.0, 1.0]);
    assert_eq!(c.f64s(ifd0.find(50730).unwrap()).unwrap(), vec![0.25]);
    assert_eq!(c.bytes(ifd0.find(50781).unwrap()).unwrap(), &out.samples_sha256[..16]);
    assert_eq!(c.bytes(ifd0.find(700).unwrap()).unwrap(), xmp.as_slice());
    assert!(ifd0.find(33422).is_none() && ifd0.find(50717).is_none(), "raw-scoped tags stay out of IFD0");
    // Preview pixels are the strips of IFD0.
    let page = tiff::decode(&out.bytes, &tiff::Limits::default(), &CancellationToken::default()).unwrap();
    assert_eq!(page.pages[0].pixels(), &tiff::Samples::U8(preview.rgb.clone()));
    // The raw SubIFD: CFA photometric with its levels and crop.
    assert_eq!((u(raw_ifd, 254), u(raw_ifd, 262), u(raw_ifd, 259), u(raw_ifd, 258)), (vec![0], vec![32803], vec![1], vec![16]));
    assert_eq!((u(raw_ifd, 33421), c.bytes(raw_ifd.find(33422).unwrap()).unwrap().to_vec()), (vec![2, 2], vec![0, 1, 1, 2]));
    assert_eq!((u(raw_ifd, 50717), u(raw_ifd, 50829), u(raw_ifd, 50719), u(raw_ifd, 50720)), (vec![4095], vec![0, 0, 48, 64], vec![0, 0], vec![64, 48]));
    assert_eq!(c.bytes(raw_ifd.find(50710).unwrap()).unwrap(), [0, 1, 2]);
    // Exif IFD subset round trips through the file.
    let again = exif_subset(&out.bytes);
    assert_eq!(again, exif);
    assert_eq!(again.iso, Some(800));
    assert_eq!(again.exposure_time, Some((1, 125)));
    assert_eq!(again.lens_model.as_deref(), Some("Studio Lens 24-70"));
    // And develop's own reader still sees the identical document.
    assert_same_document(&raw, &decode_container(&out.bytes).unwrap());
}

fn recipe_of(raw: &DngRaw) -> DevelopRecipe {
    let binding = ProfileBinding {
        profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(),
        sha256: Sha256::digest(SRGB_V4).into(),
    };
    raw.recipe(&color::xyz_d50_to_linear_srgb().unwrap(), binding, BaselineExposure::Ignore).unwrap().0
}

#[test]
fn develop_to_dng_renders_a_reduced_preview_and_is_thread_invariant() {
    let mut s = base();
    s.w = 1100;
    s.h = 700;
    s.exif = true;
    let (raw, _) = raw_of(&s);
    let mut recipe = recipe_of(&raw);
    recipe.exposure_stops = 0.5;
    let cancel = CancellationToken::default();
    let settings = DngExportSettings { compression: DngCompression::LosslessJpeg, ..DngExportSettings::default() };
    let xmp = b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>";
    let (out, receipt) = develop_to_dng(&raw, &recipe, 7, &settings, Some(xmp), None, SRGB_V4, Threads::auto(), &cancel).unwrap();
    assert_eq!(receipt.unwrap().recipe_revision, 7);
    assert_eq!(out.preview, Some((1024, 651)), "long edge 1024, aspect kept");
    let (single, _) = develop_to_dng(&raw, &recipe, 7, &settings, Some(xmp), None, SRGB_V4, Threads::single(), &cancel).unwrap();
    assert_eq!(single.bytes, out.bytes, "thread count does not change the file");
    // The preview is the area average of develop_to_output at 8 bits (independent render + reduce).
    let (plane, _) = raw.cfa_plane(raw.default_window(), RangePolicy::ClampReported, 1, Limits::LARGE).unwrap();
    let (rendered, _) = develop_to_output(&plane, &recipe, 7, OutputDepth::Eight, SRGB_V4, Threads::single(), &cancel).unwrap();
    let expected = reduce_preview(&rendered, Some(1024)).unwrap();
    let page = tiff::decode(&out.bytes, &tiff::Limits::default(), &cancel).unwrap();
    assert_eq!(page.pages[0].pixels(), &tiff::Samples::U8(expected.rgb), "embedded preview equals the reduced develop output");
    // A different recipe changes the preview but not the raw frame.
    let mut brighter = recipe.clone();
    brighter.exposure_stops = 2.0;
    let (other, _) = develop_to_dng(&raw, &brighter, 8, &settings, Some(xmp), None, SRGB_V4, Threads::auto(), &cancel).unwrap();
    assert_ne!(other.dng_sha256, out.dng_sha256);
    assert_eq!(other.samples_sha256, out.samples_sha256);
    assert_same_document(&raw, &decode_container(&other.bytes).unwrap());
    // Refusals come before the render: a recipe/profile mismatch is develop's, an unsupported profile too.
    let mut p3 = recipe.clone();
    p3.output_profile.sha256 = [9; 32];
    assert_eq!(
        develop_to_dng(&raw, &p3, 1, &settings, None, None, SRGB_V4, Threads::single(), &cancel).unwrap_err().code(),
        DevelopError::InvalidProfile.code()
    );
    // Cancellation returns canceled and no file.
    let stop = CancellationToken::default();
    stop.cancel();
    assert_eq!(
        develop_to_dng(&raw, &recipe, 1, &settings, None, None, SRGB_V4, Threads::single(), &stop).unwrap_err().code(),
        DevelopError::Canceled.code()
    );
}

#[test]
fn full_size_preview_is_the_default_window() {
    let mut s = base();
    s.crop = Some(([4, 4], [40, 30]));
    let (raw, _) = raw_of(&s);
    let recipe = recipe_of(&raw);
    let settings = DngExportSettings { preview: DngPreview::FullSize, ..DngExportSettings::default() };
    let (out, _) = develop_to_dng(&raw, &recipe, 1, &settings, None, None, SRGB_V4, Threads::single(), &CancellationToken::default()).unwrap();
    let (w, h) = out.preview.unwrap();
    assert!(w >= 1 && h >= 1 && w <= 64 && h <= 48, "{w}x{h}");
    assert_same_document(&raw, &decode_container(&out.bytes).unwrap());
}

#[test]
fn refusals_are_typed() {
    let (raw, _) = raw_of(&base());
    let none = no_preview();
    // Preview requested without pixels.
    assert_eq!(code(dng_from_raw(&raw, &DngExportSettings::default(), &DngParts::default())), "preview_missing");
    // Preview shape lies.
    let lie = PreviewImage { width: 4, height: 4, rgb: vec![0; 10] };
    assert_eq!(code(dng_from_raw(&raw, &DngExportSettings::default(), &DngParts { preview: Some(&lie), ..DngParts::default() })), "preview_length_mismatch");
    let huge = PreviewImage { width: 65_535, height: 65_535, rgb: Vec::new() };
    assert_eq!(code(dng_from_raw(&raw, &DngExportSettings::default(), &DngParts { preview: Some(&huge), ..DngParts::default() })), "limit_preview_bytes");
    let zero = PreviewImage { width: 0, height: 4, rgb: Vec::new() };
    assert_eq!(code(dng_from_raw(&raw, &DngExportSettings::default(), &DngParts { preview: Some(&zero), ..DngParts::default() })), "limit_preview_bytes");
    // XMP.
    let parts = |x: &'static [u8]| DngParts { xmp: Some(x), ..DngParts::default() };
    assert_eq!(code(dng_from_raw(&raw, &none, &parts(b""))), "limit_xmp_bytes");
    assert_eq!(code(dng_from_raw(&raw, &none, &parts(b"not xmp"))), "xmp_invalid");
    assert_eq!(code(dng_from_raw(&raw, &none, &parts(b"<x:xmpmeta/>\xFF\xFE"))), "xmp_invalid");
    assert_eq!(code(dng_from_raw(&raw, &none, &parts(b"<x:xmpmeta/>\0"))), "xmp_invalid");
    let big = vec![b'a'; MAX_XMP_BYTES + 1];
    assert_eq!(code(dng_from_raw(&raw, &none, &DngParts { xmp: Some(&big), ..DngParts::default() })), "limit_xmp_bytes");
    // Text fields.
    let sw = |s: &str| DngExportSettings { software: Some(s.into()), ..none.clone() };
    assert_eq!(code(dng_from_raw(&raw, &sw("caf\u{e9}"), &DngParts::default())), "software_invalid");
    assert_eq!(code(dng_from_raw(&raw, &sw(""), &DngParts::default())), "software_invalid");
    assert_eq!(code(dng_from_raw(&raw, &sw(&"x".repeat(MAX_TEXT_BYTES + 1)), &DngParts::default())), "software_invalid");
    let bad_exif = ExifSubset { make: Some("a\0b".into()), ..ExifSubset::default() };
    assert_eq!(code(dng_from_raw(&raw, &none, &DngParts { exif: Some(&bad_exif), ..DngParts::default() })), "exif_text_invalid");
    let zero_den = ExifSubset { exposure_time: Some((1, 0)), ..ExifSubset::default() };
    assert_eq!(code(dng_from_raw(&raw, &none, &DngParts { exif: Some(&zero_den), ..DngParts::default() })), "exif_zero_denominator");
    // Source documents.
    let (no_model, _) = raw_of(&Src { model: None, ..base() });
    assert_eq!(code(dng_from_raw(&no_model, &none, &DngParts::default())), "unique_camera_model_missing");
    let named = DngExportSettings { unique_camera_model: Some("Override Model".into()), ..none.clone() };
    let o = dng_from_raw(&no_model, &named, &DngParts::default()).unwrap();
    assert_eq!(decode_container(&o.bytes).unwrap().unique_camera_model.as_deref(), Some("Override Model"));
    let (no_colour, _) = raw_of(&Src { colour: false, ..base() });
    assert_eq!(code(dng_from_raw(&no_colour, &none, &DngParts::default())), DevelopError::MissingColourData.code());
    let (no_neutral, _) = raw_of(&Src { neutral: false, ..base() });
    assert_eq!(code(dng_from_raw(&no_neutral, &none, &DngParts::default())), "as_shot_white_missing");
}

#[test]
fn unapplied_source_features_are_refused_or_recorded() {
    let (raw, _) = raw_of(&Src { opcode_list: true, orientation: 8, ..base() });
    assert_eq!(raw.unapplied, vec!["opcode_list_2", "orientation"]);
    let refuse = code(dng_from_raw(&raw, &no_preview(), &DngParts::default()));
    assert_eq!(refuse, DevelopError::UnsupportedContribution("opcode_list_2").code());
    let record = DngExportSettings { unapplied: UnappliedPolicy::Record, ..no_preview() };
    let out = dng_from_raw(&raw, &record, &DngParts::default()).unwrap();
    assert_eq!(out.dropped, vec!["opcode_list_2"], "orientation is carried, not dropped");
    let back = decode_container(&out.bytes).unwrap();
    assert_eq!(back.orientation, 8);
    assert!(!back.unapplied.contains(&"opcode_list_2"));
    assert_eq!(back.samples(), raw.samples());
}

#[test]
fn exif_extraction_never_fails_and_never_panics_on_hostile_bytes() {
    let mut s = base();
    s.exif = true;
    let (bytes, _) = build(&s);
    let full = exif_subset(&bytes);
    assert_eq!(full.make.as_deref(), Some("StudioMake"));
    assert_eq!(full.f_number, Some((28, 10)));
    assert_eq!(full.focal_length, Some((850, 10)));
    assert_eq!(full.date_time_original.as_deref(), Some("2026:10:21 09:08:07"));
    // Not TIFF at all.
    assert_eq!(exif_subset(b"\x89PNG\r\n\x1a\n").skipped, Some("not_tiff"));
    assert_eq!(exif_subset(&[]).skipped, Some("not_tiff"));
    // A source without EXIF yields an empty subset and no error.
    let (plain, _) = build(&base());
    let none = exif_subset(&plain);
    assert_eq!((none.make.clone(), none.iso, none.skipped), (None, None, None));
    // Truncations and byte flips at every offset of a small file: no panic.
    for cut in (0..bytes.len()).step_by(7) {
        let _ = exif_subset(&bytes[..cut]);
    }
    let mut seed = 99u64;
    for _ in 0..400 {
        let mut b = bytes.clone();
        let at = (xorshift(&mut seed) as usize) % 400;
        b[at] ^= (xorshift(&mut seed) & 0xFF) as u8;
        let _ = exif_subset(&b);
    }
    // An EXIF pointer into nowhere is a skip, not an error.
    let mut b = bytes.clone();
    for window in b.windows(4).position(|w| w == [0x69, 0x87, 0x04, 0x00]).into_iter() {
        // tag 34665 entry: overwrite its value with an offset beyond the file
        let at = window + 8;
        b[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    }
    let skipped = exif_subset(&b);
    assert_eq!(skipped.skipped, Some("exif_unreadable"));
    assert_eq!(skipped.make.as_deref(), Some("StudioMake"), "IFD0 fields survive an unreadable Exif IFD");
}

#[test]
fn reduce_preview_is_exact_on_integer_ratios_and_bounded() {
    let img = |w: u32, h: u32, f: &dyn Fn(u32, u32, usize) -> u8| OutputImage {
        width: w,
        height: h,
        depth_bits: 8,
        transfer: "srgb_trc",
        data: (0..h).flat_map(|y| (0..w).flat_map(move |x| (0..3).map(move |c| (x, y, c)))).map(|(x, y, c)| f(x, y, c)).collect(),
        icc: Vec::new(),
        icc_sha256: [0; 32],
        clip: ClipReport::default(),
    };
    // 4x4 -> 2x2 averages 2x2 boxes: channel value = 10 * box index + channel.
    let src = img(4, 4, &|x, y, c| (((y / 2) * 2 + x / 2) * 10) as u8 + c as u8);
    let p = reduce_preview(&src, Some(2)).unwrap();
    assert_eq!((p.width, p.height), (2, 2));
    assert_eq!(p.rgb, vec![0, 1, 2, 10, 11, 12, 20, 21, 22, 30, 31, 32]);
    // No upscale, no change when already small.
    let same = reduce_preview(&src, Some(100)).unwrap();
    assert_eq!((same.width, same.height, same.rgb.len()), (4, 4, 48));
    // Odd ratios keep every output pixel in range and the aspect.
    let odd = reduce_preview(&img(37, 11, &|x, y, c| (x * 5 + y + c as u32) as u8), Some(10)).unwrap();
    assert_eq!((odd.width, odd.height), (10, 2));
    // Hostile: 16-bit data, wrong length.
    let mut sixteen = src.clone();
    sixteen.depth_bits = 16;
    assert_eq!(reduce_preview(&sixteen, Some(2)).unwrap_err().code(), "depth_mismatch");
    let mut short_data = src.clone();
    short_data.data.pop();
    assert_eq!(reduce_preview(&short_data, Some(2)).unwrap_err().code(), DevelopError::InvalidDimensions.code());
    let mut lie = src;
    lie.width = u32::MAX;
    assert_eq!(reduce_preview(&lie, None).unwrap_err().code(), DevelopError::InvalidDimensions.code());
}

// ---- corpus (NOT_PROVEN when absent) -----------------------------------------------------------

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

/// Write `source` as DNG both ways and prove the raw data and tags of every written file against the source
/// document read by develop's own reader.
fn corpus_round_trip(test: &str, file: &str, sha: &str, only_none: bool) {
    let Some(bytes) = corpus_file(test, file, sha) else {
        return;
    };
    let raw = decode_container(&bytes).unwrap();
    let mut variants = vec![DngCompression::None];
    if !only_none {
        variants.push(DngCompression::LosslessJpeg);
    }
    for compression in variants {
        let settings = DngExportSettings { compression, preview: DngPreview::None, unapplied: UnappliedPolicy::Record, ..DngExportSettings::default() };
        let out = dng_from_raw(&raw, &settings, &DngParts { exif: Some(&exif_subset(&bytes)), ..DngParts::default() }).unwrap();
        let back = decode_container(&out.bytes).unwrap();
        assert_same_document(&raw, &back);
        println!(
            "corpus {file} {compression:?} {}x{} source_bytes={} dng_bytes={} bits={} dropped={:?} samples_sha256={}",
            raw.width, raw.height, bytes.len(), out.bytes.len(), out.bits_per_sample, out.dropped, hex(&out.samples_sha256)
        );
    }
}

#[test]
fn corpus_canon_5d3_mlv_14bit_lossless_round_trips() {
    corpus_round_trip(
        "corpus_canon_5d3_mlv_14bit_lossless_round_trips",
        "canon-eos-5d3-mlv-14bit-lossless.dng",
        "1d77dcc6cdb839b10c1948f0b4471984ab48cf15e973fb77520584cee72b603e",
        false,
    );
}

#[test]
fn corpus_nikon_d80_tiled_linearized_round_trips() {
    corpus_round_trip(
        "corpus_nikon_d80_tiled_linearized_round_trips",
        "nikon-d80-16bit-lossless-tiled-linearized.dng",
        "5095ca264e763624cd3aa62940286dd1bcff4504eefa1ce61cbb72f50f8426bb",
        false,
    );
}

#[test]
fn corpus_samsung_g950u_uncompressed_round_trips() {
    corpus_round_trip(
        "corpus_samsung_g950u_uncompressed_round_trips",
        "samsung-sm-g950u-16bit-uncompressed.dng",
        "7b0327cf015270ec98627ed1dee02ff39a47d1a6d6ff1ecfe164586e7dc7a749",
        false,
    );
}

/// Informational comparison with the Adobe DNG Converter file in the corpus (its input CR2 is not in the corpus,
/// so only the tag profile can be compared, not the samples): prints which tags the converter writes that this
/// writer does not, and the reverse. Parity with the converter is UNVERIFIED.
#[test]
fn corpus_adobe_dng_converter_tag_profile_is_compared() {
    const TEST: &str = "corpus_adobe_dng_converter_tag_profile_is_compared";
    let Some(adobe) = corpus_file(TEST, "adobe-dng-converter-canon-eos-5d3-16bit-uncompressed.dng", "95dc76a2f255e623acaa5149b320389f7da7c995da890c3e0d511c0ff681b2e1") else {
        return;
    };
    let raw = decode_container(&adobe).unwrap();
    let settings = DngExportSettings { preview: DngPreview::None, unapplied: UnappliedPolicy::Record, ..DngExportSettings::default() };
    let ours = dng_from_raw(&raw, &settings, &DngParts::default()).unwrap();
    assert_same_document(&raw, &decode_container(&ours.bytes).unwrap());
    let tags = |bytes: &[u8]| -> std::collections::BTreeSet<u16> {
        let c = Container::parse(bytes, &tiff::Limits::default()).unwrap();
        c.walk().unwrap().iter().flat_map(|i| i.entries.iter().map(|e| e.tag)).collect()
    };
    let (a, o) = (tags(&adobe), tags(&ours.bytes));
    println!("adobe_only_tags={:?}", a.difference(&o).collect::<Vec<_>>());
    println!("ours_only_tags={:?}", o.difference(&a).collect::<Vec<_>>());
    println!("NOT_PROVEN: {TEST} reason=parity_with_adobe_dng_converter_output_unverified (input CR2 not in the corpus; tag profile compared only)");
}
