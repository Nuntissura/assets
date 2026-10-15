//! Smart-filter stack record (lane SMB-PS-SELECT-LIQUIFY-2): `StudioRasterLayerRecord::
//! smart_filters` is serde-default and skipped when absent, so earlier record sets stay
//! byte-identical; a present stack round-trips and validates; structural refusals are typed.
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
fn mask_tile(key: &str) -> Value {
    json!({"object_key":key,"layer_id":identity("SLYR",1),"column":0,"row":0,
        "width":{"value":64,"unit":"px"},"height":{"value":64,"unit":"px"},"format":"coverage_u8",
        "colour_profile_id":identity("SCPF",1),"schema_id":"hsk.studio.raster_tile@1",
        "content_digest":{"algorithm":"sha256","digest":"ab12"},"artifact_manifest_id":"manifest-m"})
}
fn document() -> StudioDocument {
    let v = json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"px",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",1),"name":"l1","parent":null,
            "kind":"raster","payload":{"payload_kind":"raster","tiles":[]},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    });
    serde_json::from_value(v).expect("document shape")
}
/// Mesh 8x6, spacing 4: cols = ceil(7/4)+1 = 3, rows = ceil(5/4)+1 = 3.
fn stack() -> Value {
    json!({"enabled":true,
        "stack_mask":{"tiles":[mask_tile("sm_0")],"enabled":true,"linked":true},
        "filters":[
            {"instance_id":"blur-1","enabled":true,"opacity":100.0,"params":{"filter_kind":"gaussian_blur","radius_px":2.0},
             "mask":{"tiles":[mask_tile("fm_0")],"enabled":true,"linked":true}},
            {"instance_id":"noise-1","enabled":false,"opacity":50.0,"params":{"filter_kind":"add_noise","amount_pct":10.0,
             "distribution":"gaussian","monochromatic":true,"seed":7}},
            {"instance_id":"liq-1","enabled":true,"opacity":60.0,"params":{"filter_kind":"liquify","resample":"bicubic",
             "mesh":{"origin_x":0,"origin_y":0,"width":8,"height":6,"spacing":4,"cols":3,"rows":3,
             "displacement":[[0.0,0.0],[0.5,0.0],[0.0,0.0],[0.0,0.0],[1.0,-1.0],[0.0,0.0],[0.0,0.0],[0.0,0.0],[0.0,0.0]],
             "freeze":[0.0,0.0,0.0,0.0,0.0,0.0,0.0,0.0,1.0]}}}
        ]})
}
fn records(with_stack: bool) -> Value {
    let mut layer = json!({"layer_id":identity("SLYR",1)});
    if with_stack {
        layer["smart_filters"] = stack();
    }
    json!({"schema_id":"hsk.studio.raster_records@1","document_id":identity("SDOC",1),"layers":[layer]})
}
fn check(v: Value) -> Result<(), String> {
    let r: StudioRasterRecords = serde_json::from_value(v).map_err(|_| "parse".to_string())?;
    validate_raster_records(&r, &document(), Budget::default(), &CancellationToken::default(), &Caller)
        .map(|_| ())
        .map_err(|e| format!("{:?}", e.code))
}

#[test]
fn absent_stack_is_byte_identical_and_present_stack_round_trips() {
    // Earlier record set: the serialized bytes do not change and carry no new key.
    // The struct's field order is the wire order, so the expected bytes are spelled out.
    let mut old = StudioRasterRecords::new(identity("SDOC", 1));
    old.layers.push(StudioRasterLayerRecord::new(identity("SLYR", 1)));
    let text = serde_json::to_string(&old).unwrap();
    assert_eq!(
        text,
        format!(r#"{{"schema_id":"hsk.studio.raster_records@1","document_id":"{}","layers":[{{"layer_id":"{}"}}]}}"#, identity("SDOC", 1), identity("SLYR", 1))
    );
    let parsed: StudioRasterRecords = serde_json::from_value(records(false)).unwrap();
    assert_eq!(serde_json::to_string(&parsed).unwrap(), text);
    assert!(!text.contains("smart_filters"));
    assert_eq!(parsed.layers[0], StudioRasterLayerRecord::new(identity("SLYR", 1)));
    // A stack round-trips exactly and validates.
    let new = records(true);
    let r: StudioRasterRecords = serde_json::from_value(new.clone()).unwrap();
    let again: StudioRasterRecords = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
    assert_eq!(again, r);
    assert_eq!(serde_json::to_value(&r).unwrap(), new);
    let s = r.layers[0].smart_filters.as_ref().unwrap();
    assert_eq!(s.filters.len(), 3);
    assert!(matches!(s.filters[2].params, StudioSmartFilterKind::Liquify { .. }));
    assert_eq!(check(new), Ok(()));
    assert!(raster_records_schema().as_value().to_string().contains("smart_filters"));
}

#[test]
fn malformed_stacks_are_refused_typed() {
    let with = |f: &dyn Fn(&mut Value)| {
        let mut v = records(true);
        f(&mut v["layers"][0]["smart_filters"]);
        check(v)
    };
    assert_eq!(with(&|s| s["filters"][1]["instance_id"] = json!("blur-1")), Err("InvalidField".into()));
    assert_eq!(with(&|s| s["filters"][0]["opacity"] = json!(150.0)), Err("InvalidField".into()));
    assert_eq!(with(&|s| s["filters"][0]["params"]["radius_px"] = json!(-1.0)), Err("InvalidField".into()));
    assert_eq!(with(&|s| s["filters"][2]["params"]["mesh"]["cols"] = json!(4)), Err("InvalidField".into()));
    assert_eq!(with(&|s| { s["filters"][2]["params"]["mesh"]["freeze"].as_array_mut().unwrap().pop(); }), Err("InvalidField".into()));
    assert_eq!(with(&|s| s["filters"][2]["params"]["mesh"]["freeze"][0] = json!(2.0)), Err("InvalidField".into()));
    // Mask tiles: another layer's tile, a duplicate key, a non-coverage format.
    assert_eq!(with(&|s| s["filters"][0]["mask"]["tiles"][0]["layer_id"] = json!(identity("SLYR", 2))), Err("InvalidField".into()));
    assert_eq!(with(&|s| s["filters"][0]["mask"]["tiles"][0]["object_key"] = json!("sm_0")), Err("InvalidField".into()));
    assert_eq!(with(&|s| s["stack_mask"]["tiles"][0]["format"] = json!("rgb_f32le")), Err("InvalidField".into()));
    // Too many entries; a mesh bomb refuses before any per-vertex walk.
    assert_eq!(
        with(&|s| {
            let f = s["filters"][1].clone();
            s["filters"] = json!((0..17).map(|i| { let mut g = f.clone(); g["instance_id"] = json!(format!("n{i}")); g }).collect::<Vec<_>>());
        }),
        Err("InvalidField".into())
    );
    assert_eq!(
        with(&|s| {
            let m = &mut s["filters"][2]["params"]["mesh"];
            m["width"] = json!(1_000_000);
            m["height"] = json!(1_000_000);
            m["spacing"] = json!(1);
            m["cols"] = json!(1_000_000);
            m["rows"] = json!(1_000_000);
        }),
        Err("BudgetExceeded".into())
    );
    // Closed wire shape.
    assert_eq!(with(&|s| s["filters"][0]["params"]["sigma"] = json!(1.0)), Err("parse".into()));
    assert_eq!(with(&|s| s["filters"][0]["params"]["filter_kind"] = json!("lens_blur")), Err("parse".into()));
}
