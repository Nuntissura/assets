//! Lane SMB-AE-KEY-TRACK-2: the corner pin of a composition layer (`StudioCompositionLayer::
//! corner_pin`). Records without it serialise byte for byte as before; with it they round trip
//! byte-identically; every refusal comes from a rule of `validate_corner_pin`.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use serde_json::{Value, json};

fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
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
fn document() -> StudioDocument {
    let layers = vec![json!({"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR", 1),"name":"l1",
        "parent":null,"kind":"raster","payload":{"payload_kind":"raster","tiles":[]},"visible":true})];
    serde_json::from_value(json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"px",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":layers,
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    }))
    .expect("document shape")
}
fn records(layer: Value) -> Value {
    json!({"schema_id":"hsk.studio.composition_records@1","document_id":identity("SDOC",1),"compositions":[
        {"schema_id":"hsk.studio.composition@1","composition_id":"main","name":"main","width":64,"height":48,
         "pixel_aspect":{"numerator":1,"denominator":1},"frame_rate":{"numerator":24,"denominator":1},
         "duration":254016000000u64,"layers":[layer]}]})
}
fn layer() -> Value {
    json!({"layer_id":identity("SLYR",1),
        "timing":{"in_point":0,"out_point":254016000000u64,"start_time":0,"stretch":{"numerator":1,"denominator":1}}})
}
fn pinned() -> Value {
    let mut l = layer();
    l["corner_pin"] = json!({"from":[[0.0,0.0],[32.0,0.0],[32.0,24.0],[0.0,24.0]],
        "static_value":[1.0,2.0,30.0,0.5,33.5,26.0,-1.0,22.0],
        "keyframes":[
            {"time":0,"value":[1.0,2.0,30.0,0.5,33.5,26.0,-1.0,22.0],"in_interp":"linear","out_interp":"linear"},
            {"time":10584000000u64,"value":[3.25,4.0,31.0,1.5,35.0,27.0,0.125,23.0],"in_interp":"linear","out_interp":"linear"}]});
    l
}
fn check(v: &Value) -> Result<Vec<ResolutionIssue>, Diagnostic> {
    let r: StudioCompositionRecords = serde_json::from_value(v.clone()).expect("record shape");
    validate_composition_records(&r, &document(), Budget::default(), &CancellationToken::default(), &Caller)
}

#[test]
fn corner_pin_is_serde_default_round_trips_and_is_validated() {
    // Without a pin: the field is absent on the wire and the bytes are the input's.
    let plain = records(layer());
    let r: StudioCompositionRecords = serde_json::from_value(plain.clone()).unwrap();
    assert!(r.compositions[0].layers[0].corner_pin.is_none());
    let bytes = serde_json::to_vec(&r).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("corner_pin"));
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), plain);
    check(&plain).unwrap();
    // With a pin: byte-identical round trip, valid.
    let with = records(pinned());
    let r: StudioCompositionRecords = serde_json::from_value(with.clone()).unwrap();
    let first = serde_json::to_vec(&r).unwrap();
    let back: StudioCompositionRecords = serde_json::from_slice(&first).unwrap();
    assert_eq!(back, r);
    assert_eq!(serde_json::to_vec(&back).unwrap(), first);
    let pin = r.compositions[0].layers[0].corner_pin.as_ref().unwrap();
    assert_eq!(pin.keyframes.len(), 2);
    check(&with).unwrap();
    // Refusals: each names the corner pin field.
    let refused = |f: &dyn Fn(&mut Value), field: &str| {
        let mut l = pinned();
        f(&mut l);
        let d = check(&records(l)).expect_err("must be refused");
        assert!(d.target.contains("corner_pin") && d.target.contains(field), "{field}: {d:?}");
    };
    refused(&|l| l["corner_pin"]["from"] = json!([[0.0,0.0],[32.0,24.0],[32.0,0.0],[0.0,24.0]]), "from");
    refused(&|l| l["corner_pin"]["from"] = json!([[0.0,0.0],[1.0,1.0],[2.0,2.0],[3.0,3.0]]), "from");
    refused(&|l| l["corner_pin"]["static_value"] = json!([1.0,2.0,30.0,0.5,33.5,26.0]), "static_value");
    refused(&|l| l["corner_pin"]["static_value"] = json!([1.0,2.0,30.0,0.5,33.5,26.0,-1.0,1e13]), "static_value");
    refused(&|l| l["corner_pin"]["keyframes"][1]["value"] = json!([0.0,0.0,10.0,10.0,10.0,0.0,0.0,10.0]), "keyframes/value");
    refused(&|l| l["corner_pin"]["keyframes"][1]["time"] = json!(0), "keyframes/time");
    // An unknown field inside the pin is a shape error (deny_unknown_fields).
    let mut l = pinned();
    l["corner_pin"]["extra"] = json!(1);
    assert!(serde_json::from_value::<StudioCompositionRecords>(records(l)).is_err());
}
