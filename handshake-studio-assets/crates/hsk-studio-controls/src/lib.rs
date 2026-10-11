//! One shared egui control/panel mechanic configured by capability registry; agent
//! projections/manual use same semantics; never imports full native app.
//!
//! Slice 1: typed control descriptors ([`ControlSpec`]) in a [`Registry`] with a dead-control
//! audit, a validated [`ValueStore`] emitting undo-friendly [`ChangeEvent`]s, one egui renderer
//! driven only by descriptors ([`egui_adapter`], headless-testable), and agent JSON / UserManual
//! projections of the same descriptors ([`projection`]).
#![forbid(unsafe_code)]

pub mod descriptor;
pub mod egui_adapter;
pub mod projection;
pub mod registry;
pub mod store;
pub mod value;

pub use descriptor::{
    ChoiceOption, ControlKind, ControlSpec, NumberSpec, NumberWidget, SoftState, StepModifier,
};
pub use registry::{AuditReport, Registry};
pub use store::{ChangeEvent, ChangeSource, Interaction, ValueStore};
pub use value::{ControlError, Value};

/// Machine-readable crate descriptor (same convention as the other Studio crates).
pub const DESCRIPTOR: &str = r#"{"schema":"hsk.studio.controls.descriptor@1","crate":"hsk-studio-controls","slice":1,"controls_schema":"hsk.studio.controls@1","kinds":["number","toggle","choice","color","text"],"routes":["typed","pointer","keyboard","model","reset","undo"],"number":{"bounds":["hard_min","hard_max","soft_min","soft_max"],"hard_violation":"error_never_clamped","pointer":"clamps_hard_only","step":"10^-precision","fine_step":"step/10","coarse_step":"step*10"},"egui":{"version":"0.33.3","eframe":false,"accesskit":false,"headless":true},"not_in_slice":["accesskit_projection","panels_docking","gizmos","pointer_resistance","gesture_coalescing","observe_reporting"]}"#;
