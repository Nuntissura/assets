//! Bounded deterministic lifecycle transition models and trace replay.

use crate::resource::{ByteCreditPosition, CreditError, OwnedByteCreditBroker};
use fforager_contracts::{
    CleanupObservation, CollisionObservation, CommitState, IdentityObservation, LeaseObservation,
    MigrationObservation, RecoveryAction, RecoveryConfinementObservation, RecoveryDecision,
    RecoveryFailure, RecoveryObservation, VolumeRelationship,
};

/// Selects one deterministic recovery action from observed durable state.
///
/// This is a pure recovery oracle. It models what a later adapter must do and
/// never claims that a file, journal, directory entry, lease, or archive row was
/// actually persisted. Use [`apply_recovery_action`] to model acknowledged action
/// application, repeat safety, and post-action convergence.
#[must_use]
pub fn decide_recovery(observation: &RecoveryObservation) -> RecoveryDecision {
    if observation.durable_prefix == CommitState::Inconsistent {
        return RecoveryDecision::FailClosed(RecoveryFailure::ExistingInconsistentPrefix);
    }
    if observation.collision == CollisionObservation::ConflictingExistingOutput {
        return RecoveryDecision::FailClosed(RecoveryFailure::ConflictingDestination);
    }

    if observation.staged_output == IdentityObservation::Mismatched {
        return RecoveryDecision::FailClosed(RecoveryFailure::MismatchedStagedOutput);
    }
    if observation.final_output == IdentityObservation::Mismatched {
        return RecoveryDecision::FailClosed(RecoveryFailure::MismatchedFinalOutput);
    }
    if observation.archive_row == IdentityObservation::Mismatched {
        return RecoveryDecision::FailClosed(RecoveryFailure::MismatchedArchiveRow);
    }
    match observation.confinement {
        RecoveryConfinementObservation::Unavailable => {
            return RecoveryDecision::FailClosed(RecoveryFailure::ConfinementUnavailable);
        }
        RecoveryConfinementObservation::Mismatched => {
            return RecoveryDecision::FailClosed(RecoveryFailure::ConfinementMismatched);
        }
        RecoveryConfinementObservation::Proven => {}
    }

    if matches!(
        observation.cleanup,
        CleanupObservation::Partial | CleanupObservation::Complete
    ) && matches!(
        observation.durable_prefix,
        CommitState::Collecting | CommitState::Prepared | CommitState::Renamed
    ) {
        return RecoveryDecision::FailClosed(RecoveryFailure::CleanupBeforeArchive);
    }

    if observation.durable_prefix == CommitState::Collecting
        && (observation.final_output == IdentityObservation::MatchesJournal
            || observation.archive_row == IdentityObservation::MatchesJournal)
    {
        return RecoveryDecision::FailClosed(RecoveryFailure::UnexpectedArtifactBeforePrepared);
    }

    match observation.lease {
        LeaseObservation::HeldByOther => {
            return RecoveryDecision::FailClosed(RecoveryFailure::LeaseHeldByOther);
        }
        LeaseObservation::Stale => {
            return RecoveryDecision::Act(RecoveryAction::ReclaimStaleLease);
        }
        LeaseObservation::NotPresent | LeaseObservation::OwnedByJob => {}
    }

    if observation.migration == MigrationObservation::Interrupted {
        return RecoveryDecision::Act(RecoveryAction::ResumeInterruptedMigration);
    }

    if observation.archive_row == IdentityObservation::MatchesJournal
        && observation.final_output == IdentityObservation::Missing
    {
        return if observation.staged_output == IdentityObservation::MatchesJournal {
            RecoveryDecision::Act(RecoveryAction::RestoreFinalFromStaged)
        } else {
            RecoveryDecision::FailClosed(RecoveryFailure::ArchiveWithoutRecoverableOutput)
        };
    }

    decide_recovery_prefix(observation)
}

/// Failure to apply a declared recovery action to an observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryApplicationError {
    ActionNotApplicable {
        action: RecoveryAction,
    },
    RetryActionMismatch {
        applied: RecoveryAction,
        requested: RecoveryAction,
    },
}

/// Opaque proof that one oracle-selected recovery action was applied.
///
/// Private immutable fields bind the selected action to both its prior and
/// resulting observations. Callers cannot construct or mutate an application;
/// they may inspect its lineage or retry the exact action through [`Self::retry`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryApplication {
    action: RecoveryAction,
    prior_observation: RecoveryObservation,
    resulting_observation: RecoveryObservation,
}

impl RecoveryApplication {
    #[must_use]
    pub const fn action(&self) -> RecoveryAction {
        self.action
    }

    #[must_use]
    pub const fn prior_observation(&self) -> &RecoveryObservation {
        &self.prior_observation
    }

    #[must_use]
    pub const fn resulting_observation(&self) -> &RecoveryObservation {
        &self.resulting_observation
    }

    #[must_use]
    pub fn into_result(self) -> RecoveryObservation {
        self.resulting_observation
    }

    /// Return the identical stored result for a retry of the exact applied action.
    ///
    /// This does not execute the recovery mutation again. The opaque application
    /// provides the immutable prior/result lineage and this method verifies that
    /// the requested action is the one that created it.
    ///
    /// # Errors
    ///
    /// Returns [`RecoveryApplicationError::RetryActionMismatch`] when `action`
    /// differs from the oracle-selected action bound into this application.
    pub fn retry(
        &self,
        action: RecoveryAction,
    ) -> Result<&RecoveryObservation, RecoveryApplicationError> {
        if action != self.action {
            return Err(RecoveryApplicationError::RetryActionMismatch {
                applied: self.action,
                requested: action,
            });
        }
        Ok(&self.resulting_observation)
    }
}

#[cfg(test)]
mod ffmpeg_v2_tests {
    use super::*;

    fn instance() -> MachineInstanceId {
        MachineInstanceId::new(77).expect("nonzero test instance")
    }

    fn model() -> FfmpegLifecycleV2 {
        FfmpegLifecycleV2::new(
            instance(),
            FfmpegLifecycleLimitsV2::new(2, 2, 2, 2).expect("nonzero cleanup limits"),
            64,
        )
    }

    fn pending(model: &FfmpegLifecycleV2) -> FfmpegEffectRequestV2 {
        model
            .pending_effect()
            .expect("one ordered effect must be pending")
    }

    fn complete(model: &mut FfmpegLifecycleV2, outcome: FfmpegEffectOutcomeV2) {
        let request = pending(model);
        model
            .complete_effect(request, outcome)
            .expect("correlated effect completion");
    }

    fn running() -> FfmpegLifecycleV2 {
        let mut model = model();
        model.start().expect("spawn request");
        complete(&mut model, FfmpegEffectOutcomeV2::Spawned);
        assert_eq!(model.state(), FfmpegLifecycleStateV2::Running);
        model
    }

    fn reaped_zero() -> FfmpegLifecycleV2 {
        let mut model = running();
        model.observe_child_exit().expect("exit observation");
        complete(
            &mut model,
            FfmpegEffectOutcomeV2::Reap(FfmpegReapOutcomeV2::Reaped(
                FfmpegTerminationV2::ExitCode(0),
            )),
        );
        assert_eq!(model.state(), FfmpegLifecycleStateV2::DiagnosticsPending);
        model
    }

    #[test]
    fn success_orders_reap_diagnostics_validation_and_release() {
        let mut model = reaped_zero();
        assert_eq!(model.reap_outcome(), Some(FfmpegTerminationV2::ExitCode(0)));
        complete(&mut model, FfmpegEffectOutcomeV2::DiagnosticsPreserved);
        assert_eq!(pending(&model).effect, FfmpegEffectV2::ValidateOutput);
        complete(&mut model, FfmpegEffectOutcomeV2::OutputValidated);
        assert_eq!(pending(&model).effect, FfmpegEffectV2::ReleaseResources);
        assert!(!model.resources_released());
        complete(&mut model, FfmpegEffectOutcomeV2::ResourcesReleased);
        assert_eq!(model.state(), FfmpegLifecycleStateV2::Exited);
        assert!(model.diagnostics_preserved());
        assert!(model.resources_released());
        assert_eq!(model.terminal_failure(), None);
    }

    #[test]
    fn reaped_termination_is_not_wait_failure_or_retried() {
        let cases = [
            (
                FfmpegTerminationV2::ExitCode(23),
                FfmpegTerminalFailureV2::NonzeroExit(23),
            ),
            (
                FfmpegTerminationV2::UnixSignal(9),
                FfmpegTerminalFailureV2::UnixSignal(9),
            ),
            (
                FfmpegTerminationV2::WindowsStatusOpaque(0xC000_0005),
                FfmpegTerminalFailureV2::WindowsStatusOpaque(0xC000_0005),
            ),
            (
                FfmpegTerminationV2::WindowsException(0xC000_0005),
                FfmpegTerminalFailureV2::WindowsException(0xC000_0005),
            ),
        ];
        for (termination, expected_failure) in cases {
            let mut model = running();
            model.observe_child_exit().expect("exit observation");
            complete(
                &mut model,
                FfmpegEffectOutcomeV2::Reap(FfmpegReapOutcomeV2::Reaped(termination)),
            );
            assert_eq!(pending(&model).effect, FfmpegEffectV2::PreserveDiagnostics);
            assert_eq!(
                model
                    .trace()
                    .iter()
                    .filter(|transition| transition.issued.is_some_and(|request| {
                        request.effect == FfmpegEffectV2::ReapDirectChild
                    }))
                    .count(),
                1,
                "a successfully reaped termination must never retry wait/reap"
            );
            complete(&mut model, FfmpegEffectOutcomeV2::DiagnosticsPreserved);
            assert_eq!(model.terminal_failure(), Some(expected_failure));
            assert_eq!(pending(&model).effect, FfmpegEffectV2::ReleaseResources);
            complete(&mut model, FfmpegEffectOutcomeV2::ResourcesReleased);
            assert_eq!(model.state(), FfmpegLifecycleStateV2::Failed);
        }
    }

    #[test]
    fn wait_failure_retries_only_to_bound_and_never_releases_early() {
        let wait_failure = FfmpegWaitFailureV2 { os_code: Some(6) };
        let mut model = running();
        model.observe_child_exit().expect("exit observation");
        complete(
            &mut model,
            FfmpegEffectOutcomeV2::Reap(FfmpegReapOutcomeV2::WaitFailed(wait_failure)),
        );
        assert_eq!(model.state(), FfmpegLifecycleStateV2::ReapPending);
        assert_eq!(pending(&model).effect, FfmpegEffectV2::ReapDirectChild);
        complete(
            &mut model,
            FfmpegEffectOutcomeV2::Reap(FfmpegReapOutcomeV2::WaitFailed(wait_failure)),
        );
        assert_eq!(model.state(), FfmpegLifecycleStateV2::DiagnosticsPending);
        assert!(!model.resources_released());
        assert_eq!(
            model.terminal_failure(),
            Some(FfmpegTerminalFailureV2::WaitFailed(wait_failure))
        );
        complete(&mut model, FfmpegEffectOutcomeV2::DiagnosticsPreserved);
        assert_eq!(model.state(), FfmpegLifecycleStateV2::Failed);
        assert!(model.pending_effect().is_none());
        assert!(!model.resources_released());

        let mut cancelled = running();
        cancelled.observe_child_exit().expect("exit observation");
        cancelled
            .request_cancellation()
            .expect("cancel while reap is pending");
        for _ in 0..2 {
            complete(
                &mut cancelled,
                FfmpegEffectOutcomeV2::Reap(FfmpegReapOutcomeV2::WaitFailed(wait_failure)),
            );
        }
        complete(&mut cancelled, FfmpegEffectOutcomeV2::DiagnosticsPreserved);
        assert_eq!(cancelled.state(), FfmpegLifecycleStateV2::Cancelled);
        assert_eq!(
            cancelled.terminal_failure(),
            Some(FfmpegTerminalFailureV2::WaitFailed(wait_failure))
        );
        assert!(!cancelled.resources_released());
    }

    #[test]
    fn cancellation_is_sticky_during_every_pending_effect() {
        let mut cancelling = model();
        cancelling.start().expect("spawn request");
        let spawn = pending(&cancelling);
        cancelling
            .request_cancellation()
            .expect("cancel during spawn");
        assert_eq!(pending(&cancelling), spawn);
        complete(&mut cancelling, FfmpegEffectOutcomeV2::Spawned);
        assert_eq!(
            cancelling.state(),
            FfmpegLifecycleStateV2::GracefulStopPending
        );

        let graceful = pending(&cancelling);
        cancelling
            .request_cancellation()
            .expect("repeat cancel during graceful request");
        assert_eq!(pending(&cancelling), graceful);
        complete(
            &mut cancelling,
            FfmpegEffectOutcomeV2::GracefulStopUnsupported,
        );

        let kill = pending(&cancelling);
        cancelling
            .request_cancellation()
            .expect("cancel during kill");
        assert_eq!(pending(&cancelling), kill);
        complete(&mut cancelling, FfmpegEffectOutcomeV2::ForcedKillRequested);

        let reap = pending(&cancelling);
        cancelling
            .request_cancellation()
            .expect("cancel during reap");
        assert_eq!(pending(&cancelling), reap);
        complete(
            &mut cancelling,
            FfmpegEffectOutcomeV2::Reap(FfmpegReapOutcomeV2::Reaped(
                FfmpegTerminationV2::ExitCode(0),
            )),
        );

        let diagnostics = pending(&cancelling);
        cancelling
            .request_cancellation()
            .expect("cancel during diagnostics");
        assert_eq!(pending(&cancelling), diagnostics);
        complete(&mut cancelling, FfmpegEffectOutcomeV2::DiagnosticsPreserved);
        assert_eq!(
            pending(&cancelling).effect,
            FfmpegEffectV2::ReleaseResources,
            "sticky cancellation skips output validation"
        );

        let release = pending(&cancelling);
        cancelling
            .request_cancellation()
            .expect("cancel during release");
        assert_eq!(pending(&cancelling), release);
        complete(&mut cancelling, FfmpegEffectOutcomeV2::ResourcesReleased);
        assert_eq!(cancelling.state(), FfmpegLifecycleStateV2::Cancelled);
        assert!(cancelling.cancellation_requested());

        let mut validating = reaped_zero();
        complete(&mut validating, FfmpegEffectOutcomeV2::DiagnosticsPreserved);
        let validation = pending(&validating);
        validating
            .request_cancellation()
            .expect("cancel during validation");
        assert_eq!(pending(&validating), validation);
        complete(&mut validating, FfmpegEffectOutcomeV2::OutputValidated);
        complete(&mut validating, FfmpegEffectOutcomeV2::ResourcesReleased);
        assert_eq!(validating.state(), FfmpegLifecycleStateV2::Cancelled);
    }

    #[test]
    fn exit_kill_and_reap_races_never_kill_after_reap() {
        let mut graceful_race = running();
        graceful_race
            .request_cancellation()
            .expect("cancel running child");
        graceful_race
            .observe_child_exit()
            .expect("exit races with graceful request");
        complete(
            &mut graceful_race,
            FfmpegEffectOutcomeV2::GracefulStopUnsupported,
        );
        assert_eq!(
            pending(&graceful_race).effect,
            FfmpegEffectV2::ReapDirectChild
        );
        assert!(!graceful_race.trace().iter().any(|transition| {
            transition
                .issued
                .is_some_and(|request| request.effect == FfmpegEffectV2::ForceKillProcessTree)
        }));

        let mut kill_race = running();
        kill_race
            .request_cancellation()
            .expect("cancel running child");
        complete(
            &mut kill_race,
            FfmpegEffectOutcomeV2::GracefulStopUnsupported,
        );
        kill_race
            .observe_child_exit()
            .expect("exit races with forced kill");
        complete(
            &mut kill_race,
            FfmpegEffectOutcomeV2::ForcedKillAlreadyExited,
        );
        complete(
            &mut kill_race,
            FfmpegEffectOutcomeV2::Reap(FfmpegReapOutcomeV2::Reaped(
                FfmpegTerminationV2::ExitCode(0),
            )),
        );
        let force_count_before = kill_race
            .trace()
            .iter()
            .filter(|transition| {
                transition
                    .issued
                    .is_some_and(|request| request.effect == FfmpegEffectV2::ForceKillProcessTree)
            })
            .count();
        kill_race
            .observe_child_exit()
            .expect("late exit observation is idempotent");
        let force_count_after = kill_race
            .trace()
            .iter()
            .filter(|transition| {
                transition
                    .issued
                    .is_some_and(|request| request.effect == FfmpegEffectV2::ForceKillProcessTree)
            })
            .count();
        assert_eq!(force_count_after, force_count_before);
        assert_eq!(
            kill_race.state(),
            FfmpegLifecycleStateV2::DiagnosticsPending
        );
    }

    #[test]
    fn cleanup_failures_are_bounded_correlated_and_idempotent() {
        let mut kill_failure = running();
        kill_failure
            .request_cancellation()
            .expect("cancel running child");
        complete(
            &mut kill_failure,
            FfmpegEffectOutcomeV2::GracefulStopUnsupported,
        );
        let first_kill = pending(&kill_failure);
        complete(&mut kill_failure, FfmpegEffectOutcomeV2::ForcedKillFailed);
        let second_kill = pending(&kill_failure);
        assert_ne!(first_kill.generation, second_kill.generation);
        assert!(matches!(
            kill_failure.complete_effect(first_kill, FfmpegEffectOutcomeV2::ForcedKillFailed),
            Err(FfmpegLifecycleErrorV2::UnexpectedEffectCompletion { .. })
        ));
        complete(&mut kill_failure, FfmpegEffectOutcomeV2::ForcedKillFailed);
        assert_eq!(
            pending(&kill_failure).effect,
            FfmpegEffectV2::PreserveDiagnostics
        );
        complete(&mut kill_failure, FfmpegEffectOutcomeV2::DiagnosticsFailed);
        assert_eq!(
            pending(&kill_failure).effect,
            FfmpegEffectV2::PreserveDiagnostics
        );
        complete(&mut kill_failure, FfmpegEffectOutcomeV2::DiagnosticsFailed);
        assert_eq!(kill_failure.state(), FfmpegLifecycleStateV2::Cancelled);
        assert_eq!(
            kill_failure.terminal_failure(),
            Some(FfmpegTerminalFailureV2::ForcedKillFailed)
        );
        assert!(kill_failure.pending_effect().is_none());
        assert!(!kill_failure.resources_released());

        let mut diagnostic_failure = reaped_zero();
        complete(
            &mut diagnostic_failure,
            FfmpegEffectOutcomeV2::DiagnosticsFailed,
        );
        complete(
            &mut diagnostic_failure,
            FfmpegEffectOutcomeV2::DiagnosticsFailed,
        );
        assert_eq!(
            diagnostic_failure.terminal_failure(),
            Some(FfmpegTerminalFailureV2::DiagnosticsFailed)
        );
        complete(
            &mut diagnostic_failure,
            FfmpegEffectOutcomeV2::OutputValidated,
        );
        complete(
            &mut diagnostic_failure,
            FfmpegEffectOutcomeV2::ResourcesReleased,
        );
        assert_eq!(diagnostic_failure.state(), FfmpegLifecycleStateV2::Failed);

        let mut release_failure = reaped_zero();
        complete(
            &mut release_failure,
            FfmpegEffectOutcomeV2::DiagnosticsPreserved,
        );
        complete(&mut release_failure, FfmpegEffectOutcomeV2::OutputValidated);
        let first_release = pending(&release_failure);
        complete(
            &mut release_failure,
            FfmpegEffectOutcomeV2::ResourceReleaseFailed,
        );
        assert_ne!(
            pending(&release_failure).generation,
            first_release.generation
        );
        complete(
            &mut release_failure,
            FfmpegEffectOutcomeV2::ResourceReleaseFailed,
        );
        assert_eq!(release_failure.state(), FfmpegLifecycleStateV2::Failed);
        assert_eq!(
            release_failure.terminal_failure(),
            Some(FfmpegTerminalFailureV2::ResourceReleaseFailed)
        );
        assert!(!release_failure.resources_released());
    }
}

/// Applies one successfully acknowledged recovery action to the pure observation model.
///
/// Initial application requires the exact action selected by [`decide_recovery`].
/// Successful application returns an opaque [`RecoveryApplication`] that binds
/// the selected action, prior observation, and resulting observation. Retry
/// safety is available only through [`RecoveryApplication::retry`].
///
/// # Errors
///
/// Returns [`RecoveryApplicationError::ActionNotApplicable`] when the observation
/// cannot be the before-or-after state of the requested action.
#[allow(clippy::too_many_lines)]
pub fn apply_recovery_action(
    observation: &RecoveryObservation,
    action: RecoveryAction,
) -> Result<RecoveryApplication, RecoveryApplicationError> {
    if decide_recovery(observation) != RecoveryDecision::Act(action) {
        return Err(RecoveryApplicationError::ActionNotApplicable { action });
    }
    let mut next = observation.clone();
    match action {
        RecoveryAction::ResumeOrRestartVerifiedData => {}
        RecoveryAction::ReclaimStaleLease => {
            next.lease = LeaseObservation::OwnedByJob;
        }
        RecoveryAction::ResumeInterruptedMigration => {
            next.migration = MigrationObservation::Complete;
        }
        RecoveryAction::RevalidatePreparedThenRename
        | RecoveryAction::CopySyncRenameWithinDestination => {
            next.durable_prefix = CommitState::Renamed;
            next.staged_output = IdentityObservation::Missing;
            next.final_output = IdentityObservation::MatchesJournal;
        }
        RecoveryAction::VerifyFinalArtifactThenArchive | RecoveryAction::InsertArchiveRow => {
            next.durable_prefix = CommitState::Archived;
            next.archive_row = IdentityObservation::MatchesJournal;
        }
        RecoveryAction::RestoreFinalFromStaged => {
            next.final_output = IdentityObservation::MatchesJournal;
        }
        RecoveryAction::VerifyArchiveOutputPair => {
            next.durable_prefix = CommitState::Archived;
        }
        RecoveryAction::RepeatCleanup => {
            next.durable_prefix = CommitState::Cleaned;
            next.cleanup = CleanupObservation::Complete;
        }
    }
    Ok(RecoveryApplication {
        action,
        prior_observation: observation.clone(),
        resulting_observation: next,
    })
}

fn decide_recovery_prefix(observation: &RecoveryObservation) -> RecoveryDecision {
    match observation.durable_prefix {
        CommitState::Collecting => {
            if observation.final_output == IdentityObservation::MatchesJournal
                || observation.archive_row == IdentityObservation::MatchesJournal
            {
                RecoveryDecision::FailClosed(RecoveryFailure::UnexpectedArtifactBeforePrepared)
            } else {
                RecoveryDecision::Act(RecoveryAction::ResumeOrRestartVerifiedData)
            }
        }
        CommitState::Prepared => {
            if observation.archive_row == IdentityObservation::MatchesJournal
                && observation.final_output == IdentityObservation::MatchesJournal
            {
                RecoveryDecision::Act(RecoveryAction::VerifyArchiveOutputPair)
            } else if observation.final_output == IdentityObservation::MatchesJournal {
                RecoveryDecision::Act(RecoveryAction::VerifyFinalArtifactThenArchive)
            } else if observation.staged_output != IdentityObservation::MatchesJournal {
                RecoveryDecision::FailClosed(RecoveryFailure::PreparedOutputMissing)
            } else if observation.volume_relationship == VolumeRelationship::CrossVolume {
                RecoveryDecision::Act(RecoveryAction::CopySyncRenameWithinDestination)
            } else {
                RecoveryDecision::Act(RecoveryAction::RevalidatePreparedThenRename)
            }
        }
        CommitState::Renamed => {
            if observation.final_output != IdentityObservation::MatchesJournal {
                RecoveryDecision::FailClosed(RecoveryFailure::RenamedOutputMissing)
            } else if observation.archive_row == IdentityObservation::MatchesJournal {
                RecoveryDecision::Act(RecoveryAction::VerifyArchiveOutputPair)
            } else {
                RecoveryDecision::Act(RecoveryAction::InsertArchiveRow)
            }
        }
        CommitState::Archived => {
            if observation.archive_row != IdentityObservation::MatchesJournal {
                RecoveryDecision::FailClosed(RecoveryFailure::ArchiveMissingForDurablePrefix)
            } else if observation.final_output != IdentityObservation::MatchesJournal {
                RecoveryDecision::FailClosed(RecoveryFailure::RenamedOutputMissing)
            } else {
                RecoveryDecision::Act(RecoveryAction::RepeatCleanup)
            }
        }
        CommitState::Cleaned => {
            if observation.archive_row != IdentityObservation::MatchesJournal {
                RecoveryDecision::FailClosed(RecoveryFailure::ArchiveMissingForDurablePrefix)
            } else if observation.final_output != IdentityObservation::MatchesJournal {
                RecoveryDecision::FailClosed(RecoveryFailure::RenamedOutputMissing)
            } else if observation.cleanup == CleanupObservation::Complete {
                RecoveryDecision::ReconciledSuccess
            } else {
                RecoveryDecision::Act(RecoveryAction::RepeatCleanup)
            }
        }
        CommitState::Inconsistent => {
            RecoveryDecision::FailClosed(RecoveryFailure::ExistingInconsistentPrefix)
        }
    }
}

/// Versioned limits for cleanup work owned by the `FFmpeg` supervisor.
///
/// Each value is an attempt ceiling rather than a duration. The effect adapter
/// owns the corresponding bounded deadline; this pure model guarantees that a
/// failing cleanup acknowledgement cannot cause an infinite retry loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FfmpegLifecycleLimitsV2 {
    force_kill: u8,
    reap: u8,
    diagnostics: u8,
    release: u8,
}

impl FfmpegLifecycleLimitsV2 {
    /// Construct nonzero cleanup attempt ceilings.
    #[must_use]
    pub const fn new(
        max_force_kill_attempts: u8,
        max_reap_attempts: u8,
        max_diagnostic_attempts: u8,
        max_release_attempts: u8,
    ) -> Option<Self> {
        if max_force_kill_attempts == 0
            || max_reap_attempts == 0
            || max_diagnostic_attempts == 0
            || max_release_attempts == 0
        {
            None
        } else {
            Some(Self {
                force_kill: max_force_kill_attempts,
                reap: max_reap_attempts,
                diagnostics: max_diagnostic_attempts,
                release: max_release_attempts,
            })
        }
    }
}

/// Public states for the version-two `FFmpeg` lifecycle boundary.
///
/// Version two deliberately supersedes the legacy `MachineKind::Ffmpeg` model
/// for process adapters. It serializes every cleanup effect, retains sticky
/// cancellation separately from phase, and exposes typed reap evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfmpegLifecycleStateV2 {
    Prepared,
    Spawning,
    Running,
    GracefulStopPending,
    GracefulExitPending,
    ForcedKillPending,
    ReapPending,
    DiagnosticsPending,
    OutputValidationPending,
    ReleasePending,
    Exited,
    Failed,
    Cancelled,
}

/// Ordered adapter effects emitted by [`FfmpegLifecycleV2`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfmpegEffectV2 {
    SpawnProcess,
    RequestGracefulStop,
    ForceKillProcessTree,
    ReapDirectChild,
    PreserveDiagnostics,
    ValidateOutput,
    ReleaseResources,
}

/// Exact termination returned by one successful direct-child wait/reap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfmpegTerminationV2 {
    ExitCode(i32),
    UnixSignal(i32),
    /// Numeric Windows termination status without debugger-proven provenance.
    WindowsStatusOpaque(u32),
    /// Windows exception proven by an independent debugger/event boundary.
    WindowsException(u32),
}

/// Bounded OS wait failure. `os_code` is absent only when the adapter has no
/// stable numeric platform error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FfmpegWaitFailureV2 {
    pub os_code: Option<i64>,
}

/// Typed direct-child wait/reap result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfmpegReapOutcomeV2 {
    /// The direct child was successfully reaped, regardless of termination status.
    Reaped(FfmpegTerminationV2),
    /// The wait/reap operation itself failed and may be retried within the limit.
    WaitFailed(FfmpegWaitFailureV2),
}

/// Typed completion of one exact pending `FFmpeg` lifecycle effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfmpegEffectOutcomeV2 {
    Spawned,
    SpawnFailed,
    GracefulStopRequested,
    GracefulStopUnsupported,
    GracefulStopFailed,
    ForcedKillRequested,
    ForcedKillAlreadyExited,
    ForcedKillFailed,
    Reap(FfmpegReapOutcomeV2),
    DiagnosticsPreserved,
    DiagnosticsFailed,
    OutputValidated,
    OutputInvalid,
    OutputValidationFailed,
    ResourcesReleased,
    ResourceReleaseFailed,
}

/// Stable correlation token for one `FFmpeg` adapter effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FfmpegEffectRequestV2 {
    pub instance_id: MachineInstanceId,
    pub generation: u64,
    pub effect: FfmpegEffectV2,
}

/// Terminal failure classification retained independently from cancellation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfmpegTerminalFailureV2 {
    SpawnFailed,
    ForcedKillFailed,
    WaitFailed(FfmpegWaitFailureV2),
    NonzeroExit(i32),
    UnixSignal(i32),
    WindowsStatusOpaque(u32),
    WindowsException(u32),
    DiagnosticsFailed,
    OutputInvalid,
    OutputValidationFailed,
    ResourceReleaseFailed,
}

/// Public input recorded in the bounded lifecycle trace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfmpegLifecycleActionV2 {
    Start,
    Cancel,
    ChildExitObserved,
    GracePeriodExpired,
    EffectCompleted {
        request: FfmpegEffectRequestV2,
        outcome: FfmpegEffectOutcomeV2,
    },
}

/// One public state/effect transition from the version-two `FFmpeg` lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FfmpegLifecycleTransitionV2 {
    pub previous: FfmpegLifecycleStateV2,
    pub action: FfmpegLifecycleActionV2,
    pub next: FfmpegLifecycleStateV2,
    pub issued: Option<FfmpegEffectRequestV2>,
}

/// Rejection from the version-two `FFmpeg` lifecycle boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfmpegLifecycleErrorV2 {
    InvalidTransition {
        state: FfmpegLifecycleStateV2,
        action: FfmpegLifecycleActionV2,
    },
    UnexpectedEffectCompletion {
        received: FfmpegEffectRequestV2,
        expected: Option<FfmpegEffectRequestV2>,
    },
    OutcomeDoesNotMatchEffect {
        effect: FfmpegEffectV2,
        outcome: FfmpegEffectOutcomeV2,
    },
    TraceLimitReached {
        limit: usize,
    },
    EffectGenerationExhausted,
}

/// Pure, bounded `FFmpeg` lifecycle model used by platform process adapters.
///
/// The machine has at most one pending effect. Cancellation is a sticky intent,
/// not a competing state transition, so it cannot be lost while spawn, stop,
/// kill, reap, diagnostics, validation, or release is pending.
#[derive(Debug)]
pub struct FfmpegLifecycleV2 {
    instance_id: MachineInstanceId,
    state: FfmpegLifecycleStateV2,
    limits: FfmpegLifecycleLimitsV2,
    trace_limit: usize,
    trace: Vec<FfmpegLifecycleTransitionV2>,
    pending: Option<FfmpegEffectRequestV2>,
    next_generation: u64,
    cancellation_requested: FfmpegFactV2,
    exit_observed: FfmpegFactV2,
    child_spawned: FfmpegFactV2,
    force_kill_attempts: u8,
    reap_attempts: u8,
    diagnostic_attempts: u8,
    release_attempts: u8,
    reaped: Option<FfmpegTerminationV2>,
    failure: Option<FfmpegTerminalFailureV2>,
    cleanup_unproven: FfmpegFactV2,
    diagnostics_preserved: FfmpegFactV2,
    resources_released: FfmpegFactV2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FfmpegFactV2 {
    Absent,
    Present,
}

impl FfmpegFactV2 {
    const fn is_present(self) -> bool {
        matches!(self, Self::Present)
    }
}

impl FfmpegLifecycleV2 {
    /// Construct one prepared supervisor with a bounded trace and cleanup policy.
    #[must_use]
    pub const fn new(
        instance_id: MachineInstanceId,
        limits: FfmpegLifecycleLimitsV2,
        trace_limit: usize,
    ) -> Self {
        Self {
            instance_id,
            state: FfmpegLifecycleStateV2::Prepared,
            limits,
            trace_limit,
            trace: Vec::new(),
            pending: None,
            next_generation: 1,
            cancellation_requested: FfmpegFactV2::Absent,
            exit_observed: FfmpegFactV2::Absent,
            child_spawned: FfmpegFactV2::Absent,
            force_kill_attempts: 0,
            reap_attempts: 0,
            diagnostic_attempts: 0,
            release_attempts: 0,
            reaped: None,
            failure: None,
            cleanup_unproven: FfmpegFactV2::Absent,
            diagnostics_preserved: FfmpegFactV2::Absent,
            resources_released: FfmpegFactV2::Absent,
        }
    }

    #[must_use]
    pub const fn state(&self) -> FfmpegLifecycleStateV2 {
        self.state
    }

    #[must_use]
    pub const fn cancellation_requested(&self) -> bool {
        self.cancellation_requested.is_present()
    }

    #[must_use]
    pub const fn pending_effect(&self) -> Option<FfmpegEffectRequestV2> {
        self.pending
    }

    #[must_use]
    pub fn trace(&self) -> &[FfmpegLifecycleTransitionV2] {
        &self.trace
    }

    #[must_use]
    pub const fn reap_outcome(&self) -> Option<FfmpegTerminationV2> {
        self.reaped
    }

    #[must_use]
    pub const fn terminal_failure(&self) -> Option<FfmpegTerminalFailureV2> {
        self.failure
    }

    #[must_use]
    pub const fn diagnostics_preserved(&self) -> bool {
        self.diagnostics_preserved.is_present()
    }

    #[must_use]
    pub const fn resources_released(&self) -> bool {
        self.resources_released.is_present()
    }

    /// Begin process creation.
    ///
    /// # Errors
    ///
    /// Returns an invalid-transition, trace-limit, or generation error.
    pub fn start(&mut self) -> Result<&FfmpegLifecycleTransitionV2, FfmpegLifecycleErrorV2> {
        let action = FfmpegLifecycleActionV2::Start;
        if self.state != FfmpegLifecycleStateV2::Prepared || self.pending.is_some() {
            return Err(self.invalid(action));
        }
        self.prepare_transition(true)?;
        Ok(self.record_with_effect(
            action,
            FfmpegLifecycleStateV2::Spawning,
            Some(FfmpegEffectV2::SpawnProcess),
        ))
    }

    /// Record sticky cancellation without replacing or duplicating a pending effect.
    ///
    /// # Errors
    ///
    /// Returns an invalid-transition or trace-limit error after terminal completion.
    pub fn request_cancellation(
        &mut self,
    ) -> Result<&FfmpegLifecycleTransitionV2, FfmpegLifecycleErrorV2> {
        let action = FfmpegLifecycleActionV2::Cancel;
        if self.is_terminal() {
            return Err(self.invalid(action));
        }
        let (next, effect) = if self.state == FfmpegLifecycleStateV2::Prepared {
            (
                FfmpegLifecycleStateV2::ReleasePending,
                Some(FfmpegEffectV2::ReleaseResources),
            )
        } else if self.state == FfmpegLifecycleStateV2::Running && self.pending.is_none() {
            (
                FfmpegLifecycleStateV2::GracefulStopPending,
                Some(FfmpegEffectV2::RequestGracefulStop),
            )
        } else {
            (self.state, None)
        };
        self.prepare_transition(effect.is_some())?;
        self.cancellation_requested = FfmpegFactV2::Present;
        if effect.is_some() {
            Ok(self.record_with_effect(action, next, effect))
        } else {
            Ok(self.record_observation(action, next))
        }
    }

    /// Record an independently observed child exit.
    ///
    /// The observation never substitutes for direct-child wait/reap. If stop or
    /// kill is pending it is remembered until that exact request is settled.
    ///
    /// # Errors
    ///
    /// Returns an invalid-transition, trace-limit, or generation error.
    pub fn observe_child_exit(
        &mut self,
    ) -> Result<&FfmpegLifecycleTransitionV2, FfmpegLifecycleErrorV2> {
        let action = FfmpegLifecycleActionV2::ChildExitObserved;
        let (next, effect) = match self.state {
            FfmpegLifecycleStateV2::Running | FfmpegLifecycleStateV2::GracefulExitPending
                if self.pending.is_none() =>
            {
                (
                    FfmpegLifecycleStateV2::ReapPending,
                    Some(FfmpegEffectV2::ReapDirectChild),
                )
            }
            FfmpegLifecycleStateV2::GracefulStopPending
            | FfmpegLifecycleStateV2::ForcedKillPending
            | FfmpegLifecycleStateV2::ReapPending
            | FfmpegLifecycleStateV2::DiagnosticsPending
            | FfmpegLifecycleStateV2::OutputValidationPending
            | FfmpegLifecycleStateV2::ReleasePending
            | FfmpegLifecycleStateV2::Exited
            | FfmpegLifecycleStateV2::Failed
            | FfmpegLifecycleStateV2::Cancelled => (self.state, None),
            _ => return Err(self.invalid(action)),
        };
        self.prepare_transition(effect.is_some())?;
        self.exit_observed = FfmpegFactV2::Present;
        if effect.is_some() {
            Ok(self.record_with_effect(action, next, effect))
        } else {
            Ok(self.record_observation(action, next))
        }
    }

    /// Expire the bounded graceful-stop wait and begin forced termination.
    ///
    /// # Errors
    ///
    /// Returns an invalid-transition, trace-limit, or generation error.
    pub fn expire_grace_period(
        &mut self,
    ) -> Result<&FfmpegLifecycleTransitionV2, FfmpegLifecycleErrorV2> {
        let action = FfmpegLifecycleActionV2::GracePeriodExpired;
        if self.state != FfmpegLifecycleStateV2::GracefulExitPending || self.pending.is_some() {
            return Err(self.invalid(action));
        }
        let (next, effect) = if self.exit_observed.is_present() {
            (
                FfmpegLifecycleStateV2::ReapPending,
                FfmpegEffectV2::ReapDirectChild,
            )
        } else {
            (
                FfmpegLifecycleStateV2::ForcedKillPending,
                FfmpegEffectV2::ForceKillProcessTree,
            )
        };
        self.prepare_transition(true)?;
        Ok(self.record_with_effect(action, next, Some(effect)))
    }

    /// Complete the exact pending effect with a typed outcome.
    ///
    /// # Errors
    ///
    /// Rejects stale/cross-instance requests, mismatched outcomes, invalid state,
    /// exhausted trace capacity, or exhausted effect generations.
    pub fn complete_effect(
        &mut self,
        request: FfmpegEffectRequestV2,
        outcome: FfmpegEffectOutcomeV2,
    ) -> Result<&FfmpegLifecycleTransitionV2, FfmpegLifecycleErrorV2> {
        let action = FfmpegLifecycleActionV2::EffectCompleted { request, outcome };
        if self.pending != Some(request) {
            return Err(FfmpegLifecycleErrorV2::UnexpectedEffectCompletion {
                received: request,
                expected: self.pending,
            });
        }
        if !outcome_matches_effect(request.effect, outcome) {
            return Err(FfmpegLifecycleErrorV2::OutcomeDoesNotMatchEffect {
                effect: request.effect,
                outcome,
            });
        }
        match request.effect {
            FfmpegEffectV2::SpawnProcess => self.complete_spawn(action, outcome),
            FfmpegEffectV2::RequestGracefulStop => self.complete_graceful(action, outcome),
            FfmpegEffectV2::ForceKillProcessTree => self.complete_forced_kill(action, outcome),
            FfmpegEffectV2::ReapDirectChild => self.complete_reap(action, outcome),
            FfmpegEffectV2::PreserveDiagnostics => self.complete_diagnostics(action, outcome),
            FfmpegEffectV2::ValidateOutput => self.complete_output_validation(action, outcome),
            FfmpegEffectV2::ReleaseResources => self.complete_release(action, outcome),
        }
    }

    fn complete_spawn(
        &mut self,
        action: FfmpegLifecycleActionV2,
        outcome: FfmpegEffectOutcomeV2,
    ) -> Result<&FfmpegLifecycleTransitionV2, FfmpegLifecycleErrorV2> {
        let (next, effect) = match outcome {
            FfmpegEffectOutcomeV2::Spawned if self.cancellation_requested.is_present() => (
                FfmpegLifecycleStateV2::GracefulStopPending,
                Some(FfmpegEffectV2::RequestGracefulStop),
            ),
            FfmpegEffectOutcomeV2::Spawned => (FfmpegLifecycleStateV2::Running, None),
            FfmpegEffectOutcomeV2::SpawnFailed => (
                FfmpegLifecycleStateV2::DiagnosticsPending,
                Some(FfmpegEffectV2::PreserveDiagnostics),
            ),
            _ => return Err(self.invalid(action)),
        };
        self.prepare_transition(effect.is_some())?;
        self.pending = None;
        if outcome == FfmpegEffectOutcomeV2::Spawned {
            self.child_spawned = FfmpegFactV2::Present;
        } else {
            self.failure = Some(FfmpegTerminalFailureV2::SpawnFailed);
        }
        Ok(self.record_with_effect(action, next, effect))
    }

    fn complete_graceful(
        &mut self,
        action: FfmpegLifecycleActionV2,
        outcome: FfmpegEffectOutcomeV2,
    ) -> Result<&FfmpegLifecycleTransitionV2, FfmpegLifecycleErrorV2> {
        let (next, effect) = if self.exit_observed.is_present() {
            (
                FfmpegLifecycleStateV2::ReapPending,
                Some(FfmpegEffectV2::ReapDirectChild),
            )
        } else if outcome == FfmpegEffectOutcomeV2::GracefulStopRequested {
            (FfmpegLifecycleStateV2::GracefulExitPending, None)
        } else {
            (
                FfmpegLifecycleStateV2::ForcedKillPending,
                Some(FfmpegEffectV2::ForceKillProcessTree),
            )
        };
        self.prepare_transition(effect.is_some())?;
        self.pending = None;
        Ok(self.record_with_effect(action, next, effect))
    }

    fn complete_forced_kill(
        &mut self,
        action: FfmpegLifecycleActionV2,
        outcome: FfmpegEffectOutcomeV2,
    ) -> Result<&FfmpegLifecycleTransitionV2, FfmpegLifecycleErrorV2> {
        let next_attempt = self.force_kill_attempts.saturating_add(1);
        let failed = outcome == FfmpegEffectOutcomeV2::ForcedKillFailed;
        let exhausted = failed && next_attempt >= self.limits.force_kill;
        let (next, effect) = if failed && !exhausted && !self.exit_observed.is_present() {
            (
                FfmpegLifecycleStateV2::ForcedKillPending,
                Some(FfmpegEffectV2::ForceKillProcessTree),
            )
        } else if exhausted && !self.exit_observed.is_present() {
            (
                FfmpegLifecycleStateV2::DiagnosticsPending,
                Some(FfmpegEffectV2::PreserveDiagnostics),
            )
        } else {
            (
                FfmpegLifecycleStateV2::ReapPending,
                Some(FfmpegEffectV2::ReapDirectChild),
            )
        };
        self.prepare_transition(true)?;
        self.pending = None;
        self.force_kill_attempts = next_attempt;
        if exhausted && !self.exit_observed.is_present() {
            self.failure = Some(FfmpegTerminalFailureV2::ForcedKillFailed);
            self.cleanup_unproven = FfmpegFactV2::Present;
        }
        Ok(self.record_with_effect(action, next, effect))
    }

    fn complete_reap(
        &mut self,
        action: FfmpegLifecycleActionV2,
        outcome: FfmpegEffectOutcomeV2,
    ) -> Result<&FfmpegLifecycleTransitionV2, FfmpegLifecycleErrorV2> {
        let FfmpegEffectOutcomeV2::Reap(reap) = outcome else {
            return Err(self.invalid(action));
        };
        let next_attempt = self.reap_attempts.saturating_add(1);
        let exhausted =
            matches!(reap, FfmpegReapOutcomeV2::WaitFailed(_)) && next_attempt >= self.limits.reap;
        let (next, effect) = if matches!(reap, FfmpegReapOutcomeV2::WaitFailed(_)) && !exhausted {
            (
                FfmpegLifecycleStateV2::ReapPending,
                FfmpegEffectV2::ReapDirectChild,
            )
        } else {
            (
                FfmpegLifecycleStateV2::DiagnosticsPending,
                FfmpegEffectV2::PreserveDiagnostics,
            )
        };
        self.prepare_transition(true)?;
        self.pending = None;
        self.reap_attempts = next_attempt;
        match reap {
            FfmpegReapOutcomeV2::Reaped(termination) => self.reaped = Some(termination),
            FfmpegReapOutcomeV2::WaitFailed(wait_failure) if exhausted => {
                self.failure = Some(FfmpegTerminalFailureV2::WaitFailed(wait_failure));
                self.cleanup_unproven = FfmpegFactV2::Present;
            }
            FfmpegReapOutcomeV2::WaitFailed(_) => {}
        }
        Ok(self.record_with_effect(action, next, Some(effect)))
    }

    fn complete_diagnostics(
        &mut self,
        action: FfmpegLifecycleActionV2,
        outcome: FfmpegEffectOutcomeV2,
    ) -> Result<&FfmpegLifecycleTransitionV2, FfmpegLifecycleErrorV2> {
        let next_attempt = self.diagnostic_attempts.saturating_add(1);
        let failed = outcome == FfmpegEffectOutcomeV2::DiagnosticsFailed;
        let retry = failed && next_attempt < self.limits.diagnostics;
        let (next, effect) = if retry {
            (
                FfmpegLifecycleStateV2::DiagnosticsPending,
                Some(FfmpegEffectV2::PreserveDiagnostics),
            )
        } else if self.cleanup_unproven.is_present() {
            (
                if self.cancellation_requested.is_present() {
                    FfmpegLifecycleStateV2::Cancelled
                } else {
                    FfmpegLifecycleStateV2::Failed
                },
                None,
            )
        } else if !self.child_spawned.is_present()
            || self.cancellation_requested.is_present()
            || self.failure.is_some()
        {
            (
                FfmpegLifecycleStateV2::ReleasePending,
                Some(FfmpegEffectV2::ReleaseResources),
            )
        } else {
            match self.reaped {
                Some(FfmpegTerminationV2::ExitCode(0)) => (
                    FfmpegLifecycleStateV2::OutputValidationPending,
                    Some(FfmpegEffectV2::ValidateOutput),
                ),
                Some(_) => (
                    FfmpegLifecycleStateV2::ReleasePending,
                    Some(FfmpegEffectV2::ReleaseResources),
                ),
                None => return Err(self.invalid(action)),
            }
        };
        self.prepare_transition(effect.is_some())?;
        self.pending = None;
        self.diagnostic_attempts = next_attempt;
        if !retry && !self.cancellation_requested.is_present() && self.failure.is_none() {
            self.failure = termination_failure(self.reaped);
        }
        if outcome == FfmpegEffectOutcomeV2::DiagnosticsPreserved {
            self.diagnostics_preserved = FfmpegFactV2::Present;
        } else if !retry && self.failure.is_none() {
            self.failure = Some(FfmpegTerminalFailureV2::DiagnosticsFailed);
        }
        Ok(self.record_with_effect(action, next, effect))
    }

    fn complete_output_validation(
        &mut self,
        action: FfmpegLifecycleActionV2,
        outcome: FfmpegEffectOutcomeV2,
    ) -> Result<&FfmpegLifecycleTransitionV2, FfmpegLifecycleErrorV2> {
        self.prepare_transition(true)?;
        self.pending = None;
        self.failure = match outcome {
            FfmpegEffectOutcomeV2::OutputValidated => self.failure,
            FfmpegEffectOutcomeV2::OutputInvalid => Some(FfmpegTerminalFailureV2::OutputInvalid),
            FfmpegEffectOutcomeV2::OutputValidationFailed => {
                Some(FfmpegTerminalFailureV2::OutputValidationFailed)
            }
            _ => return Err(self.invalid(action)),
        };
        Ok(self.record_with_effect(
            action,
            FfmpegLifecycleStateV2::ReleasePending,
            Some(FfmpegEffectV2::ReleaseResources),
        ))
    }

    fn complete_release(
        &mut self,
        action: FfmpegLifecycleActionV2,
        outcome: FfmpegEffectOutcomeV2,
    ) -> Result<&FfmpegLifecycleTransitionV2, FfmpegLifecycleErrorV2> {
        let next_attempt = self.release_attempts.saturating_add(1);
        let failed = outcome == FfmpegEffectOutcomeV2::ResourceReleaseFailed;
        let retry = failed && next_attempt < self.limits.release;
        let next = if retry {
            FfmpegLifecycleStateV2::ReleasePending
        } else if self.cancellation_requested.is_present() {
            FfmpegLifecycleStateV2::Cancelled
        } else if failed || self.failure.is_some() {
            FfmpegLifecycleStateV2::Failed
        } else {
            FfmpegLifecycleStateV2::Exited
        };
        self.prepare_transition(retry)?;
        self.pending = None;
        self.release_attempts = next_attempt;
        if failed && !retry {
            self.failure = Some(FfmpegTerminalFailureV2::ResourceReleaseFailed);
        }
        if outcome == FfmpegEffectOutcomeV2::ResourcesReleased {
            self.resources_released = FfmpegFactV2::Present;
        }
        Ok(self.record_with_effect(
            action,
            next,
            retry.then_some(FfmpegEffectV2::ReleaseResources),
        ))
    }

    const fn is_terminal(&self) -> bool {
        matches!(
            self.state,
            FfmpegLifecycleStateV2::Exited
                | FfmpegLifecycleStateV2::Failed
                | FfmpegLifecycleStateV2::Cancelled
        )
    }

    const fn invalid(&self, action: FfmpegLifecycleActionV2) -> FfmpegLifecycleErrorV2 {
        FfmpegLifecycleErrorV2::InvalidTransition {
            state: self.state,
            action,
        }
    }

    fn prepare_transition(&self, issues_effect: bool) -> Result<(), FfmpegLifecycleErrorV2> {
        if self.trace.len() >= self.trace_limit {
            return Err(FfmpegLifecycleErrorV2::TraceLimitReached {
                limit: self.trace_limit,
            });
        }
        if issues_effect && self.next_generation == u64::MAX {
            return Err(FfmpegLifecycleErrorV2::EffectGenerationExhausted);
        }
        Ok(())
    }

    fn record_with_effect(
        &mut self,
        action: FfmpegLifecycleActionV2,
        next: FfmpegLifecycleStateV2,
        effect: Option<FfmpegEffectV2>,
    ) -> &FfmpegLifecycleTransitionV2 {
        let previous = self.state;
        let issued = effect.map(|effect| {
            let request = FfmpegEffectRequestV2 {
                instance_id: self.instance_id,
                generation: self.next_generation,
                effect,
            };
            self.next_generation += 1;
            request
        });
        self.state = next;
        self.pending = issued;
        self.trace.push(FfmpegLifecycleTransitionV2 {
            previous,
            action,
            next,
            issued,
        });
        let index = self.trace.len() - 1;
        &self.trace[index]
    }

    fn record_observation(
        &mut self,
        action: FfmpegLifecycleActionV2,
        next: FfmpegLifecycleStateV2,
    ) -> &FfmpegLifecycleTransitionV2 {
        let previous = self.state;
        self.state = next;
        self.trace.push(FfmpegLifecycleTransitionV2 {
            previous,
            action,
            next,
            issued: None,
        });
        let index = self.trace.len() - 1;
        &self.trace[index]
    }
}

const fn outcome_matches_effect(effect: FfmpegEffectV2, outcome: FfmpegEffectOutcomeV2) -> bool {
    matches!(
        (effect, outcome),
        (
            FfmpegEffectV2::SpawnProcess,
            FfmpegEffectOutcomeV2::Spawned | FfmpegEffectOutcomeV2::SpawnFailed
        ) | (
            FfmpegEffectV2::RequestGracefulStop,
            FfmpegEffectOutcomeV2::GracefulStopRequested
                | FfmpegEffectOutcomeV2::GracefulStopUnsupported
                | FfmpegEffectOutcomeV2::GracefulStopFailed
        ) | (
            FfmpegEffectV2::ForceKillProcessTree,
            FfmpegEffectOutcomeV2::ForcedKillRequested
                | FfmpegEffectOutcomeV2::ForcedKillAlreadyExited
                | FfmpegEffectOutcomeV2::ForcedKillFailed
        ) | (
            FfmpegEffectV2::ReapDirectChild,
            FfmpegEffectOutcomeV2::Reap(_)
        ) | (
            FfmpegEffectV2::PreserveDiagnostics,
            FfmpegEffectOutcomeV2::DiagnosticsPreserved | FfmpegEffectOutcomeV2::DiagnosticsFailed
        ) | (
            FfmpegEffectV2::ValidateOutput,
            FfmpegEffectOutcomeV2::OutputValidated
                | FfmpegEffectOutcomeV2::OutputInvalid
                | FfmpegEffectOutcomeV2::OutputValidationFailed
        ) | (
            FfmpegEffectV2::ReleaseResources,
            FfmpegEffectOutcomeV2::ResourcesReleased | FfmpegEffectOutcomeV2::ResourceReleaseFailed
        )
    )
}

const fn termination_failure(
    termination: Option<FfmpegTerminationV2>,
) -> Option<FfmpegTerminalFailureV2> {
    match termination {
        Some(FfmpegTerminationV2::ExitCode(0)) | None => None,
        Some(FfmpegTerminationV2::ExitCode(code)) => {
            Some(FfmpegTerminalFailureV2::NonzeroExit(code))
        }
        Some(FfmpegTerminationV2::UnixSignal(signal)) => {
            Some(FfmpegTerminalFailureV2::UnixSignal(signal))
        }
        Some(FfmpegTerminationV2::WindowsStatusOpaque(status)) => {
            Some(FfmpegTerminalFailureV2::WindowsStatusOpaque(status))
        }
        Some(FfmpegTerminationV2::WindowsException(code)) => {
            Some(FfmpegTerminalFailureV2::WindowsException(code))
        }
    }
}

/// Every lifecycle required by WP-005 has one explicit model identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MachineKind {
    JobCancellation,
    SourceRedirect,
    AtomicAdmission,
    ByteCreditDurability,
    Live,
    Sink,
    Ffmpeg,
    JavascriptWorker,
    PluginIpc,
    CommitArchiveReconciliation,
    FilesystemCapability,
    Watcher,
}

/// Caller-supplied stable identity for one state-machine instance.
///
/// The value must remain unchanged across durable restoration. Callers must
/// allocate distinct values for concurrently routable instances.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MachineInstanceId(u64);

impl MachineInstanceId {
    /// Construct a nonzero instance identity.
    #[must_use]
    pub const fn new(value: u64) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// The complete finite state space. Variants retain lifecycle-specific names so
/// traces remain unambiguous without ambient context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Generic non-durable wrapper while a lifecycle-owned adapter effect is pending.
    EffectPending,
    /// Generic non-durable recovery point after a correlated adapter effect outcome failed.
    EffectRecovery,
    JobQueued,
    JobRunning,
    JobCancelling,
    JobVerifying,
    JobSucceeded,
    JobFailed,
    JobCancelled,
    SourceNew,
    SourceResolving,
    SourceRedirecting,
    SourceResolved,
    SourceFailed,
    SourceCancelled,
    AdmissionWaiting,
    AdmissionGranted,
    AdmissionReleased,
    AdmissionCancelled,
    BytesEmpty,
    BytesReceived,
    BytesWriting,
    BytesWritten,
    BytesSynchronizing,
    BytesDurable,
    BytesFailed,
    BytesCancelled,
    LiveStarting,
    LiveRefreshing,
    LiveStreaming,
    LiveStopped,
    LiveFailed,
    LiveCancelled,
    SinkPending,
    SinkActive,
    SinkDraining,
    SinkCompleted,
    SinkDropped,
    SinkFailed,
    SinkCancelled,
    FfmpegPrepared,
    FfmpegSpawning,
    FfmpegSpawned,
    FfmpegRunning,
    FfmpegReaping,
    FfmpegCancelling,
    FfmpegFailing,
    FfmpegExitReleasing,
    FfmpegCancellationReleasing,
    FfmpegFailureReleasing,
    FfmpegSpawnRecovering,
    FfmpegReapRecovering,
    FfmpegCancellationRecovering,
    FfmpegFailureRecovering,
    FfmpegExitReleaseRecovering,
    FfmpegCancellationReleaseRecovering,
    FfmpegFailureReleaseRecovering,
    FfmpegExited,
    FfmpegFailed,
    FfmpegCancelled,
    JavascriptIdle,
    JavascriptAssigned,
    JavascriptRunning,
    JavascriptRecycling,
    JavascriptQuarantined,
    JavascriptCompleted,
    JavascriptCancelled,
    PluginDisconnected,
    PluginHandshaking,
    PluginReady,
    PluginInFlight,
    PluginDraining,
    PluginStopped,
    PluginFailed,
    CommitWorking,
    CommitPreparing,
    CommitDataSynchronizing,
    CommitPreparedDirectorySynchronizing,
    CommitPreparedRecordAppending,
    CommitPreparedJournalSynchronizing,
    CommitPrepared,
    CommitRenaming,
    CommitRenameDirectorySynchronizing,
    CommitRenamedRecordAppending,
    CommitRenamedJournalSynchronizing,
    CommitRenamed,
    CommitArchiving,
    CommitArchivedRecordAppending,
    CommitArchivedJournalSynchronizing,
    CommitArchived,
    CommitCleaning,
    CommitCleanedRecordAppending,
    CommitCleanedJournalSynchronizing,
    CommitCleaned,
    CommitReconciling,
    CommitVerifyingPrepared,
    CommitVerifyingRenamed,
    CommitVerifyingArchived,
    CommitVerifyingCleaned,
    CommitCancelling,
    CommitReconciled,
    CommitInconsistent,
    CommitCancelled,
    FilesystemUnknown,
    FilesystemProbing,
    FilesystemProbed,
    FilesystemProbeFailed,
    FilesystemProbeCancelled,
    FilesystemConfining,
    FilesystemConfinementFailed,
    FilesystemConfinementCancelled,
    FilesystemConfined,
    FilesystemDegrading,
    FilesystemDegradationFailed,
    FilesystemDegradationCancelled,
    FilesystemDegraded,
    FilesystemRejecting,
    FilesystemRejectionFailed,
    FilesystemRejectionCancelled,
    FilesystemUnsupported,
    FilesystemCancelled,
    WatcherStarting,
    WatcherReady,
    WatcherServing,
    WatcherDegraded,
    WatcherStale,
    WatcherDraining,
    WatcherStopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Start,
    Assign,
    Admit,
    Receive,
    Validate,
    PersistDurably,
    Redirect,
    Continue,
    Ready,
    Serve,
    Refresh,
    Drain,
    Spawn,
    Reap,
    /// Legacy uncorrelated acknowledgement marker. Applying it is always invalid.
    /// Callers must use [`Event::EffectAcknowledged`].
    Acknowledge,
    EffectAcknowledged {
        instance_id: MachineInstanceId,
        effect: EffectIntent,
        generation: u64,
    },
    /// Correlated failure outcome for one effect adapter request.
    EffectFailed {
        instance_id: MachineInstanceId,
        effect: EffectIntent,
        generation: u64,
    },
    /// Correlated cancellation outcome for one effect adapter request.
    EffectCancelled {
        instance_id: MachineInstanceId,
        effect: EffectIntent,
        generation: u64,
    },
    Recycle,
    Quarantine,
    Prepare,
    Rename,
    Archive,
    Cleanup,
    Reconcile,
    Probe,
    Confine,
    Degrade,
    MarkStale,
    Reject,
    Release,
    Complete,
    Fail,
    Cancel,
    Restart,
}

/// Declarative requests to an effect adapter. These are data, not execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectIntent {
    BeginJob,
    RequestCancellation,
    ReleaseResources,
    ResolveSource,
    FollowRedirect,
    ReserveResources,
    AcceptBoundedBytes,
    ValidateAndWrite,
    SynchronizeData,
    RefreshManifest,
    DeliverToSink,
    DrainSink,
    SpawnProcess,
    TerminateProcess,
    ReapProcess,
    DispatchWorker,
    RecycleWorker,
    IsolateWorker,
    OpenPluginChannel,
    SendPluginRequest,
    ClosePluginChannel,
    ValidateOutput,
    /// Rename the staged output inside the destination filesystem.
    RenameOutput,
    /// Synchronize the directory entry that makes a durable prefix discoverable.
    SynchronizeDirectory,
    /// Append the transition record for the transient commit phase.
    ///
    /// The adapter must make retries identity-aware: an already-present identical
    /// sequence/hash is acknowledged, while a conflicting record fails closed.
    AppendTransitionRecord,
    /// Synchronize the journal only after its transition record was appended.
    SynchronizeJournal,
    InsertArchiveRow,
    RemoveTemporaryState,
    RevalidatePreparedOutput,
    VerifyFinalArtifact,
    VerifyArchiveOutputPair,
    ProbeFilesystem,
    EstablishConfinedPath,
    ReportDegradedGuarantees,
    RejectFilesystem,
    NegotiateWatcher,
    SelfTestWatcherStorage,
    VerifyProducerCanary,
    ReportDiagnosticsDegraded,
    FlushWatcher,
    PreserveDiagnostics,
    DrainInFlightEffect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransitionError {
    StateDoesNotBelongToMachine {
        kind: MachineKind,
        state: State,
    },
    StateIsNotDurable {
        kind: MachineKind,
        state: State,
    },
    InvalidRestorationGeneration,
    EffectGenerationExhausted,
    UnexpectedAcknowledgement {
        instance_id: MachineInstanceId,
        effect: EffectIntent,
        generation: u64,
        expected: Vec<EffectAcknowledgement>,
    },
    UnexpectedEffectFailure {
        instance_id: MachineInstanceId,
        effect: EffectIntent,
        generation: u64,
        expected: Vec<EffectAcknowledgement>,
    },
    UnexpectedEffectCancellation {
        instance_id: MachineInstanceId,
        effect: EffectIntent,
        generation: u64,
        expected: Vec<EffectAcknowledgement>,
    },
    ByteDurabilityAcknowledgementRequired {
        effect: EffectIntent,
    },
    ByteDurabilityRestorationEvidenceRequired {
        state: State,
    },
    InvalidTransition {
        kind: MachineKind,
        state: State,
        event: Event,
    },
    TraceLimitReached {
        limit: usize,
    },
    ReplayMismatch {
        index: usize,
    },
}

/// Failure while coupling a byte lifecycle receipt to authoritative credit state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ByteDurabilityEffectError {
    WrongMachine,
    UnsupportedEffect { effect: EffectIntent },
    PositionDoesNotMatchEffect { effect: EffectIntent },
    Transition(TransitionError),
    Credit(CreditError),
}

/// Failure while restoring a byte lifecycle from authoritative credit state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ByteDurabilityRestorationError {
    PositionDoesNotMatchState {
        state: State,
        position: ByteCreditPosition,
    },
    Transition(TransitionError),
    Credit(CreditError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transition {
    pub previous: State,
    pub event: Event,
    pub next: State,
    pub effects: Vec<EffectIntent>,
}

/// Correlation token for one effect completion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectAcknowledgement {
    pub instance_id: MachineInstanceId,
    pub effect: EffectIntent,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DeferredEffects {
    target: State,
    effects: Vec<EffectIntent>,
    has_unsuccessful_outcome: bool,
}

/// One bounded, deterministic state machine instance.
///
/// Mutable lifecycle instances are deliberately non-cloneable so one pending
/// effect correlation cannot be forked and acknowledged more than once.
///
/// ```compile_fail
/// use fforager_core::lifecycle::{MachineInstanceId, MachineKind, StateMachine};
///
/// let instance = MachineInstanceId::new(1).expect("nonzero instance");
/// let machine = StateMachine::new(MachineKind::ByteCreditDurability, instance, 8);
/// let fork = machine.clone();
/// # let _ = fork;
/// ```
#[derive(Debug)]
pub struct StateMachine {
    instance_id: MachineInstanceId,
    kind: MachineKind,
    state: State,
    trace_limit: usize,
    trace: Vec<Transition>,
    pending_acknowledgements: Vec<EffectAcknowledgement>,
    pending_unsuccessful_effect_outcome: bool,
    deferred_effects: Option<DeferredEffects>,
    next_effect_generation: u64,
    byte_durability_acknowledgement_authorized: bool,
    byte_durability_broker: Option<OwnedByteCreditBroker>,
}

impl PartialEq for StateMachine {
    fn eq(&self, other: &Self) -> bool {
        self.instance_id == other.instance_id
            && self.kind == other.kind
            && self.state == other.state
            && self.trace_limit == other.trace_limit
            && self.trace == other.trace
            && self.pending_acknowledgements == other.pending_acknowledgements
            && self.pending_unsuccessful_effect_outcome == other.pending_unsuccessful_effect_outcome
            && self.deferred_effects == other.deferred_effects
            && self.next_effect_generation == other.next_effect_generation
            && self.byte_durability_acknowledgement_authorized
                == other.byte_durability_acknowledgement_authorized
            && self.byte_durability_broker.is_some() == other.byte_durability_broker.is_some()
            && self
                .byte_durability_broker
                .as_ref()
                .map(OwnedByteCreditBroker::position)
                == other
                    .byte_durability_broker
                    .as_ref()
                    .map(OwnedByteCreditBroker::position)
    }
}

impl Eq for StateMachine {}

impl StateMachine {
    #[must_use]
    pub fn new(kind: MachineKind, instance_id: MachineInstanceId, trace_limit: usize) -> Self {
        Self {
            instance_id,
            kind,
            state: initial(kind),
            trace_limit,
            trace: Vec::new(),
            pending_acknowledgements: Vec::new(),
            pending_unsuccessful_effect_outcome: false,
            deferred_effects: None,
            next_effect_generation: 1,
            byte_durability_acknowledgement_authorized: false,
            byte_durability_broker: None,
        }
    }

    /// Restore a machine at a validated durable prefix.
    ///
    /// # Errors
    ///
    /// Returns an error when `state` does not belong to `kind`, is transient,
    /// or the caller does not provide a nonzero persisted generation seed.
    pub fn from_state(
        kind: MachineKind,
        state: State,
        instance_id: MachineInstanceId,
        trace_limit: usize,
        next_effect_generation: u64,
    ) -> Result<Self, TransitionError> {
        if !belongs(kind, state) {
            return Err(TransitionError::StateDoesNotBelongToMachine { kind, state });
        }
        if !is_durable_state(kind, state) {
            return Err(TransitionError::StateIsNotDurable { kind, state });
        }
        if kind == MachineKind::ByteCreditDurability {
            return Err(TransitionError::ByteDurabilityRestorationEvidenceRequired { state });
        }
        if next_effect_generation == 0 {
            return Err(TransitionError::InvalidRestorationGeneration);
        }
        Ok(Self::restored(
            kind,
            state,
            instance_id,
            trace_limit,
            next_effect_generation,
        ))
    }

    /// Restore a byte lifecycle only when the authoritative broker position
    /// substantiates the requested durable phase.
    ///
    /// # Errors
    ///
    /// Returns a typed transition, broker, or position/state mismatch error.
    pub fn from_byte_durability_state(
        state: State,
        broker: &OwnedByteCreditBroker,
        instance_id: MachineInstanceId,
        trace_limit: usize,
        next_effect_generation: u64,
    ) -> Result<Self, ByteDurabilityRestorationError> {
        let kind = MachineKind::ByteCreditDurability;
        if !belongs(kind, state) {
            return Err(ByteDurabilityRestorationError::Transition(
                TransitionError::StateDoesNotBelongToMachine { kind, state },
            ));
        }
        if !is_durable_state(kind, state) {
            return Err(ByteDurabilityRestorationError::Transition(
                TransitionError::StateIsNotDurable { kind, state },
            ));
        }
        if next_effect_generation == 0 {
            return Err(ByteDurabilityRestorationError::Transition(
                TransitionError::InvalidRestorationGeneration,
            ));
        }
        broker
            .verify()
            .map_err(ByteDurabilityRestorationError::Credit)?;
        let position = broker.position();
        if !byte_position_matches_state(position, state) {
            return Err(ByteDurabilityRestorationError::PositionDoesNotMatchState {
                state,
                position,
            });
        }
        let mut restored = Self::restored(
            kind,
            state,
            instance_id,
            trace_limit,
            next_effect_generation,
        );
        restored.byte_durability_broker = Some(broker.clone());
        Ok(restored)
    }

    fn restored(
        kind: MachineKind,
        state: State,
        instance_id: MachineInstanceId,
        trace_limit: usize,
        next_effect_generation: u64,
    ) -> Self {
        Self {
            instance_id,
            kind,
            state,
            trace_limit,
            trace: Vec::new(),
            pending_acknowledgements: Vec::new(),
            pending_unsuccessful_effect_outcome: false,
            deferred_effects: None,
            next_effect_generation,
            byte_durability_acknowledgement_authorized: false,
            byte_durability_broker: None,
        }
    }

    #[must_use]
    pub fn state(&self) -> State {
        self.state
    }

    #[must_use]
    pub fn instance_id(&self) -> MachineInstanceId {
        self.instance_id
    }

    #[must_use]
    pub fn trace(&self) -> &[Transition] {
        &self.trace
    }

    #[must_use]
    pub fn pending_acknowledgements(&self) -> &[EffectAcknowledgement] {
        &self.pending_acknowledgements
    }

    /// Acknowledge one byte lifecycle effect and advance its authoritative broker position.
    ///
    /// The broker-issued acknowledgement is deliberately created inside core effect
    /// code, consumed exactly once by the same broker, and never exposed to callers.
    /// Ordinary [`StateMachine::apply`] calls cannot acknowledge these byte effects.
    ///
    /// # Errors
    ///
    /// Returns [`ByteDurabilityEffectError`] for the wrong machine/effect, a position
    /// delta that does not belong to the acknowledged phase, an invalid correlated
    /// lifecycle receipt, or a rejected broker position.
    pub fn acknowledge_byte_durability_effect(
        &mut self,
        broker: &OwnedByteCreditBroker,
        event: Event,
        next: ByteCreditPosition,
    ) -> Result<&Transition, ByteDurabilityEffectError> {
        if self.kind != MachineKind::ByteCreditDurability {
            return Err(ByteDurabilityEffectError::WrongMachine);
        }
        let Event::EffectAcknowledged { effect, .. } = event else {
            return Err(ByteDurabilityEffectError::Transition(
                TransitionError::InvalidTransition {
                    kind: self.kind,
                    state: self.state,
                    event,
                },
            ));
        };
        if !matches!(
            effect,
            EffectIntent::AcceptBoundedBytes
                | EffectIntent::ValidateAndWrite
                | EffectIntent::SynchronizeData
        ) {
            return Err(ByteDurabilityEffectError::UnsupportedEffect { effect });
        }
        self.verify_byte_durability_broker(broker)
            .map_err(ByteDurabilityEffectError::Credit)?;
        let current = broker.position();
        if !byte_position_matches_effect(current, next, effect) {
            return Err(ByteDurabilityEffectError::PositionDoesNotMatchEffect { effect });
        }
        let acknowledgement = broker
            .acknowledge_durability_effects(next)
            .map_err(ByteDurabilityEffectError::Credit)?;
        self.byte_durability_acknowledgement_authorized = true;
        self.apply(event)
            .map_err(ByteDurabilityEffectError::Transition)?;
        broker
            .advance(acknowledgement)
            .map_err(ByteDurabilityEffectError::Credit)?;
        if self.byte_durability_broker.is_none() {
            self.byte_durability_broker = Some(broker.clone());
        }
        self.trace
            .last()
            .ok_or(ByteDurabilityEffectError::Transition(
                TransitionError::ReplayMismatch { index: 0 },
            ))
    }

    fn verify_byte_durability_broker(
        &self,
        broker: &OwnedByteCreditBroker,
    ) -> Result<(), CreditError> {
        let Some(authoritative) = &self.byte_durability_broker else {
            return Ok(());
        };
        let identity_receipt =
            authoritative.acknowledge_durability_effects(authoritative.position())?;
        broker.advance(identity_receipt)
    }

    fn record_transition(
        &mut self,
        event: Event,
        next: State,
        effects: Vec<EffectIntent>,
        required: Vec<EffectIntent>,
        deferred_effects: Option<DeferredEffects>,
    ) -> Result<usize, TransitionError> {
        let next_generation = if required.is_empty() {
            self.next_effect_generation
        } else {
            self.next_effect_generation
                .checked_add(1)
                .ok_or(TransitionError::EffectGenerationExhausted)?
        };
        let pending_acknowledgements = required
            .into_iter()
            .map(|effect| EffectAcknowledgement {
                instance_id: self.instance_id,
                effect,
                generation: self.next_effect_generation,
            })
            .collect();
        let record = Transition {
            previous: self.state,
            event,
            next,
            effects,
        };
        self.state = next;
        self.pending_acknowledgements = pending_acknowledgements;
        self.pending_unsuccessful_effect_outcome = false;
        self.deferred_effects = deferred_effects;
        self.next_effect_generation = next_generation;
        self.trace.push(record);
        Ok(self.trace.len() - 1)
    }

    fn apply_transition_index(
        &mut self,
        event: Event,
        next: State,
        effects: Vec<EffectIntent>,
    ) -> Result<usize, TransitionError> {
        if uses_generic_effect_wrapper(self.kind) && !effects.is_empty() {
            let deferred_effects = DeferredEffects {
                target: next,
                effects: effects.clone(),
                has_unsuccessful_outcome: false,
            };
            return self.record_transition(
                event,
                State::EffectPending,
                effects.clone(),
                effects,
                Some(deferred_effects),
            );
        }

        let required = acknowledgement_effects(next, &effects);
        if !uses_generic_effect_wrapper(self.kind) && required.len() != effects.len() {
            return Err(TransitionError::InvalidTransition {
                kind: self.kind,
                state: self.state,
                event,
            });
        }
        self.record_transition(event, next, effects, required, None)
    }

    fn apply_transition(
        &mut self,
        event: Event,
        next: State,
        effects: Vec<EffectIntent>,
    ) -> Result<&Transition, TransitionError> {
        let index = self.apply_transition_index(event, next, effects)?;
        self.trace
            .get(index)
            .ok_or(TransitionError::ReplayMismatch { index })
    }

    fn outcome_error(
        &self,
        event: Event,
        instance_id: MachineInstanceId,
        effect: EffectIntent,
        generation: u64,
    ) -> TransitionError {
        let expected = self.pending_acknowledgements.clone();
        match event {
            Event::EffectAcknowledged { .. } => TransitionError::UnexpectedAcknowledgement {
                instance_id,
                effect,
                generation,
                expected,
            },
            Event::EffectFailed { .. } => TransitionError::UnexpectedEffectFailure {
                instance_id,
                effect,
                generation,
                expected,
            },
            Event::EffectCancelled { .. } => TransitionError::UnexpectedEffectCancellation {
                instance_id,
                effect,
                generation,
                expected,
            },
            _ => TransitionError::InvalidTransition {
                kind: self.kind,
                state: self.state,
                event,
            },
        }
    }

    fn deferred_effect_outcome(
        &self,
        event: Event,
    ) -> Result<(MachineInstanceId, EffectIntent, u64, bool), TransitionError> {
        Ok(match event {
            Event::EffectAcknowledged {
                instance_id,
                effect,
                generation,
            } => (instance_id, effect, generation, true),
            Event::EffectFailed {
                instance_id,
                effect,
                generation,
            }
            | Event::EffectCancelled {
                instance_id,
                effect,
                generation,
            } => (instance_id, effect, generation, false),
            _ => {
                return Err(TransitionError::InvalidTransition {
                    kind: self.kind,
                    state: self.state,
                    event,
                });
            }
        })
    }

    fn restart_deferred_effect(&mut self, event: Event) -> Result<usize, TransitionError> {
        if event != Event::Restart {
            return Err(TransitionError::InvalidTransition {
                kind: self.kind,
                state: self.state,
                event,
            });
        }
        let deferred = self
            .deferred_effects
            .take()
            .ok_or(TransitionError::ReplayMismatch {
                index: self.trace.len(),
            })?;
        let preserved = deferred.clone();
        match self.apply_transition_index(event, deferred.target, deferred.effects) {
            Ok(index) => Ok(index),
            Err(error) => {
                self.deferred_effects = Some(preserved);
                Err(error)
            }
        }
    }

    fn complete_deferred_effect(&mut self, event: Event) -> Result<usize, TransitionError> {
        let (instance_id, effect, generation, successful) = self.deferred_effect_outcome(event)?;
        let Some(index) = self.pending_acknowledgements.iter().position(|expected| {
            expected.instance_id == instance_id
                && expected.effect == effect
                && expected.generation == generation
        }) else {
            return Err(self.outcome_error(event, instance_id, effect, generation));
        };
        let mut remaining = self.pending_acknowledgements.clone();
        remaining.remove(index);
        let mut deferred = self
            .deferred_effects
            .take()
            .ok_or(TransitionError::ReplayMismatch {
                index: self.trace.len(),
            })?;
        deferred.has_unsuccessful_outcome |= !successful;

        if !remaining.is_empty() {
            self.pending_acknowledgements = remaining;
            self.deferred_effects = Some(deferred);
            let record = Transition {
                previous: self.state,
                event,
                next: State::EffectPending,
                effects: Vec::new(),
            };
            self.trace.push(record);
            return Ok(self.trace.len() - 1);
        }

        if deferred.has_unsuccessful_outcome {
            return self.record_transition(
                event,
                State::EffectRecovery,
                Vec::new(),
                Vec::new(),
                Some(deferred),
            );
        }

        let (next, effects) = match transition(self.kind, deferred.target, event) {
            Ok(transition) => transition,
            Err(TransitionError::InvalidTransition { .. }) => (deferred.target, Vec::new()),
            Err(error) => return Err(error),
        };
        let preserved = deferred.clone();
        let index = match self.apply_transition_index(event, next, effects) {
            Ok(index) => index,
            Err(error) => {
                self.deferred_effects = Some(preserved);
                return Err(error);
            }
        };
        Ok(index)
    }

    fn apply_deferred_effect(&mut self, event: Event) -> Result<&Transition, TransitionError> {
        let index = if self.state == State::EffectRecovery {
            self.restart_deferred_effect(event)?
        } else {
            self.complete_deferred_effect(event)?
        };
        self.trace
            .get(index)
            .ok_or(TransitionError::ReplayMismatch { index })
    }

    /// Apply one event atomically, recording it only after validation.
    ///
    /// # Errors
    ///
    /// Returns a typed invalid-transition or trace-bound error.
    pub fn apply(&mut self, event: Event) -> Result<&Transition, TransitionError> {
        if self.kind == MachineKind::ByteCreditDurability
            && let Event::EffectAcknowledged { effect, .. } = event
            && matches!(
                effect,
                EffectIntent::AcceptBoundedBytes
                    | EffectIntent::ValidateAndWrite
                    | EffectIntent::SynchronizeData
            )
            && !std::mem::take(&mut self.byte_durability_acknowledgement_authorized)
        {
            return Err(TransitionError::ByteDurabilityAcknowledgementRequired { effect });
        }
        self.byte_durability_acknowledgement_authorized = false;
        if self.trace.len() >= self.trace_limit {
            return Err(TransitionError::TraceLimitReached {
                limit: self.trace_limit,
            });
        }
        if self.deferred_effects.is_some() {
            return self.apply_deferred_effect(event);
        }
        if !self.pending_acknowledgements.is_empty()
            && !matches!(
                event,
                Event::EffectAcknowledged { .. }
                    | Event::EffectFailed { .. }
                    | Event::EffectCancelled { .. }
            )
        {
            return Err(TransitionError::InvalidTransition {
                kind: self.kind,
                state: self.state,
                event,
            });
        }
        let outcome = match event {
            Event::EffectAcknowledged {
                instance_id,
                effect,
                generation,
            } => Some((instance_id, effect, generation, true)),
            Event::EffectFailed {
                instance_id,
                effect,
                generation,
            }
            | Event::EffectCancelled {
                instance_id,
                effect,
                generation,
            } => Some((instance_id, effect, generation, false)),
            _ => None,
        };
        if let Some((instance_id, effect, generation, successful)) = outcome
            && (!successful || !self.pending_acknowledgements.is_empty())
        {
            let mut remaining_acknowledgements = self.pending_acknowledgements.clone();
            let Some(index) = remaining_acknowledgements.iter().position(|expected| {
                expected.instance_id == instance_id
                    && expected.effect == effect
                    && expected.generation == generation
            }) else {
                return Err(self.outcome_error(event, instance_id, effect, generation));
            };
            remaining_acknowledgements.remove(index);
            self.pending_unsuccessful_effect_outcome |= !successful;
            if !remaining_acknowledgements.is_empty() {
                let record = Transition {
                    previous: self.state,
                    event,
                    next: self.state,
                    effects: Vec::new(),
                };
                self.pending_acknowledgements = remaining_acknowledgements;
                self.trace.push(record);
                let index = self.trace.len() - 1;
                return self
                    .trace
                    .get(index)
                    .ok_or(TransitionError::ReplayMismatch { index });
            }
            let has_unsuccessful_outcome = self.pending_unsuccessful_effect_outcome;
            self.pending_acknowledgements.clear();
            self.pending_unsuccessful_effect_outcome = false;
            if has_unsuccessful_outcome
                && let Some((next, effects)) = unsuccessful_effect_recovery(self.kind, self.state)
            {
                return self.apply_transition(event, next, effects);
            }
        }
        let (next, effects) = transition(self.kind, self.state, event)?;
        self.apply_transition(event, next, effects)
    }

    /// Replay a recorded trace and require each state/effect record to match.
    ///
    /// Byte-credit durability acknowledgements are deliberately rejected because
    /// a transition trace contains no broker-issued durability receipt. Re-execute
    /// those acknowledgements through [`Self::acknowledge_byte_durability_effect`]
    /// with authoritative credit state instead of trusting trace shape alone.
    ///
    /// # Errors
    ///
    /// Returns the first transition, bound, or record mismatch.
    pub fn replay(
        kind: MachineKind,
        instance_id: MachineInstanceId,
        trace_limit: usize,
        expected: &[Transition],
    ) -> Result<Self, TransitionError> {
        let mut machine = Self::new(kind, instance_id, trace_limit);
        for (index, record) in expected.iter().enumerate() {
            let actual = machine.apply(record.event)?;
            if actual != record {
                return Err(TransitionError::ReplayMismatch { index });
            }
        }
        Ok(machine)
    }
}

fn byte_position_matches_effect(
    current: ByteCreditPosition,
    next: ByteCreditPosition,
    effect: EffectIntent,
) -> bool {
    match effect {
        EffectIntent::AcceptBoundedBytes => {
            next.received > current.received
                && next.validated_written_contiguous == current.validated_written_contiguous
                && next.durable_contiguous == current.durable_contiguous
        }
        EffectIntent::ValidateAndWrite => {
            next.received == current.received
                && next.validated_written_contiguous > current.validated_written_contiguous
                && next.durable_contiguous == current.durable_contiguous
        }
        EffectIntent::SynchronizeData => {
            next.received == current.received
                && next.validated_written_contiguous == current.validated_written_contiguous
                && next.durable_contiguous > current.durable_contiguous
                && next.durable_contiguous == next.validated_written_contiguous
                && next.validated_written_contiguous == next.received
        }
        _ => false,
    }
}

fn byte_position_matches_state(position: ByteCreditPosition, state: State) -> bool {
    match state {
        State::BytesReceived => {
            position.received > position.validated_written_contiguous
                && position.validated_written_contiguous >= position.durable_contiguous
        }
        State::BytesWritten => {
            position.received >= position.validated_written_contiguous
                && position.validated_written_contiguous > position.durable_contiguous
        }
        State::BytesDurable => {
            position.durable_contiguous > 0
                && position.durable_contiguous == position.validated_written_contiguous
                && position.validated_written_contiguous == position.received
        }
        _ => false,
    }
}

fn uses_generic_effect_wrapper(kind: MachineKind) -> bool {
    !matches!(
        kind,
        MachineKind::Ffmpeg | MachineKind::FilesystemCapability
    )
}

fn initial(kind: MachineKind) -> State {
    match kind {
        MachineKind::JobCancellation => State::JobQueued,
        MachineKind::SourceRedirect => State::SourceNew,
        MachineKind::AtomicAdmission => State::AdmissionWaiting,
        MachineKind::ByteCreditDurability => State::BytesEmpty,
        MachineKind::Live => State::LiveStarting,
        MachineKind::Sink => State::SinkPending,
        MachineKind::Ffmpeg => State::FfmpegPrepared,
        MachineKind::JavascriptWorker => State::JavascriptIdle,
        MachineKind::PluginIpc => State::PluginDisconnected,
        MachineKind::CommitArchiveReconciliation => State::CommitWorking,
        MachineKind::FilesystemCapability => State::FilesystemUnknown,
        MachineKind::Watcher => State::WatcherStarting,
    }
}

#[allow(clippy::too_many_lines)]
fn belongs(kind: MachineKind, state: State) -> bool {
    if matches!(state, State::EffectPending | State::EffectRecovery) {
        return uses_generic_effect_wrapper(kind);
    }
    matches!(
        (kind, state),
        (
            MachineKind::JobCancellation,
            State::JobQueued
                | State::JobRunning
                | State::JobCancelling
                | State::JobVerifying
                | State::JobSucceeded
                | State::JobFailed
                | State::JobCancelled
        ) | (
            MachineKind::SourceRedirect,
            State::SourceNew
                | State::SourceResolving
                | State::SourceRedirecting
                | State::SourceResolved
                | State::SourceFailed
                | State::SourceCancelled
        ) | (
            MachineKind::AtomicAdmission,
            State::AdmissionWaiting
                | State::AdmissionGranted
                | State::AdmissionReleased
                | State::AdmissionCancelled
        ) | (
            MachineKind::ByteCreditDurability,
            State::BytesEmpty
                | State::BytesReceived
                | State::BytesWriting
                | State::BytesWritten
                | State::BytesSynchronizing
                | State::BytesDurable
                | State::BytesFailed
                | State::BytesCancelled
        ) | (
            MachineKind::Live,
            State::LiveStarting
                | State::LiveRefreshing
                | State::LiveStreaming
                | State::LiveStopped
                | State::LiveFailed
                | State::LiveCancelled
        ) | (
            MachineKind::Sink,
            State::SinkPending
                | State::SinkActive
                | State::SinkDraining
                | State::SinkCompleted
                | State::SinkDropped
                | State::SinkFailed
                | State::SinkCancelled
        ) | (
            MachineKind::Ffmpeg,
            State::FfmpegPrepared
                | State::FfmpegSpawning
                | State::FfmpegSpawned
                | State::FfmpegRunning
                | State::FfmpegReaping
                | State::FfmpegCancelling
                | State::FfmpegFailing
                | State::FfmpegExitReleasing
                | State::FfmpegCancellationReleasing
                | State::FfmpegFailureReleasing
                | State::FfmpegSpawnRecovering
                | State::FfmpegReapRecovering
                | State::FfmpegCancellationRecovering
                | State::FfmpegFailureRecovering
                | State::FfmpegExitReleaseRecovering
                | State::FfmpegCancellationReleaseRecovering
                | State::FfmpegFailureReleaseRecovering
                | State::FfmpegExited
                | State::FfmpegFailed
                | State::FfmpegCancelled
        ) | (
            MachineKind::JavascriptWorker,
            State::JavascriptIdle
                | State::JavascriptAssigned
                | State::JavascriptRunning
                | State::JavascriptRecycling
                | State::JavascriptQuarantined
                | State::JavascriptCompleted
                | State::JavascriptCancelled
        ) | (
            MachineKind::PluginIpc,
            State::PluginDisconnected
                | State::PluginHandshaking
                | State::PluginReady
                | State::PluginInFlight
                | State::PluginDraining
                | State::PluginStopped
                | State::PluginFailed
        ) | (
            MachineKind::CommitArchiveReconciliation,
            State::CommitWorking
                | State::CommitPreparing
                | State::CommitDataSynchronizing
                | State::CommitPreparedDirectorySynchronizing
                | State::CommitPreparedRecordAppending
                | State::CommitPreparedJournalSynchronizing
                | State::CommitPrepared
                | State::CommitRenaming
                | State::CommitRenameDirectorySynchronizing
                | State::CommitRenamedRecordAppending
                | State::CommitRenamedJournalSynchronizing
                | State::CommitRenamed
                | State::CommitArchiving
                | State::CommitArchivedRecordAppending
                | State::CommitArchivedJournalSynchronizing
                | State::CommitArchived
                | State::CommitCleaning
                | State::CommitCleanedRecordAppending
                | State::CommitCleanedJournalSynchronizing
                | State::CommitCleaned
                | State::CommitReconciling
                | State::CommitVerifyingPrepared
                | State::CommitVerifyingRenamed
                | State::CommitVerifyingArchived
                | State::CommitVerifyingCleaned
                | State::CommitCancelling
                | State::CommitReconciled
                | State::CommitInconsistent
                | State::CommitCancelled
        ) | (
            MachineKind::FilesystemCapability,
            State::FilesystemUnknown
                | State::FilesystemProbing
                | State::FilesystemProbed
                | State::FilesystemProbeFailed
                | State::FilesystemProbeCancelled
                | State::FilesystemConfining
                | State::FilesystemConfinementFailed
                | State::FilesystemConfinementCancelled
                | State::FilesystemConfined
                | State::FilesystemDegrading
                | State::FilesystemDegradationFailed
                | State::FilesystemDegradationCancelled
                | State::FilesystemDegraded
                | State::FilesystemRejecting
                | State::FilesystemRejectionFailed
                | State::FilesystemRejectionCancelled
                | State::FilesystemUnsupported
                | State::FilesystemCancelled
        ) | (
            MachineKind::Watcher,
            State::WatcherStarting
                | State::WatcherReady
                | State::WatcherServing
                | State::WatcherDegraded
                | State::WatcherStale
                | State::WatcherDraining
                | State::WatcherStopped
        )
    )
}

/// Exact public-restoration whitelist represented by the contract inventory's
/// `durable_prefixes` field. An empty slice is represented as `["none"]` in
/// the inventory because its schema requires a nonempty explanatory array.
#[must_use]
pub const fn durable_states(kind: MachineKind) -> &'static [State] {
    match kind {
        MachineKind::JobCancellation => &[State::JobSucceeded],
        MachineKind::SourceRedirect => &[State::SourceResolved],
        MachineKind::AtomicAdmission | MachineKind::JavascriptWorker | MachineKind::PluginIpc => {
            &[]
        }
        MachineKind::ByteCreditDurability => &[
            State::BytesReceived,
            State::BytesWritten,
            State::BytesDurable,
        ],
        MachineKind::Live => &[State::LiveStreaming],
        MachineKind::Sink => &[State::SinkActive, State::SinkCompleted],
        MachineKind::Ffmpeg => &[State::FfmpegExited],
        MachineKind::CommitArchiveReconciliation => &[
            State::CommitPrepared,
            State::CommitRenamed,
            State::CommitArchived,
            State::CommitCleaned,
            State::CommitReconciled,
        ],
        MachineKind::FilesystemCapability => &[
            State::FilesystemConfined,
            State::FilesystemDegraded,
            State::FilesystemUnsupported,
        ],
        MachineKind::Watcher => &[
            State::WatcherReady,
            State::WatcherServing,
            State::WatcherDegraded,
            State::WatcherStale,
            State::WatcherStopped,
        ],
    }
}

fn is_durable_state(kind: MachineKind, state: State) -> bool {
    durable_states(kind).contains(&state)
}

const COMMIT_PREPARE_EFFECTS: &[EffectIntent] = &[EffectIntent::ValidateOutput];
const COMMIT_RENAME_EFFECTS: &[EffectIntent] = &[EffectIntent::RenameOutput];

fn acknowledgement_effects(state: State, emitted: &[EffectIntent]) -> Vec<EffectIntent> {
    let required: &[EffectIntent] = match state {
        State::JobCancelling => &[EffectIntent::RequestCancellation],
        State::JobVerifying
        | State::CommitReconciling
        | State::CommitVerifyingArchived
        | State::CommitVerifyingCleaned => &[EffectIntent::VerifyArchiveOutputPair],
        State::BytesWriting => &[EffectIntent::ValidateAndWrite],
        State::BytesSynchronizing | State::CommitDataSynchronizing => {
            &[EffectIntent::SynchronizeData]
        }
        State::FfmpegSpawning => &[EffectIntent::SpawnProcess],
        State::FfmpegReaping => &[EffectIntent::ReapProcess],
        State::FfmpegCancelling => &[EffectIntent::TerminateProcess, EffectIntent::ReapProcess],
        State::FfmpegFailing => &[
            EffectIntent::TerminateProcess,
            EffectIntent::ReapProcess,
            EffectIntent::PreserveDiagnostics,
        ],
        State::FfmpegExitReleasing
        | State::FfmpegCancellationReleasing
        | State::FfmpegFailureReleasing => &[EffectIntent::ReleaseResources],
        State::FfmpegSpawnRecovering
        | State::FfmpegReapRecovering
        | State::FfmpegCancellationRecovering
        | State::FfmpegFailureRecovering
        | State::FfmpegExitReleaseRecovering
        | State::FfmpegCancellationReleaseRecovering
        | State::FfmpegFailureReleaseRecovering => &[EffectIntent::PreserveDiagnostics],
        State::JavascriptRecycling => &[EffectIntent::RecycleWorker],
        State::PluginDraining => &[EffectIntent::ClosePluginChannel],
        State::CommitPreparing => COMMIT_PREPARE_EFFECTS,
        State::CommitPreparedDirectorySynchronizing | State::CommitRenameDirectorySynchronizing => {
            &[EffectIntent::SynchronizeDirectory]
        }
        State::CommitPreparedRecordAppending
        | State::CommitRenamedRecordAppending
        | State::CommitArchivedRecordAppending
        | State::CommitCleanedRecordAppending => &[EffectIntent::AppendTransitionRecord],
        State::CommitPreparedJournalSynchronizing
        | State::CommitRenamedJournalSynchronizing
        | State::CommitArchivedJournalSynchronizing
        | State::CommitCleanedJournalSynchronizing => &[EffectIntent::SynchronizeJournal],
        State::CommitRenaming => COMMIT_RENAME_EFFECTS,
        State::CommitArchiving => &[EffectIntent::InsertArchiveRow],
        State::CommitCleaning => &[EffectIntent::RemoveTemporaryState],
        State::CommitVerifyingPrepared => &[EffectIntent::RevalidatePreparedOutput],
        State::CommitVerifyingRenamed => &[EffectIntent::VerifyFinalArtifact],
        State::CommitCancelling => &[EffectIntent::DrainInFlightEffect],
        State::FilesystemProbing => &[EffectIntent::ProbeFilesystem],
        State::FilesystemConfining => &[EffectIntent::EstablishConfinedPath],
        State::FilesystemDegrading => &[EffectIntent::ReportDegradedGuarantees],
        State::FilesystemRejecting => &[EffectIntent::RejectFilesystem],
        State::WatcherDraining => &[EffectIntent::FlushWatcher],
        _ => &[],
    };
    required
        .iter()
        .copied()
        .filter(|effect| emitted.contains(effect))
        .collect()
}

fn unsuccessful_effect_recovery(
    kind: MachineKind,
    state: State,
) -> Option<(State, Vec<EffectIntent>)> {
    match kind {
        MachineKind::Ffmpeg => ffmpeg_recovery_after_all_outcomes(state),
        _ => None,
    }
}

fn transition(
    kind: MachineKind,
    state: State,
    event: Event,
) -> Result<(State, Vec<EffectIntent>), TransitionError> {
    if !belongs(kind, state) {
        return Err(TransitionError::StateDoesNotBelongToMachine { kind, state });
    }
    let result = match kind {
        MachineKind::JobCancellation => job(state, event),
        MachineKind::SourceRedirect => source(state, event),
        MachineKind::AtomicAdmission => admission(state, event),
        MachineKind::ByteCreditDurability => bytes(state, event),
        MachineKind::Live => live(state, event),
        MachineKind::Sink => sink(state, event),
        MachineKind::Ffmpeg => ffmpeg(state, event),
        MachineKind::JavascriptWorker => javascript(state, event),
        MachineKind::PluginIpc => plugin(state, event),
        MachineKind::CommitArchiveReconciliation => commit(state, event),
        MachineKind::FilesystemCapability => filesystem(state, event),
        MachineKind::Watcher => watcher(state, event),
    };
    result.ok_or(TransitionError::InvalidTransition { kind, state, event })
}

#[allow(clippy::unnecessary_wraps)]
fn step(next: State, effects: &[EffectIntent]) -> Option<(State, Vec<EffectIntent>)> {
    Some((next, effects.to_vec()))
}

fn job(state: State, event: Event) -> Option<(State, Vec<EffectIntent>)> {
    match (state, event) {
        (State::JobQueued, Event::Start) => step(State::JobRunning, &[EffectIntent::BeginJob]),
        (State::JobQueued, Event::Cancel) => step(State::JobCancelled, &[]),
        (State::JobRunning | State::JobVerifying, Event::Cancel) => {
            step(State::JobCancelling, &[EffectIntent::RequestCancellation])
        }
        (State::JobCancelling, Event::EffectAcknowledged { .. }) => {
            step(State::JobCancelled, &[EffectIntent::ReleaseResources])
        }
        (State::JobRunning, Event::Reconcile) => step(
            State::JobVerifying,
            &[EffectIntent::VerifyArchiveOutputPair],
        ),
        (State::JobVerifying, Event::EffectAcknowledged { .. }) => {
            step(State::JobSucceeded, &[EffectIntent::ReleaseResources])
        }
        (State::JobRunning | State::JobCancelling | State::JobVerifying, Event::Fail) => step(
            State::JobFailed,
            &[
                EffectIntent::PreserveDiagnostics,
                EffectIntent::ReleaseResources,
            ],
        ),
        (State::JobFailed | State::JobCancelled, Event::Restart) => step(State::JobQueued, &[]),
        _ => None,
    }
}

fn source(state: State, event: Event) -> Option<(State, Vec<EffectIntent>)> {
    match (state, event) {
        (State::SourceNew, Event::Start) | (State::SourceRedirecting, Event::Continue) => {
            step(State::SourceResolving, &[EffectIntent::ResolveSource])
        }
        (State::SourceResolving, Event::Redirect) => {
            step(State::SourceRedirecting, &[EffectIntent::FollowRedirect])
        }
        (State::SourceResolving, Event::Complete) => step(State::SourceResolved, &[]),
        (State::SourceNew | State::SourceResolving | State::SourceRedirecting, Event::Cancel) => {
            step(State::SourceCancelled, &[])
        }
        (State::SourceResolving | State::SourceRedirecting, Event::Fail) => {
            step(State::SourceFailed, &[EffectIntent::PreserveDiagnostics])
        }
        (State::SourceFailed | State::SourceCancelled, Event::Restart) => {
            step(State::SourceNew, &[])
        }
        _ => None,
    }
}

fn admission(state: State, event: Event) -> Option<(State, Vec<EffectIntent>)> {
    match (state, event) {
        (State::AdmissionWaiting, Event::Admit) => {
            step(State::AdmissionGranted, &[EffectIntent::ReserveResources])
        }
        (State::AdmissionGranted, Event::Release) => {
            step(State::AdmissionReleased, &[EffectIntent::ReleaseResources])
        }
        (State::AdmissionWaiting, Event::Cancel) => step(State::AdmissionCancelled, &[]),
        (State::AdmissionGranted, Event::Cancel) => {
            step(State::AdmissionCancelled, &[EffectIntent::ReleaseResources])
        }
        _ => None,
    }
}

fn bytes(state: State, event: Event) -> Option<(State, Vec<EffectIntent>)> {
    match (state, event) {
        (State::BytesEmpty | State::BytesReceived, Event::Receive) => {
            step(State::BytesReceived, &[EffectIntent::AcceptBoundedBytes])
        }
        (State::BytesReceived, Event::Validate) => {
            step(State::BytesWriting, &[EffectIntent::ValidateAndWrite])
        }
        (State::BytesWriting, Event::EffectAcknowledged { .. }) => step(State::BytesWritten, &[]),
        (State::BytesWritten, Event::PersistDurably) => {
            step(State::BytesSynchronizing, &[EffectIntent::SynchronizeData])
        }
        (State::BytesSynchronizing, Event::EffectAcknowledged { .. }) => {
            step(State::BytesDurable, &[])
        }
        (
            State::BytesEmpty
            | State::BytesReceived
            | State::BytesWriting
            | State::BytesWritten
            | State::BytesSynchronizing,
            Event::Cancel,
        ) => step(State::BytesCancelled, &[EffectIntent::ReleaseResources]),
        (
            State::BytesReceived
            | State::BytesWriting
            | State::BytesWritten
            | State::BytesSynchronizing,
            Event::Fail,
        ) => step(
            State::BytesFailed,
            &[
                EffectIntent::PreserveDiagnostics,
                EffectIntent::ReleaseResources,
            ],
        ),
        (State::BytesFailed | State::BytesCancelled, Event::Restart) => {
            step(State::BytesEmpty, &[])
        }
        _ => None,
    }
}

fn live(state: State, event: Event) -> Option<(State, Vec<EffectIntent>)> {
    match (state, event) {
        (State::LiveStarting, Event::Ready) | (State::LiveStreaming, Event::Refresh) => {
            step(State::LiveRefreshing, &[EffectIntent::RefreshManifest])
        }
        (State::LiveRefreshing, Event::Serve) => {
            step(State::LiveStreaming, &[EffectIntent::DeliverToSink])
        }
        (State::LiveStreaming | State::LiveRefreshing, Event::Drain) => {
            step(State::LiveStopped, &[EffectIntent::DrainSink])
        }
        (State::LiveStarting | State::LiveRefreshing | State::LiveStreaming, Event::Cancel) => {
            step(State::LiveCancelled, &[EffectIntent::ReleaseResources])
        }
        (State::LiveRefreshing | State::LiveStreaming, Event::Fail) => {
            step(State::LiveFailed, &[EffectIntent::PreserveDiagnostics])
        }
        (State::LiveFailed | State::LiveCancelled, Event::Restart) => {
            step(State::LiveStarting, &[])
        }
        _ => None,
    }
}

fn sink(state: State, event: Event) -> Option<(State, Vec<EffectIntent>)> {
    match (state, event) {
        (State::SinkPending, Event::Start) => {
            step(State::SinkActive, &[EffectIntent::DeliverToSink])
        }
        (State::SinkActive, Event::Drain) => step(State::SinkDraining, &[EffectIntent::DrainSink]),
        (State::SinkDraining, Event::Complete) => step(State::SinkCompleted, &[]),
        (State::SinkActive, Event::Reject) => {
            step(State::SinkDropped, &[EffectIntent::ReleaseResources])
        }
        (State::SinkPending | State::SinkActive | State::SinkDraining, Event::Cancel) => {
            step(State::SinkCancelled, &[EffectIntent::ReleaseResources])
        }
        (State::SinkActive | State::SinkDraining, Event::Fail) => step(
            State::SinkFailed,
            &[
                EffectIntent::PreserveDiagnostics,
                EffectIntent::ReleaseResources,
            ],
        ),
        _ => None,
    }
}

fn ffmpeg(state: State, event: Event) -> Option<(State, Vec<EffectIntent>)> {
    ffmpeg_recovery(state, event).or_else(|| match (state, event) {
        (State::FfmpegPrepared, Event::Spawn) | (State::FfmpegSpawnRecovering, Event::Restart) => {
            step(State::FfmpegSpawning, &[EffectIntent::SpawnProcess])
        }
        (State::FfmpegSpawning, Event::EffectAcknowledged { .. }) => {
            step(State::FfmpegSpawned, &[])
        }
        (State::FfmpegSpawned, Event::Start) => step(State::FfmpegRunning, &[]),
        (State::FfmpegRunning, Event::Complete) | (State::FfmpegReapRecovering, Event::Restart) => {
            step(State::FfmpegReaping, &[EffectIntent::ReapProcess])
        }
        (State::FfmpegReaping, Event::EffectAcknowledged { .. })
        | (State::FfmpegExitReleaseRecovering, Event::Restart) => step(
            State::FfmpegExitReleasing,
            &[EffectIntent::ReleaseResources],
        ),
        (State::FfmpegExitReleasing, Event::EffectAcknowledged { .. }) => {
            step(State::FfmpegExited, &[])
        }
        (State::FfmpegPrepared, Event::Cancel)
        | (State::FfmpegCancelling, Event::EffectAcknowledged { .. })
        | (State::FfmpegCancellationReleaseRecovering, Event::Restart) => step(
            State::FfmpegCancellationReleasing,
            &[EffectIntent::ReleaseResources],
        ),
        (State::FfmpegCancellationReleasing, Event::EffectAcknowledged { .. }) => {
            step(State::FfmpegCancelled, &[])
        }
        (State::FfmpegSpawned | State::FfmpegRunning, Event::Cancel)
        | (State::FfmpegCancellationRecovering, Event::Restart) => step(
            State::FfmpegCancelling,
            &[EffectIntent::TerminateProcess, EffectIntent::ReapProcess],
        ),
        (State::FfmpegSpawned | State::FfmpegRunning | State::FfmpegReaping, Event::Fail)
        | (State::FfmpegFailureRecovering, Event::Restart) => step(
            State::FfmpegFailing,
            &[
                EffectIntent::TerminateProcess,
                EffectIntent::ReapProcess,
                EffectIntent::PreserveDiagnostics,
            ],
        ),
        (State::FfmpegFailing, Event::EffectAcknowledged { .. })
        | (State::FfmpegFailureReleaseRecovering, Event::Restart) => step(
            State::FfmpegFailureReleasing,
            &[EffectIntent::ReleaseResources],
        ),
        (State::FfmpegFailureReleasing, Event::EffectAcknowledged { .. }) => {
            step(State::FfmpegFailed, &[])
        }
        (State::FfmpegFailed | State::FfmpegCancelled, Event::Restart) => {
            step(State::FfmpegPrepared, &[])
        }
        _ => None,
    })
}

fn ffmpeg_recovery(state: State, event: Event) -> Option<(State, Vec<EffectIntent>)> {
    if matches!(
        event,
        Event::EffectFailed { .. } | Event::EffectCancelled { .. }
    ) && let Some(recovery) = ffmpeg_recovery_after_all_outcomes(state)
    {
        return Some(recovery);
    }
    match (state, event) {
        (
            State::FfmpegSpawnRecovering
            | State::FfmpegReapRecovering
            | State::FfmpegCancellationRecovering
            | State::FfmpegFailureRecovering
            | State::FfmpegExitReleaseRecovering
            | State::FfmpegCancellationReleaseRecovering
            | State::FfmpegFailureReleaseRecovering,
            Event::EffectAcknowledged { .. }
            | Event::EffectFailed { .. }
            | Event::EffectCancelled { .. },
        ) => step(state, &[]),
        _ => None,
    }
}

fn ffmpeg_recovery_after_all_outcomes(state: State) -> Option<(State, Vec<EffectIntent>)> {
    match state {
        State::FfmpegSpawning => step(
            State::FfmpegSpawnRecovering,
            &[EffectIntent::PreserveDiagnostics],
        ),
        State::FfmpegReaping => step(
            State::FfmpegReapRecovering,
            &[EffectIntent::PreserveDiagnostics],
        ),
        State::FfmpegCancelling => step(
            State::FfmpegCancellationRecovering,
            &[EffectIntent::PreserveDiagnostics],
        ),
        State::FfmpegFailing => step(
            State::FfmpegFailureRecovering,
            &[EffectIntent::PreserveDiagnostics],
        ),
        State::FfmpegExitReleasing => step(
            State::FfmpegExitReleaseRecovering,
            &[EffectIntent::PreserveDiagnostics],
        ),
        State::FfmpegCancellationReleasing => step(
            State::FfmpegCancellationReleaseRecovering,
            &[EffectIntent::PreserveDiagnostics],
        ),
        State::FfmpegFailureReleasing => step(
            State::FfmpegFailureReleaseRecovering,
            &[EffectIntent::PreserveDiagnostics],
        ),
        _ => None,
    }
}

fn javascript(state: State, event: Event) -> Option<(State, Vec<EffectIntent>)> {
    match (state, event) {
        (State::JavascriptIdle, Event::Assign) => {
            step(State::JavascriptAssigned, &[EffectIntent::DispatchWorker])
        }
        (State::JavascriptAssigned, Event::Start) => step(State::JavascriptRunning, &[]),
        (State::JavascriptRunning, Event::Complete) => step(State::JavascriptCompleted, &[]),
        (State::JavascriptRunning, Event::Recycle) => {
            step(State::JavascriptRecycling, &[EffectIntent::RecycleWorker])
        }
        (State::JavascriptRecycling, Event::EffectAcknowledged { .. }) => {
            step(State::JavascriptIdle, &[])
        }
        (State::JavascriptAssigned | State::JavascriptRunning, Event::Quarantine) => step(
            State::JavascriptQuarantined,
            &[
                EffectIntent::IsolateWorker,
                EffectIntent::PreserveDiagnostics,
            ],
        ),
        (State::JavascriptQuarantined, Event::Restart) => {
            step(State::JavascriptIdle, &[EffectIntent::RecycleWorker])
        }
        (State::JavascriptAssigned | State::JavascriptRunning, Event::Cancel) => step(
            State::JavascriptCancelled,
            &[EffectIntent::ReleaseResources],
        ),
        _ => None,
    }
}

fn plugin(state: State, event: Event) -> Option<(State, Vec<EffectIntent>)> {
    match (state, event) {
        (State::PluginDisconnected, Event::Start) => {
            step(State::PluginHandshaking, &[EffectIntent::OpenPluginChannel])
        }
        (State::PluginHandshaking, Event::Ready) | (State::PluginInFlight, Event::Complete) => {
            step(State::PluginReady, &[])
        }
        (State::PluginReady, Event::Assign) => {
            step(State::PluginInFlight, &[EffectIntent::SendPluginRequest])
        }
        (
            State::PluginHandshaking | State::PluginReady | State::PluginInFlight,
            Event::Drain | Event::Cancel,
        ) => step(State::PluginDraining, &[EffectIntent::ClosePluginChannel]),
        (State::PluginDraining, Event::EffectAcknowledged { .. }) => {
            step(State::PluginStopped, &[])
        }
        (
            State::PluginHandshaking
            | State::PluginReady
            | State::PluginInFlight
            | State::PluginDraining,
            Event::Fail,
        ) => step(
            State::PluginFailed,
            &[
                EffectIntent::ClosePluginChannel,
                EffectIntent::PreserveDiagnostics,
            ],
        ),
        (State::PluginFailed | State::PluginStopped, Event::Restart) => {
            step(State::PluginDisconnected, &[])
        }
        _ => None,
    }
}

#[allow(clippy::too_many_lines)]
fn commit(state: State, event: Event) -> Option<(State, Vec<EffectIntent>)> {
    match (state, event) {
        (State::CommitWorking, Event::Prepare) => {
            step(State::CommitPreparing, COMMIT_PREPARE_EFFECTS)
        }
        (State::CommitPreparing, Event::EffectAcknowledged { .. }) => step(
            State::CommitDataSynchronizing,
            &[EffectIntent::SynchronizeData],
        ),
        (State::CommitDataSynchronizing, Event::EffectAcknowledged { .. }) => step(
            State::CommitPreparedDirectorySynchronizing,
            &[EffectIntent::SynchronizeDirectory],
        ),
        (State::CommitPreparedDirectorySynchronizing, Event::EffectAcknowledged { .. }) => step(
            State::CommitPreparedRecordAppending,
            &[EffectIntent::AppendTransitionRecord],
        ),
        (State::CommitPreparedRecordAppending, Event::EffectAcknowledged { .. }) => step(
            State::CommitPreparedJournalSynchronizing,
            &[EffectIntent::SynchronizeJournal],
        ),
        (
            State::CommitPreparedJournalSynchronizing | State::CommitVerifyingPrepared,
            Event::EffectAcknowledged { .. },
        ) => step(State::CommitPrepared, &[]),
        (State::CommitPrepared, Event::Rename) => {
            step(State::CommitRenaming, COMMIT_RENAME_EFFECTS)
        }
        (State::CommitRenaming, Event::EffectAcknowledged { .. }) => step(
            State::CommitRenameDirectorySynchronizing,
            &[EffectIntent::SynchronizeDirectory],
        ),
        (State::CommitRenameDirectorySynchronizing, Event::EffectAcknowledged { .. }) => step(
            State::CommitRenamedRecordAppending,
            &[EffectIntent::AppendTransitionRecord],
        ),
        (State::CommitRenamedRecordAppending, Event::EffectAcknowledged { .. }) => step(
            State::CommitRenamedJournalSynchronizing,
            &[EffectIntent::SynchronizeJournal],
        ),
        (
            State::CommitRenamedJournalSynchronizing | State::CommitVerifyingRenamed,
            Event::EffectAcknowledged { .. },
        ) => step(State::CommitRenamed, &[]),
        (State::CommitRenamed, Event::Archive) => {
            step(State::CommitArchiving, &[EffectIntent::InsertArchiveRow])
        }
        (State::CommitArchiving, Event::EffectAcknowledged { .. }) => step(
            State::CommitArchivedRecordAppending,
            &[EffectIntent::AppendTransitionRecord],
        ),
        (State::CommitArchivedRecordAppending, Event::EffectAcknowledged { .. }) => step(
            State::CommitArchivedJournalSynchronizing,
            &[EffectIntent::SynchronizeJournal],
        ),
        (
            State::CommitArchivedJournalSynchronizing | State::CommitVerifyingArchived,
            Event::EffectAcknowledged { .. },
        ) => step(State::CommitArchived, &[]),
        (State::CommitArchived, Event::Cleanup) => {
            step(State::CommitCleaning, &[EffectIntent::RemoveTemporaryState])
        }
        (State::CommitCleaning, Event::EffectAcknowledged { .. }) => step(
            State::CommitCleanedRecordAppending,
            &[EffectIntent::AppendTransitionRecord],
        ),
        (State::CommitCleanedRecordAppending, Event::EffectAcknowledged { .. }) => step(
            State::CommitCleanedJournalSynchronizing,
            &[EffectIntent::SynchronizeJournal],
        ),
        (
            State::CommitCleanedJournalSynchronizing | State::CommitVerifyingCleaned,
            Event::EffectAcknowledged { .. },
        ) => step(State::CommitCleaned, &[]),
        (State::CommitCleaned, Event::Reconcile) => step(
            State::CommitReconciling,
            &[EffectIntent::VerifyArchiveOutputPair],
        ),
        (State::CommitReconciling, Event::EffectAcknowledged { .. }) => {
            step(State::CommitReconciled, &[])
        }
        (State::CommitPrepared, Event::Restart | Event::Reconcile) => step(
            State::CommitVerifyingPrepared,
            &[EffectIntent::RevalidatePreparedOutput],
        ),
        (State::CommitRenamed, Event::Restart | Event::Reconcile) => step(
            State::CommitVerifyingRenamed,
            &[EffectIntent::VerifyFinalArtifact],
        ),
        (State::CommitArchived, Event::Restart | Event::Reconcile) => step(
            State::CommitVerifyingArchived,
            &[EffectIntent::VerifyArchiveOutputPair],
        ),
        (State::CommitCleaned, Event::Restart) => step(
            State::CommitVerifyingCleaned,
            &[EffectIntent::VerifyArchiveOutputPair],
        ),
        (
            State::CommitWorking
            | State::CommitPreparing
            | State::CommitDataSynchronizing
            | State::CommitPreparedDirectorySynchronizing
            | State::CommitPreparedRecordAppending
            | State::CommitPreparedJournalSynchronizing
            | State::CommitPrepared
            | State::CommitRenaming
            | State::CommitRenameDirectorySynchronizing
            | State::CommitRenamedRecordAppending
            | State::CommitRenamedJournalSynchronizing
            | State::CommitRenamed
            | State::CommitArchiving
            | State::CommitArchivedRecordAppending
            | State::CommitArchivedJournalSynchronizing
            | State::CommitArchived
            | State::CommitCleaning
            | State::CommitCleanedRecordAppending
            | State::CommitCleanedJournalSynchronizing
            | State::CommitCleaned,
            Event::Fail,
        ) => step(
            State::CommitInconsistent,
            &[EffectIntent::PreserveDiagnostics],
        ),
        (
            State::CommitWorking
            | State::CommitPreparing
            | State::CommitDataSynchronizing
            | State::CommitPreparedDirectorySynchronizing
            | State::CommitPreparedRecordAppending
            | State::CommitPreparedJournalSynchronizing
            | State::CommitPrepared
            | State::CommitRenaming
            | State::CommitRenameDirectorySynchronizing
            | State::CommitRenamedRecordAppending
            | State::CommitRenamedJournalSynchronizing
            | State::CommitRenamed
            | State::CommitArchiving
            | State::CommitArchivedRecordAppending
            | State::CommitArchivedJournalSynchronizing
            | State::CommitArchived
            | State::CommitCleaning
            | State::CommitCleanedRecordAppending
            | State::CommitCleanedJournalSynchronizing
            | State::CommitCleaned
            | State::CommitReconciling
            | State::CommitVerifyingPrepared
            | State::CommitVerifyingRenamed
            | State::CommitVerifyingArchived
            | State::CommitVerifyingCleaned,
            Event::Cancel,
        ) => step(
            State::CommitCancelling,
            &[
                EffectIntent::PreserveDiagnostics,
                EffectIntent::DrainInFlightEffect,
            ],
        ),
        (State::CommitCancelling, Event::EffectAcknowledged { .. }) => {
            step(State::CommitCancelled, &[EffectIntent::ReleaseResources])
        }
        _ => None,
    }
}

fn filesystem(state: State, event: Event) -> Option<(State, Vec<EffectIntent>)> {
    match (state, event) {
        (State::FilesystemUnknown, Event::Probe) => {
            step(State::FilesystemProbing, &[EffectIntent::ProbeFilesystem])
        }
        (State::FilesystemProbing, Event::EffectAcknowledged { .. }) => {
            step(State::FilesystemProbed, &[])
        }
        (State::FilesystemProbing, Event::EffectFailed { .. }) => {
            step(State::FilesystemProbeFailed, &[])
        }
        (State::FilesystemProbing, Event::EffectCancelled { .. }) => {
            step(State::FilesystemProbeCancelled, &[])
        }
        (State::FilesystemProbed, Event::Confine) => step(
            State::FilesystemConfining,
            &[EffectIntent::EstablishConfinedPath],
        ),
        (State::FilesystemConfining, Event::EffectAcknowledged { .. }) => {
            step(State::FilesystemConfined, &[])
        }
        (State::FilesystemConfining, Event::EffectFailed { .. }) => {
            step(State::FilesystemConfinementFailed, &[])
        }
        (State::FilesystemConfining, Event::EffectCancelled { .. }) => {
            step(State::FilesystemConfinementCancelled, &[])
        }
        (State::FilesystemProbed, Event::Degrade) => step(
            State::FilesystemDegrading,
            &[EffectIntent::ReportDegradedGuarantees],
        ),
        (State::FilesystemDegrading, Event::EffectAcknowledged { .. }) => {
            step(State::FilesystemDegraded, &[])
        }
        (State::FilesystemDegrading, Event::EffectFailed { .. }) => {
            step(State::FilesystemDegradationFailed, &[])
        }
        (State::FilesystemDegrading, Event::EffectCancelled { .. }) => {
            step(State::FilesystemDegradationCancelled, &[])
        }
        (State::FilesystemProbed, Event::Reject) => step(
            State::FilesystemRejecting,
            &[EffectIntent::RejectFilesystem],
        ),
        (State::FilesystemRejecting, Event::EffectAcknowledged { .. }) => {
            step(State::FilesystemUnsupported, &[])
        }
        (State::FilesystemRejecting, Event::EffectFailed { .. }) => {
            step(State::FilesystemRejectionFailed, &[])
        }
        (State::FilesystemRejecting, Event::EffectCancelled { .. }) => {
            step(State::FilesystemRejectionCancelled, &[])
        }
        (State::FilesystemProbed, Event::Cancel) => step(State::FilesystemCancelled, &[]),
        (State::FilesystemProbeFailed | State::FilesystemProbeCancelled, Event::Restart) => {
            step(State::FilesystemUnknown, &[])
        }
        (
            State::FilesystemConfinementFailed
            | State::FilesystemConfinementCancelled
            | State::FilesystemDegradationFailed
            | State::FilesystemDegradationCancelled
            | State::FilesystemRejectionFailed
            | State::FilesystemRejectionCancelled,
            Event::Restart,
        ) => step(State::FilesystemProbed, &[]),
        (
            State::FilesystemDegraded | State::FilesystemUnsupported | State::FilesystemCancelled,
            Event::Restart,
        ) => step(State::FilesystemUnknown, &[]),
        _ => None,
    }
}

fn watcher(state: State, event: Event) -> Option<(State, Vec<EffectIntent>)> {
    match (state, event) {
        (State::WatcherStarting, Event::Ready) => step(
            State::WatcherReady,
            &[
                EffectIntent::NegotiateWatcher,
                EffectIntent::SelfTestWatcherStorage,
            ],
        ),
        (State::WatcherReady, Event::Serve) => {
            step(State::WatcherServing, &[EffectIntent::VerifyProducerCanary])
        }
        (
            State::WatcherStarting
            | State::WatcherReady
            | State::WatcherServing
            | State::WatcherStale,
            Event::Degrade,
        ) => step(
            State::WatcherDegraded,
            &[EffectIntent::ReportDiagnosticsDegraded],
        ),
        (State::WatcherServing, Event::MarkStale) => step(
            State::WatcherStale,
            &[EffectIntent::ReportDiagnosticsDegraded],
        ),
        (
            State::WatcherStarting
            | State::WatcherReady
            | State::WatcherServing
            | State::WatcherDegraded
            | State::WatcherStale,
            Event::Drain | Event::Cancel,
        ) => step(State::WatcherDraining, &[EffectIntent::FlushWatcher]),
        (State::WatcherDraining, Event::EffectAcknowledged { .. }) => {
            step(State::WatcherStopped, &[])
        }
        (State::WatcherDegraded | State::WatcherStale | State::WatcherStopped, Event::Restart) => {
            step(State::WatcherStarting, &[EffectIntent::PreserveDiagnostics])
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::{ByteCreditComponent, ByteCreditContractV1, ByteCreditStage, OwnerId};

    fn instance(value: u64) -> MachineInstanceId {
        MachineInstanceId::new(value).expect("test instance identity must be nonzero")
    }

    fn machine(kind: MachineKind, trace_limit: usize) -> StateMachine {
        StateMachine::new(kind, instance(1), trace_limit)
    }

    #[derive(Debug, PartialEq, Eq)]
    struct StateMachineSnapshot {
        instance_id: MachineInstanceId,
        kind: MachineKind,
        state: State,
        trace_limit: usize,
        trace: Vec<Transition>,
        pending_acknowledgements: Vec<EffectAcknowledgement>,
        pending_unsuccessful_effect_outcome: bool,
        deferred_effects: Option<DeferredEffects>,
        next_effect_generation: u64,
        byte_durability_acknowledgement_authorized: bool,
        byte_durability_position: Option<ByteCreditPosition>,
    }

    fn snapshot(model: &StateMachine) -> StateMachineSnapshot {
        StateMachineSnapshot {
            instance_id: model.instance_id,
            kind: model.kind,
            state: model.state,
            trace_limit: model.trace_limit,
            trace: model.trace.clone(),
            pending_acknowledgements: model.pending_acknowledgements.clone(),
            pending_unsuccessful_effect_outcome: model.pending_unsuccessful_effect_outcome,
            deferred_effects: model.deferred_effects.clone(),
            next_effect_generation: model.next_effect_generation,
            byte_durability_acknowledgement_authorized: model
                .byte_durability_acknowledgement_authorized,
            byte_durability_position: model
                .byte_durability_broker
                .as_ref()
                .map(OwnedByteCreditBroker::position),
        }
    }

    fn fork_for_test(model: &StateMachine) -> StateMachine {
        StateMachine {
            instance_id: model.instance_id,
            kind: model.kind,
            state: model.state,
            trace_limit: model.trace_limit,
            trace: model.trace.clone(),
            pending_acknowledgements: model.pending_acknowledgements.clone(),
            pending_unsuccessful_effect_outcome: model.pending_unsuccessful_effect_outcome,
            deferred_effects: model.deferred_effects.clone(),
            next_effect_generation: model.next_effect_generation,
            byte_durability_acknowledgement_authorized: model
                .byte_durability_acknowledgement_authorized,
            byte_durability_broker: model.byte_durability_broker.clone(),
        }
    }

    fn pending_acknowledgement_event(model: &StateMachine) -> Event {
        let pending = model.pending_acknowledgements()[0];
        Event::EffectAcknowledged {
            instance_id: pending.instance_id,
            effect: pending.effect,
            generation: pending.generation,
        }
    }

    fn acknowledge_byte_effect(
        model: &mut StateMachine,
        broker: &OwnedByteCreditBroker,
        next: ByteCreditPosition,
    ) {
        let event = pending_acknowledgement_event(model);
        model
            .acknowledge_byte_durability_effect(broker, event, next)
            .expect("WP009-REG-DURABLE-EFFECT-ACK-001 correlated effect receipt");
    }

    fn assert_partial_terminal_durability_rejected(
        model: &mut StateMachine,
        broker: &OwnedByteCreditBroker,
    ) {
        let synchronize_event = pending_acknowledgement_event(model);
        assert_eq!(
            model.acknowledge_byte_durability_effect(
                broker,
                synchronize_event,
                ByteCreditPosition {
                    received: 8,
                    validated_written_contiguous: 8,
                    durable_contiguous: 4,
                },
            ),
            Err(ByteDurabilityEffectError::PositionDoesNotMatchEffect {
                effect: EffectIntent::SynchronizeData,
            }),
            "a terminal BytesDurable acknowledgement must cover the complete written prefix"
        );
        assert_eq!(model.state(), State::EffectPending);
        assert_eq!(broker.position().durable_contiguous, 0);
    }

    fn received_byte_machine(trace_limit: usize) -> (StateMachine, OwnedByteCreditBroker) {
        let contract = ByteCreditContractV1::new(8, 2);
        let broker = OwnedByteCreditBroker::from_contract(&contract).expect("valid broker");
        let mut lease = broker
            .claim(OwnerId(1), ByteCreditStage::HttpReceive, 4)
            .expect("HTTP receive claim");
        lease
            .consume(ByteCreditComponent::Single, 4)
            .expect("consume received bytes");
        let mut model = machine(MachineKind::ByteCreditDurability, trace_limit);
        model.apply(Event::Receive).expect("receive effect emitted");
        acknowledge_byte_effect(
            &mut model,
            &broker,
            ByteCreditPosition {
                received: 4,
                validated_written_contiguous: 0,
                durable_contiguous: 0,
            },
        );
        (model, broker)
    }

    fn acknowledge_effect(model: &mut StateMachine, effect: EffectIntent) {
        let acknowledgement = model
            .pending_acknowledgements()
            .iter()
            .find(|acknowledgement| acknowledgement.effect == effect)
            .copied()
            .expect("requested effect must be pending");
        assert!(
            model
                .apply(Event::EffectAcknowledged {
                    instance_id: acknowledgement.instance_id,
                    effect: acknowledgement.effect,
                    generation: acknowledgement.generation,
                })
                .is_ok()
        );
    }

    fn unsuccessful_outcome(cancelled: bool, acknowledgement: EffectAcknowledgement) -> Event {
        if cancelled {
            Event::EffectCancelled {
                instance_id: acknowledgement.instance_id,
                effect: acknowledgement.effect,
                generation: acknowledgement.generation,
            }
        } else {
            Event::EffectFailed {
                instance_id: acknowledgement.instance_id,
                effect: acknowledgement.effect,
                generation: acknowledgement.generation,
            }
        }
    }

    fn started_ffmpeg(trace_limit: usize) -> StateMachine {
        let mut model = machine(MachineKind::Ffmpeg, trace_limit);
        assert!(model.apply(Event::Spawn).is_ok());
        acknowledge_all(&mut model);
        assert!(model.apply(Event::Start).is_ok());
        model
    }

    fn probed_filesystem(trace_limit: usize) -> StateMachine {
        let mut model = machine(MachineKind::FilesystemCapability, trace_limit);
        assert!(model.apply(Event::Probe).is_ok());
        acknowledge_all(&mut model);
        assert_eq!(model.state(), State::FilesystemProbed);
        model
    }

    fn acknowledge_all(model: &mut StateMachine) {
        let pending = model.pending_acknowledgements().to_vec();
        assert!(!pending.is_empty(), "ack marker requires pending effects");
        for acknowledgement in pending {
            assert!(
                model
                    .apply(Event::EffectAcknowledged {
                        instance_id: acknowledgement.instance_id,
                        effect: acknowledgement.effect,
                        generation: acknowledgement.generation,
                    })
                    .is_ok()
            );
        }
    }

    fn complete_pending_effects(model: &mut StateMachine) {
        while !model.pending_acknowledgements().is_empty() {
            acknowledge_all(model);
        }
    }

    fn assert_not_durable_ffmpeg_releasing(state: State) {
        assert!(matches!(
            StateMachine::from_state(MachineKind::Ffmpeg, state, instance(9), 1, 1,),
            Err(TransitionError::StateIsNotDurable { .. })
        ));
    }

    fn run(kind: MachineKind, events: &[Event], expected: State) -> StateMachine {
        let mut model = machine(kind, events.len().saturating_mul(2).saturating_add(2));
        for event in events {
            if *event == Event::Acknowledge {
                complete_pending_effects(&mut model);
            } else {
                assert!(model.apply(*event).is_ok());
            }
        }
        assert_eq!(model.state(), expected);
        model
    }

    fn recovery_observation(prefix: CommitState) -> RecoveryObservation {
        let (staged_output, final_output, archive_row, cleanup) = match &prefix {
            CommitState::Collecting | CommitState::Prepared => (
                IdentityObservation::MatchesJournal,
                IdentityObservation::Missing,
                IdentityObservation::Missing,
                CleanupObservation::NotStarted,
            ),
            CommitState::Renamed => (
                IdentityObservation::Missing,
                IdentityObservation::MatchesJournal,
                IdentityObservation::Missing,
                CleanupObservation::NotStarted,
            ),
            CommitState::Archived => (
                IdentityObservation::Missing,
                IdentityObservation::MatchesJournal,
                IdentityObservation::MatchesJournal,
                CleanupObservation::NotStarted,
            ),
            CommitState::Cleaned => (
                IdentityObservation::Missing,
                IdentityObservation::MatchesJournal,
                IdentityObservation::MatchesJournal,
                CleanupObservation::Complete,
            ),
            CommitState::Inconsistent => (
                IdentityObservation::Missing,
                IdentityObservation::Missing,
                IdentityObservation::Missing,
                CleanupObservation::NotStarted,
            ),
        };
        RecoveryObservation {
            durable_prefix: prefix,
            staged_output,
            final_output,
            archive_row,
            lease: LeaseObservation::OwnedByJob,
            cleanup,
            migration: MigrationObservation::Complete,
            volume_relationship: VolumeRelationship::SameFilesystem,
            collision: CollisionObservation::None,
            confinement: RecoveryConfinementObservation::Proven,
        }
    }

    #[test]
    fn recovery_actions_are_repeat_safe_and_converge_from_every_durable_prefix() {
        let cases = [
            (
                CommitState::Collecting,
                RecoveryDecision::Act(RecoveryAction::ResumeOrRestartVerifiedData),
            ),
            (
                CommitState::Prepared,
                RecoveryDecision::Act(RecoveryAction::RevalidatePreparedThenRename),
            ),
            (
                CommitState::Renamed,
                RecoveryDecision::Act(RecoveryAction::InsertArchiveRow),
            ),
            (
                CommitState::Archived,
                RecoveryDecision::Act(RecoveryAction::RepeatCleanup),
            ),
            (CommitState::Cleaned, RecoveryDecision::ReconciledSuccess),
            (
                CommitState::Inconsistent,
                RecoveryDecision::FailClosed(RecoveryFailure::ExistingInconsistentPrefix),
            ),
        ];

        for (prefix, expected) in cases {
            let observation = recovery_observation(prefix);
            let decision = decide_recovery(&observation);
            assert_eq!(decision, expected);
            if let RecoveryDecision::Act(action) = decision {
                let application = apply_recovery_action(&observation, action)
                    .expect("WP009-REG-RECOVERY-IDEMPOTENCE-001 action applies");
                assert_eq!(application.action(), action);
                assert_eq!(application.prior_observation(), &observation);
                let after_once = application.resulting_observation();
                let after_twice = application
                    .retry(action)
                    .expect("WP009-REG-RECOVERY-IDEMPOTENCE-001 receipt retries safely");
                assert_eq!(after_twice, after_once);
                let next_decision = decide_recovery(after_once);
                if action != RecoveryAction::ResumeOrRestartVerifiedData {
                    assert_ne!(next_decision, decision);
                }
            }
        }
    }

    #[test]
    fn recovery_actions_cover_every_declared_mutation_with_repeat_safety() {
        let mut cases = Vec::new();

        let mut stale = recovery_observation(CommitState::Prepared);
        stale.lease = LeaseObservation::Stale;
        cases.push(stale);

        let mut migration = recovery_observation(CommitState::Prepared);
        migration.migration = MigrationObservation::Interrupted;
        cases.push(migration);

        let mut cross_volume = recovery_observation(CommitState::Prepared);
        cross_volume.volume_relationship = VolumeRelationship::CrossVolume;
        cases.push(cross_volume);

        let mut prepared_with_final = recovery_observation(CommitState::Prepared);
        prepared_with_final.staged_output = IdentityObservation::Missing;
        prepared_with_final.final_output = IdentityObservation::MatchesJournal;
        cases.push(prepared_with_final);

        let mut archive_without_final = recovery_observation(CommitState::Archived);
        archive_without_final.staged_output = IdentityObservation::MatchesJournal;
        archive_without_final.final_output = IdentityObservation::Missing;
        cases.push(archive_without_final);

        let mut prepared_with_pair = recovery_observation(CommitState::Prepared);
        prepared_with_pair.staged_output = IdentityObservation::Missing;
        prepared_with_pair.final_output = IdentityObservation::MatchesJournal;
        prepared_with_pair.archive_row = IdentityObservation::MatchesJournal;
        cases.push(prepared_with_pair);

        for observation in cases {
            let RecoveryDecision::Act(action) = decide_recovery(&observation) else {
                panic!("WP009-REG-RECOVERY-IDEMPOTENCE-001 expected an action");
            };
            let application = apply_recovery_action(&observation, action)
                .expect("WP009-REG-RECOVERY-IDEMPOTENCE-001 action applies");
            assert_eq!(application.action(), action);
            assert_eq!(application.prior_observation(), &observation);
            let after_once = application.resulting_observation();
            let after_twice = application
                .retry(action)
                .expect("WP009-REG-RECOVERY-IDEMPOTENCE-001 receipt retries safely");
            assert_eq!(after_twice, after_once);
        }
    }

    #[test]
    fn recovery_preconditions_fail_closed_before_mutating_actions() {
        let mut collecting_archive = recovery_observation(CommitState::Collecting);
        collecting_archive.archive_row = IdentityObservation::MatchesJournal;
        assert_eq!(
            decide_recovery(&collecting_archive),
            RecoveryDecision::FailClosed(RecoveryFailure::UnexpectedArtifactBeforePrepared),
            "WP009-REG-COLLECTING-ARCHIVE-001"
        );

        let mut stale_mismatch = recovery_observation(CommitState::Prepared);
        stale_mismatch.lease = LeaseObservation::Stale;
        stale_mismatch.staged_output = IdentityObservation::Mismatched;
        assert_eq!(
            decide_recovery(&stale_mismatch),
            RecoveryDecision::FailClosed(RecoveryFailure::MismatchedStagedOutput),
            "WP009-REG-RECOVERY-PRECEDENCE-001"
        );
        assert_eq!(
            apply_recovery_action(&stale_mismatch, RecoveryAction::ReclaimStaleLease),
            Err(RecoveryApplicationError::ActionNotApplicable {
                action: RecoveryAction::ReclaimStaleLease
            }),
            "WP009-REG-RECOVERY-PRECEDENCE-001 fail-closed observations cannot be mutated"
        );

        let mut migration_collision = recovery_observation(CommitState::Prepared);
        migration_collision.migration = MigrationObservation::Interrupted;
        migration_collision.collision = CollisionObservation::ConflictingExistingOutput;
        assert_eq!(
            decide_recovery(&migration_collision),
            RecoveryDecision::FailClosed(RecoveryFailure::ConflictingDestination),
            "WP009-REG-RECOVERY-PRECEDENCE-001"
        );

        let mut unsafe_confinement = recovery_observation(CommitState::Prepared);
        unsafe_confinement.lease = LeaseObservation::Stale;
        unsafe_confinement.confinement = RecoveryConfinementObservation::Unavailable;
        assert_eq!(
            decide_recovery(&unsafe_confinement),
            RecoveryDecision::FailClosed(RecoveryFailure::ConfinementUnavailable),
            "WP009-REG-RECOVERY-PRECEDENCE-001"
        );
    }

    #[test]
    fn recovery_executor_rejects_actions_not_selected_by_the_oracle() {
        let same_filesystem = recovery_observation(CommitState::Prepared);
        assert_eq!(
            decide_recovery(&same_filesystem),
            RecoveryDecision::Act(RecoveryAction::RevalidatePreparedThenRename)
        );
        assert_eq!(
            apply_recovery_action(
                &same_filesystem,
                RecoveryAction::CopySyncRenameWithinDestination,
            ),
            Err(RecoveryApplicationError::ActionNotApplicable {
                action: RecoveryAction::CopySyncRenameWithinDestination,
            })
        );
        assert_eq!(
            apply_recovery_action(&same_filesystem, RecoveryAction::ReclaimStaleLease),
            Err(RecoveryApplicationError::ActionNotApplicable {
                action: RecoveryAction::ReclaimStaleLease,
            }),
            "WP009-REG-RECOVERY-LINEAGE-001 an observational after-state is not retry authority"
        );

        let rename_application = apply_recovery_action(
            &same_filesystem,
            RecoveryAction::RevalidatePreparedThenRename,
        )
        .expect("the exact oracle-selected rename action applies");
        assert_eq!(
            rename_application.resulting_observation().durable_prefix,
            CommitState::Renamed
        );
        assert_eq!(
            rename_application
                .retry(RecoveryAction::RevalidatePreparedThenRename)
                .expect("the same receipt action retries without re-execution"),
            rename_application.resulting_observation()
        );
        assert_eq!(
            rename_application.retry(RecoveryAction::CopySyncRenameWithinDestination),
            Err(RecoveryApplicationError::RetryActionMismatch {
                applied: RecoveryAction::RevalidatePreparedThenRename,
                requested: RecoveryAction::CopySyncRenameWithinDestination,
            })
        );
        assert_eq!(
            rename_application.clone().into_result(),
            rename_application.resulting_observation().clone()
        );

        let mut cross_volume = same_filesystem.clone();
        cross_volume.volume_relationship = VolumeRelationship::CrossVolume;
        assert_eq!(
            decide_recovery(&cross_volume),
            RecoveryDecision::Act(RecoveryAction::CopySyncRenameWithinDestination)
        );
        assert_eq!(
            apply_recovery_action(&cross_volume, RecoveryAction::RevalidatePreparedThenRename,),
            Err(RecoveryApplicationError::ActionNotApplicable {
                action: RecoveryAction::RevalidatePreparedThenRename,
            })
        );

        let mut prepared_final = same_filesystem;
        prepared_final.staged_output = IdentityObservation::Missing;
        prepared_final.final_output = IdentityObservation::MatchesJournal;
        assert_eq!(
            decide_recovery(&prepared_final),
            RecoveryDecision::Act(RecoveryAction::VerifyFinalArtifactThenArchive)
        );
        assert_eq!(
            apply_recovery_action(&prepared_final, RecoveryAction::InsertArchiveRow),
            Err(RecoveryApplicationError::ActionNotApplicable {
                action: RecoveryAction::InsertArchiveRow,
            })
        );
    }

    #[test]
    fn recovery_oracle_covers_collision_lease_cleanup_migration_and_cross_volume() {
        let mut cross_volume = recovery_observation(CommitState::Prepared);
        cross_volume.volume_relationship = VolumeRelationship::CrossVolume;
        assert_eq!(
            decide_recovery(&cross_volume),
            RecoveryDecision::Act(RecoveryAction::CopySyncRenameWithinDestination)
        );

        let mut stale_lease = recovery_observation(CommitState::Prepared);
        stale_lease.lease = LeaseObservation::Stale;
        assert_eq!(
            decide_recovery(&stale_lease),
            RecoveryDecision::Act(RecoveryAction::ReclaimStaleLease)
        );
        stale_lease.lease = LeaseObservation::HeldByOther;
        assert_eq!(
            decide_recovery(&stale_lease),
            RecoveryDecision::FailClosed(RecoveryFailure::LeaseHeldByOther)
        );

        let mut collision = recovery_observation(CommitState::Prepared);
        collision.collision = CollisionObservation::ConflictingExistingOutput;
        assert_eq!(
            decide_recovery(&collision),
            RecoveryDecision::FailClosed(RecoveryFailure::ConflictingDestination)
        );
        collision.collision = CollisionObservation::IdenticalExistingOutput;
        collision.final_output = IdentityObservation::MatchesJournal;
        assert_eq!(
            decide_recovery(&collision),
            RecoveryDecision::Act(RecoveryAction::VerifyFinalArtifactThenArchive)
        );

        let mut partial_cleanup = recovery_observation(CommitState::Archived);
        partial_cleanup.cleanup = CleanupObservation::Partial;
        assert_eq!(
            decide_recovery(&partial_cleanup),
            RecoveryDecision::Act(RecoveryAction::RepeatCleanup)
        );
        partial_cleanup.durable_prefix = CommitState::Renamed;
        partial_cleanup.archive_row = IdentityObservation::Missing;
        assert_eq!(
            decide_recovery(&partial_cleanup),
            RecoveryDecision::FailClosed(RecoveryFailure::CleanupBeforeArchive)
        );

        let mut migration = recovery_observation(CommitState::Archived);
        migration.migration = MigrationObservation::Interrupted;
        assert_eq!(
            decide_recovery(&migration),
            RecoveryDecision::Act(RecoveryAction::ResumeInterruptedMigration)
        );
    }

    #[test]
    fn archive_without_output_restores_only_from_matching_staged_artifact() {
        let mut recoverable = recovery_observation(CommitState::Archived);
        recoverable.final_output = IdentityObservation::Missing;
        recoverable.staged_output = IdentityObservation::MatchesJournal;
        assert_eq!(
            decide_recovery(&recoverable),
            RecoveryDecision::Act(RecoveryAction::RestoreFinalFromStaged)
        );

        recoverable.staged_output = IdentityObservation::Missing;
        assert_eq!(
            decide_recovery(&recoverable),
            RecoveryDecision::FailClosed(RecoveryFailure::ArchiveWithoutRecoverableOutput)
        );
    }

    #[test]
    fn identity_mismatches_fail_closed_before_any_recovery_action() {
        let baseline = recovery_observation(CommitState::Archived);
        let mut staged = baseline.clone();
        staged.staged_output = IdentityObservation::Mismatched;
        let mut final_output = baseline.clone();
        final_output.final_output = IdentityObservation::Mismatched;
        let mut archive = baseline;
        archive.archive_row = IdentityObservation::Mismatched;

        for (observation, expected) in [
            (
                staged,
                RecoveryDecision::FailClosed(RecoveryFailure::MismatchedStagedOutput),
            ),
            (
                final_output,
                RecoveryDecision::FailClosed(RecoveryFailure::MismatchedFinalOutput),
            ),
            (
                archive,
                RecoveryDecision::FailClosed(RecoveryFailure::MismatchedArchiveRow),
            ),
        ] {
            assert_eq!(decide_recovery(&observation), expected);
        }
    }

    #[test]
    fn success_counterfactuals_remove_every_required_reconciliation_observation() {
        let accepted = recovery_observation(CommitState::Cleaned);
        assert_eq!(
            decide_recovery(&accepted),
            RecoveryDecision::ReconciledSuccess
        );

        let mut mutations = Vec::new();
        let mut missing_archive = accepted.clone();
        missing_archive.archive_row = IdentityObservation::Missing;
        mutations.push(missing_archive);
        let mut missing_output = accepted.clone();
        missing_output.final_output = IdentityObservation::Missing;
        mutations.push(missing_output);
        let mut partial_cleanup = accepted.clone();
        partial_cleanup.cleanup = CleanupObservation::Partial;
        mutations.push(partial_cleanup);
        let mut interrupted_migration = accepted.clone();
        interrupted_migration.migration = MigrationObservation::Interrupted;
        mutations.push(interrupted_migration);
        let mut conflicting_output = accepted.clone();
        conflicting_output.collision = CollisionObservation::ConflictingExistingOutput;
        mutations.push(conflicting_output);
        let mut held_lease = accepted;
        held_lease.lease = LeaseObservation::HeldByOther;
        mutations.push(held_lease);

        for mutation in mutations {
            assert_ne!(
                decide_recovery(&mutation),
                RecoveryDecision::ReconciledSuccess
            );
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn every_named_lifecycle_has_a_success_path() {
        let cases: &[(MachineKind, &[Event], State)] = &[
            (
                MachineKind::JobCancellation,
                &[
                    Event::Start,
                    Event::Acknowledge,
                    Event::Reconcile,
                    Event::Acknowledge,
                ],
                State::JobSucceeded,
            ),
            (
                MachineKind::SourceRedirect,
                &[
                    Event::Start,
                    Event::Acknowledge,
                    Event::Redirect,
                    Event::Acknowledge,
                    Event::Continue,
                    Event::Acknowledge,
                    Event::Complete,
                ],
                State::SourceResolved,
            ),
            (
                MachineKind::AtomicAdmission,
                &[
                    Event::Admit,
                    Event::Acknowledge,
                    Event::Release,
                    Event::Acknowledge,
                ],
                State::AdmissionReleased,
            ),
            (
                MachineKind::Live,
                &[
                    Event::Ready,
                    Event::Acknowledge,
                    Event::Serve,
                    Event::Acknowledge,
                    Event::Drain,
                    Event::Acknowledge,
                ],
                State::LiveStopped,
            ),
            (
                MachineKind::Sink,
                &[
                    Event::Start,
                    Event::Acknowledge,
                    Event::Drain,
                    Event::Acknowledge,
                    Event::Complete,
                ],
                State::SinkCompleted,
            ),
            (
                MachineKind::Ffmpeg,
                &[
                    Event::Spawn,
                    Event::Acknowledge,
                    Event::Start,
                    Event::Complete,
                    Event::Acknowledge,
                ],
                State::FfmpegExited,
            ),
            (
                MachineKind::JavascriptWorker,
                &[
                    Event::Assign,
                    Event::Acknowledge,
                    Event::Start,
                    Event::Complete,
                ],
                State::JavascriptCompleted,
            ),
            (
                MachineKind::PluginIpc,
                &[
                    Event::Start,
                    Event::Acknowledge,
                    Event::Ready,
                    Event::Assign,
                    Event::Acknowledge,
                    Event::Complete,
                    Event::Drain,
                    Event::Acknowledge,
                ],
                State::PluginStopped,
            ),
            (
                MachineKind::CommitArchiveReconciliation,
                &[
                    Event::Prepare,
                    Event::Acknowledge,
                    Event::Rename,
                    Event::Acknowledge,
                    Event::Archive,
                    Event::Acknowledge,
                    Event::Cleanup,
                    Event::Acknowledge,
                    Event::Reconcile,
                    Event::Acknowledge,
                ],
                State::CommitReconciled,
            ),
            (
                MachineKind::FilesystemCapability,
                &[
                    Event::Probe,
                    Event::Acknowledge,
                    Event::Confine,
                    Event::Acknowledge,
                ],
                State::FilesystemConfined,
            ),
            (
                MachineKind::Watcher,
                &[
                    Event::Ready,
                    Event::Acknowledge,
                    Event::Serve,
                    Event::Acknowledge,
                    Event::Drain,
                    Event::Acknowledge,
                ],
                State::WatcherStopped,
            ),
        ];
        for (kind, events, expected) in cases {
            let model = run(*kind, events, *expected);
            let replay =
                StateMachine::replay(*kind, instance(1), model.trace().len(), model.trace());
            assert!(matches!(replay, Ok(replayed) if replayed.state() == *expected));
        }
    }

    #[test]
    fn cancellation_effects_release_or_drain_resources() {
        let job = run(
            MachineKind::JobCancellation,
            &[
                Event::Start,
                Event::Acknowledge,
                Event::Cancel,
                Event::Acknowledge,
            ],
            State::JobCancelled,
        );
        assert!(
            job.trace()
                .iter()
                .any(|record| record.effects.contains(&EffectIntent::ReleaseResources))
        );
        let ffmpeg = run(
            MachineKind::Ffmpeg,
            &[
                Event::Spawn,
                Event::Acknowledge,
                Event::Start,
                Event::Cancel,
                Event::Acknowledge,
            ],
            State::FfmpegCancelled,
        );
        assert!(
            ffmpeg
                .trace()
                .iter()
                .any(|record| record.effects.contains(&EffectIntent::TerminateProcess))
        );
        assert!(
            ffmpeg
                .trace()
                .iter()
                .any(|record| record.effects.contains(&EffectIntent::ReapProcess))
        );
        let watcher = run(
            MachineKind::Watcher,
            &[
                Event::Ready,
                Event::Acknowledge,
                Event::Cancel,
                Event::Acknowledge,
            ],
            State::WatcherStopped,
        );
        assert!(
            watcher
                .trace()
                .iter()
                .any(|record| record.effects.contains(&EffectIntent::FlushWatcher))
        );
    }

    #[test]
    fn cancellation_paths_reach_expected_states() {
        let cases: &[(MachineKind, &[Event], State)] = &[
            (
                MachineKind::SourceRedirect,
                &[Event::Start, Event::Acknowledge, Event::Cancel],
                State::SourceCancelled,
            ),
            (
                MachineKind::AtomicAdmission,
                &[Event::Cancel],
                State::AdmissionCancelled,
            ),
            (
                MachineKind::Live,
                &[Event::Cancel, Event::Acknowledge],
                State::LiveCancelled,
            ),
            (
                MachineKind::Sink,
                &[Event::Cancel, Event::Acknowledge],
                State::SinkCancelled,
            ),
            (
                MachineKind::JavascriptWorker,
                &[
                    Event::Assign,
                    Event::Acknowledge,
                    Event::Cancel,
                    Event::Acknowledge,
                ],
                State::JavascriptCancelled,
            ),
            (
                MachineKind::PluginIpc,
                &[
                    Event::Start,
                    Event::Acknowledge,
                    Event::Ready,
                    Event::Cancel,
                    Event::Acknowledge,
                ],
                State::PluginStopped,
            ),
            (
                MachineKind::Ffmpeg,
                &[Event::Cancel, Event::Acknowledge],
                State::FfmpegCancelled,
            ),
            (
                MachineKind::FilesystemCapability,
                &[Event::Probe, Event::Acknowledge, Event::Cancel],
                State::FilesystemCancelled,
            ),
            (
                MachineKind::Watcher,
                &[Event::Cancel, Event::Acknowledge],
                State::WatcherStopped,
            ),
            (
                MachineKind::CommitArchiveReconciliation,
                &[
                    Event::Prepare,
                    Event::Acknowledge,
                    Event::Cancel,
                    Event::Acknowledge,
                ],
                State::CommitCancelled,
            ),
        ];
        for (kind, events, expected) in cases {
            run(*kind, events, *expected);
        }
    }

    #[test]
    fn restart_preserves_diagnostics_and_resets_safely() {
        let source = run(
            MachineKind::SourceRedirect,
            &[
                Event::Start,
                Event::Acknowledge,
                Event::Fail,
                Event::Acknowledge,
                Event::Restart,
                Event::Acknowledge,
            ],
            State::SourceNew,
        );
        assert!(
            source
                .trace()
                .iter()
                .any(|record| record.effects.contains(&EffectIntent::PreserveDiagnostics))
        );
        let js = run(
            MachineKind::JavascriptWorker,
            &[
                Event::Assign,
                Event::Acknowledge,
                Event::Quarantine,
                Event::Acknowledge,
                Event::Restart,
                Event::Acknowledge,
            ],
            State::JavascriptIdle,
        );
        assert!(
            js.trace()
                .iter()
                .any(|record| record.effects.contains(&EffectIntent::IsolateWorker))
        );
    }

    #[test]
    fn failure_paths_complete_required_effects_before_their_outcome_state() {
        let failures: &[(MachineKind, &[Event], State)] = &[
            (
                MachineKind::JobCancellation,
                &[
                    Event::Start,
                    Event::Acknowledge,
                    Event::Fail,
                    Event::Acknowledge,
                ],
                State::JobFailed,
            ),
            (
                MachineKind::Live,
                &[
                    Event::Ready,
                    Event::Acknowledge,
                    Event::Fail,
                    Event::Acknowledge,
                ],
                State::LiveFailed,
            ),
            (
                MachineKind::Sink,
                &[
                    Event::Start,
                    Event::Acknowledge,
                    Event::Fail,
                    Event::Acknowledge,
                ],
                State::SinkFailed,
            ),
            (
                MachineKind::Ffmpeg,
                &[
                    Event::Spawn,
                    Event::Acknowledge,
                    Event::Fail,
                    Event::Acknowledge,
                    Event::Acknowledge,
                ],
                State::FfmpegFailed,
            ),
            (
                MachineKind::PluginIpc,
                &[
                    Event::Start,
                    Event::Acknowledge,
                    Event::Fail,
                    Event::Acknowledge,
                ],
                State::PluginFailed,
            ),
            (
                MachineKind::CommitArchiveReconciliation,
                &[
                    Event::Prepare,
                    Event::Acknowledge,
                    Event::Fail,
                    Event::Acknowledge,
                ],
                State::CommitInconsistent,
            ),
            (
                MachineKind::FilesystemCapability,
                &[
                    Event::Probe,
                    Event::Acknowledge,
                    Event::Reject,
                    Event::Acknowledge,
                ],
                State::FilesystemUnsupported,
            ),
            (
                MachineKind::Watcher,
                &[Event::Degrade, Event::Acknowledge],
                State::WatcherDegraded,
            ),
        ];
        for (kind, events, expected) in failures {
            run(*kind, events, *expected);
        }
    }

    #[test]
    fn commit_restart_replays_each_durable_prefix_without_false_success() -> Result<(), String> {
        let prefixes = [
            (
                State::CommitPrepared,
                State::CommitVerifyingPrepared,
                EffectIntent::RevalidatePreparedOutput,
            ),
            (
                State::CommitRenamed,
                State::CommitVerifyingRenamed,
                EffectIntent::VerifyFinalArtifact,
            ),
            (
                State::CommitArchived,
                State::CommitVerifyingArchived,
                EffectIntent::VerifyArchiveOutputPair,
            ),
            (
                State::CommitCleaned,
                State::CommitVerifyingCleaned,
                EffectIntent::VerifyArchiveOutputPair,
            ),
        ];
        for (state, _verifying, required) in prefixes {
            let model = StateMachine::from_state(
                MachineKind::CommitArchiveReconciliation,
                state,
                instance(1),
                3,
                100,
            );
            let Ok(mut model) = model else {
                return Err("commit prefix must belong to commit model".to_owned());
            };
            let result = model.apply(Event::Restart);
            assert!(matches!(result, Ok(record) if record.effects == [required]));
            assert_eq!(model.state(), State::EffectPending);
            assert_ne!(model.state(), State::CommitReconciled);
            acknowledge_all(&mut model);
            assert_eq!(model.state(), state);
        }
        Ok(())
    }

    #[test]
    fn success_and_durable_prefixes_require_effect_acknowledgements() {
        let mut job = machine(MachineKind::JobCancellation, 6);
        assert!(job.apply(Event::Start).is_ok());
        assert_eq!(job.state(), State::EffectPending);
        acknowledge_all(&mut job);
        assert_eq!(job.state(), State::JobRunning);
        assert!(job.apply(Event::Reconcile).is_ok());
        assert_eq!(job.state(), State::EffectPending);
        assert_ne!(job.state(), State::JobSucceeded);
        complete_pending_effects(&mut job);
        assert_eq!(job.state(), State::JobSucceeded);

        let mut commit = machine(MachineKind::CommitArchiveReconciliation, 64);
        for (request, acknowledged) in [
            (Event::Prepare, State::CommitPrepared),
            (Event::Rename, State::CommitRenamed),
            (Event::Archive, State::CommitArchived),
            (Event::Cleanup, State::CommitCleaned),
            (Event::Reconcile, State::CommitReconciled),
        ] {
            assert!(commit.apply(request).is_ok());
            assert_eq!(commit.state(), State::EffectPending);
            assert_ne!(commit.state(), acknowledged);
            complete_pending_effects(&mut commit);
            assert_eq!(commit.state(), acknowledged);
        }

        let mut cancelling = machine(MachineKind::CommitArchiveReconciliation, 32);
        assert!(cancelling.apply(Event::Prepare).is_ok());
        complete_pending_effects(&mut cancelling);
        let cancel = cancelling
            .apply(Event::Cancel)
            .expect("cancel begins draining");
        assert_eq!(cancel.next, State::EffectPending);
        assert!(cancel.effects.contains(&EffectIntent::DrainInFlightEffect));
        assert!(!cancel.effects.contains(&EffectIntent::ReleaseResources));
        acknowledge_all(&mut cancelling);
        let acknowledged = cancelling.trace().last().expect("release is requested");
        assert!(
            acknowledged
                .effects
                .contains(&EffectIntent::ReleaseResources)
        );
        assert_eq!(cancelling.state(), State::EffectPending);
        acknowledge_all(&mut cancelling);
        assert_eq!(cancelling.state(), State::CommitCancelled);
    }

    #[test]
    fn byte_durability_positions_require_correlated_effect_acknowledgements() {
        let contract = ByteCreditContractV1::new(16, 4);
        let broker = OwnedByteCreditBroker::from_contract(&contract).expect("valid broker");
        let mut receive_lease = broker
            .claim(OwnerId(1), ByteCreditStage::HttpReceive, 8)
            .expect("HTTP receive claim");
        receive_lease
            .consume(ByteCreditComponent::Single, 8)
            .expect("HTTP adapter consumed received bytes");
        let mut model = machine(MachineKind::ByteCreditDurability, 16);

        model.apply(Event::Receive).expect("receive effect emitted");
        let receive_event = pending_acknowledgement_event(&model);
        assert!(matches!(
            model.apply(receive_event),
            Err(TransitionError::ByteDurabilityAcknowledgementRequired {
                effect: EffectIntent::AcceptBoundedBytes
            })
        ));
        assert_eq!(broker.position(), ByteCreditPosition::default());
        assert_eq!(
            model.acknowledge_byte_durability_effect(
                &broker,
                receive_event,
                ByteCreditPosition::default(),
            ),
            Err(ByteDurabilityEffectError::PositionDoesNotMatchEffect {
                effect: EffectIntent::AcceptBoundedBytes
            }),
            "WP009-REG-DURABLE-EFFECT-ACK-001 a no-op receipt cannot advance the lifecycle"
        );
        acknowledge_byte_effect(
            &mut model,
            &broker,
            ByteCreditPosition {
                received: 8,
                validated_written_contiguous: 0,
                durable_contiguous: 0,
            },
        );
        assert_eq!(model.state(), State::BytesReceived);
        receive_lease
            .release()
            .expect("release received bytes so capacity can be reused");

        let mut writer_lease = broker
            .claim(OwnerId(1), ByteCreditStage::Writer, 8)
            .expect("writer claim");
        writer_lease
            .consume(ByteCreditComponent::Single, 8)
            .expect("writer consumed validated bytes");

        model.apply(Event::Validate).expect("write effect emitted");
        let write_event = pending_acknowledgement_event(&model);
        assert_eq!(
            model.acknowledge_byte_durability_effect(
                &broker,
                write_event,
                ByteCreditPosition {
                    received: 9,
                    validated_written_contiguous: 8,
                    durable_contiguous: 0,
                },
            ),
            Err(ByteDurabilityEffectError::PositionDoesNotMatchEffect {
                effect: EffectIntent::ValidateAndWrite
            })
        );
        acknowledge_byte_effect(
            &mut model,
            &broker,
            ByteCreditPosition {
                received: 8,
                validated_written_contiguous: 8,
                durable_contiguous: 0,
            },
        );
        assert_eq!(model.state(), State::BytesWritten);

        model
            .apply(Event::PersistDurably)
            .expect("sync effect emitted");
        assert_partial_terminal_durability_rejected(&mut model, &broker);
        acknowledge_byte_effect(
            &mut model,
            &broker,
            ByteCreditPosition {
                received: 8,
                validated_written_contiguous: 8,
                durable_contiguous: 8,
            },
        );
        assert_eq!(model.state(), State::BytesDurable);
        assert_eq!(broker.position().durable_contiguous, 8);
    }

    #[test]
    fn byte_durability_trace_replay_cannot_replace_broker_proof() {
        let (model, _broker) = received_byte_machine(4);
        assert!(
            matches!(
                StateMachine::replay(
                    MachineKind::ByteCreditDurability,
                    instance(1),
                    model.trace().len(),
                    model.trace(),
                ),
                Err(TransitionError::ByteDurabilityAcknowledgementRequired {
                    effect: EffectIntent::AcceptBoundedBytes
                })
            ),
            "WP009-REG-DURABLE-EFFECT-ACK-001 trace shape cannot replace broker proof"
        );
    }

    #[test]
    fn byte_durability_restoration_requires_a_matching_broker_position() {
        let (_model, broker) = received_byte_machine(4);
        assert!(matches!(
            StateMachine::from_state(
                MachineKind::ByteCreditDurability,
                State::BytesReceived,
                instance(2),
                4,
                2,
            ),
            Err(TransitionError::ByteDurabilityRestorationEvidenceRequired {
                state: State::BytesReceived
            })
        ));
        assert!(
            StateMachine::from_byte_durability_state(
                State::BytesReceived,
                &broker,
                instance(2),
                4,
                2,
            )
            .is_ok()
        );
        assert!(matches!(
            StateMachine::from_byte_durability_state(
                State::BytesDurable,
                &broker,
                instance(2),
                4,
                2,
            ),
            Err(ByteDurabilityRestorationError::PositionDoesNotMatchState {
                state: State::BytesDurable,
                ..
            })
        ));

        let partial = OwnedByteCreditBroker::from_contract(&ByteCreditContractV1::new(16, 4))
            .expect("valid partial broker");
        let mut receive = partial
            .claim(OwnerId(1), ByteCreditStage::HttpReceive, 8)
            .expect("receive claim");
        receive
            .consume(ByteCreditComponent::Single, 8)
            .expect("receive consumption");
        let mut writer = partial
            .claim(OwnerId(1), ByteCreditStage::Writer, 8)
            .expect("writer claim");
        writer
            .consume(ByteCreditComponent::Single, 8)
            .expect("writer consumption");
        for position in [
            ByteCreditPosition {
                received: 8,
                validated_written_contiguous: 0,
                durable_contiguous: 0,
            },
            ByteCreditPosition {
                received: 8,
                validated_written_contiguous: 8,
                durable_contiguous: 0,
            },
            ByteCreditPosition {
                received: 8,
                validated_written_contiguous: 8,
                durable_contiguous: 4,
            },
        ] {
            let acknowledgement = partial
                .acknowledge_durability_effects(position)
                .expect("position is credit-authorized");
            partial
                .advance(acknowledgement)
                .expect("credit-authorized position advances");
        }
        assert!(matches!(
            StateMachine::from_byte_durability_state(
                State::BytesDurable,
                &partial,
                instance(3),
                4,
                2,
            ),
            Err(ByteDurabilityRestorationError::PositionDoesNotMatchState {
                state: State::BytesDurable,
                ..
            })
        ));
    }

    #[test]
    fn restored_byte_durability_rejects_a_foreign_broker() {
        let (_first_model, broker) = received_byte_machine(8);
        let (_foreign_model, foreign) = received_byte_machine(8);
        let mut broker_writer = broker
            .claim(OwnerId(1), ByteCreditStage::Writer, 4)
            .expect("writer claim on authoritative broker");
        broker_writer
            .consume(ByteCreditComponent::Single, 4)
            .expect("authoritative writer consumed bytes");
        let mut foreign_writer = foreign
            .claim(OwnerId(1), ByteCreditStage::Writer, 4)
            .expect("writer claim on foreign broker");
        foreign_writer
            .consume(ByteCreditComponent::Single, 4)
            .expect("foreign writer consumed bytes");

        let mut restored = StateMachine::from_byte_durability_state(
            State::BytesReceived,
            &broker,
            instance(77),
            8,
            2,
        )
        .expect("matching broker restores the durable state");
        restored
            .apply(Event::Validate)
            .expect("validation effect emitted");
        let event = pending_acknowledgement_event(&restored);
        let next = ByteCreditPosition {
            received: 4,
            validated_written_contiguous: 4,
            durable_contiguous: 0,
        };
        assert_eq!(
            restored.acknowledge_byte_durability_effect(&foreign, event, next),
            Err(ByteDurabilityEffectError::Credit(
                CreditError::AcknowledgementBrokerMismatch,
            ))
        );
        assert_eq!(restored.state(), State::EffectPending);
        assert_eq!(foreign.position().validated_written_contiguous, 0);
        assert!(
            restored
                .acknowledge_byte_durability_effect(&broker, event, next)
                .is_ok()
        );
        assert_eq!(restored.state(), State::BytesWritten);
    }

    #[test]
    fn byte_durability_cancel_and_failure_complete_after_a_proven_receive() {
        for (outcome, expected) in [
            (Event::Cancel, State::BytesCancelled),
            (Event::Fail, State::BytesFailed),
        ] {
            let (mut model, _broker) = received_byte_machine(8);
            model.apply(outcome).expect("outcome effect emitted");
            assert_eq!(model.state(), State::EffectPending);
            acknowledge_all(&mut model);
            assert_eq!(model.state(), expected);
        }
    }

    #[test]
    fn byte_durability_receipts_are_broker_bound() {
        let contract = ByteCreditContractV1::new(8, 2);
        let broker = OwnedByteCreditBroker::from_contract(&contract).expect("valid broker");
        let other = OwnedByteCreditBroker::from_contract(&contract).expect("other broker");
        let foreign = broker
            .acknowledge_durability_effects(broker.position())
            .expect("core token");
        assert_eq!(
            other.advance(foreign),
            Err(CreditError::AcknowledgementBrokerMismatch)
        );
    }

    #[test]
    fn byte_durability_authorization_is_cleared_after_trace_limit_error() {
        let contract = ByteCreditContractV1::new(8, 2);
        let broker = OwnedByteCreditBroker::from_contract(&contract).expect("valid broker");
        let mut lease = broker
            .claim(OwnerId(1), ByteCreditStage::HttpReceive, 4)
            .expect("HTTP receive claim");
        lease
            .consume(ByteCreditComponent::Single, 4)
            .expect("consume credited bytes");
        let mut model = machine(MachineKind::ByteCreditDurability, 1);
        model.apply(Event::Receive).expect("fills trace");
        let event = pending_acknowledgement_event(&model);
        assert!(matches!(
            model.acknowledge_byte_durability_effect(
                &broker,
                event,
                ByteCreditPosition {
                    received: 4,
                    validated_written_contiguous: 0,
                    durable_contiguous: 0,
                },
            ),
            Err(ByteDurabilityEffectError::Transition(
                TransitionError::TraceLimitReached { limit: 1 }
            ))
        ));
        assert!(matches!(
            model.apply(event),
            Err(TransitionError::ByteDurabilityAcknowledgementRequired {
                effect: EffectIntent::AcceptBoundedBytes
            })
        ));
        assert_eq!(broker.position(), ByteCreditPosition::default());
        assert_eq!(model.state(), State::EffectPending);
        assert_eq!(model.trace().len(), 1, "WP009-REG-DURABLE-AUTH-SCOPE-001");
    }

    #[test]
    fn commit_prepared_requires_serial_prerequisite_and_journal_acknowledgements() {
        let preparation_effects = [
            EffectIntent::ValidateOutput,
            EffectIntent::SynchronizeData,
            EffectIntent::SynchronizeDirectory,
            EffectIntent::AppendTransitionRecord,
            EffectIntent::SynchronizeJournal,
        ];
        let mut commit = machine(MachineKind::CommitArchiveReconciliation, 32);
        let prepare = commit
            .apply(Event::Prepare)
            .expect("prepare starts validation");
        assert_eq!(prepare.effects, [EffectIntent::ValidateOutput]);
        for (index, effect) in preparation_effects.into_iter().enumerate() {
            assert_eq!(commit.pending_acknowledgements().len(), 1);
            assert_eq!(commit.pending_acknowledgements()[0].effect, effect);
            assert_ne!(commit.state(), State::CommitPrepared);
            acknowledge_effect(&mut commit, effect);
            if index + 1 != preparation_effects.len() {
                assert_eq!(commit.state(), State::EffectPending);
            }
        }
        assert_eq!(commit.state(), State::CommitPrepared);
        assert!(matches!(
            StateMachine::from_state(
                MachineKind::CommitArchiveReconciliation,
                State::EffectPending,
                instance(9),
                1,
                1,
            ),
            Err(TransitionError::StateIsNotDurable { .. })
        ));

        let mut job = machine(MachineKind::JobCancellation, 8);
        assert!(job.apply(Event::Start).is_ok());
        complete_pending_effects(&mut job);
        assert_eq!(job.state(), State::JobRunning);
        assert!(job.apply(Event::Cancel).is_ok());
        assert_eq!(job.state(), State::EffectPending);
        assert_eq!(
            job.pending_acknowledgements()[0].effect,
            EffectIntent::RequestCancellation
        );
        acknowledge_all(&mut job);
        assert_eq!(job.state(), State::EffectPending);
        assert_eq!(
            job.pending_acknowledgements()[0].effect,
            EffectIntent::ReleaseResources
        );
        assert_ne!(job.state(), State::JobCancelled);
        acknowledge_all(&mut job);
        assert_eq!(job.state(), State::JobCancelled);
    }

    #[test]
    fn renamed_prefix_enforces_causal_order_and_retries_only_failed_phase() {
        let mut commit = machine(MachineKind::CommitArchiveReconciliation, 64);
        assert!(commit.apply(Event::Prepare).is_ok());
        complete_pending_effects(&mut commit);
        assert_eq!(commit.state(), State::CommitPrepared);

        let rename = commit
            .apply(Event::Rename)
            .expect("rename effects are requested");
        assert_eq!(
            rename.effects,
            [EffectIntent::RenameOutput],
            "WP009-REG-RENAME-ORDER-001"
        );
        let rename_ack = commit.pending_acknowledgements()[0];
        assert!(matches!(
            commit.apply(Event::EffectAcknowledged {
                instance_id: rename_ack.instance_id,
                effect: EffectIntent::SynchronizeDirectory,
                generation: rename_ack.generation,
            }),
            Err(TransitionError::UnexpectedAcknowledgement { .. })
        ));
        acknowledge_effect(&mut commit, EffectIntent::RenameOutput);
        assert_eq!(commit.state(), State::EffectPending);
        assert_ne!(commit.state(), State::CommitRenamed);
        assert_eq!(
            commit.pending_acknowledgements()[0].effect,
            EffectIntent::SynchronizeDirectory
        );
        acknowledge_effect(&mut commit, EffectIntent::SynchronizeDirectory);
        assert_eq!(
            commit.pending_acknowledgements()[0].effect,
            EffectIntent::AppendTransitionRecord
        );
        acknowledge_effect(&mut commit, EffectIntent::AppendTransitionRecord);
        assert_eq!(
            commit.pending_acknowledgements()[0].effect,
            EffectIntent::SynchronizeJournal
        );
        acknowledge_effect(&mut commit, EffectIntent::SynchronizeJournal);
        assert_eq!(commit.state(), State::CommitRenamed);

        let mut failed_sync = machine(MachineKind::CommitArchiveReconciliation, 64);
        assert!(failed_sync.apply(Event::Prepare).is_ok());
        complete_pending_effects(&mut failed_sync);
        assert!(failed_sync.apply(Event::Rename).is_ok());
        acknowledge_effect(&mut failed_sync, EffectIntent::RenameOutput);
        let directory_sync = failed_sync
            .pending_acknowledgements()
            .iter()
            .find(|pending| pending.effect == EffectIntent::SynchronizeDirectory)
            .copied()
            .expect("directory synchronization must remain pending");
        assert!(
            failed_sync
                .apply(Event::EffectFailed {
                    instance_id: directory_sync.instance_id,
                    effect: directory_sync.effect,
                    generation: directory_sync.generation,
                })
                .is_ok()
        );
        assert_eq!(failed_sync.state(), State::EffectRecovery);
        assert_ne!(failed_sync.state(), State::CommitRenamed);
        let retry = failed_sync
            .apply(Event::Restart)
            .expect("failed directory phase is retried without repeating rename");
        assert_eq!(
            retry.effects,
            [EffectIntent::SynchronizeDirectory],
            "WP009-REG-RENAME-PARTIAL-001"
        );
        assert!(!retry.effects.contains(&EffectIntent::RenameOutput));
    }

    #[test]
    fn every_commit_prefix_requires_record_append_then_journal_sync() {
        let mut commit = machine(MachineKind::CommitArchiveReconciliation, 64);
        for (request, durable) in [
            (Event::Prepare, State::CommitPrepared),
            (Event::Rename, State::CommitRenamed),
            (Event::Archive, State::CommitArchived),
            (Event::Cleanup, State::CommitCleaned),
        ] {
            commit.apply(request).expect("commit phase starts");
            while commit.pending_acknowledgements()[0].effect
                != EffectIntent::AppendTransitionRecord
            {
                let effect = commit.pending_acknowledgements()[0].effect;
                acknowledge_effect(&mut commit, effect);
                assert_ne!(commit.state(), durable, "WP009-REG-JOURNAL-ACK-001");
            }
            acknowledge_effect(&mut commit, EffectIntent::AppendTransitionRecord);
            assert_ne!(commit.state(), durable, "WP009-REG-JOURNAL-ACK-001");
            assert_eq!(
                commit.pending_acknowledgements()[0].effect,
                EffectIntent::SynchronizeJournal
            );
            acknowledge_effect(&mut commit, EffectIntent::SynchronizeJournal);
            assert_eq!(commit.state(), durable);
        }
    }

    #[test]
    fn generic_effect_outcomes_are_correlated_and_reissue_exact_work() {
        for cancellation in [false, true] {
            let mut commit = machine(MachineKind::CommitArchiveReconciliation, 12);
            assert!(commit.apply(Event::Prepare).is_ok());
            let failed = commit.pending_acknowledgements()[0];
            let wrong = if cancellation {
                commit.apply(Event::EffectCancelled {
                    instance_id: instance(99),
                    effect: failed.effect,
                    generation: failed.generation,
                })
            } else {
                commit.apply(Event::EffectFailed {
                    instance_id: instance(99),
                    effect: failed.effect,
                    generation: failed.generation,
                })
            };
            assert!(matches!(
                wrong,
                Err(TransitionError::UnexpectedEffectFailure { .. }
                    | TransitionError::UnexpectedEffectCancellation { .. })
            ));

            let result = if cancellation {
                commit.apply(Event::EffectCancelled {
                    instance_id: failed.instance_id,
                    effect: failed.effect,
                    generation: failed.generation,
                })
            } else {
                commit.apply(Event::EffectFailed {
                    instance_id: failed.instance_id,
                    effect: failed.effect,
                    generation: failed.generation,
                })
            };
            assert!(result.is_ok());
            assert_eq!(commit.state(), State::EffectRecovery);
            assert!(matches!(
                StateMachine::from_state(
                    MachineKind::CommitArchiveReconciliation,
                    State::EffectRecovery,
                    instance(9),
                    1,
                    1,
                ),
                Err(TransitionError::StateIsNotDurable { .. })
            ));
            let retry = commit
                .apply(Event::Restart)
                .expect("restart retries exact work");
            assert_eq!(retry.next, State::EffectPending);
            assert_eq!(retry.effects, [EffectIntent::ValidateOutput]);
            let stale = if cancellation {
                commit.apply(Event::EffectCancelled {
                    instance_id: failed.instance_id,
                    effect: failed.effect,
                    generation: failed.generation,
                })
            } else {
                commit.apply(Event::EffectFailed {
                    instance_id: failed.instance_id,
                    effect: failed.effect,
                    generation: failed.generation,
                })
            };
            assert!(matches!(
                stale,
                Err(TransitionError::UnexpectedEffectFailure { .. }
                    | TransitionError::UnexpectedEffectCancellation { .. })
            ));
            complete_pending_effects(&mut commit);
            assert_eq!(commit.state(), State::CommitPrepared);
        }
    }

    #[test]
    fn watcher_restart_cannot_claim_ready_before_diagnostics_preserve_completes() {
        let mut watcher = machine(MachineKind::Watcher, 8);
        assert!(watcher.apply(Event::Degrade).is_ok());
        complete_pending_effects(&mut watcher);
        assert_eq!(watcher.state(), State::WatcherDegraded);
        let restart = watcher
            .apply(Event::Restart)
            .expect("restart requests preservation");
        assert_eq!(restart.next, State::EffectPending);
        assert_eq!(restart.effects, [EffectIntent::PreserveDiagnostics]);
        assert_ne!(watcher.state(), State::WatcherStarting);
        acknowledge_all(&mut watcher);
        assert_eq!(watcher.state(), State::WatcherStarting);
    }

    #[test]
    fn filesystem_confinement_waits_for_establishment_acknowledgement() {
        let mut model = machine(MachineKind::FilesystemCapability, 5);
        assert!(model.apply(Event::Probe).is_ok());
        assert_eq!(model.state(), State::FilesystemProbing);
        assert!(matches!(
            model.apply(Event::Confine),
            Err(TransitionError::InvalidTransition { .. })
        ));
        acknowledge_all(&mut model);
        assert_eq!(model.state(), State::FilesystemProbed);
        let confining = model
            .apply(Event::Confine)
            .expect("confinement requests establishment");
        assert_eq!(confining.next, State::FilesystemConfining);
        assert_eq!(model.state(), State::FilesystemConfining);
        assert_ne!(model.state(), State::FilesystemConfined);
        assert_eq!(
            model
                .pending_acknowledgements()
                .iter()
                .map(|acknowledgement| acknowledgement.effect)
                .collect::<Vec<_>>(),
            vec![EffectIntent::EstablishConfinedPath]
        );
        assert!(matches!(
            StateMachine::from_state(
                MachineKind::FilesystemCapability,
                State::FilesystemConfining,
                instance(9),
                1,
                1,
            ),
            Err(TransitionError::StateIsNotDurable { .. })
        ));

        acknowledge_all(&mut model);
        assert_eq!(model.state(), State::FilesystemConfined);
    }

    #[test]
    fn filesystem_confinement_failure_is_correlated() {
        let mut failed = machine(MachineKind::FilesystemCapability, 9);
        assert!(failed.apply(Event::Probe).is_ok());
        acknowledge_all(&mut failed);
        assert!(failed.apply(Event::Confine).is_ok());
        let pending = failed.pending_acknowledgements()[0];
        let before_wrong_failure = snapshot(&failed);
        assert!(matches!(
            failed.apply(Event::EffectFailed {
                instance_id: instance(2),
                effect: pending.effect,
                generation: pending.generation,
            }),
            Err(TransitionError::UnexpectedEffectFailure { .. })
        ));
        assert_eq!(snapshot(&failed), before_wrong_failure);
        assert!(matches!(
            failed.apply(Event::EffectFailed {
                instance_id: pending.instance_id,
                effect: EffectIntent::ProbeFilesystem,
                generation: pending.generation,
            }),
            Err(TransitionError::UnexpectedEffectFailure { .. })
        ));
        assert!(matches!(
            failed.apply(Event::EffectFailed {
                instance_id: pending.instance_id,
                effect: pending.effect,
                generation: pending.generation.saturating_add(1),
            }),
            Err(TransitionError::UnexpectedEffectFailure { .. })
        ));
        assert!(
            failed
                .apply(Event::EffectFailed {
                    instance_id: pending.instance_id,
                    effect: pending.effect,
                    generation: pending.generation,
                })
                .is_ok()
        );
        assert_eq!(failed.state(), State::FilesystemConfinementFailed);
        assert_ne!(failed.state(), State::FilesystemConfined);
        assert!(failed.pending_acknowledgements().is_empty());
        assert!(matches!(
            StateMachine::from_state(
                MachineKind::FilesystemCapability,
                State::FilesystemConfinementFailed,
                instance(9),
                1,
                1,
            ),
            Err(TransitionError::StateIsNotDurable { .. })
        ));
        assert!(failed.apply(Event::Restart).is_ok());
        assert_eq!(failed.state(), State::FilesystemProbed);
    }

    #[test]
    fn filesystem_confinement_cancellation_is_correlated() {
        let mut cancelled = machine(MachineKind::FilesystemCapability, 7);
        assert!(cancelled.apply(Event::Probe).is_ok());
        acknowledge_all(&mut cancelled);
        assert!(cancelled.apply(Event::Confine).is_ok());
        let pending = cancelled.pending_acknowledgements()[0];
        let before_wrong_cancellation = snapshot(&cancelled);
        assert!(matches!(
            cancelled.apply(Event::EffectCancelled {
                instance_id: instance(2),
                effect: pending.effect,
                generation: pending.generation,
            }),
            Err(TransitionError::UnexpectedEffectCancellation { .. })
        ));
        assert_eq!(snapshot(&cancelled), before_wrong_cancellation);
        assert!(matches!(
            cancelled.apply(Event::EffectCancelled {
                instance_id: pending.instance_id,
                effect: EffectIntent::ProbeFilesystem,
                generation: pending.generation,
            }),
            Err(TransitionError::UnexpectedEffectCancellation { .. })
        ));
        assert!(matches!(
            cancelled.apply(Event::EffectCancelled {
                instance_id: pending.instance_id,
                effect: pending.effect,
                generation: pending.generation.saturating_add(1),
            }),
            Err(TransitionError::UnexpectedEffectCancellation { .. })
        ));
        assert!(
            cancelled
                .apply(Event::EffectCancelled {
                    instance_id: pending.instance_id,
                    effect: pending.effect,
                    generation: pending.generation,
                })
                .is_ok()
        );
        assert_eq!(cancelled.state(), State::FilesystemConfinementCancelled);
        assert!(cancelled.pending_acknowledgements().is_empty());
        assert!(matches!(
            cancelled.apply(Event::EffectCancelled {
                instance_id: pending.instance_id,
                effect: pending.effect,
                generation: pending.generation,
            }),
            Err(TransitionError::UnexpectedEffectCancellation { .. })
        ));
    }

    #[test]
    fn filesystem_probe_requires_correlated_completion_or_recovery() {
        let mut probing = machine(MachineKind::FilesystemCapability, 8);
        assert!(probing.apply(Event::Probe).is_ok());
        assert_eq!(probing.state(), State::FilesystemProbing);
        assert_eq!(
            probing.pending_acknowledgements()[0].effect,
            EffectIntent::ProbeFilesystem
        );
        for premature in [
            Event::Confine,
            Event::Degrade,
            Event::Reject,
            Event::Restart,
        ] {
            assert!(matches!(
                probing.apply(premature),
                Err(TransitionError::InvalidTransition { .. })
            ));
        }
        for (outcome, expected) in [
            (false, State::FilesystemProbeFailed),
            (true, State::FilesystemProbeCancelled),
        ] {
            let mut model = machine(MachineKind::FilesystemCapability, 8);
            assert!(model.apply(Event::Probe).is_ok());
            let pending = model.pending_acknowledgements()[0];
            let result = if outcome {
                model.apply(Event::EffectCancelled {
                    instance_id: pending.instance_id,
                    effect: pending.effect,
                    generation: pending.generation,
                })
            } else {
                model.apply(Event::EffectFailed {
                    instance_id: pending.instance_id,
                    effect: pending.effect,
                    generation: pending.generation,
                })
            };
            assert!(result.is_ok());
            assert_eq!(model.state(), expected);
            assert!(model.pending_acknowledgements().is_empty());
            assert!(model.apply(Event::Restart).is_ok());
            let retry = model
                .apply(Event::Probe)
                .expect("probe retry must reissue effect");
            assert_eq!(retry.effects, [EffectIntent::ProbeFilesystem]);
        }
    }

    #[test]
    fn filesystem_post_probe_effects_require_acknowledgement_and_retry() {
        let cases: &[(Event, State, EffectIntent, State, State)] = &[
            (
                Event::Confine,
                State::FilesystemConfining,
                EffectIntent::EstablishConfinedPath,
                State::FilesystemConfinementFailed,
                State::FilesystemConfinementCancelled,
            ),
            (
                Event::Degrade,
                State::FilesystemDegrading,
                EffectIntent::ReportDegradedGuarantees,
                State::FilesystemDegradationFailed,
                State::FilesystemDegradationCancelled,
            ),
            (
                Event::Reject,
                State::FilesystemRejecting,
                EffectIntent::RejectFilesystem,
                State::FilesystemRejectionFailed,
                State::FilesystemRejectionCancelled,
            ),
        ];
        for (request, waiting, effect, failed, cancelled) in cases {
            let mut acknowledged = probed_filesystem(16);
            let transition = acknowledged.apply(*request).expect("effect is requested");
            assert_eq!(transition.next, *waiting);
            assert_eq!(transition.effects, [*effect]);
            assert_eq!(acknowledged.state(), *waiting);
            assert_ne!(acknowledged.state(), State::FilesystemConfined);
            assert_ne!(acknowledged.state(), State::FilesystemDegraded);
            assert_ne!(acknowledged.state(), State::FilesystemUnsupported);
            assert!(matches!(
                StateMachine::from_state(
                    MachineKind::FilesystemCapability,
                    *waiting,
                    instance(9),
                    1,
                    1,
                ),
                Err(TransitionError::StateIsNotDurable { .. })
            ));
            acknowledge_effect(&mut acknowledged, *effect);
            let terminal = match request {
                Event::Confine => State::FilesystemConfined,
                Event::Degrade => State::FilesystemDegraded,
                Event::Reject => State::FilesystemUnsupported,
                _ => unreachable!("filesystem case list is closed"),
            };
            assert_eq!(acknowledged.state(), terminal);

            for cancellation in [false, true] {
                let mut model = probed_filesystem(16);
                assert!(model.apply(*request).is_ok());
                let pending = model.pending_acknowledgements()[0];
                let result = if cancellation {
                    model.apply(Event::EffectCancelled {
                        instance_id: pending.instance_id,
                        effect: pending.effect,
                        generation: pending.generation,
                    })
                } else {
                    model.apply(Event::EffectFailed {
                        instance_id: pending.instance_id,
                        effect: pending.effect,
                        generation: pending.generation,
                    })
                };
                assert!(result.is_ok());
                assert_eq!(
                    model.state(),
                    if cancellation { *cancelled } else { *failed }
                );
                assert!(model.pending_acknowledgements().is_empty());
                assert!(model.apply(Event::Restart).is_ok());
                assert_eq!(model.state(), State::FilesystemProbed);
                let retry = model
                    .apply(*request)
                    .expect("operation retry must reissue effect");
                assert_eq!(retry.effects, [*effect]);
            }
        }
    }

    #[test]
    fn ffmpeg_terminal_states_wait_for_process_and_release_effects() {
        let mut completed = machine(MachineKind::Ffmpeg, 8);
        assert!(completed.apply(Event::Spawn).is_ok());
        acknowledge_all(&mut completed);
        assert!(completed.apply(Event::Start).is_ok());
        assert!(completed.apply(Event::Complete).is_ok());
        acknowledge_all(&mut completed);
        assert_eq!(completed.state(), State::FfmpegExitReleasing);
        assert_ne!(completed.state(), State::FfmpegExited);
        assert_not_durable_ffmpeg_releasing(State::FfmpegExitReleasing);
        acknowledge_all(&mut completed);
        assert_eq!(completed.state(), State::FfmpegExited);

        let mut prepared_cancel = machine(MachineKind::Ffmpeg, 4);
        assert!(prepared_cancel.apply(Event::Cancel).is_ok());
        assert_eq!(prepared_cancel.state(), State::FfmpegCancellationReleasing);
        acknowledge_all(&mut prepared_cancel);
        assert_eq!(prepared_cancel.state(), State::FfmpegCancelled);

        for order in [
            [EffectIntent::TerminateProcess, EffectIntent::ReapProcess],
            [EffectIntent::ReapProcess, EffectIntent::TerminateProcess],
        ] {
            let mut model = machine(MachineKind::Ffmpeg, 8);
            assert!(model.apply(Event::Spawn).is_ok());
            acknowledge_all(&mut model);
            assert!(model.apply(Event::Start).is_ok());
            let cancelling = model.apply(Event::Cancel).expect("cancel begins reaping");
            assert_eq!(cancelling.next, State::FfmpegCancelling);
            assert!(!cancelling.effects.contains(&EffectIntent::ReleaseResources));

            let first = model
                .pending_acknowledgements()
                .iter()
                .find(|acknowledgement| acknowledgement.effect == order[0])
                .copied()
                .expect("requested effect must be pending");
            let partial = model
                .apply(Event::EffectAcknowledged {
                    instance_id: first.instance_id,
                    effect: first.effect,
                    generation: first.generation,
                })
                .expect("first acknowledgement remains pending");
            assert_eq!(partial.next, State::FfmpegCancelling);
            assert!(partial.effects.is_empty());
            assert_eq!(model.state(), State::FfmpegCancelling);
            assert_ne!(model.state(), State::FfmpegCancelled);
            assert_eq!(model.pending_acknowledgements().len(), 1);

            let final_acknowledgement = model.pending_acknowledgements()[0];
            let completed = model
                .apply(Event::EffectAcknowledged {
                    instance_id: final_acknowledgement.instance_id,
                    effect: final_acknowledgement.effect,
                    generation: final_acknowledgement.generation,
                })
                .expect("both process effects complete cancellation");
            assert_eq!(completed.next, State::FfmpegCancellationReleasing);
            assert!(completed.effects.contains(&EffectIntent::ReleaseResources));
            assert_eq!(model.state(), State::FfmpegCancellationReleasing);
            assert_ne!(model.state(), State::FfmpegCancelled);
            assert_eq!(model.pending_acknowledgements().len(), 1);
            acknowledge_all(&mut model);
            assert_eq!(model.state(), State::FfmpegCancelled);
        }

        let mut failing = machine(MachineKind::Ffmpeg, 8);
        assert!(failing.apply(Event::Spawn).is_ok());
        acknowledge_all(&mut failing);
        let failure = failing
            .apply(Event::Fail)
            .expect("failure begins process cleanup");
        assert_eq!(failure.next, State::FfmpegFailing);
        assert!(failure.effects.contains(&EffectIntent::TerminateProcess));
        assert!(failure.effects.contains(&EffectIntent::ReapProcess));
        assert!(!failure.effects.contains(&EffectIntent::ReleaseResources));

        let preserve = failing
            .pending_acknowledgements()
            .iter()
            .find(|acknowledgement| acknowledgement.effect == EffectIntent::PreserveDiagnostics)
            .copied()
            .expect("diagnostic preservation must be pending");
        assert!(
            failing
                .apply(Event::EffectAcknowledged {
                    instance_id: preserve.instance_id,
                    effect: preserve.effect,
                    generation: preserve.generation,
                })
                .is_ok()
        );
        assert_eq!(failing.state(), State::FfmpegFailing);
        assert_eq!(failing.pending_acknowledgements().len(), 2);
        acknowledge_all(&mut failing);
        assert_eq!(failing.state(), State::FfmpegFailureReleasing);
        assert!(
            failing
                .trace()
                .last()
                .is_some_and(|record| record.effects.contains(&EffectIntent::ReleaseResources))
        );
        assert_ne!(failing.state(), State::FfmpegFailed);
        assert_eq!(failing.pending_acknowledgements().len(), 1);
        acknowledge_all(&mut failing);
        assert_eq!(failing.state(), State::FfmpegFailed);
    }

    #[test]
    fn ffmpeg_acknowledgement_prefixes_cannot_skip_cleanup_or_release() {
        for mask in 0_u8..4 {
            let mut model = started_ffmpeg(16);
            assert!(model.apply(Event::Cancel).is_ok());
            for (bit, effect) in [EffectIntent::TerminateProcess, EffectIntent::ReapProcess]
                .into_iter()
                .enumerate()
            {
                if mask & (1 << bit) != 0 {
                    acknowledge_effect(&mut model, effect);
                }
            }
            if mask == 3 {
                assert_eq!(model.state(), State::FfmpegCancellationReleasing);
                assert_eq!(
                    model.pending_acknowledgements()[0].effect,
                    EffectIntent::ReleaseResources
                );
            } else {
                assert_eq!(model.state(), State::FfmpegCancelling);
                assert_ne!(model.state(), State::FfmpegCancellationReleasing);
                assert_ne!(model.state(), State::FfmpegCancelled);
            }
        }

        for mask in 0_u8..8 {
            let mut model = started_ffmpeg(20);
            assert!(model.apply(Event::Fail).is_ok());
            for (bit, effect) in [
                EffectIntent::TerminateProcess,
                EffectIntent::ReapProcess,
                EffectIntent::PreserveDiagnostics,
            ]
            .into_iter()
            .enumerate()
            {
                if mask & (1 << bit) != 0 {
                    acknowledge_effect(&mut model, effect);
                }
            }
            if mask == 7 {
                assert_eq!(model.state(), State::FfmpegFailureReleasing);
                assert_eq!(
                    model.pending_acknowledgements()[0].effect,
                    EffectIntent::ReleaseResources
                );
            } else {
                assert_eq!(model.state(), State::FfmpegFailing);
                assert_ne!(model.state(), State::FfmpegFailureReleasing);
                assert_ne!(model.state(), State::FfmpegFailed);
            }
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn ffmpeg_effect_outcomes_are_correlated_nonterminal_and_recoverable() {
        fn spawning() -> StateMachine {
            let mut model = machine(MachineKind::Ffmpeg, 32);
            assert!(model.apply(Event::Spawn).is_ok());
            model
        }

        fn reaping() -> StateMachine {
            let mut model = started_ffmpeg(32);
            assert!(model.apply(Event::Complete).is_ok());
            model
        }

        fn cancelling() -> StateMachine {
            let mut model = started_ffmpeg(32);
            assert!(model.apply(Event::Cancel).is_ok());
            model
        }

        fn failing() -> StateMachine {
            let mut model = started_ffmpeg(32);
            assert!(model.apply(Event::Fail).is_ok());
            model
        }

        fn exit_releasing() -> StateMachine {
            let mut model = reaping();
            acknowledge_all(&mut model);
            model
        }

        fn cancellation_releasing() -> StateMachine {
            let mut model = cancelling();
            acknowledge_all(&mut model);
            model
        }

        fn failure_releasing() -> StateMachine {
            let mut model = failing();
            acknowledge_all(&mut model);
            model
        }

        type Builder = fn() -> StateMachine;
        let cases: &[(&str, Builder, State, State, &[EffectIntent])] = &[
            (
                "spawn",
                spawning,
                State::FfmpegSpawnRecovering,
                State::FfmpegSpawning,
                &[EffectIntent::SpawnProcess],
            ),
            (
                "reap",
                reaping,
                State::FfmpegReapRecovering,
                State::FfmpegReaping,
                &[EffectIntent::ReapProcess],
            ),
            (
                "exit-release",
                exit_releasing,
                State::FfmpegExitReleaseRecovering,
                State::FfmpegExitReleasing,
                &[EffectIntent::ReleaseResources],
            ),
            (
                "cancellation-release",
                cancellation_releasing,
                State::FfmpegCancellationReleaseRecovering,
                State::FfmpegCancellationReleasing,
                &[EffectIntent::ReleaseResources],
            ),
            (
                "failure-release",
                failure_releasing,
                State::FfmpegFailureReleaseRecovering,
                State::FfmpegFailureReleasing,
                &[EffectIntent::ReleaseResources],
            ),
        ];

        for (label, build, recovery, retry_state, retry_effects) in cases {
            for cancellation in [false, true] {
                for pending in build().pending_acknowledgements().to_vec() {
                    let mut model = build();
                    let before_wrong_receipt = snapshot(&model);
                    let wrong_instance = if cancellation {
                        model.apply(Event::EffectCancelled {
                            instance_id: instance(99),
                            effect: pending.effect,
                            generation: pending.generation,
                        })
                    } else {
                        model.apply(Event::EffectFailed {
                            instance_id: instance(99),
                            effect: pending.effect,
                            generation: pending.generation,
                        })
                    };
                    assert!(
                        matches!(
                            wrong_instance,
                            Err(TransitionError::UnexpectedEffectFailure { .. }
                                | TransitionError::UnexpectedEffectCancellation { .. })
                        ),
                        "{label} must reject cross-instance outcome receipts"
                    );
                    assert_eq!(snapshot(&model), before_wrong_receipt);

                    let result = if cancellation {
                        model.apply(Event::EffectCancelled {
                            instance_id: pending.instance_id,
                            effect: pending.effect,
                            generation: pending.generation,
                        })
                    } else {
                        model.apply(Event::EffectFailed {
                            instance_id: pending.instance_id,
                            effect: pending.effect,
                            generation: pending.generation,
                        })
                    };
                    assert!(result.is_ok(), "{label} must accept its correlated outcome");
                    assert_eq!(model.state(), *recovery);
                    assert_ne!(model.state(), State::FfmpegExited);
                    assert_ne!(model.state(), State::FfmpegCancelled);
                    assert_ne!(model.state(), State::FfmpegFailed);
                    assert_eq!(
                        model.pending_acknowledgements()[0].effect,
                        EffectIntent::PreserveDiagnostics
                    );
                    assert!(matches!(
                        StateMachine::from_state(MachineKind::Ffmpeg, *recovery, instance(9), 1, 1),
                        Err(TransitionError::StateIsNotDurable { .. })
                    ));
                    assert!(matches!(
                        model.apply(Event::Restart),
                        Err(TransitionError::InvalidTransition { .. })
                    ));

                    let stale = if cancellation {
                        model.apply(Event::EffectCancelled {
                            instance_id: pending.instance_id,
                            effect: pending.effect,
                            generation: pending.generation,
                        })
                    } else {
                        model.apply(Event::EffectFailed {
                            instance_id: pending.instance_id,
                            effect: pending.effect,
                            generation: pending.generation,
                        })
                    };
                    assert!(
                        matches!(
                            stale,
                            Err(TransitionError::UnexpectedEffectFailure { .. }
                                | TransitionError::UnexpectedEffectCancellation { .. })
                        ),
                        "{label} must reject stale outcome receipts"
                    );

                    let mut acknowledged = fork_for_test(&model);
                    acknowledge_effect(&mut acknowledged, EffectIntent::PreserveDiagnostics);
                    assert_eq!(acknowledged.state(), *recovery);
                    assert!(acknowledged.pending_acknowledgements().is_empty());
                    let restart = acknowledged
                        .apply(Event::Restart)
                        .expect("retry is explicit");
                    assert_eq!(restart.next, *retry_state);
                    assert_eq!(restart.effects, *retry_effects);

                    for diagnostic_cancellation in [false, true] {
                        let mut diagnostic_outcome = fork_for_test(&model);
                        let diagnostic = diagnostic_outcome.pending_acknowledgements()[0];
                        let result = if diagnostic_cancellation {
                            diagnostic_outcome.apply(Event::EffectCancelled {
                                instance_id: diagnostic.instance_id,
                                effect: diagnostic.effect,
                                generation: diagnostic.generation,
                            })
                        } else {
                            diagnostic_outcome.apply(Event::EffectFailed {
                                instance_id: diagnostic.instance_id,
                                effect: diagnostic.effect,
                                generation: diagnostic.generation,
                            })
                        };
                        assert!(result.is_ok(), "{label} diagnostic outcome remains typed");
                        assert_eq!(diagnostic_outcome.state(), *recovery);
                        assert!(diagnostic_outcome.pending_acknowledgements().is_empty());
                        let restart = diagnostic_outcome
                            .apply(Event::Restart)
                            .expect("failed diagnostic remains recoverable");
                        assert_eq!(restart.next, *retry_state);
                        assert_eq!(restart.effects, *retry_effects);
                    }
                }
            }
        }
    }

    #[test]
    fn ffmpeg_partial_unsuccessful_outcomes_wait_for_every_original_receipt() {
        type Builder = fn() -> StateMachine;
        let cases: &[(&str, Builder, State, State, &[EffectIntent])] = &[
            (
                "cancellation-cleanup",
                || {
                    let mut model = started_ffmpeg(48);
                    assert!(model.apply(Event::Cancel).is_ok());
                    model
                },
                State::FfmpegCancellationRecovering,
                State::FfmpegCancelling,
                &[EffectIntent::TerminateProcess, EffectIntent::ReapProcess],
            ),
            (
                "failure-cleanup",
                || {
                    let mut model = started_ffmpeg(48);
                    assert!(model.apply(Event::Fail).is_ok());
                    model
                },
                State::FfmpegFailureRecovering,
                State::FfmpegFailing,
                &[
                    EffectIntent::TerminateProcess,
                    EffectIntent::ReapProcess,
                    EffectIntent::PreserveDiagnostics,
                ],
            ),
        ];

        for (label, build, recovery, waiting, expected_reissue) in cases {
            for cancelled in [false, true] {
                let original_count = build().pending_acknowledgements().len();
                for unsuccessful_index in 0..original_count {
                    let mut model = build();
                    let original = model.pending_acknowledgements().to_vec();
                    for acknowledgement in &original[..unsuccessful_index] {
                        assert!(
                            model
                                .apply(Event::EffectAcknowledged {
                                    instance_id: acknowledgement.instance_id,
                                    effect: acknowledgement.effect,
                                    generation: acknowledgement.generation,
                                })
                                .is_ok()
                        );
                    }

                    let unsuccessful = original[unsuccessful_index];
                    let result = if cancelled {
                        model.apply(Event::EffectCancelled {
                            instance_id: unsuccessful.instance_id,
                            effect: unsuccessful.effect,
                            generation: unsuccessful.generation,
                        })
                    } else {
                        model.apply(Event::EffectFailed {
                            instance_id: unsuccessful.instance_id,
                            effect: unsuccessful.effect,
                            generation: unsuccessful.generation,
                        })
                    };
                    assert!(result.is_ok(), "{label} outcome must be correlated");

                    let remaining = original_count - unsuccessful_index - 1;
                    if remaining > 0 {
                        assert_eq!(model.state(), *waiting, "{label} must retain cleanup state");
                        assert_eq!(model.pending_acknowledgements().len(), remaining);
                        assert!(matches!(
                            model.apply(Event::Restart),
                            Err(TransitionError::InvalidTransition { .. })
                        ));
                        acknowledge_all(&mut model);
                    }

                    assert_eq!(
                        model.state(),
                        *recovery,
                        "{label} recovers only after every outcome"
                    );
                    assert_eq!(
                        model.pending_acknowledgements()[0].effect,
                        EffectIntent::PreserveDiagnostics
                    );
                    acknowledge_all(&mut model);
                    let retry = model.apply(Event::Restart).expect("explicit exact retry");
                    assert_eq!(retry.next, *waiting);
                    assert_eq!(retry.effects, *expected_reissue);
                }
            }
        }
    }

    fn assert_ffmpeg_cancellation_waits_for_out_of_order_unsuccessful_receipts() {
        let mut cancelling = started_ffmpeg(48);
        assert!(cancelling.apply(Event::Cancel).is_ok());
        let cancellation_original = cancelling.pending_acknowledgements().to_vec();
        let cancellation_effects = cancellation_original
            .iter()
            .map(|acknowledgement| acknowledgement.effect)
            .collect::<Vec<_>>();
        let cancellation_generation = cancellation_original[0].generation;
        let terminate = cancellation_original
            .iter()
            .find(|acknowledgement| acknowledgement.effect == EffectIntent::TerminateProcess)
            .copied()
            .expect("termination must be pending");
        let reap = cancellation_original
            .iter()
            .find(|acknowledgement| acknowledgement.effect == EffectIntent::ReapProcess)
            .copied()
            .expect("reap must be pending");

        assert!(cancelling.apply(unsuccessful_outcome(false, reap)).is_ok());
        assert_eq!(cancelling.state(), State::FfmpegCancelling);
        assert_eq!(cancelling.pending_acknowledgements(), &[terminate]);
        let before_stale_reap = snapshot(&cancelling);
        assert!(matches!(
            cancelling.apply(unsuccessful_outcome(false, reap)),
            Err(TransitionError::UnexpectedEffectFailure { .. })
        ));
        assert_eq!(snapshot(&cancelling), before_stale_reap);
        assert!(matches!(
            cancelling.apply(Event::Restart),
            Err(TransitionError::InvalidTransition { .. })
        ));

        assert!(
            cancelling
                .apply(unsuccessful_outcome(true, terminate))
                .is_ok()
        );
        assert_eq!(cancelling.state(), State::FfmpegCancellationRecovering);
        assert_eq!(
            cancelling.pending_acknowledgements()[0].effect,
            EffectIntent::PreserveDiagnostics
        );
        acknowledge_all(&mut cancelling);
        let cancellation_retry = cancelling
            .apply(Event::Restart)
            .expect("recovery must reissue the exact cancellation cleanup set");
        assert_eq!(cancellation_retry.next, State::FfmpegCancelling);
        assert_eq!(cancellation_retry.effects, cancellation_effects);
        assert!(
            cancelling
                .pending_acknowledgements()
                .iter()
                .all(|acknowledgement| acknowledgement.generation != cancellation_generation)
        );
    }

    fn assert_ffmpeg_failure_waits_for_out_of_order_unsuccessful_receipts() {
        let mut failing = started_ffmpeg(64);
        assert!(failing.apply(Event::Fail).is_ok());
        let failure_original = failing.pending_acknowledgements().to_vec();
        let failure_effects = failure_original
            .iter()
            .map(|acknowledgement| acknowledgement.effect)
            .collect::<Vec<_>>();
        let failure_generation = failure_original[0].generation;
        let terminate = failure_original
            .iter()
            .find(|acknowledgement| acknowledgement.effect == EffectIntent::TerminateProcess)
            .copied()
            .expect("termination must be pending");
        let reap = failure_original
            .iter()
            .find(|acknowledgement| acknowledgement.effect == EffectIntent::ReapProcess)
            .copied()
            .expect("reap must be pending");
        let preserve = failure_original
            .iter()
            .find(|acknowledgement| acknowledgement.effect == EffectIntent::PreserveDiagnostics)
            .copied()
            .expect("diagnostic preservation must be pending");

        assert!(failing.apply(unsuccessful_outcome(true, reap)).is_ok());
        assert_eq!(failing.state(), State::FfmpegFailing);
        assert_eq!(failing.pending_acknowledgements(), &[terminate, preserve]);
        assert!(failing.apply(unsuccessful_outcome(false, preserve)).is_ok());
        assert_eq!(failing.state(), State::FfmpegFailing);
        assert_eq!(failing.pending_acknowledgements(), &[terminate]);
        let before_stale_reap = snapshot(&failing);
        assert!(matches!(
            failing.apply(unsuccessful_outcome(true, reap)),
            Err(TransitionError::UnexpectedEffectCancellation { .. })
        ));
        assert_eq!(snapshot(&failing), before_stale_reap);
        assert!(matches!(
            failing.apply(Event::Restart),
            Err(TransitionError::InvalidTransition { .. })
        ));

        assert!(
            failing
                .apply(Event::EffectAcknowledged {
                    instance_id: terminate.instance_id,
                    effect: terminate.effect,
                    generation: terminate.generation,
                })
                .is_ok()
        );
        assert_eq!(failing.state(), State::FfmpegFailureRecovering);
        assert_eq!(
            failing.pending_acknowledgements()[0].effect,
            EffectIntent::PreserveDiagnostics
        );
        acknowledge_all(&mut failing);
        let failure_retry = failing
            .apply(Event::Restart)
            .expect("recovery must reissue the exact failure cleanup set");
        assert_eq!(failure_retry.next, State::FfmpegFailing);
        assert_eq!(failure_retry.effects, failure_effects);
        assert!(
            failing
                .pending_acknowledgements()
                .iter()
                .all(|acknowledgement| acknowledgement.generation != failure_generation)
        );
    }

    #[test]
    fn ffmpeg_out_of_order_mixed_unsuccessful_outcomes_recover_after_all_original_receipts() {
        assert_ffmpeg_cancellation_waits_for_out_of_order_unsuccessful_receipts();
        assert_ffmpeg_failure_waits_for_out_of_order_unsuccessful_receipts();
    }

    #[test]
    fn recovery_retries_reject_prior_generation_effect_outcomes() {
        for (event, recovery, retry_state) in [
            (
                Event::Cancel,
                State::FfmpegCancellationRecovering,
                State::FfmpegCancelling,
            ),
            (
                Event::Fail,
                State::FfmpegFailureRecovering,
                State::FfmpegFailing,
            ),
        ] {
            let mut ffmpeg = started_ffmpeg(64);
            assert!(ffmpeg.apply(event).is_ok());
            let stale = ffmpeg.pending_acknowledgements()[0];
            assert!(ffmpeg.apply(unsuccessful_outcome(false, stale)).is_ok());
            complete_pending_effects(&mut ffmpeg);
            assert_eq!(ffmpeg.state(), recovery);
            assert!(ffmpeg.apply(Event::Restart).is_ok());
            assert_eq!(ffmpeg.state(), retry_state);

            let before_stale_receipts = snapshot(&ffmpeg);
            assert!(matches!(
                ffmpeg.apply(Event::EffectAcknowledged {
                    instance_id: stale.instance_id,
                    effect: stale.effect,
                    generation: stale.generation,
                }),
                Err(TransitionError::UnexpectedAcknowledgement { .. })
            ));
            assert!(matches!(
                ffmpeg.apply(unsuccessful_outcome(false, stale)),
                Err(TransitionError::UnexpectedEffectFailure { .. })
            ));
            assert!(matches!(
                ffmpeg.apply(unsuccessful_outcome(true, stale)),
                Err(TransitionError::UnexpectedEffectCancellation { .. })
            ));
            assert_eq!(snapshot(&ffmpeg), before_stale_receipts);
        }

        for cancellation in [false, true] {
            let mut filesystem = probed_filesystem(16);
            assert!(filesystem.apply(Event::Confine).is_ok());
            let stale = filesystem.pending_acknowledgements()[0];
            assert!(
                filesystem
                    .apply(unsuccessful_outcome(cancellation, stale))
                    .is_ok()
            );
            assert!(filesystem.apply(Event::Restart).is_ok());
            assert_eq!(filesystem.state(), State::FilesystemProbed);
            assert!(filesystem.apply(Event::Confine).is_ok());

            let before_stale_receipts = snapshot(&filesystem);
            assert!(matches!(
                filesystem.apply(Event::EffectAcknowledged {
                    instance_id: stale.instance_id,
                    effect: stale.effect,
                    generation: stale.generation,
                }),
                Err(TransitionError::UnexpectedAcknowledgement { .. })
            ));
            assert!(matches!(
                filesystem.apply(unsuccessful_outcome(false, stale)),
                Err(TransitionError::UnexpectedEffectFailure { .. })
            ));
            assert!(matches!(
                filesystem.apply(unsuccessful_outcome(true, stale)),
                Err(TransitionError::UnexpectedEffectCancellation { .. })
            ));
            assert_eq!(snapshot(&filesystem), before_stale_receipts);
        }
    }

    #[test]
    fn illegal_transitions_are_typed_and_do_not_mutate_or_trace() {
        let mut model = machine(MachineKind::CommitArchiveReconciliation, 2);
        assert!(matches!(
            model.apply(Event::Archive),
            Err(TransitionError::InvalidTransition { .. })
        ));
        assert_eq!(model.state(), State::CommitWorking);
        assert!(model.trace().is_empty());
        assert!(matches!(
            StateMachine::from_state(MachineKind::Watcher, State::JobQueued, instance(1), 1, 1,),
            Err(TransitionError::StateDoesNotBelongToMachine { .. })
        ));
    }

    #[test]
    fn trace_bound_is_checked_before_transition() {
        let mut model = machine(MachineKind::SourceRedirect, 1);
        assert!(model.apply(Event::Start).is_ok());
        assert!(matches!(
            model.apply(Event::Complete),
            Err(TransitionError::TraceLimitReached { limit: 1 })
        ));
        assert_eq!(model.state(), State::EffectPending);
    }

    #[test]
    fn replay_detects_counterfactual_trace_mutation() {
        let model = run(
            MachineKind::SourceRedirect,
            &[
                Event::Start,
                Event::Acknowledge,
                Event::Redirect,
                Event::Acknowledge,
                Event::Continue,
                Event::Acknowledge,
                Event::Complete,
            ],
            State::SourceResolved,
        );
        let mut mutated = model.trace().to_vec();
        if let Some(record) = mutated.get_mut(1) {
            record.next = State::BytesDurable;
        }
        assert!(matches!(
            StateMachine::replay(
                MachineKind::SourceRedirect,
                instance(1),
                model.trace().len(),
                &mutated,
            ),
            Err(TransitionError::ReplayMismatch { index: 1 })
        ));
    }

    #[test]
    fn degraded_filesystem_never_claims_confinement() {
        let degraded = run(
            MachineKind::FilesystemCapability,
            &[
                Event::Probe,
                Event::Acknowledge,
                Event::Degrade,
                Event::Acknowledge,
            ],
            State::FilesystemDegraded,
        );
        assert!(degraded.trace().iter().any(|record| {
            record
                .effects
                .contains(&EffectIntent::ReportDegradedGuarantees)
        }));
        let model = StateMachine::from_state(
            MachineKind::FilesystemCapability,
            State::FilesystemDegraded,
            instance(1),
            1,
            1,
        );
        let Ok(mut model) = model else {
            return assert!(matches!(
                model,
                Err(TransitionError::StateDoesNotBelongToMachine { .. })
            ));
        };
        assert!(matches!(
            model.apply(Event::Confine),
            Err(TransitionError::InvalidTransition { .. })
        ));
    }

    #[test]
    fn transient_restore_and_stale_or_wrong_acknowledgements_are_rejected() {
        assert!(matches!(
            StateMachine::from_state(
                MachineKind::CommitArchiveReconciliation,
                State::CommitRenaming,
                instance(1),
                4,
                40,
            ),
            Err(TransitionError::StateIsNotDurable { .. })
        ));

        let mut model = machine(MachineKind::CommitArchiveReconciliation, 8);
        assert!(model.apply(Event::Prepare).is_ok());
        let stale = model.pending_acknowledgements()[0];
        complete_pending_effects(&mut model);
        assert!(model.apply(Event::Cancel).is_ok());
        let cancellation = model.pending_acknowledgements()[0];
        assert_ne!(stale.generation, cancellation.generation);
        assert!(matches!(
            model.apply(Event::EffectAcknowledged {
                instance_id: stale.instance_id,
                effect: stale.effect,
                generation: stale.generation,
            }),
            Err(TransitionError::UnexpectedAcknowledgement { .. })
        ));
        assert!(matches!(
            model.apply(Event::EffectAcknowledged {
                instance_id: cancellation.instance_id,
                effect: EffectIntent::RenameOutput,
                generation: cancellation.generation,
            }),
            Err(TransitionError::UnexpectedAcknowledgement { .. })
        ));
        assert!(matches!(
            model.apply(Event::Acknowledge),
            Err(TransitionError::InvalidTransition { .. })
        ));
        assert_eq!(model.state(), State::EffectPending);
        assert!(model.pending_acknowledgements().contains(&cancellation));
    }

    #[test]
    fn acknowledgement_rejects_cross_instance_routing() {
        let mut first =
            StateMachine::new(MachineKind::CommitArchiveReconciliation, instance(41), 4);
        let mut second =
            StateMachine::new(MachineKind::CommitArchiveReconciliation, instance(42), 4);
        assert!(first.apply(Event::Prepare).is_ok());
        assert!(second.apply(Event::Prepare).is_ok());
        let first_ack = first.pending_acknowledgements()[0];
        let second_ack = second.pending_acknowledgements()[0];
        assert_eq!(first_ack.effect, second_ack.effect);
        assert_eq!(first_ack.generation, second_ack.generation);
        assert_ne!(first_ack.instance_id, second_ack.instance_id);

        let prior = snapshot(&second);
        assert!(matches!(
            second.apply(Event::EffectAcknowledged {
                instance_id: first_ack.instance_id,
                effect: first_ack.effect,
                generation: first_ack.generation,
            }),
            Err(TransitionError::UnexpectedAcknowledgement { .. })
        ));
        assert_eq!(snapshot(&second), prior);
    }

    #[test]
    fn restoration_accepts_only_inventory_enumerated_durable_states() {
        for state in [
            State::CommitPrepared,
            State::CommitRenamed,
            State::CommitArchived,
            State::CommitCleaned,
            State::CommitReconciled,
        ] {
            assert!(
                StateMachine::from_state(
                    MachineKind::CommitArchiveReconciliation,
                    state,
                    instance(7),
                    1,
                    2,
                )
                .is_ok()
            );
        }
        for state in [
            State::CommitWorking,
            State::CommitPreparing,
            State::CommitDataSynchronizing,
            State::CommitPreparedDirectorySynchronizing,
            State::CommitPreparedRecordAppending,
            State::CommitPreparedJournalSynchronizing,
            State::CommitRenaming,
            State::CommitRenameDirectorySynchronizing,
            State::CommitRenamedRecordAppending,
            State::CommitRenamedJournalSynchronizing,
            State::CommitArchiving,
            State::CommitArchivedRecordAppending,
            State::CommitArchivedJournalSynchronizing,
            State::CommitCleaning,
            State::CommitCleanedRecordAppending,
            State::CommitCleanedJournalSynchronizing,
            State::CommitInconsistent,
            State::CommitCancelled,
            State::EffectPending,
            State::EffectRecovery,
        ] {
            assert!(matches!(
                StateMachine::from_state(
                    MachineKind::CommitArchiveReconciliation,
                    state,
                    instance(7),
                    1,
                    2,
                ),
                Err(TransitionError::StateIsNotDurable { .. })
            ));
        }
        for (kind, state) in [
            (MachineKind::AtomicAdmission, State::AdmissionReleased),
            (MachineKind::JavascriptWorker, State::JavascriptCompleted),
            (MachineKind::PluginIpc, State::PluginStopped),
        ] {
            assert!(matches!(
                StateMachine::from_state(kind, state, instance(7), 1, 2),
                Err(TransitionError::StateIsNotDurable { .. })
            ));
        }
    }
}
