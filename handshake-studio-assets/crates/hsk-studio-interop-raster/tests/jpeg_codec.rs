//! JPEG decode slice tests: Pillow/libjpeg-turbo-written fixtures against Pillow's own decode (independent
//! oracle), APP2 ICC reassembly, hostile inputs.
mod jpeg_fixtures;

use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_raster::*;
use jpeg_fixtures as fx;

fn live() -> CancellationToken {
    CancellationToken::default()
}

fn max_diff(a: &[u8], b: &[u8]) -> u8 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0)
}

fn u8s(img: &RasterImage) -> &[u8] {
    match &img.samples {
        Samples::U8(v) => v,
        Samples::U16(_) => panic!("JPEG decodes to 8-bit"),
    }
}

/// Inserts `segments` (marker, payload) right after SOI.
fn splice(jpeg: &[u8], segments: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let mut out = jpeg[..2].to_vec();
    for (marker, payload) in segments {
        out.extend_from_slice(&[0xFF, *marker]);
        out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(payload);
    }
    out.extend_from_slice(&jpeg[2..]);
    out
}

fn icc_segment(seq: u8, count: u8, data: &[u8]) -> (u8, Vec<u8>) {
    let mut p = b"ICC_PROFILE\0".to_vec();
    p.extend_from_slice(&[seq, count]);
    p.extend_from_slice(data);
    (0xE2, p)
}

/// Replaces the first SOF0 marker byte.
fn with_sof(jpeg: &[u8], marker: u8) -> Vec<u8> {
    let mut v = jpeg.to_vec();
    let i = v.windows(2).position(|w| w == [0xFF, 0xC0]).expect("SOF0");
    v[i + 1] = marker;
    v
}

#[test]
fn jpeg_decode_matches_pillow_for_gray_420_progressive_cmyk_and_exif() {
    let lim = Limits::default();
    type Case<'a> = (&'a str, &'a [u8], &'a [u8], Colour, &'a str);
    let cases: [Case<'_>; 4] = [
        ("gray", fx::GRAY8, fx::GRAY8_PIL, Colour::Gray, "baseline"),
        ("420", fx::COLOR420, fx::COLOR420_PIL, Colour::Rgb, "baseline"),
        ("progressive", fx::PROGRESSIVE, fx::PROGRESSIVE_PIL, Colour::Rgb, "progressive"),
        ("exif6", fx::EXIF6, fx::EXIF6_PIL, Colour::Gray, "baseline"),
    ];
    for (name, file, oracle, colour, process) in cases {
        assert_eq!(sniff(file), Some(RasterFormat::Jpeg));
        let img = decode(file, &lim, &live()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(img.colour, colour, "{name}");
        let d = max_diff(u8s(&img), oracle);
        println!("JPEG {name}: max abs diff vs Pillow {d}");
        assert!(d <= 2, "{name}: max diff {d} > 2");
        let meta = img.jpeg.as_ref().unwrap();
        assert_eq!(meta.process, process, "{name}");
        assert!(meta.jfif, "{name}");
        assert!(img.icc.is_none());
    }
    let img = decode(fx::COLOR420, &lim, &live()).unwrap();
    assert_eq!(img.jpeg.as_ref().unwrap().sampling, vec![(2, 2), (1, 1), (1, 1)]);
    assert!(decode(fx::PROGRESSIVE, &lim, &live()).unwrap().jpeg.unwrap().scans > 1);
    let img = decode(fx::EXIF6, &lim, &live()).unwrap();
    assert_eq!(img.exif_orientation, Some(6), "Exif orientation recorded, not applied");
    assert_eq!((img.width, img.height), (8, 8));
    // CMYK (Adobe APP14 transform 0): stored inverted (0 = full ink, PSD convention); Pillow un-inverts.
    let img = decode(fx::CMYK, &lim, &live()).unwrap();
    assert_eq!(img.colour, Colour::Cmyk);
    let meta = img.jpeg.as_ref().unwrap();
    assert_eq!((meta.adobe_transform, meta.cmyk_inverted, meta.ycck_converted), (Some(0), true, false));
    let stored: Vec<u8> = fx::CMYK_PIL.iter().map(|v| 255 - v).collect();
    let d = max_diff(u8s(&img), &stored);
    println!("JPEG cmyk: max abs diff vs Pillow {d}");
    assert!(d <= 2, "cmyk max diff {d}");
    let planes = img.planes_be().unwrap();
    assert_eq!((planes.len(), planes[0].len()), (4, 64));
}

#[test]
fn jpeg_icc_app2_chunks_reassembled_in_sequence_order() {
    let lim = Limits::default();
    let icc: Vec<u8> = (0..300u32).map(|i| (i * 7 + 1) as u8).collect();
    // Sequence 2 first in file order: assembly follows the sequence numbers.
    let file = splice(fx::COLOR420, &[icc_segment(2, 2, &icc[180..]), icc_segment(1, 2, &icc[..180])]);
    let img = decode(&file, &lim, &live()).expect("decode with ICC");
    assert_eq!(img.icc.as_deref(), Some(&icc[..]), "byte-exact reassembly");
    assert!(max_diff(u8s(&img), fx::COLOR420_PIL) <= 2);
    // Inconsistent sets are refused, never half-assembled.
    for bad in [
        vec![icc_segment(1, 2, &icc)],
        vec![icc_segment(1, 2, &icc), icc_segment(1, 2, &icc)],
        vec![icc_segment(1, 2, &icc), icc_segment(2, 3, &icc)],
        vec![icc_segment(0, 1, &icc)],
    ] {
        assert_eq!(decode(&splice(fx::GRAY8, &bad), &lim, &live()).unwrap_err().code(), "icc_chunks_inconsistent");
    }
    let small = Limits { max_icc_bytes: 299, ..Limits::default() };
    assert_eq!(decode(&file, &small, &live()).unwrap_err().code(), "icc_over_limit");
    let many: Vec<(u8, Vec<u8>)> = (0..256).map(|i| icc_segment((i % 255 + 1) as u8, 255, &[1])).collect();
    assert_eq!(decode(&splice(fx::GRAY8, &many), &lim, &live()).unwrap_err().code(), "icc_chunk_count");
}

#[test]
fn jpeg_hostile_inputs_refuse_typed_without_panic() {
    let lim = Limits::default();
    // dims_u32_max for JPEG: SOF 65535 x 65535 refuses before any codec allocation.
    let mut huge = fx::COLOR420.to_vec();
    let i = huge.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
    huge[i + 5..i + 9].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
    assert_eq!(decode(&huge, &lim, &live()).unwrap_err().code(), "decoded_bytes_over_limit");
    let narrow = Limits { max_dimension: 30_000, ..Limits::default() };
    assert_eq!(decode(&huge, &narrow, &live()).unwrap_err().code(), "dims_over_limit");
    // aggregate_over_budget: 16x16 RGB needs 768 output bytes plus codec MCU rows.
    let tight = Limits { max_decoded_bytes: 1024, ..Limits::default() };
    assert_eq!(decode(fx::COLOR420, &tight, &live()).unwrap_err().code(), "decoded_bytes_over_limit");
    // Truncated entropy data and a missing EOI.
    let f = fx::COLOR420;
    assert_eq!(decode(&f[..f.len() - 40], &lim, &live()).unwrap_err().code(), "truncated");
    assert_eq!(decode(&f[..f.len() - 2], &lim, &live()).unwrap_err().code(), "truncated");
    // Unsupported coding processes are typed refusals.
    assert_eq!(decode(&with_sof(fx::GRAY8, 0xC9), &lim, &live()).unwrap_err().code(), "arithmetic_coding_unsupported");
    assert_eq!(decode(&with_sof(fx::GRAY8, 0xC3), &lim, &live()).unwrap_err().code(), "jpeg_lossless_unsupported");
    assert_eq!(decode(&with_sof(fx::GRAY8, 0xC5), &lim, &live()).unwrap_err().code(), "jpeg_hierarchical_unsupported");
    let mut p12 = fx::GRAY8.to_vec();
    let i = p12.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
    p12[i + 4] = 12;
    assert_eq!(decode(&p12, &lim, &live()).unwrap_err().code(), "jpeg_precision_unsupported");
    let one_scan = Limits { max_jpeg_scans: 1, ..Limits::default() };
    assert_eq!(decode(fx::PROGRESSIVE, &one_scan, &live()).unwrap_err().code(), "scans_over_limit");
    assert_eq!(decode(&[0xFF, 0xD8, 0xFF], &lim, &live()).unwrap_err().code(), "truncated");
    // Mutation sweep over the marker region and truncations: typed result, never a panic.
    let small = Limits { max_decoded_bytes: 1 << 20, ..Limits::default() };
    for base in [fx::COLOR420, fx::PROGRESSIVE, fx::CMYK] {
        for i in 0..base.len() {
            for v in [0u8, 0x01, 0xFF] {
                let mut m = base.to_vec();
                m[i] = v;
                let _ = decode(&m, &small, &live());
            }
        }
        for k in 0..base.len() {
            let _ = decode(&base[..k], &small, &live());
        }
    }
}
