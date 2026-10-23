//! Video time-display enumeration (STU-VID-012a/012b). Display is independent of the sequence
//! frame rate and never changes stored ticks. Interpretation (UNVERIFIED against an Adobe oracle):
//! a timecode display counts frames of its OWN rate over real time.
use crate::error::{PulseError, Result};
use crate::time::{
    TimecodeFormat, frames_to_timecode, samples_floor, ticks_to_frames_floor, timebase_from_import,
};
use hsk_studio_accord::{FrameRate, TICKS_PER_SECOND, Ticks};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeDisplay {
    Timecode24,
    Timecode25,
    Timecode2997Drop,
    Timecode2997NonDrop,
    Timecode30,
    Timecode50,
    Timecode5994Drop,
    Timecode5994NonDrop,
    Timecode60,
    Frames,
    Timecode23976,
    FeetFrames16mm,
    FeetFrames35mm,
    AudioSamples,
    Milliseconds,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayImportWarning {
    /// Code 113 has no recovered meaning (STU-VID-081); the frame-rate-matched display was used.
    UnknownCode113,
}

impl TimeDisplay {
    pub const fn code(self) -> u16 {
        match self {
            Self::Timecode24 => 100,
            Self::Timecode25 => 101,
            Self::Timecode2997Drop => 102,
            Self::Timecode2997NonDrop => 103,
            Self::Timecode30 => 104,
            Self::Timecode50 => 105,
            Self::Timecode5994Drop => 106,
            Self::Timecode5994NonDrop => 107,
            Self::Timecode60 => 108,
            Self::Frames => 109,
            Self::Timecode23976 => 110,
            Self::FeetFrames16mm => 111,
            Self::FeetFrames35mm => 112,
            Self::AudioSamples => 200,
            Self::Milliseconds => 201,
        }
    }

    pub fn from_code(code: u16) -> Result<Self> {
        Ok(match code {
            100 => Self::Timecode24,
            101 => Self::Timecode25,
            102 => Self::Timecode2997Drop,
            103 => Self::Timecode2997NonDrop,
            104 => Self::Timecode30,
            105 => Self::Timecode50,
            106 => Self::Timecode5994Drop,
            107 => Self::Timecode5994NonDrop,
            108 => Self::Timecode60,
            109 => Self::Frames,
            110 => Self::Timecode23976,
            111 => Self::FeetFrames16mm,
            112 => Self::FeetFrames35mm,
            200 => Self::AudioSamples,
            201 => Self::Milliseconds,
            _ => return Err(PulseError::InvalidInput),
        })
    }

    /// `(ticks per frame, drop-frame)` for timecode displays.
    const fn timecode_base(self) -> Option<(u64, bool)> {
        Some(match self {
            Self::Timecode24 => (10_584_000_000, false),
            Self::Timecode25 => (10_160_640_000, false),
            Self::Timecode2997Drop => (8_475_667_200, true),
            Self::Timecode2997NonDrop => (8_475_667_200, false),
            Self::Timecode30 => (8_467_200_000, false),
            Self::Timecode50 => (5_080_320_000, false),
            Self::Timecode5994Drop => (4_237_833_600, true),
            Self::Timecode5994NonDrop => (4_237_833_600, false),
            Self::Timecode60 => (4_233_600_000, false),
            Self::Timecode23976 => (10_594_584_000, false),
            _ => return None,
        })
    }

    /// Non-drop timecode display matching `rate`, or `Frames` where no timecode member exists.
    pub fn matching(rate: FrameRate) -> Self {
        [
            Self::Timecode23976,
            Self::Timecode24,
            Self::Timecode25,
            Self::Timecode2997NonDrop,
            Self::Timecode30,
            Self::Timecode50,
            Self::Timecode5994NonDrop,
            Self::Timecode60,
        ]
        .into_iter()
        .find(|d| d.timecode_base() == Some((rate.ticks_per_frame(), false)))
        .unwrap_or(Self::Frames)
    }
}

/// Import mapping: code 113 (and only 113) falls back to the sequence's frame-rate-matched
/// display with a recorded warning instead of guessing a meaning (STU-VID-012b). For 29.97 and
/// 59.94 the non-drop member is chosen (UNVERIFIED interpretation of "frame-rate-matched").
pub fn time_display_from_import(
    code: u16,
    sequence_rate: FrameRate,
) -> Result<(TimeDisplay, Option<DisplayImportWarning>)> {
    if code == 113 {
        return Ok((
            TimeDisplay::matching(sequence_rate),
            Some(DisplayImportWarning::UnknownCode113),
        ));
    }
    Ok((TimeDisplay::from_code(code)?, None))
}

/// Renders `ticks` for display. Feet+frames displays are not implemented (frames per foot not
/// yet verified) and return `timecode_unsupported`; audio samples need `sample_rate_hz`.
pub fn format_ticks(
    ticks: Ticks,
    sequence_rate: FrameRate,
    display: TimeDisplay,
    sample_rate_hz: Option<u32>,
) -> Result<String> {
    if let Some((tpf, drop)) = display.timecode_base() {
        let rate = timebase_from_import(tpf)?.0;
        let format = TimecodeFormat::new(rate, drop)?;
        return Ok(frames_to_timecode(ticks_to_frames_floor(ticks, rate), format).to_string());
    }
    match display {
        TimeDisplay::Frames => Ok(ticks_to_frames_floor(ticks, sequence_rate).to_string()),
        TimeDisplay::Milliseconds => Ok((ticks.value() / (TICKS_PER_SECOND / 1000)).to_string()),
        TimeDisplay::AudioSamples => {
            let hz = sample_rate_hz.ok_or(PulseError::InvalidInput)?;
            Ok(samples_floor(ticks, hz)?.to_string())
        }
        _ => Err(PulseError::TimecodeUnsupported),
    }
}
