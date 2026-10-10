//! Observe: bounded correlation/progress/outcome adapter, not a recorder or authority.
//! Caller-owned ports deliver allowlisted bytes to the existing diagnostic owners.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};

pub const MAX_FRAME_BYTES: usize = 2048;
pub const MAX_PROGRESS_EVENTS: u32 = 1024;
pub const WIRE_VERSION: u8 = 1;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryClass {
    Progress,
    Terminal,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryError {
    Saturated,
    Rejected,
    Unavailable,
    Indeterminate,
}
/// Nonblocking, all-or-none acceptance. Terminal has reserved admission at the destination.
/// An adapter unable to know whether bytes escaped MUST return Indeterminate, never Ok.
/// The caller enforces fresh grants and destination visibility before this callback.
pub trait SinkPort: Send + Sync {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidBudget,
    InvalidProgress,
    RevisionMismatch,
    Saturated,
    Closed,
    SequenceOverflow,
    FrameLimit,
    Delivery(DeliveryError),
    ReconciliationRequired,
}
impl Error {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidBudget => "invalid_budget",
            Self::InvalidProgress => "invalid_progress",
            Self::RevisionMismatch => "revision_mismatch",
            Self::Saturated => "saturated",
            Self::Closed => "closed",
            Self::SequenceOverflow => "sequence_overflow",
            Self::FrameLimit => "frame_limit",
            Self::Delivery(DeliveryError::Saturated) => "sink_saturated",
            Self::Delivery(DeliveryError::Rejected) => "sink_rejected",
            Self::Delivery(DeliveryError::Unavailable) => "sink_unavailable",
            Self::Delivery(DeliveryError::Indeterminate) => "delivery_indeterminate",
            Self::ReconciliationRequired => "reconciliation_required",
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for Error {}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureCode {
    Validation,
    Unavailable,
    Loss,
    Unsupported,
}
impl FailureCode {
    pub const fn wire(self) -> u8 {
        match self {
            Self::Validation => 1,
            Self::Unavailable => 2,
            Self::Loss => 3,
            Self::Unsupported => 4,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Progress,
    Success,
    Failure(FailureCode),
    Canceled,
}
impl Outcome {
    pub const fn wire(self) -> u8 {
        match self {
            Self::Progress => 1,
            Self::Success => 2,
            Self::Failure(_) => 3,
            Self::Canceled => 4,
        }
    }
    pub const fn class(self) -> DeliveryClass {
        if matches!(self, Self::Progress) {
            DeliveryClass::Progress
        } else {
            DeliveryClass::Terminal
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    completed: u64,
    total: u64,
}
impl Progress {
    pub fn new(completed: u64, total: u64) -> Result<Self, Error> {
        if total == 0 || completed > total {
            Err(Error::InvalidProgress)
        } else {
            Ok(Self { completed, total })
        }
    }
    pub fn completed(self) -> u64 {
        self.completed
    }
    pub fn total(self) -> u64 {
        self.total
    }
}
/// Finite lifetime admission; one terminal is reserved separately from progress budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    progress_events: u32,
    progress_bytes: usize,
}
impl Budget {
    pub fn new(progress_events: u32, progress_bytes: usize) -> Result<Self, Error> {
        if progress_events > MAX_PROGRESS_EVENTS
            || progress_bytes > MAX_FRAME_BYTES * MAX_PROGRESS_EVENTS as usize
        {
            Err(Error::InvalidBudget)
        } else {
            Ok(Self {
                progress_events,
                progress_bytes,
            })
        }
    }
}
/// Closed command payload. Private content is a borrowed input and NEVER read or emitted.
/// No labels, messages, stack traces, arbitrary attributes or source paths are telemetry fields.
#[derive(Clone, Copy)]
pub struct Observation<'a> {
    pub correlation_id: u64,
    pub revision: u64,
    pub outcome: Outcome,
    pub progress: Option<Progress>,
    pub private_project_text: Option<&'a str>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub correlation_id: u64,
    pub revision: u64,
    pub sequence: u64,
    pub outcome: Outcome,
    pub delivered_bytes: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    pub acknowledged: u64,
    pub progress_events: u32,
    pub progress_bytes: usize,
    pub terminal_closed: bool,
    pub reconciliation_required: bool,
    pub saturation_count: u64,
    pub rejection_count: u64,
    pub unavailable_count: u64,
    pub indeterminate_count: u64,
    pub dropped_attempts: u64,
    pub canceled_delivered: u64,
}
/// One emitter per correlation/resource/revision. Mutable ownership serializes callbacks;
/// it is not a job scheduler, database, session registry, queue or Flight Recorder replacement.
pub struct Observe {
    correlation_id: u64,
    revision: u64,
    resource: DomainId,
    actor: ActorContext,
    budget: Budget,
    state: State,
}
impl Observe {
    pub fn new(
        correlation_id: u64,
        revision: u64,
        resource: DomainId,
        actor: ActorContext,
        budget: Budget,
    ) -> Self {
        Self {
            correlation_id,
            revision,
            resource,
            actor,
            budget,
            state: State {
                acknowledged: 0,
                progress_events: 0,
                progress_bytes: 0,
                terminal_closed: false,
                reconciliation_required: false,
                saturation_count: 0,
                rejection_count: 0,
                unavailable_count: 0,
                indeterminate_count: 0,
                dropped_attempts: 0,
                canceled_delivered: 0,
            },
        }
    }
    pub fn state(&self) -> State {
        self.state
    }
    pub fn emit(
        &mut self,
        event: Observation<'_>,
        cancel: &CancellationToken,
        sink: &mut impl SinkPort,
    ) -> Result<Receipt, Error> {
        if self.state.reconciliation_required {
            return Err(Error::ReconciliationRequired);
        }
        if self.state.terminal_closed {
            return Err(Error::Closed);
        }
        if event.revision != self.revision || event.correlation_id != self.correlation_id {
            return Err(Error::RevisionMismatch);
        }
        // Cancellation uses the reserved terminal path, even when progress admission is full.
        let outcome = if cancel.check().is_err() {
            Outcome::Canceled
        } else {
            event.outcome
        };
        let progress = if outcome == Outcome::Progress {
            Some(event.progress.ok_or(Error::InvalidProgress)?)
        } else {
            None
        };
        let sequence = self
            .state
            .acknowledged
            .checked_add(1)
            .ok_or(Error::SequenceOverflow)?;
        let class = outcome.class();
        if class == DeliveryClass::Progress
            && self.state.progress_events >= self.budget.progress_events
        {
            self.state.saturation_count = self.state.saturation_count.saturating_add(1);
            self.state.dropped_attempts = self.state.dropped_attempts.saturating_add(1);
            return Err(Error::Saturated);
        }
        let frame = self.encode(sequence, outcome, progress)?;
        let next_bytes = self
            .state
            .progress_bytes
            .checked_add(frame.len())
            .ok_or(Error::FrameLimit)?;
        if class == DeliveryClass::Progress && next_bytes > self.budget.progress_bytes {
            self.state.saturation_count = self.state.saturation_count.saturating_add(1);
            self.state.dropped_attempts = self.state.dropped_attempts.saturating_add(1);
            return Err(Error::Saturated);
        }
        match sink.try_send(class, &frame) {
            Ok(()) => {}
            Err(error) => {
                self.state.dropped_attempts = self.state.dropped_attempts.saturating_add(1);
                match error {
                    DeliveryError::Saturated => {
                        self.state.saturation_count = self.state.saturation_count.saturating_add(1)
                    }
                    DeliveryError::Rejected => {
                        self.state.rejection_count = self.state.rejection_count.saturating_add(1)
                    }
                    DeliveryError::Unavailable => {
                        self.state.unavailable_count =
                            self.state.unavailable_count.saturating_add(1)
                    }
                    DeliveryError::Indeterminate => {
                        self.state.indeterminate_count =
                            self.state.indeterminate_count.saturating_add(1)
                    }
                }
                if error == DeliveryError::Indeterminate {
                    self.state.reconciliation_required = true
                }
                return Err(Error::Delivery(error));
            }
        }
        self.state.acknowledged = sequence;
        if outcome == Outcome::Canceled {
            self.state.canceled_delivered = self.state.canceled_delivered.saturating_add(1)
        }
        if class == DeliveryClass::Progress {
            self.state.progress_events += 1;
            self.state.progress_bytes = next_bytes
        } else {
            self.state.terminal_closed = true
        }
        Ok(Receipt {
            correlation_id: self.correlation_id,
            revision: self.revision,
            sequence,
            outcome,
            delivered_bytes: frame.len(),
        })
    }
    fn encode(
        &self,
        sequence: u64,
        outcome: Outcome,
        progress: Option<Progress>,
    ) -> Result<Vec<u8>, Error> {
        let strings = [
            self.resource.as_str(),
            self.actor.account_id(),
            self.actor.principal_id(),
            self.actor.owner_account_id(),
            self.actor.owner_principal_id(),
            self.actor.access_space_id(),
            self.actor.session_id(),
        ];
        let length = 48 + strings.iter().map(|s| 2 + s.len()).sum::<usize>();
        if length > MAX_FRAME_BYTES {
            return Err(Error::FrameLimit);
        }
        let mut out = Vec::with_capacity(length);
        out.extend_from_slice(b"HSKO");
        out.extend_from_slice(&[
            WIRE_VERSION,
            outcome.wire(),
            if let Outcome::Failure(code) = outcome {
                code.wire()
            } else {
                0
            },
            u8::from(progress.is_some()),
        ]);
        for value in [
            self.correlation_id,
            self.revision,
            sequence,
            progress.map_or(0, Progress::completed),
            progress.map_or(0, Progress::total),
        ] {
            out.extend_from_slice(&value.to_be_bytes())
        }
        for string in strings {
            let count = u16::try_from(string.len()).map_err(|_| Error::FrameLimit)?;
            out.extend_from_slice(&count.to_be_bytes());
            out.extend_from_slice(string.as_bytes())
        }
        Ok(out)
    }
}

/// Same descriptor for models, human manual and Argus/diagnostic adapters. No new Studio schema ID.
pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-OBSERVE","version":1,"operation":"Observe::emit closed allowlisted progress/outcome delivery","operator":{"intent":"Track work without exposing project text","result":"Correlation/revision/sequence receipt only after required sink accepts the emitted bytes","recovery":"On full/rejected/unavailable sink correct destination or admission and retry same revision; indeterminate delivery requires caller reconciliation before replacing emitter; stale context must refresh; canceled work emits a reserved terminal outcome"},"model":{"inputs":"Accord DomainId/ActorContext plus local u64 correlation/revision, bounded Budget, Observation progress or fixed success/failure/canceled enum, cancellation token, caller SinkPort","consumer":"observe-consumer [--descriptor] [--mode success|error|cancel|reject|saturate] [--private-text TEXT]","wire":"Local binary transport v1, not canonical document or business event: HSKO ASCII, u8 version=1,outcome=1(progress)/2(success)/3(failure)/4(canceled),u8 fixed failure code0..4,u8 progress-present0|1; five big-endian u64 fields correlation,revision,sequence,completed,total; seven u16-byte-length UTF8 strings resource_id,account_id,principal_id,owner_account_id,owner_principal_id,access_space_id,session_id; reject trailing bytes at collector","limits":{"frame_bytes":2048,"progress_events":1024,"progress_lifetime_bytes":2097152,"terminal_reserve":1,"pending_callers_per_mutable_emitter":1},"redaction":"Private project text, raw errors, arbitrary attributes, labels and source paths never serialized; input content is borrowed and not inspected. Only caller-authorized Accord attribution IDs are included. Destination visibility/grants are caller-owned and host conformance remains pending","port":"Nonblocking atomic all-or-none try_send(Progress|Terminal,bytes); terminal reserves independent destination capacity. Indeterminate partial delivery never reports success; emitter blocks further delivery until caller reconciles","cancel":"Canceled token overrides requested outcome to Canceled; progress saturation cannot consume terminal reserve; delivered terminal closes emitter","realtime":"General diagnostic path allocates bounded frame memory; never invoke from realtime audio callback; retain existing multiple-writer/SPSC synchronization in host adapter","undo":"No document mutation or undo; this is not an EventLedger acknowledgment","argus":{"inspect":"Observe::state bounded saturating drop/saturation/rejection/unavailable/indeterminate/canceled counters and typed receipt/error","action":"same emit port and cancellation token","capture":"caller captures actual emitted bytes and independently decoded fields under fresh grant"},"diagnostics":"Caller sinks adapt to existing Flight Recorder/internal diagnostics/Palmistry; no private recorder/catalog/DB/session authority or host proof"}}"#;
