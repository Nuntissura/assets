//! Shared, non-shipped conformance helpers for versioned Ferric Forager contracts.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::thread;
#[cfg(windows)]
use std::{
    io::Read,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use fforager_contracts::{
    ArchiveCommitted, ArchiveUniquenessEvidence, ArtifactIdentity, AssetId, BYTE_CREDIT_STAGES_V1,
    ByteCreditComponent, ByteCreditContractV1, ByteCreditPosition, ByteCreditStage,
    CleanupObservation, CollisionDecision, CollisionObservation, CommitPrepared, CommitRenamed,
    CommitState, CoupledByteReservation, DerivedOutputId, DurabilityPosition,
    FilesystemProfileContract, IdentityObservation, JobId, JournalPayload, JournalPrefixFault,
    JournalRecordError, LeaseObservation, MigrationObservation, ObservedJournalRecord,
    RecoveryAction, RecoveryConfinementObservation, RecoveryDecision, RecoveryFailure,
    RecoveryObservation, ResourceContractV1, ResourceVector, TransactionId, VolumeRelationship,
    scan_observed_journal,
};
use fforager_core::lifecycle::{
    ByteDurabilityEffectError, EffectIntent, Event, MachineInstanceId, MachineKind,
    RecoveryApplicationError, State, StateMachine, TransitionError, apply_recovery_action,
    decide_recovery,
};
use fforager_core::resource::{
    CreditError, CreditLimitScope, LedgerError, OwnedAdmission, OwnedByteCreditBroker,
    OwnedResourceBroker, OwnerId,
};
use serde_json::{Value, json};

mod archive_evidence;

pub use archive_evidence::{
    run_archive_store_evidence_corpus, validate_archive_store_evidence_report,
};

/// Canonical WP-008 candidate-neutral archive evidence corpus.
pub const ARCHIVE_STORE_EVIDENCE_MANIFEST: &str =
    "build/fixtures/archive-store-evidence/manifest.json";

/// Exact compiled public test invoked by the WP-008 evidence gate.
pub const ARCHIVE_STORE_EVIDENCE_PROOF_TEST: &str =
    "tests::archive_store_evidence_corpus_executes_public_boundary";

/// Stable stdout prefix carrying the strict machine-readable WP-008 report.
pub const ARCHIVE_STORE_EVIDENCE_REPORT_PREFIX: &str = "FF-WP008-ARCHIVE-REPORT:";

const ARCHIVE_STORE_EVIDENCE_SOURCE_PATHS: &[&str] = &[
    "product/crates/fforager-contracts/src/archive.rs",
    "product/crates/fforager-storage/src/lib.rs",
    "build/crates/fforager-testkit/src/archive_evidence.rs",
    "build/crates/fforager-testkit/src/lib.rs",
    "build/tools/fforager-xtask/src/main.rs",
];

const ARCHIVE_STORE_EVIDENCE_CASE_IDS: &[&str] = &[
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

static ARCHIVE_EVIDENCE_RUN_SEQUENCE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Maximum size of one canonical conformance fixture.
pub const MAX_FIXTURE_BYTES: u64 = 1_048_576;

/// Canonical WP-009 deterministic model corpus.
pub const RESOURCE_DURABILITY_MANIFEST: &str =
    "build/fixtures/resource-durability-models/manifest.json";

/// Exact compiled public test invoked by the WP-009 deep-gate consumer.
pub const RESOURCE_DURABILITY_PROOF_TEST: &str =
    "tests::resource_durability_model_corpus_executes_public_boundaries";

/// Stable stdout prefix carrying the strict machine-readable WP-009 report.
pub const RESOURCE_DURABILITY_REPORT_PREFIX: &str = "FF-WP009-MODEL-REPORT:";

const RESOURCE_DURABILITY_SOURCE_PATHS: &[&str] = &[
    "product/crates/fforager-contracts/src/resource.rs",
    "product/crates/fforager-contracts/src/storage.rs",
    "product/crates/fforager-core/src/resource.rs",
    "product/crates/fforager-core/src/lifecycle.rs",
    "build/crates/fforager-testkit/src/lib.rs",
    "build/tools/fforager-xtask/src/main.rs",
];

const RESOURCE_DURABILITY_CASE_IDS: &[&str] = &[
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

const WP009_WINDOWS_PLATFORM_ROW_RESIDUAL: &str = "Live Windows host and repository-volume identity was probed; atomic-replace behavior, confinement races, crash behavior, power-loss behavior, and durability behavior were not executed.";
const WP009_WSL_PLATFORM_ROW_RESIDUAL: &str = "Live WSL2 host and repository-mount identity was probed; native-Linux behavior, atomic-replace behavior, confinement races, crash behavior, power-loss behavior, and durability behavior were not executed.";

/// Fail-closed fixture loading errors.
#[derive(Debug)]
pub enum FixtureError {
    EscapesRoot,
    Io(std::io::Error),
    Oversized { actual: u64, maximum: u64 },
}

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "fixture error: {self:?}")
    }
}

impl std::error::Error for FixtureError {}

/// Returns the repository-local canonical contract fixture root.
#[must_use]
pub fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/contracts")
}

/// Loads one bounded fixture without allowing absolute paths or parent traversal.
///
/// # Errors
///
/// Returns [`FixtureError`] for an unsafe path, I/O failure, or oversized fixture.
pub fn read_fixture(relative: &str) -> Result<Vec<u8>, FixtureError> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(FixtureError::EscapesRoot);
    }
    let bytes = fs::read(fixture_root().join(path)).map_err(FixtureError::Io)?;
    let actual = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if actual > MAX_FIXTURE_BYTES {
        return Err(FixtureError::Oversized {
            actual,
            maximum: MAX_FIXTURE_BYTES,
        });
    }
    Ok(bytes)
}

/// Produces the canonical four-byte big-endian framing used by process protocols.
///
/// # Errors
///
/// Returns [`FixtureError::Oversized`] when the payload cannot be represented by the frame header.
pub fn frame(payload: &[u8]) -> Result<Vec<u8>, FixtureError> {
    let length = u32::try_from(payload.len()).map_err(|_| FixtureError::Oversized {
        actual: u64::try_from(payload.len()).unwrap_or(u64::MAX),
        maximum: u64::from(u32::MAX),
    })?;
    let mut framed = Vec::with_capacity(4 + payload.len());
    framed.extend_from_slice(&length.to_be_bytes());
    framed.extend_from_slice(payload);
    Ok(framed)
}

/// Fail-closed WP-009 model-corpus error.
#[derive(Debug)]
pub enum ModelProofError {
    Fixture(FixtureError),
    Io(std::io::Error),
    Json(serde_json::Error),
    Contract(String),
}

impl fmt::Display for ModelProofError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "WP-009 model proof error: {self:?}")
    }
}

impl std::error::Error for ModelProofError {}

impl From<FixtureError> for ModelProofError {
    fn from(error: FixtureError) -> Self {
        Self::Fixture(error)
    }
}

impl From<std::io::Error> for ModelProofError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for ModelProofError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// Fail-closed WP-008 archive evidence error.
#[derive(Debug)]
pub enum ArchiveEvidenceError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Contract(String),
}

impl fmt::Display for ArchiveEvidenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "WP-008 archive evidence error: {self:?}")
    }
}

impl std::error::Error for ArchiveEvidenceError {}

impl From<std::io::Error> for ArchiveEvidenceError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for ArchiveEvidenceError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// Returns the repository root derived from the testkit manifest location.
#[must_use]
pub fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

#[derive(Clone, Debug)]
struct Wp009PlatformEvidence {
    windows: Value,
    wsl: Value,
}

fn platform_not_applicable() -> Value {
    json!({
        "schema_id": "ff.wp009-platform-observation@1",
        "mode": "not_applicable",
        "commands": [],
        "observed_fields": {},
        "verdict": "not_applicable",
        "residual_uncertainty": []
    })
}

static WP009_PLATFORM_EVIDENCE: OnceLock<Result<Wp009PlatformEvidence, String>> = OnceLock::new();

fn observe_wp009_platform(root: &Path) -> Result<Wp009PlatformEvidence, ModelProofError> {
    WP009_PLATFORM_EVIDENCE
        .get_or_init(|| {
            observe_wp009_platform_uncached(root).map_err(|error| match error {
                ModelProofError::Contract(diagnostic) => diagnostic,
                other => format!("WP009-E-PLATFORM-PROBE:{other}"),
            })
        })
        .clone()
        .map_err(ModelProofError::Contract)
}

#[cfg(not(windows))]
fn observe_wp009_platform_uncached(_root: &Path) -> Result<Wp009PlatformEvidence, ModelProofError> {
    Err(ModelProofError::Contract(
        "WP009-E-PLATFORM-UNSUPPORTED-HOST: Windows 11 plus WSL2 evidence is required".to_owned(),
    ))
}

#[cfg(windows)]
#[allow(clippy::too_many_lines)]
fn observe_wp009_platform_uncached(root: &Path) -> Result<Wp009PlatformEvidence, ModelProofError> {
    let registry_key = r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let product_args = vec![
        "query".to_owned(),
        registry_key.to_owned(),
        "/v".to_owned(),
        "ProductName".to_owned(),
    ];
    let edition_args = vec![
        "query".to_owned(),
        registry_key.to_owned(),
        "/v".to_owned(),
        "EditionID".to_owned(),
    ];
    let build_args = vec![
        "query".to_owned(),
        registry_key.to_owned(),
        "/v".to_owned(),
        "CurrentBuildNumber".to_owned(),
    ];
    let raw_product_name = parse_registry_value(&run_wp009_probe("reg.exe", &product_args)?)?;
    let edition_id = parse_registry_value(&run_wp009_probe("reg.exe", &edition_args)?)?;
    let build_number = parse_registry_value(&run_wp009_probe("reg.exe", &build_args)?)?;

    let canonical_root = fs::canonicalize(root)
        .map_err(|error| ModelProofError::Contract(format!("WP009-E-PLATFORM-ROOT:{error}")))?;
    let canonical_root_text = canonical_root.to_string_lossy().into_owned();
    let windows_root = canonical_root_text
        .strip_prefix(r"\\?\")
        .unwrap_or(&canonical_root_text)
        .to_owned();
    let drive = windows_root
        .get(0..2)
        .filter(|prefix| prefix.as_bytes().get(1) == Some(&b':'))
        .ok_or_else(|| {
            ModelProofError::Contract(
                "WP009-E-PLATFORM-ROOT: repository has no drive-letter volume".to_owned(),
            )
        })?;
    let filesystem_args = vec![
        "logicaldisk".to_owned(),
        "where".to_owned(),
        format!("DeviceID='{drive}'"),
        "get".to_owned(),
        "FileSystem".to_owned(),
        "/value".to_owned(),
    ];
    let repository_filesystem = parse_key_value(
        &run_wp009_probe("wmic.exe", &filesystem_args)?,
        "FileSystem",
        "WP009-E-WINDOWS-FILESYSTEM",
    )?;
    let parsed_build = build_number
        .parse::<u32>()
        .map_err(|error| ModelProofError::Contract(format!("WP009-E-WINDOWS-BUILD:{error}")))?;
    let derived_product = match (parsed_build >= 22_000, edition_id.as_str()) {
        (true, "Core") => "Windows 11 Home",
        _ => {
            return Err(ModelProofError::Contract(format!(
                "WP009-E-WINDOWS-PROFILE-MISMATCH: build={build_number}; edition={edition_id}"
            )));
        }
    };
    if build_number != "26200" || !repository_filesystem.eq_ignore_ascii_case("NTFS") {
        return Err(ModelProofError::Contract(format!(
            "WP009-E-WINDOWS-PROFILE-MISMATCH: build={build_number}; filesystem={repository_filesystem}"
        )));
    }
    let windows_commands = vec![
        command_record("reg.exe", &product_args),
        command_record("reg.exe", &edition_args),
        command_record("reg.exe", &build_args),
        command_record("wmic.exe", &filesystem_args),
    ];
    let windows = json!({
        "schema_id": "ff.wp009-platform-observation@1",
        "mode": "live_windows_host_and_repository_volume",
        "commands": windows_commands,
        "observed_fields": {
            "registry_product_name": raw_product_name,
            "edition_id": edition_id,
            "derived_product": derived_product,
            "build_number": build_number,
            "repository_volume": drive,
            "repository_filesystem": repository_filesystem
        },
        "verdict": "matched_frozen_windows_11_26200_ntfs",
        "residual_uncertainty": [
            "The legacy ProductName registry value may say Windows 10 on Windows 11; the observed build and EditionID derive the product identity.",
            "This observes host and volume identity only; it is not a crash, power-loss, atomic-replace, confinement-race, or durability experiment."
        ]
    });

    let os_release_args = vec![
        "--exec".to_owned(),
        "cat".to_owned(),
        "/etc/os-release".to_owned(),
    ];
    let kernel_args = vec!["--exec".to_owned(), "uname".to_owned(), "-r".to_owned()];
    let wslpath_args = vec![
        "--exec".to_owned(),
        "wslpath".to_owned(),
        "-a".to_owned(),
        windows_root.clone(),
    ];
    let os_release = run_wp009_probe("wsl.exe", &os_release_args)?;
    let distribution_id = parse_os_release(&os_release, "ID")?;
    let distribution_version = parse_os_release(&os_release, "VERSION_ID")?;
    let distribution_pretty_name = parse_os_release(&os_release, "PRETTY_NAME")?;
    let kernel_release = run_wp009_probe("wsl.exe", &kernel_args)?.trim().to_owned();
    let wsl_repository_path = run_wp009_probe("wsl.exe", &wslpath_args)?.trim().to_owned();
    if wsl_repository_path.is_empty() || !wsl_repository_path.starts_with("/mnt/") {
        return Err(ModelProofError::Contract(format!(
            "WP009-E-WSL-REPOSITORY-PATH: {wsl_repository_path}"
        )));
    }
    let stat_args = vec![
        "--exec".to_owned(),
        "stat".to_owned(),
        "-f".to_owned(),
        "-c".to_owned(),
        "%T".to_owned(),
        wsl_repository_path.clone(),
    ];
    let repository_mount_filesystem = run_wp009_probe("wsl.exe", &stat_args)?.trim().to_owned();
    if distribution_id != "ubuntu"
        || distribution_version != "24.04"
        || !kernel_release.starts_with("6.6.87.2-")
        || repository_mount_filesystem != "v9fs"
    {
        return Err(ModelProofError::Contract(format!(
            "WP009-E-WSL-PROFILE-MISMATCH: id={distribution_id}; version={distribution_version}; kernel={kernel_release}; filesystem={repository_mount_filesystem}"
        )));
    }
    let wsl_commands = vec![
        command_record("wsl.exe", &os_release_args),
        command_record("wsl.exe", &kernel_args),
        command_record("wsl.exe", &wslpath_args),
        command_record("wsl.exe", &stat_args),
    ];
    let wsl = json!({
        "schema_id": "ff.wp009-platform-observation@1",
        "mode": "live_wsl_read_only_identity_and_mount",
        "commands": wsl_commands,
        "observed_fields": {
            "distribution_id": distribution_id,
            "distribution_version": distribution_version,
            "distribution_pretty_name": distribution_pretty_name,
            "kernel_release": kernel_release,
            "repository_path": wsl_repository_path,
            "repository_mount_filesystem": repository_mount_filesystem
        },
        "verdict": "matched_frozen_wsl2_v9fs_rejected_degraded",
        "residual_uncertainty": [
            "Only read-only WSL identity, kernel, path-translation, and filesystem-stat commands executed.",
            "No Rust code or Ferric model ran inside WSL, and this is not native-Linux, crash, power-loss, confinement-race, or durability proof."
        ]
    });
    Ok(Wp009PlatformEvidence { windows, wsl })
}

fn command_record(program: &str, args: &[String]) -> Value {
    json!({"program": program, "args": args})
}

#[cfg(windows)]
fn run_wp009_probe(program: &str, args: &[String]) -> Result<String, ModelProofError> {
    const TIMEOUT: Duration = Duration::from_mins(2);
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_quiet_probe(&mut command);
    let mut child = command.spawn().map_err(|error| {
        ModelProofError::Contract(format!("WP009-E-PLATFORM-PROBE-SPAWN:{program}:{error}"))
    })?;
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return finish_wp009_probe(program, args, &mut child, status.success());
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _kill_result = child.kill();
                let _wait_result = child.wait();
                return Err(ModelProofError::Contract(format!(
                    "WP009-E-PLATFORM-PROBE-TIMEOUT:{program}:{}",
                    args.join(" ")
                )));
            }
            Err(error) => {
                let _kill_result = child.kill();
                let _wait_result = child.wait();
                return Err(ModelProofError::Contract(format!(
                    "WP009-E-PLATFORM-PROBE-WAIT:{program}:{error}"
                )));
            }
        }
    }
}

#[cfg(windows)]
fn finish_wp009_probe(
    program: &str,
    args: &[String],
    child: &mut Child,
    success: bool,
) -> Result<String, ModelProofError> {
    const MAX_OUTPUT_BYTES: usize = 65_536;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        pipe.read_to_end(&mut stdout)?;
    }
    if let Some(mut pipe) = child.stderr.take() {
        pipe.read_to_end(&mut stderr)?;
    }
    if stdout.len() > MAX_OUTPUT_BYTES || stderr.len() > MAX_OUTPUT_BYTES {
        return Err(ModelProofError::Contract(format!(
            "WP009-E-PLATFORM-PROBE-OVERSIZED:{program}"
        )));
    }
    let stdout = decode_probe_text(&stdout);
    let stderr = decode_probe_text(&stderr);
    if !success {
        return Err(ModelProofError::Contract(format!(
            "WP009-E-PLATFORM-PROBE-FAILED:{program}:{}:{stderr}",
            args.join(" ")
        )));
    }
    Ok(stdout)
}

#[cfg(windows)]
fn decode_probe_text(bytes: &[u8]) -> String {
    let utf16_likely = bytes.len() >= 2
        && bytes.len().is_multiple_of(2)
        && bytes.chunks_exact(2).filter(|pair| pair[1] == 0).count() * 2 >= bytes.len() / 2;
    if utf16_likely {
        let words = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        String::from_utf16_lossy(&words)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

#[cfg(windows)]
fn parse_registry_value(output: &str) -> Result<String, ModelProofError> {
    for line in output.lines() {
        if let Some((_, value)) = line.split_once("REG_SZ") {
            let value = value.trim();
            if !value.is_empty() {
                return Ok(value.to_owned());
            }
        }
    }
    Err(ModelProofError::Contract(
        "WP009-E-WINDOWS-REGISTRY-PARSE".to_owned(),
    ))
}

#[cfg(windows)]
fn parse_key_value(output: &str, key: &str, diagnostic: &str) -> Result<String, ModelProofError> {
    output
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| ModelProofError::Contract(diagnostic.to_owned()))
}

#[cfg(windows)]
fn parse_os_release(output: &str, key: &str) -> Result<String, ModelProofError> {
    parse_key_value(output, key, "WP009-E-WSL-OS-RELEASE").map(|value| {
        value
            .strip_prefix('"')
            .and_then(|text| text.strip_suffix('"'))
            .unwrap_or(&value)
            .to_owned()
    })
}

#[cfg(windows)]
fn configure_quiet_probe(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

/// Loads and executes the bounded WP-009 corpus against public contract/core APIs.
///
/// The returned JSON is strict supporting evidence. It does not execute a
/// shipped Ferric entrypoint and cannot advance product or runtime status.
///
/// # Errors
///
/// Returns [`ModelProofError`] for malformed/unmapped inputs, missing bound
/// fields, failed invariants, source-manifest drift, or public-boundary errors.
pub fn run_resource_durability_model_corpus() -> Result<Value, ModelProofError> {
    let root = repository_root();
    let manifest_path = root.join(RESOURCE_DURABILITY_MANIFEST);
    let manifest_bytes = fs::read(&manifest_path)?;
    if manifest_bytes.len() > usize::try_from(MAX_FIXTURE_BYTES).unwrap_or(usize::MAX) {
        return Err(ModelProofError::Contract(
            "WP009-E-MANIFEST-OVERSIZED".to_owned(),
        ));
    }
    let manifest: Value = serde_json::from_slice(&manifest_bytes)?;
    validate_resource_durability_manifest(&manifest)?;

    let source_manifest = resource_durability_source_manifest(&root)?;
    let cases = manifest
        .get("cases")
        .and_then(Value::as_array)
        .ok_or_else(|| ModelProofError::Contract("WP009-E-CASES-MISSING".to_owned()))?;
    let platform_evidence = observe_wp009_platform(&root)?;
    let rows = cases
        .iter()
        .map(|case| {
            let mut row = execute_resource_durability_case(case)?;
            let case_id = required_text(case, "case_id", "WP009-E-CASE-ID")?;
            row["platform_observation"] = match case_id {
                "wp009-filesystem-windows-ntfs" => platform_evidence.windows.clone(),
                "wp009-filesystem-wsl-v9fs" => platform_evidence.wsl.clone(),
                _ => platform_not_applicable(),
            };
            Ok::<Value, ModelProofError>(row)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let proof_classes = rows
        .iter()
        .filter_map(|row| row.get("proof_class").and_then(Value::as_str))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let proof_mechanisms = rows
        .iter()
        .filter_map(|row| row.get("proof_mechanism").and_then(Value::as_str))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let residual_uncertainty = manifest
        .get("residual_uncertainty")
        .cloned()
        .ok_or_else(|| ModelProofError::Contract("WP009-E-RESIDUAL-MISSING".to_owned()))?;
    let report = json!({
        "schema_id": "ff.resource-durability-model-report@1",
        "schema_version": "1.1.0",
        "corpus_id": "WP-FF-009-resource-durability-models-v1",
        "manifest_path": RESOURCE_DURABILITY_MANIFEST,
        "manifest_fingerprint": format!("fnv1a64:{:016x}", fnv1a64(&manifest_bytes)),
        "source_manifest": source_manifest,
        "exploration_bounds": manifest["exploration_bounds"].clone(),
        "rows": rows,
        "summary": {
            "executed_cases": cases.len(),
            "failed_cases": 0,
            "proof_classes": proof_classes,
            "proof_mechanisms": proof_mechanisms,
            "zero_product_progress": true
        },
        "residual_uncertainty": residual_uncertainty
    });
    validate_resource_durability_report(&report, &manifest)?;
    Ok(report)
}

fn validate_resource_durability_manifest(manifest: &Value) -> Result<(), ModelProofError> {
    require_exact_keys(
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
        return Err(ModelProofError::Contract(
            "WP009-E-MANIFEST-IDENTITY".to_owned(),
        ));
    }
    let source_paths = string_values(manifest, "source_paths")?;
    if source_paths != RESOURCE_DURABILITY_SOURCE_PATHS {
        return Err(ModelProofError::Contract(
            "WP009-E-SOURCE-MANIFEST-DRIFT".to_owned(),
        ));
    }
    let cases = manifest
        .get("cases")
        .and_then(Value::as_array)
        .ok_or_else(|| ModelProofError::Contract("WP009-E-CASES-MISSING".to_owned()))?;
    let mut observed = BTreeSet::new();
    for case in cases {
        require_exact_keys(
            case,
            &["case_id", "model", "scenario", "injected_fault"],
            "WP009-E-CASE-SCHEMA",
        )?;
        let id = required_text(case, "case_id", "WP009-E-CASE-ID")?;
        for key in ["model", "scenario", "injected_fault"] {
            let _value = required_text(case, key, "WP009-E-CASE-FIELD")?;
        }
        if !observed.insert(id) {
            return Err(ModelProofError::Contract(
                "WP009-E-DUPLICATE-CASE".to_owned(),
            ));
        }
    }
    let required = RESOURCE_DURABILITY_CASE_IDS
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    if observed != required {
        return Err(ModelProofError::Contract(
            "WP009-E-UNMAPPED-INPUT".to_owned(),
        ));
    }
    let bounds = manifest
        .get("exploration_bounds")
        .and_then(Value::as_object)
        .ok_or_else(|| ModelProofError::Contract("WP009-E-BOUNDS-MISSING".to_owned()))?;
    let required_bounds = BTreeSet::from([
        "maximum_active_grants",
        "maximum_cases",
        "maximum_credit_bytes",
        "maximum_credit_claim_items",
        "maximum_transitions_per_case",
        "maximum_waiter_declared_variable_bytes",
        "maximum_waiter_items",
    ]);
    if bounds.keys().map(String::as_str).collect::<BTreeSet<_>>() != required_bounds
        || bounds
            .values()
            .any(|value| value.as_u64().is_none_or(|number| number == 0))
        || bounds.get("maximum_cases").and_then(Value::as_u64) != u64::try_from(cases.len()).ok()
    {
        return Err(ModelProofError::Contract(
            "WP009-E-BOUNDS-INCOMPLETE".to_owned(),
        ));
    }
    let residual = string_values(manifest, "residual_uncertainty")?;
    if residual.len() < 3 {
        return Err(ModelProofError::Contract(
            "WP009-E-RESIDUAL-INCOMPLETE".to_owned(),
        ));
    }
    Ok(())
}

fn resource_durability_source_manifest(root: &Path) -> Result<Vec<Value>, ModelProofError> {
    RESOURCE_DURABILITY_SOURCE_PATHS
        .iter()
        .map(|relative| {
            let path = Path::new(relative);
            if path.is_absolute()
                || path
                    .components()
                    .any(|component| matches!(component, std::path::Component::ParentDir))
            {
                return Err(ModelProofError::Contract("WP009-E-SOURCE-PATH".to_owned()));
            }
            let bytes = fs::read(root.join(path))?;
            Ok(json!({
                "path": relative,
                "fnv1a64": format!("{:016x}", fnv1a64(&bytes))
            }))
        })
        .collect()
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn require_exact_keys(
    value: &Value,
    expected: &[&str],
    diagnostic: &str,
) -> Result<(), ModelProofError> {
    let object = value
        .as_object()
        .ok_or_else(|| ModelProofError::Contract(diagnostic.to_owned()))?;
    let observed = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();
    if observed != expected {
        return Err(ModelProofError::Contract(diagnostic.to_owned()));
    }
    Ok(())
}

fn string_values<'a>(value: &'a Value, key: &str) -> Result<Vec<&'a str>, ModelProofError> {
    value
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| ModelProofError::Contract(format!("WP009-E-{key}")))?
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .filter(|text| !text.trim().is_empty())
                .ok_or_else(|| ModelProofError::Contract(format!("WP009-E-{key}")))
        })
        .collect()
}

fn required_text<'a>(
    value: &'a Value,
    key: &str,
    diagnostic: &str,
) -> Result<&'a str, ModelProofError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| ModelProofError::Contract(diagnostic.to_owned()))
}

fn execute_resource_durability_case(case: &Value) -> Result<Value, ModelProofError> {
    let id = required_text(case, "case_id", "WP009-E-CASE-ID")?;
    match id {
        "wp009-resource-atomic-saturation" => resource_atomic_saturation(case),
        "wp009-resource-fifo-head" => resource_fifo_head(case),
        "wp009-resource-queue-item-bound" => resource_queue_item_bound(case),
        "wp009-resource-queue-byte-bound" => resource_queue_byte_bound(case),
        "wp009-resource-raii-drop" => resource_raii_drop(case),
        "wp009-resource-public-owned-boundary" => resource_public_owned_boundary(case),
        "wp009-credit-coupled-reservation" => credit_coupled_reservation(case),
        "wp009-credit-owner-attribution" => credit_owner_attribution(case),
        "wp009-credit-stage-bounds-all-nine" => credit_stage_bounds_all_nine(case),
        "wp009-credit-owner-bounds-all-nine" => credit_owner_bounds_all_nine(case),
        "wp009-credit-owned-success"
        | "wp009-credit-owned-error"
        | "wp009-credit-owned-panic"
        | "wp009-credit-owned-cancel" => credit_owned_outcome(case),
        "wp009-durability-effect-ack" => durability_effect_ack(case),
        "wp009-durability-stage-authorization" => durability_stage_authorization(case),
        "wp009-durability-replay-rejected" => durability_replay_rejected(case),
        "wp009-durability-restoration-evidence" => durability_restoration_evidence(case),
        "wp009-durability-prefix" => durability_prefix(case),
        "wp009-durability-outrun" => durability_outrun(case),
        "wp009-journal-torn"
        | "wp009-journal-duplicate"
        | "wp009-journal-reordered"
        | "wp009-journal-checksum-invalid" => journal_fault(case),
        "wp009-journal-false-prepared-sync"
        | "wp009-journal-false-renamed-sync"
        | "wp009-journal-mixed-job"
        | "wp009-journal-archive-uniqueness" => journal_semantic_fault(case),
        "wp009-commit-prepared-effects"
        | "wp009-commit-renamed-effects"
        | "wp009-commit-archived-effects"
        | "wp009-commit-cleaned-effects" => commit_effect_case(case),
        "wp009-recovery-prepared"
        | "wp009-recovery-renamed"
        | "wp009-recovery-archived"
        | "wp009-recovery-cleaned"
        | "wp009-recovery-stale-lease"
        | "wp009-recovery-collision"
        | "wp009-recovery-partial-cleanup"
        | "wp009-recovery-interrupted-migration"
        | "wp009-recovery-collecting-artifact"
        | "wp009-recovery-stale-mismatch-precedence"
        | "wp009-recovery-migration-collision-precedence"
        | "wp009-recovery-confinement-unavailable"
        | "wp009-recovery-confinement-mismatched"
        | "wp009-cross-volume" => recovery_case(case),
        "wp009-filesystem-windows-ntfs" | "wp009-filesystem-wsl-v9fs" => filesystem_case(case),
        _ => Err(ModelProofError::Contract(
            "WP009-E-UNMAPPED-INPUT".to_owned(),
        )),
    }
}

fn model_vector(memory_bytes: u64, open_handles: u32, ffmpeg_processes: u32) -> ResourceVector {
    ResourceVector {
        memory_bytes,
        open_handles,
        ffmpeg_processes,
        ..ResourceVector::default()
    }
}

#[allow(clippy::too_many_arguments)]
fn proof_row(
    case: &Value,
    invariant_id: &str,
    state: &str,
    resource_vector: ResourceVector,
    credit_owner: &str,
    queue_items: usize,
    queue_bytes: u64,
    queue_item_bound: usize,
    queue_byte_bound: u64,
    durability: DurabilityPosition,
    resume_at: u64,
    recovery_action: &str,
    boundary: &str,
    expected: &str,
    observed: &str,
    proof_mechanism: &str,
    residual_uncertainty: &str,
) -> Value {
    let proof_mechanism = match proof_mechanism {
        "semantic" => "direct_behavior",
        "counterfactual" => "negative_counterfactual",
        "public_boundary" => "public_boundary",
        other => other,
    };
    json!({
        "case_id": case["case_id"],
        "invariant_id": invariant_id,
        "state": state,
        "resource_vector": resource_vector,
        "credit_owner": credit_owner,
        "queue_occupancy": {
            "items": queue_items,
            "bytes": queue_bytes,
            "item_bound": queue_item_bound,
            "byte_bound": queue_byte_bound
        },
        "durability_prefix": {
            "received": durability.received_bytes,
            "validated_written_contiguous": durability.validated_bytes,
            "durable_contiguous": durability.durable_bytes,
            "resume_at": resume_at
        },
        "injected_fault": case["injected_fault"],
        "recovery_action": recovery_action,
        "concrete_input": case,
        "boundary": boundary,
        "expected": expected,
        "observed": observed,
        "proof_class": "semantic",
        "proof_mechanism": proof_mechanism,
        "credit_attribution": null,
        "supplemental_execution": null,
        "residual_uncertainty": residual_uncertainty
    })
}

fn resource_broker(
    capacity: ResourceVector,
    max_active: u64,
    max_owner: u64,
    max_waiters: u64,
    max_waiter_bytes: u64,
) -> Result<OwnedResourceBroker, ModelProofError> {
    OwnedResourceBroker::from_contract(&ResourceContractV1::new(
        capacity,
        max_active,
        max_owner,
        max_waiters,
        max_waiter_bytes,
    ))
    .map_err(|error| ModelProofError::Contract(format!("WP009-E-RESOURCE:{error:?}")))
}

fn resource_atomic_saturation(case: &Value) -> Result<Value, ModelProofError> {
    let capacity = model_vector(10, 2, 1);
    let broker = resource_broker(capacity, 1, 1, 2, 20)?;
    let first = broker
        .request(OwnerId(1), capacity)
        .map_err(resource_error)?;
    let OwnedAdmission::Granted(_first_lease) = first else {
        return Err(ModelProofError::Contract(
            "WP009-E-ATOMIC-FIRST-NOT-GRANTED".to_owned(),
        ));
    };
    let second = broker
        .request(OwnerId(2), model_vector(1, 0, 0))
        .map_err(resource_error)?;
    let OwnedAdmission::Queued(_second_waiter) = second else {
        return Err(ModelProofError::Contract(
            "WP009-E-PARTIAL-RESOURCE-GRANT".to_owned(),
        ));
    };
    if broker.in_use() != capacity {
        return Err(ModelProofError::Contract(
            "WP009-E-PARTIAL-RESOURCE-GRANT".to_owned(),
        ));
    }
    broker.verify().map_err(resource_error)?;
    let occupancy = broker.waiter_occupancy();
    Ok(proof_row(
        case,
        "WP009-INV-RESOURCE-ATOMIC-001",
        "capacity_saturated_waiter_queued",
        broker.in_use(),
        "none",
        occupancy.0,
        occupancy.1,
        2,
        20,
        DurabilityPosition::default(),
        0,
        "none",
        "fforager_core::resource::OwnedResourceBroker::request",
        "the complete second vector is queued and no dimension is partially granted",
        "the complete second vector was queued; in_use remained the exact first vector",
        "semantic",
        "The pure broker model does not execute production work.",
    ))
}

fn resource_fifo_head(case: &Value) -> Result<Value, ModelProofError> {
    let capacity = model_vector(10, 0, 0);
    let broker = resource_broker(capacity, 1, 1, 2, 20)?;
    let OwnedAdmission::Granted(first) = broker
        .request(OwnerId(1), capacity)
        .map_err(resource_error)?
    else {
        return Err(ModelProofError::Contract("WP009-E-FIFO-FIRST".to_owned()));
    };
    let OwnedAdmission::Queued(mut head) = broker
        .request(OwnerId(2), capacity)
        .map_err(resource_error)?
    else {
        return Err(ModelProofError::Contract("WP009-E-FIFO-HEAD".to_owned()));
    };
    let head_id = head.id();
    let OwnedAdmission::Queued(_later) = broker
        .request(OwnerId(3), model_vector(1, 0, 0))
        .map_err(resource_error)?
    else {
        return Err(ModelProofError::Contract("WP009-E-FIFO-LATER".to_owned()));
    };
    let issued = first.release().map_err(resource_error)?;
    if issued != [head_id] {
        return Err(ModelProofError::Contract("WP009-E-FIFO-BYPASS".to_owned()));
    }
    let Some(_head_lease) = head.try_acquire().map_err(resource_error)? else {
        return Err(ModelProofError::Contract(
            "WP009-E-FIFO-HEAD-NOT-READY".to_owned(),
        ));
    };
    broker.verify().map_err(resource_error)?;
    let occupancy = broker.waiter_occupancy();
    Ok(proof_row(
        case,
        "WP009-INV-FIFO-HEAD-001",
        "head_issued_later_waiter_retained",
        broker.in_use(),
        "none",
        occupancy.0,
        occupancy.1,
        2,
        20,
        DurabilityPosition::default(),
        0,
        "issue_exact_fifo_head",
        "fforager_core::resource::OwnedResourceLease::release then OwnedResourceWaiter::try_acquire",
        "the exact queue head issues before the smaller later waiter",
        "release issued the exact head waiter identity and retained one later waiter",
        "semantic",
        "The starvation bound remains conditional on eventual active-grant release.",
    ))
}

fn resource_queue_item_bound(case: &Value) -> Result<Value, ModelProofError> {
    let capacity = model_vector(10, 0, 0);
    let broker = resource_broker(capacity, 1, 1, 1, 10)?;
    let _first = broker
        .request(OwnerId(1), capacity)
        .map_err(resource_error)?;
    let _waiter = broker
        .request(OwnerId(2), model_vector(1, 0, 0))
        .map_err(resource_error)?;
    let rejected = broker.request(OwnerId(3), model_vector(1, 0, 0));
    if !matches!(rejected, Err(LedgerError::QueueItemLimit)) || broker.waiter_occupancy() != (1, 1)
    {
        return Err(ModelProofError::Contract(
            "WP009-E-QUEUE-ITEM-BOUND".to_owned(),
        ));
    }
    Ok(proof_row(
        case,
        "WP009-INV-QUEUE-ITEM-BOUND-001",
        "item_limit_rejected_without_mutation",
        broker.in_use(),
        "none",
        1,
        1,
        1,
        10,
        DurabilityPosition::default(),
        0,
        "reject_and_backpressure_upstream",
        "fforager_core::resource::OwnedResourceBroker::request",
        "one waiter beyond the declared item bound is rejected without occupancy growth",
        "QueueItemLimit returned and occupancy remained one item/one byte",
        "counterfactual",
        "The caller must propagate this typed refusal as upstream backpressure.",
    ))
}

fn resource_queue_byte_bound(case: &Value) -> Result<Value, ModelProofError> {
    let capacity = model_vector(10, 0, 0);
    let broker = resource_broker(capacity, 1, 1, 2, 5)?;
    let _first = broker
        .request(OwnerId(1), capacity)
        .map_err(resource_error)?;
    let rejected = broker.request(OwnerId(2), model_vector(6, 0, 0));
    if !matches!(rejected, Err(LedgerError::QueueByteLimit)) || broker.waiter_occupancy() != (0, 0)
    {
        return Err(ModelProofError::Contract(
            "WP009-E-QUEUE-BYTE-BOUND".to_owned(),
        ));
    }
    Ok(proof_row(
        case,
        "WP009-INV-QUEUE-BYTE-BOUND-001",
        "byte_limit_rejected_without_mutation",
        broker.in_use(),
        "none",
        0,
        0,
        2,
        5,
        DurabilityPosition::default(),
        0,
        "reject_and_backpressure_upstream",
        "fforager_core::resource::OwnedResourceBroker::request",
        "a six-byte waiter is rejected by the five-byte declared queue bound",
        "QueueByteLimit returned and occupancy remained zero",
        "counterfactual",
        "The model counts declared variable bytes, not allocator overhead.",
    ))
}

fn resource_raii_drop(case: &Value) -> Result<Value, ModelProofError> {
    let capacity = model_vector(10, 0, 0);
    let broker = resource_broker(capacity, 1, 1, 2, 20)?;
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let admission = broker.request(OwnerId(1), capacity);
        let Ok(OwnedAdmission::Granted(_lease)) = admission else {
            panic!("intentional WP009 unwind probe failed to acquire owned lease");
        };
        panic!("intentional WP009 unwind after owned lease acquisition");
    }));
    if unwind.is_ok() || broker.in_use() != ResourceVector::default() {
        return Err(ModelProofError::Contract(
            "WP009-E-RAII-DROP-LEAK".to_owned(),
        ));
    }
    broker.verify().map_err(resource_error)?;
    Ok(proof_row(
        case,
        "WP009-INV-RAII-RELEASE-001",
        "panic_caught_lease_released",
        broker.in_use(),
        "none",
        0,
        0,
        2,
        20,
        DurabilityPosition::default(),
        0,
        "OwnedResourceLease::drop",
        "fforager_core::resource::OwnedResourceLease Drop boundary",
        "panic unwind drops the non-cloneable lease and releases the full vector exactly once",
        "the intentional unwind was caught and broker in_use returned to the zero vector",
        "public_boundary",
        "Abort-on-panic builds do not unwind and therefore require process-level containment.",
    ))
}

fn resource_public_owned_boundary(case: &Value) -> Result<Value, ModelProofError> {
    let capacity = model_vector(4, 1, 0);
    let broker = resource_broker(capacity, 1, 1, 1, 4)?;
    let lease = match broker
        .request(OwnerId(1), capacity)
        .map_err(resource_error)?
    {
        OwnedAdmission::Granted(lease) => lease,
        OwnedAdmission::Queued(_) => {
            return Err(ModelProofError::Contract(
                "WP009-E-PUBLIC-OWNED-GRANT".to_owned(),
            ));
        }
    };
    let waiter = match broker
        .request(OwnerId(2), model_vector(1, 0, 0))
        .map_err(resource_error)?
    {
        OwnedAdmission::Queued(waiter) => waiter,
        OwnedAdmission::Granted(_) => {
            return Err(ModelProofError::Contract(
                "WP009-E-PUBLIC-OWNED-WAITER".to_owned(),
            ));
        }
    };
    drop(waiter);
    drop(lease);
    if broker.in_use() != ResourceVector::default() || broker.waiter_occupancy() != (0, 0) {
        return Err(ModelProofError::Contract(
            "WP009-E-PUBLIC-OWNERSHIP-ESCAPE".to_owned(),
        ));
    }
    broker.verify().map_err(resource_error)?;
    Ok(proof_row(
        case,
        "WP009-INV-PUBLIC-OWNERSHIP-001",
        "owned_grant_and_waiter_released",
        broker.in_use(),
        "none",
        0,
        0,
        1,
        4,
        DurabilityPosition::default(),
        0,
        "drop_owned_grant_and_waiter",
        "fforager_core::resource::OwnedResourceBroker public boundary",
        "public requests return non-cloneable owned grant/waiter identities whose drops mutate the broker",
        "the public broker returned owned identities and both occupancies returned to zero after drop",
        "public_boundary",
        "The independent xtask guard also rejects any regression that makes raw ledgers public.",
    ))
}

#[allow(clippy::needless_pass_by_value)] // `Result::map_err` transfers the typed error here.
fn resource_error(error: LedgerError) -> ModelProofError {
    ModelProofError::Contract(format!("WP009-E-RESOURCE:{error:?}"))
}

fn credit_coupled_reservation(case: &Value) -> Result<Value, ModelProofError> {
    let reservation = CoupledByteReservation {
        input_stage: ByteCreditStage::DecryptOrPackInput,
        input_bytes: 6,
        output_stage: ByteCreditStage::DecryptOrPackOutput,
        output_bytes: 5,
    };
    let credits = byte_credit_broker(10, 2)?;
    let rejected = credits.claim_coupled(OwnerId(1), reservation);
    let occupancy = credits.global_occupancy().map_err(credit_error)?;
    if !matches!(rejected, Err(CreditError::Backpressured(_))) || occupancy != (0, 0) {
        return Err(ModelProofError::Contract(
            "WP009-E-COUPLED-PARTIAL-CLAIM".to_owned(),
        ));
    }
    credits.verify().map_err(credit_error)?;
    Ok(proof_row(
        case,
        "WP009-INV-COUPLED-BYTE-RESERVATION-001",
        "combined_input_output_rejected_before_allocation",
        ResourceVector::default(),
        "owner_1",
        usize::try_from(occupancy.0).unwrap_or(usize::MAX),
        occupancy.1,
        2,
        10,
        DurabilityPosition::default(),
        0,
        "reserve_complete_input_plus_output_or_nothing",
        "fforager_core::resource::OwnedByteCreditBroker::claim_coupled",
        "the complete eleven-byte simultaneous reservation is rejected against ten-byte capacity",
        "Backpressured returned and no partial input/output credit remained live",
        "semantic",
        "The model does not perform allocation; later stages must acquire this claim before allocating.",
    ))
}

fn credit_owner_attribution(case: &Value) -> Result<Value, ModelProofError> {
    let credits = byte_credit_broker(8, 2)?;
    let mut claim = credits
        .claim_coupled(
            OwnerId(1),
            CoupledByteReservation {
                input_stage: ByteCreditStage::DecryptOrPackInput,
                input_bytes: 4,
                output_stage: ByteCreditStage::DecryptOrPackOutput,
                output_bytes: 4,
            },
        )
        .map_err(credit_error)?;
    claim
        .consume(ByteCreditComponent::Input, 4)
        .map_err(credit_error)?;
    if !matches!(
        claim.transfer_component(ByteCreditComponent::Input, OwnerId(2)),
        Err(CreditError::ConsumedComponentCannotTransfer {
            component: ByteCreditComponent::Input,
            consumed: 4
        })
    ) {
        return Err(ModelProofError::Contract(
            "WP009-E-CREDIT-OWNER-REWRITE".to_owned(),
        ));
    }
    claim
        .transfer_component(ByteCreditComponent::Output, OwnerId(2))
        .map_err(credit_error)?;
    let attribution = claim.attribution().map_err(credit_error)?;
    let input = attribution
        .components
        .iter()
        .find(|component| component.component == ByteCreditComponent::Input);
    let output = attribution
        .components
        .iter()
        .find(|component| component.component == ByteCreditComponent::Output);
    if input.is_none_or(|component| component.owner != OwnerId(1) || component.consumed != 4)
        || output.is_none_or(|component| component.owner != OwnerId(2) || component.consumed != 0)
    {
        return Err(ModelProofError::Contract(
            "WP009-E-CREDIT-ATTRIBUTION".to_owned(),
        ));
    }
    credits.verify().map_err(credit_error)?;
    let occupancy = credits.global_occupancy().map_err(credit_error)?;
    Ok(proof_row(
        case,
        "WP009-INV-CREDIT-OWNER-001",
        "consumed_claim_transfer_rejected",
        ResourceVector::default(),
        "owner_1",
        usize::try_from(occupancy.0).unwrap_or(usize::MAX),
        occupancy.1,
        2,
        8,
        DurabilityPosition::default(),
        0,
        "retain_consumed_input_attribution_and_transfer_unconsumed_output",
        "fforager_core::resource::OwnedByteCreditLease consume, transfer_component, and attribution",
        "consumed input attribution remains with owner_1 while unconsumed output transfers to owner_2",
        "ConsumedComponentCannotTransfer protected input; output transferred with exact component attribution",
        "counterfactual",
        "Owner identity is a pure model identifier, not an authenticated runtime principal.",
    ))
}

fn credit_pressure_matches(
    result: &Result<fforager_core::resource::OwnedByteCreditLease, CreditError>,
    scope: CreditLimitScope,
    stage: ByteCreditStage,
) -> bool {
    matches!(
        result,
        Err(CreditError::Backpressured(pressure) | CreditError::SpillRequired(pressure))
            if pressure.scope == scope && pressure.stage == stage
    )
}

fn credit_stage_bounds_all_nine(case: &Value) -> Result<Value, ModelProofError> {
    for stage in BYTE_CREDIT_STAGES_V1 {
        let mut item_contract = ByteCreditContractV1::new(16, 4);
        let policy = item_contract
            .stage_policies
            .iter_mut()
            .find(|policy| policy.stage == stage)
            .ok_or_else(|| {
                ModelProofError::Contract("WP009-E-CREDIT-STAGE-INVENTORY".to_owned())
            })?;
        policy.max_claim_items = 1;
        policy.max_bytes = 16;
        let item_broker =
            OwnedByteCreditBroker::from_contract(&item_contract).map_err(credit_error)?;
        let first = item_broker
            .claim(OwnerId(1), stage, 1)
            .map_err(credit_error)?;
        let item_rejected = item_broker.claim(OwnerId(2), stage, 1);
        if !credit_pressure_matches(&item_rejected, CreditLimitScope::Stage, stage) {
            return Err(ModelProofError::Contract(format!(
                "WP009-E-CREDIT-STAGE-ITEM:{stage:?}"
            )));
        }
        drop(first);
        if item_broker.global_occupancy().map_err(credit_error)? != (0, 0) {
            return Err(ModelProofError::Contract(
                "WP009-E-CREDIT-STAGE-ITEM-LEAK".to_owned(),
            ));
        }

        let mut byte_contract = ByteCreditContractV1::new(16, 4);
        let policy = byte_contract
            .stage_policies
            .iter_mut()
            .find(|policy| policy.stage == stage)
            .ok_or_else(|| {
                ModelProofError::Contract("WP009-E-CREDIT-STAGE-INVENTORY".to_owned())
            })?;
        policy.max_claim_items = 4;
        policy.max_bytes = 2;
        let byte_broker =
            OwnedByteCreditBroker::from_contract(&byte_contract).map_err(credit_error)?;
        let byte_rejected = byte_broker.claim(OwnerId(1), stage, 3);
        if !credit_pressure_matches(&byte_rejected, CreditLimitScope::Stage, stage)
            || byte_broker.global_occupancy().map_err(credit_error)? != (0, 0)
        {
            return Err(ModelProofError::Contract(format!(
                "WP009-E-CREDIT-STAGE-BYTE:{stage:?}"
            )));
        }
    }
    Ok(proof_row(
        case,
        "WP009-INV-CREDIT-STAGE-BOUNDS-001",
        "all_nine_stage_bounds_rejected_without_mutation",
        ResourceVector::default(),
        "nine_stage_inventory",
        0,
        0,
        4,
        16,
        DurabilityPosition::default(),
        0,
        "backpressure_or_spill_per_declared_stage_policy",
        "fforager_core::resource::OwnedByteCreditBroker::claim across BYTE_CREDIT_STAGES_V1",
        "every declared stage enforces both its item bound and byte bound without partial occupancy",
        "all nine stages returned their declared typed saturation outcome for item and byte overflow",
        "counterfactual",
        "This bounded model checks one stage at a time and does not benchmark concurrent throughput.",
    ))
}

fn credit_owner_bounds_all_nine(case: &Value) -> Result<Value, ModelProofError> {
    for stage in BYTE_CREDIT_STAGES_V1 {
        let mut item_contract = ByteCreditContractV1::new(16, 4);
        item_contract.max_owner_claim_items = 1;
        item_contract.max_owner_bytes = 16;
        let item_broker =
            OwnedByteCreditBroker::from_contract(&item_contract).map_err(credit_error)?;
        let first = item_broker
            .claim(OwnerId(1), stage, 1)
            .map_err(credit_error)?;
        let item_rejected = item_broker.claim(OwnerId(1), stage, 1);
        if !credit_pressure_matches(&item_rejected, CreditLimitScope::Owner, stage) {
            return Err(ModelProofError::Contract(format!(
                "WP009-E-CREDIT-OWNER-ITEM:{stage:?}"
            )));
        }
        drop(first);

        let mut byte_contract = ByteCreditContractV1::new(16, 4);
        byte_contract.max_owner_claim_items = 4;
        byte_contract.max_owner_bytes = 2;
        let byte_broker =
            OwnedByteCreditBroker::from_contract(&byte_contract).map_err(credit_error)?;
        let byte_rejected = byte_broker.claim(OwnerId(1), stage, 3);
        if !credit_pressure_matches(&byte_rejected, CreditLimitScope::Owner, stage)
            || byte_broker.global_occupancy().map_err(credit_error)? != (0, 0)
        {
            return Err(ModelProofError::Contract(format!(
                "WP009-E-CREDIT-OWNER-BYTE:{stage:?}"
            )));
        }
    }
    Ok(proof_row(
        case,
        "WP009-INV-CREDIT-OWNER-BOUNDS-001",
        "all_nine_stage_owner_bounds_rejected_without_mutation",
        ResourceVector::default(),
        "owner_1",
        0,
        0,
        4,
        16,
        DurabilityPosition::default(),
        0,
        "backpressure_or_spill_per_owner",
        "fforager_core::resource::OwnedByteCreditBroker::claim across owner and stage scopes",
        "one owner cannot exceed its item or byte limit at any declared stage",
        "all nine stage requests returned owner-scoped typed saturation without occupancy growth",
        "counterfactual",
        "OwnerId is a model identity; adapter authentication remains outside this proof.",
    ))
}

#[allow(clippy::too_many_lines)]
fn credit_owned_outcome(case: &Value) -> Result<Value, ModelProofError> {
    let id = required_text(case, "case_id", "WP009-E-CREDIT-OWNED-CASE")?;
    let credits = byte_credit_broker(8, 2)?;
    let mut cancellation_attribution = None;
    let (invariant, state, action, expected, observed) = match id {
        "wp009-credit-owned-success" => {
            let mut claim = credits
                .claim(OwnerId(1), ByteCreditStage::Writer, 4)
                .map_err(credit_error)?;
            claim
                .consume(ByteCreditComponent::Single, 4)
                .map_err(credit_error)?;
            claim.release().map_err(credit_error)?;
            (
                "WP009-INV-CREDIT-OWNED-SUCCESS-001",
                "explicit_release_zeroed_occupancy",
                "consume_then_release",
                "successful consumption remains attributed until explicit owned release",
                "consume and release succeeded; global occupancy returned to zero",
            )
        }
        "wp009-credit-owned-error" => {
            let mut claim = credits
                .claim(OwnerId(1), ByteCreditStage::Writer, 4)
                .map_err(credit_error)?;
            if !matches!(
                claim.consume(ByteCreditComponent::Single, 5),
                Err(CreditError::UncreditedBytes { .. })
            ) || credits.global_occupancy().map_err(credit_error)? != (1, 4)
            {
                return Err(ModelProofError::Contract(
                    "WP009-E-CREDIT-OWNED-ERROR".to_owned(),
                ));
            }
            drop(claim);
            (
                "WP009-INV-CREDIT-OWNED-ERROR-001",
                "error_retained_then_drop_released_credit",
                "reject_uncredited_bytes_then_drop",
                "an over-consume error preserves the owned claim until its drop path releases it",
                "UncreditedBytes preserved one four-byte claim; drop then returned occupancy to zero",
            )
        }
        "wp009-credit-owned-panic" => {
            let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _claim = credits
                    .claim(OwnerId(1), ByteCreditStage::Writer, 4)
                    .expect("intentional WP009 credit unwind must acquire");
                panic!("intentional WP009 unwind after owned byte-credit acquisition");
            }));
            if unwind.is_ok() {
                return Err(ModelProofError::Contract(
                    "WP009-E-CREDIT-PANIC-NOT-OBSERVED".to_owned(),
                ));
            }
            (
                "WP009-INV-CREDIT-OWNED-PANIC-001",
                "panic_caught_credit_released",
                "OwnedByteCreditLease::drop",
                "unwind drops the owned credit identity and releases every component exactly once",
                "intentional unwind was caught and global occupancy returned to zero",
            )
        }
        "wp009-credit-owned-cancel" => {
            let claim = credits
                .claim(OwnerId(1), ByteCreditStage::Writer, 4)
                .map_err(credit_error)?;
            let attribution = claim.cancel().map_err(credit_error)?;
            let Some(component) = attribution.components.first() else {
                return Err(ModelProofError::Contract(
                    "WP009-E-CREDIT-CANCEL-ATTRIBUTION".to_owned(),
                ));
            };
            if attribution.components.len() != 1
                || component.component != ByteCreditComponent::Single
                || component.owner != OwnerId(1)
                || component.stage != ByteCreditStage::Writer
                || component.bytes != 4
                || component.consumed != 0
            {
                return Err(ModelProofError::Contract(
                    "WP009-E-CREDIT-CANCEL-ATTRIBUTION".to_owned(),
                ));
            }
            cancellation_attribution = Some(json!({
                "component": "single",
                "owner": 1,
                "stage": "writer",
                "claimed_bytes": 4,
                "consumed_bytes": 0,
                "lease_state": "cancelled_and_released",
                "global_claim_items_after": 0,
                "global_bytes_after": 0
            }));
            (
                "WP009-INV-CREDIT-OWNED-CANCEL-001",
                "cancel_released_credit_with_attribution",
                "OwnedByteCreditLease::cancel",
                "cancelling an active owned claim returns its attribution and releases capacity",
                "cancel returned one exact component attribution and global occupancy returned to zero",
            )
        }
        _ => {
            return Err(ModelProofError::Contract(
                "WP009-E-CREDIT-OWNED-UNMAPPED".to_owned(),
            ));
        }
    };
    if credits.global_occupancy().map_err(credit_error)? != (0, 0) {
        return Err(ModelProofError::Contract(
            "WP009-E-CREDIT-OWNED-LEAK".to_owned(),
        ));
    }
    credits.verify().map_err(credit_error)?;
    let mut row = proof_row(
        case,
        invariant,
        state,
        ResourceVector::default(),
        "owner_1",
        0,
        0,
        2,
        8,
        DurabilityPosition::default(),
        0,
        action,
        "fforager_core::resource::OwnedByteCreditLease public ownership boundary",
        expected,
        observed,
        "public_boundary",
        "Abort-on-panic builds require process containment; this row exercises unwind semantics.",
    );
    if let Some(attribution) = cancellation_attribution {
        row["credit_attribution"] = attribution;
    }
    Ok(row)
}

#[allow(clippy::too_many_lines)]
fn durability_effect_ack(case: &Value) -> Result<Value, ModelProofError> {
    let contract = ByteCreditContractV1::new(16, 4);
    let broker = OwnedByteCreditBroker::from_contract(&contract).map_err(credit_error)?;
    let foreign_broker = OwnedByteCreditBroker::from_contract(&contract).map_err(credit_error)?;
    let mut receive_claim = broker
        .claim(OwnerId(1), ByteCreditStage::HttpReceive, 8)
        .map_err(credit_error)?;
    receive_claim
        .consume(ByteCreditComponent::Single, 8)
        .map_err(credit_error)?;
    let mut writer_claim = broker
        .claim(OwnerId(1), ByteCreditStage::Writer, 8)
        .map_err(credit_error)?;
    writer_claim
        .consume(ByteCreditComponent::Single, 8)
        .map_err(credit_error)?;
    let instance = MachineInstanceId::new(9009)
        .ok_or_else(|| ModelProofError::Contract("WP009-E-MACHINE-ID".to_owned()))?;
    let mut model = StateMachine::new(MachineKind::ByteCreditDurability, instance, 16);

    model.apply(Event::Receive).map_err(transition_error)?;
    let receive = model.pending_acknowledgements()[0];
    let receive_event = correlated_ack(receive);
    if !matches!(
        model.apply(receive_event),
        Err(TransitionError::ByteDurabilityAcknowledgementRequired {
            effect: EffectIntent::AcceptBoundedBytes
        })
    ) || broker.position() != ByteCreditPosition::default()
    {
        return Err(ModelProofError::Contract(
            "WP009-E-DURABILITY-ORDINARY-ACK".to_owned(),
        ));
    }
    model
        .acknowledge_byte_durability_effect(
            &broker,
            receive_event,
            ByteCreditPosition {
                received: 8,
                validated_written_contiguous: 0,
                durable_contiguous: 0,
            },
        )
        .map_err(byte_effect_error)?;

    model.apply(Event::Validate).map_err(transition_error)?;
    let write = model.pending_acknowledgements()[0];
    let write_event = correlated_ack(write);
    let wrong_broker_observed = match model.acknowledge_byte_durability_effect(
        &foreign_broker,
        write_event,
        ByteCreditPosition {
            received: 8,
            validated_written_contiguous: 8,
            durable_contiguous: 0,
        },
    ) {
        Err(ByteDurabilityEffectError::Credit(CreditError::AcknowledgementBrokerMismatch)) => {
            wp009_wrong_broker_result()
        }
        Err(error) => {
            return Err(ModelProofError::Contract(format!(
                "WP009-E-DURABILITY-WRONG-BROKER-RESULT:{error:?}"
            )));
        }
        Ok(_) => {
            return Err(ModelProofError::Contract(
                "WP009-E-DURABILITY-WRONG-BROKER-NOOP".to_owned(),
            ));
        }
    };
    if !matches!(
        model.acknowledge_byte_durability_effect(
            &broker,
            write_event,
            ByteCreditPosition {
                received: 8,
                validated_written_contiguous: 0,
                durable_contiguous: 0,
            },
        ),
        Err(ByteDurabilityEffectError::PositionDoesNotMatchEffect {
            effect: EffectIntent::ValidateAndWrite
        })
    ) {
        return Err(ModelProofError::Contract(
            "WP009-E-DURABILITY-NOOP-DELTA".to_owned(),
        ));
    }
    if !matches!(
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
    ) {
        return Err(ModelProofError::Contract(
            "WP009-E-DURABILITY-WRONG-DELTA".to_owned(),
        ));
    }
    model
        .acknowledge_byte_durability_effect(
            &broker,
            write_event,
            ByteCreditPosition {
                received: 8,
                validated_written_contiguous: 8,
                durable_contiguous: 0,
            },
        )
        .map_err(byte_effect_error)?;

    model
        .apply(Event::PersistDurably)
        .map_err(transition_error)?;
    let sync = model.pending_acknowledgements()[0];
    model
        .acknowledge_byte_durability_effect(
            &broker,
            correlated_ack(sync),
            ByteCreditPosition {
                received: 8,
                validated_written_contiguous: 8,
                durable_contiguous: 8,
            },
        )
        .map_err(byte_effect_error)?;
    if model.state() != State::BytesDurable
        || broker.position().durable_contiguous != 8
        || broker.position().validated_written_contiguous != 8
    {
        return Err(ModelProofError::Contract(
            "WP009-E-DURABILITY-EFFECT-SEQUENCE".to_owned(),
        ));
    }
    let mut row = proof_row(
        case,
        "WP009-REG-DURABLE-EFFECT-ACK-001",
        "received_written_and_durable_effects_acknowledged",
        ResourceVector::default(),
        "owner_1",
        2,
        16,
        4,
        16,
        DurabilityPosition {
            received_bytes: 8,
            validated_bytes: 8,
            durable_bytes: 8,
        },
        8,
        "accept_then_validate_then_synchronize_with_correlated_receipts",
        "StateMachine::acknowledge_byte_durability_effect with OwnedByteCreditBroker",
        "ordinary acknowledgements and wrong effect deltas fail before broker advancement; correlated receipts advance each exact prefix",
        "ordinary receive acknowledgement and wrong validate delta were rejected; three correlated receipts reached BytesDurable at 8/8/8",
        "public_boundary",
        "The wrong-broker call is executed in this row through the public lifecycle boundary; no separate core-test output is trusted.",
    );
    row["supplemental_execution"] = json!({
        "boundary": "StateMachine::acknowledge_byte_durability_effect with foreign OwnedByteCreditBroker",
        "expected": wp009_wrong_broker_result(),
        "observed": wrong_broker_observed,
        "proof_class": "semantic"
    });
    Ok(row)
}

fn durability_replay_rejected(case: &Value) -> Result<Value, ModelProofError> {
    let contract = ByteCreditContractV1::new(16, 4);
    let broker = OwnedByteCreditBroker::from_contract(&contract).map_err(credit_error)?;
    let mut receive_claim = broker
        .claim(OwnerId(1), ByteCreditStage::HttpReceive, 4)
        .map_err(credit_error)?;
    receive_claim
        .consume(ByteCreditComponent::Single, 4)
        .map_err(credit_error)?;
    let mut writer_claim = broker
        .claim(OwnerId(1), ByteCreditStage::Writer, 4)
        .map_err(credit_error)?;
    writer_claim
        .consume(ByteCreditComponent::Single, 4)
        .map_err(credit_error)?;
    let instance = MachineInstanceId::new(9014)
        .ok_or_else(|| ModelProofError::Contract("WP009-E-MACHINE-ID".to_owned()))?;
    let mut model = StateMachine::new(MachineKind::ByteCreditDurability, instance, 16);
    model.apply(Event::Receive).map_err(transition_error)?;
    let receive = model.pending_acknowledgements()[0];
    model
        .acknowledge_byte_durability_effect(
            &broker,
            correlated_ack(receive),
            ByteCreditPosition {
                received: 4,
                validated_written_contiguous: 0,
                durable_contiguous: 0,
            },
        )
        .map_err(byte_effect_error)?;
    model.apply(Event::Validate).map_err(transition_error)?;
    let write = model.pending_acknowledgements()[0];
    model
        .acknowledge_byte_durability_effect(
            &broker,
            correlated_ack(write),
            ByteCreditPosition {
                received: 4,
                validated_written_contiguous: 4,
                durable_contiguous: 0,
            },
        )
        .map_err(byte_effect_error)?;
    let replay = StateMachine::replay(
        MachineKind::ByteCreditDurability,
        instance,
        16,
        model.trace(),
    );
    if !matches!(
        replay,
        Err(TransitionError::ByteDurabilityAcknowledgementRequired { .. })
    ) {
        return Err(ModelProofError::Contract(
            "WP009-E-DURABILITY-REPLAY-BYPASS".to_owned(),
        ));
    }
    Ok(proof_row(
        case,
        "WP009-REG-DURABLE-REPLAY-001",
        "ordinary_replay_rejected_byte_effect_history",
        ResourceVector::default(),
        "owner_1",
        2,
        8,
        4,
        16,
        DurabilityPosition {
            received_bytes: 4,
            validated_bytes: 4,
            durable_bytes: 0,
        },
        0,
        "reexecute_byte_history_through_broker_coupled_acknowledgements",
        "StateMachine::replay public boundary for ByteCreditDurability",
        "ordinary trace replay cannot authorize byte durability receipts without broker-coupled position evidence",
        "a valid broker-coupled 4/4/0 trace was rejected by ordinary replay at its first byte-effect acknowledgement",
        "counterfactual",
        "A byte-history verifier must re-execute receipts with the authoritative broker rather than use ordinary replay.",
    ))
}

fn durability_restoration_evidence(case: &Value) -> Result<Value, ModelProofError> {
    let contract = ByteCreditContractV1::new(16, 4);
    let broker = OwnedByteCreditBroker::from_contract(&contract).map_err(credit_error)?;
    let mut receive_claim = broker
        .claim(OwnerId(1), ByteCreditStage::HttpReceive, 4)
        .map_err(credit_error)?;
    receive_claim
        .consume(ByteCreditComponent::Single, 4)
        .map_err(credit_error)?;
    let instance = MachineInstanceId::new(9015)
        .ok_or_else(|| ModelProofError::Contract("WP009-E-MACHINE-ID".to_owned()))?;
    let mut model = StateMachine::new(MachineKind::ByteCreditDurability, instance, 8);
    model.apply(Event::Receive).map_err(transition_error)?;
    let receive = model.pending_acknowledgements()[0];
    model
        .acknowledge_byte_durability_effect(
            &broker,
            correlated_ack(receive),
            ByteCreditPosition {
                received: 4,
                validated_written_contiguous: 0,
                durable_contiguous: 0,
            },
        )
        .map_err(byte_effect_error)?;

    if !matches!(
        StateMachine::from_state(
            MachineKind::ByteCreditDurability,
            State::BytesReceived,
            instance,
            4,
            2,
        ),
        Err(TransitionError::ByteDurabilityRestorationEvidenceRequired {
            state: State::BytesReceived
        })
    ) {
        return Err(ModelProofError::Contract(
            "WP009-E-DURABILITY-GENERIC-RESTORE".to_owned(),
        ));
    }
    let restored =
        StateMachine::from_byte_durability_state(State::BytesReceived, &broker, instance, 4, 2)
            .map_err(|error| {
                ModelProofError::Contract(format!("WP009-E-DURABILITY-RESTORE:{error:?}"))
            })?;
    if restored.state() != State::BytesReceived
        || !matches!(
            StateMachine::from_byte_durability_state(
                State::BytesDurable,
                &broker,
                instance,
                4,
                2,
            ),
            Err(
                fforager_core::lifecycle::ByteDurabilityRestorationError::PositionDoesNotMatchState {
                    state: State::BytesDurable,
                    ..
                }
            )
        )
    {
        return Err(ModelProofError::Contract(
            "WP009-E-DURABILITY-RESTORE-MISMATCH".to_owned(),
        ));
    }
    Ok(proof_row(
        case,
        "WP009-REG-DURABLE-RESTORE-001",
        "broker_position_authorized_received_restore",
        ResourceVector::default(),
        "owner_1",
        1,
        4,
        4,
        16,
        DurabilityPosition {
            received_bytes: 4,
            validated_bytes: 0,
            durable_bytes: 0,
        },
        0,
        "restore_only_through_matching_authoritative_broker_position",
        "StateMachine::from_state and from_byte_durability_state public boundaries",
        "generic byte restoration and a mismatched durable phase fail while a broker-matching received phase restores",
        "generic BytesReceived restoration required evidence; broker-specific BytesReceived restored; BytesDurable mismatch failed",
        "public_boundary",
        "Restoration validates pure broker position and lifecycle phase; adapter recovery must still reconstruct authoritative broker state.",
    ))
}

#[allow(clippy::too_many_lines)]
fn durability_stage_authorization(case: &Value) -> Result<Value, ModelProofError> {
    let contract = ByteCreditContractV1::new(16, 4);

    let decompression_broker =
        OwnedByteCreditBroker::from_contract(&contract).map_err(credit_error)?;
    let mut decompression = decompression_broker
        .claim(OwnerId(1), ByteCreditStage::Decompression, 4)
        .map_err(credit_error)?;
    decompression
        .consume(ByteCreditComponent::Single, 4)
        .map_err(credit_error)?;
    let mut receive_model = StateMachine::new(
        MachineKind::ByteCreditDurability,
        MachineInstanceId::new(9012)
            .ok_or_else(|| ModelProofError::Contract("WP009-E-MACHINE-ID".to_owned()))?,
        8,
    );
    receive_model
        .apply(Event::Receive)
        .map_err(transition_error)?;
    let receive_ack = receive_model.pending_acknowledgements()[0];
    if !matches!(
        receive_model.acknowledge_byte_durability_effect(
            &decompression_broker,
            correlated_ack(receive_ack),
            ByteCreditPosition {
                received: 4,
                validated_written_contiguous: 0,
                durable_contiguous: 0,
            },
        ),
        Err(ByteDurabilityEffectError::Credit(
            CreditError::ReceivedAheadOfConsumed
        ))
    ) || decompression_broker.position() != ByteCreditPosition::default()
    {
        return Err(ModelProofError::Contract(
            "WP009-E-DURABILITY-DECOMPRESSION-AUTHORIZATION".to_owned(),
        ));
    }

    let receive_only_broker =
        OwnedByteCreditBroker::from_contract(&contract).map_err(credit_error)?;
    let mut receive_only = receive_only_broker
        .claim(OwnerId(1), ByteCreditStage::HttpReceive, 4)
        .map_err(credit_error)?;
    receive_only
        .consume(ByteCreditComponent::Single, 4)
        .map_err(credit_error)?;
    let mut write_model = StateMachine::new(
        MachineKind::ByteCreditDurability,
        MachineInstanceId::new(9013)
            .ok_or_else(|| ModelProofError::Contract("WP009-E-MACHINE-ID".to_owned()))?,
        8,
    );
    write_model
        .apply(Event::Receive)
        .map_err(transition_error)?;
    let receive = write_model.pending_acknowledgements()[0];
    write_model
        .acknowledge_byte_durability_effect(
            &receive_only_broker,
            correlated_ack(receive),
            ByteCreditPosition {
                received: 4,
                validated_written_contiguous: 0,
                durable_contiguous: 0,
            },
        )
        .map_err(byte_effect_error)?;
    write_model
        .apply(Event::Validate)
        .map_err(transition_error)?;
    let write = write_model.pending_acknowledgements()[0];
    if !matches!(
        write_model.acknowledge_byte_durability_effect(
            &receive_only_broker,
            correlated_ack(write),
            ByteCreditPosition {
                received: 4,
                validated_written_contiguous: 4,
                durable_contiguous: 0,
            },
        ),
        Err(ByteDurabilityEffectError::Credit(
            CreditError::WrittenAheadOfConsumed
        ))
    ) || receive_only_broker.position().validated_written_contiguous != 0
    {
        return Err(ModelProofError::Contract(
            "WP009-E-DURABILITY-WRITER-AUTHORIZATION".to_owned(),
        ));
    }

    Ok(proof_row(
        case,
        "WP009-REG-DURABLE-STAGE-AUTH-001",
        "wrong_stage_consumption_rejected",
        ResourceVector::default(),
        "owner_1",
        2,
        8,
        4,
        16,
        DurabilityPosition {
            received_bytes: 4,
            validated_bytes: 0,
            durable_bytes: 0,
        },
        0,
        "reject_cross_stage_consumption_as_durability_authority",
        "OwnedByteCreditBroker stage consumption plus StateMachine::acknowledge_byte_durability_effect",
        "Decompression consumption cannot authorize received progress and HttpReceive consumption cannot authorize validated-written progress",
        "ReceivedAheadOfConsumed rejected Decompression-only input; WrittenAheadOfConsumed rejected HttpReceive-only validation",
        "counterfactual",
        "This proves stage-accounting authorization, not adapter execution or storage durability.",
    ))
}

fn correlated_ack(acknowledgement: fforager_core::lifecycle::EffectAcknowledgement) -> Event {
    Event::EffectAcknowledged {
        instance_id: acknowledgement.instance_id,
        effect: acknowledgement.effect,
        generation: acknowledgement.generation,
    }
}

fn wp009_wrong_broker_result() -> Value {
    json!({
        "kind": "error",
        "error_type": "CreditError",
        "variant": "AcknowledgementBrokerMismatch"
    })
}

#[allow(clippy::needless_pass_by_value)] // `Result::map_err` transfers the typed error here.
fn transition_error(error: TransitionError) -> ModelProofError {
    ModelProofError::Contract(format!("WP009-E-TRANSITION:{error:?}"))
}

#[allow(clippy::needless_pass_by_value)] // `Result::map_err` transfers the typed error here.
fn byte_effect_error(error: ByteDurabilityEffectError) -> ModelProofError {
    ModelProofError::Contract(format!("WP009-E-BYTE-EFFECT:{error:?}"))
}

fn durability_prefix(case: &Value) -> Result<Value, ModelProofError> {
    let previous = DurabilityPosition::default();
    let position = DurabilityPosition {
        received_bytes: 8,
        validated_bytes: 6,
        durable_bytes: 4,
    };
    previous.validate_advance(position).map_err(|error| {
        ModelProofError::Contract(format!("WP009-E-DURABILITY-ADVANCE:{error:?}"))
    })?;
    position.validate_resume(4).map_err(|error| {
        ModelProofError::Contract(format!("WP009-E-RESUME-AT-DURABLE:{error:?}"))
    })?;
    if position.validate_resume(5).is_ok() {
        return Err(ModelProofError::Contract(
            "WP009-E-RESUME-AHEAD-ACCEPTED".to_owned(),
        ));
    }
    Ok(proof_row(
        case,
        "WP009-INV-DURABLE-PREFIX-001",
        "received_8_validated_6_durable_4",
        ResourceVector::default(),
        "none",
        0,
        0,
        1,
        8,
        position,
        4,
        "resume_at_or_before_durable_contiguous",
        "fforager_contracts::DurabilityPosition::validate_advance and validate_resume",
        "resume offset four is accepted and offset five is rejected",
        "resume at durable_contiguous passed; durable_contiguous+1 failed closed",
        "semantic",
        "This is numeric contract validation only; owned broker advancement requires a core-issued effect acknowledgement and is not fabricated here.",
    ))
}

fn durability_outrun(case: &Value) -> Result<Value, ModelProofError> {
    let before = DurabilityPosition {
        received_bytes: 8,
        validated_bytes: 0,
        durable_bytes: 0,
    };
    let rejected = before.validate_advance(DurabilityPosition {
        received_bytes: 8,
        validated_bytes: 6,
        durable_bytes: 7,
    });
    if rejected.is_ok() {
        return Err(ModelProofError::Contract(
            "WP009-E-DURABILITY-OUTRUN".to_owned(),
        ));
    }
    Ok(proof_row(
        case,
        "WP009-INV-DURABLE-ORDER-001",
        "optimistic_durable_advance_rejected",
        ResourceVector::default(),
        "none",
        0,
        0,
        1,
        8,
        before,
        0,
        "retain_prior_acknowledged_prefix",
        "fforager_contracts::DurabilityPosition::validate_advance",
        "durable_contiguous seven cannot advance beyond validated_written_contiguous six",
        "DurableAheadOfWritten returned and the prior prefix remained unchanged",
        "counterfactual",
        "This rejects false accounting state but does not itself force storage synchronization.",
    ))
}

fn byte_credit_broker(
    capacity_bytes: u64,
    max_claim_items: u64,
) -> Result<OwnedByteCreditBroker, ModelProofError> {
    OwnedByteCreditBroker::from_contract(&ByteCreditContractV1::new(
        capacity_bytes,
        max_claim_items,
    ))
    .map_err(credit_error)
}

#[allow(clippy::needless_pass_by_value)] // `Result::map_err` transfers the typed error here.
fn credit_error(error: CreditError) -> ModelProofError {
    ModelProofError::Contract(format!("WP009-E-CREDIT:{error:?}"))
}

fn journal_fault(case: &Value) -> Result<Value, ModelProofError> {
    let fault = required_text(case, "injected_fault", "WP009-E-JOURNAL-FAULT")?;
    let expected_job_id = JobId::new("job_wp009")
        .map_err(|error| ModelProofError::Contract(format!("WP009-E-JOURNAL-JOB-ID:{error:?}")))?;
    let first = observed_journal_record(1, None, "hash_1", true, true)?;
    let second = match fault {
        "torn_record" => observed_journal_record(2, Some("hash_1"), "hash_2", false, true)?,
        "duplicate_sequence" => observed_journal_record(1, Some("hash_1"), "hash_2", true, true)?,
        "reordered_sequence" => observed_journal_record(3, Some("hash_1"), "hash_3", true, true)?,
        "checksum_invalid" => observed_journal_record(2, Some("hash_1"), "hash_2", true, false)?,
        _ => {
            return Err(ModelProofError::Contract(
                "WP009-E-JOURNAL-FAULT-UNMAPPED".to_owned(),
            ));
        }
    };
    let scan = scan_observed_journal(&expected_job_id, &[first, second]);
    let correct_fault = matches!(
        (&scan.stopped_by, fault),
        (Some(JournalPrefixFault::Torn { index: 1 }), "torn_record")
            | (
                Some(JournalPrefixFault::DuplicateSequence { index: 1, .. }),
                "duplicate_sequence"
            )
            | (
                Some(JournalPrefixFault::ReorderedSequence { index: 1, .. }),
                "reordered_sequence"
            )
            | (
                Some(JournalPrefixFault::ChecksumInvalid { index: 1 }),
                "checksum_invalid"
            )
    );
    if scan.valid_record_count != 1
        || scan.last_record_hash.as_deref() != Some("hash_1")
        || !correct_fault
    {
        return Err(ModelProofError::Contract(
            "WP009-E-JOURNAL-PRIOR-PREFIX".to_owned(),
        ));
    }
    Ok(proof_row(
        case,
        "WP009-INV-JOURNAL-PRIOR-PREFIX-001",
        "scan_stopped_at_second_record",
        ResourceVector::default(),
        "none",
        0,
        0,
        1,
        1,
        DurabilityPosition::default(),
        0,
        "retain_one_record_prior_valid_prefix",
        "fforager_contracts::scan_observed_journal",
        "the malformed second record is rejected and the first record remains the last valid prefix",
        &format!("valid_record_count=1; stopped_by={:?}", scan.stopped_by),
        "counterfactual",
        "Frame completeness and checksum validity are adapter observations supplied to the pure scanner.",
    ))
}

fn observed_journal_record(
    sequence: u64,
    prior_hash: Option<&str>,
    record_hash: &str,
    frame_complete: bool,
    payload_checksum_valid: bool,
) -> Result<ObservedJournalRecord, ModelProofError> {
    serde_json::from_value(json!({
        "record": {
            "schema": {"major": 1, "minor": 0},
            "job_id": "job_wp009",
            "producer_instance": "producer_wp009",
            "sequence": sequence,
            "prior_record_hash": prior_hash,
            "payload_checksum": "checksum_wp009",
            "durability": "durable",
            "payload": {"kind": "job_created"}
        },
        "record_hash": record_hash,
        "frame_complete": frame_complete,
        "payload_checksum_valid": payload_checksum_valid
    }))
    .map_err(ModelProofError::Json)
}

#[allow(clippy::too_many_lines)]
fn journal_semantic_fault(case: &Value) -> Result<Value, ModelProofError> {
    let id = required_text(case, "case_id", "WP009-E-JOURNAL-SEMANTIC-CASE")?;
    let expected_job_id = JobId::new("job_wp009")
        .map_err(|error| ModelProofError::Contract(format!("WP009-E-JOURNAL-JOB:{error:?}")))?;
    let first = observed_journal_record(1, None, "hash_1", true, true)?;
    let mut second = observed_journal_record(2, Some("hash_1"), "hash_2", true, true)?;
    let (expected_fault, invariant, expected) = match id {
        "wp009-journal-false-prepared-sync" => {
            second.record.payload = JournalPayload::CommitPrepared(CommitPrepared {
                final_rooted_path: "final.bin".to_owned(),
                working_path_identity: "staged_identity".to_owned(),
                artifacts: vec![ArtifactIdentity {
                    identity: "artifact_wp009".to_owned(),
                    size_bytes: 4,
                    checksum: "checksum_wp009".to_owned(),
                }],
                required_sidecars: Vec::new(),
                filesystem_profile_id: "ff-fs-windows-11-26200-ntfs-v1".to_owned(),
                data_synchronized: false,
                parent_directory_synchronized: true,
            });
            (
                JournalPrefixFault::InvalidRecord {
                    index: 1,
                    error: JournalRecordError::PreparedDataNotSynchronized,
                },
                "WP009-INV-JOURNAL-PREPARED-SYNC-001",
                "a prepared transition without acknowledged data synchronization stops at the prior prefix",
            )
        }
        "wp009-journal-false-renamed-sync" => {
            second.record.payload = JournalPayload::CommitRenamed(CommitRenamed {
                final_identity: "final_identity".to_owned(),
                collision_decision: CollisionDecision::CreatedNew,
                directory_synchronized: false,
            });
            (
                JournalPrefixFault::InvalidRecord {
                    index: 1,
                    error: JournalRecordError::RenamedDirectoryNotSynchronized,
                },
                "WP009-INV-JOURNAL-RENAMED-SYNC-001",
                "a renamed transition without acknowledged directory synchronization stops at the prior prefix",
            )
        }
        "wp009-journal-mixed-job" => {
            let foreign = JobId::new("job_foreign").map_err(|error| {
                ModelProofError::Contract(format!("WP009-E-JOURNAL-FOREIGN-JOB:{error:?}"))
            })?;
            second.record.job_id = foreign.clone();
            (
                JournalPrefixFault::JobIdentityMismatch {
                    index: 1,
                    expected_job_id: expected_job_id.clone(),
                    observed_job_id: foreign,
                },
                "WP009-INV-JOURNAL-JOB-IDENTITY-001",
                "a foreign-job record cannot extend the expected job journal prefix",
            )
        }
        "wp009-journal-archive-uniqueness" => {
            let transaction_id = TransactionId::new("transaction_wp009").map_err(|error| {
                ModelProofError::Contract(format!("WP009-E-JOURNAL-TRANSACTION:{error:?}"))
            })?;
            second.record.payload = JournalPayload::ArchiveCommitted(ArchiveCommitted {
                transaction_id,
                archive_row_id: "row_wp009".to_owned(),
                asset_ids: vec![AssetId::new("asset_wp009").map_err(|error| {
                    ModelProofError::Contract(format!("WP009-E-JOURNAL-ASSET:{error:?}"))
                })?],
                derived_output_ids: vec![DerivedOutputId::new("output_wp009").map_err(
                    |error| {
                        ModelProofError::Contract(format!(
                            "WP009-E-JOURNAL-DERIVED-OUTPUT:{error:?}"
                        ))
                    },
                )?],
                output_provenance_digest: "digest_wp009".to_owned(),
                commit_sequence: 2,
                uniqueness: ArchiveUniquenessEvidence {
                    claim_key: "claim_wp009".to_owned(),
                    constraint_receipt: String::new(),
                },
            });
            (
                JournalPrefixFault::InvalidRecord {
                    index: 1,
                    error: JournalRecordError::ArchiveUniquenessReceiptMissing,
                },
                "WP009-INV-JOURNAL-ARCHIVE-UNIQUENESS-001",
                "an archive transition without a uniqueness constraint receipt stops at the prior prefix",
            )
        }
        _ => {
            return Err(ModelProofError::Contract(
                "WP009-E-JOURNAL-SEMANTIC-UNMAPPED".to_owned(),
            ));
        }
    };
    let scan = scan_observed_journal(&expected_job_id, &[first, second]);
    if scan.valid_record_count != 1
        || scan.last_record_hash.as_deref() != Some("hash_1")
        || scan.stopped_by.as_ref() != Some(&expected_fault)
    {
        return Err(ModelProofError::Contract(format!(
            "WP009-E-JOURNAL-SEMANTIC-PREFIX:{id}:expected={expected_fault:?}:observed={scan:?}"
        )));
    }
    let observed = format!("valid_record_count=1; stopped_by={:?}", scan.stopped_by);
    Ok(proof_row(
        case,
        invariant,
        "semantic_fault_stopped_at_second_record",
        ResourceVector::default(),
        "none",
        0,
        0,
        1,
        1,
        DurabilityPosition::default(),
        0,
        "retain_one_record_prior_valid_prefix",
        "fforager_contracts::scan_observed_journal semantic and job identity validation",
        expected,
        &observed,
        "counterfactual",
        "Frame and checksum observations remain adapter-supplied; this row proves semantic fail-closed scanning.",
    ))
}

#[allow(clippy::too_many_lines)]
fn commit_effect_case(case: &Value) -> Result<Value, ModelProofError> {
    let id = required_text(case, "case_id", "WP009-E-COMMIT-EFFECT-CASE")?;
    let instance = MachineInstanceId::new(9010)
        .ok_or_else(|| ModelProofError::Contract("WP009-E-MACHINE-ID".to_owned()))?;
    let (mut model, request, target, effects, invariant, state) = match id {
        "wp009-commit-prepared-effects" => (
            StateMachine::new(MachineKind::CommitArchiveReconciliation, instance, 16),
            Event::Prepare,
            State::CommitPrepared,
            vec![
                EffectIntent::ValidateOutput,
                EffectIntent::SynchronizeData,
                EffectIntent::SynchronizeDirectory,
                EffectIntent::AppendTransitionRecord,
                EffectIntent::SynchronizeJournal,
            ],
            "WP009-INV-COMMIT-PREPARED-EFFECTS-001",
            "prepared_after_five_serial_receipts",
        ),
        "wp009-commit-renamed-effects" => (
            StateMachine::from_state(
                MachineKind::CommitArchiveReconciliation,
                State::CommitPrepared,
                instance,
                16,
                1,
            )
            .map_err(transition_error)?,
            Event::Rename,
            State::CommitRenamed,
            vec![
                EffectIntent::RenameOutput,
                EffectIntent::SynchronizeDirectory,
                EffectIntent::AppendTransitionRecord,
                EffectIntent::SynchronizeJournal,
            ],
            "WP009-INV-COMMIT-RENAMED-EFFECTS-001",
            "renamed_after_four_serial_receipts",
        ),
        "wp009-commit-archived-effects" => (
            StateMachine::from_state(
                MachineKind::CommitArchiveReconciliation,
                State::CommitRenamed,
                instance,
                16,
                1,
            )
            .map_err(transition_error)?,
            Event::Archive,
            State::CommitArchived,
            vec![
                EffectIntent::InsertArchiveRow,
                EffectIntent::AppendTransitionRecord,
                EffectIntent::SynchronizeJournal,
            ],
            "WP009-INV-COMMIT-ARCHIVED-EFFECTS-001",
            "archived_after_three_serial_receipts",
        ),
        "wp009-commit-cleaned-effects" => (
            StateMachine::from_state(
                MachineKind::CommitArchiveReconciliation,
                State::CommitArchived,
                instance,
                16,
                1,
            )
            .map_err(transition_error)?,
            Event::Cleanup,
            State::CommitCleaned,
            vec![
                EffectIntent::RemoveTemporaryState,
                EffectIntent::AppendTransitionRecord,
                EffectIntent::SynchronizeJournal,
            ],
            "WP009-INV-COMMIT-CLEANED-EFFECTS-001",
            "cleaned_after_three_serial_receipts",
        ),
        _ => {
            return Err(ModelProofError::Contract(
                "WP009-E-COMMIT-EFFECT-UNMAPPED".to_owned(),
            ));
        }
    };
    model.apply(request).map_err(transition_error)?;
    for expected_effect in &effects {
        let pending = model.pending_acknowledgements();
        if pending.len() != 1 || pending[0].effect != *expected_effect || model.state() == target {
            return Err(ModelProofError::Contract(format!(
                "WP009-E-COMMIT-EFFECT-ORDER:{id}:{expected_effect:?}"
            )));
        }
        model
            .apply(correlated_ack(pending[0]))
            .map_err(transition_error)?;
    }
    if model.state() != target || !model.pending_acknowledgements().is_empty() {
        return Err(ModelProofError::Contract(
            "WP009-E-COMMIT-DURABLE-PREFIX-EARLY".to_owned(),
        ));
    }

    if id == "wp009-commit-renamed-effects" {
        let retry_instance = MachineInstanceId::new(9011)
            .ok_or_else(|| ModelProofError::Contract("WP009-E-MACHINE-ID".to_owned()))?;
        let mut retry = StateMachine::from_state(
            MachineKind::CommitArchiveReconciliation,
            State::CommitPrepared,
            retry_instance,
            16,
            1,
        )
        .map_err(transition_error)?;
        retry.apply(Event::Rename).map_err(transition_error)?;
        let rename = retry.pending_acknowledgements()[0];
        retry
            .apply(correlated_ack(rename))
            .map_err(transition_error)?;
        let directory_sync = retry.pending_acknowledgements()[0];
        retry
            .apply(Event::EffectFailed {
                instance_id: directory_sync.instance_id,
                effect: directory_sync.effect,
                generation: directory_sync.generation,
            })
            .map_err(transition_error)?;
        let restart = retry.apply(Event::Restart).map_err(transition_error)?;
        if restart.effects != [EffectIntent::SynchronizeDirectory]
            || restart.effects.contains(&EffectIntent::RenameOutput)
        {
            return Err(ModelProofError::Contract(
                "WP009-E-COMMIT-RENAME-PARTIAL-RETRY".to_owned(),
            ));
        }
    }

    let observed = format!("ordered_effects={effects:?}; durable_state={target:?}");
    Ok(proof_row(
        case,
        invariant,
        state,
        ResourceVector::default(),
        "none",
        0,
        0,
        1,
        u64::try_from(effects.len()).unwrap_or(u64::MAX),
        DurabilityPosition::default(),
        0,
        "acknowledge_each_effect_in_declared_order",
        "fforager_core::lifecycle::StateMachine correlated commit effect boundary",
        "the durable prefix remains unreachable until every ordered effect including journal append and sync is acknowledged",
        &observed,
        "public_boundary",
        "Effect intents are pure adapter requests; this model does not perform filesystem or database I/O.",
    ))
}

#[allow(clippy::too_many_lines)]
fn recovery_case(case: &Value) -> Result<Value, ModelProofError> {
    let id = required_text(case, "case_id", "WP009-E-RECOVERY-CASE")?;
    let mut observation = RecoveryObservation {
        durable_prefix: CommitState::Collecting,
        staged_output: IdentityObservation::Missing,
        final_output: IdentityObservation::Missing,
        archive_row: IdentityObservation::Missing,
        lease: LeaseObservation::NotPresent,
        cleanup: CleanupObservation::NotStarted,
        migration: MigrationObservation::NotRequired,
        volume_relationship: VolumeRelationship::SameFilesystem,
        collision: CollisionObservation::None,
        confinement: RecoveryConfinementObservation::Proven,
    };
    let expected = match id {
        "wp009-recovery-prepared" => {
            observation.durable_prefix = CommitState::Prepared;
            observation.staged_output = IdentityObservation::MatchesJournal;
            RecoveryDecision::Act(fforager_contracts::RecoveryAction::RevalidatePreparedThenRename)
        }
        "wp009-recovery-renamed" => {
            observation.durable_prefix = CommitState::Renamed;
            observation.final_output = IdentityObservation::MatchesJournal;
            RecoveryDecision::Act(fforager_contracts::RecoveryAction::InsertArchiveRow)
        }
        "wp009-recovery-archived" => {
            observation.durable_prefix = CommitState::Archived;
            observation.final_output = IdentityObservation::MatchesJournal;
            observation.archive_row = IdentityObservation::MatchesJournal;
            RecoveryDecision::Act(fforager_contracts::RecoveryAction::RepeatCleanup)
        }
        "wp009-recovery-cleaned" => {
            observation.durable_prefix = CommitState::Cleaned;
            observation.final_output = IdentityObservation::MatchesJournal;
            observation.archive_row = IdentityObservation::MatchesJournal;
            observation.cleanup = CleanupObservation::Complete;
            RecoveryDecision::ReconciledSuccess
        }
        "wp009-recovery-stale-lease" => {
            observation.durable_prefix = CommitState::Prepared;
            observation.staged_output = IdentityObservation::MatchesJournal;
            observation.lease = LeaseObservation::Stale;
            RecoveryDecision::Act(fforager_contracts::RecoveryAction::ReclaimStaleLease)
        }
        "wp009-recovery-collision" => {
            observation.durable_prefix = CommitState::Prepared;
            observation.staged_output = IdentityObservation::MatchesJournal;
            observation.collision = CollisionObservation::ConflictingExistingOutput;
            RecoveryDecision::FailClosed(
                fforager_contracts::RecoveryFailure::ConflictingDestination,
            )
        }
        "wp009-recovery-partial-cleanup" => {
            observation.durable_prefix = CommitState::Archived;
            observation.final_output = IdentityObservation::MatchesJournal;
            observation.archive_row = IdentityObservation::MatchesJournal;
            observation.cleanup = CleanupObservation::Partial;
            RecoveryDecision::Act(fforager_contracts::RecoveryAction::RepeatCleanup)
        }
        "wp009-recovery-interrupted-migration" => {
            observation.durable_prefix = CommitState::Prepared;
            observation.staged_output = IdentityObservation::MatchesJournal;
            observation.migration = MigrationObservation::Interrupted;
            RecoveryDecision::Act(RecoveryAction::ResumeInterruptedMigration)
        }
        "wp009-recovery-collecting-artifact" => {
            observation.archive_row = IdentityObservation::MatchesJournal;
            RecoveryDecision::FailClosed(RecoveryFailure::UnexpectedArtifactBeforePrepared)
        }
        "wp009-recovery-stale-mismatch-precedence" => {
            observation.durable_prefix = CommitState::Prepared;
            observation.staged_output = IdentityObservation::Mismatched;
            observation.lease = LeaseObservation::Stale;
            RecoveryDecision::FailClosed(RecoveryFailure::MismatchedStagedOutput)
        }
        "wp009-recovery-migration-collision-precedence" => {
            observation.durable_prefix = CommitState::Prepared;
            observation.staged_output = IdentityObservation::MatchesJournal;
            observation.migration = MigrationObservation::Interrupted;
            observation.collision = CollisionObservation::ConflictingExistingOutput;
            RecoveryDecision::FailClosed(RecoveryFailure::ConflictingDestination)
        }
        "wp009-recovery-confinement-unavailable" => {
            observation.durable_prefix = CommitState::Prepared;
            observation.staged_output = IdentityObservation::MatchesJournal;
            observation.lease = LeaseObservation::Stale;
            observation.confinement = RecoveryConfinementObservation::Unavailable;
            RecoveryDecision::FailClosed(RecoveryFailure::ConfinementUnavailable)
        }
        "wp009-recovery-confinement-mismatched" => {
            observation.durable_prefix = CommitState::Prepared;
            observation.staged_output = IdentityObservation::MatchesJournal;
            observation.confinement = RecoveryConfinementObservation::Mismatched;
            RecoveryDecision::FailClosed(RecoveryFailure::ConfinementMismatched)
        }
        "wp009-cross-volume" => {
            observation.durable_prefix = CommitState::Prepared;
            observation.staged_output = IdentityObservation::MatchesJournal;
            observation.volume_relationship = VolumeRelationship::CrossVolume;
            RecoveryDecision::Act(
                fforager_contracts::RecoveryAction::CopySyncRenameWithinDestination,
            )
        }
        _ => {
            return Err(ModelProofError::Contract(
                "WP009-E-RECOVERY-UNMAPPED".to_owned(),
            ));
        }
    };
    let first = decide_recovery(&observation);
    let second = decide_recovery(&observation);
    if first != expected || second != first {
        return Err(ModelProofError::Contract(
            "WP009-E-RECOVERY-NON-IDEMPOTENT".to_owned(),
        ));
    }
    if let RecoveryDecision::Act(action) = first {
        let application = apply_recovery_action(&observation, action).map_err(|error| {
            ModelProofError::Contract(format!("WP009-E-RECOVERY-APPLY:{error:?}"))
        })?;
        if application.action() != action || application.prior_observation() != &observation {
            return Err(ModelProofError::Contract(
                "WP009-E-RECOVERY-APPLICATION-LINEAGE".to_owned(),
            ));
        }
        let after_once = application.resulting_observation();
        let after_twice = application.retry(action).map_err(|error| {
            ModelProofError::Contract(format!("WP009-E-RECOVERY-RETRY:{error:?}"))
        })?;
        if after_twice != after_once {
            return Err(ModelProofError::Contract(
                "WP009-E-RECOVERY-ACTION-NON-IDEMPOTENT".to_owned(),
            ));
        }
        let mismatched_action = if action == RecoveryAction::RepeatCleanup {
            RecoveryAction::ReclaimStaleLease
        } else {
            RecoveryAction::RepeatCleanup
        };
        if !matches!(
            application.retry(mismatched_action),
            Err(RecoveryApplicationError::RetryActionMismatch { applied, requested })
                if applied == action && requested == mismatched_action
        ) {
            return Err(ModelProofError::Contract(
                "WP009-E-RECOVERY-RETRY-LINEAGE".to_owned(),
            ));
        }
        if action != RecoveryAction::ResumeOrRestartVerifiedData
            && apply_recovery_action(after_once, action).is_ok()
        {
            return Err(ModelProofError::Contract(
                "WP009-E-RECOVERY-REAPPLY-BYPASS".to_owned(),
            ));
        }
        let next = decide_recovery(after_once);
        if action != RecoveryAction::ResumeOrRestartVerifiedData && next == first {
            return Err(ModelProofError::Contract(
                "WP009-E-RECOVERY-NONCONVERGENT".to_owned(),
            ));
        }
    }
    let prefix = format!("{:?}", observation.durable_prefix).to_ascii_lowercase();
    let decision = format!("{first:?}");
    let proof_class = if matches!(first, RecoveryDecision::FailClosed(_)) {
        "counterfactual"
    } else {
        "semantic"
    };
    Ok(proof_row(
        case,
        "WP009-INV-RECOVERY-IDEMPOTENT-001",
        &prefix,
        ResourceVector::default(),
        "none",
        0,
        0,
        1,
        1,
        DurabilityPosition::default(),
        0,
        &decision,
        "fforager_core::lifecycle::decide_recovery then apply_recovery_action, RecoveryApplication::retry, and decide_recovery",
        &decision,
        &decision,
        proof_class,
        "One oracle-selected action is applied to the pure acknowledged-observation model; exact-action retry is checked through its opaque application lineage. Later adapters must still execute real I/O and supply the observations.",
    ))
}

fn filesystem_case(case: &Value) -> Result<Value, ModelProofError> {
    let id = required_text(case, "case_id", "WP009-E-FILESYSTEM-CASE")?;
    let (profile, expected, observed, proof_class, state, residual) = match id {
        "wp009-filesystem-windows-ntfs" => {
            let profile = FilesystemProfileContract::windows_11_26200_ntfs_v1();
            profile.validate_exact().map_err(|error| {
                ModelProofError::Contract(format!("WP009-E-FS-WINDOWS-EXACT:{error:?}"))
            })?;
            profile.validate_secure_write_profile().map_err(|error| {
                ModelProofError::Contract(format!("WP009-E-FS-WINDOWS-CONFINEMENT:{error:?}"))
            })?;
            (
                profile,
                "exact Windows 11 26200 NTFS profile accepts secure-write model confinement",
                "exact profile and handle-relative confinement contract validated",
                "semantic",
                "supported_local_positive_model",
                WP009_WINDOWS_PLATFORM_ROW_RESIDUAL,
            )
        }
        "wp009-filesystem-wsl-v9fs" => {
            let profile = FilesystemProfileContract::ubuntu_24_04_wsl2_v9fs_v1();
            profile.validate_exact().map_err(|error| {
                ModelProofError::Contract(format!("WP009-E-FS-WSL-EXACT:{error:?}"))
            })?;
            if profile.validate_secure_write_profile().is_ok()
                || profile.native_linux_durability_proven
            {
                return Err(ModelProofError::Contract(
                    "WP009-E-FS-WSL-OVERCLAIM".to_owned(),
                ));
            }
            (
                profile,
                "WSL2 v9fs is exact but fails closed for security-sensitive confinement and native Linux durability",
                "exact v9fs interop profile validated; secure-write confinement rejected; native Linux proof=false",
                "semantic",
                "unix_interop_rejected_or_explicitly_degraded",
                WP009_WSL_PLATFORM_ROW_RESIDUAL,
            )
        }
        _ => {
            return Err(ModelProofError::Contract(
                "WP009-E-FILESYSTEM-UNMAPPED".to_owned(),
            ));
        }
    };
    Ok(proof_row(
        case,
        "WP009-INV-FILESYSTEM-PROFILE-001",
        state,
        ResourceVector::default(),
        "none",
        0,
        0,
        1,
        1,
        DurabilityPosition::default(),
        0,
        &format!("profile={}", profile.capability.profile_id),
        "fforager_contracts::FilesystemProfileContract exact and secure-write validators",
        expected,
        observed,
        proof_class,
        residual,
    ))
}

/// Strictly validates a produced WP-009 report against the exact manifest.
///
/// # Errors
///
/// Returns [`ModelProofError`] when rows are missing/unmapped, proof fields or
/// queue bounds are absent, durability outruns its prerequisites, recovery is
/// not recorded as deterministic, or the zero-progress ceiling is removed.
pub fn validate_resource_durability_report(
    report: &Value,
    manifest: &Value,
) -> Result<(), ModelProofError> {
    require_exact_keys(
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
        || report.get("manifest_path").and_then(Value::as_str) != Some(RESOURCE_DURABILITY_MANIFEST)
    {
        return Err(ModelProofError::Contract(
            "WP009-E-REPORT-IDENTITY".to_owned(),
        ));
    }
    validate_resource_durability_manifest(manifest)?;
    let manifest_ids = manifest["cases"]
        .as_array()
        .ok_or_else(|| ModelProofError::Contract("WP009-E-CASES-MISSING".to_owned()))?
        .iter()
        .map(|case| required_text(case, "case_id", "WP009-E-CASE-ID"))
        .collect::<Result<BTreeSet<_>, _>>()?;
    let rows = report
        .get("rows")
        .and_then(Value::as_array)
        .ok_or_else(|| ModelProofError::Contract("WP009-E-ROWS-MISSING".to_owned()))?;
    let mut row_ids = BTreeSet::new();
    for row in rows {
        validate_resource_durability_row(row)?;
        let id = required_text(row, "case_id", "WP009-E-ROW-ID")?;
        if !row_ids.insert(id) {
            return Err(ModelProofError::Contract(
                "WP009-E-DUPLICATE-ROW".to_owned(),
            ));
        }
    }
    if row_ids != manifest_ids {
        return Err(ModelProofError::Contract(
            "WP009-E-UNMAPPED-INPUT".to_owned(),
        ));
    }
    let summary = report
        .get("summary")
        .and_then(Value::as_object)
        .ok_or_else(|| ModelProofError::Contract("WP009-E-SUMMARY".to_owned()))?;
    let summary_value = Value::Object(summary.clone());
    require_exact_keys(
        &summary_value,
        &[
            "executed_cases",
            "failed_cases",
            "proof_classes",
            "proof_mechanisms",
            "zero_product_progress",
        ],
        "WP009-E-SUMMARY",
    )?;
    let proof_classes = string_values(&summary_value, "proof_classes")?;
    let proof_mechanisms = string_values(&summary_value, "proof_mechanisms")?
        .into_iter()
        .collect::<BTreeSet<_>>();
    if summary.get("executed_cases").and_then(Value::as_u64) != u64::try_from(rows.len()).ok()
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
        return Err(ModelProofError::Contract(
            "WP009-E-SUMMARY-CEILING".to_owned(),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn validate_resource_durability_row(row: &Value) -> Result<(), ModelProofError> {
    require_exact_keys(
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
        let _text = required_text(row, key, "WP009-E-PROOF-FIELDS-MISSING")?;
    }
    if row.get("proof_class").and_then(Value::as_str) != Some("semantic") {
        return Err(ModelProofError::Contract(
            "WP009-E-NONCANONICAL-PROOF-CLASS".to_owned(),
        ));
    }
    if !matches!(
        row.get("proof_mechanism").and_then(Value::as_str),
        Some("direct_behavior" | "negative_counterfactual" | "public_boundary")
    ) {
        return Err(ModelProofError::Contract(
            "WP009-E-PROOF-MECHANISM".to_owned(),
        ));
    }
    require_exact_keys(
        &row["queue_occupancy"],
        &["items", "bytes", "item_bound", "byte_bound"],
        "WP009-E-QUEUE-BOUNDS-MISSING",
    )?;
    let queue = &row["queue_occupancy"];
    let items = queue.get("items").and_then(Value::as_u64);
    let bytes = queue.get("bytes").and_then(Value::as_u64);
    let item_bound = queue.get("item_bound").and_then(Value::as_u64);
    let byte_bound = queue.get("byte_bound").and_then(Value::as_u64);
    if items.is_none()
        || bytes.is_none()
        || item_bound.is_none_or(|bound| bound == 0)
        || byte_bound.is_none_or(|bound| bound == 0)
        || items
            .zip(item_bound)
            .is_some_and(|(value, bound)| value > bound)
        || bytes
            .zip(byte_bound)
            .is_some_and(|(value, bound)| value > bound)
    {
        return Err(ModelProofError::Contract(
            "WP009-E-QUEUE-BOUNDS-MISSING".to_owned(),
        ));
    }
    require_exact_keys(
        &row["durability_prefix"],
        &[
            "received",
            "validated_written_contiguous",
            "durable_contiguous",
            "resume_at",
        ],
        "WP009-E-DURABILITY-FIELDS-MISSING",
    )?;
    let durability = &row["durability_prefix"];
    let received = durability.get("received").and_then(Value::as_u64);
    let validated = durability
        .get("validated_written_contiguous")
        .and_then(Value::as_u64);
    let durable = durability.get("durable_contiguous").and_then(Value::as_u64);
    let resume = durability.get("resume_at").and_then(Value::as_u64);
    if received.is_none()
        || validated.is_none()
        || durable.is_none()
        || resume.is_none()
        || validated
            .zip(received)
            .is_some_and(|(value, limit)| value > limit)
        || durable
            .zip(validated)
            .is_some_and(|(value, limit)| value > limit)
        || resume
            .zip(durable)
            .is_some_and(|(value, limit)| value > limit)
    {
        return Err(ModelProofError::Contract(
            "WP009-E-DURABILITY-OUTRUN".to_owned(),
        ));
    }
    let case_id = required_text(row, "case_id", "WP009-E-ROW-ID")?;
    validate_wp009_credit_attribution(row, case_id)?;
    validate_wp009_supplemental_execution(row, case_id)?;
    if (case_id.starts_with("wp009-recovery-") || case_id == "wp009-cross-volume")
        && (row.get("expected") != row.get("observed")
            || row.get("recovery_action") != row.get("observed"))
    {
        return Err(ModelProofError::Contract(
            "WP009-E-RECOVERY-NON-IDEMPOTENT".to_owned(),
        ));
    }
    validate_wp009_platform_row(row, case_id)?;
    Ok(())
}

fn validate_wp009_credit_attribution(row: &Value, case_id: &str) -> Result<(), ModelProofError> {
    let attribution = row
        .get("credit_attribution")
        .ok_or_else(|| ModelProofError::Contract("WP009-E-CREDIT-CANCEL-ATTRIBUTION".to_owned()))?;
    if case_id != "wp009-credit-owned-cancel" {
        if !attribution.is_null() {
            return Err(ModelProofError::Contract(
                "WP009-E-CREDIT-ATTRIBUTION-UNMAPPED".to_owned(),
            ));
        }
        return Ok(());
    }
    require_exact_keys(
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
        return Err(ModelProofError::Contract(
            "WP009-E-CREDIT-CANCEL-ATTRIBUTION".to_owned(),
        ));
    }
    Ok(())
}

fn validate_wp009_supplemental_execution(
    row: &Value,
    case_id: &str,
) -> Result<(), ModelProofError> {
    let supplemental = row
        .get("supplemental_execution")
        .ok_or_else(|| ModelProofError::Contract("WP009-E-SUPPLEMENTAL-EXECUTION".to_owned()))?;
    if case_id != "wp009-durability-effect-ack" {
        if !supplemental.is_null() {
            return Err(ModelProofError::Contract(
                "WP009-E-SUPPLEMENTAL-EXECUTION-UNMAPPED".to_owned(),
            ));
        }
        return Ok(());
    }
    if supplemental.is_null() {
        return Err(ModelProofError::Contract(
            "WP009-E-SUPPLEMENTAL-EXECUTION".to_owned(),
        ));
    }
    require_exact_keys(
        supplemental,
        &["boundary", "expected", "observed", "proof_class"],
        "WP009-E-SUPPLEMENTAL-EXECUTION",
    )?;
    if supplemental.get("boundary").and_then(Value::as_str)
        != Some(
            "StateMachine::acknowledge_byte_durability_effect with foreign OwnedByteCreditBroker",
        )
        || supplemental.get("expected") != Some(&wp009_wrong_broker_result())
        || supplemental.get("observed") != Some(&wp009_wrong_broker_result())
        || supplemental.get("proof_class").and_then(Value::as_str) != Some("semantic")
    {
        return Err(ModelProofError::Contract(
            "WP009-E-SUPPLEMENTAL-EXECUTION".to_owned(),
        ));
    }
    Ok(())
}

fn validate_wp009_platform_row(row: &Value, case_id: &str) -> Result<(), ModelProofError> {
    let observation = row.get("platform_observation").ok_or_else(|| {
        ModelProofError::Contract("WP009-E-PLATFORM-OBSERVATION-MISSING".to_owned())
    })?;
    require_exact_keys(
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
        return Err(ModelProofError::Contract(
            "WP009-E-PLATFORM-OBSERVATION-IDENTITY".to_owned(),
        ));
    }
    match case_id {
        "wp009-filesystem-windows-ntfs" => {
            if observation.get("mode").and_then(Value::as_str)
                != Some("live_windows_host_and_repository_volume")
                || observation.get("verdict").and_then(Value::as_str)
                    != Some("matched_frozen_windows_11_26200_ntfs")
                || observation
                    .pointer("/observed_fields/derived_product")
                    .and_then(Value::as_str)
                    != Some("Windows 11 Home")
                || observation
                    .pointer("/observed_fields/build_number")
                    .and_then(Value::as_str)
                    != Some("26200")
                || observation
                    .pointer("/observed_fields/repository_filesystem")
                    .and_then(Value::as_str)
                    != Some("NTFS")
            {
                return Err(ModelProofError::Contract(
                    "WP009-E-WINDOWS-PROFILE-MISMATCH".to_owned(),
                ));
            }
            if row.get("residual_uncertainty").and_then(Value::as_str)
                != Some(WP009_WINDOWS_PLATFORM_ROW_RESIDUAL)
            {
                return Err(ModelProofError::Contract(
                    "WP009-E-WINDOWS-ROW-RESIDUAL".to_owned(),
                ));
            }
            validate_wp009_platform_commands(observation, 4)?;
        }
        "wp009-filesystem-wsl-v9fs" => {
            if observation.get("mode").and_then(Value::as_str)
                != Some("live_wsl_read_only_identity_and_mount")
                || observation.get("verdict").and_then(Value::as_str)
                    != Some("matched_frozen_wsl2_v9fs_rejected_degraded")
                || observation
                    .pointer("/observed_fields/distribution_id")
                    .and_then(Value::as_str)
                    != Some("ubuntu")
                || observation
                    .pointer("/observed_fields/distribution_version")
                    .and_then(Value::as_str)
                    != Some("24.04")
                || !observation
                    .pointer("/observed_fields/kernel_release")
                    .and_then(Value::as_str)
                    .is_some_and(|value| value.starts_with("6.6.87.2-"))
                || observation
                    .pointer("/observed_fields/repository_mount_filesystem")
                    .and_then(Value::as_str)
                    != Some("v9fs")
            {
                return Err(ModelProofError::Contract(
                    "WP009-E-WSL-PROFILE-MISMATCH".to_owned(),
                ));
            }
            if row.get("residual_uncertainty").and_then(Value::as_str)
                != Some(WP009_WSL_PLATFORM_ROW_RESIDUAL)
            {
                return Err(ModelProofError::Contract(
                    "WP009-E-WSL-ROW-RESIDUAL".to_owned(),
                ));
            }
            validate_wp009_platform_commands(observation, 4)?;
        }
        _ if observation != &platform_not_applicable() => {
            return Err(ModelProofError::Contract(
                "WP009-E-PLATFORM-OBSERVATION-UNMAPPED".to_owned(),
            ));
        }
        _ => {}
    }
    Ok(())
}

fn validate_wp009_platform_commands(
    observation: &Value,
    expected_count: usize,
) -> Result<(), ModelProofError> {
    let commands = observation
        .get("commands")
        .and_then(Value::as_array)
        .filter(|commands| commands.len() == expected_count)
        .ok_or_else(|| ModelProofError::Contract("WP009-E-PLATFORM-COMMANDS".to_owned()))?;
    for command in commands {
        require_exact_keys(command, &["program", "args"], "WP009-E-PLATFORM-COMMANDS")?;
        let _program = required_text(command, "program", "WP009-E-PLATFORM-COMMANDS")?;
        if command
            .get("args")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
        {
            return Err(ModelProofError::Contract(
                "WP009-E-PLATFORM-COMMANDS".to_owned(),
            ));
        }
    }
    if observation
        .get("residual_uncertainty")
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty)
    {
        return Err(ModelProofError::Contract(
            "WP009-E-PLATFORM-RESIDUAL".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fforager_contracts::{
        AcquisitionSource, ArchiveCandidate, ArchiveCommitted, AssetId, BackpressureMode,
        CancellationAcknowledgement, CancellationRequest, CommitPrepared, CommitRenamed,
        CompatibilityRange, ConfigEnvelope, DerivedOutputId, DurabilityPosition, EdgeId, EdgeKind,
        ErrorEnvelope, EventEnvelope, ExtensionLimits, FilesystemCapability, FrameDecoder,
        FrameError, FrameLimits, GraphError, GraphLimits, ItemId, JavaScriptWorkerEnvelope,
        JournalPayload, JournalRecord, OutputSinkSpec, PluginEnvelope, ProcessEnvelope,
        ProtocolLimits, ReconcileState, RepresentationId, SchemaVersion, SinkSemantics, SourceEdge,
        SourceGraph, TrackId,
    };
    use fforager_core::lifecycle::{
        EffectAcknowledgement, EffectIntent, Event, MachineInstanceId, MachineKind, State,
        StateMachine, TransitionError, durable_states,
    };
    use fforager_core::resource::{CreditError, OwnerId, ResourceVector};
    use fforager_diagnostics_contract as diagnostics;
    use std::collections::BTreeSet;

    const CANONICAL_INVENTORY_FNV1A64: u64 = 0x8d40_e7b5_6085_fe1e;

    #[test]
    fn archive_store_evidence_corpus_executes_public_boundary() {
        let report = run_archive_store_evidence_corpus()
            .expect("WP-008 archive corpus must execute public boundaries");
        let manifest_bytes = fs::read(repository_root().join(ARCHIVE_STORE_EVIDENCE_MANIFEST))
            .expect("WP-008 manifest must be readable");
        let manifest: Value =
            serde_json::from_slice(&manifest_bytes).expect("WP-008 manifest must be JSON");
        validate_archive_store_evidence_report(&report, &manifest)
            .expect("fresh WP-008 report must validate");

        archive_evidence::verify_behavior_removal_mutations_fail()
            .expect("behavior-removal mutations must be caught by real proof oracles");

        let mut declaration_only = report.clone();
        declaration_only["rows"][0]["actions"]
            .as_array_mut()
            .expect("actions must be an array")
            .pop();
        assert!(
            validate_archive_store_evidence_report(&declaration_only, &manifest).is_err(),
            "removing executed behavior while preserving the PASS declaration must fail",
        );

        let mut forged_receipt = report.clone();
        forged_receipt["rows"][0]["actions"][0]["observed"] = json!("forged outcome");
        assert!(
            validate_archive_store_evidence_report(&forged_receipt, &manifest).is_err(),
            "a changed observation with an unchanged SHA-256 receipt must fail",
        );

        if report["rows"]
            .as_array()
            .and_then(|rows| {
                rows.iter().position(|row| {
                    row.get("case_id").and_then(Value::as_str) == Some("wp008-scale-b021")
                        && row.get("status").and_then(Value::as_str) == Some("BLOCKED")
                })
            })
            .is_some()
        {
            let mut false_scale_pass = report.clone();
            let row = false_scale_pass["rows"]
                .as_array_mut()
                .and_then(|rows| {
                    rows.iter_mut().find(|row| {
                        row.get("case_id").and_then(Value::as_str) == Some("wp008-scale-b021")
                    })
                })
                .expect("B-021 row must exist");
            row["status"] = json!("PASS");
            row["proof_class"] = json!("semantic");
            let measurement = false_scale_pass["measurements"]
                .as_array_mut()
                .and_then(|measurements| {
                    measurements.iter_mut().find(|measurement| {
                        measurement.get("profile").and_then(Value::as_str) == Some("b021")
                    })
                })
                .expect("B-021 measurement must exist");
            measurement["status"] = json!("PASS");
            measurement["elapsed_ns"] = json!(0);
            measurement["storage_size_bytes"] = json!(0);
            measurement["cache_state"] = json!("fabricated");
            false_scale_pass["summary"]["blocked_cases"] = json!(0);
            false_scale_pass["summary"]["semantic_pass_cases"] =
                false_scale_pass["summary"]["executed_cases"].clone();
            assert!(
                validate_archive_store_evidence_report(&false_scale_pass, &manifest).is_err(),
                "zero measurement fields must not be promoted from BLOCKED to B-021 PASS",
            );
        }

        println!(
            "{ARCHIVE_STORE_EVIDENCE_REPORT_PREFIX}{}",
            serde_json::to_string(&report).expect("WP-008 report must serialize")
        );
    }

    #[test]
    fn resource_durability_model_corpus_executes_public_boundaries() {
        let report = run_resource_durability_model_corpus()
            .expect("canonical WP-009 model corpus must execute");
        let encoded = serde_json::to_string(&report).expect("WP-009 report must serialize");
        println!("{RESOURCE_DURABILITY_REPORT_PREFIX}{encoded}");
    }

    #[test]
    fn resource_durability_report_rejects_unmapped_inputs_and_missing_proof_fields() {
        let manifest: Value = serde_json::from_slice(
            &fs::read(repository_root().join(RESOURCE_DURABILITY_MANIFEST))
                .expect("canonical WP-009 manifest must load"),
        )
        .expect("canonical WP-009 manifest must parse");
        let report = run_resource_durability_model_corpus()
            .expect("canonical WP-009 model corpus must execute");

        let mut unmapped = report.clone();
        let rows = unmapped["rows"]
            .as_array_mut()
            .expect("rows must be an array");
        let _removed = rows.pop();
        assert!(matches!(
            validate_resource_durability_report(&unmapped, &manifest),
            Err(ModelProofError::Contract(ref diagnostic))
                if diagnostic == "WP009-E-UNMAPPED-INPUT"
        ));

        let mut missing = report;
        let row = missing["rows"][0]
            .as_object_mut()
            .expect("row must be an object");
        let _removed = row.remove("proof_class");
        assert!(matches!(
            validate_resource_durability_report(&missing, &manifest),
            Err(ModelProofError::Contract(ref diagnostic))
                if diagnostic == "WP009-E-PROOF-FIELDS-MISSING"
        ));
    }

    #[test]
    fn resource_durability_report_rejects_missing_queue_bounds_and_durability_outrun() {
        let manifest: Value = serde_json::from_slice(
            &fs::read(repository_root().join(RESOURCE_DURABILITY_MANIFEST))
                .expect("canonical WP-009 manifest must load"),
        )
        .expect("canonical WP-009 manifest must parse");
        let report = run_resource_durability_model_corpus()
            .expect("canonical WP-009 model corpus must execute");

        let mut missing_bound = report.clone();
        let occupancy = missing_bound["rows"][0]["queue_occupancy"]
            .as_object_mut()
            .expect("queue occupancy must be an object");
        let _removed = occupancy.remove("byte_bound");
        assert!(matches!(
            validate_resource_durability_report(&missing_bound, &manifest),
            Err(ModelProofError::Contract(ref diagnostic))
                if diagnostic == "WP009-E-QUEUE-BOUNDS-MISSING"
        ));

        let mut outrun = report;
        outrun["rows"][0]["durability_prefix"]["durable_contiguous"] = json!(2);
        outrun["rows"][0]["durability_prefix"]["validated_written_contiguous"] = json!(1);
        assert!(matches!(
            validate_resource_durability_report(&outrun, &manifest),
            Err(ModelProofError::Contract(ref diagnostic))
                if diagnostic == "WP009-E-DURABILITY-OUTRUN"
        ));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn resource_durability_report_rejects_proof_attribution_and_platform_overclaims() {
        let manifest: Value = serde_json::from_slice(
            &fs::read(repository_root().join(RESOURCE_DURABILITY_MANIFEST))
                .expect("canonical WP-009 manifest must load"),
        )
        .expect("canonical WP-009 manifest must parse");
        let report = run_resource_durability_model_corpus()
            .expect("canonical WP-009 model corpus must execute");

        let mut noncanonical_class = report.clone();
        noncanonical_class["rows"][0]["proof_class"] = json!("counterfactual");
        assert!(matches!(
            validate_resource_durability_report(&noncanonical_class, &manifest),
            Err(ModelProofError::Contract(ref diagnostic))
                if diagnostic == "WP009-E-NONCANONICAL-PROOF-CLASS"
        ));

        let mut invalid_mechanism = report.clone();
        invalid_mechanism["rows"][0]["proof_mechanism"] = json!("semantic");
        assert!(matches!(
            validate_resource_durability_report(&invalid_mechanism, &manifest),
            Err(ModelProofError::Contract(ref diagnostic))
                if diagnostic == "WP009-E-PROOF-MECHANISM"
        ));

        let mut wrong_attribution = report.clone();
        let cancel = wrong_attribution["rows"]
            .as_array_mut()
            .and_then(|rows| {
                rows.iter_mut().find(|row| {
                    row.get("case_id").and_then(Value::as_str) == Some("wp009-credit-owned-cancel")
                })
            })
            .expect("cancel row must exist");
        cancel["credit_attribution"]["owner"] = json!(2);
        assert!(matches!(
            validate_resource_durability_report(&wrong_attribution, &manifest),
            Err(ModelProofError::Contract(ref diagnostic))
                if diagnostic == "WP009-E-CREDIT-CANCEL-ATTRIBUTION"
        ));

        let mut wrong_broker_observation = report.clone();
        wrong_broker_observation["rows"]
            .as_array_mut()
            .and_then(|rows| {
                rows.iter_mut().find(|row| {
                    row.get("case_id").and_then(Value::as_str)
                        == Some("wp009-durability-effect-ack")
                })
            })
            .expect("durability-effect row must exist")["supplemental_execution"]["observed"] =
            json!({"kind": "ok"});
        assert!(matches!(
            validate_resource_durability_report(&wrong_broker_observation, &manifest),
            Err(ModelProofError::Contract(ref diagnostic))
                if diagnostic == "WP009-E-SUPPLEMENTAL-EXECUTION"
        ));

        let mut missing_broker_execution = report.clone();
        missing_broker_execution["rows"]
            .as_array_mut()
            .and_then(|rows| {
                rows.iter_mut().find(|row| {
                    row.get("case_id").and_then(Value::as_str)
                        == Some("wp009-durability-effect-ack")
                })
            })
            .expect("durability-effect row must exist")["supplemental_execution"] = Value::Null;
        assert!(matches!(
            validate_resource_durability_report(&missing_broker_execution, &manifest),
            Err(ModelProofError::Contract(ref diagnostic))
                if diagnostic == "WP009-E-SUPPLEMENTAL-EXECUTION"
        ));

        for (case_id, prior_residual, expected_diagnostic) in [
            (
                "wp009-filesystem-windows-ntfs",
                "Model row only; not a live Windows host/filesystem run.",
                "WP009-E-WINDOWS-ROW-RESIDUAL",
            ),
            (
                "wp009-filesystem-wsl-v9fs",
                "Model row only; not a live WSL2 host/filesystem run.",
                "WP009-E-WSL-ROW-RESIDUAL",
            ),
        ] {
            let mut contradictory = report.clone();
            let row = contradictory["rows"]
                .as_array_mut()
                .and_then(|rows| {
                    rows.iter_mut()
                        .find(|row| row.get("case_id").and_then(Value::as_str) == Some(case_id))
                })
                .expect("platform row must exist");
            row["residual_uncertainty"] = json!(prior_residual);
            assert!(matches!(
                validate_resource_durability_report(&contradictory, &manifest),
                Err(ModelProofError::Contract(ref diagnostic))
                    if diagnostic == expected_diagnostic
            ));
        }
    }

    #[test]
    fn resource_durability_report_rejects_non_idempotent_recovery_claim() {
        let manifest: Value = serde_json::from_slice(
            &fs::read(repository_root().join(RESOURCE_DURABILITY_MANIFEST))
                .expect("canonical WP-009 manifest must load"),
        )
        .expect("canonical WP-009 manifest must parse");
        let mut report = run_resource_durability_model_corpus()
            .expect("canonical WP-009 model corpus must execute");
        let recovery = report["rows"]
            .as_array_mut()
            .and_then(|rows| {
                rows.iter_mut().find(|row| {
                    row.get("case_id").and_then(Value::as_str) == Some("wp009-recovery-prepared")
                })
            })
            .expect("prepared recovery row must exist");
        recovery["observed"] = json!("different decision on identical observation");
        assert!(matches!(
            validate_resource_durability_report(&report, &manifest),
            Err(ModelProofError::Contract(ref diagnostic))
                if diagnostic == "WP009-E-RECOVERY-NON-IDEMPOTENT"
        ));
    }

    fn inventory_digest(bytes: &[u8]) -> u64 {
        bytes
            .iter()
            .filter(|byte| **byte != b'\r')
            .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
            })
    }

    #[test]
    fn prior_and_current_wire_versions_are_accepted_but_next_major_is_rejected() {
        let supported = CompatibilityRange {
            major: 1,
            minimum_minor: 0,
            maximum_minor: 1,
        };
        for fixture in ["schema-version-v1.0.json", "schema-version-v1.1.json"] {
            let bytes = read_fixture(fixture).expect("registered fixture must load");
            let version: SchemaVersion =
                serde_json::from_slice(&bytes).expect("fixture is typed JSON");
            assert!(supported.check(version).is_ok());
        }
        let bytes = read_fixture("schema-version-v2.0.json").expect("registered fixture must load");
        let version: SchemaVersion = serde_json::from_slice(&bytes).expect("fixture is typed JSON");
        assert!(supported.check(version).is_err());
    }

    #[test]
    fn shared_framing_harness_covers_partial_oversized_and_unknown_kind() {
        let payload = read_fixture("unknown-mandatory-process-kind.json")
            .expect("registered fixture must load");
        assert!(matches!(
            FrameDecoder::decode_process(&payload, FrameLimits::default()),
            Err(FrameError::UnknownMandatoryKind { .. })
        ));

        let mut partial = FrameDecoder::new(FrameLimits::default());
        assert_eq!(
            partial.push(&[0, 0]).expect("prefix is accepted"),
            (2, None)
        );
        assert!(matches!(
            partial.finish(),
            Err(FrameError::PartialHeader { received: 2 })
        ));

        let mut oversized = FrameDecoder::new(FrameLimits {
            maximum_frame_bytes: 8,
        });
        assert!(matches!(
            oversized.push(&9_u32.to_be_bytes()),
            Err(FrameError::Oversized {
                declared: 9,
                maximum: 8
            })
        ));
    }

    #[test]
    fn diagnostic_version_range_rejects_invalid_and_incompatible_ranges() {
        assert!(diagnostics::CompatibilityRange::new(1, 0, 1).is_ok());
        assert!(diagnostics::CompatibilityRange::new(0, 0, 1).is_err());
        assert!(diagnostics::CompatibilityRange::new(1, 2, 1).is_err());
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn inventory_is_unique_complete_and_references_existing_fixtures() {
        let bytes = read_fixture("inventory.json").expect("inventory must load");
        assert_eq!(
            inventory_digest(&bytes),
            CANONICAL_INVENTORY_FNV1A64,
            "canonical inventory byte digest drifted; every semantic change requires an explicit reviewed digest update"
        );
        let inventory: serde_json::Value =
            serde_json::from_slice(&bytes).expect("inventory must be JSON");
        assert_eq!(inventory["schema_id"], "ff.contract-inventory@1");
        let entries = inventory["entries"]
            .as_array()
            .expect("entries are required");
        let states = inventory["state_machines"]
            .as_array()
            .expect("state machines are required");
        assert!(entries.len() >= 12);
        assert!(states.len() >= 12);
        let mut ids = BTreeSet::new();
        for row in entries.iter().chain(states.iter()) {
            let id = row["id"].as_str().expect("stable ID is required");
            assert!(ids.insert(id), "duplicate inventory ID {id}");
            for key in ["owner", "proof_id", "readiness_gate"] {
                assert!(
                    !row[key].as_str().unwrap_or_default().is_empty(),
                    "{id} omits {key}"
                );
            }
            for fixture in row["fixture_ids"]
                .as_array()
                .expect("fixture IDs are required")
            {
                let fixture = fixture.as_str().expect("fixture ID must be a string");
                assert!(
                    fixture_root().join(fixture).is_file(),
                    "{id} fixture {fixture} is absent"
                );
            }
        }
        let canonical_contracts = [
            (
                "FF-CONTRACT-IDENTITY-001",
                "ItemId|RepresentationId|TrackId|AssetId|DerivedOutputId",
                "contracts::identity::tests::typed_ids_reject_wrong_prefix_and_uppercase",
            ),
            (
                "FF-CONTRACT-SOURCE-GRAPH-001",
                "SourceGraph",
                "testkit::tests::public_boundary_counterexamples_reject_audit_failures",
            ),
            (
                "FF-CONTRACT-ACQUISITION-001",
                "AcquisitionSource|FragmentDescriptor",
                "testkit::tests::canonical_public_contract_fixtures_decode_and_validate",
            ),
            (
                "FF-CONTRACT-OUTPUT-SINK-001",
                "OutputSinkSpec|SinkSemantics|PlayerTransport|BackpressureMode",
                "testkit::tests::canonical_public_contract_fixtures_decode_and_validate",
            ),
            (
                "FF-CONTRACT-TRISTATE-001",
                "TriState<T>",
                "contracts::graph::tests::round_trip_preserves_tri_state",
            ),
            (
                "FF-CONTRACT-EXTENSION-001",
                "ExtensionMap",
                "contracts::identity::tests::extensions_require_namespace_and_budget",
            ),
            (
                "FF-CONTRACT-CONFIG-001",
                "ConfigEnvelope",
                "testkit::tests::canonical_public_contract_fixtures_decode_and_validate",
            ),
            (
                "FF-CONTRACT-EVENT-001",
                "EventEnvelope|EventCriticality|Sensitivity",
                "testkit::tests::canonical_public_contract_fixtures_decode_and_validate",
            ),
            (
                "FF-CONTRACT-ERROR-001",
                "ErrorEnvelope|ErrorCode",
                "testkit::tests::canonical_public_contract_fixtures_decode_and_validate",
            ),
            (
                "FF-CONTRACT-CANCELLATION-001",
                "CancellationRequest|CancellationAcknowledgement|CancellationOutcome",
                "testkit::tests::canonical_public_contract_fixtures_decode_and_validate",
            ),
            (
                "FF-CONTRACT-PROCESS-001",
                "ProcessEnvelope",
                "testkit::tests::shared_framing_harness_covers_partial_oversized_and_unknown_kind",
            ),
            (
                "FF-CONTRACT-PLUGIN-IPC-001",
                "PluginMessage",
                "testkit::tests::canonical_wire_fixtures_decode_as_their_registered_contract_types",
            ),
            (
                "FF-CONTRACT-JS-WORKER-001",
                "JavaScriptWorkerMessage",
                "testkit::tests::canonical_wire_fixtures_decode_as_their_registered_contract_types",
            ),
            (
                "FF-CONTRACT-FRAMING-001",
                "FrameDecoder|ProcessConformance",
                "testkit::tests::shared_framing_harness_covers_partial_oversized_and_unknown_kind",
            ),
            (
                "FF-CONTRACT-DURABILITY-001",
                "JournalRecord|DurabilityPosition|CommitPrepared|CommitRenamed|ArchiveCommitted|ArchiveCandidate",
                "testkit::tests::canonical_wire_fixtures_decode_as_their_registered_contract_types",
            ),
            (
                "FF-CONTRACT-FILESYSTEM-001",
                "FilesystemCapability|RootedPath",
                "contracts::storage::tests::unsupported_path_confinement_fails_closed",
            ),
            (
                "FF-CONTRACT-DIAGNOSTIC-ENVELOPE-001",
                "DiagnosticEnvelope|CrashEnvelope|DiagnosticAck",
                "testkit::tests::public_boundary_counterexamples_reject_audit_failures",
            ),
            (
                "FF-CONTRACT-DIAGNOSTIC-PROTOCOL-001",
                "ProtocolOfferV1|ProtocolOffer|SequenceTracker",
                "testkit::tests::public_boundary_counterexamples_reject_audit_failures",
            ),
            (
                "FF-CONTRACT-DIAGNOSTIC-LIFECYCLE-001",
                "LifecycleSnapshot|HealthSnapshot|WatcherState",
                "testkit::tests::canonical_wire_fixtures_decode_as_their_registered_contract_types",
            ),
            (
                "FF-CONTRACT-RESOURCE-VECTOR-001",
                "ResourceVector|ResourceLedger|ByteCreditLedger|CreditAttribution",
                "core::resource::tests::receive_requires_exact_claim_owner_and_records_attribution",
            ),
            (
                "FF-CONTRACT-FFMPEG-SUPERVISION-001",
                "FfmpegSupervisionRequestV1|FfmpegSupervisionReportV1",
                "contracts::ffmpeg::tests::registered_public_boundary_suite",
            ),
        ];
        assert_eq!(entries.len(), canonical_contracts.len());
        for (id, rust_type, proof_id) in canonical_contracts {
            let row = entries
                .iter()
                .find(|row| row["id"] == id)
                .unwrap_or_else(|| panic!("missing canonical contract {id}"));
            assert_eq!(row["rust_type"], rust_type, "{id} rust_type drift");
            assert_eq!(row["proof_id"], proof_id, "{id} proof_id drift");
        }

        let canonical_states = [
            (
                "FF-STATE-JOB-CANCEL-001",
                "EffectPending|EffectRecovery|JobQueued|JobRunning|JobCancelling|JobVerifying|JobSucceeded|JobFailed|JobCancelled",
                "every emitted effect requires an exact correlated outcome before the target state is visible|verification intent cannot directly produce success|trace is bounded",
                "core::lifecycle::tests::success_and_durable_prefixes_require_effect_acknowledgements",
            ),
            (
                "FF-STATE-SOURCE-REDIRECT-001",
                "EffectPending|EffectRecovery|SourceNew|SourceResolving|SourceRedirecting|SourceResolved|SourceFailed|SourceCancelled",
                "every emitted effect requires an exact correlated outcome before the target state is visible|redirect resolution is explicit",
                "core::lifecycle::tests::every_named_lifecycle_has_a_success_path",
            ),
            (
                "FF-STATE-ADMISSION-001",
                "EffectPending|EffectRecovery|AdmissionWaiting|AdmissionGranted|AdmissionReleased|AdmissionCancelled",
                "every emitted effect requires an exact correlated outcome before the target state is visible|vector grants are atomic|capacity never underflows",
                "core::resource::tests::atomic_zero_exact_one_over_and_release_identity",
            ),
            (
                "FF-STATE-FRAGMENT-DURABILITY-001",
                "EffectPending|EffectRecovery|BytesEmpty|BytesReceived|BytesWriting|BytesWritten|BytesSynchronizing|BytesDurable|BytesFailed|BytesCancelled",
                "every emitted effect requires an exact correlated outcome before the target state is visible|write and synchronization effects require correlated acknowledgement|durable never exceeds validated or received|positions never regress|released unused credits cannot authorize receive|received-byte consumption is attributable to one claim owner|consumed claims cannot transfer ownership",
                "core::resource::tests::receive_requires_exact_claim_owner_and_records_attribution",
            ),
            (
                "FF-STATE-LIVE-001",
                "EffectPending|EffectRecovery|LiveStarting|LiveRefreshing|LiveStreaming|LiveStopped|LiveFailed|LiveCancelled",
                "every emitted effect requires an exact correlated outcome before the target state is visible|drain precedes stop",
                "core::lifecycle::tests::every_named_lifecycle_has_a_success_path",
            ),
            (
                "FF-STATE-SINK-001",
                "EffectPending|EffectRecovery|SinkPending|SinkActive|SinkDraining|SinkCompleted|SinkDropped|SinkFailed|SinkCancelled",
                "every emitted effect requires an exact correlated outcome before the target state is visible|partial output cannot be archived",
                "core::lifecycle::tests::failure_paths_complete_required_effects_before_their_outcome_state",
            ),
            (
                "FF-STATE-FFMPEG-001",
                "FfmpegPrepared|FfmpegSpawning|FfmpegSpawned|FfmpegRunning|FfmpegReaping|FfmpegCancelling|FfmpegFailing|FfmpegExitReleasing|FfmpegCancellationReleasing|FfmpegFailureReleasing|FfmpegSpawnRecovering|FfmpegReapRecovering|FfmpegCancellationRecovering|FfmpegFailureRecovering|FfmpegExitReleaseRecovering|FfmpegCancellationReleaseRecovering|FfmpegFailureReleaseRecovering|FfmpegExited|FfmpegFailed|FfmpegCancelled",
                "spawn, process cleanup, diagnostic preservation, and resource release effects require exact correlated outcomes|terminal success, cancellation, and failure await process and release-resource acknowledgement|all releasing and recovery states are non-durable|all original process cleanup outcomes settle before recovery; failed or cancelled effect outcomes require recovery acknowledgement and exact retry",
                "testkit::tests::public_boundary_counterexamples_reject_audit_failures",
            ),
            (
                "FF-STATE-JS-WORKER-001",
                "EffectPending|EffectRecovery|JavascriptIdle|JavascriptAssigned|JavascriptRunning|JavascriptRecycling|JavascriptQuarantined|JavascriptCompleted|JavascriptCancelled",
                "every emitted effect requires an exact correlated outcome before the target state is visible|terminal response is unique",
                "core::lifecycle::tests::illegal_transitions_are_typed_and_do_not_mutate_or_trace",
            ),
            (
                "FF-STATE-PLUGIN-IPC-001",
                "EffectPending|EffectRecovery|PluginDisconnected|PluginHandshaking|PluginReady|PluginInFlight|PluginDraining|PluginStopped|PluginFailed",
                "every emitted effect requires an exact correlated outcome before the target state is visible|negotiation precedes invoke",
                "core::lifecycle::tests::cancellation_paths_reach_expected_states",
            ),
            (
                "FF-STATE-COMMIT-ARCHIVE-001",
                "EffectPending|EffectRecovery|CommitWorking|CommitPreparing|CommitPrepared|CommitRenaming|CommitRenamed|CommitArchiving|CommitArchived|CommitCleaning|CommitCleaned|CommitReconciling|CommitVerifyingPrepared|CommitVerifyingRenamed|CommitVerifyingArchived|CommitVerifyingCleaned|CommitCancelling|CommitReconciled|CommitInconsistent|CommitCancelled",
                "every emitted effect requires an exact correlated outcome before the target state is visible|archive requires acknowledged rename|restart verification is acknowledged before archive or cleanup|restart never invents success|effect intent advances only after matching instance identity, effect, and generation acknowledgement|only enumerated durable prefixes can be restored",
                "core::lifecycle::tests::transient_restore_and_stale_or_wrong_acknowledgements_are_rejected",
            ),
            (
                "FF-STATE-FILESYSTEM-CAPABILITY-001",
                "FilesystemUnknown|FilesystemProbing|FilesystemProbed|FilesystemProbeFailed|FilesystemProbeCancelled|FilesystemConfining|FilesystemConfinementFailed|FilesystemConfinementCancelled|FilesystemConfined|FilesystemDegrading|FilesystemDegradationFailed|FilesystemDegradationCancelled|FilesystemDegraded|FilesystemRejecting|FilesystemRejectionFailed|FilesystemRejectionCancelled|FilesystemUnsupported|FilesystemCancelled",
                "probe, confinement, degradation reporting, and rejection effects require exact correlated outcomes|degraded never claims confinement|requested confinement is non-durable until correlated establishment acknowledgement|all failed and cancelled effect outcomes remain non-durable and recoverable",
                "testkit::tests::public_boundary_counterexamples_reject_audit_failures",
            ),
            (
                "FF-STATE-WATCHER-001",
                "EffectPending|EffectRecovery|WatcherStarting|WatcherReady|WatcherServing|WatcherDegraded|WatcherStale|WatcherDraining|WatcherStopped",
                "every emitted effect requires an exact correlated outcome before the target state is visible|readiness and producer canary are separate",
                "core::lifecycle::tests::cancellation_paths_reach_expected_states",
            ),
        ];
        assert_eq!(states.len(), canonical_states.len());
        for (id, expected_states, expected_invariants, proof_id) in canonical_states {
            let row = states
                .iter()
                .find(|row| row["id"] == id)
                .unwrap_or_else(|| panic!("missing canonical state machine {id}"));
            assert_eq!(
                json_strings(&row["states"]),
                expected_states,
                "{id} state drift"
            );
            assert_eq!(
                json_strings(&row["invariants"]),
                expected_invariants,
                "{id} invariant drift"
            );
            assert_eq!(row["proof_id"], proof_id, "{id} proof_id drift");
            let expected_durable = durable_states(inventory_machine_kind(id))
                .iter()
                .map(|state| format!("{state:?}"))
                .collect::<Vec<_>>()
                .join("|");
            let expected_durable = if expected_durable.is_empty() {
                "none"
            } else {
                &expected_durable
            };
            assert_eq!(
                json_strings(&row["durable_prefixes"]),
                expected_durable,
                "{id} durable-prefix whitelist drift"
            );
        }
        for required_id in [
            "FF-CONTRACT-ACQUISITION-001",
            "FF-CONTRACT-OUTPUT-SINK-001",
            "FF-CONTRACT-CONFIG-001",
            "FF-CONTRACT-EVENT-001",
            "FF-CONTRACT-ERROR-001",
            "FF-CONTRACT-CANCELLATION-001",
        ] {
            assert!(ids.contains(required_id), "inventory omits {required_id}");
        }
    }

    #[test]
    fn inventory_digest_rejects_semantic_field_mutations() {
        let bytes = read_fixture("inventory.json").expect("inventory must load");
        assert_eq!(inventory_digest(&bytes), CANONICAL_INVENTORY_FNV1A64);
        for field in [
            "version_policy",
            "limits_errors",
            "design_anchors",
            "residual_uncertainty",
            "preconditions",
            "postconditions",
            "invalid_transitions",
            "cancellation_outcomes",
            "durable_prefixes",
            "finite_assumptions",
        ] {
            let mut mutated = bytes.clone();
            let needle = field.as_bytes();
            let key_offset = mutated
                .windows(needle.len())
                .position(|window| window == needle)
                .unwrap_or_else(|| panic!("canonical inventory omits {field}"));
            let value_offset = key_offset
                + needle.len()
                + mutated[key_offset + needle.len()..]
                    .iter()
                    .position(u8::is_ascii_alphabetic)
                    .unwrap_or_else(|| panic!("{field} has no textual canonical value"));
            mutated[value_offset] ^= 0x20;
            assert_ne!(
                inventory_digest(&mutated),
                CANONICAL_INVENTORY_FNV1A64,
                "{field} mutation bypassed the canonical digest"
            );
        }
    }

    fn json_strings(value: &serde_json::Value) -> String {
        value
            .as_array()
            .expect("canonical field must be an array")
            .iter()
            .map(|value| value.as_str().expect("canonical item must be a string"))
            .collect::<Vec<_>>()
            .join("|")
    }

    fn inventory_machine_kind(id: &str) -> MachineKind {
        match id {
            "FF-STATE-JOB-CANCEL-001" => MachineKind::JobCancellation,
            "FF-STATE-SOURCE-REDIRECT-001" => MachineKind::SourceRedirect,
            "FF-STATE-ADMISSION-001" => MachineKind::AtomicAdmission,
            "FF-STATE-FRAGMENT-DURABILITY-001" => MachineKind::ByteCreditDurability,
            "FF-STATE-LIVE-001" => MachineKind::Live,
            "FF-STATE-SINK-001" => MachineKind::Sink,
            "FF-STATE-FFMPEG-001" => MachineKind::Ffmpeg,
            "FF-STATE-JS-WORKER-001" => MachineKind::JavascriptWorker,
            "FF-STATE-PLUGIN-IPC-001" => MachineKind::PluginIpc,
            "FF-STATE-COMMIT-ARCHIVE-001" => MachineKind::CommitArchiveReconciliation,
            "FF-STATE-FILESYSTEM-CAPABILITY-001" => MachineKind::FilesystemCapability,
            "FF-STATE-WATCHER-001" => MachineKind::Watcher,
            _ => panic!("unregistered inventory state-machine ID {id}"),
        }
    }

    #[test]
    fn fixture_loader_rejects_parent_traversal() {
        assert!(matches!(
            read_fixture("../Cargo.toml"),
            Err(FixtureError::EscapesRoot)
        ));
    }

    #[test]
    fn canonical_wire_fixtures_decode_as_their_registered_contract_types() {
        canonical_wire_decodes_typed_identities();
        canonical_wire_decodes_source_graph();
        canonical_wire_decodes_process_and_worker_envelopes();
        canonical_wire_decodes_durability_and_filesystem_contracts();
        canonical_wire_decodes_diagnostics_contracts();
        canonical_wire_decodes_ffmpeg_supervision_contract();
    }

    fn canonical_wire_decodes_ffmpeg_supervision_contract() {
        use fforager_contracts::ffmpeg::{FfmpegSupervisionReportV1, FfmpegSupervisionRequestV1};

        let build_fixture =
            read_fixture("ffmpeg-supervision-v1.0.json").expect("FFmpeg fixture must load");
        let product_fixture = include_bytes!(
            "../../../../product/crates/fforager-contracts/testdata/ffmpeg-supervision-v1.0.json"
        );
        assert_eq!(
            build_fixture.as_slice(),
            product_fixture,
            "product contract test fixture and build-owned proof fixture must remain byte-identical"
        );
        let value: Value =
            serde_json::from_slice(&build_fixture).expect("FFmpeg fixture must be JSON");
        let request: FfmpegSupervisionRequestV1 =
            serde_json::from_value(value["request"].clone()).expect("request must decode");
        let report: FfmpegSupervisionReportV1 =
            serde_json::from_value(value["report"].clone()).expect("report must decode");
        assert!(request.validate_for_invocation().is_ok());
        assert_eq!(report.validate_against(&request), Ok(()));
    }

    fn canonical_wire_decodes_typed_identities() {
        let identities: serde_json::Value = serde_json::from_slice(
            &read_fixture("identity-set-v1.0.json").expect("identity fixture must load"),
        )
        .expect("identity fixture is JSON");
        assert!(ItemId::new(identities["item"].as_str().unwrap_or_default()).is_ok());
        assert!(
            RepresentationId::new(identities["representation"].as_str().unwrap_or_default())
                .is_ok()
        );
        assert!(TrackId::new(identities["track"].as_str().unwrap_or_default()).is_ok());
        assert!(AssetId::new(identities["asset"].as_str().unwrap_or_default()).is_ok());
        assert!(
            DerivedOutputId::new(identities["derived_output"].as_str().unwrap_or_default()).is_ok()
        );
    }

    fn canonical_wire_decodes_source_graph() {
        let graph: SourceGraph = serde_json::from_slice(
            &read_fixture("source-graph-v1.1.json").expect("graph fixture must load"),
        )
        .expect("graph fixture must decode");
        assert!(graph.validate(GraphLimits::default()).is_ok());
        let encoded = serde_json::to_vec(&graph).expect("graph must serialize");
        assert_eq!(
            serde_json::from_slice::<SourceGraph>(&encoded).ok(),
            Some(graph)
        );
    }

    fn canonical_wire_decodes_process_and_worker_envelopes() {
        let process_bytes =
            read_fixture("process-request-v1.0.json").expect("process fixture must load");
        assert!(matches!(
            FrameDecoder::decode_process(&process_bytes, FrameLimits::default()),
            Ok(ProcessEnvelope::Request(_))
        ));

        let plugin: PluginEnvelope = serde_json::from_slice(
            &read_fixture("plugin-envelope-v1.0.json").expect("plugin fixture must load"),
        )
        .expect("plugin fixture must decode");
        assert!(plugin.validate(ProtocolLimits::default()).is_ok());
        let javascript: JavaScriptWorkerEnvelope = serde_json::from_slice(
            &read_fixture("javascript-worker-envelope-v1.0.json")
                .expect("javascript fixture must load"),
        )
        .expect("javascript fixture must decode");
        assert!(javascript.validate(ProtocolLimits::default()).is_ok());
    }

    fn canonical_wire_decodes_durability_and_filesystem_contracts() {
        let archive: ArchiveCandidate = serde_json::from_slice(
            &read_fixture("archive-candidate-v1.0.json").expect("archive fixture must load"),
        )
        .expect("archive fixture must decode");
        assert!(archive.validate().is_ok());

        let durability: serde_json::Value = serde_json::from_slice(
            &read_fixture("durability-contracts-v1.0.json").expect("durability fixture must load"),
        )
        .expect("durability fixture must be JSON");
        let position: DurabilityPosition = serde_json::from_value(durability["position"].clone())
            .expect("durability position must decode");
        assert!(position.validate().is_ok());
        let _: CommitPrepared = serde_json::from_value(durability["commit_prepared"].clone())
            .expect("prepared commit must decode");
        let _: CommitRenamed = serde_json::from_value(durability["commit_renamed"].clone())
            .expect("renamed commit must decode");
        let _: ArchiveCommitted = serde_json::from_value(durability["archive_committed"].clone())
            .expect("archive commit must decode");
        let _: JournalRecord = serde_json::from_value(durability["journal_record"].clone())
            .expect("journal record must decode");
        let durable_archive: ArchiveCandidate =
            serde_json::from_value(durability["archive_candidate"].clone())
                .expect("durability archive candidate must decode");
        assert!(durable_archive.validate().is_ok());
        let filesystem: FilesystemCapability = serde_json::from_slice(
            &read_fixture("filesystem-capability-v1.0.json").expect("filesystem fixture must load"),
        )
        .expect("filesystem fixture must decode");
        assert!(filesystem.validate_secure_write().is_ok());
    }

    fn canonical_wire_decodes_diagnostics_contracts() {
        let diagnostic_bytes =
            read_fixture("diagnostic-envelope-v1.2.json").expect("diagnostic fixture must load");
        assert!(
            diagnostics::decode_json_frame(
                &diagnostic_bytes,
                diagnostics::FrameCompleteness::Complete
            )
            .is_ok()
        );
        let legacy_offer: diagnostics::ProtocolOfferV1 = serde_json::from_slice(
            &read_fixture("diagnostic-protocol-offer-v1.0.json")
                .expect("protocol offer fixture must load"),
        )
        .expect("legacy protocol offer must decode");
        let offer = legacy_offer
            .into_v2()
            .expect("legacy protocol offer must migrate into v2");
        let current_offer: diagnostics::ProtocolOffer = serde_json::from_slice(
            &read_fixture("diagnostic-protocol-offer-v2.0.json")
                .expect("current protocol offer fixture must load"),
        )
        .expect("current protocol offer must decode");
        let strict_authority = diagnostics::SchemaCompatibilityAuthority::strict();
        assert!(offer.negotiate(&offer, strict_authority).is_ok());
        assert!(
            current_offer
                .negotiate(&current_offer, strict_authority)
                .is_ok()
        );
        let lifecycle: diagnostics::LifecycleSnapshot = serde_json::from_slice(
            &read_fixture("diagnostic-lifecycle-v1.0.json")
                .expect("diagnostic lifecycle fixture must load"),
        )
        .expect("diagnostic lifecycle must decode");
        assert_eq!(lifecycle, diagnostics::LifecycleSnapshot::starting());
    }

    #[test]
    fn public_boundary_counterexamples_reject_audit_failures() {
        public_boundary_rejects_source_graph_cycle();
        public_boundary_enforces_filesystem_effect_correlation();
        public_boundary_enforces_ffmpeg_terminal_effects();
        public_boundary_waits_for_all_ffmpeg_cleanup_outcomes_before_recovery();
        public_boundary_rejects_out_of_order_mixed_ffmpeg_cleanup_outcomes();
        public_boundary_rejects_diagnostics_counterexamples();
        public_boundary_rejects_durable_storage_counterexamples();
        public_boundary_requires_every_emitted_effect_to_clear_before_durability();
        println!(
            "FF-PUBLIC-COUNTEREXAMPLE-RECEIPT:v5:source-graph-cycle,filesystem-effect-correlation,ffmpeg-terminal-release,ffmpeg-partial-unsuccessful-outcomes,schema-authority,sequence-zero,unknown-envelope-field,nested-wire-unknown-fields,durable-journal-payload-unknown-field,durable-journal-record-unknown-field,durable-reconcile-state-unknown-field,durable-journal-sequence-zero,acknowledged-effect-prefixes"
        );
    }

    fn public_boundary_rejects_source_graph_cycle() {
        let mut graph: SourceGraph = serde_json::from_slice(
            &read_fixture("source-graph-v1.1.json").expect("graph fixture must load"),
        )
        .expect("graph fixture must decode");
        let root = graph.roots[0].clone();
        graph.edges.push(SourceEdge {
            id: EdgeId::new("edge_public_cycle").expect("stable edge id"),
            from: root.clone(),
            to: root,
            kind: EdgeKind::Contains,
        });
        assert!(matches!(
            graph.validate(GraphLimits::default()),
            Err(GraphError::RelationshipCycle { .. })
        ));
    }

    #[allow(clippy::too_many_lines)]
    fn public_boundary_enforces_filesystem_effect_correlation() {
        let mut filesystem = StateMachine::new(
            MachineKind::FilesystemCapability,
            MachineInstanceId::new(901).expect("nonzero instance"),
            8,
        );
        assert!(filesystem.apply(Event::Probe).is_ok());
        assert_eq!(filesystem.state(), State::FilesystemProbing);
        acknowledge_all_pending_effects(&mut filesystem);
        assert_eq!(filesystem.state(), State::FilesystemProbed);
        assert!(filesystem.apply(Event::Confine).is_ok());
        assert_eq!(filesystem.state(), State::FilesystemConfining);
        assert_ne!(filesystem.state(), State::FilesystemConfined);
        let filesystem_ack = filesystem.pending_acknowledgements()[0];
        assert_eq!(filesystem_ack.effect, EffectIntent::EstablishConfinedPath);
        assert!(
            filesystem
                .apply(Event::EffectAcknowledged {
                    instance_id: filesystem_ack.instance_id,
                    effect: filesystem_ack.effect,
                    generation: filesystem_ack.generation,
                })
                .is_ok()
        );
        assert_eq!(filesystem.state(), State::FilesystemConfined);

        let mut failed_filesystem = StateMachine::new(
            MachineKind::FilesystemCapability,
            MachineInstanceId::new(903).expect("nonzero instance"),
            8,
        );
        assert!(failed_filesystem.apply(Event::Probe).is_ok());
        acknowledge_all_pending_effects(&mut failed_filesystem);
        assert!(failed_filesystem.apply(Event::Confine).is_ok());
        let failed_confinement = failed_filesystem.pending_acknowledgements()[0];
        assert!(
            failed_filesystem
                .apply(Event::EffectFailed {
                    instance_id: MachineInstanceId::new(904).expect("wrong nonzero instance"),
                    effect: failed_confinement.effect,
                    generation: failed_confinement.generation,
                })
                .is_err()
        );
        assert_eq!(failed_filesystem.state(), State::FilesystemConfining);
        assert!(
            failed_filesystem
                .apply(Event::EffectFailed {
                    instance_id: failed_confinement.instance_id,
                    effect: failed_confinement.effect,
                    generation: failed_confinement.generation,
                })
                .is_ok()
        );
        assert_eq!(
            failed_filesystem.state(),
            State::FilesystemConfinementFailed
        );
        assert_ne!(failed_filesystem.state(), State::FilesystemConfined);

        let mut cancelled_filesystem = StateMachine::new(
            MachineKind::FilesystemCapability,
            MachineInstanceId::new(905).expect("nonzero instance"),
            8,
        );
        assert!(cancelled_filesystem.apply(Event::Probe).is_ok());
        acknowledge_all_pending_effects(&mut cancelled_filesystem);
        assert!(cancelled_filesystem.apply(Event::Confine).is_ok());
        let cancellation = cancelled_filesystem.pending_acknowledgements()[0];
        let before_wrong_cancellation = format!("{cancelled_filesystem:?}");
        assert!(matches!(
            cancelled_filesystem.apply(Event::EffectCancelled {
                instance_id: MachineInstanceId::new(908).expect("wrong nonzero instance"),
                effect: cancellation.effect,
                generation: cancellation.generation,
            }),
            Err(TransitionError::UnexpectedEffectCancellation { .. })
        ));
        assert_eq!(
            format!("{cancelled_filesystem:?}"),
            before_wrong_cancellation
        );
        assert!(matches!(
            cancelled_filesystem.apply(Event::EffectCancelled {
                instance_id: cancellation.instance_id,
                effect: cancellation.effect,
                generation: cancellation.generation.saturating_add(1),
            }),
            Err(TransitionError::UnexpectedEffectCancellation { .. })
        ));
        assert_eq!(
            format!("{cancelled_filesystem:?}"),
            before_wrong_cancellation
        );
        assert!(
            cancelled_filesystem
                .apply(Event::EffectCancelled {
                    instance_id: cancellation.instance_id,
                    effect: cancellation.effect,
                    generation: cancellation.generation,
                })
                .is_ok()
        );
        assert_eq!(
            cancelled_filesystem.state(),
            State::FilesystemConfinementCancelled
        );
    }

    fn public_boundary_enforces_ffmpeg_terminal_effects() {
        public_boundary_requires_ffmpeg_cancellation_release();
        public_boundary_requires_ffmpeg_exit_release();
        public_boundary_requires_ffmpeg_failure_release();
    }

    fn public_boundary_waits_for_all_ffmpeg_cleanup_outcomes_before_recovery() {
        let cases: &[(&str, Event, State, State, &[EffectIntent])] = &[
            (
                "cancellation-cleanup",
                Event::Cancel,
                State::FfmpegCancellationRecovering,
                State::FfmpegCancelling,
                &[EffectIntent::TerminateProcess, EffectIntent::ReapProcess],
            ),
            (
                "failure-cleanup",
                Event::Fail,
                State::FfmpegFailureRecovering,
                State::FfmpegFailing,
                &[
                    EffectIntent::TerminateProcess,
                    EffectIntent::ReapProcess,
                    EffectIntent::PreserveDiagnostics,
                ],
            ),
        ];

        for (case_index, (label, start_cleanup, recovery, waiting, expected_reissue)) in
            cases.iter().enumerate()
        {
            for cancelled in [false, true] {
                for unsuccessful_index in 0..expected_reissue.len() {
                    let instance = 920 + (case_index as u64 * 16) + unsuccessful_index as u64;
                    let mut ffmpeg = StateMachine::new(
                        MachineKind::Ffmpeg,
                        MachineInstanceId::new(instance).expect("nonzero instance"),
                        24,
                    );
                    assert!(ffmpeg.apply(Event::Spawn).is_ok());
                    acknowledge_all_pending_effects(&mut ffmpeg);
                    assert!(ffmpeg.apply(Event::Start).is_ok());
                    assert!(ffmpeg.apply(*start_cleanup).is_ok());
                    let original = ffmpeg.pending_acknowledgements().to_vec();
                    assert_eq!(
                        original.iter().map(|ack| ack.effect).collect::<Vec<_>>(),
                        *expected_reissue,
                        "{label} must issue the complete cleanup set"
                    );

                    for acknowledgement in &original[..unsuccessful_index] {
                        assert!(
                            ffmpeg
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
                        ffmpeg.apply(Event::EffectCancelled {
                            instance_id: unsuccessful.instance_id,
                            effect: unsuccessful.effect,
                            generation: unsuccessful.generation,
                        })
                    } else {
                        ffmpeg.apply(Event::EffectFailed {
                            instance_id: unsuccessful.instance_id,
                            effect: unsuccessful.effect,
                            generation: unsuccessful.generation,
                        })
                    };
                    assert!(result.is_ok(), "{label} outcome must be correlated");

                    let remaining = original.len() - unsuccessful_index - 1;
                    if remaining > 0 {
                        assert_eq!(ffmpeg.state(), *waiting);
                        assert_eq!(ffmpeg.pending_acknowledgements().len(), remaining);
                        assert!(ffmpeg.apply(Event::Restart).is_err());
                        for acknowledgement in &original[unsuccessful_index + 1..] {
                            assert!(
                                ffmpeg
                                    .apply(Event::EffectAcknowledged {
                                        instance_id: acknowledgement.instance_id,
                                        effect: acknowledgement.effect,
                                        generation: acknowledgement.generation,
                                    })
                                    .is_ok()
                            );
                        }
                    }

                    assert_eq!(ffmpeg.state(), *recovery);
                    assert_eq!(ffmpeg.pending_acknowledgements().len(), 1);
                    assert_eq!(
                        ffmpeg.pending_acknowledgements()[0].effect,
                        EffectIntent::PreserveDiagnostics
                    );
                    acknowledge_all_pending_effects(&mut ffmpeg);
                    let retry = ffmpeg.apply(Event::Restart).expect("explicit exact retry");
                    assert_eq!(retry.next, *waiting);
                    assert_eq!(retry.effects, *expected_reissue);
                }
            }
        }
    }

    fn public_boundary_rejects_out_of_order_cancellation_cleanup_outcomes() {
        let mut cancelling = StateMachine::new(
            MachineKind::Ffmpeg,
            MachineInstanceId::new(950).expect("nonzero instance"),
            48,
        );
        assert!(cancelling.apply(Event::Spawn).is_ok());
        acknowledge_all_pending_effects(&mut cancelling);
        assert!(cancelling.apply(Event::Start).is_ok());
        assert!(cancelling.apply(Event::Cancel).is_ok());
        let original = cancelling.pending_acknowledgements().to_vec();
        let original_effects = original
            .iter()
            .map(|acknowledgement| acknowledgement.effect)
            .collect::<Vec<_>>();
        let original_generation = original[0].generation;
        let terminate = original
            .iter()
            .find(|acknowledgement| acknowledgement.effect == EffectIntent::TerminateProcess)
            .copied()
            .expect("terminate must be pending");
        let reap = original
            .iter()
            .find(|acknowledgement| acknowledgement.effect == EffectIntent::ReapProcess)
            .copied()
            .expect("reap must be pending");

        assert!(cancelling.apply(unsuccessful_outcome(false, reap)).is_ok());
        assert_eq!(cancelling.state(), State::FfmpegCancelling);
        assert_eq!(cancelling.pending_acknowledgements(), &[terminate]);
        let before_stale_reap = format!("{cancelling:?}");
        assert!(matches!(
            cancelling.apply(unsuccessful_outcome(false, reap)),
            Err(TransitionError::UnexpectedEffectFailure { .. })
        ));
        assert_eq!(format!("{cancelling:?}"), before_stale_reap);
        assert!(cancelling.apply(Event::Restart).is_err());

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
        acknowledge_all_pending_effects(&mut cancelling);
        let retry = cancelling
            .apply(Event::Restart)
            .expect("every cancellation cleanup receipt settles before recovery retry");
        assert_eq!(retry.next, State::FfmpegCancelling);
        assert_eq!(retry.effects, original_effects);
        assert!(
            cancelling
                .pending_acknowledgements()
                .iter()
                .all(|acknowledgement| acknowledgement.generation != original_generation)
        );
    }

    fn public_boundary_rejects_out_of_order_failure_cleanup_outcomes() {
        let mut failing = StateMachine::new(
            MachineKind::Ffmpeg,
            MachineInstanceId::new(951).expect("nonzero instance"),
            64,
        );
        assert!(failing.apply(Event::Spawn).is_ok());
        acknowledge_all_pending_effects(&mut failing);
        assert!(failing.apply(Event::Fail).is_ok());
        let original = failing.pending_acknowledgements().to_vec();
        let original_effects = original
            .iter()
            .map(|acknowledgement| acknowledgement.effect)
            .collect::<Vec<_>>();
        let original_generation = original[0].generation;
        let terminate = original
            .iter()
            .find(|acknowledgement| acknowledgement.effect == EffectIntent::TerminateProcess)
            .copied()
            .expect("terminate must be pending");
        let reap = original
            .iter()
            .find(|acknowledgement| acknowledgement.effect == EffectIntent::ReapProcess)
            .copied()
            .expect("reap must be pending");
        let preserve = original
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
        let before_stale_reap = format!("{failing:?}");
        assert!(matches!(
            failing.apply(unsuccessful_outcome(true, reap)),
            Err(TransitionError::UnexpectedEffectCancellation { .. })
        ));
        assert_eq!(format!("{failing:?}"), before_stale_reap);
        assert!(failing.apply(Event::Restart).is_err());

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
        acknowledge_all_pending_effects(&mut failing);
        let retry = failing
            .apply(Event::Restart)
            .expect("every failure cleanup receipt settles before recovery retry");
        assert_eq!(retry.next, State::FfmpegFailing);
        assert_eq!(retry.effects, original_effects);
        assert!(
            failing
                .pending_acknowledgements()
                .iter()
                .all(|acknowledgement| acknowledgement.generation != original_generation)
        );
    }

    fn public_boundary_rejects_out_of_order_mixed_ffmpeg_cleanup_outcomes() {
        public_boundary_rejects_out_of_order_cancellation_cleanup_outcomes();
        public_boundary_rejects_out_of_order_failure_cleanup_outcomes();
    }

    fn public_boundary_requires_ffmpeg_cancellation_release() {
        let mut ffmpeg = StateMachine::new(
            MachineKind::Ffmpeg,
            MachineInstanceId::new(902).expect("nonzero instance"),
            12,
        );
        assert!(ffmpeg.apply(Event::Spawn).is_ok());
        acknowledge_all_pending_effects(&mut ffmpeg);
        assert!(ffmpeg.apply(Event::Start).is_ok());
        assert!(ffmpeg.apply(Event::Cancel).is_ok());
        let terminate = ffmpeg
            .pending_acknowledgements()
            .iter()
            .find(|ack| ack.effect == EffectIntent::TerminateProcess)
            .copied()
            .expect("terminate acknowledgement must be pending");
        assert!(
            ffmpeg
                .apply(Event::EffectAcknowledged {
                    instance_id: terminate.instance_id,
                    effect: terminate.effect,
                    generation: terminate.generation,
                })
                .is_ok()
        );
        assert_eq!(ffmpeg.state(), State::FfmpegCancelling);
        assert_ne!(ffmpeg.state(), State::FfmpegCancelled);
        assert_eq!(ffmpeg.pending_acknowledgements().len(), 1);
        let reap = ffmpeg.pending_acknowledgements()[0];
        assert_eq!(reap.effect, EffectIntent::ReapProcess);
        assert!(
            ffmpeg
                .apply(Event::EffectAcknowledged {
                    instance_id: reap.instance_id,
                    effect: reap.effect,
                    generation: reap.generation,
                })
                .is_ok()
        );
        assert_eq!(ffmpeg.state(), State::FfmpegCancellationReleasing);
        assert_ne!(ffmpeg.state(), State::FfmpegCancelled);
        let release = ffmpeg.pending_acknowledgements()[0];
        assert_eq!(release.effect, EffectIntent::ReleaseResources);
        assert!(
            ffmpeg
                .apply(Event::EffectAcknowledged {
                    instance_id: release.instance_id,
                    effect: release.effect,
                    generation: release.generation,
                })
                .is_ok()
        );
        assert_eq!(ffmpeg.state(), State::FfmpegCancelled);
    }

    fn public_boundary_requires_ffmpeg_exit_release() {
        let mut completed_ffmpeg = StateMachine::new(
            MachineKind::Ffmpeg,
            MachineInstanceId::new(906).expect("nonzero instance"),
            12,
        );
        assert!(completed_ffmpeg.apply(Event::Spawn).is_ok());
        acknowledge_all_pending_effects(&mut completed_ffmpeg);
        assert!(completed_ffmpeg.apply(Event::Start).is_ok());
        assert!(completed_ffmpeg.apply(Event::Complete).is_ok());
        let complete_reap = completed_ffmpeg.pending_acknowledgements()[0];
        assert!(
            completed_ffmpeg
                .apply(Event::EffectAcknowledged {
                    instance_id: complete_reap.instance_id,
                    effect: complete_reap.effect,
                    generation: complete_reap.generation,
                })
                .is_ok()
        );
        assert_eq!(completed_ffmpeg.state(), State::FfmpegExitReleasing);
        let complete_release = completed_ffmpeg.pending_acknowledgements()[0];
        assert_eq!(complete_release.effect, EffectIntent::ReleaseResources);
        assert!(
            completed_ffmpeg
                .apply(Event::EffectAcknowledged {
                    instance_id: complete_release.instance_id,
                    effect: complete_release.effect,
                    generation: complete_release.generation,
                })
                .is_ok()
        );
        assert_eq!(completed_ffmpeg.state(), State::FfmpegExited);
    }

    fn public_boundary_requires_ffmpeg_failure_release() {
        let mut failed_ffmpeg = StateMachine::new(
            MachineKind::Ffmpeg,
            MachineInstanceId::new(907).expect("nonzero instance"),
            12,
        );
        assert!(failed_ffmpeg.apply(Event::Spawn).is_ok());
        acknowledge_all_pending_effects(&mut failed_ffmpeg);
        assert!(failed_ffmpeg.apply(Event::Fail).is_ok());
        for acknowledgement in failed_ffmpeg.pending_acknowledgements().to_vec() {
            assert!(
                failed_ffmpeg
                    .apply(Event::EffectAcknowledged {
                        instance_id: acknowledgement.instance_id,
                        effect: acknowledgement.effect,
                        generation: acknowledgement.generation,
                    })
                    .is_ok()
            );
        }
        assert_eq!(failed_ffmpeg.state(), State::FfmpegFailureReleasing);
        let failure_release = failed_ffmpeg.pending_acknowledgements()[0];
        assert_eq!(failure_release.effect, EffectIntent::ReleaseResources);
        assert!(
            failed_ffmpeg
                .apply(Event::EffectAcknowledged {
                    instance_id: failure_release.instance_id,
                    effect: failure_release.effect,
                    generation: failure_release.generation,
                })
                .is_ok()
        );
        assert_eq!(failed_ffmpeg.state(), State::FfmpegFailed);
    }

    fn public_boundary_rejects_diagnostics_counterexamples() {
        let key = diagnostics::SequenceKey {
            producer_instance: diagnostics::ProducerInstanceId::new("producer-a")
                .expect("producer id"),
            boot_session: diagnostics::BootSessionId::new("boot-a").expect("boot id"),
            channel: diagnostics::ChannelId::new("diagnostic-a").expect("channel id"),
        };
        let mut tracker = diagnostics::SequenceTracker::new(key.clone());
        let first = diagnostics::SequenceIdentity::new(key.clone(), 1).expect("sequence one");
        assert!(tracker.admit(&first).is_ok());
        let zero_replay = diagnostics::SequenceIdentity { key, sequence: 0 };
        assert_eq!(
            tracker.admit_replay(&zero_replay, u64::MAX),
            Err(diagnostics::ContractError::Sequence {
                fault: diagnostics::SequenceFault::InvalidStart
            })
        );
        assert!(matches!(
            tracker.acknowledge_durable(0),
            Err(diagnostics::ContractError::Sequence {
                fault: diagnostics::SequenceFault::InvalidStart
            })
        ));

        let legacy_offer: diagnostics::ProtocolOfferV1 = serde_json::from_slice(
            &read_fixture("diagnostic-protocol-offer-v1.0.json")
                .expect("protocol offer fixture must load"),
        )
        .expect("legacy protocol offer must decode");
        let offer = legacy_offer
            .into_v2()
            .expect("legacy protocol offer must migrate into v2");
        let unrelated_schema = diagnostics::SchemaIdentity::new(
            diagnostics::SchemaHashAlgorithm::Sha256,
            3,
            diagnostics::SchemaHash::new(
                "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            )
            .expect("canonical schema hash"),
        )
        .expect("schema identity");
        let current_offer: diagnostics::ProtocolOffer = serde_json::from_slice(
            &read_fixture("diagnostic-protocol-offer-v2.0.json")
                .expect("current protocol offer fixture must load"),
        )
        .expect("current target offer must decode");
        let strict_authority = diagnostics::SchemaCompatibilityAuthority::strict();
        assert!(matches!(
            offer.negotiate(&current_offer, strict_authority),
            Err(diagnostics::ContractError::SchemaIncompatible)
        ));
        assert!(matches!(
            current_offer.negotiate(&offer, strict_authority),
            Err(diagnostics::ContractError::SchemaIncompatible)
        ));
        let unrelated_offer =
            diagnostics::ProtocolOffer::new(offer.versions(), vec![unrelated_schema])
                .expect("unrelated target offer");
        assert!(matches!(
            offer.negotiate(&unrelated_offer, strict_authority),
            Err(diagnostics::ContractError::SchemaIncompatible)
        ));

        let mut envelope: serde_json::Value = serde_json::from_slice(
            &read_fixture("diagnostic-envelope-v1.2.json").expect("diagnostic fixture must load"),
        )
        .expect("diagnostic envelope fixture is JSON");
        envelope["undeclared_top_level"] = serde_json::json!(true);
        let encoded = serde_json::to_vec(&envelope).expect("mutated envelope serializes");
        assert!(
            diagnostics::decode_json_frame(&encoded, diagnostics::FrameCompleteness::Complete)
                .is_err()
        );
        assert_nested_envelope_unknown_fields_rejected();
        for fixture in [
            "diagnostic-protocol-offer-v1.0.json",
            "diagnostic-protocol-offer-v2.0.json",
        ] {
            let mut offer: serde_json::Value = serde_json::from_slice(
                &read_fixture(fixture).expect("protocol offer fixture must load"),
            )
            .expect("protocol offer fixture is JSON");
            offer["undeclared_nested"] = serde_json::json!(true);
            let encoded = serde_json::to_vec(&offer).expect("offer mutation serializes");
            if fixture.ends_with("v1.0.json") {
                assert!(serde_json::from_slice::<diagnostics::ProtocolOfferV1>(&encoded).is_err());
            } else {
                assert!(serde_json::from_slice::<diagnostics::ProtocolOffer>(&encoded).is_err());
            }
        }
    }

    fn assert_nested_envelope_unknown_fields_rejected() {
        for path in [
            ["sequence", "undeclared_nested"].as_slice(),
            ["descriptor", "undeclared_nested"].as_slice(),
            ["fields", "0", "undeclared_nested"].as_slice(),
        ] {
            let mut nested: serde_json::Value = serde_json::from_slice(
                &read_fixture("diagnostic-envelope-v1.2.json")
                    .expect("diagnostic fixture must load"),
            )
            .expect("diagnostic envelope fixture is JSON");
            if path[0] == "fields" {
                nested["fields"][0][path[2]] = serde_json::json!(true);
            } else {
                nested[path[0]][path[1]] = serde_json::json!(true);
            }
            let encoded = serde_json::to_vec(&nested).expect("nested mutation serializes");
            assert!(
                diagnostics::decode_json_frame(&encoded, diagnostics::FrameCompleteness::Complete)
                    .is_err(),
                "nested unknown field at {path:?} must fail closed"
            );
        }
    }

    fn public_boundary_rejects_durable_storage_counterexamples() {
        let payload = serde_json::json!({
            "kind": "fragment_verified",
            "body": {
                "sequence": 1,
                "bytes": 16,
                "checksum": "checksum",
                "undeclared_nested": true
            }
        });
        assert!(
            serde_json::from_value::<JournalPayload>(payload).is_err(),
            "JournalPayload unknown body field must fail closed"
        );

        let reconcile = serde_json::json!({
            "kind": "output_without_archive",
            "final_identity": "final_1",
            "undeclared_nested": true
        });
        assert!(
            serde_json::from_value::<ReconcileState>(reconcile).is_err(),
            "ReconcileState unknown variant field must fail closed"
        );

        let mut record: serde_json::Value = serde_json::from_slice(
            &read_fixture("durability-contracts-v1.0.json").expect("durability fixture must load"),
        )
        .expect("durability fixture is JSON");
        record["journal_record"]["undeclared_top_level"] = serde_json::json!(true);
        assert!(
            serde_json::from_value::<JournalRecord>(record["journal_record"].clone()).is_err(),
            "JournalRecord unknown top-level field must fail closed"
        );

        record["journal_record"]
            .as_object_mut()
            .expect("journal record fixture must be an object")
            .remove("undeclared_top_level");
        record["journal_record"]["sequence"] = serde_json::json!(0);
        assert!(
            serde_json::from_value::<JournalRecord>(record["journal_record"].clone()).is_err(),
            "JournalRecord sequence zero must fail closed"
        );
    }

    fn acknowledge_all_pending_effects(machine: &mut StateMachine) {
        let _events = acknowledge_all_pending_effects_recording(machine);
    }

    fn acknowledge_all_pending_effects_recording(machine: &mut StateMachine) -> Vec<Event> {
        let mut observed = false;
        let mut events = Vec::new();
        for _ in 0..64 {
            let pending = machine.pending_acknowledgements().to_vec();
            if pending.is_empty() {
                assert!(observed, "expected one or more pending effects");
                return events;
            }
            observed = true;
            for acknowledgement in pending {
                let event = Event::EffectAcknowledged {
                    instance_id: acknowledgement.instance_id,
                    effect: acknowledgement.effect,
                    generation: acknowledgement.generation,
                };
                assert!(
                    machine.apply(event).is_ok(),
                    "pending effect {:?} must acknowledge through its exact token; machine={machine:?}",
                    acknowledgement.effect,
                );
                events.push(event);
            }
        }
        panic!("effect acknowledgement chain exceeded the bounded public-test guard");
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

    #[allow(clippy::too_many_lines)]
    fn public_boundary_requires_every_emitted_effect_to_clear_before_durability() {
        #[derive(Clone, Copy)]
        enum ProofStep {
            Apply(Event),
            AcknowledgeByteDurability(Event, ByteCreditPosition),
        }

        let kinds = [
            MachineKind::JobCancellation,
            MachineKind::SourceRedirect,
            MachineKind::AtomicAdmission,
            MachineKind::ByteCreditDurability,
            MachineKind::Live,
            MachineKind::Sink,
            MachineKind::Ffmpeg,
            MachineKind::JavascriptWorker,
            MachineKind::PluginIpc,
            MachineKind::CommitArchiveReconciliation,
            MachineKind::FilesystemCapability,
            MachineKind::Watcher,
        ];
        let events = [
            Event::Start,
            Event::Assign,
            Event::Admit,
            Event::Receive,
            Event::Validate,
            Event::PersistDurably,
            Event::Redirect,
            Event::Continue,
            Event::Ready,
            Event::Serve,
            Event::Refresh,
            Event::Drain,
            Event::Spawn,
            Event::Reap,
            Event::Recycle,
            Event::Quarantine,
            Event::Prepare,
            Event::Rename,
            Event::Archive,
            Event::Cleanup,
            Event::Reconcile,
            Event::Probe,
            Event::Confine,
            Event::Degrade,
            Event::MarkStale,
            Event::Reject,
            Event::Release,
            Event::Complete,
            Event::Fail,
            Event::Cancel,
            Event::Restart,
        ];
        for (index, kind) in kinds.into_iter().enumerate() {
            let instance = MachineInstanceId::new(10_000 + u64::try_from(index).expect("index"))
                .expect("nonzero instance");
            let mut frontier = vec![Vec::<ProofStep>::new()];
            let mut visited = BTreeSet::new();
            let mut observed_effect = false;
            for _ in 0..12 {
                let mut next_frontier = Vec::new();
                for path in frontier {
                    for event in events {
                        let mut next = StateMachine::new(kind, instance, 96);
                        let broker = byte_credit_broker(32, 4)
                            .expect("bounded byte durability broker must construct");
                        let mut receive_claim = broker
                            .claim(OwnerId(1), ByteCreditStage::HttpReceive, 16)
                            .expect("bounded receive proof claim must issue");
                        receive_claim
                            .consume(ByteCreditComponent::Single, 16)
                            .expect("bounded receive proof bytes must be consumed");
                        let mut writer_claim = broker
                            .claim(OwnerId(1), ByteCreditStage::Writer, 16)
                            .expect("bounded writer proof claim must issue");
                        writer_claim
                            .consume(ByteCreditComponent::Single, 16)
                            .expect("bounded writer proof bytes must be consumed");
                        for prior_step in &path {
                            match prior_step {
                                ProofStep::Apply(prior_event) => {
                                    assert!(next.apply(*prior_event).is_ok());
                                }
                                ProofStep::AcknowledgeByteDurability(prior_event, position) => {
                                    next.acknowledge_byte_durability_effect(
                                        &broker,
                                        *prior_event,
                                        *position,
                                    )
                                    .expect("recorded byte durability acknowledgement must replay");
                                }
                            }
                        }
                        if next.apply(event).is_err() {
                            continue;
                        }
                        let mut acknowledgement_steps = Vec::new();
                        if !next.pending_acknowledgements().is_empty() {
                            observed_effect = true;
                            assert!(
                                !durable_states(kind).contains(&next.state()),
                                "{kind:?} reached durable state {:?} with pending effects {:?}",
                                next.state(),
                                next.pending_acknowledgements()
                            );
                            if kind == MachineKind::ByteCreditDurability
                                && matches!(
                                    next.pending_acknowledgements()[0].effect,
                                    EffectIntent::AcceptBoundedBytes
                                        | EffectIntent::ValidateAndWrite
                                        | EffectIntent::SynchronizeData
                                )
                            {
                                assert_eq!(next.pending_acknowledgements().len(), 1);
                                let acknowledgement = next.pending_acknowledgements()[0];
                                let acknowledgement_event = Event::EffectAcknowledged {
                                    instance_id: acknowledgement.instance_id,
                                    effect: acknowledgement.effect,
                                    generation: acknowledgement.generation,
                                };
                                let current = broker.position();
                                let position = match acknowledgement.effect {
                                    EffectIntent::AcceptBoundedBytes => ByteCreditPosition {
                                        received: current.received.saturating_add(1),
                                        ..current
                                    },
                                    EffectIntent::ValidateAndWrite => ByteCreditPosition {
                                        validated_written_contiguous: current.received,
                                        ..current
                                    },
                                    EffectIntent::SynchronizeData => ByteCreditPosition {
                                        durable_contiguous: current.validated_written_contiguous,
                                        ..current
                                    },
                                    _ => unreachable!("special byte effect match is exhaustive"),
                                };
                                next.acknowledge_byte_durability_effect(
                                    &broker,
                                    acknowledgement_event,
                                    position,
                                )
                                .expect(
                                    "byte durability effect must clear through broker boundary",
                                );
                                acknowledgement_steps.push(ProofStep::AcknowledgeByteDurability(
                                    acknowledgement_event,
                                    position,
                                ));
                            } else {
                                acknowledgement_steps.extend(
                                    acknowledge_all_pending_effects_recording(&mut next)
                                        .into_iter()
                                        .map(ProofStep::Apply),
                                );
                            }
                        }
                        let key =
                            format!("{:?}:{:?}", next.state(), next.pending_acknowledgements());
                        if visited.insert(key) {
                            let mut next_path = path.clone();
                            next_path.push(ProofStep::Apply(event));
                            next_path.extend(acknowledgement_steps);
                            next_frontier.push(next_path);
                        }
                    }
                }
                frontier = next_frontier;
                if frontier.is_empty() {
                    break;
                }
            }
            assert!(
                observed_effect,
                "{kind:?} emitted no discoverable required effect"
            );
        }
    }

    #[test]
    fn canonical_public_contract_fixtures_decode_and_validate() {
        let acquisition_sink: serde_json::Value = serde_json::from_slice(
            &read_fixture("acquisition-sink-v1.0.json")
                .expect("acquisition and sink fixture must load"),
        )
        .expect("acquisition and sink fixture is JSON");
        let acquisition: AcquisitionSource =
            serde_json::from_value(acquisition_sink["source"].clone())
                .expect("acquisition source must decode");
        assert!(matches!(
            &acquisition,
            AcquisitionSource::DirectUrl { url } if !url.is_empty()
        ));
        let sink: OutputSinkSpec = serde_json::from_value(acquisition_sink["sink"].clone())
            .expect("output sink must decode");
        assert!(matches!(
            &sink,
            OutputSinkSpec::AtomicFile { rooted_path }
                if !rooted_path.is_empty()
                    && !Path::new(rooted_path).is_absolute()
                    && !Path::new(rooted_path).components().any(|component| matches!(
                        component,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    ))
        ));
        let semantics: SinkSemantics =
            serde_json::from_value(acquisition_sink["semantics"].clone())
                .expect("sink semantics must decode");
        assert_eq!(semantics.backpressure, BackpressureMode::BlockProducer);
        assert!(semantics.seekable && semantics.atomic);
        assert!(!semantics.postprocessing_requires_seekable_temporary);

        let config: ConfigEnvelope = serde_json::from_slice(
            &read_fixture("config-envelope-v1.0.json").expect("config fixture must load"),
        )
        .expect("config fixture must decode");
        assert!(config.header.validate(ProtocolLimits::default()).is_ok());
        assert!(config.compatibility.check(config.header.version).is_ok());
        assert!(config.values.validate(ExtensionLimits::default()).is_ok());

        let event: EventEnvelope = serde_json::from_slice(
            &read_fixture("event-envelope-v1.0.json").expect("event fixture must load"),
        )
        .expect("event fixture must decode");
        assert!(
            ProcessEnvelope::Event(event.clone())
                .validate(ProtocolLimits::default())
                .is_ok()
        );

        let error: ErrorEnvelope = serde_json::from_slice(
            &read_fixture("error-envelope-v1.0.json").expect("error fixture must load"),
        )
        .expect("error fixture must decode");
        assert!(
            ProcessEnvelope::Error(error.clone())
                .validate(ProtocolLimits::default())
                .is_ok()
        );

        let cancellation: serde_json::Value = serde_json::from_slice(
            &read_fixture("cancellation-v1.0.json").expect("cancellation fixture must load"),
        )
        .expect("cancellation fixture is JSON");
        let request: CancellationRequest = serde_json::from_value(cancellation["request"].clone())
            .expect("cancellation request must decode");
        let acknowledgement: CancellationAcknowledgement =
            serde_json::from_value(cancellation["acknowledgement"].clone())
                .expect("cancellation acknowledgement must decode");
        assert!(
            ProcessEnvelope::Cancel(request.clone())
                .validate(ProtocolLimits::default())
                .is_ok()
        );
        assert!(
            ProcessEnvelope::CancelAcknowledged(acknowledgement.clone())
                .validate(ProtocolLimits::default())
                .is_ok()
        );
        assert_eq!(request.target_request_id, acknowledgement.target_request_id);
        assert_eq!(request.generation, acknowledgement.generation);
        assert!(acknowledgement.header.sequence > request.header.sequence);

        let encoded = serde_json::to_vec(&(
            acquisition,
            sink,
            semantics,
            config,
            event,
            error,
            request,
            acknowledgement,
        ))
        .expect("canonical public contracts must serialize");
        assert!(!encoded.is_empty());
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn lifecycle_fixture_executes_success_failure_cancel_restart_and_invalid_paths() {
        let fixture: serde_json::Value = serde_json::from_slice(
            &read_fixture("lifecycle-scenarios-v1.0.json").expect("lifecycle fixture must load"),
        )
        .expect("lifecycle fixture must be JSON");
        let scenarios = fixture["scenarios"]
            .as_array()
            .expect("scenarios are required");
        let all_machines = [
            "JobCancellation",
            "SourceRedirect",
            "AtomicAdmission",
            "ByteCreditDurability",
            "Live",
            "Sink",
            "Ffmpeg",
            "JavascriptWorker",
            "PluginIpc",
            "CommitArchiveReconciliation",
            "FilesystemCapability",
            "Watcher",
        ];
        let restart_machines = [
            "JobCancellation",
            "SourceRedirect",
            "ByteCreditDurability",
            "Live",
            "Ffmpeg",
            "JavascriptWorker",
            "PluginIpc",
            "CommitArchiveReconciliation",
            "FilesystemCapability",
            "Watcher",
        ];
        let failure_machines = [
            "JobCancellation",
            "SourceRedirect",
            "ByteCreditDurability",
            "Live",
            "Sink",
            "Ffmpeg",
            "JavascriptWorker",
            "PluginIpc",
            "CommitArchiveReconciliation",
            "FilesystemCapability",
            "Watcher",
        ];
        let mut coverage = BTreeSet::new();
        for scenario in scenarios {
            let machine_name = scenario["machine_kind"].as_str().unwrap_or_default();
            let case = scenario["case"]
                .as_str()
                .expect("scenario case is required");
            let kind = machine_kind(machine_name).expect("registered machine kind");
            let events = scenario["events"].as_array().expect("events are required");
            let instance_id = MachineInstanceId::new(1).expect("fixture instance is nonzero");
            let byte_broker = byte_credit_broker(32, 4)
                .expect("bounded lifecycle fixture byte broker must construct");
            let mut receive_claim = byte_broker
                .claim(OwnerId(1), ByteCreditStage::HttpReceive, 16)
                .expect("bounded lifecycle fixture receive claim must issue");
            receive_claim
                .consume(ByteCreditComponent::Single, 16)
                .expect("bounded lifecycle fixture receive bytes must be consumed");
            let mut writer_claim = byte_broker
                .claim(OwnerId(1), ByteCreditStage::Writer, 16)
                .expect("bounded lifecycle fixture writer claim must issue");
            writer_claim
                .consume(ByteCreditComponent::Single, 16)
                .expect("bounded lifecycle fixture writer bytes must be consumed");
            let mut model = StateMachine::new(
                kind,
                instance_id,
                events.len().saturating_mul(2).saturating_add(2),
            );
            for event in events {
                let event_name = event.as_str().unwrap_or_default();
                if matches!(event_name, "EffectFailed" | "EffectCancelled") {
                    let acknowledgement = model.pending_acknowledgements()[0];
                    let outcome = if event_name == "EffectFailed" {
                        Event::EffectFailed {
                            instance_id: acknowledgement.instance_id,
                            effect: acknowledgement.effect,
                            generation: acknowledgement.generation,
                        }
                    } else {
                        Event::EffectCancelled {
                            instance_id: acknowledgement.instance_id,
                            effect: acknowledgement.effect,
                            generation: acknowledgement.generation,
                        }
                    };
                    assert!(model.apply(outcome).is_ok());
                    continue;
                }
                let event = lifecycle_event(event_name).expect("registered lifecycle event");
                if event == Event::Acknowledge {
                    assert!(
                        !model.pending_acknowledgements().is_empty(),
                        "fixture acknowledgement requires an effect: case={case}, machine={machine_name}, state={:?}",
                        model.state()
                    );
                    acknowledge_pending(
                        &mut model,
                        &byte_broker,
                        kind == MachineKind::ByteCreditDurability,
                    );
                } else {
                    assert!(
                        model.apply(event).is_ok(),
                        "fixture event must be legal: case={case}, machine={machine_name}, event={event_name}, state={:?}",
                        model.state()
                    );
                }
            }
            if case == "invalid" {
                let prior_state = model.state();
                let prior_trace_length = model.trace().len();
                let invalid = lifecycle_event(
                    scenario["invalid_event"]
                        .as_str()
                        .expect("invalid event is required"),
                )
                .expect("registered invalid lifecycle event");
                assert!(matches!(
                    model.apply(invalid),
                    Err(TransitionError::InvalidTransition { .. })
                ));
                assert_eq!(model.state(), prior_state);
                assert_eq!(model.trace().len(), prior_trace_length);
                assert_eq!(scenario["expected_error"], "InvalidTransition");
            }
            assert_eq!(
                format!("{:?}", model.state()),
                scenario["expected_state"].as_str().unwrap_or_default()
            );
            assert!(coverage.insert((case.to_owned(), machine_name.to_owned())));
        }
        for machine in all_machines {
            for case in ["success", "cancel", "invalid"] {
                assert!(
                    coverage.contains(&(case.to_owned(), machine.to_owned())),
                    "{machine} omits {case} coverage"
                );
            }
        }
        for machine in failure_machines {
            assert!(
                coverage.contains(&("failure".to_owned(), machine.to_owned())),
                "{machine} omits applicable failure coverage"
            );
        }
        for machine in restart_machines {
            assert!(
                coverage.contains(&("restart".to_owned(), machine.to_owned())),
                "{machine} omits applicable restart coverage"
            );
        }
    }

    #[test]
    fn resource_fixture_executes_all_dimensions_and_byte_credit_release() {
        let fixture: serde_json::Value = serde_json::from_slice(
            &read_fixture("resource-boundary-scenario.json").expect("resource fixture must load"),
        )
        .expect("resource fixture must be JSON");
        let capacity = resource_vector(&fixture["capacity"]);
        let contract = ResourceContractV1::new(capacity, 1, 1, 2, u64::MAX);
        let broker = OwnedResourceBroker::from_contract(&contract).expect("valid contract");
        let requests = fixture["requests"]
            .as_array()
            .expect("requests are required");
        let first = resource_vector(&requests[0]);
        let second = resource_vector(&requests[1]);
        assert_eq!(
            first, capacity,
            "fixture must exercise every exact-capacity dimension"
        );
        let lease = match broker.request(OwnerId(1), first).expect("first request") {
            OwnedAdmission::Granted(lease) => lease,
            OwnedAdmission::Queued(_) => panic!("exact-capacity request must be granted"),
        };
        let mut waiter = match broker.request(OwnerId(2), second).expect("second request") {
            OwnedAdmission::Queued(waiter) => waiter,
            OwnedAdmission::Granted(_) => panic!("second request must queue"),
        };
        assert!(broker.verify().is_ok());
        lease.release().expect("owned lease must release");
        let promoted = waiter
            .try_acquire()
            .expect("owned waiter lookup")
            .expect("released capacity must promote the FIFO waiter");
        drop(promoted);
        assert_eq!(broker.in_use(), ResourceVector::default());
        assert!(broker.verify().is_ok());

        let credit_fixture = &fixture["byte_credit"];
        let capacity = credit_fixture["capacity"]
            .as_u64()
            .expect("credit capacity");
        let claim_bytes = credit_fixture["claim_bytes"].as_u64().expect("claim bytes");
        let received = credit_fixture["received_before_release"]
            .as_u64()
            .expect("received bytes");
        let credits = OwnedByteCreditBroker::from_contract(&ByteCreditContractV1::new(capacity, 1))
            .expect("valid byte-credit contract");
        let mut claim = credits
            .claim(OwnerId(1), ByteCreditStage::HttpReceive, claim_bytes)
            .expect("claim must fit");
        assert!(claim.consume(ByteCreditComponent::Single, received).is_ok());
        assert!(matches!(
            claim.consume(
                ByteCreditComponent::Single,
                claim_bytes.saturating_sub(received).saturating_add(1)
            ),
            Err(CreditError::UncreditedBytes { .. })
        ));
        claim
            .release()
            .expect("owned byte-credit lease must release");
        assert_eq!(credits.global_occupancy(), Ok((0, 0)));
        assert!(credits.verify().is_ok());
    }

    fn acknowledge_pending(
        model: &mut StateMachine,
        byte_broker: &OwnedByteCreditBroker,
        byte_durability: bool,
    ) {
        let mut observed = false;
        for _ in 0..64 {
            let pending = model.pending_acknowledgements().to_vec();
            if pending.is_empty() {
                assert!(observed, "fixture acknowledgement requires an effect");
                return;
            }
            observed = true;
            for acknowledgement in pending {
                let event = Event::EffectAcknowledged {
                    instance_id: acknowledgement.instance_id,
                    effect: acknowledgement.effect,
                    generation: acknowledgement.generation,
                };
                if byte_durability
                    && matches!(
                        acknowledgement.effect,
                        EffectIntent::AcceptBoundedBytes
                            | EffectIntent::ValidateAndWrite
                            | EffectIntent::SynchronizeData
                    )
                {
                    let current = byte_broker.position();
                    let position = match acknowledgement.effect {
                        EffectIntent::AcceptBoundedBytes => ByteCreditPosition {
                            received: current.received.saturating_add(1),
                            ..current
                        },
                        EffectIntent::ValidateAndWrite => ByteCreditPosition {
                            validated_written_contiguous: current.received,
                            ..current
                        },
                        EffectIntent::SynchronizeData => ByteCreditPosition {
                            durable_contiguous: current.validated_written_contiguous,
                            ..current
                        },
                        _ => unreachable!("special byte effect match is exhaustive"),
                    };
                    model
                        .acknowledge_byte_durability_effect(byte_broker, event, position)
                        .expect("fixture byte durability effect must acknowledge through broker");
                } else {
                    assert!(model.apply(event).is_ok());
                }
            }
        }
        panic!("fixture acknowledgement chain exceeded its bounded guard");
    }

    fn resource_vector(value: &serde_json::Value) -> ResourceVector {
        ResourceVector {
            metadata_requests: u32::try_from(
                value["metadata_requests"].as_u64().unwrap_or_default(),
            )
            .expect("metadata requests fit u32"),
            media_requests: u32::try_from(value["media_requests"].as_u64().unwrap_or_default())
                .expect("media requests fit u32"),
            memory_bytes: value["memory_bytes"].as_u64().expect("memory bytes"),
            disk_read_bytes_in_flight: value["disk_read_bytes_in_flight"]
                .as_u64()
                .expect("disk read bytes"),
            disk_write_bytes_in_flight: value["disk_write_bytes_in_flight"]
                .as_u64()
                .expect("disk write bytes"),
            open_handles: u32::try_from(value["open_handles"].as_u64().unwrap_or_default())
                .expect("open handles fit u32"),
            cpu_light_slots: u32::try_from(value["cpu_light_slots"].as_u64().unwrap_or_default())
                .expect("light slots fit u32"),
            cpu_heavy_slots: u32::try_from(value["cpu_heavy_slots"].as_u64().unwrap_or_default())
                .expect("heavy slots fit u32"),
            javascript_workers: u32::try_from(
                value["javascript_workers"].as_u64().unwrap_or_default(),
            )
            .expect("javascript workers fit u32"),
            ffmpeg_processes: u32::try_from(value["ffmpeg_processes"].as_u64().unwrap_or_default())
                .expect("ffmpeg processes fit u32"),
            ffmpeg_cpu_threads: u32::try_from(
                value["ffmpeg_cpu_threads"].as_u64().unwrap_or_default(),
            )
            .expect("ffmpeg threads fit u32"),
            archive_writer_slots: u32::try_from(
                value["archive_writer_slots"].as_u64().unwrap_or_default(),
            )
            .expect("archive slots fit u32"),
            sink_bytes: value["sink_bytes"].as_u64().expect("sink bytes"),
        }
    }

    fn machine_kind(value: &str) -> Option<MachineKind> {
        Some(match value {
            "JobCancellation" => MachineKind::JobCancellation,
            "SourceRedirect" => MachineKind::SourceRedirect,
            "AtomicAdmission" => MachineKind::AtomicAdmission,
            "ByteCreditDurability" => MachineKind::ByteCreditDurability,
            "Live" => MachineKind::Live,
            "Sink" => MachineKind::Sink,
            "Ffmpeg" => MachineKind::Ffmpeg,
            "JavascriptWorker" => MachineKind::JavascriptWorker,
            "PluginIpc" => MachineKind::PluginIpc,
            "CommitArchiveReconciliation" => MachineKind::CommitArchiveReconciliation,
            "FilesystemCapability" => MachineKind::FilesystemCapability,
            "Watcher" => MachineKind::Watcher,
            _ => return None,
        })
    }

    fn lifecycle_event(value: &str) -> Option<Event> {
        Some(match value {
            "Start" => Event::Start,
            "Assign" => Event::Assign,
            "Admit" => Event::Admit,
            "Receive" => Event::Receive,
            "Validate" => Event::Validate,
            "PersistDurably" => Event::PersistDurably,
            "Redirect" => Event::Redirect,
            "Continue" => Event::Continue,
            "Ready" => Event::Ready,
            "Serve" => Event::Serve,
            "Refresh" => Event::Refresh,
            "Drain" => Event::Drain,
            "Spawn" => Event::Spawn,
            "Reap" => Event::Reap,
            "Acknowledge" => Event::Acknowledge,
            "Recycle" => Event::Recycle,
            "Quarantine" => Event::Quarantine,
            "Prepare" => Event::Prepare,
            "Rename" => Event::Rename,
            "Archive" => Event::Archive,
            "Cleanup" => Event::Cleanup,
            "Reconcile" => Event::Reconcile,
            "Probe" => Event::Probe,
            "Confine" => Event::Confine,
            "Degrade" => Event::Degrade,
            "MarkStale" => Event::MarkStale,
            "Reject" => Event::Reject,
            "Release" => Event::Release,
            "Complete" => Event::Complete,
            "Fail" => Event::Fail,
            "Cancel" => Event::Cancel,
            "Restart" => Event::Restart,
            _ => return None,
        })
    }
}
