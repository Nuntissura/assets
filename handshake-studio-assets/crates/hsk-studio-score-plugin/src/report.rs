use crate::error::PluginError;
use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{FailureCode, Observation, Observe, Outcome, Receipt, SinkPort};

/// Outcome class for diagnostics: Ok -> Success, Canceled -> Canceled, unauthorized, hash, ABI,
/// lifecycle and configuration refusals -> Failure(Validation), a stale or lost state ->
/// Failure(Loss), every process fault (crash, timeout, protocol, budget, unavailable) ->
/// Failure(Unavailable).
pub fn outcome_of<T>(result: &Result<T, PluginError>) -> Outcome {
    match result {
        Ok(_) => Outcome::Success,
        Err(PluginError::Canceled) => Outcome::Canceled,
        Err(
            PluginError::Unauthorized
            | PluginError::HashMismatch
            | PluginError::AbiMismatch
            | PluginError::IllegalTransition { .. }
            | PluginError::InvalidConfig
            | PluginError::InvalidBlock(_),
        ) => Outcome::Failure(FailureCode::Validation),
        Err(PluginError::StaleState | PluginError::NonFiniteOutput) => {
            Outcome::Failure(FailureCode::Loss)
        }
        Err(
            PluginError::ProbeTimeout
            | PluginError::Crashed { .. }
            | PluginError::Timeout
            | PluginError::BudgetExceeded
            | PluginError::Protocol(_)
            | PluginError::PluginFault
            | PluginError::Unavailable,
        ) => Outcome::Failure(FailureCode::Unavailable),
    }
}

/// Emits the terminal diagnostic for one plugin call through the caller's Observe/sink.
/// `correlation_id` and `revision` must equal the values `obs` was created with. No plugin path,
/// project text or sample data is ever attached.
pub fn report<T>(
    result: &Result<T, PluginError>,
    correlation_id: u64,
    revision: u64,
    cancel: &CancellationToken,
    obs: &mut Observe,
    sink: &mut (impl SinkPort + ?Sized),
) -> Result<Receipt, hsk_studio_observe::Error> {
    obs.emit(
        Observation {
            correlation_id,
            revision,
            outcome: outcome_of(result),
            progress: None,
            private_project_text: None,
        },
        cancel,
        sink,
    )
}
