//! The leaf that holds all Vulkan `unsafe` (decision O2, `direction.blocker_decisions_20261107`): ash =0.38.0 on a
//! Vulkan 1.3 baseline, with no ash type in a public contract. It takes a host-supplied device (O4). Windows x86-64 is
//! the only scoped platform and a missing Vulkan is a typed refusal (O8).
//!
//! Skeleton: no API yet. The default build's closure excludes wgpu, naga, winit, eframe and egui; the quarantine gate
//! in `tools/workspace-probe.rs` checks that (`--self-test`).
#![deny(unsafe_op_in_unsafe_fn)]

/// Self description for models and tooling.
pub const DESCRIPTOR: &str = r#"{"schema":"hsk.studio.vulkan.descriptor@1","crate":"hsk-studio-vulkan","slice":0,"state":"skeleton, no API yet","owns":"all Vulkan unsafe; ash =0.38.0, Vulkan 1.3 baseline; no ash type in a public contract","default_closure_excludes":["wgpu","naga","winit","eframe","egui"],"closure_checked_by":"tools/workspace-probe.rs --self-test","platforms":"Windows x86-64 with Vulkan 1.3; otherwise a typed refusal","decisions":"direction.blocker_decisions_20261107 O2 O3 O4 O6 O8"}"#;
