//! Portable RSS source, acquisition and owner contracts. Hosts execute and authorize effects.
pub mod sources;
pub mod source_tags;
pub use source_tags::{SourceTags, SOURCE_TAGS_EXTENSION, MAX_SOURCE_TAGS};
pub mod acquisition;
pub mod ports;
pub mod transfer;
pub use sources::*;
pub use acquisition::*;
pub use ports::*;
pub use hsscrap::{HostRef, ScopeRef, ItemId, ObservationId, OperationId, Revision, Instant, TagRef, Extensions, Bounds};
pub const CONTRACT_VERSION: &str = "rss.contracts.v1";

use serde::{Serialize, Deserialize};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum Error {
    Invalid { field: String },
    LimitExceeded { field: String },
    Denied,
    NotFound,
    Unsupported { provider: Provider, mode: AccessMode },
    ReconnectRequired,
    Expired,
    Challenged,
    RateLimited { retry_after: Option<Instant> },
    Network,
    Conflict,
    Cancelled,
    StaleResult,
    UnknownOutcome { operation_id: OperationId },
    ItemOwner { error: hsscrap::Error },
}
pub type Result<T> = std::result::Result<T, Error>;
impl From<hsscrap::Error> for Error {
    fn from(error: hsscrap::Error) -> Self { Self::ItemOwner { error } }
}
pub(crate) fn invalid(field: &str) -> Error { Error::Invalid { field: field.into() } }
pub(crate) fn bounded(value: &str, field: &str, max: usize) -> Result<()> {
    if value.len() > max { return Err(Error::LimitExceeded { field: field.into() }); }
    Ok(())
}
pub(crate) fn validate_ref(value: &HostRef, bounds: &Bounds) -> Result<()> {
    for field in [&value.host_instance_id, &value.owner_kind, &value.native_id] {
        if field.is_empty() { return Err(invalid("host_ref")); }
        bounded(field, "host_ref", bounds.max_text_bytes)?;
    }
    Ok(())
}
