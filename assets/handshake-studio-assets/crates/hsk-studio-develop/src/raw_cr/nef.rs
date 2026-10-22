//! Nikon NEF, Huffman-coded CFA (TIFF Compression 34713) (clean room, lane SMB-RAW-CLEANROOM-2).
//!
//! Method: black-box study against an independent oracle, Adobe DNG Converter 18.7 (`-u -p0`,
//! uncompressed DNG), plus public tag documentation. The coding scheme was stated as a hypothesis
//! and the prefix codes were recovered by fitting the bit stream to the converter's stored CFA
//! planes; every rule below was accepted only when whole images decoded bit-exactly (evidence,
//! counts and probes in the lane notes, `builders/lanes/hsk-studio-raw-cleanroom-2.json`). No code
//! or output of any other RAW decoder was read or used.
//!
//! Public sources (facts only, no code):
//! - TIFF 6.0 and TIFF/EP: IFDs, SubIFDs (330), strips, CFARepeatPatternDim/CFAPattern.
//! - Exif 2.3 (CIPA DC-008): Exif IFD 0x8769, MakerNote 0x927C.
//! - Exiv2 maker-note page (https://exiv2.org/makernote.html, documentation prose): the Nikon
//!   (format 3) maker note is `Nikon\0`, a version, then a complete TIFF header at offset 10 whose
//!   offsets are relative to that header.
//! - ExifTool Nikon tag table (https://exiftool.org/TagNames/Nikon.html, documentation): 0x000C
//!   WB_RBLevels, 0x003D BlackLevel, 0x0093 NEFCompression, 0x0096 NEFLinearizationTable.
//!
//! Layout (derived; corpus files whose whole stored plane matched are named in the lane notes):
//! - Maker-note tag 0x0096: byte 0 = 0x46 (lossless) or 0x44 (lossy), byte 1 a sub-version, then
//!   four 16-bit seeds in the maker note's byte order (row parity x column parity), then, lossy
//!   only, a 16-bit count and that many 16-bit curve points.
//! - The strip is one MSB-first bit stream. Per sample, row by row: a prefix code gives the
//!   difference category s (0..=bits), then s bits: the difference itself when the top bit is 1,
//!   else that value minus 2^s - 1 (s = 0: difference 0). Prediction: columns 0 and 1 from the
//!   previous value of a vertical chain per (row parity, column) seeded by the 0x0096 seeds; every
//!   other column from the sample two columns to its left.
//! - Four fixed prefix codes (listed below), selected by lossless/lossy and 12/14 bit.
//! - Lossy samples are curve indices: a sample is the curve point at its index (count 683, the
//!   "type 1" table, equals the converter's LinearizationTable bit for bit); a 257-point curve
//!   ("type 2") is expanded by linear interpolation (UNVERIFIED: the converter's table for those
//!   files is a camera-table curve that is not in the file, see the lane notes).
// V4-01: this module decodes untrusted bytes; every integer operation is checked or saturating.
#![deny(clippy::arithmetic_side_effects)]
use super::{EXIF_IFD, MAKER_NOTE, ascii_trimmed};
use crate::DevelopError;
use crate::camera_colour::VendorColour;
use crate::dng::{DngRaw, VendorFrame};
use crate::levels::{Region, measured_white, remaining};
use crate::tiffep::{self, Ifd, Tiff};

const FAMILY: &str = "nef_compressed";
const HUFFMAN: u32 = 34_713;
const PHOTOMETRIC_CFA: u32 = 32_803;
const NOTE_WB: u16 = 0x000c;
const NOTE_BLACK: u16 = 0x003d;
const NOTE_TABLE: u16 = 0x0096;

/// A prefix code as (code length, code, difference category) entries; a prefix not listed is
/// `InvalidContainer` (never guessed).
type Code = &'static [(u8, u16, u8)];

/// 12-bit lossless (D500, Z6 12-bit: whole frames; categories 10..12 also by a re-encoded probe).
const LOSSLESS_12: Code = &[
    (2, 0b00, 5), (3, 0b010, 4), (3, 0b011, 6), (3, 0b100, 3), (3, 0b101, 7), (4, 0b1100, 2), (4, 0b1101, 8),
    (5, 0b11100, 1), (5, 0b11101, 9), (5, 0b11110, 0), (6, 0b111110, 10), (7, 0b1111110, 11), (7, 0b1111111, 12),
];
/// 14-bit lossless (D3 14-bit: whole frame, every category).
const LOSSLESS_14: Code = &[
    (2, 0b00, 7), (3, 0b010, 6), (3, 0b011, 8), (3, 0b100, 5), (3, 0b101, 9), (4, 0b1100, 4), (4, 0b1101, 10),
    (5, 0b11100, 3), (5, 0b11101, 11), (6, 0b111100, 12), (6, 0b111101, 2), (6, 0b111110, 0), (7, 0b1111110, 1),
    (8, 0b11111110, 13), (8, 0b11111111, 14),
];
/// 12-bit lossy (D70s, D2H, D200 type 1, D3 12-bit type 2: whole frames). The two remaining codes
/// (`11111110`, `111111111`) are not categories 11/12 (re-encoded probes) and stay unknown.
const LOSSY_12: Code = &[
    (2, 0b00, 5), (3, 0b010, 4), (3, 0b011, 3), (3, 0b100, 6), (3, 0b101, 2), (3, 0b110, 7), (4, 0b1110, 1),
    (5, 0b11110, 0), (6, 0b111110, 8), (7, 0b1111110, 9), (9, 0b111111110, 10),
];
/// 14-bit lossy (D3 14-bit type 2, Z6 14-bit: whole frames). The last code (`111111111`) is
/// unknown (no file needs a category above 12).
const LOSSY_14: Code = &[
    (2, 0b00, 5), (3, 0b010, 6), (3, 0b011, 4), (3, 0b100, 7), (3, 0b101, 8), (4, 0b1100, 3), (4, 0b1101, 9),
    (4, 0b1110, 2), (5, 0b11110, 1), (6, 0b111110, 0), (7, 0b1111110, 10), (8, 0b11111110, 11), (9, 0b111111110, 12),
];

fn unavailable() -> DevelopError {
    DevelopError::DecoderUnavailable(FAMILY)
}

/// A 16-bit lookup: index = the next 16 stream bits, value = (code length, category); length 0 =
/// no code with that prefix.
fn lookup(code: Code) -> Result<Vec<(u8, u8)>, DevelopError> {
    let mut table = vec![(0u8, 0u8); 1 << 16];
    for &(len, bits, sym) in code {
        let free = 16u32.checked_sub(u32::from(len)).ok_or(DevelopError::Overflow)?;
        let start = u32::from(bits).checked_shl(free).ok_or(DevelopError::Overflow)?;
        let span = 1u32.checked_shl(free).ok_or(DevelopError::Overflow)?;
        for idx in start..start.saturating_add(span) {
            *table.get_mut(usize::try_from(idx).map_err(|_| DevelopError::Overflow)?).ok_or(DevelopError::Overflow)? = (len, sym);
        }
    }
    Ok(table)
}

/// MSB-first bit reader; reading past the end is `InvalidContainer` (truncated stream).
struct Bits<'a> {
    data: &'a [u8],
    pos: u64,
}

impl Bits<'_> {
    fn peek16(&self) -> u32 {
        let byte = usize::try_from(self.pos >> 3).unwrap_or(usize::MAX);
        let mut acc = 0u32;
        for k in 0..3usize {
            acc = (acc << 8) | u32::from(byte.checked_add(k).and_then(|i| self.data.get(i)).copied().unwrap_or(0));
        }
        (acc >> (8u64.saturating_sub(self.pos & 7))) & 0xffff
    }

    fn take(&mut self, n: u32) -> Result<u32, DevelopError> {
        let end = self.pos.checked_add(u64::from(n)).ok_or(DevelopError::Overflow)?;
        if end > (self.data.len() as u64).saturating_mul(8) {
            return Err(DevelopError::InvalidContainer);
        }
        let mut v = 0u32;
        for _ in 0..n {
            let byte = self.data[usize::try_from(self.pos >> 3).map_err(|_| DevelopError::Overflow)?];
            v = (v << 1) | u32::from((byte >> (7u64.saturating_sub(self.pos & 7))) & 1);
            self.pos = self.pos.saturating_add(1);
        }
        Ok(v)
    }
}

/// The raw CFA IFD: photometric CFA, Huffman compression, the largest such frame.
fn raw_ifd(tiff: &Tiff<'_>, ifds: &[Ifd]) -> Result<Option<Ifd>, DevelopError> {
    let mut best: Option<(u64, Ifd)> = None;
    for ifd in ifds {
        let get = |t: u16| ifd.find(t).map(|e| tiffep::first_u32(tiff, e)).transpose();
        if get(tiffep::PHOTOMETRIC)? != Some(PHOTOMETRIC_CFA) || get(tiffep::COMPRESSION)? != Some(HUFFMAN) {
            continue;
        }
        let px = u64::from(get(tiffep::WIDTH)?.unwrap_or(0)).saturating_mul(u64::from(get(tiffep::HEIGHT)?.unwrap_or(0)));
        if best.as_ref().is_none_or(|(b, _)| px > *b) {
            best = Some((px, ifd.clone()));
        }
    }
    Ok(best.map(|(_, i)| i))
}

/// True for a TIFF whose Make starts with NIKON and that holds a Huffman-coded CFA frame.
pub(crate) fn is_nef_huffman(bytes: &[u8]) -> bool {
    let Ok((tiff, first)) = Tiff::parse(bytes) else { return false };
    let Ok(ifd0) = tiff.ifd(first) else { return false };
    let nikon = ascii_trimmed(&tiff, &ifd0, tiffep::MAKE).ok().flatten().is_some_and(|m| m.to_ascii_uppercase().starts_with("NIKON"));
    nikon && ifd0.find(tiffep::DNG_VERSION).is_none() && tiff.walk(first).ok().and_then(|ifds| raw_ifd(&tiff, &ifds).ok().flatten()).is_some()
}

/// The maker note's embedded TIFF (`Nikon\0`, 4 bytes, TIFF header at offset 10).
fn maker_note<'a>(tiff: &Tiff<'a>, ifd0: &Ifd) -> Option<&'a [u8]> {
    let exif = tiff.ifd(tiffep::first_u32(tiff, ifd0.find(EXIF_IFD)?).ok()?).ok()?;
    let note = tiff.bytes(exif.find(MAKER_NOTE)?).ok()?;
    note.starts_with(b"Nikon\0").then(|| note.get(10..)).flatten()
}

struct Table {
    lossless: bool,
    seeds: [u32; 4],
    curve: Vec<u16>,
}

fn u16_at(raw: &[u8], at: usize, big: bool) -> Option<u32> {
    let b = raw.get(at..at.checked_add(2)?)?;
    Some(u32::from(if big { u16::from_be_bytes([b[0], b[1]]) } else { u16::from_le_bytes([b[0], b[1]]) }))
}

/// Maker-note tag 0x0096 (see the module header). Lossy curves: 683 points are the table as is;
/// 257 points are expanded linearly over 2^bits outputs (UNVERIFIED expansion).
fn table(raw: &[u8], big: bool, bits: u16) -> Result<Table, DevelopError> {
    let lossless = match raw.first() {
        Some(0x46) => true,
        Some(0x44) => false,
        _ => return Err(DevelopError::UnsupportedCompression),
    };
    let mut seeds = [0u32; 4];
    for (k, s) in seeds.iter_mut().enumerate() {
        *s = u16_at(raw, k.saturating_mul(2).saturating_add(2), big).ok_or(DevelopError::InvalidContainer)?;
    }
    let mut curve = Vec::new();
    if !lossless {
        let n = usize::try_from(u16_at(raw, 10, big).ok_or(DevelopError::InvalidContainer)?).map_err(|_| DevelopError::Overflow)?;
        let points: Vec<u16> = (0..n)
            .map(|k| u16_at(raw, k.saturating_mul(2).saturating_add(12), big).and_then(|v| u16::try_from(v).ok()))
            .collect::<Option<_>>()
            .ok_or(DevelopError::InvalidContainer)?;
        curve = match n {
            257 => {
                // Points at a fixed index step: 2^bits / 256 for sub-version 0x20 (D3: the points
                // saturate exactly at the largest stored index, 688 at 12 bit, 2752 at 14), 16 for
                // 0x40 (Z6 14-bit: 12-bit indices). Linear between points (UNVERIFIED).
                let full = VendorFrame::full_scale(bits).ok_or_else(unavailable)?;
                let step = if raw.get(1) == Some(&0x40) { 16 } else { full.saturating_add(1) >> 8 };
                if step == 0 {
                    return Err(unavailable());
                }
                let len = step.saturating_mul(256);
                let mut c = Vec::with_capacity(usize::try_from(len.saturating_add(1)).map_err(|_| DevelopError::Overflow)?);
                for x in 0..=len {
                    let i = usize::try_from(x.checked_div(step).ok_or(DevelopError::Overflow)?).map_err(|_| DevelopError::Overflow)?;
                    let a = u32::from(*points.get(i).ok_or(DevelopError::Overflow)?);
                    let b = u32::from(*points.get(i.saturating_add(1)).unwrap_or(&points[i]));
                    let f = x.checked_rem(step).ok_or(DevelopError::Overflow)?;
                    let v = if b >= a {
                        a.saturating_add(b.saturating_sub(a).saturating_mul(f).checked_div(step).ok_or(DevelopError::Overflow)?)
                    } else {
                        a.saturating_sub(a.saturating_sub(b).saturating_mul(f).checked_div(step).ok_or(DevelopError::Overflow)?)
                    };
                    c.push(u16::try_from(v.min(full)).map_err(|_| DevelopError::Overflow)?);
                }
                c
            }
            2..=16_384 => points,
            _ => return Err(unavailable()),
        };
    }
    Ok(Table { lossless, seeds, curve })
}

/// The file's stored plane: width, height and the decoded values (lossy: curve indices, before
/// the curve), for the oracle parity test.
#[cfg(test)]
pub(crate) fn plane(bytes: &[u8]) -> Result<(u32, u32, Vec<u16>), DevelopError> {
    let p = parse(bytes)?;
    Ok((p.width, p.height, p.codes))
}

struct Parsed {
    width: u32,
    height: u32,
    bits: u16,
    cfa_pattern: crate::pipeline::CfaPattern,
    table: Table,
    codes: Vec<u16>,
    black: Option<Vec<u32>>,
    wb: Option<Vec<f64>>,
}

fn parse(bytes: &[u8]) -> Result<Parsed, DevelopError> {
    let (tiff, first) = Tiff::parse(bytes)?;
    let ifd0 = tiff.ifd(first)?;
    let ifds = tiff.walk(first)?;
    let raw = raw_ifd(&tiff, &ifds)?.ok_or(DevelopError::InvalidContainer)?;
    let (width, height, bits, _) = tiffep::geometry(&tiff, &raw)?;
    if !matches!(bits, 12 | 14) {
        return Err(DevelopError::UnsupportedCompression);
    }
    let cfa_pattern = tiffep::cfa_pattern(&tiff, &raw)?;
    let note_bytes = maker_note(&tiff, &ifd0).ok_or_else(unavailable)?;
    let (note, note_first) = Tiff::parse(note_bytes).map_err(|_| unavailable())?;
    let note_ifd = note.ifd(note_first).map_err(|_| unavailable())?;
    let table = table(note.bytes(note_ifd.find(NOTE_TABLE).ok_or_else(unavailable)?)?, note.is_big_endian(), bits)?;
    let code = match (table.lossless, bits) {
        (true, 12) => LOSSLESS_12,
        (true, _) => LOSSLESS_14,
        (false, 12) => LOSSY_12,
        (false, _) => LOSSY_14,
    };
    let offset = tiffep::first_u32(&tiff, raw.find(tiffep::STRIP_OFFSETS).ok_or(DevelopError::InvalidContainer)?)?;
    let count = tiffep::first_u32(&tiff, raw.find(tiffep::STRIP_BYTE_COUNTS).ok_or(DevelopError::InvalidContainer)?)?;
    let stream = tiff.region(u64::from(offset), u64::from(count))?;
    // V4-02: a sample costs at least 2 bits (the shortest code), so a stream shorter than
    // width * height / 4 bytes cannot hold the frame; refused before the allocation.
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if (stream.len() as u64).saturating_mul(4) < pixels {
        return Err(DevelopError::InvalidContainer);
    }
    let codes = huffman(stream, code, &table, width, height, bits)?;
    let black = note_ifd.find(NOTE_BLACK).and_then(|e| note.u32s(e).ok());
    let wb = note_ifd.find(NOTE_WB).and_then(|e| note.f64s(e).ok());
    Ok(Parsed { width, height, bits, cfa_pattern, table, codes, black, wb })
}

pub(crate) fn decode(bytes: &[u8]) -> Result<DngRaw, DevelopError> {
    let Parsed { width, height, bits, cfa_pattern, table, codes, black: note_black, wb } = parse(bytes)?;
    let samples = if table.lossless {
        codes
    } else {
        // A lossy index past the curve takes its last point (as a DNG LinearizationTable does; D3
        // 12-bit type 2 stores indices up to 688 against the converter's 683 table entries).
        let last = table.curve.len().saturating_sub(1);
        codes.iter().map(|c| table.curve.get(usize::from(*c).min(last)).copied().ok_or(DevelopError::InvalidContainer)).collect::<Result<_, _>>()?
    };
    let (tiff, first) = Tiff::parse(bytes)?;
    let ifd0 = tiff.ifd(first)?;
    let mut applied: Vec<&str> = Vec::new();
    let mut black = vec![0.0];
    let mut black_repeat = (1, 1);
    // No 0x003D: black 0 (the converter's DNG BlackLevel is 0 for every corpus file without it:
    // D70s, D2H, D200, D3 x3). 0x003D: four levels in 14-bit units, a 12-bit frame divides by 4
    // (D500 400 -> DNG 100, Z6 14-bit 1008 -> 1008).
    match note_black {
        None => applied.push("nef_black_level_unread"),
        Some(levels) if levels.len() == 4 => {
            let scale = if bits == 12 { 4.0 } else { 1.0 };
            black = levels.iter().map(|v| f64::from(*v) / scale).collect();
            black_repeat = (2, 2);
            applied.push("nef_black_level_unread");
        }
        Some(_) => {}
    }
    let full = VendorFrame::full_scale(bits).ok_or_else(unavailable)?;
    let white = if table.lossless {
        match measured_white(&samples, width, height, Region::whole(width, height), full) {
            Some(v) => {
                applied.push("nef_white_level_unread");
                v
            }
            None => full,
        }
    } else {
        // Lossy: the measured saturation below the curve's largest output, else that output (a
        // file fact; D70s/D2H/D200 curve maximum 4095 = the converter's WhiteLevel).
        applied.push("nef_white_level_unread");
        let top = table.curve.iter().copied().max().map(u32::from).filter(|v| *v > 0).unwrap_or(full);
        measured_white(&samples, width, height, Region::whole(width, height), top).unwrap_or(top)
    };
    let mut colour = VendorColour { make: ascii_trimmed(&tiff, &ifd0, tiffep::MAKE)?, ..VendorColour::default() };
    if let Some(wb) = wb
        && let [r, b, ..] = wb.as_slice()
        && let Some(n) = VendorColour::neutral_from_multipliers(*r, *b)
    {
        colour.as_shot_neutral = Some(n);
        colour.neutral_tag = Some("nikon_makernote_0x000c");
    }
    let mut names = vec!["nef_black_level_unread", "nef_white_level_unread"];
    if !table.lossless && table.curve.len() != 683 {
        names.push("nef_lossy_curve_interpolation_unverified");
    }
    DngRaw::from_vendor(
        VendorFrame {
            width,
            height,
            bits,
            cfa_pattern,
            unique_camera_model: ascii_trimmed(&tiff, &ifd0, tiffep::MODEL)?,
            unapplied: remaining(&names, &applied),
            samples,
            white_level: white,
            black_repeat,
            black_level: black,
            colour,
        },
        bytes,
    )
}

fn huffman(stream: &[u8], code: Code, table: &Table, width: u32, height: u32, bits: u16) -> Result<Vec<u16>, DevelopError> {
    let lut = lookup(code)?;
    let w = usize::try_from(width).map_err(|_| DevelopError::Overflow)?;
    let h = usize::try_from(height).map_err(|_| DevelopError::Overflow)?;
    let max = i64::from(VendorFrame::full_scale(bits).ok_or_else(unavailable)?);
    let mut out = vec![0u16; w.checked_mul(h).ok_or(DevelopError::InvalidDimensions)?];
    let mut chain = [[0i64; 2]; 2];
    for (k, s) in table.seeds.iter().enumerate() {
        chain[k >> 1][k & 1] = i64::from(*s);
    }
    let mut b = Bits { data: stream, pos: 0 };
    let mut row_vals = vec![0i64; w];
    for y in 0..h {
        for x in 0..w {
            let pred = if x < 2 { chain[y & 1][x] } else { row_vals[x.saturating_sub(2)] };
            let (len, s) = lut[usize::try_from(b.peek16()).map_err(|_| DevelopError::Overflow)?];
            if len == 0 || u16::from(s) > bits {
                return Err(DevelopError::InvalidContainer);
            }
            b.take(u32::from(len))?;
            let s = u32::from(s);
            let raw = i64::from(b.take(s)?);
            let diff = if s == 0 {
                0
            } else if raw >> (s.saturating_sub(1)) != 0 {
                raw
            } else {
                raw.saturating_sub((1i64 << s).saturating_sub(1))
            };
            let v = pred.checked_add(diff).ok_or(DevelopError::Overflow)?;
            if !(0..=max).contains(&v) {
                return Err(DevelopError::InvalidContainer);
            }
            row_vals[x] = v;
            if x < 2 {
                chain[y & 1][x] = v;
            }
            let i = y.checked_mul(w).and_then(|r| r.checked_add(x)).ok_or(DevelopError::Overflow)?;
            out[i] = u16::try_from(v).map_err(|_| DevelopError::Overflow)?;
        }
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::{LOSSLESS_12, LOSSY_12, decode, plane};
    use crate::DevelopError;
    use crate::raw_cr::oracle::{Case, compare, dng_linearization, read_pinned};

    /// MSB-first bit writer.
    #[derive(Default)]
    struct W(Vec<u8>, u32, u64);
    impl W {
        fn put(&mut self, v: u32, n: u32) {
            for k in (0..n).rev() {
                self.2 = (self.2 << 1) | u64::from((v >> k) & 1);
                self.1 += 1;
                if self.1 == 8 {
                    self.0.push(self.2 as u8);
                    self.1 = 0;
                    self.2 = 0;
                }
            }
        }
        fn finish(mut self) -> Vec<u8> {
            while self.1 != 0 {
                self.put(0, 1);
            }
            self.0
        }
    }

    /// Encode `img` (row-major, `w` wide) with the module's predictor and the given code.
    fn encode(img: &[u32], w: usize, seeds: [u32; 4], code: super::Code) -> Vec<u8> {
        let mut out = W::default();
        let mut chain = [[seeds[0], seeds[1]], [seeds[2], seeds[3]]];
        for (i, &v) in img.iter().enumerate() {
            let (y, x) = (i / w, i % w);
            let pred = if x < 2 { chain[y & 1][x] } else { img[i - 2] };
            let diff = i64::from(v) - i64::from(pred);
            let s = 64 - diff.unsigned_abs().leading_zeros();
            let &(len, bits, _) = code.iter().find(|c| u32::from(c.2) == s).expect("category coded");
            out.put(u32::from(bits), u32::from(len));
            out.put((if diff >= 0 { diff } else { diff + (1 << s) - 1 }) as u32, s);
            if x < 2 {
                chain[y & 1][x] = v;
            }
        }
        out.finish()
    }

    fn be16(v: u32) -> [u8; 2] {
        (v as u16).to_be_bytes()
    }

    fn entry(tag: u16, ty: u16, count: u32, value: [u8; 4]) -> Vec<u8> {
        [tag.to_be_bytes().to_vec(), ty.to_be_bytes().to_vec(), count.to_be_bytes().to_vec(), value.to_vec()].concat()
    }

    /// A big-endian NEF: IFD0 is the CFA frame (RGGB, one strip at 8192), Make NIKON, an Exif IFD
    /// with a Nikon format-3 maker note whose IFD holds 0x0096 = `table`.
    fn nef(w: u32, h: u32, bits: u32, table: &[u8], stream: &[u8]) -> Vec<u8> {
        let long = |v: u32| v.to_be_bytes();
        let short = |v: u32| [be16(v)[0], be16(v)[1], 0, 0];
        let mut f = b"MM\0*\0\0\0\x08".to_vec();
        let ifd0 = [
            entry(256, 4, 1, long(w)),
            entry(257, 4, 1, long(h)),
            entry(258, 3, 1, short(bits)),
            entry(259, 3, 1, short(34_713)),
            entry(262, 3, 1, short(32_803)),
            entry(271, 2, 6, long(4096)),
            entry(273, 4, 1, long(8192)),
            entry(277, 3, 1, short(1)),
            entry(279, 4, 1, long(stream.len() as u32)),
            entry(33_421, 3, 2, [0, 2, 0, 2]),
            entry(33_422, 1, 4, [0, 1, 1, 2]),
            entry(34_665, 4, 1, long(1024)),
        ];
        f.extend((ifd0.len() as u16).to_be_bytes());
        ifd0.iter().for_each(|e| f.extend(e));
        f.extend(0u32.to_be_bytes());
        f.resize(1024, 0);
        // Exif IFD: one MakerNote entry, the note at 1100.
        let note_len = 10 + 8 + 2 + 12 + 4 + table.len();
        f.extend(1u16.to_be_bytes());
        f.extend(entry(0x927c, 7, note_len as u32, long(1100)));
        f.extend(0u32.to_be_bytes());
        f.resize(1100, 0);
        f.extend(b"Nikon\0\x02\x10\0\0MM\0*\0\0\0\x08");
        f.extend(1u16.to_be_bytes());
        f.extend(entry(0x0096, 7, table.len() as u32, long(8 + 2 + 12 + 4)));
        f.extend(0u32.to_be_bytes());
        f.extend(table);
        assert!(f.len() <= 4096, "maker note fits below the Make string");
        f.resize(4096, 0);
        f.extend(b"NIKON\0");
        f.resize(8192, 0);
        f.extend(stream);
        f
    }

    fn table(version: [u8; 2], seeds: u32, curve: &[u32]) -> Vec<u8> {
        let mut t = version.to_vec();
        (0..4).for_each(|_| t.extend(be16(seeds)));
        if !curve.is_empty() {
            t.extend(be16(curve.len() as u32));
            curve.iter().for_each(|v| t.extend(be16(*v)));
        }
        t
    }

    #[test]
    fn nef_clean_room_huffman_known_answers_and_hostile_inputs_are_typed() {
        // 12-bit lossless, 8 x 3: categories 0..=12 all appear (large steps along the rows).
        let img: Vec<u32> = vec![512, 520, 513, 4095, 0, 1, 2048, 3000, 2, 3, 1000, 1001, 5, 2100, 4000, 7, 900, 901, 902, 903, 64, 65, 4094, 1];
        let stream = encode(&img, 8, [512; 4], LOSSLESS_12);
        let file = nef(8, 3, 12, &table([0x46, 0x30], 512, &[]), &stream);
        let r = decode(&file).unwrap();
        let got: Vec<u32> = (0..3).flat_map(|y| (0..8).map(move |x| (x, y))).map(|(x, y)| u32::from(r.sample(x, y))).collect();
        assert_eq!(got, img);
        // 12-bit lossy: stored indices through a 683-point curve (index k -> 6 k).
        let curve: Vec<u32> = (0..683).map(|k| 6 * k).collect();
        let idx: Vec<u32> = vec![0, 682, 300, 1, 340, 341, 5, 600];
        let stream = encode(&idx, 4, [328; 4], LOSSY_12);
        let file = nef(4, 2, 12, &table([0x44, 0x10], 328, &curve), &stream);
        assert_eq!(plane(&file).unwrap().2, idx.iter().map(|v| *v as u16).collect::<Vec<_>>());
        let r = decode(&file).unwrap();
        assert_eq!((0..4).map(|x| r.sample(x, 0)).collect::<Vec<_>>(), [0, 4092, 1800, 6]);
        assert_eq!((r.white_level, r.black_level.clone()), (4092, vec![0.0]), "lossy white = the curve maximum; no 0x003D = black 0");

        // Hostile and unsupported inputs: typed, refused before the frame allocation.
        let good = encode(&img, 8, [512; 4], LOSSLESS_12);
        let lossless = table([0x46, 0x30], 512, &[]);
        assert_eq!(decode(&nef(8, 3, 12, &lossless, &good[..good.len() / 2])).err(), Some(DevelopError::InvalidContainer), "truncated stream");
        assert_eq!(decode(&nef(60_000, 4_000, 12, &lossless, &good)).err(), Some(DevelopError::InvalidContainer), "aggregate over budget");
        assert_eq!(decode(&nef(65_535, 65_535, 12, &lossless, &good)).err(), Some(DevelopError::InvalidDimensions), "dims over the pixel cap");
        // An unknown prefix: the lossy-12 code space has unassigned codes (11111110, 111111111).
        assert_eq!(decode(&nef(4, 2, 12, &table([0x44, 0x10], 328, &curve), &[0xff; 64])).err(), Some(DevelopError::InvalidContainer));
        // A value leaving 0..=4095 (seed 4095, difference +4095).
        let mut w = W::default();
        w.put(0b1111111, 7);
        w.put(4095, 12);
        let over = w.finish();
        assert_eq!(decode(&nef(1, 1, 12, &table([0x46, 0x30], 4095, &[]), &[over, vec![0; 8]].concat())).err(), Some(DevelopError::InvalidContainer));
        assert_eq!(decode(&nef(8, 3, 12, &table([0x47, 0x30], 512, &[]), &good)).err(), Some(DevelopError::UnsupportedCompression), "unknown 0x0096 version");
        assert_eq!(decode(&nef(8, 3, 10, &lossless, &good)).err(), Some(DevelopError::UnsupportedCompression), "10-bit");
        let mut no_note = nef(8, 3, 12, &lossless, &good);
        no_note[1100] = b'X';
        assert_eq!(decode(&no_note).err(), Some(DevelopError::DecoderUnavailable("nef_compressed")), "no Nikon maker note");
    }

    /// Every Huffman NEF corpus file against its Adobe DNG Converter DNG: the stored planes (lossy:
    /// curve indices) bit for bit; for 683-point curves also the decoded sample against the DNG
    /// LinearizationTable at the stored index.
    #[test]
    #[ignore = "needs the oracle corpus (raw-vendor, raw-vendor-cleanroom, raw-dng-oracle)"]
    fn nef_clean_room_matches_the_dng_converter_oracle_bit_exact() {
        let case = |raw, raw_sha256, dng, dng_sha256, origin| Case { raw, raw_sha256, dng, dng_sha256, origin };
        let cases = [
            case("raw-vendor/nikon-d70s-12bit-lossy-type1.nef", "6c46f9854033e7623ebbb5de8c951df6762ca44f05d45035ccc5cb7e9f4e5692", "raw-dng-oracle/nikon-d70s-12bit-lossy-type1.dng", "5af81faafc16411dc7f48ad6a3f4f7c1cab7b600ce73bf88bea62a19118a3946", (0, 0)),
            case("raw-vendor/nikon-z6-12bit-compressed.nef", "f0ab08da469ee205f2bc08efd9005778c64cdcb064893cee03768e32e5da1a36", "raw-dng-oracle/nikon-z6-12bit-compressed.dng", "3e7a5ac2bd4e23a57a9d996fd1dfa0b2a194fd3877878fe25ad129eff2ee63d9", (0, 0)),
            case("raw-vendor-cleanroom/nikon-d500-12bit-lossless.nef", "290ea97c0aa0f490cc1eca0bbfb68048f20219bd921682907d04a8bf8a6bcc70", "raw-dng-oracle/nikon-d500-12bit-lossless.dng", "3369a555209adfbe1c5c5148e8938a29ca24b8d33674fe73636f25f92ae69ace", (0, 0)),
            case("raw-vendor-cleanroom/nikon-z6-12bit-lossless.nef", "59615b65f8a7edc92a845b2a9c8ef313d6e7d7a5d842f94e6adf786da045a6db", "raw-dng-oracle/nikon-z6-12bit-lossless.dng", "d1fd7ff1bfbd83ded5a54edd6c9bb55e8b4582b68c47db1466cb96885aa62dde", (0, 0)),
            case("raw-vendor-cleanroom/nikon-d3-14bit-lossless.nef", "c280a2afc31fb15f8c0bc8d8cf6fb13b1972f7d913fa673c5db9d1c1f50beddd", "raw-dng-oracle/nikon-d3-14bit-lossless.dng", "07a2f00b7b4b36a73e1aa0c6edef4873bb502cb4d67578bc4a6a603599eada56", (2, 0)),
            case("raw-vendor-cleanroom/nikon-d3-12bit-lossy-type2.nef", "880c60c5f611adf6b70de3d099f8433492de3a3d96866d570122f17f0a651fc8", "raw-dng-oracle/nikon-d3-12bit-lossy-type2.dng", "7365d8dc5be49c97618c22feabb81f2a7e76bde3ba205fa8e21d32725ff78cc5", (2, 0)),
            case("raw-vendor-cleanroom/nikon-d3-14bit-lossy-type2.nef", "0626c09a58ca12ee4c12f919772abd4fd053c7d88c2f2b25e8d9be7c26cc7fd5", "raw-dng-oracle/nikon-d3-14bit-lossy-type2.dng", "3f1eac60f1e3d89c602a19f415fa6dafa83887dcbcf8aa487ede137cce1eb754", (2, 0)),
            case("raw-vendor-cleanroom/nikon-z6-14bit-compressed.nef", "8ca94584578b35a422744c1a610174ce37df0691588edfdecaec3111b3fbbb02", "raw-dng-oracle/nikon-z6-14bit-compressed.dng", "7d06b97611e91edf8cfe5b34a71b0fb38ed015ffce471c10846ae22f2d95c403", (0, 0)),
            case("raw-vendor-cleanroom/nikon-d2h-12bit-lossy-type1.nef", "155edb938f884ea7372ce98d4ff5f965c3e413b43b95bc9923da6e92082cf914", "raw-dng-oracle/nikon-d2h-12bit-lossy-type1.dng", "0a13703b3818f0aa6ea99a438163e04408b1b56e7d5307493046dc26b001e757", (6, 0)),
            case("raw-vendor-cleanroom/nikon-d200-12bit-lossy-type1.nef", "b0800a1166b952430b255600373573e1d528adf8de98857781e06a98472b3c35", "raw-dng-oracle/nikon-d200-12bit-lossy-type1.dng", "34ca47b163b87d0a68875d6f1b155356b4f54fd0afa7ce3aea3c26253f046864", (1, 0)),
        ];
        let bad = compare("nef_clean_room_oracle", &cases, plane);
        assert!(bad.is_empty(), "{bad:#?}");
        // Type-1 curves: the decoded sample is the converter's LinearizationTable at the stored index.
        for c in cases.iter().filter(|c| c.raw.contains("type1")) {
            let (Some(raw), Some(dng)) = (read_pinned("nef_clean_room_oracle", c.raw, c.raw_sha256), read_pinned("nef_clean_room_oracle", c.dng, c.dng_sha256)) else {
                continue;
            };
            let lin = dng_linearization(&dng).expect("type-1 DNG has a LinearizationTable");
            let (w, h, codes) = plane(&raw).unwrap();
            let r = decode(&raw).unwrap();
            for y in (0..h).step_by(97) {
                for x in 0..w {
                    let code = usize::from(codes[(y * w + x) as usize]);
                    assert_eq!(u32::from(r.sample(x, y)), lin[code.min(lin.len() - 1)], "{} ({x}, {y})", c.raw);
                }
            }
            println!("CURVE {} equal=true", c.raw);
        }
    }
}
