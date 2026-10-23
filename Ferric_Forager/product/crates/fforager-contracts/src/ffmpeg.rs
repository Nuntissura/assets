//! Versioned, bounded, data-only `FFmpeg` and `ffprobe` supervision contracts.
//!
//! This module describes trusted executable identity, one deliberately narrow
//! stream-copy operation, resource and I/O ceilings, and evidence emitted by a
//! future process adapter. It does not spawn a process, inspect a host, hash a
//! file, or prove that a declaration is true. Consumers must independently
//! acquire and verify every identity, digest, lifecycle, reap, containment, and
//! output fact at the process boundary.

use crate::{
    BYTE_CREDIT_SCHEMA_ID, ByteCreditStage, JobId, RESOURCE_VECTOR_SCHEMA_ID, RequestId,
    ResourceVector, SchemaVersion,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

/// Exact schema identity for a version-one supervision request.
pub const FFMPEG_SUPERVISION_SCHEMA_ID: &str = "ff.ffmpeg-supervision@1";
/// Exact schema identity for a version-one supervision report.
pub const FFMPEG_SUPERVISION_REPORT_SCHEMA_ID: &str = "ff.ffmpeg-supervision-report@1";
/// Only accepted version of the closed version-one DTOs.
pub const FFMPEG_SUPERVISION_VERSION: SchemaVersion = SchemaVersion { major: 1, minor: 0 };
/// Canonical byte projection used for request identities in version one.
///
/// The hashed bytes are this UTF-8 identifier, one zero byte, then the compact
/// UTF-8 JSON serialization of the closed DTO in its declared field order.
pub const FFMPEG_REQUEST_PROJECTION_ID: &str = "ff.ffmpeg-request-canonical-json@1";
/// Canonical byte projection used for operation identities in version one,
/// using the same identifier-plus-zero-plus-compact-JSON construction.
pub const FFMPEG_OPERATION_PROJECTION_ID: &str = "ff.ffmpeg-operation-canonical-json@1";
/// Versioned projection for the exact platform-bound vector executed by the
/// focused adapter. This is deliberately distinct from the stable request @1
/// logical pathname vector.
pub const FFMPEG_BOUND_RUNTIME_INVOCATION_PROJECTION_ID: &str =
    "ff.ffmpeg-bound-runtime-invocation-canonical-json@1";

const MAX_PATH_BYTES: usize = 4_096;
const MAX_VERSION_BYTES: usize = 512;
const MAX_FILE_IDENTITY_BYTES: usize = 512;
const MAX_CAPABILITIES: usize = 128;
const MAX_CAPABILITY_BYTES: usize = 128;
const MAX_INPUTS: usize = 32;
const MAX_STREAM_MAPS: usize = 128;
const MAX_ENVIRONMENT_BINDINGS: usize = 4;
const MAX_ARGUMENTS: usize = 512;
const MAX_ARGUMENT_BYTES: usize = 4_096;
const MAX_ARGUMENT_VECTOR_BYTES: usize = 64 * 1_024;
const MAX_INPUTS_U16: u16 = 32;
/// First deterministic inherited input descriptor in bound runtime projection v1.
pub const FFMPEG_LINUX_FD_INPUT_BASE_V1: u16 = 64;
/// Deterministic inherited output descriptor in bound runtime projection v1.
pub const FFMPEG_LINUX_FD_OUTPUT_V1: u16 = FFMPEG_LINUX_FD_INPUT_BASE_V1 + MAX_INPUTS_U16;
const MAX_TIMEOUT_MILLIS: u64 = 60 * 60 * 1_000;
const MAX_PROGRESS_RECORDS: u64 = 1_000_000;
const MAX_PROGRESS_TOTAL_BYTES: u64 = 64 * 1_024 * 1_024;
const MAX_PROGRESS_RECORD_BYTES: u64 = 64 * 1_024;
const MAX_PROGRESS_FIELD_BYTES: u64 = 4 * 1_024;
const MAX_PARSER_STEPS: u64 = 4_000_000;
const MAX_STDERR_TOTAL_BYTES: u64 = 256 * 1_024 * 1_024;
const MAX_STDERR_TAIL_BYTES: u64 = 256 * 1_024;
const MAX_PIPE_ALLOCATION_BYTES: u64 = 8 * 1_024 * 1_024;
const MAX_OUTPUT_STREAMS: u32 = 1_024;
const MAX_OUTPUT_FORMATS: usize = 32;
const MAX_OUTPUT_DURATION_MILLIS: u64 = 31 * 24 * 60 * 60 * 1_000;
const MAX_OUTPUT_FILE_SIZE_BYTES: u64 = 1024 * 1024 * 1024 * 1024;
const MAX_OUTPUT_DIMENSION: u32 = 32_768;
const MAX_OUTPUT_CHANNELS: u32 = 256;
const MAX_REPORT_EVENTS: usize = 32;
const MAX_RESIDUALS: usize = 16;
const MAX_RESIDUAL_BYTES: usize = 1_024;

/// Process executable selected by the typed request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FfmpegExecutableKindV1 {
    Ffmpeg,
    Ffprobe,
}

/// Host operating-system identity bound into executable evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FfmpegHostOperatingSystemV1 {
    Windows,
    Linux,
}

/// Host architecture identity bound into executable evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FfmpegHostArchitectureV1 {
    X86_64,
    Aarch64,
}

/// Exact host identity used for executable and report correlation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FfmpegHostIdentityV1 {
    pub operating_system: FfmpegHostOperatingSystemV1,
    pub architecture: FfmpegHostArchitectureV1,
}

/// Normalized, bounded capability probe bound to one executable content digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FfmpegCapabilityBindingV1 {
    pub executable_content_sha256: String,
    pub normalized_probe_sha256: String,
    pub capabilities: Vec<String>,
}

/// Trusted absolute executable identity supplied to a future process adapter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FfmpegExecutableIdentityV1 {
    pub kind: FfmpegExecutableKindV1,
    pub absolute_path: String,
    pub file_identity: String,
    pub content_sha256: String,
    pub version_output_sha256: String,
    pub normalized_version: String,
    pub host: FfmpegHostIdentityV1,
    pub capability_binding: FfmpegCapabilityBindingV1,
}

/// Required `FFmpeg` and `ffprobe` identities for one request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FfmpegToolchainIdentityV1 {
    pub ffmpeg: FfmpegExecutableIdentityV1,
    pub ffprobe: FfmpegExecutableIdentityV1,
}

/// Protocols accepted by this first file-output profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FfmpegProtocolV1 {
    File,
}

/// Input mechanisms accepted by this first file-output profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FfmpegInputMechanismV1 {
    AuditedElementaryFile,
}

/// Closed single-file demuxers that do not resolve playlists, sequences, or
/// external references in the Phase 0 supervision profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FfmpegInputDemuxerV1 {
    AacAdts,
    H264AnnexB,
}

impl FfmpegInputDemuxerV1 {
    const fn argument_name(self) -> &'static str {
        match self {
            Self::AacAdts => "aac",
            Self::H264AnnexB => "h264",
        }
    }
}

/// One job-relative input bound to an audited non-transitive demuxer.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FfmpegInputFileV1 {
    pub path: String,
    pub demuxer: FfmpegInputDemuxerV1,
}

/// Stream class used to derive a direct `-map` argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FfmpegStreamKindV1 {
    Video,
    Audio,
    Subtitle,
    Data,
    Attachment,
}

impl FfmpegStreamKindV1 {
    const fn argument_code(self) -> &'static str {
        match self {
            Self::Video => "v",
            Self::Audio => "a",
            Self::Subtitle => "s",
            Self::Data => "d",
            Self::Attachment => "t",
        }
    }
}

/// Typed stream selection that cannot contain shell or arbitrary command text.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FfmpegStreamMapV1 {
    pub input_index: u16,
    pub stream_kind: FfmpegStreamKindV1,
    pub stream_index: u16,
    pub source_payload_sha256: String,
}

impl FfmpegStreamMapV1 {
    fn argument(&self) -> String {
        format!(
            "{}:{}:{}",
            self.input_index,
            self.stream_kind.argument_code(),
            self.stream_index
        )
    }
}

/// Narrow Phase 0 operation: explicit-map file-to-file stream copy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FfmpegStreamCopyPlanV1 {
    pub inputs: Vec<FfmpegInputFileV1>,
    pub stream_maps: Vec<FfmpegStreamMapV1>,
    pub output_path: String,
    pub output_muxer: String,
}

/// Typed `FFmpeg` operation. Adding another kind requires a schema revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum FfmpegOperationPlanV1 {
    StreamCopy(FfmpegStreamCopyPlanV1),
}

/// Environment values are synthesized by the adapter from trusted host and
/// job-root context; the request cannot supply executable environment text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FfmpegEnvironmentBindingV1 {
    HostSystemRoot,
    JobTemporaryDirectory,
    LocaleC,
}

/// Environment policy applied before process creation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FfmpegEnvironmentPolicyV1 {
    pub inherit_parent: bool,
    pub trusted_bindings: Vec<FfmpegEnvironmentBindingV1>,
}

/// Machine-readable progress stream and diagnostic stream ownership.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FfmpegProgressChannelV1 {
    StdoutPipeOne,
}

/// Human diagnostic output is retained only as a bounded tail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FfmpegDiagnosticChannelV1 {
    StderrTail,
}

/// Closed file-output I/O profile; media never shares the progress pipe.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FfmpegIoPolicyV1 {
    pub progress_channel: FfmpegProgressChannelV1,
    pub diagnostic_channel: FfmpegDiagnosticChannelV1,
    pub output_is_job_scoped_file: bool,
    pub stdin_disabled: bool,
}

/// Every variable-size and time-dependent child boundary has an explicit limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FfmpegSupervisionLimitsV1 {
    pub startup_timeout_millis: u64,
    pub execution_timeout_millis: u64,
    pub graceful_stop_timeout_millis: u64,
    pub forced_kill_timeout_millis: u64,
    pub reap_timeout_millis: u64,
    pub ffprobe_timeout_millis: u64,
    pub progress_max_records: u64,
    pub progress_max_total_bytes: u64,
    pub progress_max_record_bytes: u64,
    pub progress_max_field_bytes: u64,
    pub progress_max_parser_steps: u64,
    pub progress_max_silence_millis: u64,
    pub consumer_stall_timeout_millis: u64,
    pub stderr_max_total_bytes: u64,
    pub stderr_tail_bytes: u64,
    pub pipe_allocation_bytes: u64,
    pub output_max_streams: u32,
    pub output_max_duration_millis: u64,
    pub output_max_file_size_bytes: u64,
    pub output_max_width: u32,
    pub output_max_height: u32,
    pub output_max_channels: u32,
}

/// Resource-ledger identities and exact immutable claim for this operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FfmpegResourceReferenceV1 {
    pub resource_contract_schema_id: String,
    pub byte_credit_contract_schema_id: String,
    pub pipe_stage: ByteCreditStage,
    pub claim: ResourceVector,
    pub progress_pipe_bytes: u64,
    pub stderr_pipe_bytes: u64,
}

/// Complete version-one supervision request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FfmpegSupervisionRequestV1 {
    pub schema_id: String,
    pub version: SchemaVersion,
    pub request_id: RequestId,
    pub job_id: JobId,
    pub operation_plan_sha256: String,
    pub toolchain: FfmpegToolchainIdentityV1,
    pub operation: FfmpegOperationPlanV1,
    pub environment: FfmpegEnvironmentPolicyV1,
    pub working_directory: String,
    pub allowed_protocols: Vec<FfmpegProtocolV1>,
    pub allowed_input_mechanisms: Vec<FfmpegInputMechanismV1>,
    pub io: FfmpegIoPolicyV1,
    pub limits: FfmpegSupervisionLimitsV1,
    pub resources: FfmpegResourceReferenceV1,
}

/// Lifecycle states observable after a child has been created.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FfmpegLifecycleStateV1 {
    Spawned,
    Running,
    GracefulStopRequested,
    ForcedKillRequested,
    Reaped,
    ReapUnproven,
    Validated,
}

/// Ordered lifecycle observation. Sequences start at one and are contiguous.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FfmpegLifecycleObservationV1 {
    pub sequence: u64,
    pub state: FfmpegLifecycleStateV1,
    pub monotonic_millis: u64,
}

/// Stable failure classification safe to expose without raw child text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FfmpegFailureKindV1 {
    ExecutableMissing,
    ExecutableIdentityChanged,
    CapabilityMismatch,
    InvalidPlan,
    SpawnFailed,
    TimedOut,
    ProgressInvalid,
    DiagnosticLimitExceeded,
    ExitFailed,
    ReapFailed,
    ContainmentUnproven,
    OutputInvalid,
}

/// Terminal classification separated from lifecycle observations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FfmpegTerminalOutcomeV1 {
    Validated {},
    PreflightRejected { reason: FfmpegFailureKindV1 },
    Failed { reason: FfmpegFailureKindV1 },
    Cancelled { forced: bool },
}

/// Direct-child exit observation captured by a real wait operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FfmpegDirectChildReapV1 {
    NotSpawned {},
    ReapUnproven {
        wait_error_sha256: String,
    },
    Reaped {
        exit_code: Option<i32>,
        terminated_by_signal_or_exception: bool,
        wait_receipt_sha256: String,
    },
}

/// Independent Windows Job active-process query outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WindowsActiveProcessQueryV1 {
    Zero {},
    Nonzero { count: u32 },
    QueryFailed {},
}

/// Truthfully scoped process containment evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum FfmpegContainmentEvidenceV1 {
    NotEstablished {},
    WindowsJob {
        attached_before_execution: bool,
        kill_on_job_close: bool,
        active_process_query: WindowsActiveProcessQueryV1,
    },
    UnixProcessGroup {
        declared_nonescaping_members_remaining: u32,
        setsid_escape_capable: bool,
    },
}

/// Normalized ffprobe stream fact; unknown raw fields never cross this boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FfmpegOutputStreamFactV1 {
    pub index: u32,
    pub kind: FfmpegStreamKindV1,
    pub selected_source: FfmpegStreamMapV1,
    pub output_payload_sha256: String,
    pub codec_name: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub channels: Option<u32>,
}

/// Bounded normalized facts required before output eligibility.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FfmpegOutputFactsV1 {
    pub output_identity_sha256: String,
    pub file_size_bytes: u64,
    pub duration_millis: u64,
    pub format_names: Vec<String>,
    pub streams: Vec<FfmpegOutputStreamFactV1>,
}

/// Output validation state bound to the exact ffprobe executable identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum FfmpegOutputValidationV1 {
    NotRun {},
    Rejected {
        reason: FfmpegFailureKindV1,
    },
    Validated {
        ffprobe_content_sha256: String,
        facts: FfmpegOutputFactsV1,
    },
}

/// Strict report correlated to one request without disclosing request paths.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FfmpegSupervisionReportV1 {
    pub schema_id: String,
    pub version: SchemaVersion,
    pub request_id: RequestId,
    pub job_id: JobId,
    pub source_commit: String,
    pub request_contract_sha256: String,
    pub operation_plan_sha256: String,
    pub ffmpeg_content_sha256: String,
    pub ffprobe_content_sha256: String,
    pub host: FfmpegHostIdentityV1,
    pub lifecycle: Vec<FfmpegLifecycleObservationV1>,
    pub terminal_outcome: FfmpegTerminalOutcomeV1,
    pub direct_child: FfmpegDirectChildReapV1,
    pub containment: FfmpegContainmentEvidenceV1,
    pub output_validation: FfmpegOutputValidationV1,
    pub residual_uncertainty: Vec<String>,
}

/// Pre-spawn result that binds the exact validated request projection to the
/// only argument vector an adapter may execute. Private fields prevent callers
/// from constructing an invocation that bypasses request and digest checks.
#[derive(Debug, PartialEq, Eq)]
pub struct FfmpegValidatedInvocationV1 {
    request_contract_sha256: String,
    operation_plan_sha256: String,
    arguments: Vec<String>,
    bound_runtime: FfmpegBoundRuntimeInvocationV1,
}

/// Exact platform-bound argument projection. Private fields ensure only a
/// fully validated request can produce executable runtime arguments.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct FfmpegBoundRuntimeInvocationV1 {
    projection_id: &'static str,
    request_contract_sha256: String,
    operation_plan_sha256: String,
    arguments: Vec<String>,
    canonical_sha256: String,
}

#[derive(Serialize)]
struct BoundRuntimeProjection<'a> {
    request_contract_sha256: &'a str,
    operation_plan_sha256: &'a str,
    arguments: &'a [String],
}

impl FfmpegBoundRuntimeInvocationV1 {
    #[must_use]
    pub fn projection_id(&self) -> &str {
        self.projection_id
    }

    #[must_use]
    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }

    #[must_use]
    pub fn canonical_sha256(&self) -> &str {
        &self.canonical_sha256
    }
}

impl FfmpegValidatedInvocationV1 {
    /// Recompute the complete validated boundary and require this token to
    /// remain correlated with the exact request presented to an adapter.
    ///
    /// # Errors
    ///
    /// Returns the request validation error or `CorrelationMismatch` when a
    /// valid token is stale or belongs to another request.
    pub fn validate_against(
        &self,
        request: &FfmpegSupervisionRequestV1,
    ) -> Result<(), FfmpegContractError> {
        let expected = request.validate_for_invocation()?;
        if self == &expected {
            Ok(())
        } else {
            Err(FfmpegContractError::CorrelationMismatch {
                field: "validated_invocation",
            })
        }
    }

    /// Independently computed identity of the exact validated request.
    #[must_use]
    pub fn request_contract_sha256(&self) -> &str {
        &self.request_contract_sha256
    }

    /// Independently computed identity of the exact validated operation.
    #[must_use]
    pub fn operation_plan_sha256(&self) -> &str {
        &self.operation_plan_sha256
    }

    /// Exact direct argument vector bound to the validated request.
    #[must_use]
    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }

    /// Exact versioned platform-bound vector that the adapter may execute.
    #[must_use]
    pub fn bound_runtime(&self) -> &FfmpegBoundRuntimeInvocationV1 {
        &self.bound_runtime
    }
}

/// Typed contract validation failure. These are structural or semantic DTO
/// failures, never proof that an external process behaved as declared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FfmpegContractError {
    SchemaIdMismatch,
    UnsupportedVersion(SchemaVersion),
    EmptyField {
        field: &'static str,
    },
    FieldTooLong {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },
    InvalidField {
        field: &'static str,
    },
    InvalidDigest {
        field: &'static str,
    },
    CountLimitExceeded {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },
    InvalidAbsolutePath {
        field: &'static str,
    },
    InvalidJobPath {
        field: &'static str,
    },
    CapabilityBindingMismatch,
    CapabilityInventoryNotCanonical,
    MissingCapability {
        capability: String,
    },
    ToolchainKindMismatch,
    ToolchainHostMismatch,
    EnvironmentNotScrubbed,
    EnvironmentInventoryNotCanonical,
    InvalidProtocolAllowlist,
    InvalidInputMechanismAllowlist,
    InvalidIoPolicy,
    InvalidStreamMap,
    InvalidLimit {
        field: &'static str,
    },
    InvalidResourceReference,
    InvalidResourceClaim,
    ArgumentLimitExceeded,
    CanonicalProjectionFailed,
    CorrelationMismatch {
        field: &'static str,
    },
    InvalidLifecycle,
    InvalidDirectChildEvidence,
    InvalidContainmentEvidence,
    InvalidOutputFacts,
    SuccessBeforeReapAndValidation,
    ResidualUncertaintyRequired,
}

impl fmt::Display for FfmpegContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid FFmpeg supervision contract: {self:?}")
    }
}

impl std::error::Error for FfmpegContractError {}

impl FfmpegExecutableIdentityV1 {
    fn validate(&self) -> Result<(), FfmpegContractError> {
        validate_absolute_path(
            "executable.absolute_path",
            &self.absolute_path,
            self.host.operating_system,
        )?;
        validate_text(
            "executable.file_identity",
            &self.file_identity,
            MAX_FILE_IDENTITY_BYTES,
        )?;
        validate_digest("executable.content_sha256", &self.content_sha256)?;
        validate_digest(
            "executable.version_output_sha256",
            &self.version_output_sha256,
        )?;
        validate_text(
            "executable.normalized_version",
            &self.normalized_version,
            MAX_VERSION_BYTES,
        )?;
        validate_digest(
            "capability.normalized_probe_sha256",
            &self.capability_binding.normalized_probe_sha256,
        )?;
        validate_digest(
            "capability.executable_content_sha256",
            &self.capability_binding.executable_content_sha256,
        )?;
        if self.capability_binding.executable_content_sha256 != self.content_sha256 {
            return Err(FfmpegContractError::CapabilityBindingMismatch);
        }
        validate_capabilities(&self.capability_binding.capabilities)
    }
}

impl FfmpegStreamCopyPlanV1 {
    fn validate(&self, host: FfmpegHostOperatingSystemV1) -> Result<(), FfmpegContractError> {
        validate_count("operation.inputs", self.inputs.len(), MAX_INPUTS)?;
        validate_count(
            "operation.stream_maps",
            self.stream_maps.len(),
            MAX_STREAM_MAPS,
        )?;
        if self.inputs.is_empty() || self.stream_maps.is_empty() {
            return Err(FfmpegContractError::InvalidStreamMap);
        }
        for input in &self.inputs {
            validate_job_path("operation.input_path", &input.path, host)?;
        }
        for (index, input) in self.inputs.iter().enumerate() {
            if self.inputs[index + 1..]
                .iter()
                .any(|candidate| paths_alias(&input.path, &candidate.path, host))
            {
                return Err(FfmpegContractError::InvalidField {
                    field: "operation.inputs",
                });
            }
        }
        validate_job_path("operation.output_path", &self.output_path, host)?;
        if self
            .inputs
            .iter()
            .any(|input| paths_alias(&input.path, &self.output_path, host))
        {
            return Err(FfmpegContractError::InvalidJobPath {
                field: "operation.output_path",
            });
        }
        validate_component(
            "operation.output_muxer",
            &self.output_muxer,
            MAX_CAPABILITY_BYTES,
        )?;
        if !self.stream_maps.windows(2).all(|pair| {
            (
                pair[0].input_index,
                pair[0].stream_kind,
                pair[0].stream_index,
            ) < (
                pair[1].input_index,
                pair[1].stream_kind,
                pair[1].stream_index,
            )
        }) {
            return Err(FfmpegContractError::InvalidStreamMap);
        }
        if self
            .stream_maps
            .iter()
            .any(|mapping| usize::from(mapping.input_index) >= self.inputs.len())
        {
            return Err(FfmpegContractError::InvalidStreamMap);
        }
        for mapping in &self.stream_maps {
            validate_digest(
                "operation.stream_map.source_payload_sha256",
                &mapping.source_payload_sha256,
            )?;
        }
        Ok(())
    }

    fn direct_arguments(&self) -> Vec<String> {
        let mut arguments = vec![
            "-hide_banner".to_owned(),
            "-nostdin".to_owned(),
            "-n".to_owned(),
            "-protocol_whitelist".to_owned(),
            "file,pipe".to_owned(),
            "-progress".to_owned(),
            "pipe:1".to_owned(),
        ];
        for input in &self.inputs {
            arguments.push("-protocol_whitelist".to_owned());
            arguments.push("file".to_owned());
            arguments.push("-f".to_owned());
            arguments.push(input.demuxer.argument_name().to_owned());
            arguments.push("-i".to_owned());
            arguments.push(input.path.clone());
        }
        for mapping in &self.stream_maps {
            arguments.push("-map".to_owned());
            arguments.push(mapping.argument());
        }
        arguments.extend([
            "-protocol_whitelist".to_owned(),
            "file".to_owned(),
            "-c".to_owned(),
            "copy".to_owned(),
            "-f".to_owned(),
            self.output_muxer.clone(),
            self.output_path.clone(),
        ]);
        arguments
    }

    fn linux_fd_arguments(&self) -> Vec<String> {
        let mut arguments = vec![
            "-hide_banner".to_owned(),
            "-nostdin".to_owned(),
            "-y".to_owned(),
            "-protocol_whitelist".to_owned(),
            "fd,pipe".to_owned(),
            "-progress".to_owned(),
            "pipe:1".to_owned(),
        ];
        for (input_index, input) in self.inputs.iter().enumerate() {
            arguments.extend([
                "-protocol_whitelist".to_owned(),
                "fd".to_owned(),
                "-f".to_owned(),
                input.demuxer.argument_name().to_owned(),
                "-fd".to_owned(),
                (FFMPEG_LINUX_FD_INPUT_BASE_V1
                    + u16::try_from(input_index).expect("input count is contract bounded"))
                .to_string(),
                "-i".to_owned(),
                "fd:".to_owned(),
            ]);
        }
        for mapping in &self.stream_maps {
            arguments.extend(["-map".to_owned(), mapping.argument()]);
        }
        arguments.extend([
            "-protocol_whitelist".to_owned(),
            "fd".to_owned(),
            "-c".to_owned(),
            "copy".to_owned(),
            "-f".to_owned(),
            self.output_muxer.clone(),
            "-fd".to_owned(),
            FFMPEG_LINUX_FD_OUTPUT_V1.to_string(),
            "fd:".to_owned(),
        ]);
        arguments
    }
}

impl FfmpegSupervisionLimitsV1 {
    fn validate(self) -> Result<(), FfmpegContractError> {
        self.validate_time_limits()?;
        self.validate_progress_limits()?;
        self.validate_diagnostic_limits()?;
        if self.output_max_streams == 0 || self.output_max_streams > MAX_OUTPUT_STREAMS {
            return Err(FfmpegContractError::InvalidLimit {
                field: "output_max_streams",
            });
        }
        validate_nonzero_limit(
            "output_max_duration_millis",
            self.output_max_duration_millis,
            MAX_OUTPUT_DURATION_MILLIS,
        )?;
        validate_nonzero_limit(
            "output_max_file_size_bytes",
            self.output_max_file_size_bytes,
            MAX_OUTPUT_FILE_SIZE_BYTES,
        )?;
        for (field, value, maximum) in [
            (
                "output_max_width",
                u64::from(self.output_max_width),
                u64::from(MAX_OUTPUT_DIMENSION),
            ),
            (
                "output_max_height",
                u64::from(self.output_max_height),
                u64::from(MAX_OUTPUT_DIMENSION),
            ),
            (
                "output_max_channels",
                u64::from(self.output_max_channels),
                u64::from(MAX_OUTPUT_CHANNELS),
            ),
        ] {
            validate_nonzero_limit(field, value, maximum)?;
        }
        Ok(())
    }

    fn validate_time_limits(self) -> Result<(), FfmpegContractError> {
        for (field, value, maximum) in [
            (
                "startup_timeout_millis",
                self.startup_timeout_millis,
                MAX_TIMEOUT_MILLIS,
            ),
            (
                "execution_timeout_millis",
                self.execution_timeout_millis,
                MAX_TIMEOUT_MILLIS,
            ),
            (
                "graceful_stop_timeout_millis",
                self.graceful_stop_timeout_millis,
                MAX_TIMEOUT_MILLIS,
            ),
            (
                "forced_kill_timeout_millis",
                self.forced_kill_timeout_millis,
                MAX_TIMEOUT_MILLIS,
            ),
            (
                "reap_timeout_millis",
                self.reap_timeout_millis,
                MAX_TIMEOUT_MILLIS,
            ),
            (
                "ffprobe_timeout_millis",
                self.ffprobe_timeout_millis,
                MAX_TIMEOUT_MILLIS,
            ),
        ] {
            validate_nonzero_limit(field, value, maximum)?;
        }
        Ok(())
    }

    fn validate_progress_limits(self) -> Result<(), FfmpegContractError> {
        for (field, value, maximum) in [
            (
                "progress_max_records",
                self.progress_max_records,
                MAX_PROGRESS_RECORDS,
            ),
            (
                "progress_max_total_bytes",
                self.progress_max_total_bytes,
                MAX_PROGRESS_TOTAL_BYTES,
            ),
            (
                "progress_max_record_bytes",
                self.progress_max_record_bytes,
                MAX_PROGRESS_RECORD_BYTES,
            ),
            (
                "progress_max_field_bytes",
                self.progress_max_field_bytes,
                MAX_PROGRESS_FIELD_BYTES,
            ),
            (
                "progress_max_parser_steps",
                self.progress_max_parser_steps,
                MAX_PARSER_STEPS,
            ),
            (
                "progress_max_silence_millis",
                self.progress_max_silence_millis,
                MAX_TIMEOUT_MILLIS,
            ),
        ] {
            validate_nonzero_limit(field, value, maximum)?;
        }
        if self.progress_max_field_bytes > self.progress_max_record_bytes
            || self.progress_max_record_bytes > self.progress_max_total_bytes
        {
            return Err(FfmpegContractError::InvalidLimit {
                field: "nested_progress_limits",
            });
        }
        Ok(())
    }

    fn validate_diagnostic_limits(self) -> Result<(), FfmpegContractError> {
        for (field, value, maximum) in [
            (
                "consumer_stall_timeout_millis",
                self.consumer_stall_timeout_millis,
                MAX_TIMEOUT_MILLIS,
            ),
            (
                "stderr_max_total_bytes",
                self.stderr_max_total_bytes,
                MAX_STDERR_TOTAL_BYTES,
            ),
            (
                "stderr_tail_bytes",
                self.stderr_tail_bytes,
                MAX_STDERR_TAIL_BYTES,
            ),
            (
                "pipe_allocation_bytes",
                self.pipe_allocation_bytes,
                MAX_PIPE_ALLOCATION_BYTES,
            ),
        ] {
            validate_nonzero_limit(field, value, maximum)?;
        }
        if self.stderr_tail_bytes > self.stderr_max_total_bytes {
            return Err(FfmpegContractError::InvalidLimit {
                field: "nested_byte_limits",
            });
        }
        Ok(())
    }
}

impl FfmpegResourceReferenceV1 {
    fn validate(&self, limits: FfmpegSupervisionLimitsV1) -> Result<(), FfmpegContractError> {
        if self.resource_contract_schema_id != RESOURCE_VECTOR_SCHEMA_ID
            || self.byte_credit_contract_schema_id != BYTE_CREDIT_SCHEMA_ID
            || self.pipe_stage != ByteCreditStage::FfmpegPipe
        {
            return Err(FfmpegContractError::InvalidResourceReference);
        }
        if self.claim.ffmpeg_processes != 1
            || self.claim.ffmpeg_cpu_threads == 0
            || self.claim.memory_bytes == 0
            || self.claim.disk_read_bytes_in_flight == 0
            || self.claim.disk_write_bytes_in_flight == 0
            || self.progress_pipe_bytes == 0
            || self.stderr_pipe_bytes == 0
        {
            return Err(FfmpegContractError::InvalidResourceClaim);
        }
        let pipe_bytes = self
            .progress_pipe_bytes
            .checked_add(self.stderr_pipe_bytes)
            .ok_or(FfmpegContractError::InvalidResourceClaim)?;
        if pipe_bytes > self.claim.memory_bytes || pipe_bytes > limits.pipe_allocation_bytes {
            return Err(FfmpegContractError::InvalidResourceClaim);
        }
        Ok(())
    }
}

impl FfmpegSupervisionRequestV1 {
    /// Validate the closed v1 request structurally and semantically.
    ///
    /// # Errors
    ///
    /// Returns a typed schema, identity, bound, plan, or resource error.
    pub fn validate(&self) -> Result<(), FfmpegContractError> {
        if self.schema_id != FFMPEG_SUPERVISION_SCHEMA_ID {
            return Err(FfmpegContractError::SchemaIdMismatch);
        }
        if self.version != FFMPEG_SUPERVISION_VERSION {
            return Err(FfmpegContractError::UnsupportedVersion(self.version));
        }
        validate_digest("operation_plan_sha256", &self.operation_plan_sha256)?;
        self.toolchain.ffmpeg.validate()?;
        self.toolchain.ffprobe.validate()?;
        if self.toolchain.ffmpeg.kind != FfmpegExecutableKindV1::Ffmpeg
            || self.toolchain.ffprobe.kind != FfmpegExecutableKindV1::Ffprobe
        {
            return Err(FfmpegContractError::ToolchainKindMismatch);
        }
        if self.toolchain.ffmpeg.host != self.toolchain.ffprobe.host {
            return Err(FfmpegContractError::ToolchainHostMismatch);
        }
        if self.toolchain.ffmpeg.absolute_path == self.toolchain.ffprobe.absolute_path
            || self.toolchain.ffmpeg.content_sha256 == self.toolchain.ffprobe.content_sha256
        {
            return Err(FfmpegContractError::ToolchainKindMismatch);
        }
        if self.environment.inherit_parent {
            return Err(FfmpegContractError::EnvironmentNotScrubbed);
        }
        validate_count(
            "environment.trusted_bindings",
            self.environment.trusted_bindings.len(),
            MAX_ENVIRONMENT_BINDINGS,
        )?;
        let expected_bindings: &[FfmpegEnvironmentBindingV1] =
            match self.toolchain.ffmpeg.host.operating_system {
                FfmpegHostOperatingSystemV1::Windows => &[
                    FfmpegEnvironmentBindingV1::HostSystemRoot,
                    FfmpegEnvironmentBindingV1::JobTemporaryDirectory,
                ],
                FfmpegHostOperatingSystemV1::Linux => &[
                    FfmpegEnvironmentBindingV1::JobTemporaryDirectory,
                    FfmpegEnvironmentBindingV1::LocaleC,
                ],
            };
        if self.environment.trusted_bindings != expected_bindings {
            return Err(FfmpegContractError::EnvironmentInventoryNotCanonical);
        }
        validate_absolute_path(
            "working_directory",
            &self.working_directory,
            self.toolchain.ffmpeg.host.operating_system,
        )?;
        if self.allowed_protocols != [FfmpegProtocolV1::File] {
            return Err(FfmpegContractError::InvalidProtocolAllowlist);
        }
        if self.allowed_input_mechanisms != [FfmpegInputMechanismV1::AuditedElementaryFile] {
            return Err(FfmpegContractError::InvalidInputMechanismAllowlist);
        }
        if self.io.progress_channel != FfmpegProgressChannelV1::StdoutPipeOne
            || self.io.diagnostic_channel != FfmpegDiagnosticChannelV1::StderrTail
            || !self.io.output_is_job_scoped_file
            || !self.io.stdin_disabled
        {
            return Err(FfmpegContractError::InvalidIoPolicy);
        }
        self.limits.validate()?;
        self.resources.validate(self.limits)?;
        match &self.operation {
            FfmpegOperationPlanV1::StreamCopy(plan) => {
                plan.validate(self.toolchain.ffmpeg.host.operating_system)?;
                for input in &plan.inputs {
                    require_capability(
                        &self.toolchain.ffmpeg,
                        &format!("demuxer:{}", input.demuxer.argument_name()),
                    )?;
                }
                require_capability(&self.toolchain.ffmpeg, "progress")?;
                require_capability(&self.toolchain.ffmpeg, "stream_copy")?;
                require_capability(
                    &self.toolchain.ffmpeg,
                    &format!("muxer:{}", plan.output_muxer),
                )?;
            }
        }
        require_capability(&self.toolchain.ffprobe, "json_output")?;
        require_capability(&self.toolchain.ffprobe, "stream_metadata")?;
        if self.toolchain.ffmpeg.host.operating_system == FfmpegHostOperatingSystemV1::Linux {
            require_capability(&self.toolchain.ffmpeg, "protocol:fd")?;
            require_capability(&self.toolchain.ffprobe, "protocol:fd")?;
        }
        let arguments = self.direct_arguments();
        validate_arguments(&arguments)
    }

    /// Validate independent digests immediately before spawn and return the
    /// only executable argument boundary exposed by this contract.
    ///
    /// # Errors
    ///
    /// Returns a typed contract or correlation error before any argument
    /// vector becomes available to an adapter.
    pub fn validate_for_invocation(
        &self,
    ) -> Result<FfmpegValidatedInvocationV1, FfmpegContractError> {
        self.validate()?;
        let operation_plan_sha256 = self.canonical_operation_plan_sha256()?;
        if self.operation_plan_sha256 != operation_plan_sha256 {
            return Err(FfmpegContractError::CorrelationMismatch {
                field: "operation_plan_sha256",
            });
        }
        let request_contract_sha256 = self.canonical_request_contract_sha256()?;
        let arguments = self.direct_arguments();
        let runtime_arguments =
            if self.toolchain.ffmpeg.host.operating_system == FfmpegHostOperatingSystemV1::Linux {
                match &self.operation {
                    FfmpegOperationPlanV1::StreamCopy(plan) => plan.linux_fd_arguments(),
                }
            } else {
                arguments.clone()
            };
        validate_arguments(&runtime_arguments)?;
        let canonical_sha256 = canonical_sha256(
            FFMPEG_BOUND_RUNTIME_INVOCATION_PROJECTION_ID,
            &BoundRuntimeProjection {
                request_contract_sha256: &request_contract_sha256,
                operation_plan_sha256: &operation_plan_sha256,
                arguments: &runtime_arguments,
            },
        )?;
        Ok(FfmpegValidatedInvocationV1 {
            bound_runtime: FfmpegBoundRuntimeInvocationV1 {
                projection_id: FFMPEG_BOUND_RUNTIME_INVOCATION_PROJECTION_ID,
                request_contract_sha256: request_contract_sha256.clone(),
                operation_plan_sha256: operation_plan_sha256.clone(),
                arguments: runtime_arguments,
                canonical_sha256,
            },
            request_contract_sha256,
            operation_plan_sha256,
            arguments,
        })
    }

    /// Compute the version-one canonical operation identity.
    ///
    /// # Errors
    ///
    /// Returns `CanonicalProjectionFailed` if the closed DTO cannot be encoded.
    pub fn canonical_operation_plan_sha256(&self) -> Result<String, FfmpegContractError> {
        canonical_sha256(FFMPEG_OPERATION_PROJECTION_ID, &self.operation)
    }

    /// Compute the version-one canonical request identity. Call this only after
    /// `operation_plan_sha256` has been set to the canonical operation identity.
    ///
    /// # Errors
    ///
    /// Returns `CanonicalProjectionFailed` if the closed DTO cannot be encoded.
    pub fn canonical_request_contract_sha256(&self) -> Result<String, FfmpegContractError> {
        canonical_sha256(FFMPEG_REQUEST_PROJECTION_ID, self)
    }

    fn direct_arguments(&self) -> Vec<String> {
        match &self.operation {
            FfmpegOperationPlanV1::StreamCopy(plan) => plan.direct_arguments(),
        }
    }
}

impl FfmpegOutputFactsV1 {
    fn validate(
        &self,
        limits: FfmpegSupervisionLimitsV1,
        operation: &FfmpegOperationPlanV1,
    ) -> Result<(), FfmpegContractError> {
        validate_digest("output_identity_sha256", &self.output_identity_sha256)?;
        if self.file_size_bytes == 0
            || self.file_size_bytes > limits.output_max_file_size_bytes
            || self.duration_millis > limits.output_max_duration_millis
            || self.format_names.is_empty()
            || self.format_names.len() > MAX_OUTPUT_FORMATS
            || self.streams.is_empty()
            || self.streams.len() > usize::try_from(limits.output_max_streams).unwrap_or(usize::MAX)
            || !strictly_sorted_unique(&self.format_names)
        {
            return Err(FfmpegContractError::InvalidOutputFacts);
        }
        for format_name in &self.format_names {
            validate_component("output.format_name", format_name, MAX_CAPABILITY_BYTES)?;
        }
        let FfmpegOperationPlanV1::StreamCopy(plan) = operation;
        for (expected_index, stream) in self.streams.iter().enumerate() {
            let expected_source = plan.stream_maps.get(expected_index);
            if stream.index != u32::try_from(expected_index).unwrap_or(u32::MAX)
                || expected_source != Some(&stream.selected_source)
                || expected_source.is_none_or(|source| source.stream_kind != stream.kind)
            {
                return Err(FfmpegContractError::InvalidOutputFacts);
            }
            validate_digest(
                "output.output_payload_sha256",
                &stream.output_payload_sha256,
            )?;
            if stream.selected_source.source_payload_sha256 != stream.output_payload_sha256 {
                return Err(FfmpegContractError::InvalidOutputFacts);
            }
            validate_component(
                "output.codec_name",
                &stream.codec_name,
                MAX_CAPABILITY_BYTES,
            )?;
            match stream.kind {
                FfmpegStreamKindV1::Video => {
                    if stream.width == Some(0)
                        || stream.height == Some(0)
                        || stream
                            .width
                            .is_some_and(|width| width > limits.output_max_width)
                        || stream
                            .height
                            .is_some_and(|height| height > limits.output_max_height)
                        || stream.channels.is_some()
                    {
                        return Err(FfmpegContractError::InvalidOutputFacts);
                    }
                }
                FfmpegStreamKindV1::Audio => {
                    if stream.channels == Some(0)
                        || stream
                            .channels
                            .is_some_and(|channels| channels > limits.output_max_channels)
                        || stream.width.is_some()
                        || stream.height.is_some()
                    {
                        return Err(FfmpegContractError::InvalidOutputFacts);
                    }
                }
                FfmpegStreamKindV1::Subtitle
                | FfmpegStreamKindV1::Data
                | FfmpegStreamKindV1::Attachment => {
                    if stream.width.is_some()
                        || stream.height.is_some()
                        || stream.channels.is_some()
                    {
                        return Err(FfmpegContractError::InvalidOutputFacts);
                    }
                }
            }
        }
        if self.format_names.binary_search(&plan.output_muxer).is_err() {
            return Err(FfmpegContractError::InvalidOutputFacts);
        }
        if self.streams.len() != plan.stream_maps.len() {
            return Err(FfmpegContractError::InvalidOutputFacts);
        }
        Ok(())
    }
}

impl FfmpegSupervisionReportV1 {
    /// Validate report shape, request correlation, ordering, reap, containment,
    /// output facts, and the no-success-before-validation invariant.
    ///
    /// # Errors
    ///
    /// Returns a typed error when declared evidence is inconsistent or unsafe
    /// to consume. This does not independently verify the declaration.
    pub fn validate_against(
        &self,
        request: &FfmpegSupervisionRequestV1,
    ) -> Result<(), FfmpegContractError> {
        let invocation = request.validate_for_invocation()?;
        self.validate_correlation(request, &invocation)?;
        validate_residuals(&self.residual_uncertainty)?;

        match self.terminal_outcome {
            FfmpegTerminalOutcomeV1::PreflightRejected { reason } => {
                if !matches!(
                    reason,
                    FfmpegFailureKindV1::ExecutableMissing
                        | FfmpegFailureKindV1::ExecutableIdentityChanged
                        | FfmpegFailureKindV1::CapabilityMismatch
                        | FfmpegFailureKindV1::InvalidPlan
                ) || !self.pre_spawn_shape_is_valid()
                {
                    return Err(FfmpegContractError::InvalidDirectChildEvidence);
                }
                return Ok(());
            }
            FfmpegTerminalOutcomeV1::Failed {
                reason: FfmpegFailureKindV1::SpawnFailed,
            } => {
                if !self.pre_spawn_shape_is_valid() {
                    return Err(FfmpegContractError::InvalidDirectChildEvidence);
                }
                return Ok(());
            }
            FfmpegTerminalOutcomeV1::Cancelled { forced: false }
                if self.pre_spawn_shape_is_valid() =>
            {
                return Ok(());
            }
            FfmpegTerminalOutcomeV1::Validated {}
            | FfmpegTerminalOutcomeV1::Failed { .. }
            | FfmpegTerminalOutcomeV1::Cancelled { .. } => {}
        }

        validate_lifecycle(
            &self.lifecycle,
            self.terminal_outcome,
            &self.direct_child,
            request.limits,
        )?;
        self.validate_direct_child()?;
        validate_containment(&self.containment, self.host, self.terminal_outcome)?;

        match (&self.terminal_outcome, &self.output_validation) {
            (
                FfmpegTerminalOutcomeV1::Validated {},
                FfmpegOutputValidationV1::Validated {
                    ffprobe_content_sha256,
                    facts,
                },
            ) => {
                if ffprobe_content_sha256 != &request.toolchain.ffprobe.content_sha256 {
                    return Err(FfmpegContractError::CorrelationMismatch {
                        field: "output_validation.ffprobe_content_sha256",
                    });
                }
                validate_digest(
                    "output_validation.ffprobe_content_sha256",
                    ffprobe_content_sha256,
                )?;
                facts.validate(request.limits, &request.operation)?;
            }
            (FfmpegTerminalOutcomeV1::Validated {}, _)
            | (_, FfmpegOutputValidationV1::Validated { .. }) => {
                return Err(FfmpegContractError::SuccessBeforeReapAndValidation);
            }
            (
                _,
                FfmpegOutputValidationV1::Rejected { .. } | FfmpegOutputValidationV1::NotRun {},
            ) => {}
        }
        Ok(())
    }

    fn validate_correlation(
        &self,
        request: &FfmpegSupervisionRequestV1,
        invocation: &FfmpegValidatedInvocationV1,
    ) -> Result<(), FfmpegContractError> {
        if self.schema_id != FFMPEG_SUPERVISION_REPORT_SCHEMA_ID {
            return Err(FfmpegContractError::SchemaIdMismatch);
        }
        if self.version != FFMPEG_SUPERVISION_VERSION {
            return Err(FfmpegContractError::UnsupportedVersion(self.version));
        }
        let expected_request_digest = invocation.request_contract_sha256();
        let expected_operation_digest = invocation.operation_plan_sha256();
        for (matches, field) in [
            (self.request_id == request.request_id, "request_id"),
            (self.job_id == request.job_id, "job_id"),
            (
                request.operation_plan_sha256 == expected_operation_digest
                    && self.operation_plan_sha256 == expected_operation_digest,
                "operation_plan_sha256",
            ),
            (
                self.request_contract_sha256 == expected_request_digest,
                "request_contract_sha256",
            ),
            (
                self.ffmpeg_content_sha256 == request.toolchain.ffmpeg.content_sha256,
                "ffmpeg_content_sha256",
            ),
            (
                self.ffprobe_content_sha256 == request.toolchain.ffprobe.content_sha256,
                "ffprobe_content_sha256",
            ),
            (self.host == request.toolchain.ffmpeg.host, "host"),
        ] {
            if !matches {
                return Err(FfmpegContractError::CorrelationMismatch { field });
            }
        }
        validate_commit("source_commit", &self.source_commit)?;
        validate_digest("request_contract_sha256", &self.request_contract_sha256)?;
        validate_digest("report.operation_plan_sha256", &self.operation_plan_sha256)?;
        validate_digest("report.ffmpeg_content_sha256", &self.ffmpeg_content_sha256)?;
        validate_digest(
            "report.ffprobe_content_sha256",
            &self.ffprobe_content_sha256,
        )?;
        Ok(())
    }

    fn validate_direct_child(&self) -> Result<(), FfmpegContractError> {
        match &self.direct_child {
            FfmpegDirectChildReapV1::NotSpawned {} => {
                return Err(FfmpegContractError::InvalidDirectChildEvidence);
            }
            FfmpegDirectChildReapV1::ReapUnproven { wait_error_sha256 } => {
                validate_digest("direct_child.wait_error_sha256", wait_error_sha256)?;
                if !matches!(
                    self.terminal_outcome,
                    FfmpegTerminalOutcomeV1::Failed {
                        reason: FfmpegFailureKindV1::ReapFailed
                            | FfmpegFailureKindV1::ContainmentUnproven
                    }
                ) {
                    return Err(FfmpegContractError::InvalidDirectChildEvidence);
                }
            }
            FfmpegDirectChildReapV1::Reaped {
                exit_code,
                terminated_by_signal_or_exception,
                wait_receipt_sha256,
            } => {
                validate_digest("direct_child.wait_receipt_sha256", wait_receipt_sha256)?;
                match self.terminal_outcome {
                    FfmpegTerminalOutcomeV1::Validated {}
                        if *exit_code == Some(0) && !*terminated_by_signal_or_exception => {}
                    FfmpegTerminalOutcomeV1::Failed {
                        reason: FfmpegFailureKindV1::ExitFailed,
                    } if *exit_code != Some(0) || *terminated_by_signal_or_exception => {}
                    FfmpegTerminalOutcomeV1::Validated {}
                    | FfmpegTerminalOutcomeV1::Failed {
                        reason: FfmpegFailureKindV1::ExitFailed,
                    } => return Err(FfmpegContractError::InvalidDirectChildEvidence),
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn pre_spawn_shape_is_valid(&self) -> bool {
        self.lifecycle.is_empty()
            && matches!(self.direct_child, FfmpegDirectChildReapV1::NotSpawned {})
            && matches!(
                self.containment,
                FfmpegContainmentEvidenceV1::NotEstablished {}
            )
            && matches!(self.output_validation, FfmpegOutputValidationV1::NotRun {})
    }
}

fn validate_capabilities(capabilities: &[String]) -> Result<(), FfmpegContractError> {
    validate_count("capabilities", capabilities.len(), MAX_CAPABILITIES)?;
    if capabilities.is_empty() || !strictly_sorted_unique(capabilities) {
        return Err(FfmpegContractError::CapabilityInventoryNotCanonical);
    }
    for capability in capabilities {
        validate_component("capability", capability, MAX_CAPABILITY_BYTES)?;
    }
    Ok(())
}

fn require_capability(
    executable: &FfmpegExecutableIdentityV1,
    capability: &str,
) -> Result<(), FfmpegContractError> {
    if executable
        .capability_binding
        .capabilities
        .binary_search_by(|candidate| candidate.as_str().cmp(capability))
        .is_err()
    {
        return Err(FfmpegContractError::MissingCapability {
            capability: capability.to_owned(),
        });
    }
    Ok(())
}

fn canonical_sha256<T: Serialize>(
    projection_id: &'static str,
    value: &T,
) -> Result<String, FfmpegContractError> {
    let encoded =
        serde_json::to_vec(value).map_err(|_| FfmpegContractError::CanonicalProjectionFailed)?;
    let mut hasher = Sha256::new();
    hasher.update(projection_id.as_bytes());
    hasher.update([0]);
    hasher.update(encoded);
    let digest = hasher.finalize();
    let mut encoded_digest = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut encoded_digest, "{byte:02x}")
            .map_err(|_| FfmpegContractError::CanonicalProjectionFailed)?;
    }
    Ok(encoded_digest)
}

fn validate_arguments(arguments: &[String]) -> Result<(), FfmpegContractError> {
    if arguments.is_empty() || arguments.len() > MAX_ARGUMENTS {
        return Err(FfmpegContractError::ArgumentLimitExceeded);
    }
    let mut total = 0_usize;
    for argument in arguments {
        validate_text("direct_argument", argument, MAX_ARGUMENT_BYTES)?;
        total = total
            .checked_add(argument.len())
            .and_then(|value| value.checked_add(1))
            .ok_or(FfmpegContractError::ArgumentLimitExceeded)?;
    }
    if total > MAX_ARGUMENT_VECTOR_BYTES {
        return Err(FfmpegContractError::ArgumentLimitExceeded);
    }
    Ok(())
}

fn validate_lifecycle(
    lifecycle: &[FfmpegLifecycleObservationV1],
    outcome: FfmpegTerminalOutcomeV1,
    direct_child: &FfmpegDirectChildReapV1,
    limits: FfmpegSupervisionLimitsV1,
) -> Result<(), FfmpegContractError> {
    if lifecycle.is_empty() || lifecycle.len() > MAX_REPORT_EVENTS {
        return Err(FfmpegContractError::InvalidLifecycle);
    }
    let mut prior_state = None;
    let mut prior_time = 0;
    for (index, observation) in lifecycle.iter().enumerate() {
        if observation.sequence != u64::try_from(index + 1).unwrap_or(u64::MAX)
            || (index > 0 && observation.monotonic_millis < prior_time)
            || !valid_lifecycle_edge(prior_state, observation.state)
            || !lifecycle_deadline_is_valid(
                prior_state,
                prior_time,
                observation.state,
                observation.monotonic_millis,
                limits,
            )
        {
            return Err(FfmpegContractError::InvalidLifecycle);
        }
        prior_state = Some(observation.state);
        prior_time = observation.monotonic_millis;
    }
    let expected_last = match (outcome, direct_child) {
        (FfmpegTerminalOutcomeV1::Validated {}, _) => FfmpegLifecycleStateV1::Validated,
        (_, FfmpegDirectChildReapV1::ReapUnproven { .. }) => FfmpegLifecycleStateV1::ReapUnproven,
        _ => FfmpegLifecycleStateV1::Reaped,
    };
    if prior_state != Some(expected_last) {
        return Err(FfmpegContractError::SuccessBeforeReapAndValidation);
    }
    let saw_graceful = lifecycle
        .iter()
        .any(|row| row.state == FfmpegLifecycleStateV1::GracefulStopRequested);
    let saw_forced = lifecycle
        .iter()
        .any(|row| row.state == FfmpegLifecycleStateV1::ForcedKillRequested);
    match outcome {
        FfmpegTerminalOutcomeV1::Validated {} if saw_graceful || saw_forced => {
            return Err(FfmpegContractError::InvalidLifecycle);
        }
        FfmpegTerminalOutcomeV1::Cancelled { forced } if !saw_graceful || forced != saw_forced => {
            return Err(FfmpegContractError::InvalidLifecycle);
        }
        _ => {}
    }
    Ok(())
}

fn lifecycle_deadline_is_valid(
    prior: Option<FfmpegLifecycleStateV1>,
    prior_time: u64,
    next: FfmpegLifecycleStateV1,
    next_time: u64,
    limits: FfmpegSupervisionLimitsV1,
) -> bool {
    let Some(prior) = prior else {
        return next_time <= limits.startup_timeout_millis;
    };
    let elapsed = next_time.saturating_sub(prior_time);
    let maximum = match (prior, next) {
        (FfmpegLifecycleStateV1::Spawned, FfmpegLifecycleStateV1::Running) => {
            limits.startup_timeout_millis
        }
        (
            FfmpegLifecycleStateV1::Running,
            FfmpegLifecycleStateV1::GracefulStopRequested
            | FfmpegLifecycleStateV1::ForcedKillRequested
            | FfmpegLifecycleStateV1::Reaped
            | FfmpegLifecycleStateV1::ReapUnproven,
        ) => limits
            .execution_timeout_millis
            .saturating_add(limits.reap_timeout_millis),
        (
            FfmpegLifecycleStateV1::GracefulStopRequested,
            FfmpegLifecycleStateV1::ForcedKillRequested,
        ) => limits.graceful_stop_timeout_millis,
        (
            FfmpegLifecycleStateV1::GracefulStopRequested
            | FfmpegLifecycleStateV1::ForcedKillRequested,
            FfmpegLifecycleStateV1::Reaped | FfmpegLifecycleStateV1::ReapUnproven,
        ) => limits
            .forced_kill_timeout_millis
            .saturating_add(limits.reap_timeout_millis),
        (FfmpegLifecycleStateV1::Reaped, FfmpegLifecycleStateV1::Validated) => {
            limits.ffprobe_timeout_millis
        }
        _ => return false,
    };
    elapsed <= maximum
}

fn valid_lifecycle_edge(
    prior: Option<FfmpegLifecycleStateV1>,
    next: FfmpegLifecycleStateV1,
) -> bool {
    matches!(
        (prior, next),
        (None, FfmpegLifecycleStateV1::Spawned)
            | (
                Some(FfmpegLifecycleStateV1::Spawned),
                FfmpegLifecycleStateV1::Running
            )
            | (
                Some(FfmpegLifecycleStateV1::Running),
                FfmpegLifecycleStateV1::GracefulStopRequested
                    | FfmpegLifecycleStateV1::ForcedKillRequested
                    | FfmpegLifecycleStateV1::Reaped
                    | FfmpegLifecycleStateV1::ReapUnproven
            )
            | (
                Some(FfmpegLifecycleStateV1::GracefulStopRequested),
                FfmpegLifecycleStateV1::ForcedKillRequested
                    | FfmpegLifecycleStateV1::Reaped
                    | FfmpegLifecycleStateV1::ReapUnproven
            )
            | (
                Some(FfmpegLifecycleStateV1::ForcedKillRequested),
                FfmpegLifecycleStateV1::Reaped | FfmpegLifecycleStateV1::ReapUnproven
            )
            | (
                Some(FfmpegLifecycleStateV1::Reaped),
                FfmpegLifecycleStateV1::Validated
            )
    )
}

fn validate_containment(
    containment: &FfmpegContainmentEvidenceV1,
    host: FfmpegHostIdentityV1,
    outcome: FfmpegTerminalOutcomeV1,
) -> Result<(), FfmpegContractError> {
    let may_be_unproven = matches!(
        outcome,
        FfmpegTerminalOutcomeV1::Failed {
            reason: FfmpegFailureKindV1::ContainmentUnproven | FfmpegFailureKindV1::ReapFailed
        }
    );
    match (host.operating_system, containment) {
        (
            FfmpegHostOperatingSystemV1::Windows,
            FfmpegContainmentEvidenceV1::WindowsJob {
                attached_before_execution,
                kill_on_job_close,
                active_process_query,
            },
        ) => {
            if !attached_before_execution
                || !kill_on_job_close
                || (!may_be_unproven
                    && !matches!(active_process_query, WindowsActiveProcessQueryV1::Zero {}))
            {
                return Err(FfmpegContractError::InvalidContainmentEvidence);
            }
        }
        (
            FfmpegHostOperatingSystemV1::Linux,
            FfmpegContainmentEvidenceV1::UnixProcessGroup {
                declared_nonescaping_members_remaining,
                setsid_escape_capable,
            },
        ) => {
            if !setsid_escape_capable
                || (!may_be_unproven && *declared_nonescaping_members_remaining != 0)
            {
                return Err(FfmpegContractError::InvalidContainmentEvidence);
            }
        }
        _ => return Err(FfmpegContractError::InvalidContainmentEvidence),
    }
    Ok(())
}

fn validate_residuals(residuals: &[String]) -> Result<(), FfmpegContractError> {
    if residuals.is_empty() {
        return Err(FfmpegContractError::ResidualUncertaintyRequired);
    }
    validate_count("residual_uncertainty", residuals.len(), MAX_RESIDUALS)?;
    if !strictly_sorted_unique(residuals) {
        return Err(FfmpegContractError::InvalidField {
            field: "residual_uncertainty",
        });
    }
    for residual in residuals {
        validate_text("residual_uncertainty", residual, MAX_RESIDUAL_BYTES)?;
    }
    Ok(())
}

fn validate_count(
    field: &'static str,
    actual: usize,
    maximum: usize,
) -> Result<(), FfmpegContractError> {
    if actual > maximum {
        return Err(FfmpegContractError::CountLimitExceeded {
            field,
            actual,
            maximum,
        });
    }
    Ok(())
}

fn validate_nonzero_limit(
    field: &'static str,
    value: u64,
    maximum: u64,
) -> Result<(), FfmpegContractError> {
    if value == 0 || value > maximum {
        return Err(FfmpegContractError::InvalidLimit { field });
    }
    Ok(())
}

fn validate_text(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), FfmpegContractError> {
    if value.is_empty() {
        return Err(FfmpegContractError::EmptyField { field });
    }
    if value.len() > maximum {
        return Err(FfmpegContractError::FieldTooLong {
            field,
            actual: value.len(),
            maximum,
        });
    }
    if value
        .bytes()
        .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return Err(FfmpegContractError::InvalidField { field });
    }
    Ok(())
}

fn validate_component(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), FfmpegContractError> {
    validate_text(field, value, maximum)?;
    if value.bytes().any(|byte| {
        !byte.is_ascii_lowercase()
            && !byte.is_ascii_digit()
            && !matches!(byte, b'_' | b'-' | b'.' | b':')
    }) {
        return Err(FfmpegContractError::InvalidField { field });
    }
    Ok(())
}

fn validate_digest(field: &'static str, value: &str) -> Result<(), FfmpegContractError> {
    if value.len() != 64
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
    {
        return Err(FfmpegContractError::InvalidDigest { field });
    }
    Ok(())
}

fn validate_commit(field: &'static str, value: &str) -> Result<(), FfmpegContractError> {
    if !matches!(value.len(), 40 | 64)
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
    {
        return Err(FfmpegContractError::InvalidDigest { field });
    }
    Ok(())
}

fn validate_absolute_path(
    field: &'static str,
    value: &str,
    operating_system: FfmpegHostOperatingSystemV1,
) -> Result<(), FfmpegContractError> {
    validate_text(field, value, MAX_PATH_BYTES)?;
    let bytes = value.as_bytes();
    let absolute = match operating_system {
        FfmpegHostOperatingSystemV1::Linux => value.starts_with('/'),
        FfmpegHostOperatingSystemV1::Windows => {
            (bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && matches!(bytes[2], b'/' | b'\\'))
                || value.starts_with("\\\\")
        }
    };
    if !absolute
        || contains_dot_segment(value)
        || value.ends_with(['/', '\\'])
        || (operating_system == FfmpegHostOperatingSystemV1::Windows
            && windows_path_has_unsafe_component(value, true))
    {
        return Err(FfmpegContractError::InvalidAbsolutePath { field });
    }
    Ok(())
}

fn validate_job_path(
    field: &'static str,
    value: &str,
    host: FfmpegHostOperatingSystemV1,
) -> Result<(), FfmpegContractError> {
    validate_text(field, value, MAX_PATH_BYTES)?;
    let bytes = value.as_bytes();
    let drive_absolute = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if value.starts_with('/')
        || value.starts_with('\\')
        || value.starts_with('-')
        || drive_absolute
        || value.contains(':')
        || contains_dot_segment(value)
        || value.ends_with(['/', '\\'])
        || value.contains("//")
        || value.contains("\\\\")
    {
        return Err(FfmpegContractError::InvalidJobPath { field });
    }
    if host == FfmpegHostOperatingSystemV1::Windows
        && windows_path_has_unsafe_component(value, false)
    {
        return Err(FfmpegContractError::InvalidJobPath { field });
    }
    Ok(())
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

fn paths_alias(left: &str, right: &str, host: FfmpegHostOperatingSystemV1) -> bool {
    match host {
        FfmpegHostOperatingSystemV1::Windows => left
            .replace('/', "\\")
            .eq_ignore_ascii_case(&right.replace('/', "\\")),
        FfmpegHostOperatingSystemV1::Linux => left == right,
    }
}

fn contains_dot_segment(value: &str) -> bool {
    value
        .split(['/', '\\'])
        .any(|segment| matches!(segment, "." | ".."))
}

fn strictly_sorted_unique<T: Ord>(values: &[T]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}

#[cfg(test)]
#[path = "../tests/support/ffmpeg_fixture.rs"]
mod test_fixture;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn fixture() -> Value {
        test_fixture::fixture()
    }

    fn request_and_report() -> (FfmpegSupervisionRequestV1, FfmpegSupervisionReportV1) {
        let value = fixture();
        let request = serde_json::from_value(value["request"].clone())
            .expect("canonical request must decode");
        let report =
            serde_json::from_value(value["report"].clone()).expect("canonical report must decode");
        (request, report)
    }

    #[test]
    fn canonical_fixture_decodes_and_validates() {
        let (request, report) = request_and_report();
        assert_eq!(request.validate(), Ok(()));
        let invocation = request
            .validate_for_invocation()
            .expect("canonical request must bind before invocation");
        assert_eq!(report.validate_against(&request), Ok(()));
        assert_eq!(
            invocation.arguments(),
            [
                "-hide_banner",
                "-nostdin",
                "-n",
                "-protocol_whitelist",
                "file,pipe",
                "-progress",
                "pipe:1",
                "-protocol_whitelist",
                "file",
                "-f",
                "aac",
                "-i",
                "input/audio.aac",
                "-protocol_whitelist",
                "file",
                "-f",
                "h264",
                "-i",
                "input/video.h264",
                "-map",
                "0:a:0",
                "-map",
                "1:v:0",
                "-protocol_whitelist",
                "file",
                "-c",
                "copy",
                "-f",
                "matroska",
                "output/merged.mkv",
            ]
        );
    }

    #[test]
    fn linux_bound_runtime_projection_preserves_logical_v1_and_binds_exact_fds() {
        let (mut request, _) = request_and_report();
        request.toolchain.ffmpeg.host.operating_system = FfmpegHostOperatingSystemV1::Linux;
        request.toolchain.ffprobe.host.operating_system = FfmpegHostOperatingSystemV1::Linux;
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
        request.working_directory = "/var/lib/ferric/job_ffmpeg_stream_copy_1".to_owned();
        let invocation = request.validate_for_invocation().expect("Linux binding");
        assert!(invocation.arguments().iter().any(|value| value == "-n"));
        assert!(
            invocation
                .arguments()
                .iter()
                .any(|value| value == "input/audio.aac")
        );
        let runtime = invocation.bound_runtime();
        assert_eq!(
            runtime.projection_id(),
            FFMPEG_BOUND_RUNTIME_INVOCATION_PROJECTION_ID
        );
        assert!(runtime.arguments().iter().any(|value| value == "-y"));
        assert!(!runtime.arguments().iter().any(|value| value == "-n"));
        assert_eq!(
            runtime
                .arguments()
                .iter()
                .filter(|value| *value == "fd:")
                .count(),
            3
        );
        for descriptor in [64_u16, 65, FFMPEG_LINUX_FD_OUTPUT_V1] {
            assert!(
                runtime
                    .arguments()
                    .iter()
                    .any(|value| value == &descriptor.to_string())
            );
        }
        assert_eq!(runtime.canonical_sha256().len(), 64);
    }

    #[test]
    fn canonical_fixture_declares_real_projection_hashes() {
        let (request, report) = request_and_report();
        let operation_plan_sha256 = request
            .canonical_operation_plan_sha256()
            .expect("canonical operation projection must serialize");
        let request_contract_sha256 = request
            .canonical_request_contract_sha256()
            .expect("canonical request projection must serialize");

        assert_eq!(request.operation_plan_sha256, operation_plan_sha256);
        assert_eq!(report.operation_plan_sha256, operation_plan_sha256);
        assert_eq!(report.request_contract_sha256, request_contract_sha256);
    }

    #[test]
    fn wire_rejects_missing_unknown_reordered_forged_and_cross_version_fields() {
        let value = fixture();

        let mut missing = value["request"].clone();
        missing
            .as_object_mut()
            .expect("request object")
            .remove("limits");
        assert!(serde_json::from_value::<FfmpegSupervisionRequestV1>(missing).is_err());

        let mut unknown = value["request"].clone();
        unknown["toolchain"]["ffmpeg"]["capability_binding"]["undeclared"] = json!(true);
        assert!(serde_json::from_value::<FfmpegSupervisionRequestV1>(unknown).is_err());

        let mut reordered = value["report"].clone();
        reordered["lifecycle"]
            .as_array_mut()
            .expect("lifecycle array")
            .swap(1, 2);
        let reordered: FfmpegSupervisionReportV1 =
            serde_json::from_value(reordered).expect("reordered report remains typed JSON");
        let request: FfmpegSupervisionRequestV1 =
            serde_json::from_value(value["request"].clone()).expect("request must decode");
        assert_eq!(
            reordered.validate_against(&request),
            Err(FfmpegContractError::InvalidLifecycle)
        );

        let mut forged = value["request"].clone();
        forged["toolchain"]["ffmpeg"]["capability_binding"]["executable_content_sha256"] =
            json!("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff");
        let forged: FfmpegSupervisionRequestV1 =
            serde_json::from_value(forged).expect("forged binding remains typed JSON");
        assert_eq!(
            forged.validate(),
            Err(FfmpegContractError::CapabilityBindingMismatch)
        );

        let mut cross_version = value["request"].clone();
        cross_version["version"] = json!({"major": 2, "minor": 0});
        let cross_version: FfmpegSupervisionRequestV1 = serde_json::from_value(cross_version)
            .expect("cross-version request remains typed JSON");
        assert!(matches!(
            cross_version.validate(),
            Err(FfmpegContractError::UnsupportedVersion(SchemaVersion {
                major: 2,
                minor: 0
            }))
        ));
    }

    #[test]
    fn success_requires_reap_containment_and_ffprobe_validation() {
        let (request, report) = request_and_report();

        let mut no_reap = report.clone();
        no_reap.lifecycle.remove(2);
        assert!(no_reap.validate_against(&request).is_err());

        let mut active_child = report.clone();
        active_child.containment = FfmpegContainmentEvidenceV1::WindowsJob {
            attached_before_execution: true,
            kill_on_job_close: true,
            active_process_query: WindowsActiveProcessQueryV1::Nonzero { count: 1 },
        };
        assert_eq!(
            active_child.validate_against(&request),
            Err(FfmpegContractError::InvalidContainmentEvidence)
        );

        let mut unprobed = report;
        unprobed.output_validation = FfmpegOutputValidationV1::NotRun {};
        assert_eq!(
            unprobed.validate_against(&request),
            Err(FfmpegContractError::SuccessBeforeReapAndValidation)
        );
    }

    #[test]
    fn operation_and_resource_boundaries_fail_closed() {
        let (mut request, _) = request_and_report();
        let FfmpegOperationPlanV1::StreamCopy(plan) = &mut request.operation;
        plan.inputs[0].path = "../escape.aac".to_owned();
        assert!(matches!(
            request.validate(),
            Err(FfmpegContractError::InvalidJobPath { .. })
        ));

        let (mut request, _) = request_and_report();
        request.environment.inherit_parent = true;
        assert_eq!(
            request.validate(),
            Err(FfmpegContractError::EnvironmentNotScrubbed)
        );

        let (mut request, _) = request_and_report();
        request.resources.claim.ffmpeg_processes = 0;
        assert_eq!(
            request.validate(),
            Err(FfmpegContractError::InvalidResourceClaim)
        );

        let (mut request, _) = request_and_report();
        request.limits.stderr_max_total_bytes = 32_768;
        request.limits.stderr_tail_bytes = 65_536;
        assert_eq!(
            request.validate(),
            Err(FfmpegContractError::InvalidLimit {
                field: "nested_byte_limits"
            })
        );
    }

    #[test]
    fn semantic_counterexamples_fail_closed_at_the_public_boundary() {
        let (request, report) = request_and_report();
        let mut nonzero = report.clone();
        let FfmpegDirectChildReapV1::Reaped { exit_code, .. } = &mut nonzero.direct_child else {
            panic!("fixture must contain a reaped child");
        };
        *exit_code = Some(7);
        assert_eq!(
            nonzero.validate_against(&request),
            Err(FfmpegContractError::InvalidDirectChildEvidence)
        );

        let mut forced_success = report.clone();
        forced_success.lifecycle.insert(
            2,
            FfmpegLifecycleObservationV1 {
                sequence: 3,
                state: FfmpegLifecycleStateV1::ForcedKillRequested,
                monotonic_millis: 50,
            },
        );
        for (index, row) in forced_success.lifecycle.iter_mut().enumerate() {
            row.sequence = u64::try_from(index + 1).expect("bounded fixture index");
        }
        assert_eq!(
            forced_success.validate_against(&request),
            Err(FfmpegContractError::InvalidLifecycle)
        );

        let mut cancelled = report.clone();
        cancelled.terminal_outcome = FfmpegTerminalOutcomeV1::Cancelled { forced: true };
        cancelled.lifecycle = vec![
            FfmpegLifecycleObservationV1 {
                sequence: 1,
                state: FfmpegLifecycleStateV1::Spawned,
                monotonic_millis: 1,
            },
            FfmpegLifecycleObservationV1 {
                sequence: 2,
                state: FfmpegLifecycleStateV1::Running,
                monotonic_millis: 2,
            },
            FfmpegLifecycleObservationV1 {
                sequence: 3,
                state: FfmpegLifecycleStateV1::GracefulStopRequested,
                monotonic_millis: 3,
            },
            FfmpegLifecycleObservationV1 {
                sequence: 4,
                state: FfmpegLifecycleStateV1::Reaped,
                monotonic_millis: 4,
            },
        ];
        cancelled.output_validation = FfmpegOutputValidationV1::NotRun {};
        assert_eq!(
            cancelled.validate_against(&request),
            Err(FfmpegContractError::InvalidLifecycle)
        );
    }

    #[test]
    fn digest_output_and_deadline_counterexamples_fail_closed() {
        let (request, report) = request_and_report();
        let invocation = request
            .validate_for_invocation()
            .expect("original invocation");
        let mut changed_request = request.clone();
        let FfmpegOperationPlanV1::StreamCopy(plan) = &mut changed_request.operation;
        plan.output_path = "output/other.mkv".to_owned();
        assert!(matches!(
            changed_request.validate_for_invocation(),
            Err(FfmpegContractError::CorrelationMismatch { .. })
        ));
        assert!(matches!(
            report.validate_against(&changed_request),
            Err(FfmpegContractError::CorrelationMismatch { .. })
        ));
        changed_request.operation_plan_sha256 = changed_request
            .canonical_operation_plan_sha256()
            .expect("changed operation projection");
        assert!(changed_request.validate_for_invocation().is_ok());
        assert_eq!(
            invocation.validate_against(&changed_request),
            Err(FfmpegContractError::CorrelationMismatch {
                field: "validated_invocation"
            })
        );

        let mut arbitrary_request_digest = report.clone();
        arbitrary_request_digest.request_contract_sha256 = "9".repeat(64);
        assert_eq!(
            arbitrary_request_digest.validate_against(&request),
            Err(FfmpegContractError::CorrelationMismatch {
                field: "request_contract_sha256"
            })
        );

        let mut wrong_output = report.clone();
        let FfmpegOutputValidationV1::Validated { facts, .. } = &mut wrong_output.output_validation
        else {
            panic!("fixture must contain validated output facts");
        };
        facts.format_names = vec!["webm".to_owned()];
        facts.streams.truncate(1);
        facts.streams[0].kind = FfmpegStreamKindV1::Subtitle;
        facts.streams[0].width = None;
        facts.streams[0].height = None;
        assert_eq!(
            wrong_output.validate_against(&request),
            Err(FfmpegContractError::InvalidOutputFacts)
        );

        let mut wrong_source_request = request.clone();
        let FfmpegOperationPlanV1::StreamCopy(plan) = &mut wrong_source_request.operation;
        plan.stream_maps[0].stream_index = 17;
        wrong_source_request.operation_plan_sha256 = wrong_source_request
            .canonical_operation_plan_sha256()
            .expect("changed operation projection must serialize");
        let wrong_source_request_sha256 = wrong_source_request
            .canonical_request_contract_sha256()
            .expect("changed request projection must serialize");
        assert!(wrong_source_request.validate_for_invocation().is_ok());

        let mut wrong_source_report = report.clone();
        wrong_source_report.operation_plan_sha256 =
            wrong_source_request.operation_plan_sha256.clone();
        wrong_source_report.request_contract_sha256 = wrong_source_request_sha256;
        assert_eq!(
            wrong_source_report.validate_against(&wrong_source_request),
            Err(FfmpegContractError::InvalidOutputFacts)
        );

        let mut contradictory_duplicate = request.clone();
        let FfmpegOperationPlanV1::StreamCopy(plan) = &mut contradictory_duplicate.operation;
        let mut duplicate = plan.stream_maps[0].clone();
        duplicate.source_payload_sha256 = "7".repeat(64);
        plan.stream_maps.insert(1, duplicate);
        contradictory_duplicate.operation_plan_sha256 = contradictory_duplicate
            .canonical_operation_plan_sha256()
            .expect("duplicate operation projection must serialize");
        assert_eq!(
            contradictory_duplicate.validate_for_invocation(),
            Err(FfmpegContractError::InvalidStreamMap)
        );

        let mut oversized_output = report.clone();
        let FfmpegOutputValidationV1::Validated { facts, .. } =
            &mut oversized_output.output_validation
        else {
            panic!("fixture must contain validated output facts");
        };
        facts.file_size_bytes = u64::MAX;
        facts.streams[0].width = Some(u32::MAX);
        assert_eq!(
            oversized_output.validate_against(&request),
            Err(FfmpegContractError::InvalidOutputFacts)
        );

        let mut late = report;
        late.lifecycle[2].monotonic_millis = request
            .limits
            .execution_timeout_millis
            .saturating_add(request.limits.reap_timeout_millis)
            .saturating_add(3);
        late.lifecycle[3].monotonic_millis = late.lifecycle[2].monotonic_millis;
        assert_eq!(
            late.validate_against(&request),
            Err(FfmpegContractError::InvalidLifecycle)
        );
    }

    #[test]
    fn pre_spawn_and_invocation_boundaries_are_exact() {
        let (request, mut report) = request_and_report();
        report.terminal_outcome = FfmpegTerminalOutcomeV1::Failed {
            reason: FfmpegFailureKindV1::SpawnFailed,
        };
        report.lifecycle.clear();
        report.direct_child = FfmpegDirectChildReapV1::NotSpawned {};
        report.containment = FfmpegContainmentEvidenceV1::NotEstablished {};
        report.output_validation = FfmpegOutputValidationV1::NotRun {};
        assert_eq!(report.validate_against(&request), Ok(()));

        report.terminal_outcome = FfmpegTerminalOutcomeV1::Cancelled { forced: false };
        assert_eq!(report.validate_against(&request), Ok(()));
        report.terminal_outcome = FfmpegTerminalOutcomeV1::Cancelled { forced: true };
        assert_eq!(
            report.validate_against(&request),
            Err(FfmpegContractError::InvalidLifecycle)
        );

        let mut environment_injection = request.clone();
        environment_injection.environment.trusted_bindings =
            vec![FfmpegEnvironmentBindingV1::LocaleC];
        assert!(environment_injection.validate().is_err());

        let mut hostile_environment_value = fixture()["request"].clone();
        hostile_environment_value["environment"]["trusted_bindings"][0] =
            json!({"host_system_root": "C:\\attacker-controlled"});
        assert!(
            serde_json::from_value::<FfmpegSupervisionRequestV1>(hostile_environment_value)
                .is_err()
        );

        let mut manifest_shaped_input = request.clone();
        let FfmpegOperationPlanV1::StreamCopy(plan) = &mut manifest_shaped_input.operation;
        plan.inputs[0].path = "input/list.m3u8".to_owned();
        manifest_shaped_input.operation_plan_sha256 = manifest_shaped_input
            .canonical_operation_plan_sha256()
            .expect("manifest-shaped operation projection must serialize");
        let invocation = manifest_shaped_input
            .validate_for_invocation()
            .expect("path spelling cannot replace the audited demuxer");
        assert!(
            invocation
                .arguments()
                .windows(4)
                .any(|window| { window == ["-f", "aac", "-i", "input/list.m3u8"] })
        );
        assert!(
            !invocation
                .arguments()
                .iter()
                .any(|argument| argument == "hls")
        );

        assert_windows_path_boundaries(&request);
    }

    fn assert_windows_path_boundaries(request: &FfmpegSupervisionRequestV1) {
        let mut option_path = request.clone();
        let FfmpegOperationPlanV1::StreamCopy(plan) = &mut option_path.operation;
        plan.output_path = "-version".to_owned();
        assert!(matches!(
            option_path.validate(),
            Err(FfmpegContractError::InvalidJobPath { .. })
        ));

        for reserved_path in [
            "output/NUL.mkv",
            "output/COM1.log",
            "output/COM¹.log",
            "output/LPT²",
            "output/CONIN$",
            "output/CONOUT$",
            "output/name./file.mkv",
            "output/bad?.mkv",
            "output/bad*.mkv",
            "output/bad|name.mkv",
            "output/<bad>.mkv",
            "output/\"bad\".mkv",
        ] {
            let mut reserved = request.clone();
            let FfmpegOperationPlanV1::StreamCopy(plan) = &mut reserved.operation;
            plan.output_path = reserved_path.to_owned();
            assert!(
                matches!(
                    reserved.validate(),
                    Err(FfmpegContractError::InvalidJobPath { .. })
                ),
                "Windows reserved path must fail: {reserved_path}"
            );
        }

        for unsafe_absolute_path in [
            "C:\\NUL\\job",
            "\\\\?\\C:\\NUL\\job",
            "\\\\.\\C:\\NUL\\job",
            "C:\\safe\\name.\\job",
            "C:\\safe\\bad?name\\job",
            "\\\\C:\\share\\job",
        ] {
            let mut unsafe_working_directory = request.clone();
            unsafe_working_directory.working_directory = unsafe_absolute_path.to_owned();
            assert!(
                matches!(
                    unsafe_working_directory.validate(),
                    Err(FfmpegContractError::InvalidAbsolutePath { .. })
                ),
                "unsafe working directory must fail: {unsafe_absolute_path}"
            );

            let mut unsafe_executable = request.clone();
            unsafe_executable.toolchain.ffmpeg.absolute_path = unsafe_absolute_path.to_owned();
            assert!(
                matches!(
                    unsafe_executable.validate(),
                    Err(FfmpegContractError::InvalidAbsolutePath { .. })
                ),
                "unsafe executable path must fail: {unsafe_absolute_path}"
            );
        }

        let mut alias = request.clone();
        let FfmpegOperationPlanV1::StreamCopy(plan) = &mut alias.operation;
        plan.inputs[1].path = "output/MERGED.mkv".to_owned();
        assert!(matches!(
            alias.validate(),
            Err(FfmpegContractError::InvalidJobPath { .. })
        ));
    }

    #[test]
    fn tagged_unit_variants_reject_unknown_fields() {
        for (target, value) in [
            ("terminal", json!({"kind":"validated","undeclared":true})),
            (
                "direct_child",
                json!({"kind":"not_spawned","undeclared":true}),
            ),
            ("active_processes", json!({"kind":"zero","undeclared":true})),
            (
                "output_validation",
                json!({"kind":"not_run","undeclared":true}),
            ),
        ] {
            let rejected = match target {
                "terminal" => serde_json::from_value::<FfmpegTerminalOutcomeV1>(value).is_err(),
                "direct_child" => serde_json::from_value::<FfmpegDirectChildReapV1>(value).is_err(),
                "active_processes" => {
                    serde_json::from_value::<WindowsActiveProcessQueryV1>(value).is_err()
                }
                "output_validation" => {
                    serde_json::from_value::<FfmpegOutputValidationV1>(value).is_err()
                }
                _ => false,
            };
            assert!(rejected, "{target} must reject unknown fields");
        }
    }

    #[test]
    fn registered_public_boundary_suite() {
        canonical_fixture_decodes_and_validates();
        canonical_fixture_declares_real_projection_hashes();
        wire_rejects_missing_unknown_reordered_forged_and_cross_version_fields();
        success_requires_reap_containment_and_ffprobe_validation();
        operation_and_resource_boundaries_fail_closed();
        semantic_counterexamples_fail_closed_at_the_public_boundary();
        digest_output_and_deadline_counterexamples_fail_closed();
        pre_spawn_and_invocation_boundaries_are_exact();
        tagged_unit_variants_reject_unknown_fields();
    }
}
