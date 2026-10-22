//! Keyframes/easing and temporal/property-expression/time/seed/dependency policy; typed provider
//! ports preserve pure Rust domain closure. Static composition plan belongs to Composite; optional
//! JS engine belongs to MotionJS. No vendor runtime or unconditional NLE/audio/native provider
//! dependency.
#![forbid(unsafe_code)]

mod assistants;
mod audio_amplitude;
mod date;
mod depgraph;
mod error;
mod expression;
mod graph;
mod keyframe;
mod path;
mod preset;
mod property;
mod receipt;
mod spatial;
mod store;
mod text_animator;
mod time;
mod time_remap;

pub use audio_amplitude::{AMPLITUDE_CHANNELS, AMPLITUDE_SLIDER, amplitude_property, insert_audio_amplitude};
pub use preset::{
    AppliedPreset, MAX_PRESET_ENTRIES, MotionPreset, PRESET_TARGET_INCOMPATIBLE, PresetEffect, PresetEntry, PresetEntryKind,
    PresetError, apply_preset, capture_preset, validate_preset,
};
pub use date::{
    CivilTime, DateProfile, MAX_ABS_UTC_MS, civil_from_ms, parse_iso_exact, to_iso_string,
};
pub use depgraph::{
    DepGraph, MAX_GRAPH_NODES, MAX_READS_PER_EXPRESSION, ReferenceResolver, Resolution,
    resolve_references,
};
pub use assistants::{
    EaseSide, LayerSpan, MAX_SEQUENCE_LAYERS, SequenceTransition, SequencedLayer, auto_bezier, ease,
    make_continuous, rove, sequence_layers, set_interpolation, toggle_hold,
};
pub use error::MotionError;
pub use expression::{
    ExprError, ExprErrorCode, ExprRequest, ExpressionProvider, Fuel, HostRead, NoHost, NoProvider,
    SourceSpan,
};
pub use graph::{
    GraphChannel, GraphKind, GraphPoint, MAX_GRAPH_SAMPLES, VelocityRecord, fit_range, graph,
    join_dimensions, path_speed_at, separate_dimensions, set_velocity, speed_at, velocity_record,
};
pub use keyframe::{
    DEFAULT_INFLUENCE, Interpolation, Keyframe, MAX_DIMENSION, MAX_KEYFRAMES, TemporalTangent,
};
pub use path::{MAX_KEY_BYTES, MAX_PATH_BYTES, PropertyPath, validate_key};
pub use property::{
    EvalContext, Evaluation, MAX_EXPRESSION_BYTES, Property, PropertyState,
};
pub use receipt::{DESCRIPTOR, EvalReceipt, PROFILE_KEYFRAMES, report};
pub use spatial::{SpatialTangents, spatial_auto_bezier};
pub use store::{PropertyStore, StoreContext, TickEvaluation};
pub use text_animator::{
    Animator, AnimatorParam, AnimatorProperties, AnimatorTrack, BasedOn, CharTransform,
    MAX_ANIMATORS, MAX_SELECTORS, MAX_TEXT_UNITS, RangeSelector, RangeShape, RangeUnits, Selector,
    TextChar, WigglySelector, evaluate_text_animators, range_value, selector_amounts, splitmix64,
    units_of, value_noise,
};
pub use time::EvalTick;
pub use time_remap::{
    FrameBlending, FrameSample, RemapClamp, RemapSample, SourceTiming, TIME_REMAP_KEY, TimeRemap, frame_sample,
    ticks_to_seconds,
};
