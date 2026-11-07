//! Lane SMB-DOC-NEW-1 (STU-SHL-023, STU-SHL-026, STU-VEC-028): a blank document at a stated size, the
//! first step of every design scene of `workflows/scenario-*.json`.

use hsk_studio_harness::cli;
use hsk_studio_harness_core::action;
use serde_json::Value;
use std::path::PathBuf;

fn cli_json(args: &[&str]) -> (i32, Value) {
    let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    let out = cli::run(&args);
    (out.exit, serde_json::from_str(&out.stdout).unwrap_or(Value::Null))
}

/// Test scratch directory under TMP, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hsk-doc-new-{name}-{}", std::process::id()));
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

const WHO: [&str; 4] = ["--actor", "doc-new-test", "--session", "doc-new-session"];

fn new_args<'a>(out: &'a str, rest: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["new", out];
    args.extend_from_slice(rest);
    args.extend_from_slice(&WHO);
    args
}

fn open_summary(path: &str) -> Value {
    let (exit, report) = cli_json(&["open", path]);
    assert_eq!(exit, 0, "{report}");
    report["ops"].as_array().unwrap().iter().find(|o| o["op"] == "summary").unwrap().clone()
}

/// vector-brand step 1: one document, one artboard, an empty layer stack, revision 0, saved and open.
#[test]
fn new_vector_document_reaches_the_file_and_reopens() {
    let scratch = Scratch::new("vector");
    let out = scratch.path("brand.handshake");
    let (exit, report) = cli_json(&new_args(&out, &["--profile", "vector", "--size", "1024x1024", "--unit", "px", "--colour-mode", "rgb"]));
    assert_eq!(exit, 0, "{report}");
    assert_eq!(report["verdict"], "pass", "{report}");
    let summary = open_summary(&out);
    assert_eq!(summary["detail"]["revision"], 0, "{summary}");
    assert_eq!(summary["detail"]["layers"].as_array().unwrap().len(), 0, "{summary}");
    let boards = summary["detail"]["artboards"].as_array().unwrap();
    assert_eq!(boards.len(), 1, "{summary}");
    assert_eq!(boards[0]["bounds"], serde_json::json!([0.0, 0.0, 1024.0, 1024.0]), "{summary}");
}

/// board-slides-web step 28: a deck at 1920 by 1080, one page of that size.
#[test]
fn new_layout_document_has_one_page_at_the_stated_size() {
    let scratch = Scratch::new("layout");
    let out = scratch.path("deck.handshake");
    let (exit, report) = cli_json(&new_args(&out, &["--profile", "layout", "--size", "1920x1080", "--unit", "px"]));
    assert_eq!(exit, 0, "{report}");
    let summary = open_summary(&out);
    let spreads = summary["detail"]["page_spreads"].as_array().unwrap();
    assert_eq!(spreads.len(), 1, "{summary}");
    assert_eq!(spreads[0]["page_sizes"][0]["width"], 1920.0, "{summary}");
    assert_eq!(spreads[0]["page_sizes"][0]["height"], 1080.0, "{summary}");
}

/// Hostile and impossible requests: typed, exit 1 or 2, and nothing is written. No default actor.
#[test]
fn refusals_are_typed_and_write_nothing() {
    let scratch = Scratch::new("refuse");
    let out = scratch.path("never.handshake");
    for (rest, code) in [
        (vec!["--profile", "board", "--size", "100x100"], "profile_not_available"),
        (vec!["--profile", "nope", "--size", "100x100"], "profile_unknown"),
        (vec!["--profile", "vector"], "size_required"),
        (vec!["--profile", "vector", "--size", "99999999x100"], "dimension_out_of_range"),
        (vec!["--profile", "vector", "--size", "1e309x100"], "dimension_out_of_range"),
        (vec!["--profile", "vector", "--size", "100.5x100"], "size_not_whole_pixels"),
        (vec!["--profile", "raster", "--size", "100x100", "--unit", "mm"], "raster_needs_px"),
        (vec!["--profile", "vector", "--size", "100x100", "--colour-mode", "duotone"], "colour_mode_not_creatable"),
        (vec!["--profile", "vector", "--size", "100x100", "--bits", "7"], "bit_depth_invalid"),
    ] {
        let (exit, report) = cli_json(&new_args(&out, &rest));
        assert_eq!(exit, 1, "{rest:?}: {report}");
        assert_eq!(report["error"], code, "{rest:?}: {report}");
        assert!(!std::path::Path::new(&out).exists(), "{rest:?} wrote a file");
    }
    let (exit, report) = cli_json(&["new", &out, "--profile", "vector", "--size", "100x100"]);
    assert_eq!(exit, 2, "no actor: {report}");
    assert!(!std::path::Path::new(&out).exists());
}

/// A file is replaced only on request.
#[test]
fn new_never_replaces_a_file_unasked() {
    let scratch = Scratch::new("exists");
    let out = scratch.path("kept.handshake");
    std::fs::write(&out, b"keep").unwrap();
    let (exit, report) = cli_json(&new_args(&out, &["--profile", "vector", "--size", "100x100"]));
    assert_eq!(exit, 1, "{report}");
    assert_eq!(report["error"], "overwrite_required", "{report}");
    assert_eq!(std::fs::read(&out).unwrap(), b"keep");
}

/// Every row of the slice is in the catalog, mutating and not recordable (a recording reports it).
#[test]
fn the_commands_are_catalog_rows() {
    for id in ["document.new", "artboard.add", "artboard.rename", "artboard.resize", "artboard.move", "artboard.delete", "artboard.set_options", "page.add", "page.rename", "page.resize", "page.delete"] {
        let row = action::command(id).unwrap_or_else(|| panic!("{id} has no RUN_COMMANDS row"));
        assert!(row.mutates_authority && !row.recordable, "{id}");
    }
}
