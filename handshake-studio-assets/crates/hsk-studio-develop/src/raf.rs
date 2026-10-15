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
//! Typed refusals: uncompressed RAF (`decoder_unavailable:raf_uncompressed`, not in this lane's
//! compressed scope), older FinePix layouts without the raw IFD such as the SuperCCD S5000
//! (`decoder_unavailable:raf`), unknown compression modes (`unsupported_compression`), container
//! and stream modes that disagree (`invalid_container`). Not read (named in `unapplied`): black
//! levels (0xF00A), white level, the crop records 0x0110/0x0111.
// V4-01: this module decodes untrusted bytes; every integer operation is checked or saturating.
#![deny(clippy::arithmetic_side_effects)]
use crate::DevelopError;
use crate::dng::{DngRaw, MAX_DNG_PIXELS, VendorFrame};
use crate::pipeline::{Bayer, CfaPattern, XTRANS};
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
        Some(0) => return Err(DevelopError::DecoderUnavailable("raf_uncompressed")),
        Some(mode @ (2 | 3)) => {
            // The stream's own flag (byte 2: 1 lossless, 0 lossy) must agree with the container.
            if src.get(2).copied() != Some(u8::from(mode == 2)) {
                return Err(DevelopError::InvalidContainer);
            }
            rafc::decode(src, width as usize, height as usize, bits, cfa_pattern == CfaPattern::XTrans)?
        }
        _ => return Err(DevelopError::UnsupportedCompression),
    };
    DngRaw::from_vendor(
        VendorFrame {
            width,
            height,
            bits: bits16,
            cfa_pattern,
            unique_camera_model: camera_name(bytes),
            unapplied: UNAPPLIED.to_vec(),
            samples,
            white_level: VendorFrame::full_scale(bits16).ok_or(DevelopError::UnsupportedContainer)?,
            black_repeat: (1, 1),
            black_level: vec![0.0],
        },
        bytes,
    )
}
