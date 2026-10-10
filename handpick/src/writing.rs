//! Native editor orchestration. No content, native coordinates, undo or persistence is copied here.
use crate::*;
use serde::{Deserialize, Serialize};

/// Explicit owner-held titles take precedence; derived labels never mutate draft metadata.
pub fn resolve_draft_title<'a>(title: &'a str, fallback: &'a str) -> &'a str {
    let title = title.trim();
    if title.is_empty() { fallback.trim() } else { title }
}

// Decimal strings preserve u64 counters across JSON/JavaScript boundaries.
mod counter {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        let value = String::deserialize(deserializer)?;
        let parsed = value.parse::<u64>().map_err(serde::de::Error::custom)?;
        if parsed.to_string() != value {
            return Err(serde::de::Error::custom("expected canonical decimal u64"));
        }
        Ok(parsed)
    }
}

/// Distinct from foundation text coordinates; every callback carries the whole lifecycle fence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WritingFence {
    pub context: ContextId,
    pub session: SessionId,
    pub editor: EditorId,
    #[serde(with = "counter")]
    pub revision: u64,
    #[serde(with = "counter")]
    pub generation: u64,
}

/// IME token survives edits within its editor but never a park/restore/rebind lifecycle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionToken {
    pub context: ContextId,
    pub session: SessionId,
    pub editor: EditorId,
    #[serde(with = "counter")]
    pub sequence: u64,
}

/// An immutable owner-resolved address, separate from an occurrence in a source note.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NoteAddress {
    Note { note: NoteId },
    Heading { note: NoteId, block: BlockId },
    Block { note: NoteId, block: BlockId },
}

/// A submitted destination is never inferred from search text, a date or a title.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SaveDestination {
    Create {
        folder: Option<FolderId>,
    },
    Append {
        note: NoteId,
        expected_revision: RevisionId,
    },
    /// Continue the fragment committed by an earlier capture, preserving its ordinary Note identity.
    Update {
        note: NoteId,
        expected_revision: RevisionId,
        prior_operation: OperationId,
    },
    Insert {
        target: NoteAddress,
        expected_revision: RevisionId,
        side: InsertionSide,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsertionSide {
    Before,
    After,
}

/// Owner validates the native snapshot at this editor revision and retains replay evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveIntent {
    pub operation: OperationId,
    pub fence: WritingFence,
    pub destination: SaveDestination,
}

/// Derived relationships/search readiness never changes canonical content commitment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionReadiness {
    Pending,
    Ready,
    Stale,
    Failed,
    Skipped,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContentOutcome {
    Committed {
        note: NoteId,
        revision: RevisionId,
        relationships: ProjectionReadiness,
        search: ProjectionReadiness,
    },
    Rejected,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveOutcome {
    pub submitted: SaveIntent,
    pub content: ContentOutcome,
}

/// Matching acknowledgement is evidence for this revision only; the core never clears an editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveDisposition {
    CurrentRevisionCommitted,
    OlderRevisionCommitted,
    RetainActiveComposition,
    RetainForRetry,
    ReconcileBeforeRetry,
}

/// Capability declarations are owner evidence, never inferred from local view fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WritingCapability {
    RichEditor,
    MarkdownSource,
    LivePreview,
    SplitMerge,
    IndentOutdent,
    MoveSubtree,
    CollapseFocus,
    NoteReference,
    HeadingReference,
    BlockReference,
    Embed,
    Backlinks,
    UnlinkedMentions,
    FolderPlacement,
    AnchoredInsert,
    Append,
    UpdateCapture,
    Recovery,
    Templates,
    Search,
    StructuralQuery,
    DurableTags,
    TagHierarchy,
    TagProperties,
    DurableAliases,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum CapabilitySupport {
    Supported { contract: RevisionId },
    Unsupported,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityDeclaration {
    pub capability: WritingCapability,
    pub support: CapabilitySupport,
}

/// Providers declare the canonical authorized set before pagination and bound every dispatch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WritingProvider {
    pub provider: ProviderId,
    pub canonical_set: RevisionId,
    pub capabilities: Vec<CapabilityDeclaration>,
    pub max_query_bytes: usize,
    pub max_page_items: usize,
}

/// Deliberately selected text or explicit query only; never implicitly the entire composition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionQuery {
    pub fence: WritingFence,
    pub query: String,
    pub provider: ProviderId,
    pub page_items: usize,
    pub cursor: Option<RevisionId>,
}

impl WritingProvider {
    pub fn validate_query(
        &self,
        session: &WritingSession,
        query: &CompositionQuery,
    ) -> Result<(), Error> {
        session.validate(&query.fence)?;
        if query.provider != self.provider {
            return Err(Error::Stale);
        }
        if query.query.is_empty()
            || query.query.len() > self.max_query_bytes
            || query.page_items == 0
            || query.page_items > self.max_page_items
        {
            return Err(Error::PayloadLimit);
        }
        if !self.capabilities.iter().any(|cap| {
            matches!(cap.support, CapabilitySupport::Supported { .. })
                && cap.capability == WritingCapability::Search
        }) {
            return Err(Error::Revoked);
        }
        Ok(())
    }
}

/// Typed action preview; a matching suggestion alone never applies an edit or effect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WritingAction {
    Reference {
        target: NoteAddress,
        embed: bool,
    },
    Outline {
        block: BlockId,
        operation: OutlineOperation,
    },
    Template {
        template: ResultKey,
    },
    Save {
        destination: SaveDestination,
    },
    OwnerAction {
        action: Action,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutlineOperation {
    Split,
    Merge,
    Indent,
    Outdent,
    MoveUp,
    MoveDown,
    Collapse,
    Expand,
    Focus,
    Back,
}

/// Native selection/marks/coordinates are resolved by the editor at this fenced revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WritingActionIntent {
    pub fence: WritingFence,
    pub action: WritingAction,
}

/// Counts explicitly name their canonical coverage; loaded rows cannot stand in for totals.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
pub enum RelationshipCounts {
    CanonicalAuthorized {
        #[serde(with = "counter")]
        source_notes: u64,
        #[serde(with = "counter")]
        relationships: u64,
    },
    AuthorizedProjection {
        #[serde(with = "counter")]
        source_notes: u64,
        #[serde(with = "counter")]
        relationships: u64,
    },
    PageOnly,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RelationshipEvidence {
    Linked {
        relationship: RelationshipId,
        source_note: NoteId,
        source_block: Option<BlockId>,
        source_revision: RevisionId,
        target: NoteAddress,
        context: Option<RelationshipContext>,
    },
    Unlinked {
        source_note: NoteId,
        source_block: Option<BlockId>,
        source_revision: RevisionId,
        candidate: NoteAddress,
        context: Option<RelationshipContext>,
    },
    Provisional {
        editor: EditorId,
        candidate: NoteAddress,
    },
}

/// Authorized owner context; never resolved by reading another provider's private state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationshipContext {
    pub source_title: String,
    pub snippet: Option<String>,
    pub source_hash: Option<RevisionId>,
}

/// Owner-provided page evidence; missing freshness never appears ready.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationshipPage {
    pub fence: WritingFence,
    pub provider: ProviderId,
    pub projection_revision: Option<RevisionId>,
    pub freshness: ProjectionReadiness,
    pub status: DeliveryStatus,
    pub counts: RelationshipCounts,
    pub next_cursor: Option<RevisionId>,
    pub rows: Vec<RelationshipEvidence>,
}

impl RelationshipPage {
    pub fn validate(
        &self,
        session: &WritingSession,
        provider: &ProviderId,
        max_rows: usize,
    ) -> Result<(), Error> {
        session.validate(&self.fence)?;
        if &self.provider != provider {
            return Err(Error::Stale);
        }
        if self.rows.len() > max_rows {
            return Err(Error::PayloadLimit);
        }
        for row in &self.rows {
            let context = match row {
                RelationshipEvidence::Linked { context, .. }
                | RelationshipEvidence::Unlinked { context, .. } => context.as_ref(),
                RelationshipEvidence::Provisional { .. } => None,
            };
            if context.is_some_and(|context| {
                context
                    .source_title
                    .len()
                    .saturating_add(context.snippet.as_ref().map_or(0, String::len))
                    > 4096
            }) {
                return Err(Error::PayloadLimit);
            }
        }
        if self.freshness == ProjectionReadiness::Ready && self.projection_revision.is_none() {
            return Err(Error::Stale);
        }
        if matches!(self.counts, RelationshipCounts::CanonicalAuthorized { .. })
            && self.freshness != ProjectionReadiness::Ready
        {
            return Err(Error::Stale);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReferenceResolution {
    Resolved {
        target: NoteAddress,
        revision: RevisionId,
    },
    Provisional,
    Missing,
    Deleted,
    Inaccessible,
    Ambiguous,
    Unsupported,
}

/// Native recovery is an owner handle; this contract promises neither disk durability nor sync.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum RecoveryState {
    MemoryOnly,
    Pending,
    Recoverable { checkpoint: RevisionId },
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WritingPin {
    pub id: PinId,
    pub editor: EditorId,
    pub revision: u64,
}

/// Bounded orchestration over live owner editor handles. Pins keep the entire native session alive.
/// Hosts must allocate unique SessionId/EditorId values and release handles only after owner recovery.
#[derive(Debug)]
pub struct WritingSession {
    context: ContextId,
    session: SessionId,
    active: bool,
    generation: u64,
    editor: Option<(EditorId, u64)>,
    pins: Vec<WritingPin>,
    max_pins: usize,
    pending_save: Option<SaveIntent>,
    query_purpose: QueryPurpose,
    expanded: bool,
    composition: Option<CompositionToken>,
    results: Session,
}

impl WritingSession {
    pub fn new(context: ContextId, session: SessionId, max_pins: usize) -> Self {
        Self::with_limits(
            context,
            session,
            Limits {
                max_text_bytes: 0,
                max_pins,
                max_items: 100,
                max_item_bytes: 4096,
                max_actions_per_item: 16,
            },
        )
    }
    /// Host-selected result and pin bounds; native content remains entirely owner-held.
    pub fn with_limits(context: ContextId, session: SessionId, limits: Limits) -> Self {
        let results = Session::new(context.clone(), limits);
        Self {
            context,
            session,
            active: true,
            generation: 0,
            editor: None,
            pins: vec![],
            max_pins: limits.max_pins,
            pending_save: None,
            query_purpose: QueryPurpose::SearchWrite,
            expanded: false,
            composition: None,
            results,
        }
    }
    fn active(&self) -> Result<(), Error> {
        if self.active {
            Ok(())
        } else {
            Err(Error::Revoked)
        }
    }
    fn next(&self) -> Result<u64, Error> {
        self.generation
            .checked_add(1)
            .ok_or(Error::CounterExhausted)
    }
    pub fn editor(&self) -> Option<(&EditorId, u64)> {
        self.editor.as_ref().map(|(id, rev)| (id, *rev))
    }
    pub fn pins(&self) -> &[WritingPin] {
        &self.pins
    }
    pub fn pending_save(&self) -> Option<&SaveIntent> {
        self.pending_save.as_ref()
    }
    pub fn expanded(&self) -> bool {
        self.expanded
    }
    pub fn query_purpose(&self) -> QueryPurpose {
        self.query_purpose
    }
    /// Change lookup purpose without replacing or hiding the owner-held draft.
    /// Old deliveries are invalidated before the host starts its next request.
    pub fn set_query_purpose(&mut self, purpose: QueryPurpose) -> Result<bool, Error> {
        self.active()?;
        if self.query_purpose == purpose {
            return Ok(false);
        }
        if self.composition.is_some() || self.pending_save.is_some() {
            return Err(Error::Stale);
        }
        let next = self.next()?;
        self.query_purpose = purpose;
        self.generation = next;
        self.results.invalidate();
        Ok(true)
    }
    pub fn bind(&mut self, editor: EditorId, revision: u64) -> Result<(), Error> {
        self.active()?;
        if self.editor.is_some() {
            return Err(Error::OccupiedDraft);
        }
        if self.pins.iter().any(|pin| pin.editor == editor) {
            return Err(Error::DuplicatePin);
        }
        let next = self.next()?;
        self.editor = Some((editor, revision));
        self.generation = next;
        self.results.invalidate();
        self.composition = None;
        Ok(())
    }
    pub fn fence(&self) -> Result<WritingFence, Error> {
        self.active()?;
        let (editor, revision) = self.editor.as_ref().ok_or(Error::Stale)?;
        Ok(WritingFence {
            context: self.context.clone(),
            session: self.session.clone(),
            editor: editor.clone(),
            revision: *revision,
            generation: self.generation,
        })
    }
    pub fn validate(&self, fence: &WritingFence) -> Result<(), Error> {
        if &self.fence()? == fence {
            Ok(())
        } else {
            Err(Error::Stale)
        }
    }
    pub fn edited(&mut self, fence: &WritingFence, revision: u64) -> Result<(), Error> {
        self.validate(fence)?;
        if revision <= fence.revision {
            return Err(Error::Stale);
        }
        let next = self.next()?;
        self.editor = Some((fence.editor.clone(), revision));
        self.generation = next;
        self.results.invalidate();
        Ok(())
    }
    /// Presentation changes never remount or mutate the owner editor.
    pub fn expand(&mut self) -> Result<(), Error> {
        self.active()?;
        self.expanded = true;
        Ok(())
    }
    pub fn begin_composition(&mut self, fence: &WritingFence) -> Result<CompositionToken, Error> {
        self.validate(fence)?;
        if self.composition.is_some() {
            return Err(Error::Stale);
        }
        let sequence = self.next()?;
        let token = CompositionToken {
            context: self.context.clone(),
            session: self.session.clone(),
            editor: fence.editor.clone(),
            sequence,
        };
        self.generation = sequence;
        self.results.invalidate();
        self.composition = Some(token.clone());
        Ok(token)
    }
    pub fn end_composition(&mut self, token: &CompositionToken) -> Result<(), Error> {
        self.active()?;
        if self.composition.as_ref() != Some(token) {
            return Err(Error::Stale);
        }
        self.composition = None;
        Ok(())
    }
    /// Each lookup advances request identity while leaving native content and selection untouched.
    pub fn begin_lookup(&mut self) -> Result<WritingFence, Error> {
        self.fence()?;
        let next = self.next()?;
        self.generation = next;
        self.results.invalidate();
        self.fence()
    }
    /// Shared bounded admission and identity selection reuse the literal core without copying content.
    pub fn deliver(
        &mut self,
        fence: &WritingFence,
        channel: Channel,
        status: DeliveryStatus,
        items: Vec<SearchItem>,
    ) -> Result<(), Error> {
        self.validate(fence)?;
        self.results
            .deliver(&self.results.fence(), channel, status, items)
    }
    pub fn delivery(&self, channel: Channel) -> &Delivery {
        self.results.delivery(channel)
    }
    pub fn select(&mut self, fence: &WritingFence, key: ResultKey) -> Result<(), Error> {
        self.validate(fence)?;
        if self.composition.is_some() {
            return Err(Error::Stale);
        }
        self.results.select(key)
    }
    pub fn selected(&self) -> Option<&ResultKey> {
        self.results.selected()
    }
    /// Only a completed-empty applicable suggestion collection permits automatic expansion.
    /// Host adapters combine all applicable providers before delivering that collection.
    pub fn can_expand(&self) -> bool {
        self.active
            && self.query_purpose == QueryPurpose::SearchWrite
            && self.editor.is_some()
            && self.composition.is_none()
            && self.pending_save.is_none()
            && self.results.delivery(Channel::Suggestions).status() == DeliveryStatus::Complete
            && self
                .results
                .delivery(Channel::Suggestions)
                .items()
                .is_empty()
    }
    pub fn expand_if_empty(&mut self, fence: &WritingFence) -> Result<bool, Error> {
        self.validate(fence)?;
        if !self.can_expand() {
            return Ok(false);
        }
        self.expanded = true;
        Ok(true)
    }
    /// Host observes native text in UTF-16 units and newline-delimited lines; no content is copied.
    /// Prose intent is independent of result delivery; active lifecycle and IME fences still apply.
    pub fn expand_for_prose(&mut self, fence: &WritingFence, utf16_units: u32, line_count: u32) -> Result<bool, Error> {
        self.validate(fence)?;
        if self.query_purpose != QueryPurpose::SearchWrite || self.composition.is_some() || self.pending_save.is_some() || utf16_units == 0 || (utf16_units < 160 && line_count < 2) {
            return Ok(false);
        }
        self.expanded = true;
        Ok(true)
    }
    /// Apply owner-observed native emptiness/metrics without copying content or changing selection.
    pub fn present_content(&mut self, fence: &WritingFence, utf16_units: u32, line_count: u32, is_empty: bool) -> Result<bool, Error> {
        self.validate(fence)?;
        if self.composition.is_some() || self.pending_save.is_some() {
            return Ok(self.expanded);
        }
        if is_empty {
            let next = self.next()?;
            self.expanded = false;
            self.generation = next;
            self.results.invalidate();
        } else if self.can_expand() || (self.query_purpose == QueryPurpose::SearchWrite && utf16_units > 0 && (utf16_units >= 160 || line_count >= 2)) {
            self.expanded = true;
        }
        Ok(self.expanded)
    }
    /// Title and body remain one owner-held draft. A title keeps its editor discoverable.
    pub fn present_draft(&mut self, fence: &WritingFence, utf16_units: u32, line_count: u32, body_is_empty: bool, title: &str) -> Result<bool, Error> {
        self.validate(fence)?;
        if self.composition.is_some() || self.pending_save.is_some() {
            return Ok(self.expanded);
        }
        if !title.trim().is_empty() {
            if self.query_purpose == QueryPurpose::SearchWrite {
                self.expanded = true;
            }
            return Ok(self.expanded);
        }
        self.present_content(fence, utf16_units, line_count, body_is_empty)
    }
    /// Explicit compact presentation retains the same native editor and lookup state.
    pub fn compact(&mut self) -> Result<(), Error> {
        self.active()?;
        if self.composition.is_some() {
            return Err(Error::Stale);
        }
        self.expanded = false;
        Ok(())
    }
    pub fn validate_action(&self, intent: &WritingActionIntent) -> Result<(), Error> {
        self.validate(&intent.fence)?;
        if self.composition.is_some() {
            return Err(Error::Stale);
        }
        Ok(())
    }
    pub fn park(&mut self, id: PinId) -> Result<(), Error> {
        let fence = self.fence()?;
        if self.composition.is_some() {
            return Err(Error::Stale);
        }
        if self.pins.iter().any(|pin| pin.id == id) {
            return Err(Error::DuplicatePin);
        }
        if self.pins.len() >= self.max_pins {
            return Err(Error::PinLimit);
        }
        let next = self.next()?;
        self.pins.push(WritingPin {
            id,
            editor: fence.editor,
            revision: fence.revision,
        });
        self.editor = None;
        self.generation = next;
        self.results.invalidate();
        Ok(())
    }
    /// Occupied restore exchanges handles atomically; native content/selection/undo stay untouched.
    pub fn restore(&mut self, id: &PinId, replacement_pin: Option<PinId>) -> Result<(), Error> {
        self.active()?;
        if self.composition.is_some() {
            return Err(Error::Stale);
        }
        let index = self
            .pins
            .iter()
            .position(|pin| &pin.id == id)
            .ok_or(Error::MissingPin)?;
        if self.editor.is_some() && replacement_pin.is_none() {
            return Err(Error::OccupiedDraft);
        }
        if let Some(replacement) = &replacement_pin {
            if self.pins.iter().any(|pin| &pin.id == replacement) {
                return Err(Error::DuplicatePin);
            }
        }
        let next = self.next()?;
        let restored = self.pins.remove(index);
        if let Some((editor, revision)) = self.editor.take() {
            self.pins.insert(
                index,
                WritingPin {
                    id: replacement_pin.unwrap(),
                    editor,
                    revision,
                },
            );
        }
        self.editor = Some((restored.editor, restored.revision));
        self.generation = next;
        self.results.invalidate();
        Ok(())
    }
    /// One outstanding save remains until a definitive outcome, including committed-response loss.
    pub fn submit(&mut self, intent: SaveIntent) -> Result<(), Error> {
        self.validate(&intent.fence)?;
        if self.composition.is_some() || self.pending_save.is_some() {
            return Err(Error::Stale);
        }
        if matches!(
            &intent.destination,
            SaveDestination::Insert {
                target: NoteAddress::Note { .. },
                ..
            }
        ) {
            return Err(Error::InvalidSpan);
        }
        if matches!(&intent.destination, SaveDestination::Update { prior_operation, .. } if prior_operation == &intent.operation) {
            return Err(Error::InvalidSpan);
        }
        self.pending_save = Some(intent);
        Ok(())
    }
    pub fn acknowledge(&mut self, outcome: &SaveOutcome) -> Result<SaveDisposition, Error> {
        self.active()?;
        if self.pending_save.as_ref() != Some(&outcome.submitted) {
            return Err(Error::Stale);
        }
        match &outcome.content {
            ContentOutcome::Unknown => Ok(SaveDisposition::ReconcileBeforeRetry),
            ContentOutcome::Rejected => {
                self.pending_save = None;
                Ok(SaveDisposition::RetainForRetry)
            }
            ContentOutcome::Committed { note, .. } => {
                let existing = match &outcome.submitted.destination {
                    SaveDestination::Create { .. } => None,
                    SaveDestination::Append { note, .. } | SaveDestination::Update { note, .. } => Some(note),
                    SaveDestination::Insert { target, .. } => Some(match target {
                        NoteAddress::Note { note }
                        | NoteAddress::Heading { note, .. }
                        | NoteAddress::Block { note, .. } => note,
                    }),
                };
                if existing.is_some_and(|expected| expected != note) {
                    return Err(Error::Stale);
                }
                let current = self.editor.as_ref().is_some_and(|(editor, revision)| {
                    editor == &outcome.submitted.fence.editor
                        && revision == &outcome.submitted.fence.revision
                });
                self.pending_save = None;
                Ok(if current && self.composition.is_some() {
                    SaveDisposition::RetainActiveComposition
                } else if current {
                    SaveDisposition::CurrentRevisionCommitted
                } else {
                    SaveDisposition::OlderRevisionCommitted
                })
            }
        }
    }
    /// Caller must first recover/release native owner handles. Old callbacks remain revoked.
    pub fn revoke(&mut self) {
        self.active = false;
        self.editor = None;
        self.pins.clear();
        self.pending_save = None;
        self.composition = None;
        self.expanded = false;
        self.results.revoke();
    }
}
