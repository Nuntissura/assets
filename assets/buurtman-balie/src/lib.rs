//! Host-independent CRM. All effects enter through authorization and atomic storage ports.
mod contracts;
pub mod memory;
mod pricing;
mod service;
#[cfg(feature = "sqlite")]
pub mod sqlite;
#[cfg(feature = "surreal")]
pub mod surreal;
pub mod worker;
pub use contracts::*;
pub use pricing::*;
pub use service::Crm;
pub const API_VERSION: u32 = 2;
