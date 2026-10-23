//! Rust owns arrangement; hosts own task meaning and browser effects.
#![forbid(unsafe_code)]
mod host;
pub mod projection;
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
mod wasm;
pub mod wire;
pub use host::{HostDispatcher, PilotConfig, PilotHost, WebAdapter};
use serde::Serialize;

/// Redacted boundary failure: caller data is never included in diagnostics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WebError {
    pub code: String,
    pub scope: Option<String>,
    pub retryable: bool,
}
impl WebError {
    pub fn new(code: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            scope: None,
            retryable: false,
        }
    }
    pub fn scoped(mut self, scope: impl Into<String>) -> Self {
        self.scope = Some(scope.into());
        self
    }
}
impl std::fmt::Display for WebError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.code)
    }
}
impl std::error::Error for WebError {}
impl From<kumihimo_core::Error> for WebError {
    fn from(error: kumihimo_core::Error) -> Self {
        use kumihimo_core::Error::*;
        Self::new(match error {
            IdentityMismatch | InvalidIdentity => "identity_mismatch",
            StaleGeneration { .. } => "stale_generation",
            StalePreview { .. } => "stale_preview",
            IntentMismatch(_) => "intent_mismatch",
            ExpiredOperation(_) => "expired_operation",
            ZeroOperation => "zero_operation",
            Missing(_) | ClosedView(_) => "missing_target",
            Admission { .. } => "admission",
            CloseVeto(_) => "close_veto",
            Pinned(_) => "pinned",
            Geometry(_) => "invalid_geometry",
            ForbiddenDestination => "forbidden_destination",
            Cycle { .. } => "cycle",
            UnsupportedSchema { .. } => "unsupported_schema",
            RestoreConflict => "restore_conflict",
            _ => "invalid_state",
        })
    }
}
impl From<kumihimo_content::Error> for WebError {
    fn from(error: kumihimo_content::Error) -> Self {
        use kumihimo_content::Error::*;
        match error {
            Core(error) => error.into(),
            IdentityMismatch | SessionMismatch => Self::new("identity_mismatch"),
            Missing(_, _) => Self::new("missing_target"),
            Stale(_) => Self::new("stale_content"),
            Admission { .. } => Self::new("admission"),
            PermissionDenied => Self::new("permission_denied"),
            Unsupported(_) => Self::new("unsupported"),
            Invalid(cause) => Self::new(match cause {
                "geometry clip" => "content_geometry_clip",
                "geometry scale" => "content_geometry_scale",
                "acknowledgment result kind/content" => "content_acknowledgment",
                _ => "content_invalid",
            })
            .scoped(cause),
            Expired(request) => Self::new("content_expired").scoped(request.0.to_string()),
            Conflict(request) => Self::new("content_conflict").scoped(request.0.to_string()),
            Busy(instance) => Self::new("content_busy").scoped(instance.0.to_string()),
            TimeWentBack => Self::new("content_time_went_back"),
            Exhausted(cause) => Self::new("content_exhausted").scoped(cause),
        }
    }
}
