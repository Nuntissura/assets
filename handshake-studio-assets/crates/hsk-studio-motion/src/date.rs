//! Deterministic Date clock (STU-MOT-256/257 subset). The persisted profile maps an exact
//! evaluation tick to integer UTC milliseconds; no wall clock is ever read. Calendar rules:
//! proleptic Gregorian UTC, floor division / non-negative modulo for negative times.
use crate::{error::MotionError, time::EvalTick};
use hsk_studio_accord::TICKS_PER_SECOND;

/// Inclusive ECMAScript Date range in milliseconds since the epoch.
pub const MAX_ABS_UTC_MS: i64 = 8_640_000_000_000_000;
const MS_PER_DAY: i64 = 86_400_000;
const TICKS_PER_SECOND_WIDE: i128 = TICKS_PER_SECOND as i128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateProfile {
    epoch_ms: i64,
    origin_tick: u64,
}

impl Default for DateProfile {
    fn default() -> Self {
        Self {
            epoch_ms: 0,
            origin_tick: 0,
        }
    }
}

impl DateProfile {
    pub fn new(epoch_ms: i64, origin_tick: u64) -> Result<Self, MotionError> {
        if epoch_ms.unsigned_abs() > MAX_ABS_UTC_MS.unsigned_abs() {
            return Err(MotionError::InvalidDateProfile);
        }
        Ok(Self {
            epoch_ms,
            origin_tick,
        })
    }

    pub const fn epoch_ms(&self) -> i64 {
        self.epoch_ms
    }

    pub const fn origin_tick(&self) -> u64 {
        self.origin_tick
    }

    /// `N = epoch_ms*TPS + (tick - origin_tick)*1000` in checked `i128`; reject `|N|` above
    /// `MAX_ABS_UTC_MS*TPS`; `utc_ms = trunc_toward_zero(N / TPS)` (zero is canonical).
    pub fn utc_ms(&self, tick: EvalTick) -> Result<i64, MotionError> {
        let elapsed = i128::from(tick.value()) - i128::from(self.origin_tick);
        let n = i128::from(self.epoch_ms)
            .checked_mul(TICKS_PER_SECOND_WIDE)
            .and_then(|base| elapsed.checked_mul(1000).and_then(|d| base.checked_add(d)))
            .ok_or(MotionError::DateOutOfRange)?;
        let bound = i128::from(MAX_ABS_UTC_MS) * TICKS_PER_SECOND_WIDE;
        if n.unsigned_abs() > bound.unsigned_abs() {
            return Err(MotionError::DateOutOfRange);
        }
        i64::try_from(n / TICKS_PER_SECOND_WIDE).map_err(|_| MotionError::DateOutOfRange)
    }
}

/// Broken-down UTC time. `weekday`: 0 = Sunday.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CivilTime {
    pub year: i64,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub millisecond: u16,
    pub weekday: u8,
}

/// Days since 1970-01-01 -> (year, month 1..=12, day 1..=31). Hinnant civil-from-days, i64.
fn civil_from_days(days: i64) -> (i64, u8, u8) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// (year, month, day) -> days since 1970-01-01. Hinnant days-from-civil, i64.
fn days_from_civil(year: i64, month: u8, day: u8) -> i64 {
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * i64::from(mp) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn is_leap(year: i64) -> bool {
    year.rem_euclid(4) == 0 && (year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0)
}

fn days_in_month(year: i64, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap(year) => 29,
        _ => 28,
    }
}

/// Breaks `utc_ms` into calendar fields; `None` outside the exact Date range.
pub fn civil_from_ms(utc_ms: i64) -> Option<CivilTime> {
    if utc_ms.unsigned_abs() > MAX_ABS_UTC_MS.unsigned_abs() {
        return None;
    }
    let days = utc_ms.div_euclid(MS_PER_DAY);
    let in_day = utc_ms.rem_euclid(MS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    Some(CivilTime {
        year,
        month,
        day,
        hour: (in_day / 3_600_000) as u8,
        minute: (in_day / 60_000 % 60) as u8,
        second: (in_day / 1000 % 60) as u8,
        millisecond: (in_day % 1000) as u16,
        weekday: (days + 4).rem_euclid(7) as u8,
    })
}

/// `Date.prototype.toISOString` text: 4-digit year 0000..=9999, else sign + 6 digits; always
/// `.sssZ`. `None` outside the exact Date range.
pub fn to_iso_string(utc_ms: i64) -> Option<String> {
    let c = civil_from_ms(utc_ms)?;
    let year = if (0..=9999).contains(&c.year) {
        format!("{:04}", c.year)
    } else {
        let sign = if c.year < 0 { '-' } else { '+' };
        format!("{sign}{:06}", c.year.unsigned_abs())
    };
    Some(format!(
        "{year}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        c.month, c.day, c.hour, c.minute, c.second, c.millisecond
    ))
}

fn digits(bytes: &[u8], from: usize, count: usize) -> Option<i64> {
    let slice = bytes.get(from..from + count)?;
    let mut value = 0_i64;
    for byte in slice {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value * 10 + i64::from(byte - b'0');
    }
    Some(value)
}

/// Parses exactly `YYYY-MM-DDTHH:mm:ss[.sss]Z` or the signed form `+/-YYYYYY-MM-DDTHH:mm:ss[.sss]Z`
/// (UTC only). Rejects whitespace, offsets, date-only forms, `24:00`, leap seconds, `-000000`,
/// impossible calendar days and results outside the Date range.
pub fn parse_iso_exact(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let (year, rest) = match bytes.first()? {
        b'+' | b'-' => {
            let magnitude = digits(bytes, 1, 6)?;
            if bytes[0] == b'-' {
                if magnitude == 0 {
                    return None;
                }
                (-magnitude, 7)
            } else {
                (magnitude, 7)
            }
        }
        _ => (digits(bytes, 0, 4)?, 4),
    };
    let separators = [(rest, b'-'), (rest + 3, b'-'), (rest + 6, b'T'), (rest + 9, b':'), (rest + 12, b':')];
    if separators.iter().any(|(at, ch)| bytes.get(*at) != Some(ch)) {
        return None;
    }
    let month = u8::try_from(digits(bytes, rest + 1, 2)?).ok()?;
    let day = u8::try_from(digits(bytes, rest + 4, 2)?).ok()?;
    let hour = digits(bytes, rest + 7, 2)?;
    let minute = digits(bytes, rest + 10, 2)?;
    let second = digits(bytes, rest + 13, 2)?;
    let mut tail = rest + 15;
    let mut millis = 0;
    if bytes.get(tail) == Some(&b'.') {
        millis = digits(bytes, tail + 1, 3)?;
        tail += 4;
    }
    if bytes.get(tail) != Some(&b'Z') || bytes.len() != tail + 1 {
        return None;
    }
    if !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let ms = days
        .checked_mul(MS_PER_DAY)?
        .checked_add(hour * 3_600_000 + minute * 60_000 + second * 1000 + millis)?;
    (ms.unsigned_abs() <= MAX_ABS_UTC_MS.unsigned_abs()).then_some(ms)
}
