//! Journal, commit/archive, durability, and filesystem-capability DTOs.

use crate::{AssetId, DerivedOutputId, JobId, SchemaVersion, TransactionId};
use serde::{Deserialize, Serialize};

/// Journal durability class selected by policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurabilityClass {
    Fast,
    Balanced,
    Durable,
}

/// Ordered positions; validation requires durable <= validated <= received.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurabilityPosition {
    pub received_bytes: u64,
    pub validated_bytes: u64,
    pub durable_bytes: u64,
}

impl DurabilityPosition {
    /// Verifies the monotonic durable/validated/received ordering.
    ///
    /// # Errors
    ///
    /// Returns [`DurabilityError`] when a later stage is ahead of its prerequisite.
    pub fn validate(self) -> Result<(), DurabilityError> {
        if self.durable_bytes > self.validated_bytes {
            return Err(DurabilityError::DurableAheadOfValidated);
        }
        if self.validated_bytes > self.received_bytes {
            return Err(DurabilityError::ValidatedAheadOfReceived);
        }
        Ok(())
    }

    /// Validates a monotonic transition from this acknowledged position.
    ///
    /// This is a pure contract check. It does not flush data or establish that
    /// any operating-system persistence primitive completed.
    ///
    /// # Errors
    ///
    /// Returns [`DurabilityTransitionError`] when `next` is internally invalid
    /// or any of the three acknowledged positions regresses.
    pub fn validate_advance(self, next: Self) -> Result<(), DurabilityTransitionError> {
        next.validate()
            .map_err(DurabilityTransitionError::InvalidPosition)?;
        if next.received_bytes < self.received_bytes {
            return Err(DurabilityTransitionError::ReceivedRegressed);
        }
        if next.validated_bytes < self.validated_bytes {
            return Err(DurabilityTransitionError::ValidatedWrittenContiguousRegressed);
        }
        if next.durable_bytes < self.durable_bytes {
            return Err(DurabilityTransitionError::DurableContiguousRegressed);
        }
        Ok(())
    }

    /// Validates a requested resume offset against the acknowledged durable prefix.
    ///
    /// # Errors
    ///
    /// Returns [`DurabilityTransitionError::ResumeAheadOfDurableContiguous`] when
    /// the resume offset would trust bytes beyond `durable_bytes`.
    pub fn validate_resume(self, resume_offset: u64) -> Result<(), DurabilityTransitionError> {
        self.validate()
            .map_err(DurabilityTransitionError::InvalidPosition)?;
        if resume_offset > self.durable_bytes {
            return Err(DurabilityTransitionError::ResumeAheadOfDurableContiguous);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DurabilityError {
    DurableAheadOfValidated,
    ValidatedAheadOfReceived,
}

/// Failure of a transition between acknowledged durability positions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DurabilityTransitionError {
    InvalidPosition(DurabilityError),
    ReceivedRegressed,
    ValidatedWrittenContiguousRegressed,
    DurableContiguousRegressed,
    ResumeAheadOfDurableContiguous,
}

/// Append-only journal record with hash-chain and payload checksum fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "JournalRecordWire")]
pub struct JournalRecord {
    pub schema: SchemaVersion,
    pub job_id: JobId,
    pub producer_instance: String,
    pub sequence: u64,
    pub prior_record_hash: Option<String>,
    pub payload_checksum: String,
    pub durability: DurabilityClass,
    pub payload: JournalPayload,
}

impl JournalRecord {
    /// Verifies invariants required for an ordered durable journal entry.
    ///
    /// # Errors
    ///
    /// Returns [`JournalRecordError::SequenceZero`] when the record has no
    /// valid one-based position in the journal.
    pub fn validate(&self) -> Result<(), JournalRecordError> {
        if self.sequence == 0 {
            return Err(JournalRecordError::SequenceZero);
        }
        match &self.payload {
            JournalPayload::CommitPrepared(prepared)
                if prepared.final_rooted_path.trim().is_empty() =>
            {
                Err(JournalRecordError::PreparedFinalPathMissing)
            }
            JournalPayload::CommitPrepared(prepared)
                if prepared.working_path_identity.trim().is_empty() =>
            {
                Err(JournalRecordError::PreparedWorkingIdentityMissing)
            }
            JournalPayload::CommitPrepared(prepared) if prepared.artifacts.is_empty() => {
                Err(JournalRecordError::PreparedArtifactInventoryMissing)
            }
            JournalPayload::CommitPrepared(prepared)
                if prepared.artifacts.iter().any(|artifact| {
                    artifact.identity.trim().is_empty() || artifact.checksum.trim().is_empty()
                }) =>
            {
                Err(JournalRecordError::PreparedArtifactIdentityIncomplete)
            }
            JournalPayload::CommitPrepared(prepared)
                if prepared.filesystem_profile_id.trim().is_empty() =>
            {
                Err(JournalRecordError::PreparedFilesystemProfileMissing)
            }
            JournalPayload::CommitPrepared(prepared) if !prepared.data_synchronized => {
                Err(JournalRecordError::PreparedDataNotSynchronized)
            }
            JournalPayload::CommitPrepared(prepared) if !prepared.parent_directory_synchronized => {
                Err(JournalRecordError::PreparedDirectoryNotSynchronized)
            }
            JournalPayload::CommitRenamed(renamed) if renamed.final_identity.trim().is_empty() => {
                Err(JournalRecordError::RenamedFinalIdentityMissing)
            }
            JournalPayload::CommitRenamed(renamed) if !renamed.directory_synchronized => {
                Err(JournalRecordError::RenamedDirectoryNotSynchronized)
            }
            JournalPayload::ArchiveCommitted(archived)
                if archived.archive_row_id.trim().is_empty() =>
            {
                Err(JournalRecordError::ArchiveRowIdentityMissing)
            }
            JournalPayload::ArchiveCommitted(archived) if archived.asset_ids.is_empty() => {
                Err(JournalRecordError::ArchiveAssetIdentitiesMissing)
            }
            JournalPayload::ArchiveCommitted(archived)
                if archived.derived_output_ids.is_empty() =>
            {
                Err(JournalRecordError::ArchiveDerivedOutputIdentitiesMissing)
            }
            JournalPayload::ArchiveCommitted(archived)
                if archived.output_provenance_digest.trim().is_empty() =>
            {
                Err(JournalRecordError::ArchiveProvenanceMissing)
            }
            JournalPayload::ArchiveCommitted(archived)
                if archived.commit_sequence != self.sequence =>
            {
                Err(JournalRecordError::ArchiveCommitSequenceMismatch {
                    record_sequence: self.sequence,
                    commit_sequence: archived.commit_sequence,
                })
            }
            JournalPayload::ArchiveCommitted(archived)
                if archived.uniqueness.claim_key.trim().is_empty() =>
            {
                Err(JournalRecordError::ArchiveUniquenessClaimMissing)
            }
            JournalPayload::ArchiveCommitted(archived)
                if archived.uniqueness.constraint_receipt.trim().is_empty() =>
            {
                Err(JournalRecordError::ArchiveUniquenessReceiptMissing)
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalRecordWire {
    schema: SchemaVersion,
    job_id: JobId,
    producer_instance: String,
    sequence: u64,
    prior_record_hash: Option<String>,
    payload_checksum: String,
    durability: DurabilityClass,
    payload: JournalPayload,
}

impl TryFrom<JournalRecordWire> for JournalRecord {
    type Error = JournalRecordError;

    fn try_from(wire: JournalRecordWire) -> Result<Self, Self::Error> {
        let record = Self {
            schema: wire.schema,
            job_id: wire.job_id,
            producer_instance: wire.producer_instance,
            sequence: wire.sequence,
            prior_record_hash: wire.prior_record_hash,
            payload_checksum: wire.payload_checksum,
            durability: wire.durability,
            payload: wire.payload,
        };
        record.validate()?;
        Ok(record)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalRecordError {
    SequenceZero,
    PreparedFinalPathMissing,
    PreparedWorkingIdentityMissing,
    PreparedArtifactInventoryMissing,
    PreparedArtifactIdentityIncomplete,
    PreparedFilesystemProfileMissing,
    PreparedDataNotSynchronized,
    PreparedDirectoryNotSynchronized,
    RenamedFinalIdentityMissing,
    RenamedDirectoryNotSynchronized,
    ArchiveRowIdentityMissing,
    ArchiveAssetIdentitiesMissing,
    ArchiveDerivedOutputIdentitiesMissing,
    ArchiveProvenanceMissing,
    ArchiveCommitSequenceMismatch {
        record_sequence: u64,
        commit_sequence: u64,
    },
    ArchiveUniquenessClaimMissing,
    ArchiveUniquenessReceiptMissing,
}

impl std::fmt::Display for JournalRecordError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SequenceZero => formatter.write_str("journal sequence must be greater than zero"),
            Self::PreparedFinalPathMissing => {
                formatter.write_str("commit_prepared final rooted path is missing")
            }
            Self::PreparedWorkingIdentityMissing => {
                formatter.write_str("commit_prepared working-path identity is missing")
            }
            Self::PreparedArtifactInventoryMissing => {
                formatter.write_str("commit_prepared artifact inventory is missing")
            }
            Self::PreparedArtifactIdentityIncomplete => {
                formatter.write_str("commit_prepared artifact identity or checksum is incomplete")
            }
            Self::PreparedFilesystemProfileMissing => {
                formatter.write_str("commit_prepared filesystem profile is missing")
            }
            Self::PreparedDataNotSynchronized => {
                formatter.write_str("commit_prepared data synchronization was not acknowledged")
            }
            Self::PreparedDirectoryNotSynchronized => formatter
                .write_str("commit_prepared directory synchronization was not acknowledged"),
            Self::RenamedFinalIdentityMissing => {
                formatter.write_str("commit_renamed final identity is missing")
            }
            Self::RenamedDirectoryNotSynchronized => {
                formatter.write_str("commit_renamed directory synchronization was not acknowledged")
            }
            Self::ArchiveRowIdentityMissing => {
                formatter.write_str("archive_committed row identity is missing")
            }
            Self::ArchiveAssetIdentitiesMissing => {
                formatter.write_str("archive_committed asset identities are missing")
            }
            Self::ArchiveDerivedOutputIdentitiesMissing => {
                formatter.write_str("archive_committed derived-output identities are missing")
            }
            Self::ArchiveProvenanceMissing => {
                formatter.write_str("archive_committed output provenance is missing")
            }
            Self::ArchiveCommitSequenceMismatch {
                record_sequence,
                commit_sequence,
            } => write!(
                formatter,
                "archive_committed sequence {commit_sequence} does not match journal record sequence {record_sequence}"
            ),
            Self::ArchiveUniquenessClaimMissing => {
                formatter.write_str("archive uniqueness claim key is missing")
            }
            Self::ArchiveUniquenessReceiptMissing => {
                formatter.write_str("archive uniqueness constraint receipt is missing")
            }
        }
    }
}

impl std::error::Error for JournalRecordError {}

/// Integrity observations for one journal frame.
///
/// The caller must set `frame_complete` only after bounded framing succeeds and
/// `payload_checksum_valid` only after checking the payload bytes. The model
/// deliberately does not claim to perform I/O, hashing, or persistence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedJournalRecord {
    pub record: JournalRecord,
    pub record_hash: String,
    pub frame_complete: bool,
    pub payload_checksum_valid: bool,
}

/// First reason a journal scan stopped without trusting the affected record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum JournalPrefixFault {
    Torn {
        index: usize,
    },
    ChecksumInvalid {
        index: usize,
    },
    SequenceZero {
        index: usize,
    },
    InvalidRecord {
        index: usize,
        error: JournalRecordError,
    },
    JobIdentityMismatch {
        index: usize,
        expected_job_id: JobId,
        observed_job_id: JobId,
    },
    DuplicateSequence {
        index: usize,
        expected: u64,
        observed: u64,
    },
    ReorderedSequence {
        index: usize,
        expected: u64,
        observed: u64,
    },
    PriorHashMismatch {
        index: usize,
    },
    RecordHashMissing {
        index: usize,
    },
    InvalidCommitTransition {
        index: usize,
        durable_prefix: CommitState,
        observed: JournalCommitTransition,
    },
}

/// Commit transition named by a durable journal payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalCommitTransition {
    Prepared,
    Renamed,
    Archived,
    Cleaned,
}

/// Result of scanning a bounded observed journal sequence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalPrefixScan {
    pub valid_record_count: usize,
    pub next_sequence: Option<u64>,
    pub last_record_hash: Option<String>,
    pub stopped_by: Option<JournalPrefixFault>,
}

/// Stops at the first untrusted journal frame and returns the prior valid prefix.
///
/// Integrity booleans and record hashes are observations from the persistence
/// adapter. This pure function binds every record to `expected_job_id` and
/// validates record semantics, ordering, and linkage.
#[must_use]
pub fn scan_observed_journal(
    expected_job_id: &JobId,
    records: &[ObservedJournalRecord],
) -> JournalPrefixScan {
    let mut expected_sequence = Some(1_u64);
    let mut last_record_hash: Option<String> = None;
    let mut valid_record_count = 0_usize;
    let mut durable_prefix = CommitState::Collecting;

    for (index, observed) in records.iter().enumerate() {
        let next_durable_prefix = advance_commit_prefix(durable_prefix, &observed.record.payload);
        let fault = if !observed.frame_complete {
            Some(JournalPrefixFault::Torn { index })
        } else if !observed.payload_checksum_valid {
            Some(JournalPrefixFault::ChecksumInvalid { index })
        } else if observed.record.job_id != *expected_job_id {
            Some(JournalPrefixFault::JobIdentityMismatch {
                index,
                expected_job_id: expected_job_id.clone(),
                observed_job_id: observed.record.job_id.clone(),
            })
        } else if observed.record.sequence == 0 {
            Some(JournalPrefixFault::SequenceZero { index })
        } else if let Err(error) = observed.record.validate() {
            Some(JournalPrefixFault::InvalidRecord { index, error })
        } else if let Some(expected) = expected_sequence {
            if observed.record.sequence < expected {
                Some(JournalPrefixFault::DuplicateSequence {
                    index,
                    expected,
                    observed: observed.record.sequence,
                })
            } else if observed.record.sequence > expected {
                Some(JournalPrefixFault::ReorderedSequence {
                    index,
                    expected,
                    observed: observed.record.sequence,
                })
            } else if observed.record.prior_record_hash != last_record_hash {
                Some(JournalPrefixFault::PriorHashMismatch { index })
            } else if observed.record_hash.is_empty() {
                Some(JournalPrefixFault::RecordHashMissing { index })
            } else if let Err(observed_transition) = next_durable_prefix {
                Some(JournalPrefixFault::InvalidCommitTransition {
                    index,
                    durable_prefix,
                    observed: observed_transition,
                })
            } else {
                None
            }
        } else {
            Some(JournalPrefixFault::ReorderedSequence {
                index,
                expected: u64::MAX,
                observed: observed.record.sequence,
            })
        };

        if let Some(stopped_by) = fault {
            return JournalPrefixScan {
                valid_record_count,
                next_sequence: expected_sequence,
                last_record_hash,
                stopped_by: Some(stopped_by),
            };
        }

        valid_record_count = valid_record_count.saturating_add(1);
        expected_sequence = observed.record.sequence.checked_add(1);
        last_record_hash = Some(observed.record_hash.clone());
        if let Ok(next) = next_durable_prefix {
            durable_prefix = next;
        }
    }

    JournalPrefixScan {
        valid_record_count,
        next_sequence: expected_sequence,
        last_record_hash,
        stopped_by: None,
    }
}

fn advance_commit_prefix(
    durable_prefix: CommitState,
    payload: &JournalPayload,
) -> Result<CommitState, JournalCommitTransition> {
    let (required, next, observed) = match payload {
        JournalPayload::CommitPrepared(_) => (
            CommitState::Collecting,
            CommitState::Prepared,
            JournalCommitTransition::Prepared,
        ),
        JournalPayload::CommitRenamed(_) => (
            CommitState::Prepared,
            CommitState::Renamed,
            JournalCommitTransition::Renamed,
        ),
        JournalPayload::ArchiveCommitted(_) => (
            CommitState::Renamed,
            CommitState::Archived,
            JournalCommitTransition::Archived,
        ),
        JournalPayload::CleanupCompleted(_) => (
            CommitState::Archived,
            CommitState::Cleaned,
            JournalCommitTransition::Cleaned,
        ),
        _ => return Ok(durable_prefix),
    };
    if durable_prefix == required {
        Ok(next)
    } else {
        Err(observed)
    }
}

/// Stable journal payload kinds. Fields are descriptors, never open handles.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum JournalPayload {
    JobCreated,
    ExtractionCompleted {
        source_graph_digest: String,
    },
    SelectedFormats {
        representation_ids: Vec<String>,
    },
    OutputPlan {
        sink_count: usize,
    },
    ManifestIdentity {
        identity: String,
    },
    FragmentVerified {
        sequence: u64,
        bytes: u64,
        checksum: String,
    },
    OutputCheckpoint {
        position: DurabilityPosition,
    },
    FfmpegStarted {
        invocation_digest: String,
    },
    FfmpegCompleted {
        exit_code: i32,
    },
    FinalValidationCompleted {
        artifact_digest: String,
    },
    CommitPrepared(CommitPrepared),
    CommitRenamed(CommitRenamed),
    ArchiveCommitted(ArchiveCommitted),
    CleanupCompleted(CleanupCompleted),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitPrepared {
    pub final_rooted_path: String,
    pub working_path_identity: String,
    pub artifacts: Vec<ArtifactIdentity>,
    pub required_sidecars: Vec<String>,
    pub filesystem_profile_id: String,
    pub data_synchronized: bool,
    pub parent_directory_synchronized: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitRenamed {
    pub final_identity: String,
    pub collision_decision: CollisionDecision,
    pub directory_synchronized: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollisionDecision {
    CreatedNew,
    ReplacedAuthorized,
    ReusedIdentical,
    Rejected,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveCommitted {
    pub transaction_id: TransactionId,
    pub archive_row_id: String,
    pub asset_ids: Vec<AssetId>,
    pub derived_output_ids: Vec<DerivedOutputId>,
    pub output_provenance_digest: String,
    pub commit_sequence: u64,
    pub uniqueness: ArchiveUniquenessEvidence,
}

/// Evidence that the archive adapter enforced one stable uniqueness claim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveUniquenessEvidence {
    pub claim_key: String,
    pub constraint_receipt: String,
}

/// Complete cleanup transition payload retained in the durable journal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CleanupCompleted {
    pub removed_temporary_artifacts: Vec<String>,
    pub retained_temporary_artifacts: Vec<String>,
    pub diagnostic_retention_references: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactIdentity {
    pub identity: String,
    pub size_bytes: u64,
    pub checksum: String,
}

/// Candidate output checked before archive insertion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveCandidate {
    pub artifact: ArtifactIdentity,
    pub completeness: ArtifactCompleteness,
    pub final_validation_passed: bool,
    pub committed_output_identity: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactCompleteness {
    Partial,
    Complete,
}

impl ArchiveCandidate {
    /// Rejects partial, unvalidated, or uncommitted output before archive insertion.
    ///
    /// # Errors
    ///
    /// Returns [`ArchiveEligibilityError`] for the first unmet archive prerequisite.
    pub fn validate(&self) -> Result<(), ArchiveEligibilityError> {
        if self.completeness != ArtifactCompleteness::Complete {
            return Err(ArchiveEligibilityError::PartialOutput);
        }
        if !self.final_validation_passed {
            return Err(ArchiveEligibilityError::FinalValidationMissing);
        }
        if self.committed_output_identity.is_none() {
            return Err(ArchiveEligibilityError::CommittedIdentityMissing);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArchiveEligibilityError {
    PartialOutput,
    FinalValidationMissing,
    CommittedIdentityMissing,
}

/// Durable commit prefix. Success is legal only after `Cleaned` reconciliation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitState {
    Collecting,
    Prepared,
    Renamed,
    Archived,
    Cleaned,
    Inconsistent,
}

/// Archive/output reconciliation observation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReconcileState {
    BeforePrepared,
    PreparedNotRenamed,
    OutputWithoutArchive { final_identity: String },
    ArchiveWithoutOutput { archive_row_id: String },
    ArchivedNotCleaned,
    Reconciled,
    Inconsistent { reason: String },
}

/// Identity observation made while reconciling one durable commit prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityObservation {
    Missing,
    MatchesJournal,
    Mismatched,
}

/// Lease observation used by startup reconciliation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseObservation {
    NotPresent,
    OwnedByJob,
    Stale,
    HeldByOther,
}

/// Cleanup progress observed after an archive commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupObservation {
    NotStarted,
    Partial,
    Complete,
}

/// Schema-migration status observed at startup.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationObservation {
    NotRequired,
    Complete,
    Interrupted,
}

/// Relationship between the working and destination filesystem.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeRelationship {
    SameFilesystem,
    CrossVolume,
}

/// Collision observation at the final rooted destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollisionObservation {
    None,
    IdenticalExistingOutput,
    ConflictingExistingOutput,
}

/// Result of the rooted, no-follow confinement check for recovery paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryConfinementObservation {
    Proven,
    Unavailable,
    Mismatched,
}

/// Complete deterministic observation consumed by the recovery oracle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryObservation {
    pub durable_prefix: CommitState,
    pub staged_output: IdentityObservation,
    pub final_output: IdentityObservation,
    pub archive_row: IdentityObservation,
    pub lease: LeaseObservation,
    pub cleanup: CleanupObservation,
    pub migration: MigrationObservation,
    pub volume_relationship: VolumeRelationship,
    pub collision: CollisionObservation,
    pub confinement: RecoveryConfinementObservation,
}

/// One idempotent next action selected by the recovery oracle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAction {
    ResumeOrRestartVerifiedData,
    ReclaimStaleLease,
    ResumeInterruptedMigration,
    RevalidatePreparedThenRename,
    /// Copy, synchronize, then rename within the destination filesystem.
    /// This never claims atomicity across the preceding cross-volume copy.
    CopySyncRenameWithinDestination,
    VerifyFinalArtifactThenArchive,
    InsertArchiveRow,
    RestoreFinalFromStaged,
    VerifyArchiveOutputPair,
    RepeatCleanup,
}

/// Fail-closed recovery reasons. No variant is a successful reconciliation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryFailure {
    ExistingInconsistentPrefix,
    LeaseHeldByOther,
    ConflictingDestination,
    MismatchedStagedOutput,
    MismatchedFinalOutput,
    MismatchedArchiveRow,
    ConfinementUnavailable,
    ConfinementMismatched,
    PreparedOutputMissing,
    RenamedOutputMissing,
    ArchiveWithoutRecoverableOutput,
    ArchiveMissingForDurablePrefix,
    CleanupBeforeArchive,
    UnexpectedArtifactBeforePrepared,
}

/// Recovery oracle result for a single immutable observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum RecoveryDecision {
    Act(RecoveryAction),
    ReconciledSuccess,
    FailClosed(RecoveryFailure),
}

/// Declared path-confinement and filesystem durability capabilities.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemCapability {
    pub profile_id: String,
    pub platform: PlatformFamily,
    pub filesystem: String,
    pub path_confinement: PathConfinement,
    pub atomic_replace: CapabilitySupport,
    pub file_sync: CapabilitySupport,
    pub directory_sync: CapabilitySupport,
    pub locking: CapabilitySupport,
    pub sparse_files: CapabilitySupport,
    pub case_sensitive: bool,
    pub unicode_normalization: UnicodeNormalization,
    pub maximum_path_bytes: Option<u32>,
    pub crash_recovery: CapabilitySupport,
    pub cross_volume_commit: CrossVolumeCommit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformFamily {
    Linux,
    Windows,
    MacOs,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathConfinement {
    RootHandleBeneathNoSymlink,
    RootHandleComponentWalk,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySupport {
    Supported,
    Degraded,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnicodeNormalization {
    None,
    Nfc,
    Nfd,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrossVolumeCommit {
    CopySyncRenameNotAtomic,
    Unsupported,
}

/// Evidence ceiling assigned to a frozen filesystem profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesystemProofMode {
    SupportedLocalPositiveModel,
    UnixInteropRejectedOrExplicitlyDegraded,
}

/// Handle-relative confinement semantics required before a secure write is acknowledged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfinementContract {
    pub rooted_resolution: RootedResolution,
    pub link_traversal: LinkTraversal,
    pub final_handle_verification: FinalHandleVerification,
    pub normalization_role: NormalizationRole,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootedResolution {
    RootHandleRelative,
    NameBased,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkTraversal {
    NoFollowWithReparseInspection,
    NoFollowWithoutReparseInspection,
    MayFollow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinalHandleVerification {
    IdentityAndVolume,
    IdentityOnly,
    NotVerified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NormalizationRole {
    NonSecurityCanonicalization,
    SecurityBoundary,
}

impl ConfinementContract {
    /// Validates the model semantics required for a security-sensitive write.
    ///
    /// # Errors
    ///
    /// Returns a typed failure if the observation permits name-based or
    /// symlink/reparse-following confinement.
    pub fn validate_secure_write(&self) -> Result<(), ConfinementContractError> {
        if self.rooted_resolution != RootedResolution::RootHandleRelative {
            return Err(ConfinementContractError::RootHandleRelativeRequired);
        }
        if self.link_traversal != LinkTraversal::NoFollowWithReparseInspection {
            return Err(ConfinementContractError::NoFollowReparseInspectionRequired);
        }
        if self.final_handle_verification != FinalHandleVerification::IdentityAndVolume {
            return Err(ConfinementContractError::FinalIdentityAndVolumeRequired);
        }
        if self.normalization_role == NormalizationRole::SecurityBoundary {
            return Err(ConfinementContractError::NormalizationCannotConfine);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfinementContractError {
    RootHandleRelativeRequired,
    NoFollowReparseInspectionRequired,
    FinalIdentityAndVolumeRequired,
    NormalizationCannotConfine,
}

/// Frozen profile data. This is a model contract, not a live filesystem probe.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemProfileContract {
    pub environment: String,
    pub proof_mode: FilesystemProofMode,
    pub capability: FilesystemCapability,
    pub confinement: ConfinementContract,
    pub native_linux_durability_proven: bool,
}

/// Data-only result supplied by a platform probe.
///
/// This contract performs no host or filesystem I/O. The caller is responsible
/// for obtaining every field from an actual probe rather than copying profile
/// constants into an observation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemProbeObservation {
    pub environment: String,
    pub capability: FilesystemCapability,
    pub confinement: ConfinementContract,
    pub native_linux_durability_proven: bool,
}

impl FilesystemProfileContract {
    /// Exact local Windows profile frozen by WP-009.
    #[must_use]
    pub fn windows_11_26200_ntfs_v1() -> Self {
        Self {
            environment:
                "Microsoft Windows 11 Home 10.0.26200 build 26200; NTFS; rustc/cargo 1.97.1"
                    .to_owned(),
            proof_mode: FilesystemProofMode::SupportedLocalPositiveModel,
            capability: FilesystemCapability {
                profile_id: "ff-fs-windows-11-26200-ntfs-v1".to_owned(),
                platform: PlatformFamily::Windows,
                filesystem: "NTFS".to_owned(),
                path_confinement: PathConfinement::RootHandleComponentWalk,
                atomic_replace: CapabilitySupport::Supported,
                file_sync: CapabilitySupport::Supported,
                directory_sync: CapabilitySupport::Degraded,
                locking: CapabilitySupport::Supported,
                sparse_files: CapabilitySupport::Supported,
                case_sensitive: false,
                unicode_normalization: UnicodeNormalization::Unknown,
                maximum_path_bytes: None,
                crash_recovery: CapabilitySupport::Degraded,
                cross_volume_commit: CrossVolumeCommit::CopySyncRenameNotAtomic,
            },
            confinement: ConfinementContract {
                rooted_resolution: RootedResolution::RootHandleRelative,
                link_traversal: LinkTraversal::NoFollowWithReparseInspection,
                final_handle_verification: FinalHandleVerification::IdentityAndVolume,
                normalization_role: NormalizationRole::NonSecurityCanonicalization,
            },
            native_linux_durability_proven: false,
        }
    }

    /// Exact WSL2 interop profile frozen by WP-009.
    #[must_use]
    pub fn ubuntu_24_04_wsl2_v9fs_v1() -> Self {
        Self {
            environment: "Ubuntu 24.04 on WSL2 kernel 6.6.87.2; repository mount v9fs".to_owned(),
            proof_mode: FilesystemProofMode::UnixInteropRejectedOrExplicitlyDegraded,
            capability: FilesystemCapability {
                profile_id: "ff-fs-ubuntu-24.04-wsl2-v9fs-v1".to_owned(),
                platform: PlatformFamily::Linux,
                filesystem: "v9fs".to_owned(),
                path_confinement: PathConfinement::Unsupported,
                atomic_replace: CapabilitySupport::Degraded,
                file_sync: CapabilitySupport::Degraded,
                directory_sync: CapabilitySupport::Degraded,
                locking: CapabilitySupport::Degraded,
                sparse_files: CapabilitySupport::Degraded,
                case_sensitive: true,
                unicode_normalization: UnicodeNormalization::Unknown,
                maximum_path_bytes: None,
                crash_recovery: CapabilitySupport::Unsupported,
                cross_volume_commit: CrossVolumeCommit::Unsupported,
            },
            confinement: ConfinementContract {
                rooted_resolution: RootedResolution::NameBased,
                link_traversal: LinkTraversal::NoFollowWithoutReparseInspection,
                final_handle_verification: FinalHandleVerification::NotVerified,
                normalization_role: NormalizationRole::NonSecurityCanonicalization,
            },
            native_linux_durability_proven: false,
        }
    }

    /// Validates that the profile is one of the two exact frozen identities.
    ///
    /// # Errors
    ///
    /// Returns [`FilesystemProfileError`] for unknown or mutated profile data.
    pub fn validate_exact(&self) -> Result<(), FilesystemProfileError> {
        let expected = match self.capability.profile_id.as_str() {
            "ff-fs-windows-11-26200-ntfs-v1" => Self::windows_11_26200_ntfs_v1(),
            "ff-fs-ubuntu-24.04-wsl2-v9fs-v1" => Self::ubuntu_24_04_wsl2_v9fs_v1(),
            _ => return Err(FilesystemProfileError::UnknownProfile),
        };
        if *self != expected {
            return Err(FilesystemProfileError::ProfileDataMismatch);
        }
        Ok(())
    }

    /// Validates the exact profile and secure-write confinement ceiling.
    ///
    /// # Errors
    ///
    /// Returns a profile or confinement error. The WSL2 v9fs profile therefore
    /// fails closed for a security-sensitive write.
    pub fn validate_secure_write_profile(&self) -> Result<(), FilesystemProfileError> {
        self.validate_exact()?;
        self.capability
            .validate_secure_write()
            .map_err(|_| FilesystemProfileError::PathConfinementUnavailable)?;
        self.confinement
            .validate_secure_write()
            .map_err(FilesystemProfileError::ConfinementContract)?;
        Ok(())
    }

    /// Compares data supplied by a live probe with this exact frozen profile.
    ///
    /// This comparison does not execute a probe and therefore cannot establish
    /// that the observation was collected from the current host.
    ///
    /// # Errors
    ///
    /// Returns [`FilesystemProfileError::ProbeObservationMismatch`] if any
    /// observed platform, filesystem, capability, confinement, or evidence field
    /// differs from the selected exact profile.
    pub fn validate_probe_observation(
        &self,
        observation: &FilesystemProbeObservation,
    ) -> Result<(), FilesystemProfileError> {
        self.validate_exact()?;
        let expected = FilesystemProbeObservation {
            environment: self.environment.clone(),
            capability: self.capability.clone(),
            confinement: self.confinement.clone(),
            native_linux_durability_proven: self.native_linux_durability_proven,
        };
        if *observation != expected {
            return Err(FilesystemProfileError::ProbeObservationMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilesystemProfileError {
    UnknownProfile,
    ProfileDataMismatch,
    PathConfinementUnavailable,
    ConfinementContract(ConfinementContractError),
    ProbeObservationMismatch,
}

impl FilesystemCapability {
    /// Security-sensitive writes fail closed if root-handle confinement is unavailable.
    ///
    /// # Errors
    ///
    /// Returns [`FilesystemCapabilityError::PathConfinementUnavailable`] when confinement is unsupported.
    pub fn validate_secure_write(&self) -> Result<(), FilesystemCapabilityError> {
        if self.path_confinement == PathConfinement::Unsupported {
            Err(FilesystemCapabilityError::PathConfinementUnavailable)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilesystemCapabilityError {
    PathConfinementUnavailable,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn observed_journal_record(
        sequence: u64,
        prior_record_hash: Option<&str>,
        record_hash: &str,
    ) -> ObservedJournalRecord {
        let record = serde_json::from_value(json!({
            "schema": {"major": 1, "minor": 0},
            "job_id": "job_1",
            "producer_instance": "producer_1",
            "sequence": sequence,
            "prior_record_hash": prior_record_hash,
            "payload_checksum": "verified-by-test-adapter",
            "durability": "durable",
            "payload": {"kind": "job_created"}
        }))
        .expect("test journal record must satisfy the public wire contract");
        ObservedJournalRecord {
            record,
            record_hash: record_hash.to_owned(),
            frame_complete: true,
            payload_checksum_valid: true,
        }
    }

    fn valid_prepared() -> CommitPrepared {
        CommitPrepared {
            final_rooted_path: "final.bin".to_owned(),
            working_path_identity: "staged-identity".to_owned(),
            artifacts: vec![ArtifactIdentity {
                identity: "artifact-identity".to_owned(),
                size_bytes: 4,
                checksum: "artifact-checksum".to_owned(),
            }],
            required_sidecars: Vec::new(),
            filesystem_profile_id: "ff-fs-windows-11-26200-ntfs-v1".to_owned(),
            data_synchronized: true,
            parent_directory_synchronized: true,
        }
    }

    fn valid_archive(commit_sequence: u64) -> ArchiveCommitted {
        ArchiveCommitted {
            transaction_id: TransactionId::new("transaction_1").expect("valid transaction"),
            archive_row_id: "row-1".to_owned(),
            asset_ids: vec![AssetId::new("asset_1").expect("valid asset")],
            derived_output_ids: vec![
                DerivedOutputId::new("output_1").expect("valid derived output"),
            ],
            output_provenance_digest: "provenance-hash".to_owned(),
            commit_sequence,
            uniqueness: ArchiveUniquenessEvidence {
                claim_key: "canonical-output-key".to_owned(),
                constraint_receipt: "unique-index-receipt".to_owned(),
            },
        }
    }

    #[test]
    fn durable_position_cannot_advance_past_data() {
        assert_eq!(
            DurabilityPosition {
                received_bytes: 9,
                validated_bytes: 10,
                durable_bytes: 9
            }
            .validate(),
            Err(DurabilityError::ValidatedAheadOfReceived)
        );
        assert_eq!(
            DurabilityPosition {
                received_bytes: 10,
                validated_bytes: 9,
                durable_bytes: 10
            }
            .validate(),
            Err(DurabilityError::DurableAheadOfValidated)
        );
    }

    #[test]
    fn durability_transition_is_monotonic_and_resume_never_exceeds_durable() {
        let current = DurabilityPosition {
            received_bytes: 10,
            validated_bytes: 9,
            durable_bytes: 8,
        };
        let next = DurabilityPosition {
            received_bytes: 12,
            validated_bytes: 11,
            durable_bytes: 10,
        };
        assert_eq!(current.validate_advance(next), Ok(()));
        assert_eq!(next.validate_resume(10), Ok(()));
        assert_eq!(
            next.validate_resume(11),
            Err(DurabilityTransitionError::ResumeAheadOfDurableContiguous)
        );

        for (regressed, expected) in [
            (
                DurabilityPosition {
                    received_bytes: 9,
                    ..current
                },
                DurabilityTransitionError::ReceivedRegressed,
            ),
            (
                DurabilityPosition {
                    validated_bytes: 8,
                    ..current
                },
                DurabilityTransitionError::ValidatedWrittenContiguousRegressed,
            ),
            (
                DurabilityPosition {
                    durable_bytes: 7,
                    ..current
                },
                DurabilityTransitionError::DurableContiguousRegressed,
            ),
        ] {
            assert_eq!(current.validate_advance(regressed), Err(expected));
        }

        let maximum = DurabilityPosition {
            received_bytes: u64::MAX,
            validated_bytes: u64::MAX,
            durable_bytes: u64::MAX,
        };
        assert_eq!(maximum.validate_advance(maximum), Ok(()));
        assert_eq!(maximum.validate_resume(u64::MAX), Ok(()));
    }

    #[test]
    fn journal_faults_stop_at_the_prior_valid_prefix() {
        let expected_job_id = JobId::new("job_1").expect("valid expected job");
        let first = observed_journal_record(1, None, "record-hash-1");
        let second = observed_journal_record(2, Some("record-hash-1"), "record-hash-2");
        let third = observed_journal_record(3, Some("record-hash-2"), "record-hash-3");
        let valid = scan_observed_journal(
            &expected_job_id,
            &[first.clone(), second.clone(), third.clone()],
        );
        assert_eq!(valid.valid_record_count, 3);
        assert_eq!(valid.next_sequence, Some(4));
        assert_eq!(valid.last_record_hash.as_deref(), Some("record-hash-3"));
        assert_eq!(valid.stopped_by, None);

        let mut torn_first = first.clone();
        torn_first.frame_complete = false;
        let empty_prefix = scan_observed_journal(&expected_job_id, &[torn_first, second.clone()]);
        assert_eq!(empty_prefix.valid_record_count, 0);
        assert_eq!(empty_prefix.next_sequence, Some(1));
        assert_eq!(empty_prefix.last_record_hash, None);
        assert_eq!(
            empty_prefix.stopped_by,
            Some(JournalPrefixFault::Torn { index: 0 })
        );

        let mut cases = Vec::new();
        let mut torn = second.clone();
        torn.frame_complete = false;
        cases.push((torn, JournalPrefixFault::Torn { index: 1 }));
        let mut checksum = second.clone();
        checksum.payload_checksum_valid = false;
        cases.push((checksum, JournalPrefixFault::ChecksumInvalid { index: 1 }));
        let duplicate = observed_journal_record(1, Some("record-hash-1"), "duplicate");
        cases.push((
            duplicate,
            JournalPrefixFault::DuplicateSequence {
                index: 1,
                expected: 2,
                observed: 1,
            },
        ));
        let reordered = observed_journal_record(3, Some("record-hash-1"), "reordered");
        cases.push((
            reordered,
            JournalPrefixFault::ReorderedSequence {
                index: 1,
                expected: 2,
                observed: 3,
            },
        ));
        let wrong_prior = observed_journal_record(2, Some("wrong"), "wrong-prior");
        cases.push((
            wrong_prior,
            JournalPrefixFault::PriorHashMismatch { index: 1 },
        ));
        let mut sequence_zero = second.clone();
        sequence_zero.record.sequence = 0;
        cases.push((sequence_zero, JournalPrefixFault::SequenceZero { index: 1 }));
        let missing_hash = observed_journal_record(2, Some("record-hash-1"), "");
        cases.push((
            missing_hash,
            JournalPrefixFault::RecordHashMissing { index: 1 },
        ));

        for (invalid, expected_fault) in cases {
            let scan =
                scan_observed_journal(&expected_job_id, &[first.clone(), invalid, third.clone()]);
            assert_eq!(scan.valid_record_count, 1);
            assert_eq!(scan.next_sequence, Some(2));
            assert_eq!(scan.last_record_hash.as_deref(), Some("record-hash-1"));
            assert_eq!(scan.stopped_by, Some(expected_fault));
        }
    }

    #[test]
    fn false_commit_prerequisites_and_foreign_jobs_stop_at_the_prior_prefix() {
        let expected_job_id = JobId::new("job_1").expect("valid expected job");
        let first = observed_journal_record(1, None, "record-hash-1");
        let mut false_prepared = observed_journal_record(2, Some("record-hash-1"), "record-hash-2");
        false_prepared.record.payload = JournalPayload::CommitPrepared(CommitPrepared {
            data_synchronized: false,
            parent_directory_synchronized: false,
            ..valid_prepared()
        });
        assert_eq!(
            scan_observed_journal(&expected_job_id, &[first.clone(), false_prepared]).stopped_by,
            Some(JournalPrefixFault::InvalidRecord {
                index: 1,
                error: JournalRecordError::PreparedDataNotSynchronized,
            })
        );

        let mut false_renamed = observed_journal_record(2, Some("record-hash-1"), "record-hash-2");
        false_renamed.record.payload = JournalPayload::CommitRenamed(CommitRenamed {
            final_identity: "final-identity".to_owned(),
            collision_decision: CollisionDecision::CreatedNew,
            directory_synchronized: false,
        });
        assert_eq!(
            scan_observed_journal(&expected_job_id, &[first.clone(), false_renamed]).stopped_by,
            Some(JournalPrefixFault::InvalidRecord {
                index: 1,
                error: JournalRecordError::RenamedDirectoryNotSynchronized,
            })
        );

        let mut foreign = observed_journal_record(2, Some("record-hash-1"), "record-hash-2");
        foreign.record.job_id = JobId::new("job_2").expect("valid foreign job");
        assert_eq!(
            scan_observed_journal(&expected_job_id, &[first, foreign]).stopped_by,
            Some(JournalPrefixFault::JobIdentityMismatch {
                index: 1,
                expected_job_id,
                observed_job_id: JobId::new("job_2").expect("valid foreign job"),
            })
        );
    }

    #[test]
    fn renamed_record_requires_a_nonempty_final_identity() {
        let expected_job_id = JobId::new("job_1").expect("valid expected job");
        let mut prepared = observed_journal_record(1, None, "prepared-hash");
        prepared.record.payload = JournalPayload::CommitPrepared(valid_prepared());
        let mut renamed = observed_journal_record(2, Some("prepared-hash"), "renamed-hash");
        renamed.record.payload = JournalPayload::CommitRenamed(CommitRenamed {
            final_identity: String::new(),
            collision_decision: CollisionDecision::CreatedNew,
            directory_synchronized: true,
        });

        let scan = scan_observed_journal(&expected_job_id, &[prepared, renamed]);
        assert_eq!(scan.valid_record_count, 1);
        assert_eq!(scan.next_sequence, Some(2));
        assert_eq!(scan.last_record_hash.as_deref(), Some("prepared-hash"));
        assert_eq!(
            scan.stopped_by,
            Some(JournalPrefixFault::InvalidRecord {
                index: 1,
                error: JournalRecordError::RenamedFinalIdentityMissing,
            })
        );
    }

    #[test]
    fn commit_payloads_require_complete_durable_identities() {
        let mut prepared_record = observed_journal_record(1, None, "prepared").record;
        let mut prepared = valid_prepared();
        prepared.final_rooted_path.clear();
        prepared_record.payload = JournalPayload::CommitPrepared(prepared);
        assert_eq!(
            prepared_record.validate(),
            Err(JournalRecordError::PreparedFinalPathMissing)
        );

        let mut prepared = valid_prepared();
        prepared.artifacts.clear();
        prepared_record.payload = JournalPayload::CommitPrepared(prepared);
        assert_eq!(
            prepared_record.validate(),
            Err(JournalRecordError::PreparedArtifactInventoryMissing)
        );

        let mut prepared = valid_prepared();
        prepared.artifacts[0].checksum.clear();
        prepared_record.payload = JournalPayload::CommitPrepared(prepared);
        assert_eq!(
            prepared_record.validate(),
            Err(JournalRecordError::PreparedArtifactIdentityIncomplete)
        );

        let mut archive_record = observed_journal_record(3, None, "archived").record;
        let mut archive = valid_archive(3);
        archive.archive_row_id.clear();
        archive_record.payload = JournalPayload::ArchiveCommitted(archive);
        assert_eq!(
            archive_record.validate(),
            Err(JournalRecordError::ArchiveRowIdentityMissing)
        );

        let mut archive = valid_archive(3);
        archive.asset_ids.clear();
        archive_record.payload = JournalPayload::ArchiveCommitted(archive);
        assert_eq!(
            archive_record.validate(),
            Err(JournalRecordError::ArchiveAssetIdentitiesMissing)
        );

        let mut archive = valid_archive(3);
        archive.derived_output_ids.clear();
        archive_record.payload = JournalPayload::ArchiveCommitted(archive);
        assert_eq!(
            archive_record.validate(),
            Err(JournalRecordError::ArchiveDerivedOutputIdentitiesMissing)
        );

        let mut archive = valid_archive(3);
        archive.output_provenance_digest.clear();
        archive_record.payload = JournalPayload::ArchiveCommitted(archive);
        assert_eq!(
            archive_record.validate(),
            Err(JournalRecordError::ArchiveProvenanceMissing)
        );

        archive_record.payload = JournalPayload::ArchiveCommitted(valid_archive(99));
        assert_eq!(
            archive_record.validate(),
            Err(JournalRecordError::ArchiveCommitSequenceMismatch {
                record_sequence: 3,
                commit_sequence: 99,
            })
        );
    }

    #[test]
    fn journal_commit_payloads_follow_the_durable_prefix_fsm() {
        let expected_job_id = JobId::new("job_1").expect("valid expected job");
        let invalid_first_payloads = [
            (
                JournalPayload::CommitRenamed(CommitRenamed {
                    final_identity: "final-identity".to_owned(),
                    collision_decision: CollisionDecision::CreatedNew,
                    directory_synchronized: true,
                }),
                JournalCommitTransition::Renamed,
            ),
            (
                JournalPayload::ArchiveCommitted(valid_archive(1)),
                JournalCommitTransition::Archived,
            ),
            (
                JournalPayload::CleanupCompleted(CleanupCompleted {
                    removed_temporary_artifacts: vec!["media.part".to_owned()],
                    retained_temporary_artifacts: Vec::new(),
                    diagnostic_retention_references: Vec::new(),
                }),
                JournalCommitTransition::Cleaned,
            ),
        ];
        for (payload, observed) in invalid_first_payloads {
            let mut record = observed_journal_record(1, None, "invalid-first");
            record.record.payload = payload;
            assert_eq!(
                scan_observed_journal(&expected_job_id, &[record]).stopped_by,
                Some(JournalPrefixFault::InvalidCommitTransition {
                    index: 0,
                    durable_prefix: CommitState::Collecting,
                    observed,
                })
            );
        }

        let mut prepared = observed_journal_record(1, None, "prepared");
        prepared.record.payload = JournalPayload::CommitPrepared(valid_prepared());
        let mut renamed = observed_journal_record(2, Some("prepared"), "renamed");
        renamed.record.payload = JournalPayload::CommitRenamed(CommitRenamed {
            final_identity: "final-identity".to_owned(),
            collision_decision: CollisionDecision::CreatedNew,
            directory_synchronized: true,
        });
        let mut archived = observed_journal_record(3, Some("renamed"), "archived");
        archived.record.payload = JournalPayload::ArchiveCommitted(valid_archive(3));
        let mut cleaned = observed_journal_record(4, Some("archived"), "cleaned");
        cleaned.record.payload = JournalPayload::CleanupCompleted(CleanupCompleted {
            removed_temporary_artifacts: vec!["media.part".to_owned()],
            retained_temporary_artifacts: Vec::new(),
            diagnostic_retention_references: Vec::new(),
        });
        let scan = scan_observed_journal(&expected_job_id, &[prepared, renamed, archived, cleaned]);
        assert_eq!(scan.valid_record_count, 4);
        assert_eq!(scan.stopped_by, None);
    }

    #[test]
    fn archive_and_cleanup_payloads_are_complete_and_strict() {
        let archive = json!({
            "kind": "archive_committed",
            "body": {
                "transaction_id": "transaction_1",
                "archive_row_id": "row-1",
                "asset_ids": ["asset_1"],
                "derived_output_ids": ["output_1"],
                "output_provenance_digest": "provenance-hash",
                "commit_sequence": 4,
                "uniqueness": {
                    "claim_key": "canonical-output-key",
                    "constraint_receipt": "unique-index-receipt"
                }
            }
        });
        let archive_payload: JournalPayload =
            serde_json::from_value(archive.clone()).expect("complete archive payload");
        assert_eq!(
            serde_json::to_value(&archive_payload).expect("archive serializes"),
            archive
        );
        let mut archive_unknown = archive;
        archive_unknown["body"]["uniqueness"]["unknown"] = json!(true);
        assert!(serde_json::from_value::<JournalPayload>(archive_unknown).is_err());

        let cleanup = json!({
            "kind": "cleanup_completed",
            "body": {
                "removed_temporary_artifacts": ["media.part"],
                "retained_temporary_artifacts": ["diagnostics/failure.json"],
                "diagnostic_retention_references": ["diag-1"]
            }
        });
        let cleanup_payload: JournalPayload =
            serde_json::from_value(cleanup.clone()).expect("complete cleanup payload");
        assert_eq!(
            serde_json::to_value(&cleanup_payload).expect("cleanup serializes"),
            cleanup
        );
        let mut cleanup_missing = cleanup;
        cleanup_missing["body"]
            .as_object_mut()
            .expect("cleanup body")
            .remove("removed_temporary_artifacts");
        assert!(serde_json::from_value::<JournalPayload>(cleanup_missing).is_err());
    }

    #[test]
    fn frozen_filesystem_profiles_preserve_their_evidence_ceiling() {
        let windows = FilesystemProfileContract::windows_11_26200_ntfs_v1();
        assert_eq!(windows.validate_exact(), Ok(()));
        assert_eq!(windows.validate_secure_write_profile(), Ok(()));
        assert_eq!(windows.capability.filesystem, "NTFS");
        assert_eq!(
            windows.capability.cross_volume_commit,
            CrossVolumeCommit::CopySyncRenameNotAtomic
        );
        assert!(!windows.native_linux_durability_proven);
        let windows_observation = FilesystemProbeObservation {
            environment: windows.environment.clone(),
            capability: windows.capability.clone(),
            confinement: windows.confinement.clone(),
            native_linux_durability_proven: windows.native_linux_durability_proven,
        };
        assert_eq!(
            windows.validate_probe_observation(&windows_observation),
            Ok(())
        );
        let mut wrong_observation = windows_observation;
        wrong_observation.capability.filesystem = "ReFS".to_owned();
        assert_eq!(
            windows.validate_probe_observation(&wrong_observation),
            Err(FilesystemProfileError::ProbeObservationMismatch)
        );

        let wsl = FilesystemProfileContract::ubuntu_24_04_wsl2_v9fs_v1();
        assert_eq!(wsl.validate_exact(), Ok(()));
        assert_eq!(wsl.capability.filesystem, "v9fs");
        assert_eq!(
            wsl.proof_mode,
            FilesystemProofMode::UnixInteropRejectedOrExplicitlyDegraded
        );
        assert_eq!(
            wsl.validate_secure_write_profile(),
            Err(FilesystemProfileError::PathConfinementUnavailable)
        );
        assert!(!wsl.native_linux_durability_proven);

        let mut mislabeled = windows.clone();
        mislabeled.capability.filesystem = "ReFS".to_owned();
        assert_eq!(
            mislabeled.validate_exact(),
            Err(FilesystemProfileError::ProfileDataMismatch)
        );

        let mut follows_reparse = windows;
        follows_reparse.confinement.link_traversal = LinkTraversal::MayFollow;
        assert_eq!(
            follows_reparse.validate_secure_write_profile(),
            Err(FilesystemProfileError::ProfileDataMismatch)
        );

        let mut native_linux_overclaim = wsl;
        native_linux_overclaim.native_linux_durability_proven = true;
        assert_eq!(
            native_linux_overclaim.validate_exact(),
            Err(FilesystemProfileError::ProfileDataMismatch)
        );
    }

    #[test]
    fn unsupported_path_confinement_fails_closed() {
        let capability = FilesystemCapability {
            profile_id: "fixture".into(),
            platform: PlatformFamily::Other,
            filesystem: "unknown".into(),
            path_confinement: PathConfinement::Unsupported,
            atomic_replace: CapabilitySupport::Unsupported,
            file_sync: CapabilitySupport::Unsupported,
            directory_sync: CapabilitySupport::Unsupported,
            locking: CapabilitySupport::Unsupported,
            sparse_files: CapabilitySupport::Unsupported,
            case_sensitive: false,
            unicode_normalization: UnicodeNormalization::Unknown,
            maximum_path_bytes: None,
            crash_recovery: CapabilitySupport::Unsupported,
            cross_volume_commit: CrossVolumeCommit::Unsupported,
        };
        assert_eq!(
            capability.validate_secure_write(),
            Err(FilesystemCapabilityError::PathConfinementUnavailable)
        );
    }

    #[test]
    fn partial_output_is_archive_ineligible() {
        let candidate = ArchiveCandidate {
            artifact: ArtifactIdentity {
                identity: "partial".into(),
                size_bytes: 4,
                checksum: "hash".into(),
            },
            completeness: ArtifactCompleteness::Partial,
            final_validation_passed: true,
            committed_output_identity: Some("final".into()),
        };
        assert_eq!(
            candidate.validate(),
            Err(ArchiveEligibilityError::PartialOutput)
        );
    }
}
