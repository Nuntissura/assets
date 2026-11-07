//! Lane SMB-SESSION-WRITE-SAFETY-1 (HBR-SWARM-001, GLOBAL-PARALLEL-SAFE): saves of one package from separate OS
//! processes. Every process here is real: the test binary re-executes itself (`child_entry`), so a child has its own
//! pid, its own session and its own file handles. No thread stands in for a process.
//!
//! Fixture: `tests/fixtures/demo.handshake`, revision 9.

use hsk_studio_accord::CancellationToken;
use hsk_studio_harness::{Harness, Selection, asset_local_test_context, cli};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const L1: &str = "SLYR-019abcde-0000-7000-8000-000000000001";
const CHILD_SPEC: &str = "HSK_WRITE_SAFETY_CHILD";
/// A child gives up after this long, so a stuck parent never leaves a process behind.
const CHILD_DEADLINE: Duration = Duration::from_secs(120);

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/demo.handshake")
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hsk-write-safety-{}-{label}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn package(&self) -> PathBuf {
        let package = self.0.join("doc.handshake");
        std::fs::copy(fixture_path(), &package).unwrap();
        package
    }
    fn signal(&self, name: &str) {
        std::fs::write(self.0.join(name), b"").unwrap();
    }
    /// Files of the scratch directory that are not this test's own signals and results.
    fn leftovers(&self, known: &[&Path]) -> Vec<String> {
        std::fs::read_dir(&self.0)
            .unwrap()
            .map(|e| e.unwrap())
            .filter(|e| !known.contains(&e.path().as_path()))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| !(n.starts_with("opened-") || n.starts_with("go") || n.starts_with("result-")))
            .collect()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn wait_for(path: &Path) {
    let start = Instant::now();
    while !path.exists() {
        assert!(start.elapsed() < CHILD_DEADLINE, "timed out waiting for {}", path.display());
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Starts a real child process of this test binary with `spec`.
fn spawn(spec: &Value) -> Child {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--ignored", "--exact", "child_entry", "--nocapture", "--test-threads=1"])
        .env(CHILD_SPEC, spec.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.spawn().unwrap()
}

fn finish(mut child: Child) -> i32 {
    child.wait().unwrap().code().unwrap_or(-1)
}

fn result(dir: &Path, id: &str) -> Value {
    serde_json::from_slice(&std::fs::read(dir.join(format!("result-{id}.json"))).unwrap()).unwrap()
}

fn op<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["ops"].as_array().unwrap().iter().find(|o| o["op"] == name).unwrap_or_else(|| panic!("no op {name} in {report}"))
}

/// The process body of every child. Not a proof by itself: the parent tests read what it wrote.
#[test]
#[ignore = "child process entry of this file's tests; proves nothing on its own"]
fn child_entry() {
    let Ok(spec) = std::env::var(CHILD_SPEC) else {
        println!("NOT_PROVEN: child_entry is only the process body of the multi-process save tests");
        return;
    };
    let spec: Value = serde_json::from_str(&spec).unwrap();
    let dir = PathBuf::from(spec["dir"].as_str().unwrap());
    let id = spec["id"].as_str().unwrap();
    let write_result = |text: &str| std::fs::write(dir.join(format!("result-{id}.json")), text).unwrap();
    match spec["mode"].as_str().unwrap() {
        // Opens PACKAGE, says so, waits for go-ID, renames a layer and saves back to PACKAGE.
        "session" => {
            let package = PathBuf::from(spec["package"].as_str().unwrap());
            let harness = Harness::new(Selection { providers: vec!["chronicle.patch".to_string()], ..Selection::default() }).unwrap();
            let cancel = CancellationToken::default();
            let mut run = harness.run(asset_local_test_context(), &cancel);
            assert!(run.open_path(&package));
            std::fs::write(dir.join(format!("opened-{id}")), b"").unwrap();
            wait_for(&dir.join(format!("go-{id}")));
            assert!(run.rename(L1, spec["name"].as_str().unwrap(), None));
            run.save_to(&package);
            write_result(&run.finish("child").to_string());
            std::process::exit(0);
        }
        // Says it is ready, waits for the shared go signal, runs one command line.
        "cli" => {
            let args: Vec<String> = spec["args"].as_array().unwrap().iter().map(|a| a.as_str().unwrap().to_string()).collect();
            std::fs::write(dir.join(format!("opened-{id}")), b"").unwrap();
            wait_for(&dir.join("go"));
            let out = cli::run(&args);
            write_result(&out.stdout);
            std::process::exit(out.exit);
        }
        other => panic!("unknown child mode {other}"),
    }
}

fn session_child(scratch: &Scratch, package: &Path, id: &str, name: &str) -> Child {
    spawn(&json!({"mode":"session","dir":scratch.0,"package":package,"id":id,"name":name}))
}

fn verify_name(package: &Path, name: &str) -> (i32, Value) {
    let args: Vec<String> = ["verify", package.to_str().unwrap(), "--expect-name", L1, name].iter().map(|s| (*s).to_string()).collect();
    let out = cli::run(&args);
    (out.exit, serde_json::from_str(&out.stdout).unwrap_or(Value::Null))
}

fn cli_json(args: &[&str]) -> (i32, Value) {
    let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    let out = cli::run(&args);
    (out.exit, serde_json::from_str(&out.stdout).unwrap_or(Value::Null))
}

/// Two processes hold one package open, then save one after the other. The second has not seen the first's save, so
/// it is refused with a typed code and the first's save stays on disk.
#[test]
fn two_processes_saving_one_package_the_second_is_refused() {
    let scratch = Scratch::new("two-sessions");
    let package = scratch.package();
    let (a, b) = (session_child(&scratch, &package, "a", "from-a"), session_child(&scratch, &package, "b", "from-b"));
    wait_for(&scratch.0.join("opened-a"));
    wait_for(&scratch.0.join("opened-b"));

    scratch.signal("go-a");
    assert_eq!(finish(a), 0);
    let report_a = result(&scratch.0, "a");
    assert_eq!(op(&report_a, "save")["ok"], true, "{report_a}");
    let after_a = std::fs::read(&package).unwrap();

    scratch.signal("go-b");
    assert_eq!(finish(b), 0);
    let report_b = result(&scratch.0, "b");
    assert_eq!(op(&report_b, "save")["error"], "destination_changed", "{report_b}");

    assert_eq!(std::fs::read(&package).unwrap(), after_a, "the first process's save is byte-identical");
    assert_eq!(verify_name(&package, "from-a").0, 0);
    assert_eq!(scratch.leftovers(&[package.as_path()]), Vec::<String>::new(), "no temp or lock left behind");
}

/// One process holds a package open; another process replaces the file; the first refuses and leaves it alone.
#[test]
fn a_process_refuses_to_save_over_a_file_another_writer_changed() {
    let scratch = Scratch::new("operator-edit");
    let package = scratch.package();
    let child = session_child(&scratch, &package, "a", "from-a");
    wait_for(&scratch.0.join("opened-a"));

    // The other writer is this test process: a valid package at another revision, written over the path.
    let other = scratch.0.join("other.handshake");
    let (exit, r) = cli_json(&["edit", fixture_path().to_str().unwrap(), "--provider", "chronicle.patch", "--rename", L1, "operator", "--save", other.to_str().unwrap()]);
    assert_eq!(exit, 0, "{r}");
    std::fs::copy(&other, &package).unwrap();
    let operator = std::fs::read(&package).unwrap();

    scratch.signal("go-a");
    assert_eq!(finish(child), 0);
    let report = result(&scratch.0, "a");
    assert_eq!(op(&report, "save")["error"], "destination_changed", "{report}");
    assert_eq!(std::fs::read(&package).unwrap(), operator, "the operator's edit is byte-identical");
    assert_eq!(scratch.leftovers(&[package.as_path(), other.as_path()]), Vec::<String>::new());
}

/// The file vanished after the process opened it: refused typed, not silently recreated.
#[test]
fn a_process_refuses_to_save_over_a_file_that_vanished() {
    let scratch = Scratch::new("vanished");
    let package = scratch.package();
    let child = session_child(&scratch, &package, "a", "from-a");
    wait_for(&scratch.0.join("opened-a"));
    std::fs::remove_file(&package).unwrap();
    scratch.signal("go-a");
    assert_eq!(finish(child), 0);
    let report = result(&scratch.0, "a");
    assert_eq!(op(&report, "save")["error"], "destination_missing", "{report}");
    assert!(!package.exists(), "nothing was recreated");
    assert_eq!(scratch.leftovers(&[]), Vec::<String>::new());
}

/// Parallel in-place command lines from one revision: exactly one lands, every other process is refused with a typed
/// code, and the file holds the winner's edit at revision 10.
#[test]
fn racing_cli_edits_in_place_have_exactly_one_winner() {
    let scratch = Scratch::new("race");
    let package = scratch.package();
    let p = package.to_str().unwrap();
    let names: Vec<String> = (0..6).map(|i| format!("racer-{i}")).collect();
    let children: Vec<Child> = names
        .iter()
        .map(|name| {
            spawn(&json!({"mode":"cli","dir":scratch.0,"id":name,"args":["edit",p,"--provider","chronicle.patch","--expect-revision","9","--rename",L1,name,"--save",p,"--overwrite"]}))
        })
        .collect();
    for name in &names {
        wait_for(&scratch.0.join(format!("opened-{name}")));
    }
    scratch.signal("go");
    let exits: Vec<i32> = children.into_iter().map(finish).collect();

    let mut winners = Vec::new();
    for (name, exit) in names.iter().zip(&exits) {
        let report = result(&scratch.0, name);
        if *exit == 0 {
            winners.push(name.clone());
        } else {
            assert_eq!(*exit, 1, "{report}");
            let failed: Vec<&str> = report["ops"].as_array().unwrap().iter().filter(|o| o["ok"] != true).map(|o| o["error"].as_str().unwrap()).collect();
            assert!(matches!(failed.as_slice(), ["destination_changed"] | ["stale_revision"]), "{name}: {report}");
        }
    }
    assert_eq!(winners.len(), 1, "exits {exits:?}");
    let (exit, v) = verify_name(&package, &winners[0]);
    assert_eq!(exit, 0, "the file holds the winner's edit: {v}");
    assert_eq!(cli_json(&["verify", p, "--expect-revision", "10"]).0, 0);
    assert_eq!(scratch.leftovers(&[package.as_path()]), Vec::<String>::new(), "no temp or lock left behind");
}

/// `edit` in place requires `--expect-revision`; a check that nothing was opened against is a usage error.
#[test]
fn edit_in_place_needs_expect_revision() {
    let scratch = Scratch::new("needs-expect");
    let package = scratch.package();
    let p = package.to_str().unwrap();
    let before = std::fs::read(&package).unwrap();
    let (exit, r) = cli_json(&["edit", p, "--provider", "chronicle.patch", "--rename", L1, "x", "--save", p, "--overwrite"]);
    assert!(exit == 2 && r["error"] == "usage" && r["message"].as_str().unwrap().contains("--expect-revision"), "{r}");
    assert_eq!(std::fs::read(&package).unwrap(), before);
    // A second spelling of the same file is the same requirement.
    let alias = scratch.0.join(".").join("doc.handshake");
    assert_eq!(cli_json(&["edit", p, "--provider", "chronicle.patch", "--rename", L1, "x", "--save", alias.to_str().unwrap(), "--overwrite"]).0, 2);
    // Saving elsewhere and the in-memory checkpoint write nothing over the input and need no expectation.
    assert_eq!(cli_json(&["edit", p, "--provider", "chronicle.patch", "--rename", L1, "x"]).0, 0);
    let out = scratch.0.join("out.handshake");
    assert_eq!(cli_json(&["edit", p, "--provider", "chronicle.patch", "--rename", L1, "x", "--save", out.to_str().unwrap()]).0, 0);
}

/// `--expect-revision N` names the revision the file was opened at: the edits of one command line advance from it.
#[test]
fn expect_revision_covers_every_edit_of_one_command_line() {
    let scratch = Scratch::new("multi-edit");
    let package = scratch.package();
    let p = package.to_str().unwrap();
    let (exit, r) = cli_json(&["edit", p, "--provider", "chronicle.patch", "--expect-revision", "9", "--rename", L1, "one", "--visible", L1, "false", "--rename", L1, "two", "--save", p, "--overwrite"]);
    assert_eq!(exit, 0, "{r}");
    assert_eq!(op(&r, "save")["detail"]["revision"], 12, "{r}");
    assert_eq!(cli_json(&["verify", p, "--expect-revision", "12", "--expect-name", L1, "two", "--expect-visible", L1, "false"]).0, 0);
    // A wrong expectation is refused on the first edit, as before.
    let (exit, r) = cli_json(&["edit", p, "--provider", "chronicle.patch", "--expect-revision", "9", "--rename", L1, "three", "--save", p, "--overwrite"]);
    assert!(exit == 1 && op(&r, "rename")["error"] == "stale_revision", "{r}");
}

/// Another existing file is not replaced by `--save` unless `--overwrite` says so.
#[test]
fn save_over_another_existing_file_needs_overwrite() {
    let scratch = Scratch::new("foreign-out");
    let package = scratch.package();
    let other = scratch.0.join("other.handshake");
    std::fs::write(&other, b"not mine").unwrap();
    let (p, o) = (package.to_str().unwrap(), other.to_str().unwrap());
    let (exit, r) = cli_json(&["edit", p, "--provider", "chronicle.patch", "--rename", L1, "x", "--save", o]);
    assert!(exit == 1 && op(&r, "save")["error"] == "destination_exists", "{r}");
    assert_eq!(std::fs::read(&other).unwrap(), b"not mine");
    let (exit, r) = cli_json(&["edit", p, "--provider", "chronicle.patch", "--rename", L1, "x", "--save", o, "--overwrite"]);
    assert_eq!(exit, 0, "explicit replace: {r}");
    assert_eq!(verify_name(&other, "x").0, 0);
}

/// Hostile paths: each refuses with a typed code, exit 1, and writes nothing.
#[test]
fn hostile_package_paths_refuse_typed() {
    let scratch = Scratch::new("hostile");
    let package = scratch.package();
    let p = package.to_str().unwrap();
    let before = std::fs::read(&package).unwrap();
    let edit = |save: &str, overwrite: bool| {
        let mut args = vec!["edit", p, "--provider", "chronicle.patch", "--rename", L1, "x", "--save", save];
        if overwrite {
            args.push("--overwrite");
        }
        cli_json(&args)
    };

    // A directory as the destination, with and without --overwrite.
    let folder = scratch.0.join("folder");
    std::fs::create_dir(&folder).unwrap();
    for overwrite in [false, true] {
        let (exit, r) = edit(folder.to_str().unwrap(), overwrite);
        assert!(exit == 1 && op(&r, "save")["error"] == "destination_invalid", "{r}");
    }
    // A directory as the package.
    let (exit, r) = cli_json(&["edit", folder.to_str().unwrap(), "--provider", "chronicle.patch", "--expect-revision", "9", "--rename", L1, "x"]);
    assert!(exit == 1 && op(&r, "open")["error"] == "io", "{r}");

    // A name no filesystem takes (over 255 bytes).
    let long = scratch.0.join("n".repeat(300));
    let (exit, r) = edit(long.to_str().unwrap(), false);
    assert!(exit == 1 && op(&r, "save")["error"] == "io", "{r}");
    assert!(!long.exists());

    // A vanished package.
    let gone = scratch.0.join("gone.handshake");
    let (exit, r) = cli_json(&["edit", gone.to_str().unwrap(), "--provider", "chronicle.patch", "--expect-revision", "9", "--rename", L1, "x", "--save", gone.to_str().unwrap(), "--overwrite"]);
    assert!(exit == 1 && op(&r, "open")["error"] == "io", "{r}");
    assert!(!gone.exists());

    // A read-only package saved in place.
    let mut permissions = std::fs::metadata(&package).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&package, permissions.clone()).unwrap();
    let (exit, r) = cli_json(&["edit", p, "--provider", "chronicle.patch", "--expect-revision", "9", "--rename", L1, "x", "--save", p, "--overwrite"]);
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    std::fs::set_permissions(&package, permissions).unwrap();
    assert!(exit == 1 && op(&r, "save")["error"] == "destination_read_only", "{r}");
    assert_eq!(std::fs::read(&package).unwrap(), before);
    assert_eq!(scratch.leftovers(&[package.as_path(), folder.as_path()]), Vec::<String>::new());
}
