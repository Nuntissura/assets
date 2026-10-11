//! Static render-plan evaluator consumes immutable snapshot; same semantics for previews/export.
//! Temporal/effect providers attach through typed ports; no unconditional motion/pulse engine
//! dependency in the static render closure.
#![forbid(unsafe_code)]
pub mod blend;

pub use blend::{
    Applicability, BlendContext, BlendError, BlendMode, BlendReceipt, BlendSpace, Fidelity,
    ModeInfo, OperatorTag, PixelSite, Rgba, StackNode, blend_rgb, composite_over, evaluate_stack,
};
