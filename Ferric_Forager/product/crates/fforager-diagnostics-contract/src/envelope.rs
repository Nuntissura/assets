use crate::{
    ArtifactId, BoundedText, CapabilityId, ContractError, EventDescriptor, LimitKind, MAX_FIELDS,
    MAX_FRAME_BYTES, MAX_OPAQUE_VALUE_BYTES, ProducerInstanceId, ProtocolVersion, RequestId,
    SchemaIdentity, Sensitivity, SequenceFault, SequenceIdentity,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Representation of a diagnostic value after producer-side privacy handling.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "representation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ValueRepresentation {
    Plain {
        value: BoundedText,
    },
    Redacted {
        marker: BoundedText,
    },
    /// Metadata for an unknown optional value held in watcher-local opaque storage.
    /// The plaintext is intentionally absent from exportable diagnostic contracts.
    OpaqueUnknown {
        byte_len: u32,
        exportable: bool,
    },
}

/// Named bounded diagnostic field with explicit sensitivity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "DiagnosticFieldWire")]
pub struct DiagnosticField {
    pub name: BoundedText,
    pub sensitivity: Sensitivity,
    pub value: ValueRepresentation,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiagnosticFieldWire {
    name: BoundedText,
    sensitivity: Sensitivity,
    value: ValueRepresentation,
}

impl DiagnosticField {
    /// Creates a field and applies its privacy and opaque-value rules.
    ///
    /// # Errors
    /// Returns [`ContractError`] for plaintext secrets or unsafe unknown values.
    pub fn new(
        name: BoundedText,
        sensitivity: Sensitivity,
        value: ValueRepresentation,
    ) -> Result<Self, ContractError> {
        let field = Self {
            name,
            sensitivity,
            value,
        };
        field.validate()?;
        Ok(field)
    }

    /// Creates a public plaintext field.
    ///
    /// # Errors
    /// Returns [`ContractError`] if field validation fails.
    pub fn public(name: BoundedText, value: BoundedText) -> Result<Self, ContractError> {
        Self::new(
            name,
            Sensitivity::Public,
            ValueRepresentation::Plain { value },
        )
    }

    /// Creates a producer-redacted field.
    ///
    /// # Errors
    /// Returns [`ContractError`] if field validation fails.
    pub fn redacted(
        name: BoundedText,
        sensitivity: Sensitivity,
        marker: BoundedText,
    ) -> Result<Self, ContractError> {
        Self::new(name, sensitivity, ValueRepresentation::Redacted { marker })
    }

    /// Records bounded metadata for a non-exportable unknown optional value.
    ///
    /// # Errors
    /// Returns [`ContractError`] for secret, empty, oversized, or exportable unknown values.
    pub fn unknown_optional(
        name: BoundedText,
        sensitivity: Sensitivity,
        byte_len: u32,
    ) -> Result<Self, ContractError> {
        Self::new(
            name,
            sensitivity,
            ValueRepresentation::OpaqueUnknown {
                byte_len,
                exportable: false,
            },
        )
    }

    /// Revalidates privacy and opaque-value bounds.
    ///
    /// # Errors
    /// Returns [`ContractError`] for a privacy or size violation.
    pub fn validate(&self) -> Result<(), ContractError> {
        match &self.value {
            ValueRepresentation::Plain { .. } if self.sensitivity == Sensitivity::Secret => {
                Err(ContractError::PrivacyViolation)
            }
            ValueRepresentation::OpaqueUnknown {
                byte_len,
                exportable,
            } => {
                let length =
                    usize::try_from(*byte_len).map_err(|_| ContractError::LimitExceeded {
                        kind: LimitKind::OpaqueValue,
                        limit: MAX_OPAQUE_VALUE_BYTES,
                        actual: MAX_OPAQUE_VALUE_BYTES.saturating_add(1),
                    })?;
                if length > MAX_OPAQUE_VALUE_BYTES {
                    Err(ContractError::LimitExceeded {
                        kind: LimitKind::OpaqueValue,
                        limit: MAX_OPAQUE_VALUE_BYTES,
                        actual: length,
                    })
                } else if length == 0 || *exportable || self.sensitivity == Sensitivity::Secret {
                    Err(ContractError::PrivacyViolation)
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        }
    }
}

impl TryFrom<DiagnosticFieldWire> for DiagnosticField {
    type Error = ContractError;

    fn try_from(value: DiagnosticFieldWire) -> Result<Self, Self::Error> {
        Self::new(value.name, value.sensitivity, value.value)
    }
}

/// One validated diagnostic event frame.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "DiagnosticEnvelopeWire")]
pub struct DiagnosticEnvelope {
    pub protocol: ProtocolVersion,
    pub schema: SchemaIdentity,
    pub producer_instance: ProducerInstanceId,
    pub capability_id: CapabilityId,
    pub sequence: SequenceIdentity,
    pub observed_monotonic_ns: u64,
    pub descriptor: EventDescriptor,
    pub fields: Vec<DiagnosticField>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiagnosticEnvelopeWire {
    protocol: ProtocolVersion,
    schema: SchemaIdentity,
    producer_instance: ProducerInstanceId,
    capability_id: CapabilityId,
    sequence: SequenceIdentity,
    observed_monotonic_ns: u64,
    descriptor: EventDescriptor,
    fields: Vec<DiagnosticField>,
}

impl DiagnosticEnvelope {
    /// Revalidates protocol, stream identity, metadata, field bounds, and privacy.
    ///
    /// # Errors
    /// Returns [`ContractError`] when any envelope invariant fails.
    pub fn validate(&self) -> Result<(), ContractError> {
        self.protocol.validate()?;
        self.schema.validate()?;
        self.sequence.validate()?;
        if self.sequence.key.producer_instance != self.producer_instance {
            return Err(ContractError::Sequence {
                fault: SequenceFault::IdentityChanged,
            });
        }
        self.descriptor.validate()?;
        if self.fields.len() > MAX_FIELDS {
            return Err(ContractError::LimitExceeded {
                kind: LimitKind::Fields,
                limit: MAX_FIELDS,
                actual: self.fields.len(),
            });
        }
        let mut names = HashSet::with_capacity(self.fields.len());
        let mut highest = Sensitivity::Public;
        for field in &self.fields {
            field.validate()?;
            if !names.insert(field.name.as_str()) {
                return Err(ContractError::DuplicateField);
            }
            highest = highest.max(field.sensitivity);
        }
        if highest > self.descriptor.sensitivity {
            return Err(ContractError::PrivacyViolation);
        }
        Ok(())
    }
}

impl TryFrom<DiagnosticEnvelopeWire> for DiagnosticEnvelope {
    type Error = ContractError;

    fn try_from(value: DiagnosticEnvelopeWire) -> Result<Self, Self::Error> {
        let envelope = Self {
            protocol: value.protocol,
            schema: value.schema,
            producer_instance: value.producer_instance,
            capability_id: value.capability_id,
            sequence: value.sequence,
            observed_monotonic_ns: value.observed_monotonic_ns,
            descriptor: value.descriptor,
            fields: value.fields,
        };
        envelope.validate()?;
        Ok(envelope)
    }
}

/// Whether a transport has supplied a complete frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameCompleteness {
    Complete,
    Partial,
}

/// Decode one bounded complete JSON frame and re-run every constructor invariant.
///
/// # Errors
/// Returns [`ContractError`] for partial, malformed, oversized, or invalid frames.
pub fn decode_json_frame(
    bytes: &[u8],
    completeness: FrameCompleteness,
) -> Result<DiagnosticEnvelope, ContractError> {
    if completeness == FrameCompleteness::Partial {
        return Err(ContractError::PartialFrame);
    }
    if bytes.is_empty() {
        return Err(ContractError::MalformedFrame);
    }
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(ContractError::LimitExceeded {
            kind: LimitKind::Frame,
            limit: MAX_FRAME_BYTES,
            actual: bytes.len(),
        });
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| ContractError::MalformedFrame)?;
    let is_unknown = value
        .pointer("/descriptor/kind/kind")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|kind| kind == "unknown");
    let is_mandatory = value
        .pointer("/descriptor/kind/mandatory")
        .and_then(serde_json::Value::as_bool)
        .is_some_and(|mandatory| mandatory);
    if is_unknown && is_mandatory {
        return Err(ContractError::UnknownMandatoryKind);
    }
    serde_json::from_value(value).map_err(|_| ContractError::MalformedFrame)
}

/// Encode one validated envelope, refusing any unexpectedly oversized output.
///
/// # Errors
/// Returns [`ContractError`] if validation, serialization, or the frame bound fails.
pub fn encode_json_frame(envelope: &DiagnosticEnvelope) -> Result<Vec<u8>, ContractError> {
    envelope.validate()?;
    let bytes = serde_json::to_vec(envelope).map_err(|_| ContractError::MalformedFrame)?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(ContractError::LimitExceeded {
            kind: LimitKind::Frame,
            limit: MAX_FRAME_BYTES,
            actual: bytes.len(),
        });
    }
    Ok(bytes)
}

/// Durability reached by a watcher acknowledgement.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Durability {
    Admitted,
    Written,
    Synced,
}

/// Acknowledgement of a diagnostic event; never a product-control acknowledgement.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticAck {
    pub sequence: SequenceIdentity,
    pub durability: Durability,
    pub acknowledged_monotonic_ns: u64,
}

/// Request to cancel only a diagnostic operation such as flush or collection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancellationRequest {
    pub cancellation_id: RequestId,
    pub target_request_id: RequestId,
    pub requested_monotonic_ns: u64,
}

/// Outcome of a diagnostic cancellation request.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationDisposition {
    Accepted,
    AlreadyTerminal,
    UnknownRequest,
}

/// Bounded cancellation acknowledgement.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancellationAck {
    pub cancellation_id: RequestId,
    pub target_request_id: RequestId,
    pub disposition: CancellationDisposition,
    pub acknowledged_monotonic_ns: u64,
}

/// Governed diagnostic startup result, separate from Ferric's product result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum DiagnosticStartupStatus {
    Ready,
    Degraded { reason: crate::ReasonCode },
    Incompatible { reason: crate::ReasonCode },
}

/// Result envelope for a bounded diagnostic request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticResult {
    pub request_id: RequestId,
    pub status: DiagnosticStartupStatus,
    pub completed_monotonic_ns: u64,
}

/// Watcher-authored observation when no producer terminal event can exist.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrashObservation {
    ProcessExitObserved,
}

/// Process termination classification observed independently by the watcher.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExitKind {
    ExitCode { code: i32 },
    Signal { signal: u32 },
    Unknown,
}

/// Explicit acknowledgement authorizing deletion of retained crash evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionAck {
    pub request_id: RequestId,
    pub acknowledged_monotonic_ns: u64,
}

/// Retention state; retained evidence can terminate only by acknowledgement or expiry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum EvidenceRetention {
    Retained,
    Collected { acknowledgement: CollectionAck },
    Expired { expired_monotonic_ns: u64 },
}

impl EvidenceRetention {
    /// Marks retained evidence collected through an explicit acknowledgement.
    ///
    /// # Errors
    /// Returns [`ContractError`] if retention is already terminal.
    pub fn acknowledge(&self, acknowledgement: CollectionAck) -> Result<Self, ContractError> {
        if !matches!(self, Self::Retained) {
            return Err(ContractError::RetentionTerminal);
        }
        Ok(Self::Collected { acknowledgement })
    }

    /// Marks retained evidence expired through governed retention.
    ///
    /// # Errors
    /// Returns [`ContractError`] if retention is already terminal.
    pub fn expire(&self, expired_monotonic_ns: u64) -> Result<Self, ContractError> {
        if !matches!(self, Self::Retained) {
            return Err(ContractError::RetentionTerminal);
        }
        Ok(Self::Expired {
            expired_monotonic_ns,
        })
    }
}

/// Crash evidence linking watcher provenance to the last durable producer sequence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "CrashEnvelopeWire")]
pub struct CrashEnvelope {
    pub observation: CrashObservation,
    pub producer_instance: ProducerInstanceId,
    pub watcher_artifact_id: ArtifactId,
    pub exit: ExitKind,
    pub observed_monotonic_ns: u64,
    pub last_durable_producer_sequence: Option<SequenceIdentity>,
    pub retention: EvidenceRetention,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CrashEnvelopeWire {
    observation: CrashObservation,
    producer_instance: ProducerInstanceId,
    watcher_artifact_id: ArtifactId,
    exit: ExitKind,
    observed_monotonic_ns: u64,
    last_durable_producer_sequence: Option<SequenceIdentity>,
    retention: EvidenceRetention,
}

impl CrashEnvelope {
    /// Creates watcher-authored retained crash evidence.
    ///
    /// # Errors
    /// Returns [`ContractError`] when the last durable identity belongs to another producer.
    pub fn new(
        producer_instance: ProducerInstanceId,
        watcher_artifact_id: ArtifactId,
        exit: ExitKind,
        observed_monotonic_ns: u64,
        last_durable_producer_sequence: Option<SequenceIdentity>,
    ) -> Result<Self, ContractError> {
        let envelope = Self {
            observation: CrashObservation::ProcessExitObserved,
            producer_instance,
            watcher_artifact_id,
            exit,
            observed_monotonic_ns,
            last_durable_producer_sequence,
            retention: EvidenceRetention::Retained,
        };
        envelope.validate()?;
        Ok(envelope)
    }

    /// Revalidates watcher crash-evidence provenance.
    ///
    /// # Errors
    /// Returns [`ContractError`] on producer identity mismatch.
    pub fn validate(&self) -> Result<(), ContractError> {
        if let Some(sequence) = &self.last_durable_producer_sequence
            && sequence.key.producer_instance != self.producer_instance
        {
            return Err(ContractError::Sequence {
                fault: SequenceFault::IdentityChanged,
            });
        }
        Ok(())
    }
}

impl TryFrom<CrashEnvelopeWire> for CrashEnvelope {
    type Error = ContractError;

    fn try_from(value: CrashEnvelopeWire) -> Result<Self, Self::Error> {
        let envelope = Self {
            observation: value.observation,
            producer_instance: value.producer_instance,
            watcher_artifact_id: value.watcher_artifact_id,
            exit: value.exit,
            observed_monotonic_ns: value.observed_monotonic_ns,
            last_durable_producer_sequence: value.last_durable_producer_sequence,
            retention: value.retention,
        };
        envelope.validate()?;
        Ok(envelope)
    }
}
