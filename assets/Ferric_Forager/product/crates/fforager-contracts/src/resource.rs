//! Versioned resource-vector and byte-credit contracts.
//!
//! These contracts describe the bounded Phase 0 admission model. They contain
//! data only: runtime ownership and release behavior is implemented by the
//! consuming core model.

use crate::SchemaVersion;
use serde::{Deserialize, Serialize};

pub const RESOURCE_VECTOR_SCHEMA_ID: &str = "ff.resource-vector@1";
pub const BYTE_CREDIT_SCHEMA_ID: &str = "ff.byte-credit@1";
pub const RESOURCE_CONTRACT_VERSION: SchemaVersion = SchemaVersion { major: 1, minor: 0 };
pub const BYTE_CREDIT_CONTRACT_VERSION: SchemaVersion = SchemaVersion { major: 1, minor: 0 };

/// The immutable 13-dimensional claim submitted before executable work starts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceVector {
    pub metadata_requests: u32,
    pub media_requests: u32,
    pub memory_bytes: u64,
    pub disk_read_bytes_in_flight: u64,
    pub disk_write_bytes_in_flight: u64,
    pub open_handles: u32,
    pub cpu_light_slots: u32,
    pub cpu_heavy_slots: u32,
    pub javascript_workers: u32,
    pub ffmpeg_processes: u32,
    pub ffmpeg_cpu_threads: u32,
    pub archive_writer_slots: u32,
    pub sink_bytes: u64,
}

impl ResourceVector {
    /// Checked component-wise addition. No dimension may wrap.
    #[must_use]
    pub fn checked_add(self, rhs: Self) -> Option<Self> {
        Some(Self {
            metadata_requests: self.metadata_requests.checked_add(rhs.metadata_requests)?,
            media_requests: self.media_requests.checked_add(rhs.media_requests)?,
            memory_bytes: self.memory_bytes.checked_add(rhs.memory_bytes)?,
            disk_read_bytes_in_flight: self
                .disk_read_bytes_in_flight
                .checked_add(rhs.disk_read_bytes_in_flight)?,
            disk_write_bytes_in_flight: self
                .disk_write_bytes_in_flight
                .checked_add(rhs.disk_write_bytes_in_flight)?,
            open_handles: self.open_handles.checked_add(rhs.open_handles)?,
            cpu_light_slots: self.cpu_light_slots.checked_add(rhs.cpu_light_slots)?,
            cpu_heavy_slots: self.cpu_heavy_slots.checked_add(rhs.cpu_heavy_slots)?,
            javascript_workers: self
                .javascript_workers
                .checked_add(rhs.javascript_workers)?,
            ffmpeg_processes: self.ffmpeg_processes.checked_add(rhs.ffmpeg_processes)?,
            ffmpeg_cpu_threads: self
                .ffmpeg_cpu_threads
                .checked_add(rhs.ffmpeg_cpu_threads)?,
            archive_writer_slots: self
                .archive_writer_slots
                .checked_add(rhs.archive_writer_slots)?,
            sink_bytes: self.sink_bytes.checked_add(rhs.sink_bytes)?,
        })
    }

    /// Checked component-wise subtraction. No dimension may underflow.
    #[must_use]
    pub fn checked_sub(self, rhs: Self) -> Option<Self> {
        Some(Self {
            metadata_requests: self.metadata_requests.checked_sub(rhs.metadata_requests)?,
            media_requests: self.media_requests.checked_sub(rhs.media_requests)?,
            memory_bytes: self.memory_bytes.checked_sub(rhs.memory_bytes)?,
            disk_read_bytes_in_flight: self
                .disk_read_bytes_in_flight
                .checked_sub(rhs.disk_read_bytes_in_flight)?,
            disk_write_bytes_in_flight: self
                .disk_write_bytes_in_flight
                .checked_sub(rhs.disk_write_bytes_in_flight)?,
            open_handles: self.open_handles.checked_sub(rhs.open_handles)?,
            cpu_light_slots: self.cpu_light_slots.checked_sub(rhs.cpu_light_slots)?,
            cpu_heavy_slots: self.cpu_heavy_slots.checked_sub(rhs.cpu_heavy_slots)?,
            javascript_workers: self
                .javascript_workers
                .checked_sub(rhs.javascript_workers)?,
            ffmpeg_processes: self.ffmpeg_processes.checked_sub(rhs.ffmpeg_processes)?,
            ffmpeg_cpu_threads: self
                .ffmpeg_cpu_threads
                .checked_sub(rhs.ffmpeg_cpu_threads)?,
            archive_writer_slots: self
                .archive_writer_slots
                .checked_sub(rhs.archive_writer_slots)?,
            sink_bytes: self.sink_bytes.checked_sub(rhs.sink_bytes)?,
        })
    }

    #[must_use]
    pub fn fits_within(self, capacity: Self) -> bool {
        self.metadata_requests <= capacity.metadata_requests
            && self.media_requests <= capacity.media_requests
            && self.memory_bytes <= capacity.memory_bytes
            && self.disk_read_bytes_in_flight <= capacity.disk_read_bytes_in_flight
            && self.disk_write_bytes_in_flight <= capacity.disk_write_bytes_in_flight
            && self.open_handles <= capacity.open_handles
            && self.cpu_light_slots <= capacity.cpu_light_slots
            && self.cpu_heavy_slots <= capacity.cpu_heavy_slots
            && self.javascript_workers <= capacity.javascript_workers
            && self.ffmpeg_processes <= capacity.ffmpeg_processes
            && self.ffmpeg_cpu_threads <= capacity.ffmpeg_cpu_threads
            && self.archive_writer_slots <= capacity.archive_writer_slots
            && self.sink_bytes <= capacity.sink_bytes
    }

    /// Sum of variable-size dimensions used to bound waiter declarations.
    #[must_use]
    pub fn declared_variable_bytes(self) -> Option<u64> {
        self.memory_bytes
            .checked_add(self.disk_read_bytes_in_flight)?
            .checked_add(self.disk_write_bytes_in_flight)?
            .checked_add(self.sink_bytes)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceGrantRule {
    CompleteCheckedVectorOrNothing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceFairnessPolicy {
    StrictFifoHeadReservationV1WithDeterministicPerOwnerActiveCeiling,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseOwnershipPolicy {
    NonCloneOwnedDropOrExplicitRelease,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueCancellationPolicy {
    RemoveExactWaiterThenDispatchHead,
}

/// Complete v1 resource-admission profile and its declared item/byte bounds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceContractV1 {
    pub schema_id: String,
    pub version: SchemaVersion,
    pub capacity: ResourceVector,
    pub max_active_grants: u64,
    pub max_active_per_owner: u64,
    pub max_waiter_items: u64,
    pub max_waiter_declared_variable_bytes: u64,
    pub grant_rule: ResourceGrantRule,
    pub fairness_policy: ResourceFairnessPolicy,
    pub ownership_policy: LeaseOwnershipPolicy,
    pub cancellation_policy: QueueCancellationPolicy,
}

impl ResourceContractV1 {
    #[must_use]
    pub fn new(
        capacity: ResourceVector,
        max_active_grants: u64,
        max_active_per_owner: u64,
        max_waiter_items: u64,
        max_waiter_declared_variable_bytes: u64,
    ) -> Self {
        Self {
            schema_id: RESOURCE_VECTOR_SCHEMA_ID.to_owned(),
            version: RESOURCE_CONTRACT_VERSION,
            capacity,
            max_active_grants,
            max_active_per_owner,
            max_waiter_items,
            max_waiter_declared_variable_bytes,
            grant_rule: ResourceGrantRule::CompleteCheckedVectorOrNothing,
            fairness_policy:
                ResourceFairnessPolicy::StrictFifoHeadReservationV1WithDeterministicPerOwnerActiveCeiling,
            ownership_policy: LeaseOwnershipPolicy::NonCloneOwnedDropOrExplicitRelease,
            cancellation_policy: QueueCancellationPolicy::RemoveExactWaiterThenDispatchHead,
        }
    }

    /// Validate the exact closed v1 profile before it enters the core model.
    ///
    /// # Errors
    ///
    /// Returns a typed schema or bound error. Zero waiter bounds are legal and
    /// mean that queuing is disabled.
    pub fn validate(&self) -> Result<(), ResourceContractError> {
        if self.schema_id != RESOURCE_VECTOR_SCHEMA_ID {
            return Err(ResourceContractError::SchemaIdMismatch);
        }
        if self.version != RESOURCE_CONTRACT_VERSION {
            return Err(ResourceContractError::UnsupportedVersion(self.version));
        }
        if self.max_active_grants == 0 {
            return Err(ResourceContractError::ZeroActiveGrantLimit);
        }
        if self.max_active_per_owner == 0 || self.max_active_per_owner > self.max_active_grants {
            return Err(ResourceContractError::InvalidOwnerCeiling);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceContractError {
    SchemaIdMismatch,
    UnsupportedVersion(SchemaVersion),
    ZeroActiveGrantLimit,
    InvalidOwnerCeiling,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ByteCreditStage {
    HttpReceive,
    Decompression,
    DecryptOrPackInput,
    DecryptOrPackOutput,
    Reorder,
    Writer,
    Journal,
    FfmpegPipe,
    SpillAndSink,
}

pub const BYTE_CREDIT_STAGES_V1: [ByteCreditStage; 9] = [
    ByteCreditStage::HttpReceive,
    ByteCreditStage::Decompression,
    ByteCreditStage::DecryptOrPackInput,
    ByteCreditStage::DecryptOrPackOutput,
    ByteCreditStage::Reorder,
    ByteCreditStage::Writer,
    ByteCreditStage::Journal,
    ByteCreditStage::FfmpegPipe,
    ByteCreditStage::SpillAndSink,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ByteCreditSaturationPolicy {
    Backpressured,
    SpillRequired,
}

/// One closed stage-local item/byte bound and its saturation outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ByteCreditStagePolicy {
    pub stage: ByteCreditStage,
    pub max_claim_items: u64,
    pub max_bytes: u64,
    pub saturation_policy: ByteCreditSaturationPolicy,
}

impl ByteCreditStagePolicy {
    #[must_use]
    pub const fn new(
        stage: ByteCreditStage,
        max_claim_items: u64,
        max_bytes: u64,
        saturation_policy: ByteCreditSaturationPolicy,
    ) -> Self {
        Self {
            stage,
            max_claim_items,
            max_bytes,
            saturation_policy,
        }
    }
}

#[must_use]
pub const fn byte_credit_stage_policies_v1(
    max_claim_items: u64,
    max_bytes: u64,
) -> [ByteCreditStagePolicy; 9] {
    [
        ByteCreditStagePolicy::new(
            ByteCreditStage::HttpReceive,
            max_claim_items,
            max_bytes,
            ByteCreditSaturationPolicy::Backpressured,
        ),
        ByteCreditStagePolicy::new(
            ByteCreditStage::Decompression,
            max_claim_items,
            max_bytes,
            ByteCreditSaturationPolicy::Backpressured,
        ),
        ByteCreditStagePolicy::new(
            ByteCreditStage::DecryptOrPackInput,
            max_claim_items,
            max_bytes,
            ByteCreditSaturationPolicy::Backpressured,
        ),
        ByteCreditStagePolicy::new(
            ByteCreditStage::DecryptOrPackOutput,
            max_claim_items,
            max_bytes,
            ByteCreditSaturationPolicy::Backpressured,
        ),
        ByteCreditStagePolicy::new(
            ByteCreditStage::Reorder,
            max_claim_items,
            max_bytes,
            ByteCreditSaturationPolicy::Backpressured,
        ),
        ByteCreditStagePolicy::new(
            ByteCreditStage::Writer,
            max_claim_items,
            max_bytes,
            ByteCreditSaturationPolicy::Backpressured,
        ),
        ByteCreditStagePolicy::new(
            ByteCreditStage::Journal,
            max_claim_items,
            max_bytes,
            ByteCreditSaturationPolicy::Backpressured,
        ),
        ByteCreditStagePolicy::new(
            ByteCreditStage::FfmpegPipe,
            max_claim_items,
            max_bytes,
            ByteCreditSaturationPolicy::Backpressured,
        ),
        ByteCreditStagePolicy::new(
            ByteCreditStage::SpillAndSink,
            max_claim_items,
            max_bytes,
            ByteCreditSaturationPolicy::SpillRequired,
        ),
    ]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ByteReservationPolicy {
    SimultaneouslyLiveInputAndOutputBeforeAllocation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ByteTransferPolicy {
    UnconsumedClaimSingleOwner,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequiredByteLossPolicy {
    LosslessBackpressure,
}

/// Complete v1 byte-credit profile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ByteCreditContractV1 {
    pub schema_id: String,
    pub version: SchemaVersion,
    pub capacity_bytes: u64,
    pub max_claim_items: u64,
    pub max_owner_bytes: u64,
    pub max_owner_claim_items: u64,
    pub stages: [ByteCreditStage; 9],
    pub stage_policies: [ByteCreditStagePolicy; 9],
    pub reservation_policy: ByteReservationPolicy,
    pub transfer_policy: ByteTransferPolicy,
    pub ownership_policy: LeaseOwnershipPolicy,
    pub required_byte_loss_policy: RequiredByteLossPolicy,
}

impl ByteCreditContractV1 {
    #[must_use]
    pub fn new(capacity_bytes: u64, max_claim_items: u64) -> Self {
        Self {
            schema_id: BYTE_CREDIT_SCHEMA_ID.to_owned(),
            version: BYTE_CREDIT_CONTRACT_VERSION,
            capacity_bytes,
            max_claim_items,
            max_owner_bytes: capacity_bytes,
            max_owner_claim_items: max_claim_items,
            stages: BYTE_CREDIT_STAGES_V1,
            stage_policies: byte_credit_stage_policies_v1(max_claim_items, capacity_bytes),
            reservation_policy:
                ByteReservationPolicy::SimultaneouslyLiveInputAndOutputBeforeAllocation,
            transfer_policy: ByteTransferPolicy::UnconsumedClaimSingleOwner,
            ownership_policy: LeaseOwnershipPolicy::NonCloneOwnedDropOrExplicitRelease,
            required_byte_loss_policy: RequiredByteLossPolicy::LosslessBackpressure,
        }
    }

    /// Validate the exact closed v1 profile before it enters the core model.
    ///
    /// # Errors
    ///
    /// Returns a typed schema, stage, or item-bound error.
    pub fn validate(&self) -> Result<(), ByteCreditContractError> {
        if self.schema_id != BYTE_CREDIT_SCHEMA_ID {
            return Err(ByteCreditContractError::SchemaIdMismatch);
        }
        if self.version != BYTE_CREDIT_CONTRACT_VERSION {
            return Err(ByteCreditContractError::UnsupportedVersion(self.version));
        }
        if self.max_claim_items == 0 {
            return Err(ByteCreditContractError::ZeroClaimItemLimit);
        }
        if self.capacity_bytes == 0 {
            return Err(ByteCreditContractError::ZeroByteCapacity);
        }
        if self.max_owner_claim_items == 0
            || self.max_owner_claim_items > self.max_claim_items
            || self.max_owner_bytes == 0
            || self.max_owner_bytes > self.capacity_bytes
        {
            return Err(ByteCreditContractError::InvalidOwnerLimit);
        }
        if self.stages != BYTE_CREDIT_STAGES_V1 {
            return Err(ByteCreditContractError::StageInventoryMismatch);
        }
        for (policy, expected_stage) in self.stage_policies.iter().zip(BYTE_CREDIT_STAGES_V1) {
            if policy.stage != expected_stage {
                return Err(ByteCreditContractError::StagePolicyInventoryMismatch);
            }
            let expected_saturation = if expected_stage == ByteCreditStage::SpillAndSink {
                ByteCreditSaturationPolicy::SpillRequired
            } else {
                ByteCreditSaturationPolicy::Backpressured
            };
            if policy.saturation_policy != expected_saturation {
                return Err(ByteCreditContractError::StageSaturationPolicyMismatch(
                    expected_stage,
                ));
            }
            if policy.max_claim_items == 0
                || policy.max_claim_items > self.max_claim_items
                || policy.max_bytes == 0
                || policy.max_bytes > self.capacity_bytes
            {
                return Err(ByteCreditContractError::InvalidStageLimit(policy.stage));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ByteCreditContractError {
    SchemaIdMismatch,
    UnsupportedVersion(SchemaVersion),
    ZeroByteCapacity,
    ZeroClaimItemLimit,
    InvalidOwnerLimit,
    StageInventoryMismatch,
    StagePolicyInventoryMismatch,
    StageSaturationPolicyMismatch(ByteCreditStage),
    InvalidStageLimit(ByteCreditStage),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ByteCreditComponent {
    Single,
    Input,
    Output,
}

/// Atomic reservation for transform bytes that coexist before either allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoupledByteReservation {
    pub input_stage: ByteCreditStage,
    pub input_bytes: u64,
    pub output_stage: ByteCreditStage,
    pub output_bytes: u64,
}

impl CoupledByteReservation {
    /// Return the complete reservation or fail closed on an empty component or overflow.
    ///
    /// # Errors
    ///
    /// Returns a typed error instead of wrapping or admitting a componentless claim.
    pub fn total_bytes(self) -> Result<u64, CoupledByteReservationError> {
        if self.input_bytes == 0 {
            return Err(CoupledByteReservationError::ZeroInput);
        }
        if self.output_bytes == 0 {
            return Err(CoupledByteReservationError::ZeroOutput);
        }
        let total = self
            .input_bytes
            .checked_add(self.output_bytes)
            .ok_or(CoupledByteReservationError::ArithmeticOverflow)?;
        Ok(total)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoupledByteReservationError {
    ZeroInput,
    ZeroOutput,
    ArithmeticOverflow,
}

/// The three monotonic positions used by byte-credit durability accounting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ByteCreditPosition {
    pub received: u64,
    pub validated_written_contiguous: u64,
    pub durable_contiguous: u64,
}

impl ByteCreditPosition {
    /// Verify durable <= validated-written <= received.
    ///
    /// # Errors
    ///
    /// Returns a typed ordering error for an optimistic position.
    pub fn validate(self) -> Result<(), ByteCreditPositionError> {
        if self.durable_contiguous > self.validated_written_contiguous {
            return Err(ByteCreditPositionError::DurableAheadOfValidatedWritten);
        }
        if self.validated_written_contiguous > self.received {
            return Err(ByteCreditPositionError::ValidatedWrittenAheadOfReceived);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ByteCreditPositionError {
    DurableAheadOfValidatedWritten,
    ValidatedWrittenAheadOfReceived,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_contract_is_strict_versioned_and_has_thirteen_dimensions() {
        let contract = ResourceContractV1::new(ResourceVector::default(), 4, 2, 8, 1024);
        assert!(contract.validate().is_ok());
        let value = serde_json::to_value(&contract).expect("contract must serialize");
        let capacity = value
            .get("capacity")
            .and_then(serde_json::Value::as_object)
            .expect("capacity must be an object");
        assert_eq!(capacity.len(), 13);

        let mut wrong = contract.clone();
        wrong.version = SchemaVersion { major: 2, minor: 0 };
        assert_eq!(
            wrong.validate(),
            Err(ResourceContractError::UnsupportedVersion(wrong.version))
        );
    }

    #[test]
    fn resource_contract_rejects_unknown_fields_and_invalid_owner_ceiling() {
        let json = r#"{"schema_id":"ff.resource-vector@1","version":{"major":1,"minor":0},"capacity":{"metadata_requests":0,"media_requests":0,"memory_bytes":0,"disk_read_bytes_in_flight":0,"disk_write_bytes_in_flight":0,"open_handles":0,"cpu_light_slots":0,"cpu_heavy_slots":0,"javascript_workers":0,"ffmpeg_processes":0,"ffmpeg_cpu_threads":0,"archive_writer_slots":0,"sink_bytes":0,"unexpected":1},"max_active_grants":1,"max_active_per_owner":1,"max_waiter_items":0,"max_waiter_declared_variable_bytes":0,"grant_rule":"complete_checked_vector_or_nothing","fairness_policy":"strict_fifo_head_reservation_v1_with_deterministic_per_owner_active_ceiling","ownership_policy":"non_clone_owned_drop_or_explicit_release","cancellation_policy":"remove_exact_waiter_then_dispatch_head"}"#;
        assert!(serde_json::from_str::<ResourceContractV1>(json).is_err());

        let contract = ResourceContractV1::new(ResourceVector::default(), 1, 2, 0, 0);
        assert_eq!(
            contract.validate(),
            Err(ResourceContractError::InvalidOwnerCeiling)
        );
    }

    #[test]
    fn byte_contract_freezes_stages_and_coupled_overflow_fails_closed() {
        let contract = ByteCreditContractV1::new(4096, 4);
        assert!(contract.validate().is_ok());
        assert_eq!(contract.stages, BYTE_CREDIT_STAGES_V1);
        assert_eq!(contract.stage_policies.len(), BYTE_CREDIT_STAGES_V1.len());
        for (policy, stage) in contract.stage_policies.iter().zip(BYTE_CREDIT_STAGES_V1) {
            assert_eq!(policy.stage, stage);
        }
        assert_eq!(
            CoupledByteReservation {
                input_stage: ByteCreditStage::DecryptOrPackInput,
                input_bytes: u64::MAX,
                output_stage: ByteCreditStage::DecryptOrPackOutput,
                output_bytes: 1,
            }
            .total_bytes(),
            Err(CoupledByteReservationError::ArithmeticOverflow)
        );
        assert_eq!(
            CoupledByteReservation {
                input_stage: ByteCreditStage::DecryptOrPackInput,
                input_bytes: 0,
                output_stage: ByteCreditStage::DecryptOrPackOutput,
                output_bytes: 0,
            }
            .total_bytes(),
            Err(CoupledByteReservationError::ZeroInput)
        );

        let mut invalid_stage_policy = contract;
        invalid_stage_policy.stage_policies[0].stage = ByteCreditStage::Writer;
        assert_eq!(
            invalid_stage_policy.validate(),
            Err(ByteCreditContractError::StagePolicyInventoryMismatch)
        );

        let mut invalid_saturation_policy = ByteCreditContractV1::new(4096, 4);
        invalid_saturation_policy.stage_policies[0].saturation_policy =
            ByteCreditSaturationPolicy::SpillRequired;
        assert_eq!(
            invalid_saturation_policy.validate(),
            Err(ByteCreditContractError::StageSaturationPolicyMismatch(
                ByteCreditStage::HttpReceive
            ))
        );
    }

    #[test]
    fn credit_positions_use_exact_monotonic_boundaries() {
        assert!(
            ByteCreditPosition {
                received: 3,
                validated_written_contiguous: 2,
                durable_contiguous: 1,
            }
            .validate()
            .is_ok()
        );
        assert_eq!(
            ByteCreditPosition {
                received: 1,
                validated_written_contiguous: 1,
                durable_contiguous: 2,
            }
            .validate(),
            Err(ByteCreditPositionError::DurableAheadOfValidatedWritten)
        );
    }
}
