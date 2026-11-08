//! Lane SMB-MOTION-COMMANDS-2 ([STU-MOT-005] `solid` and `adjustment`, [STU-CMP-015]): the
//! `solid_layers` member of `hsk.studio.motion_records@1` (absent when empty, so earlier record sets
//! stay byte-identical) and the composition adjustment layer, a document layer of kind `adjustment`
//! with a `group` payload. Records are written as JSON so the red commit compiles before the types.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use serde_json::{Value, json};

fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn slyr(n: u32) -> String {
    identity("SLYR", n)
}
struct Caller;
impl Resolver for Caller {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Available
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Available
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        true
    }
}

fn raster(n: u32, tiles: Value) -> Value {
    json!({"schema_id":"hsk.studio.layer@1","layer_id":slyr(n),"name":format!("l{n}"),"parent":null,"kind":"raster",
        "payload":{"payload_kind":"raster","tiles":tiles},"visible":true})
}
fn tile(n: u32) -> Value {
    json!({"object_key":"t","layer_id":slyr(n),"column":0,"row":0,"width":{"value":4,"unit":"px"},"height":{"value":4,"unit":"px"},
        "format":"caller-format","colour_profile_id":identity("SCPF",1),"schema_id":"hsk.studio.raster_tile@1",
        "content_digest":{"algorithm":"sha256","digest":"00"},"artifact_manifest_id":"manifest-1"})
}
fn adjustment(n: u32) -> Value {
    json!({"schema_id":"hsk.studio.layer@1","layer_id":slyr(n),"name":"Adjustment","parent":null,"kind":"adjustment",
        "payload":{"payload_kind":"group"},"visible":true})
}
fn source(n: u32) -> Value {
    json!({"layer_id":slyr(n),"operation_key":"source","operation_kind":"source","inputs":[],
        "outputs":[{"port_key":"out","value_type":"image","cardinality":"one","semantic":"source"}]})
}
/// Layers 1-3 empty rasters, 4 a raster with a tile, 5 a composition adjustment layer.
fn document_value() -> Value {
    json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"px",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[raster(1, json!([])), raster(2, json!([])), raster(3, json!([])), raster(4, json!([tile(4)])), adjustment(5)],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[source(5)],"edges":[],"outputs":[]}
    })
}
fn document() -> StudioDocument {
    serde_json::from_value(document_value()).expect("document shape")
}
fn compositions() -> StudioCompositionRecords {
    let layer = |n: u32| json!({"layer_id":slyr(n),
        "timing":{"in_point":0,"out_point":254016000000u64,"start_time":0,"stretch":{"numerator":1,"denominator":1}}});
    serde_json::from_value(json!({"schema_id":"hsk.studio.composition_records@1","document_id":identity("SDOC",1),
        "compositions":[{"schema_id":"hsk.studio.composition@1","composition_id":identity("SCOM",9),"name":"c","width":64,"height":32,
            "pixel_aspect":{"numerator":1,"denominator":1},"frame_rate":{"numerator":25,"denominator":1},
            "duration":254016000000u64,"layers":[layer(1),layer(2),layer(4),layer(5)]}]}))
    .expect("composition shape")
}
fn solid(n: u32) -> Value {
    json!({"layer_id":slyr(n),"width":64,"height":32,"colour":[0.25,0.5,1.0,1.0]})
}
fn records(solids: Value) -> Value {
    json!({"schema_id":"hsk.studio.motion_records@1","document_id":identity("SDOC",1),"solid_layers":solids})
}
fn check(v: &Value) -> Result<(), Diagnostic> {
    let r: StudioMotionRecords = serde_json::from_value(v.clone()).map_err(|e| Diagnostic { code: Code::InvalidField, target: format!("shape: {e}") })?;
    validate_motion_records(&r, &document(), &compositions(), Budget::default(), &CancellationToken::default(), &Caller)
}

#[test]
fn solid_records_round_trip_validate_and_leave_older_sets_byte_identical() {
    let v = records(json!([solid(1)]));
    check(&v).expect("a solid on an empty raster layer of a composition validates");
    let r: StudioMotionRecords = serde_json::from_value(v.clone()).unwrap();
    let bytes = serde_json::to_vec(&r).unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), v, "wire form is the record as written");
    let back: StudioMotionRecords = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(back, r);
    // A set without solids carries no `solid_layers` member: byte-identical to the earlier form.
    let empty = StudioMotionRecords::new(identity("SDOC", 1));
    assert_eq!(
        serde_json::to_string(&empty).unwrap(),
        format!("{{\"schema_id\":\"hsk.studio.motion_records@1\",\"document_id\":\"{}\"}}", identity("SDOC", 1))
    );
    let older = json!({"schema_id":"hsk.studio.motion_records@1","document_id":identity("SDOC",1),
        "composition_work_areas":[{"composition_id":identity("SCOM",9),"start_tick":0,"end_tick":10}]});
    let parsed: StudioMotionRecords = serde_json::from_value(older.clone()).unwrap();
    assert_eq!(serde_json::to_value(&parsed).unwrap(), older);
    assert!(serde_json::to_string(&motion_records_schema()).unwrap().contains("solid_layers"));
}

#[test]
fn solid_records_are_refused_typed() {
    let code = |solids: Value| check(&records(solids)).expect_err("must be refused");
    // Size bound 1..=30000 (STU-MOT-122 resize bound; STU-MOT-005 states none: Studio reading).
    for (w, h) in [(0, 32), (64, 0), (30_001, 32), (64, 30_001)] {
        let mut s = solid(1);
        s["width"] = json!(w);
        s["height"] = json!(h);
        let d = code(json!([s]));
        assert_eq!((d.code, d.target.as_str()), (Code::InvalidField, format!("{}/size", slyr(1)).as_str()), "{w}x{h}");
    }
    let mut s = solid(1);
    s["width"] = json!(30_000);
    s["height"] = json!(30_000);
    check(&records(json!([s]))).expect("the bound itself is accepted");
    // Colour components finite in 0..=1 (the shape fill rule).
    for bad in [json!([1.5, 0.0, 0.0, 1.0]), json!([-0.1, 0.0, 0.0, 1.0]), json!([0.0, 0.0, 0.0, 2.0])] {
        let mut s = solid(1);
        s["colour"] = bad;
        let d = code(json!([s]));
        assert_eq!((d.code, d.target.as_str()), (Code::InvalidField, format!("{}/colour", slyr(1)).as_str()));
    }
    // A layer in no composition, a layer with content, a doubled record.
    assert_eq!(code(json!([solid(3)])).code, Code::DanglingReference);
    let d = code(json!([solid(4)]));
    assert_eq!((d.code, d.target.as_str()), (Code::PayloadMismatch, slyr(4).as_str()), "a solid's document layer is an empty raster layer");
    assert_eq!(code(json!([solid(5)])).code, Code::PayloadMismatch, "an adjustment layer is not a solid host");
    assert_eq!(code(json!([solid(1), solid(1)])).code, Code::DuplicateIdentity);
    // One procedural record per layer: not also a shape or text-animator layer.
    let mut v = records(json!([solid(1)]));
    v["shape_layers"] = json!([{"layer_id":slyr(1),"root":{"items":[]}}]);
    assert_eq!(check(&v).expect_err("solid and shape").code, Code::DuplicateIdentity);
    let mut v = records(json!([solid(2)]));
    v["text_animators"] = json!([{"layer_id":slyr(2),"animators":[]}]);
    assert_eq!(check(&v).expect_err("solid and text animators").code, Code::DuplicateIdentity);
    // Unknown members are refused (deny_unknown_fields).
    let mut s = solid(1);
    s["opacity"] = json!(1.0);
    assert!(serde_json::from_value::<StudioMotionRecords>(records(json!([s]))).is_err());
}

#[test]
fn a_group_payload_adjustment_layer_validates_and_needs_no_pigment_record() {
    let bytes = serde_json::to_vec(&document_value()).unwrap();
    let snapshot = match inspect_bytes(&bytes, Budget::default(), &CancellationToken::default(), &Caller).expect("document validates") {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("read only"),
    };
    let doc = snapshot.document();
    let l = doc.layers.iter().find(|l| l.layer_id == slyr(5)).unwrap();
    assert_eq!((l.kind.as_str(), &l.payload), ("adjustment", &Payload::Group));
    // The raster records demand a pigment adjustment record only for the primitive-reference form.
    let raster: StudioRasterRecords = serde_json::from_value(json!({"schema_id":"hsk.studio.raster_records@1","document_id":identity("SDOC",1)})).unwrap();
    validate_raster_records(&raster, doc, Budget::default(), &CancellationToken::default(), &Caller).expect("no record required");
    // A pigment adjustment record on the group form is refused (it hosts none).
    let raster: StudioRasterRecords = serde_json::from_value(json!({"schema_id":"hsk.studio.raster_records@1","document_id":identity("SDOC",1),
        "layers":[{"layer_id":slyr(5),"adjustment":{"kind":"invert"}}]}))
    .unwrap();
    assert_eq!(validate_raster_records(&raster, doc, Budget::default(), &CancellationToken::default(), &Caller).expect_err("hosts none").code, Code::PayloadMismatch);
    // Its graph operation is an image source; any other payload of kind adjustment stays refused.
    let mut v = document_value();
    v["layers"][4]["payload"] = json!({"payload_kind":"raster","tiles":[]});
    let bytes = serde_json::to_vec(&v).unwrap();
    assert!(inspect_bytes(&bytes, Budget::default(), &CancellationToken::default(), &Caller).is_err());
}
