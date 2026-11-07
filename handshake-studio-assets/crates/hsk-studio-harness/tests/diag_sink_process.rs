//! Lane SMB-OBSERVE-DIAG-SINK-1: the real `studio-harness` example binary, spawned. What it ran survives it, `health` is
//! typed, and no test reaches the real default diag dir: every Command here sets every variable the default is resolved
//! from to a directory under this test's scratch, and a child that names no diag dir writes there.
use hsk_studio_harness::cli;
use hsk_studio_harness::manual::{COMMANDS, FLAGS};
use hsk_studio_harness_core::diag_sink;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const FIXTURE: &[u8] = include_bytes!("fixtures/demo.handshake");
const L2: &str = "SLYR-019abcde-0000-7000-8000-000000000002";
const DECOY_NAME: &str = "DECOY-LAYER-NAME-zqxj";
const DECOY_FILE: &str = "decoy-project-file-wolf";
const ENV: &str = "STUDIO_DIAG_DIR";

struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hsk-diag-process-{}-{label}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The example `cargo test` builds beside the test binaries. Absent is NOT_PROVEN, never a pass.
fn binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let path = exe.parent()?.parent()?.join("examples").join(format!("studio-harness{}", std::env::consts::EXE_SUFFIX));
    if path.is_file() {
        return Some(path);
    }
    println!("NOT_PROVEN: the studio-harness example is not built beside the test target ({})", path.display());
    None
}

/// A child whose default diag dir is `local`, whatever the machine's is: the only way a test spawns the binary.
fn studio(binary: &Path, local: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(binary);
    command
        .args(args)
        .env_remove(ENV)
        .env("LOCALAPPDATA", local)
        .env("XDG_STATE_HOME", local)
        .env("HOME", local)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}
fn output(command: &mut Command) -> Output {
    command.output().unwrap()
}
fn json_of(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&output.stdout)))
}

fn entries_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).map(|e| e.flatten().collect::<Vec<_>>()).unwrap_or_default() {
        found.push(entry.path());
        if entry.path().is_dir() {
            found.extend(entries_under(&entry.path()));
        }
    }
    found
}
fn diag_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = entries_under(dir).into_iter().filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("studio-diag-"))).collect();
    files.sort();
    files
}
fn lines(dir: &Path) -> Vec<Value> {
    diag_files(dir).iter().flat_map(|f| fs::read_to_string(f).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect::<Vec<Value>>()).collect()
}
fn kinds(lines: &[Value]) -> Vec<&str> {
    lines.iter().map(|l| l["kind"].as_str().unwrap()).collect()
}

#[test]
fn a_spawned_binary_with_the_variable_set_writes_nothing_to_the_default_location() {
    let Some(binary) = binary() else { return };
    let scratch = Scratch::new("variable");
    let (local, named) = (scratch.path("local"), scratch.path("named"));
    fs::create_dir_all(&local).unwrap();
    let out = output(studio(&binary, &local, &["providers"]).env(ENV, &named));
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(entries_under(&local).is_empty(), "written to the default location: {:?}", entries_under(&local));
    let all = lines(&named);
    assert_eq!(kinds(&all), ["segment_start", "run_start", "run_end"]);
    assert_eq!((all[2]["command"].as_str(), all[2]["exit"].as_i64()), (Some("providers"), Some(0)));
    // --diag-dir does the same, and wins over the variable.
    let (flagged, other) = (scratch.path("flagged"), scratch.path("other"));
    let args = ["providers", "--diag-dir", flagged.to_str().unwrap()];
    assert_eq!(output(studio(&binary, &local, &args).env(ENV, &other)).status.code(), Some(0));
    assert!(entries_under(&local).is_empty() && !other.exists());
    assert_eq!(diag_files(&flagged).len(), 1);
    // Counterfactual: named nowhere, the same run writes exactly to the default location, here under the scratch.
    assert_eq!(output(&mut studio(&binary, &local, &["providers"])).status.code(), Some(0));
    let default_dir = local.join("hsk-studio").join("diag");
    assert_eq!(diag_files(&local).len(), 1, "{:?}", entries_under(&local));
    assert!(diag_files(&local)[0].starts_with(&default_dir), "{:?}", diag_files(&local));
}

#[test]
fn a_spawned_edit_leaves_what_ran_in_order_with_codes_and_counters_and_no_names_or_paths() {
    let Some(binary) = binary() else { return };
    let scratch = Scratch::new("edit");
    let (local, diag) = (scratch.path("local"), scratch.path("diag"));
    let package = scratch.path(&format!("{DECOY_FILE}.handshake"));
    fs::write(&package, FIXTURE).unwrap();
    let saved = scratch.path(&format!("{DECOY_FILE}-out.handshake"));
    let args = ["edit", package.to_str().unwrap(), "--provider", "chronicle.patch", "--rename", L2, DECOY_NAME, "--save", saved.to_str().unwrap(), "--diag-dir", diag.to_str().unwrap()];
    let out = output(&mut studio(&binary, &local, &args));
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stdout));
    let report = json_of(&out);
    assert!(saved.is_file() && !local.exists());

    let all = lines(&diag);
    assert_eq!(all.iter().map(|l| l["seq"].as_u64().unwrap()).collect::<Vec<_>>(), (1..=all.len() as u64).collect::<Vec<_>>(), "in order, none missing");
    assert_eq!(all[1]["kind"], "run_start");
    assert_eq!(all[1]["command"], "edit");
    let end = all.last().unwrap();
    assert_eq!((end["kind"].as_str(), end["exit"].as_i64()), (Some("run_end"), Some(0)));
    // The same operations as the report on stdout, in its order, with its counters.
    let ops: Vec<&Value> = all.iter().filter(|l| l["kind"] == "op").collect();
    let reported = report["ops"].as_array().unwrap();
    assert!(reported.len() >= 3, "{report}");
    assert_eq!(ops.len(), reported.len());
    for (line, op) in ops.iter().zip(reported) {
        assert_eq!((&line["op"], &line["ok"], &line["revision"], &line["frames"]), (&op["op"], &op["ok"], &op["revision"], &op["observe"]["frames"]));
        assert_eq!(line["correlation_id"], op["correlation"]);
    }
    let frames: Vec<&Value> = all.iter().filter(|l| l["kind"] == "frame").collect();
    assert_eq!(frames.len() as u64, report["observe"]["frames_total"].as_u64().unwrap());
    assert!(frames.iter().all(|f| f["event_code"].is_u64() && f["sequence_id"].is_u64() && f["counter_a"].is_u64() && f["resource_id"].is_string()), "{frames:?}");
    assert!(frames.iter().any(|f| f["code"] == "Success"), "{frames:?}");

    let text = fs::read_to_string(&diag_files(&diag)[0]).unwrap();
    assert!(!text.contains(DECOY_NAME), "a layer name in the diag file");
    assert!(!text.contains(DECOY_FILE), "a user path in the diag file");
}

/// A command nobody knows, with a path where a command goes: the run is recorded and the text is not.
#[test]
fn an_unknown_command_line_is_recorded_without_its_text() {
    let Some(binary) = binary() else { return };
    let scratch = Scratch::new("unknown");
    let (local, diag) = (scratch.path("local"), scratch.path("diag"));
    let out = output(studio(&binary, &local, &[DECOY_FILE, DECOY_NAME]).env(ENV, &diag));
    assert_ne!(out.status.code(), Some(0));
    let all = lines(&diag);
    assert_eq!(kinds(&all), ["segment_start", "run_start", "run_end"]);
    assert_eq!(all[1]["command"], "unknown");
    assert_eq!(all[2]["exit"].as_i64(), out.status.code().map(i64::from));
    let text = fs::read_to_string(&diag_files(&diag)[0]).unwrap();
    assert!(!text.contains(DECOY_FILE) && !text.contains(DECOY_NAME));
}

#[test]
fn health_prints_json_and_exits_zero_with_a_usable_dir() {
    let Some(binary) = binary() else { return };
    let scratch = Scratch::new("health");
    let (local, diag) = (scratch.path("local"), scratch.path("diag"));
    assert_eq!(output(studio(&binary, &local, &["providers"]).env(ENV, &diag)).status.code(), Some(0));
    let out = output(studio(&binary, &local, &["health"]).env(ENV, &diag));
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stdout));
    let report = json_of(&out);
    assert_eq!((report["command"].as_str(), report["ok"].as_bool(), report["verdict"].as_str()), (Some("health"), Some(true), Some("pass")));
    assert!(report["error"].is_null());
    let checks = report["checks"].as_array().unwrap();
    let check = |id: &str| checks.iter().find(|c| c["id"] == id).unwrap_or_else(|| panic!("no check {id}: {report}"));
    assert_eq!(check("descriptors")["ok"], true);
    assert!(check("descriptors")["count"].as_u64().unwrap() >= 5);
    let sink = check("diag_sink");
    assert_eq!((sink["ok"].as_bool(), sink["status"].as_str()), (Some(true), Some("active")));
    assert!(sink["lines_written"].as_u64().unwrap() >= 1);
    // The earlier run is there and ended; this run does not count itself.
    let previous = &report["previous_runs"];
    assert_eq!((previous["runs_surveyed"].as_u64(), previous["unterminated_or_running"].as_u64(), previous["panic_files"].as_u64()), (Some(1), Some(0), Some(0)));
    // The probe is on disk.
    assert!(lines(&diag).iter().any(|l| l["kind"] == "health"));
}

/// Named by the variable or by the default: a location that cannot be written fails no command, and `health` says so.
#[test]
fn a_location_that_cannot_be_written_never_fails_the_command_and_health_is_typed() {
    let Some(binary) = binary() else { return };
    let scratch = Scratch::new("unwritable");
    let (local, blocked, good) = (scratch.path("local"), scratch.path("blocked"), scratch.path("good"));
    fs::write(&blocked, b"a file, not a directory").unwrap();
    let working = output(studio(&binary, &local, &["providers"]).env(ENV, &good));
    assert_eq!(working.status.code(), Some(0));

    let out = output(studio(&binary, &local, &["providers"]).env(ENV, &blocked));
    assert_eq!((out.status.code(), &out.stdout), (Some(0), &working.stdout), "the command's result does not change");
    let out = output(studio(&binary, &local, &["health"]).env(ENV, &blocked));
    assert_eq!(out.status.code(), Some(diag_sink::HEALTH_EXIT_DIAG_UNAVAILABLE), "{}", String::from_utf8_lossy(&out.stdout));
    let report = json_of(&out);
    assert_eq!((report["ok"].as_bool(), report["verdict"].as_str(), report["error"].as_str()), (Some(false), Some("fail"), Some("diag_unavailable")));

    // The default location, blocked: no fallback anywhere else, and the same answers.
    let out = output(Command::new(&binary).arg("providers").env_remove(ENV).env("LOCALAPPDATA", &blocked).env("XDG_STATE_HOME", &blocked).env("HOME", &blocked).stdin(Stdio::null()));
    assert_eq!(out.status.code(), Some(0));
    let out = output(Command::new(&binary).arg("health").env_remove(ENV).env("LOCALAPPDATA", &blocked).env("XDG_STATE_HOME", &blocked).env("HOME", &blocked).stdin(Stdio::null()));
    assert_eq!(out.status.code(), Some(diag_sink::HEALTH_EXIT_DIAG_UNAVAILABLE));
    assert_eq!(fs::read(&blocked).unwrap(), b"a file, not a directory");
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_string()).collect()
}

/// `cli::run` is the pure surface every in-process test uses: it reaches a diag dir only when told one.
#[test]
fn cli_run_in_process_uses_no_default_and_health_without_a_dir_is_typed() {
    let out = cli::run(&args(&["health"]));
    assert_eq!(out.exit, diag_sink::HEALTH_EXIT_DIAG_NOT_CONFIGURED, "{}", out.stdout);
    let report: Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!((report["ok"].as_bool(), report["error"].as_str()), (Some(false), Some("diag_not_configured")));
    assert_eq!(cli::run(&args(&["providers"])).exit, 0);
    assert!(diag_sink::process_sink().is_none(), "only a host's main installs the process sink");
    assert!(diag_sink::default_diag_dir().ends_with("diag") || std::env::var_os(ENV).is_some());

    let scratch = Scratch::new("inprocess");
    let diag = scratch.path("diag");
    let out = cli::run(&args(&["health", "--diag-dir", diag.to_str().unwrap()]));
    assert_eq!(out.exit, 0, "{}", out.stdout);
    assert_eq!(kinds(&lines(&diag)), ["segment_start", "run_start", "health", "run_end"]);
    assert_eq!(cli::run(&args(&["health", "--diag-dir"])).exit, 2, "a flag with no value is a usage error");
    assert_eq!(cli::run(&args(&["health", "--diag-dir", ""])).exit, 2, "an empty value is a usage error");
}

#[test]
fn the_manual_names_the_command_the_flag_the_variable_and_the_default() {
    let health = COMMANDS.iter().find(|c| c.name == "health").expect("health in the command table");
    assert!(health.usage.contains("--diag-dir") && health.summary.contains("exit"), "{}", health.summary);
    assert!(FLAGS.iter().any(|(flag, _)| flag.starts_with("--diag-dir")));
    let manual: Value = serde_json::from_str(&cli::run(&args(&["help", "--json"])).stdout).unwrap();
    let d = &manual["diagnostics"];
    assert_eq!(d["dir_variable"], ENV);
    for (key, needles) in [
        ("default_dir", &["LOCALAPPDATA", "XDG_STATE_HOME", "hsk-studio"][..]),
        ("not_writable", &["no fallback", "health", "never"][..]),
        ("bounds", &["4194304", "16", "8192"][..]),
        ("hard_death", &["stack overflow", "run_end"][..]),
        ("tests", &["STUDIO_DIAG_DIR", "--diag-dir"][..]),
    ] {
        let text = d[key].to_string();
        for needle in needles {
            assert!(text.contains(needle), "diagnostics.{key} lacks {needle}: {text}");
        }
    }
    for (code, name) in [("3", "diag_not_configured"), ("4", "diag_unavailable"), ("5", "descriptor_mismatch")] {
        assert!(manual["output"]["exit_codes"][code].to_string().contains(name), "{}", manual["output"]);
    }
    let text = cli::run(&args(&["help"])).stdout;
    assert!(text.contains("DIAGNOSTICS") && text.contains(ENV) && text.contains("health"));
}
