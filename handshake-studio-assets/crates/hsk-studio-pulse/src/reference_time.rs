//! Media Foundation `REFERENCE_TIME` (signed 100 ns units, 10_000_000 per second) <-> ticks.
//!
//! One tick is 1/254_016_000_000 s, so one 100 ns unit is `254_016_000_000 / 10_000_000 = 25401.6`
//! ticks, the reduced fraction `127_008 / 5`. The unit is not a whole number of ticks, so the
//! conversion has a stated rule and every call returns a receipt (STU-VID-012: rational and
//! interchange time never replaces stored ticks).
//!
//! Rule:
//! 1. `n` units of 100 ns are exactly `n * 127_008 / 5` ticks; the value is whole iff `n % 5 == 0`.
//! 2. Otherwise the caller's [`Rounding`] (Accord's enum, Accord does the arithmetic) picks the
//!    neighbouring tick. Interchange uses [`REFERENCE_TIME_ROUNDING`] (`NearestHalfUp`). The
//!    denominator 5 is odd, so a forward conversion never lands on a tie.
//! 3. Ticks to 100 ns divides by `127_008 / 5`: whole iff `ticks % 127_008 == 0`; the denominator
//!    is even, so a tie (`ticks % 127_008 == 63_504`) exists and `NearestHalfUp` takes the larger
//!    value (2.5 -> 3, -2.5 -> -2).
//! 4. Receipt `residual` is `exact - result` as an exact fraction (`numerator / denominator`, with
//!    denominator 5 forward and 127_008 backward), so `exact = result + residual`: positive means
//!    the result was rounded down, negative up, zero means `exact == true`.
//! 5. Round trip: under `NearestHalfUp` the forward error is at most 2/5 tick, which is
//!    2/127_008 of a unit, far under half a unit, so `100 ns -> ticks -> 100 ns` is the identity
//!    for every input whose tick value fits `i64`. The other modes are not round-trip safe
//!    (`1` unit, `Floor` both ways, comes back as `0`); ticks -> 100 ns -> ticks is lossy by
//!    construction (ticks are finer) and its receipt records the loss.
//! 6. Range: tick values are `i64`; a result outside it (more than 363_102_010_773_131 units,
//!    about 420 days, in either sign) is [`PulseError::Overflow`]. Ticks -> 100 ns cannot
//!    overflow. Negative values stay negative (`REFERENCE_TIME` is signed, sample times can
//!    precede zero); [`ReferenceTimeToTicks::sequence_ticks`] refuses a negative result as
//!    [`PulseError::Underflow`] because sequence time (`Ticks`) is unsigned.
use crate::error::{PulseError, Result};
use hsk_studio_accord::{Conversion, Rounding, TICKS_PER_SECOND, Ticks, Timescale};

/// `REFERENCE_TIME` units per second (100 ns).
pub const REFERENCE_TIME_UNITS_PER_SECOND: u32 = 10_000_000;

const fn gcd_const(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

const UNIT_GCD: u64 = gcd_const(TICKS_PER_SECOND, REFERENCE_TIME_UNITS_PER_SECOND as u64);

/// Numerator of the ticks per 100 ns unit as a reduced fraction (127_008).
pub const TICKS_PER_REFERENCE_UNIT_NUM: u64 = TICKS_PER_SECOND / UNIT_GCD;
/// Denominator of the ticks per 100 ns unit as a reduced fraction (5).
pub const TICKS_PER_REFERENCE_UNIT_DEN: u64 = REFERENCE_TIME_UNITS_PER_SECOND as u64 / UNIT_GCD;

/// Rounding used when interchange does not name another one (round trip safe, see module rule 5).
pub const REFERENCE_TIME_ROUNDING: Rounding = Rounding::NearestHalfUp;

/// What the rounding removed: an exact fraction, `exact = result + numerator / denominator`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Residual {
    pub numerator: i64,
    pub denominator: u64,
}

impl Residual {
    pub const fn is_zero(self) -> bool {
        self.numerator == 0
    }
}

/// Receipt of `100 ns -> ticks`. `denominator` of the residual is
/// [`TICKS_PER_REFERENCE_UNIT_DEN`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReferenceTimeToTicks {
    pub input_100ns: i64,
    pub ticks: i64,
    pub rounding: Rounding,
    /// `false` = `ticks` is a rounded value (`input_100ns % 5 != 0`).
    pub exact: bool,
    pub residual: Residual,
}

/// Receipt of `ticks -> 100 ns`. `denominator` of the residual is
/// [`TICKS_PER_REFERENCE_UNIT_NUM`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TicksToReferenceTime {
    pub input_ticks: i64,
    pub reference_time_100ns: i64,
    pub rounding: Rounding,
    /// `false` = the result is rounded (`input_ticks % 127_008 != 0`).
    pub exact: bool,
    pub residual: Residual,
}

impl ReferenceTimeToTicks {
    /// Refuses a rounded conversion with [`PulseError::Inexact`].
    pub fn require_exact(self) -> Result<Self> {
        if self.exact {
            Ok(self)
        } else {
            Err(PulseError::Inexact)
        }
    }

    /// The result as sequence time; a negative tick value is [`PulseError::Underflow`].
    pub fn sequence_ticks(&self) -> Result<Ticks> {
        u64::try_from(self.ticks)
            .map(Ticks::new)
            .map_err(|_| PulseError::Underflow)
    }
}

impl TicksToReferenceTime {
    /// Refuses a rounded conversion with [`PulseError::Inexact`].
    pub fn require_exact(self) -> Result<Self> {
        if self.exact {
            Ok(self)
        } else {
            Err(PulseError::Inexact)
        }
    }
}

fn reference_timescale() -> Result<Timescale> {
    Ok(Timescale::new(REFERENCE_TIME_UNITS_PER_SECOND)?)
}

/// `input * mul - result * div` in 128 bits: the part of the exact quotient `input * mul / div`
/// that `result` does not carry, as the numerator over `div`.
fn residual(input: i64, mul: u64, result: i64, div: u64) -> Result<Residual> {
    let exact_scaled = i128::from(input) * i128::from(mul);
    let numerator = exact_scaled - i128::from(result) * i128::from(div);
    Ok(Residual {
        numerator: i64::try_from(numerator).map_err(|_| PulseError::Overflow)?,
        denominator: div,
    })
}

/// `input_100ns` units of 100 ns to signed ticks under `rounding`, with a receipt.
/// A result outside `i64` is [`PulseError::Overflow`]; nothing panics.
pub fn ticks_from_reference_time(
    input_100ns: i64,
    rounding: Rounding,
) -> Result<ReferenceTimeToTicks> {
    let Conversion { value, exact } =
        reference_timescale()?.signed_ticks_from_units(input_100ns, rounding)?;
    Ok(ReferenceTimeToTicks {
        input_100ns,
        ticks: value,
        rounding,
        exact,
        residual: residual(
            input_100ns,
            TICKS_PER_REFERENCE_UNIT_NUM,
            value,
            TICKS_PER_REFERENCE_UNIT_DEN,
        )?,
    })
}

/// Signed ticks to units of 100 ns under `rounding`, with a receipt.
pub fn reference_time_from_ticks(
    input_ticks: i64,
    rounding: Rounding,
) -> Result<TicksToReferenceTime> {
    let Conversion { value, exact } =
        reference_timescale()?.signed_units_from_ticks(input_ticks, rounding)?;
    Ok(TicksToReferenceTime {
        input_ticks,
        reference_time_100ns: value,
        rounding,
        exact,
        residual: residual(
            input_ticks,
            TICKS_PER_REFERENCE_UNIT_DEN,
            value,
            TICKS_PER_REFERENCE_UNIT_NUM,
        )?,
    })
}

/// Sequence time (`Ticks`) to units of 100 ns. Ticks above `i64::MAX` (about 420 days) are
/// [`PulseError::Overflow`] because the receipt carries signed ticks.
pub fn reference_time_from_sequence_ticks(
    ticks: Ticks,
    rounding: Rounding,
) -> Result<TicksToReferenceTime> {
    let signed = i64::try_from(ticks.value()).map_err(|_| PulseError::Overflow)?;
    reference_time_from_ticks(signed, rounding)
}
