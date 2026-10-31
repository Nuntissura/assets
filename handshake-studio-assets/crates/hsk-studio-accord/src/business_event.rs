//! Studio business-event vocabulary (lane SMB-BUSINESS-EVENTS-1, RF-07; STU-CON-003,
//! STU-CON-013, STU-CON-014): the closed catalog of the 22 canonical wire identities with their
//! `studio.*` family and exact `FR-EVT-STUDIO-*` business id, the record a producer hands over
//! when a command was committed, and the non-blocking sink it hands it to.
//!
//! The record carries ids only: the event identity, the document resource id, the exact revision,
//! the committing command's [`CommandContext`] and typed artifact refs. It has no field for
//! document text, a label, a path or a font name. Like the context, a record grants nothing.
//! The canonical `hsk.studio.business_event@1` envelope and its encoder live in
//! `hsk-studio-kernel-bindings`; the host's Flight Recorder / EventLedger delivery is MT-36230.
use crate::{CommandContext, DomainId, SCHEMA_STUDIO_DOCUMENT, SchemaId, ValidationError};

/// Most typed artifact refs one record carries; checked before any ref is copied.
pub const MAX_BUSINESS_EVENT_ARTIFACTS: usize = 64;

/// The 22 canonical Studio business events (STU-CON-003 wire identities, STU-CON-014 catalog).
/// Every match over it is exhaustive, so a new variant does not compile until it has a wire
/// name, a family and a business id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StudioBusinessEventId {
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

impl StudioBusinessEventId {
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

/// The deterministic event id of the `sequence`-th event (0-based) of `revision` of `document`
/// (STU-CON-013 deterministic identity; the host adds its workspace partition). Two different
/// events never share an id while revisions are unique per document; a producer that reuses one
/// for different content meets the sink's immutable-envelope collision refusal.
pub fn business_event_id(document: &DomainId, revision: u64, sequence: u64) -> String {
    format!("{}@{revision}#{sequence}", document.as_str())
}

/// What a producer hands over after the session accepted a commit: which event, of which
/// document, at which exact revision, the committing command's context and typed artifact refs.
/// Ids only. Built through [`BusinessEventRecord::new`], so the document is always an `SDOC` id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BusinessEventRecord {
    event: StudioBusinessEventId,
    document: DomainId,
    revision: u64,
    sequence: u64,
    context: CommandContext,
    artifacts: Vec<DomainId>,
}

impl BusinessEventRecord {
    /// `document` must be a valid id for `hsk.studio.document@1` (`SDOC`): `invalid_id` or
    /// `prefix_mismatch` otherwise. The context may lack a command id; the envelope encoder
    /// refuses such a record (a business event records a committed command).
    pub fn new(
        event: StudioBusinessEventId,
        document: &str,
        revision: u64,
        sequence: u64,
        context: CommandContext,
    ) -> Result<Self, ValidationError> {
        let schema = SchemaId::parse(SCHEMA_STUDIO_DOCUMENT)?;
        let document = DomainId::for_schema(document, &schema)?;
        Ok(Self {
            event,
            document,
            revision,
            sequence,
            context,
            artifacts: Vec::new(),
        })
    }

    /// Typed artifact refs (accord domain ids: layers, paths, stories, ...). More than
    /// [`MAX_BUSINESS_EVENT_ARTIFACTS`] is `item_limit` before any is copied; an invalid or
    /// repeated id is `invalid_id`.
    pub fn with_artifacts(mut self, refs: &[&str]) -> Result<Self, ValidationError> {
        if refs.len() > MAX_BUSINESS_EVENT_ARTIFACTS {
            return Err(ValidationError::ItemLimit);
        }
        let mut out: Vec<DomainId> = Vec::with_capacity(refs.len());
        for value in refs {
            let id = DomainId::parse(value)?;
            if out.contains(&id) {
                return Err(ValidationError::InvalidId);
            }
            out.push(id);
        }
        self.artifacts = out;
        Ok(self)
    }

    pub fn event(&self) -> StudioBusinessEventId {
        self.event
    }
    pub fn document(&self) -> &DomainId {
        &self.document
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn context(&self) -> &CommandContext {
        &self.context
    }
    pub fn artifacts(&self) -> &[DomainId] {
        &self.artifacts
    }
    /// [`business_event_id`] of this record.
    pub fn event_id(&self) -> String {
        business_event_id(&self.document, self.revision, self.sequence)
    }
}

/// Why a business event was not delivered. Closed; the semantics are observe's `DeliveryError`:
/// all-or-none and non-blocking, and a sink that cannot know whether the event escaped returns
/// `Indeterminate`, never `Ok`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusinessEventDeliveryError {
    Saturated,
    Rejected,
    Unavailable,
    Indeterminate,
}

impl BusinessEventDeliveryError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Saturated => "saturated",
            Self::Rejected => "rejected",
            Self::Unavailable => "unavailable",
            Self::Indeterminate => "indeterminate",
        }
    }

    /// `Indeterminate` alone: the event may have escaped, so the producer must reconcile before
    /// it can call the event delivered or lost.
    pub const fn needs_reconciliation(self) -> bool {
        matches!(self, Self::Indeterminate)
    }
}

impl std::fmt::Display for BusinessEventDeliveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for BusinessEventDeliveryError {}

/// Where a producer hands committed business events. Non-blocking and all-or-none. The
/// asset-local in-memory sink is `hsk-studio-kernel-bindings` `TransientBusinessEvents`; a host
/// sink delivers to the Flight Recorder / EventLedger (MT-36230).
pub trait BusinessEventSink: Send {
    fn try_emit(&mut self, record: &BusinessEventRecord) -> Result<(), BusinessEventDeliveryError>;
}
