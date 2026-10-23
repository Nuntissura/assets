//! Panasonic RW2 / RAW (clean room, lane SMB-RAW-CLEANROOM-2).
//!
//! Method: black-box study against an independent oracle, Adobe DNG Converter 18.7 (`-u -p0`,
//! uncompressed DNG), plus public tag documentation. Every layout rule below was derived as a
//! hypothesis and accepted only when the decoded plane equals the converter's stored CFA plane bit
//! for bit on the corpus files named with it (evidence and counts in the lane notes,
//! `builders/lanes/hsk-studio-raw-cleanroom-2.json`). No code or output of any other RAW decoder
//! was read or used.
//!
//! Public sources (facts only, no code):
//! - TIFF 6.0: RW2 is a little-endian TIFF whose magic is `U\0` (0x55); read through a copy with
//!   the magic patched.
//! - ExifTool PanasonicRaw tag table (https://exiftool.org/TagNames/PanasonicRaw.html,
//!   documentation): IFD0 0x0002/0x0003 SensorWidth/Height, 0x0004..0x0007 sensor borders, 0x0009
//!   CFAPattern (1 = RGGB, 2 = GRBG, 3 = GBRG, 4 = BGGR), 0x000A BitsPerSample, 0x000B Compression,
//!   0x001C..0x001E BlackLevelRed/Green/Blue, 0x0024..0x0026 WB levels, 0x002D RawFormat, 0x0111
//!   StripOffsets, 0x0118 RawDataOffset, 0x010F Make, 0x0110 Model.
//!
//! Layouts (oracle evidence = corpus files whose whole stored DNG region matched):
//! - The stream starts at RawDataOffset (0x0118), else StripOffsets (0x0111). Every block layout is
//!   row aligned: each row starts a new 16-byte block, ceil(width / samples per block) blocks per
//!   row, the last block's surplus samples dropped (LX7 probe with the width tag set to 1900).
//! - 16-bit words (no RawFormat, Compression 34828 or 34830): little-endian u16 per sample at
//!   StripOffsets, the sample in the top `bits` bits (LX1: word >> 4); a 0 sample stays 0.
//! - Formats 2 and 7: 16-byte blocks read as a little-endian 128-bit word, floor(128 / bits)
//!   samples from bit 0 up, stored in file order (FZ8 format 2 at 12 bit; format 7: GH5S with the
//!   tag set to 7 at 14 bit, LX7/LX3 padded probes at 12 bit). A 0 sample is a missing pixel.
//! - Format 5: as format 7, but the stream is stored in 0x4000-byte chunks each rotated so that the
//!   stored bytes begin with logical byte 0x1FF8 (logical byte k at stored (k + 0x2008) mod 0x4000;
//!   a final short chunk as is) (GH5S at 14 bit; LX7 padded probe at 12 bit).
//! - Formats 4 and 6, the block codec: 16-byte blocks, fields most significant bit first from bit
//!   127 of the little-endian word: two seeds (even and odd column of the block), then groups of a
//!   2-bit scale (steps 1, 2, 4, 16) and three codes. A code is absolute for the top scale or when
//!   the column's previous value is below step * 2^(code bits - 1) (code * step, plus the previous
//!   value's low `log2 step` bits at 12 bit only), else a difference (code - 2^(code bits - 1)) *
//!   step. 12 bit (14 samples per block, four groups, 8-bit codes): a seed is an 8-bit high field
//!   and, when it is not 0, 4 low bits; a 0 high field is a missing pixel and leaves the column
//!   unseeded, so its next turn reads a seed instead of a code. Format 4 (LX7, LX3, G9; chunk
//!   rotated like format 5): code 0 keeps the column's value. Format 6 at 12 bit (GH5M2; not
//!   rotated): code 0 is a missing pixel and the column keeps its value. Format 6 at 14 bit (S1;
//!   11 samples per block, plain 14-bit seeds, three groups of 10-bit codes, 4 trailing bits): no
//!   code-0 rule; every value below 16 is a missing pixel and still predicts the next. Format 6
//!   samples are the decoded values minus 15 (GH5M2, S1). Probes: crafted blocks in the LX7, GH5M2
//!   and S1 files (deferred seeds, a column unseeded twice, code 0, absolute and small values).
//!   Known divergence: the converter wraps a format-6 12-bit value below 15 modulo 2^16 (a crafted
//!   3 gave 65524); this decoder clamps it to 0 (no corpus value is below 125).
//! - Missing pixels are filled with the mean of their four nearest same-colour neighbours that are
//!   inside the frame and not missing (green: the diagonals; red and blue: two sites away along the
//!   row and column), rounded half up (LX7, LX3, G9: 82 codec fills, FZ8: 15 packed fills, LX7
//!   and LX3 probes: 300 more, every colour). Known divergence: the converter averages only
//!   neighbours inside ITS crop of the frame, so a missing pixel within two sites of that crop's
//!   edge can differ (LX3 format-7 probe: 14 such sites); the crop is a converter camera-table
//!   fact, not in the file, and no corpus file has such a site.
// V4-01: this module decodes untrusted bytes; every integer operation is checked or saturating.
#![deny(clippy::arithmetic_side_effects)]
use super::{ascii_trimmed, bayer_from_codes, patch_tiff_magic};
use crate::DevelopError;
use crate::camera_colour::VendorColour;
use crate::dng::{DngRaw, MAX_DNG_PIXELS, VendorFrame};
use crate::levels::{Region, measured_white, remaining};
use crate::pipeline::CfaPattern;
use crate::tiffep::{self, Ifd, Tiff};

const FAMILY: &str = "rw2";
const SENSOR_WIDTH: u16 = 0x0002;
const SENSOR_HEIGHT: u16 = 0x0003;
const BORDERS: [u16; 4] = [0x0004, 0x0005, 0x0006, 0x0007];
const CFA: u16 = 0x0009;
const BITS: u16 = 0x000a;
const COMPRESSION: u16 = 0x000b;
const BLACK: [u16; 3] = [0x001c, 0x001d, 0x001e];
const WB: [u16; 3] = [0x0024, 0x0025, 0x0026];
const RAW_FORMAT: u16 = 0x002d;
const STRIP_OFFSET: u16 = 0x0111;
const RAW_OFFSET: u16 = 0x0118;
const MAKE: u16 = 0x010f;
const MODEL: u16 = 0x0110;
const CHUNK: usize = 0x4000;
const ROTATION: usize = 0x1ff8;
const BLOCK: usize = 16;
/// Format 4 stores samples this many codes above the IFD0 black levels; format 6 samples are the
/// decoded values minus it (both from the DNG oracle: LX7/G9/LX3 black = tag + 15, GH5M2/S1
/// samples = decoded - 15 with black = tag).
const CODEC_OFFSET: u16 = 15;

pub(crate) fn is_rw2(bytes: &[u8]) -> bool {
    bytes.starts_with(b"IIU\0")
}

fn unavailable() -> DevelopError {
    DevelopError::DecoderUnavailable(FAMILY)
}

fn tag(tiff: &Tiff<'_>, ifd: &Ifd, t: u16) -> Result<Option<u32>, DevelopError> {
    ifd.find(t).map(|e| tiffep::first_u32(tiff, e)).transpose()
}

/// The stored layout, chosen from RawFormat (0x002D) and Compression (0x000B).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layout {
    Words,
    Packed { rotated: bool },
    Codec(Codec),
}

fn layout(format: Option<u32>, compression: Option<u32>, bits: u16) -> Result<Layout, DevelopError> {
    match (format, bits) {
        (None, 12 | 14 | 16) if matches!(compression, Some(34_828 | 34_830)) => Ok(Layout::Words),
        (Some(2), 12 | 14) => Ok(Layout::Packed { rotated: false }),
        (Some(5), 12 | 14) => Ok(Layout::Packed { rotated: true }),
        (Some(7), 12 | 14) => Ok(Layout::Packed { rotated: false }),
        (Some(4), 12) => Ok(Layout::Codec(Codec::Twelve4)),
        (Some(6), 12) => Ok(Layout::Codec(Codec::Twelve6)),
        (Some(6), 14) => Ok(Layout::Codec(Codec::Fourteen6)),
        _ => Err(unavailable()),
    }
}

pub(crate) fn decode(bytes: &[u8]) -> Result<DngRaw, DevelopError> {
    let patched = patch_tiff_magic(bytes).ok_or(DevelopError::InvalidContainer)?;
    let (tiff, first) = Tiff::parse(&patched)?;
    let ifd0 = tiff.ifd(first)?;
    let req = |t: u16| tag(&tiff, &ifd0, t)?.ok_or(DevelopError::InvalidContainer);
    let (width, height) = (req(SENSOR_WIDTH)?, req(SENSOR_HEIGHT)?);
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if width == 0 || height == 0 || width > 65_535 || height > 65_535 || pixels > MAX_DNG_PIXELS {
        return Err(DevelopError::InvalidDimensions);
    }
    let bits = u16::try_from(req(BITS)?).map_err(|_| DevelopError::InvalidContainer)?;
    let codes = match req(CFA)? {
        1 => [0, 1, 1, 2],
        2 => [1, 0, 2, 1],
        3 => [1, 2, 0, 1],
        4 => [2, 1, 1, 0],
        _ => return Err(DevelopError::UnsupportedPattern),
    };
    let bayer = bayer_from_codes(codes).ok_or(DevelopError::UnsupportedPattern)?;
    let layout = layout(tag(&tiff, &ifd0, RAW_FORMAT)?, tag(&tiff, &ifd0, COMPRESSION)?, bits)?;
    let full = VendorFrame::full_scale(bits).ok_or_else(unavailable)?;
    let offset = tag(&tiff, &ifd0, RAW_OFFSET)?.or(tag(&tiff, &ifd0, STRIP_OFFSET)?).ok_or(DevelopError::InvalidContainer)?;
    let stored = bytes.get(usize::try_from(offset).map_err(|_| DevelopError::InvalidContainer)?..).ok_or(DevelopError::InvalidContainer)?;
    let w = usize::try_from(width).map_err(|_| DevelopError::InvalidDimensions)?;
    let h = usize::try_from(height).map_err(|_| DevelopError::InvalidDimensions)?;
    let (samples, missing) = match layout {
        Layout::Words => (words(stored, w.checked_mul(h).ok_or(DevelopError::InvalidDimensions)?, bits)?, Vec::new()),
        Layout::Packed { rotated } => {
            let (mut s, missing) = packed(stored, w, h, bits, rotated)?;
            fill_missing(&mut s, &missing, w, codes)?;
            (s, missing)
        }
        Layout::Codec(kind) => {
            // Format 4 is chunk-rotated and keeps its values; format 6 is not, minus 15.
            let (mut s, missing) = codec(stored, w, h, kind, kind == Codec::Twelve4)?;
            fill_missing(&mut s, &missing, w, codes)?;
            if kind != Codec::Twelve4 {
                s.iter_mut().for_each(|v| *v = v.saturating_sub(CODEC_OFFSET));
            }
            (s, missing)
        }
    };

    let mut applied: Vec<&str> = Vec::new();
    let mut black_repeat = (1, 1);
    let mut black_level = vec![0.0];
    let levels: Option<Vec<u32>> = BLACK.iter().map(|t| tag(&tiff, &ifd0, *t).ok().flatten()).collect();
    if let Some(levels) = levels {
        let add = if layout == Layout::Codec(Codec::Twelve4) { u32::from(CODEC_OFFSET) } else { 0 };
        let site: Option<Vec<f64>> =
            codes.iter().map(|c| levels.get(usize::from(*c)).and_then(|v| v.checked_add(add)).map(f64::from)).collect();
        if let Some(site) = site
            && site.iter().all(|b| *b < f64::from(full))
        {
            black_repeat = (2, 2);
            black_level = site;
            applied.push("rw2_black_level_unread");
        }
    }
    let region = borders(&tiff, &ifd0, width, height).unwrap_or(Region::whole(width, height));
    let white = match measured_white(&samples, width, height, region, full) {
        Some(v) => {
            applied.push("rw2_white_level_unread");
            v
        }
        None => full,
    };
    // The block codec can decode a few codes above the nominal full scale (G9 and LX3: up to 4097
    // at 12 bit, kept as is by the converter's DNG); the white then covers the largest sample so
    // no decoded value is clipped or refused.
    let top = samples.iter().copied().max().map_or(0, u32::from);
    let white = if top > full { white.max(top) } else { white };
    let mut colour = VendorColour { make: ascii_trimmed(&tiff, &ifd0, MAKE)?, ..VendorColour::default() };
    let wb: Option<Vec<u32>> = WB.iter().map(|t| tag(&tiff, &ifd0, *t).ok().flatten()).collect();
    if let Some([r, g, b]) = wb.as_deref().and_then(|v| <[u32; 3]>::try_from(v).ok())
        && g > 0
        && let Some(neutral) = VendorColour::neutral_from_multipliers(f64::from(r) / f64::from(g), f64::from(b) / f64::from(g))
    {
        colour.as_shot_neutral = Some(neutral);
        colour.neutral_tag = Some("panasonic_ifd0_0x0024_0x0026");
    }
    let mut names = vec!["rw2_black_level_unread", "rw2_white_level_unread", "rw2_borders_unread"];
    if !missing.is_empty() {
        names.push("rw2_missing_pixels_interpolated");
    }
    DngRaw::from_vendor(
        VendorFrame {
            width,
            height,
            bits,
            cfa_pattern: CfaPattern::Bayer(bayer),
            unique_camera_model: ascii_trimmed(&tiff, &ifd0, MODEL)?,
            unapplied: remaining(&names, &applied),
            samples,
            white_level: white,
            black_repeat,
            black_level,
            colour,
        },
        bytes,
    )
}

/// The image area inside the sensor borders (0x0004..0x0007: top, left, bottom, right).
fn borders(tiff: &Tiff<'_>, ifd: &Ifd, width: u32, height: u32) -> Option<Region> {
    let [top, left, bottom, right] = BORDERS.map(|t| tag(tiff, ifd, t).ok().flatten());
    let r = Region { x0: left?, y0: top?, x1: right?, y1: bottom? };
    (r.x0 < r.x1 && r.y0 < r.y1 && r.x1 <= width && r.y1 <= height).then_some(r)
}

/// The logical stream of `len` bytes: whole 0x4000-byte chunks un-rotated (when `rotated`), a final
/// short chunk as stored. Refused before allocation when the file does not hold `len` bytes (V4-02).
fn logical(stored: &[u8], len: usize, rotated: bool) -> Result<Vec<u8>, DevelopError> {
    let data = stored.get(..len).ok_or(DevelopError::InvalidContainer)?;
    if !rotated {
        return Ok(data.to_vec());
    }
    let mut out = Vec::with_capacity(len);
    for chunk in stored.chunks(CHUNK) {
        if out.len() >= len {
            break;
        }
        if chunk.len() == CHUNK {
            out.extend_from_slice(chunk.get(ROTATION..).ok_or(DevelopError::Overflow)?);
            out.extend_from_slice(chunk.get(..ROTATION).ok_or(DevelopError::Overflow)?);
        } else {
            out.extend_from_slice(chunk);
        }
    }
    out.truncate(len);
    Ok(out)
}

/// 16-bit little-endian words, the sample in the top `bits` bits.
fn words(stored: &[u8], pixels: usize, bits: u16) -> Result<Vec<u16>, DevelopError> {
    let len = pixels.checked_mul(2).ok_or(DevelopError::InvalidContainer)?;
    let data = stored.get(..len).ok_or(DevelopError::InvalidContainer)?;
    let shift = 16u16.checked_sub(bits).ok_or_else(unavailable)?;
    Ok(data.chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]]) >> shift).collect())
}

/// Walk the block stream row by row: every row starts a new 16-byte block (ceil(width /
/// per_block) blocks per row, the last block's surplus samples dropped; LX7 width-1900 probe).
/// `block` turns one little-endian 128-bit word into up to 14 samples (`None` = missing pixel).
/// The stream size is checked before any allocation (V4-02).
fn rows_of_blocks(
    stored: &[u8],
    width: usize,
    height: usize,
    per_block: usize,
    rotated: bool,
    mut block: impl FnMut(u128) -> Result<[Option<u32>; 14], DevelopError>,
) -> Result<(Vec<u16>, Vec<usize>), DevelopError> {
    if per_block == 0 || per_block > 14 {
        return Err(DevelopError::InvalidContainer);
    }
    let row_bytes = width.div_ceil(per_block).checked_mul(BLOCK).ok_or(DevelopError::InvalidContainer)?;
    let len = row_bytes.checked_mul(height).ok_or(DevelopError::InvalidContainer)?;
    let data = logical(stored, len, rotated)?;
    let pixels = width.checked_mul(height).ok_or(DevelopError::InvalidDimensions)?;
    let mut out = Vec::with_capacity(pixels);
    let mut missing = Vec::new();
    for row in data.chunks_exact(row_bytes) {
        let end = out.len().checked_add(width).ok_or(DevelopError::Overflow)?;
        for b in row.chunks_exact(BLOCK) {
            let values = block(u128::from_le_bytes(b.try_into().map_err(|_| DevelopError::InvalidContainer)?))?;
            for v in values.iter().take(per_block) {
                if out.len() == end {
                    break;
                }
                match v {
                    Some(v) => out.push(u16::try_from(*v).map_err(|_| DevelopError::InvalidContainer)?),
                    None => {
                        missing.push(out.len());
                        out.push(0);
                    }
                }
            }
        }
    }
    Ok((out, missing))
}

/// Formats 2, 5 and 7: floor(128 / bits) samples per 16-byte block, from bit 0 of the
/// little-endian word up; a sample of 0 is a missing pixel (LX7 format-5/7 probes, FZ8: every 0
/// in the converter's region is filled).
fn packed(stored: &[u8], width: usize, height: usize, bits: u16, rotated: bool) -> Result<(Vec<u16>, Vec<usize>), DevelopError> {
    let per_block = usize::from(128u16.checked_div(bits).ok_or(DevelopError::InvalidContainer)?);
    let mask = 1u128.checked_shl(u32::from(bits)).and_then(|v| v.checked_sub(1)).ok_or(DevelopError::Overflow)?;
    rows_of_blocks(stored, width, height, per_block, rotated, |word| {
        let mut values = [None; 14];
        for (k, slot) in values.iter_mut().enumerate().take(per_block) {
            let shift = u32::try_from(k).ok().and_then(|k| k.checked_mul(u32::from(bits))).ok_or(DevelopError::Overflow)?;
            let v = u32::try_from(word.checked_shr(shift).ok_or(DevelopError::Overflow)? & mask).map_err(|_| DevelopError::Overflow)?;
            *slot = (v != 0).then_some(v);
        }
        Ok(values)
    })
}

/// MSB-first reader over a 128-bit block.
struct Fields {
    word: u128,
    left: u32,
}

impl Fields {
    fn take(&mut self, n: u32) -> Result<u32, DevelopError> {
        self.left = self.left.checked_sub(n).ok_or(DevelopError::InvalidContainer)?;
        let mask = 1u128.checked_shl(n).and_then(|v| v.checked_sub(1)).ok_or(DevelopError::Overflow)?;
        u32::try_from(self.word.checked_shr(self.left).ok_or(DevelopError::Overflow)? & mask).map_err(|_| DevelopError::Overflow)
    }

    /// A 12-bit seed: the high field, then 4 low bits when it is not 0; `None` (missing pixel,
    /// unseeded column) for 0.
    fn seed(&mut self, high: u32) -> Result<Option<u32>, DevelopError> {
        let h = self.take(high)?;
        if h == 0 {
            return Ok(None);
        }
        Ok(Some(h.checked_shl(4).ok_or(DevelopError::Overflow)? | self.take(4)?))
    }
}

/// Variant of the block codec (see the module header).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Codec {
    /// Format 4: 12 bit, deferred seeds, code 0 keeps the column's value.
    Twelve4,
    /// Format 6 at 12 bit: as format 4, but code 0 is a missing pixel (the column keeps its value).
    Twelve6,
    /// Format 6 at 14 bit: plain 14-bit seeds, no code-0 rule, any value below 16 is missing.
    Fourteen6,
}

/// The block codec of formats 4 and 6 (see the module header). Returns the samples and the
/// indices of the missing pixels (left 0).
fn codec(stored: &[u8], width: usize, height: usize, kind: Codec, rotated: bool) -> Result<(Vec<u16>, Vec<usize>), DevelopError> {
    const STEPS: [u32; 4] = [1, 2, 4, 16];
    let twelve = kind != Codec::Fourteen6;
    let (code_bits, groups) = if twelve { (8u32, 4usize) } else { (10, 3) };
    let per_block = groups.checked_mul(3).and_then(|v| v.checked_add(2)).ok_or(DevelopError::Overflow)?;
    let half = 1u32.checked_shl(code_bits.saturating_sub(1)).ok_or(DevelopError::Overflow)?;
    rows_of_blocks(stored, width, height, per_block, rotated, |word| {
        let mut f = Fields { word, left: 128 };
        // 12 bit: `None` = the column is unseeded (its seed's high byte was 0).
        let mut prev: [Option<u32>; 2] = [None, None];
        let mut values: [Option<u32>; 14] = [None; 14];
        for (i, p) in prev.iter_mut().enumerate() {
            *p = if twelve { f.seed(8)? } else { Some(f.take(14)?) };
            values[i] = *p;
        }
        let mut i = 2usize;
        for _ in 0..groups {
            let step = STEPS[usize::try_from(f.take(2)?).map_err(|_| DevelopError::Overflow)?];
            for _ in 0..3 {
                let column = i & 1;
                let v = match prev[column] {
                    None => f.seed(8)?,
                    Some(p) => {
                        let code = f.take(code_bits)?;
                        if code == 0 && kind == Codec::Twelve4 {
                            Some(p)
                        } else if code == 0 && kind == Codec::Twelve6 {
                            None
                        } else if step == 16 || p < step.saturating_mul(half) {
                            let low = if twelve { p & step.saturating_sub(1) } else { 0 };
                            Some(code.checked_mul(step).and_then(|v| v.checked_add(low)).ok_or(DevelopError::Overflow)?)
                        } else {
                            let up = code.checked_mul(step).ok_or(DevelopError::Overflow)?;
                            let down = step.checked_mul(half).ok_or(DevelopError::Overflow)?;
                            Some(p.checked_add(up).and_then(|v| v.checked_sub(down)).ok_or(DevelopError::InvalidContainer)?)
                        }
                    }
                };
                prev[column] = if twelve { v.or(prev[column]) } else { v };
                values[i] = v;
                i = i.saturating_add(1);
            }
        }
        if !twelve {
            // 14 bit: a value below 16 (a zero high field) is a missing pixel; it still predicts.
            values.iter_mut().for_each(|v| *v = v.filter(|v| *v >= 16));
        }
        Ok(values)
    })
}

/// Fill each missing pixel with the rounded-half-up mean of its present same-colour neighbours
/// (green: the four diagonals; red/blue: two sites away on the row and column); 0 when none.
fn fill_missing(samples: &mut [u16], missing: &[usize], width: usize, codes: [u8; 4]) -> Result<(), DevelopError> {
    if missing.is_empty() || width == 0 {
        return Ok(());
    }
    let height = samples.len().checked_div(width).ok_or(DevelopError::Overflow)?;
    let is_missing = |i: usize| missing.binary_search(&i).is_ok();
    let mut fills = Vec::with_capacity(missing.len());
    for &i in missing {
        let (y, x) = (i.checked_div(width).ok_or(DevelopError::Overflow)?, i.checked_rem(width).ok_or(DevelopError::Overflow)?);
        let colour = codes[((y & 1) << 1) | (x & 1)];
        let offsets: [(isize, isize); 4] = if colour == 1 { [(-1, -1), (-1, 1), (1, -1), (1, 1)] } else { [(-2, 0), (2, 0), (0, -2), (0, 2)] };
        let (mut sum, mut n) = (0u32, 0u32);
        for (dy, dx) in offsets {
            let (Some(ny), Some(nx)) = (y.checked_add_signed(dy), x.checked_add_signed(dx)) else { continue };
            if ny >= height || nx >= width {
                continue;
            }
            let j = ny.checked_mul(width).and_then(|v| v.checked_add(nx)).ok_or(DevelopError::Overflow)?;
            if is_missing(j) {
                continue;
            }
            sum = sum.saturating_add(u32::from(samples[j]));
            n = n.saturating_add(1);
        }
        let v = sum.saturating_mul(2).saturating_add(n).checked_div(n.saturating_mul(2)).unwrap_or(0);
        fills.push((i, u16::try_from(v).map_err(|_| DevelopError::Overflow)?));
    }
    for (i, v) in fills {
        samples[i] = v;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::decode;
    use crate::DevelopError;
    use crate::raw_cr::oracle::{Case, compare, read_pinned, samples_of};

    fn entry(tag: u16, ty: u16, value: u32) -> Vec<u8> {
        [tag.to_le_bytes().to_vec(), ty.to_le_bytes().to_vec(), 1u32.to_le_bytes().to_vec(), value.to_le_bytes().to_vec()].concat()
    }

    /// A synthetic `IIU` file: Panasonic IFD0 size, CFA code, bits, optional RawFormat and
    /// Compression, the stream at offset 256 (RawDataOffset and StripOffsets).
    fn rw2(w: u32, h: u32, cfa: u32, bits: u32, format: Option<u32>, compression: Option<u32>, data: &[u8]) -> Vec<u8> {
        let mut entries = vec![entry(0x0002, 3, w), entry(0x0003, 3, h), entry(0x0009, 3, cfa), entry(0x000a, 3, bits)];
        entries.extend(compression.map(|c| entry(0x000b, 3, c)));
        entries.extend(format.map(|f| entry(0x002d, 3, f)));
        entries.extend([entry(0x0111, 4, 256), entry(0x0118, 4, 256)]);
        let mut file = b"IIU\0\x08\0\0\0".to_vec();
        file.extend((entries.len() as u16).to_le_bytes());
        entries.into_iter().for_each(|e| file.extend(e));
        file.extend(0u32.to_le_bytes());
        file.resize(256, 0);
        file.extend_from_slice(data);
        file
    }

    /// MSB-first field writer into a 128-bit block (bit 127 down), stored little-endian.
    #[derive(Default)]
    struct Block(u128, u32);
    impl Block {
        fn put(&mut self, v: u32, n: u32) -> &mut Self {
            self.1 += n;
            self.0 |= u128::from(v) << (128 - self.1);
            self
        }
        fn bytes(&self) -> [u8; 16] {
            self.0.to_le_bytes()
        }
    }

    fn row(r: &crate::DngRaw, y: u32) -> Vec<u16> {
        (0..r.width).map(|x| r.sample(x, y)).collect()
    }

    /// A format-4 block of seeds `base`/`base + 16` and difference codes of +1.
    fn plain4(base: u32) -> [u8; 16] {
        let mut b = Block::default();
        b.put(base >> 4, 8).put(base & 15, 4).put((base + 16) >> 4, 8).put((base + 16) & 15, 4);
        for _ in 0..4 {
            b.put(0, 2).put(129, 8).put(129, 8).put(129, 8);
        }
        b.bytes()
    }

    #[test]
    fn rw2_clean_room_layouts_known_answers_and_hostile_inputs_are_typed() {
        // 16-bit words: the sample is the top 12 bits; the low nibble is ignored.
        let words: Vec<u8> = [100u16, 4095, 0, 7].iter().flat_map(|v| ((v << 4) | 0x9).to_le_bytes()).collect();
        let r = decode(&rw2(2, 2, 1, 12, None, Some(34_828), &words)).unwrap();
        assert_eq!((row(&r, 0), row(&r, 1)), (vec![100, 4095], vec![0, 7]));
        // Format 2: ten 12-bit samples from bit 0 of the little-endian word, not rotated.
        let ten: u128 = (0..10u128).fold(0, |a, k| a | ((k * 400 + 3) << (12 * k)));
        let r = decode(&rw2(10, 1, 1, 12, Some(2), None, &ten.to_le_bytes())).unwrap();
        assert_eq!(row(&r, 0), (0..10u16).map(|k| k * 400 + 3).collect::<Vec<_>>());
        // Format 5 at 14 bit: nine samples per block (a single short chunk is stored as is).
        let nine: u128 = (0..9u128).fold(0, |a, k| a | ((k * 1800 + 5) << (14 * k)));
        let r = decode(&rw2(9, 1, 1, 14, Some(5), None, &nine.to_le_bytes())).unwrap();
        assert_eq!(row(&r, 0), (0..9u16).map(|k| k * 1800 + 5).collect::<Vec<_>>());
        // Format 7, rows of 12 = two blocks per row (8 surplus samples dropped); a 0 sample at
        // (1, 1) (blue in RGGB) is missing: only (3, 1) of its neighbours exists.
        let block7 = |row: u128, first: bool| -> [u8; 16] {
            let v = |k: u128| if row == 1 && first && k == 1 { 0 } else { row * 1000 + k * 7 + 1 + if first { 0 } else { 70 } };
            (0..10u128).fold(0u128, |a, k| a | (v(k) << (12 * k))).to_le_bytes()
        };
        let data: Vec<u8> = (0..3u128).flat_map(|r| [block7(r, true), block7(r, false)].concat()).collect();
        let r = decode(&rw2(12, 3, 1, 12, Some(7), None, &data)).unwrap();
        assert_eq!(row(&r, 0), [1, 8, 15, 22, 29, 36, 43, 50, 57, 64, 71, 78]);
        assert_eq!(&row(&r, 1)[..4], &[1001, 1022, 1015, 1022]);
        assert_eq!(r.sample(0, 2), 2001);
        // Format 6 at 14 bit: 14-bit seeds, three groups of a scale and three 10-bit codes,
        // absolute codes without low bits, samples minus 15.
        let mut b = Block::default();
        b.put(2403 >> 4, 10).put(2403 & 15, 4).put(2547 >> 4, 10).put(2547 & 15, 4);
        b.put(0, 2).put(667, 10).put(559, 10).put(367, 10); // differences: 2558, 2594, 2413
        b.put(3, 2).put(100, 10).put(0, 10).put(9, 10); // top scale absolute: 1600, 0 (missing, still predicts), 144
        b.put(1, 2).put(600, 10).put(10, 10).put(600, 10); // 0 < 1024: absolute 1200; absolute 20; 1200 + 1200 - 1024
        let r = decode(&rw2(11, 1, 1, 14, Some(6), None, &b.bytes())).unwrap();
        // (6, 0) is red: the mean of (4, 0) = 2413 and (8, 0) = 1200, rounded half up = 1807.
        assert_eq!(row(&r, 0), [2403, 2547, 2558, 2594, 2413, 1600, 1807, 144, 1200, 20, 1376].map(|v| v - 15).to_vec());
        // Format 6 at 12 bit: code 0 is a missing pixel; the column keeps predicting from 1000.
        let mut b = Block::default();
        b.put(1000 >> 4, 8).put(1000 & 15, 4).put(1016 >> 4, 8).put(1016 & 15, 4);
        b.put(0, 2).put(0, 8).put(129, 8).put(130, 8);
        for _ in 0..3 {
            b.put(0, 2).put(129, 8).put(129, 8).put(129, 8);
        }
        let r = decode(&rw2(14, 1, 1, 12, Some(6), None, &b.bytes())).unwrap();
        let want = [1000, 1016, 1001, 1017, 1002, 1018, 1003, 1019, 1004, 1020, 1005, 1021, 1006, 1022];
        assert_eq!(row(&r, 0), want.map(|v| v - 15).to_vec());
        // Format 4 (rows of 14, one block each): the middle row's odd seed has a zero high byte; the
        // column re-seeds at its next turn and the missing pixel is filled from its same-colour
        // neighbours. (1, 1) is blue in RGGB: of (1, 3), (1, -1), (-1, 1), (3, 1) only (3, 1) exists.
        let mut d = Block::default();
        d.put(0x20, 8).put(0, 4).put(0, 8); // even seed 512, odd seed missing
        d.put(0, 2).put(130, 8).put(0x30, 8).put(5, 4).put(131, 8); // 514, odd re-seeds 773, 517
        for _ in 0..3 {
            d.put(0, 2).put(127, 8).put(127, 8).put(127, 8);
        }
        let data = [plain4(400), d.bytes(), plain4(600)].concat();
        let r = decode(&rw2(14, 3, 1, 12, Some(4), None, &data)).unwrap();
        assert_eq!(&row(&r, 1)[..5], &[512, 773, 514, 773, 517]);
        assert!(r.unapplied.contains(&"rw2_missing_pixels_interpolated"), "{:?}", r.unapplied);

        // Hostile and unsupported inputs: typed, never guessed, refused before allocation.
        let one = plain4(400);
        assert_eq!(decode(&rw2(28, 1, 1, 12, Some(4), None, &one)).err(), Some(DevelopError::InvalidContainer), "truncated stream");
        assert_eq!(decode(&rw2(65_535, 4_096, 1, 12, Some(4), None, &one)).err(), Some(DevelopError::InvalidContainer), "aggregate over budget");
        assert_eq!(decode(&rw2(65_535, 65_535, 1, 12, Some(4), None, &one)).err(), Some(DevelopError::InvalidDimensions), "dims over the pixel cap");
        assert_eq!(decode(&rw2(0, 1, 1, 12, Some(4), None, &one)).err(), Some(DevelopError::InvalidDimensions));
        for (format, bits) in [(8, 12), (9, 12), (4, 14), (3, 12), (1, 12), (2, 16)] {
            assert_eq!(decode(&rw2(14, 1, 1, bits, Some(format), None, &one)).err(), Some(DevelopError::DecoderUnavailable("rw2")), "format {format}");
        }
        assert_eq!(decode(&rw2(14, 1, 1, 12, None, None, &one)).err(), Some(DevelopError::DecoderUnavailable("rw2")), "no format, no word compression");
        assert_eq!(decode(&rw2(14, 1, 9, 12, Some(4), None, &one)).err(), Some(DevelopError::UnsupportedPattern));
        assert_eq!(decode(&rw2(14, 1, 1, 12, Some(4), None, &[])).err(), Some(DevelopError::InvalidContainer), "no stream");
        assert_eq!(decode(&rw2(4, 1, 1, 12, None, Some(34_828), &[0; 6])).err(), Some(DevelopError::InvalidContainer), "short words");
    }

    /// Every RW2/RAW corpus file against its Adobe DNG Converter DNG (module `oracle`).
    #[test]
    #[ignore = "needs the oracle corpus (raw-vendor, raw-vendor-cleanroom, raw-dng-oracle)"]
    fn rw2_clean_room_matches_the_dng_converter_oracle_bit_exact() {
        let case = |raw, raw_sha256, dng, dng_sha256, origin| Case { raw, raw_sha256, dng, dng_sha256, origin };
        let cases = [
            case("raw-vendor/panasonic-dmc-lx7.rw2", "18dfc3877e944020299635a8c2fad61fca2485b1c9b67645a495ee870a86e599", "raw-dng-oracle/panasonic-dmc-lx7.dng", "c3373387d4a48cdc7dea2397056a2e3ba0baf31e119b2c0c5971b923b1094c6b", (0, 0)),
            case("raw-vendor-cleanroom/panasonic-dmc-lx3.rw2", "f2aeaa1cf9a1922a6b9bbe5d0bf1ba19d4c8988fefa320377a5e7461a31ffc23", "raw-dng-oracle/panasonic-dmc-lx3.dng", "4413caa1d86d0f132a586e78510b94b48ebe49d383f01c1edd3088a8c0fc2f42", (0, 0)),
            case("raw-vendor-cleanroom/panasonic-dc-g9.rw2", "16282357ce0143ff7d36737f82074662db7aa0790ecd309196f406baf0f586a7", "raw-dng-oracle/panasonic-dc-g9.dng", "81f9503856a4f14061ea5f7d59642501962c9e21e9edbf3f503a4966e9429e38", (0, 0)),
            case("raw-vendor-cleanroom/panasonic-dc-gh5m2.rw2", "0ea0efd2a9442d8c68508e4196e29f1da4577407dc3df5add4cf360f70493c7b", "raw-dng-oracle/panasonic-dc-gh5m2.dng", "b774b1ce3f6d94cea623f92fd41027e1504744f089e8aacc13f967d8d2af7bc9", (0, 0)),
            case("raw-vendor-cleanroom/panasonic-dc-s1.rw2", "45309a096d6010281e2fa310e6ec2fe1bcc02947fa37b1a2414cfc7377ce4ac6", "raw-dng-oracle/panasonic-dc-s1.dng", "b316e0a7de56753556fb57f57bbb0f540de6aa2785a0d78a30ae5b15727bbaa1", (0, 0)),
            case("raw-vendor-cleanroom/panasonic-dc-gh5s.rw2", "b617151876e62b492641cef14b8c98ee3797e5014d24c6af1c780715a501fa53", "raw-dng-oracle/panasonic-dc-gh5s.dng", "dcd5a17d5110d8bbfbcb38045902df5084867280672a93a841f83f0b70a7e0d9", (0, 0)),
            case("raw-vendor-cleanroom/panasonic-dmc-fz8.raw", "a11f1534f3092a4d5f2b57d182f6b06f8d26cf766eb463c3e02b26a338b63b8b", "raw-dng-oracle/panasonic-dmc-fz8.dng", "a520a0e3e479172d0b1c27166491135c60bd67d3a20a8a7a0704da1f24429985", (4, 0)),
            case("raw-vendor-cleanroom/panasonic-dmc-lx1.raw", "8ff982526d215d337706640cc6aa0822cccbe65a86ebe01daef0fd43cb45a4ca", "raw-dng-oracle/panasonic-dmc-lx1.dng", "58182186a1778ceb0b7f43e9c3c418138f7ed6db34d03542a4496160f2001e52", (6, 0)),
        ];
        let bad = compare("rw2_clean_room_oracle", &cases, |b| samples_of(decode(b)));
        assert!(bad.is_empty(), "{bad:#?}");
        // Format 8 (S5M2) stays a typed refusal (no layout derived).
        let s5m2 = "7480e44ed8557b71b1618e2d91c0f3eea58b60cb5a9196d2a1a552cfa509a1d2";
        if let Some(bytes) = read_pinned("rw2_clean_room_oracle", "raw-vendor-cleanroom/panasonic-dc-s5m2-12bit.rw2", s5m2) {
            assert_eq!(decode(&bytes).err(), Some(DevelopError::DecoderUnavailable("rw2")));
        }
    }
}
