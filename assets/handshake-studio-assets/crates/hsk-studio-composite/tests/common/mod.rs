//! Shared Folio fixtures for the composite integration tests.
#![allow(dead_code)]
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::{BlendSpace, Limits, LoweredPlan, NodeAddr, lower};
use hsk_studio_folio::{
    Budget, Inspection, Resolution, Resolver, Snapshot, TileRef, inspect_bytes,
};
use serde_json::{Value, json};

pub fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let outcome = catch_unwind(AssertUnwindSafe(body));
        let _ = sender.send(outcome);
    });
    match receiver.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("case {name} exceeded declared 30-second deadline")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("case {name} lost deadline worker result")
        }
    }
}

pub fn identity(prefix: &str, n: u8) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
pub fn address(n: u8, op: &str, port: &str) -> Value {
    json!({"layer_id":identity("SLYR",n),"operation_key":op,"port_key":port})
}
pub fn source(n: u8, ty: &str) -> Value {
    json!({"layer_id":identity("SLYR",n),"operation_key":"source","operation_kind":"source","inputs":[],"outputs":[{"port_key":"out","value_type":ty,"cardinality":"one","semantic":"source"}]})
}
pub fn composite(n: u8) -> Value {
    json!({"layer_id":identity("SLYR",n),"operation_key":"mix","operation_kind":"composite","inputs":[{"port_key":"inputs","value_type":"image","cardinality":"many","semantic":"ordered_composite"}],"outputs":[{"port_key":"out","value_type":"image","cardinality":"one","semantic":"composite_result"}]})
}
pub fn layer(n: u8, kind: &str, payload: Value) -> Value {
    json!({"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",n),"name":format!("layer-{n}"),"parent":null,"kind":kind,"payload":payload,"visible":true})
}
pub fn edge(source: Value, target: Value, n: u64) -> Value {
    json!({"source":source,"target":target,"dependency_kind":"data","input_ordinal":n})
}
pub fn tile(layer_n: u8, key: &str, column: i64, row: i64, w: u32, h: u32) -> Value {
    json!({"object_key":key,"layer_id":identity("SLYR",layer_n),"column":column,"row":row,
        "width":{"value":w,"unit":"px"},"height":{"value":h,"unit":"px"},"format":"caller-format",
        "colour_profile_id":identity("SCPF",1),"schema_id":"hsk.studio.raster_tile@1",
        "content_digest":{"algorithm":"sha256","digest":"00"},"artifact_manifest_id":"manifest-1"})
}
/// Folio foundation fixture: group 1 composites rasters 2 then 3 (edges stored out of order);
/// vector 4 and text 5 are standalone sources.
pub fn mixed() -> Value {
    json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":7,"geometry_unit":"mm","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[layer(1,"group",json!({"payload_kind":"group"})),layer(2,"raster",json!({"payload_kind":"raster","tiles":[]})),layer(3,"raster",json!({"payload_kind":"raster","tiles":[]})),layer(4,"vector",json!({"payload_kind":"vector","path_ids":[identity("SVPT",1)]})),layer(5,"text",json!({"payload_kind":"text","story_id":identity("STXT",1)}))],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[source(2,"image"),source(3,"image"),source(4,"vector"),source(5,"text"),composite(1)],"edges":[edge(address(3,"source","out"),address(1,"mix","inputs"),1),edge(address(2,"source","out"),address(1,"mix","inputs"),0)],"outputs":[address(1,"mix","out"),address(4,"source","out"),address(5,"source","out")]}
    })
}
pub fn assign_all_profiles(v: &mut Value) {
    for name in [
        "working_space_rgb",
        "working_space_cmyk",
        "working_space_gray",
        "working_space_spot",
        "blending_space",
    ] {
        v[name] = json!(identity("SCPF", 1));
    }
}

pub struct Available;
impl Resolver for Available {
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
        false
    }
}
pub fn open_bytes(bytes: &[u8], resolver: &dyn Resolver) -> Snapshot {
    match inspect_bytes(bytes, Budget::default(), &CancellationToken::default(), resolver).unwrap()
    {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("typed document became read-only"),
    }
}
pub fn open(v: &Value) -> Snapshot {
    open_bytes(&serde_json::to_vec(v).unwrap(), &Available)
}
pub fn plan_of(s: &Snapshot) -> LoweredPlan {
    lower(
        s,
        s.document().revision,
        BlendSpace::Encoded,
        &Limits::default(),
        &CancellationToken::default(),
    )
    .unwrap()
}
pub fn addr(n: u8, op: &str, port: &str) -> NodeAddr {
    NodeAddr {
        layer_id: identity("SLYR", n),
        operation_key: op.into(),
        port_key: port.into(),
    }
}
