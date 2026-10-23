//! Lane SMB-PS-ADJUST-STYLES slice 1b: the second STU-RAS-151 adjustment set as Folio records
//! (validation, round trip, hostile tables) and their lowering to pigment ops (unit conversions and
//! typed refusals of `hsk_studio_composite::lower_extensions`). Pixel proof of the ops:
//! render-cpu `tests/cpu_adjust_colour.rs`; formula proof: pigment `tests/adjust_colour.rs`.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::BlendSpace;
use hsk_studio_composite::lower_extensions::{
    REFUSE_ADJUSTMENT_COLOUR_LINEAR, REFUSE_ADJUSTMENT_COLOUR_PROFILE, REFUSE_ADJUSTMENT_DITHER,
    REFUSE_CHANNEL_MIXER_SHAPE, REFUSE_COLOUR_LOOKUP, REFUSE_GRADIENT_MAP_STOP,
    REFUSE_TONE_COMPRESSION, adjustment_op,
};
use hsk_studio_folio::*;
use hsk_studio_pigment::adjust_math as am;
use hsk_studio_pigment::adjust_math::AdjustmentOp;
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
/// One adjustment layer (2) hosting its own adjustment.
fn document() -> StudioDocument {
    serde_json::from_value(json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"px",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",2),"name":"a","parent":null,
            "kind":"adjustment","visible":true,"payload":{"payload_kind":"primitive_reference",
            "schema_id":"hsk.studio.adjustment@1","object_id":identity("SLYR",2)}}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    }))
    .expect("document shape")
}
fn validate(adjustment: &Value) -> Result<StudioAdjustment, Code> {
    let v = json!({"schema_id":"hsk.studio.raster_records@1","document_id":identity("SDOC",1),
        "layers":[{"layer_id":identity("SLYR",2),"adjustment":adjustment}]});
    let r: StudioRasterRecords = serde_json::from_value(v).map_err(|_| Code::InvalidField)?;
    validate_raster_records(&r, &document(), Budget::default(), &CancellationToken::default(), &Caller)
        .map_err(|d| d.code)?;
    let again: StudioRasterRecords =
        serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
    assert_eq!(again, r, "round trip");
    Ok(r.layers[0].adjustment.clone().unwrap())
}
fn colour(r: f64, g: f64, b: f64) -> Value {
    json!({"r":r,"g":g,"b":b,"colour_profile_id":null})
}
fn stop(position: f64, c: Value) -> Value {
    json!({"position":position,"midpoint":null,"opacity":100.0,"colour":c})
}
fn lowered(v: &Value) -> AdjustmentOp {
    adjustment_op(&validate(v).unwrap(), BlendSpace::Encoded).unwrap()
}

#[test]
fn second_set_records_validate_round_trip_and_lower() {
    // Percent stays percent, gradient stop positions percent -> fraction, black & white tint only
    // when enabled, mixer rows by output, LUT cube carried by value.
    assert_eq!(
        lowered(&json!({"kind":"vibrance","vibrance":40.0,"saturation":-10.0})),
        AdjustmentOp::Vibrance(am::Vibrance { vibrance: 40.0, saturation: -10.0 })
    );
    assert_eq!(
        lowered(&json!({"kind":"colour_balance","shadows":[0.0,0.0,0.0],"midtones":[50.0,0.0,-20.0],
            "highlights":[0.0,10.0,0.0],"preserve_luminosity":true})),
        AdjustmentOp::ColourBalance(am::ColourBalance {
            shadows: [0.0; 3],
            midtones: [50.0, 0.0, -20.0],
            highlights: [0.0, 10.0, 0.0],
            preserve_luminosity: true
        })
    );
    assert_eq!(
        lowered(&json!({"kind":"photo_filter","filter_colour":colour(1.0,0.5,0.0),"density":25.0,
            "preserve_luminosity":false})),
        AdjustmentOp::PhotoFilter(am::PhotoFilter {
            colour: [1.0, 0.5, 0.0],
            density: 25.0,
            preserve_luminosity: false
        })
    );
    let bw = |tint: bool| {
        json!({"kind":"black_and_white","red":40.0,"yellow":60.0,"green":40.0,"cyan":60.0,"blue":20.0,
            "magenta":80.0,"tint_enabled":tint,"tint_hue":35.0,"tint_saturation":25.0})
    };
    assert_eq!(
        lowered(&bw(true)),
        AdjustmentOp::BlackAndWhite(am::BlackAndWhite {
            weights: [40.0, 60.0, 40.0, 60.0, 20.0, 80.0],
            tint: Some([35.0, 25.0])
        })
    );
    assert!(matches!(lowered(&bw(false)), AdjustmentOp::BlackAndWhite(b) if b.tint.is_none()));
    let row = |w: [f64; 3], k: f64| json!({"weights":w,"constant":k});
    let mixer = |rows: Vec<Value>| json!({"kind":"channel_mixer","outputs":rows,"monochrome":false});
    assert_eq!(
        lowered(&mixer(vec![
            row([100.0, 50.0, 0.0], 10.0),
            row([0.0, 100.0, 0.0], 0.0),
            row([0.0, 0.0, 100.0], 0.0)
        ])),
        AdjustmentOp::ChannelMixer(am::ChannelMixer {
            red: [100.0, 50.0, 0.0, 10.0],
            green: [0.0, 100.0, 0.0, 0.0],
            blue: [0.0, 0.0, 100.0, 0.0],
            monochrome: false
        })
    );
    let q = |c: f64| json!([c, 0.0, 0.0, 0.0]);
    let sel = json!({"kind":"selective_colour","method":"absolute","reds":q(-25.0),"yellows":q(0.0),
        "greens":q(0.0),"cyans":q(0.0),"blues":q(0.0),"magentas":q(0.0),"whites":q(0.0),"neutrals":q(0.0),
        "blacks":q(5.0)});
    let AdjustmentOp::SelectiveColour(s) = lowered(&sel) else { panic!("selective") };
    assert_eq!((s.method, s.ranges[0][0], s.ranges[8][0]), (am::SelectiveColourMethod::Absolute, -25.0, 5.0));
    let gm = json!({"kind":"gradient_map","stops":[stop(0.0,colour(0.0,0.0,0.0)),stop(25.0,colour(1.0,0.0,0.0))],
        "reverse":true,"dither":false});
    assert_eq!(
        lowered(&gm),
        AdjustmentOp::GradientMap(am::GradientMap {
            stops: vec![
                am::GradientMapStop { position: 0.0, colour: [0.0; 3] },
                am::GradientMapStop { position: 0.25, colour: [1.0, 0.0, 0.0] }
            ],
            reverse: true
        })
    );
    let cube: Vec<Value> = (0..8).map(|i| json!([f64::from(i & 1), f64::from((i >> 1) & 1), f64::from(i >> 2)])).collect();
    let lut = json!({"kind":"colour_lookup","lut":"luts/identity.cube","lut_kind":"3dlut","dither":false,
        "table":{"side":2,"values":cube}});
    let AdjustmentOp::ColourLookup(l) = lowered(&lut) else { panic!("lookup") };
    assert_eq!((l.side, l.table.len(), l.table[5]), (2, 8, [1.0, 0.0, 1.0]));
    let sh = json!({"kind":"shadows_highlights","shadow_amount":35.0,"shadow_tone_width":50.0,"shadow_radius":0.0,
        "highlight_amount":0.0,"highlight_tone_width":50.0,"highlight_radius":0.0,"colour_correction":20.0,
        "midtone_contrast":0.0,"black_clip":0.01,"white_clip":0.01});
    assert!(matches!(lowered(&sh), AdjustmentOp::ShadowsHighlights(p) if p.shadow_amount == 35.0 && p.colour_correction == 20.0));
    let tc = |compression: f64| {
        json!({"kind":"tone_compression","method":"exposure_gamma","exposure":1.0,"gamma":2.0,"compression":compression})
    };
    assert_eq!(
        lowered(&tc(0.0)),
        AdjustmentOp::ToneCompression(am::ToneCompression {
            method: am::ToneCompressionMethod::ExposureGamma,
            exposure: 1.0,
            gamma: 2.0
        })
    );
    let mc = json!({"kind":"match_colour","luminance":100.0,"colour_intensity":120.0,"fade":10.0,"neutralize":true,
        "source":{"source_mean":[0.6,0.5,0.4],"source_std":[0.2,0.2,0.2],"target_mean":[0.4,0.4,0.4],
        "target_std":[0.1,0.1,0.1]}});
    assert!(matches!(lowered(&mc), AdjustmentOp::MatchColour(m) if m.neutralize && m.colour_intensity == 120.0 && m.source_mean == [0.6, 0.5, 0.4]));
    let rc = json!({"kind":"replace_colour","fuzziness":40.0,"samples":[colour(1.0,0.0,0.0)],
        "shift":{"hue":120.0,"saturation":0.0,"lightness":0.0}});
    assert_eq!(
        lowered(&rc),
        AdjustmentOp::ReplaceColour(am::ReplaceColour {
            samples: vec![[1.0, 0.0, 0.0]],
            fuzziness: 40.0,
            hue: 120.0,
            saturation: 0.0,
            lightness: 0.0
        })
    );
    // Every lowered op passes pigment's own parameter check.
    for v in [&sel, &gm, &lut, &sh, &mc, &rc] {
        lowered(v).prepare().unwrap();
    }

    // Hostile records refused by Folio with typed codes: a cube side of 65535 before any value is
    // walked, a cube with the wrong value count, a NaN-free but out-of-range value, a gradient
    // map with one stop or decreasing positions, fuzziness above 200, an unknown method token.
    let bad_lut = |side: u64, values: Value| {
        json!({"kind":"colour_lookup","lut":"x","lut_kind":"3dlut","dither":false,"table":{"side":side,"values":values}})
    };
    assert_eq!(validate(&bad_lut(65535, json!([]))), Err(Code::InvalidField));
    assert_eq!(validate(&bad_lut(2, json!([[0.0, 0.0, 0.0]]))), Err(Code::InvalidField));
    let mut wild = cube.clone();
    wild[3] = json!([0.0, 2.0, 0.0]);
    assert_eq!(validate(&bad_lut(2, json!(wild))), Err(Code::InvalidField));
    assert_eq!(
        validate(&json!({"kind":"gradient_map","stops":[stop(0.0,colour(0.0,0.0,0.0))],"reverse":false,"dither":false})),
        Err(Code::InvalidField)
    );
    assert_eq!(
        validate(&json!({"kind":"gradient_map","stops":[stop(60.0,colour(0.0,0.0,0.0)),stop(40.0,colour(1.0,1.0,1.0))],
            "reverse":false,"dither":false})),
        Err(Code::InvalidField)
    );
    let mut fuzz = rc.clone();
    fuzz["fuzziness"] = json!(200.5);
    assert_eq!(validate(&fuzz), Err(Code::InvalidField));
    let mut method = sel.clone();
    method["method"] = json!("proportional");
    assert_eq!(validate(&method), Err(Code::InvalidField));
}

#[test]
fn lowering_refusals_are_typed() {
    let refused = |v: Value, space: BlendSpace| adjustment_op(&validate(&v).unwrap(), space).unwrap_err();
    let row = json!({"weights":[100.0,0.0,0.0],"constant":0.0});
    // A CMYK-shaped mixer (four outputs) has no RGB pigment op.
    assert_eq!(
        refused(json!({"kind":"channel_mixer","outputs":[row,row,row,row],"monochrome":false}), BlendSpace::Encoded),
        REFUSE_CHANNEL_MIXER_SHAPE
    );
    let gm = |midpoint: Value, dither: bool| {
        json!({"kind":"gradient_map","stops":[{"position":0.0,"midpoint":midpoint,"opacity":100.0,
            "colour":colour(0.0,0.0,0.0)},stop(100.0,colour(1.0,1.0,1.0))],"reverse":false,"dither":dither})
    };
    assert_eq!(refused(gm(json!(30.0), false), BlendSpace::Encoded), REFUSE_GRADIENT_MAP_STOP);
    assert_eq!(refused(gm(Value::Null, true), BlendSpace::Encoded), REFUSE_ADJUSTMENT_DITHER);
    // Midpoint 50 is the linear interpolation: accepted.
    assert!(adjustment_op(&validate(&gm(json!(50.0), false)).unwrap(), BlendSpace::Encoded).is_ok());
    // A colour lookup without its resolved cube, or an abstract profile, is refused.
    assert_eq!(
        refused(json!({"kind":"colour_lookup","lut":"luts/a.cube","lut_kind":"3dlut","dither":false}), BlendSpace::Encoded),
        REFUSE_COLOUR_LOOKUP
    );
    assert_eq!(
        refused(json!({"kind":"colour_lookup","lut":"luts/a.icc","lut_kind":"abstract","dither":false}), BlendSpace::Encoded),
        REFUSE_COLOUR_LOOKUP
    );
    assert_eq!(
        refused(json!({"kind":"tone_compression","method":"highlight_compression","exposure":0.0,"gamma":1.0,
            "compression":0.5}), BlendSpace::Encoded),
        REFUSE_TONE_COMPRESSION
    );
    // Colours: an assigned profile is refused (composite converts no colour); in a linear-light
    // plan only black and white are the same in every encoding.
    let pf = |c: Value| json!({"kind":"photo_filter","filter_colour":c,"density":25.0,"preserve_luminosity":true});
    let mut assigned = colour(1.0, 0.5, 0.0);
    assigned["colour_profile_id"] = json!(identity("SCPF", 1));
    assert_eq!(refused(pf(assigned), BlendSpace::Encoded), REFUSE_ADJUSTMENT_COLOUR_PROFILE);
    assert_eq!(refused(pf(colour(1.0, 0.5, 0.0)), BlendSpace::LinearLight), REFUSE_ADJUSTMENT_COLOUR_LINEAR);
    assert!(adjustment_op(&validate(&pf(colour(1.0, 0.0, 0.0))).unwrap(), BlendSpace::LinearLight).is_ok());
}
