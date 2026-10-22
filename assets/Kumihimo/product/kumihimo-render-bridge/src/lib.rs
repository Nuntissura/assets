//! Bounded host-owned pixels, capture scheduling and lifecycle contracts.
//!
//! The default package uses only `std`. The optional `desktop` feature connects
//! validated CPU pixels and host scenes to the existing Kumihimo desktop adapter.
//! Browser engines remain external processes; this crate does not link Servo.
//!
//! Hosts supply complete content scopes, independent geometry/engine/renderer
//! epochs, limits and actual time. Namespace admission and process-termination
//! proof belong to the independent host controller, not the engine worker.

pub mod frame;
pub mod lifecycle;
pub mod mapping;
pub mod scheduler;

#[cfg(feature = "desktop")]
pub mod gpu;
#[cfg(feature = "desktop")]
pub mod provider;

pub use frame::*;
pub use lifecycle::*;
pub use mapping::*;
pub use scheduler::*;
