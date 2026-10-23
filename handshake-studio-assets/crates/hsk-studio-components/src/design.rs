//! The Folio design records this crate evaluates, read through one borrowed index, and the few
//! things Folio deliberately does not store.
//!
//! Folio owns the records: `StudioDesignRecords` (`hsk.studio.design_records@1`) holds components,
//! component sets, instances, variable collections, variables and the per-node design entries
//! (auto layout, child layout, size clamps, constraints, reactions, component property references),
//! and `validate_design_records` is the one validator for them. This crate keeps no copy. A component
//! definition is a group layer of the Folio document (`StudioComponent::layer_id`); its definition
//! nodes are the layers under it, so names, hierarchy and sibling order come from the document.
//!
//! Crate-local ([`LocalContent`], recorded gaps in Folio's design-record notes): literal field values
//! of definition nodes, variable bindings of definition fields, and the authored geometry of a layer
//! (Folio layers carry no x/y/width/height). Local numbers are in the document's geometry unit, as
//! Folio's spacing and padding are; `RectPt`/`SizePt` are points when the document unit is `pt`.
//!
//! [`Design::new`] indexes and refuses only a repeated id (`Duplicate`), the one defect an index
//! cannot re-check later: a lookup by a repeated id would pick one record silently. The id
//! namespaces are Folio's (`hsk_studio_folio::check_identity_uniqueness`, the single owner,
//! CX-CAP-001); this crate keeps no copy of the rules. `new` runs no other validation (test
//! `design_refuses_repeated_ids`). [`Design::validate`] is the validation step
//! (Folio, then the crate-local rules). The evaluators re-check what they depend on, so an
//! unvalidated set with these defects yields a typed refusal: alias cycles and alias type clashes
//! (test `alias_type_disjoint_stale_cancel`), a missing mode value (`override_rules_and_properties`),
//! container/child sizing disagreement (`wrap_lines_and_typed_refusals`).
use super::constraints::RectPt;
use super::instance_resolution::{Limits, VariableTable, check_id, check_key, check_value, unique};
use crate::{ComponentsError as E, R};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    Budget, ComponentPropertyType, Parent, ResolutionIssue, Resolver, StudioComponent,
    StudioComponentInstance, StudioComponentProperty, StudioComponentSet, StudioDesignRecords,
    StudioDocument, StudioLayer, StudioNodeDesign, StudioValue, Unresolved,
    check_identity_uniqueness, validate_design_records,
};
use std::collections::{BTreeMap, BTreeSet};

/// What Folio has no home for, per definition layer (keyed by the layer's `SLYR` id).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NodeLocal {
    /// Literal field values of a definition node (tier 3 of STU-DS-116). Never an alias: a field
    /// driven by a variable is a binding.
    pub fields: BTreeMap<String, StudioValue>,
    /// Field name to the `SVAR` variable bound on the definition (tier 4).
    pub variable_bindings: BTreeMap<String, String>,
    /// Authored rectangle in the parent's local space (x, y, width, height). Feeds constraint
    /// resize and auto layout; the rectangle of a component's container layer is the component size.
    pub rect: Option<RectPt>,
}

/// The crate-local companion of the Folio records.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LocalContent {
    pub nodes: BTreeMap<String, NodeLocal>,
}

/// A layer inside a component definition, as the evaluators read it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DefNode<'a> {
    pub node_id: &'a str,
    /// The Folio layer name (override re-matching after a swap uses it).
    pub name: &'a str,
    /// `None` for a child of the component's container layer.
    pub parent: Option<&'a str>,
    pub design: Option<&'a StudioNodeDesign>,
    pub local: Option<&'a NodeLocal>,
}

/// Borrowed read index over one document, its design records and the local content.
#[derive(Clone, Debug)]
pub struct Design<'a> {
    document: &'a StudioDocument,
    records: &'a StudioDesignRecords,
    local: &'a LocalContent,
    layers: BTreeMap<&'a str, &'a StudioLayer>,
    children: BTreeMap<&'a str, Vec<&'a str>>,
    designs: BTreeMap<&'a str, &'a StudioNodeDesign>,
    components: BTreeMap<&'a str, &'a StudioComponent>,
    sets: BTreeMap<&'a str, &'a StudioComponentSet>,
    member_of: BTreeMap<&'a str, &'a StudioComponentSet>,
    vars: VariableTable<'a>,
}

impl<'a> Design<'a> {
    /// Indexes the inputs and refuses a repeated id with `Duplicate`, so every lookup names exactly
    /// one record. The namespaces and the reported id are Folio's
    /// (`hsk_studio_folio::check_identity_uniqueness`): document node ids (artboards, page spreads and
    /// layers share one namespace), design entry node ids, component and set ids (one namespace), the
    /// container layer of a component or set (reported with the component or set id), variable
    /// collection ids, variable ids, component instance ids and paint style ids. Nothing else is
    /// validated here; see [`Design::validate`].
    pub fn new(
        document: &'a StudioDocument,
        records: &'a StudioDesignRecords,
        local: &'a LocalContent,
    ) -> R<Self> {
        check_identity_uniqueness(document, records).map_err(E::from)?;
        let mut layers: BTreeMap<&str, &StudioLayer> = BTreeMap::new();
        let mut children: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for l in &document.layers {
            let id = l.layer_id.as_str();
            layers.insert(id, l);
            if let Parent::Node(p) = &l.parent {
                children.entry(p.node_id.as_str()).or_default().push(id);
            }
        }
        // Every id below is unique (checked above), so a plain insert never replaces a record.
        let mut designs: BTreeMap<&str, &StudioNodeDesign> = BTreeMap::new();
        for n in &records.nodes {
            designs.insert(n.node.node_id.as_str(), n);
        }
        let mut components: BTreeMap<&str, &StudioComponent> = BTreeMap::new();
        for k in &records.components {
            components.insert(k.component_id.as_str(), k);
        }
        let mut sets: BTreeMap<&str, &StudioComponentSet> = BTreeMap::new();
        let mut set_by_layer: BTreeMap<&str, &StudioComponentSet> = BTreeMap::new();
        for s in &records.component_sets {
            sets.insert(s.component_id.as_str(), s);
            set_by_layer.insert(s.layer_id.as_str(), s);
        }
        // Set membership is a document fact: the components whose layer is a child of the set layer.
        let mut member_of: BTreeMap<&str, &StudioComponentSet> = BTreeMap::new();
        for k in &records.components {
            let parent = layers
                .get(k.layer_id.as_str())
                .and_then(|l| match &l.parent {
                    Parent::Node(p) => Some(p.node_id.as_str()),
                    Parent::Root(()) => None,
                });
            if let Some(s) = parent.and_then(|p| set_by_layer.get(p)) {
                member_of.insert(k.component_id.as_str(), *s);
            }
        }
        Ok(Self {
            document,
            records,
            local,
            layers,
            children,
            designs,
            components,
            sets,
            member_of,
            vars: VariableTable::index(&records.variable_collections, &records.variables),
        })
    }

    pub fn document(&self) -> &'a StudioDocument {
        self.document
    }
    pub fn records(&self) -> &'a StudioDesignRecords {
        self.records
    }
    pub fn local(&self) -> &'a LocalContent {
        self.local
    }
    pub fn variables(&self) -> &VariableTable<'a> {
        &self.vars
    }
    pub fn node_design(&self, node_id: &str) -> Option<&'a StudioNodeDesign> {
        self.designs.get(node_id).copied()
    }
    pub(crate) fn layer(&self, node_id: &str) -> Option<&'a StudioLayer> {
        self.layers.get(node_id).copied()
    }
    /// Child layers of a node in document order.
    pub(crate) fn children_of(&self, node_id: &str) -> &[&'a str] {
        self.children.get(node_id).map_or(&[], Vec::as_slice)
    }

    /// The component definition `id` names: a plain component, a variant member, or (for a set id)
    /// the set's default variant.
    pub fn component(&self, id: &str) -> Option<&'a StudioComponent> {
        if let Some(k) = self.components.get(id) {
            return Some(k);
        }
        let set = self.sets.get(id)?;
        let default = self.components.get(set.default_variant_id.as_str())?;
        let in_set = self
            .member_of
            .get(default.component_id.as_str())
            .is_some_and(|s| s.component_id == set.component_id);
        in_set.then_some(*default)
    }
    /// The set `id` names, or the set a member `id` belongs to.
    pub fn set_of(&self, id: &str) -> Option<&'a StudioComponentSet> {
        self.sets
            .get(id)
            .or_else(|| self.member_of.get(id))
            .copied()
    }
    /// Members of a set (components whose layer is a child of the set layer), in record order.
    pub fn members(&self, set: &StudioComponentSet) -> Vec<&'a StudioComponent> {
        self.records
            .components
            .iter()
            .filter(|k| {
                self.member_of
                    .get(k.component_id.as_str())
                    .is_some_and(|s| s.component_id == set.component_id)
            })
            .collect()
    }

    /// The layers under a component's container layer, parents before children, siblings in
    /// document order. Repeated or cyclic parents cannot loop: a layer is visited once.
    pub(crate) fn definition_nodes(&self, component: &StudioComponent) -> Vec<DefNode<'a>> {
        let mut out = Vec::new();
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        if let Some((root, _)) = self.layers.get_key_value(component.layer_id.as_str()) {
            seen.insert(*root);
        }
        let mut stack: Vec<(&'a str, Option<&'a str>)> = self
            .children_of(&component.layer_id)
            .iter()
            .rev()
            .map(|id| (*id, None))
            .collect();
        while let Some((id, parent)) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            let Some(layer) = self.layers.get(id) else {
                continue;
            };
            out.push(DefNode {
                node_id: id,
                name: layer.name.as_str(),
                parent,
                design: self.designs.get(id).copied(),
                local: self.local.nodes.get(id),
            });
            stack.extend(self.children_of(id).iter().rev().map(|c| (*c, Some(id))));
        }
        out
    }

    /// Property definitions in force for a component: its own, then its set's (a member's win).
    /// A name defined twice in one list is `Duplicate(name)` (Folio refuses it too), never a pick.
    pub(crate) fn property_definitions(
        &self,
        member: &'a StudioComponent,
        set: Option<&'a StudioComponentSet>,
    ) -> R<BTreeMap<&'a str, &'a StudioComponentProperty>> {
        let mut defs = BTreeMap::new();
        for list in [
            set.map_or(&[][..], |s| s.component_property_definitions.as_slice()),
            member.component_property_definitions.as_slice(),
        ] {
            let mut names = BTreeSet::new();
            for d in list {
                unique(&mut names, &d.property_name, &d.property_name)?;
                defs.insert(d.property_name.as_str(), d);
            }
        }
        Ok(defs)
    }

    fn budget(limits: &Limits) -> Budget {
        Budget {
            nodes: limits.max_nodes,
            ..Budget::default()
        }
    }

    /// `folio::validate_design_records` over the record set, then the crate-local rules. Returns
    /// Folio's unresolved-reference issues (colour profiles, style references) for the host.
    ///
    /// Crate-local rules (not stated by Folio): a variant set holds two or more components
    /// (STU-DS-102; Folio leaves it unenforced); `SLOT` properties are recognised and refused;
    /// local content names layers of the document, holds no alias literal, and a field has exactly
    /// one of literal and binding; a definition node with authored geometry needs a container
    /// rectangle (the component size).
    pub fn validate(
        &self,
        limits: &Limits,
        cancel: &CancellationToken,
        resolver: &dyn Resolver,
    ) -> R<Vec<ResolutionIssue>> {
        cancel.check().map_err(|_| E::Canceled)?;
        let issues = validate_design_records(
            self.records,
            self.document,
            Self::budget(limits),
            cancel,
            resolver,
        )
        .map_err(E::from)?;
        self.check_local(cancel)?;
        Ok(issues)
    }

    /// `validate_design_records` over the record set with `instance` replacing the stored instance
    /// of the same id (or joining the set). A proposal is accepted only if the successor validates.
    pub fn validate_with_instance(
        &self,
        instance: &StudioComponentInstance,
        limits: &Limits,
        cancel: &CancellationToken,
    ) -> R<()> {
        let mut records = self.records.clone();
        match records
            .component_instances
            .iter_mut()
            .find(|i| i.instance_id == instance.instance_id)
        {
            Some(slot) => *slot = instance.clone(),
            None => records.component_instances.push(instance.clone()),
        }
        validate_design_records(
            &records,
            self.document,
            Self::budget(limits),
            cancel,
            &Unresolved,
        )
        .map(|_| ())
        .map_err(E::from)
    }

    fn check_local(&self, cancel: &CancellationToken) -> R<()> {
        for (id, n) in &self.local.nodes {
            cancel.check().map_err(|_| E::Canceled)?;
            check_id(id, "SLYR")?;
            if !self.layers.contains_key(id.as_str()) {
                return Err(E::NoSuchNode(id.clone()));
            }
            for (f, v) in &n.fields {
                check_key(f)?;
                check_value(v, id, false)?;
            }
            for (f, v) in &n.variable_bindings {
                check_key(f)?;
                check_id(v, "SVAR")?;
                if n.fields.contains_key(f) {
                    return Err(E::AmbiguousSource {
                        node_id: id.clone(),
                        field: f.clone(),
                    });
                }
            }
            if let Some(r) = n.rect
                && (![r.x, r.y].iter().all(|v| v.is_finite())
                    || ![r.w, r.h].iter().all(|v| v.is_finite() && *v >= 0.0))
            {
                return Err(E::InvalidGeometry);
            }
        }
        let slot = |defs: &[StudioComponentProperty]| {
            defs.iter()
                .find(|d| d.property_type == ComponentPropertyType::Slot)
                .map_or(Ok(()), |d| {
                    Err(E::UnsupportedProperty(d.property_name.clone()))
                })
        };
        for s in &self.records.component_sets {
            slot(&s.component_property_definitions)?;
            if self.members(s).len() < 2 {
                return Err(E::NoSuchVariant);
            }
        }
        for k in &self.records.components {
            cancel.check().map_err(|_| E::Canceled)?;
            slot(&k.component_property_definitions)?;
            let has_rect = |id: &str| self.local.nodes.get(id).is_some_and(|n| n.rect.is_some());
            if !has_rect(&k.layer_id)
                && self.definition_nodes(k).iter().any(|n| has_rect(n.node_id))
            {
                return Err(E::InvalidGeometry);
            }
        }
        Ok(())
    }
}
