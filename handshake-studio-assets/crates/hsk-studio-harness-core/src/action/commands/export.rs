//! Export rows of the command corpus (lane SMB-ARTBOARD-EXPORT-1; [STU-IO-010] single-target export per
//! layer and per artboard, [STU-IO-111] to [STU-IO-114], [STU-IO-117]): the node export settings and the
//! run that writes the files of many nodes and settings at once. Lane SMB-DELIVERY-QUEUE-1 adds the delivery
//! queue and the user export recipes.

use super::super::ValueType;
use super::{CommandEntry, ParamSpec, entry, flag as b, list as l, number as n, one_of, record as r, shaped, text as t, typed};

/// The Folio `StudioNodeRenderSetting` list (wire form).
const SETTINGS: &str = "[{format: PNG|JPG|SVG|PDF, suffix, contents_only, use_absolute_bounds, colour_profile: DOCUMENT|SRGB|DISPLAY_P3_V4, constraint: {type: SCALE|WIDTH|HEIGHT, value} (PNG and JPG only), outline_text, include_id_attribute, simplify_stroke (SVG only)}, ...]";

/// Parameter edits of `export.recipe_save`: identifier to value, `null` removes the identifier.
const PARAMETERS: &str = "{<export parameter identifier, e.g. ADBEVideoWidth>: integer|float|boolean|text|null, ...}";

/// A folder the command writes files into; each file is guarded by the command itself (never the opened
/// source, never an existing file without `overwrite`).
const fn output_dir(key: &'static str) -> ParamSpec {
    shaped(key, ValueType::String, "output_dir")
}

pub(super) const ROWS: &[CommandEntry] = &[
    // A layer or artboard by id or by its unique name; an empty list removes the node's settings.
    typed("export.set_settings", "Viewer::set_export_settings", "Export settings", true, &[t("node"), r("settings", SETTINGS)]),
    // Without `settings`: every stored node (or the named ones) with its stored settings. With `settings`:
    // those settings on every artboard (or the named nodes), nothing stored.
    typed("export.run", "Viewer::export_run", "Export", false, &[output_dir("dir"), l("nodes").opt(), r("settings", SETTINGS).opt(), b("overwrite").opt()]),
    // Review F6: the Run method behind `export.set_settings` (one entry-only undo step), not recordable,
    // as `artboard.set_options` is the Run row of its edit.
    entry("document.set_export_settings", "set_export_settings", "Set node export settings", false, true, &[]),
    // Lane SMB-DELIVERY-QUEUE-1 ([STU-VID-063], [STU-VID-064], [STU-MOT-124], [STU-MOT-125]): one headless
    // delivery queue (per Viewer, not saved) of sequence recipe items and composition render queue items, one
    // receipt per item and module; `output` is relative to the save root. Not document edits: the queue is
    // viewer state and a run writes new files only.
    typed(
        "delivery.add",
        "Viewer::delivery_add",
        "Add to delivery queue",
        false,
        &[one_of("kind", "sequence|composition"), t("sequence_id").opt(), t("recipe_id").opt(), n("in_tick").opt(), n("out_tick").opt(), n("width").opt(), n("height").opt(), n("bit_depth").opt(), t("output").opt(), t("item_id").opt(), b("overwrite").opt()],
    ),
    typed("delivery.remove", "Viewer::delivery_remove", "Remove from delivery queue", false, &[n("delivery_id")]),
    typed("delivery.list", "Viewer::delivery_list", "Delivery queue", false, &[]),
    typed("delivery.run", "Viewer::delivery_run", "Run delivery queue", false, &[]),
    typed("delivery.cancel", "Viewer::delivery_cancel", "Cancel delivery item", false, &[n("delivery_id")]),
    typed("delivery.receipts", "Viewer::delivery_receipts", "Delivery receipts", false, &[]),
    // User export recipes, the same record as a shipped preset ([STU-VID-063]): saved from a preset or recipe
    // id plus parameter edits, each change one entry-only undo step.
    typed("export.recipe_save", "Viewer::export_recipe_save", "Save export recipe", true, &[t("from"), t("recipe_id"), t("name").opt(), r("parameters", PARAMETERS).opt()]),
    typed("export.recipe_delete", "Viewer::export_recipe_delete", "Delete export recipe", true, &[t("recipe_id")]),
    typed("export.recipes", "Viewer::export_recipes", "Export recipes", false, &[]),
    // The Run method behind both recipe rows (one entry-only undo step), not recordable.
    entry("document.set_export_recipes", "set_export_recipes", "Set user export recipes", false, true, &[]),
];
