mod compatibility;

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
const PUBLIC_BOUNDARY_COUNTEREXAMPLE_TEST: &str =
    "tests::public_boundary_counterexamples_reject_audit_failures";
const PUBLIC_BOUNDARY_COUNTEREXAMPLE_PROOF_ID: &str =
    "testkit::tests::public_boundary_counterexamples_reject_audit_failures";
const PUBLIC_BOUNDARY_COUNTEREXAMPLE_RECEIPT: &str = "FF-PUBLIC-COUNTEREXAMPLE-RECEIPT:v5:source-graph-cycle,filesystem-effect-correlation,ffmpeg-terminal-release,ffmpeg-partial-unsuccessful-outcomes,schema-authority,sequence-zero,unknown-envelope-field,nested-wire-unknown-fields,durable-journal-payload-unknown-field,durable-journal-record-unknown-field,durable-reconcile-state-unknown-field,durable-journal-sequence-zero,acknowledged-effect-prefixes";
const TOOL_COMMAND_TIMEOUT: Duration = Duration::from_mins(1);
const CARGO_PROOF_COMMAND_TIMEOUT: Duration = Duration::from_mins(5);
const METADATA_COMMAND_TIMEOUT: Duration = Duration::from_mins(2);
const GATE_COMMAND_TIMEOUT: Duration = Duration::from_mins(15);
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
    allowed_consumers: BTreeSet<String>,
    allowed_direct_dependencies: BTreeSet<String>,
    allowed_versions: BTreeSet<String>,
    allowed_native_link_packages: BTreeSet<String>,
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

#[derive(Debug, Serialize)]
struct SourceState {
    git_commit: String,
    dirty: bool,
    dirty_paths: Vec<String>,
    content_fingerprint: String,
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

#[derive(Debug, Serialize)]
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
        [gate] if matches!(gate.as_str(), "verify-release" | "watcher-check") => {
            Err(format!("{gate} is NOT_IMPLEMENTED for Phase 0 and cannot report PASS"))
        }
        _ => Err("usage: fforager-xtask <architecture-check|runtime-truth-check --evidence-from-taskboard|verify-pr --evidence-from-taskboard|verify-deep --evidence-from-taskboard|transport-corpus|compatibility-generate --oracle-exe PATH --source-root PATH [--output PATH]|compatibility-validate|compatibility-replay [--shard INDEX/TOTAL]|compatibility-diff --candidate PATH|compatibility-inventory-diff --before PATH --after PATH|compatibility-live-canaries --enable-live --oracle-exe PATH|verify-release|watcher-check>".to_owned()),
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
    let architecture = run_verify_deep_checks(root, &mut checks, &mut transport_report)?;
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
            "Concrete network, storage, FFmpeg, JavaScript, plugin, scheduler, watcher, and archive adapters remain future consumers of these contracts and models.".to_owned(),
        ],
        artifacts: vec![
            "build/fixtures/contracts/inventory.json".to_owned(),
            "build/reports".to_owned(),
            transport_report,
            "build/target".to_owned(),
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
) -> Result<ArchitectureResult, String> {
    verify_tool_identities(root, checks)?;
    run_rust_verification(root, checks)?;
    execute_public_boundary_counterexample_test(root, checks)?;
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
        "locked workspace, contract inventory, model manual, data-only scan, public-boundary counterexamples, transport corpus, doctests, and canonical architecture rules were executed; active-packet acceptance attribution remains external evidence work",
    ));
    Ok(architecture)
}

fn run_transport_corpus(root: &Path) -> Result<(), String> {
    let mut checks = Vec::new();
    let report = execute_transport_corpus(root, &mut checks)?;
    println!("COMPLETE WP-FF-007 transport corpus; report={report}");
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
    if entries.len() != 20 || states.len() != 12 {
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
            if let Some(token) = forbidden_data_model_token(&source) {
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

fn architecture_check(root: &Path) -> Result<ArchitectureResult, String> {
    let policy: ArchitecturePolicy = read_toml(&root.join("build/architecture-policy.toml"))?;
    let native_exceptions =
        read_native_dependency_exceptions(&root.join(&policy.exception_authority))?;
    validate_policy_identity(&policy, &native_exceptions)?;
    let tooling: ToolingPolicy = read_toml(&root.join("build/tooling-policy.toml"))?;
    validate_tooling_policy(&tooling)?;
    validate_current_host(root, &tooling)?;
    let rule_map: RuleMap = read_toml(&root.join("build/rule-to-proof.toml"))?;
    validate_rule_map_identity(&rule_map)?;
    let metadata = cargo_metadata(root, &policy)?;
    let mut checks = Vec::new();
    validate_workspace(root, &policy, &metadata, &mut checks)?;
    validate_internal_graph(&policy, &metadata, &mut checks)?;
    validate_three_roots(root, &policy, &mut checks)?;
    validate_dependencies(&policy, &metadata, &native_exceptions, &mut checks)?;
    let canonical_rules = required_architecture_rules(&root.join(".GOV/rules/build-rules.yaml"))?;
    validate_rule_map(&rule_map, &canonical_rules, &mut checks)?;
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
        (policy.target_directory.as_str(), "build/target"),
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
    if policy.unsafe_policy != "forbid_in_workspace_members"
        || policy.unsafe_exception_authority != ".GOV/rules/build-rules.yaml#exception_authority"
        || !policy.unsafe_decision_ids.is_empty()
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

fn cargo_metadata(root: &Path, policy: &ArchitecturePolicy) -> Result<Metadata, String> {
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
        ],
        METADATA_COMMAND_TIMEOUT,
    )?;
    serde_json::from_str(&output).map_err(|error| format!("parse locked Cargo metadata: {error}"))
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
        "locked resolver-3 workspace members exactly match policy, inherit the pinned package contract, and use build/target output",
    ));
    Ok(())
}

fn validate_member_metadata(
    root: &Path,
    policy: &ArchitecturePolicy,
    member: &MemberPolicy,
    metadata: &Metadata,
) -> Result<(), String> {
    if member.shipped && member.layer == "build_tooling" {
        return Err(format!("FF-ARCH-E-SHIPPED-BUILD-TOOLING: {}", member.name));
    }
    let ownership_root = if member.shipped {
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
    if member.layer.trim().is_empty()
        || member.artifact_role.trim().is_empty()
        || member.feature_owner.trim().is_empty()
        || member.profile.trim().is_empty()
        || member.removal_condition.trim().is_empty()
        || member.runtime_native_constraint_ref != "FF-START-BOUNDARY-001"
        || member.unsafe_policy_ref != "FF-BUILD-050"
        || member.exception_policy_ref != "FF-BUILD-052"
        || member.publish_allowed
        || (member.shipped && member.test_only)
        || (!member.shipped && !member.test_only && member.layer != "build_tooling")
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
    Ok(lints.get("workspace").is_none()
        && rust.get("unsafe_code").and_then(toml::Value::as_str) == Some("forbid")
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
    if policy.forbidden_product_runtime_roots != [".GOV", "build"] {
        return Err("product runtime boundary must forbid .GOV and build".to_owned());
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
    let generated_target = root.join("build/target");
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
        || product_oracle_documentation_exception(relative, bytes)
    {
        return None;
    }
    product_oracle_path_diagnostic(relative)
        .or_else(|| product_oracle_source_diagnostic(&String::from_utf8_lossy(bytes)))
        .or_else(|| {
            product_documentation_asset_reference_diagnostic(&String::from_utf8_lossy(bytes))
        })
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
    for manifest in [
        "product/crates/fforager-contracts/Cargo.toml",
        "product/crates/fforager-diagnostics-contract/Cargo.toml",
        "product/crates/fforager-core/Cargo.toml",
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
        .map(|member| member.name.as_str())
        .collect::<BTreeSet<_>>();
    for decision in decisions.values() {
        if !members.contains(decision.consumer.as_str())
            || !matches!(
                decision.runtime_class.as_str(),
                "non_shipped_build_tooling"
                    | "non_shipped_test_tooling"
                    | "non_shipped_phase0_prerequisite"
                    | "non_shipped_phase0_proof_telemetry"
                    | "shipped_rust_product"
            )
            || decision.purpose.trim().is_empty()
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
    if !decision.native {
        return decision.exception_id.is_none();
    }
    let Some(exception_id) = decision.exception_id.as_deref() else {
        return false;
    };
    let Some(exception) = native_exceptions.get(exception_id) else {
        return false;
    };
    exception.owner == decision.owner
        && exception.approval_id == decision.approval_id
        && exception.allowed_consumers.contains(&decision.consumer)
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
    validate_native_supply_chain(metadata, &expected_native_links)?;
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
        let governed_package_ids = metadata
            .packages
            .iter()
            .filter(|package| {
                let identity = format!("{}@{}", package.name, package.version);
                exception.allowed_versions.contains(&identity)
                    || exception.allowed_native_link_packages.contains(&identity)
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
            if member.shipped
                || member.manifest.starts_with("product/")
                || !exception.allowed_consumers.contains(&member.name)
            {
                return Err(format!(
                    "FF-ARCH-E-NATIVE-CONSUMER-REACHABILITY: {} reaches {exception_id} but is not an allowed non-shipped consumer",
                    member.name
                ));
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

fn validate_native_supply_chain(
    metadata: &Metadata,
    expected_native_links: &BTreeSet<String>,
) -> Result<(), String> {
    let forbidden_environment = std::env::vars_os()
        .filter_map(|(name, _)| name.into_string().ok())
        .filter(|name| name.to_ascii_uppercase().contains("BORING_BSSL"))
        .collect::<BTreeSet<_>>();
    if !forbidden_environment.is_empty() {
        return Err(format!(
            "FF-ARCH-E-NATIVE-SOURCE-OVERRIDE: BoringSSL environment overrides are forbidden: {forbidden_environment:?}"
        ));
    }
    let resolve = metadata
        .resolve
        .as_ref()
        .ok_or("Cargo metadata omitted resolve graph")?;
    for identity in expected_native_links {
        let Some((name, version)) = identity.rsplit_once('@') else {
            return Err(format!(
                "FF-ARCH-E-NATIVE-SOURCE: invalid native package identity {identity}"
            ));
        };
        if !matches!(name, "boring-sys2" | "btls-sys") {
            continue;
        }
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
        validate_boringssl_source_dir(identity, manifest_dir, &selected_features)?;
    }
    Ok(())
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

fn read_native_dependency_exceptions(
    path: &Path,
) -> Result<BTreeMap<String, NativeDependencyException>, String> {
    let text = fs::read_to_string(path).map_err(|error| {
        format!(
            "read native exception authority {}: {error}",
            path.display()
        )
    })?;
    let document = parse_single_yaml(&text, &path.display().to_string())?;
    let authority = document
        .as_mapping_get("exception_authority")
        .ok_or("build rules omit exception_authority")?;
    let rows = authority
        .as_mapping_get("canonical_allowlist")
        .and_then(Yaml::as_sequence)
        .ok_or("exception_authority canonical_allowlist is not a sequence")?;
    let expected_keys = BTreeSet::from([
        "allow_build_scripts",
        "allow_build_time_downloads",
        "allow_packaging_or_release",
        "allow_prebuilt_binaries",
        "allow_product_use",
        "allowed_consumers",
        "allowed_direct_dependencies",
        "allowed_native_link_packages",
        "allowed_runtime_classes",
        "allowed_versions",
        "approval_id",
        "id",
        "kind",
        "owner",
        "promotion_requires_new_operator_decision",
        "reason",
        "removal_trigger",
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
        if keys != expected_keys {
            return Err(format!(
                "FF-ARCH-E-EXCEPTION-SCHEMA: native exception keys are invalid: {keys:?}"
            ));
        }
        let id = yaml_string(row, "id")?;
        let owner = yaml_string(row, "owner")?;
        let approval_id = yaml_string(row, "approval_id")?;
        if !id.starts_with("FF-DEC-")
            || yaml_string(row, "kind")? != "native_dependency"
            || yaml_string(row, "status")? != "ACTIVE_NON_SHIPPED_ADJUDICATION_ONLY"
            || !work_packet_base_owner_valid(owner)
            || !decision_approval_matches_owner(approval_id, owner)
            || !yaml_bool(row, "allow_build_scripts")?
            || yaml_bool(row, "allow_build_time_downloads")?
            || yaml_bool(row, "allow_prebuilt_binaries")?
            || yaml_bool(row, "allow_product_use")?
            || yaml_bool(row, "allow_packaging_or_release")?
            || !yaml_bool(row, "promotion_requires_new_operator_decision")?
            || yaml_string(row, "reason")?.trim().is_empty()
            || yaml_string(row, "removal_trigger")?.trim().is_empty()
        {
            return Err(format!(
                "FF-ARCH-E-EXCEPTION-SCHEMA: native exception {id} violates the bounded policy"
            ));
        }
        let exception = NativeDependencyException {
            id: id.to_owned(),
            owner: owner.to_owned(),
            approval_id: approval_id.to_owned(),
            allowed_consumers: yaml_string_set(row, "allowed_consumers")?,
            allowed_direct_dependencies: yaml_string_set(row, "allowed_direct_dependencies")?,
            allowed_versions: yaml_string_set(row, "allowed_versions")?,
            allowed_native_link_packages: yaml_string_set(row, "allowed_native_link_packages")?,
            allowed_runtime_classes: yaml_string_set(row, "allowed_runtime_classes")?,
        };
        if exception.allowed_consumers.is_empty()
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
                &["structural", "semantic", "negative_fixture"],
                &["compatibility-semantic-replay"],
                &[
                    "structural-replay-behavioral-pass",
                    "source-content-fingerprint",
                ],
            ),
            "FF-BUILD-091" => (
                &["semantic", "counterfactual", "negative_fixture"],
                &["public-boundary-counterexample"],
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
                &["semantic", "negative_fixture"],
                &["behavior-sensitive-proof-map"],
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
                &["proof-class-aggregation"],
                &["proof-class-promotion", "gate-report-runtime-claim"],
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
            "state_effect",
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
                "build/target/compiler-guard-probe",
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
    let target_dir = root.join("build/target/proof-integrity-fixtures/contract-inventory-resolver");
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
    let target_dir = root.join("build/target/proof-integrity-fixtures/contract-inventory-behavior");
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
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let sandbox = root
        .join("build/target/proof-integrity-fixtures")
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

fn public_invariant_mutation_fixture_execution(
    root: &Path,
    mutation: &str,
    diagnostic: &str,
    proof_class: &'static str,
) -> Result<FixtureExecution, String> {
    let target_dir = root.join("build/target/proof-integrity-fixtures/public-invariant-mutations");
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
    let target_dir = root.join("build/target/proof-integrity-fixtures/compiled-testkit");
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
    let target_dir = root.join("build/target/proof-integrity-fixtures/public-receipt-only");
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
    ] {
        copy_fixture_tree(&source_root.join(relative), &sandbox.join(relative))?;
    }
    let manifest = r#"[workspace]
members = [
    "crates/fforager-testkit",
    "../product/crates/fforager-contracts",
    "../product/crates/fforager-diagnostics-contract",
    "../product/crates/fforager-core",
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
    fs::write(&path, manifest).map_err(|error| error.to_string())?;
    fs::copy(
        source_root.join("build/Cargo.lock"),
        sandbox.join("build/Cargo.lock"),
    )
    .map_err(|error| format!("copy isolated workspace lockfile: {error}"))?;
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
            fs::copy(entry.path(), target).map_err(|error| error.to_string())?;
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
    let policy: ArchitecturePolicy = read_toml(&root.join("build/architecture-policy.toml"))?;
    let current_has_shipped_member = policy.members.iter().any(|member| member.shipped);
    let base_policy_text = command_output(
        root,
        "git",
        &["show", &format!("{base}:build/architecture-policy.toml")],
    )?;
    let base_policy: ArchitecturePolicy = toml::from_str(&base_policy_text)
        .map_err(|error| format!("FF-RUNTIME-E-BASE-POLICY: {error}"))?;
    let base_has_shipped_member = base_policy.members.iter().any(|member| member.shipped);
    let has_shipped_member = current_has_shipped_member || base_has_shipped_member;
    let product_paths = changed
        .iter()
        .filter(|path| product_affecting_path(path, has_shipped_member))
        .cloned()
        .collect::<Vec<_>>();
    if product_paths.is_empty() {
        if impact != "NONE" {
            return Err(format!(
                "FF-RUNTIME-E-IMPACT-MISMATCH: packet declares {impact} but no shipped product path changed"
            ));
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
        return Ok(RuntimeTruthResult {
            checks: prerequisite_runtime_checks(&id, &product_paths),
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

fn prerequisite_runtime_checks(id: &str, product_paths: &[String]) -> Vec<Check> {
    vec![
        pass_with_class(
            "runtime-impact",
            "policy",
            "active packet impact and changed-path classification",
            &format!(
                "packet {id} changes product paths only as a predeclared Phase 0 non-product prerequisite: {product_paths:?}"
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
    reject_progress_claims_at(value, false)
}

fn reject_progress_claims_at(value: &Value, progress_subject_in_scope: bool) -> Result<(), String> {
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
                reject_progress_claims_at(child, child_progress_subject)?;
            }
        }
        Value::Array(values) => {
            for child in values {
                reject_progress_claims_at(child, progress_subject_in_scope)?;
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
                .filter(|(_, token)| is_completion_status(token))
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
        || ["product", "runtime", "packaging", "release", "phase"]
            .iter()
            .any(|subject| tokens.iter().any(|token| token == subject))
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
    for (subject, _) in tokens.iter().enumerate().filter(|(_, token)| {
        matches!(
            token.as_str(),
            "product" | "runtime" | "packaging" | "release" | "phase"
        )
    }) {
        for (status, _) in tokens
            .iter()
            .enumerate()
            .filter(|(_, token)| is_completion_status(token))
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

fn pronoun_asserts_completion(tokens: &[String]) -> bool {
    tokens.first().is_some_and(|token| {
        matches!(token.as_str(), "it" | "this" | "that")
            && tokens.iter().enumerate().any(|(status, token)| {
                is_completion_status(token) && !claim_subject_is_negated(tokens, 0, 1, status)
            })
    })
}

fn claim_tokens(text: &str) -> Vec<String> {
    text.to_ascii_lowercase()
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(ToOwned::to_owned)
        .collect()
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
        "build/target".to_owned(),
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
        .join("build/target/release")
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
        .join("build/target/release")
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
    let stage = env::temp_dir()
        .join("ferric-forager-runtime-proof")
        .join(format!("{}-{nonce}-{}", scenario.id, std::process::id()));
    fs::create_dir_all(&stage)
        .map_err(|error| format!("FF-RUNTIME-E-CLEAN-STAGE: create stage: {error}"))?;
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("FF-RUNTIME-E-CLEAN-STAGE: canonicalize root: {error}"))?;
    let canonical_stage = stage
        .canonicalize()
        .map_err(|error| format!("FF-RUNTIME-E-CLEAN-STAGE: canonicalize stage: {error}"))?;
    if canonical_stage.starts_with(&canonical_root) {
        return Err(
            "FF-RUNTIME-E-CLEAN-STAGE: runtime stage must be outside the repository tree"
                .to_owned(),
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

fn run_verify_pr_inner(root: &Path, gate_args: &[String]) -> Result<(), String> {
    let mut checks = Vec::new();
    validate_change_evidence(root, &mut checks)?;
    let mut transport_report = String::new();
    let architecture = run_verify_deep_checks(root, &mut checks, &mut transport_report)?;
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
            "build/target",
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
    checks.extend(runtime.checks);
    verify_future_gate_applicability(root, &mut checks)?;
    let mut declared_supported_proof_classes = architecture.declared_supported_proof_classes;
    declared_supported_proof_classes.extend(runtime.proof_classes);
    declared_supported_proof_classes.sort();
    declared_supported_proof_classes.dedup();
    let mut proof_limitations = architecture.limitations;
    proof_limitations.extend(runtime.limitations);
    let mut artifacts = vec![
        "build/target".to_owned(),
        "build/reports".to_owned(),
        transport_report,
    ];
    artifacts.extend(runtime.artifacts);
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
        return Err(
            "watcher-check is applicable because product/watcher/Cargo.toml exists, but the gate is NOT_IMPLEMENTED"
                .to_owned(),
        );
    }
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
        args.extend(["--target-dir", "build/target"]);
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
            "build/target",
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
            "build/target",
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
            "build/target",
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
        .pointer("/extensions/refinement")
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

fn source_state(root: &Path) -> Result<SourceState, String> {
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

fn source_states_equal(left: &SourceState, right: &SourceState) -> bool {
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
    command_output_with_timeout(root, program, args, CARGO_PROOF_COMMAND_TIMEOUT)
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
    let mut command = Command::new(program);
    command.args(args).current_dir(root).stdin(Stdio::null());
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
    let capture_root = root.join("build/target/command-capture");
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
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(root)
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
    let present = config_paths
        .into_iter()
        .filter(|path| path.exists())
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    if present.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "FF-ARCH-E-RUST-ENV-OVERRIDE: verification refuses effective Cargo configuration files: {present:?}"
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
    let bytes = serde_json::to_vec_pretty(report).map_err(|e| e.to_string())?;
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
    if report.schema_id != "ff.gate-report@1" || report.schema_version != "1.0.0" {
        return Err("FF-ARCH-E-GATE-REPORT-SCHEMA: invalid gate report identity".to_owned());
    }
    let expected_args: &[&str] = match report.gate_id.as_str() {
        ARCH_GATE => &["architecture-check"],
        DEEP_GATE => &["verify-deep", "--evidence-from-taskboard"],
        PR_GATE => &["verify-pr", "--evidence-from-taskboard"],
        RUNTIME_GATE => &["runtime-truth-check", "--evidence-from-taskboard"],
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
    if report.executed_proof_classes.iter().any(|class| {
        matches!(
            class.as_str(),
            "production_runtime" | "runtime_observable" | "runtime_fault"
        )
    }) {
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
    fn slash_normalizes_windows_separator() {
        assert_eq!(slash(Path::new("build\\target")), "build/target");
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
        assert!(pr.contains("run_verify_deep_checks(root, &mut checks, &mut transport_report)?;"));
        assert!(!pr.contains("checks.push(pass(\n        \"verify-deep\""));
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
                "build/target/research-oracle/yt-dlp.exe",
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
        for directory in [".GOV", "product/nested", "build/target"] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        fs::write(root.join("rust-toolchain.toml"), "[toolchain]\n").unwrap();
        fs::write(
            root.join("product/nested/rust-toolchain.toml"),
            "[toolchain]\n",
        )
        .unwrap();
        fs::write(
            root.join("build/target/rust-toolchain.toml"),
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
                    "exception_decision_ids = [\"FF-DEC-001\"]",
                    "exception_decision_ids = [\"SELF-AUTHORIZED\"]",
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
    fn native_exception_mutations_reach_production_validation() {
        assert_architecture_mutation_fails(
            "native-exception-wrong-consumer",
            |root| {
                replace_file_text(
                    &root.join(".GOV/rules/build-rules.yaml"),
                    "allowed_consumers: [fforager-transport]",
                    "allowed_consumers: [fforager-xtask]",
                );
            },
            "FF-ARCH-E-UNAPPROVED-NATIVE-DEPENDENCY",
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
            "FF-ARCH-E-NATIVE-LINK-SURFACE",
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
            "native-exception-shipped-product-reachability",
            mutate_shipped_product_native_reachability,
            "FF-ARCH-E-NATIVE-CONSUMER-REACHABILITY",
        );
    }

    #[test]
    fn native_exception_shipped_product_reachability_rejects_actual_resolve_graph() {
        let root = architecture_sandbox("native-exception-shipped-product-resolve-graph");
        mutate_shipped_product_native_reachability(&root);
        let policy: ArchitecturePolicy =
            read_toml(&root.join("build/architecture-policy.toml")).unwrap();
        let native_exceptions =
            read_native_dependency_exceptions(&root.join(&policy.exception_authority)).unwrap();
        let metadata = cargo_metadata(&root, &policy).unwrap();
        let error = validate_native_exception_reachability(&policy, &metadata, &native_exceptions)
            .unwrap_err();
        fs::remove_dir_all(&root).unwrap();
        assert!(
            error.contains("FF-ARCH-E-NATIVE-CONSUMER-REACHABILITY"),
            "expected shipped-product reachability rejection, observed {error}"
        );
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
    fn product_path_classifier_excludes_manual_but_not_runtime_code() {
        assert!(!product_runtime_path("product/MODEL_MANUAL.md"));
        assert!(product_runtime_path("product/fforager/src/main.rs"));
        assert!(product_runtime_path("product\\watcher\\src\\main.rs"));
        assert!(!product_affecting_path("build/Cargo.lock", false));
        assert!(product_affecting_path("build/Cargo.lock", true));
        assert!(product_affecting_path("rust-toolchain.toml", true));
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
        env::temp_dir().join(format!(
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

    fn architecture_sandbox(label: &str) -> PathBuf {
        let source = repo_root().unwrap();
        let root = test_root(label);
        for relative in [
            "rust-toolchain.toml",
            ".GOV/rules/build-rules.yaml",
            "build/Cargo.toml",
            "build/Cargo.lock",
            "build/architecture-policy.toml",
            "build/tooling-policy.toml",
            "build/rule-to-proof.toml",
            "build/tools/fforager-xtask/Cargo.toml",
            "build/tools/fforager-xtask/src/main.rs",
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
        let text = fs::read_to_string(path).unwrap();
        assert!(text.contains(before), "missing mutation anchor {before}");
        fs::write(path, text.replacen(before, after, 1)).unwrap();
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
}
