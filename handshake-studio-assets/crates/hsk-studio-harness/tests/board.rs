//! Lane SMB-BOARD-1 (feature `viewer`): whiteboard mode, first slice ([STU-WB-001], [STU-WB-002],
//! [STU-WB-102] to [STU-WB-109], [STU-WB-115], [STU-WB-120], [STU-WB-127], [STU-WB-134], [STU-WB-142]): a board
//! document, boards, stickies, shapes with text, sections and connectors through the typed `board.*`
//! commands, as a model runs them (`Viewer::run_command` and the JSONL loop of `cli/drive.rs`). Every mutating
//! command is one undo step and the result reaches save, reopen and a PNG export. FigJam behaviour is
//! UNVERIFIED (D5): sizes, routes and cap drawings are Studio readings.
#![cfg(feature = "viewer")]

mod common;

use common::Scratch;
use hsk_studio_accord::CancellationToken;
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness_core::document_new::{DocumentProfile, NewDocument};
use hsk_studio_interop_raster as raster;
use serde_json::{Value, json};
use std::sync::mpsc;
use std::time::Duration;

fn board_doc() -> Viewer {
    let mut v = Viewer::new(Prefer::Cpu);
    v.new_document(&NewDocument::new(DocumentProfile::Board, None), None).unwrap();
    v
}

fn call(v: &mut Viewer, id: &str, params: Value) -> Result<Value, (String, Value)> {
    let Value::Object(map) = params else { panic!("parameters are an object") };
    v.run_command(id, &map).map_err(|r| (r.code, r.detail))
}

fn ok(v: &mut Viewer, id: &str, params: Value) -> Value {
    call(v, id, params).unwrap_or_else(|(code, detail)| panic!("{id}: {code} {detail}"))
}

fn refused(v: &mut Viewer, id: &str, params: Value, code: &str) {
    match call(v, id, params) {
        Ok(r) => panic!("{id} should refuse {code}: {r}"),
        Err((c, detail)) => assert_eq!(c, code, "{id}: {detail}"),
    }
}

fn text(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or_else(|| panic!("no {key} in {v}")).to_string()
}

/// The document's layers and its vector and text entry bytes (what an undo must restore; review F7).
fn snapshot(v: &Viewer) -> (String, Vec<u8>, Vec<u8>) {
    let doc = v.run().snapshot().unwrap().document();
    let entry = |token: &str| v.run().document_entry(token).map(<[u8]>::to_vec).unwrap_or_default();
    (serde_json::to_string(&doc.layers).unwrap(), entry("vector"), entry("text"))
}

/// The vector entry as JSON.
fn vector_entry(v: &Viewer) -> Value {
    serde_json::from_slice(v.run().document_entry("vector").expect("a vector entry")).unwrap()
}

/// The text entry as JSON.
fn text_entry(v: &Viewer) -> Value {
    v.run().document_entry("text").map_or(Value::Null, |b| serde_json::from_slice(b).unwrap())
}

/// Board node records and area-text bindings of the vector entry naming `layer`.
fn records_naming(v: &Viewer, layer: &str) -> usize {
    let e = vector_entry(v);
    let r = &e["vector_records"];
    let count = |key: &str, field: &str| r[key].as_array().map_or(0, |a| a.iter().filter(|x| x[field] == layer).count());
    count("board_nodes", "layer_id") + count("area_texts", "container_layer_id")
}

fn node(v: &mut Viewer, id: &str) -> Value {
    ok(v, "board.node.read", json!({"node": id}))
}

fn bounds(n: &Value) -> [f64; 4] {
    let b = n["bounds"].as_array().unwrap_or_else(|| panic!("no bounds in {n}"));
    [0, 1, 2, 3].map(|i| b[i].as_f64().unwrap())
}

fn route(n: &Value) -> Vec<[f64; 2]> {
    n["route"].as_array().unwrap_or_else(|| panic!("no route in {n}")).iter().map(|p| [p[0].as_f64().unwrap(), p[1].as_f64().unwrap()]).collect()
}

fn layer_kind(v: &Viewer, id: &str) -> String {
    let doc = v.run().snapshot().unwrap().document();
    doc.layers.iter().find(|l| l.layer_id == id).map(|l| l.kind.clone()).unwrap_or_else(|| panic!("no layer {id}"))
}

fn shape(v: &mut Viewer, x: f64, y: f64, label: &str) -> String {
    text(&ok(v, "board.shape_with_text.create", json!({"x": x, "y": y, "shape_type": "studio_declared:rectangle", "text": label})), "node_id")
}

#[test]
fn a_board_document_starts_in_whiteboard_mode_and_board_add_is_one_undo_step() {
    let mut v = board_doc();
    let r = ok(&mut v, "board.read", json!({}));
    assert_eq!(r["whiteboard_mode"], true, "{r}");
    assert_eq!(r["boards"].as_array().unwrap().len(), 1, "{r}");
    assert_eq!(r["boards"][0]["name"], "Board 1");
    assert_eq!(r["boards"][0]["bounds"], json!([0.0, 0.0, 1920.0, 1080.0]));
    let added = ok(&mut v, "board.add", json!({"name": "Affinity"}));
    let board = text(&added, "board_id");
    let r = ok(&mut v, "board.read", json!({}));
    assert_eq!(r["boards"][1]["board_id"], json!(board), "{r}");
    assert_eq!(r["boards"][1]["name"], "Affinity");
    v.undo().unwrap();
    assert_eq!(ok(&mut v, "board.read", json!({}))["boards"].as_array().unwrap().len(), 1);
    v.redo().unwrap();
    assert_eq!(ok(&mut v, "board.read", json!({}))["boards"].as_array().unwrap().len(), 2);
    // A node goes to the board it names; a read of that board lists only its nodes.
    let s = ok(&mut v, "board.sticky.create", json!({"board": board, "x": 2100, "y": 40, "text": "Plan"}));
    assert_eq!(s["board_id"], json!(board));
    let r = ok(&mut v, "board.read", json!({"board": board}));
    assert_eq!(r["nodes"].as_array().unwrap().len(), 1, "{r}");
    // Review F4 (lead ruling 2026-11-13): a vector document is never switched into whiteboard mode silently;
    // a board node or a board in it is refused typed.
    let mut w = Viewer::new(Prefer::Cpu);
    w.new_document(&NewDocument::new(DocumentProfile::Vector, Some((800.0, 600.0))), None).unwrap();
    assert_eq!(ok(&mut w, "board.read", json!({}))["whiteboard_mode"], false);
    refused(&mut w, "board.add", json!({"name": "B"}), "not_whiteboard_document");
    refused(&mut w, "board.sticky.create", json!({"x": 10, "y": 10, "text": "first"}), "not_whiteboard_document");
    assert_eq!(ok(&mut w, "board.read", json!({}))["whiteboard_mode"], false);
}

/// Review F4 (lead ruling, option B): `new --profile board` from the command line and `document.new` in a
/// session both write whiteboard mode at creation, and it survives save and reopen.
#[test]
fn whiteboard_mode_is_written_at_creation_by_the_cli_and_the_viewer_and_survives_reopen() {
    let scratch = Scratch::new("board-cli");
    let package = scratch.0.join("cli.handshake");
    let args: Vec<String> = ["new", package.to_str().unwrap(), "--profile", "board", "--size", "800x600", "--actor", "board-cli", "--session", "s"].iter().map(|s| (*s).to_string()).collect();
    let out = hsk_studio_harness::cli::run(&args);
    assert_eq!(out.exit, 0, "{}", out.stdout);
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&package).unwrap();
    assert_eq!(ok(&mut v, "board.read", json!({}))["whiteboard_mode"], true, "the CLI writes the mode");
    ok(&mut v, "board.add", json!({"name": "Plan"}));
    ok(&mut v, "board.sticky.create", json!({"x": 10, "y": 10, "text": "first"}));
    let mut w = board_doc();
    let saved = scratch.0.join("viewer.handshake");
    w.save(&saved, false).unwrap();
    let mut r = Viewer::new(Prefer::Cpu);
    r.open(&saved).unwrap();
    assert_eq!(ok(&mut r, "board.read", json!({}))["whiteboard_mode"], true, "the viewer's mode survives save and reopen");
    ok(&mut r, "board.add", json!({"name": "Plan"}));
}

/// Review F1 (lead ruling): the plain layer delete drops the node's board record and text binding in its own
/// undo step, so a layer made afterwards never meets a stale record; undo brings both back.
#[test]
fn a_layer_delete_drops_the_board_record_in_the_same_step_and_the_board_stays_readable() {
    let scratch = Scratch::new("board-delete");
    let mut v = board_doc();
    let s = text(&ok(&mut v, "board.sticky.create", json!({"x": 0, "y": 0, "text": "A"})), "node_id");
    assert_eq!(records_naming(&v, &s), 2, "a record and a text binding");
    let before = snapshot(&v);
    v.delete_layer(&s).unwrap();
    assert_eq!(records_naming(&v, &s), 0, "the delete drops the record and the binding");
    v.draw_shape(hsk_studio_harness::view::vector::ShapeKind::Rectangle, [0.0, 0.0], [50.0, 50.0], false).unwrap();
    ok(&mut v, "board.read", json!({}));
    v.render_if_dirty();
    assert!(v.render_error().is_none(), "{:?}", v.render_error());
    let path = scratch.0.join("after-delete.handshake");
    v.save(&path, false).unwrap();
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&path).unwrap();
    ok(&mut w, "board.read", json!({}));
    v.undo().unwrap();
    v.undo().unwrap();
    assert_eq!(snapshot(&v), before, "undo of the delete brings the record back");
}

/// Review F2 (lead ruling): a move replaces the node's path records in place; nothing grows with moves, and
/// one undo returns the state before the last move exactly.
#[test]
fn five_thousand_moves_keep_the_vector_entry_bounded_and_undo_exact() {
    let mut v = board_doc();
    let a = text(&ok(&mut v, "board.sticky.create", json!({"x": 0, "y": 0, "text": "A"})), "node_id");
    let b = text(&ok(&mut v, "board.sticky.create", json!({"x": 600, "y": 0, "text": "B"})), "node_id");
    ok(&mut v, "board.connector.create", json!({"start": {"endpoint_node_id": a, "magnet": "AUTO"}, "end": {"endpoint_node_id": b, "magnet": "AUTO"}}));
    let paths = vector_entry(&v)["paths"].as_array().unwrap().len();
    let started = std::time::Instant::now();
    let mut last = snapshot(&v);
    for i in 0..5000_u32 {
        if i == 4999 {
            last = snapshot(&v);
        }
        ok(&mut v, "board.node.move", json!({"node": a, "x": f64::from(i % 50), "y": f64::from(i % 7) * 10.0}));
    }
    println!("5000 moves in {:?}", started.elapsed());
    assert_eq!(vector_entry(&v)["paths"].as_array().unwrap().len(), paths, "a move adds no path record");
    v.undo().unwrap();
    assert_eq!(snapshot(&v), last, "one undo returns the state before the last move");
}

/// Review F2 (lead ruling): a text write rewrites the node's own story in place, inside the undo step; nothing
/// grows with edits, no cap is met, and undo restores the text entry exactly.
#[test]
fn a_thousand_text_writes_keep_the_text_entry_bounded_and_undo_exact() {
    let mut v = board_doc();
    let s = text(&ok(&mut v, "board.sticky.create", json!({"x": 0, "y": 0, "text": "A"})), "node_id");
    let stories = text_entry(&v)["stories"].as_array().unwrap().len();
    let mut last = snapshot(&v);
    for i in 0..1000_u32 {
        if i == 999 {
            last = snapshot(&v);
        }
        ok(&mut v, "board.node.set_text", json!({"node": s, "text": format!("edit {i}")}));
    }
    assert_eq!(text_entry(&v)["stories"].as_array().unwrap().len(), stories, "a text write adds no story");
    assert_eq!(node(&mut v, &s)["text"], "edit 999");
    v.undo().unwrap();
    assert_eq!(snapshot(&v), last, "one undo restores the layers, the vector entry and the text entry");
    assert_eq!(node(&mut v, &s)["text"], "edit 998");
}

/// Review F5 (lead ruling): a connector collapsed to one point never blocks a node move; it reads unroutable
/// with a connector_route_degenerate warning until its nodes separate.
#[test]
fn moving_a_node_against_its_connected_neighbour_is_not_refused() {
    let mut v = board_doc();
    let a = text(&ok(&mut v, "board.sticky.create", json!({"x": 0, "y": 0, "text": "A"})), "node_id");
    let b = text(&ok(&mut v, "board.sticky.create", json!({"x": 600, "y": 0, "text": "B"})), "node_id");
    let c = text(&ok(&mut v, "board.connector.create", json!({"start": {"endpoint_node_id": a, "magnet": "AUTO"}, "end": {"endpoint_node_id": b, "magnet": "AUTO"}})), "node_id");
    let before = snapshot(&v);
    ok(&mut v, "board.node.move", json!({"node": b, "x": 240, "y": 0}));
    let n = node(&mut v, &c);
    assert_eq!(n["state"], "unroutable", "{n}");
    assert_eq!(n["issues"][0]["code"], "connector_route_degenerate", "{n}");
    assert_eq!(n["issues"][0]["severity"], "warning", "{n}");
    v.undo().unwrap();
    assert_eq!(snapshot(&v), before);
    ok(&mut v, "board.node.move", json!({"node": b, "x": 240, "y": 0}));
    ok(&mut v, "board.node.move", json!({"node": b, "x": 700, "y": 0}));
    let n = node(&mut v, &c);
    assert_eq!((n["state"].clone(), n["route_current"].clone()), (json!("attached"), json!(true)), "{n}");
}

/// Review F6 (lead ruling): an arrange transform keeps the board: the node's text follows its outline and its
/// connectors reroute in the same step; pathfinder and outlines on a board node are refused typed.
#[test]
fn arrange_keeps_board_nodes_and_vector_rewrites_refuse_them() {
    use hsk_studio_harness::view::vector::TransformOp;
    use hsk_studio_harness::view::vector::arrange::ReferencePoint;
    let mut v = board_doc();
    let a = shape(&mut v, 100.0, 100.0, "Discover");
    let b = shape(&mut v, 600.0, 100.0, "Visit");
    let c = text(&ok(&mut v, "board.connector.create", json!({"start": {"endpoint_node_id": a, "magnet": "RIGHT"}, "end": {"endpoint_node_id": b, "magnet": "LEFT"}})), "node_id");
    v.transform_layers(&[b.clone()], TransformOp::Move { dx: 0.0, dy: 200.0 }, ReferencePoint::Centre).unwrap();
    let e = vector_entry(&v);
    let doc = v.run().snapshot().unwrap().document().clone();
    let layer = doc.layers.iter().find(|l| l.layer_id == b).unwrap();
    let paths: Vec<String> = serde_json::to_value(&layer.payload).unwrap()["path_ids"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_owned()).collect();
    let container = e["vector_records"]["area_texts"].as_array().unwrap().iter().find(|t| t["container_layer_id"] == json!(b)).unwrap()["container_id"].as_str().unwrap().to_owned();
    assert!(paths.contains(&container), "the text follows the outline: {container} in {paths:?}");
    let n = node(&mut v, &c);
    assert_eq!((n["state"].clone(), n["route_current"].clone()), (json!("attached"), json!(true)), "{n}");
    assert_eq!(bounds(&node(&mut v, &b))[1], 300.0);
    let before = snapshot(&v);
    let unite = v.pathfinder_layers(hsk_studio_nib::pathfinder::PathfinderOp::Union, &[a.clone(), b.clone()]);
    assert!(unite.err().unwrap_or_default().contains("board_node_not_vector_editable"));
    let outlines = v.create_outlines(&a);
    assert!(outlines.err().unwrap_or_default().contains("board_node_not_vector_editable"));
    assert_eq!(snapshot(&v), before, "a refused rewrite changes nothing");
}

/// Review F8: the ERD zero is a hollow ring: its centre shows the background.
#[test]
fn an_erd_zero_ring_is_hollow() {
    let scratch = Scratch::new("board-erd");
    let mut v = board_doc();
    let a = text(&ok(&mut v, "board.sticky.create", json!({"x": 0, "y": 0, "text": "A"})), "node_id");
    let b = text(&ok(&mut v, "board.sticky.create", json!({"x": 600, "y": 0, "text": "B"})), "node_id");
    ok(&mut v, "board.connector.create", json!({"start": {"endpoint_node_id": a, "magnet": "RIGHT"}, "end": {"endpoint_node_id": b, "magnet": "LEFT"}, "connector_line_type": "STRAIGHT", "connector_end_stroke_cap": "ERD_ZERO_OR_MORE"}));
    let png = scratch.0.join("erd.png");
    let report = ok(&mut v, "board.export_png", json!({"path": png.to_string_lossy()}));
    let origin = [report["bounds"][0].as_f64().unwrap(), report["bounds"][1].as_f64().unwrap()];
    let img = raster::decode(&std::fs::read(&png).unwrap(), &raster::Limits::default(), &CancellationToken::default()).unwrap();
    let raster::Samples::U8(px) = &img.samples else { panic!("8-bit PNG") };
    let channels = px.len() / (img.width as usize * img.height as usize);
    let at = |x: f64, y: f64| {
        let i = (((y - origin[1]) as usize) * img.width as usize + (x - origin[0]) as usize) * channels;
        px[i..i + channels].to_vec()
    };
    // The ring centre at stroke width 2 (composite board.rs: cap size 12, ring radius 6, centre 12 + 6 + 2 back
    // from the tip) and a background point beside it.
    let back = 20.0;
    assert_eq!(at(600.0 - back, 120.0), at(600.0 - back, 90.0), "the ring centre is background");
}

/// Review F11: every native shape's text fits it.
#[test]
fn every_native_shape_fits_its_text() {
    let mut v = board_doc();
    let tokens = ["rectangle", "rounded_rectangle", "ellipse", "diamond", "triangle", "parallelogram", "hexagon", "cylinder", "cloud", "document", "predefined_process", "terminator"];
    for (i, token) in tokens.iter().enumerate() {
        let made = ok(&mut v, "board.shape_with_text.create", json!({"x": 300.0 * (i % 4) as f64, "y": 200.0 * (i / 4) as f64, "shape_type": format!("studio_declared:{token}"), "text": format!("{token} text")}));
        let n = node(&mut v, &text(&made, "node_id"));
        assert_eq!(n["text_overset"], 0, "{token}: {n}");
    }
}

/// Review F12: a sticky wraps text over several lines (an ordinary story); a connector reads no fill.
#[test]
fn a_sticky_holds_several_lines_and_a_connector_reads_no_fill() {
    let mut v = board_doc();
    let s = ok(&mut v, "board.sticky.create", json!({"x": 0, "y": 0, "text": "Goals\nParking at night\nShuttle"}));
    let sticky = text(&s, "node_id");
    let n = node(&mut v, &sticky);
    assert_eq!(n["text"], "Goals\nParking at night\nShuttle");
    assert_eq!(n["text_overset"], 0, "{n}");
    let a = shape(&mut v, 400.0, 0.0, "A");
    let c = ok(&mut v, "board.connector.create", json!({"start": {"endpoint_node_id": sticky, "magnet": "AUTO"}, "end": {"endpoint_node_id": a, "magnet": "AUTO"}}));
    let n = node(&mut v, &text(&c, "node_id"));
    assert_eq!(n["fill"], Value::Null, "STU-WB-106: a connector carries no fill: {n}");
    assert!(n["stroke"].is_array(), "{n}");
}

#[test]
fn stickies_shapes_and_sections_are_board_layer_kinds_each_made_and_edited_in_one_undo_step() {
    let mut v = board_doc();
    let before = snapshot(&v);
    let made = ok(&mut v, "board.sticky.create", json!({"x": 100, "y": 100, "text": "Night-time parking is the blocker", "author_visible": true}));
    let sticky = text(&made, "node_id");
    assert_eq!(layer_kind(&v, &sticky), "sticky", "STU-WB-102: the layer's kind is the board node kind");
    let n = node(&mut v, &sticky);
    assert_eq!(n["kind"], "sticky");
    assert_eq!(n["text"], "Night-time parking is the blocker");
    assert_eq!(n["fields"]["author_visible"], true);
    assert_eq!(n["fields"]["author_name"], json!(v.run().actor().account_id()), "the display name defaults to the creating actor");
    assert_eq!(n["fields"]["is_wide_width"], false);
    assert_eq!(bounds(&n), [100.0, 100.0, 240.0, 240.0]);
    v.undo().unwrap();
    assert_eq!(snapshot(&v), before, "one undo removes the sticky and its record");
    v.redo().unwrap();
    // The wide variant, the author display name and the colour are edits of their own.
    let wide = ok(&mut v, "board.sticky.set", json!({"node": sticky, "is_wide_width": true, "author_name": "Ilse"}));
    assert_eq!(bounds(&wide)[2], 480.0, "{wide}");
    assert_eq!(wide["fields"]["author_name"], "Ilse");
    ok(&mut v, "board.node.set_fill", json!({"node": sticky, "fill": [255, 170, 200]}));
    assert_eq!(node(&mut v, &sticky)["fill"], json!([255, 170, 200]));
    v.undo().unwrap();
    v.undo().unwrap();
    let n = node(&mut v, &sticky);
    assert_eq!((bounds(&n)[2], n["fields"]["author_name"].clone()), (240.0, json!(v.run().actor().account_id())));

    // A shape with text keeps its vocabulary and token apart and resizes around its text.
    let s = shape(&mut v, 400.0, 100.0, "Discover");
    assert_eq!(layer_kind(&v, &s), "shape_with_text");
    let n = node(&mut v, &s);
    assert_eq!(n["fields"]["shape_type"], json!({"vocabulary": "studio_declared", "token": "rectangle"}));
    let narrow = bounds(&n)[2];
    let grown = ok(&mut v, "board.node.set_text", json!({"node": s, "text": "Discover the city after dark, block by block"}));
    assert!(bounds(&grown)[2] > narrow, "{grown}");
    assert_eq!(grown["text"], "Discover the city after dark, block by block");
    v.undo().unwrap();
    assert_eq!(node(&mut v, &s)["text"], "Discover", "undo returns the old story");
    let db = ok(&mut v, "board.shape_with_text.create", json!({"x": 400, "y": 300, "shape_type": "figma_plugin_api@1.141.0:ENG_DATABASE", "text": "Store"}));
    assert_eq!(node(&mut v, &text(&db, "node_id"))["fields"]["shape_type"], json!({"vocabulary": "figma_plugin_api", "vocabulary_version": "1.141.0", "token": "ENG_DATABASE"}));
    ok(&mut v, "board.shape_with_text.set", json!({"node": s, "shape_type": "studio_declared:diamond"}));
    assert_eq!(node(&mut v, &s)["fields"]["shape_type"]["token"], "diamond");

    // A section: a named container whose members move with it.
    let sec = text(&ok(&mut v, "board.section.create", json!({"x": 50, "y": 50, "width": 900, "height": 700, "name": "Goals"})), "node_id");
    assert_eq!(layer_kind(&v, &sec), "section");
    let n = node(&mut v, &sec);
    assert_eq!(n["text"], "Goals");
    let members: Vec<String> = n["members"].as_array().unwrap().iter().map(|m| m.as_str().unwrap().to_owned()).collect();
    assert!(members.contains(&sticky) && members.contains(&s), "{n}");
    let set = ok(&mut v, "board.section.set", json!({"node": sec, "section_contents_hidden": true, "dev_status": "READY_FOR_DEV", "name": "Goals and scope"}));
    assert_eq!((set["fields"]["section_contents_hidden"].clone(), set["fields"]["dev_status"].clone(), set["text"].clone()), (json!(true), json!("READY_FOR_DEV"), json!("Goals and scope")));
    let sticky_at = bounds(&node(&mut v, &sticky));
    let moved = ok(&mut v, "board.node.move", json!({"node": sec, "x": 150, "y": 90}));
    assert!(moved["moved"].as_array().unwrap().contains(&json!(sticky)), "{moved}");
    let now = bounds(&node(&mut v, &sticky));
    assert_eq!([now[0] - sticky_at[0], now[1] - sticky_at[1]], [100.0, 40.0]);
}

#[test]
fn a_connector_attaches_to_magnets_reroutes_when_a_node_moves_and_breaks_when_its_node_is_deleted() {
    let mut v = board_doc();
    let a = shape(&mut v, 100.0, 100.0, "Discover");
    let b = shape(&mut v, 600.0, 300.0, "Visit");
    let ra = bounds(&node(&mut v, &a));
    let made = ok(
        &mut v,
        "board.connector.create",
        json!({"start": {"endpoint_node_id": a, "magnet": "RIGHT"}, "end": {"endpoint_node_id": b, "magnet": "AUTO"}, "connector_line_type": "ELBOWED", "connector_end_stroke_cap": "ARROW_EQUILATERAL"}),
    );
    let c = text(&made, "node_id");
    assert_eq!(layer_kind(&v, &c), "connector");
    let n = node(&mut v, &c);
    assert_eq!(n["state"], "attached");
    assert_eq!(n["route_current"], true);
    let r = route(&n);
    assert_eq!(r[0], [ra[0] + ra[2], ra[1] + ra[3] / 2.0], "start on A's RIGHT magnet");
    let rb = bounds(&node(&mut v, &b));
    assert_eq!(*r.last().unwrap(), [rb[0], rb[1] + rb[3] / 2.0], "AUTO picks B's side facing A");
    assert!(r.windows(2).all(|w| w[0][0] == w[1][0] || w[0][1] == w[1][1]), "ELBOWED is axis-parallel: {r:?}");
    let endpoint = n["fields"]["connector_end"].clone();

    // Moving B reroutes the connector in the same step; the endpoint record is unchanged.
    let moved = ok(&mut v, "board.node.move", json!({"node": b, "x": 600, "y": 700}));
    assert_eq!(moved["rerouted"], json!([c]), "{moved}");
    let n = node(&mut v, &c);
    // B is now mostly below A: AUTO re-picks B's TOP magnet.
    assert_eq!(*route(&n).last().unwrap(), [600.0 + rb[2] / 2.0, 700.0]);
    assert_eq!(n["fields"]["connector_end"], endpoint);
    assert_eq!(n["route_current"], true);
    v.undo().unwrap();
    assert_eq!(route(&node(&mut v, &c)), r, "one undo returns the node and the route");

    // Line type and every cap of the closed set draw.
    let straight = ok(&mut v, "board.connector.set", json!({"node": c, "connector_line_type": "STRAIGHT"}));
    assert_eq!(straight["route"].as_array().unwrap().len(), 2);
    for cap in ["NONE", "ARROW_EQUILATERAL", "ARROW_LINES", "TRIANGLE_FILLED", "CIRCLE_FILLED", "DIAMOND_FILLED", "ERD_ONE", "ERD_MANY", "ERD_ONE_OR_MORE", "ERD_ZERO_OR_ONE", "ERD_ZERO_OR_MORE", "ERD_EXACTLY_ONE"] {
        ok(&mut v, "board.connector.set", json!({"node": c, "connector_start_stroke_cap": cap, "connector_end_stroke_cap": cap}));
        assert_eq!(node(&mut v, &c)["fields"]["connector_end_stroke_cap"], cap);
    }

    // Deleting B breaks the connector: kept at its last route, reported, never deleted (STU-WB-108/142).
    let last = route(&node(&mut v, &c));
    let deleted = ok(&mut v, "board.node.delete", json!({"node": b}));
    assert_eq!(deleted["broken_connectors"], json!([c]));
    let n = node(&mut v, &c);
    assert_eq!(n["state"], "broken", "{n}");
    assert_eq!(route(&n), last);
    assert_eq!(n["issues"][0], json!({"code": "connector_endpoint_node_unresolvable", "severity": "warning", "node_id": c, "endpoint": "end", "endpoint_node_id": b}));
    assert_eq!(ok(&mut v, "board.read", json!({}))["issues"][0]["code"], "connector_endpoint_node_unresolvable");
    refused(&mut v, "board.connector.set", json!({"node": c, "end": {"endpoint_node_id": b, "magnet": "LEFT"}}), "connector_endpoint_node_unresolvable");
    v.undo().unwrap();
    assert_eq!(node(&mut v, &c)["state"], "attached");

    // A position endpoint is pinned to the canvas; a position-and-node endpoint follows its node.
    let p = ok(&mut v, "board.connector.create", json!({"start": {"position": {"x": 0, "y": 0}}, "end": {"position": {"x": 10, "y": 5}, "endpoint_node_id": a}}));
    let r = route(&node(&mut v, &text(&p, "node_id")));
    assert_eq!((r[0], *r.last().unwrap()), ([0.0, 0.0], [ra[0] + 10.0, ra[1] + 5.0]));
    refused(&mut v, "board.node.move", json!({"node": text(&p, "node_id"), "x": 1, "y": 1}), "board_node_not_movable");
}

#[test]
fn a_board_survives_save_and_reopen_and_exports_its_content_to_png() {
    let scratch = Scratch::new("board-save");
    let mut v = board_doc();
    let sticky = text(&ok(&mut v, "board.sticky.create", json!({"x": 100, "y": 100, "text": "Parking", "fill": [255, 235, 153]})), "node_id");
    let a = shape(&mut v, 500.0, 100.0, "Discover");
    let c = text(&ok(&mut v, "board.connector.create", json!({"start": {"endpoint_node_id": sticky, "magnet": "AUTO"}, "end": {"endpoint_node_id": a, "magnet": "AUTO"}})), "node_id");
    let before = ok(&mut v, "board.read", json!({}));
    let path = scratch.0.join("workshop.handshake");
    v.save(&path, false).unwrap();
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&path).unwrap();
    let after = ok(&mut w, "board.read", json!({}));
    assert_eq!(after["nodes"], before["nodes"], "board records, text and routes reopen unchanged");
    assert_eq!(after["whiteboard_mode"], true);
    // The reopened board takes further commands.
    ok(&mut w, "board.node.move", json!({"node": a, "x": 520, "y": 160}));
    assert_eq!(node(&mut w, &c)["route_current"], true);

    let png = scratch.0.join("plan-board.png");
    let report = ok(&mut w, "board.export_png", json!({"path": png.to_string_lossy()}));
    let b = report["bounds"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect::<Vec<_>>();
    let s = bounds(&node(&mut w, &sticky));
    assert!(b[0] <= s[0] - 32.0 && b[1] <= s[1] - 32.0, "content bounds with the margin: {report}");
    let img = raster::decode(&std::fs::read(&png).unwrap(), &raster::Limits::default(), &CancellationToken::default()).unwrap();
    assert_eq!((img.width, img.height), (report["width"].as_u64().unwrap() as u32, report["height"].as_u64().unwrap() as u32));
    let raster::Samples::U8(px) = &img.samples else { panic!("8-bit PNG") };
    let channels = px.len() / (img.width as usize * img.height as usize);
    // A point of the sticky card away from its text: the card colour.
    let (x, y) = ((s[0] + 8.0 - b[0]) as usize, (s[1] + s[3] - 8.0 - b[1]) as usize);
    let at = (y * img.width as usize + x) * channels;
    let rgb = [px[at], px[at + 1], px[at + 2]];
    assert!(rgb.iter().zip([255u8, 235, 153]).all(|(g, w)| g.abs_diff(w) <= 2), "sticky card pixel {rgb:?}");
    // The board bounds variant, and no silent overwrite.
    let err = call(&mut w, "board.export_png", json!({"path": png.to_string_lossy()})).unwrap_err();
    assert_eq!(err.0, "overwrite_required");
    let whole = ok(&mut w, "board.export_png", json!({"path": png.to_string_lossy(), "overwrite": true, "bounds": "board"}));
    assert_eq!((whole["width"].clone(), whole["height"].clone()), (json!(1920), json!(1080)));
}

/// Every native shape (with its interior lines) and every cap of the closed set renders in a board export: a
/// node whose drawing the renderer refused would refuse the whole frame.
#[test]
fn every_native_shape_and_every_cap_renders_in_a_board_export() {
    let scratch = Scratch::new("board-shapes");
    let mut v = board_doc();
    let tokens = ["rectangle", "rounded_rectangle", "ellipse", "diamond", "triangle", "parallelogram", "hexagon", "cylinder", "cloud", "document", "predefined_process", "terminator"];
    let mut nodes = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        let (x, y) = (40.0 + 300.0 * (i % 4) as f64, 40.0 + 200.0 * (i / 4) as f64);
        let made = ok(&mut v, "board.shape_with_text.create", json!({"x": x, "y": y, "shape_type": format!("studio_declared:{token}"), "text": token}));
        nodes.push(text(&made, "node_id"));
    }
    for token in ["ENG_DATABASE", "SUMMING_JUNCTION", "INTERNAL_STORAGE", "STAR"] {
        ok(&mut v, "board.shape_with_text.create", json!({"x": 40, "y": 700, "shape_type": format!("figma_plugin_api@1.141.0:{token}"), "text": token}));
    }
    let caps = ["NONE", "ARROW_EQUILATERAL", "ARROW_LINES", "TRIANGLE_FILLED", "CIRCLE_FILLED", "DIAMOND_FILLED", "ERD_ONE", "ERD_MANY", "ERD_ONE_OR_MORE", "ERD_ZERO_OR_ONE", "ERD_ZERO_OR_MORE", "ERD_EXACTLY_ONE"];
    for (i, cap) in caps.iter().enumerate() {
        let (a, b) = (&nodes[i], &nodes[(i + 1) % nodes.len()]);
        ok(&mut v, "board.connector.create", json!({"start": {"endpoint_node_id": a, "magnet": "AUTO"}, "end": {"endpoint_node_id": b, "magnet": "AUTO"}, "connector_start_stroke_cap": cap, "connector_end_stroke_cap": cap}));
    }
    // A shape keeps its interior lines through a move and a recolour.
    ok(&mut v, "board.node.move", json!({"node": nodes[7], "x": 980, "y": 260}));
    ok(&mut v, "board.node.set_fill", json!({"node": nodes[10], "fill": [200, 230, 200]}));
    let png = scratch.0.join("shapes.png");
    let report = ok(&mut v, "board.export_png", json!({"path": png.to_string_lossy()}));
    assert!(report["bytes"].as_u64().unwrap() > 0, "{report}");
    let read = ok(&mut v, "board.read", json!({}));
    assert_eq!(read["issues"], json!([]));
    // A shape's text is centred on its widest band: it fits (a diamond's top held no line).
    for n in read["nodes"].as_array().unwrap().iter().filter(|n| n["kind"] == "shape_with_text") {
        println!("TEXT_OVERSET {} {}", n["fields"]["shape_type"]["token"], n["text_overset"]);
        if ["rectangle", "ellipse", "diamond", "cylinder", "terminator", "hexagon"].contains(&n["text"].as_str().unwrap_or_default()) {
            assert_eq!(n["text_overset"], 0, "{}", n["text"]);
        }
    }
}

#[test]
fn hostile_board_commands_are_typed_refusals_that_change_nothing() {
    let mut v = board_doc();
    let a = shape(&mut v, 100.0, 100.0, "A");
    let c = text(&ok(&mut v, "board.connector.create", json!({"start": {"position": {"x": 0, "y": 0}}, "end": {"endpoint_node_id": a, "magnet": "TOP"}})), "node_id");
    let before = snapshot(&v);
    let long = "x".repeat(5000);
    let cases = [
        ("board.connector.create", json!({"start": {"endpoint_node_id": a, "magnet": "NORTH"}, "end": {"position": {"x": 1, "y": 1}}}), "connector_endpoint_magnet_unknown"),
        ("board.connector.create", json!({"start": {"endpoint_node_id": "SLYR-019abcde-0000-7000-8000-00000000dead", "magnet": "TOP"}, "end": {"position": {"x": 1, "y": 1}}}), "connector_endpoint_node_unresolvable"),
        ("board.connector.create", json!({"start": {"endpoint_node_id": c, "magnet": "TOP"}, "end": {"position": {"x": 1, "y": 1}}}), "connector_endpoint_node_not_attachable"),
        ("board.connector.create", json!({"start": {"position": {"x": 0, "y": 0}}, "end": {"position": {"x": 1, "y": 1}}, "connector_line_type": "CURVED"}), "connector_line_type_not_available"),
        ("board.connector.create", json!({"start": {"position": {"x": 0}}, "end": {"position": {"x": 1, "y": 1}}}), "parameter_invalid"),
        ("board.connector.create", json!({"start": {"position": {"x": 1e300, "y": 0}}, "end": {"position": {"x": 1, "y": 1}}}), "board_position_invalid"),
        ("board.connector.set", json!({"node": c, "start": {"endpoint_node_id": c, "magnet": "TOP"}}), "connector_endpoint_self"),
        ("board.shape_with_text.create", json!({"x": 0, "y": 0, "shape_type": "studio_declared:blob", "text": "A"}), "shape_type_unknown"),
        ("board.shape_with_text.create", json!({"x": 0, "y": 0, "shape_type": "figma_plugin_api@9.9.9:SQUARE", "text": "A"}), "shape_type_unknown"),
        ("board.shape_with_text.create", json!({"x": 0, "y": 0, "shape_type": "figma_plugin_api@1.141.0:SHIELD", "text": "A"}), "shape_type_geometry_not_available"),
        ("board.shape_with_text.create", json!({"x": 0, "y": 0, "shape_type": "studio_declared:rectangle", "text": "A", "width": 0}), "board_size_invalid"),
        ("board.sticky.create", json!({"x": 0, "y": 0, "text": "two\nlines"}), "board_text_invalid"),
        ("board.sticky.create", json!({"x": 0, "y": 0, "text": long}), "board_text_invalid"),
        ("board.sticky.create", json!({"x": "NaN", "y": 0, "text": "A"}), "parameter_invalid"),
        ("board.sticky.create", json!({"x": 1e9, "y": 0, "text": "A"}), "board_position_invalid"),
        ("board.sticky.create", json!({"x": 0, "y": 0, "text": "A", "author_name": "n".repeat(300)}), "author_name_invalid"),
        ("board.sticky.create", json!({"board": "SART-019abcde-0000-7000-8000-00000000beef", "x": 0, "y": 0, "text": "A"}), "board_unknown"),
        ("board.sticky.create", json!({"x": 0, "y": 0, "text": "A", "colour": [1, 2, 3]}), "parameter_unknown"),
        ("board.node.read", json!({"node": "SLYR-019abcde-0000-7000-8000-00000000dead"}), "board_node_unknown"),
        ("board.node.move", json!({"node": c, "x": 1, "y": 1}), "board_node_not_movable"),
        ("board.node.set_text", json!({"node": c, "text": "label"}), "board_node_kind_mismatch"),
        ("board.sticky.set", json!({"node": a, "is_wide_width": true}), "board_node_kind_mismatch"),
        ("board.section.set", json!({"node": a, "name": "S"}), "board_node_kind_mismatch"),
        ("board.export_png", json!({"path": ""}), "export_path_missing"),
        ("board.connector.create", json!({"start": {"endpoint_node_id": a, "magnet": "TOP"}, "end": {"endpoint_node_id": a, "magnet": "TOP"}}), "board_geometry_invalid"),
    ];
    for (id, params, code) in cases {
        refused(&mut v, id, params.clone(), code);
        assert_eq!(snapshot(&v), before, "{id} {params} changed the document");
    }
    // A layer that is not a board node.
    let doc = v.run().snapshot().unwrap().document().clone();
    assert!(doc.layers.iter().all(|l| l.kind != "raster"));
    let id = v.draw_shape(hsk_studio_harness::view::vector::ShapeKind::Rectangle, [0.0, 0.0], [10.0, 10.0], false).unwrap();
    refused(&mut v, "board.node.read", json!({"node": id}), "not_a_board_node");
}

/// The in-process JSONL loop of `cli/drive.rs` as a live session: one line in, its one reply out, so a later
/// line can name what an earlier reply returned (the process tests' `Live`, without a spawned binary).
struct ChanReader {
    rx: mpsc::Receiver<Vec<u8>>,
    buf: Vec<u8>,
    pos: usize,
}

impl std::io::Read for ChanReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.pos == self.buf.len() {
            match self.rx.recv() {
                Ok(bytes) => {
                    self.buf = bytes;
                    self.pos = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

struct ChanWriter(mpsc::Sender<Vec<u8>>);

impl std::io::Write for ChanWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let _ = self.0.send(bytes.to_vec());
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Live {
    tx: Option<mpsc::Sender<Vec<u8>>>,
    rx: mpsc::Receiver<Vec<u8>>,
    pending: Vec<u8>,
}

impl Live {
    /// One reply within 30 s (review F10: a hang names its step, well inside nextest's 120 s cap).
    fn read(&mut self, step: &str) -> Value {
        loop {
            if let Some(at) = self.pending.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.pending.drain(..=at).collect();
                return serde_json::from_slice(&line).unwrap_or_else(|e| panic!("{step}: reply is not JSON ({e}): {}", String::from_utf8_lossy(&line)));
            }
            let more = self.rx.recv_timeout(Duration::from_secs(30)).unwrap_or_else(|_| panic!("{step}: no reply within 30 s"));
            self.pending.extend(more);
        }
    }

    fn call(&mut self, id: &str, parameters: Value) -> Value {
        let line = json!({"command": "command", "command_id": id, "parameters": parameters}).to_string();
        let started = std::time::Instant::now();
        self.tx.as_ref().expect("open session").send(format!("{line}\n").into_bytes()).unwrap();
        let reply = self.read(id);
        println!("REPLY {id} {:?}", started.elapsed());
        reply
    }

    fn ok(&mut self, id: &str, parameters: Value) -> Value {
        let reply = self.call(id, parameters);
        assert_eq!(reply["ok"], true, "{id}: {reply}");
        reply
    }
}

/// Review F3 (common rule UNDO NAMES): a mutating row's undo name is its catalog id, and its own record names
/// that id with the caller's actor (the reply's `attribution` is the session's and cannot fail).
fn assert_committed_as(reply: &Value, id: &str, actor: &str) {
    assert_eq!(reply["detail"]["undo_name"], id, "the undo name is the catalog id: {reply}");
    let record = reply["state"]["ops"].as_array().and_then(|o| o.last()).cloned().unwrap_or_default();
    assert_eq!((record["op"].as_str(), record["command_id"].as_str()), (Some(id), Some(id)), "{record}");
    assert_eq!(record["actor"]["account_id"], actor, "{record}");
}

/// The workshop steps of `workflows/scenario-board-slides-web.json` (1, 6, 8, 10, 11, 12, 26) as a model runs
/// them: `new --profile board`, then the JSONL loop of `cli/drive.rs` with only what needs the wire (one create
/// per kind, a connector, a move, save, export; review F10), every parameter as text and every edit named by
/// its catalog id with the caller's actor (review F3).
#[test]
fn the_workshop_board_steps_run_through_the_jsonl_loop() {
    let scratch = Scratch::new("board-jsonl");
    let package = scratch.0.join("start.handshake").to_string_lossy().into_owned();
    let who = ["--actor", "board-lane", "--session", "board-session"];
    let mut args: Vec<String> = ["new", package.as_str(), "--profile", "board", "--size", "640x480"].iter().map(|s| (*s).to_string()).collect();
    args.extend(who.iter().map(|s| (*s).to_string()));
    let out = hsk_studio_harness::cli::run(&args);
    assert_eq!(out.exit, 0, "{}", out.stdout);
    let saved = scratch.0.join("workshop.handshake").to_string_lossy().into_owned();
    let png = scratch.0.join("plan.png").to_string_lossy().into_owned();
    let args: Vec<String> = ["command", package.as_str(), "--stdin"].iter().chain(who.iter()).map(|s| (*s).to_string()).collect();
    let started = std::time::Instant::now();
    let exit = std::thread::scope(|scope| {
        let (in_tx, in_rx) = mpsc::channel::<Vec<u8>>();
        let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>();
        let args = &args;
        let handle = scope.spawn(move || {
            let mut input = std::io::BufReader::new(ChanReader { rx: in_rx, buf: Vec::new(), pos: 0 });
            hsk_studio_harness::cli::run_streams(args, &mut input, &mut ChanWriter(out_tx))
        });
        let mut s = Live { tx: Some(in_tx), rx: out_rx, pending: Vec::new() };
        assert_eq!(s.read("open")["ok"], true, "the session opens");
        let section = s.ok("board.section.create", json!({"x": 0, "y": 0, "width": 600, "height": 440, "name": "Goals"}));
        assert_committed_as(&section, "board.section.create", "board-lane");
        let sticky = s.ok("board.sticky.create", json!({"x": 20, "y": 40, "text": "Night-time parking is the blocker", "author_visible": true, "is_wide_width": false}));
        assert_committed_as(&sticky, "board.sticky.create", "board-lane");
        let a = s.ok("board.shape_with_text.create", json!({"x": 300, "y": 40, "shape_type": "studio_declared:rectangle", "text": "Discover"}));
        assert_committed_as(&a, "board.shape_with_text.create", "board-lane");
        let shape_a = a["detail"]["result"]["node_id"].as_str().unwrap().to_owned();
        let shape_b = sticky["detail"]["result"]["node_id"].as_str().unwrap().to_owned();
        let c = s.ok("board.connector.create", json!({"start": {"endpoint_node_id": shape_a, "magnet": "BOTTOM"}, "end": {"endpoint_node_id": shape_b, "magnet": "AUTO"}, "connector_end_stroke_cap": "ARROW_EQUILATERAL"}));
        assert_committed_as(&c, "board.connector.create", "board-lane");
        let moved = s.ok("board.node.move", json!({"node": shape_a, "x": 320, "y": 300}));
        assert_committed_as(&moved, "board.node.move", "board-lane");
        assert_eq!(moved["detail"]["result"]["rerouted"].as_array().unwrap().len(), 1, "{moved}");
        s.ok("document.save", json!({"path": saved}));
        s.ok("board.export_png", json!({"path": png}));
        drop(s.tx.take());
        handle.join().unwrap()
    });
    println!("JSONL steps in {:?}", started.elapsed());
    assert_eq!(exit, 0);
    assert!(std::fs::metadata(&png).unwrap().len() > 0);
    // The saved document reopens in whiteboard mode with its nodes.
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(std::path::Path::new(&saved)).unwrap();
    let again = ok(&mut w, "board.read", json!({}));
    assert_eq!(again["nodes"].as_array().unwrap().len(), 4);
    assert_eq!(again["whiteboard_mode"], true);
}
