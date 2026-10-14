//! The four PSD/PSB channel compressions: 0 raw, 1 PackBits RLE, 2 ZIP (zlib), 3 ZIP with
//! prediction. Output is always raw big-endian planar samples, `rows * row_bytes` per plane.
//!
//! Evidence status: raw and RLE follow the public specification text. ZIP being a zlib stream
//! and the prediction transforms (8-bit byte delta, 16-bit big-endian word delta, 32-bit
//! byte-plane shuffle plus byte delta) are NOT stated in the specification; they are
//! implemented from behaviour of independent readers and stay UNVERIFIED for 16/32-bit until a
//! Photoshop-authored oracle file is compared.
#![deny(clippy::arithmetic_side_effects)]

use hsk_studio_accord::CancellationToken;

use crate::error::{PsdError, Result};
use crate::limits::Limits;

pub const COMPRESSION_RAW: u16 = 0;
pub const COMPRESSION_RLE: u16 = 1;
pub const COMPRESSION_ZIP: u16 = 2;
pub const COMPRESSION_ZIP_PREDICTION: u16 = 3;

/// Bytes of one scanline: `ceil(width * depth / 8)`.
pub fn row_bytes(width: u32, depth: u16) -> Result<usize> {
    let bits = u64::from(width).saturating_mul(u64::from(depth));
    usize::try_from(bits.div_ceil(8)).map_err(|_| PsdError::Overflow)
}

/// Geometry of the decoded region: `planes` planes of `height` rows each.
#[derive(Clone, Copy, Debug)]
pub struct PlaneGeometry {
    pub width: u32,
    pub height: u32,
    pub depth: u16,
    pub planes: usize,
    pub psb: bool,
}

impl PlaneGeometry {
    pub(crate) fn plane_bytes(&self) -> Result<usize> {
        row_bytes(self.width, self.depth)?
            .checked_mul(self.height as usize)
            .ok_or(PsdError::Overflow)
    }

    pub(crate) fn total_bytes(&self, limits: &Limits) -> Result<usize> {
        let total = self.plane_bytes()?.checked_mul(self.planes).ok_or(PsdError::Overflow)?;
        if total > limits.max_decoded_bytes {
            return Err(PsdError::PixelLimit);
        }
        Ok(total)
    }
}

pub fn decode(
    compression: u16,
    data: &[u8],
    geometry: PlaneGeometry,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<Vec<u8>> {
    decode_ordered(compression, data, geometry, false, limits, cancel)
}

/// As [`decode`]; `little` reads the RLE row-count table little-endian (TIFF ImageSourceData).
pub(crate) fn decode_ordered(
    compression: u16,
    data: &[u8],
    geometry: PlaneGeometry,
    little: bool,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<Vec<u8>> {
    cancel.check()?;
    let total = geometry.total_bytes(limits)?;
    match compression {
        COMPRESSION_RAW => {
            if data.len() < total {
                return Err(PsdError::ChannelSize);
            }
            Ok(data[..total].to_vec())
        }
        COMPRESSION_RLE => decode_rle(data, geometry, total, little, cancel),
        COMPRESSION_ZIP | COMPRESSION_ZIP_PREDICTION => {
            let out = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(data, total)
                .map_err(|_| PsdError::ZipFailed)?;
            if out.len() != total {
                return Err(PsdError::ChannelSize);
            }
            let mut out = out;
            if compression == COMPRESSION_ZIP_PREDICTION {
                undo_prediction(&mut out, geometry)?;
            }
            Ok(out)
        }
        other => Err(PsdError::BadCompression(other)),
    }
}

fn decode_rle(
    data: &[u8],
    geometry: PlaneGeometry,
    total: usize,
    little: bool,
    cancel: &CancellationToken,
) -> Result<Vec<u8>> {
    let row_len = row_bytes(geometry.width, geometry.depth)?;
    let rows = (geometry.height as usize).checked_mul(geometry.planes).ok_or(PsdError::Overflow)?;
    let count_width = if geometry.psb { 4 } else { 2 };
    let table = rows.checked_mul(count_width).ok_or(PsdError::Overflow)?;
    if data.len() < table {
        return Err(PsdError::RleRowCounts);
    }
    let (counts, mut body) = data.split_at(table);
    // PackBits expands at most 64:1, so the table's own size bounds the useful capacity.
    let mut out = Vec::with_capacity(total.min(data.len().saturating_mul(64)));
    for (row, chunk) in counts.chunks_exact(count_width).enumerate() {
        if row % 256 == 0 {
            cancel.check()?;
        }
        let count = if geometry.psb {
            let bytes = [chunk[0], chunk[1], chunk[2], chunk[3]];
            u64::from(if little { u32::from_le_bytes(bytes) } else { u32::from_be_bytes(bytes) })
        } else {
            let bytes = [chunk[0], chunk[1]];
            u64::from(if little { u16::from_le_bytes(bytes) } else { u16::from_be_bytes(bytes) })
        };
        let count = usize::try_from(count).map_err(|_| PsdError::Overflow)?;
        if count > body.len() {
            return Err(PsdError::Truncated("rle_row"));
        }
        let (line, tail) = body.split_at(count);
        unpack_row(line, row_len, &mut out)?;
        body = tail;
    }
    Ok(out)
}

/// PackBits: header byte `n`; 0..=127 copies `n + 1` literals; 129..=255 repeats the next byte
/// `257 - n` times; 128 is a no-op. Must produce exactly `want` bytes.
fn unpack_row(src: &[u8], want: usize, out: &mut Vec<u8>) -> Result<()> {
    let start = out.len();
    let produced = |out: &Vec<u8>| out.len().saturating_sub(start);
    let mut i = 0usize;
    while i < src.len() && produced(out) < want {
        let header = src[i];
        i = i.saturating_add(1);
        if header < 128 {
            let n = usize::from(header).saturating_add(1);
            let literal = i.checked_add(n).and_then(|end| src.get(i..end)).ok_or(PsdError::RleShort)?;
            if produced(out).saturating_add(n) > want {
                return Err(PsdError::RleOverflow);
            }
            out.extend_from_slice(literal);
            i = i.saturating_add(n);
        } else if header > 128 {
            let n = 257usize.saturating_sub(usize::from(header));
            let value = *src.get(i).ok_or(PsdError::RleShort)?;
            if produced(out).saturating_add(n) > want {
                return Err(PsdError::RleOverflow);
            }
            out.resize(out.len().saturating_add(n), value);
            i = i.saturating_add(1);
        }
    }
    if produced(out) != want {
        return Err(PsdError::RleShort);
    }
    Ok(())
}

fn undo_prediction(data: &mut [u8], geometry: PlaneGeometry) -> Result<()> {
    let row_len = row_bytes(geometry.width, geometry.depth)?;
    if row_len == 0 {
        return Ok(());
    }
    let width = geometry.width as usize;
    match geometry.depth {
        8 => {
            for row in data.chunks_exact_mut(row_len) {
                let mut prev = 0u8;
                for (x, byte) in row.iter_mut().enumerate() {
                    if x > 0 {
                        *byte = byte.wrapping_add(prev);
                    }
                    prev = *byte;
                }
            }
        }
        16 => {
            for row in data.chunks_exact_mut(row_len) {
                let mut prev = 0u16;
                for (x, pair) in row.chunks_exact_mut(2).take(width).enumerate() {
                    let cur = u16::from_be_bytes([pair[0], pair[1]]);
                    let value = if x > 0 { cur.wrapping_add(prev) } else { cur };
                    pair.copy_from_slice(&value.to_be_bytes());
                    prev = value;
                }
            }
        }
        32 => {
            let mut scratch = vec![0u8; row_len];
            for row in data.chunks_exact_mut(row_len) {
                let mut prev = 0u8;
                for (i, byte) in row.iter_mut().enumerate() {
                    if i > 0 {
                        *byte = byte.wrapping_add(prev);
                    }
                    prev = *byte;
                }
                // Byte planes (all first bytes, then all second bytes, ...) back to interleaved samples.
                for (k, plane) in row.chunks_exact(width).enumerate() {
                    for (x, &byte) in plane.iter().enumerate() {
                        let at = x.checked_mul(4).and_then(|at| at.checked_add(k));
                        if let Some(slot) = at.and_then(|at| scratch.get_mut(at)) {
                            *slot = byte;
                        }
                    }
                }
                row.copy_from_slice(&scratch);
            }
        }
        _ => return Err(PsdError::UnsupportedMode("zip_prediction_depth")),
    }
    Ok(())
}

/// PackBits encoder used by the writer and by fixtures; produces one scanline.
pub fn pack_row(row: &[u8], out: &mut Vec<u8>) {
    let mut i = 0usize;
    while i < row.len() {
        let mut run = 1usize;
        while run < 128 && row.get(i.saturating_add(run)).is_some_and(|&b| b == row[i]) {
            run = run.saturating_add(1);
        }
        if run >= 2 {
            // 129..=255: header = 257 - run for run in 2..=128.
            out.push(u8::try_from(257usize.saturating_sub(run)).unwrap_or(129));
            out.push(row[i]);
            i = i.saturating_add(run);
        } else {
            let start = i;
            let mut n = 0usize;
            while i < row.len() && n < 128 {
                if row.get(i.saturating_add(1)).is_some_and(|&b| b == row[i]) {
                    break;
                }
                i = i.saturating_add(1);
                n = n.saturating_add(1);
            }
            out.push(u8::try_from(n.saturating_sub(1)).unwrap_or(127));
            out.extend_from_slice(&row[start..i]);
        }
    }
}

/// Encodes `planes` planes of raw samples as RLE (counts table, then rows).
pub fn encode_rle(raw: &[u8], geometry: PlaneGeometry) -> Result<Vec<u8>> {
    let row_len = row_bytes(geometry.width, geometry.depth)?;
    let rows = (geometry.height as usize).checked_mul(geometry.planes).ok_or(PsdError::Overflow)?;
    if row_len == 0 || Some(raw.len()) != rows.checked_mul(row_len) {
        return Err(PsdError::Encode("rle_input_size"));
    }
    let mut counts = Vec::with_capacity(rows.saturating_mul(4).min(raw.len()));
    let mut body = Vec::new();
    for row in raw.chunks_exact(row_len) {
        let before = body.len();
        pack_row(row, &mut body);
        let n = body.len().saturating_sub(before);
        if geometry.psb {
            counts.extend_from_slice(&u32::try_from(n).map_err(|_| PsdError::Overflow)?.to_be_bytes());
        } else {
            counts.extend_from_slice(&u16::try_from(n).map_err(|_| PsdError::Overflow)?.to_be_bytes());
        }
    }
    counts.extend_from_slice(&body);
    Ok(counts)
}
