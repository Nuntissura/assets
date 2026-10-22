//! Terminal outcome delivery through the shared Observe adapter. Exactly one terminal frame per
//! call; no source text, names or paths are emitted.

use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{Error as ObserveError, FailureCode, Observation, Observe, Outcome, Receipt, SinkPort};

use crate::error::PsdError;
use crate::loss::LossKind;
use crate::proposal::ImportProposal;

#[derive(Clone, Copy, Debug)]
pub struct ReportContext {
    /// Must equal the `Observe` instance's correlation id.
    pub correlation_id: u64,
    /// Must equal the `Observe` instance's revision.
    pub revision: u64,
    /// Caller policy: an `Unsupported` loss entry makes the import a failure instead of a
    /// success carrying a report.
    pub loss_is_error: bool,
}

pub fn outcome_of(result: &Result<ImportProposal, PsdError>, loss_is_error: bool) -> Outcome {
    match result {
        Ok(p) if loss_is_error && p.loss.entries.iter().any(|e| e.kind == LossKind::Unsupported) => {
            Outcome::Failure(FailureCode::Loss)
        }
        Ok(_) => Outcome::Success,
        Err(PsdError::Canceled) => Outcome::Canceled,
        Err(PsdError::UnsupportedVersion(_) | PsdError::UnsupportedMode(_)) => Outcome::Failure(FailureCode::Unsupported),
        Err(_) => Outcome::Failure(FailureCode::Validation),
    }
}

pub fn report(
    result: &Result<ImportProposal, PsdError>,
    context: &ReportContext,
    cancel: &CancellationToken,
    observe: &mut Observe,
    sink: &mut (impl SinkPort + ?Sized),
) -> Result<Receipt, ObserveError> {
    let event = Observation {
        correlation_id: context.correlation_id,
        revision: context.revision,
        outcome: outcome_of(result, context.loss_is_error),
        progress: None,
        private_project_text: None,
    };
    observe.emit(event, cancel, sink)
}
