//! The asset-local Vulkan host (decisions O4, O6, `window_layer` in `direction.blocker_decisions_20261107`): window,
//! swapchain, OS IME and the AccessKit adapter. The only crate allowed winit, with a typed event stream past it and no
//! winit type in a public contract, so a raw Win32 backend can replace winit by editing this crate alone.
//!
//! Asset-local host, never host acceptance (HBR-HOST-001). Three paths, one pane: [`offscreen`] (no window, no HWND:
//! the proof path agents screenshot through), [`window`] (quiet for agents and tests, plain for the Operator), and the
//! `document` feature, which shows a real document through harness-view's `Viewer` on render-cpu. The chrome is the CPU
//! painter; the Vulkan UI pass is pending. All Vulkan `unsafe` lives in hsk-studio-vulkan; the Win32 style and
//! foreground calls winit lacks live in one private module here.
//!
//! A run of this crate's binary takes the machine-wide GPU test lock (`studio_gpu_run`), and its tests sit in the GPU
//! test group (`.config/nextest.toml`). Manual: `README.md` beside this crate's manifest.
#![deny(unsafe_code)]

pub mod capture;
pub mod cli;
pub mod diag;
#[cfg(feature = "document")]
pub mod document;
pub mod error;
pub mod event;
pub mod offscreen;
pub mod scene;
mod win32;
pub mod window;

pub use error::HostError;
pub use event::HostEvent;

/// Self description for models and tooling.
pub const DESCRIPTOR: &str = r#"{"schema":"hsk.studio.native_host.descriptor@1","crate":"hsk-studio-native-host","slice":1,"state":"offscreen capture, quiet window, swapchain state machine, CPU buffer present, document feature","host":"asset-local host, never host acceptance","owns":"window, swapchain presentation, typed HostEvent stream; OS IME and AccessKit adapter pending","window_layer":"winit =0.30.13 with windows-sys, this crate only","binary":"studio-vk offscreen|window|help","modes":{"offscreen":"no window, no HWND, PNG plus receipt","window --quiet":"WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, SW_SHOWNOACTIVATE, bottom of Z order, left of every monitor, focus_changes must be 0","window":"the Operator's own window"},"scenes":["clear","painted","document (feature document)"],"chrome":"cpu painter, Vulkan UI pass pending","receipt":"hsk.studio.native_host.receipt@1, validation: absent","exit_codes":{"0":"ok","2":"usage","3":"vulkan refusal","4":"io","5":"focus violation","6":"timeout","7":"document","8":"window"},"gpu_lock":"tests and binary runs take the GPU test lock","decisions":"direction.blocker_decisions_20261107 O4 O6 O8 window_layer gpu_contention"}"#;
