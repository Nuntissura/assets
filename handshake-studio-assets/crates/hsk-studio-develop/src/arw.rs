//! Sony ARW container decode: ARW2 "cRAW" (Compression 32767, one byte per pixel) and
//! uncompressed (Compression 1) raw SubIFDs. Adapted from rawkit 0.1.0 (Graphite Authors,
//! MIT OR Apache-2.0, https://github.com/GraphiteEditor/Graphite/tree/master/libraries/rawkit;
//! files `src/decoder/arw2.rs`, `src/tiff/values.rs` `CurveLookupTable::from_sony_tone_table`,
//! `src/lib.rs` dispatch; licence checked in the crate archive, sha256
//! e397e78b5ebb24f7f20579ae91404b6556747e2192713d5d68e13fb333df41fb; attribution in
//! `NOTICE`), under the lead's permissive-source decision (START_LOG 2026-10-14, R1 refinement).
//! No copyleft decoder source was read. Public scheme descriptions consistent with it: Dietz,
//! EI 2016 (https://aggregate.org/DIT/EI2016/ei2016ARW.pdf: 32-pixel spans split into two
//! interleaved 16-pixel groups; max, min, their positions, 14 x 7-bit deltas; 11-bit codes
//! expanded by a 5-segment curve whose breakpoints are in SonyToneCurve 0x7010).
//!
//! cRAW block (16 bytes, little-endian u32 header in file byte order): bits 0-10 max, 11-21 min,
//! 22-25 index of the max pixel, 26-29 index of the min pixel, then 7-bit deltas from bit 30 for
//! the other 14 pixels, each `(delta << shift) + min` capped at 0x7ff, where `shift` is the
//! smallest of 0..=3 with `0x80 << shift > max - min`, else 4. A row is `width` bytes; each
//! 32-column span holds the even columns then the odd columns. Codes go through the curve table
//! (index `code << 1`) and, as in rawkit, `>> 2 << 2` (rawkit's 12/14-bit unification).
//! Differences from rawkit: hostile-input checks instead of asserts (V4-01/02), width must be a
//! multiple of 32 (rawkit leaves a ragged tail unset), the bits-per-sample tag is not required to
//! be 12, and the white level is the curve's largest output (rawkit's 16383 clips the top of the
//! curve; the sample's top output is 17204). Black level stays unread (rawkit hard-codes 512 as a
//! TODO; SR2Private is encrypted) and is named in `unapplied`.
// V4-01: this module decodes untrusted bytes; every integer operation is checked or saturating.
#![deny(clippy::arithmetic_side_effects)]
use crate::DevelopError;
use crate::dng::{DngRaw, VendorFrame};
use crate::tiff::{Ifd, Tiff};
use crate::tiffep;

const COMPRESSION_CRAW: u32 = 32767;
const TAG_SONY_TONE_CURVE: u16 = 0x7010;

pub const UNAPPLIED: [&str; 2] = ["arw_black_level_unread", "arw_white_level_unread"];

/// IFD0 Make `SONY` and no DNGVersion (a DNG converted from an ARW stays with the DNG reader).
pub fn is_arw(bytes: &[u8]) -> bool {
    let Ok((tiff, first)) = Tiff::parse(bytes) else {
        return false;
    };
    let Ok(root) = tiff.ifd(first) else {
        return false;
    };
    root.find(tiffep::DNG_VERSION).is_none()
        && matches!(tiffep::ascii(&tiff, &root, tiffep::MAKE), Ok(Some(make)) if make.trim_end() == "SONY")
}

/// SonyToneCurve -> 4096-entry lookup: breakpoints `[0, c0>>2, c1>>2, c2>>2, c3>>2, 4095]` (each
/// `& 0xfff`), slope `2^segment` between them.
fn curve_table(points: &[u32]) -> Result<Vec<u32>, DevelopError> {
    let [a, b, c, d] = points else {
        return Err(DevelopError::InvalidContainer);
    };
    let mut knots = [0u32, (a >> 2) & 0xfff, (b >> 2) & 0xfff, (c >> 2) & 0xfff, (d >> 2) & 0xfff, 4095];
    // rawkit fills only ascending ranges; a descending knot leaves its segment empty. Same here:
    // clamp each knot to at least the previous one so the ranges stay well formed.
    for i in 1..knots.len() {
        let prev = knots.get(i.saturating_sub(1)).copied().unwrap_or(0);
        if let Some(k) = knots.get_mut(i) {
            *k = (*k).max(prev);
        }
    }
    let mut table = vec![0u32; 4096];
    let mut value = 0u32;
    for (segment, pair) in knots.windows(2).enumerate() {
        let step = 1u32.checked_shl(segment as u32).ok_or(DevelopError::Overflow)?;
        for j in pair[0].saturating_add(1)..=pair[1] {
            value = value.checked_add(step).ok_or(DevelopError::Overflow)?;
            *table.get_mut(j as usize).ok_or(DevelopError::InvalidContainer)? = value;
        }
    }
    Ok(table)
}

fn craw_rows(
    stream: &[u8],
    width: usize,
    height: usize,
    curve: &[u32],
    big_endian: bool,
) -> Result<Vec<u16>, DevelopError> {
    let total = width.checked_mul(height).ok_or(DevelopError::InvalidDimensions)?;
    if stream.len() < total {
        return Err(DevelopError::InvalidContainer);
    }
    let mut out = vec![0u16; total];
    let byte = |row: &[u8], at: usize| row.get(at).copied().unwrap_or(0);
    for (y, row) in stream.chunks_exact(width).take(height).enumerate() {
        let line = y.checked_mul(width).ok_or(DevelopError::Overflow)?;
        for (span, block_pair) in row.chunks_exact(32).enumerate() {
            let span_x = span.checked_mul(32).ok_or(DevelopError::Overflow)?;
            for (parity, block) in block_pair.chunks_exact(16).enumerate() {
                let head = [block[0], block[1], block[2], block[3]];
                let h = if big_endian { u32::from_be_bytes(head) } else { u32::from_le_bytes(head) };
                let max = h & 0x7ff;
                let min = (h >> 11) & 0x7ff;
                let imax = (h >> 22) & 0x0f;
                let imin = (h >> 26) & 0x0f;
                let span_range = max.saturating_sub(min);
                let shift = (0..4u32).find(|s| (0x80u32 << s) > span_range).unwrap_or(4);
                let mut bit = 30usize;
                for i in 0..16u32 {
                    let code = if i == imax {
                        max
                    } else if i == imin {
                        min
                    } else {
                        let at = bit >> 3;
                        let pair = if big_endian {
                            u16::from_be_bytes([byte(block, at), byte(block, at.saturating_add(1))])
                        } else {
                            u16::from_le_bytes([byte(block, at), byte(block, at.saturating_add(1))])
                        };
                        bit = bit.checked_add(7).ok_or(DevelopError::Overflow)?;
                        let delta = (u32::from(pair) >> (bit.saturating_sub(7) & 7)) & 0x7f;
                        ((delta << shift).saturating_add(min)).min(0x7ff)
                    };
                    let mapped = *curve.get((code << 1) as usize).ok_or(DevelopError::InvalidContainer)?;
                    let value = (mapped >> 2) << 2;
                    let x = span_x
                        .checked_add(parity)
                        .and_then(|v| v.checked_add((i as usize).checked_mul(2)?))
                        .ok_or(DevelopError::Overflow)?;
                    let at = line.checked_add(x).ok_or(DevelopError::Overflow)?;
                    *out.get_mut(at).ok_or(DevelopError::InvalidContainer)? =
                        u16::try_from(value).map_err(|_| DevelopError::InvalidContainer)?;
                }
            }
        }
    }
    Ok(out)
}

pub fn decode(bytes: &[u8]) -> Result<DngRaw, DevelopError> {
    let (tiff, first) = Tiff::parse(bytes)?;
    let ifds = tiff.walk(first)?;
    let root = ifds.first().ok_or(DevelopError::InvalidContainer)?;
    let raw: &Ifd = tiffep::cfa_ifd(&tiff, &ifds)?.ok_or(DevelopError::UnsupportedContainer)?;
    let (width, height, bits, compression) = tiffep::geometry(&tiff, raw)?;
    let cfa_pattern = tiffep::cfa_pattern(&tiff, raw)?;
    let model = tiffep::ascii(&tiff, root, tiffep::MODEL)?;
    let (samples, white_level) = match compression {
        1 => {
            let samples = tiffep::uncompressed_strips(&tiff, raw, width, height, bits, bytes.len())?;
            (samples, VendorFrame::full_scale(bits).ok_or(DevelopError::UnsupportedContainer)?)
        }
        COMPRESSION_CRAW => {
            let one = |tag: u16| -> Result<u32, DevelopError> {
                match tiff.u32s(raw.find(tag).ok_or(DevelopError::InvalidContainer)?)?.as_slice() {
                    [v] => Ok(*v),
                    _ => Err(DevelopError::UnsupportedContainer),
                }
            };
            let pixels = u64::from(width).saturating_mul(u64::from(height));
            // cRAW stores exactly one byte per pixel (rawkit's ARW2 test); anything else is ARW1
            // or an unknown variant. Width must hold whole 32-column spans.
            if u64::from(one(tiffep::STRIP_BYTE_COUNTS)?) != pixels || !width.is_multiple_of(32) {
                return Err(DevelopError::UnsupportedCompression);
            }
            let stream = tiff.region(u64::from(one(tiffep::STRIP_OFFSETS)?), pixels)?;
            let curve = curve_table(&tiff.u32s(raw.find(TAG_SONY_TONE_CURVE).ok_or(DevelopError::InvalidContainer)?)?)?;
            let samples = craw_rows(stream, width as usize, height as usize, &curve, tiff.is_big_endian())?;
            // White = the curve's largest reachable output (code 0x7ff -> index 4094).
            let white = curve.get(4094).map(|v| (v >> 2) << 2).ok_or(DevelopError::InvalidContainer)?;
            (samples, white)
        }
        _ => return Err(DevelopError::UnsupportedCompression),
    };
    DngRaw::from_vendor(
        VendorFrame {
            width,
            height,
            bits,
            cfa_pattern,
            unique_camera_model: model,
            unapplied: UNAPPLIED.to_vec(),
            samples,
            white_level,
        },
        bytes,
    )
}
