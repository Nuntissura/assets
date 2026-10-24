//! Lane SMB-FIG-EDIT-2: the design state record set (`hsk.studio.design_state@1`, STU-IO-006 row 35
//! edit-preserve, STU-DS-003/008/016/017) round-trips through wire JSON and is validated against the
//! document and, when given, the design records: the instance layer, the (layer, content) key, the
//! member component, its variant assignment, property types, override nodes and fields and the
//! swap target are typed refusals; absent records leave documents byte-identical.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use serde_json::{Value, json};

fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn layer(n: u32, kind: &str, parent: Value) -> Value {
    let payload = match kind {
        "group" => json!({"payload_kind": "group"}),
        _ => json!({"payload_kind": "raster", "tiles": []}),
    };
    json!({"schema_id": "hsk.studio.layer@1", "layer_id": identity("SLYR", n),
        "name": format!("layer {n}"), "parent": parent, "kind": kind, "payload": payload, "visible": true})
}
fn under(n: u32) -> Value {
    json!({"node_kind": "layer", "node_id": identity("SLYR", n)})
}
fn pt(v: f64) -> Value {
    json!({"value": v, "unit": "pt"})
}
fn publishing(key: &str) -> Value {
    json!({"description": "", "description_markdown": "", "documentation_links": [], "publish_key": key, "is_remote": false})
}
/// Set container 10 with members 11 (child 13) and 12 (child 14); plain component 30 (child 31);
/// instance layer 20 drawing children 21 and 22; raster 40 at the root.
fn document() -> StudioDocument {
    let board = json!({"node_kind": "artboard", "node_id": identity("SART", 1)});
    serde_json::from_value(json!({"schema_id": "hsk.studio.document@1", "document_id": identity("SDOC", 1), "revision": 1,
        "geometry_unit": "pt", "colour_mode": 2, "bits_per_channel": 8,
        "working_space_rgb": null, "working_space_cmyk": null, "working_space_gray": null, "working_space_spot": null, "blending_space": null,
        "artboards": [{"schema_id": "hsk.studio.artboard@1", "artboard_id": identity("SART", 1), "name": "screen", "parent": null,
            "bounds": {"x": pt(0.0), "y": pt(0.0), "width": pt(800.0), "height": pt(600.0)}}],
        "page_spreads": [],
        "layers": [
            layer(10, "group", board.clone()), layer(11, "group", under(10)), layer(12, "group", under(10)),
            layer(13, "raster", under(11)), layer(14, "raster", under(12)),
            layer(30, "group", board.clone()), layer(31, "raster", under(30)),
            layer(20, "group", board.clone()), layer(21, "raster", under(20)), layer(22, "raster", under(20)),
            layer(40, "raster", board)],
        "graph": {"schema_id": "hsk.studio.layer_graph@1", "operations": [], "edges": [], "outputs": []}}))
    .unwrap()
}
fn component(n: u32, layer_n: u32, variant: Value, defs: Value) -> Value {
    json!({"schema_id": "hsk.studio.component@1", "component_id": identity("SCMP", n), "layer_id": identity("SLYR", layer_n),
        "publishing": publishing(&format!("k{n}")), "variant_properties": variant, "component_property_definitions": defs})
}
fn design() -> StudioDesignRecords {
    serde_json::from_value(json!({"schema_id": "hsk.studio.design_records@1", "document_id": identity("SDOC", 1),
        "components": [
            component(2, 11, json!({"size": "small"}), json!([{"property_name": "label", "type": "TEXT", "default_value": "OK"}])),
            component(3, 12, json!({"size": "large"}), json!([])),
            component(4, 30, Value::Null, json!([{"property_name": "flag", "type": "BOOLEAN", "default_value": false}]))],
        "component_sets": [{"schema_id": "hsk.studio.component@1", "component_id": identity("SCMP", 1), "layer_id": identity("SLYR", 10),
            "publishing": publishing("set"), "default_variant_id": identity("SCMP", 2),
            "variant_group_properties": {"size": ["small", "large"]},
            "component_property_definitions": [{"property_name": "showIcon", "type": "BOOLEAN", "default_value": false}]}],
        "component_instances": [{"schema_id": "hsk.studio.component_instance@1", "instance_id": identity("SCIN", 1),
            "main_component_id": identity("SCMP", 2), "component_properties": {}, "overrides": [],
            "scale_factor": 1.0, "is_exposed_instance": false}]}))
    .unwrap()
}
fn state() -> StudioInstanceState {
    serde_json::from_value(json!({"layer_id": identity("SLYR", 20), "instance_id": identity("SCIN", 1),
        "component_id": identity("SCMP", 2), "variant": {"size": "small"},
        "properties": {"showIcon": {"type": "BOOLEAN", "value": true}, "label": {"type": "TEXT", "value": "Go"}},
        "overrides": [
            {"node_id": identity("SLYR", 13), "field": "characters", "value": {"type": "STRING", "value": "Changed"}},
            {"node_id": identity("SLYR", 13), "field": "visible", "value": {"type": "BOOLEAN", "value": false}},
            {"node_id": identity("SLYR", 13), "field": "opacity", "value": {"type": "FLOAT", "value": 0.5}}],
        "content_signature": [identity("SLYR", 21), identity("SLYR", 22)]}))
    .unwrap()
}
fn records() -> StudioDesignStateRecords {
    let mut r = StudioDesignStateRecords::new(identity("SDOC", 1));
    r.instances = vec![state()];
    r
}
fn check(r: &StudioDesignStateRecords, with_design: bool) -> Result<(), (Code, String)> {
    let d = design();
    validate_design_state_records(r, &document(), with_design.then_some(&d), Budget::default(), &CancellationToken::default()).map_err(|e| (e.code, e.target))
}

#[test]
fn design_state_validates_against_document_and_design_records() {
    let token = CancellationToken::default();
    let (doc, des) = (document(), design());
    let r = records();
    assert_eq!(check(&r, true), Ok(()));
    assert_eq!(check(&r, false), Ok(()));
    // Wire round trip, unknown fields and duplicate keys refused, the input bound before the parse.
    let bytes = serde_json::to_vec(&r).unwrap();
    assert_eq!(parse_design_state_records(&bytes, &doc, Some(&des), Budget::default(), &token).unwrap(), r);
    let wire: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(wire["schema_id"], "hsk.studio.design_state@1");
    let mut extra = wire.clone();
    extra["instances"][0]["scale"] = json!(2.0);
    assert_eq!(parse_design_state_records(&serde_json::to_vec(&extra).unwrap(), &doc, None, Budget::default(), &token).unwrap_err().code, Code::InvalidJson);
    let doc_id = identity("SDOC", 1);
    let dup = format!(r#"{{"schema_id":"hsk.studio.design_state@1","document_id":"{doc_id}","document_id":"{doc_id}"}}"#);
    assert_eq!(parse_design_state_records(dup.as_bytes(), &doc, None, Budget::default(), &token).unwrap_err().code, Code::DuplicateField);
    let tight = Budget { input_bytes: 16, ..Budget::default() };
    assert_eq!(parse_design_state_records(&bytes, &doc, None, tight, &token).unwrap_err().code, Code::BudgetExceeded);
    let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
    assert_eq!(parse_design_state_records(deep.as_bytes(), &doc, None, Budget::default(), &token).unwrap_err().code, Code::BudgetExceeded);

    // A second state of the same layer for other content (undo history) is admitted; the lookup
    // returns the state recorded for exactly that content.
    let mut r2 = records();
    let mut older = state();
    older.content_signature = vec![identity("SLYR", 51), identity("SLYR", 52)];
    older.overrides.clear();
    r2.instances.push(older.clone());
    assert_eq!(check(&r2, true), Ok(()));
    assert_eq!(r2.state(&identity("SLYR", 20), &[identity("SLYR", 51), identity("SLYR", 52)]), Some(&older));
    assert_eq!(r2.state(&identity("SLYR", 20), &[identity("SLYR", 52), identity("SLYR", 51)]), None);
    // A swap to the plain component with its own property; a variant switch inside a swapped-to set.
    let mut swapped = records();
    let s = &mut swapped.instances[0];
    s.component_id = identity("SCMP", 4);
    s.variant.clear();
    s.properties = serde_json::from_value(json!({"flag": {"type": "BOOLEAN", "value": true}})).unwrap();
    s.overrides.clear();
    s.overrides.push(serde_json::from_value(json!({"node_id": identity("SLYR", 31), "field": "visible", "value": {"type": "BOOLEAN", "value": true}})).unwrap());
    s.swap_target = Some(identity("SCMP", 4));
    assert_eq!(check(&swapped, true), Ok(()));
    let mut in_set = records();
    in_set.instances[0].swap_target = Some(identity("SCMP", 3));
    assert_eq!(check(&in_set, true), Ok(()), "a later variant switch stays inside the swapped-to set");
    in_set.instances[0].swap_target = Some(identity("SCMP", 1));
    assert_eq!(check(&in_set, true), Ok(()), "the set itself");

    // Document refusals (with and without design records).
    let refuse = |f: &dyn Fn(&mut StudioDesignStateRecords), with_design: bool, code: Code, target: &str| {
        let mut r = records();
        f(&mut r);
        assert_eq!(check(&r, with_design), Err((code, target.to_string())), "{target}");
    };
    for with_design in [false, true] {
        refuse(&|r| r.document_id = identity("SDOC", 2), with_design, Code::InvalidField, "document_id");
        refuse(&|r| r.instances[0].layer_id = identity("SLYR", 99), with_design, Code::DanglingReference, "instances/0/layer_id");
        refuse(&|r| r.instances[0].layer_id = identity("SLYR", 40), with_design, Code::InvalidField, "instances/0/layer_id");
        refuse(&|r| r.instances[0].layer_id = identity("SDOC", 20), with_design, Code::InvalidId, "instances/0/layer_id");
        refuse(&|r| r.instances[0].instance_id = identity("SCMP", 1), with_design, Code::InvalidId, "instances/0/instance_id");
        refuse(&|r| r.instances[0].content_signature = vec!["x".into()], with_design, Code::InvalidId, "instances/0/content_signature");
        refuse(&|r| r.instances[0].content_signature = vec![identity("SLYR", 21), identity("SLYR", 21)], with_design, Code::InvalidField, "instances/0/content_signature");
        refuse(&|r| r.instances[0].content_signature = vec![identity("SLYR", 20)], with_design, Code::InvalidField, "instances/0/content_signature");
        refuse(&|r| r.instances.push(state()), with_design, Code::DuplicateIdentity, "instances/1/content_signature");
        refuse(
            &|r| {
                let mut other = state();
                other.content_signature.clear();
                other.instance_id = identity("SCIN", 2);
                r.instances.push(other);
            },
            with_design,
            Code::InvalidField,
            "instances/1/instance_id",
        );
        refuse(
            &|r| {
                let mut other = state();
                other.layer_id = identity("SLYR", 30);
                other.content_signature = vec![identity("SLYR", 31)];
                r.instances.push(other);
            },
            with_design,
            Code::DuplicateIdentity,
            "instances/1/instance_id",
        );
        refuse(&|r| r.instances[0].overrides[0].field = "font_weight".into(), with_design, Code::InvalidField, "instances/0/overrides/0/field");
        refuse(&|r| r.instances[0].overrides[1].field = "characters".into(), with_design, Code::DuplicateIdentity, "instances/0/overrides/1/field");
        refuse(&|r| r.instances[0].overrides[0].value = StudioValue::Boolean { value: true }, with_design, Code::InvalidField, "instances/0/overrides/0/value");
        refuse(&|r| r.instances[0].overrides[2].value = StudioValue::Float { value: 1.5 }, with_design, Code::InvalidField, "instances/0/overrides/2/value");
        refuse(&|r| r.instances[0].overrides[2].value = StudioValue::Float { value: f64::NAN }, with_design, Code::InvalidField, "instances/0/overrides/2/value");
        refuse(&|r| r.instances[0].overrides[0].node_id = identity("SLYR", 98), with_design, Code::DanglingReference, "instances/0/overrides/0/node_id");
        refuse(
            &|r| {
                r.instances[0].variant.insert("bad\u{7}".into(), "x".into());
            },
            with_design,
            Code::InvalidField,
            "instances/0/variant",
        );
    }
    // A variable alias of the right type is admitted for a drawn field.
    let mut aliased = records();
    aliased.instances[0].overrides[1].value = StudioValue::VariableAlias { id: identity("SVAR", 1), resolved_type: VariableType::Boolean };
    assert_eq!(check(&aliased, true), Ok(()));
    aliased.instances[0].overrides[1].value = StudioValue::VariableAlias { id: identity("SVAR", 1), resolved_type: VariableType::String };
    assert_eq!(check(&aliased, true), Err((Code::InvalidField, "instances/0/overrides/1/value".into())));

    // Ceilings and the node budget before the walk; cancellation.
    let mut big = records();
    big.instances = (0..=MAX_DESIGN_STATES)
        .map(|i| {
            let mut s = state();
            s.content_signature = vec![identity("SLYR", 1000 + u32::try_from(i).unwrap())];
            s
        })
        .collect();
    assert_eq!(check(&big, false), Err((Code::BudgetExceeded, "instances".into())));
    let mut wide = records();
    wide.instances[0].content_signature = (0..=MAX_STATE_SIGNATURE_LAYERS).map(|i| identity("SLYR", 5000 + u32::try_from(i).unwrap())).collect();
    assert_eq!(check(&wide, false), Err((Code::BudgetExceeded, "instances/0/content_signature".into())));
    let small = Budget { nodes: 2, ..Budget::default() };
    assert_eq!(validate_design_state_records(&records(), &doc, None, small, &token).unwrap_err().code, Code::BudgetExceeded);
    let canceled = CancellationToken::default();
    canceled.cancel();
    assert_eq!(validate_design_state_records(&records(), &doc, Some(&des), Budget::default(), &canceled).unwrap_err().code, Code::Canceled);
}

#[test]
fn unknown_component_or_variant_is_refused() {
    let refuse = |f: &dyn Fn(&mut StudioDesignStateRecords), code: Code, target: &str| {
        let mut r = records();
        f(&mut r);
        assert_eq!(check(&r, false), Ok(()), "{target}: the document alone cannot judge it");
        assert_eq!(check(&r, true), Err((code, target.to_string())), "{target}");
    };
    refuse(&|r| r.instances[0].instance_id = identity("SCIN", 9), Code::DanglingReference, "instances/0/instance_id");
    refuse(&|r| r.instances[0].component_id = identity("SCMP", 9), Code::DanglingReference, "instances/0/component_id");
    refuse(&|r| r.instances[0].component_id = identity("SCMP", 1), Code::DanglingReference, "instances/0/component_id");
    refuse(&|r| r.instances[0].variant = [("colour".to_string(), "red".to_string())].into(), Code::DanglingReference, "instances/0/variant/colour");
    refuse(&|r| r.instances[0].variant = [("size".to_string(), "huge".to_string())].into(), Code::DanglingReference, "instances/0/variant/size");
    refuse(&|r| r.instances[0].variant = [("size".to_string(), "large".to_string())].into(), Code::InvalidField, "instances/0/variant");
    refuse(&|r| r.instances[0].variant.clear(), Code::InvalidField, "instances/0/variant");
    refuse(
        &|r| {
            r.instances[0].properties.insert("nope".into(), serde_json::from_value(json!({"type": "BOOLEAN", "value": true})).unwrap());
        },
        Code::DanglingReference,
        "instances/0/properties/nope",
    );
    refuse(
        &|r| {
            r.instances[0].properties.insert("showIcon".into(), serde_json::from_value(json!({"type": "TEXT", "value": "x"})).unwrap());
        },
        Code::InvalidField,
        "instances/0/properties/showIcon",
    );
    // An override node outside the member's definition (the other member's layer, the instance's own child).
    refuse(&|r| r.instances[0].overrides[0].node_id = identity("SLYR", 14), Code::DanglingReference, "instances/0/overrides/0/node_id");
    refuse(&|r| r.instances[0].overrides[0].node_id = identity("SLYR", 21), Code::DanglingReference, "instances/0/overrides/0/node_id");
    // The plain component has no set: a variant assignment names nothing.
    refuse(
        &|r| {
            let s = &mut r.instances[0];
            s.component_id = identity("SCMP", 4);
            s.properties.clear();
            s.overrides.clear();
        },
        Code::DanglingReference,
        "instances/0/variant/size",
    );
    refuse(&|r| r.instances[0].swap_target = Some(identity("SCMP", 9)), Code::DanglingReference, "instances/0/swap_target");
    refuse(&|r| r.instances[0].swap_target = Some(identity("SCMP", 4)), Code::InvalidField, "instances/0/swap_target");
    // Property values of the wrong shape are refused by the document check already.
    let mut r = records();
    r.instances[0].properties.insert("showIcon".into(), serde_json::from_value(json!({"type": "BOOLEAN", "value": "yes"})).unwrap());
    assert_eq!(check(&r, false), Err((Code::InvalidField, "instances/0/properties/showIcon".into())));
}

#[test]
fn absent_design_state_keeps_documents_byte_identical() {
    let d = document();
    let des = design();
    let before = serde_json::to_vec(&d).unwrap();
    let design_before = serde_json::to_vec(&des).unwrap();
    assert_eq!(check(&records(), true), Ok(()));
    // The record set lives beside the document and the design records: both wires are untouched.
    assert_eq!(serde_json::to_vec(&d).unwrap(), before);
    assert_eq!(serde_json::to_vec(&des).unwrap(), design_before);
    let reread: StudioDocument = serde_json::from_slice(&before).unwrap();
    assert_eq!(serde_json::to_vec(&reread).unwrap(), before);
    let doc_schema = serde_json::to_string(&document_schema()).unwrap();
    let design_schema = serde_json::to_string(&design_records_schema()).unwrap();
    for s in [&doc_schema, &design_schema] {
        assert!(!s.contains("design_state") && !s.contains("content_signature") && !s.contains("swap_target"));
    }
    // An empty set is the schema token and the document id only.
    let empty = StudioDesignStateRecords::new(identity("SDOC", 1));
    assert_eq!(serde_json::to_string(&empty).unwrap(), format!(r#"{{"schema_id":"hsk.studio.design_state@1","document_id":"{}"}}"#, identity("SDOC", 1)));
    assert_eq!(check(&empty, true), Ok(()));
    let schema = serde_json::to_string(&design_state_records_schema()).unwrap();
    assert!(schema.contains("hsk.studio.design_state@1") && schema.contains("content_signature") && schema.contains("swap_target"));
}
