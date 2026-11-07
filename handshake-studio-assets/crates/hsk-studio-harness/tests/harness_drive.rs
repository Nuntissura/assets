//! Lane SMB-HARNESS-DRIVE-1 (feature `viewer`): a model with no Rust test reads state and drives
//! Studio as a process. Every test here spawns the built binary and speaks to it through arguments
//! and stdin; the only in-process calls are the window classifier that guards each spawn and the
//! fixture writer. Fixture: `tests/fixtures/demo.handshake` (revision 9, group layer ...0001, raster
//! layer ...0002).
#![cfg(feature = "viewer")]

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness::cli::{self, WindowArgs};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const FIXTURE: &[u8] = include_bytes!("fixtures/demo.handshake");
const L1: &str = "SLYR-019abcde-0000-7000-8000-000000000001";
const L2: &str = "SLYR-019abcde-0000-7000-8000-000000000002";
/// A process that has not ended by then is treated as stuck (a window would block forever).
const PROCESS_LIMIT: Duration = Duration::from_secs(120);

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let base = std::env::temp_dir();
        let text = base.to_string_lossy().to_ascii_uppercase();
        assert!(!(cfg!(windows) && text.starts_with("C:")), "TMP is on C: ({}); run through studio-env.sh", base.display());
        let dir = base.join(format!("hsk-harness-drive-{}-{label}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn package(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, FIXTURE).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The built binary: `studio-harness` when feature `desktop` built it, otherwise the example of the
/// same name beside the test target (cargo builds examples for every test run). A missing binary
/// fails: this lane proves nothing without the real process.
fn binary() -> PathBuf {
    if let Some(bin) = option_env!("CARGO_BIN_EXE_studio-harness") {
        return PathBuf::from(bin);
    }
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().and_then(Path::parent).expect("target directory").to_path_buf();
    let example = dir.join("examples").join(format!("studio-harness{}", std::env::consts::EXE_SUFFIX));
    assert!(example.is_file(), "the studio-harness example was not built at {}", example.display());
    example
}

/// One quiet child: no console of its own, piped streams, and STUDIO_DIAG_DIR under TMP so a
/// default diagnostics directory (SMB-OBSERVE-DIAG-SINK-1) never lands outside the artifact root.
/// The classifier guards every spawn: arguments that would open a window never reach a process.
fn spawn(args: &[&str], diag: &Path) -> Child {
    let owned: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    assert!(!matches!(cli::window_args(&owned, &|p| p.is_file()), WindowArgs::Window { .. }), "{args:?} would open a window; not spawned");
    let mut command = Command::new(binary());
    command.args(args).env("STUDIO_DIAG_DIR", diag).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.spawn().expect("spawn studio-harness")
}

/// Waits for the end of `child` within [`PROCESS_LIMIT`], killing it past that, and gives the exit
/// code and stdout. stdout and stderr are drained on threads so a full pipe never blocks the child.
fn finish(mut child: Child, input: &[u8]) -> (i32, Vec<u8>) {
    let mut stdin = child.stdin.take().expect("stdin");
    let bytes = input.to_vec();
    // A child that stops reading closes the pipe; the write error is the child's business here.
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&bytes);
    });
    let mut stdout = child.stdout.take().expect("stdout");
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stdout, &mut out);
        out
    });
    let mut stderr = child.stderr.take().expect("stderr");
    let errors = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stderr, &mut out);
        out
    });
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        if start.elapsed() > PROCESS_LIMIT {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the process did not end within {PROCESS_LIMIT:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    writer.join().unwrap();
    let out = reader.join().unwrap();
    let errors = errors.join().unwrap();
    assert!(errors.is_empty(), "stderr: {}", String::from_utf8_lossy(&errors));
    (status.code().expect("an exit code, not a crash"), out)
}

/// A one-shot command: exit code and its one JSON document.
fn run(args: &[&str], diag: &Path) -> (i32, Value) {
    let (exit, out) = finish(spawn(args, diag), b"");
    let text = String::from_utf8(out).expect("utf-8 stdout");
    (exit, serde_json::from_str(&text).unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {text}")))
}

/// A stdin session on `package`: exit code and one JSON value per stdout line.
fn session(package: &Path, actor: &str, client_session: &str, input: &[u8], diag: &Path) -> (i32, Vec<Value>) {
    let p = package.to_str().unwrap();
    let (exit, out) = finish(spawn(&["command", p, "--actor", actor, "--session", client_session, "--stdin"], diag), input);
    let text = String::from_utf8(out).expect("utf-8 stdout");
    let lines = text.lines().map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("line is not JSON ({e}): {l}"))).collect();
    (exit, lines)
}

fn lines(values: &[Value]) -> Vec<u8> {
    values.iter().flat_map(|v| format!("{v}\n").into_bytes()).collect()
}

fn layer<'a>(state: &'a Value, id: &str) -> &'a Value {
    state["layers"].as_array().unwrap().iter().find(|l| l["id"] == id).unwrap_or_else(|| panic!("no layer {id} in {state}"))
}

fn rename(id: &str, name: &str) -> Value {
    json!({"command":"command","command_id":"layer.rename","parameters":{"layer":id,"name":name}})
}

#[test]
fn state_prints_the_revision_and_the_state_of_the_package() {
    let dir = Scratch::new("state");
    let package = dir.package("a.handshake");
    let (exit, r) = run(&["state", package.to_str().unwrap(), "--actor", "lane-a", "--session", "s-1"], &dir.0);
    assert_eq!(exit, 0, "{r}");
    assert_eq!((r["command"].as_str(), r["ok"].as_bool(), r["verdict"].as_str()), (Some("state"), Some(true), Some("pass")));
    assert_eq!(r["revision"], 9);
    assert_eq!(r["state"]["revision"], 9);
    assert_eq!(layer(&r["state"], L1)["kind"], "group");
    assert_eq!(layer(&r["state"], L2)["visible"], false);
    for key in ["edit", "shell", "develop", "frame", "ops"] {
        assert!(r["state"].get(key).is_some(), "state_json key {key} missing");
    }
}

#[test]
fn every_response_prints_the_resolved_attribution() {
    let dir = Scratch::new("attribution");
    let p = dir.package("a.handshake");
    let p = p.to_str().unwrap();
    let (exit, r) = run(&["state", p, "--actor", "lane-a", "--session", "s-1"], &dir.0);
    assert_eq!(exit, 0, "{r}");
    let a = &r["attribution"];
    assert_eq!((a["actor"]["account_id"].as_str(), a["actor"]["principal_id"].as_str()), (Some("lane-a"), Some("lane-a")));
    assert_eq!((a["actor"]["owner_account_id"].as_str(), a["actor"]["access_space_id"].as_str()), (Some("lane-a"), Some("lane-a")), "the defaults are visible");
    assert_eq!((a["actor"]["session_id"].as_str(), a["client_session"].as_str()), (Some("s-1"), Some("s-1")));

    let (exit, r) = run(&["state", p, "--actor", "lane-a", "--session", "s-1", "--owner", "op", "--access-space", "space-9"], &dir.0);
    assert_eq!(exit, 0, "{r}");
    let a = &r["attribution"]["actor"];
    assert_eq!((a["principal_id"].as_str(), a["owner_account_id"].as_str(), a["owner_principal_id"].as_str(), a["access_space_id"].as_str()), (Some("lane-a"), Some("op"), Some("op"), Some("space-9")));
}

#[test]
fn a_command_is_attributed_and_saved_and_a_new_process_reads_it() {
    let dir = Scratch::new("command");
    let package = dir.package("a.handshake");
    let out = dir.0.join("out.handshake");
    let (p, o) = (package.to_str().unwrap(), out.to_str().unwrap());
    let (exit, r) = run(&["command", p, "--actor", "lane-a", "--session", "s-1", "--command-id", "layer.rename", "--param", "layer", L1, "--param", "name", "by-process", "--expect-revision", "9", "--save", o], &dir.0);
    assert_eq!(exit, 0, "{r}");
    assert_eq!((r["command_id"].as_str(), r["revision"].as_u64()), (Some("layer.rename"), Some(10)));
    let step = &r["state"]["edit"]["history"]["undo_steps"][0];
    assert_eq!((step["actor"]["principal_id"].as_str(), step["client_session"].as_str(), step["command_id"].as_str()), (Some("lane-a"), Some("s-1"), Some("layer.rename")));
    assert_eq!(step["base_revision"], 9);

    let (exit, again) = run(&["state", o, "--actor", "reader", "--session", "s-2"], &dir.0);
    assert_eq!(exit, 0, "{again}");
    assert_eq!(layer(&again["state"], L1)["name"], "by-process");
    assert_eq!(again["revision"], 10);
    let (exit, source) = run(&["state", p, "--actor", "reader", "--session", "s-2"], &dir.0);
    assert_eq!(exit, 0, "{source}");
    assert_ne!(layer(&source["state"], L1)["name"], "by-process", "the input package is untouched");
}

#[test]
fn every_driveable_row_of_the_manual_runs_from_help_alone() {
    let dir = Scratch::new("manual");
    let package = dir.package("a.handshake");
    let (exit, help) = run(&["help", "--json"], &dir.0);
    assert_eq!(exit, 0, "{help}");
    for name in ["state", "command", "undo"] {
        assert!(help["commands"].as_array().unwrap().iter().any(|c| c["name"] == name), "{name} missing from help");
    }
    let drive = &help["drive"];
    let rows = drive["commands"].as_array().expect("drive.commands");
    for id in ["layer.rename", "layer.set_visible", "layer.set_opacity", "layer.set_blend_mode", "layer.panel_set", "history.undo", "history.redo", "document.save"] {
        assert!(rows.iter().any(|r| r["command_id"] == id), "{id} missing from drive.commands");
    }
    // Values chosen from each row's declared parameter types, nothing from this file's knowledge
    // of the command: only the two layer ids of the fixture.
    let save_to = dir.0.join("from-manual.handshake");
    for row in rows {
        let mut parameters = serde_json::Map::new();
        for p in row["parameters"].as_array().unwrap() {
            let key = p["key"].as_str().unwrap();
            let value = match (key, p["value_type"].as_str().unwrap()) {
                ("path", _) => json!(save_to.to_str().unwrap()),
                ("control", _) => json!("name"),
                (_, "reference") => json!(L1),
                (_, "boolean") => json!(false),
                (_, "number") => json!(1),
                (_, "measured") => json!(50),
                (_, _) => json!("x"),
            };
            parameters.insert(key.to_string(), value);
        }
        let line = json!({"command":"command","command_id":row["command_id"],"parameters":parameters});
        let (_, replies) = session(&package, "lane-a", "s-1", &lines(&[line]), &dir.0);
        let reply = &replies[1];
        assert!(reply["ok"] == true || reply["error"] == "nothing_to_undo" || reply["error"] == "nothing_to_redo", "row {row} from help refused: {reply}");
    }
    assert!(save_to.is_file(), "document.save ran from the row's own parameters");
    let text = run_text(&["help"], &dir.0);
    for needle in ["--actor", "--session", "--owner", "--access-space", "--stdin", "undo_needs_live_session", "timeline.razor", "cmd.sequence.razorateditline", "layer.rename"] {
        assert!(text.contains(needle), "help text lacks {needle}");
    }
}

fn run_text(args: &[&str], diag: &Path) -> String {
    let (exit, out) = finish(spawn(args, diag), b"");
    assert_eq!(exit, 0);
    String::from_utf8(out).unwrap()
}

#[test]
fn a_command_without_attribution_is_a_usage_error_and_nothing_is_written() {
    let dir = Scratch::new("noactor");
    let package = dir.package("a.handshake");
    let out = dir.0.join("out.handshake");
    let (p, o) = (package.to_str().unwrap(), out.to_str().unwrap());
    let edit = ["--command-id", "layer.rename", "--param", "layer", L1, "--param", "name", "x", "--save", o];
    let cases: Vec<Vec<&str>> = vec![
        [&["command", p][..], &edit[..]].concat(),
        [&["command", p, "--actor", "a"][..], &edit[..]].concat(),
        [&["command", p, "--session", "s"][..], &edit[..]].concat(),
        vec!["undo", p],
        vec!["state", p],
        vec!["command", p, "--stdin"],
    ];
    for args in cases {
        let (exit, r) = run(&args, &dir.0);
        assert_eq!((exit, r["error"].as_str()), (2, Some("usage")), "{args:?}: {r}");
    }
    assert!(!out.exists(), "a refused command wrote a file");
    for (actor, session) in [("", "s"), ("a", ""), ("a\u{7}b", "s"), (&"a".repeat(129)[..], "s"), ("a", &"s".repeat(129)[..])] {
        let (exit, r) = run(&["state", p, "--actor", actor, "--session", session], &dir.0);
        assert_eq!((exit, r["error"].as_str(), r["ok"].as_bool()), (1, Some("actor_invalid"), Some(false)), "{r}");
    }
}

#[test]
fn saving_in_place_needs_overwrite_and_the_expected_revision() {
    let dir = Scratch::new("inplace");
    let package = dir.package("a.handshake");
    let p = package.to_str().unwrap();
    let base = ["command", p, "--actor", "a", "--session", "s", "--command-id", "layer.rename", "--param", "layer", L1, "--param", "name", "in-place", "--save", p];
    let before = std::fs::read(&package).unwrap();
    let (exit, r) = run(&base, &dir.0);
    assert_eq!((exit, r["error"].as_str()), (2, Some("usage")), "{r}");
    let (exit, r) = run(&[&base[..], &["--overwrite"]].concat(), &dir.0);
    assert_eq!((exit, r["error"].as_str()), (2, Some("usage")), "in place without --expect-revision: {r}");
    let (exit, r) = run(&[&base[..], &["--expect-revision", "9"]].concat(), &dir.0);
    assert_eq!((exit, r["error"].as_str()), (2, Some("usage")), "in place without --overwrite: {r}");
    let (exit, r) = run(&[&base[..], &["--overwrite", "--expect-revision", "8"]].concat(), &dir.0);
    assert_eq!((exit, r["error"].as_str()), (1, Some("stale_revision")), "{r}");
    assert_eq!(std::fs::read(&package).unwrap(), before, "every refusal left the package alone");
    let (exit, r) = run(&[&base[..], &["--overwrite", "--expect-revision", "9"]].concat(), &dir.0);
    assert_eq!(exit, 0, "{r}");
    let (exit, r) = run(&["state", p, "--actor", "a", "--session", "s"], &dir.0);
    assert_eq!((exit, layer(&r["state"], L1)["name"].as_str(), r["revision"].as_u64()), (0, Some("in-place"), Some(10)));

    // The same rule in a session line.
    let save = |extra: Value| {
        let mut parameters = json!({"path": p, "overwrite": true});
        let _ = &mut parameters;
        let mut line = json!({"command":"command","command_id":"document.save","parameters":parameters});
        if !extra.is_null() {
            line["expected_revision"] = extra;
        }
        line
    };
    let (exit, replies) = session(&package, "a", "s", &lines(&[save(Value::Null), save(json!(10))]), &dir.0);
    assert_eq!(exit, 1, "{replies:?}");
    assert_eq!(replies[1]["error"], "expect_revision_required");
    assert_eq!(replies[2]["ok"], true, "{}", replies[2]);
}

#[test]
fn one_shot_undo_refuses_and_names_the_live_session() {
    let dir = Scratch::new("undo");
    let package = dir.package("a.handshake");
    let (exit, r) = run(&["undo", package.to_str().unwrap(), "--actor", "a", "--session", "s"], &dir.0);
    assert_eq!((exit, r["ok"].as_bool(), r["error"].as_str()), (1, Some(false), Some("undo_needs_live_session")), "{r}");
    assert_eq!(r["revision"], 9);
    assert!(r["detail"]["hint"].as_str().unwrap().contains("--stdin"), "{r}");
    // The loop's own empty history is the plain refusal.
    let (exit, replies) = session(&package, "a", "s", &lines(&[json!({"command":"undo"})]), &dir.0);
    assert_eq!(exit, 1);
    assert_eq!(replies[1]["error"], "nothing_to_undo");
}

#[test]
fn a_stdin_session_runs_commands_undo_redo_and_state_in_one_process() {
    let dir = Scratch::new("loop");
    let package = dir.package("a.handshake");
    let input = lines(&[
        rename(L1, "one"),
        json!({"command":"command","command_id":"layer.set_visible","parameters":{"layer":L2,"visible":true},"expected_revision":10}),
        json!({"command":"state"}),
        json!({"command":"undo"}),
        json!({"command":"command","command_id":"history.redo"}),
        json!({"command":"undo"}),
        json!({"command":"undo"}),
        json!({"command":"state"}),
    ]);
    let (exit, r) = session(&package, "lane-a", "s-1", &input, &dir.0);
    assert_eq!(exit, 0, "{r:?}");
    assert_eq!(r.len(), 10, "open, 8 lines, end: {r:?}");
    assert_eq!((r[0]["command"].as_str(), r[0]["revision"].as_u64()), (Some("open"), Some(9)));
    assert_eq!(layer(&r[1]["state"], L1)["name"], "one");
    assert_eq!(layer(&r[2]["state"], L2)["visible"], true);
    assert_eq!(r[3]["revision"], r[2]["revision"], "state does not move the revision");
    assert_eq!(layer(&r[4]["state"], L2)["visible"], false, "undo");
    assert_eq!(layer(&r[5]["state"], L2)["visible"], true, "redo");
    assert_eq!(layer(&r[7]["state"], L1)["name"], layer(&r[0]["state"], L1)["name"], "two undos restore the name");
    // Every step carries the session's actor and client session; the undo is caused by its step.
    let undone = &r[7]["state"]["edit"]["history"]["redo_steps"];
    for step in undone.as_array().unwrap() {
        assert_eq!((step["actor"]["principal_id"].as_str(), step["client_session"].as_str()), (Some("lane-a"), Some("s-1")));
    }
    assert_eq!(undone.as_array().unwrap().last().unwrap()["command_id"], "history.undo");
    // One terminal frame per dispatched command, through observe.
    let ops = r[8]["state"]["ops"].as_array().unwrap();
    assert!(ops.iter().filter(|o| o["op"] != "open").all(|o| o["observe"]["frames"] == 1), "{ops:?}");
    let end = r.last().unwrap();
    assert_eq!((end["ended"].as_str(), end["lines"].as_u64(), end["refused"].as_u64()), (Some("stdin_closed"), Some(8), Some(0)));
}

#[test]
fn hostile_lines_refuse_and_the_session_continues() {
    let dir = Scratch::new("hostile");
    let package = dir.package("a.handshake");
    let mut input = Vec::new();
    let mut expect: Vec<&str> = Vec::new();
    let mut push = |bytes: &[u8], code: &'static str| {
        input.extend_from_slice(bytes);
        input.push(b'\n');
        expect.push(code);
    };
    push(b"{not json", "line_malformed");
    push(b"[1,2]", "line_malformed");
    push(b"\"state\"", "line_malformed");
    push(&[0xff, 0xfe, 0x00], "line_not_utf8");
    push(&vec![b'x'; 5 * 1024 * 1024], "line_too_long");
    let deep = format!("{}{}", "[".repeat(200_000), "]".repeat(200_000));
    push(deep.as_bytes(), "line_malformed");
    let deep_parameter = format!("{{\"command\":\"command\",\"command_id\":\"layer.rename\",\"parameters\":{{\"name\":{}{}}}}}", "[".repeat(100_000), "]".repeat(100_000));
    push(deep_parameter.as_bytes(), "line_malformed");
    push(br#"{"command":"state","extra":1}"#, "line_malformed");
    push(br#"{"command":"command"}"#, "line_malformed");
    push(br#"{"command":"command","command_id":"layer.rename","expected_revision":"nine","parameters":{}}"#, "line_malformed");
    push(br#"{"command":"launch"}"#, "unknown_command");
    push(br#"{"command":"command","command_id":"layer.nope"}"#, "unknown_command");
    push(br#"{"command":"command","command_id":"timeline.nope"}"#, "unknown_command");
    push(br#"{"command":"command","command_id":"layer.set_tiles"}"#, "command_not_driveable");
    push(br#"{"command":"command","command_id":"timeline.export_media_run"}"#, "command_not_driveable");
    push(br#"{"command":"command","command_id":"layer.rename","parameters":{"layer":"x"}}"#, "parameter_missing");
    push(br#"{"command":"command","command_id":"layer.rename","parameters":{"layer":"x","name":"y","colour":1}}"#, "parameter_unknown");
    push(br#"{"command":"command","command_id":"layer.set_visible","parameters":{"layer":"x","visible":"maybe"}}"#, "parameter_invalid");
    push(br#"{"command":"command","command_id":"layer.set_opacity","parameters":{"layer":"x","opacity":"NaN"}}"#, "parameter_invalid");
    push(br#"{"command":"command","command_id":"layer.set_blend_mode","parameters":{"layer":"x","blend_mode":70000}}"#, "parameter_invalid");
    push(br#"{"command":"command","command_id":"layer.rename","parameters":{"layer":"SLYR-nope","name":"y"}}"#, "unknown_layer");
    push(br#"{"command":"command","command_id":"layer.rename","parameters":{"layer":"SLYR-nope","name":"y"},"expected_revision":3}"#, "stale_revision");
    let good = rename(L1, "still-alive");
    let good = format!("{good}\n");
    input.extend_from_slice(good.as_bytes());
    let refusals = expect.len();

    let (exit, r) = session(&package, "a", "s", &input, &dir.0);
    assert_eq!(exit, 1, "a refused line makes the exit non-zero");
    assert_eq!(r.len(), refusals + 3, "open, one reply per line, end");
    for (n, code) in expect.iter().enumerate() {
        let reply = &r[n + 1];
        assert_eq!((reply["ok"].as_bool(), reply["error"].as_str()), (Some(false), Some(*code)), "line {}: {reply}", n + 1);
        assert_eq!(reply["line"].as_u64(), Some(n as u64 + 1));
    }
    assert_eq!(layer(&r[refusals + 1]["state"], L1)["name"], "still-alive", "the session survived every hostile line");
    let end = r.last().unwrap();
    assert_eq!((end["ended"].as_str(), end["refused"].as_u64()), (Some("stdin_closed"), Some(refusals as u64)));
}

#[test]
fn a_hostile_package_and_a_closed_stdin_end_cleanly() {
    let dir = Scratch::new("closed");
    let package = dir.package("a.handshake");
    let (exit, r) = session(&package, "a", "s", b"", &dir.0);
    assert_eq!(exit, 0, "{r:?}");
    assert_eq!(r.len(), 2);
    assert_eq!((r[1]["ended"].as_str(), r[1]["lines"].as_u64(), r[1]["refused"].as_u64()), (Some("stdin_closed"), Some(0), Some(0)));
    // A final line with no newline still counts as a line.
    let (exit, r) = session(&package, "a", "s", br#"{"command":"state"}"#, &dir.0);
    assert_eq!((exit, r.len(), r[1]["ok"].as_bool()), (0, 3, Some(true)), "{r:?}");

    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("deep.handshake", format!("{}{}", "[".repeat(200_000), "]".repeat(200_000)).into_bytes()),
        ("garbage.handshake", vec![0xA5; 4096]),
        ("empty.handshake", Vec::new()),
        ("truncated.handshake", FIXTURE[..FIXTURE.len() / 2].to_vec()),
    ];
    for (name, bytes) in cases {
        let path = dir.0.join(name);
        std::fs::write(&path, bytes).unwrap();
        let (exit, r) = run(&["state", path.to_str().unwrap(), "--actor", "a", "--session", "s"], &dir.0);
        assert_eq!((exit, r["ok"].as_bool()), (1, Some(false)), "{name}: {r}");
        assert!(r["error"].is_string(), "{name}: {r}");
        let (exit, replies) = session(&path, "a", "s", br#"{"command":"state"}"#, &dir.0);
        assert_eq!((exit, replies.len(), replies[0]["ok"].as_bool()), (1, 1, Some(false)), "{name} ends the session at open: {replies:?}");
    }
    let (exit, r) = run(&["state", dir.0.join("missing.handshake").to_str().unwrap(), "--actor", "a", "--session", "s"], &dir.0);
    assert_eq!((exit, r["ok"].as_bool()), (1, Some(false)), "{r}");
}

/// The same commands on the same package in two runs: byte-identical replies. Named fields that
/// cannot be identical would be listed here; today there are none.
#[test]
fn the_same_commands_on_the_same_package_give_byte_identical_output() {
    let dir = Scratch::new("determinism");
    let package = dir.package("a.handshake");
    let input = lines(&[
        rename(L1, "one"),
        json!({"command":"command","command_id":"layer.set_opacity","parameters":{"layer":L2,"opacity":40}}),
        json!({"command":"undo"}),
        json!({"command":"command","command_id":"history.redo"}),
        json!({"command":"state"}),
        json!({"command":"command","command_id":"layer.nope"}),
    ]);
    let p = package.to_str().unwrap();
    let args = ["command", p, "--actor", "lane-a", "--session", "s-1", "--stdin"];
    let (first_exit, first) = finish(spawn(&args, &dir.0), &input);
    let (second_exit, second) = finish(spawn(&args, &dir.0), &input);
    assert_eq!((first_exit, second_exit), (1, 1));
    assert!(first == second, "two runs differ:\n{}", first_difference(&first, &second));
}

fn first_difference(a: &[u8], b: &[u8]) -> String {
    let at = a.iter().zip(b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
    let window = |s: &[u8]| String::from_utf8_lossy(&s[at.saturating_sub(120)..(at + 120).min(s.len())]).into_owned();
    format!("offset {at}\n{}\n---\n{}", window(a), window(b))
}

/// Parallel lanes on one source: each process has its own session and actor, and writes its own
/// destination. Process-level save safety on one destination is SMB-SESSION-WRITE-SAFETY-1.
#[test]
fn parallel_sessions_on_one_source_do_not_touch_each_other() {
    let dir = Scratch::new("parallel");
    let package = dir.package("a.handshake");
    let handles: Vec<_> = (0..4)
        .map(|n| {
            let (package, diag) = (package.clone(), dir.0.clone());
            let out = dir.0.join(format!("out-{n}.handshake"));
            std::thread::spawn(move || {
                let input = lines(&[rename(L1, &format!("lane-{n}")), json!({"command":"command","command_id":"document.save","parameters":{"path":out.to_str().unwrap()}})]);
                let (exit, replies) = session(&package, &format!("lane-{n}"), &format!("s-{n}"), &input, &diag);
                assert_eq!(exit, 0, "{replies:?}");
                (out, replies)
            })
        })
        .collect();
    for (n, handle) in handles.into_iter().enumerate() {
        let (out, replies) = handle.join().unwrap();
        let step = &replies[1]["state"]["edit"]["history"]["undo_steps"][0];
        assert_eq!((step["actor"]["principal_id"].as_str(), step["client_session"].as_str()), (Some(format!("lane-{n}").as_str()), Some(format!("s-{n}").as_str())));
        let (exit, r) = run(&["state", out.to_str().unwrap(), "--actor", "reader", "--session", "s"], &dir.0);
        assert_eq!((exit, layer(&r["state"], L1)["name"].as_str(), r["revision"].as_u64()), (0, Some(format!("lane-{n}").as_str()), Some(10)));
    }
    assert_eq!(std::fs::read(&package).unwrap(), FIXTURE, "the source is untouched");
}

// ---- no window, ever ------------------------------------------------------------------------------

/// The drive sources name no window, no focus and no OS input; `tests/harness_headless.rs` scans the
/// same sources for process, network and environment APIs.
#[test]
fn drive_sources_name_no_window_focus_or_os_input() {
    let sources = [("cli.rs", include_str!("../src/cli.rs")), ("cli/drive.rs", include_str!("../src/cli/drive.rs"))];
    for (name, text) in sources {
        for token in ["egui", "eframe", "winit", "wgpu", "SendInput", "SetForegroundWindow", "SetCursorPos", "keybd_event", "mouse_event", "SetFocus", "ViewportCommand"] {
            assert!(!text.contains(token), "{name} mentions `{token}`");
        }
    }
}

/// The live process: a session held open answers on its pipes and owns no top-level window. Read
/// from outside the process, by the operating system's own process table.
#[test]
fn a_live_session_owns_no_window() {
    let dir = Scratch::new("nowindow");
    let package = dir.package("a.handshake");
    let p = package.to_str().unwrap();
    let mut child = spawn(&["command", p, "--actor", "a", "--session", "s", "--stdin"], &dir.0);
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut first = String::new();
    stdout.read_line(&mut first).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&first).unwrap()["ok"], true, "{first}");

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let script = format!("(Get-Process -Id {}).MainWindowHandle.ToInt64()", child.id());
        let handle = Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", &script]).creation_flags(CREATE_NO_WINDOW).output();
        match handle {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout);
                assert_eq!(text.trim(), "0", "the session owns a window");
            }
            _ => println!("NOT_PROVEN: powershell did not answer; the process table was not read"),
        }
    }
    #[cfg(not(windows))]
    println!("NOT_PROVEN: the process-table read exists only on Windows");

    writeln!(stdin, "{}", json!({"command":"state"})).unwrap();
    let mut second = String::new();
    stdout.read_line(&mut second).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&second).unwrap()["ok"], true);
    drop(stdin);
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if start.elapsed() > PROCESS_LIMIT {
            let _ = child.kill();
            let _ = child.wait();
            panic!("closing stdin did not end the session");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(status.code(), Some(0));
}

/// The window trap, classified without a window: a command word goes to the CLI, a flag or word it
/// does not know is usage, and only no argument or an existing file opens a window.
#[test]
fn only_no_argument_or_an_existing_file_opens_a_window() {
    let classify = |args: &[&str], exists: bool| {
        let owned: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        cli::window_args(&owned, &|_| exists)
    };
    for word in ["help", "develop", "state", "command", "undo", "export", "edit", "open", "batch", "verify", "--help", "-h"] {
        // A command word wins even over a file of that name.
        assert!(matches!(classify(&[word], true), WindowArgs::Cli), "{word}");
        assert!(matches!(classify(&[word, "x"], false), WindowArgs::Cli), "{word} x");
    }
    for args in [&["nonsense"][..], &["--nonsense"], &["-x"], &["--cpu", "nonsense"], &["missing.psd"], &["--cpu", "help"], &["--recent"]] {
        assert!(matches!(classify(args, false), WindowArgs::Usage(_)), "{args:?}");
    }
    assert!(matches!(classify(&["a.psd", "b.psd"], true), WindowArgs::Usage(_)), "two files are ambiguous");
    assert!(matches!(classify(&["--nonsense"], true), WindowArgs::Usage(_)), "a flag is never a file");
    match classify(&[], false) {
        WindowArgs::Window { cpu: false, recent: None, file: None } => {}
        _ => panic!("no argument is the window"),
    }
    match classify(&["--cpu", "--recent", "list.txt", "a.psd"], true) {
        WindowArgs::Window { cpu: true, recent: Some(r), file: Some(f) } => {
            assert_eq!((r, f), (PathBuf::from("list.txt"), PathBuf::from("a.psd")));
        }
        _ => panic!("the window's own arguments"),
    }
}

/// The trap through the real binary, feature `desktop` (the only build with `[[bin]]`). `spawn`
/// asserts the classifier first, so a regression fails there and opens nothing.
#[cfg(feature = "desktop")]
#[test]
fn the_desktop_binary_answers_an_unknown_word_with_usage_and_no_window() {
    let dir = Scratch::new("trap");
    assert!(option_env!("CARGO_BIN_EXE_studio-harness").is_some(), "feature desktop builds the binary");
    for word in ["nonsense", "--nonsense", "missing-file.psd"] {
        let (exit, r) = run(&[word], &dir.0);
        assert_eq!((exit, r["error"].as_str(), r["hint"].as_str()), (2, Some("usage"), Some("run `help`")), "{word}: {r}");
    }
    let (exit, r) = run(&["develop"], &dir.0);
    assert_eq!((exit, r["error"].as_str()), (2, Some("usage")), "{r}");
    let text = run_text(&["help"], &dir.0);
    assert!(text.contains("COMMANDS") && text.contains("--stdin"), "{text}");
    let package = dir.package("a.handshake");
    let (exit, r) = run(&["state", package.to_str().unwrap(), "--actor", "a", "--session", "s"], &dir.0);
    assert_eq!((exit, r["revision"].as_u64()), (0, Some(9)), "{r}");
}

// ---- timeline -------------------------------------------------------------------------------------

/// 25 fps ticks per frame (STU-VID-013).
const F: u64 = 10_160_640_000;

fn clip(n: u32, start: u64, frames: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: format!("SCLP-019abcde-0000-7000-8000-{n:012x}"),
        source_ref: ClipSource::Media { artifact_manifest_id: "tone".into(), content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() }, asset_id: None },
        source_in: Ticks::new(0),
        source_out: Ticks::new(frames * F),
        timeline_start: Ticks::new(start * F),
        duration: Ticks::new(frames * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
        retime: Default::default(),
    }
}

struct Any;
impl folio::Resolver for Any {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Available
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Available
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
    fn media(&self, _: &str, _: &ContentDigest) -> Resolution {
        Resolution::Available
    }
}

/// A package with one sequence: V1 holds clip 1 at frames [0, 20) and clip 2 at [20, 50).
fn sequence_package(dir: &Path) -> PathBuf {
    let sequence = StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(50 * F),
        tracks: vec![StudioTrack {
            schema_id: TrackSchema::V1,
            track_id: "STRK-019abcde-0000-7000-8000-000000000001".into(),
            kind: TrackKind::Video,
            index: 0,
            name: "V1".into(),
            enabled: true,
            locked: false,
            sync_locked: true,
            targeted: true,
            muted: false,
            soloed: false,
            clips: vec![clip(1, 0, 20), clip(2, 20, 30)],
            volume: 1.0,
            pan: 0.0,
        }],
        markers: Vec::new(),
        work_area: None,
    };
    let doc = json!({
        "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000003","revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":L1,"name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":[serde_json::to_value(sequence).unwrap()]
    });
    let token = CancellationToken::default();
    let snapshot = match folio::inspect_bytes(&serde_json::to_vec(&doc).unwrap(), Budget::default(), &token, &Any).unwrap() {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("read-only fixture"),
    };
    let assets = hsk_studio_package::MemoryAssetSource::new();
    let mut bytes = Vec::new();
    hsk_studio_package::write_package_with(
        &snapshot,
        &assets as &dyn hsk_studio_package::AssetSource,
        &hsk_studio_package::Extras { quarantined: &[], previews: &[] },
        &hsk_studio_package::WriteOptions::default(),
        &hsk_studio_package::Limits::default(),
        &token,
        &mut bytes,
    )
    .unwrap();
    let path = dir.join("timeline.handshake");
    std::fs::write(&path, bytes).unwrap();
    path
}

fn timeline(key: &str, parameters: Value) -> Value {
    json!({"command":"command","command_id":format!("timeline.{key}"),"parameters":parameters})
}

#[test]
fn timeline_commands_drive_the_sequence_through_the_process_and_undo() {
    let dir = Scratch::new("timeline");
    let package = sequence_package(&dir.0);
    let clip1 = "SCLP-019abcde-0000-7000-8000-000000000001";
    let input = lines(&[
        timeline("trim_out", json!({"clip_id":clip1,"tail":10 * F})),
        timeline("scrub", json!({"tick":5 * F})),
        timeline("razor", json!({})),
        timeline("zoom_in", json!({})),
        json!({"command":"undo"}),
        json!({"command":"undo"}),
        timeline("trim_out", json!({"clip_id":clip1})),
        json!({"command":"command","command_id":"timeline.nest","parameters":{"clips":clip1}}),
    ]);
    let (exit, r) = session(&package, "lane-a", "s-1", &input, &dir.0);
    assert_eq!(exit, 1, "{r:?}");
    let clips = |n: usize| r[n]["state"]["timeline"]["tracks"].clone();
    assert_eq!((r[1]["ok"].as_bool(), r[1]["command_id"].as_str()), (Some(true), Some("timeline.trim_out")), "{}", r[1]);
    assert_eq!(r[1]["revision"], r[0]["revision"].as_u64().unwrap() + 1);
    assert_eq!(clips(1), 1, "the timeline opened on the document's sequence");
    assert_eq!(r[1]["state"]["timeline"]["sequence_id"], "seq-main");
    let trim_step = &r[1]["state"]["edit"]["history"]["undo_steps"][0];
    assert_eq!((trim_step["actor"]["principal_id"].as_str(), trim_step["client_session"].as_str()), (Some("lane-a"), Some("s-1")));
    assert_eq!(r[2]["ok"], true, "scrub: {}", r[2]);
    assert_eq!(r[3]["ok"], true, "razor: {}", r[3]);
    assert_eq!(r[3]["revision"], r[1]["revision"].as_u64().unwrap() + 1, "razor is one step");
    assert_eq!(r[4]["ok"], true, "zoom: {}", r[4]);
    assert_eq!(r[4]["revision"], r[3]["revision"], "a view command moves no revision");
    assert_eq!(r[5]["ok"], true, "undo of razor: {}", r[5]);
    assert_eq!(r[6]["ok"], true, "undo of trim: {}", r[6]);
    assert_eq!(r[6]["state"]["edit"]["history"]["undo"].as_array().unwrap().len(), 0);
    assert_eq!(r[7]["error"], "parameter_missing", "{}", r[7]);
    assert_eq!(r[8]["ok"], true, "nest: {}", r[8]);
    assert_eq!(r[8]["revision"], r[6]["revision"].as_u64().unwrap() + 1, "nest is one step");
}

#[test]
fn timeline_on_a_document_with_no_sequence_is_a_typed_refusal() {
    let dir = Scratch::new("nosequence");
    let package = dir.package("a.handshake");
    let (exit, r) = session(&package, "a", "s", &lines(&[timeline("razor", json!({}))]), &dir.0);
    assert_eq!(exit, 1);
    assert_eq!(r[1]["error"], "no_timeline", "{}", r[1]);
}
