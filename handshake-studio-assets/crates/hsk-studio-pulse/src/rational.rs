//! Rational values at the interchange boundary only (STU-VID-012: rational time is never a
//! stored authority). Conversions into ticks carry an explicit rounding policy and a loss flag.
use crate::error::{PulseError, Result};
use hsk_studio_accord::{TICKS_PER_SECOND, Ticks};

pub(crate) fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Rational speed multiplier; reverse is a negative numerator (STU-VID-031). Always reduced, so
/// derived equality is value equality. Pure scaling arithmetic over tick spans.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Speed {
    num: i64,
    den: u64,
}

impl Speed {
    pub const UNITY: Speed = Speed { num: 1, den: 1 };

    pub fn new(num: i64, den: u64) -> Result<Self> {
        if num == 0 {
            return Err(PulseError::ZeroSpeed);
        }
        if den == 0 || num == i64::MIN {
            return Err(PulseError::InvalidSpeed);
        }
        let g = gcd(num.unsigned_abs(), den);
        let magnitude =
            i64::try_from(num.unsigned_abs() / g).map_err(|_| PulseError::InvalidSpeed)?;
        Ok(Self {
            num: if num < 0 { -magnitude } else { magnitude },
            den: den / g,
        })
    }
    pub fn numerator(self) -> i64 {
        self.num
    }
    pub fn denominator(self) -> u64 {
        self.den
    }
    pub fn is_reversed(self) -> bool {
        self.num < 0
    }
    /// Source ticks consumed by `duration` timeline ticks: `duration * |num| / den`, exact.
    pub fn span_for_duration(self, duration: Ticks) -> Result<Ticks> {
        let product = u128::from(duration.value()) * u128::from(self.num.unsigned_abs());
        let den = u128::from(self.den);
        if !product.is_multiple_of(den) {
            return Err(PulseError::Inexact);
        }
        u64::try_from(product / den)
            .map(Ticks::new)
            .map_err(|_| PulseError::Overflow)
    }
    /// Timeline ticks for a `span` of source ticks: `span * den / |num|`, exact.
    pub fn duration_for_span(self, span: Ticks) -> Result<Ticks> {
        let product = u128::from(span.value()) * u128::from(self.den);
        let num = u128::from(self.num.unsigned_abs());
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
