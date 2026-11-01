//! Lane SMB-TIFF-CHANNELS-1 (R52 F-R52-2, TIFF part; STU-RAS-022, STU-RAS-043, STU-IO-006 row 3): any
//! number of extra samples decode and encode as typed planes carrying their ExtraSamples (tag 338)
//! values, Folio-free. Hand-assembled files (tests/common, no encoder) check the decoder on its own;
//! the encoder is checked by decoding what it wrote. TIFF 6.0 section 18 (ExtraSamples) is the layout
//! reference; how Photoshop marks its alpha channels in a TIFF is UNVERIFIED (D5).
mod common;
use common::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_tiff::*;

fn live() -> CancellationToken {
    CancellationToken::default()
}

fn dec(bytes: &[u8]) -> Result<TiffFile> {
    decode(bytes, &Limits::default(), &live())
}

/// Sample `at` of every pixel of an interleaved buffer with `spp` samples per pixel (this test's own
/// de-interleave, independent of `TiffPage::extra_planes`).
fn plane(s: &Samples, spp: usize, at: usize) -> Samples {
    fn pick<T: Copy>(v: &[T], spp: usize, at: usize) -> Vec<T> {
        v.iter().skip(at).step_by(spp).copied().collect()
    }
    match s {
        Samples::U8(v) => Samples::U8(pick(v, spp, at)),
        Samples::U16(v) => Samples::U16(pick(v, spp, at)),
        Samples::U32(v) => Samples::U32(pick(v, spp, at)),
        Samples::F32(v) => Samples::F32(pick(v, spp, at)),
        Samples::F64(v) => Samples::F64(pick(v, spp, at)),
    }
}

/// The first `colour` samples of every pixel, interleaved.
fn colour_part(s: &Samples, spp: usize, colour: usize) -> Samples {
    fn take<T: Copy>(v: &[T], spp: usize, colour: usize) -> Vec<T> {
        v.chunks(spp).flat_map(|px| px[..colour].iter().copied()).collect()
    }
    match s {
        Samples::U8(v) => Samples::U8(take(v, spp, colour)),
        Samples::U16(v) => Samples::U16(take(v, spp, colour)),
        Samples::U32(v) => Samples::U32(take(v, spp, colour)),
        Samples::F32(v) => Samples::F32(take(v, spp, colour)),
        Samples::F64(v) => Samples::F64(take(v, spp, colour)),
    }
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    U8,
    U16,
    U32,
    F32,
}

fn samples(kind: Kind, n: usize, seed: u32) -> Samples {
    let p = pattern(n, seed);
    match kind {
        Kind::U8 => Samples::U8(p.iter().map(|v| *v as u8).collect()),
        Kind::U16 => Samples::U16(p.iter().map(|v| *v as u16).collect()),
        Kind::U32 => Samples::U32(p.iter().map(|v| v.wrapping_mul(2_654_435_761)).collect()),
        Kind::F32 => Samples::F32(p.iter().map(|v| (*v % 1000) as f32 / 999.0 - 0.25).collect()),
    }
}

fn depth(kind: Kind) -> u8 {
    match kind {
        Kind::U8 => 8,
        Kind::U16 => 16,
        Kind::U32 | Kind::F32 => 32,
    }
}

fn request<'a>(w: u32, h: u32, color: ColorModel, alpha: Alpha, kind: Kind, s: &'a Samples) -> EncodeRequest<'a> {
    EncodeRequest {
        width: w,
        height: h,
        color,
        alpha,
        bit_depth: depth(kind),
        samples: s,
        icc: None,
        compression: Compression::None,
        predictor: Predictor::None,
        big_tiff: false,
        resolution: None,
        orientation: None,
    }
}

/// Hand-built pages with 0, 1 and several extra samples decode to the stored samples, with every extra
/// sample handed out as its own plane and its ExtraSamples value, in both byte orders, for RGB, gray,
/// CMYK, 16-bit and planar (PlanarConfiguration 2) layouts.
#[test]
fn hand_built_pages_with_zero_one_and_several_extra_samples_decode_as_planes() {
    for le in [true, false] {
        // RGB 2x2 8-bit with no, one unspecified, and three extra samples (unspecified, unassociated
        // alpha, unspecified: an alpha that is not first is a plain extra plane).
        for (pi, colour, extras) in [
            (2u16, 3usize, vec![]),
            (2, 3, vec![0u16]),
            (2, 3, vec![0, 2, 0]),
            (2, 3, vec![2, 0]),
            (1, 1, vec![0, 0]),
            (5, 4, vec![0, 0]),
        ] {
            let spp = colour + extras.len();
            let pixels: Vec<u8> = (0..4 * spp).map(|i| (i * 7 + 1) as u8).collect();
            let mut b = Builder::new(le);
            let tag = if extras.is_empty() { vec![] } else { vec![shorts_e(le, 338, &extras)] };
            b.page(2, 2, pi, &vec![8; spp], &pixels, tag);
            let f = dec(&b.buf).unwrap_or_else(|e| panic!("pi {pi} extras {extras:?} le {le}: {e:?}"));
            let p = &f.pages[0];
            let label = format!("pi {pi} extras {extras:?} le {le}");
            assert_eq!(p.pixels(), &Samples::U8(pixels.clone()), "{label}");
            assert_eq!((p.samples_per_pixel as usize, p.colour_samples_per_pixel() as usize), (spp, colour), "{label}");
            assert_eq!(p.extra_samples, extras, "{label}");
            let planes = p.extra_planes();
            assert_eq!(planes.len(), extras.len(), "{label}");
            for (k, got) in planes.iter().enumerate() {
                assert_eq!(got.extra_sample, extras[k], "{label} plane {k}");
                assert_eq!(got.samples, plane(p.pixels(), spp, colour + k), "{label} plane {k}");
            }
        }
        // 16-bit RGB with two extra samples: samples in the file's byte order.
        let values: Vec<u16> = (0..4 * 5).map(|i| (i as u16).wrapping_mul(3001)).collect();
        let mut b = Builder::new(le);
        b.page(2, 2, 2, &[16; 5], &shorts(le, &values), vec![shorts_e(le, 338, &[0, 0])]);
        let f = dec(&b.buf).unwrap();
        let planes = f.pages[0].extra_planes();
        assert_eq!(planes[1].samples, Samples::U16(values.iter().skip(4).step_by(5).copied().collect()), "le {le}");
        // Planar configuration 2: one strip per sample plane, interleaved on decode.
        let planes_in: Vec<Vec<u8>> = (0..5u8).map(|k| (0..4u8).map(|i| k * 40 + i).collect()).collect();
        let mut b = Builder::new(le);
        let offs: Vec<u32> = planes_in.iter().map(|p| b.data(p)).collect();
        b.ifd(vec![
            long(le, 256, 2),
            long(le, 257, 2),
            shorts_e(le, 258, &[8; 5]),
            short(le, 259, 1),
            short(le, 262, 2),
            E { tag: 273, ty: 4, count: 5, data: longs(le, &offs) },
            short(le, 277, 5),
            long(le, 278, 2),
            E { tag: 279, ty: 4, count: 5, data: longs(le, &[4; 5]) },
            short(le, 284, 2),
            shorts_e(le, 338, &[1, 0]),
        ]);
        let f = dec(&b.buf).unwrap();
        let p = &f.pages[0];
        assert!(p.planar_in_file);
        assert_eq!(p.alpha, Alpha::Associated);
        let planes = p.extra_planes();
        assert_eq!(planes.iter().map(|x| x.extra_sample).collect::<Vec<_>>(), [1, 0]);
        assert_eq!(planes[0].samples, Samples::U8(planes_in[3].clone()));
        assert_eq!(planes[1].samples, Samples::U8(planes_in[4].clone()));
    }
}

/// The encoder writes 0, 1 and several extra planes after the colour model's own samples (alpha
/// included) at every depth it supports, with every compression, the horizontal predictor on integer
/// samples, both byte orders and BigTIFF; decoding gives back the colour samples, the ExtraSamples
/// values (alpha kind first) and every plane exactly.
#[test]
fn extra_planes_encode_and_decode_exactly_at_every_depth_compression_and_byte_order() {
    let (w, h) = (5u32, 3u32);
    let n = (w * h) as usize;
    let mut cases = 0;
    for (color, alpha) in [
        (ColorModel::Gray, Alpha::None),
        (ColorModel::GrayA, Alpha::Unassociated),
        (ColorModel::Rgb, Alpha::None),
        (ColorModel::Rgba, Alpha::Associated),
        (ColorModel::Cmyk, Alpha::None),
    ] {
        let base = usize::from(color.samples_per_pixel());
        for kind in [Kind::U8, Kind::U16, Kind::U32, Kind::F32] {
            let base_samples = samples(kind, n * base, 3);
            for values in [&[][..], &[0u16][..], &[0, 2, 0][..]] {
                let extra: Vec<ExtraPlane> = values
                    .iter()
                    .enumerate()
                    .map(|(k, v)| ExtraPlane { extra_sample: *v, samples: samples(kind, n, 11 + k as u32) })
                    .collect();
                for compression in [Compression::None, Compression::Lzw, Compression::Deflate, Compression::PackBits] {
                    let predictors: &[Predictor] =
                        if matches!(kind, Kind::F32) { &[Predictor::None] } else { &[Predictor::None, Predictor::Horizontal] };
                    for &predictor in predictors {
                        for (byte_order, big_tiff) in [(ByteOrder::Little, false), (ByteOrder::Big, false), (ByteOrder::Little, true)] {
                            let mut req = request(w, h, color, alpha, kind, &base_samples);
                            req.compression = compression;
                            req.predictor = predictor;
                            req.big_tiff = big_tiff;
                            let opts = EncodeOptions { byte_order, metadata: Metadata::default() };
                            let label = format!("{color:?} {kind:?} {values:?} {compression:?} {predictor:?} {byte_order:?} big={big_tiff}");
                            let bytes = encode_with_planes(&req, &extra, &opts).unwrap_or_else(|e| panic!("{label}: {e:?}"));
                            let f = dec(&bytes).unwrap_or_else(|e| panic!("{label}: decode {e:?}"));
                            let p = &f.pages[0];
                            let spp = base + values.len();
                            assert_eq!((f.byte_order, f.big_tiff), (byte_order, big_tiff), "{label}");
                            assert_eq!(p.samples_per_pixel as usize, spp, "{label}");
                            let mut want_values: Vec<u16> = match alpha {
                                Alpha::None => vec![],
                                Alpha::Associated => vec![1],
                                Alpha::Unassociated => vec![2],
                            };
                            want_values.extend_from_slice(values);
                            assert_eq!(p.extra_samples, want_values, "{label}");
                            assert_eq!(colour_part(p.pixels(), spp, base), base_samples, "{label}: colour and alpha samples");
                            let planes = p.extra_planes();
                            let skip = usize::from(alpha != Alpha::None);
                            assert_eq!(planes.len(), skip + values.len(), "{label}");
                            if skip == 1 {
                                assert_eq!(planes[0].samples, plane(&base_samples, base, base - 1), "{label}: alpha plane");
                            }
                            assert_eq!(&planes[skip..], &extra[..], "{label}: extra planes");
                            cases += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(cases, 5 * (3 * 3 * 4 * 2 + 3 * 4 * 1) * 3);
}

/// Hostile and out-of-envelope requests are typed refusals before any allocation (V4-01/02): extra
/// sample counts above the samples-per-pixel budget (dims_u32_max-style), the page byte budget and the
/// running aggregate with many extra samples (aggregate_over_budget-style), ExtraSamples values outside
/// TIFF 6.0, mismatched planes, a tag whose count disagrees with the extra samples, and several extra
/// samples on a JPEG page (the codec cannot hand them out).
#[test]
fn extra_sample_counts_values_and_budgets_are_bounded() {
    let (w, h) = (2u32, 2u32);
    let base = Samples::U8(vec![9; 12]);
    let req = request(w, h, ColorModel::Rgb, Alpha::None, Kind::U8, &base);
    let opts = EncodeOptions::default();
    let planes = |k: usize, v: u16| -> Vec<ExtraPlane> { (0..k).map(|i| ExtraPlane { extra_sample: v, samples: Samples::U8(vec![i as u8; 4]) }).collect() };
    // 3 colour samples + 61 planes is the 64-sample ceiling; one more is refused.
    let at_cap = encode_with_planes(&req, &planes(61, 0), &opts).expect("64 samples per pixel");
    assert_eq!(dec(&at_cap).unwrap().pages[0].extra_planes().len(), 61);
    assert_eq!(MAX_ENCODE_SAMPLES_PER_PIXEL, 64);
    assert_eq!(encode_with_planes(&req, &planes(62, 0), &opts).unwrap_err(), TiffError::Limit("limit_samples_per_pixel"));
    assert_eq!(
        encode_with_planes(&req, &planes(usize::from(u16::MAX) + 1, 0), &opts).unwrap_err(),
        TiffError::Limit("limit_samples_per_pixel")
    );
    // ExtraSamples values TIFF 6.0 does not define.
    assert_eq!(encode_with_planes(&req, &planes(2, 3), &opts).unwrap_err(), TiffError::unsupported_value("extra_sample_unsupported", 3));
    // A plane of the wrong length or sample type.
    let short_plane = [ExtraPlane { extra_sample: 0, samples: Samples::U8(vec![0; 3]) }];
    assert_eq!(encode_with_planes(&req, &short_plane, &opts).unwrap_err(), TiffError::Usage("sample_count_mismatch"));
    let wrong_type = [ExtraPlane { extra_sample: 0, samples: Samples::U16(vec![0; 4]) }];
    assert_eq!(encode_with_planes(&req, &wrong_type, &opts).unwrap_err(), TiffError::Usage("extra_plane_sample_type"));
    // The encode byte budget counts the extra planes, checked before any plane is read or interleaved:
    // a 2048 x 2049 gray u32 page (16 MiB) with 63 planes is over the 1 GiB encode budget.
    let gray = Samples::U32(vec![0; 2048 * 2049]);
    let big = request(2048, 2049, ColorModel::Gray, Alpha::None, Kind::U32, &gray);
    let heavy: Vec<ExtraPlane> = (0..63).map(|_| ExtraPlane { extra_sample: 0, samples: Samples::U32(Vec::new()) }).collect();
    assert_eq!(encode_with_planes(&big, &heavy, &opts).unwrap_err(), TiffError::Limit("limit_encode_bytes"));

    for le in [true, false] {
        // 3 colour + 62 extra samples: above the decoder's samples-per-pixel limit, refused at scan.
        let mut b = Builder::new(le);
        b.page(1, 1, 2, &[8; 65], &[0; 65], vec![shorts_e(le, 338, &[0; 62])]);
        assert_eq!(dec(&b.buf).unwrap_err(), TiffError::Limit("limit_samples_per_pixel"));
        // Five extra samples on a 64x64 page: 32768 decoded bytes, over a 30000-byte page budget, and
        // two such pages over a 40000-byte running aggregate.
        let mut b = Builder::new(le);
        b.page(64, 64, 2, &[8; 8], &vec![1; 64 * 64 * 8], vec![shorts_e(le, 338, &[0; 5])]);
        let page_cap = Limits { max_page_decoded_bytes: 30_000, ..Limits::default() };
        assert_eq!(decode(&b.buf, &page_cap, &live()).unwrap_err(), TiffError::Limit("limit_page_bytes"));
        b.page(64, 64, 2, &[8; 8], &vec![1; 64 * 64 * 8], vec![shorts_e(le, 338, &[0; 5])]);
        let total_cap = Limits { max_total_decoded_bytes: 40_000, ..Limits::default() };
        assert_eq!(decode(&b.buf, &total_cap, &live()).unwrap_err(), TiffError::Limit("aggregate_over_budget"));
        assert_eq!(dec(&b.buf).unwrap().pages.len(), 2);
        // ExtraSamples lists one value for two extra samples.
        let mut b = Builder::new(le);
        b.page(2, 2, 2, &[8; 5], &[3; 20], vec![shorts_e(le, 338, &[0])]);
        assert_eq!(dec(&b.buf).unwrap_err(), TiffError::Corrupt("extra_samples_count_mismatch"));
        // No ExtraSamples tag at all: the extra samples read as unspecified.
        let mut b = Builder::new(le);
        b.page(2, 2, 2, &[8; 5], &[3; 20], vec![]);
        let f = dec(&b.buf).unwrap();
        assert_eq!(f.pages[0].extra_planes().iter().map(|p| p.extra_sample).collect::<Vec<_>>(), [0, 0]);
        // Two extra samples on a JPEG-compressed page.
        let mut b = Builder::new(le);
        let off = b.data(&[0; 20]);
        b.ifd(vec![
            long(le, 256, 2),
            long(le, 257, 2),
            shorts_e(le, 258, &[8; 5]),
            short(le, 259, 7),
            short(le, 262, 2),
            long(le, 273, off),
            short(le, 277, 5),
            long(le, 278, 2),
            long(le, 279, 20),
            shorts_e(le, 338, &[0, 0]),
        ]);
        assert_eq!(dec(&b.buf).unwrap_err(), TiffError::unsupported_value("extra_samples_unsupported", 2));
    }
}
