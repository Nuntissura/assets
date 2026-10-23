//! Terminal observability for a render: exactly one Observe terminal frame per call, carrying only
//! the closed outcome vocabulary (no pixel, profile, path, adapter or error text). The mapping is
//! the same as render-cpu's, so a given `BackendError` has one terminal outcome on every backend.
use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{FailureCode, Observation, Observe, Outcome, Receipt, SinkPort};
use hsk_studio_pigment::render_plan::{BackendError, RenderReceipt};

/// Terminal observe outcome for a backend error.
pub const fn outcome_of(error: BackendError) -> Outcome {
    match error {
        BackendError::Canceled => Outcome::Canceled,
        BackendError::Unsupported { .. } => Outcome::Failure(FailureCode::Unsupported),
        BackendError::DeadlineExceeded
        | BackendError::Unavailable(_)
        | BackendError::DeviceLost { .. }
        | BackendError::UnresolvedSource(_)
        | BackendError::Sink(_) => Outcome::Failure(FailureCode::Unavailable),
        BackendError::StaleRevision { .. }
        | BackendError::InvalidPlan(_)
        | BackendError::SourceMismatch(_)
        | BackendError::BudgetExceeded { .. }
        | BackendError::Overflow => Outcome::Failure(FailureCode::Validation),
    }
}

/// Emit the terminal frame for `result`. `correlation_id` and `revision` must be the values the
/// `Observe` was created with; a cancelled `cancel` token is rewritten to `Canceled` by Observe.
pub fn report(
    result: &Result<RenderReceipt, BackendError>,
    correlation_id: u64,
    revision: u64,
    cancel: &CancellationToken,
    observe: &mut Observe,
    sink: &mut (impl SinkPort + ?Sized),
) -> Result<Receipt, hsk_studio_observe::Error> {
    let outcome = match result {
        Ok(_) => Outcome::Success,
        Err(error) => outcome_of(*error),
    };
    observe.emit(
        Observation {
            correlation_id,
            revision,
            outcome,
            progress: None,
            private_project_text: None,
        },
        cancel,
        sink,
    )
}
