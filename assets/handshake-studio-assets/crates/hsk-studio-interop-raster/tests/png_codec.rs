//! PNG slice tests: round trip + determinism, foreign hand-assembled files (palette, Adam7), hostile inputs.
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_raster::*;

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for b in bytes {
        crc ^= u32::from(*b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
}

/// zlib stream of stored (uncompressed) deflate blocks: an encoder independent of the `png` crate.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = if data.is_empty() { vec![&[]] } else { data.chunks(65_535).collect() };
    for (i, block) in blocks.iter().enumerate() {
        out.push(u8::from(i + 1 == blocks.len()));
        let len = block.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for x in data {
        a = (a + u32::from(*x)) % 65_521;
        b = (b + a) % 65_521;
    }
    out.extend_from_slice(&((b << 16) | a).to_be_bytes());
    out
}

fn ihdr(w: u32, h: u32, depth: u8, colour_type: u8, interlace: u8) -> Vec<u8> {
    let mut d = w.to_be_bytes().to_vec();
    d.extend_from_slice(&h.to_be_bytes());
    d.extend_from_slice(&[depth, colour_type, 0, 0, interlace]);
    d
}

fn png_file(header: &[u8], extra: &[(&[u8; 4], Vec<u8>)], raw: &[u8]) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", header);
    for (kind, data) in extra {
        chunk(&mut out, kind, data);
    }
    chunk(&mut out, b"IDAT", &zlib_stored(raw));
    chunk(&mut out, b"IEND", &[]);
    out
}

fn live() -> CancellationToken {
    CancellationToken::default()
}

#[test]
fn png_round_trip_keeps_samples_icc_and_is_deterministic() {
    let icc: Vec<u8> = (0..3144u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
    let (w, h) = (7u32, 5u32);
    let n = (w * h) as usize;
    let cases = [
        (Colour::Gray, Samples::U8((0..n).map(|i| (i * 37) as u8).collect())),
        (Colour::GrayA, Samples::U16((0..n * 2).map(|i| (i as u16).wrapping_mul(4099)).collect())),
        (Colour::Rgb, Samples::U8((0..n * 3).map(|i| (i * 11 + 3) as u8).collect())),
        (Colour::Rgba, Samples::U16((0..n * 4).map(|i| (i as u16).wrapping_mul(65_521)).collect())),
    ];
    for (colour, samples) in &cases {
        for compression in [PngCompression::Stored, PngCompression::Fast, PngCompression::Balanced, PngCompression::High] {
            let req = PngEncodeRequest { width: w, height: h, colour: *colour, samples, icc: Some(&icc), compression };
            let a = encode_png(&req).expect("encode");
            let b = encode_png(&req).expect("encode again");
            assert_eq!(a, b, "{colour:?} {compression:?}: two encodes byte-identical");
            assert_eq!(sniff(&a), Some(RasterFormat::Png));
            let img = decode(&a, &Limits::default(), &live()).expect("decode");
            assert_eq!((img.width, img.height, img.colour), (w, h, *colour));
            assert_eq!(&img.samples, samples, "{colour:?} samples");
            assert_eq!(img.depth(), samples.depth());
            assert_eq!(img.icc.as_deref(), Some(&icc[..]), "ICC bytes exact");
            assert!(img.losses.is_empty());
        }
    }
    // Planes in interop-psd `Plane` layout: one per channel, 16-bit big-endian.
    let req = PngEncodeRequest { width: w, height: h, colour: Colour::Rgba, samples: &cases[3].1, icc: None, compression: PngCompression::Fast };
    let img = decode(&encode_png(&req).unwrap(), &Limits::default(), &live()).unwrap();
    let planes = img.planes_be().unwrap();
    assert_eq!(planes.len(), 4);
    let Samples::U16(v) = &cases[3].1 else { unreachable!() };
    assert_eq!(&planes[3][..2], &v[3].to_be_bytes());
    assert_eq!(&planes[1][2..4], &v[5].to_be_bytes());
    assert!(img.icc.is_none());
    // CMYK and mismatched sample counts are refused, not guessed.
    let bad = PngEncodeRequest { width: w, height: h, colour: Colour::Cmyk, samples: &cases[3].1, icc: None, compression: PngCompression::Fast };
    assert_eq!(encode_png(&bad).unwrap_err().code(), "png_colour_unsupported");
    let bad = PngEncodeRequest { width: w + 1, height: h, colour: Colour::Rgba, samples: &cases[3].1, icc: None, compression: PngCompression::Fast };
    assert_eq!(encode_png(&bad).unwrap_err().code(), "sample_count_mismatch");
}

#[test]
fn png_from_foreign_encoder_palette_trns_and_adam7() {
    // 4-bit palette, 3x2, tRNS on entry 1: rows pack two indices per byte (high nibble first).
    let palette = vec![10, 20, 30, 200, 100, 50, 0, 255, 0];
    let raw = [0u8, 0x01, 0x20, 0, 0x21, 0x00];
    let file = png_file(&ihdr(3, 2, 4, 3, 0), &[(b"PLTE", palette), (b"tRNS", vec![255, 128])], &raw);
    let img = decode(&file, &Limits::default(), &live()).expect("palette decode");
    assert_eq!(img.colour, Colour::Rgba);
    let want: Vec<u8> = vec![
        10, 20, 30, 255, 200, 100, 50, 128, 0, 255, 0, 255, //
        0, 255, 0, 255, 200, 100, 50, 128, 10, 20, 30, 255,
    ];
    assert_eq!(img.samples, Samples::U8(want));
    let meta = img.png.as_ref().unwrap();
    assert_eq!((meta.source_colour_type, meta.source_bit_depth, meta.transparency_chunk), (3, 4, true));

    // Adam7 interlaced 8-bit gray 5x6 with gAMA + sRGB recorded, plus a 1-bit gray file expanded to 0/255.
    let (w, h) = (5usize, 6usize);
    let pix = |x: usize, y: usize| (x * 40 + y * 7) as u8;
    let passes = [(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)];
    let mut raw = Vec::new();
    for (x0, y0, dx, dy) in passes {
        let cols: Vec<usize> = (x0..w).step_by(dx).collect();
        if cols.is_empty() {
            continue;
        }
        for y in (y0..h).step_by(dy) {
            raw.push(0);
            raw.extend(cols.iter().map(|&x| pix(x, y)));
        }
    }
    let file = png_file(
        &ihdr(w as u32, h as u32, 8, 0, 1),
        &[(b"gAMA", 45_455u32.to_be_bytes().to_vec()), (b"sRGB", vec![1])],
        &raw,
    );
    let img = decode(&file, &Limits::default(), &live()).expect("adam7 decode");
    let want: Vec<u8> = (0..h).flat_map(|y| (0..w).map(move |x| pix(x, y))).collect();
    assert_eq!(img.samples, Samples::U8(want));
    let meta = img.png.as_ref().unwrap();
    assert!(meta.interlaced);
    assert_eq!((meta.gamma, meta.srgb_intent), (Some(45_455), Some(1)));

    let file = png_file(&ihdr(4, 1, 1, 0, 0), &[], &[0, 0b1010_0000]);
    let img = decode(&file, &Limits::default(), &live()).expect("1-bit decode");
    assert_eq!((img.colour, img.source_bit_depth), (Colour::Gray, 1));
    assert_eq!(img.samples, Samples::U8(vec![255, 0, 255, 0]));
}

#[test]
fn png_hostile_inputs_refuse_typed_without_panic() {
    let lim = Limits::default();
    // dims_u32_max: IHDR 0xFFFFFFFF x 0xFFFFFFFF refuses before any allocation.
    let huge = png_file(&ihdr(u32::MAX, u32::MAX, 16, 6, 0), &[], &[0]);
    assert_eq!(decode(&huge, &lim, &live()).unwrap_err().code(), "dims_over_limit");
    let wide = Limits { max_dimension: u32::MAX, ..Limits::default() };
    assert_eq!(decode(&huge, &wide, &live()).unwrap_err().code(), "dims_overflow");
    // aggregate_over_budget: a 64x64 RGBA16 image against a 16 KiB decoded budget.
    let raw = vec![0u8; 64 * (1 + 64 * 8)];
    let file = png_file(&ihdr(64, 64, 16, 6, 0), &[], &raw);
    let small = Limits { max_decoded_bytes: 16 << 10, ..Limits::default() };
    assert_eq!(decode(&file, &small, &live()).unwrap_err().code(), "decoded_bytes_over_limit");
    assert!(decode(&file, &lim, &live()).is_ok());
    // Truncation inside IDAT, missing IEND, broken IHDR, unknown format, WebP, corrupt iCCP.
    assert_eq!(decode(&file[..file.len() / 2], &lim, &live()).unwrap_err().code(), "truncated");
    assert_eq!(decode(&file[..file.len() - 12], &lim, &live()).unwrap_err().code(), "truncated");
    let mut bad = file.clone();
    bad[16 + 8] = 7; // bit depth 7
    assert_eq!(decode(&bad, &lim, &live()).unwrap_err().code(), "corrupt_png");
    assert_eq!(decode(b"GIF89a....", &lim, &live()).unwrap_err().code(), "unknown_format");
    let webp = b"RIFF\x10\0\0\0WEBPVP8 ";
    assert_eq!(decode(webp, &lim, &live()).unwrap_err().code(), "webp_decoder_unavailable");
    let iccp = png_file(&ihdr(1, 1, 8, 0, 0), &[(b"iCCP", b"x\0\0garbage".to_vec())], &[0, 9]);
    assert_eq!(decode(&iccp, &lim, &live()).unwrap_err().code(), "icc_unreadable");
    let tiny_icc = Limits { max_icc_bytes: 100, ..Limits::default() };
    let req_samples = Samples::U8(vec![1]);
    let big_icc = vec![7u8; 5000];
    let req = PngEncodeRequest { width: 1, height: 1, colour: Colour::Gray, samples: &req_samples, icc: Some(&big_icc), compression: PngCompression::Balanced };
    let with_icc = encode_png(&req).unwrap();
    assert!(matches!(decode(&with_icc, &tiny_icc, &live()).unwrap_err().code(), "icc_unreadable" | "icc_over_limit"));
    let canceled = live();
    canceled.cancel();
    assert_eq!(decode(&file, &lim, &canceled).unwrap_err().code(), "canceled");
    // Mutation sweep over header and chunk bytes plus truncations: typed result, never a panic.
    let small = Limits { max_decoded_bytes: 1 << 20, ..Limits::default() };
    for base in [&with_icc, &file] {
        for i in 0..base.len().min(200) {
            for v in [0u8, 0x7F, 0xFF] {
                let mut m = base.clone();
                m[i] = v;
                let _ = decode(&m, &small, &live());
            }
        }
        for k in 0..base.len().min(300) {
            let _ = decode(&base[..k], &small, &live());
        }
    }
}
