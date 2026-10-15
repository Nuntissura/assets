//! `StudioDesignRecords`: the design records that the spec places on `StudioLayer` / `StudioDocument`
//! and that Folio keeps beside the document because adding fields to those structs would break every
//! struct-literal constructor in the workspace (interop-media, pulse, layout, components, interop-vector
//! build them). One record set annotates one document (`document_id`); every reference into the
//! document is a node id, and a reference to a record is checked against the same set.
//!
//! The set carries per-node design entries (auto layout, child layout, size clamps, constraints,
//! reactions, component property references, explicit variable modes, style references),
//! components, component sets, instances, variable collections, variables and paint styles. There is no spec primitive name for the
//! set; its members keep their spec names and schema tokens. A document that has no design records
//! is unchanged and is not re-encoded.
//!
//! [`validate_design_records`] is the single cross-record validator: it checks each record, then the
//! rules that need two records or the document (variant completeness and uniqueness, STU-DS-109 /
//! 173 / 174; mode coverage and alias cycles, STU-DS-121 / 175; auto-layout parentage and the
//! constraints-versus-auto-layout rule of STU-DS-048; reaction and mode references).
//!
//! Declared gaps, not invented here: the record set carries no revision (Folio revisions are
//! caller-owned and a snapshot is only built for `StudioDocument`); pages carry no
//! `explicit_variable_modes` (only nodes can be keyed; flow starting points are keyed by the page
//! node in `prototype_pages` since lane SMB-INTEROP-FIG-PROTOTYPE); instance layers are
//! linked to `SCIN` records by the existing `PrimitiveReference` payload, which this validator does
//! not duplicate; whether an override's `overridden_node_id` is a definition node id or an
//! instance-scoped id is not stated by STU-DS-117, so only the `SLYR` grammar is checked; a member
//! count of two or more per set (STU-DS-102) is not enforced; the alias-cycle check uses the union of
//! all modes (a conservative reading of STU-DS-175).
use super::{
    Budget, Code, NodeKind, NodeRef, Parent, Payload, Resolution, ResolutionIssue, Resolver,
    Result, StudioDocument,
    autolayout::{
        ChildPositioning, StudioAutoLayout, StudioChildLayout, StudioConstraint, StudioSizeClamps,
        validate_auto_layout, validate_clamps,
    },
    canceled,
    components::{
        ComponentPropertyType, StudioComponent, StudioComponentInstance, StudioComponentProperty,
        StudioComponentSet, StudioPropertyValue, StudioVariable, StudioVariableCollection,
        validate_collection, validate_component, validate_instance, validate_set,
        validate_variable,
    },
    err, id,
    identity::Namespaces,
    paint::{STYLE_REF_FIELDS, StudioPaintStyle, validate_style},
    prototype::{StudioFrameOverflow, StudioPrototypePage, check_prototype},
    reactions::{StudioReaction, validate_reactions},
    values::{Ctx, VariableType, label},
};
use hsk_studio_accord::CancellationToken;
use schemars::{JsonSchema, generate::SchemaSettings};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

schema_token!(DesignRecordsSchema, "hsk.studio.design_records@1");

/// The design fields of one node (an artboard, a page spread or a layer). Every member is optional;
/// an absent member is the spec default and is not encoded.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioNodeDesign {
    pub node: NodeRef,
    /// STU-DS-144 container fields; the node must be an artboard or a `group` layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_layout: Option<StudioAutoLayout>,
    /// STU-DS-148 fields; the node's parent must be an auto-layout container.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_layout: Option<StudioChildLayout>,
    /// STU-DS-149 clamps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_clamps: Option<StudioSizeClamps>,
    /// STU-DS-152 constraints; legal when the parent is not an auto-layout container, or when the
    /// child is `ABSOLUTE` (STU-DS-048).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constraints: Option<StudioConstraint>,
    /// STU-PRO-103 ordered reactions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reactions: Vec<StudioReaction>,
    /// STU-DS-113: field name to component-property reference, on a layer inside a component
    /// definition.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub component_property_references: BTreeMap<String, String>,
    /// STU-DS-132: `SVCL` collection id to mode id, on a container node.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub explicit_variable_modes: BTreeMap<String, String>,
    /// STU-DS-136: style-reference field name (one of [`STYLE_REF_FIELDS`]) to `SSTY` style id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub style_refs: BTreeMap<String, String>,
}

/// The design records of one document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioDesignRecords {
    pub schema_id: DesignRecordsSchema,
    /// `SDOC` id of the document these records annotate.
    pub document_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<StudioNodeDesign>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<StudioComponent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub component_sets: Vec<StudioComponentSet>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub component_instances: Vec<StudioComponentInstance>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variable_collections: Vec<StudioVariableCollection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variables: Vec<StudioVariable>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paint_styles: Vec<StudioPaintStyle>,
    /// STU-PRO-128/129 flow starting points and entry frame per page (`crate::prototype`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prototype_pages: Vec<StudioPrototypePage>,
    /// STU-PRO-138 overflow (scroll) direction per frame (`crate::prototype`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frame_overflow: Vec<StudioFrameOverflow>,
}
impl StudioDesignRecords {
    /// An empty set for `document_id`.
    pub fn new(document_id: impl Into<String>) -> Self {
        Self {
            schema_id: DesignRecordsSchema::V1,
            document_id: document_id.into(),
            nodes: Vec::new(),
            components: Vec::new(),
            component_sets: Vec::new(),
            component_instances: Vec::new(),
            variable_collections: Vec::new(),
            variables: Vec::new(),
            paint_styles: Vec::new(),
            prototype_pages: Vec::new(),
            frame_overflow: Vec::new(),
        }
    }
    /// The design entry of a node.
    pub fn node_design(&self, node_id: &str) -> Option<&StudioNodeDesign> {
        self.nodes.iter().find(|n| n.node.node_id == node_id)
    }
}

/// JSON schema (draft 2020-12) of the design record set.
pub fn design_records_schema() -> schemars::Schema {
    SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<StudioDesignRecords>()
}

/// Record lookups that reaction validation needs.
pub(crate) struct Refs<'a> {
    pub(crate) nodes: BTreeSet<&'a str>,
    pub(crate) variables: BTreeMap<&'a str, &'a StudioVariable>,
    pub(crate) collections: BTreeMap<&'a str, &'a StudioVariableCollection>,
}

struct DocNode<'a> {
    kind: NodeKind,
    container: bool,
    parent: Option<&'a str>,
}

fn parent_id(p: &Parent) -> Option<&str> {
    match p {
        Parent::Node(n) => Some(n.node_id.as_str()),
        Parent::Root(()) => None,
    }
}

fn index(d: &StudioDocument) -> BTreeMap<&str, DocNode<'_>> {
    let parent = parent_id;
    let mut nodes = BTreeMap::new();
    for a in &d.artboards {
        nodes.insert(
            a.artboard_id.as_str(),
            DocNode {
                kind: NodeKind::Artboard,
                container: true,
                parent: parent(&a.parent),
            },
        );
    }
    for s in &d.page_spreads {
        nodes.insert(
            s.page_spread_id.as_str(),
            DocNode {
                kind: NodeKind::PageSpread,
                container: true,
                parent: parent(&s.parent),
            },
        );
    }
    for l in &d.layers {
        nodes.insert(
            l.layer_id.as_str(),
            DocNode {
                kind: NodeKind::Layer,
                container: l.kind == "group" && matches!(l.payload, Payload::Group),
                parent: parent(&l.parent),
            },
        );
    }
    nodes
}

/// True when `ancestor` is a strict ancestor of `node` (bounded by the node count, so a cyclic
/// document cannot loop).
fn inside(nodes: &BTreeMap<&str, DocNode<'_>>, node: &str, ancestor: &str) -> bool {
    let mut cursor = nodes.get(node).and_then(|n| n.parent);
    for _ in 0..nodes.len() {
        match cursor {
            Some(p) if p == ancestor => return true,
            Some(p) => cursor = nodes.get(p).and_then(|n| n.parent),
            None => return false,
        }
    }
    false
}

/// The union alias graph of all variables; returns a variable on a cycle.
fn alias_cycle<'a>(
    edges: &BTreeMap<&'a str, Vec<&'a str>>,
    token: &CancellationToken,
) -> Result<Option<&'a str>> {
    let mut state: BTreeMap<&str, u8> = BTreeMap::new();
    for &start in edges.keys() {
        if state.contains_key(start) {
            continue;
        }
        state.insert(start, 1);
        let mut stack: Vec<(&str, usize)> = vec![(start, 0)];
        while let Some((node, next)) = stack.last().copied() {
            canceled(token)?;
            let targets = edges.get(node).map_or(&[][..], Vec::as_slice);
            if let Some(&target) = targets.get(next) {
                if let Some(top) = stack.last_mut() {
                    top.1 += 1;
                }
                match state.get(target) {
                    Some(1) => return Ok(Some(target)),
                    Some(_) => {}
                    None => {
                        state.insert(target, 1);
                        stack.push((target, 0));
                    }
                }
            } else {
                state.insert(node, 2);
                stack.pop();
            }
        }
    }
    Ok(None)
}

/// `BOOLEAN` properties bind to `BOOLEAN` variables and `TEXT` properties to `STRING` variables
/// (STU-DS-110); the other property types state no variable type.
fn bound_type_ok(t: ComponentPropertyType, v: VariableType) -> bool {
    match t {
        ComponentPropertyType::Boolean => v == VariableType::Boolean,
        ComponentPropertyType::Text => v == VariableType::String,
        _ => true,
    }
}

fn check_variables(r: &StudioDesignRecords, refs: &Refs, c: &mut Ctx) -> Result<()> {
    let mut members: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for v in &r.variables {
        c.tick()?;
        let vid = v.variable_id.as_str();
        let collection = refs
            .collections
            .get(v.variable_collection_id.as_str())
            .ok_or_else(|| {
                err(
                    Code::DanglingReference,
                    format!("{vid}/variable_collection_id"),
                )
            })?;
        members
            .entry(collection.collection_id.as_str())
            .or_default()
            .insert(vid);
        let modes: BTreeSet<&str> = collection
            .modes
            .iter()
            .map(|m| m.mode_id.as_str())
            .collect();
        let have: BTreeSet<&str> = v.values_by_mode.keys().map(String::as_str).collect();
        if modes != have {
            return Err(err(Code::InvalidField, format!("{vid}/values_by_mode")));
        }
    }
    for k in &r.variable_collections {
        c.tick()?;
        let cid = k.collection_id.as_str();
        let listed: BTreeSet<&str> = k.variable_ids.iter().map(String::as_str).collect();
        if members.get(cid).cloned().unwrap_or_default() != listed {
            return Err(err(Code::InvalidField, format!("{cid}/variable_ids")));
        }
    }
    let mut edges: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for v in &r.variables {
        let vid = v.variable_id.as_str();
        let targets = edges.entry(vid).or_default();
        for value in v.values_by_mode.values() {
            let Some(alias) = value.alias_target() else {
                continue;
            };
            let target = refs
                .variables
                .get(alias)
                .ok_or_else(|| err(Code::DanglingReference, format!("{vid}/values_by_mode")))?;
            if target.resolved_type != value.resolved_type() {
                return Err(err(
                    Code::InvalidField,
                    format!("{vid}/values_by_mode/type"),
                ));
            }
            targets.push(target.variable_id.as_str());
        }
    }
    if let Some(on_cycle) = alias_cycle(&edges, c.token)? {
        return Err(err(Code::VariableAliasCycle, on_cycle));
    }
    Ok(())
}

fn check_components(
    r: &StudioDesignRecords,
    nodes: &BTreeMap<&str, DocNode<'_>>,
    c: &mut Ctx,
) -> Result<()> {
    for (owner, layer) in r
        .components
        .iter()
        .map(|k| (k.component_id.as_str(), k.layer_id.as_str()))
        .chain(
            r.component_sets
                .iter()
                .map(|s| (s.component_id.as_str(), s.layer_id.as_str())),
        )
    {
        c.tick()?;
        if !nodes
            .get(layer)
            .is_some_and(|n| n.kind == NodeKind::Layer && n.container)
        {
            return Err(err(Code::DanglingReference, format!("{owner}/layer_id")));
        }
    }
    let mut member_of_set: BTreeSet<&str> = BTreeSet::new();
    for s in &r.component_sets {
        c.tick()?;
        let sid = s.component_id.as_str();
        let members: Vec<&StudioComponent> = r
            .components
            .iter()
            .filter(|k| nodes.get(k.layer_id.as_str()).and_then(|n| n.parent) == Some(&s.layer_id))
            .collect();
        if !members
            .iter()
            .any(|k| k.component_id == s.default_variant_id)
        {
            return Err(err(
                Code::DanglingReference,
                format!("{sid}/default_variant_id"),
            ));
        }
        let mut assignments: BTreeSet<Vec<(&str, &str)>> = BTreeSet::new();
        for k in members {
            let kid = k.component_id.as_str();
            member_of_set.insert(kid);
            let Some(assigned) = &k.variant_properties else {
                return Err(err(Code::VariantAssignmentIncomplete, kid));
            };
            if assigned.iter().any(|(name, value)| {
                !s.variant_group_properties
                    .get(name.as_str())
                    .is_some_and(|values| values.contains(value))
            }) {
                return Err(err(Code::InvalidField, format!("{kid}/variant_properties")));
            }
            if s.variant_group_properties
                .keys()
                .any(|name| !assigned.contains_key(name))
            {
                return Err(err(Code::VariantAssignmentIncomplete, kid));
            }
            let assignment = assigned
                .iter()
                .map(|(n, v)| (n.as_str(), v.as_str()))
                .collect::<Vec<_>>();
            if !assignments.insert(assignment) {
                return Err(err(Code::VariantAssignmentDuplicate, kid));
            }
        }
    }
    for k in &r.components {
        if k.variant_properties.is_some() && !member_of_set.contains(k.component_id.as_str()) {
            return Err(err(
                Code::InvalidField,
                format!("{}/variant_properties", k.component_id),
            ));
        }
    }
    Ok(())
}

/// Property definitions of a component: its own, plus its set's when it is a variant member.
fn definitions_of<'a>(
    r: &'a StudioDesignRecords,
    nodes: &BTreeMap<&str, DocNode<'_>>,
    k: &'a StudioComponent,
) -> Vec<&'a StudioComponentProperty> {
    let mut defs: Vec<&StudioComponentProperty> = k.component_property_definitions.iter().collect();
    let parent = nodes.get(k.layer_id.as_str()).and_then(|n| n.parent);
    if let Some(set) = r
        .component_sets
        .iter()
        .find(|s| Some(s.layer_id.as_str()) == parent)
    {
        defs.extend(set.component_property_definitions.iter());
    }
    defs
}

fn check_instances(
    r: &StudioDesignRecords,
    nodes: &BTreeMap<&str, DocNode<'_>>,
    refs: &Refs,
    c: &mut Ctx,
) -> Result<()> {
    let components: BTreeMap<&str, &StudioComponent> = r
        .components
        .iter()
        .map(|k| (k.component_id.as_str(), k))
        .collect();
    for i in &r.component_instances {
        c.tick()?;
        let iid = i.instance_id.as_str();
        let Some(main) = &i.main_component_id else {
            continue;
        };
        let target = format!("{iid}/main_component_id");
        let Some(component) = components.get(main.as_str()) else {
            let names_a_set = r.component_sets.iter().any(|s| &s.component_id == main);
            return Err(err(
                if names_a_set {
                    Code::InvalidField
                } else {
                    Code::DanglingReference
                },
                target,
            ));
        };
        let defs = definitions_of(r, nodes, component);
        for (n, (name, p)) in i.component_properties.iter().enumerate() {
            let ptarget = format!("{iid}/component_properties/{n}");
            let Some(def) = defs.iter().find(|d| &d.property_name == name) else {
                return Err(err(Code::InvalidField, ptarget));
            };
            if def.property_type != p.property_type {
                return Err(err(Code::InvalidField, ptarget));
            }
            let alias = match &p.value {
                StudioPropertyValue::Alias(a) => Some(a.id.as_str()),
                _ => None,
            };
            for variable in alias.into_iter().chain(p.bound_variable_id.as_deref()) {
                let v = refs
                    .variables
                    .get(variable)
                    .ok_or_else(|| err(Code::DanglingReference, format!("{ptarget}/variable")))?;
                if !bound_type_ok(p.property_type, v.resolved_type) {
                    return Err(err(Code::InvalidField, format!("{ptarget}/variable")));
                }
            }
        }
    }
    Ok(())
}

fn check_nodes<'a>(
    r: &'a StudioDesignRecords,
    nodes: &BTreeMap<&str, DocNode<'_>>,
    refs: &Refs,
    ids: &mut Namespaces<'a>,
    c: &mut Ctx,
) -> Result<()> {
    let mut flow: BTreeSet<&str> = BTreeSet::new();
    for n in &r.nodes {
        if n.auto_layout
            .as_ref()
            .is_some_and(|a| a.layout_mode.is_flow())
        {
            flow.insert(n.node.node_id.as_str());
        }
    }
    let paint_styles: BTreeSet<&str> = r.paint_styles.iter().map(|s| s.style_id.as_str()).collect();
    let definitions: BTreeSet<&str> = r
        .components
        .iter()
        .map(|k| k.layer_id.as_str())
        .chain(r.component_sets.iter().map(|s| s.layer_id.as_str()))
        .collect();
    for n in &r.nodes {
        c.tick()?;
        let nid = n.node.node_id.as_str();
        let found = nodes
            .get(nid)
            .filter(|d| d.kind == n.node.node_kind)
            .ok_or_else(|| err(Code::DanglingReference, nid))?;
        ids.entry(nid)?;
        c.nodes(1, nid)?;
        let in_flow = found.parent.is_some_and(|p| flow.contains(p));
        if let Some(a) = &n.auto_layout {
            if found.kind == NodeKind::PageSpread || !found.container {
                return Err(err(Code::InvalidField, format!("{nid}/auto_layout")));
            }
            validate_auto_layout(a, nid, c)?;
        }
        if let Some(k) = &n.size_clamps {
            validate_clamps(k, nid)?;
        }
        if n.child_layout.is_some() && !in_flow {
            return Err(err(Code::InvalidField, format!("{nid}/child_layout")));
        }
        if let Some(k) = &n.child_layout
            && k.layout_grow > 1
        {
            return Err(err(
                Code::InvalidField,
                format!("{nid}/child_layout/layout_grow"),
            ));
        }
        if n.constraints.is_some() {
            let absolute = n
                .child_layout
                .as_ref()
                .is_some_and(|k| k.layout_positioning == ChildPositioning::Absolute);
            if found.parent.is_none() || (in_flow && !absolute) {
                return Err(err(Code::InvalidField, format!("{nid}/constraints")));
            }
        }
        validate_reactions(&n.reactions, nid, refs, c)?;
        if !n.component_property_references.is_empty() {
            let inside_definition =
                found.kind == NodeKind::Layer && definitions.iter().any(|d| inside(nodes, nid, d));
            if !inside_definition || n.component_property_references.len() > 256 {
                return Err(err(
                    Code::InvalidField,
                    format!("{nid}/component_property_references"),
                ));
            }
            for (field, reference) in &n.component_property_references {
                if !label(field) || !label(reference) {
                    return Err(err(
                        Code::InvalidField,
                        format!("{nid}/component_property_references"),
                    ));
                }
                c.text(field, nid)?;
                c.text(reference, nid)?;
            }
        }
        for (field, style) in &n.style_refs {
            let target = format!("{nid}/style_refs");
            if !STYLE_REF_FIELDS.contains(&field.as_str()) {
                return Err(err(Code::InvalidField, target));
            }
            id(style, "SSTY", &target)?;
            if !paint_styles.contains(style.as_str()) {
                let disposition = c
                    .resolver
                    .primitive(hsk_studio_accord::SCHEMA_STUDIO_STYLE_REGISTRY, style);
                if disposition != Resolution::Available {
                    c.issues.push(ResolutionIssue {
                        target,
                        disposition,
                    });
                }
            }
        }
        if !n.explicit_variable_modes.is_empty() {
            let target = format!("{nid}/explicit_variable_modes");
            if !found.container {
                return Err(err(Code::InvalidField, target));
            }
            for (collection, mode) in &n.explicit_variable_modes {
                id(collection, "SVCL", &target)?;
                let known = refs
                    .collections
                    .get(collection.as_str())
                    .ok_or_else(|| err(Code::DanglingReference, &target))?;
                if !known.modes.iter().any(|m| &m.mode_id == mode) {
                    return Err(err(Code::DanglingReference, &target));
                }
            }
        }
    }
    Ok(())
}

/// Validates a design record set against its document. Each record is checked on its own first
/// (ids, enumerations, ranges, text), then the cross-record rules. Unresolved colour profiles are
/// reported as [`ResolutionIssue`]s; everything else is a typed refusal. Records count against
/// `Budget::nodes` and text against `Budget::payload_bytes`.
pub fn validate_design_records(
    r: &StudioDesignRecords,
    d: &StudioDocument,
    b: Budget,
    token: &CancellationToken,
    resolver: &dyn Resolver,
) -> Result<Vec<ResolutionIssue>> {
    let mut c = Ctx::new(b, token, resolver);
    c.tick()?;
    id(&r.document_id, "SDOC", "document_id")?;
    if r.document_id != d.document_id {
        return Err(err(Code::InvalidField, "document_id"));
    }
    let mut ids = Namespaces::default();
    for k in &r.components {
        validate_component(k, &mut c)?;
        ids.definition(&k.component_id, &k.layer_id)?;
    }
    for s in &r.component_sets {
        validate_set(s, &mut c)?;
        ids.definition(&s.component_id, &s.layer_id)?;
    }
    for i in &r.component_instances {
        validate_instance(i, &mut c)?;
        ids.instance(&i.instance_id)?;
    }
    for k in &r.variable_collections {
        validate_collection(k, &mut c)?;
        ids.collection(&k.collection_id)?;
    }
    for v in &r.variables {
        validate_variable(v, &mut c)?;
        ids.variable(&v.variable_id)?;
    }
    for s in &r.paint_styles {
        validate_style(s, &mut c)?;
        ids.style(&s.style_id)?;
    }
    let nodes = index(d);
    let refs = Refs {
        nodes: nodes.keys().copied().collect(),
        variables: r
            .variables
            .iter()
            .map(|v| (v.variable_id.as_str(), v))
            .collect(),
        collections: r
            .variable_collections
            .iter()
            .map(|k| (k.collection_id.as_str(), k))
            .collect(),
    };
    check_variables(r, &refs, &mut c)?;
    check_components(r, &nodes, &mut c)?;
    check_instances(r, &nodes, &refs, &mut c)?;
    check_nodes(r, &nodes, &refs, &mut ids, &mut c)?;
    let kinds: BTreeMap<&str, NodeKind> = nodes.iter().map(|(k, v)| (*k, v.kind.clone())).collect();
    check_prototype(&r.prototype_pages, &r.frame_overflow, &kinds, &mut c)?;
    c.tick()?;
    Ok(c.issues)
}

/// Validates one paint style on its own (no document needed): ids, name, publishing fields, every
/// appearance row. Unresolved colour profiles are reported as [`ResolutionIssue`]s.
pub fn validate_paint_style(
    style: &StudioPaintStyle,
    b: Budget,
    token: &CancellationToken,
    resolver: &dyn Resolver,
) -> Result<Vec<ResolutionIssue>> {
    let mut c = Ctx::new(b, token, resolver);
    validate_style(style, &mut c)?;
    c.tick()?;
    Ok(c.issues)
}
