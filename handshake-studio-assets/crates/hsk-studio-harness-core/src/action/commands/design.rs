//! Design rows of the command corpus: auto layout and grid records.

use super::{CommandEntry, ParamSpec, flag as b, index as i, number as n, one_of as o, output_path as out, path as file, record as r, text as t, typed};

const SIZING: &str = "FIXED|HUG|FILL";
const INSTANCE: &str = "Viewer::design_command";
const TRACK_MOVE: &[ParamSpec] = &[t("container"), i("from_index"), i("to_index")];

pub(super) const ROWS: &[CommandEntry] = &[
    typed("design.set_auto_layout_mode", "Viewer::set_auto_layout_mode", "Set auto layout mode", true, &[t("node"), o("mode", "NONE|HORIZONTAL|VERTICAL|GRID")]),
    typed("design.set_auto_layout_spacing", "Viewer::set_auto_layout_spacing", "Set auto layout spacing", true, &[t("node"), n("item_spacing"), r("padding", "[top, right, bottom, left]")]),
    typed("design.set_child_sizing", "Viewer::set_child_sizing", "Set child sizing", true, &[t("node"), o("horizontal", SIZING), o("vertical", SIZING)]),
    typed("design.set_grid_child", "Viewer::set_grid_child", "Set grid child placement", true, &[t("node"), r("placement", "a Folio StudioGridChild record").opt()]),
    typed("design.set_grid_options", "Viewer::set_grid_options", "Set grid options", true, &[t("node"), o("auto_tracks", "NONE|ROWS"), o("positioning", "MANUAL|ROW_AUTO_FLOW")]),
    typed("design.set_grid_tracks", "Viewer::set_grid_tracks", "Set grid tracks", true, &[t("node"), r("rows", "a list of Folio StudioGridTrackSize records"), r("columns", "a list of Folio StudioGridTrackSize records")]),
    typed("design.grid_reorder_rows", "Viewer::grid_reorder_rows", "Reorder grid rows", true, TRACK_MOVE),
    typed("design.grid_reorder_columns", "Viewer::grid_reorder_columns", "Reorder grid columns", true, TRACK_MOVE),
    // Lane SMB-DRIVE-TYPED-2 (STU-DS-008/009/016/017/162): a design file opened as a native document, and
    // every `DesignCommand` on the instance drawn by `layer` (one chronicle step each, `design_instance <layer>`).
    typed("design.import_fig", "Viewer::import_fig", "Open design file", false, &[file("source"), out("dest"), b("overwrite").opt()]),
    typed("design.instance.inspect", "Viewer::instance_view", "Inspect instance", false, &[t("layer")]),
    typed("design.instance.switch_variant", INSTANCE, "Switch variant", true, &[t("layer"), t("property"), t("value")]),
    typed("design.instance.set_property", INSTANCE, "Set component property", true, &[t("layer"), t("name"), o("kind", "boolean|text"), t("value")]),
    typed("design.instance.set_override", INSTANCE, "Override instance field", true, &[t("layer"), t("node_id"), o("field", "characters|visible|opacity"), t("value")]),
    typed("design.instance.reset_override", INSTANCE, "Reset override", true, &[t("layer"), t("node_id"), t("field")]),
    typed("design.instance.reset_all", INSTANCE, "Reset all overrides", true, &[t("layer")]),
    typed("design.instance.swap_component", INSTANCE, "Swap component", true, &[t("layer"), t("component_id")]),
];
