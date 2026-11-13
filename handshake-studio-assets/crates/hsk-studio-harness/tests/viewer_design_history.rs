//! Lane SMB-DS-VARIABLES-AUTHOR-1 (feature `viewer`), the two ledger candidates of 2026-11-12:
//!
//! - history-aware text compaction for component syncs (lead ruling P2, 2026-11-13: the compaction is
//!   recorded in the undo step): a sync drops the `text` records of the copies it deletes inside its own
//!   history step, so undo brings them back byte for byte and the per-session sync cap is gone;
//! - `.fig` design-state prune at save (lead ruling P3, review F1 / Q2): the saved `design` entry leaves
//!   out only the states that name a layer the document no longer holds (copies a redraw deleted), built
//!   on a copy, so the in-session entry, which undo still needs, is never changed, and the reopen is
//!   byte-identical to what was saved. A state whose children all still exist is kept: moving a child out
//!   and back, or grouping and ungrouping, returns to it without any history.
//!
//! Review F2 / Q1 (lead 2026-11-13, option (b)): the design entry keeps at most 1024 instance states; a
//! sync needs room for its new states next to every state undo can still reach, counted net of the states
//! it replaces. The measured bound is printed by the probe tests and stated in the manual. "Byte for byte"
//! below holds for the entries; layers compare by id, record and stack placement (undo re-creates deleted
//! layers at another index of the layer array, chronicle behaviour from before this lane).
#![cfg(feature = "viewer")]

mod common;

use common::Scratch;
use hsk_studio_accord::CancellationToken;
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::vector::ShapeKind;
use hsk_studio_harness::{Harness, Place, PlacementWrite, Selection, asset_local_test_context};
use hsk_studio_harness_core::document_new::{DocumentProfile, NewDocument};
use serde_json::{Value, json};
use std::collections::BTreeSet;

const INTER: &[u8] = include_bytes!("../../hsk-studio-type/tests/fixtures/fonts/Inter-Regular.ttf");

fn design_doc() -> Viewer {
    let mut v = Viewer::new(Prefer::Cpu);
    v.new_document(&NewDocument::new(DocumentProfile::Design, None), None).unwrap();
    v.add_font("Inter-Regular", INTER.to_vec());
    v
}

fn call(v: &mut Viewer, id: &str, params: Value) -> Result<Value, (String, Value)> {
    let Value::Object(map) = params else { panic!("parameters are an object") };
    v.run_command(id, &map).map_err(|r| (r.code, r.detail))
}

fn ok(v: &mut Viewer, id: &str, params: Value) -> Value {
    call(v, id, params).unwrap_or_else(|(code, detail)| panic!("{id}: {code} {detail}"))
}

fn text(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or_else(|| panic!("no {key} in {v}")).to_string()
}

fn entry(v: &Viewer, token: &str) -> Vec<u8> {
    v.run().document_entry(token).map(<[u8]>::to_vec).unwrap_or_default()
}

/// Every layer (its record and its place in its parent's stack), by id: undo re-creates deleted layers at
/// another index of the document's layer array, which carries no meaning (the stack order is the placement).
fn layers(v: &Viewer) -> String {
    let doc = v.run().snapshot().unwrap().document();
    let mut all: Vec<(String, String, String)> = doc
        .layers
        .iter()
        .map(|l| (l.layer_id.clone(), serde_json::to_string(l).unwrap(), format!("{:?}", v.run().placement(&l.layer_id))))
        .collect();
    all.sort();
    format!("{all:?}")
}

/// (text entry, vector entry, layers): what one history step must restore together.
fn snapshot(v: &Viewer) -> (Vec<u8>, Vec<u8>, String) {
    (entry(v, "text"), entry(v, "vector"), layers(v))
}

fn undo_depth(v: &Viewer) -> usize {
    v.run().history()["undo"].as_array().map_or(0, Vec::len)
}

/// Every text record names a layer the document draws with that story, and every story is named by a
/// record or a layer: nothing a compaction should have dropped is left.
fn only_drawn_text(v: &Viewer) -> Result<(), String> {
    let doc = v.run().snapshot().unwrap().document();
    let drawn: BTreeSet<(String, String)> = doc
        .layers
        .iter()
        .filter_map(|l| {
            let p = serde_json::to_value(&l.payload).ok()?;
            Some((l.layer_id.clone(), p["story_id"].as_str()?.to_string()))
        })
        .collect();
    let e: Value = serde_json::from_slice(&entry(v, "text")).unwrap();
    for r in e["layers"].as_array().unwrap() {
        let key = (text(r, "layer_id"), text(r, "story_id"));
        if !drawn.contains(&key) {
            return Err(format!("text record of an undrawn layer {key:?}"));
        }
    }
    let named: BTreeSet<String> = e["layers"].as_array().unwrap().iter().map(|r| text(r, "story_id")).chain(drawn.iter().map(|(_, s)| s.clone())).collect();
    for s in e["stories"].as_array().unwrap() {
        let id = text(s, "story_id");
        if !named.contains(&id) {
            return Err(format!("story {id} nothing names"));
        }
    }
    Ok(())
}

/// A label component (one text layer) and two of its instances: (component, first instance).
fn label_component(v: &mut Viewer) -> (String, String) {
    let label = v.place_text_with_font(10.0, 40.0, "OK", "Inter-Regular").unwrap();
    v.select(None);
    let a = text(&ok(v, "component.create_from_selection", json!({"layers": [label], "name": "Label"})), "component_id");
    let one = text(&ok(v, "component.create_instance", json!({"component": a, "x": 200, "y": 10})), "layer_id");
    ok(v, "component.create_instance", json!({"component": a, "x": 300, "y": 10}));
    (a, one)
}

fn sync(v: &mut Viewer, component: &str) -> Result<Value, (String, Value)> {
    call(v, "component.sync_instances", json!({"component": component}))
}

/// `n` more instances of `component` in a row.
fn instances(v: &mut Viewer, component: &str, n: usize) -> Vec<String> {
    (0..n).map(|i| text(&ok(v, "component.create_instance", json!({"component": component, "x": 200 + 3 * (i % 100), "y": 10 + 3 * (i / 100)})), "layer_id")).collect()
}

/// A component of one rectangle (no text: the text entry's 256 records do not bound it).
fn box_component(v: &mut Viewer) -> String {
    let r = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [20.0, 20.0], false).unwrap();
    v.select(None);
    text(&ok(v, "component.create_from_selection", json!({"layers": [r], "name": "Box"})), "component_id")
}

fn states_held(v: &Viewer) -> usize {
    let e = entry(v, "design");
    if e.is_empty() {
        return 0;
    }
    serde_json::from_slice::<Value>(&e).unwrap()["instances"].as_array().map_or(0, Vec::len)
}

/// Everything a refused command must leave as it was: entries, layers, history.
fn whole(v: &Viewer) -> (Vec<u8>, Vec<u8>, Vec<u8>, String, Value) {
    (entry(v, "text"), entry(v, "vector"), entry(v, "design"), layers(v), v.run().history()["undo"].clone())
}

// ---- P2: text compaction recorded in the undo step ---------------------------------------------------

/// 300 syncs of a label component with 16 instances (review F8: many, not 2) never refuse (the text cap
/// was reached after about 127 syncs of two instances before), and the text entry holds only what the
/// document draws.
#[test]
fn three_hundred_syncs_of_a_sixteen_instance_label_never_refuse() {
    let mut v = design_doc();
    let (a, _) = label_component(&mut v);
    instances(&mut v, &a, 14);
    for i in 0..300 {
        if let Err((code, detail)) = sync(&mut v, &a) {
            panic!("sync {i} refused {code}: {detail}");
        }
    }
    only_drawn_text(&v).unwrap();
    println!("MEASURED: 16 label instances, 300 syncs, design states held {}", states_held(&v));
}

/// Undo back across compactions restores the copies' text records byte for byte.
#[test]
fn undo_back_across_compactions_restores_the_copies_text_byte_for_byte() {
    let mut v = design_doc();
    let (a, _) = label_component(&mut v);
    let start = snapshot(&v);
    for _ in 0..5 {
        sync(&mut v, &a).unwrap();
    }
    only_drawn_text(&v).unwrap();
    for _ in 0..5 {
        v.undo().unwrap();
    }
    assert_eq!(snapshot(&v), start, "text entry, vector entry and layers back byte for byte");
}

/// One undo step restores the text entry and the vector entry together with the layers.
#[test]
fn one_undo_step_restores_the_text_and_vector_entries_together() {
    let mut v = design_doc();
    let (a, _) = label_component(&mut v);
    sync(&mut v, &a).unwrap();
    let before = snapshot(&v);
    let depth = undo_depth(&v);
    sync(&mut v, &a).unwrap();
    assert_eq!(undo_depth(&v), depth + 1, "a sync is one history step");
    only_drawn_text(&v).unwrap();
    v.undo().unwrap();
    let after = snapshot(&v);
    assert_eq!(after.0, before.0, "the text entry comes back with the step");
    assert_eq!(after.1, before.1, "the vector entry comes back with the step");
    assert_eq!(after.2, before.2, "the layers come back with the step");
}

/// Redo after that undo gives the compacted state again.
#[test]
fn redo_after_the_undo_gives_the_compacted_state_again() {
    let mut v = design_doc();
    let (a, _) = label_component(&mut v);
    sync(&mut v, &a).unwrap();
    sync(&mut v, &a).unwrap();
    only_drawn_text(&v).unwrap();
    let compacted = snapshot(&v);
    v.undo().unwrap();
    v.redo().unwrap();
    assert_eq!(snapshot(&v), compacted);
    only_drawn_text(&v).unwrap();
}

// ---- P3: design file states pruned at save ---------------------------------------------------------

/// The design file fixture opened as a native document: (scratch, viewer, an instance layer with a
/// variant property of at least two values, that property, its current value, another value).
fn fig_doc(label: &str) -> (Scratch, Viewer, String, String, String, String) {
    let scratch = Scratch::new(label);
    let fig = scratch.0.join("components.fig");
    std::fs::copy(format!("{}/../hsk-studio-interop-fig/tests/fixtures/fig-components-autolayout.fig", env!("CARGO_MANIFEST_DIR")), &fig).unwrap();
    let dest = scratch.0.join("components.handshake");
    let mut v = design_doc();
    let report = ok(&mut v, "design.import_fig", json!({"source": fig.to_str().unwrap(), "dest": dest.to_str().unwrap()}));
    for layer in report["instance_layers"].as_array().unwrap() {
        let layer = layer.as_str().unwrap().to_string();
        let view = ok(&mut v, "design.instance.inspect", json!({"layer": layer}));
        for (property, variant) in view["variants"].as_object().unwrap() {
            let current = text(variant, "current");
            if let Some(other) = variant["values"].as_array().unwrap().iter().filter_map(Value::as_str).find(|x| *x != current) {
                return (scratch, v, layer, property.clone(), current, other.to_string());
            }
        }
    }
    panic!("no instance with a variant property of two values in the fixture: {report}");
}

fn states(bytes: &[u8]) -> Vec<Value> {
    let v: Value = serde_json::from_slice(bytes).unwrap();
    v["instances"].as_array().cloned().unwrap_or_default()
}

/// Two variant switches leave a superseded state in the session; the saved file leaves it out, the
/// reopen is byte-identical to the saved bytes and prunes nothing more, and the session keeps every
/// state, so undo still returns to the superseded content's state.
#[test]
fn a_design_file_state_superseded_in_the_session_is_left_out_of_the_save_only() {
    let (scratch, mut v, layer, property, current, other) = fig_doc("ds-p3-prune");
    ok(&mut v, "design.instance.switch_variant", json!({"layer": layer, "property": property, "value": other}));
    ok(&mut v, "design.instance.switch_variant", json!({"layer": layer, "property": property, "value": current}));
    let session = entry(&v, "design");
    let held = states(&session);
    assert!(held.iter().filter(|s| s["layer_id"] == layer.as_str()).count() >= 2, "a superseded state in the session: {held:?}");
    let newest = held.iter().rev().find(|s| s["layer_id"] == layer.as_str()).cloned().unwrap();
    let history = v.run().history()["undo"].clone();
    let path = scratch.0.join("saved.handshake");
    v.save(&path, false).unwrap();
    assert_eq!(entry(&v, "design"), session, "the save never changes the in-session entry");
    assert_eq!(v.run().history()["undo"], history, "nor the history (depth and labels)");
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&path).unwrap();
    let reopened = entry(&w, "design");
    let kept = states(&reopened);
    let mine: Vec<&Value> = kept.iter().filter(|s| s["layer_id"] == layer.as_str()).collect();
    assert_eq!(mine, vec![&newest], "the layer's newest state (its children are drawn) is the only one saved: {kept:?}");
    assert!(kept.len() < held.len());
    // Reopen prunes nothing more: saving the reopened document again writes the same entry.
    let again = scratch.0.join("again.handshake");
    w.save(&again, false).unwrap();
    let mut x = Viewer::new(Prefer::Cpu);
    x.open(&again).unwrap();
    assert_eq!(entry(&x, "design"), reopened, "save, reopen, save, reopen: byte-identical");
    assert_eq!(ok(&mut w, "design.instance.inspect", json!({"layer": layer}))["state"], "edited");
    // In the session, undo still reaches the superseded content's state.
    v.undo().unwrap();
    let back = ok(&mut v, "design.instance.inspect", json!({"layer": layer}));
    assert_eq!((back["state"].as_str(), back["variants"][&property]["current"].as_str()), (Some("edited"), Some(other.as_str())), "{back}");
}

/// A save that fails leaves the in-session entry unchanged and writes no file.
#[test]
fn a_failed_save_leaves_the_session_entry_unchanged() {
    let (scratch, mut v, layer, property, current, other) = fig_doc("ds-p3-failed");
    ok(&mut v, "design.instance.switch_variant", json!({"layer": layer, "property": property, "value": other}));
    ok(&mut v, "design.instance.switch_variant", json!({"layer": layer, "property": property, "value": current}));
    let session = entry(&v, "design");
    let depth = undo_depth(&v);
    let path = scratch.0.join("missing-folder").join("saved.handshake");
    assert!(v.save(&path, false).is_err(), "a save into a missing folder fails");
    assert!(!path.exists(), "no file is left");
    assert_eq!(entry(&v, "design"), session, "the in-session entry is unchanged");
    assert_eq!(undo_depth(&v), depth, "the history is unchanged");
}

fn child_layers(v: &Viewer, parent: &str) -> Vec<String> {
    v.run().snapshot().unwrap().document().layers.iter().filter(|l| serde_json::to_value(&l.parent).unwrap()["node_id"] == parent).map(|l| l.layer_id.clone()).collect()
}

fn reopen(path: &std::path::Path) -> Viewer {
    let mut w = Viewer::new(Prefer::Cpu);
    w.add_font("Inter-Regular", INTER.to_vec());
    w.open(path).unwrap();
    w
}

/// Review F1 (lead Q2): a design file state whose children all still exist survives the save. Moving a
/// child out of the instance and back returns to it with no history: after reopen the instance reads
/// `edited` with its switched variant and answers commands.
#[test]
fn a_design_file_state_whose_children_still_exist_survives_save() {
    let (scratch, mut v, layer, property, _, other) = fig_doc("ds-f1-move-out");
    ok(&mut v, "design.instance.switch_variant", json!({"layer": layer, "property": property, "value": other}));
    let children = child_layers(&v, &layer);
    let moved = children.last().cloned().unwrap();
    v.move_layer(&moved, Place::AtEnd(None)).unwrap();
    let path = scratch.0.join("moved-out.handshake");
    v.save(&path, false).unwrap();
    let mut w = reopen(&path);
    w.move_layer(&moved, Place::Inside(layer.clone())).unwrap();
    assert_eq!(child_layers(&w, &layer), children, "the children are back as they were");
    let view = ok(&mut w, "design.instance.inspect", json!({"layer": layer}));
    assert_eq!((view["state"].as_str(), view["variants"][&property]["current"].as_str()), (Some("edited"), Some(other.as_str())), "{view}");
    ok(&mut w, "design.instance.reset_all", json!({"layer": layer}));
}

/// Review F1: grouping two children of a design file instance and ungrouping them after a save and reopen
/// returns to the switched state.
#[test]
fn a_design_file_state_survives_group_save_reopen_ungroup() {
    let (scratch, mut v, layer, property, _, other) = fig_doc("ds-f1-group");
    ok(&mut v, "design.instance.switch_variant", json!({"layer": layer, "property": property, "value": other}));
    let children = child_layers(&v, &layer);
    let take: Vec<String> = children.iter().take(2).cloned().collect();
    let group = v.group_layers(&take, "Tmp").unwrap();
    let path = scratch.0.join("grouped.handshake");
    v.save(&path, false).unwrap();
    let mut w = reopen(&path);
    w.ungroup(&group).unwrap();
    assert_eq!(child_layers(&w, &layer), children, "the children are back as they were");
    let view = ok(&mut w, "design.instance.inspect", json!({"layer": layer}));
    assert_eq!((view["state"].as_str(), view["variants"][&property]["current"].as_str()), (Some("edited"), Some(other.as_str())), "{view}");
}

/// Review F1 (lead Q2): the same rule for an authored (native) instance, at save and at open: a child moved
/// out before the save and back after the reopen leaves an instance that still answers commands.
#[test]
fn a_native_state_whose_children_still_exist_survives_save_and_open() {
    let scratch = Scratch::new("ds-f1-native");
    let mut v = design_doc();
    let a = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [50.0, 50.0], false).unwrap();
    let b = v.draw_shape(ShapeKind::Ellipse, [60.0, 10.0], [90.0, 40.0], false).unwrap();
    v.select(None);
    let component = text(&ok(&mut v, "component.create_from_selection", json!({"layers": [a, b], "name": "Card"})), "component_id");
    let frame = text(&ok(&mut v, "component.create_instance", json!({"component": component, "x": 300, "y": 10})), "layer_id");
    let children = child_layers(&v, &frame);
    let moved = children.last().cloned().unwrap();
    v.move_layer(&moved, Place::AtEnd(None)).unwrap();
    let path = scratch.0.join("native.handshake");
    v.save(&path, false).unwrap();
    let mut w = reopen(&path);
    w.move_layer(&moved, Place::Inside(frame.clone())).unwrap();
    assert_eq!(child_layers(&w, &frame), children);
    assert_eq!(ok(&mut w, "design.instance.inspect", json!({"layer": frame}))["state"], "edited");
    let synced = ok(&mut w, "component.sync_instances", json!({"component": component}));
    assert_eq!(synced["redrawn"], json!([frame]), "the instance answers commands: {synced}");
}

// ---- review F2: the design entry's state bound, net of replaced states -------------------------------

/// The refusal codes a sync of a large component may answer: no room for its states, or a structure patch
/// past the session's budget. Either refusal must change nothing.
const SYNC_BOUNDS: [&str; 2] = ["design_state_over_cap", "budget_exceeded"];

/// Review F2 probe (lead Q8): a box component grown one instance at a time (a duplicate of 32 or more
/// instances is one structure patch past the session's budget, measured `budget_exceeded`), synced at each
/// size. Prints the sizes that sync, the states they hold and the first refusal: the patch budget refuses a
/// sync long before the design entry's 1024 states fill up. Every refusal is typed and changes nothing.
#[test]
fn measured_probe_sync_bound_by_instance_count() {
    let mut v = design_doc();
    let component = box_component(&mut v);
    let mut count = 0_usize;
    for size in [1_usize, 2, 4, 8, 16, 32, 48, 64, 96, 128, 160, 192] {
        instances(&mut v, &component, size - count);
        count = size;
        let rounds = if count <= 16 { 3 } else { 1 };
        for _ in 0..rounds {
            let before = whole(&v);
            if let Err((code, detail)) = sync(&mut v, &component) {
                assert!(SYNC_BOUNDS.contains(&code.as_str()), "{count} instances: {code} {detail}");
                assert!(whole(&v) == before, "{count} instances: the refused sync changed nothing");
                println!("MEASURED: {count} instances: the sync refused {code} (states held {})", states_held(&v));
                return;
            }
            // How many states a sync adds depends on whether the copies' minted ids repeat (they do while the
            // document's layer count stays the same: three states per instance); measured, not asserted.
            assert!(states_held(&v) <= 1024, "{count} instances: Folio's bound holds ({})", states_held(&v));
        }
        println!("MEASURED: {count} instances: {rounds} syncs, none refused, design states held {}", states_held(&v));
    }
    println!("NOT_PROVEN: a sync refusal by size (192 instances still synced)");
}

/// Review F2 (lead): a sync with no room for its new states is refused `design_state_over_cap` before its
/// step, typed, and nothing changes (no partial sync). A 513-instance component cannot be built and synced
/// here (the structure patch budget refuses its sync first, measured by the probe above), so the design entry
/// is filled instead: a saved document of four instances whose entry holds 1020 more states of the same
/// layers and instances (all their layers exist, so the reopen keeps them), then reopened.
#[test]
fn a_sync_with_no_room_for_its_states_is_refused_whole() {
    let scratch = Scratch::new("ds-f2-full");
    let mut v = design_doc();
    let component = box_component(&mut v);
    instances(&mut v, &component, 4);
    let path = scratch.0.join("states.handshake");
    v.save(&path, false).unwrap();
    let h = Harness::new(Selection { providers: vec!["chronicle.patch".to_string()], ..Selection::default() }).unwrap();
    let cancel = CancellationToken::default();
    let mut run = h.run(asset_local_test_context(), &cancel);
    assert!(run.open_path(&path));
    let ids: Vec<String> = run.snapshot().unwrap().document().layers.iter().map(|l| l.layer_id.clone()).collect();
    let mut design: Value = serde_json::from_slice(run.document_entry("design").unwrap()).unwrap();
    let held: Vec<Value> = design["instances"].as_array().unwrap().clone();
    let mut extra = Vec::new();
    for (k, s) in held.iter().enumerate() {
        for i in 0..255 {
            let mut more = s.clone();
            let pick = |n: usize| ids[n % ids.len()].clone();
            more["content_signature"] = json!([pick(i), pick(i / ids.len() + 1), pick(i / (ids.len() * ids.len()) + k + 2)]);
            extra.push(more);
        }
    }
    design["instances"].as_array_mut().unwrap().extend(extra);
    run.set_document_entry("design", Some(serde_json::to_vec(&design).unwrap())).unwrap();
    let full = scratch.0.join("full.handshake");
    assert!(run.save_to(&full));
    let mut w = reopen(&full);
    assert_eq!(states_held(&w), 1024, "the reopen keeps every state whose layers exist");
    let before = whole(&w);
    let (code, detail) = sync(&mut w, &component).expect_err("four new states do not fit");
    assert_eq!(code, "design_state_over_cap", "{detail}");
    assert!(whole(&w) == before, "no partial sync");
    assert!(w.redo().is_err(), "and no redo entry");
}

/// Review F2 (lead): the 513-instance first sync, kept as a NOT_PROVEN marker: building 513 instances one
/// by one passes the per-test time cap and a duplicate of 32 or more is refused `budget_exceeded`.
#[test]
fn a_513_instance_first_sync_is_refused_and_changes_nothing() {
    println!("NOT_PROVEN: a 513-instance component's first sync (cannot be built within a test: one by one passes the 120 s cap, a duplicate of 32 instances is refused budget_exceeded); the refusal path is proven by a_sync_with_no_room_for_its_states_is_refused_whole and persist::room_is_counted_net_of_replaced_states");
}

/// Review F2 probe: a two-instance component syncs without bound (300 syncs; the copies' ids alternate, so
/// a state of the same layer, content and instance is replaced and three states per instance stay held).
#[test]
fn measured_probe_two_instances_sync_without_bound() {
    let mut v = design_doc();
    let component = box_component(&mut v);
    instances(&mut v, &component, 2);
    for i in 0..300 {
        if let Err((code, detail)) = sync(&mut v, &component) {
            panic!("2 instances: sync {i} refused {code}: {detail} (states held {})", states_held(&v));
        }
    }
    assert!(states_held(&v) <= 6, "the states stay bounded ({})", states_held(&v));
    println!("MEASURED: 2 instances: 300 syncs, none refused, design states held {}", states_held(&v));
}

// ---- review F4 / F5: refusals and rollbacks ------------------------------------------------------------

/// Review F4 (lead Q4): `Run::restructure_and_entries` refused before any write leaves the layers, both
/// entries, the history and the redo history as they were.
#[test]
fn restructure_and_entries_refused_before_any_write_changes_nothing() {
    const FIXTURE: &[u8] = include_bytes!("fixtures/demo.handshake");
    const L1: &str = "SLYR-019abcde-0000-7000-8000-000000000001";
    let h = Harness::new(Selection { providers: vec!["chronicle.patch".to_string()], ..Selection::default() }).unwrap();
    let cancel = CancellationToken::default();
    let mut run = h.run(asset_local_test_context(), &cancel);
    assert!(run.open_bytes(FIXTURE));
    assert!(run.rename(L1, "renamed", None));
    assert!(run.undo());
    let state = |run: &hsk_studio_harness::Run<'_>| {
        let doc = serde_json::to_string(&run.snapshot().unwrap().document().layers).unwrap();
        (doc, run.document_entry("vector").map(<[u8]>::to_vec), run.document_entry("text").map(<[u8]>::to_vec), run.history())
    };
    let before = state(&run);
    // The same layer twice: the structure edit is refused (duplicate_layer) before anything is written.
    let twice = vec![PlacementWrite { layer_id: L1.into(), to: Some(Place::AtEnd(None)), create: None }, PlacementWrite { layer_id: L1.into(), to: Some(Place::AtBeginning(None)), create: None }];
    assert!(!run.restructure_and_entries("probe", &twice, vec![("vector", b"{}".to_vec()), ("text", b"{}".to_vec())]));
    assert_eq!(state(&run), before, "nothing changed, the redo history kept");
    assert!(!run.restructure_and_entries("probe", &[], Vec::new()), "no entry: nothing_to_change");
    assert_eq!(state(&run), before);
    assert!(run.redo(), "the redo history is still there");
}

/// Review F5: a design state refused after the sync committed rolls the sync back quietly and leaves no
/// redo entry; the design entry and the history are as before the command. (The stored entry names
/// another document: its native states still bind, the whole entry is refused when a state is written.)
#[test]
fn a_state_refused_after_the_sync_commits_leaves_no_redo_entry() {
    let scratch = Scratch::new("ds-f5-rollback");
    let mut v = design_doc();
    let component = box_component(&mut v);
    instances(&mut v, &component, 2);
    let path = scratch.0.join("rollback.handshake");
    v.save(&path, false).unwrap();
    let h = Harness::new(Selection { providers: vec!["chronicle.patch".to_string()], ..Selection::default() }).unwrap();
    let cancel = CancellationToken::default();
    let mut run = h.run(asset_local_test_context(), &cancel);
    assert!(run.open_path(&path));
    let mut design: Value = serde_json::from_slice(run.document_entry("design").unwrap()).unwrap();
    design["document_id"] = json!("SDOC-019a0000-0000-7000-8000-0000000000ff");
    run.set_document_entry("design", Some(serde_json::to_vec(&design).unwrap())).unwrap();
    let stale = scratch.0.join("stale.handshake");
    assert!(run.save_to(&stale));
    let mut w = reopen(&stale);
    let before = whole(&w);
    let (code, detail) = sync(&mut w, &component).expect_err("the whole design entry is refused when the states are written");
    assert_eq!(code, "design_state_entry_invalid", "{detail}");
    assert!(whole(&w) == before, "the sync was rolled back and the design entry is unchanged");
    assert!(w.redo().is_err(), "the rollback leaves no redo entry");
}
