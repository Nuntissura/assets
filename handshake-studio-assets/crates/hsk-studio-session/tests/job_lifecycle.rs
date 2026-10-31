//! Lane SMB-JOB-ADMISSION-1 (RF-08 phase 0): the typed job lifecycle over the `JobPort`
//! (STU-ARC-008 bounded admission by actor, document and resource; STU-ARC-013 stale results are
//! never dropped silently; GUI-DESKTOP-033 cancellation releases only at the terminal receipt;
//! GUI-DESKTOP-034 exactly one release).
//!
//! Oracles: a counting port double (every admit and release recorded with its ticket), a sink
//! double (every terminal frame counted), hand-counted slots and bytes.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};
use hsk_studio_session::*;
use std::sync::{Arc, Mutex};

fn actor() -> ActorContext {
    ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap()
}

fn doc() -> DomainId {
    DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000009").unwrap()
}

fn request(bound: u64, correlation: u64, kind: JobKind) -> JobRequest {
    JobRequest {
        resource: Some(doc()),
        action: Action::Export,
        actor: actor(),
        cost: Cost { jobs: 1, bytes: 1024 },
        bound_revision: bound,
        correlation_id: correlation,
        kind,
    }
}

/// Port double: admits while `refuse` is `None`, records every admit and release.
#[derive(Default)]
struct Counting {
    refuse: Option<u64>,
    log: Mutex<(u64, Vec<u64>, Vec<u64>)>,
}
impl Counting {
    fn admitted(&self) -> Vec<u64> {
        self.log.lock().unwrap().1.clone()
    }
    fn released(&self) -> Vec<u64> {
        self.log.lock().unwrap().2.clone()
    }
}
impl JobPort for Counting {
    fn admit(&self, scope: &JobScope<'_>, _cost: Cost) -> Result<JobTicket, JobRefused> {
        assert_eq!(scope.resource, &doc(), "the request's resource reaches the port");
        if let Some(retry_after_ms) = self.refuse {
            return Err(JobRefused { retry_after_ms });
        }
        let mut log = self.log.lock().unwrap();
        log.0 += 1;
        let ticket = log.0;
        log.1.push(ticket);
        Ok(JobTicket(ticket))
    }
    fn release(&self, ticket: JobTicket) {
        self.log.lock().unwrap().2.push(ticket.0);
    }
}

#[derive(Clone, Default)]
struct Frames(Arc<Mutex<Vec<Vec<u8>>>>);
impl SinkPort for Frames {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        assert_eq!(class, DeliveryClass::Terminal, "only terminal frames");
        self.0.lock().unwrap().push(bytes.to_vec());
        Ok(())
    }
}

fn setup(port: Counting) -> (Arc<Counting>, Arc<dyn JobPort>, JobLifecycle, Frames) {
    let port = Arc::new(port);
    let dynamic: Arc<dyn JobPort> = port.clone();
    let lifecycle = JobLifecycle::default();
    let frames = Frames::default();
    lifecycle.set_sink(Box::new(frames.clone()));
    (port, dynamic, lifecycle, frames)
}

/// Success, failure, cancel (settled from the work's own receipt, never at the cancel) and drop
/// each release their own ticket exactly once; the lifecycle counts each outcome once, one
/// terminal frame per job reaches the sink and nothing is in flight afterwards.
#[test]
fn settle_releases_the_ticket_exactly_once_on_every_exit() {
    let (port, dynamic, lifecycle, frames) = setup(Counting::default());
    let token = CancellationToken::default();
    let ok = lifecycle.admit(&dynamic, request(3, 1, JobKind::Committed), 3, &token).unwrap();
    let failed = lifecycle.admit(&dynamic, request(3, 2, JobKind::Committed), 3, &token).unwrap();
    let canceled = lifecycle.admit(&dynamic, request(3, 3, JobKind::Committed), 3, &token).unwrap();
    let dropped = lifecycle.admit(&dynamic, request(3, 4, JobKind::Committed), 3, &token).unwrap();
    assert_eq!(port.admitted(), [1, 2, 3, 4]);
    assert_eq!(lifecycle.counts().in_flight, 4);

    let s = ok.complete(3);
    assert_eq!((s.ticket, s.outcome, s.correlation_id), (Some(JobTicket(1)), JobOutcome::Completed, 1));
    assert!(matches!(s.report, ReportStatus::Delivered(_)), "{:?}", s.report);
    let s = failed.settle(JobOutcome::Failed("encoder_failed"));
    assert_eq!((s.ticket, s.outcome), (Some(JobTicket(2)), JobOutcome::Failed("encoder_failed")));
    // The cancel is requested on the work; the ticket stays held until its receipt arrives.
    assert_eq!(port.released(), [1, 2], "a requested cancel releases nothing by itself");
    let s = canceled.settle(JobOutcome::Canceled);
    assert_eq!((s.ticket, s.outcome), (Some(JobTicket(3)), JobOutcome::Canceled));
    drop(dropped);

    assert_eq!(port.released(), [1, 2, 3, 4], "each ticket released once, in settlement order");
    let c = lifecycle.counts();
    assert_eq!((c.admitted, c.completed, c.failed, c.canceled, c.abandoned, c.released, c.in_flight), (4, 1, 1, 1, 1, 4, 0), "{c:?}");
    assert_eq!(frames.0.lock().unwrap().len(), 4, "one terminal frame per job");
}

/// A token canceled before admission refuses `Canceled` without asking the port (nothing held,
/// nothing to release); the refusal is counted and reported.
#[test]
fn a_canceled_token_refuses_before_admission() {
    let (port, dynamic, lifecycle, frames) = setup(Counting::default());
    let token = CancellationToken::default();
    token.cancel();
    let refusal = lifecycle.admit(&dynamic, request(3, 1, JobKind::Committed), 3, &token).unwrap_err();
    assert_eq!(refusal, JobAdmissionRefusal::Canceled);
    assert_eq!(refusal.code(), "canceled");
    assert!(port.admitted().is_empty() && port.released().is_empty(), "the port was never asked");
    let c = lifecycle.counts();
    assert_eq!((c.refused_canceled, c.admitted, c.in_flight), (1, 0, 0), "{c:?}");
    assert_eq!(frames.0.lock().unwrap().len(), 1, "the refusal is a terminal outcome");
}

/// A port refusal maps to `Overloaded` with the port's own retry hint; nothing is held.
#[test]
fn job_refused_maps_to_overloaded_with_its_retry() {
    let (port, dynamic, lifecycle, _) = setup(Counting { refuse: Some(750), ..Counting::default() });
    let refusal = lifecycle.admit(&dynamic, request(3, 1, JobKind::Committed), 3, &CancellationToken::default()).unwrap_err();
    assert_eq!(refusal, JobAdmissionRefusal::Overloaded { retry_after_ms: 750 });
    assert_eq!(refusal.code(), "overloaded");
    assert!(port.released().is_empty());
    assert_eq!(lifecycle.counts().refused_overloaded, 1);
    assert_eq!(JobAdmissionRefusal::from(JobRefused { retry_after_ms: 9 }), JobAdmissionRefusal::Overloaded { retry_after_ms: 9 });
}

/// A job whose input revision is behind the head refuses `StaleRevision { expected, head }`
/// before the port is asked; a committed job that finishes after the head moved settles
/// `CompletedStale { bound, head }` (both revisions); `Superseded` is accepted for a replaceable
/// job and recorded `Failed("not_replaceable")` for a committed one.
#[test]
fn stale_revision_and_completed_stale_carry_both_revisions() {
    let (port, dynamic, lifecycle, _) = setup(Counting::default());
    let token = CancellationToken::default();
    let refusal = lifecycle.admit(&dynamic, request(4, 1, JobKind::Committed), 7, &token).unwrap_err();
    assert_eq!(refusal, JobAdmissionRefusal::StaleRevision { expected: 4, head: 7 });
    assert!(port.admitted().is_empty(), "refused before the port");

    let job = lifecycle.admit(&dynamic, request(7, 2, JobKind::Committed), 7, &token).unwrap();
    assert_eq!(job.bound_revision(), 7);
    let s = job.complete(9);
    assert_eq!(s.outcome, JobOutcome::CompletedStale { bound: 7, head: 9 });
    assert_eq!(s.outcome.code(), "completed_stale");

    let preview = lifecycle.admit(&dynamic, request(9, 3, JobKind::Replaceable), 9, &token).unwrap();
    assert_eq!(preview.settle(JobOutcome::Superseded { revision: 10 }).outcome, JobOutcome::Superseded { revision: 10 });
    let export = lifecycle.admit(&dynamic, request(9, 4, JobKind::Committed), 9, &token).unwrap();
    assert_eq!(export.settle(JobOutcome::Superseded { revision: 10 }).outcome, JobOutcome::Failed("not_replaceable"));
    let c = lifecycle.counts();
    assert_eq!((c.refused_stale, c.completed_stale, c.superseded, c.failed, c.released), (1, 1, 1, 1, 3), "{c:?}");
}

/// The asset-local `AdmissionJobPort` bounds slots and bytes all-or-none (2 slots / 1000 bytes
/// here): a third job and a job over the remaining bytes are refused with the admission's retry
/// hint, a cost over the whole budget is refused `RETRY_NEVER_MS`, a release frees exactly its own
/// slots and bytes, and a second release of the same ticket does nothing. A resource-less request
/// is bounded by the lifecycle's own admission, never reaches a port and reports `Unaddressed`.
#[test]
fn admission_job_port_bounds_slots_and_bytes() {
    let port = Arc::new(AdmissionJobPort::new(Admission::new(2, 1000, 40).unwrap()));
    let dynamic: Arc<dyn JobPort> = port.clone();
    let scope = JobScope { resource: &doc(), action: Action::Export };
    let a = dynamic.admit(&scope, Cost { jobs: 1, bytes: 600 }).unwrap();
    assert_eq!(dynamic.admit(&scope, Cost { jobs: 1, bytes: 500 }), Err(JobRefused { retry_after_ms: 40 }), "bytes over the remaining budget");
    let b = dynamic.admit(&scope, Cost { jobs: 1, bytes: 400 }).unwrap();
    assert_eq!(port.admission().in_flight(), (2, 1000));
    assert_eq!(dynamic.admit(&scope, Cost { jobs: 1, bytes: 0 }), Err(JobRefused { retry_after_ms: 40 }), "no slot left");
    assert_eq!(dynamic.admit(&scope, Cost { jobs: 1, bytes: 1001 }), Err(JobRefused { retry_after_ms: RETRY_NEVER_MS }), "never fits");
    dynamic.release(a);
    assert_eq!(port.admission().in_flight(), (1, 400));
    dynamic.release(a);
    assert_eq!(port.admission().in_flight(), (1, 400), "a second release of the same ticket does nothing");
    assert_eq!(dynamic.admit(&scope, Cost { jobs: 2, bytes: 0 }), Err(JobRefused { retry_after_ms: 40 }), "two slots, one free: all or none");
    assert_eq!(port.admission().in_flight(), (1, 400), "the partial slot was given back");
    dynamic.release(b);
    assert_eq!((port.admission().in_flight(), port.tickets_held()), ((0, 0), 0));

    let lifecycle = JobLifecycle::new(Admission::new(1, 100, 25).unwrap());
    let mut local = request(0, 5, JobKind::Committed);
    local.resource = None;
    local.cost = Cost { jobs: 1, bytes: 60 };
    let token = CancellationToken::default();
    let held = lifecycle.admit(&dynamic, local.clone(), 0, &token).unwrap();
    assert_eq!((held.ticket(), lifecycle.local_admission().in_flight()), (None, (1, 60)));
    assert_eq!(lifecycle.admit(&dynamic, local.clone(), 0, &token).unwrap_err(), JobAdmissionRefusal::Overloaded { retry_after_ms: 25 });
    let s = held.complete(0);
    assert_eq!((s.ticket, s.outcome, s.report), (None, JobOutcome::Completed, ReportStatus::Unaddressed));
    assert_eq!((lifecycle.local_admission().in_flight(), port.tickets_held()), ((0, 0), 0), "freed; no port ticket was ever taken");
}
