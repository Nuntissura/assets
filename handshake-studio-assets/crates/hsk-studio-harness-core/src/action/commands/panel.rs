//! Hosted panel rows of the command corpus: every `PanelAction`, addressed `panel.<panel key>.<action>`,
//! run through the same `Viewer::hosted_action` the panel's control calls.

use super::{CommandEntry, ParamSpec, flag as b, list as l, number as n, one_of as o, output_path as out, path as p, record as r, rgb8, text as t, typed};

const HOSTED: &str = "Viewer::hosted_action";
const STROKE: &str = "{paint, weight, cap, join, miter_limit, dash, dash_offset, align}; a paint is {kind: solid|linear|radial, ...}";
const MARKERS: &str = "{start, end, alignment, start_scale, end_scale}";
const ALIGN_HOW: &str = "left|centre_horizontal|right|top|centre_vertical|bottom";
const REFERENCE: &str = "top_left|top|top_right|left|centre|right|bottom_left|bottom|bottom_right";
const TO: &[ParamSpec] = &[l("layers"), o("to", "selection|artboard|key_object"), t("key_object").opt(), o("how", ALIGN_HOW)];
const DISTRIBUTE: &[ParamSpec] = &[l("layers"), o("to", "selection|artboard|key_object"), t("key_object").opt(), o("how", "left|centre_horizontal|right|top|centre_vertical|bottom|spacing"), n("axis").opt(), n("gap").opt()];
const OUT: &[ParamSpec] = &[out("path"), b("overwrite").opt()];

pub(super) const ROWS: &[CommandEntry] = &[
    typed("panel.appearance.set_fill", HOSTED, "Set solid fill", true, &[rgb8("rgb8").opt()]),
    typed("panel.appearance.clear_appearance", HOSTED, "Clear appearance", true, &[]),
    typed("panel.appearance.reduce_to_basic_appearance", HOSTED, "Reduce to basic appearance", true, &[]),
    typed("panel.stroke.set_stroke", HOSTED, "Set stroke", true, &[r("stroke", STROKE).opt()]),
    typed("panel.stroke.set_stroke_markers", HOSTED, "Set arrowheads", true, &[r("markers", MARKERS).opt()]),
    typed("panel.stroke.swap_stroke_markers", HOSTED, "Swap arrowheads", true, &[]),
    typed("panel.swatches.add_swatch", HOSTED, "Add swatch", true, &[t("name"), rgb8("rgb8")]),
    typed("panel.swatches.apply_swatch", HOSTED, "Apply swatch", true, &[t("swatch"), o("kind", "fill|stroke"), n("tint").opt()]),
    typed("panel.swatches.detach_swatch", HOSTED, "Detach swatch", true, &[o("kind", "fill|stroke")]),
    typed("panel.graphic_styles.add_graphic_style", HOSTED, "New graphic style", true, &[t("name")]),
    typed("panel.graphic_styles.apply_graphic_style", HOSTED, "Apply graphic style", true, &[t("style")]),
    typed("panel.graphic_styles.merge_graphic_style", HOSTED, "Merge graphic style", true, &[t("style")]),
    typed("panel.graphic_styles.redefine_graphic_style", HOSTED, "Redefine graphic style", true, &[t("style")]),
    typed("panel.graphic_styles.break_graphic_style", HOSTED, "Break graphic style link", true, &[]),
    typed("panel.align_distribute.align", HOSTED, "Align", true, TO),
    typed("panel.align_distribute.distribute", HOSTED, "Distribute", true, DISTRIBUTE),
    typed("panel.transform.transform", HOSTED, "Transform", true, &[l("layers"), r("op", "{kind: rotate|scale|reflect|shear|move, ...}"), o("reference", REFERENCE).opt()]),
    typed("panel.transform.transform_each", HOSTED, "Transform each", true, &[l("layers"), r("each", "{scale_horizontal, scale_vertical, move_horizontal, move_vertical, rotate, reflect_x, reflect_y, reference, randomize, seed}")]),
    typed("panel.path_operations.offset_path", HOSTED, "Offset path", true, &[n("offset"), o("join", "miter|round|bevel")]),
    typed("panel.path_operations.outline_stroke", HOSTED, "Outline stroke", true, &[]),
    typed("panel.path_operations.expand_shape", HOSTED, "Expand shape", true, &[]),
    typed("panel.path_operations.promote_network", HOSTED, "Promote to network", true, &[]),
    typed("panel.path_operations.edit_path", HOSTED, "Edit path", true, &[r("edit", "{kind: insert_anchor|delete_anchor|convert|move_anchor|move_handle|bend|split_at|split_at_anchor|join|reverse|corner, ...}")]),
    typed("panel.path_operations.bind_live_text", HOSTED, "Type on path", true, &[t("text"), o("kind", "path|area"), n("inset_pt").opt()]),
    typed("panel.path_operations.convert_text_kind", HOSTED, "Convert text kind", true, &[]),
    typed("panel.path_operations.create_outlines", HOSTED, "Create outlines", true, &[]),
    typed("panel.masks.make_clip", HOSTED, "Make clip", true, &[t("source"), b("even_odd").opt()]),
    typed("panel.masks.make_mask", HOSTED, "Make mask", true, &[t("source"), o("mode", "ALPHA|VECTOR|LUMINANCE"), b("invert").opt()]),
    typed("panel.masks.release_masks", HOSTED, "Release masks", true, &[]),
    typed("panel.envelope_distort.make_envelope", HOSTED, "Make envelope", true, &[r("spec", "{kind: warp|mesh|top_object, ...}"), n("fidelity").opt(), b("distort_appearance").opt()]),
    typed("panel.envelope_distort.release_envelope", HOSTED, "Release envelope", true, &[]),
    typed("panel.envelope_distort.expand_envelope", HOSTED, "Expand envelope", true, &[]),
    typed("panel.brushes.add_brush", HOSTED, "New brush", true, &[t("name"), o("kind", "art|scatter|pattern"), o("colorization", "none|tints|tints_and_shades|hue_shift"), l("layers")]),
    typed("panel.brushes.apply_brush", HOSTED, "Apply brush", true, &[t("brush"), n("scale").opt()]),
    typed("panel.brushes.remove_brush", HOSTED, "Remove brush", true, &[]),
    typed("panel.vector_export.export_svg", HOSTED, "Export SVG", false, OUT),
    typed("panel.vector_export.export_pdf", HOSTED, "Export PDF", false, OUT),
    typed("panel.vector_import.import_vector", HOSTED, "Import vector artwork", false, &[p("source"), out("dest"), b("overwrite").opt()]),
    typed("panel.audio_batch.batch_add", HOSTED, "Queue audio batch item", false, &[p("input"), out("output"), n("gain_db").opt(), n("bits").opt(), o("format", "flac|aiff|aiff_float"), b("overwrite").opt()]),
    typed("panel.audio_batch.batch_run", HOSTED, "Run audio batch", false, &[]),
    typed("panel.audio_batch.batch_clear", HOSTED, "Clear audio batch", false, &[]),
];
