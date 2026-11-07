//! `design.import_fig` and `design.instance.*` (lane SMB-DRIVE-TYPED-2, STU-DS-008/009/016/017/162): a
//! design file opened as a native document and every `DesignCommand` on the instance a layer draws,
//! through `Viewer::design_command` (one chronicle step each, named `design_<key>`, with the instance's
//! state in the `design` entry, so it survives save and reopen). The other `design.*` rows are the auto
//! layout and grid methods of `vector.rs`.
#![deny(clippy::arithmetic_side_effects)]

use super::super::Viewer;
use super::super::design::DesignCommand;
use super::decode::{Fields, invalid};
use super::{CommandRefusal, engine_refusal};
use hsk_studio_folio::{StudioPropertyValue, StudioValue};
use serde_json::{Value, json};

/// The rows this file runs.
pub(super) fn owns(id: &str) -> bool {
    id == "design.import_fig" || id.starts_with("design.instance.")
}

fn command_of(id: &str, f: &Fields<'_>) -> Result<DesignCommand, CommandRefusal> {
    Ok(match id {
        "design.instance.switch_variant" => DesignCommand::SwitchVariant { property: f.text("property")?, value: f.text("value")? },
        "design.instance.set_property" => {
            let value = match f.text("kind")?.as_str() {
                "boolean" => StudioPropertyValue::Boolean(f.flag("value")?),
                "text" => StudioPropertyValue::Text(f.text("value")?),
                _ => return Err(invalid("kind", "boolean|text")),
            };
            DesignCommand::SetProperty { name: f.text("name")?, value }
        }
        "design.instance.set_override" => {
            let field = f.text("field")?;
            let value = match field.as_str() {
                "characters" => StudioValue::String { value: f.text("value")? },
                "visible" => StudioValue::Boolean { value: f.flag("value")? },
                "opacity" => StudioValue::Float { value: f.number("value")? },
                _ => return Err(invalid("field", "characters|visible|opacity")),
            };
            DesignCommand::SetOverride { node_id: f.text("node_id")?, field, value }
        }
        "design.instance.reset_override" => DesignCommand::ResetOverride { node_id: f.text("node_id")?, field: f.text("field")? },
        "design.instance.reset_all" => DesignCommand::ResetAll,
        "design.instance.swap_component" => DesignCommand::Swap { component_id: f.text("component_id")? },
        other => return Err(CommandRefusal::new("command_not_driveable", json!({"command_id": other}))),
    })
}

/// One row of this file (module docs).
pub(super) fn run(v: &mut Viewer, id: &str, f: Fields<'_>, before: usize) -> Result<Value, CommandRefusal> {
    match id {
        "design.import_fig" => {
            let report = v.import_fig(&f.path("source")?, &f.path("dest")?, f.flag_or("overwrite", false)?).map_err(|e| engine_refusal(v, before, e))?;
            let layers: Vec<Value> = v.receipt().and_then(|r| r["design"]["instances"].as_array()).map(|list| list.iter().map(|b| b["layer_id"].clone()).collect()).unwrap_or_default();
            Ok(json!({
                "layers": report.layers, "vector_paths": report.vector_paths, "text_layers": report.text_layers, "instances": report.instances,
                "loss_entries": report.losses.len(), "instance_layers": layers,
            }))
        }
        "design.instance.inspect" => {
            let layer = f.text("layer")?;
            v.instance_view(&layer).map(|view| view.json()).ok_or_else(|| CommandRefusal::new("not_an_instance", json!({"layer": layer})))
        }
        _ => {
            let layer = f.text("layer")?;
            let command = command_of(id, &f)?;
            v.design_command(&layer, command).map_err(|e| engine_refusal(v, before, e))
        }
    }
}
