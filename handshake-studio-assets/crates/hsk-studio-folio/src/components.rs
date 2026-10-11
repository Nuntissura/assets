//! Component, component-set, instance, variable and variable-collection records (spec STU-DS-102..105,
//! STU-DS-108..111, STU-DS-113..117, STU-DS-121..122, STU-DS-125..129, STU-DS-141). The record shapes are
//! the spec's stored-record contracts; a definition or instance is a node of the document graph, so a
//! component record names its container layer (`layer_id`), and a `component_instance` layer names its
//! `SCIN` record through the existing `PrimitiveReference` payload. Cross-record rules (variant
//! completeness, alias cycles, mode coverage, references) are checked by `validate_design_records`.
//! Folio stores and validates only; resolving an instance, overrides and variables is the components
//! crate's job.
//!
//! Declared gaps, not invented here: STU-DS-117 gives an override record no place for the value, so
//! `values` (field name to variable value record, keys within `overridden_fields`) is an optional
//! extension and `fills`/`strokes`/`effects` overrides cannot yet carry a value; node field values of a
//! definition (fills, characters, corner radius and the rest) have no Folio home and are not stored;
//! `exposed_instance_ids` is a read-only projection of `is_exposed_instance` (STU-DS-118) and is not
//! stored; extended collections (`is_extension`, parent/root ids, `variable_overrides`, STU-DS-124)
//! are recognised and refused as `UnsupportedDescriptor`; `EXPRESSION` values and expression cycles
//! are not modelled; whether a scope is legal for a resolved type is not stated in STU-DS-128 and is
//! not enforced; the `VARIANT` property option list is the set's `variant_group_properties` entry and
//! is not duplicated; the `description`/`description_markdown`/`documentation_links` members that
//! STU-DS-141 gives every publishable entity are carried by `StudioPublishing` for collections and
//! variables although STU-DS-121/122 do not list them; the reference spelling of
//! `component_property_references` is only exemplified by STU-DS-113 and is not parsed.
use super::{
    Code, Result, err, id,
    values::{Ctx, MAX_TEXT_BYTES, StudioPublishing, StudioValue, VariableType, label, slash_path},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

schema_token!(ComponentSchema, "hsk.studio.component@1");
schema_token!(ComponentInstanceSchema, "hsk.studio.component_instance@1");
schema_token!(VariableSchema, "hsk.studio.variable@1");
schema_token!(VariableCollectionSchema, "hsk.studio.variable_collection@1");

/// STU-DS-115 closed set of fields an instance may override.
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
/// STU-DS-128 closed variable scope set.
pub const VARIABLE_SCOPES: [&str; 22] = [
    "ALL_SCOPES",
    "ALL_FILLS",
    "FRAME_FILL",
    "SHAPE_FILL",
    "TEXT_FILL",
    "STROKE_COLOR",
    "STROKE_FLOAT",
    "EFFECT_COLOR",
    "EFFECT_FLOAT",
    "CORNER_RADIUS",
    "GAP",
    "WIDTH_HEIGHT",
    "OPACITY",
    "FONT_FAMILY",
    "FONT_SIZE",
    "FONT_STYLE",
    "FONT_WEIGHT",
    "LETTER_SPACING",
    "LINE_HEIGHT",
    "PARAGRAPH_INDENT",
    "PARAGRAPH_SPACING",
    "TEXT_CONTENT",
];
/// STU-DS-129 closed code-syntax platform set.
pub const CODE_SYNTAX_PLATFORMS: [&str; 3] = ["WEB", "ANDROID", "iOS"];
/// Folio hard ceilings; the spec declares none (STU-DS-123 declares no mode ceiling).
pub const MAX_PROPERTIES: usize = 256;
pub const MAX_MODES: usize = 1024;
pub const MAX_OVERRIDES: usize = 4096;

/// STU-DS-110 component-property type (closed, five members).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ComponentPropertyType {
    Boolean,
    Text,
    InstanceSwap,
    Variant,
    Slot,
}

/// The `type` member of an alias record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum AliasKind {
    #[serde(rename = "VARIABLE_ALIAS")]
    VariableAlias,
}

/// `{type: "VARIABLE_ALIAS", id}`: a property value that is bound to a variable (STU-DS-110/111).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioAliasRef {
    #[serde(rename = "type")]
    pub alias_type: AliasKind,
    /// `SVAR-{uuid_v7}`.
    pub id: String,
}

/// A component-property default or instance value: boolean, string, or a variable alias.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum StudioPropertyValue {
    Boolean(bool),
    Text(String),
    Alias(StudioAliasRef),
}

/// STU-DS-110 `preferred_values` entry kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PreferredKind {
    Component,
    ComponentSet,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioPreferredValue {
    #[serde(rename = "type")]
    pub preferred_type: PreferredKind,
    /// Publish key of the preferred component or set.
    pub key: String,
}

/// Options record of an `INSTANCE_SWAP` property.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioPropertyOptions {
    pub preferred_values: Vec<StudioPreferredValue>,
}

/// STU-DS-111 component-property definition. `options` is present exactly for `INSTANCE_SWAP`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioComponentProperty {
    pub property_name: String,
    #[serde(rename = "type")]
    pub property_type: ComponentPropertyType,
    pub default_value: StudioPropertyValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<StudioPropertyOptions>,
}

/// A `component` node record (STU-DS-103). `variant_properties` is explicit null unless the
/// component is a member of a set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioComponent {
    pub schema_id: ComponentSchema,
    /// `SCMP-{uuid_v7}`.
    pub component_id: String,
    /// The `group` layer that holds the definition's children.
    pub layer_id: String,
    pub publishing: StudioPublishing,
    #[serde(deserialize_with = "super::required")]
    #[schemars(required)]
    pub variant_properties: Option<BTreeMap<String, String>>,
    pub component_property_definitions: Vec<StudioComponentProperty>,
}

/// A `component_set` node record (STU-DS-104); its members are the components whose container layer
/// is a child of `layer_id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioComponentSet {
    pub schema_id: ComponentSchema,
    pub component_id: String,
    pub layer_id: String,
    pub publishing: StudioPublishing,
    /// The member returned when the set is instantiated.
    pub default_variant_id: String,
    /// Variant-property name to its ordered declared values; order is authority (STU-DS-108).
    pub variant_group_properties: BTreeMap<String, Vec<String>>,
    pub component_property_definitions: Vec<StudioComponentProperty>,
}

/// One entry of `component_properties` on an instance (STU-DS-105).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioInstanceProperty {
    #[serde(rename = "type")]
    pub property_type: ComponentPropertyType,
    pub value: StudioPropertyValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_variable_id: Option<String>,
}

/// STU-DS-117 override record. `values` is the optional value extension named in the module note.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioOverrideRecord {
    /// The node inside the instance (a layer of the definition).
    pub overridden_node_id: String,
    pub overridden_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub values: BTreeMap<String, StudioValue>,
}

/// A `component_instance` node record (STU-DS-105).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioComponentInstance {
    pub schema_id: ComponentInstanceSchema,
    /// `SCIN-{uuid_v7}`.
    pub instance_id: String,
    /// `SCMP-*` of a component, or explicit null in the deleted-main state.
    #[serde(deserialize_with = "super::required")]
    #[schemars(required)]
    pub main_component_id: Option<String>,
    pub component_properties: BTreeMap<String, StudioInstanceProperty>,
    pub overrides: Vec<StudioOverrideRecord>,
    /// STU-DS-106: `0.01..=100` (the maximum is Studio-declared).
    #[schemars(range(min = 0.01, max = 100.0))]
    pub scale_factor: f64,
    pub is_exposed_instance: bool,
}

/// STU-DS-122 mode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioMode {
    pub mode_id: String,
    pub name: String,
}

/// STU-DS-122 variable collection. Extended collections are not modelled (`is_extension` must be
/// false).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioVariableCollection {
    pub schema_id: VariableCollectionSchema,
    /// `SVCL-{uuid_v7}`.
    pub collection_id: String,
    /// Slash-path name.
    pub name: String,
    pub hidden_from_publishing: bool,
    pub publishing: StudioPublishing,
    pub is_extension: bool,
    /// Ordered, at least one.
    pub modes: Vec<StudioMode>,
    pub default_mode_id: String,
    /// The variables of the collection (`SVAR` ids); equals the set of variables that name it.
    pub variable_ids: Vec<String>,
}

/// STU-DS-121 variable. `values_by_mode` has exactly one entry per mode of the owning collection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioVariable {
    pub schema_id: VariableSchema,
    /// `SVAR-{uuid_v7}`.
    pub variable_id: String,
    /// Slash-path name.
    pub name: String,
    pub hidden_from_publishing: bool,
    pub publishing: StudioPublishing,
    /// Owning `SVCL` collection.
    pub variable_collection_id: String,
    pub resolved_type: VariableType,
    pub values_by_mode: BTreeMap<String, StudioValue>,
    /// STU-DS-128 scope tokens.
    pub scopes: Vec<String>,
    /// Platform (`WEB`, `ANDROID`, `iOS`) to code name.
    pub code_syntax: BTreeMap<String, String>,
}

fn property_value(
    t: ComponentPropertyType,
    v: &StudioPropertyValue,
    target: &str,
    c: &mut Ctx,
) -> Result<()> {
    let bad = || err(Code::InvalidField, format!("{target}/value"));
    match (t, v) {
        (_, StudioPropertyValue::Alias(a)) => id(&a.id, "SVAR", target),
        (ComponentPropertyType::Boolean, StudioPropertyValue::Boolean(_)) => Ok(()),
        (ComponentPropertyType::InstanceSwap, StudioPropertyValue::Text(s)) => id(s, "SCMP", target),
        (
            ComponentPropertyType::Text
            | ComponentPropertyType::Variant
            | ComponentPropertyType::Slot,
            StudioPropertyValue::Text(s),
        ) if s.len() <= MAX_TEXT_BYTES => c.text(s, target),
        _ => Err(bad()),
    }
}

fn definitions(defs: &[StudioComponentProperty], owner: &str, c: &mut Ctx) -> Result<()> {
    if defs.len() > MAX_PROPERTIES {
        return Err(err(Code::BudgetExceeded, format!("{owner}/properties")));
    }
    c.nodes(defs.len(), owner)?;
    let mut names = BTreeSet::new();
    for (n, d) in defs.iter().enumerate() {
        c.tick()?;
        let target = format!("{owner}/properties/{n}");
        if !label(&d.property_name) || !names.insert(d.property_name.as_str()) {
            return Err(err(Code::InvalidField, format!("{target}/property_name")));
        }
        c.text(&d.property_name, &target)?;
        property_value(d.property_type, &d.default_value, &target, c)?;
        match (&d.options, d.property_type) {
            (None, ComponentPropertyType::InstanceSwap) => {
                return Err(err(Code::InvalidField, format!("{target}/options")));
            }
            (Some(o), ComponentPropertyType::InstanceSwap) => {
                if o.preferred_values.len() > MAX_PROPERTIES
                    || o.preferred_values.iter().any(|p| !label(&p.key))
                {
                    return Err(err(Code::InvalidField, format!("{target}/options")));
                }
                c.nodes(o.preferred_values.len(), &target)?;
            }
            (Some(_), _) => return Err(err(Code::InvalidField, format!("{target}/options"))),
            (None, _) => {}
        }
    }
    Ok(())
}

fn identity(
    publishing: &StudioPublishing,
    owner: &str,
    layer_id: &str,
    c: &mut Ctx,
) -> Result<()> {
    id(layer_id, "SLYR", &format!("{owner}/layer_id"))?;
    publishing.validate(owner, c)
}

pub(crate) fn validate_component(k: &StudioComponent, c: &mut Ctx) -> Result<()> {
    c.tick()?;
    let cid = k.component_id.as_str();
    id(cid, "SCMP", "component_id")?;
    c.nodes(1, cid)?;
    identity(&k.publishing, cid, &k.layer_id, c)?;
    if let Some(v) = &k.variant_properties {
        if v.is_empty() || v.len() > MAX_PROPERTIES {
            return Err(err(Code::InvalidField, format!("{cid}/variant_properties")));
        }
        for (name, value) in v {
            if !label(name) || !label(value) {
                return Err(err(Code::InvalidField, format!("{cid}/variant_properties")));
            }
            c.text(name, cid)?;
            c.text(value, cid)?;
        }
    }
    definitions(&k.component_property_definitions, cid, c)
}

pub(crate) fn validate_set(s: &StudioComponentSet, c: &mut Ctx) -> Result<()> {
    c.tick()?;
    let cid = s.component_id.as_str();
    id(cid, "SCMP", "component_id")?;
    c.nodes(1, cid)?;
    identity(&s.publishing, cid, &s.layer_id, c)?;
    id(
        &s.default_variant_id,
        "SCMP",
        &format!("{cid}/default_variant_id"),
    )?;
    let groups = &s.variant_group_properties;
    if groups.is_empty() || groups.len() > MAX_PROPERTIES {
        return Err(err(
            Code::InvalidField,
            format!("{cid}/variant_group_properties"),
        ));
    }
    for (name, values) in groups {
        c.tick()?;
        let unique: BTreeSet<&str> = values.iter().map(String::as_str).collect();
        if !label(name)
            || values.is_empty()
            || values.len() > MAX_PROPERTIES
            || unique.len() != values.len()
            || !values.iter().all(|v| label(v))
        {
            return Err(err(
                Code::InvalidField,
                format!("{cid}/variant_group_properties"),
            ));
        }
        c.text(name, cid)?;
        c.nodes(values.len(), cid)?;
    }
    definitions(&s.component_property_definitions, cid, c)
}

pub(crate) fn validate_instance(i: &StudioComponentInstance, c: &mut Ctx) -> Result<()> {
    c.tick()?;
    let iid = i.instance_id.as_str();
    id(iid, "SCIN", "instance_id")?;
    c.nodes(1, iid)?;
    if let Some(m) = &i.main_component_id {
        id(m, "SCMP", &format!("{iid}/main_component_id"))?;
    }
    if !i.scale_factor.is_finite() || !(0.01..=100.0).contains(&i.scale_factor) {
        return Err(err(Code::InvalidField, format!("{iid}/scale_factor")));
    }
    if i.component_properties.len() > MAX_PROPERTIES || i.overrides.len() > MAX_OVERRIDES {
        return Err(err(Code::BudgetExceeded, iid));
    }
    c.nodes(i.component_properties.len() + i.overrides.len(), iid)?;
    for (n, (name, p)) in i.component_properties.iter().enumerate() {
        c.tick()?;
        let target = format!("{iid}/component_properties/{n}");
        if !label(name) {
            return Err(err(Code::InvalidField, target));
        }
        c.text(name, iid)?;
        property_value(p.property_type, &p.value, &target, c)?;
        if let Some(v) = &p.bound_variable_id {
            id(v, "SVAR", &format!("{target}/bound_variable_id"))?;
        }
    }
    let mut nodes = BTreeSet::new();
    for (n, o) in i.overrides.iter().enumerate() {
        c.tick()?;
        let target = format!("{iid}/overrides/{n}");
        id(&o.overridden_node_id, "SLYR", &format!("{target}/overridden_node_id"))?;
        if !nodes.insert(o.overridden_node_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, format!("{target}/overridden_node_id")));
        }
        let fields: BTreeSet<&str> = o.overridden_fields.iter().map(String::as_str).collect();
        if o.overridden_fields.is_empty()
            || fields.len() != o.overridden_fields.len()
            || !fields.iter().all(|f| OVERRIDABLE_FIELDS.contains(f))
        {
            return Err(err(Code::InvalidField, format!("{target}/overridden_fields")));
        }
        for (field, value) in &o.values {
            if !fields.contains(field.as_str()) {
                return Err(err(Code::InvalidField, format!("{target}/values")));
            }
            value.validate(&format!("{target}/values"), c)?;
        }
    }
    Ok(())
}

pub(crate) fn validate_collection(k: &StudioVariableCollection, c: &mut Ctx) -> Result<()> {
    c.tick()?;
    let cid = k.collection_id.as_str();
    id(cid, "SVCL", "collection_id")?;
    c.nodes(1 + k.modes.len() + k.variable_ids.len(), cid)?;
    if !slash_path(&k.name) {
        return Err(err(Code::InvalidField, format!("{cid}/name")));
    }
    c.text(&k.name, cid)?;
    k.publishing.validate(cid, c)?;
    if k.is_extension {
        return Err(err(Code::UnsupportedDescriptor, format!("{cid}/is_extension")));
    }
    if k.modes.is_empty() || k.modes.len() > MAX_MODES {
        return Err(err(Code::InvalidField, format!("{cid}/modes")));
    }
    let mut modes = BTreeSet::new();
    for m in &k.modes {
        c.tick()?;
        if !label(&m.mode_id) || !label(&m.name) || !modes.insert(m.mode_id.as_str()) {
            return Err(err(Code::InvalidField, format!("{cid}/modes")));
        }
        c.text(&m.mode_id, cid)?;
        c.text(&m.name, cid)?;
    }
    if !modes.contains(k.default_mode_id.as_str()) {
        return Err(err(Code::DanglingReference, format!("{cid}/default_mode_id")));
    }
    let mut variables = BTreeSet::new();
    for v in &k.variable_ids {
        id(v, "SVAR", &format!("{cid}/variable_ids"))?;
        if !variables.insert(v.as_str()) {
            return Err(err(Code::DuplicateIdentity, format!("{cid}/variable_ids")));
        }
    }
    Ok(())
}

pub(crate) fn validate_variable(v: &StudioVariable, c: &mut Ctx) -> Result<()> {
    c.tick()?;
    let vid = v.variable_id.as_str();
    id(vid, "SVAR", "variable_id")?;
    c.nodes(1 + v.values_by_mode.len(), vid)?;
    id(
        &v.variable_collection_id,
        "SVCL",
        &format!("{vid}/variable_collection_id"),
    )?;
    if !slash_path(&v.name) {
        return Err(err(Code::InvalidField, format!("{vid}/name")));
    }
    c.text(&v.name, vid)?;
    v.publishing.validate(vid, c)?;
    if v.values_by_mode.is_empty() {
        return Err(err(Code::InvalidField, format!("{vid}/values_by_mode")));
    }
    for (mode, value) in &v.values_by_mode {
        let target = format!("{vid}/values_by_mode");
        if !label(mode) {
            return Err(err(Code::InvalidField, &target));
        }
        c.text(mode, vid)?;
        value.validate(&target, c)?;
        if value.resolved_type() != v.resolved_type {
            return Err(err(Code::InvalidField, format!("{target}/type")));
        }
    }
    let scopes: BTreeSet<&str> = v.scopes.iter().map(String::as_str).collect();
    if scopes.len() != v.scopes.len() || !scopes.iter().all(|s| VARIABLE_SCOPES.contains(s)) {
        return Err(err(Code::InvalidField, format!("{vid}/scopes")));
    }
    for (platform, name) in &v.code_syntax {
        if !CODE_SYNTAX_PLATFORMS.contains(&platform.as_str()) || !label(name) {
            return Err(err(Code::InvalidField, format!("{vid}/code_syntax")));
        }
        c.text(name, vid)?;
    }
    Ok(())
}
