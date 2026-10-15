//! Nikon Huffman-compressed NEF strips (TIFF Compression 34713): "lossless compressed" and "lossy
//! compressed" (type 1 full curve, type 2 257-point curve, incl. "lossy after split") at 12 and 14
//! bits.
//!
//! Adapted from LightCraft `crates/raw/src/vendor/nefc.rs` and the `Bits`/`Huffman`/`diff` helpers
//! of `crates/raw/src/vendor/pef.rs` (https://github.com/storytold/lightcraft at
//! af4c5e102c9f97529f582003b99c21b89067e906; Copyright (c) 2026 ArtCraft Team and the LightCraft
//! contributors; MIT OR Apache-2.0, licence checked in the repository NOTICE / LICENSE-MIT /
//! Cargo.toml; attribution in this crate's `NOTICE`). LightCraft is a clean-room project (it reads
//! no dcraw, LibRaw, rawspeed, rawloader/rawler, darktable or RawTherapee source); its Huffman
//! tables were recovered by black-box analysis of CC0 raw.pixls.us samples (same scene compressed
//! and uncompressed, beam search over code assignments) and verified on ~45 files of ~35 bodies;
//! its format facts come from the Clevy and Claff prose write-ups, ExifTool tag names and ITU-T
//! T.81 Annex F/H. No copyleft decoder source was read for this module.
//!
//! Format: one strip, MSB-first bit stream, no byte stuffing, continuous across rows. Per pixel a
//! canonical Huffman code gives the difference category `n`, then `n` additional bits (T.81: a
//! leading 0 means negative). Each pixel is predicted from the same-colour pixel two to the left;
//! the first two pixels of a row from the first two pixels of the previous row of the same parity,
//! seeded by maker note `0x0096` (NEFLinearizationTable: version bytes, four u16 seeds at offset 2,
//! curve size at 10, curve at 12, split row at 562, in the maker note's byte order). Lossless
//! (`0x46`): the decoded values are the samples. Lossy (`0x44 0x10`): they index the stored curve;
//! `0x44 0x20` / `0x44 0x40`: 257 points one every `2^bits/256` / `2^bits/1024` codes, linearly
//! interpolated. "Lossy after split": rows from the split row are fixed-rate words of `bits - 4`
//! bits (prefix category + kept top extra bits, bin-centre dequantised); the strip must end within
//! the last byte, otherwise the file is refused (never decoded wrongly).
//!
//! Changes against LightCraft: every arithmetic step is checked (V4-01) with develop's typed
//! errors; the size check runs before allocation (V4-02); an unseen code word is a typed refusal.
// V4-01: this module decodes untrusted bytes; every integer operation is checked or saturating.
#![deny(clippy::arithmetic_side_effects)]
use crate::DevelopError;

/// A code word whose symbol never occurred in the samples LightCraft analysed (refused as corrupt).
const UNSEEN: u8 = u8::MAX;

/// Canonical Huffman table: number of codes of length 1..=16 and the symbols in code order.
struct Table {
    counts: &'static [u8],
    symbols: &'static [u8],
}

const LOSSLESS_12: Table = Table { counts: &[0, 1, 4, 2, 3, 1, 2], symbols: &[5, 4, 6, 3, 7, 2, 8, 1, 9, 0, 10, 11, 12] };
const LOSSLESS_14: Table =
    Table { counts: &[0, 1, 4, 2, 2, 3, 1, 2], symbols: &[7, 6, 8, 5, 9, 4, 10, 3, 11, 12, 2, 0, 1, 13, 14] };
const LOSSY_12: Table = Table { counts: &[0, 1, 5, 1, 1, 1, 1, 1, 1], symbols: &[5, 4, 3, 6, 2, 7, 1, 0, 8, 9, UNSEEN, 10] };
const LOSSY_14: Table = Table { counts: &[0, 1, 4, 3, 1, 1, 1, 1, 1], symbols: &[5, 6, 4, 7, 8, 3, 9, 2, 1, 0, 10, 11, 12] };

/// 12-bit lookup Huffman decoder: indexed by the next 12 bits -> (symbol, length); length 0 = invalid.
struct Huffman {
    lut: Vec<(u8, u8)>,
}

impl Huffman {
    fn from_table(t: &Table) -> Result<Self, DevelopError> {
        let mut lut = vec![(0u8, 0u8); 4096];
        let mut code = 0u32;
        let mut symbols = t.symbols.iter();
        for (i, &n) in t.counts.iter().enumerate() {
            let len = u32::try_from(i).map_err(|_| DevelopError::Overflow)?.checked_add(1).ok_or(DevelopError::Overflow)?;
            if len > 12 {
                return Err(DevelopError::InvalidContainer);
            }
            for _ in 0..n {
                let sym = *symbols.next().ok_or(DevelopError::InvalidContainer)?;
                let shift = 12u32.checked_sub(len).ok_or(DevelopError::Overflow)?;
                let first = usize::try_from(code << shift).map_err(|_| DevelopError::Overflow)?;
                let span = 1usize << shift;
                let end = first.checked_add(span).ok_or(DevelopError::Overflow)?;
                let slots = lut.get_mut(first..end).ok_or(DevelopError::InvalidContainer)?;
                let entry = (sym, u8::try_from(len).map_err(|_| DevelopError::Overflow)?);
                slots.iter_mut().for_each(|s| *s = entry);
                code = code.checked_add(1).ok_or(DevelopError::Overflow)?;
            }
            code <<= 1;
        }
        Ok(Self { lut })
    }
}

/// MSB-first bit reader that yields zeros past the end (the caller bounds the overrun).
struct Bits<'a> {
    src: &'a [u8],
    pos: usize,
    acc: u64,
    n: u32,
}

impl<'a> Bits<'a> {
    fn new(src: &'a [u8]) -> Self {
        Self { src, pos: 0, acc: 0, n: 0 }
    }

    fn fill(&mut self) {
        while self.n <= 56 {
            let b = self.src.get(self.pos).copied().unwrap_or(0);
            self.pos = self.pos.saturating_add(1);
            self.acc |= u64::from(b) << 56u32.saturating_sub(self.n);
            self.n = self.n.saturating_add(8);
        }
    }

    /// The next `k` (1..=32) bits without consuming them.
    fn peek(&mut self, k: u32) -> u32 {
        if self.n < k {
            self.fill();
        }
        u32::try_from(self.acc >> 64u32.saturating_sub(k)).unwrap_or(0)
    }

    fn skip(&mut self, k: u32) {
        self.acc = self.acc.checked_shl(k).unwrap_or(0);
        self.n = self.n.saturating_sub(k);
    }

    fn get(&mut self, k: u32) -> u32 {
        if k == 0 {
            return 0;
        }
        let v = self.peek(k);
        self.skip(k);
        v
    }

    /// Bits handed out so far (may exceed the source length: zeros are read past its end).
    fn consumed_bits(&self) -> usize {
        self.pos.saturating_mul(8).saturating_sub(self.n as usize)
    }
}

/// One difference: Huffman category then T.81 additional bits; `None` on an invalid code.
fn diff(bits: &mut Bits<'_>, h: &Huffman) -> Option<i32> {
    let (sym, len) = *h.lut.get(bits.peek(12) as usize)?;
    if len == 0 || sym > 16 {
        return None;
    }
    bits.skip(u32::from(len));
    let s = u32::from(sym);
    if s == 0 {
        return Some(0);
    }
    let v = i32::try_from(bits.get(s)).ok()?;
    let half = 1i32 << s.checked_sub(1)?;
    Some(if v < half { v.checked_sub((1i32 << s).checked_sub(1)?)? } else { v })
}

/// How the decoded values map to samples.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Encoding {
    /// The decoded values are the samples (`0..2^bits`).
    Lossless,
    /// The decoded values index this curve.
    Lossy(Vec<u16>),
}

/// Maker note `0x0096` (NEF linearization table), parsed.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DecodeTable {
    pub encoding: Encoding,
    /// Predictor seeds `[row parity][column]`.
    pub seeds: [[i32; 2]; 2],
    /// First row of the fixed-rate section ("lossy after split"), 0 when the whole strip is Huffman.
    pub split: usize,
}

/// Parse maker note `0x0096` for a `bits`-bit image; `big_endian` is the maker note's byte order.
pub(crate) fn parse_table(t: &[u8], big_endian: bool, bits: u16) -> Result<DecodeTable, DevelopError> {
    let u16_at = |o: usize| -> Option<u16> {
        let b: [u8; 2] = t.get(o..o.checked_add(2)?)?.try_into().ok()?;
        Some(if big_endian { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) })
    };
    if bits != 12 && bits != 14 {
        return Err(DevelopError::UnsupportedCompression);
    }
    let short = || DevelopError::InvalidContainer;
    let (v0, v1) = (*t.first().ok_or_else(short)?, *t.get(1).ok_or_else(short)?);
    let mut seeds = [[0i32; 2]; 2];
    for (i, s) in seeds.iter_mut().flatten().enumerate() {
        let at = i.checked_mul(2).and_then(|o| o.checked_add(2)).ok_or(DevelopError::Overflow)?;
        *s = i32::from(u16_at(at).ok_or_else(short)?);
    }
    let mut split = 0usize;
    let encoding = match (v0, v1) {
        (0x46, _) => Encoding::Lossless,
        (0x44, 0x10 | 0x20 | 0x40) => {
            let n = usize::from(u16_at(10).ok_or_else(short)?);
            if n < 2 {
                return Err(DevelopError::InvalidContainer);
            }
            let points: Vec<u16> = (0..n)
                .map(|i| i.checked_mul(2).and_then(|o| o.checked_add(12)).and_then(u16_at))
                .collect::<Option<_>>()
                .ok_or_else(short)?;
            if v1 == 0x10 {
                Encoding::Lossy(points)
            } else {
                split = usize::from(u16_at(562).unwrap_or(0));
                // Type 2: `n` points over `range` codes, one every `range / (n - 1)`; 0x20 covers all
                // 2^bits codes, 0x40 the first quarter (LightCraft's measured span rule).
                let range = if v1 == 0x40 { 1usize << bits.saturating_sub(2) } else { 1usize << bits };
                let intervals = n.checked_sub(1).ok_or(DevelopError::Overflow)?;
                if intervals > range || range.checked_rem(intervals) != Some(0) {
                    return Err(DevelopError::UnsupportedCompression);
                }
                let step = u32::try_from(range.checked_div(intervals).ok_or(DevelopError::Overflow)?)
                    .map_err(|_| DevelopError::Overflow)?;
                let mut curve = Vec::with_capacity(range.checked_add(1).ok_or(DevelopError::Overflow)?);
                for pair in points.windows(2) {
                    let &[a, b] = pair else { continue };
                    let (a, b) = (u32::from(a), u32::from(b));
                    for k in 0..step {
                        let wa = a.checked_mul(step.checked_sub(k).ok_or(DevelopError::Overflow)?);
                        let wb = b.checked_mul(k);
                        let v = wa
                            .zip(wb)
                            .and_then(|(x, y)| x.checked_add(y))
                            .and_then(|s| s.checked_div(step))
                            .ok_or(DevelopError::Overflow)?;
                        curve.push(u16::try_from(v).map_err(|_| DevelopError::Overflow)?);
                    }
                }
                curve.extend(points.last().copied());
                Encoding::Lossy(curve)
            }
        }
        _ => return Err(DevelopError::UnsupportedCompression),
    };
    Ok(DecodeTable { encoding, seeds, split })
}

/// Fixed-rate section prefixes: `(prefix bits, prefix length, category n)`.
const FIXED_12: &[(u32, u32, u32)] = &[
    (0b00, 2, 9),
    (0b010, 3, 10),
    (0b011, 3, 8),
    (0b100, 3, 7),
    (0b101, 3, 6),
    (0b110, 3, 5),
    (0b1110, 4, 4),
    (0b11110, 5, 3),
    (0b111110, 6, 2),
    (0b1111110, 7, 1),
    (0b11111110, 8, 0),
];
const FIXED_14: &[(u32, u32, u32)] = &[
    (0b00, 2, 8),
    (0b010, 3, 12),
    (0b011, 3, 11),
    (0b100, 3, 10),
    (0b101, 3, 9),
    (0b110, 3, 7),
    (0b1110, 4, 6),
    (0b11110, 5, 5),
    (0b111110, 6, 4),
    (0b1111110, 7, 3),
    (0b11111110, 8, 2),
    (0b111111110, 9, 1),
    (0b1111111110, 10, 0),
];

/// Word (`bits - 4` bits) -> dequantised difference; `None` marks the unused all-ones word.
fn fixed_word_table(bits: u16) -> Vec<Option<i32>> {
    let (w, codes) = if bits == 12 { (8u32, FIXED_12) } else { (10u32, FIXED_14) };
    let mut lut = vec![None; 1usize << w];
    for &(prefix, len, n) in codes {
        let kept = w.saturating_sub(len);
        for m in 0..1u32 << kept {
            let e = n.saturating_sub(kept);
            let ex = i32::try_from(m << e).unwrap_or(i32::MAX);
            let half = (1i32 << e) >> 1;
            let full = 1i32 << n;
            let d = if n == 0 {
                0
            } else if ex >= 1i32 << n.saturating_sub(1) {
                ex.saturating_add(half)
            } else if e > 0 {
                ex.saturating_add(half).saturating_sub(full)
            } else {
                ex.saturating_sub(full.saturating_sub(1))
            };
            if let Some(slot) = lut.get_mut(((prefix << kept) | m) as usize) {
                *slot = Some(d);
            }
        }
    }
    lut
}

/// Decode a whole `w x h` Nikon Huffman-compressed strip with `bits`-bit samples. The size check
/// (every code word is at least 2 bits) runs before the frame is allocated (V4-02).
pub(crate) fn decode(src: &[u8], w: usize, h: usize, bits: u16, table: &DecodeTable) -> Result<Vec<u16>, DevelopError> {
    if bits != 12 && bits != 14 {
        return Err(DevelopError::UnsupportedCompression);
    }
    let n = w.checked_mul(h).filter(|&n| n > 0).ok_or(DevelopError::InvalidDimensions)?;
    if n.checked_div(4).unwrap_or(0) > src.len() {
        return Err(DevelopError::InvalidContainer);
    }
    if table.split != 0 && table.split >= h {
        return Err(DevelopError::InvalidContainer);
    }
    let huff = Huffman::from_table(match (&table.encoding, bits) {
        (Encoding::Lossless, 12) => &LOSSLESS_12,
        (Encoding::Lossless, _) => &LOSSLESS_14,
        (Encoding::Lossy(_), 12) => &LOSSY_12,
        (Encoding::Lossy(_), _) => &LOSSY_14,
    })?;
    let (lut, max): (&[u16], i32) = match &table.encoding {
        Encoding::Lossless => (&[], (1i32 << bits).saturating_sub(1)),
        Encoding::Lossy(curve) => {
            (curve, i32::try_from(curve.len()).map_err(|_| DevelopError::Overflow)?.saturating_sub(1))
        }
    };
    let fixed = if table.split != 0 { fixed_word_table(bits) } else { Vec::new() };
    let word = u32::from(bits).saturating_sub(4);
    let src_bits = src.len().checked_mul(8).ok_or(DevelopError::Overflow)?;
    let mut out = vec![0u16; n];
    let mut stream = Bits::new(src);
    let mut vpred = table.seeds;
    let mut clipped = 0usize;
    for (y, row) in out.chunks_exact_mut(w).enumerate() {
        let seeds = vpred.get_mut(y & 1).ok_or(DevelopError::InvalidContainer)?;
        let mut hpred = [0i32; 2];
        let fixed_row = table.split != 0 && y >= table.split;
        for (x, o) in row.iter_mut().enumerate() {
            let d = if fixed_row {
                fixed.get(stream.get(word) as usize).copied().flatten().ok_or(DevelopError::InvalidContainer)?
            } else {
                // An invalid or never-observed (`UNSEEN`) code word is corrupt data.
                diff(&mut stream, &huff).ok_or(DevelopError::InvalidContainer)?
            };
            // Columns 0/1 predict from the previous same-parity row's seed, the rest from two to the left.
            let pred = if x < 2 { seeds.get_mut(x) } else { hpred.get_mut(x & 1) }.ok_or(DevelopError::InvalidContainer)?;
            // A run of large differences in a corrupt stream must not overflow the predictor.
            let p = pred.checked_add(d).ok_or(DevelopError::InvalidContainer)?;
            *pred = p;
            if let Some(hp) = hpred.get_mut(x & 1) {
                *hp = p;
            }
            let v = p.clamp(0, max);
            if v != p && !fixed_row {
                clipped = clipped.saturating_add(1);
            }
            let v = usize::try_from(v).map_err(|_| DevelopError::Overflow)?;
            *o = if lut.is_empty() {
                u16::try_from(v).map_err(|_| DevelopError::Overflow)?
            } else {
                *lut.get(v).ok_or(DevelopError::InvalidContainer)?
            };
        }
        // The camera pads the strip; reading more than a few bytes past its end is truncation.
        if stream.consumed_bits() > src_bits.saturating_add(64) {
            return Err(DevelopError::InvalidContainer);
        }
    }
    if table.split != 0 {
        // The fixed-rate section fills the rest of the strip: whole rows of `w` words, < 8 bits of
        // padding (a strip may carry up to 4 extra rows); anything else is refused.
        let left = src_bits.checked_sub(stream.consumed_bits());
        let row_bits = w.checked_mul(word as usize).filter(|&b| b > 0).ok_or(DevelopError::Overflow)?;
        let fits = left.is_some_and(|l| l.checked_rem(row_bits).is_some_and(|r| r < 8) && l.checked_div(row_bits).is_some_and(|q| q <= 4));
        if !fits {
            return Err(DevelopError::UnsupportedCompression);
        }
    }
    // A wrong table or corrupt stream drifts out of range quickly; real files stay inside.
    if clipped > n.checked_div(1000).unwrap_or(0).saturating_add(16) {
        return Err(DevelopError::InvalidContainer);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_prefix_codes() {
        for t in [&LOSSLESS_12, &LOSSLESS_14, &LOSSY_12, &LOSSY_14] {
            assert_eq!(t.counts.iter().map(|&c| usize::from(c)).sum::<usize>(), t.symbols.len());
            assert!(Huffman::from_table(t).is_ok());
        }
    }
}
