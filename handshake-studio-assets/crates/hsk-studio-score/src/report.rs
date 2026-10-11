use crate::engine::BlockReceipt;
use crate::error::ScoreError;
use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{FailureCode, Observation, Observe, Outcome, Receipt, SinkPort};

/// Outcome class for diagnostics: Ok -> Success, Canceled -> Canceled, Busy/CapacityExceeded ->
/// Failure(Unavailable), every other refusal -> Failure(Validation).
pub fn outcome_of(result: &Result<BlockReceipt, ScoreError>) -> Outcome {
    match result {
        Ok(_) => Outcome::Success,
        Err(ScoreError::Canceled) => Outcome::Canceled,
        Err(ScoreError::Busy | ScoreError::CapacityExceeded) => {
            Outcome::Failure(FailureCode::Unavailable)
        }
        Err(_) => Outcome::Failure(FailureCode::Validation),
    }
}

/// Emits the terminal diagnostic for one processed block through the caller's Observe/sink.
/// `correlation_id` and `revision` must equal the values `obs` was created with. No project
/// text or sample data is ever attached.
pub fn report(
    result: &Result<BlockReceipt, ScoreError>,
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
