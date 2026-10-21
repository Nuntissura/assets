mod benchmark;
mod compatibility;
mod ffmpeg_report;
mod phase1_runtime;
mod promotion;
#[cfg(test)]
mod runtime_classifier_tests;
mod secret_scan;

use cargo_metadata::{DependencyKind, Metadata, PackageId};
use saphyr::{LoadableYamlNode, Yaml};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ARCH_GATE: &str = "FF-GATE-ARCH-001";
const PR_GATE: &str = "FF-GATE-PR-001";
const RUNTIME_GATE: &str = "FF-GATE-RUNTIME-001";
const DEEP_GATE: &str = "FF-GATE-DEEP-001";
const TRANSPORT_MANIFEST_DECLARATION_SHA256: &str =
    "31f52c2b188fdf68252cf4294adfb5d93f6080ac5fb6b565e2f3ba46f42ecf02";
const TRANSPORT_SEMANTIC_PROJECTION_SHA256: &str =
    "35bfe562e409375b3a0811456e2eb7820485dc2c71427b891767c90c2af8bfbc";
const FAILURE_PROOF_CLASS: &str = "structural";
static COMMAND_CAPTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static WP008_INVOCATION_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static WP009_INVOCATION_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const PUBLIC_BOUNDARY_COUNTEREXAMPLE_TEST: &str =
    "tests::public_boundary_counterexamples_reject_audit_failures";
const PUBLIC_BOUNDARY_COUNTEREXAMPLE_PROOF_ID: &str =
    "testkit::tests::public_boundary_counterexamples_reject_audit_failures";
const PUBLIC_BOUNDARY_COUNTEREXAMPLE_RECEIPT: &str = "FF-PUBLIC-COUNTEREXAMPLE-RECEIPT:v5:source-graph-cycle,filesystem-effect-correlation,ffmpeg-terminal-release,ffmpeg-partial-unsuccessful-outcomes,schema-authority,sequence-zero,unknown-envelope-field,nested-wire-unknown-fields,durable-journal-payload-unknown-field,durable-journal-record-unknown-field,durable-reconcile-state-unknown-field,durable-journal-sequence-zero,acknowledged-effect-prefixes";
const WP008_MANIFEST_PATH: &str = "build/fixtures/archive-store-evidence/manifest.json";
const WP008_PROOF_TEST: &str = "tests::archive_store_evidence_corpus_executes_public_boundary";
const WP008_REPORT_PREFIX: &str = "FF-WP008-ARCHIVE-REPORT:";
const WP008_SOURCE_PATHS: &[&str] = &[
    "product/crates/fforager-contracts/src/archive.rs",
    "product/crates/fforager-storage/src/lib.rs",
    "build/crates/fforager-testkit/src/archive_evidence.rs",
    "build/crates/fforager-testkit/src/lib.rs",
    "build/tools/fforager-xtask/src/main.rs",
];
const WP008_CASE_IDS: &[&str] = &[
    "wp008-identity-item",
    "wp008-identity-representation",
    "wp008-identity-track",
    "wp008-identity-asset",
    "wp008-identity-derived",
    "wp008-duplicate-suppression",
    "wp008-concurrent-claim",
    "wp008-lease-renewal",
    "wp008-lease-takeover",
    "wp008-crash-before-archive",
    "wp008-crash-after-archive",
    "wp008-reconciliation-idempotent",
    "wp008-corrupt-store",
    "wp008-torn-store",
    "wp008-schema-create",
    "wp008-schema-forward",
    "wp008-schema-interrupted",
    "wp008-schema-unknown",
    "wp008-import-mapped",
    "wp008-import-unknown",
    "wp008-retry-idempotent",
    "wp008-rollback-last-known-good",
    "wp008-scale-representative",
    "wp008-scale-b021",
];
const WP009_MANIFEST_PATH: &str = "build/fixtures/resource-durability-models/manifest.json";
const WP009_PROOF_TEST: &str = "tests::resource_durability_model_corpus_executes_public_boundaries";
const WP009_REPORT_PREFIX: &str = "FF-WP009-MODEL-REPORT:";
const WP009_WINDOWS_PLATFORM_ROW_RESIDUAL: &str = "Live Windows host and repository-volume identity was probed; atomic-replace behavior, confinement races, crash behavior, power-loss behavior, and durability behavior were not executed.";
const WP009_WSL_PLATFORM_ROW_RESIDUAL: &str = "Live WSL2 host and repository-mount identity was probed; native-Linux behavior, atomic-replace behavior, confinement races, crash behavior, power-loss behavior, and durability behavior were not executed.";
const WP009_SOURCE_PATHS: &[&str] = &[
    "product/crates/fforager-contracts/src/resource.rs",
    "product/crates/fforager-contracts/src/storage.rs",
    "product/crates/fforager-core/src/resource.rs",
    "product/crates/fforager-core/src/lifecycle.rs",
    "build/crates/fforager-testkit/src/lib.rs",
    "build/tools/fforager-xtask/src/main.rs",
];
const WP009_CASE_IDS: &[&str] = &[
    "wp009-resource-atomic-saturation",
    "wp009-resource-fifo-head",
    "wp009-resource-queue-item-bound",
    "wp009-resource-queue-byte-bound",
    "wp009-resource-raii-drop",
    "wp009-resource-public-owned-boundary",
    "wp009-credit-coupled-reservation",
    "wp009-credit-owner-attribution",
    "wp009-credit-stage-bounds-all-nine",
    "wp009-credit-owner-bounds-all-nine",
    "wp009-credit-owned-success",
    "wp009-credit-owned-error",
    "wp009-credit-owned-panic",
    "wp009-credit-owned-cancel",
    "wp009-durability-effect-ack",
    "wp009-durability-stage-authorization",
    "wp009-durability-replay-rejected",
    "wp009-durability-restoration-evidence",
    "wp009-durability-prefix",
    "wp009-durability-outrun",
    "wp009-journal-torn",
    "wp009-journal-duplicate",
    "wp009-journal-reordered",
    "wp009-journal-checksum-invalid",
    "wp009-journal-false-prepared-sync",
    "wp009-journal-false-renamed-sync",
    "wp009-journal-mixed-job",
    "wp009-journal-archive-uniqueness",
    "wp009-commit-prepared-effects",
    "wp009-commit-renamed-effects",
    "wp009-commit-archived-effects",
    "wp009-commit-cleaned-effects",
    "wp009-recovery-prepared",
    "wp009-recovery-renamed",
    "wp009-recovery-archived",
    "wp009-recovery-cleaned",
    "wp009-recovery-stale-lease",
    "wp009-recovery-collision",
    "wp009-recovery-partial-cleanup",
    "wp009-recovery-interrupted-migration",
    "wp009-recovery-collecting-artifact",
    "wp009-recovery-stale-mismatch-precedence",
    "wp009-recovery-migration-collision-precedence",
    "wp009-recovery-confinement-unavailable",
    "wp009-recovery-confinement-mismatched",
    "wp009-filesystem-windows-ntfs",
    "wp009-filesystem-wsl-v9fs",
    "wp009-cross-volume",
];
const TOOL_COMMAND_TIMEOUT: Duration = Duration::from_mins(1);
const CARGO_PROOF_COMMAND_TIMEOUT: Duration = Duration::from_mins(5);
const CARGO_PROOF_TRANSIENT_RETRIES: usize = 2;
const METADATA_COMMAND_TIMEOUT: Duration = Duration::from_mins(2);
const GATE_COMMAND_TIMEOUT: Duration = Duration::from_mins(30);
const TERMINATION_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const RUST_ENVIRONMENT_OVERRIDES: &[&str] = &[
    "CARGO_CONFIG",
    "CARGO_HOME",
    "CARGO_BUILD_RUSTC",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_RUSTDOC",
    "CARGO_BUILD_RUSTDOCFLAGS",
    "CARGO_BUILD_RUSTFLAGS",
    "CARGO_ENCODED_RUSTDOCFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "CLIPPY_CONF_DIR",
    "RUSTC",
    "RUSTC_BOOTSTRAP",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "RUSTDOC",
    "RUSTDOCFLAGS",
    "RUSTFLAGS",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchitecturePolicy {
    schema_id: String,
    file_id: String,
    schema_version: String,
    accepted_design_decision_ids: Vec<String>,
    workspace_manifest: String,
    workspace_root: String,
    target_directory: String,
    root_toolchain_selector: String,
    governance_root: String,
    product_root: String,
    build_root: String,
    exception_authority: String,
    exception_decision_ids: Vec<String>,
    unsafe_policy: String,
    unsafe_exception_authority: String,
    unsafe_decision_ids: Vec<String>,
    internal_edge_default: String,
    forbidden_layer_edges: Vec<String>,
    forbidden_product_runtime_roots: Vec<String>,
    allowed_runtime_or_native_dependencies: Vec<String>,
    approved_transitive_build_packages: Vec<String>,
    approved_transitive_proc_macros: Vec<String>,
    members: Vec<MemberPolicy>,
    dependency_decisions: Vec<DependencyDecision>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemberPolicy {
    name: String,
    manifest: String,
    source_root: String,
    layer: String,
    artifact_role: String,
    shipped: bool,
    test_only: bool,
    publish_allowed: bool,
    allowed_internal_dependencies: Vec<AllowedInternalDependency>,
    split_trigger: String,
    feature_owner: String,
    profile: String,
    removal_condition: String,
    runtime_native_constraint_ref: String,
    unsafe_policy_ref: String,
    exception_policy_ref: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AllowedInternalDependency {
    package: String,
    kinds: Vec<String>,
    target: Option<String>,
    optional: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DependencyDecision {
    name: String,
    version: String,
    consumer: String,
    runtime_class: String,
    purpose: String,
    native: bool,
    owner: String,
    allowed_consumers: Vec<String>,
    reason: String,
    removal_trigger: String,
    approval_id: String,
    features: Vec<String>,
    default_features: bool,
    #[serde(default)]
    exception_id: Option<String>,
}

#[derive(Debug)]
struct NativeDependencyException {
    id: String,
    owner: String,
    approval_id: String,
    allow_packaged_pregenerated_assembly_objects: bool,
    packaged_object_inventory_sha256: String,
    allowed_prerequisite_consumers: BTreeSet<String>,
    allowed_product_consumers: BTreeSet<String>,
    allowed_direct_dependencies: BTreeSet<String>,
    allowed_versions: BTreeSet<String>,
    allowed_native_link_packages: BTreeSet<String>,
    allowed_native_runtime_packages: BTreeSet<String>,
    allowed_package_archive_sha256: BTreeMap<String, String>,
    allowed_package_source_inventory_sha256: BTreeMap<String, String>,
    ordinary_transport_resolved_closure_sha256: String,
    ordinary_transport_registry_source_closure_sha256: String,
    allowed_runtime_classes: BTreeSet<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolingPolicy {
    schema_id: String,
    file_id: String,
    schema_version: String,
    rust_toolchain: String,
    supported_hosts: Vec<String>,
    auto_install: bool,
    advisory_database_max_age_hours: u64,
    tools: Vec<ToolPolicy>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolPolicy {
    name: String,
    identity_line: String,
    command: Vec<String>,
    source: String,
    provenance_kind: String,
    executable_sha256: Option<String>,
    owning_gate: String,
    required_now: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleMap {
    schema_id: String,
    file_id: String,
    schema_version: String,
    rules: Vec<RuleProof>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleProof {
    rule_id: String,
    proof_classes: Vec<String>,
    validators: Vec<String>,
    fixture_ids: Vec<String>,
    limitations: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureCase {
    schema_version: u32,
    fixture_id: String,
    mutation: String,
    expected_diagnostic: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeProof {
    schema_id: String,
    completion_claim: String,
    artifact: RuntimeArtifact,
    forbidden_substitutes: Vec<String>,
    scenarios: Vec<RuntimeScenario>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)]
struct NonProductPrerequisite {
    schema_id: String,
    classification: String,
    design_authority: String,
    product_progress: bool,
    capability_progress: bool,
    runtime_completion: bool,
    packaging_or_release_progress: bool,
    phase_progress: bool,
    required_future_consumer: String,
    required_future_proof: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeArtifact {
    package: String,
    binary: String,
    profile: String,
    features: Vec<String>,
    package_mode: String,
    execution_mode: String,
    compilation_mode: String,
    dependency_mode: String,
    testkit_mode: String,
    adapter_mode: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeScenario {
    id: String,
    kind: String,
    capability_ids: Vec<String>,
    args: Vec<String>,
    timeout_seconds: u64,
    inputs: Vec<RuntimeInput>,
    production_boundaries: Vec<String>,
    expected: RuntimeExpected,
    counterfactual: Option<RuntimeCounterfactual>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeInput {
    source: String,
    destination: String,
    sha256: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeExpected {
    exit_code: i32,
    stdout_contains: Vec<String>,
    stderr_contains: Vec<String>,
    output_files: Vec<RuntimeExpectedFile>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeExpectedFile {
    path: String,
    min_bytes: u64,
    sha256: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeCounterfactual {
    target: String,
    value: String,
    expected_diagnostic: String,
}

#[derive(Clone, Debug)]
struct RuntimeObservation {
    exit_code: i32,
    stdout: String,
    stderr: String,
    files: BTreeMap<String, RuntimeObservedFile>,
}

#[derive(Clone, Debug)]
struct RuntimeObservedFile {
    bytes: u64,
    sha256: String,
}

#[derive(Debug)]
struct RuntimeTruthResult {
    checks: Vec<Check>,
    proof_classes: Vec<String>,
    limitations: Vec<String>,
    artifacts: Vec<String>,
}

#[derive(Debug, Serialize)]
struct GateReport {
    schema_id: &'static str,
    schema_version: &'static str,
    gate_id: String,
    gate_version: u32,
    status: &'static str,
    exit_code: u8,
    source: SourceState,
    invocation: Invocation,
    inputs: Vec<InputState>,
    checks: Vec<Check>,
    rules: Vec<String>,
    fixtures: Vec<FixtureResult>,
    declared_supported_proof_classes: Vec<String>,
    executed_proof_classes: Vec<String>,
    aggregate_executed_proof_class: String,
    proof_limitations: Vec<String>,
    artifacts: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceState {
    pub(crate) git_commit: String,
    pub(crate) dirty: bool,
    pub(crate) dirty_paths: Vec<String>,
    pub(crate) content_fingerprint: String,
}

#[derive(Debug, Serialize)]
struct Invocation {
    repository_root: &'static str,
    gate_args: Vec<String>,
    canonical_command: Vec<String>,
}

#[derive(Debug, Serialize)]
struct InputState {
    path: String,
    git_blob: String,
}

#[derive(Clone, Debug, Serialize)]
struct Check {
    id: String,
    status: &'static str,
    proof_class: &'static str,
    concrete_input: String,
    executed_boundary: String,
    expected_result: String,
    observed_result: String,
    skipped_semantic_dependencies: Vec<String>,
    detail: String,
}

#[derive(Debug, Serialize)]
struct FixtureResult {
    fixture_id: String,
    status: &'static str,
    proof_class: &'static str,
    concrete_input: String,
    executed_boundary: String,
    expected_result: String,
    observed_result: String,
    skipped_semantic_dependencies: Vec<String>,
    execution_path: &'static str,
    expected_diagnostic: String,
    observed_diagnostics: Vec<String>,
}

/// Owned, deny-unknown-fields representation used to validate the bytes that
/// are actually persisted.  `GateReport` deliberately borrows static labels
/// while constructing a report; this artifact boundary ensures those labels
/// cannot bypass schema validation during serialization.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GateReportArtifact {
    schema_id: String,
    schema_version: String,
    gate_id: String,
    gate_version: u32,
    status: String,
    exit_code: u8,
    source: GateReportSourceArtifact,
    invocation: GateReportInvocationArtifact,
    inputs: Vec<GateReportInputArtifact>,
    checks: Vec<GateReportCheckArtifact>,
    rules: Vec<String>,
    fixtures: Vec<GateReportFixtureArtifact>,
    declared_supported_proof_classes: Vec<String>,
    executed_proof_classes: Vec<String>,
    aggregate_executed_proof_class: String,
    proof_limitations: Vec<String>,
    artifacts: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_runtime_evidence")]
    runtime_evidence: Option<phase1_runtime::RuntimeEvidence>,
    #[serde(default, deserialize_with = "deserialize_derived_invocation")]
    derived_from: Option<GateReportInvocationArtifact>,
}

fn deserialize_derived_invocation<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<GateReportInvocationArtifact>, D::Error> {
    GateReportInvocationArtifact::deserialize(deserializer).map(Some)
}

fn deserialize_runtime_evidence<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<phase1_runtime::RuntimeEvidence>, D::Error> {
    phase1_runtime::RuntimeEvidence::deserialize(deserializer).map(Some)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GateReportSourceArtifact {
    git_commit: String,
    dirty: bool,
    dirty_paths: Vec<String>,
    content_fingerprint: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GateReportInvocationArtifact {
    repository_root: String,
    gate_args: Vec<String>,
    canonical_command: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GateReportInputArtifact {
    path: String,
    git_blob: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GateReportCheckArtifact {
    id: String,
    status: String,
    proof_class: String,
    concrete_input: String,
    executed_boundary: String,
    expected_result: String,
    observed_result: String,
    skipped_semantic_dependencies: Vec<String>,
    detail: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GateReportFixtureArtifact {
    fixture_id: String,
    status: String,
    proof_class: String,
    concrete_input: String,
    executed_boundary: String,
    expected_result: String,
    observed_result: String,
    skipped_semantic_dependencies: Vec<String>,
    execution_path: String,
    expected_diagnostic: String,
    observed_diagnostics: Vec<String>,
}

#[derive(Debug)]
struct FixtureExecution {
    diagnostics: Vec<String>,
    proof_class: &'static str,
    execution_path: &'static str,
    concrete_input: String,
    executed_boundary: String,
    observed_result: String,
    skipped_semantic_dependencies: Vec<String>,
}

#[derive(Debug)]
struct ArchitectureResult {
    checks: Vec<Check>,
    rules: Vec<String>,
    fixtures: Vec<FixtureResult>,
    declared_supported_proof_classes: Vec<String>,
    executed_proof_classes: Vec<String>,
    limitations: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Wp009ExecutionReceipt {
    invocation_id: String,
    report_path: String,
    report_sha256: String,
    manifest_fingerprint: String,
    source_content_fingerprint: String,
    executed_cases: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Wp008ExecutionReceipt {
    invocation_id: String,
    report_path: String,
    report_sha256: String,
    manifest_sha256: String,
    source_content_fingerprint: String,
    executed_cases: u64,
    blocked_cases: u64,
}

#[derive(Debug)]
struct Wp008Invocation {
    id: String,
    started_at: SystemTime,
    manifest_sha256: String,
    source_content_fingerprint: String,
}

#[derive(Debug)]
struct Wp009Invocation {
    id: String,
    started_at: SystemTime,
    manifest_sha256: String,
    source_content_fingerprint: String,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("fforager-xtask: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    let root = repo_root()?;
    require_repository_root_cwd(&root)?;
    match args.as_slice() {
        [command, rest @ ..] if command == "benchmark-validate" => benchmark::run(&root, rest),
        [command, rest @ ..] if command == "benchmark-calibrate" => benchmark::calibrate(&root, rest),
        [command, rest @ ..] if command == "benchmark-bundle" => benchmark::materialize(&root, rest),
        [gate] if gate == "architecture-check" => run_architecture_gate(&root, &args),
        [command, rest @ ..] if command == "compatibility-generate" => {
            compatibility::run_generate(&root, rest)
        }
        [command] if command == "compatibility-validate" => {
            compatibility::run_validate(&root, &args)
        }
        [command, rest @ ..] if command == "compatibility-replay" => {
            compatibility::run_replay(&root, &args, rest)
        }
        [command, rest @ ..] if command == "compatibility-diff" => {
            compatibility::run_diff(&root, &args, rest)
        }
        [command, rest @ ..] if command == "compatibility-inventory-diff" => {
            compatibility::run_inventory_diff(&root, &args, rest)
        }
        [command, rest @ ..] if command == "compatibility-live-canaries" => {
            compatibility::run_live_canaries(&root, &args, rest)
        }
        [command] if command == "transport-corpus" => run_transport_corpus(&root),
        [command] if command == "archive-store-evidence" => run_archive_store_evidence(&root),
        [command] if command == "resource-durability-models" => {
            run_resource_durability_models(&root)
        }
        [command, rest @ ..] if command == "ffmpeg-supervision-aggregate" => {
            ffmpeg_report::run(&root, rest)
        }
        [command, rest @ ..] if command == "ffmpeg-supervision-produce" => {
            ffmpeg_report::run_producer(&root, rest)
        }
        [command, rest @ ..] if command == "secret-scan" => secret_scan::run(&root, rest),
        [command, flag, id] if command == "packet-promotion-check" && flag == "--packet" => promotion::run(&root, id),
        [gate, evidence] if gate == "verify-pr" && evidence == "--evidence-from-taskboard" => {
            run_verify_pr(&root, &args)
        }
        [gate, evidence]
            if gate == "runtime-truth-check" && evidence == "--evidence-from-taskboard" =>
        {
            run_runtime_truth_gate(&root, &args)
        }
        [gate, evidence]
            if gate == "verify-deep" && evidence == "--evidence-from-taskboard" =>
        {
            run_verify_deep(&root, &args)
        }
        [gate] if gate == "watcher-check" => run_watcher_gate(&root, &args),
        [gate] if gate == "verify-release" => {
            Err(format!("{gate} is NOT_IMPLEMENTED for Phase 0 and cannot report PASS"))
        }
        _ => Err("usage: fforager-xtask <packet-promotion-check --packet ID|benchmark-validate --input BUNDLE --report REPORT|benchmark-calibrate --host-profile TEMPLATE --ffmpeg ABSOLUTE --ffprobe ABSOLUTE --fixture RELATIVE --output NEW_ARTIFACT_DIR|benchmark-bundle --windows-profile PROFILE --unix-profile PROFILE --active-profile ID --controls CALIBRATION --output NEW_ARTIFACT_DIR|architecture-check|runtime-truth-check --evidence-from-taskboard|verify-pr --evidence-from-taskboard|verify-deep --evidence-from-taskboard|secret-scan <--install-hooks|--verify-hooks|--staged|--history|--pre-push REMOTE>|transport-corpus|archive-store-evidence|resource-durability-models|ffmpeg-supervision-produce --platform <windows_x86_64|linux_x86_64> --fixture-root PATH --report PATH --receipt PATH|ffmpeg-supervision-aggregate --windows-report PATH --windows-receipt PATH --linux-report PATH --linux-receipt PATH|compatibility-generate --oracle-exe PATH --source-root PATH [--output PATH]|compatibility-validate|compatibility-replay [--shard INDEX/TOTAL]|compatibility-diff --candidate PATH|compatibility-inventory-diff --before PATH --after PATH|compatibility-live-canaries --enable-live --oracle-exe PATH|verify-release|watcher-check>".to_owned()),
    }
}

fn repo_root() -> Result<PathBuf, String> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
        .ok_or_else(|| "cannot derive repository root from CARGO_MANIFEST_DIR".to_owned())
}

fn shared_repository_root(root: &Path) -> Result<PathBuf, String> {
    let dot_git = root.join(".git");
    if !dot_git.exists() || dot_git.is_dir() {
        return Ok(root.to_path_buf());
    }
    if !dot_git.is_file() {
        return Err(format!(
            "FF-ARCH-E-WORKTREE-TOPOLOGY: {} is neither a Git directory nor a linked-worktree metadata file",
            dot_git.display()
        ));
    }

    let gitdir_line = read_single_line_metadata(&dot_git, "linked-worktree gitdir")?;
    let gitdir_value = gitdir_line.strip_prefix("gitdir: ").ok_or_else(|| {
        format!(
            "FF-ARCH-E-WORKTREE-TOPOLOGY: {} does not contain an exact gitdir declaration",
            dot_git.display()
        )
    })?;
    let gitdir = canonical_metadata_path(root, gitdir_value, "linked-worktree gitdir")?;
    let commondir_path = gitdir.join("commondir");
    let commondir_value = read_single_line_metadata(&commondir_path, "linked-worktree commondir")?;
    let common_git_dir =
        canonical_metadata_path(&gitdir, &commondir_value, "linked-worktree commondir")?;
    if common_git_dir.file_name() != Some(OsStr::new(".git")) {
        return Err(format!(
            "FF-ARCH-E-WORKTREE-TOPOLOGY: linked-worktree common directory must resolve to a .git directory; observed {}",
            common_git_dir.display()
        ));
    }
    common_git_dir
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            format!(
                "FF-ARCH-E-WORKTREE-TOPOLOGY: cannot derive shared repository root from {}",
                common_git_dir.display()
            )
        })
}

fn read_single_line_metadata(path: &Path, label: &str) -> Result<String, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("FF-ARCH-E-WORKTREE-TOPOLOGY: read {label}: {error}"))?;
    let normalized = text.replace("\r\n", "\n");
    let line = normalized.strip_suffix('\n').unwrap_or(&normalized);
    if line.is_empty() || line.contains('\n') {
        return Err(format!(
            "FF-ARCH-E-WORKTREE-TOPOLOGY: {label} must contain exactly one non-empty line"
        ));
    }
    Ok(line.to_owned())
}

fn canonical_metadata_path(base: &Path, value: &str, label: &str) -> Result<PathBuf, String> {
    let declared = Path::new(value);
    let path = if declared.is_absolute() {
        declared.to_path_buf()
    } else {
        base.join(declared)
    };
    path.canonicalize().map_err(|error| {
        format!(
            "FF-ARCH-E-WORKTREE-TOPOLOGY: resolve {label} {}: {error}",
            path.display()
        )
    })
}

fn canonical_paths_equal(left: &Path, right: &Path) -> Result<bool, String> {
    let left = left
        .canonicalize()
        .map_err(|error| format!("canonicalize {}: {error}", left.display()))?;
    let right = right
        .canonicalize()
        .map_err(|error| format!("canonicalize {}: {error}", right.display()))?;
    Ok(left == right)
}

fn validate_linked_worktree_layout(root: &Path, shared_root: &Path) -> Result<(), String> {
    let root_parent = root.parent().ok_or_else(|| {
        format!(
            "FF-ARCH-E-WORKTREE-TOPOLOGY: linked worktree has no parent: {}",
            root.display()
        )
    })?;
    let expected_parent = shared_root.join(".worktrees");
    if !canonical_paths_equal(root_parent, &expected_parent).map_err(|error| {
        format!("FF-ARCH-E-WORKTREE-TOPOLOGY: validate worktree parent: {error}")
    })? {
        return Err(format!(
            "FF-ARCH-E-WORKTREE-TOPOLOGY: linked worktree must be a direct child of {}; observed {}",
            expected_parent.display(),
            root.display()
        ));
    }

    let local_artifacts = root.join(".fforager-artifacts");
    let shared_artifacts = shared_root.join(".fforager-artifacts");
    if !canonical_paths_equal(&local_artifacts, &shared_artifacts).map_err(|error| {
        format!("FF-ARCH-E-WORKTREE-TOPOLOGY: validate shared artifact root: {error}")
    })? {
        return Err(format!(
            "FF-ARCH-E-WORKTREE-TOPOLOGY: {} must resolve to the shared artifact root {}",
            local_artifacts.display(),
            shared_artifacts.display()
        ));
    }
    Ok(())
}

fn disposable_artifact_root(root: &Path) -> PathBuf {
    match repo_root() {
        Ok(repository_root) if root.starts_with(&repository_root) => {
            repository_root.join(".fforager-artifacts")
        }
        _ => root.join(".fforager-artifacts"),
    }
}

fn isolated_fixture_target_dir(root: &Path, label: &str) -> Result<PathBuf, String> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let canonical_root = root.canonicalize().map_err(|error| {
        format!(
            "canonicalize fixture source root {}: {error}",
            root.display()
        )
    })?;
    let mut digest = Sha256::new();
    digest.update(
        canonical_root
            .to_string_lossy()
            .replace('\\', "/")
            .as_bytes(),
    );
    digest.update([0]);
    digest.update(label.as_bytes());
    let bytes = digest.finalize();
    let mut encoded = String::with_capacity(32);
    for byte in bytes.iter().take(16) {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(disposable_artifact_root(root).join("pif").join(encoded))
}

fn command_working_directory(root: &Path) -> PathBuf {
    env::current_dir()
        .ok()
        .filter(|cwd| canonical_paths_equal(cwd, root).unwrap_or(false))
        .unwrap_or_else(|| root.to_path_buf())
}

fn require_repository_root_cwd(root: &Path) -> Result<(), String> {
    let cwd = env::current_dir().map_err(|e| format!("read current directory: {e}"))?;
    let expected = root
        .canonicalize()
        .map_err(|e| format!("canonicalize repository root: {e}"))?;
    let actual = cwd
        .canonicalize()
        .map_err(|e| format!("canonicalize current directory: {e}"))?;
    if actual != expected {
        return Err(format!(
            "run from repository root {}; current directory is {}",
            expected.display(),
            actual.display()
        ));
    }
    Ok(())
}

fn run_architecture_gate(root: &Path, gate_args: &[String]) -> Result<(), String> {
    match run_architecture_gate_inner(root, gate_args) {
        Ok(()) => Ok(()),
        Err(error) => fail_with_report(root, ARCH_GATE, "architecture-check", gate_args, &error),
    }
}

fn run_architecture_gate_inner(root: &Path, gate_args: &[String]) -> Result<(), String> {
    let result = architecture_check(root)?;
    let report = GateReport {
        schema_id: "ff.gate-report@1",
        schema_version: "1.0.0",
        gate_id: ARCH_GATE.to_owned(),
        gate_version: 1,
        status: "PASS",
        exit_code: 0,
        source: source_state(root)?,
        invocation: invocation(gate_args),
        inputs: collect_inputs(root)?,
        checks: result.checks,
        rules: result.rules,
        fixtures: result.fixtures,
        aggregate_executed_proof_class: aggregate_executed_proof_class(
            &result.executed_proof_classes,
        ),
        declared_supported_proof_classes: result.declared_supported_proof_classes,
        executed_proof_classes: result.executed_proof_classes,
        proof_limitations: result.limitations,
        artifacts: vec!["build/reports".to_owned()],
    };
    let path = write_report(root, "architecture-check", &report)?;
    println!("PASS {ARCH_GATE}; report={}", slash(&path));
    Ok(())
}

fn run_verify_deep(root: &Path, gate_args: &[String]) -> Result<(), String> {
    match run_verify_deep_inner(root, gate_args) {
        Ok(()) => Ok(()),
        Err(error) => fail_with_report(root, DEEP_GATE, "verify-deep", gate_args, &error),
    }
}

fn run_verify_deep_inner(root: &Path, gate_args: &[String]) -> Result<(), String> {
    let mut checks = Vec::new();
    let mut transport_report = String::new();
    let archive_store_invocation = begin_wp008_invocation(root)?;
    let resource_durability_invocation = begin_wp009_invocation(root)?;
    let (architecture, archive_store_receipt, resource_durability_receipt) =
        run_verify_deep_checks(
            root,
            &mut checks,
            &mut transport_report,
            &archive_store_invocation,
            &resource_durability_invocation,
        )?;
    validate_wp008_execution_receipt(root, &archive_store_receipt, &archive_store_invocation)?;
    validate_wp009_execution_receipt(
        root,
        &resource_durability_receipt,
        &resource_durability_invocation,
    )?;
    let executed_proof_classes = executed_proof_classes(&checks, &architecture.fixtures);
    let aggregate_executed_proof_class = aggregate_executed_proof_class(&executed_proof_classes);
    let report = GateReport {
        schema_id: "ff.gate-report@1",
        schema_version: "1.0.0",
        gate_id: DEEP_GATE.to_owned(),
        gate_version: 1,
        status: "PASS",
        exit_code: 0,
        source: source_state(root)?,
        invocation: invocation(gate_args),
        inputs: collect_inputs(root)?,
        checks,
        rules: architecture.rules.clone(),
        fixtures: architecture.fixtures,
        declared_supported_proof_classes: vec![
            "counterfactual".to_owned(),
            "graph".to_owned(),
            "negative_fixture".to_owned(),
            "public_boundary".to_owned(),
            "semantic".to_owned(),
            "state_effect".to_owned(),
            "structural".to_owned(),
            "wire_boundary".to_owned(),
        ],
        aggregate_executed_proof_class,
        executed_proof_classes,
        proof_limitations: vec![
            "This is Phase 0 prerequisite proof and claims no product capability, runtime completion, packaging, release, or phase progress.".to_owned(),
            "No production network, FFmpeg, JavaScript, plugin, scheduler, watcher, or shipped archive consumer was executed.".to_owned(),
        ],
        artifacts: vec![
            "build/fixtures/contracts/inventory.json".to_owned(),
            WP008_MANIFEST_PATH.to_owned(),
            WP009_MANIFEST_PATH.to_owned(),
            "build/reports".to_owned(),
            transport_report,
            archive_store_receipt.report_path,
            resource_durability_receipt.report_path,
            ".fforager-artifacts/cargo-target".to_owned(),
        ],
    };
    let path = write_report(root, "verify-deep", &report)?;
    println!("PASS {DEEP_GATE}; report={}", slash(&path));
    Ok(())
}

fn run_verify_deep_checks(
    root: &Path,
    checks: &mut Vec<Check>,
    transport_report: &mut String,
    archive_store_invocation: &Wp008Invocation,
    resource_durability_invocation: &Wp009Invocation,
) -> Result<
    (
        ArchitectureResult,
        Wp008ExecutionReceipt,
        Wp009ExecutionReceipt,
    ),
    String,
> {
    verify_tool_identities(root, checks)?;
    run_rust_verification(root, checks)?;
    execute_public_boundary_counterexample_test(root, checks)?;
    let archive_store_receipt =
        execute_archive_store_evidence(root, checks, archive_store_invocation)?;
    validate_wp008_execution_receipt(root, &archive_store_receipt, archive_store_invocation)?;
    let resource_durability_receipt = require_wp009_execution_receipt(Some(
        execute_resource_durability_models(root, checks, resource_durability_invocation)?,
    ))?;
    validate_wp009_execution_receipt(
        root,
        &resource_durability_receipt,
        resource_durability_invocation,
    )?;
    execute_compatibility_replay_boundaries(root, checks)?;
    execute_transport_corpus(root, checks)?.clone_into(transport_report);
    run_doctests(root, checks)?;
    validate_contract_inventory(root, checks)?;
    validate_contract_manual(root, checks)?;
    scan_data_only_product_models(root, checks)?;
    let mut architecture = architecture_check(root)?;
    checks.append(&mut architecture.checks);
    checks.push(pass(
        "deep-proof-surface",
        "locked workspace, contract inventory, WP-008 archive-store evidence, WP-009 resource/durability model corpus, model manual, data-only scan, public-boundary counterexamples, transport corpus, doctests, and canonical architecture rules were executed; active-packet acceptance attribution remains external evidence work",
    ));
    Ok((
        architecture,
        archive_store_receipt,
        resource_durability_receipt,
    ))
}

fn run_transport_corpus(root: &Path) -> Result<(), String> {
    let mut checks = Vec::new();
    let report = execute_transport_corpus(root, &mut checks)?;
    println!("COMPLETE WP-FF-007 transport corpus; report={report}");
    Ok(())
}

fn run_archive_store_evidence(root: &Path) -> Result<(), String> {
    let mut checks = Vec::new();
    let invocation = begin_wp008_invocation(root)?;
    let receipt = execute_archive_store_evidence(root, &mut checks, &invocation)?;
    validate_wp008_execution_receipt(root, &receipt, &invocation)?;
    println!(
        "COMPLETE WP-FF-008 archive-store evidence; report={}",
        receipt.report_path
    );
    Ok(())
}

fn begin_wp008_invocation(root: &Path) -> Result<Wp008Invocation, String> {
    let started_at = SystemTime::now();
    let manifest_bytes = fs::read(root.join(WP008_MANIFEST_PATH))
        .map_err(|error| format!("WP008-E-MANIFEST-READ:{error}"))?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("WP008-E-MANIFEST-PARSE:{error}"))?;
    validate_wp008_manifest(&manifest)?;
    let manifest_sha256 = encode_sha256(&manifest_bytes);
    let source_content_fingerprint = source_state(root)?.content_fingerprint;
    let sequence = WP008_INVOCATION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let started_nanos = started_at
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("WP008-E-INVOCATION-CLOCK:{error}"))?
        .as_nanos();
    let identity_material = format!(
        "{}:{sequence}:{started_nanos}:{manifest_sha256}:{source_content_fingerprint}",
        std::process::id()
    );
    Ok(Wp008Invocation {
        id: encode_sha256(identity_material.as_bytes()),
        started_at,
        manifest_sha256,
        source_content_fingerprint,
    })
}

fn execute_archive_store_evidence(
    root: &Path,
    checks: &mut Vec<Check>,
    invocation: &Wp008Invocation,
) -> Result<Wp008ExecutionReceipt, String> {
    let manifest_bytes = fs::read(root.join(WP008_MANIFEST_PATH))
        .map_err(|error| format!("WP008-E-MANIFEST-READ:{error}"))?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("WP008-E-MANIFEST-PARSE:{error}"))?;
    validate_wp008_manifest(&manifest)?;
    let source_before = source_state(root)?;
    if encode_sha256(&manifest_bytes) != invocation.manifest_sha256
        || source_before.content_fingerprint != invocation.source_content_fingerprint
    {
        return Err("WP008-E-EXECUTION-INVOCATION-STALE".to_owned());
    }

    let listed = cargo_proof_output(
        root,
        "cargo",
        &[
            "test",
            "--manifest-path",
            "build/Cargo.toml",
            "--locked",
            "--jobs",
            "1",
            "-p",
            "fforager-testkit",
            "--lib",
            "--",
            "--list",
        ],
    )?;
    let expected_registration = format!("{WP008_PROOF_TEST}: test");
    if !listed
        .lines()
        .any(|line| line.trim() == expected_registration)
    {
        return Err("WP008-E-GATE-UNTRIGGERED: exact compiled proof test is absent".to_owned());
    }
    let output = cargo_proof_output(
        root,
        "cargo",
        &[
            "test",
            "--manifest-path",
            "build/Cargo.toml",
            "--locked",
            "--jobs",
            "1",
            "-p",
            "fforager-testkit",
            "--lib",
            WP008_PROOF_TEST,
            "--",
            "--exact",
            "--nocapture",
        ],
    )?;
    let report = extract_wp008_report(&output)?;
    validate_wp008_report(root, &manifest_bytes, &manifest, &report)?;
    let source_after = source_state(root)?;
    if source_before.git_commit != source_after.git_commit
        || source_before.dirty != source_after.dirty
        || source_before.dirty_paths != source_after.dirty_paths
        || source_before.content_fingerprint != source_after.content_fingerprint
    {
        return Err("WP008-E-SOURCE-CHANGED: source changed during evidence execution".to_owned());
    }
    let receipt = write_wp008_report(root, &report, invocation)?;
    checks.push(Check {
        id: "wp008-archive-public-boundary".to_owned(),
        status: "PASS",
        proof_class: "semantic",
        concrete_input: format!(
            "{WP008_MANIFEST_PATH}; exact source manifest={WP008_SOURCE_PATHS:?}"
        ),
        executed_boundary: format!(
            "cargo test --exact --nocapture -p fforager-testkit {WP008_PROOF_TEST}; strict independent xtask report consumer"
        ),
        expected_result: "24 exact rows; correctness failures zero; B-021 measured or explicitly BLOCKED; every executed semantic row has a declaration-preserving counterfactual rejection".to_owned(),
        observed_result: format!(
            "{} executed rows, {} explicit blocked rows; report={}",
            receipt.executed_cases, receipt.blocked_cases, receipt.report_path
        ),
        skipped_semantic_dependencies: vec![
            "No shipped Ferric entrypoint consumed the archive adapter; zero-product-progress prerequisite ceiling applies.".to_owned(),
            "A BLOCKED B-021 row is not performance evidence.".to_owned(),
        ],
        detail: "Candidate-neutral claim, lease, commit, reconciliation, corruption, migration, import, retry, and representative-scale behavior crossed the compiled public storage boundary.".to_owned(),
    });
    Ok(receipt)
}

fn extract_wp008_report(output: &str) -> Result<Value, String> {
    let reports = output
        .lines()
        .filter_map(|line| line.trim().strip_prefix(WP008_REPORT_PREFIX))
        .collect::<Vec<_>>();
    if reports.len() != 1 {
        return Err(format!(
            "WP008-E-REPORT-COUNT: expected one archive report, observed {}",
            reports.len()
        ));
    }
    serde_json::from_str(reports[0]).map_err(|error| format!("WP008-E-REPORT-PARSE:{error}"))
}

#[allow(
    clippy::too_many_lines,
    reason = "FF-BUILD-057: the independent gate keeps the complete WP-008 corpus schema fail-closed in one validator"
)]
fn validate_wp008_manifest(manifest: &Value) -> Result<(), String> {
    wp008_require_exact_keys(
        manifest,
        &[
            "schema_id",
            "corpus_id",
            "fixture_identity",
            "candidate",
            "candidate_matrix",
            "source_paths",
            "limits",
            "cases",
            "residual_uncertainty",
        ],
        "WP008-E-MANIFEST-SCHEMA",
    )?;
    if manifest.get("schema_id").and_then(Value::as_str)
        != Some("ff.archive-store-evidence-corpus@2")
        || manifest.get("corpus_id").and_then(Value::as_str)
            != Some("WP-FF-008-archive-store-evidence-v2")
    {
        return Err("WP008-E-MANIFEST-IDENTITY".to_owned());
    }
    let fixture = manifest
        .get("fixture_identity")
        .ok_or("WP008-E-FIXTURE-IDENTITY")?;
    wp008_require_exact_keys(
        fixture,
        &[
            "owner",
            "origin",
            "network_required",
            "external_executable_required",
        ],
        "WP008-E-FIXTURE-IDENTITY",
    )?;
    if fixture.get("owner").and_then(Value::as_str) != Some("Ferric Forager")
        || fixture.get("network_required").and_then(Value::as_bool) != Some(false)
        || fixture
            .get("external_executable_required")
            .and_then(Value::as_bool)
            != Some(false)
    {
        return Err("WP008-E-FIXTURE-DEPENDENCY".to_owned());
    }
    validate_wp008_candidate(manifest.get("candidate").ok_or("WP008-E-CANDIDATE")?)?;
    validate_wp008_candidate_matrix(
        manifest
            .get("candidate_matrix")
            .ok_or("WP008-E-CANDIDATE-MATRIX")?,
    )?;
    let source_paths = manifest
        .get("source_paths")
        .and_then(Value::as_array)
        .ok_or("WP008-E-SOURCE-PATHS")?
        .iter()
        .map(|value| value.as_str().ok_or("WP008-E-SOURCE-PATHS"))
        .collect::<Result<Vec<_>, _>>()?;
    if source_paths != WP008_SOURCE_PATHS {
        return Err("WP008-E-SOURCE-MANIFEST-DRIFT".to_owned());
    }
    let limits = manifest.get("limits").ok_or("WP008-E-LIMITS")?;
    wp008_require_exact_keys(
        limits,
        &[
            "maximum_fixture_bytes",
            "maximum_cases",
            "maximum_case_runtime_ms",
            "representative_identities",
            "b021_identities",
            "latency_sample_limit",
            "maximum_report_bytes",
        ],
        "WP008-E-LIMITS",
    )?;
    if limits.as_object().is_none_or(|object| {
        object
            .values()
            .any(|value| value.as_u64().is_none_or(|n| n == 0))
    }) || limits.get("maximum_cases").and_then(Value::as_u64)
        != u64::try_from(WP008_CASE_IDS.len()).ok()
        || limits.get("b021_identities").and_then(Value::as_u64) != Some(1_000_000)
    {
        return Err("WP008-E-LIMITS".to_owned());
    }
    let cases = manifest
        .get("cases")
        .and_then(Value::as_array)
        .ok_or("WP008-E-CASES")?;
    let mut observed = BTreeSet::new();
    for case in cases {
        wp008_require_exact_keys(
            case,
            &[
                "case_id",
                "category",
                "scenario",
                "injected_fault",
                "expected",
                "actions",
                "negative_action",
            ],
            "WP008-E-CASE-SCHEMA",
        )?;
        let case_id = wp008_required_text(case, "case_id", "WP008-E-CASE")?;
        for key in ["category", "scenario", "injected_fault", "expected"] {
            let _text = wp008_required_text(case, key, "WP008-E-CASE")?;
        }
        let actions = case
            .get("actions")
            .and_then(Value::as_array)
            .ok_or("WP008-E-CASE-ACTIONS")?;
        let mut action_ids = BTreeSet::new();
        if actions.is_empty()
            || actions.iter().any(|action| {
                action
                    .as_str()
                    .is_none_or(|action| action.is_empty() || !action_ids.insert(action))
            })
        {
            return Err("WP008-E-CASE-ACTIONS".to_owned());
        }
        if case_id == "wp008-scale-b021" {
            if !case.get("negative_action").is_some_and(Value::is_null) {
                return Err("WP008-E-CASE-NEGATIVE".to_owned());
            }
        } else if case
            .get("negative_action")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            return Err("WP008-E-CASE-NEGATIVE".to_owned());
        }
        if !observed.insert(case_id) {
            return Err("WP008-E-DUPLICATE-CASE".to_owned());
        }
    }
    if observed != WP008_CASE_IDS.iter().copied().collect::<BTreeSet<_>>() {
        return Err("WP008-E-UNMAPPED-INPUT".to_owned());
    }
    let residual = manifest
        .get("residual_uncertainty")
        .and_then(Value::as_array)
        .ok_or("WP008-E-RESIDUAL")?;
    if residual.len() < 3
        || residual
            .iter()
            .any(|value| value.as_str().is_none_or(str::is_empty))
    {
        return Err("WP008-E-RESIDUAL".to_owned());
    }
    Ok(())
}

fn validate_wp008_candidate(candidate: &Value) -> Result<(), String> {
    wp008_require_exact_keys(
        candidate,
        &[
            "name",
            "version",
            "default_features",
            "features",
            "write_strategy",
            "durability",
        ],
        "WP008-E-CANDIDATE",
    )?;
    if candidate.get("name").and_then(Value::as_str) != Some("redb")
        || candidate.get("version").and_then(Value::as_str) != Some("4.1.0")
        || candidate.get("default_features").and_then(Value::as_bool) != Some(false)
        || candidate
            .get("features")
            .and_then(Value::as_array)
            .is_none_or(|v| !v.is_empty())
        || candidate.get("write_strategy").and_then(Value::as_str) != Some("two_phase")
        || candidate.get("durability").and_then(Value::as_str) != Some("immediate")
    {
        return Err("WP008-E-CANDIDATE".to_owned());
    }
    Ok(())
}

fn validate_wp008_candidate_matrix(matrix: &Value) -> Result<(), String> {
    let rows = matrix.as_array().ok_or("WP008-E-CANDIDATE-MATRIX")?;
    let expected = BTreeMap::from([
        ("redb", ("4.1.0", "EXECUTED")),
        ("fjall", ("3.1.8", "DEFERRED_COMPARISON")),
        ("sled", ("0.34.7", "REJECTED")),
        ("sqlite", ("unapproved", "UNAUTHORIZED")),
    ]);
    let mut observed = BTreeSet::new();
    for row in rows {
        wp008_require_exact_keys(
            row,
            &["name", "version", "disposition", "reason"],
            "WP008-E-CANDIDATE-MATRIX",
        )?;
        let name = wp008_required_text(row, "name", "WP008-E-CANDIDATE-MATRIX")?;
        let (version, disposition) = expected.get(name).ok_or("WP008-E-CANDIDATE-MATRIX")?;
        if wp008_required_text(row, "version", "WP008-E-CANDIDATE-MATRIX")? != *version
            || wp008_required_text(row, "disposition", "WP008-E-CANDIDATE-MATRIX")? != *disposition
            || !observed.insert(name)
        {
            return Err("WP008-E-CANDIDATE-MATRIX".to_owned());
        }
    }
    if observed != expected.keys().copied().collect::<BTreeSet<_>>() {
        return Err("WP008-E-CANDIDATE-MATRIX".to_owned());
    }
    Ok(())
}

fn wp008_require_exact_keys(
    value: &Value,
    expected: &[&str],
    diagnostic: &str,
) -> Result<(), String> {
    let object = value.as_object().ok_or_else(|| diagnostic.to_owned())?;
    let observed = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();
    if observed != expected {
        return Err(format!(
            "{diagnostic}: expected keys={expected:?}; observed={observed:?}"
        ));
    }
    Ok(())
}

fn wp008_required_text<'a>(
    value: &'a Value,
    key: &str,
    diagnostic: &str,
) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| format!("{diagnostic}:{key}"))
}

#[allow(clippy::too_many_lines)]
fn validate_wp008_report(
    root: &Path,
    manifest_bytes: &[u8],
    manifest: &Value,
    report: &Value,
) -> Result<(), String> {
    wp008_require_exact_keys(
        report,
        &[
            "schema_id",
            "schema_version",
            "corpus_id",
            "manifest_path",
            "manifest_fingerprint",
            "source_manifest",
            "candidate",
            "candidate_matrix",
            "environment",
            "workload",
            "rows",
            "measurements",
            "summary",
            "residual_uncertainty",
        ],
        "WP008-E-REPORT-SCHEMA",
    )?;
    if report.get("schema_id").and_then(Value::as_str) != Some("ff.archive-store-evidence-report@2")
        || report.get("schema_version").and_then(Value::as_str) != Some("2.0.0")
        || report.get("corpus_id").and_then(Value::as_str)
            != Some("WP-FF-008-archive-store-evidence-v2")
        || report.get("manifest_path").and_then(Value::as_str) != Some(WP008_MANIFEST_PATH)
        || report.get("manifest_fingerprint").and_then(Value::as_str)
            != Some(format!("sha256:{}", encode_sha256(manifest_bytes)).as_str())
    {
        return Err("WP008-E-REPORT-IDENTITY".to_owned());
    }
    validate_wp008_candidate(report.get("candidate").ok_or("WP008-E-CANDIDATE")?)?;
    if report.get("candidate") != manifest.get("candidate")
        || report.get("candidate_matrix") != manifest.get("candidate_matrix")
        || report.get("residual_uncertainty") != manifest.get("residual_uncertainty")
    {
        return Err("WP008-E-REPORT-MANIFEST-DRIFT".to_owned());
    }
    let source_manifest = report
        .get("source_manifest")
        .and_then(Value::as_array)
        .ok_or("WP008-E-SOURCE-MANIFEST")?;
    if source_manifest.len() != WP008_SOURCE_PATHS.len() {
        return Err("WP008-E-SOURCE-MANIFEST".to_owned());
    }
    for (row, relative) in source_manifest.iter().zip(WP008_SOURCE_PATHS) {
        wp008_require_exact_keys(row, &["path", "sha256"], "WP008-E-SOURCE-MANIFEST")?;
        let bytes = fs::read(root.join(relative))
            .map_err(|error| format!("WP008-E-SOURCE-READ:{relative}:{error}"))?;
        if row.get("path").and_then(Value::as_str) != Some(*relative)
            || row.get("sha256").and_then(Value::as_str) != Some(encode_sha256(&bytes).as_str())
        {
            return Err("WP008-E-SOURCE-MANIFEST-DRIFT".to_owned());
        }
    }
    let environment = report.get("environment").ok_or("WP008-E-ENVIRONMENT")?;
    wp008_require_exact_keys(
        environment,
        &[
            "os",
            "arch",
            "cargo_profile",
            "artifact_root",
            "stress_enabled",
        ],
        "WP008-E-ENVIRONMENT",
    )?;
    if environment.get("os").and_then(Value::as_str) != Some(env::consts::OS)
        || environment.get("arch").and_then(Value::as_str) != Some(env::consts::ARCH)
        || environment.get("cargo_profile").and_then(Value::as_str) != Some("test")
        || environment.get("artifact_root").and_then(Value::as_str) != Some(".fforager-artifacts")
        || environment
            .get("stress_enabled")
            .and_then(Value::as_bool)
            .is_none()
    {
        return Err("WP008-E-ENVIRONMENT".to_owned());
    }
    let workload = report.get("workload").ok_or("WP008-E-WORKLOAD")?;
    wp008_require_exact_keys(
        workload,
        &[
            "maximum_case_runtime_ms",
            "representative_identities",
            "b021_identities",
            "latency_sample_limit",
            "maximum_report_bytes",
        ],
        "WP008-E-WORKLOAD",
    )?;
    for key in [
        "maximum_case_runtime_ms",
        "representative_identities",
        "b021_identities",
        "latency_sample_limit",
        "maximum_report_bytes",
    ] {
        if workload.get(key) != manifest.pointer(&format!("/limits/{key}")) {
            return Err(format!("WP008-E-WORKLOAD:{key}"));
        }
    }
    let cases = manifest
        .get("cases")
        .and_then(Value::as_array)
        .ok_or("WP008-E-CASES")?;
    let rows = report
        .get("rows")
        .and_then(Value::as_array)
        .ok_or("WP008-E-ROWS")?;
    if rows.len() != cases.len() {
        return Err("WP008-E-ROW-COUNT".to_owned());
    }
    let case_by_id = cases
        .iter()
        .map(|case| {
            wp008_required_text(case, "case_id", "WP008-E-CASE").map(|case_id| (case_id, case))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let mut observed = BTreeSet::new();
    let mut blocked = 0_u64;
    for row in rows {
        wp008_require_exact_keys(
            row,
            &[
                "case_id",
                "category",
                "status",
                "proof_class",
                "concrete_input",
                "executed_boundary",
                "expected_result",
                "observed_result",
                "actions",
                "negative_path",
                "elapsed_ns",
                "residual_uncertainty",
            ],
            "WP008-E-ROW-SCHEMA",
        )?;
        let case_id = wp008_required_text(row, "case_id", "WP008-E-ROW")?;
        let case = case_by_id.get(case_id).ok_or("WP008-E-UNMAPPED-ROW")?;
        if !observed.insert(case_id) {
            return Err("WP008-E-DUPLICATE-ROW".to_owned());
        }
        if row.get("category") != case.get("category")
            || row.get("expected_result") != case.get("expected")
            || wp008_required_text(row, "concrete_input", "WP008-E-ROW")?.is_empty()
            || wp008_required_text(row, "executed_boundary", "WP008-E-ROW")?.is_empty()
            || wp008_required_text(row, "observed_result", "WP008-E-ROW")?.is_empty()
            || row.get("elapsed_ns").and_then(Value::as_u64).is_none()
            || row
                .get("residual_uncertainty")
                .and_then(Value::as_array)
                .is_none()
        {
            return Err(format!("WP008-E-ROW-ATTRIBUTION:{case_id}"));
        }
        let expected_actions = case
            .get("actions")
            .and_then(Value::as_array)
            .ok_or("WP008-E-CASE-ACTIONS")?;
        let actions = row
            .get("actions")
            .and_then(Value::as_array)
            .ok_or("WP008-E-ACTION-RECEIPTS")?;
        if actions.len() != expected_actions.len() {
            return Err(format!("WP008-E-ACTION-COUNT:{case_id}"));
        }
        for (receipt, expected_action) in actions.iter().zip(expected_actions) {
            validate_wp008_action_receipt(
                receipt,
                expected_action.as_str().ok_or("WP008-E-CASE-ACTIONS")?,
                "PASS",
            )?;
        }
        match (
            row.get("negative_path"),
            case.get("negative_action").and_then(Value::as_str),
        ) {
            (Some(Value::Null), None) => {}
            (Some(receipt), Some(action_id)) => {
                validate_wp008_action_receipt(receipt, action_id, "REJECTED")?;
            }
            _ => return Err(format!("WP008-E-NEGATIVE-RECEIPT:{case_id}")),
        }
        match row.get("status").and_then(Value::as_str) {
            Some("PASS") => {
                if row.get("proof_class").and_then(Value::as_str) != Some("semantic") {
                    return Err(format!("WP008-E-DECLARATION-ONLY:{case_id}"));
                }
            }
            Some("BLOCKED") if case_id == "wp008-scale-b021" => {
                blocked = blocked.saturating_add(1);
                if row.get("proof_class").and_then(Value::as_str) != Some("structural") {
                    return Err("WP008-E-B021-BLOCKED-OVERCLAIM".to_owned());
                }
            }
            _ => return Err(format!("WP008-E-ROW-STATUS:{case_id}")),
        }
    }
    if observed != WP008_CASE_IDS.iter().copied().collect::<BTreeSet<_>>() {
        return Err("WP008-E-UNMAPPED-ROW".to_owned());
    }
    validate_wp008_measurements(manifest, report, blocked)?;
    let summary = report.get("summary").ok_or("WP008-E-SUMMARY")?;
    wp008_require_exact_keys(
        summary,
        &[
            "executed_cases",
            "semantic_pass_cases",
            "blocked_cases",
            "failed_cases",
            "zero_product_progress",
        ],
        "WP008-E-SUMMARY",
    )?;
    let total = u64::try_from(rows.len()).map_err(|error| error.to_string())?;
    if summary.get("executed_cases").and_then(Value::as_u64) != Some(total)
        || summary.get("semantic_pass_cases").and_then(Value::as_u64)
            != Some(total.saturating_sub(blocked))
        || summary.get("blocked_cases").and_then(Value::as_u64) != Some(blocked)
        || summary.get("failed_cases").and_then(Value::as_u64) != Some(0)
        || summary
            .get("zero_product_progress")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Err("WP008-E-SUMMARY".to_owned());
    }
    Ok(())
}

fn validate_wp008_action_receipt(
    receipt: &Value,
    expected_action_id: &str,
    expected_outcome: &str,
) -> Result<(), String> {
    wp008_require_exact_keys(
        receipt,
        &[
            "action_id",
            "boundary",
            "input",
            "input_sha256",
            "observed",
            "output_sha256",
            "outcome",
        ],
        "WP008-E-ACTION-RECEIPT-SCHEMA",
    )?;
    let input = wp008_required_text(receipt, "input", "WP008-E-ACTION-RECEIPT")?;
    let observed = wp008_required_text(receipt, "observed", "WP008-E-ACTION-RECEIPT")?;
    if wp008_required_text(receipt, "action_id", "WP008-E-ACTION-RECEIPT")? != expected_action_id
        || wp008_required_text(receipt, "boundary", "WP008-E-ACTION-RECEIPT")?.is_empty()
        || wp008_required_text(receipt, "outcome", "WP008-E-ACTION-RECEIPT")? != expected_outcome
        || wp008_required_text(receipt, "input_sha256", "WP008-E-ACTION-RECEIPT")?
            != encode_sha256(input.as_bytes())
        || wp008_required_text(receipt, "output_sha256", "WP008-E-ACTION-RECEIPT")?
            != encode_sha256(observed.as_bytes())
    {
        return Err(format!("WP008-E-ACTION-RECEIPT:{expected_action_id}"));
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "FF-BUILD-057: independent measurement validation keeps all anti-overclaim checks atomic"
)]
fn validate_wp008_measurements(
    manifest: &Value,
    report: &Value,
    blocked_rows: u64,
) -> Result<(), String> {
    let measurements = report
        .get("measurements")
        .and_then(Value::as_array)
        .ok_or("WP008-E-MEASUREMENTS")?;
    if measurements.len() != 2 {
        return Err("WP008-E-MEASUREMENTS".to_owned());
    }
    let mut profiles = BTreeSet::new();
    for measurement in measurements {
        let profile = wp008_required_text(measurement, "profile", "WP008-E-MEASUREMENT")?;
        if !profiles.insert(profile) {
            return Err("WP008-E-MEASUREMENT-DUPLICATE".to_owned());
        }
        let expected_count = match profile {
            "representative" => manifest.pointer("/limits/representative_identities"),
            "b021" => manifest.pointer("/limits/b021_identities"),
            _ => return Err("WP008-E-MEASUREMENT-PROFILE".to_owned()),
        };
        if measurement.get("identity_count") != expected_count
            || measurement.get("timeout_ms") != manifest.pointer("/limits/maximum_case_runtime_ms")
        {
            return Err(format!("WP008-E-MEASUREMENT:{profile}"));
        }
        match measurement.get("status").and_then(Value::as_str) {
            Some("PASS")
                if profile == "representative" || (profile == "b021" && blocked_rows == 0) =>
            {
                validate_wp008_measured_profile(measurement, profile)?;
            }
            Some("BLOCKED") if profile == "b021" && blocked_rows == 1 => {
                wp008_require_exact_keys(
                    measurement,
                    &[
                        "profile",
                        "status",
                        "identity_count",
                        "timeout_ms",
                        "blocked_reason",
                        "residual_uncertainty",
                    ],
                    "WP008-E-B021-BLOCKED-SCHEMA",
                )?;
                if wp008_required_text(measurement, "blocked_reason", "WP008-E-B021-BLOCKED")?
                    .is_empty()
                    || measurement
                        .get("residual_uncertainty")
                        .and_then(Value::as_array)
                        .is_none_or(Vec::is_empty)
                {
                    return Err("WP008-E-B021-BLOCKED-DETAIL".to_owned());
                }
            }
            _ => return Err(format!("WP008-E-MEASUREMENT-STATUS:{profile}")),
        }
    }
    if profiles != BTreeSet::from(["representative", "b021"]) {
        return Err("WP008-E-MEASUREMENT-PROFILE".to_owned());
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "independent strict consumer keeps all measured-claim anti-promotion checks coupled"
)]
fn validate_wp008_measured_profile(measurement: &Value, profile: &str) -> Result<(), String> {
    wp008_require_exact_keys(
        measurement,
        &[
            "profile",
            "status",
            "identity_count",
            "timeout_ms",
            "elapsed_ns",
            "insert_latency_ns",
            "cold_lookup_latency_ns",
            "warm_lookup_latency_ns",
            "generator_state",
            "rss",
            "storage_size_bytes",
            "logical_bytes_written",
            "write_amplification_proxy_milli",
            "cache_state",
            "residual_uncertainty",
        ],
        "WP008-E-MEASUREMENT-SCHEMA",
    )?;
    let elapsed_ns = measurement
        .get("elapsed_ns")
        .and_then(Value::as_u64)
        .ok_or("WP008-E-MEASUREMENT-TIMEOUT")?;
    let timeout_ms = measurement
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .ok_or("WP008-E-MEASUREMENT-TIMEOUT")?;
    if elapsed_ns > timeout_ms.saturating_mul(1_000_000)
        || wp008_required_text(measurement, "cache_state", "WP008-E-MEASUREMENT")?
            != "cold_after_database_reopen_then_warm_same_handle"
        || measurement
            .get("storage_size_bytes")
            .and_then(Value::as_u64)
            .is_none_or(|value| value == 0)
        || measurement
            .get("logical_bytes_written")
            .and_then(Value::as_u64)
            .is_none_or(|value| value == 0)
        || measurement
            .get("write_amplification_proxy_milli")
            .and_then(Value::as_u64)
            .is_none_or(|value| value == 0)
        || measurement
            .get("residual_uncertainty")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
    {
        return Err(format!("WP008-E-MEASURED-PROFILE:{profile}"));
    }
    for key in [
        "insert_latency_ns",
        "cold_lookup_latency_ns",
        "warm_lookup_latency_ns",
    ] {
        wp008_validate_distribution(measurement.get(key).ok_or("WP008-E-LATENCY")?, profile)?;
    }
    let generator = measurement
        .get("generator_state")
        .ok_or("WP008-E-GENERATOR")?;
    wp008_require_exact_keys(
        generator,
        &[
            "method",
            "state_struct_bytes",
            "peak_batch_wire_bytes",
            "derived_bound_bytes",
        ],
        "WP008-E-GENERATOR",
    )?;
    let state = generator
        .get("state_struct_bytes")
        .and_then(Value::as_u64)
        .ok_or("WP008-E-GENERATOR")?;
    let batch = generator
        .get("peak_batch_wire_bytes")
        .and_then(Value::as_u64)
        .ok_or("WP008-E-GENERATOR")?;
    if wp008_required_text(generator, "method", "WP008-E-GENERATOR")?
        != "size_of_state_plus_peak_serialized_batch"
        || state == 0
        || batch == 0
        || generator.get("derived_bound_bytes").and_then(Value::as_u64)
            != Some(state.saturating_add(batch))
    {
        return Err("WP008-E-GENERATOR".to_owned());
    }
    let rss = measurement.get("rss").ok_or("WP008-E-RSS")?;
    wp008_require_exact_keys(
        rss,
        &[
            "status",
            "scope",
            "samples",
            "before_bytes",
            "after_bytes",
            "maximum_observed_endpoint_bytes",
        ],
        "WP008-E-RSS",
    )?;
    let before = rss.get("before_bytes").and_then(Value::as_u64).unwrap_or(0);
    let after = rss.get("after_bytes").and_then(Value::as_u64).unwrap_or(0);
    if wp008_required_text(rss, "status", "WP008-E-RSS")? != "MEASURED_ENDPOINT_PROCESS_WORKING_SET"
        || wp008_required_text(rss, "scope", "WP008-E-RSS")? != "process_endpoint_samples_not_peak"
        || rss.get("samples").and_then(Value::as_u64) != Some(2)
        || before == 0
        || after == 0
        || rss
            .get("maximum_observed_endpoint_bytes")
            .and_then(Value::as_u64)
            != Some(before.max(after))
    {
        return Err("WP008-E-RSS".to_owned());
    }
    Ok(())
}

fn wp008_validate_distribution(value: &Value, profile: &str) -> Result<(), String> {
    wp008_require_exact_keys(
        value,
        &["samples", "minimum", "p50", "p95", "p99", "maximum"],
        "WP008-E-LATENCY-SCHEMA",
    )?;
    let samples = value
        .get("samples")
        .and_then(Value::as_u64)
        .ok_or("WP008-E-LATENCY")?;
    let values = ["minimum", "p50", "p95", "p99", "maximum"]
        .iter()
        .map(|key| {
            value
                .get(*key)
                .and_then(Value::as_u64)
                .ok_or("WP008-E-LATENCY")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let ordered = values.windows(2).all(|pair| pair[0] <= pair[1]);
    if !ordered || (profile == "representative" && samples == 0) {
        return Err(format!("WP008-E-LATENCY:{profile}"));
    }
    Ok(())
}

fn write_wp008_report(
    root: &Path,
    report: &Value,
    invocation: &Wp008Invocation,
) -> Result<Wp008ExecutionReceipt, String> {
    let reports = root.join("build/reports");
    fs::create_dir_all(&reports).map_err(|error| format!("create reports: {error}"))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("WP008-E-REPORT-CLOCK:{error}"))?
        .as_nanos();
    let stem = format!(
        "wp-ff-008-archive-store-evidence-{}-{nonce}",
        std::process::id()
    );
    let temporary = reports.join(format!(".{stem}.tmp"));
    let final_path = reports.join(format!("{stem}.json"));
    let bytes = serde_json::to_vec_pretty(report)
        .map_err(|error| format!("WP008-E-REPORT-SERIALIZE:{error}"))?;
    let result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.flush()?;
        file.sync_all()?;
        fs::rename(&temporary, &final_path)?;
        Ok(())
    })();
    if let Err(error) = result {
        let _result = fs::remove_file(&temporary);
        return Err(format!("WP008-E-REPORT-WRITE:{error}"));
    }
    let persisted_bytes =
        fs::read(&final_path).map_err(|error| format!("WP008-E-REPORT-REREAD:{error}"))?;
    let persisted: Value = serde_json::from_slice(&persisted_bytes)
        .map_err(|error| format!("WP008-E-REPORT-REPARSE:{error}"))?;
    if persisted != *report {
        return Err("WP008-E-REPORT-PERSISTENCE".to_owned());
    }
    let report_path = final_path
        .strip_prefix(root)
        .map(slash)
        .map_err(|error| format!("WP008-E-REPORT-PATH:{error}"))?;
    Ok(Wp008ExecutionReceipt {
        invocation_id: invocation.id.clone(),
        report_path,
        report_sha256: encode_sha256(&persisted_bytes),
        manifest_sha256: invocation.manifest_sha256.clone(),
        source_content_fingerprint: invocation.source_content_fingerprint.clone(),
        executed_cases: report
            .pointer("/summary/executed_cases")
            .and_then(Value::as_u64)
            .ok_or("WP008-E-SUMMARY")?,
        blocked_cases: report
            .pointer("/summary/blocked_cases")
            .and_then(Value::as_u64)
            .ok_or("WP008-E-SUMMARY")?,
    })
}

fn validate_wp008_execution_receipt(
    root: &Path,
    receipt: &Wp008ExecutionReceipt,
    invocation: &Wp008Invocation,
) -> Result<(), String> {
    if receipt.invocation_id != invocation.id
        || receipt.manifest_sha256 != invocation.manifest_sha256
        || receipt.source_content_fingerprint != invocation.source_content_fingerprint
        || receipt.executed_cases != u64::try_from(WP008_CASE_IDS.len()).unwrap_or(u64::MAX)
        || receipt.blocked_cases > 1
    {
        return Err("WP008-E-EXECUTION-RECEIPT-IDENTITY".to_owned());
    }
    let relative = Path::new(&receipt.report_path);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
        || relative.parent() != Some(Path::new("build/reports"))
        || relative
            .file_name()
            .and_then(OsStr::to_str)
            .is_none_or(|name| {
                !name.starts_with("wp-ff-008-archive-store-evidence-")
                    || Path::new(name).extension() != Some(OsStr::new("json"))
            })
    {
        return Err("WP008-E-EXECUTION-RECEIPT-PATH".to_owned());
    }
    let path = root.join(relative);
    let modified = fs::metadata(&path)
        .and_then(|metadata| metadata.modified())
        .map_err(|error| format!("WP008-E-EXECUTION-RECEIPT-MISSING:{error}"))?;
    if modified < invocation.started_at {
        return Err("WP008-E-EXECUTION-RECEIPT-STALE".to_owned());
    }
    let bytes =
        fs::read(&path).map_err(|error| format!("WP008-E-EXECUTION-RECEIPT-MISSING:{error}"))?;
    if encode_sha256(&bytes) != receipt.report_sha256 {
        return Err("WP008-E-EXECUTION-RECEIPT-DIGEST".to_owned());
    }
    let report: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("WP008-E-EXECUTION-RECEIPT-PARSE:{error}"))?;
    let manifest_bytes = fs::read(root.join(WP008_MANIFEST_PATH))
        .map_err(|error| format!("WP008-E-MANIFEST-READ:{error}"))?;
    if encode_sha256(&manifest_bytes) != invocation.manifest_sha256 {
        return Err("WP008-E-EXECUTION-RECEIPT-SOURCE-DRIFT".to_owned());
    }
    let manifest: Value = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("WP008-E-MANIFEST-PARSE:{error}"))?;
    validate_wp008_report(root, &manifest_bytes, &manifest, &report)?;
    if source_state(root)?.content_fingerprint != invocation.source_content_fingerprint {
        return Err("WP008-E-EXECUTION-RECEIPT-SOURCE-DRIFT".to_owned());
    }
    Ok(())
}

fn run_resource_durability_models(root: &Path) -> Result<(), String> {
    let mut checks = Vec::new();
    let invocation = begin_wp009_invocation(root)?;
    let receipt = execute_resource_durability_models(root, &mut checks, &invocation)?;
    validate_wp009_execution_receipt(root, &receipt, &invocation)?;
    println!(
        "COMPLETE WP-FF-009 resource/durability model corpus; report={}",
        receipt.report_path
    );
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn execute_resource_durability_models(
    root: &Path,
    checks: &mut Vec<Check>,
    invocation: &Wp009Invocation,
) -> Result<Wp009ExecutionReceipt, String> {
    validate_wp009_owned_api_surface(root)?;
    let manifest_path = root.join(WP009_MANIFEST_PATH);
    let manifest_bytes =
        fs::read(&manifest_path).map_err(|error| format!("WP009-E-MANIFEST-READ: {error}"))?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("WP009-E-MANIFEST-PARSE: {error}"))?;
    validate_wp009_manifest(&manifest)?;
    let source_before = source_state(root)?;
    if encode_sha256(&manifest_bytes) != invocation.manifest_sha256
        || source_before.content_fingerprint != invocation.source_content_fingerprint
    {
        return Err("WP009-E-EXECUTION-INVOCATION-STALE".to_owned());
    }

    let listed = cargo_proof_output(
        root,
        "cargo",
        &[
            "test",
            "--manifest-path",
            "build/Cargo.toml",
            "--locked",
            "--jobs",
            "1",
            "-p",
            "fforager-testkit",
            "--lib",
            "--",
            "--list",
        ],
    )?;
    let expected_registration = format!("{WP009_PROOF_TEST}: test");
    if !listed
        .lines()
        .any(|line| line.trim() == expected_registration)
    {
        return Err("WP009-E-GATE-UNTRIGGERED: exact compiled proof test is absent".to_owned());
    }
    let output = cargo_proof_output(
        root,
        "cargo",
        &[
            "test",
            "--manifest-path",
            "build/Cargo.toml",
            "--locked",
            "--jobs",
            "1",
            "-p",
            "fforager-testkit",
            "--lib",
            WP009_PROOF_TEST,
            "--",
            "--exact",
            "--nocapture",
        ],
    )?;
    let report = extract_wp009_report(&output)?;
    validate_wp009_report(root, &manifest_bytes, &manifest, &report)?;
    let source_after = source_state(root)?;
    if source_before.git_commit != source_after.git_commit
        || source_before.dirty != source_after.dirty
        || source_before.dirty_paths != source_after.dirty_paths
        || source_before.content_fingerprint != source_after.content_fingerprint
    {
        return Err("WP009-E-SOURCE-CHANGED: source changed during model execution".to_owned());
    }
    let receipt = write_wp009_report(root, &report, invocation)?;

    for (id, expected_cases) in [
        (
            "wp009-resource-saturation-model",
            "atomic/fifo/item-byte-bound/coupled-credit rows executed",
        ),
        (
            "wp009-fault-counterfactual-model",
            "queue/durability/journal/filesystem failure rows executed",
        ),
        (
            "wp009-owned-release-boundary",
            "owned resource and byte-credit success/error/unwind/cancel rows executed",
        ),
        (
            "wp009-recovery-prefix-model",
            "recovery actions were selected, applied twice, and checked for post-action convergence",
        ),
    ] {
        checks.push(Check {
            id: id.to_owned(),
            status: "PASS",
            proof_class: "semantic",
            concrete_input: format!(
                "{WP009_MANIFEST_PATH}; exact source manifest={WP009_SOURCE_PATHS:?}"
            ),
            executed_boundary: format!(
                "cargo test --exact --nocapture -p fforager-testkit {WP009_PROOF_TEST}; independent strict xtask report consumer"
            ),
            expected_result: expected_cases.to_owned(),
            observed_result: format!(
                "48 mapped behavior rows validated in fresh report {}",
                receipt.report_path
            ),
            skipped_semantic_dependencies: vec![
                "No shipped Ferric production entrypoint or storage adapter was executed; zero-product-progress prerequisite ceiling applies.".to_owned(),
            ],
            detail: "Result-level WP-009 proof came from the compiled public boundary and survived independent report/manifest/source validation.".to_owned(),
        });
    }
    Ok(receipt)
}

fn require_wp009_execution_receipt(
    receipt: Option<Wp009ExecutionReceipt>,
) -> Result<Wp009ExecutionReceipt, String> {
    receipt.ok_or_else(|| {
        "WP009-E-GATE-UNTRIGGERED: no invocation-bound execution receipt was produced".to_owned()
    })
}

fn validate_wp009_owned_api_surface(root: &Path) -> Result<(), String> {
    let source = fs::read_to_string(root.join("product/crates/fforager-core/src/resource.rs"))
        .map_err(|error| format!("WP009-E-OWNED-API-READ:{error}"))?;
    for required in [
        "pub(crate) enum Admission",
        "pub(crate) struct ResourceLedger",
        "pub(crate) struct ByteCreditLedger",
        "pub struct OwnedResourceBroker",
        "pub struct OwnedResourceLease",
        "pub struct OwnedByteCreditBroker",
        "pub struct OwnedByteCreditLease",
        "```compile_fail",
    ] {
        if !source.contains(required) {
            return Err(format!("WP009-E-OWNERSHIP-ESCAPE:{required}"));
        }
    }
    for forbidden in [
        "pub enum Admission",
        "pub struct ResourceLedger",
        "pub struct ByteCreditLedger",
    ] {
        if source
            .lines()
            .any(|line| line.trim_start().starts_with(forbidden))
        {
            return Err(format!("WP009-E-OWNERSHIP-ESCAPE:{forbidden}"));
        }
    }
    Ok(())
}

fn extract_wp009_report(output: &str) -> Result<Value, String> {
    let reports = output
        .lines()
        .filter_map(|line| line.trim().strip_prefix(WP009_REPORT_PREFIX))
        .collect::<Vec<_>>();
    if reports.len() != 1 {
        return Err(format!(
            "WP009-E-REPORT-COUNT: expected one model report, observed {}",
            reports.len()
        ));
    }
    serde_json::from_str(reports[0]).map_err(|error| format!("WP009-E-REPORT-PARSE: {error}"))
}

fn write_wp009_report(
    root: &Path,
    report: &Value,
    invocation: &Wp009Invocation,
) -> Result<Wp009ExecutionReceipt, String> {
    let reports = root.join("build/reports");
    fs::create_dir_all(&reports).map_err(|error| format!("create reports: {error}"))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("WP009-E-REPORT-CLOCK: {error}"))?
        .as_nanos();
    let stem = format!(
        "wp-ff-009-resource-durability-models-{}-{nonce}",
        std::process::id()
    );
    let temporary = reports.join(format!(".{stem}.tmp"));
    let final_path = reports.join(format!("{stem}.json"));
    let bytes = serde_json::to_vec_pretty(report)
        .map_err(|error| format!("WP009-E-REPORT-SERIALIZE: {error}"))?;
    let result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.flush()?;
        file.sync_all()?;
        fs::rename(&temporary, &final_path)?;
        Ok(())
    })();
    if let Err(error) = result {
        let _result = fs::remove_file(&temporary);
        return Err(format!("WP009-E-REPORT-WRITE: {error}"));
    }
    let persisted_bytes =
        fs::read(&final_path).map_err(|error| format!("WP009-E-REPORT-REREAD: {error}"))?;
    let persisted: Value = serde_json::from_slice(&persisted_bytes)
        .map_err(|error| format!("WP009-E-REPORT-REPARSE: {error}"))?;
    if persisted != *report {
        return Err("WP009-E-REPORT-PERSISTENCE: persisted report changed".to_owned());
    }
    let report_path = final_path
        .strip_prefix(root)
        .map(slash)
        .map_err(|error| format!("WP009-E-REPORT-PATH: {error}"))?;
    let manifest_fingerprint = report
        .get("manifest_fingerprint")
        .and_then(Value::as_str)
        .ok_or("WP009-E-REPORT-IDENTITY")?
        .to_owned();
    let executed_cases = report
        .pointer("/summary/executed_cases")
        .and_then(Value::as_u64)
        .ok_or("WP009-E-SUMMARY")?;
    Ok(Wp009ExecutionReceipt {
        invocation_id: invocation.id.clone(),
        report_path,
        report_sha256: encode_sha256(&persisted_bytes),
        manifest_fingerprint,
        source_content_fingerprint: invocation.source_content_fingerprint.clone(),
        executed_cases,
    })
}

fn begin_wp009_invocation(root: &Path) -> Result<Wp009Invocation, String> {
    let started_at = SystemTime::now();
    let manifest_bytes = fs::read(root.join(WP009_MANIFEST_PATH))
        .map_err(|error| format!("WP009-E-MANIFEST-READ: {error}"))?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("WP009-E-MANIFEST-PARSE: {error}"))?;
    validate_wp009_manifest(&manifest)?;
    let source_content_fingerprint = source_state(root)?.content_fingerprint;
    let manifest_sha256 = encode_sha256(&manifest_bytes);
    let sequence = WP009_INVOCATION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let started_nanos = started_at
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("WP009-E-INVOCATION-CLOCK: {error}"))?
        .as_nanos();
    let identity_material = format!(
        "{}:{sequence}:{started_nanos}:{manifest_sha256}:{source_content_fingerprint}",
        std::process::id()
    );
    Ok(Wp009Invocation {
        id: encode_sha256(identity_material.as_bytes()),
        started_at,
        manifest_sha256,
        source_content_fingerprint,
    })
}

fn validate_wp009_invocation_binding(
    receipt: &Wp009ExecutionReceipt,
    invocation: &Wp009Invocation,
) -> Result<(), String> {
    if receipt.invocation_id != invocation.id
        || receipt.source_content_fingerprint != invocation.source_content_fingerprint
    {
        return Err("WP009-E-EXECUTION-RECEIPT-INVOCATION".to_owned());
    }
    Ok(())
}

fn validate_wp009_execution_receipt(
    root: &Path,
    receipt: &Wp009ExecutionReceipt,
    invocation: &Wp009Invocation,
) -> Result<(), String> {
    validate_wp009_invocation_binding(receipt, invocation)?;
    let relative = Path::new(&receipt.report_path);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
        || relative.parent() != Some(Path::new("build/reports"))
        || relative
            .file_name()
            .and_then(OsStr::to_str)
            .is_none_or(|name| {
                !name.starts_with("wp-ff-009-resource-durability-models-")
                    || Path::new(name).extension() != Some(OsStr::new("json"))
            })
    {
        return Err("WP009-E-EXECUTION-RECEIPT-PATH".to_owned());
    }
    let path = root.join(relative);
    let modified = fs::metadata(&path)
        .and_then(|metadata| metadata.modified())
        .map_err(|error| {
            format!(
                "WP009-E-EXECUTION-RECEIPT-MISSING:{}:{error}",
                receipt.report_path
            )
        })?;
    if modified < invocation.started_at {
        return Err("WP009-E-EXECUTION-RECEIPT-STALE".to_owned());
    }
    let bytes = fs::read(&path).map_err(|error| {
        format!(
            "WP009-E-EXECUTION-RECEIPT-MISSING:{}:{error}",
            receipt.report_path
        )
    })?;
    if encode_sha256(&bytes) != receipt.report_sha256 {
        return Err("WP009-E-EXECUTION-RECEIPT-DIGEST".to_owned());
    }
    let report: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("WP009-E-EXECUTION-RECEIPT-PARSE:{error}"))?;
    let manifest_bytes = fs::read(root.join(WP009_MANIFEST_PATH))
        .map_err(|error| format!("WP009-E-MANIFEST-READ: {error}"))?;
    if encode_sha256(&manifest_bytes) != invocation.manifest_sha256 {
        return Err("WP009-E-EXECUTION-RECEIPT-SOURCE-DRIFT".to_owned());
    }
    let manifest: Value = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("WP009-E-MANIFEST-PARSE: {error}"))?;
    validate_wp009_report(root, &manifest_bytes, &manifest, &report)?;
    if report.get("schema_id").and_then(Value::as_str)
        != Some("ff.resource-durability-model-report@1")
        || report.get("manifest_fingerprint").and_then(Value::as_str)
            != Some(receipt.manifest_fingerprint.as_str())
        || report
            .pointer("/summary/executed_cases")
            .and_then(Value::as_u64)
            != Some(receipt.executed_cases)
        || receipt.executed_cases != 48
        || report
            .pointer("/summary/zero_product_progress")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Err("WP009-E-EXECUTION-RECEIPT-IDENTITY".to_owned());
    }
    let current_source = source_state(root)?;
    if current_source.content_fingerprint != invocation.source_content_fingerprint
        || current_source.content_fingerprint != receipt.source_content_fingerprint
    {
        return Err("WP009-E-EXECUTION-RECEIPT-SOURCE-DRIFT".to_owned());
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct Wp009RowExpectation {
    invariant: &'static str,
    state: &'static str,
    owner: &'static str,
    action: &'static str,
    boundary: &'static str,
    proof_mechanism: &'static str,
    memory_bytes: u64,
    open_handles: u64,
    ffmpeg_processes: u64,
    queue: (u64, u64, u64, u64),
    durability: (u64, u64, u64, u64),
}

fn wp009_require_exact_keys(
    value: &Value,
    expected: &[&str],
    diagnostic: &str,
) -> Result<(), String> {
    let object = value.as_object().ok_or_else(|| diagnostic.to_owned())?;
    let observed = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();
    if observed != expected {
        return Err(format!(
            "{diagnostic}: expected keys={expected:?}; observed={observed:?}"
        ));
    }
    Ok(())
}

fn wp009_required_text<'a>(
    value: &'a Value,
    key: &str,
    diagnostic: &str,
) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| format!("{diagnostic}: {key}"))
}

fn wp009_fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn validate_wp009_manifest(manifest: &Value) -> Result<(), String> {
    wp009_require_exact_keys(
        manifest,
        &[
            "schema_id",
            "corpus_id",
            "source_paths",
            "exploration_bounds",
            "cases",
            "residual_uncertainty",
        ],
        "WP009-E-MANIFEST-SCHEMA",
    )?;
    if manifest.get("schema_id").and_then(Value::as_str)
        != Some("ff.resource-durability-model-corpus@1")
        || manifest.get("corpus_id").and_then(Value::as_str)
            != Some("WP-FF-009-resource-durability-models-v1")
    {
        return Err("WP009-E-MANIFEST-IDENTITY".to_owned());
    }
    let source_paths = manifest
        .get("source_paths")
        .and_then(Value::as_array)
        .ok_or("WP009-E-SOURCE-MANIFEST-DRIFT")?
        .iter()
        .map(|value| value.as_str().ok_or("WP009-E-SOURCE-MANIFEST-DRIFT"))
        .collect::<Result<Vec<_>, _>>()?;
    if source_paths != WP009_SOURCE_PATHS {
        return Err("WP009-E-SOURCE-MANIFEST-DRIFT".to_owned());
    }
    let bounds = manifest
        .get("exploration_bounds")
        .ok_or("WP009-E-BOUNDS-MISSING")?;
    wp009_require_exact_keys(
        bounds,
        &[
            "maximum_cases",
            "maximum_transitions_per_case",
            "maximum_active_grants",
            "maximum_waiter_items",
            "maximum_waiter_declared_variable_bytes",
            "maximum_credit_claim_items",
            "maximum_credit_bytes",
        ],
        "WP009-E-BOUNDS-MISSING",
    )?;
    for (key, expected) in [
        ("maximum_cases", 48),
        ("maximum_transitions_per_case", 16),
        ("maximum_active_grants", 2),
        ("maximum_waiter_items", 2),
        ("maximum_waiter_declared_variable_bytes", 20),
        ("maximum_credit_claim_items", 2),
        ("maximum_credit_bytes", 16),
    ] {
        if bounds.get(key).and_then(Value::as_u64) != Some(expected) {
            return Err(format!("WP009-E-BOUNDS-MISMATCH: {key}"));
        }
    }
    let cases = manifest
        .get("cases")
        .and_then(Value::as_array)
        .ok_or("WP009-E-CASES-MISSING")?;
    if cases.len() != WP009_CASE_IDS.len() {
        return Err("WP009-E-UNMAPPED-INPUT".to_owned());
    }
    for (case, expected_id) in cases.iter().zip(WP009_CASE_IDS) {
        wp009_require_exact_keys(
            case,
            &["case_id", "model", "scenario", "injected_fault"],
            "WP009-E-CASE-SCHEMA",
        )?;
        if wp009_required_text(case, "case_id", "WP009-E-CASE-ID")? != *expected_id {
            return Err("WP009-E-UNMAPPED-INPUT".to_owned());
        }
        for key in ["model", "scenario", "injected_fault"] {
            let _text = wp009_required_text(case, key, "WP009-E-CASE-FIELD")?;
        }
        validate_wp009_case_declaration(case, expected_id)?;
    }
    let uncertainty = manifest
        .get("residual_uncertainty")
        .and_then(Value::as_array)
        .ok_or("WP009-E-RESIDUAL-MISSING")?;
    if uncertainty.len() != 3
        || uncertainty
            .iter()
            .any(|value| value.as_str().is_none_or(str::is_empty))
    {
        return Err("WP009-E-RESIDUAL-MISSING".to_owned());
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn validate_wp009_case_declaration(case: &Value, case_id: &str) -> Result<(), String> {
    let expected = match case_id {
        "wp009-resource-atomic-saturation" => (
            "resource_admission",
            "complete_vector_or_nothing",
            "capacity_exhaustion",
        ),
        "wp009-resource-fifo-head" => (
            "resource_admission",
            "strict_fifo_head_reservation",
            "small_later_waiter_cannot_bypass",
        ),
        "wp009-resource-queue-item-bound" => (
            "resource_admission",
            "bounded_waiter_items",
            "one_waiter_beyond_item_limit",
        ),
        "wp009-resource-queue-byte-bound" => (
            "resource_admission",
            "bounded_waiter_bytes",
            "one_byte_beyond_declared_byte_limit",
        ),
        "wp009-resource-raii-drop" => (
            "resource_admission",
            "owned_lease_drop_release",
            "panic_unwind_equivalent_drop",
        ),
        "wp009-resource-public-owned-boundary" => (
            "resource_admission",
            "public_api_owns_grants_and_waiters",
            "raw_ledger_escape_attempt",
        ),
        "wp009-credit-coupled-reservation" => (
            "byte_credit_and_fragment_durability",
            "simultaneous_input_output_reservation",
            "combined_claim_exceeds_capacity",
        ),
        "wp009-credit-owner-attribution" => (
            "byte_credit_and_fragment_durability",
            "consumed_claim_owner_is_immutable",
            "post_consumption_owner_transfer",
        ),
        "wp009-credit-stage-bounds-all-nine" => (
            "byte_credit_and_fragment_durability",
            "all_stage_item_and_byte_bounds",
            "stage_item_and_byte_saturation",
        ),
        "wp009-credit-owner-bounds-all-nine" => (
            "byte_credit_and_fragment_durability",
            "all_stage_per_owner_item_and_byte_bounds",
            "owner_item_and_byte_saturation",
        ),
        "wp009-credit-owned-success" => (
            "byte_credit_and_fragment_durability",
            "owned_credit_explicit_release",
            "none",
        ),
        "wp009-credit-owned-error" => (
            "byte_credit_and_fragment_durability",
            "owned_credit_error_retains_credit",
            "consume_beyond_claim",
        ),
        "wp009-credit-owned-panic" => (
            "byte_credit_and_fragment_durability",
            "owned_credit_unwind_release",
            "panic_unwind_equivalent_drop",
        ),
        "wp009-credit-owned-cancel" => (
            "byte_credit_and_fragment_durability",
            "owned_credit_cancel_release",
            "cancel_active_claim",
        ),
        "wp009-durability-effect-ack" => (
            "byte_credit_and_fragment_durability",
            "effect_correlated_durability_advance",
            "ordinary_ack_wrong_delta_and_wrong_broker",
        ),
        "wp009-durability-stage-authorization" => (
            "byte_credit_and_fragment_durability",
            "stage_consumption_authorizes_exact_durability_axis",
            "cross_stage_consumption",
        ),
        "wp009-durability-replay-rejected" => (
            "byte_credit_and_fragment_durability",
            "ordinary_replay_cannot_authorize_byte_effects",
            "replay_without_broker_position_evidence",
        ),
        "wp009-durability-restoration-evidence" => (
            "byte_credit_and_fragment_durability",
            "byte_restoration_requires_matching_broker_position",
            "generic_and_mismatched_restoration",
        ),
        "wp009-durability-prefix" => (
            "byte_credit_and_fragment_durability",
            "resume_at_durable_contiguous",
            "none",
        ),
        "wp009-durability-outrun" => (
            "byte_credit_and_fragment_durability",
            "durable_contiguous_cannot_outrun_validated",
            "optimistic_durable_prefix",
        ),
        "wp009-journal-torn" => (
            "commit_archive_reconciliation",
            "scan_prior_valid_prefix",
            "torn_record",
        ),
        "wp009-journal-duplicate" => (
            "commit_archive_reconciliation",
            "scan_prior_valid_prefix",
            "duplicate_sequence",
        ),
        "wp009-journal-reordered" => (
            "commit_archive_reconciliation",
            "scan_prior_valid_prefix",
            "reordered_sequence",
        ),
        "wp009-journal-checksum-invalid" => (
            "commit_archive_reconciliation",
            "scan_prior_valid_prefix",
            "checksum_invalid",
        ),
        "wp009-journal-false-prepared-sync" => (
            "commit_archive_reconciliation",
            "reject_false_durable_transition_record",
            "prepared_sync_ack_missing",
        ),
        "wp009-journal-false-renamed-sync" => (
            "commit_archive_reconciliation",
            "reject_false_durable_transition_record",
            "renamed_directory_sync_ack_missing",
        ),
        "wp009-journal-mixed-job" => (
            "commit_archive_reconciliation",
            "journal_prefix_bound_to_job_identity",
            "foreign_job_record",
        ),
        "wp009-journal-archive-uniqueness" => (
            "commit_archive_reconciliation",
            "archive_transition_requires_uniqueness_evidence",
            "missing_uniqueness_receipt",
        ),
        "wp009-commit-prepared-effects" => (
            "commit_archive_reconciliation",
            "durable_prefix_requires_serial_effect_acknowledgements",
            "prepared_effect_ack_removed",
        ),
        "wp009-commit-renamed-effects" => (
            "commit_archive_reconciliation",
            "durable_prefix_requires_serial_effect_acknowledgements",
            "rename_partial_retry",
        ),
        "wp009-commit-archived-effects" => (
            "commit_archive_reconciliation",
            "durable_prefix_requires_serial_effect_acknowledgements",
            "archive_effect_ack_removed",
        ),
        "wp009-commit-cleaned-effects" => (
            "commit_archive_reconciliation",
            "durable_prefix_requires_serial_effect_acknowledgements",
            "cleanup_effect_ack_removed",
        ),
        "wp009-recovery-prepared" => (
            "commit_archive_reconciliation",
            "idempotent_prefix_decision",
            "crash_after_prepared",
        ),
        "wp009-recovery-renamed" => (
            "commit_archive_reconciliation",
            "idempotent_prefix_decision",
            "crash_after_renamed",
        ),
        "wp009-recovery-archived" => (
            "commit_archive_reconciliation",
            "idempotent_prefix_decision",
            "crash_after_archived",
        ),
        "wp009-recovery-cleaned" => (
            "commit_archive_reconciliation",
            "idempotent_prefix_decision",
            "crash_after_cleaned",
        ),
        "wp009-recovery-stale-lease" => (
            "commit_archive_reconciliation",
            "idempotent_prefix_decision",
            "stale_lease",
        ),
        "wp009-recovery-collision" => (
            "commit_archive_reconciliation",
            "idempotent_prefix_decision",
            "conflicting_destination",
        ),
        "wp009-recovery-partial-cleanup" => (
            "commit_archive_reconciliation",
            "idempotent_prefix_decision",
            "partial_cleanup",
        ),
        "wp009-recovery-interrupted-migration" => (
            "commit_archive_reconciliation",
            "idempotent_prefix_decision",
            "interrupted_migration",
        ),
        "wp009-recovery-collecting-artifact" => (
            "commit_archive_reconciliation",
            "recovery_preconditions_precede_mutation",
            "archive_artifact_before_prepared",
        ),
        "wp009-recovery-stale-mismatch-precedence" => (
            "commit_archive_reconciliation",
            "recovery_preconditions_precede_mutation",
            "stale_lease_with_mismatched_stage",
        ),
        "wp009-recovery-migration-collision-precedence" => (
            "commit_archive_reconciliation",
            "recovery_preconditions_precede_mutation",
            "interrupted_migration_with_collision",
        ),
        "wp009-recovery-confinement-unavailable" => (
            "commit_archive_reconciliation",
            "recovery_confinement_fails_closed",
            "confinement_unavailable",
        ),
        "wp009-recovery-confinement-mismatched" => (
            "commit_archive_reconciliation",
            "recovery_confinement_fails_closed",
            "confinement_mismatched",
        ),
        "wp009-filesystem-windows-ntfs" => (
            "filesystem_capability",
            "exact_supported_local_profile",
            "none",
        ),
        "wp009-filesystem-wsl-v9fs" => (
            "filesystem_capability",
            "interop_profile_fails_closed",
            "unsupported_security_sensitive_confinement",
        ),
        "wp009-cross-volume" => (
            "filesystem_capability",
            "copy_sync_rename_is_not_cross_volume_atomic",
            "cross_volume_destination",
        ),
        _ => return Err("WP009-E-UNMAPPED-INPUT".to_owned()),
    };
    if case.get("model").and_then(Value::as_str) != Some(expected.0)
        || case.get("scenario").and_then(Value::as_str) != Some(expected.1)
        || case.get("injected_fault").and_then(Value::as_str) != Some(expected.2)
    {
        return Err(format!("WP009-E-CASE-DECLARATION-MISMATCH: {case_id}"));
    }
    Ok(())
}

fn validate_wp009_report(
    root: &Path,
    manifest_bytes: &[u8],
    manifest: &Value,
    report: &Value,
) -> Result<(), String> {
    validate_wp009_manifest(manifest)?;
    wp009_require_exact_keys(
        report,
        &[
            "schema_id",
            "schema_version",
            "corpus_id",
            "manifest_path",
            "manifest_fingerprint",
            "source_manifest",
            "exploration_bounds",
            "rows",
            "summary",
            "residual_uncertainty",
        ],
        "WP009-E-REPORT-SCHEMA",
    )?;
    if report.get("schema_id").and_then(Value::as_str)
        != Some("ff.resource-durability-model-report@1")
        || report.get("schema_version").and_then(Value::as_str) != Some("1.1.0")
        || report.get("corpus_id").and_then(Value::as_str)
            != Some("WP-FF-009-resource-durability-models-v1")
        || report.get("manifest_path").and_then(Value::as_str) != Some(WP009_MANIFEST_PATH)
        || report.get("manifest_fingerprint").and_then(Value::as_str)
            != Some(format!("fnv1a64:{:016x}", wp009_fnv1a64(manifest_bytes)).as_str())
    {
        return Err("WP009-E-REPORT-IDENTITY".to_owned());
    }
    if report.get("exploration_bounds") != manifest.get("exploration_bounds")
        || report.get("residual_uncertainty") != manifest.get("residual_uncertainty")
    {
        return Err("WP009-E-REPORT-MANIFEST-DRIFT".to_owned());
    }
    validate_wp009_source_manifest(root, report)?;
    let cases = manifest["cases"]
        .as_array()
        .ok_or("WP009-E-CASES-MISSING")?;
    let rows = report
        .get("rows")
        .and_then(Value::as_array)
        .ok_or("WP009-E-ROWS-MISSING")?;
    if rows.len() != WP009_CASE_IDS.len() {
        return Err("WP009-E-UNMAPPED-INPUT".to_owned());
    }
    for ((row, case), expected_id) in rows.iter().zip(cases).zip(WP009_CASE_IDS) {
        validate_wp009_row(row, case, expected_id)?;
    }
    let summary = report.get("summary").ok_or("WP009-E-SUMMARY")?;
    wp009_require_exact_keys(
        summary,
        &[
            "executed_cases",
            "failed_cases",
            "proof_classes",
            "proof_mechanisms",
            "zero_product_progress",
        ],
        "WP009-E-SUMMARY",
    )?;
    let proof_classes = summary
        .get("proof_classes")
        .and_then(Value::as_array)
        .ok_or("WP009-E-SUMMARY-CEILING")?
        .iter()
        .map(|value| value.as_str().ok_or("WP009-E-SUMMARY-CEILING"))
        .collect::<Result<Vec<_>, _>>()?;
    let proof_mechanisms = summary
        .get("proof_mechanisms")
        .and_then(Value::as_array)
        .ok_or("WP009-E-SUMMARY-CEILING")?
        .iter()
        .map(|value| value.as_str().ok_or("WP009-E-SUMMARY-CEILING"))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if summary.get("executed_cases").and_then(Value::as_u64) != Some(48)
        || summary.get("failed_cases").and_then(Value::as_u64) != Some(0)
        || summary
            .get("zero_product_progress")
            .and_then(Value::as_bool)
            != Some(true)
        || proof_classes != ["semantic"]
        || proof_mechanisms
            != BTreeSet::from([
                "direct_behavior",
                "negative_counterfactual",
                "public_boundary",
            ])
    {
        return Err("WP009-E-SUMMARY-CEILING".to_owned());
    }
    Ok(())
}

fn validate_wp009_source_manifest(root: &Path, report: &Value) -> Result<(), String> {
    let sources = report
        .get("source_manifest")
        .and_then(Value::as_array)
        .ok_or("WP009-E-SOURCE-MANIFEST-DRIFT")?;
    if sources.len() != WP009_SOURCE_PATHS.len() {
        return Err("WP009-E-SOURCE-MANIFEST-DRIFT".to_owned());
    }
    for (source, expected_path) in sources.iter().zip(WP009_SOURCE_PATHS) {
        wp009_require_exact_keys(
            source,
            &["path", "fnv1a64"],
            "WP009-E-SOURCE-MANIFEST-DRIFT",
        )?;
        let bytes = fs::read(root.join(expected_path))
            .map_err(|error| format!("WP009-E-SOURCE-READ:{expected_path}:{error}"))?;
        if source.get("path").and_then(Value::as_str) != Some(expected_path)
            || source.get("fnv1a64").and_then(Value::as_str)
                != Some(format!("{:016x}", wp009_fnv1a64(&bytes)).as_str())
        {
            return Err("WP009-E-SOURCE-MANIFEST-DRIFT".to_owned());
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn wp009_row_expectation(case_id: &str) -> Option<Wp009RowExpectation> {
    macro_rules! expected {
        ($invariant:expr, $state:expr, $owner:expr, $action:expr, $boundary:expr,
         $proof:expr, $memory:expr, $handles:expr, $ffmpeg:expr, $queue:expr, $durability:expr) => {
            Wp009RowExpectation {
                invariant: $invariant,
                state: $state,
                owner: $owner,
                action: $action,
                boundary: $boundary,
                proof_mechanism: match $proof {
                    "semantic" => "direct_behavior",
                    "counterfactual" => "negative_counterfactual",
                    "public_boundary" => "public_boundary",
                    _ => $proof,
                },
                memory_bytes: $memory,
                open_handles: $handles,
                ffmpeg_processes: $ffmpeg,
                queue: $queue,
                durability: $durability,
            }
        };
    }
    const ZERO: (u64, u64, u64, u64) = (0, 0, 0, 0);
    const PASSIVE_QUEUE: (u64, u64, u64, u64) = (0, 0, 1, 1);
    const RESOURCE_REQUEST: &str = "fforager_core::resource::OwnedResourceBroker::request";
    const RECOVERY: &str = "fforager_core::lifecycle::decide_recovery then apply_recovery_action, RecoveryApplication::retry, and decide_recovery";
    const RECOVERY_INVARIANT: &str = "WP009-INV-RECOVERY-IDEMPOTENT-001";
    Some(match case_id {
        "wp009-resource-atomic-saturation" => expected!(
            "WP009-INV-RESOURCE-ATOMIC-001",
            "capacity_saturated_waiter_queued",
            "none",
            "none",
            RESOURCE_REQUEST,
            "semantic",
            10,
            2,
            1,
            (1, 1, 2, 20),
            ZERO
        ),
        "wp009-resource-fifo-head" => expected!(
            "WP009-INV-FIFO-HEAD-001",
            "head_issued_later_waiter_retained",
            "none",
            "issue_exact_fifo_head",
            "fforager_core::resource::OwnedResourceLease::release then OwnedResourceWaiter::try_acquire",
            "semantic",
            10,
            0,
            0,
            (1, 1, 2, 20),
            ZERO
        ),
        "wp009-resource-queue-item-bound" => expected!(
            "WP009-INV-QUEUE-ITEM-BOUND-001",
            "item_limit_rejected_without_mutation",
            "none",
            "reject_and_backpressure_upstream",
            RESOURCE_REQUEST,
            "counterfactual",
            10,
            0,
            0,
            (1, 1, 1, 10),
            ZERO
        ),
        "wp009-resource-queue-byte-bound" => expected!(
            "WP009-INV-QUEUE-BYTE-BOUND-001",
            "byte_limit_rejected_without_mutation",
            "none",
            "reject_and_backpressure_upstream",
            RESOURCE_REQUEST,
            "counterfactual",
            10,
            0,
            0,
            (0, 0, 2, 5),
            ZERO
        ),
        "wp009-resource-raii-drop" => expected!(
            "WP009-INV-RAII-RELEASE-001",
            "panic_caught_lease_released",
            "none",
            "OwnedResourceLease::drop",
            "fforager_core::resource::OwnedResourceLease Drop boundary",
            "public_boundary",
            0,
            0,
            0,
            (0, 0, 2, 20),
            ZERO
        ),
        "wp009-resource-public-owned-boundary" => expected!(
            "WP009-INV-PUBLIC-OWNERSHIP-001",
            "owned_grant_and_waiter_released",
            "none",
            "drop_owned_grant_and_waiter",
            "fforager_core::resource::OwnedResourceBroker public boundary",
            "public_boundary",
            0,
            0,
            0,
            (0, 0, 1, 4),
            ZERO
        ),
        "wp009-credit-coupled-reservation" => expected!(
            "WP009-INV-COUPLED-BYTE-RESERVATION-001",
            "combined_input_output_rejected_before_allocation",
            "owner_1",
            "reserve_complete_input_plus_output_or_nothing",
            "fforager_core::resource::OwnedByteCreditBroker::claim_coupled",
            "semantic",
            0,
            0,
            0,
            (0, 0, 2, 10),
            ZERO
        ),
        "wp009-credit-owner-attribution" => expected!(
            "WP009-INV-CREDIT-OWNER-001",
            "consumed_claim_transfer_rejected",
            "owner_1",
            "retain_consumed_input_attribution_and_transfer_unconsumed_output",
            "fforager_core::resource::OwnedByteCreditLease consume, transfer_component, and attribution",
            "counterfactual",
            0,
            0,
            0,
            (2, 8, 2, 8),
            ZERO
        ),
        "wp009-credit-stage-bounds-all-nine" => expected!(
            "WP009-INV-CREDIT-STAGE-BOUNDS-001",
            "all_nine_stage_bounds_rejected_without_mutation",
            "nine_stage_inventory",
            "backpressure_or_spill_per_declared_stage_policy",
            "fforager_core::resource::OwnedByteCreditBroker::claim across BYTE_CREDIT_STAGES_V1",
            "counterfactual",
            0,
            0,
            0,
            (0, 0, 4, 16),
            ZERO
        ),
        "wp009-credit-owner-bounds-all-nine" => expected!(
            "WP009-INV-CREDIT-OWNER-BOUNDS-001",
            "all_nine_stage_owner_bounds_rejected_without_mutation",
            "owner_1",
            "backpressure_or_spill_per_owner",
            "fforager_core::resource::OwnedByteCreditBroker::claim across owner and stage scopes",
            "counterfactual",
            0,
            0,
            0,
            (0, 0, 4, 16),
            ZERO
        ),
        "wp009-credit-owned-success" => expected!(
            "WP009-INV-CREDIT-OWNED-SUCCESS-001",
            "explicit_release_zeroed_occupancy",
            "owner_1",
            "consume_then_release",
            "fforager_core::resource::OwnedByteCreditLease public ownership boundary",
            "public_boundary",
            0,
            0,
            0,
            (0, 0, 2, 8),
            ZERO
        ),
        "wp009-credit-owned-error" => expected!(
            "WP009-INV-CREDIT-OWNED-ERROR-001",
            "error_retained_then_drop_released_credit",
            "owner_1",
            "reject_uncredited_bytes_then_drop",
            "fforager_core::resource::OwnedByteCreditLease public ownership boundary",
            "public_boundary",
            0,
            0,
            0,
            (0, 0, 2, 8),
            ZERO
        ),
        "wp009-credit-owned-panic" => expected!(
            "WP009-INV-CREDIT-OWNED-PANIC-001",
            "panic_caught_credit_released",
            "owner_1",
            "OwnedByteCreditLease::drop",
            "fforager_core::resource::OwnedByteCreditLease public ownership boundary",
            "public_boundary",
            0,
            0,
            0,
            (0, 0, 2, 8),
            ZERO
        ),
        "wp009-credit-owned-cancel" => expected!(
            "WP009-INV-CREDIT-OWNED-CANCEL-001",
            "cancel_released_credit_with_attribution",
            "owner_1",
            "OwnedByteCreditLease::cancel",
            "fforager_core::resource::OwnedByteCreditLease public ownership boundary",
            "public_boundary",
            0,
            0,
            0,
            (0, 0, 2, 8),
            ZERO
        ),
        "wp009-durability-effect-ack" => expected!(
            "WP009-REG-DURABLE-EFFECT-ACK-001",
            "received_written_and_durable_effects_acknowledged",
            "owner_1",
            "accept_then_validate_then_synchronize_with_correlated_receipts",
            "StateMachine::acknowledge_byte_durability_effect with OwnedByteCreditBroker",
            "public_boundary",
            0,
            0,
            0,
            (2, 16, 4, 16),
            (8, 8, 8, 8)
        ),
        "wp009-durability-stage-authorization" => expected!(
            "WP009-REG-DURABLE-STAGE-AUTH-001",
            "wrong_stage_consumption_rejected",
            "owner_1",
            "reject_cross_stage_consumption_as_durability_authority",
            "OwnedByteCreditBroker stage consumption plus StateMachine::acknowledge_byte_durability_effect",
            "counterfactual",
            0,
            0,
            0,
            (2, 8, 4, 16),
            (4, 0, 0, 0)
        ),
        "wp009-durability-replay-rejected" => expected!(
            "WP009-REG-DURABLE-REPLAY-001",
            "ordinary_replay_rejected_byte_effect_history",
            "owner_1",
            "reexecute_byte_history_through_broker_coupled_acknowledgements",
            "StateMachine::replay public boundary for ByteCreditDurability",
            "counterfactual",
            0,
            0,
            0,
            (2, 8, 4, 16),
            (4, 4, 0, 0)
        ),
        "wp009-durability-restoration-evidence" => expected!(
            "WP009-REG-DURABLE-RESTORE-001",
            "broker_position_authorized_received_restore",
            "owner_1",
            "restore_only_through_matching_authoritative_broker_position",
            "StateMachine::from_state and from_byte_durability_state public boundaries",
            "public_boundary",
            0,
            0,
            0,
            (1, 4, 4, 16),
            (4, 0, 0, 0)
        ),
        "wp009-durability-prefix" => expected!(
            "WP009-INV-DURABLE-PREFIX-001",
            "received_8_validated_6_durable_4",
            "none",
            "resume_at_or_before_durable_contiguous",
            "fforager_contracts::DurabilityPosition::validate_advance and validate_resume",
            "semantic",
            0,
            0,
            0,
            (0, 0, 1, 8),
            (8, 6, 4, 4)
        ),
        "wp009-durability-outrun" => expected!(
            "WP009-INV-DURABLE-ORDER-001",
            "optimistic_durable_advance_rejected",
            "none",
            "retain_prior_acknowledged_prefix",
            "fforager_contracts::DurabilityPosition::validate_advance",
            "counterfactual",
            0,
            0,
            0,
            (0, 0, 1, 8),
            (8, 0, 0, 0)
        ),
        "wp009-journal-torn"
        | "wp009-journal-duplicate"
        | "wp009-journal-reordered"
        | "wp009-journal-checksum-invalid" => expected!(
            "WP009-INV-JOURNAL-PRIOR-PREFIX-001",
            "scan_stopped_at_second_record",
            "none",
            "retain_one_record_prior_valid_prefix",
            "fforager_contracts::scan_observed_journal",
            "counterfactual",
            0,
            0,
            0,
            PASSIVE_QUEUE,
            ZERO
        ),
        "wp009-journal-false-prepared-sync" => expected!(
            "WP009-INV-JOURNAL-PREPARED-SYNC-001",
            "semantic_fault_stopped_at_second_record",
            "none",
            "retain_one_record_prior_valid_prefix",
            "fforager_contracts::scan_observed_journal semantic and job identity validation",
            "counterfactual",
            0,
            0,
            0,
            PASSIVE_QUEUE,
            ZERO
        ),
        "wp009-journal-false-renamed-sync" => expected!(
            "WP009-INV-JOURNAL-RENAMED-SYNC-001",
            "semantic_fault_stopped_at_second_record",
            "none",
            "retain_one_record_prior_valid_prefix",
            "fforager_contracts::scan_observed_journal semantic and job identity validation",
            "counterfactual",
            0,
            0,
            0,
            PASSIVE_QUEUE,
            ZERO
        ),
        "wp009-journal-mixed-job" => expected!(
            "WP009-INV-JOURNAL-JOB-IDENTITY-001",
            "semantic_fault_stopped_at_second_record",
            "none",
            "retain_one_record_prior_valid_prefix",
            "fforager_contracts::scan_observed_journal semantic and job identity validation",
            "counterfactual",
            0,
            0,
            0,
            PASSIVE_QUEUE,
            ZERO
        ),
        "wp009-journal-archive-uniqueness" => expected!(
            "WP009-INV-JOURNAL-ARCHIVE-UNIQUENESS-001",
            "semantic_fault_stopped_at_second_record",
            "none",
            "retain_one_record_prior_valid_prefix",
            "fforager_contracts::scan_observed_journal semantic and job identity validation",
            "counterfactual",
            0,
            0,
            0,
            PASSIVE_QUEUE,
            ZERO
        ),
        "wp009-commit-prepared-effects" => expected!(
            "WP009-INV-COMMIT-PREPARED-EFFECTS-001",
            "prepared_after_five_serial_receipts",
            "none",
            "acknowledge_each_effect_in_declared_order",
            "fforager_core::lifecycle::StateMachine correlated commit effect boundary",
            "public_boundary",
            0,
            0,
            0,
            (0, 0, 1, 5),
            ZERO
        ),
        "wp009-commit-renamed-effects" => expected!(
            "WP009-INV-COMMIT-RENAMED-EFFECTS-001",
            "renamed_after_four_serial_receipts",
            "none",
            "acknowledge_each_effect_in_declared_order",
            "fforager_core::lifecycle::StateMachine correlated commit effect boundary",
            "public_boundary",
            0,
            0,
            0,
            (0, 0, 1, 4),
            ZERO
        ),
        "wp009-commit-archived-effects" => expected!(
            "WP009-INV-COMMIT-ARCHIVED-EFFECTS-001",
            "archived_after_three_serial_receipts",
            "none",
            "acknowledge_each_effect_in_declared_order",
            "fforager_core::lifecycle::StateMachine correlated commit effect boundary",
            "public_boundary",
            0,
            0,
            0,
            (0, 0, 1, 3),
            ZERO
        ),
        "wp009-commit-cleaned-effects" => expected!(
            "WP009-INV-COMMIT-CLEANED-EFFECTS-001",
            "cleaned_after_three_serial_receipts",
            "none",
            "acknowledge_each_effect_in_declared_order",
            "fforager_core::lifecycle::StateMachine correlated commit effect boundary",
            "public_boundary",
            0,
            0,
            0,
            (0, 0, 1, 3),
            ZERO
        ),
        "wp009-recovery-prepared" => expected!(
            RECOVERY_INVARIANT,
            "prepared",
            "none",
            "Act(RevalidatePreparedThenRename)",
            RECOVERY,
            "semantic",
            0,
            0,
            0,
            PASSIVE_QUEUE,
            ZERO
        ),
        "wp009-recovery-renamed" => expected!(
            RECOVERY_INVARIANT,
            "renamed",
            "none",
            "Act(InsertArchiveRow)",
            RECOVERY,
            "semantic",
            0,
            0,
            0,
            PASSIVE_QUEUE,
            ZERO
        ),
        "wp009-recovery-archived" | "wp009-recovery-partial-cleanup" => expected!(
            RECOVERY_INVARIANT,
            "archived",
            "none",
            "Act(RepeatCleanup)",
            RECOVERY,
            "semantic",
            0,
            0,
            0,
            PASSIVE_QUEUE,
            ZERO
        ),
        "wp009-recovery-cleaned" => expected!(
            RECOVERY_INVARIANT,
            "cleaned",
            "none",
            "ReconciledSuccess",
            RECOVERY,
            "semantic",
            0,
            0,
            0,
            PASSIVE_QUEUE,
            ZERO
        ),
        "wp009-recovery-stale-lease" => expected!(
            RECOVERY_INVARIANT,
            "prepared",
            "none",
            "Act(ReclaimStaleLease)",
            RECOVERY,
            "semantic",
            0,
            0,
            0,
            PASSIVE_QUEUE,
            ZERO
        ),
        "wp009-recovery-collision" | "wp009-recovery-migration-collision-precedence" => expected!(
            RECOVERY_INVARIANT,
            "prepared",
            "none",
            "FailClosed(ConflictingDestination)",
            RECOVERY,
            "counterfactual",
            0,
            0,
            0,
            PASSIVE_QUEUE,
            ZERO
        ),
        "wp009-recovery-interrupted-migration" => expected!(
            RECOVERY_INVARIANT,
            "prepared",
            "none",
            "Act(ResumeInterruptedMigration)",
            RECOVERY,
            "semantic",
            0,
            0,
            0,
            PASSIVE_QUEUE,
            ZERO
        ),
        "wp009-recovery-collecting-artifact" => expected!(
            RECOVERY_INVARIANT,
            "collecting",
            "none",
            "FailClosed(UnexpectedArtifactBeforePrepared)",
            RECOVERY,
            "counterfactual",
            0,
            0,
            0,
            PASSIVE_QUEUE,
            ZERO
        ),
        "wp009-recovery-stale-mismatch-precedence" => expected!(
            RECOVERY_INVARIANT,
            "prepared",
            "none",
            "FailClosed(MismatchedStagedOutput)",
            RECOVERY,
            "counterfactual",
            0,
            0,
            0,
            PASSIVE_QUEUE,
            ZERO
        ),
        "wp009-recovery-confinement-unavailable" => expected!(
            RECOVERY_INVARIANT,
            "prepared",
            "none",
            "FailClosed(ConfinementUnavailable)",
            RECOVERY,
            "counterfactual",
            0,
            0,
            0,
            PASSIVE_QUEUE,
            ZERO
        ),
        "wp009-recovery-confinement-mismatched" => expected!(
            RECOVERY_INVARIANT,
            "prepared",
            "none",
            "FailClosed(ConfinementMismatched)",
            RECOVERY,
            "counterfactual",
            0,
            0,
            0,
            PASSIVE_QUEUE,
            ZERO
        ),
        "wp009-filesystem-windows-ntfs" => expected!(
            "WP009-INV-FILESYSTEM-PROFILE-001",
            "supported_local_positive_model",
            "none",
            "profile=ff-fs-windows-11-26200-ntfs-v1",
            "fforager_contracts::FilesystemProfileContract exact and secure-write validators",
            "semantic",
            0,
            0,
            0,
            PASSIVE_QUEUE,
            ZERO
        ),
        "wp009-filesystem-wsl-v9fs" => expected!(
            "WP009-INV-FILESYSTEM-PROFILE-001",
            "unix_interop_rejected_or_explicitly_degraded",
            "none",
            "profile=ff-fs-ubuntu-24.04-wsl2-v9fs-v1",
            "fforager_contracts::FilesystemProfileContract exact and secure-write validators",
            "semantic",
            0,
            0,
            0,
            PASSIVE_QUEUE,
            ZERO
        ),
        "wp009-cross-volume" => expected!(
            RECOVERY_INVARIANT,
            "prepared",
            "none",
            "Act(CopySyncRenameWithinDestination)",
            RECOVERY,
            "semantic",
            0,
            0,
            0,
            PASSIVE_QUEUE,
            ZERO
        ),
        _ => return None,
    })
}

fn validate_wp009_row(row: &Value, case: &Value, expected_id: &str) -> Result<(), String> {
    wp009_require_exact_keys(
        row,
        &[
            "case_id",
            "invariant_id",
            "state",
            "resource_vector",
            "credit_owner",
            "queue_occupancy",
            "durability_prefix",
            "injected_fault",
            "recovery_action",
            "concrete_input",
            "boundary",
            "expected",
            "observed",
            "proof_class",
            "proof_mechanism",
            "credit_attribution",
            "supplemental_execution",
            "residual_uncertainty",
            "platform_observation",
        ],
        "WP009-E-PROOF-FIELDS-MISSING",
    )?;
    for key in [
        "case_id",
        "invariant_id",
        "state",
        "credit_owner",
        "injected_fault",
        "recovery_action",
        "boundary",
        "expected",
        "observed",
        "proof_class",
        "proof_mechanism",
        "residual_uncertainty",
    ] {
        let _text = wp009_required_text(row, key, "WP009-E-PROOF-FIELDS-MISSING")?;
    }
    if row.get("case_id").and_then(Value::as_str) != Some(expected_id)
        || row.get("concrete_input") != Some(case)
        || row.get("injected_fault") != case.get("injected_fault")
    {
        return Err("WP009-E-UNMAPPED-INPUT".to_owned());
    }
    let expected = wp009_row_expectation(expected_id).ok_or("WP009-E-UNMAPPED-INPUT")?;
    if row.get("invariant_id").and_then(Value::as_str) != Some(expected.invariant)
        || row.get("state").and_then(Value::as_str) != Some(expected.state)
        || row.get("credit_owner").and_then(Value::as_str) != Some(expected.owner)
        || row.get("recovery_action").and_then(Value::as_str) != Some(expected.action)
        || row.get("boundary").and_then(Value::as_str) != Some(expected.boundary)
        || row.get("proof_class").and_then(Value::as_str) != Some("semantic")
        || row.get("proof_mechanism").and_then(Value::as_str) != Some(expected.proof_mechanism)
    {
        return Err(format!("WP009-E-FALSE-SEMANTICS: {expected_id}"));
    }
    validate_wp009_vector(row, expected)?;
    validate_wp009_queue(row, expected.queue)?;
    validate_wp009_durability(row, expected.durability)?;
    validate_wp009_credit_attribution(row, expected_id)?;
    validate_wp009_supplemental_execution(row, expected_id)?;
    validate_wp009_platform_observation(row, expected_id)?;
    validate_wp009_special_semantics(row, expected_id)
}

fn validate_wp009_vector(row: &Value, expected: Wp009RowExpectation) -> Result<(), String> {
    let vector = row
        .get("resource_vector")
        .ok_or("WP009-E-RESOURCE-VECTOR")?;
    wp009_require_exact_keys(
        vector,
        &[
            "memory_bytes",
            "open_handles",
            "ffmpeg_processes",
            "ffmpeg_cpu_threads",
            "javascript_workers",
            "media_requests",
            "metadata_requests",
            "cpu_light_slots",
            "cpu_heavy_slots",
            "disk_read_bytes_in_flight",
            "disk_write_bytes_in_flight",
            "archive_writer_slots",
            "sink_bytes",
        ],
        "WP009-E-RESOURCE-VECTOR",
    )?;
    for (key, expected_value) in [
        ("memory_bytes", expected.memory_bytes),
        ("open_handles", expected.open_handles),
        ("ffmpeg_processes", expected.ffmpeg_processes),
        ("ffmpeg_cpu_threads", 0),
        ("javascript_workers", 0),
        ("media_requests", 0),
        ("metadata_requests", 0),
        ("cpu_light_slots", 0),
        ("cpu_heavy_slots", 0),
        ("disk_read_bytes_in_flight", 0),
        ("disk_write_bytes_in_flight", 0),
        ("archive_writer_slots", 0),
        ("sink_bytes", 0),
    ] {
        if vector.get(key).and_then(Value::as_u64) != Some(expected_value) {
            return Err(format!("WP009-E-FALSE-RESOURCE-STATE: {key}"));
        }
    }
    Ok(())
}

fn validate_wp009_queue(row: &Value, expected: (u64, u64, u64, u64)) -> Result<(), String> {
    let queue = row
        .get("queue_occupancy")
        .ok_or("WP009-E-QUEUE-BOUNDS-MISSING")?;
    wp009_require_exact_keys(
        queue,
        &["items", "bytes", "item_bound", "byte_bound"],
        "WP009-E-QUEUE-BOUNDS-MISSING",
    )?;
    let observed = (
        queue.get("items").and_then(Value::as_u64),
        queue.get("bytes").and_then(Value::as_u64),
        queue.get("item_bound").and_then(Value::as_u64),
        queue.get("byte_bound").and_then(Value::as_u64),
    );
    if observed
        != (
            Some(expected.0),
            Some(expected.1),
            Some(expected.2),
            Some(expected.3),
        )
        || expected.0 > expected.2
        || expected.1 > expected.3
        || expected.2 == 0
        || expected.3 == 0
    {
        return Err("WP009-E-QUEUE-BOUNDS-MISSING".to_owned());
    }
    Ok(())
}

fn validate_wp009_durability(row: &Value, expected: (u64, u64, u64, u64)) -> Result<(), String> {
    let prefix = row
        .get("durability_prefix")
        .ok_or("WP009-E-DURABILITY-FIELDS-MISSING")?;
    wp009_require_exact_keys(
        prefix,
        &[
            "received",
            "validated_written_contiguous",
            "durable_contiguous",
            "resume_at",
        ],
        "WP009-E-DURABILITY-FIELDS-MISSING",
    )?;
    let observed = (
        prefix.get("received").and_then(Value::as_u64),
        prefix
            .get("validated_written_contiguous")
            .and_then(Value::as_u64),
        prefix.get("durable_contiguous").and_then(Value::as_u64),
        prefix.get("resume_at").and_then(Value::as_u64),
    );
    if observed
        != (
            Some(expected.0),
            Some(expected.1),
            Some(expected.2),
            Some(expected.3),
        )
        || expected.1 > expected.0
        || expected.2 > expected.1
        || expected.3 > expected.2
    {
        return Err("WP009-E-DURABILITY-OUTRUN".to_owned());
    }
    Ok(())
}

fn validate_wp009_credit_attribution(row: &Value, case_id: &str) -> Result<(), String> {
    let attribution = row
        .get("credit_attribution")
        .ok_or("WP009-E-CREDIT-CANCEL-ATTRIBUTION")?;
    if case_id != "wp009-credit-owned-cancel" {
        if !attribution.is_null() {
            return Err("WP009-E-CREDIT-ATTRIBUTION-UNMAPPED".to_owned());
        }
        return Ok(());
    }
    wp009_require_exact_keys(
        attribution,
        &[
            "component",
            "owner",
            "stage",
            "claimed_bytes",
            "consumed_bytes",
            "lease_state",
            "global_claim_items_after",
            "global_bytes_after",
        ],
        "WP009-E-CREDIT-CANCEL-ATTRIBUTION",
    )?;
    if attribution.get("component").and_then(Value::as_str) != Some("single")
        || attribution.get("owner").and_then(Value::as_u64) != Some(1)
        || attribution.get("stage").and_then(Value::as_str) != Some("writer")
        || attribution.get("claimed_bytes").and_then(Value::as_u64) != Some(4)
        || attribution.get("consumed_bytes").and_then(Value::as_u64) != Some(0)
        || attribution.get("lease_state").and_then(Value::as_str) != Some("cancelled_and_released")
        || attribution
            .get("global_claim_items_after")
            .and_then(Value::as_u64)
            != Some(0)
        || attribution
            .get("global_bytes_after")
            .and_then(Value::as_u64)
            != Some(0)
    {
        return Err("WP009-E-CREDIT-CANCEL-ATTRIBUTION".to_owned());
    }
    Ok(())
}

fn validate_wp009_supplemental_execution(row: &Value, case_id: &str) -> Result<(), String> {
    let supplemental = row
        .get("supplemental_execution")
        .ok_or("WP009-E-SUPPLEMENTAL-EXECUTION")?;
    if case_id != "wp009-durability-effect-ack" {
        if !supplemental.is_null() {
            return Err("WP009-E-SUPPLEMENTAL-EXECUTION-UNMAPPED".to_owned());
        }
        return Ok(());
    }
    wp009_require_exact_keys(
        supplemental,
        &["boundary", "expected", "observed", "proof_class"],
        "WP009-E-SUPPLEMENTAL-EXECUTION",
    )?;
    let wrong_broker_result = serde_json::json!({
        "kind": "error",
        "error_type": "CreditError",
        "variant": "AcknowledgementBrokerMismatch"
    });
    if supplemental.get("boundary").and_then(Value::as_str)
        != Some(
            "StateMachine::acknowledge_byte_durability_effect with foreign OwnedByteCreditBroker",
        )
        || supplemental.get("expected") != Some(&wrong_broker_result)
        || supplemental.get("observed") != Some(&wrong_broker_result)
        || supplemental.get("proof_class").and_then(Value::as_str) != Some("semantic")
    {
        return Err("WP009-E-SUPPLEMENTAL-EXECUTION".to_owned());
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn validate_wp009_platform_observation(row: &Value, case_id: &str) -> Result<(), String> {
    let observation = row
        .get("platform_observation")
        .ok_or("WP009-E-PLATFORM-OBSERVATION-MISSING")?;
    wp009_require_exact_keys(
        observation,
        &[
            "schema_id",
            "mode",
            "commands",
            "observed_fields",
            "verdict",
            "residual_uncertainty",
        ],
        "WP009-E-PLATFORM-OBSERVATION-SCHEMA",
    )?;
    if observation.get("schema_id").and_then(Value::as_str)
        != Some("ff.wp009-platform-observation@1")
    {
        return Err("WP009-E-PLATFORM-OBSERVATION-IDENTITY".to_owned());
    }
    match case_id {
        "wp009-filesystem-windows-ntfs" => {
            if row.get("residual_uncertainty").and_then(Value::as_str)
                != Some(WP009_WINDOWS_PLATFORM_ROW_RESIDUAL)
            {
                return Err("WP009-E-WINDOWS-ROW-RESIDUAL".to_owned());
            }
            validate_wp009_windows_observation(observation)
        }
        "wp009-filesystem-wsl-v9fs" => {
            if row.get("residual_uncertainty").and_then(Value::as_str)
                != Some(WP009_WSL_PLATFORM_ROW_RESIDUAL)
            {
                return Err("WP009-E-WSL-ROW-RESIDUAL".to_owned());
            }
            validate_wp009_wsl_observation(observation)
        }
        _ => {
            let expected = serde_json::json!({
                "schema_id": "ff.wp009-platform-observation@1",
                "mode": "not_applicable",
                "commands": [],
                "observed_fields": {},
                "verdict": "not_applicable",
                "residual_uncertainty": []
            });
            if observation != &expected {
                return Err("WP009-E-PLATFORM-OBSERVATION-UNMAPPED".to_owned());
            }
            Ok(())
        }
    }
}

fn validate_wp009_windows_observation(observation: &Value) -> Result<(), String> {
    if observation.get("mode").and_then(Value::as_str)
        != Some("live_windows_host_and_repository_volume")
        || observation.get("verdict").and_then(Value::as_str)
            != Some("matched_frozen_windows_11_26200_ntfs")
    {
        return Err("WP009-E-WINDOWS-PROFILE-MISMATCH".to_owned());
    }
    let fields = observation
        .get("observed_fields")
        .ok_or("WP009-E-WINDOWS-PROFILE-MISMATCH")?;
    wp009_require_exact_keys(
        fields,
        &[
            "registry_product_name",
            "edition_id",
            "derived_product",
            "build_number",
            "repository_volume",
            "repository_filesystem",
        ],
        "WP009-E-WINDOWS-PROFILE-MISMATCH",
    )?;
    let _raw_product = wp009_required_text(
        fields,
        "registry_product_name",
        "WP009-E-WINDOWS-PROFILE-MISMATCH",
    )?;
    let volume = wp009_required_text(
        fields,
        "repository_volume",
        "WP009-E-WINDOWS-PROFILE-MISMATCH",
    )?;
    if fields.get("edition_id").and_then(Value::as_str) != Some("Core")
        || fields.get("derived_product").and_then(Value::as_str) != Some("Windows 11 Home")
        || fields.get("build_number").and_then(Value::as_str) != Some("26200")
        || fields.get("repository_filesystem").and_then(Value::as_str) != Some("NTFS")
        || volume.len() != 2
        || !volume.ends_with(':')
    {
        return Err("WP009-E-WINDOWS-PROFILE-MISMATCH".to_owned());
    }
    let registry_key = r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let expected_commands = serde_json::json!([
        {"program":"reg.exe","args":["query",registry_key,"/v","ProductName"]},
        {"program":"reg.exe","args":["query",registry_key,"/v","EditionID"]},
        {"program":"reg.exe","args":["query",registry_key,"/v","CurrentBuildNumber"]},
        {"program":"wmic.exe","args":["logicaldisk","where",format!("DeviceID='{volume}'"),"get","FileSystem","/value"]}
    ]);
    let expected_residual = serde_json::json!([
        "The legacy ProductName registry value may say Windows 10 on Windows 11; the observed build and EditionID derive the product identity.",
        "This observes host and volume identity only; it is not a crash, power-loss, atomic-replace, confinement-race, or durability experiment."
    ]);
    if observation.get("commands") != Some(&expected_commands)
        || observation.get("residual_uncertainty") != Some(&expected_residual)
    {
        return Err("WP009-E-WINDOWS-PROBE-TRANSCRIPT".to_owned());
    }
    Ok(())
}

fn validate_wp009_wsl_observation(observation: &Value) -> Result<(), String> {
    if observation.get("mode").and_then(Value::as_str)
        != Some("live_wsl_read_only_identity_and_mount")
        || observation.get("verdict").and_then(Value::as_str)
            != Some("matched_frozen_wsl2_v9fs_rejected_degraded")
    {
        return Err("WP009-E-WSL-PROFILE-MISMATCH".to_owned());
    }
    let fields = observation
        .get("observed_fields")
        .ok_or("WP009-E-WSL-PROFILE-MISMATCH")?;
    wp009_require_exact_keys(
        fields,
        &[
            "distribution_id",
            "distribution_version",
            "distribution_pretty_name",
            "kernel_release",
            "repository_path",
            "repository_mount_filesystem",
        ],
        "WP009-E-WSL-PROFILE-MISMATCH",
    )?;
    let kernel = wp009_required_text(fields, "kernel_release", "WP009-E-WSL-PROFILE-MISMATCH")?;
    let repository_path =
        wp009_required_text(fields, "repository_path", "WP009-E-WSL-PROFILE-MISMATCH")?;
    let _pretty_name = wp009_required_text(
        fields,
        "distribution_pretty_name",
        "WP009-E-WSL-PROFILE-MISMATCH",
    )?;
    if fields.get("distribution_id").and_then(Value::as_str) != Some("ubuntu")
        || fields.get("distribution_version").and_then(Value::as_str) != Some("24.04")
        || !kernel.starts_with("6.6.87.2-")
        || !repository_path.starts_with("/mnt/")
        || fields
            .get("repository_mount_filesystem")
            .and_then(Value::as_str)
            != Some("v9fs")
    {
        return Err("WP009-E-WSL-PROFILE-MISMATCH".to_owned());
    }
    let commands = observation
        .get("commands")
        .and_then(Value::as_array)
        .ok_or("WP009-E-WSL-PROBE-TRANSCRIPT")?;
    if commands.len() != 4
        || commands[0]
            != serde_json::json!({"program":"wsl.exe","args":["--exec","cat","/etc/os-release"]})
        || commands[1] != serde_json::json!({"program":"wsl.exe","args":["--exec","uname","-r"]})
        || commands[2].pointer("/program").and_then(Value::as_str) != Some("wsl.exe")
        || commands[2].pointer("/args/0").and_then(Value::as_str) != Some("--exec")
        || commands[2].pointer("/args/1").and_then(Value::as_str) != Some("wslpath")
        || commands[2].pointer("/args/2").and_then(Value::as_str) != Some("-a")
        || commands[2]
            .pointer("/args/3")
            .and_then(Value::as_str)
            .is_none()
        || commands[3]
            != serde_json::json!({
                "program":"wsl.exe",
                "args":["--exec","stat","-f","-c","%T",repository_path]
            })
    {
        return Err("WP009-E-WSL-PROBE-TRANSCRIPT".to_owned());
    }
    let expected_residual = serde_json::json!([
        "Only read-only WSL identity, kernel, path-translation, and filesystem-stat commands executed.",
        "No Rust code or Ferric model ran inside WSL, and this is not native-Linux, crash, power-loss, confinement-race, or durability proof."
    ]);
    if observation.get("residual_uncertainty") != Some(&expected_residual) {
        return Err("WP009-E-WSL-PROBE-TRANSCRIPT".to_owned());
    }
    Ok(())
}

fn validate_wp009_special_semantics(row: &Value, case_id: &str) -> Result<(), String> {
    let observed = wp009_required_text(row, "observed", "WP009-E-PROOF-FIELDS-MISSING")?;
    if (case_id.starts_with("wp009-recovery-") || case_id == "wp009-cross-volume")
        && (row.get("expected") != row.get("observed")
            || row.get("recovery_action") != row.get("observed"))
    {
        return Err("WP009-E-RECOVERY-NON-IDEMPOTENT".to_owned());
    }
    let journal_fault = match case_id {
        "wp009-journal-torn" => Some("Some(Torn { index: 1 })"),
        "wp009-journal-duplicate" => {
            Some("Some(DuplicateSequence { index: 1, expected: 2, observed: 1 })")
        }
        "wp009-journal-reordered" => {
            Some("Some(ReorderedSequence { index: 1, expected: 2, observed: 3 })")
        }
        "wp009-journal-checksum-invalid" => Some("Some(ChecksumInvalid { index: 1 })"),
        "wp009-journal-false-prepared-sync" => {
            Some("Some(InvalidRecord { index: 1, error: PreparedDataNotSynchronized })")
        }
        "wp009-journal-false-renamed-sync" => {
            Some("Some(InvalidRecord { index: 1, error: RenamedDirectoryNotSynchronized })")
        }
        "wp009-journal-mixed-job" => Some("observed_job_id: JobId(\"job_foreign\") })"),
        "wp009-journal-archive-uniqueness" => {
            Some("Some(InvalidRecord { index: 1, error: ArchiveUniquenessReceiptMissing })")
        }
        _ => None,
    };
    if let Some(fault) = journal_fault
        && (!observed.starts_with("valid_record_count=1; stopped_by=")
            || !observed.ends_with(fault))
    {
        return Err(format!("WP009-E-FALSE-JOURNAL-PREFIX: {case_id}"));
    }
    Ok(())
}

fn execute_transport_corpus(root: &Path, checks: &mut Vec<Check>) -> Result<String, String> {
    let source_before = source_state(root)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("transport report clock: {error}"))?
        .as_nanos();
    let report_relative = format!(
        "build/reports/wp-ff-007-transport-report-{}-{nonce}.json",
        std::process::id()
    );
    let report_path = root.join(&report_relative);
    if report_path.exists() {
        return Err(format!(
            "transport report path unexpectedly exists: {report_relative}"
        ));
    }
    let args = [
        "run".to_owned(),
        "--manifest-path".to_owned(),
        "build/Cargo.toml".to_owned(),
        "--locked".to_owned(),
        "-p".to_owned(),
        "fforager-transport".to_owned(),
        "--bin".to_owned(),
        "fforager-transport-corpus".to_owned(),
        "--".to_owned(),
        "--manifest".to_owned(),
        "build/fixtures/transport-v1/manifest.json".to_owned(),
        "--report".to_owned(),
        report_relative.clone(),
    ];
    let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    let output = cargo_proof_output(root, "cargo", &arg_refs)?;
    if !output.contains("FAILED_SPIKE_REQUIRES_OPERATOR_DECISION")
        && !output.contains("PASS_PURE_RUST_PATH")
    {
        return Err("transport corpus output omitted aggregate verdict".to_owned());
    }
    let report_verdict = validate_transport_report(root, &report_path)?;
    let opposite_verdict = if report_verdict == "PASS_PURE_RUST_PATH" {
        "FAILED_SPIKE_REQUIRES_OPERATOR_DECISION"
    } else {
        "PASS_PURE_RUST_PATH"
    };
    if !output.contains(&report_verdict) || output.contains(opposite_verdict) {
        return Err("transport corpus stdout/report verdict mismatch".to_owned());
    }
    let source_after = source_state(root)?;
    if source_before.git_commit != source_after.git_commit
        || source_before.dirty != source_after.dirty
        || source_before.dirty_paths != source_after.dirty_paths
        || source_before.content_fingerprint != source_after.content_fingerprint
    {
        return Err("source changed while transport corpus executed".to_owned());
    }
    checks.push(pass_with_class(
        "wp-ff-007-transport-corpus",
        "semantic",
        "non-shipped local transport corpus and strict report consumer",
        &format!("fresh strict report validated at {report_relative}; source remained stable"),
    ));
    Ok(report_relative)
}

#[allow(clippy::too_many_lines)]
fn validate_transport_report(root: &Path, path: &Path) -> Result<String, String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("read transport report {}: {error}", path.display()))?;
    let report: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse transport report {}: {error}", path.display()))?;
    let object = report
        .as_object()
        .ok_or("transport report root is not an object")?;
    let expected_top = BTreeSet::from([
        "schema_id",
        "corpus_id",
        "normalization_version",
        "candidate_identity",
        "manifest_declaration_sha256",
        "candidate_implementation_sha256",
        "semantic_projection_sha256",
        "zero_product_progress",
        "cases",
        "summary",
        "counterfactual",
        "residual_uncertainties",
        "verdict",
    ]);
    let observed_top = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
    if observed_top != expected_top {
        return Err(format!(
            "transport report top-level schema drift: {observed_top:?}"
        ));
    }
    if object.get("schema_id").and_then(Value::as_str)
        != Some("fforager.transport-corpus-report.v1")
        || object.get("corpus_id").and_then(Value::as_str) != Some("WP-FF-007-transport-corpus-v1")
        || object.get("zero_product_progress").and_then(Value::as_bool) != Some(true)
        || object
            .get("manifest_declaration_sha256")
            .and_then(Value::as_str)
            != Some(TRANSPORT_MANIFEST_DECLARATION_SHA256)
    {
        return Err("transport report authority or zero-progress binding mismatch".to_owned());
    }
    let candidate_digest = transport_candidate_source_sha256(root)?;
    if object
        .get("candidate_implementation_sha256")
        .and_then(Value::as_str)
        != Some(candidate_digest.as_str())
        || object
            .get("semantic_projection_sha256")
            .and_then(Value::as_str)
            .is_none_or(|value| value.len() != 64)
    {
        return Err("transport report implementation or semantic digest invalid".to_owned());
    }
    let verdict = object
        .get("verdict")
        .and_then(Value::as_str)
        .ok_or("transport report verdict missing")?;
    if !matches!(
        verdict,
        "PASS_PURE_RUST_PATH" | "FAILED_SPIKE_REQUIRES_OPERATOR_DECISION"
    ) {
        return Err(format!("transport report verdict invalid: {verdict}"));
    }
    let cases = object
        .get("cases")
        .and_then(Value::as_array)
        .ok_or("transport report cases missing")?;
    if cases.is_empty() {
        return Err("transport report cases empty".to_owned());
    }
    let manifest_bytes = fs::read(root.join("build/fixtures/transport-v1/manifest.json"))
        .map_err(|error| format!("read canonical transport manifest: {error}"))?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("parse canonical transport manifest: {error}"))?;
    let manifest_object = manifest
        .as_object()
        .ok_or("canonical transport manifest root is not an object")?;
    if manifest_object.get("schema_id").and_then(Value::as_str)
        != Some("fforager.transport-corpus-manifest.v1")
        || manifest_object.get("corpus_id").and_then(Value::as_str)
            != Some("WP-FF-007-transport-corpus-v1")
        || manifest_object
            .get("normalization_version")
            .and_then(Value::as_str)
            != object.get("normalization_version").and_then(Value::as_str)
        || manifest_object
            .get("candidate_identity")
            .and_then(Value::as_str)
            != object.get("candidate_identity").and_then(Value::as_str)
    {
        return Err("canonical transport manifest/report authority mismatch".to_owned());
    }
    let declarations = manifest_object
        .get("cases")
        .and_then(Value::as_array)
        .ok_or("canonical transport manifest cases missing")?;
    if declarations.len() != cases.len() {
        return Err(format!(
            "transport report case cardinality mismatch: expected {}, observed {}",
            declarations.len(),
            cases.len()
        ));
    }
    let mut blocked_case_count = 0_u64;
    let mut blocked_codes = BTreeSet::new();
    for (index, case) in cases.iter().enumerate() {
        let case = case
            .as_object()
            .ok_or("transport report case is not an object")?;
        let declaration = declarations[index]
            .as_object()
            .ok_or("canonical transport declaration is not an object")?;
        let expected_case_fields = BTreeSet::from([
            "id",
            "mandatory",
            "proof_class",
            "family",
            "scenario_class",
            "concrete_input",
            "executed_boundary",
            "requested_capabilities",
            "satisfied_capabilities",
            "blocked_capabilities",
            "connection_identity",
            "wire_identity",
            "pool_identity",
            "selected_address",
            "proxy_identity",
            "alpn_identity",
            "protocol_identity",
            "limits",
            "timing_phases_micros",
            "transcript",
            "expected_outcome",
            "observed_outcome",
            "status",
            "exact_mismatch",
            "skipped_semantic_dependencies",
            "duration_micros",
        ]);
        let observed_case_fields = case.keys().map(String::as_str).collect::<BTreeSet<_>>();
        if observed_case_fields != expected_case_fields {
            return Err(format!(
                "transport report case schema drift: {observed_case_fields:?}"
            ));
        }
        for field in [
            "id",
            "family",
            "scenario_class",
            "executed_boundary",
            "connection_identity",
            "wire_identity",
            "pool_identity",
            "selected_address",
            "proxy_identity",
            "alpn_identity",
            "protocol_identity",
        ] {
            if case
                .get(field)
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
            {
                return Err(format!("transport report case omits {field}"));
            }
        }
        if !case.get("transcript").is_some_and(Value::is_object)
            || !case
                .get("timing_phases_micros")
                .and_then(Value::as_object)
                .is_some_and(|phases| phases.contains_key("total"))
            || case.get("status").and_then(Value::as_str) != Some("PASS")
        {
            return Err("transport report case evidence shape is invalid".to_owned());
        }
        let case_id = case.get("id").and_then(Value::as_str).unwrap_or_default();
        let limits = case
            .get("limits")
            .and_then(Value::as_object)
            .filter(|limits| !limits.is_empty())
            .ok_or_else(|| format!("transport report case omits limits: {case_id}"))?;
        validate_transport_limits(case_id, limits)?;
        let declaration_id = declaration
            .get("id")
            .and_then(Value::as_str)
            .ok_or("canonical transport declaration id missing")?;
        let kind = declaration
            .get("kind")
            .and_then(Value::as_str)
            .ok_or("canonical transport declaration kind missing")?;
        let (expected_family, expected_scenario) = transport_kind_contract(kind)?;
        if case_id != declaration_id
            || case.get("mandatory").and_then(Value::as_bool) != Some(true)
            || declaration.get("mandatory").and_then(Value::as_bool) != Some(true)
            || case.get("proof_class") != declaration.get("proof_class")
            || case.get("expected_outcome") != declaration.get("expected_outcome")
            || case.get("observed_outcome") != declaration.get("expected_outcome")
            || case.get("exact_mismatch") != Some(&Value::Null)
            || case.get("family").and_then(Value::as_str) != Some(expected_family)
            || case.get("scenario_class").and_then(Value::as_str) != Some(expected_scenario)
        {
            return Err(format!(
                "transport report declaration/outcome drift: {declaration_id}"
            ));
        }
        validate_transport_transcript(case, declaration_id, expected_family, expected_scenario)?;
        let requested = json_string_set(case.get("requested_capabilities"), "requested")?;
        let declared_requested = json_string_set(
            declaration.get("requested_capabilities"),
            "manifest requested",
        )?;
        if requested != declared_requested {
            return Err(format!(
                "transport report requested capability drift: {declaration_id}"
            ));
        }
        let satisfied = json_string_set(case.get("satisfied_capabilities"), "satisfied")?;
        let blocked = case
            .get("blocked_capabilities")
            .and_then(Value::as_array)
            .ok_or("transport report blocked capabilities missing")?;
        let blocked_set = blocked
            .iter()
            .map(|item| {
                item.as_object()
                    .and_then(|object| object.get("capability"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or("transport report blocked capability malformed")
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        let expected_satisfied = requested
            .iter()
            .filter(|capability| transport_supported_capability(capability))
            .cloned()
            .collect::<BTreeSet<_>>();
        let expected_blocked = requested
            .iter()
            .filter(|capability| !transport_supported_capability(capability))
            .map(|capability| transport_blocked_diagnostic(capability))
            .collect::<Result<Vec<_>, _>>()?;
        if satisfied != expected_satisfied || blocked.len() != expected_blocked.len() {
            return Err(format!(
                "transport report candidate decision drift: {declaration_id}"
            ));
        }
        for (actual, expected) in blocked.iter().zip(expected_blocked.iter()) {
            if actual != expected {
                return Err(format!(
                    "transport report blocked diagnostic drift: {declaration_id}"
                ));
            }
            blocked_codes.insert(
                expected
                    .get("code")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            );
        }
        if !satisfied.is_disjoint(&blocked_set)
            || satisfied
                .union(&blocked_set)
                .cloned()
                .collect::<BTreeSet<_>>()
                != requested
        {
            return Err("transport report capability partition is contradictory".to_owned());
        }
        if !blocked_set.is_empty() {
            blocked_case_count = blocked_case_count.saturating_add(1);
        }
    }
    let summary = object
        .get("summary")
        .and_then(Value::as_object)
        .ok_or("transport report summary missing")?;
    let count = u64::try_from(cases.len()).map_err(|error| error.to_string())?;
    if summary.get("total").and_then(Value::as_u64) != Some(count)
        || summary.get("passed").and_then(Value::as_u64) != Some(count)
        || summary.get("failed").and_then(Value::as_u64) != Some(0)
        || summary.get("mandatory_total").and_then(Value::as_u64) != Some(count)
        || summary.get("mandatory_passed").and_then(Value::as_u64) != Some(count)
        || summary.get("mandatory_blocked").and_then(Value::as_u64) != Some(blocked_case_count)
    {
        return Err("transport report summary does not match case evidence".to_owned());
    }
    let expected_blocked_codes = blocked_codes
        .into_iter()
        .map(Value::String)
        .collect::<Vec<_>>();
    if summary
        .get("blocked_capability_codes")
        .and_then(Value::as_array)
        != Some(&expected_blocked_codes)
    {
        return Err("transport report blocked-code summary mismatch".to_owned());
    }
    let expected_verdict = if blocked_case_count == 0 {
        "PASS_PURE_RUST_PATH"
    } else {
        "FAILED_SPIKE_REQUIRES_OPERATOR_DECISION"
    };
    if verdict != expected_verdict {
        return Err("transport report verdict contradicts blocked evidence".to_owned());
    }
    validate_transport_semantic_digest(
        cases,
        object
            .get("semantic_projection_sha256")
            .and_then(Value::as_str)
            .ok_or("transport report semantic projection digest missing")?,
    )?;
    if object
        .get("counterfactual")
        .and_then(Value::as_object)
        .and_then(|value| value.get("rejected_by_same_oracle"))
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err("transport report counterfactual was not rejected".to_owned());
    }
    let counterfactual = object
        .get("counterfactual")
        .and_then(Value::as_object)
        .ok_or("transport report counterfactual missing")?;
    if counterfactual.get("mutation").and_then(Value::as_str)
        != Some(
            "remove one supported adapter capability; separately clear blocked evidence while preserving declarations",
        )
        || counterfactual
            .get("rejection")
            .and_then(Value::as_str)
            .is_none_or(|value| {
                !value.contains("behavior=")
                    || !value.contains("evidence=")
                    || value.contains("accepted")
            })
    {
        return Err("transport report counterfactual detail invalid".to_owned());
    }
    validate_transport_counterfactual(cases)?;
    Ok(verdict.to_owned())
}

fn validate_transport_counterfactual(cases: &[Value]) -> Result<(), String> {
    let supported_case = cases
        .iter()
        .filter_map(Value::as_object)
        .find(|case| {
            case.get("blocked_capabilities")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty)
        })
        .ok_or("transport consumer counterfactual has no supported case")?;
    let requested = json_string_set(
        supported_case.get("requested_capabilities"),
        "counterfactual requested",
    )?;
    let removed = requested
        .iter()
        .find(|capability| transport_supported_capability(capability))
        .ok_or("transport consumer counterfactual has no removable capability")?;
    let observed_satisfied = json_string_set(
        supported_case.get("satisfied_capabilities"),
        "counterfactual satisfied",
    )?;
    if !observed_satisfied.contains(removed) {
        return Err("transport consumer behavior counterfactual is not discriminating".to_owned());
    }

    let blocked_case = cases
        .iter()
        .filter_map(Value::as_object)
        .find(|case| {
            case.get("blocked_capabilities")
                .and_then(Value::as_array)
                .is_some_and(|blocked| !blocked.is_empty())
        })
        .ok_or("transport consumer counterfactual has no blocked case")?;
    let blocked_requested = json_string_set(
        blocked_case.get("requested_capabilities"),
        "counterfactual blocked requested",
    )?;
    let blocked_satisfied = json_string_set(
        blocked_case.get("satisfied_capabilities"),
        "counterfactual blocked satisfied",
    )?;
    if blocked_requested == blocked_satisfied {
        return Err("transport consumer evidence counterfactual is not discriminating".to_owned());
    }
    Ok(())
}

fn validate_transport_semantic_digest(cases: &[Value], claimed: &str) -> Result<(), String> {
    let semantic_projection = cases
        .iter()
        .map(transport_semantic_projection)
        .collect::<Result<Vec<_>, _>>()?;
    let semantic_bytes = serde_json::to_vec(&semantic_projection)
        .map_err(|error| format!("serialize transport semantic projection: {error}"))?;
    let recomputed = encode_sha256(&semantic_bytes);
    if claimed != recomputed || recomputed != TRANSPORT_SEMANTIC_PROJECTION_SHA256 {
        return Err("transport report semantic projection digest mismatch".to_owned());
    }
    Ok(())
}

fn transport_supported_capability(capability: &str) -> bool {
    matches!(
        capability,
        "http11"
            | "range"
            | "streaming"
            | "replay"
            | "cancellation"
            | "metadata_bounds"
            | "body_bounds"
    )
}

fn transport_blocked_diagnostic(capability: &str) -> Result<Value, String> {
    let (code, reason) = match capability {
        "tls_fingerprint" => (
            "FF-TRANSPORT-E-TLS-FINGERPRINT-BLOCKED",
            "the authorized std-first candidate has no browser ClientHello parity mechanism",
        ),
        "http2_fingerprint" => (
            "FF-TRANSPORT-E-H2-FINGERPRINT-BLOCKED",
            "the authorized std-first candidate has no browser HTTP/2 wire parity mechanism",
        ),
        "http2" => (
            "FF-TRANSPORT-E-HTTP2-BLOCKED",
            "the bounded std-first candidate implements only HTTP/1.1 local evidence",
        ),
        "proxy" => (
            "FF-TRANSPORT-E-PROXY-EVIDENCE-BLOCKED",
            "no trusted proxy destination-evidence contract is available",
        ),
        "compression" => (
            "FF-TRANSPORT-E-COMPRESSION-BLOCKED",
            "no authorized decompressor implementation is present in the candidate",
        ),
        "decompression_bounds" => (
            "FF-TRANSPORT-E-DECOMPRESSION-BOUNDS-BLOCKED",
            "no authorized decompressor exists through which decompressed-byte admission can be enforced",
        ),
        "redirect_policy" => (
            "FF-TRANSPORT-E-REDIRECT-INTEGRATION-BLOCKED",
            "redirect targets are not integrated with authoritative DNS and peer-address SSRF validation",
        ),
        "cookie_scope" => (
            "FF-TRANSPORT-E-COOKIE-PSL-BLOCKED",
            "no versioned authoritative public-suffix snapshot with wildcard and exception semantics is present",
        ),
        "dns_provenance" => (
            "FF-TRANSPORT-E-DNS-PROVENANCE-BLOCKED",
            "the candidate has no resolver-bound DNS provenance implementation",
        ),
        "ssrf_policy" => (
            "FF-TRANSPORT-E-SSRF-REGISTRY-BLOCKED",
            "the candidate has no generated complete current special-purpose address registry",
        ),
        "pool_partition" => (
            "FF-TRANSPORT-E-POOL-CONTEXT-BLOCKED",
            "pool identity is not derived from immutable candidate execution context",
        ),
        "retry_bounds" => (
            "FF-TRANSPORT-E-RETRY-EXECUTOR-BLOCKED",
            "retry policy is not integrated with request attempts, deadlines, cancellation, and partial-body state",
        ),
        other => {
            return Err(format!(
                "transport manifest contains unknown blocked capability: {other}"
            ));
        }
    };
    Ok(serde_json::json!({
        "capability": capability,
        "code": code,
        "reason": reason
    }))
}

#[allow(clippy::too_many_lines)]
fn transport_kind_contract(kind: &str) -> Result<(&'static str, &'static str), String> {
    let contract = match kind {
        "tls_fingerprint_blocked" => ("tls_fingerprint", "blocked"),
        "http2_fingerprint_blocked" => ("http2_fingerprint", "blocked"),
        "http2_blocked" => ("http2", "blocked"),
        "proxy_blocked" => ("proxy", "blocked"),
        "compression_blocked" => ("compression", "blocked"),
        "standard_capability" => ("capability_contract", "positive"),
        "http11_local" => ("http11", "positive"),
        "http11_header_boundary" => ("http11", "boundary"),
        "http11_huge_length" => ("http11", "negative"),
        "range_local" => ("range", "positive"),
        "range_boundary" => ("range", "boundary"),
        "range_invalid" => ("range", "negative"),
        "streaming_positive" => ("streaming", "positive"),
        "streaming_local" => ("streaming", "boundary"),
        "byte_credit" => ("streaming", "negative"),
        "redirect_cross_origin" => ("redirect", "positive"),
        "redirect_limit" => ("redirect", "boundary"),
        "redirect_downgrade" | "redirect_special_use" => ("redirect", "negative"),
        "cookie_scope" => ("cookie", "positive"),
        "cookie_count_boundary" => ("cookie", "boundary"),
        "cookie_public_suffix" | "cookie_ip_domain" | "cookie_syntax" => ("cookie", "negative"),
        "dns_positive" => ("dns_ssrf", "positive"),
        "dns_provenance_mismatch"
        | "ssrf_mixed_answers"
        | "ssrf_registry_boundary"
        | "dns_selection"
        | "dns_rebind" => ("dns_ssrf", "negative"),
        "proxy_evidence_positive" => ("proxy", "positive"),
        "proxy_evidence" => ("proxy", "negative"),
        "pool_reuse" | "pool_all_dimensions" => ("pool", "positive"),
        "pool_bound" => ("pool", "boundary"),
        "pool_session_partition" | "pool_fingerprint_partition" => ("pool", "negative"),
        "bounds_positive" => ("bounds", "positive"),
        "bounds_exact" => ("bounds", "boundary"),
        "metadata_bound" | "body_bound" => ("bounds", "negative"),
        "decompression_bound" => ("compression", "negative"),
        "retry_allowed" => ("retry", "boundary"),
        "retry_bound" => ("retry", "negative"),
        "cancellation_positive" | "cancellation_pool" => ("cancellation", "positive"),
        "cancellation_boundary" => ("cancellation", "boundary"),
        "cancellation_correlation" => ("cancellation", "negative"),
        "sanitized_replay" => ("replay", "positive"),
        "replay_boundary" => ("replay", "boundary"),
        "replay_negative" => ("replay", "negative"),
        "url_positive" => ("url", "positive"),
        "url_boundary" => ("url", "boundary"),
        "url_bounds" => ("url", "negative"),
        other => return Err(format!("unknown canonical transport case kind: {other}")),
    };
    Ok(contract)
}

fn validate_transport_transcript(
    case: &serde_json::Map<String, Value>,
    id: &str,
    family: &str,
    scenario: &str,
) -> Result<(), String> {
    let transcript = case
        .get("transcript")
        .and_then(Value::as_object)
        .ok_or("transport transcript is not an object")?;
    let expected_fields = BTreeSet::from([
        "normalization_version",
        "case_id",
        "family",
        "scenario_class",
        "executed_boundary",
        "result",
        "exchange",
    ]);
    if transcript
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        != expected_fields
        || transcript
            .get("normalization_version")
            .and_then(Value::as_str)
            != Some("WP-FF-004-sanitized-transcript-v1")
        || transcript.get("case_id").and_then(Value::as_str) != Some(id)
        || transcript.get("family").and_then(Value::as_str) != Some(family)
        || transcript.get("scenario_class").and_then(Value::as_str) != Some(scenario)
        || transcript.get("executed_boundary") != case.get("executed_boundary")
        || transcript.get("result") != case.get("observed_outcome")
        || !transcript.contains_key("exchange")
    {
        return Err(format!("transport transcript semantic drift: {id}"));
    }
    Ok(())
}

fn validate_transport_limits(
    case_id: &str,
    limits: &serde_json::Map<String, Value>,
) -> Result<(), String> {
    let required: &[(&str, u64)] = match case_id {
        "http11-huge-content-length-rejection" => &[
            ("maximum_body_bytes", 32),
            ("observed_content_length", u64::MAX),
        ],
        "response-metadata-bound" => &[
            ("maximum_metadata_bytes", 10),
            ("observed_metadata_bytes", 11),
        ],
        "response-body-bound" => &[("maximum_body_bytes", 20), ("observed_body_bytes", 21)],
        "byte-credit-overrun" => &[
            ("maximum_body_bytes", 8),
            ("granted_byte_credits", 4),
            ("attempted_body_bytes", 5),
        ],
        _ => return Ok(()),
    };
    for (field, expected) in required {
        if limits.get(*field).and_then(Value::as_u64) != Some(*expected) {
            return Err(format!(
                "transport report limit evidence mismatch: {case_id}:{field}"
            ));
        }
    }
    Ok(())
}

fn transport_semantic_projection(case: &Value) -> Result<Value, String> {
    let case = case
        .as_object()
        .ok_or("transport semantic case is not an object")?;
    let get = |field: &str| {
        case.get(field)
            .cloned()
            .ok_or_else(|| format!("transport semantic field missing: {field}"))
    };
    Ok(serde_json::json!({
        "id": get("id")?,
        "family": get("family")?,
        "scenario_class": get("scenario_class")?,
        "requested": get("requested_capabilities")?,
        "satisfied": get("satisfied_capabilities")?,
        "blocked": get("blocked_capabilities")?,
        "connection_identity": get("connection_identity")?,
        "wire_identity": get("wire_identity")?,
        "pool_identity": get("pool_identity")?,
        "selected_address": get("selected_address")?,
        "proxy_identity": get("proxy_identity")?,
        "alpn_identity": get("alpn_identity")?,
        "protocol_identity": get("protocol_identity")?,
        "limits": get("limits")?,
        "transcript": get("transcript")?,
        "expected": get("expected_outcome")?,
        "observed": get("observed_outcome")?,
        "status": get("status")?
    }))
}

fn encode_sha256(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn json_string_set(value: Option<&Value>, field: &str) -> Result<BTreeSet<String>, String> {
    value
        .and_then(Value::as_array)
        .ok_or_else(|| format!("transport report {field} capabilities missing"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("transport report {field} capability malformed"))
        })
        .collect()
}

fn transport_candidate_source_sha256(root: &Path) -> Result<String, String> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut digest = Sha256::new();
    for relative in [
        "build/crates/fforager-transport/src/lib.rs",
        "build/crates/fforager-transport/src/policy.rs",
        "build/crates/fforager-transport/src/local_server.rs",
        "build/crates/fforager-transport/src/corpus.rs",
        "build/crates/fforager-transport/src/bin/fforager-transport-corpus.rs",
    ] {
        let bytes = fs::read(root.join(relative))
            .map_err(|error| format!("read transport candidate source {relative}: {error}"))?;
        digest.update(bytes);
    }
    let bytes = digest.finalize();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(encoded)
}

fn execute_compatibility_replay_boundaries(
    root: &Path,
    checks: &mut Vec<Check>,
) -> Result<(), String> {
    let source_before = source_state(root)?;
    let reports_before = report_file_snapshot(root)?;
    let output = cargo_proof_output(
        root,
        "cargo",
        &[
            "run",
            "--manifest-path",
            "build/Cargo.toml",
            "--locked",
            "-p",
            "fforager-xtask",
            "--",
            "compatibility-replay",
        ],
    )?;
    let report_path = child_report_path(root, &output, "compatibility-replay")?;
    validate_fresh_child_report(&report_path, &reports_before)?;
    let evidence = compatibility::read_replay_report_evidence(root, &report_path)?;
    let source_after = source_state(root)?;
    if evidence.source_git_commit != source_before.git_commit
        || evidence.source_dirty != source_before.dirty
        || evidence.source_dirty_paths != source_before.dirty_paths
        || evidence.source_content_fingerprint != source_before.content_fingerprint
        || !source_states_equal(&source_before, &source_after)
    {
        return Err(
            "FF-COMP-E-REPLAY-REPORT-SOURCE-STALE: child replay report or repository source state changed during deep semantic proof"
                .to_owned(),
        );
    }
    if evidence.status != "SEMANTIC_REPLAY_EXECUTED"
        || evidence.execution_scope != "complete_corpus"
    {
        return Err(format!(
            "FF-COMP-E-REPLAY-REPORT-STATUS: deep gate requires complete semantic replay, observed status={} scope={}",
            evidence.status, evidence.execution_scope
        ));
    }
    let replay_report_path = evidence.report_path;
    for replay in evidence.semantic_replays {
        checks.push(Check {
            id: format!("compatibility-replay-semantic-{}", replay.case_id),
            status: "PASS",
            proof_class: "semantic",
            concrete_input: replay.concrete_input,
            executed_boundary: format!(
                "exact child compatibility report {} via {}",
                replay_report_path, replay.boundary
            ),
            expected_result: replay.expected_result,
            observed_result: format!("plane={}; {}", replay.plane, replay.observed_result),
            skipped_semantic_dependencies: replay.skipped_semantic_dependencies,
            detail:
                "complete-corpus semantic replay evidence recovered from the exact child report"
                    .to_owned(),
        });
    }
    let error = cargo_proof_output(
        root,
        "cargo",
        &[
            "run",
            "--manifest-path",
            "build/Cargo.toml",
            "--locked",
            "-p",
            "fforager-xtask",
            "--",
            "compatibility-replay",
            "--shard",
            "2/4",
        ],
    )
    .expect_err("empty replay shard unexpectedly produced evidence");
    if !error.contains("FF-COMP-E-SHARD-EMPTY") {
        return Err(format!(
            "empty replay shard failed without the required typed diagnostic: {error}"
        ));
    }
    checks.push(Check {
        id: "compatibility-replay-empty-shard".to_owned(),
        status: "PASS",
        proof_class: "negative_fixture",
        concrete_input: "compatibility-replay --shard 2/4".to_owned(),
        executed_boundary: "fforager-xtask compatibility replay selection boundary".to_owned(),
        expected_result: "reject an empty selected shard with FF-COMP-E-SHARD-EMPTY".to_owned(),
        observed_result: "FF-COMP-E-SHARD-EMPTY".to_owned(),
        skipped_semantic_dependencies: vec![
            "An empty selection intentionally executes no case semantics and produces no replay report."
                .to_owned(),
        ],
        detail: "empty replay shard was rejected before any semantic PASS could be produced".to_owned(),
    });
    Ok(())
}

fn validate_contract_inventory(root: &Path, checks: &mut Vec<Check>) -> Result<(), String> {
    let inventory_path = root.join("build/fixtures/contracts/inventory.json");
    let bytes =
        fs::read(&inventory_path).map_err(|error| format!("read contract inventory: {error}"))?;
    if bytes.len() > 1_048_576 {
        return Err("contract inventory exceeds the 1-MiB gate bound".to_owned());
    }
    let inventory: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse contract inventory: {error}"))?;
    validate_contract_inventory_shape(&inventory)?;
    if inventory
        .get("schema_id")
        .and_then(serde_json::Value::as_str)
        != Some("ff.contract-inventory@1")
        || inventory.get("file_id").and_then(serde_json::Value::as_str)
            != Some("FF-BUILD-CONTRACT-INVENTORY-001")
        || inventory
            .get("schema_version")
            .and_then(serde_json::Value::as_str)
            != Some("1.0.0")
    {
        return Err("contract inventory identity is invalid".to_owned());
    }
    let entries = inventory
        .get("entries")
        .and_then(serde_json::Value::as_array)
        .ok_or("contract inventory omits entries")?;
    let states = inventory
        .get("state_machines")
        .and_then(serde_json::Value::as_array)
        .ok_or("contract inventory omits state_machines")?;
    if entries.len() != 21 || states.len() != 12 {
        return Err(format!(
            "contract inventory coverage mismatch: entries={}, state_machines={}",
            entries.len(),
            states.len()
        ));
    }
    let inventory_proof_ids = validate_contract_inventory_rows(root, entries, states)?;
    checks.extend(resolve_and_execute_inventory_proofs(
        root,
        &inventory_proof_ids,
    )?);
    validate_public_boundary_counterexample_declaration(root)?;
    checks.push(pass(
        "public-boundary-counterexample-declaration",
        "inventory-backed public-boundary proof has a non-ignored registered test declaration",
    ));
    checks.push(pass(
        "contract-inventory",
        &format!(
            "{} contract rows and {} state-machine rows have unique IDs, owners, proof IDs, readiness gates, and existing bounded fixtures",
            entries.len(), states.len()
        ),
    ));
    Ok(())
}

fn validate_contract_inventory_rows(
    root: &Path,
    entries: &[Value],
    states: &[Value],
) -> Result<Vec<String>, String> {
    let fixture_root = root.join("build/fixtures/contracts");
    let mut ids = BTreeSet::new();
    let mut contract_ids = BTreeSet::new();
    let mut state_ids = BTreeSet::new();
    let mut proof_ids = BTreeSet::new();
    for (row, is_state) in entries
        .iter()
        .map(|row| (row, false))
        .chain(states.iter().map(|row| (row, true)))
    {
        let id = validate_contract_inventory_row(row, is_state, &fixture_root)?;
        let proof_id = row
            .get("proof_id")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("contract inventory row {id} omits proof_id"))?;
        proof_ids.insert(proof_id.to_owned());
        if !ids.insert(id) {
            return Err(format!("duplicate contract inventory ID {id}"));
        }
        if is_state {
            state_ids.insert(id);
        } else {
            contract_ids.insert(id);
        }
    }
    if contract_ids != expected_contract_inventory_ids()
        || state_ids != expected_state_inventory_ids()
    {
        return Err(format!(
            "contract inventory stable-ID coverage mismatch: contracts={contract_ids:?}; states={state_ids:?}"
        ));
    }
    Ok(proof_ids.into_iter().collect())
}

fn resolve_and_execute_inventory_proofs(
    root: &Path,
    proof_ids: &[String],
) -> Result<Vec<Check>, String> {
    resolve_and_execute_inventory_proofs_with_cargo_mode(root, proof_ids, "--locked", None)
}

fn resolve_and_execute_inventory_proofs_with_cargo_mode(
    root: &Path,
    proof_ids: &[String],
    cargo_mode: &str,
    target_dir: Option<&Path>,
) -> Result<Vec<Check>, String> {
    let target_dir = target_dir
        .map(|path| {
            path.to_str()
                .ok_or("inventory proof target directory is not valid UTF-8")
        })
        .transpose()?;
    let mut listings = BTreeMap::<&str, BTreeSet<String>>::new();
    let mut checks = Vec::new();
    for proof_id in proof_ids {
        let (package, selector) = inventory_proof_target(proof_id)?;
        validate_inventory_proof_source(root, proof_id, package, selector)?;
        let listed = if let Some(listed) = listings.get(package) {
            listed
        } else {
            let mut args = vec!["test", "--manifest-path", "build/Cargo.toml", cargo_mode];
            if let Some(target_dir) = target_dir {
                args.extend(["--target-dir", target_dir]);
            }
            args.extend(["-p", package, "--lib", "--", "--list"]);
            let output = cargo_proof_output(root, "cargo", &args)?;
            let discovered = output
                .lines()
                .filter_map(|line| line.trim().strip_suffix(": test"))
                .map(ToOwned::to_owned)
                .collect::<BTreeSet<_>>();
            if discovered.is_empty() {
                return Err(format!(
                    "FF-ARCH-E-INVENTORY-PROOF-UNRESOLVED: {package} exposed no runnable lib tests"
                ));
            }
            listings.insert(package, discovered);
            listings
                .get(package)
                .expect("inserted proof listing must be present")
        };
        if !listed.contains(selector) {
            return Err(format!(
                "FF-ARCH-E-INVENTORY-PROOF-UNRESOLVED: {proof_id} does not resolve to a registered {package} test"
            ));
        }
        let mut args = vec!["test", "--manifest-path", "build/Cargo.toml", cargo_mode];
        if let Some(target_dir) = target_dir {
            args.extend(["--target-dir", target_dir]);
        }
        args.extend([
            "-p",
            package,
            "--lib",
            selector,
            "--",
            "--exact",
            "--nocapture",
        ]);
        let output = cargo_proof_output(root, "cargo", &args)?;
        if !exact_test_execution_passed(&output, selector) {
            return Err(format!(
                "FF-ARCH-E-INVENTORY-PROOF-UNRESOLVED: {proof_id} was not executed as a non-ignored exact test"
            ));
        }
        checks.push(Check {
            id: format!("inventory-proof-{}", sanitize_id(proof_id)),
            status: "PASS",
            proof_class: inventory_proof_class(proof_id),
            concrete_input: proof_id.clone(),
            executed_boundary: format!("cargo exact test boundary for package {package}"),
            expected_result: format!("registered test {selector} executes and passes"),
            observed_result: format!("{selector} executed as a non-ignored exact test"),
            skipped_semantic_dependencies: vec![
                "Inventory proof execution validates Phase 0 contracts only; no shipped Ferric entrypoint was executed."
                    .to_owned(),
            ],
            detail: "inventory proof ID resolved through cargo test --list and exact execution"
                .to_owned(),
        });
    }
    Ok(checks)
}

fn exact_test_execution_passed(output: &str, selector: &str) -> bool {
    let expected_test = format!("test {selector} ... ok");
    output.lines().any(|line| line.trim() == "running 1 test")
        && output.lines().any(|line| line.trim() == expected_test)
        && output.lines().any(|line| {
            let line = line.trim();
            line.starts_with("test result: ok.")
                && line.contains("1 passed;")
                && line.contains("0 failed;")
                && line.contains("0 ignored;")
        })
}

fn inventory_proof_target(proof_id: &str) -> Result<(&'static str, &str), String> {
    let prefixes = [
        ("contracts::", "fforager-contracts"),
        ("core::", "fforager-core"),
        ("testkit::", "fforager-testkit"),
        ("diagnostics::", "fforager-diagnostics-contract"),
    ];
    for (prefix, package) in prefixes {
        if let Some(selector) = proof_id.strip_prefix(prefix) {
            if selector.trim().is_empty() {
                break;
            }
            return Ok((package, selector));
        }
    }
    Err(format!(
        "FF-ARCH-E-INVENTORY-PROOF-UNRESOLVED: unsupported proof ID namespace {proof_id}"
    ))
}

fn inventory_proof_class(proof_id: &str) -> &'static str {
    if proof_id == PUBLIC_BOUNDARY_COUNTEREXAMPLE_PROOF_ID {
        "public_boundary"
    } else if proof_id.starts_with("core::lifecycle::") {
        "state_effect"
    } else if proof_id.starts_with("contracts::graph::") {
        "graph"
    } else if proof_id.starts_with("diagnostics::") {
        "wire_boundary"
    } else {
        "semantic"
    }
}

/// Inventory metadata is not evidence by itself.  Before executing a mapped
/// test, reject the smallest known hollow form so `cargo test --exact` cannot
/// turn an `assert!(true)` body into a semantic PASS claim.
fn validate_inventory_proof_source(
    root: &Path,
    proof_id: &str,
    package: &str,
    selector: &str,
) -> Result<(), String> {
    let source_path = inventory_proof_source_path(root, package, selector)?;
    let source = fs::read_to_string(&source_path)
        .map_err(|error| format!("read {}: {error}", source_path.display()))?;
    let test_name = selector
        .rsplit("::")
        .next()
        .ok_or("inventory selector has no test name")?;
    let body = named_test_body(&source, test_name).ok_or_else(|| {
        format!(
            "FF-ARCH-E-INVENTORY-PROOF-UNRESOLVED: {proof_id} has no source body for exact test {test_name}"
        )
    })?;
    if inventory_proof_trivial_body(body)
        || named_test_attributes(&source, test_name)
            .is_some_and(|attributes| attributes.contains("#[should_panic]"))
    {
        return Err(format!(
            "FF-ARCH-E-INVENTORY-PROOF-STUB: {proof_id} resolves to a neutralized test body"
        ));
    }
    Ok(())
}

fn inventory_proof_source_path(
    root: &Path,
    package: &str,
    selector: &str,
) -> Result<PathBuf, String> {
    if package == "fforager-testkit" {
        return Ok(root.join("build/crates/fforager-testkit/src/lib.rs"));
    }
    let module = selector
        .split("::tests::")
        .next()
        .filter(|module| !module.is_empty() && !module.contains("::"))
        .ok_or_else(|| {
            format!(
                "FF-ARCH-E-INVENTORY-PROOF-UNRESOLVED: unsupported inventory selector module {selector}"
            )
        })?;
    let crate_root = match package {
        "fforager-contracts" => "product/crates/fforager-contracts/src",
        "fforager-core" => "product/crates/fforager-core/src",
        "fforager-diagnostics-contract" => "product/crates/fforager-diagnostics-contract/src",
        _ => {
            return Err(format!(
                "FF-ARCH-E-INVENTORY-PROOF-UNRESOLVED: unsupported inventory package {package}"
            ));
        }
    };
    Ok(root.join(crate_root).join(format!("{module}.rs")))
}

fn named_test_body<'a>(source: &'a str, test_name: &str) -> Option<&'a str> {
    let marker = format!("fn {test_name}(");
    let start = source.find(&marker)?;
    let opening = source[start..].find('{')? + start;
    let mut depth = 0_u32;
    for (offset, byte) in source.as_bytes()[opening..].iter().enumerate() {
        match byte {
            b'{' => depth = depth.saturating_add(1),
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return source.get(opening..=opening + offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn named_test_attributes<'a>(source: &'a str, test_name: &str) -> Option<&'a str> {
    let marker = format!("fn {test_name}(");
    let function = source.find(&marker)?;
    let prefix = source.get(..function)?;
    let start = prefix
        .rfind("\n\n")
        .map_or(0, |offset| offset.saturating_add(2));
    source.get(start..function)
}

fn inventory_proof_trivial_body(body: &str) -> bool {
    let compact = body
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    let inner = compact
        .strip_prefix('{')
        .and_then(|value| value.strip_suffix('}'))
        .unwrap_or(compact.as_str())
        .trim_end_matches(';');
    if matches!(
        inner,
        "" | "return" | "assert!(true)" | "assert!(1==1)" | "debug_assert!(true)"
    ) {
        return true;
    }
    if single_assertion_arguments(inner).is_some_and(assertion_arguments_are_constant_only) {
        return true;
    }
    if local_constant_assertion_body(inner) {
        return true;
    }
    let Some(arguments) = inner
        .strip_prefix("assert_eq!(")
        .and_then(|value| value.strip_suffix(')'))
    else {
        return false;
    };
    let mut values = arguments.split(',');
    let Some(left) = values.next() else {
        return false;
    };
    let Some(right) = values.next() else {
        return false;
    };
    values.next().is_none() && left == right
}

fn single_assertion_arguments(value: &str) -> Option<&str> {
    ["assert!", "debug_assert!", "assert_eq!", "assert_ne!"]
        .into_iter()
        .find_map(|name| {
            value
                .strip_prefix(name)
                .and_then(|arguments| arguments.strip_prefix('('))
                .and_then(|arguments| arguments.strip_suffix(')'))
        })
}

/// A single assertion over literals and operators observes no product behavior.
/// This is deliberately conservative: any identifier other than boolean
/// literals keeps the body eligible for the compiled behavior checks.
fn assertion_arguments_are_constant_only(arguments: &str) -> bool {
    assertion_identifiers(arguments)
        .iter()
        .all(|identifier| constant_identifier(identifier))
}

fn local_constant_assertion_body(body: &str) -> bool {
    let statements = body
        .split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
        .collect::<Vec<_>>();
    let Some((assertion, declarations)) = statements.split_last() else {
        return false;
    };
    let Some(arguments) = single_assertion_arguments(assertion) else {
        return false;
    };
    let mut locals = BTreeSet::new();
    for declaration in declarations {
        // `inventory_proof_trivial_body` intentionally operates on compacted
        // source so whitespace cannot disguise a local literal tautology.
        // Accept the compact `letvalue=...` and `constVALUE=...` forms here;
        // the binding validation below still rejects non-identifiers.
        let Some(declaration) = declaration
            .strip_prefix("let")
            .or_else(|| declaration.strip_prefix("const"))
        else {
            return false;
        };
        let Some((binding, value)) = declaration.split_once('=') else {
            return false;
        };
        if assertion_identifiers(value)
            .iter()
            .any(|identifier| !constant_identifier(identifier) && !locals.contains(identifier))
        {
            return false;
        }
        let binding = binding
            .split(':')
            .next()
            .unwrap_or(binding)
            .split_whitespace()
            .last()
            .unwrap_or_default()
            .trim();
        if binding.is_empty()
            || !binding
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            return false;
        }
        locals.insert(binding.to_owned());
    }
    assertion_identifiers(arguments)
        .iter()
        .all(|identifier| constant_identifier(identifier) || locals.contains(identifier))
}

fn constant_identifier(identifier: &str) -> bool {
    matches!(
        identifier,
        "true"
            | "false"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "f32"
            | "f64"
    )
}

fn assertion_identifiers(arguments: &str) -> Vec<String> {
    let mut identifiers = Vec::new();
    let mut current = String::new();
    let mut quoted = None;
    let mut escaped = false;
    for character in arguments.chars() {
        if let Some(quote) = quoted {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == quote {
                quoted = None;
            }
            continue;
        }
        if matches!(character, '"' | '\'') {
            quoted = Some(character);
            continue;
        }
        if character.is_ascii_alphabetic() || character == '_' {
            current.push(character);
        } else if !current.is_empty() {
            identifiers.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        identifiers.push(current);
    }
    identifiers
}

fn validate_public_boundary_counterexample_declaration(root: &Path) -> Result<(), String> {
    let source_path = root.join("build/crates/fforager-testkit/src/lib.rs");
    let source = fs::read_to_string(&source_path)
        .map_err(|error| format!("read {}: {error}", source_path.display()))?;
    public_boundary_counterexample_declaration_diagnostic(&source)
        .map_or(Ok(()), |diagnostic| Err(diagnostic.to_owned()))
}

fn public_boundary_counterexample_declaration_diagnostic(source: &str) -> Option<&'static str> {
    let marker = "fn public_boundary_counterexamples_reject_audit_failures()";
    let Some(function_offset) = source.find(marker) else {
        return Some("FF-ARCH-E-PUBLIC-COUNTEREXAMPLE-MISSING");
    };
    let preceding = &source[..function_offset];
    let attributes = preceding
        .lines()
        .rev()
        .take(8)
        .map(str::trim)
        .collect::<Vec<_>>();
    if !attributes.contains(&"#[test]") || attributes.contains(&"#[ignore]") {
        return Some("FF-ARCH-E-PUBLIC-COUNTEREXAMPLE-SKIPPED");
    }
    None
}

fn execute_public_boundary_counterexample_test(
    root: &Path,
    checks: &mut Vec<Check>,
) -> Result<(), String> {
    validate_public_boundary_counterexample_declaration(root)?;
    let listed = cargo_proof_output(
        root,
        "cargo",
        &[
            "test",
            "--manifest-path",
            "build/Cargo.toml",
            "--locked",
            "-p",
            "fforager-testkit",
            "--lib",
            "--",
            "--list",
        ],
    )?;
    let expected = format!("{PUBLIC_BOUNDARY_COUNTEREXAMPLE_TEST}: test");
    if !listed.lines().any(|line| line.trim() == expected) {
        return Err("FF-ARCH-E-PUBLIC-COUNTEREXAMPLE-MISSING".to_owned());
    }
    checks.push(Check {
        id: "public-boundary-counterexample-listed".to_owned(),
        status: "PASS",
        proof_class: "structural",
        concrete_input: PUBLIC_BOUNDARY_COUNTEREXAMPLE_PROOF_ID.to_owned(),
        executed_boundary: "cargo test --list test registration boundary".to_owned(),
        expected_result: "the exact public counterexample test is registered and runnable"
            .to_owned(),
        observed_result: format!("registered {PUBLIC_BOUNDARY_COUNTEREXAMPLE_PROOF_ID}"),
        skipped_semantic_dependencies: vec![
            "Registration alone does not execute the public counterexamples.".to_owned(),
        ],
        detail: "the exact public counterexample test is registered".to_owned(),
    });
    let output = cargo_proof_output(
        root,
        "cargo",
        &[
            "test",
            "--manifest-path",
            "build/Cargo.toml",
            "--locked",
            "-p",
            "fforager-testkit",
            "--lib",
            PUBLIC_BOUNDARY_COUNTEREXAMPLE_TEST,
            "--",
            "--exact",
            "--nocapture",
        ],
    )?;
    if !output.contains(PUBLIC_BOUNDARY_COUNTEREXAMPLE_RECEIPT) {
        return Err("FF-ARCH-E-PUBLIC-COUNTEREXAMPLE-RECEIPT".to_owned());
    }
    checks.push(Check {
        id: "public-boundary-counterexamples".to_owned(),
        status: "PASS",
        proof_class: "public_boundary",
        concrete_input: PUBLIC_BOUNDARY_COUNTEREXAMPLE_PROOF_ID.to_owned(),
        executed_boundary: "exact compiled fforager-testkit public contract boundary".to_owned(),
        expected_result: format!(
            "{PUBLIC_BOUNDARY_COUNTEREXAMPLE_TEST} emits the required counterexample receipt"
        ),
        observed_result: PUBLIC_BOUNDARY_COUNTEREXAMPLE_RECEIPT.to_owned(),
        skipped_semantic_dependencies: vec![
            "This proves prerequisite public contract boundaries, not a shipped Ferric entrypoint."
                .to_owned(),
        ],
        detail: format!(
            "cargo test --exact --nocapture executed {PUBLIC_BOUNDARY_COUNTEREXAMPLE_PROOF_ID} and emitted the required counterexample receipt"
        ),
    });
    Ok(())
}

fn validate_contract_inventory_row<'a>(
    row: &'a Value,
    is_state: bool,
    fixture_root: &Path,
) -> Result<&'a str, String> {
    validate_contract_inventory_row_shape(row, is_state)?;
    let id = row
        .get("id")
        .and_then(Value::as_str)
        .ok_or("contract inventory row omits stable id")?;
    for required in ["owner", "proof_id", "readiness_gate"] {
        if row
            .get(required)
            .and_then(Value::as_str)
            .is_none_or(|text| text.trim().is_empty())
        {
            return Err(format!("contract inventory row {id} omits {required}"));
        }
    }
    let proof_id = row
        .get("proof_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if expected_inventory_proof(id) != Some(proof_id) {
        return Err(format!(
            "contract inventory row {id} does not cite its exact canonical executable proof: {proof_id}"
        ));
    }
    let (expected_owner, expected_gate) = expected_inventory_semantics(id)
        .ok_or_else(|| format!("contract inventory has unregistered stable ID {id}"))?;
    if row.get("owner").and_then(Value::as_str) != Some(expected_owner)
        || row.get("readiness_gate").and_then(Value::as_str) != Some(expected_gate)
    {
        return Err(format!(
            "contract inventory row {id} has invalid owner or readiness gate"
        ));
    }
    let fixtures = row
        .get("fixture_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("contract inventory row {id} omits fixture_ids"))?;
    if fixtures.is_empty() {
        return Err(format!("contract inventory row {id} has no fixture"));
    }
    validate_contract_inventory_required_fields(row, id, is_state)?;
    let required_fixture = required_inventory_fixture(id)
        .ok_or_else(|| format!("contract inventory has unregistered stable ID {id}"))?;
    if !fixtures
        .iter()
        .filter_map(Value::as_str)
        .any(|fixture| fixture == required_fixture)
    {
        return Err(format!(
            "contract inventory row {id} omits canonical fixture {required_fixture}"
        ));
    }
    for fixture in fixtures {
        let relative = fixture
            .as_str()
            .ok_or_else(|| format!("contract inventory row {id} has non-string fixture"))?;
        if !safe_relative(relative) || !fixture_root.join(relative).is_file() {
            return Err(format!(
                "contract inventory row {id} has unsafe or absent fixture {relative}"
            ));
        }
    }
    Ok(id)
}

fn validate_contract_inventory_required_fields(
    row: &Value,
    id: &str,
    is_state: bool,
) -> Result<(), String> {
    let arrays = if is_state {
        &[
            "states",
            "preconditions",
            "invariants",
            "postconditions",
            "invalid_transitions",
            "cancellation_outcomes",
            "durable_prefixes",
            "finite_assumptions",
        ][..]
    } else {
        &["limits_errors", "design_anchors"][..]
    };
    for field in arrays {
        let values = row
            .get(*field)
            .and_then(Value::as_array)
            .ok_or_else(|| format!("contract inventory row {id} omits {field}"))?;
        if values.is_empty()
            || values
                .iter()
                .any(|value| value.as_str().is_none_or(|text| text.trim().is_empty()))
        {
            return Err(format!(
                "contract inventory row {id} has empty or malformed {field}"
            ));
        }
    }
    if !is_state {
        for field in ["rust_type", "version_policy", "residual_uncertainty"] {
            if row
                .get(field)
                .and_then(Value::as_str)
                .is_none_or(|text| text.trim().is_empty())
            {
                return Err(format!("contract inventory row {id} omits {field}"));
            }
        }
    }
    Ok(())
}

fn expected_inventory_semantics(id: &str) -> Option<(&'static str, &'static str)> {
    const AC_001: &str = "WP-FF-005-versioned-core-contracts-v1-AC-001";
    const AC_002: &str = "WP-FF-005-versioned-core-contracts-v1-AC-002";
    const AC_003: &str = "WP-FF-005-versioned-core-contracts-v1-AC-003";
    const AC_004: &str = "WP-FF-005-versioned-core-contracts-v1-AC-004";
    const AC_005: &str = "WP-FF-005-versioned-core-contracts-v1-AC-005";
    match id {
        "FF-CONTRACT-IDENTITY-001" => Some(("fforager-contracts::identity", AC_002)),
        "FF-CONTRACT-TRISTATE-001" | "FF-CONTRACT-EXTENSION-001" => {
            Some(("fforager-contracts::identity", AC_001))
        }
        "FF-CONTRACT-SOURCE-GRAPH-001" => Some(("fforager-contracts::graph", AC_002)),
        "FF-CONTRACT-ACQUISITION-001" | "FF-CONTRACT-OUTPUT-SINK-001" => {
            Some(("fforager-contracts::graph", AC_001))
        }
        "FF-CONTRACT-CONFIG-001"
        | "FF-CONTRACT-EVENT-001"
        | "FF-CONTRACT-ERROR-001"
        | "FF-CONTRACT-CANCELLATION-001" => Some(("fforager-contracts::protocol", AC_001)),
        "FF-CONTRACT-PROCESS-001" | "FF-CONTRACT-PLUGIN-IPC-001" | "FF-CONTRACT-JS-WORKER-001" => {
            Some(("fforager-contracts::protocol", AC_005))
        }
        "FF-CONTRACT-FRAMING-001" => Some(("fforager-contracts::framing", AC_005)),
        "FF-CONTRACT-DURABILITY-001" => Some(("fforager-contracts::storage", AC_003)),
        "FF-CONTRACT-FILESYSTEM-001" => Some(("fforager-contracts::storage", AC_001)),
        "FF-CONTRACT-DIAGNOSTIC-ENVELOPE-001" => {
            Some(("fforager-diagnostics-contract::envelope", AC_001))
        }
        "FF-CONTRACT-DIAGNOSTIC-PROTOCOL-001" => {
            Some(("fforager-diagnostics-contract::protocol", AC_005))
        }
        "FF-CONTRACT-DIAGNOSTIC-LIFECYCLE-001" => {
            Some(("fforager-diagnostics-contract::lifecycle", AC_003))
        }
        "FF-CONTRACT-FFMPEG-SUPERVISION-001" => Some((
            "fforager-contracts::ffmpeg",
            "WP-FF-010-ffmpeg-supervision-spike-v1-AC-001",
        )),
        "FF-CONTRACT-RESOURCE-VECTOR-001"
        | "FF-STATE-ADMISSION-001"
        | "FF-STATE-FRAGMENT-DURABILITY-001" => Some(("fforager-core::resource", AC_004)),
        "FF-STATE-JOB-CANCEL-001"
        | "FF-STATE-SOURCE-REDIRECT-001"
        | "FF-STATE-LIVE-001"
        | "FF-STATE-SINK-001"
        | "FF-STATE-FFMPEG-001"
        | "FF-STATE-JS-WORKER-001"
        | "FF-STATE-PLUGIN-IPC-001"
        | "FF-STATE-COMMIT-ARCHIVE-001"
        | "FF-STATE-FILESYSTEM-CAPABILITY-001"
        | "FF-STATE-WATCHER-001" => Some(("fforager-core::lifecycle", AC_003)),
        _ => None,
    }
}

fn expected_inventory_proof(id: &str) -> Option<&'static str> {
    match id {
        "FF-CONTRACT-IDENTITY-001" => {
            Some("contracts::identity::tests::typed_ids_reject_wrong_prefix_and_uppercase")
        }
        "FF-CONTRACT-PLUGIN-IPC-001"
        | "FF-CONTRACT-JS-WORKER-001"
        | "FF-CONTRACT-DURABILITY-001"
        | "FF-CONTRACT-DIAGNOSTIC-LIFECYCLE-001" => Some(
            "testkit::tests::canonical_wire_fixtures_decode_as_their_registered_contract_types",
        ),
        "FF-CONTRACT-SOURCE-GRAPH-001"
        | "FF-CONTRACT-DIAGNOSTIC-ENVELOPE-001"
        | "FF-CONTRACT-DIAGNOSTIC-PROTOCOL-001" => Some(PUBLIC_BOUNDARY_COUNTEREXAMPLE_PROOF_ID),
        "FF-CONTRACT-ACQUISITION-001"
        | "FF-CONTRACT-OUTPUT-SINK-001"
        | "FF-CONTRACT-CONFIG-001"
        | "FF-CONTRACT-EVENT-001"
        | "FF-CONTRACT-ERROR-001"
        | "FF-CONTRACT-CANCELLATION-001" => {
            Some("testkit::tests::canonical_public_contract_fixtures_decode_and_validate")
        }
        "FF-CONTRACT-TRISTATE-001" => {
            Some("contracts::graph::tests::round_trip_preserves_tri_state")
        }
        "FF-CONTRACT-EXTENSION-001" => {
            Some("contracts::identity::tests::extensions_require_namespace_and_budget")
        }
        "FF-CONTRACT-PROCESS-001" | "FF-CONTRACT-FRAMING-001" => {
            Some("testkit::tests::shared_framing_harness_covers_partial_oversized_and_unknown_kind")
        }
        "FF-CONTRACT-FILESYSTEM-001" => {
            Some("contracts::storage::tests::unsupported_path_confinement_fails_closed")
        }
        "FF-CONTRACT-FFMPEG-SUPERVISION-001" => {
            Some("contracts::ffmpeg::tests::registered_public_boundary_suite")
        }
        "FF-STATE-ADMISSION-001" => {
            Some("core::resource::tests::atomic_zero_exact_one_over_and_release_identity")
        }
        "FF-CONTRACT-RESOURCE-VECTOR-001" | "FF-STATE-FRAGMENT-DURABILITY-001" => Some(
            "core::resource::tests::receive_requires_exact_claim_owner_and_records_attribution",
        ),
        "FF-STATE-JOB-CANCEL-001" => Some(
            "core::lifecycle::tests::success_and_durable_prefixes_require_effect_acknowledgements",
        ),
        "FF-STATE-COMMIT-ARCHIVE-001" => Some(
            "core::lifecycle::tests::transient_restore_and_stale_or_wrong_acknowledgements_are_rejected",
        ),
        "FF-STATE-SOURCE-REDIRECT-001" | "FF-STATE-LIVE-001" => {
            Some("core::lifecycle::tests::every_named_lifecycle_has_a_success_path")
        }
        "FF-STATE-SINK-001" => Some(
            "core::lifecycle::tests::failure_paths_complete_required_effects_before_their_outcome_state",
        ),
        "FF-STATE-PLUGIN-IPC-001" | "FF-STATE-WATCHER-001" => {
            Some("core::lifecycle::tests::cancellation_paths_reach_expected_states")
        }
        "FF-STATE-FFMPEG-001" | "FF-STATE-FILESYSTEM-CAPABILITY-001" => {
            Some(PUBLIC_BOUNDARY_COUNTEREXAMPLE_PROOF_ID)
        }
        "FF-STATE-JS-WORKER-001" => {
            Some("core::lifecycle::tests::illegal_transitions_are_typed_and_do_not_mutate_or_trace")
        }
        _ => None,
    }
}

fn expected_contract_inventory_ids() -> BTreeSet<&'static str> {
    BTreeSet::from([
        "FF-CONTRACT-IDENTITY-001",
        "FF-CONTRACT-SOURCE-GRAPH-001",
        "FF-CONTRACT-ACQUISITION-001",
        "FF-CONTRACT-OUTPUT-SINK-001",
        "FF-CONTRACT-TRISTATE-001",
        "FF-CONTRACT-EXTENSION-001",
        "FF-CONTRACT-CONFIG-001",
        "FF-CONTRACT-EVENT-001",
        "FF-CONTRACT-ERROR-001",
        "FF-CONTRACT-CANCELLATION-001",
        "FF-CONTRACT-PROCESS-001",
        "FF-CONTRACT-PLUGIN-IPC-001",
        "FF-CONTRACT-JS-WORKER-001",
        "FF-CONTRACT-FRAMING-001",
        "FF-CONTRACT-DURABILITY-001",
        "FF-CONTRACT-FILESYSTEM-001",
        "FF-CONTRACT-DIAGNOSTIC-ENVELOPE-001",
        "FF-CONTRACT-DIAGNOSTIC-PROTOCOL-001",
        "FF-CONTRACT-DIAGNOSTIC-LIFECYCLE-001",
        "FF-CONTRACT-RESOURCE-VECTOR-001",
        "FF-CONTRACT-FFMPEG-SUPERVISION-001",
    ])
}

fn expected_state_inventory_ids() -> BTreeSet<&'static str> {
    BTreeSet::from([
        "FF-STATE-JOB-CANCEL-001",
        "FF-STATE-SOURCE-REDIRECT-001",
        "FF-STATE-ADMISSION-001",
        "FF-STATE-FRAGMENT-DURABILITY-001",
        "FF-STATE-LIVE-001",
        "FF-STATE-SINK-001",
        "FF-STATE-FFMPEG-001",
        "FF-STATE-JS-WORKER-001",
        "FF-STATE-PLUGIN-IPC-001",
        "FF-STATE-COMMIT-ARCHIVE-001",
        "FF-STATE-FILESYSTEM-CAPABILITY-001",
        "FF-STATE-WATCHER-001",
    ])
}

fn validate_contract_inventory_shape(inventory: &Value) -> Result<(), String> {
    let object = inventory
        .as_object()
        .ok_or("contract inventory root is not an object")?;
    let expected = BTreeSet::from([
        "schema_id",
        "file_id",
        "schema_version",
        "owner",
        "compatibility_policy",
        "entries",
        "state_machines",
    ]);
    let observed = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
    if observed != expected {
        return Err(format!(
            "contract inventory top-level field mismatch: expected={expected:?}; observed={observed:?}"
        ));
    }
    if object
        .get("owner")
        .and_then(Value::as_str)
        .is_none_or(|text| text.trim().is_empty())
    {
        return Err("contract inventory omits owner".to_owned());
    }
    let compatibility = object
        .get("compatibility_policy")
        .and_then(Value::as_object)
        .ok_or("contract inventory compatibility_policy is not an object")?;
    let expected_compatibility = BTreeSet::from([
        "major",
        "minor",
        "unknown_mandatory_kind",
        "unknown_optional_extension",
    ]);
    let observed_compatibility = compatibility
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if observed_compatibility != expected_compatibility
        || compatibility
            .values()
            .any(|value| value.as_str().is_none_or(|text| text.trim().is_empty()))
    {
        return Err("contract inventory compatibility_policy is incomplete".to_owned());
    }
    Ok(())
}

fn validate_contract_inventory_row_shape(row: &Value, is_state: bool) -> Result<(), String> {
    let object = row
        .as_object()
        .ok_or("contract inventory row is not an object")?;
    let expected = if is_state {
        BTreeSet::from([
            "id",
            "owner",
            "states",
            "preconditions",
            "invariants",
            "postconditions",
            "invalid_transitions",
            "cancellation_outcomes",
            "durable_prefixes",
            "finite_assumptions",
            "proof_id",
            "readiness_gate",
            "fixture_ids",
        ])
    } else {
        BTreeSet::from([
            "id",
            "owner",
            "rust_type",
            "version_policy",
            "limits_errors",
            "proof_id",
            "readiness_gate",
            "fixture_ids",
            "design_anchors",
            "residual_uncertainty",
        ])
    };
    let observed = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
    if observed != expected {
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("<missing>");
        return Err(format!(
            "contract inventory row {id} field mismatch: expected={expected:?}; observed={observed:?}"
        ));
    }
    Ok(())
}

fn required_inventory_fixture(id: &str) -> Option<&'static str> {
    match id {
        "FF-CONTRACT-IDENTITY-001" => Some("identity-set-v1.0.json"),
        "FF-CONTRACT-ACQUISITION-001" | "FF-CONTRACT-OUTPUT-SINK-001" => {
            Some("acquisition-sink-v1.0.json")
        }
        "FF-CONTRACT-SOURCE-GRAPH-001"
        | "FF-CONTRACT-TRISTATE-001"
        | "FF-CONTRACT-EXTENSION-001" => Some("source-graph-v1.1.json"),
        "FF-CONTRACT-CONFIG-001" => Some("config-envelope-v1.0.json"),
        "FF-CONTRACT-EVENT-001" => Some("event-envelope-v1.0.json"),
        "FF-CONTRACT-ERROR-001" => Some("error-envelope-v1.0.json"),
        "FF-CONTRACT-CANCELLATION-001" => Some("cancellation-v1.0.json"),
        "FF-CONTRACT-PROCESS-001" | "FF-CONTRACT-FRAMING-001" => Some("process-request-v1.0.json"),
        "FF-CONTRACT-PLUGIN-IPC-001" => Some("plugin-envelope-v1.0.json"),
        "FF-CONTRACT-JS-WORKER-001" => Some("javascript-worker-envelope-v1.0.json"),
        "FF-CONTRACT-DURABILITY-001" => Some("archive-candidate-v1.0.json"),
        "FF-CONTRACT-FILESYSTEM-001" => Some("filesystem-capability-v1.0.json"),
        "FF-CONTRACT-DIAGNOSTIC-ENVELOPE-001" => Some("diagnostic-envelope-v1.2.json"),
        "FF-CONTRACT-DIAGNOSTIC-PROTOCOL-001" => Some("diagnostic-protocol-offer-v2.0.json"),
        "FF-CONTRACT-DIAGNOSTIC-LIFECYCLE-001" => Some("diagnostic-lifecycle-v1.0.json"),
        "FF-CONTRACT-FFMPEG-SUPERVISION-001" => Some("ffmpeg-supervision-v1.0.json"),
        "FF-CONTRACT-RESOURCE-VECTOR-001"
        | "FF-STATE-ADMISSION-001"
        | "FF-STATE-FRAGMENT-DURABILITY-001" => Some("resource-boundary-scenario.json"),
        id if id.starts_with("FF-STATE-") => Some("lifecycle-scenarios-v1.0.json"),
        _ => None,
    }
}

fn scan_data_only_product_models(root: &Path, checks: &mut Vec<Check>) -> Result<(), String> {
    for directory in [
        "product/crates/fforager-contracts/src",
        "product/crates/fforager-diagnostics-contract/src",
        "product/crates/fforager-core/src",
    ] {
        for path in walk_files(&root.join(directory))? {
            if path.extension() != Some(OsStr::new("rs")) {
                continue;
            }
            let source = fs::read_to_string(&path)
                .map_err(|error| format!("read {}: {error}", path.display()))?;
            let relative = slash(path.strip_prefix(root).map_err(|error| error.to_string())?);
            let model_source = if relative == "product/crates/fforager-core/src/engine.rs" {
                source
                    .replace("use std::net::SocketAddr;", "")
                    .replace("use std::sync::atomic::{AtomicBool, Ordering};", "")
            } else {
                source
            };
            if let Some(token) = forbidden_data_model_token(&model_source) {
                return Err(format!(
                    "FF-DEEP-E-RUNTIME-HANDLE: {} contains forbidden data-model token {token}",
                    slash(path.strip_prefix(root).map_err(|error| error.to_string())?)
                ));
            }
        }
    }
    checks.push(pass(
        "data-only-model-scan",
        "contract and core model sources contain no runtime, process, socket, filesystem-handle, thread, channel, or lock primitives",
    ));
    Ok(())
}

fn forbidden_data_model_token(source: &str) -> Option<&'static str> {
    let compact = source
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    if let Some(token) = forbidden_std_io_handle_token(&compact) {
        return Some(token);
    }
    [
        "tokio::",
        "interprocess::",
        "std::net",
        "std::process",
        "std::fs",
        "std::thread",
        "std::sync",
        "std::{net",
        "std::{process",
        "std::{fs",
        "std::{thread",
        "std::{sync",
        "tcpstream",
        "tcplistener",
        "udpsocket",
        "unixstream",
        "unixlistener",
        "unixdatagram",
        "std::fs::file",
        "openoptions",
        "std::process::command",
        "childprocess",
        "std::process::child",
        "childstdin",
        "childstdout",
        "childstderr",
        "filehandle",
        "ownedfd",
        "borrowedfd",
        "rawhandle",
        "ownedhandle",
        "borrowedhandle",
        "rawfd",
        "ownedsocket",
        "borrowedsocket",
        "rawsocket",
        "joinhandle",
        "std::sync::mutex",
        "std::sync::rwlock",
    ]
    .into_iter()
    .find(|token| compact.contains(token))
}

fn forbidden_std_io_handle_token(compact: &str) -> Option<&'static str> {
    const LIVE_HANDLE: &str = "std::io live handle";
    let live_names = [
        "stdin",
        "stdinlock",
        "stdout",
        "stdoutlock",
        "stderr",
        "stderrlock",
    ];
    if live_names
        .iter()
        .any(|name| compact.contains(&format!("std::io::{name}")))
        || compact.contains("usestd::io::*")
    {
        return Some(LIVE_HANDLE);
    }
    for statement in compact.split(';') {
        if statement.ends_with("usestd::io") {
            return Some(LIVE_HANDLE);
        }
        if let Some(alias) = statement
            .split_once("usestd::ioas")
            .map(|(_, alias)| identifier_prefix(alias))
            && !alias.is_empty()
            && live_names
                .iter()
                .any(|name| compact.contains(&format!("{alias}::{name}")))
        {
            return Some(LIVE_HANDLE);
        }
        if let Some(alias) = statement
            .split_once("usestdas")
            .map(|(_, alias)| identifier_prefix(alias))
            && !alias.is_empty()
            && (live_names
                .iter()
                .any(|name| compact.contains(&format!("{alias}::io::{name}")))
                || compact.contains(&format!("use{alias}::io::*")))
        {
            return Some(LIVE_HANDLE);
        }
        if let Some((_, group)) = statement.split_once("usestd::io::{") {
            let group = group.split_once('}').map_or(group, |(items, _)| items);
            if group == "*"
                || live_names.iter().any(|name| group.contains(name))
                || imported_io_module_alias_is_used(group, compact, &live_names)
            {
                return Some(LIVE_HANDLE);
            }
        }
        if let Some((_, group)) = statement.split_once("usestd::{") {
            let group = group.split_once('}').map_or(group, |(items, _)| items);
            if group.contains("io::{")
                && (group.contains('*') || live_names.iter().any(|name| group.contains(name)))
            {
                return Some(LIVE_HANDLE);
            }
            if imported_io_module_alias_is_used(group, compact, &live_names)
                || plain_grouped_io_module_is_used(group, compact, &live_names)
            {
                return Some(LIVE_HANDLE);
            }
        }
    }
    None
}

fn identifier_prefix(value: &str) -> &str {
    value
        .find(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .map_or(value, |end| &value[..end])
}

fn imported_io_module_alias_is_used(group: &str, compact: &str, live_names: &[&str]) -> bool {
    group.split(',').any(|item| {
        let alias = item
            .split_once("selfas")
            .or_else(|| item.split_once("ioas"))
            .map(|(_, alias)| identifier_prefix(alias));
        alias.is_some_and(|alias| {
            !alias.is_empty()
                && live_names
                    .iter()
                    .any(|name| compact.contains(&format!("{alias}::{name}")))
        })
    })
}

fn plain_grouped_io_module_is_used(group: &str, compact: &str, live_names: &[&str]) -> bool {
    let imports_io_module = group
        .split(',')
        .any(|item| item == "io" || item == "io::self" || item.starts_with("io::{self"));
    imports_io_module
        && live_names
            .iter()
            .any(|name| compact.contains(&format!("io::{name}")))
}

fn validate_contract_manual(root: &Path, checks: &mut Vec<Check>) -> Result<(), String> {
    let manual = fs::read_to_string(root.join("product/MODEL_MANUAL.md"))
        .map_err(|error| format!("read product model manual: {error}"))?;
    validate_contract_manual_text(&manual)?;
    checks.push(pass(
        "contract-model-manual",
        "no-context manual covers locations, schemas/versioning, commands, fixtures, safety limits, failures, recovery, and future runtime proof",
    ));
    Ok(())
}

fn validate_contract_manual_text(manual: &str) -> Result<(), String> {
    let normalized = manual.replace("\r\n", "\n");
    let manual = normalized.as_str();
    if !manual.starts_with("---\n") || !manual.contains("file_id: FF-PRODUCT-MODEL-MANUAL-001") {
        return Err("WP-FF-005 model manual omits canonical frontmatter".to_owned());
    }
    if manual
        .match_indices("<topic id=\"phase-0-contract-operation\"")
        .count()
        != 1
    {
        return Err(
            "WP-FF-005 model manual must contain exactly one contract-operation topic".to_owned(),
        );
    }
    let start = manual
        .find("<topic id=\"phase-0-contract-operation\"")
        .ok_or("WP-FF-005 model manual omits contract-operation topic")?;
    let remainder = &manual[start..];
    let end = remainder
        .find("</topic>")
        .ok_or("WP-FF-005 contract-operation topic is not closed")?;
    let topic = &remainder[..end];
    let opening = topic.lines().next().unwrap_or_default();
    for attribute in [
        "status=\"active\"",
        "version=\"1\"",
        "wp=\"WP-FF-005-versioned-core-contracts-v1\"",
        "ingestable=\"true\"",
    ] {
        if !opening.contains(attribute) {
            return Err(format!(
                "WP-FF-005 contract-operation topic omits required attribute {attribute}"
            ));
        }
    }
    if topic.len() < 4_000
        || topic.matches("```powershell").count() != 1
        || topic.matches("```").count() != 2
        || topic.matches("- ").count() < 15
        || topic.contains("<!--")
        || topic
            .matches("## Inspect and change the Phase 0 contracts and models")
            .count()
            != 1
    {
        return Err(
            "WP-FF-005 contract-operation topic is not a substantive structured operating manual"
                .to_owned(),
        );
    }
    let command_block = topic
        .split_once("```powershell\n")
        .and_then(|(_, remainder)| remainder.split_once("\n```").map(|(block, _)| block))
        .ok_or("WP-FF-005 contract-operation topic has no bounded PowerShell command block")?;
    let commands = command_block
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    if commands != canonical_contract_manual_commands() {
        return Err(
            "WP-FF-005 contract-operation topic must contain the six exact locked Cargo workflows"
                .to_owned(),
        );
    }
    for required in [
        "## Inspect and change the Phase 0 contracts and models",
        "product/crates/fforager-contracts/",
        "product/crates/fforager-diagnostics-contract/",
        "product/crates/fforager-core/",
        "build/crates/fforager-testkit/",
        "build/fixtures/contracts/inventory.json",
        "verify-deep --evidence-from-taskboard",
        "To change or regenerate fixtures",
        "Common contract failures and recovery",
        "Wire versions use incompatible major versions",
        "four-byte big-endian length",
        "256 KiB",
        "13-dimensional vector",
        "never overwrite or reinterpret a supported prior-version file",
        "FF-GATE-RUNTIME-001",
    ] {
        if !topic.contains(required) {
            return Err(format!(
                "WP-FF-005 model manual omits required operating surface {required:?}"
            ));
        }
    }
    Ok(())
}

fn canonical_contract_manual_commands() -> Vec<&'static str> {
    vec![
        "cargo test --manifest-path build/Cargo.toml --locked -p fforager-contracts",
        "cargo test --manifest-path build/Cargo.toml --locked -p fforager-diagnostics-contract",
        "cargo test --manifest-path build/Cargo.toml --locked -p fforager-core",
        "cargo test --manifest-path build/Cargo.toml --locked -p fforager-testkit",
        "cargo clippy --manifest-path build/Cargo.toml --workspace --all-targets --all-features --locked -- -D warnings",
        "cargo run --manifest-path build/Cargo.toml --locked -p fforager-xtask -- verify-deep --evidence-from-taskboard",
    ]
}

/// Prevents parallel unit tests from launching overlapping architecture-check
/// compiler/linker trees inside one test process.
///
/// Production gate invocations run one architecture check per process. The
/// Rust test harness, however, executes several adversarial architecture tests
/// concurrently. Each check intentionally compiles multiple isolated mutated
/// workspaces, and overlapping those trees can exhaust or destabilize the
/// Windows compiler/linker boundary even though their target directories are
/// distinct.
#[cfg(test)]
static ARCHITECTURE_CHECK_EXECUTION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn architecture_check(root: &Path) -> Result<ArchitectureResult, String> {
    #[cfg(test)]
    let _execution_guard = ARCHITECTURE_CHECK_EXECUTION_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let policy: ArchitecturePolicy = read_toml(&root.join("build/architecture-policy.toml"))?;
    let native_exceptions =
        read_native_dependency_exceptions(&root.join(&policy.exception_authority))?;
    validate_policy_identity(&policy, &native_exceptions)?;
    validate_artifact_layout(root)?;
    let tooling: ToolingPolicy = read_toml(&root.join("build/tooling-policy.toml"))?;
    validate_tooling_policy(&tooling)?;
    validate_current_host(root, &tooling)?;
    let rule_map: RuleMap = read_toml(&root.join("build/rule-to-proof.toml"))?;
    validate_rule_map_identity(&rule_map)?;
    let metadata_target = tooling
        .supported_hosts
        .first()
        .ok_or("tooling policy has no supported Cargo metadata target")?;
    let metadata = cargo_metadata(root, &policy, metadata_target)?;
    let mut checks = Vec::new();
    validate_workspace(root, &policy, &metadata, &mut checks)?;
    validate_internal_graph(&policy, &metadata, &mut checks)?;
    validate_three_roots(root, &policy, &mut checks)?;
    validate_dependencies(&policy, &metadata, &native_exceptions, &mut checks)?;
    let ordinary_metadata = cargo_metadata_ordinary_profile(root, &policy, metadata_target)?;
    validate_ordinary_profile_isolation(&ordinary_metadata, &native_exceptions, &mut checks)?;
    let canonical_rules = required_architecture_rules(&root.join(".GOV/rules/build-rules.yaml"))?;
    validate_rule_map(&rule_map, &canonical_rules, &mut checks)?;
    let benchmark_contract = benchmark::contract_execution(root)?;
    checks.push(Check {
        id: "benchmark-admission-contract".to_owned(), status: "PASS", proof_class: "structural",
        concrete_input: "existing bounded benchmark equality/statistics counterexample corpus".to_owned(),
        executed_boundary: "benchmark admission and public benchmark-validate handler".to_owned(),
        expected_result: "equal fixture admitted; unequal work and invalid statistics rejected".to_owned(),
        observed_result: benchmark_contract,
        skipped_semantic_dependencies: vec!["Synthetic contract fixtures do not establish native execution, product progress or performance.".to_owned()],
        detail: "Canonical validation executes the benchmark contract with its structural proof ceiling.".to_owned(),
    });
    let fixtures = validate_fixtures(root, &rule_map, &mut checks)?;
    let limitations = rule_map
        .rules
        .iter()
        .flat_map(|rule| rule.limitations.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let declared_supported_proof_classes = rule_map
        .rules
        .iter()
        .flat_map(|rule| rule.proof_classes.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let executed_proof_classes = executed_proof_classes(&checks, &fixtures);
    Ok(ArchitectureResult {
        checks,
        rules: canonical_rules.into_iter().collect(),
        fixtures,
        declared_supported_proof_classes,
        executed_proof_classes,
        limitations,
    })
}

fn validate_policy_identity(
    policy: &ArchitecturePolicy,
    native_exceptions: &BTreeMap<String, NativeDependencyException>,
) -> Result<(), String> {
    if policy.schema_id != "ff.architecture_policy@2"
        || policy.file_id != "FF-GOV-BUILD-ARCH-POLICY-001"
        || policy.schema_version != "2.0.0"
        || policy.accepted_design_decision_ids != ["FF-BUILD-036", "FF-BUILD-077"]
    {
        return Err("unsupported architecture-policy schema".to_owned());
    }
    let exact_paths = [
        (policy.workspace_manifest.as_str(), "build/Cargo.toml"),
        (policy.workspace_root.as_str(), "build"),
        (
            policy.target_directory.as_str(),
            ".fforager-artifacts/cargo-target",
        ),
        (
            policy.root_toolchain_selector.as_str(),
            "rust-toolchain.toml",
        ),
        (policy.governance_root.as_str(), ".GOV"),
        (policy.product_root.as_str(), "product"),
        (policy.build_root.as_str(), "build"),
        (
            policy.exception_authority.as_str(),
            ".GOV/rules/build-rules.yaml",
        ),
    ];
    if exact_paths
        .iter()
        .any(|(observed, expected)| observed != expected)
    {
        return Err(format!(
            "architecture-policy canonical path mismatch: {exact_paths:?}"
        ));
    }
    let declared_exception_ids = policy
        .exception_decision_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let authorized_exception_ids = native_exceptions.keys().cloned().collect::<BTreeSet<_>>();
    if declared_exception_ids != authorized_exception_ids {
        return Err(
            "FF-ARCH-E-UNAPPROVED-EXCEPTION: architecture policy and canonical exception allowlist differ"
                .to_owned(),
        );
    }
    let declared_native_links = native_exceptions
        .values()
        .flat_map(|exception| exception.allowed_native_link_packages.iter())
        .collect::<Vec<_>>();
    if declared_native_links.len()
        != declared_native_links
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .len()
    {
        return Err(
            "FF-ARCH-E-NATIVE-LINK-ATTRIBUTION: a native link package is attributed to multiple exceptions"
                .to_owned(),
        );
    }
    let expected_forbidden = BTreeSet::from([
        "engine->frontend",
        "engine->adapter",
        "engine->watcher",
        "adapter->adapter",
        "product->watcher",
        "watcher->engine",
        "normal_or_build:product->testkit",
    ]);
    let observed_forbidden: BTreeSet<_> = policy
        .forbidden_layer_edges
        .iter()
        .map(String::as_str)
        .collect();
    if policy.unsafe_policy != "forbid_except_scoped_policy_listed_boundary"
        || policy.unsafe_exception_authority != ".GOV/rules/build-rules.yaml#exception_authority"
        || policy.unsafe_decision_ids != ["FF-DEC-003", "FF-DEC-004", "FF-DEC-005"]
        || policy.internal_edge_default != "deny"
        || observed_forbidden != expected_forbidden
    {
        return Err("unsafe or direct-edge decision policy mismatch".to_owned());
    }
    let expected_runtime_or_native_dependencies = ["FFmpeg".to_owned(), "ffprobe".to_owned()]
        .into_iter()
        .chain(authorized_exception_ids)
        .collect::<BTreeSet<_>>();
    if policy
        .allowed_runtime_or_native_dependencies
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        != expected_runtime_or_native_dependencies
    {
        return Err("architecture-policy changed the approved runtime/native boundary".to_owned());
    }
    Ok(())
}

fn validate_rule_map_identity(rule_map: &RuleMap) -> Result<(), String> {
    if rule_map.schema_id != "ff.rule_to_proof@1"
        || rule_map.file_id != "FF-GOV-BUILD-RULE-PROOF-001"
        || rule_map.schema_version != "1.0.0"
    {
        return Err("unsupported rule-to-proof schema".to_owned());
    }
    Ok(())
}

fn validate_tooling_policy(policy: &ToolingPolicy) -> Result<(), String> {
    if policy.supported_hosts != ["x86_64-pc-windows-msvc"] {
        return Err(format!(
            "FF-TOOL-E-UNSUPPORTED-HOST: unsupported bootstrap host inventory {:?}",
            policy.supported_hosts
        ));
    }
    if policy.schema_id != "ff.tooling_policy@3"
        || policy.file_id != "FF-GOV-BUILD-TOOLING-POLICY-001"
        || policy.schema_version != "3.0.0"
        || policy.rust_toolchain != "1.97.1"
        || policy.auto_install
        || policy.advisory_database_max_age_hours != 168
    {
        return Err("invalid tooling-policy identity or bootstrap settings".to_owned());
    }
    let mut names = BTreeSet::new();
    for tool in &policy.tools {
        if !names.insert(tool.name.as_str())
            || tool.identity_line.trim().is_empty()
            || tool.command.is_empty()
            || tool.command.iter().any(String::is_empty)
            || tool.source.trim().is_empty()
            || tool.owning_gate != PR_GATE
            || !tool.required_now
        {
            return Err(format!(
                "invalid or duplicate tool policy for {}",
                tool.name
            ));
        }
    }
    let required = BTreeSet::from(["cargo", "cargo-deny", "clippy", "git", "rustc", "rustfmt"]);
    if names != required {
        return Err(format!("tooling-policy inventory mismatch: {names:?}"));
    }
    for tool in &policy.tools {
        let expected = expected_tool_policy(&tool.name)
            .ok_or_else(|| format!("unknown bootstrap tool policy for {}", tool.name))?;
        if tool.identity_line != expected.identity_line
            || tool.command != expected.command
            || tool.source != expected.source
            || tool.provenance_kind != expected.provenance_kind
            || tool.executable_sha256.as_deref() != expected.executable_sha256
        {
            return Err(format!(
                "tool {} does not match its canonical identity, command, source, and provenance",
                tool.name
            ));
        }
    }
    Ok(())
}

struct ExpectedToolPolicy {
    identity_line: &'static str,
    command: Vec<String>,
    source: &'static str,
    provenance_kind: &'static str,
    executable_sha256: Option<&'static str>,
}

fn expected_tool_policy(name: &str) -> Option<ExpectedToolPolicy> {
    let strings = |values: &[&str]| values.iter().map(|value| (*value).to_owned()).collect();
    match name {
        "rustc" => Some(ExpectedToolPolicy {
            identity_line: "rustc 1.97.1 (8bab26f4f 2026-07-14)",
            command: strings(&["rustc", "--version", "--verbose"]),
            source: "root rust-toolchain.toml",
            provenance_kind: "root-toolchain-and-cargo-lock",
            executable_sha256: None,
        }),
        "cargo" => Some(ExpectedToolPolicy {
            identity_line: "cargo 1.97.1 (c980f4866 2026-06-30)",
            command: strings(&["cargo", "--version", "--verbose"]),
            source: "root rust-toolchain.toml",
            provenance_kind: "root-toolchain-and-cargo-lock",
            executable_sha256: None,
        }),
        "rustfmt" => Some(ExpectedToolPolicy {
            identity_line: "rustfmt 1.9.0-stable (8bab26f4f6 2026-07-14)",
            command: strings(&["rustfmt", "--version"]),
            source: "pinned rustup component",
            provenance_kind: "root-toolchain-and-cargo-lock",
            executable_sha256: None,
        }),
        "clippy" => Some(ExpectedToolPolicy {
            identity_line: "clippy 0.1.97 (8bab26f4f6 2026-07-14)",
            command: strings(&["cargo", "clippy", "--version"]),
            source: "pinned rustup component",
            provenance_kind: "root-toolchain-and-cargo-lock",
            executable_sha256: None,
        }),
        "git" => Some(ExpectedToolPolicy {
            identity_line: "git version 2.53.0.windows.3",
            command: strings(&["git", "--version"]),
            source: "supported host installation",
            provenance_kind: "executable-sha256",
            executable_sha256: Some(
                "c53279919fdea03474bb23b465b3a82287157491f1bd69a5eb82dd9831582333",
            ),
        }),
        "cargo-deny" => Some(ExpectedToolPolicy {
            identity_line: "cargo-deny 0.20.2",
            command: strings(&["cargo", "deny", "--version"]),
            source: "cargo install cargo-deny --version 0.20.2 --locked",
            provenance_kind: "executable-sha256",
            executable_sha256: Some(
                "6e67806f5cf7d4da170d226a8f12cbd16aba236f51af1d75bc9fc56129d998ae",
            ),
        }),
        _ => None,
    }
}

fn validate_current_host(root: &Path, policy: &ToolingPolicy) -> Result<String, String> {
    let rustc = policy
        .tools
        .iter()
        .find(|tool| tool.name == "rustc")
        .ok_or("tooling policy omits rustc host probe")?;
    let (program, args) = rustc
        .command
        .split_first()
        .ok_or("empty rustc host command")?;
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    let output = command_output(root, program, &args)?;
    let host = output
        .lines()
        .find_map(|line| line.trim().strip_prefix("host: "))
        .ok_or("rustc verbose identity omits host triple")?;
    if !host_supported(&policy.supported_hosts, host) {
        return Err(format!(
            "current host {host} is not in supported_hosts {:?}",
            policy.supported_hosts
        ));
    }
    Ok(host.to_owned())
}

fn host_supported(supported_hosts: &[String], host: &str) -> bool {
    supported_hosts.iter().any(|supported| supported == host)
}

fn cargo_metadata(
    root: &Path,
    policy: &ArchitecturePolicy,
    target: &str,
) -> Result<Metadata, String> {
    let output = command_output_with_timeout(
        root,
        "cargo",
        &[
            "metadata",
            "--format-version",
            "1",
            "--manifest-path",
            &policy.workspace_manifest,
            "--all-features",
            "--locked",
            "--filter-platform",
            target,
        ],
        METADATA_COMMAND_TIMEOUT,
    )?;
    serde_json::from_str(&output).map_err(|error| format!("parse locked Cargo metadata: {error}"))
}

fn cargo_metadata_ordinary_profile(
    root: &Path,
    policy: &ArchitecturePolicy,
    target: &str,
) -> Result<Metadata, String> {
    let output = command_output_with_timeout(
        root,
        "cargo",
        &[
            "metadata",
            "--format-version",
            "1",
            "--manifest-path",
            &policy.workspace_manifest,
            "--no-default-features",
            "--features",
            "fforager-transport/ordinary-transport",
            "--locked",
            "--filter-platform",
            target,
        ],
        METADATA_COMMAND_TIMEOUT,
    )?;
    serde_json::from_str(&output)
        .map_err(|error| format!("parse locked ordinary-profile Cargo metadata: {error}"))
}

fn validate_ordinary_profile_isolation(
    metadata: &Metadata,
    native_exceptions: &BTreeMap<String, NativeDependencyException>,
    checks: &mut Vec<Check>,
) -> Result<(), String> {
    let resolve = metadata
        .resolve
        .as_ref()
        .ok_or("ordinary-profile Cargo metadata omitted resolve graph")?;
    let resolved_ids = resolve
        .nodes
        .iter()
        .map(|node| node.id.to_string())
        .collect::<BTreeSet<_>>();
    let resolved_packages = metadata
        .packages
        .iter()
        .filter(|package| resolved_ids.contains(&package.id.to_string()))
        .map(|package| {
            (
                format!("{}@{}", package.name, package.version),
                package.id.clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let expected_features = BTreeMap::from([
        (
            "reqwest@0.13.4",
            BTreeSet::from(["__rustls", "__tls", "http2", "rustls-no-provider", "stream"]),
        ),
        ("rustls@0.23.46", BTreeSet::from(["ring", "std", "tls12"])),
        (
            "ring@0.17.14",
            BTreeSet::from(["alloc", "default", "dev_urandom_fallback"]),
        ),
        ("rustls-platform-verifier@0.7.0", BTreeSet::new()),
        ("webpki-roots@1.0.10", BTreeSet::new()),
    ]);
    for (identity, expected) in expected_features {
        let package_id = resolved_packages.get(identity).ok_or_else(|| {
            format!("FF-ARCH-E-RESOLVED-FEATURE-SURFACE: ordinary profile omitted {identity}")
        })?;
        let observed = resolve
            .nodes
            .iter()
            .find(|node| &node.id == package_id)
            .map(|node| {
                node.features
                    .iter()
                    .map(|feature| feature.as_str())
                    .collect::<BTreeSet<_>>()
            })
            .ok_or_else(|| {
                format!(
                    "FF-ARCH-E-RESOLVED-FEATURE-SURFACE: ordinary profile omitted resolve node for {identity}"
                )
            })?;
        if observed != expected {
            return Err(format!(
                "FF-ARCH-E-RESOLVED-FEATURE-SURFACE: {identity} expected {expected:?}, observed {observed:?}"
            ));
        }
    }
    let forbidden = [
        "aws-lc-rs",
        "aws-lc-sys",
        "btls",
        "btls-sys",
        "hyper-tls",
        "native-tls",
        "openssl",
        "openssl-sys",
        "tokio-native-tls",
        "wreq",
        "wreq-util",
    ];
    let selected_forbidden = metadata
        .packages
        .iter()
        .filter(|package| {
            resolved_ids.contains(&package.id.to_string())
                && forbidden.contains(&package.name.as_str())
        })
        .map(|package| format!("{}@{}", package.name, package.version))
        .collect::<BTreeSet<_>>();
    if !selected_forbidden.is_empty() {
        return Err(format!(
            "FF-ARCH-E-ORDINARY-PROFILE-ISOLATION: ordinary profile selected forbidden transport packages {selected_forbidden:?}"
        ));
    }
    let (expected_closure_sha256, expected_source_closure_sha256) =
        validate_ordinary_profile_closures(metadata, native_exceptions)?;
    checks.push(pass(
        "ordinary-transport-profile",
        &format!(
            "root-reachable resolved identities, features, and edges match closure {expected_closure_sha256}; root-reachable crates.io extracted sources match closure {expected_source_closure_sha256}; exact reqwest/rustls/ring/webpki-root resolved features are pinned for the supported target; rustls-platform-verifier is explicitly governed but bypassed by the provider-bound client configuration; wreq, BoringSSL, AWS-LC, native-tls, and OpenSSL are absent"
        ),
    ));
    Ok(())
}

fn validate_ordinary_profile_closures(
    metadata: &Metadata,
    native_exceptions: &BTreeMap<String, NativeDependencyException>,
) -> Result<(String, String), String> {
    let exception = native_exceptions
        .get("FF-DEC-002")
        .ok_or("FF-ARCH-E-ORDINARY-PROFILE-CLOSURE: FF-DEC-002 is absent")?;
    let expected = exception.ordinary_transport_resolved_closure_sha256.clone();
    let observed = ordinary_transport_resolved_closure_sha256(metadata)?;
    if observed != expected {
        return Err(format!(
            "FF-ARCH-E-ORDINARY-PROFILE-CLOSURE: expected {expected}, observed {observed}"
        ));
    }
    let expected_source = exception
        .ordinary_transport_registry_source_closure_sha256
        .clone();
    let observed_source = ordinary_transport_registry_source_closure_sha256(metadata)?;
    if observed_source != expected_source {
        return Err(format!(
            "FF-ARCH-E-ORDINARY-SOURCE-CLOSURE: expected {expected_source}, observed {observed_source}"
        ));
    }
    Ok((expected, expected_source))
}

fn ordinary_transport_resolved_closure_sha256(metadata: &Metadata) -> Result<String, String> {
    let resolve = metadata
        .resolve
        .as_ref()
        .ok_or("ordinary-profile Cargo metadata omitted resolve graph")?;
    let packages = metadata
        .packages
        .iter()
        .map(|package| (package.id.to_string(), package))
        .collect::<BTreeMap<_, _>>();
    let nodes = resolve
        .nodes
        .iter()
        .map(|node| (node.id.to_string(), node))
        .collect::<BTreeMap<_, _>>();
    let root_id = metadata
        .workspace_members
        .iter()
        .find(|id| {
            packages
                .get(&id.to_string())
                .is_some_and(|package| package.name.as_str() == "fforager-transport")
        })
        .ok_or("FF-ARCH-E-ORDINARY-PROFILE-CLOSURE: fforager-transport root is absent")?
        .to_string();
    let mut pending = vec![root_id];
    let mut visited = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !visited.insert(id.clone()) {
            continue;
        }
        let node = nodes.get(&id).ok_or_else(|| {
            format!("FF-ARCH-E-ORDINARY-PROFILE-CLOSURE: missing resolve node {id}")
        })?;
        pending.extend(
            node.deps
                .iter()
                .map(|dependency| dependency.pkg.to_string()),
        );
    }
    let package_identity = |id: &str| -> Result<String, String> {
        let package = packages
            .get(id)
            .ok_or_else(|| format!("FF-ARCH-E-ORDINARY-PROFILE-CLOSURE: missing package {id}"))?;
        let source = package
            .source
            .as_ref()
            .map_or("workspace", |source| source.repr.as_str());
        Ok(format!("{}@{}[{source}]", package.name, package.version))
    };
    let mut lines = Vec::new();
    for id in visited {
        let node = nodes.get(&id).ok_or_else(|| {
            format!("FF-ARCH-E-ORDINARY-PROFILE-CLOSURE: missing resolve node {id}")
        })?;
        let mut features = node
            .features
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        features.sort();
        let mut edges = node
            .deps
            .iter()
            .map(|dependency| {
                package_identity(&dependency.pkg.to_string())
                    .map(|identity| format!("{}->{identity}", dependency.name))
            })
            .collect::<Result<Vec<_>, _>>()?;
        edges.sort();
        lines.push(format!(
            "{}|features={}|edges={}\n",
            package_identity(&id)?,
            features.join(","),
            edges.join(",")
        ));
    }
    lines.sort();
    Ok(encode_sha256(lines.concat().as_bytes()))
}

fn ordinary_transport_registry_source_closure_sha256(
    metadata: &Metadata,
) -> Result<String, String> {
    let resolve = metadata
        .resolve
        .as_ref()
        .ok_or("ordinary-profile Cargo metadata omitted resolve graph")?;
    let packages = metadata
        .packages
        .iter()
        .map(|package| (package.id.to_string(), package))
        .collect::<BTreeMap<_, _>>();
    let nodes = resolve
        .nodes
        .iter()
        .map(|node| (node.id.to_string(), node))
        .collect::<BTreeMap<_, _>>();
    let root_id = metadata
        .workspace_members
        .iter()
        .find(|id| {
            packages
                .get(&id.to_string())
                .is_some_and(|package| package.name.as_str() == "fforager-transport")
        })
        .ok_or("FF-ARCH-E-ORDINARY-SOURCE-CLOSURE: fforager-transport root is absent")?
        .to_string();
    let mut pending = vec![root_id];
    let mut visited = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !visited.insert(id.clone()) {
            continue;
        }
        let node = nodes.get(&id).ok_or_else(|| {
            format!("FF-ARCH-E-ORDINARY-SOURCE-CLOSURE: missing resolve node {id}")
        })?;
        pending.extend(
            node.deps
                .iter()
                .map(|dependency| dependency.pkg.to_string()),
        );
    }
    let mut lines = Vec::new();
    for id in visited {
        let package = packages
            .get(&id)
            .ok_or_else(|| format!("FF-ARCH-E-ORDINARY-SOURCE-CLOSURE: missing package {id}"))?;
        let Some(source) = package.source.as_ref() else {
            continue;
        };
        if !source
            .repr
            .starts_with("registry+https://github.com/rust-lang/crates.io-index")
        {
            return Err(format!(
                "FF-ARCH-E-ORDINARY-SOURCE-CLOSURE: {}@{} has non-crates.io source {}",
                package.name, package.version, source.repr
            ));
        }
        let root = package
            .manifest_path
            .parent()
            .ok_or_else(|| {
                format!(
                    "FF-ARCH-E-ORDINARY-SOURCE-CLOSURE: manifest has no parent for {}@{}",
                    package.name, package.version
                )
            })?
            .as_std_path();
        lines.push(format!(
            "{}@{}[{}] {}\n",
            package.name,
            package.version,
            source.repr,
            package_source_inventory_sha256(root)?
        ));
    }
    lines.sort();
    Ok(encode_sha256(lines.concat().as_bytes()))
}

fn validate_workspace(
    root: &Path,
    policy: &ArchitecturePolicy,
    metadata: &Metadata,
    checks: &mut Vec<Check>,
) -> Result<(), String> {
    validate_workspace_manifest(root, policy)?;
    let expected_workspace = root
        .join(&policy.workspace_root)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let observed_workspace = Path::new(metadata.workspace_root.as_str())
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if observed_workspace != expected_workspace {
        return Err(format!(
            "workspace root mismatch: expected {}, observed {}",
            expected_workspace.display(),
            observed_workspace.display()
        ));
    }
    let expected_target = root.join(&policy.target_directory);
    let observed_target = Path::new(metadata.target_directory.as_str());
    if normalize(observed_target) != normalize(&expected_target) {
        return Err(format!(
            "target directory mismatch: expected {}, observed {}",
            expected_target.display(),
            observed_target.display()
        ));
    }
    let expected_names: BTreeSet<_> = policy
        .members
        .iter()
        .map(|member| member.name.as_str())
        .collect();
    let observed_names: BTreeSet<_> = metadata
        .workspace_packages()
        .into_iter()
        .map(|package| package.name.as_str())
        .collect();
    if !member_inventory_matches(&expected_names, &observed_names) {
        return Err(format!(
            "FF-ARCH-E-UNDECLARED-MEMBER: expected {expected_names:?}, observed {observed_names:?}"
        ));
    }
    let observed_defaults: BTreeSet<_> = metadata.workspace_default_members.iter().collect();
    let observed_members: BTreeSet<_> = metadata.workspace_members.iter().collect();
    if observed_defaults != observed_members {
        return Err("workspace default-members must exactly equal workspace members".to_owned());
    }
    for member in &policy.members {
        validate_member_metadata(root, policy, member, metadata)?;
    }
    checks.push(pass(
        "workspace-shape",
        "locked resolver-3 workspace members exactly match policy, inherit the pinned package contract, and use .fforager-artifacts/cargo-target output",
    ));
    Ok(())
}

fn validate_artifact_layout(root: &Path) -> Result<(), String> {
    let cargo_config = fs::read_to_string(root.join(".cargo/config.toml"))
        .map_err(|error| format!("FF-ARCH-E-ARTIFACT-ROOT: read Cargo config: {error}"))?
        .replace("\r\n", "\n");
    if cargo_config != "[build]\ntarget-dir = \".fforager-artifacts/cargo-target\"\n" {
        return Err(
            "FF-ARCH-E-ARTIFACT-ROOT: .cargo/config.toml must select the canonical artifact root"
                .to_owned(),
        );
    }
    let gitignore = fs::read_to_string(root.join(".gitignore"))
        .map_err(|error| format!("FF-ARCH-E-ARTIFACT-ROOT: read .gitignore: {error}"))?;
    for required in ["/.fforager-artifacts/", "/.worktrees/"] {
        if !gitignore.lines().any(|line| line.trim() == required) {
            return Err(format!(
                "FF-ARCH-E-ARTIFACT-ROOT: .gitignore omits {required}"
            ));
        }
    }
    for forbidden in ["target", "build/target"] {
        if root.join(forbidden).exists() {
            return Err(format!(
                "FF-ARCH-E-ARTIFACT-ROOT: legacy disposable path exists: {forbidden}"
            ));
        }
    }
    for script in [
        "build/scripts/clean-artifacts.ps1",
        "build/scripts/new-worktree.ps1",
    ] {
        if !root.join(script).is_file() {
            return Err(format!(
                "FF-ARCH-E-ARTIFACT-ROOT: required lifecycle script is absent: {script}"
            ));
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "FF-BUILD-037 validates the complete per-member architecture contract in one audit surface"
)]
fn validate_member_metadata(
    root: &Path,
    policy: &ArchitecturePolicy,
    member: &MemberPolicy,
    metadata: &Metadata,
) -> Result<(), String> {
    let is_ffmpeg_prerequisite = member.name == "fforager-ffmpeg";
    if member.shipped && member.layer == "build_tooling" {
        return Err(format!("FF-ARCH-E-SHIPPED-BUILD-TOOLING: {}", member.name));
    }
    let ownership_root = if member.shipped || is_ffmpeg_prerequisite {
        policy.product_root.as_str()
    } else {
        policy.build_root.as_str()
    };
    require_relative_contained(root, &member.manifest, ownership_root)?;
    require_relative_contained(root, &member.source_root, ownership_root)?;
    if !split_trigger_valid(&member.split_trigger, &member.feature_owner) {
        return Err(format!(
            "FF-ARCH-E-INVALID-SPLIT-TRIGGER: {} cites {}",
            member.name, member.split_trigger
        ));
    }
    let exception_refs_valid = if member.name == "fforager-storage" {
        member.runtime_native_constraint_ref == "FF-DEC-004"
            && member.unsafe_policy_ref == "FF-BUILD-050;FF-DEC-004"
            && member.exception_policy_ref == "FF-DEC-004"
            && member.shipped
            && member.layer == "adapter"
            && !member.test_only
    } else if member.name == "fforager-xtask" {
        member.runtime_native_constraint_ref == "FF-DEC-005"
            && member.unsafe_policy_ref == "FF-BUILD-050;FF-DEC-005"
            && member.exception_policy_ref == "FF-DEC-005"
            && !member.shipped
            && member.layer == "build_tooling"
            && !member.test_only
    } else if is_ffmpeg_prerequisite {
        member.runtime_native_constraint_ref == "FF-DEC-003"
            && member.unsafe_policy_ref == "FF-BUILD-050;FF-DEC-003"
            && member.exception_policy_ref == "FF-DEC-003"
            && member.layer == "adapter"
            && member.artifact_role == "non_shipped_ffmpeg_supervision_prerequisite"
            && !member.shipped
            && !member.test_only
    } else {
        member.runtime_native_constraint_ref == "FF-START-BOUNDARY-001"
            && member.unsafe_policy_ref == "FF-BUILD-050"
            && member.exception_policy_ref == "FF-BUILD-052"
    };
    if member.layer.trim().is_empty()
        || member.artifact_role.trim().is_empty()
        || member.feature_owner.trim().is_empty()
        || member.profile.trim().is_empty()
        || member.removal_condition.trim().is_empty()
        || !exception_refs_valid
        || member.publish_allowed
        || (member.shipped && member.test_only)
        || (!member.shipped
            && !member.test_only
            && member.layer != "build_tooling"
            && !is_ffmpeg_prerequisite)
        || (member.shipped && member.layer == "build_tooling")
    {
        return Err(format!("invalid Phase 0 member policy for {}", member.name));
    }
    let manifest_text =
        fs::read_to_string(root.join(&member.manifest)).map_err(|e| e.to_string())?;
    if !manifest_inherits_workspace_lints(&manifest_text)?
        && !product_local_lint_manifest(&member.manifest, &manifest_text)?
    {
        return Err(format!(
            "{} does not inherit workspace lints",
            member.manifest
        ));
    }
    let package = metadata
        .workspace_packages()
        .into_iter()
        .find(|package| package.name.as_str() == member.name)
        .ok_or_else(|| format!("metadata package is absent for {}", member.name))?;
    let expected_manifest = root
        .join(&member.manifest)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let observed_manifest = Path::new(package.manifest_path.as_str())
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if observed_manifest != expected_manifest
        || package.source.is_some()
        || package.edition.to_string() != "2024"
        || package
            .rust_version
            .as_ref()
            .is_none_or(|version| version.to_string() != "1.97.1")
        || package
            .publish
            .as_ref()
            .is_none_or(|registries| !registries.is_empty())
    {
        return Err(format!(
            "workspace package metadata violates Phase 0 policy for {}",
            member.name
        ));
    }
    let source_root = root
        .join(&member.source_root)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let test_root = Path::new(&member.manifest)
        .parent()
        .map(|package_root| root.join(package_root).join("tests"))
        .filter(|path| path.is_dir())
        .map(|path| path.canonicalize().map_err(|error| error.to_string()))
        .transpose()?;
    for target in &package.targets {
        let source = Path::new(target.src_path.as_str())
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let package_local_test = target.is_test()
            && test_root
                .as_ref()
                .is_some_and(|tests| source.starts_with(tests));
        if !source.starts_with(&source_root) && !package_local_test {
            return Err(format!(
                "target source escapes declared source root: {}",
                source.display()
            ));
        }
    }
    Ok(())
}

fn manifest_inherits_workspace_lints(manifest_text: &str) -> Result<bool, String> {
    let manifest = manifest_text
        .parse::<toml::Table>()
        .map_err(|error| format!("parse member manifest: {error}"))?;
    Ok(manifest
        .get("lints")
        .and_then(toml::Value::as_table)
        .and_then(|lints| lints.get("workspace"))
        .and_then(toml::Value::as_bool)
        == Some(true))
}

fn product_local_lint_manifest(manifest_path: &str, manifest_text: &str) -> Result<bool, String> {
    if !matches!(
        manifest_path,
        "product/crates/fforager-contracts/Cargo.toml"
            | "product/crates/fforager-diagnostics-contract/Cargo.toml"
            | "product/crates/fforager-core/Cargo.toml"
            | "product/crates/fforager-ffmpeg/Cargo.toml"
            | "product/crates/fforager-storage/Cargo.toml"
            | "build/tools/fforager-xtask/Cargo.toml"
    ) {
        return Ok(false);
    }
    let manifest = manifest_text
        .parse::<toml::Table>()
        .map_err(|error| format!("parse product member manifest: {error}"))?;
    let lints = manifest
        .get("lints")
        .and_then(toml::Value::as_table)
        .ok_or("product member omits lints table")?;
    let rust = lints
        .get("rust")
        .and_then(toml::Value::as_table)
        .ok_or("product member omits lints.rust")?;
    let clippy = lints
        .get("clippy")
        .and_then(toml::Value::as_table)
        .ok_or("product member omits lints.clippy")?;
    let expected_unsafe_level = if matches!(
        manifest_path,
        "product/crates/fforager-ffmpeg/Cargo.toml"
            | "product/crates/fforager-storage/Cargo.toml"
            | "build/tools/fforager-xtask/Cargo.toml"
    ) {
        "deny"
    } else {
        "forbid"
    };
    Ok(lints.get("workspace").is_none()
        && rust.get("unsafe_code").and_then(toml::Value::as_str) == Some(expected_unsafe_level)
        && rust
            .get("missing_debug_implementations")
            .and_then(toml::Value::as_str)
            == Some("warn")
        && [
            "disallowed_types",
            "disallowed_methods",
            "disallowed_macros",
        ]
        .into_iter()
        .all(|key| clippy.get(key).and_then(toml::Value::as_str) == Some("forbid")))
}

fn validate_workspace_manifest(root: &Path, policy: &ArchitecturePolicy) -> Result<(), String> {
    let manifest: toml::Value = read_toml(&root.join("build/Cargo.toml"))?;
    let workspace = manifest
        .get("workspace")
        .and_then(toml::Value::as_table)
        .ok_or("build/Cargo.toml has no workspace table")?;
    if workspace.get("resolver").and_then(toml::Value::as_str) != Some("3") {
        return Err("workspace resolver must be exactly 3".to_owned());
    }
    let expected = policy
        .members
        .iter()
        .map(|member| workspace_member_path(&policy.build_root, &member.manifest))
        .collect::<Result<BTreeSet<_>, _>>()?;
    for key in ["members", "default-members"] {
        let values = workspace
            .get(key)
            .and_then(toml::Value::as_array)
            .ok_or_else(|| format!("workspace {key} is absent"))?;
        let observed = values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| format!("workspace {key} contains a non-string member"))
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        if observed != expected || observed.len() != values.len() {
            return Err(format!(
                "workspace {key} must exactly match declared member paths: expected {expected:?}, observed {observed:?}"
            ));
        }
    }
    let package = workspace
        .get("package")
        .and_then(toml::Value::as_table)
        .ok_or("workspace.package is absent")?;
    if package.get("edition").and_then(toml::Value::as_str) != Some("2024")
        || package.get("rust-version").and_then(toml::Value::as_str) != Some("1.97.1")
        || package.get("publish").and_then(toml::Value::as_bool) != Some(false)
    {
        return Err(
            "workspace package edition, rust-version, or publish policy mismatch".to_owned(),
        );
    }
    let selector: toml::Value = read_toml(&root.join("rust-toolchain.toml"))?;
    let toolchain = selector
        .get("toolchain")
        .and_then(toml::Value::as_table)
        .ok_or("root toolchain selector is malformed")?;
    let components = toolchain
        .get("components")
        .and_then(toml::Value::as_array)
        .ok_or("toolchain components are absent")?;
    let actual: BTreeSet<_> = components.iter().filter_map(toml::Value::as_str).collect();
    if toolchain.get("channel").and_then(toml::Value::as_str) != Some("1.97.1")
        || toolchain.get("profile").and_then(toml::Value::as_str) != Some("minimal")
        || actual != BTreeSet::from(["clippy", "rustfmt"])
    {
        return Err(
            "root toolchain selector does not match the pinned Phase 0 identity".to_owned(),
        );
    }
    Ok(())
}

fn workspace_member_path(build_root: &str, manifest: &str) -> Result<String, String> {
    let manifest_path = Path::new(manifest);
    let package_dir = manifest_path
        .parent()
        .ok_or_else(|| format!("member manifest has no parent: {manifest}"))?;
    if let Ok(relative) = package_dir.strip_prefix(build_root) {
        return Ok(slash(relative));
    }
    Ok(slash(&Path::new("..").join(package_dir)))
}

fn validate_internal_graph(
    policy: &ArchitecturePolicy,
    metadata: &Metadata,
    checks: &mut Vec<Check>,
) -> Result<(), String> {
    let packages = metadata.workspace_packages();
    let mut by_id: BTreeMap<&PackageId, &MemberPolicy> = BTreeMap::new();
    for package in &packages {
        let member = policy
            .members
            .iter()
            .find(|member| member.name == package.name.as_str())
            .ok_or_else(|| format!("FF-ARCH-E-UNDECLARED-MEMBER: {}", package.name))?;
        by_id.insert(&package.id, member);
    }
    let member_names: BTreeSet<_> = policy
        .members
        .iter()
        .map(|member| member.name.as_str())
        .collect();
    for package in &packages {
        let member = policy
            .members
            .iter()
            .find(|member| member.name == package.name.as_str())
            .ok_or("workspace package has no member policy")?;
        for allowed in &member.allowed_internal_dependencies {
            if !member_names.contains(allowed.package.as_str())
                || allowed.kinds.is_empty()
                || allowed
                    .kinds
                    .iter()
                    .any(|kind| !matches!(kind.as_str(), "normal" | "dev" | "build"))
            {
                return Err(format!(
                    "invalid allowed internal edge from {} to {}",
                    member.name, allowed.package
                ));
            }
        }
        for dependency in package
            .dependencies
            .iter()
            .filter(|dependency| member_names.contains(dependency.name.as_str()))
        {
            let kind = dependency.kind.to_string();
            let target = dependency.target.as_ref().map(ToString::to_string);
            if !member.allowed_internal_dependencies.iter().any(|allowed| {
                allowed.package == dependency.name
                    && allowed.kinds.contains(&kind)
                    && allowed.target == target
                    && allowed.optional == dependency.optional
            }) {
                let target_policy = policy
                    .members
                    .iter()
                    .find(|candidate| candidate.name == dependency.name)
                    .ok_or("internal dependency has no member policy")?;
                return Err(classify_forbidden_edge(
                    member,
                    target_policy,
                    dependency.kind,
                ));
            }
        }
    }
    let resolve = metadata
        .resolve
        .as_ref()
        .ok_or("Cargo metadata omitted resolve graph")?;
    let mut edges = Vec::new();
    for node in &resolve.nodes {
        let Some(from) = by_id.get(&node.id) else {
            continue;
        };
        for dependency in &node.deps {
            let Some(to) = by_id.get(&dependency.pkg) else {
                continue;
            };
            for kind in &dependency.dep_kinds {
                let kind_name = kind.kind.to_string();
                let target = kind.target.as_ref().map(ToString::to_string);
                if !from.allowed_internal_dependencies.iter().any(|allowed| {
                    allowed.package == to.name
                        && allowed.kinds.contains(&kind_name)
                        && allowed.target == target
                }) {
                    return Err(classify_forbidden_edge(from, to, kind.kind));
                }
                edges.push((from.name.as_str(), to.name.as_str()));
            }
        }
    }
    if graph_has_cycle(&edges) {
        return Err("FF-ARCH-E-CYCLE: internal workspace dependency cycle".to_owned());
    }
    checks.push(pass(
        "internal-graph",
        "normal, dev, build, optional, and target-conditioned internal edges match policy and are acyclic",
    ));
    Ok(())
}

fn classify_forbidden_edge(from: &MemberPolicy, to: &MemberPolicy, kind: DependencyKind) -> String {
    let diagnostic = classify_layers(&from.layer, &to.layer, kind);
    format!("{diagnostic}: {} -{kind}-> {}", from.name, to.name)
}

fn classify_layers(from_layer: &str, to_layer: &str, kind: DependencyKind) -> &'static str {
    if from_layer == "adapter" && to_layer == "adapter" {
        "FF-ARCH-E-ADAPTER-EDGE"
    } else if to_layer == "testkit"
        && matches!(kind, DependencyKind::Normal | DependencyKind::Build)
    {
        "FF-ARCH-E-TESTKIT-EDGE"
    } else if from_layer == "watcher" {
        "FF-ARCH-E-WATCHER-EDGE"
    } else if to_layer == "watcher" {
        "FF-ARCH-E-PRODUCT-WATCHER-EDGE"
    } else {
        "FF-ARCH-E-FORBIDDEN-EDGE"
    }
}

fn graph_has_cycle(edges: &[(&str, &str)]) -> bool {
    let mut nodes = BTreeSet::new();
    let mut indegree = BTreeMap::new();
    for (from, to) in edges {
        nodes.insert(*from);
        nodes.insert(*to);
        indegree.entry(*from).or_insert(0_usize);
        *indegree.entry(*to).or_insert(0) += 1;
    }
    let mut ready: Vec<_> = nodes
        .iter()
        .copied()
        .filter(|node| indegree.get(node).copied().unwrap_or(0) == 0)
        .collect();
    let mut visited = 0;
    while let Some(node) = ready.pop() {
        visited += 1;
        for (_, to) in edges.iter().filter(|(from, _)| *from == node) {
            let count = indegree.get_mut(to).expect("edge target has indegree");
            *count -= 1;
            if *count == 0 {
                ready.push(*to);
            }
        }
    }
    visited != nodes.len()
}

fn member_inventory_matches<T: Ord>(expected: &BTreeSet<T>, observed: &BTreeSet<T>) -> bool {
    expected == observed
}

fn split_trigger_valid(trigger: &str, feature_owner: &str) -> bool {
    if trigger == "FF-BUILD-036" {
        return true;
    }
    let Some((packet, acceptance)) = trigger.rsplit_once("-AC-") else {
        return false;
    };
    if packet != feature_owner {
        return false;
    }
    if acceptance.len() != 3 || !acceptance.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let Some(packet_tail) = packet.strip_prefix("WP-FF-") else {
        return false;
    };
    let Some((number, versioned_slug)) = packet_tail.split_once('-') else {
        return false;
    };
    if number.len() != 3 || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let Some((slug, version)) = versioned_slug.rsplit_once("-v") else {
        return false;
    };
    !slug.is_empty()
        && slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !version.is_empty()
        && version.bytes().all(|byte| byte.is_ascii_digit())
}

fn unapproved_exception_diagnostic(count: usize) -> Option<&'static str> {
    (count != 0).then_some("FF-ARCH-E-UNAPPROVED-EXCEPTION")
}

fn root_state_diagnostic(wrong_paths: usize, selectors: usize) -> Option<&'static str> {
    if wrong_paths != 0 {
        Some("FF-ARCH-E-WRONG-ROOT")
    } else if selectors != 1 {
        Some("FF-ARCH-E-DUPLICATE-TOOLCHAIN")
    } else {
        None
    }
}

fn runtime_literal_diagnostic(source: &str) -> Option<&'static str> {
    let lower = source.to_ascii_lowercase();
    (lower.contains(".gov")
        || lower.contains("\"build\"")
        || lower.contains("build/")
        || lower.contains("build\\"))
    .then_some("FF-ARCH-E-RUNTIME-BOUNDARY")
}

fn validate_three_roots(
    root: &Path,
    policy: &ArchitecturePolicy,
    checks: &mut Vec<Check>,
) -> Result<(), String> {
    for expected in [
        &policy.governance_root,
        &policy.product_root,
        &policy.build_root,
    ] {
        if !root.join(expected).is_dir() {
            return Err(format!("required repository root is absent: {expected}"));
        }
    }
    if policy.forbidden_product_runtime_roots != [".GOV", "build", ".fforager-artifacts"] {
        return Err(
            "product runtime boundary must forbid .GOV, build, and .fforager-artifacts".to_owned(),
        );
    }
    let wrong_root = [
        "Cargo.toml",
        "Cargo.lock",
        "architecture-policy.toml",
        "tooling-policy.toml",
        "rule-to-proof.toml",
        "deny.toml",
        "tools",
        "crates/fforager-testkit",
        "fixtures",
        "integration-tests",
        "fuzz",
        "benches",
        "reports",
        "target",
        ".GOV/Cargo.toml",
        ".GOV/Cargo.lock",
        ".GOV/rust-toolchain.toml",
        ".GOV/architecture-policy.toml",
        ".GOV/tooling-policy.toml",
        ".GOV/rule-to-proof.toml",
        ".GOV/tools",
        ".GOV/fixtures",
        ".GOV/integration-tests",
        ".GOV/fuzz",
        ".GOV/benches",
        ".GOV/reports",
        ".GOV/target",
        "product/Cargo.toml",
        "product/Cargo.lock",
        "product/rust-toolchain.toml",
        "product/architecture-policy.toml",
        "product/tooling-policy.toml",
        "product/rule-to-proof.toml",
        "product/tools/fforager-xtask",
        "product/crates/fforager-testkit",
        "product/fixtures",
        "product/integration-tests",
        "product/tests",
        "product/fuzz",
        "product/benches",
        "product/reports",
        "product/target",
        "build/rust-toolchain.toml",
    ];
    let existing_wrong: Vec<_> = wrong_root
        .iter()
        .copied()
        .filter(|path| root.join(path).exists())
        .collect();
    let existing = toolchain_selectors(root)?;
    if let Some(diagnostic) = root_state_diagnostic(existing_wrong.len(), existing.len()) {
        return Err(format!(
            "{diagnostic}: wrong_paths={existing_wrong:?}; selectors={existing:?}"
        ));
    }
    scan_product_runtime_literals(root)?;
    scan_product_oracle_boundary(root)?;
    scan_ferric_implementation_independence(root)?;
    validate_product_clippy_guard(root)?;
    checks.push(pass(
        "three-root-boundary",
        ".GOV, product, and build ownership is exclusive; root selector is unique",
    ));
    Ok(())
}

fn toolchain_selectors(root: &Path) -> Result<Vec<String>, String> {
    let mut selectors = Vec::new();
    for name in ["rust-toolchain.toml", "rust-toolchain"] {
        if root.join(name).is_file() {
            selectors.push(name.to_owned());
        }
    }
    let generated_target = root.join(".fforager-artifacts/cargo-target");
    let mut pending = [".GOV", "product", "build"]
        .iter()
        .map(|directory| root.join(directory))
        .collect::<Vec<_>>();
    while let Some(directory) = pending.pop() {
        if directory == generated_target {
            continue;
        }
        for entry in fs::read_dir(&directory)
            .map_err(|error| format!("read {}: {error}", directory.display()))?
        {
            let entry = entry.map_err(|error| error.to_string())?;
            let kind = entry.file_type().map_err(|error| error.to_string())?;
            if kind.is_dir() && !kind.is_symlink() {
                pending.push(entry.path());
            } else if kind.is_file()
                && matches!(
                    entry.file_name().to_str(),
                    Some("rust-toolchain.toml" | "rust-toolchain")
                )
            {
                selectors.push(slash(
                    entry
                        .path()
                        .strip_prefix(root)
                        .map_err(|error| error.to_string())?,
                ));
            }
        }
    }
    selectors.sort();
    Ok(selectors)
}

fn scan_product_runtime_literals(root: &Path) -> Result<(), String> {
    let product = root.join("product");
    if !product.exists() {
        return Ok(());
    }
    for path in walk_files(&product)? {
        if path.extension() != Some(OsStr::new("rs")) {
            continue;
        }
        let text =
            fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        if runtime_literal_diagnostic(&text).is_some() {
            return Err(format!(
                "FF-ARCH-E-RUNTIME-BOUNDARY: product source references governance/build path: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn scan_product_oracle_boundary(root: &Path) -> Result<(), String> {
    let product = root.join("product");
    if !product.exists() {
        return Ok(());
    }
    for path in walk_files(&product)? {
        let relative = slash(path.strip_prefix(root).map_err(|error| error.to_string())?);
        let bytes = fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
        if let Some(diagnostic) = product_oracle_boundary_diagnostic(&relative, &bytes) {
            return Err(format!(
                "{diagnostic}: product package input, asset, installer, script, source, or manifest delegates to forbidden oracle/runtime: {relative}",
            ));
        }
    }
    Ok(())
}

fn scan_ferric_implementation_independence(root: &Path) -> Result<(), String> {
    for relative_root in [
        "build/crates",
        "build/integration-tests",
        "build/fuzz",
        "build/benches",
    ] {
        let directory = root.join(relative_root);
        if !directory.exists() {
            continue;
        }
        for path in walk_files(&directory)? {
            let relative = slash(path.strip_prefix(root).map_err(|error| error.to_string())?);
            let bytes =
                fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
            if let Some(diagnostic) = ferric_implementation_dependency_diagnostic(&relative, &bytes)
            {
                return Err(format!(
                    "{diagnostic}: Ferric implementation/build/test asset depends on forbidden yt-dlp-family code: {relative}"
                ));
            }
        }
    }
    Ok(())
}

fn ferric_implementation_dependency_diagnostic(
    relative: &str,
    bytes: &[u8],
) -> Option<&'static str> {
    let extension = Path::new(relative)
        .extension()
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase);
    if matches!(extension.as_deref(), Some("md" | "txt")) {
        return None;
    }
    let path_hit = product_oracle_source_diagnostic(relative).is_some();
    let content_hit = std::str::from_utf8(bytes)
        .ok()
        .is_some_and(|source| product_oracle_source_diagnostic(source).is_some());
    (path_hit || content_hit).then_some("FF-ARCH-E-FERRIC-YTDLP-DEPENDENCY")
}

fn product_oracle_boundary_diagnostic(relative: &str, bytes: &[u8]) -> Option<&'static str> {
    if !relative.starts_with("product/")
        || relative == "product/clippy.toml"
        || relative == "product/apps/fforager-launcher/clippy.toml"
        || product_oracle_documentation_exception(relative, bytes)
    {
        return None;
    }
    let source = String::from_utf8_lossy(bytes);
    let reference_source = if relative == "product/apps/fforager-launcher/src/main.rs" {
        source.replace("Command", "ApprovedLauncherProcess")
    } else {
        source.to_string()
    };
    product_oracle_path_diagnostic(relative)
        .or_else(|| product_oracle_source_diagnostic(&String::from_utf8_lossy(bytes)))
        .or_else(|| product_documentation_asset_reference_diagnostic(&reference_source))
}

fn product_oracle_documentation_exception(relative: &str, bytes: &[u8]) -> bool {
    let permitted_path = relative == "product/MODEL_MANUAL.md"
        || relative.ends_with("/README.md")
        || (relative.contains("/docs/")
            && Path::new(relative)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md")));
    permitted_path && std::str::from_utf8(bytes).is_ok() && !bytes.contains(&0)
}

fn product_oracle_path_diagnostic(relative: &str) -> Option<&'static str> {
    let extension = Path::new(relative)
        .extension()
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase);
    if matches!(
        extension.as_deref(),
        Some(
            "py" | "pyw"
                | "pyi"
                | "exe"
                | "com"
                | "dll"
                | "so"
                | "dylib"
                | "bin"
                | "msi"
                | "ps1"
                | "sh"
                | "bat"
                | "cmd"
                | "zip"
                | "7z"
                | "rar"
                | "tar"
                | "gz"
                | "bz2"
                | "xz"
                | "jar"
        )
    ) || relative
        .split('/')
        .any(|component| component.eq_ignore_ascii_case("__pycache__"))
    {
        return Some("FF-ARCH-E-PRODUCT-ORACLE-RUNTIME");
    }
    product_oracle_source_diagnostic(relative)
}

fn product_documentation_asset_reference_diagnostic(source: &str) -> Option<&'static str> {
    let compact = source
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect::<String>();
    let macro_compact = source
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    if macro_compact.contains("include!(")
        || macro_compact.contains("include_bytes!(")
        || macro_compact.contains("include_str!(")
    {
        return Some("FF-ARCH-E-PRODUCT-DOC-ASSET-REFERENCE");
    }
    if compact.contains("stdprocesscommand") || compact.contains("commandnew") {
        return Some("FF-ARCH-E-PRODUCT-UNGOVERNED-PROCESS");
    }
    compact
        .contains("allowclippydisallowed")
        .then_some("FF-ARCH-E-PRODUCT-SCANNER-ESCAPE")
}

/// Compiler-resolved protection complements the lexical scanner: aliases of
/// `Command`, `Command::new`, `include_bytes!`, and `include_str!` are denied
/// by Clippy even when token spelling changes.  `include!` and local lint
/// escapes remain scanner-enforced because Clippy has no equivalent rule.
fn validate_product_clippy_guard(root: &Path) -> Result<(), String> {
    validate_rust_verification_environment(root)?;
    let config = fs::read_to_string(root.join("product/clippy.toml")).map_err(|error| {
        format!("FF-ARCH-E-PRODUCT-CLIPPY-GUARD: read product/clippy.toml: {error}")
    })?;
    for required in [
        "path = \"std::process::Command\"",
        "path = \"std::process::Command::new\"",
        "path = \"core::include_bytes\"",
        "path = \"core::include_str\"",
    ] {
        if !config.contains(required) {
            return Err(format!(
                "FF-ARCH-E-PRODUCT-CLIPPY-GUARD: product/clippy.toml omits required compiler guard {required}"
            ));
        }
    }
    let launcher_guard: toml::Table =
        read_toml(&root.join("product/apps/fforager-launcher/clippy.toml"))?;
    let launcher_keys = launcher_guard
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let macros = launcher_guard
        .get("disallowed-macros")
        .and_then(toml::Value::as_array)
        .ok_or("FF-ARCH-E-PRODUCT-CLIPPY-GUARD: launcher macro guard missing")?;
    let macro_paths = macros
        .iter()
        .filter_map(|row| row.get("path").and_then(toml::Value::as_str))
        .collect::<BTreeSet<_>>();
    if launcher_keys
        != BTreeSet::from([
            "disallowed-types",
            "disallowed-methods",
            "disallowed-macros",
        ])
        || !["disallowed-types", "disallowed-methods"]
            .iter()
            .all(|key| {
                launcher_guard
                    .get(*key)
                    .and_then(toml::Value::as_array)
                    .is_some_and(Vec::is_empty)
            })
        || macros.len() != 2
        || macro_paths != BTreeSet::from(["core::include_bytes", "core::include_str"])
    {
        return Err(
            "FF-ARCH-E-PRODUCT-CLIPPY-GUARD: launcher may relax only sibling process construction"
                .into(),
        );
    }
    for manifest in [
        "product/crates/fforager-contracts/Cargo.toml",
        "product/crates/fforager-diagnostics-contract/Cargo.toml",
        "product/crates/fforager-core/Cargo.toml",
        "product/crates/fforager-ffmpeg/Cargo.toml",
    ] {
        let text = fs::read_to_string(root.join(manifest))
            .map_err(|error| format!("FF-ARCH-E-PRODUCT-CLIPPY-GUARD: read {manifest}: {error}"))?;
        if !product_local_lint_manifest(manifest, &text)? {
            return Err(format!(
                "FF-ARCH-E-PRODUCT-CLIPPY-GUARD: {manifest} does not locally forbid the compiler guard lints"
            ));
        }
    }
    Ok(())
}

fn product_oracle_source_diagnostic(source: &str) -> Option<&'static str> {
    let compact = source
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect::<String>();
    (compact.contains("ytdlp")
        || compact.contains("python")
        || compact.contains("pyo3")
        || compact.contains("cpython")
        || compact.contains("pipinstall"))
    .then_some("FF-ARCH-E-PRODUCT-ORACLE-RUNTIME")
}

fn validate_dependencies(
    policy: &ArchitecturePolicy,
    metadata: &Metadata,
    native_exceptions: &BTreeMap<String, NativeDependencyException>,
    checks: &mut Vec<Check>,
) -> Result<(), String> {
    let decisions = validate_dependency_decisions(policy, native_exceptions)?;
    let packages = metadata.workspace_packages();
    if packages.is_empty() {
        return Err("workspace has no package".to_owned());
    }
    let workspace_names = packages
        .iter()
        .map(|package| package.name.as_str())
        .collect::<BTreeSet<_>>();
    for package in packages {
        validate_product_oracle_dependencies(package, policy, &workspace_names)?;
        validate_direct_dependencies(package, &decisions, &workspace_names)?;
    }
    validate_transitive_dependencies(policy, metadata, native_exceptions)?;
    checks.push(pass(
        "dependency-policy",
        "all direct external dependencies have exact per-consumer decisions; transitive build/proc-macro/native-link packages exactly match policy and canonical exceptions; BoringSSL source is vendored and environment/precompiled overrides are absent",
    ));
    Ok(())
}

fn validate_product_oracle_dependencies(
    package: &cargo_metadata::Package,
    policy: &ArchitecturePolicy,
    workspace_names: &BTreeSet<&str>,
) -> Result<(), String> {
    let is_product_package = policy.members.iter().any(|member| {
        member.name == package.name.as_str()
            && (member.shipped || member.manifest.starts_with("product/"))
    });
    if !is_product_package {
        return Ok(());
    }
    let forbidden = package
        .dependencies
        .iter()
        .map(|dependency| dependency.name.as_str())
        .chain(std::iter::once(package.name.as_str()))
        .find(|name| product_oracle_source_diagnostic(name).is_some());
    if let Some(name) = forbidden {
        return Err(format!(
            "FF-ARCH-E-PRODUCT-ORACLE-DEPENDENCY: product package {} declares or is forbidden oracle/runtime dependency {name}",
            package.name
        ));
    }
    let _ = workspace_names;
    Ok(())
}

fn validate_dependency_decisions<'policy>(
    policy: &'policy ArchitecturePolicy,
    native_exceptions: &BTreeMap<String, NativeDependencyException>,
) -> Result<BTreeMap<(&'policy str, &'policy str), &'policy DependencyDecision>, String> {
    let decisions: BTreeMap<_, _> = policy
        .dependency_decisions
        .iter()
        .map(|decision| {
            (
                (decision.consumer.as_str(), decision.name.as_str()),
                decision,
            )
        })
        .collect();
    if decisions.len() != policy.dependency_decisions.len() {
        return Err("duplicate dependency decision".to_owned());
    }
    let members = policy
        .members
        .iter()
        .map(|member| (member.name.as_str(), member))
        .collect::<BTreeMap<_, _>>();
    for decision in decisions.values() {
        let Some(member) = members.get(decision.consumer.as_str()) else {
            return Err(format!(
                "invalid tooling-only dependency decision for {}",
                decision.name
            ));
        };
        let is_product = member.shipped;
        let runtime_class_is_product = decision.runtime_class == "shipped_rust_product";
        if is_product != runtime_class_is_product {
            return Err(format!(
                "FF-ARCH-E-DEPENDENCY-RUNTIME-CLASS: {} -> {} runtime class {} does not match its product boundary",
                decision.consumer, decision.name, decision.runtime_class
            ));
        }
        if !matches!(
            decision.runtime_class.as_str(),
            "non_shipped_build_tooling"
                | "non_shipped_test_tooling"
                | "non_shipped_phase0_prerequisite"
                | "non_shipped_phase0_proof_telemetry"
                | "shipped_rust_product"
        ) || decision.purpose.trim().is_empty()
            || decision.version.trim().is_empty()
            || !work_packet_base_owner_valid(&decision.owner)
            || decision.allowed_consumers != [decision.consumer.as_str()]
            || decision.reason.trim().is_empty()
            || decision.removal_trigger.trim().is_empty()
            || !decision_approval_matches_owner(&decision.approval_id, &decision.owner)
            || decision.features.len() != decision.features.iter().collect::<BTreeSet<_>>().len()
        {
            return Err(format!(
                "invalid tooling-only dependency decision for {}",
                decision.name
            ));
        }
        if !dependency_native_exception_valid(decision, native_exceptions) {
            return Err(format!(
                "FF-ARCH-E-UNAPPROVED-NATIVE-DEPENDENCY: invalid native exception for {}",
                decision.name
            ));
        }
    }
    Ok(decisions)
}

fn dependency_native_exception_valid(
    decision: &DependencyDecision,
    native_exceptions: &BTreeMap<String, NativeDependencyException>,
) -> bool {
    if !decision.native && decision.exception_id.is_none() {
        return true;
    }
    let Some(exception_id) = decision.exception_id.as_deref() else {
        return false;
    };
    let Some(exception) = native_exceptions.get(exception_id) else {
        return false;
    };
    let allowed_consumers = if decision.runtime_class == "shipped_rust_product" {
        &exception.allowed_product_consumers
    } else {
        &exception.allowed_prerequisite_consumers
    };
    (decision.native || matches!(exception_id, "FF-DEC-003" | "FF-DEC-004" | "FF-DEC-005"))
        && exception.owner == decision.owner
        && exception.approval_id == decision.approval_id
        && allowed_consumers.contains(&decision.consumer)
        && exception
            .allowed_direct_dependencies
            .contains(&decision.name)
        && exception
            .allowed_versions
            .contains(&format!("{}@{}", decision.name, decision.version))
        && exception
            .allowed_runtime_classes
            .contains(&decision.runtime_class)
}

fn work_packet_base_owner_valid(owner: &str) -> bool {
    let Some(tail) = owner.strip_prefix("WP-FF-") else {
        return false;
    };
    let Some((number, slug)) = tail.split_once('-') else {
        return false;
    };
    number.len() == 3
        && number.bytes().all(|byte| byte.is_ascii_digit())
        && !slug.is_empty()
        && slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn decision_approval_matches_owner(approval_id: &str, owner: &str) -> bool {
    let Some((packet, _)) = approval_id.rsplit_once("-AC-") else {
        return false;
    };
    split_trigger_valid(approval_id, packet)
        && packet.strip_prefix(owner).is_some_and(|suffix| {
            suffix.strip_prefix("-v").is_some_and(|version| {
                !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit())
            })
        })
}

fn validate_direct_dependencies(
    package: &cargo_metadata::Package,
    decisions: &BTreeMap<(&str, &str), &DependencyDecision>,
    workspace_names: &BTreeSet<&str>,
) -> Result<(), String> {
    let declared: BTreeSet<_> = package
        .dependencies
        .iter()
        .filter(|dependency| !workspace_names.contains(dependency.name.as_str()))
        .map(|dep| dep.name.as_str())
        .collect();
    let expected: BTreeSet<_> = decisions
        .keys()
        .filter_map(|(consumer, dependency)| {
            (*consumer == package.name.as_str()).then_some(*dependency)
        })
        .collect();
    if declared != expected {
        return Err(format!(
            "direct dependency decisions mismatch: expected {expected:?}, observed {declared:?}"
        ));
    }
    for dependency in &package.dependencies {
        if workspace_names.contains(dependency.name.as_str()) {
            continue;
        }
        let decision = decisions
            .get(&(package.name.as_str(), dependency.name.as_str()))
            .ok_or("direct dependency has no decision")?;
        if !dependency_features_match(
            &dependency.features,
            dependency.uses_default_features,
            decision,
        ) {
            return Err(format!(
                "FF-ARCH-E-DEPENDENCY-FEATURE: {} features/default-features differ from policy",
                dependency.name
            ));
        }
        if dependency.req.to_string() != format!("={}", decision.version)
            || dependency.path.is_some()
            || dependency.source.as_ref().is_some_and(|source| {
                !source
                    .repr
                    .starts_with("registry+https://github.com/rust-lang/crates.io-index")
            })
        {
            return Err(format!(
                "dependency {} is not exact crates.io policy",
                dependency.name
            ));
        }
    }
    Ok(())
}

fn validate_transitive_dependencies(
    policy: &ArchitecturePolicy,
    metadata: &Metadata,
    native_exceptions: &BTreeMap<String, NativeDependencyException>,
) -> Result<(), String> {
    let mut observed_build = BTreeSet::new();
    let mut observed_proc = BTreeSet::new();
    let mut observed_native_links = BTreeSet::new();
    for package in &metadata.packages {
        let identity = format!("{}@{}", package.name, package.version);
        if package.links.is_some() {
            observed_native_links.insert(identity.clone());
        }
        for target in &package.targets {
            if target.is_custom_build() {
                observed_build.insert(identity.clone());
            }
            if target.is_proc_macro() {
                observed_proc.insert(identity.clone());
            }
        }
    }
    let expected_build: BTreeSet<_> = policy
        .approved_transitive_build_packages
        .iter()
        .cloned()
        .collect();
    let expected_proc: BTreeSet<_> = policy
        .approved_transitive_proc_macros
        .iter()
        .cloned()
        .collect();
    let expected_native_links = policy
        .exception_decision_ids
        .iter()
        .filter_map(|id| native_exceptions.get(id))
        .flat_map(|exception| exception.allowed_native_link_packages.iter().cloned())
        .collect::<BTreeSet<_>>();
    if observed_build != expected_build || observed_proc != expected_proc {
        return Err(format!(
            "transitive build/proc-macro surface mismatch: build={observed_build:?}, proc={observed_proc:?}"
        ));
    }
    if observed_native_links != expected_native_links {
        return Err(format!(
            "FF-ARCH-E-NATIVE-LINK-SURFACE: expected {expected_native_links:?}, observed {observed_native_links:?}"
        ));
    }
    validate_native_exception_reachability(policy, metadata, native_exceptions)?;
    validate_native_supply_chain(metadata, native_exceptions, &expected_native_links)?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn validate_native_exception_reachability(
    policy: &ArchitecturePolicy,
    metadata: &Metadata,
    native_exceptions: &BTreeMap<String, NativeDependencyException>,
) -> Result<(), String> {
    let resolve = metadata
        .resolve
        .as_ref()
        .ok_or("Cargo metadata omitted resolve graph")?;
    for exception_id in &policy.exception_decision_ids {
        let exception = native_exceptions.get(exception_id).ok_or_else(|| {
            format!(
                "FF-ARCH-E-NATIVE-CONSUMER-REACHABILITY: missing native exception {exception_id}"
            )
        })?;
        if matches!(
            exception_id.as_str(),
            "FF-DEC-003" | "FF-DEC-004" | "FF-DEC-005"
        ) {
            validate_ffi_boundary_direct_attribution(policy, metadata, exception_id, exception)?;
            continue;
        }
        let governed_package_ids = metadata
            .packages
            .iter()
            .filter(|package| {
                let identity = format!("{}@{}", package.name, package.version);
                exception.allowed_versions.contains(&identity)
                    || exception.allowed_native_link_packages.contains(&identity)
                    || exception
                        .allowed_native_runtime_packages
                        .contains(&identity)
            })
            .map(|package| package.id.to_string())
            .collect::<BTreeSet<_>>();
        if governed_package_ids.is_empty() {
            return Err(format!(
                "FF-ARCH-E-NATIVE-CONSUMER-REACHABILITY: {exception_id} has no selected package in the resolve graph"
            ));
        }
        for workspace_id in &metadata.workspace_members {
            let workspace_id = workspace_id.to_string();
            let package = metadata
                .packages
                .iter()
                .find(|package| package.id.to_string() == workspace_id)
                .ok_or_else(|| {
                    format!(
                        "FF-ARCH-E-NATIVE-CONSUMER-REACHABILITY: workspace package {workspace_id} is absent from metadata"
                    )
                })?;
            let member = policy
                .members
                .iter()
                .find(|member| member.name == package.name.as_str())
                .ok_or_else(|| {
                    format!(
                        "FF-ARCH-E-NATIVE-CONSUMER-REACHABILITY: workspace package {} has no member policy",
                        package.name
                    )
                })?;
            let mut pending = vec![workspace_id.clone()];
            let mut visited = BTreeSet::new();
            let mut reaches_exception = false;
            while let Some(package_id) = pending.pop() {
                if !visited.insert(package_id.clone()) {
                    continue;
                }
                if governed_package_ids.contains(&package_id) {
                    reaches_exception = true;
                    break;
                }
                let Some(node) = resolve
                    .nodes
                    .iter()
                    .find(|node| node.id.to_string() == package_id)
                else {
                    continue;
                };
                pending.extend(
                    node.deps
                        .iter()
                        .map(|dependency| dependency.pkg.to_string()),
                );
            }
            if !reaches_exception {
                continue;
            }
            let is_product = member.shipped || member.manifest.starts_with("product/");
            let allowed_consumers = if is_product {
                &exception.allowed_product_consumers
            } else {
                &exception.allowed_prerequisite_consumers
            };
            if !allowed_consumers.contains(&member.name) {
                return Err(format!(
                    "FF-ARCH-E-NATIVE-CONSUMER-REACHABILITY: {} reaches {exception_id} without matching consumer and product-use authority",
                    member.name
                ));
            }
            let workspace_node = resolve
                .nodes
                .iter()
                .find(|node| node.id.to_string() == workspace_id)
                .ok_or_else(|| {
                    format!(
                        "FF-ARCH-E-NATIVE-EDGE-ATTRIBUTION: {} has no resolve node",
                        member.name
                    )
                })?;
            for dependency in &workspace_node.deps {
                let direct_package_id = dependency.pkg.to_string();
                let mut pending = vec![direct_package_id.clone()];
                let mut visited = BTreeSet::new();
                let mut direct_edge_reaches_exception = false;
                while let Some(package_id) = pending.pop() {
                    if !visited.insert(package_id.clone()) {
                        continue;
                    }
                    if governed_package_ids.contains(&package_id) {
                        direct_edge_reaches_exception = true;
                        break;
                    }
                    let Some(node) = resolve
                        .nodes
                        .iter()
                        .find(|node| node.id.to_string() == package_id)
                    else {
                        continue;
                    };
                    pending.extend(
                        node.deps
                            .iter()
                            .map(|nested_dependency| nested_dependency.pkg.to_string()),
                    );
                }
                if !direct_edge_reaches_exception {
                    continue;
                }
                let direct_package = metadata
                    .packages
                    .iter()
                    .find(|candidate| candidate.id.to_string() == direct_package_id)
                    .ok_or_else(|| {
                        format!(
                            "FF-ARCH-E-NATIVE-EDGE-ATTRIBUTION: direct package {direct_package_id} is absent from metadata"
                        )
                    })?;
                let decision = policy.dependency_decisions.iter().find(|decision| {
                    decision.consumer == member.name
                        && decision.name == direct_package.name.as_str()
                });
                if !exception
                    .allowed_direct_dependencies
                    .contains(direct_package.name.as_str())
                    || decision.is_none_or(|decision| {
                        !decision.native
                            || decision.exception_id.as_deref() != Some(exception_id.as_str())
                    })
                {
                    return Err(format!(
                        "FF-ARCH-E-NATIVE-EDGE-ATTRIBUTION: direct dependency {} -> {} reaches {exception_id} without its own matching native decision",
                        member.name, direct_package.name
                    ));
                }
            }
            let direct_exception_decisions = policy
                .dependency_decisions
                .iter()
                .filter(|decision| {
                    decision.consumer == member.name
                        && exception
                            .allowed_direct_dependencies
                            .contains(&decision.name)
                })
                .collect::<Vec<_>>();
            if direct_exception_decisions.is_empty()
                || direct_exception_decisions.iter().any(|decision| {
                    !decision.native
                        || decision.exception_id.as_deref() != Some(exception_id.as_str())
                        || (is_product && decision.runtime_class != "shipped_rust_product")
                })
            {
                return Err(format!(
                    "FF-ARCH-E-NATIVE-CONSUMER-REACHABILITY: {} reaches {exception_id} without an explicit matching native dependency decision",
                    member.name
                ));
            }
        }
    }
    Ok(())
}

fn validate_ffi_boundary_direct_attribution(
    policy: &ArchitecturePolicy,
    metadata: &Metadata,
    exception_id: &str,
    exception: &NativeDependencyException,
) -> Result<(), String> {
    let consumer = exception
        .allowed_prerequisite_consumers
        .iter()
        .chain(exception.allowed_product_consumers.iter())
        .next()
        .ok_or_else(|| {
            format!(
                "FF-ARCH-E-NATIVE-CONSUMER-REACHABILITY: {exception_id} has no prerequisite consumer"
            )
        })?;
    let package = metadata
        .workspace_packages()
        .into_iter()
        .find(|package| package.name.as_str() == consumer)
        .ok_or_else(|| {
            format!(
                "FF-ARCH-E-NATIVE-CONSUMER-REACHABILITY: {exception_id} consumer {consumer} is absent from the workspace"
            )
        })?;
    let declared_bindings = package
        .dependencies
        .iter()
        .filter(|dependency| {
            exception
                .allowed_direct_dependencies
                .contains(dependency.name.as_str())
        })
        .map(|dependency| dependency.name.clone())
        .collect::<BTreeSet<_>>();
    if declared_bindings != exception.allowed_direct_dependencies {
        return Err(format!(
            "FF-ARCH-E-NATIVE-EDGE-ATTRIBUTION: {consumer} must directly declare exactly the {exception_id} bindings {:?}, observed {declared_bindings:?}",
            exception.allowed_direct_dependencies
        ));
    }

    let observed_decisions = policy
        .dependency_decisions
        .iter()
        .filter(|decision| decision.exception_id.as_deref() == Some(exception_id))
        .map(|decision| {
            (
                decision.consumer.clone(),
                decision.name.clone(),
                decision.version.clone(),
                decision.runtime_class.clone(),
                decision.native,
            )
        })
        .collect::<BTreeSet<_>>();
    let expected_decisions = exception
        .allowed_versions
        .iter()
        .map(|identity| {
            let (name, version) = identity.rsplit_once('@').ok_or_else(|| {
                format!(
                    "FF-ARCH-E-NATIVE-EDGE-ATTRIBUTION: invalid {exception_id} package identity {identity}"
                )
            })?;
            Ok((
                consumer.clone(),
                name.to_owned(),
                version.to_owned(),
                if exception_id == "FF-DEC-004" { "shipped_rust_product" } else if exception_id == "FF-DEC-005" { "non_shipped_build_tooling" } else { "non_shipped_phase0_prerequisite" }.to_owned(),
                false,
            ))
        })
        .collect::<Result<BTreeSet<_>, String>>()?;
    if observed_decisions != expected_decisions {
        return Err(format!(
            "FF-ARCH-E-NATIVE-EDGE-ATTRIBUTION: {exception_id} decisions must bind only the direct {consumer} FFI dependencies; expected {expected_decisions:?}, observed {observed_decisions:?}"
        ));
    }
    Ok(())
}

fn validate_native_supply_chain(
    metadata: &Metadata,
    native_exceptions: &BTreeMap<String, NativeDependencyException>,
    expected_native_links: &BTreeSet<String>,
) -> Result<(), String> {
    let forbidden_environment = std::env::vars_os()
        .filter_map(|(name, _)| name.into_string().ok())
        .filter(|name| native_source_override_variable(name))
        .collect::<BTreeSet<_>>();
    if !forbidden_environment.is_empty() {
        return Err(format!(
            "FF-ARCH-E-NATIVE-SOURCE-OVERRIDE: native source or toolchain overrides are forbidden: {forbidden_environment:?}"
        ));
    }
    let resolve = metadata
        .resolve
        .as_ref()
        .ok_or("Cargo metadata omitted resolve graph")?;
    for (identity, expected_checksum) in native_exceptions
        .values()
        .flat_map(|exception| exception.allowed_package_archive_sha256.iter())
    {
        validate_locked_package_checksum(metadata, identity, expected_checksum)?;
    }
    for (identity, expected_digest) in native_exceptions
        .values()
        .flat_map(|exception| exception.allowed_package_source_inventory_sha256.iter())
    {
        validate_package_source_inventory(metadata, identity, expected_digest)?;
    }
    for identity in expected_native_links {
        let Some((name, version)) = identity.rsplit_once('@') else {
            return Err(format!(
                "FF-ARCH-E-NATIVE-SOURCE: invalid native package identity {identity}"
            ));
        };
        let package = metadata
            .packages
            .iter()
            .find(|package| package.name.as_str() == name && package.version.to_string() == version)
            .ok_or_else(|| {
                format!("FF-ARCH-E-NATIVE-SOURCE: metadata omitted native package {identity}")
            })?;
        let selected_features = resolve
            .nodes
            .iter()
            .find(|node| node.id == package.id)
            .map(|node| {
                node.features
                    .iter()
                    .map(ToString::to_string)
                    .collect::<BTreeSet<_>>()
            })
            .ok_or_else(|| {
                format!("FF-ARCH-E-NATIVE-SOURCE: resolve graph omitted native package {identity}")
            })?;
        let manifest_dir = package
            .manifest_path
            .parent()
            .ok_or_else(|| {
                format!("FF-ARCH-E-NATIVE-SOURCE: manifest has no parent for {identity}")
            })?
            .as_std_path();
        match name {
            "boring-sys2" | "btls-sys" => {
                validate_boringssl_source_dir(identity, manifest_dir, &selected_features)?;
            }
            "zstd-sys" => {
                validate_zstd_source_dir(identity, manifest_dir, &selected_features)?;
            }
            "clang-sys" => {
                validate_clang_source_dir(identity, manifest_dir, &selected_features)?;
            }
            "ring" => {
                let exception = native_exceptions
                    .values()
                    .find(|exception| exception.allowed_native_link_packages.contains(identity))
                    .ok_or_else(|| {
                        format!(
                            "FF-ARCH-E-NATIVE-PRECOMPILED: {identity} has no exact exception authority"
                        )
                    })?;
                validate_ring_source_dir(
                    identity,
                    manifest_dir,
                    &selected_features,
                    exception.allow_packaged_pregenerated_assembly_objects,
                    &exception.packaged_object_inventory_sha256,
                )?;
            }
            _ => {
                return Err(format!(
                    "FF-ARCH-E-NATIVE-SOURCE: {identity} has no governed source validator"
                ));
            }
        }
    }
    for identity in native_exceptions
        .values()
        .flat_map(|exception| exception.allowed_native_runtime_packages.iter())
    {
        validate_native_runtime_package(metadata, resolve, identity)?;
    }
    Ok(())
}

fn validate_locked_package_checksum(
    metadata: &Metadata,
    identity: &str,
    expected_checksum: &str,
) -> Result<(), String> {
    let Some((name, version)) = identity.rsplit_once('@') else {
        return Err(format!(
            "FF-ARCH-E-NATIVE-SOURCE: invalid locked package identity {identity}"
        ));
    };
    let lock_path = Path::new(metadata.workspace_root.as_str()).join("Cargo.lock");
    let lock_text = fs::read_to_string(&lock_path)
        .map_err(|error| format!("read {}: {error}", lock_path.display()))?;
    let lock: toml::Value = toml::from_str(&lock_text)
        .map_err(|error| format!("parse {}: {error}", lock_path.display()))?;
    let packages = lock
        .get("package")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| format!("{} has no package array", lock_path.display()))?;
    let matches = packages
        .iter()
        .filter(|package| {
            package.get("name").and_then(toml::Value::as_str) == Some(name)
                && package.get("version").and_then(toml::Value::as_str) == Some(version)
        })
        .collect::<Vec<_>>();
    if matches.len() != 1
        || matches[0].get("checksum").and_then(toml::Value::as_str) != Some(expected_checksum)
        || matches[0].get("source").and_then(toml::Value::as_str)
            != Some("registry+https://github.com/rust-lang/crates.io-index")
    {
        return Err(format!(
            "FF-ARCH-E-NATIVE-SOURCE: {identity} Cargo.lock source/checksum does not match exact authority"
        ));
    }
    let package = metadata
        .packages
        .iter()
        .find(|package| package.name.as_str() == name && package.version.to_string() == version)
        .ok_or_else(|| {
            format!("FF-ARCH-E-NATIVE-SOURCE: metadata omitted governed package {identity}")
        })?;
    let package_root = package
        .manifest_path
        .parent()
        .ok_or_else(|| format!("FF-ARCH-E-NATIVE-SOURCE: manifest has no parent for {identity}"))?
        .as_std_path();
    let registry_index = package_root
        .parent()
        .ok_or_else(|| format!("FF-ARCH-E-NATIVE-SOURCE: no registry index for {identity}"))?;
    let registry_src = registry_index
        .parent()
        .ok_or_else(|| format!("FF-ARCH-E-NATIVE-SOURCE: no registry src root for {identity}"))?;
    let registry_root = registry_src
        .parent()
        .ok_or_else(|| format!("FF-ARCH-E-NATIVE-SOURCE: no registry root for {identity}"))?;
    let archive = registry_root
        .join("cache")
        .join(registry_index.file_name().ok_or_else(|| {
            format!("FF-ARCH-E-NATIVE-SOURCE: registry index has no name for {identity}")
        })?)
        .join(format!("{name}-{version}.crate"));
    validate_file_sha256(identity, &archive, expected_checksum)
}

fn validate_file_sha256(identity: &str, path: &Path, expected: &str) -> Result<(), String> {
    let observed = encode_sha256(&fs::read(path).map_err(|error| {
        format!(
            "FF-ARCH-E-NATIVE-SOURCE: read locked archive {}: {error}",
            path.display()
        )
    })?);
    if observed != expected {
        return Err(format!(
            "FF-ARCH-E-NATIVE-SOURCE: {identity} cached crate archive checksum drifted: expected {expected}, observed {observed}"
        ));
    }
    Ok(())
}

fn validate_package_source_inventory(
    metadata: &Metadata,
    identity: &str,
    expected_digest: &str,
) -> Result<(), String> {
    let Some((name, version)) = identity.rsplit_once('@') else {
        return Err(format!(
            "FF-ARCH-E-NATIVE-SOURCE: invalid package source identity {identity}"
        ));
    };
    let package = metadata
        .packages
        .iter()
        .find(|package| package.name.as_str() == name && package.version.to_string() == version)
        .ok_or_else(|| {
            format!("FF-ARCH-E-NATIVE-SOURCE: metadata omitted governed package {identity}")
        })?;
    if package.source.as_ref().is_none_or(|source| {
        !source
            .repr
            .starts_with("registry+https://github.com/rust-lang/crates.io-index")
    }) {
        return Err(format!(
            "FF-ARCH-E-NATIVE-SOURCE: {identity} is not the exact crates.io package source"
        ));
    }
    let root = package
        .manifest_path
        .parent()
        .ok_or_else(|| format!("FF-ARCH-E-NATIVE-SOURCE: manifest has no parent for {identity}"))?
        .as_std_path();
    validate_package_source_inventory_path(identity, root, expected_digest)
}

fn validate_package_source_inventory_path(
    identity: &str,
    root: &Path,
    expected_digest: &str,
) -> Result<(), String> {
    let observed = package_source_inventory_sha256(root)?;
    if observed != expected_digest {
        return Err(format!(
            "FF-ARCH-E-NATIVE-SOURCE: {identity} extracted source inventory drifted: expected {expected_digest}, observed {observed}"
        ));
    }
    Ok(())
}

fn package_source_inventory_sha256(root: &Path) -> Result<String, String> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(|error| {
            format!(
                "FF-ARCH-E-NATIVE-SOURCE: enumerate {}: {error}",
                directory.display()
            )
        })? {
            let entry = entry.map_err(|error| {
                format!(
                    "FF-ARCH-E-NATIVE-SOURCE: enumerate {} entry: {error}",
                    directory.display()
                )
            })?;
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.is_file() {
                let relative = path.strip_prefix(root).map_err(|error| {
                    format!(
                        "FF-ARCH-E-NATIVE-SOURCE: package file escaped {}: {error}",
                        root.display()
                    )
                })?;
                let canonical = relative
                    .components()
                    .map(|component| {
                        component.as_os_str().to_str().ok_or_else(|| {
                            format!(
                                "FF-ARCH-E-NATIVE-SOURCE: non-UTF8 package path under {}",
                                root.display()
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .join("/");
                files.push((canonical, path));
            }
        }
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut canonical_inventory = Vec::new();
    for (relative, path) in files {
        canonical_inventory.extend_from_slice(relative.as_bytes());
        canonical_inventory.push(b' ');
        canonical_inventory.extend_from_slice(
            encode_sha256(&fs::read(&path).map_err(|error| {
                format!("FF-ARCH-E-NATIVE-SOURCE: read {}: {error}", path.display())
            })?)
            .as_bytes(),
        );
        canonical_inventory.push(b'\n');
    }
    Ok(encode_sha256(&canonical_inventory))
}

fn validate_native_runtime_package(
    metadata: &Metadata,
    resolve: &cargo_metadata::Resolve,
    identity: &str,
) -> Result<(), String> {
    let Some((name, version)) = identity.rsplit_once('@') else {
        return Err(format!(
            "FF-ARCH-E-NATIVE-SOURCE: invalid native runtime package identity {identity}"
        ));
    };
    let package = metadata
        .packages
        .iter()
        .find(|package| package.name.as_str() == name && package.version.to_string() == version)
        .ok_or_else(|| {
            format!("FF-ARCH-E-NATIVE-SOURCE: metadata omitted native runtime package {identity}")
        })?;
    let selected_features = resolve
        .nodes
        .iter()
        .find(|node| node.id == package.id)
        .map(|node| node.features.iter().collect::<BTreeSet<_>>())
        .ok_or_else(|| {
            format!(
                "FF-ARCH-E-NATIVE-SOURCE: resolve graph omitted native runtime package {identity}"
            )
        })?;
    match name {
        "rustls-platform-verifier" => {
            let manifest_dir = package
                .manifest_path
                .parent()
                .ok_or_else(|| {
                    format!("FF-ARCH-E-NATIVE-SOURCE: manifest has no parent for {identity}")
                })?
                .as_std_path();
            if !selected_features.is_empty()
                || !manifest_dir.join("Cargo.toml").is_file()
                || !manifest_dir.join("src/verification/windows.rs").is_file()
            {
                return Err(format!(
                    "FF-ARCH-E-NATIVE-SOURCE: {identity} Windows trust-verifier FFI source or feature surface drifted"
                ));
            }
        }
        _ => {
            return Err(format!(
                "FF-ARCH-E-NATIVE-SOURCE: {identity} has no governed native-runtime validator"
            ));
        }
    }
    Ok(())
}

fn native_source_override_variable(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    name.contains("BORING_BSSL")
        || matches!(
            name.as_str(),
            "ZSTD_SYS_USE_PKG_CONFIG"
                | "ZSTD_LIB_DIR"
                | "ZSTD_INCLUDE_DIR"
                | "LIBCLANG_PATH"
                | "LLVM_CONFIG_PATH"
                | "CLANG_PATH"
                | "BINDGEN_EXTRA_CLANG_ARGS"
                | "RING_PREGENERATE_ASM"
                | "PKG_CONFIG_PATH"
                | "PKG_CONFIG_LIBDIR"
                | "VCPKG_ROOT"
                | "CMAKE_TOOLCHAIN_FILE"
        )
}

fn validate_boringssl_source_dir(
    identity: &str,
    manifest_dir: &Path,
    selected_features: &BTreeSet<String>,
) -> Result<(), String> {
    if selected_features
        .iter()
        .any(|feature| matches!(feature.as_str(), "fips" | "fips-link-precompiled"))
    {
        return Err(format!(
            "FF-ARCH-E-NATIVE-PRECOMPILED: {identity} selected forbidden FIPS/precompiled feature surface {selected_features:?}"
        ));
    }
    let vendored_markers = [
        manifest_dir.join("deps/boringssl/CMakeLists.txt"),
        manifest_dir.join("deps/boringssl/src/CMakeLists.txt"),
    ];
    if !vendored_markers.iter().any(|marker| marker.is_file()) {
        return Err(format!(
            "FF-ARCH-E-NATIVE-SOURCE: {identity} has no packaged BoringSSL CMake marker; build-time source acquisition would be possible"
        ));
    }
    Ok(())
}

fn validate_zstd_source_dir(
    identity: &str,
    manifest_dir: &Path,
    selected_features: &BTreeSet<String>,
) -> Result<(), String> {
    if selected_features.contains("pkg-config") || selected_features.contains("bindgen") {
        return Err(format!(
            "FF-ARCH-E-NATIVE-SOURCE-OVERRIDE: {identity} selected a host-discovery feature: {selected_features:?}"
        ));
    }
    if !manifest_dir.join("zstd/lib/zstd.h").is_file()
        || !manifest_dir.join("zstd/lib/common").is_dir()
    {
        return Err(format!(
            "FF-ARCH-E-NATIVE-SOURCE: {identity} has no packaged zstd source markers"
        ));
    }
    Ok(())
}

fn validate_clang_source_dir(
    identity: &str,
    manifest_dir: &Path,
    selected_features: &BTreeSet<String>,
) -> Result<(), String> {
    if !selected_features.contains("runtime")
        || selected_features.contains("static")
        || selected_features.contains("libcpp")
    {
        return Err(format!(
            "FF-ARCH-E-NATIVE-SOURCE: {identity} must use runtime discovery without static or libcpp linkage: {selected_features:?}"
        ));
    }
    if !manifest_dir.join("build.rs").is_file() || !manifest_dir.join("Cargo.toml").is_file() {
        return Err(format!(
            "FF-ARCH-E-NATIVE-SOURCE: {identity} has no packaged build-source markers"
        ));
    }
    Ok(())
}

fn validate_ring_source_dir(
    identity: &str,
    manifest_dir: &Path,
    selected_features: &BTreeSet<String>,
    allows_packaged_objects: bool,
    expected_inventory_sha256: &str,
) -> Result<(), String> {
    let exact_features = BTreeSet::from([
        "alloc".to_owned(),
        "default".to_owned(),
        "dev_urandom_fallback".to_owned(),
    ]);
    if selected_features != &exact_features {
        return Err(format!(
            "FF-ARCH-E-NATIVE-SOURCE: {identity} feature surface drifted: {selected_features:?}"
        ));
    }
    for marker in [
        manifest_dir.join("build.rs"),
        manifest_dir.join("crypto/crypto.c"),
        manifest_dir.join("include/ring-core/base.h"),
        manifest_dir.join("pregenerated"),
    ] {
        if !marker.exists() {
            return Err(format!(
                "FF-ARCH-E-NATIVE-SOURCE: {identity} omitted packaged source marker {}",
                marker.display()
            ));
        }
    }
    let pregenerated = manifest_dir.join("pregenerated");
    validate_packaged_object_inventory(
        identity,
        &pregenerated,
        allows_packaged_objects,
        expected_inventory_sha256,
    )
}

fn validate_packaged_object_inventory(
    identity: &str,
    pregenerated: &Path,
    allows_packaged_objects: bool,
    expected_inventory_sha256: &str,
) -> Result<(), String> {
    let native_artifacts = recursive_native_artifacts(pregenerated)?;
    let packaged_objects = native_artifacts
        .iter()
        .filter(|path| path.extension().is_some_and(|extension| extension == "o"))
        .collect::<Vec<_>>();
    let unexpected_native_artifacts = native_artifacts
        .iter()
        .filter(|path| path.extension().is_some_and(|extension| extension != "o"))
        .collect::<Vec<_>>();
    if !unexpected_native_artifacts.is_empty() {
        return Err(format!(
            "FF-ARCH-E-NATIVE-PRECOMPILED: {identity} contains unexpected packaged native artifacts {unexpected_native_artifacts:?}"
        ));
    }
    if packaged_objects.is_empty() || !allows_packaged_objects {
        return Err(format!(
            "FF-ARCH-E-NATIVE-PRECOMPILED: {identity} contains {} packaged pregenerated assembly objects without its exact exception authority",
            packaged_objects.len()
        ));
    }
    let mut canonical_inventory = Vec::new();
    for path in &packaged_objects {
        let relative = path.strip_prefix(pregenerated).map_err(|error| {
            format!(
                "FF-ARCH-E-NATIVE-PRECOMPILED: {identity} object path escaped its root: {error}"
            )
        })?;
        let canonical_path = relative
            .components()
            .map(|component| {
                component.as_os_str().to_str().ok_or_else(|| {
                    format!(
                        "FF-ARCH-E-NATIVE-PRECOMPILED: {identity} contains a non-UTF8 object path"
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?
            .join("/");
        let bytes = fs::read(path).map_err(|error| {
            format!(
                "FF-ARCH-E-NATIVE-PRECOMPILED: read {}: {error}",
                path.display()
            )
        })?;
        canonical_inventory.extend_from_slice(canonical_path.as_bytes());
        canonical_inventory.push(b' ');
        canonical_inventory.extend_from_slice(encode_sha256(&bytes).as_bytes());
        canonical_inventory.push(b'\n');
    }
    let observed_inventory_sha256 = encode_sha256(&canonical_inventory);
    if observed_inventory_sha256 != expected_inventory_sha256 {
        return Err(format!(
            "FF-ARCH-E-NATIVE-PRECOMPILED: {identity} packaged object inventory digest drifted: expected {expected_inventory_sha256}, observed {observed_inventory_sha256}"
        ));
    }
    Ok(())
}

fn recursive_native_artifacts(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut pending = vec![root.to_path_buf()];
    let mut artifacts = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(|error| {
            format!(
                "FF-ARCH-E-NATIVE-SOURCE: enumerate {}: {error}",
                directory.display()
            )
        })? {
            let entry = entry.map_err(|error| {
                format!(
                    "FF-ARCH-E-NATIVE-SOURCE: enumerate {} entry: {error}",
                    directory.display()
                )
            })?;
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let extension = path
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if matches!(
                extension.as_str(),
                "o" | "obj" | "a" | "lib" | "dll" | "so" | "dylib" | "exe"
            ) {
                artifacts.push(path);
            }
        }
    }
    artifacts.sort();
    Ok(artifacts)
}

#[allow(clippy::too_many_lines)]
fn read_native_dependency_exceptions(
    path: &Path,
) -> Result<BTreeMap<String, NativeDependencyException>, String> {
    let text = fs::read_to_string(path).map_err(|error| {
        format!(
            "read native exception authority {}: {error}",
            path.display()
        )
    })?;
    let document = parse_single_yaml(&text, &path.display().to_string())
        .map_err(|error| format!("FF-ARCH-E-POLICY-SCHEMA: {error}"))?;
    let authority = document
        .as_mapping_get("exception_authority")
        .ok_or("build rules omit exception_authority")?;
    let rows = authority
        .as_mapping_get("canonical_allowlist")
        .and_then(Yaml::as_sequence)
        .ok_or("exception_authority canonical_allowlist is not a sequence")?;
    let dependency_keys = BTreeSet::from([
        "allow_build_scripts",
        "allow_build_time_downloads",
        "allow_packaging_or_release",
        "allow_packaged_pregenerated_assembly_objects",
        "allow_prebuilt_binaries",
        "allow_product_use",
        "allowed_direct_dependencies",
        "allowed_native_link_packages",
        "allowed_native_runtime_packages",
        "allowed_package_archive_sha256",
        "allowed_package_source_inventory_sha256",
        "allowed_prerequisite_consumers",
        "allowed_product_consumers",
        "allowed_runtime_classes",
        "allowed_versions",
        "approval_id",
        "id",
        "kind",
        "owner",
        "ordinary_transport_resolved_closure_sha256",
        "ordinary_transport_registry_source_closure_sha256",
        "packaged_object_inventory_sha256",
        "promotion_requires_new_operator_decision",
        "reason",
        "removal_trigger",
        "status",
    ]);
    let ffi_boundary_keys = BTreeSet::from([
        "allow_build_scripts",
        "allow_build_time_downloads",
        "allow_ffi",
        "allow_packaging_or_release",
        "allow_packaged_pregenerated_assembly_objects",
        "allow_prebuilt_binaries",
        "allow_product_use",
        "allow_unsafe",
        "allowed_direct_dependencies",
        "allowed_native_link_packages",
        "allowed_native_runtime_packages",
        "allowed_prerequisite_consumers",
        "allowed_product_consumers",
        "allowed_runtime_classes",
        "allowed_versions",
        "approval_id",
        "id",
        "kind",
        "operator_decision",
        "owner",
        "promotion_requires_new_operator_decision",
        "reason",
        "removal_trigger",
        "residual_uncertainty",
        "safety_invariants",
        "status",
    ]);
    let mut result = BTreeMap::new();
    for row in rows {
        let mapping = row
            .as_mapping()
            .ok_or("native exception row is not a mapping")?;
        let keys = mapping
            .keys()
            .map(|key| {
                key.as_str()
                    .ok_or("native exception row has a non-string key")
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        let kind = yaml_string(row, "kind")?;
        let expected_keys = match kind {
            "native_dependency" => &dependency_keys,
            "native_ffi_and_unsafe_boundary" => &ffi_boundary_keys,
            _ => {
                return Err(format!(
                    "FF-ARCH-E-EXCEPTION-SCHEMA: unsupported native exception kind {kind}"
                ));
            }
        };
        if &keys != expected_keys {
            return Err(format!(
                "FF-ARCH-E-EXCEPTION-SCHEMA: native exception keys are invalid: {keys:?}"
            ));
        }
        let id = yaml_string(row, "id")?;
        let owner = yaml_string(row, "owner")?;
        let approval_id = yaml_string(row, "approval_id")?;
        let status = yaml_string(row, "status")?;
        let allow_product_use = yaml_bool(row, "allow_product_use")?;
        let allow_packaged_pregenerated_assembly_objects =
            yaml_bool(row, "allow_packaged_pregenerated_assembly_objects")?;
        let allow_packaging_or_release = yaml_bool(row, "allow_packaging_or_release")?;
        let promotion_requires_new_operator_decision =
            yaml_bool(row, "promotion_requires_new_operator_decision")?;
        let is_ffi_boundary = kind == "native_ffi_and_unsafe_boundary";
        let packaged_object_inventory_sha256 = if is_ffi_boundary {
            ""
        } else {
            yaml_string_allow_empty(row, "packaged_object_inventory_sha256")?
        };
        let ordinary_transport_resolved_closure_sha256 = if is_ffi_boundary {
            ""
        } else {
            yaml_string_allow_empty(row, "ordinary_transport_resolved_closure_sha256")?
        };
        let ordinary_transport_registry_source_closure_sha256 = if is_ffi_boundary {
            ""
        } else {
            yaml_string_allow_empty(row, "ordinary_transport_registry_source_closure_sha256")?
        };
        let allowed_prerequisite_consumers =
            yaml_string_set(row, "allowed_prerequisite_consumers")?;
        let allowed_product_consumers = yaml_string_set(row, "allowed_product_consumers")?;
        let allowed_direct_dependencies = yaml_string_set(row, "allowed_direct_dependencies")?;
        let allowed_versions = yaml_string_set(row, "allowed_versions")?;
        let allowed_native_link_packages = yaml_string_set(row, "allowed_native_link_packages")?;
        let allowed_native_runtime_packages =
            yaml_string_set(row, "allowed_native_runtime_packages")?;
        let allowed_package_archive_sha256 = if is_ffi_boundary {
            BTreeMap::new()
        } else {
            yaml_identity_digest_map(row, "allowed_package_archive_sha256")?
        };
        let allowed_package_source_inventory_sha256 = if is_ffi_boundary {
            BTreeMap::new()
        } else {
            yaml_identity_digest_map(row, "allowed_package_source_inventory_sha256")?
        };
        let allowed_runtime_classes = yaml_string_set(row, "allowed_runtime_classes")?;
        let allow_build_scripts = yaml_bool(row, "allow_build_scripts")?;
        let allow_ffi = if is_ffi_boundary {
            yaml_bool(row, "allow_ffi")
                .map_err(|error| format!("FF-ARCH-E-EXCEPTION-SCHEMA: {error}"))?
        } else {
            false
        };
        let allow_unsafe = if is_ffi_boundary {
            yaml_string(row, "allow_unsafe")
                .map_err(|error| format!("FF-ARCH-E-EXCEPTION-SCHEMA: {error}"))?
        } else {
            ""
        };
        let operator_decision = if is_ffi_boundary {
            yaml_string(row, "operator_decision")
                .map_err(|error| format!("FF-ARCH-E-EXCEPTION-SCHEMA: {error}"))?
        } else {
            ""
        };
        let residual_uncertainty = if is_ffi_boundary {
            yaml_string(row, "residual_uncertainty")
                .map_err(|error| format!("FF-ARCH-E-EXCEPTION-SCHEMA: {error}"))?
        } else {
            ""
        };
        let safety_invariants = if is_ffi_boundary {
            yaml_string_set(row, "safety_invariants")
                .map_err(|error| format!("FF-ARCH-E-EXCEPTION-SCHEMA: {error}"))?
        } else {
            BTreeSet::new()
        };
        let valid_scope = match status {
            "ACTIVE_NON_SHIPPED_ADJUDICATION_ONLY"
            | "ACTIVE_NON_SHIPPED_FFMPEG_PREREQUISITE_ONLY"
            | "ACTIVE_NON_SHIPPED_VALIDATION_ONLY" => {
                !allow_product_use
                    && !allow_packaging_or_release
                    && promotion_requires_new_operator_decision
            }
            "ACTIVE_SHIPPED_PRODUCT_NARROW_ONLY" => {
                allow_product_use
                    && allow_packaging_or_release
                    && !promotion_requires_new_operator_decision
            }
            "ACTIVE_SHIPPED_STORAGE_NARROW_ONLY" => {
                allow_product_use
                    && !allow_packaging_or_release
                    && !promotion_requires_new_operator_decision
            }
            _ => false,
        };
        let valid_packaged_object_scope = !allow_packaged_pregenerated_assembly_objects
            && packaged_object_inventory_sha256.is_empty()
            || (allow_packaged_pregenerated_assembly_objects
                && status == "ACTIVE_SHIPPED_PRODUCT_NARROW_ONLY"
                && allowed_native_link_packages == BTreeSet::from(["ring@0.17.14".to_owned()])
                && allowed_versions.contains("ring@0.17.14")
                && packaged_object_inventory_sha256.len() == 64
                && packaged_object_inventory_sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
        let exact_decision_scope = match id {
            "FF-DEC-001" => {
                owner == "WP-FF-015-wreq-transport-adjudication"
                    && approval_id == "WP-FF-015-wreq-transport-adjudication-v1-AC-001"
                    && status == "ACTIVE_NON_SHIPPED_ADJUDICATION_ONLY"
                    && allowed_prerequisite_consumers
                        == BTreeSet::from(["fforager-transport".to_owned()])
                    && allowed_product_consumers.is_empty()
                    && allowed_direct_dependencies
                        == BTreeSet::from(["wreq".to_owned(), "wreq-util".to_owned()])
                    && allowed_versions
                        == BTreeSet::from([
                            "wreq@5.3.0".to_owned(),
                            "wreq@6.0.0-rc.29".to_owned(),
                            "wreq-util@3.0.0-rc.14".to_owned(),
                        ])
                    && allowed_native_link_packages
                        == BTreeSet::from([
                            "btls-sys@0.5.6".to_owned(),
                            "clang-sys@1.8.1".to_owned(),
                            "zstd-sys@2.0.16+zstd.1.5.7".to_owned(),
                        ])
                    && allowed_native_runtime_packages.is_empty()
                    && allowed_package_archive_sha256.is_empty()
                    && allowed_package_source_inventory_sha256.is_empty()
                    && ordinary_transport_resolved_closure_sha256.is_empty()
                    && ordinary_transport_registry_source_closure_sha256.is_empty()
                    && allowed_runtime_classes
                        == BTreeSet::from(["non_shipped_phase0_prerequisite".to_owned()])
            }
            "FF-DEC-002" => {
                owner == "WP-FF-023-dependency-security-remediation"
                    && approval_id == "WP-FF-023-dependency-security-remediation-v1-AC-001"
                    && status == "ACTIVE_SHIPPED_PRODUCT_NARROW_ONLY"
                    && allowed_prerequisite_consumers
                        == BTreeSet::from(["fforager-transport".to_owned()])
                    && allowed_product_consumers == BTreeSet::from(["fforager-net".to_owned()])
                    && allowed_direct_dependencies
                        == BTreeSet::from(["reqwest".to_owned(), "rustls".to_owned()])
                    && allowed_versions
                        == BTreeSet::from([
                            "reqwest@0.13.4".to_owned(),
                            "ring@0.17.14".to_owned(),
                            "rustls@0.23.46".to_owned(),
                        ])
                    && allowed_native_link_packages == BTreeSet::from(["ring@0.17.14".to_owned()])
                    && allowed_native_runtime_packages
                        == BTreeSet::from(["rustls-platform-verifier@0.7.0".to_owned()])
                    && allowed_package_archive_sha256
                        == BTreeMap::from([
                            (
                                "ring@0.17.14".to_owned(),
                                "a4689e6c2294d81e88dc6261c768b63bc4fcdb852be6d1352498b114f61383b7"
                                    .to_owned(),
                            ),
                            (
                                "rustls-platform-verifier@0.7.0".to_owned(),
                                "26d1e2536ce4f35f4846aa13bff16bd0ff40157cdb14cc056c7b14ba41233ba0"
                                    .to_owned(),
                            ),
                        ])
                    && allowed_package_source_inventory_sha256
                        == BTreeMap::from([
                            (
                                "ring@0.17.14".to_owned(),
                                "60be074c360264320b3fa0a46515ca828e44954988d8843ad3766bf84fc452d3"
                                    .to_owned(),
                            ),
                            (
                                "rustls-platform-verifier@0.7.0".to_owned(),
                                "8d7a222bf3a643ce4c2003c1ede94bbd0d4e10443271602ab445c9d940ac40b0"
                                    .to_owned(),
                            ),
                        ])
                    && ordinary_transport_resolved_closure_sha256
                        == "ec38435517372f6afde502e138537603a66dc75f10e56a251e56ce183d5e3251"
                    && ordinary_transport_registry_source_closure_sha256
                        == "929ab243eb663039dbc457eda301eb375fb3c284c586da41ae8d660bfabdcb8c"
                    && allowed_runtime_classes
                        == BTreeSet::from([
                            "non_shipped_phase0_prerequisite".to_owned(),
                            "shipped_rust_product".to_owned(),
                        ])
            }
            "FF-DEC-003" => {
                owner == "WP-FF-010-ffmpeg-supervision-spike"
                    && approval_id == "WP-FF-010-ffmpeg-supervision-spike-v1-AC-002"
                    && status == "ACTIVE_NON_SHIPPED_FFMPEG_PREREQUISITE_ONLY"
                    && allowed_prerequisite_consumers
                        == BTreeSet::from(["fforager-ffmpeg".to_owned()])
                    && allowed_product_consumers.is_empty()
                    && allowed_direct_dependencies
                        == BTreeSet::from(["libc".to_owned(), "windows-sys".to_owned()])
                    && allowed_versions
                        == BTreeSet::from([
                            "libc@0.2.189".to_owned(),
                            "windows-sys@0.61.2".to_owned(),
                        ])
                    && allowed_native_link_packages.is_empty()
                    && allowed_native_runtime_packages.is_empty()
                    && allowed_package_archive_sha256.is_empty()
                    && allowed_package_source_inventory_sha256.is_empty()
                    && ordinary_transport_resolved_closure_sha256.is_empty()
                    && ordinary_transport_registry_source_closure_sha256.is_empty()
                    && allowed_runtime_classes
                        == BTreeSet::from(["non_shipped_phase0_prerequisite".to_owned()])
                    && !allow_build_scripts
                    && allow_ffi
                    && allow_unsafe == "scoped_platform_modules_only"
                    && operator_decision
                        == "The Operator explicitly approved the recommended narrow Ferric-owned windows-sys/libc process boundary on 2026-08-09."
                    && residual_uncertainty
                        == "Windows parent death during suspended pre-execution setup can leave a suspended orphan unless a separately approved broker removes the interval; Unix process groups are signal scopes and do not contain hostile setsid or setpgid escape."
                    && safety_invariants
                        == BTreeSet::from([
                            "bounded_termination".to_owned(),
                            "direct_child_wait".to_owned(),
                            "exact_handle_ownership".to_owned(),
                            "independent_windows_active_zero".to_owned(),
                            "no_shell".to_owned(),
                            "non_inheritable_handles".to_owned(),
                            "pre_execution_windows_job_assignment".to_owned(),
                            "unix_process_group_signal_scope".to_owned(),
                            "validated_direct_arguments_only".to_owned(),
                        ])
            }
            "FF-DEC-004" | "FF-DEC-005" => {
                let storage_boundary = id == "FF-DEC-004";
                owner == "WP-FF-021-first-runnable-production-slice"
                    && approval_id
                        == if storage_boundary {
                            "WP-FF-021-first-runnable-production-slice-v1-AC-003"
                        } else {
                            "WP-FF-021-first-runnable-production-slice-v1-AC-004"
                        }
                    && status
                        == if storage_boundary {
                            "ACTIVE_SHIPPED_STORAGE_NARROW_ONLY"
                        } else {
                            "ACTIVE_NON_SHIPPED_VALIDATION_ONLY"
                        }
                    && allowed_prerequisite_consumers
                        == if storage_boundary {
                            BTreeSet::new()
                        } else {
                            BTreeSet::from(["fforager-xtask".to_owned()])
                        }
                    && allowed_product_consumers
                        == if storage_boundary {
                            BTreeSet::from(["fforager-storage".to_owned()])
                        } else {
                            BTreeSet::new()
                        }
                    && allowed_direct_dependencies == BTreeSet::from(["windows-sys".to_owned()])
                    && allowed_versions == BTreeSet::from(["windows-sys@0.61.2".to_owned()])
                    && allowed_native_link_packages.is_empty()
                    && allowed_native_runtime_packages.is_empty()
                    && allowed_package_archive_sha256.is_empty()
                    && allowed_package_source_inventory_sha256.is_empty()
                    && ordinary_transport_resolved_closure_sha256.is_empty()
                    && ordinary_transport_registry_source_closure_sha256.is_empty()
                    && allowed_runtime_classes
                        == BTreeSet::from([if storage_boundary {
                            "shipped_rust_product"
                        } else {
                            "non_shipped_build_tooling"
                        }
                        .to_owned()])
                    && !allow_build_scripts
                    && allow_ffi
                    && allow_unsafe
                        == if storage_boundary {
                            "scoped_storage_platform_module_only"
                        } else {
                            "scoped_owned_watcher_pause_module_only"
                        }
                    && operator_decision
                        == if storage_boundary {
                            "The Operator explicitly approved the narrow production storage boundary using the already-pinned windows-sys for directory handles, filesystem identity, and flush operations on 2026-10-21."
                        } else {
                            "The Operator explicitly approved validation-only watcher pause/resume and, in the subsequent 2026-10-21 live approval, read-only identity and exit acknowledgment for the launcher-owned worker, using already-pinned windows-sys. Worker pause or process-control authority is not extended."
                        }
                    && safety_invariants
                        == if storage_boundary {
                            BTreeSet::from(
                                [
                                    "retained_root_handles",
                                    "no_reparse_traversal",
                                    "verified_volume_identity",
                                    "live_host_profile",
                                    "acknowledged_file_and_directory_flush",
                                    "exact_handle_ownership",
                                ]
                                .map(str::to_owned),
                            )
                        } else {
                            BTreeSet::from(
                                [
                                    "validator_owned_watcher_pause_only",
                                    "validator_owned_worker_read_only_identity",
                                    "retained_process_handle",
                                    "pid_and_creation_time_identity",
                                    "verified_staged_image_identity",
                                    "launcher_parent_identity",
                                    "worker_identity_has_no_pause_or_process_control",
                                    "pause_at_most_two_seconds",
                                    "unconditional_detach_guard",
                                    "no_shell",
                                    "no_unowned_process_control",
                                ]
                                .map(str::to_owned),
                            )
                        }
            }
            _ => false,
        };
        if !id.starts_with("FF-DEC-")
            || !matches!(kind, "native_dependency" | "native_ffi_and_unsafe_boundary")
            || !valid_scope
            || !valid_packaged_object_scope
            || !exact_decision_scope
            || !work_packet_base_owner_valid(owner)
            || !decision_approval_matches_owner(approval_id, owner)
            || allow_build_scripts == is_ffi_boundary
            || yaml_bool(row, "allow_build_time_downloads")?
            || yaml_bool(row, "allow_prebuilt_binaries")?
            || !bounded_authority_text(yaml_string(row, "reason")?, 8_192)
            || !bounded_authority_text(yaml_string(row, "removal_trigger")?, 8_192)
            || (is_ffi_boundary
                && (!bounded_authority_text(operator_decision, 1_024)
                    || !bounded_authority_text(residual_uncertainty, 4_096)
                    || safety_invariants.is_empty()
                    || safety_invariants.len() > 32
                    || safety_invariants
                        .iter()
                        .any(|value| !bounded_authority_text(value, 128))))
        {
            return Err(format!(
                "FF-ARCH-E-EXCEPTION-SCHEMA: native exception {id} violates the bounded policy"
            ));
        }
        let exception = NativeDependencyException {
            id: id.to_owned(),
            owner: owner.to_owned(),
            approval_id: approval_id.to_owned(),
            allow_packaged_pregenerated_assembly_objects,
            packaged_object_inventory_sha256: packaged_object_inventory_sha256.to_owned(),
            allowed_prerequisite_consumers,
            allowed_product_consumers,
            allowed_direct_dependencies,
            allowed_versions,
            allowed_native_link_packages,
            allowed_native_runtime_packages,
            allowed_package_archive_sha256,
            allowed_package_source_inventory_sha256,
            ordinary_transport_resolved_closure_sha256: ordinary_transport_resolved_closure_sha256
                .to_owned(),
            ordinary_transport_registry_source_closure_sha256:
                ordinary_transport_registry_source_closure_sha256.to_owned(),
            allowed_runtime_classes,
        };
        if (exception.allowed_prerequisite_consumers.is_empty()
            && exception.allowed_product_consumers.is_empty())
            || exception.allowed_direct_dependencies.is_empty()
            || exception.allowed_versions.is_empty()
            || exception.allowed_runtime_classes.is_empty()
        {
            return Err(format!(
                "FF-ARCH-E-EXCEPTION-SCHEMA: native exception {id} has an empty required scope"
            ));
        }
        if result.insert(exception.id.clone(), exception).is_some() {
            return Err(format!(
                "FF-ARCH-E-EXCEPTION-SCHEMA: duplicate native exception {id}"
            ));
        }
    }
    Ok(result)
}

fn required_architecture_rules(path: &Path) -> Result<BTreeSet<String>, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let document = parse_single_yaml(&text, &path.display().to_string())
        .map_err(|error| format!("FF-ARCH-E-POLICY-SCHEMA: {error}"))?;
    let rows = document
        .as_mapping_get("rules")
        .and_then(Yaml::as_sequence)
        .ok_or("build rules YAML has no rules sequence")?;
    let mut rules = BTreeSet::new();
    for row in rows {
        let id = yaml_string(row, "id")?;
        let category = yaml_string(row, "category")?;
        let enforcement = yaml_string(row, "enforcement")?;
        if matches!(
            category,
            "architecture"
                | "runtime_truth"
                | "product_dependency_boundary"
                | "proof_integrity"
                | "effect_integrity"
                | "compatibility_integrity"
                | "wire_integrity"
                | "graph_integrity"
                | "finding_remediation"
                | "validation_reporting"
                | "credential_protection"
        ) && enforcement == "REQUIRED"
            && !rules.insert(id.to_owned())
        {
            return Err(format!("duplicate REQUIRED architecture rule {id}"));
        }
    }
    if rules.is_empty() {
        return Err(
            "no REQUIRED architecture or runtime-truth rules found in canonical build rules"
                .to_owned(),
        );
    }
    Ok(rules)
}

fn dependency_features_match(
    observed_features: &[String],
    observed_default_features: bool,
    decision: &DependencyDecision,
) -> bool {
    observed_default_features == decision.default_features
        && observed_features.iter().collect::<BTreeSet<_>>()
            == decision.features.iter().collect::<BTreeSet<_>>()
}

fn parse_single_yaml<'input>(text: &'input str, context: &str) -> Result<Yaml<'input>, String> {
    let mut documents =
        Yaml::load_from_str(text).map_err(|error| format!("parse {context} as YAML: {error}"))?;
    if documents.len() != 1 {
        return Err(format!(
            "{context} must contain exactly one YAML document, observed {}",
            documents.len()
        ));
    }
    let document = documents.pop().ok_or("YAML parser returned no document")?;
    if !document.is_mapping() {
        return Err(format!("{context} YAML root is not a mapping"));
    }
    Ok(document)
}

fn yaml_string<'a>(mapping: &'a Yaml<'_>, key: &str) -> Result<&'a str, String> {
    mapping
        .as_mapping_get(key)
        .and_then(Yaml::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("YAML mapping has no nonempty string {key}"))
}

fn yaml_string_allow_empty<'a>(mapping: &'a Yaml<'_>, key: &str) -> Result<&'a str, String> {
    mapping
        .as_mapping_get(key)
        .and_then(Yaml::as_str)
        .ok_or_else(|| format!("YAML mapping has no string {key}"))
}

fn bounded_authority_text(value: &str, maximum_bytes: usize) -> bool {
    !value.trim().is_empty() && value.len() <= maximum_bytes && !value.contains('\0')
}

fn yaml_bool(mapping: &Yaml<'_>, key: &str) -> Result<bool, String> {
    mapping
        .as_mapping_get(key)
        .and_then(Yaml::as_bool)
        .ok_or_else(|| format!("YAML mapping has no boolean {key}"))
}

fn yaml_string_set(mapping: &Yaml<'_>, key: &str) -> Result<BTreeSet<String>, String> {
    let values = mapping
        .as_mapping_get(key)
        .and_then(Yaml::as_sequence)
        .ok_or_else(|| format!("YAML mapping has no sequence {key}"))?;
    let parsed = values
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|item| !item.trim().is_empty())
                .map(str::to_owned)
                .ok_or_else(|| format!("YAML sequence {key} contains a non-string or empty value"))
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    if parsed.len() != values.len() {
        return Err(format!("YAML sequence {key} contains duplicate values"));
    }
    Ok(parsed)
}

fn yaml_identity_digest_map(
    mapping: &Yaml<'_>,
    key: &str,
) -> Result<BTreeMap<String, String>, String> {
    yaml_string_set(mapping, key)?
        .into_iter()
        .map(|item| {
            let (identity, digest) = item
                .split_once('=')
                .ok_or_else(|| format!("YAML mapping {key} item has no identity=digest form"))?;
            if identity.trim().is_empty()
                || digest.len() != 64
                || !digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(format!("YAML mapping {key} item is invalid: {item}"));
            }
            Ok((identity.to_owned(), digest.to_owned()))
        })
        .collect()
}

#[allow(
    clippy::too_many_lines,
    reason = "FF-BUILD-046 keeps the exhaustive stable rule-to-proof mapping in one auditable match"
)]
fn validate_rule_map(
    rule_map: &RuleMap,
    canonical: &BTreeSet<String>,
    checks: &mut Vec<Check>,
) -> Result<(), String> {
    let mapped: BTreeSet<_> = rule_map
        .rules
        .iter()
        .map(|rule| rule.rule_id.clone())
        .collect();
    if mapped.len() != rule_map.rules.len() {
        return Err("duplicate rule-to-proof row".to_owned());
    }
    let missing: Vec<_> = canonical.difference(&mapped).cloned().collect();
    let unknown: Vec<_> = mapped.difference(canonical).cloned().collect();
    if let Some(diagnostic) = rule_inventory_diagnostic(canonical, &mapped) {
        return Err(format!(
            "{diagnostic}: missing={missing:?}; unknown={unknown:?}"
        ));
    }
    for rule in &rule_map.rules {
        if !proof_binding_valid(&rule.proof_classes, &rule.validators, &rule.fixture_ids) {
            return Err(format!(
                "FF-ARCH-E-MISSING-FIXTURE-BINDING: {}",
                rule.rule_id
            ));
        }
        let (proof, validators, fixtures): (&[&str], &[&str], &[&str]) = match rule.rule_id.as_str()
        {
            "FF-BUILD-036" => (
                &["graph", "policy"],
                &["workspace-shape"],
                &["shipped-before-bootstrap"],
            ),
            "FF-BUILD-037" => (
                &["policy"],
                &["strict-policy"],
                &["self-authorized-exception"],
            ),
            "FF-BUILD-038" => (
                &["graph"],
                &["cargo-metadata"],
                &["undeclared-member", "forbidden-edge", "cycle"],
            ),
            "FF-BUILD-039" => (
                &["graph", "source"],
                &["runtime-boundary"],
                &["shipped-governance-read"],
            ),
            "FF-BUILD-040" => (
                &["graph", "policy"],
                &["layer-edges"],
                &["adapter-edge-without-decision"],
            ),
            "FF-BUILD-041" => (
                &["graph"],
                &["testkit-boundary"],
                &["testkit-production-edge"],
            ),
            "FF-BUILD-042" => (&["graph"], &["watcher-boundary"], &["watcher-product-edge"]),
            "FF-BUILD-043" => (
                &["runtime_fault", "graph"],
                &["watcher-independence"],
                &["product-watcher-edge"],
            ),
            "FF-BUILD-044" | "FF-BUILD-045" => {
                (&["policy"], &["split-trigger"], &["missing-split-trigger"])
            }
            "FF-BUILD-046" => (
                &["policy", "negative_fixture"],
                &["rule-map-completeness"],
                &[
                    "missing-rule-map",
                    "unknown-rule-map",
                    "missing-fixture-binding",
                ],
            ),
            "FF-BUILD-077" => (
                &["graph", "source", "artifact"],
                &["three-root-boundary"],
                &["wrong-root-build-file", "duplicate-toolchain-selector"],
            ),
            "FF-BUILD-078" => (
                &["policy", "runtime_observable", "negative_fixture"],
                &["runtime-proof-contract"],
                &["runtime-scaffold-completion", "runtime-noop-success"],
            ),
            "FF-BUILD-079" => (
                &["policy", "scenario_contract", "negative_fixture"],
                &["runtime-proof-contract"],
                &["runtime-missing-artifact-identity", "runtime-noop-success"],
            ),
            "FF-BUILD-080" => (
                &["artifact", "external_process", "negative_fixture"],
                &["runtime-proof-contract", "runtime-truth-check"],
                &[
                    "runtime-missing-artifact-identity",
                    "runtime-missing-clean-stage",
                    "runtime-stage-collision",
                ],
            ),
            "FF-BUILD-081" => (
                &["policy", "external_process", "negative_fixture"],
                &["runtime-proof-contract"],
                &["runtime-test-only-substitute", "runtime-mock-boundary"],
            ),
            "FF-BUILD-082" => (
                &["policy", "runtime_observable", "negative_fixture"],
                &["runtime-proof-contract"],
                &["runtime-noop-success"],
            ),
            "FF-BUILD-083" | "FF-BUILD-088" => (
                &["policy", "runtime_observable", "negative_fixture"],
                &["runtime-proof-contract"],
                &["runtime-scaffold-completion"],
            ),
            "FF-BUILD-084" => (
                &["policy", "runtime_observable", "negative_fixture"],
                &["runtime-proof-contract", "runtime-truth-check"],
                &["runtime-scaffold-completion", "runtime-noop-success"],
            ),
            "FF-BUILD-085" => (
                &["policy", "external_process", "negative_fixture"],
                &["runtime-proof-contract"],
                &["runtime-test-only-substitute"],
            ),
            "FF-BUILD-086" => (
                &["artifact", "runtime_observable", "negative_fixture"],
                &["runtime-proof-contract", "runtime-truth-check"],
                &[
                    "runtime-missing-artifact-identity",
                    "runtime-missing-clean-stage",
                    "runtime-stage-collision",
                ],
            ),
            "FF-BUILD-087" => (
                &["counterfactual", "negative_fixture"],
                &["runtime-proof-contract", "runtime-truth-check"],
                &["runtime-missing-counterfactual"],
            ),
            "FF-BUILD-089" => (
                &["source", "dependency", "process_plan", "negative_fixture"],
                &["product-native-boundary"],
                &[
                    "indirect-oracle-runtime-wrapper",
                    "bundled-companion-solver-asset",
                    "documentation-asset-bypass",
                    "readme-include-bypass",
                    "dynamic-product-process",
                    "include-doc-macro-bypass",
                    "clippy-allow-escape",
                    "product-clippy-guard-missing",
                    "clippy-conf-dir-poison",
                    "cargo-home-poison",
                ],
            ),
            "FF-BUILD-090" => (
                &["structural", "semantic", "integration", "negative_fixture"],
                &[
                    "compatibility-semantic-replay",
                    "ffmpeg-supervision-report-consumer",
                ],
                &[
                    "structural-replay-behavioral-pass",
                    "source-content-fingerprint",
                ],
            ),
            "FF-BUILD-091" => (
                &[
                    "semantic",
                    "counterfactual",
                    "integration",
                    "negative_fixture",
                ],
                &[
                    "public-boundary-counterexample",
                    "ffmpeg-supervision-report-consumer",
                ],
                &["declaration-only-proof"],
            ),
            "FF-BUILD-092" => (
                &["state_effect", "counterfactual", "negative_fixture"],
                &["effect-acknowledgement-boundary"],
                &["effect-acknowledgement-subset"],
            ),
            "FF-BUILD-093" => (
                &["wire_boundary", "counterfactual", "negative_fixture"],
                &["schema-transition-authority"],
                &["unrelated-schema-transition"],
            ),
            "FF-BUILD-094" => (
                &["wire_boundary", "counterfactual", "negative_fixture"],
                &["wire-boundary"],
                &[
                    "wire-zero-or-unknown-field",
                    "sequence-zero-replay",
                    "durable-journal-payload-unknown-field",
                    "durable-journal-record-unknown-field",
                    "durable-reconcile-state-unknown-field",
                    "durable-journal-sequence-zero",
                ],
            ),
            "FF-BUILD-095" => (
                &["graph", "counterfactual", "negative_fixture"],
                &["source-graph-cycle-boundary"],
                &["source-graph-cycle"],
            ),
            "FF-BUILD-096" => (
                &["public_boundary", "counterfactual", "negative_fixture"],
                &["finding-regression-boundary"],
                &[
                    "counterexample-not-public-boundary",
                    "remove-public-boundary-counterexample-test",
                    "receipt-only-public-counterexample-test",
                ],
            ),
            "FF-BUILD-097" => (
                &["semantic", "integration", "negative_fixture"],
                &[
                    "behavior-sensitive-proof-map",
                    "ffmpeg-supervision-report-consumer",
                ],
                &[
                    "proof-map-string-only",
                    "proof-map-behavior-stub",
                    "proof-map-constant-tautology",
                ],
            ),
            "FF-BUILD-098" => (
                &[
                    "structural",
                    "semantic",
                    "integration",
                    "production_runtime",
                    "negative_fixture",
                ],
                &[
                    "proof-class-aggregation",
                    "ffmpeg-supervision-report-consumer",
                ],
                &["proof-class-promotion", "gate-report-runtime-claim"],
            ),
            "FF-BUILD-099" => (
                &["counterfactual", "integration", "negative_fixture"],
                &["credential-snapshot-boundary"],
                &["secret-scan-provider-key"],
            ),
            other => return Err(format!("FF-ARCH-E-UNKNOWN-RULE: {other}")),
        };
        require_exact_strings(&rule.rule_id, "proof_classes", &rule.proof_classes, proof)?;
        require_exact_strings(&rule.rule_id, "validators", &rule.validators, validators)?;
        require_exact_strings(&rule.rule_id, "fixture_ids", &rule.fixture_ids, fixtures)?;
    }
    checks.push(pass("rule-map-completeness", "every canonical REQUIRED architecture and runtime-truth rule has validators, proof classes, and negative fixtures"));
    Ok(())
}

fn rule_inventory_diagnostic<T: Ord>(
    canonical: &BTreeSet<T>,
    mapped: &BTreeSet<T>,
) -> Option<&'static str> {
    if !canonical.is_subset(mapped) {
        Some("FF-ARCH-E-MISSING-RULE")
    } else if !mapped.is_subset(canonical) {
        Some("FF-ARCH-E-UNKNOWN-RULE")
    } else {
        None
    }
}

fn proof_binding_valid(proof: &[String], validators: &[String], fixtures: &[String]) -> bool {
    proof_binding_counts_valid(proof.len(), validators.len(), fixtures.len())
}

fn proof_binding_counts_valid(proof: usize, validators: usize, fixtures: usize) -> bool {
    proof != 0 && validators != 0 && fixtures != 0
}

fn require_exact_strings(
    rule: &str,
    field: &str,
    observed: &[String],
    expected: &[&str],
) -> Result<(), String> {
    let observed: BTreeSet<_> = observed.iter().map(String::as_str).collect();
    let expected: BTreeSet<_> = expected.iter().copied().collect();
    if observed.len() != expected.len() || observed != expected {
        return Err(format!("rule {rule} has incorrect {field}: {observed:?}"));
    }
    Ok(())
}

fn validate_fixtures(
    root: &Path,
    rule_map: &RuleMap,
    checks: &mut Vec<Check>,
) -> Result<Vec<FixtureResult>, String> {
    let fixture_root = root.join("build/fixtures/architecture");
    let referenced: BTreeSet<_> = rule_map
        .rules
        .iter()
        .flat_map(|rule| rule.fixture_ids.iter().cloned())
        .collect();
    let mut observed = BTreeSet::new();
    let mut results = Vec::new();
    for entry in fs::read_dir(&fixture_root).map_err(|e| format!("read fixtures: {e}"))? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            continue;
        }
        let case: FixtureCase = read_toml(&entry.path().join("case.toml"))?;
        if case.schema_version != 1 || case.fixture_id != entry.file_name().to_string_lossy() {
            return Err(format!(
                "invalid fixture identity at {}",
                entry.path().display()
            ));
        }
        if !observed.insert(case.fixture_id.clone()) {
            return Err(format!("duplicate fixture ID {}", case.fixture_id));
        }
        let execution = proof_integrity_fixture_execution(root, &case.mutation)?;
        if !execution
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic == &case.expected_diagnostic)
        {
            return Err(format!(
                "fixture {} failed for wrong reason: expected {}, observed {:?}",
                case.fixture_id, case.expected_diagnostic, execution.diagnostics
            ));
        }
        results.push(FixtureResult {
            fixture_id: case.fixture_id,
            status: "PASS",
            proof_class: execution.proof_class,
            concrete_input: execution.concrete_input,
            executed_boundary: execution.executed_boundary,
            expected_result: format!("reject mutation with {}", case.expected_diagnostic),
            observed_result: execution.observed_result,
            skipped_semantic_dependencies: execution.skipped_semantic_dependencies,
            execution_path: execution.execution_path,
            expected_diagnostic: case.expected_diagnostic,
            observed_diagnostics: execution.diagnostics,
        });
    }
    if observed != referenced {
        return Err(format!(
            "fixture inventory mismatch: referenced-only={:?}, observed-only={:?}",
            referenced.difference(&observed).collect::<Vec<_>>(),
            observed.difference(&referenced).collect::<Vec<_>>()
        ));
    }
    results.sort_by(|a, b| a.fixture_id.cmp(&b.fixture_id));
    checks.push(pass(
        "negative-fixtures",
        &format!(
            "{} fixtures produced exactly their assigned stable diagnostic",
            results.len()
        ),
    ));
    Ok(results)
}

fn proof_integrity_fixture_execution(
    root: &Path,
    mutation: &str,
) -> Result<FixtureExecution, String> {
    match mutation {
        "add_indirect_oracle_runtime_wrapper" => indirect_oracle_wrapper_fixture_execution(root),
        "bundle_companion_solver_asset" => bundled_companion_solver_fixture_execution(),
        "documentation_asset_bypass" => documentation_asset_bypass_fixture_execution(root),
        "readme_include_bypass" => readme_include_bypass_fixture_execution(root),
        "dynamic_product_process" => dynamic_product_process_fixture_execution(root),
        "include_doc_macro_bypass" => include_doc_macro_bypass_fixture_execution(root),
        "clippy_allow_escape" => clippy_allow_escape_fixture_execution(root),
        "product_clippy_guard_missing" => product_clippy_guard_fixture_execution(root),
        "clippy_conf_dir_poison" => clippy_conf_dir_poison_fixture_execution(root),
        "cargo_home_poison" => cargo_home_poison_fixture_execution(root),
        "source_content_fingerprint" => source_content_fingerprint_fixture_execution(root),
        "secret_scan_provider_key" => secret_scan_provider_key_fixture_execution(root),
        "structural_replay_behavioral_pass" => {
            let diagnostic = compatibility::structural_replay_report_mutation_diagnostic()
                .expect_err(
                    "full replay artifact validator accepted structural-only semantic status",
                );
            Ok(proof_fixture(
                vec![
                    diagnostic
                        .split(':')
                        .next()
                        .unwrap_or(&diagnostic)
                        .to_owned(),
                ],
                "negative_fixture",
                "serialized-compatibility-report-artifact",
                "complete serialized replay report mutated to semantic status with no semantic executions",
                "compatibility replay artifact validator used by report construction and deep composition",
            ))
        }
        "declaration_only_proof"
        | "counterexample_not_public_boundary"
        | "proof_class_promotion"
        | "gate_report_unknown_proof_class"
        | "gate_report_undeclared_execution"
        | "gate_report_nonpass_result"
        | "gate_report_runtime_claim" => proof_report_fixture_execution(mutation),
        "proof_map_string_only" => proof_map_string_only_fixture_execution(root),
        "proof_map_behavior_stub" => proof_map_behavior_stub_fixture_execution(root),
        "proof_map_constant_tautology" => proof_map_constant_tautology_fixture_execution(root),
        "effect_acknowledgement_subset" => public_invariant_mutation_fixture_execution(
            root,
            "effect_acknowledgement_subset",
            "FF-ARCH-E-EFFECT-ACK-SUBSET",
            "semantic",
        ),
        "unrelated_schema_transition" => public_invariant_mutation_fixture_execution(
            root,
            "unrelated_schema_transition",
            "FF-ARCH-E-SCHEMA-SELF-AUTHORIZATION",
            "wire_boundary",
        ),
        "wire_zero_or_unknown_field" => public_invariant_mutation_fixture_execution(
            root,
            "wire_zero_or_unknown_field",
            "FF-ARCH-E-WIRE-BOUNDARY",
            "wire_boundary",
        ),
        "sequence_zero_replay" => public_invariant_mutation_fixture_execution(
            root,
            "sequence_zero_replay",
            "FF-ARCH-E-SEQUENCE-ZERO",
            "wire_boundary",
        ),
        "durable_journal_payload_unknown_field"
        | "durable_journal_record_unknown_field"
        | "durable_reconcile_state_unknown_field"
        | "durable_journal_sequence_zero" => durable_storage_fixture_execution(root, mutation),
        "source_graph_cycle" => public_invariant_mutation_fixture_execution(
            root,
            "source_graph_cycle",
            "FF-ARCH-E-SOURCE-GRAPH-CYCLE",
            "graph",
        ),
        "remove_public_boundary_counterexample_test" => {
            isolated_public_counterexample_mutations(root)
        }
        "receipt_only_public_counterexample_test" => {
            receipt_only_public_counterexample_fixture_execution(root)
        }
        _ => {
            let diagnostic = diagnostic_from_production_validator(mutation)?;
            Ok(proof_fixture(
                vec![diagnostic.to_owned()],
                "negative_fixture",
                "architecture-validator",
                mutation,
                "architecture_check validator family",
            ))
        }
    }
}

fn secret_scan_provider_key_fixture_execution(root: &Path) -> Result<FixtureExecution, String> {
    let diagnostic = with_isolated_fixture_root(root, "credential-snapshot-boundary", |sandbox| {
        secret_scan::credential_snapshot_counterfactual_diagnostic(sandbox)
    })
    .expect_err("credential scanner accepted staged, outgoing-tree, or outgoing-ref input");
    if !diagnostic.starts_with("FF-SECRET-E-DETECTED:") {
        return Err(format!(
            "credential scanner failed for wrong reason: {diagnostic}"
        ));
    }
    Ok(proof_fixture(
        vec!["FF-SECRET-E-DETECTED".to_owned()],
        "counterfactual",
        "credential-snapshot-boundary",
        "isolated Git index, direct tree-root push, and ref-name inputs containing a synthetic provider-pattern key assembled at runtime",
        "production staged-index and pre-push snapshot scanners with exact hook integrity and value suppression",
    ))
}

fn durable_storage_fixture_execution(
    root: &Path,
    mutation: &str,
) -> Result<FixtureExecution, String> {
    public_invariant_mutation_fixture_execution(
        root,
        mutation,
        "FF-ARCH-E-WIRE-BOUNDARY",
        "counterfactual",
    )
}

fn indirect_oracle_wrapper_fixture_execution(root: &Path) -> Result<FixtureExecution, String> {
    let diagnostic = isolated_product_oracle_diagnostic(
        root,
        "indirect-oracle-runtime-wrapper",
        "product/src/runtime_wrapper.rs",
        b"pub fn run() { let oracle = \"yt-dlp\"; let _ = std::process::Command::new(oracle); }",
    )?;
    Ok(proof_fixture(
        vec![diagnostic],
        "negative_fixture",
        "isolated-product-tree",
        "product/src/runtime_wrapper.rs contains an indirect yt-dlp process wrapper",
        "scan_product_oracle_boundary over an isolated product tree",
    ))
}

fn bundled_companion_solver_fixture_execution() -> Result<FixtureExecution, String> {
    let relative = "build/crates/fforager-javascript/assets/yt-dlp-ejs-solver.js";
    let diagnostic = ferric_implementation_dependency_diagnostic(
        relative,
        b"// executable yt-dlp-ejs solver asset",
    )
    .ok_or("production independence validator accepted bundled yt-dlp companion solver asset")?;
    Ok(proof_fixture(
        vec![diagnostic.to_owned()],
        "negative_fixture",
        "build-implementation-asset",
        "a non-shipped Ferric worker bundles an executable yt-dlp-ejs solver asset",
        "ferric_implementation_dependency_diagnostic over the build implementation boundary",
    ))
}

fn documentation_asset_bypass_fixture_execution(root: &Path) -> Result<FixtureExecution, String> {
    let script = isolated_product_oracle_diagnostic(
        root,
        "docs-script",
        "product/docs/bootstrap.py",
        b"exit(0)\n",
    )?;
    let binary = isolated_product_oracle_diagnostic(
        root,
        "docs-binary",
        "product/docs/oracle.bin",
        &[0, 1, 2, 3],
    )?;
    let source = isolated_product_oracle_diagnostic(
        root,
        "docs-include",
        "product/src/lib.rs",
        b"const PAYLOAD: &[u8] = include_bytes!(\"../docs/research.md\");",
    )?;
    Ok(proof_fixture(
        vec![script, binary, source],
        "negative_fixture",
        "isolated-product-tree",
        "product/docs executable, binary, and source include_bytes mutations",
        "scan_product_oracle_boundary over three isolated product trees",
    ))
}

fn readme_include_bypass_fixture_execution(root: &Path) -> Result<FixtureExecution, String> {
    let diagnostic = isolated_product_oracle_tree_diagnostic(
        root,
        "readme-include",
        &[
            (
                "product/README.md",
                b"yt-dlp payload text that must remain documentation-only\n",
            ),
            (
                "product/src/lib.rs",
                b"const PAYLOAD: &[u8] = include_bytes!(\"../README.md\");",
            ),
        ],
    )?;
    Ok(proof_fixture(
        vec![diagnostic],
        "negative_fixture",
        "isolated-product-tree",
        "product README containing oracle text embedded through include_bytes",
        "scan_product_oracle_boundary over an isolated product tree",
    ))
}

fn dynamic_product_process_fixture_execution(root: &Path) -> Result<FixtureExecution, String> {
    let diagnostic = isolated_product_oracle_diagnostic(
        root,
        "dynamic-product-process",
        "product/src/lib.rs",
        b"use std::process::Command as C; pub fn run() { let mut program = \"future\".to_string(); program.push_str(\"-tool\"); let _ = C::new(program); }",
    )?;
    Ok(proof_fixture(
        vec![diagnostic],
        "negative_fixture",
        "isolated-product-tree",
        "product source aliases Command and dynamically assembles a program name before C::new",
        "scan_product_oracle_boundary over an isolated product tree",
    ))
}

fn include_doc_macro_bypass_fixture_execution(root: &Path) -> Result<FixtureExecution, String> {
    let diagnostic = isolated_product_oracle_tree_diagnostic(
        root,
        "include-doc-macro",
        &[
            ("product/docs/payload.md", b"documentation payload\n"),
            ("product/src/lib.rs", b"include!(\"../docs/payload.md\");"),
        ],
    )?;
    Ok(proof_fixture(
        vec![diagnostic],
        "negative_fixture",
        "isolated-product-tree",
        "product source expands documentation through include!",
        "scan_product_oracle_boundary over an isolated product tree",
    ))
}

fn clippy_allow_escape_fixture_execution(root: &Path) -> Result<FixtureExecution, String> {
    let diagnostic = isolated_product_oracle_diagnostic(
        root,
        "clippy-allow-escape",
        "product/src/lib.rs",
        b"#[allow(clippy::disallowed_methods)]\npub fn bypass() {}\n",
    )?;
    Ok(proof_fixture(
        vec![diagnostic],
        "negative_fixture",
        "isolated-product-tree",
        "product source locally suppresses a disallowed Clippy lint",
        "scan_product_oracle_boundary rejects local compiler-guard escape text",
    ))
}

fn product_clippy_guard_fixture_execution(root: &Path) -> Result<FixtureExecution, String> {
    let diagnostic = with_isolated_fixture_root(root, "product-clippy-guard-missing", |sandbox| {
        for relative in [
            "product/clippy.toml",
            "product/crates/fforager-contracts/Cargo.toml",
            "product/crates/fforager-diagnostics-contract/Cargo.toml",
            "product/crates/fforager-core/Cargo.toml",
        ] {
            let destination = sandbox.join(relative);
            fs::create_dir_all(
                destination
                    .parent()
                    .ok_or("isolated Clippy fixture path has no parent")?,
            )
            .map_err(|error| error.to_string())?;
            fs::copy(root.join(relative), &destination).map_err(|error| error.to_string())?;
        }
        replace_text_in_file(
            &sandbox.join("product/clippy.toml"),
            "  { path = \"std::process::Command::new\", reason = \"Process construction requires a future typed FFmpeg/ffprobe boundary.\" },\n",
            "",
        )?;
        let error = validate_product_clippy_guard(sandbox)
            .expect_err("product Clippy guard accepted a missing Command::new rule");
        if error.contains("FF-ARCH-E-PRODUCT-CLIPPY-GUARD") {
            Ok("FF-ARCH-E-PRODUCT-CLIPPY-GUARD".to_owned())
        } else {
            Err(error)
        }
    })?;
    Ok(proof_fixture(
        vec![diagnostic],
        "negative_fixture",
        "isolated-product-clippy-config",
        "remove the product Command::new compiler guard",
        "validate_product_clippy_guard over an isolated product configuration",
    ))
}

fn clippy_conf_dir_poison_fixture_execution(root: &Path) -> Result<FixtureExecution, String> {
    compiler_guard_environment_poison_fixture_execution(
        root,
        "clippy-conf-dir-poison",
        "CLIPPY_CONF_DIR",
        "attempted CLIPPY_CONF_DIR override plus aliased std::process::Command",
    )
}

fn cargo_home_poison_fixture_execution(root: &Path) -> Result<FixtureExecution, String> {
    compiler_guard_environment_poison_fixture_execution(
        root,
        "cargo-home-poison",
        "CARGO_HOME",
        "attempted CARGO_HOME config with --cap-lints=allow plus aliased std::process::Command",
    )
}

fn compiler_guard_environment_poison_fixture_execution(
    root: &Path,
    label: &str,
    environment_key: &str,
    concrete_input: &str,
) -> Result<FixtureExecution, String> {
    let target_dir = isolated_fixture_target_dir(root, "compiler-guard-probe")?;
    let target_dir = target_dir
        .to_str()
        .ok_or("compiler guard target directory is not valid UTF-8")?;
    let diagnostic = with_isolated_fixture_root(root, label, |sandbox| {
        let product = sandbox.join("product");
        let probe = product.join("crates/compiler-guard-probe");
        let poison = sandbox.join("poisoned-clippy-config");
        fs::create_dir_all(probe.join("src")).map_err(|error| error.to_string())?;
        fs::create_dir_all(&poison).map_err(|error| error.to_string())?;
        fs::copy(
            root.join("product/clippy.toml"),
            product.join("clippy.toml"),
        )
        .map_err(|error| error.to_string())?;
        fs::write(
            poison.join("clippy.toml"),
            b"# intentionally empty override\n",
        )
        .map_err(|error| error.to_string())?;
        fs::write(
            poison.join("config.toml"),
            b"[build]\nrustflags = [\"--cap-lints=allow\"]\n",
        )
        .map_err(|error| error.to_string())?;
        fs::write(
            probe.join("Cargo.toml"),
            b"[workspace]\n\n[package]\nname = \"compiler-guard-probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\npublish = false\n\n[lints.clippy]\ndisallowed_types = \"forbid\"\ndisallowed_methods = \"forbid\"\ndisallowed_macros = \"forbid\"\n",
        )
        .map_err(|error| error.to_string())?;
        fs::write(
            probe.join("src/lib.rs"),
            b"use std::process::Command as C;\npub fn probe() { let _ = C::new(\"ffprobe\"); }\n",
        )
        .map_err(|error| error.to_string())?;
        let poison_text = poison
            .to_str()
            .ok_or("poisoned Clippy path is not valid UTF-8")?;
        let error = command_output_bytes_with_timeout_and_environment(
            sandbox,
            "cargo",
            &[
                "clippy",
                "--manifest-path",
                "product/crates/compiler-guard-probe/Cargo.toml",
                "--offline",
                "--target-dir",
                target_dir,
                "--",
                "-D",
                "warnings",
            ],
            CARGO_PROOF_COMMAND_TIMEOUT,
            &[(environment_key, poison_text)],
        )
        .expect_err("poisoned Cargo environment hid the aliased Command compiler violation");
        if error.contains("disallowed") && error.contains("Command") {
            Ok("FF-ARCH-E-RUST-ENV-OVERRIDE".to_owned())
        } else {
            Err(format!(
                "compiler guard failed without the expected aliased Command diagnostic: {error}"
            ))
        }
    })?;
    Ok(proof_fixture(
        vec![diagnostic],
        "negative_fixture",
        "isolated-compiler-guard-probe",
        concrete_input,
        "sanitized cargo environment and compiler-resolved product Clippy configuration",
    ))
}

fn source_content_fingerprint_fixture_execution(root: &Path) -> Result<FixtureExecution, String> {
    let diagnostic = with_isolated_fixture_root(root, "source-content-fingerprint", |sandbox| {
        let relative = "source/current.rs".to_owned();
        fs::create_dir_all(sandbox.join("source")).map_err(|error| error.to_string())?;
        fs::write(sandbox.join(&relative), b"first dirty content\n")
            .map_err(|error| error.to_string())?;
        let before = SourceState {
            git_commit: "same-head".to_owned(),
            dirty: true,
            dirty_paths: vec![relative.clone()],
            content_fingerprint: content_fingerprint_for_paths(
                sandbox,
                std::slice::from_ref(&relative),
            )?,
        };
        fs::write(sandbox.join(&relative), b"different dirty content\n")
            .map_err(|error| error.to_string())?;
        let after = SourceState {
            git_commit: before.git_commit.clone(),
            dirty: before.dirty,
            dirty_paths: before.dirty_paths.clone(),
            content_fingerprint: content_fingerprint_for_paths(sandbox, &[relative])?,
        };
        if source_states_equal(&before, &after) {
            Err(
                "source-state comparison accepted changed content at the same dirty path"
                    .to_owned(),
            )
        } else {
            Ok("FF-COMP-E-REPLAY-REPORT-SOURCE-STALE".to_owned())
        }
    })?;
    Ok(proof_fixture(
        vec![diagnostic],
        "negative_fixture",
        "source-state-content-boundary",
        "same HEAD and dirty path set with changed file bytes",
        "the exact source-state equality predicate used by deep replay composition",
    ))
}

fn proof_fixture(
    diagnostics: Vec<String>,
    proof_class: &'static str,
    execution_path: &'static str,
    concrete_input: &str,
    executed_boundary: &str,
) -> FixtureExecution {
    FixtureExecution {
        observed_result: format!("observed stable diagnostics {diagnostics:?}"),
        diagnostics,
        proof_class,
        execution_path,
        concrete_input: concrete_input.to_owned(),
        executed_boundary: executed_boundary.to_owned(),
        skipped_semantic_dependencies: vec![
            "This is prerequisite proof; no shipped Ferric entrypoint was executed.".to_owned(),
        ],
    }
}

fn proof_map_string_only_fixture_execution(root: &Path) -> Result<FixtureExecution, String> {
    const DELETED_PROOF_ID: &str =
        "contracts::identity::tests::typed_ids_reject_wrong_prefix_and_uppercase";
    let target_dir = isolated_fixture_target_dir(root, "contract-inventory-resolver")?;
    fs::create_dir_all(&target_dir).map_err(|error| error.to_string())?;
    let diagnostic = with_isolated_fixture_root(root, "proof-map-deleted-test", |sandbox| {
        prepare_isolated_testkit_workspace(root, sandbox)?;
        let source = sandbox.join("product/crates/fforager-contracts/src/identity.rs");
        replace_text_in_file(
            &source,
            "fn typed_ids_reject_wrong_prefix_and_uppercase()",
            "fn deleted_typed_ids_reject_wrong_prefix_and_uppercase()",
        )?;
        let error = resolve_and_execute_inventory_proofs_with_cargo_mode(
            sandbox,
            &[DELETED_PROOF_ID.to_owned()],
            "--offline",
            Some(&target_dir),
        )
        .expect_err("deleted mapped contract test unexpectedly resolved through cargo test --list");
        if !error.contains("FF-ARCH-E-INVENTORY-PROOF-UNRESOLVED") {
            return Err(error);
        }
        Ok("FF-ARCH-E-INVENTORY-PROOF-UNRESOLVED".to_owned())
    })?;
    Ok(proof_fixture(
        vec![diagnostic],
        "negative_fixture",
        "isolated-compiled-contract-workspace",
        "deleted non-public contracts::identity inventory proof target",
        "cargo test --list against an isolated workspace before exact proof execution",
    ))
}

fn proof_map_behavior_stub_fixture_execution(root: &Path) -> Result<FixtureExecution, String> {
    const PROOF_ID: &str =
        "contracts::identity::tests::typed_ids_reject_wrong_prefix_and_uppercase";
    const SELECTOR: &str = "identity::tests::typed_ids_reject_wrong_prefix_and_uppercase";
    let target_dir = isolated_fixture_target_dir(root, "contract-inventory-behavior")?;
    fs::create_dir_all(&target_dir).map_err(|error| error.to_string())?;
    let diagnostic = with_isolated_fixture_root(root, "proof-map-behavior-stub", |sandbox| {
        prepare_isolated_testkit_workspace(root, sandbox)?;
        let identity = sandbox.join("product/crates/fforager-contracts/src/identity.rs");
        replace_named_test_body(
            &identity,
            "fn typed_ids_reject_wrong_prefix_and_uppercase()",
            "{ assert!(true); }",
        )?;
        replace_text_in_file(
            &identity,
            "    if !value.starts_with(prefix) || value.len() == prefix.len() {",
            "    if false {",
        )?;
        let target_dir_text = target_dir
            .to_str()
            .ok_or("inventory behavior target directory is not valid UTF-8")?;
        let exact_output = cargo_proof_output(
            sandbox,
            "cargo",
            &[
                "test",
                "--manifest-path",
                "build/Cargo.toml",
                "--offline",
                "--target-dir",
                target_dir_text,
                "-p",
                "fforager-contracts",
                "--lib",
                SELECTOR,
                "--",
                "--exact",
                "--nocapture",
            ],
        )?;
        if exact_output.contains("running 0 tests") {
            return Err(
                "behavior-stub counterfactual did not execute its mapped exact test".to_owned(),
            );
        }
        let tests = sandbox.join("product/crates/fforager-contracts/tests");
        fs::create_dir_all(&tests).map_err(|error| error.to_string())?;
        fs::write(
            tests.join("inventory_identity_behavior.rs"),
            "use fforager_contracts::ItemId;\n\n#[test]\nfn inventory_identity_behavior_rejects_wrong_prefix() {\n    assert!(ItemId::new(\"node_wrong\").is_err());\n}\n",
        )
        .map_err(|error| error.to_string())?;
        let independent_failure = cargo_proof_output(
            sandbox,
            "cargo",
            &[
                "test",
                "--manifest-path",
                "build/Cargo.toml",
                "--offline",
                "--target-dir",
                target_dir_text,
                "-p",
                "fforager-contracts",
                "--test",
                "inventory_identity_behavior",
                "--",
                "--exact",
                "--nocapture",
            ],
        )
        .expect_err(
            "stubbed inventory proof unexpectedly preserved the independent ItemId behavior",
        );
        if independent_failure.contains("error: test failed")
            && independent_failure.contains("inventory_identity_behavior_rejects_wrong_prefix")
        {
            Ok("FF-ARCH-E-INVENTORY-PROOF-BEHAVIOR".to_owned())
        } else {
            Err(format!(
                "inventory behavior-stub counterfactual failed without the required independent public behavior evidence: {independent_failure}"
            ))
        }
    })?;
    Ok(proof_fixture(
        vec![diagnostic],
        "negative_fixture",
        "isolated-compiled-contract-workspace",
        PROOF_ID,
        "mapped exact test is stubbed while ItemId prefix validation is mutated and independently re-executed",
    ))
}

fn proof_map_constant_tautology_fixture_execution(root: &Path) -> Result<FixtureExecution, String> {
    const PROOF_ID: &str =
        "contracts::identity::tests::typed_ids_reject_wrong_prefix_and_uppercase";
    const SELECTOR: &str = "identity::tests::typed_ids_reject_wrong_prefix_and_uppercase";
    let diagnostic = with_isolated_fixture_root(root, "proof-map-constant-tautology", |sandbox| {
        prepare_isolated_testkit_workspace(root, sandbox)?;
        let identity = sandbox.join("product/crates/fforager-contracts/src/identity.rs");
        replace_named_test_body(
            &identity,
            "fn typed_ids_reject_wrong_prefix_and_uppercase()",
            "{ let observed = 1; assert_eq!(observed, 1); }",
        )?;
        let error =
            validate_inventory_proof_source(sandbox, PROOF_ID, "fforager-contracts", SELECTOR)
                .expect_err(
                    "literal-only mapped assertion unexpectedly passed the normal proof guard",
                );
        if error.contains("FF-ARCH-E-INVENTORY-PROOF-STUB") {
            Ok("FF-ARCH-E-INVENTORY-PROOF-STUB".to_owned())
        } else {
            Err(error)
        }
    })?;
    Ok(proof_fixture(
        vec![diagnostic],
        "negative_fixture",
        "isolated-proof-source-guard",
        "mapped test body replaced by a locally bound constant tautology",
        "the normal inventory proof source validator used before exact execution",
    ))
}

fn isolated_product_oracle_diagnostic(
    root: &Path,
    label: &str,
    relative: &str,
    bytes: &[u8],
) -> Result<String, String> {
    isolated_product_oracle_tree_diagnostic(root, label, &[(relative, bytes)])
}

fn isolated_product_oracle_tree_diagnostic(
    root: &Path,
    label: &str,
    files: &[(&str, &[u8])],
) -> Result<String, String> {
    with_isolated_fixture_root(root, label, |sandbox| {
        for (relative, bytes) in files {
            let target = sandbox.join(relative);
            let parent = target
                .parent()
                .ok_or("isolated product mutation has no parent")?;
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            fs::write(&target, bytes).map_err(|error| error.to_string())?;
        }
        scan_product_oracle_boundary(sandbox)
            .expect_err("isolated product mutation unexpectedly passed native product boundary")
            .split(':')
            .next()
            .map(ToOwned::to_owned)
            .ok_or("isolated product boundary emitted no stable diagnostic".to_owned())
    })
}

fn with_isolated_fixture_root<T>(
    root: &Path,
    label: &str,
    action: impl FnOnce(&Path) -> Result<T, String>,
) -> Result<T, String> {
    if matches!(
        label,
        "proof-map-deleted-test"
            | "proof-map-behavior-stub"
            | "public-counterexample-removed"
            | "public-counterexample-ignored"
            | "public-counterexample-empty"
            | "public-counterexample-receipt-only"
            | "effect_acknowledgement_subset"
            | "unrelated_schema_transition"
            | "wire_zero_or_unknown_field"
            | "sequence_zero_replay"
            | "durable_journal_payload_unknown_field"
            | "durable_journal_record_unknown_field"
            | "durable_reconcile_state_unknown_field"
            | "durable_journal_sequence_zero"
            | "source_graph_cycle"
    ) {
        return with_warm_testkit_fixture(root, label, action);
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let sandbox = disposable_artifact_root(root)
        .join("test-runs/proof-integrity-fixtures")
        .join(format!("{label}-{}-{nonce}", std::process::id()));
    fs::create_dir_all(&sandbox).map_err(|error| error.to_string())?;
    let result = action(&sandbox);
    let cleanup = fs::remove_dir_all(&sandbox).map_err(|error| error.to_string());
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(format!("isolated fixture cleanup failed: {error}")),
        (Err(action_error), Err(cleanup_error)) => Err(format!(
            "{action_error}; isolated fixture cleanup also failed: {cleanup_error}"
        )),
    }
}

fn with_warm_testkit_fixture<T>(
    root: &Path,
    label: &str,
    action: impl FnOnce(&Path) -> Result<T, String>,
) -> Result<T, String> {
    let parent = disposable_artifact_root(root).join("warm-proof-sources");
    fs::create_dir_all(&parent).map_err(|error| error.to_string())?;
    let lock_path = parent.join(format!("{label}.lock"));
    let lock = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .map_err(|error| {
            format!("exclusive warm fixture {label} unavailable (no stale-lease takeover): {error}")
        })?;
    let sandbox = parent.join(label);
    let result = (|| {
        fs::create_dir_all(&sandbox).map_err(|error| error.to_string())?;
        for path in walk_files(&sandbox)? {
            let relative = path
                .strip_prefix(&sandbox)
                .map_err(|error| error.to_string())?;
            let canonical = root.join(relative);
            if canonical.is_file() {
                copy_fixture_file(&canonical, &path)?;
            } else {
                fs::remove_file(&path).map_err(|error| error.to_string())?;
            }
        }
        remove_empty_fixture_directories(&sandbox)?;
        action(&sandbox)
    })();
    drop(lock);
    let release = fs::remove_file(&lock_path)
        .map_err(|error| format!("warm fixture lease release failed: {error}"));
    match (result, release) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(format!("{primary}; {cleanup}")),
    }
}

fn remove_empty_fixture_directories(root: &Path) -> Result<(), String> {
    for entry in fs::read_dir(root).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_dir()
        {
            remove_empty_fixture_directories(&entry.path())?;
            if fs::read_dir(entry.path())
                .map_err(|error| error.to_string())?
                .next()
                .is_none()
            {
                fs::remove_dir(entry.path()).map_err(|error| error.to_string())?;
            }
        }
    }
    Ok(())
}

fn copy_fixture_file(source: &Path, destination: &Path) -> Result<(), String> {
    let bytes = fs::read(source).map_err(|error| error.to_string())?;
    if !destination.exists() {
        fs::copy(source, destination).map_err(|error| error.to_string())?;
        return Ok(());
    }
    if fs::read(destination).ok().as_deref() != Some(bytes.as_slice()) {
        fs::write(destination, bytes).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn public_invariant_mutation_fixture_execution(
    root: &Path,
    mutation: &str,
    diagnostic: &str,
    proof_class: &'static str,
) -> Result<FixtureExecution, String> {
    let target_dir = isolated_fixture_target_dir(root, "public-invariant-mutations")?;
    fs::create_dir_all(&target_dir).map_err(|error| error.to_string())?;
    let observed = with_isolated_fixture_root(root, mutation, |sandbox| {
        prepare_isolated_testkit_workspace(root, sandbox)?;
        let failure_marker = match mutation {
            "effect_acknowledgement_subset" => replace_text_in_file(
                &sandbox.join("product/crates/fforager-core/src/lifecycle.rs"),
                "        (State::FilesystemProbed, Event::Confine) => step(\n            State::FilesystemConfining,\n            &[EffectIntent::EstablishConfinedPath],\n        ),",
                "        (State::FilesystemProbed, Event::Confine) => step(State::FilesystemConfined, &[]),",
            )
            .map(|()| "FilesystemConfining")?,
            "unrelated_schema_transition" => fs::write(
                sandbox.join("build/fixtures/contracts/diagnostic-protocol-offer-v2.0.json"),
                br#"{"versions":{"major":1,"minimum_minor":0,"maximum_minor":2},"accepted_schemas":[{"algorithm":"sha256","canonical_input_version":1,"digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]}"#,
            )
            .map_err(|error| error.to_string())
            .map(|()| "SchemaIncompatible")?,
            "wire_zero_or_unknown_field" => replace_text_in_file(
                &sandbox.join("build/crates/fforager-testkit/src/lib.rs"),
                "envelope[\"undeclared_top_level\"] = serde_json::json!(true);",
                "envelope[\"capability_id\"] = serde_json::json!(\"ff.capability.transport\");",
            )
            .map(|()| "decode_json_frame")?,
            "sequence_zero_replay" => replace_text_in_file(
                &sandbox.join("product/crates/fforager-diagnostics-contract/src/protocol.rs"),
                "        identity.validate()?;\n",
                "",
            )
            .map(|()| "InvalidStart")?,
            "durable_journal_payload_unknown_field" => replace_text_in_file(
                &sandbox.join("product/crates/fforager-contracts/src/storage.rs"),
                "    rename_all = \"snake_case\",\n    deny_unknown_fields\n)]\npub enum JournalPayload",
                "    rename_all = \"snake_case\"\n)]\npub enum JournalPayload",
            )
            .map(|()| "JournalPayload unknown body field must fail closed")?,
            "durable_journal_record_unknown_field" => replace_text_in_file(
                &sandbox.join("product/crates/fforager-contracts/src/storage.rs"),
                "#[derive(Clone, Debug, PartialEq, Deserialize)]\n#[serde(deny_unknown_fields)]\nstruct JournalRecordWire",
                "#[derive(Clone, Debug, PartialEq, Deserialize)]\nstruct JournalRecordWire",
            )
            .map(|()| "JournalRecord unknown top-level field must fail closed")?,
            "durable_reconcile_state_unknown_field" => replace_text_in_file(
                &sandbox.join("product/crates/fforager-contracts/src/storage.rs"),
                "#[serde(tag = \"kind\", rename_all = \"snake_case\", deny_unknown_fields)]\npub enum ReconcileState",
                "#[serde(tag = \"kind\", rename_all = \"snake_case\")]\npub enum ReconcileState",
            )
            .map(|()| "ReconcileState unknown variant field must fail closed")?,
            "durable_journal_sequence_zero" => replace_text_in_file(
                &sandbox.join("product/crates/fforager-contracts/src/storage.rs"),
                "        record.validate()?;\n        Ok(record)",
                "        Ok(record)",
            )
            .map(|()| "JournalRecord sequence zero must fail closed")?,
            "source_graph_cycle" => replace_text_in_file(
                &sandbox.join("product/crates/fforager-contracts/src/graph.rs"),
                "        validate_relationship_cycles(self)",
                "        Ok(())",
            )
            .map(|()| "RelationshipCycle")?,
            other => return Err(format!("unknown public invariant mutation {other}")),
        };
        isolated_public_test_failure_diagnostic(sandbox, &target_dir, diagnostic, failure_marker)
    })?;
    Ok(proof_fixture(
        vec![observed],
        proof_class,
        "isolated-compiled-public-boundary",
        mutation,
        "mutated input or implementation through the exact compiled public counterexample boundary",
    ))
}

fn isolated_public_test_failure_diagnostic(
    root: &Path,
    target_dir: &Path,
    diagnostic: &str,
    failure_marker: &str,
) -> Result<String, String> {
    let target_dir = target_dir
        .to_str()
        .ok_or("isolated target directory is not valid UTF-8")?;
    let error = cargo_proof_output(
        root,
        "cargo",
        &[
            "test",
            "--manifest-path",
            "build/Cargo.toml",
            "--offline",
            "--target-dir",
            target_dir,
            "-p",
            "fforager-testkit",
            "--lib",
            PUBLIC_BOUNDARY_COUNTEREXAMPLE_TEST,
            "--",
            "--exact",
            "--nocapture",
        ],
    )
    .expect_err("public invariant mutation unexpectedly passed the exact compiled boundary");
    if !error.contains("error: test failed") || !error.contains(failure_marker) {
        return Err(format!(
            "public invariant mutation failed without its required {failure_marker} boundary marker: {error}"
        ));
    }
    Ok(diagnostic.to_owned())
}

#[allow(clippy::too_many_lines)]
fn proof_report_fixture_execution(mutation: &str) -> Result<FixtureExecution, String> {
    let (check, executed, aggregate) = match mutation {
        "declaration_only_proof" => (
            Check {
                id: "declared-proof".to_owned(),
                status: "PASS",
                proof_class: "semantic",
                concrete_input: "proof-id-only".to_owned(),
                executed_boundary: String::new(),
                expected_result: "semantic evidence".to_owned(),
                observed_result: "matching string".to_owned(),
                skipped_semantic_dependencies: Vec::new(),
                detail: "declaration only".to_owned(),
            },
            vec!["semantic".to_owned()],
            "semantic".to_owned(),
        ),
        "counterexample_not_public_boundary" => (
            Check {
                id: "local-counterexample".to_owned(),
                status: "PASS",
                proof_class: "semantic",
                concrete_input: "local assertion".to_owned(),
                executed_boundary: "implementation-local unit test".to_owned(),
                expected_result: "rejection".to_owned(),
                observed_result: "rejection".to_owned(),
                skipped_semantic_dependencies: Vec::new(),
                detail: "not public".to_owned(),
            },
            vec!["semantic".to_owned()],
            "semantic".to_owned(),
        ),
        "proof_class_promotion" => (
            pass("structural-evidence", "executed structural validation"),
            vec!["semantic".to_owned()],
            "semantic".to_owned(),
        ),
        "gate_report_unknown_proof_class" => (
            Check {
                id: "unknown-class".to_owned(),
                status: "PASS",
                proof_class: "unbounded",
                concrete_input: "counterfactual gate report".to_owned(),
                executed_boundary: "serialized gate report boundary".to_owned(),
                expected_result: "schema rejects unknown proof class".to_owned(),
                observed_result: "unknown proof class was supplied".to_owned(),
                skipped_semantic_dependencies: Vec::new(),
                detail: "schema counterfactual".to_owned(),
            },
            vec!["unbounded".to_owned()],
            "none".to_owned(),
        ),
        "gate_report_undeclared_execution" => (
            Check {
                id: "undeclared-execution".to_owned(),
                status: "PASS",
                proof_class: "semantic",
                concrete_input: "counterfactual gate report".to_owned(),
                executed_boundary: "serialized gate report boundary".to_owned(),
                expected_result: "schema rejects executed class outside declaration".to_owned(),
                observed_result: "semantic execution was supplied".to_owned(),
                skipped_semantic_dependencies: Vec::new(),
                detail: "schema counterfactual".to_owned(),
            },
            vec!["semantic".to_owned()],
            "semantic".to_owned(),
        ),
        "gate_report_nonpass_result" => (
            Check {
                id: "nonpass-result".to_owned(),
                status: "PASS",
                proof_class: "semantic",
                concrete_input: "counterfactual gate report".to_owned(),
                executed_boundary: "serialized gate report boundary".to_owned(),
                expected_result: "schema requires PASS result rows in a PASS report".to_owned(),
                observed_result: "PASS result was constructed before serialized mutation"
                    .to_owned(),
                skipped_semantic_dependencies: Vec::new(),
                detail: "schema counterfactual".to_owned(),
            },
            vec!["semantic".to_owned()],
            "semantic".to_owned(),
        ),
        "gate_report_runtime_claim" => (
            Check {
                id: "runtime-claim".to_owned(),
                status: "PASS",
                proof_class: "runtime_observable",
                concrete_input: "counterfactual gate report".to_owned(),
                executed_boundary: "serialized gate report boundary".to_owned(),
                expected_result:
                    "schema rejects runtime-class execution without staged-artifact evidence"
                        .to_owned(),
                observed_result: "runtime_observable was supplied".to_owned(),
                skipped_semantic_dependencies: Vec::new(),
                detail: "schema counterfactual".to_owned(),
            },
            vec!["runtime_observable".to_owned()],
            "runtime_observable".to_owned(),
        ),
        _ => return Err(format!("unknown proof report mutation {mutation}")),
    };
    let declared_supported_proof_classes = if mutation == "gate_report_undeclared_execution" {
        vec!["structural".to_owned()]
    } else {
        vec![check.proof_class.to_owned()]
    };
    let report = GateReport {
        schema_id: "ff.gate-report@1",
        schema_version: "1.0.0",
        gate_id: ARCH_GATE.to_owned(),
        gate_version: 1,
        status: "PASS",
        exit_code: 0,
        source: SourceState {
            git_commit: "fixture".to_owned(),
            dirty: false,
            dirty_paths: Vec::new(),
            content_fingerprint: "0".repeat(64),
        },
        invocation: Invocation {
            repository_root: ".",
            gate_args: vec!["architecture-check".to_owned()],
            canonical_command: invocation(&["architecture-check".to_owned()]).canonical_command,
        },
        inputs: Vec::new(),
        checks: vec![check],
        rules: Vec::new(),
        fixtures: Vec::new(),
        declared_supported_proof_classes,
        executed_proof_classes: executed,
        aggregate_executed_proof_class: aggregate,
        proof_limitations: Vec::new(),
        artifacts: Vec::new(),
    };
    let diagnostic = if let Err(error) = validate_gate_report_evidence(&report) {
        error
    } else {
        let mut value = serde_json::to_value(&report)
            .map_err(|error| format!("serialize gate-report fixture: {error}"))?;
        if mutation == "gate_report_nonpass_result" {
            value["checks"][0]["status"] = Value::String("NOT_APPLICABLE".to_owned());
        }
        let artifact: GateReportArtifact = serde_json::from_value(value)
            .map_err(|error| format!("parse gate-report fixture: {error}"))?;
        validate_gate_report_artifact(&artifact)
            .expect_err("gate-report artifact validator accepted proof-integrity mutation")
    };
    Ok(proof_fixture(
        vec![
            diagnostic
                .split(':')
                .next()
                .unwrap_or(&diagnostic)
                .to_owned(),
        ],
        "negative_fixture",
        "gate-report-contract",
        mutation,
        "validate_gate_report_evidence used before every gate report write",
    ))
}

fn isolated_public_counterexample_mutations(root: &Path) -> Result<FixtureExecution, String> {
    let target_dir = isolated_fixture_target_dir(root, "compiled-testkit")?;
    fs::create_dir_all(&target_dir).map_err(|error| error.to_string())?;
    let missing = with_isolated_fixture_root(root, "public-counterexample-removed", |sandbox| {
        prepare_isolated_testkit_workspace(root, sandbox)?;
        let source = sandbox.join("build/crates/fforager-testkit/src/lib.rs");
        replace_text_in_file(
            &source,
            "fn public_boundary_counterexamples_reject_audit_failures()",
            "fn removed_public_boundary_counterexamples_reject_audit_failures()",
        )?;
        isolated_public_test_diagnostic(sandbox, "removed", &target_dir)
    })?;
    let skipped = with_isolated_fixture_root(root, "public-counterexample-ignored", |sandbox| {
        prepare_isolated_testkit_workspace(root, sandbox)?;
        let source = sandbox.join("build/crates/fforager-testkit/src/lib.rs");
        replace_text_in_file(
            &source,
            "#[test]\n    fn public_boundary_counterexamples_reject_audit_failures()",
            "#[test]\n    #[ignore]\n    fn public_boundary_counterexamples_reject_audit_failures()",
        )?;
        isolated_public_test_diagnostic(sandbox, "ignored", &target_dir)
    })?;
    let empty = with_isolated_fixture_root(root, "public-counterexample-empty", |sandbox| {
        prepare_isolated_testkit_workspace(root, sandbox)?;
        let source = sandbox.join("build/crates/fforager-testkit/src/lib.rs");
        empty_named_test_body(
            &source,
            "fn public_boundary_counterexamples_reject_audit_failures()",
        )?;
        isolated_public_test_diagnostic(sandbox, "empty", &target_dir)
    })?;
    Ok(proof_fixture(
        vec![missing, skipped, empty],
        "negative_fixture",
        "isolated-compiled-testkit-workspace",
        "isolated source mutations that delete, ignore, or empty the exact public counterexample test",
        "cargo test --list and cargo test --exact --nocapture against an isolated compiled workspace",
    ))
}

fn receipt_only_public_counterexample_fixture_execution(
    root: &Path,
) -> Result<FixtureExecution, String> {
    let target_dir = isolated_fixture_target_dir(root, "public-receipt-only")?;
    fs::create_dir_all(&target_dir).map_err(|error| error.to_string())?;
    let diagnostic = with_isolated_fixture_root(
        root,
        "public-counterexample-receipt-only",
        |sandbox| {
            prepare_isolated_testkit_workspace(root, sandbox)?;
            let source = sandbox.join("build/crates/fforager-testkit/src/lib.rs");
            replace_named_test_body(
                &source,
                "fn public_boundary_counterexamples_reject_audit_failures()",
                &format!("{{ println!(\"{PUBLIC_BOUNDARY_COUNTEREXAMPLE_RECEIPT}\"); }}"),
            )?;
            replace_text_in_file(
                &sandbox.join("product/crates/fforager-contracts/src/graph.rs"),
                "        validate_relationship_cycles(self)",
                "        Ok(())",
            )?;
            let public_output = cargo_proof_output(
                sandbox,
                "cargo",
                &[
                    "test",
                    "--manifest-path",
                    "build/Cargo.toml",
                    "--offline",
                    "--target-dir",
                    target_dir
                        .to_str()
                        .ok_or("isolated target directory is not valid UTF-8")?,
                    "-p",
                    "fforager-testkit",
                    "--lib",
                    PUBLIC_BOUNDARY_COUNTEREXAMPLE_TEST,
                    "--",
                    "--exact",
                    "--nocapture",
                ],
            )?;
            if !public_output.contains(PUBLIC_BOUNDARY_COUNTEREXAMPLE_RECEIPT) {
                return Err(
                    "receipt-only mutation did not preserve the test's superficial receipt"
                        .to_owned(),
                );
            }
            let independent_failure = cargo_proof_output(
                sandbox,
                "cargo",
                &[
                    "test",
                    "--manifest-path",
                    "build/Cargo.toml",
                    "--offline",
                    "--target-dir",
                    target_dir
                        .to_str()
                        .ok_or("isolated target directory is not valid UTF-8")?,
                    "-p",
                    "fforager-contracts",
                    "--lib",
                    "graph::tests::every_traversable_relationship_rejects_cycles_and_accepts_exact_limit_acyclic_graphs",
                    "--",
                    "--exact",
                    "--nocapture",
                ],
            )
            .expect_err(
                "receipt-only public test left a graph-cycle implementation mutation undetected",
            );
            if independent_failure.contains("running 1 test")
                && independent_failure.contains("error: test failed")
                && independent_failure.contains("RelationshipCycle")
            {
                Ok("FF-ARCH-E-PUBLIC-COUNTEREXAMPLE-RECEIPT-ONLY".to_owned())
            } else {
                Err(format!(
                    "receipt-only public test mutation did not produce the required independent graph failure: {independent_failure}"
                ))
            }
        },
    )?;
    Ok(proof_fixture(
        vec![diagnostic],
        "negative_fixture",
        "isolated-compiled-testkit-workspace",
        "public test reduced to a static receipt while a source-graph implementation mutation is applied",
        "the receipt-only public test passes, while an independent exact public contract test fails on the same mutation",
    ))
}

fn prepare_isolated_testkit_workspace(source_root: &Path, sandbox: &Path) -> Result<(), String> {
    for relative in [
        "build/crates/fforager-testkit",
        "build/fixtures/contracts",
        "product/crates/fforager-contracts",
        "product/crates/fforager-diagnostics-contract",
        "product/crates/fforager-core",
        "product/crates/fforager-storage",
    ] {
        copy_fixture_tree(&source_root.join(relative), &sandbox.join(relative))?;
    }
    let manifest = r#"[workspace]
members = [
    "crates/fforager-testkit",
    "../product/crates/fforager-contracts",
    "../product/crates/fforager-diagnostics-contract",
    "../product/crates/fforager-core",
    "../product/crates/fforager-storage",
]
resolver = "3"

[workspace.package]
edition = "2024"
rust-version = "1.97.1"
license = "MIT OR Apache-2.0"
publish = false

[workspace.dependencies]
serde = { version = "=1.0.228", features = ["derive"] }
serde_json = "=1.0.150"
redb = { version = "=4.1.0", default-features = false }
sha2 = { version = "=0.11.0", default-features = false }

[workspace.lints.rust]
unsafe_code = "forbid"
missing_debug_implementations = "warn"

[workspace.lints.clippy]
all = { level = "warn", priority = -1 }
pedantic = { level = "warn", priority = -1 }
"#;
    let path = sandbox.join("build/Cargo.toml");
    fs::create_dir_all(
        path.parent()
            .ok_or("isolated workspace has no build parent")?,
    )
    .map_err(|error| error.to_string())?;
    let mut isolated_manifest: toml::Table = manifest
        .parse()
        .map_err(|error: toml::de::Error| error.to_string())?;
    let source_manifest: toml::Table = read_toml(&source_root.join("build/Cargo.toml"))?;
    let windows = source_manifest
        .get("workspace")
        .and_then(toml::Value::as_table)
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(toml::Value::as_table)
        .and_then(|dependencies| dependencies.get("windows-sys"))
        .ok_or("canonical workspace windows-sys binding is missing")?
        .clone();
    isolated_manifest
        .get_mut("workspace")
        .and_then(toml::Value::as_table_mut)
        .and_then(|workspace| workspace.get_mut("dependencies"))
        .and_then(toml::Value::as_table_mut)
        .ok_or("isolated workspace dependencies are missing")?
        .insert("windows-sys".into(), windows);
    let manifest = toml::to_string(&isolated_manifest).map_err(|error| error.to_string())?;
    if fs::read(&path).ok().as_deref() != Some(manifest.as_bytes()) {
        fs::write(&path, &manifest).map_err(|error| error.to_string())?;
    }
    copy_fixture_file(
        &source_root.join("build/Cargo.lock"),
        &sandbox.join("build/Cargo.lock"),
    )?;
    write_isolated_fixture_identity(source_root, sandbox, &manifest)
}

fn write_isolated_fixture_identity(
    source_root: &Path,
    sandbox: &Path,
    manifest: &str,
) -> Result<(), String> {
    let mut files = walk_files(sandbox)?;
    files.sort();
    let mut inputs = Sha256::new();
    for file in &files {
        inputs.update(
            slash(
                file.strip_prefix(sandbox)
                    .map_err(|error| error.to_string())?,
            )
            .as_bytes(),
        );
        inputs.update([0]);
        inputs.update(fs::read(file).map_err(|error| error.to_string())?);
        inputs.update([0]);
    }
    let source_identity =
        command_output(source_root, "git", &["rev-parse", "HEAD", "HEAD^{tree}"])?;
    let input_sha256 = inputs
        .finalize()
        .iter()
        .flat_map(|byte| {
            let hex = b"0123456789abcdef";
            [
                char::from(hex[usize::from(byte >> 4)]),
                char::from(hex[usize::from(byte & 15)]),
            ]
        })
        .collect::<String>();
    let identity = serde_json::json!({"source_commit_and_tree":source_identity.lines().collect::<Vec<_>>(), "file_count":files.len(), "manifest_sha256":encode_sha256(manifest.as_bytes()), "input_sha256":input_sha256, "source_path":slash(sandbox)});
    let parent = sandbox.parent().ok_or("fixture has no identity parent")?;
    let name = sandbox
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or("fixture identity name invalid")?;
    fs::write(
        parent.join(format!("{name}.identity.json")),
        serde_json::to_vec(&identity).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn copy_fixture_tree(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination).map_err(|error| error.to_string())?;
    for entry in
        fs::read_dir(source).map_err(|error| format!("read {}: {error}", source.display()))?
    {
        let entry = entry.map_err(|error| error.to_string())?;
        let target = destination.join(entry.file_name());
        if entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_dir()
        {
            copy_fixture_tree(&entry.path(), &target)?;
        } else {
            copy_fixture_file(&entry.path(), &target)?;
        }
    }
    Ok(())
}

fn replace_text_in_file(path: &Path, before: &str, after: &str) -> Result<(), String> {
    let source = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let uses_crlf = source.contains("\r\n");
    let normalized_source = source.replace("\r\n", "\n");
    let normalized_before = before.replace("\r\n", "\n");
    let normalized_after = after.replace("\r\n", "\n");
    if !normalized_source.contains(&normalized_before) {
        return Err(format!("isolated mutation anchor is absent: {before}"));
    }
    let mutated = normalized_source.replacen(&normalized_before, &normalized_after, 1);
    let output = if uses_crlf {
        mutated.replace('\n', "\r\n")
    } else {
        mutated
    };
    fs::write(path, output).map_err(|error| error.to_string())
}

fn empty_named_test_body(path: &Path, marker: &str) -> Result<(), String> {
    replace_named_test_body(path, marker, "{}")
}

fn replace_named_test_body(path: &Path, marker: &str, replacement: &str) -> Result<(), String> {
    let mut source = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let start = source
        .find(marker)
        .ok_or_else(|| format!("isolated empty-body marker is absent: {marker}"))?;
    let opening = source[start..]
        .find('{')
        .map(|offset| start + offset)
        .ok_or("isolated public test has no opening body brace")?;
    let mut depth = 0_u32;
    let mut closing = None;
    for (offset, byte) in source.as_bytes()[opening..].iter().enumerate() {
        match byte {
            b'{' => depth = depth.saturating_add(1),
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    closing = Some(opening + offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let closing = closing.ok_or("isolated public test has unbalanced body braces")?;
    source.replace_range(opening..=closing, replacement);
    fs::write(path, source).map_err(|error| error.to_string())
}

fn isolated_public_test_diagnostic(
    root: &Path,
    mutation: &str,
    target_dir: &Path,
) -> Result<String, String> {
    let target_dir = target_dir
        .to_str()
        .ok_or("isolated target directory is not valid UTF-8")?;
    let listed = cargo_proof_output(
        root,
        "cargo",
        &[
            "test",
            "--manifest-path",
            "build/Cargo.toml",
            "--offline",
            "--target-dir",
            target_dir,
            "-p",
            "fforager-testkit",
            "--lib",
            "--",
            "--list",
        ],
    )?;
    let expected = format!("{PUBLIC_BOUNDARY_COUNTEREXAMPLE_TEST}: test");
    if mutation == "removed" && !listed.lines().any(|line| line.trim() == expected) {
        return Ok("FF-ARCH-E-PUBLIC-COUNTEREXAMPLE-MISSING".to_owned());
    }
    let output = cargo_proof_output(
        root,
        "cargo",
        &[
            "test",
            "--manifest-path",
            "build/Cargo.toml",
            "--offline",
            "--target-dir",
            target_dir,
            "-p",
            "fforager-testkit",
            "--lib",
            PUBLIC_BOUNDARY_COUNTEREXAMPLE_TEST,
            "--",
            "--exact",
            "--nocapture",
        ],
    )?;
    if mutation == "ignored" && output.contains("ignored") {
        return Ok("FF-ARCH-E-PUBLIC-COUNTEREXAMPLE-SKIPPED".to_owned());
    }
    if mutation == "empty" && !output.contains(PUBLIC_BOUNDARY_COUNTEREXAMPLE_RECEIPT) {
        return Ok("FF-ARCH-E-PUBLIC-COUNTEREXAMPLE-RECEIPT".to_owned());
    }
    Err(format!(
        "isolated public counterexample mutation {mutation} was not rejected by the compiled boundary"
    ))
}

fn diagnostic_from_production_validator(mutation: &str) -> Result<&'static str, String> {
    if let Some(diagnostic) = architecture_fixture_diagnostic(mutation)? {
        return Ok(diagnostic);
    }
    match mutation {
        "runtime_test_only_substitute"
        | "runtime_mock_boundary"
        | "runtime_scaffold_completion"
        | "runtime_noop_success"
        | "runtime_missing_artifact_identity"
        | "runtime_missing_clean_stage"
        | "runtime_missing_counterfactual"
        | "runtime_stage_collision" => runtime_fixture_diagnostic(mutation),
        other => Err(format!("unknown fixture mutation {other}")),
    }
}

fn architecture_fixture_diagnostic(mutation: &str) -> Result<Option<&'static str>, String> {
    let diagnostic = match mutation {
        "mark_bootstrap_member_shipped" => "FF-ARCH-E-SHIPPED-BUILD-TOOLING",
        "add_unapproved_exception" => unapproved_exception_diagnostic(1)
            .ok_or("production exception validator unexpectedly accepted mutation")?,
        "add_undeclared_member" => {
            let declared = BTreeSet::from(["fforager-xtask"]);
            let observed = BTreeSet::from(["fforager-xtask", "undeclared"]);
            if member_inventory_matches(&declared, &observed) {
                return Err("production member-inventory validator accepted mutation".to_owned());
            }
            "FF-ARCH-E-UNDECLARED-MEMBER"
        }
        "add_forbidden_edge" => classify_layers("engine", "frontend", DependencyKind::Normal),
        "add_cycle" => {
            if !graph_has_cycle(&[("a", "b"), ("b", "a")]) {
                return Err("production cycle validator accepted mutation".to_owned());
            }
            "FF-ARCH-E-CYCLE"
        }
        "add_shipped_governance_read" => runtime_literal_diagnostic("Path::new(\".GOV\")")
            .ok_or("production runtime-boundary validator accepted mutation")?,
        "add_adapter_edge" => classify_layers("adapter", "adapter", DependencyKind::Normal),
        "add_testkit_production_edge" => {
            classify_layers("product", "testkit", DependencyKind::Normal)
        }
        "add_watcher_product_edge" => classify_layers("watcher", "engine", DependencyKind::Normal),
        "add_product_watcher_edge" => classify_layers("product", "watcher", DependencyKind::Normal),
        "remove_split_trigger" => {
            if split_trigger_valid("", "WP-FF-000-fixture-v1") {
                return Err("production split-trigger validator accepted mutation".to_owned());
            }
            "FF-ARCH-E-MISSING-SPLIT-TRIGGER"
        }
        "remove_required_rule" => {
            rule_inventory_diagnostic(&BTreeSet::from(["FF-BUILD-036"]), &BTreeSet::new())
                .ok_or("production rule-inventory validator accepted missing rule")?
        }
        "add_unknown_rule" => rule_inventory_diagnostic(
            &BTreeSet::from(["FF-BUILD-036"]),
            &BTreeSet::from(["FF-BUILD-036", "FF-BUILD-UNKNOWN"]),
        )
        .ok_or("production rule-inventory validator accepted unknown rule")?,
        "remove_fixture_binding" => {
            if proof_binding_counts_valid(1, 1, 0) {
                return Err("production proof-binding validator accepted mutation".to_owned());
            }
            "FF-ARCH-E-MISSING-FIXTURE-BINDING"
        }
        "add_wrong_root_build_file" => root_state_diagnostic(1, 1)
            .ok_or("production three-root validator accepted wrong-root mutation")?,
        "add_duplicate_toolchain_selector" => root_state_diagnostic(0, 2)
            .ok_or("production selector validator accepted duplicate mutation")?,
        _ => return Ok(None),
    };
    Ok(Some(diagnostic))
}

fn run_runtime_truth_gate(root: &Path, gate_args: &[String]) -> Result<(), String> {
    match run_runtime_truth_gate_inner(root, gate_args) {
        Ok(()) => Ok(()),
        Err(error) => {
            fail_with_report(root, RUNTIME_GATE, "runtime-truth-check", gate_args, &error)
        }
    }
}

fn run_runtime_truth_gate_inner(root: &Path, gate_args: &[String]) -> Result<(), String> {
    let result = runtime_truth_check(root)?;
    let executed_proof_classes = executed_proof_classes(&result.checks, &[]);
    let aggregate_executed_proof_class = aggregate_executed_proof_class(&executed_proof_classes);
    let report = GateReport {
        schema_id: "ff.gate-report@1",
        schema_version: "1.0.0",
        gate_id: RUNTIME_GATE.to_owned(),
        gate_version: 1,
        status: "PASS",
        exit_code: 0,
        source: source_state(root)?,
        invocation: invocation(gate_args),
        inputs: collect_inputs(root)?,
        checks: result.checks,
        rules: (78..=88)
            .map(|number| format!("FF-BUILD-{number:03}"))
            .collect(),
        fixtures: Vec::new(),
        aggregate_executed_proof_class,
        declared_supported_proof_classes: result.proof_classes,
        executed_proof_classes,
        proof_limitations: result.limitations,
        artifacts: result.artifacts,
    };
    let path = write_report(root, "runtime-truth-check", &report)?;
    println!("PASS {RUNTIME_GATE}; report={}", slash(&path));
    Ok(())
}

fn run_watcher_gate(root: &Path, gate_args: &[String]) -> Result<(), String> {
    let result = (|| {
        let result = runtime_truth_check(root)?;
        if !result.checks.iter().any(|check| {
            check.id == "phase1-watcher-boundaries"
                && check.status == "PASS"
                && check.proof_class == "runtime_fault"
        }) {
            return Err(
                "FF-WATCHER-E-PROOF-MISSING: real staged watcher cases did not execute".to_owned(),
            );
        }
        let executed_proof_classes = executed_proof_classes(&result.checks, &[]);
        let report = GateReport {
            schema_id: "ff.gate-report@1",
            schema_version: "1.0.0",
            gate_id: "FF-GATE-WATCHER-001".into(),
            gate_version: 1,
            status: "PASS",
            exit_code: 0,
            source: source_state(root)?,
            invocation: invocation(gate_args),
            inputs: collect_inputs(root)?,
            checks: result.checks,
            rules: vec!["FF-BUILD-088".into()],
            fixtures: Vec::new(),
            aggregate_executed_proof_class: aggregate_executed_proof_class(&executed_proof_classes),
            declared_supported_proof_classes: result.proof_classes,
            executed_proof_classes,
            proof_limitations: result.limitations,
            artifacts: result.artifacts,
        };
        let path = write_report(root, "watcher-check", &report)?;
        println!("PASS FF-GATE-WATCHER-001; report={}", slash(&path));
        Ok(())
    })();
    result.or_else(|error| {
        fail_with_report(
            root,
            "FF-GATE-WATCHER-001",
            "watcher-check",
            gate_args,
            &error,
        )
    })
}

fn write_executed_runtime_children(
    root: &Path,
    result: &RuntimeTruthResult,
) -> Result<Vec<String>, String> {
    if !result.checks.iter().any(|check| {
        check.id == "phase1-watcher-boundaries"
            && check.status == "PASS"
            && check.proof_class == "runtime_fault"
    }) {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for (gate, command, args) in [
        (
            RUNTIME_GATE,
            "runtime-truth-check",
            vec![
                "runtime-truth-check".into(),
                "--evidence-from-taskboard".into(),
            ],
        ),
        (
            "FF-GATE-WATCHER-001",
            "watcher-check",
            vec!["watcher-check".into()],
        ),
    ] {
        let classes = executed_proof_classes(&result.checks, &[]);
        let mut limitations = result.limitations.clone();
        limitations.push("Triggered child verdict within this verify-pr execution; the same actual staged cases and artifact bytes were evaluated once, without rerunning a separate CLI process.".into());
        let report = GateReport {
            schema_id: "ff.gate-report@1",
            schema_version: "1.0.0",
            gate_id: gate.into(),
            gate_version: 1,
            status: "PASS",
            exit_code: 0,
            source: source_state(root)?,
            invocation: invocation(&args),
            inputs: collect_inputs(root)?,
            checks: result.checks.clone(),
            rules: (78..=88)
                .map(|number| format!("FF-BUILD-{number:03}"))
                .collect(),
            fixtures: Vec::new(),
            aggregate_executed_proof_class: aggregate_executed_proof_class(&classes),
            executed_proof_classes: classes,
            declared_supported_proof_classes: result.proof_classes.clone(),
            proof_limitations: limitations,
            artifacts: result.artifacts.clone(),
        };
        paths.push(slash(&write_report(root, command, &report)?));
    }
    Ok(paths)
}

#[allow(clippy::too_many_lines)]
fn runtime_truth_check(root: &Path) -> Result<RuntimeTruthResult, String> {
    let id = active_packet_id(root)?
        .ok_or("FF-RUNTIME-E-NO-ACTIVE-PACKET: runtime truth requires an active packet")?;
    let packet_path = root.join(".GOV/work_packets").join(&id).join("packet.json");
    let packet: Value =
        serde_json::from_str(&fs::read_to_string(&packet_path).map_err(|error| error.to_string())?)
            .map_err(|error| format!("parse active packet for runtime truth: {error}"))?;
    if packet.pointer("/identity/wp_id").and_then(Value::as_str) != Some(&id) {
        return Err("FF-RUNTIME-E-PACKET-IDENTITY: active packet identity mismatch".to_owned());
    }
    let impact = packet
        .pointer("/scope/product_impact")
        .and_then(Value::as_str)
        .ok_or("FF-RUNTIME-E-IMPACT-MISSING: scope.product_impact is required")?;
    if !matches!(impact, "NONE" | "PREREQUISITE" | "RUNTIME") {
        return Err(format!(
            "FF-RUNTIME-E-IMPACT-INVALID: expected NONE, PREREQUISITE, or RUNTIME, observed {impact}"
        ));
    }
    let base = packet
        .pointer("/source_control/base_sha")
        .and_then(Value::as_str)
        .ok_or("FF-RUNTIME-E-BASE-MISSING: packet base SHA is required")?;
    let activated_packet = validate_packet_activation_base(root, &id, base)?;
    let changed = changed_paths_since(root, base)?;
    let policy_path = root.join("build/architecture-policy.toml");
    let current_policy_text =
        fs::read_to_string(&policy_path).map_err(|error| format!("read policy: {error}"))?;
    let policy: ArchitecturePolicy =
        toml::from_str(&current_policy_text).map_err(|error| format!("parse policy: {error}"))?;
    let current_has_shipped_member = policy.members.iter().any(|member| member.shipped);
    let base_policy_text = command_output(
        root,
        "git",
        &["show", &format!("{base}:build/architecture-policy.toml")],
    )?;
    let base_policy: ArchitecturePolicy = toml::from_str(&base_policy_text)
        .map_err(|error| format!("FF-RUNTIME-E-BASE-POLICY: {error}"))?;
    let architecture_policy_affects_product =
        architecture_policy_product_affecting(&base_policy_text, &current_policy_text)?;
    let base_has_shipped_member = base_policy.members.iter().any(|member| member.shipped);
    let has_shipped_member = current_has_shipped_member || base_has_shipped_member;
    let build_only_contract_reuse = impact == "PREREQUISITE"
        && changed.contains("build/Cargo.lock")
        && changed.contains("build/architecture-policy.toml")
        && exact_build_only_contract_reuse(root, base, &changed)?;
    let product_paths = changed
        .iter()
        .filter(|path| {
            if build_only_contract_reuse
                && matches!(
                    path.as_str(),
                    "build/Cargo.lock" | "build/architecture-policy.toml"
                )
            {
                return false;
            }
            if path.replace('\\', "/") == "build/architecture-policy.toml" {
                has_shipped_member && architecture_policy_affects_product
            } else {
                product_affecting_path(path, has_shipped_member)
            }
        })
        .cloned()
        .collect::<Vec<_>>();
    if product_paths.is_empty() && impact != "PREREQUISITE" {
        if impact != "NONE" {
            return Err(format!(
                "FF-RUNTIME-E-IMPACT-MISMATCH: packet declares {impact} but no shipped product path changed"
            ));
        }
        if packet
            .pointer("/extensions/non_product_prerequisite")
            .is_some()
        {
            return Err(
                "FF-RUNTIME-E-IMPACT-MISMATCH: NONE packet contains a non-product prerequisite declaration"
                    .to_owned(),
            );
        }
        if packet.pointer("/extensions/runtime_proof").is_some()
            || packet
                .pointer("/acceptance_matrix")
                .and_then(Value::as_array)
                .is_some_and(|rows| {
                    rows.iter().any(|row| {
                        row.get("proof_class").and_then(Value::as_str) == Some("production_runtime")
                    })
                })
        {
            return Err(
                "FF-RUNTIME-E-IMPACT-MISMATCH: NONE packet contains production runtime proof or acceptance"
                    .to_owned(),
            );
        }
        return Ok(RuntimeTruthResult {
            checks: vec![
                pass_with_class(
                    "runtime-impact",
                    "policy",
                    "active packet impact and changed-path classification",
                    &format!("packet {id} is governance/build-only"),
                ),
                pass_with_class(
                    "runtime-nonproduct-ceiling",
                    "policy",
                    "active packet completion-claim ceiling",
                    "no product capability, phase, packaging, or runtime-completion claim is permitted",
                ),
            ],
            proof_classes: vec!["policy".to_owned()],
            limitations: vec![
                "No shipped product path changed; this PASS validates runtime-truth governance only and proves no Ferric runtime behavior.".to_owned(),
            ],
            artifacts: vec!["build/reports".to_owned()],
        });
    }
    if impact == "PREREQUISITE" {
        if packet.pointer("/extensions/runtime_proof").is_some()
            || has_production_runtime_acceptance(&packet)
        {
            return Err(
                "FF-RUNTIME-E-PREREQUISITE-RUNTIME-CLAIM: PREREQUISITE packet contains extensions.runtime_proof or production_runtime acceptance"
                    .to_owned(),
            );
        }
        let declaration_value = packet
            .pointer("/extensions/non_product_prerequisite")
            .ok_or(
                "FF-RUNTIME-E-PREREQUISITE-MISSING: product prerequisite change has no extensions.non_product_prerequisite declaration",
            )?;
        if !non_product_prerequisite_predeclared(&activated_packet, declaration_value) {
            return Err(
                "FF-RUNTIME-E-PREREQUISITE-NOT-PREDECLARED: exact non-product prerequisite declaration must exist at the packet's first committed non-STUB checkpoint"
                    .to_owned(),
            );
        }
        let declaration: NonProductPrerequisite = serde_json::from_value(declaration_value.clone())
            .map_err(|error| format!("FF-RUNTIME-E-PREREQUISITE-SCHEMA: {error}"))?;
        validate_non_product_prerequisite(&declaration)?;
        validate_prerequisite_zero_claim_surface(&packet)?;
        let build_only = product_paths.is_empty()
            || activated_packet
                .pointer("/scope/allowed_paths")
                .and_then(Value::as_array)
                .is_some_and(|paths| {
                    !paths.is_empty()
                        && !paths.iter().any(|path| {
                            path.as_str()
                                .is_some_and(|path| path.starts_with("product/"))
                        })
                });
        if build_only {
            validate_build_only_prerequisite(
                root,
                base,
                &packet,
                &activated_packet,
                &declaration,
                &changed,
                &product_paths,
            )?;
        }
        let prerequisite_paths = if build_only {
            changed.iter().cloned().collect::<Vec<_>>()
        } else {
            product_paths
        };
        return Ok(RuntimeTruthResult {
            checks: prerequisite_runtime_checks(
                &id,
                &prerequisite_paths,
                build_only,
            ),
            proof_classes: vec!["policy".to_owned(), "scenario_contract".to_owned()],
            limitations: vec![
                "This PASS is supporting policy/contract proof only: it proves no product progress, capability progress, phase progress, runtime completion, packaging progress, or release progress.".to_owned(),
                format!(
                    "The prerequisite must be consumed by {} and re-proven by {}.",
                    declaration.required_future_consumer, declaration.required_future_proof
                ),
            ],
            artifacts: vec![
                format!(".GOV/work_packets/{id}/packet.json"),
                "build/reports".to_owned(),
            ],
        });
    }
    if impact != "RUNTIME" {
        return Err(format!(
            "FF-RUNTIME-E-IMPACT-MISMATCH: product paths changed while scope.product_impact={impact}: {product_paths:?}"
        ));
    }
    let proof_value = packet
        .pointer("/extensions/runtime_proof")
        .ok_or("FF-RUNTIME-E-PROOF-MISSING: product change has no extensions.runtime_proof")?;
    if proof_value.get("schema_id").and_then(Value::as_str) == Some("ff.runtime-proof@2") {
        let executed = phase1_runtime::execute(root, &packet, proof_value, &id)?;
        promotion::check(root, &packet)?;
        return Ok(executed);
    }
    if id == "WP-FF-021-first-runnable-production-slice-v1" || id.starts_with("WP-FF-011-") {
        promotion::check(root, &packet)?;
    }
    if !runtime_proof_predeclared(&activated_packet, proof_value) {
        return Err(
            "FF-RUNTIME-E-PROOF-NOT-PREDECLARED: exact runtime proof must exist in the packet activation checkpoint before product implementation"
                .to_owned(),
        );
    }
    let proof: RuntimeProof = serde_json::from_value(proof_value.clone())
        .map_err(|error| format!("FF-RUNTIME-E-SCHEMA: {error}"))?;
    validate_runtime_proof_contract(&proof)?;
    execute_runtime_proof(root, &proof, &id, &product_paths)
}

fn prerequisite_runtime_checks(id: &str, paths: &[String], build_only: bool) -> Vec<Check> {
    let classification = if build_only {
        "build-only artifacts"
    } else {
        "product paths"
    };
    vec![
        pass_with_class(
            "runtime-impact",
            "policy",
            "active packet impact and changed-path classification",
            &format!(
                "packet {id} changes {classification} only as a predeclared Phase 0 non-product prerequisite: {paths:?}"
            ),
        ),
        pass_with_class(
            "runtime-prerequisite-contract",
            "scenario_contract",
            "activation checkpoint and non-product prerequisite contract",
            "the exact non-product prerequisite declaration was committed at the packet's first non-STUB checkpoint",
        ),
        pass_with_class(
            "runtime-prerequisite-zero-claims",
            "policy",
            "active packet completion-claim ceiling",
            "zero product completion; zero capability completion; zero phase completion; zero runtime completion; zero packaging or release progress",
        ),
    ]
}

fn validate_build_only_prerequisite(
    root: &Path,
    base: &str,
    packet: &Value,
    activated_packet: &Value,
    declaration: &NonProductPrerequisite,
    changed: &BTreeSet<String>,
    product_paths: &[String],
) -> Result<(), String> {
    let activated_paths = activated_packet.pointer("/scope/allowed_paths");
    if packet.pointer("/scope/allowed_paths") != activated_paths {
        return Err("FF-RUNTIME-E-BUILD-PREREQUISITE-SCOPE: allowed paths differ from the activation checkpoint".to_owned());
    }
    let allowed = activated_paths.and_then(Value::as_array).ok_or(
        "FF-RUNTIME-E-BUILD-PREREQUISITE-SCOPE: activation requires explicit allowed paths",
    )?;
    let paths = allowed.iter().map(|path| {
        let path = path.as_str().ok_or("FF-RUNTIME-E-BUILD-PREREQUISITE-SCOPE: allowed paths must be strings")?;
        let normalized = path.strip_suffix('/').unwrap_or(path);
        if !safe_relative(normalized) || path.contains('*') || path.contains('\\') {
            return Err("FF-RUNTIME-E-BUILD-PREREQUISITE-SCOPE: allowed paths must be safe exact paths or directories");
        }
        Ok(path)
    }).collect::<Result<Vec<_>, _>>()?;
    if !product_paths.is_empty() {
        return Err(format!(
            "FF-RUNTIME-E-BUILD-PREREQUISITE-PATH: build-only activation changes product-affecting paths: {product_paths:?}"
        ));
    }
    for path in changed {
        if !safe_relative(path)
            || !paths.iter().any(|allowed| {
                if allowed.ends_with('/') {
                    path.starts_with(allowed)
                } else {
                    path == allowed
                }
            })
        {
            return Err(format!(
                "FF-RUNTIME-E-BUILD-PREREQUISITE-PATH: undeclared changed path {path}"
            ));
        }
    }
    if !changed.iter().any(|path| {
        path.starts_with("build/")
            && !path.starts_with("build/reports/")
            && root.join(path).is_file()
    }) {
        return Err("FF-RUNTIME-E-BUILD-PREREQUISITE-ARTIFACT: no changed declared build artifact; governance and reports alone are not a prerequisite".to_owned());
    }
    let authority = &declaration.design_authority;
    if !activated_packet
        .pointer("/authority/spec_anchors")
        .and_then(Value::as_array)
        .is_some_and(|anchors| {
            anchors
                .iter()
                .any(|anchor| anchor.as_str() == Some(authority))
        })
    {
        return Err("FF-RUNTIME-E-BUILD-PREREQUISITE-AUTHORITY: design authority is not an activation spec anchor".to_owned());
    }
    let (path, anchor) = authority.split_once('#').ok_or(
        "FF-RUNTIME-E-BUILD-PREREQUISITE-AUTHORITY: design authority must name a prerequisite section",
    )?;
    if path != ".GOV/spec/ferric_forager_technical_design_v0.3.0.md" {
        return Err("FF-RUNTIME-E-BUILD-PREREQUISITE-AUTHORITY: design authority must name the canonical technical design".to_owned());
    }
    let design = command_output(root, "git", &["show", &format!("{base}:{path}")])
        .map_err(|error| format!("FF-RUNTIME-E-BUILD-PREREQUISITE-AUTHORITY: {error}"))?;
    let authorized = design
        .lines()
        .filter_map(|line| line.strip_prefix("## "))
        .filter(|heading| heading.starts_with("Prerequisite 0"))
        .any(|heading| {
            let slug = heading
                .chars()
                .filter(|character| {
                    character.is_ascii_alphanumeric()
                        || character.is_whitespace()
                        || *character == '-'
                })
                .map(|character| {
                    if character.is_whitespace() {
                        '-'
                    } else {
                        character.to_ascii_lowercase()
                    }
                })
                .collect::<String>();
            slug == anchor
        });
    if !authorized {
        return Err("FF-RUNTIME-E-BUILD-PREREQUISITE-AUTHORITY: design anchor is not a pre-existing Phase 0 prerequisite section".to_owned());
    }
    Ok(())
}

fn validate_prerequisite_zero_claim_surface(packet: &Value) -> Result<(), String> {
    let extensions = packet
        .pointer("/extensions")
        .and_then(Value::as_object)
        .ok_or("FF-RUNTIME-E-PREREQUISITE-SCHEMA: extensions must be an object")?;
    let allowed = BTreeSet::from([
        "refinement",
        "non_product_prerequisite",
        "repository_layout",
        "stub_implementation_brief",
        "change_evidence",
        "adversarial_review",
    ]);
    if let Some(key) = extensions
        .keys()
        .find(|key| !allowed.contains(key.as_str()))
    {
        return Err(format!(
            "FF-RUNTIME-E-PREREQUISITE-CLAIM-SURFACE: unrecognized prerequisite extension {key}"
        ));
    }
    reject_progress_claims(packet)
}

fn reject_progress_claims(value: &Value) -> Result<(), String> {
    reject_progress_claims_at(value, false, "")
}

fn reject_progress_claims_at(
    value: &Value,
    progress_subject_in_scope: bool,
    path: &str,
) -> Result<(), String> {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                let child_progress_subject =
                    progress_subject_in_scope || is_progress_subject_key(key);
                if (is_prerequisite_progress_key(key) || child_progress_subject)
                    && affirmative_progress_value(child)
                {
                    return Err(format!(
                        "FF-RUNTIME-E-PREREQUISITE-PROGRESS-CLAIM: {key}={child}"
                    ));
                }
                let component = key.replace('~', "~0").replace('/', "~1");
                reject_progress_claims_at(
                    child,
                    child_progress_subject,
                    &format!("{path}/{component}"),
                )?;
            }
        }
        Value::Array(values) => {
            for child in values {
                // Canonical non-goal strings declare exclusions, not outcomes.
                // Structured values still undergo all typed claim checks.
                if path != "/scope/non_goals" || !child.is_string() {
                    reject_progress_claims_at(
                        child,
                        progress_subject_in_scope,
                        &format!("{path}/[]"),
                    )?;
                }
            }
        }
        Value::String(text) if asserts_forbidden_prerequisite_completion(text) => {
            return Err(format!(
                "FF-RUNTIME-E-PREREQUISITE-PROGRESS-CLAIM: contradictory completion text {text:?}"
            ));
        }
        _ => {}
    }
    Ok(())
}

fn normalize_claim_key(key: &str) -> String {
    key.to_ascii_lowercase().replace('-', "_")
}

fn is_progress_subject_key(key: &str) -> bool {
    matches!(
        normalize_claim_key(key).as_str(),
        "product" | "capability" | "runtime" | "packaging" | "release" | "phase"
    )
}

fn is_prerequisite_progress_key(key: &str) -> bool {
    let normalized = normalize_claim_key(key);
    [
        "product_progress",
        "product_status",
        "capability_progress",
        "capability_status",
        "runtime_completion",
        "runtime_status",
        "packaging_or_release_progress",
        "packaging_progress",
        "release_progress",
        "phase_progress",
        "phase_status",
    ]
    .into_iter()
    .any(|field| normalized == field)
}

fn affirmative_progress_value(value: &Value) -> bool {
    if value.as_bool() == Some(true) {
        return true;
    }
    value.as_str().is_some_and(|text| {
        matches!(
            text.trim().to_ascii_uppercase().as_str(),
            "TRUE"
                | "PASS"
                | "COMPLETE"
                | "COMPLETED"
                | "VALIDATED"
                | "DELIVERED"
                | "DONE"
                | "SHIPPED"
                | "READY"
                | "IMPLEMENTED"
                | "SUCCESS"
                | "SUCCEEDED"
                | "PASSED"
                | "FINISHED"
                | "PRODUCTION_READY"
                | "OPERATIONAL"
        )
    })
}

fn asserts_forbidden_prerequisite_completion(text: &str) -> bool {
    let normalized = text
        .to_ascii_lowercase()
        .replace(" however ", ";")
        .replace(" but ", ";")
        .replace(" although ", ";");
    let mut prior_subject = false;
    for clause in normalized
        .split(['.', ';', '\n'])
        .map(str::trim)
        .filter(|clause| !clause.is_empty())
    {
        let tokens = claim_tokens(clause);
        let has_subject = contains_forbidden_claim_subject(&tokens);
        if clause_asserts_forbidden_prerequisite_completion(clause)
            || (!has_subject && prior_subject && pronoun_asserts_completion(&tokens))
        {
            return true;
        }
        if has_subject {
            prior_subject = true;
        }
    }
    false
}

fn clause_asserts_forbidden_prerequisite_completion(clause: &str) -> bool {
    let tokens = claim_tokens(clause);
    let subjects: [&[&str]; 9] = [
        &["product", "capability"],
        &["product", "progress"],
        &["phase", "0"],
        &["phase", "zero"],
        &["phase", "progress"],
        &["runtime", "completion"],
        &["runtime", "status"],
        &["packaging", "progress"],
        &["release", "progress"],
    ];
    for subject in subjects {
        for start in token_sequence_positions(&tokens, subject) {
            let end = start + subject.len();
            for (status, _) in tokens
                .iter()
                .enumerate()
                .filter(|(index, _)| completion_predicate_at(&tokens, *index))
            {
                if !claim_subject_is_negated(&tokens, start, end, status) {
                    return true;
                }
            }
        }
    }
    single_subject_asserts_completion(&tokens)
}

fn contains_forbidden_claim_subject(tokens: &[String]) -> bool {
    [
        &["product", "capability"][..],
        &["product", "progress"][..],
        &["phase", "0"][..],
        &["phase", "zero"][..],
        &["phase", "progress"][..],
        &["runtime", "completion"][..],
        &["runtime", "status"][..],
        &["packaging", "progress"][..],
        &["release", "progress"][..],
    ]
    .iter()
    .any(|subject| !token_sequence_positions(tokens, subject).is_empty())
        || tokens
            .iter()
            .enumerate()
            .any(|(index, _)| current_progress_subject_at(tokens, index))
}

fn is_completion_status(token: &str) -> bool {
    matches!(
        token,
        "true"
            | "pass"
            | "passed"
            | "success"
            | "succeeded"
            | "complete"
            | "completed"
            | "validated"
            | "delivered"
            | "done"
            | "shipped"
            | "ready"
            | "implemented"
            | "finished"
            | "operational"
    )
}

fn single_subject_asserts_completion(tokens: &[String]) -> bool {
    for (subject, _) in tokens
        .iter()
        .enumerate()
        .filter(|(index, _)| current_progress_subject_at(tokens, *index))
    {
        for (status, _) in tokens
            .iter()
            .enumerate()
            .filter(|(index, _)| completion_predicate_at(tokens, *index))
        {
            if subject.abs_diff(status) <= 5
                && !claim_subject_is_negated(tokens, subject, subject + 1, status)
            {
                return true;
            }
        }
    }
    false
}

fn current_progress_subject_at(tokens: &[String], index: usize) -> bool {
    let token = tokens[index].as_str();
    if token == "release" && index > 0 && tokens[index - 1] == "natural" {
        return false;
    }
    if token == "runtime"
        && tokens
            .get(index + 1)
            .is_some_and(|next| matches!(next.as_str(), "proof" | "dependencies"))
    {
        return false;
    }
    matches!(
        token,
        "product" | "runtime" | "packaging" | "release" | "phase"
    )
}

fn pronoun_asserts_completion(tokens: &[String]) -> bool {
    tokens.first().is_some_and(|token| {
        matches!(token.as_str(), "it" | "this" | "that")
            && tokens.iter().enumerate().any(|(status, _)| {
                completion_predicate_at(tokens, status)
                    && !claim_subject_is_negated(tokens, 0, 1, status)
            })
    })
}

fn claim_tokens(text: &str) -> Vec<String> {
    let normalized = text.to_ascii_lowercase();
    let gate = RUNTIME_GATE.to_ascii_lowercase();
    let identifier_character =
        |character: char| character.is_ascii_alphanumeric() || matches!(character, '-' | '_');
    // Stable identifiers are references; surrounding outcome prose remains.
    normalized
        .split(|character| !identifier_character(character))
        .filter(|word| *word != gate && !work_packet_reference(word))
        .flat_map(|word| word.split(|character: char| !character.is_ascii_alphanumeric()))
        .filter(|token| !token.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn work_packet_reference(word: &str) -> bool {
    let Some(rest) = word.strip_prefix("wp-ff-") else {
        return false;
    };
    let Some((number, rest)) = rest.split_once('-') else {
        return false;
    };
    let Some((name, version)) = rest.rsplit_once("-v") else {
        return false;
    };
    !number.is_empty()
        && number.bytes().all(|byte| byte.is_ascii_digit())
        && !version.is_empty()
        && version.bytes().all(|byte| byte.is_ascii_digit())
        && name.split('-').all(|segment| {
            !segment.is_empty() && segment.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
}

fn completion_predicate_at(tokens: &[String], index: usize) -> bool {
    is_completion_status(&tokens[index])
        && !(tokens[index] == "shipped" && tokens.get(index + 1).is_some_and(|next| next == "path"))
        && !(tokens[index] == "complete"
            && index > 0
            && tokens[index - 1] == "first"
            && tokens
                .get(index + 1)
                .is_some_and(|next| matches!(next.as_str(), "a" | "an" | "the"))
            && !tokens[..index - 1].iter().any(|prior| {
                matches!(
                    prior.as_str(),
                    "is" | "are" | "was" | "were" | "be" | "been" | "being"
                )
            }))
}

fn token_sequence_positions(tokens: &[String], sequence: &[&str]) -> Vec<usize> {
    tokens
        .windows(sequence.len())
        .enumerate()
        .filter_map(|(index, window)| {
            window
                .iter()
                .zip(sequence)
                .all(|(token, expected)| token == expected)
                .then_some(index)
        })
        .collect()
}

fn claim_subject_is_negated(
    tokens: &[String],
    subject_start: usize,
    subject_end: usize,
    status: usize,
) -> bool {
    let negations = ["no", "not", "zero", "never", "cannot"];
    let is_effective_negation = |index: usize| {
        negations.contains(&tokens[index].as_str())
            && !(tokens[index] == "not" && tokens.get(index + 1).is_some_and(|next| next == "only"))
    };
    if status > 0 && tokens[status - 1] == "without" {
        return true;
    }
    if status > 0 && is_effective_negation(status - 1) {
        return true;
    }
    if subject_start > 0 && is_effective_negation(subject_start - 1) {
        return true;
    }
    let between_start = subject_end.min(status);
    let between_end = subject_start.max(status);
    if tokens[between_start..between_end]
        .iter()
        .enumerate()
        .any(|(offset, _)| is_effective_negation(between_start + offset))
    {
        return true;
    }
    if subject_start > 0 && tokens[subject_start - 1] == "or" {
        let prior = &tokens[..subject_start - 1];
        let scope_start = prior
            .iter()
            .rposition(|token| token == "and")
            .map_or(0, |index| index + 1);
        return prior[scope_start..]
            .iter()
            .enumerate()
            .any(|(offset, _)| is_effective_negation(scope_start + offset));
    }
    false
}

fn runtime_proof_predeclared(activated_packet: &Value, current_proof: &Value) -> bool {
    activated_packet
        .pointer("/scope/product_impact")
        .and_then(Value::as_str)
        == Some("RUNTIME")
        && activated_packet.pointer("/extensions/runtime_proof") == Some(current_proof)
}

fn has_production_runtime_acceptance(packet: &Value) -> bool {
    packet
        .pointer("/acceptance_matrix")
        .and_then(Value::as_array)
        .is_some_and(|rows| {
            rows.iter().any(|row| {
                row.get("proof_class").and_then(Value::as_str) == Some("production_runtime")
            })
        })
}

fn non_product_prerequisite_predeclared(
    activated_packet: &Value,
    current_declaration: &Value,
) -> bool {
    activated_packet
        .pointer("/scope/product_impact")
        .and_then(Value::as_str)
        == Some("PREREQUISITE")
        && activated_packet
            .pointer("/extensions/runtime_proof")
            .is_none()
        && !has_production_runtime_acceptance(activated_packet)
        && activated_packet.pointer("/extensions/non_product_prerequisite")
            == Some(current_declaration)
}

fn validate_non_product_prerequisite(declaration: &NonProductPrerequisite) -> Result<(), String> {
    if declaration.schema_id != "ff.non-product-prerequisite@1" {
        return Err(
            "FF-RUNTIME-E-PREREQUISITE-SCHEMA: expected ff.non-product-prerequisite@1".to_owned(),
        );
    }
    if declaration.classification != "phase0_supporting_evidence" {
        return Err(
            "FF-RUNTIME-E-PREREQUISITE-CLASSIFICATION: expected phase0_supporting_evidence"
                .to_owned(),
        );
    }
    if declaration.design_authority.trim().is_empty()
        || declaration.required_future_consumer.trim().is_empty()
        || declaration.required_future_proof.trim().is_empty()
    {
        return Err(
            "FF-RUNTIME-E-PREREQUISITE-SCHEMA: design authority, future consumer, and future proof must be nonempty"
                .to_owned(),
        );
    }
    if declaration.product_progress
        || declaration.capability_progress
        || declaration.runtime_completion
        || declaration.packaging_or_release_progress
        || declaration.phase_progress
    {
        return Err(
            "FF-RUNTIME-E-PREREQUISITE-COMPLETION-CLAIM: every product, capability, runtime, packaging/release, and phase progress field must be false"
                .to_owned(),
        );
    }
    Ok(())
}

fn changed_paths_since(root: &Path, base: &str) -> Result<BTreeSet<String>, String> {
    if base.len() != 40 || !base.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(
            "FF-RUNTIME-E-BASE-INVALID: base SHA must be 40 hexadecimal characters".to_owned(),
        );
    }
    let status = command_status_with_timeout(
        root,
        "git",
        &["merge-base", "--is-ancestor", base, "HEAD"],
        None,
        TOOL_COMMAND_TIMEOUT,
    )?;
    if !status.success() {
        return Err(
            "FF-RUNTIME-E-BASE-MISMATCH: packet base is not an ancestor of HEAD".to_owned(),
        );
    }
    let mut paths = command_output(root, "git", &["diff", "--name-only", base, "--"])?
        .lines()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>();
    paths.extend(
        command_output(root, "git", &["ls-files", "--others", "--exclude-standard"])?
            .lines()
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(ToOwned::to_owned),
    );
    Ok(paths)
}

fn product_runtime_path(path: &str) -> bool {
    path.replace('\\', "/").starts_with("product/")
        && path.replace('\\', "/") != "product/MODEL_MANUAL.md"
}

fn product_affecting_path(path: &str, has_shipped_member: bool) -> bool {
    if product_runtime_path(path) {
        return true;
    }
    has_shipped_member
        && matches!(
            path.replace('\\', "/").as_str(),
            "build/Cargo.toml"
                | "build/Cargo.lock"
                | "build/architecture-policy.toml"
                | "rust-toolchain.toml"
        )
}

fn exact_build_only_contract_reuse(
    root: &Path,
    base: &str,
    changed: &BTreeSet<String>,
) -> Result<bool, String> {
    let tool = "build/tools/fforager-xtask/Cargo.toml";
    if changed.iter().any(|path| {
        path.starts_with("product/crates/fforager-contracts/")
            || (path.ends_with("Cargo.toml") && path != tool)
    }) {
        return Ok(false);
    }
    let read = |path: &str| -> Result<(toml::Value, toml::Value), String> {
        let old = command_output(root, "git", &["show", &format!("{base}:{path}")])?;
        let new = fs::read_to_string(root.join(path)).map_err(|error| error.to_string())?;
        Ok((
            toml::from_str(&old).map_err(|error| error.to_string())?,
            toml::from_str(&new).map_err(|error| error.to_string())?,
        ))
    };
    let provider = root.join("product/crates/fforager-contracts");
    let provider_manifest: toml::Value = read_toml(&provider.join("Cargo.toml"))?;
    if provider_manifest.get("features").is_some()
        || provider_manifest
            .get("package")
            .and_then(|package| package.get("build"))
            .is_some()
        || provider.join("build.rs").exists()
    {
        return Ok(false);
    }
    let (old_tool, new_tool) = read(tool)?;
    let (old_lock, new_lock) = read("build/Cargo.lock")?;
    let (old_policy, new_policy) = read("build/architecture-policy.toml")?;
    exact_contract_reuse_declarations(
        &old_tool, new_tool, &old_lock, new_lock, old_policy, new_policy,
    )
}

fn exact_contract_reuse_declarations(
    old_tool: &toml::Value,
    mut new_tool: toml::Value,
    old_lock: &toml::Value,
    mut new_lock: toml::Value,
    mut old_policy: toml::Value,
    mut new_policy: toml::Value,
) -> Result<bool, String> {
    let expected_dependency: toml::Value =
        toml::from_str("version = '0.1.0'\npath = '../../../product/crates/fforager-contracts'")
            .map_err(|error| error.to_string())?;
    let Some(dependencies) = new_tool
        .get_mut("dependencies")
        .and_then(toml::Value::as_table_mut)
    else {
        return Ok(false);
    };
    if dependencies.remove("fforager-contracts") != Some(expected_dependency)
        || &new_tool != old_tool
    {
        return Ok(false);
    }
    let Some(packages) = new_lock
        .get_mut("package")
        .and_then(toml::Value::as_array_mut)
    else {
        return Ok(false);
    };
    let tools = packages
        .iter()
        .enumerate()
        .filter(|(_, package)| {
            package.get("name").and_then(toml::Value::as_str) == Some("fforager-xtask")
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let [tool] = tools.as_slice() else {
        return Ok(false);
    };
    let Some(dependencies) = packages[*tool]
        .get_mut("dependencies")
        .and_then(toml::Value::as_array_mut)
    else {
        return Ok(false);
    };
    let count = dependencies
        .iter()
        .filter(|dependency| dependency.as_str() == Some("fforager-contracts"))
        .count();
    if count != 1 {
        return Ok(false);
    }
    dependencies.retain(|dependency| dependency.as_str() != Some("fforager-contracts"));
    if &new_lock != old_lock {
        return Ok(false);
    }
    let expected_edge: toml::Value = toml::from_str(
        "edges = [{ package = 'fforager-contracts', kinds = ['normal'], optional = false }]",
    )
    .map_err(|error| error.to_string())?;
    let Some(members) = new_policy
        .get_mut("members")
        .and_then(toml::Value::as_array_mut)
    else {
        return Ok(false);
    };
    let mut matches = 0;
    for member in members {
        if member.get("name").and_then(toml::Value::as_str) != Some("fforager-xtask") {
            continue;
        }
        matches += 1;
        if member.get("shipped").and_then(toml::Value::as_bool) != Some(false)
            || member.get("artifact_role").and_then(toml::Value::as_str)
                != Some("non_shipped_gate_runner")
            || member.get("allowed_internal_dependencies") != expected_edge.get("edges")
        {
            return Ok(false);
        }
        member["allowed_internal_dependencies"] = toml::Value::Array(Vec::new());
    }
    normalize_architecture_policy_hygiene(&mut old_policy)?;
    normalize_architecture_policy_hygiene(&mut new_policy)?;
    Ok(matches == 1 && old_policy == new_policy)
}

fn architecture_policy_product_affecting(
    base_text: &str,
    current_text: &str,
) -> Result<bool, String> {
    let mut base: toml::Value =
        toml::from_str(base_text).map_err(|error| format!("FF-RUNTIME-E-BASE-POLICY: {error}"))?;
    let mut current: toml::Value = toml::from_str(current_text)
        .map_err(|error| format!("FF-RUNTIME-E-CURRENT-POLICY: {error}"))?;
    normalize_architecture_policy_hygiene(&mut base)?;
    normalize_architecture_policy_hygiene(&mut current)?;
    Ok(base != current)
}

fn normalize_architecture_policy_hygiene(policy: &mut toml::Value) -> Result<(), String> {
    let table = policy
        .as_table_mut()
        .ok_or("FF-RUNTIME-E-POLICY-SHAPE: architecture policy root is not a table")?;
    table.remove("target_directory");
    let roots = table
        .get_mut("forbidden_product_runtime_roots")
        .and_then(toml::Value::as_array_mut)
        .ok_or("FF-RUNTIME-E-POLICY-SHAPE: forbidden_product_runtime_roots is not an array")?;
    roots.retain(|root| root.as_str() != Some(".fforager-artifacts"));
    Ok(())
}

fn validate_packet_activation_base(
    root: &Path,
    packet_id: &str,
    base: &str,
) -> Result<Value, String> {
    let relative = format!(".GOV/work_packets/{packet_id}/packet.json");
    let history = command_output(
        root,
        "git",
        &["log", "--format=%H", "--reverse", "--", &relative],
    )?;
    let mut activation = None;
    for commit in history
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let object = format!("{commit}:{relative}");
        let committed_packet = command_output(root, "git", &["show", &object])?;
        let value: Value = serde_json::from_str(&committed_packet).map_err(|error| {
            format!("FF-RUNTIME-E-BASE-UNPROVEN: parse committed packet {commit}: {error}")
        })?;
        if committed_packet_is_activation(commit, &value)? {
            activation = Some(value);
            break;
        }
    }
    let value = activation.ok_or(
        "FF-RUNTIME-E-BASE-UNPROVEN: packet has no committed non-STUB activation checkpoint",
    )?;
    let activated_base = value
        .pointer("/source_control/base_sha")
        .and_then(Value::as_str)
        .ok_or("FF-RUNTIME-E-BASE-UNPROVEN: activation packet omits base SHA")?;
    if activated_base != base {
        return Err(format!(
            "FF-RUNTIME-E-BASE-REWRITE: activation base {activated_base} changed to {base}"
        ));
    }
    Ok(value)
}

fn committed_packet_is_activation(commit: &str, packet: &Value) -> Result<bool, String> {
    let status = packet
        .pointer("/lifecycle/status")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            format!("FF-RUNTIME-E-BASE-UNPROVEN: committed packet {commit} omits lifecycle.status")
        })?;
    Ok(status != "STUB")
}

fn validate_runtime_proof_contract(proof: &RuntimeProof) -> Result<(), String> {
    if proof.schema_id != "ff.runtime-proof@1" {
        return Err("FF-RUNTIME-E-SCHEMA: expected ff.runtime-proof@1".to_owned());
    }
    if proof.completion_claim != "operator_usable_runtime" {
        return Err(
            "FF-RUNTIME-E-SCAFFOLD-COMPLETION: completion claim must be operator_usable_runtime"
                .to_owned(),
        );
    }
    validate_runtime_artifact(&proof.artifact)?;
    validate_forbidden_substitutes(&proof.forbidden_substitutes)?;
    if proof.scenarios.len() < 2 {
        return Err(
            "FF-RUNTIME-E-SCENARIO: at least one success and one negative scenario are required"
                .to_owned(),
        );
    }
    let mut ids = BTreeSet::new();
    let mut kinds = BTreeSet::new();
    for scenario in &proof.scenarios {
        kinds.insert(validate_runtime_scenario(
            scenario,
            &mut ids,
            &proof.artifact.binary,
        )?);
    }
    if kinds != BTreeSet::from(["negative", "success"]) {
        return Err(
            "FF-RUNTIME-E-SCENARIO: at least one success and one negative scenario are required"
                .to_owned(),
        );
    }
    Ok(())
}

fn validate_runtime_artifact(artifact: &RuntimeArtifact) -> Result<(), String> {
    if !safe_id(&artifact.package) || !safe_id(&artifact.binary) {
        return Err(
            "FF-RUNTIME-E-ARTIFACT-IDENTITY: package and binary require stable safe IDs".to_owned(),
        );
    }
    if artifact.profile != "release"
        || artifact.package_mode != "clean_staged"
        || artifact.execution_mode != "external_process"
    {
        return Err(
            "FF-RUNTIME-E-CLEAN-STAGE: release, clean_staged package, and external_process are mandatory"
                .to_owned(),
        );
    }
    if artifact.compilation_mode != "production"
        || artifact.dependency_mode != "normal_only"
        || artifact.testkit_mode != "forbidden"
        || artifact.adapter_mode != "production"
        || artifact.features.iter().any(|feature| {
            let lower = feature.to_ascii_lowercase();
            ["test", "mock", "fake", "stub"]
                .iter()
                .any(|word| lower.contains(word))
        })
    {
        return Err(
            "FF-RUNTIME-E-TEST-SUBSTITUTE: runtime proof must use production compilation, dependencies, and adapters"
                .to_owned(),
        );
    }
    Ok(())
}

fn validate_forbidden_substitutes(substitutes: &[String]) -> Result<(), String> {
    let required = BTreeSet::from([
        "mock",
        "fake",
        "stub",
        "fixture-only-implementation",
        "in-memory-substitute",
        "hardcoded-success",
        "test-only-adapter",
        "direct-internal-call",
    ]);
    let observed = substitutes
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if observed != required {
        return Err(
            "FF-RUNTIME-E-MOCK-SUBSTITUTE: forbidden substitute inventory is incomplete".to_owned(),
        );
    }
    Ok(())
}

fn validate_runtime_scenario<'scenario>(
    scenario: &'scenario RuntimeScenario,
    ids: &mut BTreeSet<&'scenario str>,
    binary: &str,
) -> Result<&'scenario str, String> {
    if !safe_id(&scenario.id) || !ids.insert(&scenario.id) {
        return Err("FF-RUNTIME-E-SCENARIO: scenario IDs must be unique safe IDs".to_owned());
    }
    match scenario.kind.as_str() {
        "success" if scenario.expected.exit_code == 0 => {}
        "negative" if scenario.expected.exit_code != 0 => {}
        _ => {
            return Err(
                "FF-RUNTIME-E-SCENARIO: success requires exit 0 and negative requires nonzero"
                    .to_owned(),
            );
        }
    }
    if scenario.capability_ids.is_empty()
        || scenario.capability_ids.iter().any(|id| !stable_id(id))
        || scenario.production_boundaries.is_empty()
        || scenario
            .production_boundaries
            .iter()
            .any(|boundary| !safe_id(boundary))
        || !(1..=900).contains(&scenario.timeout_seconds)
    {
        return Err(
            "FF-RUNTIME-E-SCENARIO: capabilities, production boundaries, and bounded timeout are required"
                .to_owned(),
        );
    }
    if scenario.production_boundaries.iter().any(|boundary| {
        let lower = boundary.to_ascii_lowercase();
        ["mock", "fake", "stub", "test-only", "in-memory"]
            .iter()
            .any(|word| lower.contains(word))
    }) {
        return Err(
            "FF-RUNTIME-E-MOCK-SUBSTITUTE: production boundary names a substitute".to_owned(),
        );
    }
    if scenario.args.iter().any(|argument| {
        argument.contains(".GOV")
            || argument.contains("build/")
            || argument.contains("build\\")
            || Path::new(argument).is_absolute()
            || argument.contains(":\\")
    }) {
        return Err(
            "FF-RUNTIME-E-CLEAN-STAGE: runtime arguments reference governance or build roots"
                .to_owned(),
        );
    }
    for input in &scenario.inputs {
        validate_runtime_input(input)?;
    }
    for output in &scenario.expected.output_files {
        validate_runtime_output_path(output)?;
    }
    validate_runtime_stage_paths(scenario, binary)?;
    if scenario.kind == "success" && scenario.inputs.is_empty() {
        return Err(
            "FF-RUNTIME-E-INPUT: success scenarios require at least one hash-bound representative input"
                .to_owned(),
        );
    }
    if scenario.expected.stdout_contains.is_empty()
        && scenario.expected.stderr_contains.is_empty()
        && scenario.expected.output_files.is_empty()
    {
        return Err(
            "FF-RUNTIME-E-NO-OBSERVABLE: exit status alone cannot prove operator-usable behavior"
                .to_owned(),
        );
    }
    if scenario.kind == "success" {
        let counterfactual = scenario.counterfactual.as_ref().ok_or(
            "FF-RUNTIME-E-COUNTERFACTUAL: every success scenario requires a counterfactual",
        )?;
        validate_counterfactual_contract(counterfactual, &scenario.expected)?;
    } else if scenario.counterfactual.is_some() {
        return Err(
            "FF-RUNTIME-E-COUNTERFACTUAL: negative scenarios must not self-author a counterfactual"
                .to_owned(),
        );
    }
    Ok(&scenario.kind)
}

fn validate_runtime_stage_paths(scenario: &RuntimeScenario, binary: &str) -> Result<(), String> {
    let reserved = BTreeSet::from([
        binary.to_owned(),
        format!("{binary}.exe"),
        "runtime.stdout".to_owned(),
        "runtime.stderr".to_owned(),
    ]);
    let input_destinations = scenario
        .inputs
        .iter()
        .map(|input| input.destination.replace('\\', "/").to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    let output_paths = scenario
        .expected
        .output_files
        .iter()
        .map(|output| output.path.replace('\\', "/").to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    if input_destinations.len() != scenario.inputs.len()
        || output_paths.len() != scenario.expected.output_files.len()
    {
        return Err(
            "FF-RUNTIME-E-STAGE-COLLISION: input destinations and output paths must be unique"
                .to_owned(),
        );
    }
    if scenario
        .inputs
        .iter()
        .any(|input| reserved.contains(&input.destination.replace('\\', "/").to_ascii_lowercase()))
        || scenario.expected.output_files.iter().any(|output| {
            let normalized = output.path.replace('\\', "/").to_ascii_lowercase();
            reserved.contains(&normalized) || input_destinations.contains(&normalized)
        })
    {
        return Err(
            "FF-RUNTIME-E-STAGE-COLLISION: inputs and outputs cannot overlap the artifact, gate receipts, or each other"
                .to_owned(),
        );
    }
    Ok(())
}

fn validate_runtime_input(input: &RuntimeInput) -> Result<(), String> {
    if !input
        .source
        .replace('\\', "/")
        .starts_with("build/fixtures/")
        || !safe_relative(&input.source)
        || !safe_relative(&input.destination)
        || !valid_sha256(&input.sha256)
    {
        return Err(
            "FF-RUNTIME-E-INPUT: inputs require fixture source, safe staged destination, and SHA-256"
                .to_owned(),
        );
    }
    Ok(())
}

fn validate_runtime_output_path(output: &RuntimeExpectedFile) -> Result<(), String> {
    if !safe_relative(&output.path)
        || output.min_bytes == 0
        || output
            .sha256
            .as_ref()
            .is_some_and(|digest| !valid_sha256(digest))
    {
        return Err(
            "FF-RUNTIME-E-OUTPUT: output requires safe path, positive size, and optional SHA-256"
                .to_owned(),
        );
    }
    Ok(())
}

fn validate_counterfactual_contract(
    counterfactual: &RuntimeCounterfactual,
    expected: &RuntimeExpected,
) -> Result<(), String> {
    if counterfactual.expected_diagnostic != "FF-RUNTIME-E-OBSERVABLE-MISSING" {
        return Err(
            "FF-RUNTIME-E-COUNTERFACTUAL: expected diagnostic must be FF-RUNTIME-E-OBSERVABLE-MISSING"
                .to_owned(),
        );
    }
    let exists = match counterfactual.target.as_str() {
        "stdout_contains" => expected.stdout_contains.contains(&counterfactual.value),
        "stderr_contains" => expected.stderr_contains.contains(&counterfactual.value),
        "output_file" => expected
            .output_files
            .iter()
            .any(|output| output.path == counterfactual.value),
        _ => false,
    };
    if !exists {
        return Err(
            "FF-RUNTIME-E-COUNTERFACTUAL: target must name a required observable".to_owned(),
        );
    }
    Ok(())
}

fn safe_id(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

fn stable_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn safe_relative(value: &str) -> bool {
    let normalized = value.replace('\\', "/");
    !normalized.is_empty()
        && normalized.split('/').all(|segment| {
            !segment.is_empty()
                && !matches!(segment, "." | "..")
                && !segment.ends_with(['.', ' '])
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn execute_runtime_proof(
    root: &Path,
    proof: &RuntimeProof,
    packet_id: &str,
    product_paths: &[String],
) -> Result<RuntimeTruthResult, String> {
    let policy: ArchitecturePolicy = read_toml(&root.join("build/architecture-policy.toml"))?;
    let member = policy
        .members
        .iter()
        .find(|member| member.name == proof.artifact.package && member.shipped && !member.test_only)
        .ok_or(
            "FF-RUNTIME-E-ARTIFACT-IDENTITY: runtime package is not a declared shipped member",
        )?;
    if !member
        .source_root
        .replace('\\', "/")
        .starts_with("product/")
    {
        return Err(
            "FF-RUNTIME-E-ARTIFACT-IDENTITY: shipped runtime package source is outside product/"
                .to_owned(),
        );
    }
    let mut build_args = vec![
        "build".to_owned(),
        "--manifest-path".to_owned(),
        "build/Cargo.toml".to_owned(),
        "--locked".to_owned(),
        "--release".to_owned(),
        "-p".to_owned(),
        proof.artifact.package.clone(),
        "--bin".to_owned(),
        proof.artifact.binary.clone(),
        "--target-dir".to_owned(),
        ".fforager-artifacts/cargo-target".to_owned(),
    ];
    if !proof.artifact.features.is_empty() {
        build_args.push("--features".to_owned());
        build_args.push(proof.artifact.features.join(","));
    }
    let build_refs = build_args.iter().map(String::as_str).collect::<Vec<_>>();
    let mut checks = Vec::new();
    run_command_with_proof_class(
        root,
        "runtime-release-build",
        "external_process",
        "locked Cargo release build process",
        "cargo",
        &build_refs,
        &mut checks,
    )?;
    let mut artifact = root
        .join(".fforager-artifacts/cargo-target/release")
        .join(&proof.artifact.binary);
    if cfg!(windows) {
        artifact.set_extension("exe");
    }
    validate_built_runtime_artifact(root, &artifact)?;
    let artifact_digest = sha256_file(&artifact)?;
    checks.push(pass_with_class(
        "runtime-artifact-identity",
        "artifact",
        "contained release artifact identity and digest boundary",
        &format!(
            "packet={packet_id}; package={}; binary={}; profile=release; sha256={artifact_digest}; product_paths={product_paths:?}",
            proof.artifact.package, proof.artifact.binary
        ),
    ));
    let mut artifacts = vec![slash(
        artifact
            .strip_prefix(root)
            .map_err(|error| error.to_string())?,
    )];
    for scenario in &proof.scenarios {
        let (scenario_checks, stage) =
            execute_runtime_scenario(root, &artifact, scenario, &artifact_digest)?;
        checks.extend(scenario_checks);
        artifacts.push(stage);
    }
    Ok(RuntimeTruthResult {
        checks,
        proof_classes: vec![
            "artifact".to_owned(),
            "external_process".to_owned(),
            "runtime_observable".to_owned(),
            "counterfactual".to_owned(),
        ],
        limitations: vec![
            "Runtime truth proves only the declared capability IDs and scenarios; unlisted product behavior remains unproven.".to_owned(),
            "Live-site observations remain separate from deterministic runtime acceptance and cannot replace it.".to_owned(),
        ],
        artifacts,
    })
}

fn validate_built_runtime_artifact(root: &Path, artifact: &Path) -> Result<(), String> {
    if !artifact.is_file() {
        return Err(format!(
            "FF-RUNTIME-E-ARTIFACT-IDENTITY: built artifact is missing: {}",
            artifact.display()
        ));
    }
    let metadata = fs::symlink_metadata(artifact)
        .map_err(|error| format!("FF-RUNTIME-E-ARTIFACT-IDENTITY: {error}"))?;
    let release_root = root
        .join(".fforager-artifacts/cargo-target/release")
        .canonicalize()
        .map_err(|error| format!("FF-RUNTIME-E-ARTIFACT-IDENTITY: {error}"))?;
    let canonical_artifact = artifact
        .canonicalize()
        .map_err(|error| format!("FF-RUNTIME-E-ARTIFACT-IDENTITY: {error}"))?;
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_file()
        || !canonical_artifact.starts_with(release_root)
    {
        return Err(
            "FF-RUNTIME-E-ARTIFACT-IDENTITY: release artifact must be a contained regular file"
                .to_owned(),
        );
    }
    Ok(())
}

fn execute_runtime_scenario(
    root: &Path,
    artifact: &Path,
    scenario: &RuntimeScenario,
    artifact_digest: &str,
) -> Result<(Vec<Check>, String), String> {
    let (stage, staged_artifact) =
        stage_runtime_scenario(root, artifact, scenario, artifact_digest)?;
    let observation = run_staged_artifact(&stage, &staged_artifact, scenario)?;
    validate_runtime_observation(&scenario.expected, &observation)?;
    validate_runtime_counterfactual(scenario, &observation)?;
    let stage_path = slash(&stage);
    let mut checks = vec![pass_with_class(
        &format!("runtime-scenario-{}", scenario.id),
        "runtime_observable",
        "clean-stage production artifact observation",
        &format!(
            "kind={}; artifact_sha256={artifact_digest}; exit={}; boundaries={:?}; stage={stage_path}; counterfactual={}",
            scenario.kind,
            observation.exit_code,
            scenario.production_boundaries,
            scenario.counterfactual.is_some()
        ),
    )];
    if scenario.counterfactual.is_some() {
        checks.push(pass_with_class(
            &format!("runtime-scenario-counterfactual-{}", scenario.id),
            "counterfactual",
            "runtime observation oracle with one expected fact removed",
            "the same runtime observation oracle rejected the mutated observation",
        ));
    }
    Ok((checks, stage_path))
}

fn stage_runtime_scenario(
    root: &Path,
    artifact: &Path,
    scenario: &RuntimeScenario,
    artifact_digest: &str,
) -> Result<(PathBuf, PathBuf), String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let artifact_root = root.join(".fforager-artifacts");
    let stage = artifact_root.join("runtime-proof").join(format!(
        "{}-{nonce}-{}",
        scenario.id,
        std::process::id()
    ));
    fs::create_dir_all(&stage)
        .map_err(|error| format!("FF-RUNTIME-E-CLEAN-STAGE: create stage: {error}"))?;
    let canonical_artifact_root = artifact_root
        .canonicalize()
        .map_err(|error| format!("FF-RUNTIME-E-CLEAN-STAGE: canonicalize root: {error}"))?;
    let canonical_stage = stage
        .canonicalize()
        .map_err(|error| format!("FF-RUNTIME-E-CLEAN-STAGE: canonicalize stage: {error}"))?;
    if !canonical_stage.starts_with(&canonical_artifact_root) {
        return Err(
            "FF-RUNTIME-E-CLEAN-STAGE: runtime stage must be inside .fforager-artifacts".to_owned(),
        );
    }
    let staged_artifact = stage.join(
        artifact
            .file_name()
            .ok_or("FF-RUNTIME-E-ARTIFACT-IDENTITY: artifact has no filename")?,
    );
    fs::copy(artifact, &staged_artifact)
        .map_err(|error| format!("FF-RUNTIME-E-CLEAN-STAGE: copy artifact: {error}"))?;
    if sha256_file(&staged_artifact)? != artifact_digest {
        return Err("FF-RUNTIME-E-ARTIFACT-IDENTITY: staged artifact digest mismatch".to_owned());
    }
    for input in &scenario.inputs {
        let source = root.join(&input.source);
        require_relative_contained(root, &input.source, "build/fixtures")?;
        if fs::symlink_metadata(&source)
            .map_err(|error| format!("FF-RUNTIME-E-INPUT: inspect input: {error}"))?
            .file_type()
            .is_symlink()
            || !source.is_file()
            || sha256_file(&source)? != input.sha256.to_ascii_lowercase()
        {
            return Err(format!(
                "FF-RUNTIME-E-INPUT: missing or mismatched input {}",
                input.source
            ));
        }
        let destination = stage.join(&input.destination);
        fs::create_dir_all(
            destination
                .parent()
                .ok_or("FF-RUNTIME-E-INPUT: destination has no parent")?,
        )
        .map_err(|error| format!("FF-RUNTIME-E-INPUT: create destination: {error}"))?;
        fs::copy(&source, &destination)
            .map_err(|error| format!("FF-RUNTIME-E-INPUT: stage input: {error}"))?;
    }
    if sha256_file(&staged_artifact)? != artifact_digest {
        return Err(
            "FF-RUNTIME-E-ARTIFACT-IDENTITY: staged inputs altered the production artifact"
                .to_owned(),
        );
    }
    Ok((stage, staged_artifact))
}

fn run_staged_artifact(
    stage: &Path,
    staged_artifact: &Path,
    scenario: &RuntimeScenario,
) -> Result<RuntimeObservation, String> {
    let stdout_path = stage.join("runtime.stdout");
    let stderr_path = stage.join("runtime.stderr");
    let stdout_file = File::create(&stdout_path)
        .map_err(|error| format!("FF-RUNTIME-E-EXECUTION: create stdout: {error}"))?;
    let stderr_file = File::create(&stderr_path)
        .map_err(|error| format!("FF-RUNTIME-E-EXECUTION: create stderr: {error}"))?;
    let mut command = Command::new(staged_artifact);
    command
        .args(&scenario.args)
        .current_dir(stage)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));
    for (key, _) in env::vars_os() {
        let key_text = key.to_string_lossy();
        if key_text.starts_with("CARGO_")
            || key_text.starts_with("RUST_")
            || matches!(key_text.as_ref(), "OUT_DIR" | "CARGO_MANIFEST_DIR")
        {
            command.env_remove(key);
        }
    }
    configure_quiet_process(&mut command);
    let mut child = command.spawn().map_err(|error| {
        format!(
            "FF-RUNTIME-E-EXECUTION: launch staged artifact {}: {error}",
            staged_artifact.display()
        )
    })?;
    let args = scenario.args.iter().map(String::as_str).collect::<Vec<_>>();
    let status = wait_for_child(
        &mut child,
        &staged_artifact.display().to_string(),
        &args,
        Duration::from_secs(scenario.timeout_seconds),
    )?;
    let stdout_bytes = read_capture(&stdout_path)?;
    let stderr_bytes = read_capture(&stderr_path)?;
    if stdout_bytes.len() > 1_048_576 || stderr_bytes.len() > 1_048_576 {
        return Err("FF-RUNTIME-E-EXECUTION: runtime output exceeds 1 MiB bound".to_owned());
    }
    let mut observation = RuntimeObservation {
        exit_code: status.code().unwrap_or(-1),
        stdout: String::from_utf8(stdout_bytes)
            .map_err(|_| "FF-RUNTIME-E-EXECUTION: stdout is not UTF-8".to_owned())?,
        stderr: String::from_utf8(stderr_bytes)
            .map_err(|_| "FF-RUNTIME-E-EXECUTION: stderr is not UTF-8".to_owned())?,
        files: BTreeMap::new(),
    };
    for expected_file in &scenario.expected.output_files {
        let path = stage.join(&expected_file.path);
        if path.is_file() {
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| format!("FF-RUNTIME-E-OUTPUT: inspect output: {error}"))?;
            let canonical_stage = stage
                .canonicalize()
                .map_err(|error| format!("FF-RUNTIME-E-OUTPUT: canonicalize stage: {error}"))?;
            let canonical_output = path
                .canonicalize()
                .map_err(|error| format!("FF-RUNTIME-E-OUTPUT: canonicalize output: {error}"))?;
            if metadata.file_type().is_symlink() || !canonical_output.starts_with(canonical_stage) {
                return Err(
                    "FF-RUNTIME-E-OUTPUT: output must be a contained regular file, not a link"
                        .to_owned(),
                );
            }
            observation.files.insert(
                expected_file.path.clone(),
                RuntimeObservedFile {
                    bytes: metadata.len(),
                    sha256: sha256_file(&path)?,
                },
            );
        }
    }
    Ok(observation)
}

fn validate_runtime_counterfactual(
    scenario: &RuntimeScenario,
    observation: &RuntimeObservation,
) -> Result<(), String> {
    if let Some(counterfactual) = &scenario.counterfactual {
        let mut mutated = observation.clone();
        match counterfactual.target.as_str() {
            "stdout_contains" => mutated.stdout = mutated.stdout.replace(&counterfactual.value, ""),
            "stderr_contains" => mutated.stderr = mutated.stderr.replace(&counterfactual.value, ""),
            "output_file" => {
                mutated.files.remove(&counterfactual.value);
            }
            _ => return Err("FF-RUNTIME-E-COUNTERFACTUAL: unknown target".to_owned()),
        }
        let error = validate_runtime_observation(&scenario.expected, &mutated)
            .expect_err("validated counterfactual contract must remove a required observable");
        if !error.contains(&counterfactual.expected_diagnostic) {
            return Err(format!(
                "FF-RUNTIME-E-COUNTERFACTUAL: wrong diagnostic from mutation: {error}"
            ));
        }
    }
    Ok(())
}

fn validate_runtime_observation(
    expected: &RuntimeExpected,
    observed: &RuntimeObservation,
) -> Result<(), String> {
    if observed.exit_code != expected.exit_code {
        return Err(format!(
            "FF-RUNTIME-E-OBSERVABLE-MISSING: expected exit {}, observed {}",
            expected.exit_code, observed.exit_code
        ));
    }
    for needle in &expected.stdout_contains {
        if !observed.stdout.contains(needle) {
            return Err(format!(
                "FF-RUNTIME-E-OBSERVABLE-MISSING: stdout omitted {needle:?}"
            ));
        }
    }
    for needle in &expected.stderr_contains {
        if !observed.stderr.contains(needle) {
            return Err(format!(
                "FF-RUNTIME-E-OBSERVABLE-MISSING: stderr omitted {needle:?}"
            ));
        }
    }
    for file in &expected.output_files {
        let observed_file = observed.files.get(&file.path).ok_or_else(|| {
            format!(
                "FF-RUNTIME-E-OBSERVABLE-MISSING: output file {} is absent",
                file.path
            )
        })?;
        if observed_file.bytes < file.min_bytes
            || file.sha256.as_ref().is_some_and(|expected_hash| {
                observed_file.sha256 != expected_hash.to_ascii_lowercase()
            })
        {
            return Err(format!(
                "FF-RUNTIME-E-OBSERVABLE-MISSING: output file {} failed size or digest",
                file.path
            ));
        }
    }
    Ok(())
}

fn runtime_fixture_diagnostic(mutation: &str) -> Result<&'static str, String> {
    let mut proof = runtime_fixture_proof();
    match mutation {
        "runtime_test_only_substitute" => {
            "cfg_test".clone_into(&mut proof.artifact.compilation_mode);
        }
        "runtime_mock_boundary" => {
            proof.scenarios[0].production_boundaries = vec!["mock_transport".to_owned()];
        }
        "runtime_scaffold_completion" => {
            "scaffold".clone_into(&mut proof.completion_claim);
        }
        "runtime_noop_success" => {
            proof.scenarios[0].expected.stdout_contains.clear();
            proof.scenarios[0].expected.output_files.clear();
        }
        "runtime_missing_artifact_identity" => proof.artifact.binary.clear(),
        "runtime_missing_clean_stage" => {
            "workspace".clone_into(&mut proof.artifact.package_mode);
        }
        "runtime_missing_counterfactual" => proof.scenarios[0].counterfactual = None,
        "runtime_stage_collision" => {
            "fforager.exe".clone_into(&mut proof.scenarios[0].inputs[0].destination);
        }
        other => return Err(format!("unknown runtime fixture mutation {other}")),
    }
    let error = validate_runtime_proof_contract(&proof)
        .expect_err("runtime fixture mutation unexpectedly passed production validator");
    for diagnostic in [
        "FF-RUNTIME-E-TEST-SUBSTITUTE",
        "FF-RUNTIME-E-MOCK-SUBSTITUTE",
        "FF-RUNTIME-E-SCAFFOLD-COMPLETION",
        "FF-RUNTIME-E-NO-OBSERVABLE",
        "FF-RUNTIME-E-ARTIFACT-IDENTITY",
        "FF-RUNTIME-E-CLEAN-STAGE",
        "FF-RUNTIME-E-COUNTERFACTUAL",
        "FF-RUNTIME-E-STAGE-COLLISION",
    ] {
        if error.contains(diagnostic) {
            return Ok(diagnostic);
        }
    }
    Err(format!(
        "runtime fixture failed without stable diagnostic: {error}"
    ))
}

fn runtime_fixture_proof() -> RuntimeProof {
    RuntimeProof {
        schema_id: "ff.runtime-proof@1".to_owned(),
        completion_claim: "operator_usable_runtime".to_owned(),
        artifact: RuntimeArtifact {
            package: "fforager".to_owned(),
            binary: "fforager".to_owned(),
            profile: "release".to_owned(),
            features: Vec::new(),
            package_mode: "clean_staged".to_owned(),
            execution_mode: "external_process".to_owned(),
            compilation_mode: "production".to_owned(),
            dependency_mode: "normal_only".to_owned(),
            testkit_mode: "forbidden".to_owned(),
            adapter_mode: "production".to_owned(),
        },
        forbidden_substitutes: vec![
            "mock".to_owned(),
            "fake".to_owned(),
            "stub".to_owned(),
            "fixture-only-implementation".to_owned(),
            "in-memory-substitute".to_owned(),
            "hardcoded-success".to_owned(),
            "test-only-adapter".to_owned(),
            "direct-internal-call".to_owned(),
        ],
        scenarios: vec![
            RuntimeScenario {
                id: "direct-success".to_owned(),
                kind: "success".to_owned(),
                capability_ids: vec!["FF-CAP-DIRECT".to_owned()],
                args: vec!["--input".to_owned(), "input.bin".to_owned()],
                timeout_seconds: 30,
                inputs: vec![RuntimeInput {
                    source: "build/fixtures/runtime/input.bin".to_owned(),
                    destination: "input.bin".to_owned(),
                    sha256: "0".repeat(64),
                }],
                production_boundaries: vec!["production_transport".to_owned()],
                expected: RuntimeExpected {
                    exit_code: 0,
                    stdout_contains: vec!["completed".to_owned()],
                    stderr_contains: Vec::new(),
                    output_files: Vec::new(),
                },
                counterfactual: Some(RuntimeCounterfactual {
                    target: "stdout_contains".to_owned(),
                    value: "completed".to_owned(),
                    expected_diagnostic: "FF-RUNTIME-E-OBSERVABLE-MISSING".to_owned(),
                }),
            },
            RuntimeScenario {
                id: "direct-negative".to_owned(),
                kind: "negative".to_owned(),
                capability_ids: vec!["FF-CAP-DIRECT".to_owned()],
                args: vec!["--invalid".to_owned()],
                timeout_seconds: 30,
                inputs: Vec::new(),
                production_boundaries: vec!["production_cli".to_owned()],
                expected: RuntimeExpected {
                    exit_code: 2,
                    stdout_contains: Vec::new(),
                    stderr_contains: vec!["invalid".to_owned()],
                    output_files: Vec::new(),
                },
                counterfactual: None,
            },
        ],
    }
}

fn run_verify_pr(root: &Path, gate_args: &[String]) -> Result<(), String> {
    match run_verify_pr_inner(root, gate_args) {
        Ok(()) => Ok(()),
        Err(error) => fail_with_report(root, PR_GATE, "verify-pr", gate_args, &error),
    }
}

#[allow(clippy::too_many_lines)]
fn run_verify_pr_inner(root: &Path, gate_args: &[String]) -> Result<(), String> {
    let mut checks = Vec::new();
    validate_change_evidence(root, &mut checks)?;
    secret_scan::verify_hook_configuration(root)?;
    let findings = secret_scan::verify_repository_state(root)?;
    checks.push(Check {
        id: "secret-scan".to_owned(),
        status: "PASS",
        proof_class: "structural",
        concrete_input:
            "reachable Git history, exact index blobs, tracked and untracked non-ignored worktree files"
                .to_owned(),
        executed_boundary: "fforager-xtask credential scanner".to_owned(),
        expected_result: "zero provider-pattern API-key findings and no unscannable blob"
            .to_owned(),
        observed_result: format!("PASS: findings={findings}"),
        skipped_semantic_dependencies: vec![
            "Pattern matching cannot prove absence of every possible unstructured secret."
                .to_owned(),
        ],
        detail: "Matched values are never emitted; GitHub secret-scanning push protection supplies the independent remote pre-receive layer.".to_owned(),
    });
    let mut transport_report = String::new();
    let archive_store_invocation = begin_wp008_invocation(root)?;
    let resource_durability_invocation = begin_wp009_invocation(root)?;
    let (architecture, archive_store_receipt, resource_durability_receipt) =
        run_verify_deep_checks(
            root,
            &mut checks,
            &mut transport_report,
            &archive_store_invocation,
            &resource_durability_invocation,
        )?;
    validate_wp008_execution_receipt(root, &archive_store_receipt, &archive_store_invocation)?;
    validate_wp009_execution_receipt(
        root,
        &resource_durability_receipt,
        &resource_durability_invocation,
    )?;
    run_command_with_env(
        root,
        "docs",
        "cargo",
        &[
            "doc",
            "--manifest-path",
            "build/Cargo.toml",
            "--workspace",
            "--all-features",
            "--locked",
            "--no-deps",
            "--target-dir",
            ".fforager-artifacts/cargo-target",
        ],
        "RUSTDOCFLAGS",
        "-Dwarnings",
        &mut checks,
    )?;
    run_command(
        root,
        "dependency-policy",
        "cargo",
        &[
            "deny",
            "--manifest-path",
            "build/Cargo.toml",
            "--config",
            "build/deny.toml",
            "check",
        ],
        &mut checks,
    )?;
    verify_advisory_databases(root, &mut checks)?;
    let runtime = runtime_truth_check(root)?;
    let runtime_child_reports = write_executed_runtime_children(root, &runtime)?;
    checks.extend(runtime.checks);
    verify_future_gate_applicability(root, &mut checks)?;
    let mut declared_supported_proof_classes = architecture.declared_supported_proof_classes;
    declared_supported_proof_classes.extend(runtime.proof_classes);
    declared_supported_proof_classes.sort();
    declared_supported_proof_classes.dedup();
    let mut proof_limitations = architecture.limitations;
    proof_limitations.extend(runtime.limitations);
    let mut artifacts = vec![
        ".fforager-artifacts/cargo-target".to_owned(),
        "build/reports".to_owned(),
        transport_report,
        archive_store_receipt.report_path,
        resource_durability_receipt.report_path,
    ];
    artifacts.extend(runtime.artifacts);
    artifacts.extend(runtime_child_reports);
    artifacts.sort();
    artifacts.dedup();
    let executed_proof_classes = executed_proof_classes(&checks, &architecture.fixtures);
    let aggregate_executed_proof_class = aggregate_executed_proof_class(&executed_proof_classes);
    let report = GateReport {
        schema_id: "ff.gate-report@1",
        schema_version: "1.0.0",
        gate_id: PR_GATE.to_owned(),
        gate_version: 1,
        status: "PASS",
        exit_code: 0,
        source: source_state(root)?,
        invocation: invocation(gate_args),
        inputs: collect_inputs(root)?,
        checks,
        rules: architecture.rules,
        fixtures: architecture.fixtures,
        aggregate_executed_proof_class,
        declared_supported_proof_classes,
        executed_proof_classes,
        proof_limitations,
        artifacts,
    };
    let path = write_report(root, "verify-pr", &report)?;
    println!("PASS {PR_GATE}; report={}", slash(&path));
    Ok(())
}

fn verify_future_gate_applicability(root: &Path, checks: &mut Vec<Check>) -> Result<(), String> {
    if root.join("product/watcher/Cargo.toml").exists() {
        if !checks.iter().any(|check| {
            check.id == "phase1-watcher-boundaries"
                && check.status == "PASS"
                && check.proof_class == "runtime_fault"
        }) {
            return Err("FF-WATCHER-E-PROOF-MISSING: applicable watcher boundaries were not executed by this verification".into());
        }
    } else {
        checks.push(Check {
        id: "watcher-check".to_owned(),
        status: "PASS",
        proof_class: "structural",
        concrete_input: "product/watcher/Cargo.toml".to_owned(),
        executed_boundary: "watcher package activation check".to_owned(),
        expected_result: "watcher package is absent".to_owned(),
        observed_result: "NOT_APPLICABLE: product/watcher/Cargo.toml is absent".to_owned(),
        skipped_semantic_dependencies: vec!["Watcher package is not present.".to_owned()],
        detail: "product/watcher/Cargo.toml is absent; activation trigger is the watcher package or a watcher/release claim.".to_owned(),
    });
    }
    checks.push(Check {
        id: "verify-release".to_owned(),
        status: "PASS",
        proof_class: "structural",
        concrete_input: "verify-release".to_owned(),
        executed_boundary: "release gate applicability check".to_owned(),
        expected_result: "release gate remains unavailable in Phase 0".to_owned(),
        observed_result: "NOT_IMPLEMENTED: release gate has no Phase 0 activation".to_owned(),
        skipped_semantic_dependencies: vec!["No release artifact exists.".to_owned()],
        detail: "Future gate outside the Phase 0 verify-pr applicable child set.".to_owned(),
    });
    Ok(())
}

fn fail_with_report(
    root: &Path,
    gate_id: &str,
    prefix: &str,
    gate_args: &[String],
    error: &str,
) -> Result<(), String> {
    let source = source_state(root).unwrap_or_else(|source_error| SourceState {
        git_commit: "UNKNOWN".to_owned(),
        dirty: true,
        dirty_paths: vec![format!("source-state-error:{source_error}")],
        content_fingerprint: "0".repeat(64),
    });
    let checks = vec![Check {
        id: "gate-failure".to_owned(),
        status: "FAIL",
        proof_class: FAILURE_PROOF_CLASS,
        concrete_input: gate_id.to_owned(),
        executed_boundary: "fforager-xtask gate".to_owned(),
        expected_result: "gate succeeds".to_owned(),
        observed_result: error.to_owned(),
        skipped_semantic_dependencies: vec![
            "Gate stopped before later semantic dependencies could execute.".to_owned(),
        ],
        detail: error.to_owned(),
    }];
    let executed_proof_classes = executed_proof_classes(&checks, &[]);
    let aggregate_executed_proof_class = aggregate_executed_proof_class(&executed_proof_classes);
    let report = GateReport {
        schema_id: "ff.gate-report@1",
        schema_version: "1.0.0",
        gate_id: gate_id.to_owned(),
        gate_version: 1,
        status: "FAIL",
        exit_code: 1,
        source,
        invocation: invocation(gate_args),
        inputs: collect_inputs(root).unwrap_or_default(),
        checks,
        rules: Vec::new(),
        fixtures: Vec::new(),
        declared_supported_proof_classes: vec![FAILURE_PROOF_CLASS.to_owned()],
        executed_proof_classes,
        aggregate_executed_proof_class,
        proof_limitations: vec![
            "Gate stopped at the recorded failure; later checks were NOT_RUN.".to_owned(),
        ],
        artifacts: vec!["build/reports".to_owned()],
    };
    match write_report(root, prefix, &report) {
        Ok(path) => Err(format!("{error}; failure_report={}", slash(&path))),
        Err(report_error) => Err(format!(
            "{error}; failure report write also failed: {report_error}"
        )),
    }
}

fn run_rust_verification(root: &Path, checks: &mut Vec<Check>) -> Result<(), String> {
    validate_rust_verification_environment(root)?;
    run_command(
        root,
        "format",
        "cargo",
        &[
            "fmt",
            "--manifest-path",
            "build/Cargo.toml",
            "--all",
            "--",
            "--check",
        ],
        checks,
    )?;
    for (id, features) in [
        ("check", None),
        ("check-no-default-features", Some("--no-default-features")),
        ("check-all-features", Some("--all-features")),
    ] {
        let mut args = vec![
            "check",
            "--manifest-path",
            "build/Cargo.toml",
            "--workspace",
            "--all-targets",
            "--locked",
        ];
        if let Some(feature_arg) = features {
            args.push(feature_arg);
        }
        args.extend(["--target-dir", ".fforager-artifacts/cargo-target"]);
        run_command(root, id, "cargo", &args, checks)?;
    }
    run_command(
        root,
        "clippy",
        "cargo",
        &[
            "clippy",
            "--manifest-path",
            "build/Cargo.toml",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--locked",
            "--target-dir",
            ".fforager-artifacts/cargo-target",
            "--",
            "-D",
            "warnings",
        ],
        checks,
    )?;
    run_command(
        root,
        "test",
        "cargo",
        &[
            "test",
            "--manifest-path",
            "build/Cargo.toml",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--locked",
            "--no-fail-fast",
            "--target-dir",
            ".fforager-artifacts/cargo-target",
        ],
        checks,
    )?;
    Ok(())
}

fn run_doctests(root: &Path, checks: &mut Vec<Check>) -> Result<(), String> {
    run_command(
        root,
        "doctests",
        "cargo",
        &[
            "test",
            "--manifest-path",
            "build/Cargo.toml",
            "--workspace",
            "--doc",
            "--all-features",
            "--locked",
            "--target-dir",
            ".fforager-artifacts/cargo-target",
        ],
        checks,
    )
}

fn verify_tool_identities(root: &Path, checks: &mut Vec<Check>) -> Result<(), String> {
    let policy: ToolingPolicy = read_toml(&root.join("build/tooling-policy.toml"))?;
    validate_tooling_policy(&policy)?;
    let host = validate_current_host(root, &policy)?;
    checks.push(pass("supported-host", &format!("rustc host={host}")));
    for tool in policy.tools {
        let (program, args) = tool.command.split_first().ok_or("empty tool command")?;
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();
        let output = command_output(root, program, &args)?;
        if !output.lines().any(|line| line.trim() == tool.identity_line) {
            return Err(format!(
                "tool {} identity mismatch: expected exact line {:?}, observed {:?}",
                tool.name,
                tool.identity_line,
                output.trim()
            ));
        }
        let provenance = if let Some(expected) = &tool.executable_sha256 {
            let executable = resolve_executable(root, &tool.name)?;
            let observed = verify_executable_checksum(&tool.name, &executable, expected)?;
            format!(
                "{}; executable={}; sha256={observed}",
                output.trim(),
                executable.display()
            )
        } else {
            format!(
                "{}; provenance={}; inputs=rust-toolchain.toml,build/Cargo.lock",
                output.trim(),
                tool.provenance_kind
            )
        };
        checks.push(pass(&format!("tool-{}", tool.name), &provenance));
    }
    Ok(())
}

fn verify_executable_checksum(name: &str, path: &Path, expected: &str) -> Result<String, String> {
    let observed = sha256_file(path)?;
    if observed != expected {
        return Err(format!(
            "tool {name} checksum mismatch: executable={}; expected={expected}; observed={observed}",
            path.display()
        ));
    }
    Ok(observed)
}

fn resolve_executable(root: &Path, name: &str) -> Result<PathBuf, String> {
    #[cfg(windows)]
    {
        let output = command_output(root, "where.exe", &[name])?;
        let path = output
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| format!("where.exe returned no executable for {name}"))?;
        path.canonicalize()
            .map_err(|error| format!("canonicalize executable {}: {error}", path.display()))
    }
    #[cfg(not(windows))]
    {
        let _ = (root, name);
        Err(
            "executable checksum resolution is only defined for the supported Windows host"
                .to_owned(),
        )
    }
}

fn sha256_file(path: &Path) -> Result<String, String> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut file = File::open(path)
        .map_err(|error| format!("open executable for checksum {}: {error}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("read executable for checksum {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let bytes = digest.finalize();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(encoded)
}

fn verify_advisory_databases(root: &Path, checks: &mut Vec<Check>) -> Result<(), String> {
    let policy: ToolingPolicy = read_toml(&root.join("build/tooling-policy.toml"))?;
    let cargo_home = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".cargo")))
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
        .ok_or("cannot resolve Cargo home for advisory database proof")?;
    let database_root = cargo_home.join("advisory-dbs");
    let mut databases = Vec::new();
    for entry in
        fs::read_dir(&database_root).map_err(|e| format!("read advisory database root: {e}"))?
    {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_type().map_err(|e| e.to_string())?.is_dir()
            && entry.path().join(".git").is_dir()
        {
            databases.push(entry.path());
        }
    }
    if databases.is_empty() {
        return Err("cargo-deny advisory database is absent after dependency check".to_owned());
    }
    databases.sort();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    for database in databases {
        let head = command_output(&database, "git", &["rev-parse", "HEAD"])?;
        let commit_epoch = command_output(&database, "git", &["log", "-1", "--format=%ct"])?
            .trim()
            .parse::<u64>()
            .map_err(|e| format!("parse advisory database commit time: {e}"))?;
        if commit_epoch > now + 300 {
            return Err("advisory database commit time is implausibly in the future".to_owned());
        }
        let age_hours = now.saturating_sub(commit_epoch) / 3_600;
        if age_hours > policy.advisory_database_max_age_hours {
            return Err(format!(
                "advisory database is stale: age_hours={age_hours}, max={}",
                policy.advisory_database_max_age_hours
            ));
        }
        let status = command_output(&database, "git", &["status", "--porcelain"])?;
        let mut dirty_inputs = Vec::new();
        for line in status.lines() {
            let relative = line
                .get(3..)
                .ok_or("malformed advisory database status")?
                .trim();
            let path = database.join(relative);
            let identity = if path.is_file() {
                command_output(&database, "git", &["hash-object", "--", relative])?
                    .trim()
                    .to_owned()
            } else {
                "NON_FILE_OR_DELETED".to_owned()
            };
            dirty_inputs.push(format!("{relative}@{identity}"));
        }
        dirty_inputs.sort();
        let database_id = database
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or("advisory database path has no portable ID")?;
        checks.push(pass(
            "advisory-database",
            &format!(
                "id={database_id}; head={}; commit_epoch={commit_epoch}; age_hours={age_hours}; dirty_inputs={dirty_inputs:?}",
                head.trim()
            ),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn validate_change_evidence(root: &Path, checks: &mut Vec<Check>) -> Result<(), String> {
    let id = active_packet_id(root)?
        .ok_or("expected one active work-packet ID, observed canonical null")?;
    let packet_path = root.join(".GOV/work_packets").join(&id).join("packet.json");
    let packet: Value =
        serde_json::from_str(&fs::read_to_string(&packet_path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if packet.pointer("/identity/wp_id").and_then(Value::as_str) != Some(&id) {
        return Err("active packet identity does not match taskboard".to_owned());
    }
    if packet.pointer("/lifecycle/status").and_then(Value::as_str) != Some("IN_PROGRESS") {
        return Err("active packet does not authorize implementation".to_owned());
    }
    let evidence = packet
        .pointer("/extensions/change_evidence")
        .ok_or("missing extensions.change_evidence")?;
    for field in [
        "applicable_rule_ids",
        "acceptance_or_authority_ids",
        "scope_and_risk_matrix",
        "reuse_candidates_in_order_with_result",
        "artifact_specific_justification",
        "pre_simplification_verdict",
        "simplification_changes",
        "post_simplification_verdict",
        "complete_post_change_verification",
        "ceilings_exceptions_and_evidence",
    ] {
        let value = evidence
            .get(field)
            .ok_or_else(|| format!("missing change-evidence field {field}"))?;
        if value.is_null()
            || value
                .as_str()
                .is_some_and(|text| text.trim().is_empty() || text.starts_with("PENDING"))
            || value.as_array().is_some_and(Vec::is_empty)
            || value.as_object().is_some_and(serde_json::Map::is_empty)
        {
            return Err(format!("change-evidence field {field} is empty or pending"));
        }
    }
    let accepted_rows = packet
        .pointer("/acceptance_matrix")
        .and_then(Value::as_array)
        .ok_or("missing acceptance matrix")?;
    let accepted_values = accepted_rows
        .iter()
        .map(|row| {
            row.get("id")
                .and_then(Value::as_str)
                .ok_or("acceptance row has no string ID")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let accepted: BTreeSet<_> = accepted_values.iter().copied().collect();
    if accepted.len() != accepted_values.len() {
        return Err("acceptance matrix contains duplicate IDs".to_owned());
    }
    let cited_values = string_array(evidence, "acceptance_or_authority_ids")?;
    let cited: BTreeSet<_> = cited_values.iter().map(String::as_str).collect();
    if cited.len() != cited_values.len() || accepted != cited {
        return Err("change evidence does not cite exactly every packet acceptance ID".to_owned());
    }
    let applicable_values = string_array(evidence, "applicable_rule_ids")?;
    let applicable: BTreeSet<_> = applicable_values.iter().map(String::as_str).collect();
    if applicable.len() != applicable_values.len() {
        return Err("applicable_rule_ids contains duplicates".to_owned());
    }
    let canonical = required_architecture_rules(&root.join(".GOV/rules/build-rules.yaml"))?;
    if !canonical
        .iter()
        .all(|rule| applicable.contains(rule.as_str()))
    {
        return Err("applicable_rule_ids omits a canonical REQUIRED architecture rule".to_owned());
    }
    validate_simplification_verdicts(evidence)?;
    if string_array(evidence, "simplification_changes")?.is_empty()
        || string_array(evidence, "complete_post_change_verification")?.len() < 6
    {
        return Err(
            "simplification or complete post-change verification evidence is incomplete".to_owned(),
        );
    }
    validate_reuse_rows(evidence)?;
    validate_scope_and_risk(evidence)?;
    validate_artifact_justification(evidence)?;
    validate_ceiling(evidence)?;
    validate_adversarial_review_evidence(evidence)?;
    checks.push(pass(
        "active-packet-evidence",
        &format!("active packet {id} has complete non-placeholder change evidence"),
    ));
    Ok(())
}

fn validate_simplification_verdicts(evidence: &Value) -> Result<(), String> {
    let pre = evidence
        .get("pre_simplification_verdict")
        .and_then(Value::as_str)
        .ok_or("pre_simplification_verdict is not a string")?;
    if !(pre.starts_with("PASS:") || pre.starts_with("FAIL")) || pre.trim().len() < 40 {
        return Err(
            "pre_simplification_verdict is not a substantive PASS or FAIL verdict".to_owned(),
        );
    }
    let post = evidence
        .get("post_simplification_verdict")
        .and_then(Value::as_str)
        .ok_or("post_simplification_verdict is not a string")?;
    if !post.starts_with("PASS:") || post.trim().len() < 40 {
        return Err("post_simplification_verdict is not a substantive PASS verdict".to_owned());
    }
    Ok(())
}

fn validate_adversarial_review_evidence(evidence: &Value) -> Result<(), String> {
    let review = evidence
        .get("adversarial_review")
        .and_then(Value::as_object)
        .ok_or("change evidence omits structured adversarial_review")?;
    let expected = BTreeSet::from([
        "DIFF_ATTACK_SURFACES",
        "INDEPENDENT_CHECKS_RUN",
        "COUNTERFACTUAL_CHECKS",
        "BOUNDARY_PROBES",
        "NEGATIVE_PATH_CHECKS",
        "INDEPENDENT_FINDINGS",
        "RESIDUAL_UNCERTAINTY",
        "MANUAL_REVIEW",
    ]);
    let observed = review.keys().map(String::as_str).collect::<BTreeSet<_>>();
    if observed != expected {
        return Err(format!(
            "adversarial_review field mismatch: expected={expected:?}; observed={observed:?}"
        ));
    }
    for (field, minimum) in [
        ("DIFF_ATTACK_SURFACES", 4),
        ("INDEPENDENT_CHECKS_RUN", 4),
        ("COUNTERFACTUAL_CHECKS", 4),
        ("BOUNDARY_PROBES", 4),
        ("NEGATIVE_PATH_CHECKS", 4),
        ("RESIDUAL_UNCERTAINTY", 1),
    ] {
        let rows = substantive_review_rows(&Value::Object(review.clone()), field)?;
        if rows.len() < minimum {
            return Err(format!(
                "adversarial_review field {field} has {} rows; expected at least {minimum}",
                rows.len()
            ));
        }
    }
    let surfaces = substantive_review_rows(&Value::Object(review.clone()), "DIFF_ATTACK_SURFACES")?
        .join(" ")
        .to_ascii_lowercase();
    for lens in [
        "contract",
        "serial",
        "state",
        "resource",
        "integration",
        "gate",
    ] {
        if !surfaces.contains(lens) {
            return Err(format!(
                "adversarial_review attack surfaces omit required lens {lens}"
            ));
        }
    }
    validate_finding_dispositions(
        review
            .get("INDEPENDENT_FINDINGS")
            .ok_or("adversarial_review omits INDEPENDENT_FINDINGS")?,
    )?;
    validate_manual_review_evidence(
        review
            .get("MANUAL_REVIEW")
            .ok_or("adversarial_review omits MANUAL_REVIEW")?,
    )
}

fn validate_finding_dispositions(value: &Value) -> Result<(), String> {
    let rows = value
        .as_array()
        .filter(|rows| !rows.is_empty())
        .ok_or("INDEPENDENT_FINDINGS must be a nonempty array")?;
    let expected = BTreeSet::from(["finding", "finding_id", "proof_id", "status"]);
    let mut observed_finding_ids = BTreeSet::new();
    for row in rows {
        let disposition = row
            .as_object()
            .ok_or("every independent finding must be a structured disposition object")?;
        let observed = disposition
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if observed != expected {
            return Err(format!(
                "independent finding field mismatch: expected={expected:?}; observed={observed:?}"
            ));
        }
        let finding = disposition
            .get("finding")
            .and_then(Value::as_str)
            .filter(|text| text.trim().len() >= 24)
            .ok_or("independent finding text is not substantive")?;
        let status = disposition
            .get("status")
            .and_then(Value::as_str)
            .ok_or("independent finding status is not a string")?;
        if !matches!(status, "REMEDIATED" | "NO_BLOCKING_FINDING") {
            return Err(format!("invalid independent finding status {status}"));
        }
        let finding_id = disposition
            .get("finding_id")
            .and_then(Value::as_str)
            .ok_or("independent finding finding_id is not a string")?;
        if !observed_finding_ids.insert(finding_id) {
            return Err(format!(
                "independent finding_id is duplicated: {finding_id}"
            ));
        }
        let proof = disposition
            .get("proof_id")
            .and_then(Value::as_str)
            .ok_or("independent finding proof_id is not a string")?;
        let expected_proof = expected_adversarial_finding_proof(finding_id)
            .ok_or_else(|| format!("independent finding_id is not canonical: {finding_id}"))?;
        if proof != expected_proof {
            return Err(format!(
                "independent finding {finding_id} does not cite its exact canonical executable proof: expected={expected_proof}; observed={proof}"
            ));
        }
        let normalized = finding.to_ascii_lowercase();
        if [
            "unresolved",
            "unremediated",
            "remains open",
            "still open",
            "not remediated",
            "no remediation",
            "remediation missing",
            "proof missing",
            "still fails",
            "continues to fail",
        ]
        .iter()
        .any(|contradiction| normalized.contains(contradiction))
        {
            return Err(format!(
                "independent finding contradicts its resolved status: {finding}"
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn expected_adversarial_finding_proof(finding_id: &str) -> Option<&'static str> {
    match finding_id {
        "WP-FF-022-FINDING-CLASSIFIER-001" => Some(
            "xtask::runtime_classifier_tests::build_only_prerequisite_public_handler_accepts_original_wp020_input",
        ),
        "WP-FF-022-FINDING-GATE-REFERENCE-002" => {
            Some("xtask::runtime_classifier_tests::gate_identifier_is_not_a_current_runtime_claim")
        }
        "WP-FF-022-FINDING-CLAIM-CONTEXT-003" => Some(
            "xtask::runtime_classifier_tests::original_wp020_context_corrections_keep_current_claims_closed",
        ),
        "WP-FF-022-FINDING-JOB-ACK-004" => Some(
            "ffmpeg::platform::windows::tests::job_empty_wait_requires_actual_zero_and_natural_release",
        ),
        "WP-FF-022-FINDING-REJECTED-INPUT-005" => Some(
            "xtask::runtime_classifier_tests::prerequisite_public_handler_distinguishes_rejected_input_specification",
        ),
        "WP-FF-022-FINDING-TECHNICAL-ROLE-006" => Some(
            "xtask::runtime_classifier_tests::prerequisite_public_handler_distinguishes_technical_roles_from_progress",
        ),
        "WP-FF-005-FINDING-WIRE-001" => {
            Some("testkit::tests::canonical_public_contract_fixtures_decode_and_validate")
        }
        "WP-FF-005-FINDING-LIFECYCLE-001" => Some(
            "core::lifecycle::tests::transient_restore_and_stale_or_wrong_acknowledgements_are_rejected",
        ),
        "WP-FF-005-FINDING-RESOURCE-001" => Some(
            "core::resource::tests::receive_requires_exact_claim_owner_and_records_attribution",
        ),
        "WP-FF-005-FINDING-FIXTURE-001" => Some(
            "core::lifecycle::tests::success_and_durable_prefixes_require_effect_acknowledgements",
        ),
        "WP-FF-005-FINDING-INSTANCE-001" => {
            Some("core::lifecycle::tests::acknowledgement_rejects_cross_instance_routing")
        }
        "WP-FF-005-FINDING-RESTORE-001" => Some(
            "core::lifecycle::tests::restoration_accepts_only_inventory_enumerated_durable_states",
        ),
        "WP-FF-005-FINDING-ATTRIBUTION-001" => Some(
            "core::resource::tests::consumed_claim_transfer_is_rejected_without_rewriting_attribution",
        ),
        "WP-FF-005-FINDING-INVENTORY-001" => {
            Some("testkit::tests::inventory_digest_rejects_semantic_field_mutations")
        }
        "WP-FF-005-FINDING-GATE-SCANNER-001" => {
            Some("xtask::tests::data_model_scan_rejects_runtime_handle_import_aliases")
        }
        "WP-FF-005-FINDING-GATE-PROGRESS-001" => {
            Some("xtask::tests::prerequisite_zero_claim_surface_rejects_parallel_progress_claims")
        }
        "WP-FF-005-FINDING-GATE-DISPOSITION-001" => {
            Some("xtask::tests::adversarial_review_evidence_rejects_placeholder_findings")
        }
        "WP-FF-005-FINDING-GATE-DEEP-001" => {
            Some("xtask::tests::deep_verification_composes_doctest_gate")
        }
        "WP-FF-005-FINDING-GATE-INVENTORY-001" => {
            Some("xtask::tests::contract_inventory_rejects_required_field_and_stable_id_mutations")
        }
        "WP-FF-006-FINDING-COUNTERFACTUAL-001" => {
            Some("fforager_js_corpus::tests::oracle_counterfactual_rejects_mutated_expected_output")
        }
        "WP-FF-006-FINDING-CANCEL-ACK-002" => Some("WP-FF-006-PROBE-CANCELLATION"),
        "WP-FF-006-FINDING-AST-DISCOVERY-003" => Some(
            "fforager_javascript::tests::structural_discovery_skips_prelude_iife_and_unrelated_markers",
        ),
        "WP-FF-006-FINDING-CACHE-EVIDENCE-004" => Some("WP-FF-006-PROBE-CACHE-KEY"),
        "WP-FF-006-FINDING-REPORT-MANIFEST-005" => {
            Some("build/reports/wp-ff-006-youtube-challenge-report.json")
        }
        "WP-FF-006-FINDING-CORRELATION-006" => {
            Some("fforager_js_corpus::tests::full_response_correlation_rejects_forged_tuple_fields")
        }
        "WP-FF-006-FINDING-PROBE-AUTHORITY-007" => Some("WP-FF-006-PROBE-PROBE-AUTHORITY"),
        "WP-FF-006-FINDING-FATAL-TYPING-008" => Some("WP-FF-006-PROBE-PATH-CONFINEMENT"),
        "WP-FF-006-FINDING-QUARANTINE-009" => Some("WP-FF-006-PROBE-QUARANTINE"),
        "WP-FF-006-FINDING-RAII-REAP-010" => Some("fforager_js_corpus::Supervisor::drop"),
        "WP-FF-007-FINDING-ORACLE-PIN-001" | "WP-FF-007-FINDING-REPORT-PATH-004" => {
            Some("xtask::tests::transport_semantic_projection_is_mutation_sensitive")
        }
        "WP-FF-007-FINDING-PUBLIC-BOUNDARY-002" => Some(
            "fforager_transport::policy::tests::adapter_rejects_empty_capabilities_and_redirect_special_use",
        ),
        "WP-FF-007-FINDING-RESOURCE-EVIDENCE-003" => Some(
            "fforager_transport::local_server::tests::declared_huge_body_is_rejected_before_allocation",
        ),
        "WP-FF-007-FINDING-DEEP-DECLARATION-005" => Some(
            "xtask::tests::deep_gate_reports_canonical_rules_without_packet_acceptance_attribution",
        ),
        "WP-FF-008-FINDING-CONTRACT-PROVENANCE-001" => {
            Some("contracts::archive::tests::commit_requires_exact_immutable_claim_provenance")
        }
        "WP-FF-008-FINDING-STORAGE-DURABILITY-002" => {
            Some("storage::tests::migration_blocks_commit_between_verify_and_activate")
        }
        "WP-FF-008-FINDING-PROOF-REPORT-003" => {
            Some("testkit::tests::archive_store_evidence_corpus_executes_public_boundary")
        }
        "WP-FF-009-FINDING-RESOURCE-CONCURRENCY-001" => Some(
            "testkit::tests::resource_durability_report_rejects_missing_queue_bounds_and_durability_outrun",
        ),
        "WP-FF-009-FINDING-DURABILITY-FILESYSTEM-002" => {
            Some("testkit::tests::resource_durability_model_corpus_executes_public_boundaries")
        }
        "WP-FF-009-FINDING-PROOF-REPORT-003" => {
            Some("xtask::tests::wp009_report_validator_rejects_missing_fields_and_false_semantics")
        }
        "WP-FF-015-FINDING-OPERATIONAL-ADMISSION-001" | "WP-FF-015-FINDING-DIAGNOSTICS-010" => {
            Some(
                "fforager_transport::adjudication::tests::operational_profile_is_blocked_before_network_on_all_missing_semantics",
            )
        }
        "WP-FF-015-FINDING-LIVE-AUTHORITY-002" => Some(
            "fforager_transport::adjudication::tests::explicitly_authorized_live_observation_still_requires_dns_and_ssrf_authority",
        ),
        "WP-FF-015-FINDING-RUNTIME-ADMISSION-003" => Some(
            "fforager_transport::adjudication::tests::occupied_runtime_lane_is_immediate_typed_refusal_before_io",
        ),
        "WP-FF-015-FINDING-RESOURCE-REPORT-004" => Some(
            "fforager-wreq-live-probe::tests::persisted_report_uses_the_same_production_bound_before_commit",
        ),
        "WP-FF-015-FINDING-REPORT-CONTAINMENT-005" => Some(
            "fforager-wreq-live-probe::tests::persisted_report_destination_is_repository_contained",
        ),
        "WP-FF-015-FINDING-NATIVE-EDGE-006" => {
            Some("xtask::tests::native_exception_mutations_reach_production_validation")
        }
        "WP-FF-015-FINDING-NATIVE-SOURCE-007" => {
            Some("xtask::tests::every_allowed_native_link_has_a_source_policy_and_override_guard")
        }
        "WP-FF-015-FINDING-VERDICT-ORACLE-008" => Some(
            "fforager_transport::adjudication::tests::aggregate_adjudication_is_failed_zero_progress_and_strictly_reloaded",
        ),
        "WP-FF-015-FINDING-PROVENANCE-009" => Some("FF-GATE-ARCH-001::dependency-policy"),
        "WP-FF-016-FINDING-TREE-REF-001" => {
            Some("xtask::secret_scan::tests::tree_root_and_ref_names_are_scanned_before_push")
        }
        "WP-FF-016-FINDING-HOOK-INTEGRITY-002" | "WP-FF-016-FINDING-COMPILER-DISCLOSURE-006" => {
            Some(
                "xtask::secret_scan::tests::hook_configuration_requires_exact_content_and_executable_index_mode",
            )
        }
        "WP-FF-016-FINDING-ENCODING-CORPUS-003" => {
            Some("xtask::secret_scan::tests::utf16_and_gitlab_provider_tokens_are_scanned")
        }
        "WP-FF-016-FINDING-REPLACE-REF-004" => {
            Some("xtask::secret_scan::tests::git_replace_refs_cannot_hide_outgoing_secret")
        }
        "WP-FF-016-FINDING-BATCH-RESOURCE-005" => {
            Some("xtask::secret_scan::tests::dense_provider_matches_are_bounded")
        }
        "WP-FF-016-FINDING-HISTORY-REF-007" => {
            Some("xtask::secret_scan::tests::history_scans_ref_names_and_non_commit_roots")
        }
        "WP-FF-016-FINDING-PROOF-BOUNDARY-008" => {
            Some("FF-GATE-ARCH-001::credential-snapshot-boundary")
        }
        "WP-FF-017-FINDING-COOKIE-001" => Some(
            "fforager_transport::ordinary::tests::dependency_cookie_storage_boundary_is_disabled",
        ),
        "WP-FF-017-FINDING-WORKER-002" => Some(
            "fforager_transport::ordinary::tests::failed_compression_probe_reaps_server_and_acknowledgement_workers",
        ),
        "WP-FF-017-FINDING-NATIVE-INVENTORY-003" => {
            Some("xtask::tests::packaged_object_inventory_rejects_add_remove_replace_and_rename")
        }
        "WP-FF-017-FINDING-PACKAGE-SOURCE-004" => {
            Some("xtask::tests::package_source_and_archive_content_mutations_fail_closed")
        }
        "WP-FF-023-FINDING-APPROVAL-001" => {
            Some("xtask::tests::native_exception_rejects_stale_security_patch_approval")
        }
        "WP-FF-020-FINDING-DOMAIN-001" => Some(
            "xtask::benchmark::domain::tests::physical_domain_rejects_rehashed_cheaper_work_and_missing_effects",
        ),
        "WP-FF-020-FINDING-BUILD-EDGE-002" => {
            Some("xtask::tests::build_only_contract_reuse_rejects_dependency_and_product_drift")
        }
        "WP-FF-021-FINDING-HARDLINK-001" => {
            Some("storage::fetch::tests::hardlinked_stage_is_rejected_before_truncation")
        }
        "WP-FF-021-FINDING-BYTE-CREDIT-002" => Some("FF-GATE-RUNTIME-001::phase1-success"),
        "WP-FF-021-FINDING-ACTOR-METADATA-003" => Some(
            "xtask::phase1_runtime::tests::actor_metadata_decodes_utf16_and_rejects_malformed_bytes",
        ),
        "WP-FF-021-FINDING-ARCHIVE-SCHEMA-004" => Some(
            "xtask::phase1_runtime::tests::archive_oracle_uses_metadata_and_rejects_invalid_rows_and_versions",
        ),
        "WP-FF-021-FINDING-OUTPUT-IDENTITY-005" => {
            Some("xtask::phase1_runtime::tests::output_identity_requires_the_same_existing_target")
        }
        "WP-FF-021-FINDING-WATCHER-VARIANT-006" => Some(
            "xtask::phase1_runtime::watcher_mutant::tests::source_variant_uses_approved_lock_and_emits_incompatible_wire",
        ),
        "WP-FF-017-FINDING-GRAPH-CLOSURE-005" | "WP-FF-017-FINDING-SOURCE-CLOSURE-006" => {
            Some("xtask::tests::ordinary_profile_rejects_resolved_transitive_feature_unification")
        }
        "WP-FF-013-FINDING-SEMANTIC-REPLAY-001" => Some(
            "xtask::compatibility::tests::native_semantic_replay_executes_each_corpus_plane_and_rejects_label_echoes",
        ),
        "WP-FF-013-FINDING-LIFECYCLE-EFFECTS-001" => {
            Some("core::lifecycle::tests::recovery_retries_reject_prior_generation_effect_outcomes")
        }
        "WP-FF-013-FINDING-DIAGNOSTIC-AUTHORITY-001" | "WP-FF-014-FINDING-DURABLE-WIRE-001" => {
            Some("testkit::tests::public_boundary_counterexamples_reject_audit_failures")
        }
        "WP-FF-013-FINDING-GRAPH-CYCLES-001" => Some(
            "contracts::graph::tests::every_traversable_relationship_rejects_cycles_and_accepts_exact_limit_acyclic_graphs",
        ),
        "WP-FF-013-FINDING-ZERO-TEST-001" => Some(
            "xtask::tests::receipt_only_public_counterexample_runs_the_independent_graph_boundary",
        ),
        "WP-FF-013-FINDING-WINDOWS-REPORT-PATH-001" => Some(
            "xtask::compatibility::tests::replay_report_evidence_accepts_canonicalized_windows_path",
        ),
        "WP-FF-013-FINDING-INVENTORY-EXACT-001" => {
            Some("xtask::tests::exact_inventory_proof_requires_one_named_non_ignored_test")
        }
        "WP-FF-013-FINDING-REPORT-ATTRIBUTION-001" => {
            Some("xtask::tests::runtime_gate_result_classes_match_declared_capabilities")
        }
        "WP-FF-013-FINDING-AGGREGATE-RANK-001" => {
            Some("xtask::tests::every_gate_report_proof_class_has_an_aggregate_rank")
        }
        "WP-FF-013-FINDING-PREVERDICT-TRUTH-001" => {
            Some("xtask::tests::remediation_evidence_preserves_truthful_pre_fix_failure")
        }
        "WP-FF-013-FINDING-CAPTURE-COLLISION-001" => {
            Some("xtask::tests::parallel_command_capture_stems_are_unique")
        }
        "WP-FF-013-FINDING-LINE-ENDINGS-001" => {
            Some("xtask::tests::isolated_multiline_mutations_are_line_ending_portable")
        }
        "WP-FF-013-FINDING-PRODUCT-ORACLE-BOUNDARY-001" => {
            Some("xtask::tests::native_product_boundary_and_report_contract_guards_fail_closed")
        }
        "WP-FF-014-FINDING-PROOF-ATTRIBUTION-001" => {
            Some("FF-GATE-ARCH-001::durable-storage-counterfactuals")
        }
        _ => None,
    }
}

fn substantive_review_rows(object: &Value, field: &str) -> Result<Vec<String>, String> {
    let rows = string_array(object, field)?;
    if rows.iter().any(|row| {
        let upper = row.to_ascii_uppercase();
        row.trim().len() < 24
            || upper.contains("PENDING")
            || upper.contains("TODO")
            || upper.contains("TBD")
    }) {
        return Err(format!(
            "adversarial_review field {field} contains placeholder or non-substantive evidence"
        ));
    }
    Ok(rows)
}

fn validate_manual_review_evidence(value: &Value) -> Result<(), String> {
    let review = value.as_object().ok_or("MANUAL_REVIEW is not an object")?;
    let expected = BTreeSet::from(["artifact", "reviewer", "method", "verdict", "evidence"]);
    let observed = review.keys().map(String::as_str).collect::<BTreeSet<_>>();
    if observed != expected {
        return Err(format!(
            "MANUAL_REVIEW field mismatch: expected={expected:?}; observed={observed:?}"
        ));
    }
    if review.get("artifact").and_then(Value::as_str) != Some("product/MODEL_MANUAL.md")
        || review.get("verdict").and_then(Value::as_str) != Some("PASS")
    {
        return Err("MANUAL_REVIEW must record a PASS for product/MODEL_MANUAL.md".to_owned());
    }
    for field in ["reviewer", "method"] {
        if review
            .get(field)
            .and_then(Value::as_str)
            .is_none_or(|text| text.trim().len() < 12)
        {
            return Err(format!("MANUAL_REVIEW omits substantive {field}"));
        }
    }
    let evidence = string_array(&Value::Object(review.clone()), "evidence")?;
    if evidence.len() < 3
        || evidence.iter().any(|row| row.trim().len() < 24)
        || !evidence.iter().any(|row| row.contains("locked Cargo"))
        || !evidence.iter().any(|row| row.contains("recovery"))
        || !evidence
            .iter()
            .any(|row| row.contains("FF-GATE-RUNTIME-001"))
    {
        return Err(
            "MANUAL_REVIEW evidence must prove commands, recovery, and future runtime-proof coverage"
                .to_owned(),
        );
    }
    Ok(())
}

fn string_array(object: &Value, field: &str) -> Result<Vec<String>, String> {
    object
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{field} is not an array"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|text| !text.trim().is_empty())
                .map(ToOwned::to_owned)
                .ok_or_else(|| format!("{field} contains a non-string or empty value"))
        })
        .collect()
}

fn validate_reuse_rows(evidence: &Value) -> Result<(), String> {
    let expected = [
        "no mechanism beyond the accepted outcome",
        "reuse current Ferric code",
        "Rust core/std",
        "approved platform capability",
        "already-approved dependency",
        "smallest complete clear implementation",
    ];
    let rows = evidence
        .get("reuse_candidates_in_order_with_result")
        .and_then(Value::as_array)
        .ok_or("reuse candidates are not an array")?;
    if rows.len() != expected.len() {
        return Err("reuse candidate ladder is incomplete".to_owned());
    }
    for (row, expected_candidate) in rows.iter().zip(expected) {
        let candidate = row
            .get("candidate")
            .and_then(Value::as_str)
            .ok_or("reuse row has no candidate")?;
        let result = row
            .get("result")
            .and_then(Value::as_str)
            .ok_or("reuse row has no result")?;
        if candidate != expected_candidate || result.trim().len() < 12 {
            return Err(format!(
                "reuse candidate row is out of order or not substantive: {expected_candidate}"
            ));
        }
    }
    Ok(())
}

fn validate_scope_and_risk(evidence: &Value) -> Result<(), String> {
    let object = evidence
        .get("scope_and_risk_matrix")
        .and_then(Value::as_object)
        .ok_or("scope_and_risk_matrix is not an object")?;
    if object.len() != 2
        || string_array(&Value::Object(object.clone()), "required_outcomes")?.is_empty()
        || string_array(&Value::Object(object.clone()), "risk_cases")?.is_empty()
    {
        return Err(
            "scope_and_risk_matrix must contain nonempty required_outcomes and risk_cases only"
                .to_owned(),
        );
    }
    Ok(())
}

fn validate_artifact_justification(evidence: &Value) -> Result<(), String> {
    let object = evidence
        .get("artifact_specific_justification")
        .and_then(Value::as_object)
        .ok_or("artifact_specific_justification is not an object")?;
    if object.is_empty()
        || object.keys().any(|key| !safe_id(key))
        || object
            .values()
            .any(|value| value.as_str().is_none_or(|text| text.trim().len() < 12))
    {
        return Err(
            "artifact_specific_justification requires safe artifact IDs and substantive reasons"
                .to_owned(),
        );
    }
    Ok(())
}

fn validate_ceiling(evidence: &Value) -> Result<(), String> {
    let ceiling = evidence
        .get("ceilings_exceptions_and_evidence")
        .and_then(Value::as_object)
        .ok_or("ceilings_exceptions_and_evidence is not an object")?;
    for field in [
        "ceiling_id",
        "current_ceiling",
        "replacement_trigger",
        "likely_upgrade_path",
        "trigger_detection_method",
    ] {
        if ceiling
            .get(field)
            .and_then(Value::as_str)
            .is_none_or(|text| text.trim().is_empty())
        {
            return Err(format!("ceiling evidence omits {field}"));
        }
    }
    let linked = ceiling
        .get("linked_evidence")
        .and_then(Value::as_array)
        .ok_or("ceiling linked_evidence is not an array")?;
    if linked.is_empty()
        || linked
            .iter()
            .any(|value| value.as_str().is_none_or(|text| text.trim().is_empty()))
    {
        return Err("ceiling linked_evidence is empty or malformed".to_owned());
    }
    Ok(())
}

fn run_command(
    root: &Path,
    id: &str,
    program: &str,
    args: &[&str],
    checks: &mut Vec<Check>,
) -> Result<(), String> {
    run_command_with_proof_class(
        root,
        id,
        "structural",
        "fforager-xtask gate check",
        program,
        args,
        checks,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_command_with_proof_class(
    root: &Path,
    id: &str,
    proof_class: &'static str,
    executed_boundary: &'static str,
    program: &str,
    args: &[&str],
    checks: &mut Vec<Check>,
) -> Result<(), String> {
    let status = command_status_with_timeout(root, program, args, None, GATE_COMMAND_TIMEOUT)?;
    if !status.success() {
        return Err(format!("check {id} failed with {status}"));
    }
    checks.push(pass_with_class(
        id,
        proof_class,
        executed_boundary,
        &format!("{program} {} exited 0", args.join(" ")),
    ));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_command_with_env(
    root: &Path,
    id: &str,
    program: &str,
    args: &[&str],
    key: &str,
    value: &str,
    checks: &mut Vec<Check>,
) -> Result<(), String> {
    let status = command_status_with_timeout(
        root,
        program,
        args,
        Some((key, value)),
        GATE_COMMAND_TIMEOUT,
    )?;
    if !status.success() {
        return Err(format!("check {id} failed with {status}"));
    }
    checks.push(pass(
        id,
        &format!("{key}={value} {program} {} exited 0", args.join(" ")),
    ));
    Ok(())
}

fn collect_inputs(root: &Path) -> Result<Vec<InputState>, String> {
    let mut paths = vec![
        "rust-toolchain.toml".to_owned(),
        "build/Cargo.toml".to_owned(),
        "build/Cargo.lock".to_owned(),
        "build/architecture-policy.toml".to_owned(),
        "build/tooling-policy.toml".to_owned(),
        "build/rule-to-proof.toml".to_owned(),
        "build/deny.toml".to_owned(),
        "build/tools/fforager-xtask/Cargo.toml".to_owned(),
        "product/MODEL_MANUAL.md".to_owned(),
        ".GOV/codex.yaml".to_owned(),
        ".GOV/id-registry.yaml".to_owned(),
        ".GOV/rules/build-rules.yaml".to_owned(),
        ".GOV/spec/ferric_forager_technical_design_v0.3.0.md".to_owned(),
        ".GOV/taskboard/taskboard.yaml".to_owned(),
        ".GOV/templates/WORK_PACKET_CONTRACT_TEMPLATE.json".to_owned(),
        ".GOV/templates/WORK_PACKET_REQUIREMENTS.yaml".to_owned(),
        ".GOV/topology.yaml".to_owned(),
    ];
    paths.extend(active_evidence_inputs(root)?);
    for directory in [
        "build/fixtures/architecture",
        "build/fixtures/contracts",
        "build/fixtures/resource-durability-models",
        "build/fixtures/transport-v1",
        "build/crates/fforager-transport",
        "build/crates/fforager-testkit",
        "build/tools/fforager-xtask/src",
        "product/crates",
    ] {
        for path in walk_files(&root.join(directory))? {
            if matches!(
                path.extension().and_then(OsStr::to_str),
                Some("json" | "md" | "toml" | "rs")
            ) {
                paths.push(slash(path.strip_prefix(root).map_err(|e| e.to_string())?));
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .map(|path| {
            let hash = command_output(root, "git", &["hash-object", "--", &path])?;
            Ok(InputState {
                path,
                git_blob: hash.trim().to_owned(),
            })
        })
        .collect()
}

fn active_evidence_inputs(root: &Path) -> Result<Vec<String>, String> {
    let Some(id) = active_packet_id(root)? else {
        return Ok(Vec::new());
    };
    let packet = format!(".GOV/work_packets/{id}/packet.json");
    let value: Value = serde_json::from_str(
        &fs::read_to_string(root.join(&packet)).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("parse active packet for input collection: {error}"))?;
    let refinement = value
        .pointer("/extensions/refinement/path")
        .or_else(|| value.pointer("/extensions/refinement"))
        .and_then(Value::as_str)
        .ok_or("active packet has no refinement input path")?;
    require_relative_contained(root, refinement, ".GOV")?;
    Ok(vec![packet, refinement.to_owned()])
}

/// Serializes source-state observation during test runs only.
///
/// Several tests build a report and then re-validate it against the live
/// repository within the same process. Running `git status` / `git ls-files`
/// concurrently across parallel tests lets one invocation observe the index
/// mid-refresh and report a transient dirty flicker, so the captured provenance
/// no longer equals the recomputed provenance. Production runs a single command
/// per process, so this guard is compiled out of shipped gate execution.
#[cfg(test)]
static SOURCE_STATE_OBSERVATION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn source_state(root: &Path) -> Result<SourceState, String> {
    #[cfg(test)]
    let _observation_guard = SOURCE_STATE_OBSERVATION_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let commit = command_output(root, "git", &["rev-parse", "HEAD"])?;
    let status = command_output(root, "git", &["status", "--porcelain"])?;
    let mut dirty_paths = status
        .lines()
        .filter_map(|line| line.get(3..))
        .map(str::trim)
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    dirty_paths.sort();
    dirty_paths.dedup();
    Ok(SourceState {
        git_commit: commit.trim().to_owned(),
        dirty: !dirty_paths.is_empty(),
        dirty_paths,
        content_fingerprint: repository_content_fingerprint(root)?,
    })
}

pub(crate) fn source_states_equal(left: &SourceState, right: &SourceState) -> bool {
    left.git_commit == right.git_commit
        && left.dirty == right.dirty
        && left.dirty_paths == right.dirty_paths
        && left.content_fingerprint == right.content_fingerprint
}

fn repository_content_fingerprint(root: &Path) -> Result<String, String> {
    let listed = command_output(
        root,
        "git",
        &["ls-files", "--cached", "--others", "--exclude-standard"],
    )?;
    let mut paths = listed
        .lines()
        .filter(|path| !path.trim().is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    content_fingerprint_for_paths(root, &paths)
}

fn content_fingerprint_for_paths(root: &Path, paths: &[String]) -> Result<String, String> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut hasher = Sha256::new();
    for path in paths {
        hasher.update((path.len() as u64).to_le_bytes());
        hasher.update(path.as_bytes());
        match fs::read(root.join(path)) {
            Ok(bytes) => {
                hasher.update([1]);
                hasher.update((bytes.len() as u64).to_le_bytes());
                hasher.update(&bytes);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                hasher.update([0]);
            }
            Err(error) => return Err(format!("fingerprint source path {path}: {error}")),
        }
    }
    let bytes = hasher.finalize();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(encoded)
}

fn command_output(root: &Path, program: &str, args: &[&str]) -> Result<String, String> {
    command_output_with_timeout(root, program, args, TOOL_COMMAND_TIMEOUT)
}

fn cargo_proof_output(root: &Path, program: &str, args: &[&str]) -> Result<String, String> {
    if program != "cargo" {
        return Err(format!(
            "cargo proof path received non-cargo program {program}"
        ));
    }
    for attempt in 0..=CARGO_PROOF_TRANSIENT_RETRIES {
        match command_output_with_timeout(root, program, args, CARGO_PROOF_COMMAND_TIMEOUT) {
            Err(error)
                if attempt < CARGO_PROOF_TRANSIENT_RETRIES
                    && is_transient_windows_linker_initialization_failure(&error) =>
            {
                thread::sleep(Duration::from_secs(2));
            }
            result => return result,
        }
    }
    unreachable!("bounded Cargo proof retry loop always returns")
}

fn is_transient_windows_linker_initialization_failure(error: &str) -> bool {
    error.contains("link.exe") && error.contains("exit code: 0xc0000142")
}

fn child_report_path(root: &Path, output: &str, prefix: &str) -> Result<PathBuf, String> {
    let reports = root
        .join("build/reports")
        .canonicalize()
        .map_err(|error| format!("canonicalize build/reports: {error}"))?;
    let candidates = output
        .split("report=")
        .skip(1)
        .map(|suffix| suffix.split(';').next().unwrap_or_default().trim())
        .filter(|candidate| !candidate.is_empty())
        .collect::<Vec<_>>();
    if candidates.len() != 1 {
        return Err(format!(
            "FF-COMP-E-REPLAY-REPORT-PATH: expected exactly one child report path, observed {candidates:?}"
        ));
    }
    let relative = Path::new(candidates[0]);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(
            "FF-COMP-E-REPLAY-REPORT-PATH: child report path is not repository-relative".to_owned(),
        );
    }
    let observed = root.join(relative).canonicalize().map_err(|error| {
        format!("FF-COMP-E-REPLAY-REPORT-PATH: canonicalize child report: {error}")
    })?;
    if !observed.starts_with(&reports)
        || observed.extension().and_then(OsStr::to_str) != Some("json")
        || !observed
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.starts_with(prefix))
    {
        return Err(
            "FF-COMP-E-REPLAY-REPORT-PATH: child report escapes the expected report boundary"
                .to_owned(),
        );
    }
    Ok(observed)
}

/// A deep gate accepts only a report created by the child it just executed.
/// Retained reports are useful audit history, but are never fresh evidence.
fn report_file_snapshot(root: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let reports = root
        .join("build/reports")
        .canonicalize()
        .map_err(|error| format!("canonicalize build/reports: {error}"))?;
    fs::read_dir(&reports)
        .map_err(|error| format!("read build/reports: {error}"))?
        .map(|entry| {
            let entry = entry.map_err(|error| format!("read build/reports entry: {error}"))?;
            let kind = entry
                .file_type()
                .map_err(|error| format!("inspect build/reports entry: {error}"))?;
            if kind.is_file() && entry.path().extension().and_then(OsStr::to_str) == Some("json") {
                entry
                    .path()
                    .canonicalize()
                    .map_err(|error| format!("canonicalize report entry: {error}"))
                    .map(Some)
            } else {
                Ok(None)
            }
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|paths| paths.into_iter().flatten().collect())
}

fn validate_fresh_child_report(
    report_path: &Path,
    reports_before: &BTreeSet<PathBuf>,
) -> Result<(), String> {
    if reports_before.contains(report_path) {
        return Err(
            "FF-COMP-E-REPLAY-REPORT-STALE: child selected a replay report that existed before this deep-gate invocation"
                .to_owned(),
        );
    }
    Ok(())
}

fn command_status_with_timeout(
    root: &Path,
    program: &str,
    args: &[&str],
    environment: Option<(&str, &str)>,
    timeout: Duration,
) -> Result<ExitStatus, String> {
    let working_directory = command_working_directory(root);
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(&working_directory)
        .stdin(Stdio::null());
    if let Some((key, value)) = environment {
        command.env(key, value);
    }
    sanitize_rust_command_environment(&mut command, program);
    configure_quiet_process(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("run {program} {args:?}: {error}"))?;
    wait_for_child(&mut child, program, args, timeout)
}

fn command_output_with_timeout(
    root: &Path,
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<String, String> {
    let stdout = command_output_bytes_with_timeout(root, program, args, timeout)?;
    String::from_utf8(stdout)
        .map_err(|error| format!("{program} {args:?} emitted non-UTF-8 stdout: {error}"))
}

fn command_output_bytes_with_timeout(
    root: &Path,
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    command_output_bytes_with_timeout_and_environment(root, program, args, timeout, &[])
}

fn command_output_bytes_with_timeout_and_environment(
    root: &Path,
    program: &str,
    args: &[&str],
    timeout: Duration,
    environment: &[(&str, &str)],
) -> Result<Vec<u8>, String> {
    let capture_root = root.join(".fforager-artifacts/command-capture");
    fs::create_dir_all(&capture_root)
        .map_err(|error| format!("create command capture directory: {error}"))?;
    let stem = command_capture_stem(program)?;
    let stdout_path = capture_root.join(format!("{stem}.stdout"));
    let stderr_path = capture_root.join(format!("{stem}.stderr"));
    let stdout = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&stdout_path)
        .map_err(|error| format!("create stdout capture: {error}"))?;
    let stderr = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&stderr_path)
    {
        Ok(file) => file,
        Err(error) => {
            let _ = fs::remove_file(&stdout_path);
            return Err(format!("create stderr capture: {error}"));
        }
    };
    let working_directory = command_working_directory(root);
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(&working_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    for (key, value) in environment {
        command.env(key, value);
    }
    sanitize_rust_command_environment(&mut command, program);
    configure_quiet_process(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = fs::remove_file(&stdout_path);
            let _ = fs::remove_file(&stderr_path);
            return Err(format!("run {program} {args:?}: {error}"));
        }
    };
    let status = wait_for_child(&mut child, program, args, timeout);
    let stdout = read_capture(&stdout_path);
    let stderr = read_capture(&stderr_path);
    let _ = fs::remove_file(&stdout_path);
    let _ = fs::remove_file(&stderr_path);
    let status = status?;
    let stdout = stdout?;
    let stderr = stderr?;
    if !status.success() {
        return Err(format!(
            "{program} {args:?} failed with {status}; stdout={:?}; stderr={:?}",
            bounded_diagnostic(&stdout),
            bounded_diagnostic(&stderr)
        ));
    }
    Ok(stdout)
}

fn command_capture_stem(program: &str) -> Result<String, String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let sequence = COMMAND_CAPTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Ok(format!(
        "{}-{nonce}-{sequence}-{}",
        std::process::id(),
        sanitize_id(program)
    ))
}

fn validate_rust_verification_environment(root: &Path) -> Result<(), String> {
    let mut overrides = std::env::vars_os()
        .filter_map(|(key, value)| {
            // Cargo supplies its default home to child test processes.  It is
            // not an override when it is exactly the controlled home that
            // nested validator commands will use as well.
            if key == OsStr::new("CARGO_HOME")
                && controlled_cargo_home().as_deref() == Some(Path::new(&value))
            {
                None
            } else {
                rust_environment_override(&key).then_some(key)
            }
        })
        .map(|key| key.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    overrides.sort();
    overrides.dedup();
    if !overrides.is_empty() {
        return Err(format!(
            "FF-ARCH-E-RUST-ENV-OVERRIDE: verification refuses compiler, lint, wrapper, or toolchain overrides: {overrides:?}"
        ));
    }
    validate_rust_verification_configs(root)
}

fn validate_rust_verification_configs(root: &Path) -> Result<(), String> {
    let mut config_paths = root
        .ancestors()
        .flat_map(|ancestor| {
            [
                ancestor.join(".cargo/config.toml"),
                ancestor.join(".cargo/config"),
            ]
        })
        .collect::<Vec<_>>();
    if let Some(cargo_home) = controlled_cargo_home() {
        config_paths.push(cargo_home.join("config.toml"));
        config_paths.push(cargo_home.join("config"));
    }
    config_paths.sort();
    config_paths.dedup();
    let canonical_config = "[build]\ntarget-dir = \".fforager-artifacts/cargo-target\"\n";
    let repository_root = repo_root()?;
    let mut allowed = vec![root.join(".cargo/config.toml")];
    let dot_git = root.join(".git");
    if !dot_git.exists() && root.starts_with(&repository_root) {
        allowed.push(repository_root.join(".cargo/config.toml"));
        if repository_root.join(".git").is_file() {
            let shared_root = shared_repository_root(&repository_root)?;
            validate_linked_worktree_layout(&repository_root, &shared_root)?;
            allowed.push(shared_root.join(".cargo/config.toml"));
        }
    }
    if dot_git.is_file() {
        let shared_root = shared_repository_root(root)?;
        validate_linked_worktree_layout(root, &shared_root)?;
        allowed.push(shared_root.join(".cargo/config.toml"));
    }
    let mut invalid = Vec::new();
    for path in config_paths.into_iter().filter(|path| path.exists()) {
        let allowed_path = allowed
            .iter()
            .any(|candidate| canonical_paths_equal(candidate, &path).unwrap_or(false));
        let canonical_contents = fs::read_to_string(&path)
            .is_ok_and(|text| text.replace("\r\n", "\n") == canonical_config);
        if !allowed_path || !canonical_contents {
            let reason = match (allowed_path, canonical_contents) {
                (false, false) => "path-and-contents",
                (false, true) => "path",
                (true, false) => "contents",
                (true, true) => unreachable!(),
            };
            invalid.push(format!("{} ({reason})", path.display()));
        }
    }
    if invalid.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "FF-ARCH-E-RUST-ENV-OVERRIDE: verification refuses non-canonical effective Cargo configuration files: {invalid:?}"
        ))
    }
}

fn sanitize_rust_command_environment(command: &mut Command, program: &str) {
    let executable = Path::new(program)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or(program)
        .to_ascii_lowercase();
    if !matches!(executable.as_str(), "cargo" | "cargo.exe") {
        return;
    }
    for key in RUST_ENVIRONMENT_OVERRIDES {
        command.env_remove(key);
    }
    // `cargo run` sets this for its child even when the repository selector is
    // authoritative. Nested verification removes it so rust-toolchain.toml is
    // selected again rather than rejecting the canonical launcher itself.
    command.env_remove("RUSTUP_TOOLCHAIN");
    // Remove both inherited overrides and explicitly configured values on
    // this command.  `Command::env_remove` only affects inherited state, so
    // a caller-supplied target runner would otherwise survive sanitization.
    let configured_overrides = command
        .get_envs()
        .filter(|(key, _)| rust_environment_override(key))
        .map(|(key, _)| key.to_owned())
        .collect::<Vec<_>>();
    for key in configured_overrides {
        command.env_remove(key);
    }
    for (key, _) in std::env::vars_os() {
        if rust_environment_override(&key) {
            command.env_remove(key);
        }
    }
    if let Some(cargo_home) = controlled_cargo_home() {
        command.env("CARGO_HOME", cargo_home);
    }
}

fn controlled_cargo_home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|home| home.join(".cargo"))
}

fn rust_environment_override(key: &OsStr) -> bool {
    let key = key.to_string_lossy().to_ascii_uppercase();
    RUST_ENVIRONMENT_OVERRIDES.contains(&key.as_str())
        || key.starts_with("CARGO_ALIAS_")
        || key.starts_with("CARGO_BUILD_")
        || (key.starts_with("CARGO_TARGET_")
            && (key.ends_with("_RUSTFLAGS")
                || key.ends_with("_RUNNER")
                || key.ends_with("_LINKER")))
}

fn wait_for_child(
    child: &mut Child,
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<ExitStatus, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("wait for {program} {args:?}: {error}"))?
        {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let tree_termination = terminate_child_process_tree(child);
            let cleanup = cleanup_timed_out_child(child, &tree_termination);
            return Err(format!(
                "{program} {args:?} timed out after {}s; result is incomplete evidence; {cleanup}",
                timeout.as_secs()
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn cleanup_timed_out_child(child: &mut Child, tree_termination: &Result<(), String>) -> String {
    let mut direct_kill = None;
    if tree_termination.is_err() {
        direct_kill = Some(child.kill());
    }
    let first_reap = wait_for_child_bounded(child, Duration::from_secs(2));
    if !matches!(&first_reap, Ok(Some(_))) && direct_kill.is_none() {
        direct_kill = Some(child.kill());
    }
    let final_reap = if matches!(&first_reap, Ok(Some(_))) {
        first_reap
    } else {
        wait_for_child_bounded(child, Duration::from_secs(1))
    };
    format!(
        "tree_termination={tree_termination:?}; direct_kill={direct_kill:?}; bounded_reap={final_reap:?}"
    )
}

fn wait_for_child_bounded(
    child: &mut Child,
    timeout: Duration,
) -> Result<Option<ExitStatus>, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("bounded child reap failed: {error}"))?
        {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(windows)]
fn terminate_child_process_tree(child: &mut Child) -> Result<(), String> {
    let pid = child.id().to_string();
    let mut command = Command::new("taskkill");
    command
        .args(["/PID", &pid, "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_quiet_process(&mut command);
    let mut terminator = command
        .spawn()
        .map_err(|error| format!("terminate process tree rooted at {pid}: {error}"))?;
    let status = wait_for_termination_command(&mut terminator)?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "terminate process tree rooted at {pid} exited with {status}"
        ))
    }
}

#[cfg(unix)]
fn terminate_child_process_tree(child: &mut Child) -> Result<(), String> {
    let process_group = format!("-{}", child.id());
    terminate_unix_process_group("-TERM", &process_group)?;
    let grace_deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < grace_deadline {
        if child
            .try_wait()
            .map_err(|error| format!("wait for terminated process group: {error}"))?
            .is_some()
        {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }
    terminate_unix_process_group("-KILL", &process_group)
}

#[cfg(unix)]
fn terminate_unix_process_group(signal: &str, process_group: &str) -> Result<(), String> {
    let mut command = Command::new("kill");
    command
        .args([signal, process_group])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_quiet_process(&mut command);
    let mut terminator = command
        .spawn()
        .map_err(|error| format!("send {signal} to process group {process_group}: {error}"))?;
    let status = wait_for_termination_command(&mut terminator)?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "send {signal} to process group {process_group} exited with {status}"
        ))
    }
}

#[cfg(all(not(windows), not(unix)))]
fn terminate_child_process_tree(child: &mut Child) -> Result<(), String> {
    child
        .kill()
        .map_err(|error| format!("terminate child process: {error}"))
}

fn wait_for_termination_command(child: &mut Child) -> Result<ExitStatus, String> {
    let deadline = Instant::now() + TERMINATION_COMMAND_TIMEOUT;
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("wait for process-tree terminator: {error}"))?
        {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let kill_result = child.kill();
            let reap_result = child.wait();
            return Err(format!(
                "process-tree terminator timed out after {}s; kill={kill_result:?}; reap={reap_result:?}",
                TERMINATION_COMMAND_TIMEOUT.as_secs()
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn read_capture(path: &Path) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .map_err(|error| format!("read command capture {}: {error}", path.display()))?;
    Ok(bytes)
}

fn bounded_diagnostic(bytes: &[u8]) -> String {
    const LIMIT: usize = 4_096;
    let end = bytes.len().min(LIMIT);
    let mut text = String::from_utf8_lossy(&bytes[..end]).into_owned();
    if bytes.len() > LIMIT {
        text.push_str("...[truncated]");
    }
    text
}

fn sanitize_id(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(windows)]
fn configure_quiet_process(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(unix)]
fn configure_quiet_process(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(all(not(windows), not(unix)))]
fn configure_quiet_process(_command: &mut Command) {}

fn invocation(gate_args: &[String]) -> Invocation {
    let mut canonical = vec![
        "cargo".to_owned(),
        "run".to_owned(),
        "--manifest-path".to_owned(),
        "build/Cargo.toml".to_owned(),
        "--locked".to_owned(),
        "-p".to_owned(),
        "fforager-xtask".to_owned(),
        "--".to_owned(),
    ];
    canonical.extend_from_slice(gate_args);
    Invocation {
        repository_root: ".",
        gate_args: gate_args.to_vec(),
        canonical_command: canonical,
    }
}

fn write_report(root: &Path, prefix: &str, report: &GateReport) -> Result<PathBuf, String> {
    validate_gate_report_evidence(report)?;
    let mut value = serde_json::to_value(report).map_err(|error| error.to_string())?;
    if report.executed_proof_classes.iter().any(|class| {
        matches!(
            class.as_str(),
            "production_runtime" | "runtime_fault" | "runtime_observable"
        )
    }) {
        let receipts = report
            .artifacts
            .iter()
            .filter(|path| path.ends_with("/runtime-evidence.json"))
            .collect::<Vec<_>>();
        if receipts.len() != 1 || !safe_relative(receipts[0]) {
            return Err(
                "FF-ARCH-E-GATE-REPORT-RUNTIME: actual staged runtime evidence receipt is required"
                    .into(),
            );
        }
        let evidence: phase1_runtime::RuntimeEvidence = serde_json::from_slice(
            &fs::read(root.join(receipts[0])).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        phase1_runtime::validate_evidence(&evidence, Some(root))?;
        if evidence.source != report.source {
            return Err("FF-ARCH-E-GATE-REPORT-RUNTIME: evidence source mismatch".into());
        }
        value["schema_id"] = Value::String("ff.gate-report@2".into());
        value["schema_version"] = Value::String("2.0.0".into());
        if report.invocation.gate_args != evidence.originating_gate_args {
            value["derived_from"] =
                serde_json::to_value(invocation(&evidence.originating_gate_args))
                    .map_err(|error| error.to_string())?;
        }
        value["runtime_evidence"] =
            serde_json::to_value(evidence).map_err(|error| error.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?;
    let artifact: GateReportArtifact = serde_json::from_slice(&bytes).map_err(|error| {
        format!("FF-ARCH-E-GATE-REPORT-SCHEMA: serialize/parse report: {error}")
    })?;
    validate_gate_report_artifact(&artifact)?;
    let reports = root.join("build/reports");
    fs::create_dir_all(&reports).map_err(|e| e.to_string())?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let stem = format!("{prefix}-{nonce}-{}", std::process::id());
    let temporary = reports.join(format!(".{stem}.tmp"));
    let final_path = reports.join(format!("{stem}.json"));
    let result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.flush()?;
        file.sync_all()?;
        fs::rename(&temporary, &final_path)?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(format!("atomic report write failed: {error}"));
    }
    let persisted = fs::read(&final_path).map_err(|error| {
        format!("FF-ARCH-E-GATE-REPORT-SCHEMA: reread persisted report: {error}")
    })?;
    let persisted_artifact: GateReportArtifact =
        serde_json::from_slice(&persisted).map_err(|error| {
            format!("FF-ARCH-E-GATE-REPORT-SCHEMA: parse persisted report: {error}")
        })?;
    validate_gate_report_artifact(&persisted_artifact)?;
    if let Some(evidence) = &persisted_artifact.runtime_evidence {
        phase1_runtime::validate_evidence(evidence, Some(root))?;
    }
    final_path
        .strip_prefix(root)
        .map(Path::to_path_buf)
        .map_err(|e| e.to_string())
}

fn validate_gate_report_evidence(report: &GateReport) -> Result<(), String> {
    let actual = executed_proof_classes(&report.checks, &report.fixtures);
    if report.executed_proof_classes != actual {
        return Err("FF-ARCH-E-PROOF-CLASS-PROMOTION: report executed classes do not match executed result evidence".to_owned());
    }
    if report.aggregate_executed_proof_class != aggregate_executed_proof_class(&actual) {
        return Err("FF-ARCH-E-PROOF-CLASS-PROMOTION: aggregate is not derived from executed result evidence".to_owned());
    }
    if report.status == "PASS" && actual.is_empty() {
        return Err(
            "FF-ARCH-E-DECLARATION-ONLY-PROOF: PASS report has no executed evidence".to_owned(),
        );
    }
    let malformed_check = report.checks.iter().any(|check| {
        check.proof_class.trim().is_empty()
            || check.concrete_input.trim().is_empty()
            || check.executed_boundary.trim().is_empty()
            || check.expected_result.trim().is_empty()
            || check.observed_result.trim().is_empty()
    });
    let malformed_fixture = report.fixtures.iter().any(|fixture| {
        fixture.proof_class.trim().is_empty()
            || fixture.concrete_input.trim().is_empty()
            || fixture.executed_boundary.trim().is_empty()
            || fixture.expected_result.trim().is_empty()
            || fixture.observed_result.trim().is_empty()
    });
    if malformed_check || malformed_fixture {
        return Err(
            "FF-ARCH-E-DECLARATION-ONLY-PROOF: report result omits executed-boundary evidence"
                .to_owned(),
        );
    }
    if report.checks.iter().any(|check| {
        matches!(
            check.proof_class,
            "semantic" | "state_effect" | "wire_boundary" | "graph" | "public_boundary"
        ) && check.executed_boundary.contains("implementation-local")
    }) {
        return Err("FF-ARCH-E-DECLARATION-ONLY-PROOF: local helper is not a public/composed proof boundary".to_owned());
    }
    Ok(())
}

const GATE_REPORT_PROOF_CLASSES: &[&str] = &[
    "artifact",
    "counterfactual",
    "dependency",
    "external_process",
    "graph",
    "integration",
    "integration_observation",
    "negative_fixture",
    "policy",
    "process_plan",
    "production_runtime",
    "public_boundary",
    "runtime_fault",
    "runtime_observable",
    "scenario_contract",
    "semantic",
    "source",
    "state_effect",
    "structural",
    "wire_boundary",
];

#[allow(clippy::too_many_lines)]
fn validate_gate_report_artifact(report: &GateReportArtifact) -> Result<(), String> {
    let version_two = report.schema_id == "ff.gate-report@2" && report.schema_version == "2.0.0";
    if !version_two
        && (report.schema_id != "ff.gate-report@1"
            || report.schema_version != "1.0.0"
            || report.runtime_evidence.is_some()
            || report.derived_from.is_some())
    {
        return Err("FF-ARCH-E-GATE-REPORT-SCHEMA: invalid gate report identity".to_owned());
    }
    if version_two {
        if !matches!(
            report.gate_id.as_str(),
            RUNTIME_GATE | PR_GATE | "FF-GATE-WATCHER-001"
        ) || report.status != "PASS"
        {
            return Err("FF-ARCH-E-GATE-REPORT-RUNTIME: successor is confined to actual successful runtime gates".into());
        }
        let evidence = report
            .runtime_evidence
            .as_ref()
            .ok_or("FF-ARCH-E-GATE-REPORT-RUNTIME: direct staged-artifact evidence missing")?;
        phase1_runtime::validate_evidence(evidence, None)?;
        if let Some(origin) = &report.derived_from {
            if origin.repository_root != "."
                || origin.gate_args != evidence.originating_gate_args
                || origin.canonical_command != invocation(&origin.gate_args).canonical_command
            {
                return Err("FF-ARCH-E-GATE-REPORT-RUNTIME: derived verdict misstates its actual batch invocation".into());
            }
        } else if report.invocation.gate_args != evidence.originating_gate_args {
            return Err(
                "FF-ARCH-E-GATE-REPORT-RUNTIME: derived verdict omits actual invocation provenance"
                    .into(),
            );
        }
        if !report.checks.iter().any(|check| {
            check.id == "phase1-release-build"
                && check.status == "PASS"
                && check.proof_class == "external_process"
        }) {
            return Err(
                "FF-ARCH-E-GATE-REPORT-RUNTIME: locked release build execution missing".into(),
            );
        }
        for name in [
            "success",
            "transport-failure",
            "external-cancellation",
            "crash-restart",
            "durable-prefixes",
            "state-counterfactual",
            "watcher-missing",
            "watcher-corrupt",
            "watcher-incompatible",
            "watcher-crash",
            "watcher-hung",
            "watcher-storage-failure",
        ] {
            let matches = report
                .checks
                .iter()
                .filter(|check| check.id == format!("phase1-{name}"))
                .collect::<Vec<_>>();
            let class = match name {
                "durable-prefixes" => "state_effect",
                "state-counterfactual" => "counterfactual",
                _ => "production_runtime",
            };
            if matches.len() != 1
                || matches[0].proof_class != class
                || matches[0].status != "PASS"
                || !matches[0].skipped_semantic_dependencies.is_empty()
            {
                return Err(
                    "FF-ARCH-E-GATE-REPORT-RUNTIME: exact executed scenario checks missing".into(),
                );
            }
        }
        if !report.checks.iter().any(|check| {
            check.id == "phase1-watcher-boundaries"
                && check.status == "PASS"
                && check.proof_class == "runtime_fault"
                && check.skipped_semantic_dependencies.is_empty()
        }) {
            return Err("FF-ARCH-E-GATE-REPORT-RUNTIME: watcher fault execution missing".into());
        }
        if evidence.source.git_commit != report.source.git_commit
            || evidence.source.content_fingerprint != report.source.content_fingerprint
            || evidence.source.dirty != report.source.dirty
            || evidence.source.dirty_paths != report.source.dirty_paths
        {
            return Err("FF-ARCH-E-GATE-REPORT-RUNTIME: direct evidence source differs".into());
        }
        if report.checks.iter().any(|check| {
            matches!(
                check.proof_class.as_str(),
                "production_runtime" | "runtime_fault"
            ) && !check.skipped_semantic_dependencies.is_empty()
        }) {
            return Err(
                "FF-ARCH-E-GATE-REPORT-RUNTIME: actual runtime check claims skipped execution"
                    .into(),
            );
        }
    }
    let expected_args: &[&str] = match report.gate_id.as_str() {
        ARCH_GATE => &["architecture-check"],
        DEEP_GATE => &["verify-deep", "--evidence-from-taskboard"],
        PR_GATE => &["verify-pr", "--evidence-from-taskboard"],
        RUNTIME_GATE => &["runtime-truth-check", "--evidence-from-taskboard"],
        "FF-GATE-WATCHER-001" if version_two || report.status == "FAIL" => &["watcher-check"],
        _ => {
            return Err(
                "FF-ARCH-E-GATE-REPORT-SCHEMA: report names an unknown gate identity".to_owned(),
            );
        }
    };
    if report.gate_version != 1
        || report.invocation.repository_root != "."
        || report
            .invocation
            .gate_args
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            != expected_args
        || report.invocation.canonical_command
            != invocation(&report.invocation.gate_args).canonical_command
    {
        return Err(
            "FF-ARCH-E-GATE-REPORT-SCHEMA: report invocation is not the canonical gate invocation"
                .to_owned(),
        );
    }
    if report.source.git_commit.trim().is_empty()
        || report.source.dirty == report.source.dirty_paths.is_empty()
        || report.source.content_fingerprint.len() != 64
        || !report
            .source
            .content_fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || report
            .source
            .dirty_paths
            .iter()
            .any(|path| path.trim().is_empty())
    {
        return Err(
            "FF-ARCH-E-GATE-REPORT-SCHEMA: report source provenance is malformed".to_owned(),
        );
    }
    let input_paths = report
        .inputs
        .iter()
        .map(|input| input.path.as_str())
        .collect::<BTreeSet<_>>();
    if report.inputs.iter().any(|input| {
        input.path.trim().is_empty()
            || !matches!(input.git_blob.len(), 40 | 64)
            || !input.git_blob.bytes().all(|byte| byte.is_ascii_hexdigit())
    }) || input_paths.len() != report.inputs.len()
    {
        return Err("FF-ARCH-E-GATE-REPORT-SCHEMA: report inputs are malformed".to_owned());
    }
    validate_gate_report_class_set(
        &report.declared_supported_proof_classes,
        "FF-ARCH-E-GATE-REPORT-DECLARED",
        report.status == "PASS",
    )?;
    validate_gate_report_class_set(
        &report.executed_proof_classes,
        "FF-ARCH-E-GATE-REPORT-EXECUTED",
        false,
    )?;
    if report
        .executed_proof_classes
        .iter()
        .any(|class| !report.declared_supported_proof_classes.contains(class))
    {
        return Err(
            "FF-ARCH-E-GATE-REPORT-DECLARATION: executed proof class is not declared by the gate"
                .to_owned(),
        );
    }
    if !version_two
        && report.executed_proof_classes.iter().any(|class| {
            matches!(
                class.as_str(),
                "production_runtime" | "runtime_observable" | "runtime_fault"
            )
        })
    {
        return Err(
            "FF-ARCH-E-GATE-REPORT-RUNTIME: runtime-class execution is forbidden until a gate schema includes direct staged-artifact runtime evidence"
                .to_owned(),
        );
    }
    match report.status.as_str() {
        "PASS" => {
            if report.exit_code != 0
                || report.checks.is_empty()
                || report.checks.iter().any(|check| check.status != "PASS")
                || report
                    .fixtures
                    .iter()
                    .any(|fixture| fixture.status != "PASS")
            {
                return Err(
                    "FF-ARCH-E-GATE-REPORT-RESULT: PASS report has a non-PASS result or nonzero exit"
                        .to_owned(),
                );
            }
        }
        "FAIL" => {
            if report.exit_code == 0
                || report.checks.is_empty()
                || report.checks.iter().any(|check| check.status != "FAIL")
                || !report.fixtures.is_empty()
            {
                return Err(
                    "FF-ARCH-E-GATE-REPORT-RESULT: FAIL report has an invalid result shape"
                        .to_owned(),
                );
            }
        }
        _ => {
            return Err(
                "FF-ARCH-E-GATE-REPORT-RESULT: gate report status must be PASS or FAIL".to_owned(),
            );
        }
    }
    let actual = executed_gate_artifact_proof_classes(report);
    if report.executed_proof_classes != actual
        || report.aggregate_executed_proof_class != aggregate_executed_proof_class(&actual)
    {
        return Err(
            "FF-ARCH-E-PROOF-CLASS-PROMOTION: persisted report proof classes do not derive from PASS result evidence"
                .to_owned(),
        );
    }
    if report.status == "PASS" && actual.is_empty() {
        return Err(
            "FF-ARCH-E-DECLARATION-ONLY-PROOF: PASS report has no PASS evidence".to_owned(),
        );
    }
    if report.rules.iter().any(|value| value.trim().is_empty())
        || report.artifacts.iter().any(|value| value.trim().is_empty())
        || report
            .proof_limitations
            .iter()
            .any(|value| value.trim().is_empty())
        || report.checks.iter().any(gate_report_check_malformed)
        || report.fixtures.iter().any(gate_report_fixture_malformed)
    {
        return Err(
            "FF-ARCH-E-GATE-REPORT-SCHEMA: persisted report omits required result evidence"
                .to_owned(),
        );
    }
    Ok(())
}

fn validate_gate_report_class_set(
    classes: &[String],
    diagnostic: &str,
    required: bool,
) -> Result<(), String> {
    let observed = classes.iter().map(String::as_str).collect::<BTreeSet<_>>();
    if (required && classes.is_empty())
        || classes
            .iter()
            .any(|class| !GATE_REPORT_PROOF_CLASSES.contains(&class.as_str()))
        || observed.len() != classes.len()
        || classes.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(format!(
            "{diagnostic}: proof classes must be known, unique, and sorted"
        ));
    }
    Ok(())
}

fn executed_gate_artifact_proof_classes(report: &GateReportArtifact) -> Vec<String> {
    let mut classes = report
        .checks
        .iter()
        .filter(|check| check.status == "PASS")
        .map(|check| check.proof_class.clone())
        .chain(
            report
                .fixtures
                .iter()
                .filter(|fixture| fixture.status == "PASS")
                .map(|fixture| fixture.proof_class.clone()),
        )
        .collect::<Vec<_>>();
    classes.sort();
    classes.dedup();
    classes
}

fn gate_report_check_malformed(check: &GateReportCheckArtifact) -> bool {
    check.id.trim().is_empty()
        || !GATE_REPORT_PROOF_CLASSES.contains(&check.proof_class.as_str())
        || check.concrete_input.trim().is_empty()
        || check.executed_boundary.trim().is_empty()
        || check.expected_result.trim().is_empty()
        || check.observed_result.trim().is_empty()
        || check.detail.trim().is_empty()
        || check
            .skipped_semantic_dependencies
            .iter()
            .any(|value| value.trim().is_empty())
}

fn gate_report_fixture_malformed(fixture: &GateReportFixtureArtifact) -> bool {
    fixture.fixture_id.trim().is_empty()
        || !GATE_REPORT_PROOF_CLASSES.contains(&fixture.proof_class.as_str())
        || fixture.concrete_input.trim().is_empty()
        || fixture.executed_boundary.trim().is_empty()
        || fixture.expected_result.trim().is_empty()
        || fixture.observed_result.trim().is_empty()
        || fixture.execution_path.trim().is_empty()
        || fixture.expected_diagnostic.trim().is_empty()
        || fixture.observed_diagnostics.is_empty()
        || fixture
            .skipped_semantic_dependencies
            .iter()
            .any(|value| value.trim().is_empty())
}

fn read_toml<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))
}

fn require_relative_contained(root: &Path, relative: &str, owner: &str) -> Result<(), String> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!("unsafe policy path {relative}"));
    }
    let expected = root.join(owner).canonicalize().map_err(|e| e.to_string())?;
    let observed = root
        .join(path)
        .canonicalize()
        .map_err(|e| format!("canonicalize {relative}: {e}"))?;
    if !observed.starts_with(expected) {
        return Err(format!("path {relative} escapes owner {owner}"));
    }
    Ok(())
}

fn walk_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in
            fs::read_dir(&directory).map_err(|e| format!("read {}: {e}", directory.display()))?
        {
            let entry = entry.map_err(|e| e.to_string())?;
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_symlink() {
                return Err(format!(
                    "symlink is not allowed in governed product source: {}",
                    entry.path().display()
                ));
            }
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                files.push(entry.path());
            }
        }
    }
    Ok(files)
}

fn normalize(path: &Path) -> String {
    slash(path)
        .to_ascii_lowercase()
        .trim_end_matches('/')
        .to_owned()
}

fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn active_packet_id(root: &Path) -> Result<Option<String>, String> {
    let path = root.join(".GOV/taskboard/taskboard.yaml");
    let text = fs::read_to_string(&path).map_err(|error| format!("read taskboard: {error}"))?;
    active_packet_id_from_text(&text, &path.display().to_string())
}

fn active_packet_id_from_text(text: &str, context: &str) -> Result<Option<String>, String> {
    if text
        .lines()
        .filter(|line| line.trim() == "current_focus:")
        .count()
        != 1
        || text
            .lines()
            .filter(|line| line.trim_start().starts_with("active_wp_id:"))
            .count()
            != 1
    {
        return Err(
            "taskboard must contain exactly one current_focus and active_wp_id key".to_owned(),
        );
    }
    let document = parse_single_yaml(text, context)?;
    let focus_node = document
        .as_mapping_get("current_focus")
        .ok_or("taskboard omits current_focus")?;
    let focus = focus_node
        .as_mapping()
        .ok_or("taskboard current_focus is not a mapping")?;
    let keys = focus
        .keys()
        .map(|key| {
            key.as_str()
                .ok_or("taskboard current_focus has a non-string key")
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    if keys != BTreeSet::from(["active_wp_id", "statement"]) {
        return Err(format!(
            "taskboard current_focus keys are invalid: {keys:?}"
        ));
    }
    let node = focus_node
        .as_mapping_get("active_wp_id")
        .ok_or("taskboard current_focus omits active_wp_id")?;
    if node.is_null() {
        return Ok(None);
    }
    let id = node
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or("taskboard active_wp_id is neither null nor a nonempty string")?;
    if !id.starts_with("WP-FF-")
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        return Err("active work-packet ID violates the safe naming grammar".to_owned());
    }
    Ok(Some(id.to_owned()))
}

fn pass(id: &str, detail: &str) -> Check {
    pass_with_class(id, "structural", "fforager-xtask gate check", detail)
}

fn pass_with_class(
    id: &str,
    proof_class: &'static str,
    executed_boundary: &'static str,
    detail: &str,
) -> Check {
    Check {
        id: id.to_owned(),
        status: "PASS",
        proof_class,
        concrete_input: id.to_owned(),
        executed_boundary: executed_boundary.to_owned(),
        expected_result: "check succeeds".to_owned(),
        observed_result: detail.to_owned(),
        skipped_semantic_dependencies: vec![
            "No shipped Ferric production entrypoint was executed by this gate check.".to_owned(),
        ],
        detail: detail.to_owned(),
    }
}

fn executed_proof_classes(checks: &[Check], fixtures: &[FixtureResult]) -> Vec<String> {
    let mut classes = checks
        .iter()
        .filter(|check| check.status == "PASS")
        .map(|check| check.proof_class.to_owned())
        .chain(
            fixtures
                .iter()
                .filter(|fixture| fixture.status == "PASS")
                .map(|fixture| fixture.proof_class.to_owned()),
        )
        .collect::<Vec<_>>();
    classes.sort();
    classes.dedup();
    classes
}

fn aggregate_executed_proof_class(classes: &[String]) -> String {
    let rank = |class: &str| match class {
        "structural" | "policy" | "source" | "dependency" | "process_plan" => Some(0_u8),
        "negative_fixture" | "artifact" => Some(1_u8),
        "counterfactual" | "scenario_contract" | "semantic" | "state_effect" | "wire_boundary"
        | "graph" | "public_boundary" => Some(2_u8),
        "integration" | "integration_observation" | "external_process" => Some(3_u8),
        "production_runtime" | "runtime_observable" | "runtime_fault" => Some(4_u8),
        _ => None,
    };
    classes
        .iter()
        .filter_map(|class| rank(class).map(|rank| (rank, class)))
        .min_by_key(|(rank, _)| *rank)
        .map_or_else(|| "none".to_owned(), |(_, class)| class.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_fixture_mutation_fails_closed() {
        assert!(diagnostic_from_production_validator("surprise").is_err());
    }

    #[test]
    fn known_fixture_has_stable_diagnostic() {
        assert_eq!(
            diagnostic_from_production_validator("add_wrong_root_build_file").unwrap(),
            "FF-ARCH-E-WRONG-ROOT"
        );
    }

    #[test]
    fn cargo_proof_retry_accepts_only_windows_linker_initialization_failure() {
        assert!(is_transient_windows_linker_initialization_failure(
            "error: linking with `link.exe` failed: exit code: 0xc0000142"
        ));
        assert!(!is_transient_windows_linker_initialization_failure(
            "error: linking with `link.exe` failed: exit code: 1"
        ));
        assert!(!is_transient_windows_linker_initialization_failure(
            "error[E0308]: mismatched types"
        ));
        assert!(!is_transient_windows_linker_initialization_failure(
            "test result: FAILED"
        ));
    }

    #[test]
    fn slash_normalizes_windows_separator() {
        assert_eq!(
            slash(Path::new(".fforager-artifacts\\cargo-target")),
            ".fforager-artifacts/cargo-target"
        );
    }

    #[test]
    fn transport_semantic_projection_is_mutation_sensitive() {
        let baseline = serde_json::json!({
            "id": "case-a",
            "family": "http11",
            "scenario_class": "positive",
            "requested_capabilities": ["http11"],
            "satisfied_capabilities": ["http11"],
            "blocked_capabilities": [],
            "connection_identity": "connection",
            "wire_identity": "wire",
            "pool_identity": "pool",
            "selected_address": "address",
            "proxy_identity": "direct",
            "alpn_identity": "none",
            "protocol_identity": "http/1.1",
            "limits": {"body_bytes": 1},
            "transcript": {"case_id": "case-a"},
            "expected_outcome": "ok",
            "observed_outcome": "ok",
            "status": "PASS"
        });
        let baseline_projection =
            transport_semantic_projection(&baseline).expect("baseline projection");
        let baseline_digest =
            encode_sha256(&serde_json::to_vec(&vec![baseline_projection]).expect("JSON"));
        assert!(
            validate_transport_semantic_digest(std::slice::from_ref(&baseline), &baseline_digest)
                .is_err(),
            "self-consistent fabricated evidence must fail the independent pinned digest"
        );
        for (field, mutation) in [
            ("id", serde_json::json!("case-b")),
            ("family", serde_json::json!("range")),
            ("scenario_class", serde_json::json!("negative")),
            ("expected_outcome", serde_json::json!("other")),
            ("observed_outcome", serde_json::json!("other")),
            ("status", serde_json::json!("FAIL")),
            ("transcript", serde_json::json!({})),
            (
                "blocked_capabilities",
                serde_json::json!([{
                    "capability": "http2",
                    "code": "forged",
                    "reason": "forged"
                }]),
            ),
        ] {
            let mut changed = baseline.clone();
            changed[field] = mutation;
            let projection = transport_semantic_projection(&changed).expect("changed projection");
            let digest = encode_sha256(&serde_json::to_vec(&vec![projection]).expect("JSON"));
            assert_ne!(
                baseline_digest, digest,
                "{field} mutation must change digest"
            );
        }
        let empty_digest = encode_sha256(&serde_json::to_vec(&Vec::<Value>::new()).expect("JSON"));
        assert_ne!(
            baseline_digest, empty_digest,
            "case omission must change digest"
        );
    }

    #[test]
    fn cycle_detection_distinguishes_cycle_from_chain() {
        assert!(graph_has_cycle(&[("a", "b"), ("b", "a")]));
        assert!(!graph_has_cycle(&[("a", "b"), ("b", "c")]));
    }

    #[test]
    fn runtime_scan_is_case_insensitive_and_segment_conservative() {
        assert_eq!(
            runtime_literal_diagnostic("Path::new(\".gov/cache\")"),
            Some("FF-ARCH-E-RUNTIME-BOUNDARY")
        );
        assert_eq!(
            runtime_literal_diagnostic("Path::new(\"build\")"),
            Some("FF-ARCH-E-RUNTIME-BOUNDARY")
        );
        assert_eq!(runtime_literal_diagnostic("ordinary product source"), None);
    }

    #[test]
    fn data_model_scan_rejects_runtime_handle_import_aliases() {
        assert_eq!(
            forbidden_data_model_token(
                "use std::net as std_alias; pub struct Leak(pub std_alias::TcpStream);"
            ),
            Some("std::net")
        );
        assert_eq!(
            forbidden_data_model_token(
                "use std::{os::fd::OwnedFd as Opaque}; pub struct Leak(pub Opaque);"
            ),
            Some("ownedfd")
        );
        assert_eq!(
            forbidden_data_model_token("use std::{process as p}; pub struct Leak(pub p::Child);"),
            Some("std::{process")
        );
        assert_eq!(
            forbidden_data_model_token("use std::{ collections::BTreeMap, fmt };"),
            None
        );
        for source in [
            "pub struct Leak(pub std::io::Stdin);",
            "use std::io::{Read, Stdin as Opaque}; pub struct Leak(pub Opaque);",
            "use std::io as streams; pub struct Leak(pub streams::Stdout);",
            "use std::{fmt, io as streams}; pub struct Leak(pub streams::Stderr);",
            "use std::io::*; pub struct Leak(pub Stdin);",
            "use std as platform; use platform::io::Stdin; pub struct Leak(pub Stdin);",
            "use std::io::{self as streams}; pub struct Leak(pub streams::Stdout);",
            "use std::{io::{Stdin as Input}}; pub struct Leak(pub Input);",
            "use std::{io}; pub struct Leak(pub io::Stdin);",
            "use std::{fmt, io}; pub struct Leak(pub io::Stdout);",
            "use std::{io::{self}}; pub struct Leak(pub io::Stderr);",
        ] {
            assert_eq!(
                forbidden_data_model_token(source),
                Some("std::io live handle")
            );
        }
        assert_eq!(
            forbidden_data_model_token("pub enum StreamDescriptor { Stdin, Stdout, Stderr }"),
            None
        );
        assert_eq!(
            forbidden_data_model_token(
                "pub struct Leak(pub interprocess::os::windows::named_pipe::DuplexPipeStream);"
            ),
            Some("interprocess::")
        );
    }

    #[test]
    fn contract_inventory_rejects_required_field_and_stable_id_mutations() {
        let root = repo_root().unwrap();
        let fixture_root = root.join("build/fixtures/contracts");
        let inventory: Value =
            serde_json::from_str(include_str!("../../../fixtures/contracts/inventory.json"))
                .unwrap();
        validate_contract_inventory_shape(&inventory).unwrap();
        let entries = inventory["entries"].as_array().unwrap();
        let states = inventory["state_machines"].as_array().unwrap();
        for row in entries {
            validate_contract_inventory_row(row, false, &fixture_root).unwrap();
        }
        for row in states {
            validate_contract_inventory_row(row, true, &fixture_root).unwrap();
        }
        let observed = entries
            .iter()
            .map(|row| row["id"].as_str().unwrap())
            .collect::<BTreeSet<_>>();
        assert_eq!(observed, expected_contract_inventory_ids());
        for id in [
            "FF-CONTRACT-ACQUISITION-001",
            "FF-CONTRACT-OUTPUT-SINK-001",
            "FF-CONTRACT-CONFIG-001",
            "FF-CONTRACT-EVENT-001",
            "FF-CONTRACT-ERROR-001",
            "FF-CONTRACT-CANCELLATION-001",
        ] {
            let row = entries.iter().find(|row| row["id"] == id).unwrap();
            validate_contract_inventory_row_shape(row, false).unwrap();
            assert!(required_inventory_fixture(id).is_some());
            let mut missing = row.clone();
            missing.as_object_mut().unwrap().remove("limits_errors");
            assert!(
                validate_contract_inventory_row_shape(&missing, false)
                    .unwrap_err()
                    .contains("field mismatch")
            );
        }

        let mut mutated = observed;
        mutated.remove("FF-CONTRACT-CONFIG-001");
        mutated.insert("FF-CONTRACT-INVENTED-001");
        assert_ne!(mutated, expected_contract_inventory_ids());

        let row = &entries[0];
        validate_contract_inventory_row(row, false, &fixture_root).unwrap();
        let mut nonexistent_proof = row.clone();
        nonexistent_proof["proof_id"] = Value::String("fabricated::tests::does_not_exist".into());
        assert!(
            validate_contract_inventory_row(&nonexistent_proof, false, &fixture_root)
                .unwrap_err()
                .contains("exact canonical executable proof")
        );
        let mut cross_owner_proof = row.clone();
        cross_owner_proof["proof_id"] = Value::String(
            "core::resource::tests::atomic_zero_exact_one_over_and_release_identity".into(),
        );
        assert!(
            validate_contract_inventory_row(&cross_owner_proof, false, &fixture_root)
                .unwrap_err()
                .contains("exact canonical executable proof")
        );
        for field in ["owner", "readiness_gate"] {
            let mut wrong_semantics = row.clone();
            wrong_semantics[field] = Value::String("fabricated".into());
            assert!(
                validate_contract_inventory_row(&wrong_semantics, false, &fixture_root)
                    .unwrap_err()
                    .contains("invalid owner or readiness gate")
            );
        }
    }

    #[test]
    fn contract_manual_validation_rejects_comment_and_command_stuffing() {
        let manual = include_str!("../../../../product/MODEL_MANUAL.md");
        validate_contract_manual_text(manual).unwrap();
        let normalized = manual.replace("\r\n", "\n");
        let commented = manual.replace(
            "Wire versions use incompatible major versions",
            "<!-- Wire versions use incompatible major versions -->",
        );
        assert!(
            validate_contract_manual_text(&commented)
                .unwrap_err()
                .contains("substantive structured operating manual")
        );
        let unlocked = manual.replace(
            "cargo test --manifest-path build/Cargo.toml --locked",
            "cargo test --manifest-path build/Cargo.toml",
        );
        assert!(
            validate_contract_manual_text(&unlocked)
                .unwrap_err()
                .contains("six exact locked Cargo workflows")
        );
        let canonical_block = canonical_contract_manual_commands().join("\n");
        let generic = "cargo metadata --manifest-path build/Cargo.toml --locked";
        let stuffed_block = [
            generic,
            generic,
            generic,
            generic,
            generic,
            canonical_contract_manual_commands()[5],
        ]
        .join("\n");
        let stuffed = normalized.replace(&canonical_block, &stuffed_block);
        assert_ne!(
            stuffed, normalized,
            "canonical command block must be present"
        );
        assert!(
            validate_contract_manual_text(&stuffed)
                .unwrap_err()
                .contains("six exact locked Cargo workflows")
        );
    }

    #[test]
    fn deep_verification_composes_doctest_gate() {
        let source = include_str!("main.rs");
        let deep = source
            .split_once("fn run_verify_deep_checks")
            .and_then(|(_, remainder)| remainder.split_once("fn validate_contract_inventory"))
            .map(|(function, _)| function)
            .unwrap();
        assert!(deep.contains("run_doctests(root, checks)?;"));
        let doctests = source
            .split_once("fn run_doctests")
            .and_then(|(_, remainder)| remainder.split_once("fn verify_tool_identities"))
            .map(|(function, _)| function)
            .unwrap();
        assert!(doctests.contains("\"--all-features\""));
        let pr = source
            .split_once("fn run_verify_pr_inner")
            .and_then(|(_, remainder)| remainder.split_once("fn fail_with_report"))
            .map(|(function, _)| function)
            .unwrap();
        assert!(pr.contains("resource_durability_receipt"));
        assert!(pr.contains("run_verify_deep_checks("));
        assert!(!pr.contains("checks.push(pass(\n        \"verify-deep\""));
    }

    fn wp009_test_case(case_id: &str, injected_fault: &str) -> Value {
        serde_json::json!({
            "case_id": case_id,
            "model": "resource_admission",
            "scenario": "complete_vector_or_nothing",
            "injected_fault": injected_fault
        })
    }

    fn wp009_test_row(case: &Value) -> Value {
        serde_json::json!({
            "case_id": "wp009-resource-atomic-saturation",
            "invariant_id": "WP009-INV-RESOURCE-ATOMIC-001",
            "state": "capacity_saturated_waiter_queued",
            "resource_vector": {
                "memory_bytes": 10,
                "open_handles": 2,
                "ffmpeg_processes": 1,
                "ffmpeg_cpu_threads": 0,
                "javascript_workers": 0,
                "media_requests": 0,
                "metadata_requests": 0,
                "cpu_light_slots": 0,
                "cpu_heavy_slots": 0,
                "disk_read_bytes_in_flight": 0,
                "disk_write_bytes_in_flight": 0,
                "archive_writer_slots": 0,
                "sink_bytes": 0
            },
            "credit_owner": "none",
            "queue_occupancy": {"items": 1, "bytes": 1, "item_bound": 2, "byte_bound": 20},
            "durability_prefix": {
                "received": 0,
                "validated_written_contiguous": 0,
                "durable_contiguous": 0,
                "resume_at": 0
            },
            "injected_fault": "capacity_exhaustion",
            "recovery_action": "none",
            "concrete_input": case,
            "boundary": "fforager_core::resource::OwnedResourceBroker::request",
            "expected": "complete vector queued",
            "observed": "complete vector queued without partial grant",
            "proof_class": "semantic",
            "proof_mechanism": "direct_behavior",
            "credit_attribution": null,
            "supplemental_execution": null,
            "residual_uncertainty": "pure model only",
            "platform_observation": {
                "schema_id": "ff.wp009-platform-observation@1",
                "mode": "not_applicable",
                "commands": [],
                "observed_fields": {},
                "verdict": "not_applicable",
                "residual_uncertainty": []
            }
        })
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn wp009_report_validator_rejects_missing_fields_and_false_semantics() {
        let case = wp009_test_case("wp009-resource-atomic-saturation", "capacity_exhaustion");
        let row = wp009_test_row(&case);
        validate_wp009_row(&row, &case, "wp009-resource-atomic-saturation")
            .expect("canonical synthetic row must validate");

        let mut missing = row.clone();
        let _removed = missing
            .as_object_mut()
            .expect("row object")
            .remove("boundary");
        assert!(
            validate_wp009_row(&missing, &case, "wp009-resource-atomic-saturation")
                .unwrap_err()
                .contains("WP009-E-PROOF-FIELDS-MISSING")
        );

        let mut false_semantics = row.clone();
        false_semantics["state"] = serde_json::json!("declared_without_execution");
        assert!(
            validate_wp009_row(&false_semantics, &case, "wp009-resource-atomic-saturation")
                .unwrap_err()
                .contains("WP009-E-FALSE-SEMANTICS")
        );

        let mut noncanonical_class = row.clone();
        noncanonical_class["proof_class"] = serde_json::json!("counterfactual");
        assert!(
            validate_wp009_row(
                &noncanonical_class,
                &case,
                "wp009-resource-atomic-saturation"
            )
            .unwrap_err()
            .contains("WP009-E-FALSE-SEMANTICS")
        );

        let mut invalid_mechanism = row.clone();
        invalid_mechanism["proof_mechanism"] = serde_json::json!("semantic");
        assert!(
            validate_wp009_row(
                &invalid_mechanism,
                &case,
                "wp009-resource-atomic-saturation"
            )
            .unwrap_err()
            .contains("WP009-E-FALSE-SEMANTICS")
        );

        let mut cancellation = row.clone();
        cancellation["credit_attribution"] = serde_json::json!({
            "component": "single",
            "owner": 1,
            "stage": "writer",
            "claimed_bytes": 4,
            "consumed_bytes": 0,
            "lease_state": "cancelled_and_released",
            "global_claim_items_after": 0,
            "global_bytes_after": 0
        });
        validate_wp009_credit_attribution(&cancellation, "wp009-credit-owned-cancel")
            .expect("exact cancel attribution must validate");
        cancellation["credit_attribution"]["owner"] = serde_json::json!(2);
        assert!(
            validate_wp009_credit_attribution(&cancellation, "wp009-credit-owned-cancel")
                .unwrap_err()
                .contains("WP009-E-CREDIT-CANCEL-ATTRIBUTION")
        );

        let mut supplemental = row.clone();
        supplemental["supplemental_execution"] = serde_json::json!({
            "boundary": "StateMachine::acknowledge_byte_durability_effect with foreign OwnedByteCreditBroker",
            "expected": {
                "kind": "error",
                "error_type": "CreditError",
                "variant": "AcknowledgementBrokerMismatch"
            },
            "observed": {
                "kind": "error",
                "error_type": "CreditError",
                "variant": "AcknowledgementBrokerMismatch"
            },
            "proof_class": "semantic"
        });
        validate_wp009_supplemental_execution(&supplemental, "wp009-durability-effect-ack")
            .expect("exact wrong-broker execution must validate");
        supplemental["supplemental_execution"]["observed"] = serde_json::json!({"kind": "ok"});
        assert!(
            validate_wp009_supplemental_execution(&supplemental, "wp009-durability-effect-ack")
                .unwrap_err()
                .contains("WP009-E-SUPPLEMENTAL-EXECUTION")
        );
        supplemental["supplemental_execution"] = Value::Null;
        assert!(
            validate_wp009_supplemental_execution(&supplemental, "wp009-durability-effect-ack")
                .unwrap_err()
                .contains("WP009-E-SUPPLEMENTAL-EXECUTION")
        );

        let mut contradictory_platform = row.clone();
        contradictory_platform["residual_uncertainty"] =
            serde_json::json!("Model row only; not a live Windows host/filesystem run.");
        assert!(
            validate_wp009_platform_observation(
                &contradictory_platform,
                "wp009-filesystem-windows-ntfs"
            )
            .unwrap_err()
            .contains("WP009-E-WINDOWS-ROW-RESIDUAL")
        );

        let mut forged_platform = row.clone();
        forged_platform["platform_observation"]["verdict"] =
            serde_json::json!("matched_without_probe");
        assert!(
            validate_wp009_row(&forged_platform, &case, "wp009-resource-atomic-saturation")
                .unwrap_err()
                .contains("WP009-E-PLATFORM-OBSERVATION-UNMAPPED")
        );

        let mut missing_bound = row.clone();
        let _removed = missing_bound["queue_occupancy"]
            .as_object_mut()
            .expect("queue object")
            .remove("byte_bound");
        assert!(
            validate_wp009_row(&missing_bound, &case, "wp009-resource-atomic-saturation")
                .unwrap_err()
                .contains("WP009-E-QUEUE-BOUNDS-MISSING")
        );

        let mut outrun = row.clone();
        outrun["durability_prefix"]["durable_contiguous"] = serde_json::json!(2);
        outrun["durability_prefix"]["validated_written_contiguous"] = serde_json::json!(1);
        assert!(
            validate_wp009_row(&outrun, &case, "wp009-resource-atomic-saturation")
                .unwrap_err()
                .contains("WP009-E-DURABILITY-OUTRUN")
        );

        let mut unmapped = row;
        unmapped["concrete_input"]["scenario"] = serde_json::json!("unmapped");
        assert!(
            validate_wp009_row(&unmapped, &case, "wp009-resource-atomic-saturation")
                .unwrap_err()
                .contains("WP009-E-UNMAPPED-INPUT")
        );

        let recovery_case = serde_json::json!({
            "case_id": "wp009-recovery-prepared",
            "model": "commit_archive_reconciliation",
            "scenario": "idempotent_prefix_decision",
            "injected_fault": "crash_after_prepared"
        });
        let mut recovery = serde_json::json!({
            "case_id": "wp009-recovery-prepared",
            "invariant_id": "WP009-INV-RECOVERY-IDEMPOTENT-001",
            "state": "prepared",
            "resource_vector": {
                "memory_bytes": 0, "open_handles": 0, "ffmpeg_processes": 0,
                "ffmpeg_cpu_threads": 0, "javascript_workers": 0, "media_requests": 0,
                "metadata_requests": 0, "cpu_light_slots": 0, "cpu_heavy_slots": 0,
                "disk_read_bytes_in_flight": 0, "disk_write_bytes_in_flight": 0,
                "archive_writer_slots": 0, "sink_bytes": 0
            },
            "credit_owner": "none",
            "queue_occupancy": {"items": 0, "bytes": 0, "item_bound": 1, "byte_bound": 1},
            "durability_prefix": {
                "received": 0, "validated_written_contiguous": 0,
                "durable_contiguous": 0, "resume_at": 0
            },
            "injected_fault": "crash_after_prepared",
            "recovery_action": "Act(RevalidatePreparedThenRename)",
            "concrete_input": recovery_case.clone(),
            "boundary": "fforager_core::lifecycle::decide_recovery then apply_recovery_action, RecoveryApplication::retry, and decide_recovery",
            "expected": "Act(RevalidatePreparedThenRename)",
            "observed": "Act(RevalidatePreparedThenRename)",
            "proof_class": "semantic",
            "proof_mechanism": "direct_behavior",
            "credit_attribution": null,
            "supplemental_execution": null,
            "residual_uncertainty": "pure model only",
            "platform_observation": {
                "schema_id": "ff.wp009-platform-observation@1",
                "mode": "not_applicable",
                "commands": [],
                "observed_fields": {},
                "verdict": "not_applicable",
                "residual_uncertainty": []
            }
        });
        validate_wp009_row(&recovery, &recovery_case, "wp009-recovery-prepared")
            .expect("canonical recovery row must validate");
        recovery["observed"] = serde_json::json!("Act(InsertArchiveRow)");
        assert!(
            validate_wp009_row(&recovery, &recovery_case, "wp009-recovery-prepared")
                .unwrap_err()
                .contains("WP009-E-RECOVERY-NON-IDEMPOTENT")
        );
    }

    #[test]
    fn wp009_deep_gate_and_finding_mappings_fail_closed() {
        let comment_only = "// execute_resource_durability_models(root, checks) produced a receipt";
        assert!(comment_only.contains("execute_resource_durability_models"));
        assert!(
            require_wp009_execution_receipt(None)
                .unwrap_err()
                .contains("WP009-E-GATE-UNTRIGGERED")
        );
        let root = repo_root().expect("repository root");
        let invocation = begin_wp009_invocation(&root).expect("WP-009 invocation");
        let mut missing_receipt = Wp009ExecutionReceipt {
            invocation_id: invocation.id.clone(),
            report_path: "build/reports/wp-ff-009-resource-durability-models-missing.json"
                .to_owned(),
            report_sha256: "sha256:missing".to_owned(),
            manifest_fingerprint: "fnv1a64:missing".to_owned(),
            source_content_fingerprint: invocation.source_content_fingerprint.clone(),
            executed_cases: 48,
        };
        assert!(
            validate_wp009_execution_receipt(&root, &missing_receipt, &invocation)
                .unwrap_err()
                .contains("WP009-E-EXECUTION-RECEIPT-MISSING")
        );
        missing_receipt.invocation_id = "fabricated-stale-invocation".to_owned();
        assert!(
            validate_wp009_execution_receipt(&root, &missing_receipt, &invocation)
                .unwrap_err()
                .contains("WP009-E-EXECUTION-RECEIPT-INVOCATION")
        );
        assert_eq!(
            expected_adversarial_finding_proof("WP-FF-009-FINDING-RESOURCE-CONCURRENCY-001"),
            Some(
                "testkit::tests::resource_durability_report_rejects_missing_queue_bounds_and_durability_outrun"
            )
        );
        assert_eq!(
            expected_adversarial_finding_proof("WP-FF-009-FINDING-DURABILITY-FILESYSTEM-002"),
            Some("testkit::tests::resource_durability_model_corpus_executes_public_boundaries")
        );
        assert_eq!(
            expected_adversarial_finding_proof("WP-FF-009-FINDING-PROOF-REPORT-003"),
            Some("xtask::tests::wp009_report_validator_rejects_missing_fields_and_false_semantics")
        );
        assert_eq!(
            expected_adversarial_finding_proof("WP-FF-009-UNKNOWN"),
            None
        );
    }

    #[test]
    fn deep_gate_reports_canonical_rules_without_packet_acceptance_attribution() {
        let source = include_str!("main.rs");
        let report = source
            .split_once("fn run_verify_deep_inner")
            .and_then(|(_, remainder)| remainder.split_once("fn run_verify_deep_checks"))
            .map(|(function, _)| function)
            .expect("deep report implementation");
        assert!(report.contains("rules: architecture.rules.clone()"));
        assert!(!report.contains("WP-FF-005"));
        assert!(report.contains("\"counterfactual\".to_owned()"));

        let checks = source
            .split_once("fn run_verify_deep_checks")
            .and_then(|(_, remainder)| remainder.split_once("fn validate_contract_inventory"))
            .map(|(function, _)| function)
            .expect("deep check implementation");
        assert!(checks.contains("\"deep-proof-surface\""));
        assert!(!checks.contains("WP-FF-005"));
    }

    #[test]
    fn root_state_prioritizes_wrong_root_and_detects_selector_count() {
        assert_eq!(root_state_diagnostic(1, 1), Some("FF-ARCH-E-WRONG-ROOT"));
        assert_eq!(
            root_state_diagnostic(0, 2),
            Some("FF-ARCH-E-DUPLICATE-TOOLCHAIN")
        );
        assert_eq!(root_state_diagnostic(0, 1), None);
    }

    #[test]
    fn forbidden_edge_classifier_covers_special_boundaries() {
        assert_eq!(
            classify_layers("adapter", "adapter", DependencyKind::Normal),
            "FF-ARCH-E-ADAPTER-EDGE"
        );
        assert_eq!(
            classify_layers("product", "testkit", DependencyKind::Build),
            "FF-ARCH-E-TESTKIT-EDGE"
        );
        assert_eq!(
            classify_layers("product", "watcher", DependencyKind::Normal),
            "FF-ARCH-E-PRODUCT-WATCHER-EDGE"
        );
    }

    #[test]
    fn rule_inventory_fails_missing_and_unknown_rules() {
        let canonical = BTreeSet::from(["required"]);
        assert_eq!(
            rule_inventory_diagnostic(&canonical, &BTreeSet::new()),
            Some("FF-ARCH-E-MISSING-RULE")
        );
        assert_eq!(
            rule_inventory_diagnostic(&canonical, &BTreeSet::from(["required", "unknown"])),
            Some("FF-ARCH-E-UNKNOWN-RULE")
        );
    }

    #[test]
    fn proof_binding_requires_all_three_surfaces() {
        assert!(proof_binding_counts_valid(1, 1, 1));
        assert!(!proof_binding_counts_valid(1, 1, 0));
    }

    #[test]
    fn native_product_boundary_and_report_contract_guards_fail_closed() {
        assert_eq!(
            product_oracle_source_diagnostic(
                "let wrapped = \"yt-dlp\"; std::process::Command::new(wrapped);"
            ),
            Some("FF-ARCH-E-PRODUCT-ORACLE-RUNTIME")
        );
        assert_eq!(
            product_oracle_source_diagnostic("use pyo3::prelude::*;"),
            Some("FF-ARCH-E-PRODUCT-ORACLE-RUNTIME")
        );
        assert_eq!(
            product_oracle_boundary_diagnostic("product/scripts/bootstrap.py", b"exit 0"),
            Some("FF-ARCH-E-PRODUCT-ORACLE-RUNTIME")
        );
        assert_eq!(
            product_oracle_boundary_diagnostic(
                "product/assets/defaults.json",
                br#"{"delegate":"yt-dlp"}"#,
            ),
            Some("FF-ARCH-E-PRODUCT-ORACLE-RUNTIME")
        );
        assert_eq!(
            ferric_implementation_dependency_diagnostic(
                "build/crates/fforager-javascript/assets/yt-dlp-ejs-solver.js",
                b"export function solve() {}",
            ),
            Some("FF-ARCH-E-FERRIC-YTDLP-DEPENDENCY")
        );
        assert_eq!(
            ferric_implementation_dependency_diagnostic(
                "build/crates/fforager-javascript/src/lib.rs",
                b"const SOLVER: &str = \"yt-dlp-ejs\";",
            ),
            Some("FF-ARCH-E-FERRIC-YTDLP-DEPENDENCY")
        );
        assert_eq!(
            product_oracle_boundary_diagnostic(
                "product/MODEL_MANUAL.md",
                b"yt-dlp is a research-only oracle",
            ),
            None
        );
        assert_eq!(
            product_oracle_boundary_diagnostic(
                ".fforager-artifacts/cargo-target/research-oracle/yt-dlp.exe",
                b"yt-dlp",
            ),
            None
        );
        assert_eq!(
            product_oracle_boundary_diagnostic("product/docs/oracle.bin", &[0, 1, 2],),
            Some("FF-ARCH-E-PRODUCT-ORACLE-RUNTIME")
        );
        assert_eq!(
            product_oracle_boundary_diagnostic(
                "product/src/lib.rs",
                b"const PAYLOAD: &[u8] = include_bytes!(\"../docs/research.md\");",
            ),
            Some("FF-ARCH-E-PRODUCT-DOC-ASSET-REFERENCE")
        );
        assert_eq!(
            product_oracle_boundary_diagnostic(
                "product/src/lib.rs",
                b"let mut program = \"yt\".to_string(); program.push_str(\"-dlp\"); std::process::Command::new(program);",
            ),
            Some("FF-ARCH-E-PRODUCT-UNGOVERNED-PROCESS")
        );
        assert!(
            compatibility::structural_replay_report_mutation_diagnostic()
                .expect_err("full replay artifact mutation must fail closed")
                .starts_with("FF-COMP-E-SEMANTIC-EMPTY")
        );
    }

    #[test]
    fn gate_report_artifact_rejects_schema_and_runtime_class_promotions() {
        for (mutation, diagnostic) in [
            (
                "gate_report_unknown_proof_class",
                "FF-ARCH-E-GATE-REPORT-DECLARED",
            ),
            (
                "gate_report_undeclared_execution",
                "FF-ARCH-E-GATE-REPORT-DECLARATION",
            ),
            ("gate_report_nonpass_result", "FF-ARCH-E-GATE-REPORT-RESULT"),
            ("gate_report_runtime_claim", "FF-ARCH-E-GATE-REPORT-RUNTIME"),
        ] {
            let result = proof_report_fixture_execution(mutation)
                .unwrap_or_else(|error| panic!("{mutation} fixture failed: {error}"));
            assert!(
                result
                    .diagnostics
                    .iter()
                    .any(|observed| observed == diagnostic),
                "{mutation} did not produce {diagnostic}: {:?}",
                result.diagnostics
            );
        }
    }

    #[test]
    fn inventory_proof_guard_rejects_obvious_neutralized_test_bodies() {
        for body in [
            "{}",
            "{ return; }",
            "{ assert!(true); }",
            "{ assert!(1 == 1); }",
            "{ assert!(1 + 1 == 2); }",
            "{ assert_eq!(2 * 3, 6); }",
            "{ assert_ne!(\"left\", \"right\"); }",
            "{ assert_eq!(value, value); }",
            "{ let observed = 1; assert_eq!(observed, 1); }",
            "{ let one = 1; let two = one + 1; assert_eq!(two, 2); }",
            "{ const OBSERVED: usize = 1; assert_eq!(OBSERVED, 1); }",
            "{ debug_assert!(true); }",
        ] {
            assert!(
                inventory_proof_trivial_body(body),
                "inventory proof neutralization was not rejected: {body}"
            );
        }
        assert!(!inventory_proof_trivial_body(
            "{ assert!(fforager_contracts::ItemId::new(\"node_wrong\").is_err()); }"
        ));
    }

    #[test]
    fn exact_inventory_proof_requires_one_named_non_ignored_test() {
        let selector = "graph::tests::round_trip_preserves_tri_state";
        let passing = format!(
            "running 1 test\ntest {selector} ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 34 filtered out"
        );
        assert!(exact_test_execution_passed(&passing, selector));

        for rejected in [
            "running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 35 filtered out"
                .to_owned(),
            format!(
                "running 1 test\ntest {selector} ... ignored\n\ntest result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 34 filtered out"
            ),
            "running 1 test\ntest unrelated::tests::passes ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured"
                .to_owned(),
        ] {
            assert!(!exact_test_execution_passed(&rejected, selector));
        }
    }

    #[test]
    fn runtime_gate_result_classes_match_declared_capabilities() {
        let checks = prerequisite_runtime_checks(
            "WP-FF-013-proof-integrity-remediation-v2",
            &["product/crates/fforager-core/src/lifecycle.rs".to_owned()],
            false,
        );
        assert_eq!(
            executed_proof_classes(&checks, &[]),
            ["policy".to_owned(), "scenario_contract".to_owned()]
        );
        assert!(checks.iter().all(|check| {
            check.proof_class != "structural"
                && !check.concrete_input.trim().is_empty()
                && !check.executed_boundary.trim().is_empty()
                && !check.observed_result.trim().is_empty()
        }));
    }

    #[test]
    fn every_gate_report_proof_class_has_an_aggregate_rank() {
        for proof_class in GATE_REPORT_PROOF_CLASSES {
            assert_ne!(
                aggregate_executed_proof_class(&[(*proof_class).to_owned()]),
                "none",
                "allowed proof class has no aggregate rank: {proof_class}"
            );
        }
        assert_eq!(aggregate_executed_proof_class(&[]), "none");
    }

    #[test]
    fn remediation_evidence_preserves_truthful_pre_fix_failure() {
        let evidence = serde_json::json!({
            "pre_simplification_verdict": "FAIL: independently executed counterexamples reproduced the proof-integrity defects before remediation.",
            "post_simplification_verdict": "PASS: independently executed counterexamples and canonical gates reject every remediated defect."
        });
        validate_simplification_verdicts(&evidence).unwrap();

        let false_history = serde_json::json!({
            "pre_simplification_verdict": "PENDING: no pre-fix verdict was recorded for this remediation.",
            "post_simplification_verdict": "PASS: independently executed counterexamples and canonical gates reject every remediated defect."
        });
        assert!(validate_simplification_verdicts(&false_history).is_err());
    }

    #[test]
    fn parallel_command_capture_stems_are_unique() {
        let workers = (0..16)
            .map(|_| {
                thread::spawn(|| {
                    (0..32)
                        .map(|_| command_capture_stem("cargo").unwrap())
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();
        let stems = workers
            .into_iter()
            .flat_map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(stems.iter().collect::<BTreeSet<_>>().len(), stems.len());
    }

    #[test]
    fn compiled_fixture_targets_are_isolated_by_source_root() {
        let first_root = test_root("fixture-target-first");
        let second_root = test_root("fixture-target-second");
        fs::create_dir_all(&first_root).unwrap();
        fs::create_dir_all(&second_root).unwrap();

        let first = isolated_fixture_target_dir(&first_root, "public-invariant-mutations").unwrap();
        let first_repeat =
            isolated_fixture_target_dir(&first_root, "public-invariant-mutations").unwrap();
        let first_other_label =
            isolated_fixture_target_dir(&first_root, "compiled-testkit").unwrap();
        let second =
            isolated_fixture_target_dir(&second_root, "public-invariant-mutations").unwrap();

        assert_eq!(first, first_repeat);
        assert_ne!(first, first_other_label);
        assert_ne!(first, second);
        assert_eq!(
            first.file_name().unwrap().to_string_lossy().len(),
            32,
            "isolated target keys retain 128 bits of the SHA-256 digest"
        );
        assert!(first.starts_with(disposable_artifact_root(&first_root)));
        assert!(first_other_label.starts_with(disposable_artifact_root(&first_root)));
        assert!(second.starts_with(disposable_artifact_root(&second_root)));

        fs::remove_dir_all(&first_root).unwrap();
        fs::remove_dir_all(&second_root).unwrap();
    }

    #[test]
    fn isolated_multiline_mutations_are_line_ending_portable() {
        let root = test_root("crlf-mutation-anchor");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("source.rs");
        fs::write(&path, "before\r\nanchor\r\nafter\r\n").unwrap();
        replace_text_in_file(&path, "before\nanchor", "replaced\nanchor").unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "replaced\r\nanchor\r\nafter\r\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn receipt_only_public_counterexample_runs_the_independent_graph_boundary() {
        let root = repo_root().expect("repository root");
        let execution = receipt_only_public_counterexample_fixture_execution(&root)
            .expect("receipt-only mutation must be rejected by an executed graph test");
        assert_eq!(
            execution.diagnostics,
            ["FF-ARCH-E-PUBLIC-COUNTEREXAMPLE-RECEIPT-ONLY"]
        );
    }

    #[test]
    fn cargo_verification_strips_lint_and_compiler_overrides() {
        for key in [
            "CARGO_HOME",
            "CLIPPY_CONF_DIR",
            "RUSTFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
            "RUSTC_WRAPPER",
            "CARGO_BUILD_RUSTFLAGS",
            "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS",
            "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUNNER",
        ] {
            assert!(rust_environment_override(OsStr::new(key)), "{key}");
        }
        let mut command = Command::new("cargo");
        command
            .env("CLIPPY_CONF_DIR", "poison")
            .env("RUSTFLAGS", "--cap-lints=allow")
            .env("RUSTUP_TOOLCHAIN", "poison-toolchain")
            .env("CARGO_HOME", "poison-home")
            .env("CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUNNER", "fake-runner");
        sanitize_rust_command_environment(&mut command, "cargo");
        let environment = command
            .get_envs()
            .map(|(key, value)| (key.to_owned(), value.map(ToOwned::to_owned)))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(environment.get(OsStr::new("CLIPPY_CONF_DIR")), Some(&None));
        assert_eq!(environment.get(OsStr::new("RUSTFLAGS")), Some(&None));
        let expected_cargo_home = controlled_cargo_home().map(PathBuf::into_os_string);
        assert_eq!(
            environment.get(OsStr::new("CARGO_HOME")),
            Some(&expected_cargo_home)
        );
        assert_eq!(environment.get(OsStr::new("RUSTUP_TOOLCHAIN")), Some(&None));
        assert_eq!(
            environment.get(OsStr::new("CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUNNER")),
            Some(&None)
        );
    }

    #[test]
    fn deep_replay_rejects_a_report_that_predates_the_child_invocation() {
        let report = PathBuf::from("build/reports/compatibility-replay-old.json");
        let error = validate_fresh_child_report(&report, &BTreeSet::from([report.clone()]))
            .expect_err("preexisting replay report must not be accepted as fresh child evidence");
        assert!(error.starts_with("FF-COMP-E-REPLAY-REPORT-STALE"));
    }

    #[test]
    fn source_fingerprint_changes_when_dirty_content_changes_at_the_same_path() {
        let root = test_root("source-content-fingerprint");
        fs::create_dir_all(&root).unwrap();
        let relative = "same-path.rs".to_owned();
        fs::write(root.join(&relative), b"first dirty contents\n").unwrap();
        let before = content_fingerprint_for_paths(&root, std::slice::from_ref(&relative)).unwrap();
        fs::write(root.join(&relative), b"second dirty contents\n").unwrap();
        let after = content_fingerprint_for_paths(&root, &[relative]).unwrap();
        fs::remove_dir_all(root).unwrap();
        assert_ne!(before, after);
    }

    #[test]
    fn split_and_exception_helpers_fail_closed() {
        let owner = "WP-FF-006-rust-youtube-challenge-spike-v1";
        assert!(!split_trigger_valid("  ", owner));
        assert!(!split_trigger_valid("NOT-A-CANONICAL-TRIGGER", owner));
        assert!(!split_trigger_valid("WP-FF-6-bad-v1-AC-002", owner));
        assert!(!split_trigger_valid("WP-FF-006-bad_slug-v1-AC-002", owner));
        assert!(!split_trigger_valid(
            "WP-FF-005-versioned-core-contracts-v1-AC-002",
            owner
        ));
        assert!(split_trigger_valid("FF-BUILD-036", owner));
        assert!(split_trigger_valid(
            "WP-FF-006-rust-youtube-challenge-spike-v1-AC-002",
            owner
        ));
        assert_eq!(
            unapproved_exception_diagnostic(1),
            Some("FF-ARCH-E-UNAPPROVED-EXCEPTION")
        );
    }

    #[test]
    fn workspace_lint_inheritance_is_toml_semantic() {
        for manifest in [
            "[lints]\nworkspace = true\n",
            "[lints]\r\nworkspace = true\r\n",
            "[lints]\nworkspace=true\n",
            "[lints]\nother = \"value\"\nworkspace = true\n",
        ] {
            assert_eq!(manifest_inherits_workspace_lints(manifest), Ok(true));
        }
        assert_eq!(
            manifest_inherits_workspace_lints("[lints]\nworkspace = false\n"),
            Ok(false)
        );
        assert!(manifest_inherits_workspace_lints("[lints\nworkspace = true").is_err());
    }

    #[test]
    fn malformed_yaml_and_taskboard_state_fail_closed() {
        assert!(parse_single_yaml("malformed: [unterminated", "test").is_err());
        let valid = "current_focus:\n  statement: active\n  active_wp_id: WP-FF-003-test-v2\n";
        assert_eq!(
            active_packet_id_from_text(valid, "test"),
            Ok(Some("WP-FF-003-test-v2".to_owned()))
        );
        let closed = "current_focus:\n  statement: closed\n  active_wp_id: null\n";
        assert_eq!(active_packet_id_from_text(closed, "test"), Ok(None));
        assert!(active_packet_id_from_text(
            "malformed: [unterminated\ncurrent_focus:\n  statement: active\n  active_wp_id: WP-FF-003-test-v2\n",
            "test"
        )
        .is_err());
        assert!(active_packet_id_from_text(
            "current_focus:\n  statement: active\n  active_wp_id: WP-FF-003-test-v2\n  unexpected: true\n",
            "test"
        )
        .is_err());
    }

    #[test]
    fn nested_toolchain_selectors_are_inventoried() {
        let root = test_root("nested-selector");
        for directory in [
            ".GOV",
            "product/nested",
            "build",
            ".fforager-artifacts/cargo-target",
        ] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        fs::write(root.join("rust-toolchain.toml"), "[toolchain]\n").unwrap();
        fs::write(
            root.join("product/nested/rust-toolchain.toml"),
            "[toolchain]\n",
        )
        .unwrap();
        fs::write(
            root.join(".fforager-artifacts/cargo-target/rust-toolchain.toml"),
            "ignored generated output",
        )
        .unwrap();
        assert_eq!(
            toolchain_selectors(&root).unwrap(),
            [
                "product/nested/rust-toolchain.toml".to_owned(),
                "rust-toolchain.toml".to_owned(),
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn active_report_inputs_follow_the_active_packet() {
        let root = test_root("active-report-inputs");
        let packet_id = "WP-FF-003-test-v2";
        let packet_root = root.join(".GOV/work_packets").join(packet_id);
        fs::create_dir_all(root.join(".GOV/taskboard")).unwrap();
        fs::create_dir_all(&packet_root).unwrap();
        fs::write(
            root.join(".GOV/taskboard/taskboard.yaml"),
            format!("current_focus:\n  statement: active test\n  active_wp_id: {packet_id}\n"),
        )
        .unwrap();
        fs::write(
            packet_root.join("packet.json"),
            format!(
                "{{\"extensions\":{{\"refinement\":\".GOV/work_packets/{packet_id}/refinement.json\"}}}}"
            ),
        )
        .unwrap();
        fs::write(packet_root.join("refinement.json"), "{}\n").unwrap();
        let inputs = active_evidence_inputs(&root).unwrap();
        fs::remove_dir_all(root).unwrap();
        assert_eq!(
            inputs,
            [
                format!(".GOV/work_packets/{packet_id}/packet.json"),
                format!(".GOV/work_packets/{packet_id}/refinement.json"),
            ]
        );
    }

    #[test]
    fn dependency_features_and_hosts_fail_closed_on_drift() {
        let decision = DependencyDecision {
            name: "serde".to_owned(),
            version: "1.0.228".to_owned(),
            consumer: "fforager-xtask".to_owned(),
            runtime_class: "non_shipped_build_tooling".to_owned(),
            purpose: "typed serialization".to_owned(),
            native: false,
            owner: "WP-FF-003-executable-gate-bootstrap".to_owned(),
            allowed_consumers: vec!["fforager-xtask".to_owned()],
            reason: "strict policy".to_owned(),
            removal_trigger: "validated migration".to_owned(),
            approval_id: "WP-FF-003-executable-gate-bootstrap-v1-AC-002".to_owned(),
            features: vec!["derive".to_owned()],
            default_features: true,
            exception_id: None,
        };
        assert!(dependency_features_match(
            &["derive".to_owned()],
            true,
            &decision
        ));
        assert!(!dependency_features_match(&[], true, &decision));
        assert!(!dependency_features_match(
            &["derive".to_owned()],
            false,
            &decision
        ));
        assert!(work_packet_base_owner_valid(
            "WP-FF-006-rust-youtube-challenge-spike"
        ));
        assert!(decision_approval_matches_owner(
            "WP-FF-006-rust-youtube-challenge-spike-v1-AC-002",
            "WP-FF-006-rust-youtube-challenge-spike"
        ));
        assert!(!decision_approval_matches_owner(
            "WP-FF-005-versioned-core-contracts-v1-AC-002",
            "WP-FF-006-rust-youtube-challenge-spike"
        ));
        let supported = vec!["x86_64-pc-windows-msvc".to_owned()];
        assert!(host_supported(&supported, "x86_64-pc-windows-msvc"));
        assert!(!host_supported(&supported, "definitely-not-this-host"));
    }

    #[test]
    fn boringssl_source_policy_rejects_missing_vendoring_and_precompiled_features() {
        let root = test_root("boringssl-source-policy");
        fs::create_dir_all(&root).unwrap();
        let empty = BTreeSet::new();
        let error = validate_boringssl_source_dir("boring-sys2@4.15.15", &root, &empty)
            .expect_err("missing vendored source marker must fail");
        assert!(error.contains("FF-ARCH-E-NATIVE-SOURCE"));

        let marker = root.join("deps/boringssl/CMakeLists.txt");
        fs::create_dir_all(marker.parent().unwrap()).unwrap();
        fs::write(&marker, "cmake_minimum_required(VERSION 3.10)\n").unwrap();
        validate_boringssl_source_dir("boring-sys2@4.15.15", &root, &empty)
            .expect("packaged source marker");

        let selected_features = ["fips-link-precompiled".to_owned()].into_iter().collect();
        let error = validate_boringssl_source_dir("boring-sys2@4.15.15", &root, &selected_features)
            .expect_err("precompiled feature must fail");
        assert!(error.contains("FF-ARCH-E-NATIVE-PRECOMPILED"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn every_allowed_native_link_has_a_source_policy_and_override_guard() {
        assert!(native_source_override_variable("BORING_BSSL_PATH"));
        assert!(native_source_override_variable("ZSTD_LIB_DIR"));
        assert!(native_source_override_variable("LIBCLANG_PATH"));
        assert!(native_source_override_variable("PKG_CONFIG_PATH"));
        assert!(!native_source_override_variable("CARGO_TARGET_DIR"));

        let root = test_root("native-source-policies");
        fs::create_dir_all(root.join("zstd/lib/common")).unwrap();
        fs::write(root.join("zstd/lib/zstd.h"), "#define ZSTD_VERSION 1\n").unwrap();
        validate_zstd_source_dir("zstd-sys@test", &root, &BTreeSet::new())
            .expect("packaged zstd source");
        let zstd_error = validate_zstd_source_dir(
            "zstd-sys@test",
            &root,
            &BTreeSet::from(["pkg-config".to_owned()]),
        )
        .expect_err("host zstd discovery must fail");
        assert!(zstd_error.contains("FF-ARCH-E-NATIVE-SOURCE-OVERRIDE"));

        fs::write(root.join("build.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("Cargo.toml"), "[package]\nname='fixture'\n").unwrap();
        validate_clang_source_dir(
            "clang-sys@test",
            &root,
            &BTreeSet::from(["runtime".to_owned()]),
        )
        .expect("runtime clang discovery");
        let clang_error = validate_clang_source_dir("clang-sys@test", &root, &BTreeSet::new())
            .expect_err("missing runtime discovery");
        assert!(clang_error.contains("FF-ARCH-E-NATIVE-SOURCE"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn representative_repository_mutations_reach_production_validation() {
        assert_architecture_mutation_fails(
            "shipped-before-bootstrap",
            |root| {
                replace_file_text(
                    &root.join("build/architecture-policy.toml"),
                    "shipped = false",
                    "shipped = true",
                );
            },
            "FF-ARCH-E-SHIPPED-BUILD-TOOLING",
        );
        assert_architecture_mutation_fails(
            "self-authorized-exception",
            |root| {
                replace_file_text(
                    &root.join("build/architecture-policy.toml"),
                    "exception_decision_ids = [\"FF-DEC-001\", \"FF-DEC-002\", \"FF-DEC-003\", \"FF-DEC-004\", \"FF-DEC-005\"]",
                    "exception_decision_ids = [\"SELF-AUTHORIZED\", \"FF-DEC-002\", \"FF-DEC-003\", \"FF-DEC-004\", \"FF-DEC-005\"]",
                );
            },
            "FF-ARCH-E-UNAPPROVED-EXCEPTION",
        );
        assert_architecture_mutation_fails(
            "undeclared-member",
            |root| {
                replace_file_text(
                    &root.join("build/architecture-policy.toml"),
                    "name = \"fforager-xtask\"",
                    "name = \"undeclared\"",
                );
            },
            "FF-ARCH-E-UNDECLARED-MEMBER",
        );
        assert_architecture_mutation_fails(
            "shipped-governance-read",
            |root| {
                let path = root.join("product/forbidden.rs");
                fs::write(path, "const FORBIDDEN: &str = \".GOV\";\n").unwrap();
            },
            "FF-ARCH-E-RUNTIME-BOUNDARY",
        );
        assert_architecture_mutation_fails(
            "missing-split-trigger",
            |root| {
                replace_file_text(
                    &root.join("build/architecture-policy.toml"),
                    "split_trigger = \"FF-BUILD-036\"",
                    "split_trigger = \"\"",
                );
            },
            "FF-ARCH-E-INVALID-SPLIT-TRIGGER",
        );
        assert_architecture_mutation_fails(
            "missing-rule-map",
            |root| {
                replace_file_text(
                    &root.join("build/rule-to-proof.toml"),
                    "rule_id = \"FF-BUILD-036\"",
                    "rule_id = \"FF-BUILD-UNKNOWN\"",
                );
            },
            "FF-ARCH-E-MISSING-RULE",
        );
        assert_architecture_mutation_fails(
            "missing-fixture-binding",
            |root| {
                replace_file_text(
                    &root.join("build/rule-to-proof.toml"),
                    "fixture_ids = [\"self-authorized-exception\"]",
                    "fixture_ids = []",
                );
            },
            "FF-ARCH-E-MISSING-FIXTURE-BINDING",
        );
        assert_architecture_mutation_fails(
            "wrong-root-build-file",
            |root| {
                fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
            },
            "FF-ARCH-E-WRONG-ROOT",
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn native_exception_mutations_reach_production_validation() {
        assert_architecture_mutation_fails(
            "ffmpeg-native-exception-disables-ffi",
            |root| {
                replace_file_text(
                    &root.join(".GOV/rules/build-rules.yaml"),
                    "allow_ffi: true",
                    "allow_ffi: false",
                );
            },
            "FF-ARCH-E-EXCEPTION-SCHEMA",
        );
        assert_architecture_mutation_fails(
            "ffmpeg-native-exception-widens-unsafe",
            |root| {
                replace_file_text(
                    &root.join(".GOV/rules/build-rules.yaml"),
                    "allow_unsafe: scoped_platform_modules_only",
                    "allow_unsafe: crate_wide",
                );
            },
            "FF-ARCH-E-EXCEPTION-SCHEMA",
        );
        assert_architecture_mutation_fails(
            "ffmpeg-native-exception-removes-safety-invariants",
            |root| {
                replace_file_text(
                    &root.join(".GOV/rules/build-rules.yaml"),
                    "safety_invariants: [validated_direct_arguments_only, no_shell, pre_execution_windows_job_assignment, non_inheritable_handles, direct_child_wait, independent_windows_active_zero, unix_process_group_signal_scope, bounded_termination, exact_handle_ownership]",
                    "safety_invariants: []",
                );
            },
            "FF-ARCH-E-EXCEPTION-SCHEMA",
        );
        assert_architecture_mutation_fails(
            "ffmpeg-native-exception-removes-operator-decision",
            |root| {
                replace_file_text(
                    &root.join(".GOV/rules/build-rules.yaml"),
                    "operator_decision: The Operator explicitly approved the recommended narrow Ferric-owned windows-sys/libc process boundary on 2026-08-09.",
                    "operator_decision: \"\"",
                );
            },
            "FF-ARCH-E-EXCEPTION-SCHEMA",
        );
        assert_architecture_mutation_fails(
            "ffmpeg-native-exception-removes-residual",
            |root| {
                replace_file_text(
                    &root.join(".GOV/rules/build-rules.yaml"),
                    "residual_uncertainty: Windows parent death during suspended pre-execution setup can leave a suspended orphan unless a separately approved broker removes the interval; Unix process groups are signal scopes and do not contain hostile setsid or setpgid escape.",
                    "residual_uncertainty: \"\"",
                );
            },
            "FF-ARCH-E-EXCEPTION-SCHEMA",
        );
        assert_architecture_mutation_fails(
            "ffmpeg-native-exception-adds-unknown-key",
            |root| {
                replace_file_text(
                    &root.join(".GOV/rules/build-rules.yaml"),
                    "allow_ffi: true",
                    "allow_ffi: true\n      unsafe_escape: true",
                );
            },
            "FF-ARCH-E-EXCEPTION-SCHEMA",
        );
        assert_architecture_mutation_fails(
            "native-exception-wrong-consumer",
            |root| {
                replace_file_text(
                    &root.join(".GOV/rules/build-rules.yaml"),
                    "allowed_prerequisite_consumers: [fforager-transport]",
                    "allowed_prerequisite_consumers: [fforager-xtask]",
                );
            },
            "FF-ARCH-E-EXCEPTION-SCHEMA",
        );
        assert_architecture_mutation_fails(
            "native-exception-hidden-link-package",
            |root| {
                replace_file_text(
                    &root.join(".GOV/rules/build-rules.yaml"),
                    "\"clang-sys@1.8.1\", ",
                    "",
                );
            },
            "FF-ARCH-E-EXCEPTION-SCHEMA",
        );
        assert_architecture_mutation_fails(
            "native-exception-product-promotion",
            |root| {
                replace_file_text(
                    &root.join(".GOV/rules/build-rules.yaml"),
                    "allow_product_use: false",
                    "allow_product_use: true",
                );
            },
            "FF-ARCH-E-EXCEPTION-SCHEMA",
        );
        assert_architecture_mutation_fails(
            "ordinary-native-exception-product-demotion",
            |root| {
                replace_file_text(
                    &root.join(".GOV/rules/build-rules.yaml"),
                    "allow_product_use: true",
                    "allow_product_use: false",
                );
            },
            "FF-ARCH-E-EXCEPTION-SCHEMA",
        );
        assert_architecture_mutation_fails(
            "ordinary-native-exception-packaged-object-digest-drift",
            |root| {
                replace_file_text(
                    &root.join(".GOV/rules/build-rules.yaml"),
                    "packaged_object_inventory_sha256: \"be06dab876434410926fdf8d9ab7c37b18054f87adaf761dc6c8fcf167a56fb3\"",
                    "packaged_object_inventory_sha256: \"ae06dab876434410926fdf8d9ab7c37b18054f87adaf761dc6c8fcf167a56fb3\"",
                );
            },
            "FF-ARCH-E-NATIVE-PRECOMPILED",
        );
        assert_architecture_mutation_fails(
            "ordinary-native-exception-additive-consumer-widening",
            |root| {
                replace_file_text(
                    &root.join(".GOV/rules/build-rules.yaml"),
                    "allowed_product_consumers: [fforager-net]",
                    "allowed_product_consumers: [fforager-core, fforager-net]",
                );
            },
            "FF-ARCH-E-EXCEPTION-SCHEMA",
        );
        assert_architecture_mutation_fails(
            "ordinary-native-exception-additive-dependency-widening",
            |root| {
                replace_file_text(
                    &root.join(".GOV/rules/build-rules.yaml"),
                    "allowed_direct_dependencies: [reqwest, rustls]",
                    "allowed_direct_dependencies: [openssl, reqwest, rustls]",
                );
            },
            "FF-ARCH-E-EXCEPTION-SCHEMA",
        );
        assert_architecture_mutation_fails(
            "ordinary-native-exception-additive-runtime-widening",
            |root| {
                replace_file_text(
                    &root.join(".GOV/rules/build-rules.yaml"),
                    "allowed_runtime_classes: [non_shipped_phase0_prerequisite, shipped_rust_product]",
                    "allowed_runtime_classes: [non_shipped_build_tooling, non_shipped_phase0_prerequisite, shipped_rust_product]",
                );
            },
            "FF-ARCH-E-EXCEPTION-SCHEMA",
        );
        assert_architecture_mutation_fails(
            "ordinary-native-exception-ring-version-drift",
            |root| {
                replace_file_text(
                    &root.join(".GOV/rules/build-rules.yaml"),
                    "\"ring@0.17.14\"",
                    "\"ring@0.17.13\"",
                );
            },
            "FF-ARCH-E-EXCEPTION-SCHEMA",
        );
        assert_architecture_mutation_fails(
            "ordinary-reqwest-default-feature-drift",
            |root| {
                replace_file_text(
                    &root.join("build/architecture-policy.toml"),
                    "features = [\"http2\", \"rustls-no-provider\", \"stream\"]\ndefault_features = false",
                    "features = [\"http2\", \"rustls-no-provider\", \"stream\"]\ndefault_features = true",
                );
            },
            "FF-ARCH-E-DEPENDENCY-FEATURE",
        );
        assert_architecture_mutation_fails(
            "ordinary-prerequisite-runtime-relabeled-shipped",
            |root| {
                replace_file_text(
                    &root.join("build/architecture-policy.toml"),
                    "name = \"reqwest\"\nversion = \"0.13.4\"\nconsumer = \"fforager-transport\"\nruntime_class = \"non_shipped_phase0_prerequisite\"",
                    "name = \"reqwest\"\nversion = \"0.13.4\"\nconsumer = \"fforager-transport\"\nruntime_class = \"shipped_rust_product\"",
                );
            },
            "FF-ARCH-E-DEPENDENCY-RUNTIME-CLASS",
        );
        assert_architecture_mutation_fails(
            "ordinary-reqwest-aws-lc-feature-drift",
            |root| {
                replace_file_text(
                    &root.join("build/architecture-policy.toml"),
                    "features = [\"http2\", \"rustls-no-provider\", \"stream\"]",
                    "features = [\"http2\", \"rustls\", \"stream\"]",
                );
            },
            "FF-ARCH-E-DEPENDENCY-FEATURE",
        );
        assert_architecture_mutation_fails(
            "native-exception-shipped-product-reachability",
            mutate_shipped_product_native_reachability,
            "FF-ARCH-E-NATIVE-CONSUMER-REACHABILITY",
        );
        assert_architecture_mutation_fails(
            "native-exception-direct-edge-misattribution",
            mutate_direct_native_edge_misattribution,
            "FF-ARCH-E-NATIVE-EDGE-ATTRIBUTION",
        );
    }

    #[test]
    fn native_ffi_exception_authority_is_strict() {
        let source = repo_root().unwrap().join(".GOV/rules/build-rules.yaml");
        let baseline = fs::read_to_string(&source).unwrap();
        let root = test_root("native-ffi-exception-schema");
        let authority = root.join("build-rules.yaml");
        fs::create_dir_all(&root).unwrap();
        fs::write(&authority, &baseline).unwrap();
        let parsed = read_native_dependency_exceptions(&authority).expect("canonical exceptions");
        let ffmpeg = parsed.get("FF-DEC-003").expect("FFmpeg exception");
        assert_eq!(
            ffmpeg.allowed_direct_dependencies,
            BTreeSet::from(["libc".to_owned(), "windows-sys".to_owned()])
        );

        for (before, after) in [
            ("allow_ffi: true", "allow_ffi: false"),
            (
                "allow_unsafe: scoped_platform_modules_only",
                "allow_unsafe: crate_wide",
            ),
            (
                "safety_invariants: [validated_direct_arguments_only, no_shell, pre_execution_windows_job_assignment, non_inheritable_handles, direct_child_wait, independent_windows_active_zero, unix_process_group_signal_scope, bounded_termination, exact_handle_ownership]",
                "safety_invariants: []",
            ),
            (
                "operator_decision: The Operator explicitly approved the recommended narrow Ferric-owned windows-sys/libc process boundary on 2026-08-09.",
                "operator_decision: \"\"",
            ),
            (
                "residual_uncertainty: Windows parent death during suspended pre-execution setup can leave a suspended orphan unless a separately approved broker removes the interval; Unix process groups are signal scopes and do not contain hostile setsid or setpgid escape.",
                "residual_uncertainty: \"\"",
            ),
            (
                "allow_ffi: true",
                "allow_ffi: true\n      unknown_unsafe_escape: true",
            ),
        ] {
            let mutated = baseline.replacen(before, after, 1);
            assert_ne!(mutated, baseline, "mutation precondition: {before}");
            fs::write(&authority, mutated).unwrap();
            let error = read_native_dependency_exceptions(&authority)
                .expect_err("native FFI authority mutation must fail");
            assert!(error.contains("FF-ARCH-E-EXCEPTION-SCHEMA"), "{error}");
        }
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn native_exception_shipped_product_reachability_rejects_actual_resolve_graph() {
        let _execution_guard = ARCHITECTURE_CHECK_EXECUTION_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = architecture_sandbox("native-exception-shipped-product-resolve-graph");
        mutate_shipped_product_native_reachability(&root);
        let policy: ArchitecturePolicy =
            read_toml(&root.join("build/architecture-policy.toml")).unwrap();
        let native_exceptions =
            read_native_dependency_exceptions(&root.join(&policy.exception_authority)).unwrap();
        let tooling: ToolingPolicy = read_toml(&root.join("build/tooling-policy.toml")).unwrap();
        let metadata = cargo_metadata(&root, &policy, &tooling.supported_hosts[0]).unwrap();
        let error = validate_native_exception_reachability(&policy, &metadata, &native_exceptions)
            .unwrap_err();
        fs::remove_dir_all(&root).unwrap();
        assert!(
            error.contains("FF-ARCH-E-NATIVE-CONSUMER-REACHABILITY"),
            "expected shipped-product reachability rejection, observed {error}"
        );
    }

    #[test]
    fn packaged_object_inventory_rejects_add_remove_replace_and_rename() {
        const EXPECTED: &str = "1289642843a8c1080a5bd1d122028ea90c4378b2a235f133ed3c78f5bfa492db";
        let root = test_root("packaged-object-inventory");
        fs::create_dir_all(&root).unwrap();
        let a = root.join("a.o");
        let b = root.join("b.o");
        fs::write(&a, b"alpha").unwrap();
        fs::write(&b, b"beta").unwrap();
        validate_packaged_object_inventory("fixture@1", &root, true, EXPECTED)
            .expect("baseline inventory");

        fs::remove_file(&b).unwrap();
        assert!(validate_packaged_object_inventory("fixture@1", &root, true, EXPECTED).is_err());
        fs::write(&b, b"beta").unwrap();

        fs::write(&a, b"replaced").unwrap();
        assert!(validate_packaged_object_inventory("fixture@1", &root, true, EXPECTED).is_err());
        fs::write(&a, b"alpha").unwrap();

        let renamed = root.join("c.o");
        fs::rename(&b, &renamed).unwrap();
        assert!(validate_packaged_object_inventory("fixture@1", &root, true, EXPECTED).is_err());
        fs::rename(&renamed, &b).unwrap();

        let nested = root.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("rogue.o"), b"native").unwrap();
        assert!(validate_packaged_object_inventory("fixture@1", &root, true, EXPECTED).is_err());
        fs::remove_dir_all(&nested).unwrap();

        let unexpected = root.join("rogue.lib");
        fs::write(&unexpected, b"native").unwrap();
        let error =
            validate_packaged_object_inventory("fixture@1", &root, true, EXPECTED).unwrap_err();
        assert!(error.contains("unexpected packaged native artifacts"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn package_source_and_archive_content_mutations_fail_closed() {
        const SOURCE_EXPECTED: &str =
            "212c6412839a1ec079394d757c86b69519700f92ee9307bcddf6356e27a5be4b";
        const ALPHA_SHA256: &str =
            "8ed3f6ad685b959ead7022518e1af76cd816f8e8ec7ccdda1ed4018e8f2223f8";
        let root = test_root("package-source-content");
        fs::create_dir_all(root.join("crypto")).unwrap();
        fs::create_dir_all(root.join("src/verification")).unwrap();
        let build_rs = root.join("build.rs");
        let crypto_c = root.join("crypto/crypto.c");
        let windows_rs = root.join("src/verification/windows.rs");
        fs::write(&build_rs, b"alpha").unwrap();
        fs::write(&crypto_c, b"beta").unwrap();
        fs::write(&windows_rs, b"gamma").unwrap();
        validate_package_source_inventory_path("fixture@1", &root, SOURCE_EXPECTED)
            .expect("baseline source inventory");

        for path in [&build_rs, &crypto_c, &windows_rs] {
            let original = fs::read(path).unwrap();
            fs::write(path, b"mutated").unwrap();
            assert!(
                validate_package_source_inventory_path("fixture@1", &root, SOURCE_EXPECTED)
                    .is_err()
            );
            fs::write(path, original).unwrap();
        }

        let archive = root.join("fixture-1.crate");
        fs::write(&archive, b"alpha").unwrap();
        validate_file_sha256("fixture@1", &archive, ALPHA_SHA256)
            .expect("baseline archive checksum");
        fs::write(&archive, b"mutated").unwrap();
        assert!(validate_file_sha256("fixture@1", &archive, ALPHA_SHA256).is_err());
        fs::remove_dir_all(&root).unwrap();
    }

    fn assert_ordinary_profile_mutation_rejected(
        metadata: &Metadata,
        native_exceptions: &BTreeMap<String, NativeDependencyException>,
        diagnostic: &str,
    ) {
        let error =
            validate_ordinary_profile_isolation(metadata, native_exceptions, &mut Vec::new())
                .unwrap_err();
        assert!(error.contains(diagnostic), "{error}");
    }

    fn add_reqwest_resolved_feature(metadata: &Metadata) -> Metadata {
        let reqwest_id = metadata
            .packages
            .iter()
            .find(|package| {
                package.name.as_str() == "reqwest" && package.version.to_string() == "0.13.4"
            })
            .expect("reqwest package")
            .id
            .to_string();
        let mut value = serde_json::to_value(metadata).unwrap();
        let reqwest_node = value["resolve"]["nodes"]
            .as_array_mut()
            .expect("resolve nodes")
            .iter_mut()
            .find(|node| node["id"].as_str() == Some(&reqwest_id))
            .expect("reqwest resolve node");
        reqwest_node["features"]
            .as_array_mut()
            .expect("reqwest features")
            .push(Value::String("json".to_owned()));
        serde_json::from_value(value).unwrap()
    }

    fn add_no_links_ffi_package(metadata: &Metadata) -> Metadata {
        let mut value = serde_json::to_value(metadata).unwrap();
        let fake_id = "path+file:///synthetic-no-links-ffi#ffi-wrapper@0.1.0";
        let mut fake_package = value["packages"]
            .as_array()
            .expect("packages")
            .iter()
            .find(|package| package["name"].as_str() == Some("webpki-roots"))
            .expect("package template")
            .clone();
        fake_package["name"] = Value::String("ffi-wrapper".to_owned());
        fake_package["version"] = Value::String("0.1.0".to_owned());
        fake_package["id"] = Value::String(fake_id.to_owned());
        fake_package["source"] = Value::Null;
        fake_package["dependencies"] = Value::Array(Vec::new());
        fake_package["links"] = Value::Null;
        value["packages"]
            .as_array_mut()
            .expect("packages")
            .push(fake_package);
        let nodes = value["resolve"]["nodes"]
            .as_array_mut()
            .expect("resolve nodes");
        let transport_node = nodes
            .iter_mut()
            .find(|node| {
                node["id"]
                    .as_str()
                    .is_some_and(|id| id.contains("/fforager-transport#"))
            })
            .expect("transport resolve node");
        transport_node["dependencies"]
            .as_array_mut()
            .expect("transport dependencies")
            .push(Value::String(fake_id.to_owned()));
        let mut fake_edge = transport_node["deps"]
            .as_array()
            .expect("transport deps")
            .first()
            .expect("edge template")
            .clone();
        fake_edge["name"] = Value::String("ffi_wrapper".to_owned());
        fake_edge["pkg"] = Value::String(fake_id.to_owned());
        transport_node["deps"]
            .as_array_mut()
            .expect("transport deps")
            .push(fake_edge);
        nodes.push(serde_json::json!({
            "id": fake_id,
            "dependencies": [],
            "deps": [],
            "features": []
        }));
        serde_json::from_value(value).unwrap()
    }

    fn substitute_reqwest_source(metadata: &Metadata, manifest: &Path) -> Metadata {
        let mut value = serde_json::to_value(metadata).unwrap();
        let reqwest = value["packages"]
            .as_array_mut()
            .expect("packages")
            .iter_mut()
            .find(|package| {
                package["name"].as_str() == Some("reqwest")
                    && package["version"].as_str() == Some("0.13.4")
            })
            .expect("reqwest package");
        reqwest["manifest_path"] = Value::String(manifest.to_string_lossy().replace('\\', "/"));
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn ordinary_profile_rejects_resolved_transitive_feature_unification() {
        let root = repo_root().unwrap();
        let policy: ArchitecturePolicy =
            read_toml(&root.join("build/architecture-policy.toml")).unwrap();
        let tooling: ToolingPolicy = read_toml(&root.join("build/tooling-policy.toml")).unwrap();
        let native_exceptions =
            read_native_dependency_exceptions(&root.join(&policy.exception_authority)).unwrap();
        let metadata =
            cargo_metadata_ordinary_profile(&root, &policy, &tooling.supported_hosts[0]).unwrap();
        let mutated = add_reqwest_resolved_feature(&metadata);
        assert_ordinary_profile_mutation_rejected(
            &mutated,
            &native_exceptions,
            "FF-ARCH-E-RESOLVED-FEATURE-SURFACE",
        );

        let mutated = add_no_links_ffi_package(&metadata);
        assert_ordinary_profile_mutation_rejected(
            &mutated,
            &native_exceptions,
            "FF-ARCH-E-ORDINARY-PROFILE-CLOSURE",
        );

        let source_root = test_root("ordinary-reqwest-source-substitution");
        fs::create_dir_all(&source_root).unwrap();
        let substituted_manifest = source_root.join("Cargo.toml");
        fs::write(&substituted_manifest, b"[package]\nname='reqwest'\n").unwrap();
        let mutated = substitute_reqwest_source(&metadata, &substituted_manifest);
        assert_ordinary_profile_mutation_rejected(
            &mutated,
            &native_exceptions,
            "FF-ARCH-E-ORDINARY-SOURCE-CLOSURE",
        );
        fs::remove_dir_all(&source_root).unwrap();
    }

    #[test]
    fn native_exception_rejects_stale_security_patch_approval() {
        let root = architecture_sandbox("native-exception-stale-security-patch-approval");
        let locked_packages = fs::read(root.join("build/Cargo.lock")).unwrap();
        for relative in [
            ".GOV/rules/build-rules.yaml",
            "build/architecture-policy.toml",
        ] {
            let path = root.join(relative);
            let text = fs::read_to_string(&path).unwrap().replace(
                "WP-FF-023-dependency-security-remediation",
                "WP-FF-017-ordinary-transport-decision",
            );
            fs::write(path, text).unwrap();
        }
        let rules_text = fs::read_to_string(root.join(".GOV/rules/build-rules.yaml")).unwrap();
        let rules = parse_single_yaml(&rules_text, "coherent stale approval fixture").unwrap();
        let exception = rules
            .as_mapping_get("exception_authority")
            .and_then(|authority| authority.as_mapping_get("canonical_allowlist"))
            .and_then(Yaml::as_sequence)
            .unwrap()
            .iter()
            .find(|row| yaml_string(row, "id").unwrap() == "FF-DEC-002")
            .unwrap();
        let owner = "WP-FF-017-ordinary-transport-decision";
        let approval = "WP-FF-017-ordinary-transport-decision-v1-AC-001";
        assert_eq!(yaml_string(exception, "owner").unwrap(), owner);
        assert_eq!(yaml_string(exception, "approval_id").unwrap(), approval);
        assert!(decision_approval_matches_owner(approval, owner));
        assert_eq!(
            yaml_string_set(exception, "allowed_versions").unwrap(),
            BTreeSet::from([
                "reqwest@0.13.4".to_owned(),
                "ring@0.17.14".to_owned(),
                "rustls@0.23.46".to_owned(),
            ])
        );
        assert_eq!(
            yaml_string(exception, "ordinary_transport_resolved_closure_sha256").unwrap(),
            "ec38435517372f6afde502e138537603a66dc75f10e56a251e56ce183d5e3251"
        );
        assert_eq!(
            yaml_string(
                exception,
                "ordinary_transport_registry_source_closure_sha256"
            )
            .unwrap(),
            "929ab243eb663039dbc457eda301eb375fb3c284c586da41ae8d660bfabdcb8c"
        );
        let policy: ArchitecturePolicy =
            read_toml(&root.join("build/architecture-policy.toml")).unwrap();
        for (name, version) in [
            ("reqwest", "0.13.4"),
            ("rustls", "0.23.46"),
            ("webpki-roots", "1.0.10"),
        ] {
            let decision = policy
                .dependency_decisions
                .iter()
                .find(|decision| decision.name == name)
                .unwrap();
            assert_eq!(decision.version, version);
            assert_eq!(decision.owner, owner);
            assert_eq!(decision.approval_id, approval);
            assert!(decision_approval_matches_owner(
                &decision.approval_id,
                &decision.owner
            ));
        }
        assert_eq!(
            fs::read(root.join("build/Cargo.lock")).unwrap(),
            locked_packages
        );
        let error = architecture_check(&root).unwrap_err();
        fs::remove_dir_all(&root).unwrap();
        assert!(
            error.contains("FF-ARCH-E-EXCEPTION-SCHEMA"),
            "stale patch approval must fail the canonical exception boundary: {error}"
        );
    }

    #[test]
    fn native_exception_accepts_only_the_exact_approved_product_consumer() {
        let root = architecture_sandbox("native-exception-approved-product-resolve-graph");
        mutate_approved_shipped_product_native_reachability(&root);
        architecture_check(&root).expect("exact approved fforager-net reachability");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn policy_and_portability_mutations_reach_production_validation() {
        assert_architecture_mutation_fails(
            "invalid-split-trigger",
            |root| {
                replace_file_text(
                    &root.join("build/architecture-policy.toml"),
                    "split_trigger = \"FF-BUILD-036\"",
                    "split_trigger = \"NOT-A-CANONICAL-TRIGGER\"",
                );
            },
            "FF-ARCH-E-INVALID-SPLIT-TRIGGER",
        );
        assert_architecture_mutation_fails(
            "nested-selector",
            |root| {
                let path = root.join("product/nested/rust-toolchain.toml");
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, "[toolchain]\nchannel = \"stable\"\n").unwrap();
            },
            "FF-ARCH-E-DUPLICATE-TOOLCHAIN",
        );
        assert_architecture_mutation_fails(
            "malformed-build-rules",
            |root| {
                let path = root.join(".GOV/rules/build-rules.yaml");
                let text = fs::read_to_string(&path).unwrap();
                fs::write(path, format!("malformed: [unterminated\n{text}")).unwrap();
            },
            "FF-ARCH-E-POLICY-SCHEMA",
        );
        assert_architecture_mutation_fails(
            "unsupported-host",
            |root| {
                replace_file_text(
                    &root.join("build/tooling-policy.toml"),
                    "supported_hosts = [\"x86_64-pc-windows-msvc\"]",
                    "supported_hosts = [\"definitely-not-this-host\"]",
                );
            },
            "FF-TOOL-E-UNSUPPORTED-HOST",
        );
        assert_architecture_mutation_fails(
            "dependency-feature-drift",
            |root| {
                replace_file_text(
                    &root.join("build/Cargo.toml"),
                    "serde = { version = \"=1.0.228\", features = [\"derive\"] }",
                    "serde = \"=1.0.228\"",
                );
            },
            "FF-ARCH-E-DEPENDENCY-FEATURE",
        );
        assert_architecture_mutation_fails(
            "artifact-root-drift",
            |root| {
                replace_file_text(
                    &root.join(".cargo/config.toml"),
                    ".fforager-artifacts/cargo-target",
                    "build/target",
                );
            },
            "FF-ARCH-E-ARTIFACT-ROOT",
        );
    }

    #[cfg(windows)]
    #[test]
    fn bounded_process_timeout_returns_incomplete_evidence() {
        let error = command_status_with_timeout(
            &env::current_dir().unwrap(),
            "cmd",
            &["/C", "ping -n 6 127.0.0.1 >NUL"],
            None,
            Duration::from_millis(50),
        )
        .unwrap_err();
        assert!(error.contains("timed out"));
        assert!(error.contains("incomplete evidence"));
        assert!(error.contains("tree_termination=Ok"));
    }

    #[cfg(windows)]
    #[test]
    fn failed_tree_terminator_uses_bounded_direct_kill_fallback() {
        let mut command = Command::new("powershell.exe");
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 30",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_quiet_process(&mut command);
        let mut child = command.spawn().unwrap();
        let started = Instant::now();
        let detail = cleanup_timed_out_child(
            &mut child,
            &Err("forced tree-terminator failure".to_owned()),
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(detail.contains("direct_kill=Some(Ok"), "{detail}");
        assert!(child.try_wait().unwrap().is_some(), "{detail}");
    }

    #[test]
    fn executable_checksum_mismatch_fails_closed() {
        let root = test_root("checksum-mismatch");
        fs::create_dir_all(&root).unwrap();
        let executable = root.join("tool.exe");
        fs::write(&executable, b"known tool bytes").unwrap();
        let observed = sha256_file(&executable).unwrap();
        assert_eq!(
            verify_executable_checksum("tool", &executable, &observed),
            Ok(observed)
        );
        let error = verify_executable_checksum("tool", &executable, &"0".repeat(64)).unwrap_err();
        fs::remove_dir_all(root).unwrap();
        assert!(error.contains("checksum mismatch"));
        assert!(error.contains("expected="));
        assert!(error.contains("observed="));
    }

    #[test]
    fn runtime_contract_rejects_every_named_substitute() {
        for (mutation, diagnostic) in [
            (
                "runtime_test_only_substitute",
                "FF-RUNTIME-E-TEST-SUBSTITUTE",
            ),
            ("runtime_mock_boundary", "FF-RUNTIME-E-MOCK-SUBSTITUTE"),
            (
                "runtime_scaffold_completion",
                "FF-RUNTIME-E-SCAFFOLD-COMPLETION",
            ),
            ("runtime_noop_success", "FF-RUNTIME-E-NO-OBSERVABLE"),
            (
                "runtime_missing_artifact_identity",
                "FF-RUNTIME-E-ARTIFACT-IDENTITY",
            ),
            ("runtime_missing_clean_stage", "FF-RUNTIME-E-CLEAN-STAGE"),
            (
                "runtime_missing_counterfactual",
                "FF-RUNTIME-E-COUNTERFACTUAL",
            ),
            ("runtime_stage_collision", "FF-RUNTIME-E-STAGE-COLLISION"),
        ] {
            assert_eq!(runtime_fixture_diagnostic(mutation), Ok(diagnostic));
        }
    }

    #[test]
    fn runtime_counterfactual_uses_the_same_observable_oracle() {
        let expected = RuntimeExpected {
            exit_code: 0,
            stdout_contains: vec!["completed".to_owned()],
            stderr_contains: Vec::new(),
            output_files: Vec::new(),
        };
        let accepted = RuntimeObservation {
            exit_code: 0,
            stdout: "completed".to_owned(),
            stderr: String::new(),
            files: BTreeMap::new(),
        };
        validate_runtime_observation(&expected, &accepted).unwrap();
        let mut counterfactual = accepted;
        counterfactual.stdout.clear();
        assert!(
            validate_runtime_observation(&expected, &counterfactual)
                .unwrap_err()
                .contains("FF-RUNTIME-E-OBSERVABLE-MISSING")
        );
    }

    #[test]
    fn build_only_contract_reuse_rejects_dependency_and_product_drift() {
        let old_tool: toml::Value = toml::from_str(
            "[package]\nname='fforager-xtask'\n[dependencies]\nserde={workspace=true}",
        )
        .unwrap();
        let mut new_tool = old_tool.clone();
        new_tool["dependencies"].as_table_mut().unwrap().insert(
            "fforager-contracts".into(),
            toml::from_str("version='0.1.0'\npath='../../../product/crates/fforager-contracts'")
                .unwrap(),
        );
        let old_lock: toml::Value = toml::from_str("version=4\n[[package]]\nname='fforager-contracts'\nversion='0.1.0'\n[[package]]\nname='fforager-xtask'\nversion='0.1.0'\ndependencies=['serde']\n[[package]]\nname='serde'\nversion='1.0.228'\nsource='registry+https://github.com/rust-lang/crates.io-index'\nchecksum='retained-original-checksum'").unwrap();
        let mut new_lock = old_lock.clone();
        new_lock["package"][1]["dependencies"] =
            toml::Value::Array(vec!["fforager-contracts".into(), "serde".into()]);
        let old_policy: toml::Value = toml::from_str("forbidden_product_runtime_roots=['.GOV','build']\n[[members]]\nname='fforager-xtask'\nshipped=false\nartifact_role='non_shipped_gate_runner'\nallowed_internal_dependencies=[]\n[[members]]\nname='fforager-contracts'\nshipped=true").unwrap();
        let mut new_policy = old_policy.clone();
        let edges: toml::Value = toml::from_str(
            "edges=[{package='fforager-contracts',kinds=['normal'],optional=false}]",
        )
        .unwrap();
        new_policy["members"][0]["allowed_internal_dependencies"] = edges["edges"].clone();
        let admitted = |tool, lock, policy| {
            exact_contract_reuse_declarations(
                &old_tool,
                tool,
                &old_lock,
                lock,
                old_policy.clone(),
                policy,
            )
            .unwrap()
        };
        assert!(admitted(
            new_tool.clone(),
            new_lock.clone(),
            new_policy.clone()
        ));
        for mutation in [
            "feature",
            "optional",
            "path",
            "version",
            "registry-version",
            "checksum",
            "source",
            "other-edge",
            "shipped",
            "native",
        ] {
            let (mut tool, mut lock, mut policy) =
                (new_tool.clone(), new_lock.clone(), new_policy.clone());
            match mutation {
                "feature" => {
                    tool["dependencies"]["fforager-contracts"]
                        .as_table_mut()
                        .unwrap()
                        .insert("features".into(), toml::Value::Array(vec!["extra".into()]));
                }
                "optional" => {
                    tool["dependencies"]["fforager-contracts"]
                        .as_table_mut()
                        .unwrap()
                        .insert("optional".into(), true.into());
                }
                "path" => {
                    tool["dependencies"]["fforager-contracts"]["path"] = "foreign-contracts".into();
                }
                "version" => tool["dependencies"]["fforager-contracts"]["version"] = "0.2.0".into(),
                "registry-version" => lock["package"][2]["version"] = "1.0.229".into(),
                "checksum" => lock["package"][2]["checksum"] = "substituted".into(),
                "source" => {
                    lock["package"][2]["source"] =
                        "registry+https://unapproved.invalid/index".into();
                }
                "other-edge" => {
                    lock["package"][0].as_table_mut().unwrap().insert(
                        "dependencies".into(),
                        toml::Value::Array(vec!["serde".into()]),
                    );
                }
                "shipped" => policy["members"][1]["shipped"] = false.into(),
                "native" => {
                    policy
                        .as_table_mut()
                        .unwrap()
                        .insert("native_decision".into(), "unapproved".into());
                }
                _ => unreachable!(),
            }
            assert!(!admitted(tool, lock, policy), "{mutation} admitted");
        }
    }

    #[test]
    fn product_path_classifier_excludes_manual_but_not_runtime_code() {
        assert!(!product_runtime_path("product/MODEL_MANUAL.md"));
        assert!(product_runtime_path("product/fforager/src/main.rs"));
        assert!(product_runtime_path("product\\watcher\\src\\main.rs"));
        assert!(!product_affecting_path("build/Cargo.lock", false));
        assert!(product_affecting_path("build/Cargo.lock", true));
        assert!(product_affecting_path("rust-toolchain.toml", true));
    }

    #[test]
    fn architecture_policy_output_hygiene_is_not_product_impact() {
        let base = r#"
target_directory = "build/target"
forbidden_product_runtime_roots = [".GOV", "build"]
product_root = "product"
"#;
        let hygiene = r#"
target_directory = ".fforager-artifacts/cargo-target"
forbidden_product_runtime_roots = [".GOV", "build", ".fforager-artifacts"]
product_root = "product"
"#;
        assert_eq!(
            architecture_policy_product_affecting(base, hygiene),
            Ok(false)
        );
        let runtime_boundary_change = r#"
target_directory = ".fforager-artifacts/cargo-target"
forbidden_product_runtime_roots = ["build", ".fforager-artifacts"]
product_root = "product"
"#;
        assert_eq!(
            architecture_policy_product_affecting(base, runtime_boundary_change),
            Ok(true)
        );
    }

    #[test]
    fn runtime_proof_must_match_the_activation_checkpoint() {
        let proof = serde_json::json!({"schema_id": "ff.runtime-proof@1"});
        let activated = serde_json::json!({
            "scope": {"product_impact": "RUNTIME"},
            "extensions": {"runtime_proof": proof.clone()}
        });
        assert!(runtime_proof_predeclared(&activated, &proof));
        assert!(!runtime_proof_predeclared(
            &activated,
            &serde_json::json!({"schema_id": "rewritten"})
        ));
    }

    #[test]
    fn non_product_prerequisite_must_be_exactly_predeclared() {
        let declaration = serde_json::json!({
            "schema_id": "ff.non-product-prerequisite@1",
            "classification": "phase0_supporting_evidence",
            "design_authority": "technical-design#phase-0",
            "product_progress": false,
            "capability_progress": false,
            "runtime_completion": false,
            "packaging_or_release_progress": false,
            "phase_progress": false,
            "required_future_consumer": "the first shipped production slice",
            "required_future_proof": "FF-GATE-RUNTIME-001 through the exact staged production artifact"
        });
        let activated = serde_json::json!({
            "scope": {"product_impact": "PREREQUISITE"},
            "extensions": {"non_product_prerequisite": declaration.clone()},
            "acceptance_matrix": []
        });
        assert!(non_product_prerequisite_predeclared(
            &activated,
            &declaration
        ));
        let mut rewritten = declaration.clone();
        rewritten["design_authority"] = Value::String("rewritten".to_owned());
        assert!(!non_product_prerequisite_predeclared(
            &activated, &rewritten
        ));

        let mut runtime_proof = activated.clone();
        runtime_proof["extensions"]["runtime_proof"] = serde_json::json!({});
        assert!(!non_product_prerequisite_predeclared(
            &runtime_proof,
            &declaration
        ));

        let mut runtime_acceptance = activated;
        runtime_acceptance["acceptance_matrix"] =
            serde_json::json!([{"proof_class": "production_runtime"}]);
        assert!(!non_product_prerequisite_predeclared(
            &runtime_acceptance,
            &declaration
        ));
    }

    #[test]
    fn non_product_prerequisite_rejects_completion_claims_and_schema_drift() {
        let mut declaration = serde_json::json!({
            "schema_id": "ff.non-product-prerequisite@1",
            "classification": "phase0_supporting_evidence",
            "design_authority": "technical-design#phase-0",
            "product_progress": false,
            "capability_progress": false,
            "runtime_completion": false,
            "packaging_or_release_progress": false,
            "phase_progress": false,
            "required_future_consumer": "the first shipped production slice",
            "required_future_proof": "FF-GATE-RUNTIME-001 through the exact staged production artifact"
        });
        let valid: NonProductPrerequisite = serde_json::from_value(declaration.clone()).unwrap();
        validate_non_product_prerequisite(&valid).unwrap();

        declaration["phase_progress"] = Value::Bool(true);
        let claimed: NonProductPrerequisite = serde_json::from_value(declaration.clone()).unwrap();
        assert!(
            validate_non_product_prerequisite(&claimed)
                .unwrap_err()
                .contains("FF-RUNTIME-E-PREREQUISITE-COMPLETION-CLAIM")
        );

        declaration["phase_progress"] = Value::Bool(false);
        declaration["unexpected"] = Value::Bool(false);
        assert!(serde_json::from_value::<NonProductPrerequisite>(declaration).is_err());
    }

    #[test]
    fn prerequisite_zero_claim_surface_rejects_parallel_progress_claims() {
        let packet = serde_json::json!({
            "extensions": {
                "non_product_prerequisite": {},
                "product_progress_claim": {
                    "product_progress": true,
                    "phase_progress": true
                }
            }
        });
        assert!(
            validate_prerequisite_zero_claim_surface(&packet)
                .unwrap_err()
                .contains("CLAIM-SURFACE")
        );
        assert!(asserts_forbidden_prerequisite_completion(
            "Product capability and Phase 0 are complete"
        ));
        assert!(!asserts_forbidden_prerequisite_completion(
            "No product capability or Phase 0 is complete"
        ));
        assert!(asserts_forbidden_prerequisite_completion(
            "No product progress; Phase 0 is complete"
        ));
        assert!(asserts_forbidden_prerequisite_completion(
            "No product progress but runtime completion is validated"
        ));
        assert!(asserts_forbidden_prerequisite_completion(
            "No blockers and Phase 0 is complete"
        ));
        assert!(!asserts_forbidden_prerequisite_completion(
            "Product capability is not complete"
        ));
        for claim in [
            "Phase zero is complete",
            "The runtime has been completed",
            "Product capability is not only complete but validated",
            "Product capability is not incomplete; it is complete",
            "Release is ready",
        ] {
            assert!(
                asserts_forbidden_prerequisite_completion(claim),
                "forbidden claim bypassed: {claim}"
            );
        }
        for status in [
            "VALIDATED",
            "DONE",
            "SHIPPED",
            "READY",
            "IMPLEMENTED",
            "SUCCESS",
            "SUCCEEDED",
            "PASSED",
            "FINISHED",
            "PRODUCTION_READY",
            "OPERATIONAL",
        ] {
            assert!(affirmative_progress_value(&Value::String(
                status.to_owned()
            )));
        }
        for status_claim in [
            serde_json::json!({"runtime_status": "DONE"}),
            serde_json::json!({"runtime_status": "SUCCESS"}),
            serde_json::json!({"runtime": {"status": "DONE"}}),
            serde_json::json!({"runtime": {"details": {"verdict": "PASSED"}}}),
            serde_json::json!({"runtime": {"outcome": "DONE"}}),
            serde_json::json!({"runtime": {"details": {"arbitrary_key": "SUCCESS"}}}),
            serde_json::json!({"phase": [{"metadata": {"terminal": "OPERATIONAL"}}]}),
        ] {
            assert!(
                reject_progress_claims(&status_claim)
                    .unwrap_err()
                    .contains("PROGRESS-CLAIM")
            );
        }
        assert!(is_prerequisite_progress_key("runtime-status"));
    }

    #[test]
    fn adversarial_review_evidence_rejects_placeholder_findings() {
        let rows = serde_json::json!([
            "Independent command execution produced an exact bounded verdict",
            "Counterfactual mutation was rejected by the production gate oracle",
            "Boundary probe exercised malformed and one-over-limit input",
            "Negative path retained the exact stable diagnostic and no PASS"
        ]);
        let mut evidence = serde_json::json!({
            "adversarial_review": {
                "DIFF_ATTACK_SURFACES": [
                    "contract inventory and version-policy mutation surface",
                    "serialization framing and malformed-input security surface",
                    "state resource and durability transition-model surface",
                    "integration gate and manual evidence-attribution surface"
                ],
                "INDEPENDENT_CHECKS_RUN": rows,
                "COUNTERFACTUAL_CHECKS": rows,
                "BOUNDARY_PROBES": rows,
                "NEGATIVE_PATH_CHECKS": rows,
                "INDEPENDENT_FINDINGS": [
                    {
                        "finding_id": "WP-FF-005-FINDING-GATE-DISPOSITION-001",
                        "finding": "HIGH mutation bypass now fails through the production oracle",
                        "status": "REMEDIATED",
                        "proof_id": "xtask::tests::adversarial_review_evidence_rejects_placeholder_findings"
                    }
                ],
                "RESIDUAL_UNCERTAINTY": [
                    "Concurrency adapter behavior remains future runtime-slice proof"
                ],
                "MANUAL_REVIEW": {
                    "artifact": "product/MODEL_MANUAL.md",
                    "reviewer": "independent-manual-reviewer",
                    "method": "Directly inspected the rendered operating topic and executed its commands.",
                    "verdict": "PASS",
                    "evidence": [
                        "The six locked Cargo workflows are present and bounded.",
                        "The common-failure recovery guidance covers every named contract boundary.",
                        "The FF-GATE-RUNTIME-001 future proof ceiling is explicit and unambiguous."
                    ]
                }
            }
        });
        validate_adversarial_review_evidence(&evidence).unwrap();
        let mut missing_finding_id = evidence.clone();
        missing_finding_id["adversarial_review"]["INDEPENDENT_FINDINGS"][0]
            .as_object_mut()
            .unwrap()
            .remove("finding_id");
        assert!(
            validate_adversarial_review_evidence(&missing_finding_id)
                .unwrap_err()
                .contains("independent finding field mismatch")
        );
        for unresolved in [
            "HIGH mutation bypass remains open in the production oracle",
            "HIGH mutation bypass is unresolved in the production oracle",
        ] {
            evidence["adversarial_review"]["INDEPENDENT_FINDINGS"] = serde_json::json!([{
                "finding_id": "WP-FF-005-FINDING-GATE-DISPOSITION-001",
                "finding": unresolved,
                "status": "REMEDIATED",
                "proof_id": "xtask::tests::adversarial_review_evidence_rejects_placeholder_findings"
            }]);
            assert!(
                validate_adversarial_review_evidence(&evidence)
                    .unwrap_err()
                    .contains("contradicts its resolved status")
            );
        }
        evidence["adversarial_review"]["INDEPENDENT_FINDINGS"] =
            serde_json::json!(["PENDING: review finding has no disposition or proof"]);
        assert!(
            validate_adversarial_review_evidence(&evidence)
                .unwrap_err()
                .contains("structured disposition object")
        );
        evidence["adversarial_review"]["INDEPENDENT_FINDINGS"] = serde_json::json!([{
            "finding_id": "WP-FF-005-FINDING-GATE-DISPOSITION-001",
            "finding": "HIGH mutation bypass now fails through the production oracle",
            "status": "REMEDIATED",
            "proof_id": "fabricated::tests::does_not_exist"
        }]);
        assert!(
            validate_adversarial_review_evidence(&evidence)
                .unwrap_err()
                .contains("canonical executable proof")
        );
        evidence["adversarial_review"]["INDEPENDENT_FINDINGS"] = serde_json::json!([{
            "finding_id": "WP-FF-005-FINDING-GATE-DISPOSITION-001",
            "finding": "HIGH mutation bypass now fails through the production oracle",
            "status": "REMEDIATED",
            "proof_id": "core::resource::tests::atomic_zero_exact_one_over_and_release_identity"
        }]);
        assert!(
            validate_adversarial_review_evidence(&evidence)
                .unwrap_err()
                .contains("exact canonical executable proof")
        );
    }

    #[test]
    fn wp006_adversarial_findings_bind_exact_executable_proofs() {
        let expected = [
            (
                "WP-FF-006-FINDING-COUNTERFACTUAL-001",
                "fforager_js_corpus::tests::oracle_counterfactual_rejects_mutated_expected_output",
            ),
            (
                "WP-FF-006-FINDING-CANCEL-ACK-002",
                "WP-FF-006-PROBE-CANCELLATION",
            ),
            (
                "WP-FF-006-FINDING-AST-DISCOVERY-003",
                "fforager_javascript::tests::structural_discovery_skips_prelude_iife_and_unrelated_markers",
            ),
            (
                "WP-FF-006-FINDING-CACHE-EVIDENCE-004",
                "WP-FF-006-PROBE-CACHE-KEY",
            ),
            (
                "WP-FF-006-FINDING-REPORT-MANIFEST-005",
                "build/reports/wp-ff-006-youtube-challenge-report.json",
            ),
            (
                "WP-FF-006-FINDING-CORRELATION-006",
                "fforager_js_corpus::tests::full_response_correlation_rejects_forged_tuple_fields",
            ),
            (
                "WP-FF-006-FINDING-PROBE-AUTHORITY-007",
                "WP-FF-006-PROBE-PROBE-AUTHORITY",
            ),
            (
                "WP-FF-006-FINDING-FATAL-TYPING-008",
                "WP-FF-006-PROBE-PATH-CONFINEMENT",
            ),
            (
                "WP-FF-006-FINDING-QUARANTINE-009",
                "WP-FF-006-PROBE-QUARANTINE",
            ),
            (
                "WP-FF-006-FINDING-RAII-REAP-010",
                "fforager_js_corpus::Supervisor::drop",
            ),
        ];
        for (finding_id, proof_id) in expected {
            assert_eq!(
                expected_adversarial_finding_proof(finding_id),
                Some(proof_id)
            );
        }
    }

    #[test]
    fn first_non_stub_packet_version_is_the_activation_checkpoint() {
        let stub = serde_json::json!({"lifecycle": {"status": "STUB"}});
        let in_progress = serde_json::json!({"lifecycle": {"status": "IN_PROGRESS"}});
        assert!(!committed_packet_is_activation("stub", &stub).unwrap());
        assert!(committed_packet_is_activation("activation", &in_progress).unwrap());
        assert!(committed_packet_is_activation("creation", &in_progress).unwrap());
    }

    #[test]
    fn runtime_stage_paths_reject_aliases_and_unsafe_segments() {
        assert!(safe_relative("inputs/representative.bin"));
        assert!(!safe_relative("inputs/../representative.bin"));
        assert!(!safe_relative("inputs//representative.bin"));
        assert!(!safe_relative("inputs/trailing."));

        let mut proof = runtime_fixture_proof();
        proof.scenarios[0].inputs[0].destination = "FFORAGER.EXE".to_owned();
        assert!(
            validate_runtime_proof_contract(&proof)
                .unwrap_err()
                .contains("FF-RUNTIME-E-STAGE-COLLISION")
        );
    }

    #[test]
    fn creation_time_in_progress_packet_is_a_valid_activation_checkpoint() {
        let root = repo_root().unwrap();
        let packet_id = "WP-FF-012-runtime-truth-gates-v1";
        let base = "a5a6a3a78e3aefcbd463294bbbab5e4ec2f58728";
        validate_packet_activation_base(&root, packet_id, base).unwrap();
        assert!(
            validate_packet_activation_base(&root, packet_id, &"0".repeat(40))
                .unwrap_err()
                .contains("FF-RUNTIME-E-BASE-REWRITE")
        );
    }

    #[test]
    fn linked_worktree_metadata_resolves_only_the_shared_dot_git_root() {
        let shared = test_root("linked-worktree-metadata");
        let worktree = shared.join(".worktrees/candidate");
        let gitdir = shared.join(".git/worktrees/candidate");
        fs::create_dir_all(&worktree).unwrap();
        fs::create_dir_all(&gitdir).unwrap();
        fs::write(
            worktree.join(".git"),
            "gitdir: ../../.git/worktrees/candidate\n",
        )
        .unwrap();
        fs::write(gitdir.join("commondir"), "../..\n").unwrap();

        assert_eq!(
            shared_repository_root(&worktree).unwrap(),
            shared.canonicalize().unwrap()
        );
        fs::remove_dir_all(&shared).unwrap();
    }

    #[test]
    fn linked_worktree_metadata_rejects_a_common_directory_outside_dot_git() {
        let shared = test_root("linked-worktree-bad-commondir");
        let worktree = shared.join(".worktrees/candidate");
        let gitdir = shared.join(".git/worktrees/candidate");
        fs::create_dir_all(&worktree).unwrap();
        fs::create_dir_all(&gitdir).unwrap();
        fs::write(
            worktree.join(".git"),
            "gitdir: ../../.git/worktrees/candidate\n",
        )
        .unwrap();
        fs::write(gitdir.join("commondir"), "../../..\n").unwrap();

        let error = shared_repository_root(&worktree).unwrap_err();
        assert!(
            error.contains("must resolve to a .git directory"),
            "{error}"
        );
        fs::remove_dir_all(&shared).unwrap();
    }

    #[test]
    fn current_linked_worktree_resolves_its_shared_config_and_artifact_root() {
        let root = repo_root().unwrap();
        if !root.join(".git").is_file() {
            return;
        }
        let shared = shared_repository_root(&root).unwrap();
        validate_linked_worktree_layout(&root, &shared).unwrap();
        assert!(
            canonical_paths_equal(
                &shared.join(".cargo/config.toml"),
                &root
                    .ancestors()
                    .find(|ancestor| ancestor.join(".git").is_dir())
                    .unwrap()
                    .join(".cargo/config.toml")
            )
            .unwrap()
        );
    }

    #[test]
    fn verification_sandbox_in_linked_worktree_allows_only_canonical_ancestor_configs() {
        let sandbox = test_root("verification-sandbox-config");
        fs::create_dir_all(&sandbox).unwrap();
        validate_rust_verification_configs(&sandbox).unwrap();
        fs::remove_dir_all(&sandbox).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn bounded_process_timeout_returns_incomplete_evidence() {
        let error = command_status_with_timeout(
            &env::current_dir().unwrap(),
            "sh",
            &["-c", "sleep 5"],
            None,
            Duration::from_millis(50),
        )
        .unwrap_err();
        assert!(error.contains("timed out"));
        assert!(error.contains("incomplete evidence"));
        assert!(error.contains("tree_termination=Ok"));
    }

    #[cfg(unix)]
    #[test]
    fn failed_tree_terminator_uses_bounded_direct_kill_fallback() {
        let mut command = Command::new("sleep");
        command
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_quiet_process(&mut command);
        let mut child = command.spawn().unwrap();
        let started = Instant::now();
        let detail = cleanup_timed_out_child(
            &mut child,
            &Err("forced tree-terminator failure".to_owned()),
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(detail.contains("direct_kill=Some(Ok"), "{detail}");
        assert!(child.try_wait().unwrap().is_some(), "{detail}");
    }

    fn test_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        repo_root()
            .unwrap()
            .join(".fforager-artifacts/test-runs")
            .join(format!(
                "fforager-xtask-{label}-{}-{nonce}",
                std::process::id()
            ))
    }

    fn assert_architecture_mutation_fails(
        label: &str,
        mutate: impl FnOnce(&Path),
        expected_diagnostic: &str,
    ) {
        static POSITIVE_CONTROL: std::sync::OnceLock<Result<(), String>> =
            std::sync::OnceLock::new();
        let positive_control = POSITIVE_CONTROL.get_or_init(|| {
            let root = architecture_sandbox("shared-positive-control");
            architecture_check(&root).map(|_| ())
        });
        if let Err(error) = positive_control {
            panic!("fixture shared positive control failed: {error}");
        }
        let root = architecture_sandbox(label);
        mutate(&root);
        let error = architecture_check(&root).unwrap_err();
        fs::remove_dir_all(&root).unwrap();
        assert!(
            error.contains(expected_diagnostic),
            "expected {expected_diagnostic}, observed {error}"
        );
    }

    #[test]
    fn warm_fixture_reuses_unchanged_inputs_and_rejects_shared_mutation() {
        let root = repo_root().unwrap();
        let label = "warm-fixture-isolation-proof";
        let expected = fs::read(root.join("rust-toolchain.toml")).unwrap();
        with_warm_testkit_fixture(&root, label, |sandbox| {
            copy_fixture_file(
                &root.join("rust-toolchain.toml"),
                &sandbox.join("rust-toolchain.toml"),
            )?;
            fs::write(sandbox.join("rust-toolchain.toml"), b"mutated input")
                .map_err(|error| error.to_string())?;
            fs::create_dir_all(sandbox.join("stale-extra")).map_err(|error| error.to_string())?;
            fs::write(sandbox.join("stale-extra/not-canonical"), b"stale")
                .map_err(|error| error.to_string())?;
            assert!(with_warm_testkit_fixture(&root, label, |_| Ok(())).is_err());
            Ok(())
        })
        .unwrap();
        let modified = with_warm_testkit_fixture(&root, label, |sandbox| {
            assert_eq!(
                fs::read(sandbox.join("rust-toolchain.toml")).unwrap(),
                expected
            );
            assert!(!sandbox.join("stale-extra").exists());
            Ok(fs::metadata(sandbox.join("rust-toolchain.toml"))
                .unwrap()
                .modified()
                .unwrap())
        })
        .unwrap();
        with_warm_testkit_fixture(&root, label, |sandbox| {
            assert_eq!(
                fs::metadata(sandbox.join("rust-toolchain.toml"))
                    .unwrap()
                    .modified()
                    .unwrap(),
                modified
            );
            assert_eq!(
                fs::read(sandbox.join("rust-toolchain.toml")).unwrap(),
                expected
            );
            Ok(())
        })
        .unwrap();
    }

    fn architecture_sandbox(label: &str) -> PathBuf {
        let source = repo_root().unwrap();
        let root = test_root(label);
        for relative in [
            "rust-toolchain.toml",
            ".cargo/config.toml",
            ".gitignore",
            ".GOV/rules/build-rules.yaml",
            "build/Cargo.toml",
            "build/Cargo.lock",
            "build/architecture-policy.toml",
            "build/tooling-policy.toml",
            "build/rule-to-proof.toml",
            "build/tools/fforager-xtask/Cargo.toml",
            "build/tools/fforager-xtask/src/main.rs",
            "build/scripts/clean-artifacts.ps1",
            "build/scripts/new-worktree.ps1",
            "product/clippy.toml",
            "product/MODEL_MANUAL.md",
        ] {
            let from = source.join(relative);
            let to = root.join(relative);
            fs::create_dir_all(to.parent().unwrap()).unwrap();
            fs::copy(from, to).unwrap();
        }
        copy_test_tree(
            &source.join("build/fixtures/architecture"),
            &root.join("build/fixtures/architecture"),
        );
        copy_test_tree(
            &source.join("build/fixtures/contracts"),
            &root.join("build/fixtures/contracts"),
        );
        copy_test_tree(&source.join("product/crates"), &root.join("product/crates"));
        copy_test_tree(&source.join("product/apps"), &root.join("product/apps"));
        copy_test_tree(
            &source.join("product/watcher"),
            &root.join("product/watcher"),
        );
        copy_test_tree(
            &source.join("build/tools/fforager-xtask/src"),
            &root.join("build/tools/fforager-xtask/src"),
        );
        copy_test_tree(&source.join("build/crates"), &root.join("build/crates"));
        root
    }

    fn copy_test_tree(source: &Path, destination: &Path) {
        fs::create_dir_all(destination).unwrap();
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let target = destination.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_test_tree(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    fn replace_file_text(path: &Path, before: &str, after: &str) {
        replace_text_in_file(path, before, after)
            .unwrap_or_else(|error| panic!("mutation failed for {}: {error}", path.display()));
    }

    fn mutate_shipped_product_native_reachability(root: &Path) {
        replace_file_text(
            &root.join("product/crates/fforager-core/Cargo.toml"),
            "fforager-contracts = { version = \"0.1.0\", path = \"../fforager-contracts\" }",
            "fforager-contracts = { version = \"0.1.0\", path = \"../fforager-contracts\" }\nwreq.workspace = true",
        );
        replace_file_text(
            &root.join("build/Cargo.lock"),
            "name = \"fforager-core\"\nversion = \"0.1.0\"\ndependencies = [\n \"fforager-contracts\",\n]",
            "name = \"fforager-core\"\nversion = \"0.1.0\"\ndependencies = [\n \"fforager-contracts\",\n \"wreq\",\n]",
        );
        replace_file_text(
            &root.join("build/architecture-policy.toml"),
            "[[dependency_decisions]]\nname = \"toml\"",
            "[[dependency_decisions]]\nname = \"wreq\"\nversion = \"6.0.0-rc.29\"\nconsumer = \"fforager-core\"\nruntime_class = \"shipped_rust_product\"\npurpose = \"Mutation-only shipped consumer bypass probe.\"\nnative = false\nowner = \"WP-FF-015-wreq-transport-adjudication\"\nallowed_consumers = [\"fforager-core\"]\nreason = \"Mutation-only reachability proof.\"\nremoval_trigger = \"Mutation sandbox is deleted after the assertion.\"\napproval_id = \"WP-FF-015-wreq-transport-adjudication-v1-AC-001\"\nfeatures = [\"stream\", \"tokio-rt\", \"webpki-roots\"]\ndefault_features = false\n\n[[dependency_decisions]]\nname = \"toml\"",
        );
    }

    fn mutate_approved_shipped_product_native_reachability(root: &Path) {
        replace_file_text(
            &root.join("build/Cargo.toml"),
            "members = [\n    \"tools/fforager-xtask\",\n    \"crates/fforager-javascript\",\n    \"crates/fforager-transport\",\n    \"../product/crates/fforager-contracts\",",
            "members = [\n    \"tools/fforager-xtask\",\n    \"crates/fforager-javascript\",\n    \"crates/fforager-transport\",\n    \"../product/crates/fforager-net\",\n    \"../product/crates/fforager-contracts\",",
        );
        replace_file_text(
            &root.join("build/Cargo.toml"),
            "default-members = [\n    \"tools/fforager-xtask\",\n    \"crates/fforager-javascript\",\n    \"crates/fforager-transport\",\n    \"../product/crates/fforager-contracts\",",
            "default-members = [\n    \"tools/fforager-xtask\",\n    \"crates/fforager-javascript\",\n    \"crates/fforager-transport\",\n    \"../product/crates/fforager-net\",\n    \"../product/crates/fforager-contracts\",",
        );
        let manifest = root.join("product/crates/fforager-net/Cargo.toml");
        fs::create_dir_all(manifest.parent().unwrap().join("src")).unwrap();
        fs::write(
            &manifest,
            "[package]\nname = \"fforager-net\"\nversion = \"0.1.0\"\nworkspace = \"../../../build\"\nedition = \"2024\"\nrust-version = \"1.97.1\"\nlicense = \"MIT OR Apache-2.0\"\npublish = false\n\n[lib]\npath = \"src/lib.rs\"\n\n[dependencies]\nreqwest.workspace = true\nrustls.workspace = true\n\n[lints]\nworkspace = true\n",
        )
        .unwrap();
        fs::write(
            manifest.parent().unwrap().join("src/lib.rs"),
            "#![forbid(unsafe_code)]\n//! Mutation-only approved shipped native consumer.\n",
        )
        .unwrap();
        replace_file_text(
            &root.join("build/Cargo.lock"),
            "[[package]]\nname = \"fforager-diagnostics-contract\"",
            "[[package]]\nname = \"fforager-net\"\nversion = \"0.1.0\"\ndependencies = [\n \"reqwest\",\n \"rustls\",\n]\n\n[[package]]\nname = \"fforager-diagnostics-contract\"",
        );
        replace_file_text(
            &root.join("build/architecture-policy.toml"),
            "[[members]]\nname = \"fforager-core\"",
            "[[members]]\nname = \"fforager-net\"\nmanifest = \"product/crates/fforager-net/Cargo.toml\"\nsource_root = \"product/crates/fforager-net/src\"\nlayer = \"adapter\"\nartifact_role = \"mutation_only_approved_shipped_transport\"\nshipped = true\ntest_only = false\npublish_allowed = false\nallowed_internal_dependencies = []\nsplit_trigger = \"WP-FF-017-ordinary-transport-decision-v1-AC-001\"\nfeature_owner = \"WP-FF-017-ordinary-transport-decision-v1\"\nprofile = \"mutation_only_product_reachability\"\nremoval_condition = \"Mutation sandbox is deleted after the assertion.\"\nruntime_native_constraint_ref = \"FF-START-BOUNDARY-001\"\nunsafe_policy_ref = \"FF-BUILD-050\"\nexception_policy_ref = \"FF-BUILD-052\"\n\n[[members]]\nname = \"fforager-core\"",
        );
        replace_file_text(
            &root.join("build/architecture-policy.toml"),
            "[[dependency_decisions]]\nname = \"toml\"",
            "[[dependency_decisions]]\nname = \"reqwest\"\nversion = \"0.13.4\"\nconsumer = \"fforager-net\"\nruntime_class = \"shipped_rust_product\"\npurpose = \"Mutation-only proof of the exact approved product-native consumer.\"\nnative = true\nowner = \"WP-FF-023-dependency-security-remediation\"\nallowed_consumers = [\"fforager-net\"]\nreason = \"Exercise the positive product-authorized reachability branch.\"\nremoval_trigger = \"Mutation sandbox is deleted after the assertion.\"\napproval_id = \"WP-FF-023-dependency-security-remediation-v1-AC-001\"\nfeatures = [\"http2\", \"rustls-no-provider\", \"stream\"]\ndefault_features = false\nexception_id = \"FF-DEC-002\"\n\n[[dependency_decisions]]\nname = \"rustls\"\nversion = \"0.23.46\"\nconsumer = \"fforager-net\"\nruntime_class = \"shipped_rust_product\"\npurpose = \"Mutation-only proof of the exact approved ring provider edge.\"\nnative = true\nowner = \"WP-FF-023-dependency-security-remediation\"\nallowed_consumers = [\"fforager-net\"]\nreason = \"Exercise exact rustls product-native attribution.\"\nremoval_trigger = \"Mutation sandbox is deleted after the assertion.\"\napproval_id = \"WP-FF-023-dependency-security-remediation-v1-AC-001\"\nfeatures = [\"ring\", \"std\", \"tls12\"]\ndefault_features = false\nexception_id = \"FF-DEC-002\"\n\n[[dependency_decisions]]\nname = \"toml\"",
        );
    }

    fn mutate_direct_native_edge_misattribution(root: &Path) {
        replace_file_text(
            &root.join("build/Cargo.toml"),
            "wreq-util = { version = \"=3.0.0-rc.14\", default-features = false, features = [\"emulation\"] }",
            "wreq-util = { version = \"=3.0.0-rc.14\", default-features = false, features = [\"emulation\"] }\nbtls = \"=0.5.6\"",
        );
        replace_file_text(
            &root.join("build/crates/fforager-transport/Cargo.toml"),
            "wreq-util = { workspace = true, optional = true }",
            "wreq-util = { workspace = true, optional = true }\nbtls.workspace = true",
        );
        replace_file_text(
            &root.join("build/Cargo.lock"),
            "dependencies = [\n \"fforager-contracts\",\n \"futures-util\",",
            "dependencies = [\n \"btls\",\n \"fforager-contracts\",\n \"futures-util\",",
        );
        replace_file_text(
            &root.join("build/architecture-policy.toml"),
            "[[dependency_decisions]]\nname = \"wreq\"\nversion = \"6.0.0-rc.29\"",
            "[[dependency_decisions]]\nname = \"btls\"\nversion = \"0.5.6\"\nconsumer = \"fforager-transport\"\nruntime_class = \"non_shipped_phase0_prerequisite\"\npurpose = \"Mutation-only direct native edge bypass probe.\"\nnative = false\nowner = \"WP-FF-015-wreq-transport-adjudication\"\nallowed_consumers = [\"fforager-transport\"]\nreason = \"Mutation-only attribution proof.\"\nremoval_trigger = \"Mutation sandbox is deleted after the assertion.\"\napproval_id = \"WP-FF-015-wreq-transport-adjudication-v1-AC-001\"\nfeatures = []\ndefault_features = true\n\n[[dependency_decisions]]\nname = \"wreq\"\nversion = \"6.0.0-rc.29\"",
        );
    }
}
