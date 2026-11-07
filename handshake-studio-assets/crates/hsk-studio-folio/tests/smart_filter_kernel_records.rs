//! Raster filter kernel kinds of the smart-filter record (lane SMB-PIGMENT-FILTER-KERNELS-1). Folio
//! checks shape and finiteness only: the ranges are pigment's and refused at lowering (STU-FX-106).
//! Every case goes over the wire, because the record arrives as untrusted JSON.
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
    serde_json::from_value(json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"px",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",1),"name":"l1","parent":null,
            "kind":"raster","payload":{"payload_kind":"raster","tiles":[]},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    }))
    .expect("document shape")
}

/// One legal record per new kind.
fn kinds() -> Vec<Value> {
    let weights: Vec<i32> = (0..25).map(|i| if i == 12 { 1 } else { 0 }).collect();
    vec![
        json!({"filter_kind":"box_blur","radius_px":2.0,"iterations":3,"dimensions":"horizontal_and_vertical"}),
        json!({"filter_kind":"motion_blur","angle_deg":-45.0,"length_px":10.0}),
        json!({"filter_kind":"radial_blur","centre_x_px":-3.5,"centre_y_px":4.0,"amount":20.0,"kind":"zoom"}),
        json!({"filter_kind":"find_edges","invert":true}),
        json!({"filter_kind":"custom_convolution","weights":weights,"scale":1,"offset":-3}),
        json!({"filter_kind":"sharpen"}),
        json!({"filter_kind":"sharpen_more"}),
        json!({"filter_kind":"basic_blur"}),
        json!({"filter_kind":"blur_more"}),
        json!({"filter_kind":"sharpen_edges"}),
        json!({"filter_kind":"despeckle"}),
        json!({"filter_kind":"emboss","angle_deg":-135.0,"height_px":3.0,"amount_pct":100.0}),
        json!({"filter_kind":"solarize","threshold_levels":128}),
        json!({"filter_kind":"mosaic","horizontal_blocks":4,"vertical_blocks":3}),
        json!({"filter_kind":"maximum","radius_px":3,"preserve":"squareness"}),
        json!({"filter_kind":"minimum","radius_px":3,"preserve":"roundness"}),
        json!({"filter_kind":"dust_and_scratches","radius_px":2,"threshold":0.1}),
        json!({"filter_kind":"surface_blur","radius_px":5,"threshold_levels":15.0}),
        json!({"filter_kind":"bilateral_blur","radius_px":5,"range_sigma":0.1}),
    ]
}

fn records(params: Value) -> Value {
    json!({"schema_id":"hsk.studio.raster_records@1","document_id":identity("SDOC",1),"layers":[{"layer_id":identity("SLYR",1),
        "smart_filters":{"enabled":true,"filters":[{"instance_id":"f-1","enabled":true,"opacity":100.0,"params":params}]}}]})
}

fn check(v: Value) -> Result<StudioRasterRecords, String> {
    let r: StudioRasterRecords = serde_json::from_value(v).map_err(|e| format!("parse: {e}"))?;
    validate_raster_records(&r, &document(), Budget::default(), &CancellationToken::default(), &Caller).map_err(|e| format!("validate: {e:?}"))?;
    Ok(r)
}

#[test]
fn every_kernel_kind_parses_validates_and_round_trips() {
    for k in kinds() {
        let v = records(k.clone());
        let r = check(v).unwrap_or_else(|e| panic!("{k}: {e}"));
        let back = serde_json::to_value(&r).unwrap();
        assert_eq!(back["layers"][0]["smart_filters"]["filters"][0]["params"], k, "round trip");
    }
}

#[test]
fn unknown_fields_and_kinds_are_refused_on_the_wire() {
    for k in kinds() {
        let mut bad = k.clone();
        bad["extra"] = json!(1);
        let e = check(records(bad)).unwrap_err();
        assert!(e.starts_with("parse"), "{k}: {e}");
    }
    assert!(check(records(json!({"filter_kind":"not_a_filter"}))).unwrap_err().starts_with("parse"));
    assert!(check(records(json!({"filter_kind":"custom_convolution","weights":[1,2,3],"scale":1,"offset":0}))).unwrap_err().starts_with("parse"));
    assert!(check(records(json!({"filter_kind":"box_blur","radius_px":2.0,"iterations":3,"dimensions":"diagonal"}))).unwrap_err().starts_with("parse"));
}

/// 1e39 is finite on the wire and infinite as f32: Folio refuses it, for every float of every kind.
#[test]
fn a_float_that_overflows_f32_is_refused_for_every_float_field() {
    for (kind, fields) in [
        ("box_blur", &["radius_px"][..]),
        ("motion_blur", &["angle_deg", "length_px"][..]),
        ("radial_blur", &["centre_x_px", "centre_y_px", "amount"][..]),
        ("emboss", &["angle_deg", "height_px", "amount_pct"][..]),
        ("dust_and_scratches", &["threshold"][..]),
        ("surface_blur", &["threshold_levels"][..]),
        ("bilateral_blur", &["range_sigma"][..]),
    ] {
        let good = kinds().into_iter().find(|k| k["filter_kind"] == kind).unwrap();
        for f in fields {
            let mut bad = good.clone();
            bad[*f] = json!(1e39);
            assert!(check(records(bad)).is_err(), "{kind}.{f}");
        }
    }
}

/// STU-FX-106: Folio invents no range. A negative angle or a value above the clip blur cap passes
/// here, and an out-of-range value is pigment's to refuse.
#[test]
fn folio_invents_no_range_for_the_new_kinds() {
    for k in [
        json!({"filter_kind":"motion_blur","angle_deg":-720.0,"length_px":10.0}),
        json!({"filter_kind":"motion_blur","angle_deg":0.0,"length_px":999999.0}),
        json!({"filter_kind":"emboss","angle_deg":-1.0,"height_px":-1.0,"amount_pct":-1.0}),
        json!({"filter_kind":"custom_convolution","weights":vec![0;25],"scale":0,"offset":0}),
        json!({"filter_kind":"maximum","radius_px":4294967295u32,"preserve":"squareness"}),
        json!({"filter_kind":"surface_blur","radius_px":4294967295u32,"threshold_levels":-5.0}),
    ] {
        check(records(k.clone())).unwrap_or_else(|e| panic!("{k}: {e}"));
    }
}
