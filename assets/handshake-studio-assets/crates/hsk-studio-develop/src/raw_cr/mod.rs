//! Clean-room proprietary camera-RAW decoders (lane SMB-RAW-CLEANROOM, Operator decision 2026-10-15).
//!
//! One module per vendor, written only from public format documentation (TIFF 6.0, TIFF/EP, Exif,
//! the DNG specification, ISO-BMFF, published maker-note tag tables and reverse-engineering
//! write-ups in prose). The sources of each module are named in its header and in the lane notes
//! (`builders/lanes/hsk-studio-raw-cleanroom.json`). No copyleft code and no code of the earlier
//! LightCraft-derived decoders was read.
//!
//! Shared helpers here: TIFF-variant magic patching, Exif IFD and maker-note lookup, the Exif
//! CFAPattern, and signed 16-bit tag values.
// V4-01: this module decodes untrusted bytes; every integer operation is checked or saturating.
#![deny(clippy::arithmetic_side_effects)]
use crate::DevelopError;
use crate::pipeline::{Bayer, CfaPattern};
use crate::tiffep::{self, Entry, Ifd, Tiff};

#[cfg(test)]
mod oracle;
pub(crate) mod nef;
pub(crate) mod orf;
pub(crate) mod rw2;

/// Exif IFD pointer (TIFF/Exif 2.3, tag 34665).
pub(crate) const EXIF_IFD: u16 = 0x8769;
/// Exif MakerNote (UNDEFINED).
pub(crate) const MAKER_NOTE: u16 = 0x927c;
/// Exif CFAPattern (UNDEFINED: columns, rows as SHORTs, then one byte per site).
pub(crate) const EXIF_CFA_PATTERN: u16 = 0xa302;

/// A copy of `bytes` whose 2-byte TIFF magic (offset 2) is replaced by 42 in the file's byte order,
/// so a vendor TIFF variant (`IIRO`, `IIRS`, `MMOR`, `IIU\0`) parses through the shared TIFF reader.
/// `None` when the byte-order mark is not `II` / `MM`.
pub(crate) fn patch_tiff_magic(bytes: &[u8]) -> Option<Vec<u8>> {
    let magic: [u8; 2] = match bytes.get(..2)? {
        b"II" => [42, 0],
        b"MM" => [0, 42],
        _ => return None,
    };
    let mut copy = bytes.to_vec();
    copy.get_mut(2..4)?.copy_from_slice(&magic);
    Some(copy)
}

/// The Exif IFD of `ifd0`, if present and readable.
pub(crate) fn exif_ifd(tiff: &Tiff<'_>, ifd0: &Ifd) -> Option<Ifd> {
    let offset = tiffep::first_u32(tiff, ifd0.find(EXIF_IFD)?).ok()?;
    tiff.ifd(offset).ok()
}

/// File offset and bytes of the Exif MakerNote.
pub(crate) fn maker_note<'a>(tiff: &Tiff<'a>, exif: &Ifd) -> Option<(u64, &'a [u8])> {
    let e = exif.find(MAKER_NOTE)?;
    Some((e.value_offset, tiff.bytes(e).ok()?))
}

/// IFD0 ASCII tag with trailing NULs and spaces removed; `None` when absent or empty.
pub(crate) fn ascii_trimmed(tiff: &Tiff<'_>, ifd: &Ifd, tag: u16) -> Result<Option<String>, DevelopError> {
    Ok(tiffep::ascii(tiff, ifd, tag)?
        .map(|s| s.trim_end_matches(['\0', ' ']).to_string())
        .filter(|s| !s.is_empty()))
}

/// 16-bit values of an entry as signed integers: SHORT values reinterpreted as two's complement
/// (vendors store signed matrices in SHORT tags), SSHORT values as read.
pub(crate) fn i16s(tiff: &Tiff<'_>, e: &Entry) -> Option<Vec<i16>> {
    match e.field_type {
        3 => tiff
            .u32s(e)
            .ok()?
            .into_iter()
            .map(|v| u16::try_from(v).ok().map(|u| i16::from_ne_bytes(u.to_ne_bytes())))
            .collect(),
        8 => tiff
            .f64s(e)
            .ok()?
            .into_iter()
            .map(|v| (v.fract() == 0.0 && (-32_768.0..=32_767.0).contains(&v)).then_some(v as i16))
            .collect(),
        _ => None,
    }
}

/// 2x2 Bayer layout from four TIFF/EP colour codes (0 = R, 1 = G, 2 = B) in row-major order.
pub(crate) fn bayer_from_codes(codes: [u8; 4]) -> Option<Bayer> {
    match codes {
        [0, 1, 1, 2] => Some(Bayer::Rggb),
        [2, 1, 1, 0] => Some(Bayer::Bggr),
        [1, 0, 2, 1] => Some(Bayer::Grbg),
        [1, 2, 0, 1] => Some(Bayer::Gbrg),
        _ => None,
    }
}

/// Exif CFAPattern (Exif 2.3 tag 0xA302): two SHORTs (columns, rows) then `columns * rows` colour
/// codes. Only a 2x2 Bayer layout is accepted; the SHORTs are accepted in either byte order (both
/// read 2 only as `02 00 02 00` or `00 02 00 02`). `UnsupportedPattern` when absent or other.
pub(crate) fn exif_bayer(tiff: &Tiff<'_>, exif: Option<&Ifd>) -> Result<CfaPattern, DevelopError> {
    let e = exif.and_then(|x| x.find(EXIF_CFA_PATTERN)).ok_or(DevelopError::UnsupportedPattern)?;
    let raw = tiff.bytes(e)?;
    match raw {
        [2, 0, 2, 0, a, b, c, d] | [0, 2, 0, 2, a, b, c, d] => {
            bayer_from_codes([*a, *b, *c, *d]).map(CfaPattern::Bayer).ok_or(DevelopError::UnsupportedPattern)
        }
        _ => Err(DevelopError::UnsupportedPattern),
    }
}
