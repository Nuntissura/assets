//! SVGZ container (RFC 1952 gzip) over a bounded raw-deflate decoder. The decompressed size is
//! capped while inflating, so a decompression bomb costs at most the cap in memory; the member
//! is never unwrapped twice and the CRC-32 and size trailer are verified.
use crate::error::{Result, VectorError};
use miniz_oxide::deflate::compress_to_vec;
use miniz_oxide::inflate::{TINFLStatus, decompress_to_vec_with_limit};

const MAGIC: [u8; 2] = [0x1f, 0x8b];
const FHCRC: u8 = 0x02;
const FEXTRA: u8 = 0x04;
const FNAME: u8 = 0x08;
const FCOMMENT: u8 = 0x10;
const RESERVED: u8 = 0xe0;

const fn make_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

static CRC_TABLE: [u32; 256] = make_table();

fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c = CRC_TABLE[((c ^ u32::from(b)) & 0xff) as usize] ^ (c >> 8);
    }
    !c
}

pub(crate) fn is_gzip(bytes: &[u8]) -> bool {
    bytes.starts_with(&MAGIC)
}

fn skip_zero_terminated(input: &[u8], pos: usize) -> Result<usize> {
    let rest = input.get(pos..).ok_or(VectorError::Gzip("truncated"))?;
    let n = rest
        .iter()
        .position(|&b| b == 0)
        .ok_or(VectorError::Gzip("truncated"))?;
    Ok(pos + n + 1)
}

/// Inflates one gzip member to at most `max_out` bytes.
pub(crate) fn gunzip(input: &[u8], max_out: usize) -> Result<Vec<u8>> {
    if input.len() < 18 {
        return Err(VectorError::Gzip("truncated"));
    }
    if input[2] != 8 {
        return Err(VectorError::Gzip("method"));
    }
    let flags = input[3];
    if flags & RESERVED != 0 {
        return Err(VectorError::Gzip("flags"));
    }
    let mut pos = 10usize;
    if flags & FEXTRA != 0 {
        let len = input
            .get(pos..pos + 2)
            .map(|b| usize::from(u16::from_le_bytes([b[0], b[1]])))
            .ok_or(VectorError::Gzip("truncated"))?;
        pos += 2 + len;
    }
    if flags & FNAME != 0 {
        pos = skip_zero_terminated(input, pos)?;
    }
    if flags & FCOMMENT != 0 {
        pos = skip_zero_terminated(input, pos)?;
    }
    if flags & FHCRC != 0 {
        pos += 2;
    }
    let end = input.len() - 8;
    if pos >= end {
        return Err(VectorError::Gzip("truncated"));
    }
    let out = decompress_to_vec_with_limit(&input[pos..end], max_out).map_err(|e| {
        if e.status == TINFLStatus::HasMoreOutput {
            VectorError::BudgetExceeded("decompressed_bytes")
        } else {
            VectorError::Gzip("deflate")
        }
    })?;
    let trailer = &input[end..];
    let crc = u32::from_le_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
    let size = u32::from_le_bytes([trailer[4], trailer[5], trailer[6], trailer[7]]);
    if crc != crc32(&out) || size != (out.len() as u64 & 0xffff_ffff) as u32 {
        return Err(VectorError::Gzip("checksum"));
    }
    Ok(out)
}

/// Writes SVG text as SVGZ: a deterministic gzip member (no name, zero mtime, level 6).
pub fn gzip_svg(text: &str) -> Vec<u8> {
    let data = text.as_bytes();
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255];
    out.extend_from_slice(&compress_to_vec(data, 6));
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&((data.len() as u64 & 0xffff_ffff) as u32).to_le_bytes());
    out
}
