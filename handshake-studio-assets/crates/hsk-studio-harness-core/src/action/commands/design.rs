//! Design rows of the command corpus: auto layout and grid records, and design variables.

use super::{CommandEntry, ParamSpec, flag as b, index as i, list as l, number as n, one_of as o, output_path as out, path as file, record as r, text as t, typed};

const SIZING: &str = "FIXED|HUG|FILL";
const INSTANCE: &str = "Viewer::design_command";
const TRACK_MOVE: &[ParamSpec] = &[t("container"), i("from_index"), i("to_index")];
/// Lane SMB-DS-VARIABLES-AUTHOR-1 ([STU-DS-132], [STU-DS-133]): every variable row runs `Viewer::variable_command`.
const VARIABLE: &str = "Viewer::variable_command";
/// [STU-DS-125], closed.
const RESOLVED_TYPES: &str = "BOOLEAN|COLOR|EASING|FLOAT|STRING|TIMING";
/// [STU-DS-129], closed: a fourth platform is refused typed.
const PLATFORMS: &str = "WEB|ANDROID|iOS";
/// [STU-DS-128], closed.
const SCOPES: &str = "ALL_SCOPES|ALL_FILLS|FRAME_FILL|SHAPE_FILL|TEXT_FILL|STROKE_COLOR|STROKE_FLOAT|EFFECT_COLOR|EFFECT_FLOAT|CORNER_RADIUS|GAP|WIDTH_HEIGHT|OPACITY|FONT_FAMILY|FONT_SIZE|FONT_STYLE|FONT_WEIGHT|LETTER_SPACING|LINE_HEIGHT|PARAGRAPH_INDENT|PARAGRAPH_SPACING|TEXT_CONTENT";
const VALUE: &str = "a value record ([STU-DS-127]): {type, value} or {type: VARIABLE_ALIAS, id, resolved_type}";

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
    // Lane SMB-DS-VARIABLES-AUTHOR-1: collections, modes, variables, aliases, code syntax and explicit modes
    // ([STU-DS-121..133]); one undo step per mutating row, the records in the vector entry's `design` member.
    typed("variable_collection.create", VARIABLE, "Create variable collection", true, &[t("name"), t("mode_name").opt()]),
    typed("variable_collection.add_mode", VARIABLE, "Add mode", true, &[t("collection"), t("name")]),
    typed("variable_collection.rename_mode", VARIABLE, "Rename mode", true, &[t("collection"), t("mode"), t("name")]),
    typed("variable_collection.remove_mode", VARIABLE, "Remove mode", true, &[t("collection"), t("mode")]),
    typed("variable_collection.remove", VARIABLE, "Remove variable collection", true, &[t("collection")]),
    typed("variable.create", VARIABLE, "Create variable", true, &[t("collection"), t("name"), o("resolved_type", RESOLVED_TYPES), r("value", VALUE), l("scopes").opt(), t("description").opt(), b("hidden_from_publishing").opt()]),
    typed("variable.remove", VARIABLE, "Remove variable", true, &[t("variable")]),
    typed("variable.set_value_for_mode", VARIABLE, "Set variable value", true, &[t("variable"), t("mode"), r("value", VALUE)]),
    typed("variable.set_code_syntax", VARIABLE, "Set code syntax", true, &[t("variable"), o("platform", PLATFORMS), t("name")]),
    typed("variable.remove_code_syntax", VARIABLE, "Remove code syntax", true, &[t("variable"), o("platform", PLATFORMS)]),
    typed("variable.create_alias", VARIABLE, "Create variable alias", false, &[t("variable")]),
    typed("variable.resolve_for_consumer", VARIABLE, "Resolve variable", false, &[t("variable"), t("node"), o("scope", SCOPES).opt()]),
    typed("node.set_explicit_variable_mode_for_collection", VARIABLE, "Set variable mode", true, &[t("node"), t("collection"), t("mode")]),
    typed("node.clear_explicit_variable_mode_for_collection", VARIABLE, "Clear variable mode", true, &[t("node"), t("collection")]),
];
