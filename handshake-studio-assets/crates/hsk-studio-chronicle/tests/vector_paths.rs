//! `Property::VectorPaths` (lane SMB-ILLUSTRATOR-PEN-2): a vector layer's path id list edits in
//! place (the layer keeps its id) and inverts as one step; other payloads and value types are
//! refused typed.
use hsk_studio_accord::CancellationToken;
use hsk_studio_chronicle::*;
use hsk_studio_folio::{Inspection, NodeKind, NodeRef, Payload, Resolution, Resolver, Snapshot, TileRef};
use serde_json::json;

fn id(prefix: &str, n: u8) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}

/// Every referenced record is available (the caller's path records).
struct Available;
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

/// Root stack: raster L2 below vector L1 (path P1).
fn snapshot() -> Snapshot {
    let layer = |n: u8, kind: &str, payload: serde_json::Value| {
        json!({"schema_id":"hsk.studio.layer@1","layer_id":id("SLYR",n),"name":format!("L{n}"),"parent":null,"kind":kind,"payload":payload,"visible":true})
    };
    let op = |n: u8| {
        json!({"layer_id":id("SLYR",n),"operation_key":"source","operation_kind":"source","inputs":[],
            "outputs":[{"port_key":"out","value_type":"image","cardinality":"one","semantic":"source"}]})
    };
    let out = |n: u8| json!({"layer_id":id("SLYR",n),"operation_key":"source","port_key":"out"});
    let d = json!({"schema_id":"hsk.studio.document@1","document_id":id("SDOC",1),"revision":3,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[layer(2,"raster",json!({"payload_kind":"raster","tiles":[]})),layer(1,"vector",json!({"payload_kind":"vector","path_ids":[id("SVPT",1)]}))],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[op(2),op(1)],"edges":[],"outputs":[out(2),out(1)]}});
    match hsk_studio_folio::inspect_bytes(&serde_json::to_vec(&d).unwrap(), Default::default(), &CancellationToken::default(), &Available).unwrap() {
        Inspection::Editable(s) => s,
        _ => panic!("fixture read-only"),
    }
}

fn actor() -> Actor {
    Actor {
        account_id: "account".into(),
        principal_id: "principal".into(),
        owner_account_id: "owner".into(),
        owner_principal_id: "owner-principal".into(),
        access_space_id: "space".into(),
        session_id: "session".into(),
    }
}

fn at(n: u8, property: Property) -> PropertyAddress {
    PropertyAddress { target: NodeRef { node_kind: NodeKind::Layer, node_id: id("SLYR", n) }, property }
}

fn patch(s: &Snapshot, address: PropertyAddress, revision: u64, expected: PropertyValue, value: PropertyValue) -> Patch {
    Patch {
        transport_version: 1,
        document_id: id("SDOC", 1),
        base_revision: s.document().revision,
        command_id: "c1".into(),
        correlation_id: 1,
        actor: actor(),
        reads: vec![PropertyRead { address: address.clone(), expected_revision: revision, expected_value: expected }],
        writes: vec![PropertyWrite { address, value }],
    }
}

fn run(p: &Patch, s: &Snapshot, v: &[RevisionEntry]) -> Result<Prepared, Diagnostic> {
    prepare(p, s, v, Budget::default(), &CancellationToken::default(), &Available)
}

fn paths(s: &Snapshot, n: u8) -> Vec<String> {
    match &s.document().layers.iter().find(|l| l.layer_id == id("SLYR", n)).unwrap().payload {
        Payload::Vector { path_ids } => path_ids.clone(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn vector_paths_property_edits_in_place_and_inverts() {
    let s = snapshot();
    let mut vector = seed_revisions(s.document());
    // Seeded for the vector layer only.
    let a = at(1, Property::VectorPaths);
    assert!(vector.iter().any(|e| e.address == a));
    assert!(!vector.iter().any(|e| e.address == at(2, Property::VectorPaths)));
    assert!(PROPERTIES.contains(&Property::VectorPaths) && addressable(&NodeKind::Layer, Property::VectorPaths));
    assert!(!addressable(&NodeKind::Artboard, Property::VectorPaths));

    // Write [P2, P3]: same layer id, new ids, one revision update.
    let new = vec![id("SVPT", 2), id("SVPT", 3)];
    let p = patch(&s, a.clone(), 0, PropertyValue::PathIds(vec![id("SVPT", 1)]), PropertyValue::PathIds(new.clone()));
    let changed = run(&p, &s, &vector).unwrap_or_else(|e| panic!("{e:?}"));
    let next = changed.successor().expect("changed").clone();
    assert_eq!(paths(&next, 1), new);
    assert_eq!(next.document().layers.len(), 2, "no layer created or deleted");
    assert_eq!(changed.revision_updates().len(), 1);
    // Lineage: equal to the base once the declared address is masked.
    let (mut base, mut succ) = (s.document().clone(), next.document().clone());
    base.revision = succ.revision;
    assert!(mask_property(&mut base, &a) && mask_property(&mut succ, &a));
    assert_eq!(base, succ);
    for u in changed.revision_updates() {
        vector.iter_mut().find(|e| e.address == u.address).unwrap().revision = u.revision;
    }
    // The inverse restores the original list exactly.
    let inverse = changed.inverse().unwrap().invocation("undo".into(), 2, actor(), next.document().revision);
    let undone = run(&inverse, &next, &vector).unwrap_or_else(|e| panic!("{e:?}"));
    let back = undone.successor().unwrap();
    assert_eq!(back.document().layers, s.document().layers);

    // Typed refusals: not a vector layer, wrong value type, a malformed id (folio), stale read.
    let raster = at(2, Property::VectorPaths);
    let mut v2 = vector.clone();
    v2.push(RevisionEntry { address: raster.clone(), revision: 0 });
    let e = run(&patch(&s, raster, 0, PropertyValue::PathIds(vec![]), PropertyValue::PathIds(vec![id("SVPT", 4)])), &s, &v2).unwrap_err();
    assert_eq!(e.code, Code::InvalidTarget);
    let fresh = seed_revisions(s.document());
    let e = run(&patch(&s, a.clone(), 0, PropertyValue::PathIds(vec![id("SVPT", 1)]), PropertyValue::String("x".into())), &s, &fresh).unwrap_err();
    assert_eq!(e.code, Code::WrongType);
    let e = run(&patch(&s, a.clone(), 0, PropertyValue::PathIds(vec![id("SVPT", 1)]), PropertyValue::PathIds(vec!["nope".into()])), &s, &fresh).unwrap_err();
    assert_eq!(e.code, Code::FolioRejected);
    let e = run(&patch(&s, a.clone(), 0, PropertyValue::PathIds(vec![id("SVPT", 9)]), PropertyValue::PathIds(new.clone())), &s, &fresh).unwrap_err();
    assert_ne!(e.code, Code::InvalidTarget, "a stale value read is a conflict, not a target error: {e:?}");
    // Wire form round trips through the typed decoder.
    let wire = serde_json::to_vec(&p).unwrap();
    assert_eq!(decode_patch(&wire, Budget::default(), &CancellationToken::default()).unwrap(), p);
}
