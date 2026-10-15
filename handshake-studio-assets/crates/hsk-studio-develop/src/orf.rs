//! Olympus ORF container decode: the 12-bit "two-field" packed layout of the older compacts
//! (C5060WZ, C7070WZ, SP-510UZ ... per LightCraft's analysis of six CC0 files).
//!
//! Adapted from LightCraft `crates/raw/src/vendor/orf.rs` (`field_rows`, the two-field decode path)
//! and `crates/raw/src/vendor/mod.rs` (`cfa_from_exif`) (https://github.com/storytold/lightcraft at
//! af4c5e102c9f97529f582003b99c21b89067e906; MIT OR Apache-2.0; attribution in this crate's
//! `NOTICE`); LightCraft's sources are TIFF 6.0, Exif 2.3 (CFAPattern 0xA302), ExifTool's Olympus tag
//! names and black-box analysis of CC0 raw.pixls.us samples. No copyleft decoder source was read.
//!
//! Layout: an `IIRO`/`IIRS`/`MMOR` file is a classic TIFF with a different magic number (read here
//! through a copy whose magic is set to 42; the container hash covers the original bytes). IFD0 says
//! uncompressed, 12 bits, many strips whose byte counts add up to exactly `width * height * 1.5`.
//! Samples are packed MSB first, two per three bytes; rows are `width * 1.5` bytes without padding.
//! Stored rows `0..h0` are the even output rows and the rows from `h0 = (height + 1) / 2` the odd
//! ones (height odd); the strip table has exactly one gap, where the second field starts, and omits
//! the last row of the second field, which follows the last strip. The CFA is the Exif CFAPattern.
//!
//! Typed refusals: Olympus compressed ORF (every interchangeable-lens body since ~2008; no
//! permissively licensed description exists) and the other uncompressed ORF layouts not built in
//! this lane are `decoder_unavailable:orf`; a missing or unknown CFAPattern is
//! `unsupported_pattern`. Levels: black from maker note 0x1012 (ValidBits 0x102C units), white
//! measured (see `levels`). Not read (named in `unapplied`): active area.
// V4-01: this module decodes untrusted bytes; every integer operation is checked or saturating.
#![deny(clippy::arithmetic_side_effects)]
use crate::DevelopError;
use crate::dng::{DngRaw, MAX_DNG_PIXELS, VendorFrame};
use crate::levels;
use crate::pipeline::{Bayer, CfaPattern};
use crate::tiffep::{self, Ifd, Tiff};

pub const UNAPPLIED: [&str; 3] = ["orf_black_level_unread", "orf_white_level_unread", "orf_active_area_unread"];

const EXIF_IFD: u16 = 0x8769;
const EXIF_CFA_PATTERN: u16 = 0xa302;

pub fn is_orf(bytes: &[u8]) -> bool {
    [&b"IIRO"[..], b"IIRS", b"MMOR"].iter().any(|m| bytes.starts_with(m))
}

fn add(a: usize, b: usize) -> Result<usize, DevelopError> {
    a.checked_add(b).ok_or(DevelopError::Overflow)
}

fn mul(a: usize, b: usize) -> Result<usize, DevelopError> {
    a.checked_mul(b).ok_or(DevelopError::Overflow)
}

/// Byte offsets of the `height` stored rows plus the trailing last row, or `None` unless every
/// condition of the two-field layout holds exactly.
fn field_rows(strips: &[(usize, usize)], w: usize, h: usize, file_len: usize) -> Option<Vec<usize>> {
    let rb = w.checked_mul(3)? / 2;
    if rb == 0 || !w.is_multiple_of(2) || h.is_multiple_of(2) || h < 3 || strips.len() < 2 {
        return None;
    }
    let h0 = h.div_ceil(2);
    let total = strips.iter().try_fold(0usize, |a, s| a.checked_add(s.1))?;
    if total != rb.checked_mul(h)? {
        return None;
    }
    let mut rows = Vec::with_capacity(h.checked_add(1)?);
    let mut gaps = 0u32;
    let mut end: Option<usize> = None;
    for &(offset, len) in strips {
        let next_end = offset.checked_add(len).filter(|e| len > 0 && len.is_multiple_of(rb) && *e <= file_len)?;
        match end {
            Some(e) if offset < e => return None,
            // the gap must be the field seam
            Some(e) if offset > e => {
                if rows.len() != h0 {
                    return None;
                }
                gaps = gaps.saturating_add(1);
            }
            _ => {}
        }
        end = Some(next_end);
        for r in 0..len.checked_div(rb)? {
            rows.push(offset.checked_add(r.checked_mul(rb)?)?);
        }
    }
    let tail = end?;
    if gaps != 1 || rows.len() != h || tail.checked_add(rb)? > file_len {
        return None;
    }
    rows.push(tail);
    Some(rows)
}

/// Exif CFAPattern (2 x 2 repeat dims in either byte order, then the 4 colours).
fn cfa_from_exif(tiff: &Tiff<'_>, root: &Ifd) -> Result<CfaPattern, DevelopError> {
    let exif = root.find(EXIF_IFD).ok_or(DevelopError::UnsupportedPattern)?;
    let exif = tiff.ifd(tiffep::first_u32(tiff, exif)?)?;
    let bytes = tiff.bytes(exif.find(EXIF_CFA_PATTERN).ok_or(DevelopError::UnsupportedPattern)?)?;
    let &[c0, c1, r0, r1, s0, s1, s2, s3] = bytes else {
        return Err(DevelopError::UnsupportedPattern);
    };
    let two = |a: u8, b: u8| matches!((a, b), (2, 0) | (0, 2));
    if !(two(c0, c1) && two(r0, r1)) {
        return Err(DevelopError::UnsupportedPattern);
    }
    Ok(CfaPattern::Bayer(match [s0, s1, s2, s3] {
        [0, 1, 1, 2] => Bayer::Rggb,
        [2, 1, 1, 0] => Bayer::Bggr,
        [1, 0, 2, 1] => Bayer::Grbg,
        [1, 2, 0, 1] => Bayer::Gbrg,
        _ => return Err(DevelopError::UnsupportedPattern),
    }))
}

pub fn decode(bytes: &[u8]) -> Result<DngRaw, DevelopError> {
    if !is_orf(bytes) {
        return Err(DevelopError::InvalidContainer);
    }
    let mut patched = bytes.to_vec();
    let magic: [u8; 2] = if bytes.starts_with(b"MM") { [0, 42] } else { [42, 0] };
    patched.get_mut(2..4).ok_or(DevelopError::InvalidContainer)?.copy_from_slice(&magic);
    let (tiff, first) = Tiff::parse(&patched)?;
    let root = tiff.ifd(first)?;
    let (width, height, bits, compression) = tiffep::geometry(&tiff, &root)?;
    if compression != 1 {
        return Err(DevelopError::UnsupportedCompression);
    }
    let (w, h) = (width as usize, height as usize);
    if u64::from(width).saturating_mul(u64::from(height)) > MAX_DNG_PIXELS {
        return Err(DevelopError::InvalidDimensions);
    }
    let offsets = tiff.u32s(root.find(tiffep::STRIP_OFFSETS).ok_or(DevelopError::InvalidContainer)?)?;
    let counts = tiff.u32s(root.find(tiffep::STRIP_BYTE_COUNTS).ok_or(DevelopError::InvalidContainer)?)?;
    if offsets.len() != counts.len() {
        return Err(DevelopError::InvalidContainer);
    }
    let strips: Vec<(usize, usize)> = offsets.iter().zip(&counts).map(|(o, c)| (*o as usize, *c as usize)).collect();
    // Anything but the exact two-field layout (incl. Olympus compressed ORF) is not decoded here.
    let rows = (bits == 12).then(|| field_rows(&strips, w, h, bytes.len())).flatten().ok_or(DevelopError::DecoderUnavailable("orf"))?;
    let cfa_pattern = cfa_from_exif(&tiff, &root)?;
    let rb = mul(w, 3)? / 2;
    let h0 = h.div_ceil(2);
    let mut samples = vec![0u16; mul(w, h)?];
    for (y, row) in samples.chunks_exact_mut(w).enumerate() {
        let stored = if y % 2 == 0 { y / 2 } else { add(h0, y / 2)? };
        let at = *rows.get(stored).ok_or(DevelopError::InvalidContainer)?;
        let src = bytes.get(at..add(at, rb)?).ok_or(DevelopError::InvalidContainer)?;
        for (pair, b) in row.chunks_exact_mut(2).zip(src.chunks_exact(3)) {
            let (b0, b1, b2) = (u16::from(b[0]), u16::from(b[1]), u16::from(b[2]));
            pair[0] = (b0 << 4) | (b1 >> 4);
            pair[1] = ((b1 & 15) << 8) | b2;
        }
    }
    // Levels: black = old-style maker note 0x1012 BlackLevel (mean of its four values, in 0x102C
    // ValidBits units scaled to 12 bit; ExifTool Olympus tag names, LightCraft's reading); white =
    // what the frame proves (saturation plateau, or >= 16 samples at full scale), else unread.
    let full = VendorFrame::full_scale(bits).ok_or(DevelopError::UnsupportedContainer)?;
    let mut applied = Vec::new();
    let black = olympus_black(&tiff, &root, bits);
    if black.is_some() {
        applied.push("orf_black_level_unread");
    }
    let white = levels::measured_white(&samples, width, height, levels::Region::whole(width, height), full);
    if white.is_some() {
        applied.push("orf_white_level_unread");
    }
    DngRaw::from_vendor(
        VendorFrame {
            width,
            height,
            bits,
            cfa_pattern,
            unique_camera_model: tiffep::ascii(&tiff, &root, tiffep::MODEL)?.map(|m| m.trim().to_string()),
            unapplied: levels::remaining(&UNAPPLIED, &applied),
            samples,
            white_level: white.unwrap_or(full),
            black_repeat: (1, 1),
            black_level: vec![black.unwrap_or(0.0)],
        },
        bytes,
    )
}

/// Old-style Olympus maker note (`OLYMP\0` + 2 version bytes, then an IFD whose offsets are file
/// offsets): 0x1012 BlackLevel (four values; mean) in 0x102C ValidBits units, scaled to `bits`.
fn olympus_black(tiff: &Tiff<'_>, root: &Ifd, bits: u16) -> Option<f64> {
    let exif = tiff.ifd(tiffep::first_u32(tiff, root.find(EXIF_IFD)?).ok()?).ok()?;
    let entry = exif.find(0x927c)?;
    if !tiff.bytes(entry).ok()?.starts_with(b"OLYMP\0") {
        return None;
    }
    let note = tiff.ifd(u32::try_from(entry.value_offset.checked_add(8)?).ok()?).ok()?;
    let v = tiff.u32s(note.find(0x1012)?).ok()?;
    if v.len() != 4 {
        return None;
    }
    let valid = match note.find(0x102c) {
        Some(e) => *tiff.u32s(e).ok()?.first()?,
        None => u32::from(bits),
    };
    if !(8..=16).contains(&valid) {
        return None;
    }
    let mean = v.iter().map(|b| f64::from(*b)).sum::<f64>() / 4.0;
    Some(mean * f64::from(1u32 << bits.min(16)) / f64::from(1u32 << valid))
}
