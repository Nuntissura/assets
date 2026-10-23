//! Single-owner orchestration of validation, resources, process effects, and output proof.

#[cfg(not(target_os = "linux"))]
use crate::platform::spawn_verified;
#[cfg(target_os = "linux")]
use crate::platform::{InheritedFdBinding, create_governed_output, spawn_verified_with_bindings};
use crate::{
    bounded_io::{
        BoundedDrainLimits, BoundedDrainResult, ProgressDrainError, ProgressRuntimeLimits,
        drain_diagnostics, drain_progress_supervised, drain_required_bytes,
    },
    identity::{
        ExecutableCapabilityObservation, ExecutableObservation, IdentityError,
        MAXIMUM_EXECUTABLE_BYTES, observe_executable_bounded,
    },
    platform::{
        ExecutablePinExpectation, ExitObservation, ForceTerminationOutcome, GovernedPathPin,
        PlatformChild, PlatformError, append_windows_system_environment,
        append_windows_writable_environment, configure_cancellable_pipe, pin_governed_directory,
        pin_governed_file,
    },
    progress::{ProgressLimits, ProgressSummary},
};
#[cfg(target_os = "linux")]
use fforager_contracts::FFMPEG_LINUX_FD_OUTPUT_V1;
use fforager_contracts::{
    ByteCreditStage, FFMPEG_LINUX_FD_INPUT_BASE_V1, FFMPEG_SUPERVISION_REPORT_SCHEMA_ID,
    FFMPEG_SUPERVISION_VERSION, FfmpegContainmentEvidenceV1, FfmpegContractError,
    FfmpegDirectChildReapV1, FfmpegEnvironmentBindingV1, FfmpegHostOperatingSystemV1,
    FfmpegInputDemuxerV1, FfmpegLifecycleObservationV1, FfmpegLifecycleStateV1,
    FfmpegOperationPlanV1, FfmpegOutputFactsV1, FfmpegOutputStreamFactV1, FfmpegOutputValidationV1,
    FfmpegStreamKindV1, FfmpegSupervisionReportV1, FfmpegSupervisionRequestV1,
    FfmpegTerminalOutcomeV1, FfmpegValidatedInvocationV1, WindowsActiveProcessQueryV1,
};
#[cfg(test)]
use fforager_core::lifecycle::FfmpegLifecycleActionV2;
use fforager_core::lifecycle::{
    FfmpegEffectOutcomeV2, FfmpegEffectV2, FfmpegLifecycleLimitsV2, FfmpegLifecycleStateV2,
    FfmpegLifecycleV2, FfmpegReapOutcomeV2, FfmpegTerminationV2, MachineInstanceId,
};
use fforager_core::resource::{
    OwnedAdmission, OwnedByteCreditBroker, OwnedByteCreditLease, OwnedResourceBroker,
    OwnedResourceLease, OwnerId,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// Cancellation timing for one execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancellationProfile {
    Never,
    After(Duration),
}

/// Retained, no-follow directory identity supplied through trusted supervisor
/// configuration rather than the caller-authored request.
#[derive(Debug)]
pub struct TrustedDirectoryPin {
    canonical_path: PathBuf,
    pin: GovernedPathPin,
}

impl TrustedDirectoryPin {
    /// Pin one trusted directory by exact canonical filesystem identity.
    ///
    /// # Errors
    ///
    /// Rejects missing, non-directory, symlink/reparse, non-canonicalizable, or
    /// platform-unpinnable paths.
    pub fn acquire(path: &Path) -> Result<Self, SupervisorError> {
        if !path.is_absolute() {
            return Err(SupervisorError::Filesystem(
                "trusted directory is not absolute",
            ));
        }
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|_| SupervisorError::Filesystem("trusted directory missing"))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(SupervisorError::Filesystem(
                "trusted directory is not a direct directory",
            ));
        }
        let canonical_path = path
            .canonicalize()
            .map_err(|_| SupervisorError::Filesystem("canonicalize trusted directory"))?;
        let pin = pin_governed_directory(&canonical_path)?;
        Ok(Self {
            canonical_path,
            pin,
        })
    }

    /// Canonical absolute path bound by this trusted pin.
    #[must_use]
    pub fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    fn validate_exact(&self, requested: &Path) -> Result<(), SupervisorError> {
        let canonical_requested = requested
            .canonicalize()
            .map_err(|_| SupervisorError::Filesystem("canonicalize requested directory"))?;
        if canonical_requested != self.canonical_path {
            return Err(SupervisorError::Filesystem(
                "request working directory does not match trusted root",
            ));
        }
        self.pin.verify_path(&self.canonical_path)?;
        Ok(())
    }

    fn verify_retained(&self) -> Result<(), SupervisorError> {
        self.pin.verify_path(&self.canonical_path)?;
        Ok(())
    }
}

/// Trusted values which cannot be authored by an `FFmpeg` request.
#[derive(Debug)]
pub struct SupervisorTrustedContext<'a> {
    pub source_commit: &'a str,
    pub host_system_root: Option<&'a str>,
    pub working_directory: &'a TrustedDirectoryPin,
    pub job_temporary_directory: &'a TrustedDirectoryPin,
    pub resource_broker: &'a OwnedResourceBroker,
    pub byte_credit_broker: &'a OwnedByteCreditBroker,
    pub ffmpeg_capability: &'a ExecutableCapabilityObservation,
    pub ffprobe_capability: &'a ExecutableCapabilityObservation,
}

/// Successful execution evidence. Leases have already been released only after
/// this complete result was constructed and contract-validated.
#[derive(Debug)]
pub struct FfmpegExecutionEvidence {
    pub report: FfmpegSupervisionReportV1,
    pub progress: ProgressSummary,
    pub diagnostics: BoundedDrainResult,
}

/// Bounded supervisor failure.
#[derive(Debug)]
pub enum SupervisorError {
    Contract(FfmpegContractError),
    Identity(IdentityError),
    ExecutableIdentityChanged(&'static str),
    Filesystem(&'static str),
    Resource(String),
    Lifecycle(String),
    Platform(PlatformError),
    Progress(ProgressDrainError),
    Diagnostic(std::io::Error),
    DiagnosticLimitExceeded,
    StartupTimedOut,
    ExecutionTimedOut,
    Cancelled {
        forced: bool,
    },
    GracefulStopFailed,
    ForcedTerminationFailed,
    ReapUnproven,
    NonzeroExit(ExitObservation),
    ContainmentUnproven,
    DrainJoinFailed,
    CleanupFailed {
        primary: String,
        cleanup: String,
    },
    FfprobeFailed,
    ValidationTimedOut,
    FfprobeInvalid,
    ExistingOutputFfprobeRejected {
        stage: &'static str,
        reason: &'static str,
    },
}

impl std::fmt::Display for SupervisorError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "FFmpeg supervision failed: {self:?}")
    }
}

impl std::error::Error for SupervisorError {}

impl From<FfmpegContractError> for SupervisorError {
    fn from(value: FfmpegContractError) -> Self {
        Self::Contract(value)
    }
}

impl From<IdentityError> for SupervisorError {
    fn from(value: IdentityError) -> Self {
        Self::Identity(value)
    }
}

impl From<PlatformError> for SupervisorError {
    fn from(value: PlatformError) -> Self {
        Self::Platform(value)
    }
}

/// Focused process adapter. It is intentionally single-thread owned; only pipe
/// readers move to worker threads.
#[derive(Debug, Default)]
pub struct FfmpegSupervisor;

impl FfmpegSupervisor {
    /// Validate a request/token pair against the exact trusted working-root
    /// path and retained directory identity before resource admission.
    ///
    /// # Errors
    ///
    /// Rejects stale/foreign validation tokens, unsafe job paths, and any
    /// caller-selected working directory other than the trusted pinned root.
    pub fn validate_trusted_working_directory(
        &self,
        request: &FfmpegSupervisionRequestV1,
        invocation: &FfmpegValidatedInvocationV1,
        trusted_working_directory: &TrustedDirectoryPin,
    ) -> Result<PathBuf, SupervisorError> {
        invocation.validate_against(request)?;
        let working_directory = validate_paths(request)?;
        trusted_working_directory.validate_exact(&working_directory)?;
        Ok(working_directory)
    }

    /// Execute one validated file-output request and prove direct wait,
    /// declared containment, progress, diagnostics, and ffprobe output facts.
    ///
    /// # Errors
    ///
    /// Every preflight, admission, process, pipe, deadline, wait, containment,
    /// or output failure is returned as a typed bounded error.
    #[allow(
        clippy::too_many_lines,
        reason = "the single-owner supervisor keeps the security-critical spawn, drain, reap, validation, and release order explicit"
    )]
    pub fn execute(
        &self,
        request: &FfmpegSupervisionRequestV1,
        invocation: &FfmpegValidatedInvocationV1,
        trusted: &SupervisorTrustedContext<'_>,
        cancellation: CancellationProfile,
    ) -> Result<FfmpegExecutionEvidence, SupervisorError> {
        let start = Instant::now();
        let cancellation_deadline = cancellation_deadline(start, cancellation);
        let startup_deadline = phase_deadline(start, request.limits.startup_timeout_millis);
        let startup_deadlines = PhaseDeadlines {
            phase_deadline: startup_deadline,
            cancellation_deadline,
            phase: DeadlinePhase::Startup,
        };
        startup_deadlines.ensure()?;
        let working_directory = self.validate_trusted_working_directory(
            request,
            invocation,
            trusted.working_directory,
        )?;
        validate_commit(trusted.source_commit)?;
        validate_trusted_temporary_directory(trusted, &working_directory)?;
        let ffmpeg = verify_identity(&request.toolchain.ffmpeg, "ffmpeg", startup_deadlines)?;
        verify_identity(&request.toolchain.ffprobe, "ffprobe", startup_deadlines)?;
        let environment = synthesize_environment(request, trusted, &working_directory)?;
        verify_capability_binding(&request.toolchain.ffmpeg, trusted.ffmpeg_capability)?;
        verify_capability_binding(&request.toolchain.ffprobe, trusted.ffprobe_capability)?;
        startup_deadlines.ensure()?;
        let owner = owner_id(request.job_id.as_str());

        let (resource_lease, pipe_lease) = acquire_adapter_capacity(
            trusted.resource_broker,
            trusted.byte_credit_broker,
            owner,
            request,
        )?;
        let mut resource_lease = Some(resource_lease);
        let mut pipe_lease = Some(pipe_lease);
        let mut lifecycle = Vec::with_capacity(8);
        let instance_id = MachineInstanceId::new(owner.0).ok_or_else(|| {
            SupervisorError::Resource("zero lifecycle instance identity".to_owned())
        })?;
        let lifecycle_limits = FfmpegLifecycleLimitsV2::new(2, 2, 2, 2)
            .ok_or_else(|| SupervisorError::Lifecycle("invalid lifecycle limits".to_owned()))?;
        let mut lifecycle_model = FfmpegLifecycleV2::new(instance_id, lifecycle_limits, 64);
        if latch_cancellation(cancellation_deadline, &mut lifecycle_model)? {
            return Err(settle_terminal_error(
                &mut lifecycle_model,
                &mut pipe_lease,
                &mut resource_lease,
                cancellation_deadline,
                false,
                SupervisorError::Cancelled { forced: false },
                || {},
            ));
        }
        lifecycle_model.start().map_err(lifecycle_error)?;
        let media_pins = match GovernedMediaPins::capture(
            request,
            &working_directory,
            Some(startup_deadlines),
        ) {
            Ok(pins) => pins,
            Err(error) => {
                return Err(settle_pre_spawn_error(
                    &mut lifecycle_model,
                    &mut pipe_lease,
                    &mut resource_lease,
                    cancellation_deadline,
                    error,
                ));
            }
        };
        if latch_cancellation(cancellation_deadline, &mut lifecycle_model)? {
            return Err(settle_pre_spawn_error(
                &mut lifecycle_model,
                &mut pipe_lease,
                &mut resource_lease,
                cancellation_deadline,
                SupervisorError::Cancelled { forced: false },
            ));
        }

        // Recheck the executable and every governed path after admission at the
        // immediate spawn boundary. Platform code retains its own exact OS
        // identities through process creation where the operating system allows it.
        let spawn_preflight = (|| {
            startup_deadlines.ensure()?;
            let ffmpeg_at_spawn =
                verify_identity(&request.toolchain.ffmpeg, "ffmpeg", startup_deadlines)?;
            if ffmpeg_at_spawn != ffmpeg {
                return Err(SupervisorError::ExecutableIdentityChanged("ffmpeg"));
            }
            let working_directory_at_spawn = validate_paths(request)?;
            if working_directory_at_spawn != working_directory {
                return Err(SupervisorError::Filesystem(
                    "job paths changed before spawn",
                ));
            }
            trusted
                .working_directory
                .validate_exact(&working_directory_at_spawn)?;
            validate_trusted_temporary_directory(trusted, &working_directory_at_spawn)?;
            media_pins.verify_all()?;
            startup_deadlines.ensure()?;
            Ok(())
        })();
        if let Err(error) = spawn_preflight {
            return Err(settle_pre_spawn_error(
                &mut lifecycle_model,
                &mut pipe_lease,
                &mut resource_lease,
                cancellation_deadline,
                error,
            ));
        }
        if latch_cancellation(cancellation_deadline, &mut lifecycle_model)? {
            return Err(settle_pre_spawn_error(
                &mut lifecycle_model,
                &mut pipe_lease,
                &mut resource_lease,
                cancellation_deadline,
                SupervisorError::Cancelled { forced: false },
            ));
        }
        let spawn_hash_timeout = startup_remaining_or_settle(
            startup_deadlines,
            &mut lifecycle_model,
            &mut pipe_lease,
            &mut resource_lease,
        )?;
        let (mut child, mut precreated_output_pin) = match run_before_startup_deadline(
            startup_deadlines,
            || {
                spawn_bound_ffmpeg(
                    Path::new(ffmpeg.canonical_path()),
                    invocation.bound_runtime().arguments(),
                    &environment,
                    &working_directory,
                    ExecutablePinExpectation {
                        file_identity: ffmpeg.file_identity(),
                        content_sha256: ffmpeg.content_sha256(),
                        maximum_bytes: MAXIMUM_EXECUTABLE_BYTES,
                        hash_timeout: spawn_hash_timeout,
                    },
                    &media_pins,
                    trusted,
                )
            },
            |spawned| {
                spawned
                    .0
                    .cleanup_force_reap(Duration::from_millis(
                        request.limits.forced_kill_timeout_millis,
                    ))
                    .map(|_| ())
                    .map_err(SupervisorError::Platform)
            },
        ) {
            Ok(child) => child,
            Err(error) => {
                return Err(settle_pre_spawn_error(
                    &mut lifecycle_model,
                    &mut pipe_lease,
                    &mut resource_lease,
                    cancellation_deadline,
                    error,
                ));
            }
        };
        complete_lifecycle(&mut lifecycle_model, FfmpegEffectOutcomeV2::Spawned)?;
        if let Err(error) = trusted
            .working_directory
            .validate_exact(&working_directory)
            .and_then(|()| validate_trusted_temporary_directory(trusted, &working_directory))
        {
            return Err(abort_spawned_execution(
                &mut child,
                &mut lifecycle_model,
                &mut pipe_lease,
                &mut resource_lease,
                Duration::from_millis(request.limits.forced_kill_timeout_millis),
                None,
                error,
            ));
        }
        push_state(&mut lifecycle, FfmpegLifecycleStateV1::Spawned, start);
        push_state(&mut lifecycle, FfmpegLifecycleStateV1::Running, start);
        if let Err(error) = media_pins.verify_all() {
            return Err(abort_spawned_execution(
                &mut child,
                &mut lifecycle_model,
                &mut pipe_lease,
                &mut resource_lease,
                Duration::from_millis(request.limits.forced_kill_timeout_millis),
                None,
                error,
            ));
        }
        let (stdout, stderr) = match child.take_pipes() {
            Ok(pipes) => pipes,
            Err(error) => {
                return Err(abort_spawned_execution(
                    &mut child,
                    &mut lifecycle_model,
                    &mut pipe_lease,
                    &mut resource_lease,
                    Duration::from_millis(request.limits.forced_kill_timeout_millis),
                    None,
                    SupervisorError::Platform(error),
                ));
            }
        };
        if let Err(error) =
            configure_cancellable_pipe(&stdout).and_then(|()| configure_cancellable_pipe(&stderr))
        {
            return Err(abort_spawned_execution(
                &mut child,
                &mut lifecycle_model,
                &mut pipe_lease,
                &mut resource_lease,
                Duration::from_millis(request.limits.forced_kill_timeout_millis),
                None,
                SupervisorError::Platform(error),
            ));
        }
        let pipe_stop = Arc::new(AtomicBool::new(false));
        let progress_limits = ProgressLimits {
            max_records: request.limits.progress_max_records,
            max_total_bytes: request.limits.progress_max_total_bytes,
            max_record_bytes: request.limits.progress_max_record_bytes,
            max_field_bytes: request.limits.progress_max_field_bytes,
            max_parser_steps: request.limits.progress_max_parser_steps,
        };
        let progress_runtime_limits = ProgressRuntimeLimits {
            allocation_bytes: usize::try_from(request.resources.progress_pipe_bytes)
                .unwrap_or(usize::MAX),
            cadence_timeout: Duration::from_millis(request.limits.progress_max_silence_millis),
            consumer_stall_timeout: Duration::from_millis(
                request.limits.consumer_stall_timeout_millis,
            ),
        };
        let progress_stop = Arc::clone(&pipe_stop);
        let progress_thread = std::thread::spawn(move || {
            drain_progress_supervised(
                CancellablePipeReader::new(stdout, progress_stop),
                progress_limits,
                progress_runtime_limits,
            )
        });
        let diagnostic_limits = BoundedDrainLimits {
            max_total_bytes: request.limits.stderr_max_total_bytes,
            // Retained diagnostic bytes are charged only to the stderr share
            // of the acquired FfmpegPipe byte-credit lease. Any smaller
            // resource share is observable as explicit prefix truncation.
            tail_bytes: usize::try_from(
                request
                    .limits
                    .stderr_tail_bytes
                    .min(request.resources.stderr_pipe_bytes),
            )
            .unwrap_or(usize::MAX),
        };
        let diagnostic_stop = Arc::clone(&pipe_stop);
        let diagnostic_thread = std::thread::spawn(move || {
            drain_diagnostics(
                CancellablePipeReader::new(stderr, diagnostic_stop),
                diagnostic_limits,
            )
        });
        let workers = PipeWorkers {
            progress: progress_thread,
            diagnostics: diagnostic_thread,
            stop: pipe_stop,
        };

        let wait_result = wait_or_cancel(
            &mut child,
            request,
            cancellation,
            cancellation_deadline,
            &mut lifecycle,
            &mut lifecycle_model,
            start,
        );
        let (exit, cancelled, forced) = match wait_result {
            Ok(result) => result,
            Err(error) => {
                return Err(abort_spawned_execution(
                    &mut child,
                    &mut lifecycle_model,
                    &mut pipe_lease,
                    &mut resource_lease,
                    Duration::from_millis(request.limits.forced_kill_timeout_millis),
                    Some(workers),
                    error,
                ));
            }
        };
        push_state(&mut lifecycle, FfmpegLifecycleStateV1::Reaped, start);
        let scope_result = wait_for_scope_empty(
            &child,
            Duration::from_millis(request.limits.reap_timeout_millis),
        );
        let scope_empty = match scope_result {
            Ok(empty) => empty,
            Err(error) => {
                return Err(abort_spawned_execution(
                    &mut child,
                    &mut lifecycle_model,
                    &mut pipe_lease,
                    &mut resource_lease,
                    Duration::from_millis(request.limits.forced_kill_timeout_millis),
                    Some(workers),
                    error,
                ));
            }
        };
        if !scope_empty {
            // A direct child may exit while descendants remain. Terminating the
            // owned Job/group here does not retry or kill the already-reaped child.
            if let Err(error) = child.force_terminate() {
                return Err(abort_spawned_execution(
                    &mut child,
                    &mut lifecycle_model,
                    &mut pipe_lease,
                    &mut resource_lease,
                    Duration::from_millis(request.limits.forced_kill_timeout_millis),
                    Some(workers),
                    SupervisorError::Platform(error),
                ));
            }
            match wait_for_scope_empty(
                &child,
                Duration::from_millis(request.limits.forced_kill_timeout_millis),
            ) {
                Ok(true) => {}
                Ok(false) => {
                    return Err(abort_spawned_execution(
                        &mut child,
                        &mut lifecycle_model,
                        &mut pipe_lease,
                        &mut resource_lease,
                        Duration::from_millis(request.limits.forced_kill_timeout_millis),
                        Some(workers),
                        SupervisorError::ContainmentUnproven,
                    ));
                }
                Err(error) => {
                    return Err(abort_spawned_execution(
                        &mut child,
                        &mut lifecycle_model,
                        &mut pipe_lease,
                        &mut resource_lease,
                        Duration::from_millis(request.limits.forced_kill_timeout_millis),
                        Some(workers),
                        error,
                    ));
                }
            }
        }
        if let Err(error) = media_pins.verify_all() {
            return Err(abort_spawned_execution(
                &mut child,
                &mut lifecycle_model,
                &mut pipe_lease,
                &mut resource_lease,
                Duration::from_millis(request.limits.forced_kill_timeout_millis),
                Some(workers),
                error,
            ));
        }
        let mut cancelled =
            cancelled || latch_cancellation(cancellation_deadline, &mut lifecycle_model)?;
        workers.stop.store(true, Ordering::Release);
        let progress_join = workers.progress.join();
        let diagnostics_join = workers.diagnostics.join();
        let Ok(progress_result) = progress_join else {
            return Err(finish_after_terminal_cleanup(
                &mut lifecycle_model,
                &mut pipe_lease,
                &mut resource_lease,
                diagnostics_join.is_ok(),
                SupervisorError::DrainJoinFailed,
            ));
        };
        let Ok(diagnostics_result) = diagnostics_join else {
            return Err(finish_after_terminal_cleanup(
                &mut lifecycle_model,
                &mut pipe_lease,
                &mut resource_lease,
                false,
                SupervisorError::DrainJoinFailed,
            ));
        };
        let diagnostics = match diagnostics_result {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                poison_adapter_leases(&mut pipe_lease, &mut resource_lease);
                return Err(SupervisorError::CleanupFailed {
                    primary: SupervisorError::ContainmentUnproven.to_string(),
                    cleanup: error.to_string(),
                });
            }
            Err(error) => {
                return Err(finish_after_terminal_cleanup(
                    &mut lifecycle_model,
                    &mut pipe_lease,
                    &mut resource_lease,
                    false,
                    SupervisorError::Diagnostic(error),
                ));
            }
        };
        let progress = match progress_result {
            Ok(progress) => progress,
            Err(ProgressDrainError::Io(error)) if error.kind() == std::io::ErrorKind::TimedOut => {
                poison_adapter_leases(&mut pipe_lease, &mut resource_lease);
                return Err(SupervisorError::CleanupFailed {
                    primary: SupervisorError::ContainmentUnproven.to_string(),
                    cleanup: error.to_string(),
                });
            }
            Err(error) => {
                return Err(finish_after_terminal_cleanup(
                    &mut lifecycle_model,
                    &mut pipe_lease,
                    &mut resource_lease,
                    true,
                    SupervisorError::Progress(error),
                ));
            }
        };
        cancelled |= latch_cancellation(cancellation_deadline, &mut lifecycle_model)?;
        if diagnostics.total_limit_exceeded {
            return Err(finish_after_terminal_cleanup(
                &mut lifecycle_model,
                &mut pipe_lease,
                &mut resource_lease,
                false,
                SupervisorError::DiagnosticLimitExceeded,
            ));
        }
        complete_lifecycle(
            &mut lifecycle_model,
            FfmpegEffectOutcomeV2::DiagnosticsPreserved,
        )?;
        if cancelled {
            return Err(settle_terminal_error(
                &mut lifecycle_model,
                &mut pipe_lease,
                &mut resource_lease,
                cancellation_deadline,
                forced,
                SupervisorError::Cancelled { forced },
                || {},
            ));
        }
        if !exit.successful() {
            return Err(settle_terminal_error(
                &mut lifecycle_model,
                &mut pipe_lease,
                &mut resource_lease,
                cancellation_deadline,
                forced,
                SupervisorError::NonzeroExit(exit),
                || {},
            ));
        }

        let validation_started = Instant::now();
        let validation_deadlines = PhaseDeadlines {
            phase_deadline: phase_deadline(
                validation_started,
                request.limits.ffprobe_timeout_millis,
            ),
            cancellation_deadline,
            phase: DeadlinePhase::Validation,
        };
        let output_pin = match finalize_output_pin(&media_pins, &mut precreated_output_pin) {
            Ok(pin) => pin,
            Err(error) => {
                return Err(settle_validation_failure(
                    &mut lifecycle_model,
                    &mut pipe_lease,
                    &mut resource_lease,
                    cancellation_deadline,
                    forced,
                    error,
                    || {},
                ));
            }
        };
        if latch_cancellation(cancellation_deadline, &mut lifecycle_model)? {
            complete_lifecycle(
                &mut lifecycle_model,
                FfmpegEffectOutcomeV2::OutputValidationFailed,
            )?;
            return Err(settle_terminal_error(
                &mut lifecycle_model,
                &mut pipe_lease,
                &mut resource_lease,
                cancellation_deadline,
                forced,
                SupervisorError::Cancelled { forced },
                || {},
            ));
        }
        if let Err(error) = validation_deadlines.ensure() {
            return Err(settle_validation_failure(
                &mut lifecycle_model,
                &mut pipe_lease,
                &mut resource_lease,
                cancellation_deadline,
                forced,
                error,
                || {},
            ));
        }
        let facts = match validate_output_with_ffprobe(
            request,
            &environment,
            &working_directory,
            &media_pins,
            &output_pin,
            validation_deadlines,
            Some((trusted.working_directory, trusted.job_temporary_directory)),
        ) {
            Ok(facts) => {
                complete_lifecycle(&mut lifecycle_model, FfmpegEffectOutcomeV2::OutputValidated)?;
                facts
            }
            Err(error) => {
                return Err(settle_validation_failure(
                    &mut lifecycle_model,
                    &mut pipe_lease,
                    &mut resource_lease,
                    cancellation_deadline,
                    forced,
                    error,
                    || {},
                ));
            }
        };
        push_state(&mut lifecycle, FfmpegLifecycleStateV1::Validated, start);
        let direct_wait_receipt = hash_bytes(
            format!(
                "exit={:?};signal={:?};windows_status_opaque={:?};forced={}",
                exit.exit_code, exit.signal, exit.windows_status_opaque, exit.forced_by_supervisor
            )
            .as_bytes(),
        );
        let containment = match request.toolchain.ffmpeg.host.operating_system {
            FfmpegHostOperatingSystemV1::Windows => FfmpegContainmentEvidenceV1::WindowsJob {
                attached_before_execution: child.attached_before_execution(),
                kill_on_job_close: child.kill_on_close(),
                active_process_query: WindowsActiveProcessQueryV1::Zero {},
            },
            FfmpegHostOperatingSystemV1::Linux => FfmpegContainmentEvidenceV1::UnixProcessGroup {
                declared_nonescaping_members_remaining: 0,
                setsid_escape_capable: true,
            },
        };
        let mut residual_uncertainty = match request.toolchain.ffmpeg.host.operating_system {
            FfmpegHostOperatingSystemV1::Windows => vec![
                "pre-existing writable handles remain outside the trusted-media-root threat model"
                    .to_owned(),
                "windows exception provenance is unproven by numeric exit code".to_owned(),
                "windows suspended-orphan interval remains without a broker".to_owned(),
            ],
            FfmpegHostOperatingSystemV1::Linux => vec![
                "linux descriptor handoff pins exact media objects, but concurrent mutation through a pre-existing writable descriptor remains outside the trusted-root threat model"
                    .to_owned(),
                "unix setsid escape proves process groups are signal scopes, not containment"
                    .to_owned(),
            ],
        };
        residual_uncertainty.sort();
        let report = FfmpegSupervisionReportV1 {
            schema_id: FFMPEG_SUPERVISION_REPORT_SCHEMA_ID.to_owned(),
            version: FFMPEG_SUPERVISION_VERSION,
            request_id: request.request_id.clone(),
            job_id: request.job_id.clone(),
            source_commit: trusted.source_commit.to_owned(),
            request_contract_sha256: invocation.request_contract_sha256().to_owned(),
            operation_plan_sha256: invocation.operation_plan_sha256().to_owned(),
            ffmpeg_content_sha256: request.toolchain.ffmpeg.content_sha256.clone(),
            ffprobe_content_sha256: request.toolchain.ffprobe.content_sha256.clone(),
            host: request.toolchain.ffmpeg.host,
            lifecycle,
            terminal_outcome: FfmpegTerminalOutcomeV1::Validated {},
            direct_child: FfmpegDirectChildReapV1::Reaped {
                exit_code: exit.exit_code,
                terminated_by_signal_or_exception: false,
                wait_receipt_sha256: direct_wait_receipt,
            },
            containment,
            output_validation: FfmpegOutputValidationV1::Validated {
                ffprobe_content_sha256: request.toolchain.ffprobe.content_sha256.clone(),
                facts,
            },
            residual_uncertainty,
        };
        if let Err(error) = report.validate_against(request) {
            return Err(settle_terminal_error(
                &mut lifecycle_model,
                &mut pipe_lease,
                &mut resource_lease,
                cancellation_deadline,
                forced,
                SupervisorError::Contract(error),
                || {},
            ));
        }
        let cancellation_won_release = release_terminal_leases(
            &mut pipe_lease,
            &mut resource_lease,
            &mut lifecycle_model,
            cancellation_deadline,
            || {},
        )?;
        if cancellation_won_release {
            return Err(SupervisorError::Cancelled { forced });
        }
        Ok(FfmpegExecutionEvidence {
            report,
            progress,
            diagnostics,
        })
    }
}

fn verify_identity(
    expected: &fforager_contracts::FfmpegExecutableIdentityV1,
    label: &'static str,
    deadlines: PhaseDeadlines,
) -> Result<ExecutableObservation, SupervisorError> {
    let observed = match observe_executable_bounded(
        Path::new(&expected.absolute_path),
        MAXIMUM_EXECUTABLE_BYTES,
        deadlines.remaining()?,
    ) {
        Ok(observed) => observed,
        Err(IdentityError::HashDeadlineExceeded) => {
            deadlines.ensure()?;
            return Err(deadlines.phase.error());
        }
        Err(error) => return Err(error.into()),
    };
    deadlines.ensure()?;
    if observed.file_identity() != expected.file_identity
        || observed.content_sha256() != expected.content_sha256
    {
        return Err(SupervisorError::ExecutableIdentityChanged(label));
    }
    Ok(observed)
}

fn verify_capability_binding(
    expected: &fforager_contracts::FfmpegExecutableIdentityV1,
    observed: &ExecutableCapabilityObservation,
) -> Result<(), SupervisorError> {
    if observed.executable().file_identity() != expected.file_identity
        || observed.executable().content_sha256() != expected.content_sha256
        || observed.host() != expected.host
        || observed.version_output_sha256() != expected.version_output_sha256
        || observed.normalized_version() != expected.normalized_version
        || observed.normalized_probe_sha256() != expected.capability_binding.normalized_probe_sha256
        || observed.capabilities() != expected.capability_binding.capabilities
    {
        return Err(SupervisorError::ExecutableIdentityChanged(
            "version or capability binding",
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct GovernedMediaPins {
    inputs: Vec<(PathBuf, GovernedPathPin)>,
    output_parent: (PathBuf, GovernedPathPin),
    output: PathBuf,
    maximum_file_bytes: u64,
    hash_timeout: Duration,
}

impl GovernedMediaPins {
    fn capture(
        request: &FfmpegSupervisionRequestV1,
        working_directory: &Path,
        startup_deadlines: Option<PhaseDeadlines>,
    ) -> Result<Self, SupervisorError> {
        let FfmpegOperationPlanV1::StreamCopy(plan) = &request.operation;
        let hash_timeout = Duration::from_millis(request.limits.ffprobe_timeout_millis);
        let mut inputs = Vec::with_capacity(plan.inputs.len());
        for input in &plan.inputs {
            let input_hash_timeout = startup_deadlines
                .map(PhaseDeadlines::remaining)
                .transpose()?
                .map_or(hash_timeout, |remaining| remaining.min(hash_timeout));
            let path = working_directory.join(&input.path);
            let pin = pin_governed_file(
                &path,
                request.limits.output_max_file_size_bytes,
                input_hash_timeout,
            )?;
            require_valid_media_pin(&pin, true)?;
            inputs.push((path, pin));
        }
        if let Some(deadlines) = startup_deadlines {
            deadlines.ensure()?;
        }
        let output = working_directory.join(&plan.output_path);
        let parent = output
            .parent()
            .ok_or(SupervisorError::Filesystem("output parent"))?
            .to_path_buf();
        let parent_pin = pin_governed_directory(&parent)?;
        require_valid_media_pin(&parent_pin, false)?;
        Ok(Self {
            inputs,
            output_parent: (parent, parent_pin),
            output,
            maximum_file_bytes: request.limits.output_max_file_size_bytes,
            hash_timeout,
        })
    }

    fn verify_all(&self) -> Result<(), SupervisorError> {
        for (path, pin) in &self.inputs {
            pin.verify_path(path)?;
        }
        self.output_parent.1.verify_path(&self.output_parent.0)?;
        Ok(())
    }

    fn pin_output(&self) -> Result<GovernedPathPin, SupervisorError> {
        self.verify_all()?;
        let pin = pin_governed_file(&self.output, self.maximum_file_bytes, self.hash_timeout)?;
        require_valid_media_pin(&pin, true)?;
        pin.verify_path(&self.output)?;
        Ok(pin)
    }
}

#[cfg(target_os = "linux")]
fn spawn_bound_ffmpeg(
    executable: &Path,
    arguments: &[String],
    environment: &[(String, String)],
    _working_directory: &Path,
    expectation: ExecutablePinExpectation<'_>,
    media_pins: &GovernedMediaPins,
    trusted: &SupervisorTrustedContext<'_>,
) -> Result<(PlatformChild, Option<GovernedPathPin>), PlatformError> {
    let output_pin = create_governed_output(
        &media_pins.output_parent.1,
        &media_pins.output,
        media_pins.maximum_file_bytes,
        media_pins.hash_timeout,
    )?;
    let mut bindings = Vec::with_capacity(media_pins.inputs.len() + 1);
    for (index, (_, pin)) in media_pins.inputs.iter().enumerate() {
        let target = i32::from(FFMPEG_LINUX_FD_INPUT_BASE_V1)
            + i32::try_from(index).map_err(|_| {
                PlatformError::state("bind FFmpeg input descriptor", "input index overflow")
            })?;
        bindings.push(InheritedFdBinding::new(pin, target)?);
    }
    bindings.push(InheritedFdBinding::new(
        &output_pin,
        i32::from(FFMPEG_LINUX_FD_OUTPUT_V1),
    )?);
    let child = spawn_verified_with_bindings(
        executable,
        arguments,
        environment,
        &trusted.working_directory.pin,
        &trusted.job_temporary_directory.pin,
        expectation,
        &bindings,
    )?;
    Ok((child, Some(output_pin)))
}

#[cfg(not(target_os = "linux"))]
fn spawn_bound_ffmpeg(
    executable: &Path,
    arguments: &[String],
    environment: &[(String, String)],
    working_directory: &Path,
    expectation: ExecutablePinExpectation<'_>,
    _media_pins: &GovernedMediaPins,
    _trusted: &SupervisorTrustedContext<'_>,
) -> Result<(PlatformChild, Option<GovernedPathPin>), PlatformError> {
    spawn_verified(
        executable,
        arguments,
        environment,
        working_directory,
        expectation,
    )
    .map(|child| (child, None))
}

#[cfg(target_os = "linux")]
fn finalize_output_pin(
    media_pins: &GovernedMediaPins,
    precreated: &mut Option<GovernedPathPin>,
) -> Result<GovernedPathPin, SupervisorError> {
    media_pins.verify_all()?;
    let mut pin = precreated.take().ok_or(SupervisorError::Filesystem(
        "missing precreated Linux output pin",
    ))?;
    pin.finalize_written_content()?;
    require_valid_media_pin(&pin, true)?;
    pin.verify_path(&media_pins.output)?;
    Ok(pin)
}

#[cfg(not(target_os = "linux"))]
fn finalize_output_pin(
    media_pins: &GovernedMediaPins,
    _precreated: &mut Option<GovernedPathPin>,
) -> Result<GovernedPathPin, SupervisorError> {
    media_pins.pin_output()
}

fn require_valid_media_pin(
    pin: &GovernedPathPin,
    content_required: bool,
) -> Result<(), SupervisorError> {
    if pin.file_identity().is_empty()
        || (content_required
            && !pin.content_sha256().is_some_and(|digest| {
                digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            }))
    {
        return Err(SupervisorError::Filesystem("governed media pin identity"));
    }
    Ok(())
}

fn validate_paths(request: &FfmpegSupervisionRequestV1) -> Result<PathBuf, SupervisorError> {
    let root = std::fs::canonicalize(&request.working_directory)
        .map_err(|_| SupervisorError::Filesystem("working directory"))?;
    let FfmpegOperationPlanV1::StreamCopy(plan) = &request.operation;
    for input in &plan.inputs {
        let joined = root.join(&input.path);
        let metadata = std::fs::symlink_metadata(&joined)
            .map_err(|_| SupervisorError::Filesystem("input metadata"))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(SupervisorError::Filesystem("input symlink or non-file"));
        }
        let canonical = std::fs::canonicalize(&joined)
            .map_err(|_| SupervisorError::Filesystem("input canonicalization"))?;
        if !canonical.starts_with(&root) {
            return Err(SupervisorError::Filesystem("input escaped job root"));
        }
    }
    let output = root.join(&plan.output_path);
    if output.exists() {
        return Err(SupervisorError::Filesystem("output already exists"));
    }
    let parent = output
        .parent()
        .ok_or(SupervisorError::Filesystem("output parent"))?;
    let canonical_parent = std::fs::canonicalize(parent)
        .map_err(|_| SupervisorError::Filesystem("output parent canonicalization"))?;
    if !canonical_parent.starts_with(&root) {
        return Err(SupervisorError::Filesystem("output escaped job root"));
    }
    Ok(root)
}

fn synthesize_environment(
    request: &FfmpegSupervisionRequestV1,
    trusted: &SupervisorTrustedContext<'_>,
    working_directory: &Path,
) -> Result<Vec<(String, String)>, SupervisorError> {
    let mut values = Vec::new();
    for binding in &request.environment.trusted_bindings {
        match binding {
            FfmpegEnvironmentBindingV1::HostSystemRoot => {
                let system_root = trusted
                    .host_system_root
                    .ok_or(SupervisorError::Filesystem("trusted SYSTEMROOT missing"))?;
                append_windows_system_environment(&mut values, system_root)
                    .map_err(SupervisorError::Filesystem)?;
            }
            FfmpegEnvironmentBindingV1::JobTemporaryDirectory => {
                let temporary = validate_job_temporary_directory(
                    trusted.job_temporary_directory,
                    working_directory,
                )?;
                if request.toolchain.ffmpeg.host.operating_system
                    == FfmpegHostOperatingSystemV1::Windows
                {
                    append_windows_writable_environment(&mut values, &temporary);
                } else {
                    values.push(("TMPDIR".to_owned(), "/proc/self/fd/98".to_owned()));
                }
            }
            FfmpegEnvironmentBindingV1::LocaleC => {
                values.push(("LANG".to_owned(), "C".to_owned()));
                values.push(("LC_ALL".to_owned(), "C".to_owned()));
            }
        }
    }
    Ok(values)
}

fn validate_job_temporary_directory(
    temporary: &TrustedDirectoryPin,
    working_directory: &Path,
) -> Result<String, SupervisorError> {
    temporary.verify_retained()?;
    let canonical_temporary = temporary.canonical_path();
    let canonical_working = working_directory
        .canonicalize()
        .map_err(|_| SupervisorError::Filesystem("canonicalize working directory"))?;
    if canonical_temporary == canonical_working
        || !canonical_temporary.starts_with(canonical_working)
    {
        return Err(SupervisorError::Filesystem(
            "job temporary directory escapes working directory",
        ));
    }
    canonical_temporary
        .to_str()
        .ok_or(SupervisorError::Filesystem("temporary path encoding"))
        .map(ToOwned::to_owned)
}

fn validate_trusted_temporary_directory(
    trusted: &SupervisorTrustedContext<'_>,
    working_directory: &Path,
) -> Result<(), SupervisorError> {
    validate_job_temporary_directory(trusted.job_temporary_directory, working_directory).map(drop)
}

fn owner_id(job_id: &str) -> OwnerId {
    let digest = Sha256::digest(job_id.as_bytes());
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    OwnerId(u64::from_be_bytes(bytes))
}

fn acquire_adapter_capacity(
    resource_broker: &OwnedResourceBroker,
    byte_credit_broker: &OwnedByteCreditBroker,
    owner: OwnerId,
    request: &FfmpegSupervisionRequestV1,
) -> Result<(OwnedResourceLease, OwnedByteCreditLease), SupervisorError> {
    let bytes = request
        .resources
        .progress_pipe_bytes
        .checked_add(request.resources.stderr_pipe_bytes)
        .ok_or_else(|| SupervisorError::Resource("pipe byte overflow".to_owned()))?;
    acquire_exact_adapter_capacity(
        resource_broker,
        byte_credit_broker,
        owner,
        request.resources.claim,
        bytes,
    )
}

fn acquire_exact_adapter_capacity(
    resource_broker: &OwnedResourceBroker,
    byte_credit_broker: &OwnedByteCreditBroker,
    owner: OwnerId,
    claim: fforager_contracts::ResourceVector,
    pipe_bytes: u64,
) -> Result<(OwnedResourceLease, OwnedByteCreditLease), SupervisorError> {
    let resource_lease = match resource_broker.request(owner, claim) {
        Ok(OwnedAdmission::Granted(lease)) => Ok(lease),
        Ok(OwnedAdmission::Queued(_waiter)) => Err(SupervisorError::Resource(
            "compound resource vector backpressured".to_owned(),
        )),
        Err(error) => Err(SupervisorError::Resource(format!("{error:?}"))),
    }?;
    match byte_credit_broker.claim(owner, ByteCreditStage::FfmpegPipe, pipe_bytes) {
        Ok(pipe_lease) => Ok((resource_lease, pipe_lease)),
        Err(error) => {
            resource_lease
                .release()
                .map_err(|release| SupervisorError::CleanupFailed {
                    primary: format!("pipe admission failed: {error:?}"),
                    cleanup: format!("resource admission rollback failed: {release:?}"),
                })?;
            Err(SupervisorError::Resource(format!("{error:?}")))
        }
    }
}

fn wait_or_cancel(
    child: &mut PlatformChild,
    request: &FfmpegSupervisionRequestV1,
    cancellation: CancellationProfile,
    cancellation_deadline: Option<Instant>,
    lifecycle: &mut Vec<FfmpegLifecycleObservationV1>,
    lifecycle_model: &mut FfmpegLifecycleV2,
    start: Instant,
) -> Result<(ExitObservation, bool, bool), SupervisorError> {
    let execution_deadline =
        phase_deadline(Instant::now(), request.limits.execution_timeout_millis);
    let (stop_deadline, stop_cause) =
        execution_stop_deadline(execution_deadline, cancellation_deadline, cancellation);
    let wait = stop_deadline.saturating_duration_since(Instant::now());
    let observed_exit = child.wait_timeout(wait)?;
    let deadline_reached = Instant::now() >= stop_deadline;
    if let Some(exit) = observed_exit {
        record_reap(lifecycle_model, &exit)?;
        if !deadline_reached {
            return Ok((exit, false, false));
        }
        return if stop_cause == ExecutionStopCause::Cancellation {
            if !lifecycle_model.cancellation_requested() {
                lifecycle_model
                    .request_cancellation()
                    .map_err(lifecycle_error)?;
            }
            Ok((exit, true, false))
        } else {
            Err(SupervisorError::ExecutionTimedOut)
        };
    }
    let cancellation_requested = stop_cause == ExecutionStopCause::Cancellation;
    if !lifecycle_model.cancellation_requested() {
        lifecycle_model
            .request_cancellation()
            .map_err(lifecycle_error)?;
    }
    push_state(
        lifecycle,
        FfmpegLifecycleStateV1::GracefulStopRequested,
        start,
    );
    let graceful_supported = child.request_graceful_stop()?;
    complete_lifecycle(
        lifecycle_model,
        if graceful_supported {
            FfmpegEffectOutcomeV2::GracefulStopRequested
        } else {
            FfmpegEffectOutcomeV2::GracefulStopUnsupported
        },
    )?;
    if graceful_supported
        && let Some(exit) = child.wait_timeout(Duration::from_millis(
            request.limits.graceful_stop_timeout_millis,
        ))?
    {
        record_reap(lifecycle_model, &exit)?;
        return if cancellation_requested {
            Ok((exit, true, false))
        } else {
            Err(SupervisorError::ExecutionTimedOut)
        };
    }
    if graceful_supported {
        lifecycle_model
            .expire_grace_period()
            .map_err(lifecycle_error)?;
    }
    push_state(
        lifecycle,
        FfmpegLifecycleStateV1::ForcedKillRequested,
        start,
    );
    let forced = match child.force_terminate() {
        Ok(outcome) => complete_force_termination(lifecycle_model, outcome)?,
        Err(error) => {
            complete_lifecycle(lifecycle_model, FfmpegEffectOutcomeV2::ForcedKillFailed)?;
            return Err(error.into());
        }
    };
    let exit = child
        .wait_timeout(Duration::from_millis(request.limits.reap_timeout_millis))?
        .ok_or(SupervisorError::ReapUnproven)?;
    complete_reap(lifecycle_model, &exit)?;
    if !cancellation_requested {
        return Err(SupervisorError::ExecutionTimedOut);
    }
    Ok((exit, true, forced))
}

fn complete_force_termination(
    lifecycle_model: &mut FfmpegLifecycleV2,
    outcome: ForceTerminationOutcome,
) -> Result<bool, SupervisorError> {
    let (lifecycle_outcome, forced) = match outcome {
        ForceTerminationOutcome::Requested => (FfmpegEffectOutcomeV2::ForcedKillRequested, true),
        ForceTerminationOutcome::AlreadyEmpty => {
            (FfmpegEffectOutcomeV2::ForcedKillAlreadyExited, false)
        }
    };
    complete_lifecycle(lifecycle_model, lifecycle_outcome)?;
    Ok(forced)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExecutionStopCause {
    Cancellation,
    ExecutionTimeout,
}

fn execution_stop_deadline(
    execution_deadline: Instant,
    cancellation_deadline: Option<Instant>,
    cancellation: CancellationProfile,
) -> (Instant, ExecutionStopCause) {
    match (cancellation, cancellation_deadline) {
        (CancellationProfile::After(_), Some(deadline)) if deadline <= execution_deadline => {
            // An exact tie belongs to explicit operator cancellation.
            (deadline, ExecutionStopCause::Cancellation)
        }
        _ => (execution_deadline, ExecutionStopCause::ExecutionTimeout),
    }
}

fn cancellation_deadline(start: Instant, cancellation: CancellationProfile) -> Option<Instant> {
    match cancellation {
        CancellationProfile::Never => None,
        CancellationProfile::After(duration) => Some(start.checked_add(duration).unwrap_or(start)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeadlinePhase {
    Startup,
    Validation,
}

impl DeadlinePhase {
    fn error(self) -> SupervisorError {
        match self {
            Self::Startup => SupervisorError::StartupTimedOut,
            Self::Validation => SupervisorError::ValidationTimedOut,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct PhaseDeadlines {
    phase_deadline: Instant,
    cancellation_deadline: Option<Instant>,
    phase: DeadlinePhase,
}

impl PhaseDeadlines {
    fn effective(self) -> Instant {
        self.cancellation_deadline
            .map_or(self.phase_deadline, |deadline| {
                deadline.min(self.phase_deadline)
            })
    }

    fn ensure(self) -> Result<(), SupervisorError> {
        let now = Instant::now();
        if self
            .cancellation_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            Err(SupervisorError::Cancelled { forced: false })
        } else if now >= self.phase_deadline {
            Err(self.phase.error())
        } else {
            Ok(())
        }
    }

    fn remaining(self) -> Result<Duration, SupervisorError> {
        self.ensure()?;
        Ok(self.effective().saturating_duration_since(Instant::now()))
    }
}

fn phase_deadline(start: Instant, milliseconds: u64) -> Instant {
    start
        .checked_add(Duration::from_millis(milliseconds))
        .unwrap_or(start)
}

fn run_before_startup_deadline<T>(
    deadlines: PhaseDeadlines,
    spawn: impl FnOnce() -> Result<T, PlatformError>,
    cleanup: impl FnOnce(&mut T) -> Result<(), SupervisorError>,
) -> Result<T, SupervisorError> {
    deadlines.ensure()?;
    let mut spawned = spawn().map_err(SupervisorError::Platform)?;
    if let Err(deadline_error) = deadlines.ensure() {
        return match cleanup(&mut spawned) {
            Ok(()) => Err(deadline_error),
            Err(cleanup_error) => Err(SupervisorError::CleanupFailed {
                primary: deadline_error.to_string(),
                cleanup: cleanup_error.to_string(),
            }),
        };
    }
    Ok(spawned)
}

fn latch_cancellation(
    deadline: Option<Instant>,
    lifecycle: &mut FfmpegLifecycleV2,
) -> Result<bool, SupervisorError> {
    if lifecycle.cancellation_requested() {
        return Ok(true);
    }
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        lifecycle.request_cancellation().map_err(lifecycle_error)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

fn record_reap(
    lifecycle_model: &mut FfmpegLifecycleV2,
    exit: &ExitObservation,
) -> Result<(), SupervisorError> {
    lifecycle_model
        .observe_child_exit()
        .map_err(lifecycle_error)?;
    complete_reap(lifecycle_model, exit)
}

fn complete_reap(
    lifecycle_model: &mut FfmpegLifecycleV2,
    exit: &ExitObservation,
) -> Result<(), SupervisorError> {
    let termination = if let Some(status) = exit.windows_status_opaque {
        FfmpegTerminationV2::WindowsStatusOpaque(status)
    } else if let Some(signal) = exit.signal {
        FfmpegTerminationV2::UnixSignal(signal)
    } else {
        FfmpegTerminationV2::ExitCode(exit.exit_code.unwrap_or(i32::MIN))
    };
    complete_lifecycle(
        lifecycle_model,
        FfmpegEffectOutcomeV2::Reap(FfmpegReapOutcomeV2::Reaped(termination)),
    )
}

fn complete_lifecycle(
    lifecycle_model: &mut FfmpegLifecycleV2,
    outcome: FfmpegEffectOutcomeV2,
) -> Result<(), SupervisorError> {
    let request = lifecycle_model
        .pending_effect()
        .ok_or_else(|| SupervisorError::Lifecycle("missing pending lifecycle effect".to_owned()))?;
    lifecycle_model
        .complete_effect(request, outcome)
        .map_err(lifecycle_error)?;
    Ok(())
}

fn settle_pre_spawn_error(
    lifecycle_model: &mut FfmpegLifecycleV2,
    pipe_lease: &mut Option<OwnedByteCreditLease>,
    resource_lease: &mut Option<OwnedResourceLease>,
    cancellation_deadline: Option<Instant>,
    primary: SupervisorError,
) -> SupervisorError {
    let primary_text = primary.to_string();
    let result: Result<SupervisorError, SupervisorError> = (|| {
        let mut cancelled = latch_cancellation(cancellation_deadline, lifecycle_model)?;
        complete_lifecycle(lifecycle_model, FfmpegEffectOutcomeV2::SpawnFailed)?;
        complete_lifecycle(lifecycle_model, FfmpegEffectOutcomeV2::DiagnosticsPreserved)?;
        cancelled |= release_terminal_leases(
            pipe_lease,
            resource_lease,
            lifecycle_model,
            cancellation_deadline,
            || {},
        )?;
        Ok(if cancelled {
            SupervisorError::Cancelled { forced: false }
        } else {
            primary
        })
    })();
    match result {
        Ok(error) => error,
        Err(cleanup) => {
            poison_adapter_leases(pipe_lease, resource_lease);
            SupervisorError::CleanupFailed {
                primary: primary_text,
                cleanup: cleanup.to_string(),
            }
        }
    }
}

fn startup_remaining_or_settle(
    deadlines: PhaseDeadlines,
    lifecycle_model: &mut FfmpegLifecycleV2,
    pipe_lease: &mut Option<OwnedByteCreditLease>,
    resource_lease: &mut Option<OwnedResourceLease>,
) -> Result<Duration, SupervisorError> {
    match deadlines.remaining() {
        Ok(remaining) => Ok(remaining),
        Err(error) => Err(settle_pre_spawn_error(
            lifecycle_model,
            pipe_lease,
            resource_lease,
            deadlines.cancellation_deadline,
            error,
        )),
    }
}

fn settle_terminal_error(
    lifecycle_model: &mut FfmpegLifecycleV2,
    pipe_lease: &mut Option<OwnedByteCreditLease>,
    resource_lease: &mut Option<OwnedResourceLease>,
    cancellation_deadline: Option<Instant>,
    forced: bool,
    primary: SupervisorError,
    after_physical_release: impl FnOnce(),
) -> SupervisorError {
    let primary_text = primary.to_string();
    match release_terminal_leases(
        pipe_lease,
        resource_lease,
        lifecycle_model,
        cancellation_deadline,
        after_physical_release,
    ) {
        Ok(true) => SupervisorError::Cancelled { forced },
        Ok(false) => primary,
        Err(cleanup) => {
            poison_adapter_leases(pipe_lease, resource_lease);
            SupervisorError::CleanupFailed {
                primary: primary_text,
                cleanup: cleanup.to_string(),
            }
        }
    }
}

fn settle_validation_failure(
    lifecycle_model: &mut FfmpegLifecycleV2,
    pipe_lease: &mut Option<OwnedByteCreditLease>,
    resource_lease: &mut Option<OwnedResourceLease>,
    cancellation_deadline: Option<Instant>,
    forced: bool,
    primary: SupervisorError,
    after_physical_release: impl FnOnce(),
) -> SupervisorError {
    let primary_text = primary.to_string();
    let result: Result<SupervisorError, SupervisorError> = (|| {
        let mut cancelled = latch_cancellation(cancellation_deadline, lifecycle_model)?;
        complete_lifecycle(
            lifecycle_model,
            FfmpegEffectOutcomeV2::OutputValidationFailed,
        )?;
        cancelled |= release_terminal_leases(
            pipe_lease,
            resource_lease,
            lifecycle_model,
            cancellation_deadline,
            after_physical_release,
        )?;
        Ok(if cancelled {
            SupervisorError::Cancelled { forced }
        } else {
            primary
        })
    })();
    match result {
        Ok(error) => error,
        Err(cleanup) => {
            poison_adapter_leases(pipe_lease, resource_lease);
            SupervisorError::CleanupFailed {
                primary: primary_text,
                cleanup: cleanup.to_string(),
            }
        }
    }
}

type ProgressWorker = std::thread::JoinHandle<Result<ProgressSummary, ProgressDrainError>>;
type DiagnosticWorker = std::thread::JoinHandle<Result<BoundedDrainResult, std::io::Error>>;

struct PipeWorkers {
    progress: ProgressWorker,
    diagnostics: DiagnosticWorker,
    stop: Arc<AtomicBool>,
}

struct CancellablePipeReader {
    file: std::fs::File,
    stop: Arc<AtomicBool>,
}

impl CancellablePipeReader {
    fn new(file: std::fs::File, stop: Arc<AtomicBool>) -> Self {
        Self { file, stop }
    }
}

impl Read for CancellablePipeReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        loop {
            match self.file.read(buffer) {
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if self.stop.load(Ordering::Acquire) {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "governed pipe remained open outside the declared process scope",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                result => return result,
            }
        }
    }
}

/// Convert any failure after successful process creation into one ordered,
/// bounded terminal cleanup. The physical process scope is proven empty and
/// the direct child is reaped before either worker is joined and before either
/// admission lease is released.
#[allow(
    clippy::too_many_arguments,
    reason = "all post-spawn ownership is explicit at the one fail-closed cleanup boundary"
)]
fn abort_spawned_execution(
    child: &mut PlatformChild,
    lifecycle_model: &mut FfmpegLifecycleV2,
    pipe_lease: &mut Option<OwnedByteCreditLease>,
    resource_lease: &mut Option<OwnedResourceLease>,
    timeout: Duration,
    workers: Option<PipeWorkers>,
    primary: SupervisorError,
) -> SupervisorError {
    if let Err(cleanup) = request_cleanup_lineage(lifecycle_model)
        .and_then(|()| cleanup_child_and_lifecycle(child, lifecycle_model, timeout))
    {
        // Returning would run the leases' convenience Drop paths even though
        // terminal cleanup was not proven. Permanently consume this adapter's
        // capacity instead; a new job must not be admitted over an unproven OS
        // scope. PlatformChild::drop still performs one final bounded attempt.
        poison_adapter_leases(pipe_lease, resource_lease);
        // Dropping a JoinHandle detaches it. Joining after an unproven process
        // cleanup could block forever on a pipe inherited outside the declared
        // scope, so this failure path must remain bounded.
        if let Some(workers) = workers {
            workers.stop.store(true, Ordering::Release);
            drop(workers.progress);
            drop(workers.diagnostics);
        }
        return SupervisorError::CleanupFailed {
            primary: primary.to_string(),
            cleanup: cleanup.to_string(),
        };
    }

    let diagnostic_worker_present = workers.is_some();
    if let Some(workers) = &workers {
        workers.stop.store(true, Ordering::Release);
    }
    let (progress_join, diagnostic_join) = workers.map_or((None, None), |workers| {
        (
            Some(workers.progress.join()),
            Some(workers.diagnostics.join()),
        )
    });
    let progress_ok = progress_join
        .as_ref()
        .is_none_or(|joined| joined.as_ref().is_ok_and(Result::is_ok));
    let diagnostic_ok = diagnostic_join
        .as_ref()
        .is_none_or(|joined| joined.as_ref().is_ok_and(Result::is_ok));
    let diagnostics_preserved = diagnostic_worker_present && diagnostic_ok;
    let worker_failure = !(progress_ok && diagnostic_ok);
    let escaped_pipe_scope_unproven = progress_join.as_ref().is_some_and(|joined| {
        matches!(
            joined,
            Ok(Err(ProgressDrainError::Io(error)))
                if error.kind() == std::io::ErrorKind::TimedOut
        )
    }) || diagnostic_join.as_ref().is_some_and(|joined| {
        matches!(
            joined,
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::TimedOut
        )
    });
    if escaped_pipe_scope_unproven {
        poison_adapter_leases(pipe_lease, resource_lease);
        return SupervisorError::CleanupFailed {
            primary: primary.to_string(),
            cleanup: "pipe writer escaped the declared process scope; adapter capacity poisoned"
                .to_owned(),
        };
    }
    let original = if worker_failure {
        SupervisorError::CleanupFailed {
            primary: primary.to_string(),
            cleanup: "one or more bounded pipe workers failed while draining after cleanup"
                .to_owned(),
        }
    } else {
        primary
    };
    finish_after_terminal_cleanup(
        lifecycle_model,
        pipe_lease,
        resource_lease,
        diagnostics_preserved,
        original,
    )
}

fn request_cleanup_lineage(lifecycle_model: &mut FfmpegLifecycleV2) -> Result<(), SupervisorError> {
    if !matches!(
        lifecycle_model.state(),
        FfmpegLifecycleStateV2::Exited
            | FfmpegLifecycleStateV2::Failed
            | FfmpegLifecycleStateV2::Cancelled
    ) && !lifecycle_model.cancellation_requested()
    {
        lifecycle_model
            .request_cancellation()
            .map_err(lifecycle_error)?;
    }
    if lifecycle_model.state() == FfmpegLifecycleStateV2::GracefulExitPending {
        lifecycle_model
            .expire_grace_period()
            .map_err(lifecycle_error)?;
    }
    Ok(())
}

fn cleanup_child_and_lifecycle(
    child: &mut PlatformChild,
    lifecycle_model: &mut FfmpegLifecycleV2,
    timeout: Duration,
) -> Result<(), SupervisorError> {
    let exit = child.cleanup_force_reap(timeout)?;
    if !exit.forced_by_supervisor
        && matches!(
            lifecycle_model.state(),
            FfmpegLifecycleStateV2::GracefulStopPending
                | FfmpegLifecycleStateV2::GracefulExitPending
                | FfmpegLifecycleStateV2::ForcedKillPending
        )
    {
        lifecycle_model
            .observe_child_exit()
            .map_err(lifecycle_error)?;
    }
    while let Some(pending) = lifecycle_model.pending_effect() {
        match pending.effect {
            FfmpegEffectV2::RequestGracefulStop => complete_lifecycle(
                lifecycle_model,
                FfmpegEffectOutcomeV2::GracefulStopUnsupported,
            )?,
            FfmpegEffectV2::ForceKillProcessTree => complete_lifecycle(
                lifecycle_model,
                if exit.forced_by_supervisor {
                    FfmpegEffectOutcomeV2::ForcedKillRequested
                } else {
                    FfmpegEffectOutcomeV2::ForcedKillAlreadyExited
                },
            )?,
            FfmpegEffectV2::ReapDirectChild => complete_reap(lifecycle_model, &exit)?,
            FfmpegEffectV2::PreserveDiagnostics
            | FfmpegEffectV2::ValidateOutput
            | FfmpegEffectV2::ReleaseResources => break,
            FfmpegEffectV2::SpawnProcess => {
                return Err(SupervisorError::Lifecycle(
                    "spawn remained pending after successful process creation".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

fn finish_after_terminal_cleanup(
    lifecycle_model: &mut FfmpegLifecycleV2,
    pipe_lease: &mut Option<OwnedByteCreditLease>,
    resource_lease: &mut Option<OwnedResourceLease>,
    diagnostics_preserved: bool,
    primary: SupervisorError,
) -> SupervisorError {
    let result = (|| {
        request_cleanup_lineage(lifecycle_model)?;
        while lifecycle_model
            .pending_effect()
            .is_some_and(|pending| pending.effect == FfmpegEffectV2::PreserveDiagnostics)
        {
            complete_lifecycle(
                lifecycle_model,
                if diagnostics_preserved {
                    FfmpegEffectOutcomeV2::DiagnosticsPreserved
                } else {
                    FfmpegEffectOutcomeV2::DiagnosticsFailed
                },
            )?;
        }
        if lifecycle_model
            .pending_effect()
            .is_some_and(|pending| pending.effect == FfmpegEffectV2::ValidateOutput)
        {
            complete_lifecycle(
                lifecycle_model,
                FfmpegEffectOutcomeV2::OutputValidationFailed,
            )?;
        }
        release_adapter_leases(pipe_lease, resource_lease, lifecycle_model)
    })();
    match result {
        Ok(()) => primary,
        Err(cleanup) => SupervisorError::CleanupFailed {
            primary: primary.to_string(),
            cleanup: cleanup.to_string(),
        },
    }
}

fn poison_adapter_leases(
    pipe_lease: &mut Option<OwnedByteCreditLease>,
    resource_lease: &mut Option<OwnedResourceLease>,
) {
    if let Some(lease) = pipe_lease.take() {
        std::mem::forget(lease);
    }
    if let Some(lease) = resource_lease.take() {
        std::mem::forget(lease);
    }
}

fn release_adapter_leases(
    pipe_lease: &mut Option<OwnedByteCreditLease>,
    resource_lease: &mut Option<OwnedResourceLease>,
    lifecycle_model: &mut FfmpegLifecycleV2,
) -> Result<(), SupervisorError> {
    let pipe_result = pipe_lease
        .take()
        .ok_or_else(|| SupervisorError::Resource("pipe lease already released".to_owned()))?
        .release();
    let resource_result = resource_lease
        .take()
        .ok_or_else(|| SupervisorError::Resource("resource lease already released".to_owned()))?
        .release();
    if let Err(error) = pipe_result {
        complete_lifecycle(
            lifecycle_model,
            FfmpegEffectOutcomeV2::ResourceReleaseFailed,
        )?;
        return Err(SupervisorError::Resource(format!("{error:?}")));
    }
    if let Err(error) = resource_result {
        complete_lifecycle(
            lifecycle_model,
            FfmpegEffectOutcomeV2::ResourceReleaseFailed,
        )?;
        return Err(SupervisorError::Resource(format!("{error:?}")));
    }
    complete_lifecycle(lifecycle_model, FfmpegEffectOutcomeV2::ResourcesReleased)
}

fn release_terminal_leases(
    pipe_lease: &mut Option<OwnedByteCreditLease>,
    resource_lease: &mut Option<OwnedResourceLease>,
    lifecycle_model: &mut FfmpegLifecycleV2,
    cancellation_deadline: Option<Instant>,
    after_physical_release: impl FnOnce(),
) -> Result<bool, SupervisorError> {
    let mut cancellation_won = latch_cancellation(cancellation_deadline, lifecycle_model)?;
    let pipe_result = pipe_lease
        .take()
        .ok_or_else(|| SupervisorError::Resource("pipe lease already released".to_owned()))?
        .release();
    let resource_result = resource_lease
        .take()
        .ok_or_else(|| SupervisorError::Resource("resource lease already released".to_owned()))?
        .release();
    if let Err(error) = pipe_result {
        complete_lifecycle(
            lifecycle_model,
            FfmpegEffectOutcomeV2::ResourceReleaseFailed,
        )?;
        return Err(SupervisorError::Resource(format!("{error:?}")));
    }
    if let Err(error) = resource_result {
        complete_lifecycle(
            lifecycle_model,
            FfmpegEffectOutcomeV2::ResourceReleaseFailed,
        )?;
        return Err(SupervisorError::Resource(format!("{error:?}")));
    }
    after_physical_release();
    cancellation_won |= latch_cancellation(cancellation_deadline, lifecycle_model)?;
    complete_lifecycle(lifecycle_model, FfmpegEffectOutcomeV2::ResourcesReleased)?;
    Ok(cancellation_won)
}

fn lifecycle_error(error: impl std::fmt::Debug) -> SupervisorError {
    SupervisorError::Lifecycle(format!("{error:?}"))
}

fn wait_for_scope_empty(child: &PlatformChild, timeout: Duration) -> Result<bool, SupervisorError> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    loop {
        if child.declared_scope_empty()? {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn push_state(
    lifecycle: &mut Vec<FfmpegLifecycleObservationV1>,
    state: FfmpegLifecycleStateV1,
    start: Instant,
) {
    lifecycle.push(FfmpegLifecycleObservationV1 {
        sequence: u64::try_from(lifecycle.len() + 1).unwrap_or(u64::MAX),
        state,
        monotonic_millis: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
    });
}

#[derive(Debug, Deserialize)]
struct ProbeRoot {
    format: ProbeFormat,
    streams: Vec<ProbeStream>,
    packets: Vec<ProbePacket>,
}

#[derive(Debug, Deserialize)]
struct ProbeFormat {
    format_name: String,
    duration: String,
    size: String,
}

#[derive(Debug, Deserialize)]
struct ProbeStream {
    index: u32,
    codec_type: String,
    codec_name: String,
    width: Option<u32>,
    height: Option<u32>,
    channels: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct ProbePacket {
    stream_index: u32,
    size: String,
    data_hash: String,
    data: String,
}

#[derive(Debug, Deserialize)]
struct PacketProbeRoot {
    packets: Vec<ProbePacket>,
}

fn validate_output_with_ffprobe(
    request: &FfmpegSupervisionRequestV1,
    environment: &[(String, String)],
    working_directory: &Path,
    media_pins: &GovernedMediaPins,
    output_pin: &GovernedPathPin,
    deadlines: PhaseDeadlines,
    trusted_directories: Option<(&TrustedDirectoryPin, &TrustedDirectoryPin)>,
) -> Result<FfmpegOutputFactsV1, SupervisorError> {
    deadlines.ensure()?;
    media_pins.verify_all()?;
    output_pin.verify_path(&media_pins.output)?;
    let ffprobe = verify_identity(&request.toolchain.ffprobe, "ffprobe", deadlines)?;
    let FfmpegOperationPlanV1::StreamCopy(plan) = &request.operation;
    let source_payloads = measure_source_payloads(
        request,
        plan,
        &ffprobe,
        environment,
        working_directory,
        media_pins,
        output_pin,
        deadlines,
        trusted_directories,
    )?;
    let probe = probe_output(
        request,
        plan,
        &ffprobe,
        environment,
        working_directory,
        media_pins,
        output_pin,
        deadlines,
        trusted_directories,
    )?;
    deadlines.ensure()?;
    let facts = normalize_output_facts(request, plan, probe, source_payloads, output_pin)?;
    deadlines.ensure()?;
    media_pins.verify_all()?;
    output_pin.verify_path(&media_pins.output)?;
    Ok(facts)
}

/// Re-run the exact product output validator against an already-created file.
/// This is crate-visible solely so the platform proof producer can execute
/// real negative boundary cases; it does not bypass request or identity checks.
pub(crate) fn validate_existing_output_with_ffprobe(
    request: &FfmpegSupervisionRequestV1,
    environment: &[(String, String)],
    working_directory: &Path,
) -> Result<FfmpegOutputFactsV1, SupervisorError> {
    request.validate_for_invocation()?;
    let canonical_working = validate_existing_output_paths(request)?;
    if canonical_working
        != working_directory.canonicalize().map_err(|_| {
            SupervisorError::Filesystem("existing-output working directory canonicalization")
        })?
    {
        return Err(SupervisorError::Filesystem(
            "existing-output working directory mismatch",
        ));
    }
    let media_pins = GovernedMediaPins::capture(request, &canonical_working, None)?;
    let output_pin = media_pins.pin_output()?;
    validate_output_with_ffprobe(
        request,
        environment,
        &canonical_working,
        &media_pins,
        &output_pin,
        PhaseDeadlines {
            phase_deadline: phase_deadline(Instant::now(), request.limits.ffprobe_timeout_millis),
            cancellation_deadline: None,
            phase: DeadlinePhase::Validation,
        },
        None,
    )
    .map_err(|error| match error {
        SupervisorError::FfprobeInvalid => SupervisorError::ExistingOutputFfprobeRejected {
            stage: "ffprobe-normalization",
            reason: "output facts or packet payload did not match the validated request",
        },
        SupervisorError::FfprobeFailed => SupervisorError::ExistingOutputFfprobeRejected {
            stage: "ffprobe-execution",
            reason: "ffprobe reaped unsuccessfully or exceeded its bounded deadline",
        },
        other => other,
    })
}

fn validate_existing_output_paths(
    request: &FfmpegSupervisionRequestV1,
) -> Result<PathBuf, SupervisorError> {
    let root = std::fs::canonicalize(&request.working_directory)
        .map_err(|_| SupervisorError::Filesystem("working directory"))?;
    let FfmpegOperationPlanV1::StreamCopy(plan) = &request.operation;
    for input in &plan.inputs {
        let path = root.join(&input.path);
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|_| SupervisorError::Filesystem("input metadata"))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(SupervisorError::Filesystem("input symlink or non-file"));
        }
        let canonical = path
            .canonicalize()
            .map_err(|_| SupervisorError::Filesystem("input canonicalization"))?;
        if !canonical.starts_with(&root) {
            return Err(SupervisorError::Filesystem("input escaped job root"));
        }
    }
    let output = root.join(&plan.output_path);
    let metadata = std::fs::symlink_metadata(&output)
        .map_err(|_| SupervisorError::Filesystem("existing output metadata"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(SupervisorError::Filesystem(
            "existing output symlink or non-file",
        ));
    }
    let canonical = output
        .canonicalize()
        .map_err(|_| SupervisorError::Filesystem("existing output canonicalization"))?;
    if !canonical.starts_with(&root) {
        return Err(SupervisorError::Filesystem(
            "existing output escaped job root",
        ));
    }
    Ok(root)
}

#[allow(
    clippy::too_many_arguments,
    reason = "source probing keeps every trusted executable, path pin, deadline, and bounded context explicit"
)]
fn measure_source_payloads(
    request: &FfmpegSupervisionRequestV1,
    plan: &fforager_contracts::FfmpegStreamCopyPlanV1,
    ffprobe: &ExecutableObservation,
    environment: &[(String, String)],
    working_directory: &Path,
    media_pins: &GovernedMediaPins,
    output_pin: &GovernedPathPin,
    deadlines: PhaseDeadlines,
    trusted_directories: Option<(&TrustedDirectoryPin, &TrustedDirectoryPin)>,
) -> Result<Vec<String>, SupervisorError> {
    let mut source_payloads = Vec::with_capacity(plan.stream_maps.len());
    for selected_source in &plan.stream_maps {
        deadlines.ensure()?;
        let input = plan
            .inputs
            .get(usize::from(selected_source.input_index))
            .ok_or(SupervisorError::FfprobeInvalid)?;
        let mut arguments = vec![
            "-v".to_owned(),
            "error".to_owned(),
            "-f".to_owned(),
            demuxer_name(input.demuxer).to_owned(),
            "-select_streams".to_owned(),
            stream_specifier(selected_source.stream_kind, selected_source.stream_index),
            "-show_packets".to_owned(),
            "-show_data".to_owned(),
            "-show_data_hash".to_owned(),
            "sha256".to_owned(),
            "-show_entries".to_owned(),
            "packet=stream_index,size,data_hash,data".to_owned(),
            "-of".to_owned(),
            "json".to_owned(),
        ];
        append_ffprobe_media_argument(
            &mut arguments,
            &input.path,
            request.toolchain.ffprobe.host.operating_system,
        );
        let input_pin = &media_pins
            .inputs
            .get(usize::from(selected_source.input_index))
            .ok_or(SupervisorError::FfprobeInvalid)?
            .1;
        media_pins.verify_all()?;
        output_pin.verify_path(&media_pins.output)?;
        let json = run_bounded_ffprobe(
            Path::new(ffprobe.canonical_path()),
            &arguments,
            request,
            environment,
            working_directory,
            Some(input_pin),
            deadlines,
            trusted_directories,
        )?;
        deadlines.ensure()?;
        media_pins.verify_all()?;
        output_pin.verify_path(&media_pins.output)?;
        let probe: PacketProbeRoot =
            serde_json::from_slice(&json).map_err(|_| SupervisorError::FfprobeInvalid)?;
        deadlines.ensure()?;
        let measured = packet_payload_sha256(
            &probe.packets,
            request.limits.progress_max_records,
            packet_framing(input.demuxer, false),
        )?;
        require_payload_match(&selected_source.source_payload_sha256, &measured)?;
        deadlines.ensure()?;
        source_payloads.push(measured);
    }
    Ok(source_payloads)
}

#[allow(
    clippy::too_many_arguments,
    reason = "output probing keeps every trusted executable, path pin, deadline, and bounded context explicit"
)]
fn probe_output(
    request: &FfmpegSupervisionRequestV1,
    plan: &fforager_contracts::FfmpegStreamCopyPlanV1,
    ffprobe: &ExecutableObservation,
    environment: &[(String, String)],
    working_directory: &Path,
    media_pins: &GovernedMediaPins,
    output_pin: &GovernedPathPin,
    deadlines: PhaseDeadlines,
    trusted_directories: Option<(&TrustedDirectoryPin, &TrustedDirectoryPin)>,
) -> Result<ProbeRoot, SupervisorError> {
    deadlines.ensure()?;
    let mut arguments = vec![
        "-v".to_owned(),
        "error".to_owned(),
        "-show_packets".to_owned(),
        "-show_data".to_owned(),
        "-show_data_hash".to_owned(),
        "sha256".to_owned(),
        "-show_entries".to_owned(),
        "format=format_name,duration,size:stream=index,codec_type,codec_name,width,height,channels:packet=stream_index,size,data_hash,data".to_owned(),
        "-of".to_owned(),
        "json".to_owned(),
    ];
    append_ffprobe_media_argument(
        &mut arguments,
        &plan.output_path,
        request.toolchain.ffprobe.host.operating_system,
    );
    media_pins.verify_all()?;
    output_pin.verify_path(&media_pins.output)?;
    let json = run_bounded_ffprobe(
        Path::new(ffprobe.canonical_path()),
        &arguments,
        request,
        environment,
        working_directory,
        Some(output_pin),
        deadlines,
        trusted_directories,
    )?;
    deadlines.ensure()?;
    media_pins.verify_all()?;
    output_pin.verify_path(&media_pins.output)?;
    let probe = serde_json::from_slice(&json).map_err(|_| SupervisorError::FfprobeInvalid)?;
    deadlines.ensure()?;
    Ok(probe)
}

fn normalize_output_facts(
    request: &FfmpegSupervisionRequestV1,
    plan: &fforager_contracts::FfmpegStreamCopyPlanV1,
    probe: ProbeRoot,
    source_payloads: Vec<String>,
    output_pin: &GovernedPathPin,
) -> Result<FfmpegOutputFactsV1, SupervisorError> {
    let output_identity_sha256 = output_pin
        .content_sha256()
        .ok_or(SupervisorError::FfprobeInvalid)?
        .to_owned();
    let actual_file_size = output_pin.file_size()?;
    let file_size_bytes = probe
        .format
        .size
        .parse::<u64>()
        .map_err(|_| SupervisorError::FfprobeInvalid)?;
    if file_size_bytes != actual_file_size {
        return Err(SupervisorError::FfprobeInvalid);
    }
    let duration_seconds = probe
        .format
        .duration
        .parse::<f64>()
        .map_err(|_| SupervisorError::FfprobeInvalid)?;
    if !duration_seconds.is_finite() || duration_seconds.is_sign_negative() {
        return Err(SupervisorError::FfprobeInvalid);
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "finite nonnegative duration is bounded by contract immediately after conversion"
    )]
    let duration_millis = (duration_seconds * 1000.0).round() as u64;
    let mut format_names: Vec<String> = probe
        .format
        .format_name
        .split(',')
        .map(str::to_owned)
        .collect();
    format_names.sort();
    format_names.dedup();
    let probe_streams = sorted_output_streams(probe.streams, plan.stream_maps.len())?;
    let mut output_packets = vec![Vec::new(); probe_streams.len()];
    if probe.packets.len()
        > usize::try_from(request.limits.progress_max_records).unwrap_or(usize::MAX)
    {
        return Err(SupervisorError::FfprobeInvalid);
    }
    for packet in probe.packets {
        let packets = output_packets
            .get_mut(usize::try_from(packet.stream_index).unwrap_or(usize::MAX))
            .ok_or(SupervisorError::FfprobeInvalid)?;
        packets.push(packet);
    }
    let streams = probe_streams
        .into_iter()
        .zip(&plan.stream_maps)
        .zip(output_packets)
        .zip(source_payloads)
        .map(|(((stream, selected_source), packets), source_payload)| {
            let kind = match stream.codec_type.as_str() {
                "video" => FfmpegStreamKindV1::Video,
                "audio" => FfmpegStreamKindV1::Audio,
                "subtitle" => FfmpegStreamKindV1::Subtitle,
                "data" => FfmpegStreamKindV1::Data,
                "attachment" => FfmpegStreamKindV1::Attachment,
                _ => return Err(SupervisorError::FfprobeInvalid),
            };
            if kind != selected_source.stream_kind {
                return Err(SupervisorError::FfprobeInvalid);
            }
            let input = plan
                .inputs
                .get(usize::from(selected_source.input_index))
                .ok_or(SupervisorError::FfprobeInvalid)?;
            let output_payload_sha256 = packet_payload_sha256(
                &packets,
                request.limits.progress_max_records,
                packet_framing(input.demuxer, true),
            )?;
            require_payload_match(&source_payload, &output_payload_sha256)?;
            Ok(FfmpegOutputStreamFactV1 {
                index: stream.index,
                kind,
                selected_source: selected_source.clone(),
                output_payload_sha256,
                codec_name: stream.codec_name,
                width: stream.width,
                height: stream.height,
                channels: stream.channels,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(FfmpegOutputFactsV1 {
        output_identity_sha256,
        file_size_bytes,
        duration_millis,
        format_names,
        streams,
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "the bounded probe boundary keeps executable, request, media pin, cumulative deadline, and trusted root/temp pins explicit"
)]
fn run_bounded_ffprobe(
    executable: &Path,
    arguments: &[String],
    request: &FfmpegSupervisionRequestV1,
    environment: &[(String, String)],
    working_directory: &Path,
    media_pin: Option<&GovernedPathPin>,
    deadlines: PhaseDeadlines,
    trusted_directories: Option<(&TrustedDirectoryPin, &TrustedDirectoryPin)>,
) -> Result<Vec<u8>, SupervisorError> {
    deadlines.ensure()?;
    let mut child = spawn_bound_ffprobe(
        executable,
        arguments,
        environment,
        working_directory,
        ExecutablePinExpectation {
            file_identity: &request.toolchain.ffprobe.file_identity,
            content_sha256: &request.toolchain.ffprobe.content_sha256,
            maximum_bytes: MAXIMUM_EXECUTABLE_BYTES,
            hash_timeout: deadlines.remaining()?,
        },
        media_pin,
        trusted_directories,
    )?;
    let cleanup_timeout = Duration::from_millis(request.limits.forced_kill_timeout_millis);
    let (stdout, stderr) = match child.take_pipes() {
        Ok(pipes) => pipes,
        Err(primary) => {
            return match child.cleanup_force_reap(cleanup_timeout) {
                Ok(_) => Err(SupervisorError::Platform(primary)),
                Err(cleanup) => Err(SupervisorError::CleanupFailed {
                    primary: primary.to_string(),
                    cleanup: cleanup.to_string(),
                }),
            };
        }
    };
    let stdout_limit = request.limits.progress_max_total_bytes;
    let out_thread = std::thread::spawn(move || drain_required_bytes(stdout, stdout_limit));
    let err_limits = BoundedDrainLimits {
        max_total_bytes: request.limits.stderr_max_total_bytes,
        tail_bytes: usize::try_from(request.limits.stderr_tail_bytes).unwrap_or(usize::MAX),
    };
    let err_thread = std::thread::spawn(move || drain_diagnostics(stderr, err_limits));
    let process_result = (|| {
        let configured_timeout = Duration::from_millis(request.limits.ffprobe_timeout_millis);
        let wait_timeout = configured_timeout.min(deadlines.remaining()?);
        let Some(exit) = child.wait_timeout(wait_timeout)? else {
            deadlines.ensure()?;
            return Err(SupervisorError::FfprobeFailed);
        };
        deadlines.ensure()?;
        if !exit.successful() {
            return Err(SupervisorError::FfprobeFailed);
        }
        if !wait_for_scope_empty(
            &child,
            Duration::from_millis(request.limits.reap_timeout_millis),
        )? {
            return Err(SupervisorError::ContainmentUnproven);
        }
        Ok(())
    })();
    if let Err(primary) = process_result {
        if let Err(cleanup) = child.cleanup_force_reap(cleanup_timeout) {
            // An unproven scope may still own pipe writers. Never synchronously
            // join in that state; detach and fail within the declared deadline.
            drop(out_thread);
            drop(err_thread);
            return Err(SupervisorError::CleanupFailed {
                primary: primary.to_string(),
                cleanup: cleanup.to_string(),
            });
        }
        let out_join = out_thread.join();
        let err_join = err_thread.join();
        if out_join.is_err() || err_join.is_err() {
            return Err(SupervisorError::CleanupFailed {
                primary: primary.to_string(),
                cleanup: "ffprobe drain worker failed after terminal cleanup".to_owned(),
            });
        }
        return Err(primary);
    }
    let out_join = out_thread.join();
    let err_join = err_thread.join();
    let json = out_join
        .map_err(|_| SupervisorError::DrainJoinFailed)?
        .map_err(SupervisorError::Diagnostic)?;
    let diagnostics = err_join
        .map_err(|_| SupervisorError::DrainJoinFailed)?
        .map_err(SupervisorError::Diagnostic)?;
    if diagnostics.total_limit_exceeded {
        return Err(SupervisorError::DiagnosticLimitExceeded);
    }
    deadlines.ensure()?;
    Ok(json)
}

#[cfg(target_os = "linux")]
fn spawn_bound_ffprobe(
    executable: &Path,
    arguments: &[String],
    environment: &[(String, String)],
    working_directory: &Path,
    expectation: ExecutablePinExpectation<'_>,
    media_pin: Option<&GovernedPathPin>,
    trusted_directories: Option<(&TrustedDirectoryPin, &TrustedDirectoryPin)>,
) -> Result<PlatformChild, PlatformError> {
    let pin = media_pin.ok_or_else(|| {
        PlatformError::state(
            "bind ffprobe media descriptor",
            "missing governed media pin",
        )
    })?;
    let bindings = [InheritedFdBinding::new(
        pin,
        i32::from(FFMPEG_LINUX_FD_INPUT_BASE_V1),
    )?];
    if let Some((working, temporary)) = trusted_directories {
        spawn_verified_with_bindings(
            executable,
            arguments,
            environment,
            &working.pin,
            &temporary.pin,
            expectation,
            &bindings,
        )
    } else {
        let local_working = pin_governed_directory(working_directory)?;
        spawn_verified_with_bindings(
            executable,
            arguments,
            environment,
            &local_working,
            &local_working,
            expectation,
            &bindings,
        )
    }
}

#[cfg(not(target_os = "linux"))]
fn spawn_bound_ffprobe(
    executable: &Path,
    arguments: &[String],
    environment: &[(String, String)],
    working_directory: &Path,
    expectation: ExecutablePinExpectation<'_>,
    _media_pin: Option<&GovernedPathPin>,
    _trusted_directories: Option<(&TrustedDirectoryPin, &TrustedDirectoryPin)>,
) -> Result<PlatformChild, PlatformError> {
    spawn_verified(
        executable,
        arguments,
        environment,
        working_directory,
        expectation,
    )
}

fn append_ffprobe_media_argument(
    arguments: &mut Vec<String>,
    pathname: &str,
    operating_system: FfmpegHostOperatingSystemV1,
) {
    if operating_system == FfmpegHostOperatingSystemV1::Linux {
        arguments.extend([
            "-protocol_whitelist".to_owned(),
            "fd".to_owned(),
            "-fd".to_owned(),
            FFMPEG_LINUX_FD_INPUT_BASE_V1.to_string(),
            "fd:".to_owned(),
        ]);
    } else {
        arguments.push(pathname.to_owned());
    }
}

fn demuxer_name(demuxer: FfmpegInputDemuxerV1) -> &'static str {
    match demuxer {
        FfmpegInputDemuxerV1::AacAdts => "aac",
        FfmpegInputDemuxerV1::H264AnnexB => "h264",
    }
}

fn stream_specifier(kind: FfmpegStreamKindV1, index: u16) -> String {
    let kind = match kind {
        FfmpegStreamKindV1::Video => "v",
        FfmpegStreamKindV1::Audio => "a",
        FfmpegStreamKindV1::Subtitle => "s",
        FfmpegStreamKindV1::Data => "d",
        FfmpegStreamKindV1::Attachment => "t",
    };
    format!("{kind}:{index}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PacketFraming {
    AacAdts,
    AacRaw,
    H264AnnexB,
    H264Container,
}

fn packet_framing(demuxer: FfmpegInputDemuxerV1, output: bool) -> PacketFraming {
    match (demuxer, output) {
        (FfmpegInputDemuxerV1::AacAdts, false) => PacketFraming::AacAdts,
        (FfmpegInputDemuxerV1::AacAdts, true) => PacketFraming::AacRaw,
        (FfmpegInputDemuxerV1::H264AnnexB, false) => PacketFraming::H264AnnexB,
        (FfmpegInputDemuxerV1::H264AnnexB, true) => PacketFraming::H264Container,
    }
}

fn sorted_output_streams(
    mut streams: Vec<ProbeStream>,
    expected_count: usize,
) -> Result<Vec<ProbeStream>, SupervisorError> {
    streams.sort_by_key(|stream| stream.index);
    if streams.len() != expected_count
        || streams
            .iter()
            .enumerate()
            .any(|(expected, stream)| stream.index != u32::try_from(expected).unwrap_or(u32::MAX))
    {
        return Err(SupervisorError::FfprobeInvalid);
    }
    Ok(streams)
}

fn require_payload_match(expected: &str, observed: &str) -> Result<(), SupervisorError> {
    if expected == observed {
        Ok(())
    } else {
        Err(SupervisorError::FfprobeInvalid)
    }
}

fn packet_payload_sha256(
    packets: &[ProbePacket],
    maximum_packets: u64,
    framing: PacketFraming,
) -> Result<String, SupervisorError> {
    if packets.is_empty() || packets.len() > usize::try_from(maximum_packets).unwrap_or(usize::MAX)
    {
        return Err(SupervisorError::FfprobeInvalid);
    }
    let mut projection = Sha256::new();
    projection.update(b"ff.ffmpeg-packet-payload@1\0");
    for (ordinal, packet) in packets.iter().enumerate() {
        let size = packet
            .size
            .parse::<u64>()
            .map_err(|_| SupervisorError::FfprobeInvalid)?;
        if size == 0 {
            return Err(SupervisorError::FfprobeInvalid);
        }
        let bytes = decode_packet_data(&packet.data)?;
        if usize::try_from(size).ok() != Some(bytes.len()) {
            return Err(SupervisorError::FfprobeInvalid);
        }
        let declared_digest = packet
            .data_hash
            .strip_prefix("SHA256:")
            .ok_or(SupervisorError::FfprobeInvalid)?;
        let declared_digest = decode_sha256(declared_digest)?;
        if Sha256::digest(&bytes).as_slice() != declared_digest {
            return Err(SupervisorError::FfprobeInvalid);
        }
        let units = canonical_packet_units(&bytes, framing)?;
        projection.update(u64::try_from(ordinal).unwrap_or(u64::MAX).to_be_bytes());
        projection.update(u64::try_from(units.len()).unwrap_or(u64::MAX).to_be_bytes());
        for unit in units {
            projection.update(u64::try_from(unit.len()).unwrap_or(u64::MAX).to_be_bytes());
            projection.update(unit);
        }
    }
    projection.update(
        u64::try_from(packets.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    Ok(hex_digest(projection.finalize()))
}

fn decode_packet_data(value: &str) -> Result<Vec<u8>, SupervisorError> {
    let mut output = Vec::new();
    for line in value.lines().filter(|line| !line.trim().is_empty()) {
        let (_, encoded) = line
            .split_once(':')
            .ok_or(SupervisorError::FfprobeInvalid)?;
        let encoded = encoded.split("  ").next().unwrap_or(encoded);
        for group in encoded.split_ascii_whitespace() {
            if group.len() % 2 != 0 || !group.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(SupervisorError::FfprobeInvalid);
            }
            for pair in group.as_bytes().chunks_exact(2) {
                let text =
                    std::str::from_utf8(pair).map_err(|_| SupervisorError::FfprobeInvalid)?;
                output.push(
                    u8::from_str_radix(text, 16).map_err(|_| SupervisorError::FfprobeInvalid)?,
                );
            }
        }
    }
    if output.is_empty() {
        return Err(SupervisorError::FfprobeInvalid);
    }
    Ok(output)
}

fn canonical_packet_units(
    bytes: &[u8],
    framing: PacketFraming,
) -> Result<Vec<&[u8]>, SupervisorError> {
    match framing {
        PacketFraming::AacAdts => {
            if bytes.len() < 7
                || bytes[0] != 0xff
                || bytes[1] & 0xf6 != 0xf0
                || bytes[6] & 0x03 != 0
            {
                return Err(SupervisorError::FfprobeInvalid);
            }
            let header_length = if bytes[1] & 0x01 == 0 { 9 } else { 7 };
            let frame_length = (usize::from(bytes[3] & 0x03) << 11)
                | (usize::from(bytes[4]) << 3)
                | usize::from(bytes[5] >> 5);
            if frame_length != bytes.len() || bytes.len() <= header_length {
                return Err(SupervisorError::FfprobeInvalid);
            }
            Ok(vec![&bytes[header_length..]])
        }
        PacketFraming::AacRaw => Ok(vec![bytes]),
        PacketFraming::H264AnnexB => split_h264_annex_b(bytes),
        PacketFraming::H264Container => split_h264_length_prefixed(bytes),
    }
}

fn h264_start_code_length(bytes: &[u8], offset: usize) -> Option<usize> {
    let remaining = bytes.get(offset..)?;
    if remaining.starts_with(&[0, 0, 0, 1]) {
        Some(4)
    } else if remaining.starts_with(&[0, 0, 1]) {
        Some(3)
    } else {
        None
    }
}

fn split_h264_annex_b(bytes: &[u8]) -> Result<Vec<&[u8]>, SupervisorError> {
    let first_length = h264_start_code_length(bytes, 0).ok_or(SupervisorError::FfprobeInvalid)?;
    let mut units = Vec::new();
    let mut payload_start = first_length;
    let mut cursor = payload_start;
    while cursor < bytes.len() {
        if let Some(length) = h264_start_code_length(bytes, cursor) {
            if cursor == payload_start {
                return Err(SupervisorError::FfprobeInvalid);
            }
            units.push(&bytes[payload_start..cursor]);
            payload_start = cursor + length;
            cursor = payload_start;
        } else {
            cursor += 1;
        }
    }
    if payload_start >= bytes.len() {
        return Err(SupervisorError::FfprobeInvalid);
    }
    units.push(&bytes[payload_start..]);
    Ok(units)
}

fn split_h264_length_prefixed(bytes: &[u8]) -> Result<Vec<&[u8]>, SupervisorError> {
    let mut units = Vec::new();
    let mut cursor = 0_usize;
    while cursor < bytes.len() {
        let length_bytes: [u8; 4] = bytes
            .get(cursor..cursor.saturating_add(4))
            .and_then(|value| value.try_into().ok())
            .ok_or(SupervisorError::FfprobeInvalid)?;
        cursor = cursor.saturating_add(4);
        let length = usize::try_from(u32::from_be_bytes(length_bytes))
            .map_err(|_| SupervisorError::FfprobeInvalid)?;
        if length == 0 {
            return Err(SupervisorError::FfprobeInvalid);
        }
        let end = cursor
            .checked_add(length)
            .ok_or(SupervisorError::FfprobeInvalid)?;
        let unit = bytes
            .get(cursor..end)
            .ok_or(SupervisorError::FfprobeInvalid)?;
        units.push(unit);
        cursor = end;
    }
    if units.is_empty() {
        return Err(SupervisorError::FfprobeInvalid);
    }
    Ok(units)
}

fn decode_sha256(value: &str) -> Result<[u8; 32], SupervisorError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(SupervisorError::FfprobeInvalid);
    }
    let mut output = [0_u8; 32];
    for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
        let text = std::str::from_utf8(chunk).map_err(|_| SupervisorError::FfprobeInvalid)?;
        output[index] =
            u8::from_str_radix(text, 16).map_err(|_| SupervisorError::FfprobeInvalid)?;
    }
    Ok(output)
}

fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    let digest = digest.as_ref();
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push(char::from(LOWER_HEX[usize::from(byte >> 4)]));
        output.push(char::from(LOWER_HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn hash_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        output.push(char::from(LOWER_HEX[usize::from(byte >> 4)]));
        output.push(char::from(LOWER_HEX[usize::from(byte & 0x0f)]));
    }
    output
}

const LOWER_HEX: &[u8; 16] = b"0123456789abcdef";

fn validate_commit(value: &str) -> Result<(), SupervisorError> {
    if value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(SupervisorError::Filesystem("source commit identity"))
    }
}

#[cfg(test)]
mod output_validation_tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use crate::identity::observe_executable;
    use fforager_contracts::{ByteCreditContractV1, ResourceContractV1, ResourceVector};

    struct TestAdmission {
        resources: OwnedResourceBroker,
        bytes: OwnedByteCreditBroker,
        resource_lease: Option<OwnedResourceLease>,
        pipe_lease: Option<OwnedByteCreditLease>,
        lifecycle: FfmpegLifecycleV2,
    }

    fn admitted_test_lifecycle(owner_value: u64) -> TestAdmission {
        let claim = ResourceVector {
            ffmpeg_processes: 1,
            open_handles: 1,
            ..ResourceVector::default()
        };
        let resources =
            OwnedResourceBroker::from_contract(&ResourceContractV1::new(claim, 1, 1, 0, 0))
                .expect("resources");
        let bytes =
            OwnedByteCreditBroker::from_contract(&ByteCreditContractV1::new(1, 1)).expect("bytes");
        let owner = OwnerId(owner_value);
        let resource_lease = Some(match resources.request(owner, claim).expect("admission") {
            OwnedAdmission::Granted(lease) => lease,
            OwnedAdmission::Queued(_) => panic!("first claim must be granted"),
        });
        let pipe_lease = Some(
            bytes
                .claim(owner, ByteCreditStage::FfmpegPipe, 1)
                .expect("pipe claim"),
        );
        let lifecycle = FfmpegLifecycleV2::new(
            MachineInstanceId::new(owner.0).expect("instance"),
            FfmpegLifecycleLimitsV2::new(2, 2, 2, 2).expect("limits"),
            32,
        );
        TestAdmission {
            resources,
            bytes,
            resource_lease,
            pipe_lease,
            lifecycle,
        }
    }

    fn advance_to_post_diagnostics(
        lifecycle: &mut FfmpegLifecycleV2,
        termination: FfmpegTerminationV2,
    ) {
        lifecycle.start().expect("spawn pending");
        complete_lifecycle(lifecycle, FfmpegEffectOutcomeV2::Spawned).expect("spawned");
        lifecycle.observe_child_exit().expect("exit observed");
        complete_lifecycle(
            lifecycle,
            FfmpegEffectOutcomeV2::Reap(FfmpegReapOutcomeV2::Reaped(termination)),
        )
        .expect("reaped");
        complete_lifecycle(lifecycle, FfmpegEffectOutcomeV2::DiagnosticsPreserved)
            .expect("diagnostics");
    }

    fn packet(stream_index: u32, bytes: &[u8]) -> ProbePacket {
        let encoded = bytes.iter().fold(String::new(), |mut output, byte| {
            use std::fmt::Write as _;
            write!(output, "{byte:02x}").expect("write to String");
            output
        });
        ProbePacket {
            stream_index,
            size: bytes.len().to_string(),
            data_hash: format!("SHA256:{}", hash_bytes(bytes)),
            data: format!("\n00000000: {encoded}\n"),
        }
    }

    fn adts(payload: &[u8]) -> Vec<u8> {
        let length = payload.len() + 7;
        let mut frame = vec![
            0xff,
            0xf1,
            0x50,
            0x80 | u8::try_from((length >> 11) & 0x03).expect("two bits"),
            u8::try_from((length >> 3) & 0xff).expect("eight bits"),
            u8::try_from((length & 0x07) << 5).expect("three bits") | 0x1f,
            0xfc,
        ];
        frame.extend_from_slice(payload);
        frame
    }

    fn stream(index: u32) -> ProbeStream {
        ProbeStream {
            index,
            codec_type: "audio".to_owned(),
            codec_name: "aac".to_owned(),
            width: None,
            height: None,
            channels: Some(2),
        }
    }

    #[test]
    fn windows_writable_profile_roots_are_all_job_scoped() {
        let mut environment = Vec::new();
        append_windows_writable_environment(&mut environment, r"C:\job\tmp");
        assert_eq!(environment.len(), 6);
        for (name, value) in environment {
            assert!(matches!(
                name.as_str(),
                "TEMP" | "TMP" | "PROGRAMDATA" | "LOCALAPPDATA" | "APPDATA" | "USERPROFILE"
            ));
            assert_eq!(value, r"C:\job\tmp");
            assert!(!value.contains('%'));
        }
    }

    #[test]
    fn every_capability_cache_dimension_is_rejected_before_spawn_is_reachable() {
        let expected = fforager_contracts::FfmpegExecutableIdentityV1 {
            kind: fforager_contracts::FfmpegExecutableKindV1::Ffmpeg,
            absolute_path: if cfg!(windows) {
                r"C:\trusted\ffmpeg.exe".to_owned()
            } else {
                "/trusted/ffmpeg".to_owned()
            },
            file_identity: "identity-a".to_owned(),
            content_sha256: "content-a".to_owned(),
            version_output_sha256: "version-output-a".to_owned(),
            normalized_version: "ffmpeg version governed".to_owned(),
            host: crate::identity::local_host_identity(),
            capability_binding: fforager_contracts::FfmpegCapabilityBindingV1 {
                executable_content_sha256: "content-a".to_owned(),
                normalized_probe_sha256: "probe-a".to_owned(),
                capabilities: vec!["progress".to_owned(), "stream_copy".to_owned()],
            },
        };
        let baseline = ExecutableCapabilityObservation::test_fixture(
            ExecutableObservation::test_fixture(
                expected.absolute_path.clone(),
                expected.file_identity.clone(),
                expected.content_sha256.clone(),
                1,
            ),
            expected.host,
            expected.version_output_sha256.clone(),
            expected.normalized_version.clone(),
            expected.capability_binding.normalized_probe_sha256.clone(),
            expected.capability_binding.capabilities.clone(),
        );
        verify_capability_binding(&expected, &baseline).expect("baseline capability binding");

        let mut mutations = Vec::new();
        let mut identity = baseline.clone();
        identity.test_mutate_executable_file_identity("identity-b".to_owned());
        mutations.push(("file identity", identity));
        let mut content = baseline.clone();
        content.test_mutate_executable_content("content-b".to_owned());
        mutations.push(("executable content", content));
        let mut operating_system = baseline.clone();
        let mut changed_host = operating_system.host();
        changed_host.operating_system = match changed_host.operating_system {
            fforager_contracts::FfmpegHostOperatingSystemV1::Windows => {
                fforager_contracts::FfmpegHostOperatingSystemV1::Linux
            }
            fforager_contracts::FfmpegHostOperatingSystemV1::Linux => {
                fforager_contracts::FfmpegHostOperatingSystemV1::Windows
            }
        };
        operating_system.test_mutate_host(changed_host);
        mutations.push(("host operating system", operating_system));
        let mut architecture = baseline.clone();
        let mut changed_host = architecture.host();
        changed_host.architecture = match changed_host.architecture {
            fforager_contracts::FfmpegHostArchitectureV1::X86_64 => {
                fforager_contracts::FfmpegHostArchitectureV1::Aarch64
            }
            fforager_contracts::FfmpegHostArchitectureV1::Aarch64 => {
                fforager_contracts::FfmpegHostArchitectureV1::X86_64
            }
        };
        architecture.test_mutate_host(changed_host);
        mutations.push(("host architecture", architecture));
        let mut version_output = baseline.clone();
        version_output.test_mutate_version_output("version-output-b".to_owned());
        mutations.push(("version output", version_output));
        let mut normalized_version = baseline.clone();
        normalized_version.test_mutate_normalized_version("ffmpeg version substituted".to_owned());
        mutations.push(("normalized version", normalized_version));
        let mut normalized_probe = baseline.clone();
        normalized_probe.test_mutate_probe("probe-b".to_owned());
        mutations.push(("normalized probe", normalized_probe));
        let mut capabilities = baseline;
        capabilities.test_mutate_capabilities(vec!["progress".to_owned()]);
        mutations.push(("capability set", capabilities));

        for (dimension, mutation) in mutations {
            assert!(
                matches!(
                    verify_capability_binding(&expected, &mutation),
                    Err(SupervisorError::ExecutableIdentityChanged(
                        "version or capability binding"
                    ))
                ),
                "{dimension} mutation reached beyond the pre-spawn verifier"
            );
        }
    }

    #[test]
    fn public_trusted_root_boundary_rejects_another_valid_job_before_admission() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = cleanup_test_root().join(format!("trusted-root-{}-{nonce}", std::process::id()));
        let trusted_root = root.join("trusted");
        let requested_root = root.join("requested");
        std::fs::create_dir_all(&trusted_root).expect("trusted root");
        std::fs::create_dir_all(requested_root.join("input")).expect("requested input");
        std::fs::create_dir_all(requested_root.join("output")).expect("requested output");
        std::fs::write(requested_root.join("input/audio.aac"), b"audio").expect("audio");
        std::fs::write(requested_root.join("input/video.h264"), b"video").expect("video");
        let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates")
            .join("fforager-contracts/testdata/ffmpeg-supervision-v1.0.json");
        let fixture: serde_json::Value =
            serde_json::from_slice(&std::fs::read(fixture_path).expect("fixture bytes"))
                .expect("fixture JSON");
        let mut request: FfmpegSupervisionRequestV1 =
            serde_json::from_value(fixture["request"].clone()).expect("request");
        let requested_text = requested_root.to_str().expect("requested root text");
        request.working_directory = requested_text
            .strip_prefix(r"\\?\")
            .unwrap_or(requested_text)
            .to_owned();
        if cfg!(target_os = "linux") {
            let host = crate::identity::local_host_identity();
            request.toolchain.ffmpeg.host = host;
            request.toolchain.ffprobe.host = host;
            request.toolchain.ffmpeg.absolute_path = "/usr/bin/ffmpeg".to_owned();
            request.toolchain.ffprobe.absolute_path = "/usr/bin/ffprobe".to_owned();
            request.environment.trusted_bindings = vec![
                FfmpegEnvironmentBindingV1::JobTemporaryDirectory,
                FfmpegEnvironmentBindingV1::LocaleC,
            ];
            request
                .toolchain
                .ffmpeg
                .capability_binding
                .capabilities
                .push("protocol:fd".to_owned());
            request
                .toolchain
                .ffmpeg
                .capability_binding
                .capabilities
                .sort();
            request
                .toolchain
                .ffprobe
                .capability_binding
                .capabilities
                .push("protocol:fd".to_owned());
            request
                .toolchain
                .ffprobe
                .capability_binding
                .capabilities
                .sort();
        }
        let invocation = request
            .validate_for_invocation()
            .expect("valid foreign-root request");
        let trusted = TrustedDirectoryPin::acquire(&trusted_root).expect("trusted pin");
        let resources = OwnedResourceBroker::from_contract(&ResourceContractV1::new(
            request.resources.claim,
            1,
            1,
            0,
            0,
        ))
        .expect("resource broker");
        let pipe_bytes = request
            .resources
            .progress_pipe_bytes
            .checked_add(request.resources.stderr_pipe_bytes)
            .expect("pipe bytes");
        let bytes = OwnedByteCreditBroker::from_contract(&ByteCreditContractV1::new(pipe_bytes, 1))
            .expect("byte broker");
        let spawn_called = std::cell::Cell::new(false);
        let result =
            FfmpegSupervisor.validate_trusted_working_directory(&request, &invocation, &trusted);
        assert!(matches!(result, Err(SupervisorError::Filesystem(_))));
        assert!(!spawn_called.get());
        assert_eq!(resources.active_grant_count(), 0);
        assert_eq!(bytes.global_occupancy().expect("occupancy"), (0, 0));
    }

    #[test]
    fn retained_trusted_root_and_temp_fail_closed_on_post_preflight_swap() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = cleanup_test_root().join(format!("trusted-swap-{}-{nonce}", std::process::id()));
        let working = root.join("working");
        let temporary = working.join("tmp");
        let outside = root.join("outside");
        std::fs::create_dir_all(&temporary).expect("temporary");
        std::fs::create_dir_all(&outside).expect("outside");
        let working_pin = TrustedDirectoryPin::acquire(&working).expect("working pin");
        let temporary_pin = TrustedDirectoryPin::acquire(&temporary).expect("temporary pin");
        let retained = root.join("retained-working");
        let rename = std::fs::rename(&working, &retained);
        #[cfg(windows)]
        {
            assert!(rename.is_err(), "non-delete-sharing pins must block rename");
            working_pin.verify_retained().expect("working retained");
            temporary_pin.verify_retained().expect("temporary retained");
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::symlink;
            match rename {
                Ok(()) => {
                    symlink(&outside, &working).expect("substitute root symlink");
                    assert!(working_pin.verify_retained().is_err());
                    assert!(temporary_pin.verify_retained().is_err());
                }
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                    working_pin.verify_retained().expect("working retained");
                    temporary_pin.verify_retained().expect("temporary retained");
                }
                Err(error) => panic!("unexpected substitution failure: {error}"),
            }
            assert!(!outside.join("sentinel").exists());
        }
    }

    #[test]
    fn zero_deadline_latches_in_prepared_state_without_starting_spawn() {
        let mut lifecycle = FfmpegLifecycleV2::new(
            MachineInstanceId::new(0xdead).expect("instance"),
            FfmpegLifecycleLimitsV2::new(2, 2, 2, 2).expect("limits"),
            16,
        );
        assert!(latch_cancellation(Some(Instant::now()), &mut lifecycle).expect("latch"));
        assert!(lifecycle.cancellation_requested());
        assert_eq!(lifecycle.state(), FfmpegLifecycleStateV2::ReleasePending);
        assert_eq!(
            lifecycle.pending_effect().expect("release pending").effect,
            FfmpegEffectV2::ReleaseResources
        );
    }

    #[test]
    fn expired_startup_deadline_never_invokes_spawn_boundary() {
        let spawn_called = std::cell::Cell::new(false);
        let result: Result<(), SupervisorError> = run_before_startup_deadline(
            PhaseDeadlines {
                phase_deadline: Instant::now(),
                cancellation_deadline: None,
                phase: DeadlinePhase::Startup,
            },
            || {
                spawn_called.set(true);
                Err(PlatformError::state("sentinel spawn", "must not run"))
            },
            |()| panic!("cleanup is unreachable when pre-spawn deadline rejects"),
        );
        assert!(matches!(result, Err(SupervisorError::StartupTimedOut)));
        assert!(!spawn_called.get());
    }

    #[test]
    fn startup_deadline_crossing_during_spawn_forces_bounded_child_cleanup() {
        let root = cleanup_test_root().join(format!("startup-crossing-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("root");
        let (executable, arguments, environment) = cleanup_test_process(&root);
        let cleaned = std::cell::Cell::new(false);
        let result = run_before_startup_deadline(
            PhaseDeadlines {
                phase_deadline: Instant::now() + Duration::from_millis(200),
                cancellation_deadline: None,
                phase: DeadlinePhase::Startup,
            },
            || {
                let child = crate::platform::spawn(&executable, &arguments, &environment, &root)?;
                std::thread::sleep(Duration::from_millis(250));
                Ok(child)
            },
            |child| {
                child
                    .cleanup_force_reap(Duration::from_secs(2))
                    .map_err(SupervisorError::Platform)?;
                assert!(
                    child
                        .declared_scope_empty()
                        .map_err(SupervisorError::Platform)?
                );
                cleaned.set(true);
                Ok(())
            },
        );
        assert!(matches!(result, Err(SupervisorError::StartupTimedOut)));
        assert!(cleaned.get());
    }

    #[test]
    fn startup_deadline_crossing_after_admission_settles_release_before_spawn() {
        let mut fixture = admitted_test_lifecycle(0x5101);
        fixture.lifecycle.start().expect("spawn pending");
        let result = startup_remaining_or_settle(
            PhaseDeadlines {
                phase_deadline: Instant::now(),
                cancellation_deadline: None,
                phase: DeadlinePhase::Startup,
            },
            &mut fixture.lifecycle,
            &mut fixture.pipe_lease,
            &mut fixture.resource_lease,
        );
        assert!(matches!(result, Err(SupervisorError::StartupTimedOut)));
        assert!(fixture.lifecycle.resources_released());
        assert_eq!(fixture.lifecycle.state(), FfmpegLifecycleStateV2::Failed);
        assert_eq!(fixture.resources.active_grant_count(), 0);
        assert_eq!(fixture.bytes.global_occupancy().expect("occupancy"), (0, 0));
    }

    #[test]
    fn cancellation_crossing_admitted_preflight_error_wins_and_releases() {
        let mut fixture = admitted_test_lifecycle(0x5102);
        fixture.lifecycle.start().expect("spawn pending");
        let result = settle_pre_spawn_error(
            &mut fixture.lifecycle,
            &mut fixture.pipe_lease,
            &mut fixture.resource_lease,
            Some(Instant::now()),
            SupervisorError::Filesystem("preflight sentinel"),
        );
        assert!(matches!(
            result,
            SupervisorError::Cancelled { forced: false }
        ));
        assert!(fixture.lifecycle.cancellation_requested());
        assert!(fixture.lifecycle.resources_released());
        assert_eq!(fixture.lifecycle.state(), FfmpegLifecycleStateV2::Cancelled);
        assert_eq!(fixture.resources.active_grant_count(), 0);
        assert_eq!(fixture.bytes.global_occupancy().expect("occupancy"), (0, 0));
    }

    #[test]
    fn cancellation_crossing_physical_success_release_wins_terminal_lineage() {
        let claim = ResourceVector {
            ffmpeg_processes: 1,
            open_handles: 1,
            ..ResourceVector::default()
        };
        let resources =
            OwnedResourceBroker::from_contract(&ResourceContractV1::new(claim, 1, 1, 0, 0))
                .expect("resources");
        let bytes =
            OwnedByteCreditBroker::from_contract(&ByteCreditContractV1::new(1, 1)).expect("bytes");
        let owner = OwnerId(0xface);
        let mut resource_lease = Some(match resources.request(owner, claim).expect("admission") {
            OwnedAdmission::Granted(lease) => lease,
            OwnedAdmission::Queued(_) => panic!("first claim must be granted"),
        });
        let mut pipe_lease = Some(
            bytes
                .claim(owner, ByteCreditStage::FfmpegPipe, 1)
                .expect("pipe claim"),
        );
        let mut lifecycle = FfmpegLifecycleV2::new(
            MachineInstanceId::new(owner.0).expect("instance"),
            FfmpegLifecycleLimitsV2::new(2, 2, 2, 2).expect("limits"),
            32,
        );
        lifecycle.start().expect("spawn pending");
        complete_lifecycle(&mut lifecycle, FfmpegEffectOutcomeV2::Spawned).expect("spawned");
        lifecycle.observe_child_exit().expect("exit observed");
        complete_lifecycle(
            &mut lifecycle,
            FfmpegEffectOutcomeV2::Reap(FfmpegReapOutcomeV2::Reaped(
                FfmpegTerminationV2::ExitCode(0),
            )),
        )
        .expect("reaped");
        complete_lifecycle(&mut lifecycle, FfmpegEffectOutcomeV2::DiagnosticsPreserved)
            .expect("diagnostics");
        complete_lifecycle(&mut lifecycle, FfmpegEffectOutcomeV2::OutputValidated).expect("output");
        let cancelled = release_terminal_leases(
            &mut pipe_lease,
            &mut resource_lease,
            &mut lifecycle,
            Some(Instant::now() + Duration::from_millis(1)),
            || std::thread::sleep(Duration::from_millis(5)),
        )
        .expect("release");
        assert!(cancelled);
        assert!(lifecycle.resources_released());
        assert_eq!(lifecycle.state(), FfmpegLifecycleStateV2::Cancelled);
        assert_eq!(resources.active_grant_count(), 0);
        assert_eq!(bytes.global_occupancy().expect("occupancy"), (0, 0));
    }

    fn assert_validation_failure_cancellation_wins(primary: SupervisorError, owner: u64) {
        let mut fixture = admitted_test_lifecycle(owner);
        advance_to_post_diagnostics(&mut fixture.lifecycle, FfmpegTerminationV2::ExitCode(0));
        let result = settle_validation_failure(
            &mut fixture.lifecycle,
            &mut fixture.pipe_lease,
            &mut fixture.resource_lease,
            Some(Instant::now()),
            false,
            primary,
            || {},
        );
        assert!(matches!(
            result,
            SupervisorError::Cancelled { forced: false }
        ));
        assert!(fixture.lifecycle.cancellation_requested());
        assert!(fixture.lifecycle.resources_released());
        assert_eq!(fixture.lifecycle.state(), FfmpegLifecycleStateV2::Cancelled);
        assert_eq!(fixture.resources.active_grant_count(), 0);
        assert_eq!(fixture.bytes.global_occupancy().expect("occupancy"), (0, 0));
    }

    #[test]
    fn cancellation_crossing_output_pin_error_is_latched_before_release() {
        assert_validation_failure_cancellation_wins(
            SupervisorError::Filesystem("output pin sentinel"),
            0x5103,
        );
    }

    #[test]
    fn cancellation_crossing_pre_ffprobe_deadline_is_latched_before_release() {
        assert_validation_failure_cancellation_wins(SupervisorError::ValidationTimedOut, 0x5104);
    }

    #[test]
    fn cancellation_crossing_validation_physical_release_wins_terminal_lineage() {
        let mut fixture = admitted_test_lifecycle(0x5105);
        advance_to_post_diagnostics(&mut fixture.lifecycle, FfmpegTerminationV2::ExitCode(0));
        let result = settle_validation_failure(
            &mut fixture.lifecycle,
            &mut fixture.pipe_lease,
            &mut fixture.resource_lease,
            Some(Instant::now() + Duration::from_millis(1)),
            false,
            SupervisorError::FfprobeInvalid,
            || std::thread::sleep(Duration::from_millis(5)),
        );
        assert!(matches!(
            result,
            SupervisorError::Cancelled { forced: false }
        ));
        assert!(fixture.lifecycle.resources_released());
        assert_eq!(fixture.lifecycle.state(), FfmpegLifecycleStateV2::Cancelled);
        assert_eq!(fixture.resources.active_grant_count(), 0);
        assert_eq!(fixture.bytes.global_occupancy().expect("occupancy"), (0, 0));
    }

    #[test]
    fn validation_cleanup_failure_preserves_correlated_primary_error() {
        let mut fixture = admitted_test_lifecycle(0x5106);
        advance_to_post_diagnostics(&mut fixture.lifecycle, FfmpegTerminationV2::ExitCode(0));
        fixture
            .pipe_lease
            .take()
            .expect("pipe lease")
            .release()
            .expect("controlled early release");
        let result = settle_validation_failure(
            &mut fixture.lifecycle,
            &mut fixture.pipe_lease,
            &mut fixture.resource_lease,
            None,
            false,
            SupervisorError::Filesystem("output pin sentinel"),
            || {},
        );
        let SupervisorError::CleanupFailed { primary, cleanup } = result else {
            panic!("validation cleanup failure must retain both errors");
        };
        assert!(primary.contains("output pin sentinel"));
        assert!(cleanup.contains("pipe lease already released"));
        assert_eq!(fixture.resources.active_grant_count(), 1);
        assert_eq!(fixture.bytes.global_occupancy().expect("occupancy"), (0, 0));
    }

    #[test]
    fn nonzero_reap_release_crossing_stays_single_reap_and_cancellation_lineage() {
        let mut fixture = admitted_test_lifecycle(0x5107);
        advance_to_post_diagnostics(&mut fixture.lifecycle, FfmpegTerminationV2::ExitCode(7));
        let result = settle_terminal_error(
            &mut fixture.lifecycle,
            &mut fixture.pipe_lease,
            &mut fixture.resource_lease,
            Some(Instant::now() + Duration::from_millis(1)),
            false,
            SupervisorError::NonzeroExit(ExitObservation {
                exit_code: Some(7),
                signal: None,
                windows_status_opaque: None,
                forced_by_supervisor: false,
            }),
            || std::thread::sleep(Duration::from_millis(5)),
        );
        assert!(matches!(
            result,
            SupervisorError::Cancelled { forced: false }
        ));
        assert_eq!(
            fixture.lifecycle.reap_outcome(),
            Some(FfmpegTerminationV2::ExitCode(7))
        );
        let reap_count = fixture
            .lifecycle
            .trace()
            .iter()
            .filter(|transition| {
                matches!(
                    transition.action,
                    FfmpegLifecycleActionV2::EffectCompleted {
                        outcome: FfmpegEffectOutcomeV2::Reap(_),
                        ..
                    }
                )
            })
            .count();
        assert_eq!(reap_count, 1);
        assert!(fixture.lifecycle.trace().iter().all(|transition| {
            transition
                .issued
                .is_none_or(|issued| issued.effect != FfmpegEffectV2::ForceKillProcessTree)
        }));
        assert_eq!(fixture.lifecycle.state(), FfmpegLifecycleStateV2::Cancelled);
        assert_eq!(fixture.resources.active_grant_count(), 0);
        assert_eq!(fixture.bytes.global_occupancy().expect("occupancy"), (0, 0));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn ffprobe_crossing_absolute_cancellation_deadline_is_bounded_cancelled() {
        let root = cleanup_test_root().join(format!("ffprobe-deadline-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("root");
        let media = root.join("media.bin");
        std::fs::write(&media, b"media").expect("media");
        let pin = pin_governed_file(&media, 1024, Duration::from_secs(1)).expect("media pin");
        let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates")
            .join("fforager-contracts/testdata/ffmpeg-supervision-v1.0.json");
        let fixture: serde_json::Value =
            serde_json::from_slice(&std::fs::read(fixture_path).expect("fixture bytes"))
                .expect("fixture JSON");
        let mut request: FfmpegSupervisionRequestV1 =
            serde_json::from_value(fixture["request"].clone()).expect("request");
        let shell = observe_executable(Path::new("/usr/bin/dash")).expect("shell identity");
        request.toolchain.ffprobe.absolute_path = shell.canonical_path().to_owned();
        request.toolchain.ffprobe.file_identity = shell.file_identity().to_owned();
        request.toolchain.ffprobe.content_sha256 = shell.content_sha256().to_owned();
        request.limits.ffprobe_timeout_millis = 5_000;
        request.limits.forced_kill_timeout_millis = 1_000;
        request.limits.reap_timeout_millis = 1_000;
        let arguments = vec!["-c".to_owned(), "sleep 5; printf '{}'".to_owned()];
        let started = Instant::now();
        let result = run_bounded_ffprobe(
            Path::new(shell.canonical_path()),
            &arguments,
            &request,
            &[("LANG".to_owned(), "C".to_owned())],
            &root,
            Some(&pin),
            PhaseDeadlines {
                phase_deadline: Instant::now() + Duration::from_secs(5),
                cancellation_deadline: Some(Instant::now() + Duration::from_millis(50)),
                phase: DeadlinePhase::Validation,
            },
            None,
        );
        assert!(matches!(result, Err(SupervisorError::Cancelled { .. })));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn multiple_ffprobes_share_one_cumulative_validation_deadline() {
        let root = cleanup_test_root().join(format!("ffprobe-cumulative-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("root");
        let media = root.join("media.bin");
        std::fs::write(&media, b"media").expect("media");
        let pin = pin_governed_file(&media, 1024, Duration::from_secs(1)).expect("media pin");
        let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates")
            .join("fforager-contracts/testdata/ffmpeg-supervision-v1.0.json");
        let fixture: serde_json::Value =
            serde_json::from_slice(&std::fs::read(fixture_path).expect("fixture bytes"))
                .expect("fixture JSON");
        let mut request: FfmpegSupervisionRequestV1 =
            serde_json::from_value(fixture["request"].clone()).expect("request");
        let shell = observe_executable(Path::new("/usr/bin/dash")).expect("shell identity");
        request.toolchain.ffprobe.absolute_path = shell.canonical_path().to_owned();
        request.toolchain.ffprobe.file_identity = shell.file_identity().to_owned();
        request.toolchain.ffprobe.content_sha256 = shell.content_sha256().to_owned();
        request.limits.ffprobe_timeout_millis = 5_000;
        request.limits.forced_kill_timeout_millis = 1_000;
        request.limits.reap_timeout_millis = 1_000;
        let started = Instant::now();
        let deadlines = PhaseDeadlines {
            phase_deadline: started + Duration::from_secs(2),
            cancellation_deadline: None,
            phase: DeadlinePhase::Validation,
        };
        let first = run_bounded_ffprobe(
            Path::new(shell.canonical_path()),
            &["-c".to_owned(), "sleep 0.1; printf '{}'".to_owned()],
            &request,
            &[("LANG".to_owned(), "C".to_owned())],
            &root,
            Some(&pin),
            deadlines,
            None,
        )
        .expect("first probe within shared deadline");
        assert_eq!(first, b"{}");
        let second = run_bounded_ffprobe(
            Path::new(shell.canonical_path()),
            &["-c".to_owned(), "sleep 5; printf '{}'".to_owned()],
            &request,
            &[("LANG".to_owned(), "C".to_owned())],
            &root,
            Some(&pin),
            deadlines,
            None,
        );
        assert!(matches!(second, Err(SupervisorError::ValidationTimedOut)));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn adts_and_container_aac_have_the_same_canonical_packet_payload() {
        let first = b"first-aac-access-unit";
        let second = b"second-aac-access-unit";
        let source = vec![packet(0, &adts(first)), packet(0, &adts(second))];
        let output = vec![packet(0, first), packet(0, second)];
        let source_digest =
            packet_payload_sha256(&source, 2, PacketFraming::AacAdts).expect("source");
        let output_digest =
            packet_payload_sha256(&output, 2, PacketFraming::AacRaw).expect("output");
        require_payload_match(&source_digest, &output_digest).expect("equivalent payload");
    }

    #[test]
    fn annex_b_and_length_prefixed_h264_have_the_same_canonical_packet_payload() {
        let annex_b = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3];
        let length_prefixed = [0, 0, 0, 3, 0x67, 1, 2, 0, 0, 0, 2, 0x68, 3];
        let source = packet_payload_sha256(&[packet(0, &annex_b)], 1, PacketFraming::H264AnnexB)
            .expect("source");
        let output = packet_payload_sha256(
            &[packet(0, &length_prefixed)],
            1,
            PacketFraming::H264Container,
        )
        .expect("output");
        require_payload_match(&source, &output).expect("equivalent payload");
    }

    #[test]
    fn container_length_prefix_that_begins_like_annex_b_is_not_misclassified() {
        let mut unit = vec![0x41; 468];
        unit[1] = 0x9a;
        let mut annex_b = vec![0, 0, 0, 1];
        annex_b.extend_from_slice(&unit);
        let mut length_prefixed = vec![0, 0, 1, 0xd4];
        length_prefixed.extend_from_slice(&unit);
        let source = packet_payload_sha256(&[packet(0, &annex_b)], 1, PacketFraming::H264AnnexB)
            .expect("source");
        let output = packet_payload_sha256(
            &[packet(0, &length_prefixed)],
            1,
            PacketFraming::H264Container,
        )
        .expect("output");
        require_payload_match(&source, &output).expect("unambiguous container framing");
    }

    #[test]
    fn wrong_partial_stale_substituted_and_reordered_payloads_are_rejected() {
        let first = b"first-packet";
        let second = b"second-packet";
        let source_packets = vec![packet(0, &adts(first)), packet(0, &adts(second))];
        let expected =
            packet_payload_sha256(&source_packets, 2, PacketFraming::AacAdts).expect("source");
        let counterexamples = [
            vec![packet(0, b"wrong-stream"), packet(0, second)],
            vec![packet(0, first)],
            vec![packet(0, b"stale-packet"), packet(0, second)],
            vec![packet(0, b"substituted!"), packet(0, second)],
            vec![packet(0, second), packet(0, first)],
        ];
        for counterexample in counterexamples {
            let observed = packet_payload_sha256(&counterexample, 2, PacketFraming::AacRaw)
                .expect("well-formed counterexample");
            assert!(matches!(
                require_payload_match(&expected, &observed),
                Err(SupervisorError::FfprobeInvalid)
            ));
        }
    }

    #[test]
    fn forged_packet_hash_and_truncated_packet_data_are_rejected() {
        let mut forged = packet(0, b"payload");
        forged.data_hash = format!("SHA256:{}", "0".repeat(64));
        assert!(matches!(
            packet_payload_sha256(&[forged], 1, PacketFraming::AacRaw),
            Err(SupervisorError::FfprobeInvalid)
        ));

        let mut truncated = packet(0, b"payload");
        truncated.size = "8".to_owned();
        assert!(matches!(
            packet_payload_sha256(&[truncated], 1, PacketFraming::AacRaw),
            Err(SupervisorError::FfprobeInvalid)
        ));
    }

    #[test]
    fn output_streams_are_sorted_and_must_be_contiguous_and_complete() {
        let sorted = sorted_output_streams(vec![stream(1), stream(0)], 2).expect("sorted");
        assert_eq!(
            sorted.iter().map(|value| value.index).collect::<Vec<_>>(),
            [0, 1]
        );
        assert!(matches!(
            sorted_output_streams(vec![stream(0), stream(2)], 2),
            Err(SupervisorError::FfprobeInvalid)
        ));
        assert!(matches!(
            sorted_output_streams(vec![stream(0)], 2),
            Err(SupervisorError::FfprobeInvalid)
        ));
    }

    fn cleanup_test_root() -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .expect("repository root")
            .join(".fforager-artifacts/test-runs/ffmpeg-supervisor-cleanup");
        std::fs::create_dir_all(&root).expect("create governed test root");
        root.canonicalize().expect("canonical governed test root")
    }

    #[cfg(windows)]
    fn cleanup_test_process(root: &Path) -> (PathBuf, Vec<String>, Vec<(String, String)>) {
        let system_root = std::env::var("SYSTEMROOT").expect("Windows SYSTEMROOT");
        let executable =
            Path::new(&system_root).join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let mut environment = Vec::new();
        append_windows_system_environment(&mut environment, &system_root)
            .expect("ordinary Windows system root");
        append_windows_writable_environment(
            &mut environment,
            root.to_str().expect("Unicode cleanup root"),
        );
        (
            executable,
            vec![
                "-NoProfile".to_owned(),
                "-NonInteractive".to_owned(),
                "-Command".to_owned(),
                "Start-Sleep -Seconds 30".to_owned(),
            ],
            environment,
        )
    }

    #[cfg(target_os = "linux")]
    fn cleanup_test_process(_root: &Path) -> (PathBuf, Vec<String>, Vec<(String, String)>) {
        (
            PathBuf::from("/usr/bin/dash"),
            vec!["-c".to_owned(), "sleep 30".to_owned()],
            vec![
                ("LANG".to_owned(), "C".to_owned()),
                ("LC_ALL".to_owned(), "C".to_owned()),
            ],
        )
    }

    #[test]
    fn execution_deadline_precedes_later_cancellation_and_forces_one_bounded_reap() {
        let root = cleanup_test_root();
        let (executable, default_arguments, environment) = cleanup_test_process(&root);
        let arguments = if cfg!(target_os = "linux") {
            vec![
                "-c".to_owned(),
                "trap '' TERM; exec /bin/sleep 30".to_owned(),
            ]
        } else {
            default_arguments
        };
        let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates")
            .join("fforager-contracts/testdata/ffmpeg-supervision-v1.0.json");
        let fixture: serde_json::Value =
            serde_json::from_slice(&std::fs::read(fixture_path).expect("fixture bytes"))
                .expect("fixture JSON");
        let mut request: FfmpegSupervisionRequestV1 =
            serde_json::from_value(fixture["request"].clone()).expect("request");
        request.limits.execution_timeout_millis = 75;
        request.limits.graceful_stop_timeout_millis = 75;
        request.limits.reap_timeout_millis = 2_000;
        let started = Instant::now();
        let cancellation = CancellationProfile::After(Duration::from_secs(5));
        let cancellation_deadline = cancellation_deadline(started, cancellation);
        let mut child = crate::platform::spawn(&executable, &arguments, &environment, &root)
            .expect("controlled hung child");
        let mut lifecycle = FfmpegLifecycleV2::new(
            MachineInstanceId::new(0x7e11).expect("instance"),
            FfmpegLifecycleLimitsV2::new(2, 2, 2, 2).expect("limits"),
            64,
        );
        lifecycle.start().expect("spawn pending");
        complete_lifecycle(&mut lifecycle, FfmpegEffectOutcomeV2::Spawned).expect("spawned child");
        let mut observations = Vec::new();
        let result = wait_or_cancel(
            &mut child,
            &request,
            cancellation,
            cancellation_deadline,
            &mut observations,
            &mut lifecycle,
            started,
        );
        assert!(
            matches!(result, Err(SupervisorError::ExecutionTimedOut)),
            "unexpected force-race result: {result:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(3));
        let exit = child
            .wait_timeout(Duration::ZERO)
            .expect("cached direct wait")
            .expect("direct child reaped once");
        assert!(
            exit.forced_by_supervisor,
            "hung child must be force-terminated"
        );
        assert!(child.declared_scope_empty().expect("owned scope empty"));
    }

    #[test]
    fn explicit_cancellation_wins_an_exact_execution_deadline_tie() {
        let deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(
            execution_stop_deadline(
                deadline,
                Some(deadline),
                CancellationProfile::After(Duration::from_secs(1)),
            ),
            (deadline, ExecutionStopCause::Cancellation)
        );
    }

    #[test]
    fn supervisor_force_race_records_already_exited_without_false_forced_provenance() {
        let mut lifecycle = FfmpegLifecycleV2::new(
            MachineInstanceId::new(0x7e12).expect("instance"),
            FfmpegLifecycleLimitsV2::new(2, 2, 2, 2).expect("limits"),
            64,
        );
        lifecycle.start().expect("spawn pending");
        complete_lifecycle(&mut lifecycle, FfmpegEffectOutcomeV2::Spawned).expect("spawned child");
        lifecycle
            .request_cancellation()
            .expect("termination intent");
        complete_lifecycle(
            &mut lifecycle,
            FfmpegEffectOutcomeV2::GracefulStopUnsupported,
        )
        .expect("force pending");
        assert!(
            !complete_force_termination(&mut lifecycle, ForceTerminationOutcome::AlreadyEmpty,)
                .expect("record already-empty race")
        );
        assert!(lifecycle.trace().iter().any(|transition| matches!(
            transition.action,
            FfmpegLifecycleActionV2::EffectCompleted {
                outcome: FfmpegEffectOutcomeV2::ForcedKillAlreadyExited,
                ..
            }
        )));
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the cleanup regression owns a complete real child, both drains, lifecycle, and paired broker assertions"
    )]
    fn prove_cleanup_releases_capacity(cancellation_already_requested: bool, owner_value: u64) {
        let root = cleanup_test_root();
        let (executable, arguments, environment) = cleanup_test_process(&root);
        let claim = ResourceVector {
            ffmpeg_processes: 1,
            open_handles: 4,
            ..ResourceVector::default()
        };
        let resources =
            OwnedResourceBroker::from_contract(&ResourceContractV1::new(claim, 1, 1, 0, 0))
                .expect("capacity-one resource broker");
        let bytes = OwnedByteCreditBroker::from_contract(&ByteCreditContractV1::new(2, 1))
            .expect("capacity-one pipe broker");
        let owner = OwnerId(owner_value);
        let mut resource_lease = Some(match resources.request(owner, claim).expect("admission") {
            OwnedAdmission::Granted(lease) => lease,
            OwnedAdmission::Queued(_) => panic!("capacity-one first request must be granted"),
        });
        let mut pipe_lease = Some(
            bytes
                .claim(owner, ByteCreditStage::FfmpegPipe, 2)
                .expect("capacity-one pipe claim"),
        );
        assert_eq!(resources.active_grant_count(), 1);
        assert_eq!(resources.in_use(), claim);
        assert_eq!(bytes.global_occupancy().expect("occupied bytes"), (1, 2));
        let mut lifecycle = FfmpegLifecycleV2::new(
            MachineInstanceId::new(owner_value).expect("nonzero instance"),
            FfmpegLifecycleLimitsV2::new(2, 2, 2, 2).expect("bounded limits"),
            64,
        );
        lifecycle.start().expect("spawn pending");
        let mut child = crate::platform::spawn(&executable, &arguments, &environment, &root)
            .expect("controlled cleanup child");
        complete_lifecycle(&mut lifecycle, FfmpegEffectOutcomeV2::Spawned)
            .expect("record spawned child");
        if cancellation_already_requested {
            lifecycle
                .request_cancellation()
                .expect("sticky cancellation request");
        }
        let (stdout, stderr) = child.take_pipes().expect("owned child pipes");
        assert_eq!(resources.active_grant_count(), 1);
        assert_eq!(bytes.global_occupancy().expect("running bytes"), (1, 2));
        let progress_worker = std::thread::spawn(move || {
            drain_progress_supervised(
                stdout,
                ProgressLimits {
                    max_records: 4,
                    max_total_bytes: 4 * 1024,
                    max_record_bytes: 1024,
                    max_field_bytes: 256,
                    max_parser_steps: 16 * 1024,
                },
                ProgressRuntimeLimits {
                    allocation_bytes: 16 * 1024,
                    cadence_timeout: Duration::from_secs(5),
                    consumer_stall_timeout: Duration::from_secs(5),
                },
            )
        });
        let diagnostic_worker = std::thread::spawn(move || {
            drain_diagnostics(
                stderr,
                BoundedDrainLimits {
                    max_total_bytes: 4 * 1024,
                    tail_bytes: 1024,
                },
            )
        });

        let _expected_failure = abort_spawned_execution(
            &mut child,
            &mut lifecycle,
            &mut pipe_lease,
            &mut resource_lease,
            Duration::from_secs(3),
            Some(PipeWorkers {
                progress: progress_worker,
                diagnostics: diagnostic_worker,
                stop: Arc::new(AtomicBool::new(false)),
            }),
            SupervisorError::ExecutionTimedOut,
        );
        assert!(child.declared_scope_empty().expect("scope query"));
        assert_eq!(resources.active_grant_count(), 0);
        assert_eq!(resources.in_use(), ResourceVector::default());
        assert_eq!(bytes.global_occupancy().expect("byte occupancy"), (0, 0));

        let next_owner = OwnerId(owner_value + 10);
        let reacquired = match resources
            .request(next_owner, claim)
            .expect("reacquire resources after terminal cleanup")
        {
            OwnedAdmission::Granted(lease) => lease,
            OwnedAdmission::Queued(_) => panic!("released capacity must be immediately reusable"),
        };
        let pipe_reacquired = bytes
            .claim(next_owner, ByteCreditStage::FfmpegPipe, 2)
            .expect("reacquire pipe capacity after terminal cleanup");
        reacquired.release().expect("release reacquired resources");
        pipe_reacquired.release().expect("release reacquired bytes");
        resources.verify().expect("resource invariants");
        bytes.verify().expect("byte-credit invariants");
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the real escaped-descendant regression keeps PID ownership, bounded cleanup, and capacity poison proof in one test lifetime"
    )]
    fn setsid_pipe_escape_fails_bounded_and_poisons_capacity() {
        let root = cleanup_test_root().join(format!("setsid-pipes-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("escape root");
        let pid_path = root.join("escape.pid");
        let ready_path = root.join("escape.ready");
        for marker in [&pid_path, &ready_path] {
            if let Err(failure) = std::fs::remove_file(marker)
                && failure.kind() != std::io::ErrorKind::NotFound
            {
                panic!("remove stale escape marker: {failure}");
            }
        }
        let arguments = vec![
            "-c".to_owned(),
            "exec 3>&1 4>&2; /usr/bin/setsid /bin/sh -c 'echo $$ > escape.ready || exit 25; exec /bin/sleep 30' >&3 2>&4 & escaped=$!; echo \"$escaped\" > escape.pid || { /bin/kill -KILL \"$escaped\" 2>/dev/null; wait \"$escaped\" 2>/dev/null; exit 24; }; exit 0"
                .to_owned(),
        ];
        let environment = vec![
            ("LANG".to_owned(), "C".to_owned()),
            ("LC_ALL".to_owned(), "C".to_owned()),
        ];
        let claim = ResourceVector {
            ffmpeg_processes: 1,
            open_handles: 4,
            ..ResourceVector::default()
        };
        let resources =
            OwnedResourceBroker::from_contract(&ResourceContractV1::new(claim, 1, 1, 0, 0))
                .expect("resources");
        let bytes =
            OwnedByteCreditBroker::from_contract(&ByteCreditContractV1::new(2, 1)).expect("bytes");
        let owner = OwnerId(0x51de);
        let mut resource_lease = Some(match resources.request(owner, claim).expect("admission") {
            OwnedAdmission::Granted(lease) => lease,
            OwnedAdmission::Queued(_) => panic!("first claim must be granted"),
        });
        let mut pipe_lease = Some(
            bytes
                .claim(owner, ByteCreditStage::FfmpegPipe, 2)
                .expect("pipe claim"),
        );
        let mut lifecycle = FfmpegLifecycleV2::new(
            MachineInstanceId::new(owner.0).expect("instance"),
            FfmpegLifecycleLimitsV2::new(2, 2, 2, 2).expect("limits"),
            64,
        );
        lifecycle.start().expect("spawn pending");
        let mut child =
            crate::platform::spawn(Path::new("/usr/bin/dash"), &arguments, &environment, &root)
                .expect("spawn escape fixture");
        complete_lifecycle(&mut lifecycle, FfmpegEffectOutcomeV2::Spawned).expect("spawned");
        let (stdout, stderr) = child.take_pipes().expect("pipes");
        configure_cancellable_pipe(&stdout).expect("stdout nonblocking");
        configure_cancellable_pipe(&stderr).expect("stderr nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let progress_stop = Arc::clone(&stop);
        let progress = std::thread::spawn(move || {
            drain_progress_supervised(
                CancellablePipeReader::new(stdout, progress_stop),
                ProgressLimits {
                    max_records: 4,
                    max_total_bytes: 4096,
                    max_record_bytes: 1024,
                    max_field_bytes: 256,
                    max_parser_steps: 16_384,
                },
                ProgressRuntimeLimits {
                    allocation_bytes: 16_384,
                    cadence_timeout: Duration::from_millis(100),
                    consumer_stall_timeout: Duration::from_millis(100),
                },
            )
        });
        let diagnostic_stop = Arc::clone(&stop);
        let diagnostics = std::thread::spawn(move || {
            drain_diagnostics(
                CancellablePipeReader::new(stderr, diagnostic_stop),
                BoundedDrainLimits {
                    max_total_bytes: 4096,
                    tail_bytes: 1024,
                },
            )
        });
        let direct = child
            .wait_timeout(Duration::from_secs(10))
            .expect("direct wait")
            .expect("direct exit");
        assert_eq!(direct.exit_code, Some(0));
        let escaped_pid = std::fs::read_to_string(&pid_path)
            .expect("escape pid")
            .trim()
            .parse::<i32>()
            .expect("numeric pid");
        let escaped_pid_text = escaped_pid.to_string();
        let ready_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let ready_matches = std::fs::read_to_string(&ready_path)
                .is_ok_and(|value| value.trim() == escaped_pid_text);
            match child.declared_scope_empty() {
                Ok(true) if ready_matches => break,
                Ok(_) if Instant::now() < ready_deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(_) => {
                    crate::platform::terminate_owned_test_process(
                        escaped_pid,
                        Duration::from_secs(2),
                    )
                    .expect("bounded cleanup after readiness timeout");
                    panic!("escaped descendant did not become ready within the bounded deadline");
                }
                Err(failure) => {
                    crate::platform::terminate_owned_test_process(
                        escaped_pid,
                        Duration::from_secs(2),
                    )
                    .expect("bounded cleanup after group-query failure");
                    panic!("declared group query failed: {failure}");
                }
            }
        }
        let started = Instant::now();
        let failure = abort_spawned_execution(
            &mut child,
            &mut lifecycle,
            &mut pipe_lease,
            &mut resource_lease,
            Duration::from_secs(1),
            Some(PipeWorkers {
                progress,
                diagnostics,
                stop,
            }),
            SupervisorError::ContainmentUnproven,
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(matches!(failure, SupervisorError::CleanupFailed { .. }));
        assert!(resource_lease.is_none());
        assert!(pipe_lease.is_none());
        assert_eq!(
            resources.active_grant_count(),
            1,
            "capacity must remain poisoned"
        );
        assert_eq!(bytes.global_occupancy().expect("occupancy"), (1, 2));
        crate::platform::terminate_owned_test_process(escaped_pid, Duration::from_secs(2))
            .expect("escaped descendant absent after bounded cleanup");
    }

    #[test]
    fn post_spawn_error_reaps_before_capacity_one_reacquisition() {
        prove_cleanup_releases_capacity(false, 71);
    }

    #[test]
    fn cancellation_reaps_before_capacity_one_reacquisition() {
        prove_cleanup_releases_capacity(true, 72);
    }

    #[test]
    fn pipe_admission_failure_atomically_rolls_back_resource_capacity() {
        let claim = ResourceVector {
            ffmpeg_processes: 1,
            ..ResourceVector::default()
        };
        let resources =
            OwnedResourceBroker::from_contract(&ResourceContractV1::new(claim, 1, 1, 0, 0))
                .expect("capacity-one resource broker");
        let bytes = OwnedByteCreditBroker::from_contract(&ByteCreditContractV1::new(1, 1))
            .expect("capacity-one pipe broker");
        let blocker = bytes
            .claim(OwnerId(80), ByteCreditStage::FfmpegPipe, 1)
            .expect("occupy all pipe capacity");

        assert!(matches!(
            acquire_exact_adapter_capacity(&resources, &bytes, OwnerId(81), claim, 1),
            Err(SupervisorError::Resource(_))
        ));
        assert_eq!(resources.active_grant_count(), 0);
        assert_eq!(resources.in_use(), ResourceVector::default());
        assert_eq!(bytes.global_occupancy().expect("blocked occupancy"), (1, 1));

        blocker.release().expect("release blocking pipe claim");
        let (resource_lease, pipe_lease) =
            acquire_exact_adapter_capacity(&resources, &bytes, OwnerId(81), claim, 1)
                .expect("atomic capacity is reusable after rollback");
        assert_eq!(resources.active_grant_count(), 1);
        assert_eq!(
            bytes.global_occupancy().expect("reacquired occupancy"),
            (1, 1)
        );
        resource_lease.release().expect("release resource lease");
        pipe_lease.release().expect("release pipe lease");
        resources.verify().expect("resource invariants");
        bytes.verify().expect("byte-credit invariants");
    }

    #[test]
    fn governed_input_pin_blocks_or_detects_content_mutation_and_path_swap() {
        let root = cleanup_test_root().join("pin-mutation");
        std::fs::create_dir_all(&root).expect("create pin root");
        let path = root.join("input.bin");
        let moved = root.join("moved.bin");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&moved);
        std::fs::write(&path, b"original-governed-content").expect("write original input");
        let pin = pin_governed_file(&path, 1024, Duration::from_secs(1)).expect("pin direct input");

        #[cfg(windows)]
        {
            assert!(
                std::fs::write(&path, b"mutated-content").is_err(),
                "non-share-write pin must block in-place mutation"
            );
            assert!(
                std::fs::rename(&path, &moved).is_err(),
                "non-share-delete pin must block pathname substitution"
            );
            pin.verify_path(&path).expect("unchanged Windows pin");
        }

        #[cfg(target_os = "linux")]
        {
            std::fs::rename(&path, &moved).expect("Linux permits rename of retained fd");
            std::fs::write(&path, b"substituted-content").expect("replace pathname");
            assert!(
                pin.verify_path(&path).is_err(),
                "retained Linux fd plus recheck must detect pathname substitution"
            );
        }
    }

    #[test]
    fn governed_input_pin_rejects_symlink_or_reparse_input() {
        let root = cleanup_test_root().join("pin-link");
        std::fs::create_dir_all(&root).expect("create link root");
        let target = root.join("target.bin");
        let link = root.join("link.bin");
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_file(&target);
        std::fs::write(&target, b"target").expect("write link target");

        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&target, &link)
            .expect("create unprivileged Windows file symlink for reparse rejection");
        #[cfg(target_os = "linux")]
        std::os::unix::fs::symlink(&target, &link).expect("create Unix symlink");

        assert!(
            pin_governed_file(&link, 1024, Duration::from_secs(1)).is_err(),
            "no-follow media pin must reject symlink/reparse input"
        );
    }

    #[test]
    fn governed_file_pin_rejects_oversized_input_or_output_before_hashing() {
        let root = cleanup_test_root().join("pin-oversized");
        std::fs::create_dir_all(&root).expect("create oversized root");
        let path = root.join("oversized.bin");
        std::fs::write(&path, b"ninebytes").expect("write bounded counterexample");
        assert!(
            pin_governed_file(&path, 8, Duration::from_secs(1)).is_err(),
            "the retained file pin must enforce its byte bound before accepting content"
        );
    }
}
