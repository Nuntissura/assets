//! Sequence settings and sequence presets in the session (lane SMB-PR-SEQUENCE-SESSION-1;
//! STU-VID-011, 016, 016a, 017, 018, 019): a saved document carries its Folio
//! `StudioSequenceSettingsRecords` and `StudioSequencePresetSet` as the typed document entries
//! `studio/sequence_settings.<sha256>` and `studio/sequence_presets.<sha256>` (the mechanism of
//! `crate::multicam`: written by every save and checkpoint, held again on reopen without a loss
//! note, bounded by `limits.max_entry_bytes` together with the other entries).
//!
//! The bytes are parsed and validated by Folio on open, on every set and on every read. One bad entry
//! does not lock the file out (lead decision 2026-11-05, refuse on read as multicam does): a package
//! with an entry that does not validate opens, the entry is reported as a `LossReason::Opaque` note
//! (`Session::loss`: preserved, not semantically imported), a read gives the Folio diagnostic
//! (two well-formed entries of one token: `DuplicateIdentity`, neither is chosen), a command or step
//! that would use it is refused and changes nothing, and a save keeps it byte-for-byte until a
//! successful commit or an explicit set replaces it. A name whose digest is not the body's is not a
//! document entry of the token: it stays an opaque entry with its note, as before this lane, and reads
//! as no records. Tests: tests/sequence_records_entry.rs, tests/sequence_records_round_trip.rs
//! `save_keeps_an_invalid_entry_byte_for_byte_until_a_commit_replaces_it`.
//!
//! A valid settings entry is also kept valid while the session runs: `Session::set_document_entry`
//! applies the same check to these tokens, a commit that changes a sequence may not make it invalid
//! (`folio_rejected`, so a timeline or multicam step cannot remove its sequence or change its frame
//! rate), and `resync_diverged` refuses an authority document it does not validate against
//! (tests/sequence_records_commit.rs). An entry already invalid does not hold those up; it stays as it
//! came. The preset entry names nothing in the document.
//!
//! The track display records (lane SMB-PR-TRACK-DISPLAY-1, STU-VID-036) are the entry
//! `studio/track_display.<sha256>`, opened, read, set and saved as above, with one difference (lead
//! decision on this lane): display state is a view preference and blocks no commit, so no guard holds
//! it. A commit that removes a sequence leaves the entry alone, and an entry naming a sequence the
//! document does not hold is inert, not invalid (`Session::inert_track_display_entries`). A sequence
//! with no entry resolves to the
//! declared defaults (`Session::resolved_track_displays`), so that read also says which sequences have
//! stored state. Tests: tests/track_display.rs.
//!
//! Bounds: the Folio input budget (`budget.input_bytes`, depth, duplicate keys) applies before any
//! parse, then the Folio record bounds; the package limits apply to the entry bytes on open and on
//! every write. Serialisation is serde_json of the Folio type (deterministic field order).
use crate::composition_token::{COMPOSITION_TOKEN_ENTRY, COMPOSITION_TOKEN_UNKNOWN};
use crate::error::SessionError;
use crate::session::Session;
use crate::source::{DOCUMENT_ENTRY_PREFIX, document_entry};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    Budget, Code, Diagnostic, StudioDocument, StudioResolvedTrackDisplay, StudioSequencePresetSet,
    StudioSequenceSettingsRecords, StudioTrackDisplayEntry, StudioTrackDisplayRecords, parse_composition_token_record, parse_sequence_presets, parse_sequence_settings_records,
    inert_track_display_entries, parse_track_display_records, resolve_track_displays,
};
use hsk_studio_package::{LossNote, LossReason, PackageError, Quarantined};

/// Document-entry token of the sequence settings records.
pub const SEQUENCE_SETTINGS_ENTRY: &str = "sequence_settings";
/// Document-entry token of the sequence preset library (the session's stored presets; the shipped
/// library is Folio data and is not copied here).
pub const SEQUENCE_PRESETS_ENTRY: &str = "sequence_presets";
/// Document-entry token of the track display records (STU-VID-036).
pub const TRACK_DISPLAY_ENTRY: &str = "track_display";

fn invalid_json(token: &str) -> SessionError {
    folio_err(Diagnostic { code: Code::InvalidJson, target: format!("{DOCUMENT_ENTRY_PREFIX}{token}") })
}

pub(crate) fn folio_err(d: Diagnostic) -> SessionError {
    match d.code {
        Code::Canceled => SessionError::Canceled,
        Code::UnknownCompositionToken => SessionError::Provider(COMPOSITION_TOKEN_UNKNOWN),
        _ => SessionError::Package(PackageError::Document(d)),
    }
}

/// True for the tokens this module validates.
pub(crate) fn validated_token(token: &str) -> bool {
    token == SEQUENCE_SETTINGS_ENTRY || token == SEQUENCE_PRESETS_ENTRY || token == TRACK_DISPLAY_ENTRY || token == COMPOSITION_TOKEN_ENTRY
}

/// Parses and validates the body of a [`validated_token`]; any other token passes (its bytes belong to
/// its producer).
pub(crate) fn check_entry(
    token: &str,
    bytes: &[u8],
    document: &StudioDocument,
    budget: Budget,
    cancel: &CancellationToken,
) -> Result<(), SessionError> {
    match token {
        SEQUENCE_SETTINGS_ENTRY => parse_sequence_settings_records(bytes, document, budget, cancel).map(drop).map_err(folio_err),
        SEQUENCE_PRESETS_ENTRY => parse_sequence_presets(bytes, budget, cancel).map(drop).map_err(folio_err),
        TRACK_DISPLAY_ENTRY => parse_track_display_records(bytes, document, budget, cancel).map(drop).map_err(folio_err),
        COMPOSITION_TOKEN_ENTRY => parse_composition_token_record(bytes, document, budget, cancel).map(drop).map_err(folio_err),
        _ => Ok(()),
    }
}

/// The one held body of `token`. Two well-formed entries of one token are `DuplicateIdentity`: neither
/// is chosen silently.
pub(crate) fn held_body<'a>(held: &'a [Quarantined], token: &str) -> Result<Option<&'a [u8]>, Diagnostic> {
    let mut entries = held.iter().filter(|q| document_entry(&q.name, &q.sha256_hex) == Some(token));
    let Some(entry) = entries.next() else {
        return Ok(None);
    };
    if entries.next().is_some() {
        return Err(Diagnostic { code: Code::DuplicateIdentity, target: format!("{DOCUMENT_ENTRY_PREFIX}{token}") });
    }
    Ok(Some(entry.bytes.as_slice()))
}

/// The loss notes of an opened package's invalid entries: every entry of a token that is repeated or
/// does not validate against the opened document. Only a cancel is an error: the file still opens.
pub(crate) fn invalid_entry_notes(
    held: &[Quarantined],
    document: &StudioDocument,
    budget: Budget,
    cancel: &CancellationToken,
) -> Result<Vec<LossNote>, SessionError> {
    let mut notes = Vec::new();
    for token in [SEQUENCE_SETTINGS_ENTRY, SEQUENCE_PRESETS_ENTRY, TRACK_DISPLAY_ENTRY, COMPOSITION_TOKEN_ENTRY] {
        let invalid = match held_body(held, token) {
            Ok(None) => false,
            Err(_) => true,
            Ok(Some(bytes)) => match check_entry(token, bytes, document, budget, cancel) {
                Ok(()) => false,
                Err(SessionError::Canceled) => return Err(SessionError::Canceled),
                Err(_) => true,
            },
        };
        if invalid {
            let named = held.iter().filter(|q| document_entry(&q.name, &q.sha256_hex) == Some(token));
            notes.extend(named.map(|q| LossNote { name: q.name.clone(), reason: LossReason::Opaque }));
        }
    }
    Ok(notes)
}

impl Session {
    /// The stored sequence settings records, validated against the document (`None` = none stored). An
    /// entry that does not validate is the Folio diagnostic, never partial records.
    pub fn sequence_settings_records(
        &self,
        cancel: &CancellationToken,
    ) -> Result<Option<StudioSequenceSettingsRecords>, SessionError> {
        cancel.check().map_err(|_| SessionError::Canceled)?;
        let Some(bytes) = held_body(self.held_entries(), SEQUENCE_SETTINGS_ENTRY).map_err(folio_err)? else {
            return Ok(None);
        };
        parse_sequence_settings_records(bytes, self.snapshot().document(), self.folio_budget(), cancel)
            .map(Some)
            .map_err(folio_err)
    }

    /// The ids of the document's sequences that have no settings entry, in document order (R55
    /// F-R55-4): validation passes such a sequence with no report, though STU-VID-011 makes every field
    /// required and STU-VID-017 has a sequence reference one mode, and a nest or multicam source
    /// sequence is created without one. Read-only: it creates no entry and sends no frame, as the other
    /// typed reads. While the settings entry is invalid this is the refusal of
    /// [`Session::sequence_settings_records`], not a guess at what the bad entry names.
    pub fn sequences_without_settings(&self, cancel: &CancellationToken) -> Result<Vec<String>, SessionError> {
        let records = self.sequence_settings_records(cancel)?;
        let named = |id: &str| records.as_ref().is_some_and(|r| r.entry(id).is_some());
        let sequences = &self.snapshot().document().sequences;
        Ok(sequences.iter().filter(|s| !named(&s.sequence_id)).map(|s| s.sequence_id.clone()).collect())
    }

    /// Stores (`Some`, validated first) or removes (`None`) the sequence settings records, replacing
    /// whatever entry is held, an invalid one included. Not a revision and not undoable, like every
    /// direct document-entry write: an edit that belongs in the history goes through
    /// [`Session::submit_sequence_records`]. Refusals change nothing.
    pub fn set_sequence_settings_records(
        &mut self,
        records: Option<&StudioSequenceSettingsRecords>,
        cancel: &CancellationToken,
    ) -> Result<(), SessionError> {
        let bytes = match records {
            None => None,
            Some(r) => Some(serde_json::to_vec(r).map_err(|_| invalid_json(SEQUENCE_SETTINGS_ENTRY))?),
        };
        self.set_document_entry(SEQUENCE_SETTINGS_ENTRY, bytes, cancel)
    }

    /// The stored sequence preset library, validated (`None` = none stored; an entry that does not
    /// validate is the Folio diagnostic). The shipped library
    /// (`hsk_studio_folio::shipped_sequence_presets`) is not part of it.
    pub fn sequence_presets(&self, cancel: &CancellationToken) -> Result<Option<StudioSequencePresetSet>, SessionError> {
        cancel.check().map_err(|_| SessionError::Canceled)?;
        let Some(bytes) = held_body(self.held_entries(), SEQUENCE_PRESETS_ENTRY).map_err(folio_err)? else {
            return Ok(None);
        };
        parse_sequence_presets(bytes, self.folio_budget(), cancel).map(Some).map_err(folio_err)
    }

    /// Stores (`Some`, validated first) or removes (`None`) the sequence preset library, replacing
    /// whatever entry is held. Not a revision and not undoable (see
    /// [`Session::set_sequence_settings_records`]).
    pub fn set_sequence_presets(
        &mut self,
        presets: Option<&StudioSequencePresetSet>,
        cancel: &CancellationToken,
    ) -> Result<(), SessionError> {
        let bytes = match presets {
            None => None,
            Some(p) => Some(serde_json::to_vec(p).map_err(|_| invalid_json(SEQUENCE_PRESETS_ENTRY))?),
        };
        self.set_document_entry(SEQUENCE_PRESETS_ENTRY, bytes, cancel)
    }

    /// The stored track display records, validated against the document (`None` = none stored). An
    /// entry that does not validate is the Folio diagnostic, never partial records.
    pub fn track_display_records(&self, cancel: &CancellationToken) -> Result<Option<StudioTrackDisplayRecords>, SessionError> {
        cancel.check().map_err(|_| SessionError::Canceled)?;
        let Some(bytes) = held_body(self.held_entries(), TRACK_DISPLAY_ENTRY).map_err(folio_err)? else {
            return Ok(None);
        };
        parse_track_display_records(bytes, self.snapshot().document(), self.folio_budget(), cancel)
            .map(Some)
            .map_err(folio_err)
    }

    /// Every document sequence, in document order, with the display state it resolves to: its entry's,
    /// or the declared defaults with `stored` false. Read-only: it creates no entry and sends no frame,
    /// as the other typed reads. While the entry is invalid this is the refusal of
    /// [`Session::track_display_records`], not a guess at what the bad entry names.
    pub fn resolved_track_displays(&self, cancel: &CancellationToken) -> Result<Vec<StudioResolvedTrackDisplay>, SessionError> {
        let records = self.track_display_records(cancel)?;
        Ok(resolve_track_displays(records.as_ref(), self.snapshot().document()))
    }

    /// The stored entries that name a sequence the document does not hold, in stored order. Read-only:
    /// nothing removes one on its own, and a sequence that comes back has its state again. A records
    /// step that quotes the records and writes them without one removes it. While the entry is invalid
    /// this is the refusal of [`Session::track_display_records`].
    pub fn inert_track_display_entries(&self, cancel: &CancellationToken) -> Result<Vec<StudioTrackDisplayEntry>, SessionError> {
        let records = self.track_display_records(cancel)?;
        let document = self.snapshot().document();
        Ok(records.as_ref().map_or_else(Vec::new, |r| inert_track_display_entries(r, document).into_iter().cloned().collect()))
    }

    /// Stores (`Some`, validated first) or removes (`None`) the track display records, replacing
    /// whatever entry is held, an invalid one included. Not a revision and not undoable (see
    /// [`Session::set_sequence_settings_records`]).
    pub fn set_track_display_records(
        &mut self,
        records: Option<&StudioTrackDisplayRecords>,
        cancel: &CancellationToken,
    ) -> Result<(), SessionError> {
        let bytes = match records {
            None => None,
            Some(r) => Some(serde_json::to_vec(r).map_err(|_| invalid_json(TRACK_DISPLAY_ENTRY))?),
        };
        self.set_document_entry(TRACK_DISPLAY_ENTRY, bytes, cancel)
    }
}
