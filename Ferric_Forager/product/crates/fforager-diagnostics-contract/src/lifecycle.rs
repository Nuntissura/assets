use crate::{
    ArtifactId, BootSessionId, BuildId, CapabilityId, ContractError, LimitKind, MAX_HEALTH_LOOPS,
    ProcessStartId, ProducerInstanceId, ProtocolVersion, ReasonCode, SchemaIdentity, SequenceFault,
    SequenceIdentity, WorkId,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Independent watcher lifecycle. `Ready` is local; `Serving` requires a producer canary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum WatcherState {
    Starting,
    Ready,
    Serving,
    Degraded { reason: ReasonCode },
    Stale { reason: ReasonCode },
    Draining,
    Stopped,
}

/// Evidence required before local watcher readiness can be claimed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadyEvidence {
    pub protocol_negotiated: CheckStatus,
    pub storage_self_test: CheckStatus,
    pub retention_policy: CheckStatus,
    pub watcher_canary: CheckStatus,
}

/// Outcome of one mandatory local readiness check.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Passed,
    Failed,
}

impl ReadyEvidence {
    /// Proves all local readiness conditions passed.
    ///
    /// # Errors
    /// Returns [`ContractError`] if any mandatory readiness check failed.
    pub fn validate(self) -> Result<(), ContractError> {
        if self.protocol_negotiated == CheckStatus::Passed
            && self.storage_self_test == CheckStatus::Passed
            && self.retention_policy == CheckStatus::Passed
            && self.watcher_canary == CheckStatus::Passed
        {
            Ok(())
        } else {
            Err(ContractError::MissingReadyEvidence)
        }
    }
}

/// Input to the pure watcher lifecycle transition function.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LifecycleInput {
    LocalReady(ReadyEvidence),
    ProducerCanary,
    Degrade(ReasonCode),
    MarkStale(ReasonCode),
    RecoverReady(ReadyEvidence),
    RecoverServing,
    BeginDrain,
    Stop,
}

/// Versioned lifecycle snapshot with monotonic generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleSnapshot {
    pub state: WatcherState,
    pub generation: u64,
}

impl LifecycleSnapshot {
    #[must_use]
    pub const fn starting() -> Self {
        Self {
            state: WatcherState::Starting,
            generation: 0,
        }
    }

    /// Applies one legal lifecycle input and advances the monotonic generation.
    ///
    /// # Errors
    /// Returns [`ContractError`] for missing readiness evidence, invalid transitions, or overflow.
    pub fn transition(&self, input: LifecycleInput) -> Result<Self, ContractError> {
        let state = match (&self.state, input) {
            (WatcherState::Starting, LifecycleInput::LocalReady(evidence))
            | (WatcherState::Degraded { .. }, LifecycleInput::RecoverReady(evidence)) => {
                evidence.validate()?;
                WatcherState::Ready
            }
            (WatcherState::Ready, LifecycleInput::ProducerCanary)
            | (
                WatcherState::Degraded { .. } | WatcherState::Stale { .. },
                LifecycleInput::RecoverServing,
            ) => WatcherState::Serving,
            (
                WatcherState::Starting
                | WatcherState::Ready
                | WatcherState::Serving
                | WatcherState::Stale { .. },
                LifecycleInput::Degrade(reason),
            ) => WatcherState::Degraded { reason },
            (
                WatcherState::Serving | WatcherState::Degraded { .. },
                LifecycleInput::MarkStale(reason),
            ) => WatcherState::Stale { reason },
            (
                WatcherState::Ready
                | WatcherState::Serving
                | WatcherState::Degraded { .. }
                | WatcherState::Stale { .. },
                LifecycleInput::BeginDrain,
            ) => WatcherState::Draining,
            (WatcherState::Draining, LifecycleInput::Stop) => WatcherState::Stopped,
            _ => return Err(ContractError::InvalidTransition),
        };
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(ContractError::CounterInvariant)?;
        Ok(Self { state, generation })
    }
}

/// State of one watcher loop, including the dependency or deadline needed to judge staleness.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum LoopState {
    Idle {
        expected_next_monotonic_ns: Option<u64>,
        dependency: Option<ReasonCode>,
    },
    Working {
        work_id: WorkId,
        deadline_monotonic_ns: u64,
    },
    Blocked {
        reason: ReasonCode,
        deadline_monotonic_ns: u64,
    },
    Failed {
        reason: ReasonCode,
    },
}

/// Loop health generations; completion advances only behind admitted work.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "LoopHealthWire")]
pub struct LoopHealth {
    pub capability_id: CapabilityId,
    pub state: LoopState,
    pub admitted_generation: u64,
    pub completion_generation: u64,
    pub responsiveness_generation: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoopHealthWire {
    capability_id: CapabilityId,
    state: LoopState,
    admitted_generation: u64,
    completion_generation: u64,
    responsiveness_generation: u64,
}

impl LoopHealth {
    #[must_use]
    pub fn new(capability_id: CapabilityId, state: LoopState) -> Self {
        Self {
            capability_id,
            state,
            admitted_generation: 0,
            completion_generation: 0,
            responsiveness_generation: 0,
        }
    }

    /// Advances the admitted-work generation.
    ///
    /// # Errors
    /// Returns [`ContractError`] on generation overflow.
    pub fn admit_work(&mut self) -> Result<u64, ContractError> {
        self.admitted_generation = self
            .admitted_generation
            .checked_add(1)
            .ok_or(ContractError::CounterInvariant)?;
        Ok(self.admitted_generation)
    }

    /// Completes exactly the next admitted work generation.
    ///
    /// # Errors
    /// Returns [`ContractError`] for unadmitted, duplicate, or reordered completion.
    pub fn complete_admitted(&mut self, generation: u64) -> Result<(), ContractError> {
        if generation != self.completion_generation.saturating_add(1)
            || generation > self.admitted_generation
        {
            return Err(ContractError::CounterInvariant);
        }
        self.completion_generation = generation;
        Ok(())
    }

    /// Advances the independent responsiveness-canary generation.
    ///
    /// # Errors
    /// Returns [`ContractError`] on generation overflow.
    pub fn record_responsiveness(&mut self) -> Result<(), ContractError> {
        self.responsiveness_generation = self
            .responsiveness_generation
            .checked_add(1)
            .ok_or(ContractError::CounterInvariant)?;
        Ok(())
    }

    fn validate(&self) -> Result<(), ContractError> {
        if self.completion_generation > self.admitted_generation {
            return Err(ContractError::CounterInvariant);
        }
        Ok(())
    }
}

impl TryFrom<LoopHealthWire> for LoopHealth {
    type Error = ContractError;

    fn try_from(value: LoopHealthWire) -> Result<Self, Self::Error> {
        let health = Self {
            capability_id: value.capability_id,
            state: value.state,
            admitted_generation: value.admitted_generation,
            completion_generation: value.completion_generation,
            responsiveness_generation: value.responsiveness_generation,
        };
        health.validate()?;
        Ok(health)
    }
}

/// Bounded queue and loss counters reported by watcher health.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "HealthCountersWire")]
pub struct HealthCounters {
    pub queue_depth: u32,
    pub queue_high_water: u32,
    pub queue_capacity: u32,
    pub dropped_lossy: u64,
    pub discarded_unknown_optional: u64,
    pub residual_gap_count: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HealthCountersWire {
    queue_depth: u32,
    queue_high_water: u32,
    queue_capacity: u32,
    dropped_lossy: u64,
    discarded_unknown_optional: u64,
    residual_gap_count: u64,
}

impl HealthCounters {
    /// Creates queue and loss counters while enforcing depth/high-water/capacity order.
    ///
    /// # Errors
    /// Returns [`ContractError`] for zero capacity or inconsistent queue counters.
    pub fn new(
        queue_depth: u32,
        queue_high_water: u32,
        queue_capacity: u32,
        dropped_lossy: u64,
        discarded_unknown_optional: u64,
        residual_gap_count: u64,
    ) -> Result<Self, ContractError> {
        if queue_capacity == 0
            || queue_depth > queue_high_water
            || queue_high_water > queue_capacity
        {
            return Err(ContractError::CounterInvariant);
        }
        Ok(Self {
            queue_depth,
            queue_high_water,
            queue_capacity,
            dropped_lossy,
            discarded_unknown_optional,
            residual_gap_count,
        })
    }
}

impl TryFrom<HealthCountersWire> for HealthCounters {
    type Error = ContractError;

    fn try_from(value: HealthCountersWire) -> Result<Self, Self::Error> {
        Self::new(
            value.queue_depth,
            value.queue_high_water,
            value.queue_capacity,
            value.dropped_lossy,
            value.discarded_unknown_optional,
            value.residual_gap_count,
        )
    }
}

/// Complete bounded health record used to distinguish existence from responsiveness.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "HealthSnapshotWire")]
pub struct HealthSnapshot {
    pub producer_instance: ProducerInstanceId,
    pub boot_session: BootSessionId,
    pub process_start_id: ProcessStartId,
    pub ferric_artifact_id: ArtifactId,
    pub watcher_artifact_id: ArtifactId,
    pub build_id: BuildId,
    pub protocol: ProtocolVersion,
    pub schema: SchemaIdentity,
    pub observed_monotonic_ns: u64,
    pub last_admitted: Option<SequenceIdentity>,
    pub last_durable: Option<SequenceIdentity>,
    pub counters: HealthCounters,
    pub lifecycle: LifecycleSnapshot,
    pub loops: Vec<LoopHealth>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HealthSnapshotWire {
    producer_instance: ProducerInstanceId,
    boot_session: BootSessionId,
    process_start_id: ProcessStartId,
    ferric_artifact_id: ArtifactId,
    watcher_artifact_id: ArtifactId,
    build_id: BuildId,
    protocol: ProtocolVersion,
    schema: SchemaIdentity,
    observed_monotonic_ns: u64,
    last_admitted: Option<SequenceIdentity>,
    last_durable: Option<SequenceIdentity>,
    counters: HealthCounters,
    lifecycle: LifecycleSnapshot,
    loops: Vec<LoopHealth>,
}

impl HealthSnapshot {
    /// Revalidates sequence, counter, capability, and collection bounds.
    ///
    /// # Errors
    /// Returns [`ContractError`] for inconsistent or oversized health state.
    pub fn validate(&self) -> Result<(), ContractError> {
        self.protocol.validate()?;
        self.schema.validate()?;
        if self.loops.len() > MAX_HEALTH_LOOPS {
            return Err(ContractError::LimitExceeded {
                kind: LimitKind::Loops,
                limit: MAX_HEALTH_LOOPS,
                actual: self.loops.len(),
            });
        }
        let mut capabilities = HashSet::with_capacity(self.loops.len());
        for loop_health in &self.loops {
            loop_health.validate()?;
            if !capabilities.insert(loop_health.capability_id.as_str()) {
                return Err(ContractError::DuplicateField);
            }
        }
        if let Some(admitted) = &self.last_admitted
            && (admitted.key.producer_instance != self.producer_instance
                || admitted.key.boot_session != self.boot_session)
        {
            return Err(ContractError::Sequence {
                fault: SequenceFault::IdentityChanged,
            });
        }
        if let Some(durable) = &self.last_durable {
            let admitted = self.last_admitted.as_ref().ok_or(ContractError::Sequence {
                fault: SequenceFault::DurableAheadOfAdmitted,
            })?;
            if durable.key != admitted.key || durable.sequence > admitted.sequence {
                return Err(ContractError::Sequence {
                    fault: SequenceFault::DurableAheadOfAdmitted,
                });
            }
        }
        Ok(())
    }
}

impl TryFrom<HealthSnapshotWire> for HealthSnapshot {
    type Error = ContractError;

    fn try_from(value: HealthSnapshotWire) -> Result<Self, Self::Error> {
        let snapshot = Self {
            producer_instance: value.producer_instance,
            boot_session: value.boot_session,
            process_start_id: value.process_start_id,
            ferric_artifact_id: value.ferric_artifact_id,
            watcher_artifact_id: value.watcher_artifact_id,
            build_id: value.build_id,
            protocol: value.protocol,
            schema: value.schema,
            observed_monotonic_ns: value.observed_monotonic_ns,
            last_admitted: value.last_admitted,
            last_durable: value.last_durable,
            counters: value.counters,
            lifecycle: value.lifecycle,
            loops: value.loops,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }
}
