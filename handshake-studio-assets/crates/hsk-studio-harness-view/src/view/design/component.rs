//! Component, variant-set, component-property and instance authoring (lane SMB-DS-COMPONENTS-AUTHOR-1;
//! [STU-DS-002/003/003a/004/006], [STU-DS-102..113], [STU-DS-162]): the spec's typed commands
//! ([`ComponentCommand`], ids `component.*`, `component_set.*`, `instance.*`) over Folio's
//! `StudioDesignRecords`, egui-free. Every mutating command is ONE undo step.
//!
//! Home: the vector entry's `design` member, the one record set of the document (it already holds the
//! paint styles and auto-layout nodes; Folio `validate_design_records` checks it whole before every
//! commit, on the document the step produces). A command that only changes records is an entry-only
//! step; one that also groups layers or draws an instance is a structure edit and the entry in one step
//! (`Run::restructure_and_entry`). Instances are drawn by the instance redraw ([`super::instance`]): a
//! group layer holding a translated copy of the definition; its binding is a state of the `design`
//! entry ([`super::persist`]).
//!
//! Studio readings (declared; Figma behaviour UNVERIFIED, D5): a component is named by its container
//! layer, so creating one from a single group keeps that group's name (another name is refused,
//! `component_name_mismatch`: rename the layer first); several layers or one leaf are grouped first in
//! the same step. A VARIANT property lives in the set's `variant_group_properties` and every member's
//! assignment, not in `component_property_definitions` (Folio's note: not duplicated). Property
//! references use the plain property name (Folio does not parse the spelling, STU-DS-113). An instance
//! renders the current definition only after `component.sync_instances` (STU-DS-003 partly met: no
//! automatic propagation in this slice). Native origins are the layers' bounds at command time.
//! A component is never created inside another definition or an instance (`component_nested`,
//! `layer_inside_instance`), and an instance is never placed where it would contain itself
//! (`component_cycle`). Out of scope: slots, clone, extract member, restore main, exposed nested
//! instances, the deferred `list_instances`, per-record expected revisions (STU-DS-163).
#![deny(clippy::arithmetic_side_effects)]
use super::super::layers::{mint_layer_id, new_layer, operation_for};
use super::super::vector::{self, VectorEntry};
use super::super::Viewer;
use super::instance::{NativeJob, Receipt, carries_nothing, settle};
use super::persist;
use crate::{Place, PlacementWrite};
use hsk_studio_folio::{
    self as folio, ComponentPropertyType, NodeKind, NodeRef, Parent, Payload, StudioComponent, StudioComponentInstance, StudioComponentProperty,
    StudioComponentSet, StudioDesignRecords, StudioDocument, StudioDocumentationLink, StudioNodeDesign, StudioPropertyOptions, StudioPropertyValue,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Most layers one selection names (charged before any lookup).
pub const MAX_SELECTION: usize = 1024;
/// Most members one combine makes a set of (Folio's property ceiling).
pub const MAX_SET_MEMBERS: usize = folio::MAX_PROPERTIES;
/// Longest component, set or property name in bytes (Folio's label rule).
pub const MAX_NAME_BYTES: usize = 128;
/// Longest TEXT property default in bytes.
pub const MAX_TEXT_DEFAULT_BYTES: usize = 4096;
/// Farthest an instance's top-left may be placed from the origin (document units; Studio-declared).
pub const INSTANCE_COORDINATE_LIMIT: f64 = 1.0e8;
/// The Folio budget an authored record set is validated under (the default's node and text ceilings
/// are below the entry's own caps).
const AUTHORED_BUDGET: folio::Budget = folio::Budget { input_bytes: 1_048_576, nodes: 65_536, edges: 65_536, payload_bytes: 16_777_216, depth: 32 };

/// A typed authoring command ([STU-DS-107], [STU-DS-112]).
#[derive(Clone, Debug, PartialEq)]
pub enum ComponentCommand {
    /// `component.create_from_selection`: one group becomes the definition; several layers (or one leaf)
    /// are grouped first.
    CreateFromSelection { layers: Vec<String>, name: Option<String> },
    /// `component.create_one_per_top_level_node`: one component per listed layer (leaves wrapped).
    CreateOnePerTopLevelNode { layers: Vec<String> },
    /// `component.create_instance`: a component (or a set: its default member) drawn at `at`.
    CreateInstance { component: String, at: [f64; 2], parent: Option<String> },
    /// `component.list_instances` (immediate form).
    ListInstances { component: String },
    /// `component.add_property`; `options` is the per-type record ([STU-DS-110]).
    AddProperty { component: String, name: String, property_type: ComponentPropertyType, default: Value, options: Option<Value> },
    /// `component.edit_property`: rename, default and options in one call.
    EditProperty { component: String, name: String, new_name: Option<String>, default: Option<Value>, options: Option<Value> },
    /// `component.delete_property`.
    DeleteProperty { component: String, name: String },
    /// `component.set_property_reference` (`property` absent: the reference is removed).
    SetPropertyReference { layer: String, field: String, property: Option<String> },
    /// `component.set_description` ([STU-DS-004]).
    SetDescription { component: String, description: String, links: Option<Vec<StudioDocumentationLink>> },
    /// `component.sync_instances`: every instance of the definition redrawn from it.
    SyncInstances { component: String },
    /// `component_set.combine_as_variants`.
    CombineAsVariants { components: Vec<String>, properties: BTreeMap<String, Vec<String>>, assignments: BTreeMap<String, BTreeMap<String, String>>, name: Option<String> },
    /// `component_set.set_default_variant`.
    SetDefaultVariant { set: String, component: String },
    /// `instance.detach`: the frame stays with plain children.
    Detach { layer: String },
    /// `instance.get_main_component`.
    GetMainComponent { layer: String },
}

impl ComponentCommand {
    /// The command id of the row.
    pub fn id(&self) -> &'static str {
        match self {
            Self::CreateFromSelection { .. } => "component.create_from_selection",
            Self::CreateOnePerTopLevelNode { .. } => "component.create_one_per_top_level_node",
            Self::CreateInstance { .. } => "component.create_instance",
            Self::ListInstances { .. } => "component.list_instances",
            Self::AddProperty { .. } => "component.add_property",
            Self::EditProperty { .. } => "component.edit_property",
            Self::DeleteProperty { .. } => "component.delete_property",
            Self::SetPropertyReference { .. } => "component.set_property_reference",
            Self::SetDescription { .. } => "component.set_description",
            Self::SyncInstances { .. } => "component.sync_instances",
            Self::CombineAsVariants { .. } => "component_set.combine_as_variants",
            Self::SetDefaultVariant { .. } => "component_set.set_default_variant",
            Self::Detach { .. } => "instance.detach",
            Self::GetMainComponent { .. } => "instance.get_main_component",
        }
    }

    fn op(&self) -> String {
        self.id().replace('.', "_")
    }
}

fn refused(code: &str, why: impl std::fmt::Display) -> String {
    format!("component: {code} ({why})")
}

/// Folio's slash-path name rule with its label length: no control character, no empty segment.
fn name_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_NAME_BYTES && !s.chars().any(char::is_control) && s.split('/').all(|seg| !seg.trim().is_empty())
}

/// Folio's label rule (a property name or variant value).
fn label_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_NAME_BYTES && !s.chars().any(char::is_control)
}

fn folio_refusal(d: &folio::Diagnostic) -> String {
    format!("component: {} ({})", persist::snake(d.code), d.target)
}

fn parent_id(doc: &StudioDocument, id: &str) -> Option<String> {
    match &doc.layers.iter().find(|l| l.layer_id == id)?.parent {
        Parent::Node(n) => Some(n.node_id.clone()),
        Parent::Root(()) => None,
    }
}

/// `id`'s ancestors, nearest first (bounded by the layer count).
fn ancestors(doc: &StudioDocument, id: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut at = parent_id(doc, id);
    while let Some(p) = at {
        if out.len() > doc.layers.len() {
            break;
        }
        at = parent_id(doc, &p);
        out.push(p);
    }
    out
}

fn is_group(doc: &StudioDocument, id: &str) -> bool {
    doc.layers.iter().any(|l| l.layer_id == id && l.kind == "group" && matches!(l.payload, Payload::Group))
}

fn layer_name(doc: &StudioDocument, id: &str) -> String {
    doc.layers.iter().find(|l| l.layer_id == id).map_or_else(String::new, |l| l.name.clone())
}

/// The document as `writes` leave it (parents only; Folio's design checks read nothing else).
fn project(doc: &StudioDocument, writes: &[PlacementWrite]) -> StudioDocument {
    let mut d = doc.clone();
    for w in writes {
        match (&w.create, &w.to) {
            (_, None) => d.layers.retain(|l| l.layer_id != w.layer_id),
            (Some((layer, _)), Some(_)) => d.layers.push(layer.clone()),
            (None, Some(place)) => {
                let parent = match place {
                    Place::Inside(g) => Some(g.clone()),
                    Place::Before(x) | Place::After(x) => parent_id(&d, x),
                    Place::AtBeginning(c) | Place::AtEnd(c) => c.clone(),
                };
                if let Some(l) = d.layers.iter_mut().find(|l| l.layer_id == w.layer_id) {
                    l.parent = match parent {
                        Some(p) => Parent::Node(NodeRef { node_kind: NodeKind::Layer, node_id: p }),
                        None => Parent::Root(()),
                    };
                }
            }
        }
    }
    d
}

/// A fresh `<prefix>` record id no record of `taken` holds.
fn mint_record_id(doc: &StudioDocument, prefix: &str, taken: &BTreeSet<String>) -> String {
    let uuid = doc.document_id.split_once('-').map_or("", |(_, u)| u);
    let (high, low) = (uuid.get(0..8).unwrap_or("00000000"), uuid.get(9..13).unwrap_or("0000"));
    let mut n = u64::try_from(taken.len()).unwrap_or(0);
    loop {
        let id = format!("{prefix}-{high}-{low}-7ede-a000-{n:012x}");
        if !taken.contains(&id) {
            return id;
        }
        n = n.wrapping_add(1);
    }
}

/// Empty publishing fields; the publish key is the record id under `local/` (Folio wants a key; a
/// library publish lane replaces it).
fn publishing(id: &str) -> Value {
    json!({"description": "", "description_markdown": "", "documentation_links": [], "publish_key": format!("local/{id}"), "is_remote": false})
}

fn new_component(id: &str, layer: &str) -> Result<StudioComponent, String> {
    serde_json::from_value(json!({
        "schema_id": "hsk.studio.component@1", "component_id": id, "layer_id": layer, "publishing": publishing(id),
        "variant_properties": null, "component_property_definitions": [],
    }))
    .map_err(|e| format!("component: record {e}"))
}

/// The authoring context: the document, the vector entry, its authored records (settled) and the
/// inspector's merged records.
struct Ctx {
    doc: StudioDocument,
    ventry: VectorEntry,
    authored: StudioDesignRecords,
    receipt: Option<Receipt>,
}

impl Ctx {
    fn ids(&self) -> BTreeSet<String> {
        let a = &self.authored;
        let mut ids: BTreeSet<String> = a.components.iter().map(|k| k.component_id.clone()).chain(a.component_sets.iter().map(|s| s.component_id.clone())).chain(a.component_instances.iter().map(|i| i.instance_id.clone())).collect();
        if let Some(r) = &self.receipt {
            ids.extend(r.records.components.iter().map(|k| k.component_id.clone()));
            ids.extend(r.records.component_sets.iter().map(|s| s.component_id.clone()));
            ids.extend(r.records.component_instances.iter().map(|i| i.instance_id.clone()));
        }
        ids
    }

    /// Container layers of every definition (authored and imported): layer -> component or set id.
    fn definitions(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        let mut add = |r: &StudioDesignRecords| {
            for k in &r.components {
                out.insert(k.layer_id.clone(), k.component_id.clone());
            }
            for s in &r.component_sets {
                out.insert(s.layer_id.clone(), s.component_id.clone());
            }
        };
        add(&self.authored);
        if let Some(r) = &self.receipt {
            add(&r.records);
        }
        out
    }

    /// Layers that draw an instance (authored and imported).
    fn instance_layers(&self) -> BTreeSet<String> {
        self.receipt.iter().flat_map(|r| r.bindings.iter().map(|b| b.layer_id.clone())).collect()
    }

    /// `component_unknown` / `component_not_authored`.
    fn unknown(&self, id: &str) -> String {
        let imported = self.receipt.as_ref().is_some_and(|r| r.records.components.iter().any(|k| k.component_id == id) || r.records.component_sets.iter().any(|s| s.component_id == id));
        if imported { refused("component_not_authored", format!("{id} comes from the design file's records")) } else { refused("component_unknown", id) }
    }

    fn component(&self, id: &str) -> Result<usize, String> {
        self.authored.components.iter().position(|k| k.component_id == id).ok_or_else(|| self.unknown(id))
    }

    fn set(&self, id: &str) -> Result<usize, String> {
        self.authored.component_sets.iter().position(|s| s.component_id == id).ok_or_else(|| self.unknown(id))
    }

    /// The set `component` is a member of (its container layer's parent is the set's layer).
    fn set_of(&self, component: &StudioComponent) -> Option<usize> {
        let parent = parent_id(&self.doc, &component.layer_id)?;
        self.authored.component_sets.iter().position(|s| s.layer_id == parent)
    }

    fn members(&self, set: &StudioComponentSet) -> Vec<usize> {
        (0..self.authored.components.len()).filter(|i| parent_id(&self.doc, &self.authored.components[*i].layer_id).as_deref() == Some(set.layer_id.as_str())).collect()
    }

    /// Checks a layer a component is made of: known, stacked, outside definitions and instances.
    fn source(&self, v: &Viewer, id: &str) -> Result<(), String> {
        if !self.doc.layers.iter().any(|l| l.layer_id == id) {
            return Err(refused("layer_unknown", id));
        }
        if v.run.placement(id).is_none() {
            return Err(refused("layer_not_stacked", id));
        }
        let defs = self.definitions();
        let insts = self.instance_layers();
        let up = ancestors(&self.doc, id);
        if insts.contains(id) || up.iter().any(|a| insts.contains(a)) {
            return Err(refused("layer_inside_instance", format!("{id} draws or is inside an instance")));
        }
        let contains_def = defs.keys().any(|d| ancestors(&self.doc, d).iter().any(|a| a == id));
        if defs.contains_key(id) || up.iter().any(|a| defs.contains_key(a)) || contains_def {
            return Err(refused("component_nested", format!("{id} is, holds or is inside a component definition")));
        }
        Ok(())
    }
}

/// Every set's VARIANT property definitions follow its `variant_group_properties` (Folio checks an
/// instance's VARIANT value against a definition): one definition per group, its default the default
/// member's value (else the group's first value), none for a group that is gone.
fn variant_definitions(r: &mut StudioDesignRecords, doc: &StudioDocument) {
    let components = r.components.clone();
    for s in &mut r.component_sets {
        let default = components.iter().find(|k| k.component_id == s.default_variant_id && parent_id(doc, &k.layer_id).as_deref() == Some(s.layer_id.as_str())).and_then(|k| k.variant_properties.clone()).unwrap_or_default();
        s.component_property_definitions.retain(|d| d.property_type != ComponentPropertyType::Variant || s.variant_group_properties.contains_key(&d.property_name));
        for (name, values) in &s.variant_group_properties {
            let value = default.get(name).cloned().or_else(|| values.first().cloned()).unwrap_or_default();
            match s.component_property_definitions.iter_mut().find(|d| &d.property_name == name) {
                Some(d) => {
                    d.property_type = ComponentPropertyType::Variant;
                    d.default_value = StudioPropertyValue::Text(value);
                    d.options = None;
                }
                None => s.component_property_definitions.push(StudioComponentProperty { property_name: name.clone(), property_type: ComponentPropertyType::Variant, default_value: StudioPropertyValue::Text(value), options: None }),
            }
        }
    }
}

/// A JSON property default as the typed value of `t`.
fn default_value(t: ComponentPropertyType, v: &Value, known: &BTreeSet<String>) -> Result<StudioPropertyValue, String> {
    match (t, v) {
        (ComponentPropertyType::Boolean, Value::Bool(b)) => Ok(StudioPropertyValue::Boolean(*b)),
        // The process drive surface carries every parameter as text.
        (ComponentPropertyType::Boolean, Value::String(s)) if s == "true" || s == "false" => Ok(StudioPropertyValue::Boolean(s == "true")),
        (ComponentPropertyType::Text | ComponentPropertyType::Variant, Value::String(s)) if s.len() <= MAX_TEXT_DEFAULT_BYTES && !s.chars().any(char::is_control) => Ok(StudioPropertyValue::Text(s.clone())),
        (ComponentPropertyType::InstanceSwap, Value::String(s)) if known.contains(s) => Ok(StudioPropertyValue::Text(s.clone())),
        _ => Err(refused("property_default_type", format!("a {t:?} property takes {}", match t {
            ComponentPropertyType::Boolean => "a boolean",
            ComponentPropertyType::InstanceSwap => "the id of a component of the document",
            _ => "one line of text",
        }))),
    }
}

/// A VARIANT option list: 1..=256 distinct labels.
fn variant_values(v: &Value) -> Result<Vec<String>, String> {
    let list: Vec<String> = v.as_array().filter(|a| !a.is_empty() && a.len() <= folio::MAX_PROPERTIES).and_then(|a| a.iter().map(|x| x.as_str().map(str::to_owned)).collect::<Option<Vec<_>>>()).ok_or_else(|| refused("variant_values_invalid", "1 to 256 text values"))?;
    let unique: BTreeSet<&String> = list.iter().collect();
    if unique.len() != list.len() || !list.iter().all(|s| label_ok(s)) {
        return Err(refused("variant_values_invalid", "distinct values of 1..=128 bytes"));
    }
    Ok(list)
}

fn swap_options(v: Option<&Value>) -> Result<StudioPropertyOptions, String> {
    match v {
        None => Ok(StudioPropertyOptions { preferred_values: Vec::new() }),
        Some(Value::Array(list)) => serde_json::from_value(json!({"preferred_values": list})).map_err(|_| refused("property_options_invalid", "[{type: COMPONENT|COMPONENT_SET, key}]")),
        Some(other) => serde_json::from_value(other.clone()).map_err(|_| refused("property_options_invalid", "{preferred_values: [{type, key}]}")),
    }
}

impl Viewer {
    /// Runs one authoring command (module docs). Returns the command's result.
    pub fn component_command(&mut self, command: ComponentCommand) -> Result<Value, String> {
        let id = command.id();
        let r = self.component_inner(command);
        match &r {
            Ok(_) => self.say(format!("{id} done")),
            Err(e) => self.say(format!("{id} refused: {e}")),
        }
        r
    }

    fn ctx(&self) -> Result<Ctx, String> {
        let doc = self.run.snapshot().ok_or("component: nothing_open (no document)")?.document().clone();
        let ventry = self.vector_entry()?;
        let mut authored = ventry.design.clone().unwrap_or_else(|| StudioDesignRecords::new(doc.document_id.clone()));
        settle(&mut authored, &doc);
        let receipt = self.design_receipt();
        if let Some(e) = receipt.as_ref().and_then(|r| r.error.clone()) {
            return Err(e);
        }
        Ok(Ctx { doc, ventry, authored, receipt })
    }

    /// Folio `validate_design_records` over `design` on the document `writes` produce (node entries of
    /// layers the document no longer holds are left out: they belong to revisions the history returns to).
    pub(super) fn check_authored(&self, doc: &StudioDocument, writes: &[PlacementWrite], design: &StudioDesignRecords) -> Result<(), String> {
        let proj = project(doc, writes);
        let present: BTreeSet<&str> = proj.layers.iter().map(|l| l.layer_id.as_str()).chain(proj.artboards.iter().map(|a| a.artboard_id.as_str())).chain(proj.page_spreads.iter().map(|s| s.page_spread_id.as_str())).collect();
        let mut r = design.clone();
        r.nodes.retain(|n| present.contains(n.node.node_id.as_str()));
        folio::validate_design_records(&r, &proj, AUTHORED_BUDGET, self.cancel, &crate::harness::TrustAll).map(|_| ()).map_err(|d| folio_refusal(&d))
    }

    /// Stores `authored` as an entry-only step (settled and Folio-checked first).
    fn commit_records(&mut self, op: &str, target: &str, c: Ctx) -> Result<u64, String> {
        self.commit_structure(op, target, c, Vec::new())
    }

    /// Stores `authored` with `writes` as one step (entry-only without writes).
    fn commit_structure(&mut self, op: &str, target: &str, c: Ctx, writes: Vec<PlacementWrite>) -> Result<u64, String> {
        if !vector::entry_persistable() {
            return Err(refused("vector_entry_not_persistable", "the session holds no vector entry token"));
        }
        let Ctx { doc, mut ventry, mut authored, .. } = c;
        let proj = project(&doc, &writes);
        settle(&mut authored, &proj);
        variant_definitions(&mut authored, &proj);
        self.check_authored(&doc, &writes, &authored)?;
        ventry.design = Some(authored);
        let bytes = ventry.to_bytes(&doc.document_id)?;
        let ok = if writes.is_empty() { self.run.set_tiles_and_entry(op, target, None, vector::ENTRY, bytes) } else { self.run.restructure_and_entry(op, &writes, vector::ENTRY, bytes) };
        self.after_edit(ok, format!("{op} {target}"))
    }

    /// A new group named `name` at the place of the topmost of `members` (siblings) holding them in
    /// their order: (group id, writes).
    fn group_writes(&self, doc: &StudioDocument, members: &[String], name: &str, salt: &mut u64, taken: &BTreeSet<String>) -> Result<(String, Vec<PlacementWrite>), String> {
        let mut placed = Vec::new();
        for id in members {
            placed.push((id.clone(), self.run.placement(id).ok_or_else(|| refused("layer_not_stacked", id))?));
        }
        let parent = placed.first().map(|(_, (p, _))| p.clone()).ok_or_else(|| refused("selection_empty", "no layers"))?;
        if placed.iter().any(|(_, (p, _))| *p != parent) {
            return Err(refused("selection_not_siblings", "the layers are not siblings of one stack"));
        }
        placed.sort_by_key(|(_, (_, i))| *i);
        let top = placed.last().map(|(id, _)| id.clone()).ok_or_else(|| refused("selection_empty", "no layers"))?;
        let mut gid = mint_layer_id(doc, *salt);
        while taken.contains(&gid) {
            *salt = salt.saturating_add(1);
            gid = mint_layer_id(doc, *salt);
        }
        *salt = salt.saturating_add(1);
        let group = new_layer(&gid, name, &parent, "group", json!({"payload_kind": "group"}))?;
        let mut writes = vec![PlacementWrite { layer_id: gid.clone(), to: Some(Place::Before(top)), create: Some((group, operation_for(&gid, true)?)) }];
        for (id, _) in &placed {
            writes.push(PlacementWrite { layer_id: id.clone(), to: Some(Place::Inside(gid.clone())), create: None });
        }
        Ok((gid, writes))
    }

    #[allow(clippy::too_many_lines)]
    fn component_inner(&mut self, command: ComponentCommand) -> Result<Value, String> {
        let op = command.op();
        match command {
            ComponentCommand::CreateFromSelection { layers, name } => self.create_from_selection(&op, &layers, name),
            ComponentCommand::CreateOnePerTopLevelNode { layers } => self.create_per_node(&op, &layers),
            ComponentCommand::CreateInstance { component, at, parent } => self.create_instance(&op, &component, at, parent.as_deref()),
            ComponentCommand::ListInstances { component } => self.list_instances(&component),
            ComponentCommand::AddProperty { component, name, property_type, default, options } => self.add_property(&op, &component, &name, property_type, &default, options.as_ref()),
            ComponentCommand::EditProperty { component, name, new_name, default, options } => self.edit_property(&op, &component, &name, new_name, default.as_ref(), options.as_ref()),
            ComponentCommand::DeleteProperty { component, name } => self.delete_property(&op, &component, &name),
            ComponentCommand::SetPropertyReference { layer, field, property } => self.set_property_reference(&op, &layer, &field, property),
            ComponentCommand::SetDescription { component, description, links } => {
                let mut c = self.ctx()?;
                if description.len() > 65_536 {
                    return Err(refused("description_too_long", "at most 65536 bytes"));
                }
                let (k, s) = (c.component(&component).ok(), c.set(&component).ok());
                let publishing = match (k, s) {
                    (Some(i), _) => &mut c.authored.components[i].publishing,
                    (None, Some(i)) => &mut c.authored.component_sets[i].publishing,
                    (None, None) => return Err(c.unknown(&component)),
                };
                publishing.description = description;
                if let Some(l) = links {
                    publishing.documentation_links = l;
                }
                let revision = self.commit_records(&op, &component, c)?;
                Ok(json!({"component_id": component, "revision": revision}))
            }
            ComponentCommand::SyncInstances { component } => self.sync_instances(&op, &component),
            ComponentCommand::CombineAsVariants { components, properties, assignments, name } => self.combine(&op, &components, &properties, &assignments, name),
            ComponentCommand::SetDefaultVariant { set, component } => {
                let mut c = self.ctx()?;
                let si = c.set(&set)?;
                let ki = c.authored.components.iter().position(|k| k.component_id == component).filter(|i| c.members(&c.authored.component_sets[si]).contains(i)).ok_or_else(|| refused("component_not_a_member", format!("{component} is not a member of {set}")))?;
                let member = c.authored.components[ki].component_id.clone();
                c.authored.component_sets[si].default_variant_id.clone_from(&member);
                let revision = self.commit_records(&op, &set, c)?;
                Ok(json!({"set_id": set, "default_variant_id": member, "revision": revision}))
            }
            ComponentCommand::Detach { layer } => {
                let mut c = self.ctx()?;
                let receipt = c.receipt.as_ref().ok_or_else(|| refused("not_an_instance", &layer))?;
                let b = receipt.bindings.iter().find(|b| b.layer_id == layer).ok_or_else(|| refused("not_an_instance", &layer))?;
                if !b.native {
                    return Err(refused("instance_not_authored", format!("{layer} draws an instance of the design file's records")));
                }
                let instance = b.instance_id.clone();
                c.authored.component_instances.retain(|i| i.instance_id != instance);
                let revision = self.commit_records(&op, &layer, c)?;
                Ok(json!({"layer_id": layer, "instance_id": instance, "revision": revision}))
            }
            ComponentCommand::GetMainComponent { layer } => {
                let view = self.instance_view(&layer).ok_or_else(|| refused("not_an_instance", &layer))?;
                Ok(json!({"layer_id": view.layer_id, "instance_id": view.instance_id, "state": view.state, "main_component_id": view.record.main_component_id, "component_id": view.component_id, "component_name": view.component_name}))
            }
        }
    }

    fn create_from_selection(&mut self, op: &str, layers: &[String], name: Option<String>) -> Result<Value, String> {
        if layers.is_empty() {
            return Err(refused("selection_empty", "no layers"));
        }
        if layers.len() > MAX_SELECTION {
            return Err(refused("selection_over_cap", format!("at most {MAX_SELECTION} layers")));
        }
        if layers.iter().collect::<BTreeSet<_>>().len() != layers.len() {
            return Err(refused("selection_duplicate", "a layer is listed twice"));
        }
        if let Some(n) = &name
            && !name_ok(n)
        {
            return Err(refused("component_name_invalid", "1..=128 bytes, no control character, no empty slash segment"));
        }
        let mut c = self.ctx()?;
        for id in layers {
            c.source(self, id)?;
        }
        let mut taken = c.ids();
        let id = mint_record_id(&c.doc, "SCMP", &taken);
        taken.insert(id.clone());
        if let [only] = layers
            && is_group(&c.doc, only)
        {
            if name.as_ref().is_some_and(|n| *n != layer_name(&c.doc, only)) {
                return Err(refused("component_name_mismatch", "a group keeps its own name: rename the layer first"));
            }
            c.authored.components.push(new_component(&id, only)?);
            let revision = self.commit_records(op, only, c)?;
            self.selected = Some(only.clone());
            return Ok(json!({"component_id": id, "layer_id": only, "grouped": false, "revision": revision}));
        }
        let name = name.unwrap_or_else(|| if let [only] = layers { layer_name(&c.doc, only) } else { "Component".to_owned() });
        if !name_ok(&name) {
            return Err(refused("component_name_invalid", "the layer's name is not a component name"));
        }
        let mut salt = u64::try_from(c.doc.layers.len()).unwrap_or(0);
        let (gid, writes) = self.group_writes(&c.doc, layers, &name, &mut salt, &BTreeSet::new())?;
        c.authored.components.push(new_component(&id, &gid)?);
        let revision = self.commit_structure(op, &gid, c, writes)?;
        self.selected = Some(gid.clone());
        Ok(json!({"component_id": id, "layer_id": gid, "grouped": true, "revision": revision}))
    }

    fn create_per_node(&mut self, op: &str, layers: &[String]) -> Result<Value, String> {
        if layers.is_empty() {
            return Err(refused("selection_empty", "no layers"));
        }
        if layers.len() > MAX_SELECTION {
            return Err(refused("selection_over_cap", format!("at most {MAX_SELECTION} layers")));
        }
        if layers.iter().collect::<BTreeSet<_>>().len() != layers.len() {
            return Err(refused("selection_duplicate", "a layer is listed twice"));
        }
        let mut c = self.ctx()?;
        for id in layers {
            if !c.doc.layers.iter().any(|l| &l.layer_id == id) {
                return Err(refused("layer_unknown", id));
            }
            if ancestors(&c.doc, id).iter().any(|a| layers.contains(a)) {
                return Err(refused("selection_not_top_level", format!("{id} is inside another listed layer")));
            }
        }
        for id in layers {
            c.source(self, id)?;
        }
        let mut taken = c.ids();
        let mut salt = u64::try_from(c.doc.layers.len()).unwrap_or(0);
        let (mut writes, mut minted, mut made) = (Vec::new(), BTreeSet::new(), Vec::new());
        for id in layers {
            let cid = mint_record_id(&c.doc, "SCMP", &taken);
            taken.insert(cid.clone());
            let layer = if is_group(&c.doc, id) {
                id.clone()
            } else {
                let name = layer_name(&c.doc, id);
                let name = if name_ok(&name) { name } else { "Component".to_owned() };
                let (gid, w) = self.group_writes(&c.doc, std::slice::from_ref(id), &name, &mut salt, &minted)?;
                minted.insert(gid.clone());
                writes.extend(w);
                gid
            };
            c.authored.components.push(new_component(&cid, &layer)?);
            made.push(json!({"component_id": cid, "layer_id": layer}));
        }
        let target = layers.first().cloned().unwrap_or_default();
        let revision = self.commit_structure(op, &target, c, writes)?;
        Ok(json!({"components": made, "revision": revision}))
    }

    /// Components whose definitions hold instances of `of` (transitively) plus `of` itself and its set
    /// relations: what an instance of `of` may not be placed inside.
    fn reach(c: &Ctx, of: &str) -> BTreeSet<String> {
        let Some(r) = &c.receipt else { return BTreeSet::from([of.to_owned()]) };
        let defs = c.definitions();
        let main_of = |instance: &str| r.native.get(instance).and_then(|i| i.main_component_id.clone());
        let mut out = BTreeSet::from([of.to_owned()]);
        let mut todo = vec![of.to_owned()];
        while let Some(k) = todo.pop() {
            if out.len() > c.authored.components.len().saturating_add(c.authored.component_sets.len()).saturating_add(1) {
                break;
            }
            // The definition layers of k (a set: every member's).
            let mut layers: Vec<String> = c.authored.components.iter().filter(|x| x.component_id == k).map(|x| x.layer_id.clone()).collect();
            if let Some(s) = c.authored.component_sets.iter().find(|s| s.component_id == k) {
                layers.push(s.layer_id.clone());
            }
            for b in r.bindings.iter().filter(|b| b.native) {
                let up = ancestors(&c.doc, &b.layer_id);
                if !up.iter().any(|a| layers.contains(a)) {
                    continue;
                }
                if let Some(m) = main_of(&b.instance_id) {
                    let set = c.authored.components.iter().find(|x| x.component_id == m).and_then(|x| parent_id(&c.doc, &x.layer_id)).and_then(|p| defs.get(&p).cloned());
                    for next in std::iter::once(m).chain(set) {
                        if out.insert(next.clone()) {
                            todo.push(next);
                        }
                    }
                }
            }
        }
        out
    }

    fn create_instance(&mut self, op: &str, component: &str, at: [f64; 2], parent: Option<&str>) -> Result<Value, String> {
        if !at.iter().all(|v| v.is_finite() && v.abs() <= INSTANCE_COORDINATE_LIMIT) {
            return Err(refused("instance_coordinate_out_of_range", format!("x and y within {INSTANCE_COORDINATE_LIMIT}")));
        }
        let c = self.ctx()?;
        let (member, frame_name, set_id) = match (c.authored.components.iter().find(|k| k.component_id == component), c.authored.component_sets.iter().find(|s| s.component_id == component)) {
            (Some(k), _) => (k.component_id.clone(), layer_name(&c.doc, &k.layer_id), c.set_of(k).map(|i| c.authored.component_sets[i].component_id.clone())),
            (None, Some(s)) => (s.default_variant_id.clone(), layer_name(&c.doc, &s.layer_id), Some(s.component_id.clone())),
            (None, None) => return Err(c.unknown(component)),
        };
        let receipt = c.receipt.as_ref().ok_or_else(|| refused("component_unknown", component))?;
        let parent_ref = match parent {
            None => Parent::Root(()),
            Some(p) => {
                if !c.doc.layers.iter().any(|l| l.layer_id == p) {
                    return Err(refused("parent_unknown", p));
                }
                if !is_group(&c.doc, p) {
                    return Err(refused("parent_not_a_group", p));
                }
                let insts = c.instance_layers();
                if insts.contains(p) || ancestors(&c.doc, p).iter().any(|a| insts.contains(a)) {
                    return Err(refused("layer_inside_instance", format!("{p} draws or is inside an instance")));
                }
                if c.authored.component_sets.iter().any(|s| s.layer_id == p) {
                    return Err(refused("parent_is_a_component_set", "a set holds only its members"));
                }
                let defs = c.definitions();
                let reach = Self::reach(&c, &member);
                let inside: Vec<String> = std::iter::once(p.to_owned()).chain(ancestors(&c.doc, p)).filter_map(|l| defs.get(&l).cloned()).collect();
                if inside.iter().any(|k| reach.contains(k) || Some(k) == set_id.as_ref() || k == component) {
                    return Err(refused("component_cycle", format!("an instance of {component} inside {p} would contain itself")));
                }
                Parent::Node(NodeRef { node_kind: NodeKind::Layer, node_id: p.to_owned() })
            }
        };
        let mut taken = c.ids();
        let instance = mint_record_id(&c.doc, "SCIN", &taken);
        taken.insert(instance.clone());
        let gid = mint_layer_id(&c.doc, u64::try_from(c.doc.layers.len()).unwrap_or(0));
        let group = new_layer(&gid, &frame_name, &parent_ref, "group", json!({"payload_kind": "group"}))?;
        let place = match parent {
            Some(p) => Place::Inside(p.to_owned()),
            None => Place::AtBeginning(None),
        };
        let write = PlacementWrite { layer_id: gid.clone(), to: Some(place), create: Some((group, operation_for(&gid, true)?)) };
        let record: StudioComponentInstance = serde_json::from_value(json!({
            "schema_id": "hsk.studio.component_instance@1", "instance_id": instance, "main_component_id": member,
            "component_properties": {}, "overrides": [], "scale_factor": 1.0, "is_exposed_instance": false,
        }))
        .map_err(|e| format!("component: record {e}"))?;
        let job = NativeJob { group: gid.clone(), record, origin: at, swap: None };
        let (notes, _, revision) = self.native_redraw(op, receipt, &[job], Some(write)).map_err(|e| e.replacen("design: ", "component: ", 1))?;
        self.selected = Some(gid.clone());
        Ok(json!({"instance_id": instance, "layer_id": gid, "component_id": member, "main_component_id": member, "notes": notes, "revision": revision}))
    }

    /// Every instance whose main component is `component`, a member of the set `component`, or the
    /// set's member the record resolves to: (binding layer, instance id, state, main).
    fn instances_of(&self, receipt: &Receipt, component: &str) -> Vec<(String, String, &'static str, Option<String>)> {
        let doc = match self.run.snapshot() {
            Some(s) => s.document(),
            None => return Vec::new(),
        };
        let set_layer = receipt.records.component_sets.iter().find(|s| s.component_id == component).map(|s| s.layer_id.clone());
        let entry = self.read_design_state(Some(receipt));
        let mut out = Vec::new();
        for b in &receipt.bindings {
            let Some((_, state, record, _)) = self.instance_record(receipt, &entry, &b.layer_id) else { continue };
            let main = record.main_component_id.clone();
            let in_set = main.as_ref().and_then(|m| receipt.records.components.iter().find(|k| &k.component_id == m)).and_then(|k| parent_id(doc, &k.layer_id)).is_some_and(|p| Some(p) == set_layer);
            if main.as_deref() == Some(component) || in_set {
                out.push((b.layer_id.clone(), b.instance_id.clone(), state, main));
            }
        }
        out
    }

    fn list_instances(&self, component: &str) -> Result<Value, String> {
        let receipt = self.design_receipt().ok_or_else(|| refused("component_unknown", component))?;
        let known = receipt.records.components.iter().any(|k| k.component_id == component) || receipt.records.component_sets.iter().any(|s| s.component_id == component);
        if !known {
            return Err(refused("component_unknown", component));
        }
        let list: Vec<Value> = self.instances_of(&receipt, component).into_iter().map(|(l, i, s, m)| json!({"layer_id": l, "instance_id": i, "state": s, "main_component_id": m})).collect();
        Ok(json!({"component_id": component, "instances": list}))
    }

    fn sync_instances(&mut self, op: &str, component: &str) -> Result<Value, String> {
        let c = self.ctx()?;
        if c.component(component).is_err() && c.set(component).is_err() {
            return Err(c.unknown(component));
        }
        let receipt = c.receipt.as_ref().ok_or_else(|| refused("component_unknown", component))?;
        let mut jobs = Vec::new();
        let mut skipped = Vec::new();
        for (layer, instance, state, main) in self.instances_of(receipt, component) {
            let Some(b) = receipt.bindings.iter().find(|b| b.layer_id == layer && b.native) else { continue };
            let record = receipt.native.get(&instance).cloned();
            match (state, main, record) {
                ("edited", Some(_), Some(record)) => jobs.push(NativeJob { group: layer, record, origin: b.origin, swap: None }),
                _ => skipped.push(layer),
            }
        }
        if jobs.is_empty() {
            return Ok(json!({"component_id": component, "redrawn": [], "skipped": skipped}));
        }
        let redrawn: Vec<String> = jobs.iter().map(|j| j.group.clone()).collect();
        let (notes, _, revision) = self.native_redraw(op, receipt, &jobs, None).map_err(|e| e.replacen("design: ", "component: ", 1))?;
        Ok(json!({"component_id": component, "redrawn": redrawn, "skipped": skipped, "notes": notes, "revision": revision}))
    }

    /// The owner of a property: an authored standalone component or a set (members' properties are
    /// the set's).
    fn owner(c: &Ctx, component: &str) -> Result<(Option<usize>, Option<usize>), String> {
        if let Ok(i) = c.component(component) {
            if c.set_of(&c.authored.components[i]).is_some() || c.authored.components[i].variant_properties.is_some() {
                return Err(refused("property_on_set_member", "a member's properties are its set's: name the set"));
            }
            return Ok((Some(i), None));
        }
        c.set(component).map(|s| (None, Some(s)))
    }

    /// The definitions, variant group names and definition layers of the owner.
    fn owned(c: &Ctx, owner: (Option<usize>, Option<usize>)) -> (Vec<StudioComponentProperty>, Vec<String>, Vec<String>, Vec<String>) {
        match owner {
            (Some(i), _) => {
                let k = &c.authored.components[i];
                (k.component_property_definitions.clone(), Vec::new(), vec![k.layer_id.clone()], vec![k.component_id.clone()])
            }
            (None, Some(s)) => {
                let set = &c.authored.component_sets[s];
                let members: Vec<usize> = c.members(set);
                let ids = std::iter::once(set.component_id.clone()).chain(members.iter().map(|m| c.authored.components[*m].component_id.clone())).collect();
                (set.component_property_definitions.clone(), set.variant_group_properties.keys().cloned().collect(), vec![set.layer_id.clone()], ids)
            }
            (None, None) => (Vec::new(), Vec::new(), Vec::new(), Vec::new()),
        }
    }

    fn add_property(&mut self, op: &str, component: &str, name: &str, t: ComponentPropertyType, default: &Value, options: Option<&Value>) -> Result<Value, String> {
        if !label_ok(name) {
            return Err(refused("property_name_invalid", "1..=128 bytes, no control character"));
        }
        if t == ComponentPropertyType::Slot {
            return Err(refused("slot_not_supported", "SLOT properties are not authored in this slice"));
        }
        let mut c = self.ctx()?;
        let owner = Self::owner(&c, component)?;
        let (defs, groups, _, _) = Self::owned(&c, owner);
        if defs.iter().any(|d| d.property_name == name) || groups.iter().any(|g| g == name) {
            return Err(refused("property_name_taken", name));
        }
        if defs.len().saturating_add(groups.len()) >= folio::MAX_PROPERTIES {
            return Err(refused("property_over_cap", format!("at most {} properties", folio::MAX_PROPERTIES)));
        }
        let known = c.ids();
        let value = default_value(t, default, &known)?;
        match (t, owner) {
            (ComponentPropertyType::Variant, (_, Some(s))) => {
                let StudioPropertyValue::Text(first) = value else { return Err(refused("property_default_type", "a VARIANT default is text")) };
                let values = match options {
                    Some(o) => variant_values(o)?,
                    None => vec![first.clone()],
                };
                if !values.contains(&first) || !label_ok(&first) {
                    return Err(refused("variant_value_undeclared", "the default is one of the values"));
                }
                let members = c.members(&c.authored.component_sets[s]);
                c.authored.component_sets[s].variant_group_properties.insert(name.to_owned(), values);
                for m in members {
                    c.authored.components[m].variant_properties.get_or_insert_with(BTreeMap::new).insert(name.to_owned(), first.clone());
                }
            }
            (ComponentPropertyType::Variant, _) => return Err(refused("variant_property_needs_a_set", "combine the components as variants first")),
            _ => {
                let options = if t == ComponentPropertyType::InstanceSwap { Some(swap_options(options)?) } else if options.is_some() { return Err(refused("property_options_invalid", "only INSTANCE_SWAP and VARIANT take options")) } else { None };
                let def = StudioComponentProperty { property_name: name.to_owned(), property_type: t, default_value: value, options };
                match owner {
                    (Some(i), _) => c.authored.components[i].component_property_definitions.push(def),
                    (None, Some(s)) => c.authored.component_sets[s].component_property_definitions.push(def),
                    (None, None) => return Err(c.unknown(component)),
                }
            }
        }
        let revision = self.commit_records(op, component, c)?;
        Ok(json!({"component_id": component, "property": name, "revision": revision}))
    }

    /// Rewrites (or with `to` None removes) `from` in every instance value of the owner and every
    /// definition layer reference: the instance count it touched.
    fn rewrite(c: &mut Ctx, ids: &[String], layers: &[String], from: &str, to: Option<&str>) -> usize {
        let mut count = 0_usize;
        for i in &mut c.authored.component_instances {
            if !i.main_component_id.as_ref().is_some_and(|m| ids.contains(m)) {
                continue;
            }
            if let Some(v) = i.component_properties.remove(from) {
                count = count.saturating_add(1);
                if let Some(to) = to {
                    i.component_properties.insert(to.to_owned(), v);
                }
            }
        }
        let doc = &c.doc;
        for n in &mut c.authored.nodes {
            if !ancestors(doc, &n.node.node_id).iter().any(|a| layers.contains(a)) {
                continue;
            }
            for v in n.component_property_references.values_mut() {
                if v == from
                    && let Some(to) = to
                {
                    to.clone_into(v);
                }
            }
            if to.is_none() {
                n.component_property_references.retain(|_, v| v != from);
            }
        }
        c.authored.nodes.retain(|n| !carries_nothing(n));
        count
    }

    fn edit_property(&mut self, op: &str, component: &str, name: &str, new_name: Option<String>, default: Option<&Value>, options: Option<&Value>) -> Result<Value, String> {
        let mut c = self.ctx()?;
        let owner = Self::owner(&c, component)?;
        let (defs, groups, layers, ids) = Self::owned(&c, owner);
        let variant = groups.iter().any(|g| g == name);
        if !variant && !defs.iter().any(|d| d.property_name == name) {
            return Err(refused("property_unknown", name));
        }
        if let Some(n) = &new_name {
            if !label_ok(n) {
                return Err(refused("property_name_invalid", "1..=128 bytes, no control character"));
            }
            if n != name && (defs.iter().any(|d| &d.property_name == n) || groups.iter().any(|g| g == n)) {
                return Err(refused("property_name_taken", n));
            }
        }
        let to = new_name.clone().unwrap_or_else(|| name.to_owned());
        let known = c.ids();
        let mut count = 0_usize;
        if variant {
            let Some(s) = owner.1 else { return Err(refused("property_unknown", name)) };
            if default.is_some() {
                return Err(refused("variant_default_not_stored", "a set's default is its default member (component_set.set_default_variant)"));
            }
            let members = c.members(&c.authored.component_sets[s]);
            let mut values = c.authored.component_sets[s].variant_group_properties.remove(name).unwrap_or_default();
            if let Some(o) = options {
                values = variant_values(o)?;
                let in_use = members.iter().any(|m| c.authored.components[*m].variant_properties.as_ref().and_then(|v| v.get(name)).is_some_and(|v| !values.contains(v)));
                if in_use {
                    return Err(refused("variant_value_in_use", "a member holds a value the new list drops"));
                }
            }
            c.authored.component_sets[s].variant_group_properties.insert(to.clone(), values);
            for m in members {
                if let Some(v) = c.authored.components[m].variant_properties.as_mut()
                    && let Some(x) = v.remove(name)
                {
                    v.insert(to.clone(), x);
                }
            }
        } else {
            let list = match owner {
                (Some(i), _) => &mut c.authored.components[i].component_property_definitions,
                (None, Some(s)) => &mut c.authored.component_sets[s].component_property_definitions,
                (None, None) => return Err(refused("property_unknown", name)),
            };
            let def = list.iter_mut().find(|d| d.property_name == name).ok_or_else(|| refused("property_unknown", name))?;
            if let Some(v) = default {
                def.default_value = default_value(def.property_type, v, &known)?;
            }
            if let Some(o) = options {
                if def.property_type != ComponentPropertyType::InstanceSwap {
                    return Err(refused("property_options_invalid", "only INSTANCE_SWAP and VARIANT take options"));
                }
                def.options = Some(swap_options(Some(o))?);
            }
            def.property_name.clone_from(&to);
        }
        if to != name {
            count = Self::rewrite(&mut c, &ids, &layers, name, Some(&to));
        }
        let revision = self.commit_records(op, component, c)?;
        Ok(json!({"component_id": component, "property": to, "instances_rewritten": count, "revision": revision}))
    }

    fn delete_property(&mut self, op: &str, component: &str, name: &str) -> Result<Value, String> {
        let mut c = self.ctx()?;
        let owner = Self::owner(&c, component)?;
        let (defs, groups, layers, ids) = Self::owned(&c, owner);
        if groups.iter().any(|g| g == name) {
            let Some(s) = owner.1 else { return Err(refused("property_unknown", name)) };
            let members = c.members(&c.authored.component_sets[s]);
            c.authored.component_sets[s].variant_group_properties.remove(name);
            for m in members {
                if let Some(v) = c.authored.components[m].variant_properties.as_mut() {
                    v.remove(name);
                }
            }
        } else if defs.iter().any(|d| d.property_name == name) {
            match owner {
                (Some(i), _) => c.authored.components[i].component_property_definitions.retain(|d| d.property_name != name),
                (None, Some(s)) => c.authored.component_sets[s].component_property_definitions.retain(|d| d.property_name != name),
                (None, None) => {}
            }
        } else {
            return Err(refused("property_unknown", name));
        }
        let count = Self::rewrite(&mut c, &ids, &layers, name, None);
        let revision = self.commit_records(op, component, c)?;
        Ok(json!({"component_id": component, "property": name, "instances_rewritten": count, "revision": revision}))
    }

    fn set_property_reference(&mut self, op: &str, layer: &str, field: &str, property: Option<String>) -> Result<Value, String> {
        let mut c = self.ctx()?;
        let l = c.doc.layers.iter().find(|l| l.layer_id == layer).ok_or_else(|| refused("layer_unknown", layer))?;
        let wanted = match field {
            "visible" => ComponentPropertyType::Boolean,
            "characters" if matches!(l.payload, Payload::Text { .. }) => ComponentPropertyType::Text,
            "characters" => return Err(refused("property_reference_field", "characters is a text layer's field")),
            _ => return Err(refused("property_reference_field", "visible|characters")),
        };
        // The nearest authored definition above the layer (a member: its set's properties too).
        let up = ancestors(&c.doc, layer);
        let owner = up.iter().find_map(|a| c.authored.components.iter().position(|k| &k.layer_id == a)).ok_or_else(|| refused("layer_not_in_a_definition", layer))?;
        if let Some(p) = &property {
            let k = &c.authored.components[owner];
            let set = c.set_of(k).map(|s| &c.authored.component_sets[s]);
            let def = k.component_property_definitions.iter().chain(set.into_iter().flat_map(|s| s.component_property_definitions.iter())).find(|d| &d.property_name == p).ok_or_else(|| refused("property_unknown", p))?;
            if def.property_type != wanted {
                return Err(refused("property_reference_type", format!("{field} takes a {wanted:?} property, {p} is {:?}", def.property_type)));
            }
        }
        match c.authored.nodes.iter().position(|n| n.node.node_id == layer) {
            Some(i) => {
                let n = &mut c.authored.nodes[i];
                match &property {
                    Some(p) => {
                        n.component_property_references.insert(field.to_owned(), p.clone());
                    }
                    None => {
                        n.component_property_references.remove(field);
                    }
                }
            }
            None => {
                let Some(p) = &property else { return Err(refused("property_reference_absent", format!("{layer} has no {field} reference"))) };
                let mut n: StudioNodeDesign = serde_json::from_value(json!({"node": {"node_kind": "layer", "node_id": layer}})).map_err(|e| format!("component: node {e}"))?;
                n.component_property_references.insert(field.to_owned(), p.clone());
                c.authored.nodes.push(n);
            }
        }
        c.authored.nodes.retain(|n| !carries_nothing(n));
        let revision = self.commit_records(op, layer, c)?;
        Ok(json!({"layer": layer, "field": field, "property": property, "revision": revision}))
    }

    #[allow(clippy::too_many_lines)]
    fn combine(&mut self, op: &str, components: &[String], properties: &BTreeMap<String, Vec<String>>, assignments: &BTreeMap<String, BTreeMap<String, String>>, name: Option<String>) -> Result<Value, String> {
        if components.len() < 2 || components.len() > MAX_SET_MEMBERS {
            return Err(refused("set_member_count", format!("2..={MAX_SET_MEMBERS} components")));
        }
        if components.iter().collect::<BTreeSet<_>>().len() != components.len() {
            return Err(refused("selection_duplicate", "a component is listed twice"));
        }
        if properties.is_empty() || properties.len() > folio::MAX_PROPERTIES {
            return Err(refused("variant_values_invalid", format!("1..={} variant properties", folio::MAX_PROPERTIES)));
        }
        for (p, values) in properties {
            if !label_ok(p) {
                return Err(refused("property_name_invalid", p));
            }
            variant_values(&json!(values))?;
        }
        if assignments.len() > components.len() {
            return Err(refused("assignment_component_unknown", "an assignment names a component that is not listed"));
        }
        if let Some(k) = assignments.keys().find(|k| !components.contains(k)) {
            return Err(refused("assignment_component_unknown", k));
        }
        let name = name.unwrap_or_else(|| "Component Set".to_owned());
        if !name_ok(&name) {
            return Err(refused("component_name_invalid", "1..=128 bytes, no control character, no empty slash segment"));
        }
        let mut c = self.ctx()?;
        let mut layers = Vec::with_capacity(components.len());
        for k in components {
            let i = c.component(k)?;
            let comp = &c.authored.components[i];
            if comp.variant_properties.is_some() || c.set_of(comp).is_some() {
                return Err(refused("component_already_in_set", k));
            }
            layers.push(comp.layer_id.clone());
        }
        // STU-DS-109, checked here so a refusal precedes any structure edit: every member carries a
        // declared value for every property, and no two carry the same assignment.
        let mut seen = BTreeSet::new();
        for k in components {
            let given = assignments.get(k).ok_or_else(|| refused("variant_assignment_incomplete", format!("{k} has no assignment")))?;
            for (p, v) in given {
                let values = properties.get(p).ok_or_else(|| refused("variant_property_undeclared", p))?;
                if !values.contains(v) {
                    return Err(refused("variant_value_undeclared", format!("{p}={v}")));
                }
            }
            if let Some(p) = properties.keys().find(|p| !given.contains_key(*p)) {
                return Err(refused("variant_assignment_incomplete", format!("{k} has no {p}")));
            }
            if !seen.insert(given.clone()) {
                return Err(refused("variant_assignment_duplicate", format!("{k} repeats an assignment")));
            }
        }
        let mut taken = c.ids();
        let set_id = mint_record_id(&c.doc, "SCMP", &taken);
        taken.insert(set_id.clone());
        let mut salt = u64::try_from(c.doc.layers.len()).unwrap_or(0);
        let (gid, writes) = self.group_writes(&c.doc, &layers, &name, &mut salt, &BTreeSet::new())?;
        for k in components {
            if let Some(comp) = c.authored.components.iter_mut().find(|x| &x.component_id == k) {
                comp.variant_properties = assignments.get(k).cloned();
            }
        }
        let set: StudioComponentSet = serde_json::from_value(json!({
            "schema_id": "hsk.studio.component@1", "component_id": set_id, "layer_id": gid, "publishing": publishing(&set_id),
            "default_variant_id": components[0], "variant_group_properties": properties, "component_property_definitions": [],
        }))
        .map_err(|e| format!("component: record {e}"))?;
        c.authored.component_sets.push(set);
        let revision = self.commit_structure(op, &gid, c, writes)?;
        self.selected = Some(gid.clone());
        Ok(json!({"set_id": set_id, "layer_id": gid, "default_variant_id": components[0], "revision": revision}))
    }

    /// `state_json()["design"]["components"]`: the document's authored components, sets and instances.
    pub(super) fn components_json(&self, receipt: Option<&Receipt>) -> Value {
        let empty = || json!({"components": [], "sets": [], "instances": [], "error": null});
        let (Some(r), Some(doc)) = (receipt, self.run.snapshot().map(|s| s.document())) else { return empty() };
        let Some(a) = &r.authored else { return json!({"components": [], "sets": [], "instances": [], "error": r.error}) };
        let set_of = |layer: &str| parent_id(doc, layer).and_then(|p| a.component_sets.iter().find(|s| s.layer_id == p)).map(|s| s.component_id.clone());
        let components: Vec<Value> = a
            .components
            .iter()
            .map(|k| {
                json!({
                    "component_id": k.component_id, "layer_id": k.layer_id, "name": layer_name(doc, &k.layer_id), "set_id": set_of(&k.layer_id),
                    "variant_properties": k.variant_properties, "properties": k.component_property_definitions,
                    "description": k.publishing.description, "documentation_links": k.publishing.documentation_links,
                })
            })
            .collect();
        let sets: Vec<Value> = a
            .component_sets
            .iter()
            .map(|s| {
                let members: Vec<&str> = a.components.iter().filter(|k| parent_id(doc, &k.layer_id).as_deref() == Some(s.layer_id.as_str())).map(|k| k.component_id.as_str()).collect();
                json!({
                    "set_id": s.component_id, "layer_id": s.layer_id, "name": layer_name(doc, &s.layer_id), "default_variant_id": s.default_variant_id,
                    "variant_group_properties": s.variant_group_properties, "members": members, "properties": s.component_property_definitions,
                    "description": s.publishing.description,
                })
            })
            .collect();
        let instances: Vec<Value> = a
            .component_instances
            .iter()
            .map(|i| {
                let layer = r.bindings.iter().find(|b| b.native && b.instance_id == i.instance_id).map(|b| b.layer_id.clone());
                json!({"instance_id": i.instance_id, "layer_id": layer, "main_component_id": i.main_component_id})
            })
            .collect();
        json!({"components": components, "sets": sets, "instances": instances, "error": r.error})
    }
}

#[cfg(test)]
mod tests {
    use super::{label_ok, name_ok};

    #[test]
    fn names_follow_the_slash_path_and_label_rules() {
        assert!(name_ok("Buttons/Primary"));
        assert!(!name_ok("A//B") && !name_ok("") && !name_ok(" /x") && !name_ok("a\u{7}b") && !name_ok(&"x".repeat(129)));
        assert!(label_ok("Show icon") && !label_ok("") && !label_ok("a\nb"));
    }
}
