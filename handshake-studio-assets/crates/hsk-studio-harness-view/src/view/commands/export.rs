//! `export.*` (lane SMB-ARTBOARD-EXPORT-1): the node export settings and the export run (module
//! `export_set`). A node is a layer or artboard id, or a name exactly one node has. Lane SMB-DELIVERY-QUEUE-1:
//! `delivery.*` (the delivery queue, module `delivery`) and the user export recipes `export.recipe_save`,
//! `export.recipe_delete`, `export.recipes`; a build without feature `media` answers them `feature_unavailable`.
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
        #[cfg(feature = "media")]
        "delivery.add" | "delivery.remove" | "delivery.list" | "delivery.run" | "delivery.cancel" | "delivery.receipts" | "export.recipe_save" | "export.recipe_delete" | "export.recipes" => delivery(viewer, id, f),
        #[cfg(not(feature = "media"))]
        "delivery.add" | "delivery.remove" | "delivery.list" | "delivery.run" | "delivery.cancel" | "delivery.receipts" | "export.recipe_save" | "export.recipe_delete" | "export.recipes" => {
            Err(CommandRefusal::new("feature_unavailable", json!({"feature": "media"})))
        }
        _ => Err(CommandRefusal::new("command_not_driveable", json!({"command_id": id}))),
    }
}

#[cfg(feature = "media")]
fn delivery(viewer: &mut Viewer, id: &str, f: Fields<'_>) -> Result<Value, CommandRefusal> {
    use super::super::delivery::{DeliveryItem, SequenceItem};
    use super::super::monitor::MonitorFail;
    let fail = |e: MonitorFail| CommandRefusal::new(e.code, json!({"message": e.detail}));
    let opt_u32 = |key: &str| -> Result<Option<u32>, CommandRefusal> {
        if f.has(key) { f.count(key).map(Some) } else { Ok(None) }
    };
    let opt_u64 = |key: &str| -> Result<Option<u64>, CommandRefusal> {
        if f.has(key) { f.unsigned(key).map(Some) } else { Ok(None) }
    };
    match id {
        "delivery.add" => {
            let item = match f.token("kind", &[("sequence", true), ("composition", false)])? {
                true => {
                    let mut s = SequenceItem::new(&f.text("sequence_id")?, &f.text("recipe_id")?, &f.text("output")?);
                    s.in_tick = opt_u64("in_tick")?;
                    s.out_tick = opt_u64("out_tick")?;
                    s.width = opt_u32("width")?;
                    s.height = opt_u32("height")?;
                    if let Some(bits) = opt_u32("bit_depth")? {
                        s.bit_depth = u8::try_from(bits).map_err(|_| super::decode::invalid("bit_depth", "8 or 10"))?;
                    }
                    s.overwrite = f.flag_or("overwrite", false)?;
                    DeliveryItem::Sequence(s)
                }
                false => DeliveryItem::Composition { item_id: f.text("item_id")?, overwrite: f.flag_or("overwrite", false)? },
            };
            let kind = if matches!(item, DeliveryItem::Sequence(_)) { "sequence" } else { "composition" };
            let id = viewer.delivery_add(item).map_err(fail)?;
            Ok(json!({"delivery_id": id, "kind": kind}))
        }
        "delivery.remove" => viewer.delivery_remove(f.unsigned("delivery_id")?).map(|()| Value::Null).map_err(fail),
        "delivery.cancel" => viewer.delivery_cancel(f.unsigned("delivery_id")?).map(|()| Value::Null).map_err(fail),
        "delivery.list" => Ok(viewer.delivery_list()),
        "delivery.receipts" => Ok(viewer.delivery_receipts()),
        "delivery.run" => viewer.delivery_run().map_err(fail),
        "export.recipe_save" => {
            let edits = if f.has("parameters") {
                match f.json("parameters", "an object of export parameter identifiers")? {
                    Value::Object(m) => m.into_iter().collect(),
                    _ => return Err(super::decode::invalid("parameters", "an object of export parameter identifiers")),
                }
            } else {
                Vec::new()
            };
            let before = viewer.run().ops().len();
            viewer.export_recipe_save(&f.text("from")?, &f.text("recipe_id")?, f.opt_text("name")?.as_deref(), &edits).map_err(|e| {
                if viewer.run().ops().len() > before { method_refusal(viewer, before, e.detail) } else { fail(e) }
            })
        }
        "export.recipe_delete" => {
            let before = viewer.run().ops().len();
            let (recipe_id, all) = (f.opt_text("recipe_id")?, f.flag_or("all", false)?);
            viewer.export_recipe_delete(recipe_id.as_deref(), all).map_err(|e| if viewer.run().ops().len() > before { method_refusal(viewer, before, e.detail) } else { fail(e) })
        }
        "export.recipes" => viewer.export_recipes().map_err(fail),
        _ => Err(CommandRefusal::new("command_not_driveable", json!({"command_id": id}))),
    }
}
