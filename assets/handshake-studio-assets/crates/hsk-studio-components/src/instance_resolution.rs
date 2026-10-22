//! Instances of Folio components resolved into plain values with the source of every field, and
//! variables resolved through modes and alias chains (STU-DS-102..129). The records are Folio's
//! (`StudioComponent`, `StudioComponentSet`, `StudioComponentInstance`, `StudioVariable`,
//! `StudioVariableCollection`, `StudioValue`) read through a [`Design`]; this module only evaluates
//! them. Every input is borrowed; nothing is mutated, a base definition is never rewritten by an
//! instance, and a failure never yields a partial result.
//!
//! Decisions recorded for the lane (spec gaps, not source-derived):
//! * STU-DS-117 gives `overridden_node_id` + `overridden_fields[]`; Folio stores the values in the
//!   record's optional `values` map. Every overridden field must carry a value here (a missing one is
//!   `InvalidValue`), and `values` may not hold a field that is not listed. A value may be a
//!   `VARIABLE_ALIAS`; it resolves like any bound variable and its chain joins the read footprint.
//! * STU-DS-116 requires "exactly one source" per field. A definition field with both a literal and a
//!   variable binding, a field listed twice in overrides, or two override records for one node/field
//!   are the ambiguous double-source (`AmbiguousSource`), as is an instance property that names two
//!   different variables (`bound_variable_id` and an alias value). Local override > component
//!   property > definition literal > variable binding is the precedence among distinct tiers.
//! * STU-DS-113 shows the `component_property_references` spelling only by example (`showIcon#12:3`)
//!   and Folio stores it as an opaque label. This crate matches the label to `property_name`
//!   exactly; a label that names no property is `UnsupportedProperty` at resolution.
//! * An instance may name a set (it resolves to `default_variant_id`, STU-DS-104). Folio's validator
//!   accepts only a component as `main_component_id`, so a proposal whose successor names a set is
//!   refused by `validate_with_instance` (`Folio`, invalid field); name the default member instead.
use super::design::{DefNode, Design};
use crate::{ComponentsError as E, R};
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_folio::{
    ComponentPropertyType as PropType, StudioComponent, StudioComponentInstance,
    StudioComponentProperty, StudioComponentSet, StudioInstanceProperty, StudioOverrideRecord,
    StudioPropertyValue, StudioValue, StudioVariable, StudioVariableCollection, VariableType,
    check_variable_identity_uniqueness,
};
use std::collections::{BTreeMap, BTreeSet};

/// STU-DS-115 closed overridable field set: Folio's `OVERRIDABLE_FIELDS`.
pub fn is_overridable(field: &str) -> bool {
    hsk_studio_folio::OVERRIDABLE_FIELDS.contains(&field)
}

const MAX_TEXT: usize = 65_536;
/// Footprint keys: `(record_id, "")` is the record level and `(instance_id, "property/<name>")` a
/// part of an instance record. An override proposal carries the whole successor instance record, so
/// it writes the record key `(instance_id, "")` (STU-ARC-011; tests
/// `concurrent_proposals_on_one_instance_conflict`, `override_write_keys_are_scoped_to_the_instance`).
type Footprint = Vec<(String, String)>;

/// Inserts `key` into `seen`; a repeat is `Duplicate(reported)`.
pub(crate) fn unique<'a>(seen: &mut BTreeSet<&'a str>, key: &'a str, reported: &str) -> R<()> {
    if seen.insert(key) {
        Ok(())
    } else {
        Err(E::Duplicate(reported.to_owned()))
    }
}

pub(crate) fn check_id(s: &str, prefix: &str) -> R<()> {
    match DomainId::parse(s) {
        Ok(d) if d.prefix() == prefix => Ok(()),
        _ => Err(E::InvalidId(s.to_owned())),
    }
}
pub(crate) fn check_key(s: &str) -> R<()> {
    if s.is_empty() || s.len() > 128 || s.chars().any(char::is_control) {
        return Err(E::InvalidValue(s.to_owned()));
    }
    Ok(())
}
/// Guard for values that did not pass through Folio's validator (local literals, override values
/// of an instance that is not in the record set): finite numbers, bounded text, one easing function.
/// Colour ranges and easing bounds are Folio's to enforce. `alias_ok` admits a `VARIABLE_ALIAS`.
pub(crate) fn check_value(v: &StudioValue, target: &str, alias_ok: bool) -> R<()> {
    let ok = match v {
        StudioValue::Boolean { .. } => true,
        StudioValue::Color { value } => [value.r, value.g, value.b, value.a]
            .iter()
            .all(|c| c.is_finite()),
        StudioValue::Easing { value } => match (
            &value.easing_function_cubic_bezier,
            &value.easing_function_spring,
        ) {
            (Some(b), None) => [b.x1, b.y1, b.x2, b.y2].iter().all(|n| n.is_finite()),
            (None, Some(s)) => [s.mass, s.stiffness, s.damping, s.initial_velocity]
                .iter()
                .all(|n| n.is_finite()),
            _ => false,
        },
        StudioValue::Float { value } => value.is_finite(),
        StudioValue::Timing { value } => value.is_finite() && *value >= 0.0,
        StudioValue::String { value } => value.len() <= MAX_TEXT,
        StudioValue::VariableAlias { id, .. } => {
            if !alias_ok {
                false
            } else {
                return check_id(id, "SVAR");
            }
        }
    };
    if ok {
        Ok(())
    } else {
        Err(E::InvalidValue(target.to_owned()))
    }
}

// ----------------------------------------------------------------------------------------------
// Variables

/// Borrowed lookup over Folio variable collections and variables. Built from the record vectors
/// directly so an evaluator can be run (and refuse typed) on a set that Folio's validator rejects;
/// only a repeated collection or variable id is refused here (`Duplicate`), because a lookup by that
/// id would otherwise pick one record silently. The rule is Folio's
/// (`hsk_studio_folio::check_variable_identity_uniqueness`); this crate keeps no copy.
#[derive(Clone, Debug, Default)]
pub struct VariableTable<'a> {
    collections: BTreeMap<&'a str, &'a StudioVariableCollection>,
    variables: BTreeMap<&'a str, &'a StudioVariable>,
}
impl<'a> VariableTable<'a> {
    pub fn new(
        collections: &'a [StudioVariableCollection],
        variables: &'a [StudioVariable],
    ) -> R<Self> {
        check_variable_identity_uniqueness(collections, variables).map_err(E::from)?;
        Ok(Self::index(collections, variables))
    }
    /// The lookup without the uniqueness pass, for a caller that has already run it (`Design::new`
    /// runs Folio's full pass first): a repeated id would keep the last record.
    pub(crate) fn index(
        collections: &'a [StudioVariableCollection],
        variables: &'a [StudioVariable],
    ) -> Self {
        Self {
            collections: collections
                .iter()
                .map(|c| (c.collection_id.as_str(), c))
                .collect(),
            variables: variables
                .iter()
                .map(|v| (v.variable_id.as_str(), v))
                .collect(),
        }
    }
    pub fn variable(&self, id: &str) -> Option<&'a StudioVariable> {
        self.variables.get(id).copied()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_nodes: usize,
    pub max_fields: usize,
    pub max_alias_depth: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_nodes: 4096,
            max_fields: 65_536,
            max_alias_depth: 32,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedVariable {
    /// Never a `VARIABLE_ALIAS`: the literal at the end of the chain.
    pub value: StudioValue,
    /// Collection and mode used for the variable that was asked for (aliases use their own).
    pub collection_id: String,
    pub mode_id: String,
    /// Every variable read, asked-for variable first, alias targets in order.
    pub chain: Vec<String>,
}

/// Resolves a variable to a literal. `modes` maps collection id to the selected mode id; a missing
/// entry selects the collection's default mode. An alias chain is a single path, so the walk is the
/// white/grey/black traversal reduced to a visited list: revisiting a variable is `AliasCycle`.
pub fn resolve_variable(
    vars: &VariableTable,
    variable_id: &str,
    modes: &BTreeMap<String, String>,
    limits: &Limits,
    cancel: &CancellationToken,
) -> R<ResolvedVariable> {
    let mut cur = *vars
        .variables
        .get(variable_id)
        .ok_or_else(|| E::NoSuchVariable(variable_id.to_owned()))?;
    let wanted = cur.resolved_type;
    let mut chain: Vec<String> = Vec::new();
    let mut first: Option<(String, String)> = None;
    loop {
        cancel.check().map_err(|_| E::Canceled)?;
        if chain.contains(&cur.variable_id) {
            return Err(E::AliasCycle(cur.variable_id.clone()));
        }
        chain.push(cur.variable_id.clone());
        if chain.len() > limits.max_alias_depth {
            return Err(E::BudgetExceeded);
        }
        let coll = vars
            .collections
            .get(cur.variable_collection_id.as_str())
            .ok_or_else(|| E::NoSuchCollection(cur.variable_collection_id.clone()))?;
        let mode = modes
            .get(&coll.collection_id)
            .map_or(coll.default_mode_id.as_str(), String::as_str);
        if !coll.modes.iter().any(|m| m.mode_id == mode) {
            return Err(E::NoSuchMode {
                collection_id: coll.collection_id.clone(),
                mode_id: mode.to_owned(),
            });
        }
        first.get_or_insert_with(|| (coll.collection_id.clone(), mode.to_owned()));
        match cur.values_by_mode.get(mode) {
            None => {
                return Err(E::MissingModeValue {
                    variable_id: cur.variable_id.clone(),
                    mode_id: mode.to_owned(),
                });
            }
            Some(StudioValue::VariableAlias {
                id: target,
                resolved_type,
            }) => {
                if *resolved_type != wanted {
                    return Err(E::TypeMismatch(cur.variable_id.clone()));
                }
                let next = *vars
                    .variables
                    .get(target.as_str())
                    .ok_or_else(|| E::NoSuchVariable(target.clone()))?;
                if next.resolved_type != wanted {
                    return Err(E::TypeMismatch(target.clone()));
                }
                cur = next;
            }
            Some(v) => {
                if v.resolved_type() != wanted {
                    return Err(E::TypeMismatch(cur.variable_id.clone()));
                }
                check_value(v, &cur.variable_id, false)?;
                let (collection_id, mode_id) = first.unwrap_or_default();
                return Ok(ResolvedVariable {
                    value: v.clone(),
                    collection_id,
                    mode_id,
                    chain,
                });
            }
        }
    }
}

// ----------------------------------------------------------------------------------------------
// Property values

/// A value fits a component-property type: BOOLEAN takes booleans; TEXT, INSTANCE_SWAP and VARIANT
/// take strings; SLOT takes nothing (refused in this slice).
fn prop_value_ok(ty: PropType, v: &StudioValue) -> bool {
    prop_type_ok(ty, v.resolved_type())
}
/// The value type a component-property type takes (a variable bound to it must resolve to it).
fn prop_type_ok(ty: PropType, value_type: VariableType) -> bool {
    match ty {
        PropType::Boolean => value_type == VariableType::Boolean,
        PropType::Text | PropType::InstanceSwap | PropType::Variant => {
            value_type == VariableType::String
        }
        PropType::Slot => false,
    }
}
/// A literal property value as a value; `None` for an alias.
fn literal(v: &StudioPropertyValue) -> Option<StudioValue> {
    match v {
        StudioPropertyValue::Boolean(b) => Some(StudioValue::Boolean { value: *b }),
        StudioPropertyValue::Text(s) => Some(StudioValue::String { value: s.clone() }),
        StudioPropertyValue::Alias(_) => None,
    }
}
/// The variable a property value or binding names, when it names one. Both spellings naming
/// different variables is a double source.
fn property_variable<'p>(
    instance_id: &str,
    name: &str,
    p: &'p StudioInstanceProperty,
) -> R<Option<&'p str>> {
    let alias = match &p.value {
        StudioPropertyValue::Alias(a) => Some(a.id.as_str()),
        _ => None,
    };
    match (p.bound_variable_id.as_deref(), alias) {
        (Some(a), Some(b)) if a != b => Err(E::AmbiguousSource {
            node_id: instance_id.to_owned(),
            field: format!("property/{name}"),
        }),
        (Some(v), _) | (None, Some(v)) => Ok(Some(v)),
        (None, None) => Ok(None),
    }
}

// ----------------------------------------------------------------------------------------------
// Instances

/// Where a resolved field came from (STU-DS-116 tiers).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Override,
    ComponentProperty(String),
    PropertyDefault(String),
    Definition,
    Variable {
        variable_id: String,
        collection_id: String,
        mode_id: String,
    },
}
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedField {
    pub node_id: String,
    pub field: String,
    /// Never a `VARIABLE_ALIAS`.
    pub value: StudioValue,
    pub source: Source,
    /// Footprint keys this value depended on: `(component_id, "")` and `(variable_id, "")` record
    /// level keys (STU-DS-163) and `(instance_id, "property/<name>")`.
    pub reads: Vec<(String, String)>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Resolution {
    pub base_revision: u64,
    /// The variant member actually used.
    pub component_id: String,
    /// Definition node order, then field name order.
    pub fields: Vec<ResolvedField>,
}
impl Resolution {
    pub fn field(&self, node_id: &str, field: &str) -> Option<&ResolvedField> {
        self.fields
            .iter()
            .find(|f| f.node_id == node_id && f.field == field)
    }
}

fn select_member<'a>(
    inst: &StudioComponentInstance,
    design: &Design<'a>,
) -> R<(&'a StudioComponent, Option<&'a StudioComponentSet>)> {
    let main = inst
        .main_component_id
        .as_deref()
        .ok_or_else(|| E::NoSuchComponent("deleted-main".to_owned()))?;
    let start = design
        .component(main)
        .ok_or_else(|| E::NoSuchComponent(main.to_owned()))?;
    let Some(set) = design.set_of(main) else {
        return Ok((start, None));
    };
    let mut want = start.variant_properties.clone().unwrap_or_default();
    for (name, p) in &inst.component_properties {
        if p.property_type != PropType::Variant {
            continue;
        }
        let declared = set
            .variant_group_properties
            .get(name)
            .ok_or_else(|| E::UnsupportedProperty(name.clone()))?;
        let StudioPropertyValue::Text(v) = &p.value else {
            return Err(E::TypeMismatch(name.clone()));
        };
        if !declared.contains(v) {
            return Err(E::UnknownVariantValue {
                property: name.clone(),
                value: v.clone(),
            });
        }
        want.insert(name.clone(), v.clone());
    }
    // Two members with one assignment (STU-DS-109 duplicate, refused by Folio) are refused here
    // too, never resolved to whichever comes first.
    let mut matching = design
        .members(set)
        .into_iter()
        .filter(|m| m.variant_properties.as_ref() == Some(&want));
    let m = matching.next().ok_or(E::NoSuchVariant)?;
    if let Some(other) = matching.next() {
        return Err(E::VariantAssignmentDuplicate(other.component_id.clone()));
    }
    Ok((m, Some(set)))
}

type Overrides<'a> = BTreeMap<(&'a str, &'a str), &'a StudioValue>;
type PropDefs<'a> = BTreeMap<&'a str, &'a StudioComponentProperty>;

/// Checks an instance property against its definition. `true`: it carries a value to use;
/// `false`: a VARIANT property, which only selects the member and is legal only on a set.
fn check_property(
    name: &str,
    p: &StudioInstanceProperty,
    defs: &PropDefs,
    in_set: bool,
) -> R<bool> {
    if p.property_type == PropType::Variant {
        return if in_set {
            Ok(false)
        } else {
            Err(E::UnsupportedProperty(name.to_owned()))
        };
    }
    let def = defs
        .get(name)
        .ok_or_else(|| E::UnsupportedProperty(name.to_owned()))?;
    if def.property_type == PropType::Slot {
        return Err(E::UnsupportedProperty(name.to_owned()));
    }
    if p.property_type != def.property_type {
        return Err(E::TypeMismatch(name.to_owned()));
    }
    Ok(true)
}

fn check_property_value(name: &str, ty: PropType, value: &StudioValue) -> R<()> {
    check_value(value, name, false)?;
    if !prop_value_ok(ty, value) {
        return Err(E::TypeMismatch(name.to_owned()));
    }
    Ok(())
}

/// Every override must fit the node it names and the type of each source that can feed the field:
/// a definition literal, the referenced component property, the bound variable.
fn build_overrides<'a>(
    inst: &'a StudioComponentInstance,
    nodes: &[DefNode<'a>],
    prop_defs: &PropDefs,
    vars: &VariableTable,
) -> R<Overrides<'a>> {
    let by_id: BTreeMap<&str, &DefNode> = nodes.iter().map(|n| (n.node_id, n)).collect();
    let mut out: Overrides = BTreeMap::new();
    for rec in &inst.overrides {
        let node = by_id
            .get(rec.overridden_node_id.as_str())
            .ok_or_else(|| E::NoSuchNode(rec.overridden_node_id.clone()))?;
        for f in &rec.overridden_fields {
            if !is_overridable(f) {
                return Err(E::FieldNotOverridable {
                    node_id: rec.overridden_node_id.clone(),
                    field: f.clone(),
                });
            }
            let v = rec
                .values
                .get(f)
                .ok_or_else(|| E::InvalidValue(f.clone()))?;
            check_value(v, f, true)?;
            let literal_clash = node
                .local
                .and_then(|l| l.fields.get(f))
                .is_some_and(|lit| lit.resolved_type() != v.resolved_type());
            let property_clash = node
                .design
                .and_then(|d| d.component_property_references.get(f))
                .and_then(|pname| prop_defs.get(pname.as_str()))
                .is_some_and(|def| !prop_value_ok(def.property_type, v));
            let variable_clash = node
                .local
                .and_then(|l| l.variable_bindings.get(f))
                .and_then(|vid| vars.variable(vid))
                .is_some_and(|var| var.resolved_type != v.resolved_type());
            if literal_clash || property_clash || variable_clash {
                return Err(E::TypeMismatch(f.clone()));
            }
            if out.insert((node.node_id, f.as_str()), v).is_some() {
                return Err(E::AmbiguousSource {
                    node_id: rec.overridden_node_id.clone(),
                    field: f.clone(),
                });
            }
        }
        if rec.values.len() != rec.overridden_fields.len() {
            return Err(E::InvalidValue(rec.overridden_node_id.clone()));
        }
    }
    Ok(out)
}

/// Every check `resolve_instance` makes on the instance's own data, without a mode selection: the
/// instance id; every property is defined by the member (or its set) with the right type; a literal
/// property value has that type; a variable bound to a property exists and resolves to that type;
/// every override fits its node, including the type of the node's variable binding; an alias
/// override names an existing variable of the type it declares. Swap and variant switch run it on
/// their result (tests `switch_variant_and_swap_never_return_an_unresolvable_instance`,
/// `swap_and_switch_check_the_variable_types_of_the_result`). Resolution can still refuse for causes
/// outside the instance, which refuse every instance of that target alike: the caller's modes and
/// limits, and defects of the target definition or the variable set (a property reference nobody
/// defines, an alias cycle, a missing mode value; Folio refuses the last two in `Design::validate`).
fn check_shape<'a>(
    inst: &'a StudioComponentInstance,
    member: &'a StudioComponent,
    set: Option<&'a StudioComponentSet>,
    design: &Design<'a>,
) -> R<()> {
    check_id(&inst.instance_id, "SCIN")?;
    let prop_defs = design.property_definitions(member, set)?;
    let vars = design.variables();
    let typed_variable = |vid: &str, fits: &dyn Fn(VariableType) -> bool, refused: String| {
        let var = vars
            .variable(vid)
            .ok_or_else(|| E::NoSuchVariable(vid.to_owned()))?;
        if fits(var.resolved_type) {
            Ok(())
        } else {
            Err(E::TypeMismatch(refused))
        }
    };
    for (name, p) in &inst.component_properties {
        check_key(name)?;
        if !check_property(name, p, &prop_defs, set.is_some())? {
            continue; // VARIANT: it selected the member
        }
        match property_variable(&inst.instance_id, name, p)? {
            Some(vid) => typed_variable(vid, &|t| prop_type_ok(p.property_type, t), name.clone())?,
            None => {
                let value = literal(&p.value).ok_or_else(|| E::TypeMismatch(name.clone()))?;
                check_property_value(name, p.property_type, &value)?;
            }
        }
    }
    let overrides = build_overrides(inst, &design.definition_nodes(member), &prop_defs, vars)?;
    for v in overrides.values() {
        if let StudioValue::VariableAlias { id, resolved_type } = v {
            typed_variable(id, &|t| t == *resolved_type, id.clone())?;
        }
    }
    Ok(())
}

fn record_keys(out: &mut Vec<(String, String)>, ids: &[String]) {
    out.extend(ids.iter().map(|v| (v.clone(), String::new())));
}

/// An override or binding value as a literal: an alias resolves through the variables (its chain
/// joins `reads`) and must land on the type it declares.
fn materialize(
    v: &StudioValue,
    design: &Design,
    modes: &BTreeMap<String, String>,
    limits: &Limits,
    cancel: &CancellationToken,
    reads: &mut Vec<(String, String)>,
) -> R<StudioValue> {
    let StudioValue::VariableAlias { id, resolved_type } = v else {
        return Ok(v.clone());
    };
    let r = resolve_variable(design.variables(), id, modes, limits, cancel)?;
    if r.value.resolved_type() != *resolved_type {
        return Err(E::TypeMismatch(id.clone()));
    }
    record_keys(reads, &r.chain);
    Ok(r.value)
}

/// Resolves every field of every node of the instance's component through the STU-DS-116 tiers.
/// `modes` selects a mode per variable collection (unselected collections use their default).
pub fn resolve_instance(
    inst: &StudioComponentInstance,
    design: &Design,
    modes: &BTreeMap<String, String>,
    base_revision: u64,
    limits: &Limits,
    cancel: &CancellationToken,
) -> R<Resolution> {
    check_id(&inst.instance_id, "SCIN")?;
    cancel.check().map_err(|_| E::Canceled)?;
    let (member, set) = select_member(inst, design)?;
    let nodes = design.definition_nodes(member);
    if nodes.len() > limits.max_nodes {
        return Err(E::BudgetExceeded);
    }
    // Property definitions: the member's win over the set's; every instance property must be known.
    let prop_defs = design.property_definitions(member, set)?;
    let mut given: BTreeMap<&str, (StudioValue, Footprint)> = BTreeMap::new();
    for (name, p) in &inst.component_properties {
        check_key(name)?;
        if !check_property(name, p, &prop_defs, set.is_some())? {
            continue; // VARIANT: it selected the member above
        }
        let mut reads = vec![(inst.instance_id.clone(), format!("property/{name}"))];
        let value = if let Some(vid) = property_variable(&inst.instance_id, name, p)? {
            let r = resolve_variable(design.variables(), vid, modes, limits, cancel)?;
            record_keys(&mut reads, &r.chain);
            r.value
        } else {
            literal(&p.value).ok_or_else(|| E::TypeMismatch(name.clone()))?
        };
        check_property_value(name, p.property_type, &value)?;
        given.insert(name.as_str(), (value, reads));
    }
    let overrides = build_overrides(inst, &nodes, &prop_defs, design.variables())?;
    let mut ov_by_node: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (n, f) in overrides.keys() {
        ov_by_node.entry(n).or_default().push(f);
    }

    let mut fields = Vec::new();
    for node in &nodes {
        cancel.check().map_err(|_| E::Canceled)?;
        let refs = node.design.map(|d| &d.component_property_references);
        let mut names: BTreeSet<&str> = BTreeSet::new();
        if let Some(l) = node.local {
            names.extend(l.fields.keys().map(String::as_str));
            names.extend(l.variable_bindings.keys().map(String::as_str));
        }
        names.extend(refs.into_iter().flat_map(|r| r.keys().map(String::as_str)));
        names.extend(ov_by_node.get(node.node_id).into_iter().flatten().copied());
        for f in names {
            if fields.len() >= limits.max_fields {
                return Err(E::BudgetExceeded);
            }
            let mut reads = vec![(member.component_id.clone(), String::new())];
            let (value, source) = if let Some(v) = overrides.get(&(node.node_id, f)) {
                (
                    materialize(v, design, modes, limits, cancel, &mut reads)?,
                    Source::Override,
                )
            } else if let Some(pname) = refs.and_then(|r| r.get(f)) {
                if let Some((v, r)) = given.get(pname.as_str()) {
                    reads.extend(r.iter().cloned());
                    (v.clone(), Source::ComponentProperty(pname.clone()))
                } else {
                    let d = prop_defs
                        .get(pname.as_str())
                        .ok_or_else(|| E::UnsupportedProperty(pname.clone()))?;
                    let value = match &d.default_value {
                        StudioPropertyValue::Alias(a) => {
                            let r =
                                resolve_variable(design.variables(), &a.id, modes, limits, cancel)?;
                            record_keys(&mut reads, &r.chain);
                            check_property_value(pname, d.property_type, &r.value)?;
                            r.value
                        }
                        lit => literal(lit).ok_or_else(|| E::TypeMismatch(pname.clone()))?,
                    };
                    (value, Source::PropertyDefault(pname.clone()))
                }
            } else if let Some(v) = node.local.and_then(|l| l.fields.get(f)) {
                (v.clone(), Source::Definition)
            } else if let Some(vid) = node.local.and_then(|l| l.variable_bindings.get(f)) {
                let r = resolve_variable(design.variables(), vid, modes, limits, cancel)?;
                record_keys(&mut reads, &r.chain);
                (
                    r.value,
                    Source::Variable {
                        variable_id: vid.clone(),
                        collection_id: r.collection_id,
                        mode_id: r.mode_id,
                    },
                )
            } else {
                return Err(E::UnsupportedProperty(f.to_owned()));
            };
            reads.sort();
            reads.dedup();
            fields.push(ResolvedField {
                node_id: node.node_id.to_owned(),
                field: f.to_owned(),
                value,
                source,
                reads,
            });
        }
    }
    Ok(Resolution {
        base_revision,
        component_id: member.component_id.clone(),
        fields,
    })
}

// ----------------------------------------------------------------------------------------------
// Swap and variant switch (STU-DS-119)

/// An override that did not survive a swap or variant switch; reported, never silently moved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DroppedOverride {
    pub node_id: String,
    pub field: String,
}

type NodePath = Vec<(String, usize)>;
/// Per node: the chain of `(layer name, ordinal among its siblings)` from the root.
fn node_paths<'a>(nodes: &[DefNode<'a>]) -> BTreeMap<&'a str, NodePath> {
    let mut paths: BTreeMap<&str, NodePath> = BTreeMap::new();
    let mut counts: BTreeMap<Option<&str>, usize> = BTreeMap::new();
    for n in nodes {
        let ord = counts.entry(n.parent).or_default();
        let mut path = n
            .parent
            .and_then(|p| paths.get(p))
            .cloned()
            .unwrap_or_default();
        path.push((n.name.to_owned(), *ord));
        *ord += 1;
        paths.insert(n.node_id, path);
    }
    paths
}

/// Re-matches override records from definition `from` to `to` by (layer name, position in the child
/// hierarchy). A record whose node has no counterpart is dropped and listed.
pub fn rematch_overrides(
    inst: &StudioComponentInstance,
    design: &Design,
    from: &StudioComponent,
    to: &StudioComponent,
) -> R<(Vec<StudioOverrideRecord>, Vec<DroppedOverride>)> {
    let from_paths = node_paths(&design.definition_nodes(from));
    let to_by_path: BTreeMap<NodePath, &str> = node_paths(&design.definition_nodes(to))
        .into_iter()
        .map(|(id, p)| (p, id))
        .collect();
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    for rec in &inst.overrides {
        let path = from_paths
            .get(rec.overridden_node_id.as_str())
            .ok_or_else(|| E::NoSuchNode(rec.overridden_node_id.clone()))?;
        match to_by_path.get(path) {
            Some(target) => kept.push(StudioOverrideRecord {
                overridden_node_id: (*target).to_owned(),
                ..rec.clone()
            }),
            None => dropped.extend(rec.overridden_fields.iter().map(|f| DroppedOverride {
                node_id: rec.overridden_node_id.clone(),
                field: f.clone(),
            })),
        }
    }
    Ok((kept, dropped))
}

/// `instance.swap_component`: points the instance at `new_main`, carrying matched overrides. The
/// result passes every check resolution makes on the instance's own data against the new
/// definition and the design's variables (properties, value types, variable bindings, alias
/// targets); see `check_shape` for what can still refuse at resolution.
pub fn swap_component(
    inst: &StudioComponentInstance,
    design: &Design,
    new_main: &str,
) -> R<(StudioComponentInstance, Vec<DroppedOverride>)> {
    let (from, _) = select_member(inst, design)?;
    let mut next = inst.clone();
    next.main_component_id = Some(new_main.to_owned());
    let (to, set) = select_member(&next, design)?;
    let (overrides, dropped) = rematch_overrides(inst, design, from, to)?;
    next.overrides = overrides;
    check_shape(&next, to, set, design)?;
    Ok((next, dropped))
}

/// Variant switch in place: sets the VARIANT property `property` to `value` and re-matches overrides.
/// The result passes the same instance checks against the new member as a swap (`check_shape`).
pub fn switch_variant(
    inst: &StudioComponentInstance,
    design: &Design,
    property: &str,
    value: &str,
) -> R<(StudioComponentInstance, Vec<DroppedOverride>)> {
    let (from, _) = select_member(inst, design)?;
    let mut next = inst.clone();
    next.component_properties.insert(
        property.to_owned(),
        StudioInstanceProperty {
            property_type: PropType::Variant,
            value: StudioPropertyValue::Text(value.to_owned()),
            bound_variable_id: None,
        },
    );
    // A plain (non-set) instance has no variant to switch: `check_shape` refuses the VARIANT property.
    let (to, set) = select_member(&next, design)?;
    let (overrides, dropped) = rematch_overrides(inst, design, from, to)?;
    next.overrides = overrides;
    check_shape(&next, to, set, design)?;
    Ok((next, dropped))
}

// ----------------------------------------------------------------------------------------------
// Override proposals and footprints (STU-ARC-011, STU-DS-163)

#[derive(Clone, Debug, PartialEq)]
pub struct OverrideEdit {
    pub node_id: String,
    pub field: String,
    pub value: StudioValue,
}
/// A successor instance value, produced only after the full instance resolves and the record set
/// with the successor validates in Folio. The caller's instance is untouched; acceptance (revision
/// check, grants, commit) stays with the host.
#[derive(Clone, Debug, PartialEq)]
pub struct OverrideProposal {
    pub base_revision: u64,
    pub instance: StudioComponentInstance,
    pub resolution: Resolution,
    /// Exactly `[(instance_id, "")]`: `instance` is the whole successor record, so accepting the
    /// proposal replaces the whole record and any concurrent change to it (a second proposal, a
    /// property edit) is lost unless it is detected. Detection holds when the host moves the record
    /// key `(instance_id, "")` on every accepted write into that record, and a writer of only a part
    /// of the record (a field-level key such as `(instance_id, "property/<name>")`) moves the record
    /// key as well. Two instances of one component write different records, so their keys stay
    /// disjoint.
    pub write_footprint: Vec<(String, String)>,
    /// Keys the edited fields' values depend on (definition record, variables, properties).
    pub read_footprint: Vec<(String, String)>,
}
impl OverrideProposal {
    /// Write footprint then read footprint, sorted and unique.
    pub fn footprint(&self) -> Vec<(String, String)> {
        let set: BTreeSet<_> = self
            .write_footprint
            .iter()
            .chain(&self.read_footprint)
            .cloned()
            .collect();
        set.into_iter().collect()
    }
    /// Admits the proposal against current per-key revisions: see [`check_footprint`].
    pub fn admit(
        &self,
        expected: &BTreeMap<(String, String), u64>,
        current: &BTreeMap<(String, String), u64>,
    ) -> R<()> {
        check_footprint(expected, current, &self.footprint())
    }
}

#[allow(clippy::too_many_arguments)]
pub fn propose_overrides(
    inst: &StudioComponentInstance,
    edits: &[OverrideEdit],
    design: &Design,
    modes: &BTreeMap<String, String>,
    base_revision: u64,
    limits: &Limits,
    cancel: &CancellationToken,
) -> R<OverrideProposal> {
    if edits.is_empty() {
        return Err(E::InvalidValue("edits".to_owned()));
    }
    let mut next = inst.clone();
    for e in edits {
        if !is_overridable(&e.field) {
            return Err(E::FieldNotOverridable {
                node_id: e.node_id.clone(),
                field: e.field.clone(),
            });
        }
        let at = match next
            .overrides
            .iter()
            .position(|r| r.overridden_node_id == e.node_id)
        {
            Some(i) => i,
            None => {
                next.overrides.push(StudioOverrideRecord {
                    overridden_node_id: e.node_id.clone(),
                    overridden_fields: Vec::new(),
                    values: BTreeMap::new(),
                });
                next.overrides.len() - 1
            }
        };
        let rec = &mut next.overrides[at];
        if !rec.overridden_fields.contains(&e.field) {
            rec.overridden_fields.push(e.field.clone());
        }
        rec.values.insert(e.field.clone(), e.value.clone());
    }
    let resolution = resolve_instance(&next, design, modes, base_revision, limits, cancel)?;
    design.validate_with_instance(&next, limits, cancel)?;
    let edited: BTreeSet<(&str, &str)> = edits
        .iter()
        .map(|e| (e.node_id.as_str(), e.field.as_str()))
        .collect();
    // The proposal carries the whole successor instance, so it writes the whole instance record
    // (PLR-046): a field-level write key would let two proposals that edit different fields of one
    // instance both be admitted, and the second to be accepted would replace the first one's edit.
    let write = (inst.instance_id.clone(), String::new());
    let mut read = BTreeSet::new();
    for f in &resolution.fields {
        if edited.contains(&(f.node_id.as_str(), f.field.as_str())) {
            read.extend(f.reads.iter().cloned());
        }
    }
    read.remove(&write);
    Ok(OverrideProposal {
        base_revision,
        instance: next,
        resolution,
        write_footprint: vec![write],
        read_footprint: read.into_iter().collect(),
    })
}

/// Same-key revision drift refuses; drift on keys outside the footprint is admissible.
/// Absent keys count as revision 0.
pub fn check_footprint(
    expected: &BTreeMap<(String, String), u64>,
    current: &BTreeMap<(String, String), u64>,
    footprint: &[(String, String)],
) -> R<()> {
    for k in footprint {
        if expected.get(k).copied().unwrap_or(0) != current.get(k).copied().unwrap_or(0) {
            return Err(E::StaleProperty {
                node_id: k.0.clone(),
                field: k.1.clone(),
            });
        }
    }
    Ok(())
}
