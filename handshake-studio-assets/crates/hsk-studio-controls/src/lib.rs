//! One shared egui control/panel mechanic configured by capability registry; agent
//! projections/manual use same semantics; never imports full native app.
//!
//! Slices 1-2: typed control descriptors ([`ControlSpec`]) in a [`Registry`] with a dead-control
//! audit, a validated [`ValueStore`] emitting undo-friendly [`ChangeEvent`]s, one egui renderer
//! driven only by descriptors ([`egui_adapter`], headless-testable), and agent JSON / UserManual
//! projections of the same descriptors ([`projection`]), spec-aligned scrub gestures with one undo
//! step per gesture, an integer domain, collapsible sections, and Observe reports ([`report`]).
//! Menu and window descriptors ([`menu`], lane SMB-HARNESS-SHELL-2): one availability evaluation
//! feeding the egui menu bar, the agent JSON and the manual, with the menu-law audit.
#![forbid(unsafe_code)]

pub mod descriptor;
pub mod egui_adapter;
pub mod menu;
pub mod projection;
pub mod registry;
pub mod report;
pub mod store;
pub mod value;

pub use descriptor::{
    ChoiceOption, ControlKind, ControlSpec, GestureRoles, IntegerSpec, NumberSpec, NumberWidget, ScrubGesture,
    SoftState,
};
pub use menu::{Availability, AvailabilityState, DescriptorIssue, MenuBar, MenuNode, MenuNodeKind, ReasonCode, WindowRole, WindowSpec};
pub use registry::{AuditReport, Registry, SectionSpec};
pub use store::{ChangeEvent, ChangeSource, EventPhase, GesturePhase, Interaction, ValueStore};
pub use value::{ControlError, Value};

/// Machine-readable crate descriptor (same convention as the other Studio crates).
pub const DESCRIPTOR: &str = r#"{"schema":"hsk.studio.controls.descriptor@1","crate":"hsk-studio-controls","slice":2,"controls_schema":"hsk.studio.controls@1","kinds":["number","integer","toggle","choice","color","text"],"routes":["typed","pointer","keyboard","model","reset","undo"],"number":{"bounds":["hard_min","hard_max","soft_min","soft_max"],"hard_violation":"error_never_clamped","hard_violation_proof":"tests/foundation_controls.rs fixture_numeric_legality_store_and_undo","pointer":"clamps_hard_only","step":"10^-precision","fine_step":"step/10","coarse_step":"step*10"},"integer":{"step":1,"coarse_step":1,"fine_step":1},"steps":{"owner":"ControlSpec::step_value","no_effect":"refused_step_no_effect","slider_arrows":"descriptor_step","proof":"tests/step_effect.rs"},"gesture":{"roles":"shift_coarse_x10_command_fine_div10_both_x100_alt_unused","sensitivity":"STU-SHL-191","undo":"one_committed_event_per_gesture","abort":"escape_restores_press_value"},"sections":{"group_path":"declared_parent_chain","collapsed_by_default":true},"observe":"one_terminal_frame_per_operation_values_redacted","egui":{"version":"0.33.3","eframe":false,"accesskit":false,"headless":true},"not_in_slice":["accesskit_projection","docking_windows","gizmos","pointer_resistance_zone","wheel_gesture_400ms","gesture_binding_rebinding","search_and_show_non_default_filters"]}"#;
