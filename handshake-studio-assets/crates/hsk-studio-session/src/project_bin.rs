//! The project bin in the session (lane SMB-PROJECT-BIN-1; STU-VID-050): a saved document carries its
//! Folio `StudioProjectBin` as the document entry `studio/project_bin.<sha256>`, under the open, set,
//! read and save rules of `crate::sequence_records` (validated on open, on every set and on every read;
//! one that does not validate opens with a `LossReason::Opaque` note, refuses on read and keeps its
//! bytes until a set replaces it).
//!
//! The bin names the document only by its `SDOC` id and the media by content digest, never a sequence,
//! track or clip id, so no commit can make it invalid and none is guarded by it (the media markers
//! reading). The bytes an item names are staged by the host (`Session::stage_tile`, which holds any
//! body by sha256) and written with the package as `assets/<sha256>`, because a save embeds the timeline
//! media its staged set grants (`EmbedExternal::Granted`); on reopen `Session::tile_bytes` serves them
//! from the package. Tests: hsk-studio-harness tests/project_bin.rs.
use crate::error::SessionError;
use crate::sequence_records::{folio_err, held_body};
use crate::session::Session;
use hsk_studio_accord::{CancellationToken, CommandContext};
use hsk_studio_folio::{
    Code, Diagnostic, StudioProjectBin, StudioProjectBinCommand, StudioProjectBinError, StudioProjectBinOutcome, apply_project_bin_command,
    parse_project_bin,
};

/// Document-entry token of the project bin (STU-VID-050).
pub const PROJECT_BIN_ENTRY: &str = "project_bin";

impl Session {
    /// The stored project bin, validated against the document (`None` = none stored). An entry that does
    /// not validate is the Folio diagnostic, never a partial bin.
    pub fn project_bin(&self, cancel: &CancellationToken) -> Result<Option<StudioProjectBin>, SessionError> {
        cancel.check().map_err(|_| SessionError::Canceled)?;
        let Some(bytes) = held_body(self.held_entries(), PROJECT_BIN_ENTRY).map_err(folio_err)? else {
            return Ok(None);
        };
        parse_project_bin(bytes, self.snapshot().document(), self.folio_budget(), cancel).map(Some).map_err(folio_err)
    }

    /// Stores (`Some`, validated first) or removes (`None`) the project bin, replacing whatever entry is
    /// held, an invalid one included. Not a revision and not undoable, like every direct document-entry
    /// write: the host's history step restores the bytes it replaced.
    pub fn set_project_bin(&mut self, bin: Option<&StudioProjectBin>, cancel: &CancellationToken) -> Result<(), SessionError> {
        let bytes = match bin {
            None => None,
            Some(b) => Some(serde_json::to_vec(b).map_err(|_| folio_err(Diagnostic { code: Code::InvalidJson, target: PROJECT_BIN_ENTRY.to_owned() }))?),
        };
        self.set_document_entry(PROJECT_BIN_ENTRY, bytes, cancel)
    }

    /// Runs the Folio command on the stored bin (none stored = an empty bin of this document) under the
    /// caller's `context` at the live revision, and returns the outcome without storing it. Refusals:
    /// the read refusal of [`Session::project_bin`], then Folio's codes (`command_mismatch`,
    /// `stale_revision`, `duplicate_item`, `item_in_use`, ...) as `SessionError::Provider`.
    pub fn apply_project_bin_command(
        &self,
        context: &CommandContext,
        command: &StudioProjectBinCommand,
        cancel: &CancellationToken,
    ) -> Result<StudioProjectBinOutcome, SessionError> {
        let document = self.snapshot().document();
        let base = self.project_bin(cancel)?.unwrap_or_else(|| StudioProjectBin::new(document.document_id.clone()));
        apply_project_bin_command(document, &base, context, self.document_revision(), command, self.folio_budget(), cancel).map_err(|e| match e {
            StudioProjectBinError::Canceled => SessionError::Canceled,
            StudioProjectBinError::StaleRevision { expected, current } => SessionError::StaleRevision { expected, current },
            other => SessionError::Provider(other.code()),
        })
    }
}
