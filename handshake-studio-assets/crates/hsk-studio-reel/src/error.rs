//! Typed, determinate failures. Codes are stable wire strings; nothing here carries file names, paths,
//! or parser text.
use hsk_studio_accord::{Ticks, ValidationError};

/// Fault of an ISOLATED decoder (a worker process or any other external decoder), as opposed to a
/// statement about the media: the stream may be perfectly valid. Distinct from `Corrupt`/`Unsupported`
/// so a consumer can retry on another decoder or report an infrastructure problem instead of blaming the
/// file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecoderFault {
    /// The decoder ended without a result: abort, native crash or killed from outside.
    Crashed,
    /// The decoder exceeded its time budget and was stopped.
    Deadline,
    /// The decoder broke the isolation protocol (garbled, stale, oversize or inconsistent reply).
    Protocol,
}

impl DecoderFault {
    /// Stable wire string.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Crashed => "decoder_crashed",
            Self::Deadline => "decoder_deadline",
            Self::Protocol => "decoder_protocol",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReelError {
    /// No adapter for the codec: determinate `MEDIA_CODEC_UNAVAILABLE`, never a silent empty render.
    CodecUnavailable {
        codec: String,
    },
    /// Container/type not recognised: determinate `MEDIA_IMPORT_UNSUPPORTED` naming the detected type.
    ImportUnsupported {
        detected: String,
    },
    Corrupt {
        what: &'static str,
    },
    /// Source ended early; `delivered` complete frames/samples/packets were already handed to the sink.
    Truncated {
        delivered: u64,
    },
    LimitExceeded {
        what: &'static str,
    },
    Canceled,
    /// Grant length/revision no longer matches the source (checked before the first payload read).
    StaleGrant,
    Unsupported {
        what: &'static str,
    },
    /// Requested time/range is outside the presentable range. Never clamped silently.
    OutOfRange {
        requested: Ticks,
        first: i64,
        end: i64,
    },
    /// Export purpose with a proxy preference: proxy fallback is refused.
    StrictOriginalRequired,
    Io(std::io::ErrorKind),
    /// An isolated/external decoder failed as a process or protocol (see `DecoderFault`); says nothing
    /// about the validity of the media.
    DecoderFault {
        fault: DecoderFault,
    },
    /// `Purpose::Export` asked for a start that is not a whole unit of the source (inside a frame or
    /// sample, or inside an edit-trimmed first sample): export is exact or it fails, so nothing is
    /// delivered. `nearest` is a start the same export can be retried at: the start of the unit the
    /// requested start falls in, which a preview delivers (test `export_is_exact_or_error_for_every_provider`);
    /// for a start inside an edit-trimmed first sample, whose own presented start no export can use, the
    /// start of the next whole presented sample, so `nearest` differs from `requested` there (test
    /// `nearest_of_a_trimmed_first_sample_is_a_start_export_accepts`).
    NotExact {
        requested: Ticks,
        nearest: Ticks,
    },
    /// `Purpose::Export` would deliver, or did decode, fewer units than `requested`. Two cases: a
    /// compressed-video decode that produced fewer pictures than requested (units already handed to the
    /// sink are delivered; this error is the statement that the export must not publish them, test
    /// `export_refuses_partial_video_decode`), and a packet range whose edit list presents one media
    /// sample more than once, which is refused before the first read or any decoder session with
    /// `delivered` the number of different media samples in the range (tests
    /// `repeated_media_sample_is_a_shortfall_not_a_silent_success`,
    /// `export_range_is_checked_before_a_decoder_session_opens`).
    Incomplete {
        requested: u64,
        delivered: u64,
    },
}

impl ReelError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::CodecUnavailable { .. } => "MEDIA_CODEC_UNAVAILABLE",
            Self::ImportUnsupported { .. } => "MEDIA_IMPORT_UNSUPPORTED",
            Self::Corrupt { .. } => "MEDIA_CORRUPT",
            Self::Truncated { .. } => "MEDIA_TRUNCATED",
            Self::LimitExceeded { .. } => "MEDIA_LIMIT_EXCEEDED",
            Self::Canceled => "MEDIA_CANCELED",
            Self::StaleGrant => "MEDIA_STALE_GRANT",
            Self::Unsupported { .. } => "MEDIA_UNSUPPORTED",
            Self::OutOfRange { .. } => "MEDIA_OUT_OF_RANGE",
            Self::StrictOriginalRequired => "MEDIA_STRICT_ORIGINAL_REQUIRED",
            Self::Io(_) => "MEDIA_IO",
            Self::DecoderFault { .. } => "MEDIA_DECODER_FAULT",
            Self::NotExact { .. } => "MEDIA_NOT_EXACT",
            Self::Incomplete { .. } => "MEDIA_INCOMPLETE",
        }
    }
}

impl std::fmt::Display for ReelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CodecUnavailable { codec } => write!(f, "{} codec={codec}", self.code()),
            Self::ImportUnsupported { detected } => {
                write!(f, "{} detected={detected}", self.code())
            }
            Self::Truncated { delivered } => write!(f, "{} delivered={delivered}", self.code()),
            Self::DecoderFault { fault } => write!(f, "{} {}", self.code(), fault.code()),
            Self::NotExact { requested, nearest } => write!(
                f,
                "{} requested={} nearest={}",
                self.code(),
                requested.value(),
                nearest.value()
            ),
            Self::Incomplete {
                requested,
                delivered,
            } => write!(
                f,
                "{} requested={requested} delivered={delivered}",
                self.code()
            ),
            Self::Corrupt { what } | Self::LimitExceeded { what } | Self::Unsupported { what } => {
                write!(f, "{} {what}", self.code())
            }
            _ => f.write_str(self.code()),
        }
    }
}

impl std::error::Error for ReelError {}

impl From<std::io::Error> for ReelError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.kind())
    }
}

/// Cancellation checkpoint shared by every provider loop.
pub(crate) fn check_cancel(cancel: &hsk_studio_accord::CancellationToken) -> Result<(), ReelError> {
    cancel.check().map_err(|_| ReelError::Canceled)
}

/// Time conversion errors from accord: a zero timescale is a corrupt header, a result that does not fit
/// 64-bit ticks is a limit.
impl From<ValidationError> for ReelError {
    fn from(error: ValidationError) -> Self {
        match error {
            ValidationError::TickOverflow => Self::LimitExceeded { what: "tick-range" },
            ValidationError::OutOfRange => Self::Corrupt {
                what: "zero-timescale",
            },
            ValidationError::Canceled => Self::Canceled,
            _ => Self::Corrupt {
                what: "time-conversion",
            },
        }
    }
}
