//! Headless content contracts. Hosts execute effects and report their outcomes.
//! Dropping a controller drops metadata only; it never disposes host engines.
#![forbid(unsafe_code)]
mod diagnostics;
mod lifecycle;
mod protocol;
mod routing;
mod scheduler;
pub use diagnostics::*;
pub use protocol::*;
pub use routing::*;
