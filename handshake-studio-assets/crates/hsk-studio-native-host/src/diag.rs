//! Terminal outcomes through observe (common rule 5): codes and counters only, never text. The caller supplies the
//! actor, the resource id, the correlation and the sink.
use crate::error::HostError;
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::{Budget, Error, FailureCode, Observation, Observe, Outcome, Receipt, SinkPort};

/// The observe outcome of a host run.
pub fn outcome(result: Result<(), HostError>) -> Outcome {
    match result {
        Ok(()) => Outcome::Success,
        Err(HostError::Vulkan(r)) => Outcome::Failure(match r.class() {
            "loss" => FailureCode::Loss,
            "unsupported" => FailureCode::Unsupported,
            "validation" => FailureCode::Validation,
            _ => FailureCode::Unavailable,
        }),
        Err(HostError::Usage(_) | HostError::Document(_)) => Outcome::Failure(FailureCode::Validation),
        Err(HostError::FocusViolation) => Outcome::Failure(FailureCode::Validation),
        Err(HostError::Io(_) | HostError::Window(_) | HostError::Timeout) => Outcome::Failure(FailureCode::Unavailable),
    }
}

/// Emits the terminal frame of one host run (device open, window run or offscreen capture).
pub fn emit_terminal(
    actor: &ActorContext,
    resource: &DomainId,
    correlation_id: u64,
    result: Result<(), HostError>,
    cancel: &CancellationToken,
    sink: &mut dyn SinkPort,
) -> Result<Receipt, Error> {
    let mut observe = Observe::new(correlation_id, 0, resource.clone(), actor.clone(), Budget::new(0, 0)?);
    let observation =
        Observation { correlation_id, revision: 0, outcome: outcome(result), progress: None, private_project_text: None };
    observe.emit(observation, cancel, sink)
}
