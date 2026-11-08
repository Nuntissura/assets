//! Export rows of the command corpus (lane SMB-ARTBOARD-EXPORT-1; [STU-IO-010] single-target export per
//! layer and per artboard, [STU-IO-111] to [STU-IO-114], [STU-IO-117]): the node export settings and the
//! run that writes the files of many nodes and settings at once.

use super::super::ValueType;
use super::{CommandEntry, ParamSpec, entry, flag as b, list as l, record as r, shaped, text as t, typed};

/// The Folio `StudioNodeRenderSetting` list (wire form).
const SETTINGS: &str = "[{format: PNG|JPG|SVG|PDF, suffix, contents_only, use_absolute_bounds, colour_profile: DOCUMENT|SRGB|DISPLAY_P3_V4, constraint: {type: SCALE|WIDTH|HEIGHT, value} (PNG and JPG only), outline_text, include_id_attribute, simplify_stroke (SVG only)}, ...]";

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
];
