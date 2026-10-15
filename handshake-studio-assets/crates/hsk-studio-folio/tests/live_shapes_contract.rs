//! `StudioLiveShapes` contract (lane SMB-ILLUSTRATOR-PEN slice 2): live shape parameters of a
//! vector layer path (STU-VEC-010, STU-VEC-127 bounds, STU-VEC-112 corner kinds), round trip and
//! typed refusals.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use serde_json::{Value, json};

fn id(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn pt(v: f64) -> Value {
    json!({"value": v, "unit": "pt"})
}
fn document() -> StudioDocument {
    let layer = |n: u32, kind: &str, payload: Value| {
        json!({"schema_id":"hsk.studio.layer@1","layer_id":id("SLYR",n),"name":format!("l{n}"),
            "parent":null,"kind":kind,"payload":payload,"visible":true})
    };
    serde_json::from_value(json!({
        "schema_id":"hsk.studio.document@1","document_id":id("SDOC",1),"revision":1,"geometry_unit":"pt",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[layer(3, "vector", json!({"payload_kind":"vector","path_ids":[id("SVPT",1)]})),
            layer(4, "raster", json!({"payload_kind":"raster","tiles":[]})),
            layer(5, "vector", json!({"payload_kind":"vector","path_ids":[id("SVPT",2)]}))],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    }))
    .expect("document shape")
}
fn shapes() -> Value {
    json!({"schema_id":"hsk.studio.live_shapes@1","document_id":id("SDOC",1),"shapes":[
        {"layer_id":id("SLYR",3),"path_id":id("SVPT",1),"shape":{"shape_kind":"rectangle","x":pt(0.0),"y":pt(0.0),
            "width":pt(100.0),"height":pt(50.0),"corner_radius":[pt(4.0),pt(4.0),pt(0.0),pt(0.0)],
            "corner_kind":"round","corner_smoothing":0.0}},
        {"layer_id":id("SLYR",5),"path_id":id("SVPT",2),"shape":{"shape_kind":"polygon","center_x":pt(10.0),
            "center_y":pt(10.0),"radius":pt(5.0),"side_count":6,"inset_percentage":0.0}}
    ]})
}
fn validate(v: &Value) -> Result<(), Diagnostic> {
    let r: StudioLiveShapes = serde_json::from_value(v.clone()).expect("record shape");
    validate_live_shapes(&r, &document(), Budget::default(), &CancellationToken::default())
}

#[test]
fn live_shapes_round_trip_and_refuse_typed() {
    let r: StudioLiveShapes = serde_json::from_value(shapes()).unwrap();
    assert_eq!(validate(&shapes()), Ok(()));
    let back: StudioLiveShapes = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
    assert_eq!(back, r);
    assert_eq!(r.shape(&id("SLYR", 5)).unwrap().shape.kind(), "polygon");
    assert_eq!(StudioCornerKind::RoundInverted.capture_value(), "inverse rounded corner");
    // Every kind is accepted at its documented bounds.
    let ok = |shape: Value| {
        let mut v = shapes();
        v["shapes"][1]["shape"] = shape;
        validate(&v)
    };
    assert_eq!(ok(json!({"shape_kind":"star","center_x":pt(0.0),"center_y":pt(0.0),"radius":pt(9.0),"point_count":50,"inner_radius":0.4})), Ok(()));
    assert_eq!(ok(json!({"shape_kind":"ellipse","center_x":pt(0.0),"center_y":pt(0.0),"radius_x":pt(9.0),"radius_y":pt(3.0),"start_angle":0.0,"end_angle":270.0,"inner_ratio":0.0})), Ok(()));
    assert_eq!(ok(json!({"shape_kind":"line","x":pt(0.0),"y":pt(0.0),"length":pt(9.0),"angle":45.0})), Ok(()));
    assert_eq!(ok(json!({"shape_kind":"polygon","center_x":pt(0.0),"center_y":pt(0.0),"radius":pt(1.0),"side_count":100,"inset_percentage":100.0})), Ok(()));
    // Bounds (STU-VEC-127) and domains.
    let code = |shape: Value| ok(shape).expect_err("refused");
    assert_eq!(code(json!({"shape_kind":"polygon","center_x":pt(0.0),"center_y":pt(0.0),"radius":pt(1.0),"side_count":101,"inset_percentage":0.0})).target, format!("{}/side_count", id("SLYR", 5)));
    assert_eq!(code(json!({"shape_kind":"star","center_x":pt(0.0),"center_y":pt(0.0),"radius":pt(1.0),"point_count":2,"inner_radius":0.4})).code, Code::InvalidField);
    assert_eq!(code(json!({"shape_kind":"ellipse","center_x":pt(0.0),"center_y":pt(0.0),"radius_x":pt(9.0),"radius_y":pt(3.0),"start_angle":0.0,"end_angle":270.0,"inner_ratio":1.0})).code, Code::InvalidField);
    assert_eq!(code(json!({"shape_kind":"line","x":pt(0.0),"y":pt(0.0),"length":pt(-1.0),"angle":0.0})).code, Code::InvalidUnit);
    assert_eq!(code(json!({"shape_kind":"line","x":{"value":0.0,"unit":"px"},"y":pt(0.0),"length":pt(1.0),"angle":0.0})).code, Code::InvalidUnit);
    let mut wide = shapes();
    wide["shapes"][0]["shape"]["width"] = pt(16348.5);
    assert_eq!(validate(&wide).unwrap_err().code, Code::InvalidField);
    // References: a non-vector layer, a path the layer does not hold, a dangling layer, a duplicate.
    let mut v = shapes();
    v["shapes"][1]["layer_id"] = json!(id("SLYR", 4));
    assert_eq!(validate(&v).unwrap_err().code, Code::PayloadMismatch);
    let mut v = shapes();
    v["shapes"][1]["path_id"] = json!(id("SVPT", 1));
    assert_eq!(validate(&v).unwrap_err().code, Code::DanglingReference);
    let mut v = shapes();
    v["shapes"][1]["layer_id"] = json!(id("SLYR", 9));
    assert_eq!(validate(&v).unwrap_err().code, Code::DanglingReference);
    let mut v = shapes();
    v["shapes"][1] = v["shapes"][0].clone();
    assert_eq!(validate(&v).unwrap_err().code, Code::DuplicateIdentity);
    // Strict wire: unknown fields and unknown kinds are refused at decode.
    let mut v = shapes();
    v["shapes"][0]["shape"]["corner_kind"] = json!("swirl");
    assert!(serde_json::from_value::<StudioLiveShapes>(v).is_err());
    let mut v = shapes();
    v["shapes"][0]["extra"] = json!(1);
    assert!(serde_json::from_value::<StudioLiveShapes>(v).is_err());
    // Budget: more shapes than Budget::nodes is refused before the walk.
    let r: StudioLiveShapes = serde_json::from_value(shapes()).unwrap();
    let tight = Budget { nodes: 1, ..Budget::default() };
    assert_eq!(validate_live_shapes(&r, &document(), tight, &CancellationToken::default()).unwrap_err().code, Code::BudgetExceeded);
}
