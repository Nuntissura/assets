//! Layer-stack structure through `Property::Placement` (lane SMB-HARNESS-UI-3): create, delete,
//! reorder, group and ungroup as chronicle transactions whose inverses restore the stack exactly.
use hsk_studio_accord::CancellationToken;
use hsk_studio_chronicle::*;
use hsk_studio_folio::{
    Inspection, NodeKind, NodeRef, Operation, Parent, Snapshot, StudioLayer, Unresolved,
};
use serde_json::{Value, json};

fn id(prefix: &str, n: u8) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn layer_json(n: u8, kind: &str, parent: Value) -> Value {
    let payload = if kind == "group" { json!({"payload_kind":"group"}) } else { json!({"payload_kind":"raster","tiles":[]}) };
    json!({"schema_id":"hsk.studio.layer@1","layer_id":id("SLYR",n),"name":format!("L{n}"),"parent":parent,"kind":kind,"payload":payload,"visible":true})
}
fn op_json(n: u8, kind: &str) -> Value {
    if kind == "group" {
        json!({"layer_id":id("SLYR",n),"operation_key":"mix","operation_kind":"composite",
            "inputs":[{"port_key":"inputs","value_type":"image","cardinality":"many","semantic":"ordered_composite"}],
            "outputs":[{"port_key":"out","value_type":"image","cardinality":"one","semantic":"composite_result"}]})
    } else {
        json!({"layer_id":id("SLYR",n),"operation_key":"source","operation_kind":"source","inputs":[],
            "outputs":[{"port_key":"out","value_type":"image","cardinality":"one","semantic":"source"}]})
    }
}
fn out(n: u8, kind: &str) -> Value {
    json!({"layer_id":id("SLYR",n),"operation_key":if kind == "group" {"mix"} else {"source"},"port_key":"out"})
}
/// Root stack (bottom first): L1, L2, L3, G4 = [L5] (L5 inside the group).
fn snapshot() -> Snapshot {
    let g = json!({"node_kind":"layer","node_id":id("SLYR",4)});
    let d = json!({"schema_id":"hsk.studio.document@1","document_id":id("SDOC",1),"revision":3,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[layer_json(1,"raster",Value::Null),layer_json(2,"raster",Value::Null),layer_json(3,"raster",Value::Null),layer_json(4,"group",Value::Null),layer_json(5,"raster",g)],
        "graph":{"schema_id":"hsk.studio.layer_graph@1",
            "operations":[op_json(1,"raster"),op_json(2,"raster"),op_json(3,"raster"),op_json(4,"group"),op_json(5,"raster")],
            "edges":[{"source":out(5,"raster"),"target":{"layer_id":id("SLYR",4),"operation_key":"mix","port_key":"inputs"},"dependency_kind":"data","input_ordinal":0}],
            "outputs":[out(1,"raster"),out(2,"raster"),out(3,"raster"),out(4,"group")]}});
    match hsk_studio_folio::inspect_bytes(&serde_json::to_vec(&d).unwrap(), Default::default(), &CancellationToken::default(), &Unresolved).unwrap() {
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
fn at(n: u8) -> PropertyAddress {
    PropertyAddress { target: NodeRef { node_kind: NodeKind::Layer, node_id: id("SLYR", n) }, property: Property::Placement }
}
fn root() -> Parent {
    Parent::Root(())
}
fn in_group(n: u8) -> Parent {
    Parent::Node(NodeRef { node_kind: NodeKind::Layer, node_id: id("SLYR", n) })
}
fn place(parent: Parent, index: u64) -> PropertyValue {
    PropertyValue::Placement(Some(Placement { parent, index, create: None }))
}
/// A session-like holder: snapshot plus revision vector (new placement entries appended, deleted
/// layers' entries kept, seeds merged after each structural change).
struct Doc {
    snap: Snapshot,
    vector: Vec<RevisionEntry>,
    n: u64,
}
impl Doc {
    fn new() -> Self {
        let snap = snapshot();
        let vector = seed_revisions(snap.document());
        Self { snap, vector, n: 0 }
    }
    fn placement(&self, n: u8) -> PropertyValue {
        let d = self.snap.document();
        if !d.layers.iter().any(|l| l.layer_id == id("SLYR", n)) {
            return PropertyValue::Placement(None);
        }
        let shape = shape(d);
        let (parent, index) = shape.iter().find(|(l, _, _)| *l == id("SLYR", n)).map(|(_, p, i)| (p.clone(), *i)).unwrap();
        place(parent, index)
    }
    fn revision(&self, a: &PropertyAddress) -> u64 {
        self.vector.iter().find(|e| &e.address == a).map_or(0, |e| e.revision)
    }
    fn patch(&mut self, writes: Vec<(PropertyAddress, PropertyValue)>) -> Patch {
        self.n += 1;
        Patch {
            transport_version: 1,
            document_id: id("SDOC", 1),
            base_revision: self.snap.document().revision,
            command_id: format!("c{}", self.n),
            correlation_id: self.n,
            actor: actor(),
            reads: writes
                .iter()
                .map(|(a, _)| {
                    let n = u8::from_str_radix(&a.target.node_id[a.target.node_id.len() - 2..], 16).unwrap();
                    PropertyRead { address: a.clone(), expected_revision: self.revision(a), expected_value: self.placement(n) }
                })
                .collect(),
            writes: writes.into_iter().map(|(address, value)| PropertyWrite { address, value }).collect(),
        }
    }
    fn prepare(&self, p: &Patch) -> Result<Prepared, Diagnostic> {
        prepare(p, &self.snap, &self.vector, Budget::default(), &CancellationToken::default(), &Unresolved)
    }
    fn apply(&mut self, p: &Patch) -> Inverse {
        let prepared = self.prepare(p).unwrap_or_else(|e| panic!("{e:?}"));
        let successor = prepared.successor().expect("changed").clone();
        // Lineage as the session checks it: equal after masking every declared address.
        let (mut base, mut next) = (self.snap.document().clone(), successor.document().clone());
        base.revision = next.revision;
        for u in prepared.revision_updates() {
            assert!(mask_property(&mut base, &u.address) && mask_property(&mut next, &u.address));
        }
        assert_eq!(base, next, "successor carries undeclared changes");
        for u in prepared.revision_updates() {
            match self.vector.iter_mut().find(|e| e.address == u.address) {
                Some(e) => {
                    assert_eq!(e.revision, u.previous_revision);
                    e.revision = u.revision;
                }
                None => {
                    assert_eq!(u.previous_revision, 0);
                    self.vector.push(RevisionEntry { address: u.address.clone(), revision: u.revision });
                }
            }
        }
        for e in seed_revisions(successor.document()) {
            if !self.vector.iter().any(|v| v.address == e.address) {
                self.vector.push(e);
            }
        }
        self.snap = successor;
        prepared.inverse().unwrap().clone()
    }
    /// Applies an inverse as a history does: later steps were undone first, which advanced the
    /// revisions the inverse quotes, so its reads are refreshed to the current vector while their
    /// expected values stay (DESCRIPTOR recovery: invert only when the conditional reads still hold).
    fn undo(&mut self, inverse: &Inverse) -> Inverse {
        self.n += 1;
        let mut p = inverse.invocation(format!("u{}", self.n), self.n, actor(), self.snap.document().revision);
        for read in &mut p.reads {
            read.expected_revision = self.revision(&read.address);
        }
        self.apply(&p)
    }
}
/// (layer, parent, stack index) of every stacked layer, sorted by layer id.
fn shape(d: &hsk_studio_folio::StudioDocument) -> Vec<(String, Parent, u64)> {
    let mut out = Vec::new();
    for l in &d.layers {
        let port = d.graph.operations.iter().find(|o| o.layer_id == l.layer_id).map(|o| (o.operation_key.clone(), o.outputs[0].port_key.clone()));
        let Some((op, port)) = port else { continue };
        let index = d.graph.outputs.iter().position(|o| o.layer_id == l.layer_id && o.operation_key == op && o.port_key == port).map(|i| i as u64).or_else(|| {
            d.graph.edges.iter().find(|e| e.source.layer_id == l.layer_id).map(|e| e.input_ordinal)
        });
        out.push((l.layer_id.clone(), l.parent.clone(), index.unwrap()));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}
/// Shape plus every layer record (order-free): what an exact undo must restore.
type Full = (Vec<(String, Parent, u64)>, Vec<String>, Vec<String>);
fn full(d: &hsk_studio_folio::StudioDocument) -> Full {
    let mut layers: Vec<String> = d.layers.iter().map(|l| serde_json::to_string(l).unwrap()).collect();
    let mut ops: Vec<String> = d.graph.operations.iter().map(|o| serde_json::to_string(o).unwrap()).collect();
    layers.sort();
    ops.sort();
    (shape(d), layers, ops)
}
fn new_raster(n: u8, parent: Parent, index: u64) -> PropertyValue {
    let pj = match &parent {
        Parent::Root(()) => Value::Null,
        Parent::Node(r) => json!({"node_kind":"layer","node_id":r.node_id}),
    };
    let layer: StudioLayer = serde_json::from_value(layer_json(n, "raster", pj)).unwrap();
    let operation: Operation = serde_json::from_value(op_json(n, "raster")).unwrap();
    PropertyValue::Placement(Some(Placement { parent, index, create: Some(Box::new(LayerCreate { layer, operation })) }))
}

#[test]
fn create_reorder_delete_and_undo_restore_the_stack() {
    let mut doc = Doc::new();
    let original = full(doc.snap.document());
    // Seeded: every stacked layer has a placement entry at 0.
    for n in 1..=5 {
        assert!(doc.vector.iter().any(|e| e.address == at(n) && e.revision == 0));
    }
    // Create L6 at root index 1 (between L1 and L2): never seen, so revision 0, no vector entry.
    let p = doc.patch(vec![(at(6), new_raster(6, root(), 1))]);
    let undo_create = doc.apply(&p);
    assert_eq!(doc.placement(6), place(root(), 1));
    assert_eq!(doc.placement(2), place(root(), 2), "siblings above move up");
    assert_eq!(doc.revision(&at(6)), 1);
    assert!(doc.vector.iter().any(|e| e.address.target.node_id == id("SLYR", 6) && e.address.property == Property::Name), "created layer's properties are seeded");
    // Reorder L1 to the top of the root stack (index 4 of 5 after its own removal).
    let p = doc.patch(vec![(at(1), place(root(), 4))]);
    let undo_move = doc.apply(&p);
    assert_eq!(doc.placement(1), place(root(), 4));
    assert_eq!(doc.placement(6), place(root(), 0));
    // Delete L2: the inverse carries its record and operation.
    let p = doc.patch(vec![(at(2), PropertyValue::Placement(None))]);
    let undo_delete = doc.apply(&p);
    assert_eq!(doc.placement(2), PropertyValue::Placement(None));
    let PropertyValue::Placement(Some(restore)) = &undo_delete.writes[0].value else { panic!("delete inverse") };
    assert!(restore.create.as_ref().is_some_and(|c| c.layer.layer_id == id("SLYR", 2)));
    // Undo in reverse order restores the original stack and records exactly.
    doc.undo(&undo_delete);
    doc.undo(&undo_move);
    doc.undo(&undo_create);
    assert_eq!(full(doc.snap.document()), original);
    // L6's entries survive its deletion (tombstones): a redo-style re-create starts from them.
    assert_eq!(doc.revision(&at(6)), 2);
}

#[test]
fn group_and_ungroup_apply_in_order_and_invert_exactly() {
    let mut doc = Doc::new();
    let original = full(doc.snap.document());
    // Group L2 and L3 into a new group G7 at root index 1, in one transaction: create the group,
    // then move each child in (each index measured after the previous write).
    let group: StudioLayer = serde_json::from_value(layer_json(7, "group", Value::Null)).unwrap();
    let mix: Operation = serde_json::from_value(op_json(7, "group")).unwrap();
    let create_group = PropertyValue::Placement(Some(Placement { parent: root(), index: 1, create: Some(Box::new(LayerCreate { layer: group, operation: mix })) }));
    let p = doc.patch(vec![(at(7), create_group), (at(2), place(in_group(7), 0)), (at(3), place(in_group(7), 1))]);
    let undo_group = doc.apply(&p);
    assert_eq!(doc.placement(7), place(root(), 1));
    assert_eq!(doc.placement(2), place(in_group(7), 0));
    assert_eq!(doc.placement(3), place(in_group(7), 1));
    assert_eq!(doc.placement(4), place(root(), 2));
    // Ungroup: move the children out where the group was, then delete the group.
    let p = doc.patch(vec![(at(3), place(root(), 2)), (at(2), place(root(), 2)), (at(7), PropertyValue::Placement(None))]);
    let undo_ungroup = doc.apply(&p);
    assert_eq!(full(doc.snap.document()).0, original.0, "ungroup restores the original stack");
    doc.undo(&undo_ungroup);
    assert_eq!(doc.placement(2), place(in_group(7), 0));
    doc.undo(&undo_group);
    assert_eq!(full(doc.snap.document()), original);
    // Sequential semantics: moving two layers where the first move shifts the second's index.
    let mut doc = Doc::new();
    let p = doc.patch(vec![(at(3), place(root(), 0)), (at(1), place(root(), 3))]);
    let inverse = doc.apply(&p);
    doc.undo(&inverse);
    assert_eq!(full(doc.snap.document()), original);
}

#[test]
fn hostile_structure_edits_are_typed_and_change_nothing() {
    let mut doc = Doc::new();
    let before = doc.snap.encoded_bytes().to_vec();
    let refuse = |doc: &mut Doc, writes: Vec<(PropertyAddress, PropertyValue)>, code: Code| {
        let p = doc.patch(writes);
        assert_eq!(doc.prepare(&p).unwrap_err().code, code, "{p:?}");
    };
    // Index past the end of the stack.
    refuse(&mut doc, vec![(at(1), place(root(), 9))], Code::InvalidTarget);
    refuse(&mut doc, vec![(at(6), new_raster(6, root(), 99))], Code::InvalidTarget);
    // A create whose record names another layer, or a move that carries a record.
    refuse(&mut doc, vec![(at(6), new_raster(8, root(), 0))], Code::InvalidInput);
    let PropertyValue::Placement(Some(mut carried)) = new_raster(1, root(), 3) else { unreachable!() };
    carried.index = 3;
    refuse(&mut doc, vec![(at(1), PropertyValue::Placement(Some(carried)))], Code::InvalidInput);
    // Creating without a record.
    refuse(&mut doc, vec![(at(6), place(root(), 0))], Code::InvalidInput);
    // Parent that is not a group (a raster layer), or a group moved into its own child.
    refuse(&mut doc, vec![(at(1), place(in_group(2), 0))], Code::InvalidTarget);
    refuse(&mut doc, vec![(at(1), place(in_group(9), 0))], Code::InvalidTarget);
    // Deleting a group that still has children leaves a dangling parent: Folio refuses it.
    refuse(&mut doc, vec![(at(4), PropertyValue::Placement(None))], Code::FolioRejected);
    // A read may not carry a create record.
    let mut p = doc.patch(vec![(at(6), new_raster(6, root(), 0))]);
    p.reads[0].expected_value = new_raster(6, root(), 0);
    assert_eq!(doc.prepare(&p).unwrap_err().code, Code::WrongType);
    // A stale expected placement conflicts.
    let mut p = doc.patch(vec![(at(1), place(root(), 2))]);
    p.reads[0].expected_value = place(root(), 1);
    assert_eq!(doc.prepare(&p).unwrap_err().code, Code::ValueConflict);
    // A layer the document holds but the vector does not name is unavailable (no default zero).
    let p = doc.patch(vec![(at(1), place(root(), 2))]);
    doc.vector.retain(|e| e.address != at(1));
    assert_eq!(doc.prepare(&p).unwrap_err().code, Code::RevisionUnavailable);
    assert_eq!(doc.snap.encoded_bytes(), before.as_slice());
    // The wire form decodes through the bounded decoder (tile lists inside a create record).
    let p = Doc::new().patch(vec![(at(6), new_raster(6, root(), 0))]);
    let bytes = serde_json::to_vec(&p).unwrap();
    assert_eq!(decode_patch(&bytes, Budget::default(), &CancellationToken::default()).unwrap(), p);
}
