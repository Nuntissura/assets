//! Lane SMB-DEVELOP-EXPORT-TIFF: byte-order option and ASCII metadata of `encode_with`.
//! Spec basis: STU-IO-136 (TIFF compression domain None/LZW/ZIP), STU-COL-* (embedded ICC, tag 34675).
//! All bytes are built in the tests (V4-03).
mod common;
use common::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_tiff::*;

fn dec(bytes: &[u8]) -> Result<TiffFile> {
    decode(bytes, &Limits::default(), &CancellationToken::default())
}

fn req<'a>(w: u32, h: u32, color: ColorModel, depth: u8, samples: &'a Samples) -> EncodeRequest<'a> {
    EncodeRequest {
        width: w,
        height: h,
        color,
        alpha: Alpha::None,
        bit_depth: depth,
        samples,
        icc: None,
        compression: Compression::None,
        predictor: Predictor::None,
        big_tiff: false,
        resolution: None,
        orientation: None,
    }
}

fn opts(order: ByteOrder) -> EncodeOptions {
    EncodeOptions { byte_order: order, metadata: Metadata::default() }
}

/// Big-endian output for every compression x predictor x depth x container decodes to the same samples as the
/// little-endian output, carries the `MM` mark, and is byte-identical on a second run.
#[test]
fn big_endian_round_trips_for_every_compression_predictor_depth_and_container() {
    let (w, h) = (37_u32, 29_u32);
    let icc: Vec<u8> = (0..301_u32).map(|i| (i * 7 % 251) as u8).collect();
    for big_tiff in [false, true] {
        for compression in [Compression::None, Compression::Lzw, Compression::Deflate, Compression::PackBits] {
            for predictor in [Predictor::None, Predictor::Horizontal] {
                for color in [ColorModel::Gray, ColorModel::Rgb, ColorModel::Rgba] {
                    let n = (w * h * u32::from(color.samples_per_pixel())) as usize;
                    let raw = pattern(n, 5);
                    let s8 = Samples::U8(raw.iter().map(|v| *v as u8).collect());
                    let s16 = Samples::U16(raw.iter().map(|v| (*v as u16).wrapping_mul(257)).collect());
                    let s32 = Samples::U32(raw.iter().map(|v| v.wrapping_mul(2_654_435_761)).collect());
                    for (samples, depth) in [(s8, 8_u8), (s16, 16), (s32, 32)] {
                        let mut r = req(w, h, color, depth, &samples);
                        r.compression = compression;
                        r.predictor = predictor;
                        r.big_tiff = big_tiff;
                        r.icc = Some(&icc);
                        r.resolution = Some(Resolution { x: (300, 1), y: (300, 1), unit: 2 });
                        r.orientation = Some(1);
                        if color == ColorModel::Rgba {
                            r.alpha = Alpha::Unassociated;
                        }
                        let tag = format!("{big_tiff} {compression:?} {predictor:?} {color:?} {depth}");
                        let be = encode_with(&r, &opts(ByteOrder::Big)).expect("encode be");
                        assert_eq!(&be[..2], b"MM", "{tag}");
                        assert_eq!(be, encode_with(&r, &opts(ByteOrder::Big)).unwrap(), "deterministic {tag}");
                        let le = encode_with(&r, &opts(ByteOrder::Little)).expect("encode le");
                        assert_eq!(le, encode(&r).unwrap(), "default options equal encode() {tag}");
                        assert_eq!(&le[..2], b"II", "{tag}");
                        let (fb, fl) = (dec(&be).expect("decode be"), dec(&le).expect("decode le"));
                        assert_eq!(fb.byte_order, ByteOrder::Big, "{tag}");
                        assert_eq!(fb.big_tiff, big_tiff, "{tag}");
                        let (pb, pl) = (&fb.pages[0], &fl.pages[0]);
                        assert_eq!(pb.pixels(), &samples, "samples {tag}");
                        assert_eq!(pb.pixels(), pl.pixels(), "be equals le {tag}");
                        assert_eq!(pb.icc.as_deref(), Some(icc.as_slice()), "icc {tag}");
                        assert_eq!(pb.alpha, pl.alpha, "{tag}");
                        assert_eq!(pb.resolution, pl.resolution, "{tag}");
                        assert_eq!(pb.orientation, Some(1), "{tag}");
                        assert_eq!(pb.predictor, if predictor == Predictor::None { 1 } else { 2 }, "{tag}");
                        assert_eq!(pb.compression, pl.compression, "{tag}");
                    }
                }
            }
        }
    }
    // Float, big-endian, with a non-trivial bit pattern (negative, subnormal, infinity).
    let fl = Samples::F32(vec![-3.5, 0.25, f32::MIN_POSITIVE / 4.0, f32::INFINITY, 1.0e-30, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0]);
    let mut r = req(2, 2, ColorModel::Rgb, 32, &fl);
    r.compression = Compression::Deflate;
    let f = dec(&encode_with(&r, &opts(ByteOrder::Big)).unwrap()).unwrap();
    assert_eq!(f.pages[0].pixels(), &fl);
    r.predictor = Predictor::Horizontal;
    assert_eq!(encode_with(&r, &opts(ByteOrder::Big)).unwrap_err().code(), "predictor_unsupported");
}

/// The first bytes of a 16-bit uncompressed big-endian file hold the sample high byte first (the strip is not
/// merely relabelled), and the header fields are big-endian.
#[test]
fn big_endian_header_and_sample_bytes_are_big_endian() {
    let s = Samples::U16(vec![0x0102, 0x0304, 0x0506]);
    let r = req(1, 1, ColorModel::Rgb, 16, &s);
    let be = encode_with(&r, &opts(ByteOrder::Big)).unwrap();
    assert_eq!(&be[..4], &[b'M', b'M', 0, 42]);
    let hit = be.windows(6).position(|w| w == [1, 2, 3, 4, 5, 6]);
    assert!(hit.is_some(), "big-endian sample bytes present");
    let le = encode(&r).unwrap();
    assert!(le.windows(6).any(|w| w == [2, 1, 4, 3, 6, 5]));
    // Container walk (independent of the pixel decoder): IFD entries read big-endian, BitsPerSample = 16 x 3.
    let c = Container::parse(&be, &Limits::default()).unwrap();
    let ifds = c.walk().unwrap();
    let ifd = &ifds[0];
    let bps = ifd.find(258).expect("BitsPerSample");
    assert_eq!(c.uints(bps).unwrap(), vec![16, 16, 16]);
}

/// ASCII metadata tags are written in both byte orders and read back through the container walk; hostile
/// metadata is refused before any encoding.
#[test]
fn metadata_tags_round_trip_and_hostile_metadata_is_refused() {
    let s = Samples::U8(vec![1, 2, 3, 4, 5, 6]);
    let r = req(2, 1, ColorModel::Rgb, 8, &s);
    let meta = Metadata {
        software: Some("Studio develop 1.0".into()),
        description: Some("export test".into()),
        artist: Some("A. Photographer".into()),
        copyright: Some("(c) test".into()),
    };
    for order in [ByteOrder::Little, ByteOrder::Big] {
        let bytes = encode_with(&r, &EncodeOptions { byte_order: order, metadata: meta.clone() }).unwrap();
        assert_eq!(dec(&bytes).unwrap().pages[0].pixels(), &s);
        let c = Container::parse(&bytes, &Limits::default()).unwrap();
        let ifds = c.walk().unwrap();
        let ascii = |tag: u16| {
            let e = ifds[0].find(tag).unwrap_or_else(|| panic!("tag {tag}"));
            c.ascii(e).unwrap()
        };
        assert_eq!(ascii(305), "Studio develop 1.0");
        assert_eq!(ascii(270), "export test");
        assert_eq!(ascii(315), "A. Photographer");
        assert_eq!(ascii(33432), "(c) test");
    }
    let bad = |m: Metadata| encode_with(&r, &EncodeOptions { byte_order: ByteOrder::Little, metadata: m }).unwrap_err().code();
    assert_eq!(bad(Metadata { software: Some("caf\u{e9}".into()), ..Metadata::default() }), "metadata_not_ascii");
    assert_eq!(bad(Metadata { artist: Some("a\0b".into()), ..Metadata::default() }), "metadata_not_ascii");
    assert_eq!(
        bad(Metadata { description: Some("x".repeat(MAX_METADATA_BYTES + 1)), ..Metadata::default() }),
        "limit_metadata_bytes"
    );
    assert_eq!(
        bad(Metadata { description: Some("x".repeat(MAX_METADATA_BYTES)), copyright: Some("y".into()), ..Metadata::default() }),
        "limit_metadata_bytes"
    );
}

/// Encoder admission (V4-01/02): hostile sizes are refused before any sample buffer is built, in both byte orders.
#[test]
fn hostile_sizes_are_refused_before_allocation() {
    let small = Samples::U8(vec![0; 3]);
    for order in [ByteOrder::Little, ByteOrder::Big] {
        let o = opts(order);
        assert_eq!(encode_with(&req(0, 1, ColorModel::Rgb, 8, &small), &o).unwrap_err().code(), "zero_dimension");
        assert_eq!(encode_with(&req(u32::MAX, u32::MAX, ColorModel::Rgb, 8, &small), &o).unwrap_err().code(), "dims_overflow");
        assert_eq!(
            encode_with(&req(100_000, 100_000, ColorModel::Gray, 8, &small), &o).unwrap_err().code(),
            "sample_count_mismatch"
        );
        assert_eq!(encode_with(&req(1, 1, ColorModel::Rgb, 10, &small), &o).unwrap_err().code(), "bit_depth_10_unsupported");
    }
}
