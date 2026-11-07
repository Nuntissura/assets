//! Design rows of the command corpus: auto layout and grid records.

use super::{CommandEntry, ParamSpec, index as i, number as n, one_of as o, record as r, text as t, typed};

const SIZING: &str = "FIXED|HUG|FILL";
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
];
