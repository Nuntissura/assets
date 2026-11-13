//! Photo rows of the command corpus: selection, fill, liquify, info and develop records.

use super::super::ValueType;
use super::{CommandEntry, ParamSpec, flag as b, index as i, list as l, number as n, one_of as o, output_path as out, path as file, point, points, record as r, samples, text as t, typed, vector3};

const MODEL: &str = "rgb|hsb|lab|cmyk";
const INFO: &str = "Viewer::info_command";
const DEVELOP: &str = "Viewer::develop_op";
const SLIDERS: &str = "Viewer::set_develop_many";
const FLOOD: &[ParamSpec] = &[t("layer"), n("x"), n("y"), n("opacity_pct").opt(), b("reverse").opt(), b("dither").opt(), n("tolerance").opt(), b("contiguous").opt(), b("anti_alias").opt(), b("sample_merged").opt(), t("pattern").opt()];
const FILL_SELECTION: &[ParamSpec] = &[t("layer"), n("opacity_pct").opt(), b("reverse").opt(), b("dither").opt(), n("tolerance").opt(), b("contiguous").opt(), b("anti_alias").opt(), b("sample_merged").opt(), t("pattern").opt()];
const GRADIENT: &[ParamSpec] = &[t("layer"), point("from"), point("to"), n("opacity_pct").opt(), b("reverse").opt(), b("dither").opt(), n("tolerance").opt(), b("contiguous").opt(), b("anti_alias").opt(), b("sample_merged").opt(), t("pattern").opt()];

/// Encoded sRGB colour samples, each `[r, g, b]` in 0..1.
const fn colours(key: &'static str) -> ParamSpec {
    ParamSpec { key, value_type: ValueType::List, unit: None, required: true, shape: "[[r, g, b], ...]" }
}

/// Develop mask sources (lane SMB-DEVELOP-COMMANDS-1): the panel's linear / radial / brush defaults and
/// the two range sources take optional fields, a field of another source is refused; the model kinds
/// refuse `model_unavailable` (no model provider).
const SOURCES: &str = "linear|radial|brush|luminance_range|colour_range|subject|sky|background|landscape_component|object|person|person_component";
macro_rules! mask_params {
    ($($head:expr),*) => {
        &[$($head,)* o("source", SOURCES), point("zero").opt(), point("full").opt(), b("flipped").opt(), n("left").opt(), n("top").opt(), n("right").opt(),
          n("bottom").opt(), n("angle").opt(), n("midpoint").opt(), n("roundness").opt(), n("feather").opt(), points("dabs").opt(), n("radius").opt(),
          n("flow").opt(), n("lo").opt(), n("hi").opt(), colours("samples").opt(), n("breadth").opt()]
    };
}
const ADD_CORRECTION: &[ParamSpec] = mask_params!();
const ADD_MASK: &[ParamSpec] = mask_params!(i("correction"), b("subtract").opt());
/// The twelve STU-RAW-105 group enables.
const GROUPS: &str = "calibration|colour_adjustments|detail|effects|grayscale_mix|lens_corrections|mask_group_based_corrections|red_eye|retouch|tone_curve|transform|distraction_removal";

/// The 25 Folio `StudioSmartFilterKind` tags (lane SMB-PS-FILTER-ADJUST-COMMANDS-1, `view::raster_effects`).
const FILTER_KINDS: &str = "gaussian_blur|unsharp_mask|high_pass|median|add_noise|liquify|box_blur|motion_blur|radial_blur|find_edges|custom_convolution|sharpen|sharpen_more|basic_blur|blur_more|sharpen_edges|despeckle|emboss|solarize|mosaic|maximum|minimum|dust_and_scratches|surface_blur|bilateral_blur";
const FILTER_PARAMS: &str = "{<parameter>: value, ...} as effect.parameters lists them for the kind";
const EFFECT: &str = "Viewer::effect_command";
/// A Folio `StudioAdjustment` record, tag `kind`.
const ADJUSTMENT: &str = "{kind: levels|curves|hue_saturation|brightness_contrast|exposure|invert|threshold|posterize|vibrance|colour_balance|photo_filter|black_and_white|channel_mixer|selective_colour|gradient_map|colour_lookup|shadows_highlights|tone_compression|match_colour|replace_colour, ...}";
const ADJUST: &str = "Viewer::adjustment_command";

pub(super) const ROWS: &[CommandEntry] = &[
    typed("photo.liquify", "Viewer::liquify", "Liquify", true, &[t("layer"), r("tool", "{kind: forward_warp|push_left|twirl|pucker|bloat|reconstruct|smooth|turbulence|mesh_clone, ...}"), samples("samples"), r("brush", "{size, density, pressure, rate}")]),
    typed("photo.select_quick", "Viewer::select_quick_merged", "Quick selection", false, &[t("layer"), samples("samples"), n("diameter_px"), o("mode", "add|subtract").opt(), b("sample_merged").opt()]),
    typed("photo.select_magnetic", "Viewer::select_magnetic", "Magnetic selection", false, &[t("layer"), points("trace"), n("width").opt(), n("contrast").opt(), n("frequency").opt(), b("sample_merged").opt(), i("margin_px").opt()]),
    typed("photo.refine_stroke", "Viewer::refine_stroke", "Refine selection stroke", false, &[t("layer"), samples("samples")]),
    typed("photo.flood_fill", "Viewer::flood_fill", "Flood fill", true, FLOOD),
    typed("photo.fill_selection", "Viewer::fill_selection", "Fill selection", true, FILL_SELECTION),
    typed("photo.fill_gradient", "Viewer::fill_gradient", "Gradient fill", true, GRADIENT),
    // The info rows write the session entry `info` (persistent samplers are document state, STU-RAS-163,
    // STU-COL-232; the readout model, sample size and scope share that entry), one undo step each, so they
    // mutate (R60 attempt-2 probe on SMB-COMMAND-SURFACE-1). The readouts themselves are in state_json.
    typed("photo.info.add_sampler", INFO, "Add colour sampler", true, &[n("x"), n("y")]),
    typed("photo.info.move_sampler", INFO, "Move colour sampler", true, &[i("index"), n("x"), n("y")]),
    typed("photo.info.remove_sampler", INFO, "Remove colour sampler", true, &[i("index")]),
    typed("photo.info.set_sampler_model", INFO, "Set sampler colour model", true, &[i("index"), o("model", MODEL)]),
    typed("photo.info.set_sample_size", INFO, "Set sample size", true, &[o("size", "point|s3|s5|s11|s31|s51|s101")]),
    typed("photo.info.set_sample_scope", INFO, "Set sample scope", true, &[o("scope", "current_layer|current_and_below|all_layers")]),
    typed("photo.info.set_model", INFO, "Set info colour model", true, &[o("model", MODEL)]),
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
    // Lane SMB-DEVELOP-COMMANDS-1: every other DevelopOp / FamilyOp member, the sliders as one step, and
    // the develop must-haves (group enables, point curves, crop and straighten, snapshots, preset save).
    typed("photo.develop.set", SLIDERS, "Set develop control", true, &[t("control"), n("value")]),
    typed("photo.develop.set_many", SLIDERS, "Set develop controls", true, &[r("changes", "{control: value, ...}")]),
    typed("photo.develop.add_colour_range", DEVELOP, "Add colour range correction", true, &[colours("samples"), n("breadth")]),
    typed("photo.develop.add_correction", DEVELOP, "Add local correction", true, ADD_CORRECTION),
    typed("photo.develop.add_mask", DEVELOP, "Add correction mask", true, ADD_MASK),
    typed("photo.develop.lens_profile", DEVELOP, "Apply lens profile", true, &[file("path"), n("focal_mm"), n("aperture_value").opt(), n("distance_m").opt()]),
    typed("photo.develop.clear_lens_profile", DEVELOP, "Clear lens profile", true, &[]),
    typed("photo.develop.lens_toggles", DEVELOP, "Lens profile and CA toggles", true, &[b("profile_enable"), b("auto_ca")]),
    typed("photo.develop.upright", DEVELOP, "Upright mode", true, &[o("mode", "off|auto|level|vertical|full|guided")]),
    typed("photo.develop.upright_guides", DEVELOP, "Upright guides", true, &[r("guides", "[[[x, y], [x, y]], ...] at most 4")]),
    typed("photo.develop.grain_seed", DEVELOP, "Grain seed", true, &[i("seed")]),
    typed("photo.develop.vignette_style", DEVELOP, "Post-crop vignette style", true, &[o("style", "highlight_priority|colour_priority")]),
    typed("photo.develop.look", DEVELOP, "Apply creative look", true, &[file("path"), t("name"), o("domain", "srgb_encoded|linear")]),
    typed("photo.develop.clear_look", DEVELOP, "Clear creative look", true, &[]),
    typed("photo.develop.apply_preset", DEVELOP, "Apply develop preset", true, &[file("path"), n("amount")]),
    typed("photo.develop.set_enable", DEVELOP, "Set develop group enable", true, &[o("group", GROUPS), b("on")]),
    typed("photo.develop.clear_enable", DEVELOP, "Clear develop group enable override", true, &[o("group", GROUPS)]),
    typed("photo.develop.set_point_curve", DEVELOP, "Set point curve", true, &[o("channel", "composite|red|green|blue"), points("points")]),
    typed("photo.develop.set_crop", DEVELOP, "Crop and straighten", true, &[n("left"), n("top"), n("right"), n("bottom"), n("angle").opt(), b("flip_horizontal").opt(), b("flip_vertical").opt()]),
    typed("photo.develop.snapshot.save", DEVELOP, "Save develop snapshot", true, &[t("name")]),
    typed("photo.develop.snapshot.apply", DEVELOP, "Apply develop snapshot", true, &[t("name")]),
    typed("photo.develop.snapshot.delete", DEVELOP, "Delete develop snapshot", true, &[t("name")]),
    typed("photo.develop.save_preset", "Viewer::save_develop_preset", "Save develop preset", false, &[out("path"), t("name"), l("keys"), b("overwrite").opt()]),
    // Lane SMB-PS-FILTER-ADJUST-COMMANDS-1: the STU-FX-007 effect commands over a raster layer's smart filter stack
    // (one entry-only undo step each), the parameter specs a model plans with (STU-RAS-110), and the STU-RAS-035
    // adjustment layer (create, edit) and its destructive apply-in-place form. Module `view::raster_effects`.
    typed("effect.apply", EFFECT, "Apply smart filter", true, &[t("layer"), o("filter_kind", FILTER_KINDS), r("params", FILTER_PARAMS).opt(), n("opacity").opt(), b("enabled").opt(), i("at").opt()]),
    typed("effect.update", EFFECT, "Update smart filter", true, &[t("layer"), t("instance"), r("params", FILTER_PARAMS).opt(), n("opacity").opt()]),
    typed("effect.reorder", EFFECT, "Reorder smart filter", true, &[t("layer"), t("instance"), i("to")]),
    typed("effect.toggle", EFFECT, "Enable or disable smart filter", true, &[t("layer"), t("instance").opt(), b("enabled")]),
    typed("effect.remove", EFFECT, "Remove smart filter", true, &[t("layer"), t("instance")]),
    typed("effect.read", EFFECT, "Read smart filters", false, &[t("layer")]),
    typed("effect.parameters", EFFECT, "Smart filter parameter specs", false, &[o("filter_kind", FILTER_KINDS).opt()]),
    typed("adjustment.create", ADJUST, "New adjustment layer", true, &[r("adjustment", ADJUSTMENT), t("name").opt(), b("clipped").opt()]),
    typed("adjustment.edit", ADJUST, "Edit adjustment layer", true, &[t("layer"), r("adjustment", ADJUSTMENT)]),
    typed("adjustment.apply", ADJUST, "Apply adjustment in place", true, &[t("layer"), r("adjustment", ADJUSTMENT)]),
];
