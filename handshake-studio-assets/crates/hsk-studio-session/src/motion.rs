//! Composition and motion records in the session (lane SMB-AE-MOTION-RECORDS-2): a saved document
//! carries its Folio `StudioCompositionRecords` and `StudioMotionRecords` (shape layers, text
//! animators, composition audio, render queue) as the typed document entries
//! `studio/composition.<sha256>` and `studio/motion.<sha256>` (the existing document-entry
//! mechanism: written by every save and checkpoint, held again on reopen without a loss note, bounded
//! by `limits.max_entry_bytes` together with the other entries).
//!
//! Both are validated by Folio on every set AND every read (the bytes come from a file): the
//! composition records against the session document, the motion records against the document and
//! the stored composition records. A refusal changes nothing; a stored entry that no longer
//! validates (the document changed under it) reads as the Folio diagnostic, never as partial
//! records. Serialisation is serde_json of the Folio types (deterministic field order).
use crate::error::SessionError;
use crate::session::Session;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    Budget, Code, Diagnostic, Resolver, StudioCompositionRecords, StudioMotionRecords,
    validate_composition_records, validate_motion_records,
};
use hsk_studio_package::PackageError;

/// Document-entry token of the composition records.
pub const COMPOSITION_ENTRY: &str = "composition";
/// Document-entry token of the motion records.
pub const MOTION_ENTRY: &str = "motion";

fn diag(code: Code, target: &str) -> SessionError {
    SessionError::Package(PackageError::Document(Diagnostic { code, target: target.to_owned() }))
}

fn folio_err(d: Diagnostic) -> SessionError {
    SessionError::Package(PackageError::Document(d))
}

impl Session {
    /// The stored composition records, validated against the document (`None` = none stored).
    pub fn composition_records(
        &self,
        resolver: &dyn Resolver,
        cancel: &CancellationToken,
    ) -> Result<Option<StudioCompositionRecords>, SessionError> {
        let Some(bytes) = self.document_entry(COMPOSITION_ENTRY) else {
            return Ok(None);
        };
        let records: StudioCompositionRecords =
            serde_json::from_slice(bytes).map_err(|_| diag(Code::InvalidJson, "studio/composition"))?;
        validate_composition_records(&records, self.snapshot().document(), Budget::default(), cancel, resolver)
            .map_err(folio_err)?;
        Ok(Some(records))
    }

    /// Stores (`Some`, validated first) or removes (`None`) the composition records. Removing them
    /// while motion records are stored is refused (`dangling_reference`).
    pub fn set_composition_records(
        &mut self,
        records: Option<&StudioCompositionRecords>,
        resolver: &dyn Resolver,
        cancel: &CancellationToken,
    ) -> Result<(), SessionError> {
        cancel.check().map_err(|_| SessionError::Canceled)?;
        let bytes = match records {
            None => {
                if self.document_entry(MOTION_ENTRY).is_some() {
                    return Err(diag(Code::DanglingReference, "studio/motion"));
                }
                None
            }
            Some(r) => {
                validate_composition_records(r, self.snapshot().document(), Budget::default(), cancel, resolver)
                    .map_err(folio_err)?;
                Some(serde_json::to_vec(r).map_err(|_| diag(Code::InvalidJson, "studio/composition"))?)
            }
        };
        self.set_document_entry(COMPOSITION_ENTRY, bytes, cancel)
    }

    /// The stored motion records, validated against the document and the stored composition records.
    pub fn motion_records(
        &self,
        resolver: &dyn Resolver,
        cancel: &CancellationToken,
    ) -> Result<Option<StudioMotionRecords>, SessionError> {
        let Some(bytes) = self.document_entry(MOTION_ENTRY) else {
            return Ok(None);
        };
        let records: StudioMotionRecords =
            serde_json::from_slice(bytes).map_err(|_| diag(Code::InvalidJson, "studio/motion"))?;
        let compositions = self
            .composition_records(resolver, cancel)?
            .ok_or_else(|| diag(Code::DanglingReference, "studio/composition"))?;
        validate_motion_records(&records, self.snapshot().document(), &compositions, Budget::default(), cancel, resolver)
            .map_err(folio_err)?;
        Ok(Some(records))
    }

    /// Stores (`Some`, validated against the stored composition records first) or removes the
    /// motion records.
    pub fn set_motion_records(
        &mut self,
        records: Option<&StudioMotionRecords>,
        resolver: &dyn Resolver,
        cancel: &CancellationToken,
    ) -> Result<(), SessionError> {
        cancel.check().map_err(|_| SessionError::Canceled)?;
        let bytes = match records {
            None => None,
            Some(r) => {
                let compositions = self
                    .composition_records(resolver, cancel)?
                    .ok_or_else(|| diag(Code::DanglingReference, "studio/composition"))?;
                validate_motion_records(r, self.snapshot().document(), &compositions, Budget::default(), cancel, resolver)
                    .map_err(folio_err)?;
                Some(serde_json::to_vec(r).map_err(|_| diag(Code::InvalidJson, "studio/motion"))?)
            }
        };
        self.set_document_entry(MOTION_ENTRY, bytes, cancel)
    }
}
