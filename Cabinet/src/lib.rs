//! Cabinet's mechanics do not perform I/O. Hosts execute requests and return canonical snapshots.
pub mod bin;
pub mod contracts;
mod core;
pub use cabinet_peak as peak;
pub use contracts::*;
pub use core::Cabinet;
pub mod interaction;
