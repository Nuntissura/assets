//! Typed refusals. Every code is a stable snake_case string; nothing is defaulted or guessed.
use hsk_studio_observe::FailureCode;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TiffError {
    Canceled,
    /// Not a TIFF header (byte order mark, magic, or BigTIFF offset size).
    NotTiff,
    /// A declared range lies (partly) beyond the input.
    Truncated,
    /// Structurally invalid input; the code names the check.
    Corrupt(&'static str),
    IfdCycle,
    /// IFD entry count above `Limits::max_ifd_entries`.
    IfdEntryCount,
    /// A caller limit was exceeded; the code names it (`limit_*`, `aggregate_over_budget`).
    Limit(&'static str),
    /// A size computation overflowed u64/usize (`dims_overflow`).
    DimsOverflow,
    /// A required tag is missing (tag id).
    MissingTag(u16),
    /// A valid feature this crate refuses; `value` is the offending id where one exists.
    Unsupported { code: &'static str, value: Option<u32> },
    /// The caller's request is inconsistent or outside the encoder envelope.
    Usage(&'static str),
}

impl TiffError {
    pub const fn unsupported(code: &'static str) -> Self {
        Self::Unsupported { code, value: None }
    }

    pub const fn unsupported_value(code: &'static str, value: u32) -> Self {
        Self::Unsupported { code, value: Some(value) }
    }

    pub const fn code(&self) -> &'static str {
        match self {
            Self::Canceled => "canceled",
            Self::NotTiff => "not_tiff",
            Self::Truncated => "truncated",
            Self::Corrupt(c) | Self::Limit(c) | Self::Usage(c) => c,
            Self::IfdCycle => "ifd_cycle",
            Self::IfdEntryCount => "ifd_entry_count",
            Self::DimsOverflow => "dims_overflow",
            Self::MissingTag(_) => "missing_tag",
            Self::Unsupported { code, .. } => code,
        }
    }

    /// Observe mapping: `None` is cancellation.
    pub const fn failure_code(&self) -> Option<FailureCode> {
        match self {
            Self::Canceled => None,
            Self::Unsupported { .. } => Some(FailureCode::Unsupported),
            _ => Some(FailureCode::Validation),
        }
    }
}

impl fmt::Display for TiffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported { code, value: Some(v) } => write!(f, "{code}({v})"),
            Self::MissingTag(t) => write!(f, "missing_tag({t})"),
            _ => f.write_str(self.code()),
        }
    }
}

impl std::error::Error for TiffError {}

pub type Result<T> = std::result::Result<T, TiffError>;

/// Checked arithmetic helpers (V4-01): overflow is a typed refusal, never a wrap.
pub(crate) fn cadd(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b).ok_or(TiffError::DimsOverflow)
}

pub(crate) fn cmul(a: u64, b: u64) -> Result<u64> {
    a.checked_mul(b).ok_or(TiffError::DimsOverflow)
}

pub(crate) fn to_usize(v: u64) -> Result<usize> {
    usize::try_from(v).map_err(|_| TiffError::DimsOverflow)
}
