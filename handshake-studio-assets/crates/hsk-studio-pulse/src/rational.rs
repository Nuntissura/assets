//! Rational values at the interchange boundary only (STU-VID-012: rational time is never a
//! stored authority). Conversions into ticks carry an explicit rounding policy and a loss flag.
use crate::error::{PulseError, Result};
use hsk_studio_accord::{TICKS_PER_SECOND, Ticks};
use hsk_studio_folio::Speed;

pub(crate) fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Reduced speed in Folio's document type (`hsk_studio_folio::Speed` is the single owner of the
/// stored value); `num/den` is reduced and a negative numerator is reverse (STU-VID-031).
pub fn reduced_speed(numerator: i64, denominator: u64) -> Result<Speed> {
    if numerator == 0 {
        return Err(PulseError::ZeroSpeed);
    }
    if denominator == 0 || numerator == i64::MIN {
        return Err(PulseError::InvalidSpeed);
    }
    let g = gcd(numerator.unsigned_abs(), denominator);
    let magnitude =
        i64::try_from(numerator.unsigned_abs() / g).map_err(|_| PulseError::InvalidSpeed)?;
    Ok(Speed {
        numerator: if numerator < 0 { -magnitude } else { magnitude },
        denominator: denominator / g,
    })
}

/// Pulse's exact tick arithmetic over Folio's stored [`Speed`].
pub trait SpeedMath {
    /// Source ticks consumed by `duration` timeline ticks: `duration * |num| / den`, exact.
    fn span_for_duration(&self, duration: Ticks) -> Result<Ticks>;
    /// Timeline ticks for a `span` of source ticks: `span * den / |num|`, exact.
    fn duration_for_span(&self, span: Ticks) -> Result<Ticks>;
}

fn checked_speed(speed: &Speed) -> Result<(u128, u128)> {
    if speed.numerator == 0 {
        return Err(PulseError::ZeroSpeed);
    }
    if speed.denominator == 0 {
        return Err(PulseError::InvalidSpeed);
    }
    Ok((
        u128::from(speed.numerator.unsigned_abs()),
        u128::from(speed.denominator),
    ))
}

impl SpeedMath for Speed {
    fn span_for_duration(&self, duration: Ticks) -> Result<Ticks> {
        let (num, den) = checked_speed(self)?;
        let product = u128::from(duration.value()) * num;
        if !product.is_multiple_of(den) {
            return Err(PulseError::Inexact);
        }
        u64::try_from(product / den)
            .map(Ticks::new)
            .map_err(|_| PulseError::Overflow)
    }
    fn duration_for_span(&self, span: Ticks) -> Result<Ticks> {
        let (num, den) = checked_speed(self)?;
        let product = u128::from(span.value()) * den;
        if !product.is_multiple_of(num) {
            return Err(PulseError::Inexact);
        }
        u64::try_from(product / num)
            .map(Ticks::new)
            .map_err(|_| PulseError::Overflow)
    }
}

/// Rounding policy for an interchange value that is not a whole number of ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rounding {
    /// Refuse (`inexact`) when the value is between ticks.
    Exact,
    Floor,
    Ceil,
    /// Nearest tick, ties toward the larger value.
    NearestHalfUp,
}

/// Result of an interchange conversion; `exact == false` is a recorded rounding loss.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RationalConversion {
    pub ticks: Ticks,
    pub exact: bool,
}

/// `numerator/denominator` seconds to ticks under an explicit rounding policy.
pub fn ticks_from_rational_seconds(
    numerator: u64,
    denominator: u64,
    rounding: Rounding,
) -> Result<RationalConversion> {
    if denominator == 0 {
        return Err(PulseError::InvalidInput);
    }
    let scaled = u128::from(numerator) * u128::from(TICKS_PER_SECOND);
    let den = u128::from(denominator);
    let (quotient, remainder) = (scaled / den, scaled % den);
    let exact = remainder == 0;
    let rounded = match rounding {
        Rounding::Exact if !exact => return Err(PulseError::Inexact),
        Rounding::Exact | Rounding::Floor => quotient,
        Rounding::Ceil => quotient + u128::from(!exact),
        Rounding::NearestHalfUp => quotient + u128::from(remainder * 2 >= den),
    };
    Ok(RationalConversion {
        ticks: Ticks::new(u64::try_from(rounded).map_err(|_| PulseError::Overflow)?),
        exact,
    })
}

/// Ticks as reduced rational seconds `(numerator, denominator)`; zero is `(0, 1)`.
pub fn rational_seconds_from_ticks(ticks: Ticks) -> (u64, u64) {
    if ticks.value() == 0 {
        return (0, 1);
    }
    let g = gcd(ticks.value(), TICKS_PER_SECOND);
    (ticks.value() / g, TICKS_PER_SECOND / g)
}
