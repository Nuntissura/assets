//! In-process ProRes decoder written from SMPTE RDD 36:2022 (cargo feature `prores`, default OFF, D24;
//! user-facing name "ProRes (RDD 36 decoder, not Apple-certified)", decoder id `prores-rdd36`). No
//! third-party ProRes implementation was consulted (`SOURCE_PROVENANCE.json`).
//!
//! One packet = one compressed frame (5.1: `frame_size`, `icpf`, frame header, one picture or two
//! field pictures, stuffing). Decoded: 4:2:2 and 4:4:4 color components, progressive and interlaced,
//! bitstream versions 0 and 1. Refused with a typed error: encoded alpha (`prores-alpha`: the decoded
//! frame model has no alpha plane yet), bitstream version 2 and above (6.1.1: abort), reserved
//! chroma/interlace codes.
//!
//! RDD 36 has no bit-depth field. Studio outputs b = 10 for 4:2:2 frames and b = 12 for 4:4:4 frames
//! (an assumption by profile family, recorded in the lane brief; not a statement of the standard).
//! Samples: `s = clamp(floor(2^b * (v + 256) / 512 + 0.5))` with the default clamp `[0, 2^b - 1]`
//! (7.5.1 allows it; ties are pinned upward). Dequantized coefficients `QF * W * qScale / 8` are exact
//! in f64 and go through the crate's reference IDCT (`crate::idct8`), so the output is bit-identical
//! on every IEEE-754 binary64 host.
//!
//! Hostile-input bounds (V4-01/02): frame, picture, slice-table and slice sizes are checked against
//! the bytes that exist (the running sum of slice sizes against the picture) before they are used; the
//! output planes are sized only after the header passed the container-declared size and
//! `Limits::max_chunk_bytes`; per-slice scratch is fixed (8 macroblocks x 4 blocks x 64 coefficients
//! per component). Tests: tests/prores_decode.rs.
#![forbid(unsafe_code)]
#![deny(clippy::arithmetic_side_effects)]
mod golomb;

use crate::error::{ReelError, check_cancel};
use crate::idct8::idct8x8_f64;
use crate::isobmff::ColorTag;
use crate::model::{ChromaFormat, ColorInfo, ColorOrigin, Limits, VideoFormat};
use crate::video::{
    DecodedPicture, PictureSink, VideoDecodeError, VideoDecodeSession, VideoStreamSpec,
};
use hsk_studio_accord::CancellationToken;

/// Largest side accepted when the container declares none.
const UNDECLARED_MAX_SIDE: u32 = 16384;
/// Largest slice: 8 macroblocks (log2_desired_slice_size_in_mb <= 3, 6.2.1).
const MAX_SLICE_MB: usize = 8;
/// Coefficients per component of the largest slice (4 blocks per macroblock for Y and for 4:4:4).
const SLICE_COEFFS: usize = MAX_SLICE_MB * 4 * 64;

fn corrupt(what: &'static str) -> ReelError {
    ReelError::Corrupt { what }
}

fn unsupported(what: &'static str) -> ReelError {
    ReelError::Unsupported { what }
}

/// Progressive scan pattern, Figure 4 (`[v][u]` -> scanned index).
const SCAN_PROGRESSIVE: [[u8; 8]; 8] = [
    [0, 1, 4, 5, 16, 17, 21, 22],
    [2, 3, 6, 7, 18, 20, 23, 28],
    [8, 9, 12, 13, 19, 24, 27, 29],
    [10, 11, 14, 15, 25, 26, 30, 31],
    [32, 33, 37, 38, 45, 46, 53, 54],
    [34, 36, 39, 44, 47, 52, 55, 60],
    [35, 40, 43, 48, 51, 56, 59, 61],
    [41, 42, 49, 50, 57, 58, 62, 63],
];

/// Interlaced scan pattern, Figure 5.
const SCAN_INTERLACED: [[u8; 8]; 8] = [
    [0, 2, 8, 10, 32, 34, 35, 41],
    [1, 3, 9, 11, 33, 36, 40, 42],
    [4, 6, 12, 14, 37, 39, 43, 49],
    [5, 7, 13, 15, 38, 44, 48, 50],
    [16, 18, 19, 25, 45, 47, 51, 57],
    [17, 20, 24, 26, 46, 52, 56, 58],
    [21, 23, 27, 30, 53, 55, 59, 62],
    [22, 28, 29, 31, 54, 60, 61, 63],
];

/// The frame header fields the decoder uses (5.1.1, 6.1.1). Reserved bits are ignored (6.1.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProResFrameHeader {
    pub frame_size: u32,
    pub frame_header_size: u16,
    pub bitstream_version: u8,
    pub encoder_identifier: [u8; 4],
    pub width: u16,
    pub height: u16,
    /// 2 = 4:2:2, 3 = 4:4:4 (Table 1).
    pub chroma_format: u8,
    /// 0 progressive, 1 interlaced top field first, 2 interlaced bottom field first (Table 2).
    pub interlace_mode: u8,
    pub color_primaries: u8,
    pub transfer_characteristic: u8,
    pub matrix_coefficients: u8,
    pub alpha_channel_type: u8,
    pub luma_matrix: [[u8; 8]; 8],
    pub chroma_matrix: [[u8; 8]; 8],
    pub luma_matrix_loaded: bool,
    pub chroma_matrix_loaded: bool,
}

fn get(frame: &[u8], at: usize) -> Result<u8, ReelError> {
    frame
        .get(at)
        .copied()
        .ok_or_else(|| corrupt("prores-header-size"))
}

fn be16(frame: &[u8], at: usize) -> Result<u16, ReelError> {
    Ok(u16::from_be_bytes([
        get(frame, at)?,
        get(
            frame,
            at.checked_add(1)
                .ok_or_else(|| corrupt("prores-header-size"))?,
        )?,
    ]))
}

fn be32(frame: &[u8], at: usize) -> Result<u32, ReelError> {
    let next = at
        .checked_add(2)
        .ok_or_else(|| corrupt("prores-header-size"))?;
    Ok((u32::from(be16(frame, at)?) << 16) | u32::from(be16(frame, next)?))
}

fn matrix(frame: &[u8], at: usize) -> Result<[[u8; 8]; 8], ReelError> {
    let bytes = at
        .checked_add(64)
        .and_then(|end| frame.get(at..end))
        .ok_or_else(|| corrupt("prores-header-size"))?;
    let mut m = [[0u8; 8]; 8];
    for (row, chunk) in m.iter_mut().zip(bytes.chunks_exact(8)) {
        row.copy_from_slice(chunk);
    }
    // 6.1.1: each entry will be in 2..=63.
    if m.iter().flatten().any(|w| !(2..=63).contains(w)) {
        return Err(corrupt("prores-quantization-matrix"));
    }
    Ok(m)
}

/// Parse and validate the frame header (5.1, 5.1.1, 6.1.1, 6.4). `frame` is one whole packet.
pub fn parse_frame_header(frame: &[u8]) -> Result<ProResFrameHeader, ReelError> {
    if frame.len() < 8 {
        return Err(corrupt("prores-frame-size"));
    }
    let frame_size = be32(frame, 0)?;
    if u64::from(frame_size) > frame.len() as u64 || frame_size < 8 {
        return Err(corrupt("prores-frame-size"));
    }
    if frame.get(4..8) != Some(b"icpf".as_slice()) {
        return Err(corrupt("prores-frame-identifier"));
    }
    let h = 8usize;
    let frame_header_size = be16(frame, h)?;
    // Fixed fields end at byte 20 of the header; the size must also cover the matrices it loads.
    h.checked_add(usize::from(frame_header_size))
        .filter(|end| *end as u64 <= u64::from(frame_size))
        .ok_or_else(|| corrupt("prores-header-size"))?;
    if frame_header_size < 20 {
        return Err(corrupt("prores-header-size"));
    }
    let bitstream_version = get(frame, h + 3)?;
    if bitstream_version >= 2 {
        return Err(unsupported("prores-bitstream-version"));
    }
    let mut encoder_identifier = [0u8; 4];
    encoder_identifier.copy_from_slice(
        frame
            .get(h + 4..h + 8)
            .ok_or_else(|| corrupt("prores-header-size"))?,
    );
    let width = be16(frame, h + 8)?;
    let height = be16(frame, h + 10)?;
    let b12 = get(frame, h + 12)?;
    let chroma_format = b12 >> 6;
    let interlace_mode = (b12 >> 2) & 0x03;
    let color_primaries = get(frame, h + 14)?;
    let transfer_characteristic = get(frame, h + 15)?;
    let matrix_coefficients = get(frame, h + 16)?;
    let alpha_channel_type = get(frame, h + 17)? & 0x0F;
    let flags = get(frame, h + 19)?;
    let luma_matrix_loaded = flags & 0x02 != 0;
    let chroma_matrix_loaded = flags & 0x01 != 0;
    if bitstream_version == 0 && (chroma_format != 2 || alpha_channel_type != 0) {
        return Err(corrupt("prores-version-constraint"));
    }
    if chroma_format < 2 {
        return Err(unsupported("prores-chroma-format"));
    }
    if interlace_mode == 3 {
        return Err(unsupported("prores-interlace-mode"));
    }
    if alpha_channel_type > 2 {
        return Err(unsupported("prores-alpha-type"));
    }
    if width == 0 || height == 0 {
        return Err(corrupt("prores-dimensions"));
    }
    let mut at = h + 20;
    let mut needed = 20usize;
    // 7.3: default luma matrix all 4; chroma falls back to luma.
    let mut luma_matrix = [[4u8; 8]; 8];
    if luma_matrix_loaded {
        needed = needed.saturating_add(64);
        if needed > usize::from(frame_header_size) {
            return Err(corrupt("prores-header-size"));
        }
        luma_matrix = matrix(frame, at)?;
        at = at.saturating_add(64);
    }
    let mut chroma_matrix = luma_matrix;
    if chroma_matrix_loaded {
        needed = needed.saturating_add(64);
        if needed > usize::from(frame_header_size) {
            return Err(corrupt("prores-header-size"));
        }
        chroma_matrix = matrix(frame, at)?;
    }
    Ok(ProResFrameHeader {
        frame_size,
        frame_header_size,
        bitstream_version,
        encoder_identifier,
        width,
        height,
        chroma_format,
        interlace_mode,
        color_primaries,
        transfer_characteristic,
        matrix_coefficients,
        alpha_channel_type,
        luma_matrix,
        chroma_matrix,
        luma_matrix_loaded,
        chroma_matrix_loaded,
    })
}

/// slice_size_in_mb (6.2): greedy powers of two down from 2^log2.
fn slice_sizes(width_in_mb: u32, log2: u32) -> Vec<u32> {
    let mut sizes = Vec::new();
    let mut size = 1u32.checked_shl(log2.min(3)).unwrap_or(1);
    let mut remaining = width_in_mb;
    while remaining > 0 && size > 0 {
        while remaining >= size {
            sizes.push(size);
            remaining = remaining.saturating_sub(size);
        }
        size = size.checked_shr(1).unwrap_or(0);
    }
    sizes
}

/// Colour of the decoded frame: the frame header's codes when it states any (0 and 2 are unspecified),
/// else the container `colr`, else untagged. Range is video (limited, 7.5.1 levels).
fn frame_color(h: &ProResFrameHeader, container: ColorTag) -> ColorInfo {
    let code = |v: u8| if v == 0 { 2 } else { u16::from(v) };
    let (p, t, m) = (
        code(h.color_primaries),
        code(h.transfer_characteristic),
        code(h.matrix_coefficients),
    );
    if p != 2 || t != 2 || m != 2 {
        return ColorInfo {
            primaries: p,
            transfer: t,
            matrix: m,
            full_range: false,
            origin: ColorOrigin::SequenceHeader,
        };
    }
    match container {
        ColorTag::Nclx {
            primaries,
            transfer,
            matrix,
            ..
        } => ColorInfo {
            primaries,
            transfer,
            matrix,
            full_range: false,
            origin: ColorOrigin::Container,
        },
        _ => ColorInfo {
            primaries: 2,
            transfer: 2,
            matrix: 2,
            full_range: false,
            origin: ColorOrigin::Untagged,
        },
    }
}

/// Output planes and sample conversion (7.5.1).
struct Output {
    planes: Vec<Vec<u8>>,
    dims: Vec<(u32, u32)>,
    /// 2^b as f64 and the clamp maximum.
    scale: f64,
    max: i64,
}

impl Output {
    fn new(format: &VideoFormat) -> Result<Self, ReelError> {
        let dims = format.plane_dims();
        let mut planes = Vec::with_capacity(dims.len());
        for &(w, h) in &dims {
            let bytes =
                usize::try_from(u64::from(w).saturating_mul(u64::from(h)).saturating_mul(2))
                    .map_err(|_| ReelError::LimitExceeded {
                        what: "prores-frame-bytes",
                    })?;
            planes.push(vec![0u8; bytes]);
        }
        let max = 1i64
            .checked_shl(u32::from(format.bit_depth))
            .and_then(|v| v.checked_sub(1))
            .ok_or_else(|| corrupt("prores-bit-depth"))?;
        Ok(Self {
            planes,
            dims,
            scale: f64::from(
                1u32.checked_shl(u32::from(format.bit_depth))
                    .unwrap_or(1 << 16),
            ),
            max,
        })
    }

    /// Write an 8x8 block of reconstructed values `v` at picture position (x0, picture row y0) of
    /// plane `plane`; picture row r lands on frame row `r * row_step + row_offset` (7.5.3).
    #[allow(clippy::too_many_arguments)]
    fn put(
        &mut self,
        plane: usize,
        x0: u32,
        y0: u32,
        picture_height: u32,
        row_step: u32,
        row_offset: u32,
        block: &[[f64; 8]; 8],
    ) {
        let Some(&(w, h)) = self.dims.get(plane) else {
            return;
        };
        let (scale, max) = (self.scale, self.max);
        let Some(data) = self.planes.get_mut(plane) else {
            return;
        };
        for (dy, row) in (0u32..).zip(block.iter()) {
            let Some(py) = y0.checked_add(dy).filter(|y| *y < picture_height) else {
                break;
            };
            let Some(fy) = py
                .checked_mul(row_step)
                .and_then(|y| y.checked_add(row_offset))
                .filter(|y| *y < h)
            else {
                break;
            };
            for (dx, v) in (0u32..).zip(row.iter()) {
                let Some(x) = x0.checked_add(dx).filter(|x| *x < w) else {
                    break;
                };
                let s = (scale * (*v + 256.0) / 512.0 + 0.5).floor();
                let s = if s.is_nan() || s <= 0.0 {
                    0
                } else if s >= max as f64 {
                    max
                } else {
                    s as i64
                };
                let index = u64::from(fy)
                    .saturating_mul(u64::from(w))
                    .saturating_add(u64::from(x))
                    .saturating_mul(2);
                let Ok(index) = usize::try_from(index) else {
                    return;
                };
                if let Some(cell) = data.get_mut(index..index.saturating_add(2)) {
                    cell.copy_from_slice(&(s as u16).to_le_bytes());
                }
            }
        }
    }
}

/// Per-session scratch: one slice's scanned coefficients per component.
struct Scratch {
    y: Vec<i32>,
    cb: Vec<i32>,
    cr: Vec<i32>,
}

impl Scratch {
    fn new() -> Self {
        Self {
            y: vec![0; SLICE_COEFFS],
            cb: vec![0; SLICE_COEFFS],
            cr: vec![0; SLICE_COEFFS],
        }
    }
}

/// Decode one picture (5.2) starting at `pic` of `frame` into `out`.
#[allow(clippy::too_many_arguments)]
fn decode_picture(
    frame: &[u8],
    pic: usize,
    pic_end: usize,
    head: &ProResFrameHeader,
    picture_height: u32,
    row_step: u32,
    row_offset: u32,
    out: &mut Output,
    scratch: &mut Scratch,
    cancel: &CancellationToken,
) -> Result<usize, ReelError> {
    let picture_header_size = usize::from(get(frame, pic)? >> 3);
    let picture_size = be32(
        frame,
        pic.checked_add(1)
            .ok_or_else(|| corrupt("prores-picture-size"))?,
    )?;
    let log2 = u32::from(
        (get(
            frame,
            pic.checked_add(7)
                .ok_or_else(|| corrupt("prores-picture-size"))?,
        )? >> 4)
            & 0x03,
    );
    let end = usize::try_from(picture_size)
        .ok()
        .and_then(|s| pic.checked_add(s))
        .filter(|end| *end <= pic_end)
        .ok_or_else(|| corrupt("prores-picture-size"))?;
    if picture_header_size < 8 {
        return Err(corrupt("prores-picture-size"));
    }
    let width_in_mb = u32::from(head.width).div_ceil(16);
    let height_in_mb = picture_height.div_ceil(16);
    let sizes = slice_sizes(width_in_mb, log2);
    let slices_per_row = sizes.len();
    let table_at = pic
        .checked_add(picture_header_size)
        .filter(|at| *at <= end)
        .ok_or_else(|| corrupt("prores-picture-size"))?;
    // The slice table (2 bytes per slice) must fit the picture before it is read.
    let table_len = (height_in_mb as usize)
        .checked_mul(slices_per_row)
        .and_then(|n| n.checked_mul(2))
        .ok_or(ReelError::LimitExceeded {
            what: "prores-slices",
        })?;
    let mut slice_at = table_at
        .checked_add(table_len)
        .filter(|at| *at <= end)
        .ok_or_else(|| corrupt("prores-slice-table"))?;
    let (n_c, chroma_w_mb) = if head.chroma_format == 3 {
        (4usize, 16u32)
    } else {
        (2usize, 8u32)
    };
    let scan = if head.interlace_mode == 0 {
        &SCAN_PROGRESSIVE
    } else {
        &SCAN_INTERLACED
    };
    let mut table_index = table_at;
    for i in 0..height_in_mb {
        check_cancel(cancel)?;
        let mut mb_x = 0u32;
        for &slice_mb in &sizes {
            let coded = usize::from(be16(frame, table_index)?);
            table_index = table_index.saturating_add(2);
            // Running aggregate: every slice must lie inside the picture.
            let slice_end = slice_at
                .checked_add(coded)
                .filter(|e| *e <= end)
                .ok_or_else(|| corrupt("prores-slice-size"))?;
            let slice = frame
                .get(slice_at..slice_end)
                .ok_or_else(|| corrupt("prores-slice-size"))?;
            decode_slice(
                slice,
                head,
                slice_mb,
                n_c,
                chroma_w_mb,
                mb_x,
                i,
                picture_height,
                row_step,
                row_offset,
                scan,
                out,
                scratch,
            )?;
            slice_at = slice_end;
            mb_x = mb_x.saturating_add(slice_mb);
        }
    }
    Ok(end)
}

#[allow(clippy::too_many_arguments)]
fn decode_slice(
    slice: &[u8],
    head: &ProResFrameHeader,
    slice_mb: u32,
    n_c: usize,
    chroma_w_mb: u32,
    mb_x: u32,
    mb_row: u32,
    picture_height: u32,
    row_step: u32,
    row_offset: u32,
    scan: &[[u8; 8]; 8],
    out: &mut Output,
    scratch: &mut Scratch,
) -> Result<(), ReelError> {
    let header_size = usize::from(*slice.first().ok_or_else(|| corrupt("prores-slice-size"))? >> 3);
    if header_size < 6 || header_size > slice.len() {
        return Err(corrupt("prores-slice-size"));
    }
    let q_index = get(slice, 1)?;
    if q_index == 0 || q_index > 224 {
        return Err(corrupt("prores-quantization-index"));
    }
    let q_scale: i64 = if q_index <= 128 {
        i64::from(q_index)
    } else {
        128i64.saturating_add(4i64.saturating_mul(i64::from(q_index).saturating_sub(128)))
    };
    let y_size = usize::from(be16(slice, 2)?);
    let cb_size = usize::from(be16(slice, 4)?);
    let y_end = header_size
        .checked_add(y_size)
        .filter(|e| *e <= slice.len())
        .ok_or_else(|| corrupt("prores-slice-size"))?;
    let cb_end = y_end
        .checked_add(cb_size)
        .filter(|e| *e <= slice.len())
        .ok_or_else(|| corrupt("prores-slice-size"))?;
    let y_data = slice
        .get(header_size..y_end)
        .ok_or_else(|| corrupt("prores-slice-size"))?;
    let cb_data = slice
        .get(y_end..cb_end)
        .ok_or_else(|| corrupt("prores-slice-size"))?;
    let cr_data = slice
        .get(cb_end..)
        .ok_or_else(|| corrupt("prores-slice-size"))?;
    let slice_mb_usize = slice_mb as usize;
    if slice_mb_usize == 0 || slice_mb_usize > MAX_SLICE_MB {
        return Err(corrupt("prores-slice-size"));
    }
    let n_y_blocks = slice_mb_usize.saturating_mul(4);
    let n_c_blocks = slice_mb_usize.saturating_mul(n_c);
    golomb::scanned_coefficients(y_data, n_y_blocks, &mut scratch.y)?;
    golomb::scanned_coefficients(cb_data, n_c_blocks, &mut scratch.cb)?;
    golomb::scanned_coefficients(cr_data, n_c_blocks, &mut scratch.cr)?;
    let y0 = mb_row.saturating_mul(16);
    for (plane, coeffs, n_b, weights) in [
        (0usize, &scratch.y, 4usize, &head.luma_matrix),
        (1, &scratch.cb, n_c, &head.chroma_matrix),
        (2, &scratch.cr, n_c, &head.chroma_matrix),
    ] {
        let mb_w = if plane == 0 { 16u32 } else { chroma_w_mb };
        for m in 0..slice_mb_usize {
            for b in 0..n_b {
                // Inverse slice scan (7.2.1) and block scan (7.2.2), then dequantization (7.3).
                let mut f = [[0.0f64; 8]; 8];
                for (v, row) in f.iter_mut().enumerate() {
                    for (u, cell) in row.iter_mut().enumerate() {
                        let n =
                            usize::from(scan.get(v).and_then(|r| r.get(u)).copied().unwrap_or(0));
                        let index = n_b
                            .saturating_mul(slice_mb_usize)
                            .saturating_mul(n)
                            .saturating_add(n_b.saturating_mul(m))
                            .saturating_add(b);
                        let qf = i64::from(coeffs.get(index).copied().unwrap_or(0));
                        let w =
                            i64::from(weights.get(v).and_then(|r| r.get(u)).copied().unwrap_or(0));
                        let product = qf
                            .checked_mul(w)
                            .and_then(|x| x.checked_mul(q_scale))
                            .ok_or_else(|| corrupt("prores-entropy"))?;
                        // Exact: |product| < 2^53 (12-bit range level * 63 * 512), and /8 is exact.
                        *cell = product as f64 / 8.0;
                    }
                }
                let mut v_out = [[0.0f64; 8]; 8];
                idct8x8_f64(&f, &mut v_out);
                // Block position inside the macroblock: luma Figure 6 (0 1 / 2 3); 4:2:2 chroma
                // Figure 7 (0 over 1); 4:4:4 chroma Figure 8 (0 2 / 1 3, column-major).
                const LUMA: [(u32, u32); 4] = [(0, 0), (8, 0), (0, 8), (8, 8)];
                const CHROMA_422: [(u32, u32); 2] = [(0, 0), (0, 8)];
                const CHROMA_444: [(u32, u32); 4] = [(0, 0), (0, 8), (8, 0), (8, 8)];
                let layout: &[(u32, u32)] = match (plane, n_b) {
                    (0, _) => &LUMA,
                    (_, 2) => &CHROMA_422,
                    _ => &CHROMA_444,
                };
                let (bx, by) = layout.get(b).copied().unwrap_or((0, 0));
                let x0 = mb_x
                    .saturating_add(m as u32)
                    .saturating_mul(mb_w)
                    .saturating_add(bx);
                out.put(
                    plane,
                    x0,
                    y0.saturating_add(by),
                    picture_height,
                    row_step,
                    row_offset,
                    &v_out,
                );
            }
        }
    }
    Ok(())
}

/// Decode one compressed ProRes frame (one packet).
fn decode_frame(
    frame: &[u8],
    declared: (u32, u32),
    limits: &Limits,
    container: ColorTag,
    cancel: &CancellationToken,
    scratch: &mut Scratch,
) -> Result<(VideoFormat, Vec<Vec<u8>>), ReelError> {
    let head = parse_frame_header(frame)?;
    if head.alpha_channel_type != 0 {
        return Err(unsupported("prores-alpha"));
    }
    let (width, height) = (u32::from(head.width), u32::from(head.height));
    let (dw, dh) = declared;
    let cap_w = if dw == 0 { UNDECLARED_MAX_SIDE } else { dw };
    let cap_h = if dh == 0 { UNDECLARED_MAX_SIDE } else { dh };
    if width > cap_w || height > cap_h {
        return Err(corrupt("prores-size-mismatch"));
    }
    let (chroma, bit_depth) = if head.chroma_format == 3 {
        (ChromaFormat::Yuv444, 12u8)
    } else {
        (ChromaFormat::Yuv422, 10u8)
    };
    let format = VideoFormat {
        width,
        height,
        bit_depth,
        chroma,
        color: frame_color(&head, container),
    };
    if format
        .frame_bytes()
        .is_none_or(|b| b > limits.max_chunk_bytes)
    {
        return Err(ReelError::LimitExceeded {
            what: "prores-frame-bytes",
        });
    }
    let frame_end = usize::try_from(head.frame_size).map_err(|_| corrupt("prores-frame-size"))?;
    let first_at = 8usize
        .checked_add(usize::from(head.frame_header_size))
        .filter(|at| *at < frame_end)
        .ok_or_else(|| corrupt("prores-header-size"))?;
    // Every picture needs its 8-byte header; refuse before allocating when not even that exists.
    if first_at.saturating_add(8) > frame_end {
        return Err(corrupt("prores-picture-size"));
    }
    let mut out = Output::new(&format)?;
    if head.interlace_mode == 0 {
        decode_picture(
            frame, first_at, frame_end, &head, height, 1, 0, &mut out, scratch, cancel,
        )?;
    } else {
        // 6.2: the top field has (h+1)/2 rows, the bottom field h/2; 7.5.3: top rows 2r, bottom 2r+1.
        let top = height.div_ceil(2);
        let bottom = height / 2;
        let first_is_top = head.interlace_mode == 1;
        let (h1, off1, h2, off2) = if first_is_top {
            (top, 0, bottom, 1)
        } else {
            (bottom, 1, top, 0)
        };
        let second_at = decode_picture(
            frame, first_at, frame_end, &head, h1, 2, off1, &mut out, scratch, cancel,
        )?;
        decode_picture(
            frame, second_at, frame_end, &head, h2, 2, off2, &mut out, scratch, cancel,
        )?;
    }
    Ok((format, out.planes))
}

/// Decode session of the in-process ProRes decoder: one picture per packet, released immediately.
pub(crate) struct ProResSession {
    declared: (u32, u32),
    limits: Limits,
    color: ColorTag,
    scratch: Scratch,
}

pub(crate) fn open_session(spec: &VideoStreamSpec<'_>) -> ProResSession {
    ProResSession {
        declared: (spec.width, spec.height),
        limits: spec.limits,
        color: spec.color,
        scratch: Scratch::new(),
    }
}

impl VideoDecodeSession for ProResSession {
    fn send(
        &mut self,
        packet: &[u8],
        tag: u64,
        cancel: &CancellationToken,
        out: &mut dyn PictureSink,
    ) -> Result<(), VideoDecodeError> {
        let (format, planes) = decode_frame(
            packet,
            self.declared,
            &self.limits,
            self.color,
            cancel,
            &mut self.scratch,
        )?;
        out.accept(DecodedPicture {
            tag,
            format,
            film_grain_applied: false,
            planes,
        })?;
        Ok(())
    }

    fn finish(
        &mut self,
        _cancel: &CancellationToken,
        _out: &mut dyn PictureSink,
    ) -> Result<(), VideoDecodeError> {
        Ok(())
    }
}

/// Test/diagnostic entry: decode one raw ProRes frame (no container) with the route's bounds.
pub fn decode_prores_frame(
    frame: &[u8],
    declared: (u32, u32),
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<(VideoFormat, Vec<Vec<u8>>), ReelError> {
    decode_frame(
        frame,
        declared,
        limits,
        ColorTag::Untagged,
        cancel,
        &mut Scratch::new(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_sizes_follow_6_2() {
        assert_eq!(slice_sizes(3, 3), vec![2, 1]);
        assert_eq!(slice_sizes(3, 0), vec![1, 1, 1]);
        assert_eq!(slice_sizes(120, 3), vec![8; 15]);
        assert_eq!(slice_sizes(13, 3), vec![8, 4, 1]);
    }

    #[test]
    fn scan_patterns_are_permutations() {
        for scan in [&SCAN_PROGRESSIVE, &SCAN_INTERLACED] {
            let mut seen = [false; 64];
            for v in scan.iter().flatten() {
                seen[usize::from(*v)] = true;
            }
            assert!(seen.iter().all(|s| *s));
        }
    }
}
