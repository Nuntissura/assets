//! Olympus ORF, two-field 12-bit layout of the older compact cameras (clean room).
//!
//! Sources (format facts only, no code):
//! - TIFF 6.0 (Adobe, public): IFD, strips, byte order. ORF is a TIFF whose magic is `RO` / `RS`
//!   (`II`) or `OR` (`MM`) instead of 42; the file is read through a copy with the magic patched.
//! - Exif 2.3 (CIPA DC-008, public): Exif IFD 0x8769, MakerNote 0x927C, CFAPattern 0xA302.
//! - Exiv2 maker-note page (https://exiv2.org/makernote.html, documentation prose): the old Olympus
//!   maker note is `OLYMP\0` + 2 bytes followed by an IFD whose offsets are relative to the TIFF
//!   header, in the Exif byte order.
//! - ExifTool Olympus tag table (https://exiftool.org/TagNames/Olympus.html, documentation):
//!   0x1011 ColorMatrix int16u[9], 0x1012 BlackLevel int16u[4], 0x1017 RedBalance int16u[2],
//!   0x1018 BlueBalance int16u[2], 0x102C ValidBits int16u[2].
//! - The layout itself is described by develop's tests (tests/develop_orf.rs): the strips hold
//!   exactly `width * height * 1.5` bytes of 12-bit samples packed two per three bytes, most
//!   significant bits first; the even rows come first (one contiguous field), then, after one gap
//!   in the strip table (the field seam), the odd rows (the second field), whose last stored row
//!   lies after the strip table's end and must be present in the file.
// V4-01: this module decodes untrusted bytes; every integer operation is checked or saturating.
#![deny(clippy::arithmetic_side_effects)]
use super::{ascii_trimmed, exif_bayer, exif_ifd, i16s, maker_note, patch_tiff_magic};
use crate::DevelopError;
use crate::camera_colour::{FileMatrix, VendorColour};
use crate::dng::{DngRaw, VendorFrame};
use crate::levels::{Region, measured_white, remaining};
use crate::tiffep::{self, Ifd, Tiff};

const FAMILY: &str = "orf";
const OLD_NOTE: &[u8] = b"OLYMP\0";
const COLOR_MATRIX: u16 = 0x1011;
const BLACK_LEVEL: u16 = 0x1012;
const RED_BALANCE: u16 = 0x1017;
const BLUE_BALANCE: u16 = 0x1018;
const VALID_BITS: u16 = 0x102c;

pub(crate) fn is_orf(bytes: &[u8]) -> bool {
    [&b"IIRO"[..], b"IIRS", b"MMOR"].iter().any(|m| bytes.starts_with(m))
}

fn unavailable() -> DevelopError {
    DevelopError::DecoderUnavailable(FAMILY)
}

pub(crate) fn decode(bytes: &[u8]) -> Result<DngRaw, DevelopError> {
    let patched = patch_tiff_magic(bytes).ok_or(DevelopError::InvalidContainer)?;
    let (tiff, first) = Tiff::parse(&patched)?;
    let ifd0 = tiff.ifd(first)?;
    let (width, height, bits, compression) = tiffep::geometry(&tiff, &ifd0)?;
    if compression != 1 {
        return Err(DevelopError::UnsupportedCompression);
    }
    if bits != 12 {
        return Err(unavailable());
    }
    let samples = two_field(&tiff, &ifd0, width, height)?;
    let exif = exif_ifd(&tiff, &ifd0);
    let cfa_pattern = exif_bayer(&tiff, exif.as_ref())?;
    let model = ascii_trimmed(&tiff, &ifd0, tiffep::MODEL)?;
    let make = ascii_trimmed(&tiff, &ifd0, tiffep::MAKE)?;
    let note = exif.as_ref().and_then(|x| old_maker_note(&tiff, x));

    let mut applied: Vec<&str> = Vec::new();
    let mut black = vec![0.0f64];
    if let Some(b) = note.as_ref().and_then(|n| note_black(&tiff, n)) {
        black = vec![b];
        applied.push("orf_black_level_unread");
    }
    let full = 4095u32;
    let white = match measured_white(&samples, width, height, Region::whole(width, height), full) {
        Some(w) => {
            applied.push("orf_white_level_unread");
            w
        }
        None => full,
    };
    let mut colour = VendorColour { make, ..VendorColour::default() };
    if let Some(n) = note.as_ref() {
        let first = |tag: u16| n.find(tag).and_then(|e| tiff.u32s(e).ok()).and_then(|v| v.first().copied());
        if let (Some(r), Some(b)) = (first(RED_BALANCE), first(BLUE_BALANCE))
            && let Some(neutral) = VendorColour::neutral_from_multipliers(f64::from(r) / 256.0, f64::from(b) / 256.0)
        {
            colour.as_shot_neutral = Some(neutral);
            colour.neutral_tag = Some("olympus_makernote_0x1017_0x1018");
        }
        colour.file_matrix = n
            .find(COLOR_MATRIX)
            .and_then(|e| i16s(&tiff, e))
            .and_then(|v| <[i16; 9]>::try_from(v).ok())
            .and_then(|m| FileMatrix::from_scaled(m.map(f64::from), 256.0, "olympus_makernote_0x1011"));
    }
    let unapplied = remaining(&["orf_black_level_unread", "orf_white_level_unread", "orf_active_area_unread"], &applied);
    DngRaw::from_vendor(
        VendorFrame {
            width,
            height,
            bits,
            cfa_pattern,
            unique_camera_model: model,
            unapplied,
            samples,
            white_level: white,
            black_repeat: (1, 1),
            black_level: black,
            colour,
        },
        bytes,
    )
}

/// The old-style maker-note IFD (`OLYMP\0` + 2 bytes, TIFF-relative offsets).
fn old_maker_note(tiff: &Tiff<'_>, exif: &Ifd) -> Option<Ifd> {
    let (offset, raw) = maker_note(tiff, exif)?;
    if !raw.starts_with(OLD_NOTE) {
        return None;
    }
    tiff.ifd(u32::try_from(offset.checked_add(8)?).ok()?).ok()
}

/// BlackLevel 0x1012 as one level: applied only when all four values are equal and ValidBits
/// (0x102C, when present) states 12 bits. Unequal values have no documented site order, and a
/// different ValidBits no documented unit, so those stay unread.
fn note_black(tiff: &Tiff<'_>, note: &Ifd) -> Option<f64> {
    let values = tiff.u32s(note.find(BLACK_LEVEL)?).ok()?;
    if let Some(e) = note.find(VALID_BITS)
        && tiff.u32s(e).ok()?.first().copied() != Some(12)
    {
        return None;
    }
    let first = *values.first()?;
    (values.len() == 4 && values.iter().all(|v| *v == first) && first < 4095).then_some(f64::from(first))
}

/// Read the two fields into a `width x height` frame (see the module header). Every check runs
/// before the frame is allocated (V4-02); any other layout is `decoder_unavailable:orf`.
fn two_field(tiff: &Tiff<'_>, ifd: &Ifd, width: u32, height: u32) -> Result<Vec<u16>, DevelopError> {
    let offsets = tiff.u32s(ifd.find(tiffep::STRIP_OFFSETS).ok_or(DevelopError::InvalidContainer)?)?;
    let counts = tiff.u32s(ifd.find(tiffep::STRIP_BYTE_COUNTS).ok_or(DevelopError::InvalidContainer)?)?;
    if offsets.is_empty() || offsets.len() != counts.len() || !width.is_multiple_of(2) {
        return Err(unavailable());
    }
    let row_bytes = u64::from(width).checked_mul(3).map(|v| v / 2).ok_or(DevelopError::Overflow)?;
    let total = counts.iter().fold(0u64, |a, c| a.saturating_add(u64::from(*c)));
    if Some(total) != row_bytes.checked_mul(u64::from(height)) {
        return Err(unavailable());
    }
    // Exactly one discontinuity in the strip table, at the end of the even-row field.
    let even_rows = height.div_ceil(2);
    let odd_rows = height / 2;
    let mut seam = None;
    let mut before = 0u64;
    for i in 1..offsets.len() {
        let (prev_off, prev_len) = (u64::from(offsets[i.saturating_sub(1)]), u64::from(counts[i.saturating_sub(1)]));
        before = before.saturating_add(prev_len);
        if u64::from(offsets[i]) != prev_off.saturating_add(prev_len) {
            if seam.is_some() {
                return Err(unavailable());
            }
            seam = Some((i, before));
        }
    }
    let (seam_index, seam_bytes) = seam.ok_or_else(unavailable)?;
    if Some(seam_bytes) != row_bytes.checked_mul(u64::from(even_rows)) {
        return Err(unavailable());
    }
    // The second field holds as many stored rows as the first (its last one past the picture).
    let first_field = tiff
        .region(u64::from(offsets[0]), row_bytes.checked_mul(u64::from(even_rows)).ok_or(DevelopError::Overflow)?)
        .map_err(|_| unavailable())?;
    let second_field = tiff
        .region(u64::from(offsets[seam_index]), row_bytes.checked_mul(u64::from(even_rows)).ok_or(DevelopError::Overflow)?)
        .map_err(|_| unavailable())?;
    let w = usize::try_from(width).map_err(|_| DevelopError::Overflow)?;
    let rb = usize::try_from(row_bytes).map_err(|_| DevelopError::Overflow)?;
    let pixels = w.checked_mul(usize::try_from(height).map_err(|_| DevelopError::Overflow)?).ok_or(DevelopError::Overflow)?;
    let mut samples = vec![0u16; pixels];
    for (data, rows, first_row) in [(first_field, even_rows, 0u32), (second_field, odd_rows, 1)] {
        for (k, packed) in data.chunks_exact(rb).take(usize::try_from(rows).map_err(|_| DevelopError::Overflow)?).enumerate() {
            let y = u32::try_from(k).ok().and_then(|k| k.checked_mul(2)).and_then(|v| v.checked_add(first_row)).ok_or(DevelopError::Overflow)?;
            let start = usize::try_from(y).map_err(|_| DevelopError::Overflow)?.checked_mul(w).ok_or(DevelopError::Overflow)?;
            let out = samples.get_mut(start..start.checked_add(w).ok_or(DevelopError::Overflow)?).ok_or(DevelopError::InvalidContainer)?;
            for (pair, three) in out.chunks_exact_mut(2).zip(packed.chunks_exact(3)) {
                let (b0, b1, b2) = (u16::from(three[0]), u16::from(three[1]), u16::from(three[2]));
                pair[0] = (b0 << 4) | (b1 >> 4);
                pair[1] = ((b1 & 0x0f) << 8) | b2;
            }
        }
    }
    Ok(samples)
}
