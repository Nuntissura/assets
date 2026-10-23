use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum Error {
    Invalid { field: String, reason: String },
    LimitExceeded { field: String, limit: usize },
    UnsupportedRequiredFeature { feature: String },
    Denied,
    NotFound,
    Conflict { field: String },
    ProjectContextUnresolved,
    UnknownOutcome { operation_id: crate::OperationId },
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{self:?}") }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;
pub fn invalid(field: &str, reason: &str) -> Error { Error::Invalid { field: field.into(), reason: reason.into() } }
