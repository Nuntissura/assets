//! Lane SMB-PS-ACTIONS-1 slice 1: action records ([STU-AUT-102]..[STU-AUT-107]), the strict portable
//! artifact ([STU-AUT-114]), the validators ([STU-AUT-181]..[STU-AUT-185]) and the command corpus
//! ([STU-AUT-147] subset).

use hsk_studio_harness_core::action::{
    self, ActionRecord, ActionSet, ActionStep, DialogOption, ImportError, RUN_COMMANDS, RecordedValue, Severity, UnitToken, ValueType,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const L1: &str = "SLYR-019abcde-0000-7000-8000-000000000001";
const L2: &str = "SLYR-019abcde-0000-7000-8000-000000000002";

fn act_id(n: u64) -> String {
    format!("SACT-019abcde-0000-7000-8000-{n:012x}")
}

fn step(index: u64, command: &str, params: Vec<(&str, RecordedValue)>) -> ActionStep {
    ActionStep {
        index,
        enabled: true,
        expanded: false,
        with_dialog: false,
        dialog_options: DialogOption::Silent,
        command_id: command.to_string(),
        display_name: command.to_string(),
        has_parameters: !params.is_empty(),
        parameter_class: None,
        parameters: Some(params.into_iter().map(|(k, v)| (k.to_string(), v)).collect::<BTreeMap<_, _>>()),
    }
}

fn action(n: u64, steps: Vec<ActionStep>) -> ActionRecord {
    ActionRecord {
        action_id: act_id(n),
        index: n,
        name: format!("action {n}"),
        function_key_index: 0,
        shift_modifier: false,
        command_modifier: false,
        color_index: 0,
        expanded: false,
        declared_step_count: steps.len() as u64,
        steps,
    }
}

fn rename(index: u64) -> ActionStep {
    step(index, "layer.rename", vec![("layer", RecordedValue::Reference(L1.into())), ("name", RecordedValue::String("renamed".into()))])
}

fn set(actions: Vec<ActionRecord>) -> ActionSet {
    ActionSet { action_set_id: "set-default".into(), name: "Default Actions".into(), actions }
}

fn codes(d: &[action::Diagnostic]) -> Vec<&'static str> {
    d.iter().map(|d| d.code).collect()
}

fn find<'a>(d: &'a [action::Diagnostic], code: &str) -> &'a action::Diagnostic {
    d.iter().find(|x| x.code == code).unwrap_or_else(|| panic!("no {code} in {:?}", codes(d)))
}

#[test]
fn every_validator_fires_with_its_code_and_named_fields() {
    // 181: declared 3, one step.
    let mut miscount = action(1, vec![rename(0)]);
    miscount.declared_step_count = 3;
    // 182: two actions on F5 + shift.
    let mut f5a = action(2, vec![rename(0)]);
    let mut f5b = action(3, vec![rename(0)]);
    for a in [&mut f5a, &mut f5b] {
        a.function_key_index = 5;
        a.shift_modifier = true;
    }
    // 183: unknown command; 184: unknown key; 185: measured without unit (nested in a list too).
    let mut unknown_key = rename(1);
    unknown_key.parameters.as_mut().unwrap().insert("colour".into(), RecordedValue::Number(3.0));
    let mixed = action(
        4,
        vec![
            step(0, "layer.does_not_exist", vec![]),
            unknown_key,
            step(2, "layer.set_opacity", vec![("layer", RecordedValue::Reference(L2.into())), ("opacity", RecordedValue::Measured { value: 40.0, unit: None })]),
        ],
    );
    let all = set(vec![miscount.clone(), f5a.clone(), f5b.clone(), mixed.clone()]);
    let d = action::validate_set(&all);

    let c = find(&d, "action_declared_step_count_mismatch");
    assert!(c.severity == Severity::Error && c.clause == "STU-AUT-181");
    assert_eq!(c.fields, json!({"action_id": act_id(1), "declared_step_count": 3, "steps_length": 1}));
    let c = find(&d, "action_shortcut_collision");
    assert!(c.severity == Severity::Error && c.clause == "STU-AUT-182");
    assert_eq!(c.fields["actions"], json!([{"action_set_id": "set-default", "action_id": act_id(2)}, {"action_set_id": "set-default", "action_id": act_id(3)}]));
    assert_eq!(c.fields["binding"], json!({"function_key_index": 5, "shift_modifier": true, "command_modifier": false}));
    let c = find(&d, "action_step_command_unresolvable");
    assert!(c.severity == Severity::Error && c.clause == "STU-AUT-183");
    assert_eq!(c.fields, json!({"action_id": act_id(4), "step_index": 0, "command_id": "layer.does_not_exist"}));
    let c = find(&d, "action_step_parameter_key_unknown");
    assert!(c.severity == Severity::Warning && c.clause == "STU-AUT-184");
    assert_eq!(c.fields, json!({"action_id": act_id(4), "step_index": 1, "key": "colour", "command_id": "layer.rename"}));
    let c = find(&d, "action_measured_value_missing_unit");
    assert!(c.severity == Severity::Error && c.clause == "STU-AUT-185");
    assert_eq!(c.fields, json!({"action_id": act_id(4), "step_index": 2, "key": "opacity"}));

    // Different modifiers are no collision; a collision across two enabled sets names both sets.
    let mut f5c = f5b.clone();
    f5c.command_modifier = true;
    assert!(action::shortcut_collisions(&[&set(vec![f5a.clone(), f5c])]).is_empty());
    let other = ActionSet { action_set_id: "set-other".into(), name: "Other".into(), actions: vec![f5b] };
    let across = action::shortcut_collisions(&[&set(vec![f5a]), &other]);
    assert_eq!(across.len(), 1);
    assert_eq!(across[0].fields["actions"][1]["action_set_id"], "set-other");

    // 181 fails the load; 185 fails the load; 183/184 import with the step kept and reported.
    assert_eq!(ActionSet::import(&set(vec![miscount]).export()).unwrap_err().code(), "action_declared_step_count_mismatch");
    let unit_only = action(5, vec![step(0, "layer.set_opacity", vec![("layer", RecordedValue::Reference(L2.into())), ("opacity", RecordedValue::List(vec![RecordedValue::Measured { value: 1.0, unit: None }]))])]);
    match ActionSet::import(&set(vec![unit_only]).export()) {
        Err(ImportError::Rejected(d)) => assert_eq!(find(&d, "action_measured_value_missing_unit").fields["key"], "opacity[0]"),
        other => panic!("{other:?}"),
    }
}

/// A set using every recorded value type, every unit token and every dialog option.
fn rich_set() -> ActionSet {
    let units: Vec<RecordedValue> = UnitToken::ALL.iter().map(|u| RecordedValue::Measured { value: 115.046, unit: Some(*u) }).collect();
    let everything = step(
        0,
        "layer.panel_set",
        vec![
            ("layer", RecordedValue::Reference(L1.into())),
            ("control", RecordedValue::String("layer.name".into())),
            ("value", RecordedValue::String("Ünïcode \"quoted\" \\ name".into())),
            ("class", RecordedValue::Class("rgb_colour".into())),
            ("mode", RecordedValue::Enumerated("blend_mode.normal".into())),
            ("count", RecordedValue::Number(3.0)),
            ("fraction", RecordedValue::Number(-0.125)),
            ("anti_alias", RecordedValue::Boolean(true)),
            ("units", RecordedValue::List(units)),
            ("nested", RecordedValue::List(vec![RecordedValue::List(vec![RecordedValue::Reference(L2.into()), RecordedValue::Boolean(false)])])),
        ],
    );
    let mut steps = vec![everything];
    for (i, d) in DialogOption::ALL.into_iter().enumerate() {
        let mut s = rename(i as u64 + 1);
        s.dialog_options = d;
        s.with_dialog = i % 2 == 1;
        s.expanded = true;
        s.parameter_class = Some("rename_parameters".into());
        steps.push(s);
    }
    let mut empty = step(5, "layer.set_visible", vec![]);
    empty.parameters = None;
    empty.has_parameters = false;
    empty.enabled = false;
    steps.push(empty);
    let mut a = action(7, steps);
    a.function_key_index = 12;
    a.color_index = 7;
    a.command_modifier = true;
    a.expanded = true;
    set(vec![a, action(8, vec![])])
}

#[test]
fn export_then_import_is_byte_identical() {
    assert_eq!(ValueType::ALL.len(), 8, "STU-AUT-106 table rows (heading says seven: spec-delta request)");
    let original = rich_set();
    let bytes = original.export();
    let imported = ActionSet::import(&bytes).expect("imports");
    assert_eq!(imported.set, original);
    assert_eq!(imported.set.export(), bytes, "export -> import -> export is byte-identical");
    // The unknown keys of the panel step ("class", "mode", ...) are warnings, nothing else.
    assert!(imported.diagnostics.iter().all(|d| d.code == "action_step_parameter_key_unknown"), "{:?}", codes(&imported.diagnostics));
    assert!(imported.unresolvable.is_empty());
    // Every value type and unit token appears on the wire by its literal token.
    let text = String::from_utf8(bytes).unwrap();
    for t in ValueType::ALL {
        assert!(text.contains(&format!("\"type\":\"{}\"", t.token())), "{}", t.token());
    }
    for u in UnitToken::ALL {
        assert!(text.contains(&format!("\"unit\":\"{}\"", u.token())), "{}", u.token());
    }
}

#[test]
fn unknown_command_survives_import_and_reexport_and_is_reported() {
    let foreign = step(1, "photoshop.unsharp_mask", vec![("amount", RecordedValue::Measured { value: 120.0, unit: Some(UnitToken::Percent) })]);
    let original = set(vec![action(9, vec![rename(0), foreign.clone()])]);
    let bytes = original.export();
    let imported = ActionSet::import(&bytes).expect("an unresolvable step does not fail the load");
    assert_eq!(imported.unresolvable, vec![(act_id(9), 1, "photoshop.unsharp_mask".to_string())]);
    assert_eq!(imported.set.actions[0].steps[1], foreign, "kept, not dropped");
    assert!(imported.set.actions[0].steps[1].command().is_none());
    assert_eq!(codes(&imported.diagnostics), ["action_step_command_unresolvable"]);
    assert_eq!(imported.set.export(), bytes, "re-export is unchanged");
}

fn decode_err(v: &Value) -> (String, String) {
    let e = ActionSet::decode(&serde_json::to_vec(v).unwrap()).unwrap_err();
    (e.code.to_string(), e.path)
}

#[test]
fn domain_edges_and_strictness_are_refused() {
    let base = rich_set().to_json();
    let with = |f: &dyn Fn(&mut Value)| {
        let mut v = base.clone();
        f(&mut v);
        v
    };
    // Accepted edges: F12, colour 7, dialog 3 (in rich_set).
    assert!(ActionSet::decode(&rich_set().export()).is_ok());
    assert_eq!(decode_err(&with(&|v| v["actions"][0]["function_key_index"] = json!(13))).0, "function_key_index_out_of_domain");
    assert_eq!(decode_err(&with(&|v| v["actions"][0]["color_index"] = json!(8))).0, "color_index_out_of_domain");
    let (code, path) = decode_err(&with(&|v| v["actions"][0]["steps"][1]["dialog_options"] = json!(4)));
    assert_eq!((code.as_str(), path.as_str()), ("dialog_options_out_of_domain", "/actions/0/steps/1/dialog_options"));
    assert_eq!(decode_err(&with(&|v| v["actions"][0]["steps"][0]["parameters"]["units"]["value"][0]["unit"] = json!("inches"))).0, "unit_token_unknown");
    assert_eq!(decode_err(&with(&|v| v["actions"][0]["steps"][0]["parameters"]["count"]["type"] = json!("integer"))).0, "value_type_unknown");
    assert_eq!(decode_err(&with(&|v| v["actions"][0]["steps"][0]["parameters"]["mode"]["value"] = json!("normal"))).0, "enumerated_token_malformed");
    assert_eq!(decode_err(&with(&|v| v["actions"][0]["steps"][0]["parameters"]["layer"]["value"] = json!("layer-1"))).0, "reference_malformed");
    assert_eq!(decode_err(&with(&|v| v["actions"][0]["action_id"] = json!("SACT-not-a-uuid"))).0, "action_id_malformed");
    assert_eq!(decode_err(&with(&|v| v["actions"][0]["steps"][0]["parameters"]["count"]["unit"] = json!("percent"))).0, "field_unknown");
    assert_eq!(decode_err(&with(&|v| v["actions"][0]["extra"] = json!(1))).0, "field_unknown");
    assert_eq!(decode_err(&with(&|v| { v["actions"][0].as_object_mut().unwrap().remove("expanded"); })).0, "field_missing");
    assert_eq!(decode_err(&with(&|v| { v["actions"][0]["steps"][0].as_object_mut().unwrap().remove("parameter_class"); })).0, "field_missing");
    assert_eq!(decode_err(&with(&|v| v["actions"][0]["index"] = json!(-1))).0, "field_type");
    // Duplicate keys (a JSON object keeps only the last one, so they are found on the bytes),
    // also when the duplicate is spelled with an escape.
    let text = String::from_utf8(rich_set().export()).unwrap();
    for dup in ["\"name\":\"Default Actions\",\"name\":\"x\"", "\"name\":\"Default Actions\",\"n\\u0061me\":\"x\""] {
        let bytes = text.replacen("\"name\":\"Default Actions\"", dup, 1);
        assert_eq!(ActionSet::decode(bytes.as_bytes()).unwrap_err().code, "field_duplicate", "{dup}");
    }
    // A duplicate-looking key inside a string value is not a key.
    let decoy = text.replacen("\"Default Actions\"", "\"{\\\"name\\\":1,\\\"name\\\":2}\"", 1);
    assert!(ActionSet::decode(decoy.as_bytes()).is_ok());
    assert_eq!(ActionSet::decode(b"{").unwrap_err().code, "artifact_not_json");
}

#[test]
fn budgets_are_checked_before_building() {
    // Byte budget first: nothing is parsed.
    let big = vec![b' '; action::MAX_ARTIFACT_BYTES + 1];
    assert_eq!(ActionSet::decode(&big).unwrap_err().code, "artifact_too_large");
    // Aggregate step budget across actions of one set.
    let per = action::MAX_STEPS_PER_SET / 5 + 1;
    let steps: Vec<Value> = (0..per).map(|i| step(i as u64, "layer.set_visible", vec![]).to_json()).collect();
    let mut actions = Vec::new();
    for n in 0..5u64 {
        let mut a = action(n, vec![]).to_json();
        a["steps"] = json!(steps);
        a["declared_step_count"] = json!(per);
        actions.push(a);
    }
    let v = json!({"action_set_id": "big", "name": "big", "actions": actions});
    assert_eq!(decode_err(&v).0, "steps_over_budget");
    // List nesting depth.
    let mut deep = json!({"type": "boolean", "value": true});
    for _ in 0..=action::MAX_LIST_DEPTH {
        deep = json!({"type": "list", "value": [deep]});
    }
    let mut a = action(1, vec![rename(0)]).to_json();
    a["steps"][0]["parameters"]["name"] = deep;
    assert_eq!(decode_err(&json!({"action_set_id": "d", "name": "d", "actions": [a]})).0, "list_too_deep");
}

#[test]
fn corpus_lists_every_run_operation_with_recordability() {
    let ids: Vec<&str> = RUN_COMMANDS.iter().map(|c| c.command_id).collect();
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "command ids are unique");
    let recordable: Vec<&str> = RUN_COMMANDS.iter().filter(|c| c.recordable).map(|c| c.command_id).collect();
    assert_eq!(recordable, ["layer.rename", "layer.set_visible", "layer.set_opacity", "layer.set_blend_mode", "layer.panel_set", "layer.restructure"]);
    for id in ["document.open", "document.import", "document.reopen", "document.save", "document.checkpoint", "document.verify", "document.summary", "layer.panel", "history.undo", "history.redo", "layer.set_tiles", "layer.set_tiles_and_entry", "layer.set_vector_paths", "layer.set_vector_paths_and_entry", "layer.restructure_and_entry", "document.set_document_entry", "document.submit_sequence"] {
        let c = action::command(id).unwrap_or_else(|| panic!("{id} missing"));
        assert!(!c.recordable, "{id}");
    }
    let opacity = action::command("layer.set_opacity").unwrap();
    assert!(opacity.parameters.iter().any(|p| p.key == "opacity" && p.value_type == ValueType::Measured && p.unit == Some(UnitToken::Percent)));
    assert_eq!(action::corpus_json().as_array().unwrap().len(), RUN_COMMANDS.len());
    println!("NOT_PROVEN: schemars input_schema/output_schema per corpus command (STU-AUT-147; schemars is not an approved harness-core dependency)");
    println!("NOT_PROVEN: SACT identity prefix in hsk-studio-accord (action_id is checked as text)");
    println!("NOT_PROVEN: dry_runnable commands (Run has no dry-run mode; every corpus entry declares dry_runnable false)");
    println!("NOT_PROVEN: STU-AUT-111 step kinds (message_stop, path_literal, tool_stroke, conditional_branch, conditional_mode_change)");
    println!("NOT_PROVEN: event-bound actions (STU-AUT-112) and conditional predicates (STU-AUT-113)");
    println!("NOT_PROVEN: Folio persistence of action sets as authority records and CKC registration (STU-AUT-114)");
    println!("NOT_PROVEN: GUI actions panel (record / play / step editor)");
    println!("NOT_PROVEN: STU-AUT-110 explicit insertion of a recordable:false step (a non-recordable command step refuses action_step_not_playable)");
}
