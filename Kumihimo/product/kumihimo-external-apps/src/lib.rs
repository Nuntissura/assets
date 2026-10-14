//! Host-owned, explicitly selected external windows. Core remains free of OS handles.
//! No process launching, foreground activation, reparenting, or desktop-wide operations.
mod identity;
mod recovery;
mod tiling;
#[cfg(all(windows, feature = "windows"))]
pub mod windows;
pub use identity::*;
pub use recovery::*;
pub use tiling::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: String,
    pub message: String,
    pub os_error: Option<u32>,
}
impl Diagnostic {
    pub fn new(code: &str, message: &str) -> Self {
        Self { code: code.into(), message: message.into(), os_error: None }
    }
}
impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}: {} ({:?})", self.code, self.message, self.os_error) }
}
impl std::error::Error for Diagnostic {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PixelBounds { pub x: i32, pub y: i32, pub width: i32, pub height: i32 }
impl PixelBounds {
    pub fn validate(self) -> Result<(), Diagnostic> {
        if self.width <= 0 || self.height <= 0 || self.x.checked_add(self.width).is_none() || self.y.checked_add(self.height).is_none() {
            Err(Diagnostic::new("invalid_bounds", "Positive dimensions and representable physical edges required"))
        } else { Ok(()) }
    }
    pub fn contains(self, other: Self) -> bool {
        self.validate().is_ok() && other.validate().is_ok() && other.x >= self.x && other.y >= self.y
            && i64::from(other.x) + i64::from(other.width) <= i64::from(self.x) + i64::from(self.width)
            && i64::from(other.y) + i64::from(other.height) <= i64::from(self.y) + i64::from(self.height)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindowState { Normal, Minimized, Maximized }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Placement { pub bounds: PixelBounds, pub state: WindowState }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub work_area: PixelBounds,
    pub dpi: u32,
    pub minimum_width: i32,
    pub minimum_height: i32,
    pub can_resize: bool,
}

pub fn unsupported_platform() -> Diagnostic {
    Diagnostic::new("unsupported_platform", "Native external-window backend requires Windows and the windows feature")
}
