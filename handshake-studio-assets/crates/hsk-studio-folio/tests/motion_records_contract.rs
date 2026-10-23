//! SMB-AE-MOTION-RECORDS: `hsk.studio.motion_records@1` (shape layers, text animators, composition
//! audio, render queue). Wire round trip byte for byte, an empty set is two fields, and each refusal
//! code derived from the rule named next to it.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use serde::{Serialize, de::DeserializeOwned};
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
fn wire_identical<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(v: &T) -> T {
    let first = serde_json::to_vec(v).expect("encode");
    let back: T = serde_json::from_slice(&first).expect("decode");
    assert_eq!(&back, v);
    assert_eq!(serde_json::to_vec(&back).expect("encode again"), first);
    back
}
fn document() -> StudioDocument {
    let layers: Vec<Value> = (1..=3)
        .map(|n| {
            json!({"schema_id":"hsk.studio.layer@1","layer_id":slyr(n),"name":format!("l{n}"),
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
fn compositions() -> StudioCompositionRecords {
    let layer = |n: u32| json!({"layer_id":slyr(n),
        "timing":{"in_point":0,"out_point":254016000000u64,"start_time":0,"stretch":{"numerator":1,"denominator":1}}});
    serde_json::from_value(json!({"schema_id":"hsk.studio.composition_records@1","document_id":identity("SDOC",1),
        "compositions":[{"schema_id":"hsk.studio.composition@1","composition_id":identity("SCOM",9),"name":"c","width":64,"height":32,
            "pixel_aspect":{"numerator":1,"denominator":1},"frame_rate":{"numerator":25,"denominator":1},
            "duration":254016000000u64,"layers":[layer(1),layer(2),layer(3)]}]}))
    .expect("composition shape")
}

fn full() -> Value {
    json!({"schema_id":"hsk.studio.motion_records@1","document_id":identity("SDOC",1),
        "shape_layers":[{"layer_id":slyr(1),
            "root":{"items":[
                {"kind":"rect","position":[8.0,8.0],"size":[16.0,16.0]},
                {"kind":"gradient_fill","opacity":100.0,"rule":"non_zero",
                    "gradient":{"radial":false,"start":[0.0,0.0],"end":[16.0,0.0],"stops":[{"offset":0.0,"rgba":[1.0,0.0,0.0,1.0]},{"offset":1.0,"rgba":[0.0,0.0,1.0,1.0]}]}},
                {"kind":"stroke","rgba":[0.0,0.0,0.0,1.0],"opacity":100.0,"style":{"width":2.0,"cap":"butt","join":"miter","miter_limit":4.0}},
                {"kind":"trim","start":0.0,"end":100.0,"mode":"simultaneously"},
                {"kind":"group","group":{"items":[{"kind":"ellipse","position":[0.0,0.0],"size":[4.0,4.0]},{"kind":"fill","rgba":[1.0,1.0,1.0,1.0],"opacity":50.0,"rule":"even_odd"}],
                    "transform":{"anchor":[0.0,0.0],"position":[20.0,4.0],"scale":[100.0,100.0],"rotation":0.0,"opacity":100.0}}},
                {"kind":"repeater","copies":3.0,"anchor":[0.0,0.0],"position":[20.0,0.0],"scale":[100.0,100.0],"rotation":0.0,"start_opacity":100.0,"end_opacity":50.0,"above":true}]},
            "streams":[{"path":[3],"field":"trim_end","value":{"static_value":[100.0],"keyframes":[
                {"time":0,"value":[0.0],"in_interp":"linear","out_interp":"linear"},
                {"time":254016000000u64,"value":[100.0],"in_interp":"linear","out_interp":"linear"}]}},
                {"path":[4],"field":"group_rotation","value":{"static_value":[0.0]}}]}],
        "text_animators":[{"layer_id":slyr(2),"animators":[{"name":"wave","properties":{"position":[0.0,-10.0],"opacity":0.0},
            "selectors":[{"kind":"range","start":0.0,"end":20.0,"index_units":false,"based_on":"characters","shape":"square","smoothness":100.0,"amount":100.0},
                {"kind":"expression","based_on":"words","source":"textIndex / textTotal * 100"}]}],
            "streams":[{"animator":0,"selector":0,"param":"range_offset","value":{"static_value":[0.0]}}]}],
        "composition_audio":[{"composition_id":identity("SCOM",9),"layers":[{"audio_id":"music-01","start_tick":101606400000u64,"gain":0.5}]}],
        "render_queue":[{"item_id":"master","composition_id":identity("SCOM",9),
            "settings":{"quality":"best","resolution":"full","disk_cache":"read_only","proxy_use":"use_no_proxies","effects":"current_settings",
                "solo_switches":"current_settings","guide_layers":"all_off","color_depth":"8_bits_per_channel","frame_blending":"current_settings",
                "field_render":"off","pulldown_3_2":"WSSWW","motion_blur":"off_for_all_layers","time_span":{"custom":{"start_tick":0,"end_tick":101606400000u64}}},
            "output_modules":[{"module_id":"mezzanine","format":"mov_vc3","codec_option":"dnxhr_hq","include_project_link":false,"post_render_action":"none",
                "include_source_metadata":false,"channels":"rgb","depth":8,"color":"straight","starting_number":0,"use_comp_frame_number":false,
                "audio":{"output":"auto","sample_rate":48000,"bit_depth":32,"float":true,"channels":2},
                "colour":{"preserve_rgb":false,"embed_color_space":true},"output_path":"renders/master.mov"}]}]})
}

fn check(v: &Value) -> Result<(), Diagnostic> {
    let r: StudioMotionRecords = serde_json::from_value(v.clone()).expect("record shape");
    validate_motion_records(&r, &document(), &compositions(), Budget::default(), &CancellationToken::default(), &Caller)
}

#[test]
fn motion_records_round_trip_and_validate() {
    let r: StudioMotionRecords = serde_json::from_value(full()).expect("record shape");
    wire_identical(&r);
    check(&full()).expect("valid");
    assert_eq!(r.shape_layer(&slyr(1)).map(|s| s.root.items.len()), Some(6));
    assert_eq!(
        shape_item_at(&r.shape_layers[0].root, &[4, 1]),
        Some(&StudioShapeItem::Fill { rgba: [1.0; 4], opacity: 50.0, rule: StudioShapeFillRule::EvenOdd })
    );
    assert_eq!(r.render_queue[0].settings.pulldown_3_2, StudioPulldown::Wssww);
    assert_eq!(r.render_queue[0].settings.color_depth, StudioColorDepth::Bits8);
    // An empty set is exactly its two header fields; the composition records stay what they were.
    let empty = StudioMotionRecords::new(identity("SDOC", 1));
    assert_eq!(
        serde_json::to_string(&empty).unwrap(),
        format!("{{\"schema_id\":\"hsk.studio.motion_records@1\",\"document_id\":\"{}\"}}", identity("SDOC", 1))
    );
    let comps = compositions();
    assert!(!serde_json::to_string(&wire_identical(&comps)).unwrap().contains("motion"));
    let schema = serde_json::to_string(&motion_records_schema()).unwrap();
    assert!(schema.contains("gradient_stroke") && schema.contains("render_queue"));
}

#[test]
fn hostile_or_dangling_motion_records_are_refused() {
    let code = |v: Value| check(&v).expect_err("must be refused").code;
    let mut v = full();
    v["shape_layers"][0]["layer_id"] = json!(slyr(7));
    assert_eq!(code(v), Code::DanglingReference, "a shape layer outside every composition");
    let mut v = full();
    v["shape_layers"][0]["streams"][0]["path"] = json!([0]);
    assert_eq!(code(v), Code::DanglingReference, "trim_end on a rect item");
    let mut v = full();
    v["shape_layers"][0]["streams"][0]["value"]["keyframes"][1]["time"] = json!(0);
    assert_eq!(code(v), Code::InvalidField, "keyframe times must increase");
    let mut v = full();
    v["shape_layers"][0]["root"]["items"][5]["copies"] = json!(1025.0);
    assert_eq!(code(v), Code::InvalidField, "repeater copies above 1024");
    let mut v = full();
    v["shape_layers"][0]["root"]["items"][0]["size"] = json!([1.0e12, 1.0]);
    assert_eq!(code(v), Code::InvalidField, "value above the stored magnitude bound");
    let mut v = full();
    v["shape_layers"][0]["root"]["items"][1]["gradient"]["stops"] = json!([]);
    assert_eq!(code(v), Code::InvalidField, "gradient without stops");
    let mut bomb = json!({"items":[{"kind":"rect","position":[0.0,0.0],"size":[1.0,1.0]}]});
    for _ in 0..40 {
        bomb = json!({"items":[{"kind":"group","group":bomb}]});
    }
    let mut v = full();
    v["shape_layers"][0]["root"] = bomb;
    v["shape_layers"][0]["streams"] = json!([]);
    assert_eq!(code(v), Code::BudgetExceeded, "group nesting bomb");
    let mut v = full();
    v["text_animators"][0]["streams"][0]["selector"] = json!(1);
    assert_eq!(code(v), Code::DanglingReference, "range_offset on an expression selector");
    let mut v = full();
    v["render_queue"][0]["output_modules"][0]["output_path"] = json!("../escape.mov");
    assert_eq!(code(v), Code::InvalidField, "output path leaves the project");
    let mut v = full();
    let item = v["render_queue"][0].clone();
    v["render_queue"].as_array_mut().unwrap().push(item);
    assert_eq!(code(v), Code::DuplicateIdentity, "duplicate render-queue item");
    let mut v = full();
    v["composition_audio"][0]["composition_id"] = json!(identity("SCOM", 8));
    assert_eq!(code(v), Code::DanglingReference, "audio for an unknown composition");
    let mut v = full();
    v["render_queue"][0]["settings"]["time_span"] = json!({"custom":{"start_tick":5,"end_tick":5}});
    assert_eq!(code(v), Code::InvalidField, "empty custom time span");
    // Wire names are exact and unknown fields are refused by the decoder.
    let mut v = full();
    v["render_queue"][0]["settings"]["pulldown_3_2"] = json!("wssww");
    assert!(serde_json::from_value::<StudioMotionRecords>(v).is_err());
    let mut v = full();
    v["shape_layers"][0]["root"]["surprise"] = json!(1);
    assert!(serde_json::from_value::<StudioMotionRecords>(v).is_err());
}
