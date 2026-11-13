//! The raster Layout Preset as data (SMB-GUI-FRAME-1), transcribed from the designer's layouts.json (presets.raster,
//! tool_rail.groups, panel_minimums.panels, fit_rules.menu_abbreviations) and the Master Spec: menu titles from
//! STU-SHL-011 (14:31521), tool names from the STU-SHL-155 rows (14:32661-32977), the raster centre and inspector
//! groups from STU-SHL-085 (14:32216). Panel icons follow the designer's mock (tools/mock_lib.py PANEL_ICON).
use super::frame::{EdgeState, Edges};

/// The named shape of this preset (layouts.json `presets.raster.named_shape`).
pub const PRESET: &str = "raster";
/// The default Layout Preset name (OD-4 provisional).
pub const LAYOUT_PRESET: &str = "Photo";

/// STU-SHL-011: the nineteen titles in the closed order.
pub const MENU: [&str; 19] = [
    "FILE", "EDIT", "VIEW", "DOCUMENT", "SELECT", "OBJECT", "TYPE", "COLOR", "EFFECTS", "MOTION", "INSERT", "CODE", "AUTOMATE", "WORKSPACE", "GO", "RUN", "HELP", "EDITORS",
    "OPERATOR",
];

/// fit_rules.menu_abbreviations (FIT-M1); titles not listed keep their text.
pub fn abbreviation(title: &str) -> &str {
    match title {
        "DOCUMENT" => "DOC",
        "EFFECTS" => "FX",
        "AUTOMATE" => "AUTO",
        "WORKSPACE" => "WKSP",
        "OPERATOR" => "OPER",
        "EDITORS" => "EDIT.",
        "OBJECT" => "OBJ",
        "SELECT" => "SEL",
        "INSERT" => "INS",
        other => other,
    }
}

/// One Tool Rail slot: tool_rail.groups `group_id`, `face_icon`, the group's first tool (the face tool) and its
/// Studio name (STU-SHL-155), and whether the group has more than one tool (the variant marker).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RailGroup {
    pub group: &'static str,
    pub icon: &'static str,
    pub face_tool: &'static str,
    pub name: &'static str,
    pub variants: bool,
}

const fn g(group: &'static str, face_tool: &'static str, name: &'static str) -> RailGroup {
    RailGroup { group, icon: group, face_tool, name, variants: true }
}

/// presets.raster.rail, in order (every raster group has more than one tool).
pub const RAIL: [RailGroup; 19] = [
    g("select", "select", "Select"),
    g("smart_select", "object_selection", "Object Selection"),
    g("marquee", "rectangular_marquee", "Rectangular Marquee"),
    g("lasso", "lasso", "Lasso"),
    g("wand", "magic_wand", "Magic Wand"),
    g("move", "move", "Move"),
    g("crop", "crop", "Crop"),
    g("paint", "paint_brush", "Paint Brush"),
    g("erase", "erase_brush", "Erase Brush"),
    g("retouch", "healing_brush", "Healing Brush"),
    g("tone", "dodge_brush", "Dodge Brush"),
    g("mask", "mask_paint", "Mask Paint"),
    g("fill", "gradient", "Gradient"),
    g("pen", "pen", "Pen"),
    g("shape", "rectangle", "Rectangle"),
    g("type", "type", "Type"),
    g("warp", "warp", "Warp"),
    g("measure", "measure", "Measure"),
    g("navigate", "hand", "Hand"),
];

/// The STU-MDL-103 id of a rail group: `studio.toolgroup.<group id in kebab case>` (the designer's
/// `studio.toolgroup.smart_select` fails the id charset; fixed in layouts.json on the lead's ruling of 2026-11-08).
pub fn slot_id(group: &str) -> String {
    format!("studio.toolgroup.{}", group.replace('_', "-"))
}

/// A panel: its id (panel_minimums), display name, and the designer's rail icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelEntry {
    pub id: &'static str,
    pub name: &'static str,
    pub icon: &'static str,
}

const fn p(id: &'static str, name: &'static str, icon: &'static str) -> PanelEntry {
    PanelEntry { id, name, icon }
}

/// presets.raster.left (state rail): the Browse Dock's panels.
pub const LEFT: [PanelEntry; 6] = [
    p("brushes", "Brushes", "brushes"),
    p("preset-library", "Preset Library", "presets"),
    p("library", "Library", "library"),
    p("project-bin", "Project Bin", "project"),
    p("effects-browser", "Effects Browser", "effects"),
    p("actions-macros", "Actions & Macros", "actions"),
];

/// The Browse Dock's one group when FIT-L1 expands it: the left panels as tabs (panel id, display name).
pub const LEFT_TABS: [(&str, &str); 6] = [
    ("brushes", "Brushes"),
    ("preset-library", "Preset Library"),
    ("library", "Library"),
    ("project-bin", "Project Bin"),
    ("effects-browser", "Effects Browser"),
    ("actions-macros", "Actions & Macros"),
];

/// presets.raster.bottom (state rail): the Time & Results Dock's panels (Jobs and Problems are shell panes).
pub const BOTTOM: [PanelEntry; 5] = [
    p("keyframe-timeline", "Keyframe Timeline", "diamond"),
    p("find-replace", "Find & Replace", "find"),
    p("preflight", "Preflight", "preflight"),
    p("jobs", "Jobs", "jobs"),
    p("problems", "Problems", "problems"),
];

/// One Inspect Dock group: its share of the dock height and its tabs (panel id, display name) in STU-SHL-085 order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroupPreset {
    pub share: f32,
    pub tabs: &'static [(&'static str, &'static str)],
}

/// presets.raster.right (expanded, 300).
pub const RIGHT: [GroupPreset; 3] = [
    GroupPreset { share: 0.3, tabs: &[("properties", "Properties"), ("adjustments", "Adjustments"), ("effect-controls", "Effect Controls"), ("masks", "Masks")] },
    GroupPreset { share: 0.24, tabs: &[("colour", "Colour"), ("swatches", "Swatches"), ("scopes", "Scopes"), ("info", "Info"), ("navigator", "Navigator")] },
    GroupPreset { share: 0.46, tabs: &[("layers", "Layers"), ("channels", "Channels"), ("paths", "Paths"), ("history", "History")] },
];

/// The raster preset's edge row for the frame layout.
pub fn raster_edges() -> Edges {
    Edges {
        left: EdgeState::Rail,
        left_w: 260.0,
        right: EdgeState::Expanded,
        right_w: 300.0,
        shares: RIGHT.iter().map(|g| g.share).collect(),
        right_foot: false,
        meter: false,
        bottom: EdgeState::Rail,
        bottom_h: 240.0,
        time_panel: true,
        viewers: 1,
        slots: RAIL.len(),
    }
}

/// Studio Default chords by command id (components.json keyboard: PROVISIONAL, STU-SHL-045 not frozen), the chords
/// [`super::GuiFrame`] binds. A menu row and an L0 tip resolve the chord here at render from the leaf's command id
/// (STU-SHL-242: never baked into a label).
pub const SHORTCUTS: [(&str, &str); 2] = [("history.undo", "Ctrl+Z"), ("history.redo", "Ctrl+Shift+Z")];

/// The chord bound to `command_id` in the active set, if any.
pub fn chord_of(command_id: &str) -> Option<&'static str> {
    SHORTCUTS.iter().find(|(c, _)| *c == command_id).map(|(_, k)| *k)
}

/// The Layers context menu (STU-SHL-014: references to menu-bar leaf ids, never homes), in order.
pub const LAYER_CONTEXT: [&str; 8] = [
    "studio.object.layer.duplicate_layer",
    "studio.object.layer.delete_layer",
    "studio.object.hide.hide_selection",
    "studio.object.arrange.bring_forward",
    "studio.object.arrange.send_backward",
    "studio.object.group",
    "studio.object.ungroup",
    "studio.object.layer.new",
];
