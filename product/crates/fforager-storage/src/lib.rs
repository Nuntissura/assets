//! Candidate-neutral archive persistence for the WP-FF-008 Phase 0 prerequisite.
//!
//! This crate is not product-completion evidence. Its public boundary exists so
//! maintained pure-Rust store candidates can be judged against the same Ferric
//! identity, claim, lease, migration, reconciliation, and failure contract.

#![deny(unsafe_code)]

mod fetch;
pub use fetch::{DurableFetchStorage, DurableFetchWriter};

use fforager_contracts::{
    ARCHIVE_SCHEMA, ArchiveClaimOutcome, ArchiveClaimRequest, ArchiveCommitOutcome,
    ArchiveCommitRequest, ArchiveContractError, ArchiveImportBatch, ArchiveImportMarker,
    ArchiveKey, ArchiveLease, ArchiveLeaseRenewalRequest, ArchiveLimits, ArchiveMembership,
    ArchiveMigrationPhase, ArchiveMigrationPlan, ArchiveMigrationState,
    ArchiveReconciliationDecision, ArchiveReconciliationObservation, ArchiveRecord,
    ArchiveRowObservation,
};
use redb::{
    Database, Durability, ReadTransaction, ReadableDatabase, ReadableTable, ReadableTableMetadata,
    TableDefinition, WriteTransaction,
};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

const META_U64: TableDefinition<&str, u64> = TableDefinition::new("ff_archive_meta_u64_v1");
const LEASES: TableDefinition<&str, &[u8]> = TableDefinition::new("ff_archive_leases_v1");
const RECORDS_V1: TableDefinition<&str, &[u8]> = TableDefinition::new("ff_archive_records_v1");
const RECORDS_V2: TableDefinition<&str, &[u8]> = TableDefinition::new("ff_archive_records_v2");
const IMPORTS: TableDefinition<&str, &[u8]> = TableDefinition::new("ff_archive_imports_v1");
const INCONSISTENCIES: TableDefinition<&str, &[u8]> =
    TableDefinition::new("ff_archive_inconsistencies_v1");
const RECONCILIATION_RECEIPTS: TableDefinition<&str, &[u8]> =
    TableDefinition::new("ff_archive_reconciliation_receipts_v1");
const MIGRATION_RECORDS: TableDefinition<&str, &[u8]> =
    TableDefinition::new("ff_archive_migration_state_v1");

const STORE_VERSION: &str = "store_version";
const COMMIT_SEQUENCE: &str = "commit_sequence";
const ACTIVE_MIGRATION: &str = "active";
const INITIAL_STORE_VERSION: u64 = 1;
const CURRENT_STORE_VERSION: u64 = 2;

/// Observable persistence settings applied to every archive write transaction.
///
/// These are semantic guarantees at the Ferric adapter boundary. They do not
/// claim that storage hardware honors its documented flush contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArchiveDurabilityPolicy {
    /// A successful method does not return before the database commit is flushed.
    pub immediate: bool,
    /// Commit activation uses a second flush after persisting the inactive slot.
    pub two_phase_commit: bool,
    /// Allocator state is persisted so crash reopen need not scan the full store.
    pub quick_repair: bool,
}

/// Durability policy enforced by [`ArchiveStore`].
pub const ARCHIVE_DURABILITY_POLICY: ArchiveDurabilityPolicy = ArchiveDurabilityPolicy {
    immediate: true,
    two_phase_commit: true,
    quick_repair: true,
};

/// Atomic mapped-text import counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ArchiveImportResult {
    pub inserted: usize,
    pub already_present: usize,
}

/// Candidate-neutral failure returned by the archive persistence boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArchiveStoreError {
    /// A Ferric archive DTO failed contract validation.
    Contract(ArchiveContractError),
    /// The database could not be opened or repaired safely.
    OpenFailed(String),
    /// A read transaction or table operation failed.
    ReadFailed(String),
    /// A durable write could not be started, configured, or committed.
    WriteFailed(String),
    /// Persisted Ferric wire data was malformed or violated the active contract.
    CorruptRecord(String),
    /// The operation conflicts with current durable archive state.
    Conflict(String),
    /// The requested migration/import mapping is not supported.
    UnsupportedMapping(String),
    /// A configured item/byte bound was exceeded.
    LimitExceeded(String),
    /// No exact current lease exists for a lease-owned operation.
    LeaseNotFound,
    /// The supplied lease does not exactly match current durable ownership.
    LeaseMismatch,
    /// The current lease is stale at the supplied deterministic clock instant.
    LeaseExpired,
    /// The on-disk store version is not supported by this adapter.
    UnsupportedStoreVersion(u64),
    /// The requested migration is missing or in an illegal durable phase.
    MigrationState(String),
}

impl fmt::Display for ArchiveStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (kind, detail) = match self {
            Self::Contract(error) => return write!(formatter, "{error}"),
            Self::OpenFailed(detail) => ("archive open failed", detail.as_str()),
            Self::ReadFailed(detail) => ("archive read failed", detail.as_str()),
            Self::WriteFailed(detail) => ("archive write failed", detail.as_str()),
            Self::CorruptRecord(detail) => ("archive record is corrupt", detail.as_str()),
            Self::Conflict(detail) => ("archive state conflict", detail.as_str()),
            Self::UnsupportedMapping(detail) => ("unsupported archive mapping", detail.as_str()),
            Self::LimitExceeded(detail) => ("archive limit exceeded", detail.as_str()),
            Self::LeaseNotFound => ("archive lease not found", "no current lease"),
            Self::LeaseMismatch => (
                "archive lease mismatch",
                "ownership token or generation differs",
            ),
            Self::LeaseExpired => (
                "archive lease expired",
                "deterministic expiry boundary reached",
            ),
            Self::UnsupportedStoreVersion(version) => {
                return write!(formatter, "unsupported archive store version: {version}");
            }
            Self::MigrationState(detail) => ("archive migration state conflict", detail.as_str()),
        };
        write!(formatter, "{kind}: {detail}")
    }
}

impl std::error::Error for ArchiveStoreError {}

impl From<ArchiveContractError> for ArchiveStoreError {
    fn from(error: ArchiveContractError) -> Self {
        Self::Contract(error)
    }
}

/// Ferric's candidate-neutral durable archive handle.
///
/// No selected-store type crosses this public boundary. The Phase 0 candidate
/// is exact-pinned `redb` 4.1.0 behind this handle.
#[derive(Debug)]
pub struct ArchiveStore {
    database: Database,
    limits: ArchiveLimits,
}

impl ArchiveStore {
    /// Opens an existing archive or creates a new one.
    ///
    /// # Errors
    ///
    /// Returns [`ArchiveStoreError::OpenFailed`] when the file cannot be opened
    /// or the candidate refuses corrupt on-disk state.
    pub fn open(path: impl AsRef<Path>, limits: ArchiveLimits) -> Result<Self, ArchiveStoreError> {
        let path = path.as_ref();
        let database = catch_unwind(AssertUnwindSafe(|| Database::create(path)))
            .map_err(|_| {
                ArchiveStoreError::OpenFailed(
                    "database candidate panicked while rejecting malformed or torn state"
                        .to_owned(),
                )
            })?
            .map_err(|error| {
                ArchiveStoreError::OpenFailed(format!("database create/open: {error}"))
            })?;
        let store = Self { database, limits };
        store.initialize_tables()?;
        Ok(store)
    }

    /// Returns the persistence policy enforced on all write paths.
    #[must_use]
    pub const fn durability_policy(&self) -> ArchiveDurabilityPolicy {
        ARCHIVE_DURABILITY_POLICY
    }

    /// Atomically acquires one identity lease, returns the current holder, or
    /// reports an already committed row.
    ///
    /// A lease whose expiry equals `requested_at_unix_millis` is stale and may
    /// be taken over. Generation increments are persisted in the same write.
    ///
    /// # Errors
    ///
    /// Returns a contract, corruption, conflict, limit, or durable-write error.
    pub fn claim(
        &self,
        request: &ArchiveClaimRequest,
    ) -> Result<ArchiveClaimOutcome, ArchiveStoreError> {
        request.validate(self.limits)?;
        let key = request.key.canonical_key(self.limits)?;
        let transaction = self.begin_archive_write()?;
        let records_definition = active_records_definition(&transaction)?;
        if let Some(outcome) =
            self.existing_claim_outcome(&transaction, records_definition, key.as_str())?
        {
            return Ok(outcome);
        }

        let current_lease = {
            let leases = transaction
                .open_table(LEASES)
                .map_err(|error| write_detail("open leases", error))?;
            leases
                .get(key.as_str())
                .map_err(|error| write_detail("read lease", error))?
                .map(|bytes| decode_lease_for_key(key.as_str(), bytes.value(), self.limits))
                .transpose()?
        };

        if let Some(lease) = &current_lease
            && !lease.is_stale_at(request.requested_at_unix_millis)
        {
            if lease.owner_job_id == request.owner_job_id && lease.token == request.lease_token {
                if lease.claim_provenance != request.provenance {
                    return Err(ArchiveStoreError::Conflict(
                        "claim replay changed immutable transaction or provenance digests"
                            .to_owned(),
                    ));
                }
                return Ok(ArchiveClaimOutcome::Acquired {
                    lease: lease.clone(),
                });
            }
            return Ok(ArchiveClaimOutcome::HeldByOther {
                owner_job_id: lease.owner_job_id.clone(),
                generation: lease.generation,
                expires_at_unix_millis: lease.expires_at_unix_millis,
            });
        }

        let generation_key = lease_generation_key(&key);
        let generation = {
            let mut metadata = transaction
                .open_table(META_U64)
                .map_err(|error| write_detail("open metadata", error))?;
            let previous = metadata
                .get(generation_key.as_str())
                .map_err(|error| write_detail("read lease generation", error))?
                .map_or(0, |value| value.value());
            let durable_previous = current_lease
                .as_ref()
                .map_or(previous, |lease| previous.max(lease.generation));
            let next = durable_previous.checked_add(1).ok_or_else(|| {
                ArchiveStoreError::LimitExceeded("lease generation exhausted".to_owned())
            })?;
            metadata
                .insert(generation_key.as_str(), next)
                .map_err(|error| write_detail("advance lease generation", error))?;
            next
        };
        let expires_at_unix_millis = request
            .requested_at_unix_millis
            .checked_add(request.lease_duration_millis)
            .ok_or(ArchiveContractError::TimeOverflow)?;
        let lease = ArchiveLease {
            key: request.key.clone(),
            owner_job_id: request.owner_job_id.clone(),
            claim_provenance: request.provenance.clone(),
            token: request.lease_token.clone(),
            generation,
            acquired_at_unix_millis: request.requested_at_unix_millis,
            expires_at_unix_millis,
        };
        let wire = lease.to_wire_bytes(self.limits)?;
        {
            let mut leases = transaction
                .open_table(LEASES)
                .map_err(|error| write_detail("open leases", error))?;
            leases
                .insert(key.as_str(), wire.as_slice())
                .map_err(|error| write_detail("persist lease", error))?;
        }
        clear_reconciliation_receipt(&transaction, key.as_str())?;
        transaction
            .commit()
            .map_err(|error| write_detail("commit claim", error))?;
        Ok(ArchiveClaimOutcome::Acquired { lease })
    }

    // Release only the exact uncommitted claim; generation history is retained.
    fn release_claim(&self, lease: &ArchiveLease) -> Result<(), ArchiveStoreError> {
        lease.validate(self.limits)?;
        let key = lease.key.canonical_key(self.limits)?;
        let transaction = self.begin_archive_write()?;
        {
            let mut leases = transaction
                .open_table(LEASES)
                .map_err(|error| write_detail("open released lease", error))?;
            let stored = leases
                .get(key.as_str())
                .map_err(|error| write_detail("read released lease", error))?
                .map(|bytes| decode_lease_for_key(key.as_str(), bytes.value(), self.limits))
                .transpose()?;
            match stored {
                None => return Ok(()),
                Some(stored) if stored == *lease => {}
                Some(_) => return Err(ArchiveStoreError::LeaseMismatch),
            }
            leases
                .remove(key.as_str())
                .map_err(|error| write_detail("remove released lease", error))?;
        }
        transaction
            .commit()
            .map_err(|error| write_detail("commit released lease", error))
    }

    /// Renews exact current ownership with an explicit deterministic clock and
    /// a caller-generated replacement token.
    ///
    /// # Errors
    ///
    /// Returns a contract, lease ownership, corruption, or durable-write error.
    pub fn renew_lease(
        &self,
        request: &ArchiveLeaseRenewalRequest,
    ) -> Result<ArchiveLease, ArchiveStoreError> {
        request.validate(self.limits)?;
        let current = &request.current_lease;
        if current.is_stale_at(request.renewed_at_unix_millis) {
            return Err(ArchiveStoreError::LeaseExpired);
        }
        let renewed = request.renewed_lease(self.limits)?;
        let key = current.key.canonical_key(self.limits)?;
        let transaction = self.begin_archive_write()?;
        {
            let mut leases = transaction
                .open_table(LEASES)
                .map_err(|error| write_detail("open leases", error))?;
            let stored = leases
                .get(key.as_str())
                .map_err(|error| write_detail("read lease for renewal", error))?
                .map(|bytes| ArchiveLease::from_wire_bytes(bytes.value(), self.limits))
                .transpose()
                .map_err(corrupt_contract("lease"))?
                .ok_or(ArchiveStoreError::LeaseNotFound)?;
            if stored != *current {
                return Err(ArchiveStoreError::LeaseMismatch);
            }
            let generation_key = lease_generation_key(&key);
            {
                let mut metadata = transaction
                    .open_table(META_U64)
                    .map_err(|error| write_detail("open metadata", error))?;
                metadata
                    .insert(generation_key.as_str(), renewed.generation)
                    .map_err(|error| write_detail("advance renewed lease generation", error))?;
            }
            let wire = renewed.to_wire_bytes(self.limits)?;
            leases
                .insert(key.as_str(), wire.as_slice())
                .map_err(|error| write_detail("persist renewed lease", error))?;
        }
        transaction
            .commit()
            .map_err(|error| write_detail("commit lease renewal", error))?;
        Ok(renewed)
    }

    /// Atomically records a reconciled successful output and releases its exact
    /// current lease. Conflicting repeat commits fail closed.
    ///
    /// # Errors
    ///
    /// Returns a contract, lease ownership, conflict, corruption, or write error.
    pub fn commit_success(
        &self,
        request: &ArchiveCommitRequest,
    ) -> Result<ArchiveCommitOutcome, ArchiveStoreError> {
        request.validate(self.limits)?;
        if request.lease.is_stale_at(request.committed_at_unix_millis) {
            return Err(ArchiveStoreError::LeaseExpired);
        }
        let key = request.lease.key.canonical_key(self.limits)?;
        let transaction = self.begin_archive_write()?;
        let records_definition = active_records_definition(&transaction)?;
        {
            let records = transaction
                .open_table(records_definition)
                .map_err(|error| write_detail("open active records", error))?;
            if let Some(bytes) = records
                .get(key.as_str())
                .map_err(|error| write_detail("read existing record", error))?
            {
                let record = decode_record_for_key(
                    key.as_str(),
                    bytes.value(),
                    self.limits,
                    "committed record",
                )?;
                if record.key == request.lease.key
                    && record.success_event == request.success_event
                    && record.output == request.output
                    && record.provenance == request.provenance
                    && record.claim_lease_token == request.lease.token
                    && record.claim_lease_generation == request.lease.generation
                    && record.committed_at_unix_millis == request.committed_at_unix_millis
                {
                    return Ok(ArchiveCommitOutcome::AlreadyCommitted { record });
                }
                return Err(ArchiveStoreError::Conflict(
                    "identity already has a different committed output".to_owned(),
                ));
            }
        }
        {
            let leases = transaction
                .open_table(LEASES)
                .map_err(|error| write_detail("open leases", error))?;
            let stored = leases
                .get(key.as_str())
                .map_err(|error| write_detail("read lease for commit", error))?
                .map(|bytes| decode_lease_for_key(key.as_str(), bytes.value(), self.limits))
                .transpose()?
                .ok_or(ArchiveStoreError::LeaseNotFound)?;
            if stored != request.lease {
                return Err(ArchiveStoreError::LeaseMismatch);
            }
        }
        let commit_sequence = {
            let mut metadata = transaction
                .open_table(META_U64)
                .map_err(|error| write_detail("open metadata", error))?;
            let previous = metadata
                .get(COMMIT_SEQUENCE)
                .map_err(|error| write_detail("read commit sequence", error))?
                .map_or(0, |value| value.value());
            let next = previous.checked_add(1).ok_or_else(|| {
                ArchiveStoreError::LimitExceeded("commit sequence exhausted".to_owned())
            })?;
            metadata
                .insert(COMMIT_SEQUENCE, next)
                .map_err(|error| write_detail("advance commit sequence", error))?;
            next
        };
        let record = ArchiveRecord {
            schema: ARCHIVE_SCHEMA,
            archive_row_id: format!("archive-row-{commit_sequence:020}"),
            key: request.lease.key.clone(),
            success_event: request.success_event.clone(),
            output: request.output.clone(),
            provenance: request.provenance.clone(),
            claim_lease_token: request.lease.token.clone(),
            claim_lease_generation: request.lease.generation,
            commit_sequence,
            committed_at_unix_millis: request.committed_at_unix_millis,
        };
        let wire = record.to_wire_bytes(self.limits)?;
        {
            let mut records = transaction
                .open_table(records_definition)
                .map_err(|error| write_detail("open active records", error))?;
            records
                .insert(key.as_str(), wire.as_slice())
                .map_err(|error| write_detail("insert archive record", error))?;
        }
        {
            let mut leases = transaction
                .open_table(LEASES)
                .map_err(|error| write_detail("open leases", error))?;
            leases
                .remove(key.as_str())
                .map_err(|error| write_detail("release committed lease", error))?;
        }
        transaction
            .commit()
            .map_err(|error| write_detail("commit archive record", error))?;
        Ok(ArchiveCommitOutcome::Inserted { record })
    }

    /// Reads one identity without scanning or loading the archive.
    ///
    /// # Errors
    ///
    /// Returns a contract, corruption, unsupported-version, or read error.
    pub fn membership(&self, key: &ArchiveKey) -> Result<ArchiveMembership, ArchiveStoreError> {
        let canonical = key.canonical_key(self.limits)?;
        let transaction = self
            .database
            .begin_read()
            .map_err(|error| read_detail("begin membership read", error))?;
        let records_definition = active_records_definition_read(&transaction)?;
        let records = transaction
            .open_table(records_definition)
            .map_err(|error| read_detail("open active records", error))?;
        if let Some(bytes) = records
            .get(canonical.as_str())
            .map_err(|error| read_detail("read committed record", error))?
        {
            let record = decode_record_for_key(
                canonical.as_str(),
                bytes.value(),
                self.limits,
                "committed record",
            )?;
            return Ok(ArchiveMembership::Committed {
                record: Box::new(record),
            });
        }
        drop(records);
        let leases = transaction
            .open_table(LEASES)
            .map_err(|error| read_detail("open leases", error))?;
        if let Some(bytes) = leases
            .get(canonical.as_str())
            .map_err(|error| read_detail("read lease", error))?
        {
            let lease = decode_lease_for_key(canonical.as_str(), bytes.value(), self.limits)?;
            return Ok(ArchiveMembership::Claimed {
                lease: Box::new(lease),
            });
        }
        drop(leases);
        let imports = transaction
            .open_table(IMPORTS)
            .map_err(|error| read_detail("open imports", error))?;
        if let Some(bytes) = imports
            .get(canonical.as_str())
            .map_err(|error| read_detail("read import marker", error))?
        {
            let marker = decode_import_for_key(canonical.as_str(), bytes.value(), self.limits)?;
            return Ok(ArchiveMembership::Imported {
                marker: Box::new(marker),
            });
        }
        Ok(ArchiveMembership::Absent)
    }

    /// Returns whether an identity is committed or explicitly imported.
    ///
    /// # Errors
    ///
    /// Returns any membership validation or persistence error.
    pub fn contains(&self, key: &ArchiveKey) -> Result<bool, ArchiveStoreError> {
        Ok(matches!(
            self.membership(key)?,
            ArchiveMembership::Committed { .. } | ArchiveMembership::Imported { .. }
        ))
    }

    /// Reconciles one caller-observed filesystem/store state against the actual
    /// durable row and lease inside one transaction.
    ///
    /// Missing-row insertion and stale-lease reclamation are applied
    /// idempotently. Filesystem restoration remains an explicit caller action.
    /// Fail-closed decisions retain the complete bounded observation.
    ///
    /// # Errors
    ///
    /// Returns a contract, stale-observation, corruption, or durable-write error.
    #[allow(
        clippy::too_many_lines,
        reason = "the atomic reconciliation transaction keeps receipt, state validation, and each durable action together"
    )]
    pub fn reconcile(
        &self,
        observation: &ArchiveReconciliationObservation,
    ) -> Result<ArchiveReconciliationDecision, ArchiveStoreError> {
        observation.validate(self.limits)?;
        let canonical = observation.key.canonical_key(self.limits)?;
        let observation_wire = observation.to_wire_bytes(self.limits)?;
        let transaction = self.begin_archive_write()?;
        let records_definition = active_records_definition(&transaction)?;
        let (actual_record, actual_lease) =
            self.read_reconciliation_actual(&transaction, records_definition, canonical.as_str())?;

        if let Some(previous) =
            reconciliation_receipt_write(&transaction, canonical.as_str(), self.limits)?
        {
            if previous != *observation {
                return Err(ArchiveStoreError::Conflict(
                    "reconciliation replay differs from the durable applied observation".to_owned(),
                ));
            }
            return replay_applied_reconciliation(
                observation,
                actual_record.as_ref(),
                actual_lease.as_ref(),
                self.limits,
            );
        }

        validate_reconciliation_actual(observation, actual_record.as_ref(), actual_lease.as_ref())?;

        let decision = observation.decide(self.limits)?;
        match &decision {
            ArchiveReconciliationDecision::InsertMissingRow { record } => {
                let wire = record.to_wire_bytes(self.limits)?;
                {
                    let mut records = transaction
                        .open_table(records_definition)
                        .map_err(|error| write_detail("open active records", error))?;
                    records
                        .insert(canonical.as_str(), wire.as_slice())
                        .map_err(|error| write_detail("insert reconciled row", error))?;
                }
                {
                    let mut metadata = transaction
                        .open_table(META_U64)
                        .map_err(|error| write_detail("open metadata", error))?;
                    let sequence = metadata
                        .get(COMMIT_SEQUENCE)
                        .map_err(|error| write_detail("read commit sequence", error))?
                        .map_or(0, |value| value.value());
                    if record.commit_sequence > sequence {
                        metadata
                            .insert(COMMIT_SEQUENCE, record.commit_sequence)
                            .map_err(|error| {
                                write_detail("advance reconciled commit sequence", error)
                            })?;
                    }
                }
                if actual_lease.is_some() {
                    let mut leases = transaction
                        .open_table(LEASES)
                        .map_err(|error| write_detail("open leases", error))?;
                    leases
                        .remove(canonical.as_str())
                        .map_err(|error| write_detail("release reconciled lease", error))?;
                }
                persist_reconciliation_receipt(
                    &transaction,
                    canonical.as_str(),
                    observation_wire.as_slice(),
                )?;
                transaction
                    .commit()
                    .map_err(|error| write_detail("commit missing-row reconciliation", error))?;
            }
            ArchiveReconciliationDecision::ReclaimStaleLease => {
                let lease = actual_lease.ok_or(ArchiveStoreError::LeaseNotFound)?;
                if !lease.is_stale_at(observation.now_unix_millis) {
                    return Err(ArchiveStoreError::Conflict(
                        "reconciliation attempted to reclaim a fresh lease".to_owned(),
                    ));
                }
                {
                    let mut leases = transaction
                        .open_table(LEASES)
                        .map_err(|error| write_detail("open leases", error))?;
                    leases
                        .remove(canonical.as_str())
                        .map_err(|error| write_detail("reclaim stale lease", error))?;
                }
                persist_reconciliation_receipt(
                    &transaction,
                    canonical.as_str(),
                    observation_wire.as_slice(),
                )?;
                transaction
                    .commit()
                    .map_err(|error| write_detail("commit stale-lease reconciliation", error))?;
            }
            ArchiveReconciliationDecision::FailClosed { .. } => {
                let wire = observation.to_wire_bytes(self.limits)?;
                {
                    let mut inconsistencies = transaction
                        .open_table(INCONSISTENCIES)
                        .map_err(|error| write_detail("open inconsistencies", error))?;
                    inconsistencies
                        .insert(canonical.as_str(), wire.as_slice())
                        .map_err(|error| write_detail("persist inconsistency", error))?;
                }
                transaction
                    .commit()
                    .map_err(|error| write_detail("commit fail-closed reconciliation", error))?;
            }
            ArchiveReconciliationDecision::NoArchiveState
            | ArchiveReconciliationDecision::LeaseStillActive
            | ArchiveReconciliationDecision::Reconciled
            | ArchiveReconciliationDecision::RestoreOutputFromStaged => {}
        }
        Ok(decision)
    }

    /// Creates the durable checkpoint for the supported v1-to-v2 shadow-copy
    /// migration while leaving the v1 table active as last-known-good state.
    ///
    /// # Errors
    ///
    /// Returns a contract, version, existing-migration, or durable-write error.
    pub fn begin_migration(
        &self,
        plan: &ArchiveMigrationPlan,
        source_store_digest: String,
    ) -> Result<ArchiveMigrationState, ArchiveStoreError> {
        plan.validate(self.limits)?;
        if u64::from(plan.from_store_version) != INITIAL_STORE_VERSION
            || u64::from(plan.to_store_version) != CURRENT_STORE_VERSION
        {
            return Err(ArchiveStoreError::UnsupportedMapping(
                "only store migration 1 -> 2 is implemented".to_owned(),
            ));
        }
        let state = ArchiveMigrationState {
            plan: plan.clone(),
            phase: ArchiveMigrationPhase::Prepared,
            migrated_records: 0,
            last_migrated_key: None,
            source_store_digest,
        };
        state.validate(self.limits)?;
        let transaction = self.begin_archive_write()?;
        if active_store_version(&transaction)? != INITIAL_STORE_VERSION {
            return Err(ArchiveStoreError::MigrationState(
                "source store version is not active".to_owned(),
            ));
        }
        {
            let target = transaction
                .open_table(RECORDS_V2)
                .map_err(|error| write_detail("open migration target", error))?;
            if target
                .len()
                .map_err(|error| write_detail("measure migration target", error))?
                != 0
            {
                return Err(ArchiveStoreError::CorruptRecord(
                    "v2 target contains rows without an active migration checkpoint".to_owned(),
                ));
            }
        }
        {
            let migrations = transaction
                .open_table(MIGRATION_RECORDS)
                .map_err(|error| write_detail("open migration state", error))?;
            if migrations
                .get(ACTIVE_MIGRATION)
                .map_err(|error| write_detail("read migration state", error))?
                .is_some()
            {
                return Err(ArchiveStoreError::MigrationState(
                    "a migration checkpoint already exists".to_owned(),
                ));
            }
        }
        let wire = state.to_wire_bytes(self.limits)?;
        {
            let mut migrations = transaction
                .open_table(MIGRATION_RECORDS)
                .map_err(|error| write_detail("open migration state", error))?;
            migrations
                .insert(ACTIVE_MIGRATION, wire.as_slice())
                .map_err(|error| write_detail("persist prepared migration", error))?;
        }
        transaction
            .commit()
            .map_err(|error| write_detail("commit prepared migration", error))?;
        Ok(state)
    }

    /// Resumes at most the plan's bounded number of records from the durable
    /// canonical-key cursor. Repeating after interruption is idempotent.
    ///
    /// # Errors
    ///
    /// Returns a phase, identity, corruption, limit, or durable-write error.
    pub fn resume_migration(
        &self,
        migration_id: &str,
    ) -> Result<ArchiveMigrationState, ArchiveStoreError> {
        let transaction = self.begin_durable_write()?;
        let mut state = migration_state_write(&transaction, self.limits)?
            .ok_or_else(|| ArchiveStoreError::MigrationState("no migration exists".to_owned()))?;
        require_migration_id(&state, migration_id)?;
        if !matches!(
            state.phase,
            ArchiveMigrationPhase::Prepared | ArchiveMigrationPhase::Copying
        ) {
            return Err(ArchiveStoreError::MigrationState(
                "migration is not resumable from its current phase".to_owned(),
            ));
        }
        if active_store_version(&transaction)? != INITIAL_STORE_VERSION {
            return Err(ArchiveStoreError::MigrationState(
                "last-known-good source is not active".to_owned(),
            ));
        }
        let cursor = state
            .last_migrated_key
            .as_ref()
            .map(|key| key.canonical_key(self.limits))
            .transpose()?;
        let mut migrated_this_batch = 0_u32;
        let mut last_key = None;
        {
            let source = transaction
                .open_table(RECORDS_V1)
                .map_err(|error| write_detail("open migration source", error))?;
            let mut target = transaction
                .open_table(RECORDS_V2)
                .map_err(|error| write_detail("open migration target", error))?;
            let mut range = match cursor.as_deref() {
                Some(start) => source.range(start..),
                None => source.iter(),
            }
            .map_err(|error| write_detail("iterate migration source", error))?;
            while migrated_this_batch < state.plan.maximum_records_per_batch {
                let Some(entry) = range.next() else {
                    break;
                };
                let (key, value) =
                    entry.map_err(|error| write_detail("read migration source record", error))?;
                if cursor.as_deref() == Some(key.value()) {
                    continue;
                }
                let record = decode_record_for_key(
                    key.value(),
                    value.value(),
                    self.limits,
                    "migration source record",
                )?;
                if let Some(existing) = target
                    .get(key.value())
                    .map_err(|error| write_detail("read migration target record", error))?
                {
                    let _existing_record = decode_record_for_key(
                        key.value(),
                        existing.value(),
                        self.limits,
                        "migration target record",
                    )?;
                    if existing.value() != value.value() {
                        return Err(ArchiveStoreError::Conflict(format!(
                            "migration target already differs at {}",
                            key.value()
                        )));
                    }
                }
                target
                    .insert(key.value(), value.value())
                    .map_err(|error| write_detail("copy migration record", error))?;
                migrated_this_batch += 1;
                last_key = Some(record.key);
            }
        }
        state.migrated_records = state
            .migrated_records
            .checked_add(u64::from(migrated_this_batch))
            .ok_or_else(|| {
                ArchiveStoreError::LimitExceeded("migration record count exhausted".to_owned())
            })?;
        if let Some(key) = last_key {
            state.last_migrated_key = Some(key);
        }
        state.phase = if migrated_this_batch < state.plan.maximum_records_per_batch {
            ArchiveMigrationPhase::Verifying
        } else {
            ArchiveMigrationPhase::Copying
        };
        persist_migration_state(&transaction, &state, self.limits)?;
        transaction
            .commit()
            .map_err(|error| write_detail("commit migration batch", error))?;
        Ok(state)
    }

    /// Streams both generations and proves exact key/value equality before the
    /// target can become active.
    ///
    /// # Errors
    ///
    /// Returns a phase, divergence, corruption, limit, or durable-write error.
    pub fn verify_migration(
        &self,
        migration_id: &str,
    ) -> Result<ArchiveMigrationState, ArchiveStoreError> {
        let transaction = self.begin_durable_write()?;
        let mut state = migration_state_write(&transaction, self.limits)?
            .ok_or_else(|| ArchiveStoreError::MigrationState("no migration exists".to_owned()))?;
        require_migration_id(&state, migration_id)?;
        if state.phase != ArchiveMigrationPhase::Verifying {
            return Err(ArchiveStoreError::MigrationState(
                "migration has not completed copying".to_owned(),
            ));
        }
        validate_migration_cursor(&transaction, &state, self.limits)?;
        let verified_records = verify_source_target_exact(&transaction, self.limits)?;
        if verified_records != state.migrated_records {
            return Err(ArchiveStoreError::Conflict(format!(
                "migration checkpoint count {} differs from verified {verified_records}",
                state.migrated_records
            )));
        }
        state.phase = ArchiveMigrationPhase::ReadyToActivate;
        persist_migration_state(&transaction, &state, self.limits)?;
        transaction
            .commit()
            .map_err(|error| write_detail("commit verified migration", error))?;
        Ok(state)
    }

    /// Atomically switches membership reads and writes to the verified target.
    ///
    /// # Errors
    ///
    /// Returns a phase, identity, corruption, or durable-write error.
    pub fn activate_migration(
        &self,
        migration_id: &str,
    ) -> Result<ArchiveMigrationState, ArchiveStoreError> {
        let transaction = self.begin_durable_write()?;
        let mut state = migration_state_write(&transaction, self.limits)?
            .ok_or_else(|| ArchiveStoreError::MigrationState("no migration exists".to_owned()))?;
        require_migration_id(&state, migration_id)?;
        if state.phase != ArchiveMigrationPhase::ReadyToActivate {
            return Err(ArchiveStoreError::MigrationState(
                "migration is not verified and ready".to_owned(),
            ));
        }
        if active_store_version(&transaction)? != INITIAL_STORE_VERSION {
            return Err(ArchiveStoreError::CorruptRecord(
                "ready migration checkpoint is not coupled to active v1 source".to_owned(),
            ));
        }
        validate_migration_cursor(&transaction, &state, self.limits)?;
        let verified_records = verify_source_target_exact(&transaction, self.limits)?;
        if verified_records != state.migrated_records {
            return Err(ArchiveStoreError::Conflict(format!(
                "migration changed after verification: checkpoint={}, current={verified_records}",
                state.migrated_records
            )));
        }
        {
            let mut metadata = transaction
                .open_table(META_U64)
                .map_err(|error| write_detail("open metadata", error))?;
            metadata
                .insert(STORE_VERSION, CURRENT_STORE_VERSION)
                .map_err(|error| write_detail("activate target store version", error))?;
        }
        state.phase = ArchiveMigrationPhase::Activated;
        persist_migration_state(&transaction, &state, self.limits)?;
        transaction
            .commit()
            .map_err(|error| write_detail("commit migration activation", error))?;
        Ok(state)
    }

    /// Restores the preserved v1 generation as active without deleting either
    /// generation, leaving both available for forensic inspection.
    ///
    /// # Errors
    ///
    /// Returns a missing-state, identity, corruption, or durable-write error.
    pub fn rollback_migration(
        &self,
        migration_id: &str,
    ) -> Result<ArchiveMigrationState, ArchiveStoreError> {
        let transaction = self.begin_durable_write()?;
        let mut state = migration_state_write(&transaction, self.limits)?
            .ok_or_else(|| ArchiveStoreError::MigrationState("no migration exists".to_owned()))?;
        require_migration_id(&state, migration_id)?;
        if state.phase == ArchiveMigrationPhase::RolledBack {
            return Ok(state);
        }
        if state.phase == ArchiveMigrationPhase::Activated {
            return Err(ArchiveStoreError::MigrationState(
                "activated migration cannot roll back because v2 may contain later writes"
                    .to_owned(),
            ));
        }
        if active_store_version(&transaction)? != INITIAL_STORE_VERSION {
            return Err(ArchiveStoreError::CorruptRecord(
                "non-activated migration is not coupled to active v1 source".to_owned(),
            ));
        }
        {
            let mut metadata = transaction
                .open_table(META_U64)
                .map_err(|error| write_detail("open metadata", error))?;
            metadata
                .insert(STORE_VERSION, INITIAL_STORE_VERSION)
                .map_err(|error| write_detail("restore source store version", error))?;
        }
        state.phase = ArchiveMigrationPhase::RolledBack;
        persist_migration_state(&transaction, &state, self.limits)?;
        transaction
            .commit()
            .map_err(|error| write_detail("commit migration rollback", error))?;
        Ok(state)
    }

    /// Returns the sole durable migration checkpoint, if any.
    ///
    /// # Errors
    ///
    /// Returns a corruption or persistence-read error.
    pub fn migration_state(&self) -> Result<Option<ArchiveMigrationState>, ArchiveStoreError> {
        let transaction = self
            .database
            .begin_read()
            .map_err(|error| read_detail("begin migration-state read", error))?;
        let migrations = transaction
            .open_table(MIGRATION_RECORDS)
            .map_err(|error| read_detail("open migration state", error))?;
        migrations
            .get(ACTIVE_MIGRATION)
            .map_err(|error| read_detail("read migration state", error))?
            .map(|bytes| decode_migration_state(bytes.value(), self.limits))
            .transpose()
    }

    /// Atomically imports one bounded, explicitly mapped Ferric text batch.
    /// Unknown formats or identity-level mappings are rejected by the contract
    /// before this write path begins.
    ///
    /// # Errors
    ///
    /// Returns a contract, lease/provenance conflict, limit, or write error.
    pub fn import_mapped_text(
        &self,
        batch: &ArchiveImportBatch,
        imported_at_unix_millis: u64,
    ) -> Result<ArchiveImportResult, ArchiveStoreError> {
        batch.validate(self.limits)?;
        let transaction = self.begin_archive_write()?;
        let records_definition = active_records_definition(&transaction)?;
        let mut result = ArchiveImportResult::default();
        {
            let records = transaction
                .open_table(records_definition)
                .map_err(|error| write_detail("open active records", error))?;
            let leases = transaction
                .open_table(LEASES)
                .map_err(|error| write_detail("open leases", error))?;
            let mut imports = transaction
                .open_table(IMPORTS)
                .map_err(|error| write_detail("open imports", error))?;
            for entry in &batch.entries {
                let key = entry.target_key.canonical_key(self.limits)?;
                if let Some(bytes) = leases
                    .get(key.as_str())
                    .map_err(|error| write_detail("read import lease conflict", error))?
                {
                    let _lease = decode_lease_for_key(key.as_str(), bytes.value(), self.limits)?;
                    return Err(ArchiveStoreError::Conflict(format!(
                        "mapped import identity has an active lease: {key}"
                    )));
                }
                if let Some(bytes) = records
                    .get(key.as_str())
                    .map_err(|error| write_detail("read import record conflict", error))?
                {
                    let _record = decode_record_for_key(
                        key.as_str(),
                        bytes.value(),
                        self.limits,
                        "committed record",
                    )?;
                    result.already_present =
                        result.already_present.checked_add(1).ok_or_else(|| {
                            ArchiveStoreError::LimitExceeded(
                                "import result count exhausted".to_owned(),
                            )
                        })?;
                    continue;
                }
                let marker = ArchiveImportMarker::from_entry(
                    entry,
                    batch.source_digest.clone(),
                    imported_at_unix_millis,
                    self.limits,
                )?;
                if let Some(existing) = imports
                    .get(key.as_str())
                    .map_err(|error| write_detail("read existing import marker", error))?
                {
                    let existing =
                        decode_import_for_key(key.as_str(), existing.value(), self.limits)?;
                    if existing.key != marker.key
                        || existing.mapping != marker.mapping
                        || existing.source_identity != marker.source_identity
                        || existing.source_digest != marker.source_digest
                        || existing.source_line_number != marker.source_line_number
                    {
                        return Err(ArchiveStoreError::Conflict(format!(
                            "mapped import identity has different provenance: {key}"
                        )));
                    }
                    result.already_present =
                        result.already_present.checked_add(1).ok_or_else(|| {
                            ArchiveStoreError::LimitExceeded(
                                "import result count exhausted".to_owned(),
                            )
                        })?;
                    continue;
                }
                let wire = marker.to_wire_bytes(self.limits)?;
                imports
                    .insert(key.as_str(), wire.as_slice())
                    .map_err(|error| write_detail("insert import marker", error))?;
                clear_reconciliation_receipt(&transaction, key.as_str())?;
                result.inserted = result.inserted.checked_add(1).ok_or_else(|| {
                    ArchiveStoreError::LimitExceeded("import result count exhausted".to_owned())
                })?;
            }
        }
        transaction
            .commit()
            .map_err(|error| write_detail("commit mapped import", error))?;
        Ok(result)
    }

    fn initialize_tables(&self) -> Result<(), ArchiveStoreError> {
        let transaction = self.begin_durable_write()?;
        transaction
            .open_table(META_U64)
            .map_err(write_error("open metadata table"))?;
        transaction
            .open_table(LEASES)
            .map_err(write_error("open lease table"))?;
        transaction
            .open_table(RECORDS_V1)
            .map_err(write_error("open archive-record table"))?;
        transaction
            .open_table(RECORDS_V2)
            .map_err(write_error("open migrated archive-record table"))?;
        transaction
            .open_table(IMPORTS)
            .map_err(write_error("open import table"))?;
        transaction
            .open_table(MIGRATION_RECORDS)
            .map_err(write_error("open migration table"))?;
        transaction
            .open_table(RECONCILIATION_RECEIPTS)
            .map_err(write_error("open reconciliation-receipt table"))?;
        let version = {
            let metadata = transaction
                .open_table(META_U64)
                .map_err(write_error("read metadata table"))?;
            metadata
                .get(STORE_VERSION)
                .map_err(|error| write_detail("read store version", error))?
                .map(|value| value.value())
        };
        match version {
            Some(INITIAL_STORE_VERSION | CURRENT_STORE_VERSION) => {}
            Some(version) => return Err(ArchiveStoreError::UnsupportedStoreVersion(version)),
            None => {
                if initialized_tables_have_state(&transaction)? {
                    return Err(ArchiveStoreError::CorruptRecord(
                        "store version is missing from a non-empty archive".to_owned(),
                    ));
                }
                let mut metadata = transaction
                    .open_table(META_U64)
                    .map_err(write_error("open metadata table"))?;
                metadata
                    .insert(STORE_VERSION, INITIAL_STORE_VERSION)
                    .map_err(|error| write_detail("initialize store version", error))?;
                metadata
                    .insert(COMMIT_SEQUENCE, 0)
                    .map_err(|error| write_detail("initialize commit sequence", error))?;
            }
        }
        validate_store_topology(
            &transaction,
            version.unwrap_or(INITIAL_STORE_VERSION),
            self.limits,
        )?;
        transaction
            .commit()
            .map_err(|error| ArchiveStoreError::WriteFailed(format!("initialize schema: {error}")))
    }

    fn existing_claim_outcome(
        &self,
        transaction: &WriteTransaction,
        records_definition: TableDefinition<'static, &'static str, &'static [u8]>,
        canonical: &str,
    ) -> Result<Option<ArchiveClaimOutcome>, ArchiveStoreError> {
        let records = transaction
            .open_table(records_definition)
            .map_err(|error| write_detail("open active records", error))?;
        if let Some(bytes) = records
            .get(canonical)
            .map_err(|error| write_detail("read committed record", error))?
        {
            let record =
                decode_record_for_key(canonical, bytes.value(), self.limits, "committed record")?;
            return Ok(Some(ArchiveClaimOutcome::AlreadyCommitted {
                record: Box::new(record),
            }));
        }
        drop(records);
        let imports = transaction
            .open_table(IMPORTS)
            .map_err(|error| write_detail("open imports", error))?;
        if let Some(bytes) = imports
            .get(canonical)
            .map_err(|error| write_detail("read imported marker", error))?
        {
            let marker = decode_import_for_key(canonical, bytes.value(), self.limits)?;
            return Ok(Some(ArchiveClaimOutcome::AlreadyImported {
                marker: Box::new(marker),
            }));
        }
        Ok(None)
    }

    fn read_reconciliation_actual(
        &self,
        transaction: &WriteTransaction,
        records_definition: TableDefinition<'static, &'static str, &'static [u8]>,
        canonical: &str,
    ) -> Result<(Option<ArchiveRecord>, Option<ArchiveLease>), ArchiveStoreError> {
        let actual_record = {
            let records = transaction
                .open_table(records_definition)
                .map_err(|error| write_detail("open active records", error))?;
            records
                .get(canonical)
                .map_err(|error| write_detail("read reconciliation row", error))?
                .map(|bytes| {
                    decode_record_for_key(canonical, bytes.value(), self.limits, "committed record")
                })
                .transpose()?
        };
        let actual_lease = {
            let leases = transaction
                .open_table(LEASES)
                .map_err(|error| write_detail("open leases", error))?;
            leases
                .get(canonical)
                .map_err(|error| write_detail("read reconciliation lease", error))?
                .map(|bytes| decode_lease_for_key(canonical, bytes.value(), self.limits))
                .transpose()?
        };
        {
            let imports = transaction
                .open_table(IMPORTS)
                .map_err(|error| write_detail("open imports", error))?;
            if let Some(bytes) = imports
                .get(canonical)
                .map_err(|error| write_detail("read reconciliation import", error))?
            {
                let _marker = decode_import_for_key(canonical, bytes.value(), self.limits)?;
                return Err(ArchiveStoreError::Conflict(
                    "reconciliation key already exists as a mapped import".to_owned(),
                ));
            }
        }
        Ok((actual_record, actual_lease))
    }

    /// Starts the sole write path and applies all durability hardening before
    /// any table mutation is possible.
    fn begin_durable_write(&self) -> Result<WriteTransaction, ArchiveStoreError> {
        let mut transaction = self.database.begin_write().map_err(|error| {
            ArchiveStoreError::WriteFailed(format!("begin transaction: {error}"))
        })?;
        transaction
            .set_durability(Durability::Immediate)
            .map_err(|error| {
                ArchiveStoreError::WriteFailed(format!("set immediate durability: {error}"))
            })?;
        transaction.set_two_phase_commit(true);
        transaction.set_quick_repair(true);
        Ok(transaction)
    }

    fn begin_archive_write(&self) -> Result<WriteTransaction, ArchiveStoreError> {
        let transaction = self.begin_durable_write()?;
        require_archive_write_allowed(&transaction, self.limits)?;
        Ok(transaction)
    }
}

fn write_error(operation: &'static str) -> impl FnOnce(redb::TableError) -> ArchiveStoreError {
    move |error| ArchiveStoreError::WriteFailed(format!("{operation}: {error}"))
}

fn write_detail(operation: &str, error: impl fmt::Display) -> ArchiveStoreError {
    ArchiveStoreError::WriteFailed(format!("{operation}: {error}"))
}

fn read_detail(operation: &str, error: impl fmt::Display) -> ArchiveStoreError {
    ArchiveStoreError::ReadFailed(format!("{operation}: {error}"))
}

fn corrupt_contract(
    record_kind: &'static str,
) -> impl FnOnce(ArchiveContractError) -> ArchiveStoreError {
    move |error| ArchiveStoreError::CorruptRecord(format!("{record_kind}: {error}"))
}

fn require_embedded_key(
    stored_key: &str,
    embedded_key: &ArchiveKey,
    limits: ArchiveLimits,
    record_kind: &str,
) -> Result<(), ArchiveStoreError> {
    let embedded = embedded_key
        .canonical_key(limits)
        .map_err(|error| ArchiveStoreError::CorruptRecord(format!("{record_kind}: {error}")))?;
    if embedded != stored_key {
        return Err(ArchiveStoreError::CorruptRecord(format!(
            "{record_kind} embedded key {embedded} differs from table key {stored_key}"
        )));
    }
    Ok(())
}

fn decode_record_for_key(
    stored_key: &str,
    bytes: &[u8],
    limits: ArchiveLimits,
    record_kind: &'static str,
) -> Result<ArchiveRecord, ArchiveStoreError> {
    let record =
        ArchiveRecord::from_wire_bytes(bytes, limits).map_err(corrupt_contract(record_kind))?;
    require_embedded_key(stored_key, &record.key, limits, record_kind)?;
    Ok(record)
}

fn decode_lease_for_key(
    stored_key: &str,
    bytes: &[u8],
    limits: ArchiveLimits,
) -> Result<ArchiveLease, ArchiveStoreError> {
    let lease = ArchiveLease::from_wire_bytes(bytes, limits).map_err(corrupt_contract("lease"))?;
    require_embedded_key(stored_key, &lease.key, limits, "lease")?;
    Ok(lease)
}

fn decode_import_for_key(
    stored_key: &str,
    bytes: &[u8],
    limits: ArchiveLimits,
) -> Result<ArchiveImportMarker, ArchiveStoreError> {
    let marker = ArchiveImportMarker::from_wire_bytes(bytes, limits)
        .map_err(corrupt_contract("import marker"))?;
    require_embedded_key(stored_key, &marker.key, limits, "import marker")?;
    Ok(marker)
}

fn decode_reconciliation_for_key(
    stored_key: &str,
    bytes: &[u8],
    limits: ArchiveLimits,
) -> Result<ArchiveReconciliationObservation, ArchiveStoreError> {
    let observation = ArchiveReconciliationObservation::from_wire_bytes(bytes, limits)
        .map_err(corrupt_contract("reconciliation receipt"))?;
    require_embedded_key(
        stored_key,
        &observation.key,
        limits,
        "reconciliation receipt",
    )?;
    Ok(observation)
}

fn decode_migration_state(
    bytes: &[u8],
    limits: ArchiveLimits,
) -> Result<ArchiveMigrationState, ArchiveStoreError> {
    ArchiveMigrationState::from_wire_bytes(bytes, limits)
        .map_err(corrupt_contract("migration state"))
}

fn require_archive_write_allowed(
    transaction: &WriteTransaction,
    limits: ArchiveLimits,
) -> Result<(), ArchiveStoreError> {
    let Some(state) = migration_state_write(transaction, limits)? else {
        return Ok(());
    };
    if matches!(
        state.phase,
        ArchiveMigrationPhase::Activated | ArchiveMigrationPhase::RolledBack
    ) {
        return Ok(());
    }
    Err(ArchiveStoreError::MigrationState(
        "ordinary archive writes are blocked until migration activation or rollback".to_owned(),
    ))
}

fn verify_source_target_exact(
    transaction: &WriteTransaction,
    limits: ArchiveLimits,
) -> Result<u64, ArchiveStoreError> {
    let source = transaction
        .open_table(RECORDS_V1)
        .map_err(|error| write_detail("open migration source", error))?;
    let target = transaction
        .open_table(RECORDS_V2)
        .map_err(|error| write_detail("open migration target", error))?;
    let mut source_rows = source
        .iter()
        .map_err(|error| write_detail("iterate migration source", error))?;
    let mut target_rows = target
        .iter()
        .map_err(|error| write_detail("iterate migration target", error))?;
    let mut count = 0_u64;
    loop {
        let source_row = source_rows
            .next()
            .transpose()
            .map_err(|error| write_detail("read migration source during verification", error))?;
        let target_row = target_rows
            .next()
            .transpose()
            .map_err(|error| write_detail("read migration target during verification", error))?;
        match (source_row, target_row) {
            (None, None) => break,
            (Some((source_key, source_value)), Some((target_key, target_value)))
                if source_key.value() == target_key.value()
                    && source_value.value() == target_value.value() =>
            {
                let canonical = source_key.value();
                let _source_record = decode_record_for_key(
                    canonical,
                    source_value.value(),
                    limits,
                    "migration source record",
                )?;
                let _target_record = decode_record_for_key(
                    canonical,
                    target_value.value(),
                    limits,
                    "migration target record",
                )?;
                count = count.checked_add(1).ok_or_else(|| {
                    ArchiveStoreError::LimitExceeded(
                        "migration verification count exhausted".to_owned(),
                    )
                })?;
            }
            _ => {
                return Err(ArchiveStoreError::Conflict(
                    "migration target differs from last-known-good source".to_owned(),
                ));
            }
        }
    }
    Ok(count)
}

fn clear_reconciliation_receipt(
    transaction: &WriteTransaction,
    canonical: &str,
) -> Result<(), ArchiveStoreError> {
    let mut receipts = transaction
        .open_table(RECONCILIATION_RECEIPTS)
        .map_err(|error| write_detail("open reconciliation receipts", error))?;
    receipts
        .remove(canonical)
        .map_err(|error| write_detail("clear reconciliation receipt", error))?;
    Ok(())
}

fn reconciliation_receipt_write(
    transaction: &WriteTransaction,
    canonical: &str,
    limits: ArchiveLimits,
) -> Result<Option<ArchiveReconciliationObservation>, ArchiveStoreError> {
    let receipts = transaction
        .open_table(RECONCILIATION_RECEIPTS)
        .map_err(|error| write_detail("open reconciliation receipts", error))?;
    receipts
        .get(canonical)
        .map_err(|error| write_detail("read reconciliation receipt", error))?
        .map(|bytes| decode_reconciliation_for_key(canonical, bytes.value(), limits))
        .transpose()
}

fn persist_reconciliation_receipt(
    transaction: &WriteTransaction,
    canonical: &str,
    wire: &[u8],
) -> Result<(), ArchiveStoreError> {
    let mut receipts = transaction
        .open_table(RECONCILIATION_RECEIPTS)
        .map_err(|error| write_detail("open reconciliation receipts", error))?;
    receipts
        .insert(canonical, wire)
        .map_err(|error| write_detail("persist reconciliation receipt", error))?;
    Ok(())
}

fn validate_reconciliation_actual(
    observation: &ArchiveReconciliationObservation,
    actual_record: Option<&ArchiveRecord>,
    actual_lease: Option<&ArchiveLease>,
) -> Result<(), ArchiveStoreError> {
    match &observation.row {
        ArchiveRowObservation::Missing if actual_record.is_some() => {
            return Err(ArchiveStoreError::Conflict(
                "reconciliation claimed a missing row that exists".to_owned(),
            ));
        }
        ArchiveRowObservation::Matching { record } if actual_record != Some(record.as_ref()) => {
            return Err(ArchiveStoreError::Conflict(
                "reconciliation row observation is stale".to_owned(),
            ));
        }
        _ => {}
    }
    if observation.lease.as_ref() != actual_lease {
        return Err(ArchiveStoreError::Conflict(
            "reconciliation lease observation is stale".to_owned(),
        ));
    }
    Ok(())
}

fn replay_applied_reconciliation(
    observation: &ArchiveReconciliationObservation,
    actual_record: Option<&ArchiveRecord>,
    actual_lease: Option<&ArchiveLease>,
    limits: ArchiveLimits,
) -> Result<ArchiveReconciliationDecision, ArchiveStoreError> {
    match observation.decide(limits)? {
        ArchiveReconciliationDecision::InsertMissingRow { record }
            if actual_record == Some(&record) && actual_lease.is_none() =>
        {
            Ok(ArchiveReconciliationDecision::Reconciled)
        }
        ArchiveReconciliationDecision::ReclaimStaleLease
            if actual_record.is_none() && actual_lease.is_none() =>
        {
            Ok(ArchiveReconciliationDecision::ReclaimStaleLease)
        }
        _ => Err(ArchiveStoreError::Conflict(
            "durable reconciliation receipt does not match current terminal state".to_owned(),
        )),
    }
}

fn lease_generation_key(canonical_key: &str) -> String {
    format!("lease-generation|{canonical_key}")
}

fn records_definition(
    version: u64,
) -> Result<TableDefinition<'static, &'static str, &'static [u8]>, ArchiveStoreError> {
    match version {
        INITIAL_STORE_VERSION => Ok(RECORDS_V1),
        CURRENT_STORE_VERSION => Ok(RECORDS_V2),
        other => Err(ArchiveStoreError::UnsupportedStoreVersion(other)),
    }
}

fn active_records_definition(
    transaction: &WriteTransaction,
) -> Result<TableDefinition<'static, &'static str, &'static [u8]>, ArchiveStoreError> {
    let metadata = transaction
        .open_table(META_U64)
        .map_err(|error| write_detail("open metadata", error))?;
    let version = metadata
        .get(STORE_VERSION)
        .map_err(|error| write_detail("read store version", error))?
        .ok_or_else(|| ArchiveStoreError::CorruptRecord("store version missing".to_owned()))?
        .value();
    records_definition(version)
}

fn active_store_version(transaction: &WriteTransaction) -> Result<u64, ArchiveStoreError> {
    let metadata = transaction
        .open_table(META_U64)
        .map_err(|error| write_detail("open metadata", error))?;
    metadata
        .get(STORE_VERSION)
        .map_err(|error| write_detail("read store version", error))?
        .ok_or_else(|| ArchiveStoreError::CorruptRecord("store version missing".to_owned()))
        .map(|version| version.value())
}

fn initialized_tables_have_state(
    transaction: &WriteTransaction,
) -> Result<bool, ArchiveStoreError> {
    let metadata = transaction
        .open_table(META_U64)
        .map_err(|error| write_detail("open metadata", error))?;
    if metadata
        .len()
        .map_err(|error| write_detail("measure metadata", error))?
        != 0
    {
        return Ok(true);
    }
    drop(metadata);
    for definition in [
        LEASES,
        RECORDS_V1,
        RECORDS_V2,
        IMPORTS,
        INCONSISTENCIES,
        MIGRATION_RECORDS,
        RECONCILIATION_RECEIPTS,
    ] {
        let table = transaction
            .open_table(definition)
            .map_err(|error| write_detail("open initialized table", error))?;
        if table
            .len()
            .map_err(|error| write_detail("measure initialized table", error))?
            != 0
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn validate_store_topology(
    transaction: &WriteTransaction,
    version: u64,
    limits: ArchiveLimits,
) -> Result<(), ArchiveStoreError> {
    validate_migration_table_integrity(transaction, limits)?;
    let migration = migration_state_write(transaction, limits)?;
    match (version, migration.as_ref().map(|state| state.phase)) {
        (INITIAL_STORE_VERSION, None) => {
            let target = transaction
                .open_table(RECORDS_V2)
                .map_err(|error| write_detail("open inactive v2 records", error))?;
            if target
                .len()
                .map_err(|error| write_detail("measure inactive v2 records", error))?
                != 0
            {
                return Err(ArchiveStoreError::CorruptRecord(
                    "v1 metadata has v2 rows without a migration checkpoint".to_owned(),
                ));
            }
        }
        (INITIAL_STORE_VERSION, Some(ArchiveMigrationPhase::Activated)) => {
            return Err(ArchiveStoreError::CorruptRecord(
                "v1 metadata conflicts with an activated migration checkpoint".to_owned(),
            ));
        }
        (INITIAL_STORE_VERSION, Some(ArchiveMigrationPhase::Prepared)) => {
            let state = migration.expect("matched durable migration state");
            let target = transaction
                .open_table(RECORDS_V2)
                .map_err(|error| write_detail("open prepared migration target", error))?;
            let target_len = target
                .len()
                .map_err(|error| write_detail("measure prepared migration target", error))?;
            if state.migrated_records != 0 || target_len != 0 {
                return Err(ArchiveStoreError::CorruptRecord(
                    "prepared migration has copied target state".to_owned(),
                ));
            }
        }
        (INITIAL_STORE_VERSION, Some(ArchiveMigrationPhase::Copying)) => {
            let state = migration.expect("matched durable migration state");
            let copied = validate_target_subset_of_source(transaction, limits)?;
            if copied != state.migrated_records {
                return Err(ArchiveStoreError::CorruptRecord(format!(
                    "copying checkpoint count {} differs from target rows {copied}",
                    state.migrated_records
                )));
            }
            validate_migration_cursor(transaction, &state, limits)?;
        }
        (
            INITIAL_STORE_VERSION,
            Some(ArchiveMigrationPhase::Verifying | ArchiveMigrationPhase::ReadyToActivate),
        ) => {
            let state = migration.expect("matched durable migration state");
            let verified = verify_source_target_exact(transaction, limits).map_err(|error| {
                ArchiveStoreError::CorruptRecord(format!(
                    "verified migration tables diverge: {error}"
                ))
            })?;
            if verified != state.migrated_records {
                return Err(ArchiveStoreError::CorruptRecord(format!(
                    "verified checkpoint count {} differs from table rows {verified}",
                    state.migrated_records
                )));
            }
            validate_migration_cursor(transaction, &state, limits)?;
        }
        (INITIAL_STORE_VERSION, Some(ArchiveMigrationPhase::RolledBack)) => {}
        (CURRENT_STORE_VERSION, Some(ArchiveMigrationPhase::Activated)) => {
            let state = migration.expect("matched durable migration state");
            let copied = validate_source_subset_of_target(transaction, limits)?;
            if copied != state.migrated_records {
                return Err(ArchiveStoreError::CorruptRecord(format!(
                    "activated checkpoint count {} differs from preserved source rows {copied}",
                    state.migrated_records
                )));
            }
        }
        (CURRENT_STORE_VERSION, None) => {
            return Err(ArchiveStoreError::CorruptRecord(
                "v2 metadata is missing its activated migration checkpoint".to_owned(),
            ));
        }
        (CURRENT_STORE_VERSION, Some(phase)) => {
            return Err(ArchiveStoreError::CorruptRecord(format!(
                "v2 metadata conflicts with migration phase {phase:?}"
            )));
        }
        (other, _) => return Err(ArchiveStoreError::UnsupportedStoreVersion(other)),
    }
    Ok(())
}

fn validate_migration_table_integrity(
    transaction: &WriteTransaction,
    limits: ArchiveLimits,
) -> Result<(), ArchiveStoreError> {
    let migrations = transaction
        .open_table(MIGRATION_RECORDS)
        .map_err(|error| write_detail("open migrations for integrity validation", error))?;
    for row in migrations
        .iter()
        .map_err(|error| write_detail("iterate migrations for integrity validation", error))?
    {
        let (key, value) =
            row.map_err(|error| write_detail("read migration for integrity validation", error))?;
        if key.value() != ACTIVE_MIGRATION {
            return Err(ArchiveStoreError::CorruptRecord(format!(
                "migration checkpoint stored under foreign key {}",
                key.value()
            )));
        }
        let _state = decode_migration_state(value.value(), limits)?;
    }
    Ok(())
}

fn validate_target_subset_of_source(
    transaction: &WriteTransaction,
    limits: ArchiveLimits,
) -> Result<u64, ArchiveStoreError> {
    validate_table_subset(transaction, RECORDS_V2, RECORDS_V1, limits)
}

fn validate_source_subset_of_target(
    transaction: &WriteTransaction,
    limits: ArchiveLimits,
) -> Result<u64, ArchiveStoreError> {
    validate_table_subset(transaction, RECORDS_V1, RECORDS_V2, limits)
}

fn validate_migration_cursor(
    transaction: &WriteTransaction,
    state: &ArchiveMigrationState,
    limits: ArchiveLimits,
) -> Result<(), ArchiveStoreError> {
    let target = transaction
        .open_table(RECORDS_V2)
        .map_err(|error| write_detail("open migration target for cursor validation", error))?;
    let mut last = None;
    for row in target
        .iter()
        .map_err(|error| write_detail("iterate migration target for cursor validation", error))?
    {
        let (key, _value) =
            row.map_err(|error| write_detail("read migration cursor row", error))?;
        last = Some(key.value().to_owned());
    }
    let checkpoint = state
        .last_migrated_key
        .as_ref()
        .map(|key| key.canonical_key(limits))
        .transpose()
        .map_err(|error| {
            ArchiveStoreError::CorruptRecord(format!("migration cursor key: {error}"))
        })?;
    if checkpoint != last {
        return Err(ArchiveStoreError::CorruptRecord(format!(
            "migration cursor {checkpoint:?} differs from target tail {last:?}"
        )));
    }
    Ok(())
}

fn validate_table_subset(
    transaction: &WriteTransaction,
    subset_definition: TableDefinition<'static, &'static str, &'static [u8]>,
    superset_definition: TableDefinition<'static, &'static str, &'static [u8]>,
    limits: ArchiveLimits,
) -> Result<u64, ArchiveStoreError> {
    let subset = transaction
        .open_table(subset_definition)
        .map_err(|error| write_detail("open migration subset", error))?;
    let superset = transaction
        .open_table(superset_definition)
        .map_err(|error| write_detail("open migration superset", error))?;
    let mut count = 0_u64;
    for row in subset
        .iter()
        .map_err(|error| write_detail("iterate migration subset", error))?
    {
        let (key, value) = row.map_err(|error| write_detail("read migration subset", error))?;
        let canonical = key.value();
        let _record =
            decode_record_for_key(canonical, value.value(), limits, "migration subset record")?;
        let matching = superset
            .get(canonical)
            .map_err(|error| write_detail("read migration superset", error))?
            .is_some_and(|other| other.value() == value.value());
        if !matching {
            return Err(ArchiveStoreError::CorruptRecord(format!(
                "migration generation differs at canonical key {canonical}"
            )));
        }
        count = count.checked_add(1).ok_or_else(|| {
            ArchiveStoreError::LimitExceeded("migration subset count exhausted".to_owned())
        })?;
    }
    Ok(count)
}

fn migration_state_write(
    transaction: &WriteTransaction,
    limits: ArchiveLimits,
) -> Result<Option<ArchiveMigrationState>, ArchiveStoreError> {
    let migrations = transaction
        .open_table(MIGRATION_RECORDS)
        .map_err(|error| write_detail("open migration state", error))?;
    migrations
        .get(ACTIVE_MIGRATION)
        .map_err(|error| write_detail("read migration state", error))?
        .map(|bytes| decode_migration_state(bytes.value(), limits))
        .transpose()
}

fn persist_migration_state(
    transaction: &WriteTransaction,
    state: &ArchiveMigrationState,
    limits: ArchiveLimits,
) -> Result<(), ArchiveStoreError> {
    let wire = state.to_wire_bytes(limits)?;
    let mut migrations = transaction
        .open_table(MIGRATION_RECORDS)
        .map_err(|error| write_detail("open migration state", error))?;
    migrations
        .insert(ACTIVE_MIGRATION, wire.as_slice())
        .map_err(|error| write_detail("persist migration state", error))?;
    Ok(())
}

fn require_migration_id(
    state: &ArchiveMigrationState,
    migration_id: &str,
) -> Result<(), ArchiveStoreError> {
    if state.plan.migration_id == migration_id {
        Ok(())
    } else {
        Err(ArchiveStoreError::MigrationState(
            "migration identifier does not match active checkpoint".to_owned(),
        ))
    }
}

fn active_records_definition_read(
    transaction: &ReadTransaction,
) -> Result<TableDefinition<'static, &'static str, &'static [u8]>, ArchiveStoreError> {
    let metadata = transaction
        .open_table(META_U64)
        .map_err(|error| read_detail("open metadata", error))?;
    let version = metadata
        .get(STORE_VERSION)
        .map_err(|error| read_detail("read store version", error))?
        .ok_or_else(|| ArchiveStoreError::CorruptRecord("store version missing".to_owned()))?
        .value();
    records_definition(version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fforager_contracts::{
        ArchiveConfinementProof, ArchiveFilesystemCommitment, ArchiveIdentity, ArchiveImportEntry,
        ArchiveImportFormat, ArchiveImportMapping, ArchiveNamespace, ArchiveOutputObservation,
        ArchivePlacementProof, ArchiveProvenance, ArchiveReconciliationFailure,
        ArchiveRowObservation, ArchiveSuccessEvent, ArchiveSynchronizationProof, AssetId,
        DerivedOutputId, FilesystemProfileContract, JobId, LeaseToken, ReconciledArchiveOutput,
        TransactionId,
    };
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Barrier};

    static TEST_ID: AtomicU64 = AtomicU64::new(1);

    fn test_path(label: &str) -> PathBuf {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap();
        let directory = repository
            .join(".fforager-artifacts")
            .join("test-runs")
            .join("fforager-storage");
        fs::create_dir_all(&directory).unwrap();
        directory.join(format!(
            "{label}-{}-{}.redb",
            std::process::id(),
            TEST_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn raw_write(path: &Path, mutate: impl FnOnce(&WriteTransaction)) {
        let database = Database::create(path).unwrap();
        let mut transaction = database.begin_write().unwrap();
        transaction.set_durability(Durability::Immediate).unwrap();
        transaction.set_two_phase_commit(true);
        transaction.set_quick_repair(true);
        mutate(&transaction);
        transaction.commit().unwrap();
    }

    fn digest(character: char) -> String {
        std::iter::repeat_n(character, 64).collect()
    }

    fn key(suffix: &str) -> ArchiveKey {
        ArchiveKey {
            schema: ARCHIVE_SCHEMA,
            namespace: ArchiveNamespace::SourceAsset,
            identity: ArchiveIdentity::Asset(AssetId::new(format!("asset_{suffix}")).unwrap()),
            identity_rule_version: 1,
            extractor_id: "generic-http".to_owned(),
        }
    }

    fn provenance(suffix: &str) -> ArchiveProvenance {
        ArchiveProvenance {
            job_id: JobId::new(format!("job_{suffix}")).unwrap(),
            transaction_id: TransactionId::new(format!("transaction_{suffix}")).unwrap(),
            source_locator_digest: digest('a'),
            request_provenance_digest: digest('b'),
        }
    }

    fn claim_request(suffix: &str, requested_at: u64) -> ArchiveClaimRequest {
        claim_request_for("shared", suffix, requested_at)
    }

    fn claim_request_for(
        asset_suffix: &str,
        owner_suffix: &str,
        requested_at: u64,
    ) -> ArchiveClaimRequest {
        let provenance = provenance(owner_suffix);
        ArchiveClaimRequest {
            key: key(asset_suffix),
            owner_job_id: provenance.job_id.clone(),
            lease_token: LeaseToken::new(format!("lease_{owner_suffix}")).unwrap(),
            requested_at_unix_millis: requested_at,
            lease_duration_millis: 1_000,
            provenance,
        }
    }

    fn acquired(outcome: ArchiveClaimOutcome) -> ArchiveLease {
        match outcome {
            ArchiveClaimOutcome::Acquired { lease } => lease,
            other => panic!("expected acquired lease, received {other:?}"),
        }
    }

    fn output_for(asset_suffix: &str) -> ReconciledArchiveOutput {
        ReconciledArchiveOutput {
            final_output_identity: format!("output/{asset_suffix}.mp4"),
            artifact_size_bytes: 42,
            artifact_digest: digest('c'),
            reconciliation_receipt_digest: digest('d'),
            filesystem_commitment: ArchiveFilesystemCommitment {
                profile: FilesystemProfileContract::windows_11_26200_ntfs_v1(),
                placement: ArchivePlacementProof::SameFilesystem,
                synchronization: ArchiveSynchronizationProof::DataAndParentDirectory,
                confinement: ArchiveConfinementProof::RootHandleVerified,
            },
            asset_ids: vec![AssetId::new(format!("asset_{asset_suffix}")).unwrap()],
            derived_output_ids: vec![
                DerivedOutputId::new(format!("output_{asset_suffix}")).unwrap(),
            ],
        }
    }

    fn commit_request(
        lease: ArchiveLease,
        suffix: &str,
        committed_at: u64,
    ) -> ArchiveCommitRequest {
        let asset_id = match &lease.key.identity {
            ArchiveIdentity::Asset(asset_id) => asset_id.clone(),
            other => panic!("storage test expected asset identity, received {other:?}"),
        };
        let asset_suffix = asset_id
            .as_str()
            .strip_prefix("asset_")
            .expect("test asset identifier has prefix")
            .to_owned();
        ArchiveCommitRequest {
            lease,
            success_event: ArchiveSuccessEvent::PerAsset { asset_id },
            output: output_for(&asset_suffix),
            provenance: provenance(suffix),
            committed_at_unix_millis: committed_at,
        }
    }

    #[test]
    fn durability_claim_renew_commit_and_idempotency() {
        let store = ArchiveStore::open(test_path("lifecycle"), ArchiveLimits::default()).unwrap();
        assert_eq!(store.durability_policy(), ARCHIVE_DURABILITY_POLICY);
        assert!(store.durability_policy().immediate);
        assert!(store.durability_policy().two_phase_commit);
        assert!(store.durability_policy().quick_repair);

        let lease = acquired(store.claim(&claim_request("first", 1_000)).unwrap());
        assert!(matches!(
            store.membership(&lease.key).unwrap(),
            ArchiveMembership::Claimed { .. }
        ));
        let renewed = store
            .renew_lease(&ArchiveLeaseRenewalRequest {
                current_lease: lease,
                new_token: LeaseToken::new("lease_first-renewed").unwrap(),
                renewed_at_unix_millis: 1_500,
                lease_duration_millis: 1_000,
            })
            .unwrap();
        assert_eq!(renewed.generation, 2);
        let request = commit_request(renewed, "first", 2_000);
        let inserted = store.commit_success(&request).unwrap();
        assert!(matches!(inserted, ArchiveCommitOutcome::Inserted { .. }));
        assert!(matches!(
            store.commit_success(&request).unwrap(),
            ArchiveCommitOutcome::AlreadyCommitted { .. }
        ));
        assert!(store.contains(&request.lease.key).unwrap());
        assert!(matches!(
            store.claim(&claim_request("other", 3_000)).unwrap(),
            ArchiveClaimOutcome::AlreadyCommitted { .. }
        ));
    }

    #[test]
    fn claim_replay_binds_full_provenance_before_and_after_reopen() {
        let path = test_path("claim-provenance");
        let store = ArchiveStore::open(&path, ArchiveLimits::default()).unwrap();
        let original = claim_request("provenance", 1_000);
        let lease = acquired(store.claim(&original).unwrap());
        assert_eq!(lease.claim_provenance, original.provenance);
        assert_eq!(
            acquired(store.claim(&original).unwrap()).claim_provenance,
            original.provenance
        );

        let mut changed_transaction = original.clone();
        changed_transaction.provenance.transaction_id =
            TransactionId::new("transaction_changed").unwrap();
        assert!(matches!(
            store.claim(&changed_transaction),
            Err(ArchiveStoreError::Conflict(_))
        ));
        let mut changed_digest = original.clone();
        changed_digest.provenance.request_provenance_digest = digest('f');
        assert!(matches!(
            store.claim(&changed_digest),
            Err(ArchiveStoreError::Conflict(_))
        ));

        drop(store);
        let reopened = ArchiveStore::open(&path, ArchiveLimits::default()).unwrap();
        assert!(matches!(
            reopened.claim(&changed_transaction),
            Err(ArchiveStoreError::Conflict(_))
        ));
        assert_eq!(
            acquired(reopened.claim(&original).unwrap()).claim_provenance,
            original.provenance
        );
    }

    #[test]
    fn concurrent_duplicate_claim_has_one_winner() {
        let store = Arc::new(
            ArchiveStore::open(test_path("concurrent"), ArchiveLimits::default()).unwrap(),
        );
        let barrier = Arc::new(Barrier::new(8));
        let mut workers = Vec::new();
        for index in 0..8 {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                let request = claim_request(&format!("worker-{index}"), 10_000);
                barrier.wait();
                store.claim(&request).unwrap()
            }));
        }
        let outcomes: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, ArchiveClaimOutcome::Acquired { .. }))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, ArchiveClaimOutcome::HeldByOther { .. }))
                .count(),
            7
        );
    }

    #[test]
    fn stale_takeover_rejects_old_lease_commit() {
        let store = ArchiveStore::open(test_path("takeover"), ArchiveLimits::default()).unwrap();
        let stale = acquired(store.claim(&claim_request("stale", 1_000)).unwrap());
        assert!(matches!(
            store.commit_success(&commit_request(stale.clone(), "stale", 2_000)),
            Err(ArchiveStoreError::Contract(
                ArchiveContractError::LeaseExpired
            ))
        ));
        let renewed = store
            .renew_lease(&ArchiveLeaseRenewalRequest {
                current_lease: stale.clone(),
                new_token: LeaseToken::new("lease_stale-renewed").unwrap(),
                renewed_at_unix_millis: 1_500,
                lease_duration_millis: 1_000,
            })
            .unwrap();
        let replacement = acquired(store.claim(&claim_request("replacement", 2_500)).unwrap());
        assert_eq!(replacement.generation, renewed.generation + 1);
        assert!(matches!(
            store.commit_success(&commit_request(stale, "stale", 1_500)),
            Err(ArchiveStoreError::LeaseMismatch)
        ));
    }

    #[test]
    fn migration_activation_preserves_later_writes_and_rejects_rollback() {
        let path = test_path("migration");
        let store = ArchiveStore::open(&path, ArchiveLimits::default()).unwrap();
        let lease = acquired(store.claim(&claim_request("migration", 1_000)).unwrap());
        store
            .commit_success(&commit_request(lease, "migration", 1_500))
            .unwrap();
        let plan = ArchiveMigrationPlan {
            migration_id: "archive-store-v1-to-v2".to_owned(),
            from_store_version: 1,
            to_store_version: 2,
            maximum_records_per_batch: 1,
        };
        store.begin_migration(&plan, digest('e')).unwrap();
        assert_eq!(
            store.resume_migration(&plan.migration_id).unwrap().phase,
            ArchiveMigrationPhase::Copying
        );
        assert_eq!(
            store.resume_migration(&plan.migration_id).unwrap().phase,
            ArchiveMigrationPhase::Verifying
        );
        assert_eq!(
            store.verify_migration(&plan.migration_id).unwrap().phase,
            ArchiveMigrationPhase::ReadyToActivate
        );
        assert_eq!(
            store.activate_migration(&plan.migration_id).unwrap().phase,
            ArchiveMigrationPhase::Activated
        );
        assert!(store.contains(&key("shared")).unwrap());
        let later_lease = acquired(
            store
                .claim(&claim_request_for("later", "later", 3_000))
                .unwrap(),
        );
        store
            .commit_success(&commit_request(later_lease, "later", 3_500))
            .unwrap();
        assert!(matches!(
            store.rollback_migration(&plan.migration_id),
            Err(ArchiveStoreError::MigrationState(_))
        ));
        drop(store);
        let reopened = ArchiveStore::open(&path, ArchiveLimits::default()).unwrap();
        assert!(reopened.contains(&key("shared")).unwrap());
        assert!(reopened.contains(&key("later")).unwrap());
    }

    #[test]
    fn migration_blocks_commit_between_verify_and_activate() {
        let path = test_path("migration-write-block");
        let store = ArchiveStore::open(&path, ArchiveLimits::default()).unwrap();
        let pending = acquired(
            store
                .claim(&claim_request_for("pending", "pending", 1_000))
                .unwrap(),
        );
        let plan = ArchiveMigrationPlan {
            migration_id: "archive-store-write-block".to_owned(),
            from_store_version: 1,
            to_store_version: 2,
            maximum_records_per_batch: 8,
        };
        store.begin_migration(&plan, digest('e')).unwrap();
        assert_eq!(
            store.resume_migration(&plan.migration_id).unwrap().phase,
            ArchiveMigrationPhase::Verifying
        );
        store.verify_migration(&plan.migration_id).unwrap();
        assert!(matches!(
            store.commit_success(&commit_request(pending.clone(), "pending", 1_500)),
            Err(ArchiveStoreError::MigrationState(_))
        ));
        store.activate_migration(&plan.migration_id).unwrap();
        store
            .commit_success(&commit_request(pending, "pending", 1_500))
            .unwrap();
        drop(store);
        let reopened = ArchiveStore::open(&path, ArchiveLimits::default()).unwrap();
        assert!(reopened.contains(&key("pending")).unwrap());
    }

    #[test]
    fn activation_atomically_reverifies_source_and_target() {
        let path = test_path("migration-activation-reverify");
        let store = ArchiveStore::open(&path, ArchiveLimits::default()).unwrap();
        let seed = acquired(store.claim(&claim_request("seed", 1_000)).unwrap());
        store
            .commit_success(&commit_request(seed, "seed", 1_500))
            .unwrap();
        let plan = ArchiveMigrationPlan {
            migration_id: "archive-store-reverify".to_owned(),
            from_store_version: 1,
            to_store_version: 2,
            maximum_records_per_batch: 8,
        };
        store.begin_migration(&plan, digest('e')).unwrap();
        assert_eq!(
            store.resume_migration(&plan.migration_id).unwrap().phase,
            ArchiveMigrationPhase::Verifying
        );
        store.verify_migration(&plan.migration_id).unwrap();

        let late_lease = ArchiveLease {
            key: key("late-source"),
            owner_job_id: provenance("late-source").job_id,
            claim_provenance: provenance("late-source"),
            token: LeaseToken::new("lease_late-source").unwrap(),
            generation: 1,
            acquired_at_unix_millis: 2_000,
            expires_at_unix_millis: 3_000,
        };
        let late_request = commit_request(late_lease, "late-source", 2_500);
        let late_record = ArchiveRecord {
            schema: ARCHIVE_SCHEMA,
            archive_row_id: "archive-row-late-source".to_owned(),
            key: late_request.lease.key.clone(),
            success_event: late_request.success_event.clone(),
            output: late_request.output.clone(),
            provenance: late_request.provenance.clone(),
            claim_lease_token: late_request.lease.token.clone(),
            claim_lease_generation: late_request.lease.generation,
            commit_sequence: 2,
            committed_at_unix_millis: late_request.committed_at_unix_millis,
        };
        let canonical = late_record
            .key
            .canonical_key(ArchiveLimits::default())
            .unwrap();
        let wire = late_record.to_wire_bytes(ArchiveLimits::default()).unwrap();
        let transaction = store.begin_durable_write().unwrap();
        {
            let mut source = transaction.open_table(RECORDS_V1).unwrap();
            source.insert(canonical.as_str(), wire.as_slice()).unwrap();
        }
        transaction.commit().unwrap();

        assert!(matches!(
            store.activate_migration(&plan.migration_id),
            Err(ArchiveStoreError::Conflict(_))
        ));
        drop(store);
        assert!(matches!(
            ArchiveStore::open(&path, ArchiveLimits::default()),
            Err(ArchiveStoreError::CorruptRecord(_))
        ));
    }

    #[test]
    fn mapped_import_is_atomic_idempotent_membership() {
        let store = ArchiveStore::open(test_path("import"), ArchiveLimits::default()).unwrap();
        let imported_key = key("imported");
        let batch = ArchiveImportBatch {
            schema: ARCHIVE_SCHEMA,
            format: ArchiveImportFormat::FerricMappedTextV1,
            source_digest: digest('f'),
            entries: vec![ArchiveImportEntry {
                source_line_number: 1,
                source_identity: "generic-http imported".to_owned(),
                mapping: ArchiveImportMapping::AssetIdentity,
                target_key: imported_key.clone(),
            }],
        };
        assert_eq!(
            store.import_mapped_text(&batch, 5_000).unwrap(),
            ArchiveImportResult {
                inserted: 1,
                already_present: 0
            }
        );
        assert_eq!(
            store.import_mapped_text(&batch, 6_000).unwrap(),
            ArchiveImportResult {
                inserted: 0,
                already_present: 1
            }
        );
        assert!(matches!(
            store.membership(&imported_key).unwrap(),
            ArchiveMembership::Imported { .. }
        ));
    }

    #[test]
    fn reconciliation_inserts_missing_row_and_reclaims_stale_lease() {
        let path = test_path("reconcile");
        let store = ArchiveStore::open(&path, ArchiveLimits::default()).unwrap();
        let recovery_record = ArchiveRecord {
            schema: ARCHIVE_SCHEMA,
            archive_row_id: "archive-row-recovery".to_owned(),
            key: key("recovered"),
            success_event: ArchiveSuccessEvent::PerAsset {
                asset_id: AssetId::new("asset_recovered").unwrap(),
            },
            output: output_for("recovered"),
            provenance: provenance("recovery"),
            claim_lease_token: LeaseToken::new("lease_recovery").unwrap(),
            claim_lease_generation: 1,
            commit_sequence: 7,
            committed_at_unix_millis: 7_000,
        };
        let recovery_observation = ArchiveReconciliationObservation {
            key: recovery_record.key.clone(),
            now_unix_millis: 8_000,
            output: ArchiveOutputObservation::Matching {
                output: Box::new(recovery_record.output.clone()),
            },
            row: ArchiveRowObservation::Missing,
            lease: None,
            staged_output: None,
            recovery_record: Some(recovery_record.clone()),
        };
        let decision = store.reconcile(&recovery_observation).unwrap();
        assert!(matches!(
            decision,
            ArchiveReconciliationDecision::InsertMissingRow { .. }
        ));
        assert!(store.contains(&recovery_record.key).unwrap());
        assert_eq!(
            store.reconcile(&recovery_observation).unwrap(),
            ArchiveReconciliationDecision::Reconciled
        );
        drop(store);
        let store = ArchiveStore::open(&path, ArchiveLimits::default()).unwrap();
        assert_eq!(
            store.reconcile(&recovery_observation).unwrap(),
            ArchiveReconciliationDecision::Reconciled
        );
        let mut divergent_recovery = recovery_observation.clone();
        divergent_recovery.now_unix_millis = 8_001;
        assert!(matches!(
            store.reconcile(&divergent_recovery),
            Err(ArchiveStoreError::Conflict(_))
        ));

        let stale = acquired(store.claim(&claim_request("reclaim", 10_000)).unwrap());
        let stale_observation = ArchiveReconciliationObservation {
            key: stale.key.clone(),
            now_unix_millis: stale.expires_at_unix_millis,
            output: ArchiveOutputObservation::Missing,
            row: ArchiveRowObservation::Missing,
            lease: Some(stale.clone()),
            staged_output: None,
            recovery_record: None,
        };
        let decision = store.reconcile(&stale_observation).unwrap();
        assert_eq!(decision, ArchiveReconciliationDecision::ReclaimStaleLease);
        assert_eq!(
            store.reconcile(&stale_observation).unwrap(),
            ArchiveReconciliationDecision::ReclaimStaleLease
        );
        assert_eq!(
            store.membership(&stale.key).unwrap(),
            ArchiveMembership::Absent
        );
        drop(store);
        let reopened = ArchiveStore::open(&path, ArchiveLimits::default()).unwrap();
        assert_eq!(
            reopened.reconcile(&stale_observation).unwrap(),
            ArchiveReconciliationDecision::ReclaimStaleLease
        );
        let mut divergent_stale = stale_observation;
        divergent_stale.now_unix_millis += 1;
        assert!(matches!(
            reopened.reconcile(&divergent_stale),
            Err(ArchiveStoreError::Conflict(_))
        ));
    }

    #[test]
    fn row_without_recoverable_output_fails_closed() {
        let store = ArchiveStore::open(test_path("fail-closed"), ArchiveLimits::default()).unwrap();
        let lease = acquired(store.claim(&claim_request("closed", 1_000)).unwrap());
        let record = match store
            .commit_success(&commit_request(lease, "closed", 1_500))
            .unwrap()
        {
            ArchiveCommitOutcome::Inserted { record } => record,
            ArchiveCommitOutcome::AlreadyCommitted { record } => {
                panic!("expected inserted record, received {record:?}")
            }
        };
        let decision = store
            .reconcile(&ArchiveReconciliationObservation {
                key: record.key.clone(),
                now_unix_millis: 2_000,
                output: ArchiveOutputObservation::Missing,
                row: ArchiveRowObservation::Matching {
                    record: Box::new(record),
                },
                lease: None,
                staged_output: None,
                recovery_record: None,
            })
            .unwrap();
        assert_eq!(
            decision,
            ArchiveReconciliationDecision::FailClosed {
                reason: ArchiveReconciliationFailure::RowWithoutRecoverableOutput
            }
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one corruption regression covers every durable key-bound table with the same attack"
    )]
    fn foreign_values_under_table_keys_fail_closed_on_read() {
        let limits = ArchiveLimits::default();

        let record_path = test_path("foreign-record-key");
        let store = ArchiveStore::open(&record_path, limits).unwrap();
        let lease = acquired(store.claim(&claim_request("record", 1_000)).unwrap());
        let record = match store
            .commit_success(&commit_request(lease, "record", 1_500))
            .unwrap()
        {
            ArchiveCommitOutcome::Inserted { record } => record,
            other @ ArchiveCommitOutcome::AlreadyCommitted { .. } => {
                panic!("expected inserted record, received {other:?}")
            }
        };
        let record_wire = record.to_wire_bytes(limits).unwrap();
        drop(store);
        let foreign = key("foreign").canonical_key(limits).unwrap();
        raw_write(&record_path, |transaction| {
            transaction
                .open_table(RECORDS_V1)
                .unwrap()
                .insert(foreign.as_str(), record_wire.as_slice())
                .unwrap();
        });
        let reopened = ArchiveStore::open(&record_path, limits).unwrap();
        assert!(matches!(
            reopened.membership(&key("foreign")),
            Err(ArchiveStoreError::CorruptRecord(_))
        ));

        let lease_path = test_path("foreign-lease-key");
        let store = ArchiveStore::open(&lease_path, limits).unwrap();
        let lease = acquired(store.claim(&claim_request("lease", 1_000)).unwrap());
        let lease_wire = lease.to_wire_bytes(limits).unwrap();
        drop(store);
        raw_write(&lease_path, |transaction| {
            transaction
                .open_table(LEASES)
                .unwrap()
                .insert(foreign.as_str(), lease_wire.as_slice())
                .unwrap();
        });
        let reopened = ArchiveStore::open(&lease_path, limits).unwrap();
        assert!(matches!(
            reopened.membership(&key("foreign")),
            Err(ArchiveStoreError::CorruptRecord(_))
        ));

        let import_path = test_path("foreign-import-key");
        let store = ArchiveStore::open(&import_path, limits).unwrap();
        let imported_key = key("imported-key-binding");
        let batch = ArchiveImportBatch {
            schema: ARCHIVE_SCHEMA,
            format: ArchiveImportFormat::FerricMappedTextV1,
            source_digest: digest('f'),
            entries: vec![ArchiveImportEntry {
                source_line_number: 1,
                source_identity: "key-bound import".to_owned(),
                mapping: ArchiveImportMapping::AssetIdentity,
                target_key: imported_key.clone(),
            }],
        };
        store.import_mapped_text(&batch, 5_000).unwrap();
        let marker = match store.membership(&imported_key).unwrap() {
            ArchiveMembership::Imported { marker } => marker,
            other => panic!("expected imported marker, received {other:?}"),
        };
        let marker_wire = marker.to_wire_bytes(limits).unwrap();
        drop(store);
        raw_write(&import_path, |transaction| {
            transaction
                .open_table(IMPORTS)
                .unwrap()
                .insert(foreign.as_str(), marker_wire.as_slice())
                .unwrap();
        });
        let reopened = ArchiveStore::open(&import_path, limits).unwrap();
        assert!(matches!(
            reopened.membership(&key("foreign")),
            Err(ArchiveStoreError::CorruptRecord(_))
        ));

        let migration_path = test_path("foreign-migration-key");
        let store = ArchiveStore::open(&migration_path, limits).unwrap();
        let lease = acquired(store.claim(&claim_request("migration-key", 1_000)).unwrap());
        store
            .commit_success(&commit_request(lease, "migration-key", 1_500))
            .unwrap();
        let plan = ArchiveMigrationPlan {
            migration_id: "foreign-migration-key".to_owned(),
            from_store_version: 1,
            to_store_version: 2,
            maximum_records_per_batch: 8,
        };
        store.begin_migration(&plan, digest('e')).unwrap();
        store.resume_migration(&plan.migration_id).unwrap();
        drop(store);
        let shared = key("shared").canonical_key(limits).unwrap();
        raw_write(&migration_path, |transaction| {
            let mut target = transaction.open_table(RECORDS_V2).unwrap();
            let bytes = target.get(shared.as_str()).unwrap().unwrap();
            let owned = bytes.value().to_vec();
            drop(bytes);
            target.insert(foreign.as_str(), owned.as_slice()).unwrap();
        });
        assert!(matches!(
            ArchiveStore::open(&migration_path, limits),
            Err(ArchiveStoreError::CorruptRecord(_))
        ));

        let migration_state_path = test_path("foreign-migration-state-key");
        let store = ArchiveStore::open(&migration_state_path, limits).unwrap();
        let plan = ArchiveMigrationPlan {
            migration_id: "foreign-state-key".to_owned(),
            from_store_version: 1,
            to_store_version: 2,
            maximum_records_per_batch: 8,
        };
        let state = store.begin_migration(&plan, digest('e')).unwrap();
        let state_wire = state.to_wire_bytes(limits).unwrap();
        drop(store);
        raw_write(&migration_state_path, |transaction| {
            transaction
                .open_table(MIGRATION_RECORDS)
                .unwrap()
                .insert("foreign", state_wire.as_slice())
                .unwrap();
        });
        assert!(matches!(
            ArchiveStore::open(&migration_state_path, limits),
            Err(ArchiveStoreError::CorruptRecord(_))
        ));
    }

    #[test]
    fn store_version_is_coupled_to_migration_checkpoint_and_tables() {
        let limits = ArchiveLimits::default();

        let missing_checkpoint = test_path("v2-missing-checkpoint");
        drop(ArchiveStore::open(&missing_checkpoint, limits).unwrap());
        raw_write(&missing_checkpoint, |transaction| {
            transaction
                .open_table(META_U64)
                .unwrap()
                .insert(STORE_VERSION, CURRENT_STORE_VERSION)
                .unwrap();
        });
        assert!(matches!(
            ArchiveStore::open(&missing_checkpoint, limits),
            Err(ArchiveStoreError::CorruptRecord(_))
        ));

        let wrong_phase = test_path("v2-wrong-phase");
        let store = ArchiveStore::open(&wrong_phase, limits).unwrap();
        let plan = ArchiveMigrationPlan {
            migration_id: "wrong-phase".to_owned(),
            from_store_version: 1,
            to_store_version: 2,
            maximum_records_per_batch: 8,
        };
        store.begin_migration(&plan, digest('e')).unwrap();
        drop(store);
        raw_write(&wrong_phase, |transaction| {
            transaction
                .open_table(META_U64)
                .unwrap()
                .insert(STORE_VERSION, CURRENT_STORE_VERSION)
                .unwrap();
        });
        assert!(matches!(
            ArchiveStore::open(&wrong_phase, limits),
            Err(ArchiveStoreError::CorruptRecord(_))
        ));

        let divergent = test_path("v2-divergent-target");
        let store = ArchiveStore::open(&divergent, limits).unwrap();
        let lease = acquired(store.claim(&claim_request("divergent", 1_000)).unwrap());
        store
            .commit_success(&commit_request(lease, "divergent", 1_500))
            .unwrap();
        let plan = ArchiveMigrationPlan {
            migration_id: "divergent-target".to_owned(),
            from_store_version: 1,
            to_store_version: 2,
            maximum_records_per_batch: 8,
        };
        store.begin_migration(&plan, digest('e')).unwrap();
        store.resume_migration(&plan.migration_id).unwrap();
        store.verify_migration(&plan.migration_id).unwrap();
        store.activate_migration(&plan.migration_id).unwrap();
        drop(store);
        let shared = key("shared").canonical_key(limits).unwrap();
        raw_write(&divergent, |transaction| {
            transaction
                .open_table(RECORDS_V2)
                .unwrap()
                .remove(shared.as_str())
                .unwrap();
        });
        assert!(matches!(
            ArchiveStore::open(&divergent, limits),
            Err(ArchiveStoreError::CorruptRecord(_))
        ));

        let hidden_activation = test_path("v1-hidden-activation");
        let store = ArchiveStore::open(&hidden_activation, limits).unwrap();
        let plan = ArchiveMigrationPlan {
            migration_id: "hidden-activation".to_owned(),
            from_store_version: 1,
            to_store_version: 2,
            maximum_records_per_batch: 8,
        };
        store.begin_migration(&plan, digest('e')).unwrap();
        store.resume_migration(&plan.migration_id).unwrap();
        store.verify_migration(&plan.migration_id).unwrap();
        store.activate_migration(&plan.migration_id).unwrap();
        drop(store);
        raw_write(&hidden_activation, |transaction| {
            transaction
                .open_table(META_U64)
                .unwrap()
                .insert(STORE_VERSION, INITIAL_STORE_VERSION)
                .unwrap();
        });
        assert!(matches!(
            ArchiveStore::open(&hidden_activation, limits),
            Err(ArchiveStoreError::CorruptRecord(_))
        ));
    }

    #[test]
    fn corrupt_file_and_unknown_store_version_are_rejected() {
        let torn = test_path("torn");
        drop(ArchiveStore::open(&torn, ArchiveLimits::default()).unwrap());
        let original_length = fs::metadata(&torn).unwrap().len();
        fs::OpenOptions::new()
            .write(true)
            .open(&torn)
            .unwrap()
            .set_len(original_length / 2)
            .unwrap();
        let guarded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ArchiveStore::open(&torn, ArchiveLimits::default())
        }));
        match guarded {
            Ok(Err(ArchiveStoreError::OpenFailed(_))) => {}
            Ok(other) => panic!("torn store returned unexpected result: {other:?}"),
            Err(payload) => panic!(
                "torn store panic escaped ArchiveStore::open: {:?}",
                (*payload).type_id()
            ),
        }

        let corrupt = test_path("corrupt");
        fs::write(&corrupt, b"not-a-redb-database").unwrap();
        assert!(matches!(
            ArchiveStore::open(&corrupt, ArchiveLimits::default()),
            Err(ArchiveStoreError::OpenFailed(_))
        ));

        let newer = test_path("newer-version");
        let store = ArchiveStore::open(&newer, ArchiveLimits::default()).unwrap();
        drop(store);
        let database = Database::create(&newer).unwrap();
        let mut transaction = database.begin_write().unwrap();
        transaction.set_durability(Durability::Immediate).unwrap();
        transaction.set_two_phase_commit(true);
        transaction.set_quick_repair(true);
        {
            let mut metadata = transaction.open_table(META_U64).unwrap();
            metadata.insert(STORE_VERSION, 99).unwrap();
        }
        transaction.commit().unwrap();
        drop(database);
        assert!(matches!(
            ArchiveStore::open(&newer, ArchiveLimits::default()),
            Err(ArchiveStoreError::UnsupportedStoreVersion(99))
        ));

        let missing = test_path("missing-version");
        drop(ArchiveStore::open(&missing, ArchiveLimits::default()).unwrap());
        let database = Database::create(&missing).unwrap();
        let mut transaction = database.begin_write().unwrap();
        transaction.set_durability(Durability::Immediate).unwrap();
        transaction.set_two_phase_commit(true);
        transaction.set_quick_repair(true);
        {
            let mut metadata = transaction.open_table(META_U64).unwrap();
            metadata.remove(STORE_VERSION).unwrap();
        }
        transaction.commit().unwrap();
        drop(database);
        assert!(matches!(
            ArchiveStore::open(&missing, ArchiveLimits::default()),
            Err(ArchiveStoreError::CorruptRecord(_))
        ));
    }
}
