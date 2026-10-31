//! Lane SMB-PS-ACTIONS-1 slice 3: the batch runner over documents ([STU-AUT-126]..[STU-AUT-136],
//! [STU-AUT-186], [STU-AUT-187]). Inputs are copies of the harness fixture
//! `tests/fixtures/demo.handshake`; every file lives under `std::env::temp_dir()`.

use hsk_studio_accord::CancellationToken;
use hsk_studio_harness_core::action::{ActionStep, DialogOption, UnitToken};
use hsk_studio_harness_core::batch::{self, Automation, BatchPorts, ErrorPolicy, JobState, OutputTarget, OverwritePolicy, SourceSet, TemplateToken};
use hsk_studio_harness_core::{ActionRecord, BatchJob, Harness, RecordedValue, Selection};
mod support;
use support::test_context;
use hsk_studio_session::{Cost, JobPort, JobRefused, JobScope, JobTicket};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

const FIXTURE: &[u8] = include_bytes!("../../hsk-studio-harness/tests/fixtures/demo.handshake");
const L1: &str = "SLYR-019abcde-0000-7000-8000-000000000001";
const L2: &str = "SLYR-019abcde-0000-7000-8000-000000000002";
const ACT: &str = "SACT-019abcde-0000-7000-8000-0000000000b1";

/// One scratch directory per test under TMP, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hsk-ps-actions-batch-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("in")).unwrap();
        Self(dir)
    }

    fn input(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let p = self.0.join("in").join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    fn dir(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn harness() -> Harness {
    Harness::new(Selection { providers: vec!["chronicle.patch".to_string()], ..Selection::default() }).unwrap()
}

fn step(index: u64, command: &str, params: Vec<(&str, RecordedValue)>) -> ActionStep {
    ActionStep {
        index,
        enabled: true,
        expanded: false,
        with_dialog: false,
        dialog_options: DialogOption::Silent,
        command_id: command.to_string(),
        display_name: command.to_string(),
        has_parameters: true,
        parameter_class: None,
        parameters: Some(params.into_iter().map(|(k, v)| (k.to_string(), v)).collect::<BTreeMap<_, _>>()),
    }
}

/// Rename L1 to "batched", L2 opacity 25 %.
fn the_action() -> ActionRecord {
    let steps = vec![
        step(0, "layer.rename", vec![("layer", RecordedValue::Reference(L1.into())), ("name", RecordedValue::String("batched".into()))]),
        step(1, "layer.set_opacity", vec![("layer", RecordedValue::Reference(L2.into())), ("opacity", RecordedValue::Measured { value: 25.0, unit: Some(UnitToken::Percent) })]),
    ];
    ActionRecord { action_id: ACT.into(), index: 0, name: "batch".into(), function_key_index: 0, shift_modifier: false, command_modifier: false, color_index: 0, expanded: false, declared_step_count: 2, steps }
}

fn target(dest: &Path, overwrite_policy: OverwritePolicy) -> OutputTarget {
    OutputTarget { format_id: "handshake".into(), export_recipe_id: None, destination: dest.to_string_lossy().into_owned(), colour_profile_policy: None, resize_policy: None, overwrite_policy }
}

fn job(source_set: SourceSet, targets: Vec<OutputTarget>, error_policy: ErrorPolicy) -> BatchJob {
    BatchJob {
        job_id: "job-1".into(),
        source_set,
        applied_automation: Automation::Action(the_action()),
        output_targets: targets,
        filename_template: vec![TemplateToken::OriginalFilename, TemplateToken::Custom("_out".into())],
        error_policy,
        concurrency: 1,
        post_processing: vec![],
    }
}

fn files(paths: &[&PathBuf]) -> SourceSet {
    SourceSet::ExplicitFileList(paths.iter().map(|p| p.to_string_lossy().into_owned()).collect())
}

/// Reopens an output in a fresh run and checks the action's values.
fn applied(h: &Harness, path: &Path) -> bool {
    let cancel = CancellationToken::default();
    let mut run = h.run(test_context(), &cancel);
    run.open_path(path) && run.verify_live(None, vec![(L1.into(), "name", json!("batched")), (L2.into(), "opacity", json!(25.0))])
}

#[test]
fn three_fixture_copies_give_three_outputs_with_the_action_applied() {
    let s = Scratch::new("three");
    for n in ["a", "b", "c"] {
        s.input(&format!("{n}.handshake"), FIXTURE);
    }
    s.input("notes.txt", b"not an input");
    let out = s.dir("out");
    let source = SourceSet::Folder { folder: s.dir("in").to_string_lossy().into_owned(), recurse: false, include: vec!["?.handshake".into()], exclude: vec!["*.txt".into()] };
    let h = harness();
    let cancel = CancellationToken::default();
    let mut states: Vec<JobState> = Vec::new();
    let mut on_state = |st: &JobState| states.push(st.clone());
    let report = batch::run_batch(&h, &job(source, vec![target(&out, OverwritePolicy::Rename)], ErrorPolicy::SkipAndContinue), BatchPorts { on_state: Some(&mut on_state), ..BatchPorts::new(test_context()) }, &cancel).unwrap();
    assert_eq!(report.receipts.len(), 3, "{}", report.to_json());
    for (r, n) in report.receipts.iter().zip(["a", "b", "c"]) {
        assert_eq!(r.outcome, "succeeded", "{}", r.to_json());
        assert!(r.input_path.ends_with(&format!("{n}.handshake")));
        assert_eq!(r.input_bytes, Some(FIXTURE.len() as u64));
        let digest = r.content_digest.as_ref().unwrap();
        assert!(digest["algorithm"] == "sha256" && digest["digest"].as_str().unwrap().len() == 64);
        assert!(r.automation["kind"] == "action" && r.automation["action_id"] == ACT && r.automation["steps"][1]["parameters"]["opacity"]["unit"] == "percent");
        let written = out.join(format!("{n}_out.handshake"));
        assert_eq!(r.targets[0].path.as_deref(), Some(&*written.to_string_lossy()));
        assert!(applied(&h, &written), "{n}: action applied in the output");
    }
    // Same input bytes, same digest; inputs untouched.
    assert_eq!(report.receipts[0].content_digest, report.receipts[2].content_digest);
    assert_eq!(std::fs::read(s.dir("in").join("a.handshake")).unwrap(), FIXTURE);
    assert!(report.state.done == 3 && report.state.total == 3 && report.state.succeeded == 3 && !report.state.cancelled && report.stopped.is_none());
    assert!(states.iter().any(|st| st.current_input.as_deref().is_some_and(|c| c.ends_with("b.handshake"))));
    assert_eq!(states.last().unwrap().done, 3);
    // Pattern law: `*` any run, `?` one character, literal otherwise.
    assert!(batch::pattern_matches("*.handshake", "a.handshake") && batch::pattern_matches("a?c*", "abc") && !batch::pattern_matches("?.handshake", "ab.handshake") && !batch::pattern_matches("*.txt", "a.handshake"));
    println!("NOT_PROVEN: ArtifactService manifest id and CKC registration per output (STU-AUT-129/132; the receipt carries the output path)");
    println!("NOT_PROVEN: EventLedger binding of batch receipts (STU-AUT-132)");
    println!("NOT_PROVEN: Photoshop Batch / Image Processor behaviour for folder patterns and sequential order (Studio reading, UNVERIFIED (D5))");
}

#[test]
fn collisions_rename_or_skip_and_unsupported_formats_refuse_per_target() {
    let s = Scratch::new("collide");
    let a = s.input("a.handshake", FIXTURE);
    let out = s.dir("out");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("a_out.handshake"), b"existing").unwrap();
    let h = harness();
    let cancel = CancellationToken::default();
    let r = batch::run_batch(&h, &job(files(&[&a]), vec![target(&out, OverwritePolicy::Rename)], ErrorPolicy::SkipAndContinue), BatchPorts::new(test_context()), &cancel).unwrap();
    let t = &r.receipts[0].targets[0];
    assert!(t.outcome == "written" && t.path.as_deref().unwrap().ends_with("a_out-1.handshake"), "{}", r.to_json());
    assert!(applied(&h, &out.join("a_out-1.handshake")));
    assert_eq!(std::fs::read(out.join("a_out.handshake")).unwrap(), b"existing");

    let r = batch::run_batch(&h, &job(files(&[&a]), vec![target(&out, OverwritePolicy::Skip)], ErrorPolicy::SkipAndContinue), BatchPorts::new(test_context()), &cancel).unwrap();
    assert_eq!(r.receipts[0].targets[0].outcome, "skipped_existing");
    assert_eq!(std::fs::read(out.join("a_out.handshake")).unwrap(), b"existing", "skip leaves the existing file alone");
    assert!(!out.join("a_out-2.handshake").exists());

    let mut png = target(&s.dir("png"), OverwritePolicy::Rename);
    png.format_id = "png".into();
    let r = batch::run_batch(&h, &job(files(&[&a]), vec![png, target(&s.dir("native"), OverwritePolicy::Rename)], ErrorPolicy::SkipAndContinue), BatchPorts::new(test_context()), &cancel).unwrap();
    let rc = &r.receipts[0];
    assert!(rc.targets[0].outcome == "refused" && rc.targets[0].code.as_deref() == Some("batch_output_format_unsupported"), "{}", rc.to_json());
    assert_eq!(rc.targets[1].outcome, "written");
    assert!(rc.outcome == "failed" && rc.error.as_ref().unwrap()["stage"] == "output");
}

#[test]
fn a_corrupt_input_fails_alone_or_stops_the_job() {
    let s = Scratch::new("corrupt");
    let a = s.input("a.handshake", FIXTURE);
    let bad = s.input("b.handshake", b"PK\x03\x04 this is not a package");
    let c = s.input("c.handshake", FIXTURE);
    let h = harness();
    let cancel = CancellationToken::default();
    let out = s.dir("out");
    let r = batch::run_batch(&h, &job(files(&[&a, &bad, &c]), vec![target(&out, OverwritePolicy::Rename)], ErrorPolicy::SkipAndContinue), BatchPorts::new(test_context()), &cancel).unwrap();
    let outcomes: Vec<&str> = r.receipts.iter().map(|x| x.outcome).collect();
    assert_eq!(outcomes, ["succeeded", "failed", "succeeded"], "{}", r.to_json());
    let failed = &r.receipts[1];
    assert_eq!(failed.error.as_ref().unwrap()["stage"], "open");
    assert!(failed.content_digest.is_none() && failed.digest_note.as_deref() == Some("input_not_opened") && failed.targets.is_empty());
    assert!(r.state.succeeded == 2 && r.state.failed == 1);
    assert!(out.join("c_out.handshake").exists());

    let out2 = s.dir("out2");
    let r = batch::run_batch(&h, &job(files(&[&a, &bad, &c]), vec![target(&out2, OverwritePolicy::Rename)], ErrorPolicy::StopOnFirstError), BatchPorts::new(test_context()), &cancel).unwrap();
    assert_eq!(r.receipts.len(), 2);
    assert_eq!(r.stopped, Some("stop_on_first_error"));
    assert!(out2.join("a_out.handshake").exists() && !out2.join("c_out.handshake").exists());

    // Quarantine copies the failing input (never moves it).
    let q = s.dir("quarantine");
    let r = batch::run_batch(&h, &job(files(&[&bad]), vec![target(&s.dir("out3"), OverwritePolicy::Rename)], ErrorPolicy::Quarantine { folder: q.to_string_lossy().into_owned() }), BatchPorts::new(test_context()), &cancel).unwrap();
    assert!(r.receipts[0].quarantined_to.as_deref().unwrap().ends_with("b.handshake"));
    assert!(bad.exists() && q.join("b.handshake").exists());
}

#[test]
fn always_show_ask_and_unsupported_configurations_refuse_before_any_file_is_touched() {
    let s = Scratch::new("refuse");
    let a = s.input("a.handshake", FIXTURE);
    let out = s.dir("out");
    let h = harness();
    let cancel = CancellationToken::default();
    let mut j = job(files(&[&a]), vec![target(&out, OverwritePolicy::Rename)], ErrorPolicy::SkipAndContinue);
    let Automation::Action(action) = &mut j.applied_automation else { unreachable!() };
    action.steps[1].dialog_options = DialogOption::AlwaysShow;
    let e = batch::run_batch(&h, &j, BatchPorts::new(test_context()), &cancel).unwrap_err();
    assert_eq!(e.code, "batch_always_show_dialog_in_headless");
    assert!(e.detail["action_id"] == ACT && e.detail["step_index"] == 1, "{}", e.to_json());
    // A disabled always_show step does not block.
    let Automation::Action(action) = &mut j.applied_automation else { unreachable!() };
    action.steps[1].enabled = false;
    assert!(batch::check_start(&j).is_ok());

    let mut ask = job(files(&[&a]), vec![target(&out, OverwritePolicy::Ask)], ErrorPolicy::SkipAndContinue);
    let e = batch::run_batch(&h, &ask, BatchPorts::new(test_context()), &cancel).unwrap_err();
    assert!(e.code == "batch_ask_overwrite_in_headless" && e.detail["job_id"] == "job-1");
    ask.output_targets[0].overwrite_policy = OverwritePolicy::Rename;

    let refusals: Vec<(&str, Mutation)> = vec![
        ("batch_source_kind_unsupported", Box::new(|j| j.source_set = SourceSet::OpenDocuments)),
        ("batch_source_kind_unsupported", Box::new(|j| j.source_set = SourceSet::CatalogQuery { query: "tag:x".into() })),
        ("batch_source_kind_unsupported", Box::new(|j| j.source_set = SourceSet::SelectionWithinDocument { document: "d".into(), scope: "layer".into() })),
        ("batch_concurrency_unsupported", Box::new(|j| j.concurrency = 2)),
        ("batch_template_token_unsupported", Box::new(|j| j.filename_template.push(TemplateToken::Date))),
        ("batch_template_token_unsupported", Box::new(|j| j.filename_template.push(TemplateToken::DocumentWidth))),
        ("batch_template_text_invalid", Box::new(|j| j.filename_template.push(TemplateToken::Custom("../up".into())))),
        ("batch_sequence_out_of_domain", Box::new(|j| j.filename_template.push(TemplateToken::Sequence { start: batch::SEQUENCE_HARD_MAX + 1, padding: 3 }))),
        ("batch_post_processing_unsupported", Box::new(|j| j.post_processing.push("sharpen".into()))),
        ("batch_retry_count_out_of_domain", Box::new(|j| j.error_policy = ErrorPolicy::RetryThenSkip { retry_count: batch::MAX_RETRIES + 1, backoff_ms: 0 })),
        ("action_step_command_unresolvable", Box::new(|j| j.applied_automation = Automation::CommandSequence(vec![step(0, "layer.no_such", vec![])]))),
    ];
    for (code, f) in refusals {
        let mut j = ask.clone();
        f(&mut j);
        let e = batch::run_batch(&h, &j, BatchPorts::new(test_context()), &cancel).unwrap_err();
        assert_eq!(e.code, code, "{}", e.to_json());
    }
    assert!(!out.exists(), "no output folder was created by a refused job");
    assert_eq!(std::fs::read(&a).unwrap(), FIXTURE);
    // A sequence start outside the soft range is a warning, not a refusal.
    let mut soft = ask.clone();
    soft.filename_template.push(TemplateToken::Sequence { start: 0, padding: 4 });
    assert_eq!(batch::check_start(&soft).unwrap()[0]["code"], "batch_sequence_start_outside_soft_range");
    println!("NOT_PROVEN: open_documents, selection_within_document and catalog_query sources (refused batch_source_kind_unsupported)");
    println!("NOT_PROVEN: date/time, document dimension and scale-suffix template tokens (refused: no clock port)");
    println!("NOT_PROVEN: concurrency > 1 and output formats other than the native package");
}

#[test]
fn cancelling_after_the_first_input_stops_the_job() {
    let s = Scratch::new("cancel");
    let ins: Vec<PathBuf> = ["a", "b", "c"].iter().map(|n| s.input(&format!("{n}.handshake"), FIXTURE)).collect();
    let out = s.dir("out");
    let h = harness();
    let cancel = CancellationToken::default();
    let mut on_state = |st: &JobState| {
        if st.done == 1 {
            cancel.cancel();
        }
    };
    let j = job(files(&ins.iter().collect::<Vec<_>>()), vec![target(&out, OverwritePolicy::Rename)], ErrorPolicy::SkipAndContinue);
    let r = batch::run_batch(&h, &j, BatchPorts { on_state: Some(&mut on_state), ..BatchPorts::new(test_context()) }, &cancel).unwrap();
    assert_eq!(r.receipts.len(), 1);
    assert!(r.state.cancelled && r.stopped == Some("cancelled") && r.state.done == 1 && r.state.total == 3, "{}", r.to_json());
    assert!(out.join("a_out.handshake").exists() && !out.join("b_out.handshake").exists());
}

/// One change applied to a job before it is run.
type Mutation = Box<dyn Fn(&mut BatchJob)>;

/// A Job Runtime double: refuses the first `refuse` admissions, counts admits and releases.
struct Jobs {
    refuse: AtomicU32,
    admitted: AtomicU64,
    released: AtomicU64,
    costs: std::sync::Mutex<Vec<Cost>>,
}

impl Jobs {
    fn new(refuse: u32) -> Arc<Self> {
        Arc::new(Self { refuse: AtomicU32::new(refuse), admitted: AtomicU64::new(0), released: AtomicU64::new(0), costs: std::sync::Mutex::new(Vec::new()) })
    }
}

impl JobPort for Jobs {
    fn admit(&self, scope: &JobScope<'_>, cost: Cost) -> Result<JobTicket, JobRefused> {
        assert!(scope.resource.as_str().starts_with("SDOC-"));
        self.costs.lock().unwrap().push(cost);
        if self.refuse.load(Ordering::SeqCst) > 0 {
            self.refuse.fetch_sub(1, Ordering::SeqCst);
            return Err(JobRefused { retry_after_ms: 7 });
        }
        Ok(JobTicket(self.admitted.fetch_add(1, Ordering::SeqCst)))
    }

    fn release(&self, _ticket: JobTicket) {
        self.released.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn a_job_port_refusing_once_triggers_a_retry_and_every_ticket_is_released() {
    let s = Scratch::new("jobs");
    let a = s.input("a.handshake", FIXTURE);
    let h = harness();
    let cancel = CancellationToken::default();
    let jobs = Jobs::new(1);
    let j = job(files(&[&a]), vec![target(&s.dir("out"), OverwritePolicy::Rename)], ErrorPolicy::RetryThenSkip { retry_count: 2, backoff_ms: 250 });
    let r = batch::run_batch(&h, &j, BatchPorts { jobs: Some(jobs.clone()), ..BatchPorts::new(test_context()) }, &cancel).unwrap();
    let rc = &r.receipts[0];
    assert!(rc.outcome == "succeeded" && rc.attempts == 2, "{}", rc.to_json());
    assert_eq!(rc.retries, vec![json!({"attempt": 1, "stage": "admission", "code": "job_admission_refused(retry_after_ms=7)", "backoff_ms": 250, "slept": false})]);
    assert_eq!(jobs.costs.lock().unwrap().as_slice(), [Cost { jobs: 1, bytes: FIXTURE.len() as u64 }; 2]);
    assert_eq!((jobs.admitted.load(Ordering::SeqCst), jobs.released.load(Ordering::SeqCst)), (1, 1));

    // skip_and_continue: the refusal fails the input; a failing automation still releases its ticket.
    let refusing = Jobs::new(1);
    let mut failing = job(files(&[&a, &a]), vec![target(&s.dir("out2"), OverwritePolicy::Rename)], ErrorPolicy::SkipAndContinue);
    let r = batch::run_batch(&h, &failing, BatchPorts { jobs: Some(refusing.clone()), ..BatchPorts::new(test_context()) }, &cancel).unwrap();
    assert_eq!(r.receipts.iter().map(|x| x.outcome).collect::<Vec<_>>(), ["failed", "succeeded"]);
    assert_eq!(r.receipts[0].error.as_ref().unwrap()["stage"], "admission");
    assert_eq!((refusing.admitted.load(Ordering::SeqCst), refusing.released.load(Ordering::SeqCst)), (1, 1));
    failing.applied_automation = Automation::CommandSequence(vec![step(0, "layer.rename", vec![("layer", RecordedValue::Reference("SLYR-019abcde-0000-7000-8000-0000000000ff".into())), ("name", RecordedValue::String("x".into()))])]);
    let counting = Jobs::new(0);
    let r = batch::run_batch(&h, &failing, BatchPorts { jobs: Some(counting.clone()), ..BatchPorts::new(test_context()) }, &cancel).unwrap();
    assert!(r.receipts.iter().all(|x| x.outcome == "failed" && x.error.as_ref().unwrap()["code"] == "action_step_reference_unresolved"), "{}", r.to_json());
    assert_eq!((counting.admitted.load(Ordering::SeqCst), counting.released.load(Ordering::SeqCst)), (2, 2));
}

#[test]
fn job_export_then_import_is_byte_identical() {
    let j = BatchJob {
        job_id: "job-portable".into(),
        source_set: SourceSet::Folder { folder: "inputs".into(), recurse: true, include: vec!["*.handshake".into(), "scan-??.handshake".into()], exclude: vec!["*-draft*".into()] },
        applied_automation: Automation::CommandSequence(the_action().steps),
        output_targets: vec![
            target(Path::new("outputs"), OverwritePolicy::Overwrite),
            OutputTarget { format_id: "png".into(), export_recipe_id: Some("SXPR-x".into()), destination: "png".into(), colour_profile_policy: Some("keep".into()), resize_policy: None, overwrite_policy: OverwritePolicy::Skip },
        ],
        filename_template: vec![TemplateToken::OriginalFilename, TemplateToken::Custom("_".into()), TemplateToken::Sequence { start: 1, padding: 4 }, TemplateToken::OutputFormat, TemplateToken::OriginalExtension, TemplateToken::Date],
        error_policy: ErrorPolicy::RetryThenSkip { retry_count: 3, backoff_ms: 500 },
        concurrency: 1,
        post_processing: vec![],
    };
    let bytes = j.export();
    let back = BatchJob::decode(&bytes).unwrap();
    assert_eq!(back, j);
    assert_eq!(back.export(), bytes);
    for (policy, kind) in [(ErrorPolicy::Quarantine { folder: "q".into() }, "quarantine"), (ErrorPolicy::StopOnFirstError, "stop_on_first_error"), (ErrorPolicy::SkipAndContinue, "skip_and_continue")] {
        let mut k = j.clone();
        k.error_policy = policy;
        k.applied_automation = Automation::None;
        assert_eq!(BatchJob::decode(&k.export()).unwrap(), k, "{kind}");
    }
    let text = String::from_utf8(bytes).unwrap();
    assert_eq!(BatchJob::decode(text.replacen("\"concurrency\":1", "\"concurrency\":1,\"concurrency\":1", 1).as_bytes()).unwrap_err().code, "field_duplicate");
    assert_eq!(BatchJob::decode(text.replacen("\"concurrency\":1", "\"concurrency\":1,\"priority\":1", 1).as_bytes()).unwrap_err().code, "field_unknown");
    assert_eq!(BatchJob::decode(text.replacen("\"overwrite\"", "\"replace\"", 1).as_bytes()).unwrap_err().code, "overwrite_policy_out_of_domain");
    assert_eq!(BatchJob::decode(text.replacen("\"retry_then_skip\"", "\"retry_forever\"", 1).as_bytes()).unwrap_err().code, "error_policy_out_of_domain");
}
