//! One Observe terminal frame per control operation. Telemetry carries only the closed outcome
//! vocabulary: never control ids, labels or values (a rejected value stays redacted; only the
//! failure class is emitted; proof `tests/gesture_panel_report.rs`
//! `one_observe_terminal_frame_per_operation_with_redacted_values`). The caller owns the sink, the
//! correlation and the resource.

use crate::store::{ChangeEvent, EventPhase, GesturePhase};
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

/// Maps the result of one operation to an Observe outcome. `gesture` is the interaction's gesture
/// phase (`Interaction::gesture`; `None` for discrete operations: set, typed entry, step, pointer
/// target, reset, undo). `None` = not a terminal operation, nothing to report.
///
/// An unchanged value (`Ok(None)` from the store) is reported by what the route asked for (R5
/// no-effect audit; proof `tests/step_effect.rs`
/// `no_change_is_reported_by_route_and_a_refused_end_keeps_the_gesture`):
/// - discrete absolute route whose requested value already holds: `Success` (idempotent). A
///   relative step does not reach this arm: `ControlSpec::step_value` refuses a no-effect step;
/// - live sample (`Update`) that moved nothing, e.g. dragged further past a hard stop: not terminal;
/// - `Abort` with no open gesture: `Canceled`, not `Success`;
/// - `End` with no net change (or no gesture opened): `Success`, and no undo step exists.
pub fn outcome_of(gesture: Option<GesturePhase>, result: &Result<Option<ChangeEvent>, ControlError>) -> Option<Outcome> {
    match result {
        Ok(Some(ev)) => match ev.phase {
            EventPhase::Live(_) => None,
            EventPhase::Aborted(_) => Some(Outcome::Canceled),
            EventPhase::Discrete | EventPhase::Committed(_) => Some(Outcome::Success),
        },
        Ok(None) => match gesture {
            Some(GesturePhase::Update) => None,
            Some(GesturePhase::Abort) => Some(Outcome::Canceled),
            Some(GesturePhase::End) | None => Some(Outcome::Success),
        },
        Err(e) => Some(Outcome::Failure(failure_code(e))),
    }
}

/// Emits one terminal frame for a finished operation (see [`outcome_of`]); `Ok(None)` when the
/// operation is not terminal. A cancelled token makes Observe record `Canceled` whatever the
/// result.
pub fn report(
    ctx: &ReportContext,
    revision: u64,
    gesture: Option<GesturePhase>,
    result: &Result<Option<ChangeEvent>, ControlError>,
    cancel: &CancellationToken,
    sink: &mut dyn SinkPort,
) -> Result<Option<Receipt>, Error> {
    let Some(outcome) = outcome_of(gesture, result) else { return Ok(None) };
    let budget = Budget::new(0, 0)?;
    let mut observe = Observe::new(ctx.correlation_id, revision, ctx.resource.clone(), ctx.actor.clone(), budget);
    let observation = Observation { correlation_id: ctx.correlation_id, revision, outcome, progress: None, private_project_text: None };
    observe.emit(observation, cancel, sink).map(Some)
}
