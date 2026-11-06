//! Markers of a media item, the third marker scope (lane SMB-PR-MEDIA-MARKERS-1; STU-VID-034: "A
//! PROJECT-ITEM marker lives on a media item and appears on every clip cut from it").
//!
//! [`StudioMediaMarkerRecords`] (`hsk.studio.media_markers@1`) holds [`StudioMarker`]s beside the
//! document, one entry per media item, each marker in that item's SOURCE ticks, the time base of a clip
//! marker. Pulse answers where a marker appears; this module stores and validates only.
//!
//! The key is the clip's [`ContentDigest`], a Studio reading: the digest identifies the bytes, so two
//! clips of one file are one media item whichever handle they came through. The manifest handle
//! (`artifact_manifest_id`) is caller-owned and may differ between clips of the same bytes, and
//! `asset_id` is optional, so neither can key the entry. Digests compare exactly, as Folio stores them.
//! The entry names no sequence and no clip and copies no document value (test
//! `the_records_name_no_sequence_or_clip_and_copy_no_document_value`), so a timeline edit cannot make it
//! stale and removing the last clip cut from an item leaves it valid. An item with no marker has no
//! entry; an entry with an empty list is refused, so one state has one form.
//!
//! `StudioMarker` says its id is unique among the document's markers. `validate_document` checks that
//! for the sequence and clip scopes over one id set; [`validate_media_marker_records`] extends it to
//! this scope, against the document it is given. A marker is checked by `validate_marker` in `timeline.rs`,
//! the document's own function, over this set's ids; test `marker_fields_are_refused_as_the_document_refuses_them`
//! pins the answers.
//!
//! [`StudioMediaMarkerCommand`] adds, replaces and removes one marker. Each takes the caller's
//! `CommandContext` and returns the next records and a report of codes and counters; it holds no
//! document it could change, so the revision and the validation of a commit stay with the caller (R55
//! F-R55-7). The declared bounds and command ids are Studio's: the clause states none. Vendor parity is
//! UNVERIFIED (D5).
//!
//! The records arrive from untrusted wire JSON: integer arithmetic is denied (workflow V4-01), every
//! list is counted before it is walked and charged against the [`Budget`], and a parse refuses bytes
//! over [`MAX_MEDIA_MARKER_INPUT_BYTES`] whatever the budget allows.
#![deny(clippy::arithmetic_side_effects)]
use super::{
    Budget, Code, ContentDigest, Diagnostic, MAX_MARKERS_PER_SEQUENCE, Result, StudioDocument, StudioMarker, Unresolved, err, id,
    sequence_preset::parse_wire,
    timeline::{control_free, validate_marker},
    values::Ctx,
};
use hsk_studio_accord::{CancellationToken, CommandContext};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

schema_token!(MediaMarkersSchema, "hsk.studio.media_markers@1");

/// Media items one record set may hold.
pub const MAX_MEDIA_MARKER_ITEMS: usize = 4096;
/// Markers one record set may hold over all its media items, the bound on one sequence's markers.
pub const MAX_MEDIA_MARKERS: usize = MAX_MARKERS_PER_SEQUENCE;
/// Largest wire form of a record set in bytes, whatever the caller's budget allows.
pub const MAX_MEDIA_MARKER_INPUT_BYTES: usize = 16_777_216;

const CANCEL_STRIDE: usize = 256;

/// The markers of one media item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioMediaMarkerEntry {
    /// The bytes of the media item, a clip's `ClipSource::Media::content_digest`.
    pub content_digest: ContentDigest,
    /// In the item's SOURCE ticks; at least one, each id unique among the document's markers.
    pub markers: Vec<StudioMarker>,
}

/// The per-document media item marker record set, one per `document_id`, beside the document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioMediaMarkerRecords {
    pub schema_id: MediaMarkersSchema,
    /// `SDOC` id of the annotated document.
    pub document_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media_items: Vec<StudioMediaMarkerEntry>,
}
impl StudioMediaMarkerRecords {
    /// An empty set for `document_id`.
    pub fn new(document_id: impl Into<String>) -> Self {
        Self { schema_id: MediaMarkersSchema::V1, document_id: document_id.into(), media_items: Vec::new() }
    }
    /// The entry of the media item with these bytes.
    pub fn entry(&self, content_digest: &ContentDigest) -> Option<&StudioMediaMarkerEntry> {
        self.media_items.iter().find(|e| &e.content_digest == content_digest)
    }
    /// Markers over all media items.
    pub fn marker_count(&self) -> usize {
        self.media_items.iter().fold(0usize, |n, e| n.saturating_add(e.markers.len()))
    }
}

fn digest_ok(digest: &ContentDigest) -> bool {
    control_free(&digest.algorithm) && control_free(&digest.digest)
}

/// Validates the record set against the document: `SDOC` id equal to the document's; at most
/// `MAX_MEDIA_MARKER_ITEMS` items and `MAX_MEDIA_MARKERS` markers (counted before they are walked, then
/// charged against `Budget::nodes`); each item's digest parts free of control characters and at most
/// `MAX_HANDLE_BYTES` (`InvalidField`, charged to `Budget::payload_bytes`), once (`DuplicateIdentity`),
/// with at least one marker (`InvalidField`); marker fields as the document's markers; marker ids of the
/// key grammar (`InvalidId`), unique over this set and the document's sequence and clip markers
/// (`DuplicateIdentity`).
pub fn validate_media_marker_records(r: &StudioMediaMarkerRecords, d: &StudioDocument, b: Budget, token: &CancellationToken) -> Result<()> {
    let mut cx = Ctx::new(b, token, &Unresolved);
    cx.tick()?;
    id(&r.document_id, "SDOC", "document_id")?;
    if r.document_id != d.document_id {
        return Err(err(Code::InvalidField, "document_id"));
    }
    if r.media_items.len() > MAX_MEDIA_MARKER_ITEMS || r.marker_count() > MAX_MEDIA_MARKERS {
        return Err(err(Code::BudgetExceeded, "media_markers"));
    }
    cx.nodes(r.media_items.len().saturating_add(r.marker_count()), "media_markers")?;
    let (mut items, mut ids) = (BTreeSet::new(), BTreeSet::new());
    // Digest and marker text share one count against `Budget::payload_bytes`.
    let mut payload = 0usize;
    for e in &r.media_items {
        cx.tick()?;
        let digest = &e.content_digest;
        if !digest_ok(digest) {
            return Err(err(Code::InvalidField, "media_markers/content_digest"));
        }
        payload = payload
            .checked_add(digest.algorithm.len())
            .and_then(|n| n.checked_add(digest.digest.len()))
            .filter(|n| *n <= b.payload_bytes)
            .ok_or_else(|| err(Code::BudgetExceeded, "media_markers/content_digest"))?;
        if !items.insert((digest.algorithm.as_str(), digest.digest.as_str())) {
            return Err(err(Code::DuplicateIdentity, "media_markers/content_digest"));
        }
        if e.markers.is_empty() {
            return Err(err(Code::InvalidField, "media_markers/markers"));
        }
        for (i, m) in e.markers.iter().enumerate() {
            if i.is_multiple_of(CANCEL_STRIDE) {
                cx.tick()?;
            }
            validate_marker(m, &mut ids, &mut payload, b)?;
        }
    }
    if ids.is_empty() {
        return Ok(());
    }
    for s in &d.sequences {
        cx.tick()?;
        let clip_markers = s.tracks.iter().flat_map(|t| t.clips.iter()).flat_map(|c| c.markers.iter());
        for (i, m) in s.markers.iter().chain(clip_markers).enumerate() {
            if i.is_multiple_of(CANCEL_STRIDE) {
                cx.tick()?;
            }
            if ids.contains(m.marker_id.as_str()) {
                return Err(err(Code::DuplicateIdentity, m.marker_id.as_str()));
            }
        }
    }
    Ok(())
}

/// JSON schema (draft 2020-12) of the media item marker record set.
pub fn media_marker_records_schema() -> schemars::Schema {
    schemars::generate::SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<StudioMediaMarkerRecords>()
}

/// Parses wire JSON (bounded by [`MAX_MEDIA_MARKER_INPUT_BYTES`], `Budget::input_bytes` and depth before
/// any parse; duplicate keys `DuplicateField`, unknown fields, a missing field and wrong types
/// `InvalidJson`) and validates it against the document.
pub fn parse_media_marker_records(input: &[u8], d: &StudioDocument, b: Budget, token: &CancellationToken) -> Result<StudioMediaMarkerRecords> {
    if input.len() > MAX_MEDIA_MARKER_INPUT_BYTES {
        return Err(err(Code::BudgetExceeded, "media_markers"));
    }
    let r: StudioMediaMarkerRecords = parse_wire(input, b, token, "media_markers")?;
    validate_media_marker_records(&r, d, b, token)?;
    Ok(r)
}

/// Command ids, in [`StudioMediaMarkerCommand`] variant order.
pub const MEDIA_MARKER_COMMAND_IDS: [&str; 3] = ["media_marker.add", "media_marker.replace", "media_marker.remove"];

/// A typed media item marker command (STU-VID-034). A media item gains an entry with its first marker
/// and loses it with its last.
#[derive(Clone, Debug, PartialEq)]
pub enum StudioMediaMarkerCommand {
    /// Adds `marker` to the media item with these bytes; its id must be new among the document's markers.
    Add { content_digest: ContentDigest, marker: StudioMarker },
    /// Replaces the marker of this media item with the same id.
    Replace { content_digest: ContentDigest, marker: StudioMarker },
    /// Removes one marker of this media item.
    Remove { content_digest: ContentDigest, marker_id: String },
}
impl StudioMediaMarkerCommand {
    /// The command id the caller's context must carry.
    pub const fn command_id(&self) -> &'static str {
        match self {
            Self::Add { .. } => MEDIA_MARKER_COMMAND_IDS[0],
            Self::Replace { .. } => MEDIA_MARKER_COMMAND_IDS[1],
            Self::Remove { .. } => MEDIA_MARKER_COMMAND_IDS[2],
        }
    }
}

/// A typed refusal; nothing changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StudioMediaMarkerError {
    /// The context carries no command id.
    CommandMissing,
    /// The context's command id is not this command's.
    CommandMismatch,
    StaleRevision { expected: u64, current: u64 },
    /// The held records fail their own validation; the Folio diagnostic code.
    InvalidInput(Code),
    InvalidDigest,
    /// The new marker fails the marker rules; the Folio diagnostic code.
    InvalidMarker(Code),
    /// The new marker's id is held by a marker of any scope.
    DuplicateMarker,
    /// This media item holds no marker with that id.
    MarkerMissing,
    BudgetExceeded,
    Canceled,
}
impl StudioMediaMarkerError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::CommandMissing => "command_missing",
            Self::CommandMismatch => "command_mismatch",
            Self::StaleRevision { .. } => "stale_revision",
            Self::InvalidInput(_) => "invalid_input",
            Self::InvalidDigest => "invalid_digest",
            Self::InvalidMarker(_) => "invalid_marker",
            Self::DuplicateMarker => "duplicate_marker",
            Self::MarkerMissing => "marker_missing",
            Self::BudgetExceeded => "budget_exceeded",
            Self::Canceled => "canceled",
        }
    }
}
impl std::fmt::Display for StudioMediaMarkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for StudioMediaMarkerError {}

/// What a host emits through Observe for one applied command: codes, counters and the opaque
/// correlation only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StudioMediaMarkerReport {
    pub command: &'static str,
    pub base_revision: u64,
    pub correlation: u64,
    /// Media items of the resulting record set.
    pub media_items: usize,
    /// Markers of the resulting record set.
    pub markers: usize,
    /// Whether the records differ from the input.
    pub changed: bool,
}

/// An applied command: the resulting records and the report. The caller's input is the undo state.
#[derive(Clone, Debug, PartialEq)]
pub struct StudioMediaMarkerOutcome {
    pub records: StudioMediaMarkerRecords,
    pub report: StudioMediaMarkerReport,
}

/// Applies one command at `revision` (the caller's current document revision; the context's base
/// revision must equal it and its command id must be `cmd.command_id()`). The records are validated
/// first (`InvalidInput`), the command runs on a copy, and the result is validated again and must fit
/// what a parse under `b` accepts (`BudgetExceeded`). A refusal returns the typed error.
pub fn apply_media_marker_command(
    document: &StudioDocument,
    records: &StudioMediaMarkerRecords,
    ctx: &CommandContext,
    revision: u64,
    cmd: &StudioMediaMarkerCommand,
    b: Budget,
    token: &CancellationToken,
) -> std::result::Result<StudioMediaMarkerOutcome, StudioMediaMarkerError> {
    use StudioMediaMarkerCommand as C;
    use StudioMediaMarkerError as E;
    let c = ctx.command().ok_or(E::CommandMissing)?;
    if c.as_str() != cmd.command_id() {
        return Err(E::CommandMismatch);
    }
    if ctx.base_revision() != revision {
        return Err(E::StaleRevision { expected: ctx.base_revision(), current: revision });
    }
    let held = |d: Diagnostic| match d.code {
        Code::Canceled => E::Canceled,
        Code::BudgetExceeded => E::BudgetExceeded,
        code => E::InvalidInput(code),
    };
    validate_media_marker_records(records, document, b, token).map_err(held)?;
    let mut next = records.clone();
    match cmd {
        C::Add { content_digest, marker } => {
            if !digest_ok(content_digest) {
                return Err(E::InvalidDigest);
            }
            match next.media_items.iter_mut().find(|e| &e.content_digest == content_digest) {
                Some(e) => e.markers.push(marker.clone()),
                None => next.media_items.push(StudioMediaMarkerEntry { content_digest: content_digest.clone(), markers: vec![marker.clone()] }),
            }
        }
        C::Replace { content_digest, marker } => {
            let entry = next.media_items.iter_mut().find(|e| &e.content_digest == content_digest).ok_or(E::MarkerMissing)?;
            let slot = entry.markers.iter_mut().find(|m| m.marker_id == marker.marker_id).ok_or(E::MarkerMissing)?;
            slot.clone_from(marker);
        }
        C::Remove { content_digest, marker_id } => {
            let at = next.media_items.iter().position(|e| &e.content_digest == content_digest).ok_or(E::MarkerMissing)?;
            let entry = &mut next.media_items[at];
            let i = entry.markers.iter().position(|m| &m.marker_id == marker_id).ok_or(E::MarkerMissing)?;
            entry.markers.remove(i);
            if entry.markers.is_empty() {
                next.media_items.remove(at);
            }
        }
    }
    let changed = next != *records;
    if changed {
        validate_media_marker_records(&next, document, b, token).map_err(|d| match d.code {
            Code::Canceled => E::Canceled,
            Code::BudgetExceeded => E::BudgetExceeded,
            Code::DuplicateIdentity => E::DuplicateMarker,
            code => E::InvalidMarker(code),
        })?;
        let bytes = serde_json::to_vec(&next).map_err(|_| E::InvalidInput(Code::InvalidJson))?;
        if bytes.len() > MAX_MEDIA_MARKER_INPUT_BYTES.min(b.input_bytes) {
            return Err(E::BudgetExceeded);
        }
    }
    let report = StudioMediaMarkerReport {
        command: cmd.command_id(),
        base_revision: ctx.base_revision(),
        correlation: ctx.correlation().value(),
        media_items: next.media_items.len(),
        markers: next.marker_count(),
        changed,
    };
    Ok(StudioMediaMarkerOutcome { records: next, report })
}
