//! Lane SMB-AE-KEY-TRACK-3: animated mask paths on a composition layer
//! (`StudioCompositionLayer::masks`, STU-CMP-025/025a). Records without masks serialise byte for
//! byte as before; with masks they round trip byte-identically; every refusal comes from a rule of
//! `validate_mask`.
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
fn quad(dx: f64) -> Value {
    json!([2.0 + dx, 2.0, 0.0, 0.0, 0.0, 0.0, 20.0 + dx, 2.0, 0.0, 0.0, 0.0, 0.0, 20.0 + dx, 15.0, 0.0, 0.0, 0.0, 0.0, 2.0 + dx, 15.0, -1.5, 0.0, 0.0, 1.25])
}
fn masked() -> Value {
    let mut l = layer();
    l["masks"] = json!([
        {"mask_id":"planar","mode":"add","closed":true,"static_value":quad(0.0),"keyframes":[
            {"time":0,"value":quad(0.0),"in_interp":"linear","out_interp":"linear"},
            {"time":10584000000u64,"value":quad(3.5),"in_interp":"linear","out_interp":"linear"}]},
        {"mask_id":"hole","mode":"subtract","inverted":true,"closed":true,"static_value":quad(1.0)},
        {"mask_id":"guide","mode":"none","closed":false,"static_value":[1.0,1.0,0.0,0.0,0.0,0.0,9.0,9.0,0.0,0.0,0.0,0.0]}
    ]);
    l
}
fn check(v: &Value) -> Result<Vec<ResolutionIssue>, Diagnostic> {
    let r: StudioCompositionRecords = serde_json::from_value(v.clone()).expect("record shape");
    validate_composition_records(&r, &document(), Budget::default(), &CancellationToken::default(), &Caller)
}

#[test]
fn mask_paths_are_serde_default_round_trip_and_are_validated() {
    let plain = records(layer());
    let r: StudioCompositionRecords = serde_json::from_value(plain.clone()).unwrap();
    assert!(r.compositions[0].layers[0].masks.is_empty());
    let bytes = serde_json::to_vec(&r).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("masks"));
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), plain);
    let with = records(masked());
    let r: StudioCompositionRecords = serde_json::from_value(with.clone()).unwrap();
    let first = serde_json::to_vec(&r).unwrap();
    let back: StudioCompositionRecords = serde_json::from_slice(&first).unwrap();
    assert_eq!(back, r);
    assert_eq!(serde_json::to_vec(&back).unwrap(), first);
    let masks = &r.compositions[0].layers[0].masks;
    assert_eq!((masks[1].mode, masks[1].inverted, masks[2].closed), (StudioCompositionMaskMode::Subtract, true, false));
    check(&with).unwrap();
    let refused = |f: &dyn Fn(&mut Value), field: &str| {
        let mut l = masked();
        f(&mut l);
        let d = check(&records(l)).expect_err("must be refused");
        assert!(d.target.contains("masks") && d.target.contains(field), "{field}: {d:?}");
    };
    refused(&|l| l["masks"][0]["static_value"] = json!([1.0, 2.0, 3.0]), "static_value");
    refused(&|l| l["masks"][0]["static_value"] = json!([]), "static_value");
    refused(&|l| l["masks"][0]["keyframes"][1]["value"] = json!(quad(0.0).as_array().unwrap()[..18].to_vec()), "keyframes/value");
    refused(&|l| l["masks"][0]["keyframes"][1]["time"] = json!(0), "keyframes/time");
    refused(&|l| l["masks"][1]["static_value"][3] = json!(2e12), "static_value");
    refused(&|l| l["masks"][2]["mode"] = json!("add"), "closed");
    refused(&|l| l["masks"][0]["mask_id"] = json!(""), "mask_id");
    let mut dup = masked();
    dup["masks"][1]["mask_id"] = json!("planar");
    assert_eq!(check(&records(dup)).unwrap_err().code, Code::DuplicateIdentity);
    let mut many = layer();
    many["masks"] = Value::Array((0..65).map(|i| json!({"mask_id":format!("m{i}"),"mode":"add","closed":true,"static_value":quad(0.0)})).collect());
    assert_eq!(check(&records(many)).unwrap_err().code, Code::BudgetExceeded);
    let mut unknown = masked();
    unknown["masks"][0]["feather"] = json!(2.0);
    assert!(serde_json::from_value::<StudioCompositionRecords>(records(unknown)).is_err(), "unknown fields are refused");
}
