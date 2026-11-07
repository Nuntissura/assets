//! One artboard or page edit = one undo step (lane SMB-DOC-NEW-1; STU-DOC-001 "`StudioArtboard` /
//! `StudioPageSpread` containers", STU-VEC-028, STU-HIS-002 undo "by applying its inverse command").
//!
//! Chronicle addresses the name of a container and nothing else, so adding, resizing or deleting one
//! does not travel as a chronicle `Patch`. A [`ContainerStep`] replaces the document's two container
//! lists: `expect_*` is the read precondition (the lists the step was computed from) and `write_*`
//! the new lists, one value write, one revision and one receipt, with the semantics of
//! [`crate::TimelineStep`]: the inverse swaps the two ([`ContainerStep::inverse`], the undo; its
//! inverse is the redo) and an edit of the lists in between makes the precondition fail
//! (`value_conflict`) rather than overwrite it.
//!
//! The session owns no rule of what a container may be: Folio validates the result (a removed
//! container that a layer still names is refused `folio_rejected`). The caller's bounds and typed
//! refusals live in harness-core.
use crate::error::SessionError;
use crate::timeline::valid_command_id;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{self as folio, Resolver, Snapshot, StudioArtboard, StudioPageSpread};

/// Provider name recorded in [`crate::Applied::provider`] and in the idempotency binding.
pub const CONTAINER_STEP_PROVIDER: &str = "document.containers";

/// Replace the document's artboards and page spreads `expect` -> `write` (see the module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct ContainerStep {
    pub command_id: String,
    pub correlation_id: u64,
    pub expect_artboards: Vec<StudioArtboard>,
    pub write_artboards: Vec<StudioArtboard>,
    pub expect_spreads: Vec<StudioPageSpread>,
    pub write_spreads: Vec<StudioPageSpread>,
}

impl ContainerStep {
    /// The step that restores `expect`, under fresh caller identity.
    pub fn inverse(&self, command_id: String, correlation_id: u64) -> ContainerStep {
        ContainerStep {
            command_id,
            correlation_id,
            expect_artboards: self.write_artboards.clone(),
            write_artboards: self.expect_artboards.clone(),
            expect_spreads: self.write_spreads.clone(),
            write_spreads: self.expect_spreads.clone(),
        }
    }

    /// Deterministic bytes of the request for the idempotency binding and admission cost.
    pub(crate) fn binding_bytes(&self) -> Result<Vec<u8>, SessionError> {
        let invalid = |_| SessionError::Provider("invalid_input");
        let parts = [
            serde_json::to_vec(&self.expect_artboards).map_err(invalid)?,
            serde_json::to_vec(&self.write_artboards).map_err(invalid)?,
            serde_json::to_vec(&self.expect_spreads).map_err(invalid)?,
            serde_json::to_vec(&self.write_spreads).map_err(invalid)?,
        ];
        let mut out = format!("hsk-studio-container-step-1\0{}\0{}\0", self.command_id, self.correlation_id).into_bytes();
        for part in &parts {
            out.extend_from_slice(part);
            out.push(0);
        }
        Ok(out)
    }
}

/// The checked successor of `snapshot` with the step applied at `revision + 1`, or `None` when the
/// step writes the lists the document already holds. Nothing is published here.
pub(crate) fn successor(
    snapshot: &Snapshot,
    step: &ContainerStep,
    budget: folio::Budget,
    resolver: &dyn Resolver,
    cancel: &CancellationToken,
) -> Result<Option<Snapshot>, SessionError> {
    if !valid_command_id(&step.command_id) {
        return Err(SessionError::Provider("invalid_command"));
    }
    let document = snapshot.document();
    if document.artboards != step.expect_artboards || document.page_spreads != step.expect_spreads {
        return Err(SessionError::Provider("value_conflict"));
    }
    if step.write_artboards == step.expect_artboards && step.write_spreads == step.expect_spreads {
        return Ok(None);
    }
    let mut next = document.clone();
    next.artboards.clone_from(&step.write_artboards);
    next.page_spreads.clone_from(&step.write_spreads);
    next.revision = document.revision.checked_add(1).ok_or(SessionError::Provider("revision_overflow"))?;
    folio::validate_document(next, budget, cancel, resolver).map(Some).map_err(|d| match d.code {
        folio::Code::Canceled => SessionError::Canceled,
        folio::Code::BudgetExceeded => SessionError::Provider("budget_exceeded"),
        _ => SessionError::Provider("folio_rejected"),
    })
}
