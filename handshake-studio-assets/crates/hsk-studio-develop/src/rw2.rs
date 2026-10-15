//! Panasonic RW2 / Leica RWL container decode: 16-bit word files and the 16-byte block encodings
//! (raw formats 2, 4, 5, 6 and 7).
//!
//! Adapted from LightCraft `crates/raw/src/vendor/rw2.rs` (https://github.com/storytold/lightcraft at
//! af4c5e102c9f97529f582003b99c21b89067e906; MIT OR Apache-2.0; attribution in this crate's `NOTICE`).
//! LightCraft is clean-room (no dcraw, LibRaw, rawspeed, rawloader/rawler, darktable, RawTherapee,
//! libopenraw or ExifTool code read); its sources are ExifTool's PanasonicRaw tag-name tables, the
//! libopenraw prose notes and black-box analysis of 178 CC0 raw.pixls.us files (~120 bodies).
//!
//! Layout: `IIU\0` magic, otherwise a classic little-endian TIFF (read here through a copy whose
//! magic is set to 42; the container hash covers the original bytes). IFD0: 0x0002/0x0003 sensor
//! width/height, 0x0009 CFA code (1 RGGB, 2 GRBG, 3 GBRG, 4 BGGR), 0x000A bits, 0x000B compression,
//! 0x002D raw format, 0x0118 raw data offset (else StripOffsets). Encodings:
//! - 16-bit words (no raw format, compression 34828/34830): little-endian, left-justified samples.
//! - Packed (formats 2, 5, 7): each 16-byte block is a little-endian 128-bit number holding
//!   floor(128 / bits) samples from bit 0 up; format 5 stores every whole 0x4000-byte chunk rotated
//!   (logical bytes 0x1FF8..0x4000 then 0..0x1FF8).
//! - Format 4 (rotated chunks as format 5): a block read from bit 127 down codes 14 pixels; before
//!   pixels 2, 5, 8, 11 a 2-bit scale (step 1, 2, 4, or 16 absolute); each colour's first pixel is a
//!   seed (8 bits `h`, then if `h != 0` or `i >= 12` 4 bits `l`, value `16h + l`; `h == 0` marks a
//!   mapped-out defect decoded as 0 and defers the seed); later pixels are 8-bit codes `j` against the
//!   colour's previous value `p`: `j == 0` keeps `p`, else `p + (j - 128) * m` for a relative scale
//!   with `p >= 128 m`, else `j * m + (p mod m)`.
//! - Format 6 (not rotated): 14-bit two 14-bit seeds and three groups of a 2-bit scale and three
//!   10-bit codes (11 pixels); 12-bit two 12-bit seeds and four groups of three 8-bit codes (14
//!   pixels); reconstructed as format 4 with bias 512 / 128.
//!
//! Typed refusals: raw format 8 (prefix-coded strips, DC-GH6 and later) and unknown formats are
//! `decoder_unavailable:rw2`; an unknown CFA code is `unsupported_pattern`. Levels: black from
//! 0x001C-0x001E plus the +15 codec offset LightCraft measured for formats 4/6, white measured
//! within the sensor borders (see `levels`). Not read or applied (named in `unapplied`): sensor
//! borders and crop as an active area, mapped-out defect pixels (stored as 0).
// V4-01: this module decodes untrusted bytes; every integer operation is checked or saturating.
#![deny(clippy::arithmetic_side_effects)]
use crate::DevelopError;
use crate::camera_colour::VendorColour;
use crate::dng::{DngRaw, MAX_DNG_PIXELS, VendorFrame};
use crate::levels;
use crate::pipeline::{Bayer, CfaPattern};
use crate::tiffep::{self, Ifd, Tiff};

pub const UNAPPLIED: [&str; 4] = ["rw2_black_level_unread", "rw2_white_level_unread", "rw2_borders_unread", "rw2_defect_pixels_unfilled"];

const SENSOR_WIDTH: u16 = 0x0002;
const SENSOR_HEIGHT: u16 = 0x0003;
const CFA_PATTERN: u16 = 0x0009;
const BITS: u16 = 0x000a;
const COMPRESSION: u16 = 0x000b;
const RAW_FORMAT: u16 = 0x002d;
const RAW_OFFSET: u16 = 0x0118;
const CHUNK: usize = 0x4000;
const ROTATE: usize = 0x1ff8;

pub fn is_rw2(bytes: &[u8]) -> bool {
    bytes.starts_with(b"IIU\0")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Block {
    Packed { bits: u32 },
    Format4,
    Format6 { bits14: bool },
}

impl Block {
    fn per(self) -> usize {
        match self {
            Block::Packed { bits } => 128usize.checked_div(bits as usize).unwrap_or(0),
            Block::Format4 | Block::Format6 { bits14: false } => 14,
            Block::Format6 { bits14: true } => 11,
        }
    }

    fn decode(self, b: &[u8; 16], out: &mut [u16]) {
        match self {
            Block::Packed { bits } => packed(b, bits, out),
            Block::Format4 => format4(b, out),
            Block::Format6 { bits14 } => format6(b, bits14, out),
        }
    }
}

enum Encoding {
    Words,
    Blocks { kind: Block, rotated: bool },
}

fn encoding(format: Option<u32>, compression: Option<u32>, bits: u32) -> Result<Encoding, DevelopError> {
    let blocks = |kind, rotated| Ok(Encoding::Blocks { kind, rotated });
    match (format, compression) {
        (Some(2), _) if bits == 12 => blocks(Block::Packed { bits }, false),
        (Some(4), _) if bits == 12 => blocks(Block::Format4, true),
        (Some(5), _) if bits == 12 || bits == 14 => blocks(Block::Packed { bits }, true),
        (Some(6), _) if bits == 12 || bits == 14 => blocks(Block::Format6 { bits14: bits == 14 }, false),
        (Some(7), _) if bits == 14 => blocks(Block::Packed { bits }, false),
        (None, Some(34828 | 34830)) if (12..=16).contains(&bits) => Ok(Encoding::Words),
        _ => Err(DevelopError::DecoderUnavailable("rw2")),
    }
}

/// Bit fields of a 16-byte block as a little-endian 128-bit number, read from bit 127 down.
struct Fields {
    word: u128,
    left: u32,
}

impl Fields {
    fn new(b: &[u8; 16]) -> Self {
        Self { word: u128::from_le_bytes(*b), left: 128 }
    }

    /// The next `n` (1..=16) bits.
    fn take(&mut self, n: u32) -> i32 {
        self.left = self.left.saturating_sub(n);
        let mask = 1u128.checked_shl(n).unwrap_or(0).wrapping_sub(1);
        i32::try_from((self.word >> self.left) & mask).unwrap_or(0)
    }
}

/// Step and absolute flag of the 2-bit scales of formats 4 and 6.
const STEPS: [(i32, bool); 4] = [(1, false), (2, false), (4, false), (16, true)];

fn step(f: &mut Fields) -> (i32, bool) {
    STEPS.get((f.take(2) & 3) as usize).copied().unwrap_or((1, false))
}

/// Formats 4 and 6: the next value from the previous value `p` and code `j`.
fn next(p: i32, j: i32, (m, absolute): (i32, bool), bias: i32) -> i32 {
    if j == 0 {
        p
    } else if !absolute && p >= bias.saturating_mul(m) {
        p.saturating_add(j.saturating_sub(bias).saturating_mul(m))
    } else {
        j.saturating_mul(m).saturating_add(p & m.saturating_sub(1))
    }
}

fn put(out: &mut [u16], i: usize, v: i32) {
    if let Some(o) = out.get_mut(i) {
        *o = u16::try_from(v.clamp(0, i32::from(u16::MAX))).unwrap_or(u16::MAX);
    }
}

fn packed(b: &[u8; 16], bits: u32, out: &mut [u16]) {
    let word = u128::from_le_bytes(*b);
    let mask = 1u128.checked_shl(bits.min(16)).unwrap_or(0).wrapping_sub(1);
    let per = 128usize.checked_div(bits as usize).unwrap_or(0);
    for (i, o) in out.iter_mut().enumerate().take(per) {
        let shift = u32::try_from(i).unwrap_or(0).saturating_mul(bits);
        *o = u16::try_from(word.checked_shr(shift).unwrap_or(0) & mask).unwrap_or(0);
    }
}

fn format4(b: &[u8; 16], out: &mut [u16]) {
    let mut f = Fields::new(b);
    let (mut prev, mut seeded, mut scale) = ([0i32; 2], [false; 2], STEPS[0]);
    for i in 0..14usize {
        if i % 3 == 2 {
            scale = step(&mut f);
        }
        let c = i & 1;
        let v = if seeded[c] {
            prev[c] = next(prev[c], f.take(8), scale, 128);
            prev[c]
        } else {
            let hi = f.take(8);
            if hi != 0 || i >= 12 {
                prev[c] = (hi << 4) | f.take(4);
                seeded[c] = hi != 0;
                prev[c]
            } else {
                0 // a defect marker; this colour's seed comes with its next pixel
            }
        };
        put(out, i, v);
    }
}

fn format6(b: &[u8; 16], bits14: bool, out: &mut [u16]) {
    let (seed, code, groups, bias) = if bits14 { (14, 10, 3usize, 512) } else { (12, 8, 4usize, 128) };
    let mut f = Fields::new(b);
    let mut prev = [f.take(seed), f.take(seed)];
    put(out, 0, prev[0]);
    put(out, 1, prev[1]);
    for g in 0..groups {
        let scale = step(&mut f);
        for k in 0..3usize {
            let i = g.saturating_mul(3).saturating_add(k).saturating_add(2);
            let c = i & 1;
            prev[c] = next(prev[c], f.take(code), scale, bias);
            put(out, i, prev[c]);
        }
    }
}

/// Decode 16-byte blocks into `out` chunk by chunk (whole 0x4000-byte chunks rotated when `rotated`).
fn decode_blocks(src: &[u8], kind: Block, rotated: bool, out: &mut [u16]) -> Result<(), DevelopError> {
    let per = kind.per();
    let samples_per_chunk = (CHUNK / 16).checked_mul(per).filter(|&n| n > 0).ok_or(DevelopError::InvalidContainer)?;
    let mut logical = Vec::with_capacity(CHUNK);
    for (c, o) in out.chunks_mut(samples_per_chunk).enumerate() {
        let start = c.checked_mul(CHUNK).ok_or(DevelopError::Overflow)?;
        let raw = src.get(start..).unwrap_or_default();
        let raw = raw.get(..CHUNK).unwrap_or(raw);
        let chunk: &[u8] = if rotated && raw.len() == CHUNK {
            logical.clear();
            logical.extend_from_slice(raw.get(ROTATE..).unwrap_or_default());
            logical.extend_from_slice(raw.get(..ROTATE).unwrap_or_default());
            &logical
        } else {
            raw
        };
        for (i, px) in o.chunks_mut(per).enumerate() {
            let at = i.checked_mul(16).ok_or(DevelopError::Overflow)?;
            let block = chunk.get(at..at.checked_add(16).ok_or(DevelopError::Overflow)?).and_then(|s| <&[u8; 16]>::try_from(s).ok());
            // The caller checked the stream holds every block; a short final chunk is truncation.
            let block = block.ok_or(DevelopError::InvalidContainer)?;
            kind.decode(block, px);
        }
    }
    Ok(())
}

fn tag_u32(tiff: &Tiff<'_>, ifd: &Ifd, tag: u16) -> Result<Option<u32>, DevelopError> {
    ifd.find(tag).map(|e| tiffep::first_u32(tiff, e)).transpose()
}

pub fn decode(bytes: &[u8]) -> Result<DngRaw, DevelopError> {
    if !is_rw2(bytes) {
        return Err(DevelopError::InvalidContainer);
    }
    let mut patched = bytes.to_vec();
    patched.get_mut(2..4).ok_or(DevelopError::InvalidContainer)?.copy_from_slice(&[42, 0]);
    let (tiff, first) = Tiff::parse(&patched)?;
    let root = tiff.ifd(first)?;
    let width = tag_u32(&tiff, &root, SENSOR_WIDTH)?.ok_or(DevelopError::InvalidContainer)?;
    let height = tag_u32(&tiff, &root, SENSOR_HEIGHT)?.ok_or(DevelopError::InvalidContainer)?;
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if width == 0 || height == 0 || width > 65_535 || height > 65_535 || pixels > MAX_DNG_PIXELS {
        return Err(DevelopError::InvalidDimensions);
    }
    let bits = tag_u32(&tiff, &root, BITS)?.unwrap_or(12);
    let bits16 = u16::try_from(bits).ok().filter(|b| (8..=16).contains(b)).ok_or(DevelopError::UnsupportedContainer)?;
    let encoding = encoding(tag_u32(&tiff, &root, RAW_FORMAT)?, tag_u32(&tiff, &root, COMPRESSION)?, bits)?;
    let cfa_pattern = CfaPattern::Bayer(match tag_u32(&tiff, &root, CFA_PATTERN)? {
        Some(1) => Bayer::Rggb,
        Some(2) => Bayer::Grbg,
        Some(3) => Bayer::Gbrg,
        Some(4) => Bayer::Bggr,
        _ => return Err(DevelopError::UnsupportedPattern),
    });
    let offset = match tag_u32(&tiff, &root, RAW_OFFSET)? {
        Some(o) => o,
        None => tag_u32(&tiff, &root, tiffep::STRIP_OFFSETS)?.ok_or(DevelopError::InvalidContainer)?,
    };
    let src = bytes.get(offset as usize..).ok_or(DevelopError::InvalidContainer)?;
    let n = usize::try_from(pixels).map_err(|_| DevelopError::InvalidDimensions)?;
    // The stream must hold every sample before the frame is allocated (V4-02).
    let need = match &encoding {
        Encoding::Words => n.checked_mul(2),
        Encoding::Blocks { kind, .. } => n.div_ceil(kind.per().max(1)).checked_mul(16),
    }
    .ok_or(DevelopError::Overflow)?;
    if src.len() < need {
        return Err(DevelopError::InvalidContainer);
    }
    // Formats 4 and 6 store every sample 15 above the tagged black (LightCraft's measurement on ~100
    // files: flat dark regions sit 13-20 above the tag; the other formats do not).
    let codec_offset = if matches!(encoding, Encoding::Blocks { kind: Block::Format4 | Block::Format6 { .. }, .. }) { 15.0 } else { 0.0 };
    let mut samples = vec![0u16; n];
    match encoding {
        Encoding::Words => {
            let shift = 16u32.saturating_sub(bits);
            for (o, b) in samples.iter_mut().zip(src.chunks_exact(2)) {
                *o = u16::from_le_bytes([b[0], b[1]]) >> shift;
            }
        }
        Encoding::Blocks { kind, rotated } => decode_blocks(src, kind, rotated, &mut samples)?,
    }
    // Levels: black = IFD0 0x001C/0x001D/0x001E (R, G, B) plus the codec offset, per 2x2 site of
    // the CFA; white = what the sensor area proves (saturation plateau, or >= 16 samples at full
    // scale), else full scale with the white unread.
    let full = VendorFrame::full_scale(bits16).ok_or(DevelopError::UnsupportedContainer)?;
    let mut applied = Vec::new();
    let rgb = [0x001c, 0x001d, 0x001e].map(|t| tag_u32(&tiff, &root, t).ok().flatten());
    let (black_repeat, black_level) = match (rgb, cfa_pattern) {
        ([Some(r), Some(g), Some(b)], CfaPattern::Bayer(bayer)) if r > 0 || g > 0 || b > 0 => {
            applied.push("rw2_black_level_unread");
            let level = [f64::from(r), f64::from(g), f64::from(b)];
            let cell = bayer.cell();
            let sites: Vec<f64> = cell.iter().flatten().map(|c| level.get(usize::from(*c)).copied().unwrap_or(0.0) + codec_offset).collect();
            ((2, 2), sites)
        }
        _ => ((1, 1), vec![0.0]),
    };
    let region = sensor_area(&tiff, &root, width, height).unwrap_or(levels::Region::whole(width, height));
    let white = levels::measured_white(&samples, width, height, region, full);
    if white.is_some() {
        applied.push("rw2_white_level_unread");
    }
    DngRaw::from_vendor(
        VendorFrame {
            width,
            height,
            bits: bits16,
            cfa_pattern,
            unique_camera_model: tiffep::ascii(&tiff, &root, tiffep::MODEL)?.map(|m| m.trim().to_string()),
            unapplied: levels::remaining(&UNAPPLIED, &applied),
            samples,
            white_level: white.unwrap_or(full),
            black_repeat,
            black_level,
            colour: VendorColour {
                make: tiffep::ascii(&tiff, &root, tiffep::MAKE)?.map(|m| m.trim().to_string()),
                as_shot_neutral: match [0x0024, 0x0025, 0x0026].map(|t| tag_u32(&tiff, &root, t).ok().flatten().map(f64::from)) {
                    // WB_RGBLevels (ExifTool Panasonic tag names, LightCraft's reading).
                    [Some(r), Some(g), Some(b)] if g > 0.0 => VendorColour::neutral_from_multipliers(r / g, b / g),
                    _ => None,
                },
                neutral_tag: Some("panasonic_0x0024_0x0026"),
                file_matrix: None,
            },
        },
        bytes,
    )
}

/// Sensor borders 0x0004 top, 0x0005 left, 0x0006 bottom, 0x0007 right as a frame region.
pub(crate) fn sensor_area(tiff: &Tiff<'_>, root: &Ifd, width: u32, height: u32) -> Option<levels::Region> {
    let get = |t: u16| tag_u32(tiff, root, t).ok().flatten();
    let region = levels::Region { x0: get(0x0005)?, y0: get(0x0004)?, x1: get(0x0007)?, y1: get(0x0006)? };
    (region.x0 < region.x1 && region.y0 < region.y1 && region.x1 <= width && region.y1 <= height).then_some(region)
}
