//! Lane SMB-MOTION-COMMANDS-1 ([STU-MOT-130], [STU-MOT-038], [STU-CMP-110]; the motion-comp scenario,
//! `workflows/scenario-motion-comp.json`): every step below runs through `Run::motion_command`, one
//! named undo step, with the caller's attribution, and survives checkpoint and reopen. The module
//! `export` (feature `media`) proves the result reaches the render queue and the renderer.
use hsk_studio_accord::CancellationToken;
use hsk_studio_harness::{Harness, Run, Selection, asset_local_test_context};
use hsk_studio_harness_core::action::{self, RUN_COMMANDS};
use hsk_studio_harness_core::harness::motion_commands::{params_from_text, row, spec_id};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const FIXTURE: &[u8] = include_bytes!("fixtures/demo.handshake");
const L1: &str = "SLYR-019abcde-0000-7000-8000-000000000001";
const L2: &str = "SLYR-019abcde-0000-7000-8000-000000000002";
const S: u64 = 254_016_000_000;
const F: u64 = 10_160_640_000; // 25 fps

fn harness() -> Harness {
    Harness::new(Selection { providers: vec!["chronicle.patch".to_string()], ..Selection::default() }).unwrap()
}

fn cmd(run: &mut Run<'_>, id: &str, params: Value) -> Value {
    assert!(run.motion_command(id, &params), "{id} {params}: {}", run.ops().last().unwrap());
    run.ops().last().unwrap()["detail"].clone()
}

/// A refused command: its typed code; asserts the entries did not change.
fn refused(run: &mut Run<'_>, id: &str, params: Value) -> String {
    let before = (run.document_entry("composition").map(<[u8]>::to_vec), run.document_entry("motion").map(<[u8]>::to_vec), run.snapshot().map(|s| s.document().revision));
    let depth = undo_len(run);
    assert!(!run.motion_command(id, &params), "{id} {params} was accepted");
    let after = (run.document_entry("composition").map(<[u8]>::to_vec), run.document_entry("motion").map(<[u8]>::to_vec), run.snapshot().map(|s| s.document().revision));
    assert_eq!(before, after, "{id}: a refusal changes nothing");
    assert_eq!(undo_len(run), depth, "{id}: a refusal adds no history step");
    run.ops().last().unwrap()["error"].as_str().unwrap().to_string()
}

fn undo_len(run: &Run<'_>) -> usize {
    run.history()["undo"].as_array().unwrap().len()
}

fn last_label(run: &Run<'_>) -> String {
    run.history()["undo"].as_array().unwrap().last().unwrap().as_str().unwrap().to_string()
}

fn comps(run: &Run<'_>) -> Value {
    serde_json::from_slice(run.document_entry("composition").unwrap()).unwrap()
}

fn motion(run: &Run<'_>) -> Value {
    serde_json::from_slice(run.document_entry("motion").unwrap()).unwrap()
}

fn layer(run: &Run<'_>, id: &str) -> Value {
    let c = comps(run);
    for comp in c["compositions"].as_array().unwrap() {
        for l in comp["layers"].as_array().into_iter().flatten() {
            if l["layer_id"] == id {
                return l.clone();
            }
        }
    }
    panic!("layer {id} is in no composition: {c}");
}

fn new_comp(run: &mut Run<'_>, id: &str) -> Value {
    cmd(run, "composition.new", json!({"composition_id": id, "name": id, "width": 64, "height": 32, "frame_rate": {"numerator": 25, "denominator": 1}, "duration": 12 * S, "background": {"r": 0, "g": 0, "b": 0, "a": 1, "colour_profile_id": null}}))
}

fn with_open(f: impl FnOnce(&mut Run<'_>)) {
    let h = harness();
    let cancel = CancellationToken::default();
    let mut run = h.run(asset_local_test_context(), &cancel);
    assert!(run.open_bytes(FIXTURE));
    f(&mut run);
}

/// Saves through a checkpoint, reopens, and returns the reopened run's records.
fn survives_reopen(run: &mut Run<'_>) {
    let want = (comps(run), run.document_entry("motion").map(|m| serde_json::from_slice::<Value>(m).unwrap()));
    let bytes = run.checkpoint().expect("checkpoint");
    let h = harness();
    let cancel = CancellationToken::default();
    let mut again = h.run(asset_local_test_context(), &cancel);
    assert!(again.open_bytes(&bytes), "{:?}", again.ops().last());
    assert_eq!(comps(&again), want.0);
    assert_eq!(again.document_entry("motion").map(|m| serde_json::from_slice::<Value>(m).unwrap()), want.1);
    // A reopened record set still validates, so the next command reads it.
    assert!(again.motion_command("composition.read", &json!({})), "{:?}", again.ops().last());
}

#[test]
fn rows_carry_the_spec_ids_and_either_spelling_runs() {
    let rows: Vec<_> = RUN_COMMANDS.iter().filter(|c| c.run_method == "motion_command").collect();
    assert!(rows.len() >= 50, "{}", rows.len());
    for c in &rows {
        assert!(!c.recordable, "{}", c.command_id);
        assert!(c.command_id.len() <= 64 && c.command_id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b)), "RUN_COMMANDS grammar: {}", c.command_id);
        assert_eq!(row(&spec_id(c.command_id)).unwrap().command_id, c.command_id);
        assert!(action::command(c.command_id).is_some());
    }
    assert_eq!(spec_id("composition.new"), "STUDIO_COMPOSITION_NEW");
    assert_eq!(spec_id("keyframe.add_at_current"), "STUDIO_KEYFRAME_ADD_AT_CURRENT");
    assert_eq!(spec_id("property.set_value"), "STUDIO_PROPERTY_SET_VALUE");
    assert_eq!(spec_id("expression.set_source"), "STUDIO_EXPRESSION_SET_SOURCE");
    with_open(|run| {
        cmd(run, "STUDIO_COMPOSITION_NEW", json!({"composition_id": "T01_MAIN", "name": "T01_MAIN", "width": 1920, "height": 1080, "frame_rate": {"numerator": 25, "denominator": 1}, "duration": 12 * S}));
        assert_eq!(last_label(run), "New Composition");
        assert_eq!(run.ops().last().unwrap()["command_id"], "composition.new");
    });
}

#[test]
fn step_10_11_new_compositions_are_named_attributed_undoable_and_saved() {
    with_open(|run| {
        let d = new_comp(run, "T01_MAIN");
        assert_eq!(d["composition_id"], "T01_MAIN");
        // A minted id is a valid SCOM identity.
        let d = cmd(run, "composition.new", json!({"name": "V02_SCREEN", "width": 1920, "height": 1080, "frame_rate": {"numerator": 25, "denominator": 1}, "duration": 6 * S}));
        assert!(d["composition_id"].as_str().unwrap().starts_with("SCOM-"), "{d}");
        assert_eq!(comps(run)["compositions"].as_array().unwrap().len(), 2);
        let h = run.history();
        assert_eq!(h["undo"], json!(["New Composition", "New Composition"]));
        let step = h["undo_steps"].as_array().unwrap().last().unwrap().clone();
        assert_eq!(step["command_id"], "composition.new");
        assert_eq!(step["client_session"], "asset-local-test-host");
        assert_eq!(step["actor"]["principal_id"], "principal", "{step}");
        survives_reopen(run);
        assert!(run.undo());
        assert_eq!(comps(run)["compositions"].as_array().unwrap().len(), 1);
        assert!(run.undo());
        assert!(run.document_entry("composition").is_none(), "undo restores the absent entry");
        assert!(run.redo());
        assert!(run.redo());
        assert_eq!(comps(run)["compositions"].as_array().unwrap().len(), 2);
        // Typed refusals: Folio's bounds, duplicate id, missing keys.
        assert_eq!(refused(run, "composition.new", json!({"composition_id": "T01_MAIN", "name": "x", "width": 64, "height": 32, "frame_rate": {"numerator": 25, "denominator": 1}, "duration": S})), "duplicate_identity");
        assert_eq!(refused(run, "composition.new", json!({"composition_id": "bad", "name": "x", "width": 1, "height": 32, "frame_rate": {"numerator": 25, "denominator": 1}, "duration": S})), "invalid_field");
        assert_eq!(run.ops().last().unwrap()["target"], "bad/size");
        assert_eq!(refused(run, "composition.new", json!({"composition_id": "c", "width": 64})), "parameter_missing");
        // Settings patch.
        cmd(run, "composition.set_settings", json!({"composition_id": "T01_MAIN", "name": "renamed", "width": 100}));
        assert_eq!(last_label(run), "Composition Settings");
        assert_eq!(comps(run)["compositions"][0]["width"], 100);
        assert_eq!(refused(run, "composition.set_settings", json!({"composition_id": "T01_MAIN", "name": "renamed"})), "nothing_to_change");
        assert_eq!(refused(run, "composition.set_settings", json!({"composition_id": "none", "name": "n"})), "composition_not_found");
    });
}

#[test]
fn step_33_38_40_55_layer_kinds_parent_matte_and_switches() {
    with_open(|run| {
        new_comp(run, "main");
        new_comp(run, "nested");
        let revision = run.snapshot().unwrap().document().revision;
        // Existing document layer (step 55: one per frame with its window).
        cmd(run, "composition.add_layer", json!({"composition_id": "main", "kind": "existing", "layer_id": L2, "in_point": F, "out_point": 2 * F, "start_time": F}));
        assert_eq!(layer(run, L2)["timing"]["in_point"], F);
        assert_eq!(run.snapshot().unwrap().document().revision, revision, "an entry-only step");
        // New kinds create their document layer in the same undo step.
        let ctrl = cmd(run, "composition.add_layer", json!({"composition_id": "main", "kind": "null", "name": "CTRL"}))["layer_id"].as_str().unwrap().to_string();
        assert_eq!(last_label(run), "New Layer");
        let doc = run.snapshot().unwrap().document();
        assert!(doc.revision > revision);
        assert!(doc.layers.iter().any(|l| l.layer_id == ctrl && l.name == "CTRL"));
        let depth = undo_len(run);
        let shape = cmd(run, "composition.add_layer", json!({"composition_id": "main", "kind": "shape", "root": {"items": [{"kind": "ellipse", "position": [8.0, 8.0], "size": [16.0, 16.0]}, {"kind": "stroke", "rgba": [1.0, 1.0, 1.0, 1.0], "opacity": 100.0, "style": {"width": 2.0, "cap": "butt", "join": "miter", "miter_limit": 4.0}}]}}))["layer_id"].as_str().unwrap().to_string();
        assert_eq!(undo_len(run), depth + 1, "layer, composition and motion records are one step");
        assert_eq!(motion(run)["shape_layers"][0]["layer_id"], shape);
        // Undo the shape: document layer, composition layer and shape record all go; redo returns them.
        assert!(run.undo());
        assert!(run.snapshot().unwrap().document().layers.iter().all(|l| l.layer_id != shape));
        assert!(run.document_entry("motion").is_none());
        assert_eq!(undo_len(run), depth);
        assert!(run.redo());
        assert!(run.snapshot().unwrap().document().layers.iter().any(|l| l.layer_id == shape));
        assert_eq!(motion(run)["shape_layers"][0]["layer_id"], shape);
        assert_eq!(undo_len(run), depth + 1);
        let camera = cmd(run, "composition.add_layer", json!({"composition_id": "main", "kind": "camera", "name": "Cam"}))["layer_id"].as_str().unwrap().to_string();
        assert!(layer(run, &camera)["camera"]["zoom"]["static_value"][0].as_f64().unwrap() > 1.0);
        assert!(layer(run, &camera)["three_d"].is_object());
        let light = cmd(run, "composition.add_layer", json!({"composition_id": "main", "kind": "light", "options": {"light_type": "spot", "casts_shadows": true}}))["layer_id"].as_str().unwrap().to_string();
        assert_eq!(layer(run, &light)["light"]["light_type"], "spot");
        let pre = cmd(run, "composition.add_layer", json!({"composition_id": "main", "kind": "precomp", "source_composition_id": "nested"}))["layer_id"].as_str().unwrap().to_string();
        assert_eq!(layer(run, &pre)["precomp"]["composition_id"], "nested");
        assert_eq!(refused(run, "composition.add_layer", json!({"composition_id": "main", "kind": "precomp", "source_composition_id": "missing"})), "dangling_reference");
        // Lane SMB-MOTION-COMMANDS-2 made `solid` a kind (its colour is required); footage is not.
        assert_eq!(refused(run, "composition.add_layer", json!({"composition_id": "main", "kind": "solid"})), "parameter_missing");
        assert_eq!(refused(run, "composition.add_layer", json!({"composition_id": "main", "kind": "footage"})), "layer_kind_unsupported");
        assert_eq!(refused(run, "composition.add_layer", json!({"composition_id": "main", "kind": "existing", "layer_id": L2})), "duplicate_identity");
        assert_eq!(refused(run, "composition.add_layer", json!({"composition_id": "main", "kind": "null", "at": 999})), "parameter_invalid");
        // Parent (step 33) and its typed cycle refusal.
        cmd(run, "composition.set_parent", json!({"layer_id": L2, "parent": ctrl}));
        assert_eq!(last_label(run), "Set Parent");
        assert_eq!(layer(run, L2)["parent"], ctrl.as_str());
        assert_eq!(refused(run, "composition.set_parent", json!({"layer_id": ctrl, "parent": L2})), "containment_cycle");
        cmd(run, "composition.set_parent", json!({"layer_id": L2, "parent": Value::Null}));
        assert!(layer(run, L2).get("parent").is_none());
        // Track matte (step 40).
        cmd(run, "composition.set_track_matte", json!({"layer_id": L2, "source_layer_id": shape, "mode": "alpha", "inverted": true}));
        assert_eq!(layer(run, L2)["track_matte"], json!({"source_layer_id": shape, "mode": "alpha", "inverted": true}));
        assert_eq!(refused(run, "composition.set_track_matte", json!({"layer_id": L2, "source_layer_id": L2, "mode": "alpha"})), "invalid_field");
        cmd(run, "composition.set_track_matte", json!({"layer_id": L2, "source_layer_id": Value::Null}));
        assert!(layer(run, L2).get("track_matte").is_none());
        // Switches (steps 39, 45, 49).
        cmd(run, "composition.set_switches", json!({"layer_id": pre, "collapse_transformations": true}));
        assert_eq!(layer(run, &pre)["precomp"]["collapse_transformations"], true);
        assert_eq!(refused(run, "composition.set_switches", json!({"layer_id": L2, "collapse_transformations": true})), "layer_not_precomp");
        cmd(run, "composition.set_switches", json!({"layer_id": L2, "three_d": true, "frame_blending": "frame_mix", "motion_blur": true}));
        assert_eq!(last_label(run), "Layer Switches");
        assert!(layer(run, L2)["three_d"].is_object());
        assert_eq!(layer(run, L2)["frame_blending"], "frame_mix");
        assert_eq!(motion(run)["composition_settings"][0]["motion_blur_layers"], json!([L2]));
        assert_eq!(refused(run, "composition.set_switches", json!({"layer_id": camera, "three_d": false})), "invalid_field");
        // Timing, order, duplicate, remove.
        cmd(run, "composition.set_layer_timing", json!({"layer_id": L2, "out_point": 3 * F, "stretch": {"numerator": 2, "denominator": 1}}));
        assert_eq!(layer(run, L2)["timing"]["stretch"]["numerator"], 2);
        assert_eq!(refused(run, "composition.set_layer_timing", json!({"layer_id": L2, "out_point": 0})), "invalid_field");
        cmd(run, "composition.reorder_layer", json!({"layer_id": L2, "index": 0}));
        assert_eq!(comps(run)["compositions"][0]["layers"][0]["layer_id"], L2);
        let dup = cmd(run, "composition.duplicate_layer", json!({"layer_id": shape}))["layer_id"].as_str().unwrap().to_string();
        assert_eq!(motion(run)["shape_layers"].as_array().unwrap().len(), 2, "{dup}");
        assert_eq!(refused(run, "composition.duplicate_layer", json!({"layer_id": L2})), "layer_duplicate_unsupported");
        // The copy's document layer keeps the source's visibility and appearance (review F2).
        assert!(run.set_visible(&ctrl, false, None));
        assert!(run.set_opacity(&ctrl, 40.0, None));
        let copy = cmd(run, "composition.duplicate_layer", json!({"layer_id": ctrl, "name": "CTRL 2"}))["layer_id"].as_str().unwrap().to_string();
        let l = run.snapshot().unwrap().document().layers.iter().find(|l| l.layer_id == copy).unwrap().clone();
        assert_eq!((l.visible, l.effective_appearance().opacity, l.name.as_str(), l.kind.as_str()), (false, 40.0, "CTRL 2", "group"));
        cmd(run, "composition.remove_layer", json!({"layer_id": shape}));
        assert_eq!(motion(run)["shape_layers"].as_array().unwrap().len(), 1);
        assert!(run.undo());
        assert_eq!(motion(run)["shape_layers"].as_array().unwrap().len(), 2);
        survives_reopen(run);
        // Delete: a composition another one nests is refused.
        assert_eq!(refused(run, "composition.delete", json!({"composition_id": "nested"})), "dangling_reference");
        let _ = L1;
    });
}

/// The value of `property` on `layer_id` at `time`.
fn value_at(run: &mut Run<'_>, layer_id: &str, property: &str, time: u64) -> Vec<f64> {
    serde_json::from_value(cmd(run, "property.read", json!({"layer_id": layer_id, "property": property, "time": time}))["value_at"].clone()).unwrap()
}

#[test]
fn step_26_keyframe_operations_are_named_and_add_at_current_never_changes_the_value() {
    with_open(|run| {
        new_comp(run, "main");
        cmd(run, "composition.add_layer", json!({"composition_id": "main", "kind": "existing", "layer_id": L2}));
        // A property with no stream and no knowable default needs a value (position is content-relative).
        assert_eq!(refused(run, "keyframe.add_at_current", json!({"layer_id": L2, "property": "position", "time": 0})), "property_default_unknown");
        // A value seeds only a property without an implied value; elsewhere it is refused, never
        // ignored and never applied (review F4).
        assert_eq!(refused(run, "keyframe.add_at_current", json!({"layer_id": L2, "property": "opacity", "time": 0, "value": [50.0]})), "parameter_invalid");
        assert_eq!(run.ops().last().unwrap()["target"], "value");
        // Enable animation: first key at the time, static retained.
        cmd(run, "property.set_value", json!({"layer_id": L2, "property": "opacity", "value": [40.0]}));
        assert_eq!(last_label(run), "Set Property Value");
        cmd(run, "property.enable_animation", json!({"layer_id": L2, "property": "opacity", "time": S}));
        assert_eq!(last_label(run), "Enable Animation");
        assert_eq!(layer(run, L2)["streams"][0]["keyframes"][0]["value"], json!([40.0]));
        // Set: a typed key; out of range is Folio's refusal.
        cmd(run, "keyframe.set", json!({"layer_id": L2, "property": "opacity", "time": 3 * S, "value": [100.0]}));
        assert_eq!(last_label(run), "Set Keyframe");
        assert_eq!(refused(run, "keyframe.set", json!({"layer_id": L2, "property": "opacity", "time": 4 * S, "value": [101.0]})), "invalid_field");
        assert_eq!(refused(run, "keyframe.set", json!({"layer_id": L2, "property": "opacity", "time": u64::MAX, "value": [1.0]})), "invalid_field");
        assert_eq!(refused(run, "keyframe.set", json!({"layer_id": L2, "property": "opacity", "time": 4 * S, "value": [1.0, 2.0]})), "invalid_field");
        assert_eq!(refused(run, "keyframe.set", json!({"layer_id": L2, "property": "nonsense", "time": 0, "value": [1.0]})), "property_not_found");
        // Add at current on the linear segment: the lerped value, nothing else moves.
        let before = value_at(run, L2, "opacity", 2 * S);
        assert!((before[0] - 70.0).abs() < 1e-9, "{before:?}");
        cmd(run, "keyframe.add_at_current", json!({"layer_id": L2, "property": "opacity", "time": 2 * S}));
        assert_eq!(last_label(run), "Add Keyframe at Current Time");
        assert_eq!(layer(run, L2)["streams"][0]["keyframes"].as_array().unwrap().len(), 3);
        assert_eq!(refused(run, "keyframe.add_at_current", json!({"layer_id": L2, "property": "opacity", "time": 2 * S})), "keyframe_exists");
        // Toggle deletes it, toggling again adds it back.
        assert_eq!(cmd(run, "keyframe.toggle_at_current", json!({"layer_id": L2, "property": "opacity", "time": 2 * S}))["toggled"], "deleted");
        assert_eq!(cmd(run, "keyframe.toggle_at_current", json!({"layer_id": L2, "property": "opacity", "time": 2 * S}))["toggled"], "added");
        // Move, shift, delete.
        cmd(run, "keyframe.move", json!({"layer_id": L2, "property": "opacity", "time": 2 * S, "to": 2 * S + F}));
        assert_eq!(refused(run, "keyframe.move", json!({"layer_id": L2, "property": "opacity", "time": 2 * S + F, "to": 3 * S})), "keyframe_exists");
        assert_eq!(refused(run, "keyframe.move", json!({"layer_id": L2, "property": "opacity", "time": 7, "to": 8})), "keyframe_not_found");
        cmd(run, "keyframe.shift", json!({"layer_id": L2, "property": "opacity", "frames": 10}));
        assert_eq!(layer(run, L2)["streams"][0]["keyframes"][0]["time"], S + 10 * F);
        cmd(run, "keyframe.shift", json!({"layer_id": L2, "property": "opacity", "frames": -10, "times": [S + 10 * F]}));
        assert_eq!(layer(run, L2)["streams"][0]["keyframes"][0]["time"], S);
        assert_eq!(refused(run, "keyframe.shift", json!({"layer_id": L2, "property": "opacity", "frames": -1000})), "tick_overflow");
        assert_eq!(refused(run, "keyframe.shift", json!({"layer_id": L2, "property": "opacity", "frames": i64::MIN})), "tick_overflow");
        cmd(run, "keyframe.delete", json!({"layer_id": L2, "property": "opacity", "time": 2 * S + 11 * F}));
        // Interpolation: bezier gets the STU-MOT-034a default ease when no tangent is given.
        cmd(run, "keyframe.set_interpolation", json!({"layer_id": L2, "property": "opacity", "time": S, "out_interp": "bezier"}));
        assert_eq!(last_label(run), "Set Keyframe Interpolation");
        let key = layer(run, L2)["streams"][0]["keyframes"][0].clone();
        assert_eq!(key["out_interp"], "bezier");
        assert_eq!(key["out_tangent"][0]["speed"], 0.0);
        // Disable: keys gone, the prior static value is back (STU-MOT-020b).
        cmd(run, "property.disable_animation", json!({"layer_id": L2, "property": "opacity"}));
        assert_eq!(last_label(run), "Disable Animation");
        assert_eq!(value_at(run, L2, "opacity", 3 * S), vec![40.0]);
        assert_eq!(refused(run, "property.disable_animation", json!({"layer_id": L2, "property": "opacity"})), "nothing_to_change");
        // R60 attempt-2 probe (SMB-MOTION-COMMANDS-2): a caller's value never seeds a key at the
        // current time, even where the implied value is unknown (STU-MOT-038).
        assert_eq!(refused(run, "keyframe.add_at_current", json!({"layer_id": L2, "property": "anchor", "time": 0, "value": [1.0, 2.0]})), "parameter_invalid");
        assert_eq!(refused(run, "keyframe.add_at_current", json!({"layer_id": L2, "property": "anchor", "time": 0})), "property_default_unknown");
        cmd(run, "keyframe.set", json!({"layer_id": L2, "property": "anchor", "time": 0, "value": [1.0, 2.0]}));
        assert_eq!(value_at(run, L2, "anchor", 5 * S), vec![1.0, 2.0]);
        assert_eq!(refused(run, "keyframe.toggle_at_current", json!({"layer_id": L2, "property": "anchor", "time": S, "value": [3.0, 3.0]})), "parameter_invalid");
        survives_reopen(run);
    });
}

#[test]
fn add_at_current_splits_bezier_hold_and_ends_without_moving_any_sample() {
    with_open(|run| {
        new_comp(run, "main");
        cmd(run, "composition.add_layer", json!({"composition_id": "main", "kind": "existing", "layer_id": L2}));
        let p = "position";
        let ease = |speed: f64, influence: f64| json!([{"speed": speed, "influence": influence}, {"speed": -speed, "influence": influence / 2.0}]);
        cmd(run, "keyframe.set", json!({"layer_id": L2, "property": p, "time": S, "value": [0.0, 10.0], "out_interp": "bezier", "out_tangent": ease(30.0, 0.4)}));
        cmd(run, "keyframe.set", json!({"layer_id": L2, "property": p, "time": 3 * S, "value": [100.0, -20.0], "in_interp": "bezier", "in_tangent": ease(-10.0, 0.7), "out_interp": "hold"}));
        cmd(run, "keyframe.set", json!({"layer_id": L2, "property": p, "time": 4 * S, "value": [50.0, 50.0], "in_interp": "linear", "out_interp": "bezier", "out_tangent": ease(5.0, 0.3)}));
        let ticks: Vec<u64> = (0..=240).map(|i| i * S / 40).collect();
        let sample = |run: &mut Run<'_>| ticks.iter().map(|t| value_at(run, L2, p, *t)).collect::<Vec<_>>();
        let want = sample(run);
        // Mid-bezier, mid-hold, before the first and after the last key whose out side is bezier.
        for t in [S + S / 3, 2 * S + 7, 3 * S + S / 2, S / 2, 5 * S] {
            cmd(run, "keyframe.add_at_current", json!({"layer_id": L2, "property": p, "time": t}));
            let got = sample(run);
            for (i, (a, b)) in want.iter().zip(&got).enumerate() {
                for c in 0..2 {
                    assert!((a[c] - b[c]).abs() < 1e-6, "add at {t}: tick {} component {c}: {} vs {}", ticks[i], a[c], b[c]);
                }
            }
        }
        survives_reopen(run);
    });
}

#[test]
fn step_18_22_25_41_46_52_61_every_property_address_keys() {
    with_open(|run| {
        new_comp(run, "main");
        new_comp(run, "nested");
        cmd(run, "composition.add_layer", json!({"composition_id": "main", "kind": "existing", "layer_id": L2}));
        // Effects (steps 18, 43, 72).
        let fx = cmd(run, "composition.add_effect", json!({"layer_id": L2, "filter_kind": "gaussian_blur"}))["effect_id"].as_str().unwrap().to_string();
        assert_eq!(last_label(run), "Add Effect");
        cmd(run, "composition.add_effect", json!({"layer_id": L2, "effect_id": "glow", "filter_kind": "glow", "at": 0}));
        assert_eq!(layer(run, L2)["effects"][0]["effect_id"], "glow");
        assert_eq!(refused(run, "composition.add_effect", json!({"layer_id": L2, "effect_id": "glow", "filter_kind": "glow"})), "duplicate_identity");
        assert_eq!(refused(run, "composition.add_effect", json!({"layer_id": L2, "filter_kind": "plasma"})), "parameter_invalid");
        let ramp = cmd(run, "composition.add_effect", json!({"layer_id": L2, "filter_kind": "gradient_ramp"}))["effect_id"].as_str().unwrap().to_string();
        let param = format!("effect.{ramp}.end_of_ramp");
        cmd(run, "keyframe.set", json!({"layer_id": L2, "property": param, "time": 0, "value": [1.0, 1.0]}));
        cmd(run, "keyframe.set", json!({"layer_id": L2, "property": param, "time": 12 * S, "value": [0.5, 1.0]}));
        assert_eq!(value_at(run, L2, &param, 6 * S), vec![0.75, 1.0]);
        assert_eq!(refused(run, "property.set_value", json!({"layer_id": L2, "property": format!("effect.{ramp}.nope"), "value": [1.0]})), "property_not_found");
        cmd(run, "composition.set_effect", json!({"layer_id": L2, "effect_id": fx, "enabled": false, "opacity": 50.0}));
        assert_eq!(layer(run, L2)["effects"][1]["opacity"], 50.0);
        cmd(run, "composition.remove_effect", json!({"layer_id": L2, "effect_id": fx}));
        // Masks (steps 41, 66).
        let m = cmd(run, "composition.add_mask", json!({"layer_id": L2, "mode": "subtract", "inverted": true, "path": [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 10.0, 0.0, 0.0, 0.0, 0.0, 0.0, 10.0, 10.0, 0.0, 0.0, 0.0, 0.0]}))["mask_id"].as_str().unwrap().to_string();
        assert_eq!(last_label(run), "New Mask");
        cmd(run, "property.set_value", json!({"layer_id": L2, "property": format!("mask.{m}.feather"), "value": [40.0, 40.0]}));
        cmd(run, "keyframe.add_at_current", json!({"layer_id": L2, "property": format!("mask.{m}.path"), "time": S}));
        cmd(run, "composition.set_mask", json!({"layer_id": L2, "mask_id": m, "mode": "add", "inverted": false}));
        assert_eq!(layer(run, L2)["masks"][0]["mode"], "add");
        assert_eq!(refused(run, "composition.add_mask", json!({"layer_id": L2, "path": [1.0]})), "invalid_field");
        // 3D, camera, light (steps 45, 46, 48, 71).
        assert_eq!(refused(run, "property.set_value", json!({"layer_id": L2, "property": "three_d.position_z", "value": [5.0]})), "layer_not_3d");
        cmd(run, "composition.set_switches", json!({"layer_id": L2, "three_d": true}));
        cmd(run, "keyframe.set", json!({"layer_id": L2, "property": "three_d.rotation_y", "time": 0, "value": [30.0]}));
        cmd(run, "composition.set_material", json!({"layer_id": L2, "casts_shadows": true}));
        cmd(run, "property.set_value", json!({"layer_id": L2, "property": "three_d.material.diffuse", "value": [75.0]}));
        assert_eq!(layer(run, L2)["three_d"]["material"]["casts_shadows"], true);
        let cam = cmd(run, "composition.add_layer", json!({"composition_id": "main", "kind": "camera"}))["layer_id"].as_str().unwrap().to_string();
        cmd(run, "composition.set_camera", json!({"layer_id": cam, "depth_of_field": true, "iris_shape": "hexagon"}));
        cmd(run, "keyframe.set", json!({"layer_id": cam, "property": "camera.focus_distance", "time": 0, "value": [800.0]}));
        cmd(run, "keyframe.add_at_current", json!({"layer_id": cam, "property": "camera.zoom", "time": S}));
        assert_eq!(refused(run, "composition.set_camera", json!({"layer_id": L2, "depth_of_field": true})), "layer_not_camera");
        let light = cmd(run, "composition.add_layer", json!({"composition_id": "main", "kind": "light"}))["layer_id"].as_str().unwrap().to_string();
        cmd(run, "composition.set_light", json!({"layer_id": light, "light_type": "spot", "falloff_type": "smooth"}));
        cmd(run, "keyframe.set", json!({"layer_id": light, "property": "light.intensity", "time": 0, "value": [150.0]}));
        cmd(run, "property.set_value", json!({"layer_id": light, "property": "light.cone_angle", "value": [45.0]}));
        // Time remap (step 52): precomp source seconds per composition tick.
        let pre = cmd(run, "composition.add_layer", json!({"composition_id": "main", "kind": "precomp", "source_composition_id": "nested"}))["layer_id"].as_str().unwrap().to_string();
        cmd(run, "composition.set_time_remap", json!({"layer_id": pre, "enabled": true}));
        assert_eq!(last_label(run), "Enable Time Remapping");
        assert_eq!(value_at(run, &pre, "time_remap", 6 * S), vec![6.0]);
        cmd(run, "keyframe.set", json!({"layer_id": pre, "property": "time_remap", "time": 9 * S, "value": [5.5]}));
        // Corner pin (step 61).
        cmd(run, "composition.set_corner_pin", json!({"layer_id": pre, "from": [[0.0, 0.0], [64.0, 0.0], [64.0, 32.0], [0.0, 32.0]], "to": [10.0, 0.0, 60.0, 5.0, 60.0, 30.0, 5.0, 25.0]}));
        cmd(run, "keyframe.set", json!({"layer_id": pre, "property": "corner_pin", "time": S, "value": [11.0, 0.0, 61.0, 5.0, 61.0, 30.0, 6.0, 25.0]}));
        assert_eq!(refused(run, "composition.set_corner_pin", json!({"layer_id": L2, "from": [[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]]})), "invalid_field");
        // Roto strokes (step 67).
        cmd(run, "composition.add_roto_stroke", json!({"layer_id": pre, "source_frames": 150, "stroke": {"frame": 10, "polarity": "foreground", "diameter": 8.0, "path": [[1.0, 1.0], [2.0, 2.0]]}}));
        assert_eq!(layer(run, &pre)["roto"]["strokes"].as_array().unwrap().len(), 1);
        // Shape items and streams (steps 24, 25).
        let shape = cmd(run, "composition.add_layer", json!({"composition_id": "main", "kind": "shape"}))["layer_id"].as_str().unwrap().to_string();
        for item in [json!({"kind": "polystar", "star": true, "points": 5.0, "position": [8.0, 8.0], "outer_radius": 8.0}), json!({"kind": "stroke", "rgba": [1.0, 1.0, 1.0, 1.0], "opacity": 100.0, "style": {"width": 2.0, "cap": "butt", "join": "miter", "miter_limit": 4.0}}), json!({"kind": "trim", "start": 0.0, "end": 100.0, "mode": "simultaneously"}), json!({"kind": "group", "group": {"items": []}})] {
            cmd(run, "composition.add_shape_item", json!({"layer_id": shape, "item": item}));
        }
        assert_eq!(last_label(run), "Add Shape Item");
        cmd(run, "composition.add_shape_item", json!({"layer_id": shape, "group_path": [3], "item": {"kind": "rect", "position": [0.0, 0.0], "size": [4.0, 4.0]}}));
        assert_eq!(motion(run)["shape_layers"][0]["root"]["items"][3]["group"]["items"][0]["kind"], "rect");
        cmd(run, "keyframe.set", json!({"layer_id": shape, "property": "shape.2.trim_end", "time": 0, "value": [0.0]}));
        cmd(run, "keyframe.add_at_current", json!({"layer_id": shape, "property": "shape.3.0.rect_size", "time": S}));
        cmd(run, "property.set_value", json!({"layer_id": shape, "property": "shape.group_rotation", "value": [10.0]}));
        assert_eq!(motion(run)["shape_layers"][0]["streams"].as_array().unwrap().len(), 3);
        assert_eq!(refused(run, "keyframe.set", json!({"layer_id": shape, "property": "shape.0.trim_end", "time": 0, "value": [0.0]})), "property_not_found");
        assert_eq!(refused(run, "composition.remove_shape_item", json!({"layer_id": shape, "path": [2]})), "shape_item_animated");
        cmd(run, "composition.set_shape_item", json!({"layer_id": shape, "path": [0], "item": {"kind": "ellipse", "position": [0.0, 0.0], "size": [1.0, 1.0]}}));
        cmd(run, "composition.remove_shape_item", json!({"layer_id": shape, "path": [1]}));
        assert_eq!(motion(run)["shape_layers"][0]["streams"][0]["path"], json!([1]), "stream paths follow a removed item");
        // Text animators and selector streams (steps 21, 22).
        cmd(run, "composition.add_text_animator", json!({"layer_id": L2, "name": "A", "properties": {"opacity": 0.0, "position": [0.0, 60.0]}, "selectors": [{"kind": "range", "start": 0.0, "end": 100.0, "index_units": false, "based_on": "characters", "shape": "square", "smoothness": 100.0, "amount": 100.0}]}));
        cmd(run, "keyframe.set", json!({"layer_id": L2, "property": "text.0.0.range_offset", "time": 2 * S, "value": [-100.0]}));
        cmd(run, "keyframe.set", json!({"layer_id": L2, "property": "text.0.0.range_offset", "time": 2 * S + S + S / 2, "value": [100.0]}));
        assert_eq!(value_at(run, L2, "text.0.0.range_offset", 2 * S + S / 2 + S / 4), vec![0.0]);
        cmd(run, "keyframe.add_at_current", json!({"layer_id": L2, "property": "text.0.opacity", "time": S}));
        assert_eq!(refused(run, "keyframe.set", json!({"layer_id": L2, "property": "text.0.1.range_offset", "time": 0, "value": [0.0]})), "property_not_found");
        survives_reopen(run);
    });
}

#[test]
fn step_8_13_49_79_to_82_motion_records_commands() {
    with_open(|run| {
        new_comp(run, "main");
        cmd(run, "composition.add_layer", json!({"composition_id": "main", "kind": "existing", "layer_id": L2}));
        // Step 13.
        cmd(run, "composition.set_work_area", json!({"composition_id": "main", "start_tick": 0, "end_tick": 10 * S}));
        assert_eq!(last_label(run), "Set Work Area");
        assert_eq!(motion(run)["composition_work_areas"][0]["end_tick"], 10 * S);
        assert_eq!(refused(run, "composition.set_work_area", json!({"composition_id": "main", "start_tick": 0, "end_tick": 13 * S})), "invalid_field");
        // Step 8.
        cmd(run, "composition.add_audio_layer", json!({"composition_id": "main", "audio_id": "music", "start_tick": 0, "gain": 1.0}));
        assert_eq!(motion(run)["composition_audio"][0]["layers"][0]["audio_id"], "music");
        assert_eq!(refused(run, "composition.add_audio_layer", json!({"composition_id": "main", "audio_id": "bad id", "start_tick": 0, "gain": 1.0})), "invalid_field");
        assert_eq!(refused(run, "composition.remove_audio_layer", json!({"composition_id": "main", "audio_id": "none"})), "audio_layer_not_found");
        // Steps 49, 75.
        cmd(run, "composition.set_motion_blur", json!({"composition_id": "main", "motion_blur": true, "shutter_angle": 180.0, "samples_per_frame": 16}));
        assert_eq!(motion(run)["composition_settings"][0]["adaptive_sample_limit"], 128);
        assert_eq!(refused(run, "composition.set_motion_blur", json!({"composition_id": "main", "samples_per_frame": 1})), "invalid_field");
        // Steps 79 to 82.
        cmd(run, "composition.queue_add", json!({"item_id": "rq-title", "composition_id": "main", "module_id": "master", "format": "mov_vc3", "output_path": "renders/master.mov"}));
        assert_eq!(last_label(run), "Add Composition to Render Queue");
        assert_eq!(refused(run, "composition.queue_add", json!({"item_id": "rq-title", "composition_id": "main"})), "duplicate_identity");
        assert_eq!(refused(run, "composition.queue_add", json!({"item_id": "x", "composition_id": "none"})), "dangling_reference");
        cmd(run, "composition.queue_set_settings", json!({"item_id": "rq-title", "quality": "best", "resolution": "full", "color_depth": "16_bits_per_channel", "time_span": "work_area_only", "motion_blur": "on_for_checked_layers", "frame_blending": "on_for_checked_layers"}));
        assert_eq!(last_label(run), "Render Settings");
        let item = motion(run)["render_queue"][0].clone();
        assert_eq!(item["settings"]["time_span"], "work_area_only");
        assert_eq!(item["settings"]["quality"], "best");
        assert_eq!(refused(run, "composition.queue_set_settings", json!({"item_id": "rq-title", "quality": "ultra"})), "parameter_invalid");
        assert_eq!(refused(run, "composition.queue_set_settings", json!({"item_id": "rq-title", "time_span": {"custom": {"start_tick": 5, "end_tick": 1}}})), "invalid_field");
        cmd(run, "composition.queue_set_module", json!({"item_id": "rq-title", "module_id": "master", "codec_option": "dnxhr_hqx", "depth": 10, "audio": {"output": "on", "sample_rate": 48000, "bit_depth": 24, "float": false, "channels": 2}}));
        cmd(run, "composition.queue_add_module", json!({"item_id": "rq-title", "module_id": "proxy", "format": "mp4_av1", "resize": {"width": 32, "height": 16, "lock_aspect": true}}));
        assert_eq!(last_label(run), "Add Output Module");
        let item = motion(run)["render_queue"][0].clone();
        assert_eq!(item["output_modules"].as_array().unwrap().len(), 2);
        assert_eq!(item["output_modules"][0]["codec_option"], "dnxhr_hqx");
        assert_eq!(item["output_modules"][1]["format"], "mp4_av1");
        assert!(item["output_modules"][1]["output_path"].as_str().unwrap().ends_with(".mp4"));
        assert_eq!(refused(run, "composition.queue_add_module", json!({"item_id": "rq-title", "module_id": "bad", "output_path": "../escape.mov"})), "invalid_field");
        cmd(run, "composition.queue_remove_module", json!({"item_id": "rq-title", "module_id": "proxy"}));
        assert_eq!(refused(run, "composition.queue_remove_module", json!({"item_id": "rq-title", "module_id": "master"})), "invalid_field");
        survives_reopen(run);
        // A composition the queue names cannot go; removing the item frees it, and its records go too.
        assert_eq!(refused(run, "composition.delete", json!({"composition_id": "main"})), "dangling_reference");
        cmd(run, "composition.queue_remove", json!({"item_id": "rq-title"}));
        cmd(run, "composition.delete", json!({"composition_id": "main"}));
        assert_eq!(last_label(run), "Delete Composition");
        assert!(comps(run).get("compositions").is_none());
        assert!(run.undo());
        assert_eq!(comps(run)["compositions"][0]["composition_id"], "main");
        assert_eq!(motion(run)["composition_work_areas"][0]["end_tick"], 10 * S);
    });
}

#[test]
fn step_38_precompose_moves_attributes_inside_as_one_step() {
    with_open(|run| {
        new_comp(run, "main");
        let a = cmd(run, "composition.add_layer", json!({"composition_id": "main", "kind": "shape", "name": "a"}))["layer_id"].as_str().unwrap().to_string();
        let b = cmd(run, "composition.add_layer", json!({"composition_id": "main", "kind": "null", "name": "b"}))["layer_id"].as_str().unwrap().to_string();
        let c = cmd(run, "composition.add_layer", json!({"composition_id": "main", "kind": "null", "name": "c"}))["layer_id"].as_str().unwrap().to_string();
        cmd(run, "composition.set_parent", json!({"layer_id": a, "parent": b}));
        cmd(run, "composition.set_switches", json!({"layer_id": a, "motion_blur": true}));
        cmd(run, "keyframe.set", json!({"layer_id": a, "property": "opacity", "time": S, "value": [50.0]}));
        // A selection that would cut a parent link is refused.
        assert_eq!(refused(run, "composition.precompose", json!({"layer_ids": [a], "name": "T02_TEXT"})), "precompose_crosses_selection");
        assert_eq!(refused(run, "composition.precompose", json!({"layer_ids": [], "name": "T02_TEXT"})), "parameter_invalid");
        assert_eq!(refused(run, "composition.precompose", json!({"layer_ids": [a, a], "name": "T02_TEXT"})), "parameter_invalid");
        assert_eq!(refused(run, "composition.precompose", json!({"layer_ids": ["SLYR-019abcde-0000-7000-8000-0000000000ee"], "name": "x"})), "layer_not_in_composition");
        let depth = undo_len(run);
        let d = cmd(run, "composition.precompose", json!({"layer_ids": [a, b], "name": "T02_TEXT", "new_composition_id": "T02_TEXT"}));
        assert_eq!(last_label(run), "Pre-compose");
        assert_eq!(undo_len(run), depth + 1);
        let outer = d["layer_id"].as_str().unwrap().to_string();
        let set = comps(run);
        let main = &set["compositions"][0];
        let ids: Vec<&str> = main["layers"].as_array().unwrap().iter().map(|l| l["layer_id"].as_str().unwrap()).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&outer.as_str()) && ids.contains(&c.as_str()));
        let inner = &set["compositions"][1];
        assert_eq!(inner["composition_id"], "T02_TEXT");
        assert_eq!((inner["width"].clone(), inner["duration"].clone()), (main["width"].clone(), main["duration"].clone()));
        assert_eq!(layer(run, &a)["streams"][0]["keyframes"][0]["value"], json!([50.0]), "attributes moved inside");
        assert_eq!(layer(run, &outer)["precomp"]["composition_id"], "T02_TEXT");
        // The per-layer blur switch follows its layer.
        assert!(motion(run)["composition_settings"].as_array().unwrap().iter().any(|s| s["composition_id"] == "T02_TEXT" && s["motion_blur_layers"] == json!([a])));
        survives_reopen(run);
        assert!(run.undo());
        assert_eq!(comps(run)["compositions"].as_array().unwrap().len(), 1);
        assert_eq!(comps(run)["compositions"][0]["layers"].as_array().unwrap().len(), 3);
        assert!(run.snapshot().unwrap().document().layers.iter().all(|l| l.layer_id != outer));
        assert!(run.redo());
        assert_eq!(comps(run)["compositions"].as_array().unwrap().len(), 2);
    });
}

#[test]
fn the_expression_field_is_a_named_step_and_a_missing_store_is_refused() {
    with_open(|run| {
        new_comp(run, "main");
        cmd(run, "composition.add_layer", json!({"composition_id": "main", "kind": "existing", "layer_id": L2}));
        cmd(run, "composition.add_text_animator", json!({"layer_id": L2, "name": "A", "selectors": [{"kind": "expression", "based_on": "characters", "source": "selectorValue"}, {"kind": "wiggly", "based_on": "words", "max_amount": 100.0, "min_amount": 0.0, "wiggles_per_second": 2.0, "correlation": 50.0, "random_seed": 7}]}));
        cmd(run, "expression.set_source", json!({"layer_id": L2, "animator": 0, "selector": 0, "source": "textIndex / textTotal * 100"}));
        assert_eq!(last_label(run), "Set Expression Source");
        assert_eq!(motion(run)["text_animators"][0]["animators"][0]["selectors"][0]["source"], "textIndex / textTotal * 100");
        assert!(run.undo());
        assert_eq!(motion(run)["text_animators"][0]["animators"][0]["selectors"][0]["source"], "selectorValue");
        assert_eq!(refused(run, "expression.set_source", json!({"layer_id": L2, "animator": 0, "selector": 1, "source": "1"})), "selector_not_expression");
        assert_eq!(refused(run, "expression.set_source", json!({"layer_id": L2, "animator": 9, "selector": 0, "source": "1"})), "property_not_found");
        assert_eq!(refused(run, "expression.set_source", json!({"layer_id": L2, "animator": 0, "selector": 0, "source": ""})), "invalid_field");
        assert_eq!(refused(run, "expression.set_source", json!({"layer_id": L2, "animator": 0, "selector": 0, "source": "x".repeat(65_537)})), "invalid_field");
    });
}

#[test]
fn hostile_parameters_are_typed_refusals_with_bounded_work() {
    let h = harness();
    let cancel = CancellationToken::default();
    let mut closed = h.run(asset_local_test_context(), &cancel);
    assert!(!closed.motion_command("composition.new", &json!({})));
    assert_eq!(closed.ops().last().unwrap()["error"], "not_open");
    with_open(|run| {
        assert_eq!(refused(run, "composition.nope", json!({})), "unknown_command");
        assert_eq!(refused(run, "layer.rename", json!({})), "unknown_command", "only motion rows run here");
        assert_eq!(refused(run, "composition.new", json!([1, 2])), "parameter_invalid");
        assert_eq!(refused(run, "composition.new", json!({"zzz": 1})), "parameter_unknown");
        assert_eq!(refused(run, "composition.new", json!({"name": "x", "width": "wide", "height": 32, "frame_rate": {"numerator": 25, "denominator": 1}, "duration": S})), "parameter_invalid");
        assert_eq!(refused(run, "composition.new", json!({"name": "x", "width": 64, "height": 32, "frame_rate": {"numerator": 1000, "denominator": 1}, "duration": S})), "invalid_field");
        assert_eq!(refused(run, "composition.new", json!({"name": "x", "width": 64, "height": 32, "frame_rate": {"numerator": 25, "denominator": 1}, "duration": u64::MAX})), "invalid_field");
        // A composition record over the ceiling is refused, not stored.
        new_comp(run, "main");
        for n in 1..64 {
            new_comp(run, &format!("c{n}"));
        }
        assert_eq!(refused(run, "composition.new", json!({"name": "x", "width": 64, "height": 32, "frame_rate": {"numerator": 25, "denominator": 1}, "duration": S})), "budget_exceeded");
        // A long property address, a bad path index and a long error target stay bounded.
        cmd(run, "composition.add_layer", json!({"composition_id": "main", "kind": "shape"}));
        let id = comps(run)["compositions"][0]["layers"][0]["layer_id"].as_str().unwrap().to_string();
        assert_eq!(refused(run, "property.set_value", json!({"layer_id": id, "property": "x".repeat(100_000), "value": [1.0]})), "parameter_invalid");
        assert_eq!(refused(run, "property.set_value", json!({"layer_id": id, "property": "shape.99999999999.trim_end", "value": [1.0]})), "property_not_found");
        assert_eq!(refused(run, "composition.remove_layer", json!({"layer_id": "y".repeat(100_000)})), "layer_not_in_composition");
        assert!(run.ops().last().unwrap()["target"].as_str().unwrap().len() <= 256);
        // Deep nesting: a shape group deeper than the Folio limit.
        let mut group = json!({"kind": "group", "group": {"items": []}});
        for _ in 0..40 {
            group = json!({"kind": "group", "group": {"items": [group]}});
        }
        assert_eq!(refused(run, "composition.add_shape_item", json!({"layer_id": id, "item": group})), "budget_exceeded");
        // Text parameters (the JSONL loop of harness/src/cli/drive.rs carries text) decode by the row.
        let row = row("composition.new").unwrap();
        let text: BTreeMap<String, String> = [("name", "text"), ("width", "64"), ("height", "32"), ("frame_rate", r#"{"numerator":25,"denominator":1}"#), ("duration", "254016000000")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        assert!(params_from_text(row, &text).is_ok());
        let mut text = text;
        text.insert("width".into(), "wide".into());
        assert_eq!(params_from_text(row, &text).unwrap_err().0, "parameter_invalid");
        text.insert("unknown".into(), "1".into());
        assert_eq!(params_from_text(row, &text).unwrap_err().0, "parameter_unknown");
    });
    let cancel = CancellationToken::default();
    let mut run = h.run(asset_local_test_context(), &cancel);
    assert!(run.open_bytes(FIXTURE));
    cancel.cancel();
    assert!(!run.motion_command("composition.new", &json!({"name": "x"})));
    assert_eq!(run.ops().last().unwrap()["error"], "canceled");
}

/// One command, one record, one terminal frame: codes and counters only (host integration block).
#[test]
fn a_command_writes_one_record_and_one_terminal_frame() {
    with_open(|run| {
        let before = run.ops().len();
        new_comp(run, "main");
        assert_eq!(run.ops().len(), before + 1);
        let op = run.ops().last().unwrap().clone();
        assert_eq!(op["op"], "composition.new");
        assert_eq!(op["observe"]["frames"], 1, "{op}");
        let before = run.ops().len();
        assert!(!run.motion_command("composition.new", &json!({"name": "x"})));
        assert_eq!(run.ops().len(), before + 1);
        assert_eq!(run.ops().last().unwrap()["observe"]["frames"], 1);
        let before = run.ops().len();
        cmd(run, "composition.add_layer", json!({"composition_id": "main", "kind": "null"}));
        assert_eq!(run.ops().len(), before + 1, "a layer-creating step is one record too");
        assert_eq!(run.ops().last().unwrap()["command_id"], "composition.add_layer");
    });
}

#[cfg(feature = "media")]
mod export {
    //! The scenario's acceptance: "the result reaches save and export". Everything is authored through
    //! commands; only the reading of the output is direct.
    use super::*;
    use hsk_studio_composite::temporal::composition::{CompositionInput, CompositionTemporalSource};
    use hsk_studio_composite::{BlendSpace, RenderInputs, ScenePixels, TileInfo};
    use hsk_studio_folio::{Budget, Resolution, Resolver, TileRef};
    use hsk_studio_harness::view::timeline_export::render_queue::session_render_queue_jobs;
    use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};
    use hsk_studio_pigment::render_plan::{ColourTag, TilePlanes};
    use hsk_studio_reel::*;
    use hsk_studio_session::{Session, SessionConfig, SessionPorts};
    use std::io::Cursor;

    const SPACE: ColourTag = ColourTag { profile_sha256: [0; 32], blend_space: BlendSpace::Encoded };

    struct Available;
    impl Resolver for Available {
        fn primitive(&self, _: &str, _: &str) -> Resolution {
            Resolution::Available
        }
        fn profile(&self, _: &str) -> Resolution {
            Resolution::Available
        }
        fn tile(&self, _: &TileRef) -> Resolution {
            Resolution::Available
        }
        fn format_supported(&self, _: &str) -> bool {
            true
        }
        fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
            false
        }
    }
    struct Quiet;
    impl SinkPort for Quiet {
        fn try_send(&mut self, _: DeliveryClass, _: &[u8]) -> Result<(), DeliveryError> {
            Ok(())
        }
    }
    struct NoPixels;
    impl ScenePixels for NoPixels {
        fn tile(&self, _: &str) -> Option<TilePlanes<'_>> {
            None
        }
    }
    fn info(_: &TileRef) -> Option<TileInfo> {
        None
    }
    fn open(bytes: &[u8], cancel: &CancellationToken) -> Session {
        let mut p = SessionPorts::new(Box::new(Quiet));
        p.resolver = Box::new(Available);
        let context = asset_local_test_context();
        Session::open_bytes(bytes, SessionConfig::new(context.actor().clone()), p, cancel).unwrap()
    }

    #[derive(Default)]
    struct Collect {
        chunks: Vec<(i64, Vec<u8>)>,
    }
    impl ChunkSink for Collect {
        fn accept(&mut self, c: Chunk<'_>) -> Result<(), ReelError> {
            self.chunks.push((c.pts, c.bytes.to_vec()));
            Ok(())
        }
    }
    fn read(bytes: &[u8], count: u64) -> Collect {
        let request = SegmentRequest {
            codec: "isobmff".to_owned(),
            start_pts: hsk_studio_accord::Ticks::new(0),
            count,
            track: Some(1),
            output: OutputMode::Decoded,
            prefer: SourcePreference::Original,
            purpose: Purpose::Export,
            grant: Grant { content_id: ContentId::parse("motion-commands").unwrap(), revision: 1, expected_len: bytes.len() as u64 },
            expected_revision: 1,
        };
        let mut sink = Collect::default();
        Registry::standard().decode_segment(&mut Cursor::new(bytes.to_vec()), None, &request, &Limits::default(), &CancellationToken::default(), &mut sink).expect("reel reads the export");
        sink
    }

    /// Renders queue item `item` of a checkpoint into `root`; the decoded luma planes per frame.
    fn export(bytes: &[u8], item: &str, frames: u64, root: &std::path::Path) -> Vec<Vec<u8>> {
        let cancel = CancellationToken::default();
        let session = open(bytes, &cancel);
        let _ = std::fs::remove_dir_all(root);
        std::fs::create_dir_all(root.join("renders")).unwrap();
        let audio = |_: &str, _: u32| None;
        let (jobs, _) = session_render_queue_jobs(&session, item, &Available, &info, &|| NoPixels, &BTreeMap::new(), &audio, SPACE, root, false, &cancel).unwrap();
        let mut queue = ExportQueue::new();
        for (_, job) in jobs {
            queue.submit(job);
        }
        for r in queue.run() {
            assert_eq!((r.outcome.clone(), r.frames_encoded), (ExportOutcome::Complete, frames), "{r:?}");
        }
        let out = read(&std::fs::read(root.join("renders/master.mov")).unwrap(), frames).chunks.into_iter().map(|c| c.1).collect();
        let _ = std::fs::remove_dir_all(root);
        out
    }

    /// A title: a red square keyed across the frame, through commands alone.
    fn author(run: &mut Run<'_>) -> String {
        new_comp(run, "T01");
        let shape = cmd(run, "composition.add_layer", json!({"composition_id": "T01", "kind": "shape", "root": {"items": [
            {"kind": "rect", "position": [8.0, 8.0], "size": [16.0, 16.0]},
            {"kind": "fill", "rgba": [0.8, 0.1, 0.1, 1.0], "opacity": 100.0, "rule": "non_zero"}]}}))["layer_id"].as_str().unwrap().to_string();
        cmd(run, "keyframe.set", json!({"layer_id": shape, "property": "position", "time": 0, "value": [0.0, 0.0]}));
        cmd(run, "keyframe.set", json!({"layer_id": shape, "property": "position", "time": 4 * F, "value": [40.0, 0.0]}));
        cmd(run, "keyframe.set", json!({"layer_id": shape, "property": "anchor", "time": 0, "value": [0.0, 0.0]}));
        cmd(run, "composition.set_work_area", json!({"composition_id": "T01", "start_tick": 0, "end_tick": 5 * F}));
        cmd(run, "composition.queue_add", json!({"item_id": "rq", "composition_id": "T01", "module_id": "master", "format": "mov_vc3", "output_path": "renders/master.mov"}));
        cmd(run, "composition.queue_set_settings", json!({"item_id": "rq", "quality": "best", "resolution": "full", "color_depth": "8_bits_per_channel", "time_span": "work_area_only"}));
        shape
    }

    #[test]
    fn commands_reach_save_reopen_and_the_exported_frames() {
        with_open(|run| {
            author(run);
            let root = std::env::temp_dir().join(format!("hsk-motion-commands-a-{}", std::process::id()));
            let frames = export(&run.checkpoint().unwrap(), "rq", 5, &root);
            assert_eq!(frames.len(), 5);
            // Frame n: the square sits at x = 10 n .. 10 n + 16, y 0..16; Y' of the red fill is about 70
            // (16 + 219 * (0.2126*0.8 + 0.7152*0.1 + 0.0722*0.1)), the background is black.
            for (n, f) in frames.iter().enumerate() {
                let inside = f[8 * 64 + 10 * n + 8];
                assert!((i32::from(inside) - 70).abs() <= 3, "frame {n} luma {inside}");
                assert!(f[28 * 64 + 8] <= 18, "frame {n} below the square");
            }
        });
    }

    #[test]
    fn add_at_current_leaves_the_real_evaluator_unchanged() {
        with_open(|run| {
            let shape = author(run);
            let ease = json!([{"speed": 40.0, "influence": 0.5}]);
            cmd(run, "keyframe.set_interpolation", json!({"layer_id": shape, "property": "position", "time": 0, "out_interp": "bezier", "out_tangent": ease}));
            cmd(run, "keyframe.set_interpolation", json!({"layer_id": shape, "property": "position", "time": 4 * F, "in_interp": "bezier", "in_tangent": [{"speed": -5.0, "influence": 0.25}]}));
            let matrices = |bytes: &[u8]| {
                let cancel = CancellationToken::default();
                let session = open(bytes, &cancel);
                let records = session.composition_records(&Available, &cancel).unwrap().unwrap();
                let snapshot = session.snapshot();
                let source = CompositionTemporalSource::new(&CompositionInput { snapshot, expected_revision: snapshot.document().revision, records: &records, composition_id: "T01", budget: Budget::default(), resolver: &Available, blend_space: SPACE.blend_space, info: &info, space: SPACE, inputs: RenderInputs::default() }, &cancel).unwrap();
                (0..=80).map(|i| source.scene_at((i * F / 20) as i64, &cancel).map(|s| s.layers.iter().map(|l| l.matrix.m).collect::<Vec<_>>()).unwrap()).collect::<Vec<_>>()
            };
            let want = matrices(&run.checkpoint().unwrap());
            assert!(!want[10].is_empty());
            cmd(run, "keyframe.add_at_current", json!({"layer_id": shape, "property": "position", "time": F + F / 3}));
            cmd(run, "keyframe.add_at_current", json!({"layer_id": shape, "property": "position", "time": 3 * F}));
            let got = matrices(&run.checkpoint().unwrap());
            for (a, b) in want.iter().zip(&got) {
                for (x, y) in a.iter().flatten().zip(b.iter().flatten()) {
                    assert!((x - y).abs() < 1e-6, "{x} vs {y}");
                }
            }
        });
    }

    #[test]
    fn precompose_renders_the_same_frames() {
        with_open(|run| {
            let shape = author(run);
            let root = std::env::temp_dir().join(format!("hsk-motion-commands-b-{}", std::process::id()));
            let flat = export(&run.checkpoint().unwrap(), "rq", 5, &root);
            cmd(run, "composition.precompose", json!({"layer_ids": [shape], "name": "T02"}));
            let nested = export(&run.checkpoint().unwrap(), "rq", 5, &root);
            assert_eq!(flat, nested, "pre-compose keeps the render");
        });
    }
}
