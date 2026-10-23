//! Candidate-neutral archive identity, claim, commit, reconciliation, migration,
//! and import contracts.
//!
//! Archive wire data is strict JSON: undeclared fields and enum variants are
//! rejected, and callers must use the bounded `from_wire_bytes` constructors at
//! trust boundaries. Identity keys are independent of any database layout.

use crate::{
    AssetId, CapabilitySupport, DerivedOutputId, FilesystemProfileContract, FilesystemProfileError,
    FilesystemProofMode, ItemId, JobId, RepresentationId, SchemaVersion, TrackId, TransactionId,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Current Ferric archive contract version.
pub const ARCHIVE_SCHEMA: SchemaVersion = SchemaVersion { major: 1, minor: 0 };

/// Bounded archive contract limits applied at every public wire boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArchiveLimits {
    pub maximum_wire_bytes: usize,
    pub maximum_text_bytes: usize,
    pub maximum_identities_per_output: usize,
    pub maximum_import_entries: usize,
    pub maximum_lease_millis: u64,
    pub maximum_migration_batch_records: u32,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            maximum_wire_bytes: 1024 * 1024,
            maximum_text_bytes: 4096,
            maximum_identities_per_output: 64,
            maximum_import_entries: 1024,
            maximum_lease_millis: 24 * 60 * 60 * 1000,
            maximum_migration_batch_records: 4096,
        }
    }
}

/// The five distinct archive identity levels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveIdentityLevel {
    Item,
    Representation,
    Track,
    Asset,
    DerivedOutput,
}

impl ArchiveIdentityLevel {
    const fn wire_name(self) -> &'static str {
        match self {
            Self::Item => "item",
            Self::Representation => "representation",
            Self::Track => "track",
            Self::Asset => "asset",
            Self::DerivedOutput => "derived_output",
        }
    }
}

/// Exactly one stable identity at one of Ferric's five archive levels.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(
    tag = "level",
    content = "id",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ArchiveIdentity {
    Item(ItemId),
    Representation(RepresentationId),
    Track(TrackId),
    Asset(AssetId),
    DerivedOutput(DerivedOutputId),
}

impl ArchiveIdentity {
    /// Returns the identity level without erasing its typed ID.
    #[must_use]
    pub const fn level(&self) -> ArchiveIdentityLevel {
        match self {
            Self::Item(_) => ArchiveIdentityLevel::Item,
            Self::Representation(_) => ArchiveIdentityLevel::Representation,
            Self::Track(_) => ArchiveIdentityLevel::Track,
            Self::Asset(_) => ArchiveIdentityLevel::Asset,
            Self::DerivedOutput(_) => ArchiveIdentityLevel::DerivedOutput,
        }
    }

    /// Returns the canonical stable-ID text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Item(value) => value.as_str(),
            Self::Representation(value) => value.as_str(),
            Self::Track(value) => value.as_str(),
            Self::Asset(value) => value.as_str(),
            Self::DerivedOutput(value) => value.as_str(),
        }
    }
}

/// Archive namespace prevents source assets, metadata, and actions colliding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveNamespace {
    SourceAsset,
    GeneratedMetadata,
    PostProcessingAction,
}

impl ArchiveNamespace {
    const fn wire_name(self) -> &'static str {
        match self {
            Self::SourceAsset => "source_asset",
            Self::GeneratedMetadata => "generated_metadata",
            Self::PostProcessingAction => "post_processing_action",
        }
    }
}

/// Versioned uniqueness key owned by Ferric, not by a storage candidate.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveKey {
    pub schema: SchemaVersion,
    pub namespace: ArchiveNamespace,
    pub identity: ArchiveIdentity,
    pub identity_rule_version: u16,
    pub extractor_id: String,
}

impl ArchiveKey {
    /// Validates version, rule version, and canonical extractor identity.
    ///
    /// # Errors
    ///
    /// Returns [`ArchiveContractError`] for an unsupported schema or invalid field.
    pub fn validate(&self, limits: ArchiveLimits) -> Result<(), ArchiveContractError> {
        validate_schema(self.schema)?;
        if self.identity_rule_version == 0 {
            return Err(ArchiveContractError::ZeroValue {
                field: "identity_rule_version",
            });
        }
        validate_component(
            "extractor_id",
            &self.extractor_id,
            limits.maximum_text_bytes,
        )
    }

    /// Returns the deterministic database key. No backend-specific bytes enter it.
    ///
    /// # Errors
    ///
    /// Returns [`ArchiveContractError`] when this key is not canonical.
    pub fn canonical_key(&self, limits: ArchiveLimits) -> Result<String, ArchiveContractError> {
        self.validate(limits)?;
        Ok(format!(
            "archive-identity-v{}|{}|{}|{}|{}",
            self.identity_rule_version,
            self.namespace.wire_name(),
            self.identity.level().wire_name(),
            self.extractor_id,
            self.identity.as_str()
        ))
    }
}

/// Provenance retained with every claim and committed archive row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveProvenance {
    pub job_id: JobId,
    pub transaction_id: TransactionId,
    pub source_locator_digest: String,
    pub request_provenance_digest: String,
}

impl ArchiveProvenance {
    /// Validates both SHA-256 provenance digests.
    ///
    /// # Errors
    ///
    /// Returns [`ArchiveContractError`] when either digest is not lowercase SHA-256.
    pub fn validate(&self, _limits: ArchiveLimits) -> Result<(), ArchiveContractError> {
        validate_digest("source_locator_digest", &self.source_locator_digest)?;
        validate_digest("request_provenance_digest", &self.request_provenance_digest)
    }
}

/// Opaque caller-generated lease token with a stable restricted grammar.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct LeaseToken(String);

impl LeaseToken {
    /// Creates a bounded canonical token beginning with `lease_`.
    ///
    /// # Errors
    ///
    /// Returns [`ArchiveContractError`] when the token grammar is invalid.
    pub fn new(value: impl Into<String>) -> Result<Self, ArchiveContractError> {
        let value = value.into();
        if !value.starts_with("lease_") || value.len() == "lease_".len() || value.len() > 128 {
            return Err(ArchiveContractError::InvalidField {
                field: "lease_token",
            });
        }
        if value.bytes().any(|byte| {
            !byte.is_ascii_lowercase()
                && !byte.is_ascii_digit()
                && !matches!(byte, b'_' | b'-' | b'.')
        }) {
            return Err(ArchiveContractError::InvalidField {
                field: "lease_token",
            });
        }
        Ok(Self(value))
    }

    /// Returns the canonical token text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for LeaseToken {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Atomic claim request. Time and token inputs are explicit for deterministic tests.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveClaimRequest {
    pub key: ArchiveKey,
    pub owner_job_id: JobId,
    pub lease_token: LeaseToken,
    pub requested_at_unix_millis: u64,
    pub lease_duration_millis: u64,
    pub provenance: ArchiveProvenance,
}

impl ArchiveClaimRequest {
    /// Validates key, ownership, provenance, clock, and lease bounds.
    ///
    /// # Errors
    ///
    /// Returns [`ArchiveContractError`] for any invalid field or exceeded bound.
    pub fn validate(&self, limits: ArchiveLimits) -> Result<(), ArchiveContractError> {
        self.key.validate(limits)?;
        self.provenance.validate(limits)?;
        if self.owner_job_id != self.provenance.job_id {
            return Err(ArchiveContractError::IdentityMismatch {
                field: "owner_job_id",
            });
        }
        if self.requested_at_unix_millis == 0 {
            return Err(ArchiveContractError::ZeroValue {
                field: "requested_at_unix_millis",
            });
        }
        if self.lease_duration_millis == 0
            || self.lease_duration_millis > limits.maximum_lease_millis
        {
            return Err(ArchiveContractError::LimitExceeded {
                field: "lease_duration_millis",
                actual: usize_from_u64(self.lease_duration_millis),
                maximum: usize_from_u64(limits.maximum_lease_millis),
            });
        }
        self.requested_at_unix_millis
            .checked_add(self.lease_duration_millis)
            .ok_or(ArchiveContractError::TimeOverflow)
            .map(|_| ())
    }
}

/// Versioned lease returned by an atomic claim transaction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveLease {
    pub key: ArchiveKey,
    pub owner_job_id: JobId,
    pub claim_provenance: ArchiveProvenance,
    pub token: LeaseToken,
    pub generation: u64,
    pub acquired_at_unix_millis: u64,
    pub expires_at_unix_millis: u64,
}

impl ArchiveLease {
    /// Validates a nonzero generation and an increasing bounded lease interval.
    ///
    /// # Errors
    ///
    /// Returns [`ArchiveContractError`] when the key, generation, or interval is invalid.
    pub fn validate(&self, limits: ArchiveLimits) -> Result<(), ArchiveContractError> {
        self.key.validate(limits)?;
        self.claim_provenance.validate(limits)?;
        if self.owner_job_id != self.claim_provenance.job_id {
            return Err(ArchiveContractError::IdentityMismatch {
                field: "lease.owner_job_id",
            });
        }
        if self.generation == 0 {
            return Err(ArchiveContractError::ZeroValue {
                field: "lease_generation",
            });
        }
        if self.acquired_at_unix_millis == 0
            || self.expires_at_unix_millis <= self.acquired_at_unix_millis
        {
            return Err(ArchiveContractError::InvalidTimeRange);
        }
        let duration = self.expires_at_unix_millis - self.acquired_at_unix_millis;
        if duration > limits.maximum_lease_millis {
            return Err(ArchiveContractError::LimitExceeded {
                field: "lease_duration_millis",
                actual: usize_from_u64(duration),
                maximum: usize_from_u64(limits.maximum_lease_millis),
            });
        }
        Ok(())
    }

    /// Reports staleness at the supplied deterministic clock instant.
    #[must_use]
    pub const fn is_stale_at(&self, now_unix_millis: u64) -> bool {
        now_unix_millis >= self.expires_at_unix_millis
    }
}

/// Exact-token lease renewal request with an explicit deterministic clock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveLeaseRenewalRequest {
    pub current_lease: ArchiveLease,
    pub new_token: LeaseToken,
    pub renewed_at_unix_millis: u64,
    pub lease_duration_millis: u64,
}

impl ArchiveLeaseRenewalRequest {
    /// Validates a live exact lease, token rotation, and bounded new interval.
    ///
    /// # Errors
    ///
    /// Returns [`ArchiveContractError`] for stale input, token reuse, or invalid bounds.
    pub fn validate(&self, limits: ArchiveLimits) -> Result<(), ArchiveContractError> {
        self.current_lease.validate(limits)?;
        if self.new_token == self.current_lease.token {
            return Err(ArchiveContractError::IdentityMismatch { field: "new_token" });
        }
        if self.renewed_at_unix_millis < self.current_lease.acquired_at_unix_millis
            || self.current_lease.is_stale_at(self.renewed_at_unix_millis)
        {
            return Err(ArchiveContractError::LeaseExpired);
        }
        if self.lease_duration_millis == 0
            || self.lease_duration_millis > limits.maximum_lease_millis
        {
            return Err(ArchiveContractError::LimitExceeded {
                field: "lease_duration_millis",
                actual: usize_from_u64(self.lease_duration_millis),
                maximum: usize_from_u64(limits.maximum_lease_millis),
            });
        }
        self.renewed_at_unix_millis
            .checked_add(self.lease_duration_millis)
            .ok_or(ArchiveContractError::TimeOverflow)?;
        self.current_lease
            .generation
            .checked_add(1)
            .ok_or(ArchiveContractError::GenerationOverflow)?;
        Ok(())
    }

    /// Produces the next validated lease without reading ambient time.
    ///
    /// # Errors
    ///
    /// Returns [`ArchiveContractError`] when renewal prerequisites are invalid.
    pub fn renewed_lease(
        &self,
        limits: ArchiveLimits,
    ) -> Result<ArchiveLease, ArchiveContractError> {
        self.validate(limits)?;
        let expires_at_unix_millis = self
            .renewed_at_unix_millis
            .checked_add(self.lease_duration_millis)
            .ok_or(ArchiveContractError::TimeOverflow)?;
        let generation = self
            .current_lease
            .generation
            .checked_add(1)
            .ok_or(ArchiveContractError::GenerationOverflow)?;
        Ok(ArchiveLease {
            key: self.current_lease.key.clone(),
            owner_job_id: self.current_lease.owner_job_id.clone(),
            claim_provenance: self.current_lease.claim_provenance.clone(),
            token: self.new_token.clone(),
            generation,
            acquired_at_unix_millis: self.renewed_at_unix_millis,
            expires_at_unix_millis,
        })
    }
}

/// Atomic claim result. Exactly one claimant receives `Acquired`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ArchiveClaimOutcome {
    Acquired {
        lease: ArchiveLease,
    },
    AlreadyCommitted {
        record: Box<ArchiveRecord>,
    },
    AlreadyImported {
        marker: Box<ArchiveImportMarker>,
    },
    HeldByOther {
        owner_job_id: JobId,
        generation: u64,
        expires_at_unix_millis: u64,
    },
}

impl ArchiveClaimOutcome {
    /// Validates the complete result payload.
    ///
    /// # Errors
    ///
    /// Returns [`ArchiveContractError`] when the embedded lease or record is invalid.
    pub fn validate(&self, limits: ArchiveLimits) -> Result<(), ArchiveContractError> {
        match self {
            Self::Acquired { lease } => lease.validate(limits),
            Self::AlreadyCommitted { record } => record.validate(limits),
            Self::AlreadyImported { marker } => marker.validate(limits),
            Self::HeldByOther {
                generation,
                expires_at_unix_millis,
                ..
            } => {
                require_nonzero("lease_generation", *generation)?;
                require_nonzero("expires_at_unix_millis", *expires_at_unix_millis)
            }
        }
    }
}

/// Configured success event that authorizes archive insertion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ArchiveSuccessEvent {
    PerAsset {
        asset_id: AssetId,
    },
    PerRepresentation {
        representation_id: RepresentationId,
        asset_id: AssetId,
    },
    PerTrack {
        track_id: TrackId,
        asset_id: AssetId,
    },
    PerDerivedOutput {
        derived_output_id: DerivedOutputId,
        asset_id: AssetId,
    },
    SuccessfulCollection {
        item_id: ItemId,
    },
}

impl ArchiveSuccessEvent {
    const fn identity_level(&self) -> ArchiveIdentityLevel {
        match self {
            Self::SuccessfulCollection { .. } => ArchiveIdentityLevel::Item,
            Self::PerRepresentation { .. } => ArchiveIdentityLevel::Representation,
            Self::PerTrack { .. } => ArchiveIdentityLevel::Track,
            Self::PerAsset { .. } => ArchiveIdentityLevel::Asset,
            Self::PerDerivedOutput { .. } => ArchiveIdentityLevel::DerivedOutput,
        }
    }

    fn validate_for(
        &self,
        key: &ArchiveKey,
        output: &ReconciledArchiveOutput,
    ) -> Result<(), ArchiveContractError> {
        let level = self.identity_level();
        let namespace_supports_level = match key.namespace {
            ArchiveNamespace::SourceAsset => level != ArchiveIdentityLevel::DerivedOutput,
            ArchiveNamespace::GeneratedMetadata => true,
            ArchiveNamespace::PostProcessingAction => matches!(
                level,
                ArchiveIdentityLevel::Asset | ArchiveIdentityLevel::DerivedOutput
            ),
        };
        if !namespace_supports_level || key.identity.level() != level {
            return Err(ArchiveContractError::InvalidIdentityEventCombination);
        }

        match (&key.identity, self) {
            (ArchiveIdentity::Item(key_id), Self::SuccessfulCollection { item_id }) => {
                require_identity_match("success_event.item_id", key_id, item_id)
            }
            (
                ArchiveIdentity::Representation(key_id),
                Self::PerRepresentation {
                    representation_id,
                    asset_id,
                },
            ) => {
                require_identity_match(
                    "success_event.representation_id",
                    key_id,
                    representation_id,
                )?;
                require_output_asset(output, asset_id)
            }
            (ArchiveIdentity::Track(key_id), Self::PerTrack { track_id, asset_id }) => {
                require_identity_match("success_event.track_id", key_id, track_id)?;
                require_output_asset(output, asset_id)
            }
            (ArchiveIdentity::Asset(key_id), Self::PerAsset { asset_id }) => {
                require_identity_match("success_event.asset_id", key_id, asset_id)?;
                require_output_asset(output, asset_id)
            }
            (
                ArchiveIdentity::DerivedOutput(key_id),
                Self::PerDerivedOutput {
                    derived_output_id,
                    asset_id,
                },
            ) => {
                require_identity_match(
                    "success_event.derived_output_id",
                    key_id,
                    derived_output_id,
                )?;
                if !output.derived_output_ids.contains(derived_output_id) {
                    return Err(ArchiveContractError::IdentityMismatch {
                        field: "output.derived_output_ids",
                    });
                }
                require_output_asset(output, asset_id)
            }
            _ => Err(ArchiveContractError::InvalidIdentityEventCombination),
        }
    }
}

/// Exact WP-009 filesystem profile plus commit-time durability acknowledgements.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveFilesystemCommitment {
    pub profile: FilesystemProfileContract,
    pub placement: ArchivePlacementProof,
    pub synchronization: ArchiveSynchronizationProof,
    pub confinement: ArchiveConfinementProof,
}

/// Whether staging and final output were proven to share one filesystem.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchivePlacementProof {
    SameFilesystem,
    CrossVolume,
}

/// Commit-time synchronization evidence for output data and its parent directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveSynchronizationProof {
    DataAndParentDirectory,
    DataOnly,
    Unacknowledged,
}

/// Commit-time acknowledgement of root-handle confinement verification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveConfinementProof {
    RootHandleVerified,
    Unacknowledged,
}

impl ArchiveFilesystemCommitment {
    /// Rejects unknown, mutated, degraded, unsupported, or unacknowledged profiles.
    ///
    /// # Errors
    ///
    /// Returns [`ArchiveContractError`] unless this is the exact supported WP-009
    /// local profile and every same-filesystem durability precondition was met.
    pub fn validate(&self) -> Result<(), ArchiveContractError> {
        self.profile.validate_exact().map_err(map_profile_error)?;
        if self.profile.proof_mode != FilesystemProofMode::SupportedLocalPositiveModel {
            return Err(ArchiveContractError::DegradedFilesystemProfile);
        }
        self.profile
            .validate_secure_write_profile()
            .map_err(map_profile_error)?;
        if self.profile.capability.atomic_replace != CapabilitySupport::Supported
            || self.profile.capability.file_sync != CapabilitySupport::Supported
            || self.profile.capability.locking != CapabilitySupport::Supported
        {
            return Err(ArchiveContractError::UnsupportedFilesystemProfile);
        }
        if self.placement != ArchivePlacementProof::SameFilesystem {
            return Err(ArchiveContractError::FilesystemCommitmentMissing {
                field: "same_filesystem",
            });
        }
        if self.synchronization != ArchiveSynchronizationProof::DataAndParentDirectory {
            return Err(ArchiveContractError::FilesystemCommitmentMissing {
                field: "data_and_parent_directory_synchronized",
            });
        }
        if self.confinement != ArchiveConfinementProof::RootHandleVerified {
            return Err(ArchiveContractError::FilesystemCommitmentMissing {
                field: "root_handle_confinement",
            });
        }
        Ok(())
    }
}

/// Output identity observed after final validation and filesystem reconciliation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciledArchiveOutput {
    pub final_output_identity: String,
    pub artifact_size_bytes: u64,
    pub artifact_digest: String,
    pub reconciliation_receipt_digest: String,
    pub filesystem_commitment: ArchiveFilesystemCommitment,
    pub asset_ids: Vec<AssetId>,
    pub derived_output_ids: Vec<DerivedOutputId>,
}

impl ReconciledArchiveOutput {
    /// Validates output identity, digests, profile, and sorted identity sets.
    ///
    /// # Errors
    ///
    /// Returns [`ArchiveContractError`] for missing, duplicate, unsorted, or oversized data.
    pub fn validate(&self, limits: ArchiveLimits) -> Result<(), ArchiveContractError> {
        validate_text(
            "final_output_identity",
            &self.final_output_identity,
            limits.maximum_text_bytes,
        )?;
        validate_digest("artifact_digest", &self.artifact_digest)?;
        validate_digest(
            "reconciliation_receipt_digest",
            &self.reconciliation_receipt_digest,
        )?;
        self.filesystem_commitment.validate()?;
        validate_nonempty_bounded(
            "asset_ids",
            self.asset_ids.len(),
            limits.maximum_identities_per_output,
        )?;
        validate_nonempty_bounded(
            "derived_output_ids",
            self.derived_output_ids.len(),
            limits.maximum_identities_per_output,
        )?;
        reject_adjacent_duplicates("asset_ids", &self.asset_ids)?;
        reject_adjacent_duplicates("derived_output_ids", &self.derived_output_ids)
    }
}

/// Commit request accepted only for an unexpired, exact lease token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveCommitRequest {
    pub lease: ArchiveLease,
    pub success_event: ArchiveSuccessEvent,
    pub output: ReconciledArchiveOutput,
    pub provenance: ArchiveProvenance,
    pub committed_at_unix_millis: u64,
}

impl ArchiveCommitRequest {
    /// Validates exact lease ownership, expiry, output, and success-event identity.
    ///
    /// # Errors
    ///
    /// Returns [`ArchiveContractError`] when commit prerequisites are not met.
    pub fn validate(&self, limits: ArchiveLimits) -> Result<(), ArchiveContractError> {
        self.lease.validate(limits)?;
        self.output.validate(limits)?;
        self.provenance.validate(limits)?;
        if self.lease.claim_provenance != self.provenance {
            return Err(ArchiveContractError::IdentityMismatch {
                field: "lease.claim_provenance",
            });
        }
        if self.committed_at_unix_millis < self.lease.acquired_at_unix_millis
            || self.committed_at_unix_millis >= self.lease.expires_at_unix_millis
        {
            return Err(ArchiveContractError::LeaseExpired);
        }
        self.success_event
            .validate_for(&self.lease.key, &self.output)
    }
}

/// One successfully inserted archive row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveRecord {
    pub schema: SchemaVersion,
    pub archive_row_id: String,
    pub key: ArchiveKey,
    pub success_event: ArchiveSuccessEvent,
    pub output: ReconciledArchiveOutput,
    pub provenance: ArchiveProvenance,
    pub claim_lease_token: LeaseToken,
    pub claim_lease_generation: u64,
    pub commit_sequence: u64,
    pub committed_at_unix_millis: u64,
}

impl ArchiveRecord {
    /// Validates the persisted row and all nested contract data.
    ///
    /// # Errors
    ///
    /// Returns [`ArchiveContractError`] for an invalid schema, key, output, or sequence.
    pub fn validate(&self, limits: ArchiveLimits) -> Result<(), ArchiveContractError> {
        validate_schema(self.schema)?;
        validate_component(
            "archive_row_id",
            &self.archive_row_id,
            limits.maximum_text_bytes,
        )?;
        self.key.validate(limits)?;
        self.output.validate(limits)?;
        self.provenance.validate(limits)?;
        require_nonzero("claim_lease_generation", self.claim_lease_generation)?;
        require_nonzero("commit_sequence", self.commit_sequence)?;
        require_nonzero("committed_at_unix_millis", self.committed_at_unix_millis)?;
        self.success_event.validate_for(&self.key, &self.output)
    }
}

/// Idempotent commit result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ArchiveCommitOutcome {
    Inserted { record: ArchiveRecord },
    AlreadyCommitted { record: ArchiveRecord },
}

impl ArchiveCommitOutcome {
    /// Validates the inserted or previously committed row.
    ///
    /// # Errors
    ///
    /// Returns [`ArchiveContractError`] when the embedded record is invalid.
    pub fn validate(&self, limits: ArchiveLimits) -> Result<(), ArchiveContractError> {
        match self {
            Self::Inserted { record } | Self::AlreadyCommitted { record } => {
                record.validate(limits)
            }
        }
    }
}

/// Candidate-neutral membership lookup result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ArchiveMembership {
    Absent,
    Claimed { lease: Box<ArchiveLease> },
    Committed { record: Box<ArchiveRecord> },
    Imported { marker: Box<ArchiveImportMarker> },
}

impl ArchiveMembership {
    /// Validates any embedded lease or committed row.
    ///
    /// # Errors
    ///
    /// Returns [`ArchiveContractError`] when embedded state is invalid.
    pub fn validate(&self, limits: ArchiveLimits) -> Result<(), ArchiveContractError> {
        match self {
            Self::Absent => Ok(()),
            Self::Claimed { lease } => lease.validate(limits),
            Self::Committed { record } => record.validate(limits),
            Self::Imported { marker } => marker.validate(limits),
        }
    }
}

/// Filesystem output observation used by deterministic startup reconciliation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ArchiveOutputObservation {
    Missing,
    Matching {
        output: Box<ReconciledArchiveOutput>,
    },
    Mismatched {
        final_output_identity: String,
    },
}

/// Store-row observation used by deterministic startup reconciliation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ArchiveRowObservation {
    Missing,
    Matching { record: Box<ArchiveRecord> },
    Mismatched { archive_row_id: String },
}

/// Immutable inputs for one reconciliation decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveReconciliationObservation {
    pub key: ArchiveKey,
    pub now_unix_millis: u64,
    pub output: ArchiveOutputObservation,
    pub row: ArchiveRowObservation,
    pub lease: Option<ArchiveLease>,
    pub staged_output: Option<ReconciledArchiveOutput>,
    pub recovery_record: Option<ArchiveRecord>,
}

impl ArchiveReconciliationObservation {
    /// Validates the complete immutable observation before deciding recovery.
    ///
    /// # Errors
    ///
    /// Returns [`ArchiveContractError`] for identity drift or invalid nested state.
    pub fn validate(&self, limits: ArchiveLimits) -> Result<(), ArchiveContractError> {
        self.key.validate(limits)?;
        require_nonzero("now_unix_millis", self.now_unix_millis)?;
        if let Some(lease) = &self.lease {
            lease.validate(limits)?;
            if lease.key != self.key {
                return Err(ArchiveContractError::IdentityMismatch { field: "lease.key" });
            }
            if self.now_unix_millis < lease.acquired_at_unix_millis {
                return Err(ArchiveContractError::ReconciliationClockBeforeLeaseAcquisition);
            }
        }
        if let Some(output) = &self.staged_output {
            output.validate(limits)?;
        }
        if let Some(record) = &self.recovery_record {
            record.validate(limits)?;
            if record.key != self.key {
                return Err(ArchiveContractError::IdentityMismatch {
                    field: "recovery_record.key",
                });
            }
        }
        match &self.output {
            ArchiveOutputObservation::Missing => {}
            ArchiveOutputObservation::Matching { output } => output.validate(limits)?,
            ArchiveOutputObservation::Mismatched {
                final_output_identity,
            } => validate_text(
                "final_output_identity",
                final_output_identity,
                limits.maximum_text_bytes,
            )?,
        }
        match &self.row {
            ArchiveRowObservation::Missing => {}
            ArchiveRowObservation::Matching { record } => {
                record.validate(limits)?;
                if record.key != self.key {
                    return Err(ArchiveContractError::IdentityMismatch {
                        field: "row.record.key",
                    });
                }
            }
            ArchiveRowObservation::Mismatched { archive_row_id } => {
                validate_component("archive_row_id", archive_row_id, limits.maximum_text_bytes)?;
            }
        }
        Ok(())
    }

    /// Calculates one idempotent next action without touching storage or the filesystem.
    ///
    /// # Errors
    ///
    /// Returns [`ArchiveContractError`] when the observation itself is invalid.
    pub fn decide(
        &self,
        limits: ArchiveLimits,
    ) -> Result<ArchiveReconciliationDecision, ArchiveContractError> {
        self.validate(limits)?;
        match (&self.output, &self.row) {
            (
                ArchiveOutputObservation::Matching { output },
                ArchiveRowObservation::Matching { record },
            ) if self.lease.is_some() => Ok(ArchiveReconciliationDecision::FailClosed {
                reason: ArchiveReconciliationFailure::UnexpectedLeaseAfterCommit,
            }),
            (
                ArchiveOutputObservation::Matching { output },
                ArchiveRowObservation::Matching { record },
            ) if &record.output == output.as_ref() => Ok(ArchiveReconciliationDecision::Reconciled),
            (ArchiveOutputObservation::Matching { .. }, ArchiveRowObservation::Matching { .. }) => {
                Ok(ArchiveReconciliationDecision::FailClosed {
                    reason: ArchiveReconciliationFailure::OutputRowMismatch,
                })
            }
            (ArchiveOutputObservation::Matching { output }, ArchiveRowObservation::Missing) => {
                if self
                    .lease
                    .as_ref()
                    .is_some_and(|lease| !lease.is_stale_at(self.now_unix_millis))
                {
                    return Ok(ArchiveReconciliationDecision::FailClosed {
                        reason: ArchiveReconciliationFailure::FreshLeaseBlocksRecovery,
                    });
                }
                if let Some(record) = &self.recovery_record
                    && &record.output == output.as_ref()
                {
                    if self.lease.as_ref().is_some_and(|lease| {
                        record.provenance != lease.claim_provenance
                            || record.claim_lease_token != lease.token
                            || record.claim_lease_generation != lease.generation
                    }) {
                        return Ok(ArchiveReconciliationDecision::FailClosed {
                            reason: ArchiveReconciliationFailure::RecoveryLeaseMismatch,
                        });
                    }
                    Ok(ArchiveReconciliationDecision::InsertMissingRow {
                        record: Box::new(record.clone()),
                    })
                } else {
                    Ok(ArchiveReconciliationDecision::FailClosed {
                        reason: ArchiveReconciliationFailure::MissingRecoveryRecord,
                    })
                }
            }
            (ArchiveOutputObservation::Missing, ArchiveRowObservation::Matching { .. })
                if self.lease.is_some() =>
            {
                Ok(ArchiveReconciliationDecision::FailClosed {
                    reason: ArchiveReconciliationFailure::UnexpectedLeaseAfterCommit,
                })
            }
            (ArchiveOutputObservation::Missing, ArchiveRowObservation::Matching { record })
                if self.staged_output.as_ref() == Some(&record.output) =>
            {
                Ok(ArchiveReconciliationDecision::RestoreOutputFromStaged)
            }
            (ArchiveOutputObservation::Missing, ArchiveRowObservation::Matching { .. }) => {
                Ok(ArchiveReconciliationDecision::FailClosed {
                    reason: ArchiveReconciliationFailure::RowWithoutRecoverableOutput,
                })
            }
            (ArchiveOutputObservation::Missing, ArchiveRowObservation::Missing) => {
                if self
                    .lease
                    .as_ref()
                    .is_some_and(|lease| lease.is_stale_at(self.now_unix_millis))
                {
                    Ok(ArchiveReconciliationDecision::ReclaimStaleLease)
                } else if self.lease.is_some() {
                    Ok(ArchiveReconciliationDecision::LeaseStillActive)
                } else {
                    Ok(ArchiveReconciliationDecision::NoArchiveState)
                }
            }
            (ArchiveOutputObservation::Mismatched { .. }, _) => {
                Ok(ArchiveReconciliationDecision::FailClosed {
                    reason: ArchiveReconciliationFailure::OutputIdentityMismatch,
                })
            }
            (_, ArchiveRowObservation::Mismatched { .. }) => {
                Ok(ArchiveReconciliationDecision::FailClosed {
                    reason: ArchiveReconciliationFailure::ArchiveRowMismatch,
                })
            }
        }
    }
}

/// One fail-closed reconciliation reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveReconciliationFailure {
    OutputRowMismatch,
    RowWithoutRecoverableOutput,
    OutputIdentityMismatch,
    ArchiveRowMismatch,
    MissingRecoveryRecord,
    FreshLeaseBlocksRecovery,
    UnexpectedLeaseAfterCommit,
    RecoveryLeaseMismatch,
}

/// Idempotent candidate-neutral reconciliation result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ArchiveReconciliationDecision {
    NoArchiveState,
    LeaseStillActive,
    Reconciled,
    InsertMissingRow {
        record: Box<ArchiveRecord>,
    },
    RestoreOutputFromStaged,
    ReclaimStaleLease,
    FailClosed {
        reason: ArchiveReconciliationFailure,
    },
}

impl ArchiveReconciliationDecision {
    /// Validates any recovery record carried by the decision.
    ///
    /// # Errors
    ///
    /// Returns [`ArchiveContractError`] when the recovery record is invalid.
    pub fn validate(&self, limits: ArchiveLimits) -> Result<(), ArchiveContractError> {
        match self {
            Self::InsertMissingRow { record } => record.validate(limits),
            _ => Ok(()),
        }
    }
}

/// Supported forward migration plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveMigrationPlan {
    pub migration_id: String,
    pub from_store_version: u32,
    pub to_store_version: u32,
    pub maximum_records_per_batch: u32,
}

impl ArchiveMigrationPlan {
    /// Validates a bounded strictly forward migration.
    ///
    /// # Errors
    ///
    /// Returns [`ArchiveContractError`] for a non-forward or oversized plan.
    pub fn validate(&self, limits: ArchiveLimits) -> Result<(), ArchiveContractError> {
        validate_component(
            "migration_id",
            &self.migration_id,
            limits.maximum_text_bytes,
        )?;
        if self.from_store_version >= self.to_store_version {
            return Err(ArchiveContractError::InvalidMigrationRange);
        }
        if self.maximum_records_per_batch == 0
            || self.maximum_records_per_batch > limits.maximum_migration_batch_records
        {
            return Err(ArchiveContractError::LimitExceeded {
                field: "maximum_records_per_batch",
                actual: self.maximum_records_per_batch as usize,
                maximum: limits.maximum_migration_batch_records as usize,
            });
        }
        Ok(())
    }
}

/// Durable migration phase. Persisting each phase enables interrupted recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveMigrationPhase {
    Prepared,
    Copying,
    Verifying,
    ReadyToActivate,
    Activated,
    RolledBack,
}

/// Durable migration checkpoint retained until activation or rollback completes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveMigrationState {
    pub plan: ArchiveMigrationPlan,
    pub phase: ArchiveMigrationPhase,
    pub migrated_records: u64,
    pub last_migrated_key: Option<ArchiveKey>,
    pub source_store_digest: String,
}

impl ArchiveMigrationState {
    /// Validates a durable migration checkpoint.
    ///
    /// # Errors
    ///
    /// Returns [`ArchiveContractError`] for an impossible phase or cursor state.
    pub fn validate(&self, limits: ArchiveLimits) -> Result<(), ArchiveContractError> {
        self.plan.validate(limits)?;
        validate_digest("source_store_digest", &self.source_store_digest)?;
        if let Some(key) = &self.last_migrated_key {
            key.validate(limits)?;
            if self.migrated_records == 0 {
                return Err(ArchiveContractError::InvalidMigrationCheckpoint);
            }
        }
        if self.migrated_records > 0 && self.last_migrated_key.is_none() {
            return Err(ArchiveContractError::InvalidMigrationCheckpoint);
        }
        match self.phase {
            ArchiveMigrationPhase::Prepared
                if self.migrated_records != 0 || self.last_migrated_key.is_some() =>
            {
                return Err(ArchiveContractError::InvalidMigrationCheckpoint);
            }
            ArchiveMigrationPhase::Copying
                if self.migrated_records == 0 || self.last_migrated_key.is_none() =>
            {
                return Err(ArchiveContractError::InvalidMigrationCheckpoint);
            }
            ArchiveMigrationPhase::Prepared
            | ArchiveMigrationPhase::Copying
            | ArchiveMigrationPhase::Verifying
            | ArchiveMigrationPhase::ReadyToActivate
            | ArchiveMigrationPhase::Activated
            | ArchiveMigrationPhase::RolledBack => {}
        }
        Ok(())
    }
}

/// Explicit known mapping for a Ferric-owned text archive entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveImportMapping {
    ItemIdentity,
    RepresentationIdentity,
    TrackIdentity,
    AssetIdentity,
    DerivedOutputIdentity,
}

impl ArchiveImportMapping {
    const fn target_level(self) -> ArchiveIdentityLevel {
        match self {
            Self::ItemIdentity => ArchiveIdentityLevel::Item,
            Self::RepresentationIdentity => ArchiveIdentityLevel::Representation,
            Self::TrackIdentity => ArchiveIdentityLevel::Track,
            Self::AssetIdentity => ArchiveIdentityLevel::Asset,
            Self::DerivedOutputIdentity => ArchiveIdentityLevel::DerivedOutput,
        }
    }
}

/// One provenance-bound, explicitly mapped text archive entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveImportEntry {
    pub source_line_number: u64,
    pub source_identity: String,
    pub mapping: ArchiveImportMapping,
    pub target_key: ArchiveKey,
}

impl ArchiveImportEntry {
    /// Validates source provenance and exact identity-level mapping.
    ///
    /// # Errors
    ///
    /// Returns [`ArchiveContractError`] for an invalid line or mapping mismatch.
    pub fn validate(&self, limits: ArchiveLimits) -> Result<(), ArchiveContractError> {
        require_nonzero("source_line_number", self.source_line_number)?;
        validate_text(
            "source_identity",
            &self.source_identity,
            limits.maximum_text_bytes,
        )?;
        self.target_key.validate(limits)?;
        if self.mapping.target_level() != self.target_key.identity.level() {
            return Err(ArchiveContractError::IdentityMismatch {
                field: "import_mapping",
            });
        }
        Ok(())
    }
}

/// Durable membership marker created by an explicitly mapped text import.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveImportMarker {
    pub schema: SchemaVersion,
    pub key: ArchiveKey,
    pub mapping: ArchiveImportMapping,
    pub source_identity: String,
    pub source_digest: String,
    pub source_line_number: u64,
    pub imported_at_unix_millis: u64,
}

impl ArchiveImportMarker {
    /// Constructs a durable marker from one validated batch entry.
    ///
    /// # Errors
    ///
    /// Returns [`ArchiveContractError`] when entry, digest, or time is invalid.
    pub fn from_entry(
        entry: &ArchiveImportEntry,
        source_digest: String,
        imported_at_unix_millis: u64,
        limits: ArchiveLimits,
    ) -> Result<Self, ArchiveContractError> {
        let marker = Self {
            schema: ARCHIVE_SCHEMA,
            key: entry.target_key.clone(),
            mapping: entry.mapping,
            source_identity: entry.source_identity.clone(),
            source_digest,
            source_line_number: entry.source_line_number,
            imported_at_unix_millis,
        };
        marker.validate(limits)?;
        Ok(marker)
    }

    /// Validates mapping, source digest, line, and import clock.
    ///
    /// # Errors
    ///
    /// Returns [`ArchiveContractError`] for invalid or mismatched imported state.
    pub fn validate(&self, limits: ArchiveLimits) -> Result<(), ArchiveContractError> {
        validate_schema(self.schema)?;
        self.key.validate(limits)?;
        if self.mapping.target_level() != self.key.identity.level() {
            return Err(ArchiveContractError::IdentityMismatch {
                field: "import_mapping",
            });
        }
        validate_text(
            "source_identity",
            &self.source_identity,
            limits.maximum_text_bytes,
        )?;
        validate_digest("source_digest", &self.source_digest)?;
        require_nonzero("source_line_number", self.source_line_number)?;
        require_nonzero("imported_at_unix_millis", self.imported_at_unix_millis)
    }
}

/// Versioned Ferric-owned text archive format identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveImportFormat {
    FerricMappedTextV1,
}

/// Bounded import batch. Unknown formats or mappings fail during deserialization.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveImportBatch {
    pub schema: SchemaVersion,
    pub format: ArchiveImportFormat,
    pub source_digest: String,
    pub entries: Vec<ArchiveImportEntry>,
}

impl ArchiveImportBatch {
    /// Validates schema, source digest, bounds, and monotonically ordered lines.
    ///
    /// # Errors
    ///
    /// Returns [`ArchiveContractError`] for malformed or ambiguous import data.
    pub fn validate(&self, limits: ArchiveLimits) -> Result<(), ArchiveContractError> {
        validate_schema(self.schema)?;
        validate_digest("source_digest", &self.source_digest)?;
        validate_nonempty_bounded(
            "import_entries",
            self.entries.len(),
            limits.maximum_import_entries,
        )?;
        let mut previous_line = 0;
        let mut target_keys = std::collections::BTreeSet::new();
        for entry in &self.entries {
            entry.validate(limits)?;
            if entry.source_line_number <= previous_line {
                return Err(ArchiveContractError::NonMonotonicImportLine);
            }
            if !target_keys.insert(&entry.target_key) {
                return Err(ArchiveContractError::DuplicateIdentity {
                    field: "import_target_key",
                });
            }
            previous_line = entry.source_line_number;
        }
        Ok(())
    }
}

/// Typed archive contract failure. Backend failures remain adapter-owned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArchiveContractError {
    UnsupportedSchema {
        received: SchemaVersion,
    },
    InvalidField {
        field: &'static str,
    },
    EmptyField {
        field: &'static str,
    },
    ZeroValue {
        field: &'static str,
    },
    LimitExceeded {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },
    IdentityMismatch {
        field: &'static str,
    },
    DuplicateIdentity {
        field: &'static str,
    },
    NonCanonicalOrder {
        field: &'static str,
    },
    InvalidIdentityEventCombination,
    UnknownFilesystemProfile,
    DegradedFilesystemProfile,
    FilesystemProfileMismatch,
    UnsupportedFilesystemProfile,
    FilesystemCommitmentMissing {
        field: &'static str,
    },
    ReconciliationClockBeforeLeaseAcquisition,
    TimeOverflow,
    GenerationOverflow,
    InvalidTimeRange,
    LeaseExpired,
    InvalidMigrationRange,
    InvalidMigrationCheckpoint,
    NonMonotonicImportLine,
    WireEncoding,
    WireDecoding,
}

impl fmt::Display for ArchiveContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "archive contract rejected: {self:?}")
    }
}

impl std::error::Error for ArchiveContractError {}

trait ValidArchiveWire {
    fn validate_wire(&self, limits: ArchiveLimits) -> Result<(), ArchiveContractError>;
}

fn to_wire<T: Serialize + ValidArchiveWire>(
    value: &T,
    limits: ArchiveLimits,
) -> Result<Vec<u8>, ArchiveContractError> {
    value.validate_wire(limits)?;
    let encoded = serde_json::to_vec(value).map_err(|_| ArchiveContractError::WireEncoding)?;
    if encoded.len() > limits.maximum_wire_bytes {
        return Err(ArchiveContractError::LimitExceeded {
            field: "wire_bytes",
            actual: encoded.len(),
            maximum: limits.maximum_wire_bytes,
        });
    }
    Ok(encoded)
}

fn from_wire<T: DeserializeOwned + ValidArchiveWire>(
    bytes: &[u8],
    limits: ArchiveLimits,
) -> Result<T, ArchiveContractError> {
    if bytes.len() > limits.maximum_wire_bytes {
        return Err(ArchiveContractError::LimitExceeded {
            field: "wire_bytes",
            actual: bytes.len(),
            maximum: limits.maximum_wire_bytes,
        });
    }
    let value: T = serde_json::from_slice(bytes).map_err(|_| ArchiveContractError::WireDecoding)?;
    value.validate_wire(limits)?;
    Ok(value)
}

macro_rules! archive_wire {
    ($($type:ty),+ $(,)?) => {
        $(
            impl ValidArchiveWire for $type {
                fn validate_wire(&self, limits: ArchiveLimits) -> Result<(), ArchiveContractError> {
                    self.validate(limits)
                }
            }

            impl $type {
                /// Encodes validated deterministic JSON within the supplied byte limit.
                ///
                /// # Errors
                ///
                /// Returns [`ArchiveContractError`] for invalid data, encoding failure, or size excess.
                pub fn to_wire_bytes(
                    &self,
                    limits: ArchiveLimits,
                ) -> Result<Vec<u8>, ArchiveContractError> {
                    to_wire(self, limits)
                }

                /// Decodes strict JSON and validates all semantic bounds.
                ///
                /// # Errors
                ///
                /// Returns [`ArchiveContractError`] for malformed, oversized, or invalid wire data.
                pub fn from_wire_bytes(
                    bytes: &[u8],
                    limits: ArchiveLimits,
                ) -> Result<Self, ArchiveContractError> {
                    from_wire(bytes, limits)
                }
            }
        )+
    };
}

archive_wire!(
    ArchiveKey,
    ArchiveProvenance,
    ArchiveClaimRequest,
    ArchiveLease,
    ArchiveLeaseRenewalRequest,
    ArchiveClaimOutcome,
    ReconciledArchiveOutput,
    ArchiveCommitRequest,
    ArchiveRecord,
    ArchiveCommitOutcome,
    ArchiveMembership,
    ArchiveReconciliationObservation,
    ArchiveReconciliationDecision,
    ArchiveMigrationPlan,
    ArchiveMigrationState,
    ArchiveImportEntry,
    ArchiveImportMarker,
    ArchiveImportBatch,
);

fn validate_schema(schema: SchemaVersion) -> Result<(), ArchiveContractError> {
    // Minor versions are additive: strict JSON still rejects fields this reader
    // does not know, while logical membership remains identity-rule-versioned.
    if schema.major != ARCHIVE_SCHEMA.major {
        return Err(ArchiveContractError::UnsupportedSchema { received: schema });
    }
    Ok(())
}

fn map_profile_error(error: FilesystemProfileError) -> ArchiveContractError {
    match error {
        FilesystemProfileError::UnknownProfile => ArchiveContractError::UnknownFilesystemProfile,
        FilesystemProfileError::ProfileDataMismatch
        | FilesystemProfileError::ProbeObservationMismatch => {
            ArchiveContractError::FilesystemProfileMismatch
        }
        FilesystemProfileError::PathConfinementUnavailable
        | FilesystemProfileError::ConfinementContract(_) => {
            ArchiveContractError::UnsupportedFilesystemProfile
        }
    }
}

fn require_identity_match<T: PartialEq>(
    field: &'static str,
    expected: &T,
    received: &T,
) -> Result<(), ArchiveContractError> {
    if expected != received {
        return Err(ArchiveContractError::IdentityMismatch { field });
    }
    Ok(())
}

fn require_output_asset(
    output: &ReconciledArchiveOutput,
    asset_id: &AssetId,
) -> Result<(), ArchiveContractError> {
    if !output.asset_ids.contains(asset_id) {
        return Err(ArchiveContractError::IdentityMismatch {
            field: "output.asset_ids",
        });
    }
    Ok(())
}

fn validate_text(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), ArchiveContractError> {
    if value.is_empty() {
        return Err(ArchiveContractError::EmptyField { field });
    }
    if value.len() > maximum {
        return Err(ArchiveContractError::LimitExceeded {
            field,
            actual: value.len(),
            maximum,
        });
    }
    if value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(ArchiveContractError::InvalidField { field });
    }
    Ok(())
}

fn validate_component(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), ArchiveContractError> {
    validate_text(field, value, maximum)?;
    if value.bytes().any(|byte| {
        !byte.is_ascii_lowercase() && !byte.is_ascii_digit() && !matches!(byte, b'_' | b'-' | b'.')
    }) {
        return Err(ArchiveContractError::InvalidField { field });
    }
    Ok(())
}

fn validate_digest(field: &'static str, value: &str) -> Result<(), ArchiveContractError> {
    if value.len() != 64
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
    {
        return Err(ArchiveContractError::InvalidField { field });
    }
    Ok(())
}

fn require_nonzero(field: &'static str, value: u64) -> Result<(), ArchiveContractError> {
    if value == 0 {
        return Err(ArchiveContractError::ZeroValue { field });
    }
    Ok(())
}

fn validate_nonempty_bounded(
    field: &'static str,
    actual: usize,
    maximum: usize,
) -> Result<(), ArchiveContractError> {
    if actual == 0 {
        return Err(ArchiveContractError::EmptyField { field });
    }
    if actual > maximum {
        return Err(ArchiveContractError::LimitExceeded {
            field,
            actual,
            maximum,
        });
    }
    Ok(())
}

fn reject_adjacent_duplicates<T: Ord>(
    field: &'static str,
    values: &[T],
) -> Result<(), ArchiveContractError> {
    for window in values.windows(2) {
        match window[0].cmp(&window[1]) {
            std::cmp::Ordering::Equal => {
                return Err(ArchiveContractError::DuplicateIdentity { field });
            }
            std::cmp::Ordering::Greater => {
                return Err(ArchiveContractError::NonCanonicalOrder { field });
            }
            std::cmp::Ordering::Less => {}
        }
    }
    Ok(())
}

fn usize_from_u64(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: char) -> String {
        std::iter::repeat_n(byte, 64).collect()
    }

    fn key(identity: ArchiveIdentity) -> ArchiveKey {
        ArchiveKey {
            schema: ARCHIVE_SCHEMA,
            namespace: ArchiveNamespace::SourceAsset,
            identity,
            identity_rule_version: 1,
            extractor_id: "ferric_test".to_owned(),
        }
    }

    fn provenance() -> ArchiveProvenance {
        ArchiveProvenance {
            job_id: JobId::new("job_archive").expect("valid fixture ID"),
            transaction_id: TransactionId::new("transaction_archive").expect("valid fixture ID"),
            source_locator_digest: digest('a'),
            request_provenance_digest: digest('b'),
        }
    }

    fn output() -> ReconciledArchiveOutput {
        ReconciledArchiveOutput {
            final_output_identity: "rooted/output.mp4#size=10".to_owned(),
            artifact_size_bytes: 10,
            artifact_digest: digest('c'),
            reconciliation_receipt_digest: digest('d'),
            filesystem_commitment: ArchiveFilesystemCommitment {
                profile: FilesystemProfileContract::windows_11_26200_ntfs_v1(),
                placement: ArchivePlacementProof::SameFilesystem,
                synchronization: ArchiveSynchronizationProof::DataAndParentDirectory,
                confinement: ArchiveConfinementProof::RootHandleVerified,
            },
            asset_ids: vec![AssetId::new("asset_primary").expect("valid fixture ID")],
            derived_output_ids: vec![
                DerivedOutputId::new("output_primary").expect("valid fixture ID"),
            ],
        }
    }

    fn lease() -> ArchiveLease {
        ArchiveLease {
            key: key(ArchiveIdentity::Asset(
                AssetId::new("asset_primary").expect("valid fixture ID"),
            )),
            owner_job_id: JobId::new("job_archive").expect("valid fixture ID"),
            claim_provenance: provenance(),
            token: LeaseToken::new("lease_archive_1").expect("valid fixture token"),
            generation: 1,
            acquired_at_unix_millis: 100,
            expires_at_unix_millis: 200,
        }
    }

    fn record() -> ArchiveRecord {
        ArchiveRecord {
            schema: ARCHIVE_SCHEMA,
            archive_row_id: "archive_row_1".to_owned(),
            key: lease().key,
            success_event: ArchiveSuccessEvent::PerAsset {
                asset_id: AssetId::new("asset_primary").expect("valid fixture ID"),
            },
            output: output(),
            provenance: provenance(),
            claim_lease_token: lease().token,
            claim_lease_generation: 1,
            commit_sequence: 1,
            committed_at_unix_millis: 150,
        }
    }

    #[test]
    fn five_identity_levels_have_distinct_canonical_keys() {
        let identities = [
            key(ArchiveIdentity::Item(
                ItemId::new("item_same").expect("valid fixture ID"),
            )),
            key(ArchiveIdentity::Representation(
                RepresentationId::new("repr_same").expect("valid fixture ID"),
            )),
            key(ArchiveIdentity::Track(
                TrackId::new("track_same").expect("valid fixture ID"),
            )),
            key(ArchiveIdentity::Asset(
                AssetId::new("asset_same").expect("valid fixture ID"),
            )),
            key(ArchiveIdentity::DerivedOutput(
                DerivedOutputId::new("output_same").expect("valid fixture ID"),
            )),
        ];
        let keys = identities
            .iter()
            .map(|value| {
                value
                    .canonical_key(ArchiveLimits::default())
                    .expect("valid canonical key")
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(keys.len(), 5);
    }

    #[test]
    fn claim_wire_is_deterministic_strict_and_semantically_validated() {
        let claim = ArchiveClaimRequest {
            key: lease().key,
            owner_job_id: provenance().job_id.clone(),
            lease_token: LeaseToken::new("lease_claim").expect("valid fixture token"),
            requested_at_unix_millis: 100,
            lease_duration_millis: 50,
            provenance: provenance(),
        };
        let first = claim
            .to_wire_bytes(ArchiveLimits::default())
            .expect("valid claim");
        let second = claim
            .to_wire_bytes(ArchiveLimits::default())
            .expect("valid claim");
        assert_eq!(first, second);
        assert_eq!(
            ArchiveClaimRequest::from_wire_bytes(&first, ArchiveLimits::default()),
            Ok(claim.clone())
        );

        let mut value: serde_json::Value =
            serde_json::from_slice(&first).expect("fixture JSON is valid");
        value["smuggled"] = serde_json::Value::Bool(true);
        let unknown = serde_json::to_vec(&value).expect("fixture JSON encodes");
        assert_eq!(
            ArchiveClaimRequest::from_wire_bytes(&unknown, ArchiveLimits::default()),
            Err(ArchiveContractError::WireDecoding)
        );

        let mut invalid = claim;
        invalid.requested_at_unix_millis = 0;
        let bytes = serde_json::to_vec(&invalid).expect("fixture JSON encodes");
        assert_eq!(
            ArchiveClaimRequest::from_wire_bytes(&bytes, ArchiveLimits::default()),
            Err(ArchiveContractError::ZeroValue {
                field: "requested_at_unix_millis"
            })
        );
    }

    #[test]
    fn commit_rejects_exact_lease_expiry_and_unreconciled_asset() {
        let mut request = ArchiveCommitRequest {
            lease: lease(),
            success_event: ArchiveSuccessEvent::PerAsset {
                asset_id: AssetId::new("asset_primary").expect("valid fixture ID"),
            },
            output: output(),
            provenance: provenance(),
            committed_at_unix_millis: 150,
        };
        assert_eq!(request.validate(ArchiveLimits::default()), Ok(()));
        request.committed_at_unix_millis = 200;
        assert_eq!(
            request.validate(ArchiveLimits::default()),
            Err(ArchiveContractError::LeaseExpired)
        );
        request.committed_at_unix_millis = 150;
        request.success_event = ArchiveSuccessEvent::PerAsset {
            asset_id: AssetId::new("asset_other").expect("valid fixture ID"),
        };
        assert_eq!(
            request.validate(ArchiveLimits::default()),
            Err(ArchiveContractError::IdentityMismatch {
                field: "success_event.asset_id"
            })
        );
    }

    #[test]
    fn commit_requires_exact_immutable_claim_provenance() {
        let request = ArchiveCommitRequest {
            lease: lease(),
            success_event: ArchiveSuccessEvent::PerAsset {
                asset_id: AssetId::new("asset_primary").expect("valid fixture ID"),
            },
            output: output(),
            provenance: provenance(),
            committed_at_unix_millis: 150,
        };
        assert_eq!(request.validate(ArchiveLimits::default()), Ok(()));

        let mut transaction_changed = request.clone();
        transaction_changed.provenance.transaction_id =
            TransactionId::new("transaction_other").expect("valid fixture ID");
        assert_eq!(
            transaction_changed.validate(ArchiveLimits::default()),
            Err(ArchiveContractError::IdentityMismatch {
                field: "lease.claim_provenance"
            })
        );

        let mut source_changed = request.clone();
        source_changed.provenance.source_locator_digest = digest('e');
        assert_eq!(
            source_changed.validate(ArchiveLimits::default()),
            Err(ArchiveContractError::IdentityMismatch {
                field: "lease.claim_provenance"
            })
        );

        let mut request_changed = request;
        request_changed.provenance.request_provenance_digest = digest('f');
        assert_eq!(
            request_changed.validate(ArchiveLimits::default()),
            Err(ArchiveContractError::IdentityMismatch {
                field: "lease.claim_provenance"
            })
        );
    }

    #[test]
    fn success_event_matrix_binds_namespace_level_and_output_ids() {
        let mut request = ArchiveCommitRequest {
            lease: lease(),
            success_event: ArchiveSuccessEvent::PerAsset {
                asset_id: AssetId::new("asset_primary").expect("valid fixture ID"),
            },
            output: output(),
            provenance: provenance(),
            committed_at_unix_millis: 150,
        };
        assert_eq!(request.validate(ArchiveLimits::default()), Ok(()));

        request.success_event = ArchiveSuccessEvent::SuccessfulCollection {
            item_id: ItemId::new("item_unrelated").expect("valid fixture ID"),
        };
        assert_eq!(
            request.validate(ArchiveLimits::default()),
            Err(ArchiveContractError::InvalidIdentityEventCombination)
        );

        request.success_event = ArchiveSuccessEvent::PerAsset {
            asset_id: AssetId::new("asset_unrelated").expect("valid fixture ID"),
        };
        assert_eq!(
            request.validate(ArchiveLimits::default()),
            Err(ArchiveContractError::IdentityMismatch {
                field: "success_event.asset_id"
            })
        );

        request.lease.key.namespace = ArchiveNamespace::PostProcessingAction;
        request.success_event = ArchiveSuccessEvent::PerAsset {
            asset_id: AssetId::new("asset_primary").expect("valid fixture ID"),
        };
        assert_eq!(request.validate(ArchiveLimits::default()), Ok(()));

        request.lease.key.identity = ArchiveIdentity::DerivedOutput(
            DerivedOutputId::new("output_primary").expect("valid fixture ID"),
        );
        request.success_event = ArchiveSuccessEvent::PerDerivedOutput {
            derived_output_id: DerivedOutputId::new("output_primary").expect("valid fixture ID"),
            asset_id: AssetId::new("asset_primary").expect("valid fixture ID"),
        };
        assert_eq!(request.validate(ArchiveLimits::default()), Ok(()));

        request.success_event = ArchiveSuccessEvent::PerDerivedOutput {
            derived_output_id: DerivedOutputId::new("output_unrelated").expect("valid fixture ID"),
            asset_id: AssetId::new("asset_primary").expect("valid fixture ID"),
        };
        assert_eq!(
            request.validate(ArchiveLimits::default()),
            Err(ArchiveContractError::IdentityMismatch {
                field: "success_event.derived_output_id"
            })
        );

        request.lease.key.namespace = ArchiveNamespace::SourceAsset;
        assert_eq!(
            request.validate(ArchiveLimits::default()),
            Err(ArchiveContractError::InvalidIdentityEventCombination)
        );
    }

    #[test]
    fn filesystem_commitment_rejects_unknown_mutated_degraded_and_missing_proof() {
        let mut candidate = output();
        assert_eq!(candidate.validate(ArchiveLimits::default()), Ok(()));

        candidate
            .filesystem_commitment
            .profile
            .capability
            .profile_id = "ff-fs-unknown-v1".to_owned();
        assert_eq!(
            candidate.validate(ArchiveLimits::default()),
            Err(ArchiveContractError::UnknownFilesystemProfile)
        );

        candidate = output();
        candidate
            .filesystem_commitment
            .profile
            .capability
            .filesystem = "FAT32".to_owned();
        assert_eq!(
            candidate.validate(ArchiveLimits::default()),
            Err(ArchiveContractError::FilesystemProfileMismatch)
        );

        candidate = output();
        candidate.filesystem_commitment.profile =
            FilesystemProfileContract::ubuntu_24_04_wsl2_v9fs_v1();
        assert_eq!(
            candidate.validate(ArchiveLimits::default()),
            Err(ArchiveContractError::DegradedFilesystemProfile)
        );

        candidate = output();
        candidate.filesystem_commitment.synchronization = ArchiveSynchronizationProof::DataOnly;
        assert_eq!(
            candidate.validate(ArchiveLimits::default()),
            Err(ArchiveContractError::FilesystemCommitmentMissing {
                field: "data_and_parent_directory_synchronized"
            })
        );
    }

    #[test]
    fn renewal_rotates_token_and_generation_before_expiry() {
        let request = ArchiveLeaseRenewalRequest {
            current_lease: lease(),
            new_token: LeaseToken::new("lease_archive_2").expect("valid fixture token"),
            renewed_at_unix_millis: 199,
            lease_duration_millis: 50,
        };
        let renewed = request
            .renewed_lease(ArchiveLimits::default())
            .expect("live lease renews");
        assert_eq!(renewed.generation, 2);
        assert_eq!(renewed.token.as_str(), "lease_archive_2");
        assert_eq!(renewed.expires_at_unix_millis, 249);

        let stale = ArchiveLeaseRenewalRequest {
            renewed_at_unix_millis: 200,
            ..request
        };
        assert_eq!(
            stale.renewed_lease(ArchiveLimits::default()),
            Err(ArchiveContractError::LeaseExpired)
        );
    }

    #[test]
    fn reconciliation_fails_closed_and_is_idempotent() {
        let stored = record();
        let reconciled = ArchiveReconciliationObservation {
            key: stored.key.clone(),
            now_unix_millis: 300,
            output: ArchiveOutputObservation::Matching {
                output: Box::new(stored.output.clone()),
            },
            row: ArchiveRowObservation::Matching {
                record: Box::new(stored.clone()),
            },
            lease: None,
            staged_output: None,
            recovery_record: None,
        };
        assert_eq!(
            reconciled.decide(ArchiveLimits::default()),
            Ok(ArchiveReconciliationDecision::Reconciled)
        );
        assert_eq!(
            reconciled.decide(ArchiveLimits::default()),
            reconciled.decide(ArchiveLimits::default())
        );

        let missing_row = ArchiveReconciliationObservation {
            row: ArchiveRowObservation::Missing,
            recovery_record: Some(stored.clone()),
            ..reconciled.clone()
        };
        assert_eq!(
            missing_row.decide(ArchiveLimits::default()),
            Ok(ArchiveReconciliationDecision::InsertMissingRow {
                record: Box::new(stored)
            })
        );

        let row_without_output = ArchiveReconciliationObservation {
            output: ArchiveOutputObservation::Missing,
            ..reconciled
        };
        assert_eq!(
            row_without_output.decide(ArchiveLimits::default()),
            Ok(ArchiveReconciliationDecision::FailClosed {
                reason: ArchiveReconciliationFailure::RowWithoutRecoverableOutput
            })
        );
    }

    #[test]
    fn stale_lease_boundary_is_explicit_and_old_state_never_succeeds() {
        let active = ArchiveReconciliationObservation {
            key: lease().key.clone(),
            now_unix_millis: 199,
            output: ArchiveOutputObservation::Missing,
            row: ArchiveRowObservation::Missing,
            lease: Some(lease()),
            staged_output: None,
            recovery_record: None,
        };
        assert_eq!(
            active.decide(ArchiveLimits::default()),
            Ok(ArchiveReconciliationDecision::LeaseStillActive)
        );
        let stale = ArchiveReconciliationObservation {
            now_unix_millis: 200,
            ..active
        };
        assert_eq!(
            stale.decide(ArchiveLimits::default()),
            Ok(ArchiveReconciliationDecision::ReclaimStaleLease)
        );
    }

    #[test]
    fn reconciliation_fails_closed_for_fresh_unexpected_and_wrong_lease_state() {
        let stored = record();
        let fresh_missing_row = ArchiveReconciliationObservation {
            key: stored.key.clone(),
            now_unix_millis: 150,
            output: ArchiveOutputObservation::Matching {
                output: Box::new(stored.output.clone()),
            },
            row: ArchiveRowObservation::Missing,
            lease: Some(lease()),
            staged_output: None,
            recovery_record: Some(stored.clone()),
        };
        assert_eq!(
            fresh_missing_row.decide(ArchiveLimits::default()),
            Ok(ArchiveReconciliationDecision::FailClosed {
                reason: ArchiveReconciliationFailure::FreshLeaseBlocksRecovery
            })
        );

        let unexpected_after_commit = ArchiveReconciliationObservation {
            row: ArchiveRowObservation::Matching {
                record: Box::new(stored.clone()),
            },
            ..fresh_missing_row.clone()
        };
        assert_eq!(
            unexpected_after_commit.decide(ArchiveLimits::default()),
            Ok(ArchiveReconciliationDecision::FailClosed {
                reason: ArchiveReconciliationFailure::UnexpectedLeaseAfterCommit
            })
        );

        let stale_exact = ArchiveReconciliationObservation {
            now_unix_millis: 200,
            ..fresh_missing_row.clone()
        };
        assert_eq!(
            stale_exact.decide(ArchiveLimits::default()),
            Ok(ArchiveReconciliationDecision::InsertMissingRow {
                record: Box::new(stored.clone())
            })
        );

        let mut wrong_token_record = stored.clone();
        wrong_token_record.claim_lease_token =
            LeaseToken::new("lease_wrong").expect("valid fixture token");
        let stale_wrong_token = ArchiveReconciliationObservation {
            now_unix_millis: 200,
            recovery_record: Some(wrong_token_record),
            ..fresh_missing_row.clone()
        };
        assert_eq!(
            stale_wrong_token.decide(ArchiveLimits::default()),
            Ok(ArchiveReconciliationDecision::FailClosed {
                reason: ArchiveReconciliationFailure::RecoveryLeaseMismatch
            })
        );

        let mut wrong_provenance_record = stored;
        wrong_provenance_record.provenance.source_locator_digest = digest('e');
        let stale_wrong_provenance = ArchiveReconciliationObservation {
            now_unix_millis: 200,
            recovery_record: Some(wrong_provenance_record),
            ..fresh_missing_row.clone()
        };
        assert_eq!(
            stale_wrong_provenance.decide(ArchiveLimits::default()),
            Ok(ArchiveReconciliationDecision::FailClosed {
                reason: ArchiveReconciliationFailure::RecoveryLeaseMismatch
            })
        );

        let clock_before_acquisition = ArchiveReconciliationObservation {
            now_unix_millis: 99,
            ..fresh_missing_row
        };
        assert_eq!(
            clock_before_acquisition.decide(ArchiveLimits::default()),
            Err(ArchiveContractError::ReconciliationClockBeforeLeaseAcquisition)
        );
    }

    #[test]
    fn wire_minor_is_not_part_of_logical_membership_identity() {
        let current = lease().key;
        let mut additive_minor = current.clone();
        additive_minor.schema.minor = additive_minor
            .schema
            .minor
            .checked_add(1)
            .expect("fixture minor increments");
        assert_eq!(
            current.canonical_key(ArchiveLimits::default()),
            additive_minor.canonical_key(ArchiveLimits::default())
        );

        let mut new_identity_rules = current.clone();
        new_identity_rules.identity_rule_version += 1;
        assert_ne!(
            current.canonical_key(ArchiveLimits::default()),
            new_identity_rules.canonical_key(ArchiveLimits::default())
        );
    }

    #[test]
    fn migration_and_import_reject_ambiguous_state() {
        let plan = ArchiveMigrationPlan {
            migration_id: "archive_v1_to_v2".to_owned(),
            from_store_version: 1,
            to_store_version: 2,
            maximum_records_per_batch: 128,
        };
        let state = ArchiveMigrationState {
            plan,
            phase: ArchiveMigrationPhase::Prepared,
            migrated_records: 1,
            last_migrated_key: None,
            source_store_digest: digest('e'),
        };
        assert_eq!(
            state.validate(ArchiveLimits::default()),
            Err(ArchiveContractError::InvalidMigrationCheckpoint)
        );

        let entry = ArchiveImportEntry {
            source_line_number: 1,
            source_identity: "legacy/source/value".to_owned(),
            mapping: ArchiveImportMapping::AssetIdentity,
            target_key: key(ArchiveIdentity::Item(
                ItemId::new("item_wrong_level").expect("valid fixture ID"),
            )),
        };
        assert_eq!(
            entry.validate(ArchiveLimits::default()),
            Err(ArchiveContractError::IdentityMismatch {
                field: "import_mapping"
            })
        );

        let mapped_entry = ArchiveImportEntry {
            source_line_number: 2,
            source_identity: "legacy/asset-primary".to_owned(),
            mapping: ArchiveImportMapping::AssetIdentity,
            target_key: key(ArchiveIdentity::Asset(
                AssetId::new("asset_imported").expect("valid fixture ID"),
            )),
        };
        let marker = ArchiveImportMarker::from_entry(
            &mapped_entry,
            digest('f'),
            500,
            ArchiveLimits::default(),
        )
        .expect("known mapping creates marker");
        let membership = ArchiveMembership::Imported {
            marker: Box::new(marker.clone()),
        };
        let wire = membership
            .to_wire_bytes(ArchiveLimits::default())
            .expect("imported membership encodes");
        assert_eq!(
            ArchiveMembership::from_wire_bytes(&wire, ArchiveLimits::default()),
            Ok(membership)
        );
        let claim_outcome = ArchiveClaimOutcome::AlreadyImported {
            marker: Box::new(marker),
        };
        let wire = claim_outcome
            .to_wire_bytes(ArchiveLimits::default())
            .expect("already-imported claim outcome encodes");
        assert_eq!(
            ArchiveClaimOutcome::from_wire_bytes(&wire, ArchiveLimits::default()),
            Ok(claim_outcome)
        );

        let mut duplicate_target = mapped_entry.clone();
        duplicate_target.source_line_number = 3;
        let duplicate_batch = ArchiveImportBatch {
            schema: ARCHIVE_SCHEMA,
            format: ArchiveImportFormat::FerricMappedTextV1,
            source_digest: digest('f'),
            entries: vec![mapped_entry, duplicate_target],
        };
        assert_eq!(
            duplicate_batch.validate(ArchiveLimits::default()),
            Err(ArchiveContractError::DuplicateIdentity {
                field: "import_target_key"
            })
        );

        let unknown_mapping = br#"{
            "source_line_number":1,
            "source_identity":"legacy/value",
            "mapping":"unknown_mapping",
            "target_key":{"schema":{"major":1,"minor":0},"namespace":"source_asset","identity":{"level":"asset","id":"asset_imported"},"identity_rule_version":1,"extractor_id":"ferric_test"}
        }"#;
        assert_eq!(
            ArchiveImportEntry::from_wire_bytes(unknown_mapping, ArchiveLimits::default()),
            Err(ArchiveContractError::WireDecoding)
        );
    }

    #[test]
    fn migration_phases_enforce_cursor_progress_and_allow_empty_completion() {
        let plan = ArchiveMigrationPlan {
            migration_id: "archive_v1_to_v2".to_owned(),
            from_store_version: 1,
            to_store_version: 2,
            maximum_records_per_batch: 128,
        };
        for phase in [
            ArchiveMigrationPhase::Verifying,
            ArchiveMigrationPhase::ReadyToActivate,
            ArchiveMigrationPhase::Activated,
            ArchiveMigrationPhase::RolledBack,
        ] {
            let empty = ArchiveMigrationState {
                plan: plan.clone(),
                phase,
                migrated_records: 0,
                last_migrated_key: None,
                source_store_digest: digest('e'),
            };
            assert_eq!(empty.validate(ArchiveLimits::default()), Ok(()));
        }

        let copying_without_progress = ArchiveMigrationState {
            plan: plan.clone(),
            phase: ArchiveMigrationPhase::Copying,
            migrated_records: 0,
            last_migrated_key: None,
            source_store_digest: digest('e'),
        };
        assert_eq!(
            copying_without_progress.validate(ArchiveLimits::default()),
            Err(ArchiveContractError::InvalidMigrationCheckpoint)
        );

        let progress_without_cursor = ArchiveMigrationState {
            plan,
            phase: ArchiveMigrationPhase::Verifying,
            migrated_records: 1,
            last_migrated_key: None,
            source_store_digest: digest('e'),
        };
        assert_eq!(
            progress_without_cursor.validate(ArchiveLimits::default()),
            Err(ArchiveContractError::InvalidMigrationCheckpoint)
        );
    }

    #[test]
    fn wire_byte_and_collection_bounds_fail_before_acceptance() {
        let limits = ArchiveLimits {
            maximum_wire_bytes: 8,
            ..ArchiveLimits::default()
        };
        assert!(matches!(
            record().to_wire_bytes(limits),
            Err(ArchiveContractError::LimitExceeded {
                field: "wire_bytes",
                ..
            })
        ));

        let mut duplicate = output();
        duplicate.asset_ids.push(duplicate.asset_ids[0].clone());
        assert_eq!(
            duplicate.validate(ArchiveLimits::default()),
            Err(ArchiveContractError::DuplicateIdentity { field: "asset_ids" })
        );

        let mut unsorted = output();
        unsorted.asset_ids = vec![
            AssetId::new("asset_z").expect("valid fixture ID"),
            AssetId::new("asset_a").expect("valid fixture ID"),
        ];
        assert_eq!(
            unsorted.validate(ArchiveLimits::default()),
            Err(ArchiveContractError::NonCanonicalOrder { field: "asset_ids" })
        );
    }
}
