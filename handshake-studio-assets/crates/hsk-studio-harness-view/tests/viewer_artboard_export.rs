//! Lane SMB-ARTBOARD-EXPORT-1 (feature `viewer`; STU-VEC-028, STU-IO-010, STU-IO-111 to STU-IO-114,
//! STU-IO-117): a named board target for the vector exports, per-board and per-node export, and the
//! node export settings record with SCALE, WIDTH and HEIGHT constraints writing many named files in one
//! run. Scenarios vector-brand steps 85, 86, 88, 90; product-design 53, 82, 85; board-slides-web 79.
#![cfg(feature = "viewer")]

mod support_context;

use hsk_studio_harness_core::document_new::{DocumentProfile, NewDocument};
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::doc_export::{DocExportRecipe, DocFormat};
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::vector::{ShapeKind, VectorTool};
use serde_json::{Map, Value, json};
use std::path::PathBuf;

fn vector_doc(w: f64, h: f64) -> Viewer {
    let mut v = support_context::viewer(Prefer::Cpu);
    v.new_document(&NewDocument::new(DocumentProfile::Vector, Some((w, h))), None).unwrap();
    v
}

fn black() -> VectorTool {
    VectorTool { fill_on: true, fill: [0, 0, 0], stroke_on: false, stroke: [0, 0, 0], stroke_width: 2.0, ..VectorTool::default() }
}

/// The four numbers of the root `viewBox`.
fn view_box(svg: &str) -> [f64; 4] {
    let start = svg.find("viewBox=\"").unwrap_or_else(|| panic!("no viewBox: {}", &svg[..svg.len().min(300)])) + 9;
    let end = start + svg[start..].find('"').unwrap();
    let n: Vec<f64> = svg[start..end].split(' ').map(|t| t.parse().unwrap()).collect();
    [n[0], n[1], n[2], n[3]]
}

fn near(a: [f64; 4], b: [f64; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6)
}

/// vector-brand step 88: a board off the document origin exports as SVG framed on that board, so its
/// art lies inside the `viewBox` (the board's origin and size in document pixels), not outside it.
#[test]
fn svg_frames_the_chosen_board_at_its_origin() {
    let mut v = vector_doc(128.0, 128.0);
    let wide = v.add_artboard("wide", 200.0, 64.0, None).unwrap();
    v.set_vector_tool(black());
    v.go_to_artboard(&wide).unwrap();
    v.draw_shape(ShapeKind::Rectangle, [200.0, 8.0], [260.0, 56.0], false).unwrap();
    let (svg, _) = v.vector_svg().unwrap();
    assert!(near(view_box(&svg), [192.0, 0.0, 200.0, 64.0]), "the board of x 192: {}", &svg[..svg.len().min(300)]);
    // The first board keeps the document origin.
    let first = v.state_json()["containers"]["artboards"][0]["id"].as_str().unwrap().to_owned();
    v.go_to_artboard(&first).unwrap();
    assert!(near(view_box(&v.vector_svg().unwrap().0), [0.0, 0.0, 128.0, 128.0]));
}

/// Test scratch directory under TMP, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hsk-artboard-export-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// V4-06 sibling site: the document export recipe frames the chosen board, as the canvas and the PNG
/// export do (it rendered the first board whatever Go to Artboard chose).
#[test]
fn export_document_frames_the_chosen_board() {
    let scratch = Scratch::new("doc-export");
    let mut v = vector_doc(128.0, 128.0);
    let wide = v.add_artboard("wide", 200.0, 64.0, None).unwrap();
    v.go_to_artboard(&wide).unwrap();
    let report = v.export_document(&scratch.0.join("wide.png"), &DocExportRecipe::of_format(DocFormat::Png { depth: 8 }), false).unwrap();
    assert_eq!((report["width"].as_u64(), report["height"].as_u64()), (Some(200), Some(64)), "{report}");
}

/// The typed command path the drive surface and a panel take.
fn command(v: &mut Viewer, id: &str, params: Value) -> Result<Value, (String, Value)> {
    let map: Map<String, Value> = params.as_object().unwrap().clone();
    v.run_command(id, &map).map_err(|r| (r.code, r.detail))
}

fn refusal(v: &mut Viewer, id: &str, params: Value) -> String {
    command(v, id, params).expect_err("refused").0
}

fn undo_labels(v: &Viewer) -> Vec<String> {
    v.run().history()["undo"].as_array().unwrap().iter().map(|l| l.as_str().unwrap().to_owned()).collect()
}

/// The centre pixel of a PNG file, RGBA.
fn centre(path: &std::path::Path) -> [u8; 4] {
    let img = hsk_studio_interop_raster::decode(&std::fs::read(path).unwrap(), &hsk_studio_interop_raster::Limits::default(), &hsk_studio_accord::CancellationToken::default()).unwrap();
    let hsk_studio_interop_raster::Samples::U8(s) = &img.samples else { panic!("8-bit") };
    let n = s.len() / (img.width as usize * img.height as usize);
    let at = ((img.height as usize / 2) * img.width as usize + img.width as usize / 2) * n;
    if n == 4 { [s[at], s[at + 1], s[at + 2], s[at + 3]] } else { [s[at], s[at + 1], s[at + 2], 255] }
}

/// A brand document: board `mark` (128 square at the origin) with a square, board `lockup` (200 by 64 at
/// x 192) with a 60 by 48 rectangle layer named `icon`.
fn brand() -> (Viewer, String, String, String) {
    let mut v = vector_doc(128.0, 128.0);
    let mark = v.state_json()["containers"]["artboards"][0]["id"].as_str().unwrap().to_owned();
    v.rename_artboard(&mark, "mark").unwrap();
    let lockup = v.add_artboard("lockup", 200.0, 64.0, None).unwrap();
    v.set_vector_tool(black());
    v.draw_shape(ShapeKind::Rectangle, [32.0, 32.0], [96.0, 96.0], false).unwrap();
    let icon = v.draw_shape(ShapeKind::Rectangle, [200.0, 8.0], [260.0, 56.0], false).unwrap();
    v.rename_layer(&icon, "icon").unwrap();
    (v, mark, lockup, icon)
}

/// vector-brand steps 85 and 86, product-design step 82: a node is marked for export by id or name; the
/// change is one named undo step with the run's attribution and one frame, the same settings again are no
/// step, and the settings survive save and reopen and read back in state.
#[test]
fn export_settings_are_one_attributed_step_and_survive_save_and_reopen() {
    let scratch = Scratch::new("settings");
    let (mut v, mark, _, icon) = brand();
    let set = json!([{"format": "PNG"}, {"format": "PNG", "suffix": "@2x", "constraint": {"type": "SCALE", "value": 2}}, {"format": "SVG"}]);
    let detail = command(&mut v, "export.set_settings", json!({"node": "mark", "settings": set.to_string()})).unwrap();
    assert_eq!((detail["node"].as_str(), detail["settings"].as_u64(), detail["changed"].as_bool()), (Some(mark.as_str()), Some(3), Some(true)), "{detail}");
    command(&mut v, "export.set_settings", json!({"node": icon, "settings": [{"format": "PNG", "suffix": "@3x", "constraint": {"type": "SCALE", "value": 3}}]})).unwrap();
    assert_eq!(undo_labels(&v).last().map(String::as_str), Some(format!("set_export_settings {icon}").as_str()));
    let op = v.run().ops().iter().rev().find(|o| o["op"] == "set_export_settings").unwrap().clone();
    assert_eq!((op["command_id"].as_str(), op["observe"]["frames"].as_u64()), (Some("export.set_settings"), Some(1)), "{op}");
    assert!(op["actor"]["account_id"].is_string(), "{op}");
    let state = v.state_json()["export_settings"].clone();
    assert_eq!(state["nodes"].as_array().unwrap().len(), 2, "{state}");
    assert_eq!(state["nodes"][0]["settings"][1]["suffix"], "@2x", "{state}");
    // The same settings again: no step.
    let steps = undo_labels(&v).len();
    let again = command(&mut v, "export.set_settings", json!({"node": "mark", "settings": set.to_string()})).unwrap();
    assert_eq!((again["changed"].as_bool(), undo_labels(&v).len()), (Some(false), steps));
    // Undo and redo the icon's settings.
    v.undo().unwrap();
    assert_eq!(v.state_json()["export_settings"]["nodes"].as_array().unwrap().len(), 1);
    v.redo().unwrap();
    assert_eq!(v.state_json()["export_settings"], state);
    // Save and reopen.
    let saved = scratch.0.join("brand.handshake");
    v.save(&saved, false).unwrap();
    let mut reopened = support_context::viewer(Prefer::Cpu);
    reopened.open(&saved).unwrap();
    assert_eq!(reopened.state_json()["export_settings"], state);
    // An empty list removes a node's settings.
    command(&mut v, "export.set_settings", json!({"node": icon, "settings": "[]"})).unwrap();
    assert_eq!(v.state_json()["export_settings"]["nodes"].as_array().unwrap().len(), 1);
}

/// Hostile or impossible settings are typed refusals that change nothing (STU-IO-114, STU-IO-117).
#[test]
fn hostile_export_settings_are_typed_and_change_nothing() {
    let (mut v, _, _, icon) = brand();
    let before = (v.state_json()["export_settings"].clone(), undo_labels(&v));
    for (settings, code) in [
        (json!([{"format": "PNG", "suffix": "@2x"}, {"format": "PNG", "suffix": "@2x", "constraint": {"type": "SCALE", "value": 3}}]), "export_suffix_collision"),
        (json!([{"format": "PNG", "constraint": {"type": "SCALE", "value": 0}}]), "export_constraint_out_of_range"),
        (json!([{"format": "PNG", "constraint": {"type": "WIDTH", "value": 100001}}]), "export_constraint_out_of_range"),
        (json!([{"format": "PNG", "suffix": "../up"}]), "export_suffix_invalid"),
        (json!([{"format": "GIF"}]), "parameter_invalid"),
        (json!({"format": "PNG"}), "parameter_invalid"),
    ] {
        assert_eq!(refusal(&mut v, "export.set_settings", json!({"node": icon, "settings": settings})), code, "{settings}");
    }
    assert_eq!(refusal(&mut v, "export.set_settings", json!({"node": "no-such-node", "settings": [{"format": "PNG"}]})), "unknown_node");
    assert_eq!(refusal(&mut v, "export.set_settings", json!({"node": icon})), "parameter_missing");
    assert_eq!((v.state_json()["export_settings"].clone(), undo_labels(&v)), before);
}

/// vector-brand steps 88 and 90, product-design steps 53 and 85, board-slides-web step 79: one run writes
/// every marked node's files, named `<node><suffix>.<ext>`, each at its constraint; a layer exports at its
/// own bounds with only its own content; ad-hoc settings export every artboard; nothing is replaced unasked.
#[test]
fn export_run_writes_every_named_file_at_its_constraint() {
    let scratch = Scratch::new("run");
    let (mut v, _, _, _) = brand();
    command(&mut v, "export.set_settings", json!({"node": "mark", "settings": [
        {"format": "PNG"}, {"format": "PNG", "suffix": "@2x", "constraint": {"type": "SCALE", "value": 2}},
        {"format": "PNG", "suffix": "-16", "constraint": {"type": "WIDTH", "value": 16}}, {"format": "JPG", "suffix": "-h32", "constraint": {"type": "HEIGHT", "value": 32}},
        {"format": "SVG"}, {"format": "PDF"},
    ]})).unwrap();
    command(&mut v, "export.set_settings", json!({"node": "icon", "settings": [{"format": "PNG", "suffix": "@3x", "constraint": {"type": "SCALE", "value": 3}}, {"format": "SVG"}]})).unwrap();
    let revision = v.run().snapshot().unwrap().document().revision;
    let dir = scratch.0.join("out");
    std::fs::create_dir_all(&dir).unwrap();
    let report = command(&mut v, "export.run", json!({"dir": dir.to_string_lossy()})).unwrap();
    assert_eq!(report["count"], 8, "{report}");
    let size = |file: &str| {
        let f = report["files"].as_array().unwrap().iter().find(|f| f["file"] == file).unwrap_or_else(|| panic!("{file}: {report}"));
        (f["width"].as_f64().unwrap(), f["height"].as_f64().unwrap())
    };
    assert_eq!(size("mark.png"), (128.0, 128.0));
    assert_eq!(size("mark@2x.png"), (256.0, 256.0));
    assert_eq!(size("mark-16.png"), (16.0, 16.0));
    assert_eq!(size("mark-h32.jpg"), (32.0, 32.0));
    assert_eq!(size("icon@3x.png"), (180.0, 144.0), "the layer's own bounds at 3x");
    for file in ["mark.png", "mark@2x.png", "mark-16.png", "mark-h32.jpg", "mark.svg", "mark.pdf", "icon@3x.png", "icon.svg"] {
        assert!(std::fs::metadata(dir.join(file)).unwrap().len() > 0, "{file}");
    }
    assert_eq!(centre(&dir.join("mark.png")), [0, 0, 0, 255], "the square in the mark board");
    assert_eq!(centre(&dir.join("icon@3x.png")), [0, 0, 0, 255], "the icon fills its own frame");
    assert!(near(view_box(&std::fs::read_to_string(dir.join("icon.svg")).unwrap()), [200.0, 8.0, 60.0, 48.0]));
    assert_eq!(v.run().snapshot().unwrap().document().revision, revision, "read-only on the document");
    let op = v.run().ops().iter().rev().find(|o| o["op"] == "export.run").unwrap().clone();
    assert_eq!((op["observe"]["frames"].as_u64(), op["detail"]["files"].as_u64()), (Some(1), Some(8)), "{op}");
    // Nothing is replaced unasked; with overwrite the run replaces its own files, byte for byte the same.
    let mark_png = std::fs::read(dir.join("mark.png")).unwrap();
    assert_eq!(refusal(&mut v, "export.run", json!({"dir": dir.to_string_lossy()})), "destination_exists");
    command(&mut v, "export.run", json!({"dir": dir.to_string_lossy(), "nodes": ["mark"], "overwrite": true})).unwrap();
    assert_eq!(std::fs::read(dir.join("mark.png")).unwrap(), mark_png, "deterministic");
    // Ad-hoc settings: every artboard as SVG, nothing stored; the lockup board frames its own origin.
    let boards = scratch.0.join("boards");
    std::fs::create_dir_all(&boards).unwrap();
    let r = command(&mut v, "export.run", json!({"dir": boards.to_string_lossy(), "settings": [{"format": "SVG"}]})).unwrap();
    assert_eq!(r["count"], 2, "{r}");
    assert!(near(view_box(&std::fs::read_to_string(boards.join("lockup.svg")).unwrap()), [192.0, 0.0, 200.0, 64.0]));
    assert_eq!(v.state_json()["export_settings"]["nodes"].as_array().unwrap().len(), 2, "ad-hoc settings are not stored");
}

/// Hostile runs are typed and write nothing: a missing folder, an unknown or unmarked node, a name two
/// nodes share, an unsupported profile, an output past the pixel budget, a repeated node.
#[test]
fn hostile_export_runs_are_typed_and_write_nothing() {
    let scratch = Scratch::new("run-hostile");
    let (mut v, _, lockup, icon) = brand();
    let dir = scratch.0.to_string_lossy().into_owned();
    let count = || std::fs::read_dir(&scratch.0).unwrap().count();
    assert_eq!(refusal(&mut v, "export.run", json!({"dir": scratch.0.join("missing").to_string_lossy()})), "export_dir_missing");
    assert_eq!(refusal(&mut v, "export.run", json!({"dir": dir})), "nothing_to_export");
    assert_eq!(refusal(&mut v, "export.run", json!({"dir": dir, "nodes": ["no-such-node"], "settings": [{"format": "PNG"}]})), "unknown_node");
    assert_eq!(refusal(&mut v, "export.run", json!({"dir": dir, "nodes": [icon]})), "export_node_has_no_settings");
    v.rename_layer(&icon, "lockup").unwrap();
    assert_eq!(refusal(&mut v, "export.run", json!({"dir": dir, "nodes": [lockup, icon], "settings": [{"format": "PNG"}]})), "export_name_collision");
    assert_eq!(refusal(&mut v, "export.run", json!({"dir": dir, "nodes": [icon], "settings": [{"format": "PNG", "colour_profile": "DISPLAY_P3_V4"}]})), "export_colour_profile_unsupported");
    assert_eq!(refusal(&mut v, "export.run", json!({"dir": dir, "nodes": [lockup], "settings": [{"format": "PNG", "constraint": {"type": "SCALE", "value": 100}}]})), "export_too_large");
    assert_eq!(refusal(&mut v, "export.run", json!({"dir": dir, "settings": [{"format": "PNG", "suffix": "a"}, {"format": "PNG", "suffix": "a"}]})), "export_suffix_collision");
    assert_eq!(refusal(&mut v, "export.run", json!({"dir": dir, "nodes": [icon, icon], "settings": [{"format": "PNG"}]})), "export_node_repeated");
    assert_eq!(count(), 0, "nothing written");
}
