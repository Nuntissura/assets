//! Exact media-time to Pulse-tick conversion with a stated rounding rule.
//!
//! `ticks = value * TICKS_PER_SECOND / timescale`. Exact when `timescale` divides
//! `TICKS_PER_SECOND` (2^12 * 3^4 * 5^6 * 7^2: 24000, 25, 30000, 44100, 48000, 90000 ... all divide).
//! Otherwise (e.g. 10_000_000 units/s) the result is rounded to nearest, ties toward +infinity, and the
//! conversion reports `exact == false` so receipts can carry the rounding.
use crate::error::ReelError;
use hsk_studio_accord::TICKS_PER_SECOND;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickValue {
    pub ticks: i64,
    pub exact: bool,
}

pub fn media_to_ticks(value: i64, timescale: u32) -> Result<TickValue, ReelError> {
    if timescale == 0 {
        return Err(ReelError::Corrupt {
            what: "zero-timescale",
        });
    }
    let scale = i128::from(timescale);
    let numerator = i128::from(value) * i128::from(TICKS_PER_SECOND);
    let quotient = numerator.div_euclid(scale);
    let remainder = numerator.rem_euclid(scale);
    let exact = remainder == 0;
    let rounded = if remainder * 2 >= scale {
        quotient + 1
    } else {
        quotient
    };
    let ticks =
        i64::try_from(rounded).map_err(|_| ReelError::LimitExceeded { what: "tick-range" })?;
    Ok(TickValue { ticks, exact })
}

/// Ticks per unit (frame or sample) of a `rate`-per-second stream, only when the division is exact.
pub fn exact_ticks_per_unit(rate: u32) -> Option<u64> {
    if rate == 0 || !TICKS_PER_SECOND.is_multiple_of(u64::from(rate)) {
        None
    } else {
        Some(TICKS_PER_SECOND / u64::from(rate))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_and_rounded_conversions() {
        assert_eq!(
            media_to_ticks(1001, 30000).unwrap(),
            TickValue {
                ticks: 8_475_667_200,
                exact: true
            }
        );
        assert_eq!(exact_ticks_per_unit(48000), Some(5_292_000));
        // 100 ns units do not divide the tick rate: 1 unit = 25401.6 ticks -> rounds to 25402.
        let t = media_to_ticks(1, 10_000_000).unwrap();
        assert_eq!((t.ticks, t.exact), (25_402, false));
        let n = media_to_ticks(-1, 10_000_000).unwrap();
        assert_eq!((n.ticks, n.exact), (-25_402, false));
    }
}
