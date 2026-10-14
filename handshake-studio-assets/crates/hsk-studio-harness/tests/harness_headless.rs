//! Slice 1 focused tests (<= 4), all headless: no window, no process spawn, no network.
//! Fixture: `tests/fixtures/demo.handshake` (revision 9; made by the session crate's
//! `session-consumer --demo`; group layer ...0001 "renamed-by-session" visible, raster layer
//! ...0002 "layer-2" hidden, one unresolved tile reference).

use hsk_studio_accord::CancellationToken;
use hsk_studio_harness::cli;
use hsk_studio_harness::manual::COMMANDS;
use hsk_studio_harness::{Harness, HarnessError, Selection, default_actor};
use serde_json::Value;
use std::path::{Path, PathBuf};

const FIXTURE: &[u8] = include_bytes!("fixtures/demo.handshake");
const L1: &str = "SLYR-019abcde-0000-7000-8000-000000000001";
const L2: &str = "SLYR-019abcde-0000-7000-8000-000000000002";

fn harness(providers: &[&str]) -> Harness {
    Harness::new(Selection { providers: providers.iter().map(|s| (*s).to_string()).collect(), ..Selection::default() }).unwrap()
}

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/demo.handshake")
}

fn cli_json(args: &[&str]) -> (i32, Value) {
    let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    let out = cli::run(&args);
    (out.exit, serde_json::from_str(&out.stdout).unwrap_or(Value::Null))
}

fn op<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["ops"].as_array().unwrap().iter().find(|o| o["op"] == name).unwrap_or_else(|| panic!("no op {name} in {report}"))
}

fn layer<'a>(summary: &'a Value, id: &str) -> &'a Value {
    summary["detail"]["layers"].as_array().unwrap().iter().find(|l| l["id"] == id).unwrap()
}

/// Test scratch directory under TMP, removed on drop. TMP must be on D: (studio-env.sh sets it).
struct Scratch(PathBuf);

impl Scratch {
    /// One directory per test (`label`), so tests running in parallel never share or remove it.
    fn new(label: &str) -> Self {
        let base = std::env::temp_dir();
        let text = base.to_string_lossy().to_ascii_uppercase();
        assert!(!(cfg!(windows) && text.starts_with("C:")), "TMP is on C: ({}); run through studio-env.sh", base.display());
        let dir = base.join(format!("hsk-harness-test-{}-{label}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn open_edit_save_reopen_verify_end_to_end() {
    let h = harness(&["chronicle.patch"]);
    let cancel = CancellationToken::default();
    let mut run = h.run(default_actor(), &cancel);
    assert!(run.open_bytes(FIXTURE) && run.summary());
    assert!(run.rename(L1, "renamed", None));
    // Panel route: the value is parsed by the control descriptor, set in the ValueStore, then
    // committed through the session's chronicle provider.
    assert!(run.panel_set(L2, "layer.visible", "true", None));
    assert!(run.panel_set(L2, "layer.name", "panel name", None));
    let bytes = run.checkpoint().expect("checkpoint");
    assert!(run.reopen_bytes(&bytes) && run.verify_reopened());
    let report = run.finish("test");

    assert!(report["ok"] == true && report["verdict"] == "pass", "{report}");
    let ops = report["ops"].as_array().unwrap();
    assert_eq!(ops.iter().map(|o| o["op"].as_str().unwrap()).collect::<Vec<_>>(), ["open", "summary", "rename", "panel_set", "panel_set", "checkpoint", "reopen", "verify"]);
    // One Observe frame per operation, none extra.
    assert!(ops.iter().all(|o| o["observe"]["frames"] == 1 && o["observe"]["outcome"] == "success"), "{report}");
    assert!(report["observe"]["frames_total"] == 8 && report["observe"]["one_frame_per_operation"] == true);
    // Revisions advance by exactly one per committed edit, and the reopened session agrees.
    assert_eq!(ops.iter().map(|o| o["revision"].as_u64().unwrap()).collect::<Vec<_>>(), [9, 9, 10, 11, 12, 12, 12, 12]);
    assert!(ops[3]["control"] == "layer.visible" && ops[4]["store_value_after"] == "panel name");
    let verify = &ops[7]["detail"];
    assert!(verify["revision_match"] == true && verify["encoded_bytes_equal"] == true);
    assert_eq!(verify["expectations"].as_array().unwrap().len(), 3);

    // Independent read: a fresh run on the checkpoint bytes sees the edits.
    let mut again = h.run(default_actor(), &cancel);
    assert!(again.open_bytes(&bytes) && again.summary());
    let r2 = again.finish("test");
    let summary = op(&r2, "summary");
    assert_eq!(summary["detail"]["revision"], 12);
    assert!(layer(summary, L1)["name"] == "renamed" && layer(summary, L2)["name"] == "panel name" && layer(summary, L2)["visible"] == true);
}

#[test]
fn json_output_is_stable_and_manual_comes_from_the_descriptors() {
    let path = fixture_path();
    let p = path.to_str().unwrap();
    let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();

    // Report schema: fixed top-level keys, fixed per-op keys, byte-identical on repeat.
    let (exit, open) = cli_json(&["open", p]);
    assert_eq!(exit, 0);
    assert_eq!(keys(&open), ["command", "not_proven", "observe", "ok", "ops", "schema", "selection", "verdict"]);
    assert!(open["schema"] == "hsk.studio.harness@1" && open["selection"]["providers"].as_array().unwrap().is_empty());
    assert_eq!(keys(&open["ops"][0]), ["detail", "n", "observe", "ok", "op", "revision"]);
    assert_eq!(keys(&open["ops"][0]["observe"]), ["frames", "outcome", "status"]);
    let again = cli::run(&["open".to_string(), p.to_string()]).stdout;
    assert_eq!(again, cli::run(&["open".to_string(), p.to_string()]).stdout);
    assert_eq!(open["ops"][1]["detail"]["unresolved_references"], 1, "default references are honest: unresolved");

    // Panel: controls JSON from the controls crate + a commit route for every control.
    let (_, panel) = cli_json(&["panel", p, "--layer", L2]);
    let detail = &op(&panel, "panel")["detail"];
    assert!(detail["controls"]["schema"] == "hsk.studio.controls@1" && detail["controls"]["controls"].as_array().unwrap().len() == 7);
    assert!(detail["bindings"]["layer.name"]["route"] == "chronicle.patch" && detail["bindings"]["appearance.opacity"]["reason"] == "no_chronicle_property");
    let visible = detail["controls"]["controls"].as_array().unwrap().iter().find(|c| c["id"] == "layer.visible").unwrap();
    assert!(visible["value"]["value"] == false && visible["consumers"][0] == "harness.commit.chronicle");

    // Manual: generated from the command table, the live provider list and the controls descriptors.
    let (exit, manual) = cli_json(&["help", "--json"]);
    assert_eq!(exit, 0);
    let names: Vec<&str> = manual["commands"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(names, COMMANDS.iter().map(|c| c.name).collect::<Vec<_>>());
    assert!(manual["providers"]["available"][0] == "chronicle.patch" && manual["providers"]["selected"].as_array().unwrap().is_empty());
    let topics: Vec<&str> = manual["panel_controls"].as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap()).collect();
    assert!(topics.contains(&"layer.name") && topics.contains(&"appearance.opacity") && topics.contains(&"appearance"), "{topics:?}");
    let text = cli::run(&["--help".to_string()]);
    assert_eq!(text.exit, 0);
    assert!(COMMANDS.iter().all(|c| text.stdout.contains(c.usage)) && text.stdout.contains("Hard range: [0, 100]") && text.stdout.contains("NOT PROVEN"));

    // Descriptors are parsed from the real consts of the five allowed foundations.
    let (_, d) = cli_json(&["descriptors"]);
    let owners: Vec<&str> = d["descriptors"].as_array().unwrap().iter().map(|e| e["owner"].as_str().unwrap()).collect();
    assert_eq!(owners, ["STUDIO-MODULE-ACCORD", "STUDIO-MODULE-FOLIO", "STUDIO-MODULE-SESSION", "hsk-studio-controls", "STUDIO-MODULE-OBSERVE"]);
    // The crate's own descriptor is valid JSON with the expected owner.
    let own: Value = serde_json::from_str(hsk_studio_harness::DESCRIPTOR).unwrap();
    assert!(own["owner"] == "STUDIO-MODULE-HARNESS" && own["mode"] == "headless_only");
}

#[test]
fn refusals_are_typed_and_nothing_is_committed() {
    let cancel = CancellationToken::default();
    let h = harness(&["chronicle.patch"]);
    let failing = |h: &Harness, f: &dyn Fn(&mut hsk_studio_harness::Run<'_>) -> bool| -> (String, Value) {
        let mut run = h.run(default_actor(), &cancel);
        assert!(run.open_bytes(FIXTURE));
        assert!(!f(&mut run));
        let report = run.finish("test");
        let last = report["ops"].as_array().unwrap().last().unwrap().clone();
        assert!(report["verdict"] == "fail" && last["ok"] == false, "{report}");
        (last["error"].as_str().unwrap().to_string(), last)
    };

    // Stale revision: the session refuses; its own frame reports the failure; the revision is unchanged.
    let (code, last) = failing(&h, &|r| r.rename(L1, "x", Some(3)));
    assert_eq!(code, "stale_revision");
    assert!(last["revision"] == 9 && last["observe"]["frames"] == 1 && last["observe"]["outcome"] == "failure_validation", "{last}");
    // Unknown layer / control, and descriptor-level rejection of typed values (never clamped).
    assert_eq!(failing(&h, &|r| r.rename("SLYR-nope", "x", None)).0, "unknown_layer");
    assert_eq!(failing(&h, &|r| r.panel_set(L2, "layer.nope", "x", None)).0, "unknown_control");
    assert_eq!(failing(&h, &|r| r.panel_set(L2, "layer.visible", "maybe", None)).0, "invalid_text");
    assert_eq!(failing(&h, &|r| r.panel_set(L2, "layer.name", &"n".repeat(300), None)).0, "text_too_long");
    // Appearance values are projected but have no commit route: refused, not faked.
    let (code, last) = failing(&h, &|r| r.panel_set(L2, "appearance.opacity", "50", None));
    assert_eq!(code, "no_commit_route");
    assert_eq!(last["observe"]["frames"], 1);
    // No provider selected: refused before any session operation.
    let none = harness(&[]);
    assert_eq!(failing(&none, &|r| r.rename(L1, "x", None)).0, "provider_not_selected");
    // Selection is explicit: unknown or duplicate providers never construct a harness.
    let bad = |providers: &[&str]| Harness::new(Selection { providers: providers.iter().map(|s| (*s).to_string()).collect(), ..Selection::default() }).unwrap_err();
    assert_eq!(bad(&["nope"]), HarnessError::UnknownProvider("nope".into()));
    assert_eq!(bad(&["chronicle.patch", "chronicle.patch"]), HarnessError::DuplicateProvider("chronicle.patch".into()));

    // A cancelled token refuses the open itself; nothing is opened, nothing is written.
    let token = CancellationToken::default();
    token.cancel();
    let mut run = h.run(default_actor(), &token);
    assert!(!run.open_bytes(FIXTURE));
    let report = run.finish("test");
    assert_eq!(report["ops"][0]["error"], "canceled");

    // Command line: refusals exit 1 with JSON, usage errors exit 2.
    let path = fixture_path();
    let p = path.to_str().unwrap();
    let (exit, r) = cli_json(&["edit", p, "--rename", L1, "x"]);
    assert!(exit == 1 && op(&r, "rename")["error"] == "provider_not_selected");
    assert_eq!(cli_json(&["open", p, "--provider", "nope"]), (1, cli_json(&["open", p, "--provider", "nope"]).1));
    assert_eq!(cli_json(&["open", p, "--provider", "nope"]).1["error"], "unknown_provider");
    assert_eq!(cli_json(&["edit", p, "--provider", "chronicle.patch"]).0, 2);
    assert_eq!(cli_json(&["frobnicate"]).0, 2);
    assert_eq!(cli_json(&["open"]).0, 2);
    assert_eq!(cli_json(&["open", p, "--bogus"]).0, 2);
    let (exit, missing) = cli_json(&["open", "does-not-exist.handshake"]);
    assert!(exit == 1 && op(&missing, "open")["error"] == "io" && missing["ops"].as_array().unwrap().len() == 1);
}

#[test]
fn files_are_saved_atomically_and_failed_edits_keep_the_last_good_file() {
    let scratch = Scratch::new("atomic-save");
    let out = scratch.0.join("edited.handshake");
    let out_s = out.to_str().unwrap();
    let src = fixture_path();
    let src_s = src.to_str().unwrap();

    // edit --save writes the file, reopens it in a second session and verifies.
    let (exit, report) = cli_json(&["edit", src_s, "--provider", "chronicle.patch", "--rename", L1, "on disk", "--visible", L2, "true", "--save", out_s]);
    assert_eq!(exit, 0, "{report}");
    assert_eq!(report["ops"].as_array().unwrap().iter().map(|o| o["op"].as_str().unwrap()).collect::<Vec<_>>(), ["open", "summary", "rename", "set_visible", "save", "reopen", "verify"]);
    assert!(op(&report, "save")["detail"]["revision"] == 11 && op(&report, "verify")["detail"]["encoded_bytes_equal"] == true);
    let good = std::fs::read(&out).unwrap();

    // The verify command independently re-reads the saved file.
    let (exit, v) = cli_json(&["verify", out_s, "--expect-revision", "11", "--expect-name", L1, "on disk", "--expect-visible", L2, "true"]);
    assert_eq!(exit, 0, "{v}");
    let (exit, v) = cli_json(&["verify", out_s, "--expect-name", L1, "something else"]);
    assert!(exit == 1 && op(&v, "verify")["error"] == "verify_mismatch" && op(&v, "verify")["detail"]["expectations"][0]["ok"] == false);

    // A refused edit (stale revision) targeting the same destination leaves the last-good file byte-identical.
    // (The destination is the input itself here, so the in-place replace is requested explicitly.)
    let (exit, r) = cli_json(&["edit", out_s, "--provider", "chronicle.patch", "--expect-revision", "3", "--rename", L1, "never", "--save", out_s, "--overwrite"]);
    assert!(exit == 1 && op(&r, "rename")["error"] == "stale_revision");
    assert!(r["ops"].as_array().unwrap().iter().all(|o| o["op"] != "save"), "no save after a refused edit");
    assert_eq!(std::fs::read(&out).unwrap(), good);
    // Only the files this test created exist in the scratch directory (no temp leftovers from the atomic save).
    let names: Vec<_> = std::fs::read_dir(&scratch.0).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["edited.handshake"]);
}

// ----- review R5 regressions -------------------------------------------------------------------

#[test]
fn verify_checks_something_and_save_never_clobbers_the_input() {
    let path = fixture_path();
    let p = path.to_str().unwrap();

    // `verify` with nothing to check is a usage error, not a pass (it used to exit 0 / "pass").
    let (exit, r) = cli_json(&["verify", p]);
    assert_eq!(exit, 2, "{r}");
    assert!(r["ok"] == false && r["error"] == "usage" && r["verdict"] != "pass", "{r}");
    assert_eq!(cli_json(&["verify", p, "--references", "trust_all"]).0, 2, "selection flags are not expectations");
    // With an expectation it checks it: a true one passes, a false one fails.
    let (exit, r) = cli_json(&["verify", p, "--expect-revision", "9"]);
    assert!(exit == 0 && r["verdict"] == "pass" && op(&r, "verify")["detail"]["revision_match"] == true, "{r}");
    let (exit, r) = cli_json(&["verify", p, "--expect-name", L1, "renamed-by-session"]);
    assert!(exit == 0 && op(&r, "verify")["detail"]["expectations"][0]["ok"] == true, "{r}");
    let (exit, r) = cli_json(&["verify", p, "--expect-revision", "3"]);
    assert!(exit == 1 && r["verdict"] == "fail" && op(&r, "verify")["error"] == "verify_mismatch", "{r}");

    // Library level: nothing to check is refused with its own frame; an edit committed earlier
    // in the run counts as something to check.
    let h = harness(&["chronicle.patch"]);
    let cancel = CancellationToken::default();
    let mut run = h.run(default_actor(), &cancel);
    assert!(run.open_bytes(FIXTURE));
    assert!(!run.verify_live(None, Vec::new()));
    assert!(run.rename(L1, "renamed", None));
    assert!(run.verify_live(None, Vec::new()));
    let report = run.finish("test");
    let verifies: Vec<&Value> = report["ops"].as_array().unwrap().iter().filter(|o| o["op"] == "verify").collect();
    assert!(verifies[0]["error"] == "nothing_to_check" && verifies[0]["observe"]["frames"] == 1, "{report}");
    assert!(verifies[1]["ok"] == true && verifies[1]["detail"]["expectations"].as_array().unwrap().len() == 1, "{report}");
    assert_eq!(report["verdict"], "fail", "a refused verify keeps the run from passing");

    // `edit --save` never replaces its own input unless asked to (any spelling of the same file).
    let scratch = Scratch::new("save-guard");
    let copy = scratch.0.join("copy.handshake");
    std::fs::copy(&path, &copy).unwrap();
    let before = std::fs::read(&copy).unwrap();
    let copy_s = copy.to_str().unwrap();
    let alias = scratch.0.join(".").join("copy.handshake");
    let alias_s = alias.to_str().unwrap();
    for save in [copy_s, alias_s] {
        let (exit, r) = cli_json(&["edit", copy_s, "--provider", "chronicle.patch", "--rename", L1, "clobber", "--save", save]);
        assert!(exit == 2 && r["error"] == "usage", "{r}");
        assert_eq!(std::fs::read(&copy).unwrap(), before, "the input is untouched");
    }
    let (exit, r) = cli_json(&["edit", copy_s, "--provider", "chronicle.patch", "--rename", L1, "replaced", "--save", copy_s, "--overwrite"]);
    assert_eq!(exit, 0, "{r}");
    assert_ne!(std::fs::read(&copy).unwrap(), before, "--overwrite replaces the input");
}

#[test]
fn an_empty_run_is_nothing_checked_not_a_pass() {
    let h = harness(&[]);
    let cancel = CancellationToken::default();
    let report = h.run(default_actor(), &cancel).finish("test");
    assert!(report["ok"] == false && report["verdict"] == "nothing_checked" && report["ops"].as_array().unwrap().is_empty(), "{report}");
    // One successful operation is evidence; the same report then passes.
    let mut run = h.run(default_actor(), &cancel);
    assert!(run.open_bytes(FIXTURE));
    let report = run.finish("test");
    assert!(report["ok"] == true && report["verdict"] == "pass", "{report}");
}

#[test]
fn quiet_sources_use_no_process_network_or_environment_api() {
    let sources = [
        ("cli.rs", include_str!("../src/cli.rs")),
        ("harness.rs", include_str!("../src/harness.rs")),
        ("lib.rs", include_str!("../src/lib.rs")),
        ("manual.rs", include_str!("../src/manual.rs")),
        ("panel.rs", include_str!("../src/panel.rs")),
        ("import.rs", include_str!("../src/import.rs")),
    ];
    for (name, text) in sources {
        for token in ["std::process", "Command::new", "std::net", "TcpStream", "UdpSocket", "std::env", "env::var"] {
            assert!(!text.contains(token), "{name} mentions `{token}`");
        }
    }
}
