//! Portable hsscrap model and owner boundaries. Hosts execute effects and authorize access.
pub mod errors;
pub mod types;
pub mod ports;
pub use errors::*;
pub use types::*;
pub mod validation;
pub use validation::{decode, Validate};
mod record_validation;
pub mod tags;
pub mod organization;
pub mod identity;
pub mod metadata;
pub mod gallery;
pub mod media_refs;
