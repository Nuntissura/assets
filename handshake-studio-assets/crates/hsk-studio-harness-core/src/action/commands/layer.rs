//! Layer rows of the command corpus.

use super::super::{UnitToken, ValueType};
use super::{CommandEntry, ParamSpec, entry, p};

pub const CMD_RENAME: &str = "layer.rename";
pub const CMD_SET_VISIBLE: &str = "layer.set_visible";
pub const CMD_SET_OPACITY: &str = "layer.set_opacity";
pub const CMD_SET_BLEND_MODE: &str = "layer.set_blend_mode";
pub const CMD_PANEL_SET: &str = "layer.panel_set";
pub const CMD_RESTRUCTURE: &str = "layer.restructure";

const LAYER: ParamSpec = p("layer", ValueType::Reference);
const NAME: ParamSpec = p("name", ValueType::String);
const VISIBLE: ParamSpec = p("visible", ValueType::Boolean);
const OPACITY: ParamSpec = ParamSpec { unit: Some(UnitToken::Percent), ..p("opacity", ValueType::Measured) };
const BLEND_MODE: ParamSpec = p("blend_mode", ValueType::Number);
const CONTROL: ParamSpec = p("control", ValueType::String);
const VALUE: ParamSpec = p("value", ValueType::String);
const OP: ParamSpec = p("op", ValueType::String);
const WRITES: ParamSpec = p("writes", ValueType::List);

pub(super) const ROWS: &[CommandEntry] = &[
    entry(CMD_RENAME, "rename", "Rename layer", true, true, &[LAYER, NAME]),
    entry(CMD_SET_VISIBLE, "set_visible", "Show or hide layer", true, true, &[LAYER, VISIBLE]),
    entry(CMD_SET_OPACITY, "set_opacity", "Set layer opacity", true, true, &[LAYER, OPACITY]),
    // The blend mode is the StudioBlendMode discriminant: no member-name source exists in
    // harness-core's dependencies (names live in Pigment), so it records as a bare `number`.
    entry(CMD_SET_BLEND_MODE, "set_blend_mode", "Set layer blend mode", true, true, &[LAYER, BLEND_MODE]),
    entry(CMD_PANEL_SET, "panel_set", "Set panel control", true, true, &[LAYER, CONTROL, VALUE]),
    // Placement-only writes; a restructure that creates a layer carries a record payload and is
    // reported not_recordable at record time.
    entry(CMD_RESTRUCTURE, "restructure", "Restructure layers", true, true, &[OP, WRITES]),
    entry("layer.restructure_and_entry", "restructure_and_entry", "Restructure layers with document entry", false, true, &[]),
    entry("layer.set_tiles", "set_tiles", "Replace raster tiles", false, true, &[]),
    entry("layer.set_tiles_and_entry", "set_tiles_and_entry", "Replace raster tiles with document entry", false, true, &[]),
    entry("layer.set_vector_paths", "set_vector_paths", "Replace vector paths", false, true, &[]),
    entry("layer.set_vector_paths_and_entry", "set_vector_paths_and_entry", "Replace vector paths with document entry", false, true, &[]),
    entry("layer.panel", "panel", "Layer panel", false, false, &[LAYER]),
];
