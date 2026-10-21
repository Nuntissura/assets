//! Non-product equivalent-work admission. Supplied timings never prove product speed.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const LIMIT: usize = 4 * 1024 * 1024;
const MAX_SAMPLES: usize = 8192;
const MAX_DURATION_NS: u64 = 3_600_000_000_000;
const DIMENSIONS: [&str; 11] = [
    "source_graph", "request_sequence", "bytes_and_fragments", "selected_streams",
    "output_and_ffprobe_facts", "metadata_and_sidecars", "archive_action", "validation",
    "failure_class", "cache_and_network_state", "process_topology",
];
const METRICS: [&str; 18] = ["wall_ns", "user_cpu_ns", "kernel_cpu_ns", "peak_rss_bytes",
    "allocation_count", "allocation_bytes", "context_switches", "tasks_created", "files_opened",
    "bytes_written", "bytes_read", "journal_writes", "http_requests", "retries", "throughput_bytes_per_second",
    "first_progress_ns", "first_byte_ns", "ffmpeg_cpu_ns"];

fn error(code: &str) -> String { format!("FF-BENCH-E-{code}") }

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Artifact {
    path: String,
    sha256: String,
    bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BuildIdentity {
    source_commit: String,
    source_tree: String,
    artifact: Artifact,
    rustc: String,
    cargo: String,
    profile: String,
    features: Vec<String>,
    allocator: String,
    dependency_decisions: BTreeMap<String, String>,
    lock_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "availability", deny_unknown_fields)]
enum Tool {
    Available { version: String, artifact: Artifact },
    Unavailable { reason: String },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Topology {
    processes: Vec<ProcessRole>,
    input_urls_per_invocation: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ProcessRole {
    role: String,
    parent: String,
    artifact_role: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_id: String,
    run_id: String,
    baseline: BuildIdentity,
    candidate: BuildIdentity,
    optional_comparison: Tool,
    ffmpeg: Tool,
    ffprobe: Tool,
    workers: BTreeMap<String, Tool>,
    transport_identity: String,
    archive_identity: String,
    challenge_identity: String,
    corpus: Artifact,
    profiles: Vec<String>,
    active_profile_id: String,
    topology: Topology,
    required_dimensions: Vec<String>,
    sampling: Sampling,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Sampling {
    independent_clusters: u32,
    pairs_per_cluster: u32,
    warmup_pairs: u32,
    random_seed: u64,
    bootstrap_resamples: u32,
    confidence_percent: u32,
    maximum_relative_noise_ppm: u32,
    maximum_outlier_ppm: u32,
    // Threshold must be justified against the supplied independent baseline calibration.
    threshold_calibration_sha256: String,
    maximum_candidate_baseline_ratio_ppm: u32,
    minimum_improvement_ppm: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "availability", deny_unknown_fields)]
enum ObservationValue {
    Observed { value: String },
    Unavailable { reason: String },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct HostProfile {
    schema_id: String,
    id: String,
    platform: String,
    machine: ObservationValue,
    os: ObservationValue,
    filesystem: ObservationValue,
    storage: ObservationValue,
    security: ObservationValue,
    power_governor: ObservationValue,
    affinity: ObservationValue,
    memory_pressure: ObservationValue,
    filesystem_cache: ObservationValue,
    dns_cache: ObservationValue,
    network_replay: ObservationValue,
    preparation: Vec<String>,
    cleanup: Vec<String>,
    preparation_deadline_ms: u32,
    cleanup_deadline_ms: u32,
    metrics: BTreeMap<String, ObservationValue>,
    baseline_calibration: Artifact,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Calibration {
    schema_id: String,
    profile_id: String,
    source_identity: String,
    origin: SampleOrigin,
    elapsed_ns: Vec<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Dimension {
    name: String,
    // Concrete normalized evidence is content-bound and independently read, not a PASS label.
    artifacts: Vec<Artifact>,
    facts: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct NormalizedRecord {
    schema_id: String,
    dimension: String,
    facts: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Observation {
    schema_id: String,
    id: String,
    manifest_sha256: String,
    profile_id: String,
    build_side: Side,
    build_artifact_sha256: String,
    topology: Topology,
    dimensions: Vec<Dimension>,
    proof_class: ProofClass,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum ProofClass { Structural, Semantic, Integration, ProductionRuntime }

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "availability", deny_unknown_fields)]
enum Metric {
    Measured { value: u64 },
    Unavailable { reason: String },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProcessCosts {
    total_system_cpu_ns: Metric,
    ferric_cpu_ns: Metric,
    ffmpeg_cpu_ns: Metric,
    other_worker_cpu_ns: Metric,
    peak_rss_bytes: Metric,
    io_bytes: Metric,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Sample {
    id: String,
    manifest_sha256: String,
    cluster_id: u32,
    pair_index: u32,
    order_index: u32,
    warmup: bool,
    side: Side,
    observation_id: String,
    elapsed_ns: u64,
    origin: SampleOrigin,
    costs: ProcessCosts,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Side { Baseline, Candidate }

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SampleOrigin { SuppliedMeasurement, StatisticalFixture }

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Bundle {
    schema_id: String,
    manifest: Manifest,
    profiles: Vec<HostProfile>,
    observations: Vec<Observation>,
    samples: Vec<Sample>,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct Report {
    schema_id: &'static str,
    manifest_sha256: String,
    raw_samples_sha256: String,
    raw_samples: Vec<Sample>,
    admitted_observations: Vec<String>,
    proof_class: ProofClass,
    statistical_fixture: bool,
    independent_clusters: usize,
    statistic_metric: &'static str,
    paired_cluster_ratios: Vec<f64>,
    distribution: Distribution,
    confidence_95_lower: f64,
    confidence_95_upper: f64,
    candidate_baseline_ratio_bound_met: bool,
    requested_improvement_bound_met: bool,
    unavailable_metrics: BTreeSet<String>,
    product_progress: bool,
    performance_claim: bool,
    limitations: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct Distribution { minimum: f64, p25: f64, median: f64, p75: f64, maximum: f64 }

fn bounded(text: &str, maximum: usize) -> bool {
    !text.is_empty() && text.len() <= maximum && !text.chars().any(char::is_control)
}

fn hash(text: &str, length: usize) -> bool {
    text.len() == length && text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && text.bytes().any(|b| b != b'0')
}

fn relative(root: &Path, value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if path.is_absolute() || path.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
        return Err(error("PATH"));
    }
    let resolved = root.join(path).canonicalize().map_err(|e| error(&format!("PATH:{e}")))?;
    let canonical = root.canonicalize().map_err(|e| e.to_string())?;
    if !resolved.starts_with(canonical) { return Err(error("PATH-ESCAPE")); }
    Ok(resolved)
}

fn artifact(root: &Path, value: &Artifact) -> Result<(), String> {
    if !hash(&value.sha256, 64) || value.bytes > 128 * 1024 * 1024 { return Err(error("ARTIFACT-IDENTITY")); }
    let path = relative(root, &value.path)?;
    let metadata = fs::metadata(&path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() != value.bytes { return Err(error("ARTIFACT-LENGTH")); }
    if super::sha256_file(&path)? != value.sha256 { return Err(error("ARTIFACT-HASH")); }
    Ok(())
}

fn canonical_hash(value: &impl Serialize) -> Result<String, String> {
    Ok(super::encode_sha256(&serde_json::to_vec(value).map_err(|e| e.to_string())?))
}

fn build(root: &Path, value: &BuildIdentity) -> Result<(), String> {
    if !hash(&value.source_commit, 40) || !hash(&value.source_tree, 40)
        || !hash(&value.lock_sha256, 64) || !bounded(&value.rustc, 128)
        || !bounded(&value.cargo, 128) || !bounded(&value.profile, 64)
        || !bounded(&value.allocator, 128) || value.features.len() > 64
        || value.features.iter().any(|s| !bounded(s, 128))
        || value.features.iter().collect::<BTreeSet<_>>().len() != value.features.len()
        || value.dependency_decisions.is_empty() || value.dependency_decisions.len() > 32
        || value.dependency_decisions.iter().any(|(key, v)| !bounded(key, 128) || !bounded(v, 128))
    { return Err(error("BUILD-IDENTITY")); }
    artifact(root, &value.artifact)
}

fn tool(root: &Path, value: &Tool) -> Result<(), String> {
    match value {
        Tool::Available { version, artifact: pin } if bounded(version, 256) => artifact(root, pin),
        Tool::Unavailable { reason } if bounded(reason, 512) => Ok(()),
        _ => Err(error("TOOL-IDENTITY")),
    }
}

fn topology(value: &Topology) -> Result<(), String> {
    let roles = value.processes.iter().map(|p| p.role.as_str()).collect::<BTreeSet<_>>();
    if roles.is_empty() || roles.len() > 16 || roles.len() != value.processes.len()
        || value.input_urls_per_invocation == 0 || value.input_urls_per_invocation > 1024
        || value.processes.iter().any(|p| !bounded(&p.role, 64) || !bounded(&p.artifact_role, 64)
            || !(p.parent == "external" || (roles.contains(p.parent.as_str()) && p.parent != p.role)))
    { return Err(error("TOPOLOGY")); }
    for process in &value.processes {
        let mut seen = BTreeSet::new();
        let mut cursor = process;
        while cursor.parent != "external" {
            if !seen.insert(cursor.role.as_str()) { return Err(error("TOPOLOGY-CYCLE")); }
            cursor = value.processes.iter().find(|p| p.role == cursor.parent).ok_or_else(|| error("TOPOLOGY-PARENT"))?;
        }
    }
    Ok(())
}

fn host(root: &Path, profile: &HostProfile) -> Result<(), String> {
    if profile.schema_id != "ff.benchmark-host@1" || !bounded(&profile.id, 128)
        || !matches!(profile.platform.as_str(), "windows" | "unix")
        || profile.preparation.is_empty() || profile.preparation.len() > 32
        || profile.cleanup.is_empty() || profile.cleanup.len() > 32
        || profile.preparation.iter().chain(&profile.cleanup).any(|v| !bounded(v, 512))
        || !(1..=30_000).contains(&profile.preparation_deadline_ms)
        || !(1..=30_000).contains(&profile.cleanup_deadline_ms)
        || profile.metrics.keys().map(String::as_str).collect::<BTreeSet<_>>() != BTreeSet::from(METRICS)
    { return Err(error("HOST-PROFILE")); }
    let fields = [&profile.machine, &profile.os, &profile.filesystem, &profile.storage,
        &profile.security, &profile.power_governor, &profile.affinity, &profile.memory_pressure,
        &profile.filesystem_cache, &profile.dns_cache, &profile.network_replay];
    for field in fields.into_iter().chain(profile.metrics.values()) {
        let text = match field { ObservationValue::Observed { value } => value,
            ObservationValue::Unavailable { reason } => reason };
        if !bounded(text, 1024) { return Err(error("HOST-OBSERVATION")); }
    }
    artifact(root, &profile.baseline_calibration)?;
    let calibration: Calibration = serde_json::from_slice(&fs::read(relative(root, &profile.baseline_calibration.path)?).map_err(|e| e.to_string())?)
        .map_err(|e| error(&format!("CALIBRATION-SCHEMA:{e}")))?;
    if calibration.schema_id != "ff.benchmark-calibration@1" || calibration.profile_id != profile.id
        || !bounded(&calibration.source_identity, 256) || !(50..=512).contains(&calibration.elapsed_ns.len())
        || calibration.elapsed_ns.iter().any(|t| *t == 0 || *t > MAX_DURATION_NS)
    { return Err(error("CALIBRATION")); }
    Ok(())
}

fn validate_manifest(root: &Path, bundle: &Bundle) -> Result<String, String> {
    let m = &bundle.manifest;
    if bundle.schema_id != "ff.benchmark-bundle@1" || m.schema_id != "ff.benchmark-manifest@1"
        || !bounded(&m.run_id, 128) || bundle.samples.len() > MAX_SAMPLES
        || bundle.observations.is_empty() || bundle.observations.len() > MAX_SAMPLES
        || m.required_dimensions.iter().map(String::as_str).collect::<BTreeSet<_>>() != BTreeSet::from(DIMENSIONS)
        || m.required_dimensions.len() != DIMENSIONS.len()
        || [&m.transport_identity, &m.archive_identity, &m.challenge_identity].iter().any(|s| !bounded(s, 256))
    { return Err(error("MANIFEST")); }
    build(root, &m.baseline)?; build(root, &m.candidate)?;
    tool(root, &m.optional_comparison)?; tool(root, &m.ffmpeg)?; tool(root, &m.ffprobe)?;
    if m.workers.len() > 16 { return Err(error("WORKER-BOUND")); }
    for (name, pin) in &m.workers { if !bounded(name, 64) { return Err(error("WORKER-IDENTITY")); } tool(root, pin)?; }
    artifact(root, &m.corpus)?; topology(&m.topology)?;
    if m.topology.processes.iter().filter(|p| p.artifact_role == "primary").count() != 1
        || m.topology.processes.iter().any(|p| p.artifact_role != "primary" && p.artifact_role != "ffmpeg" && !m.workers.contains_key(&p.artifact_role))
        || m.topology.processes.iter().any(|p| (p.artifact_role == "ffmpeg" && matches!(m.ffmpeg, Tool::Unavailable { .. }))
            || m.workers.get(&p.artifact_role).is_some_and(|tool| matches!(tool, Tool::Unavailable { .. })))
    { return Err(error("TOPOLOGY-ARTIFACT")); }
    let ids = bundle.profiles.iter().map(|p| p.id.clone()).collect::<BTreeSet<_>>();
    if ids.len() != bundle.profiles.len() || ids != m.profiles.iter().cloned().collect()
        || m.profiles.len() != ids.len() || ids.len() != 2 || !ids.contains(&m.active_profile_id)
        || bundle.profiles.iter().map(|p| p.platform.as_str()).collect::<BTreeSet<_>>() != BTreeSet::from(["windows", "unix"])
    { return Err(error("HOST-IDENTITIES")); }
    for profile in &bundle.profiles { host(root, profile)?; }
    let p = &m.sampling;
    if !(50..=512).contains(&p.independent_clusters) || !(1..=4).contains(&p.pairs_per_cluster)
        || !(3..=32).contains(&p.warmup_pairs) || p.random_seed == 0
        || !(2_000..=20_000).contains(&p.bootstrap_resamples) || p.confidence_percent != 95
        || !(1..=500_000).contains(&p.maximum_relative_noise_ppm)
        || p.maximum_outlier_ppm > 100_000 || !hash(&p.threshold_calibration_sha256, 64)
        || !(1_000_000..=1_500_000).contains(&p.maximum_candidate_baseline_ratio_ppm)
        || p.minimum_improvement_ppm > 500_000
        || !bundle.profiles.iter().any(|profile| profile.id == m.active_profile_id && profile.baseline_calibration.sha256 == p.threshold_calibration_sha256)
    { return Err(error("STATISTICS-POLICY")); }
    let active = bundle.profiles.iter().find(|profile| profile.id == m.active_profile_id).ok_or_else(|| error("ACTIVE-HOST"))?;
    let calibration: Calibration = serde_json::from_slice(&fs::read(relative(root, &active.baseline_calibration.path)?).map_err(|e| e.to_string())?)
        .map_err(|e| error(&format!("CALIBRATION-SCHEMA:{e}")))?;
    let values = calibration.elapsed_ns.iter().copied().map(number).collect::<Vec<_>>();
    let d = distribution(&values);
    let relative_noise = (d.p75 - d.p25) / d.median;
    noise(&values, p.maximum_relative_noise_ppm, p.maximum_outlier_ppm)?;
    if f64::from(p.maximum_candidate_baseline_ratio_ppm - 1_000_000) / 1_000_000.0 > 3.0 * relative_noise {
        return Err(error("UNCALIBRATED-THRESHOLD"));
    }
    canonical_hash(m)
}

fn validate_observation(root: &Path, observation: &Observation, manifest: &Manifest, digest: &str) -> Result<(), String> {
    if observation.schema_id != "ff.benchmark-observation@1" || !bounded(&observation.id, 128)
        || observation.manifest_sha256 != digest || observation.profile_id != manifest.active_profile_id
        || observation.build_artifact_sha256 != match observation.build_side { Side::Baseline => &manifest.baseline.artifact.sha256, Side::Candidate => &manifest.candidate.artifact.sha256 }.as_str()
        || observation.topology != manifest.topology || observation.dimensions.len() != DIMENSIONS.len()
        || observation.dimensions.iter().map(|d| d.name.as_str()).collect::<BTreeSet<_>>() != BTreeSet::from(DIMENSIONS)
    { return Err(error("OBSERVATION-PROVENANCE")); }
    for dimension in &observation.dimensions {
        if dimension.facts.is_empty() || dimension.facts.len() > 128 || dimension.artifacts.is_empty()
            || dimension.artifacts.len() > 32 || dimension.facts.iter().any(|(k, v)| !bounded(k, 128) || !bounded(v, 4096))
        { return Err(error("CONCRETE-WORK-MISSING")); }
        for pin in &dimension.artifacts { artifact(root, pin)?; }
        if dimension.artifacts[0].bytes > u64::try_from(LIMIT).map_err(|_| error("LIMIT"))? { return Err(error("NORMALIZED-BOUND")); }
        let normalized: NormalizedRecord = serde_json::from_slice(&fs::read(relative(root, &dimension.artifacts[0].path)?).map_err(|e| e.to_string())?)
            .map_err(|e| error(&format!("NORMALIZED-SCHEMA:{e}")))?;
        if normalized.schema_id != "ff.benchmark-normalized-record@1" || normalized.dimension != dimension.name
            || normalized.facts != dimension.facts { return Err(error("NORMALIZED-FACTS")); }
    }
    Ok(())
}

fn admit(left: &Observation, right: &Observation) -> Result<(), String> {
    if left.profile_id != right.profile_id || left.topology != right.topology {
        return Err(error("UNEQUAL-HOST-OR-TOPOLOGY"));
    }
    for name in DIMENSIONS {
        let a = left.dimensions.iter().find(|d| d.name == name).ok_or_else(|| error("WORK-MISSING"))?;
        let b = right.dimensions.iter().find(|d| d.name == name).ok_or_else(|| error("WORK-MISSING"))?;
        // Evidence paths are capture locations, not work. Compare content and facts.
        let normalized = |d: &Dimension| d.artifacts.iter().map(|a| (a.sha256.clone(), a.bytes)).collect::<Vec<_>>();
        if a.facts != b.facts || normalized(a) != normalized(b) { return Err(error(&format!("UNEQUAL-WORK:{name}"))); }
    }
    Ok(())
}

#[derive(Debug)]
struct Random(u64);
impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12; self.0 ^= self.0 << 25; self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn index(&mut self, length: usize) -> Result<usize, String> {
        let length = u64::try_from(length).map_err(|_| error("INDEX"))?;
        let threshold = length.wrapping_neg() % length;
        for _ in 0..32 { let value = self.next(); if value >= threshold {
            return usize::try_from(value % length).map_err(|_| error("INDEX"));
        } }
        Err(error("RANDOM-BOUND"))
    }
}

fn number(value: u64) -> f64 {
    // All callers bound integral inputs below 2^53; split conversion is exact.
    f64::from(u32::try_from(value >> 32).unwrap_or(u32::MAX)) * 4_294_967_296.0
        + f64::from(u32::try_from(value & 0xffff_ffff).unwrap_or(u32::MAX))
}

fn percentile(sorted: &[f64], numerator: usize, denominator: usize) -> f64 {
    sorted[(sorted.len() - 1) * numerator / denominator]
}

fn distribution(values: &[f64]) -> Distribution {
    let mut sorted = values.to_vec(); sorted.sort_by(f64::total_cmp);
    Distribution { minimum: sorted[0], p25: percentile(&sorted, 1, 4), median: percentile(&sorted, 1, 2),
        p75: percentile(&sorted, 3, 4), maximum: sorted[sorted.len() - 1] }
}

fn noise(values: &[f64], maximum_ppm: u32, maximum_outliers: u32) -> Result<(), String> {
    let d = distribution(values);
    if d.median <= 0.0 || (d.p75 - d.p25) / d.median > f64::from(maximum_ppm) / 1_000_000.0 {
        return Err(error("NOISE"));
    }
    let spread = d.p75 - d.p25;
    let count = values.iter().filter(|v| **v < d.p25 - 3.0 * spread || **v > d.p75 + 3.0 * spread).count();
    if number(u64::try_from(count).map_err(|_| error("COUNT"))?) / number(u64::try_from(values.len()).map_err(|_| error("COUNT"))?)
        > f64::from(maximum_outliers) / 1_000_000.0 { return Err(error("OUTLIERS")); }
    Ok(())
}

fn metric(value: &Metric, name: &str, unavailable: &mut BTreeSet<String>) -> Result<(), String> {
    match value {
        Metric::Unavailable { reason } if bounded(reason, 512) => { unavailable.insert(format!("{name}:{reason}")); Ok(()) },
        Metric::Measured { value } if *value <= (1_u64 << 53) => Ok(()),
        _ => Err(error("METRIC")),
    }
}

fn evaluate(root: &Path, bundle: Bundle) -> Result<Report, String> {
    let manifest_sha256 = validate_manifest(root, &bundle)?;
    let mut observations = BTreeMap::new();
    let mut weakest = ProofClass::Semantic; // This validator does not execute product runtime.
    for observation in &bundle.observations {
        validate_observation(root, observation, &bundle.manifest, &manifest_sha256)?;
        if observations.insert(observation.id.as_str(), observation).is_some() { return Err(error("DUPLICATE-OBSERVATION")); }
        weakest = weakest.min(observation.proof_class);
    }
    let p = &bundle.manifest.sampling;
    let measured_pairs = p.independent_clusters.checked_mul(p.pairs_per_cluster).ok_or_else(|| error("SAMPLE-BOUND"))?;
    let expected = usize::try_from((measured_pairs + p.warmup_pairs) * 2).map_err(|_| error("SAMPLE-BOUND"))?;
    if bundle.samples.len() != expected { return Err(error("MISSING-RAW-SAMPLES")); }
    let mut sample_ids = BTreeSet::new();
    let mut pairs = BTreeMap::<u32, Vec<&Sample>>::new();
    let mut unavailable = BTreeSet::new();
    let mut statistical_fixture = false;
    for profile in &bundle.profiles {
        let calibration: Calibration = serde_json::from_slice(&fs::read(relative(root, &profile.baseline_calibration.path)?).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        statistical_fixture |= calibration.origin == SampleOrigin::StatisticalFixture;
        for (name, value) in &profile.metrics { if let ObservationValue::Unavailable { reason } = value {
            unavailable.insert(format!("{}:{name}:{reason}", profile.id));
        } }
        for (name, value) in [("machine", &profile.machine), ("os", &profile.os), ("filesystem", &profile.filesystem),
            ("storage", &profile.storage), ("security", &profile.security), ("power_governor", &profile.power_governor),
            ("affinity", &profile.affinity), ("memory_pressure", &profile.memory_pressure), ("filesystem_cache", &profile.filesystem_cache),
            ("dns_cache", &profile.dns_cache), ("network_replay", &profile.network_replay)] {
            if let ObservationValue::Unavailable { reason } = value { unavailable.insert(format!("{}:{name}:{reason}", profile.id)); }
        }
    }
    for sample in &bundle.samples {
        if !bounded(&sample.id, 128) || !sample_ids.insert(sample.id.as_str()) || sample.manifest_sha256 != manifest_sha256
            || sample.elapsed_ns == 0 || sample.elapsed_ns > MAX_DURATION_NS || sample.order_index > 1
            || !observations.contains_key(sample.observation_id.as_str())
        { return Err(error("SAMPLE-IDENTITY")); }
        if observations[sample.observation_id.as_str()].build_side != sample.side { return Err(error("SAMPLE-BUILD-SIDE")); }
        statistical_fixture |= sample.origin == SampleOrigin::StatisticalFixture;
        for (name, value) in [("total_system_cpu_ns", &sample.costs.total_system_cpu_ns),
            ("ferric_cpu_ns", &sample.costs.ferric_cpu_ns), ("ffmpeg_cpu_ns", &sample.costs.ffmpeg_cpu_ns),
            ("other_worker_cpu_ns", &sample.costs.other_worker_cpu_ns), ("peak_rss_bytes", &sample.costs.peak_rss_bytes),
            ("io_bytes", &sample.costs.io_bytes)] { metric(value, name, &mut unavailable)?; }
        pairs.entry(sample.pair_index).or_default().push(sample);
    }
    let mut schedule = Random(p.random_seed);
    let mut cluster_ratios = BTreeMap::<u32, Vec<f64>>::new();
    let mut baseline_times = Vec::new();
    let mut candidate_times = Vec::new();
    for pair_index in 0..(measured_pairs + p.warmup_pairs) {
        let pair = pairs.get(&pair_index).ok_or_else(|| error("PAIR-MISSING"))?;
        if pair.len() != 2 { return Err(error("PAIR-SIZE")); }
        let baseline = pair.iter().find(|s| s.side == Side::Baseline).ok_or_else(|| error("PAIR-SIDE"))?;
        let candidate = pair.iter().find(|s| s.side == Side::Candidate).ok_or_else(|| error("PAIR-SIDE"))?;
        let baseline_first = schedule.next() & 1 == 0;
        if baseline.order_index != u32::from(!baseline_first) || candidate.order_index != u32::from(baseline_first) {
            return Err(error("RANDOMIZED-PAIR-ORDER"));
        }
        let warmup = pair_index < p.warmup_pairs;
        let cluster = if warmup { 0 } else { (pair_index - p.warmup_pairs) / p.pairs_per_cluster + 1 };
        if pair.iter().any(|s| s.warmup != warmup || s.cluster_id != cluster) { return Err(error("CLUSTER-OR-WARMUP")); }
        // Hard predecessor: no raw timing reaches a reducer until concrete equality passes.
        admit(observations[baseline.observation_id.as_str()], observations[candidate.observation_id.as_str()])?;
        if !warmup {
            baseline_times.push(number(baseline.elapsed_ns)); candidate_times.push(number(candidate.elapsed_ns));
            cluster_ratios.entry(cluster).or_default().push(number(candidate.elapsed_ns) / number(baseline.elapsed_ns));
        }
    }
    noise(&baseline_times, p.maximum_relative_noise_ppm, p.maximum_outlier_ppm)?;
    noise(&candidate_times, p.maximum_relative_noise_ppm, p.maximum_outlier_ppm)?;
    let ratios = cluster_ratios.values().map(|values| values.iter().sum::<f64>() / number(u64::try_from(values.len()).unwrap_or(u64::MAX))).collect::<Vec<_>>();
    if ratios.len() != usize::try_from(p.independent_clusters).map_err(|_| error("CLUSTERS"))? { return Err(error("CLUSTERS")); }
    let mut bootstrap = Random(p.random_seed ^ 0xa076_1d64_78bd_642f);
    if bootstrap.0 == 0 { bootstrap.0 = 1; }
    let mut estimates = Vec::with_capacity(usize::try_from(p.bootstrap_resamples).map_err(|_| error("RESAMPLES"))?);
    for _ in 0..p.bootstrap_resamples {
        let mut sum = 0.0;
        for _ in 0..ratios.len() { sum += ratios[bootstrap.index(ratios.len())?]; }
        estimates.push(sum / number(u64::try_from(ratios.len()).map_err(|_| error("CLUSTERS"))?));
    }
    estimates.sort_by(f64::total_cmp);
    let lower = percentile(&estimates, 25, 1000);
    let upper = percentile(&estimates, 975, 1000);
    let ratio_gate = upper <= f64::from(p.maximum_candidate_baseline_ratio_ppm) / 1_000_000.0;
    let improvement_gate = p.minimum_improvement_ppm == 0
        || (1.0 - upper) >= f64::from(p.minimum_improvement_ppm) / 1_000_000.0;
    if !ratio_gate || !improvement_gate { return Err(error("CONFIDENCE-GATE-CROSSED")); }
    Ok(Report {
        schema_id: "ff.benchmark-admission-report@1", manifest_sha256,
        raw_samples_sha256: canonical_hash(&bundle.samples)?, raw_samples: bundle.samples,
        admitted_observations: observations.keys().map(|s| (*s).to_owned()).collect(),
        proof_class: weakest, statistical_fixture, independent_clusters: ratios.len(),
        statistic_metric: "paired_cluster_candidate_baseline_wall_clock_ratio",
        distribution: distribution(&ratios), paired_cluster_ratios: ratios,
        confidence_95_lower: lower, confidence_95_upper: upper,
        candidate_baseline_ratio_bound_met: ratio_gate, requested_improvement_bound_met: improvement_gate,
        unavailable_metrics: unavailable, product_progress: false, performance_claim: false,
        limitations: vec!["Supplied measurements are untrusted inputs; this boundary proves admission/statistical calculations, not execution of the measured implementation.".into(),
            "Statistical fixtures and baseline-control calibrations never establish a Ferric speed, no-regression, runtime, packaging or release claim.".into()],
    })
}

pub(super) fn run(root: &Path, args: &[String]) -> Result<(), String> {
    let [input_flag, input, output_flag, output] = args else {
        return Err("usage: benchmark-validate --input <bundle.json> --report <artifact-report.json>; failures preserve input; correct the named FF-BENCH-E diagnostic and rerun; input <=4MiB/8192samples, independentclusters50..512, no measured-product claim".into());
    };
    if input_flag != "--input" || output_flag != "--report" { return Err(error("ARGUMENTS")); }
    let input_path = relative(root, input)?;
    let metadata = fs::metadata(&input_path).map_err(|e| e.to_string())?;
    if metadata.len() > u64::try_from(LIMIT).map_err(|_| error("LIMIT"))? { return Err(error("INPUT-BOUND")); }
    let data = fs::read(input_path).map_err(|e| e.to_string())?;
    let bundle: Bundle = serde_json::from_slice(&data).map_err(|e| error(&format!("SCHEMA:{e}")))?;
    let report = evaluate(root, bundle)?;
    let destination = Path::new(output);
    if destination.is_absolute() || destination.components().any(|c| !matches!(c, std::path::Component::Normal(_)))
        || !(output.replace('\\', "/").starts_with(".fforager-artifacts/") || output.replace('\\', "/").starts_with("build/reports/"))
    { return Err(error("REPORT-PATH")); }
    let destination = root.join(destination);
    let parent = destination.parent().ok_or_else(|| error("REPORT-PATH"))?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let canonical_parent = parent.canonicalize().map_err(|e| e.to_string())?;
    if !canonical_parent.starts_with(root.canonicalize().map_err(|e| e.to_string())?) { return Err(error("REPORT-PATH-ESCAPE")); }
    fs::write(destination, serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    println!("BENCHMARK_ADMISSION report={output}; semantic contract only; product_progress=false; performance_claim=false");
    Ok(())
}

#[cfg(test)]
mod tests;
