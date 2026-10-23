//! `StudioRasterRecords` contract: adjustment records (STU-RAS-151), mask parameters
//! (STU-RAS-115/124), advanced blending (STU-RAS-041/159, STU-VEC-153 knockout) and effect stacks
//! (STU-RAS-156..158). Every expected code is derived from the rule named next to it.
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
fn mask_tile(layer: u32) -> Value {
    json!({"object_key":"m_0_0","layer_id":identity("SLYR",layer),"column":0,"row":0,
        "width":{"value":64,"unit":"px"},"height":{"value":64,"unit":"px"},"format":"coverage_u8",
        "colour_profile_id":identity("SCPF",1),"schema_id":"hsk.studio.raster_tile@1",
        "content_digest":{"algorithm":"sha256","digest":"ab12"},"artifact_manifest_id":"manifest-m"})
}
/// Raster (1) with a coverage mask, adjustment layer (2) hosting its own adjustment, raster (3).
fn document() -> StudioDocument {
    let layer = |n: u32, kind: &str, payload: Value| {
        json!({"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",n),"name":format!("l{n}"),
            "parent":null,"kind":kind,"payload":payload,"visible":true})
    };
    let mut masked = layer(1, "raster", json!({"payload_kind":"raster","tiles":[]}));
    masked["appearance"] = json!({"opacity":100.0,"fill_opacity":100.0,"blend_mode":2,"clipped":false,
        "isolation":"isolated","mask":{"tiles":[mask_tile(1)],"enabled":true,"linked":true}});
    let v = json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"px",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[
            masked,
            layer(2, "adjustment", json!({"payload_kind":"primitive_reference",
                "schema_id":"hsk.studio.adjustment@1","object_id":identity("SLYR",2)})),
            layer(3, "raster", json!({"payload_kind":"raster","tiles":[]})),
        ],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    });
    serde_json::from_value(v).expect("document shape")
}
fn black() -> Value {
    json!({"r":0.0,"g":0.0,"b":0.0,"colour_profile_id":null})
}
fn effect(id: &str, params: Value) -> Value {
    json!({"instance_id":id,"enabled":true,"visible_in_editor":true,"blend_mode":5,"opacity":75.0,
        "colour":black(),"anti_alias":false,"noise":0.0,"angle":120.0,"use_global_light":true,"params":params})
}
/// A full, valid record set (as JSON, so tests can break single fields).
fn records() -> Value {
    json!({
        "schema_id":"hsk.studio.raster_records@1","document_id":identity("SDOC",1),
        "global_light":{"angle":120.0,"altitude":30.0},
        "layers":[
            {"layer_id":identity("SLYR",1),
             "mask":{"density":50.0,"feather":12.5,"inverted":true},
             "advanced_blending":{
                "blend_ranges":[
                    {"channel":"gray","source_black":[0.0,0.0],"source_white":[200.0,240.0],
                     "destination_black":[50.0,100.0],"destination_white":[255.0,255.0]},
                    {"channel":"red","source_black":[0.0,0.0],"source_white":[255.0,255.0],
                     "destination_black":[0.0,0.0],"destination_white":[255.0,255.0]}],
                "knockout":"enabled","channel_blend":{"red":true,"green":true,"blue":false},
                "blend_interior_effects":false,"blend_clipped_effects":true,"transparency_shapes_layer":true},
             "effects":{"effect_stack_id":identity("SEFX",1),"enabled":true,"scale":100.0,"effects":[
                effect("ds1", json!({"effect_kind":"drop_shadow","distance":5.0,"spread":0.0,"size":10.0,"layer_knocks_out":true})),
                effect("ds2", json!({"effect_kind":"drop_shadow","distance":30000.0,"spread":100.0,"size":65.0,"layer_knocks_out":false})),
                effect("st1", json!({"effect_kind":"stroke","size":3.0,"position":"outside","fill_type":"solid","overprint":false})),
                effect("co1", json!({"effect_kind":"colour_overlay"})),
                effect("be1", json!({"effect_kind":"bevel_emboss","style":"inner_bevel","technique":"smooth","direction":"up",
                    "depth":100.0,"size":5.0,"soften":0.0,"altitude":30.0,
                    "gloss_contour":{"name":"Linear","points":[{"input":0.0,"output":0.0,"corner":false},{"input":255.0,"output":255.0,"corner":false}]},
                    "highlight_mode":9,"highlight_colour":black(),"highlight_opacity":75.0,
                    "shadow_mode":5,"shadow_colour":black(),"shadow_opacity":75.0,
                    "anti_alias_gloss":false,"use_contour":false,"use_texture":false}))]}},
            {"layer_id":identity("SLYR",2),
             "adjustment":{"kind":"curves","curves":[
                {"channel":"composite","points":[{"input":0.0,"output":255.0,"corner":false},{"input":255.0,"output":0.0,"corner":false}]},
                {"channel":"red","points":[{"input":0.0,"output":0.0,"corner":false},{"input":128.0,"output":160.0,"corner":true},{"input":255.0,"output":255.0,"corner":false}]}]}}
        ]
    })
}
fn validate(v: &Value) -> Result<Vec<ResolutionIssue>, Diagnostic> {
    let r: StudioRasterRecords = serde_json::from_value(v.clone()).expect("record shape");
    validate_raster_records(
        &r,
        &document(),
        Budget::default(),
        &CancellationToken::default(),
        &Caller,
    )
}
fn code(v: &Value) -> Code {
    validate(v).expect_err("must be refused").code
}

#[test]
fn full_record_set_validates_and_round_trips() {
    let v = records();
    assert_eq!(validate(&v), Ok(vec![]));
    let r: StudioRasterRecords = serde_json::from_value(v).unwrap();
    let again: StudioRasterRecords =
        serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
    assert_eq!(again, r);
    // Absent members are not encoded; an empty set is just its identity.
    let empty = StudioRasterRecords::new(identity("SDOC", 1));
    let text = serde_json::to_string(&empty).unwrap();
    assert!(!text.contains("layers") && !text.contains("global_light"));
    // Multi-instance (STU-RAS-156): two drop shadows survive in order.
    let fx = r
        .layer(&identity("SLYR", 1))
        .unwrap()
        .effects
        .as_ref()
        .unwrap();
    let kinds: Vec<_> = fx.effects.iter().map(|e| e.params.key()).collect();
    assert_eq!(
        kinds,
        [
            "drop_shadow",
            "drop_shadow",
            "stroke",
            "colour_overlay",
            "bevel_emboss"
        ]
    );
    // STU-VEC-153 discriminants.
    assert_eq!(
        [
            StudioKnockout::Unknown,
            StudioKnockout::Disabled,
            StudioKnockout::Enabled,
            StudioKnockout::Inherited
        ]
        .map(StudioKnockout::discriminant),
        [-1, 0, 1, 2]
    );
    // Unknown fields are refused by the closed wire shape.
    let mut extra = records();
    extra["layers"][0]["mask"]["opacity"] = json!(1.0);
    assert!(serde_json::from_value::<StudioRasterRecords>(extra).is_err());
    assert!(
        raster_records_schema()
            .as_value()
            .to_string()
            .contains("raster_records")
    );
}

#[test]
fn spec_ranges_are_enforced_with_typed_codes() {
    let set = |path: &[&str], value: Value| {
        let mut v = records();
        let mut cur = &mut v;
        for p in path {
            cur = match p.parse::<usize>() {
                Ok(i) => &mut cur[i],
                Err(_) => &mut cur[*p],
            };
        }
        *cur = value;
        v
    };
    let l0 = ["layers", "0"];
    let at = |rest: &[&'static str]| [&l0[..], rest].concat();
    // STU-RAS-115/124: density 0..100 percent, feather 0..10000 px.
    assert_eq!(
        code(&set(&at(&["mask", "density"]), json!(100.5))),
        Code::InvalidField
    );
    assert_eq!(
        code(&set(&at(&["mask", "feather"]), json!(10000.5))),
        Code::InvalidField
    );
    // STU-RAS-159: values 0..255, each pair ordered, one entry per channel.
    let br = |rest: &[&'static str]| {
        at(&[&["advanced_blending", "blend_ranges", "0"][..], rest].concat())
    };
    assert_eq!(
        code(&set(&br(&["source_white"]), json!([240.0, 200.0]))),
        Code::InvalidField
    );
    assert_eq!(
        code(&set(&br(&["destination_white"]), json!([255.0, 256.0]))),
        Code::InvalidField
    );
    assert_eq!(
        code(&set(
            &at(&["advanced_blending", "blend_ranges", "1", "channel"]),
            json!("gray")
        )),
        Code::InvalidField
    );
    // Effects: opacity percent, layer-applicable blend mode, unique instance ids, SEFX id,
    // bevel altitude 0..90, a gradient stroke needs its gradient reference.
    let e0 = |rest: &[&'static str]| at(&[&["effects", "effects", "0"][..], rest].concat());
    assert_eq!(
        code(&set(&e0(&["opacity"]), json!(101.0))),
        Code::InvalidField
    );
    assert_eq!(
        code(&set(&e0(&["blend_mode"]), json!(24))),
        Code::InapplicableBlendMode
    );
    assert_eq!(
        code(&set(&e0(&["blend_mode"]), json!(1))),
        Code::InapplicableBlendMode
    );
    assert_eq!(
        code(&set(&e0(&["blend_mode"]), json!(36))),
        Code::UnknownBlendMode
    );
    assert_eq!(
        code(&set(&e0(&["instance_id"]), json!("ds2"))),
        Code::InvalidField
    );
    assert_eq!(
        code(&set(
            &at(&["effects", "effect_stack_id"]),
            json!(identity("SLYR", 9))
        )),
        Code::InvalidId
    );
    assert_eq!(
        code(&set(
            &at(&["effects", "effects", "4", "params", "altitude"]),
            json!(91.0)
        )),
        Code::InvalidField
    );
    assert_eq!(
        code(&set(
            &at(&["effects", "effects", "2", "params", "fill_type"]),
            json!("gradient")
        )),
        Code::InvalidField
    );
    // STU-RAS-151 adjustments: posterize 2..255, levels input black < white, curves need >= 2
    // points with strictly increasing input.
    let adj = ["layers", "1", "adjustment"];
    assert_eq!(
        code(&set(&adj, json!({"kind":"posterize","levels":1}))),
        Code::InvalidField
    );
    assert_eq!(
        code(&set(
            &adj,
            json!({"kind":"levels","records":[{"channel":"composite","input_black":0.6,
            "input_white":0.4,"output_black":0.0,"output_white":1.0,"gamma":1.0}]})
        )),
        Code::InvalidField
    );
    assert_eq!(
        code(&set(
            &adj,
            json!({"kind":"curves","curves":[{"channel":"composite",
            "points":[{"input":10.0,"output":0.0,"corner":false}]}]})
        )),
        Code::InvalidField
    );
    assert_eq!(
        validate(&set(
            &adj,
            json!({"kind":"hue_saturation","master":{"hue":60.0,"saturation":-50.0,"lightness":0.0},
            "colorize":false,"colorization":{"hue":0.0,"saturation":0.0,"lightness":0.0}})
        )),
        Ok(vec![])
    );
    assert_eq!(
        code(&set(&["global_light", "altitude"], json!(90.5))),
        Code::InvalidField
    );
}

#[test]
fn adjustment_linkage_and_preconditions() {
    // Adjustment record on a raster layer: payload mismatch.
    let mut v = records();
    v["layers"][1]["layer_id"] = json!(identity("SLYR", 3));
    assert_eq!(code(&v), Code::PayloadMismatch);
    // The adjustment layer without its record: dangling.
    let mut v = records();
    v["layers"].as_array_mut().unwrap().remove(1);
    assert_eq!(code(&v), Code::DanglingReference);
    // A record for a layer the document does not have: dangling.
    let mut v = records();
    v["layers"][0]["layer_id"] = json!(identity("SLYR", 99));
    assert_eq!(code(&v), Code::DanglingReference);
    // Two entries for one layer.
    let mut v = records();
    let dup = v["layers"][1].clone();
    v["layers"].as_array_mut().unwrap().push(dup);
    assert_eq!(code(&v), Code::DuplicateIdentity);
    // Mask parameters on a layer without a mask (layer 3).
    let mut v = records();
    v["layers"]
        .as_array_mut()
        .unwrap()
        .push(json!({"layer_id":identity("SLYR",3),
        "mask":{"density":100.0,"feather":0.0,"inverted":false}}));
    assert_eq!(code(&v), Code::InvalidAppearance);
    // Records of another document.
    let mut v = records();
    v["document_id"] = json!(identity("SDOC", 2));
    assert_eq!(code(&v), Code::InvalidField);
}
