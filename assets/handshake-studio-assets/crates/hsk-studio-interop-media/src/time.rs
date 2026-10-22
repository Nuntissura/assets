//! Interchange time adapters: the only place rational/decimal/timecode time becomes ticks
//! (STU-VID-012/013/013a). Stored authority stays integer ticks at 254016000000/s.
//!
//! Rational-to-tick and fps arithmetic is Accord's (`Timescale`, `FrameRate::from_fps` /
//! `fps_fraction`); this module keeps the interchange spellings, loss reporting and typed errors.
//!
//! Tick/frame conversion and rational reduction are Accord's too (`Timescale::units_from_ticks`,
//! `Timescale::rational`); no local division or gcd remains for them.
//!
//! Timecode label arithmetic (non-drop and drop-frame), tick/frame exactness and `fps_fraction`
//! are `hsk-studio-pulse`'s (D9); this module keeps the interchange syntax bound and typed errors.
use crate::error::{ErrorKind, InteropError, Result};
use hsk_studio_accord::{FrameRate, Rounding, Ticks, Timescale};
use hsk_studio_pulse as pulse;

fn fail<T>(kind: ErrorKind) -> Result<T> {
    Err(InteropError::new(kind))
}

/// Reduced ratio `num/den` (both non-zero), from Accord's reduced-rational constructor.
pub(crate) fn reduce(num: u64, den: u64) -> Result<(u64, u64)> {
    Ok(Timescale::rational(num, den)?.units_per_second())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoundingRule {
    HalfAwayFromZero,
}

/// How faithfully an interchange value became ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exactness {
    Exact,
    /// A legacy ticks/frame spelling normalized to the canonical 1001-ratio value (STU-VID-013a).
    NormalizedLegacy,
    /// Rounded with the named rule; callers must record a loss note.
    Approximate(RoundingRule),
}

/// Ticks per frame to a canonical rate; legacy spellings are normalized and reported.
pub fn frame_rate_from_ticks(ticks_per_frame: u64) -> Result<(FrameRate, Exactness)> {
    let (rate, normalization) = FrameRate::from_import(ticks_per_frame)
        .map_err(|_| InteropError::new(ErrorKind::UnsupportedFrameRate))?;
    Ok((
        rate,
        if normalization.changed {
            Exactness::NormalizedLegacy
        } else {
            Exactness::Exact
        },
    ))
}

/// Exact `num/den` frames per second to a table rate; anything inexact or off-table is refused.
pub fn frame_rate_from_fraction(num: u64, den: u64) -> Result<FrameRate> {
    if num == 0 || den == 0 {
        return fail(ErrorKind::InvalidFrameRate);
    }
    Ok(FrameRate::from_fps(num, den)?)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateSpelling {
    Exact,
    /// 23.976 / 29.97 / 59.94 spelled as decimals, read as the 1001-ratio rates.
    DecimalAlias,
}

/// `25`, `25.0`, `24000/1001`, or the decimal aliases `23.976`, `29.97`, `59.94`.
pub fn parse_frame_rate(text: &str) -> Result<(FrameRate, RateSpelling)> {
    let text = text.trim();
    let digits = |s: &str| !s.is_empty() && s.len() <= 9 && s.bytes().all(|b| b.is_ascii_digit());
    if let Some((n, d)) = text.split_once('/') {
        if !digits(n) || !digits(d) {
            return fail(ErrorKind::InvalidFrameRate);
        }
        let (n, d) = (n.parse().unwrap_or(0), d.parse().unwrap_or(0));
        return Ok((frame_rate_from_fraction(n, d)?, RateSpelling::Exact));
    }
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if !digits(whole) || (!fraction.is_empty() && !digits(fraction)) {
        return fail(ErrorKind::InvalidFrameRate);
    }
    if fraction.bytes().all(|b| b == b'0') {
        let n = whole.parse().unwrap_or(0);
        return Ok((frame_rate_from_fraction(n, 1)?, RateSpelling::Exact));
    }
    let alias = match (whole, fraction) {
        ("23", "976") => (24_000, 1001),
        ("29", "97") => (30_000, 1001),
        ("59", "94") => (60_000, 1001),
        _ => return fail(ErrorKind::InvalidFrameRate),
    };
    Ok((
        frame_rate_from_fraction(alias.0, alias.1)?,
        RateSpelling::DecimalAlias,
    ))
}

/// Reduced frames per second as `(numerator, denominator)`, e.g. 29.97 -> (30000, 1001).
pub fn fps_fraction(rate: FrameRate) -> (u64, u64) {
    pulse::fps_fraction(rate)
}

/// `value` units at `rate_num/rate_den` units per second to ticks. Exact when the division is
/// whole; otherwise rounded to nearest (identical to half-away-from-zero for the non-negative
/// values accepted here) and reported as `Approximate`.
pub fn rational_to_ticks(value: i64, rate_num: u32, rate_den: u32) -> Result<(Ticks, Exactness)> {
    if rate_num == 0 || rate_den == 0 {
        return fail(ErrorKind::InvalidRational);
    }
    let value = u64::try_from(value).map_err(|_| InteropError::new(ErrorKind::NegativeTime))?;
    let converted = Timescale::rational(u64::from(rate_num), u64::from(rate_den))?
        .ticks_from_units(value, Rounding::NearestHalfUp)?;
    Ok((
        converted.value,
        if converted.exact {
            Exactness::Exact
        } else {
            Exactness::Approximate(RoundingRule::HalfAwayFromZero)
        },
    ))
}

fn timecode_format(rate: FrameRate, drop: bool) -> Result<pulse::TimecodeFormat> {
    pulse::TimecodeFormat::new(rate, drop).map_err(InteropError::from)
}

/// `HH:MM:SS:FF` or `HH:MM:SS;FF` to a frame number. `;` is only valid with `drop`; labels the
/// drop-frame rule skips are refused. Label arithmetic is Pulse's; this keeps the interchange
/// syntax bound and the typed errors.
pub fn timecode_to_frames(text: &str, rate: FrameRate, drop: bool) -> Result<u64> {
    let format = timecode_format(rate, drop)?;
    if text.len() < 11 || text.len() > 13 {
        return fail(ErrorKind::InvalidTimecode);
    }
    if text.split([':', ';']).any(|p| p.len() > 3) {
        return fail(ErrorKind::InvalidTimecode);
    }
    let mut tc = pulse::parse_timecode(text)?;
    if tc.drop_frame && !drop {
        return fail(ErrorKind::DropMismatch);
    }
    tc.drop_frame = drop;
    if tc.minutes >= 60 || tc.seconds >= 60 || u64::from(tc.frames) >= format.nominal_fps() {
        return fail(ErrorKind::InvalidTimecode);
    }
    // Field ranges are checked above, so the only InvalidTimecode left is a skipped drop label.
    pulse::timecode_to_frames(tc, format).map_err(|e| match e {
        pulse::PulseError::InvalidTimecode => InteropError::new(ErrorKind::DroppedTimecode),
        other => InteropError::from(other),
    })
}

/// Frame number to `HH:MM:SS:FF` (`;` before the frames field for drop-frame).
pub fn frames_to_timecode(frames: u64, rate: FrameRate, drop: bool) -> Result<String> {
    let tc = pulse::frames_to_timecode(frames, timecode_format(rate, drop)?);
    if tc.hours > 99 {
        return fail(ErrorKind::TimecodeOverflow);
    }
    Ok(tc.to_string())
}

pub fn frames_to_ticks(frames: u64, rate: FrameRate) -> Result<Ticks> {
    Ticks::checked_frames(frames, rate).map_err(InteropError::from)
}

/// Exact frame count of a frame-aligned tick value (Pulse's conversion); a remainder means the
/// value is not on a frame boundary.
pub fn ticks_to_frames_exact(ticks: Ticks, rate: FrameRate) -> Result<u64> {
    pulse::ticks_to_frames_exact(ticks, rate).map_err(InteropError::from)
}
