//! Typed motion commands (lane SMB-MOTION-COMMANDS-1): red stub, the catalog rows without the engine.
#![deny(clippy::arithmetic_side_effects)]
use super::Run;
use crate::action::{self, CommandEntry, ValueType};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

const MAX_ADDRESS: usize = 256;

/// The `STUDIO_*` id [STU-MOT-130] names for a row: `composition.new` is `STUDIO_COMPOSITION_NEW`.
pub fn spec_id(command_id: &str) -> String {
    format!("STUDIO_{}", command_id.replace('.', "_").to_ascii_uppercase())
}

/// The motion row of `command_id` (its row id or its `STUDIO_*` spelling).
pub fn row(command_id: &str) -> Option<&'static CommandEntry> {
    if command_id.len() > MAX_ADDRESS {
        return None;
    }
    action::RUN_COMMANDS.iter().find(|c| c.run_method == "motion_command" && (c.command_id == command_id || spec_id(c.command_id) == command_id))
}

/// A row's parameters from text (the drive loop carries text): numbers, booleans, lists and classes
/// are JSON, strings and enumerations stay text, `null` clears. Refused: `(code, key)`.
pub fn params_from_text(row: &CommandEntry, params: &BTreeMap<String, String>) -> Result<Value, (&'static str, String)> {
    let mut out = Map::new();
    for (key, text) in params {
        let Some(spec) = row.parameters.iter().find(|p| p.key == key) else {
            return Err(("parameter_unknown", bounded(key)));
        };
        let value = if text == "null" {
            Value::Null
        } else {
            match spec.value_type {
                ValueType::Reference | ValueType::String | ValueType::Enumerated => match serde_json::from_str::<Value>(text) {
                    // A class-shaped enumeration (`resolution`, `time_span`) arrives as JSON text.
                    Ok(v @ Value::Object(_)) => v,
                    _ => Value::String(text.clone()),
                },
                ValueType::Boolean | ValueType::Number | ValueType::Measured | ValueType::List | ValueType::Class => {
                    let v: Value = serde_json::from_str(text).map_err(|_| ("parameter_invalid", bounded(key)))?;
                    let fits = match spec.value_type {
                        ValueType::Boolean => v.is_boolean(),
                        ValueType::Number | ValueType::Measured => v.is_number(),
                        ValueType::List => v.is_array(),
                        // A class parameter may be a bare member of an enumeration (`time_span`).
                        _ => v.is_object() || v.is_string(),
                    };
                    if !fits {
                        return Err(("parameter_invalid", bounded(key)));
                    }
                    v
                }
            }
        };
        out.insert(key.clone(), value);
    }
    Ok(Value::Object(out))
}

fn bounded(text: &str) -> String {
    text.chars().take(MAX_ADDRESS).collect()
}

/// Red: every command refuses until the engine lands.
pub(super) fn run(run: &mut Run<'_>, _command_id: &str, _params: &Value) -> bool {
    let before = run.frames.count();
    run.own_or_push("motion_command", before, Err("not_built"))
}
