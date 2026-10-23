//! Lane SMB-AI-TYPE-PATH-RECOLOR slice 1: text-on-path bindings in `StudioVectorRecords`
//! (STU-TYP-120/121). Absent member encodes byte-identically; a full binding round-trips; hostile
//! bindings are refused with the code of the rule named next to them.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use serde_json::{Value, json};

fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn layer(n: u32, kind: &str, payload: Value) -> Value {
    json!({"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",n),"name":format!("l{n}"),
        "parent":null,"kind":kind,"payload":payload,"visible":true})
}
/// Layer 1 vector (path SVPT 1), layer 2 raster.
fn document() -> StudioDocument {
    serde_json::from_value(json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"px",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[layer(1,"vector",json!({"payload_kind":"vector","path_ids":[identity("SVPT",1)]})),layer(2,"raster",json!({"payload_kind":"raster","tiles":[]}))],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    }))
    .expect("document shape")
}
fn binding() -> Value {
    json!({"story_id":identity("STXT",1),"path_layer_id":identity("SLYR",1),"path_id":identity("SVPT",1),
        "start_t":0.25,"end_t":1.75,"spacing":2.0,"flipped":"flipped","path_type_alignment":"top_path",
        "text_type_alignment":"above_right_icf_box","path_effect":"stair_step","flow_direction":"right_to_left","align":"center",
        "baseline_offsets":[{"start":0,"end":3,"offset_pt":-1.5},{"start":5,"end":9,"offset_pt":4.0}]})
}
fn records(entries: Vec<Value>) -> Value {
    json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1),"path_texts":entries})
}
fn check(v: &Value) -> Result<(), Code> {
    let r: StudioVectorRecords = serde_json::from_value(v.clone()).map_err(|_| Code::InvalidField)?;
    validate_vector_records(&r, &document(), None, Budget::default(), &CancellationToken::default()).map_err(|d| d.code)
}
fn with(f: impl FnOnce(&mut Value)) -> Value {
    let mut b = binding();
    f(&mut b);
    records(vec![b])
}

#[test]
fn ai_path_text_records_round_trip_and_refuse_hostile_bindings() {
    // Absent member: the bytes of a record set written before it.
    let before = format!(r#"{{"schema_id":"hsk.studio.vector_records@1","document_id":"{}"}}"#, identity("SDOC", 1));
    let r: StudioVectorRecords = serde_json::from_str(&before).unwrap();
    assert!(r.path_texts.is_empty());
    assert_eq!(serde_json::to_string(&r).unwrap(), before);

    // Full binding: validates and round-trips value-identically.
    let full = records(vec![binding()]);
    assert_eq!(check(&full), Ok(()));
    let r: StudioVectorRecords = serde_json::from_value(full.clone()).unwrap();
    let p = &r.path_texts[0];
    assert_eq!((p.flipped, p.path_effect, p.path_type_alignment, p.text_type_alignment), (StudioPathTextFlip::Flipped, StudioPathEffect::StairStep, StudioPathTypeAlignment::TopPath, StudioTextTypeAlignment::AboveRightIcfBox));
    assert_eq!((p.flow_direction, p.align, p.baseline_offsets.len()), (StudioPathFlowDirection::RightToLeft, StudioPathTextAlign::Center, 2));
    assert_eq!(serde_json::to_value(&r).unwrap(), full);
    // Defaults: rainbow, not flipped, centre path, baseline, left to right, start.
    let minimal = json!({"story_id":identity("STXT",2),"path_layer_id":identity("SLYR",1),"path_id":identity("SVPT",1),"start_t":0.0,"end_t":1.0});
    let r: StudioVectorRecords = serde_json::from_value(records(vec![minimal.clone()])).unwrap();
    assert_eq!(r.path_texts[0].path_effect, StudioPathEffect::Rainbow);
    assert_eq!(check(&records(vec![minimal])), Ok(()));

    // Hostile bindings.
    assert_eq!(check(&with(|b| b["path_effect"] = json!("warp"))), Err(Code::InvalidField), "unknown effect");
    assert_eq!(check(&with(|b| b["extra"] = json!(1))), Err(Code::InvalidField), "deny_unknown_fields");
    assert_eq!(check(&with(|b| b["story_id"] = json!(identity("SLYR", 1)))), Err(Code::InvalidId), "story id prefix");
    assert_eq!(check(&with(|b| b["path_layer_id"] = json!(identity("SLYR", 9)))), Err(Code::DanglingReference), "unknown layer");
    assert_eq!(check(&with(|b| b["path_layer_id"] = json!(identity("SLYR", 2)))), Err(Code::DanglingReference), "raster layer holds no path");
    assert_eq!(check(&with(|b| b["path_id"] = json!(identity("SVPT", 7)))), Err(Code::DanglingReference), "path not in the layer");
    assert_eq!(check(&with(|b| b["start_t"] = json!(-0.5))), Err(Code::InvalidField), "negative handle");
    assert_eq!(check(&with(|b| b["end_t"] = json!(2.0e6))), Err(Code::InvalidField), "handle over cap");
    assert_eq!(check(&with(|b| b["spacing"] = json!(1.0e9))), Err(Code::InvalidField), "spacing over cap");
    assert_eq!(check(&with(|b| b["baseline_offsets"] = json!([{"start":4,"end":4,"offset_pt":1.0}]))), Err(Code::InvalidField), "empty range");
    assert_eq!(check(&with(|b| b["baseline_offsets"] = json!([{"start":0,"end":5,"offset_pt":1.0},{"start":3,"end":8,"offset_pt":1.0}]))), Err(Code::InvalidField), "overlapping ranges");
    let many: Vec<Value> = (0..=PATH_TEXT_MAX_OFFSETS as u64).map(|i| json!({"start":i,"end":i + 1,"offset_pt":0.0})).collect();
    assert_eq!(check(&with(|b| b["baseline_offsets"] = json!(many))), Err(Code::BudgetExceeded), "offset count over cap");
    assert_eq!(check(&records(vec![binding(), binding()])), Err(Code::DuplicateIdentity), "one binding per story");
}
