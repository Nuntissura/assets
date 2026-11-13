//! Layout records in the session (lane SMB-LAYOUT-PERSIST-1; STU-LAY-013, STU-LAY-067, STU-SDB-001):
//! the layout editor's stories, layout-flow records, type styles, parent pages and long-document
//! records are the typed document entry `studio/layout_records.<sha256>` ([`LayoutRecords`]), opened,
//! read and saved under the rules of `crate::sequence_records` (an entry that does not validate opens
//! with a `LossReason::Opaque` note, refuses typed on read and keeps its bytes on save).
//!
//! One layout edit = one undo step: a [`LayoutStep`] quotes the live document and the live records
//! (`expect`) and writes their successors (`write`). The session commits both as ONE revision, one
//! receipt and one inverse ([`crate::Applied::layout_inverse`], its own inverse the redo), through
//! the submit pipeline of the other steps. The document quote ignores the `revision` field, so an
//! undo quotes the state, not the revision an earlier undo moved past. The records quote is the
//! entry's shape only (it parses), so a stale entry, one a later layer edit left naming a frame that
//! is gone, can still be replaced by records that validate; the write always validates against the
//! successor document. Refusals change nothing: `invalid_command`, `invalid_input` (another document
//! id), `value_conflict` (a quote that is not the live state, a repeated entry included),
//! `folio_rejected` (the successor would not validate), `layout_records_rejected` (the records would
//! not validate against it), `budget_exceeded` (also records whose bytes exceed the Folio input budget
//! every read applies),
//! `total_too_large`, `stale_revision`, and on [`crate::Session::submit_layout_as`] `actor_mismatch`
//! and `command_mismatch`.
//!
//! Bounds: the Folio input budget (`input_bytes`, `depth`) before any parse, [`MAX_LAYOUT_STORIES`],
//! then the Folio validators of every member; the package limits apply to the entry bytes. Not
//! closed: a repeated key inside the entry's top object keeps its last value (serde_json); every
//! member still validates.
use crate::error::SessionError;
use crate::sequence_records::{folio_err, held_body};
use crate::session::Session;
use crate::timeline::valid_command_id;
use hsk_studio_accord::{CancellationToken, SCHEMA_STUDIO_TYPE_STYLE};
use hsk_studio_folio::{
    self as folio, Budget, Code, Diagnostic, Resolver, Snapshot, StudioDocument, StudioLayoutFlowRecords, StudioLongDocRecords,
    StudioParentPageRecords, StudioTextStory, StudioTypeStyleRecords,
};
use hsk_studio_package::Quarantined;
use serde_json::{Value, json};

/// Document-entry token of the layout records.
pub const LAYOUT_RECORDS_ENTRY: &str = "layout_records";
/// Schema id the entry carries.
pub const LAYOUT_RECORDS_SCHEMA: &str = "hsk.studio.layout_records@1";
/// Provider name recorded in [`crate::Applied::provider`] and in the idempotency binding.
pub const LAYOUT_STEP_PROVIDER: &str = "layout.step";
/// Most stories one entry holds; more is `budget_exceeded` before any story is validated.
pub const MAX_LAYOUT_STORIES: usize = 4096;

const FIELDS: [&str; 7] = ["schema_id", "document_id", "stories", "flow", "styles", "parents", "longdoc"];

/// Everything the layout editor keeps beside the document (module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutRecords {
    pub document_id: String,
    pub stories: Vec<StudioTextStory>,
    pub flow: StudioLayoutFlowRecords,
    pub styles: StudioTypeStyleRecords,
    pub parents: StudioParentPageRecords,
    pub longdoc: StudioLongDocRecords,
}

fn diag(code: Code, target: &str) -> Diagnostic {
    Diagnostic { code, target: format!("{LAYOUT_RECORDS_ENTRY}/{target}") }
}

/// Bytes and nesting before any parse (the Folio input budget).
fn preflight(input: &[u8], b: Budget, cancel: &CancellationToken) -> Result<(), Diagnostic> {
    if input.len() > b.input_bytes {
        return Err(diag(Code::BudgetExceeded, "input"));
    }
    let (mut depth, mut quoted, mut escape) = (0usize, false, false);
    for (i, byte) in input.iter().enumerate() {
        if i & 0xFFFF == 0 && cancel.check().is_err() {
            return Err(diag(Code::Canceled, "input"));
        }
        if quoted {
            if escape {
                escape = false;
            } else if *byte == b'\\' {
                escape = true;
            } else if *byte == b'"' {
                quoted = false;
            }
            continue;
        }
        match byte {
            b'"' => quoted = true,
            b'{' | b'[' => {
                depth = depth.saturating_add(1);
                if depth > b.depth {
                    return Err(diag(Code::BudgetExceeded, "input_depth"));
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

impl LayoutRecords {
    /// Empty record sets for `document_id`.
    pub fn new(document_id: &str) -> Self {
        Self {
            document_id: document_id.to_owned(),
            stories: Vec::new(),
            flow: StudioLayoutFlowRecords::new(document_id.to_owned()),
            styles: StudioTypeStyleRecords::new(document_id),
            parents: StudioParentPageRecords::new(document_id),
            longdoc: StudioLongDocRecords::new(document_id),
        }
    }

    /// The entry bytes (deterministic: serde_json of the Folio types in field order).
    pub fn to_bytes(&self) -> Result<Vec<u8>, SessionError> {
        let value = json!({
            "schema_id": LAYOUT_RECORDS_SCHEMA, "document_id": self.document_id, "stories": self.stories,
            "flow": self.flow, "styles": self.styles, "parents": self.parents, "longdoc": self.longdoc,
        });
        serde_json::to_vec(&value).map_err(|_| SessionError::Provider("invalid_input"))
    }

    /// The shape of an entry, bounded, without checking it against a document.
    pub fn decode(input: &[u8], b: Budget, cancel: &CancellationToken) -> Result<Self, Diagnostic> {
        preflight(input, b, cancel)?;
        let value: Value = serde_json::from_slice(input).map_err(|_| diag(Code::InvalidJson, "input"))?;
        let Value::Object(mut object) = value else {
            return Err(diag(Code::InvalidJson, "input"));
        };
        if let Some(unknown) = object.keys().find(|k| !FIELDS.contains(&k.as_str())) {
            return Err(diag(Code::UnknownField, unknown));
        }
        if object.get("schema_id").and_then(Value::as_str) != Some(LAYOUT_RECORDS_SCHEMA) {
            return Err(diag(Code::InvalidJson, "schema_id"));
        }
        if object.get("stories").and_then(Value::as_array).is_some_and(|s| s.len() > MAX_LAYOUT_STORIES) {
            return Err(diag(Code::BudgetExceeded, "stories"));
        }
        macro_rules! take {
            ($key:literal) => {
                serde_json::from_value(object.remove($key).ok_or_else(|| diag(Code::MissingField, $key))?).map_err(|_| diag(Code::InvalidJson, $key))?
            };
        }
        Ok(Self {
            document_id: take!("document_id"),
            stories: take!("stories"),
            flow: take!("flow"),
            styles: take!("styles"),
            parents: take!("parents"),
            longdoc: take!("longdoc"),
        })
    }

    /// Validates every member against `document` (Folio's validators; a style reference the styles do
    /// not hold is `DanglingReference`).
    pub fn validate(&self, document: &StudioDocument, b: Budget, cancel: &CancellationToken) -> Result<(), Diagnostic> {
        if self.document_id != document.document_id {
            return Err(diag(Code::InvalidField, "document_id"));
        }
        if self.stories.len() > MAX_LAYOUT_STORIES {
            return Err(diag(Code::BudgetExceeded, "stories"));
        }
        folio::validate_type_style_records(&self.styles, document, b, cancel, &folio::Unresolved)?;
        let styles = StylesOnly(&self.styles);
        for (i, story) in self.stories.iter().enumerate() {
            if self.stories[..i].iter().any(|s| s.story_id == story.story_id) {
                return Err(diag(Code::DuplicateIdentity, &story.story_id));
            }
            // An empty story's one empty paragraph (and range) is Folio's own form since lane
            // SMB-TYPE-CHARACTER-ATTRIBUTES-1 (lead decision (a)); no workaround here.
            let issues = folio::validate_text_story(story, b, cancel, &styles)?;
            if !issues.is_empty() {
                return Err(diag(Code::DanglingReference, &story.story_id));
            }
            folio::check_story_style_scopes(story, &self.styles)?;
        }
        // Local character overrides lie on their story (lane SMB-TYPE-CHARACTER-ATTRIBUTES-1, review F9): a
        // stored story, inside its bytes, on character boundaries of inline text.
        let by_id: std::collections::BTreeMap<&str, &StudioTextStory> = self.stories.iter().map(|s| (s.story_id.as_str(), s)).collect();
        for o in &self.styles.character_overrides {
            cancel.check().map_err(|_| diag(Code::Canceled, "character_overrides"))?;
            let target = format!("character_overrides/{}/{}", o.text_id, o.start);
            let story = by_id.get(o.text_id.as_str()).ok_or_else(|| diag(Code::DanglingReference, &target))?;
            let boundary = |at: u64| story.inline_text().is_none_or(|t| usize::try_from(at).is_ok_and(|i| t.is_char_boundary(i)));
            if o.end > story.byte_len() || !boundary(o.start) || !boundary(o.end) {
                return Err(diag(Code::InvalidField, &target));
            }
        }
        folio::validate_layout_flow_records(&self.flow, document, cancel)?;
        folio::validate_parent_page_records(&self.parents, document, b, cancel, &folio::Unresolved)?;
        folio::validate_longdoc_records(&self.longdoc, document, b, cancel, &folio::Unresolved)?;
        Ok(())
    }

    /// [`LayoutRecords::decode`] then [`LayoutRecords::validate`].
    pub fn parse(input: &[u8], document: &StudioDocument, b: Budget, cancel: &CancellationToken) -> Result<Self, Diagnostic> {
        let records = Self::decode(input, b, cancel)?;
        records.validate(document, b, cancel)?;
        Ok(records)
    }
}

/// Resolves a type style id the records hold, nothing else.
struct StylesOnly<'a>(&'a StudioTypeStyleRecords);
impl Resolver for StylesOnly<'_> {
    fn primitive(&self, schema_id: &str, object_id: &str) -> folio::Resolution {
        if schema_id == SCHEMA_STUDIO_TYPE_STYLE && self.0.style(object_id).is_some() { folio::Resolution::Available } else { folio::Resolution::Absent }
    }
    fn profile(&self, _: &str) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn tile(&self, _: &folio::TileRef) -> folio::Resolution {
        folio::Resolution::Absent
    }
    fn format_supported(&self, _: &str) -> bool {
        false
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
}

/// A layout step (module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutStep {
    pub command_id: String,
    pub correlation_id: u64,
    /// The live document (its `revision` is not compared).
    pub expect: Box<StudioDocument>,
    /// The document after the step (its `revision` is set by the session).
    pub write: Box<StudioDocument>,
    /// The live records (`None` = no entry).
    pub records_expect: Option<LayoutRecords>,
    /// The records after the step (`None` = the entry is removed).
    pub records_write: Option<LayoutRecords>,
}

impl LayoutStep {
    /// The step that restores `expect`, under fresh caller identity.
    pub fn inverse(&self, command_id: String, correlation_id: u64) -> Self {
        Self {
            command_id,
            correlation_id,
            expect: self.write.clone(),
            write: self.expect.clone(),
            records_expect: self.records_write.clone(),
            records_write: self.records_expect.clone(),
        }
    }

    /// Deterministic bytes of the request for the idempotency binding and admission cost.
    pub(crate) fn binding_bytes(&self) -> Result<Vec<u8>, SessionError> {
        let invalid = |_| SessionError::Provider("invalid_input");
        let mut out = format!("hsk-studio-layout-step-1\0{}\0{}\0", self.command_id, self.correlation_id).into_bytes();
        for document in [&self.expect, &self.write] {
            out.extend_from_slice(&serde_json::to_vec(&**document).map_err(invalid)?);
            out.push(0);
        }
        for records in [&self.records_expect, &self.records_write] {
            match records {
                Some(r) => out.extend_from_slice(&r.to_bytes()?),
                None => out.extend_from_slice(b"null"),
            }
            out.push(0);
        }
        Ok(out)
    }
}

/// `a` and `b` are the same document, revisions aside.
fn same_state(a: &StudioDocument, b: &StudioDocument) -> bool {
    if a.revision == b.revision {
        return a == b;
    }
    let mut b = b.clone();
    b.revision = a.revision;
    *a == b
}

/// What a checked step publishes.
pub(crate) struct Checked {
    pub successor: Snapshot,
    /// The entry body (`None` = remove it).
    pub entry: Option<Vec<u8>>,
    pub inverse: LayoutStep,
}

/// The checked successor of `snapshot` with the step applied at `revision + 1`, or `None` when the step
/// writes the state the session already holds. Nothing is published here.
pub(crate) fn successor(
    snapshot: &Snapshot,
    held: &[Quarantined],
    step: &LayoutStep,
    budget: Budget,
    resolver: &dyn Resolver,
    cancel: &CancellationToken,
) -> Result<Option<Checked>, SessionError> {
    if !valid_command_id(&step.command_id) {
        return Err(SessionError::Provider("invalid_command"));
    }
    let document = snapshot.document();
    if step.expect.document_id != document.document_id || step.write.document_id != document.document_id {
        return Err(SessionError::Provider("invalid_input"));
    }
    if !same_state(document, &step.expect) {
        return Err(SessionError::Provider("value_conflict"));
    }
    let live = match held_body(held, LAYOUT_RECORDS_ENTRY) {
        Err(_) => return Err(SessionError::Provider("value_conflict")),
        Ok(None) => None,
        Ok(Some(bytes)) => match LayoutRecords::decode(bytes, budget, cancel) {
            Ok(r) => Some(r),
            Err(d) if d.code == Code::Canceled => return Err(SessionError::Canceled),
            Err(_) => return Err(SessionError::Provider("value_conflict")),
        },
    };
    if live != step.records_expect {
        return Err(SessionError::Provider("value_conflict"));
    }
    if same_state(document, &step.write) && step.records_write == step.records_expect {
        return Ok(None);
    }
    let mut next = (*step.write).clone();
    next.revision = document.revision.checked_add(1).ok_or(SessionError::Provider("revision_overflow"))?;
    let successor = folio::validate_document(next, budget, cancel, resolver).map_err(crate::sequence_records_step::folio_code)?;
    let entry = match &step.records_write {
        None => None,
        Some(r) => {
            r.validate(successor.document(), budget, cancel).map_err(|d| match d.code {
                Code::Canceled => SessionError::Canceled,
                Code::BudgetExceeded => SessionError::Provider("budget_exceeded"),
                _ => SessionError::Provider("layout_records_rejected"),
            })?;
            // Every read bounds the entry by the Folio input budget before parsing; a write may not
            // make one no read takes (review F3 of b21616b7).
            let bytes = r.to_bytes()?;
            preflight(&bytes, budget, cancel).map_err(|d| if d.code == Code::Canceled { SessionError::Canceled } else { SessionError::Provider("budget_exceeded") })?;
            Some(bytes)
        }
    };
    let mut inverse = step.inverse(step.command_id.clone(), step.correlation_id);
    inverse.expect = Box::new(successor.document().clone());
    Ok(Some(Checked { successor, entry, inverse }))
}

impl Session {
    /// The stored layout records, validated against the document (`None` = none stored). An entry that
    /// does not validate is the Folio diagnostic, never partial records.
    pub fn layout_records(&self, cancel: &CancellationToken) -> Result<Option<LayoutRecords>, SessionError> {
        cancel.check().map_err(|_| SessionError::Canceled)?;
        let Some(bytes) = held_body(self.held_entries(), LAYOUT_RECORDS_ENTRY).map_err(folio_err)? else {
            return Ok(None);
        };
        LayoutRecords::parse(bytes, self.snapshot().document(), self.folio_budget(), cancel).map(Some).map_err(folio_err)
    }

    /// The stored layout records as a [`LayoutStep`] quotes them: decoded and bounded, not checked
    /// against the document (module docs). `None` = none stored; a repeated or undecodable entry is the
    /// Folio diagnostic.
    pub fn layout_records_shape(&self, cancel: &CancellationToken) -> Result<Option<LayoutRecords>, SessionError> {
        cancel.check().map_err(|_| SessionError::Canceled)?;
        let Some(bytes) = held_body(self.held_entries(), LAYOUT_RECORDS_ENTRY).map_err(folio_err)? else {
            return Ok(None);
        };
        LayoutRecords::decode(bytes, self.folio_budget(), cancel).map(Some).map_err(folio_err)
    }
}

/// Parses and validates an entry body (`crate::sequence_records::check_entry`).
pub(crate) fn check(bytes: &[u8], document: &StudioDocument, budget: Budget, cancel: &CancellationToken) -> Result<(), SessionError> {
    LayoutRecords::parse(bytes, document, budget, cancel).map(drop).map_err(folio_err)
}
