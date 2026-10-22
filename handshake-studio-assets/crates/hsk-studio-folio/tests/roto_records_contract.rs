//! Lane SMB-AE-ROTO-WIRING: `StudioCompositionLayer::time_remap`, `frame_blending` and `roto`
//! (STU-MOT-045, STU-MOT-007a, STU-CMP-034). Records without them serialise byte for byte as
//! before; with them they round trip byte-identically; every refusal is typed; unknown-bounded
//! numerics are stored as given (STU-RAS-106); strokes, spans and freeze follow STU-CMP-034 items 1,
//! 2 and 6.
use hsk_studio_accord::{CancellationToken, TICKS_PER_SECOND};
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
fn check(v: &Value) -> Result<Vec<ResolutionIssue>, Diagnostic> {
    let r: StudioCompositionRecords = serde_json::from_value(v.clone()).expect("record shape");
    validate_composition_records(&r, &document(), Budget::default(), &CancellationToken::default(), &Caller)
}
fn refusal(v: &Value) -> String {
    let e = check(v).unwrap_err();
    format!("{:?}:{}", e.code, e.target)
}
fn roto() -> Value {
    json!({
        "strokes":[
            {"frame":3,"polarity":"foreground","diameter":4.0,"path":[[10.0,12.0],[14.5,12.25]]},
            {"frame":3,"polarity":"background","diameter":8.0,"path":[[1.0,1.0]]},
            {"frame":5,"polarity":"background","diameter":2.0,"path":[[20.0,20.0],[21.0,22.0]]}],
        "edge_strokes":[{"frame":3,"diameter":3.0,"path":[[9.0,9.0],[15.0,9.0]]}],
        "spans":[{"base_frame":3,"in_frame":0,"out_frame":24,"backward":false}],
        "parameters":{"quality":"best","search_radius":-5.0,"motion_threshold":1.0e300,
            "edge_detection":"favor_current","fine_tune_matte":true,"matte_reduce_chatter":150.0,
            "matte_shift_edge":-40.0,"refine":{"feather":0.5,"decontaminate_colours":true},
            "refine_reduce_chatter":-1.0,"view_mode":"composite"}})
}
fn full() -> Value {
    let mut l = layer();
    l["time_remap"] = json!({"static_value":[0.0],"keyframes":[
        {"time":0,"value":[0.0],"in_interp":"linear","out_interp":"linear"},
        {"time":21168000000u64,"value":[-3.5],"in_interp":"hold","out_interp":"hold"}]});
    l["frame_blending"] = json!("pixel_motion");
    l["roto"] = roto();
    l
}

#[test]
fn records_are_serde_default_round_trip_and_keep_unknown_bounded_values() {
    // Without the fields: absent on the wire, bytes unchanged.
    let plain = records(layer());
    let r: StudioCompositionRecords = serde_json::from_value(plain.clone()).unwrap();
    let l = &r.compositions[0].layers[0];
    assert!(l.time_remap.is_none() && l.frame_blending == StudioFrameBlending::Off && l.roto.is_none());
    let bytes = serde_json::to_vec(&r).unwrap();
    for name in ["time_remap", "frame_blending", "roto"] {
        assert!(!String::from_utf8_lossy(&bytes).contains(name), "{name}");
    }
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), plain);
    check(&plain).unwrap();
    // With them: valid, byte-identical round trip, values as given (never clamped).
    let with = records(full());
    check(&with).unwrap();
    let r: StudioCompositionRecords = serde_json::from_value(with.clone()).unwrap();
    let first = serde_json::to_vec(&r).unwrap();
    let back: StudioCompositionRecords = serde_json::from_slice(&first).unwrap();
    assert_eq!(back, r);
    assert_eq!(serde_json::to_vec(&back).unwrap(), first);
    assert_eq!(serde_json::from_slice::<Value>(&first).unwrap(), with);
    let l = &r.compositions[0].layers[0];
    assert_eq!(l.frame_blending, StudioFrameBlending::PixelMotion);
    let ro = l.roto.as_deref().unwrap();
    assert_eq!(ro.engine, StudioRotoEngine::Classic);
    let p = &ro.parameters;
    assert_eq!((p.search_radius, p.motion_threshold, p.matte_reduce_chatter), (Some(-5.0), Some(1.0e300), Some(150.0)));
    assert_eq!(p.refine_reduce_chatter, Some(-1.0));
    assert_eq!((p.quality, p.edge_detection, p.view_mode), (StudioRotoQuality::Best, StudioRotoEdgeDetection::FavorCurrent, StudioRotoViewMode::Composite));
    assert!(p.refine.decontaminate_colours && p.refine.decontamination_amount.is_none());
    assert!(ro.spans[0].forward && !ro.spans[0].backward);
    // Every member of the parameter table defaults; an empty table is `{}`.
    let empty: StudioRotoParameters = serde_json::from_value(json!({})).unwrap();
    assert_eq!(serde_json::to_value(&empty).unwrap(), json!({}));
    assert_eq!(empty.view_mode, StudioRotoViewMode::AlphaBoundary);
    assert_eq!(empty.numerics().len(), 15);
    assert!(empty.numerics().iter().all(|(_, v)| v.is_none()), "unknown defaults are absent, never invented");
    assert!(composition_records_schema().as_value().is_object());
}

#[test]
fn hostile_records_are_refused_typed() {
    let mutate = |f: &dyn Fn(&mut Value)| {
        let mut l = full();
        f(&mut l);
        refusal(&records(l))
    };
    let target = |s: &str| format!("InvalidField:{}{s}", identity("SLYR", 1));
    assert_eq!(mutate(&|l| l["time_remap"]["static_value"] = json!([0.0, 1.0])), target("/time_remap/static_value"));
    assert_eq!(mutate(&|l| l["time_remap"]["keyframes"][1]["time"] = json!(0)), target("/time_remap/keyframes/time"));
    assert_eq!(mutate(&|l| l["time_remap"]["keyframes"][1]["value"] = json!([2.0e12])), target("/time_remap/keyframes/value"));
    assert_eq!(mutate(&|l| l["roto"]["strokes"][0]["diameter"] = json!(0.0)), target("/roto/strokes"));
    assert_eq!(mutate(&|l| l["roto"]["strokes"][0]["path"] = json!([])), target("/roto/strokes/path"));
    assert_eq!(mutate(&|l| l["roto"]["strokes"][0]["path"][0][0] = json!(2.0e12)), target("/roto/strokes/path"));
    assert_eq!(mutate(&|l| l["roto"]["strokes"][2]["frame"] = json!(30)), target("/roto/strokes/span"));
    assert_eq!(mutate(&|l| l["roto"]["edge_strokes"][0]["frame"] = json!(40)), target("/roto/edge_strokes/span"));
    assert_eq!(mutate(&|l| l["roto"]["spans"][0]["base_frame"] = json!(4)), target("/roto/spans/base_frame"));
    assert_eq!(mutate(&|l| l["roto"]["spans"][0]["in_frame"] = json!(24)), target("/roto/spans"));
    assert_eq!(mutate(&|l| { l["roto"]["spans"][0]["forward"] = json!(false); }), target("/roto/spans"));
    assert_eq!(
        mutate(&|l| l["roto"]["spans"] = json!([{"base_frame":3,"in_frame":0,"out_frame":10},{"base_frame":5,"in_frame":5,"out_frame":24}])),
        target("/roto/spans/overlap")
    );
    assert_eq!(
        mutate(&|l| l["roto"]["freeze"] = json!({"stroke_count":9,"spans":[{"base_frame":3,"in_frame":0,"out_frame":24}]})),
        target("/roto/freeze/stroke_count")
    );
    assert_eq!(
        mutate(&|l| l["roto"]["freeze"] = json!({"stroke_count":1,"spans":[{"base_frame":5,"in_frame":0,"out_frame":24}]})),
        target("/roto/freeze/spans/base_frame")
    );
    // Too many path points: a budget refusal, before anything is kept.
    let long: Vec<[f64; 2]> = (0..=MAX_ROTO_STROKE_POINTS).map(|i| [i as f64, 1.0]).collect();
    assert!(mutate(&|l| l["roto"]["strokes"][0]["path"] = json!(long)).starts_with("BudgetExceeded:"));
    // Closed sets and unknown fields refuse at the wire.
    let mut l = full();
    l["frame_blending"] = json!("blend");
    assert!(serde_json::from_value::<StudioCompositionRecords>(records(l)).is_err());
    let mut l = full();
    l["roto"]["engine"] = json!("v3");
    assert!(serde_json::from_value::<StudioCompositionRecords>(records(l)).is_err());
    let mut l = full();
    l["roto"]["parameters"]["matte_choke"] = json!(1.0);
    assert!(serde_json::from_value::<StudioCompositionRecords>(records(l)).is_err());
    // A programmatic non-finite parameter (the wire cannot carry one) is refused.
    let mut r: StudioCompositionRecords = serde_json::from_value(records(full())).unwrap();
    r.compositions[0].layers[0].roto.as_mut().unwrap().parameters.matte_feather = Some(f64::NAN);
    let e = validate_composition_records(&r, &document(), Budget::default(), &CancellationToken::default(), &Caller).unwrap_err();
    assert_eq!(format!("{:?}:{}", e.code, e.target), format!("InvalidField:{}/roto/parameters/matte_feather", identity("SLYR", 1)));
}

#[test]
fn strokes_open_spans_and_freeze_snapshots_the_propagation_inputs() {
    let stroke = |frame: u64, polarity: StudioRotoPolarity| StudioRotoStroke { frame, polarity, diameter: 3.0, path: vec![[1.0, 2.0]] };
    let mut r = StudioAssistedRoto::default();
    // First stroke: base frame, a span over the free run of the source.
    assert_eq!(r.add_stroke(stroke(10, StudioRotoPolarity::Foreground), 48).unwrap(), StudioRotoStrokeAdded::Opened(0));
    assert_eq!(r.spans[0], StudioRotoSpan { base_frame: 10, in_frame: 0, out_frame: 48, forward: true, backward: true });
    // A stroke inside the span is added (never replaces one).
    assert_eq!(r.add_stroke(stroke(20, StudioRotoPolarity::Background), 48).unwrap(), StudioRotoStrokeAdded::Span(0));
    assert_eq!(r.strokes.len(), 2);
    // Resize the span; a stroke outside it opens a span over the free run that abuts it.
    r.spans[0].out_frame = 30;
    assert_eq!(r.add_stroke(stroke(40, StudioRotoPolarity::Foreground), 48).unwrap(), StudioRotoStrokeAdded::Opened(1));
    assert_eq!((r.spans[1].in_frame, r.spans[1].out_frame, r.spans[1].base_frame), (30, 48, 40));
    assert!(r.add_stroke(stroke(48, StudioRotoPolarity::Foreground), 48).is_err(), "beyond the source");
    assert_eq!(r.strokes.len(), 3, "a refused stroke changes nothing");
    // Freeze: the propagation inputs are the snapshot; later strokes and controls do not count.
    let live_key = r.propagation_inputs().canonical_bytes();
    r.freeze().unwrap();
    r.add_stroke(stroke(12, StudioRotoPolarity::Background), 48).unwrap();
    r.parameters.search_radius = Some(9.0);
    r.parameters.matte_feather = Some(2.0);
    let frozen = r.propagation_inputs();
    assert!(frozen.frozen && frozen.strokes.len() == 3);
    assert_eq!(frozen.canonical_bytes(), live_key);
    r.unfreeze();
    let live = r.propagation_inputs();
    assert!(!live.frozen && live.strokes.len() == 4);
    assert_ne!(live.canonical_bytes(), live_key);
    // The matte controls are not propagation inputs.
    let before = r.propagation_inputs().canonical_bytes();
    r.parameters.matte_feather = Some(7.0);
    assert_eq!(r.propagation_inputs().canonical_bytes(), before);
    // Removing a span removes its strokes; the record stays valid in a layer.
    r.remove_span(0).unwrap();
    assert!(r.strokes.iter().all(|s| s.frame >= 30) && r.spans.len() == 1);
    assert!(r.remove_span(5).is_err());
    let mut l = layer();
    l["roto"] = serde_json::to_value(&r).unwrap();
    check(&records(l)).unwrap();
}

#[test]
fn enabling_time_remap_builds_the_identity_keys() {
    let frame = 10_584_000_000u64;
    let v = time_remap_enabled(frame * 2, frame * 47).unwrap();
    assert_eq!(v.keyframes.len(), 2);
    assert_eq!((v.keyframes[0].time, v.keyframes[0].value[0]), (frame * 2, 0.0));
    assert_eq!(v.keyframes[1].time, frame * 49);
    assert_eq!(v.keyframes[1].value[0], (frame * 47) as f64 / TICKS_PER_SECOND as f64);
    assert_eq!(time_remap_enabled(5, 0).unwrap().keyframes.len(), 1, "a one-frame source has one key");
    assert!(time_remap_enabled(i64::MAX as u64, 2).is_none(), "tick overflow");
    let mut l = layer();
    l["time_remap"] = serde_json::to_value(&v).unwrap();
    check(&records(l)).unwrap();
}
