//! Multicam records in the session (lane SMB-PR-MULTICAM-1): a saved document carries its Folio
//! `StudioMulticamRecords` (multicam source sequences, angle numbering and the multicam mode of every
//! multicam clip, STU-VID-033) as the typed document entry `studio/multicam.<sha256>` (the existing
//! document-entry mechanism: written by every save and checkpoint, held again on reopen without a
//! loss note, bounded by `limits.max_entry_bytes` together with the other entries).
//!
//! The records are validated by Folio against the session document on every set AND every read (the
//! bytes come from a file). A refusal changes nothing; a stored entry that no longer validates (the
//! document changed under it) reads as the Folio diagnostic, never as partial records.
//! Serialisation is serde_json of the Folio type (deterministic field order).
use crate::error::SessionError;
use crate::session::Session;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{Budget, Code, Diagnostic, StudioMulticamRecords, validate_multicam_records};
use hsk_studio_package::PackageError;

/// Document-entry token of the multicam records.
pub const MULTICAM_ENTRY: &str = "multicam";

fn diag(code: Code) -> SessionError {
    SessionError::Package(PackageError::Document(Diagnostic { code, target: "studio/multicam".to_owned() }))
}

fn folio_err(d: Diagnostic) -> SessionError {
    SessionError::Package(PackageError::Document(d))
}

impl Session {
    /// The stored multicam records, validated against the document (`None` = none stored).
    pub fn multicam_records(&self, cancel: &CancellationToken) -> Result<Option<StudioMulticamRecords>, SessionError> {
        cancel.check().map_err(|_| SessionError::Canceled)?;
        let Some(bytes) = self.document_entry(MULTICAM_ENTRY) else {
            return Ok(None);
        };
        let records: StudioMulticamRecords = serde_json::from_slice(bytes).map_err(|_| diag(Code::InvalidJson))?;
        validate_multicam_records(&records, self.snapshot().document(), Budget::default(), cancel).map_err(folio_err)?;
        Ok(Some(records))
    }

    /// Stores (`Some`, validated first) or removes (`None`) the multicam records.
    pub fn set_multicam_records(
        &mut self,
        records: Option<&StudioMulticamRecords>,
        cancel: &CancellationToken,
    ) -> Result<(), SessionError> {
        cancel.check().map_err(|_| SessionError::Canceled)?;
        let bytes = match records {
            None => None,
            Some(r) => {
                validate_multicam_records(r, self.snapshot().document(), Budget::default(), cancel).map_err(folio_err)?;
                Some(serde_json::to_vec(r).map_err(|_| diag(Code::InvalidJson))?)
            }
        };
        self.set_document_entry(MULTICAM_ENTRY, bytes, cancel)
    }
}
