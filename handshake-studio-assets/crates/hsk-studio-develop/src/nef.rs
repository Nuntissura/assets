//! Nikon NEF container decode, uncompressed and Huffman-compressed (uncompressed path clean-room from public documentation; no
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
//! Compression 34713 (Nikon Huffman, lossless and lossy) is decoded by `nefc` (adapted from the
//! MIT OR Apache-2.0 LightCraft decoder, see that module) with the decode table read from the
//! Nikon maker note (Exif IFD 0x8769 -> MakerNote 0x927C: `Nikon\0` + 4 version bytes + an
//! embedded TIFF whose IFD holds tag 0x0096, ExifTool NEFLinearizationTable). 32769 (packed with
//! padding) stays `decoder_unavailable:nef_compressed`; other compressions are
//! `unsupported_compression`. Not read (named in `unapplied`): MakerNote black/white levels, and
//! for uncompressed files any MakerNote linearization curve.
// V4-01: this module decodes untrusted bytes; every integer operation is checked or saturating.
#![deny(clippy::arithmetic_side_effects)]
use crate::DevelopError;
use crate::camera_colour::VendorColour;
use crate::dng::{DngRaw, VendorFrame};
use crate::levels;
use crate::nefc;
use crate::tiffep::{Ifd, Tiff};
use crate::tiffep;

const EXIF_IFD: u16 = 0x8769;
const MAKER_NOTE: u16 = 0x927C;
const LINEARIZATION_TABLE: u16 = 0x0096;
const BLACK_LEVEL: u16 = 0x003d;
const WB_RB_LEVELS: u16 = 0x000c;
const COMPRESSION_NIKON_HUFFMAN: u32 = 34713;

/// Compressed frames apply the maker-note decode table (curve) but still read no levels.
pub const UNAPPLIED_COMPRESSED: [&str; 2] = ["nef_black_level_unread", "nef_white_level_unread"];

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
    let cfa_pattern = tiffep::cfa_pattern(&tiff, raw)?;
    let (samples, ceiling, unapplied) = match compression {
        1 => (
            tiffep::uncompressed_strips(&tiff, raw, width, height, bits, bytes.len())?,
            VendorFrame::full_scale(bits).ok_or(DevelopError::UnsupportedContainer)?,
            UNAPPLIED.to_vec(),
        ),
        COMPRESSION_NIKON_HUFFMAN => {
            let (samples, white) = compressed(&tiff, root, raw, width, height, bits)?;
            (samples, white, UNAPPLIED_COMPRESSED.to_vec())
        }
        // Packed-with-padding (32769): no permissive source or public bit-level documentation.
        32769 => return Err(DevelopError::DecoderUnavailable("nef_compressed")),
        _ => return Err(DevelopError::UnsupportedCompression),
    };
    // Levels: black = maker note 0x003D when present, else 0 (older bodies write none; their dark
    // floor sits within a few codes of 0, tests/real_raw_levels.rs); white = what the frame proves
    // (saturation plateau, or >= 16 samples at the code ceiling: full scale, or the lossy curve's
    // top), else the ceiling with the white named unread.
    let (black_repeat, black_level) = match nikon_black(&tiff, root, bits) {
        Some(sites) => ((2, 2), sites),
        None => ((1, 1), vec![0.0]),
    };
    let measured = levels::measured_white(&samples, width, height, levels::Region::whole(width, height), ceiling);
    let mut applied = vec!["nef_black_level_unread"];
    if measured.is_some() {
        applied.push("nef_white_level_unread");
    }
    let unapplied = levels::remaining(&unapplied, &applied);
    let white_level = measured.unwrap_or(ceiling);
    DngRaw::from_vendor(
        VendorFrame {
            width,
            height,
            bits,
            cfa_pattern,
            unique_camera_model: tiffep::ascii(&tiff, root, tiffep::MODEL)?,
            unapplied,
            samples,
            white_level,
            black_repeat,
            black_level,
            colour: VendorColour {
                make: tiffep::ascii(&tiff, root, tiffep::MAKE)?,
                as_shot_neutral: nikon_neutral(&tiff, root),
                neutral_tag: Some("nikon_makernote_0x000c"),
                file_matrix: None,
            },
        },
        bytes,
    )
}

/// The Nikon type-3 maker note: "Nikon\0", 4 version bytes, then a TIFF with note-relative offsets
/// (Exif IFD 0x8769 -> MakerNote 0x927C). `None` when absent or of another type.
fn nikon_note<'a>(tiff: &Tiff<'a>, root: &Ifd) -> Option<(Tiff<'a>, Ifd)> {
    let exif = tiff.ifd(tiffep::first_u32(tiff, root.find(EXIF_IFD)?).ok()?).ok()?;
    let note = tiff.bytes(exif.find(MAKER_NOTE)?).ok()?;
    if !note.starts_with(b"Nikon\0") {
        return None;
    }
    let (mn, first) = Tiff::parse(note.get(10..)?).ok()?;
    let ifd = mn.ifd(first).ok()?;
    Some((mn, ifd))
}

/// Maker note tag 0x0096 bytes and the maker note's byte order (`true` = big-endian). A compressed
/// file without the table (e.g. a High Efficiency NEF) is `decoder_unavailable:nef_compressed`.
fn linearization_table<'a>(tiff: &Tiff<'a>, root: &Ifd) -> Result<(&'a [u8], bool), DevelopError> {
    let unavailable = DevelopError::DecoderUnavailable("nef_compressed");
    let (mn, ifd) = nikon_note(tiff, root).ok_or(unavailable)?;
    let table = mn.bytes(ifd.find(LINEARIZATION_TABLE).ok_or(unavailable)?)?;
    Ok((table, mn.is_big_endian()))
}

/// Maker note 0x000C WB_RBLevels (ExifTool Nikon tag names, LightCraft's reading): red and blue
/// white-balance multipliers (RATIONAL), as the camera neutral. Older bodies (D70s) store white
/// balance only in the encrypted 0x0097 ColorBalance: `None`.
fn nikon_neutral(tiff: &Tiff<'_>, root: &Ifd) -> Option<[f64; 3]> {
    let (mn, ifd) = nikon_note(tiff, root)?;
    let v = mn.f64s(ifd.find(WB_RB_LEVELS)?).ok()?;
    VendorColour::neutral_from_multipliers(*v.first()?, *v.get(1)?)
}

/// Maker note 0x003D BlackLevel (ExifTool Nikon tag names; four values in 14-bit units, scaled to
/// `bits`; LightCraft measured the 14-bit unit on 12-bit files), one per 2x2 site.
fn nikon_black(tiff: &Tiff<'_>, root: &Ifd, bits: u16) -> Option<Vec<f64>> {
    let (mn, ifd) = nikon_note(tiff, root)?;
    let v = mn.u32s(ifd.find(BLACK_LEVEL)?).ok()?;
    let scale = f64::from(1u32 << bits.min(14)) / f64::from(1u32 << 14);
    let sites: Vec<f64> = v.iter().map(|b| f64::from(*b) * scale).collect();
    (sites.len() == 4).then_some(sites)
}

/// One Huffman strip through `nefc`; returns the samples and the white level (full scale, or the
/// lossy curve's largest output).
fn compressed(
    tiff: &Tiff<'_>,
    root: &Ifd,
    raw: &Ifd,
    width: u32,
    height: u32,
    bits: u16,
) -> Result<(Vec<u16>, u32), DevelopError> {
    let (table, big_endian) = linearization_table(tiff, root)?;
    let table = nefc::parse_table(table, big_endian, bits)?;
    let offsets = tiff.u32s(raw.find(tiffep::STRIP_OFFSETS).ok_or(DevelopError::InvalidContainer)?)?;
    let counts = tiff.u32s(raw.find(tiffep::STRIP_BYTE_COUNTS).ok_or(DevelopError::InvalidContainer)?)?;
    let ([offset], [count]) = (offsets.as_slice(), counts.as_slice()) else {
        return Err(DevelopError::UnsupportedContainer);
    };
    let src = tiff.region(u64::from(*offset), u64::from(*count))?;
    let samples = nefc::decode(src, width as usize, height as usize, bits, &table)?;
    let white = match &table.encoding {
        nefc::Encoding::Lossless => VendorFrame::full_scale(bits).ok_or(DevelopError::UnsupportedContainer)?,
        nefc::Encoding::Lossy(curve) => u32::from(curve.iter().copied().max().unwrap_or(0)),
    };
    if white == 0 {
        return Err(DevelopError::InvalidContainer);
    }
    Ok((samples, white))
}
