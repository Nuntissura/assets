//! Tick/frame conversion and timecode LABEL functions (STU-VID-012, 012a, 012c, 013, 013a).
//! Stored time is always integer ticks; drop-frame only changes how a frame number is labelled.
use crate::error::{PulseError, Result};
use crate::rational::{Rounding, gcd};
use hsk_studio_accord::{FrameRate, FrameRateNormalization, TICKS_PER_SECOND, Ticks};

/// Normalizes a ticks/frame import value (legacy 10594594594/8475675675 map to the 1001-ratio
/// canonical values) and returns the receipt. Unknown rates are refused.
pub fn timebase_from_import(ticks_per_frame: u64) -> Result<(FrameRate, FrameRateNormalization)> {
    Ok(FrameRate::from_import(ticks_per_frame)?)
}

pub fn frames_to_ticks(frames: u64, rate: FrameRate) -> Result<Ticks> {
    Ok(Ticks::checked_frames(frames, rate)?)
}

/// Exact conversion: a tick count that is not a whole number of frames is refused.
pub fn ticks_to_frames_exact(ticks: Ticks, rate: FrameRate) -> Result<u64> {
    let tpf = rate.ticks_per_frame();
    if !ticks.value().is_multiple_of(tpf) {
        return Err(PulseError::NotFrameAligned);
    }
    Ok(ticks.value() / tpf)
}

/// Display-side conversion: frames fully elapsed at `ticks`.
pub fn ticks_to_frames_floor(ticks: Ticks, rate: FrameRate) -> u64 {
    ticks.value() / rate.ticks_per_frame()
}

/// accord::Ticks has no subtraction; a negative result is `Underflow`.
pub fn checked_sub(a: Ticks, b: Ticks) -> Result<Ticks> {
    a.value()
        .checked_sub(b.value())
        .map(Ticks::new)
        .ok_or(PulseError::Underflow)
}

/// Frames per second as a reduced fraction, e.g. 29.97 -> (30000, 1001), 25 -> (25, 1).
pub fn fps_fraction(rate: FrameRate) -> (u64, u64) {
    let g = gcd(TICKS_PER_SECOND, rate.ticks_per_frame());
    (TICKS_PER_SECOND / g, rate.ticks_per_frame() / g)
}

/// Interchange entry: exact `num/den` frames per second to a table rate. Rates whose ticks per
/// frame are not exact integers, or not in the STU-VID-013 table, are refused.
pub fn frame_rate_from_fps(num: u64, den: u64) -> Result<FrameRate> {
    if num == 0 || den == 0 {
        return Err(PulseError::UnsupportedFrameRate);
    }
    let scaled = u128::from(TICKS_PER_SECOND) * u128::from(den);
    if !scaled.is_multiple_of(u128::from(num)) {
        return Err(PulseError::UnsupportedFrameRate);
    }
    let ticks_per_frame =
        u64::try_from(scaled / u128::from(num)).map_err(|_| PulseError::UnsupportedFrameRate)?;
    let (rate, normalization) = timebase_from_import(ticks_per_frame)?;
    debug_assert!(!normalization.changed);
    Ok(rate)
}

/// Audio sample position to ticks; exact for every rate that divides 254016000000.
pub fn ticks_from_samples(samples: u64, sample_rate_hz: u32) -> Result<Ticks> {
    let per_sample = ticks_per_sample(sample_rate_hz)?;
    samples
        .checked_mul(per_sample)
        .map(Ticks::new)
        .ok_or(PulseError::Overflow)
}

/// Ticks to audio sample position; a tick count between samples is refused.
pub fn samples_from_ticks_exact(ticks: Ticks, sample_rate_hz: u32) -> Result<u64> {
    let per_sample = ticks_per_sample(sample_rate_hz)?;
    if !ticks.value().is_multiple_of(per_sample) {
        return Err(PulseError::NotFrameAligned);
    }
    Ok(ticks.value() / per_sample)
}

/// Display-side sample position: samples fully elapsed at `ticks`.
pub fn samples_floor(ticks: Ticks, sample_rate_hz: u32) -> Result<u64> {
    Ok(ticks.value() / ticks_per_sample(sample_rate_hz)?)
}

fn ticks_per_sample(sample_rate_hz: u32) -> Result<u64> {
    let hz = u64::from(sample_rate_hz);
    if hz == 0 || !TICKS_PER_SECOND.is_multiple_of(hz) {
        return Err(PulseError::UnsupportedSampleRate);
    }
    Ok(TICKS_PER_SECOND / hz)
}

pub fn checked_add(a: Ticks, b: Ticks) -> Result<Ticks> {
    Ok(a.checked_add(b)?)
}

/// (nominal frames per second, NTSC 1001-ratio rate). `None` = no integer timecode base
/// (12.5 fps has no standard timecode here).
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

/// How frame numbers are turned into `HH:MM:SS:FF` labels for one frame rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimecodeFormat {
    nominal_fps: u64,
    drop_frame: bool,
}

impl TimecodeFormat {
    /// Drop-frame is defined only for the 30000/1001 and 60000/1001 rates (2 and 4 frame
    /// numbers skipped at the start of every minute not divisible by ten).
    pub fn new(rate: FrameRate, drop_frame: bool) -> Result<Self> {
        let (nominal_fps, ntsc) = nominal(rate).ok_or(PulseError::TimecodeUnsupported)?;
        if drop_frame && !(ntsc && nominal_fps.is_multiple_of(30)) {
            return Err(PulseError::DropFrameUnsupported);
        }
        Ok(Self {
            nominal_fps,
            drop_frame,
        })
    }
    pub fn nominal_fps(self) -> u64 {
        self.nominal_fps
    }
    pub fn drop_frame(self) -> bool {
        self.drop_frame
    }
    fn dropped_per_minute(self) -> u64 {
        if self.drop_frame {
            self.nominal_fps / 30 * 2
        } else {
            0
        }
    }
    fn frames_per_ten_minutes(self) -> u64 {
        if self.drop_frame {
            self.nominal_fps / 30 * 17_982
        } else {
            self.nominal_fps * 600
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timecode {
    pub hours: u64,
    pub minutes: u8,
    pub seconds: u8,
    pub frames: u8,
    pub drop_frame: bool,
}

impl std::fmt::Display for Timecode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sep = if self.drop_frame { ';' } else { ':' };
        write!(
            f,
            "{:02}:{:02}:{:02}{sep}{:02}",
            self.hours, self.minutes, self.seconds, self.frames
        )
    }
}

/// Label of frame number `frame` (0 = first frame). Pure label function; never used for storage.
pub fn frames_to_timecode(frame: u64, format: TimecodeFormat) -> Timecode {
    let n = u128::from(format.nominal_fps);
    let drop = u128::from(format.dropped_per_minute());
    let f = u128::from(frame);
    let labelled = if drop == 0 {
        f
    } else {
        let per_ten = u128::from(format.frames_per_ten_minutes());
        let per_minute = per_ten / 10;
        let (blocks, rem) = (f / per_ten, f % per_ten);
        f + 9 * drop * blocks
            + if rem >= drop {
                drop * ((rem - drop) / per_minute)
            } else {
                0
            }
    };
    let total_seconds = labelled / n;
    let total_minutes = total_seconds / 60;
    Timecode {
        hours: (total_minutes / 60) as u64,
        minutes: (total_minutes % 60) as u8,
        seconds: (total_seconds % 60) as u8,
        frames: (labelled % n) as u8,
        drop_frame: format.drop_frame,
    }
}

/// Inverse of [`frames_to_timecode`]; labels that drop-frame skips are refused.
pub fn timecode_to_frames(tc: Timecode, format: TimecodeFormat) -> Result<u64> {
    let n = u128::from(format.nominal_fps);
    let drop = u128::from(format.dropped_per_minute());
    if tc.drop_frame != format.drop_frame
        || tc.minutes >= 60
        || tc.seconds >= 60
        || u128::from(tc.frames) >= n
    {
        return Err(PulseError::InvalidTimecode);
    }
    let total_minutes = u128::from(tc.hours) * 60 + u128::from(tc.minutes);
    if drop > 0
        && tc.seconds == 0
        && u128::from(tc.frames) < drop
        && !total_minutes.is_multiple_of(10)
    {
        return Err(PulseError::InvalidTimecode);
    }
    let labelled = (total_minutes * 60 + u128::from(tc.seconds)) * n + u128::from(tc.frames);
    let frame = labelled - drop * (total_minutes - total_minutes / 10);
    u64::try_from(frame).map_err(|_| PulseError::Overflow)
}

/// Display label at `ticks` (sub-frame remainder floors to the current frame).
pub fn ticks_to_timecode(ticks: Ticks, rate: FrameRate, drop_frame: bool) -> Result<Timecode> {
    let format = TimecodeFormat::new(rate, drop_frame)?;
    Ok(frames_to_timecode(
        ticks_to_frames_floor(ticks, rate),
        format,
    ))
}

/// Typed-entry path: label -> exact frame-aligned ticks.
pub fn timecode_to_ticks(tc: Timecode, rate: FrameRate, drop_frame: bool) -> Result<Ticks> {
    let format = TimecodeFormat::new(rate, drop_frame)?;
    frames_to_ticks(timecode_to_frames(tc, format)?, rate)
}

/// Ticks of the frame boundary selected by `rounding` (`Exact` refuses sub-frame values).
pub fn snap_to_frame(ticks: Ticks, rate: FrameRate, rounding: Rounding) -> Result<Ticks> {
    let tpf = rate.ticks_per_frame();
    let (frames, rem) = (ticks.value() / tpf, ticks.value() % tpf);
    let frames = match rounding {
        Rounding::Exact if rem != 0 => return Err(PulseError::NotFrameAligned),
        Rounding::Exact | Rounding::Floor => frames,
        Rounding::Ceil => frames + u64::from(rem != 0),
        Rounding::NearestHalfUp => frames + u64::from(u128::from(rem) * 2 >= u128::from(tpf)),
    };
    frames_to_ticks(frames, rate)
}

/// Parses typed timecode `H..:MM:SS:FF` (non-drop) or `H..:MM:SS;FF` (drop-frame). Field ranges
/// against a frame rate are checked later by [`timecode_to_frames`].
pub fn parse_timecode(input: &str) -> Result<Timecode> {
    if input.len() > 32 || !input.is_ascii() {
        return Err(PulseError::InvalidTimecode);
    }
    let parts: Vec<&str> = input.split([':', ';']).collect();
    let separators: Vec<char> = input.chars().filter(|c| matches!(c, ':' | ';')).collect();
    if parts.len() != 4
        || separators.len() != 3
        || separators[..2].contains(&';')
        || parts
            .iter()
            .any(|p| p.is_empty() || p.len() > 10 || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(PulseError::InvalidTimecode);
    }
    let field = |i: usize| {
        parts[i]
            .parse::<u64>()
            .map_err(|_| PulseError::InvalidTimecode)
    };
    let small = |i: usize| u8::try_from(field(i)?).map_err(|_| PulseError::InvalidTimecode);
    Ok(Timecode {
        hours: field(0)?,
        minutes: small(1)?,
        seconds: small(2)?,
        frames: small(3)?,
        drop_frame: separators[2] == ';',
    })
}
