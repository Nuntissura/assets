//! Studio business events (STU-CON-003, STU-CON-013, STU-CON-014; MT-36230 asset side, lane
//! SMB-BUSINESS-EVENTS-1). The 22 canonical wire variants with their `studio.*` family and exact
//! `FR-EVT-STUDIO-*` business id, the typed `hsk.studio.business_event@1` envelope, and the
//! delivery port the binding emits through. The binding emits, never a leaf engine: no leaf crate
//! names these types.
//!
//! The envelope carries ids and digests only (document, actor, client session, event id, sequence,
//! correlation, causation, exact revision, command id, optional input/output digests and typed
//! artifact refs). It has no field that can hold document text, a label, a path or a font name
//! (test `envelopes_carry_ids_and_digests_only`). Its wire is compact JSON in canonical field
//! order; decoding refuses everything the encoder cannot produce (test
//! `envelopes_are_strict_and_refuse_typed`).
//!
//! Registration in the host's closed Flight Recorder / kernel event enums, durable delivery and
//! reconciliation are the host's (MT-36230): an asset-local sink result is evidence, not host
//! acceptance.
#![deny(clippy::arithmetic_side_effects)]
use crate::wire::ActorWire;
use hsk_studio_accord::{
    ActorContext, DomainId, SCHEMA_STUDIO_BUSINESS_EVENT, SCHEMA_STUDIO_DOCUMENT, SchemaId,
};
use serde::{Deserialize, Serialize};

/// Largest encoded event; checked on the input before it is parsed and on the output after it is
/// encoded. The largest valid event (every bound at its maximum) fits
/// (`envelopes_are_strict_and_refuse_typed`).
pub const MAX_EVENT_BYTES: usize = 8 * 1024;
/// Most typed artifact refs one event carries; checked before any ref is copied.
pub const MAX_ARTIFACT_REFS: usize = 64;
/// Longest command id (`[a-z0-9._-]`).
pub const MAX_COMMAND_ID_BYTES: usize = 64;
/// Longest client session id (the accord attribution bound).
pub const MAX_CLIENT_SESSION_BYTES: usize = 128;

/// The 22 canonical Studio business events (STU-CON-003 wire identities, STU-CON-014 catalog).
/// Every match over it is exhaustive, so a new variant does not compile until it has a wire
/// name, a family and a business id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StudioEventVariant {
    DocumentCreated,
    DocumentPromoted,
    LayerCreated,
    LayerMutated,
    SelectionChanged,
    MaskApplied,
    AdjustmentApplied,
    EffectApplied,
    VectorPathEdited,
    TextEdited,
    StyleApplied,
    ComponentPublished,
    VariableSet,
    PrototypeEdited,
    RawDeveloped,
    ExportRendered,
    ImportCompleted,
    EditProposalRecorded,
    EditPromoted,
    EditRejected,
    HistoryUndone,
    HistoryRedone,
}

impl StudioEventVariant {
    /// All 22, in the STU-CON-014 table order.
    pub const ALL: [Self; 22] = [
        Self::DocumentCreated,
        Self::DocumentPromoted,
        Self::LayerCreated,
        Self::LayerMutated,
        Self::SelectionChanged,
        Self::MaskApplied,
        Self::AdjustmentApplied,
        Self::EffectApplied,
        Self::VectorPathEdited,
        Self::TextEdited,
        Self::StyleApplied,
        Self::ComponentPublished,
        Self::VariableSet,
        Self::PrototypeEdited,
        Self::RawDeveloped,
        Self::ExportRendered,
        Self::ImportCompleted,
        Self::EditProposalRecorded,
        Self::EditPromoted,
        Self::EditRejected,
        Self::HistoryUndone,
        Self::HistoryRedone,
    ];

    /// The canonical wire variant (`STUDIO_*`, STU-CON-003).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DocumentCreated => "STUDIO_DOCUMENT_CREATED",
            Self::DocumentPromoted => "STUDIO_DOCUMENT_PROMOTED",
            Self::LayerCreated => "STUDIO_LAYER_CREATED",
            Self::LayerMutated => "STUDIO_LAYER_MUTATED",
            Self::SelectionChanged => "STUDIO_SELECTION_CHANGED",
            Self::MaskApplied => "STUDIO_MASK_APPLIED",
            Self::AdjustmentApplied => "STUDIO_ADJUSTMENT_APPLIED",
            Self::EffectApplied => "STUDIO_EFFECT_APPLIED",
            Self::VectorPathEdited => "STUDIO_VECTOR_PATH_EDITED",
            Self::TextEdited => "STUDIO_TEXT_EDITED",
            Self::StyleApplied => "STUDIO_STYLE_APPLIED",
            Self::ComponentPublished => "STUDIO_COMPONENT_PUBLISHED",
            Self::VariableSet => "STUDIO_VARIABLE_SET",
            Self::PrototypeEdited => "STUDIO_PROTOTYPE_EDITED",
            Self::RawDeveloped => "STUDIO_RAW_DEVELOPED",
            Self::ExportRendered => "STUDIO_EXPORT_RENDERED",
            Self::ImportCompleted => "STUDIO_IMPORT_COMPLETED",
            Self::EditProposalRecorded => "STUDIO_EDIT_PROPOSAL_RECORDED",
            Self::EditPromoted => "STUDIO_EDIT_PROMOTED",
            Self::EditRejected => "STUDIO_EDIT_REJECTED",
            Self::HistoryUndone => "STUDIO_HISTORY_UNDONE",
            Self::HistoryRedone => "STUDIO_HISTORY_REDONE",
        }
    }

    /// The `studio.*` event family (STU-ARC-003 names, STU-CON-014 mapping).
    pub const fn family(self) -> &'static str {
        match self {
            Self::DocumentCreated
            | Self::DocumentPromoted
            | Self::LayerCreated
            | Self::LayerMutated
            | Self::SelectionChanged
            | Self::ImportCompleted => "studio.document",
            Self::MaskApplied => "studio.raster",
            Self::AdjustmentApplied | Self::RawDeveloped => "studio.color",
            Self::EffectApplied => "studio.effect",
            Self::VectorPathEdited => "studio.vector",
            Self::TextEdited => "studio.typography",
            Self::StyleApplied => "studio.layout",
            Self::ComponentPublished | Self::VariableSet => "studio.design_system",
            Self::PrototypeEdited => "studio.prototype",
            Self::ExportRendered => "studio.export",
            Self::EditProposalRecorded | Self::EditPromoted | Self::EditRejected => {
                "studio.proposal"
            }
            Self::HistoryUndone | Self::HistoryRedone => "studio.history",
        }
    }

    /// The exact Flight Recorder business id (`FR-EVT-STUDIO-*`).
    pub const fn business_id(self) -> &'static str {
        match self {
            Self::DocumentCreated => "FR-EVT-STUDIO-DOCUMENT-CREATED",
            Self::DocumentPromoted => "FR-EVT-STUDIO-DOCUMENT-PROMOTED",
            Self::LayerCreated => "FR-EVT-STUDIO-LAYER-CREATED",
            Self::LayerMutated => "FR-EVT-STUDIO-LAYER-MUTATED",
            Self::SelectionChanged => "FR-EVT-STUDIO-SELECTION-CHANGED",
            Self::MaskApplied => "FR-EVT-STUDIO-MASK-APPLIED",
            Self::AdjustmentApplied => "FR-EVT-STUDIO-ADJUSTMENT-APPLIED",
            Self::EffectApplied => "FR-EVT-STUDIO-EFFECT-APPLIED",
            Self::VectorPathEdited => "FR-EVT-STUDIO-VECTOR-PATH-EDITED",
            Self::TextEdited => "FR-EVT-STUDIO-TEXT-EDITED",
            Self::StyleApplied => "FR-EVT-STUDIO-STYLE-APPLIED",
            Self::ComponentPublished => "FR-EVT-STUDIO-COMPONENT-PUBLISHED",
            Self::VariableSet => "FR-EVT-STUDIO-VARIABLE-SET",
            Self::PrototypeEdited => "FR-EVT-STUDIO-PROTOTYPE-EDITED",
            Self::RawDeveloped => "FR-EVT-STUDIO-RAW-DEVELOPED",
            Self::ExportRendered => "FR-EVT-STUDIO-EXPORT-RENDERED",
            Self::ImportCompleted => "FR-EVT-STUDIO-IMPORT-COMPLETED",
            Self::EditProposalRecorded => "FR-EVT-STUDIO-EDIT-PROPOSAL-RECORDED",
            Self::EditPromoted => "FR-EVT-STUDIO-EDIT-PROMOTED",
            Self::EditRejected => "FR-EVT-STUDIO-EDIT-REJECTED",
            Self::HistoryUndone => "FR-EVT-STUDIO-HISTORY-UNDONE",
            Self::HistoryRedone => "FR-EVT-STUDIO-HISTORY-REDONE",
        }
    }

    /// Whether the event describes an accepted document change. Selection, proposal and rejection
    /// events describe view, audit or proposal state and never claim a document promotion
    /// (STU-CON-014); their `revision` is the revision they observed, not a successor.
    pub const fn claims_document_promotion(self) -> bool {
        match self {
            Self::SelectionChanged | Self::EditProposalRecorded | Self::EditRejected => false,
            Self::DocumentCreated
            | Self::DocumentPromoted
            | Self::LayerCreated
            | Self::LayerMutated
            | Self::MaskApplied
            | Self::AdjustmentApplied
            | Self::EffectApplied
            | Self::VectorPathEdited
            | Self::TextEdited
            | Self::StyleApplied
            | Self::ComponentPublished
            | Self::VariableSet
            | Self::PrototypeEdited
            | Self::RawDeveloped
            | Self::ExportRendered
            | Self::ImportCompleted
            | Self::EditPromoted
            | Self::HistoryUndone
            | Self::HistoryRedone => true,
        }
    }

    /// The variant with this exact wire name.
    pub fn parse(wire: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == wire)
    }
}

/// Why a variant has no asset-local producer. Each is typed and tested in the catalog and is
/// NOT_PROVEN as an emission until a producer exists.
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
/// (`variants_without_a_producer_are_listed_not_faked`).
pub const NOT_PRODUCED_ASSET_LOCALLY: [(StudioEventVariant, NotProducedReason); 9] = [
    (StudioEventVariant::DocumentPromoted, NotProducedReason::NoPromotionOrProposalFlow),
    (StudioEventVariant::EditProposalRecorded, NotProducedReason::NoPromotionOrProposalFlow),
    (StudioEventVariant::EditPromoted, NotProducedReason::NoPromotionOrProposalFlow),
    (StudioEventVariant::EditRejected, NotProducedReason::NoPromotionOrProposalFlow),
    (StudioEventVariant::SelectionChanged, NotProducedReason::ViewStateNotACommit),
    (StudioEventVariant::ComponentPublished, NotProducedReason::NoMappedCommand),
    (StudioEventVariant::VariableSet, NotProducedReason::NoMappedCommand),
    (StudioEventVariant::PrototypeEdited, NotProducedReason::NoMappedCommand),
    (StudioEventVariant::ExportRendered, NotProducedReason::ExportOutsideRun),
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
    InvalidCommandId,
    /// Not 64 lowercase hex digits (a SHA-256).
    InvalidDigest,
    /// Not a valid accord domain id, or repeated.
    InvalidArtifact,
    TooManyArtifacts,
    /// The event id is not the one derived from document and sequence.
    EventIdMismatch,
    /// The same event id was already recorded with a different envelope (STU-CON-013).
    EnvelopeCollision,
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
            Self::InvalidCommandId => "invalid_command_id",
            Self::InvalidDigest => "invalid_digest",
            Self::InvalidArtifact => "invalid_artifact",
            Self::TooManyArtifacts => "too_many_artifacts",
            Self::EventIdMismatch => "event_id_mismatch",
            Self::EnvelopeCollision => "envelope_collision",
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

/// Who and what a business event is attributed to: the caller's command context, as wire values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventAttribution<'a> {
    pub actor: &'a ActorContext,
    pub client_session: &'a str,
    pub correlation: u64,
    pub causation: Option<u64>,
}

/// One typed `hsk.studio.business_event@1` envelope. Built only through [`StudioBusinessEvent::new`]
/// or [`decode_business_event`], so every value satisfies the construction rules: an `SDOC`
/// document, the event id derived from document and sequence, bounded ids and digests only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StudioBusinessEvent {
    variant: StudioEventVariant,
    document: DomainId,
    actor: ActorContext,
    client_session: String,
    event_id: String,
    sequence: u64,
    correlation: u64,
    causation: Option<u64>,
    revision: u64,
    command_id: String,
    input_digest: Option<String>,
    output_digest: Option<String>,
    artifacts: Vec<DomainId>,
}

/// The deterministic event id of `sequence` in `document` (STU-CON-013 deterministic identity;
/// the host adds its workspace partition).
pub fn business_event_id(document: &DomainId, sequence: u64) -> String {
    format!("{}#{sequence}", document.as_str())
}

fn valid_session(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_CLIENT_SESSION_BYTES
        && !value.chars().any(char::is_control)
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
    if refs.len() > MAX_ARTIFACT_REFS {
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
    /// Builds an event for `document` at `sequence` and the exact `revision` it describes. Every
    /// bound is checked before anything is copied.
    pub fn new(
        variant: StudioEventVariant,
        document: &str,
        attribution: &EventAttribution<'_>,
        sequence: u64,
        revision: u64,
        command_id: &str,
    ) -> Result<Self, BusinessEventError> {
        if !valid_command_id(command_id) {
            return Err(BusinessEventError::InvalidCommandId);
        }
        if !valid_session(attribution.client_session) {
            return Err(BusinessEventError::InvalidClientSession);
        }
        let document = document_id(document)?;
        Ok(Self {
            variant,
            event_id: business_event_id(&document, sequence),
            document,
            actor: attribution.actor.clone(),
            client_session: attribution.client_session.to_owned(),
            sequence,
            correlation: attribution.correlation,
            causation: attribution.causation,
            revision,
            command_id: command_id.to_owned(),
            input_digest: None,
            output_digest: None,
            artifacts: Vec::new(),
        })
    }

    /// Adds the command's input and output digests (SHA-256, lowercase hex).
    pub fn with_digests(mut self, input: Option<&str>, output: Option<&str>) -> Result<Self, BusinessEventError> {
        self.input_digest = digest(input)?;
        self.output_digest = digest(output)?;
        Ok(self)
    }

    /// Adds typed artifact refs (accord domain ids: layers, paths, stories, ...).
    pub fn with_artifacts(mut self, refs: &[&str]) -> Result<Self, BusinessEventError> {
        self.artifacts = artifact_refs(refs.iter().copied())?;
        Ok(self)
    }

    pub fn variant(&self) -> StudioEventVariant {
        self.variant
    }
    pub fn document(&self) -> &DomainId {
        &self.document
    }
    pub fn actor(&self) -> &ActorContext {
        &self.actor
    }
    pub fn client_session(&self) -> &str {
        &self.client_session
    }
    pub fn event_id(&self) -> &str {
        &self.event_id
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn correlation(&self) -> u64 {
        self.correlation
    }
    pub fn causation(&self) -> Option<u64> {
        self.causation
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn command_id(&self) -> &str {
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
            variant: self.variant.as_str().to_owned(),
            event_family: self.variant.family().to_owned(),
            business_id: self.variant.business_id().to_owned(),
            document: self.document.as_str().to_owned(),
            actor: ActorWire::from_context(&self.actor),
            client_session: self.client_session.clone(),
            event_id: self.event_id.clone(),
            sequence: self.sequence,
            correlation: self.correlation,
            causation: self.causation,
            revision: self.revision,
            command_id: self.command_id.clone(),
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
    sequence: u64,
    correlation: u64,
    causation: Option<u64>,
    revision: u64,
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
        let variant = StudioEventVariant::parse(&self.variant).ok_or(BusinessEventError::UnknownVariant)?;
        if self.event_family != variant.family() {
            return Err(BusinessEventError::FamilyMismatch);
        }
        if self.business_id != variant.business_id() {
            return Err(BusinessEventError::BusinessIdMismatch);
        }
        let actor = self.actor.to_context().map_err(|_| BusinessEventError::InvalidActor)?;
        let attribution = EventAttribution {
            actor: &actor,
            client_session: &self.client_session,
            correlation: self.correlation,
            causation: self.causation,
        };
        let event = StudioBusinessEvent::new(
            variant,
            &self.document,
            &attribution,
            self.sequence,
            self.revision,
            &self.command_id,
        )?
        .with_digests(self.input_digest.as_deref(), self.output_digest.as_deref())?;
        let event = StudioBusinessEvent {
            artifacts: artifact_refs(self.artifacts.iter().map(String::as_str))?,
            ..event
        };
        if self.event_id != event.event_id {
            return Err(BusinessEventError::EventIdMismatch);
        }
        Ok(event)
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

/// Why a business event was not delivered. Closed; the semantics are observe's `DeliveryError`:
/// all-or-none and nonblocking, and an adapter that cannot know whether the event escaped returns
/// `Indeterminate`, never `Ok`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventDeliveryError {
    Saturated,
    Rejected,
    Unavailable,
    Indeterminate,
}

impl EventDeliveryError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Saturated => "saturated",
            Self::Rejected => "rejected",
            Self::Unavailable => "unavailable",
            Self::Indeterminate => "indeterminate",
        }
    }

    /// `Indeterminate` alone: the event may have escaped, so the caller must reconcile before it
    /// can call the event delivered or lost.
    pub const fn needs_reconciliation(self) -> bool {
        matches!(self, Self::Indeterminate)
    }
}

impl std::fmt::Display for EventDeliveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for EventDeliveryError {}

/// Where the binding emits business events. A real adapter delivers to the host's Flight
/// Recorder and EventLedger (MT-36230); the asset-local [`crate::TransientBusinessEvents`] keeps
/// them in memory.
pub trait BusinessEventPort: Send + Sync {
    fn try_emit(&self, event: &StudioBusinessEvent) -> Result<(), EventDeliveryError>;
}

impl<T: BusinessEventPort + ?Sized> BusinessEventPort for std::sync::Arc<T> {
    fn try_emit(&self, event: &StudioBusinessEvent) -> Result<(), EventDeliveryError> {
        (**self).try_emit(event)
    }
}
