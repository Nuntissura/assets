//! ProRes entropy decoding of scanned coefficients (SMPTE RDD 36 7.1.1): Golomb-Rice /
//! exponential-Golomb combination codes (7.1.1.1), the signed mapping (7.1.1.2, Table 8), DC
//! coefficients with the Table 9 adaptation (7.1.1.3) and AC run/level pairs with the Table 10 and
//! Table 11 adaptations (7.1.1.4), ending at `endOfData` (clause 5 conventions).
#![deny(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]
use crate::bitreader::BitReader;
use crate::error::ReelError;

/// Longest code level (prefix of `0` bits) accepted. Every valid symbol of a scanned coefficient array
/// fits far below this (the largest level of a 12-bit-range coefficient needs a level of about 12).
const MAX_CODE_LEVEL: u32 = 31;

fn corrupt() -> ReelError {
    ReelError::Corrupt {
        what: "prores-entropy",
    }
}

/// A codebook: Golomb-Rice / exponential-Golomb combination (lastRiceQ, kRice, kExp), or a pure
/// exponential-Golomb code of order k (`Exp(k)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Code {
    Exp(u32),
    Combo(u32, u32, u32),
}

/// Count the `0` bits before the first `1` and consume them and the `1`.
fn code_level(r: &mut BitReader<'_>) -> Result<u32, ReelError> {
    let mut level = 0u32;
    loop {
        match r.read(1) {
            Some(1) => return Ok(level),
            Some(_) => {
                level = level.saturating_add(1);
                if level > MAX_CODE_LEVEL {
                    return Err(ReelError::LimitExceeded {
                        what: "prores-golomb",
                    });
                }
            }
            None => return Err(corrupt()),
        }
    }
}

/// Exponential-Golomb of order k after the code level `q` and separator were consumed: the suffix is
/// `q + k` bits and the symbol is `2^(q+k) + suffix - 2^k`.
fn exp_suffix(r: &mut BitReader<'_>, q: u32, k: u32) -> Result<u64, ReelError> {
    let bits = q.checked_add(k).ok_or_else(corrupt)?;
    let suffix = u64::from(r.read(bits).ok_or_else(corrupt)?);
    1u64.checked_shl(bits)
        .and_then(|top| top.checked_add(suffix))
        .and_then(|v| v.checked_sub(1u64.checked_shl(k)?))
        .ok_or_else(corrupt)
}

/// Decode one symbol (a nonnegative integer) with `code` (7.1.1.1).
pub(crate) fn decode(r: &mut BitReader<'_>, code: Code) -> Result<u64, ReelError> {
    match code {
        Code::Exp(k) => {
            let q = code_level(r)?;
            exp_suffix(r, q, k)
        }
        Code::Combo(last_rice_q, k_rice, k_exp) => {
            let q = code_level(r)?;
            if q <= last_rice_q {
                let rem = u64::from(r.read(k_rice).ok_or_else(corrupt)?);
                u64::from(q)
                    .checked_shl(k_rice)
                    .and_then(|v| v.checked_add(rem))
                    .ok_or_else(corrupt)
            } else {
                // Discard lastRiceQ + 1 prefix zeros: the rest is an order-kExp exponential-Golomb
                // codeword whose own level is q - (lastRiceQ + 1).
                let q_exp = q
                    .checked_sub(last_rice_q.checked_add(1).ok_or_else(corrupt)?)
                    .ok_or_else(corrupt)?;
                let base = u64::from(last_rice_q)
                    .checked_add(1)
                    .and_then(|v| v.checked_shl(k_rice))
                    .ok_or_else(corrupt)?;
                exp_suffix(r, q_exp, k_exp)?
                    .checked_add(base)
                    .ok_or_else(corrupt)
            }
        }
    }
}

/// Inverse of the signed integer-to-symbol mapping S(n) (Table 8).
pub(crate) fn unsigned_to_signed(symbol: u64) -> Result<i64, ReelError> {
    let half = i64::try_from(symbol >> 1).map_err(|_| corrupt())?;
    if symbol & 1 == 0 {
        Ok(half)
    } else {
        half.checked_add(1)
            .and_then(i64::checked_neg)
            .ok_or_else(corrupt)
    }
}

/// Table 9: codebook for `dc_coeff_difference` from |previousDCDiff|.
fn dc_code(previous: i64) -> Code {
    match previous.unsigned_abs() {
        0 => Code::Exp(0),
        1 => Code::Exp(1),
        2 => Code::Combo(1, 2, 3),
        _ => Code::Exp(3),
    }
}

/// Table 10: codebook for `run` from previousRun.
fn run_code(previous: u64) -> Code {
    match previous {
        0 | 1 => Code::Combo(2, 0, 1),
        2 | 3 => Code::Combo(1, 0, 1),
        4 => Code::Exp(0),
        5..=8 => Code::Combo(1, 1, 2),
        9..=14 => Code::Exp(1),
        _ => Code::Exp(2),
    }
}

/// Table 11: codebook for `abs_level_minus_1` from previousLevelSymbol.
fn level_code(previous: u64) -> Code {
    match previous {
        0 => Code::Combo(2, 0, 2),
        1 => Code::Combo(1, 0, 1),
        2 => Code::Combo(2, 0, 1),
        3 => Code::Exp(0),
        4..=7 => Code::Exp(1),
        _ => Code::Exp(2),
    }
}

/// `endOfData` (clause 5): 31 or fewer bits remain and all of them are 0.
fn end_of_data(r: &BitReader<'_>) -> bool {
    r.remaining() <= 31 && r.rest_is_zero()
}

/// Decode one scanned coefficient array (5.3.2) of `num_blocks` blocks into `out[..num_blocks * 64]`
/// (zeroed first). `data` is exactly the component's coded data (its `coded_size_of_*` bytes).
pub(crate) fn scanned_coefficients(
    data: &[u8],
    num_blocks: usize,
    out: &mut [i32],
) -> Result<(), ReelError> {
    let total = num_blocks.checked_mul(64).ok_or_else(corrupt)?;
    let out = out.get_mut(..total).ok_or_else(corrupt)?;
    out.fill(0);
    let mut r = BitReader::new(data);
    // DC (7.1.1.3).
    let first = unsigned_to_signed(decode(&mut r, Code::Exp(5))?)?;
    let mut dc = first;
    *out.first_mut().ok_or_else(corrupt)? = i32::try_from(dc).map_err(|_| corrupt())?;
    let mut previous_diff: i64 = 3;
    for n in 1..num_blocks {
        let magnitude = unsigned_to_signed(decode(&mut r, dc_code(previous_diff))?)?;
        let diff = if previous_diff >= 0 {
            magnitude
        } else {
            magnitude.checked_neg().ok_or_else(corrupt)?
        };
        dc = dc.checked_add(diff).ok_or_else(corrupt)?;
        previous_diff = diff;
        *out.get_mut(n).ok_or_else(corrupt)? = i32::try_from(dc).map_err(|_| corrupt())?;
    }
    // AC (7.1.1.4): run/level pairs until endOfData.
    let mut n = num_blocks;
    let mut previous_run: u64 = 4;
    let mut previous_level: u64 = 1;
    while !end_of_data(&r) {
        let run = decode(&mut r, run_code(previous_run))?;
        previous_run = run;
        n = usize::try_from(run)
            .ok()
            .and_then(|run| n.checked_add(run))
            .ok_or_else(corrupt)?;
        let symbol = decode(&mut r, level_code(previous_level))?;
        previous_level = symbol;
        let sign = r.read(1).ok_or_else(corrupt)?;
        let magnitude =
            i32::try_from(symbol.checked_add(1).ok_or_else(corrupt)?).map_err(|_| corrupt())?;
        let level = if sign == 1 {
            magnitude.checked_neg().ok_or_else(corrupt)?
        } else {
            magnitude
        };
        // A level beyond the array is corrupt (the array has numBlocks * 64 entries).
        *out.get_mut(n).ok_or_else(corrupt)? = level;
        n = n.checked_add(1).ok_or_else(corrupt)?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn bits(s: &str) -> Vec<u8> {
        let mut out = vec![0u8; s.len().div_ceil(8)];
        for (i, c) in s.chars().enumerate() {
            if c == '1' {
                out[i / 8] |= 0x80 >> (i % 8);
            }
        }
        out
    }

    #[test]
    fn golomb_codes_follow_7_1_1_1() {
        // Exp-Golomb order 0: 1 -> 0, 010 -> 1, 011 -> 2, 00100 -> 3.
        let data = bits("101001100100");
        let mut r = BitReader::new(&data);
        for want in [0, 1, 2, 3] {
            assert_eq!(decode(&mut r, Code::Exp(0)).unwrap(), want);
        }
        // Combination (1, 0, 1) (lastRiceQ 1, Rice 0, Exp 1): 1 -> 0, 01 -> 1; then 00 + exp1 "10" -> 0+2.
        let data = bits("101");
        let mut r = BitReader::new(&data);
        assert_eq!(decode(&mut r, Code::Combo(1, 0, 1)).unwrap(), 0);
        assert_eq!(decode(&mut r, Code::Combo(1, 0, 1)).unwrap(), 1);
        let data = bits("0010");
        let mut r = BitReader::new(&data);
        assert_eq!(decode(&mut r, Code::Combo(1, 0, 1)).unwrap(), 2);
        // Overlong prefix is a typed limit.
        let data = [0u8; 8];
        let mut r = BitReader::new(&data);
        assert_eq!(
            decode(&mut r, Code::Exp(0)).unwrap_err(),
            ReelError::LimitExceeded {
                what: "prores-golomb"
            }
        );
        assert_eq!(unsigned_to_signed(0).unwrap(), 0);
        assert_eq!(unsigned_to_signed(1).unwrap(), -1);
        assert_eq!(unsigned_to_signed(2).unwrap(), 1);
        assert_eq!(unsigned_to_signed(5).unwrap(), -3);
    }
}
