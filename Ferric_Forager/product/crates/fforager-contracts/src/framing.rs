//! Incremental four-byte big-endian length-prefixed JSON framing.
//!
//! The declared length is checked before payload allocation. A decoder retains at
//! most four header bytes plus one configured maximum frame.

use crate::{
    CompatibilityRange, JavaScriptWorkerEnvelope, PluginEnvelope, ProcessEnvelope, ProtocolLimits,
};
use serde::de::DeserializeOwned;
use std::collections::BTreeSet;
use std::fmt;

const HEADER_BYTES: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameLimits {
    pub maximum_frame_bytes: usize,
}

impl Default for FrameLimits {
    fn default() -> Self {
        Self {
            maximum_frame_bytes: 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameError {
    ZeroLength,
    Oversized {
        declared: usize,
        maximum: usize,
    },
    PartialHeader {
        received: usize,
    },
    PartialPayload {
        declared: usize,
        received: usize,
    },
    InvalidJson {
        message: String,
    },
    IncompatibleVersion {
        received_major: u16,
        supported_major: u16,
    },
    UnknownMandatoryKind {
        kind: String,
    },
    DuplicateRequestId {
        request_id: String,
    },
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "frame error: {self:?}")
    }
}
impl std::error::Error for FrameError {}

/// Incremental bounded frame decoder.
#[derive(Clone, Debug)]
pub struct FrameDecoder {
    limits: FrameLimits,
    header: [u8; HEADER_BYTES],
    header_len: usize,
    declared: Option<usize>,
    payload: Vec<u8>,
}

impl FrameDecoder {
    #[must_use]
    pub fn new(limits: FrameLimits) -> Self {
        Self {
            limits,
            header: [0; HEADER_BYTES],
            header_len: 0,
            declared: None,
            payload: Vec::new(),
        }
    }

    /// Consumes bytes until a complete payload is available, returning how many input bytes were used.
    ///
    /// # Errors
    ///
    /// Returns [`FrameError::ZeroLength`] or [`FrameError::Oversized`] immediately
    /// after a complete invalid header is received.
    pub fn push(&mut self, input: &[u8]) -> Result<(usize, Option<Vec<u8>>), FrameError> {
        let mut used = 0usize;
        if self.declared.is_none() {
            let wanted = HEADER_BYTES - self.header_len;
            let take = wanted.min(input.len());
            self.header[self.header_len..self.header_len + take].copy_from_slice(&input[..take]);
            self.header_len += take;
            used += take;
            if self.header_len < HEADER_BYTES {
                return Ok((used, None));
            }
            let declared = u32::from_be_bytes(self.header) as usize;
            if declared == 0 {
                self.reset();
                return Err(FrameError::ZeroLength);
            }
            if declared > self.limits.maximum_frame_bytes {
                self.reset();
                return Err(FrameError::Oversized {
                    declared,
                    maximum: self.limits.maximum_frame_bytes,
                });
            }
            self.payload = Vec::with_capacity(declared);
            self.declared = Some(declared);
        }
        let declared = self.declared.unwrap_or_default();
        let remaining = declared.saturating_sub(self.payload.len());
        let take = remaining.min(input.len().saturating_sub(used));
        self.payload.extend_from_slice(&input[used..used + take]);
        used += take;
        if self.payload.len() == declared {
            let frame = std::mem::take(&mut self.payload);
            self.reset();
            Ok((used, Some(frame)))
        } else {
            Ok((used, None))
        }
    }

    /// Signals end-of-stream and produces a typed partial-frame error.
    ///
    /// # Errors
    ///
    /// Returns a typed partial-header or partial-payload error when buffered data remains.
    pub fn finish(&mut self) -> Result<(), FrameError> {
        let error = if self.header_len > 0 && self.declared.is_none() {
            Some(FrameError::PartialHeader {
                received: self.header_len,
            })
        } else {
            self.declared.map(|declared| FrameError::PartialPayload {
                declared,
                received: self.payload.len(),
            })
        };
        self.reset();
        error.map_or(Ok(()), Err)
    }

    /// Parses a complete payload with a bounded JSON deserializer.
    ///
    /// # Errors
    ///
    /// Returns [`FrameError::Oversized`] before parsing or [`FrameError::InvalidJson`].
    pub fn decode_json<T: DeserializeOwned>(
        payload: &[u8],
        limits: FrameLimits,
    ) -> Result<T, FrameError> {
        if payload.len() > limits.maximum_frame_bytes {
            return Err(FrameError::Oversized {
                declared: payload.len(),
                maximum: limits.maximum_frame_bytes,
            });
        }
        serde_json::from_slice(payload).map_err(|error| FrameError::InvalidJson {
            message: error.to_string(),
        })
    }

    /// Decodes a process envelope while distinguishing an unknown mandatory kind
    /// from general malformed JSON.
    ///
    /// # Errors
    ///
    /// Returns a typed oversize, malformed JSON, or unknown mandatory-kind error.
    pub fn decode_process(
        payload: &[u8],
        limits: FrameLimits,
    ) -> Result<ProcessEnvelope, FrameError> {
        Self::decode_process_with_limits(payload, limits, ProtocolLimits::default())
    }

    /// Decodes and recursively validates a process envelope with caller-selected limits.
    ///
    /// # Errors
    ///
    /// Returns typed framing errors for malformed, unknown, oversized, or invalid fields.
    pub fn decode_process_with_limits(
        payload: &[u8],
        limits: FrameLimits,
        protocol_limits: ProtocolLimits,
    ) -> Result<ProcessEnvelope, FrameError> {
        if payload.len() > limits.maximum_frame_bytes {
            return Err(FrameError::Oversized {
                declared: payload.len(),
                maximum: limits.maximum_frame_bytes,
            });
        }
        let value: serde_json::Value = Self::decode_json(payload, limits)?;
        if let Some(kind) = value.get("kind").and_then(serde_json::Value::as_str) {
            const KNOWN: [&str; 7] = [
                "hello",
                "request",
                "event",
                "error",
                "cancel",
                "cancel_acknowledged",
                "goodbye",
            ];
            if !KNOWN.contains(&kind) {
                return Err(FrameError::UnknownMandatoryKind {
                    kind: kind.to_owned(),
                });
            }
        }
        let envelope: ProcessEnvelope =
            serde_json::from_value(value).map_err(|error| FrameError::InvalidJson {
                message: error.to_string(),
            })?;
        envelope
            .validate(protocol_limits)
            .map_err(|error| FrameError::InvalidJson {
                message: format!("protocol validation failed: {error:?}"),
            })?;
        Ok(envelope)
    }

    /// Decodes and validates a bounded plugin IPC envelope.
    ///
    /// # Errors
    ///
    /// Returns typed framing errors for malformed, unknown, oversized, or invalid fields.
    pub fn decode_plugin(
        payload: &[u8],
        limits: FrameLimits,
    ) -> Result<PluginEnvelope, FrameError> {
        Self::decode_plugin_with_limits(payload, limits, ProtocolLimits::default())
    }

    /// Decodes and validates a bounded plugin IPC envelope with caller-selected policy limits.
    ///
    /// # Errors
    ///
    /// Returns typed framing errors for malformed, unknown, oversized, or invalid fields.
    pub fn decode_plugin_with_limits(
        payload: &[u8],
        limits: FrameLimits,
        protocol_limits: ProtocolLimits,
    ) -> Result<PluginEnvelope, FrameError> {
        let value = Self::decode_bounded_value(payload, limits)?;
        reject_unknown_nested_kind(&value, &["describe", "invoke", "result", "failed"])?;
        let envelope: PluginEnvelope =
            serde_json::from_value(value).map_err(|error| invalid_json(&error))?;
        envelope
            .validate(protocol_limits)
            .map_err(|error| invalid_protocol(&error))?;
        Ok(envelope)
    }

    /// Decodes and validates a bounded JavaScript-worker IPC envelope.
    ///
    /// # Errors
    ///
    /// Returns typed framing errors for malformed, unknown, oversized, or invalid fields.
    pub fn decode_javascript_worker(
        payload: &[u8],
        limits: FrameLimits,
    ) -> Result<JavaScriptWorkerEnvelope, FrameError> {
        Self::decode_javascript_worker_with_limits(payload, limits, ProtocolLimits::default())
    }

    /// Decodes and validates a bounded JavaScript-worker envelope with caller-selected policy limits.
    ///
    /// # Errors
    ///
    /// Returns typed framing errors for malformed, unknown, oversized, or invalid fields.
    pub fn decode_javascript_worker_with_limits(
        payload: &[u8],
        limits: FrameLimits,
        protocol_limits: ProtocolLimits,
    ) -> Result<JavaScriptWorkerEnvelope, FrameError> {
        let value = Self::decode_bounded_value(payload, limits)?;
        reject_unknown_nested_kind(&value, &["evaluate", "result", "failed", "cancelled"])?;
        let envelope: JavaScriptWorkerEnvelope =
            serde_json::from_value(value).map_err(|error| invalid_json(&error))?;
        envelope
            .validate(protocol_limits)
            .map_err(|error| invalid_protocol(&error))?;
        Ok(envelope)
    }

    fn decode_bounded_value(
        payload: &[u8],
        limits: FrameLimits,
    ) -> Result<serde_json::Value, FrameError> {
        if payload.len() > limits.maximum_frame_bytes {
            return Err(FrameError::Oversized {
                declared: payload.len(),
                maximum: limits.maximum_frame_bytes,
            });
        }
        Self::decode_json(payload, limits)
    }

    fn reset(&mut self) {
        self.header = [0; HEADER_BYTES];
        self.header_len = 0;
        self.declared = None;
        self.payload.clear();
    }
}

/// Stateful conformance decoder adding version and duplicate-request checks.
#[derive(Clone, Debug)]
pub struct ProcessConformance {
    compatibility: CompatibilityRange,
    protocol_limits: ProtocolLimits,
    maximum_seen_requests: usize,
    seen_requests: BTreeSet<String>,
}

impl ProcessConformance {
    #[must_use]
    pub fn new(compatibility: CompatibilityRange, maximum_seen_requests: usize) -> Self {
        let protocol_limits = ProtocolLimits {
            accepted_version: compatibility,
            ..ProtocolLimits::default()
        };
        Self {
            compatibility,
            protocol_limits,
            maximum_seen_requests,
            seen_requests: BTreeSet::new(),
        }
    }

    /// Checks negotiated version and bounded duplicate-request history.
    ///
    /// # Errors
    ///
    /// Returns [`FrameError`] for an incompatible version, duplicate request, or full registry.
    pub fn validate(&mut self, envelope: &ProcessEnvelope) -> Result<(), FrameError> {
        envelope
            .validate(self.protocol_limits)
            .map_err(|error| invalid_protocol(&error))?;
        let header = match envelope {
            ProcessEnvelope::Hello(value) => &value.header,
            ProcessEnvelope::Request(value) => &value.header,
            ProcessEnvelope::Event(value) => &value.header,
            ProcessEnvelope::Error(value) => &value.header,
            ProcessEnvelope::Cancel(value) => &value.header,
            ProcessEnvelope::CancelAcknowledged(value) => &value.header,
            ProcessEnvelope::Goodbye(value) => &value.header,
        };
        self.compatibility
            .check(header.version)
            .map_err(|_| FrameError::IncompatibleVersion {
                received_major: header.version.major,
                supported_major: self.compatibility.major,
            })?;
        let ProcessEnvelope::Request(request_envelope) = envelope else {
            return Ok(());
        };
        let request = request_envelope.header.request_id.to_string();
        if self.seen_requests.contains(&request) {
            return Err(FrameError::DuplicateRequestId {
                request_id: request,
            });
        }
        if self.seen_requests.len() >= self.maximum_seen_requests {
            return Err(FrameError::Oversized {
                declared: self.seen_requests.len() + 1,
                maximum: self.maximum_seen_requests,
            });
        }
        self.seen_requests.insert(request);
        Ok(())
    }
}

fn invalid_json(error: &serde_json::Error) -> FrameError {
    FrameError::InvalidJson {
        message: error.to_string(),
    }
}

fn invalid_protocol(error: &crate::ProtocolError) -> FrameError {
    FrameError::InvalidJson {
        message: format!("protocol validation failed: {error:?}"),
    }
}

fn reject_unknown_nested_kind(value: &serde_json::Value, known: &[&str]) -> Result<(), FrameError> {
    if let Some(kind) = value
        .get("message")
        .and_then(|message| message.get("kind"))
        .and_then(serde_json::Value::as_str)
        && !known.contains(&kind)
    {
        return Err(FrameError::UnknownMandatoryKind {
            kind: kind.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_oversize_before_allocating_payload() {
        let mut decoder = FrameDecoder::new(FrameLimits {
            maximum_frame_bytes: 8,
        });
        let result = decoder.push(&16u32.to_be_bytes());
        assert_eq!(
            result,
            Err(FrameError::Oversized {
                declared: 16,
                maximum: 8
            })
        );
    }

    #[test]
    fn reports_partial_header_and_payload() {
        let mut decoder = FrameDecoder::new(FrameLimits::default());
        assert_eq!(decoder.push(&[0, 0]), Ok((2, None)));
        assert_eq!(
            decoder.finish(),
            Err(FrameError::PartialHeader { received: 2 })
        );
        assert_eq!(decoder.push(&[0, 0, 0, 4, b'{']), Ok((5, None)));
        assert_eq!(
            decoder.finish(),
            Err(FrameError::PartialPayload {
                declared: 4,
                received: 1
            })
        );
    }

    #[test]
    fn parses_two_chunks_without_consuming_next_frame() {
        let mut decoder = FrameDecoder::new(FrameLimits::default());
        assert_eq!(decoder.push(&[0, 0, 0, 2, b'{']), Ok((5, None)));
        assert_eq!(decoder.push(&[b'}', 9, 9]), Ok((1, Some(vec![b'{', b'}']))));
    }

    #[test]
    fn returns_typed_unknown_kind() {
        let payload = br#"{"kind":"future_required","body":{}}"#;
        assert_eq!(
            FrameDecoder::decode_process(payload, FrameLimits::default()),
            Err(FrameError::UnknownMandatoryKind {
                kind: "future_required".into()
            })
        );
    }

    fn request_json(extra: &str, schema_id: &str, operation: &str) -> Vec<u8> {
        format!(
            r#"{{"kind":"request","body":{{"header":{{"schema_id":"{schema_id}","version":{{"major":1,"minor":0}},"request_id":"request_1","producer_id":"producer_1","job_id":null,"sequence":1}},"operation":"{operation}","payload":{{}}}}{extra}}}"#
        )
        .into_bytes()
    }

    #[test]
    fn process_decode_enforces_field_limits_and_unknown_top_level_fields() {
        let over_schema = "s".repeat(129);
        assert!(matches!(
            FrameDecoder::decode_process(
                &request_json("", &over_schema, "op"),
                FrameLimits::default()
            ),
            Err(FrameError::InvalidJson { .. })
        ));
        assert!(matches!(
            FrameDecoder::decode_process(
                &request_json(",\"smuggled\":\"secret\"", "ff.process@1", "op"),
                FrameLimits::default()
            ),
            Err(FrameError::InvalidJson { .. })
        ));
        assert!(matches!(
            FrameDecoder::decode_process(
                &request_json("", "ff.event@1", "op"),
                FrameLimits::default()
            ),
            Err(FrameError::InvalidJson { .. })
        ));
    }

    #[test]
    fn conformance_allows_correlated_event_but_rejects_duplicate_request() {
        let request = FrameDecoder::decode_process(
            &request_json("", "ff.process@1", "op"),
            FrameLimits::default(),
        )
        .expect("request must decode");
        let event: ProcessEnvelope = serde_json::from_str(
            r#"{"kind":"event","body":{"header":{"schema_id":"ff.event@1","version":{"major":1,"minor":0},"request_id":"request_1","producer_id":"producer_1","job_id":null,"sequence":2},"kind":"progress","criticality":"operational","sensitivity":"operational","payload":{}}}"#,
        )
        .expect("event must decode");
        let mut conformance = ProcessConformance::new(
            CompatibilityRange {
                major: 1,
                minimum_minor: 0,
                maximum_minor: 1,
            },
            8,
        );
        assert!(conformance.validate(&request).is_ok());
        assert!(conformance.validate(&event).is_ok());
        assert!(matches!(
            conformance.validate(&request),
            Err(FrameError::DuplicateRequestId { .. })
        ));
    }

    #[test]
    fn framed_plugin_and_javascript_paths_validate_recursively() {
        let hash = "a".repeat(64);
        let plugin = format!(
            r#"{{"header":{{"schema_id":"ff.plugin@1","version":{{"major":1,"minor":0}},"request_id":"request_plugin","producer_id":"producer_plugin","job_id":"job_plugin","sequence":1}},"compatibility":{{"major":1,"minimum_minor":0,"maximum_minor":1}},"provenance":{{"artifact_id":"artifact-1","schema_hash":"{hash}","capability_grant_id":"grant-1"}},"message":{{"kind":"describe"}}}}"#
        );
        assert!(FrameDecoder::decode_plugin(plugin.as_bytes(), FrameLimits::default()).is_ok());
        let unknown = plugin.replace("\"describe\"", "\"future_required\"");
        assert!(matches!(
            FrameDecoder::decode_plugin(unknown.as_bytes(), FrameLimits::default()),
            Err(FrameError::UnknownMandatoryKind { .. })
        ));
        let javascript = format!(
            r#"{{"header":{{"schema_id":"ff.javascript-worker@1","version":{{"major":1,"minor":0}},"request_id":"request_js","producer_id":"producer_js","job_id":"job_js","sequence":1}},"compatibility":{{"major":1,"minimum_minor":0,"maximum_minor":1}},"provenance":{{"artifact_id":"artifact-1","schema_hash":"{hash}","capability_grant_id":"grant-1"}},"message":{{"kind":"evaluate","body":{{"program_reference":"program-1","input":{{}},"deadline_millis":18446744073709551615,"memory_limit_bytes":18446744073709551615}}}}}}"#
        );
        assert!(matches!(
            FrameDecoder::decode_javascript_worker(javascript.as_bytes(), FrameLimits::default()),
            Err(FrameError::InvalidJson { .. })
        ));
    }

    #[test]
    fn conformance_recursively_rejects_invalid_envelopes() {
        let invalid = ProcessEnvelope::Goodbye(crate::Goodbye {
            header: crate::EnvelopeHeader {
                schema_id: "ff.process@1".into(),
                version: crate::SchemaVersion { major: 1, minor: 0 },
                request_id: crate::RequestId::new("request_invalid").expect("valid fixture ID"),
                producer_id: crate::ProducerId::new("producer_invalid").expect("valid fixture ID"),
                job_id: None,
                sequence: 0,
            },
            completed: false,
        });
        let mut conformance = ProcessConformance::new(
            CompatibilityRange {
                major: 1,
                minimum_minor: 0,
                maximum_minor: 1,
            },
            1,
        );
        assert!(matches!(
            conformance.validate(&invalid),
            Err(FrameError::InvalidJson { .. })
        ));
    }
}
