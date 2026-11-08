//! Vector rows of the command corpus: the Viewer methods that take a whole typed record.

use super::{CommandEntry, ParamSpec, flag as b, index as i, list as l, number as n, one_of as o, path as p, record as r, rgb8, text as t, typed};

const PAINT: &str = "{kind: solid, rgb8} | {kind: linear, from, to, stops} | {kind: radial, centre, radius, stops}; a stop is {position, rgb8, opacity}";
const STROKE: &str = "{paint, weight, cap, join, miter_limit, dash, dash_offset, align}";
const SWATCH: &str = "{kind: process|global, colour} | {kind: spot, ink, measured_lab, alternate} | {kind: tint, base, percent} | {kind: gradient, paint}; a colour is {model: rgb|cmyk|lab, components} or {model: gray, gray}";
const PATH_EDIT: &str = "{kind: insert_anchor|delete_anchor|convert|move_anchor|move_handle|bend|split_at|split_at_anchor|join|reverse|corner, ...}";
const TO: &[ParamSpec] = &[l("layers"), o("to", "selection|artboard|key_object"), t("key_object").opt(), o("how", "left|centre_horizontal|right|top|centre_vertical|bottom")];
const DISTRIBUTE: &[ParamSpec] = &[l("layers"), o("to", "selection|artboard|key_object"), t("key_object").opt(), o("how", "left|centre_horizontal|right|top|centre_vertical|bottom|spacing"), n("axis").opt(), n("gap").opt()];
const REFERENCE: &str = "top_left|top|top_right|left|centre|right|bottom_left|bottom|bottom_right";
/// Lane SMB-AI-LIVE-EFFECTS-1 (STU-FX-007, STU-VEC-154): one typed live-effect parameter.
const LIVE_PARAM: &str = "{type: Int|Real|Bool|String, key, value}";
const LIVE_PARAMS: &str = "[{type: Int|Real|Bool|String, key, value}, ...] over the table defaults";

pub(super) const ROWS: &[CommandEntry] = &[
    typed("vector.set_fill", "Viewer::set_fill", "Set fill paint", true, &[t("layer"), r("paint", PAINT).opt()]),
    typed("vector.set_stroke", "Viewer::set_stroke", "Set stroke", true, &[t("layer"), r("stroke", STROKE).opt()]),
    typed("vector.set_stroke_markers", "Viewer::set_stroke_markers", "Set arrowheads of a stroke row", true, &[t("layer"), i("row"), r("markers", "{start, end, alignment, start_scale, end_scale}").opt()]),
    typed("vector.add_swatch", "Viewer::add_swatch", "Add paint swatch", true, &[t("name"), r("paint", PAINT)]),
    typed("vector.edit_swatch", "Viewer::edit_swatch", "Edit paint swatch", true, &[t("swatch"), r("paint", PAINT)]),
    typed("vector.add_folio_swatch", "Viewer::add_folio_swatch", "Add swatch", true, &[t("name"), r("spec", SWATCH)]),
    typed("vector.edit_folio_swatch", "Viewer::edit_folio_swatch", "Edit swatch", true, &[t("swatch"), r("spec", SWATCH)]),
    typed("vector.edit_path", "Viewer::edit_path", "Edit path", true, &[t("layer"), r("edit", PATH_EDIT)]),
    typed("vector.edit_network", "Viewer::edit_network", "Edit vector network", true, &[t("layer"), r("edit", "{kind: move_vertex|add_vertex|connect|delete_segment|tangents, ...}")]),
    typed("vector.edit_gradient_mesh", "Viewer::edit_gradient_mesh", "Edit gradient mesh", true, &[t("layer"), r("edit", "{kind: set_point|move_point|insert_column|insert_row|remove_column|remove_row, ...}")]),
    typed("vector.set_live_shape", "Viewer::set_live_shape", "Set live shape parameters", true, &[t("layer"), r("params", "a Folio StudioShapeParameters record")]),
    typed("vector.make_envelope", "Viewer::make_envelope", "Make envelope", true, &[t("layer"), r("spec", "{kind: warp|mesh|top_object, ...}"), n("fidelity").opt(), b("distort_appearance").opt()]),
    typed("vector.make_vector_mask", "Viewer::make_vector_mask", "Make vector mask", true, &[t("target"), t("source"), o("mode", "ALPHA|VECTOR|LUMINANCE"), b("invert").opt()]),
    typed("vector.align_layers", "Viewer::align_layers", "Align layers", true, TO),
    typed("vector.distribute_layers", "Viewer::distribute_layers", "Distribute layers", true, DISTRIBUTE),
    typed("vector.transform_layers", "Viewer::transform_layers", "Transform layers", true, &[l("layers"), r("op", "{kind: rotate|scale|reflect|shear|move, ...}"), o("reference", REFERENCE).opt()]),
    typed("vector.transform_each_layers", "Viewer::transform_each_layers", "Transform each layer", true, &[l("layers"), r("each", "{scale_horizontal, scale_vertical, move_horizontal, move_vertical, rotate, reflect_x, reflect_y, reference, randomize, seed}")]),
    typed("vector.recolor_artwork", "Viewer::recolor_artwork", "Recolor artwork", true, &[l("layers"), r("table", "{rows: [{current, new}], method, preserve_white, preserve_black, preserve_grays}"), r("adjust", "{hue_shift_deg, saturation_shift, brightness_shift, temperature_shift, luminosity_shift}").opt()]),
    typed("vector.bind_live_text_with_font", "Viewer::bind_live_text_with_font", "Type on path with font", true, &[t("layer"), t("text"), o("kind", "path|area"), n("inset_pt").opt(), t("family")]),
    typed("vector.text_on_path_to_outlines", "Viewer::text_on_path_to_outlines", "Text on path to outlines", true, &[t("layer"), t("text"), t("family"), n("size_pt")]),
    typed("vector.place_text_with_font", "Viewer::place_text_with_font", "Place text with font", true, &[n("x"), n("y"), t("text"), t("family")]),
    typed("vector.add_font", "Viewer::add_font", "Add font file", false, &[t("family"), p("path")]),
    typed("vector.set_document_cmyk_profile", "Viewer::set_document_cmyk_profile", "Set document CMYK profile", true, &[t("name"), p("path")]),
    typed("vector.layer_bounds", "Viewer::layer_bounds", "Layer bounds", false, &[t("layer")]),
    typed("vector.live_filter_apply", "Viewer::apply_live_filter", "Apply live effect", true, &[t("layer"), t("filter_kind"), r("params", LIVE_PARAMS).opt()]),
    typed("vector.live_filter_update", "Viewer::update_live_filter", "Update live effect parameter", true, &[t("layer"), t("instance"), r("param", LIVE_PARAM)]),
    typed("vector.live_filter_reorder", "Viewer::reorder_live_filter", "Reorder live effect", true, &[t("layer"), t("instance"), i("to")]),
    typed("vector.live_filter_toggle", "Viewer::toggle_live_filter", "Enable or disable live effect", true, &[t("layer"), t("instance"), b("enabled")]),
    typed("vector.live_filter_remove", "Viewer::remove_live_filter", "Remove live effect", true, &[t("layer"), t("instance")]),
    typed("vector.live_filter_bake", "Viewer::bake_live_filters", "Bake live effects", true, &[t("layer")]),
    typed("vector.live_filter_read", "Viewer::live_filters", "Read live effects", false, &[t("layer")]),
    typed("vector.set_tool_options", "Viewer::set_vector_tool", "Set vector tool options", false, &[rgb8("fill").opt(), b("fill_on").opt(), rgb8("stroke").opt(), b("stroke_on").opt(), n("stroke_width").opt(), n("corner_radius").opt(), i("polygon_sides").opt(), i("star_points").opt(), n("star_inner").opt()]),
];
