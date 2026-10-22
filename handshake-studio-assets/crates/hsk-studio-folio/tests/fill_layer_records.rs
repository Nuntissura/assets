//! Fill layers (lane SMB-PS-FILL-LAYERS; STU-RAS-007 layer kind `fill`, STU-RAS-025/160/161): a `fill`
//! layer is an image source of the graph, names its own fill record, and every record field has
//! its bound. A record set without fill records serialises byte-identically to before.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
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
const GROUP: u32 = 1;
const FILL: u32 = 2;

fn fill_document(schema: &str) -> Value {
    let group = json!({"schema_id":"hsk.studio.layer@1","layer_id":slyr(GROUP),"name":"g","parent":null,
        "kind":"group","payload":{"payload_kind":"group"},"visible":true});
    json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"px",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[group,
            {"schema_id":"hsk.studio.layer@1","layer_id":slyr(FILL),"name":"fill",
             "parent":{"node_kind":"layer","node_id":slyr(GROUP)},"kind":"fill",
             "payload":{"payload_kind":"primitive_reference","schema_id":schema,"object_id":slyr(FILL)},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1",
            "operations":[
                {"layer_id":slyr(FILL),"operation_key":"source","operation_kind":"source","inputs":[],
                 "outputs":[{"port_key":"out","value_type":"image","cardinality":"one","semantic":"source"}]},
                {"layer_id":slyr(GROUP),"operation_key":"composite","operation_kind":"composite",
                 "inputs":[{"port_key":"inputs","value_type":"image","cardinality":"many","semantic":"ordered_composite"}],
                 "outputs":[{"port_key":"out","value_type":"image","cardinality":"one","semantic":"composite_result"}]}],
            "edges":[{"source":{"layer_id":slyr(FILL),"operation_key":"source","port_key":"out"},
                "target":{"layer_id":slyr(GROUP),"operation_key":"composite","port_key":"inputs"},
                "dependency_kind":"data","input_ordinal":0}],
            "outputs":[{"layer_id":slyr(GROUP),"operation_key":"composite","port_key":"out"}]}
    })
}
fn parse(v: &Value) -> StudioDocument {
    serde_json::from_value(v.clone()).expect("document shape")
}
fn colour(r: f64) -> Value {
    json!({"r":r,"g":0.5,"b":0.25,"colour_profile_id":null})
}
fn stops() -> Value {
    json!([{"position":0.0,"midpoint":50.0,"opacity":100.0,"colour":colour(0.0)},
           {"position":100.0,"midpoint":null,"opacity":100.0,"colour":colour(1.0)}])
}
fn solid() -> Value {
    json!({"bounds":{"x":0,"y":0,"width":8,"height":4},"fill":{"fill_kind":"solid","colour":colour(0.2)}})
}
fn gradient() -> Value {
    json!({"bounds":{"x":0,"y":0,"width":8,"height":4},"fill":{"fill_kind":"gradient","style":"linear",
        "stops":stops(),"angle":90.0,"scale":100.0,"reverse":false,"dither":false,"align_with_layer":true,
        "offset_x":0.0,"offset_y":0.0}})
}
fn tile() -> Value {
    json!({"object_key":"tile","layer_id":slyr(FILL),"column":0,"row":0,
        "width":{"value":2.0,"unit":"px"},"height":{"value":2.0,"unit":"px"},"format":"rgb_f32le",
        "colour_profile_id":identity("SCPF",1),"schema_id":"hsk.studio.raster_tile@1",
        "content_digest":{"algorithm":"sha256","digest":"00"},"artifact_manifest_id":"m"})
}
fn pattern() -> Value {
    json!({"bounds":{"x":0,"y":0,"width":8,"height":4},"fill":{"fill_kind":"pattern",
        "pattern":{"schema_id":"hsk.studio.pattern@1","object_id":"PAT-1"},"pattern_tile":tile(),
        "scale":100.0,"phase_x":0.0,"phase_y":0.0,"link_with_layer":true}})
}
fn records(fill: Value) -> StudioRasterRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.raster_records@1","document_id":identity("SDOC",1),
        "layers":[{"layer_id":slyr(FILL),"fill_layer":fill}]}))
    .expect("record shape")
}
fn check_in(doc: &Value, fill: Value) -> Result<Vec<ResolutionIssue>, Diagnostic> {
    validate_raster_records(
        &records(fill),
        &parse(doc),
        Budget::default(),
        &CancellationToken::default(),
        &Caller,
    )
}
fn check(schema: &str, fill: Value) -> Result<Vec<ResolutionIssue>, Diagnostic> {
    check_in(&fill_document(schema), fill)
}
fn refused(schema: &str, fill: Value) -> Code {
    check(schema, fill).expect_err("must be refused").code
}
const SWATCH: &str = "hsk.studio.swatch@1";
const GRADIENT: &str = "hsk.studio.gradient@1";
const PATTERN: &str = "hsk.studio.pattern@1";

#[test]
fn fill_layers_are_graph_sources_and_validate_per_kind() {
    for (schema, fill) in [(SWATCH, solid()), (GRADIENT, gradient()), (PATTERN, pattern())] {
        let v = fill_document(schema);
        validate_document(parse(&v), Budget::default(), &CancellationToken::default(), &Caller)
            .unwrap_or_else(|d| panic!("{schema}: fill layer as a source: {d:?}"));
        check(schema, fill).unwrap_or_else(|d| panic!("{schema}: {d:?}"));
    }
}

#[test]
fn a_fill_layer_needs_its_record_and_the_matching_schema() {
    let v = fill_document(SWATCH);
    let none: StudioRasterRecords = serde_json::from_value(json!({"schema_id":"hsk.studio.raster_records@1",
        "document_id":identity("SDOC",1),"layers":[]}))
    .unwrap();
    let d = validate_raster_records(&none, &parse(&v), Budget::default(), &CancellationToken::default(), &Caller)
        .expect_err("fill layer without record");
    assert_eq!(d.code, Code::DanglingReference);
    assert_eq!(refused(GRADIENT, solid()), Code::PayloadMismatch);
    assert_eq!(refused(SWATCH, gradient()), Code::PayloadMismatch);
    let mut raster = fill_document(SWATCH);
    raster["layers"][1]["kind"] = json!("raster");
    raster["layers"][1]["payload"] = json!({"payload_kind":"raster","tiles":[]});
    assert_eq!(check_in(&raster, solid()).expect_err("fill record on a raster layer").code, Code::PayloadMismatch);
    let mut foreign = fill_document(SWATCH);
    foreign["layers"][1]["payload"]["object_id"] = json!(slyr(GROUP));
    let d = validate_document(parse(&foreign), Budget::default(), &CancellationToken::default(), &Caller)
        .expect_err("foreign fill payload");
    assert_eq!(d.code, Code::UnsupportedDescriptor);
}

#[test]
fn fill_record_bounds_are_enforced() {
    for (field, value) in [("width", json!(0)), ("width", json!(300_001)), ("x", json!(i64::MAX)), ("y", json!(i64::MIN))] {
        let mut f = solid();
        f["bounds"][field] = value;
        assert_eq!(refused(SWATCH, f), Code::InvalidField, "{field}");
    }
    let mut s = solid();
    s["fill"]["colour"]["r"] = json!(1.5);
    assert_eq!(refused(SWATCH, s), Code::InvalidField);

    for (field, value) in [
        ("angle", json!(181.0)),
        ("angle", json!(-181.0)),
        ("scale", json!(0.0)),
        ("scale", json!(-5.0)),
        ("stops", json!([])),
        ("stops", json!([{"position":0.0,"midpoint":null,"opacity":100.0,"colour":colour(0.0)}])),
        (
            "stops",
            json!([{"position":60.0,"midpoint":null,"opacity":100.0,"colour":colour(0.0)},
                   {"position":40.0,"midpoint":null,"opacity":100.0,"colour":colour(1.0)}]),
        ),
    ] {
        let mut g = gradient();
        g["fill"][field] = value;
        assert_eq!(refused(GRADIENT, g), Code::InvalidField, "{field}");
    }
    let mut g = gradient();
    g["fill"]["stops"][0]["midpoint"] = json!(5.0);
    assert_eq!(refused(GRADIENT, g), Code::InvalidField);
    // Finite and large is Folio-valid; the engine bounds it at lowering.
    let mut g = gradient();
    g["fill"]["offset_x"] = json!(f64::MAX);
    assert!(check(GRADIENT, g).is_ok());

    let mut p = pattern();
    p["fill"]["pattern"]["schema_id"] = json!(GRADIENT);
    assert_eq!(refused(PATTERN, p), Code::InvalidField);
    let mut p = pattern();
    p["fill"]["scale"] = json!(0.0);
    assert_eq!(refused(PATTERN, p), Code::InvalidField);
    let mut p = pattern();
    p["fill"]["pattern_tile"]["width"]["value"] = json!(4097.0);
    assert_eq!(refused(PATTERN, p), Code::InvalidField);
    let mut p = pattern();
    p["fill"]["pattern_tile"]["format"] = json!("rgba8");
    assert_eq!(refused(PATTERN, p), Code::InvalidField);
    // An unresolved tile is allowed (absent = unresolved).
    let mut p = pattern();
    p["fill"].as_object_mut().unwrap().remove("pattern_tile");
    assert!(check(PATTERN, p).is_ok());
}

#[test]
fn unknown_fields_and_kinds_are_refused_at_the_wire() {
    let mut g = gradient();
    g["fill"]["noise"] = json!(true);
    assert!(serde_json::from_value::<StudioFillLayer>(g).is_err(), "unknown gradient field");
    let mut s = solid();
    s["fill"]["fill_kind"] = json!("noise");
    assert!(serde_json::from_value::<StudioFillLayer>(s).is_err());
}

#[test]
fn record_sets_without_fill_layers_serialise_as_before() {
    let r: StudioRasterRecords = serde_json::from_value(json!({"schema_id":"hsk.studio.raster_records@1",
        "document_id":identity("SDOC",1),"layers":[{"layer_id":slyr(FILL),"adjustment":{"kind":"invert"}}]}))
    .unwrap();
    let out = serde_json::to_value(&r).unwrap();
    assert!(out["layers"][0].get("fill_layer").is_none());
    let with = records(solid());
    let round: StudioRasterRecords = serde_json::from_value(serde_json::to_value(&with).unwrap()).unwrap();
    assert_eq!(round, with);
}
