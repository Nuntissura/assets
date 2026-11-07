//! The asset-local Vulkan host (decisions O4, O6, `window_layer` in `direction.blocker_decisions_20261107`): window,
//! swapchain, OS IME and the AccessKit adapter. The only crate allowed winit, with a typed event stream past it and no
//! winit type in a public contract, so a raw Win32 backend can replace winit by editing this crate alone.
//!
//! Skeleton: no API yet. A run of this crate's binary takes the machine-wide GPU test lock, and its tests sit in the
//! GPU test group (`.config/nextest.toml`).
#![deny(unsafe_code)]

/// Self description for models and tooling.
pub const DESCRIPTOR: &str = r#"{"schema":"hsk.studio.native_host.descriptor@1","crate":"hsk-studio-native-host","slice":0,"state":"skeleton, no API yet","owns":"window, swapchain, OS IME, AccessKit adapter","window_layer":"winit =0.30.13 with windows-sys, this crate only","gpu_lock":"tests and binary runs take the GPU test lock","decisions":"direction.blocker_decisions_20261107 O4 O6 O8 window_layer gpu_contention"}"#;
