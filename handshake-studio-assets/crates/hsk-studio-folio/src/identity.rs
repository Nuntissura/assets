//! Identity uniqueness of a document and of the design records that annotate it: the one owner of
//! "which ids share a namespace and what a repeat is reported as" (CX-CAP-001).
//!
//! Folio's validators ([`validate_document`](super::validate_document) and
//! [`validate_design_records`](super::validate_design_records)) claim ids through [`Namespaces`] at the
//! point where each record is checked, so their refusal order is unchanged. A consumer that must index
//! records without running the full validators (an evaluator that has to refuse a repeated id before
//! it builds a lookup) calls [`check_identity_uniqueness`] instead of re-deriving the namespaces.
//!
//! Namespaces (a repeat of an id inside one namespace is `Code::DuplicateIdentity`, target = the
//! repeated id unless stated otherwise):
//! * document nodes: artboards, page spreads and layers share ONE namespace;
//! * design entries: one `StudioNodeDesign` per node id;
//! * component definitions: components and component sets share ONE namespace (target = the repeated
//!   component or set id);
//! * container layers: a layer is the container of at most one component or set (target = the id of
//!   the component or set that claims it second, not the layer id);
//! * component instances, variable collections, variables and paint styles: one namespace each.
use super::{
    Code, Result, StudioDesignRecords, StudioDocument, StudioVariable, StudioVariableCollection,
    err,
};
use std::collections::BTreeSet;

fn claim<'a>(seen: &mut BTreeSet<&'a str>, key: &'a str, target: &str) -> Result<()> {
    if seen.insert(key) {
        Ok(())
    } else {
        Err(err(Code::DuplicateIdentity, target))
    }
}

/// The ids claimed so far in every identity namespace. Crate-private: the public entry points are
/// [`check_identity_uniqueness`] and [`check_variable_identity_uniqueness`].
#[derive(Default)]
pub(crate) struct Namespaces<'a> {
    nodes: BTreeSet<&'a str>,
    entries: BTreeSet<&'a str>,
    definitions: BTreeSet<&'a str>,
    containers: BTreeSet<&'a str>,
    instances: BTreeSet<&'a str>,
    collections: BTreeSet<&'a str>,
    variables: BTreeSet<&'a str>,
    styles: BTreeSet<&'a str>,
}

impl<'a> Namespaces<'a> {
    /// An artboard, page spread or layer id.
    pub(crate) fn node(&mut self, id: &'a str) -> Result<()> {
        claim(&mut self.nodes, id, id)
    }
    /// The design entry of node `id`.
    pub(crate) fn entry(&mut self, id: &'a str) -> Result<()> {
        claim(&mut self.entries, id, id)
    }
    /// A component or component set: its id, then its container layer (reported with the id).
    pub(crate) fn definition(&mut self, component_id: &'a str, layer_id: &'a str) -> Result<()> {
        claim(&mut self.definitions, component_id, component_id)?;
        claim(&mut self.containers, layer_id, component_id)
    }
    pub(crate) fn instance(&mut self, id: &'a str) -> Result<()> {
        claim(&mut self.instances, id, id)
    }
    pub(crate) fn collection(&mut self, id: &'a str) -> Result<()> {
        claim(&mut self.collections, id, id)
    }
    pub(crate) fn variable(&mut self, id: &'a str) -> Result<()> {
        claim(&mut self.variables, id, id)
    }
    pub(crate) fn style(&mut self, id: &'a str) -> Result<()> {
        claim(&mut self.styles, id, id)
    }
}

fn claim_variables<'a>(
    ns: &mut Namespaces<'a>,
    collections: &'a [StudioVariableCollection],
    variables: &'a [StudioVariable],
) -> Result<()> {
    for k in collections {
        ns.collection(&k.collection_id)?;
    }
    for v in variables {
        ns.variable(&v.variable_id)?;
    }
    Ok(())
}

/// Refuses the first repeated id in the identity namespaces of `document` and the design records
/// `records` that annotate it, with `Code::DuplicateIdentity`; see the module documentation for the
/// namespaces and the reported target. Only uniqueness is checked: id grammar, field ranges,
/// references between records, budgets and the document-versus-records `document_id` are the
/// validators' ([`validate_document`](super::validate_document),
/// [`validate_design_records`](super::validate_design_records)), which claim the same namespaces
/// through the same code. The pass is pure, allocation-light and linear in the number of records.
///
/// First refusal wins, in this order: document nodes (artboards, page spreads, layers), design
/// entries, component definitions and their container layers (components, then component sets),
/// variable collections, variables, component instances, paint styles.
pub fn check_identity_uniqueness(
    document: &StudioDocument,
    records: &StudioDesignRecords,
) -> Result<()> {
    let mut ns = Namespaces::default();
    for a in &document.artboards {
        ns.node(&a.artboard_id)?;
    }
    for s in &document.page_spreads {
        ns.node(&s.page_spread_id)?;
    }
    for l in &document.layers {
        ns.node(&l.layer_id)?;
    }
    for n in &records.nodes {
        ns.entry(&n.node.node_id)?;
    }
    for k in &records.components {
        ns.definition(&k.component_id, &k.layer_id)?;
    }
    for s in &records.component_sets {
        ns.definition(&s.component_id, &s.layer_id)?;
    }
    claim_variables(&mut ns, &records.variable_collections, &records.variables)?;
    for i in &records.component_instances {
        ns.instance(&i.instance_id)?;
    }
    for s in &records.paint_styles {
        ns.style(&s.style_id)?;
    }
    Ok(())
}

/// The variable part of [`check_identity_uniqueness`] for a caller that holds only the collection
/// and variable slices: a repeated collection id, then a repeated variable id, is
/// `Code::DuplicateIdentity` with the repeated id as the target.
pub fn check_variable_identity_uniqueness(
    collections: &[StudioVariableCollection],
    variables: &[StudioVariable],
) -> Result<()> {
    claim_variables(&mut Namespaces::default(), collections, variables)
}
