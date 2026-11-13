//! Lane SMB-GUI-MENU-1 (feature `viewer`, lead rulings 1 and 5, review F1 and F3): the drive surface and the native
//! window's menu are one path. A process session drives a sequence of designed menu leaves by id; an in-process Viewer
//! on the same package runs the same leaves through `Viewer::run_menu_leaf_in` with the native window's hosting (the
//! call the window makes). For every leaf both give the same outcome, the same undo name, the same command and
//! parameters, the same availability record on a refusal, and leave the same selection and undo history.
#![cfg(feature = "viewer")]

use hsk_studio_harness::cli::{self, WindowArgs};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::menu_tree::MenuContext;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::shell::ShellCommand;
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const FIXTURE: &[u8] = include_bytes!("fixtures/demo.handshake");
const PROCESS_LIMIT: Duration = Duration::from_secs(120);

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let base = std::env::temp_dir();
        let text = base.to_string_lossy().to_ascii_uppercase();
        assert!(!(cfg!(windows) && text.starts_with("C:")), "TMP is on C: ({}); run through studio-env.sh", base.display());
        let dir = base.join(format!("hsk-harness-drive-menu-{}-{label}", std::process::id()));
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

#[cfg(feature = "desktop")]
fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_studio-harness"))
}

#[cfg(not(feature = "desktop"))]
fn binary() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().and_then(Path::parent).expect("target directory").to_path_buf();
    let example = dir.join("examples").join(format!("studio-harness{}", std::env::consts::EXE_SUFFIX));
    assert!(example.is_file(), "the studio-harness example was not built at {}", example.display());
    example
}

fn spawn(args: &[&str], diag: &Path) -> Child {
    let owned: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    assert!(!matches!(cli::window_args(&owned, &|p| p.is_file()), WindowArgs::Window { .. }), "{args:?} would open a window; not spawned");
    let mut command = Command::new(binary());
    command.args(args).current_dir(diag).env("STUDIO_DIAG_DIR", diag).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.spawn().expect("spawn studio-harness")
}

fn finish(mut child: Child, input: &[u8]) -> (i32, Vec<u8>) {
    let mut stdin = child.stdin.take().expect("stdin");
    let bytes = input.to_vec();
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
    let _ = errors.join();
    (status.code().expect("an exit code, not a crash"), out)
}

fn session(package: &Path, input: &[u8], diag: &Path) -> Vec<Value> {
    let p = package.to_str().unwrap();
    let (_, out) = finish(spawn(&["command", p, "--actor", "lane-a", "--session", "s-1", "--stdin"], diag), input);
    let text = String::from_utf8(out).expect("utf-8 stdout");
    text.lines().map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("line is not JSON ({e}): {l}"))).collect()
}

/// The shell commands the native window shows that are document edits (hsk-studio-native-host `gui::frame_hosts`; its
/// hosted panels are not in this sequence).
fn native_hosts(c: ShellCommand) -> bool {
    matches!(c, ShellCommand::Undo | ShellCommand::Redo | ShellCommand::CreateOutlines)
}

/// Designed leaves in an order that exercises every catalog leaf, the aliases, a shell leaf, greyed leaves (a
/// selection leaf with nothing selected, Ungroup on a raster layer, a submenu id) and New Timeline.
const LEAVES: &[&str] = &[
    "studio.select.deselect",
    "studio.select.navigate_selection.first_object",
    "studio.object.ungroup",
    "studio.history.step_backward",
    "studio.select.navigate_selection.last_object",
    "studio.object.ungroup",
    "studio.object.layer.duplicate_layer",
    "studio.edit.duplicate",
    "studio.object.group",
    "studio.object.hide.hide_selection",
    "studio.object.arrange.bring_forward",
    "studio.object.arrange.send_backward",
    "studio.select.navigate_selection.next_object_above",
    "studio.select.navigate_selection.next_object_below",
    "studio.object.layer.new",
    "studio.insert.layer.pixel_layer",
    "studio.object.layer.delete_layer",
    "studio.motion.timeline_doc.new_timeline",
    "studio.history.undo",
    "studio.history.step_forward",
    "studio.motion.keyframe",
    "studio.select.deselect",
];

#[test]
fn drive_and_the_native_menu_path_agree_on_every_leaf() {
    let dir = Scratch::new("menu");
    let package = dir.package("a.handshake");
    let input: Vec<u8> = LEAVES.iter().flat_map(|id| format!("{}\n", json!({"command":"command","command_id":id,"parameters":{}})).into_bytes()).collect();
    let replies = session(&package, &input, &dir.0);
    assert_eq!(replies.len(), LEAVES.len() + 2, "open, one reply per leaf, end: {replies:?}");
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&package).unwrap_or_else(|e| panic!("open: {e}"));
    let ctx = MenuContext { tool: None, hosts: &native_hosts };
    let (mut ran, mut refused) = (0, 0);
    for (id, reply) in LEAVES.iter().zip(&replies[1..]) {
        assert_eq!(reply["command_id"], *id);
        let (bar, state) = v.studio_menu(&ctx);
        let record = bar.find(id).map(|n| n.availability.clone());
        let native = v.run_menu_leaf_in(&bar, &state, id);
        println!("{id}: {}", native.as_ref().map_or_else(|e| format!("refused {}", e.code), |r| format!("ran {} undo {:?}", r.command, r.undo_name)));
        match &native {
            Ok(run) => {
                ran += 1;
                assert_eq!(reply["ok"], true, "{id}: the window ran it, the drive refused: {reply}");
                assert!(record.as_ref().is_some_and(|a| a.is_available()), "{id}");
                if ShellCommand::from_id(id).is_none() {
                    let d = &reply["detail"];
                    assert_eq!(d["command"], run.command, "{id}");
                    assert_eq!(d["params"], json!(run.params), "{id}");
                    assert_eq!(d["undo_name"], run.undo_name.clone().unwrap_or(Value::Null), "{id}: undo name");
                }
            }
            Err(e) => {
                refused += 1;
                assert_eq!(reply["ok"], false, "{id}: the window refused {e:?}, the drive ran it: {reply}");
                assert_eq!(reply["error"], e.code, "{id}");
                assert_eq!(reply["detail"]["availability"], e.availability.as_ref().map_or(Value::Null, |a| a.json()), "{id}: availability record");
                if let Some(a) = &e.availability {
                    assert_eq!(Some(a), record.as_ref(), "{id}: the refusal carries the menu's own record");
                }
            }
        }
        let state = v.state_json();
        assert_eq!(reply["state"]["selected"], state["selected"], "{id}: selection");
        assert_eq!(reply["state"]["edit"]["history"]["undo"], v.run().history()["undo"], "{id}: undo history");
        assert_eq!(reply["state"]["edit"]["history"]["redo"], v.run().history()["redo"], "{id}: redo history");
        let names = |s: &Value| s["layers"].as_array().unwrap().iter().map(|l| (l["name"].clone(), l["kind"].clone(), l["visible"].clone())).collect::<Vec<_>>();
        assert_eq!(names(&reply["state"]), names(&state), "{id}: layers");
    }
    // Measured on demo.handshake: 11 leaves ran, 11 refused (greyed, not a leaf, or the command's own refusal).
    assert_eq!((ran, refused), (11, 11), "ran {ran}, refused {refused}");
}
