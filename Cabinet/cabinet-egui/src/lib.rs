//! Host-embedded egui rendering backed by Cabinet's framework-independent logic.
mod view;
mod widgets;
pub use view::{CabinetView, DropDiagnostic, Palette, ViewResponse};
