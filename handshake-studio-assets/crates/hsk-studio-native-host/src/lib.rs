//! The asset-local Vulkan host (decisions O4, O6, `window_layer` in `direction.blocker_decisions_20261107`): window,
//! swapchain, OS IME and the AccessKit adapter. The only crate allowed winit, with a typed event stream past it and no
//! winit type in a public contract, so a raw Win32 backend can replace winit by editing this crate alone.
//!
//! Asset-local host, never host acceptance (HBR-HOST-001). Three paths, one pane: [`offscreen`] (no window, no HWND:
//! the proof path agents screenshot through), [`window`] (quiet for agents and tests, plain for the Operator), and the
//! `document` feature, which shows a real document through harness-view's `Viewer` on render-cpu, and in it the `gui`
//! scene ([`gui`]): the document in a Viewport beside a kumihimo-ui Layers panel, driven by typed [`HostEvent`]s and
//! scripted steps ([`script`]), with a `kumihimo.ui.tree@1` tree file as the model path. The chrome is the CPU
//! painter by default; `--chrome vulkan` draws the same display lists with hsk-studio-vulkan's UI pass on the
//! owner's device (receipt `ui_pass`, offscreen parity against the CPU painter). All Vulkan `unsafe` lives in
//! hsk-studio-vulkan; the Win32 style and
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
#[cfg(feature = "document")]
pub mod gui;
pub mod offscreen;
pub mod scene;
pub mod script;
mod win32;
pub mod window;

pub use error::HostError;
pub use event::HostEvent;

/// Self description for models and tooling.
pub const DESCRIPTOR: &str = r#"{"schema":"hsk.studio.native_host.descriptor@1","crate":"hsk-studio-native-host","slice":2,"state":"offscreen capture, quiet window, swapchain state machine, CPU buffer present, document feature","host":"asset-local host, never host acceptance","owns":"window, swapchain presentation, typed HostEvent stream; OS IME and AccessKit adapter pending","window_layer":"winit =0.30.13 with windows-sys, this crate only","binary":"studio-vk offscreen|window|help; offscreen and window take [--actor ID --session ID --resource SDOC-...] (all three or none; none = the asset-local test host's fixed context with the default resource SDOC-00000000-0000-7000-8000-000000000000 (cli::ASSET_LOCAL_RESOURCE), caller_default) and [--observe O.jsonl]","observe":"one terminal frame per host run on every exit path, refusals and a panic included (F-R60-22): offscreen::run and window::run take the caller's diag::HostRun; studio-vk writes each frame as one hsk.studio.native_host.observe@1 JSON line to --observe, else stderr; usage refusals of the binary are failure frames too (argument_encoding for a non-Unicode argument); help sends none; one correlation per run; an --observe file that does not open sends the line to stderr after observe_unavailable <code>; failed stderr writes change neither the exit code nor the receipt; a HostRun carries one run (host_run_reused)","modes":{"offscreen":"no window, no HWND, PNG plus receipt","window --quiet":"WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, SW_SHOWNOACTIVATE, bottom of Z order, left of every monitor, focus_changes must be 0","window":"the Operator's own window"},"scenes":["clear","painted","document (feature document)","gui (feature document): Viewport, right dock with the Layers panel, status bar; row click selects, eye click hides or shows as one undo step, Ctrl+Z/Ctrl+Shift+Z/Ctrl+Y, wheel scrolls; --script, --invoke AUTHOR_ID, --tree, --scale, --chrome cpu|vulkan, --ui-trace; B6 menus (SMB-GUI-MENU-1): the nineteen titles open the designed tree (harness-view menu_tree, one evaluation shared with the shell menu and the drive surface), greyed rows show their reason, remedy rows run, leaves run catalog or shell commands, keyboard (arrows, Enter, Escape, type-ahead, F10, Alt+letter, Shift+F10 and the Menu key), cascade safe triangle, scroll lane, Layers context menu, L0 and L1 tooltips (GuiFrame::show_tip; the window does not feed the dwell clock yet)"],"input":"typed HostEvent stream (pointer, button, wheel, key with modifiers) and scripted steps, never OS input","chrome":"cpu painter (default) or vulkan ui pass (--chrome vulkan, gui scene): one compose call, both targets","ui_pass":"receipt member: SPIR-V sha256 (sha2) checked against the vulkan manifest, naga 27.0.3, features, pass counters, allocation sizes, skip counters, offscreen parity against the CPU painter","receipt":"hsk.studio.native_host.receipt@1, validation on|absent|off with validation_report (errors, warnings, first ids)","exit_codes":{"0":"ok","2":"usage","3":"vulkan refusal","4":"io","5":"focus violation","6":"timeout","7":"document","8":"window"},"gpu_lock":"tests and binary runs take the GPU test lock","decisions":"direction.blocker_decisions_20261107 O4 O6 O8 window_layer gpu_contention"}"#;
