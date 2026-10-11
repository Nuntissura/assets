//! Interchange time adapters: the only place rational/decimal/timecode time becomes ticks
//! (STU-VID-012/013/013a). Stored authority stays integer ticks at 254016000000/s.
//!
//! NOTE: the timecode label arithmetic here (non-drop and drop-frame) deliberately mirrors
//! `hsk-studio-pulse`, which owns time. `pulse` is not in this crate's allowed dependency list
//! (root `workspace.metadata.studio.dependencies.hsk-studio-interop-media`), so the minimal copy
//! lives here until the lead adds that edge; then these functions collapse onto pulse.
use crate::error::{ErrorKind, InteropError, Result};
use hsk_studio_accord::{FrameRate, TICKS_PER_SECOND, Ticks};

fn fail<T>(kind: ErrorKind) -> Result<T> {
    Err(InteropError::new(kind))
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Reduced ratio `num/den` (den > 0).
pub(crate) fn reduce(num: u64, den: u64) -> (u64, u64) {
    let g = gcd(num, den).max(1);
    (num / g, den / g)
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
    let scaled = u128::from(TICKS_PER_SECOND) * u128::from(den);
    if !scaled.is_multiple_of(u128::from(num)) {
        return fail(ErrorKind::UnsupportedFrameRate);
    }
    let ticks_per_frame = u64::try_from(scaled / u128::from(num))
        .map_err(|_| InteropError::new(ErrorKind::UnsupportedFrameRate))?;
    let (rate, exactness) = frame_rate_from_ticks(ticks_per_frame)?;
    if exactness != Exactness::Exact {
        return fail(ErrorKind::UnsupportedFrameRate);
    }
    Ok(rate)
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
    reduce(TICKS_PER_SECOND, rate.ticks_per_frame())
}

/// `value` units at `rate_num/rate_den` units per second to ticks. Exact when the division is
/// whole; otherwise rounded half-away-from-zero and reported as `Approximate`.
pub fn rational_to_ticks(value: i64, rate_num: u32, rate_den: u32) -> Result<(Ticks, Exactness)> {
    if rate_num == 0 || rate_den == 0 {
        return fail(ErrorKind::InvalidRational);
    }
    let value = u128::try_from(value).map_err(|_| InteropError::new(ErrorKind::NegativeTime))?;
    let scaled = value
        .checked_mul(u128::from(TICKS_PER_SECOND))
        .and_then(|v| v.checked_mul(u128::from(rate_den)))
        .ok_or(InteropError::new(ErrorKind::TimecodeOverflow))?;
    let num = u128::from(rate_num);
    let (quotient, remainder) = (scaled / num, scaled % num);
    let (rounded, exactness) = if remainder == 0 {
        (quotient, Exactness::Exact)
    } else {
        (
            quotient + u128::from(remainder * 2 >= num),
            Exactness::Approximate(RoundingRule::HalfAwayFromZero),
        )
    };
    let ticks =
        u64::try_from(rounded).map_err(|_| InteropError::new(ErrorKind::TimecodeOverflow))?;
    Ok((Ticks::new(ticks), exactness))
}

/// `(nominal fps, NTSC 1001 rate)`; `None` = no integer timecode base (12.5 fps).
fn nominal(rate: FrameRate) -> Option<(u64, bool)> {
    Some(match rate.ticks_per_frame() {
        10_594_584_000 => (24, true),
        10_584_000_000 => (24, false),
        10_160_640_000 => (25, false),
        8_475_667_200 => (30, true),
        8_467_200_000 => (30, false),
        5_292_000_000 => (48, false),
        5_080_320_000 => (50, false),
        4_237_833_600 => (60, true),
        4_233_600_000 => (60, false),
        16_934_400_000 => (15, false),
        _ => return None,
    })
}

#[derive(Clone, Copy)]
struct Base {
    nominal: u64,
    dropped: u64,
    per_ten_minutes: u64,
}

/// Drop-frame skips 2 (29.97) or 4 (59.94) frame NUMBERS at each minute not divisible by ten.
fn base(rate: FrameRate, drop: bool) -> Result<Base> {
    let (nominal, ntsc) = nominal(rate).ok_or(InteropError::new(ErrorKind::TimecodeUnsupported))?;
    if drop && !(ntsc && nominal.is_multiple_of(30)) {
        return fail(ErrorKind::DropFrameUnsupported);
    }
    Ok(Base {
        nominal,
        dropped: if drop { nominal / 30 * 2 } else { 0 },
        per_ten_minutes: if drop {
            nominal / 30 * 17_982
        } else {
            nominal * 600
        },
    })
}

/// `HH:MM:SS:FF` or `HH:MM:SS;FF` to a frame number. `;` is only valid with `drop`; labels the
/// drop-frame rule skips are refused.
pub fn timecode_to_frames(text: &str, rate: FrameRate, drop: bool) -> Result<u64> {
    let b = base(rate, drop)?;
    let bytes = text.as_bytes();
    if text.len() < 11 || text.len() > 13 || !text.is_ascii() {
        return fail(ErrorKind::InvalidTimecode);
    }
    let parts: Vec<&str> = text.split([':', ';']).collect();
    let seps: Vec<u8> = bytes
        .iter()
        .copied()
        .filter(|c| matches!(c, b':' | b';'))
        .collect();
    if parts.len() != 4
        || seps.len() != 3
        || seps[..2].contains(&b';')
        || parts
            .iter()
            .any(|p| p.is_empty() || p.len() > 3 || !p.bytes().all(|c| c.is_ascii_digit()))
    {
        return fail(ErrorKind::InvalidTimecode);
    }
    if seps[2] == b';' && !drop {
        return fail(ErrorKind::DropMismatch);
    }
    let n = |i: usize| parts[i].parse::<u64>().unwrap_or(u64::MAX);
    let (h, m, s, f) = (n(0), n(1), n(2), n(3));
    if m >= 60 || s >= 60 || f >= b.nominal {
        return fail(ErrorKind::InvalidTimecode);
    }
    let minutes = h * 60 + m;
    if b.dropped > 0 && s == 0 && f < b.dropped && minutes % 10 != 0 {
        return fail(ErrorKind::DroppedTimecode);
    }
    Ok((minutes * 60 + s) * b.nominal + f - b.dropped * (minutes - minutes / 10))
}

/// Frame number to `HH:MM:SS:FF` (`;` before the frames field for drop-frame).
pub fn frames_to_timecode(frames: u64, rate: FrameRate, drop: bool) -> Result<String> {
    let b = base(rate, drop)?;
    let f = u128::from(frames);
    let (n, d) = (u128::from(b.nominal), u128::from(b.dropped));
    let labelled = if d == 0 {
        f
    } else {
        let per_ten = u128::from(b.per_ten_minutes);
        let (blocks, rem) = (f / per_ten, f % per_ten);
        f + 9 * d * blocks
            + if rem >= d {
                d * ((rem - d) / (per_ten / 10))
            } else {
                0
            }
    };
    let seconds = labelled / n;
    let minutes = seconds / 60;
    let hours = minutes / 60;
    if hours > 99 {
        return fail(ErrorKind::TimecodeOverflow);
    }
    Ok(format!(
        "{:02}:{:02}:{:02}{}{:02}",
        hours,
        minutes % 60,
        seconds % 60,
        if drop { ';' } else { ':' },
        labelled % n
    ))
}

pub fn frames_to_ticks(frames: u64, rate: FrameRate) -> Result<Ticks> {
    Ticks::checked_frames(frames, rate).map_err(InteropError::from)
}

/// Exact frame count of a frame-aligned tick value.
pub fn ticks_to_frames_exact(ticks: Ticks, rate: FrameRate) -> Result<u64> {
    let tpf = rate.ticks_per_frame();
    if !ticks.value().is_multiple_of(tpf) {
        return fail(ErrorKind::NotFrameAligned);
    }
    Ok(ticks.value() / tpf)
}
