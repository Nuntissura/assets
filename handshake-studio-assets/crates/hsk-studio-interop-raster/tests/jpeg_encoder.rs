//! Own baseline JPEG encoder (decision J1): emitted tables against libjpeg-turbo's output, decode-back quality
//! bounds through zune-jpeg, ICC chunking, determinism, refusals.
mod jpeg_fixtures;

use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_raster::*;
use std::collections::BTreeMap;

type Tables = (BTreeMap<u8, Vec<u8>>, BTreeMap<u8, Vec<u8>>);

/// DQT tables by id and DHT tables by class/id byte, from every segment before SOS.
fn tables(jpeg: &[u8]) -> Tables {
    let (mut dqt, mut dht) = (BTreeMap::new(), BTreeMap::new());
    let mut at = 2;
    loop {
        let marker = jpeg[at + 1];
        let len = u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]) as usize;
        let seg = &jpeg[at + 4..at + 2 + len];
        match marker {
            0xDB => {
                for t in seg.chunks(65) {
                    assert_eq!(t[0] >> 4, 0, "8-bit tables");
                    dqt.insert(t[0], t[1..].to_vec());
                }
            }
            0xC4 => {
                let mut p = 0;
                while p < seg.len() {
                    let n: usize = seg[p + 1..p + 17].iter().map(|b| *b as usize).sum();
                    dht.insert(seg[p], seg[p + 1..p + 17 + n].to_vec());
                    p += 17 + n;
                }
            }
            0xDA => return (dqt, dht),
            _ => {}
        }
        at += 2 + len;
    }
}

fn image(w: usize, h: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (x as i32 - 30, y as i32 - 22);
            let disc = dx * dx + dy * dy < 15 * 15;
            let r = if disc { 230 } else { (x * 4) as u8 };
            let g = if disc { 40 } else { (y * 5) as u8 };
            let b = ((x * y) % 97 + 60) as u8;
            v.extend_from_slice(&[r, g, b]);
        }
    }
    v
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse = a.iter().zip(b).map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2)).sum::<f64>() / a.len() as f64;
    10.0 * (255.0f64 * 255.0 / mse).log10()
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}

fn req<'a>(w: u32, h: u32, colour: Colour, samples: &'a Samples, quality: u8, subsampling: JpegSubsampling) -> JpegEncodeRequest<'a> {
    JpegEncodeRequest { width: w, height: h, colour, samples, quality, subsampling, icc: None }
}

#[test]
fn jpeg_encoder_tables_match_libjpeg_turbo_output() {
    // Pillow/libjpeg-turbo wrote the fixtures at quality 90 with the Annex K tables (no optimize): our DQT
    // (zig-zag order, quality scaling) and DHT bytes for the same quality must be identical.
    let rgb = Samples::U8(image(16, 16));
    let ours = encode_jpeg(&req(16, 16, Colour::Rgb, &rgb, 90, JpegSubsampling::S420)).unwrap();
    assert_eq!(tables(&ours), tables(jpeg_fixtures::COLOR420));
    let gray = Samples::U8((0..64).map(|i| i as u8 * 3).collect());
    let ours = encode_jpeg(&req(8, 8, Colour::Gray, &gray, 90, JpegSubsampling::S444)).unwrap();
    assert_eq!(tables(&ours), tables(jpeg_fixtures::GRAY8));
    let (dqt, dht) = tables(&ours);
    assert_eq!((dqt.len(), dht.len()), (1, 2));
}

#[test]
fn jpeg_encoder_round_trip_psnr_icc_and_determinism() {
    let (w, h) = (61u32, 47u32); // odd sizes: partial MCUs and edge replication
    let rgb = Samples::U8(image(w as usize, h as usize));
    let Samples::U8(src) = &rgb else { unreachable!() };
    let lim = Limits::default();
    let live = CancellationToken::default();
    // (quality, subsampling, minimum PSNR in dB vs the source). Reference: libjpeg-turbo 3.1.4.1 (Pillow 12.2.0)
    // on the same image measured 36.92 / 31.76 / 24.05 / 23.20 / 24.24 dB (gray q90: 43.51); bound = reference - 0.5.
    // The image is deliberately hard (hard-edged disc, (x*y) mod 97 texture, wrapping ramps).
    let cases = [
        (95, JpegSubsampling::S444, 36.42),
        (90, JpegSubsampling::S444, 31.26),
        (90, JpegSubsampling::S420, 23.55),
        (75, JpegSubsampling::S420, 22.70),
        (50, JpegSubsampling::S444, 23.74),
    ];
    for (q, sub, min) in cases {
        let file = encode_jpeg(&req(w, h, Colour::Rgb, &rgb, q, sub)).unwrap();
        assert_eq!(file, encode_jpeg(&req(w, h, Colour::Rgb, &rgb, q, sub)).unwrap(), "deterministic");
        let img = decode(&file, &lim, &live).expect("decode-back");
        assert_eq!((img.width, img.height, img.colour), (w, h, Colour::Rgb));
        let Samples::U8(back) = &img.samples else { unreachable!() };
        let p = psnr(src, back);
        println!("ENCODE q={q} {sub:?}: {} bytes, PSNR {p:.2} dB, fnv {:016x}", file.len(), fnv1a(&file));
        assert!(p >= min, "q={q} {sub:?}: PSNR {p:.2} < {min}");
        if q == 95 {
            // Output bytes are a pure function of the request (f32 FDCT with literal constants, fixed order).
            assert_eq!(fnv1a(&file), 0x4082_b101_de13_c0a7, "pinned encoder output");
        }
    }
    let gray: Vec<u8> = src.chunks_exact(3).map(|p| p[1]).collect();
    let gray_s = Samples::U8(gray.clone());
    let file = encode_jpeg(&req(w, h, Colour::Gray, &gray_s, 90, JpegSubsampling::S420)).unwrap();
    let img = decode(&file, &lim, &live).unwrap();
    let Samples::U8(back) = &img.samples else { unreachable!() };
    let p = psnr(&gray, back);
    println!("ENCODE gray q=90: {} bytes, PSNR {p:.2} dB", file.len());
    assert!(p >= 43.01 && img.colour == Colour::Gray);
    // ICC over one APP2 segment (65519 bytes per chunk) is split and reassembled byte-exact.
    let icc: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
    let with_icc = JpegEncodeRequest { icc: Some(&icc), ..req(w, h, Colour::Rgb, &rgb, 90, JpegSubsampling::S420) };
    let file = encode_jpeg(&with_icc).unwrap();
    let img = decode(&file, &lim, &live).unwrap();
    assert_eq!(img.icc.as_deref(), Some(&icc[..]));
    assert_eq!(img.jpeg.as_ref().unwrap().sampling, vec![(2, 2), (1, 1), (1, 1)]);
}

#[test]
fn jpeg_encoder_refuses_outside_its_envelope() {
    let rgb = Samples::U8(vec![0; 12]);
    let code = |r: &JpegEncodeRequest<'_>| encode_jpeg(r).unwrap_err().code();
    assert_eq!(code(&req(2, 2, Colour::Rgba, &rgb, 90, JpegSubsampling::S444)), "jpeg_colour_unsupported");
    assert_eq!(code(&req(2, 2, Colour::Cmyk, &rgb, 90, JpegSubsampling::S444)), "jpeg_colour_unsupported");
    let deep = Samples::U16(vec![0; 12]);
    assert_eq!(code(&req(2, 2, Colour::Rgb, &deep, 90, JpegSubsampling::S444)), "jpeg_depth_unsupported");
    assert_eq!(code(&req(2, 2, Colour::Rgb, &rgb, 0, JpegSubsampling::S444)), "jpeg_quality_out_of_range");
    assert_eq!(code(&req(2, 2, Colour::Rgb, &rgb, 101, JpegSubsampling::S444)), "jpeg_quality_out_of_range");
    assert_eq!(code(&req(0, 2, Colour::Rgb, &rgb, 90, JpegSubsampling::S444)), "jpeg_dims_unsupported");
    assert_eq!(code(&req(70_000, 1, Colour::Rgb, &rgb, 90, JpegSubsampling::S444)), "jpeg_dims_unsupported");
    assert_eq!(code(&req(3, 2, Colour::Rgb, &rgb, 90, JpegSubsampling::S444)), "sample_count_mismatch");
    let empty: [u8; 0] = [];
    assert_eq!(code(&JpegEncodeRequest { icc: Some(&empty), ..req(2, 2, Colour::Rgb, &rgb, 90, JpegSubsampling::S444) }), "icc_empty");
    let huge = vec![0u8; 255 * 65_519 + 1];
    assert_eq!(
        code(&JpegEncodeRequest { icc: Some(&huge), ..req(2, 2, Colour::Rgb, &rgb, 90, JpegSubsampling::S444) }),
        "icc_too_large_for_jpeg"
    );
    // 1x1 and quality 1 still produce decodable files.
    let one = Samples::U8(vec![200, 10, 90]);
    let file = encode_jpeg(&req(1, 1, Colour::Rgb, &one, 1, JpegSubsampling::S420)).unwrap();
    let img = decode(&file, &Limits::default(), &CancellationToken::default()).unwrap();
    assert_eq!((img.width, img.height), (1, 1));
}
