//! Adjustment layers in the composition graph (STU-RAS-007 lists `adjustment` as a layer kind,
//! STU-CMP-092 makes `input_ordinal` the sole stack order). Every expected code is derived from the
//! rule named next to it; documents without an adjustment layer in the graph must validate as they
//! did before (test `existing_documents_validate_identically`).
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use serde_json::{Value, json};

fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn slyr(n: u32) -> String {
    identity("SLYR", n)
}
/// Resolver that can report the adjustment primitive as absent.
struct Caller {
    adjustment_absent: bool,
}
const AVAILABLE: Caller = Caller {
    adjustment_absent: false,
};
impl Resolver for Caller {
    fn primitive(&self, schema_id: &str, _: &str) -> Resolution {
        if self.adjustment_absent && schema_id == ADJUSTMENT_SCHEMA {
            Resolution::Absent
        } else {
            Resolution::Available
        }
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
const BASE: u32 = 2;
const ADJ: u32 = 3;
const TOP: u32 = 4;

fn layer(n: u32, kind: &str, payload: Value) -> Value {
    json!({"schema_id":"hsk.studio.layer@1","layer_id":slyr(n),"name":format!("l{n}"),
        "parent":{"node_kind":"layer","node_id":slyr(GROUP)},"kind":kind,"payload":payload,"visible":true})
}
fn clipped() -> Value {
    json!({"opacity":100.0,"fill_opacity":100.0,"blend_mode":2,"clipped":true,"isolation":"isolated"})
}
fn source_op(n: u32) -> Value {
    json!({"layer_id":slyr(n),"operation_key":"source","operation_kind":"source","inputs":[],
        "outputs":[{"port_key":"out","value_type":"image","cardinality":"one","semantic":"source"}]})
}
fn edge(n: u32, ordinal: u64) -> Value {
    json!({"source":{"layer_id":slyr(n),"operation_key":"source","port_key":"out"},
        "target":{"layer_id":slyr(GROUP),"operation_key":"composite","port_key":"inputs"},
        "dependency_kind":"data","input_ordinal":ordinal})
}
fn adjustment_payload() -> Value {
    json!({"payload_kind":"primitive_reference","schema_id":"hsk.studio.adjustment@1","object_id":slyr(ADJ)})
}
/// Group (1) over raster base (2), adjustment layer (3) and raster (4); graph stack bottom to top:
/// base, adjustment, top (ordinals 0, 1, 2).
fn document_json() -> Value {
    let mut group = layer(GROUP, "group", json!({"payload_kind":"group"}));
    group["parent"] = Value::Null;
    json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"px",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[
            group,
            layer(BASE, "raster", json!({"payload_kind":"raster","tiles":[]})),
            layer(ADJ, "adjustment", adjustment_payload()),
            layer(TOP, "raster", json!({"payload_kind":"raster","tiles":[]})),
        ],
        "graph":{"schema_id":"hsk.studio.layer_graph@1",
            "operations":[
                source_op(BASE), source_op(ADJ), source_op(TOP),
                {"layer_id":slyr(GROUP),"operation_key":"composite","operation_kind":"composite",
                 "inputs":[{"port_key":"inputs","value_type":"image","cardinality":"many","semantic":"ordered_composite"}],
                 "outputs":[{"port_key":"out","value_type":"image","cardinality":"one","semantic":"composite_result"}]}],
            "edges":[edge(BASE,0), edge(ADJ,1), edge(TOP,2)],
            "outputs":[{"layer_id":slyr(GROUP),"operation_key":"composite","port_key":"out"}]}
    })
}
fn parse(v: &Value) -> StudioDocument {
    serde_json::from_value(v.clone()).expect("document shape")
}
fn validate(v: &Value, r: &Caller) -> Result<Snapshot, Diagnostic> {
    validate_document(
        parse(v),
        Budget::default(),
        &CancellationToken::default(),
        r,
    )
}
fn code(v: &Value) -> (Code, String) {
    let d = validate(v, &AVAILABLE).expect_err("must be refused");
    (d.code, d.target)
}
fn layer_mut(v: &mut Value, n: u32) -> &mut Value {
    let id = slyr(n);
    v["layers"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|l| l["layer_id"] == id.as_str())
        .unwrap()
}
fn inputs_address() -> PortAddress {
    PortAddress {
        layer_id: slyr(GROUP),
        operation_key: "composite".into(),
        port_key: "inputs".into(),
    }
}
fn stack(s: &Snapshot) -> Vec<String> {
    s.ordered_inputs(&inputs_address())
        .into_iter()
        .map(|a| a.layer_id.clone())
        .collect()
}
fn records(layers: Value) -> StudioRasterRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.raster_records@1",
        "document_id":identity("SDOC",1),"layers":layers}))
    .expect("record shape")
}
fn invert_record(n: u32) -> Value {
    json!({"layer_id":slyr(n),"adjustment":{"kind":"invert"}})
}
fn check_records(r: &StudioRasterRecords, v: &Value) -> Result<Vec<ResolutionIssue>, Diagnostic> {
    validate_raster_records(
        r,
        &parse(v),
        Budget::default(),
        &CancellationToken::default(),
        &AVAILABLE,
    )
}

/// STU-CMP-092: the adjustment layer is an image source and an edge source into the group's
/// `inputs`; its stack position is its `input_ordinal`, not the layer array position.
#[test]
fn adjustment_layer_is_a_graph_source_with_a_stack_position() {
    let v = document_json();
    let snapshot = validate(&v, &AVAILABLE).expect("adjustment layer as a source validates");
    assert!(snapshot.resolution_issues().is_empty());
    assert_eq!(stack(&snapshot), [slyr(BASE), slyr(ADJ), slyr(TOP)]);
    // The adjustment source output is the same port shape as a raster source (image, one, source).
    let op = snapshot
        .document()
        .graph
        .operations
        .iter()
        .find(|o| o.layer_id == slyr(ADJ))
        .unwrap();
    assert_eq!(op.operation_kind, OperationKind::Source);
    assert_eq!(
        op.outputs,
        [Port {
            port_key: "out".into(),
            value_type: ValueType::Image,
            cardinality: Cardinality::One,
            semantic: Semantic::Source
        }]
    );
    // Moving the adjustment to the bottom changes only the ordinals; edge array order is irrelevant.
    let mut moved = v.clone();
    moved["graph"]["edges"] = json!([edge(TOP, 2), edge(BASE, 1), edge(ADJ, 0)]);
    let moved = validate(&moved, &AVAILABLE).unwrap();
    assert_eq!(stack(&moved), [slyr(ADJ), slyr(BASE), slyr(TOP)]);
    // The bytes round-trip through the wire reader to an editable snapshot of the same document.
    match inspect_bytes(
        snapshot.encoded_bytes(),
        Budget::default(),
        &CancellationToken::default(),
        &AVAILABLE,
    )
    .unwrap()
    {
        Inspection::Editable(again) => assert_eq!(again.document(), snapshot.document()),
        Inspection::ReadOnly(_) => panic!("known document must stay editable"),
    }
    // The adjustment record validates against the same document.
    let r = records(json!([invert_record(ADJ)]));
    assert_eq!(check_records(&r, &v), Ok(vec![]));
}

/// The adjustment record must exist and match the layer (STU-RAS-007: one adjustment per
/// adjustment layer); every other adjustment-shaped payload is a typed refusal.
#[test]
fn adjustment_record_must_exist_and_match_the_layer() {
    // Payload names another layer's id: PayloadMismatch on the adjustment layer.
    let mut v = document_json();
    layer_mut(&mut v, ADJ)["payload"]["object_id"] = json!(slyr(BASE));
    assert_eq!(code(&v), (Code::PayloadMismatch, slyr(ADJ)));
    // Payload names another schema on an adjustment layer: PayloadMismatch.
    let mut v = document_json();
    layer_mut(&mut v, ADJ)["payload"]["schema_id"] = json!("hsk.studio.gradient@1");
    assert_eq!(code(&v), (Code::PayloadMismatch, slyr(ADJ)));
    // Preserved unsupported content has no graph descriptor: UnsupportedDescriptor.
    let mut v = document_json();
    layer_mut(&mut v, ADJ)["payload"] = json!({"payload_kind":"unsupported","kind":"adjustment",
        "schema_id":"vendor.adjustment@1","encoded_bytes":[1,2,3],"reason":"vendor_kind"});
    assert_eq!(code(&v), (Code::UnsupportedDescriptor, slyr(ADJ)));
    // The source output must be the declared descriptor port: a vector output is InvalidPort.
    let mut v = document_json();
    let ops = v["graph"]["operations"].as_array_mut().unwrap();
    ops[1]["outputs"][0]["value_type"] = json!("vector");
    assert_eq!(code(&v).0, Code::InvalidPort);
    // Unresolvable adjustment primitive: an issue, so the snapshot is not available.
    let absent = Caller {
        adjustment_absent: true,
    };
    let snapshot = validate(&document_json(), &absent).unwrap();
    assert_eq!(
        snapshot.resolution_issues(),
        [ResolutionIssue {
            target: slyr(ADJ),
            disposition: Resolution::Absent
        }]
    );
    assert_eq!(
        snapshot.require_available().unwrap_err().code,
        Code::Unavailable
    );
    // Record set: the adjustment layer needs its record (DanglingReference) and a record on a
    // non-adjustment layer is PayloadMismatch.
    let v = document_json();
    let missing = records(json!([]));
    assert_eq!(
        check_records(&missing, &v),
        Err(Diagnostic {
            code: Code::DanglingReference,
            target: slyr(ADJ)
        })
    );
    let wrong_layer = records(json!([invert_record(ADJ), invert_record(BASE)]));
    assert_eq!(
        check_records(&wrong_layer, &v),
        Err(Diagnostic {
            code: Code::PayloadMismatch,
            target: slyr(BASE)
        })
    );
}

/// Clip base: STU-RAS-015 defines the base through its content and alpha and STU-RAS-007 makes the
/// adjustment layer the clipped layer ("clipped to its parent"); a layer clipped to an adjustment
/// layer is refused. The base is the nearest earlier input by `input_ordinal` that is not clipped.
#[test]
fn adjustment_clip_base_rule() {
    // Top clipped directly above the unclipped adjustment layer: refused, target = the clipped layer.
    let mut v = document_json();
    layer_mut(&mut v, TOP)["appearance"] = clipped();
    assert_eq!(code(&v), (Code::AdjustmentClipBase, slyr(TOP)));
    // The rule follows input_ordinal, not edge array order: with the edges stored in reverse the
    // result is the same.
    let mut reversed = v.clone();
    reversed["graph"]["edges"] = json!([edge(TOP, 2), edge(ADJ, 1), edge(BASE, 0)]);
    assert_eq!(code(&reversed), (Code::AdjustmentClipBase, slyr(TOP)));
    // The adjustment layer clipped to its base is accepted, and layers clipped above it share the
    // base (the clipped adjustment layer is not a base).
    let mut ok = document_json();
    layer_mut(&mut ok, ADJ)["appearance"] = clipped();
    layer_mut(&mut ok, TOP)["appearance"] = clipped();
    assert_eq!(
        stack(&validate(&ok, &AVAILABLE).expect("adjustment clipped to its base")),
        [slyr(BASE), slyr(ADJ), slyr(TOP)]
    );
    // Reordering so the clipped layer sits below the adjustment layer: its base is the raster.
    let mut below = document_json();
    layer_mut(&mut below, TOP)["appearance"] = clipped();
    below["graph"]["edges"] = json!([edge(BASE, 0), edge(TOP, 1), edge(ADJ, 2)]);
    assert!(validate(&below, &AVAILABLE).is_ok());
    // Unclipped content above an adjustment layer is not a clip.
    assert!(validate(&document_json(), &AVAILABLE).is_ok());
}

/// Additivity: no document field is added and documents that do not put an adjustment layer in the
/// graph validate as before, including the old refusal of other kinds as sources.
#[test]
fn existing_documents_validate_identically() {
    // The wire shape is unchanged: the document serializes back to the JSON that built it.
    let v = document_json();
    let snapshot = validate(&v, &AVAILABLE).unwrap();
    assert_eq!(serde_json::to_value(snapshot.document()).unwrap(), v);
    // Adjustment layer outside the graph (the only form that existed before): still valid, and
    // the payload object id is not constrained there.
    let mut outside = document_json();
    outside["graph"]["operations"]
        .as_array_mut()
        .unwrap()
        .remove(1);
    outside["graph"]["edges"] = json!([edge(BASE, 0), edge(TOP, 1)]);
    assert_eq!(
        stack(&validate(&outside, &AVAILABLE).unwrap()),
        [slyr(BASE), slyr(TOP)]
    );
    layer_mut(&mut outside, ADJ)["payload"]["object_id"] = json!(slyr(BASE));
    assert!(validate(&outside, &AVAILABLE).is_ok());
    // Clipped layers over a raster base are unaffected by the new rule.
    let mut raster = document_json();
    raster["layers"].as_array_mut().unwrap().remove(2);
    raster["graph"]["operations"]
        .as_array_mut()
        .unwrap()
        .remove(1);
    raster["graph"]["edges"] = json!([edge(BASE, 0), edge(TOP, 1)]);
    layer_mut(&mut raster, TOP)["appearance"] = clipped();
    assert!(validate(&raster, &AVAILABLE).is_ok());
    // Other layer kinds are still refused as sources: the descriptor set did not widen.
    for kind in ["live_filter", "fill", "mask", "placed_asset"] {
        let mut v = document_json();
        let l = layer_mut(&mut v, ADJ);
        l["kind"] = json!(kind);
        assert_eq!(code(&v), (Code::UnsupportedDescriptor, slyr(ADJ)), "{kind}");
    }
}
