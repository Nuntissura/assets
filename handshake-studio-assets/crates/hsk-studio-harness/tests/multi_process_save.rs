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
            .filter(|n| !(n.starts_with("opened-") || n.starts_with("go") || n.starts_with("result-") || n.starts_with("log-") || n.starts_with("intent-")))
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
        // Saves in a loop from whatever is on disk: each attempt opens, renames, saves and logs before the next.
        "soak" => {
            let package = PathBuf::from(spec["package"].as_str().unwrap());
            let harness = Harness::new(Selection { providers: vec!["chronicle.patch".to_string()], ..Selection::default() }).unwrap();
            for attempt in 0..spec["attempts"].as_u64().unwrap() {
                let cancel = CancellationToken::default();
                let mut run = harness.run(asset_local_test_context(), &cancel);
                let name = format!("{id}-{attempt}");
                let line = if run.open_path(&package) && run.rename(L1, &name, None) {
                    let landed = run.save_to(&package);
                    let report = run.finish("soak");
                    let save = op(&report, "save");
                    json!({"name":name,"revision":save["revision"],"result":if landed { json!("landed") } else { save["error"].clone() }})
                } else {
                    json!({"name":name,"result":format!("open_failed:{}", run.finish("soak")["ops"][0]["error"])})
                };
                append_line(&dir.join(format!("log-{id}.jsonl")), &line);
            }
            std::process::exit(0);
        }
        // Writes down what it is about to save, then exits while the save runs: a watcher thread ends the process as
        // soon as this save's temp file exists, so the lock, the temp and its owner marker are abandoned mid-save.
        "exiter" => {
            let package = PathBuf::from(spec["package"].as_str().unwrap());
            let harness = Harness::new(Selection { providers: vec!["chronicle.patch".to_string()], ..Selection::default() }).unwrap();
            let cancel = CancellationToken::default();
            let mut run = harness.run(asset_local_test_context(), &cancel);
            let name = id.to_string();
            if !(run.open_path(&package) && run.rename(L1, &name, None)) {
                std::process::exit(9);
            }
            let revision = run.snapshot().unwrap().document().revision;
            append_line(&dir.join(format!("intent-{id}.jsonl")), &json!({"name":name,"revision":revision}));
            let (watched, prefix) = (dir.clone(), format!("doc.handshake.tmp-{}-", std::process::id()));
            std::thread::spawn(move || {
                loop {
                    let temp = std::fs::read_dir(&watched).into_iter().flatten().flatten().any(|e| {
                        let name = e.file_name().to_string_lossy().into_owned();
                        name.starts_with(&prefix) && !name.ends_with(".owner")
                    });
                    if temp {
                        std::process::exit(7);
                    }
                }
            });
            let landed = run.save_to(&package);
            let report = run.finish("exiter");
            let save = op(&report, "save");
            append_line(&dir.join(format!("log-{id}.jsonl")), &json!({"name":name,"revision":save["revision"],"result":if landed { json!("landed") } else { save["error"].clone() }}));
            std::process::exit(8);
        }
        // Holds the save lock of PACKAGE until go, then exits without releasing it.
        "hold_lock" => {
            let package = PathBuf::from(spec["package"].as_str().unwrap());
            let lock = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(hsk_studio_session::save_lock_path(&package)).unwrap();
            lock.lock().unwrap();
            std::fs::write(dir.join(format!("opened-{id}")), b"").unwrap();
            wait_for(&dir.join("go"));
            std::process::exit(0);
        }
        other => panic!("unknown child mode {other}"),
    }
}

fn append_line(path: &Path, line: &Value) {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
    writeln!(file, "{line}").unwrap();
}

fn read_lines(dir: &Path, prefix: &str) -> Vec<Value> {
    let mut lines = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        if entry.file_name().to_string_lossy().starts_with(prefix) {
            for line in std::fs::read_to_string(entry.path()).unwrap().lines() {
                lines.push(serde_json::from_str::<Value>(line).unwrap());
            }
        }
    }
    lines
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

/// `--expect-revision N` names the revision the file was opened at and the first edit carries it: the later edits of the
/// command line follow, also after an edit that changes nothing and so keeps the revision.
#[test]
fn expect_revision_covers_every_edit_of_one_command_line() {
    let scratch = Scratch::new("multi-edit");
    let package = scratch.package();
    let p = package.to_str().unwrap();
    let (exit, r) = cli_json(&["edit", p, "--provider", "chronicle.patch", "--expect-revision", "9", "--rename", L1, "one", "--visible", L1, "false", "--save", p, "--overwrite"]);
    assert_eq!(exit, 0, "{r}");
    assert_eq!(op(&r, "save")["detail"]["revision"], 11, "{r}");
    assert_eq!(cli_json(&["verify", p, "--expect-revision", "11", "--expect-name", L1, "one", "--expect-visible", L1, "false"]).0, 0);
    // The fixture's L1 is visible, so the first edit changes nothing and the revision stays 9.
    let copy = scratch.0.join("no-op-first.handshake");
    std::fs::copy(fixture_path(), &copy).unwrap();
    let c = copy.to_str().unwrap();
    let (exit, n) = cli_json(&["edit", c, "--provider", "chronicle.patch", "--expect-revision", "9", "--visible", L1, "true", "--rename", L1, "x", "--save", c, "--overwrite"]);
    assert_eq!(exit, 0, "{n}");
    assert_eq!(cli_json(&["verify", c, "--expect-revision", "10", "--expect-name", L1, "x"]).0, 0);
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

/// The soak of the lane: real processes save one package in a loop while others exit in the middle of a save. No update
/// is lost silently: every attempt lands or refuses with a typed code; no two landings share a revision, so none
/// replaced another it had not seen; the only revisions no log names belong to a process that exited after its rename;
/// the file holds the newest landing. A crash leaves a temp and its marker, harmless; the next save takes over the lock.
#[test]
fn saves_looped_from_processes_with_mid_save_exits_lose_nothing() {
    const WRITERS: usize = 4;
    const ATTEMPTS: u64 = 40;
    const EXITS: usize = 16;
    let scratch = Scratch::new("soak");
    let package = scratch.package();
    let p = package.to_str().unwrap();
    let writers: Vec<Child> = (0..WRITERS)
        .map(|i| spawn(&json!({"mode":"soak","dir":scratch.0,"package":package,"id":format!("w{i}"),"attempts":ATTEMPTS})))
        .collect();
    let mut exit_codes = Vec::new();
    for i in 0..EXITS {
        exit_codes.push(finish(spawn(&json!({"mode":"exiter","dir":scratch.0,"package":package,"id":format!("x{i}")}))));
    }
    let writer_exits: Vec<i32> = writers.into_iter().map(finish).collect();
    assert!(writer_exits.iter().all(|e| *e == 0), "{writer_exits:?}");

    let log = read_lines(&scratch.0, "log-");
    let intents = read_lines(&scratch.0, "intent-");
    let count = |code: &str| log.iter().filter(|l| l["result"] == code).count();
    let (landed, changed, busy) = (count("landed"), count("destination_changed"), count("save_busy"));
    let open_failed = log.iter().filter(|l| l["result"].as_str().is_some_and(|r| r.starts_with("open_failed"))).count();
    assert_eq!(landed + changed + busy + open_failed, log.len(), "every attempt landed or refused with a typed code: {log:?}");
    assert_eq!(log.len(), WRITERS * ATTEMPTS as usize + exit_codes.iter().filter(|e| **e == 8).count());

    // No two landings at one revision: a second one would have replaced a save it had not seen.
    let mut revisions: Vec<u64> = log.iter().filter(|l| l["result"] == "landed").map(|l| l["revision"].as_u64().unwrap()).collect();
    revisions.sort_unstable();
    let distinct = revisions.len();
    revisions.dedup();
    assert_eq!(revisions.len(), distinct, "two saves landed at one revision: {log:?}");

    // What is on disk now is the newest landing, or the save of a process that exited right after its rename.
    let (exit, opened) = cli_json(&["open", p]);
    assert_eq!(exit, 0, "the file opens: {opened}");
    let on_disk_revision = op(&opened, "summary")["revision"].as_u64().unwrap();
    let on_disk_name = op(&opened, "summary")["detail"]["layers"].as_array().unwrap().iter().find(|l| l["id"] == L1).unwrap()["name"].as_str().unwrap().to_string();
    let newest_logged = revisions.last().copied().unwrap_or(9);
    assert!(on_disk_revision >= newest_logged, "a landing was lost: {on_disk_revision} < {newest_logged}");
    let holder = log
        .iter()
        .filter(|l| l["result"] == "landed")
        .chain(intents.iter())
        .find(|l| l["revision"] == on_disk_revision && l["name"] == on_disk_name.as_str());
    assert!(holder.is_some(), "revision {on_disk_revision} name {on_disk_name} is nobody's save");

    // Every revision from 10 up has its one writer: a logged landing, or an intent of a process that exited.
    let unlogged: Vec<u64> = (10..=on_disk_revision).filter(|r| !revisions.contains(r)).collect();
    for revision in &unlogged {
        assert!(intents.iter().any(|i| i["revision"] == *revision), "revision {revision} has no writer: {log:?}");
    }

    // Leftovers of the exits, then a normal save: it works, and takes over any lock left behind.
    let names = scratch.leftovers(&[package.as_path()]);
    let temps = names.iter().filter(|n| n.starts_with("doc.handshake.tmp-") && !n.ends_with(".owner")).count();
    let exited_mid_save = exit_codes.iter().filter(|e| **e == 7).count();
    assert!(exited_mid_save > 0 && temps > 0, "no process exited mid-save: {exit_codes:?} {names:?}");
    let revision = on_disk_revision.to_string();
    let (exit, r) = cli_json(&["edit", p, "--provider", "chronicle.patch", "--expect-revision", &revision, "--rename", L1, "after-the-soak", "--save", p, "--overwrite"]);
    assert_eq!(exit, 0, "{r}");
    assert!(!hsk_studio_session::save_lock_path(&package).exists(), "the lock sidecar is gone");
    let left = scratch.leftovers(&[package.as_path()]);
    assert!(left.iter().all(|n| n.starts_with("doc.handshake.tmp-")), "only the crashed saves' own temps and markers: {left:?}");

    println!(
        "measured: writers {WRITERS} attempts_each {ATTEMPTS} attempts {} landed {landed} destination_changed {changed} save_busy {busy} open_failed {open_failed}; exiting processes {EXITS} exited_mid_save {exited_mid_save} finished_first {} other {}; revisions landed by an exiting process {}; temps left {temps}; final revision {on_disk_revision}",
        log.len(),
        exit_codes.iter().filter(|e| **e == 8).count(),
        exit_codes.iter().filter(|e| **e != 7 && **e != 8).count(),
        unlogged.len(),
    );
}

/// A process holding the save lock makes another wait, and when it exits without releasing, the waiter lands.
#[test]
fn a_save_waits_for_the_lock_and_a_process_that_exits_holding_it_blocks_nothing() {
    let scratch = Scratch::new("lock-exit");
    let package = scratch.package();
    let p = package.to_str().unwrap();
    let holder = spawn(&json!({"mode":"hold_lock","dir":scratch.0,"package":package,"id":"h"}));
    wait_for(&scratch.0.join("opened-h"));
    let go = scratch.0.join("go");
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        std::fs::write(go, b"").unwrap();
    });
    let start = Instant::now();
    let (exit, r) = cli_json(&["edit", p, "--provider", "chronicle.patch", "--expect-revision", "9", "--rename", L1, "after-the-lock", "--save", p, "--overwrite"]);
    let waited = start.elapsed();
    release.join().unwrap();
    assert_eq!(finish(holder), 0);
    assert_eq!(exit, 0, "{r}");
    assert!(waited >= Duration::from_millis(300), "the save waited for the holder: {waited:?}");
    assert_eq!(verify_name(&package, "after-the-lock").0, 0);
    assert_eq!(scratch.leftovers(&[package.as_path()]), Vec::<String>::new(), "the sidecar is gone");
}

/// Every refusal has a way out and the edits survive it: the same run saves elsewhere, or replaces on purpose, or
/// (the command line's way) the command runs again from the current revision.
#[test]
fn a_refused_save_keeps_the_edits_and_every_way_out_works() {
    let scratch = Scratch::new("way-out");
    let package = scratch.package();
    let p = package.to_str().unwrap();
    let harness = Harness::new(Selection { providers: vec!["chronicle.patch".to_string()], ..Selection::default() }).unwrap();
    let cancel = CancellationToken::default();
    let mut run = harness.run(asset_local_test_context(), &cancel);
    assert!(run.open_path(&package));
    assert!(run.rename(L1, "mine", None));

    // Another process's save lands first (here: a command line in this test, the writer is what matters).
    let (exit, r) = cli_json(&["edit", p, "--provider", "chronicle.patch", "--expect-revision", "9", "--rename", L1, "theirs", "--save", p, "--overwrite"]);
    assert_eq!(exit, 0, "{r}");
    let theirs = std::fs::read(&package).unwrap();

    // destination_changed: refused, the edit is still in the run.
    assert!(!run.save_to(&package));
    assert_eq!(run.snapshot().unwrap().document().layers.iter().find(|l| l.layer_id == L1).unwrap().name, "mine");
    let refused = run.ops().last().unwrap().clone();
    assert_eq!(refused["error"], "destination_changed", "{refused}");
    assert!(refused["detail"]["recovery"].as_str().unwrap().contains("another path"), "{refused}");
    assert_eq!(std::fs::read(&package).unwrap(), theirs);

    // Way out 1: another path. Nothing is lost on either side.
    let elsewhere = scratch.0.join("elsewhere.handshake");
    assert!(run.save_new(&elsewhere), "{:?}", run.ops().last());
    assert_eq!(verify_name(&elsewhere, "mine").0, 0);
    assert_eq!(std::fs::read(&package).unwrap(), theirs);

    // Way out 2: replace on purpose, once the caller has looked.
    assert!(run.save_replacing(&package), "{:?}", run.ops().last());
    assert_eq!(verify_name(&package, "mine").0, 0);
    // From then on the run compares against what it wrote: the next save lands.
    assert!(run.rename(L1, "mine-2", None));
    assert!(run.save_to(&package), "{:?}", run.ops().last());

    // destination_missing: the same two ways out, and save_new recreates it without a replace.
    std::fs::remove_file(&package).unwrap();
    assert!(!run.save_to(&package));
    let refused = run.ops().last().unwrap().clone();
    assert_eq!(refused["error"], "destination_missing", "{refused}");
    assert!(refused["detail"]["recovery"].as_str().unwrap().contains("another path"), "{refused}");
    assert!(!package.exists());
    assert!(run.save_new(&package), "{:?}", run.ops().last());
    assert_eq!(verify_name(&package, "mine-2").0, 0);
    drop(run.finish("way-out"));

    // The command line's way: a stale command is refused, `verify` or `open` gives the revision, the next run lands.
    let (exit, r) = cli_json(&["edit", p, "--provider", "chronicle.patch", "--expect-revision", "9", "--rename", L1, "again", "--save", p, "--overwrite"]);
    assert!(exit == 1 && op(&r, "rename")["error"] == "stale_revision", "{r}");
    let (_, opened) = cli_json(&["open", p]);
    let revision = op(&opened, "summary")["revision"].as_u64().unwrap().to_string();
    let (exit, r) = cli_json(&["edit", p, "--provider", "chronicle.patch", "--expect-revision", &revision, "--rename", L1, "again", "--save", p, "--overwrite"]);
    assert_eq!(exit, 0, "{r}");

    // destination_exists: --overwrite is the way out, tested in save_over_another_existing_file_needs_overwrite.
}

/// The Viewer: a refused save over the opened file keeps the edit in the viewer, and saving elsewhere lands it.
#[cfg(feature = "viewer")]
#[test]
fn a_refused_viewer_save_keeps_the_edit_and_saving_elsewhere_lands_it() {
    use hsk_studio_harness::view::{Viewer, render::Prefer};
    let scratch = Scratch::new("viewer-way-out");
    let package = scratch.package();
    let p = package.to_str().unwrap();
    let mut viewer = Viewer::new(Prefer::Cpu);
    viewer.open(&package).unwrap();
    viewer.set_visible(L1, false).unwrap();

    let (exit, r) = cli_json(&["edit", p, "--provider", "chronicle.patch", "--expect-revision", "9", "--rename", L1, "theirs", "--save", p, "--overwrite"]);
    assert_eq!(exit, 0, "{r}");
    let theirs = std::fs::read(&package).unwrap();

    let refused = viewer.save(&package, true).unwrap_err();
    assert!(refused.contains("destination_changed"), "{refused}");
    assert_eq!(std::fs::read(&package).unwrap(), theirs);
    let elsewhere = scratch.0.join("elsewhere.handshake");
    viewer.save(&elsewhere, false).unwrap();
    assert_eq!(cli_json(&["verify", elsewhere.to_str().unwrap(), "--expect-visible", L1, "false"]).0, 0);
    assert_eq!(std::fs::read(&package).unwrap(), theirs);
}

/// Restores a directory's permission to take new files when dropped.
struct DeniedDirectory(PathBuf);

impl DeniedDirectory {
    /// `None` when this machine cannot make a directory refuse new files for this process (then nothing is proven here).
    fn new(dir: &Path) -> Option<Self> {
        let denied = Self(dir.to_path_buf());
        denied.set(true);
        let probe = dir.join("probe");
        if std::fs::write(&probe, b"").is_ok() {
            let _ = std::fs::remove_file(&probe);
            denied.set(false);
            return None;
        }
        Some(denied)
    }
    #[cfg(windows)]
    fn set(&self, deny: bool) {
        // The directory's read-only attribute does not stop new files on Windows; an ACL entry for Everyone does.
        const EVERYONE: &str = "*S-1-1-0";
        let mut command = Command::new("icacls");
        command.arg(&self.0).stdout(Stdio::null()).stderr(Stdio::null());
        if deny {
            command.args(["/deny", &format!("{EVERYONE}:(WD,AD)")]);
        } else {
            command.args(["/remove:d", EVERYONE]);
        }
        let _ = command.status();
    }
    #[cfg(not(windows))]
    fn set(&self, deny: bool) {
        let mut permissions = std::fs::metadata(&self.0).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(deny);
        let _ = std::fs::set_permissions(&self.0, permissions);
    }
}

impl Drop for DeniedDirectory {
    fn drop(&mut self) {
        self.set(false);
    }
}

/// A directory that takes no new file: neither the temp nor the lock sidecar can exist there. Refused typed, also for
/// an in-place save of a package already there, and the package is untouched.
#[test]
fn a_directory_that_takes_no_new_file_refuses_typed() {
    let scratch = Scratch::new("denied-dir");
    let package = scratch.package();
    let p = package.to_str().unwrap();
    let before = std::fs::read(&package).unwrap();
    let locked = scratch.0.join("locked");
    std::fs::create_dir(&locked).unwrap();
    let inside = locked.join("doc.handshake");
    std::fs::copy(&package, &inside).unwrap();
    let harness = Harness::new(Selection { providers: vec!["chronicle.patch".to_string()], ..Selection::default() }).unwrap();
    let cancel = CancellationToken::default();
    let mut run = harness.run(asset_local_test_context(), &cancel);
    assert!(run.open_path(&inside));
    assert!(run.rename(L1, "x", None));
    let Some(denied) = DeniedDirectory::new(&locked) else {
        println!("NOT_PROVEN: this machine could not make a directory refuse new files");
        return;
    };
    let out = locked.join("new.handshake");
    let (exit, r) = cli_json(&["edit", p, "--provider", "chronicle.patch", "--rename", L1, "x", "--save", out.to_str().unwrap()]);
    let in_place = run.save_to(&inside);
    let refused = run.ops().last().unwrap().clone();
    drop(denied);
    assert!(exit == 1 && op(&r, "save")["error"] == "destination_read_only", "{r}");
    assert!(!in_place && refused["error"] == "destination_read_only", "{refused}");
    assert_eq!(std::fs::read(&package).unwrap(), before);
    assert_eq!(std::fs::read(&inside).unwrap(), before);
    assert_eq!(scratch.leftovers(&[package.as_path(), locked.as_path()]), Vec::<String>::new());
    assert_eq!(std::fs::read_dir(&locked).unwrap().count(), 1, "no temp or lock inside");
    // The way out: the directory takes files again, the same run saves.
    assert!(run.save_to(&inside), "{:?}", run.ops().last());
    drop(run.finish("denied-dir"));
}
