//! Lane SMB-FIX-EXPORT-ADMISSION-1 (feature `viewer`; validation round R60 attempt 3 probes e1 and e2, ported from
//! the validator's `r60a3_e_first_byte_probe.rs` and `r60a3_e_batch_port_probe.rs`): the batch per-target export
//! (`RecipeTargets`) asks its port once per target before anything reaches the destination, the checkpoint temp
//! included; a one-slot port bound as both `BatchPorts::jobs` and the exporter's port completes a batch (the runner
//! never holds its slot while a target's export holds one); a refused target leaves no folder the run created and
//! keeps a folder that already existed (HBR-HOST-004, STU-ARC-008, STU-AUT-126, STU-AUT-129).
//!
//! Oracles: port doubles that list the destination when asked, or hold one slot and count asks and releases; the
//! destination's listing after the run.
#![cfg(feature = "viewer")]

use hsk_studio_accord::CancellationToken;
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::doc_export::RecipeTargets;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness_core::batch::{
    Automation, BatchJob, BatchPorts, BatchReport, ErrorPolicy, OutputTarget, OverwritePolicy, SourceSet, TargetExporter, TemplateToken, run_batch,
};
use hsk_studio_harness_core::document_new::{DocumentProfile, NewDocument};
use hsk_studio_harness_core::{Harness, Selection, asset_local_test_context};
use hsk_studio_session::{Cost, JobPort, JobRefused, JobScope, JobTicket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Test scratch directory under TMP, removed on drop.
struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hsk-fix-export-admission-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    /// A saved 64 x 64 vector document, the batch input.
    fn input(&self) -> PathBuf {
        let input = self.0.join("in.handshake");
        let mut v = Viewer::new(Prefer::Cpu);
        v.new_document(&NewDocument::new(DocumentProfile::Vector, Some((64.0, 64.0))), None).unwrap();
        v.save(&input, false).unwrap();
        input
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

fn harness() -> Harness {
    Harness::new(Selection { providers: vec![hsk_studio_session::CHRONICLE_PROVIDER_ID.to_string()], ..Selection::default() }).unwrap()
}

fn target(format_id: &str, destination: &Path) -> OutputTarget {
    OutputTarget {
        format_id: format_id.into(),
        export_recipe_id: None,
        destination: destination.to_string_lossy().into_owned(),
        colour_profile_policy: None,
        resize_policy: None,
        overwrite_policy: OverwritePolicy::Rename,
    }
}

fn job(input: &Path, output_targets: Vec<OutputTarget>) -> BatchJob {
    BatchJob {
        job_id: "fix-export-admission".into(),
        source_set: SourceSet::ExplicitFileList(vec![input.to_string_lossy().into_owned()]),
        applied_automation: Automation::None,
        output_targets,
        filename_template: vec![TemplateToken::OriginalFilename],
        error_policy: ErrorPolicy::SkipAndContinue,
        concurrency: 1,
        post_processing: vec![],
    }
}

/// Runs `job` with `exporter` and the optional runner port.
fn run(job: &BatchJob, exporter: &mut RecipeTargets, jobs: Option<Arc<dyn JobPort>>) -> BatchReport {
    let mut ports = BatchPorts::new(asset_local_test_context());
    ports.jobs = jobs;
    ports.exporter = Some(exporter);
    run_batch(&harness(), job, ports, &CancellationToken::default()).unwrap()
}

/// Refuses every ask; records what `watched` holds at each ask (`None` when it does not exist).
struct Watching {
    watched: PathBuf,
    seen: Mutex<Vec<Option<Vec<String>>>>,
}
impl Watching {
    fn new(watched: &Path) -> Arc<Self> {
        Arc::new(Self { watched: watched.to_path_buf(), seen: Mutex::new(Vec::new()) })
    }
    fn seen(&self) -> Vec<Option<Vec<String>>> {
        self.seen.lock().unwrap().clone()
    }
}
impl JobPort for Watching {
    fn admit(&self, _scope: &JobScope<'_>, _cost: Cost) -> Result<JobTicket, JobRefused> {
        let names = self.watched.is_dir().then(|| listing(&self.watched));
        self.seen.lock().unwrap().push(names);
        Err(JobRefused { retry_after_ms: 5 })
    }
    fn release(&self, _ticket: JobTicket) {}
}

/// One slot: admits while nothing is held, else refuses; counts asks, refusals and releases.
#[derive(Default)]
struct OneSlot {
    held: AtomicU64,
    asks: AtomicU64,
    refused: AtomicU64,
    released: AtomicU64,
}
impl OneSlot {
    fn counts(&self) -> (u64, u64, u64) {
        (self.asks.load(Ordering::SeqCst), self.refused.load(Ordering::SeqCst), self.released.load(Ordering::SeqCst))
    }
}
impl JobPort for OneSlot {
    fn admit(&self, _scope: &JobScope<'_>, _cost: Cost) -> Result<JobTicket, JobRefused> {
        let n = self.asks.fetch_add(1, Ordering::SeqCst) + 1;
        if self.held.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
            Ok(JobTicket(n))
        } else {
            self.refused.fetch_add(1, Ordering::SeqCst);
            Err(JobRefused { retry_after_ms: 5 })
        }
    }
    fn release(&self, _ticket: JobTicket) {
        self.released.fetch_add(1, Ordering::SeqCst);
        self.held.store(0, Ordering::SeqCst);
    }
}

/// Probe e1 (HIGH batch-target-bytes-before-admission): a port bound with `with_job_port` is asked once, while
/// the destination folder is still empty (no checkpoint temp), and its refusal is the target's typed code with
/// nothing left behind.
#[test]
fn e1_a_batch_target_asks_its_port_before_any_byte_reaches_the_destination() {
    let s = Scratch::new("e1");
    let input = s.input();
    let out = s.0.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let h = harness();
    let cancel = CancellationToken::default();
    let mut run = h.run(asset_local_test_context(), &cancel);
    assert!(run.open_path(&input));
    let port = Watching::new(&out);
    let mut targets = RecipeTargets::new(asset_local_test_context()).with_job_port(port.clone());
    let r = targets.write(&mut run, &target("png", &out), &out.join("x.png"), false);
    assert_eq!(r, Err("overloaded".to_string()), "a saturated port refuses the target typed");
    assert_eq!(port.seen().len(), 1, "the target asks its port exactly once");
    assert_eq!(port.seen(), [Some(Vec::new())], "bytes reached the destination before the port was asked");
    assert!(listing(&out).is_empty(), "nothing left behind: {:?}", listing(&out));
}

/// Probe e2 (MEDIUM batch-double-admission-self-starvation): one one-slot port bound as `BatchPorts::jobs` and as
/// the exporter's port completes a one-input, one-target batch: the runner's ask, then the target's (2 asks, never
/// 2 slots), every ticket released.
#[test]
fn e2_one_slot_port_as_runner_and_exporter_port_completes_a_one_target_batch() {
    let s = Scratch::new("e2");
    let input = s.input();
    let out = s.0.join("out");
    let port = Arc::new(OneSlot::default());
    let mut targets = RecipeTargets::new(asset_local_test_context()).with_job_port(port.clone());
    let report = run(&job(&input, vec![target("png", &out)]), &mut targets, Some(port.clone()));
    let t = &report.receipts[0].targets[0];
    assert_eq!((t.outcome, t.code.as_deref()), ("written", None), "{}", report.to_json());
    assert!(out.join("in.png").is_file());
    assert_eq!(port.counts(), (2, 0, 2), "asks, refusals, releases: the runner's ask then the target's");
    assert_eq!(listing(&out), ["in.png"]);
}

/// Native, png, native on one one-slot port: all written, none refused. Count changed by the lead's F2 ruling on
/// the review of 54bc63fd (rule 4a, 2026-11-13; it was 3 asks, runner / target / runner again): an input's native
/// targets are written first under the runner's slot, so there is no second runner ask: 2 asks (runner, target).
#[test]
fn a_native_target_after_an_exporter_target_asks_the_runner_slot_again() {
    let s = Scratch::new("mixed");
    let input = s.input();
    let (a, b, c) = (s.0.join("a"), s.0.join("b"), s.0.join("c"));
    let port = Arc::new(OneSlot::default());
    let mut targets = RecipeTargets::new(asset_local_test_context()).with_job_port(port.clone());
    let report = run(&job(&input, vec![target("handshake", &a), target("png", &b), target("handshake", &c)]), &mut targets, Some(port.clone()));
    let outcomes: Vec<_> = report.receipts[0].targets.iter().map(|t| (t.outcome, t.code.clone())).collect();
    assert_eq!(outcomes, [("written", None), ("written", None), ("written", None)], "{}", report.to_json());
    assert_eq!(port.counts(), (2, 0, 2), "asks, refusals, releases");
    assert_eq!((listing(&a), listing(&b), listing(&c)), (vec!["in.handshake".to_string()], vec!["in.png".to_string()], vec!["in.handshake".to_string()]));
}

/// A refused target removes the destination folders this run created for it (lead ruling 2026-11-13): the target's
/// ask is refused, `new/deeper` (both made by the runner) are gone, the scratch folder that existed stays.
#[test]
fn a_refused_target_removes_the_destination_folders_the_run_created() {
    let s = Scratch::new("created");
    let input = s.input();
    let new = s.0.join("new");
    let out = new.join("deeper");
    let port = Watching::new(&out);
    let mut targets = RecipeTargets::new(asset_local_test_context()).with_job_port(port.clone());
    let report = run(&job(&input, vec![target("png", &out)]), &mut targets, None);
    let t = &report.receipts[0].targets[0];
    assert_eq!((t.outcome, t.code.as_deref()), ("failed", Some("overloaded")), "{}", report.to_json());
    assert_eq!(port.seen().len(), 1, "the target asks its port exactly once");
    assert!(!new.exists(), "the folders the run created are removed: {:?}", listing(&s.0));
    assert!(s.0.is_dir() && input.is_file(), "the folder that existed stays with its files");
}

/// A refused target keeps a destination folder that already existed, empty as it was: nothing reached it before
/// the ask and nothing is left after.
#[test]
fn a_refused_target_keeps_a_destination_folder_that_already_existed() {
    let s = Scratch::new("existing");
    let input = s.input();
    let out = s.0.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let port = Watching::new(&out);
    let mut targets = RecipeTargets::new(asset_local_test_context()).with_job_port(port.clone());
    let report = run(&job(&input, vec![target("png", &out)]), &mut targets, None);
    let t = &report.receipts[0].targets[0];
    assert_eq!((t.outcome, t.code.as_deref()), ("failed", Some("overloaded")), "{}", report.to_json());
    assert_eq!(port.seen(), [Some(Vec::new())], "asked once, with the existing folder still empty");
    assert!(out.is_dir(), "a folder that already existed is never removed");
    assert!(listing(&out).is_empty(), "nothing left behind: {:?}", listing(&out));
}

/// Focused: a target that fails after its ask (the second write to one path without replace is
/// `destination_exists`) settles once and releases its ticket, and no checkpoint temp is left; the first settles
/// `completed` into the report's `job`.
#[test]
fn a_target_failing_after_its_ask_settles_once_and_leaves_no_temp() {
    let s = Scratch::new("after-ask");
    let input = s.input();
    let out = s.0.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let h = harness();
    let cancel = CancellationToken::default();
    let mut run = h.run(asset_local_test_context(), &cancel);
    assert!(run.open_path(&input));
    let port = Arc::new(OneSlot::default());
    let mut targets = RecipeTargets::new(asset_local_test_context()).with_job_port(port.clone());
    let path = out.join("x.png");
    targets.write(&mut run, &target("png", &out), &path, false).unwrap();
    assert_eq!(targets.reports[0].1["job"]["outcome"], "completed", "{}", targets.reports[0].1);
    assert_eq!(targets.write(&mut run, &target("png", &out), &path, false), Err("destination_exists".to_string()));
    assert_eq!(port.counts(), (2, 0, 2), "asks, refusals, releases: one ask per target, each released");
    assert_eq!(listing(&out), ["x.png"], "no checkpoint temp is left");
}

// Retired by the lead's F2 ruling on the review of 54bc63fd (2026-11-13): 54bc63fd's
// `a_refused_second_runner_ask_fails_the_native_target_before_its_folder_is_made` pinned the runner's second ask,
// which no longer exists; `f2_contention_cannot_fail_a_native_target_declared_after_an_exporter_target` replaces it.

// Review of 54bc63fd (research/review-smb-fix-export-admission-1-54bc63fd.json), lead rulings 2026-11-13: F1 to F3.

/// Runs `job` with any exporter, the optional runner port and the batch's `cancel`.
fn run_on(job: &BatchJob, exporter: &mut dyn TargetExporter, jobs: Option<Arc<dyn JobPort>>, cancel: &CancellationToken) -> BatchReport {
    let mut ports = BatchPorts::new(asset_local_test_context());
    ports.jobs = jobs;
    ports.exporter = Some(exporter);
    run_batch(&harness(), job, ports, cancel).unwrap()
}

/// An exporter over `RecipeTargets` that records, at each write, how many slots of `port` are held.
struct HeldAtWrite {
    inner: RecipeTargets,
    port: Arc<OneSlot>,
    held: Vec<u64>,
}
impl TargetExporter for HeldAtWrite {
    fn extension(&self, target: &OutputTarget) -> Result<&'static str, &'static str> {
        self.inner.extension(target)
    }
    fn write(&mut self, run: &mut hsk_studio_harness_core::Run<'_>, target: &OutputTarget, path: &Path, replace: bool) -> Result<(), String> {
        self.held.push(self.port.held.load(Ordering::SeqCst));
        self.inner.write(run, target, path, replace)
    }
}

/// F1: with the host port bound only as `BatchPorts::jobs` and an exporter that asks another port (`RecipeTargets`
/// without `with_job_port`), the runner keeps its slot through the write: the export runs under one slot of the host
/// port, the host port is asked once and never holds two.
#[test]
fn f1_an_exporter_asking_another_port_writes_under_the_runners_slot() {
    let s = Scratch::new("f1-held");
    let input = s.input();
    let out = s.0.join("out");
    let port = Arc::new(OneSlot::default());
    let mut exporter = HeldAtWrite { inner: RecipeTargets::new(asset_local_test_context()), port: port.clone(), held: Vec::new() };
    let report = run_on(&job(&input, vec![target("png", &out)]), &mut exporter, Some(port.clone()), &CancellationToken::default());
    let t = &report.receipts[0].targets[0];
    assert_eq!((t.outcome, t.code.as_deref()), ("written", None), "{}", report.to_json());
    assert_eq!(exporter.held, [1], "the write runs under the runner's slot of the host port");
    assert_eq!(port.counts(), (1, 0, 1), "asks, refusals, releases on the host port");
}

/// Records every ask's action and cost, admits all.
#[derive(Default)]
struct Costed {
    asks: Mutex<Vec<(hsk_studio_session::Action, Cost)>>,
}
impl JobPort for Costed {
    fn admit(&self, scope: &JobScope<'_>, cost: Cost) -> Result<JobTicket, JobRefused> {
        let mut asks = self.asks.lock().unwrap();
        asks.push((scope.action, cost));
        Ok(JobTicket(asks.len() as u64))
    }
    fn release(&self, _ticket: JobTicket) {}
}

/// F1: the target's export job is charged the input's bytes, as the runner's slot it takes over is.
#[test]
fn f1_the_target_job_is_charged_the_inputs_bytes() {
    let s = Scratch::new("f1-bytes");
    let input = s.input();
    let bytes = std::fs::metadata(&input).unwrap().len();
    let port = Arc::new(Costed::default());
    let mut targets = RecipeTargets::new(asset_local_test_context()).with_job_port(port.clone());
    let report = run(&job(&input, vec![target("png", &s.0.join("out"))]), &mut targets, Some(port.clone()));
    assert_eq!(report.receipts[0].targets[0].outcome, "written", "{}", report.to_json());
    let cost = Cost { jobs: 1, bytes };
    assert_eq!(*port.asks.lock().unwrap(), [(hsk_studio_session::Action::Edit, cost), (hsk_studio_session::Action::Export, cost)]);
}

/// One slot; when the ticket numbered `grab_after` is released, a competing job takes the slot and keeps it.
struct Contended {
    held: AtomicU64,
    asks: AtomicU64,
    grab_after: u64,
}
impl JobPort for Contended {
    fn admit(&self, _scope: &JobScope<'_>, _cost: Cost) -> Result<JobTicket, JobRefused> {
        let n = self.asks.fetch_add(1, Ordering::SeqCst) + 1;
        if self.held.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst).is_ok() { Ok(JobTicket(n)) } else { Err(JobRefused { retry_after_ms: 5 }) }
    }
    fn release(&self, ticket: JobTicket) {
        if ticket.0 != self.grab_after {
            self.held.store(0, Ordering::SeqCst);
        }
    }
}

/// F2: an input's native targets are written before its exporter targets, under the runner's slot, so a competing
/// job that takes the slot once the PNG target releases it cannot fail the native target declared after it; the
/// receipts keep the declared order.
#[test]
fn f2_contention_cannot_fail_a_native_target_declared_after_an_exporter_target() {
    let s = Scratch::new("f2-contended");
    let input = s.input();
    let (a, b, c) = (s.0.join("a"), s.0.join("b"), s.0.join("c"));
    let port = Arc::new(Contended { held: AtomicU64::new(0), asks: AtomicU64::new(0), grab_after: 2 });
    let mut targets = RecipeTargets::new(asset_local_test_context()).with_job_port(port.clone());
    let report = run(&job(&input, vec![target("handshake", &a), target("png", &b), target("handshake", &c)]), &mut targets, Some(port.clone()));
    let receipts: Vec<_> = report.receipts[0].targets.iter().map(|t| (t.format_id.clone(), t.outcome, t.path.clone())).collect();
    let path = |dir: &Path, name: &str| Some(dir.join(name).to_string_lossy().into_owned());
    assert_eq!(
        receipts,
        [("handshake".to_string(), "written", path(&a, "in.handshake")), ("png".to_string(), "written", path(&b, "in.png")), ("handshake".to_string(), "written", path(&c, "in.handshake"))],
        "{}",
        report.to_json()
    );
    assert_eq!(port.asks.load(Ordering::SeqCst), 2, "the runner's ask and the PNG target's");
}

/// Admits every ask; cancels `cancel` on the release of `cancel_on_release` or the ask numbered `cancel_on_ask`.
struct Canceling {
    cancel: CancellationToken,
    cancel_on_release: bool,
    cancel_on_ask: u64,
    asks: AtomicU64,
    released: AtomicU64,
}
impl Canceling {
    fn new(cancel: &CancellationToken, cancel_on_release: bool, cancel_on_ask: u64) -> Arc<Self> {
        Arc::new(Self { cancel: cancel.clone(), cancel_on_release, cancel_on_ask, asks: AtomicU64::new(0), released: AtomicU64::new(0) })
    }
    fn counts(&self) -> (u64, u64) {
        (self.asks.load(Ordering::SeqCst), self.released.load(Ordering::SeqCst))
    }
}
impl JobPort for Canceling {
    fn admit(&self, _scope: &JobScope<'_>, _cost: Cost) -> Result<JobTicket, JobRefused> {
        let n = self.asks.fetch_add(1, Ordering::SeqCst) + 1;
        if n == self.cancel_on_ask {
            self.cancel.cancel();
        }
        Ok(JobTicket(n))
    }
    fn release(&self, _ticket: JobTicket) {
        self.released.fetch_add(1, Ordering::SeqCst);
        if self.cancel_on_release {
            self.cancel.cancel();
        }
    }
}

/// F3: the batch is canceled while the runner lends its slot (one port as runner and exporter port, whose release
/// cancels): the target's ask refuses `canceled` without asking, and the input is cancelled, not failed.
#[test]
fn f3_a_cancel_while_the_runner_lends_its_slot_cancels_the_input_without_a_second_ask() {
    let s = Scratch::new("f3-lend");
    let input = s.input();
    let cancel = CancellationToken::default();
    let port = Canceling::new(&cancel, true, 0);
    let mut targets = RecipeTargets::new(asset_local_test_context()).with_job_port(port.clone());
    let report = run_on(&job(&input, vec![target("png", &s.0.join("out"))]), &mut targets, Some(port.clone()), &cancel);
    let r = &report.receipts[0];
    assert_eq!((r.targets[0].outcome, r.targets[0].code.as_deref()), ("failed", Some("canceled")), "{}", report.to_json());
    assert_eq!((r.outcome, report.stopped), ("cancelled", Some("cancelled")), "{}", report.to_json());
    assert_eq!(port.counts(), (1, 1), "the runner's ask only; its slot released once");
}

/// F3: the batch is canceled right after the target's ask is admitted: the target stops on the batch's token, settles
/// `canceled` (not `batch_checkpoint_failed`, not written) and the input is cancelled.
#[test]
fn f3_a_cancel_after_the_targets_ask_settles_the_target_canceled() {
    let s = Scratch::new("f3-after-ask");
    let input = s.input();
    let out = s.0.join("out");
    let cancel = CancellationToken::default();
    let port = Canceling::new(&cancel, false, 2);
    let mut targets = RecipeTargets::new(asset_local_test_context()).with_job_port(port.clone());
    let report = run_on(&job(&input, vec![target("png", &out)]), &mut targets, Some(port.clone()), &cancel);
    let r = &report.receipts[0];
    assert_eq!((r.targets[0].outcome, r.targets[0].code.as_deref()), ("failed", Some("canceled")), "{}", report.to_json());
    assert_eq!(r.outcome, "cancelled", "{}", report.to_json());
    assert_eq!(port.counts(), (2, 2), "the runner's ask and the target's, each released once");
    assert!(!out.exists(), "nothing is left: {:?}", listing(&s.0));
}

/// Writes a file into `into` when asked, then refuses.
struct Intruding {
    into: PathBuf,
}
impl JobPort for Intruding {
    fn admit(&self, _scope: &JobScope<'_>, _cost: Cost) -> Result<JobTicket, JobRefused> {
        std::fs::write(self.into.join("late.txt"), b"late").unwrap();
        Err(JobRefused { retry_after_ms: 5 })
    }
    fn release(&self, _ticket: JobTicket) {}
}

/// F5 (data-loss guard): after a refusal, a pre-existing folder holding a file and a pre-existing empty folder stay
/// with their content, a folder the run made that gained a file during the ask stays with that file, and only the
/// run's empty folder below it is removed (`remove_dir`, never a recursive removal).
#[test]
fn f5_a_refusal_never_removes_a_folder_with_content_or_one_that_existed() {
    let s = Scratch::new("f5-guard");
    let input = s.input();
    let kept = s.0.join("kept");
    let empty = kept.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    std::fs::write(kept.join("keep.txt"), b"keep").unwrap();
    let made = empty.join("made");
    let deeper = made.join("deeper");
    let port = Arc::new(Intruding { into: made.clone() });
    let mut targets = RecipeTargets::new(asset_local_test_context()).with_job_port(port.clone());
    let report = run(&job(&input, vec![target("png", &deeper)]), &mut targets, None);
    let t = &report.receipts[0].targets[0];
    assert_eq!((t.outcome, t.code.as_deref()), ("failed", Some("overloaded")), "{}", report.to_json());
    assert_eq!(std::fs::read(kept.join("keep.txt")).unwrap(), b"keep", "a pre-existing folder keeps its file");
    assert!(empty.is_dir(), "a pre-existing empty folder stays");
    assert_eq!(listing(&made), ["late.txt"], "a made folder that gained content stays with it; its empty child is removed");
}

/// F5 (settlement): a target failing after its ask settles explicitly `failed` with its code, recorded in
/// `RecipeTargets::failed`; a job dropped unsettled would read `abandoned` and never reach that list.
#[test]
fn f5_a_failed_target_settles_explicitly_failed_not_abandoned() {
    let s = Scratch::new("f5-settle");
    let input = s.input();
    let out = s.0.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let h = harness();
    let cancel = CancellationToken::default();
    let mut run = h.run(asset_local_test_context(), &cancel);
    assert!(run.open_path(&input));
    let port = Arc::new(OneSlot::default());
    let mut targets = RecipeTargets::new(asset_local_test_context()).with_job_port(port.clone());
    let path = out.join("x.png");
    targets.write(&mut run, &target("png", &out), &path, false).unwrap();
    assert!(targets.failed.is_empty());
    assert_eq!(targets.write(&mut run, &target("png", &out), &path, false), Err("destination_exists".to_string()));
    assert_eq!(targets.failed.len(), 1, "{:?}", targets.failed);
    let (failed_path, failure) = &targets.failed[0];
    assert_eq!(failed_path, &path);
    assert_eq!((failure["code"].as_str(), failure["job"]["outcome"].as_str(), failure["job"]["code"].as_str()), (Some("destination_exists"), Some("failed"), Some("destination_exists")), "{failure}");
    assert_eq!(port.counts(), (2, 0, 2));
}
