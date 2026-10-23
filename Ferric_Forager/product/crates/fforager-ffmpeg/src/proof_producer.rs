//! Executing, environment-configured producer for the WP-FF-010 platform report.
//!
//! This is prerequisite proof tooling, not a shipped runtime entrypoint. It
//! builds its request from independently observed tool and fixture identities,
//! executes the public supervisor, and separately attacks the OS process scope.

#[cfg(windows)]
use crate::platform::{
    PARENT_DEATH_HELPER_TEST, PARENT_DEATH_RECEIPT_ENV, observe_kill_on_job_close_parent_death,
    run_handle_list_fault_probe,
};
use crate::{
    bounded_io::drain_required_bytes,
    identity::{ExecutableCapabilityObservation, observe_executable_capabilities},
    platform::{
        ExecutablePinExpectation, ExitObservation, PlatformChild, file_identity, spawn_verified,
    },
    report::{
        ContainmentObservationsV1, DiagnosticLossEvidenceV1, DirectWaitReceiptEvidenceV1,
        FFMPEG_PLATFORM_PROOF_SCHEMA_ID, FixtureToolProofIdentityV1, FixtureToolRoleV1,
        ForcedWaitReceiptEvidenceV1, PhaseDeadlineObservationsV2, PlatformBehaviorV1,
        PlatformProofReportV2, ProducerPhaseLimitsV2, ProgressObservationsV1, ProofPlatform,
        ToolProofIdentityV1,
    },
    supervisor::{
        CancellationProfile, FfmpegSupervisor, SupervisorError, SupervisorTrustedContext,
        TrustedDirectoryPin, validate_existing_output_with_ffprobe,
    },
};
use fforager_contracts::{
    BYTE_CREDIT_SCHEMA_ID, ByteCreditContractV1, ByteCreditStage, FFMPEG_SUPERVISION_SCHEMA_ID,
    FFMPEG_SUPERVISION_VERSION, FfmpegCapabilityBindingV1, FfmpegContainmentEvidenceV1,
    FfmpegDiagnosticChannelV1, FfmpegDirectChildReapV1, FfmpegEnvironmentBindingV1,
    FfmpegEnvironmentPolicyV1, FfmpegExecutableIdentityV1, FfmpegExecutableKindV1,
    FfmpegHostArchitectureV1, FfmpegHostIdentityV1, FfmpegHostOperatingSystemV1,
    FfmpegInputDemuxerV1, FfmpegInputFileV1, FfmpegInputMechanismV1, FfmpegIoPolicyV1,
    FfmpegLifecycleObservationV1, FfmpegLifecycleStateV1, FfmpegOperationPlanV1,
    FfmpegOutputValidationV1, FfmpegProgressChannelV1, FfmpegProtocolV1, FfmpegResourceReferenceV1,
    FfmpegStreamCopyPlanV1, FfmpegStreamKindV1, FfmpegStreamMapV1, FfmpegSupervisionLimitsV1,
    FfmpegSupervisionRequestV1, FfmpegToolchainIdentityV1, JobId, RESOURCE_VECTOR_SCHEMA_ID,
    RequestId, ResourceContractV1, ResourceVector, WindowsActiveProcessQueryV1,
};
use fforager_core::resource::{OwnedByteCreditBroker, OwnedResourceBroker};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    env, fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const ENV_FFMPEG: &str = "FFORAGER_WP010_FFMPEG";
const ENV_FFPROBE: &str = "FFORAGER_WP010_FFPROBE";
const ENV_FIXTURE_ROOT: &str = "FFORAGER_WP010_FIXTURE_ROOT";
const ENV_REPORT_OUTPUT: &str = "FFORAGER_WP010_REPORT_OUTPUT";
const ENV_FAKE_CHILD: &str = "FFORAGER_WP010_FAKE_CHILD";
const ENV_SOURCE_COMMIT: &str = "FFORAGER_WP010_SOURCE_COMMIT";
const ENV_SOURCE_DIRTY: &str = "FFORAGER_WP010_SOURCE_DIRTY";
const ENV_HOST_KERNEL: &str = "FFORAGER_WP010_HOST_KERNEL";
#[cfg(target_os = "linux")]
const ENV_SETSID: &str = "FFORAGER_WP010_SETSID";
const CAPTURE_LIMIT: u64 = 32 * 1024 * 1024;
const PROCESS_TIMEOUT: Duration = Duration::from_secs(30);
const FIXTURE_PROBE_TIMEOUT: Duration = Duration::from_mins(1);
const IDENTITY_PROBE_TIMEOUT: Duration = Duration::from_mins(3);
const SCOPE_SETTLEMENT_TIMEOUT: Duration = Duration::from_secs(5);
const NEGATIVE_CASES_TIMEOUT: Duration = Duration::from_mins(5);

/// Bounded failure from the executing proof producer.
#[derive(Debug)]
pub struct ProofProducerError(String);

impl std::fmt::Display for ProofProducerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ProofProducerError {}

impl From<io::Error> for ProofProducerError {
    fn from(error: io::Error) -> Self {
        Self(error.to_string())
    }
}

fn error(context: &str, detail: impl std::fmt::Display) -> ProofProducerError {
    ProofProducerError(format!("{context}: {detail}"))
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn require_phase_within(
    label: &str,
    observed_millis: u64,
    limit: Duration,
) -> Result<(), ProofProducerError> {
    if observed_millis > duration_millis(limit) {
        return Err(error(
            label,
            format_args!(
                "phase elapsed {observed_millis}ms beyond declared {}ms ceiling",
                duration_millis(limit)
            ),
        ));
    }
    Ok(())
}

fn lifecycle_delta_millis(
    lifecycle: &[FfmpegLifecycleObservationV1],
    start: FfmpegLifecycleStateV1,
    end: FfmpegLifecycleStateV1,
) -> Result<u64, ProofProducerError> {
    let start_millis = lifecycle
        .iter()
        .find(|observation| observation.state == start)
        .map(|observation| observation.monotonic_millis)
        .ok_or_else(|| error("phase timeline", format_args!("missing {start:?}")))?;
    let end_millis = lifecycle
        .iter()
        .find(|observation| observation.state == end)
        .map(|observation| observation.monotonic_millis)
        .ok_or_else(|| error("phase timeline", format_args!("missing {end:?}")))?;
    end_millis
        .checked_sub(start_millis)
        .ok_or_else(|| error("phase timeline", "monotonic phase order reversed"))
}

fn validate_phase_deadlines(
    observed: &PhaseDeadlineObservationsV2,
    producer: &ProducerPhaseLimitsV2,
    request: FfmpegSupervisionLimitsV1,
    forced_lifecycle: &[FfmpegLifecycleObservationV1],
) -> Result<(), ProofProducerError> {
    let checks = [
        (
            "fixture probes",
            observed.fixture_probe_millis,
            producer.fixture_probe_timeout_millis,
        ),
        (
            "identity probes",
            observed.identity_probe_millis,
            producer.identity_probe_timeout_millis,
        ),
        (
            "startup",
            observed.startup_millis,
            request.startup_timeout_millis,
        ),
        (
            "execution",
            observed.execution_millis,
            request.execution_timeout_millis,
        ),
        (
            "validation",
            observed.validation_millis,
            request.ffprobe_timeout_millis,
        ),
        (
            "graceful stop",
            observed.graceful_stop_millis,
            request.graceful_stop_timeout_millis,
        ),
        (
            "forced kill",
            observed.forced_kill_millis,
            request.forced_kill_timeout_millis,
        ),
        ("reap", observed.reap_millis, request.reap_timeout_millis),
        (
            "scope settlement",
            observed.scope_settlement_millis,
            producer.scope_settlement_timeout_millis,
        ),
        (
            "negative cases",
            observed.negative_cases_millis,
            producer.negative_cases_timeout_millis,
        ),
    ];
    if producer.fixture_probe_timeout_millis == 0
        || producer.identity_probe_timeout_millis == 0
        || producer.scope_settlement_timeout_millis == 0
        || producer.negative_cases_timeout_millis == 0
    {
        return Err(error("producer phase limits", "zero phase ceiling"));
    }
    let measured_forced_sum = observed
        .forced_kill_millis
        .checked_add(observed.reap_millis)
        .and_then(|value| value.checked_add(observed.scope_settlement_millis))
        .ok_or_else(|| error("forced cleanup", "measured phase sum overflow"))?;
    let forced_cleanup_limit = request
        .forced_kill_timeout_millis
        .checked_add(request.reap_timeout_millis)
        .and_then(|value| value.checked_add(producer.scope_settlement_timeout_millis))
        .ok_or_else(|| error("forced cleanup", "phase ceiling sum overflow"))?;
    let forced_cleanup_timeline = lifecycle_delta_millis(
        forced_lifecycle,
        FfmpegLifecycleStateV1::ForcedKillRequested,
        FfmpegLifecycleStateV1::Reaped,
    )?;
    if forced_cleanup_timeline != measured_forced_sum
        || forced_cleanup_timeline > forced_cleanup_limit
    {
        return Err(error(
            "forced cleanup",
            "settlement duration diverged from its lifecycle or combined ceiling",
        ));
    }
    for (label, actual, limit) in checks {
        if actual > limit {
            return Err(error(
                label,
                format_args!("phase elapsed {actual}ms beyond declared {limit}ms ceiling"),
            ));
        }
    }
    Ok(())
}

/// Execute the same proof producer on Windows x86-64 or Linux-native x86-64.
///
/// Required environment variables name absolute `ffmpeg`, `ffprobe`, fixture
/// root, fake-child, and report-output paths. `FFORAGER_WP010_SETSID` is also
/// required on Linux. Fixture and output paths must be below this worktree's
/// `.fforager-artifacts` entry. The fixture root must contain
/// `input/audio.aac` and `input/video.h264`.
///
/// # Errors
///
/// Fails closed on an unclean source, an unbound path, a failed real tool or
/// platform observation, a contract mismatch, or a report write outside the
/// artifact root.
#[allow(
    clippy::too_many_lines,
    reason = "the proof producer intentionally keeps acquisition, execution, and report binding in one auditable path"
)]
pub fn produce_platform_proof_from_environment() -> Result<PlatformProofReportV2, ProofProducerError>
{
    let repo_root = repository_root()?;
    let artifact_root = repo_root.join(".fforager-artifacts");
    let ffmpeg_path = absolute_env_path(ENV_FFMPEG)?;
    let ffprobe_path = absolute_env_path(ENV_FFPROBE)?;
    let fixture_root = artifact_path(ENV_FIXTURE_ROOT, &artifact_root)?;
    let report_output = artifact_path(ENV_REPORT_OUTPUT, &artifact_root)?;
    let fake_child = absolute_env_path(ENV_FAKE_CHILD)?;
    require_file(&ffmpeg_path, ENV_FFMPEG)?;
    require_file(&ffprobe_path, ENV_FFPROBE)?;
    require_file(&fake_child, ENV_FAKE_CHILD)?;
    require_file(&fixture_root.join("input/audio.aac"), "AAC fixture")?;
    require_file(&fixture_root.join("input/video.h264"), "H264 fixture")?;
    fs::create_dir_all(fixture_root.join("output"))?;
    fs::create_dir_all(fixture_root.join("tmp"))?;
    let working_directory_pin = TrustedDirectoryPin::acquire(&fixture_root)
        .map_err(|failure| error("trusted working directory", failure))?;
    let job_temporary_directory_pin = TrustedDirectoryPin::acquire(&fixture_root.join("tmp"))
        .map_err(|failure| error("trusted temporary directory", failure))?;
    if fixture_root.join("output/merged.mp4").exists() {
        fs::remove_file(fixture_root.join("output/merged.mp4"))?;
    }

    let source_commit =
        env::var(ENV_SOURCE_COMMIT).map_err(|failure| error(ENV_SOURCE_COMMIT, failure))?;
    if source_commit.len() != 40 || !source_commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(error("Git HEAD", "expected a full hexadecimal commit"));
    }
    let source_dirty = match env::var(ENV_SOURCE_DIRTY).as_deref() {
        Ok("false") => false,
        Ok("true") => true,
        Ok(value) => {
            return Err(error(
                ENV_SOURCE_DIRTY,
                format_args!("invalid value {value:?}"),
            ));
        }
        Err(failure) => return Err(error(ENV_SOURCE_DIRTY, failure)),
    };
    if source_dirty {
        return Err(error(
            "Git source",
            "proof requires a clean committed worktree",
        ));
    }

    let host = host_identity()?;
    let environment = trusted_environment(&fixture_root)?;
    let fixture_probe_started = Instant::now();
    let fixture_tools = observe_fixture_tools(&fake_child, &environment, &fixture_root)?;
    let fixture_probe_millis = elapsed_millis(fixture_probe_started);
    require_phase_within(
        "fixture probes",
        fixture_probe_millis,
        FIXTURE_PROBE_TIMEOUT,
    )?;
    let identity_probe_started = Instant::now();
    let ffmpeg_capability = observe_executable_capabilities(
        &ffmpeg_path,
        FfmpegExecutableKindV1::Ffmpeg,
        &environment,
        &fixture_root,
        PROCESS_TIMEOUT,
    )
    .map_err(|failure| error("observe ffmpeg", failure))?;
    let ffprobe_capability = observe_executable_capabilities(
        &ffprobe_path,
        FfmpegExecutableKindV1::Ffprobe,
        &environment,
        &fixture_root,
        PROCESS_TIMEOUT,
    )
    .map_err(|failure| error("observe ffprobe", failure))?;

    let audio_payload = measure_source_payload(
        &ffprobe_capability,
        "aac",
        "input/audio.aac",
        PacketFraming::AacAdts,
        &environment,
        &fixture_root,
    )?;
    let video_payload = measure_source_payload(
        &ffprobe_capability,
        "h264",
        "input/video.h264",
        PacketFraming::H264AnnexB,
        &environment,
        &fixture_root,
    )?;
    let identity_probe_millis = elapsed_millis(identity_probe_started);
    require_phase_within(
        "identity probes",
        identity_probe_millis,
        IDENTITY_PROBE_TIMEOUT,
    )?;
    let limits = proof_limits();
    let request = build_request(
        host,
        &fixture_root,
        &ffmpeg_capability,
        &ffprobe_capability,
        audio_payload,
        video_payload,
        limits,
    )?;
    let invocation = request
        .validate_for_invocation()
        .map_err(|failure| error("validate measured request", failure))?;

    let resource_contract = ResourceContractV1::new(request.resources.claim, 1, 1, 0, 0);
    let resource_broker = OwnedResourceBroker::from_contract(&resource_contract)
        .map_err(|failure| error("resource broker", format_args!("{failure:?}")))?;
    let pipe_bytes = request
        .resources
        .progress_pipe_bytes
        .checked_add(request.resources.stderr_pipe_bytes)
        .ok_or_else(|| error("pipe capacity", "overflow"))?;
    let byte_contract = ByteCreditContractV1::new(pipe_bytes, 1);
    let byte_credit_broker = OwnedByteCreditBroker::from_contract(&byte_contract)
        .map_err(|failure| error("byte-credit broker", format_args!("{failure:?}")))?;
    let system_root = env::var("SYSTEMROOT").ok();
    let trusted = SupervisorTrustedContext {
        source_commit: &source_commit,
        host_system_root: system_root.as_deref(),
        working_directory: &working_directory_pin,
        job_temporary_directory: &job_temporary_directory_pin,
        resource_broker: &resource_broker,
        byte_credit_broker: &byte_credit_broker,
        ffmpeg_capability: &ffmpeg_capability,
        ffprobe_capability: &ffprobe_capability,
    };
    let execution = FfmpegSupervisor
        .execute(&request, &invocation, &trusted, CancellationProfile::Never)
        .map_err(|failure| error("real stream-copy supervision", failure))?;
    let negative_cases_started = Instant::now();
    observe_real_output_rejections(
        &request,
        &trusted,
        &environment,
        &fixture_root,
        &ffmpeg_capability,
    )?;

    let forced = observe_forced_real_ffmpeg(&ffmpeg_capability, &environment, &fixture_root)?;
    let platform_evidence = observe_platform_counterexample(
        &ffmpeg_capability,
        &fixture_tools,
        &environment,
        &fixture_root,
    )?;
    let negative_cases_millis = elapsed_millis(negative_cases_started);
    require_phase_within(
        "negative cases",
        negative_cases_millis,
        NEGATIVE_CASES_TIMEOUT,
    )?;

    let (direct_wait_evidence, successful_exit_observed) = match &execution.report.direct_child {
        FfmpegDirectChildReapV1::Reaped {
            exit_code,
            terminated_by_signal_or_exception,
            wait_receipt_sha256,
        } => {
            let successful = *exit_code == Some(0) && !terminated_by_signal_or_exception;
            (
                DirectWaitReceiptEvidenceV1 {
                    exit_code: *exit_code,
                    signal: None,
                    windows_opaque_status: None,
                    forced_by_supervisor: false,
                    supervision_wait_receipt_sha256: wait_receipt_sha256.clone(),
                },
                successful,
            )
        }
        _ => return Err(error("success direct wait", "missing reaped direct child")),
    };
    let FfmpegOutputValidationV1::Validated {
        facts: output_facts,
        ..
    } = &execution.report.output_validation
    else {
        return Err(error("success output", "ffprobe facts were not validated"));
    };
    let (windows_attached, windows_kill_on_close, windows_active_zero, unix_group) =
        match &execution.report.containment {
            FfmpegContainmentEvidenceV1::WindowsJob {
                attached_before_execution,
                kill_on_job_close,
                active_process_query: WindowsActiveProcessQueryV1::Zero {},
            } => (
                Some(*attached_before_execution),
                Some(*kill_on_job_close),
                Some(0),
                None,
            ),
            FfmpegContainmentEvidenceV1::UnixProcessGroup {
                declared_nonescaping_members_remaining: 0,
                ..
            } => (None, None, None, Some(true)),
            _ => {
                return Err(error(
                    "success containment",
                    "required zero-scope observation missing",
                ));
            }
        };

    let fixture_contract_path = fixture_root.join("ffmpeg-supervision-v1.0.json");
    require_file(&fixture_contract_path, "canonical fixture contract copy")?;
    let fixture_contract_sha256 = hash_file(&fixture_contract_path)?;
    let canonical_fixture_contract =
        repo_root.join("product/crates/fforager-contracts/testdata/ffmpeg-supervision-v1.0.json");
    require_file(
        &canonical_fixture_contract,
        "product-owned canonical fixture contract",
    )?;
    if fixture_contract_sha256 != hash_file(&canonical_fixture_contract)? {
        return Err(error(
            "fixture contract",
            "artifact fixture copy diverges from product-owned canonical bytes",
        ));
    }
    let FfmpegOperationPlanV1::StreamCopy(executed_plan) = &request.operation;
    let fixture_payloads = executed_plan
        .stream_maps
        .iter()
        .map(|mapping| mapping.source_payload_sha256.clone())
        .collect::<Vec<_>>();
    if fixture_payloads.len() != 2 {
        return Err(error(
            "fixture payload projection",
            "exact AAC/H264 payload pair missing from request",
        ));
    }
    let request_contract_canonical_json = canonical_json(&request)?;
    let operation_plan_canonical_json = canonical_json(&request.operation)?;
    let argument_vector = invocation.bound_runtime().arguments().to_vec();
    let fixture_payload_canonical_json = canonical_json(&fixture_payloads)?;
    let limits_canonical_json = canonical_json(&request.limits)?;
    let lifecycle_timeline_canonical_json = canonical_json(&execution.report.lifecycle)?;
    let direct_wait_receipt_canonical_json = canonical_json(&direct_wait_evidence)?;
    let forced_lifecycle_timeline_canonical_json = canonical_json(&forced.lifecycle)?;
    let forced_lifecycle_timeline_sha256 =
        hash_bytes(forced_lifecycle_timeline_canonical_json.as_bytes());
    let forced_supervision_wait_receipt_sha256 = hash_bytes(
        format!(
            "exit={:?};signal={:?};windows_status_opaque={:?};forced={}",
            forced.exit.exit_code,
            forced.exit.signal,
            forced.exit.windows_status_opaque,
            forced.exit.forced_by_supervisor
        )
        .as_bytes(),
    );
    let forced_wait_evidence = ForcedWaitReceiptEvidenceV1 {
        exit_code: forced.exit.exit_code,
        signal: forced.exit.signal,
        windows_opaque_status: forced.exit.windows_status_opaque,
        forced_by_supervisor: forced.exit.forced_by_supervisor,
        direct_child_reaped: forced.direct_reaped,
        lifecycle_timeline_sha256: forced_lifecycle_timeline_sha256.clone(),
        supervision_wait_receipt_sha256: forced_supervision_wait_receipt_sha256,
    };
    let forced_wait_receipt_canonical_json = canonical_json(&forced_wait_evidence)?;
    let forced_wait_receipt_sha256 = hash_domain(
        "ff.ffmpeg-forced-wait-receipt-canonical-json@1",
        forced_wait_receipt_canonical_json.as_bytes(),
    );
    let output_facts_canonical_json = canonical_json(output_facts)?;
    let containment_observations = containment_observations(
        &forced,
        &platform_evidence,
        windows_attached,
        windows_kill_on_close,
    )?;
    let progress_observations = ProgressObservationsV1 {
        record_count: execution.progress.record_count,
        total_bytes: execution.progress.total_bytes,
        parser_steps: execution.progress.parser_steps,
        saw_terminal: execution.progress.saw_terminal,
    };
    let diagnostic_transcript = execution.diagnostics.complete_transcript().ok_or_else(|| {
        error(
            "diagnostic evidence",
            "complete stderr transcript was not retained",
        )
    })?;
    let execution_millis = lifecycle_delta_millis(
        &execution.report.lifecycle,
        FfmpegLifecycleStateV1::Running,
        FfmpegLifecycleStateV1::Reaped,
    )?;
    let startup_millis = execution
        .report
        .lifecycle
        .iter()
        .find(|observation| observation.state == FfmpegLifecycleStateV1::Running)
        .map(|observation| observation.monotonic_millis)
        .ok_or_else(|| error("phase timeline", "missing running state"))?;
    let validation_millis = lifecycle_delta_millis(
        &execution.report.lifecycle,
        FfmpegLifecycleStateV1::Reaped,
        FfmpegLifecycleStateV1::Validated,
    )?;
    let producer_phase_limits = ProducerPhaseLimitsV2 {
        fixture_probe_timeout_millis: duration_millis(FIXTURE_PROBE_TIMEOUT),
        identity_probe_timeout_millis: duration_millis(IDENTITY_PROBE_TIMEOUT),
        scope_settlement_timeout_millis: duration_millis(SCOPE_SETTLEMENT_TIMEOUT),
        negative_cases_timeout_millis: duration_millis(NEGATIVE_CASES_TIMEOUT),
    };
    let phase_deadlines = PhaseDeadlineObservationsV2 {
        fixture_probe_millis,
        identity_probe_millis,
        startup_millis,
        execution_millis,
        validation_millis,
        graceful_stop_millis: platform_evidence.unix_graceful_millis.unwrap_or(0),
        forced_kill_millis: forced.forced_kill_millis,
        reap_millis: forced.reap_millis,
        scope_settlement_millis: forced.scope_settlement_millis,
        negative_cases_millis,
    };
    validate_phase_deadlines(
        &phase_deadlines,
        &producer_phase_limits,
        request.limits,
        &forced.lifecycle,
    )?;
    let report = PlatformProofReportV2 {
        schema_id: FFMPEG_PLATFORM_PROOF_SCHEMA_ID.to_owned(),
        source_commit,
        source_dirty,
        platform: proof_platform()?,
        host_kernel: env::var(ENV_HOST_KERNEL)
            .map_err(|failure| error(ENV_HOST_KERNEL, failure))?,
        ffmpeg: tool_proof("ffmpeg", &ffmpeg_capability),
        ffprobe: tool_proof("ffprobe", &ffprobe_capability),
        fixture_tools: fixture_tools
            .iter()
            .map(|observation| observation.proof.clone())
            .collect(),
        request_contract_canonical_json,
        request_contract_sha256: invocation.request_contract_sha256().to_owned(),
        operation_plan_canonical_json,
        operation_plan_sha256: invocation.operation_plan_sha256().to_owned(),
        bound_runtime_projection_id: invocation.bound_runtime().projection_id().to_owned(),
        bound_runtime_sha256: invocation.bound_runtime().canonical_sha256().to_owned(),
        argument_vector_sha256: hash_serialized(&argument_vector)?,
        argument_vector,
        fixture_contract_sha256,
        fixture_payload_sha256: hash_bytes(fixture_payload_canonical_json.as_bytes()),
        fixture_payload_canonical_json,
        limits_sha256: hash_bytes(limits_canonical_json.as_bytes()),
        limits_canonical_json,
        lifecycle_timeline_sha256: hash_bytes(lifecycle_timeline_canonical_json.as_bytes()),
        lifecycle_timeline_canonical_json,
        direct_wait_receipt_sha256: direct_wait_evidence.supervision_wait_receipt_sha256,
        direct_wait_receipt_canonical_json,
        forced_lifecycle_timeline_canonical_json,
        forced_lifecycle_timeline_sha256,
        forced_wait_receipt_canonical_json,
        forced_wait_receipt_sha256,
        output_facts_sha256: hash_bytes(output_facts_canonical_json.as_bytes()),
        output_facts_canonical_json,
        producer_phase_limits,
        phase_deadlines,
        diagnostic_total_bytes: execution.diagnostics.total_bytes,
        diagnostic_tail_hex: hex_bytes(diagnostic_transcript),
        diagnostic_tail_sha256: hash_bytes(diagnostic_transcript),
        diagnostic_loss: DiagnosticLossEvidenceV1::None,
        progress_transcript_hex: hex_bytes(&execution.progress.raw_transcript),
        progress_transcript_sha256: hash_bytes(&execution.progress.raw_transcript),
        progress_observations,
        containment_observations,
        behavior: PlatformBehaviorV1 {
            direct_child_wait_observed: successful_exit_observed && forced.direct_reaped,
            direct_child_reaped: successful_exit_observed && forced.direct_reaped,
            successful_exit_observed,
            forced_cancellation_observed: forced.forced && forced.scope_empty,
            bounded_progress_observed: execution.progress.saw_terminal
                && execution.progress.total_bytes <= request.limits.progress_max_total_bytes,
            bounded_stderr_observed: !execution.diagnostics.total_limit_exceeded
                && execution.diagnostics.total_bytes <= request.limits.stderr_max_total_bytes,
            output_validated_by_ffprobe: true,
            windows_attached_before_execution: windows_attached,
            windows_kill_on_job_close: windows_kill_on_close,
            windows_active_processes: windows_active_zero,
            windows_handle_sentinel_leaked: platform_evidence.windows_handle_leaked,
            unix_process_group_observed: unix_group,
            unix_term_kill_observed: platform_evidence
                .unix_term_observed
                .map(|term| term && forced.forced && forced.scope_empty),
            unix_setsid_escape_observed: platform_evidence.unix_setsid_escape,
        },
        residual_uncertainty: execution.report.residual_uncertainty.clone(),
    };
    if report.behavior.direct_child_wait_observed
        && report.behavior.forced_cancellation_observed
        && report.behavior.successful_exit_observed
    {
        write_report(&report_output, &report)?;
        Ok(report)
    } else {
        Err(error(
            "behavior report",
            "one or more executed behavior oracles failed",
        ))
    }
}

fn build_request(
    host: FfmpegHostIdentityV1,
    fixture_root: &Path,
    ffmpeg: &ExecutableCapabilityObservation,
    ffprobe: &ExecutableCapabilityObservation,
    audio_payload: String,
    video_payload: String,
    limits: FfmpegSupervisionLimitsV1,
) -> Result<FfmpegSupervisionRequestV1, ProofProducerError> {
    let claim = ResourceVector {
        memory_bytes: 4 * 1024 * 1024,
        disk_read_bytes_in_flight: 4 * 1024 * 1024,
        disk_write_bytes_in_flight: 4 * 1024 * 1024,
        open_handles: 8,
        cpu_heavy_slots: 1,
        ffmpeg_processes: 1,
        ffmpeg_cpu_threads: 4,
        ..ResourceVector::default()
    };
    let operation = FfmpegOperationPlanV1::StreamCopy(FfmpegStreamCopyPlanV1 {
        inputs: vec![
            FfmpegInputFileV1 {
                path: "input/audio.aac".to_owned(),
                demuxer: FfmpegInputDemuxerV1::AacAdts,
            },
            FfmpegInputFileV1 {
                path: "input/video.h264".to_owned(),
                demuxer: FfmpegInputDemuxerV1::H264AnnexB,
            },
        ],
        stream_maps: vec![
            FfmpegStreamMapV1 {
                input_index: 0,
                stream_kind: FfmpegStreamKindV1::Audio,
                stream_index: 0,
                source_payload_sha256: audio_payload,
            },
            FfmpegStreamMapV1 {
                input_index: 1,
                stream_kind: FfmpegStreamKindV1::Video,
                stream_index: 0,
                source_payload_sha256: video_payload,
            },
        ],
        output_path: "output/merged.mp4".to_owned(),
        output_muxer: "mp4".to_owned(),
    });
    let trusted_bindings = match host.operating_system {
        FfmpegHostOperatingSystemV1::Windows => vec![
            FfmpegEnvironmentBindingV1::HostSystemRoot,
            FfmpegEnvironmentBindingV1::JobTemporaryDirectory,
        ],
        FfmpegHostOperatingSystemV1::Linux => vec![
            FfmpegEnvironmentBindingV1::JobTemporaryDirectory,
            FfmpegEnvironmentBindingV1::LocaleC,
        ],
    };
    let mut request = FfmpegSupervisionRequestV1 {
        schema_id: FFMPEG_SUPERVISION_SCHEMA_ID.to_owned(),
        version: FFMPEG_SUPERVISION_VERSION,
        request_id: RequestId::new("request_wp010_real_platform")
            .map_err(|failure| error("request identity", failure))?,
        job_id: JobId::new("job_wp010_real_platform")
            .map_err(|failure| error("job identity", failure))?,
        operation_plan_sha256: "0".repeat(64),
        toolchain: FfmpegToolchainIdentityV1 {
            ffmpeg: contract_tool(FfmpegExecutableKindV1::Ffmpeg, host, ffmpeg),
            ffprobe: contract_tool(FfmpegExecutableKindV1::Ffprobe, host, ffprobe),
        },
        operation,
        environment: FfmpegEnvironmentPolicyV1 {
            inherit_parent: false,
            trusted_bindings,
        },
        working_directory: path_text(fixture_root, "fixture root")?,
        allowed_protocols: vec![FfmpegProtocolV1::File],
        allowed_input_mechanisms: vec![FfmpegInputMechanismV1::AuditedElementaryFile],
        io: FfmpegIoPolicyV1 {
            progress_channel: FfmpegProgressChannelV1::StdoutPipeOne,
            diagnostic_channel: FfmpegDiagnosticChannelV1::StderrTail,
            output_is_job_scoped_file: true,
            stdin_disabled: true,
        },
        limits,
        resources: FfmpegResourceReferenceV1 {
            resource_contract_schema_id: RESOURCE_VECTOR_SCHEMA_ID.to_owned(),
            byte_credit_contract_schema_id: BYTE_CREDIT_SCHEMA_ID.to_owned(),
            pipe_stage: ByteCreditStage::FfmpegPipe,
            claim,
            progress_pipe_bytes: 64 * 1024,
            stderr_pipe_bytes: 64 * 1024,
        },
    };
    request.operation_plan_sha256 = request
        .canonical_operation_plan_sha256()
        .map_err(|failure| error("operation projection", failure))?;
    Ok(request)
}

fn contract_tool(
    kind: FfmpegExecutableKindV1,
    host: FfmpegHostIdentityV1,
    observed: &ExecutableCapabilityObservation,
) -> FfmpegExecutableIdentityV1 {
    let executable = observed.executable();
    FfmpegExecutableIdentityV1 {
        kind,
        absolute_path: executable.canonical_path().to_owned(),
        file_identity: executable.file_identity().to_owned(),
        content_sha256: executable.content_sha256().to_owned(),
        version_output_sha256: observed.version_output_sha256().to_owned(),
        normalized_version: observed.normalized_version().to_owned(),
        host,
        capability_binding: FfmpegCapabilityBindingV1 {
            executable_content_sha256: executable.content_sha256().to_owned(),
            normalized_probe_sha256: observed.normalized_probe_sha256().to_owned(),
            capabilities: observed.capabilities().to_vec(),
        },
    }
}

const fn proof_limits() -> FfmpegSupervisionLimitsV1 {
    FfmpegSupervisionLimitsV1 {
        // The Windows proof rehashes the pinned FFmpeg/ffprobe identities at
        // each governed startup boundary. Full distribution binaries can be
        // hundreds of MiB, so the real evidence profile reserves a bounded
        // two-minute cumulative startup window rather than relying on a warm
        // filesystem cache.
        startup_timeout_millis: 120_000,
        execution_timeout_millis: 60_000,
        graceful_stop_timeout_millis: 1_000,
        forced_kill_timeout_millis: 5_000,
        reap_timeout_millis: 5_000,
        ffprobe_timeout_millis: 120_000,
        progress_max_records: 100_000,
        progress_max_total_bytes: 8 * 1024 * 1024,
        progress_max_record_bytes: 8 * 1024,
        progress_max_field_bytes: 1024,
        progress_max_parser_steps: 400_000,
        progress_max_silence_millis: 30_000,
        consumer_stall_timeout_millis: 5_000,
        stderr_max_total_bytes: 32 * 1024 * 1024,
        stderr_tail_bytes: 64 * 1024,
        pipe_allocation_bytes: 256 * 1024,
        output_max_streams: 16,
        output_max_duration_millis: 24 * 60 * 60 * 1000,
        output_max_file_size_bytes: 10 * 1024 * 1024 * 1024,
        output_max_width: 8192,
        output_max_height: 8192,
        output_max_channels: 32,
    }
}

fn request_for_output(
    request: &FfmpegSupervisionRequestV1,
    output_path: &str,
) -> Result<FfmpegSupervisionRequestV1, ProofProducerError> {
    let mut candidate = request.clone();
    let FfmpegOperationPlanV1::StreamCopy(plan) = &mut candidate.operation;
    output_path.clone_into(&mut plan.output_path);
    candidate.operation_plan_sha256 = candidate
        .canonical_operation_plan_sha256()
        .map_err(|failure| error("negative output operation projection", failure))?;
    candidate
        .validate_for_invocation()
        .map_err(|failure| error("negative output request validation", failure))?;
    Ok(candidate)
}

#[allow(
    clippy::too_many_lines,
    reason = "the executing negative proof keeps all four actual-file constructions and exact rejection oracles adjacent"
)]
fn observe_real_output_rejections(
    request: &FfmpegSupervisionRequestV1,
    trusted: &SupervisorTrustedContext<'_>,
    environment: &[(String, String)],
    working_directory: &Path,
    ffmpeg: &ExecutableCapabilityObservation,
) -> Result<(), ProofProducerError> {
    let negative_root = working_directory.join("output/negative");
    fs::create_dir_all(&negative_root)?;
    let good_output = working_directory.join("output/merged.mp4");
    require_file(&good_output, "validated real output")?;
    let cases = [
        ("partial", "output/negative/partial.mp4"),
        ("stale", "output/negative/stale.mp4"),
        ("substituted", "output/negative/substituted.mp4"),
        ("wrong-stream", "output/negative/wrong-stream.mp4"),
    ];
    for (_, relative) in cases {
        let path = working_directory.join(relative);
        if path.exists() {
            fs::remove_file(path)?;
        }
    }

    let partial_relative = "output/negative/partial.mp4";
    let partial = working_directory.join(partial_relative);
    fs::copy(&good_output, &partial)?;
    let partial_length = fs::metadata(&partial)?.len() / 2;
    if partial_length == 0 {
        return Err(error(
            "partial output",
            "validated output is too small to truncate",
        ));
    }
    fs::OpenOptions::new()
        .write(true)
        .open(&partial)?
        .set_len(partial_length)?;

    let stale_relative = "output/negative/stale.mp4";
    fs::copy(&good_output, working_directory.join(stale_relative))?;

    let substitute_audio_relative = "tmp/substitute-audio.aac";
    let source_audio = fs::read(working_directory.join("input/audio.aac"))?;
    if source_audio.is_empty() {
        return Err(error("substituted output", "source AAC fixture is empty"));
    }
    let mut substitute_audio = Vec::with_capacity(source_audio.len().saturating_mul(2));
    substitute_audio.extend_from_slice(&source_audio);
    substitute_audio.extend_from_slice(&source_audio);
    fs::write(
        working_directory.join(substitute_audio_relative),
        substitute_audio,
    )?;
    let substituted_relative = "output/negative/substituted.mp4";
    let substituted_arguments = [
        "-hide_banner",
        "-v",
        "error",
        "-nostdin",
        "-f",
        "aac",
        "-i",
        substitute_audio_relative,
        "-f",
        "h264",
        "-i",
        "input/video.h264",
        "-map",
        "0:a:0",
        "-map",
        "1:v:0",
        "-c",
        "copy",
        "-progress",
        "pipe:1",
        "-f",
        "mp4",
        substituted_relative,
    ]
    .map(str::to_owned);
    run_real_ffmpeg_file_case(
        ffmpeg,
        &substituted_arguments,
        environment,
        working_directory,
        "substituted output",
    )?;

    let wrong_stream_relative = "output/negative/wrong-stream.mp4";
    let wrong_stream_arguments = [
        "-hide_banner",
        "-v",
        "error",
        "-nostdin",
        "-f",
        "h264",
        "-i",
        "input/video.h264",
        "-map",
        "0:v:0",
        "-c",
        "copy",
        "-progress",
        "pipe:1",
        "-f",
        "mp4",
        wrong_stream_relative,
    ]
    .map(str::to_owned);
    run_real_ffmpeg_file_case(
        ffmpeg,
        &wrong_stream_arguments,
        environment,
        working_directory,
        "wrong-stream output",
    )?;

    for (label, relative, expected_stage) in [
        ("partial", partial_relative, "ffprobe-execution"),
        ("substituted", substituted_relative, "ffprobe-normalization"),
        (
            "wrong-stream",
            wrong_stream_relative,
            "ffprobe-normalization",
        ),
    ] {
        let candidate = request_for_output(request, relative)?;
        require_existing_output_rejection(
            label,
            expected_stage,
            validate_existing_output_with_ffprobe(&candidate, environment, working_directory),
        )?;
    }

    let stale_request = request_for_output(request, stale_relative)?;
    let stale_invocation = stale_request
        .validate_for_invocation()
        .map_err(|failure| error("stale invocation", format_args!("{failure:?}")))?;
    match FfmpegSupervisor.execute(
        &stale_request,
        &stale_invocation,
        trusted,
        CancellationProfile::Never,
    ) {
        Err(SupervisorError::Filesystem("output already exists")) => Ok(()),
        Err(failure) => Err(error(
            "stale output rejection",
            format_args!("unexpected typed failure: {failure}"),
        )),
        Ok(_) => Err(error(
            "stale output rejection",
            "public supervisor accepted a preexisting real output",
        )),
    }
}

fn require_existing_output_rejection<T>(
    label: &str,
    expected_stage: &'static str,
    observed: Result<T, SupervisorError>,
) -> Result<(), ProofProducerError> {
    let expected_reason = match expected_stage {
        "ffprobe-execution" => "ffprobe reaped unsuccessfully or exceeded its bounded deadline",
        "ffprobe-normalization" => {
            "output facts or packet payload did not match the validated request"
        }
        _ => {
            return Err(error(
                "negative output validation",
                "unknown expected stage",
            ));
        }
    };
    match observed {
        Err(SupervisorError::ExistingOutputFfprobeRejected { stage, reason })
            if stage == expected_stage && reason == expected_reason =>
        {
            Ok(())
        }
        Err(SupervisorError::ExistingOutputFfprobeRejected { stage, reason }) => Err(error(
            "negative output validation",
            format_args!("{label} rejected at unexpected stage/reason: {stage}: {reason}"),
        )),
        Err(failure) => Err(error(
            "negative output validation",
            format_args!("{label} failed before typed ffprobe rejection: {failure}"),
        )),
        Ok(_) => Err(error(
            "negative output validation",
            format_args!("real ffprobe accepted {label} output"),
        )),
    }
}

#[derive(Debug)]
struct FixtureToolObservation {
    proof: FixtureToolProofIdentityV1,
}

fn observe_fixture_tools(
    fake_child: &Path,
    environment: &[(String, String)],
    working_directory: &Path,
) -> Result<Vec<FixtureToolObservation>, ProofProducerError> {
    let fake_child = observe_fixture_tool(
        fake_child,
        FixtureToolRoleV1::FakeChild,
        &["--version".to_owned()],
        "fforager-fake-child ",
        environment,
        working_directory,
    )?;
    #[cfg(target_os = "linux")]
    {
        let setsid = observe_fixture_tool(
            &absolute_env_path(ENV_SETSID)?,
            FixtureToolRoleV1::Setsid,
            &["--version".to_owned()],
            "setsid from util-linux ",
            environment,
            working_directory,
        )?;
        Ok(vec![fake_child, setsid])
    }
    #[cfg(not(target_os = "linux"))]
    Ok(vec![fake_child])
}

fn observe_fixture_tool(
    executable: &Path,
    role: FixtureToolRoleV1,
    arguments: &[String],
    expected_version_prefix: &str,
    environment: &[(String, String)],
    working_directory: &Path,
) -> Result<FixtureToolObservation, ProofProducerError> {
    let canonical = fs::canonicalize(executable)
        .map_err(|failure| error("canonicalize fixture tool", failure))?;
    let metadata = fs::metadata(&canonical)?;
    if !metadata.is_file() {
        return Err(error("fixture tool identity", "path is not a regular file"));
    }
    let content_sha256 = hash_file(&canonical)?;
    let observed_file_identity = file_identity(&canonical, &metadata)?;
    let child = spawn_verified(
        &canonical,
        arguments,
        environment,
        working_directory,
        ExecutablePinExpectation {
            file_identity: &observed_file_identity,
            content_sha256: &content_sha256,
            maximum_bytes: crate::identity::MAXIMUM_EXECUTABLE_BYTES,
            hash_timeout: PROCESS_TIMEOUT,
        },
    )
    .map_err(|failure| error("spawn fixture tool version", failure))?;
    let (exit, stdout, stderr) = observe_drained_child(child, "fixture tool version", |child| {
        child
            .wait_timeout(PROCESS_TIMEOUT)
            .map_err(|failure| error("wait fixture tool version", failure))?
            .ok_or_else(|| error("wait fixture tool version", "deadline expired"))
    })?;
    if !exit.successful() {
        return Err(error(
            "fixture tool version",
            format_args!("fixture tool exited unsuccessfully: {exit:?}"),
        ));
    }
    let version_bytes = if stdout.is_empty() { &stderr } else { &stdout };
    let version_text = std::str::from_utf8(version_bytes)
        .map_err(|failure| error("fixture tool version UTF-8", failure))?;
    let version_line = version_text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| error("fixture tool version", "version output was empty"))?;
    if !version_line.starts_with(expected_version_prefix) || version_line.len() > 512 {
        return Err(error(
            "fixture tool version",
            format_args!("unexpected bounded version line: {version_line:?}"),
        ));
    }
    let executable_name = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| error("fixture tool identity", "non-Unicode executable basename"))?;
    Ok(FixtureToolObservation {
        proof: FixtureToolProofIdentityV1 {
            role,
            executable_name: executable_name.to_owned(),
            canonical_path: canonical_fixture_path_text(&canonical)?,
            version_line: version_line.to_owned(),
            content_sha256,
            file_identity: observed_file_identity,
        },
    })
}

fn canonical_fixture_path_text(path: &Path) -> Result<String, ProofProducerError> {
    let text = path_text(path, "canonical fixture tool")?;
    #[cfg(windows)]
    {
        let bytes = text.as_bytes();
        if bytes.len() >= 7
            && text.starts_with(r"\\?\")
            && bytes[4].is_ascii_alphabetic()
            && bytes[5] == b':'
            && matches!(bytes[6], b'\\' | b'/')
        {
            return Ok(text[4..].to_owned());
        }
    }
    Ok(text)
}

fn spawn_observed(
    executable: &ExecutableCapabilityObservation,
    arguments: &[String],
    environment: &[(String, String)],
    working_directory: &Path,
    context: &str,
) -> Result<PlatformChild, ProofProducerError> {
    let executable = executable.executable();
    spawn_verified(
        Path::new(executable.canonical_path()),
        arguments,
        environment,
        working_directory,
        ExecutablePinExpectation {
            file_identity: executable.file_identity(),
            content_sha256: executable.content_sha256(),
            maximum_bytes: crate::identity::MAXIMUM_EXECUTABLE_BYTES,
            hash_timeout: Duration::from_secs(30),
        },
    )
    .map_err(|failure| error(context, failure))
}

fn fixture_tool(
    observations: &[FixtureToolObservation],
    role: FixtureToolRoleV1,
) -> Result<&FixtureToolObservation, ProofProducerError> {
    observations
        .iter()
        .find(|observation| observation.proof.role == role)
        .ok_or_else(|| error("fixture tool", format_args!("missing role {role:?}")))
}

fn spawn_fixture_observed(
    executable: &FixtureToolObservation,
    arguments: &[String],
    environment: &[(String, String)],
    working_directory: &Path,
    context: &str,
) -> Result<PlatformChild, ProofProducerError> {
    spawn_verified(
        Path::new(&executable.proof.canonical_path),
        arguments,
        environment,
        working_directory,
        ExecutablePinExpectation {
            file_identity: &executable.proof.file_identity,
            content_sha256: &executable.proof.content_sha256,
            maximum_bytes: crate::identity::MAXIMUM_EXECUTABLE_BYTES,
            hash_timeout: PROCESS_TIMEOUT,
        },
    )
    .map_err(|failure| error(context, failure))
}

fn run_real_ffmpeg_file_case(
    ffmpeg: &ExecutableCapabilityObservation,
    arguments: &[String],
    environment: &[(String, String)],
    working_directory: &Path,
    context: &str,
) -> Result<(), ProofProducerError> {
    let child = spawn_observed(ffmpeg, arguments, environment, working_directory, context)?;
    let (exit, _, stderr) = observe_drained_child(child, context, |child| {
        child
            .wait_timeout(PROCESS_TIMEOUT)
            .map_err(|failure| error(context, failure))?
            .ok_or_else(|| error(context, "deadline expired"))
    })?;
    if !exit.successful() {
        return Err(error(
            context,
            format_args!(
                "real FFmpeg failed with {exit:?}; stderr={}",
                String::from_utf8_lossy(&stderr)
            ),
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct ForcedEvidence {
    direct_reaped: bool,
    forced: bool,
    scope_empty: bool,
    exit: ExitObservation,
    lifecycle: Vec<FfmpegLifecycleObservationV1>,
    forced_kill_millis: u64,
    reap_millis: u64,
    scope_settlement_millis: u64,
    #[cfg(windows)]
    windows_active_process_samples: Vec<u32>,
}

fn forced_lifecycle_observation(
    sequence: u64,
    state: FfmpegLifecycleStateV1,
    started: Instant,
) -> FfmpegLifecycleObservationV1 {
    FfmpegLifecycleObservationV1 {
        sequence,
        state,
        monotonic_millis: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    }
}

fn forced_lifecycle_observation_at_millis(
    sequence: u64,
    state: FfmpegLifecycleStateV1,
    monotonic_millis: u64,
) -> FfmpegLifecycleObservationV1 {
    FfmpegLifecycleObservationV1 {
        sequence,
        state,
        monotonic_millis,
    }
}

fn observe_drained_child<T>(
    mut child: PlatformChild,
    context: &str,
    observe: impl FnOnce(&mut PlatformChild) -> Result<T, ProofProducerError>,
) -> Result<(T, Vec<u8>, Vec<u8>), ProofProducerError> {
    let (stdout, stderr) = match child.take_pipes() {
        Ok(pipes) => pipes,
        Err(failure) => {
            let cleanup = child.cleanup_force_reap(Duration::from_secs(5));
            return Err(match cleanup {
                Ok(_) => error(context, failure),
                Err(cleanup) => error(
                    context,
                    format_args!("{failure}; cleanup also failed: {cleanup}"),
                ),
            });
        }
    };
    let stdout_worker = std::thread::spawn(move || drain_required_bytes(stdout, CAPTURE_LIMIT));
    let stderr_worker = std::thread::spawn(move || drain_required_bytes(stderr, CAPTURE_LIMIT));
    let mut observation = observe(&mut child);
    if observation.is_ok() {
        observation = match wait_scope_empty(&child, Duration::from_secs(5)) {
            Ok(true) => observation,
            Ok(false) => Err(error(
                context,
                "declared process scope did not become empty",
            )),
            Err(failure) => Err(failure),
        };
    }
    let cleanup_failure = if observation.is_err() {
        child
            .cleanup_force_reap(Duration::from_secs(5))
            .err()
            .map(|failure| failure.to_string())
    } else {
        None
    };
    let stdout = stdout_worker
        .join()
        .map_err(|_| error(context, "stdout drain thread panicked"))?
        .map_err(|failure| error(context, format_args!("stdout drain: {failure}")))?;
    let stderr = stderr_worker
        .join()
        .map_err(|_| error(context, "stderr drain thread panicked"))?
        .map_err(|failure| error(context, format_args!("stderr drain: {failure}")))?;
    match observation {
        Ok(value) => Ok((value, stdout, stderr)),
        Err(failure) => Err(match cleanup_failure {
            Some(cleanup) => error(
                context,
                format_args!("{failure}; cleanup also failed: {cleanup}"),
            ),
            None => failure,
        }),
    }
}

fn observe_forced_real_ffmpeg(
    ffmpeg: &ExecutableCapabilityObservation,
    environment: &[(String, String)],
    working_directory: &Path,
) -> Result<ForcedEvidence, ProofProducerError> {
    let arguments = [
        "-hide_banner",
        "-nostdin",
        "-f",
        "lavfi",
        "-i",
        "testsrc=size=16x16:rate=10",
        "-progress",
        "pipe:1",
        "-f",
        "null",
        "-",
    ]
    .map(str::to_owned);
    let child = spawn_observed(
        ffmpeg,
        &arguments,
        environment,
        working_directory,
        "spawn forced FFmpeg",
    )?;
    let (evidence, _, _) = observe_drained_child(child, "forced real FFmpeg", |child| {
        let started = Instant::now();
        let mut lifecycle = vec![
            forced_lifecycle_observation(1, FfmpegLifecycleStateV1::Spawned, started),
            forced_lifecycle_observation(2, FfmpegLifecycleStateV1::Running, started),
        ];
        #[cfg(windows)]
        let mut windows_active_process_samples = vec![
            child.preexecution_active_process_count(),
            child
                .active_process_count()
                .map_err(|failure| error("query active FFmpeg Job", failure))?,
        ];
        if child
            .wait_timeout(Duration::from_millis(150))
            .map_err(|failure| error("pre-cancel wait", failure))?
            .is_some()
        {
            return Err(error(
                "forced cancellation",
                "infinite lavfi child exited before cancellation",
            ));
        }
        let force_started_millis = elapsed_millis(started);
        lifecycle.push(forced_lifecycle_observation_at_millis(
            3,
            FfmpegLifecycleStateV1::ForcedKillRequested,
            force_started_millis,
        ));
        child
            .force_terminate()
            .map_err(|failure| error("force real FFmpeg scope", failure))?;
        let force_finished_millis = elapsed_millis(started);
        let forced_kill_millis = force_finished_millis
            .checked_sub(force_started_millis)
            .ok_or_else(|| error("force real FFmpeg scope", "monotonic clock reversed"))?;
        let exit = child
            .wait_timeout(SCOPE_SETTLEMENT_TIMEOUT)
            .map_err(|failure| error("wait forced FFmpeg", failure))?
            .ok_or_else(|| error("wait forced FFmpeg", "deadline expired"))?;
        let reap_finished_millis = elapsed_millis(started);
        let reap_millis = reap_finished_millis
            .checked_sub(force_finished_millis)
            .ok_or_else(|| error("wait forced FFmpeg", "monotonic clock reversed"))?;
        let scope_empty = wait_scope_empty(child, SCOPE_SETTLEMENT_TIMEOUT)?;
        let scope_finished_millis = elapsed_millis(started);
        let scope_settlement_millis = scope_finished_millis
            .checked_sub(reap_finished_millis)
            .ok_or_else(|| error("query declared process scope", "monotonic clock reversed"))?;
        lifecycle.push(forced_lifecycle_observation_at_millis(
            4,
            FfmpegLifecycleStateV1::Reaped,
            scope_finished_millis,
        ));
        #[cfg(windows)]
        windows_active_process_samples.push(
            child
                .active_process_count()
                .map_err(|failure| error("query terminated FFmpeg Job", failure))?,
        );
        Ok(ForcedEvidence {
            direct_reaped: exit.forced_by_supervisor,
            forced: exit.forced_by_supervisor,
            scope_empty,
            exit,
            lifecycle,
            forced_kill_millis,
            reap_millis,
            scope_settlement_millis,
            #[cfg(windows)]
            windows_active_process_samples,
        })
    })?;
    Ok(evidence)
}

#[derive(Debug, Default)]
struct PlatformCounterexample {
    windows_handle_leaked: Option<bool>,
    #[cfg(windows)]
    windows_job_descendant_observed: Option<bool>,
    #[cfg(windows)]
    windows_parent_death_observed: Option<bool>,
    unix_term_observed: Option<bool>,
    unix_graceful_millis: Option<u64>,
    unix_setsid_escape: Option<bool>,
}

#[cfg(windows)]
fn containment_observations(
    forced: &ForcedEvidence,
    counterexample: &PlatformCounterexample,
    attached_before_execution: Option<bool>,
    kill_on_job_close: Option<bool>,
) -> Result<ContainmentObservationsV1, ProofProducerError> {
    let handle_sentinel_leaked = counterexample
        .windows_handle_leaked
        .ok_or_else(|| error("Windows containment", "handle sentinel result missing"))?;
    if !counterexample
        .windows_job_descendant_observed
        .ok_or_else(|| error("Windows containment", "Job descendant result missing"))?
    {
        return Err(error(
            "Windows containment",
            "in-Job descendant was not independently observed and cleaned",
        ));
    }
    let kill_on_job_close_parent_death_observed = counterexample
        .windows_parent_death_observed
        .ok_or_else(|| error("Windows containment", "parent-death result missing"))?;
    if !kill_on_job_close_parent_death_observed {
        return Err(error(
            "Windows containment",
            "KILL_ON_JOB_CLOSE did not remove the child and descendant after abrupt parent exit",
        ));
    }
    Ok(ContainmentObservationsV1::WindowsJob {
        active_process_samples: forced.windows_active_process_samples.clone(),
        attached_before_execution: attached_before_execution
            .ok_or_else(|| error("Windows containment", "pre-execution attachment missing"))?,
        kill_on_job_close: kill_on_job_close
            .ok_or_else(|| error("Windows containment", "kill-on-close observation missing"))?,
        handle_sentinel_leaked,
        kill_on_job_close_parent_death_observed,
        // No approved broker exists in WP-010, so parent death during the
        // suspended interval remains a residual rather than a proved behavior.
        suspended_orphan_observed: false,
    })
}

#[cfg(target_os = "linux")]
fn containment_observations(
    forced: &ForcedEvidence,
    counterexample: &PlatformCounterexample,
    _attached_before_execution: Option<bool>,
    _kill_on_job_close: Option<bool>,
) -> Result<ContainmentObservationsV1, ProofProducerError> {
    Ok(ContainmentObservationsV1::UnixProcessGroup {
        process_group_verified: true,
        term_sent: counterexample
            .unix_term_observed
            .ok_or_else(|| error("Unix containment", "TERM observation missing"))?,
        kill_sent: forced.forced,
        group_absent: forced.scope_empty,
        setsid_escape_observed: counterexample
            .unix_setsid_escape
            .ok_or_else(|| error("Unix containment", "setsid observation missing"))?,
    })
}

#[cfg(not(any(windows, target_os = "linux")))]
fn containment_observations(
    _forced: &ForcedEvidence,
    _counterexample: &PlatformCounterexample,
    _attached_before_execution: Option<bool>,
    _kill_on_job_close: Option<bool>,
) -> Result<ContainmentObservationsV1, ProofProducerError> {
    Err(error("containment observations", "unsupported host"))
}

#[cfg(windows)]
fn observe_platform_counterexample(
    _ffmpeg: &ExecutableCapabilityObservation,
    fixture_tools: &[FixtureToolObservation],
    environment: &[(String, String)],
    working_directory: &Path,
) -> Result<PlatformCounterexample, ProofProducerError> {
    let fake_child = fixture_tool(fixture_tools, FixtureToolRoleV1::FakeChild)?;
    let marker = working_directory.join("tmp/windows-handle-sentinel.marker");
    let arguments = vec![
        "windows-handle-inheritance".to_owned(),
        "--lifetime-ms".to_owned(),
        "5000".to_owned(),
        "--liveness-file".to_owned(),
        path_text(&marker, "Windows marker")?,
    ];
    let child = spawn_fixture_observed(
        fake_child,
        &arguments,
        environment,
        working_directory,
        "spawn Windows handle sentinel",
    )
    .map_err(|failure| error("spawn Windows handle sentinel", failure))?;
    let ((exit, descendant_observed, scope_empty, attached, kill_on_close), _, _) =
        observe_drained_child(child, "Windows handle sentinel", |child| {
            let exit = child
                .wait_timeout(Duration::from_secs(3))
                .map_err(|failure| error("wait Windows sentinel direct child", failure))?
                .ok_or_else(|| error("Windows handle sentinel", "direct child did not exit"))?;
            let descendant_observed = marker.is_file()
                && !child
                    .declared_scope_empty()
                    .map_err(|failure| error("query Windows sentinel Job", failure))?;
            child
                .force_terminate()
                .map_err(|failure| error("terminate Windows sentinel Job", failure))?;
            let scope_empty = wait_scope_empty(child, Duration::from_secs(5))?;
            Ok((
                exit,
                descendant_observed,
                scope_empty,
                child.attached_before_execution(),
                child.kill_on_close(),
            ))
        })?;
    let job_descendant_observed =
        exit.successful() && descendant_observed && scope_empty && attached && kill_on_close;
    let system_root =
        env::var("SYSTEMROOT").map_err(|failure| error("forbidden-handle SYSTEMROOT", failure))?;
    let powershell =
        PathBuf::from(system_root).join("System32/WindowsPowerShell/v1.0/powershell.exe");
    require_file(&powershell, "forbidden-handle PowerShell")?;
    let forbidden = run_handle_list_fault_probe(
        &powershell,
        environment,
        working_directory,
        Duration::from_secs(10),
    )
    .map_err(|failure| error("forbidden-handle fault probe", failure))?;
    let protected_excluded = forbidden.protected_exit.windows_status_opaque.is_some()
        && !forbidden.protected_exit.successful()
        && !forbidden.protected_exit.forced_by_supervisor
        && forbidden.protected_bytes.is_empty();
    let mutation_leaked = forbidden.mutated_exit.successful()
        && !forbidden.mutated_exit.forced_by_supervisor
        && forbidden.mutated_bytes == b"LEAK";
    let parent_death_receipt = working_directory.join("tmp/windows-parent-death-pids.txt");
    if parent_death_receipt.exists() {
        fs::remove_file(&parent_death_receipt)?;
    }
    let mut parent_death_environment = environment.to_vec();
    parent_death_environment.push((
        PARENT_DEATH_RECEIPT_ENV.to_owned(),
        path_text(&parent_death_receipt, "parent-death receipt")?,
    ));
    let parent_death = observe_kill_on_job_close_parent_death(
        &std::env::current_exe()
            .map_err(|failure| error("parent-death helper executable", failure))?,
        &[
            "--exact".to_owned(),
            PARENT_DEATH_HELPER_TEST.to_owned(),
            "--ignored".to_owned(),
            "--test-threads=1".to_owned(),
        ],
        &parent_death_environment,
        working_directory,
        &parent_death_receipt,
        Duration::from_secs(10),
    )
    .map_err(|failure| error("KILL_ON_JOB_CLOSE parent death", failure))?;
    Ok(PlatformCounterexample {
        windows_handle_leaked: Some(!(protected_excluded && mutation_leaked)),
        windows_job_descendant_observed: Some(job_descendant_observed),
        windows_parent_death_observed: Some(parent_death.kill_on_job_close_parent_death_observed),
        ..PlatformCounterexample::default()
    })
}

#[cfg(target_os = "linux")]
fn observe_platform_counterexample(
    ffmpeg: &ExecutableCapabilityObservation,
    fixture_tools: &[FixtureToolObservation],
    environment: &[(String, String)],
    working_directory: &Path,
) -> Result<PlatformCounterexample, ProofProducerError> {
    let fake_child = fixture_tool(fixture_tools, FixtureToolRoleV1::FakeChild)?;
    let setsid = fixture_tool(fixture_tools, FixtureToolRoleV1::Setsid)?;
    let marker = working_directory.join("tmp/unix-setsid-sentinel.marker");
    let arguments = vec![
        "setsid-escape".to_owned(),
        "--setsid-path".to_owned(),
        setsid.proof.canonical_path.clone(),
        "--lifetime-ms".to_owned(),
        "1000".to_owned(),
        "--liveness-file".to_owned(),
        path_text(&marker, "setsid marker")?,
    ];
    let escaped = spawn_fixture_observed(
        fake_child,
        &arguments,
        environment,
        working_directory,
        "spawn setsid counterexample",
    )?;
    let (escaped_observed, _, _) =
        observe_drained_child(escaped, "setsid counterexample", |escaped| {
            let direct = escaped
                .wait_timeout(Duration::from_secs(3))
                .map_err(|failure| error("wait setsid launcher", failure))?
                .ok_or_else(|| error("setsid counterexample", "direct child did not exit"))?;
            Ok(direct.successful()
                && marker.is_file()
                && escaped
                    .declared_scope_empty()
                    .map_err(|failure| error("probe escaped source group", failure))?)
        })?;
    wait_path_absent(&marker, Duration::from_secs(3))?;

    let (real_ffmpeg_term_observed, real_ffmpeg_term_millis) =
        observe_graceful_real_ffmpeg_term(ffmpeg, environment, working_directory)?;
    Ok(PlatformCounterexample {
        unix_term_observed: Some(real_ffmpeg_term_observed),
        unix_graceful_millis: Some(real_ffmpeg_term_millis),
        unix_setsid_escape: Some(escaped_observed),
        ..PlatformCounterexample::default()
    })
}

#[cfg(target_os = "linux")]
fn observe_graceful_real_ffmpeg_term(
    ffmpeg: &ExecutableCapabilityObservation,
    environment: &[(String, String)],
    working_directory: &Path,
) -> Result<(bool, u64), ProofProducerError> {
    let arguments = [
        "-hide_banner",
        "-nostdin",
        "-f",
        "lavfi",
        "-i",
        "testsrc=size=16x16:rate=10",
        "-progress",
        "pipe:1",
        "-f",
        "null",
        "-",
    ]
    .map(str::to_owned);
    let child = spawn_observed(
        ffmpeg,
        &arguments,
        environment,
        working_directory,
        "spawn graceful real FFmpeg",
    )?;
    let (observed, _, _) = observe_drained_child(child, "graceful real FFmpeg TERM", |child| {
        if child
            .wait_timeout(Duration::from_millis(150))
            .map_err(|failure| error("pre-TERM real FFmpeg wait", failure))?
            .is_some()
        {
            return Err(error(
                "graceful real FFmpeg TERM",
                "infinite lavfi child exited before TERM",
            ));
        }
        let graceful_started = Instant::now();
        let term_sent = child
            .request_graceful_stop()
            .map_err(|failure| error("send real FFmpeg TERM", failure))?;
        let exit = child
            .wait_timeout(Duration::from_secs(5))
            .map_err(|failure| error("wait TERM real FFmpeg", failure))?
            .ok_or_else(|| error("wait TERM real FFmpeg", "deadline expired"))?;
        let graceful_millis = elapsed_millis(graceful_started);
        let scope_empty = wait_scope_empty(child, Duration::from_secs(5))?;
        Ok((
            term_sent
                && !exit.forced_by_supervisor
                && (exit.exit_code.is_some() || exit.signal == Some(libc::SIGTERM))
                && scope_empty,
            graceful_millis,
        ))
    })?;
    Ok(observed)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn observe_platform_counterexample(
    _ffmpeg: &ExecutableCapabilityObservation,
    _fixture_tools: &[FixtureToolObservation],
    _environment: &[(String, String)],
    _working_directory: &Path,
) -> Result<PlatformCounterexample, ProofProducerError> {
    Err(error(
        "platform proof",
        "only Windows and Linux are authorized",
    ))
}

fn wait_scope_empty(child: &PlatformChild, timeout: Duration) -> Result<bool, ProofProducerError> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    loop {
        if child
            .declared_scope_empty()
            .map_err(|failure| error("query declared process scope", failure))?
        {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn wait_path_absent(path: &Path, timeout: Duration) -> Result<(), ProofProducerError> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    while path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if path.exists() {
        Err(error(
            "setsid cleanup",
            "escaped fixture exceeded its bounded lifetime",
        ))
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum PacketFraming {
    AacAdts,
    H264AnnexB,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PacketProbeRoot {
    packets: Vec<ProbePacket>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbePacket {
    size: String,
    data_hash: String,
    data: String,
}

fn measure_source_payload(
    ffprobe: &ExecutableCapabilityObservation,
    demuxer: &str,
    input: &str,
    framing: PacketFraming,
    environment: &[(String, String)],
    working_directory: &Path,
) -> Result<String, ProofProducerError> {
    let arguments = [
        "-v",
        "error",
        "-f",
        demuxer,
        "-select_streams",
        "0",
        "-show_packets",
        "-show_data",
        "-show_data_hash",
        "sha256",
        "-show_entries",
        "packet=size,data_hash,data",
        "-of",
        "json",
        input,
    ]
    .map(str::to_owned);
    let json = run_capture(ffprobe, &arguments, environment, working_directory)?;
    let root: PacketProbeRoot = serde_json::from_slice(&json)
        .map_err(|failure| error("decode source packet probe", failure))?;
    packet_payload_sha256(&root.packets, framing)
}

fn run_capture(
    executable: &ExecutableCapabilityObservation,
    arguments: &[String],
    environment: &[(String, String)],
    working_directory: &Path,
) -> Result<Vec<u8>, ProofProducerError> {
    let child = spawn_observed(
        executable,
        arguments,
        environment,
        working_directory,
        "spawn bounded capture",
    )?;
    let ((), stdout, stderr) = observe_drained_child(child, "bounded capture", |child| {
        let exit = child
            .wait_timeout(PROCESS_TIMEOUT)
            .map_err(|failure| error("wait bounded capture", failure))?
            .ok_or_else(|| error("bounded capture", "deadline expired"))?;
        if !exit.successful() {
            return Err(error(
                "bounded capture",
                format_args!("failed status {exit:?}"),
            ));
        }
        Ok(())
    })?;
    if !stderr.is_empty() {
        return Err(error("bounded capture", String::from_utf8_lossy(&stderr)));
    }
    Ok(stdout)
}

fn packet_payload_sha256(
    packets: &[ProbePacket],
    framing: PacketFraming,
) -> Result<String, ProofProducerError> {
    if packets.is_empty() || packets.len() > 100_000 {
        return Err(error(
            "packet projection",
            "packet count outside proof bound",
        ));
    }
    let mut projection = Sha256::new();
    projection.update(b"ff.ffmpeg-packet-payload@1\0");
    for (ordinal, packet) in packets.iter().enumerate() {
        let size = packet
            .size
            .parse::<usize>()
            .map_err(|failure| error("packet size", failure))?;
        let bytes = decode_packet_data(&packet.data)?;
        if size == 0 || size != bytes.len() {
            return Err(error("packet projection", "packet size mismatch"));
        }
        let declared = packet
            .data_hash
            .strip_prefix("SHA256:")
            .ok_or_else(|| error("packet projection", "missing SHA256 prefix"))?;
        if declared.to_ascii_lowercase() != hash_bytes(&bytes) {
            return Err(error("packet projection", "ffprobe packet digest mismatch"));
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
    Ok(hex_bytes(&projection.finalize()))
}

fn decode_packet_data(value: &str) -> Result<Vec<u8>, ProofProducerError> {
    let mut output = Vec::new();
    for line in value.lines().filter(|line| !line.trim().is_empty()) {
        let (_, encoded) = line
            .split_once(':')
            .ok_or_else(|| error("packet data", "missing address delimiter"))?;
        let encoded = encoded.split("  ").next().unwrap_or(encoded);
        for group in encoded.split_ascii_whitespace() {
            if group.len() % 2 != 0 || !group.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(error("packet data", "invalid hexadecimal group"));
            }
            for pair in group.as_bytes().chunks_exact(2) {
                let pair =
                    std::str::from_utf8(pair).map_err(|failure| error("packet data", failure))?;
                output.push(
                    u8::from_str_radix(pair, 16)
                        .map_err(|failure| error("packet data", failure))?,
                );
            }
        }
    }
    if output.is_empty() {
        Err(error("packet data", "empty packet"))
    } else {
        Ok(output)
    }
}

fn canonical_packet_units(
    bytes: &[u8],
    framing: PacketFraming,
) -> Result<Vec<&[u8]>, ProofProducerError> {
    match framing {
        PacketFraming::AacAdts => {
            if bytes.len() < 7
                || bytes[0] != 0xff
                || bytes[1] & 0xf6 != 0xf0
                || bytes[6] & 0x03 != 0
            {
                return Err(error("AAC packet", "invalid ADTS header"));
            }
            let header_length = if bytes[1] & 1 == 0 { 9 } else { 7 };
            let frame_length = (usize::from(bytes[3] & 3) << 11)
                | (usize::from(bytes[4]) << 3)
                | usize::from(bytes[5] >> 5);
            if frame_length != bytes.len() || bytes.len() <= header_length {
                return Err(error("AAC packet", "invalid ADTS frame length"));
            }
            Ok(vec![&bytes[header_length..]])
        }
        PacketFraming::H264AnnexB => split_h264_annex_b(bytes),
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

fn split_h264_annex_b(bytes: &[u8]) -> Result<Vec<&[u8]>, ProofProducerError> {
    let first = h264_start_code_length(bytes, 0)
        .ok_or_else(|| error("H264 packet", "missing Annex-B start code"))?;
    let mut units = Vec::new();
    let mut payload_start = first;
    let mut cursor = payload_start;
    while cursor < bytes.len() {
        if let Some(length) = h264_start_code_length(bytes, cursor) {
            if cursor == payload_start {
                return Err(error("H264 packet", "empty Annex-B unit"));
            }
            units.push(&bytes[payload_start..cursor]);
            payload_start = cursor + length;
            cursor = payload_start;
        } else {
            cursor += 1;
        }
    }
    if payload_start >= bytes.len() {
        return Err(error("H264 packet", "truncated Annex-B unit"));
    }
    units.push(&bytes[payload_start..]);
    Ok(units)
}

fn trusted_environment(fixture_root: &Path) -> Result<Vec<(String, String)>, ProofProducerError> {
    let temporary = path_text(&fixture_root.join("tmp"), "fixture temporary directory")?;
    #[cfg(windows)]
    {
        let system_root =
            env::var("SYSTEMROOT").map_err(|failure| error("trusted SYSTEMROOT", failure))?;
        let mut environment = Vec::new();
        crate::platform::append_windows_system_environment(&mut environment, &system_root)
            .map_err(|failure| error("trusted SYSTEMROOT", failure))?;
        crate::platform::append_windows_writable_environment(&mut environment, &temporary);
        Ok(environment)
    }
    #[cfg(target_os = "linux")]
    {
        Ok(vec![
            ("TMPDIR".to_owned(), temporary),
            ("LANG".to_owned(), "C".to_owned()),
            ("LC_ALL".to_owned(), "C".to_owned()),
        ])
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = temporary;
        Err(error("trusted environment", "unsupported host"))
    }
}

fn tool_proof(name: &str, observation: &ExecutableCapabilityObservation) -> ToolProofIdentityV1 {
    let executable = observation.executable();
    ToolProofIdentityV1 {
        executable_name: name.to_owned(),
        canonical_path: executable.canonical_path().to_owned(),
        version_line: observation.normalized_version().to_owned(),
        content_sha256: executable.content_sha256().to_owned(),
        file_identity: executable.file_identity().to_owned(),
        version_output_sha256: observation.version_output_sha256().to_owned(),
        normalized_probe_sha256: observation.normalized_probe_sha256().to_owned(),
        capabilities: observation.capabilities().to_vec(),
    }
}

fn repository_root() -> Result<PathBuf, ProofProducerError> {
    let current = env::current_dir()?;
    current
        .ancestors()
        .find(|candidate| candidate.join("START_HERE.yaml").is_file())
        .map(Path::to_path_buf)
        .ok_or_else(|| error("repository root", "START_HERE.yaml not found"))
}

fn absolute_env_path(name: &str) -> Result<PathBuf, ProofProducerError> {
    let value = env::var(name).map_err(|failure| error(name, failure))?;
    let path = PathBuf::from(value);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(error(name, "path must be absolute"))
    }
}

fn artifact_path(name: &str, artifact_root: &Path) -> Result<PathBuf, ProofProducerError> {
    let path = absolute_env_path(name)?;
    if path.starts_with(artifact_root) {
        Ok(path)
    } else {
        Err(error(
            name,
            format_args!("must be below {}", artifact_root.display()),
        ))
    }
}

fn require_file(path: &Path, label: &str) -> Result<(), ProofProducerError> {
    if path.is_file() {
        Ok(())
    } else {
        Err(error(
            label,
            format_args!("file not found: {}", path.display()),
        ))
    }
}

fn path_text(path: &Path, label: &str) -> Result<String, ProofProducerError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| error(label, "path is not Unicode"))
}

fn write_report(path: &Path, report: &PlatformProofReportV2) -> Result<(), ProofProducerError> {
    let parent = path
        .parent()
        .ok_or_else(|| error("report output", "missing parent"))?;
    fs::create_dir_all(parent)?;
    let encoded =
        serde_json::to_vec_pretty(report).map_err(|failure| error("encode report", failure))?;
    fs::write(path, encoded)?;
    Ok(())
}

fn hash_file(path: &Path) -> Result<String, ProofProducerError> {
    Ok(hash_bytes(&fs::read(path)?))
}

fn hash_serialized(value: &(impl serde::Serialize + ?Sized)) -> Result<String, ProofProducerError> {
    let encoded = serde_json::to_vec(value)
        .map_err(|failure| error("serialize digest projection", failure))?;
    Ok(hash_bytes(&encoded))
}

fn canonical_json(value: &(impl serde::Serialize + ?Sized)) -> Result<String, ProofProducerError> {
    serde_json::to_string(value).map_err(|failure| error("serialize canonical JSON", failure))
}

fn hash_bytes(bytes: &[u8]) -> String {
    hex_bytes(&Sha256::digest(bytes))
}

fn hash_domain(domain: &str, bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(domain.as_bytes());
    digest.update([0]);
    digest.update(bytes);
    hex_bytes(&digest.finalize())
}

fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn host_identity() -> Result<FfmpegHostIdentityV1, ProofProducerError> {
    let architecture = if cfg!(target_arch = "x86_64") {
        FfmpegHostArchitectureV1::X86_64
    } else if cfg!(target_arch = "aarch64") {
        FfmpegHostArchitectureV1::Aarch64
    } else {
        return Err(error("host architecture", env::consts::ARCH));
    };
    let operating_system = if cfg!(windows) {
        FfmpegHostOperatingSystemV1::Windows
    } else if cfg!(target_os = "linux") {
        FfmpegHostOperatingSystemV1::Linux
    } else {
        return Err(error("host operating system", env::consts::OS));
    };
    Ok(FfmpegHostIdentityV1 {
        operating_system,
        architecture,
    })
}

fn proof_platform() -> Result<ProofPlatform, ProofProducerError> {
    match (env::consts::OS, env::consts::ARCH) {
        ("windows", "x86_64") => Ok(ProofPlatform::WindowsX86_64),
        ("linux", "x86_64") => Ok(ProofPlatform::LinuxX86_64),
        _ => Err(error(
            "proof platform",
            format_args!("{}-{}", env::consts::OS, env::consts::ARCH),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_output_evidence_rejects_non_ffprobe_and_wrong_stage_failures() {
        assert!(
            require_existing_output_rejection::<()>(
                "partial",
                "ffprobe-execution",
                Err(SupervisorError::Filesystem("counterexample")),
            )
            .is_err()
        );
        assert!(
            require_existing_output_rejection::<()>(
                "partial",
                "ffprobe-execution",
                Err(SupervisorError::ExistingOutputFfprobeRejected {
                    stage: "ffprobe-normalization",
                    reason: "output facts or packet payload did not match the validated request",
                }),
            )
            .is_err()
        );
        require_existing_output_rejection::<()>(
            "partial",
            "ffprobe-execution",
            Err(SupervisorError::ExistingOutputFfprobeRejected {
                stage: "ffprobe-execution",
                reason: "ffprobe reaped unsuccessfully or exceeded its bounded deadline",
            }),
        )
        .expect("exact typed ffprobe rejection");
    }

    #[test]
    #[ignore = "requires explicitly provisioned real FFmpeg/ffprobe and shared artifact fixtures"]
    fn real_platform_proof_from_environment() {
        let report = produce_platform_proof_from_environment()
            .expect("environment-bound real platform proof must pass");
        assert!(!report.source_dirty);
        assert!(report.behavior.successful_exit_observed);
        assert!(report.behavior.forced_cancellation_observed);
        assert!(report.behavior.output_validated_by_ffprobe);
    }
}
