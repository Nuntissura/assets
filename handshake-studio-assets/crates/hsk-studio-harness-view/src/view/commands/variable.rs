//! `variable_collection.*`, `variable.*` and `node.*_explicit_variable_mode_for_collection` (lane
//! SMB-DS-VARIABLES-AUTHOR-1; [STU-DS-132], [STU-DS-133], [STU-DS-162]): decoders into `VariableCommand`,
//! run by `Viewer::variable_command` (one undo step per mutating row; the records ride the vector entry's
//! `design` member). A value record goes through Folio's own `Deserialize` ([STU-DS-127]); the closed
//! enumerations of the resolved type ([STU-DS-125]), the code-syntax platform ([STU-DS-129]) and a property
//! scope ([STU-DS-128]) are refused `parameter_invalid` before anything runs.
#![deny(clippy::arithmetic_side_effects)]

use super::super::Viewer;
use super::super::design::variables::{MAX_SCOPES, VariableCommand};
use super::decode::{Fields, folio_field, invalid};
use super::{CommandRefusal, engine_refusal};
use hsk_studio_folio::{self as folio, StudioValue, VariableType};
use serde_json::{Value, json};

const TYPES: &[(&str, VariableType)] = &[
    ("BOOLEAN", VariableType::Boolean),
    ("COLOR", VariableType::Color),
    ("EASING", VariableType::Easing),
    ("FLOAT", VariableType::Float),
    ("STRING", VariableType::String),
    ("TIMING", VariableType::Timing),
];
const PLATFORMS: &[(&str, &str)] = &[("WEB", "WEB"), ("ANDROID", "ANDROID"), ("iOS", "iOS")];
const VALUE: &str = "a value record: {type, value} or {type: VARIABLE_ALIAS, id, resolved_type}";

fn value(f: &Fields<'_>) -> Result<StudioValue, CommandRefusal> {
    folio_field!(f, "value", StudioValue, VALUE)
}

/// At most the 22 scope tokens; their membership is the engine's (`variable_scope_unknown`).
fn scopes(f: &Fields<'_>) -> Result<Option<Vec<String>>, CommandRefusal> {
    if !f.has("scopes") {
        return Ok(None);
    }
    let list = f.list("scopes")?;
    if list.len() > MAX_SCOPES {
        return Err(invalid("scopes", "at most the 22 STU-DS-128 scope tokens"));
    }
    Ok(Some(list))
}

fn scope(f: &Fields<'_>) -> Result<Option<String>, CommandRefusal> {
    if !f.has("scope") {
        return Ok(None);
    }
    let table: Vec<(&str, &str)> = folio::VARIABLE_SCOPES.iter().map(|s| (*s, *s)).collect();
    f.token("scope", &table).map(|s| Some(s.to_owned()))
}

fn command_of(id: &str, f: &Fields<'_>) -> Result<VariableCommand, CommandRefusal> {
    Ok(match id {
        "variable_collection.create" => VariableCommand::CollectionCreate { name: f.text("name")?, mode_name: f.opt_text("mode_name")? },
        "variable_collection.add_mode" => VariableCommand::AddMode { collection: f.text("collection")?, name: f.text("name")? },
        "variable_collection.rename_mode" => VariableCommand::RenameMode { collection: f.text("collection")?, mode: f.text("mode")?, name: f.text("name")? },
        "variable_collection.remove_mode" => VariableCommand::RemoveMode { collection: f.text("collection")?, mode: f.text("mode")? },
        "variable_collection.remove" => VariableCommand::CollectionRemove { collection: f.text("collection")? },
        "variable.create" => VariableCommand::Create {
            collection: f.text("collection")?,
            name: f.text("name")?,
            resolved_type: f.token("resolved_type", TYPES)?,
            value: value(f)?,
            scopes: scopes(f)?,
            description: f.opt_text("description")?,
            hidden_from_publishing: if f.has("hidden_from_publishing") { Some(f.flag("hidden_from_publishing")?) } else { None },
        },
        "variable.remove" => VariableCommand::Remove { variable: f.text("variable")? },
        "variable.set_value_for_mode" => VariableCommand::SetValueForMode { variable: f.text("variable")?, mode: f.text("mode")?, value: value(f)? },
        "variable.set_code_syntax" => VariableCommand::SetCodeSyntax { variable: f.text("variable")?, platform: f.token("platform", PLATFORMS)?.to_owned(), name: f.text("name")? },
        "variable.remove_code_syntax" => VariableCommand::RemoveCodeSyntax { variable: f.text("variable")?, platform: f.token("platform", PLATFORMS)?.to_owned() },
        "variable.create_alias" => VariableCommand::CreateAlias { variable: f.text("variable")? },
        "variable.resolve_for_consumer" => VariableCommand::ResolveForConsumer { variable: f.text("variable")?, node: f.text("node")?, scope: scope(f)? },
        "node.set_explicit_variable_mode_for_collection" => VariableCommand::SetExplicitMode { node: f.text("node")?, collection: f.text("collection")?, mode: f.text("mode")? },
        "node.clear_explicit_variable_mode_for_collection" => VariableCommand::ClearExplicitMode { node: f.text("node")?, collection: f.text("collection")? },
        other => return Err(CommandRefusal::new("command_not_driveable", json!({"command_id": other}))),
    })
}

/// One row of this file (module docs).
pub(super) fn run(v: &mut Viewer, id: &str, f: Fields<'_>, before: usize) -> Result<Value, CommandRefusal> {
    let command = command_of(id, &f)?;
    v.variable_command(command).map_err(|e| engine_refusal(v, before, e))
}
