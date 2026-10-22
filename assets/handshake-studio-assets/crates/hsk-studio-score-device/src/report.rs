use crate::error::DeviceError;
use crate::provider::CloseReport;
use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{FailureCode, Observation, Observe, Outcome, Receipt, SinkPort};

/// Outcome class of one stream lifetime or refused open.
///
/// - `Err(e)` -> `e.outcome()`.
/// - `Ok` with a lost device, dropped capture frames or frames that do not reconcile ->
///   Failure(Loss). Output underruns are not a failure (silence is the defined safe output); they
///   stay visible in the counters.
/// - `Ok` whose backend was not quiescent or whose permit was not released -> Failure(Unavailable).
/// - Otherwise Success.
pub fn outcome_of(result: &Result<CloseReport, DeviceError>) -> Outcome {
    match result {
        Err(e) => e.outcome(),
        Ok(r) if r.device_lost || r.dropped_frames > 0 || !r.reconciles() => {
            Outcome::Failure(FailureCode::Loss)
        }
        Ok(r) if !r.thread_joined || !r.permits_released => {
            Outcome::Failure(FailureCode::Unavailable)
        }
        Ok(_) => Outcome::Success,
    }
}

/// Emits the terminal diagnostic for one stream through the caller's Observe/sink.
/// `correlation_id` and `revision` must equal the values `obs` was created with. No device name,
/// project text or sample data is ever attached.
pub fn report(
    result: &Result<CloseReport, DeviceError>,
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
