//! Observability wiring: results of web operations become closed observe outcomes. Only the outcome class,
//! correlation id and source revision are emitted; source text, paths, selectors and attribute values never
//! reach telemetry (`private_project_text` is always `None`). Denied assets inside an `Ok` result are a
//! `Success` whose list the caller keeps (`collect_assets`).

use crate::error::WebError;
use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{Error as ObserveError, Observation, Observe, Outcome, Receipt, SinkPort};

/// `Ok` is `Success`; errors map through `WebError::outcome` (Canceled, Unsupported, otherwise Validation).
pub fn outcome_of<T>(result: &Result<T, WebError>) -> Outcome {
    match result {
        Ok(_) => Outcome::Success,
        Err(e) => e.outcome(),
    }
}

/// Deliver the terminal observation for one web operation through the caller's sink. A cancelled `cancel`
/// token delivers `Canceled` regardless of the result, as observe specifies.
pub fn report<T>(
    observer: &mut Observe,
    correlation_id: u64,
    revision: u64,
    result: &Result<T, WebError>,
    cancel: &CancellationToken,
    sink: &mut (impl SinkPort + ?Sized),
) -> Result<Receipt, ObserveError> {
    observer.emit(
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
