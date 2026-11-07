//! Photo rows of the command corpus: selection, fill, liquify, info and develop records.

use super::{CommandEntry, ParamSpec, flag as b, index as i, number as n, one_of as o, point, points, record as r, samples, text as t, typed, vector3};

const MODEL: &str = "rgb|hsb|lab|cmyk";
const INFO: &str = "Viewer::info_command";
const DEVELOP: &str = "Viewer::develop_op";
const FLOOD: &[ParamSpec] = &[t("layer"), n("x"), n("y"), n("opacity_pct").opt(), b("reverse").opt(), b("dither").opt(), n("tolerance").opt(), b("contiguous").opt(), b("anti_alias").opt(), b("sample_merged").opt(), t("pattern").opt()];
const FILL_SELECTION: &[ParamSpec] = &[t("layer"), n("opacity_pct").opt(), b("reverse").opt(), b("dither").opt(), n("tolerance").opt(), b("contiguous").opt(), b("anti_alias").opt(), b("sample_merged").opt(), t("pattern").opt()];
const GRADIENT: &[ParamSpec] = &[t("layer"), point("from"), point("to"), n("opacity_pct").opt(), b("reverse").opt(), b("dither").opt(), n("tolerance").opt(), b("contiguous").opt(), b("anti_alias").opt(), b("sample_merged").opt(), t("pattern").opt()];

pub(super) const ROWS: &[CommandEntry] = &[
    typed("photo.liquify", "Viewer::liquify", "Liquify", true, &[t("layer"), r("tool", "{kind: forward_warp|push_left|twirl|pucker|bloat|reconstruct|smooth|turbulence|mesh_clone, ...}"), samples("samples"), r("brush", "{size, density, pressure, rate}")]),
    typed("photo.select_quick", "Viewer::select_quick_merged", "Quick selection", false, &[t("layer"), samples("samples"), n("diameter_px"), o("mode", "add|subtract").opt(), b("sample_merged").opt()]),
    typed("photo.select_magnetic", "Viewer::select_magnetic", "Magnetic selection", false, &[t("layer"), points("trace"), n("width").opt(), n("contrast").opt(), n("frequency").opt(), b("sample_merged").opt(), i("margin_px").opt()]),
    typed("photo.refine_stroke", "Viewer::refine_stroke", "Refine selection stroke", false, &[t("layer"), samples("samples")]),
    typed("photo.flood_fill", "Viewer::flood_fill", "Flood fill", true, FLOOD),
    typed("photo.fill_selection", "Viewer::fill_selection", "Fill selection", true, FILL_SELECTION),
    typed("photo.fill_gradient", "Viewer::fill_gradient", "Gradient fill", true, GRADIENT),
    typed("photo.info.add_sampler", INFO, "Add colour sampler", false, &[n("x"), n("y")]),
    typed("photo.info.move_sampler", INFO, "Move colour sampler", false, &[i("index"), n("x"), n("y")]),
    typed("photo.info.remove_sampler", INFO, "Remove colour sampler", false, &[i("index")]),
    typed("photo.info.set_sampler_model", INFO, "Set sampler colour model", false, &[i("index"), o("model", MODEL)]),
    typed("photo.info.set_sample_size", INFO, "Set sample size", false, &[o("size", "point|s3|s5|s11|s31|s51|s101")]),
    typed("photo.info.set_sample_scope", INFO, "Set sample scope", false, &[o("scope", "current_layer|current_and_below|all_layers")]),
    typed("photo.info.set_model", INFO, "Set info colour model", false, &[o("model", MODEL)]),
    typed("photo.develop.kelvin", DEVELOP, "White balance by temperature", true, &[n("kelvin"), n("tint").opt()]),
    typed("photo.develop.as_shot_white", DEVELOP, "White balance as shot", true, &[]),
    typed("photo.develop.add_point_colour", DEVELOP, "Add point colour", true, &[vector3("source")]),
    typed("photo.develop.remove_point_colour", DEVELOP, "Remove point colour", true, &[i("index")]),
    typed("photo.develop.add_luminance_range", DEVELOP, "Add luminance range", true, &[n("lo"), n("hi"), n("feather").opt()]),
    typed("photo.develop.remove_correction", DEVELOP, "Remove local correction", true, &[i("index")]),
    typed("photo.develop.remove_mask", DEVELOP, "Remove correction mask", true, &[i("correction"), i("mask")]),
    typed("photo.develop.mask_flags", DEVELOP, "Set correction mask flags", true, &[i("correction"), i("mask"), b("inverted").opt(), b("subtract").opt(), b("flipped").opt()]),
    typed("photo.develop.add_brush_dabs", DEVELOP, "Add brush dabs", true, &[i("correction"), i("mask"), points("dabs")]),
    typed("photo.develop.grayscale", DEVELOP, "Grayscale", true, &[b("on")]),
    typed("photo.develop.add_spot", DEVELOP, "Add spot removal", true, &[o("spot_type", "heal|clone"), point("centre"), n("radius"), point("source").opt(), n("feather").opt(), n("opacity").opt()]),
    typed("photo.develop.move_spot_source", DEVELOP, "Move spot source", true, &[i("spot"), point("source")]),
    typed("photo.develop.set_spot", DEVELOP, "Edit spot removal", true, &[i("spot"), o("spot_type", "heal|clone"), n("feather"), n("opacity"), n("radius")]),
    typed("photo.develop.remove_spot", DEVELOP, "Remove spot removal", true, &[i("spot")]),
    typed("photo.develop.add_distraction", DEVELOP, "Add distraction removal", true, &[point("centre"), n("radius")]),
    typed("photo.develop.remove_distraction", DEVELOP, "Remove distraction removal", true, &[i("index")]),
];
