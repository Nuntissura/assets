//! `export.*` (lane SMB-ARTBOARD-EXPORT-1): the node export settings and the export run (module
//! `export_set`). A node is a layer or artboard id, or a name exactly one node has.
#![deny(clippy::arithmetic_side_effects)]

use super::super::Viewer;
use super::super::develop::Refusal;
use super::decode::{Fields, folio_field};
use super::{CommandRefusal, method_refusal};
use hsk_studio_folio::StudioNodeRenderSetting;
use serde_json::{Value, json};

const SETTINGS: &str = "a list of Folio node-render settings (STU-IO-111: {format: PNG|JPG|SVG|PDF, ...})";

fn refused(r: Refusal) -> CommandRefusal {
    CommandRefusal::new(r.code, json!({"message": r.detail}))
}

pub(super) fn run(viewer: &mut Viewer, id: &str, f: Fields<'_>) -> Result<Value, CommandRefusal> {
    match id {
        "export.set_settings" => {
            let key = f.text("node")?;
            let settings = folio_field!(f, "settings", Vec<StudioNodeRenderSetting>, SETTINGS)?;
            let (node, _) = viewer.export_node(&key).map_err(refused)?;
            let before = viewer.run().ops().len();
            viewer.set_export_settings(&node, settings).map_err(|m| method_refusal(viewer, before, m))?;
            Ok(viewer.run().ops().last().map_or(Value::Null, |op| op["detail"].clone()))
        }
        "export.run" => {
            let dir = f.path("dir")?;
            let nodes = if f.has("nodes") { Some(f.list("nodes")?) } else { None };
            let settings = if f.has("settings") { Some(folio_field!(f, "settings", Vec<StudioNodeRenderSetting>, SETTINGS)?) } else { None };
            let overwrite = f.flag_or("overwrite", false)?;
            viewer.export_run(&dir, nodes.as_deref(), settings, overwrite).map_err(refused)
        }
        _ => Err(CommandRefusal::new("command_not_driveable", json!({"command_id": id}))),
    }
}
