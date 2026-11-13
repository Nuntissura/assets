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
use hsk_studio_accord::CancellationToken;
use hsk_studio_harness::view::text::TextEntry;
use hsk_studio_harness::view::vector::{LiveTextKind, PaintSpec, ShapeKind, TransformOp, VectorTool};
use hsk_studio_harness::{Harness, Selection, asset_local_test_context};
use hsk_studio_interop_raster as raster;
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

/// Vector piece replacement (knife): the cut layer's live filter stack, live shape, clip attachment and path records
/// leave in the cut's own step; the clip's source keeps its record. (Lead ruling Q2 2026-11-13: a knife whose cut layer
/// is a clip or mask source refuses typed, `a_replacement_of_a_clip_source_or_text_carrier_refuses_typed`, so this test
/// cuts the clipped layer instead of the clip source.)
#[test]
fn a_knife_cut_takes_the_cut_layers_records_along_in_its_own_step() {
    let scratch = Scratch::new("prune-knife");
    let mut v = paper(&scratch);
    let target = v.draw_shape(ShapeKind::Rectangle, [60.0, 10.0], [80.0, 40.0], false).unwrap();
    let cut = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [50.0, 40.0], false).unwrap();
    live_filter(&mut v, &cut);
    v.make_clip(&cut, &target, false).unwrap();
    v.select(None);
    let paths = path_ids(&v, &cut);
    let named = naming(&v, &cut);
    assert!(!named.is_empty() && clip_records(&v, &target) == 1, "set-up: records name the layer {cut}");
    let (before, steps) = (layers(&v), undo_len(&v));
    let pieces = v.knife_cut(&[[30.0, 0.0], [30.0, 60.0]]).unwrap();
    assert_eq!((pieces.len(), undo_len(&v)), (2, steps + 1), "the cut is one step");
    assert_eq!(naming(&v, &cut), Vec::<String>::new(), "the cut layer's records leave in the cut's own step");
    assert_eq!(clip_records(&v, &target), 1, "the clip's source keeps its own record");
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
        assert_eq!(clip_records(x, &fresh), 0, "nor the clip attachment");
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

// ---- review of 95460d11 (FIX_FIRST, lead rulings 2026-11-13) --------------------------------------------------

/// Rewrites the session document entry `token` of the package `src` into `out` through a raw harness run (another
/// writer): `edit` gets the entry as JSON (`None` without one) and returns its new bytes.
fn rewrite_entry(src: &Path, out: &Path, token: &str, edit: impl FnOnce(Option<Value>) -> Vec<u8>) {
    let h = Harness::new(Selection { providers: vec!["chronicle.patch".to_string()], ..Selection::default() }).unwrap();
    let cancel = CancellationToken::default();
    let mut run = h.run(asset_local_test_context(), &cancel);
    assert!(run.open_path(src), "open {}", src.display());
    let held = run.document_entry(token).map(|b| serde_json::from_slice(b).unwrap());
    run.set_document_entry(token, Some(edit(held))).unwrap();
    assert!(run.save_to(out), "save {}", out.display());
}

fn ids_of(v: &Value, list: &str, key: &str) -> Vec<String> {
    v.pointer(list).and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x[key].as_str().map(str::to_owned)).collect()).unwrap_or_default()
}

fn story_ids(v: &Viewer) -> Vec<String> {
    ids_of(&entry(v, "text"), "/stories", "story_id")
}

fn type_style_ids(v: &Viewer) -> Vec<String> {
    ids_of(&entry(v, "text"), "/styles/styles", "style_id")
}

/// Every record object (the object directly holding the string) of the pruned entries that names `id`, sorted.
fn records_of(v: &Viewer, id: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for at in naming(v, id) {
        let at = at.trim_end_matches(" (key)");
        let (token, rest) = at.split_once('/').unwrap_or((at, ""));
        let parent = rest.rsplit_once('/').map_or("", |(p, _)| p);
        out.push(entry(v, token).pointer(&format!("/{parent}")).map_or_else(|| "?".to_owned(), Value::to_string));
    }
    out.sort();
    out
}

/// A solid opaque RGB PNG.
fn png(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
    let samples = raster::Samples::U8((0..width * height).flat_map(|_| rgb).collect());
    raster::encode_png(&raster::PngEncodeRequest { width, height, colour: raster::Colour::Rgb, samples: &samples, icc: None, compression: raster::PngCompression::Fast }).unwrap()
}

/// A two-page layout document whose second page carries an embedded picture frame: (viewer, picture layer).
fn layout_with_picture(scratch: &Scratch) -> (Viewer, String) {
    let mut v = Viewer::new(Prefer::Cpu);
    v.new_document(&NewDocument::new(DocumentProfile::Layout, Some((200.0, 200.0))), None).unwrap();
    v.add_page(Some("page-1"), false, None, None).unwrap();
    let hero = scratch.0.join("hero.png");
    std::fs::write(&hero, png(32, 24, [220, 40, 40])).unwrap();
    let placed = ok(&mut v, "asset.place", json!({"path": hero.to_str().unwrap(), "placement_kind": "embedded", "page": "page-2", "frame_x": 10.0, "frame_y": 10.0, "frame_width": 32.0, "frame_height": 24.0}));
    (v, text(&placed, "layer_id"))
}

/// Lead ruling F1 + Q1 (type styles): a deleted text layer takes its record, its story and its own (unnamed) type
/// styles along, never a named style: a named style its story used and the own style a kept style is based on stay,
/// the own style that nothing else uses leaves with its `style_character` row, and the pruned entry decodes.
#[test]
fn a_deleted_text_layer_keeps_named_and_based_on_styles_and_drops_only_its_own() {
    let scratch = Scratch::new("prune-type-styles");
    let mut v = paper(&scratch);
    let gone = v.place_text(10.0, 30.0, "Hello").unwrap();
    let kept = v.place_text(10.0, 50.0, "Other").unwrap();
    let src = scratch.0.join("text.handshake");
    v.save(&src, false).unwrap();
    let out = scratch.0.join("named.handshake");
    let (mut own_paragraph, mut own_character, mut named, mut child, mut story) = (String::new(), String::new(), String::new(), String::new(), String::new());
    rewrite_entry(&src, &out, "text", |held| {
        let mut t = held.expect("a text entry");
        let record = t["layers"].as_array_mut().unwrap().iter_mut().find(|r| r["layer_id"] == gone.as_str()).expect("the record");
        own_paragraph = text(record, "paragraph_style");
        own_character = text(record, "character_style");
        story = text(record, "story_id");
        // Named styles take ids no layer uuid gives (the counter digits of the own style replaced).
        let stem = own_paragraph[..own_paragraph.len() - 12].to_owned();
        named = format!("{stem}00000000beef");
        child = format!("{stem}00000000bee2");
        record["paragraph_style"] = json!(named);
        let styles = t["styles"]["styles"].as_array_mut().unwrap();
        let base = styles.iter().find(|s| s["style_id"] == own_paragraph.as_str()).unwrap().clone();
        let mut heading = base.clone();
        heading["style_id"] = json!(named);
        heading["name"] = json!("Heading/H1");
        let mut body = base;
        body["style_id"] = json!(child);
        body["name"] = json!("Body/Child");
        body["based_on"] = json!(own_paragraph);
        styles.push(heading);
        styles.push(body);
        for s in t["stories"].as_array_mut().unwrap().iter_mut().filter(|s| s["story_id"] == story.as_str()) {
            for p in s["paragraphs"].as_array_mut().unwrap() {
                p["style_ref"] = json!(named);
            }
        }
        t["styles"]["style_character"] = json!([{"style_id": own_character, "attributes": {"tracking": 12.0}}]);
        serde_json::to_vec(&t).unwrap()
    });
    let mut w = open(&out);
    let (before, steps) = (state(&w), undo_len(&w));
    w.delete_layer(&gone).unwrap();
    assert_eq!(undo_len(&w), steps + 1, "the delete is one step");
    let t = entry(&w, "text");
    assert!(!ids_of(&t, "/layers", "layer_id").contains(&gone) && !story_ids(&w).contains(&story), "the record and the story leave (the text prune ran): {t}");
    let styles = type_style_ids(&w);
    assert!(styles.contains(&named), "a named style the story used stays: {styles:?}");
    assert!(styles.contains(&child) && styles.contains(&own_paragraph), "a kept style and the own style it is based on stay: {styles:?}");
    assert!(!styles.contains(&own_character), "the own style nothing else uses leaves: {styles:?}");
    assert!(ids_of(&t, "/styles/style_character", "style_id").is_empty(), "and its style_character row with it: {t}");
    assert!(ids_of(&t, "/layers", "layer_id").contains(&kept), "the other text layer keeps its record");
    let doc = w.run().snapshot().unwrap().document().clone();
    TextEntry::decode(w.run().document_entry("text").unwrap(), &doc, &CancellationToken::default()).unwrap_or_else(|e| panic!("the pruned entry decodes: {e}"));
    w.undo().unwrap();
    assert_eq!(state(&w), before, "undo restores the layer and the text entry together");
}

/// Lead ruling Q2: a knife, pathfinder or other replacement whose layer is a clip or mask source, or carries text,
/// refuses typed `structure_would_drop_dependents` naming the dependents, and changes nothing.
#[test]
fn a_replacement_of_a_clip_source_or_text_carrier_refuses_typed() {
    let scratch = Scratch::new("prune-dependents");
    let mut v = paper(&scratch);
    let target = v.draw_shape(ShapeKind::Rectangle, [60.0, 10.0], [80.0, 40.0], false).unwrap();
    let source = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [50.0, 40.0], false).unwrap();
    v.make_clip(&target, &source, false).unwrap();
    v.select(None);
    let before = state(&v);
    let cut = v.knife_cut(&[[30.0, 0.0], [30.0, 60.0]]);
    assert!(cut.as_ref().is_err_and(|e| e.contains("structure_would_drop_dependents") && e.contains(&target)), "a knife through a clip source refuses and names the clipped layer: {cut:?}");
    assert_eq!(state(&v), before, "nothing changed");
    let other = v.draw_shape(ShapeKind::Rectangle, [40.0, 30.0], [55.0, 45.0], false).unwrap();
    let joined = v.pathfinder_layers(PathfinderOp::Union, &[source.clone(), other.clone()]);
    assert!(joined.as_ref().is_err_and(|e| e.contains("structure_would_drop_dependents") && e.contains(&target)), "a pathfinder over a clip source refuses: {joined:?}");
    // A text carrier: the area text's story would leave with its container.
    let carrier = v.draw_shape(ShapeKind::Rectangle, [10.0, 45.0], [40.0, 60.0], false).unwrap();
    v.bind_live_text(&carrier, "Hi", LiveTextKind::Area { inset_pt: 1.0 }).unwrap();
    let story = format!("STXT-{}", &carrier[5..]);
    let before = state(&v);
    let joined = v.pathfinder_layers(PathfinderOp::Union, &[carrier.clone(), other.clone()]);
    assert!(joined.as_ref().is_err_and(|e| e.contains("structure_would_drop_dependents") && e.contains(&story)), "a pathfinder over a text carrier refuses and names the story: {joined:?}");
    assert_eq!(state(&v), before, "nothing changed");
}

/// Lead ruling Q4 / review F2: `page.delete` of a page alone on its spread takes the spread's layers' records (the
/// picture's raster record and placed instance) along in the page step; undo restores them, redo removes them.
#[test]
fn a_page_delete_takes_its_layers_records_along_in_its_step() {
    let scratch = Scratch::new("prune-page-delete");
    let (mut v, picture) = layout_with_picture(&scratch);
    assert!(!naming(&v, &picture).is_empty(), "set-up: the picture has a raster record");
    let (before, steps) = (state(&v), undo_len(&v));
    v.delete_page("page-2").unwrap();
    assert_eq!(undo_len(&v), steps + 1, "the page delete is one step");
    assert!(!has_layer(&v, &picture), "the picture goes with its page");
    assert_eq!(naming(&v, &picture), Vec::<String>::new(), "its records leave in the page step");
    v.undo().unwrap();
    assert_eq!(state(&v), before, "undo restores the page's layers and their records together");
    v.redo().unwrap();
    assert_eq!(naming(&v, &picture), Vec::<String>::new(), "redo removes them again");
}

/// Lead ruling Q4 / review F2: the layout editor's page delete (`layout.page.delete_page`, through `layout_commit`)
/// takes the page's layers' records along in its step.
#[test]
fn the_layout_editor_page_delete_takes_its_layers_records_along() {
    let scratch = Scratch::new("prune-editor-page-delete");
    let (mut v, picture) = layout_with_picture(&scratch);
    let (before, steps) = (state(&v), undo_len(&v));
    let r = ok(&mut v, "layout.page.delete_page", json!({"page": "page-2"}));
    assert_eq!(undo_len(&v), steps + 1, "one step: {r}");
    assert!(!has_layer(&v, &picture), "the picture goes with its page");
    assert_eq!(naming(&v, &picture), Vec::<String>::new(), "its records leave in the editor's step");
    v.undo().unwrap();
    assert_eq!(state(&v), before, "undo restores them");
}

/// Lead ruling Q3 / review F5: when an entry naming a removed layer cannot be read, the step commits without that
/// part and the driven row's result carries the typed warning `prune_skipped:<entry>`; a result without a warning
/// carries no `warnings` member.
#[test]
fn a_step_whose_prune_cannot_run_commits_and_carries_the_typed_warning() {
    let scratch = Scratch::new("prune-skipped");
    let (mut v, picture) = layout_with_picture(&scratch);
    let clean = ok(&mut v, "layout.page.delete_page", json!({"page": "page-2"}));
    assert!(clean.get("warnings").is_none(), "a clean step's result has no warnings member: {clean}");
    v.undo().unwrap();
    let src = scratch.0.join("picture.handshake");
    v.save(&src, false).unwrap();
    let out = scratch.0.join("unreadable.handshake");
    // A vector entry another writer left unreadable, naming the picture.
    rewrite_entry(&src, &out, "vector", |_| serde_json::to_vec(&json!({"schema": "not-a-vector-entry", "note": picture})).unwrap());
    let mut w = open(&out);
    let steps = undo_len(&w);
    let r = ok(&mut w, "layout.page.delete_page", json!({"page": "page-2"}));
    assert_eq!(undo_len(&w), steps + 1, "the step commits: {r}");
    assert_eq!(r["warnings"], json!(["prune_skipped:vector"]), "the result names the entry it could not prune: {r}");
    assert_eq!(naming(&w, &picture).iter().filter(|p| p.starts_with("raster_records")).count(), 0, "the readable entries are pruned");
}

/// Review F7: Convert area to point removes the container's binding in its own write; the old area story leaves the
/// text entry in the conversion's step all the same, and undo restores it.
#[test]
fn area_to_point_takes_the_old_area_story_along() {
    let scratch = Scratch::new("prune-area-to-point");
    let mut v = paper(&scratch);
    let carrier = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [80.0, 40.0], false).unwrap();
    v.bind_live_text(&carrier, "Hi", LiveTextKind::Area { inset_pt: 1.0 }).unwrap();
    let story = format!("STXT-{}", &carrier[5..]);
    assert!(story_ids(&v).contains(&story), "set-up: the area story");
    let steps = undo_len(&v);
    let point = v.convert_text_kind(&carrier).unwrap();
    assert_eq!(undo_len(&v), steps + 1, "the conversion is one step");
    assert!(has_layer(&v, &point) && !has_layer(&v, &carrier));
    assert!(!story_ids(&v).contains(&story), "the old area story leaves in the conversion's step: {:?}", story_ids(&v));
    v.undo().unwrap();
    assert!(story_ids(&v).contains(&story) && has_layer(&v, &carrier), "undo restores it with its container");
}

/// Review F3 (bystanders): a pathfinder leaves every record of a layer it does not remove byte for byte (its live
/// filter stack, design node, own style, clip attachment, the clip's source record) and their paths resolve.
#[test]
fn a_bystander_keeps_its_records_byte_for_byte() {
    let scratch = Scratch::new("prune-bystander");
    let mut v = paper(&scratch);
    let clip = v.draw_shape(ShapeKind::Rectangle, [60.0, 40.0], [90.0, 60.0], false).unwrap();
    let bystander = v.draw_shape(ShapeKind::Rectangle, [60.0, 10.0], [90.0, 30.0], false).unwrap();
    live_filter(&mut v, &bystander);
    v.set_fill(&bystander, Some(&PaintSpec::Solid([0, 0, 200]))).unwrap();
    v.make_clip(&bystander, &clip, false).unwrap();
    let a = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [30.0, 30.0], false).unwrap();
    let b = v.draw_shape(ShapeKind::Rectangle, [20.0, 20.0], [40.0, 40.0], false).unwrap();
    live_filter(&mut v, &b);
    v.select(None);
    let kept = [records_of(&v, &bystander), records_of(&v, &clip)];
    let paths: Vec<String> = path_ids(&v, &bystander).into_iter().chain(path_ids(&v, &clip)).collect();
    let style = own_style(&bystander);
    v.pathfinder_layers(PathfinderOp::Union, &[a.clone(), b.clone()]).unwrap();
    assert_eq!(naming(&v, &b), Vec::<String>::new(), "the operand's records leave");
    assert_eq!([records_of(&v, &bystander), records_of(&v, &clip)], kept, "the bystanders' records stay byte for byte");
    assert!(paths.iter().all(|p| stored_paths(&v).contains(p)) && styles(&v).contains(&style), "their paths and own style stay");
}

/// Review F3 (lead ruling Q1, the keep branch): a duplicate shares its source's path records; deleting the source
/// keeps every path the duplicate names.
#[test]
fn a_duplicates_shared_paths_stay_when_the_source_is_deleted() {
    let scratch = Scratch::new("prune-shared-paths");
    let mut v = paper(&scratch);
    let source = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [30.0, 30.0], false).unwrap();
    let copy = v.duplicate_layer(&source).unwrap();
    let paths = path_ids(&v, &copy);
    assert_eq!(paths, path_ids(&v, &source), "set-up: the copy names the source's paths");
    v.delete_layer(&source).unwrap();
    assert!(paths.iter().all(|p| stored_paths(&v).contains(p)), "every path the copy names still resolves");
    let doc = v.run().snapshot().unwrap().document().clone();
    hsk_studio_harness::view::vector::VectorEntry::decode(v.run().document_entry("vector").unwrap(), &doc, &CancellationToken::default()).unwrap_or_else(|e| panic!("the vector entry decodes: {e}"));
}

/// Review F3: a point text layer's delete takes its record, its story and its own type styles along; an area text's
/// container takes its binding, story and own styles; undo restores both.
#[test]
fn a_point_text_and_an_area_text_delete_take_their_records_along() {
    let scratch = Scratch::new("prune-text-kinds");
    let mut v = paper(&scratch);
    let point = v.place_text(10.0, 30.0, "Hello").unwrap();
    let carrier = v.draw_shape(ShapeKind::Rectangle, [10.0, 35.0], [80.0, 60.0], false).unwrap();
    v.bind_live_text(&carrier, "Hi", LiveTextKind::Area { inset_pt: 1.0 }).unwrap();
    let (before, steps) = (state(&v), undo_len(&v));
    for layer in [&point, &carrier] {
        let uuid = &layer[5..];
        v.delete_layer(layer).unwrap();
        assert_eq!(naming(&v, layer), Vec::<String>::new(), "{layer}: its record or binding leaves");
        assert!(!story_ids(&v).contains(&format!("STXT-{uuid}")), "{layer}: its story leaves");
        let flip = if &uuid[19..20] == "b" { "8" } else { "b" };
        let own = [format!("STYS-{uuid}"), format!("STYS-{}", uuid.replacen("-9000-", "-a000-", 1)), format!("STYS-{}{flip}{}", &uuid[..19], &uuid[20..])];
        assert!(!type_style_ids(&v).iter().any(|s| own.contains(s)), "{layer}: its own type styles leave: {:?}", type_style_ids(&v));
    }
    assert_eq!(undo_len(&v), steps + 2);
    v.undo().unwrap();
    v.undo().unwrap();
    assert_eq!(state(&v), before, "undo restores both with their records");
}
