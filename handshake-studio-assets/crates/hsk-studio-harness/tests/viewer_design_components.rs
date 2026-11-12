//! Lane SMB-DS-COMPONENTS-AUTHOR-1 (feature `viewer`): components, variant sets, component properties
//! and instances authored in a native document by the spec's typed commands ([STU-DS-002/003/003a/004/
//! 102..113/162]), driven through `Viewer::run_command` as the process and a panel drive them. Every
//! command is one undo step; the records live in the vector entry's `design` member and the instance
//! bindings in the `design` entry, so the existing `design.instance.*` commands work on a native
//! instance without a design file. Figma behaviour for create, combine and detach is UNVERIFIED (D5).
#![cfg(feature = "viewer")]

mod common;

use common::Scratch;
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::vector::arrange::ReferencePoint;
use hsk_studio_harness::view::vector::{ShapeKind, TransformOp, VectorTool};
use hsk_studio_harness_core::document_new::{DocumentProfile, NewDocument};
use serde_json::{Value, json};

const INTER: &[u8] = include_bytes!("../../hsk-studio-type/tests/fixtures/fonts/Inter-Regular.ttf");

fn design_doc() -> Viewer {
    let mut v = Viewer::new(Prefer::Cpu);
    v.new_document(&NewDocument::new(DocumentProfile::Design, None), None).unwrap();
    v.set_vector_tool(VectorTool { fill_on: true, fill: [200, 0, 0], stroke_on: false, ..VectorTool::default() });
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

fn refused(v: &mut Viewer, id: &str, params: Value, code: &str) -> Value {
    match call(v, id, params) {
        Ok(r) => panic!("{id} should refuse {code}: {r}"),
        Err((c, detail)) => {
            assert_eq!(c, code, "{id}: {detail}");
            detail
        }
    }
}

fn text(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or_else(|| panic!("no {key} in {v}")).to_string()
}

/// The document's layers and its vector entry bytes (what an undo must restore).
fn snapshot(v: &Viewer) -> (String, Vec<u8>) {
    let doc = v.run().snapshot().unwrap().document();
    (serde_json::to_string(&doc.layers).unwrap(), v.run().document_entry("vector").map(<[u8]>::to_vec).unwrap_or_default())
}

fn children(v: &Viewer, layer: &str) -> Vec<String> {
    let doc = v.run().snapshot().unwrap().document();
    doc.layers.iter().filter(|l| serde_json::to_value(&l.parent).unwrap()["node_id"] == layer).map(|l| l.layer_id.clone()).collect()
}

fn layer(v: &Viewer, id: &str) -> Value {
    let doc = v.run().snapshot().unwrap().document();
    serde_json::to_value(doc.layers.iter().find(|l| l.layer_id == id).unwrap_or_else(|| panic!("no layer {id}"))).unwrap()
}

fn has_layer(v: &Viewer, id: &str) -> bool {
    v.run().snapshot().unwrap().document().layers.iter().any(|l| l.layer_id == id)
}

fn min_x(v: &Viewer, layer: &str) -> f64 {
    v.layer_bounds(layer).unwrap().min[0]
}

/// A button: background rectangle and an icon ellipse, made one component named `name`.
fn button(v: &mut Viewer, name: &str, x: f64) -> (String, String, String) {
    v.select(None);
    let bg = v.draw_shape(ShapeKind::Rectangle, [x, 10.0], [x + 60.0, 30.0], false).unwrap();
    let icon = v.draw_shape(ShapeKind::Ellipse, [x + 5.0, 15.0], [x + 15.0, 25.0], false).unwrap();
    let made = ok(v, "component.create_from_selection", json!({"layers": [bg, icon], "name": name}));
    v.select(None);
    (text(&made, "component_id"), bg, icon)
}

fn inspect(v: &mut Viewer, layer: &str) -> Value {
    ok(v, "design.instance.inspect", json!({"layer": layer}))
}

#[test]
fn create_from_selection_groups_the_shapes_into_one_component_in_one_undo_step() {
    let mut v = design_doc();
    let bg = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [70.0, 30.0], false).unwrap();
    let icon = v.draw_shape(ShapeKind::Ellipse, [15.0, 15.0], [25.0, 25.0], false).unwrap();
    v.select(None);
    let before = snapshot(&v);
    let made = ok(&mut v, "component.create_from_selection", json!({"layers": [bg, icon], "name": "Buttons/Primary"}));
    let (component, group) = (text(&made, "component_id"), text(&made, "layer_id"));
    assert!(component.starts_with("SCMP-"), "{made}");
    assert_eq!(made["grouped"], true);
    assert_eq!(layer(&v, &group)["name"], "Buttons/Primary");
    assert_eq!(children(&v, &group), vec![bg.clone(), icon.clone()]);
    let listed = v.state_json()["design"]["components"].clone();
    assert_eq!(listed["components"][0]["component_id"], component, "{listed}");
    assert_eq!(listed["components"][0]["name"], "Buttons/Primary");
    // One undo restores the layers and the vector entry byte for byte; redo brings the component back.
    v.undo().unwrap();
    assert_eq!(snapshot(&v), before);
    v.redo().unwrap();
    assert_eq!(v.state_json()["design"]["components"]["components"][0]["component_id"], component);
}

#[test]
fn a_group_becomes_a_component_as_an_entry_only_step_and_bulk_creation_makes_one_per_node() {
    let mut v = design_doc();
    let a = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [20.0, 20.0], false).unwrap();
    let g = v.group_layers(&[a], "Card").unwrap();
    v.select(None);
    let layers_before = snapshot(&v).0;
    let made = ok(&mut v, "component.create_from_selection", json!({"layers": [g]}));
    assert_eq!(made["grouped"], false, "{made}");
    assert_eq!(text(&made, "layer_id"), g);
    assert_eq!(snapshot(&v).0, layers_before, "no structure change");
    // A name that differs from the group's is refused: the group keeps its own name (layer.rename).
    v.undo().unwrap();
    refused(&mut v, "component.create_from_selection", json!({"layers": [g], "name": "Other"}), "component_name_mismatch");
    // Bulk (STU-DS-002): one component per listed top-level node, one step.
    let b = v.draw_shape(ShapeKind::Rectangle, [40.0, 10.0], [50.0, 20.0], false).unwrap();
    v.select(None);
    let before = snapshot(&v);
    let bulk = ok(&mut v, "component.create_one_per_top_level_node", json!({"layers": [g, b]}));
    let list = bulk["components"].as_array().unwrap();
    assert_eq!(list.len(), 2, "{bulk}");
    assert_eq!(list[0]["layer_id"], g.as_str());
    let wrapped = list[1]["layer_id"].as_str().unwrap().to_string();
    assert_eq!(children(&v, &wrapped), vec![b.clone()], "a leaf is wrapped in its own group");
    assert_eq!(layer(&v, &wrapped)["name"], "Rectangle");
    v.undo().unwrap();
    assert_eq!(snapshot(&v), before);
    // A node and its own child are not two top-level nodes.
    v.select(None);
    let inner = children(&v, &g)[0].clone();
    refused(&mut v, "component.create_one_per_top_level_node", json!({"layers": [g, inner]}), "selection_not_top_level");
}

#[test]
fn an_instance_copies_the_definition_and_reads_edited_after_save_and_reopen() {
    let scratch = Scratch::new("ds-instance");
    let mut v = design_doc();
    let (component, _, icon) = button(&mut v, "Button", 10.0);
    let before = snapshot(&v);
    let made = ok(&mut v, "component.create_instance", json!({"component": component, "x": 200, "y": 100}));
    let (instance, frame) = (text(&made, "instance_id"), text(&made, "layer_id"));
    assert!(instance.starts_with("SCIN-"), "{made}");
    assert_eq!(made["main_component_id"], component.as_str());
    let kids = children(&v, &frame);
    assert_eq!(kids.len(), 2, "{made}");
    assert!((min_x(&v, &kids[0]) - 200.0).abs() < 1e-9, "the instance's top-left is (x, y)");
    let view = inspect(&mut v, &frame);
    assert_eq!(view["state"], "edited", "{view}");
    assert_eq!(view["main_component_id"], component.as_str());
    assert_eq!(view["component_name"], "Button");
    // One undo removes the instance (layers and vector entry byte for byte); redo returns it.
    v.undo().unwrap();
    assert_eq!(snapshot(&v), before);
    v.redo().unwrap();
    assert_eq!(inspect(&mut v, &frame)["state"], "edited");
    // The existing instance commands run on it (no design file): hide the icon by override.
    let over = ok(&mut v, "design.instance.set_override", json!({"layer": frame, "node_id": icon, "field": "visible", "value": false}));
    assert_eq!(over["main_component_id"], component.as_str(), "{over}");
    let hidden = children(&v, &frame).into_iter().find(|k| layer(&v, k)["name"] == "Ellipse").unwrap();
    assert_eq!(layer(&v, &hidden)["visible"], false);
    // Save and reopen: the instance still reads edited and takes another command.
    let path = scratch.0.join("instance.handshake");
    v.save(&path, false).unwrap();
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&path).unwrap();
    let again = inspect(&mut w, &frame);
    assert_eq!(again["state"], "edited", "{again}");
    assert_eq!(again["overrides"][0]["field"], "visible");
    ok(&mut w, "design.instance.reset_all", json!({"layer": frame}));
    assert!(children(&w, &frame).iter().all(|k| layer(&w, k)["visible"] == true));
    // Queries: the definition lists its instance; the instance names its main component.
    let listed = ok(&mut w, "component.list_instances", json!({"component": component}));
    assert_eq!(listed["instances"][0]["layer_id"], frame.as_str(), "{listed}");
    let main = ok(&mut w, "instance.get_main_component", json!({"layer": frame}));
    assert_eq!(main["main_component_id"], component.as_str(), "{main}");
}

#[test]
fn combine_as_variants_refuses_duplicates_before_commit_and_instances_switch_variant() {
    let mut v = design_doc();
    let (a, ..) = button(&mut v, "Default", 10.0);
    let (b, ..) = button(&mut v, "Hover", 100.0);
    let props = json!({"State": ["Default", "Hover"]});
    // Step 26: two members with one assignment, and a member without one, refused before anything commits.
    let before = snapshot(&v);
    refused(&mut v, "component_set.combine_as_variants", json!({"components": [a, b], "properties": props, "assignments": {a.clone(): {"State": "Default"}, b.clone(): {"State": "Default"}}}), "variant_assignment_duplicate");
    refused(&mut v, "component_set.combine_as_variants", json!({"components": [a, b], "properties": props, "assignments": {a.clone(): {"State": "Default"}}}), "variant_assignment_incomplete");
    refused(&mut v, "component_set.combine_as_variants", json!({"components": [a, b], "properties": props, "assignments": {a.clone(): {"State": "Default"}, b.clone(): {"State": "Pressed"}}}), "variant_value_undeclared");
    assert_eq!(snapshot(&v), before);
    let set = ok(&mut v, "component_set.combine_as_variants", json!({"components": [a, b], "properties": props, "assignments": {a.clone(): {"State": "Default"}, b.clone(): {"State": "Hover"}}, "name": "Button"}));
    let set_id = text(&set, "set_id");
    assert_eq!(set["default_variant_id"], a.as_str());
    assert_eq!(children(&v, &text(&set, "layer_id")).len(), 2);
    v.undo().unwrap();
    assert_eq!(snapshot(&v), before);
    v.redo().unwrap();
    // An instance of the set draws its default member; a variant switch redraws it as Hover.
    let made = ok(&mut v, "component.create_instance", json!({"component": set_id, "x": 300, "y": 10}));
    assert_eq!(made["main_component_id"], a.as_str(), "{made}");
    let frame = text(&made, "layer_id");
    let view = inspect(&mut v, &frame);
    assert_eq!(view["variants"]["State"]["current"], "Default", "{view}");
    let switched = ok(&mut v, "design.instance.switch_variant", json!({"layer": frame, "property": "State", "value": "Hover"}));
    assert_eq!(switched["component_id"], b.as_str(), "{switched}");
    assert_eq!(inspect(&mut v, &frame)["variants"]["State"]["current"], "Hover");
    // The default variant moves; a non-member is refused.
    let moved = ok(&mut v, "component_set.set_default_variant", json!({"set": set_id, "component": b}));
    assert_eq!(moved["default_variant_id"], b.as_str());
    refused(&mut v, "component_set.set_default_variant", json!({"set": set_id, "component": set_id}), "component_not_a_member");
}

#[test]
fn properties_drive_the_redraw_and_a_rename_rewrites_every_instance_key() {
    let mut v = design_doc();
    let (component, _, icon) = button(&mut v, "Button", 10.0);
    ok(&mut v, "component.add_property", json!({"component": component, "name": "Show icon", "type": "BOOLEAN", "default": true}));
    refused(&mut v, "component.add_property", json!({"component": component, "name": "Show icon", "type": "BOOLEAN", "default": true}), "property_name_taken");
    refused(&mut v, "component.add_property", json!({"component": component, "name": "Body", "type": "SLOT", "default": ""}), "slot_not_supported");
    refused(&mut v, "component.add_property", json!({"component": component, "name": "Label", "type": "TEXT", "default": false}), "property_default_type");
    ok(&mut v, "component.add_property", json!({"component": component, "name": "Size", "type": "TEXT", "default": "M"}));
    // STU-DS-113: the icon's visibility follows the property (the plain property name: Studio reading).
    ok(&mut v, "component.set_property_reference", json!({"layer": icon, "field": "visible", "property": "Show icon"}));
    refused(&mut v, "component.set_property_reference", json!({"layer": icon, "field": "visible", "property": "Size"}), "property_reference_type");
    let made = ok(&mut v, "component.create_instance", json!({"component": component, "x": 200, "y": 10}));
    let frame = text(&made, "layer_id");
    ok(&mut v, "design.instance.set_property", json!({"layer": frame, "name": "Show icon", "kind": "boolean", "value": false}));
    let icon_copy = children(&v, &frame).into_iter().find(|k| layer(&v, k)["name"] == "Ellipse").unwrap();
    assert_eq!(layer(&v, &icon_copy)["visible"], false, "the property hides the icon");
    // Rename: the instance value follows the new key in the same step (STU-DS-112); a collision is refused.
    refused(&mut v, "component.edit_property", json!({"component": component, "name": "Show icon", "new_name": "Size"}), "property_name_taken");
    let renamed = ok(&mut v, "component.edit_property", json!({"component": component, "name": "Show icon", "new_name": "Icon", "default": false}));
    assert_eq!(renamed["instances_rewritten"], 1, "{renamed}");
    let view = inspect(&mut v, &frame);
    let names: Vec<&str> = view["properties"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"Icon") && !names.contains(&"Show icon"), "{view}");
    assert_eq!(view["properties"].as_array().unwrap().iter().find(|p| p["name"] == "Icon").unwrap()["value"], false);
    // Delete: the reference and the instance values go with it.
    let gone = ok(&mut v, "component.delete_property", json!({"component": component, "name": "Icon"}));
    assert_eq!(gone["instances_rewritten"], 1, "{gone}");
    refused(&mut v, "component.delete_property", json!({"component": component, "name": "Icon"}), "property_unknown");
    // Description (STU-DS-004).
    ok(&mut v, "component.set_description", json!({"component": component, "description": "Primary action", "documentation_links": [{"name": "Guide", "url": "https://example.invalid/button"}]}));
    let listed = v.state_json()["design"]["components"]["components"][0].clone();
    assert_eq!(listed["description"], "Primary action", "{listed}");
}

#[test]
fn detach_keeps_the_frame_and_sync_redraws_after_a_definition_edit() {
    let mut v = design_doc();
    let (component, bg, icon) = button(&mut v, "Button", 10.0);
    let first = text(&ok(&mut v, "component.create_instance", json!({"component": component, "x": 200, "y": 10})), "layer_id");
    let second = text(&ok(&mut v, "component.create_instance", json!({"component": component, "x": 300, "y": 10})), "layer_id");
    ok(&mut v, "design.instance.set_override", json!({"layer": second, "node_id": icon, "field": "opacity", "value": 0.5}));
    // Edit the definition: the background moves 5 px right; instances keep the old copy until a sync.
    v.transform_layers(std::slice::from_ref(&bg), TransformOp::Move { dx: 5.0, dy: 0.0 }, ReferencePoint::Centre).unwrap();
    let synced = ok(&mut v, "component.sync_instances", json!({"component": component}));
    assert_eq!(synced["redrawn"].as_array().unwrap().len(), 2, "{synced}");
    let bg_copy = children(&v, &second).into_iter().find(|k| layer(&v, k)["name"] == "Rectangle").unwrap();
    assert!((min_x(&v, &bg_copy) - 300.0).abs() < 1e-9, "the instance stays anchored at its top-left");
    let icon_copy = children(&v, &second).into_iter().find(|k| layer(&v, k)["name"] == "Ellipse").unwrap();
    assert_eq!(layer(&v, &icon_copy)["appearance"]["opacity"], 50.0, "the override survives the sync");
    // Detach: the frame stays with plain children; the instance record and binding go.
    let detached = ok(&mut v, "instance.detach", json!({"layer": first}));
    assert_eq!(detached["layer_id"], first.as_str(), "{detached}");
    assert!(has_layer(&v, &first) && children(&v, &first).len() == 2);
    refused(&mut v, "design.instance.inspect", json!({"layer": first}), "not_an_instance");
    refused(&mut v, "instance.get_main_component", json!({"layer": first}), "not_an_instance");
    let listed = ok(&mut v, "component.list_instances", json!({"component": component}));
    assert_eq!(listed["instances"].as_array().unwrap().len(), 1, "{listed}");
    v.undo().unwrap();
    assert_eq!(inspect(&mut v, &first)["state"], "edited");
}

#[test]
fn a_deleted_definition_leaves_its_instances_rendering_with_no_main() {
    let mut v = design_doc();
    let (component, ..) = button(&mut v, "Button", 10.0);
    let frame = text(&ok(&mut v, "component.create_instance", json!({"component": component, "x": 200, "y": 10})), "layer_id");
    let definition = v.state_json()["design"]["components"]["components"][0]["layer_id"].as_str().unwrap().to_string();
    v.delete_layer(&definition).unwrap();
    // STU-DS-003a: the instance keeps its layers and reads main_component_id null.
    assert_eq!(children(&v, &frame).len(), 2);
    let main = ok(&mut v, "instance.get_main_component", json!({"layer": frame}));
    assert_eq!(main["main_component_id"], Value::Null, "{main}");
    assert!(v.state_json()["design"]["components"]["components"].as_array().unwrap().is_empty());
    // Authoring still works in the document (the dangling record is settled, not refused whole).
    let (other, ..) = button(&mut v, "Other", 400.0);
    ok(&mut v, "component.create_instance", json!({"component": other, "x": 400, "y": 100}));
}

#[test]
fn hostile_inputs_are_typed_refusals_that_change_nothing() {
    let mut v = design_doc();
    let (component, _, icon) = button(&mut v, "Button", 10.0);
    let before = snapshot(&v);
    let many: Vec<String> = (0..4096).map(|i| format!("SLYR-019abcde-0000-7000-8000-{i:012x}")).collect();
    refused(&mut v, "component.create_from_selection", json!({"layers": many}), "selection_over_cap");
    refused(&mut v, "component.create_from_selection", json!({"layers": []}), "selection_empty");
    refused(&mut v, "component.create_from_selection", json!({"layers": ["SLYR-019abcde-0000-7000-8000-00000000ffff"]}), "layer_unknown");
    refused(&mut v, "component.create_from_selection", json!({"layers": [icon]}), "component_nested");
    refused(&mut v, "component.create_instance", json!({"component": component, "x": 1.0e12, "y": 0}), "instance_coordinate_out_of_range");
    refused(&mut v, "component.create_instance", json!({"component": component, "x": "NaN", "y": 0}), "parameter_invalid");
    refused(&mut v, "component.create_instance", json!({"component": "SCMP-019abcde-0000-7000-8000-000000000001", "x": 0, "y": 0}), "component_unknown");
    let a = v.draw_shape(ShapeKind::Rectangle, [500.0, 10.0], [510.0, 20.0], false).unwrap();
    v.select(None);
    refused(&mut v, "component.create_from_selection", json!({"layers": [a], "name": "A//B"}), "component_name_invalid");
    refused(&mut v, "component.create_from_selection", json!({"layers": [a], "name": "x".repeat(300)}), "component_name_invalid");
    let after_draw = snapshot(&v);
    // A cycle: an instance of the button placed inside the button's own definition.
    let definition = v.state_json()["design"]["components"]["components"][0]["layer_id"].as_str().unwrap().to_string();
    refused(&mut v, "component.create_instance", json!({"component": component, "x": 0, "y": 0, "parent": definition}), "component_cycle");
    assert_eq!(snapshot(&v), after_draw);
    assert_ne!(before, after_draw);
}

// Review fa15c84d (research/review-smb-ds-components-author-1-fa15c84d.json), lead rulings 2026-11-11.

fn main_of(v: &mut Viewer, frame: &str) -> Value {
    ok(v, "instance.get_main_component", json!({"layer": frame}))["main_component_id"].clone()
}

fn states(v: &Viewer) -> u64 {
    v.state_json()["design"]["entry"]["states"].as_u64().unwrap_or(0)
}

/// F1 (a): a deleted definition's instance stays an instance with main null after an unrelated write,
/// after save and reopen, and undo of the delete brings its main back (STU-DS-003a, STU-DS-105).
#[test]
fn f1_a_deleted_definition_keeps_its_instance_bound_across_writes_reopen_and_undo() {
    let scratch = Scratch::new("ds-f1-deleted-main");
    let mut v = design_doc();
    let (a, ..) = button(&mut v, "A", 10.0);
    let (b, ..) = button(&mut v, "B", 100.0);
    let frame = text(&ok(&mut v, "component.create_instance", json!({"component": a, "x": 200, "y": 10})), "layer_id");
    let definition = v.state_json()["design"]["components"]["components"][0]["layer_id"].as_str().unwrap().to_string();
    v.delete_layer(&definition).unwrap();
    assert_eq!(main_of(&mut v, &frame), Value::Null);
    // An unrelated write of the design entry.
    ok(&mut v, "component.create_instance", json!({"component": b, "x": 300, "y": 10}));
    assert_eq!(main_of(&mut v, &frame), Value::Null, "the instance stays bound with main null");
    let path = scratch.0.join("deleted-main.handshake");
    v.save(&path, false).unwrap();
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&path).unwrap();
    assert_eq!(main_of(&mut w, &frame), Value::Null, "after reopen");
    v.undo().unwrap();
    v.undo().unwrap();
    assert_eq!(main_of(&mut v, &frame), json!(a), "undo of the delete restores the main");
}

/// F1 (b), (c) and F4: an instance layer delete and a detach, each followed by another write, are
/// undone with their binding; a detach of the newest instance never makes its id minted again.
#[test]
fn f1_a_deleted_or_detached_instance_comes_back_bound_on_undo() {
    let mut v = design_doc();
    let (a, ..) = button(&mut v, "A", 10.0);
    let first = ok(&mut v, "component.create_instance", json!({"component": a, "x": 200, "y": 10}));
    let frame = text(&first, "layer_id");
    v.delete_layer(&frame).unwrap();
    ok(&mut v, "component.create_instance", json!({"component": a, "x": 300, "y": 10}));
    v.undo().unwrap();
    v.undo().unwrap();
    assert_eq!(main_of(&mut v, &frame), json!(a), "the deleted instance is back with its binding");
    // Detach the newest instance, then create another: the id is not minted again (F4).
    ok(&mut v, "instance.detach", json!({"layer": frame}));
    let next = ok(&mut v, "component.create_instance", json!({"component": a, "x": 400, "y": 10}));
    assert_ne!(next["instance_id"], first["instance_id"], "{next}");
    v.undo().unwrap();
    v.undo().unwrap();
    assert_eq!(main_of(&mut v, &frame), json!(a), "the detached instance is back with its binding");
}

/// F1 (d) and F6: an override of a definition child that is then deleted unbinds nothing, and a sync
/// refuses that instance only while it redraws the others.
#[test]
fn f6_a_stale_override_refuses_its_instance_only_and_keeps_every_binding() {
    let mut v = design_doc();
    let (a, bg, icon) = button(&mut v, "A", 10.0);
    let stale = text(&ok(&mut v, "component.create_instance", json!({"component": a, "x": 200, "y": 10})), "layer_id");
    let fine = text(&ok(&mut v, "component.create_instance", json!({"component": a, "x": 300, "y": 10})), "layer_id");
    ok(&mut v, "design.instance.set_override", json!({"layer": stale, "node_id": icon, "field": "visible", "value": false}));
    v.delete_layer(&icon).unwrap();
    // Another instance command writes the design entry.
    ok(&mut v, "design.instance.set_override", json!({"layer": fine, "node_id": bg, "field": "opacity", "value": 0.5}));
    assert_eq!(main_of(&mut v, &stale), json!(a), "the stale instance keeps its binding");
    let synced = ok(&mut v, "component.sync_instances", json!({"component": a}));
    assert_eq!(synced["redrawn"], json!([fine]), "{synced}");
    assert_eq!(synced["refused"][0]["layer_id"], stale.as_str(), "{synced}");
    assert_eq!(synced["refused"][0]["code"], "override_target_gone", "{synced}");
    assert_eq!(main_of(&mut v, &stale), json!(a));
}

/// F2: more than 256 instance states never unbind the first instance (no eviction).
#[test]
fn f2_many_redraws_never_unbind_the_first_instance() {
    let mut v = design_doc();
    let (a, ..) = button(&mut v, "A", 10.0);
    let first = text(&ok(&mut v, "component.create_instance", json!({"component": a, "x": 200, "y": 10})), "layer_id");
    for i in 0..256 {
        ok(&mut v, "component.create_instance", json!({"component": a, "x": 300 + (i % 16) * 70, "y": 100 + (i / 16) * 30}));
    }
    assert!(states(&v) > 256, "every binding state is kept: {}", states(&v));
    assert_eq!(main_of(&mut v, &first), json!(a));
    let listed = ok(&mut v, "component.list_instances", json!({"component": a}));
    assert_eq!(listed["instances"].as_array().unwrap().len(), 257, "{listed}");
}

/// F3 and F5: a layer delete drops the records keyed to it in the same undo step, so auto layout,
/// which validates the settled records, still runs after a definition and a referenced layer go.
#[test]
fn f5_deleted_definitions_and_referenced_layers_leave_no_records_behind() {
    let mut v = design_doc();
    let (a, _, icon) = button(&mut v, "A", 10.0);
    ok(&mut v, "component.add_property", json!({"component": a, "name": "Show icon", "type": "BOOLEAN", "default": true}));
    ok(&mut v, "component.set_property_reference", json!({"layer": icon, "field": "visible", "property": "Show icon"}));
    let frame = text(&ok(&mut v, "component.create_instance", json!({"component": a, "x": 200, "y": 10})), "layer_id");
    v.delete_layer(&icon).unwrap();
    let f = v.draw_shape(ShapeKind::Rectangle, [500.0, 10.0], [510.0, 20.0], false).unwrap();
    let g = v.group_layers(&[f], "Frame").unwrap();
    v.select(None);
    ok(&mut v, "design.set_auto_layout_mode", json!({"node": g, "mode": "HORIZONTAL"}));
    v.undo().unwrap();
    // The instance layer's record goes with it: no orphan record with layer_id null is listed.
    v.delete_layer(&frame).unwrap();
    let listed = v.state_json()["design"]["components"].clone();
    assert!(listed["instances"].as_array().unwrap().is_empty(), "{listed}");
    let definition = listed["components"][0]["layer_id"].as_str().unwrap().to_string();
    v.delete_layer(&definition).unwrap();
    ok(&mut v, "design.set_auto_layout_mode", json!({"node": g, "mode": "HORIZONTAL"}));
    v.undo().unwrap();
    v.undo().unwrap();
    assert_eq!(v.state_json()["design"]["components"]["components"][0]["component_id"], a.as_str(), "undo restores the record with the layer");
}

/// F1 ruling (b): states of layers that are gone are dropped when the document is opened (no history).
#[test]
fn f1_states_of_absent_layers_are_pruned_on_open_and_nothing_else_changes() {
    let scratch = Scratch::new("ds-f1-prune-open");
    let mut v = design_doc();
    let (a, ..) = button(&mut v, "A", 10.0);
    let keep = text(&ok(&mut v, "component.create_instance", json!({"component": a, "x": 200, "y": 10})), "layer_id");
    let gone = text(&ok(&mut v, "component.create_instance", json!({"component": a, "x": 300, "y": 10})), "layer_id");
    v.delete_layer(&gone).unwrap();
    assert_eq!(states(&v), 2);
    let path = scratch.0.join("prune.handshake");
    v.save(&path, false).unwrap();
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&path).unwrap();
    assert_eq!(states(&w), 1, "the absent layer's state is gone");
    assert_eq!(snapshot(&w), snapshot(&v), "layers and vector entry unchanged");
    assert_eq!(main_of(&mut w, &keep), json!(a));
}

/// F8: an instance copy carries the definition layer's live filter stack.
#[test]
fn f8_instance_copies_carry_live_filter_stacks() {
    let mut v = design_doc();
    let (a, bg, _) = button(&mut v, "A", 10.0);
    ok(&mut v, "vector.live_filter_apply", json!({"layer": bg, "filter_kind": "hsk.studio.vector_effect.twist", "params": [{"type": "Real", "key": "twist_angle", "value": 40.0}]}));
    let made = ok(&mut v, "component.create_instance", json!({"component": a, "x": 200, "y": 10}));
    let frame = text(&made, "layer_id");
    let copy = children(&v, &frame).into_iter().find(|k| layer(&v, k)["name"] == "Rectangle").unwrap();
    let read = ok(&mut v, "vector.live_filter_read", json!({"layer": copy}));
    assert_eq!(read["filters"].as_array().unwrap().len(), 1, "{read} {made}");
}

/// F10: duplicating an instance makes an instance of the same main with its overrides.
#[test]
fn f10_a_duplicated_instance_is_an_instance_of_the_same_main() {
    let mut v = design_doc();
    let (a, _, icon) = button(&mut v, "A", 10.0);
    let frame = text(&ok(&mut v, "component.create_instance", json!({"component": a, "x": 200, "y": 10})), "layer_id");
    ok(&mut v, "design.instance.set_override", json!({"layer": frame, "node_id": icon, "field": "visible", "value": false}));
    let before = snapshot(&v);
    let copy = v.duplicate_layer(&frame).unwrap();
    assert_eq!(main_of(&mut v, &copy), json!(a));
    let view = inspect(&mut v, &copy);
    assert_eq!(view["overrides"][0]["field"], "visible", "{view}");
    assert_ne!(view["instance_id"], inspect(&mut v, &frame)["instance_id"]);
    assert_eq!(ok(&mut v, "component.list_instances", json!({"component": a}))["instances"].as_array().unwrap().len(), 2);
    v.undo().unwrap();
    assert_eq!(snapshot(&v), before, "one undo step");
}

// Lead rulings R1 to R3 (2026-11-12) on BUNDLED GREEN 8a94c6af.

/// The text records the document's `text` entry holds: (layer id, story id).
fn text_records(v: &Viewer) -> Vec<(String, String)> {
    let entry: Value = serde_json::from_slice(v.run().document_entry("text").unwrap()).unwrap();
    entry["layers"].as_array().unwrap().iter().map(|r| (text(r, "layer_id"), text(r, "story_id"))).collect()
}

/// A label component (one text layer) and two of its instances: (component, label, first instance).
fn label_component(v: &mut Viewer) -> (String, String, String) {
    let label = v.place_text_with_font(10.0, 40.0, "OK", "Inter-Regular").unwrap();
    v.select(None);
    let a = text(&ok(v, "component.create_from_selection", json!({"layers": [label], "name": "Label"})), "component_id");
    let one = text(&ok(v, "component.create_instance", json!({"component": a, "x": 200, "y": 10})), "layer_id");
    ok(v, "component.create_instance", json!({"component": a, "x": 300, "y": 10}));
    (a, label, one)
}

/// R1: the text records of instance copies an undo can still return to are never compacted: a sync past
/// the text entry's cap is refused typed, and undo brings every copy back with its text record.
#[test]
fn r1_text_records_an_undo_can_reach_are_never_compacted() {
    let mut v = design_doc();
    let (a, _, one) = label_component(&mut v);
    let first: Vec<(String, String)> = children(&v, &one).into_iter().map(|k| {
        let story = layer(&v, &k)["payload"]["story_id"].as_str().unwrap().to_string();
        (k, story)
    }).collect();
    assert!(!first.is_empty());
    let mut done = 0;
    for _ in 0..140 {
        match call(&mut v, "component.sync_instances", json!({"component": a})) {
            Ok(_) => done += 1,
            Err((code, detail)) => {
                assert_eq!(code, "text_layer_over_cap", "{detail}");
                break;
            }
        }
    }
    for _ in 0..done {
        v.undo().unwrap();
    }
    assert_eq!(children(&v, &one), first.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>());
    let records = text_records(&v);
    for r in &first {
        assert!(records.contains(r), "the copy {} lost its text record after {done} syncs and their undo", r.0);
    }
}

/// F7 under R1: in a session the text entry's cap refuses typed; a save and reopen (no history) compacts
/// the records of copies no history can return to, and the redraws run again.
#[test]
fn r1_reopen_compacts_unreachable_copy_text_and_redraws_run_again() {
    let scratch = Scratch::new("ds-r1-reopen-text");
    let mut v = design_doc();
    let (a, label, one) = label_component(&mut v);
    let mut refused_at = None;
    for i in 0..140 {
        if let Err((code, detail)) = call(&mut v, "component.sync_instances", json!({"component": a})) {
            assert_eq!(code, "text_layer_over_cap", "{detail}");
            refused_at = Some(i);
            break;
        }
    }
    assert!(refused_at.is_some(), "a sync past the text entry's cap is refused in the session");
    let path = scratch.0.join("text.handshake");
    v.save(&path, false).unwrap();
    let mut w = Viewer::new(Prefer::Cpu);
    w.add_font("Inter-Regular", INTER.to_vec());
    w.open(&path).unwrap();
    assert!(text_records(&w).len() < text_records(&v).len(), "the reopen compacts the copies' records");
    ok(&mut w, "component.sync_instances", json!({"component": a}));
    ok(&mut w, "design.instance.set_override", json!({"layer": one, "node_id": label, "field": "characters", "value": "Go"}));
}

/// R2: a document reopened after more than 1024 instance commands does not refuse design_state_over_cap
/// (on open with no history, states nothing can bind again are pruned: superseded contents and instances
/// whose record is gone). An undone instance leaves a state of its layer id, which the next instance takes
/// again with a new instance id.
#[test]
fn r2_reopen_after_more_than_1024_instance_commands_keeps_working() {
    let scratch = Scratch::new("ds-r2-reopen-states");
    let mut v = design_doc();
    let (a, ..) = button(&mut v, "A", 10.0);
    let cycle = |v: &mut Viewer, n: usize| -> String {
        let mut frame = String::new();
        for i in 0..n {
            frame = text(&ok(v, "component.create_instance", json!({"component": a, "x": 200, "y": 10})), "layer_id");
            if i + 1 < n {
                v.undo().unwrap();
            }
        }
        frame
    };
    cycle(&mut v, 520);
    assert!(states(&v) > 512, "every instance kept its state: {}", states(&v));
    let path = scratch.0.join("states.handshake");
    v.save(&path, false).unwrap();
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&path).unwrap();
    let last = cycle(&mut w, 520);
    assert_eq!(main_of(&mut w, &last), json!(a));
}

/// R3 (F10): an instance of the design file's records is not duplicated as a silent plain group.
#[test]
fn r3_a_design_file_instance_duplicate_is_refused_typed() {
    let scratch = Scratch::new("ds-r3-fig-duplicate");
    let fig = scratch.0.join("components.fig");
    std::fs::copy(format!("{}/../hsk-studio-interop-fig/tests/fixtures/fig-components-autolayout.fig", env!("CARGO_MANIFEST_DIR")), &fig).unwrap();
    let dest = scratch.0.join("components.handshake");
    let mut v = design_doc();
    let report = ok(&mut v, "design.import_fig", json!({"source": fig.to_str().unwrap(), "dest": dest.to_str().unwrap()}));
    let layer = report["instance_layers"][0].as_str().unwrap_or_else(|| panic!("{report}")).to_string();
    let before = snapshot(&v);
    let e = v.duplicate_layer(&layer).unwrap_err();
    assert!(e.contains("duplicate_instance_unsupported"), "{e}");
    assert_eq!(snapshot(&v), before, "nothing is copied");
}
