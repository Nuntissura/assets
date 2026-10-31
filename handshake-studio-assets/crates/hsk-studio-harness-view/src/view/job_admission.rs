//! One admitted job path for the viewer's local queues (lane SMB-JOB-ADMISSION-1, RF-08 phase 0;
//! STU-ARC-008 / -012 / -013 / -020, GUI-DESKTOP-033..035). No toolkit code.
//!
//! # Manual (no-context operator or model)
//!
//! - **Port.** Every job of the Export Media worker, reel's export queue and the audio batch queue
//!   is admitted through the session [`JobPort`] before it is submitted, before any thread or
//!   encoder starts, through `hsk_studio_session::JobLifecycle`. The viewer starts with the
//!   asset-local `AdmissionJobPort` (session `Admission` default: 16 slots / 256 MiB); a host binds
//!   its Job Runtime port with [`Viewer::set_job_port`] (kernel-bindings `BridgeJobs`, MT-36228)
//!   and its observe sink with [`Viewer::set_job_sink`].
//! - **Refusals** are typed before anything runs: `overloaded` (with the port's retry hint),
//!   `canceled` (the viewer's token was set) and `stale_revision`. Export Media answers them as a
//!   [`MonitorFail`] with that code; the audio batch as a failed or canceled receipt with that code.
//! - **Settlement** comes from each item's own receipt: complete -> `completed`, or
//!   `completed_stale` (both revisions) when the document head moved while the job ran (the file is
//!   the bound snapshot's; the result says so and the status log names both revisions); canceled ->
//!   `canceled`; failed -> `failed`. Cancel never releases by itself: the ticket is held until the
//!   worker's receipt arrives (GUI-DESKTOP-033). A receipt that never arrives (worker panic) settles
//!   `failed` `export_worker`.
//! - **Attribution.** Each request carries the Run's actor, the open document (none: a resource-less
//!   job bounded by the lifecycle's own admission, see session `jobs.rs`), the action, a cost, the
//!   document revision the job's input was taken from and its own correlation: one per submitted job,
//!   minted from the viewer's `RunContext` correlation source ([`JobAdmission::next_correlation`],
//!   R48 F-R48-3), never a queue ordinal, an item id or a revision; every frame of that job (refusal
//!   or settlement) carries it, and [`settlement_json`] reports it (`correlation`).
//! - **Deadline.** The viewer's job classes have no deadline by nature (lane SMB-R47-FOLLOWUPS-1
//!   notes, HBR-HOST-004): an Export Media / reel export-queue item and an audio batch item each
//!   produce the operator's requested file, whose run time scales with its length; the operator ends
//!   one by cancel, and a late file is still the requested one. They admit through
//!   `JobLifecycle::admit` (no deadline); a job class with one admits through session
//!   `JobLifecycle::admit_by` and settles `deadline_exceeded`. Previews are not admitted (supersession
//!   bounds them, `super::preview_gate`).
//! - **Diagnostics.** [`Viewer::job_counts`] (`state_json()["jobs"]` is not added: no `mod.rs`
//!   surface), one Observe terminal frame per outcome (codes and counters only).
//! - **Scope.** reel itself is unchanged (no reel -> session edge): product code builds reel queues
//!   only through `timeline_export::export_queue` and admits them here. The AE render queue
//!   (`timeline_export::render_queue`) builds reel jobs but no product source runs them (only harness
//!   tests do); a product run path admits through [`AdmittedExportQueue`]. Previews are synchronous
//!   and not admitted; their stale-frame gate is `super::preview_gate`.
use super::Viewer;
#[cfg(feature = "media")]
use super::monitor::MonitorFail;
use hsk_studio_accord::{ActorContext, CancellationToken, CorrelationId, CorrelationSource, DomainId, TransientCorrelation};
use hsk_studio_observe::SinkPort;
use hsk_studio_session::{
    Action, AdmissionJobPort, AdmittedJob, Cost, JobAdmissionRefusal, JobCounts, JobKind, JobLifecycle, JobOutcome, JobPort,
    JobRequest, JobSettlement,
};
use serde_json::{Value, json};
use std::sync::Arc;

/// The viewer's job port, lifecycle and preview gate (one per viewer).
pub(in crate::view) struct ViewerJobs {
    port: Arc<dyn JobPort>,
    lifecycle: JobLifecycle,
    pub(in crate::view) previews: super::preview_gate::PreviewGate,
}

impl Default for ViewerJobs {
    fn default() -> Self {
        Self { port: Arc::new(AdmissionJobPort::default()), lifecycle: JobLifecycle::default(), previews: super::preview_gate::PreviewGate::default() }
    }
}

/// A cloneable admission handle: port, lifecycle, attribution (actor, resource) and the correlation
/// source every submitted job takes its own correlation from (clones share it).
#[derive(Clone)]
pub struct JobAdmission {
    port: Arc<dyn JobPort>,
    lifecycle: JobLifecycle,
    actor: ActorContext,
    resource: Option<DomainId>,
    /// The document revision when the handle was made (0 with nothing open).
    revision: u64,
    correlations: Arc<dyn CorrelationSource>,
}

/// The viewer's run context as a correlation source: each `next` is the next correlation of the
/// run's source (the one its commands take theirs from).
struct RunCorrelations(hsk_studio_harness_core::RunContext);

impl CorrelationSource for RunCorrelations {
    fn next(&self) -> CorrelationId {
        self.0.command(None, 0, None).correlation()
    }
}

impl std::fmt::Debug for JobAdmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobAdmission").field("lifecycle", &self.lifecycle).field("resource", &self.resource).finish_non_exhaustive()
    }
}

impl JobAdmission {
    /// Jobs take their correlations from a fresh in-memory source until [`Self::with_correlations`]
    /// binds the caller's.
    pub fn new(port: Arc<dyn JobPort>, lifecycle: JobLifecycle, actor: ActorContext, resource: Option<DomainId>) -> Self {
        Self { port, lifecycle, actor, resource, revision: 0, correlations: Arc::new(TransientCorrelation::new()) }
    }

    /// Jobs take their correlations from `source` (the viewer binds its run context's).
    #[must_use]
    pub fn with_correlations(mut self, source: Arc<dyn CorrelationSource>) -> Self {
        self.correlations = source;
        self
    }

    /// The correlation of the next submitted job (one per job, R48 F-R48-3).
    pub fn next_correlation(&self) -> u64 {
        self.correlations.next().value()
    }

    /// The document revision synchronous jobs are bound to (they cannot outlive it).
    #[must_use]
    pub fn with_revision(mut self, revision: u64) -> Self {
        self.revision = revision;
        self
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// An asset-local admission (session `AdmissionJobPort` and a fresh lifecycle) for `actor`.
    pub fn asset_local(actor: ActorContext, resource: Option<DomainId>) -> Self {
        Self::new(Arc::new(AdmissionJobPort::default()), JobLifecycle::default(), actor, resource)
    }

    pub fn lifecycle(&self) -> &JobLifecycle {
        &self.lifecycle
    }

    pub fn actor(&self) -> &ActorContext {
        &self.actor
    }

    /// The request of one committed job of `action` taken from `bound_revision`.
    pub fn request(&self, action: Action, bytes: u64, bound_revision: u64, correlation_id: u64) -> JobRequest {
        JobRequest { resource: self.resource.clone(), action, actor: self.actor.clone(), cost: Cost { jobs: 1, bytes }, bound_revision, correlation_id, kind: JobKind::Committed }
    }

    /// Admits `request` while the document head is `head_revision`.
    pub fn admit(&self, request: JobRequest, head_revision: u64, cancel: &CancellationToken) -> Result<AdmittedJob, JobAdmissionRefusal> {
        self.lifecycle.admit(&self.port, request, head_revision, cancel)
    }
}

/// Settlement JSON: `{outcome, ticket, bound_revision, correlation, frame_correlation?,
/// head_revision?}` (codes and numbers only; `correlation` is the job's, `frame_correlation` the one
/// its delivered observe frame carried).
pub fn settlement_json(s: &JobSettlement) -> Value {
    let mut v = json!({"outcome": s.outcome.code(), "ticket": s.ticket.map(|t| t.0), "bound_revision": s.bound_revision, "correlation": s.correlation_id});
    if let hsk_studio_session::ReportStatus::Delivered(receipt) = &s.report {
        v["frame_correlation"] = json!(receipt.correlation_id);
    }
    match s.outcome {
        JobOutcome::CompletedStale { head, .. } => v["head_revision"] = json!(head),
        JobOutcome::Superseded { revision } => v["superseding_revision"] = json!(revision),
        JobOutcome::Failed(code) => v["code"] = json!(code),
        _ => {}
    }
    v
}

/// Refusal JSON: `{refused, retry_after_ms?, expected?, head?}`.
pub fn refusal_json(r: JobAdmissionRefusal) -> Value {
    match r {
        JobAdmissionRefusal::Overloaded { retry_after_ms } => json!({"refused": r.code(), "retry_after_ms": retry_after_ms}),
        JobAdmissionRefusal::Canceled => json!({"refused": r.code()}),
        JobAdmissionRefusal::StaleRevision { expected, head } => json!({"refused": r.code(), "expected": expected, "head": head}),
        JobAdmissionRefusal::DeadlineExceeded => json!({"refused": r.code()}),
    }
}

/// A status-log line for a settlement, `None` for a plain completion.
pub fn settlement_line(s: &JobSettlement) -> Option<String> {
    match s.outcome {
        JobOutcome::CompletedStale { bound, head } => Some(format!("completed from revision {bound}; the document is now at revision {head} (stale result kept, not current)")),
        JobOutcome::Completed => None,
        other => Some(format!("job {}", other.code())),
    }
}

impl Viewer {
    /// Binds the host's Job Runtime port (module docs); later jobs admit through it.
    pub fn set_job_port(&mut self, port: Arc<dyn JobPort>) {
        self.jobs.port = port;
    }

    /// Terminal job frames go to `sink` (codes and counters only).
    pub fn set_job_sink(&mut self, sink: Box<dyn SinkPort>) {
        self.jobs.lifecycle.set_sink(sink);
    }

    /// Counters of every job outcome since the viewer started.
    pub fn job_counts(&self) -> JobCounts {
        self.jobs.lifecycle.counts()
    }

    /// The document head revision (0 with nothing open).
    pub(in crate::view) fn document_head(&self) -> u64 {
        self.run.snapshot().map_or(0, |s| s.document().revision)
    }

    /// The admission handle for a job of the open document (none open: resource-less).
    pub(in crate::view) fn job_admission(&self) -> JobAdmission {
        let resource = self.run.snapshot().and_then(|s| DomainId::parse(&s.document().document_id).ok());
        JobAdmission::new(Arc::clone(&self.jobs.port), self.jobs.lifecycle.clone(), self.run.actor().clone(), resource)
            .with_revision(self.document_head())
            .with_correlations(Arc::new(RunCorrelations(self.run.context().clone())))
    }

    /// Records a replaceable preview at `bound_revision` superseded by `revision` (counted and
    /// emitted; nothing was admitted).
    pub(in crate::view) fn record_superseded_preview(&self, bound_revision: u64, revision: u64) -> JobSettlement {
        let admission = self.job_admission();
        let mut request = admission.request(Action::Edit, 0, bound_revision, admission.next_correlation());
        request.kind = JobKind::Replaceable;
        self.jobs.lifecycle.record_superseded(&request, revision)
    }
}

/// A reel export queue whose items are admitted before they are submitted (module docs).
#[cfg(feature = "media")]
pub struct AdmittedExportQueue {
    queue: hsk_studio_reel::ExportQueue,
    admission: JobAdmission,
    pending: ExportSettlements,
}

/// Admitted items whose receipts have not arrived, keyed by reel item id.
#[cfg(feature = "media")]
#[derive(Default)]
pub struct ExportSettlements {
    pending: Vec<(u64, AdmittedJob)>,
}

#[cfg(feature = "media")]
impl AdmittedExportQueue {
    pub fn new(queue: hsk_studio_reel::ExportQueue, admission: JobAdmission) -> Self {
        Self { queue, admission, pending: ExportSettlements::default() }
    }

    /// Admits one export of `bytes` working set taken from `bound_revision` while the head is
    /// `head_revision`, then submits it; a refusal submits nothing.
    pub fn submit(&mut self, job: hsk_studio_reel::ExportJob, bytes: u64, bound_revision: u64, head_revision: u64, cancel: &CancellationToken) -> Result<hsk_studio_reel::ExportHandle, JobAdmissionRefusal> {
        let correlation = self.admission.next_correlation();
        let admitted = self.admission.admit(self.admission.request(Action::Export, bytes, bound_revision, correlation), head_revision, cancel)?;
        let handle = self.queue.submit(job);
        self.pending.pending.push((handle.id, admitted));
        Ok(handle)
    }

    /// Runs every submitted item on this thread and settles each from its receipt, in run order;
    /// `head_revision` is read after the run.
    pub fn run(&mut self, head_revision: impl FnOnce() -> u64) -> Vec<(hsk_studio_reel::ExportReceipt, JobSettlement)> {
        let receipts = self.queue.run();
        let head = head_revision();
        let settlements = std::mem::take(&mut self.pending).settle(&receipts, head);
        receipts.into_iter().zip(settlements).collect()
    }

    /// The queue for a worker thread and the settlements that stay with the caller.
    pub fn into_worker(self) -> (hsk_studio_reel::ExportQueue, ExportSettlements) {
        (self.queue, self.pending)
    }
}

#[cfg(feature = "media")]
impl ExportSettlements {
    /// Admitted items not yet settled.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// The port tickets still held (resource-less jobs hold none).
    pub fn tickets(&self) -> Vec<u64> {
        self.pending.iter().filter_map(|(_, job)| job.ticket().map(|t| t.0)).collect()
    }

    /// Settles every pending item from its receipt (by item id, in submission order); an item
    /// without a receipt settles `failed` `export_worker`.
    pub fn settle(self, receipts: &[hsk_studio_reel::ExportReceipt], head_revision: u64) -> Vec<JobSettlement> {
        self.pending
            .into_iter()
            .map(|(id, job)| match receipts.iter().find(|r| r.item_id == id) {
                Some(receipt) => settle_export(job, receipt, head_revision),
                None => job.settle(JobOutcome::Failed("export_worker")),
            })
            .collect()
    }

    /// Settles every pending item `failed` with `code` (the worker never ran).
    pub fn fail(self, code: &'static str) -> Vec<JobSettlement> {
        self.pending.into_iter().map(|(_, job)| job.settle(JobOutcome::Failed(code))).collect()
    }
}

/// Settles one export from its receipt.
#[cfg(feature = "media")]
pub fn settle_export(job: AdmittedJob, receipt: &hsk_studio_reel::ExportReceipt, head_revision: u64) -> JobSettlement {
    match &receipt.outcome {
        hsk_studio_reel::ExportOutcome::Complete => job.complete(head_revision),
        hsk_studio_reel::ExportOutcome::Canceled => job.settle(JobOutcome::Canceled),
        // `*code` is the `&'static str` the outcome needs; the lint's `code` would be a borrow-scoped
        // `&str` (deref coercion of `&&'static str`) and does not compile (R48 F-R48-8).
        #[allow(clippy::explicit_auto_deref)]
        hsk_studio_reel::ExportOutcome::Failed { code, .. } => job.settle(JobOutcome::Failed(*code)),
    }
}

/// A refusal as the viewer's typed failure (`overloaded`, `canceled`, `stale_revision`).
#[cfg(feature = "media")]
pub fn refusal_fail(r: JobAdmissionRefusal) -> MonitorFail {
    MonitorFail { code: r.code(), detail: refusal_json(r).to_string() }
}

/// Working-set estimate of one export frame: `width * height` RGBA f32 (16 bytes per pixel),
/// saturating.
pub fn frame_bytes(width: u32, height: u32) -> u64 {
    u64::from(width).saturating_mul(u64::from(height)).saturating_mul(16)
}
