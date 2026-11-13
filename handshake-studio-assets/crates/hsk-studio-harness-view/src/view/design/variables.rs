//! Design variables (lane SMB-DS-VARIABLES-AUTHOR-1; [STU-DS-121..133], [STU-DS-024..027], [STU-DS-162],
//! [STU-DS-164], [STU-DS-175], [STU-DS-177]): the spec's typed commands ([`VariableCommand`], ids
//! `variable_collection.*`, `variable.*` and `node.*_explicit_variable_mode_for_collection`) over Folio's
//! `StudioVariableCollection`, `StudioVariable` and `StudioNodeDesign::explicit_variable_modes`, egui-free.
//! Every mutating command is ONE undo step (an entry-only step of the vector entry, whose `design` member
//! is the document's one record set, Folio-checked whole before every commit); a read makes none.
//!
//! Resolution ([STU-DS-132], [STU-DS-164]): `variable.resolve_for_consumer` walks the consumer node and its
//! ancestors (layers, artboards, page spreads; nearest first) and takes, per collection, the first explicit
//! mode it meets; absence falls back to the collection's default mode. The alias walk is the components
//! crate's `resolve_variable` (deterministic: ordered maps, no clock, no locale). A `scope` given names the
//! property's [STU-DS-128] scope: a variable whose `scopes` do not admit it is refused
//! `variable_scope_violation` naming the variable, its scopes and the property ([STU-DS-177];
//! `ALL_SCOPES` admits every scope, `ALL_FILLS` the three fills; an empty list admits none).
//!
//! Studio readings (declared; Figma behaviour UNVERIFIED, D5): mode ids are `mode-<n>` minted past the
//! collection's highest; a new mode takes each variable's default-mode value; a removed default mode moves
//! the default to the first remaining mode; names are slash paths unique within the collection, mode names
//! unique within the collection; a variable another variable aliases is not removed (`variable_aliased`).
//! [STU-DS-123]: no mode ceiling beyond Folio's own `MAX_MODES`; the Studio-declared advisory threshold is
//! [`VARIABLE_VALUE_ADVISORY`] stored values (variables x modes), reported as `advisory` in every mutating
//! result and in `state_json().design.variables` (an advisory, never a refusal) and stated in the manual
//! (harness-core `DRIVE_LIMITS`).
//!
//! Host integration (review F3): each mutating row commits ONE entry-only step through
//! `Run::set_entry_step` under its own catalog id: the op record, the step's attribution and its label
//! (the undo name) are the row id, and its business event is STUDIO_VARIABLE_SET.
//!
//! Out of scope (declared): extended collections ([STU-DS-124]; Folio refuses `is_extension`) and their
//! commands `variable_collection.extend`, `variable.values_by_mode_for_collection` and
//! `variable.remove_override_for_mode`; `EXPRESSION` values ([STU-DS-126], [STU-DS-131]; no Folio home);
//! the binding commands and bindable-field sets ([STU-DS-130]; no Folio home), so `rebound` is always 0;
//! the library-loading asynchronous read forms; the variables of an opened design file's receipt are
//! not available here (`variable_not_authored`: not resolved, not listed in the state; review F7).
#![deny(clippy::arithmetic_side_effects)]
use super::super::Viewer;
use super::super::vector::{self, VectorEntry, style};
use super::component::{mint_in, variant_definitions};
use super::instance::{carries_nothing, settle};
use hsk_studio_accord::StudioBusinessEventId;
use hsk_studio_components::{ComponentsError, Limits, VariableTable, resolve_variable};
use hsk_studio_folio::{
    self as folio, NodeKind, NodeRef, Parent, StudioDesignRecords, StudioDocument, StudioMode, StudioNodeDesign, StudioPublishing, StudioValue, StudioVariable,
    StudioVariableCollection, VariableCollectionSchema, VariableSchema, VariableType,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Longest variable or collection name in bytes (Studio-declared).
pub const MAX_NAME_BYTES: usize = 256;
/// Longest mode name and code-syntax name in bytes (Folio's label rule).
pub const MAX_LABEL_BYTES: usize = 128;
/// Longest variable description in bytes (Folio's text bound).
pub const MAX_DESCRIPTION_BYTES: usize = 65_536;
/// Most scopes one variable lists: the closed set of [STU-DS-128].
pub const MAX_SCOPES: usize = folio::VARIABLE_SCOPES.len();
/// [STU-DS-123] advisory threshold (Studio-declared): above this many stored values (variables x modes)
/// every variable result and the state carry an advisory; nothing is refused for it.
pub const VARIABLE_VALUE_ADVISORY: usize = 16_384;
/// The default mode name of a new collection ([STU-DS-123]).
pub const DEFAULT_MODE_NAME: &str = "Mode 1";

/// A typed variable command ([STU-DS-132], [STU-DS-133]).
#[derive(Clone, Debug, PartialEq)]
pub enum VariableCommand {
    /// `variable_collection.create`: one mode, named `mode_name` or `Mode 1`.
    CollectionCreate { name: String, mode_name: Option<String> },
    /// `variable_collection.add_mode`.
    AddMode { collection: String, name: String },
    /// `variable_collection.rename_mode`.
    RenameMode { collection: String, mode: String, name: String },
    /// `variable_collection.remove_mode`: its values and the explicit modes naming it go too.
    RemoveMode { collection: String, mode: String },
    /// `variable_collection.remove`: its variables and the explicit modes naming it go too.
    CollectionRemove { collection: String },
    /// `variable.create`: `value` is set for every mode of the collection.
    Create { collection: String, name: String, resolved_type: VariableType, value: StudioValue, scopes: Option<Vec<String>>, description: Option<String>, hidden_from_publishing: Option<bool> },
    /// `variable.remove`.
    Remove { variable: String },
    /// `variable.set_value_for_mode`.
    SetValueForMode { variable: String, mode: String, value: StudioValue },
    /// `variable.set_code_syntax` (`platform` from [STU-DS-129], checked by the decoder).
    SetCodeSyntax { variable: String, platform: String, name: String },
    /// `variable.remove_code_syntax`.
    RemoveCodeSyntax { variable: String, platform: String },
    /// `variable.create_alias`: the alias value record of a variable (a read).
    CreateAlias { variable: String },
    /// `variable.resolve_for_consumer` (a read).
    ResolveForConsumer { variable: String, node: String, scope: Option<String> },
    /// `node.set_explicit_variable_mode_for_collection`.
    SetExplicitMode { node: String, collection: String, mode: String },
    /// `node.clear_explicit_variable_mode_for_collection`.
    ClearExplicitMode { node: String, collection: String },
}

impl VariableCommand {
    /// The command id of the row.
    pub fn id(&self) -> &'static str {
        match self {
            Self::CollectionCreate { .. } => "variable_collection.create",
            Self::AddMode { .. } => "variable_collection.add_mode",
            Self::RenameMode { .. } => "variable_collection.rename_mode",
            Self::RemoveMode { .. } => "variable_collection.remove_mode",
            Self::CollectionRemove { .. } => "variable_collection.remove",
            Self::Create { .. } => "variable.create",
            Self::Remove { .. } => "variable.remove",
            Self::SetValueForMode { .. } => "variable.set_value_for_mode",
            Self::SetCodeSyntax { .. } => "variable.set_code_syntax",
            Self::RemoveCodeSyntax { .. } => "variable.remove_code_syntax",
            Self::CreateAlias { .. } => "variable.create_alias",
            Self::ResolveForConsumer { .. } => "variable.resolve_for_consumer",
            Self::SetExplicitMode { .. } => "node.set_explicit_variable_mode_for_collection",
            Self::ClearExplicitMode { .. } => "node.clear_explicit_variable_mode_for_collection",
        }
    }
}

fn refused(code: &str, why: impl std::fmt::Display) -> String {
    format!("variable: {code} ({why})")
}

/// Folio's slash-path rule with a length: no control character, no empty segment.
fn name_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_NAME_BYTES && !s.chars().any(char::is_control) && s.split('/').all(|seg| !seg.trim().is_empty())
}

/// Folio's label rule (a mode name, a code-syntax name).
fn label_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_LABEL_BYTES && !s.chars().any(char::is_control)
}

/// True when `scopes` admit a property of scope `scope` ([STU-DS-128] module docs).
pub fn admits(scopes: &[String], scope: &str) -> bool {
    scopes.iter().any(|s| s == "ALL_SCOPES" || s == scope || (s == "ALL_FILLS" && matches!(scope, "FRAME_FILL" | "SHAPE_FILL" | "TEXT_FILL")))
}

/// Stored values of a record set (variables x modes, [STU-DS-123]).
pub fn stored_values(d: &StudioDesignRecords) -> usize {
    d.variables.iter().fold(0_usize, |n, v| n.saturating_add(v.values_by_mode.len()))
}

/// The [STU-DS-123] advisory for `values` stored values (null under the threshold).
pub fn advisory(values: usize) -> Value {
    if values > VARIABLE_VALUE_ADVISORY {
        json!({"code": "variables_times_modes_over_threshold", "values": values, "threshold": VARIABLE_VALUE_ADVISORY})
    } else {
        Value::Null
    }
}

/// The entry caps of the variable records (the vector entry's decode enforces the same, `vector::style`),
/// checked before every write so a write never leaves an entry that does not decode.
pub(super) fn check_caps(d: &StudioDesignRecords) -> Result<(), String> {
    if d.variable_collections.len() > style::MAX_ENTRY_COLLECTIONS {
        return Err(refused("variable_collection_over_cap", format!("at most {} collections", style::MAX_ENTRY_COLLECTIONS)));
    }
    if d.variables.len() > style::MAX_ENTRY_VARIABLES {
        return Err(refused("variable_over_cap", format!("at most {} variables", style::MAX_ENTRY_VARIABLES)));
    }
    if stored_values(d) > style::MAX_ENTRY_VARIABLE_VALUES {
        return Err(refused("variable_value_over_cap", format!("at most {} stored values (variables x modes)", style::MAX_ENTRY_VARIABLE_VALUES)));
    }
    Ok(())
}

/// The alias edges of every variable over all its modes (Folio's conservative union reading).
fn alias_edges(d: &StudioDesignRecords) -> BTreeMap<&str, BTreeSet<&str>> {
    let mut edges: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for v in &d.variables {
        let out = edges.entry(v.variable_id.as_str()).or_default();
        out.extend(v.values_by_mode.values().filter_map(StudioValue::alias_target));
    }
    edges
}

/// Most variables one cycle check visits (the entry's variable cap): past it the check is refused typed
/// `variable_alias_check_over_budget` (review F7).
pub const MAX_ALIAS_CHECK_VISITS: usize = style::MAX_ENTRY_VARIABLES;

/// [STU-DS-175]: refused `variable_alias_cycle` naming the cycle in resolution order when `variable`'s
/// values in `d` alias back to it. The alias edges are built once; ONE breadth-first walk from all the
/// variable's alias targets (sorted, so the answer is deterministic) visits each variable at most once,
/// with a cancel point per visit and a typed refusal past [`MAX_ALIAS_CHECK_VISITS`] (review F7).
fn check_cycle(d: &StudioDesignRecords, variable: &str, cancel: &hsk_studio_accord::CancellationToken) -> Result<(), String> {
    check_cycle_within(d, variable, cancel, MAX_ALIAS_CHECK_VISITS)
}

fn check_cycle_within(d: &StudioDesignRecords, variable: &str, cancel: &hsk_studio_accord::CancellationToken, bound: usize) -> Result<(), String> {
    let edges = alias_edges(d);
    let Some(starts) = edges.get(variable) else { return Ok(()) };
    let mut came: BTreeMap<&str, &str> = BTreeMap::new();
    let mut seen: BTreeSet<&str> = starts.iter().copied().collect();
    let mut queue: std::collections::VecDeque<&str> = starts.iter().copied().collect();
    let mut visits = 0_usize;
    while let Some(at) = queue.pop_front() {
        cancel.check().map_err(|_| refused("canceled", "the alias cycle check was canceled"))?;
        visits = visits.saturating_add(1);
        if visits > bound {
            return Err(refused("variable_alias_check_over_budget", format!("the alias walk passes {bound} variables")));
        }
        if at == variable {
            let mut cycle = vec![variable.to_owned()];
            let mut cur = at;
            let mut back = Vec::new();
            while let Some(prev) = came.get(cur) {
                back.push((*prev).to_owned());
                cur = prev;
            }
            back.reverse();
            cycle.extend(back);
            cycle.push(variable.to_owned());
            return Err(refused("variable_alias_cycle", cycle.join(" -> ")));
        }
        for next in edges.get(at).into_iter().flatten() {
            if seen.insert(next) {
                came.insert(next, at);
                queue.push_back(next);
            }
        }
    }
    Ok(())
}

/// A document node by id: its reference and whether it is a container ([STU-DS-132]: a group layer, an
/// artboard or a page spread).
fn node_of(doc: &StudioDocument, id: &str) -> Option<(NodeRef, bool, Parent)> {
    if let Some(l) = doc.layers.iter().find(|l| l.layer_id == id) {
        let container = l.kind == "group" && matches!(l.payload, folio::Payload::Group);
        return Some((NodeRef { node_kind: NodeKind::Layer, node_id: id.to_owned() }, container, l.parent.clone()));
    }
    if let Some(a) = doc.artboards.iter().find(|a| a.artboard_id == id) {
        return Some((NodeRef { node_kind: NodeKind::Artboard, node_id: id.to_owned() }, true, a.parent.clone()));
    }
    let s = doc.page_spreads.iter().find(|s| s.page_spread_id == id)?;
    Some((NodeRef { node_kind: NodeKind::PageSpread, node_id: id.to_owned() }, true, s.parent.clone()))
}

/// `id` and its ancestors, nearest first (bounded by the node count, so a cyclic document cannot loop).
fn lineage(doc: &StudioDocument, id: &str) -> Vec<String> {
    let bound = doc.layers.len().saturating_add(doc.artboards.len()).saturating_add(doc.page_spreads.len());
    let mut out = vec![id.to_owned()];
    let mut at = node_of(doc, id).map(|(_, _, p)| p);
    while let Some(Parent::Node(n)) = at {
        if out.len() > bound {
            break;
        }
        at = node_of(doc, &n.node_id).map(|(_, _, p)| p);
        out.push(n.node_id);
    }
    out
}

fn mode_id(k: &StudioVariableCollection) -> String {
    let next = k.modes.iter().filter_map(|m| m.mode_id.strip_prefix("mode-")).filter_map(|n| n.parse::<u64>().ok()).max().map_or(1, |n| n.saturating_add(1));
    let mut n = next;
    loop {
        let id = format!("mode-{n}");
        if !k.modes.iter().any(|m| m.mode_id == id) {
            return id;
        }
        n = n.wrapping_add(1);
    }
}

fn publishing(id: &str, description: String) -> StudioPublishing {
    StudioPublishing { description, description_markdown: String::new(), documentation_links: Vec::new(), publish_key: format!("local/{id}"), is_remote: false }
}

/// The authoring context: the document, the vector entry, its records (settled as every authoring commit
/// is) and the variable ids of an opened design file's receipt (not available here).
struct Vars {
    doc: StudioDocument,
    ventry: VectorEntry,
    d: StudioDesignRecords,
    fig: BTreeSet<String>,
}

impl Vars {
    fn collection(&self, id: &str) -> Result<usize, String> {
        self.d.variable_collections.iter().position(|k| k.collection_id == id).ok_or_else(|| {
            if self.fig.contains(id) { refused("variable_collection_not_authored", format!("{id} comes from the design file's records")) } else { refused("variable_collection_unknown", id) }
        })
    }

    fn variable(&self, id: &str) -> Result<usize, String> {
        self.d.variables.iter().position(|v| v.variable_id == id).ok_or_else(|| {
            if self.fig.contains(id) { refused("variable_not_authored", format!("{id} comes from the design file's records")) } else { refused("variable_unknown", id) }
        })
    }

    /// The collection of a variable.
    fn owner(&self, vi: usize) -> Result<usize, String> {
        let cid = self.d.variables[vi].variable_collection_id.clone();
        self.collection(&cid)
    }

    fn mode(&self, ci: usize, mode: &str) -> Result<(), String> {
        let k = &self.d.variable_collections[ci];
        if k.modes.iter().any(|m| m.mode_id == mode) { Ok(()) } else { Err(refused("variable_mode_unknown", format!("{mode} is not a mode of {}", k.collection_id))) }
    }

    /// Every id a new record must not take: authored and design file ones.
    fn taken(&self) -> BTreeSet<String> {
        let d = &self.d;
        d.variable_collections.iter().map(|k| k.collection_id.clone()).chain(d.variables.iter().map(|v| v.variable_id.clone())).chain(self.fig.iter().cloned()).collect()
    }

    /// The value's type is the variable's and an alias names an authored variable of that type.
    fn value_fits(&self, t: VariableType, value: &StudioValue) -> Result<(), String> {
        if value.resolved_type() != t {
            return Err(refused("variable_value_type", format!("a {t:?} variable takes a {t:?} value or an alias resolving to {t:?}")));
        }
        if let Some(target) = value.alias_target() {
            let ti = self.variable(target)?;
            if self.d.variables[ti].resolved_type != t {
                return Err(refused("variable_value_type", format!("{target} resolves to {:?}", self.d.variables[ti].resolved_type)));
            }
        }
        Ok(())
    }

    /// Variables outside `except` whose values alias one of `ids`.
    fn aliasing(&self, ids: &BTreeSet<&str>, except: &BTreeSet<&str>) -> Vec<String> {
        self.d.variables.iter().filter(|v| !except.contains(v.variable_id.as_str()) && v.values_by_mode.values().filter_map(StudioValue::alias_target).any(|t| ids.contains(t))).map(|v| v.variable_id.clone()).collect()
    }

    /// Drops the explicit modes `drop` selects (collection, mode); returns how many went.
    fn clear_explicit(&mut self, drop: impl Fn(&str, &str) -> bool) -> usize {
        let mut cleared = 0_usize;
        for n in &mut self.d.nodes {
            let before = n.explicit_variable_modes.len();
            n.explicit_variable_modes.retain(|c, m| !drop(c, m));
            cleared = cleared.saturating_add(before.saturating_sub(n.explicit_variable_modes.len()));
        }
        self.d.nodes.retain(|n| !carries_nothing(n));
        cleared
    }
}

impl Viewer {
    /// Runs one variable command (module docs). Returns the command's result.
    pub fn variable_command(&mut self, command: VariableCommand) -> Result<Value, String> {
        let id = command.id();
        let r = self.variable_inner(command);
        match &r {
            Ok(_) => self.say(format!("{id} done")),
            Err(e) => self.say(format!("{id} refused: {e}")),
        }
        r
    }

    fn vars(&self) -> Result<Vars, String> {
        let doc = self.run.snapshot().ok_or_else(|| refused("nothing_open", "no document"))?.document().clone();
        let ventry = self.vector_entry()?;
        let mut d = ventry.design.clone().unwrap_or_else(|| StudioDesignRecords::new(doc.document_id.clone()));
        settle(&mut d, &doc);
        variant_definitions(&mut d, &doc);
        let fig = self.design_receipt().map(|r| r.records.variable_collections.iter().map(|k| k.collection_id.clone()).chain(r.records.variables.iter().map(|v| v.variable_id.clone())).collect()).unwrap_or_default();
        Ok(Vars { doc, ventry, d, fig })
    }

    /// Stores the records as one entry-only step of the row `command` (caps and Folio's whole-set check
    /// first): review F3, its record, attribution and undo name are the catalog id and its business event
    /// is STUDIO_VARIABLE_SET. Returns the revision and the [STU-DS-123] advisory of the stored records.
    fn commit_variables(&mut self, command: &'static str, target: &str, v: Vars) -> Result<(u64, Value), String> {
        if !vector::entry_persistable() {
            return Err(refused("vector_entry_not_persistable", "the session holds no vector entry token"));
        }
        let Vars { doc, mut ventry, d, .. } = v;
        super::component::check_caps(&d)?;
        self.check_authored(&doc, &[], &d)?;
        let adv = advisory(stored_values(&d));
        ventry.design = Some(d);
        let bytes = ventry.to_bytes(&doc.document_id)?;
        let ok = self.run.set_entry_step(command, Some(StudioBusinessEventId::VariableSet), vector::ENTRY, bytes);
        self.after_edit(ok, format!("{command} {target}")).map(|revision| (revision, adv))
    }

    #[allow(clippy::too_many_lines)]
    fn variable_inner(&mut self, command: VariableCommand) -> Result<Value, String> {
        let cmd = command.id();
        match command {
            VariableCommand::CollectionCreate { name, mode_name } => {
                let mut v = self.vars()?;
                if !name_ok(&name) {
                    return Err(refused("variable_collection_name_invalid", "a slash path of 1..=256 bytes, no empty segment, no control character"));
                }
                let mode_name = mode_name.unwrap_or_else(|| DEFAULT_MODE_NAME.to_owned());
                if !label_ok(&mode_name) {
                    return Err(refused("mode_name_invalid", "1..=128 bytes, no control character"));
                }
                let id = mint_in(&v.doc.document_id, "SVCL", &v.taken());
                let mode = "mode-1".to_owned();
                v.d.variable_collections.push(StudioVariableCollection {
                    schema_id: VariableCollectionSchema::V1,
                    collection_id: id.clone(),
                    name,
                    hidden_from_publishing: false,
                    publishing: publishing(&id, String::new()),
                    is_extension: false,
                    modes: vec![StudioMode { mode_id: mode.clone(), name: mode_name }],
                    default_mode_id: mode.clone(),
                    variable_ids: Vec::new(),
                });
                let (revision, adv) = self.commit_variables(cmd, &id, v)?;
                Ok(json!({"collection_id": id, "mode_id": mode, "revision": revision, "advisory": adv}))
            }
            VariableCommand::AddMode { collection, name } => {
                let mut v = self.vars()?;
                let ci = v.collection(&collection)?;
                let k = &v.d.variable_collections[ci];
                if !label_ok(&name) {
                    return Err(refused("mode_name_invalid", "1..=128 bytes, no control character"));
                }
                if k.modes.iter().any(|m| m.name == name) {
                    return Err(refused("mode_name_taken", &name));
                }
                if k.modes.len() >= folio::MAX_MODES {
                    return Err(refused("mode_over_cap", format!("at most {} modes (Folio's ceiling)", folio::MAX_MODES)));
                }
                let mode = mode_id(k);
                let default = k.default_mode_id.clone();
                v.d.variable_collections[ci].modes.push(StudioMode { mode_id: mode.clone(), name });
                for var in v.d.variables.iter_mut().filter(|x| x.variable_collection_id == collection) {
                    let copy = var.values_by_mode.get(&default).or_else(|| var.values_by_mode.values().next()).cloned();
                    if let Some(value) = copy {
                        var.values_by_mode.insert(mode.clone(), value);
                    }
                }
                let (revision, adv) = self.commit_variables(cmd, &collection, v)?;
                Ok(json!({"collection_id": collection, "mode_id": mode, "revision": revision, "advisory": adv}))
            }
            VariableCommand::RenameMode { collection, mode, name } => {
                let mut v = self.vars()?;
                let ci = v.collection(&collection)?;
                v.mode(ci, &mode)?;
                if !label_ok(&name) {
                    return Err(refused("mode_name_invalid", "1..=128 bytes, no control character"));
                }
                let k = &mut v.d.variable_collections[ci];
                if k.modes.iter().any(|m| m.name == name && m.mode_id != mode) {
                    return Err(refused("mode_name_taken", &name));
                }
                if let Some(m) = k.modes.iter_mut().find(|m| m.mode_id == mode) {
                    m.name.clone_from(&name);
                }
                let (revision, adv) = self.commit_variables(cmd, &collection, v)?;
                Ok(json!({"collection_id": collection, "mode_id": mode, "name": name, "revision": revision, "advisory": adv}))
            }
            VariableCommand::RemoveMode { collection, mode } => {
                let mut v = self.vars()?;
                let ci = v.collection(&collection)?;
                v.mode(ci, &mode)?;
                let k = &mut v.d.variable_collections[ci];
                if k.modes.len() <= 1 {
                    return Err(refused("mode_remove_last", "a collection keeps at least one mode (STU-DS-123 hard_min 1)"));
                }
                k.modes.retain(|m| m.mode_id != mode);
                if k.default_mode_id == mode {
                    k.default_mode_id = k.modes.first().map(|m| m.mode_id.clone()).unwrap_or_default();
                }
                let default = k.default_mode_id.clone();
                for var in v.d.variables.iter_mut().filter(|x| x.variable_collection_id == collection) {
                    var.values_by_mode.remove(&mode);
                }
                let cleared = v.clear_explicit(|c, m| c == collection && m == mode);
                let (revision, adv) = self.commit_variables(cmd, &collection, v)?;
                Ok(json!({"collection_id": collection, "mode_id": mode, "default_mode_id": default, "explicit_modes_cleared": cleared, "revision": revision, "advisory": adv}))
            }
            VariableCommand::CollectionRemove { collection } => {
                let mut v = self.vars()?;
                let ci = v.collection(&collection)?;
                let inside: BTreeSet<String> = v.d.variables.iter().filter(|x| x.variable_collection_id == collection).map(|x| x.variable_id.clone()).collect();
                let ids: BTreeSet<&str> = inside.iter().map(String::as_str).collect();
                let outside = v.aliasing(&ids, &ids);
                if !outside.is_empty() {
                    return Err(refused("variable_aliased", format!("{} alias variables of {collection}", outside.join(", "))));
                }
                v.d.variable_collections.remove(ci);
                v.d.variables.retain(|x| x.variable_collection_id != collection);
                let cleared = v.clear_explicit(|c, _| c == collection);
                let (revision, adv) = self.commit_variables(cmd, &collection, v)?;
                Ok(json!({"collection_id": collection, "variables_removed": inside.len(), "explicit_modes_cleared": cleared, "revision": revision, "advisory": adv}))
            }
            VariableCommand::Create { collection, name, resolved_type, value, scopes, description, hidden_from_publishing } => {
                let mut v = self.vars()?;
                let ci = v.collection(&collection)?;
                if !name_ok(&name) {
                    return Err(refused("variable_name_invalid", "a slash path of 1..=256 bytes, no empty segment, no control character"));
                }
                if v.d.variables.iter().any(|x| x.variable_collection_id == collection && x.name == name) {
                    return Err(refused("variable_name_taken", format!("{name} is in {collection}")));
                }
                let scopes = scopes.unwrap_or_else(|| vec!["ALL_SCOPES".to_owned()]);
                let distinct: BTreeSet<&str> = scopes.iter().map(String::as_str).collect();
                if scopes.len() > MAX_SCOPES || distinct.len() != scopes.len() || !scopes.iter().all(|s| folio::VARIABLE_SCOPES.contains(&s.as_str())) {
                    return Err(refused("variable_scope_unknown", "distinct tokens of the 22 STU-DS-128 scopes"));
                }
                let description = description.unwrap_or_default();
                if description.len() > MAX_DESCRIPTION_BYTES {
                    return Err(refused("variable_description_too_long", format!("at most {MAX_DESCRIPTION_BYTES} bytes")));
                }
                v.value_fits(resolved_type, &value)?;
                let id = mint_in(&v.doc.document_id, "SVAR", &v.taken());
                let values_by_mode = v.d.variable_collections[ci].modes.iter().map(|m| (m.mode_id.clone(), value.clone())).collect();
                v.d.variables.push(StudioVariable {
                    schema_id: VariableSchema::V1,
                    variable_id: id.clone(),
                    name,
                    hidden_from_publishing: hidden_from_publishing.unwrap_or(false),
                    publishing: publishing(&id, description),
                    variable_collection_id: collection.clone(),
                    resolved_type,
                    values_by_mode,
                    scopes,
                    code_syntax: BTreeMap::new(),
                });
                v.d.variable_collections[ci].variable_ids.push(id.clone());
                check_cycle(&v.d, &id, self.cancel)?;
                let (revision, adv) = self.commit_variables(cmd, &id, v)?;
                Ok(json!({"variable_id": id, "collection_id": collection, "revision": revision, "advisory": adv}))
            }
            VariableCommand::Remove { variable } => {
                let mut v = self.vars()?;
                let vi = v.variable(&variable)?;
                let ids = BTreeSet::from([variable.as_str()]);
                let by = v.aliasing(&ids, &ids);
                if !by.is_empty() {
                    return Err(refused("variable_aliased", format!("{} alias {variable}", by.join(", "))));
                }
                let collection = v.d.variables.remove(vi).variable_collection_id;
                for k in v.d.variable_collections.iter_mut().filter(|k| k.collection_id == collection) {
                    k.variable_ids.retain(|x| *x != variable);
                }
                let (revision, adv) = self.commit_variables(cmd, &variable, v)?;
                Ok(json!({"variable_id": variable, "collection_id": collection, "revision": revision, "advisory": adv}))
            }
            VariableCommand::SetValueForMode { variable, mode, value } => {
                let mut v = self.vars()?;
                let vi = v.variable(&variable)?;
                let ci = v.owner(vi)?;
                v.mode(ci, &mode)?;
                v.value_fits(v.d.variables[vi].resolved_type, &value)?;
                v.d.variables[vi].values_by_mode.insert(mode.clone(), value);
                check_cycle(&v.d, &variable, self.cancel)?;
                let (revision, adv) = self.commit_variables(cmd, &variable, v)?;
                Ok(json!({"variable_id": variable, "mode_id": mode, "revision": revision, "advisory": adv}))
            }
            VariableCommand::SetCodeSyntax { variable, platform, name } => {
                let mut v = self.vars()?;
                let vi = v.variable(&variable)?;
                if !label_ok(&name) {
                    return Err(refused("code_syntax_name_invalid", "1..=128 bytes, no control character"));
                }
                v.d.variables[vi].code_syntax.insert(platform.clone(), name.clone());
                let (revision, adv) = self.commit_variables(cmd, &variable, v)?;
                Ok(json!({"variable_id": variable, "platform": platform, "name": name, "revision": revision, "advisory": adv}))
            }
            VariableCommand::RemoveCodeSyntax { variable, platform } => {
                let mut v = self.vars()?;
                let vi = v.variable(&variable)?;
                if v.d.variables[vi].code_syntax.remove(&platform).is_none() {
                    return Err(refused("code_syntax_absent", format!("{variable} has no {platform} name")));
                }
                let (revision, adv) = self.commit_variables(cmd, &variable, v)?;
                Ok(json!({"variable_id": variable, "platform": platform, "revision": revision, "advisory": adv}))
            }
            VariableCommand::CreateAlias { variable } => {
                let v = self.vars()?;
                let vi = v.variable(&variable)?;
                let alias = StudioValue::VariableAlias { id: variable, resolved_type: v.d.variables[vi].resolved_type };
                serde_json::to_value(&alias).map_err(|e| refused("variable_alias_encoding", e))
            }
            VariableCommand::ResolveForConsumer { variable, node, scope } => self.resolve_for_consumer(&variable, &node, scope.as_deref()),
            VariableCommand::SetExplicitMode { node, collection, mode } => {
                let mut v = self.vars()?;
                let (nref, container, _) = node_of(&v.doc, &node).ok_or_else(|| refused("node_unknown", &node))?;
                if !container {
                    return Err(refused("node_not_container", format!("{node} is not a group layer, an artboard or a page spread")));
                }
                let ci = v.collection(&collection)?;
                v.mode(ci, &mode)?;
                if v.d.node_design(&node).and_then(|n| n.explicit_variable_modes.get(&collection)) == Some(&mode) {
                    return Ok(json!({"node": node, "collection_id": collection, "mode_id": mode, "rebound": 0, "changed": false, "advisory": advisory(stored_values(&v.d))}));
                }
                match v.d.nodes.iter_mut().find(|n| n.node.node_id == node) {
                    Some(n) => {
                        n.explicit_variable_modes.insert(collection.clone(), mode.clone());
                    }
                    None => v.d.nodes.push(StudioNodeDesign {
                        node: nref,
                        auto_layout: None,
                        child_layout: None,
                        size_clamps: None,
                        constraints: None,
                        reactions: Vec::new(),
                        component_property_references: BTreeMap::new(),
                        explicit_variable_modes: BTreeMap::from([(collection.clone(), mode.clone())]),
                        style_refs: BTreeMap::new(),
                    }),
                }
                let (revision, adv) = self.commit_variables(cmd, &node, v)?;
                // No property is bound to a variable yet (bindings have no Folio home): nothing to re-resolve.
                Ok(json!({"node": node, "collection_id": collection, "mode_id": mode, "rebound": 0, "changed": true, "revision": revision, "advisory": adv}))
            }
            VariableCommand::ClearExplicitMode { node, collection } => {
                let mut v = self.vars()?;
                node_of(&v.doc, &node).ok_or_else(|| refused("node_unknown", &node))?;
                v.collection(&collection)?;
                let had = v.d.node_design(&node).is_some_and(|n| n.explicit_variable_modes.contains_key(&collection));
                if !had {
                    return Err(refused("explicit_mode_absent", format!("{node} sets no mode of {collection}")));
                }
                if let Some(n) = v.d.nodes.iter_mut().find(|n| n.node.node_id == node) {
                    n.explicit_variable_modes.remove(&collection);
                }
                v.d.nodes.retain(|n| !carries_nothing(n));
                let (revision, adv) = self.commit_variables(cmd, &node, v)?;
                Ok(json!({"node": node, "collection_id": collection, "rebound": 0, "revision": revision, "advisory": adv}))
            }
        }
    }

    /// `variable.resolve_for_consumer` (module docs).
    fn resolve_for_consumer(&self, variable: &str, node: &str, scope: Option<&str>) -> Result<Value, String> {
        let v = self.vars()?;
        let vi = v.variable(variable)?;
        if node_of(&v.doc, node).is_none() {
            return Err(refused("node_unknown", node));
        }
        let var = &v.d.variables[vi];
        if let Some(scope) = scope
            && !admits(&var.scopes, scope)
        {
            return Err(refused("variable_scope_violation", format!("{variable} scopes [{}] do not admit the property scope {scope}", var.scopes.join(", "))));
        }
        let mut modes: BTreeMap<String, String> = BTreeMap::new();
        let mut sources: BTreeMap<String, Value> = BTreeMap::new();
        for (i, at) in lineage(&v.doc, node).iter().enumerate() {
            for (c, m) in v.d.node_design(at).map(|n| &n.explicit_variable_modes).into_iter().flatten() {
                if !modes.contains_key(c) {
                    modes.insert(c.clone(), m.clone());
                    sources.insert(c.clone(), json!({"mode_id": m, "source": if i == 0 { "explicit" } else { "inherited" }, "node": at}));
                }
            }
        }
        for k in &v.d.variable_collections {
            sources.entry(k.collection_id.clone()).or_insert_with(|| json!({"mode_id": k.default_mode_id, "source": "default", "node": null}));
        }
        let table = VariableTable::new(&v.d.variable_collections, &v.d.variables).map_err(|e| resolution_refusal(&e))?;
        let r = resolve_variable(&table, variable, &modes, &Limits::default(), self.cancel).map_err(|e| resolution_refusal(&e))?;
        Ok(json!({
            "variable_id": variable, "node": node, "value": r.value, "resolved_type": var.resolved_type,
            "collection_id": r.collection_id, "mode_id": r.mode_id, "chain": r.chain, "modes": sources,
        }))
    }

    /// `state_json()["design"]["variables"]`: the document's collections, variables and explicit modes,
    /// the stored value count and the [STU-DS-123] advisory.
    pub(super) fn variables_json(&self) -> Value {
        let d = match self.vector_entry() {
            Ok(e) => e.design.unwrap_or_else(|| StudioDesignRecords::new(String::new())),
            Err(e) => return json!({"collections": [], "variables": [], "explicit_modes": [], "values": 0, "advisory": null, "error": e}),
        };
        let collections: Vec<Value> = d
            .variable_collections
            .iter()
            .map(|k| json!({"collection_id": k.collection_id, "name": k.name, "modes": k.modes, "default_mode_id": k.default_mode_id, "variable_ids": k.variable_ids, "hidden_from_publishing": k.hidden_from_publishing}))
            .collect();
        let variables: Vec<Value> = d
            .variables
            .iter()
            .map(|x| {
                json!({
                    "variable_id": x.variable_id, "name": x.name, "collection_id": x.variable_collection_id, "resolved_type": x.resolved_type,
                    "values_by_mode": x.values_by_mode, "scopes": x.scopes, "code_syntax": x.code_syntax, "description": x.publishing.description,
                    "hidden_from_publishing": x.hidden_from_publishing,
                })
            })
            .collect();
        let explicit: Vec<Value> = d.nodes.iter().flat_map(|n| n.explicit_variable_modes.iter().map(move |(c, m)| json!({"node": n.node.node_id, "collection_id": c, "mode_id": m}))).collect();
        let values = stored_values(&d);
        json!({"collections": collections, "variables": variables, "explicit_modes": explicit, "values": values, "advisory": advisory(values), "error": null})
    }
}

/// A components-crate resolution refusal as a typed code (an alias cycle is [STU-DS-175]'s).
fn resolution_refusal(e: &ComponentsError) -> String {
    match e {
        ComponentsError::AliasCycle(id) => refused("variable_alias_cycle", id),
        other => refused(&format!("variable_resolution_{}", other.code()), format!("{other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{VARIABLE_VALUE_ADVISORY, admits, advisory, check_caps, check_cycle_within, label_ok, name_ok, publishing};
    use super::style;
    use hsk_studio_accord::CancellationToken;
    use hsk_studio_folio::{StudioDesignRecords, StudioMode, StudioValue, StudioVariable, StudioVariableCollection, VariableCollectionSchema, VariableSchema, VariableType};
    use std::collections::BTreeMap;

    fn vid(i: usize) -> String {
        format!("SVAR-019abcde-0000-7000-8000-{i:012x}")
    }

    fn variable(i: usize, value: StudioValue) -> StudioVariable {
        StudioVariable {
            schema_id: VariableSchema::V1,
            variable_id: vid(i),
            name: format!("v/{i}"),
            hidden_from_publishing: false,
            publishing: publishing(&vid(i), String::new()),
            variable_collection_id: "SVCL-019abcde-0000-7000-8000-000000000001".into(),
            resolved_type: VariableType::Float,
            values_by_mode: BTreeMap::from([("mode-1".to_owned(), value)]),
            scopes: Vec::new(),
            code_syntax: BTreeMap::new(),
        }
    }

    /// `n` variables, each aliasing the next; the last aliases the first when `cycle`.
    fn chain(n: usize, cycle: bool) -> StudioDesignRecords {
        let mut d = StudioDesignRecords::new("SDOC-019abcde-0000-7000-8000-000000000001".to_owned());
        for i in 0..n {
            let next = i.saturating_add(1);
            let value = if next < n || cycle { StudioValue::VariableAlias { id: vid(if next < n { next } else { 0 }), resolved_type: VariableType::Float } } else { StudioValue::Float { value: 1.0 } };
            d.variables.push(variable(i, value));
        }
        d
    }

    /// Review F7, STU-DS-175: the cycle is named in resolution order; the walk is bounded and cancelable.
    #[test]
    fn the_alias_cycle_check_names_the_cycle_and_is_bounded_and_cancelable() {
        let token = CancellationToken::default();
        let e = check_cycle_within(&chain(5, true), &vid(0), &token, 100).unwrap_err();
        let expected = [0, 1, 2, 3, 4, 0].map(vid).join(" -> ");
        assert!(e.starts_with("variable: variable_alias_cycle") && e.contains(&expected), "{e}");
        assert!(check_cycle_within(&chain(5, false), &vid(0), &token, 100).is_ok());
        let e = check_cycle_within(&chain(50, false), &vid(0), &token, 10).unwrap_err();
        assert!(e.starts_with("variable: variable_alias_check_over_budget"), "{e}");
        let canceled = CancellationToken::default();
        canceled.cancel();
        let e = check_cycle_within(&chain(5, true), &vid(0), &canceled, 100).unwrap_err();
        assert!(e.starts_with("variable: canceled"), "{e}");
    }

    /// Review F8: the entry caps of the variable records refuse typed before a write.
    #[test]
    fn the_variable_caps_refuse_typed() {
        let collection = |i: usize| StudioVariableCollection {
            schema_id: VariableCollectionSchema::V1,
            collection_id: format!("SVCL-019abcde-0000-7000-8000-{i:012x}"),
            name: format!("c{i}"),
            hidden_from_publishing: false,
            publishing: publishing("x", String::new()),
            is_extension: false,
            modes: vec![StudioMode { mode_id: "mode-1".into(), name: "Mode 1".into() }],
            default_mode_id: "mode-1".into(),
            variable_ids: Vec::new(),
        };
        let mut d = StudioDesignRecords::new("SDOC-019abcde-0000-7000-8000-000000000001".to_owned());
        d.variable_collections = (0..style::MAX_ENTRY_COLLECTIONS).map(collection).collect();
        assert!(check_caps(&d).is_ok());
        d.variable_collections.push(collection(style::MAX_ENTRY_COLLECTIONS));
        assert!(check_caps(&d).unwrap_err().starts_with("variable: variable_collection_over_cap"));
        let mut d = StudioDesignRecords::new("SDOC-019abcde-0000-7000-8000-000000000001".to_owned());
        d.variables = (0..=style::MAX_ENTRY_VARIABLES).map(|i| variable(i, StudioValue::Float { value: 0.0 })).collect();
        assert!(check_caps(&d).unwrap_err().starts_with("variable: variable_over_cap"));
        let mut d = StudioDesignRecords::new("SDOC-019abcde-0000-7000-8000-000000000001".to_owned());
        let mut wide = variable(0, StudioValue::Float { value: 0.0 });
        wide.values_by_mode = (0..=style::MAX_ENTRY_VARIABLE_VALUES).map(|m| (format!("m{m}"), StudioValue::Float { value: 0.0 })).collect();
        d.variables.push(wide);
        assert!(check_caps(&d).unwrap_err().starts_with("variable: variable_value_over_cap"));
    }

    #[test]
    fn scope_admission_follows_the_closed_set() {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
        assert!(admits(&s(&["ALL_SCOPES"]), "OPACITY"));
        assert!(admits(&s(&["ALL_FILLS"]), "TEXT_FILL") && admits(&s(&["ALL_FILLS"]), "FRAME_FILL"));
        assert!(!admits(&s(&["ALL_FILLS"]), "STROKE_COLOR"));
        assert!(admits(&s(&["GAP"]), "GAP") && !admits(&s(&["GAP"]), "OPACITY"));
        assert!(!admits(&[], "GAP"), "an empty scope list admits nothing");
    }

    #[test]
    fn the_advisory_is_reported_above_the_threshold_only() {
        assert!(advisory(VARIABLE_VALUE_ADVISORY).is_null());
        let a = advisory(VARIABLE_VALUE_ADVISORY + 1);
        assert_eq!(a["code"], "variables_times_modes_over_threshold");
        assert_eq!(a["threshold"], VARIABLE_VALUE_ADVISORY);
    }

    #[test]
    fn names_follow_the_slash_path_and_label_rules() {
        assert!(name_ok("color/blue") && !name_ok("a//b") && !name_ok("") && !name_ok("x\u{7}"));
        assert!(!name_ok(&"x".repeat(257)));
        assert!(label_ok("Dark") && !label_ok("") && !label_ok("line\nbreak") && !label_ok(&"x".repeat(129)));
    }
}
