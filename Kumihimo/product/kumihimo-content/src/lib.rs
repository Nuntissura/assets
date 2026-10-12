//! Headless content contracts. Hosts execute effects and report their outcomes.
//! Dropping a controller drops metadata only; it never disposes host engines.
#![forbid(unsafe_code)]
#[cfg(feature = "backgrounds")]
mod backgrounds;
mod diagnostics;
mod lifecycle;
#[cfg(feature = "navigation")]
mod navigation;
#[cfg(feature = "navigation")]
mod overview;
mod protocol;
mod routing;
mod scheduler;
#[cfg(any(feature = "navigation", feature = "backgrounds"))]
mod settings;
#[cfg(feature = "backgrounds")]
pub use backgrounds::*;
pub use diagnostics::*;
#[cfg(feature = "navigation")]
pub use navigation::*;
#[cfg(feature = "navigation")]
pub use overview::*;
pub use protocol::*;
pub use routing::*;
#[cfg(any(feature = "navigation", feature = "backgrounds"))]
pub use settings::*;
