//! Thin adapters from container time to accord's `Timescale` (the single owner of timescale <-> tick
//! conversion). Nothing here does tick arithmetic: it only maps accord errors to `ReelError`.
use crate::error::ReelError;
use hsk_studio_accord::{Conversion, Rounding, Timescale};

/// `value` units at `timescale` units/second as signed ticks: nearest, an exact tie toward +infinity;
/// `exact == false` records the rounding.
pub(crate) fn media_to_ticks(value: i64, timescale: u32) -> Result<Conversion<i64>, ReelError> {
    Ok(Timescale::new(timescale)?.signed_ticks_from_units(value, Rounding::NearestHalfUp)?)
}

/// Ticks per unit (frame or sample) of a `rate`-per-second stream, only when the division is exact.
pub(crate) fn exact_ticks_per_unit(rate: u32) -> Option<u64> {
    Timescale::new(rate).ok()?.exact_ticks_per_unit()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_and_rounded_conversions() {
        let ntsc = media_to_ticks(1001, 30000).unwrap();
        assert_eq!((ntsc.value, ntsc.exact), (8_475_667_200, true));
        assert_eq!(exact_ticks_per_unit(48000), Some(5_292_000));
        // 100 ns units do not divide the tick rate: 1 unit = 25401.6 ticks -> rounds to 25402.
        let t = media_to_ticks(1, 10_000_000).unwrap();
        assert_eq!((t.value, t.exact), (25_402, false));
        let n = media_to_ticks(-1, 10_000_000).unwrap();
        assert_eq!((n.value, n.exact), (-25_402, false));
        assert_eq!(
            media_to_ticks(1, 0).unwrap_err(),
            ReelError::Corrupt {
                what: "zero-timescale"
            }
        );
        assert_eq!(
            media_to_ticks(i64::MAX, 1).unwrap_err(),
            ReelError::LimitExceeded { what: "tick-range" }
        );
    }
}
