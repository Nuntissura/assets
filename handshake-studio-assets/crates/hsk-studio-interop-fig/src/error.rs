//! Typed failures of .fig interchange. A failure carries only static codes: no project text, part names or ids.
use crate::container::FigKind;
use hsk_studio_observe::FailureCode;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FigError {
    /// The caller's cancellation token was set; no partial result exists.
    Canceled,
    /// A declared limit was hit before or while reading (`what` names the limit).
    BudgetExceeded(&'static str),
    /// The ZIP container is damaged or uses a feature this reader refuses (`what` names the check).
    Zip(&'static str),
    /// A ZIP entry name is absolute, contains `..`, a backslash, a drive colon or control characters.
    UnsafeEntryName,
    /// Two ZIP entries share a name exactly or after case folding.
    DuplicateEntry,
    /// A `canvas.fig` chunk length runs past the end of the stream (or the stream ends inside a
    /// chunk header).
    ChunkOverflow,
    /// The input is not a .fig container (`what` names the missing structure).
    NotFig(&'static str),
    /// The container opened but its (kind, version) is not in the caller's [`crate::VersionPolicy`]:
    /// nothing was decoded, the container is preserved opaque (STU-IO-187 unknown-version behaviour).
    UnsupportedVersion { kind: FigKind, found: u32 },
    /// `expected_sha256` does not match the supplied bytes.
    StaleSource,
    /// The Kiwi schema chunk is not a valid Kiwi binary schema (`what` names the check).
    SchemaInvalid(&'static str),
    /// The Kiwi message chunk does not match the schema (`what` names the check).
    MessageInvalid(&'static str),
    /// A decode stage is not available in this build or for this input (no census exists for a partly decoded message).
    DecoderUnavailable { stage: &'static str },
    /// Editable decoding is blocked until original-produced semantic/render proof exists (STU-IO-186).
    EditableDecoderBlocked { reason: &'static str },
}

impl FigError {
    /// Stable snake_case code, free of input-derived text.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Canceled => "canceled",
            Self::BudgetExceeded(_) => "budget_exceeded",
            Self::Zip(_) => "zip_invalid",
            Self::UnsafeEntryName => "unsafe_entry_name",
            Self::DuplicateEntry => "duplicate_entry",
            Self::ChunkOverflow => "chunk_overflow",
            Self::NotFig(_) => "not_fig",
            Self::UnsupportedVersion { .. } => "unsupported_version",
            Self::StaleSource => "stale_source",
            Self::SchemaInvalid(_) => "schema_invalid",
            Self::MessageInvalid(_) => "message_invalid",
            Self::DecoderUnavailable { .. } => "decoder_unavailable",
            Self::EditableDecoderBlocked { .. } => "editable_decoder_blocked",
        }
    }

    /// Observe mapping: `None` is the `Canceled` outcome. Unsupported version and unrecognised input
    /// are `Unsupported`, a missing or blocked decoder is `Unavailable` (both distinct from
    /// `Validation`, which is hostile or malformed input).
    pub const fn failure_code(&self) -> Option<FailureCode> {
        match self {
            Self::Canceled => None,
            Self::UnsupportedVersion { .. } | Self::NotFig(_) => Some(FailureCode::Unsupported),
            Self::DecoderUnavailable { .. } | Self::EditableDecoderBlocked { .. } => {
                Some(FailureCode::Unavailable)
            }
            _ => Some(FailureCode::Validation),
        }
    }
}

impl fmt::Display for FigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for FigError {}

pub type Result<T> = std::result::Result<T, FigError>;
