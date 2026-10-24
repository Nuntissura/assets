//! Design state records (lane SMB-FIG-EDIT-2): the per-document instance state set
//! `StudioDesignStateRecords` (schema `hsk.studio.design_state@1`) beside the document like the proof
//! and multicam record sets (`StudioDocument` and `StudioDesignRecords` gain no field, so every
//! existing document and design record set is byte-identical).
//!
//! Spec basis: STU-IO-006 row 35 (`.fig` local copy, `EP` = edit-preserve: an instance edit made in
//! Studio survives save and reopen); STU-DS-003 "An instance renders the current component definition
//! plus its own local overrides"; STU-DS-008 variant switch "re-selects the member definition in
//! place while preserving compatible overrides"; STU-DS-016 "Overridable surfaces are: text content,
//! fills/strokes/paint, effects, visibility, nested instance swaps, and exposed-nested property
//! values"; STU-DS-017 overrides persist across variant switches and component swaps; STU-IO-005
//! unsupported-feature handling is lossless-of-intent (a state this record cannot hold is a named
//! loss of its producer, never silent).
//!
//! Stored (data only; resolving an instance is the components crate's job): one
//! [`StudioInstanceState`] per instance layer and content, i.e. the instance record a design command
//! produced for the instance layer `layer_id` while its children were exactly `content_signature`
//! (child layer ids in document order). The main component (`component_id`, the record's
//! `main_component_id`), the drawn member's variant assignment, the instance's component property values, its
//! overrides (one row per node and field, always with a value) and the component last swapped to.
//!
//! Studio readings, declared (lane notes `declared_readings`):
//! - states are keyed by (layer, content): undo and redo of a redraw return the layer to earlier
//!   children, and the state recorded for those children is the one that applies, so the set keeps
//!   at most one state per (layer_id, content_signature), not one per layer; a layer holds one
//!   instance (`instance_id` equal on all its states); the producer bounds the history
//!   ([`MAX_DESIGN_STATES`]);
//! - a content signature lists child ids that may no longer exist (a state of earlier, undone
//!   content), so only their shape is checked; the state that applies is the one whose signature
//!   equals the layer's current children (the producer's lookup);
//! - override fields are the STU-DS-115 closed set ([`crate::OVERRIDABLE_FIELDS`]), which stores the
//!   STU-DS-016 surfaces and adds opacity, corner radii and layout sizing; `characters` takes a
//!   string, `visible` a boolean and `opacity` a float `0..=1` (or a variable alias);
//! - `component_id` is the record's `main_component_id` as stored (a component or a set); a variant
//!   switch keeps it and records the chosen value as a `VARIANT` property (hsk-studio-components
//!   `switch_variant`), so the member the instance draws is the set member whose assignment equals
//!   the main member's (a set: its default variant's) with every `VARIANT` property applied;
//!   `variant` is that assignment written out (validated, never free); `swap_target` is the
//!   component (or set) a component swap chose, and the main component or the drawn member is it
//!   or shares its set (a later variant switch moves within that set);
//! - without design records only the document is checked (ids, shapes, the instance layer); the
//!   producer says so (the window reports `design_records_absent`).
//!
//! Refusals: an entry for another document `InvalidField document_id`; a missing instance layer, an
//! unknown instance, component, set, variant property or value, component property or override node
//! `DanglingReference`; a layer that is not a group, a variant assignment, property type or swap
//! target that does not match the design records, an override field outside the closed set or a
//! value of the wrong type `InvalidField`; malformed ids `InvalidId`; two states for one (layer,
//! content), one instance on two layers or two rows for one node and field `DuplicateIdentity`;
//! counts beyond the hard ceilings or the caller node budget `BudgetExceeded`. The records arrive
//! from untrusted wire JSON: integer arithmetic is denied (workflow V4-01) and every list is counted
//! before it is walked.
#![deny(clippy::arithmetic_side_effects)]
use super::{
    Budget, Code, ComponentPropertyType, OVERRIDABLE_FIELDS, Parent, Payload, Result, StudioComponent, StudioDesignRecords,
    StudioDocument, StudioInstanceProperty, StudioPropertyValue, StudioValue, VariableType, err, id,
    values::{Ctx, MAX_TEXT_BYTES, label},
};
use hsk_studio_accord::CancellationToken;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

schema_token!(DesignStateRecordsSchema, "hsk.studio.design_state@1");

/// Instance states in one record set (the producer keeps the newest).
pub const MAX_DESIGN_STATES: usize = 1024;
/// Child layer ids in one content signature.
pub const MAX_STATE_SIGNATURE_LAYERS: usize = 4096;
/// Override rows of one state (Folio `MAX_OVERRIDES`).
pub const MAX_STATE_OVERRIDES: usize = super::MAX_OVERRIDES;
/// Component property values or variant properties of one state (Folio `MAX_PROPERTIES`).
pub const MAX_STATE_PROPERTIES: usize = super::MAX_PROPERTIES;
/// Deepest definition nesting an override node is searched through.
pub const MAX_STATE_NODE_DEPTH: usize = 64;

/// One override of an instance state: field `field` of definition node `node_id` set to `value`
/// (STU-DS-016/117).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioStateOverride {
    /// `SLYR` layer of the member's definition.
    pub node_id: String,
    /// One of the STU-DS-115 overridable fields.
    pub field: String,
    pub value: StudioValue,
}

/// The state of one instance layer for one content (module docs).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioInstanceState {
    /// The `group` layer that draws the instance.
    pub layer_id: String,
    /// `SCIN` instance record of the design records.
    pub instance_id: String,
    /// `SCMP` main component of the instance record (`main_component_id`; a component or a set).
    pub component_id: String,
    /// The drawn member's variant assignment, property to value (empty for a component outside a set).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub variant: BTreeMap<String, String>,
    /// Component property values (STU-DS-009/105), name to typed value.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, StudioInstanceProperty>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<StudioStateOverride>,
    /// The component or set a component swap chose (absent: never swapped).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swap_target: Option<String>,
    /// The instance layer's child layer ids, in document order, when this state was recorded.
    pub content_signature: Vec<String>,
}

/// The per-document design state record set.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioDesignStateRecords {
    pub schema_id: DesignStateRecordsSchema,
    /// `SDOC` id of the annotated document.
    pub document_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instances: Vec<StudioInstanceState>,
}
impl StudioDesignStateRecords {
    /// An empty set for `document_id`.
    pub fn new(document_id: impl Into<String>) -> Self {
        Self { schema_id: DesignStateRecordsSchema::V1, document_id: document_id.into(), instances: Vec::new() }
    }
    /// The state of `layer_id` recorded for the children `content` (document order).
    pub fn state(&self, layer_id: &str, content: &[String]) -> Option<&StudioInstanceState> {
        self.instances.iter().find(|s| s.layer_id == layer_id && s.content_signature == content)
    }
}

/// JSON schema (draft 2020-12) of the design state record set.
pub fn design_state_records_schema() -> schemars::Schema {
    schemars::generate::SchemaSettings::draft2020_12().into_generator().into_root_schema_for::<StudioDesignStateRecords>()
}

/// Parses wire JSON (bounded by `Budget::input_bytes` and depth before any parse; duplicate keys
/// refused) and validates it against the document and, when given, its design records.
pub fn parse_design_state_records(
    input: &[u8],
    d: &StudioDocument,
    design: Option<&StudioDesignRecords>,
    b: Budget,
    token: &CancellationToken,
) -> Result<StudioDesignStateRecords> {
    super::preflight(input, b, token)?;
    let value = serde_json::from_slice::<super::UniqueValue>(input)
        .map_err(|e| {
            err(
                if e.to_string().contains("duplicate_field") { Code::DuplicateField } else { Code::InvalidJson },
                "design_state",
            )
        })?
        .0;
    let r: StudioDesignStateRecords = serde_json::from_value(value).map_err(|_| err(Code::InvalidJson, "design_state"))?;
    validate_design_state_records(&r, d, design, b, token)?;
    Ok(r)
}

/// A typed component property value of a state (Folio's instance property rule).
fn property_ok(t: ComponentPropertyType, v: &StudioPropertyValue) -> bool {
    match (t, v) {
        (_, StudioPropertyValue::Alias(a)) => id(&a.id, "SVAR", "").is_ok(),
        (ComponentPropertyType::Boolean, StudioPropertyValue::Boolean(_)) => true,
        (ComponentPropertyType::InstanceSwap, StudioPropertyValue::Text(s)) => id(s, "SCMP", "").is_ok(),
        (ComponentPropertyType::Text | ComponentPropertyType::Variant | ComponentPropertyType::Slot, StudioPropertyValue::Text(s)) => {
            s.len() <= MAX_TEXT_BYTES
        }
        _ => false,
    }
}

/// The value type a drawn override field takes (module docs); other fields take any valid value.
fn override_value_ok(field: &str, v: &StudioValue) -> bool {
    if let StudioValue::VariableAlias { resolved_type, .. } = v {
        return match field {
            "characters" => *resolved_type == VariableType::String,
            "visible" => *resolved_type == VariableType::Boolean,
            "opacity" => *resolved_type == VariableType::Float,
            _ => true,
        };
    }
    match field {
        "characters" => matches!(v, StudioValue::String { .. }),
        "visible" => matches!(v, StudioValue::Boolean { .. }),
        "opacity" => matches!(v, StudioValue::Float { value } if (0.0..=1.0).contains(value)),
        _ => true,
    }
}

/// The parent layer id of `layer_id` (`None`: root or absent).
fn parent_of<'a>(d: &'a StudioDocument, layer_id: &str) -> Option<&'a str> {
    match &d.layers.iter().find(|l| l.layer_id == layer_id)?.parent {
        Parent::Node(n) => Some(n.node_id.as_str()),
        Parent::Root(()) => None,
    }
}

/// Whether `node` is a layer under `root` (bounded walk up the parents).
fn under(d: &StudioDocument, node: &str, root: &str) -> bool {
    let mut at = node;
    for _ in 0..MAX_STATE_NODE_DEPTH {
        match parent_of(d, at) {
            Some(p) if p == root => return true,
            Some(p) => at = p,
            None => return false,
        }
    }
    false
}

/// The set a member component belongs to (its container layer is a child of the set's layer).
fn set_of<'a>(d: &StudioDocument, design: &'a StudioDesignRecords, member: &StudioComponent) -> Option<&'a super::StudioComponentSet> {
    let parent = parent_of(d, &member.layer_id)?;
    design.component_sets.iter().find(|s| s.layer_id == parent)
}

/// Validates the record set against the document and, when given, its design records (module docs).
pub fn validate_design_state_records(
    r: &StudioDesignStateRecords,
    d: &StudioDocument,
    design: Option<&StudioDesignRecords>,
    b: Budget,
    token: &CancellationToken,
) -> Result<()> {
    let mut cx = Ctx::new(b, token, &super::Unresolved);
    cx.tick()?;
    id(&r.document_id, "SDOC", "document_id")?;
    if r.document_id != d.document_id {
        return Err(err(Code::InvalidField, "document_id"));
    }
    if r.instances.len() > MAX_DESIGN_STATES {
        return Err(err(Code::BudgetExceeded, "instances"));
    }
    cx.nodes(r.instances.len(), "instances")?;
    let mut keys: BTreeSet<(&str, &[String])> = BTreeSet::new();
    let mut layer_instance: BTreeMap<&str, &str> = BTreeMap::new();
    let mut instance_layer: BTreeMap<&str, &str> = BTreeMap::new();
    for (i, s) in r.instances.iter().enumerate() {
        cx.tick()?;
        let t = format!("instances/{i}");
        id(&s.layer_id, "SLYR", &format!("{t}/layer_id"))?;
        id(&s.instance_id, "SCIN", &format!("{t}/instance_id"))?;
        id(&s.component_id, "SCMP", &format!("{t}/component_id"))?;
        let layer = d.layers.iter().find(|l| l.layer_id == s.layer_id).ok_or_else(|| err(Code::DanglingReference, format!("{t}/layer_id")))?;
        if layer.kind != "group" || !matches!(layer.payload, Payload::Group) {
            return Err(err(Code::InvalidField, format!("{t}/layer_id")));
        }
        // Content signature: well-formed, unique child ids (module docs: they may be gone).
        if s.content_signature.len() > MAX_STATE_SIGNATURE_LAYERS {
            return Err(err(Code::BudgetExceeded, format!("{t}/content_signature")));
        }
        let mut seen = BTreeSet::new();
        for c in &s.content_signature {
            cx.tick()?;
            id(c, "SLYR", &format!("{t}/content_signature"))?;
            if !seen.insert(c.as_str()) || c == &s.layer_id {
                return Err(err(Code::InvalidField, format!("{t}/content_signature")));
            }
        }
        if !keys.insert((s.layer_id.as_str(), s.content_signature.as_slice())) {
            return Err(err(Code::DuplicateIdentity, format!("{t}/content_signature")));
        }
        // One instance per layer, one layer per instance.
        if *layer_instance.entry(s.layer_id.as_str()).or_insert(s.instance_id.as_str()) != s.instance_id {
            return Err(err(Code::InvalidField, format!("{t}/instance_id")));
        }
        if *instance_layer.entry(s.instance_id.as_str()).or_insert(s.layer_id.as_str()) != s.layer_id {
            return Err(err(Code::DuplicateIdentity, format!("{t}/instance_id")));
        }
        // Variant assignment and component property values: shapes.
        if s.variant.len() > MAX_STATE_PROPERTIES || s.properties.len() > MAX_STATE_PROPERTIES {
            return Err(err(Code::BudgetExceeded, t.clone()));
        }
        if s.overrides.len() > MAX_STATE_OVERRIDES {
            return Err(err(Code::BudgetExceeded, format!("{t}/overrides")));
        }
        cx.nodes(s.properties.len(), &t)?;
        cx.nodes(s.overrides.len(), &t)?;
        for (prop, value) in &s.variant {
            if !label(prop) || !label(value) {
                return Err(err(Code::InvalidField, format!("{t}/variant")));
            }
            cx.text(prop, &t)?;
            cx.text(value, &t)?;
        }
        for (name, p) in &s.properties {
            cx.tick()?;
            let pt = format!("{t}/properties/{name}");
            if !label(name) || !property_ok(p.property_type, &p.value) {
                return Err(err(Code::InvalidField, pt));
            }
            cx.text(name, &t)?;
            if let StudioPropertyValue::Text(text) = &p.value {
                cx.text(text, &pt)?;
            }
            if let Some(v) = &p.bound_variable_id {
                id(v, "SVAR", &format!("{pt}/bound_variable_id"))?;
            }
        }
        let mut rows = BTreeSet::new();
        for (n, o) in s.overrides.iter().enumerate() {
            cx.tick()?;
            let ot = format!("{t}/overrides/{n}");
            id(&o.node_id, "SLYR", &format!("{ot}/node_id"))?;
            if !OVERRIDABLE_FIELDS.contains(&o.field.as_str()) {
                return Err(err(Code::InvalidField, format!("{ot}/field")));
            }
            if !rows.insert((o.node_id.as_str(), o.field.as_str())) {
                return Err(err(Code::DuplicateIdentity, format!("{ot}/field")));
            }
            o.value.validate(&ot, &mut cx)?;
            if !override_value_ok(&o.field, &o.value) {
                return Err(err(Code::InvalidField, format!("{ot}/value")));
            }
            if !d.layers.iter().any(|l| l.layer_id == o.node_id) {
                return Err(err(Code::DanglingReference, format!("{ot}/node_id")));
            }
        }
        if let Some(target) = &s.swap_target {
            id(target, "SCMP", &format!("{t}/swap_target"))?;
        }
        let Some(design) = design else { continue };
        // With the design records: every reference resolves and matches.
        if !design.component_instances.iter().any(|k| k.instance_id == s.instance_id) {
            return Err(err(Code::DanglingReference, format!("{t}/instance_id")));
        }
        // The main component (a component or a set) and the set it selects within.
        let main = design.components.iter().find(|k| k.component_id == s.component_id);
        let set = match main {
            Some(k) => set_of(d, design, k),
            None => Some(
                design.component_sets.iter().find(|k| k.component_id == s.component_id).ok_or_else(|| err(Code::DanglingReference, format!("{t}/component_id")))?,
            ),
        };
        for (prop, value) in &s.variant {
            let declared = set.and_then(|k| k.variant_group_properties.get(prop)).is_some_and(|values| values.contains(value));
            if !declared {
                return Err(err(Code::DanglingReference, format!("{t}/variant/{prop}")));
            }
        }
        // Expected assignment: the main member's (a set: its default variant's), with every VARIANT
        // property of the instance applied (STU-DS-008 variant switch in place).
        let base = match main {
            Some(k) => k.variant_properties.clone(),
            None => set.and_then(|k| design.components.iter().find(|m| m.component_id == k.default_variant_id)).and_then(|m| m.variant_properties.clone()),
        };
        let mut expected = base.unwrap_or_default();
        for (name, p) in s.properties.iter().filter(|(_, p)| p.property_type == ComponentPropertyType::Variant) {
            let declared = match (set, &p.value) {
                (Some(k), StudioPropertyValue::Text(v)) => k.variant_group_properties.get(name).is_some_and(|values| values.contains(v)),
                _ => false,
            };
            if !declared {
                return Err(err(Code::DanglingReference, format!("{t}/properties/{name}")));
            }
            if let StudioPropertyValue::Text(v) = &p.value {
                expected.insert(name.clone(), v.clone());
            }
        }
        if s.variant != expected {
            return Err(err(Code::InvalidField, format!("{t}/variant")));
        }
        // The member the assignment selects (a component outside a set is its own member).
        let member = match set {
            Some(k) => design
                .components
                .iter()
                .find(|m| set_of(d, design, m).is_some_and(|x| x.component_id == k.component_id) && m.variant_properties.as_ref() == Some(&expected))
                .ok_or_else(|| err(Code::DanglingReference, format!("{t}/variant")))?,
            None => main.ok_or_else(|| err(Code::DanglingReference, format!("{t}/component_id")))?,
        };
        for (name, p) in s.properties.iter().filter(|(_, p)| p.property_type != ComponentPropertyType::Variant) {
            let def = set
                .into_iter()
                .flat_map(|k| k.component_property_definitions.iter())
                .chain(member.component_property_definitions.iter())
                .find(|k| &k.property_name == name)
                .ok_or_else(|| err(Code::DanglingReference, format!("{t}/properties/{name}")))?;
            if def.property_type != p.property_type {
                return Err(err(Code::InvalidField, format!("{t}/properties/{name}")));
            }
        }
        for (n, o) in s.overrides.iter().enumerate() {
            if !under(d, &o.node_id, &member.layer_id) {
                return Err(err(Code::DanglingReference, format!("{t}/overrides/{n}/node_id")));
            }
        }
        if let Some(target) = &s.swap_target {
            let same = target == &s.component_id
                || target == &member.component_id
                || set.is_some_and(|k| &k.component_id == target)
                || design.components.iter().find(|k| &k.component_id == target).is_some_and(|k| {
                    let theirs = set_of(d, design, k).map(|x| x.component_id.as_str());
                    theirs.is_some() && theirs == set.map(|x| x.component_id.as_str())
                });
            let known = design.components.iter().any(|k| &k.component_id == target) || design.component_sets.iter().any(|k| &k.component_id == target);
            if !known {
                return Err(err(Code::DanglingReference, format!("{t}/swap_target")));
            }
            if !same {
                return Err(err(Code::InvalidField, format!("{t}/swap_target")));
            }
        }
    }
    Ok(())
}
