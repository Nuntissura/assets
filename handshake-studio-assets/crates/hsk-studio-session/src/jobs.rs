//! Typed job lifecycle over the [`JobPort`] (lane SMB-JOB-ADMISSION-1, RF-08 phase 0; STU-ARC-008,
//! -012, -013, -020, GUI-DESKTOP-033..035). Additive: the `JobPort` trait and [`Admission`] are
//! unchanged.
//!
//! - [`JobLifecycle::admit`] checks, in this order, the cancellation token
//!   ([`JobAdmissionRefusal::Canceled`]), the bound document revision against the head
//!   ([`JobAdmissionRefusal::StaleRevision`] with both revisions), then asks the port
//!   ([`JobAdmissionRefusal::Overloaded`] with the port's retry hint). A refused job holds nothing.
//! - An admitted job is an [`AdmittedJob`] guard holding the ticket, the port, the request
//!   (resource, action, actor, cost, bound revision, correlation) and the lifecycle.
//!   [`AdmittedJob::settle`] consumes the guard and releases the ticket exactly once; a guard dropped
//!   without settling settles [`JobOutcome::Abandoned`] once and counts it. A second or late release
//!   cannot be written: settling moves the guard (test `settle_releases_the_ticket_exactly_once_on_every_exit`).
//! - Cancellation does not release: the caller cancels the work, waits for the work's own terminal
//!   receipt, then settles with that receipt's outcome (GUI-DESKTOP-033).
//! - A committed job that finished after the head moved settles [`JobOutcome::CompletedStale`]
//!   with both revisions ([`AdmittedJob::complete`]); it is never dropped silently (STU-ARC-013).
//!   [`JobOutcome::Superseded`] is for replaceable jobs (previews) only: a committed job settled
//!   superseded is recorded `Failed("not_replaceable")`.
//! - A request without a resource (no document is addressed, for example a file-to-file batch)
//!   cannot form a [`JobScope`]; it is admitted against the lifecycle's own bounded [`Admission`]
//!   (slots and bytes) and never reaches the port, and its observe report is `Unaddressed`.
//! - Every terminal outcome (refusals included) is counted in [`JobCounts`] and emitted as one
//!   Observe terminal frame through the lifecycle's sink: outcome codes, the bound revision and the
//!   caller's opaque correlation id only, never text (rule 5).
//! - [`AdmissionJobPort`] is an asset-local `JobPort` over a session [`Admission`] so a host-less
//!   caller (the harness viewer) can admit; a host binds its own port (kernel-bindings
//!   `BridgeJobs`, MT-36228).
use crate::admission::{Admission, Permit};
use crate::error::{ReportStatus, SessionError};
use crate::ports::{Action, Cost, JobPort, JobRefused, JobScope, JobTicket};
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::{
    Budget as ObserveBudget, DeliveryError, Error as ObserveError, FailureCode, Observation,
    Observe, Outcome, SinkPort,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

/// Retry hint of a refusal that waiting cannot clear (the cost exceeds the whole budget).
pub const RETRY_NEVER_MS: u64 = u64::MAX;
/// Most slots one [`AdmissionJobPort`] admission may take (`Cost::jobs`).
pub const MAX_JOBS_PER_ADMISSION: u32 = 1024;

/// Whether a newer job may replace this one (previews) or it must finish (exports).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    Committed,
    Replaceable,
}

/// What is admitted: attribution (resource, action, actor), cost, the document revision the job's
/// input was taken from, and the caller's opaque correlation id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobRequest {
    /// The addressed document; `None` admits against the lifecycle's own bounded admission.
    pub resource: Option<DomainId>,
    pub action: Action,
    pub actor: ActorContext,
    pub cost: Cost,
    pub bound_revision: u64,
    pub correlation_id: u64,
    pub kind: JobKind,
}

/// Why a job was not admitted; nothing is held after any of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobAdmissionRefusal {
    Overloaded { retry_after_ms: u64 },
    /// The token was canceled before admission.
    Canceled,
    /// The job's input revision is not the document head.
    StaleRevision { expected: u64, head: u64 },
}

impl JobAdmissionRefusal {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Overloaded { .. } => "overloaded",
            Self::Canceled => "canceled",
            Self::StaleRevision { .. } => "stale_revision",
        }
    }

    const fn observe(self) -> Outcome {
        match self {
            Self::Overloaded { .. } => Outcome::Failure(FailureCode::Unavailable),
            Self::Canceled => Outcome::Canceled,
            Self::StaleRevision { .. } => Outcome::Failure(FailureCode::Validation),
        }
    }
}

impl From<JobRefused> for JobAdmissionRefusal {
    fn from(refused: JobRefused) -> Self {
        Self::Overloaded {
            retry_after_ms: refused.retry_after_ms,
        }
    }
}

impl std::fmt::Display for JobAdmissionRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for JobAdmissionRefusal {}

/// Terminal outcome of an admitted job, taken from the job's own terminal receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobOutcome {
    Completed,
    /// Finished from its bound snapshot after the head moved (a committed export).
    CompletedStale { bound: u64, head: u64 },
    Canceled,
    /// The work's stable failure code.
    Failed(&'static str),
    /// Replaced by a newer job at `revision` (replaceable jobs only).
    Superseded { revision: u64 },
    /// The guard was dropped without a settlement.
    Abandoned,
}

impl JobOutcome {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::CompletedStale { .. } => "completed_stale",
            Self::Canceled => "canceled",
            Self::Failed(_) => "failed",
            Self::Superseded { .. } => "superseded",
            Self::Abandoned => "abandoned",
        }
    }

    const fn observe(self) -> Outcome {
        match self {
            Self::Completed | Self::CompletedStale { .. } => Outcome::Success,
            Self::Canceled | Self::Superseded { .. } => Outcome::Canceled,
            Self::Failed(_) => Outcome::Failure(FailureCode::Unavailable),
            Self::Abandoned => Outcome::Failure(FailureCode::Loss),
        }
    }
}

/// The record of one settlement: the released ticket (`None` for a resource-less job held by the
/// lifecycle's own admission), the outcome as recorded and the fate of its observe frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobSettlement {
    pub ticket: Option<JobTicket>,
    pub correlation_id: u64,
    pub bound_revision: u64,
    pub outcome: JobOutcome,
    pub report: ReportStatus,
}

/// Counters of every terminal outcome since the lifecycle was made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JobCounts {
    pub admitted: u64,
    pub refused_overloaded: u64,
    pub refused_canceled: u64,
    pub refused_stale: u64,
    pub completed: u64,
    pub completed_stale: u64,
    pub canceled: u64,
    pub failed: u64,
    pub superseded: u64,
    pub abandoned: u64,
    /// Tickets or local permits given back (one per settlement).
    pub released: u64,
    /// Admitted and not yet settled.
    pub in_flight: u64,
}

struct Inner {
    counts: Mutex<JobCounts>,
    sink: Mutex<Option<Box<dyn SinkPort>>>,
    local: Admission,
}

/// Shared counters, observe sink and resource-less admission; cloning shares them.
#[derive(Clone)]
pub struct JobLifecycle {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for JobLifecycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobLifecycle")
            .field("counts", &self.counts())
            .finish_non_exhaustive()
    }
}

impl Default for JobLifecycle {
    fn default() -> Self {
        Self::new(Admission::default())
    }
}

impl JobLifecycle {
    /// `local` bounds resource-less jobs (module docs).
    pub fn new(local: Admission) -> Self {
        Self {
            inner: Arc::new(Inner {
                counts: Mutex::new(JobCounts::default()),
                sink: Mutex::new(None),
                local,
            }),
        }
    }

    /// Terminal frames go to `sink` from now on (none before: reports are `sink_unavailable`).
    pub fn set_sink(&self, sink: Box<dyn SinkPort>) {
        *self
            .inner
            .sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(sink);
    }

    pub fn counts(&self) -> JobCounts {
        *self
            .inner
            .counts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The bounded admission of resource-less jobs.
    pub fn local_admission(&self) -> &Admission {
        &self.inner.local
    }

    /// Admits `request` (module docs for the order). `head_revision` is the document head now.
    pub fn admit(
        &self,
        port: &Arc<dyn JobPort>,
        request: JobRequest,
        head_revision: u64,
        cancel: &CancellationToken,
    ) -> Result<AdmittedJob, JobAdmissionRefusal> {
        let held = if cancel.check().is_err() {
            Err(JobAdmissionRefusal::Canceled)
        } else if request.bound_revision != head_revision {
            Err(JobAdmissionRefusal::StaleRevision {
                expected: request.bound_revision,
                head: head_revision,
            })
        } else {
            self.hold(port, &request)
        };
        match held {
            Ok(hold) => {
                self.count(|c| {
                    c.admitted = c.admitted.saturating_add(1);
                    c.in_flight = c.in_flight.saturating_add(1);
                });
                Ok(AdmittedJob {
                    hold: Some(hold),
                    request,
                    lifecycle: self.clone(),
                })
            }
            Err(refusal) => {
                self.count(|c| {
                    let n = match refusal {
                        JobAdmissionRefusal::Overloaded { .. } => &mut c.refused_overloaded,
                        JobAdmissionRefusal::Canceled => &mut c.refused_canceled,
                        JobAdmissionRefusal::StaleRevision { .. } => &mut c.refused_stale,
                    };
                    *n = n.saturating_add(1);
                });
                let _ = self.emit(&request, refusal.observe());
                Err(refusal)
            }
        }
    }

    /// Records a replaceable job that was never admitted (a synchronous preview) as superseded:
    /// counted and emitted like a settlement, nothing to release.
    pub fn record_superseded(&self, request: &JobRequest, revision: u64) -> JobSettlement {
        let outcome = JobOutcome::Superseded { revision };
        self.count(|c| c.superseded = c.superseded.saturating_add(1));
        JobSettlement {
            ticket: None,
            correlation_id: request.correlation_id,
            bound_revision: request.bound_revision,
            outcome,
            report: self.emit(request, outcome.observe()),
        }
    }

    fn hold(
        &self,
        port: &Arc<dyn JobPort>,
        request: &JobRequest,
    ) -> Result<Hold, JobAdmissionRefusal> {
        match &request.resource {
            Some(resource) => {
                let ticket = port.admit(
                    &JobScope {
                        resource,
                        action: request.action,
                    },
                    request.cost,
                )?;
                Ok(Hold::Port {
                    port: Arc::clone(port),
                    ticket,
                })
            }
            None => self
                .inner
                .local
                .try_acquire(request.cost.bytes)
                .map(Hold::Local)
                .map_err(local_refusal),
        }
    }

    fn count(&self, f: impl FnOnce(&mut JobCounts)) {
        f(&mut self
            .inner
            .counts
            .lock()
            .unwrap_or_else(PoisonError::into_inner));
    }

    fn emit(&self, request: &JobRequest, outcome: Outcome) -> ReportStatus {
        let Some(resource) = &request.resource else {
            return ReportStatus::Unaddressed;
        };
        let mut sink = self
            .inner
            .sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(sink) = sink.as_mut() else {
            return ReportStatus::Undelivered(ObserveError::Delivery(DeliveryError::Unavailable));
        };
        let budget = match ObserveBudget::new(0, 0) {
            Ok(budget) => budget,
            Err(error) => return ReportStatus::Undelivered(error),
        };
        let mut observe = Observe::new(
            request.correlation_id,
            request.bound_revision,
            resource.clone(),
            request.actor.clone(),
            budget,
        );
        // A fresh token: the job decided its terminal outcome; a later cancel must not recast it.
        match observe.emit(
            Observation {
                correlation_id: request.correlation_id,
                revision: request.bound_revision,
                outcome,
                progress: None,
                private_project_text: None,
            },
            &CancellationToken::default(),
            &mut **sink,
        ) {
            Ok(receipt) => ReportStatus::Delivered(receipt),
            Err(error) => ReportStatus::Undelivered(error),
        }
    }
}

fn local_refusal(error: SessionError) -> JobAdmissionRefusal {
    match error {
        SessionError::Overloaded { retry_after_ms } => {
            JobAdmissionRefusal::Overloaded { retry_after_ms }
        }
        _ => JobAdmissionRefusal::Overloaded {
            retry_after_ms: RETRY_NEVER_MS,
        },
    }
}

enum Hold {
    Port {
        port: Arc<dyn JobPort>,
        ticket: JobTicket,
    },
    Local(Permit),
}

/// An admitted job (module docs). Settle it with the outcome of the job's own terminal receipt.
pub struct AdmittedJob {
    hold: Option<Hold>,
    request: JobRequest,
    lifecycle: JobLifecycle,
}

impl std::fmt::Debug for AdmittedJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdmittedJob")
            .field("ticket", &self.ticket())
            .field("request", &self.request)
            .finish_non_exhaustive()
    }
}

impl AdmittedJob {
    /// The port's ticket; `None` for a resource-less job.
    pub fn ticket(&self) -> Option<JobTicket> {
        match &self.hold {
            Some(Hold::Port { ticket, .. }) => Some(*ticket),
            _ => None,
        }
    }

    pub fn request(&self) -> &JobRequest {
        &self.request
    }

    pub fn bound_revision(&self) -> u64 {
        self.request.bound_revision
    }

    /// Releases the ticket once and records `outcome`.
    pub fn settle(mut self, outcome: JobOutcome) -> JobSettlement {
        self.finish(outcome)
    }

    /// Settles a successful job: `Completed` when the head is still the bound revision, else
    /// `CompletedStale { bound, head }`.
    pub fn complete(self, head_revision: u64) -> JobSettlement {
        let bound = self.request.bound_revision;
        let outcome = if head_revision == bound {
            JobOutcome::Completed
        } else {
            JobOutcome::CompletedStale {
                bound,
                head: head_revision,
            }
        };
        self.settle(outcome)
    }

    fn finish(&mut self, outcome: JobOutcome) -> JobSettlement {
        let outcome = match outcome {
            JobOutcome::Superseded { .. } if self.request.kind == JobKind::Committed => {
                JobOutcome::Failed("not_replaceable")
            }
            other => other,
        };
        let ticket = match self.hold.take() {
            Some(Hold::Port { port, ticket }) => {
                port.release(ticket);
                Some(ticket)
            }
            // Dropping the permit gives the slot and bytes back.
            Some(Hold::Local(_permit)) => None,
            None => None,
        };
        self.lifecycle.count(|c| {
            let n = match outcome {
                JobOutcome::Completed => &mut c.completed,
                JobOutcome::CompletedStale { .. } => &mut c.completed_stale,
                JobOutcome::Canceled => &mut c.canceled,
                JobOutcome::Failed(_) => &mut c.failed,
                JobOutcome::Superseded { .. } => &mut c.superseded,
                JobOutcome::Abandoned => &mut c.abandoned,
            };
            *n = n.saturating_add(1);
            c.released = c.released.saturating_add(1);
            c.in_flight = c.in_flight.saturating_sub(1);
        });
        JobSettlement {
            ticket,
            correlation_id: self.request.correlation_id,
            bound_revision: self.request.bound_revision,
            outcome,
            report: self.lifecycle.emit(&self.request, outcome.observe()),
        }
    }
}

impl Drop for AdmittedJob {
    fn drop(&mut self) {
        if self.hold.is_some() {
            let _ = self.finish(JobOutcome::Abandoned);
        }
    }
}

/// Asset-local [`JobPort`] over a session [`Admission`] (module docs). One admission takes
/// `max(cost.jobs, 1)` slots (at most [`MAX_JOBS_PER_ADMISSION`]) and `cost.bytes`; all or none.
/// A cost no wait can admit (bytes over the whole budget, too many slots) is refused with
/// [`RETRY_NEVER_MS`]. Releasing an unknown or already released ticket does nothing.
#[derive(Debug)]
pub struct AdmissionJobPort {
    admission: Admission,
    held: Mutex<(u64, BTreeMap<u64, Vec<Permit>>)>,
}

impl Default for AdmissionJobPort {
    fn default() -> Self {
        Self::new(Admission::default())
    }
}

impl AdmissionJobPort {
    pub fn new(admission: Admission) -> Self {
        Self {
            admission,
            held: Mutex::new((0, BTreeMap::new())),
        }
    }

    pub fn admission(&self) -> &Admission {
        &self.admission
    }

    /// Tickets currently held.
    pub fn tickets_held(&self) -> usize {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .1
            .len()
    }
}

impl JobPort for AdmissionJobPort {
    fn admit(&self, _scope: &JobScope<'_>, cost: Cost) -> Result<JobTicket, JobRefused> {
        let never = JobRefused {
            retry_after_ms: RETRY_NEVER_MS,
        };
        let slots = cost.jobs.max(1);
        if slots > MAX_JOBS_PER_ADMISSION {
            return Err(never);
        }
        let mut permits = Vec::new();
        for i in 0..slots {
            let bytes = if i == 0 { cost.bytes } else { 0 };
            match self.admission.try_acquire(bytes) {
                Ok(permit) => permits.push(permit),
                // Dropping `permits` gives back what this admission already took.
                Err(SessionError::Overloaded { retry_after_ms }) => {
                    return Err(JobRefused { retry_after_ms });
                }
                Err(_) => return Err(never),
            }
        }
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let ticket = held.0.checked_add(1).ok_or(never)?;
        held.0 = ticket;
        held.1.insert(ticket, permits);
        Ok(JobTicket(ticket))
    }

    fn release(&self, ticket: JobTicket) {
        let permits = self
            .held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .1
            .remove(&ticket.0);
        drop(permits);
    }
}
