//! Layer management in the viewer (lane SMB-HARNESS-UI-3): new raster layer, duplicate, delete,
//! reorder, rename, group and ungroup. Every action is one chronicle transaction of `placement`
//! writes through the session ([`crate::Run::restructure`]), so it is undoable as one step and
//! re-validated by Folio before it is published. The stack is Folio's graph: index 0 is the
//! bottom-most input of the parent (a group's composite inputs, or the document's graph outputs);
//! the layer panel lists it top-first.
use super::Viewer;
use crate::PlacementWrite;
use hsk_studio_folio::{NodeKind, NodeRef, Operation, Parent, Payload, StudioDocument, StudioLayer};
use serde_json::{Value, json};

/// The parent of a stack: the document root or a group layer.
fn parent_of(group: Option<&str>) -> Parent {
    match group {
        None => Parent::Root(()),
        Some(id) => Parent::Node(NodeRef { node_kind: NodeKind::Layer, node_id: id.to_string() }),
    }
}

fn parent_id(parent: &Parent) -> Option<&str> {
    match parent {
        Parent::Root(()) => None,
        Parent::Node(n) => Some(n.node_id.as_str()),
    }
}

fn is_group(layer: &StudioLayer) -> bool {
    layer.kind == "group" && matches!(layer.payload, Payload::Group)
}

/// Children of a stack in stack order, bottom-most first (stacked layers only).
pub(super) fn stack(doc: &StudioDocument, group: Option<&str>) -> Vec<String> {
    match group {
        None => doc.graph.outputs.iter().map(|o| o.layer_id.clone()).collect(),
        Some(g) => {
            let mut inputs: Vec<(u64, String)> = doc
                .graph
                .edges
                .iter()
                .filter(|e| e.target.layer_id == g)
                .map(|e| (e.input_ordinal, e.source.layer_id.clone()))
                .collect();
            inputs.sort();
            inputs.into_iter().map(|(_, id)| id).collect()
        }
    }
}

/// A fresh `SLYR` id: the document id's time fields, a fixed version/variant block and the first
/// unused counter (never an id the document holds).
fn mint_layer_id(doc: &StudioDocument, salt: u64) -> String {
    let uuid = doc.document_id.split_once('-').map_or("", |(_, u)| u);
    let (high, low) = (uuid.get(0..8).unwrap_or("00000000"), uuid.get(9..13).unwrap_or("0000"));
    let mut n = salt;
    loop {
        let id = format!("SLYR-{high}-{low}-7ede-9000-{n:012x}");
        if !doc.layers.iter().any(|l| l.layer_id == id) {
            return id;
        }
        n = n.wrapping_add(1);
    }
}

/// The graph operation of a new leaf (image source) or group (ordered composite).
fn operation_for(layer_id: &str, group: bool) -> Result<Operation, String> {
    let op = if group {
        json!({"layer_id": layer_id, "operation_key": "mix", "operation_kind": "composite",
            "inputs": [{"port_key": "inputs", "value_type": "image", "cardinality": "many", "semantic": "ordered_composite"}],
            "outputs": [{"port_key": "out", "value_type": "image", "cardinality": "one", "semantic": "composite_result"}]})
    } else {
        json!({"layer_id": layer_id, "operation_key": "source", "operation_kind": "source", "inputs": [],
            "outputs": [{"port_key": "out", "value_type": "image", "cardinality": "one", "semantic": "source"}]})
    };
    serde_json::from_value(op).map_err(|e| format!("operation: {e}"))
}

fn new_layer(layer_id: &str, name: &str, parent: &Parent, kind: &str, payload: Value) -> Result<StudioLayer, String> {
    serde_json::from_value(json!({
        "schema_id": "hsk.studio.layer@1", "layer_id": layer_id, "name": name,
        "parent": serde_json::to_value(parent).unwrap_or(Value::Null),
        "kind": kind, "payload": payload, "visible": true,
    }))
    .map_err(|e| format!("layer record: {e}"))
}

impl Viewer {
    fn doc(&self) -> Result<&StudioDocument, String> {
        self.run.snapshot().map(|s| s.document()).ok_or_else(|| "nothing open".to_string())
    }

    fn structure(&mut self, op: &str, writes: Vec<PlacementWrite>, what: String) -> Result<u64, String> {
        let ok = self.run.restructure(op, &writes);
        self.after_edit(ok, what)
    }

    /// Where a new layer goes: just above the selected layer in its stack, else the top of the root.
    fn insertion_point(&self) -> Result<(Parent, u64), String> {
        let doc = self.doc()?;
        if let Some(sel) = self.selected.as_deref()
            && let Some((parent, index)) = self.run.placement(sel)
        {
            return Ok((parent, index.saturating_add(1)));
        }
        Ok((Parent::Root(()), u64::try_from(doc.graph.outputs.len()).unwrap_or(u64::MAX)))
    }

    /// A new, empty raster layer above the selected layer (painting creates its tiles). Selects it
    /// and returns its id.
    pub fn new_raster_layer(&mut self, name: &str) -> Result<String, String> {
        let (parent, index) = self.insertion_point()?;
        let doc = self.doc()?;
        let id = mint_layer_id(doc, u64::try_from(doc.layers.len()).unwrap_or(0));
        let layer = new_layer(&id, name, &parent, "raster", json!({"payload_kind": "raster", "tiles": []}))?;
        let write = PlacementWrite { layer_id: id.clone(), to: Some((parent, index)), create: Some((layer, operation_for(&id, false)?)) };
        self.structure("new_layer", vec![write], format!("new raster layer {id}"))?;
        self.selected = Some(id.clone());
        Ok(id)
    }

    /// A copy of `layer_id` (a group with its whole subtree) just above it: new ids, the same tile
    /// bodies (by digest), name + " copy". Selects the copy and returns its id.
    pub fn duplicate_layer(&mut self, layer_id: &str) -> Result<String, String> {
        let doc = self.doc()?.clone();
        let (parent, index) = self.run.placement(layer_id).ok_or_else(|| format!("duplicate: {layer_id} is not in a layer stack"))?;
        let mut writes = Vec::new();
        let mut salt = u64::try_from(doc.layers.len()).unwrap_or(0);
        let copy = self.copy_subtree(&doc, layer_id, parent, index.saturating_add(1), true, &mut salt, &mut writes)?;
        self.structure("duplicate_layer", writes, format!("duplicated {layer_id} as {copy}"))?;
        self.selected = Some(copy.clone());
        Ok(copy)
    }

    #[allow(clippy::too_many_arguments)]
    fn copy_subtree(&self, doc: &StudioDocument, source: &str, parent: Parent, index: u64, top: bool, salt: &mut u64, writes: &mut Vec<PlacementWrite>) -> Result<String, String> {
        let layer = doc.layers.iter().find(|l| l.layer_id == source).ok_or_else(|| format!("duplicate: unknown layer {source}"))?;
        if matches!(layer.payload, Payload::Unsupported { .. }) {
            return Err(format!("duplicate: {source} has an unsupported payload (read-only)"));
        }
        // Distinct from every id the document holds and from the copies minted so far.
        let mut id = mint_layer_id(doc, *salt);
        while writes.iter().any(|w| w.layer_id == id) {
            *salt = salt.saturating_add(1);
            id = mint_layer_id(doc, *salt);
        }
        *salt = salt.saturating_add(1);
        let mut value = serde_json::to_value(layer).map_err(|e| format!("duplicate: {e}"))?;
        value["layer_id"] = json!(id);
        value["parent"] = serde_json::to_value(&parent).unwrap_or(Value::Null);
        if top {
            value["name"] = json!(format!("{} copy", layer.name));
        }
        // Tile references (raster and mask) belong to their layer: same bodies, new owner.
        if let Some(tiles) = value.pointer_mut("/payload/tiles").and_then(Value::as_array_mut) {
            for t in tiles {
                let key = t["object_key"].as_str().unwrap_or_default().to_string();
                t["layer_id"] = json!(id);
                t["artifact_manifest_id"] = json!(format!("package/{id}/{key}"));
            }
        }
        if let Some(mask) = value.pointer_mut("/appearance/mask/tiles").and_then(Value::as_array_mut) {
            for t in mask {
                let key = t["object_key"].as_str().unwrap_or_default().to_string();
                t["layer_id"] = json!(id);
                t["artifact_manifest_id"] = json!(format!("package/{id}/mask/{key}"));
            }
        }
        let copy: StudioLayer = serde_json::from_value(value).map_err(|e| format!("duplicate: {e}"))?;
        let op = doc.graph.operations.iter().find(|o| o.layer_id == source).ok_or_else(|| format!("duplicate: {source} has no graph operation"))?;
        let mut op = op.clone();
        op.layer_id.clone_from(&id);
        writes.push(PlacementWrite { layer_id: id.clone(), to: Some((parent, index)), create: Some((copy, op)) });
        if is_group(layer) {
            let inner = parent_of(Some(&id));
            for (k, child) in stack(doc, Some(source)).iter().enumerate() {
                self.copy_subtree(doc, child, inner.clone(), u64::try_from(k).unwrap_or(u64::MAX), false, salt, writes)?;
            }
        }
        Ok(id)
    }

    /// Deletes `layer_id` (a group with everything inside it) as one undoable step.
    pub fn delete_layer(&mut self, layer_id: &str) -> Result<u64, String> {
        let doc = self.doc()?;
        if self.run.placement(layer_id).is_none() {
            return Err(format!("delete: {layer_id} is not in a layer stack"));
        }
        // Children first (top-first, deepest first), then the layer itself.
        fn post_order(doc: &StudioDocument, id: &str, out: &mut Vec<String>) {
            if doc.layers.iter().any(|l| l.layer_id == id && is_group(l)) {
                for child in stack(doc, Some(id)).iter().rev() {
                    post_order(doc, child, out);
                }
            }
            out.push(id.to_string());
        }
        let mut order = Vec::new();
        post_order(doc, layer_id, &mut order);
        let writes = order.into_iter().map(|id| PlacementWrite { layer_id: id, to: None, create: None }).collect();
        let revision = self.structure("delete_layer", writes, format!("deleted {layer_id}"))?;
        if self.selected.as_deref() == Some(layer_id) {
            self.selected = None;
        }
        Ok(revision)
    }

    /// Moves `layer_id` into `group` (`None` = the document root) at stack `index` (0 = bottom-most,
    /// measured after the layer left its old place).
    pub fn move_layer(&mut self, layer_id: &str, group: Option<&str>, index: u64) -> Result<u64, String> {
        let write = PlacementWrite { layer_id: layer_id.to_string(), to: Some((parent_of(group), index)), create: None };
        self.structure("move_layer", vec![write], format!("moved {layer_id} to {} #{index}", group.unwrap_or("root")))
    }

    /// One step up (`up`) or down in its own stack.
    pub fn shift_layer(&mut self, layer_id: &str, up: bool) -> Result<u64, String> {
        let (parent, index) = self.run.placement(layer_id).ok_or_else(|| format!("reorder: {layer_id} is not in a layer stack"))?;
        let len = u64::try_from(stack(self.doc()?, parent_id(&parent)).len()).unwrap_or(0);
        let to = if up { index.saturating_add(1) } else { index.checked_sub(1).ok_or("reorder: already at the bottom")? };
        if to >= len {
            return Err("reorder: already at the top".into());
        }
        let group = parent_id(&parent).map(str::to_string);
        self.move_layer(layer_id, group.as_deref(), to)
    }

    /// Renames a layer (chronicle `name`, undoable).
    pub fn rename_layer(&mut self, layer_id: &str, name: &str) -> Result<u64, String> {
        let ok = self.run.rename(layer_id, name, None);
        self.after_edit(ok, format!("renamed {layer_id}"))
    }

    /// Puts `layers` (siblings of one stack) into a new group at the place of the topmost of them,
    /// keeping their order. Selects the group and returns its id.
    pub fn group_layers(&mut self, layers: &[String], name: &str) -> Result<String, String> {
        let mut placed = Vec::new();
        for id in layers {
            let at = self.run.placement(id).ok_or_else(|| format!("group: {id} is not in a layer stack"))?;
            placed.push((id.clone(), at));
        }
        let Some((_, (parent, _))) = placed.first().cloned() else {
            return Err("group: no layers".into());
        };
        if placed.iter().any(|(_, (p, _))| *p != parent) {
            return Err("group: the layers are not siblings of one stack".into());
        }
        placed.sort_by_key(|(_, (_, i))| *i);
        let top = placed.last().map_or(0, |(_, (_, i))| *i);
        let doc = self.doc()?;
        let gid = mint_layer_id(doc, u64::try_from(doc.layers.len()).unwrap_or(0));
        let group = new_layer(&gid, name, &parent, "group", json!({"payload_kind": "group"}))?;
        // The group enters just above the topmost member; the members then move in bottom-first.
        let mut writes = vec![PlacementWrite { layer_id: gid.clone(), to: Some((parent, top.saturating_add(1))), create: Some((group, operation_for(&gid, true)?)) }];
        for (k, (id, _)) in placed.iter().enumerate() {
            writes.push(PlacementWrite { layer_id: id.clone(), to: Some((parent_of(Some(&gid)), u64::try_from(k).unwrap_or(u64::MAX))), create: None });
        }
        self.structure("group_layers", writes, format!("grouped {} layer(s) into {gid}", placed.len()))?;
        self.selected = Some(gid.clone());
        Ok(gid)
    }

    /// Moves the group's children into its parent where the group was (same order) and deletes the
    /// group (its own appearance goes with it).
    pub fn ungroup(&mut self, group_id: &str) -> Result<u64, String> {
        let doc = self.doc()?;
        if !doc.layers.iter().any(|l| l.layer_id == group_id && is_group(l)) {
            return Err(format!("ungroup: {group_id} is not a group"));
        }
        let (parent, index) = self.run.placement(group_id).ok_or_else(|| format!("ungroup: {group_id} is not in a layer stack"))?;
        let children = stack(doc, Some(group_id));
        let mut writes: Vec<PlacementWrite> = children
            .iter()
            .enumerate()
            .map(|(k, id)| PlacementWrite { layer_id: id.clone(), to: Some((parent.clone(), index.saturating_add(1).saturating_add(u64::try_from(k).unwrap_or(u64::MAX)))), create: None })
            .collect();
        writes.push(PlacementWrite { layer_id: group_id.to_string(), to: None, create: None });
        let revision = self.structure("ungroup", writes, format!("ungrouped {group_id}"))?;
        if self.selected.as_deref() == Some(group_id) {
            self.selected = children.last().cloned();
        }
        Ok(revision)
    }
}
