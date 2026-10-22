#![deny(clippy::arithmetic_side_effects)]
//! Typed failures of the fixed-format catalog decoder. Codes are stable wire strings; no message
//! ever carries file content, paths or row text.
use hsk_studio_observe::FailureCode;
use std::fmt;

/// The bound that was exceeded (checked BEFORE the corresponding allocation or read).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitKind {
    MainBytes,
    WalBytes,
    Pages,
    PageReads,
    Rows,
    Cells,
    Payload,
    OverflowPages,
    Depth,
    Columns,
    SchemaRows,
    SchemaText,
    /// Aggregate accounted memory retained by schema, lookup tables and proposals.
    RetainedText,
    Configuration,
}

impl LimitKind {
    pub const fn code(self) -> &'static str {
        match self {
            Self::MainBytes => "limit_main_bytes",
            Self::WalBytes => "limit_wal_bytes",
            Self::Pages => "limit_pages",
            Self::PageReads => "limit_page_reads",
            Self::Rows => "limit_rows",
            Self::Cells => "limit_cells",
            Self::Payload => "limit_payload",
            Self::OverflowPages => "limit_overflow_pages",
            Self::Depth => "limit_btree_depth",
            Self::Columns => "limit_columns",
            Self::SchemaRows => "limit_schema_rows",
            Self::SchemaText => "limit_schema_text",
            Self::RetainedText => "limit_retained_text",
            Self::Configuration => "limit_configuration",
        }
    }
}

/// Failure of a granted byte source (never a decoder decision).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceError {
    OutOfRange,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LrcatError {
    /// The caller's cancellation token fired; no proposal exists.
    Canceled,
    /// The main file does not start with the SQLite file magic and does not look encrypted.
    NotSqlite,
    /// Header magic is absent and the first bytes look like random data (HEURISTIC: an encrypted
    /// catalog cannot be proven encrypted without a key; the decoder never asks for one).
    Encrypted,
    /// A valid-looking but unsupported container feature (code names it).
    Unsupported(&'static str),
    /// Structural corruption detected while decoding bytes (code names it).
    Corrupt(&'static str),
    /// A page was reached twice (b-tree or overflow cycle / shared page).
    Cycle(&'static str),
    Limit(LimitKind),
    /// A `-wal` sibling exists but the caller did not grant it: the main file may be stale.
    UngrantedWal,
    /// The granted bytes differ between the fingerprint taken before and after decoding.
    SourceChanged,
    Source(SourceError),
    /// A table the extractor cannot work without is absent from the schema.
    MissingRequiredTable(&'static str),
}

pub type Result<T> = std::result::Result<T, LrcatError>;

impl LrcatError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Canceled => "canceled",
            Self::NotSqlite => "not_sqlite",
            Self::Encrypted => "encrypted_or_unrecognized",
            Self::Unsupported(c) | Self::Corrupt(c) | Self::Cycle(c) => c,
            Self::Limit(k) => k.code(),
            Self::UngrantedWal => "ungranted_wal",
            Self::SourceChanged => "source_changed",
            Self::Source(SourceError::OutOfRange) => "source_out_of_range",
            Self::Source(SourceError::Unavailable) => "source_unavailable",
            Self::MissingRequiredTable(_) => "missing_required_table",
        }
    }

    /// Observe mapping: `None` is cancellation.
    pub const fn failure_code(&self) -> Option<FailureCode> {
        match self {
            Self::Canceled => None,
            Self::UngrantedWal | Self::SourceChanged | Self::Source(_) => {
                Some(FailureCode::Unavailable)
            }
            Self::Encrypted | Self::Unsupported(_) => Some(FailureCode::Unsupported),
            Self::NotSqlite
            | Self::Corrupt(_)
            | Self::Cycle(_)
            | Self::Limit(_)
            | Self::MissingRequiredTable(_) => Some(FailureCode::Validation),
        }
    }
}

impl fmt::Display for LrcatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for LrcatError {}

impl From<SourceError> for LrcatError {
    fn from(e: SourceError) -> Self {
        Self::Source(e)
    }
}

pub(crate) fn check_cancel(cancel: &hsk_studio_accord::CancellationToken) -> Result<()> {
    cancel.check().map_err(|_| LrcatError::Canceled)
}

pub(crate) fn be16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}

pub(crate) fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}
