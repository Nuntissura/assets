#![forbid(unsafe_code)]

use fforager_contracts::{
    BoundaryProvenance, CompatibilityRange, EnvelopeHeader, ErrorCode, FrameDecoder, FrameLimits,
    JavaScriptWorkerEnvelope, JavaScriptWorkerMessage, JobId, ProducerId, ProtocolLimits,
    RequestId, SchemaVersion,
};
use fforager_javascript::{
    ChallengeKind, MAXIMUM_FRAME_BYTES, MAXIMUM_JOBS_PER_WORKER, SolverError, WorkerInput,
    WorkerOutput, read_frame, sha256_hex, write_frame,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    env, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use sysinfo::{Pid, ProcessesToUpdate, System};

const DEFAULT_MANIFEST: &str = "build/fixtures/youtube-challenge-v1/manifest.json";
const DEFAULT_PLAYER_ROOT: &str = "build/fixtures/youtube-challenge-v1/players";
const DEFAULT_REPORT: &str = "build/reports/wp-ff-006-youtube-challenge-report.json";
const DEFAULT_DEADLINE_MILLIS: u64 = 20_000;
const DEFAULT_MEMORY_LIMIT_BYTES: u64 = 512 * 1024 * 1024;
const CANCEL_ACK_GRACE_MILLIS: u64 = 500;
const PROBE_GRANT_ENV: &str = "FF_WP006_PROBE_GRANT";
const PLAYER_CAPABILITY_GRANT: &str = "capability-none-v1";
const QUARANTINE_FAILURE_THRESHOLD: u8 = 2;
static SUPERVISOR_NONCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CorpusManifest {
    schema_id: String,
    corpus_id: String,
    version: String,
    solver_implementation: String,
    engine: String,
    oracle: OracleIdentity,
    cases: Vec<CorpusCase>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OracleIdentity {
    kind: String,
    source: String,
    source_commit: String,
    execution_required: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CorpusCase {
    case_id: String,
    player_id: String,
    program_reference: String,
    program_sha256: String,
    challenge: ChallengeKind,
    input: String,
    expected: String,
    mandatory: bool,
}

#[derive(Clone, Debug, Serialize)]
struct CorpusReport {
    schema_id: &'static str,
    report_id: String,
    generated_at_unix_seconds: u64,
    proof_class: &'static str,
    corpus_id: String,
    corpus_version: String,
    manifest_sha256: String,
    solver_implementation: String,
    engine: String,
    oracle: OracleIdentity,
    boundary: &'static str,
    expected_outcome: &'static str,
    observed_outcome: String,
    skipped_semantic_dependencies: Vec<String>,
    cases: Vec<CaseReport>,
    probes: Vec<ProbeReport>,
    counterfactual: CounterfactualReport,
    mandatory_passed: usize,
    mandatory_total: usize,
    verdict: &'static str,
    zero_product_progress: bool,
}

#[derive(Clone, Debug, Serialize)]
struct CaseReport {
    case_id: String,
    player_id: String,
    challenge: ChallengeKind,
    mandatory: bool,
    program_reference: String,
    program_sha256: String,
    input: String,
    extractor_version: String,
    expected: String,
    observed: Option<String>,
    matched: bool,
    engine: Option<String>,
    solver_implementation: Option<String>,
    prepared_sha256: Option<String>,
    candidate_count: Option<usize>,
    successful_candidates: Option<usize>,
    cache_hit: Option<bool>,
    fresh_context: Option<bool>,
    worker_pid: u32,
    duration_millis: u64,
    peak_rss_bytes: u64,
    error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[allow(clippy::struct_excessive_bools)]
struct ProbeReport {
    probe_id: &'static str,
    expected: &'static str,
    observed: String,
    passed: bool,
    worker_pid: u32,
    duration_millis: u64,
    peak_rss_bytes: u64,
    terminated: bool,
    reaped: bool,
    process_absent_after_reap: bool,
}

#[derive(Clone, Debug, Serialize)]
struct CounterfactualReport {
    case_id: String,
    mutation: &'static str,
    original_oracle_passed: bool,
    mutated_oracle_rejected: bool,
}

#[derive(Clone, Copy, Debug)]
struct RequestPolicy {
    deadline: Duration,
    memory_limit_bytes: u64,
    cancel_after: Option<Duration>,
}

#[derive(Clone, Debug)]
enum RequestObservation {
    Response(WorkerOutput),
    Failed { error: ErrorCode, message: String },
    TimedOut,
    AcknowledgedCancelled { generation: u64 },
    ForceTerminatedAfterCancel,
    MemoryLimit,
    WorkerExited,
    Quarantined,
}

#[derive(Clone, Debug)]
struct SupervisedObservation {
    outcome: RequestObservation,
    worker_pid: u32,
    duration_millis: u64,
    peak_rss_bytes: u64,
    terminated: bool,
    reaped: bool,
    process_absent_after_reap: bool,
}

struct Supervisor {
    child: Child,
    stdin: Option<ChildStdin>,
    responses: Receiver<Result<Vec<u8>, String>>,
    reader: Option<thread::JoinHandle<()>>,
    pid: Pid,
    sequence: u64,
    probe_grant: String,
}

#[derive(Default)]
struct QuarantineTracker {
    failures: BTreeMap<String, u8>,
}

impl QuarantineTracker {
    fn record_failure(&mut self, key: &str) {
        let failures = self.failures.entry(key.to_owned()).or_default();
        *failures = failures.saturating_add(1);
    }

    fn is_quarantined(&self, key: &str) -> bool {
        self.failures.get(key).copied().unwrap_or_default() >= QUARANTINE_FAILURE_THRESHOLD
    }

    fn clear(&mut self, key: &str) {
        self.failures.remove(key);
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("FF-JS-CORPUS-ERROR: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), SolverError> {
    let (manifest_path, player_root, report_path) = parse_arguments()?;
    let manifest_bytes = fs::read(&manifest_path).map_err(fforager_javascript::io_error)?;
    let manifest: CorpusManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| SolverError::InvalidResult(error.to_string()))?;
    validate_manifest(&manifest)?;
    let worker_path = worker_path()?;
    let mut supervisor = Supervisor::spawn(&worker_path, &player_root)?;
    let mut case_reports = Vec::with_capacity(manifest.cases.len());

    for (case_index, case) in manifest.cases.iter().enumerate() {
        let observed = supervisor.request(
            &case.case_id,
            &case.program_reference,
            WorkerInput {
                challenge: case.challenge,
                value: case.input.clone(),
                script_sha256: case.program_sha256.clone(),
                extractor_version: manifest.version.clone(),
            },
            RequestPolicy {
                deadline: Duration::from_millis(DEFAULT_DEADLINE_MILLIS),
                memory_limit_bytes: DEFAULT_MEMORY_LIMIT_BYTES,
                cancel_after: None,
            },
        )?;
        let must_restart = observed.terminated
            || matches!(
                observed.outcome,
                RequestObservation::WorkerExited
                    | RequestObservation::TimedOut
                    | RequestObservation::AcknowledgedCancelled { .. }
                    | RequestObservation::ForceTerminatedAfterCancel
                    | RequestObservation::MemoryLimit
            );
        case_reports.push(case_report(case, &manifest.version, observed));
        if must_restart && case_index + 1 < manifest.cases.len() {
            supervisor.shutdown();
            supervisor = Supervisor::spawn(&worker_path, &player_root)?;
        }
    }
    supervisor.shutdown();
    let probes = run_probes(&worker_path, &player_root, &case_reports)?;
    let mandatory_total = case_reports.iter().filter(|case| case.mandatory).count();
    let mandatory_passed = case_reports
        .iter()
        .filter(|case| case.mandatory && case.matched)
        .count();
    let counterfactual = counterfactual_report(&case_reports, &probes);
    let pass = behavioral_acceptance(&case_reports, &probes)
        && counterfactual.original_oracle_passed
        && counterfactual.mutated_oracle_rejected;
    let report = CorpusReport {
        schema_id: "ff.youtube-challenge-report@1",
        report_id: format!("FF-REPORT-WP006-{}", unix_seconds()),
        generated_at_unix_seconds: unix_seconds(),
        proof_class: "integration",
        corpus_id: manifest.corpus_id,
        corpus_version: manifest.version,
        manifest_sha256: sha256_hex(&manifest_bytes),
        solver_implementation: manifest.solver_implementation,
        engine: manifest.engine,
        oracle: manifest.oracle,
        boundary: "spawned fforager-js-worker over WP-005 framed JavaScriptWorkerEnvelope",
        expected_outcome: "all mandatory raw-player cases and all negative probes pass",
        observed_outcome: format!(
            "{mandatory_passed}/{mandatory_total} mandatory cases; {}/{} probes",
            probes.iter().filter(|probe| probe.passed).count(),
            probes.len()
        ),
        skipped_semantic_dependencies: Vec::new(),
        cases: case_reports,
        probes,
        counterfactual,
        mandatory_passed,
        mandatory_total,
        verdict: if pass {
            "PASS_RUST_ONLY_PATH"
        } else {
            "FAILED_SPIKE_REQUIRES_OPERATOR_DECISION"
        },
        zero_product_progress: true,
    };
    write_report(&report_path, &report)?;
    println!("{} {}", report.verdict, report_path.to_string_lossy());
    if pass {
        Ok(())
    } else {
        Err(SolverError::InvalidResult(
            "corpus or negative probe failed; inspect report".to_owned(),
        ))
    }
}

fn parse_arguments() -> Result<(PathBuf, PathBuf, PathBuf), SolverError> {
    let mut manifest = PathBuf::from(DEFAULT_MANIFEST);
    let mut player_root = PathBuf::from(DEFAULT_PLAYER_ROOT);
    let mut report = PathBuf::from(DEFAULT_REPORT);
    let mut args = env::args_os().skip(1);
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| SolverError::InvalidResult("missing argument value".to_owned()))?;
        match flag.to_string_lossy().as_ref() {
            "--manifest" => manifest = value.into(),
            "--player-root" => player_root = value.into(),
            "--report" => report = value.into(),
            _ => {
                return Err(SolverError::InvalidResult(format!(
                    "unknown argument {}",
                    flag.to_string_lossy()
                )));
            }
        }
    }
    Ok((manifest, player_root, report))
}

fn validate_manifest(manifest: &CorpusManifest) -> Result<(), SolverError> {
    if manifest.schema_id != "ff.youtube-challenge-corpus@1"
        || manifest.solver_implementation != fforager_javascript::SOLVER_IMPLEMENTATION
        || manifest.engine != fforager_javascript::ENGINE_ID
        || manifest.oracle.execution_required
        || manifest.oracle.kind != "inert_external_observations"
        || manifest.oracle.source.trim().is_empty()
        || !is_lower_hex(&manifest.oracle.source_commit, 40)
        || manifest.corpus_id.trim().is_empty()
        || !is_semver_triplet(&manifest.version)
        || manifest.cases.is_empty()
    {
        return Err(SolverError::InvalidResult(
            "manifest identity or oracle policy mismatch".to_owned(),
        ));
    }
    let mut case_ids = std::collections::BTreeSet::new();
    for case in &manifest.cases {
        if !case_ids.insert(&case.case_id)
            || !case.mandatory
            || !case.challenge.requires_player()
            || !is_lower_hex(&case.player_id, 8)
            || case.program_reference != format!("{}-main.js", case.player_id)
            || !is_lower_hex(&case.program_sha256, 64)
            || case.input.is_empty()
            || case.expected.is_empty()
            || case.input.len() > 8 * 1024
            || case.expected.len() > 8 * 1024
        {
            return Err(SolverError::InvalidResult(format!(
                "invalid mandatory case {}",
                case.case_id
            )));
        }
    }
    Ok(())
}

fn is_lower_hex(value: &str, exact_length: usize) -> bool {
    value.len() == exact_length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_semver_triplet(value: &str) -> bool {
    let parts = value.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn worker_path() -> Result<PathBuf, SolverError> {
    let current = env::current_exe().map_err(fforager_javascript::io_error)?;
    let directory = current
        .parent()
        .ok_or_else(|| SolverError::Io("runner executable has no parent".to_owned()))?;
    let suffix = env::consts::EXE_SUFFIX;
    let path = directory.join(format!("fforager-js-worker{suffix}"));
    if !path.is_file() {
        return Err(SolverError::Io(format!(
            "worker binary missing at {}; build --bins first",
            path.to_string_lossy()
        )));
    }
    Ok(path)
}

fn request_envelope(
    sequence: u64,
    request_namespace: &str,
    case_id: &str,
    program_reference: &str,
    input: WorkerInput,
    policy: RequestPolicy,
    capability_grant: &str,
) -> Result<JavaScriptWorkerEnvelope, SolverError> {
    Ok(JavaScriptWorkerEnvelope {
        header: EnvelopeHeader {
            schema_id: "ff.javascript-worker@1".to_owned(),
            version: SchemaVersion { major: 1, minor: 0 },
            request_id: RequestId::new(format!("request_wp006_{request_namespace}_{sequence}"))
                .map_err(|error| SolverError::InvalidResult(error.to_string()))?,
            producer_id: ProducerId::new("producer_fforager_js_corpus")
                .map_err(|error| SolverError::InvalidResult(error.to_string()))?,
            job_id: Some(
                JobId::new("job_wp006_youtube_challenge")
                    .map_err(|error| SolverError::InvalidResult(error.to_string()))?,
            ),
            sequence,
        },
        compatibility: CompatibilityRange {
            major: 1,
            minimum_minor: 0,
            maximum_minor: 1,
        },
        provenance: BoundaryProvenance {
            artifact_id: format!("artifact-{case_id}").to_ascii_lowercase(),
            schema_hash: sha256_hex(b"ff.javascript-worker@1"),
            capability_grant_id: capability_grant.to_owned(),
        },
        message: JavaScriptWorkerMessage::Evaluate {
            program_reference: program_reference.to_owned(),
            input: serde_json::to_value(input)
                .map_err(|error| SolverError::InvalidResult(error.to_string()))?,
            deadline_millis: duration_millis(policy.deadline),
            memory_limit_bytes: policy.memory_limit_bytes,
        },
    })
}

impl Supervisor {
    fn spawn(worker_path: &Path, player_root: &Path) -> Result<Self, SolverError> {
        let player_root = fs::canonicalize(player_root).map_err(fforager_javascript::io_error)?;
        let probe_grant = fresh_probe_grant();
        let mut command = Command::new(worker_path);
        command
            .arg("--player-root")
            .arg(&player_root)
            .env_clear()
            .env(PROBE_GRANT_ENV, &probe_grant)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        configure_quiet_process(&mut command);
        let mut child = command.spawn().map_err(fforager_javascript::io_error)?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| SolverError::Io("worker stdin unavailable".to_owned()))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| SolverError::Io("worker stdout unavailable".to_owned()))?;
        let (sender, responses) = mpsc::channel();
        let reader = thread::Builder::new()
            .name("fforager-js-frame-reader".to_owned())
            .spawn(move || {
                let limits = FrameLimits {
                    maximum_frame_bytes: MAXIMUM_FRAME_BYTES,
                };
                loop {
                    let result = read_frame(&mut stdout, limits).map_err(|error| error.to_string());
                    match result {
                        Ok(Some(payload)) => {
                            if sender.send(Ok(payload)).is_err() {
                                break;
                            }
                        }
                        Ok(None) => {
                            let _ = sender.send(Err("worker stdout reached EOF".to_owned()));
                            break;
                        }
                        Err(error) => {
                            let _ = sender.send(Err(error));
                            break;
                        }
                    }
                }
            })
            .map_err(fforager_javascript::io_error)?;
        let pid = Pid::from_u32(child.id());
        Ok(Self {
            child,
            stdin: Some(stdin),
            responses,
            reader: Some(reader),
            pid,
            sequence: 1,
            probe_grant,
        })
    }

    fn request(
        &mut self,
        case_id: &str,
        program_reference: &str,
        input: WorkerInput,
        policy: RequestPolicy,
    ) -> Result<SupervisedObservation, SolverError> {
        let capability_grant = if input.challenge.requires_player() {
            PLAYER_CAPABILITY_GRANT.to_owned()
        } else {
            self.probe_grant.clone()
        };
        self.request_with_capability(case_id, program_reference, input, policy, &capability_grant)
    }

    fn request_with_capability(
        &mut self,
        case_id: &str,
        program_reference: &str,
        input: WorkerInput,
        policy: RequestPolicy,
        capability_grant: &str,
    ) -> Result<SupervisedObservation, SolverError> {
        let envelope = request_envelope(
            self.sequence,
            &self.pid.as_u32().to_string(),
            case_id,
            program_reference,
            input,
            policy,
            capability_grant,
        )?;
        let payload = serde_json::to_vec(&envelope)
            .map_err(|error| SolverError::InvalidResult(error.to_string()))?;
        let stdin = self.stdin.as_mut().ok_or_else(|| {
            SolverError::Io(format!("worker stdin closed before request {case_id}"))
        })?;
        write_frame(
            stdin,
            &payload,
            FrameLimits {
                maximum_frame_bytes: MAXIMUM_FRAME_BYTES,
            },
        )
        .map_err(|error| SolverError::Io(format!("{case_id}: {error}")))?;
        self.sequence = self.sequence.saturating_add(1);
        self.observe_response(&envelope, policy)
    }

    fn observe_response(
        &mut self,
        request: &JavaScriptWorkerEnvelope,
        policy: RequestPolicy,
    ) -> Result<SupervisedObservation, SolverError> {
        let started = Instant::now();
        let mut peak_rss_bytes = 0_u64;
        let mut system = System::new();
        let mut cancellation_sent = None;
        loop {
            match self.responses.try_recv() {
                Ok(Ok(payload)) => {
                    return self.response_observation(&payload, request, started, peak_rss_bytes);
                }
                Ok(Err(_)) | Err(TryRecvError::Disconnected) => {
                    let reaped = self.reap_without_kill();
                    return Ok(self.terminal_observation(
                        RequestObservation::WorkerExited,
                        started,
                        peak_rss_bytes,
                        false,
                        reaped,
                    ));
                }
                Err(TryRecvError::Empty) => {}
            }
            system.refresh_processes(ProcessesToUpdate::Some(&[self.pid]), true);
            if let Some(process) = system.process(self.pid) {
                peak_rss_bytes = peak_rss_bytes.max(process.memory());
            }
            if peak_rss_bytes > policy.memory_limit_bytes {
                let reaped = self.terminate_and_reap();
                return Ok(self.terminal_observation(
                    RequestObservation::MemoryLimit,
                    started,
                    peak_rss_bytes,
                    true,
                    reaped,
                ));
            }
            if self
                .child
                .try_wait()
                .map_err(fforager_javascript::io_error)?
                .is_some()
            {
                if let Ok(Ok(payload)) = self.responses.recv_timeout(Duration::from_millis(100)) {
                    return self.response_observation(&payload, request, started, peak_rss_bytes);
                }
                return Ok(self.terminal_observation(
                    RequestObservation::WorkerExited,
                    started,
                    peak_rss_bytes,
                    false,
                    true,
                ));
            }
            if let Some(cancel_after) = policy.cancel_after {
                if cancellation_sent.is_none() && started.elapsed() >= cancel_after {
                    self.send_cancellation(request, 1).map_err(|error| {
                        SolverError::Io(format!(
                            "cancellation send for {} failed: {error}",
                            request.header.request_id
                        ))
                    })?;
                    cancellation_sent = Some(Instant::now());
                }
                if cancellation_sent.is_some_and(|sent| {
                    sent.elapsed() >= Duration::from_millis(CANCEL_ACK_GRACE_MILLIS)
                }) {
                    let reaped = self.terminate_and_reap();
                    return Ok(self.terminal_observation(
                        RequestObservation::ForceTerminatedAfterCancel,
                        started,
                        peak_rss_bytes,
                        true,
                        reaped,
                    ));
                }
            }
            if started.elapsed() >= policy.deadline {
                let reaped = self.terminate_and_reap();
                return Ok(self.terminal_observation(
                    RequestObservation::TimedOut,
                    started,
                    peak_rss_bytes,
                    true,
                    reaped,
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn response_observation(
        &mut self,
        payload: &[u8],
        request: &JavaScriptWorkerEnvelope,
        started: Instant,
        peak_rss_bytes: u64,
    ) -> Result<SupervisedObservation, SolverError> {
        let response = FrameDecoder::decode_javascript_worker_with_limits(
            payload,
            FrameLimits {
                maximum_frame_bytes: MAXIMUM_FRAME_BYTES,
            },
            ProtocolLimits {
                maximum_message_bytes: MAXIMUM_FRAME_BYTES,
                ..ProtocolLimits::default()
            },
        )
        .map_err(|error| SolverError::Frame(error.to_string()))?;
        validate_response_correlation(request, &response)?;
        let outcome = match response.message {
            JavaScriptWorkerMessage::Result { output } => {
                let output: WorkerOutput = serde_json::from_value(output)
                    .map_err(|error| SolverError::InvalidResult(error.to_string()))?;
                RequestObservation::Response(output)
            }
            JavaScriptWorkerMessage::Failed { error, message } => {
                RequestObservation::Failed { error, message }
            }
            JavaScriptWorkerMessage::Cancelled { generation } => {
                if generation != 1 {
                    return Err(SolverError::InvalidResult(
                        "worker cancellation generation mismatch".to_owned(),
                    ));
                }
                RequestObservation::AcknowledgedCancelled { generation }
            }
            JavaScriptWorkerMessage::Evaluate { .. } => {
                return Err(SolverError::InvalidResult(
                    "worker echoed evaluate request".to_owned(),
                ));
            }
        };
        let mut observation = SupervisedObservation {
            outcome,
            worker_pid: self.pid.as_u32(),
            duration_millis: duration_millis(started.elapsed()),
            peak_rss_bytes,
            terminated: false,
            reaped: false,
            process_absent_after_reap: false,
        };
        if matches!(
            observation.outcome,
            RequestObservation::AcknowledgedCancelled { .. }
        ) {
            let terminal = self.await_natural_exit(Duration::from_millis(500));
            observation.terminated = terminal.terminated;
            observation.reaped = terminal.reaped;
            observation.process_absent_after_reap = terminal.process_absent_after_reap;
            observation.peak_rss_bytes = observation.peak_rss_bytes.max(terminal.peak_rss_bytes);
        }
        Ok(observation)
    }

    fn send_cancellation(
        &mut self,
        request: &JavaScriptWorkerEnvelope,
        generation: u64,
    ) -> Result<(), SolverError> {
        let mut cancel = request.clone();
        cancel.header.sequence = self.sequence;
        cancel.message = JavaScriptWorkerMessage::Cancelled { generation };
        let payload = serde_json::to_vec(&cancel)
            .map_err(|error| SolverError::InvalidResult(error.to_string()))?;
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| SolverError::Io("worker stdin closed before cancellation".to_owned()))?;
        write_frame(
            stdin,
            &payload,
            FrameLimits {
                maximum_frame_bytes: MAXIMUM_FRAME_BYTES,
            },
        )?;
        self.sequence = self.sequence.saturating_add(1);
        Ok(())
    }

    fn terminal_observation(
        &self,
        outcome: RequestObservation,
        started: Instant,
        peak_rss_bytes: u64,
        terminated: bool,
        reaped: bool,
    ) -> SupervisedObservation {
        let mut system = System::new();
        system.refresh_processes(ProcessesToUpdate::Some(&[self.pid]), true);
        SupervisedObservation {
            outcome,
            worker_pid: self.pid.as_u32(),
            duration_millis: duration_millis(started.elapsed()),
            peak_rss_bytes,
            terminated,
            reaped,
            process_absent_after_reap: system.process(self.pid).is_none(),
        }
    }

    fn terminate_and_reap(&mut self) -> bool {
        let _ = self.child.kill();
        self.stdin.take();
        let reaped = self.child.wait().is_ok();
        self.join_reader();
        reaped
    }

    fn reap_without_kill(&mut self) -> bool {
        self.stdin.take();
        let reaped = self.child.wait().is_ok();
        self.join_reader();
        reaped
    }

    fn await_natural_exit(&mut self, deadline: Duration) -> SupervisedObservation {
        let started = Instant::now();
        let mut peak_rss_bytes = 0_u64;
        let mut system = System::new();
        loop {
            system.refresh_processes(ProcessesToUpdate::Some(&[self.pid]), true);
            if let Some(process) = system.process(self.pid) {
                peak_rss_bytes = peak_rss_bytes.max(process.memory());
            }
            match self.child.try_wait() {
                Ok(Some(_)) => {
                    return self.terminal_observation(
                        RequestObservation::WorkerExited,
                        started,
                        peak_rss_bytes,
                        false,
                        true,
                    );
                }
                Ok(None) if started.elapsed() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(None) | Err(_) => {
                    let reaped = self.terminate_and_reap();
                    return self.terminal_observation(
                        RequestObservation::TimedOut,
                        started,
                        peak_rss_bytes,
                        true,
                        reaped,
                    );
                }
            }
        }
    }

    fn shutdown(&mut self) {
        self.stdin.take();
        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(500) {
            match self.child.try_wait() {
                Ok(Some(_)) => {
                    self.join_reader();
                    return;
                }
                Ok(None) => thread::sleep(Duration::from_millis(10)),
                Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.join_reader();
    }

    fn join_reader(&mut self) {
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn validate_response_correlation(
    request: &JavaScriptWorkerEnvelope,
    response: &JavaScriptWorkerEnvelope,
) -> Result<(), SolverError> {
    let cancellation_response =
        matches!(response.message, JavaScriptWorkerMessage::Cancelled { .. });
    let expected_sequence = request
        .header
        .sequence
        .checked_add(if cancellation_response { 2 } else { 1 })
        .ok_or_else(|| SolverError::InvalidResult("response sequence exhausted".to_owned()))?;
    if response.header.request_id != request.header.request_id
        || response.header.schema_id != request.header.schema_id
        || response.header.version != request.header.version
        || response.header.job_id != request.header.job_id
        || response.header.producer_id.as_str() != "producer_fforager_js_worker"
        || response.header.sequence != expected_sequence
        || response.compatibility != request.compatibility
        || response.provenance != request.provenance
    {
        return Err(SolverError::InvalidResult(
            "worker response correlation tuple mismatch".to_owned(),
        ));
    }
    Ok(())
}

fn case_report(
    case: &CorpusCase,
    extractor_version: &str,
    observation: SupervisedObservation,
) -> CaseReport {
    match observation.outcome {
        RequestObservation::Response(output) => {
            let matched = output.result == case.expected
                && output.engine == fforager_javascript::ENGINE_ID
                && output.solver_implementation == fforager_javascript::SOLVER_IMPLEMENTATION
                && output.script_sha256 == case.program_sha256
                && output.candidate_count > 0
                && output.successful_candidates > 0
                && output.fresh_context;
            CaseReport {
                case_id: case.case_id.clone(),
                player_id: case.player_id.clone(),
                challenge: case.challenge,
                mandatory: case.mandatory,
                program_reference: case.program_reference.clone(),
                program_sha256: case.program_sha256.clone(),
                input: case.input.clone(),
                extractor_version: extractor_version.to_owned(),
                expected: case.expected.clone(),
                observed: Some(output.result),
                matched,
                engine: Some(output.engine),
                solver_implementation: Some(output.solver_implementation),
                prepared_sha256: Some(output.prepared_sha256),
                candidate_count: Some(output.candidate_count),
                successful_candidates: Some(output.successful_candidates),
                cache_hit: Some(output.cache_hit),
                fresh_context: Some(output.fresh_context),
                worker_pid: observation.worker_pid,
                duration_millis: observation.duration_millis,
                peak_rss_bytes: observation.peak_rss_bytes,
                error: None,
            }
        }
        RequestObservation::Failed { error, message } => CaseReport {
            case_id: case.case_id.clone(),
            player_id: case.player_id.clone(),
            challenge: case.challenge,
            mandatory: case.mandatory,
            program_reference: case.program_reference.clone(),
            program_sha256: case.program_sha256.clone(),
            input: case.input.clone(),
            extractor_version: extractor_version.to_owned(),
            expected: case.expected.clone(),
            observed: None,
            matched: false,
            engine: None,
            solver_implementation: None,
            prepared_sha256: None,
            candidate_count: None,
            successful_candidates: None,
            cache_hit: None,
            fresh_context: None,
            worker_pid: observation.worker_pid,
            duration_millis: observation.duration_millis,
            peak_rss_bytes: observation.peak_rss_bytes,
            error: Some(format!("{error:?}: {message}")),
        },
        other => CaseReport {
            case_id: case.case_id.clone(),
            player_id: case.player_id.clone(),
            challenge: case.challenge,
            mandatory: case.mandatory,
            program_reference: case.program_reference.clone(),
            program_sha256: case.program_sha256.clone(),
            input: case.input.clone(),
            extractor_version: extractor_version.to_owned(),
            expected: case.expected.clone(),
            observed: None,
            matched: false,
            engine: None,
            solver_implementation: None,
            prepared_sha256: None,
            candidate_count: None,
            successful_candidates: None,
            cache_hit: None,
            fresh_context: None,
            worker_pid: observation.worker_pid,
            duration_millis: observation.duration_millis,
            peak_rss_bytes: observation.peak_rss_bytes,
            error: Some(format!("{other:?}")),
        },
    }
}

#[allow(clippy::too_many_lines)]
fn run_probes(
    worker_path: &Path,
    player_root: &Path,
    case_reports: &[CaseReport],
) -> Result<Vec<ProbeReport>, SolverError> {
    let mut probes = Vec::new();
    let zero_hash = "0".repeat(64);
    let no_program = "74edf1a3-main.js";

    probes.push(cache_behavior_probe(
        worker_path,
        player_root,
        case_reports,
    )?);

    probes.push(raw_worker_exit_probe(
        worker_path,
        player_root,
        "WP-FF-006-PROBE-FRAME-ZERO",
        "a zero-length frame is rejected and the worker exits and is reaped",
        "frame_zero",
        &0_u32.to_be_bytes(),
    )?);
    probes.push(raw_worker_exit_probe(
        worker_path,
        player_root,
        "WP-FF-006-PROBE-FRAME-OVERSIZED",
        "an oversized declared frame is rejected before allocation",
        "frame_oversized",
        &u32::try_from(MAXIMUM_FRAME_BYTES + 1)
            .map_err(|_| SolverError::InvalidResult("frame limit exceeds u32".to_owned()))?
            .to_be_bytes(),
    )?);
    probes.push(raw_worker_exit_probe(
        worker_path,
        player_root,
        "WP-FF-006-PROBE-FRAME-PARTIAL-HEADER",
        "a partial frame header is rejected and the worker is reaped",
        "frame_partial_header",
        &[0, 0],
    )?);
    probes.push(raw_worker_exit_probe(
        worker_path,
        player_root,
        "WP-FF-006-PROBE-FRAME-PARTIAL-PAYLOAD",
        "a partial frame payload is rejected and the worker is reaped",
        "frame_partial_payload",
        &[0, 0, 0, 4, b'{'],
    )?);
    probes.push(raw_worker_exit_probe(
        worker_path,
        player_root,
        "WP-FF-006-PROBE-FRAME-MALFORMED-JSON",
        "malformed JSON inside a valid frame is rejected",
        "frame_invalid_json",
        &framed_bytes(b"{")?,
    )?);

    let protocol_input = WorkerInput {
        challenge: ChallengeKind::CapabilityProbe,
        value: String::new(),
        script_sha256: zero_hash.clone(),
        extractor_version: "probe-v1".to_owned(),
    };
    let protocol_envelope = request_envelope(
        1,
        "raw",
        "probe-protocol",
        no_program,
        protocol_input,
        default_policy(),
        PLAYER_CAPABILITY_GRANT,
    )?;
    let mut incompatible = serde_json::to_value(&protocol_envelope)
        .map_err(|error| SolverError::InvalidResult(error.to_string()))?;
    incompatible["header"]["version"]["major"] = serde_json::json!(2);
    probes.push(raw_worker_exit_probe(
        worker_path,
        player_root,
        "WP-FF-006-PROBE-PROTOCOL-VERSION",
        "an incompatible protocol major version is rejected",
        "protocol_version",
        &framed_json(&incompatible)?,
    )?);
    let mut unknown = serde_json::to_value(&protocol_envelope)
        .map_err(|error| SolverError::InvalidResult(error.to_string()))?;
    unknown["unexpected_field"] = serde_json::json!(true);
    probes.push(raw_worker_exit_probe(
        worker_path,
        player_root,
        "WP-FF-006-PROBE-PROTOCOL-UNKNOWN-FIELD",
        "unknown mandatory envelope fields are rejected",
        "protocol_unknown_field",
        &framed_json(&unknown)?,
    )?);
    let mut over_memory = serde_json::to_value(&protocol_envelope)
        .map_err(|error| SolverError::InvalidResult(error.to_string()))?;
    over_memory["message"]["body"]["memory_limit_bytes"] =
        serde_json::json!(DEFAULT_MEMORY_LIMIT_BYTES + 1);
    probes.push(raw_worker_exit_probe(
        worker_path,
        player_root,
        "WP-FF-006-PROBE-PROTOCOL-MEMORY-LIMIT",
        "a requested memory limit above the WP-005 protocol ceiling is rejected",
        "protocol_numeric_limit",
        &framed_json(&over_memory)?,
    )?);
    let mut path_escape = serde_json::to_value(&protocol_envelope)
        .map_err(|error| SolverError::InvalidResult(error.to_string()))?;
    path_escape["message"]["body"]["program_reference"] = serde_json::json!("../manifest.json");
    probes.push(raw_worker_exit_probe(
        worker_path,
        player_root,
        "WP-FF-006-PROBE-PATH-CONFINEMENT",
        "a traversal reference is rejected with an exact protocol category and the worker is reaped",
        "protocol_reference",
        &framed_json(&path_escape)?,
    )?);

    let mut isolation = Supervisor::spawn(worker_path, player_root)?;
    let unauthorized_crash = isolation.request_with_capability(
        "probe-unauthorized-crash",
        no_program,
        WorkerInput {
            challenge: ChallengeKind::Crash,
            value: String::new(),
            script_sha256: zero_hash.clone(),
            extractor_version: "probe-v1".to_owned(),
        },
        default_policy(),
        "capability-attacker-claimed",
    )?;
    let unauthorized_pid = unauthorized_crash.worker_pid;
    probes.push(probe_from_failure(
        "WP-FF-006-PROBE-PROBE-AUTHORITY",
        "an attacker-claimed grant cannot invoke destructive probe modes and the worker remains reusable",
        unauthorized_crash,
        &ErrorCode::InvalidInput,
        "unauthorized worker capability grant",
    ));
    let capability = isolation.request(
        "probe-capability",
        no_program,
        WorkerInput {
            challenge: ChallengeKind::CapabilityProbe,
            value: String::new(),
            script_sha256: zero_hash.clone(),
            extractor_version: "probe-v1".to_owned(),
        },
        default_policy(),
    )?;
    let same_worker_after_rejection = capability.worker_pid == unauthorized_pid;
    probes.push(probe_from_response(
        "WP-FF-006-PROBE-CAPABILITY",
        "fetch, require, process, Deno, Bun, and console are all undefined",
        capability,
        move |output| {
            serde_json::from_str::<serde_json::Value>(&output.result)
                .ok()
                .is_some_and(|value| {
                    same_worker_after_rejection
                        && ["fetch", "require", "process", "deno", "bun", "console"]
                            .iter()
                            .all(|name| value[*name] == "undefined")
                })
        },
    ));
    let seeded = isolation.request(
        "probe-state-seed",
        no_program,
        WorkerInput {
            challenge: ChallengeKind::StateSeed,
            value: String::new(),
            script_sha256: zero_hash.clone(),
            extractor_version: "probe-v1".to_owned(),
        },
        default_policy(),
    )?;
    let checked = isolation.request(
        "probe-state-check",
        no_program,
        WorkerInput {
            challenge: ChallengeKind::StateCheck,
            value: String::new(),
            script_sha256: zero_hash.clone(),
            extractor_version: "probe-v1".to_owned(),
        },
        default_policy(),
    )?;
    let seed_ok = matches!(
        seeded.outcome,
        RequestObservation::Response(ref output) if output.result == "seeded"
    );
    probes.push(probe_from_response(
        "WP-FF-006-PROBE-FRESH-CONTEXT",
        "a second job observes no heap state seeded by the first job",
        checked,
        move |output| seed_ok && output.result == "undefined" && output.fresh_context,
    ));
    isolation.shutdown();

    let mut invalid_input = Supervisor::spawn(worker_path, player_root)?;
    let hash_mismatch = invalid_input.request(
        "probe-hash-mismatch",
        no_program,
        WorkerInput {
            challenge: ChallengeKind::N,
            value: "9nRTxrbM1f0yHg".to_owned(),
            script_sha256: zero_hash.clone(),
            extractor_version: "probe-v1".to_owned(),
        },
        default_policy(),
    )?;
    probes.push(probe_from_failure(
        "WP-FF-006-PROBE-HASH-MISMATCH",
        "a player whose bytes do not match the requested SHA-256 is rejected as invalid input",
        hash_mismatch,
        &ErrorCode::InvalidInput,
        "ScriptHashMismatch",
    ));
    invalid_input.shutdown();

    let mut timeout = Supervisor::spawn(worker_path, player_root)?;
    let timeout_observation = timeout.request(
        "probe-timeout",
        no_program,
        WorkerInput {
            challenge: ChallengeKind::InfiniteLoop,
            value: String::new(),
            script_sha256: zero_hash.clone(),
            extractor_version: "probe-v1".to_owned(),
        },
        RequestPolicy {
            deadline: Duration::from_millis(150),
            memory_limit_bytes: DEFAULT_MEMORY_LIMIT_BYTES,
            cancel_after: None,
        },
    )?;
    probes.push(probe_from_terminal(
        "WP-FF-006-PROBE-TIMEOUT",
        "infinite JavaScript is terminated and reaped at the wall deadline",
        timeout_observation,
        |outcome| matches!(outcome, RequestObservation::TimedOut),
    ));

    let mut cancellation = Supervisor::spawn(worker_path, player_root)?;
    let cancellation_observation = cancellation.request(
        "probe-cancellation",
        no_program,
        WorkerInput {
            challenge: ChallengeKind::InfiniteLoop,
            value: String::new(),
            script_sha256: zero_hash.clone(),
            extractor_version: "probe-v1".to_owned(),
        },
        RequestPolicy {
            deadline: Duration::from_secs(2),
            memory_limit_bytes: DEFAULT_MEMORY_LIMIT_BYTES,
            cancel_after: Some(Duration::from_millis(100)),
        },
    )?;
    probes.push(probe_from_terminal(
        "WP-FF-006-PROBE-CANCELLATION",
        "correlated cancellation is acknowledged before the non-cooperative worker exits and is reaped",
        cancellation_observation,
        |outcome| {
            matches!(
                outcome,
                RequestObservation::AcknowledgedCancelled { generation: 1 }
            )
        },
    ));

    let mut memory = Supervisor::spawn(worker_path, player_root)?;
    let memory_observation = memory.request(
        "probe-memory",
        no_program,
        WorkerInput {
            challenge: ChallengeKind::MemoryBomb,
            value: String::new(),
            script_sha256: zero_hash.clone(),
            extractor_version: "probe-v1".to_owned(),
        },
        RequestPolicy {
            deadline: Duration::from_secs(5),
            memory_limit_bytes: 128 * 1024 * 1024,
            cancel_after: None,
        },
    )?;
    probes.push(probe_from_terminal(
        "WP-FF-006-PROBE-MEMORY",
        "RSS limit terminates and reaps the allocation-bomb worker",
        memory_observation,
        |outcome| matches!(outcome, RequestObservation::MemoryLimit),
    ));

    let mut crash = Supervisor::spawn(worker_path, player_root)?;
    let crash_observation = crash.request(
        "probe-crash",
        no_program,
        WorkerInput {
            challenge: ChallengeKind::Crash,
            value: String::new(),
            script_sha256: zero_hash.clone(),
            extractor_version: "probe-v1".to_owned(),
        },
        default_policy(),
    )?;
    probes.push(probe_from_terminal(
        "WP-FF-006-PROBE-CRASH",
        "worker abort is observed and reaped without a false result",
        crash_observation,
        |outcome| matches!(outcome, RequestObservation::WorkerExited),
    ));
    probes.push(quarantine_probe(worker_path, player_root)?);

    let mut recycling = Supervisor::spawn(worker_path, player_root)?;
    let mut all_recycle_jobs_completed = true;
    for sequence in 0..MAXIMUM_JOBS_PER_WORKER {
        let observation = recycling.request(
            &format!("probe-recycle-{sequence}"),
            no_program,
            WorkerInput {
                challenge: ChallengeKind::CapabilityProbe,
                value: String::new(),
                script_sha256: zero_hash.clone(),
                extractor_version: "probe-v1".to_owned(),
            },
            default_policy(),
        )?;
        all_recycle_jobs_completed &= matches!(
            observation.outcome,
            RequestObservation::Response(ref output) if output.fresh_context
        );
    }
    let recycle_observation = recycling.await_natural_exit(Duration::from_secs(2));
    let mut recycle_probe = probe_from_terminal(
        "WP-FF-006-PROBE-WORKER-RECYCLE",
        "the worker completes the bounded job quota, then exits and is reaped before another job",
        recycle_observation,
        |outcome| matches!(outcome, RequestObservation::WorkerExited),
    );
    recycle_probe.passed &= all_recycle_jobs_completed;
    recycle_probe.observed = format!(
        "{}; completed_jobs={MAXIMUM_JOBS_PER_WORKER}",
        recycle_probe.observed
    );
    probes.push(recycle_probe);
    Ok(probes)
}

fn cache_behavior_probe(
    worker_path: &Path,
    player_root: &Path,
    cases: &[CaseReport],
) -> Result<ProbeReport, SolverError> {
    let case = cases
        .iter()
        .find(|case| case.matched && case.challenge == ChallengeKind::N)
        .ok_or_else(|| {
            SolverError::InvalidResult(format!(
                "cache probe requires matched n case; case_diagnostics=[{}]",
                case_failure_diagnostics(cases)
            ))
        })?;
    let mut supervisor = Supervisor::spawn(worker_path, player_root)?;
    let request = |version: &str| WorkerInput {
        challenge: case.challenge,
        value: case.input.clone(),
        script_sha256: case.program_sha256.clone(),
        extractor_version: version.to_owned(),
    };
    let cache_policy = RequestPolicy {
        deadline: Duration::from_millis(DEFAULT_DEADLINE_MILLIS),
        memory_limit_bytes: DEFAULT_MEMORY_LIMIT_BYTES,
        cancel_after: None,
    };
    let initial = supervisor.request(
        "probe-cache-initial",
        &case.program_reference,
        request(&case.extractor_version),
        cache_policy,
    )?;
    if !matches!(initial.outcome, RequestObservation::Response(_)) {
        return Ok(ProbeReport {
            probe_id: "WP-FF-006-PROBE-CACHE-KEY",
            expected: "same script/mode/extractor version hits while a changed extractor version misses",
            observed: format!("initial request failed: {:?}", initial.outcome),
            passed: false,
            worker_pid: initial.worker_pid,
            duration_millis: initial.duration_millis,
            peak_rss_bytes: initial.peak_rss_bytes,
            terminated: initial.terminated,
            reaped: initial.reaped,
            process_absent_after_reap: initial.process_absent_after_reap,
        });
    }
    let same_key = supervisor.request(
        "probe-cache-same",
        &case.program_reference,
        request(&case.extractor_version),
        cache_policy,
    )?;
    let changed_version = supervisor.request(
        "probe-cache-version",
        &case.program_reference,
        request("cache-counterfactual-v2"),
        cache_policy,
    )?;
    let initial_ok = matches!(
        initial.outcome,
        RequestObservation::Response(ref output)
            if !output.cache_hit && output.result == case.expected
    );
    let same_ok = matches!(
        same_key.outcome,
        RequestObservation::Response(ref output)
            if output.cache_hit && output.result == case.expected
    );
    let changed_ok = matches!(
        changed_version.outcome,
        RequestObservation::Response(ref output)
            if !output.cache_hit && output.result == case.expected
    );
    let worker_pid = same_key.worker_pid;
    let peak_rss_bytes = initial
        .peak_rss_bytes
        .max(same_key.peak_rss_bytes)
        .max(changed_version.peak_rss_bytes);
    supervisor.shutdown();
    Ok(ProbeReport {
        probe_id: "WP-FF-006-PROBE-CACHE-KEY",
        expected: "same script/mode/extractor version hits while a changed extractor version misses",
        observed: format!(
            "initial_miss={initial_ok}; same_key_hit={same_ok}; changed_extractor_version_miss={changed_ok}"
        ),
        passed: initial_ok && same_ok && changed_ok,
        worker_pid,
        duration_millis: 0,
        peak_rss_bytes,
        terminated: false,
        reaped: false,
        process_absent_after_reap: false,
    })
}

fn case_failure_diagnostics(cases: &[CaseReport]) -> String {
    cases
        .iter()
        .map(|case| {
            format!(
                "{}:matched={}:observed={:?}:error={:?}:duration_ms={}:peak_rss={}",
                case.case_id,
                case.matched,
                case.observed,
                case.error,
                case.duration_millis,
                case.peak_rss_bytes
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn quarantine_probe(worker_path: &Path, player_root: &Path) -> Result<ProbeReport, SolverError> {
    let key = "probe-crash-artifact";
    let mut tracker = QuarantineTracker::default();
    let mut spawned_pids = Vec::new();
    let mut failures_typed_and_reaped = true;
    for attempt in 0..QUARANTINE_FAILURE_THRESHOLD {
        if tracker.is_quarantined(key) {
            failures_typed_and_reaped = false;
            break;
        }
        let mut supervisor = Supervisor::spawn(worker_path, player_root)?;
        spawned_pids.push(supervisor.pid.as_u32());
        let observation = supervisor.request(
            &format!("probe-quarantine-{attempt}"),
            "74edf1a3-main.js",
            WorkerInput {
                challenge: ChallengeKind::Crash,
                value: String::new(),
                script_sha256: "0".repeat(64),
                extractor_version: "probe-v1".to_owned(),
            },
            default_policy(),
        )?;
        failures_typed_and_reaped &=
            matches!(observation.outcome, RequestObservation::WorkerExited)
                && observation.reaped
                && observation.process_absent_after_reap;
        tracker.record_failure(key);
    }
    let quarantined = tracker.is_quarantined(key);
    let independent_key_allowed = !tracker.is_quarantined("independent-artifact");
    let spawned_before_block = spawned_pids.len();
    let post_threshold = if quarantined {
        RequestObservation::Quarantined
    } else {
        RequestObservation::WorkerExited
    };
    let no_post_threshold_pid = spawned_pids.len() == spawned_before_block;
    tracker.clear(key);
    let operator_clear_restored = !tracker.is_quarantined(key);
    Ok(ProbeReport {
        probe_id: "WP-FF-006-PROBE-QUARANTINE",
        expected: "two provenance-keyed terminal failures quarantine the key and a subsequent attempt spawns no worker",
        observed: format!(
            "threshold={QUARANTINE_FAILURE_THRESHOLD}; outcome={post_threshold:?}; independent_key_allowed={independent_key_allowed}; operator_clear_restored={operator_clear_restored}; spawned_pids={spawned_pids:?}"
        ),
        passed: failures_typed_and_reaped
            && quarantined
            && matches!(post_threshold, RequestObservation::Quarantined)
            && no_post_threshold_pid
            && independent_key_allowed
            && operator_clear_restored,
        worker_pid: spawned_pids.last().copied().unwrap_or_default(),
        duration_millis: 0,
        peak_rss_bytes: 0,
        terminated: false,
        reaped: true,
        process_absent_after_reap: true,
    })
}

fn framed_json(value: &serde_json::Value) -> Result<Vec<u8>, SolverError> {
    let payload =
        serde_json::to_vec(value).map_err(|error| SolverError::InvalidResult(error.to_string()))?;
    framed_bytes(&payload)
}

fn framed_bytes(payload: &[u8]) -> Result<Vec<u8>, SolverError> {
    let declared = u32::try_from(payload.len())
        .map_err(|_| SolverError::InvalidResult("probe payload exceeds u32".to_owned()))?;
    let mut framed = Vec::with_capacity(4 + payload.len());
    framed.extend_from_slice(&declared.to_be_bytes());
    framed.extend_from_slice(payload);
    Ok(framed)
}

fn raw_worker_exit_probe(
    worker_path: &Path,
    player_root: &Path,
    probe_id: &'static str,
    expected: &'static str,
    expected_fatal_category: &'static str,
    bytes: &[u8],
) -> Result<ProbeReport, SolverError> {
    let player_root = fs::canonicalize(player_root).map_err(fforager_javascript::io_error)?;
    let probe_grant = fresh_probe_grant();
    let mut command = Command::new(worker_path);
    command
        .arg("--player-root")
        .arg(&player_root)
        .env_clear()
        .env(PROBE_GRANT_ENV, probe_grant)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    configure_quiet_process(&mut command);
    let mut child = command.spawn().map_err(fforager_javascript::io_error)?;
    let pid = Pid::from_u32(child.id());
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| SolverError::Io("raw probe worker stderr unavailable".to_owned()))?;
    let started = Instant::now();
    let write_error = child
        .stdin
        .take()
        .ok_or_else(|| SolverError::Io("raw probe worker stdin unavailable".to_owned()))?
        .write_all(bytes)
        .err()
        .map(|error| error.to_string());
    let deadline = Duration::from_secs(2);
    let (status, terminated, reaped) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (Some(status), false, true),
            Ok(None) if started.elapsed() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            Ok(None) | Err(_) => {
                let terminated = child.kill().is_ok();
                let reaped = child.wait().is_ok();
                break (None, terminated, reaped);
            }
        }
    };
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    let process_absent_after_reap = system.process(pid).is_none();
    let rejected = status.is_some_and(|status| !status.success());
    let mut diagnostic = String::new();
    stderr
        .read_to_string(&mut diagnostic)
        .map_err(fforager_javascript::io_error)?;
    let expected_marker = format!("FF-JS-WORKER-FATAL:{expected_fatal_category}:");
    Ok(ProbeReport {
        probe_id,
        expected,
        observed: format!(
            "status={status:?}; category={expected_fatal_category}; write_error={write_error:?}; bytes_written={}; diagnostic={diagnostic:?}",
            bytes.len(),
        ),
        passed: rejected
            && write_error.is_none()
            && diagnostic.contains(&expected_marker)
            && reaped
            && process_absent_after_reap,
        worker_pid: pid.as_u32(),
        duration_millis: duration_millis(started.elapsed()),
        peak_rss_bytes: 0,
        terminated,
        reaped,
        process_absent_after_reap,
    })
}

fn default_policy() -> RequestPolicy {
    RequestPolicy {
        deadline: Duration::from_secs(2),
        memory_limit_bytes: DEFAULT_MEMORY_LIMIT_BYTES,
        cancel_after: None,
    }
}

fn probe_from_failure(
    probe_id: &'static str,
    expected: &'static str,
    observation: SupervisedObservation,
    expected_error: &ErrorCode,
    expected_message: &str,
) -> ProbeReport {
    let SupervisedObservation {
        outcome,
        worker_pid,
        duration_millis,
        peak_rss_bytes,
        terminated,
        reaped,
        process_absent_after_reap,
    } = observation;
    let passed = matches!(
        &outcome,
        RequestObservation::Failed { error, message }
            if error == expected_error && message.contains(expected_message)
    ) && !terminated;
    ProbeReport {
        probe_id,
        expected,
        observed: format!("{outcome:?}"),
        passed,
        worker_pid,
        duration_millis,
        peak_rss_bytes,
        terminated,
        reaped,
        process_absent_after_reap,
    }
}

fn probe_from_response<F>(
    probe_id: &'static str,
    expected: &'static str,
    observation: SupervisedObservation,
    predicate: F,
) -> ProbeReport
where
    F: FnOnce(&WorkerOutput) -> bool,
{
    let SupervisedObservation {
        outcome,
        worker_pid,
        duration_millis,
        peak_rss_bytes,
        terminated,
        reaped,
        process_absent_after_reap,
    } = observation;
    let (observed, passed) = match &outcome {
        RequestObservation::Response(output) => (output.result.clone(), predicate(output)),
        other => (format!("{other:?}"), false),
    };
    ProbeReport {
        probe_id,
        expected,
        observed,
        passed,
        worker_pid,
        duration_millis,
        peak_rss_bytes,
        terminated,
        reaped,
        process_absent_after_reap,
    }
}

fn probe_from_terminal<F>(
    probe_id: &'static str,
    expected: &'static str,
    observation: SupervisedObservation,
    predicate: F,
) -> ProbeReport
where
    F: FnOnce(&RequestObservation) -> bool,
{
    let SupervisedObservation {
        outcome,
        worker_pid,
        duration_millis,
        peak_rss_bytes,
        terminated,
        reaped,
        process_absent_after_reap,
    } = observation;
    let controlled_terminal = terminated
        || matches!(
            outcome,
            RequestObservation::WorkerExited | RequestObservation::AcknowledgedCancelled { .. }
        );
    let passed = predicate(&outcome) && controlled_terminal && reaped && process_absent_after_reap;
    ProbeReport {
        probe_id,
        expected,
        observed: format!("{outcome:?}"),
        passed,
        worker_pid,
        duration_millis,
        peak_rss_bytes,
        terminated,
        reaped,
        process_absent_after_reap,
    }
}

fn behavioral_acceptance(cases: &[CaseReport], probes: &[ProbeReport]) -> bool {
    let mandatory = cases.iter().filter(|case| case.mandatory).count();
    mandatory > 0
        && cases
            .iter()
            .filter(|case| case.mandatory)
            .all(case_evidence_matches)
        && probes.iter().all(|probe| probe.passed)
}

fn case_evidence_matches(case: &CaseReport) -> bool {
    case.observed.as_deref() == Some(case.expected.as_str())
        && case.engine.as_deref() == Some(fforager_javascript::ENGINE_ID)
        && case.solver_implementation.as_deref() == Some(fforager_javascript::SOLVER_IMPLEMENTATION)
        && case
            .prepared_sha256
            .as_deref()
            .is_some_and(|hash| is_lower_hex(hash, 64))
        && case.candidate_count.is_some_and(|count| count > 0)
        && case.successful_candidates.is_some_and(|count| count > 0)
        && case.fresh_context == Some(true)
}

fn counterfactual_report(cases: &[CaseReport], probes: &[ProbeReport]) -> CounterfactualReport {
    let Some(case) = cases.iter().find(|case| case.matched) else {
        return CounterfactualReport {
            case_id: cases
                .first()
                .map_or_else(|| "no-case".to_owned(), |case| case.case_id.clone()),
            mutation: "replace the required expected output while leaving the producer unchanged",
            original_oracle_passed: false,
            mutated_oracle_rejected: false,
        };
    };
    let mut mutated_cases = cases.to_vec();
    let Some(mutated) = mutated_cases
        .iter_mut()
        .find(|candidate| candidate.case_id == case.case_id)
    else {
        return CounterfactualReport {
            case_id: case.case_id.clone(),
            mutation: "replace the required expected output while leaving the producer unchanged",
            original_oracle_passed: behavioral_acceptance(cases, probes),
            mutated_oracle_rejected: false,
        };
    };
    mutated.expected.push_str("-mutated");
    mutated.matched = case_evidence_matches(mutated);
    CounterfactualReport {
        case_id: case.case_id.clone(),
        mutation: "replace the required expected output while leaving the producer unchanged",
        original_oracle_passed: behavioral_acceptance(cases, probes),
        mutated_oracle_rejected: !behavioral_acceptance(&mutated_cases, probes),
    }
}

fn write_report(path: &Path, report: &CorpusReport) -> Result<(), SolverError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(fforager_javascript::io_error)?;
    }
    let payload = serde_json::to_vec_pretty(report)
        .map_err(|error| SolverError::InvalidResult(error.to_string()))?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, payload).map_err(fforager_javascript::io_error)?;
    fs::rename(&temporary, path).map_err(fforager_javascript::io_error)
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn fresh_probe_grant() -> String {
    let nonce = SUPERVISOR_NONCE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let material = format!("{}:{nanos}:{nonce}", std::process::id());
    format!("capability-wp006-probe-{}", sha256_hex(material.as_bytes()))
}

#[cfg(windows)]
fn configure_quiet_process(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn configure_quiet_process(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_in_player_bytes_match_manifest_hashes() {
        let fixture_root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/youtube-challenge-v1");
        let manifest_bytes =
            fs::read(fixture_root.join("manifest.json")).expect("checked-in manifest must exist");
        let manifest: CorpusManifest =
            serde_json::from_slice(&manifest_bytes).expect("checked-in manifest must decode");
        validate_manifest(&manifest).expect("checked-in manifest must validate");

        for case in manifest.cases {
            let player_path = fixture_root.join("players").join(&case.program_reference);
            let player_bytes = fs::read(&player_path).unwrap_or_else(|error| {
                panic!(
                    "checked-in player {} must exist: {error}",
                    player_path.display()
                )
            });
            assert_eq!(
                sha256_hex(&player_bytes),
                case.program_sha256,
                "checked-in player {} must retain its manifest-pinned bytes",
                player_path.display()
            );
        }
    }

    #[test]
    fn oracle_counterfactual_rejects_mutated_expected_output() {
        let report = counterfactual_report(
            &[CaseReport {
                case_id: "case-1".to_owned(),
                player_id: "player".to_owned(),
                challenge: ChallengeKind::N,
                mandatory: true,
                program_reference: "player.js".to_owned(),
                program_sha256: "a".repeat(64),
                input: "input".to_owned(),
                extractor_version: "1.0.0".to_owned(),
                expected: "correct".to_owned(),
                observed: Some("correct".to_owned()),
                matched: true,
                engine: Some(fforager_javascript::ENGINE_ID.to_owned()),
                solver_implementation: Some(fforager_javascript::SOLVER_IMPLEMENTATION.to_owned()),
                prepared_sha256: Some("b".repeat(64)),
                candidate_count: Some(1),
                successful_candidates: Some(1),
                cache_hit: Some(false),
                fresh_context: Some(true),
                worker_pid: 1,
                duration_millis: 1,
                peak_rss_bytes: 1,
                error: None,
            }],
            &[],
        );
        assert!(report.original_oracle_passed);
        assert!(report.mutated_oracle_rejected);
    }

    #[test]
    fn full_response_correlation_rejects_forged_tuple_fields() {
        let request = request_envelope(
            1,
            "test",
            "case",
            "player.js",
            WorkerInput {
                challenge: ChallengeKind::N,
                value: "input".to_owned(),
                script_sha256: "a".repeat(64),
                extractor_version: "1.0.0".to_owned(),
            },
            default_policy(),
            PLAYER_CAPABILITY_GRANT,
        )
        .expect("valid request");
        let mut response = request.clone();
        response.header.producer_id =
            ProducerId::new("producer_fforager_js_worker").expect("valid producer");
        response.header.sequence = 2;
        response.message = JavaScriptWorkerMessage::Failed {
            error: ErrorCode::InvalidInput,
            message: "expected".to_owned(),
        };
        assert!(validate_response_correlation(&request, &response).is_ok());

        let mut forged_producer = response.clone();
        forged_producer.header.producer_id =
            ProducerId::new("producer_attacker").expect("valid producer");
        assert!(validate_response_correlation(&request, &forged_producer).is_err());

        let mut forged_sequence = response.clone();
        forged_sequence.header.sequence = 1;
        assert!(validate_response_correlation(&request, &forged_sequence).is_err());

        let mut forged_provenance = response;
        forged_provenance.provenance.capability_grant_id = "capability-forged".to_owned();
        assert!(validate_response_correlation(&request, &forged_provenance).is_err());
    }

    #[test]
    fn request_namespace_prevents_restart_identity_reuse() {
        let input = WorkerInput {
            challenge: ChallengeKind::N,
            value: "input".to_owned(),
            script_sha256: "a".repeat(64),
            extractor_version: "1.0.0".to_owned(),
        };
        let first = request_envelope(
            1,
            "worker1",
            "case",
            "player.js",
            input.clone(),
            default_policy(),
            PLAYER_CAPABILITY_GRANT,
        )
        .expect("valid request");
        let second = request_envelope(
            1,
            "worker2",
            "case",
            "player.js",
            input,
            default_policy(),
            PLAYER_CAPABILITY_GRANT,
        )
        .expect("valid request");
        assert_ne!(first.header.request_id, second.header.request_id);
    }

    #[test]
    fn manifest_rejects_non_mandatory_or_executable_oracle() {
        let manifest = CorpusManifest {
            schema_id: "ff.youtube-challenge-corpus@1".to_owned(),
            corpus_id: "corpus".to_owned(),
            version: "1".to_owned(),
            solver_implementation: fforager_javascript::SOLVER_IMPLEMENTATION.to_owned(),
            engine: fforager_javascript::ENGINE_ID.to_owned(),
            oracle: OracleIdentity {
                kind: "external".to_owned(),
                source: "source".to_owned(),
                source_commit: "commit".to_owned(),
                execution_required: true,
            },
            cases: Vec::new(),
        };
        assert!(validate_manifest(&manifest).is_err());
    }
}
