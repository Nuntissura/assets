//! Independent WP-FF-010 platform-report consumer.
//!
//! This build-only validator intentionally mirrors the product report wire
//! schema without depending on the product adapter. It binds report claims to
//! the live clean Git source and canonical fixture bytes, then emits a
//! consumer-owned receipt classified as integration evidence. It is not
//! `ff.runtime-proof@1` and cannot establish shipped product behavior.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const PLATFORM_SCHEMA_ID: &str = "ff.ffmpeg-platform-proof@2";
const BOUND_RUNTIME_PROJECTION_ID: &str = "ff.ffmpeg-bound-runtime-invocation-canonical-json@1";
const AGGREGATE_SCHEMA_ID: &str = "ff.ffmpeg-cross-platform-proof@1";
const RECEIPT_SCHEMA_ID: &str = "ff.ffmpeg-aggregation-receipt@1";
const PRODUCER_RECEIPT_SCHEMA_ID: &str = "ff.ffmpeg-producer-receipt@1";
const RECEIPT_SCHEMA_VERSION: &str = "1.0.0";
const PROOF_CLASS: &str = "integration";
const FIXTURE_CONTRACT_PATH: &str = "build/fixtures/contracts/ffmpeg-supervision-v1.0.json";
const REPORT_ROOT: &str = ".fforager-artifacts/runtime-proof";
const MAX_REPORT_BYTES: u64 = 1_048_576;
const MAX_TEXT_BYTES: usize = 16_384;
const MAX_RESIDUALS: usize = 16;
const MAX_RESIDUAL_BYTES: usize = 1_024;
const MAX_CANONICAL_JSON_BYTES: usize = 524_288;
const MAX_ARGUMENTS: usize = 256;
const MAX_ARGUMENT_BYTES: usize = 65_536;
const MAX_ACTIVE_PROCESS_SAMPLES: usize = 64;
const MAX_FIXTURE_TOOL_BYTES: u64 = 512 * 1024 * 1024;
const MAX_TOOL_PROBE_BYTES: usize = 8 * 1_024 * 1_024;
const PRODUCER_DISCOVERY_TIMEOUT: Duration = Duration::from_mins(3);
const PRODUCER_BUILD_TIMEOUT: Duration = Duration::from_mins(10);
const PRODUCER_EXECUTION_TIMEOUT: Duration = Duration::from_mins(15);
const PRODUCER_BUILD_OUTER_TIMEOUT: Duration = Duration::from_secs(615);
const PRODUCER_EXECUTION_OUTER_TIMEOUT: Duration = Duration::from_secs(915);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProofPlatform {
    WindowsX86_64,
    LinuxX86_64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolProofIdentityV1 {
    executable_name: String,
    canonical_path: String,
    version_line: String,
    content_sha256: String,
    file_identity: String,
    version_output_sha256: String,
    normalized_probe_sha256: String,
    capabilities: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FixtureToolRoleV1 {
    FakeChild,
    Setsid,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureToolProofIdentityV1 {
    role: FixtureToolRoleV1,
    executable_name: String,
    canonical_path: String,
    version_line: String,
    content_sha256: String,
    file_identity: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "FF-BUILD-090 mirrors the frozen behavior-observation wire schema exactly"
)]
struct PlatformBehaviorV1 {
    direct_child_wait_observed: bool,
    direct_child_reaped: bool,
    successful_exit_observed: bool,
    forced_cancellation_observed: bool,
    bounded_progress_observed: bool,
    bounded_stderr_observed: bool,
    output_validated_by_ffprobe: bool,
    windows_attached_before_execution: Option<bool>,
    windows_kill_on_job_close: Option<bool>,
    windows_active_processes: Option<u32>,
    windows_handle_sentinel_leaked: Option<bool>,
    unix_process_group_observed: Option<bool>,
    unix_term_kill_observed: Option<bool>,
    unix_setsid_escape_observed: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgressObservationsV1 {
    record_count: u64,
    total_bytes: u64,
    parser_steps: u64,
    saw_terminal: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum DiagnosticLossEvidenceV1 {
    None,
    PrefixTruncated { dropped_bytes: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectWaitReceiptEvidenceV1 {
    exit_code: Option<i32>,
    signal: Option<i32>,
    windows_opaque_status: Option<u32>,
    forced_by_supervisor: bool,
    supervision_wait_receipt_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForcedWaitReceiptEvidenceV1 {
    exit_code: Option<i32>,
    signal: Option<i32>,
    windows_opaque_status: Option<u32>,
    forced_by_supervisor: bool,
    direct_child_reaped: bool,
    lifecycle_timeline_sha256: String,
    supervision_wait_receipt_sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_field_names,
    reason = "the consumer mirrors the closed product wire schema and its explicit units"
)]
struct ProducerPhaseLimitsV2 {
    fixture_probe_timeout_millis: u64,
    identity_probe_timeout_millis: u64,
    scope_settlement_timeout_millis: u64,
    negative_cases_timeout_millis: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_field_names,
    reason = "the consumer mirrors the closed product wire schema and its explicit units"
)]
struct PhaseDeadlineObservationsV2 {
    fixture_probe_millis: u64,
    identity_probe_millis: u64,
    startup_millis: u64,
    execution_millis: u64,
    validation_millis: u64,
    graceful_stop_millis: u64,
    forced_kill_millis: u64,
    reap_millis: u64,
    scope_settlement_millis: u64,
    negative_cases_millis: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum ContainmentObservationsV1 {
    WindowsJob {
        active_process_samples: Vec<u32>,
        attached_before_execution: bool,
        kill_on_job_close: bool,
        handle_sentinel_leaked: bool,
        kill_on_job_close_parent_death_observed: bool,
        suspended_orphan_observed: bool,
    },
    UnixProcessGroup {
        process_group_verified: bool,
        term_sent: bool,
        kill_sent: bool,
        group_absent: bool,
        setsid_escape_observed: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LifecycleStateV1 {
    Spawned,
    Running,
    GracefulStopRequested,
    ForcedKillRequested,
    Reaped,
    ReapUnproven,
    Validated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LifecycleObservationV1 {
    sequence: u64,
    state: LifecycleStateV1,
    monotonic_millis: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LimitsEvidenceV1 {
    startup_timeout_millis: u64,
    execution_timeout_millis: u64,
    graceful_stop_timeout_millis: u64,
    forced_kill_timeout_millis: u64,
    reap_timeout_millis: u64,
    ffprobe_timeout_millis: u64,
    progress_max_records: u64,
    progress_max_total_bytes: u64,
    progress_max_record_bytes: u64,
    progress_max_field_bytes: u64,
    progress_max_parser_steps: u64,
    progress_max_silence_millis: u64,
    consumer_stall_timeout_millis: u64,
    stderr_max_total_bytes: u64,
    stderr_tail_bytes: u64,
    pipe_allocation_bytes: u64,
    output_max_streams: u32,
    output_max_duration_millis: u64,
    output_max_file_size_bytes: u64,
    output_max_width: u32,
    output_max_height: u32,
    output_max_channels: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StreamMapEvidenceV1 {
    input_index: u16,
    stream_kind: String,
    stream_index: u16,
    source_payload_sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OutputStreamKindV1 {
    Video,
    Audio,
    Subtitle,
    Data,
    Attachment,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputSelectedSourceV1 {
    input_index: u16,
    stream_kind: OutputStreamKindV1,
    stream_index: u16,
    source_payload_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputStreamFactV1 {
    index: u32,
    kind: OutputStreamKindV1,
    selected_source: OutputSelectedSourceV1,
    output_payload_sha256: String,
    codec_name: String,
    width: Option<u32>,
    height: Option<u32>,
    channels: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputFactsEvidenceV1 {
    output_identity_sha256: String,
    file_size_bytes: u64,
    duration_millis: u64,
    format_names: Vec<String>,
    streams: Vec<OutputStreamFactV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputEvidenceV1 {
    path: String,
    demuxer: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RequestExecutableKindV1 {
    Ffmpeg,
    Ffprobe,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RequestOperatingSystemV1 {
    Windows,
    Linux,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RequestArchitectureV1 {
    X86_64,
    Aarch64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestSchemaVersionV1 {
    major: u16,
    minor: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestHostIdentityV1 {
    operating_system: RequestOperatingSystemV1,
    architecture: RequestArchitectureV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestCapabilityBindingV1 {
    executable_content_sha256: String,
    normalized_probe_sha256: String,
    capabilities: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestExecutableIdentityV1 {
    kind: RequestExecutableKindV1,
    absolute_path: String,
    file_identity: String,
    content_sha256: String,
    version_output_sha256: String,
    normalized_version: String,
    host: RequestHostIdentityV1,
    capability_binding: RequestCapabilityBindingV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestToolchainIdentityV1 {
    ffmpeg: RequestExecutableIdentityV1,
    ffprobe: RequestExecutableIdentityV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RequestProtocolV1 {
    File,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RequestInputMechanismV1 {
    AuditedElementaryFile,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RequestInputDemuxerV1 {
    AacAdts,
    H264AnnexB,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RequestStreamKindV1 {
    Video,
    Audio,
    Subtitle,
    Data,
    Attachment,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestInputFileV1 {
    path: String,
    demuxer: RequestInputDemuxerV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestStreamMapV1 {
    input_index: u16,
    stream_kind: RequestStreamKindV1,
    stream_index: u16,
    source_payload_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestStreamCopyPlanV1 {
    inputs: Vec<RequestInputFileV1>,
    stream_maps: Vec<RequestStreamMapV1>,
    output_path: String,
    output_muxer: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum RequestOperationPlanV1 {
    StreamCopy(RequestStreamCopyPlanV1),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RequestEnvironmentBindingV1 {
    HostSystemRoot,
    JobTemporaryDirectory,
    LocaleC,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestEnvironmentPolicyV1 {
    inherit_parent: bool,
    trusted_bindings: Vec<RequestEnvironmentBindingV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RequestProgressChannelV1 {
    StdoutPipeOne,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RequestDiagnosticChannelV1 {
    StderrTail,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestIoPolicyV1 {
    progress_channel: RequestProgressChannelV1,
    diagnostic_channel: RequestDiagnosticChannelV1,
    output_is_job_scoped_file: bool,
    stdin_disabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RequestByteCreditStageV1 {
    HttpReceive,
    Decompression,
    DecryptOrPackInput,
    DecryptOrPackOutput,
    Reorder,
    Writer,
    Journal,
    FfmpegPipe,
    SpillAndSink,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestResourceVectorV1 {
    metadata_requests: u32,
    media_requests: u32,
    memory_bytes: u64,
    disk_read_bytes_in_flight: u64,
    disk_write_bytes_in_flight: u64,
    open_handles: u32,
    cpu_light_slots: u32,
    cpu_heavy_slots: u32,
    javascript_workers: u32,
    ffmpeg_processes: u32,
    ffmpeg_cpu_threads: u32,
    archive_writer_slots: u32,
    sink_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestResourceReferenceV1 {
    resource_contract_schema_id: String,
    byte_credit_contract_schema_id: String,
    pipe_stage: RequestByteCreditStageV1,
    claim: RequestResourceVectorV1,
    progress_pipe_bytes: u64,
    stderr_pipe_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestEvidenceV1 {
    schema_id: String,
    version: RequestSchemaVersionV1,
    request_id: String,
    job_id: String,
    operation_plan_sha256: String,
    toolchain: RequestToolchainIdentityV1,
    operation: RequestOperationPlanV1,
    environment: RequestEnvironmentPolicyV1,
    working_directory: String,
    allowed_protocols: Vec<RequestProtocolV1>,
    allowed_input_mechanisms: Vec<RequestInputMechanismV1>,
    io: RequestIoPolicyV1,
    limits: LimitsEvidenceV1,
    resources: RequestResourceReferenceV1,
}

#[derive(Serialize)]
struct BoundRuntimeProjection<'a> {
    request_contract_sha256: &'a str,
    operation_plan_sha256: &'a str,
    arguments: &'a [String],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlatformProofReportV2 {
    schema_id: String,
    source_commit: String,
    source_dirty: bool,
    platform: ProofPlatform,
    host_kernel: String,
    ffmpeg: ToolProofIdentityV1,
    ffprobe: ToolProofIdentityV1,
    fixture_tools: Vec<FixtureToolProofIdentityV1>,
    request_contract_canonical_json: String,
    request_contract_sha256: String,
    operation_plan_canonical_json: String,
    operation_plan_sha256: String,
    bound_runtime_projection_id: String,
    bound_runtime_sha256: String,
    argument_vector: Vec<String>,
    argument_vector_sha256: String,
    fixture_contract_sha256: String,
    fixture_payload_canonical_json: String,
    fixture_payload_sha256: String,
    limits_canonical_json: String,
    limits_sha256: String,
    lifecycle_timeline_canonical_json: String,
    lifecycle_timeline_sha256: String,
    direct_wait_receipt_canonical_json: String,
    direct_wait_receipt_sha256: String,
    forced_lifecycle_timeline_canonical_json: String,
    forced_lifecycle_timeline_sha256: String,
    forced_wait_receipt_canonical_json: String,
    forced_wait_receipt_sha256: String,
    output_facts_canonical_json: String,
    output_facts_sha256: String,
    producer_phase_limits: ProducerPhaseLimitsV2,
    phase_deadlines: PhaseDeadlineObservationsV2,
    diagnostic_total_bytes: u64,
    diagnostic_tail_hex: String,
    diagnostic_tail_sha256: String,
    diagnostic_loss: DiagnosticLossEvidenceV1,
    progress_transcript_hex: String,
    progress_transcript_sha256: String,
    progress_observations: ProgressObservationsV1,
    containment_observations: ContainmentObservationsV1,
    behavior: PlatformBehaviorV1,
    residual_uncertainty: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CrossPlatformProofV1 {
    schema_id: String,
    source_commit: String,
    request_contract_sha256_by_platform: BTreeMap<ProofPlatform, String>,
    operation_plan_sha256: String,
    fixture_contract_sha256: String,
    fixture_payload_sha256: String,
    limits_sha256: String,
    platforms: Vec<ProofPlatform>,
    residual_uncertainty: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AggregationInputReceiptV1 {
    platform: ProofPlatform,
    report_path: String,
    report_sha256: String,
    producer_receipt_path: String,
    producer_receipt_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AggregationFixtureReceiptV1 {
    path: String,
    sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AggregationReceiptV1 {
    schema_id: String,
    schema_version: String,
    proof_class: String,
    source_commit: String,
    source_dirty: bool,
    fixture_contract: AggregationFixtureReceiptV1,
    inputs: Vec<AggregationInputReceiptV1>,
    aggregate: CrossPlatformProofV1,
    limitations: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProducerCommandProjectionV1 {
    program: String,
    arguments: Vec<String>,
    build_program: String,
    build_arguments: Vec<String>,
    cargo_target_directory: String,
    working_directory: String,
    fixture_root: String,
    injected_environment: BTreeMap<String, String>,
    discovery_timeout_millis: u64,
    build_timeout_millis: u64,
    producer_timeout_millis: u64,
    build_outer_timeout_millis: u64,
    producer_outer_timeout_millis: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProducerBoundaryToolRoleV1 {
    Cargo,
    Wsl,
    Ffmpeg,
    Ffprobe,
    Setsid,
    Env,
    Timeout,
    Unshare,
    Printenv,
    Uname,
    Stat,
    Readlink,
    Fsutil,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProducerBoundaryToolIdentityV1 {
    role: ProducerBoundaryToolRoleV1,
    canonical_path: String,
    size_bytes: u64,
    content_sha256: String,
    file_identity: String,
    normalized_version: String,
    version_output_sha256: String,
    normalized_probe_sha256: Option<String>,
    capabilities: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProducerFixtureFileV1 {
    path: String,
    size_bytes: u64,
    content_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProducerReceiptV1 {
    schema_id: String,
    platform: ProofPlatform,
    source_before: crate::SourceState,
    source_after: crate::SourceState,
    report_path: String,
    report_sha256: String,
    built_fake_child_path: String,
    built_fake_child_bytes: u64,
    built_fake_child_sha256: String,
    boundary_tools: Vec<ProducerBoundaryToolIdentityV1>,
    fixture_before: Vec<ProducerFixtureFileV1>,
    fixture_after: Vec<ProducerFixtureFileV1>,
    command: ProducerCommandProjectionV1,
}

struct ValidationContext<'a> {
    source_commit: &'a str,
    source_content_fingerprint: &'a str,
    fixture_contract_sha256: &'a str,
}

struct ReportInput {
    path: String,
    bytes: Vec<u8>,
    report: PlatformProofReportV2,
    producer_receipt_path: String,
    producer_receipt_bytes: Vec<u8>,
    producer_receipt: ProducerReceiptV1,
}

struct ProducerArgs {
    platform: ProofPlatform,
    fixture_root: PathBuf,
    report_path: PathBuf,
    receipt_path: PathBuf,
}

struct PreparedProducer {
    program: String,
    arguments: Vec<String>,
    build_program: String,
    build_arguments: Vec<String>,
    injected_environment: BTreeMap<String, String>,
    fake_child: PathBuf,
    boundary_tools: Vec<ProducerBoundaryToolIdentityV1>,
    working_directory: PathBuf,
}

#[allow(
    clippy::too_many_lines,
    reason = "the proof wrapper keeps its clean-source before/execute/after transaction visible in one auditable function"
)]
pub(crate) fn run_producer(root: &Path, args: &[String]) -> Result<(), String> {
    let parsed = parse_producer_args(args)?;
    let source_before = crate::source_state(root)?;
    if source_before.dirty || !is_git_commit(&source_before.git_commit) {
        return Err(
            "FF-WP010-E-DIRTY-SOURCE: producer wrapper requires a clean committed source"
                .to_owned(),
        );
    }
    let fixture_root = root.join(&parsed.fixture_root);
    if !fixture_root.is_dir() {
        return Err("FF-WP010-E-PRODUCER-FIXTURE: fixture root is absent".to_owned());
    }
    let fixture_before = observe_producer_fixture_manifest(&fixture_root)?;
    let report_path = root.join(&parsed.report_path);
    let receipt_path = root.join(&parsed.receipt_path);
    if let Some(parent) = report_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("FF-WP010-E-PRODUCER-DIRECTORY: {error}"))?;
    }
    if let Some(parent) = receipt_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("FF-WP010-E-PRODUCER-DIRECTORY: {error}"))?;
    }
    for stale in [&report_path, &receipt_path] {
        if stale.exists() {
            fs::remove_file(stale)
                .map_err(|error| format!("FF-WP010-E-PRODUCER-STALE: {error}"))?;
        }
    }

    let platform_label = match parsed.platform {
        ProofPlatform::WindowsX86_64 => "windows_x86_64",
        ProofPlatform::LinuxX86_64 => "linux_x86_64",
    };
    let cargo_target = root
        .join(".fforager-artifacts/wp010-producer")
        .join(platform_label)
        .join("target");
    let command_working_directory = root
        .join(".fforager-artifacts/wp010-producer")
        .join(platform_label)
        .join("cwd");
    let cargo_home = root
        .join(".fforager-artifacts/wp010-producer")
        .join(platform_label)
        .join("cargo-home");
    fs::create_dir_all(&cargo_target)
        .map_err(|error| format!("FF-WP010-E-PRODUCER-DIRECTORY: {error}"))?;
    fs::create_dir_all(&command_working_directory)
        .map_err(|error| format!("FF-WP010-E-PRODUCER-DIRECTORY: {error}"))?;
    fs::create_dir_all(&cargo_home)
        .map_err(|error| format!("FF-WP010-E-PRODUCER-DIRECTORY: {error}"))?;
    let build_arguments = vec![
        "build".to_owned(),
        "--manifest-path".to_owned(),
        "build/Cargo.toml".to_owned(),
        "--locked".to_owned(),
        "-p".to_owned(),
        "fforager-testkit".to_owned(),
        "--bin".to_owned(),
        "fforager-fake-child".to_owned(),
    ];
    let producer_arguments = vec![
        "test".to_owned(),
        "--manifest-path".to_owned(),
        "build/Cargo.toml".to_owned(),
        "--locked".to_owned(),
        "-p".to_owned(),
        "fforager-ffmpeg".to_owned(),
        "proof_producer::tests::real_platform_proof_from_environment".to_owned(),
        "--".to_owned(),
        "--ignored".to_owned(),
        "--exact".to_owned(),
        "--nocapture".to_owned(),
    ];
    let prepared = match parsed.platform {
        ProofPlatform::WindowsX86_64 => prepare_windows_producer(
            root,
            &fixture_root,
            &report_path,
            &cargo_target,
            &command_working_directory,
            &cargo_home,
            &source_before,
            &build_arguments,
            &producer_arguments,
        )?,
        ProofPlatform::LinuxX86_64 => prepare_linux_producer(
            root,
            &fixture_root,
            &report_path,
            &cargo_target,
            &command_working_directory,
            &cargo_home,
            &source_before,
            &build_arguments,
            &producer_arguments,
        )?,
    };
    let (fake_child_bytes, fake_child_sha256) = hash_file_bounded(&prepared.fake_child)?;
    let report_bytes = read_bounded(&report_path)?;
    let fixture_after = observe_producer_fixture_manifest(&fixture_root)?;
    if fixture_before != fixture_after {
        return Err(
            "FF-WP010-E-PRODUCER-FIXTURE: fixture bytes changed during producer execution"
                .to_owned(),
        );
    }
    let report: PlatformProofReportV2 = serde_json::from_slice(&report_bytes)
        .map_err(|error| format!("FF-WP010-E-REPORT-SCHEMA: {error}"))?;
    if report.platform != parsed.platform
        || report.source_commit != source_before.git_commit
        || report.source_dirty
    {
        return Err(
            "FF-WP010-E-PRODUCER-PROVENANCE: producer report source/platform diverged".to_owned(),
        );
    }
    let fake_identity = report
        .fixture_tools
        .iter()
        .find(|identity| identity.role == FixtureToolRoleV1::FakeChild)
        .ok_or_else(|| "FF-WP010-E-PRODUCER-FIXTURE: report omits fake-child".to_owned())?;
    if fake_identity.content_sha256 != fake_child_sha256 {
        return Err(
            "FF-WP010-E-PRODUCER-FIXTURE: report did not execute wrapper-built fake-child"
                .to_owned(),
        );
    }
    let source_after = crate::source_state(root)?;
    if source_after.dirty || !crate::source_states_equal(&source_before, &source_after) {
        return Err(
            "FF-WP010-E-PRODUCER-SOURCE-RACE: source changed during producer execution".to_owned(),
        );
    }
    let command =
        ProducerCommandProjectionV1 {
            program: prepared.program,
            arguments: prepared.arguments,
            build_program: prepared.build_program,
            build_arguments: prepared.build_arguments,
            cargo_target_directory: slash(
                cargo_target.strip_prefix(root).map_err(|_| {
                    "FF-WP010-E-PRODUCER-PATH: target escaped repository".to_owned()
                })?,
            ),
            working_directory: slash(prepared.working_directory.strip_prefix(root).map_err(
                |_| "FF-WP010-E-PRODUCER-PATH: command cwd escaped repository".to_owned(),
            )?),
            fixture_root: slash(&parsed.fixture_root),
            injected_environment: prepared.injected_environment,
            discovery_timeout_millis: duration_millis(PRODUCER_DISCOVERY_TIMEOUT),
            build_timeout_millis: duration_millis(PRODUCER_BUILD_TIMEOUT),
            producer_timeout_millis: duration_millis(PRODUCER_EXECUTION_TIMEOUT),
            build_outer_timeout_millis: duration_millis(PRODUCER_BUILD_OUTER_TIMEOUT),
            producer_outer_timeout_millis: duration_millis(PRODUCER_EXECUTION_OUTER_TIMEOUT),
        };
    let receipt =
        ProducerReceiptV1 {
            schema_id: PRODUCER_RECEIPT_SCHEMA_ID.to_owned(),
            platform: parsed.platform,
            source_before,
            source_after,
            report_path: slash(&parsed.report_path),
            report_sha256: sha256(&report_bytes),
            built_fake_child_path: slash(prepared.fake_child.strip_prefix(root).map_err(|_| {
                "FF-WP010-E-PRODUCER-PATH: fake-child escaped repository".to_owned()
            })?),
            built_fake_child_bytes: fake_child_bytes,
            built_fake_child_sha256: fake_child_sha256,
            boundary_tools: prepared.boundary_tools,
            fixture_before,
            fixture_after,
            command,
        };
    let encoded = serde_json::to_vec_pretty(&receipt)
        .map_err(|error| format!("FF-WP010-E-PRODUCER-RECEIPT: {error}"))?;
    fs::write(&receipt_path, &encoded)
        .map_err(|error| format!("FF-WP010-E-PRODUCER-RECEIPT: {error}"))?;
    let persisted: ProducerReceiptV1 = serde_json::from_slice(&read_bounded(&receipt_path)?)
        .map_err(|error| format!("FF-WP010-E-PRODUCER-RECEIPT: {error}"))?;
    if persisted != receipt {
        return Err("FF-WP010-E-PRODUCER-RECEIPT: persisted receipt changed".to_owned());
    }
    println!(
        "PASS WP-FF-010-PRODUCER-WRAPPER; platform={platform_label}; report={}; receipt={}",
        slash(&parsed.report_path),
        slash(&parsed.receipt_path)
    );
    Ok(())
}

fn parse_producer_args(args: &[String]) -> Result<ProducerArgs, String> {
    let [
        platform_flag,
        platform,
        fixture_flag,
        fixture,
        report_flag,
        report,
        receipt_flag,
        receipt,
    ] = args
    else {
        return Err(producer_usage());
    };
    if platform_flag != "--platform"
        || fixture_flag != "--fixture-root"
        || report_flag != "--report"
        || receipt_flag != "--receipt"
    {
        return Err(producer_usage());
    }
    let platform = match platform.as_str() {
        "windows_x86_64" => ProofPlatform::WindowsX86_64,
        "linux_x86_64" => ProofPlatform::LinuxX86_64,
        _ => return Err(producer_usage()),
    };
    let fixture_root = safe_relative_path(fixture)?;
    let report_path = safe_relative_path(report)?;
    let receipt_path = safe_relative_path(receipt)?;
    if !fixture_root.starts_with(".fforager-artifacts")
        || !report_path.starts_with(REPORT_ROOT)
        || !receipt_path.starts_with(REPORT_ROOT)
        || report_path == receipt_path
    {
        return Err(
            "FF-WP010-E-PRODUCER-PATH: fixture/report/receipt paths are not confined".to_owned(),
        );
    }
    Ok(ProducerArgs {
        platform,
        fixture_root,
        report_path,
        receipt_path,
    })
}

fn producer_usage() -> String {
    "usage: fforager-xtask ffmpeg-supervision-produce --platform <windows_x86_64|linux_x86_64> --fixture-root PATH --report PATH --receipt PATH".to_owned()
}

#[allow(
    clippy::too_many_arguments,
    reason = "the producer wrapper passes one explicit closed command projection without caller-authored command text"
)]
fn prepare_windows_producer(
    root: &Path,
    fixture_root: &Path,
    report_path: &Path,
    cargo_target: &Path,
    command_working_directory: &Path,
    cargo_home: &Path,
    source: &crate::SourceState,
    build_arguments: &[String],
    producer_arguments: &[String],
) -> Result<PreparedProducer, String> {
    let cargo = find_windows_program(root, "cargo.exe")?;
    let ffmpeg = find_windows_program(root, "ffmpeg.exe")?;
    let ffprobe = find_windows_program(root, "ffprobe.exe")?;
    let boundary_tools = observe_boundary_tools(root, ProofPlatform::WindowsX86_64)?;
    let fake_child = cargo_target.join("debug/fforager-fake-child.exe");
    let cargo_target_text = path_utf8(cargo_target, "cargo target")?;
    let cargo_home_text = path_utf8(cargo_home, "isolated Cargo home")?;
    let rustup_home = std::env::var_os("USERPROFILE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|home| home.join(".rustup"))
        .ok_or_else(|| "FF-WP010-E-PRODUCER-ENVIRONMENT: USERPROFILE is unavailable".to_owned())?;
    let manifest_path = root.join("build/Cargo.toml");
    let manifest = path_utf8(&manifest_path, "Windows build manifest")?;
    let build_arguments = absolute_manifest_arguments(build_arguments, manifest)?;
    let producer_arguments = absolute_manifest_arguments(producer_arguments, manifest)?;
    let mut build = Command::new(&cargo);
    build
        .args(&build_arguments)
        .current_dir(command_working_directory)
        .env("CARGO_TARGET_DIR", cargo_target_text)
        .env("CARGO_HOME", cargo_home_text)
        .env("RUSTUP_HOME", &rustup_home);
    run_bounded_command(
        &mut build,
        "Windows fake-child build",
        PRODUCER_BUILD_OUTER_TIMEOUT,
    )?;
    let mut injected = BTreeMap::from([
        (
            "FFORAGER_WP010_FFMPEG".to_owned(),
            path_utf8(&ffmpeg, "Windows ffmpeg")?.to_owned(),
        ),
        (
            "FFORAGER_WP010_FFPROBE".to_owned(),
            path_utf8(&ffprobe, "Windows ffprobe")?.to_owned(),
        ),
        (
            "FFORAGER_WP010_FIXTURE_ROOT".to_owned(),
            path_utf8(fixture_root, "Windows fixture root")?.to_owned(),
        ),
        (
            "FFORAGER_WP010_REPORT_OUTPUT".to_owned(),
            path_utf8(report_path, "Windows report")?.to_owned(),
        ),
        (
            "FFORAGER_WP010_FAKE_CHILD".to_owned(),
            path_utf8(&fake_child, "Windows fake-child")?.to_owned(),
        ),
        (
            "FFORAGER_WP010_SOURCE_COMMIT".to_owned(),
            source.git_commit.clone(),
        ),
        ("FFORAGER_WP010_SOURCE_DIRTY".to_owned(), "false".to_owned()),
        (
            "FFORAGER_WP010_HOST_KERNEL".to_owned(),
            "Windows_NT-x86_64".to_owned(),
        ),
    ]);
    injected.insert("CARGO_TARGET_DIR".to_owned(), cargo_target_text.to_owned());
    injected.insert("CARGO_HOME".to_owned(), cargo_home_text.to_owned());
    injected.insert(
        "RUSTUP_HOME".to_owned(),
        path_utf8(&rustup_home, "Windows Rustup home")?.to_owned(),
    );
    let mut producer = Command::new(&cargo);
    producer
        .args(&producer_arguments)
        .current_dir(command_working_directory)
        .envs(&injected);
    run_bounded_command(
        &mut producer,
        "Windows FFmpeg proof producer",
        PRODUCER_EXECUTION_OUTER_TIMEOUT,
    )?;
    Ok(PreparedProducer {
        program: path_utf8(&cargo, "Windows cargo")?.to_owned(),
        arguments: producer_arguments,
        build_program: path_utf8(&cargo, "Windows cargo")?.to_owned(),
        build_arguments,
        injected_environment: injected,
        fake_child,
        boundary_tools,
        working_directory: command_working_directory.to_path_buf(),
    })
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the producer wrapper passes one explicit closed WSL command projection without shell text"
)]
fn prepare_linux_producer(
    root: &Path,
    fixture_root: &Path,
    report_path: &Path,
    cargo_target: &Path,
    command_working_directory: &Path,
    cargo_home: &Path,
    source: &crate::SourceState,
    build_arguments: &[String],
    producer_arguments: &[String],
) -> Result<PreparedProducer, String> {
    const DISTRO: &str = "Ubuntu";
    let wsl = system32_program("wsl.exe")?;
    let linux_root = windows_path_to_wsl(root, DISTRO)?;
    let linux_fixture = windows_path_to_wsl(fixture_root, DISTRO)?;
    let linux_report = windows_path_to_wsl(report_path, DISTRO)?;
    let linux_target = windows_path_to_wsl(cargo_target, DISTRO)?;
    let linux_command_cwd = windows_path_to_wsl(command_working_directory, DISTRO)?;
    let linux_cargo_home = windows_path_to_wsl(cargo_home, DISTRO)?;
    let fake_child = cargo_target.join("debug/fforager-fake-child");
    let linux_fake_child = format!("{linux_target}/debug/fforager-fake-child");
    let host_kernel = capture_bounded_command(
        Command::new(&wsl).args(["-d", DISTRO, "--exec", "/usr/bin/uname", "-srmo"]),
        "Linux host kernel",
        PRODUCER_DISCOVERY_TIMEOUT,
    )?;
    let linux_home = capture_bounded_command(
        Command::new(&wsl).args(["-d", DISTRO, "--exec", "/usr/bin/printenv", "HOME"]),
        "Linux home discovery",
        PRODUCER_DISCOVERY_TIMEOUT,
    )?;
    validate_linux_absolute_path(&linux_home, "Linux home")?;
    let linux_cargo = format!("{linux_home}/.cargo/bin/cargo");
    let _canonical_cargo = linux_canonical_tool_path(&wsl, DISTRO, &linux_cargo)?;
    let boundary_tools = observe_boundary_tools(root, ProofPlatform::LinuxX86_64)?;
    let closed_base_environment = linux_closed_base_environment(&linux_home, &linux_cargo_home);
    let linux_manifest = format!("{linux_root}/build/Cargo.toml");
    let build_arguments = absolute_manifest_arguments(build_arguments, &linux_manifest)?;
    let producer_arguments = absolute_manifest_arguments(producer_arguments, &linux_manifest)?;
    let mut build_environment = closed_base_environment.clone();
    build_environment.insert("CARGO_TARGET_DIR".to_owned(), linux_target.clone());
    let build_wsl_arguments = wsl_command_arguments(
        DISTRO,
        &linux_command_cwd,
        &build_environment,
        &linux_cargo,
        &build_arguments,
        PRODUCER_BUILD_TIMEOUT,
    );
    let mut build = Command::new(&wsl);
    build
        .args(&build_wsl_arguments)
        .current_dir(command_working_directory);
    let build_output = execute_bounded_wsl_command(
        &mut build,
        "Linux fake-child build",
        PRODUCER_BUILD_TIMEOUT,
        PRODUCER_BUILD_OUTER_TIMEOUT,
    )?;
    require_bounded_command_success(&build_output, "Linux fake-child build")?;
    let mut injected = closed_base_environment;
    injected.extend([
        ("CARGO_TARGET_DIR".to_owned(), linux_target),
        (
            "FFORAGER_WP010_FFMPEG".to_owned(),
            "/usr/bin/ffmpeg".to_owned(),
        ),
        (
            "FFORAGER_WP010_FFPROBE".to_owned(),
            "/usr/bin/ffprobe".to_owned(),
        ),
        ("FFORAGER_WP010_FIXTURE_ROOT".to_owned(), linux_fixture),
        ("FFORAGER_WP010_REPORT_OUTPUT".to_owned(), linux_report),
        ("FFORAGER_WP010_FAKE_CHILD".to_owned(), linux_fake_child),
        (
            "FFORAGER_WP010_SOURCE_COMMIT".to_owned(),
            source.git_commit.clone(),
        ),
        ("FFORAGER_WP010_SOURCE_DIRTY".to_owned(), "false".to_owned()),
        ("FFORAGER_WP010_HOST_KERNEL".to_owned(), host_kernel),
        (
            "FFORAGER_WP010_SETSID".to_owned(),
            "/usr/bin/setsid".to_owned(),
        ),
    ]);
    let producer_wsl_arguments = wsl_command_arguments(
        DISTRO,
        &linux_command_cwd,
        &injected,
        &linux_cargo,
        &producer_arguments,
        PRODUCER_EXECUTION_TIMEOUT,
    );
    let mut producer = Command::new(&wsl);
    producer
        .args(&producer_wsl_arguments)
        .current_dir(command_working_directory);
    let producer_output = execute_bounded_wsl_command(
        &mut producer,
        "Linux FFmpeg proof producer",
        PRODUCER_EXECUTION_TIMEOUT,
        PRODUCER_EXECUTION_OUTER_TIMEOUT,
    )?;
    require_bounded_command_success(&producer_output, "Linux FFmpeg proof producer")?;
    Ok(PreparedProducer {
        program: path_utf8(&wsl, "Windows WSL launcher")?.to_owned(),
        arguments: producer_wsl_arguments,
        build_program: path_utf8(&wsl, "Windows WSL launcher")?.to_owned(),
        build_arguments: build_wsl_arguments,
        injected_environment: injected,
        fake_child,
        boundary_tools,
        working_directory: command_working_directory.to_path_buf(),
    })
}

fn wsl_command_arguments(
    distro: &str,
    linux_root: &str,
    environment: &BTreeMap<String, String>,
    program: &str,
    arguments: &[String],
    timeout: Duration,
) -> Vec<String> {
    let mut result = vec![
        "-d".to_owned(),
        distro.to_owned(),
        "--cd".to_owned(),
        linux_root.to_owned(),
        "--exec".to_owned(),
        "/usr/bin/timeout".to_owned(),
        "--signal=TERM".to_owned(),
        "--kill-after=5s".to_owned(),
        format!("{}s", timeout.as_secs()),
        "/usr/bin/unshare".to_owned(),
        "--user".to_owned(),
        "--map-current-user".to_owned(),
        "--pid".to_owned(),
        "--fork".to_owned(),
        "--kill-child=SIGKILL".to_owned(),
        "--mount-proc".to_owned(),
        "/usr/bin/env".to_owned(),
        "-i".to_owned(),
    ];
    result.extend(
        environment
            .iter()
            .map(|(key, value)| format!("{key}={value}")),
    );
    result.push(program.to_owned());
    result.extend_from_slice(arguments);
    result
}

fn linux_closed_base_environment(home: &str, cargo_home: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("HOME".to_owned(), home.to_owned()),
        ("CARGO_HOME".to_owned(), cargo_home.to_owned()),
        ("RUSTUP_HOME".to_owned(), format!("{home}/.rustup")),
        (
            "PATH".to_owned(),
            format!(
                "{home}/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
            ),
        ),
        ("LANG".to_owned(), "C.UTF-8".to_owned()),
        ("LC_ALL".to_owned(), "C.UTF-8".to_owned()),
    ])
}

fn absolute_manifest_arguments(
    arguments: &[String],
    manifest: &str,
) -> Result<Vec<String>, String> {
    let mut absolute = arguments.to_vec();
    let manifest_index = absolute
        .iter()
        .position(|argument| argument == "--manifest-path")
        .and_then(|index| index.checked_add(1))
        .filter(|index| *index < absolute.len())
        .ok_or_else(|| "FF-WP010-E-PRODUCER-COMMAND: manifest argument is missing".to_owned())?;
    manifest.clone_into(&mut absolute[manifest_index]);
    Ok(absolute)
}

fn windows_path_to_wsl(path: &Path, distro: &str) -> Result<String, String> {
    let wsl = system32_program("wsl.exe")?;
    capture_bounded_command(
        Command::new(wsl).args([
            "-d",
            distro,
            "--exec",
            "wslpath",
            "-a",
            path_utf8(path, "Windows path for WSL")?,
        ]),
        "WSL path translation",
        PRODUCER_DISCOVERY_TIMEOUT,
    )
}

fn find_windows_program(root: &Path, name: &str) -> Result<PathBuf, String> {
    let deadline = Instant::now()
        .checked_add(PRODUCER_DISCOVERY_TIMEOUT)
        .ok_or_else(|| "FF-WP010-E-PRODUCER-TOOL-DEADLINE: deadline overflow".to_owned())?;
    find_windows_program_until(root, name, deadline)
}

fn find_windows_program_until(
    root: &Path,
    name: &str,
    deadline: Instant,
) -> Result<PathBuf, String> {
    let where_exe = system32_program_until("where.exe", deadline)?;
    let output = capture_bounded_command(
        Command::new(where_exe).arg(name).current_dir(root),
        "Windows executable discovery",
        remaining_discovery_time(deadline)?,
    )?;
    let first = output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| format!("FF-WP010-E-PRODUCER-TOOL: {name} was not discovered"))?;
    let discovered = PathBuf::from(first);
    if !discovered.is_absolute() || !discovered.is_file() {
        return Err(format!(
            "FF-WP010-E-PRODUCER-TOOL: {name} discovery did not return an absolute file"
        ));
    }
    let canonical = fs::canonicalize(&discovered)
        .map_err(|error| format!("FF-WP010-E-PRODUCER-TOOL: {name}: {error}"))?;
    if !canonical.is_file() {
        return Err(format!(
            "FF-WP010-E-PRODUCER-TOOL: {name} canonical target is not a file"
        ));
    }
    let _remaining = remaining_discovery_time(deadline)?;
    // Preserve the discovered entrypoint for execution. Rustup's `cargo.exe`
    // shim resolves to `rustup.exe`; executing the canonical target changes
    // argv[0] semantics and runs rustup itself instead of Cargo. Identity and
    // content are still measured from the canonical target below.
    Ok(discovered)
}

fn system32_program(name: &str) -> Result<PathBuf, String> {
    let system_root = std::env::var_os("SYSTEMROOT")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "FF-WP010-E-PRODUCER-TOOL: SYSTEMROOT is unavailable".to_owned())?;
    let candidate = PathBuf::from(system_root).join("System32").join(name);
    let canonical = fs::canonicalize(&candidate)
        .map_err(|error| format!("FF-WP010-E-PRODUCER-TOOL: {}: {error}", candidate.display()))?;
    if !canonical.is_file() {
        return Err(format!(
            "FF-WP010-E-PRODUCER-TOOL: {} is not a regular file",
            canonical.display()
        ));
    }
    Ok(canonical)
}

fn system32_program_until(name: &str, deadline: Instant) -> Result<PathBuf, String> {
    let _remaining = remaining_discovery_time(deadline)?;
    let path = system32_program(name)?;
    let _remaining = remaining_discovery_time(deadline)?;
    Ok(path)
}

fn validate_linux_absolute_path(value: &str, label: &str) -> Result<(), String> {
    if !value.starts_with('/')
        || value.ends_with('/')
        || value.contains('\0')
        || value
            .split('/')
            .any(|component| matches!(component, "." | ".."))
    {
        return Err(format!(
            "FF-WP010-E-PRODUCER-PATH: {label} is not a safe absolute Linux path"
        ));
    }
    Ok(())
}

fn run_bounded_command(
    command: &mut Command,
    label: &str,
    timeout: Duration,
) -> Result<(), String> {
    let output = execute_bounded_command_with_limit(command, label, timeout, MAX_TOOL_PROBE_BYTES)?;
    require_bounded_command_success(&output, label)
}

fn require_bounded_command_success(
    output: &BoundedCommandOutput,
    label: &str,
) -> Result<(), String> {
    if output.stdout_truncated || output.stderr_truncated {
        return Err(format!(
            "FF-WP010-E-PRODUCER-COMMAND: {label}: output exceeded bound"
        ));
    }
    if !output.status.success() {
        return Err(format!(
            "FF-WP010-E-PRODUCER-COMMAND: {label}: exit={:?}; stdout={}; stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

fn execute_bounded_wsl_command(
    command: &mut Command,
    label: &str,
    inner_timeout: Duration,
    outer_timeout: Duration,
) -> Result<BoundedCommandOutput, String> {
    let required_outer = inner_timeout
        .checked_add(Duration::from_secs(15))
        .ok_or_else(|| "FF-WP010-E-PRODUCER-TIMEOUT: nested deadline overflow".to_owned())?;
    if outer_timeout < required_outer {
        return Err(
            "FF-WP010-E-PRODUCER-TIMEOUT: outer wrapper does not leave kill/reap margin".to_owned(),
        );
    }
    execute_bounded_command_with_limit(command, label, outer_timeout, MAX_TOOL_PROBE_BYTES)
}

fn capture_bounded_command(
    command: &mut Command,
    label: &str,
    timeout: Duration,
) -> Result<String, String> {
    let output = execute_bounded_command_with_limit(command, label, timeout, MAX_TOOL_PROBE_BYTES)?;
    if !output.status.success()
        || output.stdout_truncated
        || output.stderr_truncated
        || output.stdout.len() > MAX_TEXT_BYTES
    {
        return Err(format!(
            "FF-WP010-E-PRODUCER-COMMAND: {label}: exit={:?}",
            output.status.code()
        ));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|error| format!("FF-WP010-E-PRODUCER-COMMAND: {label}: {error}"))
}

struct BoundedCommandOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

fn execute_bounded_command_with_limit(
    command: &mut Command,
    label: &str,
    timeout: Duration,
    retained_limit: usize,
) -> Result<BoundedCommandOutput, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let program = command.get_program().to_string_lossy().into_owned();
    let governed_cargo_home = command
        .get_envs()
        .find(|(key, _)| key.eq_ignore_ascii_case("CARGO_HOME"))
        .and_then(|(_, value)| value)
        .map(ToOwned::to_owned);
    let governed_rustup_home = command
        .get_envs()
        .find(|(key, _)| key.eq_ignore_ascii_case("RUSTUP_HOME"))
        .and_then(|(_, value)| value)
        .map(ToOwned::to_owned);
    crate::sanitize_rust_command_environment(command, &program);
    if let Some(value) = governed_cargo_home {
        command.env("CARGO_HOME", value);
    }
    if let Some(value) = governed_rustup_home {
        command.env("RUSTUP_HOME", value);
    }
    crate::configure_quiet_process(command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("FF-WP010-E-PRODUCER-COMMAND: {label}: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("FF-WP010-E-PRODUCER-COMMAND: {label}: stdout missing"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("FF-WP010-E-PRODUCER-COMMAND: {label}: stderr missing"))?;
    let stdout_drain = spawn_bounded_drain(stdout, label.to_owned(), "stdout", retained_limit);
    let stderr_drain = spawn_bounded_drain(stderr, label.to_owned(), "stderr", retained_limit);
    let status = match crate::wait_for_child(&mut child, label, &[], timeout) {
        Ok(status) => status,
        Err(error) => {
            // A descendant outside the terminated tree may still own a pipe.
            // Detaching the bounded readers keeps the wrapper deadline hard;
            // the producer receipt is not emitted on this failure path.
            drop(stdout_drain);
            drop(stderr_drain);
            return Err(format!("FF-WP010-E-PRODUCER-TIMEOUT: {error}"));
        }
    };
    let (stdout, stdout_truncated) = join_bounded_drain(stdout_drain, label, "stdout")?;
    let (stderr, stderr_truncated) = join_bounded_drain(stderr_drain, label, "stderr")?;
    Ok(BoundedCommandOutput {
        status,
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
    })
}

fn spawn_bounded_drain<R>(
    mut reader: R,
    label: String,
    stream: &'static str,
    retained_limit: usize,
) -> thread::JoinHandle<Result<(Vec<u8>, bool), String>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut retained = Vec::new();
        let mut truncated = false;
        let mut buffer = vec![0_u8; 64 * 1_024].into_boxed_slice();
        loop {
            let read = reader.read(&mut buffer).map_err(|error| {
                format!("FF-WP010-E-PRODUCER-COMMAND: {label}: drain {stream}: {error}")
            })?;
            if read == 0 {
                break;
            }
            let remaining = retained_limit.saturating_sub(retained.len());
            let keep = remaining.min(read);
            retained.extend_from_slice(&buffer[..keep]);
            truncated |= keep != read;
        }
        Ok((retained, truncated))
    })
}

fn join_bounded_drain(
    handle: thread::JoinHandle<Result<(Vec<u8>, bool), String>>,
    label: &str,
    stream: &str,
) -> Result<(Vec<u8>, bool), String> {
    handle
        .join()
        .map_err(|_| format!("FF-WP010-E-PRODUCER-COMMAND: {label}: {stream} drain panicked"))?
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn remaining_discovery_time(deadline: Instant) -> Result<Duration, String> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| {
            "FF-WP010-E-PRODUCER-TOOL-DEADLINE: manifest discovery deadline elapsed".to_owned()
        })
}

fn path_utf8<'a>(path: &'a Path, label: &str) -> Result<&'a str, String> {
    path.to_str()
        .ok_or_else(|| format!("FF-WP010-E-PRODUCER-PATH: {label} is not Unicode"))
}

pub(crate) fn run(root: &Path, args: &[String]) -> Result<(), String> {
    let (windows_arg, windows_receipt, linux_arg, linux_receipt) = parse_args(args)?;
    let source_before = crate::source_state(root)?;
    if source_before.dirty || !is_git_commit(&source_before.git_commit) {
        return Err(
            "FF-WP010-E-DIRTY-SOURCE: aggregation requires a clean committed source".to_owned(),
        );
    }
    let fixture_bytes = read_bounded(&root.join(FIXTURE_CONTRACT_PATH))?;
    let fixture_sha256 = sha256(&fixture_bytes);
    let windows = read_report_input(root, &windows_arg, &windows_receipt)?;
    let linux = read_report_input(root, &linux_arg, &linux_receipt)?;
    if windows.report.platform != ProofPlatform::WindowsX86_64
        || linux.report.platform != ProofPlatform::LinuxX86_64
    {
        return Err(
            "FF-WP010-E-PLATFORM-PATH: labeled platform report has the wrong platform".to_owned(),
        );
    }
    let context = ValidationContext {
        source_commit: &source_before.git_commit,
        source_content_fingerprint: &source_before.content_fingerprint,
        fixture_contract_sha256: &fixture_sha256,
    };
    let inputs = vec![windows, linux];
    for input in &inputs {
        validate_producer_receipt(root, input, &context)?;
    }
    if inputs[0].producer_receipt.fixture_before != inputs[1].producer_receipt.fixture_before {
        return Err(
            "FF-WP010-E-PRODUCER-FIXTURE: Windows/Linux fixture manifests differ".to_owned(),
        );
    }
    let reports = [inputs[0].report.clone(), inputs[1].report.clone()];
    let aggregate = validate_and_aggregate(&reports, &context)?;
    let receipt = build_receipt(
        &source_before.git_commit,
        &fixture_sha256,
        &inputs,
        aggregate,
    );
    validate_receipt(&receipt, &inputs, &context)?;
    let bytes = serde_json::to_vec_pretty(&receipt)
        .map_err(|error| format!("FF-WP010-E-RECEIPT-SERIALIZE: {error}"))?;
    let report_dir = root.join(REPORT_ROOT).join("ffmpeg-supervision");
    fs::create_dir_all(&report_dir)
        .map_err(|error| format!("FF-WP010-E-RECEIPT-DIRECTORY: {error}"))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("FF-WP010-E-CLOCK: {error}"))?
        .as_nanos();
    let path = report_dir.join(format!("aggregate-{}-{nonce}.json", std::process::id()));
    fs::write(&path, &bytes).map_err(|error| format!("FF-WP010-E-RECEIPT-WRITE: {error}"))?;
    let persisted = read_bounded(&path)?;
    let persisted_receipt: AggregationReceiptV1 = serde_json::from_slice(&persisted)
        .map_err(|error| format!("FF-WP010-E-RECEIPT-PARSE: {error}"))?;
    validate_receipt(&persisted_receipt, &inputs, &context)?;
    if persisted_receipt != receipt {
        return Err("FF-WP010-E-FORGED-RECEIPT: persisted receipt changed".to_owned());
    }
    let source_after = crate::source_state(root)?;
    if !crate::source_states_equal(&source_before, &source_after) {
        return Err("FF-WP010-E-SOURCE-RACE: repository changed during aggregation".to_owned());
    }
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "FF-WP010-E-RECEIPT-PATH: receipt escaped repository".to_owned())?;
    println!(
        "PASS WP-FF-010-REPORT-CONSUMER; proof_class={PROOF_CLASS}; receipt={}; sha256={}",
        slash(relative),
        sha256(&persisted)
    );
    Ok(())
}

fn parse_args(args: &[String]) -> Result<(String, String, String, String), String> {
    match args {
        [windows_flag, windows, windows_receipt_flag, windows_receipt, linux_flag, linux, linux_receipt_flag, linux_receipt]
            if windows_flag == "--windows-report"
                && windows_receipt_flag == "--windows-receipt"
                && linux_flag == "--linux-report"
                && linux_receipt_flag == "--linux-receipt" =>
        {
            Ok((
                windows.clone(),
                windows_receipt.clone(),
                linux.clone(),
                linux_receipt.clone(),
            ))
        }
        _ => Err("usage: fforager-xtask ffmpeg-supervision-aggregate --windows-report PATH --windows-receipt PATH --linux-report PATH --linux-receipt PATH".to_owned()),
    }
}

fn read_report_input(
    root: &Path,
    argument: &str,
    producer_receipt_argument: &str,
) -> Result<ReportInput, String> {
    let relative = safe_relative_path(argument)?;
    let producer_receipt_relative = safe_relative_path(producer_receipt_argument)?;
    if !relative.starts_with(REPORT_ROOT) || !producer_receipt_relative.starts_with(REPORT_ROOT) {
        return Err(format!(
            "FF-WP010-E-REPORT-PATH: report must remain under {REPORT_ROOT}"
        ));
    }
    let bytes = read_bounded(&root.join(&relative))?;
    let report = serde_json::from_slice(&bytes)
        .map_err(|error| format!("FF-WP010-E-REPORT-SCHEMA: {error}"))?;
    let producer_receipt_bytes = read_bounded(&root.join(&producer_receipt_relative))?;
    let producer_receipt = serde_json::from_slice(&producer_receipt_bytes)
        .map_err(|error| format!("FF-WP010-E-PRODUCER-RECEIPT: {error}"))?;
    Ok(ReportInput {
        path: slash(&relative),
        bytes,
        report,
        producer_receipt_path: slash(&producer_receipt_relative),
        producer_receipt_bytes,
        producer_receipt,
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "the receipt validator keeps the complete closed provenance transaction together for auditability"
)]
fn validate_producer_receipt(
    root: &Path,
    input: &ReportInput,
    context: &ValidationContext<'_>,
) -> Result<(), String> {
    let receipt = &input.producer_receipt;
    validate_producer_source_binding(receipt, input, context)?;
    validate_boundary_tool_manifest(root, receipt, &input.report)?;
    let fake_relative = safe_relative_path(&receipt.built_fake_child_path)?;
    let platform_label = match receipt.platform {
        ProofPlatform::WindowsX86_64 => "windows_x86_64",
        ProofPlatform::LinuxX86_64 => "linux_x86_64",
    };
    let expected_target = Path::new(".fforager-artifacts/wp010-producer")
        .join(platform_label)
        .join("target");
    let expected_working_directory = Path::new(".fforager-artifacts/wp010-producer")
        .join(platform_label)
        .join("cwd");
    let expected_cargo_home = Path::new(".fforager-artifacts/wp010-producer")
        .join(platform_label)
        .join("cargo-home");
    let fixture_root = safe_relative_path(&receipt.command.fixture_root)?;
    if !fake_relative.starts_with(&expected_target)
        || receipt.command.cargo_target_directory != slash(&expected_target)
        || receipt.command.working_directory != slash(&expected_working_directory)
        || !fixture_root.starts_with(".fforager-artifacts")
        || receipt.command.discovery_timeout_millis != duration_millis(PRODUCER_DISCOVERY_TIMEOUT)
        || receipt.command.build_timeout_millis != duration_millis(PRODUCER_BUILD_TIMEOUT)
        || receipt.command.producer_timeout_millis != duration_millis(PRODUCER_EXECUTION_TIMEOUT)
        || receipt.command.build_outer_timeout_millis
            != duration_millis(PRODUCER_BUILD_OUTER_TIMEOUT)
        || receipt.command.producer_outer_timeout_millis
            != duration_millis(PRODUCER_EXECUTION_OUTER_TIMEOUT)
    {
        return Err(
            "FF-WP010-E-PRODUCER-COMMAND: receipt target, fixture, or deadline mismatch".to_owned(),
        );
    }
    let current_fixture = observe_producer_fixture_manifest(&root.join(&fixture_root))?;
    require_exact_fixture_manifest(
        &receipt.fixture_before,
        &receipt.fixture_after,
        &current_fixture,
    )?;
    let (fake_bytes, fake_sha256) = hash_file_bounded(&root.join(&fake_relative))?;
    if receipt.built_fake_child_bytes != fake_bytes
        || receipt.built_fake_child_sha256 != fake_sha256
    {
        return Err(
            "FF-WP010-E-PRODUCER-FIXTURE: wrapper-built fake-child changed after production"
                .to_owned(),
        );
    }
    let reported_fake = input
        .report
        .fixture_tools
        .iter()
        .find(|identity| identity.role == FixtureToolRoleV1::FakeChild)
        .ok_or_else(|| "FF-WP010-E-PRODUCER-FIXTURE: report omits fake-child".to_owned())?;
    if reported_fake.content_sha256 != fake_sha256 {
        return Err(
            "FF-WP010-E-PRODUCER-FIXTURE: report fixture hash is not wrapper-built bytes"
                .to_owned(),
        );
    }
    let build_arguments = vec![
        "build".to_owned(),
        "--manifest-path".to_owned(),
        "build/Cargo.toml".to_owned(),
        "--locked".to_owned(),
        "-p".to_owned(),
        "fforager-testkit".to_owned(),
        "--bin".to_owned(),
        "fforager-fake-child".to_owned(),
    ];
    let producer_arguments = vec![
        "test".to_owned(),
        "--manifest-path".to_owned(),
        "build/Cargo.toml".to_owned(),
        "--locked".to_owned(),
        "-p".to_owned(),
        "fforager-ffmpeg".to_owned(),
        "proof_producer::tests::real_platform_proof_from_environment".to_owned(),
        "--".to_owned(),
        "--ignored".to_owned(),
        "--exact".to_owned(),
        "--nocapture".to_owned(),
    ];
    match receipt.platform {
        ProofPlatform::WindowsX86_64 => {
            let cargo = find_windows_program(root, "cargo.exe")?;
            let expected_program = path_utf8(&cargo, "Windows cargo")?;
            let target_absolute = root.join(&expected_target);
            let cargo_home_absolute = root.join(&expected_cargo_home);
            let fake_absolute = target_absolute.join("debug/fforager-fake-child.exe");
            let fixture_absolute = root.join(&fixture_root);
            let report_absolute = root.join(&input.path);
            let rustup_home = std::env::var_os("USERPROFILE")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .map(|home| home.join(".rustup"))
                .ok_or_else(|| {
                    "FF-WP010-E-PRODUCER-ENVIRONMENT: USERPROFILE is unavailable".to_owned()
                })?;
            let expected_environment = BTreeMap::from([
                (
                    "CARGO_TARGET_DIR".to_owned(),
                    path_utf8(&target_absolute, "Windows cargo target")?.to_owned(),
                ),
                (
                    "CARGO_HOME".to_owned(),
                    path_utf8(&cargo_home_absolute, "isolated Cargo home")?.to_owned(),
                ),
                (
                    "RUSTUP_HOME".to_owned(),
                    path_utf8(&rustup_home, "Windows Rustup home")?.to_owned(),
                ),
                (
                    "FFORAGER_WP010_FFMPEG".to_owned(),
                    path_utf8(&find_windows_program(root, "ffmpeg.exe")?, "Windows ffmpeg")?
                        .to_owned(),
                ),
                (
                    "FFORAGER_WP010_FFPROBE".to_owned(),
                    path_utf8(
                        &find_windows_program(root, "ffprobe.exe")?,
                        "Windows ffprobe",
                    )?
                    .to_owned(),
                ),
                (
                    "FFORAGER_WP010_FIXTURE_ROOT".to_owned(),
                    path_utf8(&fixture_absolute, "Windows fixture root")?.to_owned(),
                ),
                (
                    "FFORAGER_WP010_REPORT_OUTPUT".to_owned(),
                    path_utf8(&report_absolute, "Windows report")?.to_owned(),
                ),
                (
                    "FFORAGER_WP010_FAKE_CHILD".to_owned(),
                    path_utf8(&fake_absolute, "Windows fake-child")?.to_owned(),
                ),
                (
                    "FFORAGER_WP010_SOURCE_COMMIT".to_owned(),
                    context.source_commit.to_owned(),
                ),
                ("FFORAGER_WP010_SOURCE_DIRTY".to_owned(), "false".to_owned()),
                (
                    "FFORAGER_WP010_HOST_KERNEL".to_owned(),
                    "Windows_NT-x86_64".to_owned(),
                ),
            ]);
            let manifest_path = root.join("build/Cargo.toml");
            let manifest = path_utf8(&manifest_path, "Windows build manifest")?;
            let expected_producer_arguments =
                absolute_manifest_arguments(&producer_arguments, manifest)?;
            let expected_build_arguments = absolute_manifest_arguments(&build_arguments, manifest)?;
            let expected = ProducerCommandProjectionV1 {
                program: expected_program.to_owned(),
                arguments: expected_producer_arguments,
                build_program: expected_program.to_owned(),
                build_arguments: expected_build_arguments,
                cargo_target_directory: slash(&expected_target),
                working_directory: slash(&expected_working_directory),
                fixture_root: slash(&fixture_root),
                injected_environment: expected_environment,
                discovery_timeout_millis: duration_millis(PRODUCER_DISCOVERY_TIMEOUT),
                build_timeout_millis: duration_millis(PRODUCER_BUILD_TIMEOUT),
                producer_timeout_millis: duration_millis(PRODUCER_EXECUTION_TIMEOUT),
                build_outer_timeout_millis: duration_millis(PRODUCER_BUILD_OUTER_TIMEOUT),
                producer_outer_timeout_millis: duration_millis(PRODUCER_EXECUTION_OUTER_TIMEOUT),
            };
            require_exact_command_projection(&receipt.command, &expected)?;
        }
        ProofPlatform::LinuxX86_64 => {
            const DISTRO: &str = "Ubuntu";
            let wsl = system32_program("wsl.exe")?;
            let expected_program = path_utf8(&wsl, "Windows WSL launcher")?;
            let linux_root = windows_path_to_wsl(root, DISTRO)?;
            let linux_fixture = windows_path_to_wsl(&root.join(&fixture_root), DISTRO)?;
            let linux_report = windows_path_to_wsl(&root.join(&input.path), DISTRO)?;
            let linux_target = windows_path_to_wsl(&root.join(&expected_target), DISTRO)?;
            let linux_cargo_home = windows_path_to_wsl(&root.join(&expected_cargo_home), DISTRO)?;
            let linux_command_cwd =
                windows_path_to_wsl(&root.join(&expected_working_directory), DISTRO)?;
            let linux_fake_child = format!("{linux_target}/debug/fforager-fake-child");
            let linux_home = capture_bounded_command(
                Command::new(&wsl).args(["-d", DISTRO, "--exec", "/usr/bin/printenv", "HOME"]),
                "Linux home validation",
                PRODUCER_DISCOVERY_TIMEOUT,
            )?;
            validate_linux_absolute_path(&linux_home, "Linux home")?;
            let host_kernel = capture_bounded_command(
                Command::new(&wsl).args(["-d", DISTRO, "--exec", "/usr/bin/uname", "-srmo"]),
                "Linux host-kernel validation",
                PRODUCER_DISCOVERY_TIMEOUT,
            )?;
            let linux_cargo = format!("{linux_home}/.cargo/bin/cargo");
            let _canonical_cargo = linux_canonical_tool_path(&wsl, DISTRO, &linux_cargo)?;
            let mut expected_environment =
                linux_closed_base_environment(&linux_home, &linux_cargo_home);
            expected_environment.extend([
                ("CARGO_TARGET_DIR".to_owned(), linux_target.clone()),
                (
                    "FFORAGER_WP010_FFMPEG".to_owned(),
                    "/usr/bin/ffmpeg".to_owned(),
                ),
                (
                    "FFORAGER_WP010_FFPROBE".to_owned(),
                    "/usr/bin/ffprobe".to_owned(),
                ),
                ("FFORAGER_WP010_FIXTURE_ROOT".to_owned(), linux_fixture),
                ("FFORAGER_WP010_REPORT_OUTPUT".to_owned(), linux_report),
                ("FFORAGER_WP010_FAKE_CHILD".to_owned(), linux_fake_child),
                (
                    "FFORAGER_WP010_SOURCE_COMMIT".to_owned(),
                    context.source_commit.to_owned(),
                ),
                ("FFORAGER_WP010_SOURCE_DIRTY".to_owned(), "false".to_owned()),
                ("FFORAGER_WP010_HOST_KERNEL".to_owned(), host_kernel),
                (
                    "FFORAGER_WP010_SETSID".to_owned(),
                    "/usr/bin/setsid".to_owned(),
                ),
            ]);
            let mut expected_build_environment =
                linux_closed_base_environment(&linux_home, &linux_cargo_home);
            expected_build_environment.insert("CARGO_TARGET_DIR".to_owned(), linux_target);
            let linux_manifest = format!("{linux_root}/build/Cargo.toml");
            let expected_producer_logical =
                absolute_manifest_arguments(&producer_arguments, &linux_manifest)?;
            let expected_build_logical =
                absolute_manifest_arguments(&build_arguments, &linux_manifest)?;
            let expected_arguments = wsl_command_arguments(
                DISTRO,
                &linux_command_cwd,
                &expected_environment,
                &linux_cargo,
                &expected_producer_logical,
                PRODUCER_EXECUTION_TIMEOUT,
            );
            let expected_build_arguments = wsl_command_arguments(
                DISTRO,
                &linux_command_cwd,
                &expected_build_environment,
                &linux_cargo,
                &expected_build_logical,
                PRODUCER_BUILD_TIMEOUT,
            );
            let expected = ProducerCommandProjectionV1 {
                program: expected_program.to_owned(),
                arguments: expected_arguments,
                build_program: expected_program.to_owned(),
                build_arguments: expected_build_arguments,
                cargo_target_directory: slash(&expected_target),
                working_directory: slash(&expected_working_directory),
                fixture_root: slash(&fixture_root),
                injected_environment: expected_environment,
                discovery_timeout_millis: duration_millis(PRODUCER_DISCOVERY_TIMEOUT),
                build_timeout_millis: duration_millis(PRODUCER_BUILD_TIMEOUT),
                producer_timeout_millis: duration_millis(PRODUCER_EXECUTION_TIMEOUT),
                build_outer_timeout_millis: duration_millis(PRODUCER_BUILD_OUTER_TIMEOUT),
                producer_outer_timeout_millis: duration_millis(PRODUCER_EXECUTION_OUTER_TIMEOUT),
            };
            require_exact_command_projection(&receipt.command, &expected)?;
        }
    }
    Ok(())
}

fn validate_producer_source_binding(
    receipt: &ProducerReceiptV1,
    input: &ReportInput,
    context: &ValidationContext<'_>,
) -> Result<(), String> {
    if receipt.schema_id != PRODUCER_RECEIPT_SCHEMA_ID
        || receipt.platform != input.report.platform
        || receipt.source_before != receipt.source_after
        || receipt.source_before.dirty
        || receipt.source_before.git_commit != context.source_commit
        || receipt.source_before.content_fingerprint != context.source_content_fingerprint
        || receipt.report_path != input.path
        || receipt.report_sha256 != sha256(&input.bytes)
    {
        return Err(
            "FF-WP010-E-PRODUCER-PROVENANCE: receipt source/report binding mismatch".to_owned(),
        );
    }
    Ok(())
}

fn require_exact_command_projection(
    claimed: &ProducerCommandProjectionV1,
    expected: &ProducerCommandProjectionV1,
) -> Result<(), String> {
    if claimed != expected {
        return Err(
            "FF-WP010-E-PRODUCER-COMMAND: canonical command/environment projection mismatch"
                .to_owned(),
        );
    }
    Ok(())
}

fn validate_boundary_tool_manifest(
    root: &Path,
    receipt: &ProducerReceiptV1,
    report: &PlatformProofReportV2,
) -> Result<(), String> {
    let observed = observe_boundary_tools(root, receipt.platform)?;
    require_exact_boundary_manifest(&receipt.boundary_tools, &observed)?;
    let ffmpeg = unique_boundary_tool(&observed, ProducerBoundaryToolRoleV1::Ffmpeg)?;
    let ffprobe = unique_boundary_tool(&observed, ProducerBoundaryToolRoleV1::Ffprobe)?;
    validate_reported_media_tool(ffmpeg, &report.ffmpeg, receipt.platform)?;
    validate_reported_media_tool(ffprobe, &report.ffprobe, receipt.platform)?;
    if receipt.platform == ProofPlatform::LinuxX86_64 {
        let setsid = unique_boundary_tool(&observed, ProducerBoundaryToolRoleV1::Setsid)?;
        let reported = report
            .fixture_tools
            .iter()
            .filter(|tool| tool.role == FixtureToolRoleV1::Setsid)
            .collect::<Vec<_>>();
        if reported.len() != 1
            || reported[0].canonical_path != setsid.canonical_path
            || reported[0].content_sha256 != setsid.content_sha256
            || reported[0].file_identity != setsid.file_identity
            || reported[0].version_line != setsid.normalized_version
        {
            return Err(
                "FF-WP010-E-PRODUCER-TOOL: Linux setsid report diverges from wrapper observation"
                    .to_owned(),
            );
        }
    }
    Ok(())
}

fn require_exact_boundary_manifest(
    claimed: &[ProducerBoundaryToolIdentityV1],
    observed: &[ProducerBoundaryToolIdentityV1],
) -> Result<(), String> {
    if claimed != observed {
        return Err(
            "FF-WP010-E-PRODUCER-TOOL: boundary tool path/content/identity/version changed"
                .to_owned(),
        );
    }
    Ok(())
}

fn unique_boundary_tool(
    tools: &[ProducerBoundaryToolIdentityV1],
    role: ProducerBoundaryToolRoleV1,
) -> Result<&ProducerBoundaryToolIdentityV1, String> {
    let matches = tools
        .iter()
        .filter(|tool| tool.role == role)
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(format!(
            "FF-WP010-E-PRODUCER-TOOL: expected one {role:?} boundary tool"
        ));
    }
    Ok(matches[0])
}

fn validate_reported_media_tool(
    observed: &ProducerBoundaryToolIdentityV1,
    reported: &ToolProofIdentityV1,
    platform: ProofPlatform,
) -> Result<(), String> {
    let observed_path = match platform {
        ProofPlatform::WindowsX86_64 => observed
            .canonical_path
            .strip_prefix(r"\\?\")
            .unwrap_or(&observed.canonical_path),
        ProofPlatform::LinuxX86_64 => &observed.canonical_path,
    };
    if reported.canonical_path != observed_path
        || reported.content_sha256 != observed.content_sha256
        || reported.file_identity != observed.file_identity
        || reported.version_line != observed.normalized_version
        || reported.version_output_sha256 != observed.version_output_sha256
        || Some(&reported.normalized_probe_sha256) != observed.normalized_probe_sha256.as_ref()
        || reported.capabilities != observed.capabilities
    {
        return Err(
            "FF-WP010-E-PRODUCER-TOOL: media report diverges from independent wrapper observation"
                .to_owned(),
        );
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "the closed producer manifest explicitly inventories every executable trusted by each platform wrapper"
)]
fn observe_boundary_tools(
    root: &Path,
    platform: ProofPlatform,
) -> Result<Vec<ProducerBoundaryToolIdentityV1>, String> {
    let deadline = Instant::now()
        .checked_add(PRODUCER_DISCOVERY_TIMEOUT)
        .ok_or_else(|| "FF-WP010-E-PRODUCER-TOOL-DEADLINE: deadline overflow".to_owned())?;
    match platform {
        ProofPlatform::WindowsX86_64 => {
            let cargo = find_windows_program_until(root, "cargo.exe", deadline)?;
            let ffmpeg = find_windows_program_until(root, "ffmpeg.exe", deadline)?;
            let ffprobe = find_windows_program_until(root, "ffprobe.exe", deadline)?;
            Ok(vec![
                observe_windows_boundary_tool(
                    ProducerBoundaryToolRoleV1::Cargo,
                    &cargo,
                    &["--version", "--verbose"],
                    deadline,
                )?,
                observe_windows_media_tool(
                    ProducerBoundaryToolRoleV1::Ffmpeg,
                    &ffmpeg,
                    true,
                    deadline,
                )?,
                observe_windows_media_tool(
                    ProducerBoundaryToolRoleV1::Ffprobe,
                    &ffprobe,
                    false,
                    deadline,
                )?,
                observe_windows_boundary_tool(
                    ProducerBoundaryToolRoleV1::Fsutil,
                    &system32_program_until("fsutil.exe", deadline)?,
                    &["fsinfo", "drives"],
                    deadline,
                )?,
            ])
        }
        ProofPlatform::LinuxX86_64 => {
            const DISTRO: &str = "Ubuntu";
            let wsl = system32_program_until("wsl.exe", deadline)?;
            let home = capture_bounded_command(
                Command::new(&wsl).args(["-d", DISTRO, "--exec", "/usr/bin/printenv", "HOME"]),
                "Linux home manifest validation",
                remaining_discovery_time(deadline)?,
            )?;
            validate_linux_absolute_path(&home, "Linux home")?;
            let cargo_entrypoint = format!("{home}/.cargo/bin/cargo");
            let cargo = linux_canonical_tool_path_until(&wsl, DISTRO, &cargo_entrypoint, deadline)?;
            let mut tools = vec![
                observe_linux_boundary_tool_from_entrypoint(
                    &wsl,
                    DISTRO,
                    &home,
                    ProducerBoundaryToolRoleV1::Cargo,
                    &cargo,
                    &cargo_entrypoint,
                    &["--version", "--verbose"],
                    deadline,
                )?,
                observe_windows_boundary_tool(
                    ProducerBoundaryToolRoleV1::Wsl,
                    &wsl,
                    &["--version"],
                    deadline,
                )?,
                observe_linux_media_tool(
                    &wsl,
                    DISTRO,
                    &home,
                    ProducerBoundaryToolRoleV1::Ffmpeg,
                    "/usr/bin/ffmpeg",
                    true,
                    deadline,
                )?,
                observe_linux_media_tool(
                    &wsl,
                    DISTRO,
                    &home,
                    ProducerBoundaryToolRoleV1::Ffprobe,
                    "/usr/bin/ffprobe",
                    false,
                    deadline,
                )?,
            ];
            for (role, path) in [
                (ProducerBoundaryToolRoleV1::Setsid, "/usr/bin/setsid"),
                (ProducerBoundaryToolRoleV1::Env, "/usr/bin/env"),
                (ProducerBoundaryToolRoleV1::Timeout, "/usr/bin/timeout"),
                (ProducerBoundaryToolRoleV1::Unshare, "/usr/bin/unshare"),
                (ProducerBoundaryToolRoleV1::Printenv, "/usr/bin/printenv"),
                (ProducerBoundaryToolRoleV1::Uname, "/usr/bin/uname"),
                (ProducerBoundaryToolRoleV1::Stat, "/usr/bin/stat"),
                (ProducerBoundaryToolRoleV1::Readlink, "/usr/bin/readlink"),
            ] {
                tools.push(observe_linux_boundary_tool(
                    &wsl,
                    DISTRO,
                    &home,
                    role,
                    path,
                    &["--version"],
                    deadline,
                )?);
            }
            tools.push(observe_windows_boundary_tool(
                ProducerBoundaryToolRoleV1::Fsutil,
                &system32_program_until("fsutil.exe", deadline)?,
                &["fsinfo", "drives"],
                deadline,
            )?);
            Ok(tools)
        }
    }
}

fn safe_relative_path(value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(
            "FF-WP010-E-REPORT-PATH: report path must be repository-relative and confined"
                .to_owned(),
        );
    }
    Ok(path.to_path_buf())
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("FF-WP010-E-REPORT-READ: {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.len() > MAX_REPORT_BYTES {
        return Err(format!(
            "FF-WP010-E-REPORT-BOUND: {} is not a bounded regular file",
            path.display()
        ));
    }
    fs::read(path).map_err(|error| format!("FF-WP010-E-REPORT-READ: {}: {error}", path.display()))
}

fn hash_file_bounded(path: &Path) -> Result<(u64, String), String> {
    let deadline = Instant::now()
        .checked_add(PRODUCER_DISCOVERY_TIMEOUT)
        .ok_or_else(|| "FF-WP010-E-PRODUCER-TOOL-DEADLINE: deadline overflow".to_owned())?;
    hash_file_bounded_until(path, deadline)
}

fn hash_file_bounded_until(path: &Path, deadline: Instant) -> Result<(u64, String), String> {
    if Instant::now() >= deadline {
        return Err("FF-WP010-E-PRODUCER-TOOL-DEADLINE: discovery deadline elapsed".to_owned());
    }
    let metadata = fs::metadata(path)
        .map_err(|error| format!("FF-WP010-E-FIXTURE-TOOL-READ: {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_FIXTURE_TOOL_BYTES {
        return Err(format!(
            "FF-WP010-E-FIXTURE-TOOL-BOUND: {} is not a bounded regular executable",
            path.display()
        ));
    }
    let mut file = fs::File::open(path)
        .map_err(|error| format!("FF-WP010-E-FIXTURE-TOOL-READ: {}: {error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1_024].into_boxed_slice();
    let mut total = 0_u64;
    loop {
        if Instant::now() >= deadline {
            return Err("FF-WP010-E-PRODUCER-TOOL-DEADLINE: hash deadline elapsed".to_owned());
        }
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("FF-WP010-E-FIXTURE-TOOL-READ: {error}"))?;
        if count == 0 {
            break;
        }
        if Instant::now() >= deadline {
            return Err("FF-WP010-E-PRODUCER-TOOL-DEADLINE: hash deadline elapsed".to_owned());
        }
        total = total
            .checked_add(u64::try_from(count).unwrap_or(u64::MAX))
            .ok_or_else(|| "FF-WP010-E-FIXTURE-TOOL-BOUND: size overflow".to_owned())?;
        if total > MAX_FIXTURE_TOOL_BYTES {
            return Err("FF-WP010-E-FIXTURE-TOOL-BOUND: executable exceeded ceiling".to_owned());
        }
        hasher.update(&buffer[..count]);
    }
    if Instant::now() >= deadline {
        return Err("FF-WP010-E-PRODUCER-TOOL-DEADLINE: hash deadline elapsed".to_owned());
    }
    Ok((total, hex_digest(hasher.finalize())))
}

fn observe_producer_fixture_manifest(
    fixture_root: &Path,
) -> Result<Vec<ProducerFixtureFileV1>, String> {
    let deadline = Instant::now()
        .checked_add(PRODUCER_DISCOVERY_TIMEOUT)
        .ok_or_else(|| "FF-WP010-E-PRODUCER-FIXTURE: deadline overflow".to_owned())?;
    [
        "input/audio.aac",
        "input/video.h264",
        "ffmpeg-supervision-v1.0.json",
    ]
    .into_iter()
    .map(|relative| {
        let (size_bytes, content_sha256) =
            hash_file_bounded_until(&fixture_root.join(relative), deadline)?;
        Ok(ProducerFixtureFileV1 {
            path: relative.to_owned(),
            size_bytes,
            content_sha256,
        })
    })
    .collect()
}

fn require_exact_fixture_manifest(
    before: &[ProducerFixtureFileV1],
    after: &[ProducerFixtureFileV1],
    current: &[ProducerFixtureFileV1],
) -> Result<(), String> {
    if before != after || before != current {
        return Err(
            "FF-WP010-E-PRODUCER-FIXTURE: fixture manifest changed before, after, or aggregation"
                .to_owned(),
        );
    }
    Ok(())
}

fn host_file_identity(path: &Path, deadline: Instant) -> Result<String, String> {
    #[cfg(windows)]
    {
        let fsutil = system32_program_until("fsutil.exe", deadline)?;
        let path_text = path_utf8(path, "file identity")?;
        let drive_path = path_text.strip_prefix(r"\\?\").unwrap_or(path_text);
        let drive = drive_path
            .get(..2)
            .filter(|value| {
                value.as_bytes()[0].is_ascii_alphabetic() && value.as_bytes()[1] == b':'
            })
            .ok_or_else(|| {
                format!(
                    "FF-WP010-E-PRODUCER-TOOL: {} has no drive identity",
                    path.display()
                )
            })?;
        let file_output = capture_bounded_command(
            Command::new(&fsutil).args(["file", "queryfileid", path_text]),
            "Windows boundary tool file identity",
            remaining_discovery_time(deadline)?,
        )?;
        let file_token = file_output
            .split_ascii_whitespace()
            .find_map(|value| {
                value
                    .strip_prefix("0x")
                    .or_else(|| value.strip_prefix("0X"))
            })
            .filter(|value| value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or_else(|| {
                format!(
                    "FF-WP010-E-PRODUCER-TOOL: {} has malformed fsutil identity",
                    path.display()
                )
            })?;
        let mut file_bytes = file_token
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let text = std::str::from_utf8(pair)
                    .map_err(|_| "FF-WP010-E-PRODUCER-TOOL: malformed fsutil byte".to_owned())?;
                u8::from_str_radix(text, 16)
                    .map_err(|_| "FF-WP010-E-PRODUCER-TOOL: malformed fsutil byte".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()?;
        file_bytes.reverse();
        let file_id = hex_digest(file_bytes);
        let volume_output = capture_bounded_command(
            Command::new(fsutil).args(["fsinfo", "ntfsinfo", &format!("{drive}\\")]),
            "Windows boundary tool volume identity",
            remaining_discovery_time(deadline)?,
        )?;
        let serial_token = volume_output
            .lines()
            .find(|line| line.contains("Volume Serial Number"))
            .and_then(|line| {
                line.split_ascii_whitespace().rev().find_map(|value| {
                    value
                        .strip_prefix("0x")
                        .or_else(|| value.strip_prefix("0X"))
                })
            })
            .filter(|value| {
                !value.is_empty()
                    && value.len() <= 16
                    && value.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            .ok_or_else(|| {
                "FF-WP010-E-PRODUCER-TOOL: malformed fsutil volume identity".to_owned()
            })?;
        let volume = u64::from_str_radix(serial_token, 16)
            .map_err(|_| "FF-WP010-E-PRODUCER-TOOL: invalid volume serial".to_owned())?;
        Ok(format!("windows:{volume}:{file_id}"))
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::metadata(path)
            .map_err(|error| format!("FF-WP010-E-PRODUCER-TOOL: {}: {error}", path.display()))?;
        Ok(format!("{}:{}", metadata.dev(), metadata.ino()))
    }
}

fn capture_normalized_command(
    command: &mut Command,
    label: &str,
    timeout: Duration,
) -> Result<String, String> {
    let output = execute_bounded_command_with_limit(command, label, timeout, MAX_TOOL_PROBE_BYTES)?;
    if !output.status.success() || output.stdout_truncated || output.stderr_truncated {
        return Err(format!(
            "FF-WP010-E-PRODUCER-TOOL: {label}: bounded probe failed with {:?}",
            output.status.code()
        ));
    }
    let mut bytes = output.stdout;
    if bytes.is_empty() {
        bytes = output.stderr;
    } else if !output.stderr.is_empty() {
        bytes.extend_from_slice(b"\n--stderr--\n");
        bytes.extend_from_slice(&output.stderr);
    }
    normalize_probe_output(&bytes)
}

fn normalize_probe_output(bytes: &[u8]) -> Result<String, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "FF-WP010-E-PRODUCER-TOOL: probe output is not UTF-8".to_owned())?;
    let normalized = text
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if normalized.is_empty() || normalized.len() > MAX_TOOL_PROBE_BYTES {
        return Err("FF-WP010-E-PRODUCER-TOOL: normalized probe output is invalid".to_owned());
    }
    Ok(normalized)
}

fn listed_component(output: &str, mode: char, name: &str) -> bool {
    output.lines().any(|line| {
        let mut fields = line.split_ascii_whitespace();
        fields.next().is_some_and(|flags| flags.contains(mode)) && fields.next() == Some(name)
    })
}

fn media_probe_projection(
    is_ffmpeg: bool,
    mut run: impl FnMut(&[&str], &str) -> Result<String, String>,
) -> Result<(String, String, String, Vec<String>), String> {
    let version = run(&["-version"], "version")?;
    let normalized_version = version
        .lines()
        .next()
        .ok_or_else(|| "FF-WP010-E-PRODUCER-TOOL: missing version line".to_owned())?
        .to_owned();
    let mut normalized_probe = run(&["-hide_banner", "-h", "full"], "help")?;
    let protocols = run(&["-hide_banner", "-protocols"], "protocols")?;
    let mut capabilities = Vec::new();
    if is_ffmpeg {
        let demuxers = run(&["-hide_banner", "-demuxers"], "demuxers")?;
        let muxers = run(&["-hide_banner", "-muxers"], "muxers")?;
        for name in ["aac", "h264"] {
            if listed_component(&demuxers, 'D', name) {
                capabilities.push(format!("demuxer:{name}"));
            }
        }
        for name in ["matroska", "mp4"] {
            if listed_component(&muxers, 'E', name) {
                capabilities.push(format!("muxer:{name}"));
            }
        }
        if normalized_probe.contains("-progress <url>")
            || normalized_probe
                .contains("-progress url write program-readable progress information")
        {
            capabilities.push("progress".to_owned());
        }
        if (normalized_probe.contains("-c[:<stream_spec>] <codec>")
            || normalized_probe.contains("-c codec codec name"))
            && (normalized_probe.contains("'copy' to copy stream without reencoding")
                || normalized_probe.contains("'copy' to copy stream"))
        {
            capabilities.push("stream_copy".to_owned());
        }
        normalized_probe.push_str("\n--demuxers--\n");
        normalized_probe.push_str(&demuxers);
        normalized_probe.push_str("\n--muxers--\n");
        normalized_probe.push_str(&muxers);
    } else {
        if (normalized_probe.contains("-output_format <format>")
            || normalized_probe.contains("-output_format format set the output printing format"))
            && normalized_probe.to_ascii_lowercase().contains("json")
        {
            capabilities.push("json_output".to_owned());
        }
        if normalized_probe.contains("-show_streams") {
            capabilities.push("stream_metadata".to_owned());
        }
    }
    if protocols.lines().any(|line| line.trim() == "fd") {
        capabilities.push("protocol:fd".to_owned());
    }
    normalized_probe.push_str("\n--protocols--\n");
    normalized_probe.push_str(&protocols);
    capabilities.sort();
    capabilities.dedup();
    if capabilities.is_empty() {
        return Err("FF-WP010-E-PRODUCER-TOOL: no capabilities observed".to_owned());
    }
    Ok((
        normalized_version,
        sha256(version.as_bytes()),
        sha256(normalized_probe.as_bytes()),
        capabilities,
    ))
}

fn observe_windows_boundary_tool(
    role: ProducerBoundaryToolRoleV1,
    path: &Path,
    version_arguments: &[&str],
    deadline: Instant,
) -> Result<ProducerBoundaryToolIdentityV1, String> {
    let _remaining = remaining_discovery_time(deadline)?;
    let canonical = fs::canonicalize(path)
        .map_err(|error| format!("FF-WP010-E-PRODUCER-TOOL: {}: {error}", path.display()))?;
    let _remaining = remaining_discovery_time(deadline)?;
    let (size_bytes, content_sha256) = hash_file_bounded_until(&canonical, deadline)?;
    let file_identity = host_file_identity(&canonical, deadline)?;
    let normalized = capture_normalized_command(
        Command::new(path).args(version_arguments),
        "Windows boundary tool version",
        remaining_discovery_time(deadline)?,
    )?;
    let normalized_version = normalized
        .lines()
        .next()
        .ok_or_else(|| "FF-WP010-E-PRODUCER-TOOL: missing version line".to_owned())?
        .to_owned();
    if role == ProducerBoundaryToolRoleV1::Cargo && !normalized_version.starts_with("cargo ") {
        return Err(
            "FF-WP010-E-PRODUCER-TOOL: discovered Cargo entrypoint did not execute Cargo"
                .to_owned(),
        );
    }
    Ok(ProducerBoundaryToolIdentityV1 {
        role,
        canonical_path: path_utf8(&canonical, "Windows boundary tool")?.to_owned(),
        size_bytes,
        content_sha256,
        file_identity,
        normalized_version,
        version_output_sha256: sha256(normalized.as_bytes()),
        normalized_probe_sha256: None,
        capabilities: Vec::new(),
    })
}

fn observe_windows_media_tool(
    role: ProducerBoundaryToolRoleV1,
    path: &Path,
    is_ffmpeg: bool,
    deadline: Instant,
) -> Result<ProducerBoundaryToolIdentityV1, String> {
    let _remaining = remaining_discovery_time(deadline)?;
    let canonical = fs::canonicalize(path)
        .map_err(|error| format!("FF-WP010-E-PRODUCER-TOOL: {}: {error}", path.display()))?;
    let _remaining = remaining_discovery_time(deadline)?;
    let (size_bytes, content_sha256) = hash_file_bounded_until(&canonical, deadline)?;
    let file_identity = host_file_identity(&canonical, deadline)?;
    let (normalized_version, version_output_sha256, normalized_probe_sha256, capabilities) =
        media_probe_projection(is_ffmpeg, |arguments, phase| {
            capture_normalized_command(
                Command::new(&canonical).args(arguments),
                &format!("Windows media {phase}"),
                remaining_discovery_time(deadline)?,
            )
        })?;
    Ok(ProducerBoundaryToolIdentityV1 {
        role,
        canonical_path: path_utf8(&canonical, "Windows media tool")?.to_owned(),
        size_bytes,
        content_sha256,
        file_identity,
        normalized_version,
        version_output_sha256,
        normalized_probe_sha256: Some(normalized_probe_sha256),
        capabilities,
    })
}

fn linux_unc_path(distro: &str, linux_path: &str) -> Result<PathBuf, String> {
    validate_linux_absolute_path(linux_path, "Linux boundary tool")?;
    let relative = linux_path.trim_start_matches('/').replace('/', "\\");
    Ok(PathBuf::from(format!(
        r"\\wsl.localhost\{distro}\{relative}"
    )))
}

fn linux_canonical_tool_path(wsl: &Path, distro: &str, path: &str) -> Result<String, String> {
    let deadline = Instant::now()
        .checked_add(PRODUCER_DISCOVERY_TIMEOUT)
        .ok_or_else(|| "FF-WP010-E-PRODUCER-TOOL-DEADLINE: deadline overflow".to_owned())?;
    linux_canonical_tool_path_until(wsl, distro, path, deadline)
}

fn linux_canonical_tool_path_until(
    wsl: &Path,
    distro: &str,
    path: &str,
    deadline: Instant,
) -> Result<String, String> {
    let canonical = capture_bounded_command(
        Command::new(wsl).args(["-d", distro, "--exec", "/usr/bin/readlink", "-f", path]),
        "Linux boundary tool canonical path",
        remaining_discovery_time(deadline)?,
    )?;
    validate_linux_absolute_path(&canonical, "canonical Linux boundary tool")?;
    Ok(canonical)
}

fn linux_tool_command(
    wsl: &Path,
    distro: &str,
    home: &str,
    program: &str,
    arguments: &[&str],
) -> Command {
    let mut command = Command::new(wsl);
    command.args([
        "-d",
        distro,
        "--exec",
        "/usr/bin/env",
        "-i",
        &format!("HOME={home}"),
        &format!(
            "PATH={home}/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
        ),
        "LANG=C.UTF-8",
        "LC_ALL=C.UTF-8",
        program,
    ]);
    command.args(arguments);
    command
}

fn linux_file_identity(
    wsl: &Path,
    distro: &str,
    path: &str,
    deadline: Instant,
) -> Result<String, String> {
    let observed = capture_bounded_command(
        Command::new(wsl).args([
            "-d",
            distro,
            "--exec",
            "/usr/bin/stat",
            "-Lc",
            "%d:%i",
            path,
        ]),
        "Linux boundary tool file identity",
        remaining_discovery_time(deadline)?,
    )?;
    let (device, inode) = observed
        .split_once(':')
        .ok_or_else(|| "FF-WP010-E-PRODUCER-TOOL: malformed Linux file identity".to_owned())?;
    if device.parse::<u64>().is_err() || inode.parse::<u64>().is_err() || inode == "0" {
        return Err("FF-WP010-E-PRODUCER-TOOL: invalid Linux file identity".to_owned());
    }
    Ok(format!("unix:{device}:{inode}"))
}

fn observe_linux_boundary_tool(
    wsl: &Path,
    distro: &str,
    home: &str,
    role: ProducerBoundaryToolRoleV1,
    path: &str,
    version_arguments: &[&str],
    deadline: Instant,
) -> Result<ProducerBoundaryToolIdentityV1, String> {
    observe_linux_boundary_tool_from_entrypoint(
        wsl,
        distro,
        home,
        role,
        path,
        path,
        version_arguments,
        deadline,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "Linux Cargo has a lexical rustup shim entrypoint with a separately measured canonical target"
)]
fn observe_linux_boundary_tool_from_entrypoint(
    wsl: &Path,
    distro: &str,
    home: &str,
    role: ProducerBoundaryToolRoleV1,
    path: &str,
    entrypoint: &str,
    version_arguments: &[&str],
    deadline: Instant,
) -> Result<ProducerBoundaryToolIdentityV1, String> {
    let host_path = linux_unc_path(distro, path)?;
    let (size_bytes, content_sha256) = hash_file_bounded_until(&host_path, deadline)?;
    let file_identity = linux_file_identity(wsl, distro, path, deadline)?;
    let normalized = capture_normalized_command(
        &mut linux_tool_command(wsl, distro, home, entrypoint, version_arguments),
        "Linux boundary tool version",
        remaining_discovery_time(deadline)?,
    )?;
    let normalized_version = normalized
        .lines()
        .next()
        .ok_or_else(|| "FF-WP010-E-PRODUCER-TOOL: missing Linux version line".to_owned())?
        .to_owned();
    if role == ProducerBoundaryToolRoleV1::Cargo && !normalized_version.starts_with("cargo ") {
        return Err(
            "FF-WP010-E-PRODUCER-TOOL: Linux Cargo entrypoint did not execute Cargo".to_owned(),
        );
    }
    Ok(ProducerBoundaryToolIdentityV1 {
        role,
        canonical_path: path.to_owned(),
        size_bytes,
        content_sha256,
        file_identity,
        normalized_version,
        version_output_sha256: sha256(normalized.as_bytes()),
        normalized_probe_sha256: None,
        capabilities: Vec::new(),
    })
}

fn observe_linux_media_tool(
    wsl: &Path,
    distro: &str,
    home: &str,
    role: ProducerBoundaryToolRoleV1,
    path: &str,
    is_ffmpeg: bool,
    deadline: Instant,
) -> Result<ProducerBoundaryToolIdentityV1, String> {
    let host_path = linux_unc_path(distro, path)?;
    let (size_bytes, content_sha256) = hash_file_bounded_until(&host_path, deadline)?;
    let file_identity = linux_file_identity(wsl, distro, path, deadline)?;
    let (normalized_version, version_output_sha256, normalized_probe_sha256, capabilities) =
        media_probe_projection(is_ffmpeg, |arguments, phase| {
            capture_normalized_command(
                &mut linux_tool_command(wsl, distro, home, path, arguments),
                &format!("Linux media {phase}"),
                remaining_discovery_time(deadline)?,
            )
        })?;
    Ok(ProducerBoundaryToolIdentityV1 {
        role,
        canonical_path: path.to_owned(),
        size_bytes,
        content_sha256,
        file_identity,
        normalized_version,
        version_output_sha256,
        normalized_probe_sha256: Some(normalized_probe_sha256),
        capabilities,
    })
}

fn validate_and_aggregate(
    reports: &[PlatformProofReportV2],
    context: &ValidationContext<'_>,
) -> Result<CrossPlatformProofV1, String> {
    if reports.len() != 2 {
        return Err(
            "FF-WP010-E-MISSING-PLATFORM: exactly two platform reports are required".to_owned(),
        );
    }
    let first = &reports[0];
    let mut platforms = BTreeSet::new();
    let mut request_contract_sha256_by_platform = BTreeMap::new();
    let mut residuals = BTreeSet::new();
    for report in reports {
        validate_report(report, context)?;
        if !platforms.insert(report.platform) {
            return Err("FF-WP010-E-MISSING-PLATFORM: duplicate platform report".to_owned());
        }
        if report.source_commit != first.source_commit {
            return Err(
                "FF-WP010-E-SOURCE-MISMATCH: platform reports use different source commits"
                    .to_owned(),
            );
        }
        if report.operation_plan_sha256 != first.operation_plan_sha256 {
            return Err(
                "FF-WP010-E-PLAN-MISMATCH: platform reports use different operation-plan identities"
                    .to_owned(),
            );
        }
        if report.fixture_contract_sha256 != first.fixture_contract_sha256
            || report.fixture_payload_sha256 != first.fixture_payload_sha256
        {
            return Err(
                "FF-WP010-E-FIXTURE-MISMATCH: platform reports use different fixtures".to_owned(),
            );
        }
        if report.limits_sha256 != first.limits_sha256 {
            return Err(
                "FF-WP010-E-LIMITS-MISMATCH: platform reports use different governed limits"
                    .to_owned(),
            );
        }
        request_contract_sha256_by_platform
            .insert(report.platform, report.request_contract_sha256.clone());
        residuals.extend(report.residual_uncertainty.iter().cloned());
    }
    if platforms != BTreeSet::from([ProofPlatform::WindowsX86_64, ProofPlatform::LinuxX86_64]) {
        return Err(
            "FF-WP010-E-MISSING-PLATFORM: Windows and Linux-native reports are required".to_owned(),
        );
    }
    Ok(CrossPlatformProofV1 {
        schema_id: AGGREGATE_SCHEMA_ID.to_owned(),
        source_commit: first.source_commit.clone(),
        request_contract_sha256_by_platform,
        operation_plan_sha256: first.operation_plan_sha256.clone(),
        fixture_contract_sha256: first.fixture_contract_sha256.clone(),
        fixture_payload_sha256: first.fixture_payload_sha256.clone(),
        limits_sha256: first.limits_sha256.clone(),
        platforms: platforms.into_iter().collect(),
        residual_uncertainty: residuals.into_iter().collect(),
    })
}

fn validate_report(
    report: &PlatformProofReportV2,
    context: &ValidationContext<'_>,
) -> Result<(), String> {
    if report.schema_id != PLATFORM_SCHEMA_ID {
        return Err("FF-WP010-E-REPORT-SCHEMA: unsupported platform report schema".to_owned());
    }
    if report.source_dirty || report.source_commit != context.source_commit {
        return Err(
            "FF-WP010-E-STALE-REPORT: report does not bind the current clean source".to_owned(),
        );
    }
    if !is_git_commit(&report.source_commit) {
        return Err("FF-WP010-E-SOURCE-COMMIT: invalid report source identity".to_owned());
    }
    bounded_nonempty("host_kernel", &report.host_kernel)?;
    validate_tool_identity(report.platform, "ffmpeg", &report.ffmpeg)?;
    validate_tool_identity(report.platform, "ffprobe", &report.ffprobe)?;
    validate_fixture_tool_identities(report)?;
    if report.ffmpeg.canonical_path == report.ffprobe.canonical_path
        || report.ffmpeg.file_identity == report.ffprobe.file_identity
        || report.ffmpeg.content_sha256 == report.ffprobe.content_sha256
    {
        return Err(
            "FF-WP010-E-TOOL-MANIFEST: ffmpeg and ffprobe identities must be distinct".to_owned(),
        );
    }
    for digest in [
        &report.ffmpeg.content_sha256,
        &report.ffmpeg.version_output_sha256,
        &report.ffmpeg.normalized_probe_sha256,
        &report.ffprobe.content_sha256,
        &report.ffprobe.version_output_sha256,
        &report.ffprobe.normalized_probe_sha256,
        &report.request_contract_sha256,
        &report.operation_plan_sha256,
        &report.argument_vector_sha256,
        &report.fixture_contract_sha256,
        &report.fixture_payload_sha256,
        &report.limits_sha256,
        &report.lifecycle_timeline_sha256,
        &report.direct_wait_receipt_sha256,
        &report.forced_lifecycle_timeline_sha256,
        &report.forced_wait_receipt_sha256,
        &report.output_facts_sha256,
        &report.diagnostic_tail_sha256,
        &report.progress_transcript_sha256,
    ] {
        if !is_sha256(digest) {
            return Err("FF-WP010-E-DIGEST: invalid SHA-256 identity".to_owned());
        }
    }
    if report
        .fixture_tools
        .iter()
        .any(|identity| !is_sha256(&identity.content_sha256))
    {
        return Err("FF-WP010-E-DIGEST: invalid fixture-tool SHA-256 identity".to_owned());
    }
    validate_raw_evidence(report)?;
    if report.fixture_contract_sha256 != context.fixture_contract_sha256 {
        return Err("FF-WP010-E-PRODUCER-ORACLE: producer fixture digest does not match independently hashed canonical bytes".to_owned());
    }
    if report.residual_uncertainty.is_empty() || report.residual_uncertainty.len() > MAX_RESIDUALS {
        return Err("FF-WP010-E-RESIDUAL: residual uncertainty is missing or unbounded".to_owned());
    }
    for residual in &report.residual_uncertainty {
        bounded_nonempty("residual_uncertainty", residual)?;
        if residual.len() > MAX_RESIDUAL_BYTES {
            return Err("FF-WP010-E-RESIDUAL: residual uncertainty entry is too long".to_owned());
        }
    }
    let behavior = &report.behavior;
    if !behavior.direct_child_wait_observed || !behavior.direct_child_reaped {
        return Err(
            "FF-WP010-E-DIRECT-WAIT: direct-child wait and reap evidence is required".to_owned(),
        );
    }
    if !(behavior.successful_exit_observed
        && behavior.forced_cancellation_observed
        && behavior.bounded_progress_observed
        && behavior.bounded_stderr_observed
        && behavior.output_validated_by_ffprobe)
    {
        return Err(
            "FF-WP010-E-BEHAVIOR-MUTATION: a required executed behavior is missing".to_owned(),
        );
    }
    match report.platform {
        ProofPlatform::WindowsX86_64 => validate_windows(report),
        ProofPlatform::LinuxX86_64 => validate_linux(report),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "WP-FF-010 independently reconstructs every report digest and exposed behavior in one fail-closed boundary"
)]
fn validate_raw_evidence(report: &PlatformProofReportV2) -> Result<(), String> {
    let request: RequestEvidenceV1 = parse_canonical_json_typed(
        "request_contract_canonical_json",
        &report.request_contract_canonical_json,
    )?;
    let operation: RequestOperationPlanV1 = parse_canonical_json_typed(
        "operation_plan_canonical_json",
        &report.operation_plan_canonical_json,
    )?;
    if sha256_domain(
        "ff.ffmpeg-request-canonical-json@1",
        report.request_contract_canonical_json.as_bytes(),
    ) != report.request_contract_sha256
        || sha256_domain(
            "ff.ffmpeg-operation-canonical-json@1",
            report.operation_plan_canonical_json.as_bytes(),
        ) != report.operation_plan_sha256
    {
        return Err(
            "FF-WP010-E-RAW-DIGEST: request or operation canonical bytes do not match their digest"
                .to_owned(),
        );
    }
    if request.schema_id != "ff.ffmpeg-supervision@1"
        || request.version != (RequestSchemaVersionV1 { major: 1, minor: 0 })
        || request.operation_plan_sha256 != report.operation_plan_sha256
        || request.operation != operation
    {
        return Err(
            "FF-WP010-E-RAW-REQUEST: request and operation evidence are not mutually bound"
                .to_owned(),
        );
    }
    validate_request_tool_binding(report, &request)?;
    validate_request_semantics(&request, report.platform)?;
    let request_value = serde_json::to_value(&request)
        .map_err(|error| format!("FF-WP010-E-RAW-REQUEST: {error}"))?;
    let expected_arguments = reconstruct_arguments(&request_value, report.platform)?;

    if report.argument_vector.is_empty() || report.argument_vector.len() > MAX_ARGUMENTS {
        return Err("FF-WP010-E-RAW-ARGUMENTS: argument count is outside bounds".to_owned());
    }
    let argument_bytes = report
        .argument_vector
        .iter()
        .try_fold(0_usize, |total, value| {
            bounded_nonempty("argument", value)?;
            total
                .checked_add(value.len())
                .ok_or_else(|| "FF-WP010-E-RAW-ARGUMENTS: byte count overflow".to_owned())
        })?;
    if argument_bytes > MAX_ARGUMENT_BYTES
        || report
            .argument_vector
            .iter()
            .any(|value| value.contains('\n') || value.contains('\r') || value.contains('\0'))
        || report.argument_vector != expected_arguments
    {
        return Err(
            "FF-WP010-E-RAW-ARGUMENTS: vector differs from exact governed stream-copy arguments"
                .to_owned(),
        );
    }
    let argument_json = serde_json::to_vec(&report.argument_vector)
        .map_err(|error| format!("FF-WP010-E-RAW-ARGUMENTS: {error}"))?;
    if sha256(&argument_json) != report.argument_vector_sha256 {
        return Err("FF-WP010-E-RAW-DIGEST: argument vector digest mismatch".to_owned());
    }
    if report.bound_runtime_projection_id != BOUND_RUNTIME_PROJECTION_ID {
        return Err("FF-WP010-E-RAW-ARGUMENTS: bound-runtime projection ID mismatch".to_owned());
    }
    let bound_projection = serde_json::to_vec(&BoundRuntimeProjection {
        request_contract_sha256: &report.request_contract_sha256,
        operation_plan_sha256: &report.operation_plan_sha256,
        arguments: &expected_arguments,
    })
    .map_err(|error| format!("FF-WP010-E-RAW-ARGUMENTS: bound projection: {error}"))?;
    if sha256_domain(BOUND_RUNTIME_PROJECTION_ID, &bound_projection) != report.bound_runtime_sha256
    {
        return Err("FF-WP010-E-RAW-DIGEST: bound-runtime projection mismatch".to_owned());
    }

    let fixture_payloads: Vec<String> = parse_canonical_json_typed(
        "fixture_payload_canonical_json",
        &report.fixture_payload_canonical_json,
    )?;
    if fixture_payloads.len() != 2 || fixture_payloads.iter().any(|digest| !is_sha256(digest)) {
        return Err(
            "FF-WP010-E-RAW-FIXTURE: exactly two payload identities are required".to_owned(),
        );
    }
    require_plain_sha_match(
        "fixture payload",
        &report.fixture_payload_canonical_json,
        &report.fixture_payload_sha256,
    )?;
    let request_stream_maps = request_stream_maps(&request_value)?;
    let request_payloads = request_stream_maps
        .iter()
        .map(|mapping| mapping.source_payload_sha256.clone())
        .collect::<Vec<_>>();
    if fixture_payloads != request_payloads {
        return Err(
            "FF-WP010-E-RAW-FIXTURE: fixture payload identities diverge from request stream maps"
                .to_owned(),
        );
    }

    let limits: LimitsEvidenceV1 =
        parse_canonical_json_typed("limits_canonical_json", &report.limits_canonical_json)?;
    validate_limits(&limits)?;
    require_plain_sha_match(
        "limits",
        &report.limits_canonical_json,
        &report.limits_sha256,
    )?;
    if request.limits != limits {
        return Err("FF-WP010-E-RAW-LIMITS: request/report limits diverge".to_owned());
    }

    let lifecycle: Vec<LifecycleObservationV1> = parse_canonical_json_typed(
        "lifecycle_timeline_canonical_json",
        &report.lifecycle_timeline_canonical_json,
    )?;
    validate_lifecycle(&lifecycle)?;
    require_plain_sha_match(
        "lifecycle timeline",
        &report.lifecycle_timeline_canonical_json,
        &report.lifecycle_timeline_sha256,
    )?;

    let wait: DirectWaitReceiptEvidenceV1 = parse_canonical_json_typed(
        "direct_wait_receipt_canonical_json",
        &report.direct_wait_receipt_canonical_json,
    )?;
    let wait_domain = format!(
        "exit={:?};signal={:?};windows_status_opaque={:?};forced={}",
        wait.exit_code, wait.signal, wait.windows_opaque_status, wait.forced_by_supervisor
    );
    if sha256(wait_domain.as_bytes()) != wait.supervision_wait_receipt_sha256
        || wait.supervision_wait_receipt_sha256 != report.direct_wait_receipt_sha256
    {
        return Err("FF-WP010-E-RAW-DIRECT-WAIT: wait receipt did not reconstruct".to_owned());
    }
    let successful_wait = wait.exit_code == Some(0)
        && wait.signal.is_none()
        && wait.windows_opaque_status.is_none()
        && !wait.forced_by_supervisor;

    let forced_lifecycle: Vec<LifecycleObservationV1> = parse_canonical_json_typed(
        "forced_lifecycle_timeline_canonical_json",
        &report.forced_lifecycle_timeline_canonical_json,
    )?;
    validate_forced_lifecycle(&forced_lifecycle)?;
    require_plain_sha_match(
        "forced lifecycle timeline",
        &report.forced_lifecycle_timeline_canonical_json,
        &report.forced_lifecycle_timeline_sha256,
    )?;
    let forced_wait: ForcedWaitReceiptEvidenceV1 = parse_canonical_json_typed(
        "forced_wait_receipt_canonical_json",
        &report.forced_wait_receipt_canonical_json,
    )?;
    let forced_wait_domain = format!(
        "exit={:?};signal={:?};windows_status_opaque={:?};forced={}",
        forced_wait.exit_code,
        forced_wait.signal,
        forced_wait.windows_opaque_status,
        forced_wait.forced_by_supervisor
    );
    if sha256(forced_wait_domain.as_bytes()) != forced_wait.supervision_wait_receipt_sha256
        || forced_wait.lifecycle_timeline_sha256 != report.forced_lifecycle_timeline_sha256
    {
        return Err(
            "FF-WP010-E-RAW-FORCED-WAIT: forced wait or lifecycle receipt did not reconstruct"
                .to_owned(),
        );
    }
    if sha256_domain(
        "ff.ffmpeg-forced-wait-receipt-canonical-json@1",
        report.forced_wait_receipt_canonical_json.as_bytes(),
    ) != report.forced_wait_receipt_sha256
    {
        return Err(
            "FF-WP010-E-RAW-FORCED-WAIT: versioned forced wait receipt digest mismatch".to_owned(),
        );
    }
    let forced_wait_reaped = validate_forced_wait_platform(report.platform, &forced_wait)?;
    validate_phase_deadlines(report, &limits, &lifecycle, &forced_lifecycle)?;

    let output: OutputFactsEvidenceV1 = parse_canonical_json_typed(
        "output_facts_canonical_json",
        &report.output_facts_canonical_json,
    )?;
    require_plain_sha_match(
        "output facts",
        &report.output_facts_canonical_json,
        &report.output_facts_sha256,
    )?;
    let RequestOperationPlanV1::StreamCopy(request_plan) = &request.operation;
    validate_output_facts(
        &output,
        &limits,
        &request_stream_maps,
        &request_plan.output_muxer,
    )?;

    let diagnostic_tail = decode_lower_hex(&report.diagnostic_tail_hex)?;
    if diagnostic_tail.len() as u64 > limits.stderr_tail_bytes
        || diagnostic_tail.len() as u64 != report.diagnostic_total_bytes
        || diagnostic_tail.len() as u64 > request.resources.stderr_pipe_bytes
        || sha256(&diagnostic_tail) != report.diagnostic_tail_sha256
        || report.diagnostic_loss != DiagnosticLossEvidenceV1::None
    {
        return Err(
            "FF-WP010-E-RAW-DIAGNOSTIC: diagnostic tail did not reconstruct within limits"
                .to_owned(),
        );
    }
    let bounded_stderr = report.diagnostic_total_bytes <= limits.stderr_max_total_bytes;
    let progress_transcript = decode_lower_hex(&report.progress_transcript_hex)?;
    if sha256(&progress_transcript) != report.progress_transcript_sha256
        || progress_transcript.len() > progress_transcript_capacity(&request.resources, &limits)?
    {
        return Err(
            "FF-WP010-E-RAW-PROGRESS: progress transcript identity or allocation mismatch"
                .to_owned(),
        );
    }
    let replayed_progress = replay_progress_transcript(&progress_transcript, &limits)?;
    let bounded_progress = replayed_progress == report.progress_observations;
    let forced_cancellation = forced_wait_reaped && validate_containment_observations(report)?;
    let behavior = &report.behavior;
    if behavior.direct_child_wait_observed != (successful_wait && forced_wait_reaped)
        || behavior.direct_child_reaped != (successful_wait && forced_wait_reaped)
        || behavior.successful_exit_observed != successful_wait
        || behavior.forced_cancellation_observed != forced_cancellation
        || behavior.bounded_progress_observed != bounded_progress
        || behavior.bounded_stderr_observed != bounded_stderr
        || !behavior.output_validated_by_ffprobe
    {
        return Err(
            "FF-WP010-E-BEHAVIOR-MUTATION: behavior declaration diverges from raw evidence"
                .to_owned(),
        );
    }
    Ok(())
}

fn parse_canonical_json_typed<T>(label: &str, raw: &str) -> Result<T, String>
where
    T: serde::de::DeserializeOwned + Serialize,
{
    if raw.is_empty()
        || raw.len() > MAX_CANONICAL_JSON_BYTES
        || raw.trim() != raw
        || raw.contains('\0')
    {
        return Err(format!("FF-WP010-E-RAW-BOUND: {label}"));
    }
    let parsed: T = serde_json::from_str(raw)
        .map_err(|error| format!("FF-WP010-E-RAW-JSON: {label}: {error}"))?;
    let canonical = serde_json::to_string(&parsed)
        .map_err(|error| format!("FF-WP010-E-RAW-JSON: {label}: {error}"))?;
    if canonical != raw {
        return Err(format!(
            "FF-WP010-E-RAW-CANONICAL: {label} is not exact compact declared-order JSON"
        ));
    }
    Ok(parsed)
}

fn json_string_at<'a>(value: &'a serde_json::Value, path: &[&str]) -> Result<&'a str, String> {
    let mut current = value;
    for component in path {
        current = current.get(*component).ok_or_else(|| {
            format!(
                "FF-WP010-E-RAW-REQUEST: missing JSON path {}",
                path.join(".")
            )
        })?;
    }
    current.as_str().ok_or_else(|| {
        format!(
            "FF-WP010-E-RAW-REQUEST: JSON path {} is not a string",
            path.join(".")
        )
    })
}

fn validate_request_tool_binding(
    report: &PlatformProofReportV2,
    request: &RequestEvidenceV1,
) -> Result<(), String> {
    for (name, observed, requested, expected_kind) in [
        (
            "ffmpeg",
            &report.ffmpeg,
            &request.toolchain.ffmpeg,
            RequestExecutableKindV1::Ffmpeg,
        ),
        (
            "ffprobe",
            &report.ffprobe,
            &request.toolchain.ffprobe,
            RequestExecutableKindV1::Ffprobe,
        ),
    ] {
        if requested.kind != expected_kind
            || requested.absolute_path != observed.canonical_path
            || requested.file_identity != observed.file_identity
            || requested.content_sha256 != observed.content_sha256
            || requested.normalized_version != observed.version_line
            || requested.version_output_sha256 != observed.version_output_sha256
            || requested.capability_binding.normalized_probe_sha256
                != observed.normalized_probe_sha256
            || requested.capability_binding.capabilities != observed.capabilities
            || requested.capability_binding.executable_content_sha256 != observed.content_sha256
        {
            return Err(format!(
                "FF-WP010-E-RAW-TOOL-BINDING: {name} request identity diverges from report"
            ));
        }
        let expected_os = match report.platform {
            ProofPlatform::WindowsX86_64 => RequestOperatingSystemV1::Windows,
            ProofPlatform::LinuxX86_64 => RequestOperatingSystemV1::Linux,
        };
        if requested.host.operating_system != expected_os
            || requested.host.architecture != RequestArchitectureV1::X86_64
        {
            return Err(
                "FF-WP010-E-RAW-TOOL-BINDING: request host does not match report platform"
                    .to_owned(),
            );
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "the strict mirror deliberately validates the complete closed request semantic surface together"
)]
fn validate_request_semantics(
    request: &RequestEvidenceV1,
    platform: ProofPlatform,
) -> Result<(), String> {
    let expected_bindings = match platform {
        ProofPlatform::WindowsX86_64 => vec![
            RequestEnvironmentBindingV1::HostSystemRoot,
            RequestEnvironmentBindingV1::JobTemporaryDirectory,
        ],
        ProofPlatform::LinuxX86_64 => vec![
            RequestEnvironmentBindingV1::JobTemporaryDirectory,
            RequestEnvironmentBindingV1::LocaleC,
        ],
    };
    let working_directory_valid = match platform {
        ProofPlatform::WindowsX86_64 => is_windows_absolute_path(&request.working_directory),
        ProofPlatform::LinuxX86_64 => is_linux_absolute_path(&request.working_directory),
    };
    let RequestOperationPlanV1::StreamCopy(plan) = &request.operation;
    let media_paths = plan
        .inputs
        .iter()
        .map(|input| input.path.as_str())
        .chain(std::iter::once(plan.output_path.as_str()))
        .collect::<Vec<_>>();
    let paths_do_not_alias = media_paths.iter().enumerate().all(|(index, left)| {
        media_paths
            .iter()
            .skip(index + 1)
            .all(|right| !paths_alias(left, right, platform))
    });
    let media_paths_valid = !plan.inputs.is_empty()
        && plan.inputs.len() <= 32
        && plan
            .inputs
            .iter()
            .all(|input| safe_job_relative_path(&input.path, platform))
        && safe_job_relative_path(&plan.output_path, platform)
        && paths_do_not_alias;
    let claim = &request.resources.claim;
    let required_resources_nonzero = claim.memory_bytes > 0
        && claim.disk_read_bytes_in_flight > 0
        && claim.disk_write_bytes_in_flight > 0
        && claim.ffmpeg_processes == 1
        && claim.ffmpeg_cpu_threads > 0;
    let pipe_bytes = request
        .resources
        .progress_pipe_bytes
        .checked_add(request.resources.stderr_pipe_bytes);
    if !valid_stable_id(&request.request_id, "request_")
        || !valid_stable_id(&request.job_id, "job_")
        || request.allowed_protocols != [RequestProtocolV1::File]
        || request.allowed_input_mechanisms != [RequestInputMechanismV1::AuditedElementaryFile]
        || request.environment.inherit_parent
        || request.environment.trusted_bindings != expected_bindings
        || !working_directory_valid
        || !media_paths_valid
        || !valid_component(&plan.output_muxer, 128)
        || request.io.progress_channel != RequestProgressChannelV1::StdoutPipeOne
        || request.io.diagnostic_channel != RequestDiagnosticChannelV1::StderrTail
        || !request.io.output_is_job_scoped_file
        || !request.io.stdin_disabled
        || request.resources.resource_contract_schema_id != "ff.resource-vector@1"
        || request.resources.byte_credit_contract_schema_id != "ff.byte-credit@1"
        || request.resources.pipe_stage != RequestByteCreditStageV1::FfmpegPipe
        || request.resources.claim.ffmpeg_processes != 1
        || request.resources.progress_pipe_bytes == 0
        || request.resources.stderr_pipe_bytes == 0
        || !required_resources_nonzero
        || pipe_bytes.is_none_or(|value| {
            value > claim.memory_bytes || value > request.limits.pipe_allocation_bytes
        })
    {
        return Err("FF-WP010-E-RAW-REQUEST: closed request semantics are invalid".to_owned());
    }
    for executable in [&request.toolchain.ffmpeg, &request.toolchain.ffprobe] {
        let capabilities = &executable.capability_binding.capabilities;
        if !is_sha256(&executable.content_sha256)
            || !is_sha256(&executable.version_output_sha256)
            || !is_sha256(&executable.capability_binding.normalized_probe_sha256)
            || capabilities.is_empty()
            || capabilities.len() > 128
            || capabilities.windows(2).any(|pair| pair[0] >= pair[1])
            || capabilities
                .iter()
                .any(|value| !valid_component(value, 128))
        {
            return Err(
                "FF-WP010-E-RAW-TOOL-BINDING: invalid request capability evidence".to_owned(),
            );
        }
    }
    let mut required_ffmpeg = BTreeSet::from(["progress".to_owned(), "stream_copy".to_owned()]);
    for input in &plan.inputs {
        required_ffmpeg.insert(match input.demuxer {
            RequestInputDemuxerV1::AacAdts => "demuxer:aac".to_owned(),
            RequestInputDemuxerV1::H264AnnexB => "demuxer:h264".to_owned(),
        });
    }
    required_ffmpeg.insert(format!("muxer:{}", plan.output_muxer));
    let mut required_ffprobe =
        BTreeSet::from(["json_output".to_owned(), "stream_metadata".to_owned()]);
    if platform == ProofPlatform::LinuxX86_64 {
        required_ffmpeg.insert("protocol:fd".to_owned());
        required_ffprobe.insert("protocol:fd".to_owned());
    }
    for (name, capabilities, required) in [
        (
            "ffmpeg",
            &request.toolchain.ffmpeg.capability_binding.capabilities,
            required_ffmpeg,
        ),
        (
            "ffprobe",
            &request.toolchain.ffprobe.capability_binding.capabilities,
            required_ffprobe,
        ),
    ] {
        if !required
            .iter()
            .all(|capability| capabilities.binary_search(capability).is_ok())
        {
            return Err(format!(
                "FF-WP010-E-RAW-TOOL-BINDING: {name} omits an operation-derived capability"
            ));
        }
    }
    Ok(())
}

fn safe_job_relative_path(value: &str, platform: ProofPlatform) -> bool {
    if value.is_empty()
        || value.len() > 4_096
        || value
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
        || value.starts_with('/')
        || value.starts_with('\\')
        || value.starts_with('-')
        || value.contains(':')
        || contains_dot_segment(value)
        || value.ends_with(['/', '\\'])
        || value.contains("//")
        || value.contains("\\\\")
    {
        return false;
    }
    platform != ProofPlatform::WindowsX86_64 || !windows_path_has_unsafe_component(value, false)
}

fn contains_dot_segment(value: &str) -> bool {
    value
        .split(['/', '\\'])
        .any(|segment| matches!(segment, "." | ".."))
}

fn windows_component_is_reserved(component: &str) -> bool {
    if component.ends_with(['.', ' ']) {
        return true;
    }
    let stem = component
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
    ) || stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
        .is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
}

fn windows_path_has_unsafe_component(value: &str, allow_drive_prefix: bool) -> bool {
    let normalized = value.replace('/', "\\");
    if normalized.starts_with("\\\\?\\")
        || normalized.starts_with("\\\\.\\")
        || normalized.starts_with("\\??\\")
    {
        return true;
    }
    if let Some(unc_path) = normalized.strip_prefix("\\\\") {
        let unc_components = unc_path.split('\\').collect::<Vec<_>>();
        if unc_components.len() < 2 || unc_components.iter().any(|component| component.is_empty()) {
            return true;
        }
    }
    let drive_absolute = allow_drive_prefix
        && normalized.len() >= 3
        && normalized.as_bytes()[0].is_ascii_alphabetic()
        && normalized.as_bytes()[1] == b':'
        && normalized.as_bytes()[2] == b'\\';
    let mut first_component = true;
    for component in normalized
        .split('\\')
        .filter(|component| !component.is_empty())
    {
        let is_drive = drive_absolute
            && first_component
            && component.len() == 2
            && component.as_bytes()[0].is_ascii_alphabetic()
            && component.as_bytes()[1] == b':';
        first_component = false;
        if !is_drive
            && (component
                .chars()
                .any(|character| matches!(character, ':' | '<' | '>' | '"' | '|' | '?' | '*'))
                || windows_component_is_reserved(component))
        {
            return true;
        }
    }
    false
}

fn paths_alias(left: &str, right: &str, platform: ProofPlatform) -> bool {
    match platform {
        ProofPlatform::WindowsX86_64 => left
            .replace('/', "\\")
            .eq_ignore_ascii_case(&right.replace('/', "\\")),
        ProofPlatform::LinuxX86_64 => left == right,
    }
}

fn valid_stable_id(value: &str, prefix: &str) -> bool {
    value.starts_with(prefix)
        && value.len() > prefix.len()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-' | b'.')
        })
}

fn require_plain_sha_match(label: &str, raw: &str, expected: &str) -> Result<(), String> {
    if sha256(raw.as_bytes()) != expected {
        return Err(format!("FF-WP010-E-RAW-DIGEST: {label} digest mismatch"));
    }
    Ok(())
}

fn validate_limits(limits: &LimitsEvidenceV1) -> Result<(), String> {
    let time_limits = [
        limits.startup_timeout_millis,
        limits.execution_timeout_millis,
        limits.graceful_stop_timeout_millis,
        limits.forced_kill_timeout_millis,
        limits.reap_timeout_millis,
        limits.ffprobe_timeout_millis,
        limits.progress_max_silence_millis,
        limits.consumer_stall_timeout_millis,
    ];
    if time_limits
        .iter()
        .any(|value| *value == 0 || *value > 3_600_000)
        || limits.progress_max_records == 0
        || limits.progress_max_records > 1_000_000
        || limits.progress_max_total_bytes == 0
        || limits.progress_max_total_bytes > 64 * 1_024 * 1_024
        || limits.progress_max_record_bytes == 0
        || limits.progress_max_record_bytes > 64 * 1_024
        || limits.progress_max_field_bytes == 0
        || limits.progress_max_field_bytes > 4 * 1_024
        || limits.progress_max_parser_steps == 0
        || limits.progress_max_parser_steps > 4_000_000
        || limits.stderr_max_total_bytes == 0
        || limits.stderr_max_total_bytes > 256 * 1_024 * 1_024
        || limits.stderr_tail_bytes == 0
        || limits.stderr_tail_bytes > 256 * 1_024
        || limits.pipe_allocation_bytes == 0
        || limits.pipe_allocation_bytes > 8 * 1_024 * 1_024
        || limits.output_max_streams == 0
        || limits.output_max_streams > 1_024
        || limits.output_max_duration_millis == 0
        || limits.output_max_duration_millis > 31 * 24 * 60 * 60 * 1_000
        || limits.output_max_file_size_bytes == 0
        || limits.output_max_file_size_bytes > 1_024_u64.pow(4)
        || limits.output_max_width == 0
        || limits.output_max_width > 32_768
        || limits.output_max_height == 0
        || limits.output_max_height > 32_768
        || limits.output_max_channels == 0
        || limits.output_max_channels > 256
        || limits.progress_max_field_bytes > limits.progress_max_record_bytes
        || limits.progress_max_record_bytes > limits.progress_max_total_bytes
        || limits.stderr_tail_bytes > limits.stderr_max_total_bytes
    {
        return Err("FF-WP010-E-RAW-LIMITS: invalid or unbounded limits".to_owned());
    }
    Ok(())
}

fn validate_lifecycle(lifecycle: &[LifecycleObservationV1]) -> Result<(), String> {
    let expected = [
        LifecycleStateV1::Spawned,
        LifecycleStateV1::Running,
        LifecycleStateV1::Reaped,
        LifecycleStateV1::Validated,
    ];
    if lifecycle.len() != expected.len() {
        return Err("FF-WP010-E-RAW-LIFECYCLE: timeline length is invalid".to_owned());
    }
    let mut previous_millis = 0;
    for (index, event) in lifecycle.iter().enumerate() {
        if event.sequence != u64::try_from(index + 1).unwrap_or(u64::MAX)
            || (index > 0 && event.monotonic_millis < previous_millis)
            || event.state != expected[index]
        {
            return Err(
                "FF-WP010-E-RAW-LIFECYCLE: timeline is noncontiguous, reordered, or unreaped"
                    .to_owned(),
            );
        }
        previous_millis = event.monotonic_millis;
    }
    Ok(())
}

fn validate_forced_lifecycle(lifecycle: &[LifecycleObservationV1]) -> Result<(), String> {
    let expected = [
        LifecycleStateV1::Spawned,
        LifecycleStateV1::Running,
        LifecycleStateV1::ForcedKillRequested,
        LifecycleStateV1::Reaped,
    ];
    if lifecycle.len() != expected.len() {
        return Err("FF-WP010-E-RAW-FORCED-LIFECYCLE: timeline length is invalid".to_owned());
    }
    let mut previous_millis = 0;
    for (index, event) in lifecycle.iter().enumerate() {
        if event.sequence != u64::try_from(index + 1).unwrap_or(u64::MAX)
            || (index > 0 && event.monotonic_millis < previous_millis)
            || event.state != expected[index]
        {
            return Err(
                "FF-WP010-E-RAW-FORCED-LIFECYCLE: timeline is noncontiguous, reordered, duplicated, or unreaped"
                    .to_owned(),
            );
        }
        previous_millis = event.monotonic_millis;
    }
    Ok(())
}

fn validate_phase_deadlines(
    report: &PlatformProofReportV2,
    limits: &LimitsEvidenceV1,
    lifecycle: &[LifecycleObservationV1],
    forced_lifecycle: &[LifecycleObservationV1],
) -> Result<(), String> {
    let producer = report.producer_phase_limits;
    let observed = report.phase_deadlines;
    if producer
        != (ProducerPhaseLimitsV2 {
            fixture_probe_timeout_millis: 60_000,
            identity_probe_timeout_millis: 180_000,
            scope_settlement_timeout_millis: 5_000,
            negative_cases_timeout_millis: 300_000,
        })
    {
        return Err("FF-WP010-E-DEADLINE: producer phase ceilings are not canonical".to_owned());
    }
    let normal_execution = lifecycle[2]
        .monotonic_millis
        .checked_sub(lifecycle[1].monotonic_millis)
        .ok_or_else(|| "FF-WP010-E-DEADLINE: normal lifecycle reversed".to_owned())?;
    let startup_timeline = lifecycle[1].monotonic_millis;
    let validation_timeline = lifecycle[3]
        .monotonic_millis
        .checked_sub(lifecycle[2].monotonic_millis)
        .ok_or_else(|| "FF-WP010-E-DEADLINE: validation lifecycle reversed".to_owned())?;
    let forced_combined = observed
        .forced_kill_millis
        .checked_add(observed.reap_millis)
        .and_then(|value| value.checked_add(observed.scope_settlement_millis))
        .ok_or_else(|| "FF-WP010-E-DEADLINE: forced phase sum overflow".to_owned())?;
    let forced_cleanup_limit = limits
        .forced_kill_timeout_millis
        .checked_add(limits.reap_timeout_millis)
        .and_then(|value| value.checked_add(producer.scope_settlement_timeout_millis))
        .ok_or_else(|| "FF-WP010-E-DEADLINE: forced ceiling sum overflow".to_owned())?;
    let forced_timeline = forced_lifecycle[3]
        .monotonic_millis
        .checked_sub(forced_lifecycle[2].monotonic_millis)
        .ok_or_else(|| "FF-WP010-E-DEADLINE: forced lifecycle reversed".to_owned())?;
    let grace_valid = match report.platform {
        ProofPlatform::WindowsX86_64 => observed.graceful_stop_millis == 0,
        ProofPlatform::LinuxX86_64 => observed.graceful_stop_millis > 0,
    };
    let phase_checks = [
        (
            observed.fixture_probe_millis,
            producer.fixture_probe_timeout_millis,
        ),
        (
            observed.identity_probe_millis,
            producer.identity_probe_timeout_millis,
        ),
        (observed.startup_millis, limits.startup_timeout_millis),
        (observed.execution_millis, limits.execution_timeout_millis),
        (observed.validation_millis, limits.ffprobe_timeout_millis),
        (
            observed.graceful_stop_millis,
            limits.graceful_stop_timeout_millis,
        ),
        (
            observed.forced_kill_millis,
            limits.forced_kill_timeout_millis,
        ),
        (observed.reap_millis, limits.reap_timeout_millis),
        (
            observed.scope_settlement_millis,
            producer.scope_settlement_timeout_millis,
        ),
        (
            observed.negative_cases_millis,
            producer.negative_cases_timeout_millis,
        ),
    ];
    if observed.startup_millis != startup_timeline
        || observed.execution_millis != normal_execution
        || observed.validation_millis != validation_timeline
        || forced_timeline != forced_combined
        || forced_timeline > forced_cleanup_limit
        || !grace_valid
        || phase_checks.iter().any(|(actual, limit)| actual > limit)
    {
        return Err(
            "FF-WP010-E-DEADLINE: a measured phase exceeded or diverged from its exact ceiling"
                .to_owned(),
        );
    }
    Ok(())
}

fn validate_forced_wait_platform(
    platform: ProofPlatform,
    wait: &ForcedWaitReceiptEvidenceV1,
) -> Result<bool, String> {
    if !wait.direct_child_reaped || !wait.forced_by_supervisor {
        return Err(
            "FF-WP010-E-RAW-FORCED-WAIT: forced direct-child reap was not observed".to_owned(),
        );
    }
    let platform_status_valid = match platform {
        ProofPlatform::WindowsX86_64 => {
            wait.exit_code.is_none()
                && wait.signal.is_none()
                && wait.windows_opaque_status == Some(0xffff_fffc)
        }
        ProofPlatform::LinuxX86_64 => {
            wait.exit_code.is_none()
                && wait.signal == Some(9)
                && wait.windows_opaque_status.is_none()
        }
    };
    if !platform_status_valid {
        return Err(
            "FF-WP010-E-RAW-FORCED-WAIT: termination status does not match the platform".to_owned(),
        );
    }
    Ok(true)
}

fn validate_output_facts(
    output: &OutputFactsEvidenceV1,
    limits: &LimitsEvidenceV1,
    request_stream_maps: &[StreamMapEvidenceV1],
    output_muxer: &str,
) -> Result<(), String> {
    if !is_sha256(&output.output_identity_sha256)
        || output.file_size_bytes == 0
        || output.file_size_bytes > limits.output_max_file_size_bytes
        || output.duration_millis > limits.output_max_duration_millis
    {
        return Err("FF-WP010-E-RAW-OUTPUT: invalid bounded output facts".to_owned());
    }
    if output.format_names.is_empty()
        || output.format_names.len() > 32
        || output
            .format_names
            .iter()
            .any(|text| !valid_component(text, 128))
        || output
            .format_names
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        || output
            .format_names
            .binary_search_by(|format| format.as_str().cmp(output_muxer))
            .is_err()
        || output.streams.is_empty()
        || output.streams.len() > limits.output_max_streams as usize
    {
        return Err("FF-WP010-E-RAW-OUTPUT: formats or streams are outside bounds".to_owned());
    }
    let request_tuples = request_stream_maps
        .iter()
        .map(stream_map_tuple)
        .collect::<BTreeSet<_>>();
    if request_tuples.len() != request_stream_maps.len()
        || output.streams.len() != request_stream_maps.len()
    {
        return Err(
            "FF-WP010-E-RAW-OUTPUT: request/output stream maps are not one-to-one".to_owned(),
        );
    }
    let mut output_tuples = BTreeSet::new();
    for (expected_index, stream) in output.streams.iter().enumerate() {
        let kind = match stream.kind {
            OutputStreamKindV1::Video => "video",
            OutputStreamKindV1::Audio => "audio",
            _ => {
                return Err(
                    "FF-WP010-E-RAW-OUTPUT: only bounded audio/video facts are accepted".to_owned(),
                );
            }
        };
        let selected_kind = match stream.selected_source.stream_kind {
            OutputStreamKindV1::Video => "video",
            OutputStreamKindV1::Audio => "audio",
            _ => "unsupported",
        };
        let selected = StreamMapEvidenceV1 {
            input_index: stream.selected_source.input_index,
            stream_kind: selected_kind.to_owned(),
            stream_index: stream.selected_source.stream_index,
            source_payload_sha256: stream.selected_source.source_payload_sha256.clone(),
        };
        let dimensions_valid = match stream.kind {
            OutputStreamKindV1::Video => {
                stream.width != Some(0)
                    && stream.height != Some(0)
                    && stream
                        .width
                        .is_none_or(|value| value <= limits.output_max_width)
                    && stream
                        .height
                        .is_none_or(|value| value <= limits.output_max_height)
                    && stream.channels.is_none()
            }
            OutputStreamKindV1::Audio => {
                stream.width.is_none()
                    && stream.height.is_none()
                    && stream.channels != Some(0)
                    && stream
                        .channels
                        .is_none_or(|value| value <= limits.output_max_channels)
            }
            _ => false,
        };
        if stream.index != u32::try_from(expected_index).unwrap_or(u32::MAX)
            || !valid_component(&stream.codec_name, 128)
            || !dimensions_valid
            || !is_sha256(&stream.output_payload_sha256)
            || stream.output_payload_sha256 != stream.selected_source.source_payload_sha256
            || kind != selected_kind
            || !output_tuples.insert(stream_map_tuple(&selected))
        {
            return Err(
                "FF-WP010-E-RAW-OUTPUT: stream payload is not bound to selected source".to_owned(),
            );
        }
    }
    if output_tuples != request_tuples {
        return Err(
            "FF-WP010-E-RAW-OUTPUT: output selections diverge from request stream maps".to_owned(),
        );
    }
    Ok(())
}

fn request_stream_maps(request: &serde_json::Value) -> Result<Vec<StreamMapEvidenceV1>, String> {
    let values = request
        .pointer("/operation/body/stream_maps")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            "FF-WP010-E-RAW-REQUEST: stream-copy operation omits stream maps".to_owned()
        })?;
    if values.is_empty() || values.len() > 128 {
        return Err("FF-WP010-E-RAW-REQUEST: stream-map count is outside bounds".to_owned());
    }
    values
        .iter()
        .cloned()
        .map(|value| {
            let mapping: StreamMapEvidenceV1 = serde_json::from_value(value)
                .map_err(|error| format!("FF-WP010-E-RAW-REQUEST: stream map: {error}"))?;
            if !matches!(mapping.stream_kind.as_str(), "audio" | "video")
                || !is_sha256(&mapping.source_payload_sha256)
            {
                return Err("FF-WP010-E-RAW-REQUEST: invalid stream-map identity".to_owned());
            }
            Ok(mapping)
        })
        .collect()
}

fn stream_map_tuple(mapping: &StreamMapEvidenceV1) -> (u16, String, u16, String) {
    (
        mapping.input_index,
        mapping.stream_kind.clone(),
        mapping.stream_index,
        mapping.source_payload_sha256.clone(),
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "the argument oracle mirrors the complete versioned contract projection in one closed sequence"
)]
fn reconstruct_arguments(
    request: &serde_json::Value,
    platform: ProofPlatform,
) -> Result<Vec<String>, String> {
    if json_string_at(request, &["operation", "kind"])? != "stream_copy" {
        return Err("FF-WP010-E-RAW-ARGUMENTS: unsupported operation kind".to_owned());
    }
    let body = request
        .pointer("/operation/body")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "FF-WP010-E-RAW-ARGUMENTS: stream-copy body is missing".to_owned())?;
    let body_keys = body.keys().map(String::as_str).collect::<BTreeSet<_>>();
    if body_keys != BTreeSet::from(["inputs", "output_muxer", "output_path", "stream_maps"]) {
        return Err("FF-WP010-E-RAW-ARGUMENTS: stream-copy body shape is invalid".to_owned());
    }
    let inputs: Vec<InputEvidenceV1> = serde_json::from_value(
        body.get("inputs")
            .cloned()
            .ok_or_else(|| "FF-WP010-E-RAW-ARGUMENTS: inputs are missing".to_owned())?,
    )
    .map_err(|error| format!("FF-WP010-E-RAW-ARGUMENTS: inputs: {error}"))?;
    let mappings = request_stream_maps(request)?;
    if inputs.is_empty()
        || inputs.len() > 32
        || mappings
            .iter()
            .any(|mapping| usize::from(mapping.input_index) >= inputs.len())
        || !mappings.windows(2).all(|pair| {
            (
                pair[0].input_index,
                pair[0].stream_kind.as_str(),
                pair[0].stream_index,
            ) < (
                pair[1].input_index,
                pair[1].stream_kind.as_str(),
                pair[1].stream_index,
            )
        })
    {
        return Err("FF-WP010-E-RAW-ARGUMENTS: invalid input or map ordering".to_owned());
    }
    let output_path = json_string_at(request, &["operation", "body", "output_path"])?;
    let output_muxer = json_string_at(request, &["operation", "body", "output_muxer"])?;
    bounded_nonempty("output_path", output_path)?;
    bounded_nonempty("output_muxer", output_muxer)?;
    let linux_fd_bound = platform == ProofPlatform::LinuxX86_64;
    let mut arguments = vec![
        "-hide_banner".to_owned(),
        "-nostdin".to_owned(),
        if linux_fd_bound { "-y" } else { "-n" }.to_owned(),
        "-protocol_whitelist".to_owned(),
        if linux_fd_bound {
            "fd,pipe"
        } else {
            "file,pipe"
        }
        .to_owned(),
        "-progress".to_owned(),
        "pipe:1".to_owned(),
    ];
    for (input_index, input) in inputs.into_iter().enumerate() {
        bounded_nonempty("input_path", &input.path)?;
        let demuxer = match input.demuxer.as_str() {
            "aac_adts" => "aac",
            "h264_annex_b" => "h264",
            _ => {
                return Err("FF-WP010-E-RAW-ARGUMENTS: unaudited input demuxer".to_owned());
            }
        };
        if linux_fd_bound {
            let target_fd = 64_usize
                .checked_add(input_index)
                .ok_or_else(|| "FF-WP010-E-RAW-ARGUMENTS: Linux descriptor overflow".to_owned())?;
            arguments.extend([
                "-protocol_whitelist".to_owned(),
                "fd".to_owned(),
                "-f".to_owned(),
                demuxer.to_owned(),
                "-fd".to_owned(),
                target_fd.to_string(),
                "-i".to_owned(),
                "fd:".to_owned(),
            ]);
        } else {
            arguments.extend([
                "-protocol_whitelist".to_owned(),
                "file".to_owned(),
                "-f".to_owned(),
                demuxer.to_owned(),
                "-i".to_owned(),
                input.path,
            ]);
        }
    }
    for mapping in mappings {
        let kind = match mapping.stream_kind.as_str() {
            "video" => "v",
            "audio" => "a",
            "subtitle" => "s",
            "data" => "d",
            "attachment" => "t",
            _ => return Err("FF-WP010-E-RAW-ARGUMENTS: invalid stream kind".to_owned()),
        };
        arguments.push("-map".to_owned());
        arguments.push(format!(
            "{}:{kind}:{}",
            mapping.input_index, mapping.stream_index
        ));
    }
    if linux_fd_bound {
        arguments.extend([
            "-protocol_whitelist".to_owned(),
            "fd".to_owned(),
            "-c".to_owned(),
            "copy".to_owned(),
            "-f".to_owned(),
            output_muxer.to_owned(),
            "-fd".to_owned(),
            "96".to_owned(),
            "fd:".to_owned(),
        ]);
    } else {
        arguments.extend([
            "-protocol_whitelist".to_owned(),
            "file".to_owned(),
            "-c".to_owned(),
            "copy".to_owned(),
            "-f".to_owned(),
            output_muxer.to_owned(),
            output_path.to_owned(),
        ]);
    }
    Ok(arguments)
}

fn validate_containment_observations(report: &PlatformProofReportV2) -> Result<bool, String> {
    match (&report.platform, &report.containment_observations) {
        (
            ProofPlatform::WindowsX86_64,
            ContainmentObservationsV1::WindowsJob {
                active_process_samples,
                attached_before_execution,
                kill_on_job_close,
                handle_sentinel_leaked,
                kill_on_job_close_parent_death_observed,
                suspended_orphan_observed,
            },
        ) => {
            if active_process_samples.len() < 2
                || active_process_samples.len() > MAX_ACTIVE_PROCESS_SAMPLES
                || active_process_samples
                    .first()
                    .is_none_or(|value| *value == 0)
                || active_process_samples.last() != Some(&0)
                || !attached_before_execution
                || !kill_on_job_close
                || *handle_sentinel_leaked
                || !kill_on_job_close_parent_death_observed
                || *suspended_orphan_observed
                || report.behavior.windows_attached_before_execution != Some(true)
                || report.behavior.windows_kill_on_job_close != Some(true)
                || report.behavior.windows_active_processes != Some(0)
                || report.behavior.windows_handle_sentinel_leaked != Some(false)
            {
                return Err(
                    "FF-WP010-E-RAW-CONTAINMENT: Windows Job observations do not prove live-to-zero containment"
                        .to_owned(),
                );
            }
            Ok(true)
        }
        (
            ProofPlatform::LinuxX86_64,
            ContainmentObservationsV1::UnixProcessGroup {
                process_group_verified,
                term_sent,
                kill_sent,
                group_absent,
                setsid_escape_observed,
            },
        ) => {
            if !(*process_group_verified
                && *term_sent
                && *kill_sent
                && *group_absent
                && *setsid_escape_observed)
                || report.behavior.unix_process_group_observed != Some(true)
                || report.behavior.unix_term_kill_observed != Some(true)
                || report.behavior.unix_setsid_escape_observed != Some(true)
            {
                return Err(
                    "FF-WP010-E-RAW-CONTAINMENT: Unix group and setsid observations are incomplete"
                        .to_owned(),
                );
            }
            Ok(true)
        }
        _ => Err("FF-WP010-E-RAW-CONTAINMENT: observation kind does not match platform".to_owned()),
    }
}

fn decode_lower_hex(value: &str) -> Result<Vec<u8>, String> {
    if value.len() > 512 * 1_024 || !value.len().is_multiple_of(2) {
        return Err("FF-WP010-E-RAW-DIAGNOSTIC: invalid hex bound".to_owned());
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_nibble(pair[0])?;
            let low = hex_nibble(pair[1])?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn replay_progress_transcript(
    transcript: &[u8],
    limits: &LimitsEvidenceV1,
) -> Result<ProgressObservationsV1, String> {
    if transcript.is_empty() || transcript.len() as u64 > limits.progress_max_total_bytes {
        return Err("FF-WP010-E-RAW-PROGRESS: transcript byte bound exceeded".to_owned());
    }
    let mut line = Vec::new();
    let mut record = Vec::new();
    let mut record_bytes = 0_u64;
    let mut observation = ProgressObservationsV1 {
        record_count: 0,
        total_bytes: transcript.len() as u64,
        parser_steps: 0,
        saw_terminal: false,
    };
    for byte in transcript {
        observation.parser_steps = observation
            .parser_steps
            .checked_add(1)
            .ok_or_else(|| "FF-WP010-E-RAW-PROGRESS: parser work overflow".to_owned())?;
        if observation.parser_steps > limits.progress_max_parser_steps {
            return Err("FF-WP010-E-RAW-PROGRESS: parser work bound exceeded".to_owned());
        }
        if *byte != b'\n' {
            line.push(*byte);
            if line.len() as u64 > limits.progress_max_record_bytes {
                return Err("FF-WP010-E-RAW-PROGRESS: line bound exceeded".to_owned());
            }
            continue;
        }
        if observation.saw_terminal {
            return Err("FF-WP010-E-RAW-PROGRESS: bytes follow terminal record".to_owned());
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        record_bytes = record_bytes
            .checked_add(line.len() as u64 + 1)
            .ok_or_else(|| "FF-WP010-E-RAW-PROGRESS: record size overflow".to_owned())?;
        if record_bytes > limits.progress_max_record_bytes {
            return Err("FF-WP010-E-RAW-PROGRESS: record bound exceeded".to_owned());
        }
        let text = std::str::from_utf8(&line)
            .map_err(|_| "FF-WP010-E-RAW-PROGRESS: non-UTF-8 field".to_owned())?;
        let (key, value) = text
            .split_once('=')
            .ok_or_else(|| "FF-WP010-E-RAW-PROGRESS: malformed field".to_owned())?;
        if key.is_empty()
            || key.len() as u64 > limits.progress_max_field_bytes
            || value.len() as u64 > limits.progress_max_field_bytes
            || !key
                .bytes()
                .all(|value| value.is_ascii_lowercase() || value.is_ascii_digit() || value == b'_')
        {
            return Err("FF-WP010-E-RAW-PROGRESS: invalid field".to_owned());
        }
        let scan_steps = record
            .len()
            .checked_mul(2)
            .and_then(|value| value.checked_add(key.len()))
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| "FF-WP010-E-RAW-PROGRESS: parser work overflow".to_owned())?;
        observation.parser_steps = observation
            .parser_steps
            .checked_add(scan_steps)
            .ok_or_else(|| "FF-WP010-E-RAW-PROGRESS: parser work overflow".to_owned())?;
        if observation.parser_steps > limits.progress_max_parser_steps
            || record_contains_progress_key(&record, key.as_bytes())
        {
            return Err("FF-WP010-E-RAW-PROGRESS: duplicate field or work bound".to_owned());
        }
        if key == "progress" {
            observation.saw_terminal = match value {
                "continue" => false,
                "end" => true,
                _ => return Err("FF-WP010-E-RAW-PROGRESS: invalid progress marker".to_owned()),
            };
            observation.record_count = observation
                .record_count
                .checked_add(1)
                .ok_or_else(|| "FF-WP010-E-RAW-PROGRESS: record count overflow".to_owned())?;
            if observation.record_count > limits.progress_max_records {
                return Err("FF-WP010-E-RAW-PROGRESS: record count bound exceeded".to_owned());
            }
            record.clear();
            record_bytes = 0;
        } else {
            record.extend_from_slice(&line);
            record.push(b'\n');
        }
        line.clear();
    }
    if !line.is_empty() || !record.is_empty() || !observation.saw_terminal {
        return Err("FF-WP010-E-RAW-PROGRESS: truncated or missing terminal record".to_owned());
    }
    Ok(observation)
}

fn progress_transcript_capacity(
    resources: &RequestResourceReferenceV1,
    limits: &LimitsEvidenceV1,
) -> Result<usize, String> {
    let allocation = usize::try_from(resources.progress_pipe_bytes)
        .map_err(|_| "FF-WP010-E-RAW-PROGRESS: allocation exceeds host size".to_owned())?;
    let parser_bytes = usize::try_from(limits.progress_max_record_bytes)
        .ok()
        .and_then(|bytes| bytes.checked_mul(2))
        .ok_or_else(|| "FF-WP010-E-RAW-PROGRESS: parser allocation overflow".to_owned())?;
    let available = allocation
        .checked_sub(parser_bytes)
        .ok_or_else(|| "FF-WP010-E-RAW-PROGRESS: parser allocation exceeds credit".to_owned())?;
    let chunk_bytes = (available / 8).min(8 * 1_024);
    let chunk_allocation = chunk_bytes
        .checked_mul(4)
        .ok_or_else(|| "FF-WP010-E-RAW-PROGRESS: chunk allocation overflow".to_owned())?;
    let transcript_capacity = available
        .checked_sub(chunk_allocation)
        .ok_or_else(|| "FF-WP010-E-RAW-PROGRESS: transcript allocation underflow".to_owned())?
        .min(usize::try_from(limits.progress_max_total_bytes).unwrap_or(usize::MAX));
    if chunk_bytes == 0 || transcript_capacity == 0 {
        return Err("FF-WP010-E-RAW-PROGRESS: invalid governed allocation".to_owned());
    }
    Ok(transcript_capacity)
}

fn record_contains_progress_key(record: &[u8], candidate: &[u8]) -> bool {
    record.split(|byte| *byte == b'\n').any(|line| {
        line.iter()
            .position(|byte| *byte == b'=')
            .is_some_and(|separator| &line[..separator] == candidate)
    })
}

fn hex_nibble(value: u8) -> Result<u8, String> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err("FF-WP010-E-RAW-DIAGNOSTIC: hex must be lowercase".to_owned()),
    }
}

fn validate_windows(report: &PlatformProofReportV2) -> Result<(), String> {
    let behavior = &report.behavior;
    if behavior.windows_attached_before_execution != Some(true)
        || behavior.windows_kill_on_job_close != Some(true)
        || behavior.windows_active_processes != Some(0)
    {
        return Err("FF-WP010-E-FALSE-ACTIVE-ZERO: Windows Job establishment and independent active-zero are required".to_owned());
    }
    if behavior.windows_handle_sentinel_leaked != Some(false) {
        return Err(
            "FF-WP010-E-HANDLE-LEAK: Windows sentinel observed an inheritable handle".to_owned(),
        );
    }
    if behavior.unix_process_group_observed.is_some()
        || behavior.unix_term_kill_observed.is_some()
        || behavior.unix_setsid_escape_observed.is_some()
    {
        return Err("FF-WP010-E-PLATFORM-FIELDS: Windows report contains Unix behavior".to_owned());
    }
    if !contains_terms(&report.residual_uncertainty, &["suspended", "orphan"]) {
        return Err(
            "FF-WP010-E-WINDOWS-RESIDUAL: suspended-orphan uncertainty must be retained".to_owned(),
        );
    }
    Ok(())
}

fn validate_linux(report: &PlatformProofReportV2) -> Result<(), String> {
    let behavior = &report.behavior;
    if behavior.unix_process_group_observed != Some(true)
        || behavior.unix_term_kill_observed != Some(true)
        || behavior.unix_setsid_escape_observed != Some(true)
    {
        return Err("FF-WP010-E-UNIX-BEHAVIOR: process-group, TERM/KILL, and setsid escape evidence is required".to_owned());
    }
    if behavior.windows_attached_before_execution.is_some()
        || behavior.windows_kill_on_job_close.is_some()
        || behavior.windows_active_processes.is_some()
        || behavior.windows_handle_sentinel_leaked.is_some()
    {
        return Err(
            "FF-WP010-E-PLATFORM-FIELDS: Linux report contains Windows behavior".to_owned(),
        );
    }
    if !contains_terms(
        &report.residual_uncertainty,
        &["setsid", "signal scope", "not containment"],
    ) {
        return Err(
            "FF-WP010-E-UNIX-ESCAPE-RESIDUAL: setsid signal-scope limitation must be retained"
                .to_owned(),
        );
    }
    Ok(())
}

fn validate_tool_identity(
    platform: ProofPlatform,
    expected_name: &str,
    identity: &ToolProofIdentityV1,
) -> Result<(), String> {
    for (label, value) in [
        ("executable_name", identity.executable_name.as_str()),
        ("canonical_path", identity.canonical_path.as_str()),
        ("version_line", identity.version_line.as_str()),
        ("file_identity", identity.file_identity.as_str()),
    ] {
        bounded_nonempty(label, value)?;
    }
    let name_matches = match platform {
        ProofPlatform::WindowsX86_64 => {
            identity.executable_name == expected_name
                || identity.executable_name == format!("{expected_name}.exe")
        }
        ProofPlatform::LinuxX86_64 => identity.executable_name == expected_name,
    };
    if !name_matches
        || !identity
            .version_line
            .to_ascii_lowercase()
            .starts_with(expected_name)
    {
        return Err(
            "FF-WP010-E-TOOL-MANIFEST: executable name or version output mismatch".to_owned(),
        );
    }
    let path_valid = match platform {
        ProofPlatform::WindowsX86_64 => is_windows_absolute_path(&identity.canonical_path),
        ProofPlatform::LinuxX86_64 => is_linux_absolute_path(&identity.canonical_path),
    };
    if !path_valid {
        return Err(
            "FF-WP010-E-TOOL-MANIFEST: executable path is not canonical absolute platform syntax"
                .to_owned(),
        );
    }
    Ok(())
}

fn validate_fixture_tool_identities(report: &PlatformProofReportV2) -> Result<(), String> {
    let expected_roles = match report.platform {
        ProofPlatform::WindowsX86_64 => BTreeSet::from([FixtureToolRoleV1::FakeChild]),
        ProofPlatform::LinuxX86_64 => {
            BTreeSet::from([FixtureToolRoleV1::FakeChild, FixtureToolRoleV1::Setsid])
        }
    };
    let observed_roles = report
        .fixture_tools
        .iter()
        .map(|identity| identity.role)
        .collect::<BTreeSet<_>>();
    if report.fixture_tools.len() != expected_roles.len() || observed_roles != expected_roles {
        return Err(
            "FF-WP010-E-FIXTURE-TOOL: exact platform fixture-tool roles are required".to_owned(),
        );
    }
    let mut paths = BTreeSet::new();
    let mut file_identities = BTreeSet::new();
    let mut content_digests = BTreeSet::new();
    for identity in &report.fixture_tools {
        for (label, value) in [
            ("fixture executable_name", identity.executable_name.as_str()),
            ("fixture canonical_path", identity.canonical_path.as_str()),
            ("fixture version_line", identity.version_line.as_str()),
            ("fixture file_identity", identity.file_identity.as_str()),
        ] {
            bounded_nonempty(label, value)?;
        }
        let path_valid = match report.platform {
            ProofPlatform::WindowsX86_64 => is_windows_absolute_path(&identity.canonical_path),
            ProofPlatform::LinuxX86_64 => is_linux_absolute_path(&identity.canonical_path),
        };
        let role_valid = match identity.role {
            FixtureToolRoleV1::FakeChild => {
                identity.executable_name.starts_with("fforager-fake-child")
                    && identity.version_line.starts_with("fforager-fake-child ")
            }
            FixtureToolRoleV1::Setsid => {
                report.platform == ProofPlatform::LinuxX86_64
                    && identity.executable_name == "setsid"
                    && identity.version_line.starts_with("setsid from util-linux ")
            }
        };
        if !path_valid
            || !role_valid
            || !is_sha256(&identity.content_sha256)
            || !paths.insert(identity.canonical_path.clone())
            || !file_identities.insert(identity.file_identity.clone())
            || !content_digests.insert(identity.content_sha256.clone())
        {
            return Err(
                "FF-WP010-E-FIXTURE-TOOL: invalid, duplicate, or mismatched fixture identity"
                    .to_owned(),
            );
        }
    }
    Ok(())
}

fn is_windows_absolute_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    !value.is_empty()
        && value.len() <= 4_096
        && !value
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
        && ((bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/'))
            || value.starts_with("\\\\"))
        && !contains_dot_segment(value)
        && !value.ends_with(['/', '\\'])
        && !windows_path_has_unsafe_component(value, true)
}

fn is_linux_absolute_path(value: &str) -> bool {
    value.starts_with('/')
        && value.len() <= 4_096
        && !value
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
        && !contains_dot_segment(value)
        && !value.ends_with('/')
}

fn contains_terms(values: &[String], terms: &[&str]) -> bool {
    let joined = values.join(" ").to_ascii_lowercase();
    terms.iter().all(|term| joined.contains(term))
}

fn bounded_nonempty(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > MAX_TEXT_BYTES
        || value
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return Err(format!("FF-WP010-E-TEXT-BOUND: {label}"));
    }
    Ok(())
}

fn valid_component(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'_' | b'-' | b'.' | b':')
        })
}

fn build_receipt(
    source_commit: &str,
    fixture_contract_sha256: &str,
    inputs: &[ReportInput],
    aggregate: CrossPlatformProofV1,
) -> AggregationReceiptV1 {
    let mut input_receipts = inputs
        .iter()
        .map(|input| AggregationInputReceiptV1 {
            platform: input.report.platform,
            report_path: input.path.clone(),
            report_sha256: sha256(&input.bytes),
            producer_receipt_path: input.producer_receipt_path.clone(),
            producer_receipt_sha256: sha256(&input.producer_receipt_bytes),
        })
        .collect::<Vec<_>>();
    input_receipts.sort_by_key(|input| input.platform);
    AggregationReceiptV1 {
        schema_id: RECEIPT_SCHEMA_ID.to_owned(),
        schema_version: RECEIPT_SCHEMA_VERSION.to_owned(),
        proof_class: PROOF_CLASS.to_owned(),
        source_commit: source_commit.to_owned(),
        source_dirty: false,
        fixture_contract: AggregationFixtureReceiptV1 {
            path: FIXTURE_CONTRACT_PATH.to_owned(),
            sha256: fixture_contract_sha256.to_owned(),
        },
        inputs: input_receipts,
        aggregate,
        limitations: vec![
            "Integration evidence for a non-product prerequisite; not ff.runtime-proof@1 or production-runtime proof.".to_owned(),
            "Unix process groups are signal scopes and do not contain hostile setsid escape.".to_owned(),
            "Windows parent death during suspended setup can leave a suspended orphan until a separately approved broker removes the interval.".to_owned(),
        ],
    }
}

fn validate_receipt(
    receipt: &AggregationReceiptV1,
    inputs: &[ReportInput],
    context: &ValidationContext<'_>,
) -> Result<(), String> {
    if receipt.schema_id != RECEIPT_SCHEMA_ID
        || receipt.schema_version != RECEIPT_SCHEMA_VERSION
        || receipt.proof_class != PROOF_CLASS
        || receipt.source_dirty
        || receipt.source_commit != context.source_commit
        || receipt.fixture_contract.path != FIXTURE_CONTRACT_PATH
        || receipt.fixture_contract.sha256 != context.fixture_contract_sha256
    {
        return Err(
            "FF-WP010-E-FORGED-RECEIPT: receipt provenance or proof class mismatch".to_owned(),
        );
    }
    let reports = inputs
        .iter()
        .map(|input| input.report.clone())
        .collect::<Vec<_>>();
    let aggregate = validate_and_aggregate(&reports, context)?;
    let expected = build_receipt(
        context.source_commit,
        context.fixture_contract_sha256,
        inputs,
        aggregate,
    );
    if receipt != &expected {
        return Err("FF-WP010-E-FORGED-RECEIPT: receipt content mismatch".to_owned());
    }
    Ok(())
}

fn is_git_commit(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn sha256(bytes: &[u8]) -> String {
    hex_digest(Sha256::digest(bytes))
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    let digest = bytes.as_ref();
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn sha256_domain(domain: &str, bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update([0]);
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    type ReportMutation = Box<dyn Fn(&mut PlatformProofReportV2)>;

    fn reports() -> [PlatformProofReportV2; 2] {
        [
            report(ProofPlatform::WindowsX86_64),
            report(ProofPlatform::LinuxX86_64),
        ]
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the strict test constructor populates and cryptographically binds every platform report field"
    )]
    fn report(platform: ProofPlatform) -> PlatformProofReportV2 {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../product/crates/fforager-contracts/testdata/ffmpeg-supervision-v1.0.json"
        ))
        .expect("contract fixture JSON");
        let mut request: RequestEvidenceV1 =
            serde_json::from_value(fixture["request"].clone()).expect("strict request fixture");
        if platform == ProofPlatform::LinuxX86_64 {
            for executable in [
                &mut request.toolchain.ffmpeg,
                &mut request.toolchain.ffprobe,
            ] {
                executable.host.operating_system = RequestOperatingSystemV1::Linux;
                executable
                    .capability_binding
                    .capabilities
                    .push("protocol:fd".to_owned());
                executable.capability_binding.capabilities.sort();
            }
            request.toolchain.ffmpeg.absolute_path = "/usr/bin/ffmpeg".to_owned();
            request.toolchain.ffprobe.absolute_path = "/usr/bin/ffprobe".to_owned();
            request.working_directory = "/proof/job".to_owned();
            request.environment.trusted_bindings = vec![
                RequestEnvironmentBindingV1::JobTemporaryDirectory,
                RequestEnvironmentBindingV1::LocaleC,
            ];
        }
        let operation_plan_canonical_json =
            serde_json::to_string(&request.operation).expect("operation JSON");
        let operation_plan_sha256 = sha256_domain(
            "ff.ffmpeg-operation-canonical-json@1",
            operation_plan_canonical_json.as_bytes(),
        );
        request
            .operation_plan_sha256
            .clone_from(&operation_plan_sha256);
        let request_contract_canonical_json =
            serde_json::to_string(&request).expect("request JSON");
        let request_contract_sha256 = sha256_domain(
            "ff.ffmpeg-request-canonical-json@1",
            request_contract_canonical_json.as_bytes(),
        );
        let request_value = serde_json::to_value(&request).expect("request value");
        let argument_vector = reconstruct_arguments(&request_value, platform).expect("arguments");
        let argument_vector_sha256 =
            sha256(&serde_json::to_vec(&argument_vector).expect("argument JSON"));
        let bound_runtime_sha256 = sha256_domain(
            BOUND_RUNTIME_PROJECTION_ID,
            &serde_json::to_vec(&BoundRuntimeProjection {
                request_contract_sha256: &request_contract_sha256,
                operation_plan_sha256: &operation_plan_sha256,
                arguments: &argument_vector,
            })
            .expect("bound runtime JSON"),
        );
        let tool =
            |requested: &RequestExecutableIdentityV1, executable_name: &str| ToolProofIdentityV1 {
                executable_name: executable_name.to_owned(),
                canonical_path: requested.absolute_path.clone(),
                version_line: requested.normalized_version.clone(),
                content_sha256: requested.content_sha256.clone(),
                file_identity: requested.file_identity.clone(),
                version_output_sha256: requested.version_output_sha256.clone(),
                normalized_probe_sha256: requested
                    .capability_binding
                    .normalized_probe_sha256
                    .clone(),
                capabilities: requested.capability_binding.capabilities.clone(),
            };
        let lifecycle = vec![
            LifecycleObservationV1 {
                sequence: 1,
                state: LifecycleStateV1::Spawned,
                monotonic_millis: 0,
            },
            LifecycleObservationV1 {
                sequence: 2,
                state: LifecycleStateV1::Running,
                monotonic_millis: 1,
            },
            LifecycleObservationV1 {
                sequence: 3,
                state: LifecycleStateV1::Reaped,
                monotonic_millis: 10,
            },
            LifecycleObservationV1 {
                sequence: 4,
                state: LifecycleStateV1::Validated,
                monotonic_millis: 20,
            },
        ];
        let forced_lifecycle = vec![
            LifecycleObservationV1 {
                sequence: 1,
                state: LifecycleStateV1::Spawned,
                monotonic_millis: 0,
            },
            LifecycleObservationV1 {
                sequence: 2,
                state: LifecycleStateV1::Running,
                monotonic_millis: 1,
            },
            LifecycleObservationV1 {
                sequence: 3,
                state: LifecycleStateV1::ForcedKillRequested,
                monotonic_millis: 150,
            },
            LifecycleObservationV1 {
                sequence: 4,
                state: LifecycleStateV1::Reaped,
                monotonic_millis: 160,
            },
        ];
        let lifecycle_json = serde_json::to_string(&lifecycle).expect("lifecycle JSON");
        let forced_lifecycle_json =
            serde_json::to_string(&forced_lifecycle).expect("forced lifecycle JSON");
        let forced_lifecycle_sha256 = sha256(forced_lifecycle_json.as_bytes());
        let normal_wait_domain = "exit=Some(0);signal=None;windows_status_opaque=None;forced=false";
        let direct_wait = DirectWaitReceiptEvidenceV1 {
            exit_code: Some(0),
            signal: None,
            windows_opaque_status: None,
            forced_by_supervisor: false,
            supervision_wait_receipt_sha256: sha256(normal_wait_domain.as_bytes()),
        };
        let (forced_signal, forced_windows_status) = match platform {
            ProofPlatform::WindowsX86_64 => (None, Some(0xffff_fffc)),
            ProofPlatform::LinuxX86_64 => (Some(9), None),
        };
        let forced_domain = format!(
            "exit=None;signal={forced_signal:?};windows_status_opaque={forced_windows_status:?};forced=true"
        );
        let forced_wait = ForcedWaitReceiptEvidenceV1 {
            exit_code: None,
            signal: forced_signal,
            windows_opaque_status: forced_windows_status,
            forced_by_supervisor: true,
            direct_child_reaped: true,
            lifecycle_timeline_sha256: forced_lifecycle_sha256.clone(),
            supervision_wait_receipt_sha256: sha256(forced_domain.as_bytes()),
        };
        let direct_wait_json = serde_json::to_string(&direct_wait).expect("wait JSON");
        let forced_wait_json = serde_json::to_string(&forced_wait).expect("forced wait JSON");
        let output = OutputFactsEvidenceV1 {
            output_identity_sha256: "4".repeat(64),
            file_size_bytes: 1_048_576,
            duration_millis: 10_000,
            format_names: vec!["matroska".to_owned()],
            streams: vec![
                OutputStreamFactV1 {
                    index: 0,
                    kind: OutputStreamKindV1::Audio,
                    selected_source: OutputSelectedSourceV1 {
                        input_index: 0,
                        stream_kind: OutputStreamKindV1::Audio,
                        stream_index: 0,
                        source_payload_sha256: "5".repeat(64),
                    },
                    output_payload_sha256: "5".repeat(64),
                    codec_name: "aac".to_owned(),
                    width: None,
                    height: None,
                    channels: Some(2),
                },
                OutputStreamFactV1 {
                    index: 1,
                    kind: OutputStreamKindV1::Video,
                    selected_source: OutputSelectedSourceV1 {
                        input_index: 1,
                        stream_kind: OutputStreamKindV1::Video,
                        stream_index: 0,
                        source_payload_sha256: "6".repeat(64),
                    },
                    output_payload_sha256: "6".repeat(64),
                    codec_name: "h264".to_owned(),
                    width: Some(1920),
                    height: Some(1080),
                    channels: None,
                },
            ],
        };
        let output_json = serde_json::to_string(&output).expect("output JSON");
        let limits_json = serde_json::to_string(&request.limits).expect("limits JSON");
        let fixture_payload_json = serde_json::to_string(&vec!["5".repeat(64), "6".repeat(64)])
            .expect("fixture payload JSON");
        let fixture_tools = match platform {
            ProofPlatform::WindowsX86_64 => vec![FixtureToolProofIdentityV1 {
                role: FixtureToolRoleV1::FakeChild,
                executable_name: "fforager-fake-child.exe".to_owned(),
                canonical_path: "C:\\proof\\fforager-fake-child.exe".to_owned(),
                version_line: "fforager-fake-child 0.1.0".to_owned(),
                content_sha256: "7".repeat(64),
                file_identity: "fixture-file".to_owned(),
            }],
            ProofPlatform::LinuxX86_64 => vec![
                FixtureToolProofIdentityV1 {
                    role: FixtureToolRoleV1::FakeChild,
                    executable_name: "fforager-fake-child".to_owned(),
                    canonical_path: "/proof/fforager-fake-child".to_owned(),
                    version_line: "fforager-fake-child 0.1.0".to_owned(),
                    content_sha256: "7".repeat(64),
                    file_identity: "fixture-file".to_owned(),
                },
                FixtureToolProofIdentityV1 {
                    role: FixtureToolRoleV1::Setsid,
                    executable_name: "setsid".to_owned(),
                    canonical_path: "/usr/bin/setsid".to_owned(),
                    version_line: "setsid from util-linux 2.40".to_owned(),
                    content_sha256: "8".repeat(64),
                    file_identity: "setsid-file".to_owned(),
                },
            ],
        };
        let progress_transcript = b"frame=1\nprogress=continue\nframe=2\nprogress=end\n";
        let progress_observations =
            replay_progress_transcript(progress_transcript, &request.limits)
                .expect("progress transcript");
        PlatformProofReportV2 {
            schema_id: PLATFORM_SCHEMA_ID.to_owned(),
            source_commit: "b".repeat(40),
            source_dirty: false,
            platform,
            host_kernel: "fixture host".to_owned(),
            ffmpeg: tool(
                &request.toolchain.ffmpeg,
                if platform == ProofPlatform::WindowsX86_64 {
                    "ffmpeg.exe"
                } else {
                    "ffmpeg"
                },
            ),
            ffprobe: tool(
                &request.toolchain.ffprobe,
                if platform == ProofPlatform::WindowsX86_64 {
                    "ffprobe.exe"
                } else {
                    "ffprobe"
                },
            ),
            fixture_tools,
            request_contract_canonical_json,
            request_contract_sha256,
            operation_plan_canonical_json,
            operation_plan_sha256,
            bound_runtime_projection_id: BOUND_RUNTIME_PROJECTION_ID.to_owned(),
            bound_runtime_sha256,
            argument_vector,
            argument_vector_sha256,
            fixture_contract_sha256: context().fixture_contract_sha256.to_owned(),
            fixture_payload_sha256: sha256(fixture_payload_json.as_bytes()),
            fixture_payload_canonical_json: fixture_payload_json,
            limits_sha256: sha256(limits_json.as_bytes()),
            limits_canonical_json: limits_json,
            lifecycle_timeline_sha256: sha256(lifecycle_json.as_bytes()),
            lifecycle_timeline_canonical_json: lifecycle_json,
            direct_wait_receipt_sha256: direct_wait.supervision_wait_receipt_sha256.clone(),
            direct_wait_receipt_canonical_json: direct_wait_json,
            forced_lifecycle_timeline_sha256: forced_lifecycle_sha256,
            forced_lifecycle_timeline_canonical_json: forced_lifecycle_json,
            forced_wait_receipt_sha256: sha256_domain(
                "ff.ffmpeg-forced-wait-receipt-canonical-json@1",
                forced_wait_json.as_bytes(),
            ),
            forced_wait_receipt_canonical_json: forced_wait_json,
            output_facts_sha256: sha256(output_json.as_bytes()),
            output_facts_canonical_json: output_json,
            producer_phase_limits: ProducerPhaseLimitsV2 {
                fixture_probe_timeout_millis: 60_000,
                identity_probe_timeout_millis: 180_000,
                scope_settlement_timeout_millis: 5_000,
                negative_cases_timeout_millis: 300_000,
            },
            phase_deadlines: PhaseDeadlineObservationsV2 {
                fixture_probe_millis: 100,
                identity_probe_millis: 200,
                startup_millis: 1,
                execution_millis: 9,
                validation_millis: 10,
                graceful_stop_millis: u64::from(platform == ProofPlatform::LinuxX86_64),
                forced_kill_millis: 3,
                reap_millis: 7,
                scope_settlement_millis: 0,
                negative_cases_millis: 300,
            },
            diagnostic_total_bytes: 12,
            diagnostic_tail_hex: "666978747572652d64696167".to_owned(),
            diagnostic_tail_sha256: sha256(b"fixture-diag"),
            diagnostic_loss: DiagnosticLossEvidenceV1::None,
            progress_transcript_hex: hex_digest(progress_transcript),
            progress_transcript_sha256: sha256(progress_transcript),
            progress_observations,
            containment_observations: match platform {
                ProofPlatform::WindowsX86_64 => ContainmentObservationsV1::WindowsJob {
                    active_process_samples: vec![1, 0],
                    attached_before_execution: true,
                    kill_on_job_close: true,
                    handle_sentinel_leaked: false,
                    kill_on_job_close_parent_death_observed: true,
                    suspended_orphan_observed: false,
                },
                ProofPlatform::LinuxX86_64 => ContainmentObservationsV1::UnixProcessGroup {
                    process_group_verified: true,
                    term_sent: true,
                    kill_sent: true,
                    group_absent: true,
                    setsid_escape_observed: true,
                },
            },
            behavior: PlatformBehaviorV1 {
                direct_child_wait_observed: true,
                direct_child_reaped: true,
                successful_exit_observed: true,
                forced_cancellation_observed: true,
                bounded_progress_observed: true,
                bounded_stderr_observed: true,
                output_validated_by_ffprobe: true,
                windows_attached_before_execution: (platform == ProofPlatform::WindowsX86_64)
                    .then_some(true),
                windows_kill_on_job_close: (platform == ProofPlatform::WindowsX86_64)
                    .then_some(true),
                windows_active_processes: (platform == ProofPlatform::WindowsX86_64).then_some(0),
                windows_handle_sentinel_leaked: (platform == ProofPlatform::WindowsX86_64)
                    .then_some(false),
                unix_process_group_observed: (platform == ProofPlatform::LinuxX86_64)
                    .then_some(true),
                unix_term_kill_observed: (platform == ProofPlatform::LinuxX86_64).then_some(true),
                unix_setsid_escape_observed: (platform == ProofPlatform::LinuxX86_64)
                    .then_some(true),
            },
            residual_uncertainty: match platform {
                ProofPlatform::WindowsX86_64 => {
                    vec!["suspended orphan residual retained".to_owned()]
                }
                ProofPlatform::LinuxX86_64 => {
                    vec!["setsid escape proves signal scope is not containment".to_owned()]
                }
            },
        }
    }

    fn report_bytes(report: &PlatformProofReportV2) -> Vec<u8> {
        serde_json::to_vec(report).expect("report JSON")
    }

    fn context() -> ValidationContext<'static> {
        ValidationContext {
            source_commit: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            source_content_fingerprint: "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            fixture_contract_sha256: "947eb4d885a9b1398d300a05d8f391abe209425b34a5bfa6ceadc8839d3f9d64",
        }
    }

    fn report_input(path: String, bytes: Vec<u8>, report: PlatformProofReportV2) -> ReportInput {
        let source = crate::SourceState {
            git_commit: context().source_commit.to_owned(),
            dirty: false,
            dirty_paths: Vec::new(),
            content_fingerprint: context().source_content_fingerprint.to_owned(),
        };
        let receipt = ProducerReceiptV1 {
            schema_id: PRODUCER_RECEIPT_SCHEMA_ID.to_owned(),
            platform: report.platform,
            source_before: source.clone(),
            source_after: source,
            report_path: path.clone(),
            report_sha256: sha256(&bytes),
            built_fake_child_path: ".fforager-artifacts/fake".to_owned(),
            built_fake_child_bytes: 1,
            built_fake_child_sha256: "d".repeat(64),
            boundary_tools: Vec::new(),
            fixture_before: Vec::new(),
            fixture_after: Vec::new(),
            command: ProducerCommandProjectionV1 {
                program: "fixture".to_owned(),
                arguments: Vec::new(),
                build_program: "fixture".to_owned(),
                build_arguments: Vec::new(),
                cargo_target_directory: ".fforager-artifacts/target".to_owned(),
                working_directory: ".fforager-artifacts/cwd".to_owned(),
                fixture_root: ".fforager-artifacts".to_owned(),
                injected_environment: BTreeMap::new(),
                discovery_timeout_millis: duration_millis(PRODUCER_DISCOVERY_TIMEOUT),
                build_timeout_millis: duration_millis(PRODUCER_BUILD_TIMEOUT),
                producer_timeout_millis: duration_millis(PRODUCER_EXECUTION_TIMEOUT),
                build_outer_timeout_millis: duration_millis(PRODUCER_BUILD_OUTER_TIMEOUT),
                producer_outer_timeout_millis: duration_millis(PRODUCER_EXECUTION_OUTER_TIMEOUT),
            },
        };
        ReportInput {
            path,
            bytes,
            report,
            producer_receipt_path: ".fforager-artifacts/receipt.json".to_owned(),
            producer_receipt_bytes: serde_json::to_vec(&receipt).expect("receipt JSON"),
            producer_receipt: receipt,
        }
    }

    fn refresh_bound_runtime(report: &mut PlatformProofReportV2) {
        let request: serde_json::Value =
            serde_json::from_str(&report.request_contract_canonical_json).expect("request JSON");
        report.argument_vector =
            reconstruct_arguments(&request, report.platform).expect("governed arguments");
        report.argument_vector_sha256 =
            sha256(&serde_json::to_vec(&report.argument_vector).expect("argument JSON"));
        let projection = serde_json::to_vec(&BoundRuntimeProjection {
            request_contract_sha256: &report.request_contract_sha256,
            operation_plan_sha256: &report.operation_plan_sha256,
            arguments: &report.argument_vector,
        })
        .expect("bound-runtime projection JSON");
        report.bound_runtime_sha256 = sha256_domain(BOUND_RUNTIME_PROJECTION_ID, &projection);
    }

    #[test]
    fn producer_consumer_schema_fixture_aggregates_as_integration() {
        let reports = reports();
        assert_ne!(
            reports[0].request_contract_sha256, reports[1].request_contract_sha256,
            "platform-bound request identities must be allowed to differ"
        );
        let aggregate = validate_and_aggregate(&reports, &context()).expect("valid reports");
        assert_eq!(aggregate.schema_id, AGGREGATE_SCHEMA_ID);
        assert_eq!(aggregate.platforms.len(), 2);
        assert_eq!(aggregate.request_contract_sha256_by_platform.len(), 2);
        let receipt = build_receipt(
            context().source_commit,
            context().fixture_contract_sha256,
            &[
                report_input(
                    format!("{REPORT_ROOT}/windows.json"),
                    report_bytes(&reports[0]),
                    reports[0].clone(),
                ),
                report_input(
                    format!("{REPORT_ROOT}/linux.json"),
                    report_bytes(&reports[1]),
                    reports[1].clone(),
                ),
            ],
            aggregate,
        );
        assert_eq!(receipt.proof_class, "integration");
        assert!(!receipt.proof_class.contains("runtime"));
        validate_receipt(
            &receipt,
            &[
                report_input(
                    format!("{REPORT_ROOT}/windows.json"),
                    report_bytes(&reports[0]),
                    reports[0].clone(),
                ),
                report_input(
                    format!("{REPORT_ROOT}/linux.json"),
                    report_bytes(&reports[1]),
                    reports[1].clone(),
                ),
            ],
            &context(),
        )
        .expect("consumer receipt");
    }

    #[test]
    fn missing_or_duplicate_platform_is_rejected() {
        let reports = reports();
        assert!(
            validate_and_aggregate(&reports[..1], &context())
                .unwrap_err()
                .contains("MISSING-PLATFORM")
        );
        let duplicate = [reports[0].clone(), reports[0].clone()];
        assert!(
            validate_and_aggregate(&duplicate, &context())
                .unwrap_err()
                .contains("MISSING-PLATFORM")
        );
    }

    #[test]
    fn source_tool_fixture_and_schema_mismatch_are_rejected() {
        let baseline = reports();
        let mut source = baseline.clone();
        source[1].source_commit = "c".repeat(40);
        assert!(
            validate_and_aggregate(&source, &context())
                .unwrap_err()
                .contains("STALE-REPORT")
        );

        let mut tool = baseline.clone();
        tool[0].ffmpeg.executable_name = "ffprobe.exe".to_owned();
        assert!(
            validate_and_aggregate(&tool, &context())
                .unwrap_err()
                .contains("TOOL-MANIFEST")
        );

        let mut fixture = baseline.clone();
        fixture[0].fixture_payload_canonical_json = fixture[0]
            .fixture_payload_canonical_json
            .replacen(&"6".repeat(64), &"9".repeat(64), 1);
        fixture[0].fixture_payload_sha256 =
            sha256(fixture[0].fixture_payload_canonical_json.as_bytes());
        assert!(
            validate_and_aggregate(&fixture, &context())
                .unwrap_err()
                .contains("RAW-FIXTURE")
        );

        let mut operation = baseline.clone();
        let mut request: RequestEvidenceV1 =
            serde_json::from_str(&operation[1].request_contract_canonical_json).expect("request");
        let RequestOperationPlanV1::StreamCopy(plan) = &mut request.operation;
        plan.output_muxer = "mp4".to_owned();
        for capabilities in [
            &mut request.toolchain.ffmpeg.capability_binding.capabilities,
            &mut operation[1].ffmpeg.capabilities,
        ] {
            let capability = capabilities
                .iter_mut()
                .find(|value| value.as_str() == "muxer:matroska")
                .expect("muxer capability");
            *capability = "muxer:mp4".to_owned();
            capabilities.sort();
        }
        operation[1].operation_plan_canonical_json =
            serde_json::to_string(&request.operation).expect("operation JSON");
        operation[1].operation_plan_sha256 = sha256_domain(
            "ff.ffmpeg-operation-canonical-json@1",
            operation[1].operation_plan_canonical_json.as_bytes(),
        );
        request
            .operation_plan_sha256
            .clone_from(&operation[1].operation_plan_sha256);
        operation[1].request_contract_canonical_json =
            serde_json::to_string(&request).expect("request JSON");
        operation[1].request_contract_sha256 = sha256_domain(
            "ff.ffmpeg-request-canonical-json@1",
            operation[1].request_contract_canonical_json.as_bytes(),
        );
        let mut output: OutputFactsEvidenceV1 =
            serde_json::from_str(&operation[1].output_facts_canonical_json).expect("output");
        output.format_names = vec!["mp4".to_owned()];
        operation[1].output_facts_canonical_json =
            serde_json::to_string(&output).expect("output JSON");
        operation[1].output_facts_sha256 =
            sha256(operation[1].output_facts_canonical_json.as_bytes());
        refresh_bound_runtime(&mut operation[1]);
        assert!(
            validate_and_aggregate(&operation, &context())
                .unwrap_err()
                .contains("PLAN-MISMATCH")
        );

        let mut limits = baseline.clone();
        let mut request: RequestEvidenceV1 =
            serde_json::from_str(&limits[1].request_contract_canonical_json).expect("request");
        request.limits.execution_timeout_millis += 1;
        limits[1].limits_canonical_json =
            serde_json::to_string(&request.limits).expect("limits JSON");
        limits[1].limits_sha256 = sha256(limits[1].limits_canonical_json.as_bytes());
        limits[1].request_contract_canonical_json =
            serde_json::to_string(&request).expect("request JSON");
        limits[1].request_contract_sha256 = sha256_domain(
            "ff.ffmpeg-request-canonical-json@1",
            limits[1].request_contract_canonical_json.as_bytes(),
        );
        refresh_bound_runtime(&mut limits[1]);
        assert!(
            validate_and_aggregate(&limits, &context())
                .unwrap_err()
                .contains("LIMITS-MISMATCH")
        );

        let mut schema = baseline;
        schema[0].schema_id = "ff.runtime-proof@1".to_owned();
        assert!(
            validate_and_aggregate(&schema, &context())
                .unwrap_err()
                .contains("REPORT-SCHEMA")
        );
    }

    #[test]
    fn producer_owned_digest_cannot_serve_as_its_own_oracle() {
        let mut reports = reports();
        reports[0].fixture_contract_sha256 = "0".repeat(64);
        reports[1].fixture_contract_sha256 = "0".repeat(64);
        let error = validate_and_aggregate(&reports, &context()).unwrap_err();
        assert!(error.contains("PRODUCER-ORACLE"), "{error}");
    }

    #[test]
    fn false_active_zero_missing_wait_and_unix_residual_are_rejected() {
        let baseline = reports();
        let mut active = baseline.clone();
        active[0].behavior.windows_active_processes = Some(1);
        assert!(
            validate_and_aggregate(&active, &context())
                .unwrap_err()
                .contains("RAW-CONTAINMENT")
        );

        let mut wait = baseline.clone();
        wait[1].behavior.direct_child_wait_observed = false;
        assert!(
            validate_and_aggregate(&wait, &context())
                .unwrap_err()
                .contains("BEHAVIOR-MUTATION")
        );

        let mut residual = baseline;
        residual[1].residual_uncertainty = vec!["generic uncertainty".to_owned()];
        assert!(
            validate_and_aggregate(&residual, &context())
                .unwrap_err()
                .contains("UNIX-ESCAPE-RESIDUAL")
        );
    }

    #[test]
    fn declaration_preserving_behavior_mutation_is_rejected() {
        let mut reports = reports();
        let unchanged_digests = reports[1].lifecycle_timeline_sha256.clone();
        reports[1].behavior.forced_cancellation_observed = false;
        assert_eq!(reports[1].lifecycle_timeline_sha256, unchanged_digests);
        let error = validate_and_aggregate(&reports, &context()).unwrap_err();
        assert!(error.contains("BEHAVIOR-MUTATION"), "{error}");
    }

    #[test]
    fn forced_cleanup_phases_telescope_exactly_and_bound_scope_polling() {
        let baseline = reports();
        for report in &baseline {
            let phases = report.phase_deadlines;
            let lifecycle: Vec<LifecycleObservationV1> =
                serde_json::from_str(&report.forced_lifecycle_timeline_canonical_json)
                    .expect("forced lifecycle");
            let timeline = lifecycle[3].monotonic_millis - lifecycle[2].monotonic_millis;
            let phase_sum =
                phases.forced_kill_millis + phases.reap_millis + phases.scope_settlement_millis;
            assert!(
                phase_sum == timeline,
                "shared absolute millisecond boundaries must telescope exactly"
            );
        }
        validate_and_aggregate(&baseline, &context())
            .expect("bounded settlement overhead is valid evidence");

        let mut forged = baseline;
        forged[0].phase_deadlines.scope_settlement_millis += 1;
        let error = validate_and_aggregate(&forged, &context())
            .expect_err("phase samples cannot exceed the lifecycle settlement envelope");
        assert!(error.contains("DEADLINE"), "{error}");

        let mut over_ceiling = reports();
        over_ceiling[0].phase_deadlines.scope_settlement_millis = 5_001;
        let error = validate_and_aggregate(&over_ceiling, &context())
            .expect_err("scope settlement must retain its independent ceiling");
        assert!(error.contains("DEADLINE"), "{error}");
    }

    #[test]
    fn output_self_consistent_payload_rewrite_cannot_escape_request_binding() {
        let mut reports = reports();
        let zero = "0".repeat(64);
        reports[1].output_facts_canonical_json = reports[1]
            .output_facts_canonical_json
            .replace(&"5".repeat(64), &zero)
            .replace(&"6".repeat(64), &zero);
        reports[1].output_facts_sha256 = sha256(reports[1].output_facts_canonical_json.as_bytes());
        let error = validate_and_aggregate(&reports, &context())
            .expect_err("self-consistent output rewrite must not become its own oracle");
        assert!(error.contains("RAW-OUTPUT"), "{error}");
    }

    #[test]
    fn argument_map_cannot_diverge_from_request_operation() {
        let mut reports = reports();
        for report in &mut reports {
            let map_operand = report
                .argument_vector
                .iter()
                .position(|argument| argument == "-map")
                .expect("fixture includes an exact map")
                + 1;
            report.argument_vector[map_operand] = "9:9".to_owned();
            report.argument_vector_sha256 =
                sha256(&serde_json::to_vec(&report.argument_vector).expect("argument JSON"));
        }
        let error = validate_and_aggregate(&reports, &context())
            .expect_err("self-consistent impossible map must not escape request binding");
        assert!(error.contains("RAW-ARGUMENTS"), "{error}");
    }

    fn coherently_rewrite_forced_wait(
        report: &mut PlatformProofReportV2,
        mutate: impl FnOnce(&mut ForcedWaitReceiptEvidenceV1),
    ) {
        let mut wait: ForcedWaitReceiptEvidenceV1 =
            serde_json::from_str(&report.forced_wait_receipt_canonical_json)
                .expect("fixture forced wait");
        mutate(&mut wait);
        let supervisor_domain = format!(
            "exit={:?};signal={:?};windows_status_opaque={:?};forced={}",
            wait.exit_code, wait.signal, wait.windows_opaque_status, wait.forced_by_supervisor
        );
        wait.supervision_wait_receipt_sha256 = sha256(supervisor_domain.as_bytes());
        report.forced_wait_receipt_canonical_json =
            serde_json::to_string(&wait).expect("forced wait JSON");
        report.forced_wait_receipt_sha256 = sha256_domain(
            "ff.ffmpeg-forced-wait-receipt-canonical-json@1",
            report.forced_wait_receipt_canonical_json.as_bytes(),
        );
    }

    #[test]
    fn every_forced_wait_field_is_independently_reconstructed() {
        let mutations: Vec<ReportMutation> = vec![
            Box::new(|report| {
                coherently_rewrite_forced_wait(report, |wait| wait.exit_code = Some(1));
            }),
            Box::new(|report| {
                coherently_rewrite_forced_wait(report, |wait| wait.signal = Some(9));
            }),
            Box::new(|report| {
                coherently_rewrite_forced_wait(report, |wait| {
                    wait.windows_opaque_status = Some(1);
                });
            }),
            Box::new(|report| {
                coherently_rewrite_forced_wait(report, |wait| {
                    wait.forced_by_supervisor = false;
                });
            }),
            Box::new(|report| {
                coherently_rewrite_forced_wait(report, |wait| wait.direct_child_reaped = false);
            }),
            Box::new(|report| {
                coherently_rewrite_forced_wait(report, |wait| {
                    wait.lifecycle_timeline_sha256 = "0".repeat(64);
                });
            }),
            Box::new(|report| {
                coherently_rewrite_forced_wait(report, |_| {});
                let mut wait: ForcedWaitReceiptEvidenceV1 =
                    serde_json::from_str(&report.forced_wait_receipt_canonical_json)
                        .expect("fixture forced wait");
                wait.supervision_wait_receipt_sha256 = "0".repeat(64);
                report.forced_wait_receipt_canonical_json =
                    serde_json::to_string(&wait).expect("forced wait JSON");
                report.forced_wait_receipt_sha256 = sha256_domain(
                    "ff.ffmpeg-forced-wait-receipt-canonical-json@1",
                    report.forced_wait_receipt_canonical_json.as_bytes(),
                );
            }),
        ];
        for (index, mutation) in mutations.into_iter().enumerate() {
            let mut reports = reports();
            mutation(&mut reports[0]);
            let error = validate_and_aggregate(&reports, &context())
                .expect_err("forced wait field mutation must fail");
            assert!(
                error.contains("RAW-FORCED-WAIT"),
                "mutation {index}: {error}"
            );
        }
    }

    #[test]
    fn missing_forced_wait_and_forged_forced_timeline_are_rejected() {
        let mut missing = serde_json::to_value(&reports()[0]).expect("fixture JSON");
        missing
            .as_object_mut()
            .expect("report object")
            .remove("forced_wait_receipt_canonical_json");
        assert!(serde_json::from_value::<PlatformProofReportV2>(missing).is_err());

        let mut reports = reports();
        reports[0].forced_lifecycle_timeline_canonical_json = reports[0]
            .forced_lifecycle_timeline_canonical_json
            .replace("\"forced_kill_requested\"", "\"running\"");
        reports[0].forced_lifecycle_timeline_sha256 = sha256(
            reports[0]
                .forced_lifecycle_timeline_canonical_json
                .as_bytes(),
        );
        let mutated_timeline_sha256 = reports[0].forced_lifecycle_timeline_sha256.clone();
        coherently_rewrite_forced_wait(&mut reports[0], |wait| {
            wait.lifecycle_timeline_sha256 = mutated_timeline_sha256;
        });
        let error = validate_and_aggregate(&reports, &context())
            .expect_err("declaration-preserving forced timeline mutation must fail");
        assert!(error.contains("RAW-FORCED-LIFECYCLE"), "{error}");
    }

    #[test]
    fn normal_wait_windows_status_is_reconstructed() {
        let mut reports = reports();
        let mut wait: DirectWaitReceiptEvidenceV1 =
            serde_json::from_str(&reports[0].direct_wait_receipt_canonical_json)
                .expect("success wait fixture");
        wait.windows_opaque_status = Some(1);
        let domain = format!(
            "exit={:?};signal={:?};windows_status_opaque={:?};forced={}",
            wait.exit_code, wait.signal, wait.windows_opaque_status, wait.forced_by_supervisor
        );
        wait.supervision_wait_receipt_sha256 = sha256(domain.as_bytes());
        reports[0].direct_wait_receipt_sha256 = wait.supervision_wait_receipt_sha256.clone();
        reports[0].direct_wait_receipt_canonical_json =
            serde_json::to_string(&wait).expect("success wait JSON");
        let error = validate_and_aggregate(&reports, &context())
            .expect_err("success with an opaque failure status must fail");
        assert!(error.contains("BEHAVIOR-MUTATION"), "{error}");
    }

    #[test]
    fn raw_evidence_mutations_fail_independent_reconstruction() {
        let baseline = reports();
        let mut mutations: Vec<ReportMutation> = vec![
            Box::new(|report| report.request_contract_canonical_json.push(' ')),
            Box::new(|report| report.operation_plan_canonical_json.push(' ')),
            Box::new(|report| report.bound_runtime_projection_id.push_str("-forged")),
            Box::new(|report| report.bound_runtime_sha256 = "0".repeat(64)),
            Box::new(|report| report.argument_vector.push("injected".to_owned())),
            Box::new(|report| report.fixture_payload_canonical_json.push(' ')),
            Box::new(|report| report.limits_canonical_json.push(' ')),
            Box::new(|report| report.lifecycle_timeline_canonical_json.push(' ')),
            Box::new(|report| report.direct_wait_receipt_canonical_json.push(' ')),
            Box::new(|report| report.forced_lifecycle_timeline_canonical_json.push(' ')),
            Box::new(|report| report.forced_wait_receipt_canonical_json.push(' ')),
            Box::new(|report| report.output_facts_canonical_json.push(' ')),
            Box::new(|report| report.diagnostic_tail_hex.push_str("00")),
            Box::new(|report| {
                if let ContainmentObservationsV1::WindowsJob {
                    active_process_samples,
                    ..
                } = &mut report.containment_observations
                {
                    active_process_samples.clear();
                }
            }),
            Box::new(|report| report.progress_observations.saw_terminal = false),
        ];
        for (index, mutation) in mutations.drain(..).enumerate() {
            let mut reports = baseline.clone();
            mutation(&mut reports[0]);
            let error = validate_and_aggregate(&reports, &context())
                .expect_err("raw evidence mutation must fail");
            assert!(error.contains("FF-WP010-E-"), "mutation {index}: {error}");
        }
    }

    #[test]
    fn raw_io_oracles_reject_loss_counters_grammar_and_allocation_overrun() {
        let baseline = reports();
        let mutations: Vec<ReportMutation> = vec![
            Box::new(|report| {
                report.diagnostic_loss =
                    DiagnosticLossEvidenceV1::PrefixTruncated { dropped_bytes: 1 };
            }),
            Box::new(|report| report.diagnostic_total_bytes += 1),
            Box::new(|report| report.diagnostic_tail_sha256 = "0".repeat(64)),
            Box::new(|report| report.progress_observations.record_count += 1),
            Box::new(|report| report.progress_observations.parser_steps += 1),
            Box::new(|report| {
                let malformed = b"frame=1\nprogress=end\ntrailing=1\n";
                report.progress_transcript_hex = hex_digest(malformed);
                report.progress_transcript_sha256 = sha256(malformed);
            }),
            Box::new(|report| {
                let truncated = b"frame=1\nprogress=continue\n";
                report.progress_transcript_hex = hex_digest(truncated);
                report.progress_transcript_sha256 = sha256(truncated);
            }),
        ];
        for (index, mutation) in mutations.into_iter().enumerate() {
            let mut candidate = baseline.clone();
            mutation(&mut candidate[0]);
            let error = validate_and_aggregate(&candidate, &context())
                .expect_err("raw I/O oracle mutation must fail");
            assert!(error.contains("FF-WP010-E-"), "mutation {index}: {error}");
        }

        let mut allocation = baseline;
        let request: RequestEvidenceV1 =
            serde_json::from_str(&allocation[0].request_contract_canonical_json)
                .expect("request fixture");
        let capacity = progress_transcript_capacity(&request.resources, &request.limits)
            .expect("governed transcript capacity");
        let mut transcript = Vec::new();
        while transcript.len() <= capacity {
            transcript.extend_from_slice(b"frame=1\nprogress=continue\n");
        }
        transcript.extend_from_slice(b"progress=end\n");
        let observations = replay_progress_transcript(&transcript, &request.limits)
            .expect("capacity-overrun transcript remains grammatically valid");
        allocation[0].progress_transcript_hex = hex_digest(&transcript);
        allocation[0].progress_transcript_sha256 = sha256(&transcript);
        allocation[0].progress_observations = observations;
        let error = validate_and_aggregate(&allocation, &context())
            .expect_err("valid grammar cannot exceed governed transcript allocation");
        assert!(error.contains("RAW-PROGRESS"), "{error}");
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires installed platform-native FFmpeg, ffprobe, WSL2 Ubuntu, and Linux-native proof tools"]
    fn real_wrapper_boundary_tool_manifests_are_independently_observable() {
        let manifest_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let root = manifest_root.parent().expect("repository root");
        let windows =
            observe_boundary_tools(root, ProofPlatform::WindowsX86_64).expect("Windows manifest");
        let linux =
            observe_boundary_tools(root, ProofPlatform::LinuxX86_64).expect("Linux manifest");
        assert_eq!(
            unique_boundary_tool(&windows, ProducerBoundaryToolRoleV1::Ffmpeg)
                .expect("Windows FFmpeg")
                .normalized_probe_sha256
                .as_ref()
                .map(String::len),
            Some(64)
        );
        assert_eq!(
            unique_boundary_tool(&linux, ProducerBoundaryToolRoleV1::Unshare)
                .expect("Linux unshare")
                .content_sha256
                .len(),
            64
        );
    }

    #[test]
    fn producer_tool_and_fixture_receipts_reject_stale_substitution() {
        let tool = ProducerBoundaryToolIdentityV1 {
            role: ProducerBoundaryToolRoleV1::Cargo,
            canonical_path: "C:\\proof\\cargo.exe".to_owned(),
            size_bytes: 1,
            content_sha256: "1".repeat(64),
            file_identity: "windows:1:00000000000000000000000000000001".to_owned(),
            normalized_version: "cargo 1.97.1".to_owned(),
            version_output_sha256: "2".repeat(64),
            normalized_probe_sha256: None,
            capabilities: Vec::new(),
        };
        let observed = vec![tool.clone()];
        let mut mutations = Vec::new();
        for field in 0..6 {
            let mut claimed = observed.clone();
            match field {
                0 => claimed[0].canonical_path.push_str(".old"),
                1 => claimed[0].size_bytes += 1,
                2 => claimed[0].content_sha256 = "3".repeat(64),
                3 => claimed[0].file_identity.push_str("-stale"),
                4 => claimed[0].normalized_version.push_str("-forged"),
                5 => claimed[0].version_output_sha256 = "4".repeat(64),
                _ => unreachable!(),
            }
            mutations.push(claimed);
        }
        for claimed in mutations {
            assert!(
                require_exact_boundary_manifest(&claimed, &observed)
                    .expect_err("stale tool substitution must fail")
                    .contains("PRODUCER-TOOL")
            );
        }

        let fixture = vec![ProducerFixtureFileV1 {
            path: "input/audio.aac".to_owned(),
            size_bytes: 32,
            content_sha256: "5".repeat(64),
        }];
        let mut changed = fixture.clone();
        changed[0].content_sha256 = "6".repeat(64);
        assert!(require_exact_fixture_manifest(&fixture, &changed, &fixture).is_err());
        assert!(require_exact_fixture_manifest(&fixture, &fixture, &changed).is_err());

        let executable = std::env::current_exe().expect("test executable");
        assert!(
            hash_file_bounded_until(&executable, Instant::now())
                .expect_err("expired hash deadline must fail before reading")
                .contains("DEADLINE")
        );

        let cumulative_deadline = Instant::now() + Duration::from_millis(25);
        assert!(remaining_discovery_time(cumulative_deadline).is_ok());
        thread::sleep(Duration::from_millis(15));
        assert!(remaining_discovery_time(cumulative_deadline).is_ok());
        thread::sleep(Duration::from_millis(15));
        assert!(
            remaining_discovery_time(cumulative_deadline)
                .expect_err("later manifest command cannot reset the shared deadline")
                .contains("DEADLINE")
        );
    }

    #[test]
    fn producer_command_projection_rejects_every_caller_controlled_surface() {
        let baseline = ProducerCommandProjectionV1 {
            program: "C:\\proof\\cargo.exe".to_owned(),
            arguments: vec!["run".to_owned()],
            build_program: "C:\\proof\\cargo.exe".to_owned(),
            build_arguments: vec!["build".to_owned()],
            cargo_target_directory: "C:\\proof\\target".to_owned(),
            working_directory: "C:\\proof\\cwd".to_owned(),
            fixture_root: "C:\\proof\\fixtures".to_owned(),
            injected_environment: BTreeMap::from([(
                "FFORAGER_WP010_SOURCE_COMMIT".to_owned(),
                "a".repeat(40),
            )]),
            discovery_timeout_millis: duration_millis(PRODUCER_DISCOVERY_TIMEOUT),
            build_timeout_millis: duration_millis(PRODUCER_BUILD_TIMEOUT),
            producer_timeout_millis: duration_millis(PRODUCER_EXECUTION_TIMEOUT),
            build_outer_timeout_millis: duration_millis(PRODUCER_BUILD_OUTER_TIMEOUT),
            producer_outer_timeout_millis: duration_millis(PRODUCER_EXECUTION_OUTER_TIMEOUT),
        };
        require_exact_command_projection(&baseline, &baseline).expect("exact projection");

        for mutation in 0..13 {
            let mut candidate = baseline.clone();
            match mutation {
                0 => candidate.program.push_str(".forged"),
                1 => candidate.arguments.insert(0, "--forged-prefix".to_owned()),
                2 => candidate.build_program.push_str(".forged"),
                3 => candidate.build_arguments.push("--forged".to_owned()),
                4 => candidate.cargo_target_directory.push_str("-forged"),
                5 => candidate.working_directory.push_str("-forged"),
                6 => candidate.fixture_root.push_str("-forged"),
                7 => {
                    candidate
                        .injected_environment
                        .insert("RUSTC_WRAPPER".to_owned(), "forger".to_owned());
                }
                8 => candidate.discovery_timeout_millis += 1,
                9 => candidate.build_timeout_millis += 1,
                10 => candidate.producer_timeout_millis += 1,
                11 => candidate.build_outer_timeout_millis += 1,
                12 => candidate.producer_outer_timeout_millis += 1,
                _ => unreachable!(),
            }
            assert!(
                require_exact_command_projection(&candidate, &baseline)
                    .expect_err("command projection mutation must fail")
                    .contains("PRODUCER-COMMAND"),
                "mutation {mutation}"
            );
        }
    }

    #[test]
    fn producer_source_receipt_rejects_forgery_and_restored_mutation() {
        fn input() -> ReportInput {
            let [report, _linux] = reports();
            report_input(
                format!("{REPORT_ROOT}/windows.json"),
                report_bytes(&report),
                report,
            )
        }
        let baseline = input();
        validate_producer_source_binding(&baseline.producer_receipt, &baseline, &context())
            .expect("baseline source receipt");

        for mutation in 0..6 {
            let mut candidate = input();
            match mutation {
                0 => candidate.producer_receipt.source_before.dirty = true,
                1 => candidate.producer_receipt.source_after.content_fingerprint = "0".repeat(64),
                2 => candidate.producer_receipt.source_before.git_commit = "0".repeat(40),
                3 => {
                    candidate.producer_receipt.source_before.content_fingerprint = "1".repeat(64);
                    candidate.producer_receipt.source_after.content_fingerprint = "1".repeat(64);
                }
                4 => candidate.producer_receipt.report_sha256 = "2".repeat(64),
                5 => candidate.producer_receipt.report_path.push_str(".forged"),
                _ => unreachable!(),
            }
            assert!(
                validate_producer_source_binding(
                    &candidate.producer_receipt,
                    &candidate,
                    &context()
                )
                .expect_err("producer source forgery must fail")
                .contains("PRODUCER-PROVENANCE"),
                "mutation {mutation}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires WSL2 Ubuntu with unshare, timeout, setsid, bash, sleep, and pgrep"]
    fn wsl_pid_namespace_timeout_reaps_setsid_pipe_escape() {
        const DISTRO: &str = "Ubuntu";
        let wsl = system32_program("wsl.exe").expect("absolute WSL launcher");
        let manifest_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let root = manifest_root.parent().expect("repository root");
        let linux_root = windows_path_to_wsl(root, DISTRO).expect("Linux repository path");
        let marker = format!("wp010-wrapper-escape-{}", std::process::id());
        let script = format!(
            "/usr/bin/setsid /bin/bash -c 'exec -a {marker} /usr/bin/sleep 60' & /usr/bin/sleep 60"
        );
        let environment = BTreeMap::from([
            ("HOME".to_owned(), "/tmp".to_owned()),
            ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
            ("LANG".to_owned(), "C.UTF-8".to_owned()),
            ("LC_ALL".to_owned(), "C.UTF-8".to_owned()),
        ]);
        let arguments = wsl_command_arguments(
            DISTRO,
            &linux_root,
            &environment,
            "/bin/bash",
            &["-c".to_owned(), script],
            Duration::from_secs(1),
        );
        let started = Instant::now();
        let output = execute_bounded_wsl_command(
            Command::new(&wsl).args(&arguments).current_dir(root),
            "WSL PID namespace escape regression",
            Duration::from_secs(1),
            Duration::from_secs(16),
        )
        .expect("native timeout must return and close inherited pipes");
        assert!(!output.status.success(), "inner timeout must fire");
        assert!(started.elapsed() < Duration::from_secs(16));
        let absence = execute_bounded_command_with_limit(
            Command::new(&wsl).args(["-d", DISTRO, "--exec", "/usr/bin/pgrep", "-f", &marker]),
            "WSL PID namespace escaped-process absence",
            Duration::from_secs(5),
            MAX_TEXT_BYTES,
        )
        .expect("bounded pgrep");
        assert!(
            !absence.status.success() && absence.stdout.is_empty(),
            "setsid escape survived namespace teardown: {}",
            String::from_utf8_lossy(&absence.stdout)
        );
    }

    #[test]
    fn forged_or_stale_consumer_receipt_is_rejected() {
        let reports = reports();
        let aggregate = validate_and_aggregate(&reports, &context()).expect("valid reports");
        let inputs = [
            report_input(
                format!("{REPORT_ROOT}/windows.json"),
                report_bytes(&reports[0]),
                reports[0].clone(),
            ),
            report_input(
                format!("{REPORT_ROOT}/linux.json"),
                report_bytes(&reports[1]),
                reports[1].clone(),
            ),
        ];
        let mut receipt = build_receipt(
            context().source_commit,
            context().fixture_contract_sha256,
            &inputs,
            aggregate,
        );
        receipt.proof_class = "production_runtime".to_owned();
        assert!(
            validate_receipt(&receipt, &inputs, &context())
                .unwrap_err()
                .contains("FORGED-RECEIPT")
        );
        receipt.proof_class = PROOF_CLASS.to_owned();
        receipt.source_commit = "c".repeat(40);
        assert!(
            validate_receipt(&receipt, &inputs, &context())
                .unwrap_err()
                .contains("FORGED-RECEIPT")
        );
        receipt.source_commit = context().source_commit.to_owned();
        receipt.inputs[0].report_sha256 = "0".repeat(64);
        assert!(
            validate_receipt(&receipt, &inputs, &context())
                .unwrap_err()
                .contains("FORGED-RECEIPT")
        );
    }

    #[test]
    fn unknown_report_fields_and_unsafe_paths_fail_closed() {
        let mut mutated = serde_json::to_value(&reports()[0]).expect("fixture JSON");
        mutated
            .as_object_mut()
            .expect("report object")
            .insert("unexpected".to_owned(), serde_json::Value::Bool(true));
        assert!(serde_json::from_value::<PlatformProofReportV2>(mutated).is_err());
        assert!(safe_relative_path("../outside.json").is_err());
        assert!(safe_relative_path("C:\\outside.json").is_err());
    }

    #[test]
    fn platform_proof_v2_rejects_old_hybrid_and_missing_scope_shapes() {
        let mut old_v1 = serde_json::to_value(&reports()[0]).expect("fixture JSON");
        let old = old_v1.as_object_mut().expect("report object");
        old.insert(
            "schema_id".to_owned(),
            serde_json::Value::String("ff.ffmpeg-platform-proof@1".to_owned()),
        );
        old.get_mut("producer_phase_limits")
            .and_then(serde_json::Value::as_object_mut)
            .expect("producer limits")
            .remove("scope_settlement_timeout_millis");
        old.get_mut("phase_deadlines")
            .and_then(serde_json::Value::as_object_mut)
            .expect("phase deadlines")
            .remove("scope_settlement_millis");
        assert!(
            serde_json::from_value::<PlatformProofReportV2>(old_v1).is_err(),
            "the old @1 shape must not parse as the V2 DTO"
        );

        let mut hybrid = reports();
        hybrid[0].schema_id = "ff.ffmpeg-platform-proof@1".to_owned();
        let error = validate_and_aggregate(&hybrid, &context())
            .expect_err("an @1 label cannot authorize V2 fields");
        assert!(error.contains("REPORT-SCHEMA"), "{error}");

        for field in ["scope_settlement_timeout_millis", "scope_settlement_millis"] {
            let mut missing = serde_json::to_value(&reports()[0]).expect("fixture JSON");
            let container = if field.ends_with("timeout_millis") {
                "producer_phase_limits"
            } else {
                "phase_deadlines"
            };
            missing
                .as_object_mut()
                .expect("report object")
                .get_mut(container)
                .and_then(serde_json::Value::as_object_mut)
                .expect("scope container")
                .remove(field);
            assert!(
                serde_json::from_value::<PlatformProofReportV2>(missing).is_err(),
                "missing {field} must fail closed"
            );
        }
    }
}
