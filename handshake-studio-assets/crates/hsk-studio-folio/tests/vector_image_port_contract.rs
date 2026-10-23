//! SMB-FOLIO-VECTOR-IMAGE-PORT: a vector source may declare its single `out` port `image`
//! (rasterisable vector) so a vector layer can be a group input; the vector-typed form is unchanged.
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
const RASTER: u32 = 2;
const VEC: u32 = 3;

fn layer(n: u32, kind: &str, payload: Value, parent: Option<u32>) -> Value {
    let parent = parent.map_or(Value::Null, |p| json!({"node_kind":"layer","node_id":slyr(p)}));
    json!({"schema_id":"hsk.studio.layer@1","layer_id":slyr(n),"name":format!("l{n}"),
        "parent":parent,"kind":kind,"payload":payload,"visible":true})
}
fn source_op(n: u32, value_type: &str) -> Value {
    json!({"layer_id":slyr(n),"operation_key":"source","operation_kind":"source","inputs":[],
        "outputs":[{"port_key":"out","value_type":value_type,"cardinality":"one","semantic":"source"}]})
}
fn edge(n: u32, ordinal: u64) -> Value {
    json!({"source":{"layer_id":slyr(n),"operation_key":"source","port_key":"out"},
        "target":{"layer_id":slyr(GROUP),"operation_key":"composite","port_key":"inputs"},
        "dependency_kind":"data","input_ordinal":ordinal})
}
fn document_json(vector_type: &str, edges: Vec<Value>) -> Value {
    json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"px",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[
            layer(GROUP, "group", json!({"payload_kind":"group"}), None),
            layer(RASTER, "raster", json!({"payload_kind":"raster","tiles":[]}), Some(GROUP)),
            layer(VEC, "vector", json!({"payload_kind":"vector","path_ids":[identity("SVPT",1)]}), Some(GROUP)),
        ],
        "graph":{"schema_id":"hsk.studio.layer_graph@1",
            "operations":[
                source_op(RASTER, "image"), source_op(VEC, vector_type),
                {"layer_id":slyr(GROUP),"operation_key":"composite","operation_kind":"composite",
                 "inputs":[{"port_key":"inputs","value_type":"image","cardinality":"many","semantic":"ordered_composite"}],
                 "outputs":[{"port_key":"out","value_type":"image","cardinality":"one","semantic":"composite_result"}]}],
            "edges":edges,
            "outputs":[{"layer_id":slyr(GROUP),"operation_key":"composite","port_key":"out"}]}
    })
}
fn validate_doc(v: &Value) -> Result<Snapshot, Diagnostic> {
    let d: StudioDocument = serde_json::from_value(v.clone()).expect("document shape");
    validate_document(d, Budget::default(), &CancellationToken::default(), &Caller)
}

/// Image-typed vector source feeds a group (stack order by ordinal, bytes reopen identically); the
/// vector-typed form validates unlinked and is refused (InvalidEdge) into image inputs; other
/// value types on a vector source stay refused.
#[test]
fn vector_source_image_port_feeds_group() {
    let snapshot = validate_doc(&document_json("image", vec![edge(RASTER, 0), edge(VEC, 1)]))
        .expect("raster + image-typed vector in a group");
    let stack: Vec<String> = snapshot
        .ordered_inputs(&PortAddress {
            layer_id: slyr(GROUP),
            operation_key: "composite".into(),
            port_key: "inputs".into(),
        })
        .into_iter()
        .map(|a| a.layer_id.clone())
        .collect();
    assert_eq!(stack, vec![slyr(RASTER), slyr(VEC)]);
    let reopened: StudioDocument = serde_json::from_slice(snapshot.encoded_bytes()).unwrap();
    assert_eq!(serde_json::to_vec(&reopened).unwrap(), snapshot.encoded_bytes());

    let typed = document_json("vector", vec![edge(RASTER, 0), edge(VEC, 1)]);
    assert_eq!(validate_doc(&typed).unwrap_err().code, Code::InvalidEdge);
    let unlinked = document_json("vector", vec![edge(RASTER, 0)]);
    assert!(validate_doc(&unlinked).is_ok(), "existing vector-typed form unchanged");
    let text = document_json("text", vec![edge(RASTER, 0)]);
    assert_eq!(validate_doc(&text).unwrap_err().code, Code::InvalidPort);
}
