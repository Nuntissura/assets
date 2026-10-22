//! Lane SMB-AE-3D: 3D layer, camera and light records on composition layers (STU-CMP-035, 040,
//! 045, 046; STU-MOT-013/014). Records without them serialise byte for byte as before; with them
//! they round trip byte-identically; refusals name the field; intensity is never clamped.
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
    let layers: Vec<Value> = (1..=3)
        .map(|n| {
            json!({"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR", n),"name":format!("l{n}"),
            "parent":null,"kind":"raster","payload":{"payload_kind":"raster","tiles":[]},"visible":true})
        })
        .collect();
    serde_json::from_value(json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"px",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":layers,
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    }))
    .expect("document shape")
}
fn records(layers: Vec<Value>) -> Value {
    json!({"schema_id":"hsk.studio.composition_records@1","document_id":identity("SDOC",1),"compositions":[
        {"schema_id":"hsk.studio.composition@1","composition_id":"main","name":"main","width":64,"height":48,
         "pixel_aspect":{"numerator":1,"denominator":1},"frame_rate":{"numerator":24,"denominator":1},
         "duration":254016000000u64,"layers":layers}]})
}
fn layer(n: u32) -> Value {
    json!({"layer_id":identity("SLYR",n),
        "timing":{"in_point":0,"out_point":254016000000u64,"start_time":0,"stretch":{"numerator":1,"denominator":1}}})
}
fn scene() -> Vec<Value> {
    let mut plane = layer(1);
    plane["three_d"] = json!({"position_z":{"static_value":[40.0],"keyframes":[
            {"time":0,"value":[40.0],"in_interp":"linear","out_interp":"linear"},
            {"time":10584000000u64,"value":[-12.5],"in_interp":"linear","out_interp":"linear"}]},
        "rotation_y":{"static_value":[30.0]},"orientation":{"static_value":[0.0,90.0,0.0]},
        "material":{"accepts_shadows":true,"values":{"diffuse":{"static_value":[75.0]},"shadow_colour":{"static_value":[0.1,0.0,0.2]}}}});
    let mut cam = layer(2);
    cam["three_d"] = json!({"position_z":{"static_value":[-200.0]}});
    cam["camera"] = json!({"zoom":{"static_value":[177.7]},"point_of_interest":{"static_value":[32.0,24.0,0.0]},
        "depth_of_field":true,"iris_shape":"hexagon","values":{"aperture":{"static_value":[12.0]}}});
    let mut light = layer(3);
    light["three_d"] = json!({});
    light["light"] = json!({"light_type":"spot","colour":{"static_value":[1.0,0.9,0.8]},"intensity":{"static_value":[-250.0]},
        "falloff_type":"smooth","casts_shadows":true,"values":{"cone_angle":{"static_value":[45.0]}}});
    vec![plane, cam, light]
}
fn check(v: &Value) -> Result<Vec<ResolutionIssue>, Diagnostic> {
    let r: StudioCompositionRecords = serde_json::from_value(v.clone()).expect("record shape");
    validate_composition_records(&r, &document(), Budget::default(), &CancellationToken::default(), &Caller)
}

#[test]
fn three_d_records_are_serde_default_round_trip_and_are_bounded() {
    // Without 3D members: absent on the wire, bytes unchanged.
    let plain = records(vec![layer(1)]);
    let r: StudioCompositionRecords = serde_json::from_value(plain.clone()).unwrap();
    let bytes = serde_json::to_vec(&r).unwrap();
    for f in ["three_d", "camera", "light"] {
        assert!(!String::from_utf8_lossy(&bytes).contains(f));
    }
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), plain);
    // With them: the input value back exactly, byte-identical re-encode, valid.
    let with = records(scene());
    let r: StudioCompositionRecords = serde_json::from_value(with.clone()).unwrap();
    let first = serde_json::to_vec(&r).unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&first).unwrap(), with);
    let back: StudioCompositionRecords = serde_json::from_slice(&first).unwrap();
    assert_eq!(serde_json::to_vec(&back).unwrap(), first);
    check(&with).unwrap();
    let l = &r.compositions[0].layers;
    assert_eq!(l[2].light.as_ref().unwrap().intensity.static_value, vec![-250.0], "intensity is never clamped");
    assert!(l[0].three_d.as_ref().unwrap().material.accepts_lights, "defaults: accepts lights");
    assert!(!l[2].three_d.as_ref().unwrap().material.casts_shadows, "defaults: no shadow");
    // Zoom and angle of view are two views of one value.
    let aov = camera_angle_of_view(177.7, 64.0);
    assert!((camera_zoom_of_angle(aov, 64.0) - 177.7).abs() < 1e-9);
    // Refusals name the field.
    let refused = |f: &dyn Fn(&mut Vec<Value>), field: &str| {
        let mut s = scene();
        f(&mut s);
        let d = check(&records(s)).expect_err("must be refused");
        assert!(d.target.contains(field), "{field}: {d:?}");
    };
    refused(&|s| s[1].as_object_mut().unwrap().remove("three_d").map(|_| ()).unwrap(), "camera");
    refused(&|s| s[1]["light"] = s[2]["light"].clone(), "camera");
    refused(&|s| s[1]["camera"]["zoom"]["static_value"] = json!([0.0]), "zoom");
    refused(&|s| s[1]["camera"]["values"]["cone_angle"] = json!({"static_value":[10.0]}), "values");
    refused(&|s| s[0]["three_d"]["material"]["values"]["index_of_refraction"] = json!({"static_value":[0.5]}), "index_of_refraction");
    refused(&|s| s[0]["three_d"]["orientation"]["static_value"] = json!([0.0, 1.0]), "orientation");
    refused(&|s| s[0]["three_d"]["position_z"]["static_value"] = json!([1e13]), "position_z");
    refused(&|s| s[0]["three_d"]["position_z"]["keyframes"][1]["time"] = json!(0), "position_z");
    refused(&|s| s[2]["light"]["values"]["cone_angle"]["static_value"] = json!([190.0]), "cone_angle");
    // Unknown members are shape errors.
    let mut s = scene();
    s[0]["three_d"]["material"]["glow"] = json!(1);
    assert!(serde_json::from_value::<StudioCompositionRecords>(records(s)).is_err());
}

#[test]
fn immersive_settings_are_serde_default_and_bounded() {
    // Lane SMB-AE-3D-2 (STU-CMP-055a / STU-VID-018): the nine immersive fields on a composition.
    let plain = records(vec![layer(1)]);
    let r: StudioCompositionRecords = serde_json::from_value(plain.clone()).unwrap();
    assert!(!String::from_utf8_lossy(&serde_json::to_vec(&r).unwrap()).contains("immersive"), "absent: bytes unchanged");
    let settings = json!({"projection_type":1,"stereoscopic_type":0,"stereoscopic_eye":0,
        "captured_horizontal_view":360.0,"captured_vertical_view":180.0,
        "field_of_horizontal_view":108.0,"field_of_vertical_view":108.0,"ambisonics_monitoring_type":0,
        "ambisonics_hrir":"hrir-default"});
    let mut with = records(scene());
    with["compositions"][0]["immersive"] = settings.clone();
    let r: StudioCompositionRecords = serde_json::from_value(with.clone()).unwrap();
    let bytes = serde_json::to_vec(&r).unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), with, "round trip");
    check(&with).unwrap();
    assert_eq!(r.compositions[0].immersive.as_ref().unwrap().projection_type, IMMERSIVE_PROJECTION_EQUIRECTANGULAR);
    for (field, bad) in [
        ("projection_type", json!(7)),
        ("stereoscopic_type", json!(3)),
        ("stereoscopic_eye", json!(2)),
        ("captured_horizontal_view", json!(0.0)),
        ("captured_vertical_view", json!(181.0)),
        ("field_of_horizontal_view", json!(400.0)),
        ("field_of_vertical_view", json!(-1.0)),
        ("ambisonics_monitoring_type", json!(1)),
        ("ambisonics_hrir", json!("")),
    ] {
        let mut v = with.clone();
        v["compositions"][0]["immersive"][field] = bad;
        let d = check(&v).expect_err(field);
        assert!(d.target.contains(field), "{field}: {d:?}");
    }
    let mut v = with.clone();
    v["compositions"][0]["immersive"]["projection"] = json!("equirect");
    assert!(serde_json::from_value::<StudioCompositionRecords>(v).is_err(), "unknown members are shape errors");
    let mut v = with;
    v["compositions"][0]["immersive"].as_object_mut().unwrap().remove("stereoscopic_eye");
    assert!(serde_json::from_value::<StudioCompositionRecords>(v).is_err(), "every field but the hrir reference is required");
}
