//! Typed failures of layout interchange. A failure never carries project text, part names or ids.
use hsk_studio_folio::Code as FolioCode;
use hsk_studio_observe::FailureCode;
use std::fmt;

/// Original bytes of a file Studio cannot edit natively, kept byte-exact so nothing is lost
/// (STU-IO-005 preservation blob; the host adds the content hash and receipt when it adopts it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreservedBlob {
    /// Short format label, for example `indd_document`.
    pub kind: &'static str,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdmlError {
    /// The caller's cancellation token was set; no partial result exists.
    Canceled,
    /// A declared limit was hit before or while reading (`what` names the limit).
    BudgetExceeded(&'static str),
    /// The ZIP container is damaged or uses a feature this reader refuses (`what` names the check).
    Zip(&'static str),
    /// An entry name is absolute, contains `..`, a backslash, a drive colon or control characters.
    UnsafeEntryName,
    /// Two entries share a name exactly or after case folding.
    DuplicateEntry,
    /// The `mimetype` entry is missing, not first, compressed or has the wrong value.
    Mimetype(&'static str),
    /// The package is a valid ZIP but not an IDML package (`what` names the missing structure).
    NotIdml(&'static str),
    /// A part is not UTF-8 or its XML is malformed (including undefined entity references).
    MalformedXml,
    /// A DOCTYPE (internal subset, entity declarations, external ids) was refused; hostile by default.
    EntityDenied,
    /// A number is not finite or a transform/bounds attribute is malformed.
    InvalidGeometry,
    /// `NextTextFrame` / `PreviousTextFrame` links loop back on themselves.
    CyclicThread,
    /// A thread link is not reciprocal (A.next = B while B.previous != A).
    ThreadNotReciprocal,
    /// A text frame names a `ParentStory` that no story part defines.
    DanglingStory,
    /// The designmap references a part the container does not hold.
    DanglingPart,
    /// The part changed since the edit was made against it.
    StaleSource,
    /// An edit does not address a plain `Content` element, or the new text is not valid XML text.
    InvalidEdit(&'static str),
    /// The Folio document or story built from the import was refused.
    Folio(FolioCode),
    /// Native InDesign editability needs a decoder that does not exist (STU-IO-186). The original
    /// bytes are preserved in `preserved`; an IDML or preview path never proves INDD editability.
    NativeEditabilityBlocked {
        reason: &'static str,
        preserved: PreservedBlob,
    },
    /// The input is not a format this adapter handles at all.
    Unsupported(&'static str),
}

impl IdmlError {
    /// Stable snake_case code, free of input-derived text.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Canceled => "canceled",
            Self::BudgetExceeded(_) => "budget_exceeded",
            Self::Zip(_) => "zip_invalid",
            Self::UnsafeEntryName => "unsafe_entry_name",
            Self::DuplicateEntry => "duplicate_entry",
            Self::Mimetype(_) => "mimetype_invalid",
            Self::NotIdml(_) => "not_idml",
            Self::MalformedXml => "malformed_xml",
            Self::EntityDenied => "entity_denied",
            Self::InvalidGeometry => "invalid_geometry",
            Self::CyclicThread => "cyclic_thread",
            Self::ThreadNotReciprocal => "thread_not_reciprocal",
            Self::DanglingStory => "dangling_story",
            Self::DanglingPart => "dangling_part",
            Self::StaleSource => "stale_source",
            Self::InvalidEdit(_) => "invalid_edit",
            Self::Folio(_) => "folio_refused",
            Self::NativeEditabilityBlocked { .. } => "native_editability_blocked",
            Self::Unsupported(_) => "unsupported",
        }
    }

    /// Observe mapping: `None` is the `Canceled` outcome.
    pub const fn failure_code(&self) -> Option<FailureCode> {
        match self {
            Self::Canceled => None,
            Self::NativeEditabilityBlocked { .. } => Some(FailureCode::Unavailable),
            Self::Unsupported(_) => Some(FailureCode::Unsupported),
            _ => Some(FailureCode::Validation),
        }
    }
}

impl fmt::Display for IdmlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for IdmlError {}

pub type Result<T> = std::result::Result<T, IdmlError>;
