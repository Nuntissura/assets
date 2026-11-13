//! Layer management in the viewer (lane SMB-HARNESS-UI-3): new raster layer, duplicate, delete,
//! reorder, rename, group and ungroup. Every action is one chronicle transaction of `placement`
//! writes through the session ([`crate::Run::restructure`]), so it is undoable as one step and
//! re-validated by Folio before it is published. The stack is Folio's graph: index 0 is the
//! bottom-most input of the parent (a group's composite inputs, or the document's graph outputs);
//! the layer panel lists it top-first.
use super::Viewer;
use crate::{Place, PlacementWrite};
use hsk_studio_folio::{NodeKind, NodeRef, Operation, Parent, Payload, StudioDocument, StudioLayer};
use serde_json::{Value, json};
use std::collections::BTreeSet;

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

/// Refusal code of a layer id mint that would leave the 48-bit counter ([`try_mint_layer_id`]).
pub(super) const LAYER_ID_SPACE_EXHAUSTED: &str = "layer_id_space_exhausted";
/// One past the largest value each 24-bit half of the counter holds.
const ID_HALF: u64 = 0x100_0000;

/// A fresh `SLYR` id: the document id's time fields, a fixed version/variant block and a 48-bit counter
/// (`revision * 2^24 + low`, the id's last group of 12 hex digits) whose upper 24 bits are the document revision
/// the id is minted at and whose lower 24 bits are the first value from `salt` up the document does not hold
/// (never an id the document holds).
///
/// Lane SMB-PS-FILTER-ADJUST-COMMANDS-1 (lead ruling 2026-11-13): an id is never minted again, so a layer
/// created after a delete never takes the deleted layer's id and never inherits a record other entries still
/// key by it (the `raster_records` smart filter stack or adjustment, vector and design records; this function
/// sees only the document, so the revision stands in for "every id ever seen"). A layer exists only through a
/// chronicle commit, which moves the revision past the one its id was minted at; revisions only grow (an undo
/// is a new revision) and a saved document reopens at its revision, so every later mint carries a higher
/// revision. Ids of the earlier scheme have revision bits 0 and stay distinct from every mint at revision 1 or
/// later. Nothing wraps or is truncated: a revision of 2^24 or more, a `salt` of 2^24 or more, or lower bits
/// that run out refuse [`LAYER_ID_SPACE_EXHAUSTED`]. An id is a string in Folio and the package; the tests prove the
/// largest value parses as a `DomainId` and survives save, reopen and an `export.run` that names it (unit test
/// `the_counter_carries_the_revision_and_the_largest_values_are_valid_ids`, harness test
/// `minted_layer_ids_are_never_minted_twice_and_survive_save_reopen_and_export`); other writers were read, not run.
pub(super) fn try_mint_layer_id(doc: &StudioDocument, salt: u64) -> Result<String, &'static str> {
    try_mint_block_id(doc, "9000", salt, &|_| false)
}

/// [`try_mint_layer_id`] over the id block `7ede-<block>` (`9000` the layer tools, `9200` placed layers, `9300`
/// adjustment layers; lane SMB-PS-FILTER-ADJUST-COMMANDS-1, review F2): the revision scheme, skipping the ids the
/// document holds and the ids `taken` names (a caller's second guard, such as the entry's records).
pub(super) fn try_mint_block_id(doc: &StudioDocument, block: &str, salt: u64, taken: &dyn Fn(&str) -> bool) -> Result<String, &'static str> {
    let uuid = doc.document_id.split_once('-').map_or("", |(_, u)| u);
    let (high, low) = (uuid.get(0..8).unwrap_or("00000000"), uuid.get(9..13).unwrap_or("0000"));
    if doc.revision >= ID_HALF {
        return Err(LAYER_ID_SPACE_EXHAUSTED);
    }
    let base = doc.revision.checked_mul(ID_HALF).ok_or(LAYER_ID_SPACE_EXHAUSTED)?;
    let mut at = salt;
    loop {
        if at >= ID_HALF {
            return Err(LAYER_ID_SPACE_EXHAUSTED);
        }
        let n = base.checked_add(at).ok_or(LAYER_ID_SPACE_EXHAUSTED)?;
        let id = format!("SLYR-{high}-{low}-7ede-{block}-{n:012x}");
        if !doc.layers.iter().any(|l| l.layer_id == id) && !taken(&id) {
            return Ok(id);
        }
        at = at.checked_add(1).ok_or(LAYER_ID_SPACE_EXHAUSTED)?;
    }
}

/// [`try_mint_layer_id`] for the callers that take an id unconditionally (text, vector, design). When the space
/// is exhausted it returns `layer_id_space_exhausted-<salt>`, which is not an `SLYR` id, so the commit that would
/// create the layer refuses (Folio `InvalidId` naming it): never a wrapped or truncated id, and distinct per
/// `salt`, so a caller looping past ids it already took ends. This module's own creators refuse typed first.
pub(super) fn mint_layer_id(doc: &StudioDocument, salt: u64) -> String {
    try_mint_layer_id(doc, salt).unwrap_or_else(|code| format!("{code}-{salt}"))
}

/// [`try_mint_layer_id`] as this module's refusal text (`layer_id_space_exhausted: ...`).
fn mint_or_refuse(doc: &StudioDocument, salt: u64) -> Result<String, String> {
    try_mint_layer_id(doc, salt).map_err(|code| format!("{code}: no fresh layer id at revision {} (salt {salt})", doc.revision))
}

/// The graph operation of a new leaf (image source) or group (ordered composite).
pub(super) fn operation_for(layer_id: &str, group: bool) -> Result<Operation, String> {
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

pub(super) fn new_layer(layer_id: &str, name: &str, parent: &Parent, kind: &str, payload: Value) -> Result<StudioLayer, String> {
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

    pub(super) fn structure(&mut self, op: &str, writes: Vec<PlacementWrite>, what: String) -> Result<u64, String> {
        let ok = self.run.restructure(op, &writes);
        self.after_edit(ok, what)
    }

    /// Where a new layer goes: directly above the selected layer in its stack, else the top of the
    /// document root (the parent the new record names, and the placement command).
    pub(super) fn insertion_point(&self) -> Result<(Parent, Place), String> {
        self.doc()?;
        if let Some(sel) = self.selected.as_deref()
            && let Some((parent, _)) = self.run.placement(sel)
        {
            return Ok((parent, Place::Before(sel.to_string())));
        }
        Ok((Parent::Root(()), Place::AtBeginning(None)))
    }

    /// A new, empty raster layer above the selected layer (painting creates its tiles). Selects it
    /// and returns its id.
    pub fn new_raster_layer(&mut self, name: &str) -> Result<String, String> {
        let (parent, place) = self.insertion_point()?;
        let doc = self.doc()?;
        let id = mint_or_refuse(doc, u64::try_from(doc.layers.len()).unwrap_or(0))?;
        let layer = new_layer(&id, name, &parent, "raster", json!({"payload_kind": "raster", "tiles": []}))?;
        let write = PlacementWrite { layer_id: id.clone(), to: Some(place), create: Some((layer, operation_for(&id, false)?)) };
        self.structure("new_layer", vec![write], format!("new raster layer {id}"))?;
        self.selected = Some(id.clone());
        Ok(id)
    }

    /// A copy of `layer_id` (a group with its whole subtree) just above it: new ids, the same tile
    /// bodies (by digest), name + " copy". Selects the copy and returns its id.
    pub fn duplicate_layer(&mut self, layer_id: &str) -> Result<String, String> {
        let doc = self.doc()?.clone();
        let (parent, _) = self.run.placement(layer_id).ok_or_else(|| format!("duplicate: {layer_id} is not in a layer stack"))?;
        let mut writes = Vec::new();
        let mut salt = u64::try_from(doc.layers.len()).unwrap_or(0);
        let copy = self.copy_subtree(&doc, layer_id, parent, Place::Before(layer_id.to_string()), true, &mut salt, &mut writes)?;
        let what = format!("duplicated {layer_id} as {copy}");
        // Lane SMB-DS-COMPONENTS-AUTHOR-1 (F10): a copied instance is an instance of the same main.
        if self.duplicate_instances(layer_id, &writes, &what)?.is_none() {
            self.structure("duplicate_layer", writes, what)?;
        }
        self.selected = Some(copy.clone());
        Ok(copy)
    }

    #[allow(clippy::too_many_arguments)]
    fn copy_subtree(&self, doc: &StudioDocument, source: &str, parent: Parent, place: Place, top: bool, salt: &mut u64, writes: &mut Vec<PlacementWrite>) -> Result<String, String> {
        let layer = doc.layers.iter().find(|l| l.layer_id == source).ok_or_else(|| format!("duplicate: unknown layer {source}"))?;
        if matches!(layer.payload, Payload::Unsupported { .. }) {
            return Err(format!("duplicate: {source} has an unsupported payload (read-only)"));
        }
        // Distinct from every id the document holds and from the copies minted so far.
        let mut id = mint_or_refuse(doc, *salt)?;
        while writes.iter().any(|w| w.layer_id == id) {
            *salt = salt.saturating_add(1);
            id = mint_or_refuse(doc, *salt)?;
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
        writes.push(PlacementWrite { layer_id: id.clone(), to: Some(place), create: Some((copy, op)) });
        if is_group(layer) {
            // Bottom-first, each to the top of the copy: the stack order is kept.
            let inner = parent_of(Some(&id));
            for child in stack(doc, Some(source)) {
                self.copy_subtree(doc, &child, inner.clone(), Place::Inside(id.clone()), false, salt, writes)?;
            }
        }
        Ok(id)
    }

    /// The layer ids of `layer_id`'s subtree (itself and every layer whose parent chain reaches it), from the
    /// parent links; bounded: one pass over the layers, then each layer is entered once.
    fn subtree(doc: &StudioDocument, layer_id: &str) -> Vec<String> {
        let mut children: std::collections::BTreeMap<&str, Vec<&str>> = std::collections::BTreeMap::new();
        for l in &doc.layers {
            if let Some(p) = parent_id(&l.parent) {
                children.entry(p).or_default().push(l.layer_id.as_str());
            }
        }
        let mut out: Vec<String> = Vec::new();
        let mut seen = BTreeSet::new();
        let mut stack = vec![layer_id];
        while let Some(id) = stack.pop() {
            if seen.insert(id) {
                stack.extend(children.get(id).into_iter().flatten().copied());
                out.push(id.to_string());
            }
        }
        out
    }

    /// `layers` hold a text frame, or the stored layout records name one of them (a graphic frame, a parent
    /// page item, an override's replacement, a flow record): the delete is a layout cascade. Records that do
    /// not decode name nothing here (a text frame still goes to the cascade, which refuses them typed).
    fn layout_bound(&self, doc: &StudioDocument, layers: &[String]) -> bool {
        fn names(v: &Value, ids: &BTreeSet<&str>) -> bool {
            match v {
                Value::String(s) => ids.contains(s.as_str()),
                Value::Array(items) => items.iter().any(|x| names(x, ids)),
                Value::Object(map) => map.values().any(|x| names(x, ids)),
                _ => false,
            }
        }
        let ids: BTreeSet<&str> = layers.iter().map(String::as_str).collect();
        if doc.layers.iter().any(|l| ids.contains(l.layer_id.as_str()) && l.text_frame.is_some()) {
            return true;
        }
        let Some(bytes) = self.run.document_entry(hsk_studio_session::LAYOUT_RECORDS_ENTRY) else { return false };
        let Ok(r) = hsk_studio_session::LayoutRecords::decode(bytes, hsk_studio_folio::Budget::default(), self.cancel) else { return false };
        [serde_json::to_value(&r.flow), serde_json::to_value(&r.longdoc), serde_json::to_value(&r.parents)].iter().any(|v| v.as_ref().is_ok_and(|v| names(v, &ids)))
    }

    /// Deletes `layer_id` (a group with everything inside it) as one undoable step.
    ///
    /// Layout layers (lane SMB-LAYOUT-DELETE-CASCADE-1, replacing lane SMB-R60-FOLLOWUPS-1's typed refusal
    /// `layer_bound_by_layout_records` for a threaded frame and lane SMB-LAYOUT-PLACE-1's graphic-frame delete): a
    /// text frame, a graphic frame, or a layer whose subtree holds one or that layout records name goes through
    /// `Run::delete_layout_layers`, ONE layout step: the thread closes over a removed frame (the frame before it
    /// continues into the frame after it, [STU-LAY-122]), a story no frame shows any more goes with its records,
    /// the records naming a removed layer go (harness-core `cascade_layout_delete`), and, as for every delete, the
    /// component and instance records keyed to it (`Viewer::records_after_delete`). Undo restores the layers, the
    /// thread and the records together; the step runs under the row `layer.delete_layout_layers`. Refused typed,
    /// nothing changed: `layout_records_unreadable`, `layout_records_stale`, `layout_records_missing` (component
    /// records to drop in a document without layout records), and `layer_bound_by_layout_records` when records the
    /// cascade does not follow would still name a removed layer.
    /// The raster records entry keeps a removed picture's placement record, inert until an undo brings the layer
    /// back (as for every deleted placement).
    pub fn delete_layer(&mut self, layer_id: &str) -> Result<u64, String> {
        let doc = self.doc()?;
        if !doc.layers.iter().any(|l| l.layer_id == layer_id) {
            return Err(format!("delete: {layer_id} is not in a layer stack"));
        }
        let gone = Self::subtree(doc, layer_id);
        if self.layout_bound(doc, &gone) {
            let entry = self.records_after_delete(&gone).map(|bytes| (super::vector::ENTRY, bytes));
            let ok = self.run.delete_layout_layers("delete_layer", "layer.delete_layout_layers", &[layer_id.to_string()], entry);
            let revision = self.after_edit(ok, format!("deleted {layer_id}"))?;
            if self.selected.as_ref().is_some_and(|s| gone.contains(s)) {
                self.selected = None;
            }
            self.place_sync();
            self.layout_sync();
            return Ok(revision);
        }
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
        let writes: Vec<PlacementWrite> = order.iter().map(|id| PlacementWrite { layer_id: id.clone(), to: None, create: None }).collect();
        // Lane SMB-DS-COMPONENTS-AUTHOR-1 (F5): the component and instance records keyed to a deleted
        // layer go in the same undo step.
        let revision = match self.records_after_delete(&order) {
            Some(bytes) => {
                let ok = self.run.restructure_and_entry("delete_layer", &writes, super::vector::ENTRY, bytes);
                self.after_edit(ok, format!("deleted {layer_id}"))?
            }
            None => self.structure("delete_layer", writes, format!("deleted {layer_id}"))?,
        };
        if self.selected.as_deref() == Some(layer_id) {
            self.selected = None;
        }
        Ok(revision)
    }

    /// Moves `layer_id` by an enumerated placement ([STU-RAS-114]).
    pub fn move_layer(&mut self, layer_id: &str, place: Place) -> Result<u64, String> {
        let what = format!("moved {layer_id} {place:?}");
        let write = PlacementWrite { layer_id: layer_id.to_string(), to: Some(place), create: None };
        self.structure("move_layer", vec![write], what)
    }

    /// One step up (`up`: directly above the sibling above) or down (directly below the sibling
    /// below) in its own stack.
    pub fn shift_layer(&mut self, layer_id: &str, up: bool) -> Result<u64, String> {
        let (parent, index) = self.run.placement(layer_id).ok_or_else(|| format!("reorder: {layer_id} is not in a layer stack"))?;
        let siblings = stack(self.doc()?, parent_id(&parent));
        let index = usize::try_from(index).map_err(|_| "reorder: index")?;
        let place = if up {
            Place::Before(siblings.get(index.saturating_add(1)).cloned().ok_or("reorder: already at the top")?)
        } else {
            Place::After(index.checked_sub(1).and_then(|i| siblings.get(i)).cloned().ok_or("reorder: already at the bottom")?)
        };
        self.move_layer(layer_id, place)
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
        let top = placed.last().map(|(id, _)| id.clone()).ok_or("group: no layers")?;
        let doc = self.doc()?;
        let gid = mint_or_refuse(doc, u64::try_from(doc.layers.len()).unwrap_or(0))?;
        let group = new_layer(&gid, name, &parent, "group", json!({"payload_kind": "group"}))?;
        // The group enters directly above the topmost member; the members then move in
        // bottom-first, each to the top of the group (their order is kept).
        let mut writes = vec![PlacementWrite { layer_id: gid.clone(), to: Some(Place::Before(top)), create: Some((group, operation_for(&gid, true)?)) }];
        for (id, _) in &placed {
            writes.push(PlacementWrite { layer_id: id.clone(), to: Some(Place::Inside(gid.clone())), create: None });
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
        self.run.placement(group_id).ok_or_else(|| format!("ungroup: {group_id} is not in a layer stack"))?;
        let children = stack(doc, Some(group_id));
        // Top child first, each directly above the group: the order is kept, then the group goes.
        let mut writes: Vec<PlacementWrite> = children
            .iter()
            .rev()
            .map(|id| PlacementWrite { layer_id: id.clone(), to: Some(Place::Before(group_id.to_string())), create: None })
            .collect();
        writes.push(PlacementWrite { layer_id: group_id.to_string(), to: None, create: None });
        let revision = self.structure("ungroup", writes, format!("ungrouped {group_id}"))?;
        if self.selected.as_deref() == Some(group_id) {
            self.selected = children.last().cloned();
        }
        Ok(revision)
    }
}

#[cfg(test)]
mod tests {
    //! The layer id mint of the lead ruling 2026-11-13 (lane SMB-PS-FILTER-ADJUST-COMMANDS-1): conditions (1) no
    //! silent wrap, (2) the combined value is a valid id, (3) one revision mints distinct ids, (6) ids of the
    //! earlier scheme stay valid and distinct. (4) and (5) run through the viewer (harness tests/viewer_raster_effects.rs).
    use super::*;

    const DOC: &str = "SDOC-019abcde-0000-7000-8000-000000000001";

    fn old_id(n: u64) -> String {
        format!("SLYR-019abcde-0000-7ede-9000-{n:012x}")
    }

    fn document(revision: u64, ids: &[String]) -> StudioDocument {
        let layers: Vec<Value> = ids
            .iter()
            .map(|id| json!({"schema_id":"hsk.studio.layer@1","layer_id":id,"name":id,"parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}))
            .collect();
        serde_json::from_value(json!({
            "schema_id":"hsk.studio.document@1","document_id":DOC,"revision":revision,"geometry_unit":"px",
            "colour_mode":3,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,
            "working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],"layers":layers,
            "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
        }))
        .expect("document shape")
    }

    fn counter(id: &str) -> u64 {
        u64::from_str_radix(id.rsplit('-').next().unwrap(), 16).unwrap()
    }

    #[test]
    fn the_counter_carries_the_revision_and_the_largest_values_are_valid_ids() {
        let id = try_mint_layer_id(&document(5, &[]), 3).unwrap();
        assert_eq!(counter(&id), 5 * ID_HALF + 3, "{id}");
        // (2) the largest combined value fills the 12 hex digits and is a valid SLYR id.
        let top = try_mint_layer_id(&document(ID_HALF - 1, &[]), ID_HALF - 1).unwrap();
        assert_eq!(counter(&top), (1 << 48) - 1, "{top}");
        assert!(top.ends_with("-ffffffffffff"), "{top}");
        let parsed = hsk_studio_accord::DomainId::parse(&top).expect("a valid SLYR id");
        assert_eq!(parsed.prefix(), "SLYR");
    }

    #[test]
    fn an_exhausted_space_refuses_typed_and_never_wraps() {
        // (1) A revision of 2^24 or more refuses; no id of another revision is returned.
        assert_eq!(try_mint_layer_id(&document(ID_HALF, &[]), 0), Err(LAYER_ID_SPACE_EXHAUSTED));
        assert_eq!(try_mint_layer_id(&document(u64::MAX, &[]), 0), Err(LAYER_ID_SPACE_EXHAUSTED));
        // A salt of 2^24 or more refuses (never taken modulo 2^24).
        assert_eq!(try_mint_layer_id(&document(1, &[]), ID_HALF), Err(LAYER_ID_SPACE_EXHAUSTED));
        // Lower bits that run out refuse instead of carrying into the next revision's ids.
        let last = format!("SLYR-019abcde-0000-7ede-9000-{:012x}", 2 * ID_HALF + (ID_HALF - 1));
        assert_eq!(try_mint_layer_id(&document(2, &[last]), ID_HALF - 1), Err(LAYER_ID_SPACE_EXHAUSTED));
        // The unconditional form returns a non-SLYR text a commit refuses, distinct per salt.
        let a = mint_layer_id(&document(ID_HALF, &[]), 7);
        let b = mint_layer_id(&document(ID_HALF, &[]), 8);
        assert_eq!(a, "layer_id_space_exhausted-7");
        assert_ne!(a, b);
        assert!(hsk_studio_accord::DomainId::parse(&a).is_err());
        // This module's creators (new layer, duplicate, group) refuse with the code as the leading token.
        let refusal = mint_or_refuse(&document(ID_HALF, &[]), 0).unwrap_err();
        assert_eq!(super::super::commands::leading_code(&refusal).as_deref(), Some(LAYER_ID_SPACE_EXHAUSTED), "{refusal}");
    }

    #[test]
    fn one_revision_mints_distinct_ids_from_successive_salts() {
        // (3) Callers minting several layers in one transaction pass growing salts (or skip ids they hold).
        let doc = document(9, &[]);
        let ids: Vec<String> = (0..4).map(|s| try_mint_layer_id(&doc, s).unwrap()).collect();
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 4, "{ids:?}");
        assert!(ids.iter().all(|id| counter(id) >> 24 == 9), "{ids:?}");
        // An id the document holds is skipped.
        let held = document(9, &[ids[0].clone()]);
        assert_eq!(try_mint_layer_id(&held, 0).unwrap(), ids[1]);
    }

    #[test]
    fn ids_of_the_earlier_scheme_stay_valid_and_distinct() {
        // (6) A package of the earlier scheme: counters 0..=5 (revision bits 0), saved at revision 3.
        let old: Vec<String> = (0..6).map(old_id).collect();
        for id in &old {
            assert!(hsk_studio_accord::DomainId::parse(id).is_ok(), "{id}");
        }
        let doc = document(3, &old);
        for salt in 0..8 {
            let id = try_mint_layer_id(&doc, salt).unwrap();
            assert!(!old.contains(&id), "{id} reuses an id of the earlier scheme");
            assert_eq!(counter(&id) >> 24, 3);
        }
        // Even at revision 0 the document's own ids are never minted.
        let fresh = try_mint_layer_id(&document(0, &old), 0).unwrap();
        assert!(!old.contains(&fresh), "{fresh}");
    }
}
