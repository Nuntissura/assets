//! Fujifilm striped predictive RAW compression (lossless and lossy; Bayer and X-Trans; 12/14/16 bit).
//!
//! Adapted from LightCraft `crates/raw/src/vendor/rafc.rs` (https://github.com/storytold/lightcraft at
//! af4c5e102c9f97529f582003b99c21b89067e906; MIT OR Apache-2.0; attribution in this crate's `NOTICE`).
//! LightCraft's stated clean-room basis: Fabian's prose description of lossless RAF compression
//! (https://capnfabs.net/posts/fuji-raf-compression-algorithm/, linked implementations not consulted)
//! for the broad structure; headers, colour-vector placement, predictors, contexts and adaptation
//! recovered from sample bytes and black-box comparisons of decoded sensor arrays (docs/raf-compression.md:
//! 24 files of 13 bodies matched an external decoder sample for sample). No copyleft decoder source
//! was read for this module.
//!
//! Stream: a 16-byte `IS` header (lossless flag, sensor type 0 Bayer / 16 X-Trans, bit depth,
//! height, padded width, width, 768-pixel stripe width, stripe count, six-row group count), a
//! big-endian stripe size table padded to 16 bytes, for lossy files one quantiser byte per group
//! per stripe (each table padded to 16 bytes), then independent per-stripe entropy streams.
//! Changes: serial stripes (no thread pool), checked/saturating arithmetic (V4-01), header and
//! table checks before any allocation (V4-02).
// V4-01: this module decodes untrusted bytes; every integer operation is checked or saturating.
#![deny(clippy::arithmetic_side_effects)]
use crate::DevelopError;
use crate::dng::MAX_DNG_PIXELS;

const BLOCK: usize = 768;
const XTRANS: [[usize; 6]; 6] =
    [[1, 1, 0, 1, 1, 2], [1, 1, 2, 1, 1, 0], [2, 0, 1, 0, 2, 1], [1, 1, 2, 1, 1, 0], [1, 1, 0, 1, 1, 2], [0, 2, 1, 2, 0, 1]];
const MASKS: [[u8; 2]; 6] = [[10, 15], [15, 10], [14, 10], [15, 11], [11, 15], [10, 14]];

fn corrupt() -> DevelopError {
    DevelopError::InvalidContainer
}

fn add(a: usize, b: usize) -> Result<usize, DevelopError> {
    a.checked_add(b).ok_or(DevelopError::Overflow)
}

fn mul(a: usize, b: usize) -> Result<usize, DevelopError> {
    a.checked_mul(b).ok_or(DevelopError::Overflow)
}

fn be16(src: &[u8], at: usize) -> Result<usize, DevelopError> {
    let b: [u8; 2] = src.get(at..add(at, 2)?).and_then(|s| s.try_into().ok()).ok_or_else(corrupt)?;
    Ok(usize::from(u16::from_be_bytes(b)))
}

struct Stripe<'a> {
    bytes: &'a [u8],
    quantizers: Option<&'a [u8]>,
    width: usize,
}

/// Decode a compressed strip of a `width x height` frame. The `IS` header must agree with the
/// container's dimensions, bit depth and CFA family.
pub(crate) fn decode(src: &[u8], width: usize, height: usize, bits: u32, xtrans: bool) -> Result<Vec<u16>, DevelopError> {
    let h = src.get(..16).ok_or_else(corrupt)?;
    if !h.starts_with(b"IS") {
        return Err(corrupt());
    }
    let flag = *h.get(2).ok_or_else(corrupt)?;
    if flag > 1 || *h.get(3).ok_or_else(corrupt)? != if xtrans { 16 } else { 0 } {
        return Err(corrupt());
    }
    if !matches!(bits, 12 | 14 | 16) {
        return Err(DevelopError::UnsupportedCompression);
    }
    let count = usize::from(*h.get(13).ok_or_else(corrupt)?);
    if u32::from(*h.get(4).ok_or_else(corrupt)?) != bits
        || be16(h, 5)? != height
        || be16(h, 9)? != width
        || count == 0
        || count != width.div_ceil(BLOCK)
        || be16(h, 7)? != mul(count, BLOCK)?
        || be16(h, 11)? != BLOCK
        || height == 0
        || !height.is_multiple_of(6)
        || be16(h, 14)? != height / 6
    {
        return Err(corrupt());
    }
    let n = mul(width, height)?;
    if n as u64 > MAX_DNG_PIXELS {
        return Err(DevelopError::InvalidDimensions);
    }
    let table_end = add(16, mul(count, 4)?)?;
    let mut offset = mul(table_end.div_ceil(16), 16)?;
    let quantizer_start = offset;
    let groups = height / 6;
    let quantizer_stride = mul(groups.div_ceil(16), 16)?;
    if flag == 0 {
        offset = add(offset, mul(quantizer_stride, count)?)?;
        if src.get(quantizer_start..offset).is_none() {
            return Err(corrupt());
        }
    }
    let mut stripes = Vec::with_capacity(count);
    let mut payload = 0usize;
    for i in 0..count {
        let at = add(16, mul(i, 4)?)?;
        let entry: [u8; 4] = src.get(at..add(at, 4)?).and_then(|s| s.try_into().ok()).ok_or_else(corrupt)?;
        let size = u32::from_be_bytes(entry) as usize;
        let end = add(offset, size)?;
        let bytes = src.get(offset..end).filter(|s| !s.is_empty()).ok_or_else(corrupt)?;
        payload = add(payload, size)?;
        let quantizers = if flag == 0 {
            let q = add(quantizer_start, mul(i, quantizer_stride)?)?;
            Some(src.get(q..add(q, groups)?).ok_or_else(corrupt)?)
        } else {
            None
        };
        let x0 = mul(i, BLOCK)?;
        stripes.push(Stripe { bytes, quantizers, width: width.checked_sub(x0).ok_or_else(corrupt)?.min(BLOCK) });
        offset = end;
    }
    // Every coded sample costs at least one bit: a strip far too short for the frame is refused
    // before the frame is allocated (V4-02; X-Trans interpolation slots cost nothing, hence / 16).
    if mul(payload, 16)? < n {
        return Err(corrupt());
    }
    let mut output = vec![0u16; n];
    for (i, stripe) in stripes.iter().enumerate() {
        let data = decode_stripe(stripe, height, bits, xtrans)?;
        let x0 = mul(i, BLOCK)?;
        for (row, samples) in output.chunks_exact_mut(width).zip(data.chunks_exact(stripe.width)) {
            row.get_mut(x0..add(x0, stripe.width)?).ok_or_else(corrupt)?.copy_from_slice(samples);
        }
    }
    Ok(output)
}

struct Bits<'a> {
    src: &'a [u8],
    pos: usize,
    acc: u64,
    available: u32,
}

impl<'a> Bits<'a> {
    fn new(src: &'a [u8]) -> Self {
        Self { src, pos: 0, acc: 0, available: 0 }
    }

    /// The next `n` (0..=32) bits, MSB first; reading past the end is truncation.
    fn get(&mut self, n: u32) -> Result<u32, DevelopError> {
        if n > 32 {
            return Err(corrupt());
        }
        while self.available < n {
            let b = *self.src.get(self.pos).ok_or_else(corrupt)?;
            self.pos = add(self.pos, 1)?;
            self.acc = (self.acc << 8) | u64::from(b);
            self.available = self.available.saturating_add(8);
        }
        self.available = self.available.saturating_sub(n);
        let mask = 1u64.checked_shl(n).unwrap_or(0).wrapping_sub(1);
        u32::try_from((self.acc >> self.available) & mask).map_err(|_| corrupt())
    }

    fn residual(&mut self, k: u32, bits: u32, range: u32, limit: u32) -> Result<i32, DevelopError> {
        let mut zeros = 0u32;
        while self.get(1)? == 0 {
            zeros = zeros.saturating_add(1);
            if zeros > limit {
                return Err(corrupt());
            }
        }
        let code = if zeros == limit {
            self.get(bits)?.checked_add(1).ok_or_else(corrupt)?
        } else {
            zeros.checked_shl(k).filter(|v| v >> k == zeros).ok_or_else(corrupt)? | self.get(k)?
        };
        if code > range {
            return Err(corrupt());
        }
        let half = i32::try_from(code / 2).map_err(|_| corrupt())?;
        Ok(if code & 1 == 0 { half } else { half.saturating_neg().saturating_sub(1) })
    }
}

#[derive(Clone, Copy)]
struct State {
    sum: u32,
    count: u32,
}

impl State {
    fn k(self) -> u32 {
        let mut k = 0u32;
        while k < 32 && self.count.checked_shl(k).is_some_and(|c| c < self.sum) {
            k = k.saturating_add(1);
        }
        k
    }

    fn update(&mut self, delta: i32) {
        self.sum = self.sum.saturating_add(delta.unsigned_abs());
        if self.count == 64 {
            self.sum >>= 1;
            self.count >>= 1;
        }
        self.count = self.count.saturating_add(1);
    }
}

fn quant(v: i32, near: u32) -> i32 {
    let t = [
        near.saturating_mul(3).saturating_add(18),
        near.saturating_mul(5).saturating_add(67),
        near.saturating_mul(7).saturating_add(276),
    ];
    let a = v.unsigned_abs();
    let bucket: i32 = if a <= near {
        0
    } else if a < t[0] {
        1
    } else if a < t[1] {
        2
    } else if a < t[2] {
        3
    } else {
        4
    };
    if v < 0 { bucket.saturating_neg() } else { bucket }
}

fn even_prediction(nw: i32, n: i32, ne: i32, nn: i32) -> i32 {
    let (a, b, c) = (nw.saturating_sub(n).saturating_abs(), ne.saturating_sub(n).saturating_abs(), nn.saturating_sub(n).saturating_abs());
    let pair = if a > b && a > c {
        ne.saturating_add(nn)
    } else if b > a && b > c {
        nw.saturating_add(nn)
    } else {
        nw.saturating_add(ne)
    };
    pair.saturating_add(n.saturating_mul(2)) >> 2
}

fn at(rows: &[Vec<i32>], y: usize, x: usize) -> Result<i32, DevelopError> {
    rows.get(y).and_then(|r| r.get(x)).copied().ok_or_else(corrupt)
}

fn decode_stripe(stripe: &Stripe<'_>, height: usize, bits: u32, xtrans: bool) -> Result<Vec<u16>, DevelopError> {
    let size = if xtrans { BLOCK * 2 / 3 } else { BLOCK / 2 };
    let width = add(size, 2)?;
    let mut previous = vec![vec![vec![0i32; width]; 2]; 3];
    let max = (1u32 << bits).saturating_sub(1);
    let range = |near: u32| -> u32 {
        max.saturating_add(near.saturating_mul(2)).checked_div(near.saturating_mul(2).saturating_add(1)).unwrap_or(0).saturating_add(1)
    };
    let seed = |near: u32| State { sum: (range(near).saturating_add(32) / 64).max(2), count: 1 };
    let mut states = [[[seed(0); 41]; 2]; 3];
    // Smooth regions keep finer precision and independent state across quantiser changes.
    let mut fine = [[[[seed(0); 5]; 2]; 3], [[[seed(1); 5]; 2]; 3], [[[seed(2); 5]; 2]; 3]];
    let mut last_near = None;
    let mut reader = Bits::new(stripe.bytes);
    let mut output = Vec::with_capacity(mul(height, stripe.width)?);
    let limit_base = bits.saturating_mul(4);
    for group in 0..height / 6 {
        let near = match stripe.quantizers {
            Some(q) => u32::from(*q.get(group).ok_or_else(corrupt)?),
            None => 0,
        };
        if last_near != Some(near) {
            states = [[[seed(near); 41]; 2]; 3];
            last_near = Some(near);
        }
        let mut rows = previous;
        for pair in 0..6usize {
            let colours = if pair % 2 == 0 { [0usize, 1] } else { [1, 2] };
            for &colour in &colours {
                let lines = rows.get_mut(colour).ok_or_else(corrupt)?;
                let prev = lines.len().checked_sub(1).ok_or_else(corrupt)?;
                let mut row = vec![0i32; width];
                if let Some(v) = row.first_mut() {
                    *v = at(lines, prev, 1)?;
                }
                let right = at(lines, prev, size)?;
                if let Some(v) = row.last_mut() {
                    *v = right;
                }
                lines.push(row);
            }
            for even in (0..add(size, 8)?).step_by(2) {
                for (odd, position) in [(0usize, Some(even)), (1, even.checked_sub(7))] {
                    let Some(pos) = position.filter(|&x| x < size) else { continue };
                    let x = add(pos, 1)?;
                    for (which, &colour) in colours.iter().enumerate() {
                        let lines = rows.get_mut(colour).ok_or_else(corrupt)?;
                        let y = lines.len().checked_sub(1).ok_or_else(corrupt)?;
                        let (y1, y2, xl, xr) = (y.checked_sub(1).ok_or_else(corrupt)?, y.checked_sub(2).ok_or_else(corrupt)?, pos, add(x, 1)?);
                        let (nw, north, ne, nn) = (at(lines, y1, xl)?, at(lines, y1, x)?, at(lines, y1, xr)?, at(lines, y2, x)?);
                        let (west, east) = (at(lines, y, xl)?, at(lines, y, xr)?);
                        let (predict, d1, d2) = if odd == 0 {
                            (even_prediction(nw, north, ne, nn), north.saturating_sub(nn), nw.saturating_sub(north))
                        } else {
                            let p = if (nw.min(ne)..=nw.max(ne)).contains(&north) {
                                west.saturating_add(east) >> 1
                            } else {
                                west.saturating_add(east).saturating_add(north.saturating_mul(2)) >> 2
                            };
                            (p, north.saturating_sub(nw), nw.saturating_sub(west))
                        };
                        let real = !xtrans || MASKS.get(pair).and_then(|m| m.get(which)).is_some_and(|m| (m >> (pos % 4)) & 1 != 0);
                        let value = if real {
                            let contrast = d1.unsigned_abs().saturating_add(d2.unsigned_abs());
                            let precision = match contrast {
                                0..=5 => 0,
                                6 => near.min(1),
                                7 => near.min(2),
                                _ => near,
                            };
                            let (context, state) = if precision < near {
                                let sign = |v: i32| if v.unsigned_abs() <= precision { 0 } else { v.signum() };
                                let context = sign(d1).saturating_mul(3).saturating_add(sign(d2));
                                let state = fine
                                    .get_mut(precision as usize)
                                    .and_then(|f| f.get_mut(pair % 3))
                                    .and_then(|s| s.get_mut(odd))
                                    .and_then(|s| s.get_mut(context.unsigned_abs() as usize))
                                    .ok_or_else(corrupt)?;
                                (context, state)
                            } else {
                                let context = quant(d1, near).saturating_mul(9).saturating_add(quant(d2, near));
                                let state = states
                                    .get_mut(pair % 3)
                                    .and_then(|s| s.get_mut(odd))
                                    .and_then(|s| s.get_mut(context.unsigned_abs() as usize))
                                    .ok_or_else(corrupt)?;
                                (context, state)
                            };
                            let coded_range = range(precision);
                            let coded_bits = 32u32.saturating_sub(coded_range.saturating_sub(1).leading_zeros());
                            let limit = limit_base.saturating_sub(coded_bits).saturating_sub(1);
                            let mut delta = reader.residual(state.k(), coded_bits, coded_range, limit)?;
                            state.update(delta);
                            if context < 0 {
                                delta = delta.saturating_neg();
                            }
                            let step = i32::try_from(precision.saturating_mul(2).saturating_add(1)).map_err(|_| corrupt())?;
                            let wrap = i32::try_from(coded_range).map_err(|_| corrupt())?.saturating_mul(step);
                            let mut value = predict.saturating_add(delta.saturating_mul(step));
                            let lo = i32::try_from(precision).map_err(|_| corrupt())?.saturating_neg();
                            let hi = i32::try_from(max.saturating_add(precision)).map_err(|_| corrupt())?;
                            if value < lo {
                                value = value.saturating_add(wrap);
                            } else if value > hi {
                                value = value.saturating_sub(wrap);
                            }
                            value.clamp(0, i32::try_from(max).map_err(|_| corrupt())?)
                        } else {
                            predict
                        };
                        *lines.get_mut(y).and_then(|r| r.get_mut(x)).ok_or_else(corrupt)? = value;
                    }
                }
            }
        }
        for y in 0..6usize {
            for x in 0..stripe.width {
                let colour = if xtrans {
                    *XTRANS.get(y).and_then(|r| r.get(x % 6)).ok_or_else(corrupt)?
                } else {
                    match (y % 2, x % 2) {
                        (0, 0) => 0,
                        (1, 1) => 2,
                        _ => 1,
                    }
                };
                let row = add(2, if colour == 1 { y } else { y / 2 })?;
                let col = add(1, if xtrans { add(mul(2, x / 3)?, (x % 3).min(1))? } else { x / 2 })?;
                let v = at(rows.get(colour).ok_or_else(corrupt)?, row, col)?;
                output.push(u16::try_from(v).map_err(|_| corrupt())?);
            }
        }
        previous = rows
            .into_iter()
            .map(|mut r| {
                let keep = r.len().saturating_sub(2);
                r.split_off(keep)
            })
            .collect();
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-computed residual codes (LightCraft test vector): ordinary codes 0, 1, 2, 3 with k=2,
    /// then the escape for -8192; and one even-column prediction.
    #[test]
    fn residual_sign_wrap_escape_and_prediction() {
        let mut stream = vec![true, false, false, true, false, true, true, true, false, true, true, true];
        stream.extend(std::iter::repeat_n(false, 41));
        stream.push(true);
        stream.extend((0..14).rev().map(|i| (16382u32 >> i) & 1 != 0));
        let bytes: Vec<u8> = stream.chunks(8).map(|c| c.iter().enumerate().fold(0, |b, (i, bit)| b | (u8::from(*bit) << (7 - i)))).collect();
        let mut reader = Bits::new(&bytes);
        for delta in [0, -1, 1, -2, -8192] {
            assert_eq!(reader.residual(2, 14, 16384, 41).unwrap(), delta);
        }
        assert_eq!(even_prediction(1112, 858, 1104, 1152), 983);
        assert!(Bits::new(&[0; 64]).residual(0, 14, 16384, 41).is_err());
    }
}
