//! Closed, stable error vocabulary. Codes never carry project text, ids or paths.
use hsk_studio_accord::ValidationError;
use hsk_studio_observe::FailureCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PulseError {
    Canceled,
    InvalidInput,
    UnsupportedFrameRate,
    UnsupportedSampleRate,
    Overflow,
    Underflow,
    NotFrameAligned,
    Inexact,
    ZeroSpeed,
    InvalidSpeed,
    InvalidRange,
    ZeroDuration,
    OutsideRange,
    Overlap,
    UnorderedRanges,
    NotAdjacent,
    LimitExceeded,
    TimecodeUnsupported,
    DropFrameUnsupported,
    InvalidTimecode,
}

impl PulseError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Canceled => "canceled",
            Self::InvalidInput => "invalid_input",
            Self::UnsupportedFrameRate => "unsupported_frame_rate",
            Self::UnsupportedSampleRate => "unsupported_sample_rate",
            Self::Overflow => "overflow",
            Self::Underflow => "underflow",
            Self::NotFrameAligned => "not_frame_aligned",
            Self::Inexact => "inexact",
            Self::ZeroSpeed => "zero_speed",
            Self::InvalidSpeed => "invalid_speed",
            Self::InvalidRange => "invalid_range",
            Self::ZeroDuration => "zero_duration",
            Self::OutsideRange => "outside_range",
            Self::Overlap => "overlap",
            Self::UnorderedRanges => "unordered_ranges",
            Self::NotAdjacent => "not_adjacent",
            Self::LimitExceeded => "limit_exceeded",
            Self::TimecodeUnsupported => "timecode_unsupported",
            Self::DropFrameUnsupported => "drop_frame_unsupported",
            Self::InvalidTimecode => "invalid_timecode",
        }
    }

    /// `None` means the terminal outcome is `Canceled`, not a failure.
    pub const fn failure_code(self) -> Option<FailureCode> {
        match self {
            Self::Canceled => None,
            Self::Overflow
            | Self::UnsupportedFrameRate
            | Self::UnsupportedSampleRate
            | Self::TimecodeUnsupported
            | Self::DropFrameUnsupported
            | Self::Inexact => Some(FailureCode::Unsupported),
            _ => Some(FailureCode::Validation),
        }
    }
}

impl std::fmt::Display for PulseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for PulseError {}

impl From<ValidationError> for PulseError {
    fn from(value: ValidationError) -> Self {
        match value {
            ValidationError::Canceled => Self::Canceled,
            ValidationError::TickOverflow => Self::Overflow,
            ValidationError::UnsupportedFrameRate => Self::UnsupportedFrameRate,
            _ => Self::InvalidInput,
        }
    }
}

pub type Result<T> = std::result::Result<T, PulseError>;
