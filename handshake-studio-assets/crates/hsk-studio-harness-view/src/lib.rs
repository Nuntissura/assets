//! View half of STUDIO-MODULE-HARNESS (SMB-CRATE-SEAM-SPLITS slice 3b): the desktop viewer state and
//! egui UI (feature viewer, window-free and headless-testable) and the layout editing surface
//! (feature layout-ui), on the core half's run, import routes and panel. `hsk-studio-harness` is the
//! public face.
#![forbid(unsafe_code)]

// The core half's modules and run types under the names these modules have always used.
#[allow(unused_imports)]
use hsk_studio_harness_core::{Place, PlacementWrite, Run, harness, import, panel};
/// The context a host builds for `view::Viewer::with_context` from its caller (lane SMB-GUI-GESTURE-GATE-1: the native
/// host opens its viewer as the host run's actor without a harness-core edge of its own).
pub use hsk_studio_harness_core::RunContext;

/// Layout editing surface (lane SMB-LAYOUT-UI): page view, text frames, story editor, styles.
#[cfg(feature = "layout-ui")]
pub mod layout_ui;
#[cfg(feature = "viewer")]
pub mod view;
