//! Versioned, bounded, data-only diagnostics shared by Ferric and its watcher.
//!
//! The crate deliberately exposes validation and pure transition functions,
//! never runtime handles or control-plane operations. See the package README
//! for the compatibility, privacy, sequence, and retention rules.

#![forbid(unsafe_code)]

mod envelope;
mod error;
mod event;
mod ids;
mod lifecycle;
mod protocol;

pub use envelope::{
    CancellationAck, CancellationDisposition, CancellationRequest, CollectionAck, CrashEnvelope,
    CrashObservation, DiagnosticAck, DiagnosticEnvelope, DiagnosticField, DiagnosticResult,
    DiagnosticStartupStatus, Durability, EvidenceRetention, ExitKind, FrameCompleteness,
    ValueRepresentation, decode_json_frame, encode_json_frame,
};
pub use error::{ContractError, LimitKind, SequenceFault};
pub use event::{Criticality, EventDescriptor, EventKind, Sensitivity, WatcherPolicy};
pub use ids::{
    ArtifactId, BootSessionId, BoundedText, BuildId, CapabilityId, ChannelId, EventId,
    ProcessStartId, ProducerInstanceId, ReasonCode, RequestId, WorkId,
};
pub use lifecycle::{
    CheckStatus, HealthCounters, HealthSnapshot, LifecycleInput, LifecycleSnapshot, LoopHealth,
    LoopState, ReadyEvidence, WatcherState,
};
pub use protocol::{
    CompatibilityRange, NegotiatedProtocol, ProtocolOffer, ProtocolOfferV1, ProtocolVersion,
    ReviewedSchemaTransition, SchemaCompatibilityAuthority, SchemaDisposition, SchemaHash,
    SchemaHashAlgorithm, SchemaIdentity, SequenceDisposition, SequenceIdentity, SequenceKey,
    SequenceTracker,
};

/// Maximum UTF-8 bytes accepted for a bounded text value.
pub const MAX_TEXT_BYTES: usize = 4_096;
/// Maximum identifier bytes accepted on the wire.
pub const MAX_ID_BYTES: usize = 128;
/// Maximum fields accepted in one diagnostic envelope.
pub const MAX_FIELDS: usize = 64;
/// Maximum schema identities accepted in one protocol offer.
pub const MAX_SCHEMA_IDENTITIES: usize = 16;
/// Maximum independently tracked loops accepted in one health snapshot.
pub const MAX_HEALTH_LOOPS: usize = 32;
/// Maximum encoded JSON frame size.
pub const MAX_FRAME_BYTES: usize = 256 * 1_024;
/// Maximum bytes retained for an unknown optional value.
pub const MAX_OPAQUE_VALUE_BYTES: usize = 8_192;
/// First legal sequence number.
pub const FIRST_SEQUENCE: u64 = 1;
/// Last legal sequence number; incrementing it is forbidden rather than wrapped.
pub const LAST_SEQUENCE: u64 = u64::MAX;
