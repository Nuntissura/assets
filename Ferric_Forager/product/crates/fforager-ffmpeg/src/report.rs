//! Strict platform-specific proof reports and same-source aggregation.

use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::collections::{BTreeMap, BTreeSet};

pub const FFMPEG_PLATFORM_PROOF_SCHEMA_ID: &str = "ff.ffmpeg-platform-proof@2";
#[cfg(test)]
const FFMPEG_CROSS_PLATFORM_PROOF_SCHEMA_ID: &str = "ff.ffmpeg-cross-platform-proof@1";

/// Platform row identity; exactly one Windows and one Linux row are required.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProofPlatform {
    WindowsX86_64,
    LinuxX86_64,
}

/// One pinned external-tool observation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolProofIdentityV1 {
    pub executable_name: String,
    pub canonical_path: String,
    pub version_line: String,
    pub content_sha256: String,
    pub file_identity: String,
    pub version_output_sha256: String,
    pub normalized_probe_sha256: String,
    pub capabilities: Vec<String>,
}

/// Closed role for a controlled executable used by platform counterexamples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureToolRoleV1 {
    FakeChild,
    Setsid,
}

/// Exact measured identity of a controlled fixture executable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureToolProofIdentityV1 {
    pub role: FixtureToolRoleV1,
    pub executable_name: String,
    pub canonical_path: String,
    pub version_line: String,
    pub content_sha256: String,
    pub file_identity: String,
}

/// Behavior-sensitive result from the real platform boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the frozen proof wire schema records independently mutable executed behaviors for counterfactual validation"
)]
pub struct PlatformBehaviorV1 {
    pub direct_child_wait_observed: bool,
    pub direct_child_reaped: bool,
    pub successful_exit_observed: bool,
    pub forced_cancellation_observed: bool,
    pub bounded_progress_observed: bool,
    pub bounded_stderr_observed: bool,
    pub output_validated_by_ffprobe: bool,
    pub windows_attached_before_execution: Option<bool>,
    pub windows_kill_on_job_close: Option<bool>,
    pub windows_active_processes: Option<u32>,
    pub windows_handle_sentinel_leaked: Option<bool>,
    pub unix_process_group_observed: Option<bool>,
    pub unix_term_kill_observed: Option<bool>,
    pub unix_setsid_escape_observed: Option<bool>,
}

/// Raw bounded progress counters independently reconciled by the consumer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgressObservationsV1 {
    pub record_count: u64,
    pub total_bytes: u64,
    pub parser_steps: u64,
    pub saw_terminal: bool,
}

/// Exact diagnostic-retention loss declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum DiagnosticLossEvidenceV1 {
    None,
    PrefixTruncated { dropped_bytes: u64 },
}

/// Typed direct-wait inputs used to reconstruct the supervisor receipt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectWaitReceiptEvidenceV1 {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub windows_opaque_status: Option<u32>,
    pub forced_by_supervisor: bool,
    pub supervision_wait_receipt_sha256: String,
}

/// Typed forced-cancellation wait inputs used to reconstruct both the
/// platform termination status and the supervisor receipt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForcedWaitReceiptEvidenceV1 {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub windows_opaque_status: Option<u32>,
    pub forced_by_supervisor: bool,
    pub direct_child_reaped: bool,
    pub lifecycle_timeline_sha256: String,
    pub supervision_wait_receipt_sha256: String,
}

/// Producer-only phase ceilings which are not part of the runtime request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_field_names,
    reason = "the closed wire schema names every producer timeout with explicit units"
)]
pub struct ProducerPhaseLimitsV2 {
    pub fixture_probe_timeout_millis: u64,
    pub identity_probe_timeout_millis: u64,
    pub scope_settlement_timeout_millis: u64,
    pub negative_cases_timeout_millis: u64,
}

/// Measured durations for each independently bounded proof phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_field_names,
    reason = "the closed wire schema names every independent elapsed observation with explicit units"
)]
pub struct PhaseDeadlineObservationsV2 {
    pub fixture_probe_millis: u64,
    pub identity_probe_millis: u64,
    pub startup_millis: u64,
    pub execution_millis: u64,
    pub validation_millis: u64,
    pub graceful_stop_millis: u64,
    pub forced_kill_millis: u64,
    pub reap_millis: u64,
    pub scope_settlement_millis: u64,
    pub negative_cases_millis: u64,
}

/// Raw platform observations from which containment claims are derived.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ContainmentObservationsV1 {
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

/// Strict report produced by an executing platform test, not by packet prose.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformProofReportV2 {
    pub schema_id: String,
    pub source_commit: String,
    pub source_dirty: bool,
    pub platform: ProofPlatform,
    pub host_kernel: String,
    pub ffmpeg: ToolProofIdentityV1,
    pub ffprobe: ToolProofIdentityV1,
    pub fixture_tools: Vec<FixtureToolProofIdentityV1>,
    pub request_contract_canonical_json: String,
    pub request_contract_sha256: String,
    pub operation_plan_canonical_json: String,
    pub operation_plan_sha256: String,
    pub bound_runtime_projection_id: String,
    pub bound_runtime_sha256: String,
    pub argument_vector: Vec<String>,
    pub argument_vector_sha256: String,
    pub fixture_contract_sha256: String,
    pub fixture_payload_canonical_json: String,
    pub fixture_payload_sha256: String,
    pub limits_canonical_json: String,
    pub limits_sha256: String,
    pub lifecycle_timeline_canonical_json: String,
    pub lifecycle_timeline_sha256: String,
    pub direct_wait_receipt_canonical_json: String,
    pub direct_wait_receipt_sha256: String,
    pub forced_lifecycle_timeline_canonical_json: String,
    pub forced_lifecycle_timeline_sha256: String,
    pub forced_wait_receipt_canonical_json: String,
    pub forced_wait_receipt_sha256: String,
    pub output_facts_canonical_json: String,
    pub output_facts_sha256: String,
    pub producer_phase_limits: ProducerPhaseLimitsV2,
    pub phase_deadlines: PhaseDeadlineObservationsV2,
    pub diagnostic_total_bytes: u64,
    pub diagnostic_tail_hex: String,
    pub diagnostic_tail_sha256: String,
    pub diagnostic_loss: DiagnosticLossEvidenceV1,
    pub progress_transcript_hex: String,
    pub progress_transcript_sha256: String,
    pub progress_observations: ProgressObservationsV1,
    pub containment_observations: ContainmentObservationsV1,
    pub behavior: PlatformBehaviorV1,
    pub residual_uncertainty: Vec<String>,
}

/// Independent same-source aggregation result.
#[cfg(test)]
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

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
enum ProofReportError {
    WrongSchema,
    MissingPlatform,
    DuplicatePlatform,
    DirtySource,
    InvalidDigest,
    SourceMismatch,
    RequestMismatch,
    FixtureMismatch,
    LimitsMismatch,
    MissingDirectWait,
    MissingRequiredBehavior,
    FalseWindowsActiveZero,
    WindowsHandleLeak,
    MissingUnixEscapeResidual,
    InvalidPlatformFields,
}

/// Test-only declaration join for DTO regression checks.
///
/// This deliberately is not exported and is not an evidence validator. The
/// independent xtask consumer owns raw-evidence reconstruction and aggregation.
#[cfg(test)]
fn aggregate_declarations_for_schema_tests(
    reports: &[PlatformProofReportV2],
) -> Result<CrossPlatformProofV1, ProofReportError> {
    if reports.len() != 2 {
        return Err(ProofReportError::MissingPlatform);
    }
    let first = &reports[0];
    let mut platforms = BTreeSet::new();
    let mut request_contract_sha256_by_platform = BTreeMap::new();
    let mut residuals = BTreeSet::new();
    for report in reports {
        validate_declarations_for_schema_tests(report)?;
        if !platforms.insert(report.platform) {
            return Err(ProofReportError::DuplicatePlatform);
        }
        if report.source_commit != first.source_commit {
            return Err(ProofReportError::SourceMismatch);
        }
        if report.operation_plan_sha256 != first.operation_plan_sha256 {
            return Err(ProofReportError::RequestMismatch);
        }
        if report.fixture_contract_sha256 != first.fixture_contract_sha256
            || report.fixture_payload_sha256 != first.fixture_payload_sha256
        {
            return Err(ProofReportError::FixtureMismatch);
        }
        if report.limits_sha256 != first.limits_sha256 {
            return Err(ProofReportError::LimitsMismatch);
        }
        request_contract_sha256_by_platform
            .insert(report.platform, report.request_contract_sha256.clone());
        residuals.extend(report.residual_uncertainty.iter().cloned());
    }
    if !platforms.contains(&ProofPlatform::WindowsX86_64)
        || !platforms.contains(&ProofPlatform::LinuxX86_64)
    {
        return Err(ProofReportError::MissingPlatform);
    }
    Ok(CrossPlatformProofV1 {
        schema_id: FFMPEG_CROSS_PLATFORM_PROOF_SCHEMA_ID.to_owned(),
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

#[cfg(test)]
fn validate_declarations_for_schema_tests(
    report: &PlatformProofReportV2,
) -> Result<(), ProofReportError> {
    if report.schema_id != FFMPEG_PLATFORM_PROOF_SCHEMA_ID {
        return Err(ProofReportError::WrongSchema);
    }
    if report.bound_runtime_projection_id
        != fforager_contracts::FFMPEG_BOUND_RUNTIME_INVOCATION_PROJECTION_ID
    {
        return Err(ProofReportError::InvalidPlatformFields);
    }
    if report.source_dirty {
        return Err(ProofReportError::DirtySource);
    }
    for digest in [
        &report.ffmpeg.content_sha256,
        &report.ffprobe.content_sha256,
        &report.request_contract_sha256,
        &report.operation_plan_sha256,
        &report.bound_runtime_sha256,
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
    ] {
        if !is_digest(digest) {
            return Err(ProofReportError::InvalidDigest);
        }
    }
    let behavior = &report.behavior;
    if !behavior.direct_child_wait_observed || !behavior.direct_child_reaped {
        return Err(ProofReportError::MissingDirectWait);
    }
    if !(behavior.successful_exit_observed
        && behavior.forced_cancellation_observed
        && behavior.bounded_progress_observed
        && behavior.bounded_stderr_observed
        && behavior.output_validated_by_ffprobe)
    {
        return Err(ProofReportError::MissingRequiredBehavior);
    }
    match report.platform {
        ProofPlatform::WindowsX86_64 => {
            if behavior.windows_attached_before_execution != Some(true)
                || behavior.windows_kill_on_job_close != Some(true)
                || behavior.windows_active_processes != Some(0)
            {
                return Err(ProofReportError::FalseWindowsActiveZero);
            }
            if behavior.windows_handle_sentinel_leaked != Some(false) {
                return Err(ProofReportError::WindowsHandleLeak);
            }
            if behavior.unix_process_group_observed.is_some()
                || behavior.unix_setsid_escape_observed.is_some()
            {
                return Err(ProofReportError::InvalidPlatformFields);
            }
        }
        ProofPlatform::LinuxX86_64 => {
            if behavior.unix_process_group_observed != Some(true)
                || behavior.unix_term_kill_observed != Some(true)
                || behavior.unix_setsid_escape_observed != Some(true)
            {
                return Err(ProofReportError::MissingRequiredBehavior);
            }
            if !report
                .residual_uncertainty
                .iter()
                .any(|value| value.contains("setsid") && value.contains("not containment"))
            {
                return Err(ProofReportError::MissingUnixEscapeResidual);
            }
            if behavior.windows_active_processes.is_some()
                || behavior.windows_attached_before_execution.is_some()
            {
                return Err(ProofReportError::InvalidPlatformFields);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
fn is_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(
        clippy::too_many_lines,
        reason = "the test constructor intentionally populates every field of the frozen proof wire schema"
    )]
    fn report(platform: ProofPlatform) -> PlatformProofReportV2 {
        let digest = "a".repeat(64);
        let windows = platform == ProofPlatform::WindowsX86_64;
        PlatformProofReportV2 {
            schema_id: FFMPEG_PLATFORM_PROOF_SCHEMA_ID.to_owned(),
            source_commit: "b".repeat(40),
            source_dirty: false,
            platform,
            host_kernel: "test".to_owned(),
            ffmpeg: ToolProofIdentityV1 {
                executable_name: "ffmpeg".to_owned(),
                canonical_path: "/tool/ffmpeg".to_owned(),
                version_line: "ffmpeg test".to_owned(),
                content_sha256: digest.clone(),
                file_identity: "id1".to_owned(),
                version_output_sha256: digest.clone(),
                normalized_probe_sha256: digest.clone(),
                capabilities: vec!["progress".to_owned()],
            },
            ffprobe: ToolProofIdentityV1 {
                executable_name: "ffprobe".to_owned(),
                canonical_path: "/tool/ffprobe".to_owned(),
                version_line: "ffprobe test".to_owned(),
                content_sha256: digest.clone(),
                file_identity: "id2".to_owned(),
                version_output_sha256: digest.clone(),
                normalized_probe_sha256: digest.clone(),
                capabilities: vec!["json_output".to_owned()],
            },
            fixture_tools: vec![FixtureToolProofIdentityV1 {
                role: FixtureToolRoleV1::FakeChild,
                executable_name: "fforager-fake-child".to_owned(),
                canonical_path: "/tool/fforager-fake-child".to_owned(),
                version_line: "fforager-fake-child 0.1.0".to_owned(),
                content_sha256: digest.clone(),
                file_identity: "fixture-id".to_owned(),
            }],
            request_contract_canonical_json: "{}".to_owned(),
            request_contract_sha256: digest.clone(),
            operation_plan_canonical_json: "{}".to_owned(),
            operation_plan_sha256: digest.clone(),
            bound_runtime_projection_id:
                fforager_contracts::FFMPEG_BOUND_RUNTIME_INVOCATION_PROJECTION_ID.to_owned(),
            bound_runtime_sha256: digest.clone(),
            argument_vector: vec!["-version".to_owned()],
            argument_vector_sha256: digest.clone(),
            fixture_contract_sha256: digest.clone(),
            fixture_payload_canonical_json: "[]".to_owned(),
            fixture_payload_sha256: digest.clone(),
            limits_canonical_json: "{}".to_owned(),
            limits_sha256: digest.clone(),
            lifecycle_timeline_canonical_json: "[]".to_owned(),
            lifecycle_timeline_sha256: digest.clone(),
            direct_wait_receipt_canonical_json: "{}".to_owned(),
            direct_wait_receipt_sha256: digest.clone(),
            forced_lifecycle_timeline_canonical_json: "[]".to_owned(),
            forced_lifecycle_timeline_sha256: digest.clone(),
            forced_wait_receipt_canonical_json: "{}".to_owned(),
            forced_wait_receipt_sha256: digest.clone(),
            output_facts_canonical_json: "{}".to_owned(),
            output_facts_sha256: digest.clone(),
            producer_phase_limits: ProducerPhaseLimitsV2 {
                fixture_probe_timeout_millis: 10,
                identity_probe_timeout_millis: 10,
                scope_settlement_timeout_millis: 10,
                negative_cases_timeout_millis: 10,
            },
            phase_deadlines: PhaseDeadlineObservationsV2 {
                fixture_probe_millis: 1,
                identity_probe_millis: 1,
                startup_millis: 1,
                execution_millis: 1,
                validation_millis: 1,
                graceful_stop_millis: 0,
                forced_kill_millis: 1,
                reap_millis: 1,
                scope_settlement_millis: 1,
                negative_cases_millis: 1,
            },
            diagnostic_total_bytes: 0,
            diagnostic_tail_hex: String::new(),
            diagnostic_tail_sha256: digest.clone(),
            diagnostic_loss: DiagnosticLossEvidenceV1::None,
            progress_transcript_hex: "70726f67726573733d656e640a".to_owned(),
            progress_transcript_sha256: digest,
            progress_observations: ProgressObservationsV1 {
                record_count: 1,
                total_bytes: 13,
                parser_steps: 21,
                saw_terminal: true,
            },
            containment_observations: if windows {
                ContainmentObservationsV1::WindowsJob {
                    active_process_samples: vec![1, 0],
                    attached_before_execution: true,
                    kill_on_job_close: true,
                    handle_sentinel_leaked: false,
                    kill_on_job_close_parent_death_observed: true,
                    suspended_orphan_observed: false,
                }
            } else {
                ContainmentObservationsV1::UnixProcessGroup {
                    process_group_verified: true,
                    term_sent: true,
                    kill_sent: true,
                    group_absent: true,
                    setsid_escape_observed: true,
                }
            },
            behavior: PlatformBehaviorV1 {
                direct_child_wait_observed: true,
                direct_child_reaped: true,
                successful_exit_observed: true,
                forced_cancellation_observed: true,
                bounded_progress_observed: true,
                bounded_stderr_observed: true,
                output_validated_by_ffprobe: true,
                windows_attached_before_execution: windows.then_some(true),
                windows_kill_on_job_close: windows.then_some(true),
                windows_active_processes: windows.then_some(0),
                windows_handle_sentinel_leaked: windows.then_some(false),
                unix_process_group_observed: (!windows).then_some(true),
                unix_term_kill_observed: (!windows).then_some(true),
                unix_setsid_escape_observed: (!windows).then_some(true),
            },
            residual_uncertainty: if windows {
                vec!["suspended orphan interval remains".to_owned()]
            } else {
                vec![
                    "setsid escape proves process groups are signal scopes, not containment"
                        .to_owned(),
                ]
            },
        }
    }

    #[test]
    fn same_source_pair_aggregates() {
        let windows = report(ProofPlatform::WindowsX86_64);
        let mut linux = report(ProofPlatform::LinuxX86_64);
        linux.request_contract_sha256 = "c".repeat(64);
        let aggregate = aggregate_declarations_for_schema_tests(&[windows, linux])
            .expect("platform-bound request identities may differ");
        assert_eq!(aggregate.request_contract_sha256_by_platform.len(), 2);
    }

    #[test]
    fn platform_neutral_join_fields_must_match() {
        let windows = report(ProofPlatform::WindowsX86_64);
        let mut linux = report(ProofPlatform::LinuxX86_64);
        linux.operation_plan_sha256 = "d".repeat(64);
        assert_eq!(
            aggregate_declarations_for_schema_tests(&[windows.clone(), linux]),
            Err(ProofReportError::RequestMismatch)
        );
        let mut linux = report(ProofPlatform::LinuxX86_64);
        linux.limits_sha256 = "e".repeat(64);
        assert_eq!(
            aggregate_declarations_for_schema_tests(&[windows, linux]),
            Err(ProofReportError::LimitsMismatch)
        );
    }

    #[test]
    fn false_active_zero_is_rejected() {
        let mut windows = report(ProofPlatform::WindowsX86_64);
        windows.behavior.windows_active_processes = Some(1);
        assert_eq!(
            aggregate_declarations_for_schema_tests(
                &[windows, report(ProofPlatform::LinuxX86_64),]
            ),
            Err(ProofReportError::FalseWindowsActiveZero)
        );
    }

    #[test]
    fn declaration_preserving_behavior_mutation_is_rejected() {
        let mut linux = report(ProofPlatform::LinuxX86_64);
        linux.behavior.direct_child_wait_observed = false;
        assert_eq!(
            aggregate_declarations_for_schema_tests(
                &[report(ProofPlatform::WindowsX86_64), linux,]
            ),
            Err(ProofReportError::MissingDirectWait)
        );
    }

    #[test]
    fn bound_runtime_identity_must_be_versioned_and_well_formed() {
        let mut windows = report(ProofPlatform::WindowsX86_64);
        windows.bound_runtime_projection_id = "unversioned-runtime-arguments".to_owned();
        assert_eq!(
            validate_declarations_for_schema_tests(&windows),
            Err(ProofReportError::InvalidPlatformFields)
        );

        let mut windows = report(ProofPlatform::WindowsX86_64);
        windows.bound_runtime_sha256 = "not-a-digest".to_owned();
        assert_eq!(
            validate_declarations_for_schema_tests(&windows),
            Err(ProofReportError::InvalidDigest)
        );
    }
}
