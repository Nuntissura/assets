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
use crate::error::SessionError;
use crate::session::Session;
use crate::source::DOCUMENT_ENTRY_PREFIX;
use hsk_studio_accord::CancellationToken;
use crate::sequence_records::{folio_err, held_body};
use hsk_studio_folio::{ABSENT_COMPOSITION_TOKEN, CURRENT_COMPOSITION_TOKEN, STUDIO_COMPOSITION_TOKENS, StudioCompositionTokenRecord, composition_token_rank, parse_composition_token_record};
use hsk_studio_package::{Quarantined, sha256_hex_of};

/// Document-entry token of the composition token record. (`composition` is the motion composition.)
pub const COMPOSITION_TOKEN_ENTRY: &str = "composition_token";
/// The note of a document with no stored token.
pub const COMPOSITION_TOKEN_ABSENT: &str = "composition_token_absent";
/// The refusal of a well-formed token this build cannot compose under.
pub const COMPOSITION_TOKEN_UNKNOWN: &str = "composition_token_unknown";

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
