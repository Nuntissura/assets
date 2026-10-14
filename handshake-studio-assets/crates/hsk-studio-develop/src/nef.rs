//! Nikon NEF container decode, uncompressed variant only (clean-room from public documentation; no
//! LGPL/GPL decoder source read). Sources:
//! - libopenraw NEF notes: https://libopenraw.pages.freedesktop.org/formats/nef ("a TIFF file, close
//!   to TIFF/EP or DNG"; "D1 files are not compressed"; Compression 34713 = Huffman tree plus a
//!   quantization table in MakerNote tags 0x8C/0x96, layout not documented there).
//! - Bill Claff, NEF Compression: https://www.photonstophotos.net/NikonInfo/NEF_Compression.htm
//!   (lossy encoding step then non-adaptive Huffman; tables stored in the file; no tree layout).
//! - TIFF 6.0 / TIFF-EP CFA tags through `tiffep`.
//! - Byte inspection of the CC0 sample nikon-d1h-12bit-uncompressed.nef (raw.pixls.us id 5364):
//!   big-endian, raw in a SubIFD (NewSubfileType 0, Photometric 32803, 2012 x 1324, 12-bit,
//!   Compression 1, 20-row strips, CFAPattern 2,1,1,0).
//!
//! Compression 34713 (Nikon Huffman, D70s/Z6 samples) and 32769 (packed with padding) are typed
//! `decoder_unavailable:nef_compressed`: no public documentation of the Huffman trees and no
//! permissively licensed decoder source were found (decision R1 and its permissive-source
//! refinement); other compressions are `unsupported_compression`. Not read (named in
//! `unapplied`): MakerNote black/white levels and any MakerNote linearization curve.
// V4-01: this module decodes untrusted bytes; every integer operation is checked or saturating.
#![deny(clippy::arithmetic_side_effects)]
use crate::DevelopError;
use crate::dng::{DngRaw, VendorFrame};
use crate::tiffep::Tiff;
use crate::tiffep;

pub const UNAPPLIED: [&str; 3] = [
    "nef_black_level_unread",
    "nef_white_level_unread",
    "nef_makernote_curve_unread",
];

/// A classic TIFF whose IFD0 Make starts with `NIKON` and that carries no DNGVersion (a DNG
/// converted from a Nikon file keeps the Make and stays with the DNG reader).
pub fn is_nef(bytes: &[u8]) -> bool {
    let Ok((tiff, first)) = Tiff::parse(bytes) else {
        return false;
    };
    let Ok(root) = tiff.ifd(first) else {
        return false;
    };
    root.find(tiffep::DNG_VERSION).is_none()
        && matches!(tiffep::ascii(&tiff, &root, tiffep::MAKE), Ok(Some(make)) if make.starts_with("NIKON"))
}

pub fn decode(bytes: &[u8]) -> Result<DngRaw, DevelopError> {
    let (tiff, first) = Tiff::parse(bytes)?;
    let ifds = tiff.walk(first)?;
    let root = ifds.first().ok_or(DevelopError::InvalidContainer)?;
    let raw = tiffep::cfa_ifd(&tiff, &ifds)?.ok_or(DevelopError::UnsupportedContainer)?;
    let (width, height, bits, compression) = tiffep::geometry(&tiff, raw)?;
    match compression {
        1 => {}
        // Nikon Huffman (34713) and packed-with-padding (32769): no public bit-level documentation
        // or permissive source exists (searched 2026-10-14, lane SMB-DEVELOP-RAW-ARW notes).
        34713 | 32769 => return Err(DevelopError::DecoderUnavailable("nef_compressed")),
        _ => return Err(DevelopError::UnsupportedCompression),
    }
    let cfa_pattern = tiffep::cfa_pattern(&tiff, raw)?;
    let samples = tiffep::uncompressed_strips(&tiff, raw, width, height, bits, bytes.len())?;
    DngRaw::from_vendor(
        VendorFrame {
            width,
            height,
            bits,
            cfa_pattern,
            unique_camera_model: tiffep::ascii(&tiff, root, tiffep::MODEL)?,
            unapplied: UNAPPLIED.to_vec(),
            samples,
            white_level: VendorFrame::full_scale(bits).ok_or(DevelopError::UnsupportedContainer)?,
        },
        bytes,
    )
}
