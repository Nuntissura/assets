//! Typed refusals. Every code is a stable snake_case string; nothing is defaulted or guessed.
use hsk_studio_observe::FailureCode;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RasterError {
    Canceled,
    /// No PNG / JPEG / WebP signature (`unknown_format`).
    UnknownFormat,
    /// The input ends before a declared structure does (chunk, segment, entropy data, IEND/EOI).
    Truncated,
    /// Structurally invalid input; the code names the check (`corrupt_png`, `corrupt_jpeg`, `icc_*`, ...).
    Corrupt(&'static str),
    /// A caller limit was exceeded before allocation (`dims_over_limit`, `decoded_bytes_over_limit`,
    /// `input_over_limit`, `icc_over_limit`, `metadata_over_limit`, `scans_over_limit`).
    Limit(&'static str),
    /// A size computation overflowed (`dims_overflow`).
    DimsOverflow,
    /// A valid feature this crate refuses (`webp_decoder_unavailable`, `arithmetic_coding_unsupported`, ...).
    Unsupported(&'static str),
    /// The caller's request is inconsistent or outside the encoder envelope.
    Usage(&'static str),
}

impl RasterError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Canceled => "canceled",
            Self::UnknownFormat => "unknown_format",
            Self::Truncated => "truncated",
            Self::DimsOverflow => "dims_overflow",
            Self::Corrupt(c) | Self::Limit(c) | Self::Unsupported(c) | Self::Usage(c) => c,
        }
    }

    /// Observe mapping: `None` is cancellation.
    pub const fn failure_code(&self) -> Option<FailureCode> {
        match self {
            Self::Canceled => None,
            Self::Unsupported(_) => Some(FailureCode::Unsupported),
            _ => Some(FailureCode::Validation),
        }
    }
}

impl fmt::Display for RasterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for RasterError {}

pub type Result<T> = std::result::Result<T, RasterError>;

/// Checked arithmetic helpers (V4-01): overflow is a typed refusal, never a wrap.
pub(crate) fn cadd(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b).ok_or(RasterError::DimsOverflow)
}

pub(crate) fn cmul(a: u64, b: u64) -> Result<u64> {
    a.checked_mul(b).ok_or(RasterError::DimsOverflow)
}

pub(crate) fn to_usize(v: u64) -> Result<usize> {
    usize::try_from(v).map_err(|_| RasterError::DimsOverflow)
}

pub(crate) fn check_cancel(cancel: &hsk_studio_accord::CancellationToken) -> Result<()> {
    cancel.check().map_err(|_| RasterError::Canceled)
}
