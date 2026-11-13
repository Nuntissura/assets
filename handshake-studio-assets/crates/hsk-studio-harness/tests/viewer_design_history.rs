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

/// Review F2 (lead option (b)): a component of 341 instances keeps syncing (3 states per instance at most
/// stay reachable: the copies alternate between minted ids, and a state of the same layer, content and
/// instance is replaced, not added); room is counted net of those replacements. One more instance makes
/// the next sync refuse `design_state_over_cap` typed, and that refusal changes nothing (no partial sync).
#[test]
fn measured_probe_341_instances_keep_syncing_and_342_are_refused_whole() {
    let mut v = design_doc();
    let component = box_component(&mut v);
    instances(&mut v, &component, 341);
    let mut done = 0;
    for i in 0..6 {
        match sync(&mut v, &component) {
            Ok(_) => done += 1,
            Err((code, detail)) => panic!("sync {i} of 341 instances refused {code}: {detail} (states held {})", states_held(&v)),
        }
    }
    println!("MEASURED: 341 instances: {done} syncs, none refused, design states held {}", states_held(&v));
    instances(&mut v, &component, 1);
    let before = whole(&v);
    let (code, detail) = sync(&mut v, &component).expect_err("342 instances: no room for the sync's new states");
    assert_eq!(code, "design_state_over_cap", "{detail}");
    assert!(whole(&v) == before, "the refused sync changed nothing");
    println!("MEASURED: 342 instances after {done} syncs: the next sync refused design_state_over_cap (states held {})", states_held(&v));
}

/// Review F2 (lead): a component of 513 instances is refused on its FIRST sync, typed, and nothing changes.
#[test]
fn a_513_instance_first_sync_is_refused_and_changes_nothing() {
    let mut v = design_doc();
    let component = box_component(&mut v);
    instances(&mut v, &component, 513);
    let before = whole(&v);
    let (code, detail) = sync(&mut v, &component).expect_err("513 instances: 1026 states do not fit 1024");
    assert_eq!(code, "design_state_over_cap", "{detail}");
    assert!(whole(&v) == before, "no partial sync");
    println!("MEASURED: 513 instances: the first sync refused design_state_over_cap (states held {})", states_held(&v));
}

/// Review F2 probe (lead Q8): syncs until the cap for small components: two instances and 64 instances keep
/// syncing (the bound is the instance count, not the number of syncs).
#[test]
fn measured_probe_small_components_sync_without_bound() {
    for (n, rounds) in [(2_usize, 600_usize), (64, 120)] {
        let mut v = design_doc();
        let component = box_component(&mut v);
        instances(&mut v, &component, n);
        for i in 0..rounds {
            if let Err((code, detail)) = sync(&mut v, &component) {
                panic!("{n} instances: sync {i} refused {code}: {detail} (states held {})", states_held(&v));
            }
        }
        assert!(states_held(&v) <= 3 * n, "{n} instances: the states stay bounded ({})", states_held(&v));
        println!("MEASURED: {n} instances: {rounds} syncs, none refused, design states held {}", states_held(&v));
    }
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
