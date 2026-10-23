use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::{
    command_output_bytes_with_timeout, command_output_with_timeout, sha256_file, slash,
    source_state,
};

const ORACLE_PATH: &str = "build/fixtures/compatibility/yt-dlp-2026.07.04/oracle-manifest.json";
const PROFILE_PATH: &str = "build/fixtures/compatibility/yt-dlp-2026.07.04/profile.json";
const CORPUS_PATH: &str = "build/fixtures/compatibility/corpus-v1/manifest.json";
const LIVE_PATH: &str = "build/fixtures/compatibility/live-canaries-v1/manifest.json";
const NEGATIVE_ROOT: &str = "build/fixtures/compatibility/negative";
const NORMALIZATION_VERSION: &str = "ff-normalization-v1";
const ORACLE_VERSION: &str = "2026.07.04";
const ORACLE_TAG_COMMIT: &str = "fdec00e0bf530dc6c3cc7b1dd780e95d9ae460e9";
const ORACLE_RELEASE_HEAD: &str = "997fa140840a08df3938b40da470c78049fef1f6";
const ORACLE_EXE_SHA256: &str = "52fe3c26dcf71fbdc85b528589020bb0b8e383155cfa81b64dd447bbe35e24b8";
const ORACLE_MANIFEST_SHA256: &str =
    "a11c4914b1094e9329d1aa9543671f74b3495cdee69d73341010c59ee07dd2c1";
const PROFILE_SHA256: &str = "f64d65c1c24bece65a641d4e305710a9760de3367f7f7f83bbac5f67fb79f55c";
const CORPUS_MANIFEST_SHA256: &str =
    "1016c7836f3b0d6660105abf13a639376f457f51121f709c98a27b0b161f6c78";
const LIVE_MANIFEST_SHA256: &str =
    "3808834a75c3d6f368618e740e2dbcac6fdab610b42396a39c7a8863b4522321";
const MAX_JSON_BYTES: u64 = 16 * 1024 * 1024;
const ACCEPTED_DIVERGENCE_DECISIONS: &[&str] = &[];
const SANITIZED_PLACEHOLDERS: &[&str] = &[
    "AUTHORIZATION",
    "COOKIE",
    "QUERY_TOKEN",
    "RANDOMIZED_SEARCH_EXAMPLE",
    "RANDOM_SEED",
    "SET_COOKIE",
    "TIMESTAMP",
];
const REQUIRED_NEGATIVE_FIXTURES: &[&str] = &[
    "accepted-divergence-without-decision",
    "deterministic-network",
    "duplicate-shard-case",
    "duplicate-stable-id",
    "invalid-candidate-digest",
    "missing-inventory-row",
    "nondeterministic-offline-classification",
    "self-consistent-corpus-change",
    "self-consistent-inventory-removal",
    "unauthorized-divergence-decision",
    "unknown-normalization",
    "unpinned-oracle",
    "unpinned-oracle-artifact",
    "unsafe-oracle-command",
    "unsanitized-secret",
    "unstable-id",
];
const TOOL_TIMEOUT: Duration = Duration::from_mins(1);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OracleManifest {
    schema_id: String,
    schema_version: String,
    oracle_id: String,
    release: OracleRelease,
    artifacts: Vec<OracleArtifact>,
    source_inputs: Vec<SourceInput>,
    generator: GeneratorPolicy,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OracleRelease {
    project: String,
    version: String,
    channel: String,
    immutable: bool,
    published_at_utc: String,
    release_url: String,
    tag_commit: String,
    release_git_head: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OracleArtifact {
    artifact_id: String,
    filename: String,
    url: String,
    sha256: String,
    size_bytes: u64,
    role: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SourceInput {
    path: String,
    sha256: String,
    role: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GeneratorPolicy {
    normalization_version: String,
    network_allowed: bool,
    plugins_allowed: bool,
    config_allowed: bool,
    timeout_seconds: u64,
    commands: Vec<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CompatibilityProfile {
    schema_id: String,
    schema_version: String,
    profile_id: String,
    oracle_id: String,
    normalization_version: String,
    source_identity: ProfileSourceIdentity,
    input_digests: BTreeMap<String, String>,
    counts: ProfileCounts,
    options: Vec<OptionRow>,
    presets: Vec<PresetRow>,
    interactions: Vec<InteractionRow>,
    extractors: Vec<ExtractorRow>,
    extractor_descriptions: Vec<DescriptionRow>,
    normalization_notes: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProfileSourceIdentity {
    version: String,
    tag_commit: String,
    release_git_head: String,
    executable_sha256: String,
    source_inputs: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProfileCounts {
    options: usize,
    presets: usize,
    interactions: usize,
    raw_extractor_rows: usize,
    unique_extractors: usize,
    extractor_descriptions: usize,
    collapsed_identical_extractor_rows: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OptionRow {
    id: String,
    canonical: String,
    aliases: Vec<String>,
    group: String,
    synopsis: String,
    description: String,
    takes_value: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PresetRow {
    id: String,
    name: String,
    expansion: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InteractionRow {
    id: String,
    kind: String,
    description: String,
    source: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ExtractorRow {
    id: String,
    key: String,
    url_class: String,
    working: bool,
    source_occurrences: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DescriptionRow {
    id: String,
    description: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CorpusManifest {
    schema_id: String,
    schema_version: String,
    corpus_id: String,
    profile_id: String,
    normalization_versions: Vec<String>,
    shard_algorithm: String,
    shard_count: u32,
    planes: Vec<String>,
    cases: Vec<CorpusCase>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CorpusCase {
    id: String,
    plane: String,
    deterministic: bool,
    network_allowed: bool,
    normalization_version: String,
    fixture: String,
    fixture_sha256: String,
    expected_outcome: String,
    shard: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LiveManifest {
    schema_id: String,
    schema_version: String,
    suite_id: String,
    profile_id: String,
    default_enabled: bool,
    deterministic_proof: bool,
    canaries: Vec<LiveCanary>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LiveCanary {
    id: String,
    url_class: String,
    url: String,
    timeout_seconds: u64,
    credential_policy: String,
    expected_classification: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct NegativeCase {
    schema_version: u32,
    fixture_id: String,
    mutation: String,
    expected_diagnostic: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CandidateResults {
    schema_id: String,
    schema_version: String,
    corpus_id: String,
    profile_id: String,
    observations: Vec<CandidateObservation>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CandidateObservation {
    case_id: String,
    observed_digest: String,
    classification: Option<String>,
    decision_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct CompatibilityReport {
    schema_id: &'static str,
    schema_version: &'static str,
    command: String,
    status: &'static str,
    source: CompatibilitySource,
    inputs: BTreeMap<String, String>,
    /// Capabilities the command is designed to support. This is descriptive only.
    declared_supported_proof_classes: Vec<&'static str>,
    /// Proof classes from checks that actually executed for this invocation.
    executed_proof_classes: Vec<&'static str>,
    /// Whether this report covers the complete corpus or only one selected shard.
    execution_scope: &'static str,
    checks: Vec<CompatibilityCheck>,
    semantic_replays: Vec<SemanticReplay>,
    aggregate_proof_class: &'static str,
    negative_fixtures: Vec<NegativeResult>,
    differential_rows: Vec<DifferentialRow>,
    artifacts: Vec<String>,
    proof_limitations: Vec<String>,
}

#[derive(Debug, Serialize)]
struct CompatibilitySource {
    git_commit: String,
    dirty: bool,
    dirty_paths: Vec<String>,
    content_fingerprint: String,
}

#[derive(Debug, Serialize)]
struct CompatibilityCheck {
    id: String,
    status: &'static str,
    proof_class: &'static str,
    concrete_input: String,
    boundary: String,
    expected_result: String,
    observed_result: String,
    skipped_semantic_dependencies: Vec<String>,
    detail: String,
}

#[derive(Debug, Serialize)]
struct SemanticReplay {
    case_id: String,
    plane: String,
    status: &'static str,
    proof_class: &'static str,
    concrete_input: String,
    boundary: String,
    expected_result: String,
    observed_result: String,
    skipped_semantic_dependencies: Vec<String>,
}

/// Evidence recovered from an emitted compatibility replay report by a second
/// gate. This intentionally owns its strings: it is a deserialized artifact,
/// not the in-memory declaration used to write the report.
#[derive(Clone, Debug)]
pub(super) struct ReplayReportEvidence {
    pub(super) report_path: String,
    pub(super) status: String,
    pub(super) execution_scope: String,
    pub(super) source_git_commit: String,
    pub(super) source_dirty: bool,
    pub(super) source_dirty_paths: Vec<String>,
    pub(super) source_content_fingerprint: String,
    pub(super) semantic_replays: Vec<ReplayResultEvidence>,
}

#[derive(Clone, Debug)]
pub(super) struct ReplayResultEvidence {
    pub(super) case_id: String,
    pub(super) plane: String,
    pub(super) concrete_input: String,
    pub(super) boundary: String,
    pub(super) expected_result: String,
    pub(super) observed_result: String,
    pub(super) skipped_semantic_dependencies: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayReportArtifact {
    schema_id: String,
    schema_version: String,
    command: String,
    status: String,
    source: ReplayReportSource,
    inputs: BTreeMap<String, String>,
    declared_supported_proof_classes: Vec<String>,
    executed_proof_classes: Vec<String>,
    execution_scope: String,
    checks: Vec<ReplayReportCheck>,
    semantic_replays: Vec<ReplayReportSemanticReplay>,
    aggregate_proof_class: String,
    negative_fixtures: Vec<ReplayReportNegativeResult>,
    differential_rows: Vec<ReplayReportDifferentialRow>,
    artifacts: Vec<String>,
    proof_limitations: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayReportSource {
    git_commit: String,
    dirty: bool,
    dirty_paths: Vec<String>,
    content_fingerprint: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayReportCheck {
    id: String,
    status: String,
    proof_class: String,
    concrete_input: String,
    boundary: String,
    expected_result: String,
    observed_result: String,
    skipped_semantic_dependencies: Vec<String>,
    detail: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayReportSemanticReplay {
    case_id: String,
    plane: String,
    status: String,
    proof_class: String,
    concrete_input: String,
    boundary: String,
    expected_result: String,
    observed_result: String,
    skipped_semantic_dependencies: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayReportNegativeResult {
    fixture_id: String,
    status: String,
    expected_diagnostic: String,
    observed_diagnostic: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayReportDifferentialRow {
    case_id: String,
    status: String,
    classification: String,
    decision_id: Option<String>,
    expected_digest: String,
    observed_digest: Option<String>,
}

#[derive(Debug, Serialize)]
struct NegativeResult {
    fixture_id: String,
    status: &'static str,
    expected_diagnostic: String,
    observed_diagnostic: String,
}

#[derive(Debug, Serialize)]
struct DifferentialRow {
    case_id: String,
    status: &'static str,
    classification: String,
    decision_id: Option<String>,
    expected_digest: String,
    observed_digest: Option<String>,
}

#[derive(Debug, Serialize)]
struct InventoryDiffReport {
    schema_id: &'static str,
    schema_version: &'static str,
    status: &'static str,
    source: CompatibilitySource,
    inputs: BTreeMap<String, String>,
    before_profile_id: String,
    after_profile_id: String,
    added_options: Vec<String>,
    removed_options: Vec<String>,
    changed_options: Vec<String>,
    added_presets: Vec<String>,
    removed_presets: Vec<String>,
    changed_presets: Vec<String>,
    added_interactions: Vec<String>,
    removed_interactions: Vec<String>,
    changed_interactions: Vec<String>,
    added_extractors: Vec<String>,
    removed_extractors: Vec<String>,
    changed_extractors: Vec<String>,
    added_extractor_descriptions: Vec<String>,
    removed_extractor_descriptions: Vec<String>,
    changed_extractor_descriptions: Vec<String>,
    proof_limitations: Vec<String>,
}

struct InventoryDelta {
    added: Vec<String>,
    removed: Vec<String>,
    changed: Vec<String>,
}

pub(super) fn run_generate(root: &Path, args: &[String]) -> Result<(), String> {
    let oracle_exe = required_arg(args, "--oracle-exe")?;
    let source_root = required_arg(args, "--source-root")?;
    let output = optional_arg(args, "--output").unwrap_or(PROFILE_PATH);
    let output_path = require_safe_output(root, output)?;
    let manifest: OracleManifest = read_json(&root.join(ORACLE_PATH))?;
    validate_oracle_manifest(&manifest)?;
    let profile = generate_profile(
        root,
        Path::new(oracle_exe),
        Path::new(source_root),
        &manifest,
    )?;
    validate_profile(&profile, &manifest)?;
    atomic_json(&output_path, &profile)?;
    println!(
        "PASS FF-GATE-COMPAT-GENERATE-001; profile={output}; options={}; extractors={}",
        profile.counts.options, profile.counts.unique_extractors
    );
    Ok(())
}

pub(super) fn run_validate(root: &Path, args: &[String]) -> Result<(), String> {
    let manifest: OracleManifest = read_json(&root.join(ORACLE_PATH))?;
    let profile: CompatibilityProfile = read_json(&root.join(PROFILE_PATH))?;
    let corpus: CorpusManifest = read_json(&root.join(CORPUS_PATH))?;
    let live: LiveManifest = read_json(&root.join(LIVE_PATH))?;
    validate_oracle_manifest(&manifest)?;
    validate_profile(&profile, &manifest)?;
    let mut checks = Vec::new();
    validate_corpus(root, &corpus, &profile, &mut checks)?;
    validate_live_manifest(&live, &profile)?;
    checks.push(pass(
        "live-canary-separation",
        &format!(
            "suite={} default_enabled=false deterministic_proof=false canaries={}",
            live.suite_id,
            live.canaries.len()
        ),
    ));
    let negative_fixtures = validate_negative_fixtures(root, &manifest, &profile, &corpus)?;
    checks.push(pass(
        "negative-fixtures",
        &format!(
            "{} compatibility mutations produced exact stable diagnostics",
            negative_fixtures.len()
        ),
    ));
    let report = base_report(
        root,
        "compatibility-validate",
        "STRUCTURAL_ONLY",
        "complete_corpus_structural_validation",
        BTreeMap::new(),
        checks,
        Vec::new(),
        negative_fixtures,
        Vec::new(),
    )?;
    let path = write_compatibility_report(root, "compatibility-validate", &report)?;
    println!(
        "STRUCTURAL_ONLY FF-GATE-COMPAT-001; report={}; profile_options={}; corpus_cases={}",
        slash(&path),
        profile.counts.options,
        corpus.cases.len()
    );
    let _ = args;
    Ok(())
}

pub(super) fn run_replay(root: &Path, args: &[String], rest: &[String]) -> Result<(), String> {
    let manifest: OracleManifest = read_json(&root.join(ORACLE_PATH))?;
    let profile: CompatibilityProfile = read_json(&root.join(PROFILE_PATH))?;
    let corpus: CorpusManifest = read_json(&root.join(CORPUS_PATH))?;
    validate_oracle_manifest(&manifest)?;
    validate_profile(&profile, &manifest)?;
    let shard = optional_arg(rest, "--shard").map(parse_shard).transpose()?;
    if shard.is_some_and(|(_, total)| total != corpus.shard_count) {
        return Err(format!(
            "FF-COMP-E-SHARD: requested shard total must equal corpus shard_count {}",
            corpus.shard_count
        ));
    }
    let mut checks = Vec::new();
    validate_corpus(root, &corpus, &profile, &mut checks)?;
    let selected = corpus
        .cases
        .iter()
        .filter(|case| shard.is_none_or(|(index, _)| case.shard == index))
        .collect::<Vec<_>>();
    if selected.is_empty() {
        let scope = shard.map_or_else(
            || "complete corpus".to_owned(),
            |(index, total)| format!("shard {index}/{total}"),
        );
        return Err(format!(
            "FF-COMP-E-SHARD-EMPTY: {scope} selected zero corpus cases; empty selections cannot produce replay evidence"
        ));
    }
    let mut semantic_replays = Vec::new();
    for case in &selected {
        let fixture: Value = read_json(&root.join(&case.fixture))?;
        semantic_replays.push(replay_case_semantics(case, &fixture)?);
    }
    checks.push(structural_check(
        "offline-replay",
        &format!(
            "selected {} deterministic fixture cases for native Rust semantic replay; shard={:?}; network_access=false",
            selected.len(),
            shard
        ),
    ));
    let execution_scope = if shard.is_some() {
        "selected_shard_only"
    } else {
        "complete_corpus"
    };
    let status = if shard.is_some() {
        "SEMANTIC_REPLAY_SUBSET_EXECUTED"
    } else {
        "SEMANTIC_REPLAY_EXECUTED"
    };
    let report = base_report(
        root,
        "compatibility-replay",
        status,
        execution_scope,
        BTreeMap::new(),
        checks,
        semantic_replays,
        Vec::new(),
        Vec::new(),
    )?;
    let path = write_compatibility_report(root, "compatibility-replay", &report)?;
    println!(
        "{} FF-GATE-COMPAT-REPLAY-001; report={}; cases={}; execution_scope={}; aggregate_executed_proof_class={}",
        status,
        slash(&path),
        selected.len(),
        execution_scope,
        report.aggregate_proof_class,
    );
    let _ = args;
    Ok(())
}

pub(super) fn run_diff(root: &Path, args: &[String], rest: &[String]) -> Result<(), String> {
    let candidate_path = required_arg(rest, "--candidate")?;
    let candidate_file = require_safe_input(root, candidate_path)?;
    let manifest: OracleManifest = read_json(&root.join(ORACLE_PATH))?;
    let profile: CompatibilityProfile = read_json(&root.join(PROFILE_PATH))?;
    let corpus: CorpusManifest = read_json(&root.join(CORPUS_PATH))?;
    validate_oracle_manifest(&manifest)?;
    validate_profile(&profile, &manifest)?;
    let mut canonical_checks = Vec::new();
    validate_corpus(root, &corpus, &profile, &mut canonical_checks)?;
    let candidate: CandidateResults = read_json(&candidate_file)?;
    let rows = differential_rows(&corpus, &profile, &candidate)?;
    let missing = rows
        .iter()
        .filter(|row| row.classification == "missing_feature")
        .count();
    canonical_checks.push(pass(
        "differential-completeness",
        &format!(
            "every one of {} corpus cases has an explicit row; missing_features={missing}",
            rows.len()
        ),
    ));
    let report = base_report(
        root,
        "compatibility-diff",
        "STRUCTURAL_ONLY",
        "complete_corpus_structural_comparison",
        BTreeMap::from([(
            format!("candidate:{candidate_path}"),
            sha256_file(&candidate_file)?,
        )]),
        canonical_checks,
        Vec::new(),
        Vec::new(),
        rows,
    )?;
    let path = write_compatibility_report(root, "compatibility-diff", &report)?;
    println!(
        "STRUCTURAL_ONLY FF-GATE-COMPAT-DIFF-001; report={}; this proves report completeness, not Ferric parity",
        slash(&path)
    );
    let _ = args;
    Ok(())
}

pub(super) fn run_inventory_diff(
    root: &Path,
    args: &[String],
    rest: &[String],
) -> Result<(), String> {
    let before_path = required_arg(rest, "--before")?;
    let after_path = required_arg(rest, "--after")?;
    let before_file = require_safe_input(root, before_path)?;
    let after_file = require_safe_input(root, after_path)?;
    let before: CompatibilityProfile = read_json(&before_file)?;
    let after: CompatibilityProfile = read_json(&after_file)?;
    let manifest: OracleManifest = read_json(&root.join(ORACLE_PATH))?;
    validate_oracle_manifest(&manifest)?;
    validate_inventory_profile(&before, &manifest)?;
    validate_inventory_profile(&after, &manifest)?;
    let state = source_state(root)?;
    let inputs = BTreeMap::from([
        (format!("before:{before_path}"), sha256_file(&before_file)?),
        (format!("after:{after_path}"), sha256_file(&after_file)?),
    ]);
    let report = inventory_diff(
        &before,
        &after,
        CompatibilitySource {
            git_commit: state.git_commit,
            dirty: state.dirty,
            dirty_paths: state.dirty_paths,
            content_fingerprint: state.content_fingerprint,
        },
        inputs,
    )?;
    let path = unique_report_path(root, "compatibility-inventory-diff")?;
    atomic_json(&path, &report)?;
    println!(
        "STRUCTURAL_ONLY FF-GATE-COMPAT-DIFF-002; report={}",
        slash(path.strip_prefix(root).map_err(|error| error.to_string())?)
    );
    let _ = args;
    Ok(())
}

pub(super) fn run_live_canaries(
    root: &Path,
    args: &[String],
    rest: &[String],
) -> Result<(), String> {
    if !rest.iter().any(|argument| argument == "--enable-live") {
        return Err(
            "FF-COMP-E-LIVE-OPT-IN: live canaries require the explicit --enable-live flag"
                .to_owned(),
        );
    }
    let oracle_exe = required_arg(rest, "--oracle-exe")?;
    let oracle_path = Path::new(oracle_exe);
    if !oracle_path.is_file() || sha256_file(oracle_path)? != ORACLE_EXE_SHA256 {
        return Err("FF-COMP-E-UNPINNED-ORACLE: live oracle executable mismatch".to_owned());
    }
    let profile: CompatibilityProfile = read_json(&root.join(PROFILE_PATH))?;
    let manifest: OracleManifest = read_json(&root.join(ORACLE_PATH))?;
    let live: LiveManifest = read_json(&root.join(LIVE_PATH))?;
    validate_oracle_manifest(&manifest)?;
    validate_profile(&profile, &manifest)?;
    validate_live_manifest(&live, &profile)?;
    let program = oracle_path
        .to_str()
        .ok_or("FF-COMP-E-ORACLE-INPUT: executable path is not Unicode")?;
    let mut checks = Vec::new();
    for canary in &live.canaries {
        let result = oracle_output(
            root,
            program,
            &[
                "--ignore-config",
                "--no-plugin-dirs",
                "--encoding",
                "utf-8",
                "--no-color",
                "--simulate",
                "--skip-download",
                "--no-warnings",
                "--print",
                "%(extractor_key)s|%(id)s",
                &canary.url,
            ],
            Duration::from_secs(canary.timeout_seconds),
        );
        let detail = match result {
            Ok(observation) => format!(
                "classification={}; outcome=success; observation={}",
                canary.expected_classification,
                bounded_observation(&observation)
            ),
            Err(error) => format!(
                "classification={}; outcome=failure; diagnostic={}",
                canary.expected_classification,
                bounded_observation(&error)
            ),
        };
        checks.push(CompatibilityCheck {
            id: canary.id.clone(),
            status: "OBSERVED",
            proof_class: "integration_observation",
            concrete_input: canary.url.clone(),
            boundary: "explicit opt-in pinned external oracle canary".to_owned(),
            expected_result: canary.expected_classification.clone(),
            observed_result: detail.clone(),
            skipped_semantic_dependencies: vec![
                "Live canaries are nondeterministic observations and do not prove native Ferric behavior."
                    .to_owned(),
            ],
            detail,
        });
    }
    let report = base_report(
        root,
        "compatibility-live-canaries",
        "OBSERVED",
        "selected_live_canaries_only",
        BTreeMap::from([(
            "live-oracle-executable:yt-dlp-windows-x64".to_owned(),
            ORACLE_EXE_SHA256.to_owned(),
        )]),
        checks,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )?;
    let path = write_compatibility_report(root, "compatibility-live-canaries", &report)?;
    println!(
        "OBSERVED FF-COMPAT-LIVE-001; report={}; canaries={}; deterministic_proof=false",
        slash(&path),
        live.canaries.len()
    );
    let _ = args;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn generate_profile(
    root: &Path,
    oracle_exe: &Path,
    source_root: &Path,
    manifest: &OracleManifest,
) -> Result<CompatibilityProfile, String> {
    if !oracle_exe.is_file() || !source_root.is_dir() {
        return Err(
            "FF-COMP-E-ORACLE-INPUT: oracle executable or source root is absent".to_owned(),
        );
    }
    let executable_sha256 = sha256_file(oracle_exe)?;
    let expected_executable = manifest
        .artifacts
        .iter()
        .find(|artifact| artifact.artifact_id == "yt-dlp-windows-x64")
        .ok_or("FF-COMP-E-UNPINNED-ORACLE: Windows oracle artifact is absent")?;
    if executable_sha256 != expected_executable.sha256 {
        return Err(format!(
            "FF-COMP-E-UNPINNED-ORACLE: executable expected={} observed={executable_sha256}",
            expected_executable.sha256
        ));
    }
    let program = oracle_exe
        .to_str()
        .ok_or("FF-COMP-E-ORACLE-INPUT: executable path is not Unicode")?;
    let version = oracle_output(
        root,
        program,
        &[
            "--ignore-config",
            "--no-plugin-dirs",
            "--encoding",
            "utf-8",
            "--version",
        ],
        TOOL_TIMEOUT,
    )?;
    if version.trim() != ORACLE_VERSION {
        return Err(format!(
            "FF-COMP-E-UNPINNED-ORACLE: expected version {ORACLE_VERSION}, observed {:?}",
            version.trim()
        ));
    }
    let source_head = command_output_with_timeout(
        root,
        "git",
        &[
            "-C",
            source_root.to_str().ok_or("source path is not Unicode")?,
            "rev-parse",
            "HEAD",
        ],
        TOOL_TIMEOUT,
    )?;
    if source_head.trim() != ORACLE_TAG_COMMIT {
        return Err(format!(
            "FF-COMP-E-UNPINNED-ORACLE: expected source tag {ORACLE_TAG_COMMIT}, observed {:?}",
            source_head.trim()
        ));
    }
    let mut source_inputs = BTreeMap::new();
    for input in &manifest.source_inputs {
        let observed = sha256_file(&source_root.join(&input.path))?;
        if observed != input.sha256 {
            return Err(format!(
                "FF-COMP-E-UNPINNED-ORACLE: source {} expected={} observed={observed}",
                input.path, input.sha256
            ));
        }
        source_inputs.insert(input.path.clone(), observed);
    }
    let help = normalized_capture(&oracle_output(
        root,
        program,
        &[
            "--ignore-config",
            "--no-plugin-dirs",
            "--encoding",
            "utf-8",
            "--no-color",
            "--help",
        ],
        TOOL_TIMEOUT,
    )?);
    let extractors = normalized_capture(&oracle_output(
        root,
        program,
        &[
            "--ignore-config",
            "--no-plugin-dirs",
            "--encoding",
            "utf-8",
            "--no-color",
            "--list-extractors",
        ],
        TOOL_TIMEOUT,
    )?);
    let descriptions = normalized_oracle_descriptions(&oracle_output(
        root,
        program,
        &[
            "--ignore-config",
            "--no-plugin-dirs",
            "--encoding",
            "utf-8",
            "--no-color",
            "--extractor-descriptions",
        ],
        TOOL_TIMEOUT,
    )?);
    let (mut options, mut presets) = parse_help(&help)?;
    options.sort_by(|left, right| left.id.cmp(&right.id));
    presets.sort_by(|left, right| left.id.cmp(&right.id));
    let raw_extractor_rows = extractors
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    let mut extractor_map: BTreeMap<String, ExtractorRow> = BTreeMap::new();
    for line in extractors
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let (key, working) = line
            .strip_suffix(" (CURRENTLY BROKEN)")
            .map_or((line, true), |key| (key, false));
        let id = format!("extractor-{}", stable_component(key));
        match extractor_map.get_mut(&id) {
            Some(existing) if existing.key == key && existing.working == working => {
                existing.source_occurrences += 1;
            }
            Some(_) => {
                return Err(format!(
                    "FF-COMP-E-DUPLICATE-ID: conflicting extractor {id}"
                ));
            }
            None => {
                extractor_map.insert(
                    id.clone(),
                    ExtractorRow {
                        id,
                        key: key.to_owned(),
                        url_class: format!("url-class-{}", stable_component(key)),
                        working,
                        source_occurrences: 1,
                    },
                );
            }
        }
    }
    let extractor_rows = extractor_map.into_values().collect::<Vec<_>>();
    let mut description_values = descriptions
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    description_values.sort();
    description_values.dedup();
    let description_rows = description_values
        .into_iter()
        .enumerate()
        .map(|(index, description)| DescriptionRow {
            id: format!("extractor-description-{:04}", index + 1),
            description,
        })
        .collect::<Vec<_>>();
    let interactions = pinned_interactions();
    let input_digests = BTreeMap::from([
        ("help-lf".to_owned(), sha256_text(&help)),
        ("list-extractors-lf".to_owned(), sha256_text(&extractors)),
        (
            "extractor-descriptions-lf".to_owned(),
            sha256_text(&descriptions),
        ),
    ]);
    let counts = ProfileCounts {
        options: options.len(),
        presets: presets.len(),
        interactions: interactions.len(),
        raw_extractor_rows,
        unique_extractors: extractor_rows.len(),
        extractor_descriptions: description_rows.len(),
        collapsed_identical_extractor_rows: raw_extractor_rows.saturating_sub(extractor_rows.len()),
    };
    Ok(CompatibilityProfile {
        schema_id: "ff.compatibility-profile@1".to_owned(),
        schema_version: "1.0.0".to_owned(),
        profile_id: "yt-dlp-2026.07.04-profile-v1".to_owned(),
        oracle_id: manifest.oracle_id.clone(),
        normalization_version: NORMALIZATION_VERSION.to_owned(),
        source_identity: ProfileSourceIdentity {
            version: ORACLE_VERSION.to_owned(),
            tag_commit: ORACLE_TAG_COMMIT.to_owned(),
            release_git_head: manifest.release.release_git_head.clone(),
            executable_sha256,
            source_inputs,
        },
        input_digests,
        counts,
        options,
        presets,
        interactions,
        extractors: extractor_rows,
        extractor_descriptions: description_rows,
        normalization_notes: vec![
            "Line endings are normalized to LF before hashing and parsing.".to_owned(),
            "Identical repeated extractor rows collapse to one stable row while source_occurrences retains the raw multiplicity.".to_owned(),
            "Randomized extractor search examples are replaced by {{RANDOMIZED_SEARCH_EXAMPLE}} before hashing and inventory generation.".to_owned(),
            "Help, extractor, and description commands run with config, plugins, color, and network disabled by command selection.".to_owned(),
            "No current clock, absolute path, hostname, credential, or live response enters the generated profile.".to_owned(),
        ],
    })
}

fn parse_help(help: &str) -> Result<(Vec<OptionRow>, Vec<PresetRow>), String> {
    let mut group = "Ungrouped".to_owned();
    let mut entries: Vec<(String, String, String)> = Vec::new();
    let mut current: Option<(String, String, String)> = None;
    for line in help.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !line.chars().next().is_some_and(char::is_whitespace)
            || (line.starts_with("  ") && !line.starts_with("    ") && trimmed.ends_with(':'))
        {
            if trimmed.ends_with(':') {
                if let Some(entry) = current.take() {
                    entries.push(entry);
                }
                trimmed.trim_end_matches(':').clone_into(&mut group);
            }
            continue;
        }
        if line.starts_with("    -") {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            let (synopsis, description) = split_help_line(trimmed);
            current = Some((group.clone(), synopsis.to_owned(), description.to_owned()));
        } else if let Some((_, _, description)) = current.as_mut() {
            if !description.is_empty() {
                description.push(' ');
            }
            description.push_str(trimmed);
        }
    }
    if let Some(entry) = current {
        entries.push(entry);
    }
    let mut options = Vec::new();
    let mut presets = Vec::new();
    let mut option_ids = BTreeSet::new();
    let mut preset_ids = BTreeSet::new();
    for (group, synopsis, description) in entries {
        if group == "Preset Aliases" {
            let name = synopsis
                .split_whitespace()
                .nth(1)
                .ok_or("FF-COMP-E-PARSE: preset has no name")?;
            let id = format!("preset-{}", stable_component(name));
            if !preset_ids.insert(id.clone()) {
                return Err(format!("FF-COMP-E-DUPLICATE-ID: {id}"));
            }
            presets.push(PresetRow {
                id,
                name: name.to_owned(),
                expansion: description,
            });
            continue;
        }
        let aliases = option_spellings(&synopsis);
        if aliases.is_empty() {
            return Err(format!(
                "FF-COMP-E-PARSE: option has no spelling: {synopsis}"
            ));
        }
        let canonical = aliases
            .iter()
            .find(|alias| alias.starts_with("--"))
            .unwrap_or(&aliases[0])
            .clone();
        let id = format!(
            "option-{}",
            stable_component(canonical.trim_start_matches('-'))
        );
        if !option_ids.insert(id.clone()) {
            return Err(format!("FF-COMP-E-DUPLICATE-ID: {id}"));
        }
        let takes_value = synopsis
            .split_whitespace()
            .any(|part| !part.starts_with('-') && part.chars().any(char::is_uppercase));
        options.push(OptionRow {
            id,
            canonical,
            aliases,
            group,
            synopsis,
            description,
            takes_value,
        });
    }
    Ok((options, presets))
}

fn split_help_line(line: &str) -> (&str, &str) {
    let bytes = line.as_bytes();
    let mut index = 0;
    while index + 1 < bytes.len() {
        if bytes[index] == b' ' && bytes[index + 1] == b' ' {
            let synopsis = line[..index].trim_end();
            let description = line[index..].trim();
            return (synopsis, description);
        }
        index += 1;
    }
    (line, "")
}

fn option_spellings(synopsis: &str) -> Vec<String> {
    synopsis
        .split(|character: char| character == ',' || character.is_whitespace())
        .map(|part| {
            part.trim_matches(|character: char| {
                !character.is_ascii_alphanumeric() && character != '-'
            })
        })
        .filter(|part| {
            (part.starts_with("--") && part.len() > 2)
                || (part.starts_with('-') && !part.starts_with("--") && part.len() == 2)
        })
        .map(ToOwned::to_owned)
        .collect()
}

fn pinned_interactions() -> Vec<InteractionRow> {
    [
        ("interaction-config-precedence", "configuration", "Command-line, portable, home, user, and system configuration sources have explicit precedence and can be disabled.", "yt_dlp/options.py:parseOpts"),
        ("interaction-environment-defaults", "default", "Environment and platform state may affect defaults and must be captured as explicit oracle inputs.", "yt_dlp/options.py:create_parser"),
        ("interaction-repeated-options", "configuration", "Repeated list, set, dictionary, and scalar options use distinct append, prepend, replace, and callback semantics.", "yt_dlp/options.py:create_parser callbacks"),
        ("interaction-runtime-alias", "alias", "User-defined aliases expand through bounded callback recursion and preserve argument provenance.", "yt_dlp/options.py:_create_alias"),
        ("interaction-preset-expansion", "preset", "Pinned preset names expand to ordered argument vectors before normal option processing.", "yt_dlp/options.py:_PRESET_ALIASES"),
        ("interaction-compat-profile", "compatibility", "Compatibility options alter defaults and downstream behavior after parsing and must remain separate from native Ferric semantics.", "yt_dlp/__init__.py:set_compat_opts"),
    ]
    .into_iter()
    .map(|(id, kind, description, source)| InteractionRow {
        id: id.to_owned(),
        kind: kind.to_owned(),
        description: description.to_owned(),
        source: source.to_owned(),
    })
    .collect()
}

#[allow(clippy::too_many_lines)]
fn validate_oracle_manifest(manifest: &OracleManifest) -> Result<(), String> {
    let expected_commands = vec![
        vec![
            "--ignore-config",
            "--no-plugin-dirs",
            "--encoding",
            "utf-8",
            "--no-color",
            "--help",
        ],
        vec![
            "--ignore-config",
            "--no-plugin-dirs",
            "--encoding",
            "utf-8",
            "--no-color",
            "--list-extractors",
        ],
        vec![
            "--ignore-config",
            "--no-plugin-dirs",
            "--encoding",
            "utf-8",
            "--no-color",
            "--extractor-descriptions",
        ],
    ];
    if manifest.schema_id != "ff.compatibility-oracle@1"
        || manifest.schema_version != "1.0.0"
        || manifest.oracle_id != "yt-dlp-2026.07.04-oracle-v1"
        || manifest.release.project != "yt-dlp/yt-dlp"
        || manifest.release.version != ORACLE_VERSION
        || manifest.release.channel != "stable"
        || !manifest.release.immutable
        || manifest.release.tag_commit != ORACLE_TAG_COMMIT
        || manifest.release.release_git_head != ORACLE_RELEASE_HEAD
        || manifest.release.published_at_utc != "2026-07-04T22:41:44Z"
        || manifest.release.release_url
            != "https://github.com/yt-dlp/yt-dlp/releases/tag/2026.07.04"
        || manifest.generator.normalization_version != NORMALIZATION_VERSION
        || manifest.generator.network_allowed
        || manifest.generator.plugins_allowed
        || manifest.generator.config_allowed
        || manifest.generator.timeout_seconds == 0
        || manifest.generator.commands != expected_commands
    {
        return Err(
            "FF-COMP-E-UNPINNED-ORACLE: oracle manifest identity or policy mismatch".to_owned(),
        );
    }
    let mut artifact_ids = BTreeSet::new();
    for artifact in &manifest.artifacts {
        if !artifact_ids.insert(artifact.artifact_id.as_str())
            || !valid_sha256(&artifact.sha256)
            || artifact.size_bytes == 0
            || !artifact
                .url
                .starts_with("https://github.com/yt-dlp/yt-dlp/releases/download/2026.07.04/")
        {
            return Err("FF-COMP-E-UNPINNED-ORACLE: artifact provenance is incomplete".to_owned());
        }
    }
    if !artifact_ids.contains("yt-dlp-windows-x64")
        || !artifact_ids.contains("sha2-256sums")
        || !artifact_ids.contains("source-tarball")
    {
        return Err("FF-COMP-E-UNPINNED-ORACLE: required artifact identity is absent".to_owned());
    }
    let expected_artifacts = BTreeMap::from([
        (
            "sha2-256sums",
            (
                "SHA2-256SUMS",
                "eca42575010efc77b8dc1e263c57e19c4bddc42d3e08ba789ccde72c97d48c64",
                1_595_u64,
            ),
        ),
        (
            "source-tarball",
            (
                "yt-dlp.tar.gz",
                "31c32457d1a573a341bb0929386c624fe47339a5338829e6e9c9454bdfa7397a",
                6_015_962_u64,
            ),
        ),
        (
            "yt-dlp-windows-x64",
            ("yt-dlp.exe", ORACLE_EXE_SHA256, 18_226_085_u64),
        ),
    ]);
    if manifest.artifacts.len() != expected_artifacts.len()
        || manifest.artifacts.iter().any(|artifact| {
            expected_artifacts
                .get(artifact.artifact_id.as_str())
                .is_none_or(|(filename, sha256, size)| {
                    artifact.filename != *filename
                        || artifact.sha256 != *sha256
                        || artifact.size_bytes != *size
                })
        })
    {
        return Err("FF-COMP-E-UNPINNED-ORACLE: artifact identity mismatch".to_owned());
    }
    let mut source_paths = BTreeSet::new();
    if manifest.source_inputs.iter().any(|input| {
        !source_paths.insert(input.path.as_str())
            || !safe_relative(&input.path)
            || !valid_sha256(&input.sha256)
            || input.role.trim().is_empty()
    }) {
        return Err("FF-COMP-E-UNPINNED-ORACLE: source input provenance is incomplete".to_owned());
    }
    let expected_sources = BTreeMap::from([
        (
            "yt_dlp/version.py",
            "8485c16520f205dbb8babb162c48fa30f65413230f9f8ac0317dbd2725fdecfd",
        ),
        (
            "yt_dlp/options.py",
            "b527798ae3e43213e689e6f9f6b670a3678e9998753a07d47a84ee23846d1b3a",
        ),
        (
            "yt_dlp/__init__.py",
            "f3ddb4b5759d6646aa25ff9de7026c78b14543bb1180a3f2ff5acb0757082256",
        ),
        (
            "yt_dlp/extractor/_extractors.py",
            "91c7e51ed6d369dbea1d5862afbd61adf1b53860206aedaa29b4196800acdce0",
        ),
        (
            "yt_dlp/extractor/__init__.py",
            "96b18d67eac15b220f4a557522af5d28fea7f696b3f77d44712807822eb5f5f6",
        ),
    ]);
    if manifest.source_inputs.len() != expected_sources.len()
        || manifest.source_inputs.iter().any(|input| {
            expected_sources
                .get(input.path.as_str())
                .is_none_or(|sha256| input.sha256 != *sha256)
        })
    {
        return Err("FF-COMP-E-UNPINNED-ORACLE: source identity mismatch".to_owned());
    }
    require_canonical_json_digest(
        manifest,
        ORACLE_MANIFEST_SHA256,
        "FF-COMP-E-ORACLE-INTEGRITY: pinned oracle manifest content mismatch",
    )?;
    Ok(())
}

fn validate_profile(
    profile: &CompatibilityProfile,
    manifest: &OracleManifest,
) -> Result<(), String> {
    validate_profile_structure(profile)?;
    if profile.schema_id != "ff.compatibility-profile@1"
        || profile.schema_version != "1.0.0"
        || profile.profile_id != "yt-dlp-2026.07.04-profile-v1"
        || profile.oracle_id != manifest.oracle_id
        || profile.normalization_version != NORMALIZATION_VERSION
        || profile.source_identity.version != ORACLE_VERSION
        || profile.source_identity.tag_commit != ORACLE_TAG_COMMIT
        || profile.source_identity.release_git_head != manifest.release.release_git_head
    {
        return Err("FF-COMP-E-UNPINNED-ORACLE: profile identity mismatch".to_owned());
    }
    if profile.source_identity.executable_sha256 != ORACLE_EXE_SHA256 {
        return Err("FF-COMP-E-UNPINNED-ORACLE: profile executable mismatch".to_owned());
    }
    let expected_input_keys =
        BTreeSet::from(["extractor-descriptions-lf", "help-lf", "list-extractors-lf"]);
    if profile
        .input_digests
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        != expected_input_keys
    {
        return Err("FF-COMP-E-UNPINNED-ORACLE: profile input digest keys mismatch".to_owned());
    }
    for input in &manifest.source_inputs {
        if profile.source_identity.source_inputs.get(&input.path) != Some(&input.sha256) {
            return Err(format!(
                "FF-COMP-E-UNPINNED-ORACLE: profile source digest mismatch for {}",
                input.path
            ));
        }
    }
    validate_current_profile_content(profile)
}

fn validate_profile_structure(profile: &CompatibilityProfile) -> Result<(), String> {
    if profile.schema_id != "ff.compatibility-profile@1" || profile.schema_version != "1.0.0" {
        return Err("FF-COMP-E-PROFILE-IDENTITY: unsupported profile schema".to_owned());
    }
    if profile.counts.options != profile.options.len()
        || profile.counts.presets != profile.presets.len()
        || profile.counts.interactions != profile.interactions.len()
        || profile.counts.unique_extractors != profile.extractors.len()
        || profile.counts.extractor_descriptions != profile.extractor_descriptions.len()
        || profile.counts.raw_extractor_rows
            != profile.counts.unique_extractors + profile.counts.collapsed_identical_extractor_rows
    {
        return Err("FF-COMP-E-INVENTORY-COUNT: profile counts do not match rows".to_owned());
    }
    require_unique_stable_ids(profile.options.iter().map(|row| row.id.as_str()))?;
    require_unique_stable_ids(profile.presets.iter().map(|row| row.id.as_str()))?;
    require_unique_stable_ids(profile.interactions.iter().map(|row| row.id.as_str()))?;
    require_unique_stable_ids(profile.extractors.iter().map(|row| row.id.as_str()))?;
    require_unique_stable_ids(
        profile
            .extractor_descriptions
            .iter()
            .map(|row| row.id.as_str()),
    )?;
    if profile.input_digests.len() != 3
        || profile
            .input_digests
            .values()
            .any(|digest| !valid_sha256(digest))
        || profile.source_identity.source_inputs.is_empty()
        || profile
            .source_identity
            .source_inputs
            .values()
            .any(|digest| !valid_sha256(digest))
        || !valid_sha256(&profile.source_identity.executable_sha256)
    {
        return Err("FF-COMP-E-UNPINNED-ORACLE: profile input digests are incomplete".to_owned());
    }
    if profile.options.iter().any(|row| {
        row.aliases.is_empty()
            || !row.aliases.contains(&row.canonical)
            || row.group.trim().is_empty()
            || row.synopsis.trim().is_empty()
    }) || profile.extractors.iter().any(|row| {
        row.key.trim().is_empty() || row.url_class.trim().is_empty() || row.source_occurrences == 0
    }) {
        return Err("FF-COMP-E-INVENTORY-CONTENT: incomplete profile row".to_owned());
    }
    Ok(())
}

fn validate_current_profile_content(profile: &CompatibilityProfile) -> Result<(), String> {
    require_canonical_json_digest(
        profile,
        PROFILE_SHA256,
        "FF-COMP-E-PROFILE-INTEGRITY: pinned profile content mismatch",
    )
}

fn validate_inventory_profile(
    profile: &CompatibilityProfile,
    manifest: &OracleManifest,
) -> Result<(), String> {
    validate_profile_structure(profile)?;
    if profile.profile_id == "yt-dlp-2026.07.04-profile-v1" {
        validate_profile(profile, manifest)?;
    }
    Ok(())
}

fn validate_corpus(
    root: &Path,
    corpus: &CorpusManifest,
    profile: &CompatibilityProfile,
    checks: &mut Vec<CompatibilityCheck>,
) -> Result<(), String> {
    validate_corpus_without_files(corpus)?;
    if corpus.profile_id != profile.profile_id {
        return Err("FF-COMP-E-CORPUS-IDENTITY: corpus profile mismatch".to_owned());
    }
    let mut observed_planes = BTreeSet::new();
    for case in &corpus.cases {
        observed_planes.insert(case.plane.as_str());
        let fixture_path = root.join(&case.fixture);
        if sha256_normalized_text_file(&fixture_path)? != case.fixture_sha256 {
            return Err(format!("FF-COMP-E-FIXTURE-DIGEST: {}", case.id));
        }
        let fixture: Value = read_json(&fixture_path)?;
        validate_case_fixture(case, &fixture)?;
        validate_sanitized_value(&fixture)?;
    }
    checks.push(pass(
        "corpus-coverage",
        &format!(
            "{} deterministic cases cover all {} mandatory planes with stable IDs and verified digests",
            corpus.cases.len(),
            observed_planes.len()
        ),
    ));
    checks.push(pass(
        "corpus-network-isolation",
        "every mandatory case declares deterministic=true and network_allowed=false",
    ));
    Ok(())
}

fn validate_case_fixture(case: &CorpusCase, fixture: &Value) -> Result<(), String> {
    let expected_schema = match case.plane.as_str() {
        "archive" => "ff.oracle-archive-observation@1",
        "failure" => "ff.oracle-failure-observation@1",
        "filesystem_process_artifact" => "ff.oracle-artifact-observation@1",
        "migration" => "ff.oracle-migration-observation@1",
        "normalized_observation" | "source_graph" => "ff.oracle-observation@1",
        "sanitized_network_transcript" => "ff.oracle-network-transcript@1",
        _ => return Err(format!("FF-COMP-E-COVERAGE: unknown plane {}", case.plane)),
    };
    if fixture.get("schema_id").and_then(Value::as_str) != Some(expected_schema)
        || fixture.get("case_id").and_then(Value::as_str) != Some(case.id.as_str())
    {
        return Err(format!(
            "FF-COMP-E-FIXTURE-SCHEMA: {} does not match its manifest row",
            case.id
        ));
    }
    Ok(())
}

fn replay_case_semantics(case: &CorpusCase, fixture: &Value) -> Result<SemanticReplay, String> {
    let observed_result = match case.plane.as_str() {
        "archive" => replay_archive_case(fixture)?,
        "failure" => replay_failure_case(fixture)?,
        "filesystem_process_artifact" => replay_artifact_case(fixture)?,
        "migration" => replay_migration_case(fixture)?,
        "normalized_observation" => replay_normalized_observation_case(fixture)?,
        "sanitized_network_transcript" => replay_sanitized_transcript_case(fixture)?,
        "source_graph" => replay_source_graph_case(fixture)?,
        other => return Err(format!("FF-COMP-E-COVERAGE: unknown replay plane {other}")),
    };
    if observed_result != case.expected_outcome {
        return Err(format!(
            "FF-COMP-E-SEMANTIC-OUTCOME: {} expected {:?}, native Rust replay observed {:?}",
            case.id, case.expected_outcome, observed_result
        ));
    }
    Ok(SemanticReplay {
        case_id: case.id.clone(),
        plane: case.plane.clone(),
        status: "SEMANTIC_PASS",
        proof_class: "semantic",
        concrete_input: case.fixture.clone(),
        boundary: "native Rust compatibility fixture interpreter".to_owned(),
        expected_result: case.expected_outcome.clone(),
        observed_result,
        skipped_semantic_dependencies: vec![
            "Ferric has no shipped entrypoint in this prerequisite packet; this native interpreter proves fixture semantics, not Ferric runtime parity."
                .to_owned(),
            "yt-dlp and Python are not invoked by compatibility-replay.".to_owned(),
        ],
    })
}

fn replay_archive_case(fixture: &Value) -> Result<String, String> {
    let archive_before = fixture
        .get("archive_before")
        .and_then(Value::as_array)
        .ok_or("FF-COMP-E-SEMANTIC-ARCHIVE: archive_before is absent")?
        .iter()
        .map(|entry| entry.as_str().map(ToOwned::to_owned))
        .collect::<Option<BTreeSet<_>>>()
        .ok_or("FF-COMP-E-SEMANTIC-ARCHIVE: archive_before has non-string entry")?;
    let request = fixture
        .get("request")
        .and_then(Value::as_object)
        .ok_or("FF-COMP-E-SEMANTIC-ARCHIVE: request is absent")?;
    let extractor = request
        .get("extractor_key")
        .and_then(Value::as_str)
        .ok_or("FF-COMP-E-SEMANTIC-ARCHIVE: extractor_key is absent")?;
    let media_id = request
        .get("media_id")
        .and_then(Value::as_str)
        .ok_or("FF-COMP-E-SEMANTIC-ARCHIVE: media_id is absent")?;
    let key = format!("{extractor} {media_id}");
    let observed = if archive_before.contains(&key) {
        serde_json::json!({
            "decision": "skip-already-recorded",
            "archive_after": archive_before.iter().cloned().collect::<Vec<_>>(),
            "output_created": false,
        })
    } else {
        serde_json::json!({
            "decision": "record-and-create-output",
            "archive_after": archive_before.iter().chain(std::iter::once(&key)).cloned().collect::<Vec<_>>(),
            "output_created": true,
        })
    };
    if fixture.get("expected") != Some(&observed) {
        return Err("FF-COMP-E-SEMANTIC-ARCHIVE: deduplication decision differs from expected archive state".to_owned());
    }
    if !archive_before.contains(&key) {
        return Err("FF-COMP-E-SEMANTIC-ARCHIVE: corpus duplicate case did not exercise the duplicate branch".to_owned());
    }
    Ok("duplicate-history-classification".to_owned())
}

fn replay_failure_case(fixture: &Value) -> Result<String, String> {
    let input = fixture
        .get("input")
        .and_then(Value::as_object)
        .ok_or("FF-COMP-E-SEMANTIC-FAILURE: input is absent")?;
    if input.get("operation").and_then(Value::as_str) != Some("network-request")
        || input.get("deadline_ms").and_then(Value::as_u64) != Some(1_000)
    {
        return Err(
            "FF-COMP-E-SEMANTIC-FAILURE: timeout input is not the pinned network request boundary"
                .to_owned(),
        );
    }
    let observed = serde_json::json!({
        "category": "timeout",
        "retryable": true,
        "completion": "incomplete-evidence",
        "success": false,
    });
    if fixture.get("expected_error") != Some(&observed) {
        return Err("FF-COMP-E-SEMANTIC-FAILURE: timeout classification differs from expected evidence semantics".to_owned());
    }
    Ok("timeout-failure-envelope".to_owned())
}

fn replay_artifact_case(fixture: &Value) -> Result<String, String> {
    let plan = fixture
        .get("process_plan")
        .and_then(Value::as_array)
        .ok_or("FF-COMP-E-SEMANTIC-ARTIFACT: process_plan is absent")?;
    let observed_programs = plan
        .iter()
        .map(|step| step.get("program").and_then(Value::as_str))
        .collect::<Option<Vec<_>>>()
        .ok_or("FF-COMP-E-SEMANTIC-ARTIFACT: process plan program is absent")?;
    if observed_programs != ["ffmpeg", "ffprobe"]
        || plan.iter().any(|step| {
            step.get("timeout_policy").and_then(Value::as_str) != Some("bounded")
                || step
                    .get("argument_classes")
                    .and_then(Value::as_array)
                    .is_none_or(Vec::is_empty)
        })
    {
        return Err(
            "FF-COMP-E-SEMANTIC-ARTIFACT: native process plan is not bounded ffmpeg then ffprobe"
                .to_owned(),
        );
    }
    let artifacts = fixture
        .get("artifacts")
        .and_then(Value::as_array)
        .ok_or("FF-COMP-E-SEMANTIC-ARTIFACT: artifacts are absent")?;
    if artifacts.len() != 2
        || artifacts.iter().any(|artifact| {
            let path = artifact
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default();
            !safe_relative(path)
                || !path.starts_with("output/")
                || !matches!(
                    artifact.get("state").and_then(Value::as_str),
                    Some("durable-final" | "durable-sidecar")
                )
        })
    {
        return Err(
            "FF-COMP-E-SEMANTIC-ARTIFACT: durable output artifact boundary is invalid".to_owned(),
        );
    }
    Ok("normalized-process-artifact".to_owned())
}

fn replay_migration_case(fixture: &Value) -> Result<String, String> {
    let source = fixture
        .get("source")
        .and_then(Value::as_object)
        .ok_or("FF-COMP-E-SEMANTIC-MIGRATION: source is absent")?;
    let configuration = source
        .get("configuration")
        .and_then(Value::as_object)
        .ok_or("FF-COMP-E-SEMANTIC-MIGRATION: source configuration is absent")?;
    let format = configuration
        .get("format")
        .and_then(Value::as_str)
        .ok_or("FF-COMP-E-SEMANTIC-MIGRATION: source format is absent")?;
    let output = configuration
        .get("output")
        .and_then(Value::as_str)
        .ok_or("FF-COMP-E-SEMANTIC-MIGRATION: source output is absent")?;
    let observed = serde_json::json!({
        "schema_version": "1",
        "configuration": {
            "format_selector": format,
            "output_template": output,
        },
        "warnings": [],
    });
    if source.get("schema_version").and_then(Value::as_str) != Some("0")
        || fixture.get("expected") != Some(&observed)
    {
        return Err(
            "FF-COMP-E-SEMANTIC-MIGRATION: explicit v0-to-v1 mapping differs from expected result"
                .to_owned(),
        );
    }
    Ok("explicit-migration-mapping".to_owned())
}

fn replay_normalized_observation_case(fixture: &Value) -> Result<String, String> {
    let observation = fixture
        .get("observation")
        .and_then(Value::as_object)
        .ok_or("FF-COMP-E-SEMANTIC-NORMALIZATION: observation is absent")?;
    let extractor = observation
        .get("extractor_key")
        .and_then(Value::as_str)
        .ok_or("FF-COMP-E-SEMANTIC-NORMALIZATION: extractor_key is absent")?;
    let formats = observation
        .get("formats")
        .and_then(Value::as_array)
        .ok_or("FF-COMP-E-SEMANTIC-NORMALIZATION: formats are absent")?;
    let normalized_formats = formats
        .iter()
        .map(|format| format.as_str().map(ToOwned::to_owned))
        .collect::<Option<BTreeSet<_>>>()
        .ok_or("FF-COMP-E-SEMANTIC-NORMALIZATION: format is non-string")?;
    if !extractor.eq_ignore_ascii_case("generic")
        || normalized_formats != BTreeSet::from(["audio-128".to_owned(), "video-720".to_owned()])
        || observation.get("selected_format").and_then(Value::as_str) != Some("video-720+audio-128")
        || observation.get("output_filename").and_then(Value::as_str)
            != Some("Fixture_Alpha_[alpha].mkv")
        || observation.get("timestamp").and_then(Value::as_str) != Some("{{TIMESTAMP}}")
    {
        return Err(
            "FF-COMP-E-SEMANTIC-NORMALIZATION: canonical observation invariants failed".to_owned(),
        );
    }
    Ok("stable-normalized-observation".to_owned())
}

fn replay_sanitized_transcript_case(fixture: &Value) -> Result<String, String> {
    validate_sanitized_value(fixture)?;
    let exchanges = fixture
        .get("exchanges")
        .and_then(Value::as_array)
        .ok_or("FF-COMP-E-SEMANTIC-TRANSCRIPT: exchanges are absent")?;
    let response_digest = exchanges
        .first()
        .and_then(|exchange| exchange.pointer("/response/body_sha256"))
        .and_then(Value::as_str)
        .ok_or("FF-COMP-E-SEMANTIC-TRANSCRIPT: response body digest is absent")?;
    if exchanges.len() != 1
        || !valid_sha256(response_digest)
        || fixture.get("clock").and_then(Value::as_str) != Some("{{TIMESTAMP}}")
        || fixture.get("random_seed").and_then(Value::as_str) != Some("{{RANDOM_SEED}}")
    {
        return Err(
            "FF-COMP-E-SEMANTIC-TRANSCRIPT: transcript is not a bounded sanitized replay input"
                .to_owned(),
        );
    }
    Ok("sanitized-offline-replay".to_owned())
}

fn replay_source_graph_case(fixture: &Value) -> Result<String, String> {
    let observation = fixture
        .get("observation")
        .and_then(Value::as_object)
        .ok_or("FF-COMP-E-SEMANTIC-SOURCE-GRAPH: observation is absent")?;
    let root_id = observation
        .get("root_id")
        .and_then(Value::as_str)
        .ok_or("FF-COMP-E-SEMANTIC-SOURCE-GRAPH: root_id is absent")?;
    let nodes = observation
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or("FF-COMP-E-SEMANTIC-SOURCE-GRAPH: nodes are absent")?;
    let mut children = BTreeMap::new();
    for node in nodes {
        let id = node
            .get("id")
            .and_then(Value::as_str)
            .ok_or("FF-COMP-E-SEMANTIC-SOURCE-GRAPH: node id is absent")?;
        let node_children = node
            .get("children")
            .and_then(Value::as_array)
            .ok_or("FF-COMP-E-SEMANTIC-SOURCE-GRAPH: node children are absent")?
            .iter()
            .map(|child| child.as_str().map(ToOwned::to_owned))
            .collect::<Option<Vec<_>>>()
            .ok_or("FF-COMP-E-SEMANTIC-SOURCE-GRAPH: child id is non-string")?;
        if children.insert(id.to_owned(), node_children).is_some() {
            return Err("FF-COMP-E-SEMANTIC-SOURCE-GRAPH: duplicate node id".to_owned());
        }
    }
    if !children.contains_key(root_id)
        || children.len() != 3
        || children
            .values()
            .flatten()
            .any(|child| !children.contains_key(child))
        || source_graph_has_cycle(&children)
    {
        return Err(
            "FF-COMP-E-SEMANTIC-SOURCE-GRAPH: graph expansion is dangling, cyclic, or incomplete"
                .to_owned(),
        );
    }
    Ok("direct-source-graph-expansion".to_owned())
}

fn source_graph_has_cycle(graph: &BTreeMap<String, Vec<String>>) -> bool {
    let mut indegree = graph
        .keys()
        .map(|node| (node.clone(), 0_usize))
        .collect::<BTreeMap<_, _>>();
    for children in graph.values() {
        for child in children {
            *indegree.entry(child.clone()).or_default() += 1;
        }
    }
    let mut ready = indegree
        .iter()
        .filter_map(|(node, degree)| (*degree == 0).then_some(node.clone()))
        .collect::<Vec<_>>();
    let mut visited = 0_usize;
    while let Some(node) = ready.pop() {
        visited += 1;
        for child in graph.get(&node).into_iter().flatten() {
            let degree = indegree.get_mut(child).expect("graph child has indegree");
            *degree = degree.saturating_sub(1);
            if *degree == 0 {
                ready.push(child.clone());
            }
        }
    }
    visited != graph.len()
}

fn validate_live_manifest(
    live: &LiveManifest,
    profile: &CompatibilityProfile,
) -> Result<(), String> {
    if live.schema_id != "ff.compatibility-live-canaries@1"
        || live.schema_version != "1.0.0"
        || live.profile_id != profile.profile_id
        || live.default_enabled
        || live.deterministic_proof
        || live.canaries.is_empty()
    {
        return Err(
            "FF-COMP-E-LIVE-POLICY: live canary suite is not mechanically opt-in".to_owned(),
        );
    }
    require_unique_stable_ids(live.canaries.iter().map(|canary| canary.id.as_str()))?;
    if live.canaries.iter().any(|canary| {
        !canary.url.starts_with("https://")
            || canary.timeout_seconds == 0
            || canary.timeout_seconds > 60
            || !stable_id(&canary.url_class)
            || !profile
                .extractors
                .iter()
                .any(|extractor| extractor.url_class == canary.url_class)
            || canary.credential_policy != "operator-supplied-outside-repository-or-none"
            || canary.expected_classification != "nondeterministic_observation"
            || !live_destination_is_allowlisted(canary)
    }) {
        return Err("FF-COMP-E-LIVE-POLICY: live canary row violates policy".to_owned());
    }
    require_canonical_json_digest(
        live,
        LIVE_MANIFEST_SHA256,
        "FF-COMP-E-LIVE-INTEGRITY: pinned live manifest content mismatch",
    )
}

fn live_destination_is_allowlisted(canary: &LiveCanary) -> bool {
    matches!(
        (
            canary.id.as_str(),
            canary.url_class.as_str(),
            canary.url.as_str()
        ),
        (
            "live-canary-youtube-test-video-v1",
            "url-class-youtube",
            "https://www.youtube.com/watch?v=BaW_jenozKc"
        ) | (
            "live-canary-vimeo-public-v1",
            "url-class-vimeo",
            "https://vimeo.com/56015672"
        )
    )
}

fn validate_negative_fixtures(
    root: &Path,
    manifest: &OracleManifest,
    profile: &CompatibilityProfile,
    corpus: &CorpusManifest,
) -> Result<Vec<NegativeResult>, String> {
    let mut results = Vec::new();
    let mut ids = BTreeSet::new();
    for entry in fs::read_dir(root.join(NEGATIVE_ROOT)).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_dir()
        {
            return Err("FF-COMP-E-NEGATIVE-INVENTORY: non-directory entry".to_owned());
        }
        let case: NegativeCase = read_json(&entry.path().join("case.json"))?;
        let directory_id = entry.file_name().to_string_lossy().into_owned();
        if case.schema_version != 1
            || case.fixture_id != directory_id
            || !ids.insert(case.fixture_id.clone())
        {
            return Err(
                "FF-COMP-E-NEGATIVE-INVENTORY: invalid or duplicate fixture identity".to_owned(),
            );
        }
        let observed = exercise_mutation(&case.mutation, manifest, profile, corpus)
            .err()
            .ok_or_else(|| {
                format!(
                    "negative fixture {} produced no diagnostic",
                    case.fixture_id
                )
            })?;
        if observed != case.expected_diagnostic {
            return Err(format!(
                "negative fixture {} failed for wrong reason: expected {:?}, observed {:?}",
                case.fixture_id, case.expected_diagnostic, observed
            ));
        }
        results.push(NegativeResult {
            fixture_id: case.fixture_id,
            status: "REJECTED_AS_EXPECTED",
            expected_diagnostic: case.expected_diagnostic.clone(),
            observed_diagnostic: observed,
        });
    }
    results.sort_by(|left, right| left.fixture_id.cmp(&right.fixture_id));
    let expected_ids = REQUIRED_NEGATIVE_FIXTURES
        .iter()
        .map(|id| (*id).to_owned())
        .collect::<BTreeSet<_>>();
    if ids != expected_ids {
        return Err(format!(
            "FF-COMP-E-NEGATIVE-INVENTORY: expected exact required set of {} mutations",
            expected_ids.len()
        ));
    }
    Ok(results)
}

fn exercise_mutation(
    mutation: &str,
    manifest: &OracleManifest,
    profile: &CompatibilityProfile,
    corpus: &CorpusManifest,
) -> Result<(), String> {
    match mutation {
        "remove_option_row"
        | "remove_option_row_and_count"
        | "duplicate_option_id"
        | "unstable_option_id" => exercise_profile_mutation(mutation, manifest, profile),
        "unpin_oracle_version" | "unpin_oracle_artifact" | "unsafe_oracle_command" => {
            exercise_oracle_mutation(mutation, manifest)
        }
        "unknown_normalization"
        | "change_expected_outcome"
        | "enable_network_for_deterministic_case"
        | "duplicate_shard_case" => exercise_corpus_mutation(mutation, corpus),
        "inject_unsanitized_secret" => validate_sanitized_value(&serde_json::json!({
            "headers": {"Authorization": "Bearer actual-secret"}
        })),
        "invalid_candidate_digest"
        | "accepted_divergence_without_decision"
        | "accepted_divergence_with_unauthorized_decision"
        | "nondeterministic_classification_for_offline_case" => {
            exercise_candidate_mutation(mutation, corpus, profile)
        }
        other => Err(format!("FF-COMP-E-UNKNOWN-MUTATION: {other}")),
    }
}

fn exercise_profile_mutation(
    mutation: &str,
    manifest: &OracleManifest,
    profile: &CompatibilityProfile,
) -> Result<(), String> {
    let mut changed = profile.clone();
    match mutation {
        "remove_option_row" => {
            changed.options.pop();
        }
        "remove_option_row_and_count" => {
            changed.options.pop();
            changed.counts.options = changed.options.len();
        }
        "duplicate_option_id" => {
            let duplicate = changed
                .options
                .first()
                .ok_or("profile has no option")?
                .clone();
            changed.options.push(duplicate);
            changed.counts.options += 1;
        }
        "unstable_option_id" => {
            "Option Has Spaces".clone_into(
                &mut changed
                    .options
                    .first_mut()
                    .ok_or("profile has no option")?
                    .id,
            );
        }
        other => return Err(format!("FF-COMP-E-UNKNOWN-MUTATION: {other}")),
    }
    validate_profile(&changed, manifest)
}

fn exercise_oracle_mutation(mutation: &str, manifest: &OracleManifest) -> Result<(), String> {
    let mut changed = manifest.clone();
    match mutation {
        "unpin_oracle_version" => "latest".clone_into(&mut changed.release.version),
        "unpin_oracle_artifact" => {
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".clone_into(
                &mut changed
                    .artifacts
                    .iter_mut()
                    .find(|artifact| artifact.artifact_id == "yt-dlp-windows-x64")
                    .ok_or("oracle executable artifact is absent")?
                    .sha256,
            );
        }
        "unsafe_oracle_command" => {
            "--dump-json".clone_into(
                changed
                    .generator
                    .commands
                    .first_mut()
                    .and_then(|command| command.last_mut())
                    .ok_or("oracle command is absent")?,
            );
        }
        other => return Err(format!("FF-COMP-E-UNKNOWN-MUTATION: {other}")),
    }
    validate_oracle_manifest(&changed)
}

fn exercise_corpus_mutation(mutation: &str, corpus: &CorpusManifest) -> Result<(), String> {
    let mut changed = corpus.clone();
    match mutation {
        "unknown_normalization" => {
            "unknown".clone_into(
                &mut changed
                    .cases
                    .first_mut()
                    .ok_or("corpus has no case")?
                    .normalization_version,
            );
        }
        "change_expected_outcome" => {
            "silently-redefined".clone_into(
                &mut changed
                    .cases
                    .first_mut()
                    .ok_or("corpus has no case")?
                    .expected_outcome,
            );
        }
        "enable_network_for_deterministic_case" => {
            changed
                .cases
                .first_mut()
                .ok_or("corpus has no case")?
                .network_allowed = true;
        }
        "duplicate_shard_case" => {
            let duplicate = changed.cases.first().ok_or("corpus has no case")?.clone();
            changed.cases.push(duplicate);
            return require_unique_stable_ids(changed.cases.iter().map(|case| case.id.as_str()));
        }
        other => return Err(format!("FF-COMP-E-UNKNOWN-MUTATION: {other}")),
    }
    validate_corpus_without_files(&changed)
}

fn exercise_candidate_mutation(
    mutation: &str,
    corpus: &CorpusManifest,
    profile: &CompatibilityProfile,
) -> Result<(), String> {
    let mut candidate = match mutation {
        "invalid_candidate_digest" => candidate_with_difference(corpus, None, Some("not-a-digest")),
        "accepted_divergence_without_decision"
        | "accepted_divergence_with_unauthorized_decision" => {
            candidate_with_difference(corpus, Some("accepted_divergence"), None)
        }
        "nondeterministic_classification_for_offline_case" => {
            candidate_with_difference(corpus, Some("nondeterministic_response"), None)
        }
        other => return Err(format!("FF-COMP-E-UNKNOWN-MUTATION: {other}")),
    };
    if mutation == "accepted_divergence_with_unauthorized_decision" {
        candidate.observations[0].decision_id = Some("invented-decision".to_owned());
    }
    differential_rows(corpus, profile, &candidate).map(|_| ())
}

fn candidate_with_difference(
    corpus: &CorpusManifest,
    classification: Option<&str>,
    digest: Option<&str>,
) -> CandidateResults {
    CandidateResults {
        schema_id: "ff.compatibility-candidate-results@1".to_owned(),
        schema_version: "1.0.0".to_owned(),
        corpus_id: corpus.corpus_id.clone(),
        profile_id: corpus.profile_id.clone(),
        observations: vec![CandidateObservation {
            case_id: corpus.cases[0].id.clone(),
            observed_digest: digest
                .unwrap_or("0000000000000000000000000000000000000000000000000000000000000000")
                .to_owned(),
            classification: classification.map(str::to_owned),
            decision_id: None,
        }],
    }
}

fn validate_corpus_without_files(corpus: &CorpusManifest) -> Result<(), String> {
    if corpus.schema_id != "ff.compatibility-corpus@1"
        || corpus.schema_version != "1.0.0"
        || corpus.corpus_id != "ff-ytdlp-2026.07.04-corpus-v1"
        || corpus.profile_id != "yt-dlp-2026.07.04-profile-v1"
        || corpus.normalization_versions != [NORMALIZATION_VERSION]
        || corpus.shard_algorithm != "sha256-stable-id-mod-v1"
        || corpus.shard_count == 0
    {
        return Err("FF-COMP-E-CORPUS-IDENTITY: corpus identity or version mismatch".to_owned());
    }
    let expected_planes = BTreeSet::from([
        "archive",
        "failure",
        "filesystem_process_artifact",
        "migration",
        "normalized_observation",
        "sanitized_network_transcript",
        "source_graph",
    ]);
    let declared_planes = corpus
        .planes
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if declared_planes != expected_planes || corpus.planes.len() != expected_planes.len() {
        return Err("FF-COMP-E-COVERAGE: corpus plane inventory is incomplete".to_owned());
    }
    require_unique_stable_ids(corpus.cases.iter().map(|case| case.id.as_str()))?;
    let mut observed_planes = BTreeSet::new();
    for case in &corpus.cases {
        observed_planes.insert(case.plane.as_str());
        if !expected_planes.contains(case.plane.as_str()) {
            return Err(format!("FF-COMP-E-COVERAGE: unknown plane {}", case.plane));
        }
        if case.normalization_version != NORMALIZATION_VERSION {
            return Err("FF-COMP-E-NORMALIZATION: unknown normalization version".to_owned());
        }
        if case.deterministic && case.network_allowed {
            return Err(
                "FF-COMP-E-DETERMINISTIC-NETWORK: deterministic case permits network".to_owned(),
            );
        }
        if !case.deterministic || case.expected_outcome.trim().is_empty() {
            return Err(format!(
                "FF-COMP-E-COVERAGE: mandatory case {} is incomplete",
                case.id
            ));
        }
        if case.shard != shard_for(&case.id, corpus.shard_count) {
            return Err(format!("FF-COMP-E-SHARD: {} has incorrect shard", case.id));
        }
        if !case
            .fixture
            .starts_with("build/fixtures/compatibility/cases/")
            || !safe_relative(&case.fixture)
            || !valid_sha256(&case.fixture_sha256)
        {
            return Err(format!(
                "FF-COMP-E-FIXTURE-PATH: unsafe fixture row {}",
                case.id
            ));
        }
    }
    if observed_planes != expected_planes {
        return Err("FF-COMP-E-COVERAGE: one or more mandatory planes have no case".to_owned());
    }
    require_canonical_json_digest(
        corpus,
        CORPUS_MANIFEST_SHA256,
        "FF-COMP-E-CORPUS-INTEGRITY: pinned corpus content mismatch",
    )
}

fn validate_sanitized_value(value: &Value) -> Result<(), String> {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if sensitive_key(key)
                    && value
                        .as_str()
                        .is_none_or(|text| !is_sanitized_placeholder(text))
                {
                    return Err(format!("FF-COMP-E-UNSANITIZED-SECRET: key={key}"));
                }
                validate_sanitized_value(value)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                validate_sanitized_value(value)?;
            }
        }
        Value::String(text) => {
            let lower = text.to_ascii_lowercase();
            if lower.contains("c:\\users\\")
                || lower.contains("c:/users/")
                || lower.starts_with("/home/")
                || lower.starts_with("/users/")
                || lower.starts_with("\\\\")
                || lower.starts_with("//")
                || contains_unsanitized_assignment(text, "bearer ")
                || contains_unsanitized_assignment(text, "api_key=")
                || contains_unsanitized_assignment(text, "api-key=")
                || contains_unsanitized_assignment(text, "token=")
                || contains_unsanitized_assignment(text, "access_token=")
                || contains_unsanitized_assignment(text, "password=")
                || contains_unsanitized_assignment(text, "client_secret=")
                || contains_unsanitized_assignment(text, "secret=")
                || contains_url_userinfo(text)
            {
                return Err(
                    "FF-COMP-E-UNSANITIZED-SECRET: sensitive or machine-local string".to_owned(),
                );
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
    Ok(())
}

fn sensitive_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace('-', "_");
    matches!(
        normalized.as_str(),
        "authorization"
            | "proxy_authorization"
            | "cookie"
            | "set_cookie"
            | "api_key"
            | "x_api_key"
            | "access_token"
            | "refresh_token"
            | "id_token"
            | "token"
            | "password"
            | "client_secret"
            | "secret"
            | "session"
    )
}

fn contains_url_userinfo(text: &str) -> bool {
    let Some(scheme) = text.find("://") else {
        return false;
    };
    let authority = &text[scheme + 3..];
    let authority = authority.split(['/', '?', '#']).next().unwrap_or_default();
    authority.contains('@')
}

fn contains_unsanitized_assignment(text: &str, assignment: &str) -> bool {
    let mut remaining = text;
    while let Some(offset) = remaining.to_ascii_lowercase().find(assignment) {
        let value = &remaining[offset + assignment.len()..];
        let Some(close) = value.find("}}") else {
            return true;
        };
        let end = close + 2;
        if !is_sanitized_placeholder(&value[..end])
            || value[end..]
                .chars()
                .next()
                .is_some_and(|character| !matches!(character, '&' | '#' | ' ' | '\t'))
        {
            return true;
        }
        remaining = &value[end..];
    }
    false
}

fn is_sanitized_placeholder(value: &str) -> bool {
    let Some(inner) = value
        .strip_prefix("{{")
        .and_then(|value| value.strip_suffix("}}"))
    else {
        return false;
    };
    SANITIZED_PLACEHOLDERS.contains(&inner)
}

fn differential_rows(
    corpus: &CorpusManifest,
    profile: &CompatibilityProfile,
    candidate: &CandidateResults,
) -> Result<Vec<DifferentialRow>, String> {
    validate_profile_structure(profile)?;
    validate_current_profile_content(profile)?;
    validate_corpus_without_files(corpus)?;
    validate_candidate_input(corpus, profile, candidate)?;
    let observed = candidate
        .observations
        .iter()
        .map(|row| (row.case_id.as_str(), row))
        .collect::<BTreeMap<_, _>>();
    let mut rows = Vec::new();
    for case in &corpus.cases {
        let Some(candidate_row) = observed.get(case.id.as_str()) else {
            rows.push(DifferentialRow {
                case_id: case.id.clone(),
                status: "DIFFERENT",
                classification: "missing_feature".to_owned(),
                decision_id: None,
                expected_digest: case.fixture_sha256.clone(),
                observed_digest: None,
            });
            continue;
        };
        if candidate_row.observed_digest == case.fixture_sha256 {
            if candidate_row.classification.is_some() || candidate_row.decision_id.is_some() {
                return Err(
                    "FF-COMP-E-CLASSIFICATION: equivalent row carries difference metadata"
                        .to_owned(),
                );
            }
            rows.push(DifferentialRow {
                case_id: case.id.clone(),
                status: "EQUIVALENT",
                classification: "equivalent".to_owned(),
                decision_id: None,
                expected_digest: case.fixture_sha256.clone(),
                observed_digest: Some(candidate_row.observed_digest.clone()),
            });
            continue;
        }
        let classification = difference_classification(case, candidate_row)?;
        rows.push(DifferentialRow {
            case_id: case.id.clone(),
            status: "DIFFERENT",
            classification: classification.to_owned(),
            decision_id: candidate_row.decision_id.clone(),
            expected_digest: case.fixture_sha256.clone(),
            observed_digest: Some(candidate_row.observed_digest.clone()),
        });
    }
    Ok(rows)
}

fn validate_candidate_input(
    corpus: &CorpusManifest,
    profile: &CompatibilityProfile,
    candidate: &CandidateResults,
) -> Result<(), String> {
    if candidate.schema_id != "ff.compatibility-candidate-results@1"
        || candidate.schema_version != "1.0.0"
        || candidate.corpus_id != corpus.corpus_id
        || candidate.profile_id != profile.profile_id
    {
        return Err("FF-COMP-E-CANDIDATE-IDENTITY: candidate input mismatch".to_owned());
    }
    require_unique_stable_ids(
        candidate
            .observations
            .iter()
            .map(|row| row.case_id.as_str()),
    )?;
    let expected_ids = corpus
        .cases
        .iter()
        .map(|case| case.id.as_str())
        .collect::<BTreeSet<_>>();
    if candidate
        .observations
        .iter()
        .any(|row| !expected_ids.contains(row.case_id.as_str()))
    {
        return Err("FF-COMP-E-CANDIDATE-UNKNOWN: candidate contains unknown case".to_owned());
    }
    if candidate
        .observations
        .iter()
        .any(|row| !valid_sha256(&row.observed_digest))
    {
        return Err("FF-COMP-E-CANDIDATE-DIGEST: candidate digest is not SHA-256".to_owned());
    }
    Ok(())
}

fn difference_classification<'a>(
    case: &CorpusCase,
    candidate: &'a CandidateObservation,
) -> Result<&'a str, String> {
    let classification = candidate
        .classification
        .as_deref()
        .unwrap_or("ferric_defect");
    if !matches!(
        classification,
        "accepted_baseline_correction"
            | "accepted_divergence"
            | "ferric_defect"
            | "nondeterministic_response"
    ) {
        return Err(format!(
            "FF-COMP-E-CLASSIFICATION: unknown {classification}"
        ));
    }
    if classification == "nondeterministic_response" && case.deterministic {
        return Err(
            "FF-COMP-E-CLASSIFICATION: deterministic case cannot be nondeterministic_response"
                .to_owned(),
        );
    }
    if matches!(
        classification,
        "accepted_divergence" | "accepted_baseline_correction"
    ) {
        let Some(decision_id) = candidate.decision_id.as_deref() else {
            return Err(
                "FF-COMP-E-DIVERGENCE-DECISION: accepted divergence lacks stable decision ID"
                    .to_owned(),
            );
        };
        if !stable_id(decision_id) {
            return Err(
                "FF-COMP-E-DIVERGENCE-DECISION: accepted divergence lacks stable decision ID"
                    .to_owned(),
            );
        }
        if !ACCEPTED_DIVERGENCE_DECISIONS.contains(&decision_id) {
            return Err(format!(
                "FF-COMP-E-DIVERGENCE-DECISION: decision ID is not authorized: {decision_id}"
            ));
        }
    } else if candidate.decision_id.is_some() {
        return Err(
            "FF-COMP-E-DIVERGENCE-DECISION: non-accepted row carries a decision ID".to_owned(),
        );
    }
    Ok(classification)
}

fn inventory_diff(
    before: &CompatibilityProfile,
    after: &CompatibilityProfile,
    source: CompatibilitySource,
    inputs: BTreeMap<String, String>,
) -> Result<InventoryDiffReport, String> {
    validate_profile_structure(before)?;
    validate_profile_structure(after)?;
    if before.profile_id == after.profile_id
        && canonical_json_sha256(before)? != canonical_json_sha256(after)?
    {
        return Err(format!(
            "FF-COMP-E-PROFILE-INTEGRITY: profile ID {} has divergent content",
            before.profile_id
        ));
    }
    let options = inventory_delta(&before.options, &after.options, |row| &row.id);
    let presets = inventory_delta(&before.presets, &after.presets, |row| &row.id);
    let interactions = inventory_delta(&before.interactions, &after.interactions, |row| &row.id);
    let extractors = inventory_delta(&before.extractors, &after.extractors, |row| &row.id);
    let descriptions = inventory_delta(
        &before.extractor_descriptions,
        &after.extractor_descriptions,
        |row| &row.id,
    );
    Ok(InventoryDiffReport {
        schema_id: "ff.compatibility-inventory-diff@2",
        schema_version: "2.0.0",
        status: "STRUCTURAL_ONLY",
        source,
        inputs,
        before_profile_id: before.profile_id.clone(),
        after_profile_id: after.profile_id.clone(),
        added_options: options.added,
        removed_options: options.removed,
        changed_options: options.changed,
        added_presets: presets.added,
        removed_presets: presets.removed,
        changed_presets: presets.changed,
        added_interactions: interactions.added,
        removed_interactions: interactions.removed,
        changed_interactions: interactions.changed,
        added_extractors: extractors.added,
        removed_extractors: extractors.removed,
        changed_extractors: extractors.changed,
        added_extractor_descriptions: descriptions.added,
        removed_extractor_descriptions: descriptions.removed,
        changed_extractor_descriptions: descriptions.changed,
        proof_limitations: vec![
            "Inventory diffing reports stable-row additions, removals, and changes; it does not prove behavioral parity or explain the cause of upstream drift.".to_owned(),
        ],
    })
}

fn inventory_delta<T: Serialize, F>(before: &[T], after: &[T], id: F) -> InventoryDelta
where
    F: Fn(&T) -> &String + Copy,
{
    let before = before
        .iter()
        .map(|row| (id(row), row))
        .collect::<BTreeMap<_, _>>();
    let after = after
        .iter()
        .map(|row| (id(row), row))
        .collect::<BTreeMap<_, _>>();
    InventoryDelta {
        added: set_difference(after.keys().copied(), before.keys().copied()),
        removed: set_difference(before.keys().copied(), after.keys().copied()),
        changed: changed_rows(&before, &after),
    }
}

fn changed_rows<T: Serialize>(
    before: &BTreeMap<&String, &T>,
    after: &BTreeMap<&String, &T>,
) -> Vec<String> {
    before
        .iter()
        .filter_map(|(id, row)| {
            after
                .get(id)
                .filter(|other| serde_json::to_value(row).ok() != serde_json::to_value(other).ok())
                .map(|_| (*id).clone())
        })
        .collect()
}

fn set_difference<'a>(
    left: impl Iterator<Item = &'a String>,
    right: impl Iterator<Item = &'a String>,
) -> Vec<String> {
    let left = left.cloned().collect::<BTreeSet<_>>();
    let right = right.cloned().collect::<BTreeSet<_>>();
    left.difference(&right).cloned().collect()
}

#[allow(clippy::too_many_arguments)]
fn base_report(
    root: &Path,
    command: &str,
    status: &'static str,
    execution_scope: &'static str,
    extra_inputs: BTreeMap<String, String>,
    checks: Vec<CompatibilityCheck>,
    semantic_replays: Vec<SemanticReplay>,
    negative_fixtures: Vec<NegativeResult>,
    differential_rows: Vec<DifferentialRow>,
) -> Result<CompatibilityReport, String> {
    let source = source_state(root)?;
    let mut inputs = [ORACLE_PATH, PROFILE_PATH, CORPUS_PATH, LIVE_PATH]
        .into_iter()
        .map(|path| Ok((path.to_owned(), sha256_file(&root.join(path))?)))
        .collect::<Result<BTreeMap<_, _>, String>>()?;
    for (key, digest) in extra_inputs {
        if inputs.insert(key.clone(), digest).is_some() {
            return Err(format!("duplicate report input key {key}"));
        }
    }
    let executed_proof_classes = executed_proof_classes(&checks, &semantic_replays);
    let aggregate_proof_class = aggregate_proof_class(&executed_proof_classes);
    let declared_supported_proof_classes = declared_supported_proof_classes(command);
    let report = CompatibilityReport {
        schema_id: "ff.compatibility-report@1",
        schema_version: "1.0.0",
        command: command.to_owned(),
        status,
        source: CompatibilitySource {
            git_commit: source.git_commit,
            dirty: source.dirty,
            dirty_paths: source.dirty_paths,
            content_fingerprint: source.content_fingerprint,
        },
        inputs,
        declared_supported_proof_classes,
        executed_proof_classes,
        execution_scope,
        checks,
        semantic_replays,
        aggregate_proof_class,
        negative_fixtures,
        differential_rows,
        artifacts: vec![
            PROFILE_PATH.to_owned(),
            CORPUS_PATH.to_owned(),
            "build/reports".to_owned(),
        ],
        proof_limitations: vec![
            "Corpus validation and report completeness do not prove Ferric feature parity.".to_owned(),
            "Live canaries are opt-in nondeterministic observations and never satisfy offline deterministic acceptance.".to_owned(),
            "The generated profile inventories pinned oracle surfaces; later compatibility rows remain missing features until implemented and compared.".to_owned(),
        ],
    };
    validate_report_evidence(&report)?;
    Ok(report)
}

pub(super) fn validate_replay_report_evidence_for_architecture(
    status: &str,
    execution_scope: &str,
    executed_proof_classes: &[&str],
    semantic_replay_count: usize,
) -> Result<(), &'static str> {
    let semantic_status = matches!(
        status,
        "SEMANTIC_REPLAY_EXECUTED" | "SEMANTIC_REPLAY_SUBSET_EXECUTED"
    );
    if semantic_status
        && (semantic_replay_count == 0 || !executed_proof_classes.contains(&"semantic"))
    {
        return Err("FF-ARCH-E-STRUCTURAL-BEHAVIORAL-PASS");
    }
    if status == "SEMANTIC_REPLAY_EXECUTED" && execution_scope != "complete_corpus" {
        return Err("FF-ARCH-E-PROOF-CLASS-PROMOTION");
    }
    if status == "SEMANTIC_REPLAY_SUBSET_EXECUTED" && execution_scope != "selected_shard_only" {
        return Err("FF-ARCH-E-PROOF-CLASS-PROMOTION");
    }
    Ok(())
}

/// Read the exact report emitted by a replay child process and validate it as
/// an artifact. Callers must not treat the child's exit status as semantic
/// evidence without this independent report-boundary check.
pub(super) fn read_replay_report_evidence(
    root: &Path,
    report_path: &Path,
) -> Result<ReplayReportEvidence, String> {
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("FF-COMP-E-REPLAY-REPORT-PATH: canonicalize root: {error}"))?;
    let canonical_report_path = report_path.canonicalize().map_err(|error| {
        format!("FF-COMP-E-REPLAY-REPORT-PATH: canonicalize replay report: {error}")
    })?;
    let relative_report_path = canonical_report_path
        .strip_prefix(&canonical_root)
        .map_err(|error| format!("FF-COMP-E-REPLAY-REPORT-PATH: {error}"))?;
    let report: ReplayReportArtifact = read_json(report_path)?;
    validate_replay_report_artifact(&report)?;
    validate_replay_report_input_provenance(root, &report)?;
    validate_replay_report_source_provenance(root, &report)?;

    let corpus: CorpusManifest = read_json(&root.join(CORPUS_PATH))?;
    validate_corpus_without_files(&corpus)?;
    let expected_ids = corpus
        .cases
        .iter()
        .map(|case| case.id.clone())
        .collect::<BTreeSet<_>>();
    let observed_ids = report
        .semantic_replays
        .iter()
        .map(|replay| replay.case_id.clone())
        .collect::<BTreeSet<_>>();
    if report.execution_scope == "complete_corpus" && observed_ids != expected_ids {
        return Err(
            "FF-COMP-E-REPLAY-REPORT-COVERAGE: complete replay report does not contain exactly the canonical corpus cases"
                .to_owned(),
        );
    }
    if report.execution_scope == "selected_shard_only"
        && (observed_ids.is_empty() || !observed_ids.is_subset(&expected_ids))
    {
        return Err(
            "FF-COMP-E-REPLAY-REPORT-COVERAGE: selected shard report has an empty or unknown case set"
                .to_owned(),
        );
    }
    validate_replay_report_semantic_rows(root, &corpus, &report)?;

    Ok(ReplayReportEvidence {
        report_path: slash(relative_report_path),
        status: report.status,
        execution_scope: report.execution_scope,
        source_git_commit: report.source.git_commit,
        source_dirty: report.source.dirty,
        source_dirty_paths: report.source.dirty_paths,
        source_content_fingerprint: report.source.content_fingerprint,
        semantic_replays: report
            .semantic_replays
            .into_iter()
            .map(|replay| ReplayResultEvidence {
                case_id: replay.case_id,
                plane: replay.plane,
                concrete_input: replay.concrete_input,
                boundary: replay.boundary,
                expected_result: replay.expected_result,
                observed_result: replay.observed_result,
                skipped_semantic_dependencies: replay.skipped_semantic_dependencies,
            })
            .collect(),
    })
}

/// The child report is evidence only when it was emitted for the source state
/// that the parent can still observe.  A syntactically valid report from an
/// earlier checkout or dirty-tree state is not reusable semantic proof.
fn validate_replay_report_source_provenance(
    root: &Path,
    report: &ReplayReportArtifact,
) -> Result<(), String> {
    let current = super::source_state(root)?;
    if report.source.git_commit != current.git_commit
        || report.source.dirty != current.dirty
        || report.source.dirty_paths != current.dirty_paths
        || report.source.content_fingerprint != current.content_fingerprint
    {
        return Err(
            "FF-COMP-E-REPLAY-REPORT-SOURCE-STALE: replay report source provenance does not equal the current repository state"
                .to_owned(),
        );
    }
    Ok(())
}

/// Re-execute the deterministic native semantics from the canonical corpus
/// and compare every row.  This prevents a child process from self-attesting
/// a semantic status with label-only or stale row values.
fn validate_replay_report_semantic_rows(
    root: &Path,
    corpus: &CorpusManifest,
    report: &ReplayReportArtifact,
) -> Result<(), String> {
    let cases_by_id = corpus
        .cases
        .iter()
        .map(|case| (case.id.as_str(), case))
        .collect::<BTreeMap<_, _>>();
    for observed in &report.semantic_replays {
        let case = cases_by_id.get(observed.case_id.as_str()).ok_or_else(|| {
            format!(
                "FF-COMP-E-REPLAY-REPORT-SEMANTIC: replay row {} has no canonical corpus case",
                observed.case_id
            )
        })?;
        let fixture: Value = read_json(&root.join(&case.fixture))?;
        let expected = replay_case_semantics(case, &fixture)?;
        if observed.case_id != expected.case_id
            || observed.plane != expected.plane
            || observed.status != expected.status
            || observed.proof_class != expected.proof_class
            || observed.concrete_input != expected.concrete_input
            || observed.boundary != expected.boundary
            || observed.expected_result != expected.expected_result
            || observed.observed_result != expected.observed_result
            || observed.skipped_semantic_dependencies != expected.skipped_semantic_dependencies
        {
            return Err(format!(
                "FF-COMP-E-REPLAY-REPORT-SEMANTIC: replay row {} does not match independently recomputed canonical semantics",
                observed.case_id
            ));
        }
    }
    Ok(())
}

fn validate_replay_report_input_provenance(
    root: &Path,
    report: &ReplayReportArtifact,
) -> Result<(), String> {
    let required = [ORACLE_PATH, PROFILE_PATH, CORPUS_PATH, LIVE_PATH];
    if report.inputs.len() != required.len()
        || report
            .inputs
            .keys()
            .any(|path| !required.contains(&path.as_str()))
    {
        return Err(
            "FF-COMP-E-REPLAY-REPORT-INPUT: replay report input set is not the exact replay provenance set"
                .to_owned(),
        );
    }
    for path in required {
        let recorded = report.inputs.get(path).ok_or_else(|| {
            format!(
                "FF-COMP-E-REPLAY-REPORT-INPUT: replay report omits required input digest {path}"
            )
        })?;
        let actual = sha256_file(&root.join(path))?;
        if recorded != &actual {
            return Err(format!(
                "FF-COMP-E-REPLAY-REPORT-DIGEST: replay report digest for {path} does not match the current replay input"
            ));
        }
    }
    Ok(())
}

/// This creates a complete serialized replay-report artifact with semantic
/// status but no semantic executions. It is deliberately routed through the
/// same artifact validator used after report writing and by composed gates.
pub(super) fn structural_replay_report_mutation_diagnostic() -> Result<(), String> {
    let malformed = serde_json::json!({
        "schema_id": "ff.compatibility-report@1",
        "schema_version": "1.0.0",
        "command": "compatibility-replay",
        "status": "SEMANTIC_REPLAY_EXECUTED",
        "source": {"git_commit": "fixture", "dirty": false, "dirty_paths": [], "content_fingerprint": "0".repeat(64)},
        "inputs": {
            ORACLE_PATH: "0".repeat(64),
            PROFILE_PATH: "1".repeat(64),
            CORPUS_PATH: "2".repeat(64),
            LIVE_PATH: "3".repeat(64),
        },
        "declared_supported_proof_classes": ["semantic", "structural"],
        "executed_proof_classes": ["structural"],
        "execution_scope": "complete_corpus",
        "checks": [{
            "id": "offline-replay",
            "status": "STRUCTURAL_ONLY",
            "proof_class": "structural",
            "concrete_input": "corpus",
            "boundary": "fixture validator",
            "expected_result": "structural validation succeeds",
            "observed_result": "fixture loaded",
            "skipped_semantic_dependencies": ["semantic interpreter not executed"],
            "detail": "fixture loaded"
        }],
        "semantic_replays": [],
        "aggregate_proof_class": "structural",
        "negative_fixtures": [],
        "differential_rows": [],
        "artifacts": ["build/reports"],
        "proof_limitations": ["fixture mutation"]
    });
    let bytes = serde_json::to_vec(&malformed)
        .map_err(|error| format!("FF-COMP-E-REPLAY-REPORT-SERIALIZE: {error}"))?;
    let report: ReplayReportArtifact = serde_json::from_slice(&bytes)
        .map_err(|error| format!("FF-COMP-E-REPLAY-REPORT-PARSE: {error}"))?;
    validate_replay_report_artifact(&report)
}

#[allow(clippy::too_many_lines)]
fn validate_replay_report_artifact(report: &ReplayReportArtifact) -> Result<(), String> {
    if report.schema_id != "ff.compatibility-report@1"
        || report.schema_version != "1.0.0"
        || report.command != "compatibility-replay"
    {
        return Err("FF-COMP-E-REPLAY-REPORT-IDENTITY: invalid replay report identity".to_owned());
    }
    if !matches!(
        report.status.as_str(),
        "SEMANTIC_REPLAY_EXECUTED" | "SEMANTIC_REPLAY_SUBSET_EXECUTED"
    ) {
        return Err(
            "FF-COMP-E-REPLAY-REPORT-STATUS: replay report is not semantic execution evidence"
                .to_owned(),
        );
    }
    if report.source.git_commit.trim().is_empty()
        || report
            .source
            .dirty_paths
            .iter()
            .any(|path| path.trim().is_empty())
        || report.source.dirty == report.source.dirty_paths.is_empty()
        || !valid_sha256(&report.source.content_fingerprint)
    {
        return Err(
            "FF-COMP-E-REPLAY-REPORT-SOURCE: replay report has malformed source provenance"
                .to_owned(),
        );
    }
    for path in [ORACLE_PATH, PROFILE_PATH, CORPUS_PATH, LIVE_PATH] {
        if report
            .inputs
            .get(path)
            .is_none_or(|digest| !valid_sha256(digest))
        {
            return Err(format!(
                "FF-COMP-E-REPLAY-REPORT-INPUT: replay report omits or corrupts required input digest {path}"
            ));
        }
    }
    validate_report_string_set(
        &report.declared_supported_proof_classes,
        "FF-COMP-E-REPLAY-REPORT-DECLARED",
    )?;
    validate_report_string_set(
        &report.executed_proof_classes,
        "FF-COMP-E-REPLAY-REPORT-EXECUTED",
    )?;
    if !report
        .declared_supported_proof_classes
        .iter()
        .any(|class| class == "semantic")
    {
        return Err(
            "FF-COMP-E-PROOF-DECLARATION: semantic replay omitted declared semantic support"
                .to_owned(),
        );
    }

    let malformed_check = report.checks.iter().any(|check| {
        check.id.trim().is_empty()
            || check.status.trim().is_empty()
            || check.proof_class.trim().is_empty()
            || check.concrete_input.trim().is_empty()
            || check.boundary.trim().is_empty()
            || check.expected_result.trim().is_empty()
            || check.observed_result.trim().is_empty()
            || check
                .skipped_semantic_dependencies
                .iter()
                .any(|dependency| dependency.trim().is_empty())
            || check.detail.trim().is_empty()
    });
    if malformed_check {
        return Err(
            "FF-COMP-E-PROOF-RESULT-SHAPE: replay report check omits executed evidence".to_owned(),
        );
    }
    let mut semantic_ids = BTreeSet::new();
    for replay in &report.semantic_replays {
        if replay.case_id.trim().is_empty()
            || replay.plane.trim().is_empty()
            || replay.status != "SEMANTIC_PASS"
            || replay.proof_class != "semantic"
            || replay.concrete_input.trim().is_empty()
            || replay.boundary.trim().is_empty()
            || replay.expected_result.trim().is_empty()
            || replay.observed_result.trim().is_empty()
            || !semantic_ids.insert(replay.case_id.as_str())
        {
            return Err(
                "FF-COMP-E-PROOF-RESULT-SHAPE: replay report has malformed, nonsemantic, or duplicate semantic evidence"
                    .to_owned(),
            );
        }
    }
    if report.semantic_replays.is_empty() {
        return Err(
            "FF-COMP-E-SEMANTIC-EMPTY: semantic replay report contains no executed cases"
                .to_owned(),
        );
    }
    for result in &report.negative_fixtures {
        if result.fixture_id.trim().is_empty()
            || result.status.trim().is_empty()
            || result.expected_diagnostic.trim().is_empty()
            || result.observed_diagnostic.trim().is_empty()
        {
            return Err(
                "FF-COMP-E-REPLAY-REPORT-SHAPE: malformed negative fixture evidence".to_owned(),
            );
        }
    }
    for row in &report.differential_rows {
        if row.case_id.trim().is_empty()
            || row.status.trim().is_empty()
            || row.classification.trim().is_empty()
            || row.expected_digest.trim().is_empty()
            || row.decision_id.as_deref().is_some_and(str::is_empty)
            || row.observed_digest.as_deref().is_some_and(str::is_empty)
        {
            return Err(
                "FF-COMP-E-REPLAY-REPORT-SHAPE: malformed differential evidence".to_owned(),
            );
        }
    }
    if report.artifacts.iter().any(|path| path.trim().is_empty())
        || report
            .proof_limitations
            .iter()
            .any(|limitation| limitation.trim().is_empty())
    {
        return Err("FF-COMP-E-REPLAY-REPORT-SHAPE: empty artifact or proof limitation".to_owned());
    }

    let mut actual_classes = report
        .checks
        .iter()
        .map(|check| check.proof_class.as_str())
        .chain(
            report
                .semantic_replays
                .iter()
                .map(|replay| replay.proof_class.as_str()),
        )
        .collect::<Vec<_>>();
    actual_classes.sort_unstable();
    actual_classes.dedup();
    let recorded_classes = report
        .executed_proof_classes
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    if actual_classes != recorded_classes {
        return Err("FF-COMP-E-PROOF-CLASS-PROMOTION: replay report classes do not derive from result evidence".to_owned());
    }
    if report.aggregate_proof_class != aggregate_proof_class(&actual_classes) {
        return Err("FF-COMP-E-PROOF-CLASS-PROMOTION: replay report aggregate does not derive from result evidence".to_owned());
    }
    validate_replay_report_evidence_for_architecture(
        &report.status,
        &report.execution_scope,
        &actual_classes,
        report.semantic_replays.len(),
    )
    .map_err(ToOwned::to_owned)?;
    Ok(())
}

fn validate_report_string_set(values: &[String], diagnostic: &str) -> Result<(), String> {
    let observed = values.iter().map(String::as_str).collect::<BTreeSet<_>>();
    if values.is_empty()
        || values.iter().any(|value| value.trim().is_empty())
        || observed.len() != values.len()
        || values.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(format!(
            "{diagnostic}: report proof classes must be nonempty, unique, and sorted"
        ));
    }
    Ok(())
}

fn validate_report_evidence(report: &CompatibilityReport) -> Result<(), String> {
    let declared = report.declared_supported_proof_classes.clone();
    let executed_refs = report.executed_proof_classes.clone();
    let expected_aggregate = aggregate_proof_class(&executed_refs);
    if report.aggregate_proof_class != expected_aggregate {
        return Err(
            "FF-COMP-E-PROOF-CLASS-PROMOTION: aggregate must derive from executed evidence"
                .to_owned(),
        );
    }
    if report.checks.iter().any(|check| {
        check.proof_class.trim().is_empty()
            || check.concrete_input.trim().is_empty()
            || check.boundary.trim().is_empty()
            || check.expected_result.trim().is_empty()
            || check.observed_result.trim().is_empty()
    }) || report.semantic_replays.iter().any(|replay| {
        replay.proof_class.trim().is_empty()
            || replay.concrete_input.trim().is_empty()
            || replay.boundary.trim().is_empty()
            || replay.expected_result.trim().is_empty()
            || replay.observed_result.trim().is_empty()
    }) {
        return Err(
            "FF-COMP-E-PROOF-RESULT-SHAPE: executed result omits required evidence fields"
                .to_owned(),
        );
    }
    validate_replay_report_evidence_for_architecture(
        report.status,
        report.execution_scope,
        &executed_refs,
        report.semantic_replays.len(),
    )
    .map_err(ToOwned::to_owned)?;
    if report.status == "SEMANTIC_REPLAY_EXECUTED"
        && report.execution_scope == "complete_corpus"
        && !declared.contains(&"semantic")
    {
        return Err(
            "FF-COMP-E-PROOF-DECLARATION: semantic replay omitted its declared support".to_owned(),
        );
    }
    if report.command == "compatibility-replay" {
        let artifact: ReplayReportArtifact = serde_json::from_value(
            serde_json::to_value(report)
                .map_err(|error| format!("FF-COMP-E-REPLAY-REPORT-SERIALIZE: {error}"))?,
        )
        .map_err(|error| format!("FF-COMP-E-REPLAY-REPORT-PARSE: {error}"))?;
        validate_replay_report_artifact(&artifact)?;
    }
    Ok(())
}

fn declared_supported_proof_classes(command: &str) -> Vec<&'static str> {
    match command {
        "compatibility-replay" => vec!["semantic", "structural"],
        "compatibility-live-canaries" => vec!["integration_observation"],
        _ => vec!["structural"],
    }
}

fn executed_proof_classes(
    checks: &[CompatibilityCheck],
    semantic_replays: &[SemanticReplay],
) -> Vec<&'static str> {
    let mut observed = checks
        .iter()
        .map(|check| check.proof_class)
        .chain(semantic_replays.iter().map(|replay| replay.proof_class))
        .collect::<Vec<_>>();
    observed.sort_unstable();
    observed.dedup();
    observed
}

fn aggregate_proof_class(classes: &[&str]) -> &'static str {
    if classes.contains(&"structural") {
        "structural"
    } else if classes.contains(&"semantic") {
        "semantic"
    } else if classes.contains(&"integration_observation") {
        "integration_observation"
    } else {
        "none"
    }
}

fn structural_check(id: &str, detail: &str) -> CompatibilityCheck {
    CompatibilityCheck {
        id: id.to_owned(),
        status: "STRUCTURAL_ONLY",
        proof_class: "structural",
        concrete_input: id.to_owned(),
        boundary: "compatibility manifest and fixture validator".to_owned(),
        expected_result: "structural validation succeeds".to_owned(),
        observed_result: detail.to_owned(),
        skipped_semantic_dependencies: vec![
            "No native Ferric product behavior or external oracle execution was observed."
                .to_owned(),
        ],
        detail: detail.to_owned(),
    }
}

fn pass(id: &str, detail: &str) -> CompatibilityCheck {
    structural_check(id, detail)
}

fn write_compatibility_report(
    root: &Path,
    prefix: &str,
    report: &CompatibilityReport,
) -> Result<PathBuf, String> {
    let path = unique_report_path(root, prefix)?;
    atomic_json(&path, report)?;
    path.strip_prefix(root)
        .map(Path::to_path_buf)
        .map_err(|error| error.to_string())
}

fn unique_report_path(root: &Path, prefix: &str) -> Result<PathBuf, String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    Ok(root
        .join("build/reports")
        .join(format!("{prefix}-{nonce}-{}.json", std::process::id())))
}

fn atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let parent = path.parent().ok_or("output path has no parent")?;
    fs::create_dir_all(parent).map_err(|error| format!("create {}: {error}", parent.display()))?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.flush()?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(format!("atomic JSON write {}: {error}", path.display()));
    }
    Ok(())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|error| format!("read {}: {error}", path.display()))?
        .take(MAX_JSON_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    if bytes.len() as u64 > MAX_JSON_BYTES {
        return Err(format!(
            "FF-COMP-E-JSON-SIZE: {} exceeds {MAX_JSON_BYTES} bytes",
            path.display()
        ));
    }
    serde_json::from_slice(&bytes).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn required_arg<'a>(args: &'a [String], name: &str) -> Result<&'a str, String> {
    optional_arg(args, name).ok_or_else(|| format!("missing required argument {name}"))
}

fn optional_arg<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|window| window[0] == name)
        .map(|window| window[1].as_str())
}

fn require_safe_output(root: &Path, path: &str) -> Result<PathBuf, String> {
    if !safe_relative(path)
        || !(path.starts_with("build/fixtures/compatibility/")
            || path.starts_with(".fforager-artifacts/cargo-target/"))
    {
        return Err(format!("unsafe compatibility output path {path}"));
    }
    let output = root.join(path);
    let mut ancestor = output
        .parent()
        .ok_or_else(|| format!("unsafe compatibility output path {path}"))?;
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| format!("unsafe compatibility output path {path}"))?;
    }
    let canonical_ancestor = fs::canonicalize(ancestor)
        .map_err(|error| format!("canonicalize {}: {error}", ancestor.display()))?;
    if !canonical_compatibility_roots(root)?
        .iter()
        .any(|allowed| canonical_ancestor.starts_with(allowed))
    {
        return Err(format!(
            "unsafe compatibility output path {path}: canonical containment failed"
        ));
    }
    Ok(output)
}

fn require_safe_input(root: &Path, path: &str) -> Result<PathBuf, String> {
    if !safe_relative(path)
        || !(path.starts_with("build/fixtures/compatibility/")
            || path.starts_with(".fforager-artifacts/cargo-target/"))
    {
        return Err(format!("unsafe compatibility input path {path}"));
    }
    let input = fs::canonicalize(root.join(path))
        .map_err(|error| format!("canonicalize compatibility input {path}: {error}"))?;
    if !canonical_compatibility_roots(root)?
        .iter()
        .any(|allowed| input.starts_with(allowed))
    {
        return Err(format!(
            "unsafe compatibility input path {path}: canonical containment failed"
        ));
    }
    Ok(input)
}

fn canonical_compatibility_roots(root: &Path) -> Result<[PathBuf; 2], String> {
    let fixtures = fs::canonicalize(root.join("build/fixtures/compatibility"))
        .map_err(|error| format!("canonicalize compatibility fixtures: {error}"))?;
    let target = fs::canonicalize(root.join(".fforager-artifacts/cargo-target"))
        .map_err(|error| format!("canonicalize build target: {error}"))?;
    Ok([fixtures, target])
}

fn safe_relative(path: &str) -> bool {
    let path = Path::new(path);
    !path.is_absolute()
        && !path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
}

fn stable_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
        && !id.starts_with('-')
        && !id.ends_with('-')
        && !id.contains("--")
}

fn require_unique_stable_ids<'a>(ids: impl Iterator<Item = &'a str>) -> Result<(), String> {
    let mut observed = BTreeSet::new();
    for id in ids {
        if !stable_id(id) {
            return Err(format!("FF-COMP-E-UNSTABLE-ID: {id}"));
        }
        if !observed.insert(id) {
            return Err(format!("FF-COMP-E-DUPLICATE-ID: {id}"));
        }
    }
    Ok(())
}

fn stable_component(value: &str) -> String {
    let mut output = String::new();
    let mut separator = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            if separator && !output.is_empty() {
                output.push('-');
            }
            separator = false;
            output.push(character);
        } else {
            separator = true;
        }
    }
    output.trim_matches('-').to_owned()
}

fn oracle_output(
    root: &Path,
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<String, String> {
    let bytes = command_output_bytes_with_timeout(root, program, args, timeout)?;
    decode_oracle_output(bytes)
        .map_err(|error| format!("{program} {args:?} emitted invalid oracle output: {error}"))
}

fn decode_oracle_output(bytes: Vec<u8>) -> Result<String, String> {
    match String::from_utf8(bytes) {
        Ok(text) => Ok(text),
        Err(error) => {
            let bytes = error.into_bytes();
            let mut output = String::with_capacity(bytes.len());
            for (offset, byte) in bytes.into_iter().enumerate() {
                let character = match byte {
                    0x00..=0x7f | 0xa0..=0xff => char::from_u32(u32::from(byte)),
                    0x80 => Some('\u{20ac}'),
                    0x82 => Some('\u{201a}'),
                    0x83 => Some('\u{0192}'),
                    0x84 => Some('\u{201e}'),
                    0x85 => Some('\u{2026}'),
                    0x86 => Some('\u{2020}'),
                    0x87 => Some('\u{2021}'),
                    0x88 => Some('\u{02c6}'),
                    0x89 => Some('\u{2030}'),
                    0x8a => Some('\u{0160}'),
                    0x8b => Some('\u{2039}'),
                    0x8c => Some('\u{0152}'),
                    0x8e => Some('\u{017d}'),
                    0x91 => Some('\u{2018}'),
                    0x92 => Some('\u{2019}'),
                    0x93 => Some('\u{201c}'),
                    0x94 => Some('\u{201d}'),
                    0x95 => Some('\u{2022}'),
                    0x96 => Some('\u{2013}'),
                    0x97 => Some('\u{2014}'),
                    0x98 => Some('\u{02dc}'),
                    0x99 => Some('\u{2122}'),
                    0x9a => Some('\u{0161}'),
                    0x9b => Some('\u{203a}'),
                    0x9c => Some('\u{0153}'),
                    0x9e => Some('\u{017e}'),
                    0x9f => Some('\u{0178}'),
                    _ => None,
                }
                .ok_or_else(|| format!("undefined Windows-1252 byte 0x{byte:02x} at {offset}"))?;
                output.push(character);
            }
            Ok(output)
        }
    }
}

fn normalized_capture(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n")
}

fn bounded_observation(value: &str) -> String {
    const LIMIT: usize = 240;
    let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
    let mut output = normalized.chars().take(LIMIT).collect::<String>();
    if normalized.chars().count() > LIMIT {
        output.push_str("...[truncated]");
    }
    output.replace('\n', " | ")
}

fn normalized_oracle_descriptions(value: &str) -> String {
    const RANDOM_SEARCH_PHRASES: [&str; 8] = [
        "cute kittens",
        "slithering pythons",
        "falling cat",
        "angry poodle",
        "purple fish",
        "running tortoise",
        "sleeping bunny",
        "burping cow",
    ];
    normalized_capture(value)
        .lines()
        .map(|line| {
            let Some(start) = line.find(" (e.g. \"") else {
                return line.to_owned();
            };
            let Some(relative_end) = line[start + 8..].find("\")") else {
                return line.to_owned();
            };
            let end = start + 8 + relative_end;
            let example = &line[start + 8..end];
            if !RANDOM_SEARCH_PHRASES
                .iter()
                .any(|phrase| example.ends_with(phrase))
            {
                return line.to_owned();
            }
            format!(
                "{}{{{{RANDOMIZED_SEARCH_EXAMPLE}}}}{}",
                &line[..start + 8],
                &line[end..]
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn sha256_text(text: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(text.as_bytes());
    encode_hex(&digest.finalize())
}

fn sha256_normalized_text_file(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let text = String::from_utf8(bytes)
        .map_err(|error| format!("{} is not UTF-8: {error}", path.display()))?;
    Ok(sha256_text(&normalized_capture(&text)))
}

fn canonical_json_sha256<T: Serialize>(value: &T) -> Result<String, String> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    let mut digest = Sha256::new();
    digest.update(bytes);
    Ok(encode_hex(&digest.finalize()))
}

fn require_canonical_json_digest<T: Serialize>(
    value: &T,
    expected: &str,
    diagnostic: &str,
) -> Result<(), String> {
    let observed = canonical_json_sha256(value)?;
    if observed != expected {
        return Err(diagnostic.to_owned());
    }
    Ok(())
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn shard_for(id: &str, total: u32) -> u32 {
    let mut digest = Sha256::new();
    digest.update(id.as_bytes());
    let bytes = digest.finalize();
    let value = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    value % total
}

fn parse_shard(value: &str) -> Result<(u32, u32), String> {
    let (index, total) = value
        .split_once('/')
        .ok_or("shard must use zero-based INDEX/TOTAL syntax")?;
    let index = index.parse::<u32>().map_err(|error| error.to_string())?;
    let total = total.parse::<u32>().map_err(|error| error.to_string())?;
    if total == 0 || index >= total {
        return Err("shard index must be zero-based and less than nonzero total".to_owned());
    }
    Ok((index, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
    }

    fn canonical_inputs() -> (OracleManifest, CompatibilityProfile, CorpusManifest) {
        let root = test_root();
        (
            read_json(&root.join(ORACLE_PATH)).expect("oracle manifest"),
            read_json(&root.join(PROFILE_PATH)).expect("compatibility profile"),
            read_json(&root.join(CORPUS_PATH)).expect("corpus manifest"),
        )
    }

    fn canonical_complete_replay_report(root: &Path) -> CompatibilityReport {
        let (manifest, profile, corpus) = canonical_inputs();
        validate_oracle_manifest(&manifest).expect("canonical oracle manifest");
        validate_profile(&profile, &manifest).expect("canonical compatibility profile");
        let mut checks = Vec::new();
        validate_corpus(root, &corpus, &profile, &mut checks).expect("canonical corpus");
        let semantic_replays = corpus
            .cases
            .iter()
            .map(|case| {
                let fixture: Value =
                    read_json(&root.join(&case.fixture)).expect("canonical replay case fixture");
                replay_case_semantics(case, &fixture).expect("canonical replay semantics")
            })
            .collect::<Vec<_>>();
        checks.push(structural_check(
            "offline-replay",
            "canonical replay test fixture",
        ));
        base_report(
            root,
            "compatibility-replay",
            "SEMANTIC_REPLAY_EXECUTED",
            "complete_corpus",
            BTreeMap::new(),
            checks,
            semantic_replays,
            Vec::new(),
            Vec::new(),
        )
        .expect("canonical replay report")
    }

    #[test]
    fn canonical_artifact_digests_match_constants() {
        let root = test_root();
        let (manifest, profile, corpus) = canonical_inputs();
        let live: LiveManifest = read_json(&root.join(LIVE_PATH)).expect("live manifest");
        let oracle_digest = canonical_json_sha256(&manifest).expect("oracle digest");
        let profile_digest = canonical_json_sha256(&profile).expect("profile digest");
        let corpus_digest = canonical_json_sha256(&corpus).expect("corpus digest");
        let live_digest = canonical_json_sha256(&live).expect("live digest");
        assert_eq!(oracle_digest, ORACLE_MANIFEST_SHA256);
        assert_eq!(profile_digest, PROFILE_SHA256);
        assert_eq!(corpus_digest, CORPUS_MANIFEST_SHA256);
        assert_eq!(live_digest, LIVE_MANIFEST_SHA256);
    }

    #[test]
    fn replay_report_evidence_accepts_canonicalized_windows_path() {
        let root = test_root();
        let report = canonical_complete_replay_report(&root);
        let path = unique_report_path(&root, "compatibility-replay-canonical-path")
            .expect("unique replay report path");
        atomic_json(&path, &report).expect("write canonical replay report");
        let canonical_path = path.canonicalize().expect("canonical replay report path");
        let evidence = read_replay_report_evidence(&root, &canonical_path)
            .expect("canonicalized report path must remain inside the repository");
        fs::remove_file(&path).expect("remove canonical replay report");
        assert_eq!(
            evidence.report_path,
            slash(
                path.strip_prefix(&root)
                    .expect("repository-relative report")
            )
        );
    }

    #[test]
    fn replay_report_evidence_rejects_tampered_current_input_digest() {
        let root = test_root();
        let report = canonical_complete_replay_report(&root);
        let mut artifact = serde_json::to_value(report).expect("serialize replay report");
        artifact["inputs"][ORACLE_PATH] = Value::String("0".repeat(64));
        let path = unique_report_path(&root, "compatibility-replay-tampered")
            .expect("unique replay report path");
        atomic_json(&path, &artifact).expect("write tampered replay report");
        let error = read_replay_report_evidence(&root, &path)
            .expect_err("tampered replay report input digest must be rejected");
        fs::remove_file(&path).expect("remove tampered replay report");
        assert!(
            error.contains("FF-COMP-E-REPLAY-REPORT-DIGEST"),
            "unexpected replay report diagnostic: {error}"
        );
    }

    #[test]
    fn replay_report_evidence_recomputes_canonical_semantic_rows() {
        let root = test_root();
        let report = canonical_complete_replay_report(&root);
        let mut artifact = serde_json::to_value(report).expect("serialize replay report");
        artifact["semantic_replays"][0]["observed_result"] =
            Value::String("self-attested-pass".to_owned());
        let path = unique_report_path(&root, "compatibility-replay-semantic-tampered")
            .expect("unique replay report path");
        atomic_json(&path, &artifact).expect("write tampered replay report");
        let error = read_replay_report_evidence(&root, &path)
            .expect_err("self-attested replay row must be rejected");
        fs::remove_file(&path).expect("remove tampered replay report");
        assert!(
            error.contains("FF-COMP-E-REPLAY-REPORT-SEMANTIC"),
            "unexpected replay report diagnostic: {error}"
        );
    }

    #[test]
    fn replay_report_evidence_rejects_stale_source_provenance() {
        let root = test_root();
        let report = canonical_complete_replay_report(&root);
        let mut artifact = serde_json::to_value(report).expect("serialize replay report");
        artifact["source"]["git_commit"] = Value::String("stale-source".to_owned());
        let path = unique_report_path(&root, "compatibility-replay-stale-source")
            .expect("unique replay report path");
        atomic_json(&path, &artifact).expect("write stale replay report");
        let error = read_replay_report_evidence(&root, &path)
            .expect_err("stale replay source provenance must be rejected");
        fs::remove_file(&path).expect("remove stale replay report");
        assert!(
            error.contains("FF-COMP-E-REPLAY-REPORT-SOURCE-STALE"),
            "unexpected replay report diagnostic: {error}"
        );
    }

    #[test]
    fn native_semantic_replay_executes_each_corpus_plane_and_rejects_label_echoes() {
        let root = test_root();
        let (_, profile, corpus) = canonical_inputs();
        let mut structural = Vec::new();
        validate_corpus(&root, &corpus, &profile, &mut structural).expect("canonical corpus");
        let replays = corpus
            .cases
            .iter()
            .map(|case| {
                let fixture: Value = read_json(&root.join(&case.fixture)).expect("case fixture");
                replay_case_semantics(case, &fixture)
            })
            .collect::<Result<Vec<_>, _>>()
            .expect("every corpus plane has executable native semantics");
        assert_eq!(replays.len(), 7);
        assert!(
            replays
                .iter()
                .all(|replay| replay.status == "SEMANTIC_PASS")
        );
        assert_eq!(
            aggregate_proof_class(&executed_proof_classes(&structural, &replays)),
            "structural"
        );

        for case in &corpus.cases {
            let mut mutated: Value =
                read_json(&root.join(&case.fixture)).expect("case fixture must load");
            match case.plane.as_str() {
                "archive" => mutated["archive_before"] = serde_json::json!([]),
                "failure" => mutated["input"]["deadline_ms"] = Value::from(999_u64),
                "filesystem_process_artifact" => {
                    mutated["process_plan"][0]["program"] = Value::String("invalid".to_owned());
                }
                "migration" => {
                    mutated["source"]["configuration"]["format"] =
                        Value::String("invalid".to_owned());
                }
                "normalized_observation" => {
                    mutated["observation"]["selected_format"] = Value::String("invalid".to_owned());
                }
                "sanitized_network_transcript" => {
                    mutated["clock"] = Value::String("unbounded-clock".to_owned());
                }
                "source_graph" => {
                    mutated["observation"]["nodes"][2]["children"] =
                        serde_json::json!(["media-alpha"]);
                }
                other => panic!("unexpected corpus plane {other}"),
            }
            assert!(
                replay_case_semantics(case, &mutated)
                    .expect_err(
                        "fixture-behavior counterfactual must not be accepted by semantic replay"
                    )
                    .starts_with("FF-COMP-E-SEMANTIC"),
                "semantic counterfactual for plane {} was not rejected",
                case.plane
            );
        }

        let archive_case = corpus
            .cases
            .iter()
            .find(|case| case.plane == "archive")
            .expect("archive case");
        let mut relabelled = archive_case.clone();
        relabelled.expected_outcome = "self-consistent-label".to_owned();
        let fixture: Value = read_json(&root.join(&archive_case.fixture)).expect("archive fixture");
        assert!(
            replay_case_semantics(&relabelled, &fixture)
                .expect_err("expected_outcome text alone cannot close replay")
                .contains("FF-COMP-E-SEMANTIC-OUTCOME")
        );
    }

    #[test]
    fn normalized_fixture_digest_is_line_ending_portable() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = test_root()
            .join(".fforager-artifacts/cargo-target")
            .join(format!("compatibility-crlf-{nonce}.json"));
        fs::write(&path, b"{\r\n  \"value\": true\r\n}\r\n").expect("write CRLF fixture");
        let observed = sha256_normalized_text_file(&path).expect("normalized digest");
        fs::remove_file(&path).expect("remove CRLF fixture");
        assert_eq!(observed, sha256_text("{\n  \"value\": true\n}\n"));
    }

    #[test]
    fn bounded_json_reader_rejects_oversized_input() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = test_root()
            .join(".fforager-artifacts/cargo-target")
            .join(format!("compatibility-oversized-{nonce}.json"));
        let file = fs::File::create(&path).expect("create sparse input");
        file.set_len(MAX_JSON_BYTES + 1).expect("size sparse input");
        let result = read_json::<Value>(&path);
        fs::remove_file(&path).expect("remove sparse input");
        let error = result.expect_err("oversized JSON must fail before parsing");
        assert!(error.starts_with("FF-COMP-E-JSON-SIZE"));
    }

    #[test]
    fn stable_component_is_portable() {
        assert_eq!(
            stable_component("YouTube:Tab (CURRENTLY BROKEN)"),
            "youtube-tab-currently-broken"
        );
    }

    #[test]
    fn help_parser_extracts_alias_and_value() {
        let help = "  General Options:\n    -h, --help                      Print help\n    -o, --output TEMPLATE           Output template\n\n  Preset Aliases:\n    -t mp3                          -f bestaudio -x\n";
        let (options, presets) = parse_help(help).expect("fixture help must parse");
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].aliases, ["-h", "--help"]);
        assert!(options[1].takes_value);
        assert_eq!(presets[0].name, "mp3");
    }

    #[test]
    fn help_parser_keeps_deep_option_reference_as_description() {
        let help = "  General Options:\n    --remote-components COMPONENT   Fetch components\n                                    --remote-components or defaults\n    --no-remote-components          Disable fetching\n";
        let (options, _) = parse_help(help).expect("fixture help must parse");
        assert_eq!(options.len(), 2);
        assert!(options[0].description.contains("or defaults"));
    }

    #[test]
    fn secret_scanner_rejects_bearer_value() {
        let error = validate_sanitized_value(&serde_json::json!({
            "Authorization": "Bearer private"
        }))
        .expect_err("secret must fail");
        assert!(error.starts_with("FF-COMP-E-UNSANITIZED-SECRET"));
    }

    #[test]
    fn secret_scanner_accepts_placeholder_query_value() {
        validate_sanitized_value(&serde_json::json!(
            "https://media.invalid/item?token={{QUERY_TOKEN}}"
        ))
        .expect("placeholder must be accepted");
    }

    #[test]
    fn secret_scanner_rejects_placeholder_with_secret_suffix() {
        let error = validate_sanitized_value(&serde_json::json!(
            "https://media.invalid/item?token={{QUERY_TOKEN}}actual-secret"
        ))
        .expect_err("placeholder suffix must fail");
        assert!(error.starts_with("FF-COMP-E-UNSANITIZED-SECRET"));
    }

    #[test]
    fn shard_assignment_is_stable_and_bounded() {
        let first = shard_for("case-source-graph-direct-v1", 4);
        assert_eq!(first, shard_for("case-source-graph-direct-v1", 4));
        assert!(first < 4);
    }

    #[test]
    fn oracle_decoder_preserves_windows_1252() {
        assert_eq!(
            decode_oracle_output(b"espa\xf1ol \x80".to_vec()).expect("decode"),
            "español €"
        );
    }

    #[test]
    fn description_normalizer_removes_oracle_randomness() {
        let first = normalized_oracle_descriptions(
            "search: prefix (e.g. \"ytsearch5:cute kittens\")\r\nfixed",
        );
        let second = normalized_oracle_descriptions(
            "search: prefix (e.g. \"ytsearchall:purple fish\")\nfixed",
        );
        assert_eq!(first, second);
        assert!(first.contains("{{RANDOMIZED_SEARCH_EXAMPLE}}"));
    }

    #[test]
    fn profile_rejects_self_consistent_inventory_removal() {
        let (manifest, mut profile, _) = canonical_inputs();
        profile.options.pop().expect("profile has options");
        profile.counts.options -= 1;
        let error = validate_profile(&profile, &manifest)
            .expect_err("removing a pinned row and its count must fail closed");
        assert!(error.starts_with("FF-COMP-E-PROFILE-INTEGRITY"));
    }

    #[test]
    fn corpus_rejects_self_consistent_expected_outcome_change() {
        let (_, _, mut corpus) = canonical_inputs();
        corpus.cases[0].expected_outcome = "silently-redefined".to_owned();
        let error = validate_corpus_without_files(&corpus)
            .expect_err("changing a pinned expected outcome must fail closed");
        assert!(error.starts_with("FF-COMP-E-CORPUS-INTEGRITY"));
    }

    #[test]
    fn differential_rejects_invalid_canonical_profile() {
        let (_, mut profile, corpus) = canonical_inputs();
        profile.options.pop().expect("profile has options");
        profile.counts.options -= 1;
        let candidate = CandidateResults {
            schema_id: "ff.compatibility-candidate-results@1".to_owned(),
            schema_version: "1.0.0".to_owned(),
            corpus_id: corpus.corpus_id.clone(),
            profile_id: profile.profile_id.clone(),
            observations: Vec::new(),
        };
        differential_rows(&corpus, &profile, &candidate)
            .expect_err("diff must not PASS when its canonical profile is invalid");
    }

    #[test]
    fn differential_rejects_unregistered_divergence_decision() {
        let (_, profile, corpus) = canonical_inputs();
        let mut candidate = candidate_with_difference(&corpus, Some("accepted_divergence"), None);
        candidate.observations[0].decision_id = Some("invented-decision".to_owned());
        differential_rows(&corpus, &profile, &candidate)
            .expect_err("syntactically stable but unauthorized decisions must fail closed");
    }

    #[test]
    fn differential_rejects_nondeterministic_classification_for_offline_case() {
        let (_, profile, corpus) = canonical_inputs();
        let candidate = candidate_with_difference(&corpus, Some("nondeterministic_response"), None);
        differential_rows(&corpus, &profile, &candidate)
            .expect_err("deterministic offline cases cannot be reclassified as nondeterministic");
    }

    #[test]
    fn inventory_diff_does_not_pass_invalid_profile() {
        let (_, profile, _) = canonical_inputs();
        let mut invalid = profile.clone();
        invalid.options.pop().expect("profile has options");
        invalid.counts.options -= 1;
        inventory_diff(
            &profile,
            &invalid,
            CompatibilitySource {
                git_commit: "test".to_owned(),
                dirty: false,
                dirty_paths: Vec::new(),
                content_fingerprint: "0".repeat(64),
            },
            BTreeMap::new(),
        )
        .expect_err("same profile ID with changed content must fail closed");
    }

    #[test]
    fn inventory_diff_covers_every_versioned_row_family() {
        let (_, profile, _) = canonical_inputs();
        let mut changed = profile.clone();
        changed.profile_id = "yt-dlp-2026.07.04-profile-v2-test".to_owned();
        changed.presets[0].expansion.push_str(" --test-change");
        changed.interactions[0]
            .description
            .push_str(" Test change.");
        changed.extractor_descriptions[0]
            .description
            .push_str(" test change");
        let report = inventory_diff(
            &profile,
            &changed,
            CompatibilitySource {
                git_commit: "test".to_owned(),
                dirty: false,
                dirty_paths: Vec::new(),
                content_fingerprint: "0".repeat(64),
            },
            BTreeMap::new(),
        )
        .expect("different version IDs may be structurally compared");
        assert_eq!(report.schema_id, "ff.compatibility-inventory-diff@2");
        assert_eq!(report.changed_presets, [profile.presets[0].id.clone()]);
        assert_eq!(
            report.changed_interactions,
            [profile.interactions[0].id.clone()]
        );
        assert_eq!(
            report.changed_extractor_descriptions,
            [profile.extractor_descriptions[0].id.clone()]
        );
    }

    #[test]
    fn negative_inventory_rejects_a_missing_required_mutation() {
        let (manifest, profile, corpus) = canonical_inputs();
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = test_root()
            .join(".fforager-artifacts/cargo-target")
            .join(format!("compatibility-negative-inventory-test-{nonce}"));
        let target = root.join(NEGATIVE_ROOT);
        fs::create_dir_all(&target).expect("negative target");
        let source = test_root().join(NEGATIVE_ROOT);
        for entry in fs::read_dir(source).expect("negative source") {
            let entry = entry.expect("negative entry");
            let name = entry.file_name();
            if name == "unstable-id" {
                continue;
            }
            let directory = target.join(name);
            fs::create_dir_all(&directory).expect("fixture directory");
            fs::copy(entry.path().join("case.json"), directory.join("case.json"))
                .expect("copy fixture");
        }
        let result = validate_negative_fixtures(&root, &manifest, &profile, &corpus);
        fs::remove_dir_all(&root).expect("remove fixture root");
        result.expect_err("removing one required negative mutation must fail closed");
    }

    #[test]
    fn live_report_is_observed_not_pass() {
        let report = base_report(
            &test_root(),
            "compatibility-live-canaries",
            "OBSERVED",
            "selected_live_canaries_only",
            BTreeMap::new(),
            vec![CompatibilityCheck {
                id: "live-canary-test".to_owned(),
                status: "OBSERVED",
                proof_class: "integration_observation",
                concrete_input: "https://media.invalid/".to_owned(),
                boundary: "test observation boundary".to_owned(),
                expected_result: "nondeterministic_observation".to_owned(),
                observed_result: "nondeterministic observation".to_owned(),
                skipped_semantic_dependencies: vec!["test-only observation".to_owned()],
                detail: "nondeterministic observation".to_owned(),
            }],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .expect("report construction");
        assert_eq!(report.status, "OBSERVED");
    }

    #[test]
    fn secret_scanner_rejects_sensitive_key_variants_and_unknown_placeholders() {
        for value in [
            serde_json::json!({"X-Api-Key": "actual-secret"}),
            serde_json::json!({"access_token": "actual-secret"}),
            serde_json::json!({"Authorization": "{{NOT_ALLOWLISTED}}"}),
        ] {
            validate_sanitized_value(&value)
                .expect_err("sensitive aliases and unknown placeholders must fail closed");
        }
    }

    #[test]
    fn secret_scanner_rejects_portable_machine_local_paths() {
        for path in [
            "C:/Users/Alice/private.txt",
            "/Users/alice/private.txt",
            "\\\\server\\private\\fixture.json",
        ] {
            validate_sanitized_value(&Value::String(path.to_owned()))
                .expect_err("machine-local path must fail closed");
        }
    }

    #[test]
    fn live_manifest_rejects_private_destination() {
        let root = test_root();
        let profile: CompatibilityProfile = read_json(&root.join(PROFILE_PATH)).expect("profile");
        let mut live: LiveManifest = read_json(&root.join(LIVE_PATH)).expect("live manifest");
        live.canaries[0].url = "https://127.0.0.1/internal".to_owned();
        validate_live_manifest(&live, &profile)
            .expect_err("live canary destinations must be explicitly allowlisted");
    }
}
