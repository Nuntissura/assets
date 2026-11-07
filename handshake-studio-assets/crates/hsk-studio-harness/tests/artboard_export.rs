//! Lane SMB-ARTBOARD-EXPORT-1 (feature `viewer`; STU-IO-010, STU-IO-111 to STU-IO-114): the export rows
//! through the drive surface in process (`command PACKAGE --command-id ID --param KEY VALUE`, the dispatch
//! the JSONL loop of `cli/drive.rs` runs for every line): a blank document, its board marked for export and
//! saved, and one run writing the named files from the saved package. vector-brand steps 85, 86 and 88.
#![cfg(feature = "viewer")]

use hsk_studio_harness::cli;
use serde_json::Value;
use std::path::PathBuf;

const WHO: [&str; 4] = ["--actor", "artboard-export-test", "--session", "artboard-export-session"];

fn cli_json(args: &[&str]) -> (i32, Value) {
    let mut all: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    all.extend(WHO.iter().map(|s| (*s).to_string()));
    let out = cli::run(&all);
    (out.exit, serde_json::from_str(&out.stdout).unwrap_or(Value::Null))
}

/// Test scratch directory under TMP, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hsk-artboard-export-drive-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
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

#[test]
fn a_marked_board_saves_and_exports_through_the_drive() {
    let scratch = Scratch::new("marked");
    let blank = scratch.path("blank.handshake");
    let (exit, report) = cli_json(&["new", &blank, "--profile", "vector", "--size", "64x64", "--unit", "px"]);
    assert_eq!(exit, 0, "{report}");
    let (exit, state) = cli_json(&["state", &blank]);
    assert_eq!(exit, 0, "{state}");
    let board = state["state"]["containers"]["artboards"][0]["id"].as_str().unwrap_or_else(|| panic!("{state}")).to_owned();
    let marked = scratch.path("marked.handshake");
    let settings = r#"[{"format":"PNG"},{"format":"PNG","suffix":"@2x","constraint":{"type":"SCALE","value":2}},{"format":"SVG"}]"#;
    let (exit, report) = cli_json(&["command", &blank, "--command-id", "export.set_settings", "--param", "node", &board, "--param", "settings", settings, "--save", &marked]);
    assert_eq!(exit, 0, "{report}");
    assert_eq!(report["detail"]["command"]["result"]["settings"], 3, "{report}");
    assert!(report["detail"]["command"]["undo_name"].as_str().is_some_and(|n| n.starts_with("set_export_settings")), "{report}");
    let out = scratch.0.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let (exit, report) = cli_json(&["command", &marked, "--command-id", "export.run", "--param", "dir", &out.to_string_lossy()]);
    assert_eq!(exit, 0, "{report}");
    let files = report["detail"]["result"]["files"].as_array().unwrap_or_else(|| panic!("{report}"));
    assert_eq!(files.len(), 3, "{report}");
    let sizes: Vec<(String, f64)> = files.iter().map(|f| (f["file"].as_str().unwrap().to_owned(), f["width"].as_f64().unwrap())).collect();
    assert!(sizes.iter().any(|(f, w)| f.ends_with("@2x.png") && *w == 128.0), "{sizes:?}");
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 3);
    // The run reads the package only: a second run refuses to replace the files unasked.
    let (exit, report) = cli_json(&["command", &marked, "--command-id", "export.run", "--param", "dir", &out.to_string_lossy()]);
    assert_eq!((exit, report["error"].as_str()), (1, Some("destination_exists")), "{report}");
}
