use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use handshake_canonical_json::{canonical_json_bytes, sha256_hex};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextBundleError {
    InvalidEvent(&'static str),
}

impl std::fmt::Display for ContextBundleError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidEvent(message) => write!(formatter, "invalid kernel event: {message}"),
        }
    }
}

impl std::error::Error for ContextBundleError {}

pub type ContextBundleResult<T> = Result<T, ContextBundleError>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ContextBundle {
    pub context_bundle_id: String,
    pub kernel_task_run_id: String,
    pub session_run_id: String,
    pub allowed_context: Value,
    pub context_hash: String,
    pub created_at: DateTime<Utc>,
}

impl ContextBundle {
    pub fn new_at(
        kernel_task_run_id: impl Into<String>,
        session_run_id: impl Into<String>,
        allowed_context: Value,
        created_at: DateTime<Utc>,
    ) -> ContextBundleResult<Self> {
        let kernel_task_run_id = kernel_task_run_id.into();
        let session_run_id = session_run_id.into();
        if kernel_task_run_id.trim().is_empty() {
            return Err(ContextBundleError::InvalidEvent(
                "kernel_task_run_id is required",
            ));
        }
        if session_run_id.trim().is_empty() {
            return Err(ContextBundleError::InvalidEvent("session_run_id is required"));
        }
        let context_hash = sha256_hex(&canonical_json_bytes(&allowed_context));
        let context_bundle_id = format!("CTX-{}", &context_hash[..16]);
        Ok(Self {
            context_bundle_id,
            kernel_task_run_id,
            session_run_id,
            allowed_context,
            context_hash,
            created_at,
        })
    }
}
