//! Lossless JPEG encoder (ITU-T T.81 process SOF3, one component, Huffman, predictor 1, point transform 0)
//! for DNG compression 7. Written from the public T.81 text (Annex H lossless prediction, Annex K.2 code
//! length generation); the stream is read back by [`crate::ljpeg::decode`] in the tests and by the export's
//! own verification. The Huffman table is built per strip from the difference-category histogram.
//!
//! Hostile sizes (V4-01/02): dimensions are bounded before any buffer is sized, every index is checked.
#![deny(clippy::arithmetic_side_effects)]
use crate::DevelopError;

/// Largest strip (samples) one stream may carry.
pub(crate) const MAX_STREAM_SAMPLES: usize = 1 << 28;

fn bit_length(v: u32) -> u8 {
    u8::try_from(32u32.saturating_sub(v.leading_zeros())).unwrap_or(32)
}

/// T.81 K.2: optimal-ish length-limited (16) Huffman code sizes for symbols 0..=16 with frequencies `freq`.
/// Returns (counts per length 1..=16, symbols in code order). The reserved all-ones code is kept free by the
/// extra pseudo symbol 256.
fn build_table(freq17: &[u64; 17]) -> Result<([u8; 16], Vec<u8>), DevelopError> {
    let mut freq = [0u64; 257];
    freq[..17].copy_from_slice(freq17);
    freq[256] = 1;
    let mut codesize = [0u32; 257];
    let mut others = [-1i32; 257];
    loop {
        // c1: least frequent (largest index on ties), c2: next least frequent.
        let mut c1: Option<usize> = None;
        for (i, f) in freq.iter().enumerate() {
            if *f > 0 && c1.is_none_or(|c| *f <= freq[c]) {
                c1 = Some(i);
            }
        }
        let Some(mut c1) = c1 else { break };
        let mut c2: Option<usize> = None;
        for (i, f) in freq.iter().enumerate() {
            if *f > 0 && i != c1 && c2.is_none_or(|c| *f <= freq[c]) {
                c2 = Some(i);
            }
        }
        let Some(mut c2) = c2 else { break };
        freq[c1] = freq[c1].saturating_add(freq[c2]);
        freq[c2] = 0;
        loop {
            codesize[c1] = codesize[c1].saturating_add(1);
            match usize::try_from(others[c1]) {
                Ok(next) => c1 = next,
                Err(_) => break,
            }
        }
        others[c1] = i32::try_from(c2).map_err(|_| DevelopError::Overflow)?;
        loop {
            codesize[c2] = codesize[c2].saturating_add(1);
            match usize::try_from(others[c2]) {
                Ok(next) => c2 = next,
                Err(_) => break,
            }
        }
    }
    let mut bits = [0u32; 64];
    for size in codesize.iter().copied().filter(|s| *s > 0) {
        let slot = bits.get_mut(usize::try_from(size).map_err(|_| DevelopError::Overflow)?).ok_or(DevelopError::Overflow)?;
        *slot = slot.saturating_add(1);
    }
    // Limit to 16 bits (K.2 Figure K.3).
    let mut i = 63usize;
    while i > 16 {
        while bits[i] > 0 {
            let mut j = i.saturating_sub(2);
            while j > 0 && bits[j] == 0 {
                j = j.saturating_sub(1);
            }
            if j == 0 {
                return Err(DevelopError::Overflow);
            }
            bits[i] = bits[i].saturating_sub(2);
            bits[i.saturating_sub(1)] = bits[i.saturating_sub(1)].saturating_add(1);
            bits[j.saturating_add(1)] = bits[j.saturating_add(1)].saturating_add(2);
            bits[j] = bits[j].saturating_sub(1);
        }
        i = i.saturating_sub(1);
    }
    // Remove the reserved code from the longest length in use.
    let mut longest = 16usize;
    while longest > 0 && bits[longest] == 0 {
        longest = longest.saturating_sub(1);
    }
    if longest == 0 {
        return Err(DevelopError::Overflow);
    }
    bits[longest] = bits[longest].saturating_sub(1);
    let mut counts = [0u8; 16];
    for (len, slot) in (1usize..=16).zip(counts.iter_mut()) {
        *slot = u8::try_from(bits[len]).map_err(|_| DevelopError::Overflow)?;
    }
    // Symbols by ascending code size, then value (the pseudo symbol 256 is last and dropped).
    let mut symbols: Vec<u8> = Vec::new();
    for size in 1u32..=32 {
        for (sym, cs) in codesize.iter().enumerate().take(256) {
            if *cs == size {
                symbols.push(u8::try_from(sym).map_err(|_| DevelopError::Overflow)?);
            }
        }
    }
    let kept: usize = counts.iter().map(|c| usize::from(*c)).sum();
    symbols.truncate(kept);
    if symbols.len() != kept {
        return Err(DevelopError::Overflow);
    }
    Ok((counts, symbols))
}

struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    fn put(&mut self, value: u32, bits: u32) {
        if bits == 0 {
            return;
        }
        let mask = u32::MAX.checked_shr(32u32.saturating_sub(bits)).unwrap_or(u32::MAX);
        self.acc = self.acc.checked_shl(bits).unwrap_or(0) | u64::from(value & mask);
        self.n = self.n.saturating_add(bits);
        while self.n >= 8 {
            self.n = self.n.saturating_sub(8);
            let byte = (self.acc.checked_shr(self.n).unwrap_or(0) & 0xFF) as u8;
            self.out.push(byte);
            if byte == 0xFF {
                self.out.push(0);
            }
        }
        self.acc &= 1u64.checked_shl(self.n).unwrap_or(0).saturating_sub(1);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            let pad = 8u32.saturating_sub(self.n);
            self.put(1u32.checked_shl(pad).unwrap_or(0).saturating_sub(1), pad);
        }
        self.out
    }
}

fn marker(out: &mut Vec<u8>, code: u8, body: &[u8]) -> Result<(), DevelopError> {
    let len = u16::try_from(body.len().checked_add(2).ok_or(DevelopError::Overflow)?).map_err(|_| DevelopError::Overflow)?;
    out.extend_from_slice(&[0xFF, code]);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(body);
    Ok(())
}

/// Encode `rows` rows of `width` samples (row-major `samples`) of `precision` bits (2..=16) as one lossless
/// JPEG stream. Every sample must fit `precision` bits.
pub(crate) fn encode(samples: &[u16], width: u32, rows: u32, precision: u8) -> Result<Vec<u8>, DevelopError> {
    if !(2..=16).contains(&precision) || width == 0 || rows == 0 || width > 65_535 || rows > 65_535 {
        return Err(DevelopError::InvalidDimensions);
    }
    let w = width as usize;
    let h = rows as usize;
    let total = w.checked_mul(h).ok_or(DevelopError::Overflow)?;
    if total > MAX_STREAM_SAMPLES || samples.len() != total {
        return Err(DevelopError::InvalidDimensions);
    }
    let ceiling = u32::from(u16::MAX).checked_shr(u32::from(16u8.saturating_sub(precision))).ok_or(DevelopError::Overflow)?;
    if samples.iter().any(|v| u32::from(*v) > ceiling) {
        return Err(DevelopError::SampleOutOfRange);
    }
    let initial = 1i32.checked_shl(u32::from(precision).saturating_sub(1)).unwrap_or(0);
    // Differences modulo 2^16 as signed values, with their categories.
    let mut diffs: Vec<(u8, u16)> = Vec::with_capacity(total);
    let mut freq = [0u64; 17];
    for y in 0..h {
        for x in 0..w {
            let at = y.saturating_mul(w).saturating_add(x);
            let s = i32::from(samples[at]);
            let left = || i32::from(samples[at.saturating_sub(1)]);
            let prediction = if y == 0 {
                if x == 0 { initial } else { left() }
            } else if x == 0 {
                i32::from(samples[at.saturating_sub(w)])
            } else {
                left()
            };
            let mut d = s.wrapping_sub(prediction) & 0xFFFF;
            if d >= 0x8000 {
                d = d.wrapping_sub(0x1_0000);
            }
            let category = bit_length(d.unsigned_abs());
            let extra = if category == 0 || category == 16 {
                0u16
            } else if d >= 0 {
                d as u16
            } else {
                let span = 1i32.checked_shl(u32::from(category)).unwrap_or(0);
                d.wrapping_add(span).wrapping_sub(1) as u16
            };
            if let Some(f) = freq.get_mut(usize::from(category)) {
                *f = f.saturating_add(1);
            }
            diffs.push((category, extra));
        }
    }
    let (counts, symbols) = build_table(&freq)?;
    // Canonical codes (T.81 C.2), matching the decoder's construction.
    let mut code_of = [(0u32, 0u32); 17];
    let mut code = 0u32;
    let mut k = 0usize;
    for (len, count) in (1u32..=16).zip(counts.iter()) {
        for _ in 0..*count {
            let sym = usize::from(*symbols.get(k).ok_or(DevelopError::Overflow)?);
            if let Some(slot) = code_of.get_mut(sym) {
                *slot = (code, len);
            }
            code = code.saturating_add(1);
            k = k.saturating_add(1);
        }
        code = code.checked_shl(1).unwrap_or(0);
    }
    let mut out = vec![0xFF, 0xD8];
    let mut sof = vec![precision];
    sof.extend_from_slice(&(rows as u16).to_be_bytes());
    sof.extend_from_slice(&(width as u16).to_be_bytes());
    sof.extend_from_slice(&[1, 1, 0x11, 0]);
    marker(&mut out, 0xC3, &sof)?;
    let mut dht = vec![0x00];
    dht.extend_from_slice(&counts);
    dht.extend_from_slice(&symbols);
    marker(&mut out, 0xC4, &dht)?;
    marker(&mut out, 0xDA, &[1, 1, 0x00, 1, 0, 0])?;
    let mut writer = BitWriter { out: Vec::with_capacity(total / 2), acc: 0, n: 0 };
    for (category, extra) in diffs {
        let (code, len) = *code_of.get(usize::from(category)).ok_or(DevelopError::Overflow)?;
        if len == 0 {
            return Err(DevelopError::Overflow);
        }
        writer.put(code, len);
        // Category 16 (difference 32768) carries no extra bits (T.81 H.1.2.2).
        writer.put(u32::from(extra), if category == 16 { 0 } else { u32::from(category) });
    }
    out.extend_from_slice(&writer.finish());
    out.extend_from_slice(&[0xFF, 0xD9]);
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use crate::ljpeg;

    /// Deterministic xorshift; no external RNG.
    fn lcg(seed: &mut u64) -> u64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed
    }

    fn round_trip(samples: &[u16], w: u32, h: u32, precision: u8) -> usize {
        let stream = encode(samples, w, h, precision).expect("encode");
        let image = ljpeg::decode(&stream, samples.len()).expect("decode");
        assert_eq!((image.width, image.height, image.components, image.precision), (w, h, 1, precision));
        assert_eq!(image.samples, samples, "precision {precision} {w}x{h}");
        stream.len()
    }

    #[test]
    fn streams_decode_to_the_input_for_every_precision_and_shape() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        for precision in 2u8..=16 {
            let ceiling = u32::from(u16::MAX) >> (16 - precision);
            for (w, h) in [(1u32, 1u32), (1, 7), (7, 1), (13, 9), (64, 5)] {
                let n = (w * h) as usize;
                let noisy: Vec<u16> = (0..n).map(|_| (lcg(&mut seed) % u64::from(ceiling + 1)) as u16).collect();
                round_trip(&noisy, w, h, precision);
                round_trip(&vec![ceiling as u16; n], w, h, precision);
                round_trip(&vec![0u16; n], w, h, precision);
            }
        }
    }

    #[test]
    fn extreme_differences_use_the_widest_category() {
        // Alternating 0 / 65535 at 16 bits: |difference| 65535 wraps to -1 modulo 2^16, and +/-32768 hits category 16.
        let alternate: Vec<u16> = (0..64).map(|i| if i % 2 == 0 { 0 } else { u16::MAX }).collect();
        round_trip(&alternate, 8, 8, 16);
        let half: Vec<u16> = (0..64).map(|i| if i % 2 == 0 { 0 } else { 32768 }).collect();
        round_trip(&half, 8, 8, 16);
        // First sample against the mid-grey start value 32768.
        round_trip(&[0, 65535, 0, 65535], 2, 2, 16);
    }

    #[test]
    fn skewed_histograms_stay_within_sixteen_bit_codes() {
        // Geometric category counts push the K.2 code lengths to the limit; the reserved all-ones code stays free.
        let mut samples = Vec::new();
        let mut value = 0u16;
        for category in 0..16u32 {
            for _ in 0..(1u32 << (15 - category).min(14)) {
                samples.push(value);
            }
            value = value.wrapping_add((1u32 << category) as u16);
        }
        let w = samples.len() as u32;
        let size = round_trip(&samples, w, 1, 16);
        assert!(size > 0);
    }

    #[test]
    fn smooth_data_compresses_below_the_raw_size() {
        let (w, h) = (128u32, 64u32);
        let samples: Vec<u16> = (0..h).flat_map(|y| (0..w).map(move |x| (1000 + x * 3 + y * 5) as u16)).collect();
        let size = round_trip(&samples, w, h, 14);
        assert!(size < samples.len() * 2 / 2, "{size} bytes for {} samples", samples.len());
    }

    #[test]
    fn hostile_requests_are_typed_refusals() {
        let ok = vec![0u16; 4];
        assert_eq!(encode(&ok, 2, 2, 1).unwrap_err(), DevelopError::InvalidDimensions);
        assert_eq!(encode(&ok, 2, 2, 17).unwrap_err(), DevelopError::InvalidDimensions);
        assert_eq!(encode(&ok, 0, 2, 12).unwrap_err(), DevelopError::InvalidDimensions);
        assert_eq!(encode(&ok, 2, 0, 12).unwrap_err(), DevelopError::InvalidDimensions);
        assert_eq!(encode(&ok, 65_536, 1, 12).unwrap_err(), DevelopError::InvalidDimensions);
        assert_eq!(encode(&ok, 3, 2, 12).unwrap_err(), DevelopError::InvalidDimensions, "length mismatch");
        assert_eq!(encode(&[4096, 0, 0, 0], 2, 2, 12).unwrap_err(), DevelopError::SampleOutOfRange);
        // Declared dimensions are checked before any buffer is sized (no allocation for a lie).
        assert_eq!(encode(&[], 65_535, 65_535, 16).unwrap_err(), DevelopError::InvalidDimensions);
    }
}
