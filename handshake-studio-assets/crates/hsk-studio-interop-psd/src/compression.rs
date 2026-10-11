//! The four PSD/PSB channel compressions: 0 raw, 1 PackBits RLE, 2 ZIP (zlib), 3 ZIP with
//! prediction. Output is always raw big-endian planar samples, `rows * row_bytes` per plane.
//!
//! Evidence status: raw and RLE follow the public specification text. ZIP being a zlib stream
//! and the prediction transforms (8-bit byte delta, 16-bit big-endian word delta, 32-bit
//! byte-plane shuffle plus byte delta) are NOT stated in the specification; they are
//! implemented from behaviour of independent readers and stay UNVERIFIED for 16/32-bit until a
//! Photoshop-authored oracle file is compared.

use hsk_studio_accord::CancellationToken;

use crate::error::{PsdError, Result};
use crate::limits::Limits;

pub const COMPRESSION_RAW: u16 = 0;
pub const COMPRESSION_RLE: u16 = 1;
pub const COMPRESSION_ZIP: u16 = 2;
pub const COMPRESSION_ZIP_PREDICTION: u16 = 3;

/// Bytes of one scanline: `ceil(width * depth / 8)`.
pub fn row_bytes(width: u32, depth: u16) -> Result<usize> {
    let bits = u64::from(width) * u64::from(depth);
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
    cancel.check()?;
    let total = geometry.total_bytes(limits)?;
    match compression {
        COMPRESSION_RAW => {
            if data.len() < total {
                return Err(PsdError::ChannelSize);
            }
            Ok(data[..total].to_vec())
        }
        COMPRESSION_RLE => decode_rle(data, geometry, total, cancel),
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

fn decode_rle(data: &[u8], geometry: PlaneGeometry, total: usize, cancel: &CancellationToken) -> Result<Vec<u8>> {
    let row_len = row_bytes(geometry.width, geometry.depth)?;
    let rows = (geometry.height as usize).checked_mul(geometry.planes).ok_or(PsdError::Overflow)?;
    let count_width = if geometry.psb { 4 } else { 2 };
    let table = rows.checked_mul(count_width).ok_or(PsdError::Overflow)?;
    if data.len() < table {
        return Err(PsdError::RleRowCounts);
    }
    let (counts, mut body) = data.split_at(table);
    let mut out = Vec::with_capacity(total);
    for (row, chunk) in counts.chunks_exact(count_width).enumerate() {
        if row % 256 == 0 {
            cancel.check()?;
        }
        let count = if geometry.psb {
            u64::from(u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        } else {
            u64::from(u16::from_be_bytes([chunk[0], chunk[1]]))
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
    let mut i = 0;
    while i < src.len() && out.len() - start < want {
        let header = src[i];
        i += 1;
        if header < 128 {
            let n = usize::from(header) + 1;
            let literal = src.get(i..i + n).ok_or(PsdError::RleShort)?;
            if out.len() - start + n > want {
                return Err(PsdError::RleOverflow);
            }
            out.extend_from_slice(literal);
            i += n;
        } else if header > 128 {
            let n = 257 - usize::from(header);
            let value = *src.get(i).ok_or(PsdError::RleShort)?;
            if out.len() - start + n > want {
                return Err(PsdError::RleOverflow);
            }
            out.resize(out.len() + n, value);
            i += 1;
        }
    }
    if out.len() - start != want {
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
                for x in 1..row.len() {
                    row[x] = row[x].wrapping_add(row[x - 1]);
                }
            }
        }
        16 => {
            for row in data.chunks_exact_mut(row_len) {
                for x in 1..width {
                    let prev = u16::from_be_bytes([row[(x - 1) * 2], row[(x - 1) * 2 + 1]]);
                    let cur = u16::from_be_bytes([row[x * 2], row[x * 2 + 1]]);
                    let sum = cur.wrapping_add(prev).to_be_bytes();
                    row[x * 2] = sum[0];
                    row[x * 2 + 1] = sum[1];
                }
            }
        }
        32 => {
            let mut scratch = vec![0u8; row_len];
            for row in data.chunks_exact_mut(row_len) {
                for i in 1..row.len() {
                    row[i] = row[i].wrapping_add(row[i - 1]);
                }
                for x in 0..width {
                    for k in 0..4 {
                        scratch[x * 4 + k] = row[k * width + x];
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
    let mut i = 0;
    while i < row.len() {
        let mut run = 1;
        while i + run < row.len() && run < 128 && row[i + run] == row[i] {
            run += 1;
        }
        if run >= 2 {
            out.push((257 - run) as u8);
            out.push(row[i]);
            i += run;
        } else {
            let start = i;
            let mut n = 0;
            while i < row.len() && n < 128 {
                if i + 1 < row.len() && row[i] == row[i + 1] {
                    break;
                }
                i += 1;
                n += 1;
            }
            out.push((n - 1) as u8);
            out.extend_from_slice(&row[start..start + n]);
        }
    }
}

/// Encodes `planes` planes of raw samples as RLE (counts table, then rows).
pub fn encode_rle(raw: &[u8], geometry: PlaneGeometry) -> Result<Vec<u8>> {
    let row_len = row_bytes(geometry.width, geometry.depth)?;
    let rows = (geometry.height as usize).checked_mul(geometry.planes).ok_or(PsdError::Overflow)?;
    if row_len == 0 || raw.len() != rows * row_len {
        return Err(PsdError::Encode("rle_input_size"));
    }
    let mut counts = Vec::with_capacity(rows * 4);
    let mut body = Vec::new();
    for row in raw.chunks_exact(row_len) {
        let before = body.len();
        pack_row(row, &mut body);
        let n = body.len() - before;
        if geometry.psb {
            counts.extend_from_slice(&u32::try_from(n).map_err(|_| PsdError::Overflow)?.to_be_bytes());
        } else {
            counts.extend_from_slice(&u16::try_from(n).map_err(|_| PsdError::Overflow)?.to_be_bytes());
        }
    }
    counts.extend_from_slice(&body);
    Ok(counts)
}
