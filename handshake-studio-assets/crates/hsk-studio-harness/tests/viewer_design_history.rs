//! Lane SMB-DS-VARIABLES-AUTHOR-1 (feature `viewer`), the two ledger candidates of 2026-11-12:
//!
//! - history-aware text compaction for component syncs (lead ruling P2, 2026-11-13: the compaction is
//!   recorded in the undo step): a sync drops the `text` records of the copies it deletes inside its own
//!   history step, so undo brings them back byte for byte and the per-session sync cap is gone;
//! - `.fig` design-state prune at save (lead ruling P3): the saved `design` entry leaves out the states
//!   no reopened document can reach (superseded contents), serialized from a copy, so the in-session
//!   entry, which undo still needs, is never changed, and the reopen is byte-identical to what was saved.
#![cfg(feature = "viewer")]

mod common;

use common::Scratch;
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
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

fn layers(v: &Viewer) -> String {
    serde_json::to_string(&v.run().snapshot().unwrap().document().layers).unwrap()
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

// ---- P2: text compaction recorded in the undo step ---------------------------------------------------

/// 300 syncs of a label component with two instances never refuse `text_layer_over_cap` (the cap was
/// reached after about 127 before), and the text entry holds only what the document draws.
#[test]
fn three_hundred_syncs_of_a_two_instance_label_never_refuse_text_layer_over_cap() {
    let mut v = design_doc();
    let (a, _) = label_component(&mut v);
    for i in 0..300 {
        if let Err((code, detail)) = sync(&mut v, &a) {
            panic!("sync {i} refused {code}: {detail}");
        }
    }
    only_drawn_text(&v).unwrap();
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
    let path = scratch.0.join("saved.handshake");
    v.save(&path, false).unwrap();
    assert_eq!(entry(&v, "design"), session, "the save never changes the in-session entry");
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&path).unwrap();
    let reopened = entry(&w, "design");
    let kept = states(&reopened);
    assert_eq!(kept.iter().filter(|s| s["layer_id"] == layer.as_str()).count(), 1, "only the current content's state is saved: {kept:?}");
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
