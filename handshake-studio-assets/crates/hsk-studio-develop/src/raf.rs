//! Fujifilm RAF container decode, compressed variants (lossless and lossy; Bayer and X-Trans).
//!
//! Adapted from LightCraft `crates/raw/src/vendor/raf.rs` (https://github.com/storytold/lightcraft at
//! af4c5e102c9f97529f582003b99c21b89067e906; MIT OR Apache-2.0; attribution in this crate's `NOTICE`),
//! whose sources are the ExifTool FujiFilm tag-name documentation and black-box analysis of CC0
//! raw.pixls.us samples. No copyleft decoder source was read for this module.
//!
//! Layout: 16-byte magic `FUJIFILMCCD-RAW `, camera name at bytes 28..60; at byte 84 six big-endian
//! u32 give the embedded JPEG (offset, length), the header-record directory (offset, length: a u32
//! count then `(u16 tag, u16 size, data)` records) and the raw block (offset, length); the u32 at
//! byte 108 is the compression mode (0 uncompressed, 2 lossless, 3 lossy). In current bodies the
//! raw block is a little-endian TIFF whose IFD0 tag 0xF000 points to the raw IFD (offsets relative
//! to the block): 0xF001 width, 0xF002 height, 0xF003 bits, 0xF007 strip offset, 0xF008 strip
//! bytes. Header record 0x0131 (X-Trans layout, 36 bytes, stored reversed) gives the X-Trans CFA
//! anchored at (0, 0); without it the CFA is RGGB (X-A series, LightCraft colour renders).
//! Compressed X-Trans is decoded only for the fixed X-Trans arrangement the codec uses.
//!
//! Uncompressed RAF (mode 0, LightCraft's measured packings): 16-bit little-endian words, 12-bit
//! LSB-first stream, or 32-bit little-endian words read MSB-first (raw IFD 0xF006 set), row stride
//! 0xF005 in 32-bit words. Levels: black 0xF00A per CFA site, white measured (see `levels`).
//! Typed refusals: older FinePix layouts without the raw IFD such as the SuperCCD S5000
//! (`decoder_unavailable:raf`), unknown compression modes (`unsupported_compression`), container
//! and stream modes that disagree (`invalid_container`). Not read (named in `unapplied`): the crop
//! records 0x0110/0x0111 as an active area (they bound the white measurement only).
// V4-01: this module decodes untrusted bytes; every integer operation is checked or saturating.
#![deny(clippy::arithmetic_side_effects)]
use crate::DevelopError;
use crate::dng::{DngRaw, MAX_DNG_PIXELS, VendorFrame};
use crate::pipeline::{Bayer, CfaPattern, XTRANS};
use crate::levels;
use crate::rafc;
use crate::tiffep::{self, Tiff};

pub const UNAPPLIED: [&str; 3] = ["raf_black_level_unread", "raf_white_level_unread", "raf_crop_unread"];

const XTRANS_LAYOUT: u16 = 0x0131;
const MAX_RECORDS: usize = 4096;

pub fn is_raf(bytes: &[u8]) -> bool {
    bytes.starts_with(b"FUJIFILMCCD-RAW")
}

fn be32(b: &[u8], at: usize) -> Option<usize> {
    let s: [u8; 4] = b.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_be_bytes(s) as usize)
}

/// `bytes[offset..offset + len]` when both are non-zero and inside the file.
fn region(b: &[u8], offset: Option<usize>, len: Option<usize>) -> Option<&[u8]> {
    let (o, l) = (offset?, len?);
    if o == 0 || l == 0 {
        return None;
    }
    b.get(o..o.checked_add(l)?)
}

/// The header record `tag` from the record directory.
fn record(b: &[u8], tag: u16) -> Option<&[u8]> {
    let dir = region(b, be32(b, 92), be32(b, 96))?;
    let n = be32(dir, 0)?.min(MAX_RECORDS);
    let mut p = 4usize;
    for _ in 0..n {
        let h = dir.get(p..p.checked_add(4)?)?;
        let (t, size) = (u16::from_be_bytes([*h.first()?, *h.get(1)?]), usize::from(u16::from_be_bytes([*h.get(2)?, *h.get(3)?])));
        let start = p.checked_add(4)?;
        let data = dir.get(start..start.checked_add(size)?)?;
        if t == tag {
            return Some(data);
        }
        p = start.checked_add(size)?;
    }
    None
}

fn cfa(b: &[u8]) -> Result<CfaPattern, DevelopError> {
    match record(b, XTRANS_LAYOUT) {
        Some(l) if l.len() == 36 => {
            // Stored reversed; reversed it is anchored at raw (0, 0).
            let matches = l.iter().rev().enumerate().all(|(i, &c)| XTRANS.get(i / 6).and_then(|r| r.get(i % 6)) == Some(&c));
            if matches { Ok(CfaPattern::XTrans) } else { Err(DevelopError::UnsupportedPattern) }
        }
        Some(_) => Err(DevelopError::InvalidContainer),
        None => Ok(CfaPattern::Bayer(Bayer::Rggb)),
    }
}

fn camera_name(b: &[u8]) -> Option<String> {
    let name = b.get(28..60)?;
    let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    let s = String::from_utf8_lossy(name.get(..end)?).trim().to_string();
    (!s.is_empty()).then_some(s)
}

pub fn decode(bytes: &[u8]) -> Result<DngRaw, DevelopError> {
    if !is_raf(bytes) || bytes.len() < 112 {
        return Err(DevelopError::InvalidContainer);
    }
    let raw = region(bytes, be32(bytes, 100), be32(bytes, 104)).ok_or(DevelopError::InvalidContainer)?;
    // Older FinePix layouts (e.g. SuperCCD) carry no TIFF raw block.
    let Ok((tiff, first)) = Tiff::parse(raw) else {
        return Err(DevelopError::DecoderUnavailable("raf"));
    };
    let root = tiff.ifd(first)?;
    let raw_ifd = tiff.ifd(tiffep::first_u32(&tiff, root.find(0xf000).ok_or(DevelopError::DecoderUnavailable("raf"))?)?)?;
    let get = |tag: u16| -> Result<u32, DevelopError> { tiffep::first_u32(&tiff, raw_ifd.find(tag).ok_or(DevelopError::InvalidContainer)?) };
    let (width, height, bits) = (get(0xf001)?, get(0xf002)?, get(0xf003)?);
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if width == 0 || height == 0 || width > 65_535 || height > 65_535 || pixels > MAX_DNG_PIXELS {
        return Err(DevelopError::InvalidDimensions);
    }
    let bits16 = u16::try_from(bits).ok().filter(|b| (8..=16).contains(b)).ok_or(DevelopError::UnsupportedContainer)?;
    let src = tiff.region(u64::from(get(0xf007)?), u64::from(get(0xf008)?))?;
    let cfa_pattern = cfa(bytes)?;
    let samples = match be32(bytes, 108) {
        Some(0) => {
            let words32 = match raw_ifd.find(0xf006) {
                Some(e) => tiffep::first_u32(&tiff, e)? != 0,
                None => false,
            };
            let stride_words = match raw_ifd.find(0xf005) {
                Some(e) => tiffep::first_u32(&tiff, e)?,
                None => 0,
            };
            uncompressed(src, width, height, bits, words32, stride_words)?
        }
        Some(mode @ (2 | 3)) => {
            // The stream's own flag (byte 2: 1 lossless, 0 lossy) must agree with the container.
            if src.get(2).copied() != Some(u8::from(mode == 2)) {
                return Err(DevelopError::InvalidContainer);
            }
            rafc::decode(src, width as usize, height as usize, bits, cfa_pattern == CfaPattern::XTrans)?
        }
        _ => return Err(DevelopError::UnsupportedCompression),
    };
    // Levels: black = raw IFD 0xF00A, one value per CFA site (36 for X-Trans, 4 for Bayer, anchored
    // at the frame origin as the CFA is) or one value for all; white = what the crop area proves
    // (saturation plateau, or >= 16 samples at full scale), else full scale with the white unread.
    let full = VendorFrame::full_scale(bits16).ok_or(DevelopError::UnsupportedContainer)?;
    let period: u32 = if cfa_pattern == CfaPattern::XTrans { 6 } else { 2 };
    let mut applied = Vec::new();
    let tagged = match raw_ifd.find(0xf00a) {
        Some(e) => tiff.u32s(e)?,
        None => Vec::new(),
    };
    let (black_repeat, black_level) = match tagged.len() {
        n if n == period.saturating_mul(period) as usize => ((period, period), tagged.iter().map(|v| f64::from(*v)).collect()),
        n if n >= 1 && tagged.iter().all(|v| Some(v) == tagged.first()) => ((1, 1), vec![f64::from(tagged[0])]),
        _ => ((1, 1), vec![0.0]),
    };
    if !tagged.is_empty() && black_level.iter().any(|b| *b > 0.0) {
        applied.push("raf_black_level_unread");
    }
    let region = crop(bytes, width, height).unwrap_or(levels::Region::whole(width, height));
    let white = levels::measured_white(&samples, width, height, region, full);
    if white.is_some() {
        applied.push("raf_white_level_unread");
    }
    DngRaw::from_vendor(
        VendorFrame {
            width,
            height,
            bits: bits16,
            cfa_pattern,
            unique_camera_model: camera_name(bytes),
            unapplied: levels::remaining(&UNAPPLIED, &applied),
            samples,
            white_level: white.unwrap_or(full),
            black_repeat,
            black_level,
        },
        bytes,
    )
}

/// Header records 0x0110 (crop top, left) and 0x0111 (cropped height, width) as a frame region.
pub(crate) fn crop(bytes: &[u8], width: u32, height: u32) -> Option<levels::Region> {
    let pair = |tag: u16| -> Option<(u32, u32)> {
        let d = record(bytes, tag)?;
        Some((u32::from(u16::from_be_bytes([*d.first()?, *d.get(1)?])), u32::from(u16::from_be_bytes([*d.get(2)?, *d.get(3)?]))))
    };
    let ((top, left), (h, w)) = (pair(0x0110)?, pair(0x0111)?);
    let region = levels::Region { x0: left, y0: top, x1: left.checked_add(w)?, y1: top.checked_add(h)? };
    (w > 0 && h > 0 && region.x1 <= width && region.y1 <= height).then_some(region)
}

/// Uncompressed RAF strip (LightCraft's measured packings): 16-bit little-endian words when the
/// strip holds two bytes per sample; otherwise samples packed at `bits`: a 12-bit LSB-first stream
/// unless raw IFD 0xF006 is set, else (and at other depths) little-endian 32-bit words read
/// MSB-first. 0xF005, when non-zero, is the row stride in 32-bit words. Every row must lie inside
/// the strip before the frame is allocated (V4-02).
fn uncompressed(src: &[u8], width: u32, height: u32, bits: u32, words32: bool, stride_words: u32) -> Result<Vec<u16>, DevelopError> {
    let (w, h) = (width as usize, height as usize);
    let n = w.checked_mul(h).ok_or(DevelopError::Overflow)?;
    let store = if src.len() >= n.checked_mul(2).ok_or(DevelopError::Overflow)? { 16 } else { bits };
    let packed_row = w.checked_mul(store as usize).ok_or(DevelopError::Overflow)?.div_ceil(8);
    let stride = match stride_words {
        0 => packed_row,
        words => (words as usize).checked_mul(4).ok_or(DevelopError::Overflow)?.max(packed_row),
    };
    let required = stride
        .checked_mul(h.checked_sub(1).ok_or(DevelopError::InvalidDimensions)?)
        .and_then(|v| v.checked_add(packed_row))
        .ok_or(DevelopError::Overflow)?;
    if src.len() < required {
        return Err(DevelopError::InvalidContainer);
    }
    let mut out = vec![0u16; n];
    for (y, row) in out.chunks_exact_mut(w).enumerate() {
        let at = y.checked_mul(stride).ok_or(DevelopError::Overflow)?;
        let s = src.get(at..at.checked_add(packed_row).ok_or(DevelopError::Overflow)?).ok_or(DevelopError::InvalidContainer)?;
        match store {
            16 => row.iter_mut().zip(s.chunks_exact(2)).for_each(|(o, c)| *o = u16::from_le_bytes([c[0], c[1]])),
            12 if !words32 => unpack_lsb(s, 12, row),
            _ => unpack_words32_msb(s, store, row),
        }
    }
    Ok(out)
}

/// `out.len()` LSB-first packed samples of `bits` from `src` (missing bytes read as 0).
fn unpack_lsb(src: &[u8], bits: u32, out: &mut [u16]) {
    let (mut acc, mut n) = (0u64, 0u32);
    let mut it = src.iter();
    let mask = 1u64.checked_shl(bits).unwrap_or(0).wrapping_sub(1);
    for o in out.iter_mut() {
        while n < bits {
            acc |= u64::from(*it.next().unwrap_or(&0)) << n;
            n = n.saturating_add(8);
        }
        *o = u16::try_from(acc & mask).unwrap_or(0);
        acc >>= bits;
        n = n.saturating_sub(bits);
    }
}

/// Samples of `bits` taken MSB-first from little-endian 32-bit words (a short last word is
/// zero-padded).
fn unpack_words32_msb(src: &[u8], bits: u32, out: &mut [u16]) {
    let (mut acc, mut n) = (0u64, 0u32);
    let mut words = src.chunks(4).map(|c| {
        let mut w = [0u8; 4];
        w.iter_mut().zip(c).for_each(|(d, s)| *d = *s);
        u32::from_le_bytes(w)
    });
    let mask = 1u64.checked_shl(bits).unwrap_or(0).wrapping_sub(1);
    for o in out.iter_mut() {
        while n < bits {
            acc = (acc << 32) | u64::from(words.next().unwrap_or(0));
            n = n.saturating_add(32);
        }
        n = n.saturating_sub(bits);
        *o = u16::try_from((acc >> n) & mask).unwrap_or(0);
        acc &= 1u64.checked_shl(n).unwrap_or(0).wrapping_sub(1);
    }
}
