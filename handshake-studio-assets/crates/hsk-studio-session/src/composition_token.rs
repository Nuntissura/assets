//! The composition token of a stored document in the session (lane SMB-TYPE-AUTHORED-TOKEN-1,
//! STU-TYP-130: a stored document recomposes under the token it was authored with unless it is
//! explicitly upgraded). The Folio `StudioCompositionTokenRecord` is the document entry
//! `studio/composition_token.<sha256>`, one per document, opened, read, set and saved as the sequence
//! settings records are (`crate::sequence_records`): parsed and validated on open, on every set and on
//! every read, and bounded by `limits.max_entry_bytes` with the other entries.
//!
//! A package with no entry opens unchanged and reads as Folio's `ABSENT_COMPOSITION_TOKEN` with the
//! typed note [`COMPOSITION_TOKEN_ABSENT`] on [`Session::authored_composition`], not a loss note. A
//! save never writes the entry on its own (tests/composition_token.rs
//! `saving_a_document_with_no_token_writes_no_entry`): a path that creates a new document stores
//! [`new_document_composition_entry`]. A well-formed token this build does not know, and an entry that
//! does not validate, open with a `LossReason::Opaque` note, refuse on read and keep their bytes
//! through a save: [`COMPOSITION_TOKEN_UNKNOWN`] for the first, the Folio diagnostic for the second,
//! never the newest token.
//!
//! The upgrade is one [`CompositionTokenStep`] (`Session::submit_composition_token`, `_keyed`, `_as`), the
//! commit shape of `crate::sequence_records_step`: one revision, one receipt, one inverse, one Observe
//! frame. It is recorded in the entry, so it survives a save and reopen, and its diff is the entry's
//! [`StudioCompositionUpgrade`]: from-token, to-token and the stories it affects. That is the diff this
//! layer can state: the session does not compose, so a glyph-level comparison belongs to a host (Studio
//! reading of "diffable"; the clause does not define it). Tests: tests/composition_token_upgrade.rs.
use crate::error::SessionError;
use crate::session::Session;
use crate::source::DOCUMENT_ENTRY_PREFIX;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{self as folio, Resolver, Snapshot, StudioCompositionUpgrade};
use crate::sequence_records::{folio_err, held_body};
use crate::sequence_records_step::folio_code;
use crate::timeline::valid_command_id;
use hsk_studio_folio::{
    ABSENT_COMPOSITION_TOKEN, CURRENT_COMPOSITION_TOKEN, STUDIO_COMPOSITION_TOKENS, StudioCompositionTokenRecord, StudioDocument,
    composition_affected_stories, composition_token_rank, parse_composition_token_record, validate_composition_token_record,
};
use hsk_studio_package::{Quarantined, sha256_hex_of};
use std::cmp::Ordering;

/// Document-entry token of the composition token record. (`composition` is the motion composition.)
pub const COMPOSITION_TOKEN_ENTRY: &str = "composition_token";
/// The note of a document with no stored token.
pub const COMPOSITION_TOKEN_ABSENT: &str = "composition_token_absent";
/// The refusal of a well-formed token this build cannot compose under.
pub const COMPOSITION_TOKEN_UNKNOWN: &str = "composition_token_unknown";

/// The refusal of a move to an older token.
pub const COMPOSITION_TOKEN_OLDER: &str = "composition_token_older";
/// The refusal of a [`CompositionTokenChange::Restore`] that is neither the undo nor the redo of a
/// recorded upgrade and does not move to an older token.
pub const COMPOSITION_TOKEN_UNRECORDED: &str = "composition_token_unrecorded";
/// Provider name recorded in [`crate::Applied::provider`] and in the idempotency binding.
pub const COMPOSITION_TOKEN_STEP_PROVIDER: &str = "composition.token";

/// What a [`CompositionTokenStep`] changes.
#[derive(Clone, Debug, PartialEq)]
pub enum CompositionTokenChange {
    /// The explicit upgrade: `from` quotes the stored token (`None` = no entry), `to` is the target.
    /// The session lists the stories and writes the record.
    Upgrade { from: Option<String>, to: String },
    /// The undo or redo of a recorded upgrade: `expect` quotes the live record (`None` = no entry),
    /// `write` is the record after the step (`None` = the entry is removed). The inverse a commit
    /// returns is one of these.
    Restore { expect: Option<StudioCompositionTokenRecord>, write: Option<StudioCompositionTokenRecord> },
}

/// One token change = one undo step.
#[derive(Clone, Debug, PartialEq)]
pub struct CompositionTokenStep {
    pub command_id: String,
    pub correlation_id: u64,
    pub change: CompositionTokenChange,
}

impl CompositionTokenStep {
    /// Deterministic bytes of the request for the idempotency binding and admission cost.
    pub(crate) fn binding_bytes(&self) -> Result<Vec<u8>, SessionError> {
        let fields = match &self.change {
            CompositionTokenChange::Upgrade { from, to } => serde_json::to_vec(&("upgrade", from, to)),
            CompositionTokenChange::Restore { expect, write } => serde_json::to_vec(&("restore", expect, write)),
        }
        .map_err(|_| SessionError::Provider("invalid_input"))?;
        let mut out = format!("hsk-studio-composition-token-step-1\0{}\0{}\0", self.command_id, self.correlation_id).into_bytes();
        out.extend_from_slice(&fields);
        Ok(out)
    }
}

/// The checked outcome of a step: the successor document (`revision + 1`), the entry body (`None` =
/// remove), the inverse step and the diff.
pub(crate) struct Checked {
    pub successor: Snapshot,
    pub body: Option<Vec<u8>>,
    pub inverse: CompositionTokenStep,
    pub diff: StudioCompositionUpgrade,
}

/// The live record: a token this build does not know is its refusal, any other entry that does not read
/// cannot be quoted (`value_conflict`, the rule of the sequence entries).
fn held_record(
    live: Result<Option<&[u8]>, folio::Diagnostic>,
    document: &StudioDocument,
    budget: folio::Budget,
    cancel: &CancellationToken,
) -> Result<Option<StudioCompositionTokenRecord>, SessionError> {
    let conflict = SessionError::Provider("value_conflict");
    let Some(bytes) = live.map_err(|_| conflict.clone())? else {
        return Ok(None);
    };
    match parse_composition_token_record(bytes, document, budget, cancel) {
        Ok(record) => Ok(Some(record)),
        Err(d) => match d.code {
            folio::Code::Canceled => Err(SessionError::Canceled),
            folio::Code::UnknownCompositionToken => Err(SessionError::Provider(COMPOSITION_TOKEN_UNKNOWN)),
            _ => Err(conflict),
        },
    }
}

/// The token a record reads as (`None` = no entry).
fn reads_as(record: Option<&StudioCompositionTokenRecord>) -> &str {
    record.map_or(ABSENT_COMPOSITION_TOKEN, |r| r.composition_version.as_str())
}

/// The record's upgrades (`None` = no entry, none).
fn upgrades_of(record: Option<&StudioCompositionTokenRecord>) -> &[StudioCompositionUpgrade] {
    record.map_or(&[], |r| r.upgrades.as_slice())
}

/// The upgrade `longer` records on top of `shorter`, when `longer` is `shorter` plus exactly that upgrade.
fn recorded_on_top<'a>(shorter: Option<&StudioCompositionTokenRecord>, longer: Option<&'a StudioCompositionTokenRecord>) -> Option<&'a StudioCompositionUpgrade> {
    let (last, before) = longer?.upgrades.split_last()?;
    (before == upgrades_of(shorter) && last.from_token == reads_as(shorter) && last.to_token == reads_as(longer)).then_some(last)
}

/// A restore is the undo (`write` is `expect` less its last upgrade) or the redo (`write` is `expect` plus
/// one upgrade) of a recorded upgrade, or it is refused, so no step can move a document to an older token
/// on its own: the undo of an upgrade is the one way back, and it needs that upgrade on record.
fn restore_diff(expect: Option<&StudioCompositionTokenRecord>, write: Option<&StudioCompositionTokenRecord>) -> Result<StudioCompositionUpgrade, SessionError> {
    if let Some(undone) = recorded_on_top(write, expect) {
        return Ok(StudioCompositionUpgrade { from_token: undone.to_token.clone(), to_token: undone.from_token.clone(), story_ids: undone.story_ids.clone() });
    }
    if let Some(redone) = recorded_on_top(expect, write) {
        return Ok(redone.clone());
    }
    let older = composition_token_rank(reads_as(write)) < composition_token_rank(reads_as(expect));
    Err(SessionError::Provider(if older { COMPOSITION_TOKEN_OLDER } else { COMPOSITION_TOKEN_UNRECORDED }))
}

/// Checks `step` against the live document and entry; `Ok(None)` when it changes nothing. Nothing is
/// published here. The body it would store is parsed back through the check an open applies, so a record
/// a reopen would refuse is refused here.
pub(crate) fn successor(
    snapshot: &Snapshot,
    live: Result<Option<&[u8]>, folio::Diagnostic>,
    text_entry: Option<&[u8]>,
    step: &CompositionTokenStep,
    budget: folio::Budget,
    resolver: &dyn Resolver,
    cancel: &CancellationToken,
) -> Result<Option<Checked>, SessionError> {
    if !valid_command_id(&step.command_id) {
        return Err(SessionError::Provider("invalid_command"));
    }
    let document = snapshot.document();
    let held = held_record(live, document, budget, cancel)?;
    let (write, diff) = match &step.change {
        CompositionTokenChange::Upgrade { from, to } => {
            if from.as_deref() != held.as_ref().map(|r| r.composition_version.as_str()) {
                return Err(SessionError::Provider("value_conflict"));
            }
            let target = composition_token_rank(to).ok_or(SessionError::Provider(COMPOSITION_TOKEN_UNKNOWN))?;
            let start = composition_token_rank(reads_as(held.as_ref())).ok_or(SessionError::Provider(COMPOSITION_TOKEN_UNKNOWN))?;
            match target.cmp(&start) {
                Ordering::Less => return Err(SessionError::Provider(COMPOSITION_TOKEN_OLDER)),
                Ordering::Equal => return Ok(None),
                Ordering::Greater => {}
            }
            let story_ids = composition_affected_stories(document, text_entry, budget, cancel).map_err(folio_code)?;
            let diff = StudioCompositionUpgrade { from_token: reads_as(held.as_ref()).to_owned(), to_token: to.clone(), story_ids };
            let mut record = held.clone().unwrap_or_else(|| StudioCompositionTokenRecord::new(document.document_id.clone(), ABSENT_COMPOSITION_TOKEN));
            to.clone_into(&mut record.composition_version);
            record.upgrades.push(diff.clone());
            (Some(record), diff)
        }
        CompositionTokenChange::Restore { expect, write } => {
            if expect != &held {
                return Err(SessionError::Provider("value_conflict"));
            }
            if write == &held {
                return Ok(None);
            }
            if let Some(record) = write {
                validate_composition_token_record(record, document, budget, cancel).map_err(checked_code)?;
            }
            (write.clone(), restore_diff(held.as_ref(), write.as_ref())?)
        }
    };
    let body = match &write {
        None => None,
        Some(record) => {
            let body = serde_json::to_vec(record).map_err(|_| SessionError::Provider("invalid_input"))?;
            parse_composition_token_record(&body, document, budget, cancel).map_err(checked_code)?;
            Some(body)
        }
    };
    let mut next = document.clone();
    next.revision = document.revision.checked_add(1).ok_or(SessionError::Provider("revision_overflow"))?;
    let successor = folio::validate_document(next, budget, cancel, resolver).map_err(folio_code)?;
    let inverse = CompositionTokenStep {
        command_id: step.command_id.clone(),
        correlation_id: step.correlation_id,
        change: CompositionTokenChange::Restore { expect: write, write: held },
    };
    Ok(Some(Checked { successor, body, inverse, diff }))
}

/// A record the step would store: a token this build does not know keeps its code.
fn checked_code(d: folio::Diagnostic) -> SessionError {
    match d.code {
        folio::Code::UnknownCompositionToken => SessionError::Provider(COMPOSITION_TOKEN_UNKNOWN),
        _ => folio_code(d),
    }
}

/// What a document composes under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthoredComposition {
    /// The token a host passes to Type.
    pub token: &'static str,
    /// False when the document stores no token.
    pub stored: bool,
    /// [`COMPOSITION_TOKEN_ABSENT`] when the document stores no token.
    pub note: Option<&'static str>,
}

/// The entry a path that creates a new document holds: the current token, no upgrades.
pub fn new_document_composition_entry(document_id: &str) -> Result<Quarantined, SessionError> {
    let record = StudioCompositionTokenRecord::new(document_id, CURRENT_COMPOSITION_TOKEN);
    let bytes = serde_json::to_vec(&record).map_err(|_| SessionError::Provider("invalid_input"))?;
    let sha256_hex = sha256_hex_of(&bytes);
    Ok(Quarantined { name: format!("{DOCUMENT_ENTRY_PREFIX}{COMPOSITION_TOKEN_ENTRY}.{sha256_hex}"), bytes, sha256_hex })
}

impl Session {
    /// The stored composition token record, validated against the document (`None` = none stored). An
    /// entry that does not validate is its refusal, never a partial record (module docs).
    pub fn composition_token_record(&self, cancel: &CancellationToken) -> Result<Option<StudioCompositionTokenRecord>, SessionError> {
        cancel.check().map_err(|_| SessionError::Canceled)?;
        let Some(bytes) = held_body(self.held_entries(), COMPOSITION_TOKEN_ENTRY).map_err(folio_err)? else {
            return Ok(None);
        };
        parse_composition_token_record(bytes, self.snapshot().document(), self.folio_budget(), cancel).map(Some).map_err(folio_err)
    }

    /// What the document composes under: its stored token, or the absent reading with its typed note.
    /// Read-only: it creates no entry and sends no frame, as the other typed reads.
    pub fn authored_composition(&self, cancel: &CancellationToken) -> Result<AuthoredComposition, SessionError> {
        let Some(record) = self.composition_token_record(cancel)? else {
            return Ok(AuthoredComposition { token: ABSENT_COMPOSITION_TOKEN, stored: false, note: Some(COMPOSITION_TOKEN_ABSENT) });
        };
        let rank = composition_token_rank(&record.composition_version).ok_or(SessionError::Provider(COMPOSITION_TOKEN_UNKNOWN))?;
        let token = *STUDIO_COMPOSITION_TOKENS.get(rank).ok_or(SessionError::Provider(COMPOSITION_TOKEN_UNKNOWN))?;
        Ok(AuthoredComposition { token, stored: true, note: None })
    }

    /// Stores (`Some`, validated first) or removes (`None`) the record, replacing whatever entry is
    /// held, an invalid one included. Not a revision and not undoable, like every direct document-entry
    /// write: an upgrade goes through the recorded operation.
    pub fn set_composition_token_record(&mut self, record: Option<&StudioCompositionTokenRecord>, cancel: &CancellationToken) -> Result<(), SessionError> {
        let bytes = match record {
            None => None,
            Some(r) => Some(serde_json::to_vec(r).map_err(|_| SessionError::Provider("invalid_input"))?),
        };
        self.set_document_entry(COMPOSITION_TOKEN_ENTRY, bytes, cancel)
    }
}
