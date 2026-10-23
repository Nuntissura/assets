//! Lane SMB-AI-TYPE-PATH-RECOLOR slice 3: gradient meshes in `StudioVectorRecords` (STU-VEC-034,
//! STU-COL-036, STU-VEC-059). Absent member encodes byte-identically; a 1 x 2 mesh round-trips;
//! hostile meshes are refused with the code of the rule named next to them.
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
/// Layers 1 and 3 vector, 2 raster.
fn document() -> StudioDocument {
    let v = json!({"payload_kind":"vector","path_ids":[identity("SVPT",1)]});
    serde_json::from_value(json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"px",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[layer(1,"vector",v.clone()),layer(2,"raster",json!({"payload_kind":"raster","tiles":[]})),layer(3,"vector",v)],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    }))
    .expect("document shape")
}
fn point(x: f64, y: f64, opacity: f64) -> Value {
    json!({"x":x,"y":y,"colour":{"r":1.0,"g":0.5,"b":0.0,"colour_profile_id":null},"opacity":opacity})
}
/// A 1 x 2 mesh over (0,0)-(20,10): 6 points, 4 horizontal and 3 vertical mesh lines.
fn mesh() -> Value {
    let h = |x0: f64, y: f64| json!([[x0 + 10.0 / 3.0, y], [x0 + 20.0 / 3.0, y]]);
    let v = |x: f64| json!([[x, 10.0 / 3.0], [x, 20.0 / 3.0]]);
    json!({"mesh_id":"mesh-0","layer_id":identity("SLYR",1),"source_path_id":identity("SVPT",1),"rows":1,"columns":2,
        "points":[point(0.0,0.0,100.0),point(10.0,0.0,50.0),point(20.0,0.0,100.0),point(0.0,10.0,100.0),point(10.0,10.0,0.0),point(20.0,10.0,100.0)],
        "h_handles":[h(0.0,0.0),h(10.0,0.0),h(0.0,10.0),h(10.0,10.0)],
        "v_handles":[v(0.0),v(10.0),v(20.0)]})
}
fn records(meshes: Vec<Value>) -> Value {
    json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1),"gradient_meshes":meshes})
}
fn check(v: &Value) -> Result<(), Code> {
    let r: StudioVectorRecords = serde_json::from_value(v.clone()).map_err(|_| Code::InvalidField)?;
    validate_vector_records(&r, &document(), None, Budget::default(), &CancellationToken::default()).map_err(|d| d.code)
}
fn with(f: impl FnOnce(&mut Value)) -> Value {
    let mut m = mesh();
    f(&mut m);
    records(vec![m])
}

#[test]
fn ai_gradient_mesh_records_round_trip_and_refuse_hostile_meshes() {
    let before = format!(r#"{{"schema_id":"hsk.studio.vector_records@1","document_id":"{}"}}"#, identity("SDOC", 1));
    let r: StudioVectorRecords = serde_json::from_str(&before).unwrap();
    assert!(r.gradient_meshes.is_empty());
    assert_eq!(serde_json::to_string(&r).unwrap(), before);

    let full = records(vec![mesh()]);
    assert_eq!(check(&full), Ok(()));
    let r: StudioVectorRecords = serde_json::from_value(full.clone()).unwrap();
    let m = &r.gradient_meshes[0];
    assert_eq!((m.rows, m.columns, m.points.len(), m.h_handles.len(), m.v_handles.len(), m.points[4].opacity), (1, 2, 6, 4, 3, 0.0));
    assert_eq!(serde_json::to_value(&r).unwrap(), full);
    // A tensor mesh: four interior points per patch.
    let tensor = with(|m| m["tensor"] = json!([[[3.0,3.0],[3.0,7.0],[7.0,3.0],[7.0,7.0]],[[13.0,3.0],[13.0,7.0],[17.0,3.0],[17.0,7.0]]]));
    assert_eq!(check(&tensor), Ok(()));

    assert_eq!(check(&with(|m| m["layer_id"] = json!(identity("SLYR", 2)))), Err(Code::PayloadMismatch), "raster layer");
    assert_eq!(check(&with(|m| m["layer_id"] = json!(identity("SLYR", 9)))), Err(Code::DanglingReference), "unknown layer");
    assert_eq!(check(&with(|m| m["mesh_id"] = json!("mesh 0"))), Err(Code::InvalidId), "mesh id key");
    assert_eq!(check(&with(|m| m["source_path_id"] = json!(identity("SLYR", 1)))), Err(Code::InvalidId), "source path prefix");
    assert_eq!(check(&with(|m| m["rows"] = json!(0))), Err(Code::InvalidField), "empty grid");
    assert_eq!(check(&with(|m| m["columns"] = json!(257))), Err(Code::BudgetExceeded), "columns over cap");
    assert_eq!(check(&with(|m| { m["rows"] = json!(200); m["columns"] = json!(200); })), Err(Code::BudgetExceeded), "patches over cap");
    assert_eq!(check(&with(|m| m["rows"] = json!(2))), Err(Code::InvalidField), "point count does not match the grid");
    assert_eq!(check(&with(|m| m["h_handles"].as_array_mut().unwrap().pop().map(|_| ()).unwrap())), Err(Code::InvalidField), "h_handles count");
    assert_eq!(check(&with(|m| m["points"][1]["opacity"] = json!(100.5))), Err(Code::InvalidField), "opacity range");
    assert_eq!(check(&with(|m| m["points"][1]["colour"]["g"] = json!(1.5))), Err(Code::InvalidField), "colour range");
    assert_eq!(check(&with(|m| m["v_handles"][0][0][0] = json!("inf"))), Err(Code::InvalidField), "non-numeric handle");
    assert_eq!(check(&with(|m| m["tensor"] = json!([[[0.0,0.0],[0.0,0.0],[0.0,0.0],[0.0,0.0]]]))), Err(Code::InvalidField), "tensor count");
    let mut second = mesh();
    second["mesh_id"] = json!("mesh-1");
    assert_eq!(check(&records(vec![mesh(), second])), Err(Code::DuplicateIdentity), "one mesh per layer");
    assert_eq!(check(&records(vec![mesh(), mesh()])), Err(Code::DuplicateIdentity), "duplicate mesh id");
}
