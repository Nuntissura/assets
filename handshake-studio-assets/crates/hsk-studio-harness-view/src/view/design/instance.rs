//! The instance inspector (lane SMB-FIG-EDIT-1 slices 2 and 3; [STU-DS-003/008/009/016/017],
//! [STU-DS-160] contextual binding, [STU-DS-162] typed command surface).
//!
//! Binding: a layer that draws a component instance (the expansion group of an imported instance,
//! listed in the import receipt's `design.instances`) gets a design-instance section in the
//! Properties panel when it is selected; no other layer does, and there is no panel or mode switch.
//!
//! Records: the import receipt carries the design records no session entry holds (components,
//! sets, instances). An instance's current record is the import record until a design command
//! changes it; every command redraws the instance and records the new record against the
//! instance layer's new content (its child layer ids), so undo and redo, which return the layer to
//! earlier children, return the inspector to the record that drew them. GAP (recorded, lead
//! decision): no session document entry holds design records (the `vector` entry admits paint
//! styles only), so a record changed by a command lives for the session; after a save and reopen
//! such an instance (or one whose layers were changed outside the inspector) reports
//! `state: "unknown"` and refuses commands until the design file is imported again.
//!
//! Commands ([`DesignCommand`], one chronicle transaction = one undo step each): variant switch
//! (`hsk_studio_components::switch_variant`), component swap (`swap_component`; overrides re-matched
//! by layer name and hierarchy, dropped ones reported), component property values (STU-DS-009:
//! boolean and text), override set (`characters`, `visible`, `opacity`; the fields the redraw
//! applies), per-override reset and reset-all (STU-DS-017). The redraw replaces the instance
//! layer's children with a copy of the resolved member's definition layers, translated by the
//! instance origin minus the definition origin (both from the receipt; a rotated or scaled
//! instance or definition is refused), with the overrides applied: new path records (vector entry,
//! same paint style), new stories (text entry, `characters` replaced). Clips, masks and effects of
//! definition layers are not copied (listed in the command result's `notes`).
#![deny(clippy::arithmetic_side_effects)]
use super::super::layers::{mint_layer_id, operation_for, stack};
use super::super::text::{self, TextEntry, TextLayerRecord};
use super::super::vector::{self, PathRecord};
use super::super::Viewer;
use crate::{Place, PlacementWrite};
use hsk_studio_components::{Design, LocalContent, resolve_instance, swap_component, switch_variant};
use hsk_studio_folio::{
    self as folio, ComponentPropertyType, NodeKind, NodeRef, Parent, Payload, StudioComponentInstance, StudioDesignRecords, StudioDocument,
    StudioInstanceProperty, StudioLayer, StudioOverrideRecord, StudioPropertyValue, StudioValue,
};
use hsk_studio_nib::PathPoint;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Stable widget ids of the instance section.
pub mod widget {
    pub const SECTION: &str = "design.instance";
    pub const RESET_ALL: &str = "design.instance.reset_all";
    pub fn variant(property: &str, value: &str) -> String {
        format!("design.instance.variant.{property}.{value}")
    }
    pub fn property(name: &str) -> String {
        format!("design.instance.property.{name}")
    }
    pub fn text(node_id: &str) -> String {
        format!("design.instance.text.{node_id}")
    }
    pub fn reset(node_id: &str, field: &str) -> String {
        format!("design.instance.override.{node_id}.{field}.reset")
    }
    pub fn swap(component_id: &str) -> String {
        format!("design.instance.swap.{component_id}")
    }
}

/// Most instance records one session keeps (the oldest goes first).
pub const MAX_STATES: usize = 4096;
/// Most definition layers one redraw copies.
pub const MAX_REDRAW_LAYERS: usize = 2048;
/// Override fields the redraw applies.
pub const DRAWN_OVERRIDES: [&str; 3] = ["characters", "visible", "opacity"];

/// A typed instance command ([STU-DS-162]).
#[derive(Clone, Debug, PartialEq)]
pub enum DesignCommand {
    /// STU-DS-008: set a VARIANT property of a set member instance.
    SwitchVariant { property: String, value: String },
    /// STU-DS-009: a BOOLEAN or TEXT component property value.
    SetProperty { name: String, value: StudioPropertyValue },
    /// STU-DS-016: override `field` of definition node `node_id` (one of [`DRAWN_OVERRIDES`]).
    SetOverride { node_id: String, field: String, value: StudioValue },
    /// STU-DS-017 per-property reset.
    ResetOverride { node_id: String, field: String },
    /// STU-DS-017 reset-all-overrides.
    ResetAll,
    /// Component swap (overrides re-matched by layer name and hierarchy).
    Swap { component_id: String },
}

impl DesignCommand {
    fn key(&self) -> &'static str {
        match self {
            Self::SwitchVariant { .. } => "switch_variant",
            Self::SetProperty { .. } => "set_property",
            Self::SetOverride { .. } => "set_override",
            Self::ResetOverride { .. } => "reset_override",
            Self::ResetAll => "reset_all",
            Self::Swap { .. } => "swap_component",
        }
    }
}

/// Per-session design state of the window.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DesignState {
    /// (instance id, content signature of its layer) -> the record that drew that content.
    states: Vec<((String, String), StudioComponentInstance)>,
    last: Option<Value>,
    /// Text override fields being typed (node id -> text).
    buffers: BTreeMap<String, String>,
}

impl DesignState {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    fn get(&self, instance: &str, signature: &str) -> Option<&StudioComponentInstance> {
        self.states.iter().rev().find(|((i, s), _)| i == instance && s == signature).map(|(_, r)| r)
    }
    fn put(&mut self, instance: String, signature: String, record: StudioComponentInstance) {
        if self.states.len() >= MAX_STATES {
            self.states.remove(0);
        }
        self.states.push(((instance, signature), record));
    }
}

/// One instance binding of the receipt.
#[derive(Clone, Debug, PartialEq)]
struct Binding {
    layer_id: String,
    instance_id: String,
    origin: [f64; 2],
    pure: bool,
    signature: Option<String>,
}

/// The receipt's design member, parsed.
struct Receipt {
    records: StudioDesignRecords,
    bindings: Vec<Binding>,
    /// component id -> (origin, translation only).
    origins: BTreeMap<String, ([f64; 2], bool)>,
}

fn origin_of(v: &Value) -> Option<[f64; 2]> {
    let a = v.as_array().filter(|a| a.len() == 2)?;
    let (x, y) = (a[0].as_f64()?, a[1].as_f64()?);
    (x.is_finite() && y.is_finite()).then_some([x, y])
}

fn receipt_design(receipt: Option<&Value>) -> Option<Receipt> {
    let d = receipt?.get("design")?;
    let records: StudioDesignRecords = serde_json::from_value(d.get("records")?.clone()).ok()?;
    let bindings = d
        .get("instances")?
        .as_array()?
        .iter()
        .filter_map(|b| {
            Some(Binding {
                layer_id: b["layer_id"].as_str()?.to_owned(),
                instance_id: b["instance_id"].as_str()?.to_owned(),
                origin: origin_of(&b["origin_px"])?,
                pure: b["translation_only"].as_bool()?,
                signature: b["signature"].as_str().map(str::to_owned),
            })
        })
        .collect();
    let origins = d
        .get("components")?
        .as_array()?
        .iter()
        .filter_map(|c| Some((c["component_id"].as_str()?.to_owned(), (origin_of(&c["origin_px"])?, c["translation_only"].as_bool()?))))
        .collect();
    Some(Receipt { records, bindings, origins })
}

/// The content signature of a layer: its children's ids in document order.
pub(super) fn signature(doc: &StudioDocument, layer_id: &str) -> String {
    doc.layers.iter().filter(|l| matches!(&l.parent, Parent::Node(p) if p.node_id == layer_id)).map(|l| l.layer_id.as_str()).collect::<Vec<_>>().join(",")
}

/// Definition layers under `root`, depth first in stack order (bottom first): (layer, parent id).
fn definition_layers<'a>(doc: &'a StudioDocument, root: &str) -> Result<Vec<(&'a StudioLayer, String)>, String> {
    let mut out = Vec::new();
    let mut todo: Vec<(String, usize)> = vec![(root.to_owned(), 0)];
    while let Some((parent, depth)) = todo.pop() {
        if depth > 64 {
            return Err("design: definition nesting deeper than 64".into());
        }
        let kids = stack(doc, Some(&parent));
        for k in kids.iter().rev() {
            todo.push((k.clone(), depth.saturating_add(1)));
        }
        if parent != root {
            let l = doc.layers.iter().find(|l| l.layer_id == parent).ok_or_else(|| format!("design: layer {parent} missing"))?;
            let p = match &l.parent {
                Parent::Node(n) => n.node_id.clone(),
                Parent::Root(()) => String::new(),
            };
            out.push((l, p));
            if out.len() > MAX_REDRAW_LAYERS {
                return Err(format!("design: definition over {MAX_REDRAW_LAYERS} layers"));
            }
        }
    }
    Ok(out)
}

fn value_text(v: &StudioValue) -> String {
    match v {
        StudioValue::String { value } => value.clone(),
        StudioValue::Boolean { value } => value.to_string(),
        StudioValue::Float { value } => format!("{value}"),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// What the inspector shows for one instance.
#[derive(Clone, Debug, PartialEq)]
pub struct InstanceView {
    pub layer_id: String,
    pub instance_id: String,
    /// `import`, `edited` or `unknown` (content changed outside the inspector or in an earlier session).
    pub state: &'static str,
    pub record: StudioComponentInstance,
    /// The member component the record resolves to, and its layer name.
    pub component_id: Option<String>,
    pub component_name: Option<String>,
    /// Variant properties of the set: name -> (values, current value).
    pub variants: BTreeMap<String, (Vec<String>, String)>,
    /// Component properties (STU-DS-009): name, type, current value.
    pub properties: Vec<(String, ComponentPropertyType, StudioPropertyValue)>,
    /// Overrides: node id, node name, field, value text.
    pub overrides: Vec<(String, String, String, String)>,
    /// Text layers of the definition: node id, name, current text.
    pub texts: Vec<(String, String, String)>,
    /// Swap targets: component id, layer name.
    pub swaps: Vec<(String, String)>,
    pub error: Option<String>,
}

impl InstanceView {
    pub fn json(&self) -> Value {
        json!({
            "layer_id": self.layer_id, "instance_id": self.instance_id, "state": self.state,
            "main_component_id": self.record.main_component_id, "component_id": self.component_id, "component_name": self.component_name,
            "variants": self.variants.iter().map(|(k, (vals, cur))| (k.clone(), json!({"values": vals, "current": cur}))).collect::<serde_json::Map<_, _>>(),
            "properties": self.properties.iter().map(|(n, t, v)| json!({"name": n, "type": t, "value": v})).collect::<Vec<_>>(),
            "overrides": self.overrides.iter().map(|(n, name, f, v)| json!({"node_id": n, "node_name": name, "field": f, "value": v})).collect::<Vec<_>>(),
            "texts": self.texts.iter().map(|(n, name, t)| json!({"node_id": n, "name": name, "text": t})).collect::<Vec<_>>(),
            "swap_targets": self.swaps.iter().map(|(id, name)| json!({"component_id": id, "name": name})).collect::<Vec<_>>(),
            "error": self.error,
        })
    }
}

/// An override record without a value for a listed field cannot resolve (the importer lists
/// `fills` without a value: a receipt loss); such fields are left out of evaluation.
fn evaluable(record: &StudioComponentInstance) -> StudioComponentInstance {
    let mut r = record.clone();
    for o in &mut r.overrides {
        let values = o.values.clone();
        o.overridden_fields.retain(|f| values.contains_key(f));
    }
    r.overrides.retain(|o| !o.overridden_fields.is_empty());
    r
}

fn with_instance(records: &StudioDesignRecords, record: &StudioComponentInstance) -> StudioDesignRecords {
    let mut r = records.clone();
    match r.component_instances.iter_mut().find(|i| i.instance_id == record.instance_id) {
        Some(slot) => *slot = record.clone(),
        None => r.component_instances.push(record.clone()),
    }
    r
}

impl Viewer {
    fn design_receipt(&self) -> Option<Receipt> {
        receipt_design(self.receipt.as_ref())
    }

    /// The instance binding of `layer_id` and its current record (`None`: not an instance layer).
    fn instance_record(&self, receipt: &Receipt, layer_id: &str) -> Option<(Binding, &'static str, StudioComponentInstance)> {
        let doc = self.run.snapshot()?.document();
        let b = receipt.bindings.iter().find(|b| b.layer_id == layer_id)?.clone();
        doc.layers.iter().find(|l| l.layer_id == layer_id)?;
        let sig = signature(doc, layer_id);
        if let Some(r) = self.design.get(&b.instance_id, &sig) {
            return Some((b, "edited", r.clone()));
        }
        let import = receipt.records.component_instances.iter().find(|i| i.instance_id == b.instance_id)?.clone();
        let state = if b.signature.as_deref().is_none_or(|s| s == sig) { "import" } else { "unknown" };
        Some((b, state, import))
    }

    /// The instance inspector's view of `layer_id` (`None`: the layer draws no instance).
    pub fn instance_view(&self, layer_id: &str) -> Option<InstanceView> {
        let receipt = self.design_receipt()?;
        let (b, state, record) = self.instance_record(&receipt, layer_id)?;
        let doc = self.run.snapshot()?.document();
        let mut view = InstanceView {
            layer_id: b.layer_id.clone(),
            instance_id: b.instance_id.clone(),
            state,
            record: record.clone(),
            component_id: None,
            component_name: None,
            variants: BTreeMap::new(),
            properties: Vec::new(),
            overrides: Vec::new(),
            texts: Vec::new(),
            swaps: Vec::new(),
            error: None,
        };
        let name = |id: &str| doc.layers.iter().find(|l| l.layer_id == id).map_or_else(|| id.to_owned(), |l| l.name.clone());
        for o in &record.overrides {
            for f in &o.overridden_fields {
                let v = o.values.get(f).map_or_else(|| "(no value)".to_owned(), value_text);
                view.overrides.push((o.overridden_node_id.clone(), name(&o.overridden_node_id), f.clone(), v));
            }
        }
        let records = with_instance(&receipt.records, &evaluable(&record));
        let local = LocalContent::default();
        let design = match Design::new(doc, &records, &local) {
            Ok(d) => d,
            Err(e) => {
                view.error = Some(format!("design_records: {e:?}"));
                return Some(view);
            }
        };
        let resolved = match resolve_instance(&evaluable(&record), &design, &BTreeMap::new(), 0, &hsk_studio_components::Limits::default(), self.cancel) {
            Ok(r) => r,
            Err(e) => {
                view.error = Some(format!("resolve: {e:?}"));
                return Some(view);
            }
        };
        let member = design.component(&resolved.component_id);
        view.component_id = Some(resolved.component_id.clone());
        view.component_name = member.map(|m| name(&m.layer_id));
        let set = design.set_of(&resolved.component_id);
        if let (Some(set), Some(m)) = (set, member) {
            for (prop, values) in &set.variant_group_properties {
                let cur = m.variant_properties.as_ref().and_then(|v| v.get(prop)).cloned().unwrap_or_default();
                view.variants.insert(prop.clone(), (values.clone(), cur));
            }
        }
        let mut defs: Vec<&folio::StudioComponentProperty> = Vec::new();
        if let Some(s) = set {
            defs.extend(&s.component_property_definitions);
        }
        if let Some(m) = member {
            defs.extend(&m.component_property_definitions);
        }
        for d in defs {
            if matches!(d.property_type, ComponentPropertyType::Variant | ComponentPropertyType::Slot) || view.properties.iter().any(|(n, ..)| n == &d.property_name) {
                continue;
            }
            let v = record.component_properties.get(&d.property_name).map_or_else(|| d.default_value.clone(), |p| p.value.clone());
            view.properties.push((d.property_name.clone(), d.property_type, v));
        }
        if let Some(m) = member {
            let text_entry = self.text_entry().ok();
            if let Ok(layers) = definition_layers(doc, &m.layer_id) {
                for (l, _) in layers {
                    let Payload::Text { story_id } = &l.payload else { continue };
                    let over = record.overrides.iter().find(|o| o.overridden_node_id == l.layer_id).and_then(|o| o.values.get("characters")).map(value_text);
                    let base = text_entry.as_ref().and_then(|e| e.stories.iter().find(|s| &s.story_id == story_id)).and_then(text::story_text).map(str::to_owned).unwrap_or_default();
                    view.texts.push((l.layer_id.clone(), l.name.clone(), over.unwrap_or(base)));
                }
            }
        }
        let in_set: BTreeSet<&str> = set.map(|s| design.members(s).into_iter().map(|k| k.component_id.as_str()).collect()).unwrap_or_default();
        for k in &records.components {
            if k.component_id != resolved.component_id && !in_set.contains(k.component_id.as_str()) {
                view.swaps.push((k.component_id.clone(), name(&k.layer_id)));
            }
        }
        // Set members are reached by the variant fields; members of other sets and plain components
        // are swap targets.
        Some(view)
    }

    /// Runs `command` on the instance drawn by `layer_id` as one undoable step (module docs).
    /// Returns the command result (also `state_json()["design"]["last"]`).
    pub fn design_command(&mut self, layer_id: &str, command: DesignCommand) -> Result<Value, String> {
        let key = command.key();
        let r = self.design_command_inner(layer_id, command);
        let last = match &r {
            Ok(v) => v.clone(),
            Err(e) => json!({"command": key, "layer_id": layer_id, "error": e}),
        };
        self.design.last = Some(last);
        match &r {
            Ok(v) => self.say(format!("design {key} on {layer_id}: component {}", v["component_id"])),
            Err(e) => self.say(format!("design {key} on {layer_id} refused: {e}")),
        }
        r
    }

    #[allow(clippy::too_many_lines)]
    fn design_command_inner(&mut self, layer_id: &str, command: DesignCommand) -> Result<Value, String> {
        let receipt = self.design_receipt().ok_or("design: the open document has no design records (import a design file)")?;
        let (binding, state, record) = self.instance_record(&receipt, layer_id).ok_or_else(|| format!("design: {layer_id} draws no component instance"))?;
        if state == "unknown" {
            return Err("design: instance_record_unknown (the instance's layers changed outside the inspector or in an earlier session; instance records are not persisted, import the design file again)".into());
        }
        let doc = self.run.snapshot().ok_or("nothing open")?.document().clone();
        let base = evaluable(&record);
        let records = with_instance(&receipt.records, &base);
        let local = LocalContent::default();
        let design = Design::new(&doc, &records, &local).map_err(|e| format!("design: records {e:?}"))?;
        let key = command.key();
        let (next, dropped) = match &command {
            DesignCommand::SwitchVariant { property, value } => switch_variant(&base, &design, property, value).map_err(|e| format!("design: {e:?}"))?,
            DesignCommand::Swap { component_id } => swap_component(&base, &design, component_id).map_err(|e| format!("design: {e:?}"))?,
            DesignCommand::SetProperty { name, value } => {
                let r = resolve_instance(&base, &design, &BTreeMap::new(), 0, &hsk_studio_components::Limits::default(), self.cancel).map_err(|e| format!("design: {e:?}"))?;
                let member = design.component(&r.component_id).ok_or("design: member missing")?;
                let def = design.set_of(&r.component_id).into_iter().flat_map(|s| s.component_property_definitions.iter()).chain(member.component_property_definitions.iter()).find(|d| &d.property_name == name).ok_or_else(|| format!("design: no component property {name}"))?;
                let ok = matches!((def.property_type, value), (ComponentPropertyType::Boolean, StudioPropertyValue::Boolean(_)) | (ComponentPropertyType::Text, StudioPropertyValue::Text(_)));
                if !ok {
                    return Err(format!("design: property {name} takes a {:?} value here (boolean and text properties are editable)", def.property_type));
                }
                let mut n = base.clone();
                n.component_properties.insert(name.clone(), StudioInstanceProperty { property_type: def.property_type, value: value.clone(), bound_variable_id: None });
                (n, Vec::new())
            }
            DesignCommand::SetOverride { node_id, field, value } => {
                if !DRAWN_OVERRIDES.contains(&field.as_str()) {
                    return Err(format!("design: override field {field} is not drawn here (editable: {})", DRAWN_OVERRIDES.join(", ")));
                }
                let ok = matches!((field.as_str(), value), ("characters", StudioValue::String { .. }) | ("visible", StudioValue::Boolean { .. }) | ("opacity", StudioValue::Float { .. }));
                if !ok {
                    return Err(format!("design: override {field} has the wrong value type"));
                }
                if let StudioValue::Float { value } = value
                    && !(0.0..=1.0).contains(value)
                {
                    return Err("design: opacity override outside 0..=1".into());
                }
                if let StudioValue::String { value } = value
                    && (value.len() > text::MAX_STORY_BYTES || value.contains(['\n', '\r']))
                {
                    return Err(format!("design: text override is one line of at most {} bytes", text::MAX_STORY_BYTES));
                }
                let mut n = base.clone();
                let rec = match n.overrides.iter().position(|o| &o.overridden_node_id == node_id) {
                    Some(i) => &mut n.overrides[i],
                    None => {
                        n.overrides.push(StudioOverrideRecord { overridden_node_id: node_id.clone(), overridden_fields: Vec::new(), values: BTreeMap::new() });
                        n.overrides.last_mut().ok_or("design: override")?
                    }
                };
                if !rec.overridden_fields.contains(field) {
                    rec.overridden_fields.push(field.clone());
                }
                rec.values.insert(field.clone(), value.clone());
                (n, Vec::new())
            }
            DesignCommand::ResetOverride { node_id, field } => {
                let mut n = base.clone();
                let rec = n.overrides.iter_mut().find(|o| &o.overridden_node_id == node_id && o.overridden_fields.contains(field)).ok_or_else(|| format!("design: no override {field} on {node_id}"))?;
                rec.overridden_fields.retain(|f| f != field);
                rec.values.remove(field);
                n.overrides.retain(|o| !o.overridden_fields.is_empty());
                (n, Vec::new())
            }
            DesignCommand::ResetAll => {
                let mut n = base.clone();
                n.overrides.clear();
                (n, Vec::new())
            }
        };
        // The successor must resolve (STU-DS-116 tiers) before anything is drawn.
        let records = with_instance(&receipt.records, &next);
        let design = Design::new(&doc, &records, &local).map_err(|e| format!("design: records {e:?}"))?;
        let resolved = resolve_instance(&next, &design, &BTreeMap::new(), 0, &hsk_studio_components::Limits::default(), self.cancel).map_err(|e| format!("design: {e:?}"))?;
        if let DesignCommand::SetOverride { node_id, .. } = &command
            && !definition_layers(&doc, &design.component(&resolved.component_id).ok_or("design: member missing")?.layer_id)?.iter().any(|(l, _)| &l.layer_id == node_id)
        {
            return Err(format!("design: {node_id} is not a layer of the instance's definition"));
        }
        let member = design.component(&resolved.component_id).ok_or("design: member missing")?;
        let (corigin, cpure) = *receipt.origins.get(&member.component_id).ok_or("design: the definition has no recorded origin")?;
        if !(binding.pure && cpure) {
            return Err("design: instance_transform_not_translation (a rotated or scaled instance or definition is not redrawn)".into());
        }
        let d = [binding.origin[0] - corigin[0], binding.origin[1] - corigin[1]];
        let mut notes = self.redraw(&doc, &binding.layer_id, &member.layer_id, &next, d)?;
        if base != record {
            notes.push("override_without_value_dropped".into());
        }
        let doc = self.run.snapshot().ok_or("nothing open")?.document();
        let sig = signature(doc, &binding.layer_id);
        let revision = doc.revision;
        self.design.put(binding.instance_id.clone(), sig, next.clone());
        Ok(json!({
            "command": key, "layer_id": binding.layer_id, "instance_id": binding.instance_id,
            "component_id": resolved.component_id, "main_component_id": next.main_component_id,
            "dropped": dropped.iter().map(|o| json!({"node_id": o.node_id, "field": o.field})).collect::<Vec<_>>(),
            "notes": notes, "revision": revision,
        }))
    }

    /// Replaces the children of `group` with a translated copy of the definition under `root`
    /// with `record`'s drawn overrides applied, as one chronicle transaction (entries first).
    #[allow(clippy::too_many_lines)]
    fn redraw(&mut self, doc: &StudioDocument, group: &str, root: &str, record: &StudioComponentInstance, d: [f64; 2]) -> Result<Vec<String>, String> {
        let mut notes: Vec<String> = Vec::new();
        let defs = definition_layers(doc, root)?;
        let mut ventry = self.vector_entry()?;
        let mut tentry = self.text_entry()?;
        let over = |node: &str, field: &str| record.overrides.iter().find(|o| o.overridden_node_id == node).and_then(|o| o.values.get(field));
        let vrecords = ventry.folio_records(&doc.document_id);
        if defs.iter().any(|(l, _)| vrecords.attachments.iter().any(|a| a.layer_id == l.layer_id)) {
            notes.push("definition_clips_and_masks_not_copied".into());
        }
        if self.records.as_ref().is_some_and(|r| defs.iter().any(|(l, _)| r.layers.iter().any(|x| serde_json::to_value(x).ok().and_then(|v| v.get("layer_id").cloned()) == Some(json!(l.layer_id))))) {
            notes.push("definition_effects_not_copied".into());
        }
        // Deletes: the group's current stacked children, deepest first.
        fn post_order(doc: &StudioDocument, id: &str, out: &mut Vec<String>) {
            if doc.layers.iter().any(|l| l.layer_id == id && matches!(l.payload, Payload::Group)) {
                for c in stack(doc, Some(id)).iter().rev() {
                    post_order(doc, c, out);
                }
            }
            out.push(id.to_owned());
        }
        let mut writes = Vec::new();
        for c in stack(doc, Some(group)).iter().rev() {
            let mut order = Vec::new();
            post_order(doc, c, &mut order);
            writes.extend(order.into_iter().map(|id| PlacementWrite { layer_id: id, to: None, create: None }));
        }
        // Creates: each definition layer copied (bottom first, each to the top of its new parent).
        let uuid = doc.document_id.split_once('-').map_or("", |(_, u)| u);
        let (high, low) = (uuid.get(0..8).unwrap_or("00000000"), uuid.get(9..13).unwrap_or("0000"));
        let used_paths: BTreeSet<String> = ventry.paths.iter().map(|p| p.path_id.clone()).collect();
        let used_stories: BTreeSet<String> = tentry.stories.iter().map(|s| s.story_id.clone()).collect();
        let mut counter = u64::try_from(ventry.paths.len().saturating_add(tentry.stories.len())).unwrap_or(0);
        let mut fresh = |prefix: &str, used: &BTreeSet<String>| loop {
            counter = counter.saturating_add(1);
            let id = format!("{prefix}-{high}-{low}-7f17-8000-{counter:012x}");
            if !used.contains(&id) {
                return id;
            }
        };
        let mut salt = u64::try_from(doc.layers.len()).unwrap_or(0);
        let mut new_id: BTreeMap<String, String> = BTreeMap::new();
        let mut minted: BTreeSet<String> = BTreeSet::new();
        let mut new_paths: Vec<PathRecord> = Vec::new();
        let mut new_nodes = Vec::new();
        let design = ventry.design.clone().unwrap_or_else(|| StudioDesignRecords::new(doc.document_id.clone()));
        for (l, parent) in &defs {
            let mut id = mint_layer_id(doc, salt);
            while minted.contains(&id) {
                salt = salt.saturating_add(1);
                id = mint_layer_id(doc, salt);
            }
            salt = salt.saturating_add(1);
            minted.insert(id.clone());
            new_id.insert(l.layer_id.clone(), id.clone());
            let target = if parent == root { group.to_owned() } else { new_id.get(parent).cloned().ok_or("design: definition parent order")? };
            let mut copy = (*l).clone();
            copy.layer_id.clone_from(&id);
            copy.parent = Parent::Node(NodeRef { node_kind: NodeKind::Layer, node_id: target.clone() });
            if let Some(StudioValue::Boolean { value }) = over(&l.layer_id, "visible") {
                copy.visible = *value;
            }
            if let Some(StudioValue::Float { value }) = over(&l.layer_id, "opacity") {
                let group_kind = matches!(copy.payload, Payload::Group);
                let mut a = copy.appearance.clone().unwrap_or_else(|| folio::LayerAppearance::identity(group_kind));
                a.opacity = (value * 100.0).clamp(0.0, 100.0);
                copy.appearance = Some(a);
            }
            match &l.payload {
                Payload::Group => {}
                Payload::Vector { path_ids } => {
                    let mut ids = Vec::with_capacity(path_ids.len());
                    for pid in path_ids {
                        let Some(rec) = ventry.path(pid) else { continue };
                        let mut moved = rec.clone();
                        for c in &mut moved.path.contours {
                            for a in &mut c.anchors {
                                a.position = PathPoint::new(a.position.x + d[0], a.position.y + d[1]);
                            }
                        }
                        if moved.network.is_some() || moved.width.is_some() {
                            notes.push("network_or_width_profile_not_copied".into());
                            moved.network = None;
                            moved.width = None;
                        }
                        let taken: BTreeSet<String> = used_paths.iter().cloned().chain(new_paths.iter().map(|p: &PathRecord| p.path_id.clone())).collect();
                        moved.path_id = fresh("SVPT", &taken);
                        ids.push(moved.path_id.clone());
                        new_paths.push(moved);
                    }
                    copy.payload = Payload::Vector { path_ids: ids };
                    if let Some(n) = design.nodes.iter().find(|n| n.node.node_id == l.layer_id) {
                        let mut n = n.clone();
                        n.node.node_id.clone_from(&id);
                        new_nodes.push(n);
                    }
                }
                Payload::Text { story_id } => {
                    let rec = tentry.layers.iter().find(|r| r.layer_id == l.layer_id).cloned().ok_or_else(|| format!("design: text layer {} has no text record", l.layer_id))?;
                    let mut story = tentry.stories.iter().find(|s| &s.story_id == story_id).cloned().ok_or("design: story missing")?;
                    let taken: BTreeSet<String> = used_stories.iter().cloned().chain(tentry.stories.iter().map(|s| s.story_id.clone())).collect();
                    let sid = fresh("STXT", &taken);
                    story.story_id.clone_from(&sid);
                    if let Some(StudioValue::String { value }) = over(&l.layer_id, "characters") {
                        let len = u64::try_from(value.len()).unwrap_or(u64::MAX);
                        story.content = folio::StoryContent::Inline { text_utf8: value.clone() };
                        let lang = story.character_ranges.first().and_then(|r| r.applied_language.clone());
                        story.paragraphs = vec![serde_json::from_value(json!({"start": 0, "end": len, "style_ref": rec.paragraph_style, "paragraph_direction": null})).map_err(|e| format!("design: {e}"))?];
                        story.character_ranges = vec![serde_json::from_value(json!({"start": 0, "end": len, "style_ref": rec.character_style, "applied_language": lang})).map_err(|e| format!("design: {e}"))?];
                    }
                    tentry.stories.push(story);
                    tentry.layers.push(TextLayerRecord { layer_id: id.clone(), story_id: sid.clone(), origin_px: [rec.origin_px[0] + d[0], rec.origin_px[1] + d[1]], ..rec });
                    copy.payload = Payload::Text { story_id: sid };
                }
                _ => return Err(format!("design: definition layer {} ({}) is not redrawn here", l.layer_id, l.kind)),
            }
            let op = operation_for(&id, matches!(copy.payload, Payload::Group))?;
            writes.push(PlacementWrite { layer_id: id, to: Some(Place::Inside(target)), create: Some((copy, op)) });
        }
        if tentry.layers.len() > text::MAX_TEXT_LAYERS {
            return Err(format!("design: the redraw passes the window's {} text layers", text::MAX_TEXT_LAYERS));
        }
        // Entries first (append-only records; the layers then render at once), then the layers.
        ventry.paths.extend(new_paths);
        if !new_nodes.is_empty() {
            let mut d2 = design.clone();
            d2.nodes.extend(new_nodes);
            ventry.design = Some(d2);
        }
        let vbytes = ventry.to_bytes(&doc.document_id)?;
        self.run.set_document_entry(vector::ENTRY, Some(vbytes)).map_err(|e| format!("design: vector entry {e}"))?;
        if !tentry.layers.is_empty() {
            let tbytes = TextEntry::to_bytes(&tentry)?;
            self.run.set_document_entry(text::ENTRY, Some(tbytes)).map_err(|e| format!("design: text entry {e}"))?;
        }
        notes.sort();
        notes.dedup();
        self.structure("design_instance", writes, format!("redrew instance {group}"))?;
        Ok(notes)
    }

    /// The design-instance section of the Properties panel for the selected layer (drawn only when
    /// it draws an instance; [STU-DS-160]).
    pub(in crate::view) fn design_section_ui(&mut self, ui: &mut egui::Ui, layer_id: &str) {
        let Some(view) = self.instance_view(layer_id) else { return };
        let mut run: Option<DesignCommand> = None;
        ui.separator();
        ui.push_id(super::super::widget::id(widget::SECTION), |ui| {
            ui.strong("Component instance");
            ui.label(format!("Component: {}", view.component_name.as_deref().unwrap_or("?")));
            if view.state == "unknown" {
                ui.colored_label(egui::Color32::from_rgb(200, 120, 0), "The instance's layers changed outside this section or in an earlier session: its record is unknown (import the design file again to edit).");
            }
            if let Some(e) = &view.error {
                ui.colored_label(egui::Color32::RED, e);
            }
            for (prop, (values, cur)) in &view.variants {
                ui.horizontal_wrapped(|ui| {
                    ui.label(prop);
                    for v in values {
                        let id = widget::variant(prop, v);
                        if ui.push_id(super::super::widget::id(&id), |ui| ui.selectable_label(v == cur, v.as_str())).inner.on_hover_text(format!("Switch {prop} to {v}")).clicked() && v != cur {
                            run = Some(DesignCommand::SwitchVariant { property: prop.clone(), value: v.clone() });
                        }
                        self.drawn.push(id);
                    }
                });
            }
            for (name, kind, value) in &view.properties {
                let id = widget::property(name);
                match (kind, value) {
                    (ComponentPropertyType::Boolean, StudioPropertyValue::Boolean(b)) => {
                        let mut on = *b;
                        if ui.push_id(super::super::widget::id(&id), |ui| ui.checkbox(&mut on, name.as_str())).inner.changed() {
                            run = Some(DesignCommand::SetProperty { name: name.clone(), value: StudioPropertyValue::Boolean(on) });
                        }
                    }
                    (ComponentPropertyType::Text, StudioPropertyValue::Text(t)) => {
                        let buf = self.design.buffers.entry(format!("property/{name}")).or_insert_with(|| t.clone());
                        let r = ui.horizontal(|ui| {
                            ui.label(name);
                            ui.push_id(super::super::widget::id(&id), |ui| ui.text_edit_singleline(buf)).inner
                        });
                        if r.inner.lost_focus() && buf != t {
                            run = Some(DesignCommand::SetProperty { name: name.clone(), value: StudioPropertyValue::Text(buf.clone()) });
                        }
                    }
                    _ => {
                        ui.label(format!("{name}: {kind:?} (read-only here)"));
                    }
                }
                self.drawn.push(id);
            }
            for (node, name, current) in &view.texts {
                let id = widget::text(node);
                let buf = self.design.buffers.entry(format!("text/{}/{node}", view.instance_id)).or_insert_with(|| current.clone());
                if !ui.memory(|m| m.has_focus(super::super::widget::id(&id))) && buf != current {
                    buf.clone_from(current);
                }
                let r = ui.horizontal(|ui| {
                    ui.label(format!("Text {name}"));
                    ui.add(egui::TextEdit::singleline(buf).id(super::super::widget::id(&id)))
                });
                if r.inner.lost_focus() && buf != current {
                    run = Some(DesignCommand::SetOverride { node_id: node.clone(), field: "characters".into(), value: StudioValue::String { value: buf.clone() } });
                }
                self.drawn.push(id);
            }
            ui.label(format!("Overrides ({})", view.overrides.len()));
            for (node, name, field, value) in &view.overrides {
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!("{name} . {field} = {value}"));
                    let id = widget::reset(node, field);
                    if ui.push_id(super::super::widget::id(&id), |ui| ui.button(format!("Reset {field}"))).inner.on_hover_text("Reset this override to the component's value").clicked() {
                        run = Some(DesignCommand::ResetOverride { node_id: node.clone(), field: field.clone() });
                    }
                    self.drawn.push(id);
                });
            }
            if ui.push_id(super::super::widget::id(widget::RESET_ALL), |ui| ui.add_enabled(!view.overrides.is_empty(), egui::Button::new("Reset all overrides"))).inner.clicked() {
                run = Some(DesignCommand::ResetAll);
            }
            self.drawn.push(widget::RESET_ALL.into());
            if !view.swaps.is_empty() {
                ui.label("Swap component");
                ui.horizontal_wrapped(|ui| {
                    for (id_k, name) in &view.swaps {
                        let id = widget::swap(id_k);
                        if ui.push_id(super::super::widget::id(&id), |ui| ui.button(format!("Swap to {name}"))).inner.clicked() {
                            run = Some(DesignCommand::Swap { component_id: id_k.clone() });
                        }
                        self.drawn.push(id);
                    }
                });
            }
        });
        self.drawn.push(widget::SECTION.into());
        if let Some(c) = run {
            let _ = self.design_command(layer_id, c);
        }
    }

    /// `state_json()["design"]`: the selected layer's instance (or null) and the last command.
    pub fn design_json(&self) -> Value {
        let instance = self.selected.as_deref().and_then(|l| self.instance_view(l)).map(|v| v.json());
        let bound = self.design_receipt().map(|r| r.bindings.len());
        json!({"instance": instance, "instance_layers": bound, "last": self.design.last})
    }
}
