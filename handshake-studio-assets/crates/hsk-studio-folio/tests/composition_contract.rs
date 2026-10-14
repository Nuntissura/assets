//! SMB-FOLIO-COMPOSITION: `hsk.studio.composition@1` records. Byte-identical wire round trip, and
//! every refusal code derived from the rule named next to it (STU-MOT-004/006/008, STU-CMP-030).
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
    let layers: Vec<Value> = (1..=6)
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

fn key_(time: u64, value: Value, interp: &str) -> Value {
    let mut k = json!({"time":time,"value":value,"in_interp":interp,"out_interp":interp});
    if interp == "bezier" {
        let tangent = json!([{"speed":0.0,"influence":0.16666666666}]);
        k["in_tangent"] = tangent.clone();
        k["out_tangent"] = tangent;
    }
    k
}
fn layer(n: u32) -> Value {
    json!({"layer_id":slyr(n),
        "timing":{"in_point":0,"out_point":254016000000u64,"start_time":0,"stretch":{"numerator":1,"denominator":1}}})
}
fn comp(id: &str, layers: Vec<Value>) -> Value {
    json!({"schema_id":"hsk.studio.composition@1","composition_id":id,"name":id,"width":1920,"height":1080,
        "pixel_aspect":{"numerator":1,"denominator":1},"frame_rate":{"numerator":30000,"denominator":1001},
        "duration":2540160000000u64,"layers":layers})
}
fn records(comps: Vec<Value>) -> Value {
    json!({"schema_id":"hsk.studio.composition_records@1","document_id":identity("SDOC",1),"compositions":comps})
}
fn check(v: &Value) -> Result<Vec<ResolutionIssue>, Diagnostic> {
    let r: StudioCompositionRecords = serde_json::from_value(v.clone()).expect("record shape");
    validate_composition_records(
        &r,
        &document(),
        Budget::default(),
        &CancellationToken::default(),
        &Caller,
    )
}
fn code(v: &Value) -> Code {
    check(v).expect_err("must be refused").code
}
type Mutation = Box<dyn Fn(&mut Value)>;
fn with(f: &dyn Fn(&mut Value)) -> Value {
    let mut v = rich();
    f(&mut v);
    v
}

fn rich() -> Value {
    let mut a = layer(1);
    a["timing"]["start_time"] = json!(-127008000000i64);
    a["timing"]["stretch"] = json!({"numerator":-1,"denominator":2});
    a["streams"] = json!([
        {"property":"position","static_value":[10.5,20.25],"keyframes":[
            key_(0, json!([0.1234567890123456, 0.0]), "bezier"),
            key_(254016000000, json!([100.0, 50.0]), "linear")]},
        {"property":"opacity","static_value":[100.0],"keyframes":[
            key_(0, json!([0.0]), "hold"), key_(127008000000, json!([100.0]), "linear")]},
        {"property":"rotation","static_value":[0.0]}
    ]);
    let mut b = layer(2);
    b["parent"] = json!(slyr(1));
    b["track_matte"] = json!({"source_layer_id":slyr(3),"mode":"luma","inverted":true});
    let c = layer(3);
    let mut d = layer(4);
    d["precomp"] = json!({"composition_id":"inner","collapse_transformations":true});
    let mut main = comp("main", vec![a, b, c, d]);
    main["background"] = json!({"r":0.0,"g":0.0,"b":0.0,"a":1.0,"colour_profile_id":null});
    records(vec![main, comp("inner", vec![layer(5)])])
}

/// Round trip is byte-identical (17-digit floats included), lookups work, and the timing algebra
/// is exact: windows are half-open and the source tick is a floor division by the rational stretch.
#[test]
fn composition_round_trips_and_maps_time() {
    let v = rich();
    assert_eq!(check(&v), Ok(vec![]));
    let r: StudioCompositionRecords = serde_json::from_value(v.clone()).unwrap();
    let back = wire_identical(&r);
    assert_eq!(serde_json::to_value(&back).unwrap(), v);
    let (c, l) = back.layer(&slyr(1)).unwrap();
    assert_eq!(c.composition_id, "main");
    assert_eq!(c.frame_rate.ticks_per_frame(), Some(8_475_667_200));
    assert_eq!(l.stream(StudioLayerProperty::Opacity).unwrap().keyframes.len(), 2);
    assert!(l.stream(StudioLayerProperty::Scale).is_none());
    assert!(back.composition("inner").unwrap().layer(&slyr(5)).is_some());
    let t = l.timing;
    assert!(t.contains(0) && t.contains(254015999999) && !t.contains(254016000000));
    // stretch -1/2 (reverse, twice as fast): source = (tick - start) * 2 / -1.
    assert_eq!(t.source_tick(127008000000), Some(-508032000000));
    let odd = StudioLayerTiming {
        stretch: StudioRational { numerator: 2, denominator: 3 },
        start_time: 0,
        ..t
    };
    assert_eq!(odd.source_tick(1), Some(1)); // floor(3/2)
    let neg = StudioLayerTiming {
        stretch: StudioRational { numerator: -2, denominator: 3 },
        ..odd
    };
    assert_eq!(neg.source_tick(1), Some(-2)); // floor(-3/2)
    assert!(composition_records_schema().as_value().is_object());
}

/// Cycles: parent loop (including self), matte loop, and precomposition loops (self and mutual).
#[test]
fn cycle_refusals() {
    let self_parent = with(&|v| v["compositions"][0]["layers"][0]["parent"] = json!(slyr(1)));
    assert_eq!(code(&self_parent), Code::ContainmentCycle);
    let loop_parent = with(&|v| v["compositions"][0]["layers"][0]["parent"] = json!(slyr(2)));
    assert_eq!(code(&loop_parent), Code::ContainmentCycle);
    let self_matte = with(&|v| {
        v["compositions"][0]["layers"][2]["track_matte"] =
            json!({"source_layer_id":slyr(3),"mode":"alpha","inverted":false})
    });
    assert_eq!(code(&self_matte), Code::InvalidField);
    let loop_matte = with(&|v| {
        v["compositions"][0]["layers"][2]["track_matte"] =
            json!({"source_layer_id":slyr(2),"mode":"alpha","inverted":false})
    });
    assert_eq!(code(&loop_matte), Code::RenderCycle);
    let self_precomp = with(&|v| {
        v["compositions"][0]["layers"][3]["precomp"]["composition_id"] = json!("main")
    });
    assert_eq!(code(&self_precomp), Code::RenderCycle);
    let mutual = with(&|v| {
        let mut p = layer(6);
        p["precomp"] = json!({"composition_id":"main","collapse_transformations":false});
        v["compositions"][1]["layers"].as_array_mut().unwrap().push(p);
    });
    assert_eq!(code(&mutual), Code::RenderCycle);
    let dangling = with(&|v| {
        v["compositions"][0]["layers"][3]["precomp"]["composition_id"] = json!("nowhere")
    });
    assert_eq!(code(&dangling), Code::DanglingReference);
}

/// Tick and product overflow, bounds, identity and shape refusals.
#[test]
fn bounds_overflow_and_identity_refusals() {
    let overflow = |v: &Value| {
        let e = check(v).unwrap_err();
        assert_eq!(e.code, Code::InvalidField);
        assert!(e.target.contains("tick_overflow"), "{}", e.target);
    };
    overflow(&with(&|v| v["compositions"][0]["layers"][0]["timing"]["out_point"] = json!(u64::MAX)));
    overflow(&with(&|v| v["compositions"][0]["duration"] = json!(1u64 << 63)));
    overflow(&with(&|v| {
        v["compositions"][0]["layers"][0]["streams"][0]["keyframes"][1]["time"] = json!(u64::MAX)
    }));
    // Source tick beyond i64: start_time -i64::MAX would negate out of range, use a huge denominator.
    overflow(&with(&|v| {
        let t = &mut v["compositions"][0]["layers"][1]["timing"];
        t["out_point"] = json!(i64::MAX);
        t["stretch"] = json!({"numerator":1,"denominator":u64::MAX});
    }));
    let invalid: Vec<Mutation> = vec![
        Box::new(|v| v["compositions"][0]["layers"][0]["timing"]["out_point"] = json!(0)),
        Box::new(|v| {
            v["compositions"][0]["layers"][0]["timing"]["stretch"] =
                json!({"numerator":0,"denominator":1})
        }),
        Box::new(|v| {
            v["compositions"][0]["layers"][0]["timing"]["stretch"] =
                json!({"numerator":2,"denominator":4})
        }),
        Box::new(|v| v["compositions"][0]["width"] = json!(3)),
        Box::new(|v| v["compositions"][0]["height"] = json!(30001)),
        Box::new(|v| v["compositions"][0]["duration"] = json!(0)),
        Box::new(|v| v["compositions"][0]["frame_rate"] = json!({"numerator":1000,"denominator":1})),
        Box::new(|v| v["compositions"][0]["frame_rate"] = json!({"numerator":7,"denominator":0})),
        // 999/998 fps is inside 1..=999 but is not a whole number of ticks per frame.
        Box::new(|v| v["compositions"][0]["frame_rate"] = json!({"numerator":999,"denominator":998})),
        Box::new(|v| v["compositions"][0]["pixel_aspect"] = json!({"numerator":-1,"denominator":1})),
        Box::new(|v| v["compositions"][0]["layers"][0]["streams"][1]["static_value"] = json!([101.0])),
        Box::new(|v| v["compositions"][0]["layers"][0]["streams"][0]["static_value"] = json!([1.0])),
        Box::new(|v| {
            v["compositions"][0]["layers"][0]["streams"][0]["keyframes"][1]["time"] = json!(0)
        }),
        Box::new(|v| {
            v["compositions"][0]["layers"][0]["streams"][0]["keyframes"][0]["out_tangent"] = json!([])
        }),
        Box::new(|v| {
            v["compositions"][0]["layers"][0]["streams"][0]["keyframes"][0]["out_tangent"][0]
                ["influence"] = json!(1.5)
        }),
        Box::new(|v| {
            v["compositions"][0]["layers"][0]["streams"][0]["static_value"] = json!([1.0e13, 0.0])
        }),
        Box::new(|v| v["compositions"][0]["composition_id"] = json!("bad id")),
        Box::new(|v| v["document_id"] = json!(identity("SDOC", 2))),
    ];
    for (n, f) in invalid.iter().enumerate() {
        assert_eq!(code(&with(f.as_ref())), Code::InvalidField, "case {n}");
    }
    let dup_stream = with(&|v| {
        let s = v["compositions"][0]["layers"][0]["streams"][2].clone();
        v["compositions"][0]["layers"][0]["streams"].as_array_mut().unwrap().push(s);
    });
    assert_eq!(code(&dup_stream), Code::DuplicateIdentity);
    let dup_layer = with(&|v| v["compositions"][1]["layers"][0] = layer(1));
    assert_eq!(code(&dup_layer), Code::DuplicateIdentity);
    let dup_comp = with(&|v| v["compositions"][1]["composition_id"] = json!("main"));
    assert_eq!(code(&dup_comp), Code::DuplicateIdentity);
    let missing_doc_layer = with(&|v| v["compositions"][1]["layers"][0] = layer(99));
    assert_eq!(code(&missing_doc_layer), Code::DanglingReference);
    let bad_parent = with(&|v| v["compositions"][0]["layers"][1]["parent"] = json!(slyr(5)));
    assert_eq!(code(&bad_parent), Code::DanglingReference);
    // SCOM ids: a valid one is accepted; one that only looks like it is refused.
    let scom = identity("SCOM", 1);
    assert_eq!(check(&with(&|v| v["compositions"][1]["composition_id"] = json!(scom))), Ok(vec![]));
    assert_eq!(code(&with(&|v| v["compositions"][1]["composition_id"] = json!("SCOM-nope"))), Code::InvalidField);
    // Wire shape: unknown fields and unknown properties do not deserialize.
    let mut unknown = rich();
    unknown["compositions"][0]["extra"] = json!(1);
    assert!(serde_json::from_value::<StudioCompositionRecords>(unknown).is_err());
    let mut unknown_prop = rich();
    unknown_prop["compositions"][0]["layers"][0]["streams"][0]["property"] = json!("skew");
    assert!(serde_json::from_value::<StudioCompositionRecords>(unknown_prop).is_err());
}

/// Budget and cancellation are honoured.
#[test]
fn budget_and_cancel() {
    let r: StudioCompositionRecords = serde_json::from_value(rich()).unwrap();
    let tight = Budget { nodes: 3, ..Budget::default() };
    let e = validate_composition_records(&r, &document(), tight, &CancellationToken::default(), &Caller)
        .unwrap_err();
    assert_eq!(e.code, Code::BudgetExceeded);
    let token = CancellationToken::default();
    token.cancel();
    let e =
        validate_composition_records(&r, &document(), Budget::default(), &token, &Caller).unwrap_err();
    assert_eq!(e.code, Code::Canceled);
}
