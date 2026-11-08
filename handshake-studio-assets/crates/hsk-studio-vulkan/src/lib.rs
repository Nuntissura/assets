//! The leaf that holds all Vulkan `unsafe` (decision O2, `direction.blocker_decisions_20261107`): ash =0.38.0 on a
//! Vulkan 1.3 baseline, with no ash type in a public contract. It takes a host-supplied device (O4). Windows x86-64 is
//! the only scoped platform and a missing Vulkan is a typed refusal (O8).
//!
//! Asset-local host proof only (HBR-HOST-001): the [`Owner`] is the transient owner of STU-ARC-024 behind a local
//! [`GpuClientPort`]-shaped trait, issuer `transient_asset_local`, never host acceptance. One device per process.
//! Panes are owner images; the [`Presenter`] copies a pane into a Win32 swapchain image with `vkCmdBlitImage`. There is
//! no shader, pipeline or descriptor in this crate. The Khronos validation layer is absent on this machine, so every
//! receipt says `validation: absent`; the stand-ins are bounded fence and acquire waits, a device wait-idle before every
//! swapchain re-create, present semaphores per swapchain image, and a pixel compare of offscreen against swapchain.
//!
//! The default build's closure excludes wgpu, naga, winit, eframe and egui; the quarantine gate in
//! `tools/workspace-probe.rs` checks that (`--self-test`).
#![deny(unsafe_op_in_unsafe_fn)]

pub mod machine;
mod owner;
mod present;
mod refusal;

pub use owner::{
    DeviceInfo, DeviceLease, FENCE_TIMEOUT_NS, GpuClientPort, Issuer, LayerReport, Owner, OwnerConfig, PANE_FORMAT,
    PaneId, VALIDATION_LAYER, ValidationId, ValidationReport, process_validation,
};
pub use present::{
    ACQUIRE_TIMEOUT_NS, Capture, FrameOutcome, PresentCounters, Presenter, SWAPCHAIN_FORMAT, WindowHandles,
    WindowSignal, centred_region,
};
pub use refusal::{MAX_EXTENT, MAX_PIXELS, Refusal, rgba_len};

/// Self description for models and tooling.
pub const DESCRIPTOR: &str = r#"{"schema":"hsk.studio.vulkan.descriptor@1","crate":"hsk-studio-vulkan","slice":1,"state":"transient owner, panes, Win32 presenter, swapchain state machine; no shader, pipeline or descriptor","owns":"all Vulkan unsafe; ash =0.38.0, Vulkan 1.3 baseline; no ash type in a public contract","issuer":"transient_asset_local (STU-ARC-024), asset-local host proof only, never host acceptance","one_device_per_process":true,"pane_format":"R8G8B8A8_UNORM","swapchain_format":"B8G8R8A8_UNORM, SRGB_NONLINEAR, FIFO","copy":"vkCmdCopyBufferToImage into a pane, vkCmdBlitImage pane to swapchain","overlay_layers":"VK_LOADER_LAYERS_DISABLE=~implicit~ in this process unless allow_overlays","validation":"absent","refusals":["vulkan_unavailable","api_below_baseline","no_suitable_device","cpu_device_refused","owner_already_open","extension_missing","out_of_memory","device_lost","fence_timeout","surface_unsupported","surface_unrecoverable","swapchain_format_unsupported","swapchain_usage_unsupported","invalid_extent","invalid_pane","invalid_buffer","invalid_window","vk_error"],"swap_states":["uncreated","running","stale","minimized","surface_lost","device_lost","surface_unrecoverable"],"default_closure_excludes":["wgpu","naga","winit","eframe","egui"],"closure_checked_by":"tools/workspace-probe.rs --self-test","platforms":"Windows x86-64 with Vulkan 1.3; otherwise a typed refusal","decisions":"direction.blocker_decisions_20261107 O2 O3 O4 O6 O8"}"#;
