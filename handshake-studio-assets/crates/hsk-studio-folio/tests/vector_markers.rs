//! Lane SMB-AI-ARROWHEADS-1 slice 1: stroke arrowheads in `StudioVectorRecords::stroke_markers`
//! (STU-VEC-146). A set without markers encodes byte-identically; a set with markers round-trips
//! through wire JSON; every hostile entry is refused with its typed code; swap exchanges the ends.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use serde_json::{Value, json};

fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn document() -> StudioDocument {
    let layer = json!({"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",1),"name":"l1","parent":null,
        "kind":"vector","payload":{"payload_kind":"vector","path_ids":[identity("SVPT",1)]},"visible":true});
    serde_json::from_value(json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"px",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[layer],"graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    }))
    .expect("document shape")
}
/// Style 1: row 0 fill, rows 1 and 2 strokes.
fn design() -> StudioDesignRecords {
    let paint = json!({"paint_kind":"solid","colour":{"r":0.0,"g":0.0,"b":0.0,"colour_profile_id":null}});
    let fill = json!({"row_kind":"fill","paint":paint,"opacity":100.0,"blend_mode":2,"visible":true});
    let stroke = json!({"row_kind":"stroke","paint":paint,
        "stroke":{"weight":{"value":2.0,"unit":"px"},"cap":"butt","join":"miter","align":"CENTER","miter_limit":4.0,"dash":[],"dash_offset":{"value":0.0,"unit":"px"}},
        "opacity":100.0,"blend_mode":2,"visible":true});
    serde_json::from_value(json!({"schema_id":"hsk.studio.design_records@1","document_id":identity("SDOC",1),
        "paint_styles":[{"schema_id":"hsk.studio.style_registry@1","style_id":identity("SSTY",1),"kind":"PAINT","name":"s1",
        "publishing":{"description":"","description_markdown":"","documentation_links":[],"publish_key":"s1","is_remote":false},
        "rows":[fill, stroke.clone(), stroke]}]}))
    .expect("design shape")
}
fn entry(row: u32, start: &str, end: &str) -> Value {
    json!({"style_id":identity("SSTY",1),"row":row,"start":start,"end":end,"start_scale":50.0,"end_scale":300.0,
        "alignment":"arrow_align.inside_path"})
}
fn full() -> Value {
    let mut outside = entry(2, "ARROW_LINES", "ERD_MANY");
    outside["alignment"] = json!("arrow_align.outside_path");
    json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1),
        "stroke_markers":[entry(1, "arrow.none", "arrow.triangle"), outside]})
}
fn check(v: &Value) -> Result<(), Code> {
    let r: StudioVectorRecords = serde_json::from_value(v.clone()).map_err(|_| Code::InvalidJson)?;
    validate_vector_records(&r, &document(), Some(&design()), Budget::default(), &CancellationToken::default())
        .map_err(|d| d.code)
}
fn with(k: usize, field: &str, v: Value) -> Result<(), Code> {
    let mut x = full();
    x["stroke_markers"][k][field] = v;
    check(&x)
}

#[test]
fn a_record_set_without_markers_is_byte_identical() {
    let before = format!(r#"{{"schema_id":"hsk.studio.vector_records@1","document_id":"{}"}}"#, identity("SDOC", 1));
    let r: StudioVectorRecords = serde_json::from_str(&before).unwrap();
    assert!(r.stroke_markers.is_empty());
    assert_eq!(serde_json::to_string(&r).unwrap(), before);
    assert_eq!(check(&serde_json::from_str(&before).unwrap()), Ok(()));
    // An explicitly empty member is the same set and is not encoded.
    let empty = format!(r#"{{"schema_id":"hsk.studio.vector_records@1","document_id":"{}","stroke_markers":[]}}"#, identity("SDOC", 1));
    let r: StudioVectorRecords = serde_json::from_str(&empty).unwrap();
    assert_eq!(serde_json::to_string(&r).unwrap(), before);
    assert_eq!(serde_json::to_string(&StudioVectorRecords::new(identity("SDOC", 1))).unwrap(), before);
}

#[test]
fn stroke_markers_round_trip_through_wire_json() {
    let f = full();
    assert_eq!(check(&f), Ok(()));
    let r: StudioVectorRecords = serde_json::from_value(f.clone()).unwrap();
    assert_eq!(serde_json::to_value(&r).unwrap(), f, "value for value");
    let text = serde_json::to_string(&r).unwrap();
    let again: StudioVectorRecords = serde_json::from_str(&text).unwrap();
    assert_eq!(again, r, "text round trip");
    let m = r.stroke_marker(&identity("SSTY", 1), 1).expect("row 1 entry");
    assert_eq!(m.start_marker(), Some(StudioArrowhead::ArrowNone));
    assert_eq!(m.end_marker(), Some(StudioArrowhead::ArrowTriangle));
    assert_eq!((m.start_scale, m.end_scale, m.alignment), (50.0, 300.0, StudioArrowAlignment::InsidePath));
    assert!(r.stroke_marker(&identity("SSTY", 1), 0).is_none());
    // The catalogue: 26 tokens stored verbatim, two of them "no marker", six connector terminators.
    assert_eq!(StudioArrowhead::ALL.len(), 26);
    for a in StudioArrowhead::ALL {
        assert_eq!(StudioArrowhead::from_token(a.token()), Some(a));
        assert_eq!(with(0, "end", json!(a.token())), Ok(()), "{} is stored", a.token());
    }
    assert_eq!(StudioArrowhead::ALL.iter().filter(|a| a.is_none()).count(), 2);
    assert_eq!(StudioArrowhead::ALL.iter().filter(|a| a.suite() == StudioArrowheadSuite::Connector).count(), 6);
    // The schema names the member and its bounds.
    let schema = serde_json::to_string(&vector_records_schema()).unwrap();
    assert!(schema.contains("stroke_markers") && schema.contains("arrow_align.outside_path"));
    // `new` uses the studio-declared default scale.
    let n = StudioStrokeMarkers::new(identity("SSTY", 1), 1, StudioArrowhead::Round, StudioArrowhead::ArrowBar, StudioArrowAlignment::OutsidePath);
    assert_eq!((n.start.as_str(), n.end.as_str(), n.start_scale, n.end_scale), ("ROUND", "arrow.bar", DEFAULT_MARKER_SCALE, 100.0));
}

#[test]
fn hostile_marker_entries_are_refused_with_typed_codes() {
    assert_eq!(with(0, "style_id", json!(identity("SSTY", 9))), Err(Code::DanglingReference), "dangling style");
    assert_eq!(with(0, "style_id", json!("not-an-id")), Err(Code::InvalidId), "style id grammar");
    assert_eq!(with(0, "row", json!(3)), Err(Code::DanglingReference), "row past the style's rows");
    assert_eq!(with(0, "row", json!(0)), Err(Code::InvalidField), "row 0 is a fill row");
    assert_eq!(with(0, "row", json!(2)), Err(Code::DuplicateIdentity), "two entries for one row");
    assert_eq!(with(0, "start_scale", json!(0.0)), Err(Code::InvalidField), "scale below 1");
    assert_eq!(with(0, "end_scale", json!(1001.0)), Err(Code::InvalidField), "scale above 1000");
    assert_eq!(with(0, "start_scale", json!(1.0)), Ok(()), "hard minimum");
    assert_eq!(with(0, "end_scale", json!(1000.0)), Ok(()), "hard maximum");
    assert_eq!(with(0, "end_scale", json!(0.999_999)), Err(Code::InvalidField));
    assert_eq!(with(0, "start", json!("arrow.Triangle")), Err(Code::InvalidField), "tokens are exact text");
    assert_eq!(with(0, "end", json!("ERD_TWO")), Err(Code::InvalidField), "unknown token");
    assert_eq!(with(0, "end", json!(" arrow.bar")), Err(Code::InvalidField), "no trimming");
    assert_eq!(with(0, "alignment", json!("arrow_align.center")), Err(Code::InvalidJson), "closed alignment set");
    assert_eq!(with(0, "start_scale", json!("100")), Err(Code::InvalidJson));
    // Without design records a binding cannot resolve its style.
    let r: StudioVectorRecords = serde_json::from_value(full()).unwrap();
    let code = validate_vector_records(&r, &document(), None, Budget::default(), &CancellationToken::default()).map_err(|d| d.code);
    assert_eq!(code, Err(Code::DanglingReference));
    // The entry budget is checked before any entry is examined.
    let mut many = full();
    many["stroke_markers"] = Value::Array(vec![json!({}); MAX_STROKE_MARKERS + 1]);
    assert!(check(&many).is_err());
    let mut r = StudioVectorRecords::new(identity("SDOC", 1));
    let one = StudioStrokeMarkers::new(identity("SSTY", 1), 1, StudioArrowhead::ArrowBar, StudioArrowhead::ArrowBar, StudioArrowAlignment::InsidePath);
    r.stroke_markers = vec![one; MAX_STROKE_MARKERS + 1];
    let code = validate_vector_records(&r, &document(), Some(&design()), Budget::default(), &CancellationToken::default()).map_err(|d| d.code);
    assert_eq!(code, Err(Code::BudgetExceeded));
}

#[test]
fn swap_exchanges_the_ends_and_their_scales() {
    let r: StudioVectorRecords = serde_json::from_value(full()).unwrap();
    let m = r.stroke_marker(&identity("SSTY", 1), 2).unwrap();
    let s = swap_markers(m);
    assert_eq!((s.start.as_str(), s.end.as_str()), ("ERD_MANY", "ARROW_LINES"));
    assert_eq!((s.start_scale, s.end_scale), (300.0, 50.0));
    assert_eq!((s.style_id.as_str(), s.row, s.alignment), (m.style_id.as_str(), m.row, StudioArrowAlignment::OutsidePath));
    assert_eq!(&swap_markers(&s), m, "swap twice is the identity");
}
