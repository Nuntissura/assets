//! Lane SMB-FIX-LAYER-RECORD-PRUNE-1 (feature `viewer`; common lead ruling 5: a deleted object's records are removed
//! in the same undo step as the object, and undo restores both). After any structure step, in that same step, the
//! records keyed to the layers the step removed leave the `raster_records`, `vector` and `text` entries: board records
//! and their stories, clips, masks and attachments, live filter stacks, live shapes, design node entries with the
//! layer's own appearance style, and the path records nothing references any more. One test per path family (a knife
//! cut, a pathfinder, a design instance sync that deletes its copies, a board node through Layers > Delete) and the
//! design records of a plain delete: the records leave in the step, undo restores the layer and its records together,
//! redo removes both, a layer made afterwards that takes the id inherits nothing, and save and reopen agree.
//! Oracles: the session's document, the stored entries (raw JSON) and the history. Headless (no window).
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, solid_psd};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::vector::arrange::ReferencePoint;
use hsk_studio_harness::view::vector::{PaintSpec, ShapeKind, TransformOp, VectorTool};
use hsk_studio_harness_core::document_new::{DocumentProfile, NewDocument};
use hsk_studio_nib::pathfinder::PathfinderOp;
use serde_json::{Value, json};
use std::path::Path;

/// The entries a structure step prunes (the design binding states and the layout records are not among them).
const TOKENS: [&str; 3] = ["raster_records", "vector", "text"];

fn paper(scratch: &Scratch) -> Viewer {
    let source = scratch.0.join("paper.psd");
    std::fs::write(&source, solid_psd(96, 64, "paper", [255, 255, 255])).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap();
    v.set_vector_tool(VectorTool { fill_on: true, fill: [200, 0, 0], stroke_on: false, ..VectorTool::default() });
    v
}

fn design_doc() -> Viewer {
    let mut v = Viewer::new(Prefer::Cpu);
    v.new_document(&NewDocument::new(DocumentProfile::Design, None), None).unwrap();
    v.set_vector_tool(VectorTool { fill_on: true, fill: [200, 0, 0], stroke_on: false, ..VectorTool::default() });
    v
}

fn board_doc() -> Viewer {
    let mut v = Viewer::new(Prefer::Cpu);
    v.new_document(&NewDocument::new(DocumentProfile::Board, None), None).unwrap();
    v
}

fn reopen(v: &mut Viewer, scratch: &Scratch, name: &str) -> Viewer {
    let path = scratch.0.join(name);
    v.save(&path, false).unwrap();
    open(&path)
}

fn open(path: &Path) -> Viewer {
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    w
}

fn ok(v: &mut Viewer, id: &str, params: Value) -> Value {
    let Value::Object(map) = params else { panic!("parameters are an object") };
    v.run_command(id, &map).unwrap_or_else(|r| panic!("{id}: {} {}", r.code, r.detail))
}

fn text(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or_else(|| panic!("no {key} in {v}")).to_string()
}

fn entry(v: &Viewer, token: &str) -> Value {
    v.run().document_entry(token).map_or(Value::Null, |b| serde_json::from_slice(b).unwrap())
}

/// JSON pointers of every string (or key) in the pruned entries equal to `id`.
fn naming(v: &Viewer, id: &str) -> Vec<String> {
    fn walk(v: &Value, at: &str, id: &str, out: &mut Vec<String>) {
        match v {
            Value::String(s) if s == id => out.push(at.to_string()),
            Value::Array(a) => a.iter().enumerate().for_each(|(i, x)| walk(x, &format!("{at}/{i}"), id, out)),
            Value::Object(m) => {
                for (k, x) in m {
                    if k == id {
                        out.push(format!("{at}/{k} (key)"));
                    }
                    walk(x, &format!("{at}/{k}"), id, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for t in TOKENS {
        walk(&entry(v, t), t, id, &mut out);
    }
    out
}

/// The document's layers (by id: an undone delete puts a layer back at the end of the list) and the pruned entries'
/// bytes: what an undo of a step whose entries are all in its history step must restore byte for byte.
fn state(v: &Viewer) -> (Vec<String>, Vec<Vec<u8>>) {
    (layers(v), TOKENS.iter().map(|t| v.run().document_entry(t).map(<[u8]>::to_vec).unwrap_or_default()).collect())
}

fn layers(v: &Viewer) -> Vec<String> {
    let mut out: Vec<String> = v.run().snapshot().unwrap().document().layers.iter().map(|l| serde_json::to_string(l).unwrap()).collect();
    out.sort();
    out
}

fn undo_len(v: &Viewer) -> usize {
    v.run().history()["undo"].as_array().map_or(0, Vec::len)
}

fn has_layer(v: &Viewer, id: &str) -> bool {
    v.run().snapshot().unwrap().document().layers.iter().any(|l| l.layer_id == id)
}

/// The path ids `layer`'s payload names.
fn path_ids(v: &Viewer, layer: &str) -> Vec<String> {
    let doc = v.run().snapshot().unwrap().document();
    let l = doc.layers.iter().find(|l| l.layer_id == layer).unwrap_or_else(|| panic!("no layer {layer}"));
    serde_json::to_value(&l.payload).unwrap()["path_ids"].as_array().map(|a| a.iter().map(|p| p.as_str().unwrap().to_string()).collect()).unwrap_or_default()
}

/// The path records the vector entry holds, by id.
fn stored_paths(v: &Viewer) -> Vec<String> {
    entry(v, "vector")["paths"].as_array().map(|a| a.iter().map(|p| p["path_id"].as_str().unwrap().to_string()).collect()).unwrap_or_default()
}

/// The paint style ids of the vector entry's design records.
fn styles(v: &Viewer) -> Vec<String> {
    entry(v, "vector")["design"]["paint_styles"].as_array().map(|a| a.iter().map(|s| s["style_id"].as_str().unwrap().to_string()).collect()).unwrap_or_default()
}

/// The appearance style id a layer's own style takes (`SSTY-<layer uuid>`).
fn own_style(layer: &str) -> String {
    format!("SSTY-{}", layer.strip_prefix("SLYR-").unwrap())
}

fn children(v: &Viewer, layer: &str) -> Vec<String> {
    let doc = v.run().snapshot().unwrap().document();
    doc.layers.iter().filter(|l| serde_json::to_value(&l.parent).unwrap()["node_id"] == layer).map(|l| l.layer_id.clone()).collect()
}

fn layer_name(v: &Viewer, id: &str) -> String {
    let doc = v.run().snapshot().unwrap().document();
    doc.layers.iter().find(|l| l.layer_id == id).map(|l| l.name.clone()).unwrap_or_default()
}

/// A live filter stack on `layer` (one step repeat copy).
fn live_filter(v: &mut Viewer, layer: &str) {
    let params = json!([{"type": "Int", "key": "copies", "value": 1}, {"type": "Real", "key": "move_horizontal", "value": 4.0}]);
    ok(v, "vector.live_filter_apply", json!({"layer": layer, "filter_kind": "hsk.studio.vector_effect.step_repeat", "params": params}));
}

fn live_filters(v: &mut Viewer, layer: &str) -> Value {
    ok(v, "vector.live_filter_read", json!({"layer": layer}))["filters"].clone()
}

/// Clips and masks whose source is `layer`, and attachments of `layer`.
fn clip_records(v: &Viewer, layer: &str) -> usize {
    let r = entry(v, "vector")["vector_records"].clone();
    let count = |key: &str, field: &str| r[key].as_array().map_or(0, |a| a.iter().filter(|x| x[field] == layer).count());
    count("clips", "source_layer_id") + count("masks", "source_layer_id") + count("attachments", "layer_id")
}

/// Vector piece replacement (knife): the cut layer's live filter stack, live shape, clip source record and path records
/// leave in the cut's own step; the target the clip was attached to loses the attachment with it.
#[test]
fn a_knife_cut_takes_the_cut_layers_records_along_in_its_own_step() {
    let scratch = Scratch::new("prune-knife");
    let mut v = paper(&scratch);
    let target = v.draw_shape(ShapeKind::Rectangle, [60.0, 10.0], [80.0, 40.0], false).unwrap();
    let cut = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [50.0, 40.0], false).unwrap();
    live_filter(&mut v, &cut);
    v.make_clip(&target, &cut, false).unwrap();
    v.select(None);
    let paths = path_ids(&v, &cut);
    let named = naming(&v, &cut);
    assert!(!named.is_empty() && clip_records(&v, &target) == 1, "set-up: records name the layer {cut}");
    let (before, steps) = (layers(&v), undo_len(&v));
    let pieces = v.knife_cut(&[[30.0, 0.0], [30.0, 60.0]]).unwrap();
    assert_eq!((pieces.len(), undo_len(&v)), (2, steps + 1), "the cut is one step");
    assert_eq!(naming(&v, &cut), Vec::<String>::new(), "the cut layer's records leave in the cut's own step");
    assert_eq!(clip_records(&v, &target), 0, "the attachment of the clip whose source left goes with it");
    assert!(paths.iter().all(|p| !stored_paths(&v).contains(p)), "the cut layer's path records, named nowhere else, leave too");
    v.undo().unwrap();
    assert_eq!((layers(&v), naming(&v, &cut), clip_records(&v, &target)), (before, named, 1), "undo restores the layer and its records together");
    assert!(paths.iter().all(|p| stored_paths(&v).contains(p)), "undo restores the path records");
    v.redo().unwrap();
    assert_eq!(naming(&v, &cut), Vec::<String>::new(), "redo removes them again");
    let mut w = reopen(&mut v, &scratch, "knife.handshake");
    assert_eq!(naming(&w, &cut), Vec::<String>::new(), "the saved entries hold no record of the cut layer");
    // A layer made afterwards that takes the id inherits nothing (ids are reused until SMB-LAYER-ID-STABILITY-1).
    for p in pieces.iter().rev() {
        v.delete_layer(p).unwrap();
        w.delete_layer(p).unwrap();
    }
    for x in [&mut v, &mut w] {
        let fresh = x.draw_shape(ShapeKind::Rectangle, [10.0, 50.0], [20.0, 60.0], false).unwrap();
        assert_eq!(fresh, cut, "the new layer takes the cut layer's id");
        assert_eq!(live_filters(x, &fresh), json!([]), "it inherits no live filter stack");
        assert_eq!(clip_records(x, &fresh), 0, "nor the clip source record");
    }
}

/// Pathfinder: an operand's live filter stack, live shape, design node and own appearance style and its path records
/// leave in the pathfinder's own step.
#[test]
fn a_pathfinder_takes_the_operands_records_along_in_its_own_step() {
    let scratch = Scratch::new("prune-pathfinder");
    let mut v = paper(&scratch);
    let _other = v.draw_shape(ShapeKind::Rectangle, [70.0, 10.0], [90.0, 30.0], false).unwrap();
    let a = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [30.0, 30.0], false).unwrap();
    let b = v.draw_shape(ShapeKind::Rectangle, [20.0, 20.0], [40.0, 40.0], false).unwrap();
    live_filter(&mut v, &b);
    v.set_fill(&b, Some(&PaintSpec::Solid([0, 128, 0]))).unwrap();
    assert!(styles(&v).contains(&own_style(&b)), "set-up: {b} has its own appearance style");
    let paths: Vec<String> = path_ids(&v, &a).into_iter().chain(path_ids(&v, &b)).collect();
    let named = naming(&v, &b);
    let (before, steps) = (layers(&v), undo_len(&v));
    let pieces = v.pathfinder_layers(PathfinderOp::Union, &[a.clone(), b.clone()]).unwrap();
    assert_eq!((pieces.len(), undo_len(&v)), (1, steps + 1), "the pathfinder is one step");
    assert_eq!(naming(&v, &b), Vec::<String>::new(), "the operand's records leave in the pathfinder's own step");
    assert_eq!(naming(&v, &a), Vec::<String>::new(), "so do the other operand's");
    assert!(!styles(&v).contains(&own_style(&b)), "the operand's own appearance style, referenced by nothing, leaves");
    assert!(paths.iter().all(|p| !stored_paths(&v).contains(p)), "the operands' path records leave");
    v.undo().unwrap();
    assert_eq!((layers(&v), naming(&v, &b)), (before, named), "undo restores the operands and their records together");
    assert!(styles(&v).contains(&own_style(&b)) && paths.iter().all(|p| stored_paths(&v).contains(p)));
    v.redo().unwrap();
    assert_eq!(naming(&v, &b), Vec::<String>::new(), "redo removes them again");
    let mut w = reopen(&mut v, &scratch, "pathfinder.handshake");
    assert_eq!(naming(&w, &b), Vec::<String>::new(), "the saved entries hold no record of the operand");
    for x in [&mut v, &mut w] {
        let fresh = x.draw_shape(ShapeKind::Rectangle, [10.0, 45.0], [20.0, 55.0], false).unwrap();
        assert_eq!(fresh, b, "the new layer takes the operand's id");
        assert_eq!(live_filters(x, &fresh), json!([]), "it inherits no live filter stack");
        let nodes = entry(x, "vector")["design"]["nodes"].clone();
        assert!(nodes.as_array().is_none_or(|n| n.iter().all(|n| n["node"]["node_id"] != fresh.as_str())), "nor a design node: {nodes}");
    }
}

/// Moves the definition layer `bg` and syncs the component's instances (one redraw step).
fn edit_and_sync(v: &mut Viewer, bg: &str, component: &str) -> Value {
    v.transform_layers(&[bg.to_string()], TransformOp::Move { dx: 5.0, dy: 0.0 }, ReferencePoint::Centre).unwrap();
    let Value::Object(map) = json!({"component": component}) else { unreachable!() };
    v.run_command("component.sync_instances", &map).unwrap_or_else(|r| panic!("sync: {} {} (vector entry: {:?})", r.code, r.detail, v.vector_entry().err()))
}

/// Design copy delete: a sync redraws an instance, deleting its copies; a clip attached to a deleted copy leaves in
/// the sync's own step, and the copy that a later sync mints under the same id inherits nothing (a redraw mints from
/// the document's layer count, so every other sync takes the same copy ids).
#[test]
fn a_sync_that_deletes_instance_copies_takes_their_records_along() {
    let scratch = Scratch::new("prune-design-copies");
    let mut v = design_doc();
    v.select(None);
    let bg = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [70.0, 30.0], false).unwrap();
    let icon = v.draw_shape(ShapeKind::Ellipse, [15.0, 15.0], [25.0, 25.0], false).unwrap();
    let component = text(&ok(&mut v, "component.create_from_selection", json!({"layers": [bg, icon], "name": "Button"})), "component_id");
    v.select(None);
    let frame = text(&ok(&mut v, "component.create_instance", json!({"component": component, "x": 200, "y": 10})), "layer_id");
    edit_and_sync(&mut v, &bg, &component);
    v.select(None);
    let source = v.draw_shape(ShapeKind::Rectangle, [400.0, 10.0], [420.0, 30.0], false).unwrap();
    let copy = children(&v, &frame).into_iter().find(|k| layer_name(&v, k) == "Ellipse").expect("the icon's copy");
    v.make_clip(&copy, &source, false).unwrap();
    assert_eq!(clip_records(&v, &copy), 1, "set-up: the copy carries an attachment");
    v.transform_layers(std::slice::from_ref(&bg), TransformOp::Move { dx: 5.0, dy: 0.0 }, ReferencePoint::Centre).unwrap();
    let (before, steps) = (state(&v), undo_len(&v));
    let Value::Object(map) = json!({"component": component}) else { unreachable!() };
    let synced = v.run_command("component.sync_instances", &map).map_err(|r| format!("{} {}", r.code, r.detail));
    assert!(synced.is_ok(), "a sync that deletes a copy carrying an attachment commits, the attachment leaving with the copy: {synced:?}");
    let synced = synced.unwrap();
    assert_eq!((synced["redrawn"].as_array().map_or(0, Vec::len), undo_len(&v)), (1, steps + 1), "the sync is one step: {synced}");
    assert!(!has_layer(&v, &copy), "the sync deletes the copy");
    assert_eq!(naming(&v, &copy), Vec::<String>::new(), "the deleted copy's records leave in the sync's own step");
    v.undo().unwrap();
    assert_eq!(state(&v), before, "undo restores the copies and their records together");
    v.redo().unwrap();
    assert_eq!(naming(&v, &copy), Vec::<String>::new(), "redo removes them again");
    let w = reopen(&mut v, &scratch, "copies.handshake");
    assert_eq!(naming(&w, &copy), Vec::<String>::new(), "the saved entries hold no record of the deleted copy");
    // The next sync mints the deleted copies' ids again: the new copy under the old id carries no attachment.
    edit_and_sync(&mut v, &bg, &component);
    assert!(children(&v, &frame).contains(&copy), "the next sync reuses the deleted copies' ids");
    assert_eq!(clip_records(&v, &copy), 0, "the copy under the reused id inherits no attachment");
}

/// Board node through Layers > Delete: the node's story leaves the text entry and its path records leave the vector
/// entry in the delete's own step (its board record and binding already did).
#[test]
fn a_board_node_delete_takes_its_story_and_paths_along() {
    let scratch = Scratch::new("prune-board");
    let mut v = board_doc();
    let node = text(&ok(&mut v, "board.sticky.create", json!({"x": 0, "y": 0, "text": "A"})), "node_id");
    let story = entry(&v, "vector")["vector_records"]["area_texts"].as_array().and_then(|a| a.iter().find(|x| x["container_layer_id"] == node.as_str())).map(|x| text(x, "text_id")).expect("the node's story");
    let stories = |v: &Viewer| -> Vec<String> { entry(v, "text")["stories"].as_array().map(|a| a.iter().map(|s| s["story_id"].as_str().unwrap().to_string()).collect()).unwrap_or_default() };
    assert!(stories(&v).contains(&story), "set-up: the text entry holds the story");
    let paths = path_ids(&v, &node);
    let (before, steps) = (state(&v), undo_len(&v));
    v.delete_layer(&node).unwrap();
    assert_eq!(undo_len(&v), steps + 1, "the delete is one step");
    assert!(!stories(&v).contains(&story), "the node's story leaves in the delete's own step");
    assert!(paths.iter().all(|p| !stored_paths(&v).contains(p)), "so do its path records");
    assert_eq!(naming(&v, &node), Vec::<String>::new());
    v.undo().unwrap();
    assert_eq!(state(&v), before, "undo restores the node with its story and paths");
    v.redo().unwrap();
    assert!(!stories(&v).contains(&story), "redo removes them again");
    let mut w = reopen(&mut v, &scratch, "board.handshake");
    assert!(!stories(&w).contains(&story) && paths.iter().all(|p| !stored_paths(&w).contains(p)), "the saved entries hold neither");
    for x in [&mut v, &mut w] {
        let fresh = x.draw_shape(ShapeKind::Rectangle, [0.0, 0.0], [50.0, 50.0], false).unwrap();
        assert_eq!(fresh, node, "the new layer takes the node's id");
        let r = entry(x, "vector")["vector_records"].clone();
        let bound = ["board_nodes", "area_texts"].iter().any(|k| r[*k].as_array().is_some_and(|a| a.iter().any(|n| n["layer_id"] == fresh.as_str() || n["container_layer_id"] == fresh.as_str())));
        assert!(!bound, "it inherits no board record or text binding: {r}");
    }
}

/// Lead ruling Q3 (2026-11-13): a removed layer's design node entry leaves with it in the step, and so does its own
/// appearance style when nothing references it; a style a present node still references stays, and a published
/// graphic style stays as the document palette's definition.
#[test]
fn a_deleted_layers_design_node_and_unreferenced_own_style_leave_and_a_shared_style_stays() {
    let scratch = Scratch::new("prune-styles");
    let mut v = paper(&scratch);
    let c = v.draw_shape(ShapeKind::Rectangle, [70.0, 10.0], [90.0, 30.0], false).unwrap();
    let a = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [30.0, 30.0], false).unwrap();
    let b = v.draw_shape(ShapeKind::Rectangle, [40.0, 10.0], [60.0, 30.0], false).unwrap();
    v.set_fill(&a, Some(&PaintSpec::Solid([0, 128, 0]))).unwrap();
    let shared = v.add_graphic_style("Shared", &c).unwrap();
    v.apply_graphic_style(&b, &shared).unwrap();
    v.apply_graphic_style(&c, &shared).unwrap();
    let node_of = |v: &Viewer, id: &str| entry(v, "vector")["design"]["nodes"].as_array().is_some_and(|n| n.iter().any(|n| n["node"]["node_id"] == id));
    assert!(node_of(&v, &a) && node_of(&v, &b) && styles(&v).contains(&own_style(&a)), "set-up: nodes and the own style");
    let (before, steps) = (state(&v), undo_len(&v));
    v.delete_layer(&a).unwrap();
    assert_eq!(undo_len(&v), steps + 1, "the delete is one step");
    assert!(!node_of(&v, &a), "the deleted layer's design node leaves in the delete's own step");
    assert!(!styles(&v).contains(&own_style(&a)), "its own appearance style, referenced by nothing, leaves too");
    v.delete_layer(&b).unwrap();
    assert!(!node_of(&v, &b), "the second delete drops its node");
    assert!(styles(&v).contains(&shared), "the graphic style a present node still references stays");
    v.delete_layer(&c).unwrap();
    assert!(styles(&v).contains(&shared), "a published graphic style stays as the palette's definition");
    let w = reopen(&mut v, &scratch, "styles.handshake");
    assert!(!node_of(&w, &a) && !styles(&w).contains(&own_style(&a)), "save and reopen agree");
    for _ in 0..3 {
        v.undo().unwrap();
    }
    assert_eq!(state(&v), before, "undo restores the layers, their nodes and the style together");
}

/// Lead ruling Q2 (2026-11-13): Layers > Delete of a component's layer drops its component records in the delete's
/// own step through the central prune (a document without layout records); undo restores them, redo drops them.
#[test]
fn a_deleted_component_layer_drops_its_component_records_in_the_step() {
    let mut v = design_doc();
    let bg = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [70.0, 30.0], false).unwrap();
    let icon = v.draw_shape(ShapeKind::Ellipse, [15.0, 15.0], [25.0, 25.0], false).unwrap();
    let made = ok(&mut v, "component.create_from_selection", json!({"layers": [bg, icon], "name": "Button"}));
    let group = text(&made, "layer_id");
    v.select(None);
    assert!(v.run().document_entry("layout_records").is_none(), "set-up: no layout records");
    let components = |v: &Viewer| entry(v, "vector")["design"]["components"].as_array().map_or(0, Vec::len);
    assert_eq!(components(&v), 1);
    let (before, steps) = (state(&v), undo_len(&v));
    v.delete_layer(&group).unwrap();
    assert_eq!((undo_len(&v), components(&v)), (steps + 1, 0), "the component record leaves in the delete's own step");
    assert_eq!(naming(&v, &group), Vec::<String>::new());
    v.undo().unwrap();
    assert_eq!(state(&v), before, "undo restores the layers and the component record together");
    v.redo().unwrap();
    assert_eq!(components(&v), 0, "redo drops it again");
}
