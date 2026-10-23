//! Ferric-owned, bounded FFmpeg/ffprobe supervision prerequisite.
//!
//! This crate is not a shipped product capability. It consumes only a
//! [`fforager_contracts::FfmpegValidatedInvocationV1`] obtained from the
//! versioned contract and concentrates operating-system process control in the
//! smallest platform modules authorized by `FF-DEC-003`.

mod bounded_io;
mod identity;
mod platform;
mod progress;
mod proof_producer;
mod report;
mod supervisor;

pub use bounded_io::{
    BoundedDrainLimits, BoundedDrainResult, DiagnosticLoss, ProgressDrainError,
    ProgressRuntimeLimits, drain_progress_supervised,
};
pub use identity::{ExecutableCapabilityObservation, ExecutableObservation, observe_executable};
pub use progress::{ProgressError, ProgressLimits, ProgressSummary, replay_progress_transcript};
pub use proof_producer::{ProofProducerError, produce_platform_proof_from_environment};
pub use report::{
    ContainmentObservationsV1, DirectWaitReceiptEvidenceV1, ForcedWaitReceiptEvidenceV1,
    PhaseDeadlineObservationsV2, PlatformProofReportV2, ProducerPhaseLimitsV2,
    ProgressObservationsV1,
};
pub use supervisor::{
    CancellationProfile, FfmpegExecutionEvidence, FfmpegSupervisor, SupervisorError,
    SupervisorTrustedContext, TrustedDirectoryPin,
};
