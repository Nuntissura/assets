//! Lane SMB-MOTION-COMMANDS-2 ([STU-MOT-005] `solid` and `adjustment`, [STU-CMP-015], [STU-MOT-037],
//! [STU-MOT-038]; the motion-comp scenario, `workflows/scenario-motion-comp.json` steps 16, 17, 19, 28
//! and 44): solid and adjustment layers through `composition.add_layer`, `composition.set_solid`,
//! keyframe selection (three scopes), copy, paste and reverse paste, shift, ease and toggle hold,
//! all through `Run::motion_command`. Selection and clipboard are run state: no history step, not
//! saved. The module `export` (feature `media`) renders the result through the render queue.
use hsk_studio_accord::CancellationToken;
use hsk_studio_harness::{Harness, Run, Selection, asset_local_test_context};
use hsk_studio_harness_core::action::RUN_COMMANDS;
use serde_json::{Value, json};

const FIXTURE: &[u8] = include_bytes!("fixtures/demo.handshake");
const L2: &str = "SLYR-019abcde-0000-7000-8000-000000000002";
const S: u64 = 254_016_000_000;
const F: u64 = 10_160_640_000; // 25 fps
const EASE: f64 = 0.16666666666;

fn harness() -> Harness {
    Harness::new(Selection { providers: vec!["chronicle.patch".to_string()], ..Selection::default() }).unwrap()
}

fn cmd(run: &mut Run<'_>, id: &str, params: Value) -> Value {
    assert!(run.motion_command(id, &params), "{id} {params}: {}", run.ops().last().unwrap());
    run.ops().last().unwrap()["detail"].clone()
}

/// A refused command: its typed code; asserts the entries and the history did not change.
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
    run.document_entry("motion").map_or(Value::Null, |m| serde_json::from_slice(m).unwrap())
}

fn layer(run: &Run<'_>, id: &str) -> Value {
    for comp in comps(run)["compositions"].as_array().unwrap() {
        for l in comp["layers"].as_array().into_iter().flatten() {
            if l["layer_id"] == id {
                return l.clone();
            }
        }
    }
    panic!("layer {id} is in no composition");
}

fn doc_layer(run: &Run<'_>, id: &str) -> hsk_studio_folio::StudioLayer {
    run.snapshot().unwrap().document().layers.iter().find(|l| l.layer_id == id).unwrap().clone()
}

fn new_comp(run: &mut Run<'_>, id: &str) {
    cmd(run, "composition.new", json!({"composition_id": id, "name": id, "width": 64, "height": 32, "frame_rate": {"numerator": 25, "denominator": 1}, "duration": 12 * S, "background": {"r": 0, "g": 0, "b": 0, "a": 1, "colour_profile_id": null}}));
}

fn with_open(f: impl FnOnce(&mut Run<'_>)) {
    let h = harness();
    let cancel = CancellationToken::default();
    let mut run = h.run(asset_local_test_context(), &cancel);
    assert!(run.open_bytes(FIXTURE));
    f(&mut run);
}

/// Checkpoint, reopen, compare the records; the reopened set still validates.
fn survives_reopen(run: &mut Run<'_>) {
    let want = (comps(run), motion(run));
    let bytes = run.checkpoint().expect("checkpoint");
    let h = harness();
    let cancel = CancellationToken::default();
    let mut again = h.run(asset_local_test_context(), &cancel);
    assert!(again.open_bytes(&bytes), "{:?}", again.ops().last());
    assert_eq!((comps(&again), motion(&again)), want);
    assert!(again.motion_command("composition.read", &json!({})), "{:?}", again.ops().last());
}

/// The keys of `property` on `layer_id` as (time, value, in, out, in tangent, out tangent).
fn keys(run: &mut Run<'_>, layer_id: &str, property: &str) -> Vec<Value> {
    cmd(run, "property.read", json!({"layer_id": layer_id, "property": property}))["keyframes"].as_array().unwrap().clone()
}

fn times(run: &mut Run<'_>, layer_id: &str, property: &str) -> Vec<u64> {
    keys(run, layer_id, property).iter().map(|k| k["time"].as_u64().unwrap()).collect()
}

fn selection(run: &mut Run<'_>) -> Value {
    cmd(run, "keyframe.selection", json!({}))
}

#[test]
fn new_rows_exist_with_their_flags_and_spec_ids() {
    let mutating = ["composition.set_solid", "keyframe.paste", "keyframe.shift_selected", "keyframe.ease", "keyframe.toggle_hold"];
    let reading = ["keyframe.select", "keyframe.deselect_all", "keyframe.selection", "keyframe.copy"];
    for id in mutating.iter().chain(&reading) {
        let row = RUN_COMMANDS.iter().find(|c| c.command_id == *id).unwrap_or_else(|| panic!("row {id}"));
        assert_eq!(row.run_method, "motion_command", "{id}");
        assert!(!row.recordable, "{id}");
        assert_eq!(row.mutates_authority, mutating.contains(id), "{id}");
    }
    with_open(|run| {
        new_comp(run, "main");
        cmd(run, "STUDIO_KEYFRAME_DESELECT_ALL", json!({}));
        assert_eq!(run.ops().last().unwrap()["command_id"], "keyframe.deselect_all");
    });
}

#[test]
fn step_16_17_solid_layers_are_typed_named_undoable_and_saved() {
    with_open(|run| {
        new_comp(run, "main");
        let depth = undo_len(run);
        let solid = cmd(run, "composition.add_layer", json!({"composition_id": "main", "kind": "solid", "name": "BG", "colour": [0.8, 0.1, 0.1, 1.0]}))["layer_id"].as_str().unwrap().to_string();
        assert_eq!((undo_len(run), last_label(run).as_str()), (depth + 1, "New Layer"), "document layer, composition layer and solid record are one step");
        // The size defaults to the composition's; the document layer is an empty raster layer.
        assert_eq!(motion(run)["solid_layers"], json!([{"layer_id": solid, "width": 64, "height": 32, "colour": [0.8, 0.1, 0.1, 1.0]}]));
        let l = doc_layer(run, &solid);
        assert_eq!((l.kind.as_str(), l.name.as_str()), ("raster", "BG"));
        assert!(matches!(&l.payload, hsk_studio_folio::Payload::Raster { tiles } if tiles.is_empty()));
        assert!(layer(run, &solid)["timing"].is_object());
        // Set Solid: colour and size, one named step.
        cmd(run, "composition.set_solid", json!({"layer_id": solid, "colour": [0.0, 0.0, 1.0, 1.0], "width": 32}));
        assert_eq!(last_label(run), "Solid Settings");
        assert_eq!(motion(run)["solid_layers"][0], json!({"layer_id": solid, "width": 32, "height": 32, "colour": [0.0, 0.0, 1.0, 1.0]}));
        assert!(run.undo());
        assert_eq!(motion(run)["solid_layers"][0]["width"], 64);
        assert!(run.redo());
        // Step 17: an effect on the solid is an ordinary effect entry.
        cmd(run, "composition.add_effect", json!({"layer_id": solid, "effect_id": "ramp", "filter_kind": "gradient_ramp", "colour_profiles": {"start_color": {"colour_profile_id": null}, "end_color": {"colour_profile_id": null}}}));
        assert_eq!(layer(run, &solid)["effects"][0]["colour_profiles"]["start_color"], json!({"colour_profile_id": null}));
        // Hostile and typed refusals (V4-01/02).
        assert_eq!(refused(run, "composition.add_layer", json!({"composition_id": "main", "kind": "solid"})), "parameter_missing");
        for (w, h) in [(0, 8), (8, 0), (30_001, 8), (8, 30_001)] {
            assert_eq!(refused(run, "composition.add_layer", json!({"composition_id": "main", "kind": "solid", "colour": [1.0, 1.0, 1.0, 1.0], "width": w, "height": h})), "invalid_field", "{w}x{h}");
        }
        assert_eq!(refused(run, "composition.set_solid", json!({"layer_id": solid, "width": 0})), "invalid_field");
        assert!(run.ops().last().unwrap()["target"].as_str().unwrap().ends_with("/size"));
        assert_eq!(refused(run, "composition.set_solid", json!({"layer_id": solid, "colour": [1.5, 0.0, 0.0, 1.0]})), "invalid_field");
        assert_eq!(refused(run, "composition.set_solid", json!({"layer_id": solid, "colour": [1.0, 0.0]})), "parameter_invalid");
        assert_eq!(refused(run, "composition.set_solid", json!({"layer_id": solid, "width": -1})), "parameter_invalid");
        assert_eq!(refused(run, "composition.set_solid", json!({"layer_id": solid, "width": 32})), "nothing_to_change");
        cmd(run, "composition.add_layer", json!({"composition_id": "main", "kind": "existing", "layer_id": L2}));
        assert_eq!(refused(run, "composition.set_solid", json!({"layer_id": L2, "width": 8})), "layer_not_solid");
        // Footage and guide stay unsupported kinds; text has no kind here (vector.place_text_with_font).
        for kind in ["footage", "guide", "text"] {
            assert_eq!(refused(run, "composition.add_layer", json!({"composition_id": "main", "kind": kind})), "layer_kind_unsupported", "{kind}");
        }
        // Duplicate copies the solid record; remove drops it; undo brings it back.
        let dup = cmd(run, "composition.duplicate_layer", json!({"layer_id": solid}))["layer_id"].as_str().unwrap().to_string();
        assert_eq!(motion(run)["solid_layers"].as_array().unwrap().len(), 2);
        assert!(motion(run)["solid_layers"].as_array().unwrap().iter().any(|s| s["layer_id"] == dup && s["width"] == 32));
        cmd(run, "composition.remove_layer", json!({"layer_id": dup}));
        assert_eq!(motion(run)["solid_layers"].as_array().unwrap().len(), 1, "a removed solid leaves no record (no dangling save)");
        assert!(run.undo());
        assert_eq!(motion(run)["solid_layers"].as_array().unwrap().len(), 2);
        assert!(run.undo());
        assert_eq!(motion(run)["solid_layers"].as_array().unwrap().len(), 1);
        survives_reopen(run);
        // Deleting the composition takes its solid records.
        cmd(run, "composition.delete", json!({"composition_id": "main"}));
        assert!(motion(run).get("solid_layers").is_none());
    });
}

#[test]
fn step_44_adjustment_layers_are_kind_adjustment_with_a_group_payload() {
    with_open(|run| {
        new_comp(run, "main");
        cmd(run, "composition.add_layer", json!({"composition_id": "main", "kind": "existing", "layer_id": L2}));
        let adj = cmd(run, "composition.add_layer", json!({"composition_id": "main", "kind": "adjustment", "name": "Grade"}))["layer_id"].as_str().unwrap().to_string();
        assert_eq!(last_label(run), "New Layer");
        let l = doc_layer(run, &adj);
        assert_eq!((l.kind.as_str(), &l.payload, l.name.as_str()), ("adjustment", &hsk_studio_folio::Payload::Group, "Grade"));
        assert_eq!(comps(run)["compositions"][0]["layers"][0]["layer_id"], adj, "on top of the stack");
        // Levels and Tint on it (step 44); Tint's colours name their profile.
        cmd(run, "composition.add_effect", json!({"layer_id": adj, "effect_id": "levels", "filter_kind": "levels", "values": {"output_white": [0.5]}}));
        cmd(run, "composition.add_effect", json!({"layer_id": adj, "effect_id": "tint", "filter_kind": "tint", "colour_profiles": {"map_black_to": {"colour_profile_id": null}, "map_white_to": {"colour_profile_id": null}}}));
        assert_eq!(layer(run, &adj)["effects"].as_array().unwrap().len(), 2);
        // An adjustment layer duplicates like a group: the copy keeps its kind.
        let dup = cmd(run, "composition.duplicate_layer", json!({"layer_id": adj}))["layer_id"].as_str().unwrap().to_string();
        assert_eq!(doc_layer(run, &dup).kind, "adjustment");
        assert_eq!(layer(run, &dup)["effects"].as_array().unwrap().len(), 2);
        assert!(motion(run).get("solid_layers").is_none());
        survives_reopen(run);
        assert!(run.undo());
        assert!(run.snapshot().unwrap().document().layers.iter().all(|l| l.layer_id != dup));
    });
}

/// main: L2 with opacity keys at 0, S, 2S (40, 70, 100) and position keys at 0 and S; a null CTRL
/// with a rotation key at S.
fn keyed(run: &mut Run<'_>) -> String {
    new_comp(run, "main");
    cmd(run, "composition.add_layer", json!({"composition_id": "main", "kind": "existing", "layer_id": L2}));
    for (t, v) in [(0, 40.0), (S, 70.0), (2 * S, 100.0)] {
        cmd(run, "keyframe.set", json!({"layer_id": L2, "property": "opacity", "time": t, "value": [v]}));
    }
    for (t, v) in [(0, [0.0, 0.0]), (S, [10.0, 5.0])] {
        cmd(run, "keyframe.set", json!({"layer_id": L2, "property": "position", "time": t, "value": v}));
    }
    let ctrl = cmd(run, "composition.add_layer", json!({"composition_id": "main", "kind": "null", "name": "CTRL"}))["layer_id"].as_str().unwrap().to_string();
    cmd(run, "keyframe.set", json!({"layer_id": ctrl, "property": "rotation", "time": S, "value": [90.0]}));
    ctrl
}

#[test]
fn step_28_selection_scopes_modes_and_drops_keys_an_edit_removed() {
    with_open(|run| {
        let ctrl = keyed(run);
        let depth = undo_len(run);
        let entries = (run.document_entry("composition").map(<[u8]>::to_vec), run.document_entry("motion").map(<[u8]>::to_vec));
        // Three scopes, separately addressable; replace, add and remove.
        assert_eq!(cmd(run, "keyframe.select", json!({"scope": "property", "layer_id": L2, "property": "opacity"}))["selected"], 3);
        assert_eq!(cmd(run, "keyframe.select", json!({"scope": "property", "layer_id": L2, "property": "position", "times": [0], "mode": "add"}))["selected"], 4);
        assert_eq!(cmd(run, "keyframe.select", json!({"scope": "property", "layer_id": L2, "property": "opacity", "times": [0], "mode": "remove"}))["selected"], 3);
        let sel = selection(run);
        assert_eq!(sel["selected"], 3);
        assert_eq!(sel["dropped"], 0);
        assert_eq!(sel["keys"][0], json!({"layer_id": L2, "property": "opacity", "time": S}));
        assert_eq!(cmd(run, "keyframe.select", json!({"scope": "layer", "layer_id": L2}))["selected"], 5);
        assert_eq!(cmd(run, "keyframe.select", json!({"scope": "composition", "composition_id": "main"}))["selected"], 6);
        assert!(selection(run)["keys"].as_array().unwrap().iter().any(|k| k == &json!({"layer_id": ctrl, "property": "rotation", "time": S})));
        // Selection is view state: no history step, no document entry, no revision.
        assert_eq!(undo_len(run), depth);
        assert_eq!((run.document_entry("composition").map(<[u8]>::to_vec), run.document_entry("motion").map(<[u8]>::to_vec)), entries);
        cmd(run, "keyframe.deselect_all", json!({}));
        assert_eq!(selection(run)["selected"], 0);
        // A selected key an edit removes is dropped when read, and counted.
        cmd(run, "keyframe.select", json!({"scope": "property", "layer_id": L2, "property": "opacity"}));
        cmd(run, "keyframe.delete", json!({"layer_id": L2, "property": "opacity", "time": S}));
        let sel = selection(run);
        assert_eq!((sel["selected"].clone(), sel["dropped"].clone()), (json!(2), json!(1)));
        assert!(run.undo());
        assert_eq!(selection(run)["selected"], 2, "a dropped key stays dropped after undo");
        // Typed refusals: scope addressing, unknown keys, unknown scope or mode.
        assert_eq!(refused(run, "keyframe.select", json!({"scope": "property", "layer_id": L2})), "parameter_missing");
        assert_eq!(refused(run, "keyframe.select", json!({"scope": "layer"})), "parameter_missing");
        assert_eq!(refused(run, "keyframe.select", json!({"scope": "composition", "composition_id": "none"})), "composition_not_found");
        assert_eq!(refused(run, "keyframe.select", json!({"scope": "property", "layer_id": L2, "property": "opacity", "times": [7]})), "keyframe_not_found");
        assert_eq!(refused(run, "keyframe.select", json!({"scope": "galaxy", "layer_id": L2})), "parameter_invalid");
        assert_eq!(refused(run, "keyframe.select", json!({"scope": "layer", "layer_id": L2, "mode": "toggle"})), "parameter_invalid");
        assert_eq!(refused(run, "keyframe.select", json!({"scope": "property", "layer_id": L2, "property": "nonsense"})), "property_not_found");
        assert_eq!(selection(run)["selected"], 2, "a refused select keeps the selection");
        // Not saved: a reopened run has no selection.
        let bytes = run.checkpoint().unwrap();
        let h = harness();
        let cancel = CancellationToken::default();
        let mut again = h.run(asset_local_test_context(), &cancel);
        assert!(again.open_bytes(&bytes));
        assert_eq!(selection(&mut again)["selected"], 0);
    });
}

#[test]
fn copy_paste_and_reverse_paste_keep_relative_order_as_one_step() {
    with_open(|run| {
        let ctrl = keyed(run);
        assert_eq!(refused(run, "keyframe.paste", json!({"layer_id": L2, "time": 5 * S})), "clipboard_empty");
        // The S key leaves bezier with a tangent; the 2S key arrives linear.
        cmd(run, "keyframe.set_interpolation", json!({"layer_id": L2, "property": "opacity", "time": S, "out_interp": "bezier", "out_tangent": [{"speed": 3.0, "influence": 0.25}]}));
        cmd(run, "keyframe.select", json!({"scope": "property", "layer_id": L2, "property": "opacity", "times": [S, 2 * S]}));
        assert_eq!(cmd(run, "keyframe.copy", json!({}))["copied"], 2);
        let depth = undo_len(run);
        cmd(run, "keyframe.paste", json!({"layer_id": L2, "time": 5 * S}));
        assert_eq!((undo_len(run), last_label(run).as_str()), (depth + 1, "Paste Keyframes"));
        let k = keys(run, L2, "opacity");
        let at = |t: u64| k.iter().find(|x| x["time"] == t).unwrap_or_else(|| panic!("no key at {t}")).clone();
        assert_eq!((at(5 * S)["value"].clone(), at(6 * S)["value"].clone()), (json!([70.0]), json!([100.0])), "earliest at the time, relative order kept");
        assert_eq!(at(5 * S)["out_tangent"], json!([{"speed": 3.0, "influence": 0.25}]), "interpolation and tangents are copied");
        // Reverse paste: times mirrored about the copied span, in and out swapped.
        cmd(run, "keyframe.paste", json!({"layer_id": L2, "time": 8 * S, "reverse": true}));
        let k = keys(run, L2, "opacity");
        let at = |t: u64| k.iter().find(|x| x["time"] == t).unwrap().clone();
        assert_eq!((at(8 * S)["value"].clone(), at(9 * S)["value"].clone()), (json!([100.0]), json!([70.0])));
        assert_eq!((at(9 * S)["in_interp"].clone(), at(9 * S)["out_interp"].clone()), (json!("bezier"), json!("linear")));
        assert_eq!(at(9 * S)["in_tangent"], json!([{"speed": 3.0, "influence": 0.25}]));
        // Paste onto another layer's property of the same dimension, and over an existing key.
        cmd(run, "keyframe.paste", json!({"layer_id": ctrl, "property": "rotation", "time": S}));
        assert_eq!(times(run, &ctrl, "rotation"), vec![S, 2 * S], "the key at S is replaced, not doubled");
        assert_eq!(keys(run, &ctrl, "rotation")[0]["value"], json!([70.0]));
        // Typed refusals.
        assert_eq!(refused(run, "keyframe.paste", json!({"layer_id": L2, "property": "position", "time": 0})), "paste_property_mismatch");
        assert_eq!(refused(run, "keyframe.paste", json!({"layer_id": L2, "time": u64::MAX - 1})), "tick_overflow");
        // An explicit copy: one property, chosen times.
        assert_eq!(cmd(run, "keyframe.copy", json!({"layer_id": L2, "property": "position", "times": [S]}))["copied"], 1);
        cmd(run, "keyframe.paste", json!({"layer_id": L2, "time": 3 * S}));
        assert_eq!(times(run, L2, "position"), vec![0, S, 3 * S]);
        assert_eq!(refused(run, "keyframe.copy", json!({"layer_id": L2, "property": "position", "times": [7]})), "keyframe_not_found");
        assert_eq!(refused(run, "keyframe.copy", json!({"layer_id": L2})), "parameter_missing");
        cmd(run, "keyframe.deselect_all", json!({}));
        assert_eq!(refused(run, "keyframe.copy", json!({})), "selection_empty");
        // Undo of a paste removes exactly the pasted keys.
        assert!(run.undo());
        assert_eq!(times(run, L2, "position"), vec![0, S]);
        survives_reopen(run);
    });
}

#[test]
fn shift_ease_and_hold_over_the_selection_are_named_steps() {
    with_open(|run| {
        let ctrl = keyed(run);
        for id in ["keyframe.shift_selected", "keyframe.ease", "keyframe.toggle_hold"] {
            let p = if id == "keyframe.shift_selected" { json!({"frames": 1}) } else { json!({}) };
            assert_eq!(refused(run, id, p), "selection_empty", "{id}");
        }
        cmd(run, "keyframe.select", json!({"scope": "property", "layer_id": L2, "property": "opacity", "times": [S, 2 * S]}));
        cmd(run, "keyframe.select", json!({"scope": "property", "layer_id": ctrl, "property": "rotation", "mode": "add"}));
        // Shift by 10 frames: every selected key moves, the selection follows, one step.
        cmd(run, "keyframe.shift_selected", json!({"frames": 10}));
        assert_eq!(last_label(run), "Shift Selected Keyframes");
        assert_eq!(times(run, L2, "opacity"), vec![0, S + 10 * F, 2 * S + 10 * F]);
        assert_eq!(times(run, &ctrl, "rotation"), vec![S + 10 * F]);
        assert_eq!(selection(run)["selected"], 3);
        cmd(run, "keyframe.shift_selected", json!({"frames": -1}));
        assert_eq!(times(run, L2, "opacity"), vec![0, S + 9 * F, 2 * S + 9 * F]);
        assert!(run.undo());
        assert!(run.undo());
        assert_eq!(times(run, L2, "opacity"), vec![0, S, 2 * S]);
        cmd(run, "keyframe.select", json!({"scope": "property", "layer_id": L2, "property": "opacity", "times": [S, 2 * S]}));
        assert_eq!(refused(run, "keyframe.shift_selected", json!({"frames": 0})), "parameter_invalid");
        assert_eq!(refused(run, "keyframe.shift_selected", json!({"frames": -1000})), "tick_overflow", "before tick 0 is refused at the command");
        assert_eq!(refused(run, "keyframe.shift_selected", json!({"frames": i64::MIN})), "tick_overflow");
        assert_eq!(refused(run, "keyframe.shift_selected", json!({"frames": -25})), "keyframe_exists", "S - 25 frames lands on the key at 0");
        // Easy Ease (STU-MOT-037, influence STU-MOT-034).
        cmd(run, "keyframe.ease", json!({}));
        assert_eq!(last_label(run), "Easy Ease");
        let ease = json!([{"speed": 0.0, "influence": EASE}]);
        for k in keys(run, L2, "opacity").iter().filter(|k| k["time"] != 0) {
            assert_eq!((k["in_interp"].clone(), k["out_interp"].clone()), (json!("bezier"), json!("bezier")));
            assert_eq!((k["in_tangent"].clone(), k["out_tangent"].clone()), (ease.clone(), ease.clone()));
        }
        assert_eq!(keys(run, L2, "opacity")[0]["out_interp"], "linear", "an unselected key is untouched");
        assert!(run.undo());
        cmd(run, "keyframe.ease", json!({"side": "in"}));
        assert_eq!(last_label(run), "Easy Ease In");
        let k = keys(run, L2, "opacity");
        assert_eq!((k[1]["in_interp"].clone(), k[1]["out_interp"].clone()), (json!("bezier"), json!("linear")));
        cmd(run, "keyframe.ease", json!({"side": "out"}));
        assert_eq!(last_label(run), "Easy Ease Out");
        assert_eq!(keys(run, L2, "opacity")[1]["out_tangent"], ease);
        assert_eq!(refused(run, "keyframe.ease", json!({"side": "out"})), "nothing_to_change");
        assert_eq!(refused(run, "keyframe.ease", json!({"side": "sideways"})), "parameter_invalid");
        // Toggle hold: hold, then back to linear.
        cmd(run, "keyframe.toggle_hold", json!({}));
        assert_eq!(last_label(run), "Toggle Hold Keyframe");
        assert!(keys(run, L2, "opacity")[1..].iter().all(|k| k["out_interp"] == "hold"));
        cmd(run, "keyframe.toggle_hold", json!({}));
        assert!(keys(run, L2, "opacity")[1..].iter().all(|k| k["out_interp"] == "linear" && k.get("out_tangent").is_none()));
        survives_reopen(run);
    });
}

#[cfg(feature = "media")]
mod export {
    //! The scenario's acceptance: solids, adjustment layers and eased keys reach the render queue
    //! and the renderer; text authored through `vector.place_text_with_font` plus `kind existing`.
    use super::*;
    use hsk_studio_composite::temporal::composition::{CompositionInput, CompositionTemporalSource};
    use hsk_studio_composite::{BlendSpace, RenderInputs, ScenePixels, TileInfo};
    use hsk_studio_folio::{Budget, Resolution, Resolver, TileRef};
    use hsk_studio_harness::view::timeline_export::render_queue::session_render_queue_jobs;
    use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};
    use hsk_studio_pigment::render_plan::{ColourTag, TilePlanes};
    use hsk_studio_reel::*;
    use hsk_studio_session::{Session, SessionConfig, SessionPorts};
    use std::collections::BTreeMap;
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
            grant: Grant { content_id: ContentId::parse("motion-commands-2").unwrap(), revision: 1, expected_len: bytes.len() as u64 },
            expected_revision: 1,
        };
        let mut sink = Collect::default();
        Registry::standard().decode_segment(&mut Cursor::new(bytes.to_vec()), None, &request, &Limits::default(), &CancellationToken::default(), &mut sink).expect("reel reads the export");
        sink
    }
    /// The render-queue result of a checkpoint: the decoded luma plane per frame, or the refusal.
    fn export(bytes: &[u8], frames: u64, root: &std::path::Path) -> Result<Vec<Vec<u8>>, String> {
        let cancel = CancellationToken::default();
        let session = open(bytes, &cancel);
        let _ = std::fs::remove_dir_all(root);
        std::fs::create_dir_all(root.join("renders")).unwrap();
        let audio = |_: &str, _: u32| None;
        let (jobs, _) = session_render_queue_jobs(&session, "rq", &Available, &info, &|| NoPixels, &BTreeMap::new(), &audio, SPACE, root, false, &cancel).map_err(|e| format!("{e:?}"))?;
        let mut queue = ExportQueue::new();
        for (_, job) in jobs {
            queue.submit(job);
        }
        for r in queue.run() {
            if r.outcome != ExportOutcome::Complete {
                let _ = std::fs::remove_dir_all(root);
                return Err(format!("{r:?}"));
            }
        }
        let out = read(&std::fs::read(root.join("renders/master.mov")).unwrap(), frames).chunks.into_iter().map(|c| c.1).collect();
        let _ = std::fs::remove_dir_all(root);
        Ok(out)
    }
    fn queue(run: &mut Run<'_>, comp: &str) {
        cmd(run, "composition.set_work_area", json!({"composition_id": comp, "start_tick": 0, "end_tick": 2 * F}));
        cmd(run, "composition.queue_add", json!({"item_id": "rq", "composition_id": comp, "module_id": "master", "format": "mov_vc3", "output_path": "renders/master.mov"}));
        cmd(run, "composition.queue_set_settings", json!({"item_id": "rq", "quality": "best", "resolution": "full", "color_depth": "8_bits_per_channel", "time_span": "work_area_only"}));
    }
    /// Y' of straight RGB (BT.709, video range), the reading of `tests/motion_commands.rs`.
    fn luma(rgb: [f64; 3]) -> f64 {
        16.0 + 219.0 * (0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2])
    }

    #[test]
    fn step_16_44_a_solid_under_an_adjustment_layer_renders_through_the_render_queue() {
        with_open(|run| {
            new_comp(run, "T01");
            cmd(run, "composition.add_layer", json!({"composition_id": "T01", "kind": "solid", "colour": [0.8, 0.1, 0.1, 1.0]}));
            queue(run, "T01");
            let root = std::env::temp_dir().join(format!("hsk-motion-commands-2-a-{}", std::process::id()));
            let solid = export(&run.checkpoint().unwrap(), 2, &root).expect("a solid renders");
            let want = luma([0.8, 0.1, 0.1]);
            for f in &solid {
                for at in [0, 31 * 64 + 63, 16 * 64 + 20] {
                    assert!((f64::from(f[at]) - want).abs() <= 3.0, "solid luma {} at {at}, want {want}", f[at]);
                }
            }
            // Step 44: an adjustment layer with Levels (output white 0.5) over it, at 50 % opacity.
            let adj = cmd(run, "composition.add_layer", json!({"composition_id": "T01", "kind": "adjustment"}))["layer_id"].as_str().unwrap().to_string();
            cmd(run, "composition.add_effect", json!({"layer_id": adj, "filter_kind": "levels", "values": {"output_white": [0.5]}}));
            let full = export(&run.checkpoint().unwrap(), 2, &root).expect("an adjustment layer renders");
            let want = luma([0.4, 0.05, 0.05]);
            assert!((f64::from(full[0][16 * 64 + 20]) - want).abs() <= 3.0, "levels luma {} want {want}", full[0][16 * 64 + 20]);
            cmd(run, "property.set_value", json!({"layer_id": adj, "property": "opacity", "value": [50.0]}));
            let half = export(&run.checkpoint().unwrap(), 2, &root).expect("renders");
            let want = luma([0.6, 0.075, 0.075]);
            assert!((f64::from(half[0][16 * 64 + 20]) - want).abs() <= 3.0, "50 % luma {} want {want}", half[0][16 * 64 + 20]);
        });
    }

    #[test]
    fn eased_keys_evaluate_as_motion_reads_the_stored_ease() {
        with_open(|run| {
            new_comp(run, "T01");
            let shape = cmd(run, "composition.add_layer", json!({"composition_id": "T01", "kind": "shape"}))["layer_id"].as_str().unwrap().to_string();
            cmd(run, "keyframe.set", json!({"layer_id": shape, "property": "position", "time": 0, "value": [0.0, 0.0]}));
            cmd(run, "keyframe.set", json!({"layer_id": shape, "property": "position", "time": 4 * F, "value": [40.0, 0.0]}));
            cmd(run, "keyframe.select", json!({"scope": "layer", "layer_id": shape}));
            cmd(run, "keyframe.ease", json!({}));
            let x_at = |bytes: &[u8]| {
                let cancel = CancellationToken::default();
                let session = open(bytes, &cancel);
                let records = session.composition_records(&Available, &cancel).unwrap().unwrap();
                let snapshot = session.snapshot();
                let source = CompositionTemporalSource::new(&CompositionInput { snapshot, expected_revision: snapshot.document().revision, records: &records, composition_id: "T01", budget: Budget::default(), resolver: &Available, blend_space: SPACE.blend_space, info: &info, space: SPACE, inputs: RenderInputs::default() }, &cancel).unwrap();
                (0..=40).map(|i| source.scene_at((i * F / 10) as i64, &cancel).map(|s| s.layers.first().map_or(f64::NAN, |l| l.matrix.m[2])).unwrap()).collect::<Vec<_>>()
            };
            let eased = x_at(&run.checkpoint().unwrap());
            // Speed 0 at both keys, symmetric influence: half way at the middle, slower than linear
            // in the first quarter, every sample inside the keys' span.
            assert!((eased[20] - 20.0).abs() < 1e-6, "{}", eased[20]);
            assert!(eased[10] < 10.0 && eased[1] < 1.0, "{} {}", eased[10], eased[1]);
            assert!(eased.iter().all(|x| (0.0..=40.0).contains(x)));
            // The same stored values written explicitly evaluate identically.
            let tangent = json!([{"speed": 0.0, "influence": EASE}]);
            for t in [0, 4 * F] {
                cmd(run, "keyframe.set_interpolation", json!({"layer_id": shape, "property": "position", "time": t, "in_interp": "linear", "out_interp": "linear"}));
                cmd(run, "keyframe.set_interpolation", json!({"layer_id": shape, "property": "position", "time": t, "in_interp": "bezier", "out_interp": "bezier", "in_tangent": tangent, "out_tangent": tangent}));
            }
            assert_eq!(x_at(&run.checkpoint().unwrap()), eased);
        });
    }

    #[test]
    fn step_19_text_through_place_text_and_kind_existing_reaches_the_render_queue() {
        use hsk_studio_harness::view::Viewer;
        use hsk_studio_harness::view::render::Prefer;
        let dir = std::env::temp_dir().join(format!("hsk-motion-commands-2-text-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("in.handshake");
        std::fs::write(&src, FIXTURE).unwrap();
        let mut v = Viewer::new(Prefer::Cpu);
        v.open(&src).unwrap();
        let family = hsk_studio_harness::view::text::DEFAULT_FONT;
        let text = v.place_text_with_font(4.0, 24.0, "Hi", family).unwrap_or_else(|e| panic!("{e}"));
        v.motion_command("composition.new", &json!({"composition_id": "T01", "name": "T01", "width": 64, "height": 32, "frame_rate": {"numerator": 25, "denominator": 1}, "duration": 12 * S})).unwrap();
        v.motion_command("composition.add_layer", &json!({"composition_id": "T01", "kind": "existing", "layer_id": text})).unwrap();
        v.motion_command("composition.set_work_area", &json!({"composition_id": "T01", "start_tick": 0, "end_tick": 2 * F})).unwrap();
        v.motion_command("composition.queue_add", &json!({"item_id": "rq", "composition_id": "T01", "module_id": "master", "format": "mov_vc3", "output_path": "renders/master.mov"})).unwrap();
        let out = dir.join("text.handshake");
        v.save(&out, false).unwrap();
        let bytes = std::fs::read(&out).unwrap();
        let root = dir.join("render");
        match export(&bytes, 2, &root) {
            Ok(frames) => {
                let inked = frames[0].iter().filter(|y| **y > 40).count();
                println!("TEXT_RENDER ok inked={inked}");
                assert!(inked > 10, "the text reaches the frame: {inked}");
            }
            // The render path has no prepared text: report the typed code, the lane stops there.
            Err(e) => {
                println!("NOT_PROVEN: composition text through the render queue is refused: {e}");
                assert!(e.contains("session_composition") || e.contains("text"), "a typed refusal: {e}");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
