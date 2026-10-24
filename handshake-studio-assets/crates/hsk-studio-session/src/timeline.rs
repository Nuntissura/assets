//! One timeline edit = one undo step (lane SMB-FOLIO-TIMELINE-RECORDS; STU-VID-040 "Every operation
//! in this sub-section is one reversible `StudioHistoryEntry`", STU-HIS-002 undo "by applying its
//! inverse command or a snapshot revert").
//!
//! A [`TimelineStep`] replaces ONE sequence value: `expect` is the read precondition (the sequence
//! the step was computed from) and `write` the new value. Because Folio stores the clips, the
//! sequence and clip markers and the clip transitions inside the sequence, a timeline edit plus the
//! marker and transition changes that follow it is one value write, one revision step, one commit
//! and one receipt. The applied step returns its inverse template (`expect` and `write` swapped),
//! submitted like any other step for undo, and the undo's own inverse is the redo; a foreign edit
//! of the same sequence in between makes the precondition fail (`value_conflict`) rather than
//! overwrite it. This follows chronicle's precondition/inverse semantics (STU-CON-022) at sequence
//! scope; chronicle's property table addresses layers, artboards and page spreads only, so the
//! sequence value does not travel as a chronicle `Patch`. It shares the submit pipeline (grant,
//! idempotency key and receipt, admission, commit authority, publish) with provider submits.
use crate::error::SessionError;
use hsk_studio_accord::{ActorContext, CancellationToken};
use hsk_studio_chronicle::Actor;
use hsk_studio_folio::{self as folio, Resolver, Snapshot, StudioSequence};

/// Provider name recorded in [`crate::Applied::provider`] and in the idempotency binding.
pub const TIMELINE_STEP_PROVIDER: &str = "timeline.sequence";
/// Longest `command_id` in bytes (chronicle's bound).
pub const MAX_COMMAND_ID_BYTES: usize = 128;

/// Replace one sequence value `expect` -> `write` (see the module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct TimelineStep {
    pub command_id: String,
    pub correlation_id: u64,
    pub sequence_id: String,
    pub expect: StudioSequence,
    pub write: StudioSequence,
}

impl TimelineStep {
    /// The step that restores `expect`, under fresh caller identity.
    pub fn inverse(&self, command_id: String, correlation_id: u64) -> TimelineStep {
        TimelineStep {
            command_id,
            correlation_id,
            sequence_id: self.sequence_id.clone(),
            expect: self.write.clone(),
            write: self.expect.clone(),
        }
    }

    /// Deterministic bytes of the request for the idempotency binding and admission cost.
    pub(crate) fn binding_bytes(&self) -> Result<Vec<u8>, SessionError> {
        let encode = |s: &StudioSequence| {
            serde_json::to_vec(s).map_err(|_| SessionError::Provider("invalid_input"))
        };
        let mut out = format!(
            "hsk-studio-timeline-step-1\0{}\0{}\0{}\0",
            self.command_id, self.correlation_id, self.sequence_id
        )
        .into_bytes();
        out.extend_from_slice(&encode(&self.expect)?);
        out.push(0);
        out.extend_from_slice(&encode(&self.write)?);
        Ok(out)
    }
}

/// A non-empty command id of at most [`MAX_COMMAND_ID_BYTES`] without control characters.
pub(crate) fn valid_command_id(command_id: &str) -> bool {
    !command_id.is_empty() && command_id.len() <= MAX_COMMAND_ID_BYTES && !command_id.chars().any(char::is_control)
}

pub(crate) fn actor_of(actor: &ActorContext) -> Actor {
    Actor {
        account_id: actor.account_id().to_owned(),
        principal_id: actor.principal_id().to_owned(),
        owner_account_id: actor.owner_account_id().to_owned(),
        owner_principal_id: actor.owner_principal_id().to_owned(),
        access_space_id: actor.access_space_id().to_owned(),
        session_id: actor.session_id().to_owned(),
    }
}

/// The checked successor of `snapshot` with the step applied at `revision + 1`, or `None` when
/// the step writes the value the document already holds. Nothing is published here.
pub(crate) fn successor(
    snapshot: &Snapshot,
    step: &TimelineStep,
    budget: folio::Budget,
    resolver: &dyn Resolver,
    cancel: &CancellationToken,
) -> Result<Option<Snapshot>, SessionError> {
    if !valid_command_id(&step.command_id) {
        return Err(SessionError::Provider("invalid_command"));
    }
    if step.expect.sequence_id != step.sequence_id || step.write.sequence_id != step.sequence_id {
        return Err(SessionError::Provider("invalid_input"));
    }
    let document = snapshot.document();
    let index = document
        .sequences
        .iter()
        .position(|s| s.sequence_id == step.sequence_id)
        .ok_or(SessionError::Provider("invalid_target"))?;
    if document.sequences[index] != step.expect {
        return Err(SessionError::Provider("value_conflict"));
    }
    if step.write == step.expect {
        return Ok(None);
    }
    let mut next = document.clone();
    next.sequences[index] = step.write.clone();
    next.revision = document
        .revision
        .checked_add(1)
        .ok_or(SessionError::Provider("revision_overflow"))?;
    folio::validate_document(next, budget, cancel, resolver)
        .map(Some)
        .map_err(|d| match d.code {
            folio::Code::Canceled => SessionError::Canceled,
            folio::Code::BudgetExceeded => SessionError::Provider("budget_exceeded"),
            _ => SessionError::Provider("folio_rejected"),
        })
}
