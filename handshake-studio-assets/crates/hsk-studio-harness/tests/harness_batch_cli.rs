//! Lane SMB-PS-ACTIONS-1 slice 4: the `batch JOB.json` and `action check SET.json` commands and
//! their manual entries. Headless: `cli::run` is a pure function; every file lives under
//! `std::env::temp_dir()`. Fixture: `tests/fixtures/demo.handshake`.

use hsk_studio_harness::cli;
use hsk_studio_harness::manual::COMMANDS;
use hsk_studio_harness_core::action::{ActionRecord, ActionSet, ActionStep, DialogOption, RecordedValue, UnitToken};
use hsk_studio_harness_core::batch::{Automation, BatchJob, ErrorPolicy, OutputTarget, OverwritePolicy, SourceSet, TemplateToken};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

const FIXTURE: &[u8] = include_bytes!("fixtures/demo.handshake");
const L1: &str = "SLYR-019abcde-0000-7000-8000-000000000001";
const L2: &str = "SLYR-019abcde-0000-7000-8000-000000000002";

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hsk-harness-batch-cli-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn file(&self, name: &str, bytes: &[u8]) -> String {
        let p = self.0.join(name);
        std::fs::write(&p, bytes).unwrap();
        p.to_string_lossy().into_owned()
    }

    fn path(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn cli(args: &[&str]) -> (i32, Value) {
    let out = cli::run(&args.iter().map(|s| (*s).to_string()).collect::<Vec<_>>());
    (out.exit, serde_json::from_str(&out.stdout).unwrap_or(Value::Null))
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

fn the_action() -> ActionRecord {
    let steps = vec![
        step(0, "layer.rename", vec![("layer", RecordedValue::Reference(L1.into())), ("name", RecordedValue::String("from-cli".into()))]),
        step(1, "layer.set_opacity", vec![("layer", RecordedValue::Reference(L2.into())), ("opacity", RecordedValue::Measured { value: 60.0, unit: Some(UnitToken::Percent) })]),
    ];
    ActionRecord {
        action_id: "SACT-019abcde-0000-7000-8000-0000000000c1".into(),
        index: 0,
        name: "cli".into(),
        function_key_index: 3,
        shift_modifier: false,
        command_modifier: false,
        color_index: 2,
        expanded: false,
        declared_step_count: 2,
        steps,
    }
}

fn job(input: &str, out: &str, policy: OverwritePolicy) -> BatchJob {
    BatchJob {
        job_id: "cli-job".into(),
        source_set: SourceSet::ExplicitFileList(vec![input.to_string()]),
        applied_automation: Automation::Action(the_action()),
        output_targets: vec![OutputTarget { format_id: "handshake".into(), export_recipe_id: None, destination: out.to_string(), colour_profile_policy: None, resize_policy: None, overwrite_policy: policy }],
        filename_template: vec![TemplateToken::OriginalFilename, TemplateToken::Custom("-done".into())],
        error_policy: ErrorPolicy::SkipAndContinue,
        concurrency: 1,
        post_processing: vec![],
    }
}

#[test]
fn cli_batch_runs_a_portable_job_and_refuses_typed() {
    let s = Scratch::new("batch");
    let input = s.file("doc.handshake", FIXTURE);
    let out = s.path("out");
    let job_file = s.file("job.json", &job(&input, &out, OverwritePolicy::Rename).export());
    let (exit, report) = cli(&["batch", &job_file, "--provider", "chronicle.patch"]);
    assert_eq!(exit, 0, "{report}");
    assert!(report["command"] == "batch" && report["verdict"] == "pass", "{report}");
    let receipt = &report["report"]["receipts"][0];
    assert!(receipt["outcome"] == "succeeded" && receipt["input"]["content_digest"]["algorithm"] == "sha256", "{receipt}");
    let written = receipt["targets"][0]["path"].as_str().unwrap().to_string();
    assert!(written.ends_with("doc-done.handshake"));
    // The output carries the action: the existing verify command reads it back.
    let (exit, verify) = cli(&["verify", &written, "--expect-name", L1, "from-cli"]);
    assert_eq!(exit, 0, "{verify}");
    assert_eq!(std::fs::read(&input).unwrap(), FIXTURE, "the input is never written");

    // Without the chronicle provider the action's edits are refused: the input fails, exit 1.
    let (exit, report) = cli(&["batch", &job_file]);
    assert!(exit == 1 && report["verdict"] == "fail" && report["report"]["receipts"][0]["error"]["stage"] == "automation", "{report}");
    // Start-time refusal (ask overwrite in a headless job): typed, exit 1, nothing written.
    let ask = s.file("ask.json", &job(&input, &s.path("out-ask"), OverwritePolicy::Ask).export());
    let (exit, report) = cli(&["batch", &ask, "--provider", "chronicle.patch"]);
    assert!(exit == 1 && report["error"] == "batch_ask_overwrite_in_headless" && report["detail"]["job_id"] == "cli-job", "{report}");
    assert!(!std::path::Path::new(&s.path("out-ask")).exists());
    // A malformed job is a typed decode refusal; a missing path is a usage error.
    let bad = s.file("bad.json", b"{\"job_id\":\"x\"}");
    let (exit, report) = cli(&["batch", &bad]);
    assert!(exit == 1 && report["error"] == "field_missing", "{report}");
    assert_eq!(cli(&["batch"]).0, 2);
    // A cancelled run checks nothing: never a pass.
    let (exit, report) = cli(&["batch", &job_file, "--provider", "chronicle.patch", "--cancel"]);
    assert!(exit == 1 && report["verdict"] == "nothing_checked", "{report}");
}

#[test]
fn cli_action_check_runs_the_validators() {
    let s = Scratch::new("check");
    let good = ActionSet { action_set_id: "set-cli".into(), name: "CLI".into(), actions: vec![the_action()] };
    let path = s.file("good.json", &good.export());
    let (exit, report) = cli(&["action", "check", &path]);
    assert!(exit == 0 && report["ok"] == true && report["actions"] == 1 && report["steps"] == 2, "{report}");
    assert!(report["diagnostics"].as_array().unwrap().is_empty());

    let mut bad = good.clone();
    bad.actions[0].declared_step_count = 5;
    bad.actions[0].steps.push(step(2, "photoshop.unsharp_mask", vec![]));
    let path = s.file("bad.json", &bad.export());
    let (exit, report) = cli(&["action", "check", &path]);
    assert_eq!(exit, 1, "{report}");
    let codes: Vec<&str> = report["diagnostics"].as_array().unwrap().iter().map(|d| d["code"].as_str().unwrap()).collect();
    assert_eq!(codes, ["action_declared_step_count_mismatch", "action_step_command_unresolvable"]);
    assert_eq!(report["unresolvable"][0]["command_id"], "photoshop.unsharp_mask");

    // Warnings only: exit 0.
    let mut warn = good.clone();
    warn.actions[0].steps[0].parameters.as_mut().unwrap().insert("colour".into(), RecordedValue::Number(1.0));
    let path = s.file("warn.json", &warn.export());
    let (exit, report) = cli(&["action", "check", &path]);
    assert!(exit == 0 && report["diagnostics"][0]["severity"] == "warning", "{report}");

    let path = s.file("dup.json", br#"{"action_set_id":"a","name":"n","name":"m","actions":[]}"#);
    let (exit, report) = cli(&["action", "check", &path]);
    assert!(exit == 1 && report["error"] == "field_duplicate", "{report}");
    assert_eq!(cli(&["action", "lint", &path]).0, 2);
}

#[test]
fn manual_lists_batch_action_and_the_automation_vocabulary() {
    let names: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
    assert!(names.contains(&"batch") && names.contains(&"action"));
    let (exit, manual) = cli(&["help", "--json"]);
    assert_eq!(exit, 0);
    let auto = &manual["automation"];
    assert_eq!(auto["dialog_options"].as_array().unwrap().len(), 4);
    assert_eq!(auto["value_types"].as_array().unwrap().len(), 8);
    assert_eq!(auto["unit_tokens"], serde_json::json!(["percent", "pixels", "relative", "angle_degrees", "points", "resolution_dpi"]));
    assert_eq!(auto["overwrite_policies"], serde_json::json!(["ask", "overwrite", "rename", "skip"]));
    assert!(auto["commands"].as_array().unwrap().iter().any(|c| c["command_id"] == "layer.rename" && c["recordable"] == true));
    let text = cli::run(&["help".to_string()]).stdout;
    assert!(text.contains("action check SET.json") && text.contains("AUTOMATION") && text.contains("command layer.set_opacity"));
    // The new engine modules stay quiet like the rest of the harness sources.
    for (name, src) in [("action.rs", include_str!("../../hsk-studio-harness-core/src/action.rs")), ("batch.rs", include_str!("../../hsk-studio-harness-core/src/batch.rs"))] {
        for token in ["std::process", "Command::new", "std::net", "TcpStream", "UdpSocket", "std::env", "env::var"] {
            assert!(!src.contains(token), "{name} mentions `{token}`");
        }
    }
    println!("NOT_PROVEN: GUI actions panel and batch dialog (CLI and engine only)");
}
