//! Keyframes/easing and temporal/property-expression/time/seed/dependency policy; typed provider
//! ports preserve pure Rust domain closure. Static composition plan belongs to Composite; optional
//! JS engine belongs to MotionJS. No vendor runtime or unconditional NLE/audio/native provider
//! dependency.
#![forbid(unsafe_code)]

mod date;
mod depgraph;
mod error;
mod expression;
mod keyframe;
mod path;
mod property;
mod receipt;
mod store;
mod time;

pub use date::{
    CivilTime, DateProfile, MAX_ABS_UTC_MS, civil_from_ms, parse_iso_exact, to_iso_string,
};
pub use depgraph::{
    DepGraph, MAX_GRAPH_NODES, MAX_READS_PER_EXPRESSION, ReferenceResolver, Resolution,
    resolve_references,
};
pub use error::MotionError;
pub use expression::{
    ExprError, ExprErrorCode, ExprRequest, ExpressionProvider, Fuel, HostRead, NoHost, NoProvider,
    SourceSpan,
};
pub use keyframe::{
    DEFAULT_INFLUENCE, Interpolation, Keyframe, MAX_DIMENSION, MAX_KEYFRAMES, TemporalTangent,
};
pub use path::{MAX_KEY_BYTES, MAX_PATH_BYTES, PropertyPath, validate_key};
pub use property::{
    EvalContext, Evaluation, MAX_EXPRESSION_BYTES, Property, PropertyState,
};
pub use receipt::{DESCRIPTOR, EvalReceipt, PROFILE_KEYFRAMES, report};
pub use store::{PropertyStore, StoreContext, TickEvaluation};
pub use time::EvalTick;
