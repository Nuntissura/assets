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

/// The runner's slot is asked again only before a native target that follows an exporter target: native, png,
/// native on one one-slot port is 3 asks (runner, target, runner again), none refused, all written.
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
    assert_eq!(port.counts(), (3, 0, 3), "asks, refusals, releases");
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
