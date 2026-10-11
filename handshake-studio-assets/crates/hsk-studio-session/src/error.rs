//! Typed, content-free errors: stable snake_case codes only, never paths, layer names or document
//! text. `SessionError::outcome` maps every failure onto the closed Observe outcome vocabulary.
use hsk_studio_observe::{Error as ObserveError, FailureCode, Outcome, Receipt};
use hsk_studio_package::PackageError;
use std::fmt;

/// What a registered provider reports; the code is provider-owned, stable and content-free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderError {
    Canceled,
    Rejected(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistryError {
    /// Id is empty, longer than 64 bytes, or outside `[a-z0-9._-]`.
    InvalidId,
    DuplicateId,
    /// The registry already holds `MAX_PROVIDERS` entries.
    Full,
}

impl RegistryError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidId => "invalid_provider_id",
            Self::DuplicateId => "duplicate_provider",
            Self::Full => "registry_full",
        }
    }
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for RegistryError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionError {
    Canceled,
    InvalidBudget,
    Io(std::io::ErrorKind),
    Package(PackageError),
    /// Folio accepted the document only as a read-only inspection (unknown schema): it can be
    /// inspected but not edited or re-written by this session.
    ReadOnlyDocument,
    /// The caller's expected document revision (or the revision a proposal was computed on) is
    /// not the session's current revision. Nothing was changed.
    StaleRevision {
        expected: u64,
        current: u64,
    },
    ProviderUnavailable,
    /// Provider-owned stable rejection code (for example `revision_conflict`).
    Provider(&'static str),
    /// A provider returned a transaction the session refuses to commit (wrong document, not
    /// exactly one revision ahead, or property revisions that do not follow the session vector).
    ProposalInconsistent,
    /// The saved file did not read back as the revision the session wrote.
    VerifyMismatch,
    /// `save` was called on a session with no origin path.
    NoDestination,
    /// Idempotency key is empty, longer than 128 bytes or contains control characters.
    InvalidKey,
    /// A commit authority is injected, so every submit must carry a caller-supplied key (this
    /// crate never mints identifiers).
    KeyRequired,
    /// The grant port reports the grant revoked (checked afresh on every submit and retry).
    GrantRevoked,
    /// The grant port reports no grant.
    GrantMissing,
    /// Same key, different principal/resource/action/payload/expected revision.
    IdempotencyConflict,
    /// Bounded admission is full; retry after the hint. Nothing was started.
    Overloaded {
        retry_after_ms: u64,
    },
    /// The operation alone is larger than the whole admission budget; retrying cannot help.
    CostExceedsBudget,
    /// Admission was closed by `close_admission` / `drain`.
    Closed,
    /// The commit authority did not say whether the transaction was committed. The transaction
    /// is pending under its key: `reconcile` (or a retry with the same key) looks it up and
    /// adopts it exactly once or clears it; it is never blindly re-applied.
    OutcomeUnknown,
    /// An unresolved pending outcome exists; resolve it before submitting anything else.
    ReconciliationRequired,
    /// The commit authority fenced this session's epoch (a newer lease holder exists).
    StaleEpoch,
    /// Stable authority-owned rejection code.
    CommitRejected(&'static str),
    /// The authority's receipt does not match the revision or binding the session sent.
    ReceiptMismatch,
    /// The idempotency table is full (`Budgets::max_receipts`).
    ReceiptsFull,
}

impl SessionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Canceled => "canceled",
            Self::InvalidBudget => "invalid_budget",
            Self::Io(_) => "io",
            Self::Package(e) => e.code(),
            Self::ReadOnlyDocument => "read_only_document",
            Self::StaleRevision { .. } => "stale_revision",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::Provider(code) => code,
            Self::ProposalInconsistent => "proposal_inconsistent",
            Self::VerifyMismatch => "verify_mismatch",
            Self::NoDestination => "no_destination",
            Self::InvalidKey => "invalid_idempotency_key",
            Self::KeyRequired => "key_required",
            Self::GrantRevoked => "grant_revoked",
            Self::GrantMissing => "grant_missing",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::Overloaded { .. } => "overloaded",
            Self::CostExceedsBudget => "cost_exceeds_budget",
            Self::Closed => "closed",
            Self::OutcomeUnknown => "outcome_unknown",
            Self::ReconciliationRequired => "reconciliation_required",
            Self::StaleEpoch => "stale_epoch",
            Self::CommitRejected(code) => code,
            Self::ReceiptMismatch => "receipt_mismatch",
            Self::ReceiptsFull => "receipts_full",
        }
    }

    /// Terminal Observe outcome for this failure.
    pub fn outcome(&self) -> Outcome {
        match self {
            Self::Canceled => Outcome::Canceled,
            Self::Package(PackageError::Canceled) => Outcome::Canceled,
            Self::Io(_)
            | Self::ProviderUnavailable
            | Self::Overloaded { .. }
            | Self::Closed
            | Self::OutcomeUnknown
            | Self::ReconciliationRequired => Outcome::Failure(FailureCode::Unavailable),
            Self::Package(e) => hsk_studio_package::outcome_for::<()>(&Err(e.clone())),
            Self::ReadOnlyDocument => Outcome::Failure(FailureCode::Unsupported),
            Self::InvalidBudget
            | Self::StaleRevision { .. }
            | Self::Provider(_)
            | Self::ProposalInconsistent
            | Self::VerifyMismatch
            | Self::NoDestination
            | Self::InvalidKey
            | Self::KeyRequired
            | Self::GrantRevoked
            | Self::GrantMissing
            | Self::IdempotencyConflict
            | Self::CostExceedsBudget
            | Self::StaleEpoch
            | Self::CommitRejected(_)
            | Self::ReceiptMismatch
            | Self::ReceiptsFull => Outcome::Failure(FailureCode::Validation),
        }
    }

    pub(crate) fn from_package(error: PackageError) -> Self {
        match error {
            PackageError::Canceled => Self::Canceled,
            other => Self::Package(other),
        }
    }
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for SessionError {}

/// What happened to the one Observe report of an operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportStatus {
    /// The terminal frame was accepted by the sink.
    Delivered(Receipt),
    /// Observe or the sink refused it; the operation result is unaffected.
    Undelivered(ObserveError),
    /// No report was possible because the operation failed before the container identified its
    /// document (observe frames are addressed by document id).
    Unaddressed,
}

/// A failed operation together with the fate of its report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failed {
    pub error: SessionError,
    pub report: ReportStatus,
}

impl Failed {
    pub fn code(&self) -> &'static str {
        self.error.code()
    }
}

impl fmt::Display for Failed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.error.code())
    }
}

impl std::error::Error for Failed {}
