//! Exact rational timescale <-> tick conversion, the single owner of "N units at R units/second"
//! becoming `Ticks` (STU-VID-012/013: rational time is an interchange value, stored authority is
//! integer ticks at [`TICKS_PER_SECOND`]).
//!
//! A [`Timescale`] is units per second as a reduced fraction (`90000`, `48000`, `10_000_000` for
//! 100 ns units, `30000/1001` for NTSC frames). `ticks = units * TICKS_PER_SECOND / units_per_second`
//! is computed in 128-bit integers, so a result is either exact or rounded under an explicit
//! [`Rounding`]; [`Conversion::exact`] says which, so callers can record a loss note.
//!
//! Errors reuse [`ValidationError`]: a zero timescale/rate is `OutOfRange`; a result (or a 128-bit
//! intermediate) that does not fit the target integer, including a negative result for unsigned
//! [`Ticks`], is `TickOverflow` (same convention as `Ticks::checked_sub`).
use crate::{FrameRate, Result, TICKS_PER_SECOND, Ticks, ValidationError};

/// How a value that falls between two whole target units is rounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rounding {
    /// Toward negative infinity.
    Floor,
    /// Toward positive infinity.
    Ceil,
    /// Nearest unit; an exact tie goes toward positive infinity (-2.5 -> -2, 2.5 -> 3).
    NearestHalfUp,
}

/// A converted value plus whether the source mapped onto a whole target unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Conversion<T> {
    pub value: T,
    /// `false` = the [`Rounding`] changed the value (record a rounding loss).
    pub exact: bool,
}

/// Units per second as a reduced fraction `num/den`, both non-zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Timescale {
    num: u64,
    den: u64,
}

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn overflow() -> ValidationError {
    ValidationError::TickOverflow
}

/// `round(value * mul / div)` with `div > 0`; returns the rounded value and whether it was whole.
fn scale(value: i128, mul: u128, div: u128, rounding: Rounding) -> Result<(i128, bool)> {
    let numerator = i128::try_from(mul)
        .ok()
        .and_then(|m| value.checked_mul(m))
        .ok_or_else(overflow)?;
    let div = i128::try_from(div).map_err(|_| overflow())?;
    let (floor, rem) = (numerator.div_euclid(div), numerator.rem_euclid(div));
    if rem == 0 {
        return Ok((floor, true));
    }
    let up = match rounding {
        Rounding::Floor => false,
        Rounding::Ceil => true,
        Rounding::NearestHalfUp => rem >= div - rem,
    };
    let rounded = if up {
        floor.checked_add(1).ok_or_else(overflow)?
    } else {
        floor
    };
    Ok((rounded, false))
}

fn unsigned(c: (i128, bool)) -> Result<Conversion<u64>> {
    Ok(Conversion {
        value: u64::try_from(c.0).map_err(|_| overflow())?,
        exact: c.1,
    })
}

fn signed(c: (i128, bool)) -> Result<Conversion<i64>> {
    Ok(Conversion {
        value: i64::try_from(c.0).map_err(|_| overflow())?,
        exact: c.1,
    })
}

impl Timescale {
    /// Integer units per second (media timescale, sample rate in Hz, 10_000_000 for 100 ns).
    pub fn new(units_per_second: u32) -> Result<Self> {
        Self::rational(u64::from(units_per_second), 1)
    }

    /// `num/den` units per second (e.g. `30000/1001` frames per second), reduced.
    pub fn rational(num: u64, den: u64) -> Result<Self> {
        if num == 0 || den == 0 {
            return Err(ValidationError::OutOfRange);
        }
        let g = gcd(u128::from(num), u128::from(den));
        Ok(Self {
            num: u64::try_from(u128::from(num) / g).map_err(|_| overflow())?,
            den: u64::try_from(u128::from(den) / g).map_err(|_| overflow())?,
        })
    }

    /// Reduced `(num, den)` units per second.
    pub const fn units_per_second(self) -> (u64, u64) {
        (self.num, self.den)
    }

    /// Reduced `(ticks, units)` with `ticks / units` ticks per unit.
    fn ratio(self) -> (u128, u128) {
        let ticks = u128::from(TICKS_PER_SECOND) * u128::from(self.den);
        let units = u128::from(self.num);
        let g = gcd(ticks, units);
        (ticks / g, units / g)
    }

    /// Ticks per unit when that is a whole number (48000 -> 5_292_000, 30000/1001 -> 8_475_667_200),
    /// else `None` (a 100 ns unit is 25401.6 ticks).
    pub fn exact_ticks_per_unit(self) -> Option<u64> {
        let (ticks, units) = self.ratio();
        if units == 1 {
            u64::try_from(ticks).ok()
        } else {
            None
        }
    }

    /// Non-negative `units` of this timescale to ticks.
    pub fn ticks_from_units(self, units: u64, rounding: Rounding) -> Result<Conversion<Ticks>> {
        let (ticks, per) = self.ratio();
        let c = unsigned(scale(i128::from(units), ticks, per, rounding)?)?;
        Ok(Conversion {
            value: Ticks::new(c.value),
            exact: c.exact,
        })
    }

    /// Signed `units` to signed ticks (`i64` ticks, as stored by media indexes with negative offsets).
    pub fn signed_ticks_from_units(
        self,
        units: i64,
        rounding: Rounding,
    ) -> Result<Conversion<i64>> {
        let (ticks, per) = self.ratio();
        signed(scale(i128::from(units), ticks, per, rounding)?)
    }

    /// Ticks to whole units of this timescale (`Floor` = units fully elapsed).
    pub fn units_from_ticks(self, ticks: Ticks, rounding: Rounding) -> Result<Conversion<u64>> {
        let (per, units) = self.ratio();
        unsigned(scale(i128::from(ticks.value()), units, per, rounding)?)
    }

    /// Signed ticks to signed units.
    pub fn signed_units_from_ticks(
        self,
        ticks: i64,
        rounding: Rounding,
    ) -> Result<Conversion<i64>> {
        let (per, units) = self.ratio();
        signed(scale(i128::from(ticks), units, per, rounding)?)
    }

    /// `value` units of `self` as units of `to`, with no tick intermediate (so no extra rounding):
    /// `value * to / self`, e.g. an MP4 edit `media_time` in media-timescale units to PCM frames.
    pub fn rescale(self, value: i64, to: Timescale, rounding: Rounding) -> Result<Conversion<i64>> {
        let mul = u128::from(to.num) * u128::from(self.den);
        let div = u128::from(to.den) * u128::from(self.num);
        let g = gcd(mul, div);
        signed(scale(i128::from(value), mul / g, div / g, rounding)?)
    }
}

impl Ticks {
    /// `value` units at an integer `timescale` (units/second) to ticks; see [`Timescale`].
    /// 90000 Hz and 48000 Hz are exact; `10_000_000` (100 ns) is not and reports `exact == false`.
    pub fn from_timescale(
        value: u64,
        timescale: u32,
        rounding: Rounding,
    ) -> Result<Conversion<Ticks>> {
        Timescale::new(timescale)?.ticks_from_units(value, rounding)
    }

    /// Inverse of [`Ticks::from_timescale`]: these ticks as units of an integer `timescale`.
    pub fn to_timescale(self, timescale: u32, rounding: Rounding) -> Result<Conversion<u64>> {
        Timescale::new(timescale)?.units_from_ticks(self, rounding)
    }
}

impl FrameRate {
    /// Exact `num/den` frames per second to a canonical table rate (`30000/1001`, `25/1`, ...).
    /// Zero, tick-inexact, off-table and legacy-spelling rates are `UnsupportedFrameRate`.
    pub fn from_fps(num: u64, den: u64) -> Result<Self> {
        let unsupported = || ValidationError::UnsupportedFrameRate;
        let ticks_per_frame = Timescale::rational(num, den)
            .map_err(|_| unsupported())?
            .exact_ticks_per_unit()
            .ok_or_else(unsupported)?;
        let (rate, normalization) = Self::from_import(ticks_per_frame)?;
        if normalization.changed {
            return Err(unsupported());
        }
        Ok(rate)
    }

    /// Frames per second as a reduced fraction, e.g. 29.97 -> `(30000, 1001)`, 25 -> `(25, 1)`.
    pub fn fps_fraction(self) -> (u64, u64) {
        let tpf = u128::from(self.ticks_per_frame());
        let g = gcd(u128::from(TICKS_PER_SECOND), tpf);
        ((u128::from(TICKS_PER_SECOND) / g) as u64, (tpf / g) as u64)
    }
}
