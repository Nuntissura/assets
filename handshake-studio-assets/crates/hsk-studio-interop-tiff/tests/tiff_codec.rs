//! Slice 1 focused tests. All bytes are built in the tests (V4-03: no machine-local paths).
mod common;
use common::*;
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_interop_tiff::*;
use hsk_studio_observe::{Budget, DeliveryClass, DeliveryError, Observe, Outcome, SinkPort};

fn live() -> CancellationToken {
    CancellationToken::default()
}

fn dec(bytes: &[u8]) -> Result<TiffFile> {
    decode(bytes, &Limits::default(), &live())
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

/// 1. Hand-assembled little- and big-endian 3x2 RGB 8-bit strip TIFFs decode to the exact samples.
#[test]
fn hand_built_le_and_be_rgb_decode_exactly() {
    let pixels: Vec<u8> = (1..=18).collect();
    let icc: Vec<u8> = (0..40_u8).map(|i| i.wrapping_mul(7)).collect();
    for le in [true, false] {
        let mut b = Builder::new(le);
        b.page(3, 2, 2, &[8, 8, 8], &pixels, vec![bytes_e(34675, &icc), short(le, 274, 6)]);
        let f = dec(&b.buf).expect("decode");
        assert_eq!(f.byte_order, if le { ByteOrder::Little } else { ByteOrder::Big });
        assert!(!f.big_tiff);
        let [page] = f.pages.as_slice() else { panic!("one page") };
        assert_eq!((page.width, page.height, page.samples_per_pixel, page.bits_per_sample), (3, 2, 3, 8));
        assert_eq!(page.photometric, Photometric::Rgb);
        assert_eq!(page.alpha, Alpha::None);
        assert_eq!(page.icc.as_deref(), Some(icc.as_slice()));
        assert_eq!(page.orientation, Some(6));
        assert_eq!(page.pixels(), &Samples::U8(pixels.clone()));
    }
}

/// 2. Every compression, 8/16-bit, Gray and RGB, predictor on and off, is byte-identical after a round trip;
///    a predictor on float samples is the typed refusal `predictor_unsupported`.
#[test]
fn compressions_and_predictors_round_trip() {
    let (w, h) = (37_u32, 29_u32);
    for compression in [Compression::None, Compression::Lzw, Compression::Deflate, Compression::PackBits] {
        for predictor in [Predictor::None, Predictor::Horizontal] {
            for color in [ColorModel::Gray, ColorModel::Rgb] {
                let n = (w * h * u32::from(color.samples_per_pixel())) as usize;
                let raw = pattern(n, 3);
                let s8 = Samples::U8(raw.iter().map(|v| *v as u8).collect());
                let s16 = Samples::U16(raw.iter().map(|v| *v as u16).collect());
                for (samples, depth) in [(s8, 8_u8), (s16, 16)] {
                    let mut r = req(w, h, color, depth, &samples);
                    r.compression = compression;
                    r.predictor = predictor;
                    let bytes = encode(&r).expect("encode");
                    let f = dec(&bytes).expect("decode");
                    assert_eq!(f.pages[0].pixels(), &samples, "{compression:?} {predictor:?} {color:?} {depth}");
                    assert_eq!(f.pages[0].predictor, if predictor == Predictor::None { 1 } else { 2 });
                }
            }
        }
    }
    // Multi-strip (about 6 MB of RGB16) through Deflate with a predictor.
    let (w, h) = (1000_u32, 1000_u32);
    let big = Samples::U16(pattern((w * h * 3) as usize, 9).iter().map(|v| *v as u16).collect());
    let mut r = req(w, h, ColorModel::Rgb, 16, &big);
    r.compression = Compression::Deflate;
    r.predictor = Predictor::Horizontal;
    assert_eq!(dec(&encode(&r).unwrap()).unwrap().pages[0].pixels(), &big);
    // Float: round trip without a predictor, typed refusal with one.
    let fl = Samples::F32((0..(w * 2) as usize * 3).map(|i| i as f32 * 0.25 - 3.0).collect());
    let mut r = req(w, 2, ColorModel::Rgb, 32, &fl);
    r.compression = Compression::Lzw;
    assert_eq!(dec(&encode(&r).unwrap()).unwrap().pages[0].pixels(), &fl);
    r.predictor = Predictor::Horizontal;
    assert_eq!(encode(&r).unwrap_err().code(), "predictor_unsupported");
}

/// 3. RGBA16 with unassociated alpha and a 3144-byte ICC payload round-trips; ExtraSamples reads back as 2.
///    Also GrayA associated, CMYK, F32 and BigTIFF keep samples, alpha kind and ICC.
#[test]
fn alpha_icc_bit_depth_and_bigtiff_round_trip() {
    let icc: Vec<u8> = (0..3144_u32).map(|i| (i.wrapping_mul(31) >> 2) as u8).collect();
    let (w, h) = (23_u32, 17_u32);
    let rgba = Samples::U16(pattern((w * h * 4) as usize, 5).iter().map(|v| *v as u16).collect());
    for big in [false, true] {
        for compression in [Compression::None, Compression::Lzw] {
            let mut r = req(w, h, ColorModel::Rgba, 16, &rgba);
            r.alpha = Alpha::Unassociated;
            r.icc = Some(&icc);
            r.compression = compression;
            r.predictor = Predictor::Horizontal;
            r.big_tiff = big;
            r.resolution = Some(Resolution { x: (300, 1), y: (300, 1), unit: 2 });
            let f = dec(&encode(&r).unwrap()).unwrap();
            let p = &f.pages[0];
            assert_eq!(f.big_tiff, big);
            assert_eq!(p.pixels(), &rgba);
            assert_eq!((p.alpha, p.extra_samples.as_slice(), p.samples_per_pixel), (Alpha::Unassociated, &[2_u16][..], 4));
            assert_eq!(p.icc.as_deref(), Some(icc.as_slice()));
            assert_eq!(p.resolution, Some(Resolution { x: (300, 1), y: (300, 1), unit: 2 }));
        }
    }
    let ga = Samples::U8(pattern((w * h * 2) as usize, 1).iter().map(|v| *v as u8).collect());
    let mut r = req(w, h, ColorModel::GrayA, 8, &ga);
    r.alpha = Alpha::Associated;
    r.compression = Compression::Deflate;
    r.predictor = Predictor::Horizontal;
    let p = &dec(&encode(&r).unwrap()).unwrap().pages[0];
    assert_eq!((p.pixels(), p.alpha, p.extra_samples.as_slice()), (&ga, Alpha::Associated, &[1_u16][..]));
    let cmyk = Samples::U16(pattern((w * h * 4) as usize, 8).iter().map(|v| *v as u16).collect());
    let p = &dec(&encode(&req(w, h, ColorModel::Cmyk, 16, &cmyk)).unwrap()).unwrap().pages[0];
    assert_eq!((p.pixels(), p.photometric), (&cmyk, Photometric::Cmyk));
    let fl = Samples::F32((0..(w * h * 2) as usize).map(|i| i as f32 / 7.0).collect());
    let mut r = req(w, h, ColorModel::GrayA, 32, &fl);
    r.alpha = Alpha::Unassociated;
    r.compression = Compression::Deflate;
    let p = &dec(&encode(&r).unwrap()).unwrap().pages[0];
    assert_eq!((p.pixels(), p.sample_format), (&fl, SampleFormat::Float));
    // 10-bit export is a typed refusal in this slice.
    let s = Samples::U16(vec![0; 4]);
    assert_eq!(encode(&req(2, 2, ColorModel::Gray, 10, &s)).unwrap_err().code(), "bit_depth_10_unsupported");
}

/// 4. Hostile input: u32::MAX dimensions, aggregate budget, entry count, IFD cycle, strip past the end,
///    truncation and a byte-mutation sweep. None may panic.
#[test]
fn hostile_inputs_refuse_without_panic() {
    // dims_u32_max: overflow with permissive limits, `limit_dimension` with the defaults.
    let mut b = Builder::new(true);
    b.page(u32::MAX, u32::MAX, 2, &[16, 16, 16, 16], &[0; 8], vec![short(true, 277, 4)]);
    let permissive = Limits {
        max_dimension: u32::MAX,
        max_page_decoded_bytes: u64::MAX,
        max_total_decoded_bytes: u64::MAX,
        ..Limits::default()
    };
    assert_eq!(decode(&b.buf, &permissive, &live()).unwrap_err(), TiffError::DimsOverflow);
    assert_eq!(dec(&b.buf).unwrap_err(), TiffError::Limit("limit_dimension"));
    // Big but representable dimensions fail the page byte budget before any allocation.
    let mut b = Builder::new(true);
    b.page(60_000, 60_000, 1, &[8], &[0; 8], vec![]);
    let tight = Limits { max_dimension: 70_000, ..Limits::default() };
    assert_eq!(decode(&b.buf, &tight, &live()).unwrap_err(), TiffError::Limit("limit_page_bytes"));

    // aggregate_over_budget: three 16x16 gray pages are 768 bytes together.
    let mut b = Builder::new(false);
    for _ in 0..3 {
        b.page(16, 16, 1, &[8], &[7; 256], vec![]);
    }
    let l = |total, pages| Limits { max_total_decoded_bytes: total, max_pages: pages, ..Limits::default() };
    assert_eq!(decode(&b.buf, &l(600, 8), &live()).unwrap_err(), TiffError::Limit("aggregate_over_budget"));
    assert_eq!(decode(&b.buf, &l(768, 2), &live()).unwrap_err(), TiffError::Limit("limit_pages"));
    assert_eq!(decode(&b.buf, &l(768, 3), &live()).unwrap().pages.len(), 3);

    // IFD entry count 65535 and an IFD that links to itself.
    let mut raw = vec![b'I', b'I', 42, 0, 8, 0, 0, 0, 0xFF, 0xFF];
    raw.resize(64, 0);
    assert_eq!(dec(&raw).unwrap_err(), TiffError::IfdEntryCount);
    let mut b = Builder::new(true);
    let off = b.page(2, 2, 1, &[8], &[1, 2, 3, 4], vec![]);
    let next_pos = b.buf.len() - 4; // the IFD ends with its next pointer
    b.buf[next_pos..].copy_from_slice(&off.to_le_bytes());
    assert_eq!(dec(&b.buf).unwrap_err(), TiffError::IfdCycle);

    // Strip offset past the end.
    let mut b = Builder::new(true);
    b.page(2, 2, 1, &[8], &[1, 2, 3, 4], vec![long(true, 273, 1_000_000)]);
    assert_eq!(dec(&b.buf).unwrap_err(), TiffError::Truncated);

    // Truncation at every length and a byte mutation sweep over an encoded LZW file: typed result, no panic.
    let s = Samples::U8(pattern(8 * 8 * 3, 2).iter().map(|v| *v as u8).collect());
    let small = Limits { max_page_decoded_bytes: 1 << 20, max_total_decoded_bytes: 1 << 22, ..Limits::default() };
    let mut good = Vec::new();
    for compression in [Compression::None, Compression::Lzw, Compression::Deflate, Compression::PackBits] {
        let mut r = req(8, 8, ColorModel::Rgb, 8, &s);
        r.compression = compression;
        r.predictor = Predictor::Horizontal;
        good = encode(&r).unwrap();
        for n in 0..good.len() {
            let _ = decode(&good[..n], &small, &live());
        }
        for i in 0..good.len() {
            for v in [0_u8, 1, 0x7F, 0x80, 0xFF] {
                let mut m = good.clone();
                m[i] = v;
                let _ = decode(&m, &small, &live());
            }
        }
    }
    // Cancellation is typed and produces nothing.
    let dead = live();
    dead.cancel();
    assert_eq!(decode(&good, &Limits::default(), &dead).unwrap_err(), TiffError::Canceled);
}

#[derive(Default)]
struct VecSink(Vec<(DeliveryClass, Vec<u8>)>);
impl SinkPort for VecSink {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> std::result::Result<(), DeliveryError> {
        self.0.push((class, bytes.to_vec()));
        Ok(())
    }
}

/// 5. Typed refusals (CIELab, palette, ZSTD id), multi-page, and one terminal Observe frame.
#[test]
fn typed_refusals_multipage_and_observe_frame() {
    // `Builder::page` writes Compression=1; the variants rewrite that entry in place.
    let patch = |mut buf: Vec<u8>, comp: u16| {
        // find the Compression entry (tag 259) in the first IFD and set its value
        let ifd = u32::from_le_bytes(buf[4..8].try_into().unwrap()) as usize;
        let n = u16::from_le_bytes(buf[ifd..ifd + 2].try_into().unwrap()) as usize;
        for i in 0..n {
            let e = ifd + 2 + i * 12;
            if u16::from_le_bytes(buf[e..e + 2].try_into().unwrap()) == 259 {
                buf[e + 8..e + 10].copy_from_slice(&comp.to_le_bytes());
            }
        }
        buf
    };
    let mk = |pi: u16, comp: u16| {
        let mut b = Builder::new(true);
        let bits: &[u16] = if pi == 8 { &[8, 8, 8] } else { &[8] };
        b.page(2, 2, pi, bits, &[0; 12], vec![]);
        patch(b.buf, comp)
    };
    assert_eq!(dec(&mk(10, 1)).unwrap_err(), TiffError::unsupported("photometric_itulab"));
    assert_eq!(dec(&mk(4, 1)).unwrap_err(), TiffError::unsupported("photometric_mask"));
    assert_eq!(
        dec(&mk(1, 50000)).unwrap_err(),
        TiffError::unsupported_value("compression_unsupported", 50000)
    );
    assert_eq!(dec(&mk(1, 4)).unwrap_err().code(), "compression_unsupported"); // Fax4
    assert_eq!(dec(&mk(1, 34712)).unwrap_err().code(), "compression_unsupported"); // JPEG 2000
    assert_eq!(dec(&mk(32845, 1)).unwrap_err().code(), "photometric_logluv");
    assert_eq!(dec(b"nope-not-a-tiff").unwrap_err(), TiffError::NotTiff);
    assert_eq!(dec(&[b'I', b'I', 43, 0, 8, 0, 0, 0]).unwrap_err(), TiffError::Truncated);

    // Multi-page returns every page in order.
    let mut b = Builder::new(false);
    for v in 0..3_u8 {
        b.page(4, 4, 1, &[8], &[v; 16], vec![]);
    }
    let f = dec(&b.buf).unwrap();
    assert_eq!(f.pages.len(), 3);
    for (v, p) in f.pages.iter().enumerate() {
        assert_eq!(p.pixels(), &Samples::U8(vec![v as u8; 16]));
    }

    // One terminal Observe frame per call: Success, Unsupported failure, Canceled.
    let observe = || {
        let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
        let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
        Observe::new(7, 1, resource, actor, Budget::new(1, 2048).unwrap())
    };
    let dead = live();
    dead.cancel();
    for (result, want) in [
        (dec(&b.buf).map(|_| ()), Outcome::Success),
        (dec(&mk(10, 1)).map(|_| ()), Outcome::Failure(hsk_studio_observe::FailureCode::Unsupported)),
        (decode(&b.buf, &Limits::default(), &dead).map(|_| ()), Outcome::Canceled),
    ] {
        let mut sink = VecSink::default();
        let mut o = observe();
        let receipt = report(&result, 7, 1, &live(), &mut o, &mut sink).unwrap();
        assert_eq!(receipt.outcome, want);
        assert_eq!(sink.0.len(), 1);
        assert_eq!(sink.0[0].0, DeliveryClass::Terminal);
    }
}

/// 6. Slice 2: palette expansion through the ColorMap, CIELab delivered as stored, ImageSourceData envelope.
#[test]
fn palette_cielab_and_image_source_data() {
    for le in [true, false] {
        // 4x2 8-bit palette; entry i maps to (i*257, 65535 - i*257, i*100).
        let idx: Vec<u8> = vec![0, 1, 2, 3, 255, 254, 1, 0];
        let mut map = Vec::new();
        for plane in 0..3_u32 {
            for i in 0..256_u32 {
                map.push(match plane {
                    0 => (i * 257) as u16,
                    1 => (65535 - i * 257) as u16,
                    _ => (i * 100) as u16,
                });
            }
        }
        let mut b = Builder::new(le);
        b.page(4, 2, 3, &[8], &idx, vec![shorts_e(le, 320, &map)]);
        let p = &dec(&b.buf).expect("palette").pages[0];
        let want: Vec<u16> = idx
            .iter()
            .flat_map(|i| {
                let i = u32::from(*i);
                [(i * 257) as u16, (65535 - i * 257) as u16, (i * 100) as u16]
            })
            .collect();
        assert_eq!(p.pixels(), &Samples::U16(want));
        assert_eq!((p.source_photometric, p.source_bits_per_sample, p.bits_per_sample, p.samples_per_pixel), (3, 8, 16, 3));

        // CIELab 8-bit and 16-bit: raw samples, a/b two-complement left as stored.
        let lab8: Vec<u8> = vec![200, 0x80, 0x7F, 10, 0xFF, 1];
        let mut b = Builder::new(le);
        b.page(2, 1, 8, &[8, 8, 8], &lab8, vec![]);
        let p = &dec(&b.buf).expect("lab8").pages[0];
        assert_eq!((p.photometric, p.pixels()), (Photometric::CieLab, &Samples::U8(lab8)));
        let lab16: Vec<u16> = vec![60000, 0x8000, 0x7FFF, 10, 0xFFFF, 1];
        let raw16: Vec<u8> = lab16.iter().flat_map(|v| if le { v.to_le_bytes() } else { v.to_be_bytes() }).collect();
        let mut b = Builder::new(le);
        b.page(2, 1, 8, &[16, 16, 16], &raw16, vec![]);
        assert_eq!(dec(&b.buf).expect("lab16").pages[0].pixels(), &Samples::U16(lab16));

        // Wrong ColorMap length is corrupt, not guessed.
        let mut b = Builder::new(le);
        b.page(4, 2, 3, &[8], &idx, vec![shorts_e(le, 320, &map[..300])]);
        assert_eq!(dec(&b.buf).unwrap_err(), TiffError::Corrupt("bad_color_map"));
    }

    // ImageSourceData: tag 37724 comes out raw with the file byte order; the envelope keys decode.
    let sig = b"Adobe Photoshop Document Data Block\0";
    for (key, order, wide) in [
        (b"8BIM", ByteOrder::Big, false),
        (b"MIB8", ByteOrder::Little, false),
        (b"8B64", ByteOrder::Big, true),
        (b"46B8", ByteOrder::Little, true),
    ] {
        let mut data = sig.to_vec();
        data.extend_from_slice(key);
        data.extend_from_slice(b"8BIMLayr\x00\x00\x00\x04abcd");
        let mut b = Builder::new(false);
        b.page(2, 2, 1, &[8], &[1, 2, 3, 4], vec![bytes_e(37724, &data), bytes_e(34377, b"res")]);
        let p = &dec(&b.buf).unwrap().pages[0];
        let block = p.image_source_data.clone().expect("37724");
        assert_eq!((block.byte_order, &block.bytes), (ByteOrder::Big, &data));
        assert_eq!(p.image_resources.as_ref().map(|r| r.bytes.as_slice()), Some(&b"res"[..]));
        let parsed = image_source_data(&block).unwrap();
        assert_eq!((parsed.byte_order, parsed.wide_sizes), (order, wide));
        assert_eq!(parsed.blocks, b"8BIMLayr\x00\x00\x00\x04abcd");
    }
    let bad = RawBlock { byte_order: ByteOrder::Big, bytes: b"Adobe Photoshop Document Data Blocx\0XXXX".to_vec() };
    assert_eq!(image_source_data(&bad).unwrap_err(), TiffError::Corrupt("image_source_data_signature"));
    let key = RawBlock { byte_order: ByteOrder::Big, bytes: [&sig[..], b"ZZZZ"].concat() };
    assert_eq!(image_source_data(&key).unwrap_err().code(), "image_source_data_key");
    let short = RawBlock { byte_order: ByteOrder::Big, bytes: sig.to_vec() };
    assert_eq!(image_source_data(&short).unwrap_err(), TiffError::Truncated);
}
