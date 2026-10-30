//! Lane SMB-AI-AREA-TYPE-1 slice 2: area-type bindings in `StudioVectorRecords` (STU-TYP-002,
//! STU-TYP-114, STU-TYP-116). Absent member encodes byte-identically; a full binding round-trips;
//! hostile bindings are refused with the code of the rule named next to them.
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
/// Layer 1 vector (paths SVPT 1, 2), layer 2 raster.
fn document() -> StudioDocument {
    serde_json::from_value(json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"px",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[layer(1,"vector",json!({"payload_kind":"vector","path_ids":[identity("SVPT",1),identity("SVPT",2)]})),layer(2,"raster",json!({"payload_kind":"raster","tiles":[]}))],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    }))
    .expect("document shape")
}
fn binding() -> Value {
    json!({"text_id":identity("STXT",1),"container_layer_id":identity("SLYR",1),"container_id":identity("SVPT",1),
        "inset":4.5,"first_baseline":"cap_height","first_baseline_min":2.0,"auto_size":"height_and_width_proportional"})
}
fn records(entries: Vec<Value>) -> Value {
    json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1),"area_texts":entries})
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
fn story(n: u32, kind: &str) -> StudioTextStory {
    serde_json::from_value(json!({"schema_id":"hsk.studio.text_story@1","story_id":identity("STXT",n),"text_kind":kind,
        "story_direction":"left_to_right","content":{"content_kind":"inline","text_utf8":"area"},
        "paragraphs":[{"start":0,"end":4,"style_ref":null,"paragraph_direction":null}],
        "character_ranges":[{"start":0,"end":4,"style_ref":null,"applied_language":null}]}))
    .expect("story shape")
}

#[test]
fn area_text_records_round_trip_and_refuse_hostile_bindings() {
    // Full binding: validates and round-trips value-identically; every STU-TYP-116 member parses.
    let full = records(vec![binding()]);
    assert_eq!(check(&full), Ok(()));
    let r: StudioVectorRecords = serde_json::from_value(full.clone()).unwrap();
    let a = &r.area_texts[0];
    assert_eq!((a.first_baseline, a.auto_size, a.inset, a.first_baseline_min), (StudioFirstBaseline::CapHeight, StudioAutoSize::HeightAndWidthProportional, 4.5, 2.0));
    assert_eq!(serde_json::to_value(&r).unwrap(), full);
    for (token, member) in [("off", StudioAutoSize::Off), ("height_only", StudioAutoSize::HeightOnly), ("width_only", StudioAutoSize::WidthOnly), ("height_and_width", StudioAutoSize::HeightAndWidth), ("height_and_width_proportional", StudioAutoSize::HeightAndWidthProportional)] {
        let v = with(|b| b["auto_size"] = json!(token));
        assert_eq!(check(&v), Ok(()), "{token}");
        let r: StudioVectorRecords = serde_json::from_value(v).unwrap();
        assert_eq!(r.area_texts[0].auto_size, member);
    }
    // Defaults: inset 0, ascent, minimum 0, auto-size off.
    let minimal = json!({"text_id":identity("STXT",2),"container_layer_id":identity("SLYR",1),"container_id":identity("SVPT",2)});
    let r: StudioVectorRecords = serde_json::from_value(records(vec![minimal.clone()])).unwrap();
    assert_eq!((r.area_texts[0].first_baseline, r.area_texts[0].auto_size, r.area_texts[0].inset), (StudioFirstBaseline::Ascent, StudioAutoSize::Off, 0.0));
    assert_eq!(check(&records(vec![binding(), minimal])), Ok(()));

    // Hostile bindings.
    assert_eq!(check(&with(|b| b["auto_size"] = json!("grow"))), Err(Code::InvalidField), "unknown auto-size member");
    assert_eq!(check(&with(|b| b["first_baseline"] = json!("legacy"))), Err(Code::InvalidField), "unknown first baseline");
    assert_eq!(check(&with(|b| b["extra"] = json!(1))), Err(Code::InvalidField), "deny_unknown_fields");
    assert_eq!(check(&with(|b| b["text_id"] = json!(identity("SLYR", 1)))), Err(Code::InvalidId), "story id prefix");
    assert_eq!(check(&with(|b| b["container_id"] = json!(identity("STXT", 1)))), Err(Code::InvalidId), "path id prefix");
    assert_eq!(check(&with(|b| b["container_layer_id"] = json!(identity("SLYR", 9)))), Err(Code::DanglingReference), "unknown layer");
    assert_eq!(check(&with(|b| b["container_layer_id"] = json!(identity("SLYR", 2)))), Err(Code::DanglingReference), "raster layer holds no path");
    assert_eq!(check(&with(|b| b["container_id"] = json!(identity("SVPT", 7)))), Err(Code::DanglingReference), "path not in the layer");
    assert_eq!(check(&with(|b| b["inset"] = json!(-1.0))), Err(Code::InvalidField), "negative inset");
    assert_eq!(check(&with(|b| b["inset"] = json!(1.0e9))), Err(Code::InvalidField), "inset over cap");
    assert_eq!(check(&with(|b| b["first_baseline_min"] = json!(-0.5))), Err(Code::InvalidField), "negative minimum");
    assert_eq!(check(&records(vec![binding(), binding()])), Err(Code::DuplicateIdentity), "one binding per story");
    // A story bound as path text cannot also be area text.
    let mut both = records(vec![binding()]);
    both["path_texts"] = json!([{"story_id":identity("STXT",1),"path_layer_id":identity("SLYR",1),"path_id":identity("SVPT",2),"start_t":0.0,"end_t":1.0}]);
    assert_eq!(check(&both), Err(Code::DuplicateIdentity), "story on a path and in an area");
    let many: Vec<Value> = (0..=AREA_TEXT_MAX_ENTRIES as u32).map(|i| json!({"text_id":identity("STXT",i + 10),"container_layer_id":identity("SLYR",1),"container_id":identity("SVPT",1)})).collect();
    assert_eq!(check(&records(many)), Err(Code::BudgetExceeded), "entry count over cap");

    // Story check where stories are resolved: same id, text_kind area.
    let a = &r.area_texts[0];
    assert_eq!(validate_area_text_story(a, &story(2, "area")).map_err(|d| d.code), Ok(()));
    assert_eq!(validate_area_text_story(a, &story(2, "point")).map_err(|d| d.code), Err(Code::InvalidField));
    assert_eq!(validate_area_text_story(a, &story(2, "path")).map_err(|d| d.code), Err(Code::InvalidField));
    assert_eq!(validate_area_text_story(a, &story(3, "area")).map_err(|d| d.code), Err(Code::DanglingReference));
}

#[test]
fn vector_records_without_area_texts_are_byte_identical() {
    // The bytes of a record set written before the member (with another member present).
    let before = format!(
        r#"{{"schema_id":"hsk.studio.vector_records@1","document_id":"{}","path_texts":[{{"story_id":"{}","path_layer_id":"{}","path_id":"{}","start_t":0.0,"end_t":1.0,"spacing":0.0,"flipped":"not_flipped","path_type_alignment":"center_path","text_type_alignment":"baseline","path_effect":"rainbow","flow_direction":"left_to_right","align":"start"}}]}}"#,
        identity("SDOC", 1),
        identity("STXT", 5),
        identity("SLYR", 1),
        identity("SVPT", 1)
    );
    let r: StudioVectorRecords = serde_json::from_str(&before).unwrap();
    assert!(r.area_texts.is_empty());
    assert_eq!(serde_json::to_string(&r).unwrap(), before);
    let empty = format!(r#"{{"schema_id":"hsk.studio.vector_records@1","document_id":"{}"}}"#, identity("SDOC", 1));
    assert_eq!(serde_json::to_string(&StudioVectorRecords::new(identity("SDOC", 1))).unwrap(), empty);
    // An explicit empty list reads as absent and is not written back.
    let explicit = format!(r#"{{"schema_id":"hsk.studio.vector_records@1","document_id":"{}","area_texts":[]}}"#, identity("SDOC", 1));
    let r: StudioVectorRecords = serde_json::from_str(&explicit).unwrap();
    assert_eq!(serde_json::to_string(&r).unwrap(), empty);
}
