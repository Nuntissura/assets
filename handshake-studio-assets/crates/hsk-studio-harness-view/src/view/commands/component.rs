//! `component.*`, `component_set.*` and `instance.*` (lane SMB-DS-COMPONENTS-AUTHOR-1; [STU-DS-107],
//! [STU-DS-112], [STU-DS-162]): decoders into `ComponentCommand`, run by `Viewer::component_command` (one
//! undo step per mutating row; the records ride the vector entry's `design` member). Nested records are
//! bounded before they are collected: at most [`MAX_SET_MEMBERS`] components and Folio's property ceiling
//! of properties and values.
#![deny(clippy::arithmetic_side_effects)]

use super::super::Viewer;
use super::super::design::component::{ComponentCommand, MAX_SET_MEMBERS};
use super::decode::{Fields, folio_field, invalid};
use super::{CommandRefusal, engine_refusal};
use hsk_studio_folio::{self as folio, ComponentPropertyType, StudioDocumentationLink};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

const TYPES: &[(&str, ComponentPropertyType)] = &[
    ("BOOLEAN", ComponentPropertyType::Boolean),
    ("TEXT", ComponentPropertyType::Text),
    ("INSTANCE_SWAP", ComponentPropertyType::InstanceSwap),
    ("VARIANT", ComponentPropertyType::Variant),
    ("SLOT", ComponentPropertyType::Slot),
];

/// A JSON object of at most `limit` members.
fn object<'a>(value: &'a Value, key: &str, limit: usize, expected: &str) -> Result<&'a Map<String, Value>, CommandRefusal> {
    value.as_object().filter(|m| m.len() <= limit).ok_or_else(|| invalid(key, expected))
}

/// `{name: [value, ...]}`.
fn properties(f: &Fields<'_>) -> Result<BTreeMap<String, Vec<String>>, CommandRefusal> {
    let expected = "{name: [value, ...]} of at most 256 properties and 256 values each";
    let value = f.json("properties", expected)?;
    let mut out = BTreeMap::new();
    for (name, values) in object(&value, "properties", folio::MAX_PROPERTIES, expected)? {
        let list = values.as_array().filter(|a| a.len() <= folio::MAX_PROPERTIES).ok_or_else(|| invalid("properties", expected))?;
        let list = list.iter().map(|v| v.as_str().map(str::to_owned)).collect::<Option<Vec<_>>>().ok_or_else(|| invalid("properties", expected))?;
        out.insert(name.clone(), list);
    }
    Ok(out)
}

/// `{component: {name: value}}`.
fn assignments(f: &Fields<'_>) -> Result<BTreeMap<String, BTreeMap<String, String>>, CommandRefusal> {
    let expected = "{component: {name: value}} of at most 256 components and 256 properties each";
    let value = f.json("assignments", expected)?;
    let mut out = BTreeMap::new();
    for (component, given) in object(&value, "assignments", MAX_SET_MEMBERS, expected)? {
        let mut row = BTreeMap::new();
        for (name, v) in object(given, "assignments", folio::MAX_PROPERTIES, expected)? {
            row.insert(name.clone(), v.as_str().ok_or_else(|| invalid("assignments", expected))?.to_owned());
        }
        out.insert(component.clone(), row);
    }
    Ok(out)
}

fn opt_raw(f: &Fields<'_>, key: &str, expected: &str) -> Result<Option<Value>, CommandRefusal> {
    if f.has(key) { f.raw(key, expected).map(Some) } else { Ok(None) }
}

fn command_of(id: &str, f: &Fields<'_>) -> Result<ComponentCommand, CommandRefusal> {
    const DEFAULT: &str = "a boolean, a text or an SCMP id";
    const OPTIONS: &str = "a list of preferred values or of variant values";
    Ok(match id {
        "component.create_from_selection" => ComponentCommand::CreateFromSelection { layers: f.list("layers")?, name: f.opt_text("name")? },
        "component.create_one_per_top_level_node" => ComponentCommand::CreateOnePerTopLevelNode { layers: f.list("layers")? },
        "component.create_instance" => ComponentCommand::CreateInstance { component: f.text("component")?, at: [f.number("x")?, f.number("y")?], parent: f.opt_text("parent")? },
        "component.list_instances" => ComponentCommand::ListInstances { component: f.text("component")? },
        "component.add_property" => ComponentCommand::AddProperty {
            component: f.text("component")?,
            name: f.text("name")?,
            property_type: f.token("type", TYPES)?,
            default: f.raw("default", DEFAULT)?,
            options: opt_raw(f, "options", OPTIONS)?,
        },
        "component.edit_property" => ComponentCommand::EditProperty {
            component: f.text("component")?,
            name: f.text("name")?,
            new_name: f.opt_text("new_name")?,
            default: opt_raw(f, "default", DEFAULT)?,
            options: opt_raw(f, "options", OPTIONS)?,
        },
        "component.delete_property" => ComponentCommand::DeleteProperty { component: f.text("component")?, name: f.text("name")? },
        "component.set_property_reference" => ComponentCommand::SetPropertyReference { layer: f.text("layer")?, field: f.text("field")?, property: f.opt_text("property")? },
        "component.set_description" => ComponentCommand::SetDescription {
            component: f.text("component")?,
            description: f.text("description")?,
            links: if f.has("documentation_links") { Some(folio_field!(f, "documentation_links", Vec<StudioDocumentationLink>, "[{name, url}]")?) } else { None },
        },
        "component.sync_instances" => ComponentCommand::SyncInstances { component: f.text("component")? },
        "component_set.combine_as_variants" => ComponentCommand::CombineAsVariants { components: f.list("components")?, properties: properties(f)?, assignments: assignments(f)?, name: f.opt_text("name")? },
        "component_set.set_default_variant" => ComponentCommand::SetDefaultVariant { set: f.text("set")?, component: f.text("component")? },
        "instance.detach" => ComponentCommand::Detach { layer: f.text("layer")? },
        "instance.get_main_component" => ComponentCommand::GetMainComponent { layer: f.text("layer")? },
        other => return Err(CommandRefusal::new("command_not_driveable", serde_json::json!({"command_id": other}))),
    })
}

/// One row of this file (module docs).
pub(super) fn run(v: &mut Viewer, id: &str, f: Fields<'_>, before: usize) -> Result<Value, CommandRefusal> {
    let command = command_of(id, &f)?;
    v.component_command(command).map_err(|e| engine_refusal(v, before, e))
}
