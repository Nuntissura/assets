//! Motion rows of the command corpus (lane SMB-MOTION-COMMANDS-1: the typed composition, layer,
//! property, keyframe and expression commands of [STU-MOT-130], run by `Run::motion_command`, module
//! `crate::harness::motion_commands`; lane SMB-MOTION-COMMANDS-2 adds solid settings, keyframe
//! selection, clipboard and the ease and hold assistants).

use super::super::ValueType;
use super::{CommandEntry, ParamSpec, entry, p};

// Every row's run method is `motion_command`; addressing keys are required, the rest optional.
// A `class` parameter is a JSON object, a `list` a JSON array.
const COMPOSITION_ID: ParamSpec = p("composition_id", ValueType::Reference);
const LAYER_ID: ParamSpec = p("layer_id", ValueType::Reference);
const PROPERTY: ParamSpec = p("property", ValueType::String);
const TIME: ParamSpec = p("time", ValueType::Number);
const VALUE_LIST: ParamSpec = p("value", ValueType::List);
const IN_INTERP: ParamSpec = p("in_interp", ValueType::Enumerated);
const OUT_INTERP: ParamSpec = p("out_interp", ValueType::Enumerated);
const IN_TANGENT: ParamSpec = p("in_tangent", ValueType::List);
const OUT_TANGENT: ParamSpec = p("out_tangent", ValueType::List);
const ITEM_ID: ParamSpec = p("item_id", ValueType::String);
const MODULE_ID: ParamSpec = p("module_id", ValueType::String);
const MASK_ID: ParamSpec = p("mask_id", ValueType::String);
const EFFECT_ID: ParamSpec = p("effect_id", ValueType::String);
const ANIMATOR: ParamSpec = p("animator", ValueType::Number);
const COMPOSITION_SETTINGS: &[ParamSpec] = &[COMPOSITION_ID, NAME, p("width", ValueType::Number), p("height", ValueType::Number), p("pixel_aspect", ValueType::Class), p("frame_rate", ValueType::Class), p("duration", ValueType::Number), p("background", ValueType::Class)];
const LAYER_NEW: &[ParamSpec] = &[COMPOSITION_ID, p("kind", ValueType::Enumerated), LAYER_ID, NAME, p("at", ValueType::Number), p("in_point", ValueType::Number), p("out_point", ValueType::Number), p("start_time", ValueType::Number), p("source_composition_id", ValueType::Reference), p("collapse_transformations", ValueType::Boolean), p("options", ValueType::Class), p("root", ValueType::Class), COLOUR, WIDTH, HEIGHT];
const COLOUR: ParamSpec = p("colour", ValueType::List);
const WIDTH: ParamSpec = p("width", ValueType::Number);
const HEIGHT: ParamSpec = p("height", ValueType::Number);
const RENDER_SETTINGS: &[ParamSpec] = &[ITEM_ID, p("quality", ValueType::Enumerated), p("resolution", ValueType::Class), p("disk_cache", ValueType::Enumerated), p("proxy_use", ValueType::Enumerated), p("effects", ValueType::Enumerated), p("solo_switches", ValueType::Enumerated), p("guide_layers", ValueType::Enumerated), p("color_depth", ValueType::Enumerated), p("frame_blending", ValueType::Enumerated), p("field_render", ValueType::Enumerated), p("pulldown_3_2", ValueType::Enumerated), p("motion_blur", ValueType::Enumerated), p("time_span", ValueType::Class), p("frame_rate", ValueType::Class)];
const OUTPUT_MODULE: &[ParamSpec] = &[ITEM_ID, MODULE_ID, p("format", ValueType::Enumerated), p("codec_option", ValueType::String), p("include_project_link", ValueType::Boolean), p("post_render_action", ValueType::Enumerated), p("include_source_metadata", ValueType::Boolean), p("channels", ValueType::Enumerated), p("depth", ValueType::Number), p("color", ValueType::Enumerated), p("starting_number", ValueType::Number), p("use_comp_frame_number", ValueType::Boolean), p("resize", ValueType::Class), p("crop", ValueType::Class), p("audio", ValueType::Class), p("colour", ValueType::Class), p("output_path", ValueType::String)];
const QUEUE_NEW: &[ParamSpec] = &[ITEM_ID, COMPOSITION_ID, MODULE_ID, p("format", ValueType::Enumerated), p("output_path", ValueType::String)];
const MASK_SET: &[ParamSpec] = &[LAYER_ID, MASK_ID, p("mode", ValueType::Enumerated), p("inverted", ValueType::Boolean), p("closed", ValueType::Boolean), p("path", ValueType::List)];
const EFFECT_SET: &[ParamSpec] = &[LAYER_ID, EFFECT_ID, p("filter_kind", ValueType::Enumerated), p("enabled", ValueType::Boolean), p("solo", ValueType::Boolean), p("opacity", ValueType::Number), p("blend_mode", ValueType::Number), MASK_ID, p("values", ValueType::Class), p("at", ValueType::Number), p("colour_profiles", ValueType::Class)];
const KEY_AT: &[ParamSpec] = &[LAYER_ID, PROPERTY, TIME, VALUE_LIST];
const KEY_SET: &[ParamSpec] = &[LAYER_ID, PROPERTY, TIME, VALUE_LIST, IN_INTERP, OUT_INTERP, IN_TANGENT, OUT_TANGENT];
const KEY_INTERP: &[ParamSpec] = &[LAYER_ID, PROPERTY, TIME, IN_INTERP, OUT_INTERP, IN_TANGENT, OUT_TANGENT];
const NAME: ParamSpec = p("name", ValueType::String);

pub(super) const ROWS: &[CommandEntry] = &[
    entry("composition.new", "motion_command", "New Composition", false, true, COMPOSITION_SETTINGS),
    entry("composition.set_settings", "motion_command", "Composition Settings", false, true, COMPOSITION_SETTINGS),
    entry("composition.delete", "motion_command", "Delete Composition", false, true, &[COMPOSITION_ID]),
    entry("composition.read", "motion_command", "Read Composition", false, false, &[COMPOSITION_ID]),
    entry("composition.set_work_area", "motion_command", "Set Work Area", false, true, &[COMPOSITION_ID, p("start_tick", ValueType::Number), p("end_tick", ValueType::Number)]),
    entry("composition.set_motion_blur", "motion_command", "Composition Motion Blur", false, true, &[COMPOSITION_ID, p("motion_blur", ValueType::Boolean), p("shutter_angle", ValueType::Number), p("shutter_phase", ValueType::Number), p("samples_per_frame", ValueType::Number), p("adaptive_sample_limit", ValueType::Number)]),
    entry("composition.add_audio_layer", "motion_command", "Add Audio Layer", false, true, &[COMPOSITION_ID, p("audio_id", ValueType::String), p("start_tick", ValueType::Number), p("gain", ValueType::Number)]),
    entry("composition.remove_audio_layer", "motion_command", "Remove Audio Layer", false, true, &[COMPOSITION_ID, p("audio_id", ValueType::String)]),
    entry("composition.queue_add", "motion_command", "Add Composition to Render Queue", false, true, QUEUE_NEW),
    entry("composition.queue_remove", "motion_command", "Remove Render Queue Item", false, true, &[ITEM_ID]),
    entry("composition.queue_set_settings", "motion_command", "Render Settings", false, true, RENDER_SETTINGS),
    entry("composition.queue_add_module", "motion_command", "Add Output Module", false, true, OUTPUT_MODULE),
    entry("composition.queue_set_module", "motion_command", "Output Module Settings", false, true, OUTPUT_MODULE),
    entry("composition.queue_remove_module", "motion_command", "Remove Output Module", false, true, &[ITEM_ID, MODULE_ID]),
    entry("composition.add_layer", "motion_command", "New Layer", false, true, LAYER_NEW),
    entry("composition.remove_layer", "motion_command", "Remove Layer From Composition", false, true, &[LAYER_ID]),
    entry("composition.reorder_layer", "motion_command", "Move Layer In Stack", false, true, &[LAYER_ID, p("index", ValueType::Number)]),
    entry("composition.duplicate_layer", "motion_command", "Duplicate Layer", false, true, &[LAYER_ID, p("new_layer_id", ValueType::Reference), NAME]),
    entry("composition.set_layer_timing", "motion_command", "Layer Timing", false, true, &[LAYER_ID, p("in_point", ValueType::Number), p("out_point", ValueType::Number), p("start_time", ValueType::Number), p("stretch", ValueType::Class)]),
    entry("composition.set_parent", "motion_command", "Set Parent", false, true, &[LAYER_ID, p("parent", ValueType::Reference)]),
    entry("composition.set_track_matte", "motion_command", "Set Track Matte", false, true, &[LAYER_ID, p("source_layer_id", ValueType::Reference), p("mode", ValueType::Enumerated), p("inverted", ValueType::Boolean)]),
    entry("composition.set_switches", "motion_command", "Layer Switches", false, true, &[LAYER_ID, p("collapse_transformations", ValueType::Boolean), p("frame_blending", ValueType::Enumerated), p("motion_blur", ValueType::Boolean), p("three_d", ValueType::Boolean)]),
    entry("composition.precompose", "motion_command", "Pre-compose", false, true, &[p("layer_ids", ValueType::List), NAME, p("new_composition_id", ValueType::Reference), LAYER_ID]),
    entry("composition.set_time_remap", "motion_command", "Enable Time Remapping", false, true, &[LAYER_ID, p("enabled", ValueType::Boolean)]),
    entry("composition.set_camera", "motion_command", "Camera Settings", false, true, &[LAYER_ID, p("depth_of_field", ValueType::Boolean), p("iris_shape", ValueType::Enumerated)]),
    entry("composition.set_light", "motion_command", "Light Settings", false, true, &[LAYER_ID, p("light_type", ValueType::Enumerated), p("colour_profile_id", ValueType::Reference), p("falloff_type", ValueType::Enumerated), p("casts_shadows", ValueType::Boolean), p("background_visible", ValueType::Boolean), p("environment_atom", ValueType::Reference)]),
    entry("composition.set_material", "motion_command", "Material Options", false, true, &[LAYER_ID, p("accepts_lights", ValueType::Boolean), p("accepts_shadows", ValueType::Boolean), p("casts_shadows", ValueType::Boolean), p("appears_in_reflections", ValueType::Boolean), p("bevel_style", ValueType::Enumerated), p("bevel_direction", ValueType::Enumerated)]),
    entry("composition.add_mask", "motion_command", "New Mask", false, true, MASK_SET),
    entry("composition.set_mask", "motion_command", "Mask Settings", false, true, MASK_SET),
    entry("composition.remove_mask", "motion_command", "Remove Mask", false, true, &[LAYER_ID, MASK_ID]),
    entry("composition.add_effect", "motion_command", "Add Effect", false, true, EFFECT_SET),
    entry("composition.set_effect", "motion_command", "Effect Settings", false, true, EFFECT_SET),
    entry("composition.remove_effect", "motion_command", "Remove Effect", false, true, &[LAYER_ID, EFFECT_ID]),
    entry("composition.set_corner_pin", "motion_command", "Set Corner Pin", false, true, &[LAYER_ID, p("from", ValueType::List), p("to", ValueType::List), p("clear", ValueType::Boolean)]),
    entry("composition.add_roto_stroke", "motion_command", "Add Roto Stroke", false, true, &[LAYER_ID, p("stroke", ValueType::Class), p("source_frames", ValueType::Number)]),
    entry("composition.set_roto", "motion_command", "Set Roto", false, true, &[LAYER_ID, p("roto", ValueType::Class)]),
    entry("composition.add_shape_item", "motion_command", "Add Shape Item", false, true, &[LAYER_ID, p("group_path", ValueType::List), p("item", ValueType::Class)]),
    entry("composition.set_shape_item", "motion_command", "Shape Item Settings", false, true, &[LAYER_ID, p("path", ValueType::List), p("item", ValueType::Class)]),
    entry("composition.remove_shape_item", "motion_command", "Remove Shape Item", false, true, &[LAYER_ID, p("path", ValueType::List)]),
    entry("composition.add_text_animator", "motion_command", "Add Text Animator", false, true, &[LAYER_ID, NAME, p("properties", ValueType::Class), p("selectors", ValueType::List)]),
    entry("composition.set_text_animator", "motion_command", "Text Animator Settings", false, true, &[LAYER_ID, ANIMATOR, NAME, p("properties", ValueType::Class)]),
    entry("composition.add_text_selector", "motion_command", "Add Text Selector", false, true, &[LAYER_ID, ANIMATOR, p("selector", ValueType::Class)]),
    entry("property.read", "motion_command", "Read Property", false, false, &[LAYER_ID, PROPERTY, TIME]),
    entry("property.set_value", "motion_command", "Set Property Value", false, true, KEY_AT),
    entry("property.enable_animation", "motion_command", "Enable Animation", false, true, KEY_AT),
    entry("property.disable_animation", "motion_command", "Disable Animation", false, true, &[LAYER_ID, PROPERTY]),
    entry("keyframe.add_at_current", "motion_command", "Add Keyframe at Current Time", false, true, KEY_AT),
    entry("keyframe.toggle_at_current", "motion_command", "Add/Delete Keyframe at Current Time", false, true, KEY_AT),
    entry("keyframe.set", "motion_command", "Set Keyframe", false, true, KEY_SET),
    entry("keyframe.delete", "motion_command", "Delete Keyframe", false, true, &[LAYER_ID, PROPERTY, TIME]),
    entry("keyframe.move", "motion_command", "Move Keyframe", false, true, &[LAYER_ID, PROPERTY, TIME, p("to", ValueType::Number)]),
    entry("keyframe.shift", "motion_command", "Shift Keyframes", false, true, &[LAYER_ID, PROPERTY, p("times", ValueType::List), p("frames", ValueType::Number)]),
    entry("keyframe.set_interpolation", "motion_command", "Set Keyframe Interpolation", false, true, KEY_INTERP),
    // Lane SMB-MOTION-COMMANDS-2: solid settings, keyframe selection, clipboard and assistants.
    entry("composition.set_solid", "motion_command", "Solid Settings", false, true, &[LAYER_ID, COLOUR, WIDTH, HEIGHT]),
    entry("keyframe.select", "motion_command", "Select Keyframes", false, false, &[p("scope", ValueType::Enumerated), p("mode", ValueType::Enumerated), COMPOSITION_ID, LAYER_ID, PROPERTY, p("times", ValueType::List)]),
    entry("keyframe.deselect_all", "motion_command", "Deselect All Keyframes", false, false, &[]),
    entry("keyframe.selection", "motion_command", "Keyframe Selection", false, false, &[]),
    entry("keyframe.copy", "motion_command", "Copy Keyframes", false, false, &[LAYER_ID, PROPERTY, p("times", ValueType::List)]),
    entry("keyframe.paste", "motion_command", "Paste Keyframes", false, true, &[LAYER_ID, PROPERTY, TIME, p("reverse", ValueType::Boolean)]),
    entry("keyframe.shift_selected", "motion_command", "Shift Selected Keyframes", false, true, &[p("frames", ValueType::Number)]),
    entry("keyframe.ease", "motion_command", "Easy Ease", false, true, &[p("side", ValueType::Enumerated)]),
    entry("keyframe.toggle_hold", "motion_command", "Toggle Hold Keyframe", false, true, &[]),
    entry("expression.set_source", "motion_command", "Set Expression Source", false, true, &[LAYER_ID, ANIMATOR, p("selector", ValueType::Number), p("source", ValueType::String)]),
];
