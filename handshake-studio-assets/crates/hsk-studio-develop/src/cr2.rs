//! Canon CR2 container decode (clean-room, written from public format documentation only; no
//! LGPL/GPL decoder source was read). Sources:
//! - libopenraw CR2 notes: https://libopenraw.pages.freedesktop.org/formats/cr2 (header `CR`,
//!   major 2, minor 0, u32 offset of the raw IFD at byte 12; lossless JPEG; JPEG width counts two
//!   channels per sample; tag 0xC640 holds three SHORTs, slices read top->bottom then left->right).
//! - swetake.com CR2 series: https://www.swetake.com/photo/cr2/cr2-2_en.html (header bytes),
//!   https://swetake.com/photo/cr2/cr2-6_en.html (IFD3 tags; RawImageSegmentation 0xC640 =
//!   [A slices of width B, one last slice of width C], sensor width = A*B + C, areas stored one
//!   after another), cr2-8_en.html (SOF3: X * Nf = sensor width), cr2-9_en.html (decoded data
//!   sorted per slice; masked pixels included).
//! - ITU-T T.81 (lossless process, SOF3), decoded by the crate's own `ljpeg` module.
//! - Data inspection of the CC0 sample canon-eos-350d-raw.cr2 (raw.pixls.us id 1230): raw IFD
//!   Compression 6, StripOffsets/StripByteCounts one each, 0xC640 = [1, 1758, 1758], SOF3 12-bit,
//!   2328 lines x 1758 samples x 2 components.
//!
//! Verified variant only: the JPEG frame row (`samples per line * components`) must equal the slice
//! sum (or there is no slice tag and the frame row is the raw width); anything else is a typed
//! refusal naming no guess. Levels: black from the masked columns left of the maker-note SensorInfo
//! image area, white from the image area's saturation plateau (else full scale), see `levels`.
//! Not read (named in `unapplied`): the sensor borders as an active area / crop, the CFA pattern (RGGB at the stored origin is the Canon family default, UNVERIFIED per
//! body until an independent oracle confirms it), camera colour (decision R2: caller-supplied, or
//! `missing_colour_data`).
// V4-01: this module decodes untrusted bytes; every integer operation is checked or saturating.
#![deny(clippy::arithmetic_side_effects)]
use crate::DevelopError;
use crate::dng::{DngRaw, MAX_DNG_PIXELS, VendorFrame};
use crate::levels;
use crate::ljpeg;
use crate::pipeline::{Bayer, CfaPattern};
use crate::tiffep::Tiff;

const TAG_COMPRESSION: u16 = 259;
const TAG_MODEL: u16 = 272;
const TAG_STRIP_OFFSETS: u16 = 273;
const TAG_STRIP_BYTE_COUNTS: u16 = 279;
/// RawImageSegmentation ("cr2_slice").
const TAG_SLICES: u16 = 0xC640;
/// Old-style JPEG, as written in the CR2 raw IFD (sample and swetake cr2-6).
const COMPRESSION_OJPEG: u32 = 6;

/// `unapplied` names for facts this reader does not read from a CR2.
pub const UNAPPLIED: [&str; 4] = [
    "cr2_black_level_unread",
    "cr2_white_level_unread",
    "cr2_sensor_borders_unread",
    "cr2_cfa_pattern_unverified",
];

/// True when the bytes carry the CR2 marker after a classic TIFF header.
pub fn is_cr2(bytes: &[u8]) -> bool {
    bytes.get(8..11) == Some(&b"CR\x02"[..])
}

/// SOF3 frame header of a lossless JPEG stream: (precision, lines, samples per line, components).
/// Read before any decode so the frame can be checked against the slices and the payload.
fn sof3(stream: &[u8]) -> Result<(u8, u32, u32, u32), DevelopError> {
    if stream.get(0..2) != Some(&[0xFF, 0xD8][..]) {
        return Err(DevelopError::InvalidContainer);
    }
    let mut pos = 2usize;
    loop {
        let head = pos
            .checked_add(4)
            .and_then(|end| stream.get(pos..end))
            .ok_or(DevelopError::InvalidContainer)?;
        if head[0] != 0xFF {
            return Err(DevelopError::InvalidContainer);
        }
        let len = usize::from(u16::from_be_bytes([head[2], head[3]]));
        let body_start = pos.checked_add(4).ok_or(DevelopError::InvalidContainer)?;
        let seg_end = pos
            .checked_add(2)
            .and_then(|p| p.checked_add(len))
            .ok_or(DevelopError::InvalidContainer)?;
        if len < 2 || seg_end > stream.len() {
            return Err(DevelopError::InvalidContainer);
        }
        match head[1] {
            0xC3 => {
                let seg = &stream[body_start..seg_end];
                if seg.len() < 6 {
                    return Err(DevelopError::InvalidContainer);
                }
                return Ok((
                    seg[0],
                    u32::from(u16::from_be_bytes([seg[1], seg[2]])),
                    u32::from(u16::from_be_bytes([seg[3], seg[4]])),
                    u32::from(seg[5]),
                ));
            }
            0xC0..=0xCF if !matches!(head[1], 0xC4 | 0xC8 | 0xCC) => {
                return Err(DevelopError::UnsupportedCompression);
            }
            0xDA | 0xD9 => return Err(DevelopError::InvalidContainer), // scan before frame
            _ => pos = seg_end,
        }
    }
}

/// Decode a CR2 container into the stored CFA frame (masked borders included).
pub fn decode(bytes: &[u8]) -> Result<DngRaw, DevelopError> {
    let (tiff, first) = Tiff::parse(bytes)?;
    if !is_cr2(bytes) {
        return Err(DevelopError::UnsupportedContainer);
    }
    if bytes.get(11) != Some(&0) {
        return Err(DevelopError::UnsupportedContainer); // only CR2 2.0 is documented
    }
    let raw_ifd = tiff.ifd(tiff.u32(12)?)?;
    let one = |tag: u16| -> Result<u32, DevelopError> {
        let entry = raw_ifd.find(tag).ok_or(DevelopError::InvalidContainer)?;
        match tiff.u32s(entry)?.as_slice() {
            [v] => Ok(*v),
            _ => Err(DevelopError::UnsupportedContainer), // several strips: undocumented variant
        }
    };
    if one(TAG_COMPRESSION)? != COMPRESSION_OJPEG {
        return Err(DevelopError::UnsupportedCompression);
    }
    let stream = tiff.region(
        u64::from(one(TAG_STRIP_OFFSETS)?),
        u64::from(one(TAG_STRIP_BYTE_COUNTS)?),
    )?;

    let (precision, lines, per_line, components) = sof3(stream)?;
    if !(2..=16).contains(&precision)
        || lines == 0
        || per_line == 0
        || components == 0
        || components > ljpeg::MAX_COMPONENTS as u32
    {
        return Err(DevelopError::InvalidContainer);
    }
    // One JPEG row = `per_line * components` stored samples (u16 * 4: cannot overflow u32).
    let jpeg_row = per_line.checked_mul(components).ok_or(DevelopError::InvalidDimensions)?;

    // Slice widths: A slices of width B then one of width C (checked; a zero-width full slice and
    // a sum other than the JPEG row are refused before any decode allocation).
    let slices: Vec<u32> = match raw_ifd.find(TAG_SLICES) {
        None => vec![jpeg_row],
        Some(entry) => match tiff.u32s(entry)?.as_slice() {
            [count, width, last] => {
                if *count > 0 && *width == 0 {
                    return Err(DevelopError::InvalidContainer);
                }
                let total = count
                    .checked_mul(*width)
                    .and_then(|v| v.checked_add(*last))
                    .ok_or(DevelopError::InvalidDimensions)?;
                if total == 0 || total > 65_535 {
                    return Err(DevelopError::InvalidDimensions);
                }
                if total != jpeg_row {
                    return Err(DevelopError::InvalidContainer);
                }
                let mut v: Vec<u32> = std::iter::repeat_n(*width, *count as usize).collect();
                if *last > 0 {
                    v.push(*last);
                }
                v
            }
            _ => return Err(DevelopError::InvalidContainer),
        },
    };
    let width = jpeg_row;
    let height = lines;
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if width > 65_535 || pixels > MAX_DNG_PIXELS {
        return Err(DevelopError::InvalidDimensions);
    }
    // V4-02 aggregate: lossless JPEG spends at least one bit per sample, so the declared frame must
    // fit the payload actually present before the decoder allocates it.
    if pixels.div_ceil(8) > stream.len() as u64 {
        return Err(DevelopError::InvalidDimensions);
    }
    let total = usize::try_from(pixels).map_err(|_| DevelopError::InvalidDimensions)?;
    let image = ljpeg::decode(stream, total)?;
    if image.width != per_line
        || image.height != lines
        || image.components as u32 != components
        || image.samples.len() != total
    {
        return Err(DevelopError::InvalidContainer);
    }

    // Reassemble: the decoded stream is slice 0 (all rows, top to bottom), then slice 1, ...
    let (w, h) = (width as usize, height as usize);
    let mut samples = vec![0u16; total];
    let mut read = 0usize;
    let mut x0 = 0usize;
    for slice in slices {
        let sw = slice as usize;
        for row in 0..h {
            let src_end = read.checked_add(sw).ok_or(DevelopError::Overflow)?;
            let src = image.samples.get(read..src_end).ok_or(DevelopError::InvalidContainer)?;
            let dst = row
                .checked_mul(w)
                .and_then(|v| v.checked_add(x0))
                .ok_or(DevelopError::Overflow)?;
            let dst_end = dst.checked_add(sw).ok_or(DevelopError::Overflow)?;
            samples
                .get_mut(dst..dst_end)
                .ok_or(DevelopError::InvalidContainer)?
                .copy_from_slice(src);
            read = src_end;
        }
        x0 = x0.checked_add(sw).ok_or(DevelopError::Overflow)?;
    }
    if read != total || x0 != w {
        return Err(DevelopError::InvalidContainer);
    }

    // Model (IFD0) is informational; a malformed tag is a container error, never ignored.
    let root = tiff.ifd(first)?;
    // Levels: black = per-site mean of the optically masked columns left of the SensorInfo image
    // area (columns 2..left-2 over the image rows; LightCraft's measurement rule); white = what the
    // image area proves (saturation plateau, or >= 16 samples at full scale), else full scale with
    // the white named unread. Without SensorInfo both stay unread.
    let full = VendorFrame::full_scale(u16::from(precision)).ok_or(DevelopError::InvalidContainer)?;
    let area = levels::canon_sensor_area(&tiff, &root, width, height);
    let masked = area
        .filter(|a| a.x0 >= 6)
        .and_then(|a| levels::site_means(&samples, width, height, levels::Region { x0: 2, y0: a.y0, x1: a.x0.saturating_sub(2), y1: a.y1 }, (2, 2)));
    let white = area.and_then(|a| levels::measured_white(&samples, width, height, a, full));
    let mut applied = Vec::new();
    let (black_repeat, black_level) = match masked {
        Some(black) => {
            applied.push("cr2_black_level_unread");
            ((2, 2), black)
        }
        None => ((1, 1), vec![0.0]),
    };
    if white.is_some() {
        applied.push("cr2_white_level_unread");
    }
    let white_level = white.unwrap_or(full);
    let unapplied = levels::remaining(&UNAPPLIED, &applied);
    let unique_camera_model = match root.find(TAG_MODEL) {
        Some(entry) => Some(tiff.ascii(entry)?),
        None => None,
    };
    DngRaw::from_vendor(
        VendorFrame {
            width,
            height,
            bits: u16::from(precision),
            cfa_pattern: CfaPattern::Bayer(Bayer::Rggb),
            unique_camera_model,
            unapplied,
            samples,
            white_level,
            black_repeat,
            black_level,
        },
        bytes,
    )
}
