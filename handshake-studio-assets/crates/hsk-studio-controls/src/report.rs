//! One Observe terminal frame per control operation. Telemetry carries only the closed outcome
//! vocabulary: never control ids, labels or values (a rejected value stays redacted; only the
//! failure class is emitted). The caller owns the sink, the correlation and the resource.

use crate::store::{ChangeEvent, EventPhase};
use crate::value::ControlError;
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::{Budget, Error, FailureCode, Observation, Observe, Outcome, Receipt, SinkPort};

/// Who/what an operation report is attributed to (`resource` is the host's document/panel id).
#[derive(Clone, Debug)]
pub struct ReportContext {
    pub correlation_id: u64,
    pub resource: DomainId,
    pub actor: ActorContext,
}

/// Failure class of a rejection (closed vocabulary of the Observe crate).
pub fn failure_code(error: &ControlError) -> FailureCode {
    match error {
        ControlError::UnknownControl { .. } => FailureCode::Unsupported,
        ControlError::Stale { .. } | ControlError::Gesture { .. } => FailureCode::Unavailable,
        _ => FailureCode::Validation,
    }
}

/// Maps the result of applying one interaction to an Observe outcome. `None` = not a terminal
/// operation (a live gesture sample), nothing to report.
pub fn outcome_of(result: &Result<Option<ChangeEvent>, ControlError>) -> Option<Outcome> {
    match result {
        Ok(Some(ev)) => match ev.phase {
            EventPhase::Live(_) => None,
            EventPhase::Aborted(_) => Some(Outcome::Canceled),
            EventPhase::Discrete | EventPhase::Committed(_) => Some(Outcome::Success),
        },
        Ok(None) => Some(Outcome::Success),
        Err(e) => Some(Outcome::Failure(failure_code(e))),
    }
}

/// Emits exactly one terminal frame for a finished operation; `Ok(None)` for live gesture
/// samples. A cancelled token makes Observe record `Canceled` regardless of the result.
pub fn report(
    ctx: &ReportContext,
    revision: u64,
    result: &Result<Option<ChangeEvent>, ControlError>,
    cancel: &CancellationToken,
    sink: &mut dyn SinkPort,
) -> Result<Option<Receipt>, Error> {
    let Some(outcome) = outcome_of(result) else { return Ok(None) };
    let budget = Budget::new(0, 0)?;
    let mut observe = Observe::new(ctx.correlation_id, revision, ctx.resource.clone(), ctx.actor.clone(), budget);
    let observation = Observation { correlation_id: ctx.correlation_id, revision, outcome, progress: None, private_project_text: None };
    observe.emit(observation, cancel, sink).map(Some)
}
