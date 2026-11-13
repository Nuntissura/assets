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
//! earlier children, return the inspector to the record that drew them. Each command also stores
//! the new record as the instance's state in the session entry `design` (lane SMB-FIG-EDIT-2,
//! [`super::persist`]), keyed the same way, so after a save and reopen an edited instance reads
//! `edited` and takes further commands. An instance whose layers were changed outside the
//! inspector (no state of this session or of the entry matches its children) reports
//! `state: "unknown"` and refuses commands until the design file is imported again; an entry state
//! Folio refuses makes its instance `unknown` with the Folio diagnostic as the reason.
//!
//! Commands ([`DesignCommand`], one chronicle transaction = one undo step each): variant switch
//! (`hsk_studio_components::switch_variant`), component swap (`swap_component`; overrides re-matched
//! by layer name and hierarchy, dropped ones reported), component property values (STU-DS-009:
//! boolean and text), override set (`characters`, `visible`, `opacity`; the fields the redraw
//! applies), per-override reset and reset-all (STU-DS-017). The redraw replaces the instance
//! layer's children (a native redraw of the same member keeps the ids of the copies of the same
//! definition nodes and updates them in place, lead ruling 2026-11-13 stable identity, [`Redraw::instance`])
//! with a copy of the resolved member's definition layers, translated by the
//! instance origin minus the definition origin (both from the receipt; a rotated or scaled
//! instance or definition is refused), with the overrides applied: new path records (vector entry,
//! same paint style), new stories (text entry, `characters` replaced). Clips, masks and effects of
//! definition layers are not copied (listed in the command result's `notes`).
//!
//! Authored records (lane SMB-DS-COMPONENTS-AUTHOR-1, [`super::component`]): the vector entry's
//! `design` member also holds components, sets and instances authored in Studio. The inspector reads
//! the receipt's records and the entry's (settled: a definition whose layer is gone is dropped and its
//! instances read `main_component_id` null, [STU-DS-003a]) as ONE record set; an id in both is
//! `design_record_duplicate`. A native instance is bound by its states in the `design` entry (layer,
//! instance, content); its record is the entry's instance record, which every command rewrites in the
//! same history step as the redraw (`Run::restructure_and_entry`), so undo and redo restore record and
//! layers together; a binding is never pruned by a later write ([`super::persist`]). A native redraw
//! drops, in its own step, the node entries, live filter stacks and paths of the copies it deletes;
//! a copy carries its definition layer's live filter stack (moved with the copy). Text records of
//! copies (lead ruling P2 2026-11-13, lane SMB-DS-VARIABLES-AUTHOR-1, superseding R1's in-session cap):
//! a native redraw compacts the `text` records of the copies the document no longer draws and writes the
//! text entry INSIDE its own history step (`Run::restructure_and_entries`, with the vector entry), so its
//! undo brings them back byte for byte and redo compacts again; no record an undo can reach is lost and
//! the per-session sync cap is gone (a redraw still past the entry's cap after compaction is refused
//! `text_layer_over_cap`). A design file instance's redraw ([`Viewer::design_command`]) keeps its text
//! writes outside the history and so never compacts (R1); the records of copies no longer in the document
//! are compacted when it is opened with no history ([`compact_copy_text`]). Native origins are the layers' bounds at command time (Studio reading: the
//! top-left of the vector bounds and text origins under the layer); a native redraw also draws the
//! `visible` and `characters` fields the component properties drive ([STU-DS-113], resolved by
//! `resolve_instance`).
#![deny(clippy::arithmetic_side_effects)]
use super::super::layers::{mint_or_refuse, operation_for, stack};
use super::super::text::{self, TextEntry, TextLayerRecord};
use super::super::vector::live_filters::transform_live_filters;
use super::super::vector::{self, PathRecord, VectorEntry};
use super::super::Viewer;
use super::persist;
use crate::{Place, PlacementWrite};
use hsk_studio_components::{Design, LocalContent, Source, resolve_instance, swap_component, switch_variant};
use hsk_studio_folio::{
    self as folio, ComponentPropertyType, NodeKind, NodeRef, Parent, Payload, StudioComponentInstance, StudioDesignRecords, StudioDesignStateRecords,
    StudioDocument, StudioInstanceProperty, StudioLayer, StudioNodeDesign, StudioOverrideRecord, StudioPropertyValue, StudioValue,
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

/// One instance binding of the receipt (or, `native`, of an authored instance).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Binding {
    pub(super) layer_id: String,
    pub(super) instance_id: String,
    pub(super) origin: [f64; 2],
    pure: bool,
    signature: Option<String>,
    /// An authored instance (its record is the vector entry's).
    pub(super) native: bool,
}

/// The receipt's design member, parsed, merged with the vector entry's authored records (module docs).
pub(super) struct Receipt {
    /// Receipt and authored records as one set.
    pub(super) records: StudioDesignRecords,
    pub(super) bindings: Vec<Binding>,
    /// component id -> (origin, translation only).
    origins: BTreeMap<String, ([f64; 2], bool)>,
    /// Authored instance records by instance id.
    pub(super) native: BTreeMap<String, StudioComponentInstance>,
    /// Contents each native instance layer was drawn with (from its `design` entry states).
    contents: BTreeMap<String, Vec<Vec<String>>>,
    /// The vector entry's authored records, settled (what authoring commands rewrite).
    pub(super) authored: Option<StudioDesignRecords>,
    /// The merge was refused (an id in both sets): commands refuse with it.
    pub(super) error: Option<String>,
    /// Instance ids of the design file's records (every other state is a native binding).
    pub(super) design_file: BTreeSet<String>,
}

/// Most `design` entry bytes read for native bindings (Folio's default input budget).
const MAX_BINDING_BYTES: usize = 1_048_576;

/// Settles authored records against `doc` ([STU-DS-003a]): a component or set whose container layer
/// is gone (or not a group) is dropped, a member of a dropped set loses its assignment, a set whose
/// default member is gone takes its first remaining member (none: the set goes), and an instance
/// whose main component is gone reads `main_component_id` null. Returns the ids it dropped.
pub(super) fn settle(r: &mut StudioDesignRecords, doc: &StudioDocument) -> Vec<String> {
    let groups: BTreeSet<&str> = doc.layers.iter().filter(|l| l.kind == "group" && matches!(l.payload, Payload::Group)).map(|l| l.layer_id.as_str()).collect();
    let parent = |id: &str| doc.layers.iter().find(|l| l.layer_id == id).and_then(|l| match &l.parent {
        Parent::Node(n) => Some(n.node_id.clone()),
        Parent::Root(()) => None,
    });
    let mut dropped = Vec::new();
    r.components.retain(|k| groups.contains(k.layer_id.as_str()) || {
        dropped.push(k.component_id.clone());
        false
    });
    r.component_sets.retain(|s| groups.contains(s.layer_id.as_str()) || {
        dropped.push(s.component_id.clone());
        false
    });
    let set_layers: BTreeSet<String> = r.component_sets.iter().map(|s| s.layer_id.clone()).collect();
    for k in &mut r.components {
        if k.variant_properties.is_some() && !parent(&k.layer_id).is_some_and(|p| set_layers.contains(&p)) {
            k.variant_properties = None;
        }
    }
    let members = |s: &folio::StudioComponentSet, r: &[folio::StudioComponent]| -> Vec<String> {
        r.iter().filter(|k| parent(&k.layer_id).as_deref() == Some(s.layer_id.as_str())).map(|k| k.component_id.clone()).collect()
    };
    let components = r.components.clone();
    r.component_sets.retain_mut(|s| {
        let list = members(s, &components);
        if !list.contains(&s.default_variant_id) {
            match list.first() {
                Some(first) => s.default_variant_id.clone_from(first),
                None => {
                    dropped.push(s.component_id.clone());
                    return false;
                }
            }
        }
        true
    });
    let known: BTreeSet<&str> = r.components.iter().map(|k| k.component_id.as_str()).collect();
    let mut gone = Vec::new();
    for i in &mut r.component_instances {
        if i.main_component_id.as_deref().is_some_and(|m| !known.contains(m)) {
            i.main_component_id = None;
            gone.push(i.instance_id.clone());
        }
    }
    dropped.extend(gone);
    dropped
}

/// The top-left of what `layer` draws: the minimum of its vector layers' path bounds and its text
/// layers' origins, the layer itself included ([0, 0] when it draws nothing; Studio reading).
pub(super) fn origin_of_layer(doc: &StudioDocument, ventry: &VectorEntry, tentry: &TextEntry, layer: &str) -> [f64; 2] {
    let mut min: Option<[f64; 2]> = None;
    let mut take = |p: [f64; 2]| {
        min = Some(match min {
            Some(m) => [m[0].min(p[0]), m[1].min(p[1])],
            None => p,
        });
    };
    let mut todo = vec![(layer.to_owned(), 0_usize)];
    let mut seen = 0_usize;
    while let Some((id, depth)) = todo.pop() {
        seen = seen.saturating_add(1);
        if depth > 64 || seen > MAX_REDRAW_LAYERS {
            break;
        }
        let Some(l) = doc.layers.iter().find(|l| l.layer_id == id) else { continue };
        match &l.payload {
            Payload::Group => todo.extend(stack(doc, Some(&id)).into_iter().map(|c| (c, depth.saturating_add(1)))),
            Payload::Vector { path_ids } => {
                for pid in path_ids {
                    if let Some(b) = ventry.path(pid).and_then(|p| p.path.to_owned_path().ok()).and_then(|o| hsk_studio_nib::arrange::path_bounds(&o)) {
                        take(b.min);
                    }
                }
            }
            Payload::Text { .. } => {
                if let Some(r) = tentry.layers.iter().find(|r| r.layer_id == id) {
                    take(r.origin_px);
                }
            }
            _ => {}
        }
    }
    min.unwrap_or([0.0, 0.0])
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
                native: false,
            })
        })
        .collect();
    let origins = d
        .get("components")?
        .as_array()?
        .iter()
        .filter_map(|c| Some((c["component_id"].as_str()?.to_owned(), (origin_of(&c["origin_px"])?, c["translation_only"].as_bool()?))))
        .collect();
    let design_file = records.component_instances.iter().map(|i| i.instance_id.clone()).collect();
    Some(Receipt { records, bindings, origins, native: BTreeMap::new(), contents: BTreeMap::new(), authored: None, error: None, design_file })
}

/// The ids `a` and `b` both claim (components and sets share one namespace; instances another).
fn shared_id(a: &StudioDesignRecords, b: &StudioDesignRecords) -> Option<String> {
    let defs: BTreeSet<&str> = a.components.iter().map(|k| k.component_id.as_str()).chain(a.component_sets.iter().map(|s| s.component_id.as_str())).collect();
    let insts: BTreeSet<&str> = a.component_instances.iter().map(|i| i.instance_id.as_str()).collect();
    b.components
        .iter()
        .map(|k| k.component_id.as_str())
        .chain(b.component_sets.iter().map(|s| s.component_id.as_str()))
        .find(|id| defs.contains(id))
        .or_else(|| b.component_instances.iter().map(|i| i.instance_id.as_str()).find(|id| insts.contains(id)))
        .map(str::to_owned)
}

/// True when a node entry carries no design member (it is dropped rather than stored).
pub(super) fn carries_nothing(n: &StudioNodeDesign) -> bool {
    n.auto_layout.is_none()
        && n.child_layout.is_none()
        && n.size_clamps.is_none()
        && n.constraints.is_none()
        && n.reactions.is_empty()
        && n.component_property_references.is_empty()
        && n.explicit_variable_modes.is_empty()
        && n.style_refs.is_empty()
}

/// True when the record set holds authored component records (components, sets or instances).
pub(super) fn authors_components(d: &StudioDesignRecords) -> bool {
    !d.components.is_empty() || !d.component_sets.is_empty() || !d.component_instances.is_empty()
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

/// Fields a redraw draws on the copied definition layers: (definition node id, field) -> value.
pub(super) type Drawn = BTreeMap<(String, String), StudioValue>;

/// The drawn overrides of `record` (the [`DRAWN_OVERRIDES`] fields with a value).
fn drawn_overrides(record: &StudioComponentInstance) -> Drawn {
    let mut out = Drawn::new();
    for o in &record.overrides {
        for (f, v) in &o.values {
            if DRAWN_OVERRIDES.contains(&f.as_str()) {
                out.entry((o.overridden_node_id.clone(), f.clone())).or_insert_with(|| v.clone());
            }
        }
    }
    out
}

/// One authored instance to redraw ([`Viewer::native_redraw`]).
pub(super) struct NativeJob {
    /// The group layer that draws the instance.
    pub(super) group: String,
    /// The record to draw (stored in the vector entry by the same step).
    pub(super) record: StudioComponentInstance,
    /// Where the copy's top-left goes.
    pub(super) origin: [f64; 2],
    /// The component swap target the binding state keeps.
    pub(super) swap: Option<String>,
}

/// The writes and entry records of one or more instance redraws, accumulated for one transaction.
pub(super) struct Redraw<'d> {
    doc: &'d StudioDocument,
    pub(super) ventry: VectorEntry,
    tentry: TextEntry,
    design: StudioDesignRecords,
    attachments: BTreeSet<String>,
    effects: BTreeSet<String>,
    writes: Vec<PlacementWrite>,
    notes: Vec<String>,
    used_paths: BTreeSet<String>,
    used_stories: BTreeSet<String>,
    new_paths: Vec<PathRecord>,
    new_nodes: Vec<StudioNodeDesign>,
    minted: BTreeSet<String>,
    salt: u64,
    counter: u64,
    /// Kept copies whose node entry the redraw rewrites ([`Self::finish`] drops their old entry).
    replaced_nodes: BTreeSet<String>,
}

impl<'d> Redraw<'d> {
    pub(super) fn new(v: &Viewer, doc: &'d StudioDocument) -> Result<Self, String> {
        let ventry = v.vector_entry()?;
        let tentry = v.text_entry()?;
        let attachments = ventry.folio_records(&doc.document_id).attachments.iter().map(|a| a.layer_id.clone()).collect();
        let effects = v.records.as_ref().map(|r| r.layers.iter().map(|x| x.layer_id.clone()).collect()).unwrap_or_default();
        let design = ventry.design.clone().unwrap_or_else(|| StudioDesignRecords::new(doc.document_id.clone()));
        let used_paths = ventry.paths.iter().map(|p| p.path_id.clone()).collect();
        let used_stories = tentry.stories.iter().map(|s| s.story_id.clone()).collect();
        let counter = u64::try_from(ventry.paths.len().saturating_add(tentry.stories.len())).unwrap_or(0);
        Ok(Self {
            doc,
            ventry,
            tentry,
            design,
            attachments,
            effects,
            writes: Vec::new(),
            notes: Vec::new(),
            used_paths,
            used_stories,
            new_paths: Vec::new(),
            new_nodes: Vec::new(),
            minted: BTreeSet::new(),
            salt: u64::try_from(doc.layers.len()).unwrap_or(0),
            counter,
            replaced_nodes: BTreeSet::new(),
        })
    }

    /// Keeps `id` (a layer the same transaction creates) out of the minted ids.
    pub(super) fn reserve(&mut self, id: &str) {
        self.minted.insert(id.to_owned());
    }

    /// A fresh layer id, or the typed `layer_id_space_exhausted` refusal before anything is written (review F3).
    fn mint(&mut self) -> Result<String, String> {
        let mut id = mint_or_refuse(self.doc, self.salt)?;
        while self.minted.contains(&id) {
            self.salt = self.salt.saturating_add(1);
            id = mint_or_refuse(self.doc, self.salt)?;
        }
        self.salt = self.salt.saturating_add(1);
        self.minted.insert(id.clone());
        Ok(id)
    }

    fn fresh(&mut self, prefix: &str, used: &BTreeSet<String>) -> String {
        let uuid = self.doc.document_id.split_once('-').map_or("", |(_, u)| u);
        let (high, low) = (uuid.get(0..8).unwrap_or("00000000"), uuid.get(9..13).unwrap_or("0000"));
        loop {
            self.counter = self.counter.saturating_add(1);
            let id = format!("{prefix}-{high}-{low}-7f17-8000-{:012x}", self.counter);
            if !used.contains(&id) {
                return id;
            }
        }
    }

    /// Redraws the children of `group` as a copy of the definition under `root` translated by `d`, with
    /// `drawn` applied.
    ///
    /// Stable identity (lane SMB-PS-FILTER-ADJUST-COMMANDS-1, lead ruling 2026-11-13, option A): with `keep` (a
    /// native redraw that draws the same member as before, such as a sync) a current copy that is the same
    /// definition node keeps its layer id and is updated in place ([`Self::matched_copies`]: same parent node,
    /// kind and name, its layer fields as the redraw would draw them, its path or story records present); its
    /// paths and story are rewritten under their own ids, so the layer record itself does not change and no
    /// write names it unless the stack order must change (a move). A definition node new in this redraw gets a
    /// fresh id (`layers::mint_or_refuse`); a copy whose node is gone, or that cannot be updated in place, is
    /// deleted and its id retired (never minted again). Records keyed to a kept copy (its node entry's explicit
    /// variable modes, a live filter stack the definition does not author, raster-records rows) survive the
    /// redraw; a definition's live filter stack replaces the copy's (F8). Without `keep` (a swap to another
    /// member, a design file instance) every copy is new, as before.
    #[allow(clippy::too_many_lines)]
    pub(super) fn instance(&mut self, group: &str, root: &str, drawn: &Drawn, d: [f64; 2], keep: bool) -> Result<(), String> {
        let doc = self.doc;
        let defs = definition_layers(doc, root)?;
        if defs.iter().any(|(l, _)| self.attachments.contains(&l.layer_id)) {
            self.notes.push("definition_clips_and_masks_not_copied".into());
        }
        if defs.iter().any(|(l, _)| self.effects.contains(&l.layer_id)) {
            self.notes.push("definition_effects_not_copied".into());
        }
        let old = definition_layers(doc, group).unwrap_or_default();
        let kept = if keep { self.matched_copies(&defs, root, group, &old, drawn) } else { BTreeMap::new() };
        let kept_ids: BTreeSet<String> = kept.values().cloned().collect();
        // Deletes: the group's current stacked children the redraw does not keep, deepest first.
        fn post_order(doc: &StudioDocument, id: &str, out: &mut Vec<String>) {
            if doc.layers.iter().any(|l| l.layer_id == id && matches!(l.payload, Payload::Group)) {
                for c in stack(doc, Some(id)).iter().rev() {
                    post_order(doc, c, out);
                }
            }
            out.push(id.to_owned());
        }
        for c in stack(doc, Some(group)).iter().rev() {
            let mut order = Vec::new();
            post_order(doc, c, &mut order);
            self.writes.extend(order.into_iter().filter(|id| !kept_ids.contains(id)).map(|id| PlacementWrite { layer_id: id, to: None, create: None }));
        }
        // Review F6 (lane SMB-DS-VARIABLES-AUTHOR-1): explicit variable modes set on a copy this redraw
        // deletes move to the copy of the same definition layer that replaces it (matched by position when
        // the copy and the definition have the same layers, else by kind and name); one that matches
        // nothing is a named loss. A kept copy keeps its own modes.
        let mut carried: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        let mut matched: BTreeSet<usize> = BTreeSet::new();
        for (i, (o, _)) in old.iter().enumerate() {
            if kept_ids.contains(&o.layer_id) {
                continue;
            }
            let Some(modes) = self.design.node_design(&o.layer_id).map(|n| n.explicit_variable_modes.clone()).filter(|m| !m.is_empty()) else { continue };
            let same = |j: usize| defs.get(j).is_some_and(|(dl, _)| dl.kind == o.kind && dl.name == o.name && !kept.contains_key(&dl.layer_id)) && !matched.contains(&j);
            let at = if old.len() == defs.len() && same(i) { Some(i) } else { (0..defs.len()).find(|j| same(*j)) };
            match at.and_then(|j| defs.get(j).map(|(dl, _)| (j, dl.layer_id.clone()))) {
                Some((j, def)) => {
                    matched.insert(j);
                    carried.insert(def, modes);
                }
                None => self.notes.push("explicit_modes_not_carried".into()),
            }
        }
        // Creates and in-place updates in definition order (bottom first, each to the top of its parent).
        let mut new_id: BTreeMap<String, String> = BTreeMap::new();
        let mut placed: Vec<(String, String, Option<(StudioLayer, folio::Operation)>)> = Vec::new();
        for (l, parent) in &defs {
            let target = if parent == root { group.to_owned() } else { new_id.get(parent).cloned().ok_or("design: definition parent order")? };
            if let Some(cid) = kept.get(&l.layer_id) {
                new_id.insert(l.layer_id.clone(), cid.clone());
                self.update_copy(l, cid, drawn, d)?;
                placed.push((cid.clone(), target, None));
                continue;
            }
            let id = self.mint()?;
            new_id.insert(l.layer_id.clone(), id.clone());
            let mut copy = drawn_copy(l, drawn);
            copy.layer_id.clone_from(&id);
            copy.parent = Parent::Node(NodeRef { node_kind: NodeKind::Layer, node_id: target.clone() });
            match &l.payload {
                Payload::Group => {
                    if let Some(modes) = carried.remove(&l.layer_id) {
                        self.new_nodes.push(modes_node(&id, modes));
                    }
                }
                Payload::Vector { path_ids } => {
                    let mut ids = Vec::with_capacity(path_ids.len());
                    for pid in path_ids {
                        let Some(rec) = self.ventry.path(pid).cloned() else { continue };
                        let mut moved = self.moved_path(l, rec, d)?;
                        let taken: BTreeSet<String> = self.used_paths.iter().cloned().chain(self.new_paths.iter().map(|p: &PathRecord| p.path_id.clone())).collect();
                        moved.path_id = self.fresh("SVPT", &taken);
                        ids.push(moved.path_id.clone());
                        self.new_paths.push(moved);
                    }
                    copy.payload = Payload::Vector { path_ids: ids };
                    // F8: the copy carries the definition layer's live filter stack, its free distort
                    // corners moved with the copy; a stack that cannot be carried is a named loss.
                    self.carry_live_filters(&l.layer_id, &id, d);
                    if let Some(n) = self.copied_node(&l.layer_id, &id, None)
                        && !carries_nothing(&n)
                    {
                        self.new_nodes.push(n);
                    }
                }
                Payload::Text { story_id } => {
                    let (story, record) = self.copied_text(l, story_id, None, &id, drawn, d)?;
                    let sid = story.story_id.clone();
                    self.tentry.stories.push(story);
                    self.tentry.layers.push(record);
                    copy.payload = Payload::Text { story_id: sid };
                }
                _ => return Err(format!("design: definition layer {} ({}) is not redrawn here", l.layer_id, l.kind)),
            }
            let op = operation_for(&id, matches!(copy.payload, Payload::Group))?;
            placed.push((id, target, Some((copy, op))));
        }
        if !carried.is_empty() {
            self.notes.push("explicit_modes_not_carried".into());
        }
        // A kept copy moves only when its parent's stack order must change: the kept copies out of their
        // definition order, or a new copy below a kept one (new copies go to the top of their parent).
        let restack: BTreeSet<String> = placed
            .iter()
            .map(|(_, t, _)| t.clone())
            .collect::<BTreeSet<String>>()
            .into_iter()
            .filter(|t| {
                let wanted: Vec<(&str, bool)> = placed.iter().filter(|(_, pt, _)| pt == t).map(|(id, _, c)| (id.as_str(), c.is_none())).collect();
                let kept_order: Vec<&str> = wanted.iter().filter(|(_, k)| *k).map(|(id, _)| *id).collect();
                let current: Vec<String> = stack(doc, Some(t.as_str())).into_iter().filter(|id| kept_order.contains(&id.as_str())).collect();
                let first_new = wanted.iter().position(|(_, k)| !*k).unwrap_or(wanted.len());
                let kept_after_new = wanted.iter().skip(first_new).any(|(_, k)| *k);
                kept_after_new || current.iter().map(String::as_str).ne(kept_order.iter().copied())
            })
            .collect();
        for (id, target, create) in placed {
            match create {
                Some(c) => self.writes.push(PlacementWrite { layer_id: id, to: Some(Place::Inside(target)), create: Some(c) }),
                None if restack.contains(&target) => self.writes.push(PlacementWrite { layer_id: id, to: Some(Place::Inside(target)), create: None }),
                None => {}
            }
        }
        Ok(())
    }

    /// The copies of `group` (`old`, in definition-layer order) that are the same definition nodes as `defs`
    /// and can be updated in place: definition layer id -> copy id (lead ruling 2026-11-13, option A). A copy
    /// matches a definition node with the same parent node (the group for a top-level node, else the kept copy
    /// of the node's parent), kind and name, taken in order; it is kept when its layer record is the one the
    /// redraw would draw (overrides applied; only its id, parent and payload ids differ) and its path or story
    /// records are present in the same number. A node whose parent is not kept has no kept copy.
    fn matched_copies(&self, defs: &[(&StudioLayer, String)], root: &str, group: &str, old: &[(&StudioLayer, String)], drawn: &Drawn) -> BTreeMap<String, String> {
        let mut kept: BTreeMap<String, String> = BTreeMap::new();
        let mut used: BTreeSet<String> = BTreeSet::new();
        for (l, parent) in defs {
            let want = if parent == root { Some(group.to_owned()) } else { kept.get(parent).cloned() };
            let Some(want) = want else { continue };
            let pick = old.iter().find(|(o, op)| !used.contains(&o.layer_id) && *op == want && o.kind == l.kind && o.name == l.name);
            if let Some((o, _)) = pick
                && self.in_place(l, o, drawn)
            {
                used.insert(o.layer_id.clone());
                kept.insert(l.layer_id.clone(), o.layer_id.clone());
            }
        }
        kept
    }

    /// True when copy `o` of definition layer `l` can be updated in place ([`Self::matched_copies`]).
    fn in_place(&self, l: &StudioLayer, o: &StudioLayer, drawn: &Drawn) -> bool {
        let mut expect = drawn_copy(l, drawn);
        expect.layer_id.clone_from(&o.layer_id);
        expect.parent = o.parent.clone();
        expect.payload = o.payload.clone();
        if expect != *o {
            return false;
        }
        match (&l.payload, &o.payload) {
            (Payload::Group, Payload::Group) => true,
            (Payload::Vector { path_ids }, Payload::Vector { path_ids: own }) => {
                path_ids.iter().filter(|p| self.ventry.path(p).is_some()).count() == own.len() && own.iter().all(|p| self.ventry.path(p).is_some())
            }
            (Payload::Text { story_id }, Payload::Text { story_id: own }) => {
                [story_id, own].iter().all(|s| self.tentry.stories.iter().any(|x| &x.story_id == *s))
                    && [&l.layer_id, &o.layer_id].iter().all(|id| self.tentry.layers.iter().any(|r| &r.layer_id == *id))
            }
            _ => false,
        }
    }

    /// Rewrites the records of the kept copy `cid` of definition layer `l` under the copy's own path and story
    /// ids (stable identity): the translated paths, the definition's live filter stack, the node entry (the
    /// copy's explicit variable modes kept), the story and the text record.
    fn update_copy(&mut self, l: &StudioLayer, cid: &str, drawn: &Drawn, d: [f64; 2]) -> Result<(), String> {
        let doc = self.doc;
        let copy = doc.layers.iter().find(|x| x.layer_id == cid).ok_or_else(|| format!("design: copy {cid} missing"))?;
        match (&l.payload, &copy.payload) {
            (Payload::Group, Payload::Group) => {}
            (Payload::Vector { path_ids }, Payload::Vector { path_ids: own }) => {
                let recs: Vec<PathRecord> = path_ids.iter().filter_map(|p| self.ventry.path(p).cloned()).collect();
                for (rec, pid) in recs.into_iter().zip(own.clone()) {
                    let mut moved = self.moved_path(l, rec, d)?;
                    moved.path_id.clone_from(&pid);
                    if let Some(slot) = self.ventry.paths.iter_mut().find(|p| p.path_id == pid) {
                        *slot = moved;
                    }
                }
                self.carry_live_filters(&l.layer_id, cid, d);
                let modes = self.design.node_design(cid).map(|n| n.explicit_variable_modes.clone()).filter(|m| !m.is_empty());
                self.replaced_nodes.insert(cid.to_owned());
                match self.copied_node(&l.layer_id, cid, modes.clone()) {
                    Some(n) if !carries_nothing(&n) => self.new_nodes.push(n),
                    Some(_) => {}
                    None => {
                        if let Some(m) = modes {
                            self.new_nodes.push(modes_node(cid, m));
                        }
                    }
                }
            }
            (Payload::Text { story_id }, Payload::Text { story_id: own }) => {
                let own = own.clone();
                let (story, record) = self.copied_text(l, story_id, Some(own.clone()), cid, drawn, d)?;
                if let Some(slot) = self.tentry.stories.iter_mut().find(|s| s.story_id == own) {
                    *slot = story;
                }
                if let Some(slot) = self.tentry.layers.iter_mut().find(|r| r.layer_id == cid) {
                    *slot = record;
                }
            }
            _ => return Err(format!("design: copy {cid} of {} changed kind", l.layer_id)),
        }
        Ok(())
    }

    /// Path record `rec` of definition layer `l` translated by `d` (refused past the coordinate limit; a network
    /// or width profile is not copied, a named loss).
    fn moved_path(&mut self, l: &StudioLayer, mut moved: PathRecord, d: [f64; 2]) -> Result<PathRecord, String> {
        for c in &mut moved.path.contours {
            for a in &mut c.anchors {
                a.position = PathPoint::new(a.position.x + d[0], a.position.y + d[1]);
            }
        }
        if moved.path.validate().is_err() {
            return Err(format!("design: instance_coordinate_out_of_range (the copy of {} passes the path coordinate limit)", l.layer_id));
        }
        if moved.network.is_some() || moved.width.is_some() {
            self.notes.push("network_or_width_profile_not_copied".into());
            moved.network = None;
            moved.width = None;
        }
        Ok(moved)
    }

    /// F8: the definition layer `def`'s live filter stack onto copy `id`, its free distort corners moved by `d`
    /// (a kept copy's own stack is replaced only when the definition has one); one that cannot be carried is a
    /// named loss and leaves the copy's stack as it was.
    fn carry_live_filters(&mut self, def: &str, id: &str, d: [f64; 2]) {
        let Some(stack) = self.ventry.records.live_filter_stack(def).cloned() else { return };
        let mut moved = folio::StudioVectorRecords::new(self.doc.document_id.clone());
        moved.live_filters.push(folio::StudioLiveFilterStack { layer_id: id.to_owned(), filters: stack.filters });
        let others = self.ventry.records.live_filters.iter().filter(|s| s.layer_id != id).count();
        let room = others < folio::TOOLS_MAX_ENTRIES.max(vector::MAX_PATHS);
        match transform_live_filters(&mut moved, id, &[1.0, 0.0, 0.0, 1.0, d[0], d[1]], "design") {
            Ok(()) if room => {
                self.ventry.records.live_filters.retain(|s| s.layer_id != id);
                self.ventry.records.live_filters.extend(moved.live_filters);
            }
            _ => self.notes.push("definition_live_filters_not_copied".into()),
        }
    }

    /// The definition layer `def`'s node entry for copy `id` (property references stay with the definition;
    /// `modes`: the kept copy's own explicit variable modes), `None` when the definition has none.
    fn copied_node(&self, def: &str, id: &str, modes: Option<BTreeMap<String, String>>) -> Option<StudioNodeDesign> {
        let mut n = self.design.nodes.iter().find(|n| n.node.node_id == def)?.clone();
        n.node.node_id = id.to_owned();
        // A copy is not inside a definition: its property references stay with the definition.
        n.component_property_references.clear();
        if let Some(m) = modes {
            n.explicit_variable_modes = m;
        }
        Some(n)
    }

    /// The story and text record of copy `id` of text definition layer `l` (story `story_id`): the story under
    /// `own` (a kept copy's story id) or a fresh one, `characters` drawn, the origin translated by `d`.
    fn copied_text(&mut self, l: &StudioLayer, story_id: &str, own: Option<String>, id: &str, drawn: &Drawn, d: [f64; 2]) -> Result<(folio::StudioTextStory, TextLayerRecord), String> {
        let rec = self.tentry.layers.iter().find(|r| r.layer_id == l.layer_id).cloned().ok_or_else(|| format!("design: text layer {} has no text record", l.layer_id))?;
        let mut story = self.tentry.stories.iter().find(|s| s.story_id == story_id).cloned().ok_or("design: story missing")?;
        let sid = match own {
            Some(s) => s,
            None => {
                let taken: BTreeSet<String> = self.used_stories.iter().cloned().chain(self.tentry.stories.iter().map(|s| s.story_id.clone())).collect();
                self.fresh("STXT", &taken)
            }
        };
        story.story_id.clone_from(&sid);
        if let Some(StudioValue::String { value }) = drawn.get(&(l.layer_id.clone(), "characters".to_owned())) {
            let len = u64::try_from(value.len()).unwrap_or(u64::MAX);
            story.content = folio::StoryContent::Inline { text_utf8: value.clone() };
            let lang = story.character_ranges.first().and_then(|r| r.applied_language.clone());
            story.paragraphs = vec![serde_json::from_value(json!({"start": 0, "end": len, "style_ref": rec.paragraph_style, "paragraph_direction": null})).map_err(|e| format!("design: {e}"))?];
            story.character_ranges = vec![serde_json::from_value(json!({"start": 0, "end": len, "style_ref": rec.character_style, "applied_language": lang})).map_err(|e| format!("design: {e}"))?];
        }
        let origin = [rec.origin_px[0] + d[0], rec.origin_px[1] + d[1]];
        if !origin.iter().all(|v| v.is_finite() && v.abs() <= hsk_studio_nib::COORDINATE_LIMIT) {
            return Err(format!("design: instance_coordinate_out_of_range (the copy of {} passes the coordinate limit)", l.layer_id));
        }
        Ok((story, TextLayerRecord { layer_id: id.to_owned(), story_id: sid, origin_px: origin, ..rec }))
    }

    /// Lead ruling P2 (2026-11-13; F7 and R1 before it): with `in_step` (the text entry is written in the
    /// redraw's own history step, [`Viewer::native_redraw`]) the records of copies the document no longer
    /// draws after this redraw are compacted first: the step's undo restores them. Without it (the text
    /// entry is written outside the history) nothing is compacted. A redraw still past the entry's cap is
    /// refused `text_layer_over_cap`.
    fn compact_text(&mut self, in_step: bool) -> Result<(), String> {
        if in_step {
            let after = super::component::project(self.doc, &self.writes);
            compact_copy_text(&mut self.tentry, &after);
        }
        if self.tentry.layers.len() > text::MAX_TEXT_LAYERS || self.tentry.stories.len() > text::MAX_TEXT_LAYERS {
            // Review F7: only a design file instance's redraw leaves records a save and reopen compacts.
            let why = if in_step { "the document draws that many text layers after the redraw" } else { "a design file instance's copies are compacted when the document is saved and reopened" };
            return Err(format!("design: text_layer_over_cap (the redraw passes the window's {} text records; {why})", text::MAX_TEXT_LAYERS));
        }
        Ok(())
    }

    /// The entries with the copies' records, the writes and the notes; `in_step`: the text entry goes into
    /// the redraw's history step, so the copies' records it no longer needs are compacted ([`Self::compact_text`]).
    pub(super) fn finish(mut self, in_step: bool) -> Result<(VectorEntry, TextEntry, Vec<PlacementWrite>, Vec<String>), String> {
        self.compact_text(in_step)?;
        self.ventry.paths.extend(self.new_paths);
        if !self.new_nodes.is_empty() || !self.replaced_nodes.is_empty() {
            let mut d2 = self.design;
            // A kept copy's node entry is rewritten (stable identity): its old entry leaves first.
            d2.nodes.retain(|n| !self.replaced_nodes.contains(&n.node.node_id));
            d2.nodes.extend(self.new_nodes);
            self.ventry.design = Some(d2);
        }
        Ok((self.ventry, self.tentry, self.writes, self.notes))
    }
}

/// Definition layer `l` as its copy draws it: the drawn `visible` and `opacity` (percent) overrides applied.
fn drawn_copy(l: &StudioLayer, drawn: &Drawn) -> StudioLayer {
    let over = |field: &str| drawn.get(&(l.layer_id.clone(), field.to_owned()));
    let mut copy = l.clone();
    if let Some(StudioValue::Boolean { value }) = over("visible") {
        copy.visible = *value;
    }
    if let Some(StudioValue::Float { value }) = over("opacity") {
        let group_kind = matches!(copy.payload, Payload::Group);
        let mut a = copy.appearance.clone().unwrap_or_else(|| folio::LayerAppearance::identity(group_kind));
        a.opacity = (value * 100.0).clamp(0.0, 100.0);
        copy.appearance = Some(a);
    }
    copy
}

/// A node entry holding only `modes` for layer `id` (F6: explicit variable modes carried to a copy).
fn modes_node(id: &str, modes: BTreeMap<String, String>) -> StudioNodeDesign {
    StudioNodeDesign {
        node: NodeRef { node_kind: NodeKind::Layer, node_id: id.to_owned() },
        auto_layout: None,
        child_layout: None,
        size_clamps: None,
        constraints: None,
        reactions: Vec::new(),
        component_property_references: BTreeMap::new(),
        explicit_variable_modes: modes,
        style_refs: BTreeMap::new(),
    }
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

/// F3: the live filter stacks and paths of the layers a native step deletes leave the vector entry
/// in that step (a path another layer, live shape or vector record still names stays).
fn drop_deleted_copies(ventry: &mut VectorEntry, doc: &StudioDocument, writes: &[PlacementWrite], deleted: &BTreeSet<String>) {
    if deleted.is_empty() {
        return;
    }
    ventry.records.live_filters.retain(|s| !deleted.contains(&s.layer_id));
    let paths_of = |l: &StudioLayer| match &l.payload {
        Payload::Vector { path_ids } => path_ids.clone(),
        _ => Vec::new(),
    };
    let gone: BTreeSet<String> = doc.layers.iter().filter(|l| deleted.contains(&l.layer_id)).flat_map(paths_of).collect();
    let kept: BTreeSet<String> = doc
        .layers
        .iter()
        .filter(|l| !deleted.contains(&l.layer_id))
        .chain(writes.iter().filter_map(|w| w.create.as_ref().map(|(l, _)| l)))
        .flat_map(paths_of)
        .chain(ventry.live.iter().map(|s| s.path_id.clone()))
        .collect();
    // A path any other vector record names (brush strokes, swatch bindings and the like) stays too.
    let named = serde_json::to_string(&ventry.records).unwrap_or_default();
    ventry.paths.retain(|p| !gone.contains(&p.path_id) || kept.contains(&p.path_id) || named.contains(p.path_id.as_str()));
}

impl Viewer {
    /// The design records the inspector reads: the receipt's, merged with the vector entry's authored
    /// records (module docs); `None` when the document has neither.
    pub(super) fn design_receipt(&self) -> Option<Receipt> {
        let fig = receipt_design(self.receipt.as_ref());
        let doc = self.run.snapshot()?.document();
        let Some(mut authored) = self.vector_entry().ok().and_then(|e| e.design).filter(authors_components) else { return fig };
        settle(&mut authored, doc);
        let mut r = fig.unwrap_or_else(|| Receipt {
            records: StudioDesignRecords::new(doc.document_id.clone()),
            bindings: Vec::new(),
            origins: BTreeMap::new(),
            native: BTreeMap::new(),
            contents: BTreeMap::new(),
            authored: None,
            error: None,
            design_file: BTreeSet::new(),
        });
        if let Some(id) = shared_id(&r.records, &authored) {
            r.error = Some(format!("design: design_record_duplicate ({id} is in the design file's records and the document's)"));
            r.authored = Some(authored);
            return Some(r);
        }
        r.records.components.extend(authored.components.iter().cloned());
        r.records.component_sets.extend(authored.component_sets.iter().cloned());
        r.records.component_instances.extend(authored.component_instances.iter().cloned());
        let known: BTreeSet<String> = r.records.nodes.iter().map(|n| n.node.node_id.clone()).collect();
        r.records.nodes.extend(authored.nodes.iter().filter(|n| !n.component_property_references.is_empty() && !known.contains(&n.node.node_id)).cloned());
        r.native = authored.component_instances.iter().map(|i| (i.instance_id.clone(), i.clone())).collect();
        let ventry = self.vector_entry().unwrap_or_else(|_| VectorEntry::empty());
        let tentry = self.text_entry().unwrap_or_else(|_| TextEntry::empty(&doc.document_id));
        for k in authored.components.iter().map(|k| (&k.component_id, &k.layer_id)).chain(authored.component_sets.iter().map(|s| (&s.component_id, &s.layer_id))) {
            r.origins.insert(k.0.clone(), (origin_of_layer(doc, &ventry, &tentry, k.1), true));
        }
        // Bindings: the states of the `design` entry naming an authored instance on a group layer the
        // document holds (read leniently: Folio validates the entry where it is read for commands).
        let states = self
            .run
            .document_entry(persist::ENTRY)
            .filter(|b| b.len() <= MAX_BINDING_BYTES)
            .and_then(|b| serde_json::from_slice::<StudioDesignStateRecords>(b).ok())
            .map(|s| s.instances)
            .unwrap_or_default();
        let mut claimed: BTreeMap<String, String> = BTreeMap::new();
        for s in states.into_iter().take(folio::MAX_DESIGN_STATES) {
            let group = doc.layers.iter().any(|l| l.layer_id == s.layer_id && matches!(l.payload, Payload::Group));
            // A layer id a delete gave up can be minted again: a layer is bound to the first instance with a
            // record whose state names it, and only that instance's contents count for it.
            if !group || !r.native.contains_key(&s.instance_id) || claimed.get(&s.instance_id).is_some_and(|l| l != &s.layer_id) || r.bindings.iter().any(|b| b.layer_id == s.layer_id && (!b.native || b.instance_id != s.instance_id)) {
                continue;
            }
            claimed.insert(s.instance_id.clone(), s.layer_id.clone());
            if !r.bindings.iter().any(|b| b.layer_id == s.layer_id) {
                let origin = origin_of_layer(doc, &ventry, &tentry, &s.layer_id);
                r.bindings.push(Binding { layer_id: s.layer_id.clone(), instance_id: s.instance_id.clone(), origin, pure: true, signature: None, native: true });
            }
            r.contents.entry(s.layer_id).or_default().push(s.content_signature);
        }
        r.authored = Some(authored);
        Some(r)
    }

    /// The instance binding of `layer_id`, its state, its current record and, for an `unknown`
    /// state the `design` entry explains, the reason (`None`: not an instance layer). Order: a
    /// record of this session for the layer's content, the entry's state for that content, the
    /// import record while the content is the imported one. A native instance reads `edited` while
    /// its children are content one of its states recorded, `unknown` otherwise; its record is the
    /// entry's.
    pub(super) fn instance_record(&self, receipt: &Receipt, entry: &persist::EntryRead, layer_id: &str) -> Option<(Binding, &'static str, StudioComponentInstance, Option<String>)> {
        let doc = self.run.snapshot()?.document();
        let b = receipt.bindings.iter().find(|b| b.layer_id == layer_id)?.clone();
        doc.layers.iter().find(|l| l.layer_id == layer_id)?;
        if b.native {
            let record = receipt.native.get(&b.instance_id)?.clone();
            let content = persist::content(doc, layer_id);
            if receipt.contents.get(layer_id).is_some_and(|c| c.contains(&content)) {
                return Some((b, "edited", record, None));
            }
            return Some((b, "unknown", record, Some("design_state_absent: the instance's layers changed outside the instance commands".into())));
        }
        let sig = signature(doc, layer_id);
        if let Some(r) = self.design.get(&b.instance_id, &sig) {
            return Some((b, "edited", r.clone(), None));
        }
        let import = receipt.records.component_instances.iter().find(|i| i.instance_id == b.instance_id)?.clone();
        let content = persist::content(doc, layer_id);
        if let Some(s) = entry.records.as_ref().and_then(|r| r.state(layer_id, &content)).filter(|s| s.instance_id == b.instance_id) {
            return Some((b, "edited", persist::record_of(&import, s), None));
        }
        if b.signature.as_deref().is_none_or(|s| s == sig) {
            return Some((b, "import", import, None));
        }
        let reason = entry.reason(layer_id, &content);
        Some((b, "unknown", import, reason))
    }

    /// The instance inspector's view of `layer_id` (`None`: the layer draws no instance).
    pub fn instance_view(&self, layer_id: &str) -> Option<InstanceView> {
        let receipt = self.design_receipt()?;
        let entry = self.read_design_state(Some(&receipt));
        let (b, state, record, reason) = self.instance_record(&receipt, &entry, layer_id)?;
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
            error: reason.or_else(|| receipt.error.clone()),
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
        if let Some(e) = &receipt.error {
            return Err(e.clone());
        }
        let entry = self.read_design_state(Some(&receipt));
        let (binding, state, record, reason) = self.instance_record(&receipt, &entry, layer_id).ok_or_else(|| format!("design: {layer_id} draws no component instance"))?;
        if state == "unknown" {
            return Err(match reason {
                Some(r) => format!("design: instance_record_unknown ({r})"),
                None => "design: instance_record_unknown (the instance's layers changed outside the inspector; no saved state matches them, import the design file again)".into(),
            });
        }
        // A design entry Folio refuses as a whole is never overwritten: commands wait until it is replaced.
        if let Some(e) = &entry.error {
            return Err(format!("design: design_state_entry_invalid ({} {}); the stored entry is kept unchanged", persist::snake(e.code), e.target));
        }
        let previous_content = self.run.snapshot().map(|s| persist::content(s.document(), layer_id)).unwrap_or_default();
        let previous_swap = entry.records.as_ref().and_then(|r| r.instances.iter().find(|s| s.layer_id == layer_id && s.content_signature == previous_content && s.instance_id == binding.instance_id)).and_then(|s| s.swap_target.clone());
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
                let current = resolve_instance(&base, &design, &BTreeMap::new(), 0, &hsk_studio_components::Limits::default(), self.cancel).map_err(|e| format!("design: {e:?}"))?;
                let member = design.component(&current.component_id).ok_or("design: member missing")?;
                if !definition_layers(&doc, &member.layer_id)?.iter().any(|(l, _)| &l.layer_id == node_id) {
                    return Err(format!("design: {node_id} is not a layer of the instance's definition"));
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
        let member = design.component(&resolved.component_id).ok_or("design: member missing")?;
        if binding.native {
            // An authored instance: record and layers in one step, origins from bounds (module docs).
            let swap = match &command {
                DesignCommand::Swap { component_id } => Some(component_id.clone()),
                _ => previous_swap,
            };
            let job = NativeJob { group: binding.layer_id.clone(), record: next.clone(), origin: binding.origin, swap };
            let (mut notes, _, revision) = self.native_redraw("design_instance", &receipt, &[job], None)?;
            if base != record {
                notes.push("override_without_value_dropped".into());
            }
            return Ok(json!({
                "command": key, "layer_id": binding.layer_id, "instance_id": binding.instance_id,
                "component_id": resolved.component_id, "main_component_id": next.main_component_id,
                "dropped": dropped.iter().map(|o| json!({"node_id": o.node_id, "field": o.field})).collect::<Vec<_>>(),
                "notes": notes, "revision": revision,
            }));
        }
        let (corigin, cpure) = *receipt.origins.get(&member.component_id).ok_or("design: the definition has no recorded origin")?;
        if !(binding.pure && cpure) {
            return Err("design: instance_transform_not_translation (a rotated or scaled instance or definition is not redrawn)".into());
        }
        let d = [binding.origin[0] - corigin[0], binding.origin[1] - corigin[1]];
        let mut notes = self.redraw(&doc, &binding.layer_id, &member.layer_id, &next, d)?;
        if base != record {
            notes.push("override_without_value_dropped".into());
        }
        let now = self.run.snapshot().ok_or("nothing open")?.document();
        let sig = signature(now, &binding.layer_id);
        let content = persist::content(now, &binding.layer_id);
        let revision = now.revision;
        // The instance state rides the session entry `design` (module persist); a refusal undoes the redraw.
        let swap_target = match &command {
            DesignCommand::Swap { component_id } => Some(component_id.clone()),
            _ => previous_swap,
        };
        let variant = member.variant_properties.clone().unwrap_or_default();
        let saved = persist::state_of(&next, &binding.layer_id, content, variant, swap_target).and_then(|(state, complete)| {
            let mut n = self.write_design_state(entry, &receipt, state, true)?;
            if !complete {
                n.push("override_without_value_not_saved".into());
            }
            Ok(n)
        });
        match saved {
            Ok(n) => notes.extend(n),
            Err(e) => {
                // Review F5 (sibling site): a rollback leaves no redo entry.
                let undone = if self.run.rollback_newest_step() { Ok(()) } else { Err(self.op_error()) };
                return Err(rollback_error(&e, undone));
            }
        }
        notes.sort();
        notes.dedup();
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
    fn redraw(&mut self, doc: &StudioDocument, group: &str, root: &str, record: &StudioComponentInstance, d: [f64; 2]) -> Result<Vec<String>, String> {
        let mut r = Redraw::new(self, doc)?;
        // A design file instance's redraw keeps no copy: its states are keyed by the copies' ids (module docs).
        r.instance(group, root, &drawn_overrides(record), d, false)?;
        let (ventry, tentry, writes, mut notes) = r.finish(false)?;
        // Entries first (append-only records; the layers then render at once), then the layers.
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

    /// Redraws authored instances (lane SMB-DS-COMPONENTS-AUTHOR-1) as ONE history step: each job's
    /// group gets a translated copy of the member its record resolves to (its origin minus the
    /// member's, both from layer bounds), with the overrides and the property-driven `visible` and
    /// `characters` fields drawn; `create` (a new group write) goes first. The structure edit and the
    /// vector entry (paths and every job's instance record) are one step (`Run::restructure_and_entry`);
    /// copied stories go to the text entry in the same step, which drops the records of copies the document
    /// no longer draws (`Run::restructure_and_entries`, lead ruling P2); each job's
    /// binding state is written to the `design` entry after it (a refusal undoes the step). The node
    /// entries, live filter stacks and paths of the copies the step deletes leave the vector entry in
    /// the same step (F3), and the record caps are checked before the write. Returns the notes, the
    /// member each job drew and the revision.
    pub(super) fn native_redraw(&mut self, op: &str, receipt: &Receipt, jobs: &[NativeJob], create: Option<PlacementWrite>) -> Result<(Vec<String>, Vec<String>, u64), String> {
        if !vector::entry_persistable() {
            return Err(not_persistable(op));
        }
        let doc = self.run.snapshot().ok_or("nothing open")?.document().clone();
        let mut records = receipt.records.clone();
        for job in jobs {
            records = with_instance(&records, &job.record);
        }
        let local = LocalContent::default();
        let design = Design::new(&doc, &records, &local).map_err(|e| format!("design: design_record_{} ({e:?})", e.code()))?;
        let text_before = self.text_entry()?;
        let mut r = Redraw::new(self, &doc)?;
        let mut writes = Vec::new();
        if let Some(w) = create {
            r.reserve(&w.layer_id);
            writes.push(w);
        }
        let mut members = Vec::with_capacity(jobs.len());
        let limits = hsk_studio_components::Limits::default();
        // Lead ruling 2026-11-13 (option A, stable identity): a redraw of the member the instance drew before
        // (a sync, an override or property edit) keeps its copies' ids; a swap to another member does not.
        let before = Design::new(&doc, &receipt.records, &local).ok();
        for job in jobs {
            let res = resolve_instance(&job.record, &design, &BTreeMap::new(), 0, &limits, self.cancel).map_err(|e| format!("design: {} ({e:?})", e.code()))?;
            let member = design.component(&res.component_id).ok_or("design: member missing")?;
            let keep = before
                .as_ref()
                .zip(receipt.native.get(&job.record.instance_id))
                .and_then(|(b, prev)| resolve_instance(prev, b, &BTreeMap::new(), 0, &limits, self.cancel).ok())
                .is_some_and(|prev| prev.component_id == res.component_id);
            let def = origin_of_layer(&doc, &r.ventry, &text_before, &member.layer_id);
            let d = [job.origin[0] - def[0], job.origin[1] - def[1]];
            let mut drawn = Drawn::new();
            for f in res.fields.iter().filter(|f| DRAWN_OVERRIDES.contains(&f.field.as_str()) && matches!(f.source, Source::Override | Source::ComponentProperty(_) | Source::PropertyDefault(_))) {
                drawn.insert((f.node_id.clone(), f.field.clone()), f.value.clone());
            }
            r.instance(&job.group, &member.layer_id, &drawn, d, keep)?;
            members.push(member.component_id.clone());
        }
        let (mut ventry, tentry, more, mut notes) = r.finish(true)?;
        writes.extend(more);
        // Review F2 (lead option (b)): room for each job's new state, counted net of the states it replaces
        // (same layer, content and instance: a sync keeps its copies' ids, lane SMB-PS-FILTER-ADJUST-COMMANDS-1
        // stable identity), checked before the step.
        let after = super::component::project(&doc, &writes);
        let adds: Vec<persist::StateKey> = jobs.iter().map(|j| (j.group.clone(), j.record.instance_id.clone(), persist::content(&after, &j.group))).collect();
        persist::room_for_net(self.run.document_entry(persist::ENTRY), &adds)?;
        let authored = ventry.design.get_or_insert_with(|| StudioDesignRecords::new(doc.document_id.clone()));
        for job in jobs {
            match authored.component_instances.iter_mut().find(|i| i.instance_id == job.record.instance_id) {
                Some(slot) => *slot = job.record.clone(),
                None => authored.component_instances.push(job.record.clone()),
            }
        }
        settle(authored, &doc);
        // F3: what the step deletes takes its node entries along (they are restored by its undo).
        let deleted: BTreeSet<String> = writes.iter().filter(|w| w.to.is_none()).map(|w| w.layer_id.clone()).collect();
        authored.nodes.retain(|n| !deleted.contains(&n.node.node_id));
        super::component::check_caps(authored)?;
        self.check_authored(&doc, &writes, authored)?;
        drop_deleted_copies(&mut ventry, &doc, &writes, &deleted);
        let vbytes = ventry.to_bytes(&doc.document_id)?;
        let target = jobs.first().map_or_else(String::new, |j| j.group.clone());
        // Lead ruling P2: the text entry (copies added, copies no longer drawn compacted) is in the same
        // history step as the structure edit and the vector entry.
        let ok = if tentry == text_before {
            self.run.restructure_and_entry(op, &writes, vector::ENTRY, vbytes)
        } else {
            let tbytes = TextEntry::to_bytes(&tentry)?;
            self.run.restructure_and_entries(op, &writes, vec![(vector::ENTRY, vbytes), (text::ENTRY, tbytes)])
        };
        let revision = self.after_edit(ok, format!("{op} {target}"))?;
        // Review F5: a state refused after the step puts back the states written so far and rolls the step
        // back with no redo entry.
        let design_before = self.run.document_entry(persist::ENTRY).map(<[u8]>::to_vec);
        for (job, member) in jobs.iter().zip(&members) {
            let saved = (|| {
                let after = self.design_receipt().ok_or("design: records")?;
                let now = self.run.snapshot().ok_or("nothing open")?.document();
                let content = persist::content(now, &job.group);
                let variant = after.records.components.iter().find(|k| &k.component_id == member).and_then(|k| k.variant_properties.clone()).unwrap_or_default();
                let (state, _) = persist::state_of(&job.record, &job.group, content, variant, job.swap.clone())?;
                let entry = self.read_design_state(Some(&after));
                if let Some(e) = &entry.error {
                    return Err(format!("design: design_state_entry_invalid ({} {})", persist::snake(e.code), e.target));
                }
                self.write_design_state(entry, &after, state, true)
            })();
            match saved {
                Ok(n) => notes.extend(n),
                Err(e) => {
                    let restored = self.run.set_document_entry(persist::ENTRY, design_before.clone());
                    let undone = if self.run.rollback_newest_step() { Ok(()) } else { Err(self.op_error()) };
                    let e = match restored {
                        Ok(()) => e,
                        Err(code) => format!("{e} (the design states written before it could not be put back: {code})"),
                    };
                    return Err(rollback_error(&e, undone));
                }
            }
        }
        notes.sort();
        notes.dedup();
        Ok((notes, members, revision))
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
                ui.colored_label(egui::Color32::from_rgb(200, 120, 0), "The instance's layers changed outside this section and no saved state matches them: its record is unknown (import the design file again to edit).");
            }
            if let Some(e) = &view.error {
                ui.colored_label(egui::Color32::RED, e);
            }
            for (prop, (values, cur)) in &view.variants {
                ui.horizontal_wrapped(|ui| {
                    ui.label(prop);
                    for v in values {
                        let id = widget::variant(prop, v);
                        if super::super::wrap::wrap_item(ui, super::super::widget::id(&id), v.as_str(), egui::TextStyle::Button, |ui| ui.selectable_label(v == cur, v.as_str())).on_hover_text(format!("Switch {prop} to {v}")).clicked() && v != cur {
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
                    let label = format!("Reset {field}");
                    if super::super::wrap::wrap_item(ui, super::super::widget::id(&id), &label, egui::TextStyle::Button, |ui| ui.button(label.as_str())).on_hover_text("Reset this override to the component's value").clicked() {
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
                        let label = format!("Swap to {name}");
                        if super::super::wrap::wrap_item(ui, super::super::widget::id(&id), &label, egui::TextStyle::Button, |ui| ui.button(label.as_str())).clicked() {
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
        let receipt = self.design_receipt();
        let bound = receipt.as_ref().map(|r| r.bindings.len());
        let entry = self.read_design_state(receipt.as_ref()).json();
        let components = self.components_json(receipt.as_ref());
        // Lane SMB-DS-VARIABLES-AUTHOR-1: collections, variables, explicit modes and the STU-DS-123 advisory.
        let variables = self.variables_json();
        json!({"instance": instance, "instance_layers": bound, "last": self.design.last, "entry": entry, "auto_layout": self.auto_layout_json(), "components": components, "variables": variables})
    }
}

/// The error of a refused design-state save after the redraw rollback: it says the redraw was undone
/// only when the undo succeeded, and names the undo failure otherwise (the document then still holds
/// the unsaved redraw; F-R40-5).
/// R1: drops the `text` records of redraw copies (stories minted by a redraw) that `doc` does not
/// draw: a record whose layer is gone or names another story, and the stories no remaining record or
/// layer names. Only for a document with no history (opened): nothing can return to them then.
/// Returns whether anything was dropped.
pub(super) fn compact_copy_text(tentry: &mut TextEntry, doc: &StudioDocument) -> bool {
    let copy_story = |sid: &str| sid.starts_with("STXT-") && sid.contains("-7f17-8000-");
    let drawn = |layer: &str, story: &str| doc.layers.iter().any(|l| l.layer_id == layer && matches!(&l.payload, Payload::Text { story_id } if story_id == story));
    let before = (tentry.layers.len(), tentry.stories.len());
    tentry.layers.retain(|r| !copy_story(&r.story_id) || drawn(&r.layer_id, &r.story_id));
    let named: BTreeSet<String> = tentry
        .layers
        .iter()
        .map(|r| r.story_id.clone())
        .chain(doc.layers.iter().filter_map(|l| match &l.payload {
            Payload::Text { story_id } => Some(story_id.clone()),
            _ => None,
        }))
        .collect();
    tentry.stories.retain(|st| !copy_story(&st.story_id) || named.contains(&st.story_id));
    (tentry.layers.len(), tentry.stories.len()) != before
}

/// F9: the refusal of a native redraw in a session without the vector entry token (its code is the
/// reason, not the op).
pub(super) fn not_persistable(op: &str) -> String {
    format!("design: vector_entry_not_persistable ({op})")
}

pub(super) fn rollback_error(save: &str, undone: Result<(), String>) -> String {
    match undone {
        Ok(()) => format!("{save} (the redraw was undone)"),
        Err(undo) => format!("{save} (the redraw could not be undone: undo {undo}; the document keeps the unsaved redraw)"),
    }
}

#[cfg(test)]
mod tests {
    use super::{not_persistable, rollback_error};

    #[test]
    fn a_native_redraw_without_the_vector_entry_token_refuses_with_its_reason_code() {
        let m = not_persistable("component_create_instance");
        assert_eq!(crate::view::commands::leading_code(&m).as_deref(), Some("vector_entry_not_persistable"), "{m}");
    }

    #[test]
    fn rollback_error_names_a_failed_undo_and_never_claims_the_redraw_was_undone() {
        assert_eq!(rollback_error("design: design entry x", Ok(())), "design: design entry x (the redraw was undone)");
        let failed = rollback_error("design: design entry x", Err("nothing_to_undo".into()));
        assert!(!failed.contains("the redraw was undone"), "{failed}");
        assert!(failed.contains("could not be undone") && failed.contains("undo nothing_to_undo"), "{failed}");
    }
}
