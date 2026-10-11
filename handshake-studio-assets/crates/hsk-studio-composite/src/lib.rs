//! Static render-plan evaluator consumes immutable snapshot; same semantics for previews/export.
//! Temporal/effect providers attach through typed ports; no unconditional motion/pulse engine
//! dependency in the static render closure.
#![forbid(unsafe_code)]
