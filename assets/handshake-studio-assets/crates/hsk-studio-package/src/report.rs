//! Maps package results onto the closed Observe outcome vocabulary. Only the outcome class is
//! emitted: never entry names, paths, document text or error detail.
use crate::{
    error::{AssetError, PackageError},
    read::Opened,
};
use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{Error, FailureCode, Observation, Observe, Outcome, Receipt, SinkPort};

/// Ok -> Success; Canceled -> Canceled; absent/unauthorized asset, missing asset and I/O ->
/// Failure(Unavailable); every other rejection (hostile input, hash, version, limits) ->
/// Failure(Validation).
pub fn outcome_for<T>(result: &Result<T, PackageError>) -> Outcome {
    match result {
        Ok(_) => Outcome::Success,
        Err(PackageError::Canceled) => Outcome::Canceled,
        Err(
            PackageError::Asset(AssetError::Absent | AssetError::Unauthorized)
            | PackageError::MissingAsset
            | PackageError::Io(_),
        ) => Outcome::Failure(FailureCode::Unavailable),
        Err(_) => Outcome::Failure(FailureCode::Validation),
    }
}

/// As [`outcome_for`]; quarantined content is still Success (the caller reads `loss`), unless
/// the caller's policy makes loss an error, which reports Failure(Loss).
pub fn outcome_for_opened(result: &Result<Opened, PackageError>, loss_is_error: bool) -> Outcome {
    match result {
        Ok(opened) if loss_is_error && !opened.loss.is_empty() => {
            Outcome::Failure(FailureCode::Loss)
        }
        other => outcome_for(other),
    }
}

/// Emits one terminal observation through the caller-owned sink.
pub fn report(
    outcome: Outcome,
    correlation_id: u64,
    revision: u64,
    cancel: &CancellationToken,
    observe: &mut Observe,
    sink: &mut (impl SinkPort + ?Sized),
) -> Result<Receipt, Error> {
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
