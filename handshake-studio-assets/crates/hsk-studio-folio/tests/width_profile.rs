//! Lane SMB-VECTOR-STYLES: the `StudioWidthProfile` record in `StudioVectorRecords::width_profiles`
//! (STU-VEC-122): absent and empty encode byte-identically to record sets written before it, a
//! profile round-trips, and hostile profiles are refused typed by `validate_vector_records`.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use serde_json::{Value, json};

const DOC: &str = "SDOC-019abcde-0000-7000-8000-000000000001";
const PATH: &str = "SVPT-019abcde-0000-7000-8000-000000000001";

fn document() -> StudioDocument {
    serde_json::from_value(json!({
        "schema_id":"hsk.studio.document@1","document_id":DOC,"revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],"layers":[],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    }))
    .expect("document shape")
}

fn check(profiles: Value) -> std::result::Result<(), Code> {
    let r: StudioVectorRecords = serde_json::from_value(json!({"schema_id":"hsk.studio.vector_records@1","document_id":DOC,"width_profiles":profiles})).expect("records shape");
    validate_vector_records(&r, &document(), None, Budget::default(), &CancellationToken::default()).map_err(|d| d.code)
}

#[test]
fn width_profiles_are_byte_identical_round_trip_and_refuse_hostile_input() {
    // A record set written before width profiles encodes to the same bytes.
    let before = format!(r#"{{"schema_id":"hsk.studio.vector_records@1","document_id":"{DOC}"}}"#);
    let r: StudioVectorRecords = serde_json::from_str(&before).unwrap();
    assert!(r.width_profiles.is_empty());
    assert_eq!(serde_json::to_string(&r).unwrap(), before);
    // A profile round-trips; default editing flags and symmetric widths are not encoded.
    let profile = json!([{"path_id":PATH,"points":[{"position":0.0,"width":1.0},{"position":0.5,"width":4.0,"width_out":2.0},{"position":1.0,"width":1.0}],"single_width_only_incoming":true}]);
    assert_eq!(check(profile.clone()), Ok(()));
    let r: StudioVectorRecords = serde_json::from_value(json!({"schema_id":"hsk.studio.vector_records@1","document_id":DOC,"width_profiles":profile.clone()})).unwrap();
    assert_eq!(serde_json::to_value(&r).unwrap()["width_profiles"], profile);
    assert_eq!(r.width_profile(PATH).map(StudioWidthProfile::is_symmetric), Some(false));
    // Hostile profiles: one point, non-increasing positions, out-of-range position, negative or
    // non-finite width, too many points, a duplicate path, a malformed path id.
    let pts = |p: Value| json!([{"path_id":PATH,"points":p}]);
    assert_eq!(check(pts(json!([{"position":0.0,"width":1.0}]))), Err(Code::InvalidField));
    assert_eq!(check(pts(json!([{"position":0.5,"width":1.0},{"position":0.5,"width":1.0}]))), Err(Code::InvalidField));
    assert_eq!(check(pts(json!([{"position":0.0,"width":1.0},{"position":1.5,"width":1.0}]))), Err(Code::InvalidField));
    assert_eq!(check(pts(json!([{"position":0.0,"width":-1.0},{"position":1.0,"width":1.0}]))), Err(Code::InvalidField));
    assert_eq!(check(pts(json!([{"position":0.0,"width":1.0},{"position":1.0,"width":1.0,"width_out":1e300}]))), Err(Code::InvalidField));
    let many: Vec<Value> = (0..=WIDTH_PROFILE_MAX_POINTS).map(|i| json!({"position": i as f64 / 1000.0, "width": 1.0})).collect();
    assert_eq!(check(pts(Value::Array(many))), Err(Code::BudgetExceeded));
    let one = json!({"path_id":PATH,"points":[{"position":0.0,"width":1.0},{"position":1.0,"width":1.0}]});
    assert!(check(json!([one.clone(), one])).is_err(), "duplicate path");
    assert!(check(json!([{"path_id":"SVPT-bad","points":[{"position":0.0,"width":1.0},{"position":1.0,"width":1.0}]}])).is_err());
}
