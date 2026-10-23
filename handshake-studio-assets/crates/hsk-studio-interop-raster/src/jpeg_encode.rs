//! Clean-room baseline JPEG encoder (decision J1), written from ITU-T T.81 alone: sequential DCT, Huffman
//! coding (Annex F.1.2), the Annex K.1/K.2 quantization tables and K.3 Huffman tables, JFIF APP0 (T.871) and
//! ICC APP2 chunks (ICC.1 Annex B.4). 4:4:4 and 4:2:0, 8-bit Gray or RGB (converted to YCbCr per T.871).
//!
//! Deterministic: the FDCT and colour conversion use f32 with literal constants and a fixed operation order
//! (IEEE-754 basic operations are exactly rounded; Rust never contracts to FMA), integer coding afterwards,
//! no threads, no SIMD intrinsics. The output bytes are a pure function of the request (test
//! jpeg_encoder_round_trip_psnr_icc_and_determinism pins a hash).
//!
//! Integer arithmetic below is explicit: sizes use checked operations with typed errors; the coding loop uses
//! wrapping operations on values bounded by construction (8-bit samples: |coefficient| <= 1024, Huffman
//! codes <= 16 bits, bit accumulator < 24 bits).
use crate::error::{cmul, to_usize, RasterError, Result};
use crate::limits::MAX_ENCODE_BYTES;
use crate::model::{Colour, Samples};

/// Natural (row-major) index of each zig-zag position (T.81 Figure A.6).
pub(crate) const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21,
    28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61,
    54, 47, 55, 62, 63,
];

/// T.81 Table K.1 (luminance), natural order.
pub(crate) const QUANT_LUMA: [u8; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56, 14, 17, 22, 29, 51,
    87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113, 92, 49, 64, 78, 87, 103, 121, 120,
    101, 72, 92, 95, 98, 112, 100, 103, 99,
];

/// T.81 Table K.2 (chrominance), natural order.
pub(crate) const QUANT_CHROMA: [u8; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99, 47, 66, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99,
];

/// T.81 Table K.3-K.6 Huffman specifications: (BITS, HUFFVAL).
pub(crate) const DC_LUMA: ([u8; 16], &[u8]) = ([0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0], &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
pub(crate) const DC_CHROMA: ([u8; 16], &[u8]) = ([0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0], &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
pub(crate) const AC_LUMA: ([u8; 16], &[u8]) = (
    [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d],
    &[
        0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71,
        0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72,
        0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37,
        0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59,
        0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83,
        0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3,
        0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3,
        0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2,
        0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
    ],
);
pub(crate) const AC_CHROMA: ([u8; 16], &[u8]) = (
    [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77],
    &[
        0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71, 0x13, 0x22,
        0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0, 0x15, 0x62, 0x72, 0xd1,
        0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x35, 0x36,
        0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58,
        0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a,
        0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a,
        0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba,
        0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda,
        0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
    ],
);

/// FDCT basis `T[u][x] = C(u)/2 * cos((2x+1) u pi / 16)`, `C(0) = 1/sqrt(2)` (T.81 A.3.3), rounded to f32.
const BASIS: [[f32; 8]; 8] = [
    [0.35355338, 0.35355338, 0.35355338, 0.35355338, 0.35355338, 0.35355338, 0.35355338, 0.35355338],
    [0.49039263, 0.4157348, 0.27778512, 0.09754516, -0.09754516, -0.27778512, -0.4157348, -0.49039263],
    [0.46193975, 0.19134171, -0.19134171, -0.46193975, -0.46193975, -0.19134171, 0.19134171, 0.46193975],
    [0.4157348, -0.09754516, -0.49039263, -0.27778512, 0.27778512, 0.49039263, 0.09754516, -0.4157348],
    [0.35355338, -0.35355338, -0.35355338, 0.35355338, 0.35355338, -0.35355338, -0.35355338, 0.35355338],
    [0.27778512, -0.49039263, 0.09754516, 0.4157348, -0.4157348, -0.09754516, 0.49039263, -0.27778512],
    [0.19134171, -0.46193975, 0.46193975, -0.19134171, -0.19134171, 0.46193975, -0.46193975, 0.19134171],
    [0.09754516, -0.27778512, 0.4157348, -0.49039263, 0.49039263, -0.4157348, 0.27778512, -0.09754516],
];

/// Chroma subsampling of the YCbCr output (ignored for Gray).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JpegSubsampling {
    /// Every component 1x1.
    S444,
    /// Y 2x2, Cb/Cr 1x1 (each chroma sample is the rounded mean of its 2x2 luma-grid samples).
    S420,
}

#[derive(Clone, Debug)]
pub struct JpegEncodeRequest<'a> {
    pub width: u32,
    pub height: u32,
    /// Gray or Rgb (alpha, CMYK and 16-bit are refused: `jpeg_colour_unsupported`, `jpeg_depth_unsupported`).
    pub colour: Colour,
    /// Interleaved 8-bit samples.
    pub samples: &'a Samples,
    /// 1..=100: Annex K tables scaled by the common quality convention (q < 50: 5000/q %, else 200 - 2q %),
    /// clamped to 1..=255 (8-bit baseline tables).
    pub quality: u8,
    pub subsampling: JpegSubsampling,
    pub icc: Option<&'a [u8]>,
}

/// Bytes of ICC payload per APP2 segment: 65535 - 2 (length) - 12 ("ICC_PROFILE\0") - 2 (seq, count).
const ICC_CHUNK: usize = 65_519;

/// Annex K table scaled for `quality`, natural order.
pub(crate) fn scaled_table(base: &[u8; 64], quality: u8) -> [u8; 64] {
    let q = u32::from(quality.clamp(1, 100));
    let scale = if q < 50 { 5000u32.checked_div(q).unwrap_or(5000) } else { 200u32.wrapping_sub(q.wrapping_mul(2)) };
    let mut out = [1u8; 64];
    for (o, b) in out.iter_mut().zip(base) {
        let v = u32::from(*b).wrapping_mul(scale).wrapping_add(50).checked_div(100).unwrap_or(1);
        *o = u8::try_from(v.clamp(1, 255)).unwrap_or(255);
    }
    out
}

struct Huff {
    code: [u16; 256],
    size: [u8; 256],
}

/// Canonical codes from BITS/HUFFVAL (T.81 Annex C).
fn huff((bits, vals): ([u8; 16], &[u8])) -> Huff {
    let mut h = Huff { code: [0; 256], size: [0; 256] };
    let mut code = 0u16;
    let mut k = 0usize;
    for (len, count) in (1u8..=16).zip(bits) {
        for _ in 0..count {
            if let Some(v) = vals.get(k) {
                h.code[usize::from(*v)] = code;
                h.size[usize::from(*v)] = len;
            }
            code = code.wrapping_add(1);
            k = k.wrapping_add(1);
        }
        code = code.wrapping_shl(1);
    }
    h
}

struct Bits {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl Bits {
    /// Appends the low `len` (<= 16) bits of `code`, stuffing 0x00 after every 0xFF byte (T.81 F.1.2.3).
    fn put(&mut self, code: u32, len: u32) {
        let mask = 1u32.wrapping_shl(len).wrapping_sub(1);
        self.acc = self.acc.wrapping_shl(len) | (code & mask);
        self.n = self.n.wrapping_add(len);
        while self.n >= 8 {
            self.n = self.n.wrapping_sub(8);
            let byte = (self.acc.wrapping_shr(self.n) & 0xFF) as u8;
            self.out.push(byte);
            if byte == 0xFF {
                self.out.push(0);
            }
        }
        self.acc &= 1u32.wrapping_shl(self.n).wrapping_sub(1);
    }

    /// Pads the last byte with 1-bits.
    fn flush(&mut self) {
        if self.n > 0 {
            let pad = 8u32.wrapping_sub(self.n);
            self.put(1u32.wrapping_shl(pad).wrapping_sub(1), pad);
        }
    }

    fn symbol(&mut self, table: &Huff, symbol: u8) -> Result<()> {
        let size = table.size[usize::from(symbol)];
        if size == 0 {
            return Err(RasterError::Corrupt("jpeg_huffman_symbol_missing"));
        }
        self.put(u32::from(table.code[usize::from(symbol)]), u32::from(size));
        Ok(())
    }
}

/// Magnitude category SSSS (T.81 Table F.1/F.2) and the appended bits of `v`.
fn category(v: i32) -> (u8, u32) {
    if v == 0 {
        return (0, 0);
    }
    let size = 32u32.wrapping_sub(v.unsigned_abs().leading_zeros());
    let bits = if v > 0 { v as u32 } else { (v.wrapping_sub(1) as u32) & 1u32.wrapping_shl(size).wrapping_sub(1) };
    (size as u8, bits)
}

/// One 8x8 block of level-shifted samples (`[y][x]`) -> quantized coefficients in zig-zag order.
/// Separable FDCT (T.81 A.3.3): rows first, then columns, each a fixed-order f32 dot product.
fn fdct_quantize(block: &[[f32; 8]; 8], quant: &[u8; 64]) -> [i32; 64] {
    let mut rows = [[0f32; 8]; 8]; // rows[y][u]
    for (row_out, row_in) in rows.iter_mut().zip(block) {
        for (o, basis) in row_out.iter_mut().zip(&BASIS) {
            *o = basis.iter().zip(row_in).fold(0.0, |s, (b, v)| s + b * v);
        }
    }
    let mut coef = [0f32; 64]; // natural order: v * 8 + u
    for (crow, basis) in coef.chunks_exact_mut(8).zip(&BASIS) {
        for (u, c) in crow.iter_mut().enumerate() {
            *c = basis.iter().zip(&rows).fold(0.0, |s, (b, r)| s + b * r[u]);
        }
    }
    let mut out = [0i32; 64];
    for (o, &natural) in out.iter_mut().zip(&ZIGZAG) {
        *o = (coef[natural] / f32::from(quant[natural])).round().clamp(-2047.0, 2047.0) as i32;
    }
    out
}

struct Component {
    id: u8,
    h: usize,
    v: usize,
    /// Quantization / Huffman table index (0 luma, 1 chroma).
    table: usize,
    /// Full-resolution component samples (`width x height`), before subsampling.
    plane: Vec<u8>,
}

fn rgb_to_ycbcr(r: u8, g: u8, b: u8) -> [u8; 3] {
    let (r, g, b) = (f32::from(r), f32::from(g), f32::from(b));
    let c = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    [
        c(0.299 * r + 0.587 * g + 0.114 * b),
        c(-0.168_736 * r - 0.331_264 * g + 0.5 * b + 128.0),
        c(0.5 * r - 0.418_688 * g - 0.081_312 * b + 128.0),
    ]
}

fn segment(out: &mut Vec<u8>, marker: u8, payload: &[u8]) -> Result<()> {
    let len = u16::try_from(payload.len().checked_add(2).ok_or(RasterError::DimsOverflow)?)
        .map_err(|_| RasterError::Usage("jpeg_segment_too_long"))?;
    out.extend_from_slice(&[0xFF, marker]);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(payload);
    Ok(())
}

fn dht_payload(class_id: u8, (bits, vals): ([u8; 16], &[u8]), out: &mut Vec<u8>) {
    out.push(class_id);
    out.extend_from_slice(&bits);
    out.extend_from_slice(vals);
}

pub fn encode_jpeg(req: &JpegEncodeRequest<'_>) -> Result<Vec<u8>> {
    let Samples::U8(samples) = req.samples else { return Err(RasterError::Usage("jpeg_depth_unsupported")) };
    let gray = match req.colour {
        Colour::Gray => true,
        Colour::Rgb => false,
        _ => return Err(RasterError::Usage("jpeg_colour_unsupported")),
    };
    if !(1..=100).contains(&req.quality) {
        return Err(RasterError::Usage("jpeg_quality_out_of_range"));
    }
    let (w16, h16) = match (u16::try_from(req.width), u16::try_from(req.height)) {
        (Ok(w), Ok(h)) if w > 0 && h > 0 => (w, h),
        _ => return Err(RasterError::Usage("jpeg_dims_unsupported")),
    };
    let (w, h) = (usize::from(w16), usize::from(h16));
    let channels: usize = if gray { 1 } else { 3 };
    let want = to_usize(cmul(cmul(w as u64, h as u64)?, channels as u64)?)?;
    if samples.len() != want {
        return Err(RasterError::Usage("sample_count_mismatch"));
    }
    if want as u64 > MAX_ENCODE_BYTES {
        return Err(RasterError::Limit("encode_over_limit"));
    }
    let icc_chunks = match req.icc {
        Some([]) => return Err(RasterError::Usage("icc_empty")),
        Some(p) => {
            let n = p.len().div_ceil(ICC_CHUNK);
            if n > 255 {
                return Err(RasterError::Usage("icc_too_large_for_jpeg"));
            }
            n
        }
        None => 0,
    };

    // Components at full resolution.
    let wide = !gray && req.subsampling == JpegSubsampling::S420;
    let mut comps: Vec<Component> = if gray {
        vec![Component { id: 1, h: 1, v: 1, table: 0, plane: samples.clone() }]
    } else {
        let n = want.checked_div(3).unwrap_or(0);
        let mut planes = [Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n)];
        for px in samples.chunks_exact(3) {
            let ycc = rgb_to_ycbcr(px[0], px[1], px[2]);
            for (p, s) in planes.iter_mut().zip(ycc) {
                p.push(s);
            }
        }
        let [y, cb, cr] = planes;
        let luma = if wide { 2 } else { 1 };
        vec![
            Component { id: 1, h: luma, v: luma, table: 0, plane: y },
            Component { id: 2, h: 1, v: 1, table: 1, plane: cb },
            Component { id: 3, h: 1, v: 1, table: 1, plane: cr },
        ]
    };
    let hmax = comps.iter().map(|c| c.h).max().unwrap_or(1);
    let vmax = comps.iter().map(|c| c.v).max().unwrap_or(1);
    // Subsample: a component with h < hmax keeps the rounded mean of each hmax/h x vmax/v cell (edge-clamped).
    for c in &mut comps {
        let (fx, fy) = (hmax.checked_div(c.h).unwrap_or(1), vmax.checked_div(c.v).unwrap_or(1));
        if fx == 1 && fy == 1 {
            continue;
        }
        let (cw, ch) = (w.div_ceil(fx), h.div_ceil(fy));
        let mut sub = Vec::with_capacity(cw.wrapping_mul(ch));
        let cells = u32::try_from(fx.wrapping_mul(fy)).unwrap_or(1);
        for y in 0..ch {
            for x in 0..cw {
                let mut sum = 0u32;
                for dy in 0..fy {
                    for dx in 0..fx {
                        let sx = x.wrapping_mul(fx).wrapping_add(dx).min(w.wrapping_sub(1));
                        let sy = y.wrapping_mul(fy).wrapping_add(dy).min(h.wrapping_sub(1));
                        sum = sum.wrapping_add(u32::from(c.plane[sy.wrapping_mul(w).wrapping_add(sx)]));
                    }
                }
                let mean = sum.wrapping_add(cells.wrapping_shr(1)).checked_div(cells).unwrap_or(0);
                sub.push(u8::try_from(mean).unwrap_or(255));
            }
        }
        c.plane = sub;
    }

    let tables = [scaled_table(&QUANT_LUMA, req.quality), scaled_table(&QUANT_CHROMA, req.quality)];
    let dc = [huff(DC_LUMA), huff(DC_CHROMA)];
    let ac = [huff(AC_LUMA), huff(AC_CHROMA)];

    let mut out = vec![0xFF, 0xD8];
    segment(&mut out, 0xE0, b"JFIF\0\x01\x01\x00\x00\x01\x00\x01\x00\x00")?;
    if let Some(icc) = req.icc {
        for (i, chunk) in icc.chunks(ICC_CHUNK).enumerate() {
            let mut p = b"ICC_PROFILE\0".to_vec();
            p.push(u8::try_from(i.wrapping_add(1)).map_err(|_| RasterError::Usage("icc_too_large_for_jpeg"))?);
            p.push(u8::try_from(icc_chunks).map_err(|_| RasterError::Usage("icc_too_large_for_jpeg"))?);
            p.extend_from_slice(chunk);
            segment(&mut out, 0xE2, &p)?;
        }
    }
    let used_tables = if gray { 1 } else { 2 };
    let mut dqt = Vec::new();
    for (id, table) in tables.iter().enumerate().take(used_tables) {
        dqt.push(id as u8);
        dqt.extend(ZIGZAG.iter().map(|&n| table[n]));
    }
    segment(&mut out, 0xDB, &dqt)?;
    let mut sof = vec![8];
    sof.extend_from_slice(&h16.to_be_bytes());
    sof.extend_from_slice(&w16.to_be_bytes());
    sof.push(comps.len() as u8);
    for c in &comps {
        sof.extend_from_slice(&[c.id, (c.h as u8).wrapping_shl(4) | c.v as u8, c.table as u8]);
    }
    segment(&mut out, 0xC0, &sof)?;
    let mut dht = Vec::new();
    dht_payload(0x00, DC_LUMA, &mut dht);
    dht_payload(0x10, AC_LUMA, &mut dht);
    if !gray {
        dht_payload(0x01, DC_CHROMA, &mut dht);
        dht_payload(0x11, AC_CHROMA, &mut dht);
    }
    segment(&mut out, 0xC4, &dht)?;
    let mut sos = vec![comps.len() as u8];
    for c in &comps {
        sos.extend_from_slice(&[c.id, (c.table as u8).wrapping_shl(4) | c.table as u8]);
    }
    sos.extend_from_slice(&[0, 63, 0]);
    segment(&mut out, 0xDA, &sos)?;

    // Entropy-coded data: MCUs left to right, top to bottom; each component's h x v blocks in raster order.
    let mcu_w = hmax.wrapping_mul(8);
    let mcu_h = vmax.wrapping_mul(8);
    let (mcus_x, mcus_y) = (w.div_ceil(mcu_w), h.div_ceil(mcu_h));
    let mut bits = Bits { out, acc: 0, n: 0 };
    let mut pred = [0i32; 3];
    let mut block = [[0f32; 8]; 8];
    for my in 0..mcus_y {
        for mx in 0..mcus_x {
            for (ci, c) in comps.iter().enumerate() {
                let (fx, fy) = (hmax.checked_div(c.h).unwrap_or(1), vmax.checked_div(c.v).unwrap_or(1));
                let (cw, ch) = (w.div_ceil(fx), h.div_ceil(fy));
                for by in 0..c.v {
                    for bx in 0..c.h {
                        let x0 = mx.wrapping_mul(c.h).wrapping_add(bx).wrapping_mul(8);
                        let y0 = my.wrapping_mul(c.v).wrapping_add(by).wrapping_mul(8);
                        for (dy, row) in block.iter_mut().enumerate() {
                            // Edge replication beyond the component's last column/row.
                            let y = y0.wrapping_add(dy).min(ch.wrapping_sub(1));
                            for (dx, b) in row.iter_mut().enumerate() {
                                let x = x0.wrapping_add(dx).min(cw.wrapping_sub(1));
                                *b = f32::from(c.plane[y.wrapping_mul(cw).wrapping_add(x)]) - 128.0;
                            }
                        }
                        let zz = fdct_quantize(&block, &tables[c.table]);
                        let diff = zz[0].wrapping_sub(pred[ci]);
                        pred[ci] = zz[0];
                        let (size, extra) = category(diff);
                        bits.symbol(&dc[c.table], size)?;
                        bits.put(extra, u32::from(size));
                        let mut run = 0u8;
                        for &coef in &zz[1..] {
                            if coef == 0 {
                                run = run.wrapping_add(1);
                                continue;
                            }
                            while run > 15 {
                                bits.symbol(&ac[c.table], 0xF0)?;
                                run = run.wrapping_sub(16);
                            }
                            let (size, extra) = category(coef);
                            bits.symbol(&ac[c.table], run.wrapping_shl(4) | size)?;
                            bits.put(extra, u32::from(size));
                            run = 0;
                        }
                        if run > 0 {
                            bits.symbol(&ac[c.table], 0x00)?;
                        }
                    }
                }
            }
        }
    }
    bits.flush();
    let mut out = bits.out;
    out.extend_from_slice(&[0xFF, 0xD9]);
    Ok(out)
}
