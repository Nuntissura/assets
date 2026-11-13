//! Lane SMB-DS-VARIABLES-AUTHOR-1 (feature `viewer`): design variables authored by the spec's typed
//! commands ([STU-DS-121..133], [STU-DS-024..027], [STU-DS-162], [STU-DS-164], [STU-DS-175], [STU-DS-177])
//! through `Viewer::run_command`, as the process and a panel drive them. Every mutating command is one
//! undo step; the records live in the vector entry's `design` member, so they survive save and reopen.
//! Figma's own variable behaviour (default mode name, value copy on a new mode) is UNVERIFIED (D5).
#![cfg(feature = "viewer")]

mod common;

use common::Scratch;
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::vector::ShapeKind;
use hsk_studio_harness_core::document_new::{DocumentProfile, NewDocument};
use serde_json::{Value, json};

fn design_doc() -> Viewer {
    let mut v = Viewer::new(Prefer::Cpu);
    v.new_document(&NewDocument::new(DocumentProfile::Design, None), None).unwrap();
    v
}

fn call(v: &mut Viewer, id: &str, params: Value) -> Result<Value, (String, Value)> {
    let Value::Object(map) = params else { panic!("parameters are an object") };
    v.run_command(id, &map).map_err(|r| (r.code, r.detail))
}

fn ok(v: &mut Viewer, id: &str, params: Value) -> Value {
    call(v, id, params).unwrap_or_else(|(code, detail)| panic!("{id}: {code} {detail}"))
}

/// Refused with `code`, and nothing changed: the history depth and the vector entry bytes stay.
fn refused(v: &mut Viewer, id: &str, params: Value, code: &str) -> Value {
    let before = (undo_depth(v), vector_bytes(v));
    match call(v, id, params) {
        Ok(r) => panic!("{id} should refuse {code}: {r}"),
        Err((c, detail)) => {
            assert_eq!(c, code, "{id}: {detail}");
            assert_eq!((undo_depth(v), vector_bytes(v)), before, "{id}: a refusal changes nothing");
            detail
        }
    }
}

fn text(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or_else(|| panic!("no {key} in {v}")).to_string()
}

fn undo_depth(v: &Viewer) -> usize {
    v.run().history()["undo"].as_array().map_or(0, Vec::len)
}

fn newest_label(v: &Viewer) -> String {
    v.run().history()["undo"].as_array().and_then(|a| a.last()).and_then(Value::as_str).unwrap_or_default().to_string()
}

fn vector_bytes(v: &Viewer) -> Vec<u8> {
    v.run().document_entry("vector").map(<[u8]>::to_vec).unwrap_or_default()
}

fn variables(v: &Viewer) -> Value {
    v.state_json()["design"]["variables"].clone()
}

fn float(x: f64) -> Value {
    json!({"type": "FLOAT", "value": x})
}

fn colour(r: f64, g: f64, b: f64) -> Value {
    json!({"type": "COLOR", "value": {"r": r, "g": g, "b": b, "a": 1.0, "colour_profile_id": null}})
}

/// A collection `Tokens` whose first mode is `Light` and a second mode `Dark`: (collection, light, dark).
fn tokens(v: &mut Viewer) -> (String, String, String) {
    let made = ok(v, "variable_collection.create", json!({"name": "Tokens", "mode_name": "Light"}));
    let (collection, light) = (text(&made, "collection_id"), text(&made, "mode_id"));
    let dark = text(&ok(v, "variable_collection.add_mode", json!({"collection": collection, "name": "Dark"})), "mode_id");
    (collection, light, dark)
}

/// A primitive colour and a semantic alias of it: (primitive, semantic).
fn primitive_and_alias(v: &mut Viewer, collection: &str, light: &str, dark: &str) -> (String, String) {
    let primitive = text(&ok(v, "variable.create", json!({"collection": collection, "name": "color/blue", "resolved_type": "COLOR", "value": colour(0.0, 0.0, 1.0)})), "variable_id");
    ok(v, "variable.set_value_for_mode", json!({"variable": primitive, "mode": dark, "value": colour(0.5, 0.5, 1.0)}));
    let alias = ok(v, "variable.create_alias", json!({"variable": primitive}));
    assert_eq!((alias["type"].as_str(), alias["id"].as_str(), alias["resolved_type"].as_str()), (Some("VARIABLE_ALIAS"), Some(primitive.as_str()), Some("COLOR")), "{alias}");
    let semantic = text(&ok(v, "variable.create", json!({"collection": collection, "name": "color/accent", "resolved_type": "COLOR", "value": alias})), "variable_id");
    let _ = light;
    (primitive, semantic)
}

/// Review F3 (HOST INTEGRATION (3)-(5)): the command's own record names its catalog id, its history step is
/// named and attributed by that id (the undo name is the catalog id), and its business event is
/// STUDIO_VARIABLE_SET; review F7: every mutating result carries the STU-DS-123 `advisory` key.
fn assert_committed_as(v: &Viewer, id: &str, result: &Value) {
    assert_eq!(newest_label(v), id, "the undo name is the catalog id");
    let step = v.run().history()["undo_steps"].as_array().and_then(|s| s.last()).cloned().unwrap_or_default();
    assert_eq!(step["command_id"], id, "the step is attributed to the row: {step}");
    let record = v.run().ops().last().cloned().unwrap_or_default();
    assert_eq!((record["op"].as_str(), record["command_id"].as_str()), (Some(id), Some(id)), "{record}");
    assert_eq!(record["business_events"][0]["event"], "STUDIO_VARIABLE_SET", "{record}");
    assert!(result.get("advisory").is_some(), "{id}: the result carries the advisory key: {result}");
}

/// [STU-DS-123], [STU-DS-133], [STU-DS-162]: every mutating command is ONE undo step named by its catalog
/// id; undo restores the vector entry byte for byte and redo brings the records back.
#[test]
fn every_mutating_variable_command_is_one_named_undo_step() {
    let mut v = design_doc();
    let start = vector_bytes(&v);
    let depth = undo_depth(&v);
    let made = ok(&mut v, "variable_collection.create", json!({"name": "Spacing"}));
    let collection = text(&made, "collection_id");
    assert!(collection.starts_with("SVCL-"), "{made}");
    assert_eq!(undo_depth(&v), depth + 1);
    assert_committed_as(&v, "variable_collection.create", &made);
    // STU-DS-123: a collection is created with one mode named `Mode 1`.
    let state = variables(&v);
    assert_eq!(state["collections"][0]["modes"], json!([{"mode_id": text(&made, "mode_id"), "name": "Mode 1"}]), "{state}");
    assert_eq!(state["collections"][0]["default_mode_id"], made["mode_id"]);
    let first = text(&made, "mode_id");
    let steps: Vec<(&str, Value)> = vec![
        ("variable_collection.add_mode", json!({"collection": collection, "name": "Compact"})),
        ("variable_collection.rename_mode", json!({"collection": collection, "mode": first, "name": "Comfortable"})),
        ("variable.create", json!({"collection": collection, "name": "space/m", "resolved_type": "FLOAT", "value": float(16.0), "scopes": ["GAP", "WIDTH_HEIGHT"], "description": "medium gap"})),
    ];
    let mut after_each = Vec::new();
    let mut variable = String::new();
    for (id, params) in steps {
        let before = undo_depth(&v);
        let r = ok(&mut v, id, params);
        assert_eq!(undo_depth(&v), before + 1, "{id}: one step");
        assert_committed_as(&v, id, &r);
        if id == "variable.create" {
            variable = text(&r, "variable_id");
        }
        after_each.push(vector_bytes(&v));
    }
    let compact = variables(&v)["collections"][0]["modes"][1]["mode_id"].as_str().unwrap().to_string();
    for (id, params) in [
        ("variable.set_value_for_mode", json!({"variable": variable, "mode": compact, "value": float(8.0)})),
        ("variable.set_code_syntax", json!({"variable": variable, "platform": "WEB", "name": "--space-m"})),
        ("variable.set_code_syntax", json!({"variable": variable, "platform": "iOS", "name": "spaceM"})),
        ("variable.remove_code_syntax", json!({"variable": variable, "platform": "iOS"})),
    ] {
        let before = undo_depth(&v);
        let r = ok(&mut v, id, params);
        assert_eq!(undo_depth(&v), before + 1, "{id}: one step");
        assert_committed_as(&v, id, &r);
        after_each.push(vector_bytes(&v));
    }
    let state = variables(&v);
    let var = &state["variables"][0];
    assert_eq!(var["values_by_mode"][&compact], float(8.0), "{state}");
    assert_eq!(var["values_by_mode"][&first], float(16.0), "{state}");
    assert_eq!(var["code_syntax"], json!({"WEB": "--space-m"}), "{state}");
    assert_eq!(var["scopes"], json!(["GAP", "WIDTH_HEIGHT"]));
    assert_eq!(var["description"], "medium gap");
    assert_eq!(state["collections"][0]["modes"][0]["name"], "Comfortable");
    assert_eq!(state["collections"][0]["variable_ids"], json!([variable]));
    // Undo each step: the entry steps back byte for byte, to the empty document's entry.
    let end = vector_bytes(&v);
    for i in (0..after_each.len()).rev() {
        assert_eq!(vector_bytes(&v), after_each[i]);
        v.undo().unwrap();
    }
    v.undo().unwrap();
    assert_eq!(vector_bytes(&v), start, "every step undone");
    for _ in 0..=after_each.len() {
        v.redo().unwrap();
    }
    assert_eq!(vector_bytes(&v), end, "every step redone");
}

/// [STU-DS-132], [STU-DS-133] `variable.resolve_for_consumer`, [STU-DS-026], [STU-DS-164]: an alias
/// resolves through the explicit mode of the consumer's nearest container; absence inherits; the
/// document root falls back to the collection's default mode; resolution is deterministic.
#[test]
fn an_alias_resolves_for_a_consumer_through_its_containers_explicit_mode() {
    let mut v = design_doc();
    let (collection, light, dark) = tokens(&mut v);
    let (primitive, semantic) = primitive_and_alias(&mut v, &collection, &light, &dark);
    let inside = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [50.0, 50.0], false).unwrap();
    let outside = v.draw_shape(ShapeKind::Rectangle, [100.0, 10.0], [150.0, 50.0], false).unwrap();
    let card = v.group_layers(&[inside.clone()], "Card").unwrap();
    v.select(None);
    let resolve = |v: &mut Viewer, node: &str| ok(v, "variable.resolve_for_consumer", json!({"variable": semantic, "node": node}));
    let before = resolve(&mut v, &inside);
    assert_eq!(before["value"], colour(0.0, 0.0, 1.0), "{before}");
    assert_eq!(before["mode_id"], light.as_str());
    assert_eq!(before["chain"], json!([semantic, primitive]), "alias chain in resolution order");
    let depth = undo_depth(&v);
    let set = ok(&mut v, "node.set_explicit_variable_mode_for_collection", json!({"node": card, "collection": collection, "mode": dark}));
    assert_eq!(undo_depth(&v), depth + 1);
    assert_committed_as(&v, "node.set_explicit_variable_mode_for_collection", &set);
    assert_eq!(set["rebound"], 0, "no bound property in this document: nothing to re-resolve ({set})");
    for node in [&inside, &card] {
        let r = resolve(&mut v, node);
        assert_eq!((r["value"].clone(), r["mode_id"].as_str()), (colour(0.5, 0.5, 1.0), Some(dark.as_str())), "{node}: {r}");
        assert_eq!(r["modes"][&collection]["source"], if node == &card { "explicit" } else { "inherited" }, "{r}");
    }
    let r = resolve(&mut v, &outside);
    assert_eq!((r["value"].clone(), r["modes"][&collection]["source"].as_str()), (colour(0.0, 0.0, 1.0), Some("default")), "{r}");
    // Deterministic: the same state gives the same answer.
    assert_eq!(resolve(&mut v, &inside), resolve(&mut v, &inside));
    // A leaf is not a container (STU-DS-132 keys container layers and pages).
    refused(&mut v, "node.set_explicit_variable_mode_for_collection", json!({"node": inside, "collection": collection, "mode": dark}), "node_not_container");
    refused(&mut v, "node.set_explicit_variable_mode_for_collection", json!({"node": card, "collection": collection, "mode": "no-such-mode"}), "variable_mode_unknown");
    refused(&mut v, "variable.resolve_for_consumer", json!({"variable": semantic, "node": "SLYR-019abcde-0000-7000-8000-00000000ffff"}), "node_unknown");
    // Clearing is one step back to the inherited (default) mode; clearing again is typed.
    let cleared = ok(&mut v, "node.clear_explicit_variable_mode_for_collection", json!({"node": card, "collection": collection}));
    assert_committed_as(&v, "node.clear_explicit_variable_mode_for_collection", &cleared);
    assert_eq!(resolve(&mut v, &inside)["mode_id"], light.as_str());
    refused(&mut v, "node.clear_explicit_variable_mode_for_collection", json!({"node": card, "collection": collection}), "explicit_mode_absent");
    v.undo().unwrap();
    assert_eq!(resolve(&mut v, &inside)["mode_id"], dark.as_str(), "undo brings the explicit mode back");
}

/// Hostile and wrong input is refused typed before anything is written ([STU-DS-125..129], [STU-DS-175]
/// `variable_alias_cycle` naming the cycle in resolution order, [STU-DS-177] `variable_scope_violation`).
#[test]
fn wrong_input_is_refused_typed_and_changes_nothing() {
    let mut v = design_doc();
    let (collection, light, dark) = tokens(&mut v);
    let (primitive, semantic) = primitive_and_alias(&mut v, &collection, &light, &dark);
    refused(&mut v, "variable.create", json!({"collection": "SVCL-019abcde-0000-7000-8000-00000000ffff", "name": "x", "resolved_type": "FLOAT", "value": float(1.0)}), "variable_collection_unknown");
    refused(&mut v, "variable.create", json!({"collection": collection, "name": "x", "resolved_type": "FLOAT", "value": json!({"type": "STRING", "value": "1"})}), "variable_value_type");
    refused(&mut v, "variable.create", json!({"collection": collection, "name": "x", "resolved_type": "FLOAT", "value": float(1.0), "scopes": ["GAP", "NOT_A_SCOPE"]}), "variable_scope_unknown");
    refused(&mut v, "variable.create", json!({"collection": collection, "name": "x", "resolved_type": "FLOAT", "value": float(1.0), "scopes": ["GAP", "GAP"]}), "variable_scope_unknown");
    refused(&mut v, "variable.create", json!({"collection": collection, "name": "a//b", "resolved_type": "FLOAT", "value": float(1.0)}), "variable_name_invalid");
    refused(&mut v, "variable.create", json!({"collection": collection, "name": "color/blue", "resolved_type": "COLOR", "value": colour(1.0, 0.0, 0.0)}), "variable_name_taken");
    refused(&mut v, "variable.create", json!({"collection": collection, "name": "x", "resolved_type": "FLOAT", "value": json!({"type": "FLOAT", "value": 1.0, "extra": 1})}), "parameter_invalid");
    refused(&mut v, "variable.create", json!({"collection": collection, "name": "x", "resolved_type": "NUMBER", "value": float(1.0)}), "parameter_invalid");
    refused(&mut v, "variable.create", json!({"collection": collection, "name": "x", "resolved_type": "STRING", "value": json!({"type": "STRING", "value": "x".repeat(70_000)})}), "invalid_field");
    // STU-DS-129: a fourth platform is refused typed rather than stored.
    refused(&mut v, "variable.set_code_syntax", json!({"variable": primitive, "platform": "FLUTTER", "name": "blue"}), "parameter_invalid");
    refused(&mut v, "variable.set_code_syntax", json!({"variable": primitive, "platform": "WEB", "name": "line\nbreak"}), "code_syntax_name_invalid");
    refused(&mut v, "variable.remove_code_syntax", json!({"variable": primitive, "platform": "ANDROID"}), "code_syntax_absent");
    refused(&mut v, "variable.set_value_for_mode", json!({"variable": primitive, "mode": "no-such-mode", "value": colour(1.0, 1.0, 1.0)}), "variable_mode_unknown");
    refused(&mut v, "variable.set_value_for_mode", json!({"variable": primitive, "mode": dark, "value": float(1.0)}), "variable_value_type");
    // STU-DS-175: the primitive aliasing its own alias is a cycle, named in resolution order.
    let cycle = ok(&mut v, "variable.create_alias", json!({"variable": semantic}));
    let detail = refused(&mut v, "variable.set_value_for_mode", json!({"variable": primitive, "mode": dark, "value": cycle}), "variable_alias_cycle");
    let message = detail["message"].as_str().unwrap_or_default();
    assert!(message.contains(&format!("{primitive} -> {semantic} -> {primitive}")), "{message}");
    // STU-DS-177: a property scope the variable does not admit.
    let gap = text(&ok(&mut v, "variable.create", json!({"collection": collection, "name": "space/gap", "resolved_type": "FLOAT", "value": float(4.0), "scopes": ["GAP"]})), "variable_id");
    let root = v.draw_shape(ShapeKind::Rectangle, [0.0, 0.0], [10.0, 10.0], false).unwrap();
    ok(&mut v, "variable.resolve_for_consumer", json!({"variable": gap, "node": root, "scope": "GAP"}));
    let violation = refused(&mut v, "variable.resolve_for_consumer", json!({"variable": gap, "node": root, "scope": "OPACITY"}), "variable_scope_violation");
    let message = violation["message"].as_str().unwrap_or_default();
    assert!(message.contains(&gap) && message.contains("GAP") && message.contains("OPACITY"), "names the variable, its scopes and the property: {message}");
    // ALL_SCOPES admits everything its type is legal for; ALL_FILLS admits the three fills.
    ok(&mut v, "variable.resolve_for_consumer", json!({"variable": primitive, "node": root, "scope": "STROKE_COLOR"}));
    let fills = text(&ok(&mut v, "variable.create", json!({"collection": collection, "name": "color/fill", "resolved_type": "COLOR", "value": colour(0.1, 0.1, 0.1), "scopes": ["ALL_FILLS"]})), "variable_id");
    ok(&mut v, "variable.resolve_for_consumer", json!({"variable": fills, "node": root, "scope": "TEXT_FILL"}));
    refused(&mut v, "variable.resolve_for_consumer", json!({"variable": fills, "node": root, "scope": "STROKE_COLOR"}), "variable_scope_violation");
    // STU-DS-123 hard_min 1; a variable another one aliases is not removed from under it.
    let single = text(&ok(&mut v, "variable_collection.create", json!({"name": "Single"})), "collection_id");
    let only = variables(&v)["collections"].as_array().unwrap().iter().find(|c| c["collection_id"] == single.as_str()).unwrap()["modes"][0]["mode_id"].as_str().unwrap().to_string();
    refused(&mut v, "variable_collection.remove_mode", json!({"collection": single, "mode": only}), "mode_remove_last");
    let aliased = refused(&mut v, "variable.remove", json!({"variable": primitive}), "variable_aliased");
    assert!(aliased["message"].as_str().is_some_and(|m| m.contains(&semantic)), "{aliased}");
    refused(&mut v, "variable_collection.rename_mode", json!({"collection": collection, "mode": dark, "name": "Light"}), "mode_name_taken");
    refused(&mut v, "variable_collection.add_mode", json!({"collection": collection, "name": ""}), "mode_name_invalid");
}

/// Removing a mode or a collection takes its values and the explicit modes naming it in the same undo
/// step; removing the default mode moves the default to the first remaining mode.
#[test]
fn removing_a_mode_or_a_collection_takes_its_values_and_explicit_modes_in_one_step() {
    let mut v = design_doc();
    let (collection, light, dark) = tokens(&mut v);
    let (primitive, semantic) = primitive_and_alias(&mut v, &collection, &light, &dark);
    let shape = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [50.0, 50.0], false).unwrap();
    let card = v.group_layers(&[shape.clone()], "Card").unwrap();
    ok(&mut v, "node.set_explicit_variable_mode_for_collection", json!({"node": card, "collection": collection, "mode": dark}));
    let before = vector_bytes(&v);
    let depth = undo_depth(&v);
    let removed = ok(&mut v, "variable_collection.remove_mode", json!({"collection": collection, "mode": dark}));
    assert_eq!(undo_depth(&v), depth + 1);
    assert_committed_as(&v, "variable_collection.remove_mode", &removed);
    assert_eq!(removed["explicit_modes_cleared"], 1, "{removed}");
    let state = variables(&v);
    for var in state["variables"].as_array().unwrap() {
        assert_eq!(var["values_by_mode"].as_object().unwrap().keys().collect::<Vec<_>>(), vec![&light], "{var}");
    }
    let r = ok(&mut v, "variable.resolve_for_consumer", json!({"variable": semantic, "node": shape}));
    assert_eq!(r["mode_id"], light.as_str(), "the explicit mode went with its mode: {r}");
    v.undo().unwrap();
    assert_eq!(vector_bytes(&v), before, "one undo restores the mode, its values and the explicit mode");
    // The default mode removed: the next mode becomes the default.
    ok(&mut v, "variable_collection.remove_mode", json!({"collection": collection, "mode": light}));
    assert_eq!(variables(&v)["collections"][0]["default_mode_id"], dark.as_str());
    v.undo().unwrap();
    // The collection removed: its variables and explicit modes go in the same step.
    let gone = ok(&mut v, "variable_collection.remove", json!({"collection": collection}));
    assert_committed_as(&v, "variable_collection.remove", &gone);
    assert_eq!((gone["variables_removed"].as_u64(), gone["explicit_modes_cleared"].as_u64()), (Some(2), Some(1)), "{gone}");
    assert_eq!(variables(&v)["variables"], json!([]));
    refused(&mut v, "variable.resolve_for_consumer", json!({"variable": primitive, "node": shape}), "variable_unknown");
    v.undo().unwrap();
    assert_eq!(vector_bytes(&v), before);
    // A variable nothing aliases is removed in one step.
    let removed = ok(&mut v, "variable.remove", json!({"variable": semantic}));
    assert_committed_as(&v, "variable.remove", &removed);
    assert_eq!(variables(&v)["collections"][0]["variable_ids"], json!([primitive]));
}

/// A deleted container's explicit modes go in the delete's undo step (lead common ruling: a deleted
/// object's records are removed in the same undo step, and undo restores both).
#[test]
fn deleting_a_container_takes_its_explicit_modes_in_the_same_step() {
    let mut v = design_doc();
    let (collection, _, dark) = tokens(&mut v);
    let shape = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [50.0, 50.0], false).unwrap();
    let card = v.group_layers(&[shape], "Card").unwrap();
    ok(&mut v, "node.set_explicit_variable_mode_for_collection", json!({"node": card, "collection": collection, "mode": dark}));
    let before = vector_bytes(&v);
    let depth = undo_depth(&v);
    v.delete_layer(&card).unwrap();
    assert_eq!(undo_depth(&v), depth + 1);
    let design: Value = serde_json::from_slice(&vector_bytes(&v)).unwrap();
    let nodes = design["design"]["nodes"].as_array().cloned().unwrap_or_default();
    assert!(!nodes.iter().any(|n| n["node"]["node_id"] == card.as_str()), "{nodes:?}");
    v.undo().unwrap();
    assert_eq!(vector_bytes(&v), before);
}

/// Every record survives save and reopen and takes further commands ([STU-DS-121], [STU-DS-122]).
#[test]
fn variables_survive_save_and_reopen() {
    let scratch = Scratch::new("ds-variables-reopen");
    let mut v = design_doc();
    let (collection, light, dark) = tokens(&mut v);
    let (primitive, semantic) = primitive_and_alias(&mut v, &collection, &light, &dark);
    ok(&mut v, "variable.set_code_syntax", json!({"variable": semantic, "platform": "ANDROID", "name": "color_accent"}));
    let shape = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [50.0, 50.0], false).unwrap();
    let card = v.group_layers(&[shape.clone()], "Card").unwrap();
    ok(&mut v, "node.set_explicit_variable_mode_for_collection", json!({"node": card, "collection": collection, "mode": dark}));
    let state = variables(&v);
    let resolved = ok(&mut v, "variable.resolve_for_consumer", json!({"variable": semantic, "node": shape}));
    let path = scratch.0.join("variables.handshake");
    v.save(&path, false).unwrap();
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&path).unwrap();
    assert_eq!(variables(&w), state, "the records reopen as saved");
    assert_eq!(ok(&mut w, "variable.resolve_for_consumer", json!({"variable": semantic, "node": shape})), resolved);
    ok(&mut w, "variable_collection.add_mode", json!({"collection": collection, "name": "High contrast"}));
    let modes = variables(&w)["collections"][0]["modes"].as_array().unwrap().len();
    assert_eq!(modes, 3);
    // A new mode takes the default mode's value of every variable (Studio reading, UNVERIFIED D5).
    let var = variables(&w)["variables"].as_array().unwrap().iter().find(|x| x["variable_id"] == primitive.as_str()).unwrap().clone();
    assert_eq!(var["values_by_mode"].as_object().unwrap().len(), 3, "{var}");
}

fn node_records(v: &Viewer) -> Vec<Value> {
    let entry: Value = serde_json::from_slice(&vector_bytes(v)).unwrap();
    entry["design"]["nodes"].as_array().cloned().unwrap_or_default()
}

fn explicit_modes_of(v: &Viewer, node: &str) -> Value {
    node_records(v).into_iter().find(|n| n["node"]["node_id"] == node).map_or(Value::Null, |n| n["explicit_variable_modes"].clone())
}

fn children_of(v: &Viewer, parent: &str) -> Vec<(String, String)> {
    v.run().snapshot().unwrap().document().layers.iter().filter(|l| serde_json::to_value(&l.parent).unwrap()["node_id"] == parent).map(|l| (l.layer_id.clone(), l.kind.clone())).collect()
}

/// Review F6: an artboard's explicit modes go in the artboard delete's own undo step, and undo brings both
/// back (lead common ruling on deleted objects' records).
#[test]
fn deleting_an_artboard_takes_its_explicit_modes_in_the_same_step() {
    let mut v = design_doc();
    let (collection, _, dark) = tokens(&mut v);
    let board = v.add_artboard("Board", 200.0, 100.0, Some([0.0, 0.0])).unwrap();
    v.add_artboard("Other", 200.0, 100.0, Some([300.0, 0.0])).unwrap();
    ok(&mut v, "node.set_explicit_variable_mode_for_collection", json!({"node": board, "collection": collection, "mode": dark}));
    let before = vector_bytes(&v);
    let depth = undo_depth(&v);
    v.delete_artboard(&board).unwrap();
    assert_eq!(undo_depth(&v), depth + 1, "the artboard and its records go in one step");
    assert_eq!(explicit_modes_of(&v, &board), Value::Null, "no record of a deleted artboard stays: {:?}", node_records(&v));
    v.undo().unwrap();
    assert_eq!(vector_bytes(&v), before, "undo brings the explicit modes back");
    assert_eq!(explicit_modes_of(&v, &board)[&collection], dark.as_str());
    v.redo().unwrap();
    assert_eq!(explicit_modes_of(&v, &board), Value::Null);
}

/// Review F6: a duplicated container keeps its explicit modes, in the duplicate's one undo step (Figma
/// behaviour UNVERIFIED, D5).
#[test]
fn a_duplicated_container_keeps_its_explicit_modes() {
    let mut v = design_doc();
    let (collection, light, dark) = tokens(&mut v);
    let (_, semantic) = primitive_and_alias(&mut v, &collection, &light, &dark);
    let shape = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [50.0, 50.0], false).unwrap();
    let card = v.group_layers(&[shape], "Card").unwrap();
    ok(&mut v, "node.set_explicit_variable_mode_for_collection", json!({"node": card, "collection": collection, "mode": dark}));
    let depth = undo_depth(&v);
    let copy = v.duplicate_layer(&card).unwrap();
    assert_eq!(undo_depth(&v), depth + 1, "one step");
    assert_eq!(explicit_modes_of(&v, &copy)[&collection], dark.as_str(), "{:?}", node_records(&v));
    let inner = children_of(&v, &copy)[0].0.clone();
    let r = ok(&mut v, "variable.resolve_for_consumer", json!({"variable": semantic, "node": inner}));
    assert_eq!(r["mode_id"], dark.as_str(), "the copy's content resolves in the copied mode: {r}");
    v.undo().unwrap();
    assert_eq!(explicit_modes_of(&v, &copy), Value::Null, "undo takes the copy's record with the copy");
}

/// Review F6: an explicit mode set on a container inside an instance (a copy of a definition group) moves to
/// the copy a sync draws in its place, in the sync's own undo step; undo brings the old copy and its mode back.
#[test]
fn a_sync_moves_an_explicit_mode_to_the_copy_that_replaces_its_container() {
    let mut v = design_doc();
    let (collection, light, dark) = tokens(&mut v);
    let (_, semantic) = primitive_and_alias(&mut v, &collection, &light, &dark);
    let a = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [50.0, 50.0], false).unwrap();
    let b = v.draw_shape(ShapeKind::Ellipse, [60.0, 10.0], [90.0, 40.0], false).unwrap();
    let inner = v.group_layers(&[a], "Inner").unwrap();
    v.select(None);
    let component = text(&ok(&mut v, "component.create_from_selection", json!({"layers": [inner, b], "name": "Card"})), "component_id");
    let frame = text(&ok(&mut v, "component.create_instance", json!({"component": component, "x": 300, "y": 10})), "layer_id");
    let copy_group = |v: &Viewer| children_of(v, &frame).into_iter().find(|(_, k)| k == "group").map(|(id, _)| id).expect("a copy of the inner group");
    let old = copy_group(&v);
    ok(&mut v, "node.set_explicit_variable_mode_for_collection", json!({"node": old, "collection": collection, "mode": dark}));
    let depth = undo_depth(&v);
    ok(&mut v, "component.sync_instances", json!({"component": component}));
    assert_eq!(undo_depth(&v), depth + 1);
    let new = copy_group(&v);
    assert_ne!(new, old, "the sync drew a new copy");
    assert_eq!(explicit_modes_of(&v, &new)[&collection], dark.as_str(), "the mode moved to the new copy: {:?}", node_records(&v));
    assert_eq!(explicit_modes_of(&v, &old), Value::Null, "nothing is left on the deleted copy");
    let leaf = children_of(&v, &new)[0].0.clone();
    assert_eq!(ok(&mut v, "variable.resolve_for_consumer", json!({"variable": semantic, "node": leaf}))["mode_id"], dark.as_str());
    v.undo().unwrap();
    assert_eq!(copy_group(&v), old);
    assert_eq!(explicit_modes_of(&v, &old)[&collection], dark.as_str(), "undo brings the old copy and its mode back");
}

/// An alias into another collection resolves in that collection's mode as the consumer sees it
/// ([STU-DS-026], [STU-DS-132]: each collection's mode is chosen independently).
#[test]
fn an_alias_into_another_collection_resolves_in_that_collections_mode() {
    let mut v = design_doc();
    let (primitives, light, dark) = tokens(&mut v);
    let brand = text(&ok(&mut v, "variable_collection.create", json!({"name": "Brand", "mode_name": "Acme"})), "collection_id");
    let (blue, _) = primitive_and_alias(&mut v, &primitives, &light, &dark);
    let alias = ok(&mut v, "variable.create_alias", json!({"variable": blue}));
    let accent = text(&ok(&mut v, "variable.create", json!({"collection": brand, "name": "brand/accent", "resolved_type": "COLOR", "value": alias})), "variable_id");
    let shape = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [50.0, 50.0], false).unwrap();
    let card = v.group_layers(&[shape.clone()], "Card").unwrap();
    let r = ok(&mut v, "variable.resolve_for_consumer", json!({"variable": accent, "node": shape}));
    assert_eq!((r["value"].clone(), r["collection_id"].as_str()), (colour(0.0, 0.0, 1.0), Some(brand.as_str())), "{r}");
    ok(&mut v, "node.set_explicit_variable_mode_for_collection", json!({"node": card, "collection": primitives, "mode": dark}));
    let r = ok(&mut v, "variable.resolve_for_consumer", json!({"variable": accent, "node": shape}));
    assert_eq!(r["value"], colour(0.5, 0.5, 1.0), "the alias target is read in its own collection's mode: {r}");
    assert_eq!(r["chain"], json!([accent, blue]));
}

/// Review F7, [STU-DS-123]: the UserManual states the advisory threshold and the design entry's state bound.
#[test]
fn the_manual_states_the_variable_advisory_threshold_and_the_sync_bound() {
    let limits = hsk_studio_harness_core::manual::DRIVE_LIMITS;
    let threshold = hsk_studio_harness::view::design::variables::VARIABLE_VALUE_ADVISORY.to_string();
    assert!(limits.iter().any(|l| l.contains("variables_times_modes_over_threshold") && l.contains(&threshold)), "{limits:?}");
    assert!(limits.iter().any(|l| l.contains("design_state_over_cap") && l.contains("component.sync_instances")), "{limits:?}");
}
