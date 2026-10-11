//! Component definitions, variant sets, instances with property-granular overrides, and variables
//! with collections, modes and alias chains (STU-DS-102..129), resolved into plain values with the
//! source of every field. Every input is borrowed; nothing is mutated, a base definition is never
//! rewritten by an instance, and a failure never yields a partial result.
//!
//! Decisions recorded for the lane (spec gaps, not source-derived):
//! * STU-DS-117 gives `overridden_node_id` + `overridden_fields[]` but no place for the VALUES. Each
//!   [`OverrideRecord`] carries a `values` map keyed by field name whose key set must equal
//!   `overridden_fields` exactly. Values stay on the instance record; the subtree is never copied.
//! * STU-DS-116 requires "exactly one source" per field. A definition field that has both a literal
//!   value and a variable binding, a field listed twice in overrides, or two override records for one
//!   node/field are the ambiguous double-source (`AmbiguousSource`). Local override > component
//!   property > definition literal > variable binding is the precedence among distinct tiers.
use crate::constraints::{NodeGeometry, SizePt};
use crate::{ComponentsError as E, R};
use hsk_studio_accord::{CancellationToken, DomainId};
use std::collections::{BTreeMap, BTreeSet};

/// STU-DS-115 closed overridable field set (corner radius family per STU-DS-130 node fields).
pub const OVERRIDABLE_FIELDS: [&str; 16] = [
    "characters",
    "fills",
    "strokes",
    "stroke_weight",
    "effects",
    "visible",
    "main_component_id",
    "component_properties",
    "opacity",
    "corner_radius",
    "top_left_radius",
    "top_right_radius",
    "bottom_left_radius",
    "bottom_right_radius",
    "layout_sizing_horizontal",
    "layout_sizing_vertical",
];
pub fn is_overridable(field: &str) -> bool {
    OVERRIDABLE_FIELDS.contains(&field)
}

const MAX_TEXT: usize = 65_536;
/// Footprint keys: `(node_id, field)`, `(record_id, "")`, `(instance_id, "property/<name>")`.
type Footprint = Vec<(String, String)>;

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

/// STU-DS-125 resolved variable type enumeration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum VarType {
    Boolean,
    Color,
    Easing,
    Float,
    String,
    Timing,
}

/// A literal value. Colour is straight RGBA channels (finite; range is not interpreted here).
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Boolean(bool),
    Color([f64; 4]),
    Easing(String),
    Float(f64),
    String(String),
    Timing(f64),
}
impl Value {
    pub fn var_type(&self) -> VarType {
        match self {
            Self::Boolean(_) => VarType::Boolean,
            Self::Color(_) => VarType::Color,
            Self::Easing(_) => VarType::Easing,
            Self::Float(_) => VarType::Float,
            Self::String(_) => VarType::String,
            Self::Timing(_) => VarType::Timing,
        }
    }
    fn validate(&self, target: &str) -> R<()> {
        let ok = match self {
            Self::Boolean(_) => true,
            Self::Color(c) => c.iter().all(|v| v.is_finite()),
            Self::Float(v) => v.is_finite(),
            Self::Timing(v) => v.is_finite() && *v >= 0.0,
            Self::Easing(s) | Self::String(s) => s.len() <= MAX_TEXT,
        };
        if ok {
            Ok(())
        } else {
            Err(E::InvalidValue(target.to_owned()))
        }
    }
}

// ----------------------------------------------------------------------------------------------
// Variables

/// A stored per-mode value: a literal, or an alias to another variable (STU-DS-126).
#[derive(Clone, Debug, PartialEq)]
pub enum VarEntry {
    Literal(Value),
    Alias(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mode {
    pub mode_id: String,
    pub name: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collection {
    pub collection_id: String,
    pub modes: Vec<Mode>,
    pub default_mode_id: String,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Variable {
    pub variable_id: String,
    pub collection_id: String,
    pub resolved_type: VarType,
    /// Exactly one entry per mode of the owning collection (STU-DS-121); checked by
    /// [`VarRegistry::validate`] and at every read.
    pub values_by_mode: BTreeMap<String, VarEntry>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct VarRegistry {
    collections: BTreeMap<String, Collection>,
    variables: BTreeMap<String, Variable>,
}
impl VarRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn add_collection(&mut self, c: Collection) -> R<()> {
        check_id(&c.collection_id, "SVCL")?;
        if c.modes.is_empty() {
            return Err(E::InvalidValue(c.collection_id));
        }
        let mut seen = BTreeSet::new();
        for m in &c.modes {
            check_key(&m.mode_id)?;
            if !seen.insert(m.mode_id.as_str()) {
                return Err(E::Duplicate(m.mode_id.clone()));
            }
        }
        if !seen.contains(c.default_mode_id.as_str()) {
            return Err(E::NoSuchMode {
                collection_id: c.collection_id,
                mode_id: c.default_mode_id,
            });
        }
        if self.collections.contains_key(&c.collection_id) {
            return Err(E::Duplicate(c.collection_id));
        }
        self.collections.insert(c.collection_id.clone(), c);
        Ok(())
    }
    pub fn add_variable(&mut self, v: Variable) -> R<()> {
        check_id(&v.variable_id, "SVAR")?;
        check_id(&v.collection_id, "SVCL")?;
        for (mode, entry) in &v.values_by_mode {
            check_key(mode)?;
            match entry {
                VarEntry::Literal(val) => {
                    val.validate(&v.variable_id)?;
                    if val.var_type() != v.resolved_type {
                        return Err(E::TypeMismatch(v.variable_id.clone()));
                    }
                }
                VarEntry::Alias(target) => check_id(target, "SVAR")?,
            }
        }
        if self.variables.contains_key(&v.variable_id) {
            return Err(E::Duplicate(v.variable_id));
        }
        self.variables.insert(v.variable_id.clone(), v);
        Ok(())
    }
    /// Cross-record check: every variable's collection exists and its `values_by_mode` key set is
    /// exactly the collection's mode set.
    pub fn validate(&self) -> R<()> {
        for v in self.variables.values() {
            let c = self
                .collections
                .get(&v.collection_id)
                .ok_or_else(|| E::NoSuchCollection(v.collection_id.clone()))?;
            for m in &c.modes {
                if !v.values_by_mode.contains_key(&m.mode_id) {
                    return Err(E::MissingModeValue {
                        variable_id: v.variable_id.clone(),
                        mode_id: m.mode_id.clone(),
                    });
                }
            }
            for k in v.values_by_mode.keys() {
                if !c.modes.iter().any(|m| &m.mode_id == k) {
                    return Err(E::NoSuchMode {
                        collection_id: c.collection_id.clone(),
                        mode_id: k.clone(),
                    });
                }
            }
        }
        Ok(())
    }
    pub fn variable(&self, id: &str) -> Option<&Variable> {
        self.variables.get(id)
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
    pub value: Value,
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
    vars: &VarRegistry,
    variable_id: &str,
    modes: &BTreeMap<String, String>,
    limits: &Limits,
    cancel: &CancellationToken,
) -> R<ResolvedVariable> {
    let mut cur = vars
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
            .get(&cur.collection_id)
            .ok_or_else(|| E::NoSuchCollection(cur.collection_id.clone()))?;
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
            Some(VarEntry::Literal(v)) => {
                if v.var_type() != wanted {
                    return Err(E::TypeMismatch(cur.variable_id.clone()));
                }
                let (collection_id, mode_id) = first.unwrap_or_default();
                return Ok(ResolvedVariable {
                    value: v.clone(),
                    collection_id,
                    mode_id,
                    chain,
                });
            }
            Some(VarEntry::Alias(target)) => {
                let next = vars
                    .variables
                    .get(target)
                    .ok_or_else(|| E::NoSuchVariable(target.clone()))?;
                if next.resolved_type != wanted {
                    return Err(E::TypeMismatch(target.clone()));
                }
                cur = next;
            }
        }
    }
}

// ----------------------------------------------------------------------------------------------
// Definitions

/// STU-DS-110 component-property types. `Slot` is recognised and refused in this slice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropType {
    Boolean,
    Text,
    InstanceSwap,
    Variant,
    Slot,
}
fn prop_value_ok(ty: PropType, v: &Value) -> bool {
    match ty {
        PropType::Boolean => matches!(v, Value::Boolean(_)),
        PropType::Text | PropType::InstanceSwap | PropType::Variant => {
            matches!(v, Value::String(_))
        }
        PropType::Slot => false,
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct PropertyDef {
    pub name: String,
    pub ty: PropType,
    pub default_value: Value,
}

/// A layer inside a component definition, in hierarchy order (parents before children).
#[derive(Clone, Debug, PartialEq)]
pub struct DefNode {
    pub node_id: String,
    pub name: String,
    pub parent: Option<String>,
    /// Literal field values (tier 3).
    pub fields: BTreeMap<String, Value>,
    /// STU-DS-113: field name -> component property name that drives it (tier 2).
    pub property_references: BTreeMap<String, String>,
    /// Field name -> variable id bound on the definition (tier 4).
    pub variable_bindings: BTreeMap<String, String>,
    pub geometry: Option<NodeGeometry>,
}
impl DefNode {
    pub fn new(node_id: &str, name: &str, parent: Option<&str>) -> Self {
        Self {
            node_id: node_id.to_owned(),
            name: name.to_owned(),
            parent: parent.map(str::to_owned),
            fields: BTreeMap::new(),
            property_references: BTreeMap::new(),
            variable_bindings: BTreeMap::new(),
            geometry: None,
        }
    }
}
/// One `component` node (STU-DS-103).
#[derive(Clone, Debug, PartialEq)]
pub struct ComponentDef {
    pub component_id: String,
    /// `Some` only for a variant-set member: this member's property=value assignment.
    pub variant_properties: Option<BTreeMap<String, String>>,
    pub property_definitions: Vec<PropertyDef>,
    pub nodes: Vec<DefNode>,
    /// Authored size in points; required when any node carries geometry.
    pub size: Option<SizePt>,
}
/// One `component_set` node (STU-DS-104).
#[derive(Clone, Debug, PartialEq)]
pub struct ComponentSet {
    pub component_id: String,
    pub default_variant_id: String,
    /// Ordered declared values per variant property (order is authority, STU-DS-108).
    pub variant_group_properties: BTreeMap<String, Vec<String>>,
    pub property_definitions: Vec<PropertyDef>,
    pub members: Vec<ComponentDef>,
}

fn validate_props(defs: &[PropertyDef]) -> R<()> {
    let mut names = BTreeSet::new();
    for d in defs {
        check_key(&d.name)?;
        if !names.insert(d.name.as_str()) {
            return Err(E::Duplicate(d.name.clone()));
        }
        if d.ty == PropType::Slot {
            return Err(E::UnsupportedProperty(d.name.clone()));
        }
        d.default_value.validate(&d.name)?;
        if !prop_value_ok(d.ty, &d.default_value) {
            return Err(E::TypeMismatch(d.name.clone()));
        }
    }
    Ok(())
}
fn validate_def(def: &ComponentDef, member: bool) -> R<()> {
    check_id(&def.component_id, "SCMP")?;
    if def.variant_properties.is_some() != member {
        return Err(E::InvalidValue(def.component_id.clone()));
    }
    validate_props(&def.property_definitions)?;
    if let Some(s) = def.size
        && (!s.w.is_finite() || !s.h.is_finite() || s.w < 0.0 || s.h < 0.0)
    {
        return Err(E::InvalidGeometry);
    }
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for n in &def.nodes {
        check_id(&n.node_id, "SLYR")?;
        if let Some(p) = &n.parent
            && !seen.contains(p.as_str())
        {
            return Err(E::NoSuchNode(p.clone()));
        }
        if !seen.insert(n.node_id.as_str()) {
            return Err(E::Duplicate(n.node_id.clone()));
        }
        for (f, v) in &n.fields {
            check_key(f)?;
            v.validate(&n.node_id)?;
        }
        for (f, p) in &n.property_references {
            check_key(f)?;
            check_key(p)?;
        }
        for (f, v) in &n.variable_bindings {
            check_key(f)?;
            check_id(v, "SVAR")?;
            if n.fields.contains_key(f) {
                return Err(E::AmbiguousSource {
                    node_id: n.node_id.clone(),
                    field: f.clone(),
                });
            }
        }
        if n.geometry.is_some() && def.size.is_none() {
            return Err(E::InvalidGeometry);
        }
    }
    Ok(())
}
fn validate_set(s: &ComponentSet) -> R<()> {
    check_id(&s.component_id, "SCMP")?;
    validate_props(&s.property_definitions)?;
    if s.members.len() < 2 || !s.members.iter().any(|m| m.component_id == s.default_variant_id) {
        return Err(E::NoSuchVariant);
    }
    for (name, values) in &s.variant_group_properties {
        check_key(name)?;
        let mut u = BTreeSet::new();
        for v in values {
            check_key(v)?;
            if !u.insert(v.as_str()) {
                return Err(E::Duplicate(v.clone()));
            }
        }
        if values.is_empty() {
            return Err(E::InvalidValue(name.clone()));
        }
    }
    let mut assignments: BTreeSet<&BTreeMap<String, String>> = BTreeSet::new();
    // One id names one definition: `component()` finds a member by id, so two members (or a member
    // and the set) sharing an id would let a variant selection pick the one the lookup hides.
    let mut ids: BTreeSet<&str> = BTreeSet::from([s.component_id.as_str()]);
    for m in &s.members {
        validate_def(m, true)?;
        if !ids.insert(m.component_id.as_str()) {
            return Err(E::Duplicate(m.component_id.clone()));
        }
        let a = m.variant_properties.as_ref().ok_or(E::NoSuchVariant)?;
        for (prop, declared) in &s.variant_group_properties {
            match a.get(prop) {
                None => return Err(E::VariantAssignmentIncomplete(m.component_id.clone())),
                Some(v) if !declared.contains(v) => {
                    return Err(E::UnknownVariantValue {
                        property: prop.clone(),
                        value: v.clone(),
                    });
                }
                Some(_) => {}
            }
        }
        if let Some(extra) = a.keys().find(|k| !s.variant_group_properties.contains_key(*k)) {
            return Err(E::UnknownVariantValue {
                property: extra.clone(),
                value: a[extra].clone(),
            });
        }
        if !assignments.insert(a) {
            return Err(E::VariantAssignmentDuplicate(m.component_id.clone()));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DefRegistry {
    components: BTreeMap<String, ComponentDef>,
    sets: BTreeMap<String, ComponentSet>,
    /// member component id -> set id
    members: BTreeMap<String, String>,
}
impl DefRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    fn taken(&self, id: &str) -> bool {
        self.components.contains_key(id) || self.sets.contains_key(id) || self.members.contains_key(id)
    }
    pub fn add_component(&mut self, c: ComponentDef) -> R<()> {
        validate_def(&c, false)?;
        if self.taken(&c.component_id) {
            return Err(E::Duplicate(c.component_id));
        }
        self.components.insert(c.component_id.clone(), c);
        Ok(())
    }
    pub fn add_set(&mut self, s: ComponentSet) -> R<()> {
        validate_set(&s)?;
        if self.taken(&s.component_id) || s.members.iter().any(|m| self.taken(&m.component_id)) {
            return Err(E::Duplicate(s.component_id));
        }
        for m in &s.members {
            self.members.insert(m.component_id.clone(), s.component_id.clone());
        }
        self.sets.insert(s.component_id.clone(), s);
        Ok(())
    }
    /// The component definition `id` names: a plain component, a set's default member, or a member.
    pub fn component(&self, id: &str) -> Option<&ComponentDef> {
        if let Some(c) = self.components.get(id) {
            return Some(c);
        }
        let set = self.sets.get(id).or_else(|| self.sets.get(self.members.get(id)?))?;
        let want = if set.component_id == id { &set.default_variant_id } else { id };
        set.members.iter().find(|m| m.component_id == *want)
    }
}

// ----------------------------------------------------------------------------------------------
// Instances

#[derive(Clone, Debug, PartialEq)]
pub struct InstanceProp {
    pub ty: PropType,
    pub value: Value,
    /// When set the value comes from this variable (alias to a variable of the matching type).
    pub bound_variable_id: Option<String>,
}
/// STU-DS-117 record: node inside the instance, fields drawn from STU-DS-115, plus the values
/// (crate decision, see module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct OverrideRecord {
    pub overridden_node_id: String,
    pub overridden_fields: Vec<String>,
    pub values: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Instance {
    pub instance_id: String,
    /// `None` is the deleted-main state (STU-DS-003a): resolution refuses it.
    pub main_component_id: Option<String>,
    pub component_properties: BTreeMap<String, InstanceProp>,
    pub overrides: Vec<OverrideRecord>,
}

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
    pub value: Value,
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
    inst: &Instance,
    defs: &'a DefRegistry,
) -> R<(&'a ComponentDef, Option<&'a ComponentSet>)> {
    let main = inst
        .main_component_id
        .as_deref()
        .ok_or_else(|| E::NoSuchComponent("deleted-main".to_owned()))?;
    let start = defs
        .component(main)
        .ok_or_else(|| E::NoSuchComponent(main.to_owned()))?;
    let set = defs
        .sets
        .get(main)
        .or_else(|| defs.members.get(main).and_then(|s| defs.sets.get(s)));
    let Some(set) = set else {
        return Ok((start, None));
    };
    let mut want = start.variant_properties.clone().unwrap_or_default();
    for (name, p) in &inst.component_properties {
        if p.ty != PropType::Variant {
            continue;
        }
        let declared = set
            .variant_group_properties
            .get(name)
            .ok_or_else(|| E::UnsupportedProperty(name.clone()))?;
        let Value::String(v) = &p.value else {
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
    let m = set
        .members
        .iter()
        .find(|m| m.variant_properties.as_ref() == Some(&want))
        .ok_or(E::NoSuchVariant)?;
    Ok((m, Some(set)))
}

type Overrides<'a> = BTreeMap<(&'a str, &'a str), &'a Value>;
type PropDefs<'a> = BTreeMap<&'a str, &'a PropertyDef>;

/// Property definitions in force for a member: the member's win over its set's.
fn prop_defs_of<'a>(member: &'a ComponentDef, set: Option<&'a ComponentSet>) -> PropDefs<'a> {
    let mut defs: PropDefs = BTreeMap::new();
    for d in set.iter().flat_map(|s| s.property_definitions.iter()) {
        defs.insert(d.name.as_str(), d);
    }
    for d in &member.property_definitions {
        defs.insert(d.name.as_str(), d);
    }
    defs
}

/// Checks an instance property against its definition. `true`: it carries a value to use;
/// `false`: a VARIANT property, which only selects the member and is legal only on a set.
fn check_property(name: &str, p: &InstanceProp, defs: &PropDefs, in_set: bool) -> R<bool> {
    if p.ty == PropType::Variant {
        return if in_set { Ok(false) } else { Err(E::UnsupportedProperty(name.to_owned())) };
    }
    let def = defs.get(name).ok_or_else(|| E::UnsupportedProperty(name.to_owned()))?;
    if p.ty != def.ty {
        return Err(E::TypeMismatch(name.to_owned()));
    }
    Ok(true)
}

fn check_property_value(name: &str, ty: PropType, value: &Value) -> R<()> {
    value.validate(name)?;
    if !prop_value_ok(ty, value) {
        return Err(E::TypeMismatch(name.to_owned()));
    }
    Ok(())
}

/// Every override must fit the node it names and the type of each source that can feed the field:
/// a definition literal, the referenced component property, the bound variable. `vars` is `None`
/// where no registry is at hand (swap and variant switch), which skips only the variable check.
fn build_overrides<'a>(
    inst: &'a Instance,
    member: &'a ComponentDef,
    prop_defs: &PropDefs,
    vars: Option<&VarRegistry>,
) -> R<Overrides<'a>> {
    let nodes: BTreeMap<&str, &DefNode> =
        member.nodes.iter().map(|n| (n.node_id.as_str(), n)).collect();
    let mut out: Overrides = BTreeMap::new();
    for rec in &inst.overrides {
        let node = nodes
            .get(rec.overridden_node_id.as_str())
            .ok_or_else(|| E::NoSuchNode(rec.overridden_node_id.clone()))?;
        for f in &rec.overridden_fields {
            if !is_overridable(f) {
                return Err(E::FieldNotOverridable {
                    node_id: rec.overridden_node_id.clone(),
                    field: f.clone(),
                });
            }
            let v = rec.values.get(f).ok_or_else(|| E::InvalidValue(f.clone()))?;
            v.validate(f)?;
            let literal_clash = node.fields.get(f).is_some_and(|lit| lit.var_type() != v.var_type());
            let property_clash = node
                .property_references
                .get(f)
                .and_then(|pname| prop_defs.get(pname.as_str()))
                .is_some_and(|def| !prop_value_ok(def.ty, v));
            let variable_clash = vars
                .zip(node.variable_bindings.get(f))
                .and_then(|(registry, vid)| registry.variable(vid))
                .is_some_and(|var| var.resolved_type != v.var_type());
            if literal_clash || property_clash || variable_clash {
                return Err(E::TypeMismatch(f.clone()));
            }
            if out.insert((node.node_id.as_str(), f.as_str()), v).is_some() {
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

/// What `resolve_instance` can decide without variables or modes: every property is defined by the
/// member (or its set) with the right type and value type, and every override fits its node.
/// Swap and variant switch run it so they never return an instance that cannot resolve.
fn check_shape(inst: &Instance, member: &ComponentDef, set: Option<&ComponentSet>) -> R<()> {
    let prop_defs = prop_defs_of(member, set);
    for (name, p) in &inst.component_properties {
        check_key(name)?;
        if check_property(name, p, &prop_defs, set.is_some())? && p.bound_variable_id.is_none() {
            check_property_value(name, p.ty, &p.value)?;
        }
    }
    build_overrides(inst, member, &prop_defs, None)?;
    Ok(())
}

fn record_keys(out: &mut Vec<(String, String)>, ids: &[String]) {
    out.extend(ids.iter().map(|v| (v.clone(), String::new())));
}

/// Resolves every field of every node of the instance's component through the STU-DS-116 tiers.
/// `modes` selects a mode per variable collection (unselected collections use their default).
pub fn resolve_instance(
    inst: &Instance,
    defs: &DefRegistry,
    vars: &VarRegistry,
    modes: &BTreeMap<String, String>,
    base_revision: u64,
    limits: &Limits,
    cancel: &CancellationToken,
) -> R<Resolution> {
    check_id(&inst.instance_id, "SCIN")?;
    cancel.check().map_err(|_| E::Canceled)?;
    let (member, set) = select_member(inst, defs)?;
    if member.nodes.len() > limits.max_nodes {
        return Err(E::BudgetExceeded);
    }
    // Property definitions: the member's win over the set's; every instance property must be known.
    let prop_defs = prop_defs_of(member, set);
    let mut given: BTreeMap<&str, (Value, Footprint)> = BTreeMap::new();
    for (name, p) in &inst.component_properties {
        check_key(name)?;
        if !check_property(name, p, &prop_defs, set.is_some())? {
            continue; // VARIANT: it selected the member above
        }
        let mut reads = vec![(inst.instance_id.clone(), format!("property/{name}"))];
        let value = if let Some(vid) = &p.bound_variable_id {
            let r = resolve_variable(vars, vid, modes, limits, cancel)?;
            record_keys(&mut reads, &r.chain);
            r.value
        } else {
            p.value.clone()
        };
        check_property_value(name, p.ty, &value)?;
        given.insert(name.as_str(), (value, reads));
    }
    let overrides = build_overrides(inst, member, &prop_defs, Some(vars))?;
    let mut ov_by_node: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (n, f) in overrides.keys() {
        ov_by_node.entry(n).or_default().push(f);
    }

    let mut fields = Vec::new();
    for node in &member.nodes {
        cancel.check().map_err(|_| E::Canceled)?;
        let mut names: BTreeSet<&str> = BTreeSet::new();
        names.extend(node.fields.keys().map(String::as_str));
        names.extend(node.property_references.keys().map(String::as_str));
        names.extend(node.variable_bindings.keys().map(String::as_str));
        names.extend(ov_by_node.get(node.node_id.as_str()).into_iter().flatten().copied());
        for f in names {
            if fields.len() >= limits.max_fields {
                return Err(E::BudgetExceeded);
            }
            let mut reads = vec![(member.component_id.clone(), String::new())];
            let (value, source) = if let Some(v) = overrides.get(&(node.node_id.as_str(), f)) {
                ((*v).clone(), Source::Override)
            } else if let Some(pname) = node.property_references.get(f) {
                if let Some((v, r)) = given.get(pname.as_str()) {
                    reads.extend(r.iter().cloned());
                    (v.clone(), Source::ComponentProperty(pname.clone()))
                } else {
                    let d = prop_defs
                        .get(pname.as_str())
                        .ok_or_else(|| E::UnsupportedProperty(pname.clone()))?;
                    (d.default_value.clone(), Source::PropertyDefault(pname.clone()))
                }
            } else if let Some(v) = node.fields.get(f) {
                (v.clone(), Source::Definition)
            } else if let Some(vid) = node.variable_bindings.get(f) {
                let r = resolve_variable(vars, vid, modes, limits, cancel)?;
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
                node_id: node.node_id.clone(),
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
fn node_paths(def: &ComponentDef) -> BTreeMap<&str, NodePath> {
    let mut paths: BTreeMap<&str, NodePath> = BTreeMap::new();
    let mut counts: BTreeMap<Option<&str>, usize> = BTreeMap::new();
    for n in &def.nodes {
        let parent = n.parent.as_deref();
        let ord = counts.entry(parent).or_default();
        let mut path = parent.and_then(|p| paths.get(p)).cloned().unwrap_or_default();
        path.push((n.name.clone(), *ord));
        *ord += 1;
        paths.insert(n.node_id.as_str(), path);
    }
    paths
}

/// Re-matches override records from definition `from` to `to` by (layer name, position in the child
/// hierarchy). A record whose node has no counterpart is dropped and listed.
pub fn rematch_overrides(
    inst: &Instance,
    from: &ComponentDef,
    to: &ComponentDef,
) -> R<(Vec<OverrideRecord>, Vec<DroppedOverride>)> {
    let from_paths = node_paths(from);
    let to_by_path: BTreeMap<NodePath, &str> = node_paths(to)
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
            Some(target) => kept.push(OverrideRecord {
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
/// result is checked against the new definition (properties, value types), never returned unresolvable.
pub fn swap_component(
    inst: &Instance,
    defs: &DefRegistry,
    new_main: &str,
) -> R<(Instance, Vec<DroppedOverride>)> {
    let (from, _) = select_member(inst, defs)?;
    let mut next = inst.clone();
    next.main_component_id = Some(new_main.to_owned());
    let (to, set) = select_member(&next, defs)?;
    let (overrides, dropped) = rematch_overrides(inst, from, to)?;
    next.overrides = overrides;
    check_shape(&next, to, set)?;
    Ok((next, dropped))
}

/// Variant switch in place: sets the VARIANT property `property` to `value` and re-matches overrides.
/// The result is checked against the new member, so `Ok` is never an instance that cannot resolve.
pub fn switch_variant(
    inst: &Instance,
    defs: &DefRegistry,
    property: &str,
    value: &str,
) -> R<(Instance, Vec<DroppedOverride>)> {
    let (from, _) = select_member(inst, defs)?;
    let mut next = inst.clone();
    next.component_properties.insert(
        property.to_owned(),
        InstanceProp {
            ty: PropType::Variant,
            value: Value::String(value.to_owned()),
            bound_variable_id: None,
        },
    );
    // A plain (non-set) instance has no variant to switch: `check_shape` refuses the VARIANT property.
    let (to, set) = select_member(&next, defs)?;
    let (overrides, dropped) = rematch_overrides(inst, from, to)?;
    next.overrides = overrides;
    check_shape(&next, to, set)?;
    Ok((next, dropped))
}

// ----------------------------------------------------------------------------------------------
// Override proposals and footprints (STU-ARC-011, STU-DS-163)

#[derive(Clone, Debug, PartialEq)]
pub struct OverrideEdit {
    pub node_id: String,
    pub field: String,
    pub value: Value,
}
/// A successor instance value, produced only after the full instance resolves. The caller's
/// instance is untouched; acceptance (revision check, grants, commit) stays with the host.
#[derive(Clone, Debug, PartialEq)]
pub struct OverrideProposal {
    pub base_revision: u64,
    pub instance: Instance,
    pub resolution: Resolution,
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
    inst: &Instance,
    edits: &[OverrideEdit],
    defs: &DefRegistry,
    vars: &VarRegistry,
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
                next.overrides.push(OverrideRecord {
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
    let resolution = resolve_instance(&next, defs, vars, modes, base_revision, limits, cancel)?;
    let write: BTreeSet<(String, String)> = edits
        .iter()
        .map(|e| (e.node_id.clone(), e.field.clone()))
        .collect();
    let mut read = BTreeSet::new();
    for f in &resolution.fields {
        if write.contains(&(f.node_id.clone(), f.field.clone())) {
            read.extend(f.reads.iter().cloned());
        }
    }
    read.retain(|k| !write.contains(k));
    Ok(OverrideProposal {
        base_revision,
        instance: next,
        resolution,
        write_footprint: write.into_iter().collect(),
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
