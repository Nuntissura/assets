use crate::{BoundedText, ContractError, EventId};
use serde::{Deserialize, Serialize};

/// Evidence importance used to select a legal watcher policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Criticality {
    LossyTelemetry,
    Normal,
    Critical,
}

/// Highest sensitivity carried by an event or field.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    Public,
    Internal,
    Secret,
}

/// Exhaustive watcher admission policy declared by an event kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatcherPolicy {
    DurableCritical,
    PersistRedacted,
    DropWithCounter,
    Reject,
}

/// Stable event taxonomy. Unknown kinds retain their mandatory bit for fail-closed handling.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EventKind {
    JobStateChanged,
    ExtractorSelected,
    RequestStarted,
    RequestCompleted,
    RetryScheduled,
    FragmentCompleted,
    ThroughputSample,
    QueuePressure,
    JavascriptWorkerAction,
    FfmpegPlan,
    FfmpegStatus,
    OutputCommitted,
    Warning,
    Error,
    Terminal,
    Crash,
    InvariantBreach,
    ProducerCanary,
    WatcherCanary,
    ProcessExitObserved,
    GapMarker,
    Unknown { id: BoundedText, mandatory: bool },
}

impl EventKind {
    #[must_use]
    pub const fn requires_critical_lane(&self) -> bool {
        matches!(
            self,
            Self::Terminal
                | Self::Crash
                | Self::InvariantBreach
                | Self::ProcessExitObserved
                | Self::GapMarker
        )
    }

    #[must_use]
    pub const fn is_declared_lossy(&self) -> bool {
        matches!(self, Self::ThroughputSample | Self::QueuePressure)
    }

    #[must_use]
    pub const fn is_unknown_mandatory(&self) -> bool {
        matches!(
            self,
            Self::Unknown {
                mandatory: true,
                ..
            }
        )
    }
}

/// Stable kind metadata validated before an event may be framed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "EventDescriptorWire")]
pub struct EventDescriptor {
    pub stable_id: EventId,
    pub kind: EventKind,
    pub criticality: Criticality,
    pub sensitivity: Sensitivity,
    pub watcher_policy: WatcherPolicy,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventDescriptorWire {
    stable_id: EventId,
    kind: EventKind,
    criticality: Criticality,
    sensitivity: Sensitivity,
    watcher_policy: WatcherPolicy,
}

impl EventDescriptor {
    /// Creates legal metadata for one stable event kind.
    ///
    /// # Errors
    /// Returns [`ContractError`] for unknown mandatory kinds or illegal policy combinations.
    pub fn new(
        stable_id: EventId,
        kind: EventKind,
        criticality: Criticality,
        sensitivity: Sensitivity,
        watcher_policy: WatcherPolicy,
    ) -> Result<Self, ContractError> {
        let descriptor = Self {
            stable_id,
            kind,
            criticality,
            sensitivity,
            watcher_policy,
        };
        descriptor.validate()?;
        Ok(descriptor)
    }

    /// Revalidates event criticality and watcher policy.
    ///
    /// # Errors
    /// Returns [`ContractError`] for unknown mandatory kinds or illegal policy combinations.
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.kind.is_unknown_mandatory() {
            return Err(ContractError::UnknownMandatoryKind);
        }
        if self.kind.requires_critical_lane()
            && (self.criticality != Criticality::Critical
                || self.watcher_policy != WatcherPolicy::DurableCritical)
        {
            return Err(ContractError::IllegalEventPolicy);
        }
        if self.watcher_policy == WatcherPolicy::DropWithCounter
            && (!self.kind.is_declared_lossy() || self.criticality != Criticality::LossyTelemetry)
        {
            return Err(ContractError::IllegalEventPolicy);
        }
        if self.watcher_policy == WatcherPolicy::Reject {
            return Err(ContractError::IllegalEventPolicy);
        }
        if self.criticality == Criticality::Critical
            && self.watcher_policy != WatcherPolicy::DurableCritical
        {
            return Err(ContractError::IllegalEventPolicy);
        }
        if matches!(
            self.kind,
            EventKind::Unknown {
                mandatory: false,
                ..
            }
        ) && self.watcher_policy == WatcherPolicy::DurableCritical
        {
            return Err(ContractError::IllegalEventPolicy);
        }
        Ok(())
    }
}

impl TryFrom<EventDescriptorWire> for EventDescriptor {
    type Error = ContractError;

    fn try_from(value: EventDescriptorWire) -> Result<Self, Self::Error> {
        Self::new(
            value.stable_id,
            value.kind,
            value.criticality,
            value.sensitivity,
            value.watcher_policy,
        )
    }
}
