//! Studio business-event envelope (STU-CON-003, STU-CON-013, STU-CON-014; MT-36230 asset side,
//! lane SMB-BUSINESS-EVENTS-1). The vocabulary lives in `hsk-studio-accord` (lead decision
//! 2026-10-31, option C: the closed catalog `StudioBusinessEventId`, `BusinessEventRecord` and the
//! `BusinessEventSink` with its closed delivery error), so a producer such as harness-core's Run
//! emits records without a module edge to this crate. This binding owns the canonical
//! `hsk.studio.business_event@1` envelope: it is built from a record, encoded and decoded here.
//!
//! The envelope carries ids and digests only (document, actor, client session, event id, revision,
//! sequence, correlation, causation, command id, optional input/output digests and typed artifact
//! refs). It has no field that can hold document text, a label, a path or a font name (test
//! `envelopes_carry_ids_and_digests_only`). Its wire is compact JSON in canonical field order;
//! decoding refuses everything the encoder cannot produce (test
//! `envelopes_are_strict_and_refuse_typed`).
//!
//! Registration in the host's closed Flight Recorder / kernel event enums, durable delivery and
//! reconciliation are the host's (MT-36230): an asset-local sink result is evidence, not host
//! acceptance.
#![deny(clippy::arithmetic_side_effects)]
use crate::wire::ActorWire;
use hsk_studio_accord::{
    ActorContext, BusinessEventRecord, CausationId, ClientSessionId, CommandId, CorrelationId,
    DomainId, MAX_BUSINESS_EVENT_ARTIFACTS, SCHEMA_STUDIO_BUSINESS_EVENT, SCHEMA_STUDIO_DOCUMENT,
    SchemaId, StudioBusinessEventId, business_event_id,
};
use serde::{Deserialize, Serialize};

/// Largest encoded event; checked on the input before it is parsed and on the output after it is
/// encoded. The largest valid event (every bound at its maximum) fits
/// (`envelopes_are_strict_and_refuse_typed`).
pub const MAX_EVENT_BYTES: usize = 8 * 1024;
/// Longest command id (`[a-z0-9._-]`, the RUN_COMMANDS grammar).
pub const MAX_COMMAND_ID_BYTES: usize = 64;

/// Why a variant has no asset-local producer. Each is typed in the catalog and is NOT_PROVEN as an
/// emission until a producer exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotProducedReason {
    /// No promotion or proposal flow exists in the asset-local Run.
    NoPromotionOrProposalFlow,
    /// Selection is view state, not a Run commit.
    ViewStateNotACommit,
    /// No Run command or document entry token maps to the variant unambiguously.
    NoMappedCommand,
    /// Exports run in harness-view job queues, not in Run; wired at job settlement later.
    ExportOutsideRun,
}

impl NotProducedReason {
    pub const fn code(self) -> &'static str {
        match self {
            Self::NoPromotionOrProposalFlow => "no_promotion_or_proposal_flow",
            Self::ViewStateNotACommit => "view_state_not_a_commit",
            Self::NoMappedCommand => "no_mapped_command",
            Self::ExportOutsideRun => "export_outside_run",
        }
    }
}

/// The variants nothing asset-local produces, with the reason. Never a fake producer
/// (`variants_without_a_producer_are_listed_not_faked`). harness-core's Run produces the other
/// nine: DOCUMENT_CREATED, LAYER_CREATED, LAYER_MUTATED, VECTOR_PATH_EDITED, TEXT_EDITED,
/// RAW_DEVELOPED, IMPORT_COMPLETED, HISTORY_UNDONE and HISTORY_REDONE.
pub const NOT_PRODUCED_ASSET_LOCALLY: [(StudioBusinessEventId, NotProducedReason); 13] = [
    (StudioBusinessEventId::DocumentPromoted, NotProducedReason::NoPromotionOrProposalFlow),
    (StudioBusinessEventId::EditProposalRecorded, NotProducedReason::NoPromotionOrProposalFlow),
    (StudioBusinessEventId::EditPromoted, NotProducedReason::NoPromotionOrProposalFlow),
    (StudioBusinessEventId::EditRejected, NotProducedReason::NoPromotionOrProposalFlow),
    (StudioBusinessEventId::SelectionChanged, NotProducedReason::ViewStateNotACommit),
    (StudioBusinessEventId::MaskApplied, NotProducedReason::NoMappedCommand),
    (StudioBusinessEventId::AdjustmentApplied, NotProducedReason::NoMappedCommand),
    (StudioBusinessEventId::EffectApplied, NotProducedReason::NoMappedCommand),
    (StudioBusinessEventId::StyleApplied, NotProducedReason::NoMappedCommand),
    (StudioBusinessEventId::ComponentPublished, NotProducedReason::NoMappedCommand),
    (StudioBusinessEventId::VariableSet, NotProducedReason::NoMappedCommand),
    (StudioBusinessEventId::PrototypeEdited, NotProducedReason::NoMappedCommand),
    (StudioBusinessEventId::ExportRendered, NotProducedReason::ExportOutsideRun),
];

/// Why an event was not built, encoded or decoded. Codes are stable snake_case, one per cause.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusinessEventError {
    Empty,
    Oversize,
    Syntax,
    TrailingBytes,
    UnknownField,
    DuplicateField,
    MissingField,
    InvalidType,
    WrongSchema,
    UnknownVariant,
    FamilyMismatch,
    BusinessIdMismatch,
    /// Not an `SDOC` id valid for `hsk.studio.document@1`.
    InvalidDocument,
    InvalidActor,
    InvalidClientSession,
    /// A committed command without a command id (lead decision 2026-10-31: every mutating `Run`
    /// method has a RUN_COMMANDS row, so a missing id is refused, never emitted null or invented).
    MissingCommandId,
    InvalidCommandId,
    /// Not 64 lowercase hex digits (a SHA-256).
    InvalidDigest,
    /// Not a valid accord domain id, or repeated.
    InvalidArtifact,
    TooManyArtifacts,
    /// The event id is not the one derived from document, revision and sequence.
    EventIdMismatch,
    NonCanonical,
    EncodeFailed,
}

impl BusinessEventError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Oversize => "oversize",
            Self::Syntax => "syntax",
            Self::TrailingBytes => "trailing_bytes",
            Self::UnknownField => "unknown_field",
            Self::DuplicateField => "duplicate_field",
            Self::MissingField => "missing_field",
            Self::InvalidType => "invalid_type",
            Self::WrongSchema => "wrong_schema",
            Self::UnknownVariant => "unknown_variant",
            Self::FamilyMismatch => "family_mismatch",
            Self::BusinessIdMismatch => "business_id_mismatch",
            Self::InvalidDocument => "invalid_document",
            Self::InvalidActor => "invalid_actor",
            Self::InvalidClientSession => "invalid_client_session",
            Self::MissingCommandId => "missing_command_id",
            Self::InvalidCommandId => "invalid_command_id",
            Self::InvalidDigest => "invalid_digest",
            Self::InvalidArtifact => "invalid_artifact",
            Self::TooManyArtifacts => "too_many_artifacts",
            Self::EventIdMismatch => "event_id_mismatch",
            Self::NonCanonical => "non_canonical",
            Self::EncodeFailed => "encode_failed",
        }
    }
}

impl std::fmt::Display for BusinessEventError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for BusinessEventError {}

/// One typed `hsk.studio.business_event@1` envelope. Built only through
/// [`StudioBusinessEvent::from_record`] or [`decode_business_event`], so every value satisfies the
/// construction rules: an `SDOC` document, the event id derived from document, revision and
/// sequence, a RUN_COMMANDS command id, bounded ids and digests only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StudioBusinessEvent {
    event: StudioBusinessEventId,
    document: DomainId,
    actor: ActorContext,
    client_session: ClientSessionId,
    event_id: String,
    revision: u64,
    sequence: u64,
    correlation: CorrelationId,
    causation: Option<CausationId>,
    command_id: CommandId,
    input_digest: Option<String>,
    output_digest: Option<String>,
    artifacts: Vec<DomainId>,
}

fn valid_command_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_COMMAND_ID_BYTES
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn document_id(value: &str) -> Result<DomainId, BusinessEventError> {
    let schema = SchemaId::parse(SCHEMA_STUDIO_DOCUMENT).map_err(|_| BusinessEventError::InvalidDocument)?;
    DomainId::for_schema(value, &schema).map_err(|_| BusinessEventError::InvalidDocument)
}

fn digest(value: Option<&str>) -> Result<Option<String>, BusinessEventError> {
    match value {
        None => Ok(None),
        Some(d) if valid_digest(d) => Ok(Some(d.to_owned())),
        Some(_) => Err(BusinessEventError::InvalidDigest),
    }
}

/// Bounded first, then validated one by one; a repeated ref is refused.
fn artifact_refs<'s>(refs: impl ExactSizeIterator<Item = &'s str>) -> Result<Vec<DomainId>, BusinessEventError> {
    if refs.len() > MAX_BUSINESS_EVENT_ARTIFACTS {
        return Err(BusinessEventError::TooManyArtifacts);
    }
    let mut out: Vec<DomainId> = Vec::with_capacity(refs.len());
    for value in refs {
        let id = DomainId::parse(value).map_err(|_| BusinessEventError::InvalidArtifact)?;
        if out.contains(&id) {
            return Err(BusinessEventError::InvalidArtifact);
        }
        out.push(id);
    }
    Ok(out)
}

impl StudioBusinessEvent {
    /// The envelope of an accord record: event, document, revision, sequence, artifacts and the
    /// committing command's context (actor, client session, command id, correlation, causation).
    /// A context without a command id is `missing_command_id` (lead decision 2026-10-31); a command
    /// id outside the RUN_COMMANDS grammar `[a-z0-9._-]{1,64}` is `invalid_command_id`.
    pub fn from_record(record: &BusinessEventRecord) -> Result<Self, BusinessEventError> {
        let context = record.context();
        let command = context.command().ok_or(BusinessEventError::MissingCommandId)?;
        if !valid_command_id(command.as_str()) {
            return Err(BusinessEventError::InvalidCommandId);
        }
        Ok(Self {
            event: record.event(),
            document: record.document().clone(),
            actor: context.actor().clone(),
            client_session: context.client_session().clone(),
            event_id: record.event_id(),
            revision: record.revision(),
            sequence: record.sequence(),
            correlation: context.correlation(),
            causation: context.causation(),
            command_id: command.clone(),
            input_digest: None,
            output_digest: None,
            artifacts: record.artifacts().to_vec(),
        })
    }

    /// Adds the command's input and output digests (SHA-256, lowercase hex).
    pub fn with_digests(mut self, input: Option<&str>, output: Option<&str>) -> Result<Self, BusinessEventError> {
        self.input_digest = digest(input)?;
        self.output_digest = digest(output)?;
        Ok(self)
    }

    pub fn event(&self) -> StudioBusinessEventId {
        self.event
    }
    pub fn document(&self) -> &DomainId {
        &self.document
    }
    pub fn actor(&self) -> &ActorContext {
        &self.actor
    }
    pub fn client_session(&self) -> &ClientSessionId {
        &self.client_session
    }
    pub fn event_id(&self) -> &str {
        &self.event_id
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn correlation(&self) -> CorrelationId {
        self.correlation
    }
    pub fn causation(&self) -> Option<CausationId> {
        self.causation
    }
    pub fn command_id(&self) -> &CommandId {
        &self.command_id
    }
    pub fn input_digest(&self) -> Option<&str> {
        self.input_digest.as_deref()
    }
    pub fn output_digest(&self) -> Option<&str> {
        self.output_digest.as_deref()
    }
    pub fn artifacts(&self) -> &[DomainId] {
        &self.artifacts
    }

    fn wire(&self) -> EventWire {
        EventWire {
            schema_id: SCHEMA_STUDIO_BUSINESS_EVENT.to_owned(),
            variant: self.event.as_str().to_owned(),
            event_family: self.event.family().to_owned(),
            business_id: self.event.business_id().to_owned(),
            document: self.document.as_str().to_owned(),
            actor: ActorWire::from_context(&self.actor),
            client_session: self.client_session.as_str().to_owned(),
            event_id: self.event_id.clone(),
            revision: self.revision,
            sequence: self.sequence,
            correlation: self.correlation.value(),
            causation: self.causation.map(CausationId::value),
            command_id: self.command_id.as_str().to_owned(),
            input_digest: self.input_digest.clone(),
            output_digest: self.output_digest.clone(),
            artifacts: self.artifacts.iter().map(|a| a.as_str().to_owned()).collect(),
        }
    }
}

/// The wire form, in canonical field order. Every field is always present (`null` when absent).
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EventWire {
    schema_id: String,
    variant: String,
    event_family: String,
    business_id: String,
    document: String,
    actor: ActorWire,
    client_session: String,
    event_id: String,
    revision: u64,
    sequence: u64,
    correlation: u64,
    causation: Option<u64>,
    command_id: String,
    input_digest: Option<String>,
    output_digest: Option<String>,
    artifacts: Vec<String>,
}

impl EventWire {
    fn into_event(self) -> Result<StudioBusinessEvent, BusinessEventError> {
        if self.schema_id != SCHEMA_STUDIO_BUSINESS_EVENT {
            return Err(BusinessEventError::WrongSchema);
        }
        let event = StudioBusinessEventId::parse(&self.variant).ok_or(BusinessEventError::UnknownVariant)?;
        if self.event_family != event.family() {
            return Err(BusinessEventError::FamilyMismatch);
        }
        if self.business_id != event.business_id() {
            return Err(BusinessEventError::BusinessIdMismatch);
        }
        let document = document_id(&self.document)?;
        let actor = self.actor.to_context().map_err(|_| BusinessEventError::InvalidActor)?;
        let client_session = ClientSessionId::parse(&self.client_session)
            .map_err(|_| BusinessEventError::InvalidClientSession)?;
        let command_id = CommandId::parse(&self.command_id).map_err(|_| BusinessEventError::InvalidCommandId)?;
        if !valid_command_id(command_id.as_str()) {
            return Err(BusinessEventError::InvalidCommandId);
        }
        if self.event_id != business_event_id(&document, self.revision, self.sequence) {
            return Err(BusinessEventError::EventIdMismatch);
        }
        Ok(StudioBusinessEvent {
            event,
            document,
            actor,
            client_session,
            event_id: self.event_id,
            revision: self.revision,
            sequence: self.sequence,
            correlation: CorrelationId::new(self.correlation),
            causation: self.causation.map(CausationId::new),
            command_id,
            input_digest: digest(self.input_digest.as_deref())?,
            output_digest: digest(self.output_digest.as_deref())?,
            artifacts: artifact_refs(self.artifacts.iter().map(String::as_str))?,
        })
    }
}

fn classify(error: &serde_json::Error) -> BusinessEventError {
    let message = error.to_string();
    if message.starts_with("trailing characters") {
        BusinessEventError::TrailingBytes
    } else if message.starts_with("unknown field") {
        BusinessEventError::UnknownField
    } else if message.starts_with("duplicate field") {
        BusinessEventError::DuplicateField
    } else if message.starts_with("missing field") {
        BusinessEventError::MissingField
    } else if message.starts_with("invalid type")
        || message.starts_with("invalid value")
        || message.starts_with("invalid length")
    {
        BusinessEventError::InvalidType
    } else {
        BusinessEventError::Syntax
    }
}

/// Canonical bytes of an event (refused as `oversize` past [`MAX_EVENT_BYTES`]).
pub fn encode_business_event(event: &StudioBusinessEvent) -> Result<Vec<u8>, BusinessEventError> {
    let bytes = serde_json::to_vec(&event.wire()).map_err(|_| BusinessEventError::EncodeFailed)?;
    if bytes.len() > MAX_EVENT_BYTES {
        return Err(BusinessEventError::Oversize);
    }
    Ok(bytes)
}

/// Strict decode: bounded before parsing, exactly one canonical JSON object, every construction
/// rule rechecked.
pub fn decode_business_event(bytes: &[u8]) -> Result<StudioBusinessEvent, BusinessEventError> {
    if bytes.is_empty() {
        return Err(BusinessEventError::Empty);
    }
    if bytes.len() > MAX_EVENT_BYTES {
        return Err(BusinessEventError::Oversize);
    }
    if bytes[0] != b'{' {
        return Err(BusinessEventError::Syntax);
    }
    if bytes.last().is_some_and(u8::is_ascii_whitespace) {
        return Err(BusinessEventError::TrailingBytes);
    }
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let wire = EventWire::deserialize(&mut deserializer).map_err(|e| classify(&e))?;
    deserializer.end().map_err(|e| classify(&e))?;
    let event = wire.into_event()?;
    if encode_business_event(&event)? != bytes {
        return Err(BusinessEventError::NonCanonical);
    }
    Ok(event)
}
