//! Closed, stable error vocabulary. Codes never carry project text, reel names or paths.
use hsk_studio_accord::ValidationError;
use hsk_studio_observe::FailureCode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Canceled,
    InvalidUtf8,
    InputLimit,
    LineLimit,
    EventLimit,
    NoEvents,
    InvalidLine,
    InvalidEventNumber,
    DuplicateEvent,
    NonMonotonicEvent,
    InvalidTimecode,
    DroppedTimecode,
    DropMismatch,
    DropFrameUnsupported,
    TimecodeUnsupported,
    InvalidFrameRate,
    UnsupportedFrameRate,
    InvalidRange,
    LengthMismatch,
    OverlappingEvents,
    TimecodeOverflow,
    BeforeOrigin,
    InvalidSpeed,
    NegativeTime,
    InvalidRational,
    NotFrameAligned,
    TrackUnsupported,
    SpeedNotRepresentable,
    InvalidName,
    StaleRevision,
    FolioRejected,
    LimitExceeded,
    NativeDecoderMissing,
}

impl ErrorKind {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Canceled => "canceled",
            Self::InvalidUtf8 => "invalid_utf8",
            Self::InputLimit => "input_limit",
            Self::LineLimit => "line_limit",
            Self::EventLimit => "event_limit",
            Self::NoEvents => "no_events",
            Self::InvalidLine => "invalid_line",
            Self::InvalidEventNumber => "invalid_event_number",
            Self::DuplicateEvent => "duplicate_event",
            Self::NonMonotonicEvent => "non_monotonic_event",
            Self::InvalidTimecode => "invalid_timecode",
            Self::DroppedTimecode => "dropped_timecode",
            Self::DropMismatch => "drop_mismatch",
            Self::DropFrameUnsupported => "drop_frame_unsupported",
            Self::TimecodeUnsupported => "timecode_unsupported",
            Self::InvalidFrameRate => "invalid_frame_rate",
            Self::UnsupportedFrameRate => "unsupported_frame_rate",
            Self::InvalidRange => "invalid_range",
            Self::LengthMismatch => "length_mismatch",
            Self::OverlappingEvents => "overlapping_events",
            Self::TimecodeOverflow => "timecode_overflow",
            Self::BeforeOrigin => "before_origin",
            Self::InvalidSpeed => "invalid_speed",
            Self::NegativeTime => "negative_time",
            Self::InvalidRational => "invalid_rational",
            Self::NotFrameAligned => "not_frame_aligned",
            Self::TrackUnsupported => "track_unsupported",
            Self::SpeedNotRepresentable => "speed_not_representable",
            Self::InvalidName => "invalid_name",
            Self::StaleRevision => "stale_revision",
            Self::FolioRejected => "folio_rejected",
            Self::LimitExceeded => "limit_exceeded",
            Self::NativeDecoderMissing => "native_decoder_missing",
        }
    }
}

/// A typed failure; `line` is the 1-based source line when the failure belongs to one (0 = none).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InteropError {
    pub kind: ErrorKind,
    pub line: u32,
}

impl InteropError {
    pub const fn new(kind: ErrorKind) -> Self {
        Self { kind, line: 0 }
    }
    pub const fn at(kind: ErrorKind, line: u32) -> Self {
        Self { kind, line }
    }
    pub const fn code(self) -> &'static str {
        self.kind.code()
    }
    /// `None` means the terminal outcome is `Canceled`, not a failure.
    pub const fn failure_code(self) -> Option<FailureCode> {
        match self.kind {
            ErrorKind::Canceled => None,
            ErrorKind::DropFrameUnsupported
            | ErrorKind::TimecodeUnsupported
            | ErrorKind::UnsupportedFrameRate
            | ErrorKind::TrackUnsupported
            | ErrorKind::SpeedNotRepresentable
            | ErrorKind::NativeDecoderMissing => Some(FailureCode::Unsupported),
            _ => Some(FailureCode::Validation),
        }
    }
}

impl std::fmt::Display for InteropError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.line == 0 {
            f.write_str(self.code())
        } else {
            write!(f, "{}@line{}", self.code(), self.line)
        }
    }
}
impl std::error::Error for InteropError {}

impl From<ErrorKind> for InteropError {
    fn from(kind: ErrorKind) -> Self {
        Self::new(kind)
    }
}

impl From<ValidationError> for InteropError {
    fn from(value: ValidationError) -> Self {
        Self::new(match value {
            ValidationError::Canceled => ErrorKind::Canceled,
            ValidationError::TickOverflow => ErrorKind::TimecodeOverflow,
            ValidationError::UnsupportedFrameRate => ErrorKind::UnsupportedFrameRate,
            _ => ErrorKind::InvalidFrameRate,
        })
    }
}

pub type Result<T> = std::result::Result<T, InteropError>;
