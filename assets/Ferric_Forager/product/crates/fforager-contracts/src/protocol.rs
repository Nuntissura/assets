//! Typed configuration, event, error, cancellation, process, plugin, and worker envelopes.

use crate::{
    CompatibilityRange, ExtensionLimits, ExtensionMap, JobId, ProducerId, RequestId, SchemaVersion,
};
use serde::{Deserialize, Serialize};

const CONFIG_SCHEMA: &str = "ff.config";
const EVENT_SCHEMA: &str = "ff.event";
const ERROR_SCHEMA: &str = "ff.error";
const CANCELLATION_SCHEMA: &str = "ff.cancel";
const PROCESS_SCHEMA: &str = "ff.process";
const PLUGIN_SCHEMA: &str = "ff.plugin";
const JAVASCRIPT_WORKER_SCHEMA: &str = "ff.javascript-worker";

/// Common envelope metadata used at every framed process boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvelopeHeader {
    pub schema_id: String,
    pub version: SchemaVersion,
    pub request_id: RequestId,
    pub producer_id: ProducerId,
    pub job_id: Option<JobId>,
    pub sequence: u64,
}

/// Bounded product configuration crossing a public boundary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigEnvelope {
    pub header: EnvelopeHeader,
    pub compatibility: CompatibilityRange,
    pub values: ExtensionMap,
}

/// Stable event criticality controls legal loss behavior.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventCriticality {
    Telemetry,
    Operational,
    Terminal,
    Crash,
    InvariantBreach,
}

/// Typed event; `kind` is separately validated against the negotiated inventory.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventEnvelope {
    pub header: EnvelopeHeader,
    pub kind: String,
    pub criticality: EventCriticality,
    pub sensitivity: Sensitivity,
    pub payload: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    Public,
    Operational,
    SensitiveRedacted,
}

/// Stable failure categories safe to transport across process boundaries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidInput,
    UnsupportedVersion,
    UnknownMandatoryKind,
    ResourceExhausted,
    Cancelled,
    TimedOut,
    DependencyFailed,
    InvariantViolation,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorEnvelope {
    pub header: EnvelopeHeader,
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    pub details: ExtensionMap,
}

/// Cancellation command names the request and the latest acceptable completion generation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancellationRequest {
    pub header: EnvelopeHeader,
    pub target_request_id: RequestId,
    pub generation: u64,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationOutcome {
    Accepted,
    AlreadyCompleted,
    AlreadyCancelled,
    UnknownRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancellationAcknowledgement {
    pub header: EnvelopeHeader,
    pub target_request_id: RequestId,
    pub generation: u64,
    pub outcome: CancellationOutcome,
}

/// Process protocol union. Unknown JSON tags fail deserialization as unknown mandatory kinds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ProcessEnvelope {
    Hello(Hello),
    Request(ProcessRequest),
    Event(EventEnvelope),
    Error(ErrorEnvelope),
    Cancel(CancellationRequest),
    CancelAcknowledged(CancellationAcknowledgement),
    Goodbye(Goodbye),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub header: EnvelopeHeader,
    pub accepted: CompatibilityRange,
    pub artifact_id: String,
    pub schema_hash: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessRequest {
    pub header: EnvelopeHeader,
    pub operation: String,
    pub payload: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Goodbye {
    pub header: EnvelopeHeader,
    pub completed: bool,
}

/// Plugin-specific wire request, nested inside a process request when applicable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PluginMessage {
    Describe,
    Invoke {
        plugin_id: String,
        capability: String,
        input: serde_json::Value,
    },
    Result {
        output: serde_json::Value,
    },
    Failed {
        error: ErrorCode,
        message: String,
    },
}

/// JavaScript worker wire request. Source code and secrets are referenced, not embedded.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum JavaScriptWorkerMessage {
    Evaluate {
        program_reference: String,
        input: serde_json::Value,
        deadline_millis: u64,
        memory_limit_bytes: u64,
    },
    Result {
        output: serde_json::Value,
    },
    Failed {
        error: ErrorCode,
        message: String,
    },
    Cancelled {
        generation: u64,
    },
}

/// Validation limits applied after bounded framing and before dispatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProtocolLimits {
    pub accepted_version: CompatibilityRange,
    pub maximum_schema_id_bytes: usize,
    pub maximum_kind_bytes: usize,
    pub maximum_message_bytes: usize,
    pub maximum_reference_bytes: usize,
    pub maximum_javascript_deadline_millis: u64,
    pub maximum_javascript_memory_bytes: u64,
}

impl Default for ProtocolLimits {
    fn default() -> Self {
        Self {
            accepted_version: CompatibilityRange {
                major: 1,
                minimum_minor: 0,
                maximum_minor: 1,
            },
            maximum_schema_id_bytes: 128,
            maximum_kind_bytes: 128,
            maximum_message_bytes: 8 * 1024,
            maximum_reference_bytes: 4 * 1024,
            maximum_javascript_deadline_millis: 5 * 60 * 1_000,
            maximum_javascript_memory_bytes: 512 * 1024 * 1024,
        }
    }
}

/// Protocol-level validation error independent of JSON syntax errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    FieldTooLong {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },
    UnknownMandatoryKind {
        kind: String,
    },
    DuplicateRequestId {
        request_id: RequestId,
    },
    InvalidSequence,
    InvalidVersion,
    UnexpectedSchema {
        expected: &'static str,
        received: String,
    },
    IncompatibleVersion {
        received: SchemaVersion,
        accepted: CompatibilityRange,
    },
    InvalidExtensions,
    InvalidField {
        field: &'static str,
    },
    NumericLimitExceeded {
        field: &'static str,
        actual: u64,
        maximum: u64,
    },
    CorrelationMismatch {
        field: &'static str,
    },
}

impl EnvelopeHeader {
    /// Validates header-local bounds before dispatch.
    ///
    /// # Errors
    ///
    /// Returns a typed field or sequence error.
    pub fn validate(&self, limits: ProtocolLimits) -> Result<(), ProtocolError> {
        validate_text("schema_id", &self.schema_id, limits.maximum_schema_id_bytes)?;
        if self.sequence == 0 {
            return Err(ProtocolError::InvalidSequence);
        }
        let major = parse_schema_id(&self.schema_id);
        if major != Some(self.version.major) || self.version.major == 0 {
            return Err(ProtocolError::InvalidVersion);
        }
        validate_compatibility(self.version, limits.accepted_version)
    }
}

impl ConfigEnvelope {
    /// Validates the header/version relationship, compatibility range, and extension budget.
    ///
    /// # Errors
    ///
    /// Returns a typed protocol error when any direct public-boundary field is invalid.
    pub fn validate(&self, limits: ProtocolLimits) -> Result<(), ProtocolError> {
        validate_header_schema(&self.header, CONFIG_SCHEMA, limits)?;
        validate_compatibility(self.header.version, self.compatibility)?;
        self.values
            .validate(ExtensionLimits::default())
            .map_err(|_| ProtocolError::InvalidExtensions)
    }
}

impl EventEnvelope {
    /// Validates direct event use outside a process-envelope decoder.
    ///
    /// # Errors
    ///
    /// Returns a typed error for invalid headers, kinds, or payload bounds.
    pub fn validate(&self, limits: ProtocolLimits) -> Result<(), ProtocolError> {
        validate_header_schema(&self.header, EVENT_SCHEMA, limits)?;
        validate_text("event_kind", &self.kind, limits.maximum_kind_bytes)?;
        validate_json("event_payload", &self.payload, limits.maximum_message_bytes)
    }
}

impl ErrorEnvelope {
    /// Validates direct error-envelope use and bounded extension details.
    ///
    /// # Errors
    ///
    /// Returns a typed error for invalid headers, messages, or details.
    pub fn validate(&self, limits: ProtocolLimits) -> Result<(), ProtocolError> {
        validate_header_schema(&self.header, ERROR_SCHEMA, limits)?;
        validate_text("error_message", &self.message, limits.maximum_message_bytes)?;
        self.details
            .validate(ExtensionLimits::default())
            .map_err(|_| ProtocolError::InvalidExtensions)
    }
}

impl CancellationRequest {
    /// Validates a direct cancellation request.
    ///
    /// # Errors
    ///
    /// Returns a typed error for invalid headers, generations, or reasons.
    pub fn validate(&self, limits: ProtocolLimits) -> Result<(), ProtocolError> {
        validate_header_schema(&self.header, CANCELLATION_SCHEMA, limits)?;
        if self.generation == 0 {
            return Err(ProtocolError::InvalidSequence);
        }
        validate_text("cancel_reason", &self.reason, limits.maximum_message_bytes)
    }
}

impl CancellationAcknowledgement {
    /// Validates a direct cancellation acknowledgement.
    ///
    /// # Errors
    ///
    /// Returns a typed error for invalid headers or generations.
    pub fn validate(&self, limits: ProtocolLimits) -> Result<(), ProtocolError> {
        validate_header_schema(&self.header, CANCELLATION_SCHEMA, limits)?;
        if self.generation == 0 {
            return Err(ProtocolError::InvalidSequence);
        }
        Ok(())
    }
}

/// Validates request/acknowledgement identity, version, generation, and sequence correlation.
///
/// # Errors
///
/// Returns a typed correlation error when the acknowledgement does not answer the request.
pub fn validate_cancellation_correlation(
    request: &CancellationRequest,
    acknowledgement: &CancellationAcknowledgement,
    expected_acknowledgement_producer: &ProducerId,
    limits: ProtocolLimits,
) -> Result<(), ProtocolError> {
    request.validate(limits)?;
    acknowledgement.validate(limits)?;
    for (matches, field) in [
        (
            request.header.request_id == acknowledgement.header.request_id,
            "header.request_id",
        ),
        (
            request.header.schema_id == acknowledgement.header.schema_id,
            "header.schema_id",
        ),
        (
            request.header.version == acknowledgement.header.version,
            "header.version",
        ),
        (
            request.header.job_id == acknowledgement.header.job_id,
            "header.job_id",
        ),
        (
            acknowledgement.header.producer_id == *expected_acknowledgement_producer,
            "header.producer_id",
        ),
        (
            request.target_request_id == acknowledgement.target_request_id,
            "target_request_id",
        ),
        (
            request.generation == acknowledgement.generation,
            "generation",
        ),
        (
            acknowledgement.header.sequence > request.header.sequence,
            "header.sequence",
        ),
    ] {
        if !matches {
            return Err(ProtocolError::CorrelationMismatch { field });
        }
    }
    Ok(())
}

impl ProcessEnvelope {
    /// Recursively validates every process-boundary field after deserialization.
    ///
    /// # Errors
    ///
    /// Returns a typed error for invalid headers, over-limit strings/payloads, or extensions.
    pub fn validate(&self, limits: ProtocolLimits) -> Result<(), ProtocolError> {
        match self {
            Self::Hello(value) => {
                validate_header_schema(&value.header, PROCESS_SCHEMA, limits)?;
                validate_compatibility(value.header.version, value.accepted)?;
                validate_text("artifact_id", &value.artifact_id, limits.maximum_kind_bytes)?;
                validate_text("schema_hash", &value.schema_hash, limits.maximum_kind_bytes)
            }
            Self::Request(value) => {
                validate_header_schema(&value.header, PROCESS_SCHEMA, limits)?;
                validate_text("operation", &value.operation, limits.maximum_kind_bytes)?;
                validate_json(
                    "request_payload",
                    &value.payload,
                    limits.maximum_message_bytes,
                )
            }
            Self::Event(value) => value.validate(limits),
            Self::Error(value) => value.validate(limits),
            Self::Cancel(value) => value.validate(limits),
            Self::CancelAcknowledged(value) => value.validate(limits),
            Self::Goodbye(value) => validate_header_schema(&value.header, PROCESS_SCHEMA, limits),
        }
    }
}

/// Provenance required on isolated plugin and JavaScript-worker exchanges.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundaryProvenance {
    pub artifact_id: String,
    pub schema_hash: String,
    pub capability_grant_id: String,
}

/// Complete plugin boundary envelope; callers cannot dispatch a bare plugin payload.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginEnvelope {
    pub header: EnvelopeHeader,
    pub compatibility: CompatibilityRange,
    pub provenance: BoundaryProvenance,
    pub message: PluginMessage,
}

/// Complete JavaScript-worker boundary envelope with explicit provenance and correlation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JavaScriptWorkerEnvelope {
    pub header: EnvelopeHeader,
    pub compatibility: CompatibilityRange,
    pub provenance: BoundaryProvenance,
    pub message: JavaScriptWorkerMessage,
}

impl PluginEnvelope {
    /// Validates plugin metadata and recursively bounded payloads.
    ///
    /// # Errors
    ///
    /// Returns a typed protocol bound or header error.
    pub fn validate(&self, limits: ProtocolLimits) -> Result<(), ProtocolError> {
        validate_boundary_header(&self.header, PLUGIN_SCHEMA, limits)?;
        validate_compatibility(self.header.version, self.compatibility)?;
        validate_provenance(&self.provenance, limits)?;
        match &self.message {
            PluginMessage::Describe => Ok(()),
            PluginMessage::Invoke {
                plugin_id,
                capability,
                input,
            } => {
                validate_text("plugin_id", plugin_id, limits.maximum_kind_bytes)?;
                validate_text("plugin_capability", capability, limits.maximum_kind_bytes)?;
                validate_json("plugin_input", input, limits.maximum_message_bytes)
            }
            PluginMessage::Result { output } => {
                validate_json("plugin_output", output, limits.maximum_message_bytes)
            }
            PluginMessage::Failed { message, .. } => {
                validate_text("plugin_error", message, limits.maximum_message_bytes)
            }
        }
    }
}

impl JavaScriptWorkerEnvelope {
    /// Validates worker metadata and recursively bounded payloads.
    ///
    /// # Errors
    ///
    /// Returns a typed protocol bound or header error.
    pub fn validate(&self, limits: ProtocolLimits) -> Result<(), ProtocolError> {
        validate_boundary_header(&self.header, JAVASCRIPT_WORKER_SCHEMA, limits)?;
        validate_compatibility(self.header.version, self.compatibility)?;
        validate_provenance(&self.provenance, limits)?;
        match &self.message {
            JavaScriptWorkerMessage::Evaluate {
                program_reference,
                input,
                deadline_millis,
                memory_limit_bytes,
            } => {
                validate_reference(
                    "program_reference",
                    program_reference,
                    limits.maximum_reference_bytes,
                )?;
                validate_numeric_limit(
                    "javascript_deadline_millis",
                    *deadline_millis,
                    limits.maximum_javascript_deadline_millis,
                )?;
                validate_numeric_limit(
                    "javascript_memory_limit_bytes",
                    *memory_limit_bytes,
                    limits.maximum_javascript_memory_bytes,
                )?;
                validate_json("javascript_input", input, limits.maximum_message_bytes)
            }
            JavaScriptWorkerMessage::Result { output } => {
                validate_json("javascript_output", output, limits.maximum_message_bytes)
            }
            JavaScriptWorkerMessage::Failed { message, .. } => {
                validate_text("javascript_error", message, limits.maximum_message_bytes)
            }
            JavaScriptWorkerMessage::Cancelled { generation } => {
                if *generation == 0 {
                    return Err(ProtocolError::InvalidSequence);
                }
                Ok(())
            }
        }
    }
}

fn validate_provenance(
    provenance: &BoundaryProvenance,
    limits: ProtocolLimits,
) -> Result<(), ProtocolError> {
    validate_reference(
        "artifact_id",
        &provenance.artifact_id,
        limits.maximum_reference_bytes,
    )?;
    validate_reference(
        "capability_grant_id",
        &provenance.capability_grant_id,
        limits.maximum_reference_bytes,
    )?;
    if provenance.schema_hash.len() != 64
        || !provenance
            .schema_hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ProtocolError::InvalidField {
            field: "schema_hash",
        });
    }
    Ok(())
}

fn parse_schema_id(schema_id: &str) -> Option<u16> {
    let mut parts = schema_id.split('@');
    let name = parts.next()?;
    let major = parts.next()?.parse::<u16>().ok()?;
    if parts.next().is_some()
        || name.is_empty()
        || !name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        })
    {
        return None;
    }
    Some(major)
}

fn validate_header_schema(
    header: &EnvelopeHeader,
    expected: &'static str,
    limits: ProtocolLimits,
) -> Result<(), ProtocolError> {
    header.validate(limits)?;
    let Some((name, _)) = header.schema_id.split_once('@') else {
        return Err(ProtocolError::InvalidVersion);
    };
    if name != expected {
        return Err(ProtocolError::UnexpectedSchema {
            expected,
            received: header.schema_id.clone(),
        });
    }
    Ok(())
}

fn validate_boundary_header(
    header: &EnvelopeHeader,
    expected: &'static str,
    limits: ProtocolLimits,
) -> Result<(), ProtocolError> {
    validate_header_schema(header, expected, limits)?;
    if header.job_id.is_none() {
        return Err(ProtocolError::InvalidField {
            field: "header.job_id",
        });
    }
    Ok(())
}

fn validate_compatibility(
    version: SchemaVersion,
    compatibility: CompatibilityRange,
) -> Result<(), ProtocolError> {
    if compatibility.minimum_minor > compatibility.maximum_minor
        || compatibility.check(version).is_err()
    {
        return Err(ProtocolError::IncompatibleVersion {
            received: version,
            accepted: compatibility,
        });
    }
    Ok(())
}

fn validate_text(field: &'static str, value: &str, maximum: usize) -> Result<(), ProtocolError> {
    if value.is_empty() || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(ProtocolError::InvalidField { field });
    }
    if value.len() > maximum {
        return Err(ProtocolError::FieldTooLong {
            field,
            actual: value.len(),
            maximum,
        });
    }
    Ok(())
}

fn validate_reference(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), ProtocolError> {
    validate_text(field, value, maximum)?;
    let normalized = value.replace('\\', "/");
    if normalized.starts_with('/')
        || normalized.contains(':')
        || normalized
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
    {
        return Err(ProtocolError::InvalidField { field });
    }
    Ok(())
}

fn validate_numeric_limit(
    field: &'static str,
    actual: u64,
    maximum: u64,
) -> Result<(), ProtocolError> {
    if actual == 0 || actual > maximum {
        return Err(ProtocolError::NumericLimitExceeded {
            field,
            actual,
            maximum,
        });
    }
    Ok(())
}

fn validate_json(
    field: &'static str,
    value: &serde_json::Value,
    maximum: usize,
) -> Result<(), ProtocolError> {
    let actual = serde_json::to_vec(value)
        .map_err(|_| ProtocolError::FieldTooLong {
            field,
            actual: usize::MAX,
            maximum,
        })?
        .len();
    if actual > maximum {
        return Err(ProtocolError::FieldTooLong {
            field,
            actual,
            maximum,
        });
    }
    Ok(())
}

/// Bounded registry of request IDs for duplicate/replay detection.
#[derive(Clone, Debug)]
pub struct RequestRegistry {
    maximum_entries: usize,
    entries: std::collections::BTreeSet<RequestId>,
}

impl RequestRegistry {
    #[must_use]
    pub fn new(maximum_entries: usize) -> Self {
        Self {
            maximum_entries,
            entries: std::collections::BTreeSet::new(),
        }
    }

    /// Registers a request once. At capacity, callers must retire entries before accepting more.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError`] when the ID is duplicated or the registry is full.
    pub fn register(&mut self, request_id: RequestId) -> Result<(), ProtocolError> {
        if self.entries.contains(&request_id) {
            return Err(ProtocolError::DuplicateRequestId { request_id });
        }
        if self.entries.len() >= self.maximum_entries {
            return Err(ProtocolError::FieldTooLong {
                field: "request_registry",
                actual: self.entries.len() + 1,
                maximum: self.maximum_entries,
            });
        }
        self.entries.insert(request_id);
        Ok(())
    }

    #[must_use]
    pub fn retire(&mut self, request_id: &RequestId) -> bool {
        self.entries.remove(request_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(
        schema_id: &str,
        request_id: &str,
        sequence: u64,
    ) -> Result<EnvelopeHeader, crate::IdError> {
        Ok(EnvelopeHeader {
            schema_id: schema_id.into(),
            version: SchemaVersion { major: 1, minor: 0 },
            request_id: RequestId::new(request_id)?,
            producer_id: ProducerId::new("producer_test")?,
            job_id: Some(JobId::new("job_test")?),
            sequence,
        })
    }

    #[test]
    fn unknown_mandatory_process_kind_is_rejected() {
        let json = br#"{"kind":"future_required","body":{}}"#;
        let result = serde_json::from_slice::<ProcessEnvelope>(json);
        assert!(result.is_err());
    }

    #[test]
    fn direct_envelopes_enforce_version_and_payload_bounds() -> Result<(), crate::IdError> {
        let config = ConfigEnvelope {
            header: header("ff.config@1", "request_config", 1)?,
            compatibility: CompatibilityRange {
                major: 2,
                minimum_minor: 0,
                maximum_minor: 0,
            },
            values: ExtensionMap::default(),
        };
        assert!(matches!(
            config.validate(ProtocolLimits::default()),
            Err(ProtocolError::IncompatibleVersion { .. })
        ));

        let event = EventEnvelope {
            header: header("ff.event@1", "request_event", 1)?,
            kind: "progress".into(),
            criticality: EventCriticality::Operational,
            sensitivity: Sensitivity::Operational,
            payload: serde_json::Value::String("x".repeat(32)),
        };
        assert!(matches!(
            event.validate(ProtocolLimits {
                maximum_message_bytes: 8,
                ..ProtocolLimits::default()
            }),
            Err(ProtocolError::FieldTooLong {
                field: "event_payload",
                ..
            })
        ));
        let error = ErrorEnvelope {
            header: header("ff.error@1", "request_error", 1)?,
            code: ErrorCode::InvalidInput,
            message: String::new(),
            retryable: false,
            details: ExtensionMap::default(),
        };
        assert!(matches!(
            error.validate(ProtocolLimits::default()),
            Err(ProtocolError::InvalidField {
                field: "error_message"
            })
        ));
        Ok(())
    }

    #[test]
    fn cancellation_validation_rejects_zero_and_mismatched_ack() -> Result<(), crate::IdError> {
        let target = RequestId::new("request_target")?;
        let request = CancellationRequest {
            header: header("ff.cancel@1", "request_cancel", 1)?,
            target_request_id: target.clone(),
            generation: 1,
            reason: "operator_cancelled".into(),
        };
        let zero = CancellationRequest {
            generation: 0,
            ..request.clone()
        };
        assert_eq!(
            zero.validate(ProtocolLimits::default()),
            Err(ProtocolError::InvalidSequence)
        );
        let mut acknowledgement = CancellationAcknowledgement {
            header: header("ff.cancel@1", "request_cancel", 2)?,
            target_request_id: target,
            generation: 1,
            outcome: CancellationOutcome::Accepted,
        };
        let expected_producer = ProducerId::new("producer_trusted_acknowledger")?;
        acknowledgement.header.producer_id = expected_producer.clone();
        assert_eq!(
            validate_cancellation_correlation(
                &request,
                &acknowledgement,
                &expected_producer,
                ProtocolLimits::default()
            ),
            Ok(())
        );
        acknowledgement.header.request_id = RequestId::new("request_other")?;
        assert_eq!(
            validate_cancellation_correlation(
                &request,
                &acknowledgement,
                &expected_producer,
                ProtocolLimits::default()
            ),
            Err(ProtocolError::CorrelationMismatch {
                field: "header.request_id"
            })
        );
        acknowledgement.header.request_id = request.header.request_id.clone();
        acknowledgement.header.producer_id = ProducerId::new("producer_other")?;
        assert_eq!(
            validate_cancellation_correlation(
                &request,
                &acknowledgement,
                &expected_producer,
                ProtocolLimits::default()
            ),
            Err(ProtocolError::CorrelationMismatch {
                field: "header.producer_id"
            })
        );
        Ok(())
    }

    #[test]
    fn rejects_self_declared_versions_and_cross_type_schema_ids() -> Result<(), crate::IdError> {
        let mut config = ConfigEnvelope {
            header: header("ff.config@1", "request_config_schema", 1)?,
            compatibility: CompatibilityRange {
                major: 1,
                minimum_minor: 0,
                maximum_minor: 1,
            },
            values: ExtensionMap::default(),
        };
        config.header.schema_id = "ff.config@99".into();
        config.header.version = SchemaVersion {
            major: 99,
            minor: u16::MAX,
        };
        config.compatibility = CompatibilityRange {
            major: 99,
            minimum_minor: 0,
            maximum_minor: u16::MAX,
        };
        assert!(matches!(
            config.validate(ProtocolLimits::default()),
            Err(ProtocolError::IncompatibleVersion { .. })
        ));
        config.header.schema_id = "ff.plugin@1".into();
        config.header.version = SchemaVersion { major: 1, minor: 0 };
        config.compatibility = ProtocolLimits::default().accepted_version;
        assert_eq!(
            config.validate(ProtocolLimits::default()),
            Err(ProtocolError::UnexpectedSchema {
                expected: CONFIG_SCHEMA,
                received: "ff.plugin@1".into(),
            })
        );
        Ok(())
    }

    #[test]
    fn strict_nested_serde_and_worker_resource_policy_fail_closed() -> Result<(), crate::IdError> {
        let plugin_with_unknown = r#"{
            "header":{"schema_id":"ff.plugin@1","version":{"major":1,"minor":0},"request_id":"request_plugin","producer_id":"producer_plugin","job_id":"job_plugin","sequence":1},
            "compatibility":{"major":1,"minimum_minor":0,"maximum_minor":1},
            "provenance":{"artifact_id":"artifact-1","schema_hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","capability_grant_id":"grant-1"},
            "message":{"kind":"invoke","body":{"plugin_id":"plugin-1","capability":"resolve","input":{},"smuggled":true}}
        }"#;
        assert!(serde_json::from_str::<PluginEnvelope>(plugin_with_unknown).is_err());
        let provenance = BoundaryProvenance {
            artifact_id: "artifact-1".into(),
            schema_hash: "a".repeat(64),
            capability_grant_id: "grant-1".into(),
        };
        let mut plugin = PluginEnvelope {
            header: header("ff.plugin@1", "request_plugin_job", 1)?,
            compatibility: ProtocolLimits::default().accepted_version,
            provenance: provenance.clone(),
            message: PluginMessage::Describe,
        };
        plugin.header.job_id = None;
        assert_eq!(
            plugin.validate(ProtocolLimits::default()),
            Err(ProtocolError::InvalidField {
                field: "header.job_id"
            })
        );
        let mut worker = JavaScriptWorkerEnvelope {
            header: header("ff.javascript-worker@1", "request_worker_limits", 1)?,
            compatibility: ProtocolLimits::default().accepted_version,
            provenance,
            message: JavaScriptWorkerMessage::Evaluate {
                program_reference: "program-1".into(),
                input: serde_json::Value::Null,
                deadline_millis: u64::MAX,
                memory_limit_bytes: u64::MAX,
            },
        };
        assert!(matches!(
            worker.validate(ProtocolLimits::default()),
            Err(ProtocolError::NumericLimitExceeded { .. })
        ));
        worker.message = JavaScriptWorkerMessage::Evaluate {
            program_reference: "../../host-secret".into(),
            input: serde_json::Value::Null,
            deadline_millis: 1,
            memory_limit_bytes: 1,
        };
        assert_eq!(
            worker.validate(ProtocolLimits::default()),
            Err(ProtocolError::InvalidField {
                field: "program_reference"
            })
        );
        worker.provenance.schema_hash = "not-a-hash".into();
        worker.message = JavaScriptWorkerMessage::Result {
            output: serde_json::Value::Null,
        };
        assert_eq!(
            worker.validate(ProtocolLimits::default()),
            Err(ProtocolError::InvalidField {
                field: "schema_hash"
            })
        );
        worker.provenance.schema_hash = "a".repeat(64);
        worker.header.job_id = None;
        assert_eq!(
            worker.validate(ProtocolLimits::default()),
            Err(ProtocolError::InvalidField {
                field: "header.job_id"
            })
        );
        Ok(())
    }
}
