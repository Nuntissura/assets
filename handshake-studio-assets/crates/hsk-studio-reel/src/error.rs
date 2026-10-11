//! Typed, determinate failures. Codes are stable wire strings; nothing here carries file names, paths,
//! or parser text.
use hsk_studio_accord::Ticks;

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
