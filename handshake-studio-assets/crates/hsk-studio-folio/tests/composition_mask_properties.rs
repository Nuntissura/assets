//! Lane SMB-AE-MASK-FEATHER-1: the feather, opacity and expansion streams of a composition mask
//! (`StudioCompositionMask`, STU-CMP-025 "mask feather ..., mask opacity, and mask expansion", all
//! keyframable; STU-CMP-026 opacity stored 0..1). Red-first: at c719b1dc `feather` was an unknown
//! field (`deny_unknown_fields`), so `feather_opacity_and_expansion_are_accepted_and_round_trip`
//! failed there. Records without the streams keep their bytes; every refusal is a typed
//! `InvalidField` from `validate_mask` naming the mask and the stream; the 64-mask and 1024-vertex
//! budgets are unchanged.
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
fn records(masks: Value) -> Value {
    json!({"schema_id":"hsk.studio.composition_records@1","document_id":identity("SDOC",1),"compositions":[
        {"schema_id":"hsk.studio.composition@1","composition_id":"main","name":"main","width":64,"height":48,
         "pixel_aspect":{"numerator":1,"denominator":1},"frame_rate":{"numerator":24,"denominator":1},
         "duration":254016000000u64,"layers":[{"layer_id":identity("SLYR",1),
            "timing":{"in_point":0,"out_point":254016000000u64,"start_time":0,"stretch":{"numerator":1,"denominator":1}},
            "masks":masks}]}]})
}
fn quad(n: usize) -> Value {
    let mut v = Vec::new();
    for k in 0..n {
        let a = k as f64 * std::f64::consts::TAU / n as f64;
        v.extend_from_slice(&[10.0 + 8.0 * a.cos(), 10.0 + 8.0 * a.sin(), 0.0, 0.0, 0.0, 0.0]);
    }
    json!(v)
}
fn key(time: u64, value: Value) -> Value {
    json!({"time":time,"value":value,"in_interp":"linear","out_interp":"linear"})
}
/// A mask carrying all three streams: feather keyframed 0 -> 8 px, opacity static 0.5,
/// expansion keyframed -2 -> 3 px.
fn shaped() -> Value {
    json!({"mask_id":"soft","mode":"add","closed":true,"static_value":quad(4),
        "feather":{"static_value":[0.0,0.0],"keyframes":[key(0, json!([0.0,0.0])), key(10584000000, json!([8.0,4.0]))]},
        "opacity":{"static_value":[0.5]},
        "expansion":{"static_value":[-2.0],"keyframes":[key(0, json!([-2.0])), key(21168000000, json!([3.0]))]}})
}
fn check(v: &Value) -> Result<Vec<ResolutionIssue>, Diagnostic> {
    let r: StudioCompositionRecords = serde_json::from_value(v.clone()).expect("record shape");
    validate_composition_records(&r, &document(), Budget::default(), &CancellationToken::default(), &Caller)
}

#[test]
fn feather_opacity_and_expansion_are_accepted_and_round_trip() {
    let with = records(json!([shaped()]));
    let r: StudioCompositionRecords = serde_json::from_value(with.clone()).expect("feather, opacity and expansion are known fields");
    let m = &r.compositions[0].layers[0].masks[0];
    let feather = m.feather.as_ref().expect("feather");
    assert_eq!((feather.static_value.as_slice(), feather.keyframes.len()), (&[0.0, 0.0][..], 2));
    assert_eq!(m.opacity.as_ref().map(|o| o.static_value.clone()), Some(vec![0.5]));
    assert_eq!(m.expansion.as_ref().map(|e| e.keyframes[1].value.clone()), Some(vec![3.0]));
    check(&with).expect("valid shaped mask");
    let first = serde_json::to_vec(&r).unwrap();
    let back: StudioCompositionRecords = serde_json::from_slice(&first).unwrap();
    assert_eq!(back, r);
    assert_eq!(serde_json::to_vec(&back).unwrap(), first);
    assert_eq!(serde_json::from_slice::<Value>(&first).unwrap(), with);
    // Expansion is signed; opacity takes both ends of its stored range; feather 0 on one axis.
    let mut edges = shaped();
    edges["opacity"] = json!({"static_value":[0.0],"keyframes":[key(0, json!([0.0])), key(10, json!([1.0]))]});
    edges["feather"] = json!({"static_value":[0.0, 12.5]});
    edges["expansion"] = json!({"static_value":[-1.0e12]});
    check(&records(json!([edges]))).expect("range ends are valid");
}

/// A record written before the streams existed decodes and encodes to the same bytes (the new
/// fields are serde default and skipped when absent).
#[test]
fn existing_records_are_byte_identical() {
    let id = identity("SLYR", 1);
    let doc = identity("SDOC", 1);
    let before = format!(
        concat!(
            r#"{{"schema_id":"hsk.studio.composition_records@1","document_id":"{doc}","compositions":[{{"schema_id":"hsk.studio.composition@1","#,
            r#""composition_id":"main","name":"main","width":64,"height":48,"pixel_aspect":{{"numerator":1,"denominator":1}},"#,
            r#""frame_rate":{{"numerator":24,"denominator":1}},"duration":254016000000,"layers":[{{"layer_id":"{id}","#,
            r#""timing":{{"in_point":0,"out_point":254016000000,"start_time":0,"stretch":{{"numerator":1,"denominator":1}}}},"#,
            r#""masks":[{{"mask_id":"planar","mode":"add","closed":true,"static_value":[2.0,2.0,0.0,0.0,0.0,0.0,20.0,2.0,0.0,0.0,0.0,0.0,20.0,15.0,-1.5,0.0,0.0,1.25],"#,
            r#""keyframes":[{{"time":0,"value":[2.0,2.0,0.0,0.0,0.0,0.0,20.0,2.0,0.0,0.0,0.0,0.0,20.0,15.0,-1.5,0.0,0.0,1.25],"in_interp":"linear","out_interp":"linear"}}]}},"#,
            r#"{{"mask_id":"hole","mode":"subtract","inverted":true,"closed":true,"static_value":[4.0,4.0,0.0,0.0,0.0,0.0,9.0,4.0,0.0,0.0,0.0,0.0,9.0,9.0,0.0,0.0,0.0,0.0]}}]}}]}}]}}"#
        ),
        doc = doc,
        id = id
    );
    let r: StudioCompositionRecords = serde_json::from_str(&before).expect("pre-lane record");
    check(&serde_json::from_str(&before).unwrap()).expect("pre-lane record is valid");
    let masks = &r.compositions[0].layers[0].masks;
    assert!(masks.iter().all(|m| m.feather.is_none() && m.opacity.is_none() && m.expansion.is_none()));
    assert_eq!(serde_json::to_string(&r).unwrap(), before, "decode then encode keeps every byte");
}

#[test]
fn out_of_range_streams_are_refused_typed() {
    let refused = |f: &dyn Fn(&mut Value), field: &str| {
        let mut m = shaped();
        f(&mut m);
        let d = check(&records(json!([m]))).expect_err("must be refused");
        assert_eq!(d.code, Code::InvalidField, "{field}: {d:?}");
        assert!(d.target.contains("/masks/soft/") && d.target.ends_with(field), "{field}: {d:?}");
    };
    // Opacity is stored 0..=1 (STU-CMP-026): the percent-display value 1.5 (or 50) is refused.
    refused(&|m| m["opacity"] = json!({"static_value":[1.5]}), "opacity/static_value");
    refused(&|m| m["opacity"] = json!({"static_value":[50.0]}), "opacity/static_value");
    refused(&|m| m["opacity"] = json!({"static_value":[-0.01]}), "opacity/static_value");
    refused(&|m| m["opacity"]["keyframes"] = json!([key(0, json!([0.5])), key(5, json!([1.5]))]), "opacity/keyframes/value");
    // Feather is >= 0 on both axes.
    refused(&|m| m["feather"]["static_value"] = json!([-1.0, 0.0]), "feather/static_value");
    refused(&|m| m["feather"]["keyframes"][1]["value"] = json!([0.0, -1.0]), "feather/keyframes/value");
    // Wrong component counts.
    refused(&|m| m["feather"]["static_value"] = json!([2.0]), "feather/static_value");
    refused(&|m| m["opacity"]["static_value"] = json!([0.5, 0.5]), "opacity/static_value");
    refused(&|m| m["expansion"]["static_value"] = json!([]), "expansion/static_value");
    // Expansion is finite and within the coordinate bound.
    refused(&|m| m["expansion"]["static_value"] = json!([2.0e12]), "expansion/static_value");
    refused(&|m| m["expansion"]["static_value"] = json!([-2.0e12]), "expansion/static_value");
    // Keyframe rules of the path stream: strictly increasing times, bezier needs tangents.
    refused(&|m| m["expansion"]["keyframes"][1]["time"] = json!(0), "expansion/keyframes/time");
    refused(&|m| m["feather"]["keyframes"][0]["out_interp"] = json!("bezier"), "feather/keyframes/tangent");
    // Non-finite values cannot be written as JSON numbers; a typed record carries them.
    let mut r: StudioCompositionRecords = serde_json::from_value(records(json!([shaped()]))).unwrap();
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        r.compositions[0].layers[0].masks[0].expansion = Some(StudioKeyedValue::constant(vec![bad]));
        let d = validate_composition_records(&r, &document(), Budget::default(), &CancellationToken::default(), &Caller).unwrap_err();
        assert!(d.code == Code::InvalidField && d.target.ends_with("/masks/soft/expansion/static_value"), "{bad}: {d:?}");
    }
    r.compositions[0].layers[0].masks[0].expansion = None;
    r.compositions[0].layers[0].masks[0].feather = Some(StudioKeyedValue::constant(vec![f64::NAN, 1.0]));
    let d = validate_composition_records(&r, &document(), Budget::default(), &CancellationToken::default(), &Caller).unwrap_err();
    assert!(d.target.ends_with("/masks/soft/feather/static_value"), "{d:?}");
    // An unknown member of a stream is still refused at decode.
    let mut extra = shaped();
    extra["opacity"]["percent"] = json!(50);
    assert!(serde_json::from_value::<StudioCompositionRecords>(records(json!([extra]))).is_err());
}

#[test]
fn mask_and_vertex_budgets_are_unchanged() {
    let many = |n: usize| Value::Array((0..n).map(|i| {
        let mut m = shaped();
        m["mask_id"] = json!(format!("m{i}"));
        m
    }).collect());
    check(&records(many(MAX_LAYER_MASKS))).expect("64 shaped masks");
    assert_eq!(check(&records(many(MAX_LAYER_MASKS + 1))).unwrap_err().code, Code::BudgetExceeded);
    let mut wide = shaped();
    wide["static_value"] = quad(MAX_MASK_PATH_VERTICES);
    check(&records(json!([wide]))).expect("1024 vertices");
    let mut over = shaped();
    over["static_value"] = quad(MAX_MASK_PATH_VERTICES + 1);
    let d = check(&records(json!([over]))).unwrap_err();
    assert!(d.code == Code::InvalidField && d.target.ends_with("/masks/soft/static_value"), "{d:?}");
    assert_eq!((MAX_LAYER_MASKS, MAX_MASK_PATH_VERTICES), (64, 1024));
}
