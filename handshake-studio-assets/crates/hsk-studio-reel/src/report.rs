//! Terminal-outcome delivery through `hsk-studio-observe`. Closed vocabulary only: no paths, names or
//! parser text ever reach the sink.
use crate::error::ReelError;
use crate::model::SegmentReceipt;
use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{Error, FailureCode, Observation, Observe, Outcome, Receipt, SinkPort};

/// Map a segment result to the shared outcome vocabulary.
/// Complete -> Success; truncation/partial -> Loss; missing codec/type -> Unsupported; cancel -> Canceled.
pub fn outcome_for(result: &Result<SegmentReceipt, ReelError>) -> Outcome {
    match result {
        Ok(receipt) if receipt.complete => Outcome::Success,
        Ok(_) => Outcome::Failure(FailureCode::Loss),
        Err(ReelError::Canceled) => Outcome::Canceled,
        Err(ReelError::Truncated { .. }) => Outcome::Failure(FailureCode::Loss),
        Err(
            ReelError::CodecUnavailable { .. }
            | ReelError::ImportUnsupported { .. }
            | ReelError::Unsupported { .. }
            | ReelError::StrictOriginalRequired,
        ) => Outcome::Failure(FailureCode::Unsupported),
        Err(ReelError::Io(_)) => Outcome::Failure(FailureCode::Unavailable),
        Err(
            ReelError::StaleGrant
            | ReelError::OutOfRange { .. }
            | ReelError::Corrupt { .. }
            | ReelError::LimitExceeded { .. },
        ) => Outcome::Failure(FailureCode::Validation),
    }
}

/// Emit exactly one terminal frame for a finished segment attempt.
pub fn report(
    correlation_id: u64,
    revision: u64,
    result: &Result<SegmentReceipt, ReelError>,
    cancel: &CancellationToken,
    observe: &mut Observe,
    sink: &mut (impl SinkPort + ?Sized),
) -> Result<Receipt, Error> {
    observe.emit(
        Observation {
            correlation_id,
            revision,
            outcome: outcome_for(result),
            progress: None,
            private_project_text: None,
        },
        cancel,
        sink,
    )
}
