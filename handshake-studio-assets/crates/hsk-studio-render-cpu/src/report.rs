//! Terminal observability for a render: exactly one Observe terminal frame per call, carrying only
//! the closed outcome vocabulary (no pixel, profile, path or error text).
use crate::{RenderError, RenderReceipt};
use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{Observation, Observe, Outcome, Receipt, SinkPort};

/// Emit the terminal frame for `result`. `correlation_id` and `revision` must be the values the
/// `Observe` was created with; a cancelled `cancel` token is rewritten to `Canceled` by Observe.
pub fn report(
    result: &Result<RenderReceipt, RenderError>,
    correlation_id: u64,
    revision: u64,
    cancel: &CancellationToken,
    observe: &mut Observe,
    sink: &mut (impl SinkPort + ?Sized),
) -> Result<Receipt, hsk_studio_observe::Error> {
    let outcome = match result {
        Ok(_) => Outcome::Success,
        Err(error) => error.outcome(),
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
