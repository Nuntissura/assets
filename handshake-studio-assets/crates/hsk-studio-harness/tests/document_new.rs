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

/// Review F3 of 368e1080: a catalog id the drive routes declares exactly the keys of the viewer row it
/// runs, so a model reading the corpus never meets parameter_unknown.
#[test]
fn routed_catalog_rows_declare_the_drive_rows_keys() {
    use hsk_studio_harness_core::manual::{DRIVE_CATALOG_ROUTES, DRIVE_VIEWER};
    for (id, method) in DRIVE_CATALOG_ROUTES {
        // A menu leaf id (`studio.*`, STU-SHL-023) is no catalog row: it has the viewer row's keys by route.
        if id.starts_with("studio.") {
            assert!(action::command(id).is_none(), "{id}");
            continue;
        }
        let row = DRIVE_VIEWER.iter().find(|r| r.method == *method).unwrap_or_else(|| panic!("{id}: no viewer row {method}"));
        let mut drive: Vec<&str> = row.params.iter().map(|p| p.key).collect();
        drive.sort_unstable();
        let mut catalog: Vec<&str> = action::command(id).unwrap_or_else(|| panic!("{id}: no catalog row")).parameters.iter().map(|p| p.key).collect();
        catalog.sort_unstable();
        assert_eq!(catalog, drive, "{id} -> viewer.{method}");
    }
}

/// Every row of the slice is in the catalog, mutating and not recordable (a recording reports it).
#[test]
fn the_commands_are_catalog_rows() {
    for id in ["document.new", "artboard.add", "artboard.rename", "artboard.resize", "artboard.move", "artboard.delete", "artboard.set_options", "page.add", "page.rename", "page.resize", "page.delete"] {
        let row = action::command(id).unwrap_or_else(|| panic!("{id} has no RUN_COMMANDS row"));
        assert!(row.mutates_authority && !row.recordable, "{id}");
    }
}

#[cfg(feature = "viewer")]
mod through_the_viewer {
    use super::{Scratch, WHO, cli_json};
    use hsk_studio_folio::GeometryUnit;
    use hsk_studio_harness::view::Viewer;
    use hsk_studio_harness::view::export::ExportFormat;
    use hsk_studio_harness::view::render::Prefer;
    use hsk_studio_harness::view::vector::{ShapeKind, VectorTool};
    use hsk_studio_harness_core::document_new::{ArtboardOptionsEdit, BackgroundFill, DocumentProfile, NewDocument};
    use hsk_studio_pigment::brush::StrokeSample;
    use serde_json::{Value, json};
    use std::path::Path;

    fn vector_doc(w: f64, h: f64) -> Viewer {
        let mut v = Viewer::new(Prefer::Cpu);
        v.new_document(&NewDocument::new(DocumentProfile::Vector, Some((w, h))), None).unwrap();
        v
    }

    fn containers(v: &Viewer) -> Value {
        v.state_json()["containers"].clone()
    }

    fn undo_labels(v: &Viewer) -> Vec<String> {
        v.run().history()["undo"].as_array().unwrap().iter().map(|l| l.as_str().unwrap().to_owned()).collect()
    }

    fn revision(v: &Viewer) -> u64 {
        v.run().snapshot().unwrap().document().revision
    }

    fn pixel(v: &Viewer, x: usize, y: usize) -> [u8; 4] {
        let f = v.frame().expect("a frame");
        let at = (y * f.width as usize + x) * 4;
        [f.rgba8[at], f.rgba8[at + 1], f.rgba8[at + 2], f.rgba8[at + 3]]
    }

    fn reopen(path: &Path) -> Viewer {
        let mut again = Viewer::new(Prefer::Cpu);
        again.open(path).unwrap();
        again
    }

    fn black() -> VectorTool {
        VectorTool { fill_on: true, fill: [0, 0, 0], stroke_on: false, stroke: [0, 0, 0], stroke_width: 2.0, ..VectorTool::default() }
    }

    /// vector-brand steps 1, 3 and 6: a blank vector document renders empty, takes named artboards,
    /// and a shape drawn on the second one frames, exports and reopens with that artboard.
    #[test]
    fn a_blank_vector_document_takes_artboards_art_export_and_reopen() {
        let scratch = Scratch::new("brand-flow");
        let mut v = vector_doc(128.0, 128.0);
        assert_eq!(revision(&v), 0);
        v.render_if_dirty();
        let empty = v.frame().unwrap_or_else(|| panic!("no frame: {}", v.state_json()));
        assert_eq!((empty.width, empty.height), (128, 128));
        assert_eq!(pixel(&v, 5, 5)[3], 0, "an empty document is transparent");
        let first = containers(&v)["artboards"][0]["id"].as_str().unwrap().to_owned();

        v.rename_artboard(&first, "mark").unwrap();
        let wide = v.add_artboard("lockup-horizontal", 200.0, 64.0, None).unwrap();
        let tall = v.add_artboard("lockup-stacked", 64.0, 80.0, None).unwrap();
        assert_eq!(revision(&v), 3, "one revision per command");
        assert_eq!(undo_labels(&v), ["rename_artboard mark", "add_artboard lockup-horizontal", "add_artboard lockup-stacked"]);
        let state = containers(&v);
        assert_eq!(state["artboards"][1]["bounds"], json!([192.0, 0.0, 200.0, 64.0]), "to the right at the gap: {state}");
        assert_eq!(state["artboards"][2]["bounds"], json!([456.0, 0.0, 64.0, 80.0]), "{state}");

        // Art on the second board, framed there; the first board's frame stays clear of it.
        v.set_vector_tool(black());
        v.go_to_artboard(&wide).unwrap();
        assert_eq!(containers(&v)["active"], json!(wide));
        v.draw_shape(ShapeKind::Rectangle, [200.0, 8.0], [260.0, 56.0], false).unwrap();
        v.render_if_dirty();
        let frame = v.frame().unwrap_or_else(|| panic!("no frame: {}", v.state_json()));
        assert_eq!((frame.width, frame.height, frame.x, frame.y), (200, 64, 192, 0));
        assert_eq!(pixel(&v, 30, 30), [0, 0, 0, 255], "the shape, in the second board's frame");
        assert_eq!(pixel(&v, 150, 30)[3], 0);

        let png = scratch.0.join("wide.png");
        let report = v.export(&png, ExportFormat::Png, false).unwrap();
        assert_eq!((report["width"].as_u64(), report["height"].as_u64()), (Some(200), Some(64)), "{report}");
        let svg = scratch.0.join("wide.svg");
        v.export_vector_svg(&svg, false).unwrap();
        let text = std::fs::read_to_string(&svg).unwrap();
        assert!(text.contains("200") && text.contains("64"), "the svg frames the chosen board: {}", &text[..text.len().min(300)]);
        let pdf = scratch.0.join("wide.pdf");
        v.export_vector_pdf(&pdf, false).unwrap();
        assert!(std::fs::metadata(&pdf).unwrap().len() > 0);

        v.go_to_artboard(&tall).unwrap();
        v.render_if_dirty();
        assert_eq!(v.frame().unwrap().width, 64);
        v.go_to_artboard(&first).unwrap();
        v.render_if_dirty();
        assert_eq!(v.frame().unwrap().width, 128);
        assert_eq!(pixel(&v, 20, 20)[3], 0);

        let saved = scratch.0.join("brand.handshake");
        v.save(&saved, false).unwrap();
        let again = reopen(&saved);
        assert_eq!(containers(&again)["artboards"], containers(&v)["artboards"], "the containers survive");
        assert_eq!(revision(&again), revision(&v));
    }

    /// Each edit is one named undo step, undo and redo restore the exact lists, attribution is the
    /// caller's and a removed active frame falls back.
    #[test]
    fn every_edit_undoes_as_one_step_with_attribution() {
        let mut v = vector_doc(100.0, 100.0);
        let base = containers(&v);
        let b = v.add_artboard("second", 50.0, 50.0, Some([200.0, 300.0])).unwrap();
        let one = containers(&v);
        v.go_to_artboard(&b).unwrap();
        v.resize_artboard(&b, 60.0, 70.0).unwrap();
        v.move_artboard(&b, 10.0, 20.0).unwrap();
        v.rename_artboard(&b, "renamed").unwrap();
        let edited = containers(&v);
        assert_eq!(edited["artboards"][1]["bounds"], json!([10.0, 20.0, 60.0, 70.0]));
        assert_eq!(edited["artboards"][1]["name"], "renamed");
        v.delete_artboard(&b).unwrap();
        assert_eq!(v.state_json()["containers"]["artboards"].as_array().unwrap().len(), 1);
        assert_eq!(containers(&v)["active"], base["artboards"][0]["id"], "the frame falls back");
        v.undo().unwrap();
        assert_eq!(containers(&v)["artboards"], edited["artboards"]);
        for _ in 0..3 {
            v.undo().unwrap();
        }
        assert_eq!(containers(&v)["artboards"], one["artboards"]);
        v.undo().unwrap();
        assert_eq!(containers(&v)["artboards"], base["artboards"]);
        let r = revision(&v);
        v.redo().unwrap();
        assert_eq!(containers(&v)["artboards"], one["artboards"]);
        assert_eq!(revision(&v), r + 1);

        let history = v.run().history();
        let step = &history["undo_steps"][0];
        assert_eq!(step["label"], "add_artboard second");
        assert_eq!(step["command_id"], "history.redo");
        assert!(step["actor"]["account_id"].is_string(), "{step}");
        // An add is attributed by its command id, and its one op carries one frame.
        let add = v.run().ops().iter().find(|o| o["op"] == "add_artboard").unwrap();
        assert_eq!(add["command_id"], "artboard.add");
        assert_eq!(add["observe"]["frames"], 1, "{add}");
        assert!(add["actor"]["account_id"].is_string());
    }

    /// Review F1 of 368e1080: a new artboard never takes the id of a deleted one, so the deleted one's
    /// options stay inert; a new page never takes the key of a deleted one.
    #[test]
    fn a_new_container_never_reuses_a_deleted_ones_id() {
        let mut v = vector_doc(100.0, 100.0);
        let b = v.add_artboard("B", 50.0, 50.0, None).unwrap();
        v.set_artboard_options(&b, &ArtboardOptionsEdit { show_center_mark: Some(true), ..ArtboardOptionsEdit::default() }).unwrap();
        v.delete_artboard(&b).unwrap();
        let c = v.add_artboard("C", 50.0, 50.0, None).unwrap();
        assert_ne!(c, b);
        assert_eq!(containers(&v)["artboards"][1]["options"]["show_center_mark"], Value::Null, "C starts with the defaults");
        v.go_to_artboard(&c).unwrap();
        assert!(v.go_to_artboard(&b).is_err());

        let mut l = Viewer::new(Prefer::Cpu);
        l.new_document(&NewDocument::new(DocumentProfile::Layout, Some((100.0, 100.0))), None).unwrap();
        let two = l.add_page(None, false, None, None).unwrap();
        let three = l.add_page(None, false, None, None).unwrap();
        l.delete_page(&three).unwrap();
        let next = l.add_page(None, false, None, None).unwrap();
        assert_ne!(next, three, "{two} {three} {next}");
    }

    /// R60-a3 probe lead (SMB-DOC-NEW-1; fixed in lane SMB-BOARD-1): add, set_options and delete cycles past
    /// the reader's row bound (twice the artboard ceiling) never outgrow the options entry: a delete takes its
    /// artboard's options with it in its own step (an undo brings them back), every set_options still succeeds,
    /// and the entry holds the live artboards' rows only.
    #[test]
    fn add_set_delete_cycles_keep_the_artboard_options_entry_bounded_by_the_live_artboards() {
        let rows = |v: &Viewer| -> (usize, usize) {
            let bytes = v.run().document_entry("artboard_options").expect("an options entry");
            (serde_json::from_slice::<Value>(bytes).unwrap()["artboards"].as_array().unwrap().len(), bytes.len())
        };
        let mut v = vector_doc(100.0, 100.0);
        let first = containers(&v)["artboards"][0]["id"].as_str().unwrap().to_owned();
        let on = ArtboardOptionsEdit { show_center_mark: Some(true), ..ArtboardOptionsEdit::default() };
        v.set_artboard_options(&first, &on).unwrap();
        let (_, one) = rows(&v);
        for cycle in 0..2001_u32 {
            let b = v.add_artboard("B", 50.0, 50.0, None).unwrap();
            let set = v.set_artboard_options(&b, &on);
            assert!(set.is_ok(), "set_options refused at cycle {cycle}: {set:?}");
            v.delete_artboard(&b).unwrap();
        }
        let set = v.set_artboard_options(&first, &ArtboardOptionsEdit { show_center_mark: Some(false), ..ArtboardOptionsEdit::default() });
        assert!(set.is_ok(), "set_options after 2001 cycles: {set:?}");
        let (count, len) = rows(&v);
        assert_eq!(count, 1, "one row per live artboard");
        assert!(len <= one + 8, "{len} bytes against {one} for the one live artboard");
        // The last delete's undo brings that artboard back with its options.
        v.undo().unwrap();
        v.undo().unwrap();
        assert_eq!(containers(&v)["artboards"][1]["options"]["show_center_mark"], true, "{}", containers(&v));
        assert_eq!(rows(&v).0, 2);
    }

    /// Hostile sizes, names and targets are typed and change nothing: bounded, no panic.
    #[test]
    fn hostile_artboard_input_is_typed_and_changes_nothing() {
        let mut v = vector_doc(100.0, 100.0);
        let first = containers(&v)["artboards"][0]["id"].as_str().unwrap().to_owned();
        let before = containers(&v);
        let code = |v: &Viewer| v.run().ops().last().unwrap()["error"].as_str().unwrap_or("").to_owned();
        for (w, h, x, want) in [
            (f64::NAN, 10.0, 0.0, "dimension_out_of_range"),
            (f64::INFINITY, 10.0, 0.0, "dimension_out_of_range"),
            (-1.0, 10.0, 0.0, "dimension_out_of_range"),
            (0.0, 10.0, 0.0, "dimension_out_of_range"),
            (16349.0, 10.0, 0.0, "dimension_out_of_range"),
            (10.5, 10.0, 0.0, "size_not_whole_pixels"),
            (10.0, 10.0, f64::NAN, "position_out_of_range"),
            (10.0, 10.0, 1e12, "position_out_of_range"),
            (10.0, 10.0, 0.5, "size_not_whole_pixels"),
        ] {
            assert!(v.add_artboard("x", w, h, Some([x, 0.0])).is_err(), "{w} {h} {x}");
            assert_eq!(code(&v), want, "{w} {h} {x}");
        }
        assert!(v.add_artboard(&"n".repeat(2000), 10.0, 10.0, None).is_err());
        assert_eq!(code(&v), "name_too_long");
        assert!(v.rename_artboard(&first, "bad\u{7}name").is_err());
        assert_eq!(code(&v), "name_invalid");
        for result in [v.rename_artboard("SART-nope", "x"), v.resize_artboard("SART-nope", 1.0, 1.0), v.delete_artboard("SART-nope")] {
            assert!(result.is_err());
            assert_eq!(code(&v), "unknown_artboard");
        }
        assert!(v.go_to_artboard("SART-nope").is_err());
        assert_eq!(containers(&v), before, "nothing changed");
        assert_eq!(revision(&v), 0);
        assert!(v.run().history()["undo"].as_array().unwrap().is_empty());
        let frames: Vec<&Value> = v.run().ops().iter().filter(|o| o["op"] == "add_artboard").map(|o| &o["observe"]["frames"]).collect();
        assert!(frames.iter().all(|f| **f == 1), "one terminal frame per refusal: {frames:?}");
        // A document with art keeps a frame.
        v.set_vector_tool(black());
        v.draw_shape(ShapeKind::Rectangle, [1.0, 1.0], [5.0, 5.0], false).unwrap();
        assert!(v.delete_artboard(&first).is_err());
        assert_eq!(code(&v), "last_artboard");
        // A refused new document leaves the open one alone.
        let open = revision(&v);
        let spec = NewDocument::new(DocumentProfile::Vector, Some((f64::MAX, 1.0)));
        assert!(v.new_document(&spec, None).is_err());
        assert_eq!(revision(&v), open);
        assert!(!v.layers().is_empty());
        assert!(v.new_document(&NewDocument::new(DocumentProfile::Design, None), Some("a/b")).is_err());
    }

    /// vector-brand step 4: the STU-VEC-165 fields are typed state, one undo step, saved; the bound and
    /// the absent enum value are refused.
    #[test]
    fn artboard_options_are_typed_undoable_and_saved() {
        let scratch = Scratch::new("options");
        let mut v = vector_doc(100.0, 100.0);
        let first = containers(&v)["artboards"][0]["id"].as_str().unwrap().to_owned();
        assert_eq!(containers(&v)["artboards"][0]["options"]["background_fill"], "artboard_fill.transparent", "the captured default");
        assert_eq!(containers(&v)["artboards"][0]["options"]["ruler_origin"], json!([0.0, 0.0]), "the lower-left corner");
        let edit = ArtboardOptionsEdit {
            show_center_mark: Some(true),
            ruler_origin: Some([12.0, 34.0]),
            ruler_pixel_aspect_ratio: Some(2.0),
            background_fill: Some(BackgroundFill::White),
            preset: Some("Square".into()),
            ..ArtboardOptionsEdit::default()
        };
        let revision_before = revision(&v);
        let options = v.set_artboard_options(&first, &edit).unwrap();
        assert_eq!(options["show_center_mark"], true);
        assert_eq!(revision(&v), revision_before, "an entry write is no revision");
        assert_eq!(undo_labels(&v).len(), 1);
        let saved = scratch.0.join("options.handshake");
        v.save(&saved, false).unwrap();
        let again = reopen(&saved);
        assert_eq!(containers(&again)["artboards"][0]["options"], options);
        v.undo().unwrap();
        assert_eq!(containers(&v)["artboards"][0]["options"]["show_center_mark"], Value::Null);
        v.redo().unwrap();
        assert_eq!(containers(&v)["artboards"][0]["options"], options);

        let code = |v: &Viewer| v.run().ops().last().unwrap()["error"].as_str().unwrap_or("").to_owned();
        assert!(v.set_artboard_options(&first, &ArtboardOptionsEdit { ruler_pixel_aspect_ratio: Some(10.5), ..ArtboardOptionsEdit::default() }).is_err());
        assert_eq!(code(&v), "ruler_aspect_out_of_range");
        assert!(v.set_artboard_options(&first, &ArtboardOptionsEdit { ruler_origin: Some([f64::NAN, 0.0]), ..ArtboardOptionsEdit::default() }).is_err());
        assert_eq!(code(&v), "position_out_of_range");
        assert!(v.set_artboard_options("SART-nope", &ArtboardOptionsEdit::default()).is_err());
        assert_eq!(code(&v), "unknown_artboard");
        assert_eq!(BackgroundFill::parse("artboard_fill.3"), Err("background_fill_unknown"));
        assert_eq!(containers(&v)["artboards"][0]["options"], options, "refusals changed nothing");
        // The same options again are no step.
        let steps = undo_labels(&v).len();
        v.set_artboard_options(&first, &edit).unwrap();
        assert_eq!(undo_labels(&v).len(), steps);
        // Options of an artboard the document no longer holds are inert, and come back with an undo.
        v.delete_artboard(&first).unwrap();
        assert_eq!(containers(&v)["options_error"], Value::Null);
        v.undo().unwrap();
        assert_eq!(containers(&v)["artboards"][0]["options"], options);
    }

    /// board-slides-web step 28: a layout document's pages: add, name, resize and delete, undo, save,
    /// reopen. The frame follows the page the model goes to.
    #[test]
    fn a_layout_document_edits_its_pages() {
        let scratch = Scratch::new("pages");
        let mut v = Viewer::new(Prefer::Cpu);
        let mut spec = NewDocument::new(DocumentProfile::Layout, Some((1920.0, 1080.0)));
        spec.colour_mode = 2;
        v.new_document(&spec, Some("deck")).unwrap();
        v.render_if_dirty();
        assert_eq!((v.frame().unwrap().width, v.frame().unwrap().height), (1920, 1080), "the deck frames: {}", v.state_json()["render_error"]);
        assert_eq!(v.state_json()["containers"]["geometry_unit"], "pt");

        let two = v.add_page(None, true, None, Some("section")).unwrap();
        let three = v.add_page(Some(&two), false, Some((800.0, 600.0)), None).unwrap();
        let state = containers(&v);
        assert_eq!(state["page_spreads"].as_array().unwrap().len(), 2, "{state}");
        assert_eq!(state["page_spreads"][1]["name"], "section");
        assert_eq!(state["page_spreads"][1]["pages"].as_array().unwrap().len(), 2);
        assert_eq!(state["page_spreads"][1]["bounds"], json!([0.0, 0.0, 2720.0, 1080.0]), "the spread follows its pages: {state}");
        assert_eq!(state["page_spreads"][1]["pages"][1]["id"], json!(three));

        v.resize_page(&three, 900.0, 1200.0).unwrap();
        assert_eq!(containers(&v)["page_spreads"][1]["bounds"], json!([0.0, 0.0, 2820.0, 1200.0]));
        v.rename_page(&three, "renamed").unwrap();
        assert_eq!(containers(&v)["page_spreads"][1]["name"], "renamed");
        v.go_to_page(&three).unwrap();
        v.render_if_dirty();
        assert_eq!(v.frame().unwrap().width, 2820, "framing the page's spread");

        let code = |v: &Viewer| v.run().ops().last().unwrap()["error"].as_str().unwrap_or("").to_owned();
        assert!(v.resize_page(&three, 10.0, 10.0).is_err());
        assert_eq!(code(&v), "margins_exceed_page");
        assert!(v.add_page(Some("page-99"), false, None, None).is_err());
        assert_eq!(code(&v), "unknown_page");
        assert!(v.add_page(None, false, None, Some("needs a spread")).is_err());
        assert_eq!(code(&v), "name_needs_new_spread");
        assert!(v.go_to_page("page-99").is_err());

        v.delete_page(&three).unwrap();
        v.delete_page(&two).unwrap();
        assert_eq!(containers(&v)["page_spreads"].as_array().unwrap().len(), 1, "the empty spread goes too");
        assert!(v.delete_page("page-1").is_err());
        assert_eq!(code(&v), "last_page");
        v.undo().unwrap();
        v.undo().unwrap();
        assert_eq!(containers(&v)["page_spreads"][1]["pages"].as_array().unwrap().len(), 2);

        let saved = scratch.0.join("deck.handshake");
        v.save(&saved, false).unwrap();
        assert_eq!(containers(&reopen(&saved))["page_spreads"], containers(&v)["page_spreads"]);
    }

    /// A raster document at a stated size paints, exports and reopens; a design document with no size
    /// has no container until a model adds them (product-design steps 1, 3 and 33), and a page opens
    /// the layout profile's route only. Colour and unit reach the document.
    #[test]
    fn raster_and_design_profiles_start_as_stated() {
        let scratch = Scratch::new("raster-design");
        let mut r = Viewer::new(Prefer::Cpu);
        r.new_document(&NewDocument::new(DocumentProfile::Raster, Some((64.0, 48.0))), None).unwrap();
        assert_eq!(r.layers().len(), 1);
        let background = r.layers()[0].layer_id.clone();
        let sample = |x: f64, y: f64| StrokeSample { x, y, pressure: None, time_ms: None };
        r.paint(&background, &[sample(4.0, 24.0), sample(60.0, 24.0)]).unwrap_or_else(|e| panic!("{e}"));
        r.render_if_dirty();
        assert_eq!((r.frame().unwrap().width, r.frame().unwrap().height), (64, 48), "{}", r.state_json()["render_error"]);
        assert!(pixel(&r, 30, 24)[3] > 0, "the stroke");
        assert_eq!(pixel(&r, 30, 2)[3], 0, "a raster document starts transparent");
        let png = scratch.0.join("raster.png");
        assert_eq!(r.export(&png, ExportFormat::Png, false).unwrap()["width"], 64);
        let saved = scratch.0.join("raster.handshake");
        r.save(&saved, false).unwrap();
        let mut again = reopen(&saved);
        again.render_if_dirty();
        assert!(pixel(&again, 30, 24)[3] > 0, "the stroke survives");

        let mut d = Viewer::new(Prefer::Cpu);
        d.new_document(&NewDocument::new(DocumentProfile::Design, None), Some("Larkspur-Design-File")).unwrap();
        assert_eq!(containers(&d)["artboards"], json!([]));
        assert_eq!(revision(&d), 0);
        for name in ["Foundations", "Components", "Screens", "Handoff"] {
            d.add_artboard(name, 390.0, 844.0, None).unwrap();
        }
        let names: Vec<String> = containers(&d)["artboards"].as_array().unwrap().iter().map(|a| a["name"].as_str().unwrap().to_owned()).collect();
        assert_eq!(names, ["Foundations", "Components", "Screens", "Handoff"], "in document order");
        let gaps: Vec<f64> = containers(&d)["artboards"].as_array().unwrap().windows(2).map(|p| p[1]["bounds"][0].as_f64().unwrap() - (p[0]["bounds"][0].as_f64().unwrap() + 390.0)).collect();
        assert!(gaps.iter().all(|g| *g == gaps[0]), "a consistent gap: {gaps:?}");
        d.render_if_dirty();
        assert_eq!(d.frame().unwrap().width, 390);

        let mut cmyk = NewDocument::new(DocumentProfile::Vector, Some((210.0, 297.0)));
        cmyk.unit = GeometryUnit::Mm;
        cmyk.colour_mode = 3;
        cmyk.bits_per_channel = 16;
        let mut c = Viewer::new(Prefer::Cpu);
        c.new_document(&cmyk, None).unwrap();
        let state = containers(&c);
        assert_eq!((&state["geometry_unit"], &state["colour_mode"], &state["bits_per_channel"]), (&json!("mm"), &json!(3), &json!(16)));
    }

    /// The design scenes' first steps as a model runs them: `new`, then the JSONL loop of
    /// `cli/drive.rs` by catalog id, to a saved document and an export; undo in the loop; a refusal
    /// never ends the session.
    #[test]
    fn the_first_design_steps_run_through_the_jsonl_loop() {
        let scratch = Scratch::new("jsonl");
        let package = scratch.path("brand.handshake");
        let mut args = vec!["new", &package, "--profile", "vector", "--size", "256x256", "--name", "mark-cli"];
        args.extend_from_slice(&WHO);
        let (exit, report) = cli_json(&args);
        assert_eq!(exit, 0, "{report}");
        let saved = scratch.path("saved.handshake");
        let png = scratch.path("lockup.png");
        let call = |id: &str, parameters: Value| json!({"command": "command", "command_id": id, "parameters": parameters}).to_string();
        let first_lines = [
            call("artboard.add", json!({"name": "lockup-horizontal", "width": 512, "height": 192})),
            call("artboard.add", json!({"name": "too big", "width": 99999, "height": 1})),
            call("studio.document.new", json!({"profile": "board"})),
            call("document.new", json!({"profile": "vector", "width": 100, "height": 100, "name": "mark", "file_name": "brand"})),
        ];
        let lines = |v: &[String]| v.join("\n").into_bytes();
        let mut args: Vec<String> = ["command", package.as_str(), "--stdin"].iter().map(|s| (*s).to_string()).collect();
        args.extend(WHO.iter().map(|s| (*s).to_string()));
        let drive = |input: Vec<u8>| {
            let mut output = Vec::new();
            let exit = hsk_studio_harness::cli::run_streams(&args, &mut std::io::Cursor::new(input), &mut output);
            let replies: Vec<Value> = String::from_utf8(output).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
            (exit, replies)
        };
        let (_, r) = drive(lines(&first_lines));
        assert_eq!(r[1]["ok"], true, "{}", r[1]["detail"]);
        assert_eq!(r[1]["command_id"], "artboard.add");
        assert_eq!(r[1]["revision"], 1);
        assert_eq!(r[1]["attribution"]["actor"]["account_id"], "doc-new-test");
        let board = r[1]["detail"]["result"].as_str().unwrap().to_owned();
        assert_eq!(r[1]["state"]["containers"]["artboards"][1]["id"], json!(board));
        assert_eq!(r[2]["error"], "dimension_out_of_range", "{}", r[2]);
        // Lane SMB-BOARD-1: the board profile opens a whiteboard document (STU-WB-001).
        assert_eq!(r[3]["ok"], true, "{}", r[3]);
        assert_eq!(r[3]["state"]["containers"]["artboards"][0]["name"], "Board 1", "{}", r[3]);
        assert_eq!(r[4]["ok"], true, "{}", r[4]);
        assert_eq!(r[4]["state"]["containers"]["artboards"][0]["name"], "mark", "`name` is the artboard name");
        assert_eq!(r[4]["revision"], 0);

        // A session of its own: the undo history lives in one process.
        let mut steps = vec![
            call("artboard.add", json!({"name": "lockup-horizontal", "width": 512, "height": 192})),
            call("page.add", json!({"width": 100, "height": 100})),
        ];
        let (_, r) = drive(lines(&steps));
        assert_eq!(r[2]["ok"], true, "{}", r[2]);
        let added = r[1]["detail"]["result"].as_str().unwrap().to_owned();
        steps.extend([
            call("artboard.set_options", json!({"artboard": added, "show_center_mark": true, "background_fill": "artboard_fill.transparent", "ruler_origin": [0, 0]})),
            call("viewer.go_to_artboard", json!({"artboard": added})),
            call("viewer.draw_shape", json!({"kind": "rectangle", "a": [340, 10], "b": [400, 60]})),
            call("viewer.export", json!({"path": png, "format": "png"})),
            json!({"command": "undo"}).to_string(),
            call("history.redo", json!({})),
            call("document.save", json!({"path": saved})),
        ]);
        let (_, r) = drive(lines(&steps));
        for (n, reply) in r.iter().enumerate().take(steps.len() + 1) {
            assert_eq!(reply["ok"], true, "line {n}: {reply}");
        }
        assert_eq!(r[3]["state"]["containers"]["artboards"][1]["options"]["show_center_mark"], true);
        assert_eq!(r[4]["state"]["containers"]["active"], json!(added));
        let shape = r[5]["state"]["layers"].as_array().unwrap().len();
        assert_eq!(shape, 1, "{}", r[5]["state"]["layers"]);
        assert_eq!(r[6]["state"]["frame"]["width"], 512, "{}", r[6]["state"]["frame"]);
        assert_eq!(r[7]["state"]["layers"].as_array().unwrap().len(), 0, "undo");
        assert_eq!(r[8]["state"]["layers"].as_array().unwrap().len(), 1, "redo");
        assert!(Path::new(&png).exists());
        let again = reopen(Path::new(&saved));
        assert_eq!(containers(&again)["artboards"].as_array().unwrap().len(), 2);
        assert_eq!(containers(&again)["artboards"][1]["options"]["show_center_mark"], true);
        assert_eq!(again.layers().len(), 1);
    }

    /// `new` then `open`: the composition token a path that creates a document stores (STU-TYP-130),
    /// and the document is dirty until saved. The CLI's attribution reaches the op records.
    #[test]
    fn a_new_document_holds_the_current_token_and_the_callers_attribution() {
        let scratch = Scratch::new("token");
        let mut v = vector_doc(10.0, 10.0);
        assert_eq!(v.run().authored_composition().map(|a| a.stored), Ok(true));
        let out = scratch.path("cli.handshake");
        let mut args = vec!["new", &out, "--profile", "design", "--overwrite"];
        args.extend_from_slice(&WHO);
        let (exit, report) = cli_json(&args);
        assert_eq!(exit, 0, "{report}");
        let new_op = report["ops"].as_array().unwrap().iter().find(|o| o["op"] == "new_document").unwrap();
        assert_eq!(new_op["actor"]["account_id"], "doc-new-test", "{new_op}");
        assert_eq!(new_op["command_id"], "document.new");
        assert_eq!(new_op["business_events"][0]["event"], "STUDIO_DOCUMENT_CREATED", "{new_op}");
        assert!(new_op["detail"]["document_id"].as_str().unwrap().starts_with("SDOC-"));
        // Two documents are two ids.
        let a = v.new_document(&NewDocument::new(DocumentProfile::Design, None), None).unwrap();
        let b = v.new_document(&NewDocument::new(DocumentProfile::Design, None), None).unwrap();
        assert_ne!(a, b);
    }
}
