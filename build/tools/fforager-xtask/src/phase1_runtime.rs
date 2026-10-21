//! Bounded Phase 1 staged-artifact proof. This is not a general scenario engine.

use super::{
    ArchitecturePolicy, BTreeMap, BTreeSet, COMMAND_CAPTURE_SEQUENCE, Check, Child, Command,
    Deserialize, Digest, Duration, File, Instant, Ordering, OsStr, Path, PathBuf, Read,
    RuntimeTruthResult, Serialize, Sha256, SourceState, Stdio, SystemTime, TOOL_COMMAND_TIMEOUT,
    UNIX_EPOCH, Value, Write, active_packet_id, cleanup_timed_out_child, command_output,
    command_output_bytes_with_timeout, command_output_with_timeout, configure_quiet_process, env,
    fs, io, read_toml, require_relative_contained, run_command_with_proof_class, safe_relative,
    sha256_file, slash, source_state, terminate_child_process_tree, thread,
    validate_built_runtime_artifact, wait_for_child, walk_files,
};
use fforager_contracts::{ArchiveLimits, ArchiveRecord};
use redb::{
    ReadOnlyDatabase, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition,
};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, atomic::AtomicBool};

mod watcher_mutant;
#[cfg(windows)]
mod windows_pause;

const SCHEMA: &str = "ff.runtime-proof@2";
const PACKET: &str = "WP-FF-021-first-runnable-production-slice-v1";
const REQUIRED_CASES: &[&str] = &[
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
];

#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct Proof {
    schema_id: String,
    completion_claim: String,
    authorization_reference: String,
    source_commit: String,
    artifacts: Vec<Artifact>,
    fixture: Fixture,
    scenarios: Vec<String>,
    timeout_seconds: u64,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Artifact {
    package: String,
    binary: String,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Fixture {
    source: String,
    sha256: String,
    bytes: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RuntimeEvidence {
    pub(super) schema_id: String,
    pub(super) source: SourceState,
    pub(super) originating_gate_args: Vec<String>,
    proof: Proof,
    artifacts: Vec<EvidenceFile>,
    cases: Vec<CaseEvidence>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceFile {
    path: String,
    sha256: String,
    bytes: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaseEvidence {
    name: String,
    files: Vec<EvidenceFile>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InvocationReceipt {
    schema: String,
    launcher_pid: u32,
    exit_code: Option<i32>,
    elapsed_ms: u64,
    stdout_sha256: String,
    stderr_sha256: String,
}

fn evidence_file(root: &Path, path: &Path) -> Result<EvidenceFile, String> {
    let metadata = fs::symlink_metadata(path).map_err(failure)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > 128 * 1024 * 1024
    {
        return Err(failure("invalid evidence file boundary"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(failure("reparse evidence is forbidden"));
        }
    }
    let canonical_root = root.canonicalize().map_err(failure)?;
    if !path
        .canonicalize()
        .map_err(failure)?
        .starts_with(&canonical_root)
    {
        return Err(failure("evidence escaped repository"));
    }
    Ok(EvidenceFile {
        path: slash(path.strip_prefix(root).map_err(failure)?),
        sha256: sha256_file(path)?,
        bytes: metadata.len(),
    })
}

pub(super) fn validate_evidence(
    evidence: &RuntimeEvidence,
    root: Option<&Path>,
) -> Result<(), String> {
    validate_evidence_packet(evidence, root)?;
    validate_evidence_shape(evidence)?;
    for case in &evidence.cases {
        let marker = format!("/{}/", case.name);
        if case.files.iter().any(|file| {
            !file
                .path
                .starts_with(".fforager-artifacts/runtime-proof/phase1-")
                || !file.path.contains(&marker)
        }) {
            return Err(failure("scenario evidence is outside its owned stage"));
        }
    }
    for file in evidence
        .artifacts
        .iter()
        .chain(evidence.cases.iter().flat_map(|case| &case.files))
    {
        if !safe_relative(&file.path)
            || !file.path.starts_with(".fforager-artifacts/")
            || file.sha256.len() != 64
            || !file.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            || file.bytes > 128 * 1024 * 1024
        {
            return Err(failure("malformed actual runtime artifact identity"));
        }
        if let Some(root) = root {
            let actual = evidence_file(root, &root.join(&file.path))?;
            if actual.sha256 != file.sha256 || actual.bytes != file.bytes {
                return Err(failure("runtime evidence bytes changed"));
            }
        }
    }
    if evidence.cases.iter().any(|case| {
        case.files.is_empty()
            || case.files.len() > 256
            || !case.files.iter().any(|file| file.path.ends_with(".stdout"))
            || !case.files.iter().any(|file| file.path.ends_with(".stderr"))
            || !case
                .files
                .iter()
                .any(|file| file.path.ends_with(".status.json"))
    }) {
        return Err(failure(
            "runtime case has no actual bounded invocation captures",
        ));
    }
    if let Some(root) = root {
        for case in &evidence.cases {
            for file in case
                .files
                .iter()
                .filter(|file| file.path.ends_with(".status.json"))
            {
                let receipt: InvocationReceipt =
                    serde_json::from_str(&bounded_capture(&root.join(&file.path))?)
                        .map_err(failure)?;
                let stem = file
                    .path
                    .strip_suffix(".status.json")
                    .ok_or_else(|| failure("status path suffix"))?;
                if receipt.schema != "ff.production-invocation@1"
                    || receipt.launcher_pid == 0
                    || receipt.exit_code.is_none()
                    || receipt.elapsed_ms
                        > evidence
                            .proof
                            .timeout_seconds
                            .saturating_mul(1000)
                            .saturating_add(1000)
                {
                    return Err(failure("invalid observed production invocation status"));
                }
                for (extension, digest) in [
                    ("stdout", &receipt.stdout_sha256),
                    ("stderr", &receipt.stderr_sha256),
                ] {
                    if !case.files.iter().any(|capture| {
                        capture.path == format!("{stem}.{extension}") && capture.sha256 == *digest
                    }) {
                        return Err(failure(
                            "invocation status does not bind its same-run actual captures",
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn validate_evidence_packet(evidence: &RuntimeEvidence, root: Option<&Path>) -> Result<(), String> {
    if let Some(root) = root {
        if active_packet_id(root)?.as_deref() != Some(PACKET) {
            return Err(failure("runtime evidence packet is not active"));
        }
        let packet: Value = serde_json::from_slice(
            &fs::read(root.join(format!(".GOV/work_packets/{PACKET}/packet.json")))
                .map_err(failure)?,
        )
        .map_err(failure)?;
        if packet.pointer("/extensions/runtime_proof")
            != Some(&serde_json::to_value(&evidence.proof).map_err(failure)?)
        {
            return Err(failure(
                "runtime receipt differs from the actual frozen packet declaration",
            ));
        }
    }
    Ok(())
}

fn validate_evidence_shape(evidence: &RuntimeEvidence) -> Result<(), String> {
    let cases = evidence
        .cases
        .iter()
        .map(|case| case.name.as_str())
        .collect::<BTreeSet<_>>();
    if evidence.schema_id != "ff.staged-runtime-evidence@1"
        || evidence.proof.schema_id != SCHEMA
        || evidence.cases.len() != REQUIRED_CASES.len()
        || cases != REQUIRED_CASES.iter().copied().collect()
        || evidence.artifacts.len() != 3
        || evidence.source.dirty
    {
        return Err(failure("incomplete staged runtime evidence"));
    }
    let invocation = evidence
        .originating_gate_args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    if ![
        vec!["verify-pr", "--evidence-from-taskboard"],
        vec!["runtime-truth-check", "--evidence-from-taskboard"],
        vec!["watcher-check"],
    ]
    .contains(&invocation)
    {
        return Err(failure(
            "runtime evidence omits the actual canonical batch invocation",
        ));
    }
    let artifact_names = evidence
        .artifacts
        .iter()
        .filter_map(|file| Path::new(&file.path).file_name().and_then(OsStr::to_str))
        .collect::<BTreeSet<_>>();
    let expected_names = [
        executable("fforager"),
        executable("fforager-worker"),
        executable("fforager-watcher"),
    ];
    if artifact_names != expected_names.iter().map(String::as_str).collect()
        || evidence.artifacts.iter().any(|file| {
            !file
                .path
                .starts_with(".fforager-artifacts/cargo-target/release/")
        })
    {
        return Err(failure("original release artifact identities missing"));
    }
    Ok(())
}

fn runtime_check(id: &str, class: &'static str, boundary: &str, detail: String) -> Check {
    Check {
        id: id.into(),
        status: "PASS",
        proof_class: class,
        concrete_input: detail.clone(),
        executed_boundary: boundary.into(),
        expected_result:
            "actual production oracle accepts the declared boundary and rejects its named negative"
                .into(),
        observed_result: detail.clone(),
        skipped_semantic_dependencies: Vec::new(),
        detail,
    }
}

fn failure(detail: impl std::fmt::Display) -> String {
    format!("FF-RUNTIME-E-PHASE1: {detail}")
}

pub(super) fn execute(
    root: &Path,
    packet: &Value,
    value: &Value,
    packet_id: &str,
) -> Result<RuntimeTruthResult, String> {
    let proof: Proof = serde_json::from_value(value.clone()).map_err(failure)?;
    validate_contract(root, packet, &proof, value, packet_id)?;
    let mut checks = Vec::new();
    let mut build_args = vec![
        "build",
        "--manifest-path",
        "build/Cargo.toml",
        "--locked",
        "--release",
    ];
    for artifact in &proof.artifacts {
        build_args.extend(["-p", artifact.package.as_str()]);
    }
    run_command_with_proof_class(
        root,
        "phase1-release-build",
        "external_process",
        "one locked release build of all three production executables",
        "cargo",
        &build_args,
        &mut checks,
    )?;
    let fixture = fs::read(root.join(&proof.fixture.source)).map_err(failure)?;
    let directory = root.join(".fforager-artifacts/runtime-proof").join(format!(
        "phase1-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(failure)?
            .as_nanos()
    ));
    fs::create_dir_all(&directory).map_err(failure)?;
    let mut built = Vec::new();
    for artifact in &proof.artifacts {
        let path = root
            .join(".fforager-artifacts/cargo-target/release")
            .join(executable(&artifact.binary));
        validate_built_runtime_artifact(root, &path)?;
        built.push((artifact.binary.clone(), path.clone(), sha256_file(&path)?));
    }
    let case_evidence = execute_cases(root, &directory, &fixture, &proof, &built, &mut checks)?;
    checks.push(runtime_check("phase1-watcher-boundaries", "runtime_fault",
        "real staged watcher degradation and owned-process fault boundaries",
        "all declared missing, corrupt, incompatible, crash, hung and storage-failure cases executed".into()));
    let evidence = RuntimeEvidence {
        schema_id: "ff.staged-runtime-evidence@1".into(),
        source: source_state(root)?,
        originating_gate_args: env::args().skip(1).collect(),
        artifacts: built
            .iter()
            .map(|(_, path, _)| evidence_file(root, path))
            .collect::<Result<Vec<_>, _>>()?,
        cases: case_evidence,
        proof,
    };
    validate_evidence(&evidence, Some(root))?;
    let receipt = directory.join("runtime-evidence.json");
    fs::write(
        &receipt,
        serde_json::to_vec_pretty(&evidence).map_err(failure)?,
    )
    .map_err(failure)?;
    Ok(RuntimeTruthResult {
        checks,
        proof_classes: vec!["counterfactual".into(), "external_process".into(), "production_runtime".into(), "runtime_fault".into(), "state_effect".into()],
        limitations: vec!["Only the frozen direct-HTTP Windows/NTFS slice and named fault cases are proven; no replacement parity or release claim.".into()],
        artifacts: vec![slash(&directory), slash(receipt.strip_prefix(root).map_err(failure)?)],
    })
}

fn execute_cases(
    root: &Path,
    directory: &Path,
    fixture: &[u8],
    proof: &Proof,
    built: &[(String, PathBuf, String)],
    checks: &mut Vec<Check>,
) -> Result<Vec<CaseEvidence>, String> {
    let mut case_evidence = Vec::new();
    let mut case_failures = Vec::new();
    for case in &proof.scenarios {
        let stage = directory.join(case);
        fs::create_dir_all(&stage).map_err(failure)?;
        for (name, path, digest) in built {
            let target = stage.join(executable(name));
            fs::copy(path, &target).map_err(failure)?;
            if sha256_file(&target)? != *digest {
                return Err(failure("staged executable digest changed"));
            }
        }
        if let Err(error) = execute_case(root, &stage, case, fixture, proof) {
            case_failures.push(format!("scenario={case}; stage={}; {error}", slash(&stage)));
            continue;
        }
        let files = walk_files(&stage)?;
        if files.len() > 256 {
            return Err(failure("runtime case evidence file bound exceeded"));
        }
        case_evidence.push(CaseEvidence {
            name: case.clone(),
            files: files
                .iter()
                .map(|path| evidence_file(root, path))
                .collect::<Result<Vec<_>, _>>()?,
        });
        let class = match case.as_str() {
            "state-counterfactual" => "counterfactual",
            "durable-prefixes" => "state_effect",
            _ => "production_runtime",
        };
        let identities = built
            .iter()
            .map(|(name, _, digest)| format!("{name}={digest}"))
            .collect::<Vec<_>>()
            .join(";");
        checks.push(runtime_check(
            &format!("phase1-{case}"),
            class,
            "exact staged launcher, worker, watcher and actual HTTP/filesystem/archive boundaries",
            format!(
                "scenario={case}; fixture_sha256={}; artifacts={identities}; stage={}",
                proof.fixture.sha256,
                slash(&stage)
            ),
        ));
    }
    if !case_failures.is_empty() {
        return Err(failure(format!(
            "independent scenario collection failed or blocked: {}",
            case_failures.join(" | ")
        )));
    }
    Ok(case_evidence)
}

fn validate_contract(
    root: &Path,
    packet: &Value,
    proof: &Proof,
    value: &Value,
    id: &str,
) -> Result<(), String> {
    if id != PACKET
        || proof.schema_id != SCHEMA
        || proof.completion_claim != "operator_usable_runtime"
        || proof.authorization_reference.trim().is_empty()
        || proof.source_commit.len() != 40
        || !proof
            .source_commit
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || !(5..=120).contains(&proof.timeout_seconds)
    {
        return Err(failure(
            "invalid bounded Phase 1 contract or transition identity",
        ));
    }
    if packet
        .pointer("/authorization/runtime_proof_transition_reference")
        .and_then(Value::as_str)
        != Some(proof.authorization_reference.as_str())
    {
        return Err(failure(
            "explicit current-candidate proof transition authorization is absent",
        ));
    }
    let observed = proof
        .scenarios
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if observed != REQUIRED_CASES.iter().copied().collect()
        || observed.len() != proof.scenarios.len()
        || proof
            .scenarios
            .iter()
            .map(String::as_str)
            .ne(REQUIRED_CASES.iter().copied())
    {
        return Err(failure(
            "all exact Phase 1 cases are required once in declared execution order",
        ));
    }
    let expected = BTreeSet::from([
        ("fforager-launcher", "fforager"),
        ("fforager-worker", "fforager-worker"),
        ("fforager-watcher", "fforager-watcher"),
    ]);
    let artifacts = proof
        .artifacts
        .iter()
        .map(|artifact| (artifact.package.as_str(), artifact.binary.as_str()))
        .collect::<BTreeSet<_>>();
    if artifacts != expected || proof.artifacts.len() != 3 {
        return Err(failure("exact launcher/worker/watcher artifacts required"));
    }
    let policy: ArchitecturePolicy = read_toml(&root.join("build/architecture-policy.toml"))?;
    if proof.artifacts.iter().any(|artifact| {
        !policy
            .members
            .iter()
            .any(|member| member.name == artifact.package && member.shipped && !member.test_only)
    }) {
        return Err(failure(
            "runtime artifact is not a registered shipped member",
        ));
    }
    require_relative_contained(
        root,
        &proof.fixture.source,
        "build/fixtures/phase1-direct-http",
    )?;
    let fixture = root.join(&proof.fixture.source);
    if !(256 * 1024..=4 * 1024 * 1024).contains(&proof.fixture.bytes)
        || fs::metadata(&fixture).map_err(failure)?.len() != proof.fixture.bytes
        || sha256_file(&fixture)? != proof.fixture.sha256
    {
        return Err(failure(
            "committed deterministic media fixture identity/bound mismatch",
        ));
    }
    validate_contract_source(root, proof, value, id)
}

fn validate_contract_source(
    root: &Path,
    proof: &Proof,
    value: &Value,
    id: &str,
) -> Result<(), String> {
    command_output(
        root,
        "git",
        &["merge-base", "--is-ancestor", &proof.source_commit, "HEAD"],
    )?;
    command_output(
        root,
        "git",
        &[
            "diff",
            "--exit-code",
            &proof.source_commit,
            "--",
            "product",
            "build/Cargo.toml",
            "build/Cargo.lock",
            "build/architecture-policy.toml",
            "build/tools/fforager-xtask",
            "build/fixtures/phase1-direct-http",
        ],
    )?;
    if !command_output(
        root,
        "git",
        &[
            "ls-files",
            "--others",
            "--exclude-standard",
            "--",
            "product",
            "build/tools/fforager-xtask",
            "build/fixtures/phase1-direct-http",
        ],
    )?
    .trim()
    .is_empty()
    {
        return Err(failure(
            "uncommitted runtime/proof inputs are not a frozen candidate",
        ));
    }
    let path = format!(".GOV/work_packets/{id}/packet.json");
    let commits = command_output(
        root,
        "git",
        &["log", "--reverse", "--format=%H", "HEAD", "--", &path],
    )?;
    let mut history = Vec::new();
    for commit in commits.lines() {
        let previous: Value = serde_json::from_str(&command_output(
            root,
            "git",
            &["show", &format!("{commit}:{path}")],
        )?)
        .map_err(failure)?;
        if previous
            .pointer("/extensions/runtime_proof/schema_id")
            .and_then(Value::as_str)
            == Some(SCHEMA)
        {
            history.push(
                previous
                    .pointer("/extensions/runtime_proof")
                    .cloned()
                    .ok_or_else(|| failure("historical v2 declaration missing"))?,
            );
        }
    }
    validate_candidate_history(proof, value, &history)
}

fn contract_digest(proof: &Proof) -> Result<String, String> {
    let mut contract = serde_json::to_value(proof).map_err(failure)?;
    let object = contract
        .as_object_mut()
        .ok_or_else(|| failure("typed proof is not an object"))?;
    object.remove("source_commit");
    object.remove("authorization_reference");
    Ok(sha256_bytes(
        &serde_json::to_vec(&contract).map_err(failure)?,
    ))
}

fn validate_candidate_history(
    proof: &Proof,
    value: &Value,
    history: &[Value],
) -> Result<(), String> {
    let first: Proof = serde_json::from_value(
        history
            .first()
            .ok_or_else(|| failure("no committed v2 declaration"))?
            .clone(),
    )
    .map_err(failure)?;
    if contract_digest(&first)? != contract_digest(proof)? {
        return Err(failure(
            "candidate reference cannot change the immutable fixture/scenario/artifact/bound contract; a separate authorized contract transition is required",
        ));
    }
    let frozen = history.iter().find(|item| {
        item.get("authorization_reference").and_then(Value::as_str)
            == Some(proof.authorization_reference.as_str())
    });
    if frozen != Some(value) {
        return Err(failure(
            "candidate proof differs from the first committed full declaration for its exact reference",
        ));
    }
    Ok(())
}

fn executable(name: &str) -> String {
    format!("{name}{}", env::consts::EXE_SUFFIX)
}

struct Origin {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    body_released: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Origin {
    #[cfg(test)]
    fn start(stage: &Path, fixture: &[u8], negative: bool, slow: bool) -> Result<Self, String> {
        Self::start_controlled(stage, fixture, negative, slow, false, 30)
    }

    fn start_controlled(
        stage: &Path,
        fixture: &[u8],
        negative: bool,
        slow: bool,
        hold_body: bool,
        timeout: u64,
    ) -> Result<Self, String> {
        let mut diagnostics =
            File::create(stage.join("origin-diagnostics.jsonl")).map_err(failure)?;
        let listener = TcpListener::bind("127.0.0.1:0").map_err(failure)?;
        listener.set_nonblocking(true).map_err(failure)?;
        let address = listener.local_addr().map_err(failure)?;
        let stop = Arc::new(AtomicBool::new(false));
        let running = Arc::clone(&stop);
        let body_released = Arc::new(AtomicBool::new(!hold_body));
        let release = Arc::clone(&body_released);
        let bytes = fixture.to_vec();
        let thread = thread::spawn(move || {
            let mut failures = 0_u32;
            while !running.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        if let Err(error) = serve(
                            &mut stream,
                            &bytes,
                            negative,
                            slow,
                            &running,
                            &release,
                            timeout,
                        ) && failures < 32
                        {
                            failures += 1;
                            let event = serde_json::json!({"schema":"ff.origin-diagnostic@1", "sequence":failures, "kind":format!("{:?}",error.kind()), "os_error":error.raw_os_error(), "detail":error.to_string().chars().take(1024).collect::<String>()});
                            if let Err(record_error) = writeln!(diagnostics, "{event}") {
                                eprintln!("origin diagnostic write failed: {record_error}");
                            }
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => {
                        eprintln!("owned origin accept failed: {error}");
                        break;
                    }
                }
            }
        });
        Ok(Self {
            address,
            stop,
            body_released,
            thread: Some(thread),
        })
    }

    fn release_body(&self) {
        self.body_released.store(true, Ordering::Release);
    }
}

impl Drop for Origin {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(
    stream: &mut TcpStream,
    bytes: &[u8],
    negative: bool,
    slow: bool,
    stop: &AtomicBool,
    released: &AtomicBool,
    timeout: u64,
) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_millis(200)))?;
    stream.set_write_timeout(Some(Duration::from_millis(200)))?;
    let mut request = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    while request.len() < 8192 && !request.ends_with(b"\r\n\r\n") {
        if stop.load(Ordering::Acquire) || Instant::now() >= deadline {
            return Err(io::Error::other("origin request deadline"));
        }
        let mut byte = [0];
        if stream.read(&mut byte)? == 0 {
            return Ok(());
        }
        request.push(byte[0]);
    }
    if !request.starts_with(b"GET /media HTTP/1.1\r\n") {
        return Err(io::Error::other("unexpected origin request"));
    }
    if negative {
        return stream.write_all(
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
    }
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: audio/wav\r\nConnection: close\r\n\r\n",
        bytes.len()
    )?;
    let body_deadline = Instant::now() + Duration::from_secs(timeout);
    for (index, chunk) in bytes.chunks(8192).enumerate() {
        while index > 0 && !released.load(Ordering::Acquire) {
            if stop.load(Ordering::Acquire) || Instant::now() >= body_deadline {
                return Err(io::Error::other("owned origin body gate deadline"));
            }
            thread::sleep(Duration::from_millis(2));
        }
        if stop.load(Ordering::Acquire) {
            break;
        }
        stream.write_all(chunk)?;
        if slow {
            thread::sleep(Duration::from_millis(50));
        }
    }
    Ok(())
}

struct Invocation {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
    started: Instant,
}

impl Invocation {
    fn start(stage: &Path, origin: &Origin, timeout: u64) -> Result<Self, String> {
        let stdout = stage.join(format!(
            "run-{}.stdout",
            COMMAND_CAPTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let stderr = stdout.with_extension("stderr");
        let mut command = Command::new(stage.join(executable("fforager")));
        command
            .current_dir(stage)
            .arg("fetch")
            .arg(format!("http://{}/media", origin.address))
            .arg("--output")
            .arg(stage.join("media.wav"))
            .arg("--state-dir")
            .arg(stage.join("state"))
            .arg("--max-bytes")
            .arg("4194304")
            .arg("--timeout-ms")
            .arg((timeout * 1000).to_string())
            .arg("--cancel-file")
            .arg(stage.join("cancel"))
            .arg("--allow-loopback")
            .arg(origin.address.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::from(File::create(&stdout).map_err(failure)?))
            .stderr(Stdio::from(File::create(&stderr).map_err(failure)?));
        configure_quiet_process(&mut command);
        let child = command.spawn().map_err(failure)?;
        Ok(Self {
            child,
            stdout,
            stderr,
            started: Instant::now(),
        })
    }

    fn finish(mut self, timeout: u64) -> Result<(i32, String, String), String> {
        let remaining = Duration::from_secs(timeout).saturating_sub(self.started.elapsed());
        let status = wait_for_child(&mut self.child, "staged-fforager", &[], remaining)?;
        let stdout = bounded_capture(&self.stdout)?;
        let stderr = bounded_capture(&self.stderr)?;
        let receipt = InvocationReceipt {
            schema: "ff.production-invocation@1".into(),
            launcher_pid: self.child.id(),
            exit_code: status.code(),
            elapsed_ms: u64::try_from(self.started.elapsed().as_millis()).map_err(failure)?,
            stdout_sha256: sha256_file(&self.stdout)?,
            stderr_sha256: sha256_file(&self.stderr)?,
        };
        fs::write(
            self.stdout.with_extension("status.json"),
            serde_json::to_vec(&receipt).map_err(failure)?,
        )
        .map_err(failure)?;
        Ok((status.code().unwrap_or(-1), stdout, stderr))
    }
}

impl Drop for Invocation {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let result = terminate_child_process_tree(&mut self.child);
            let _ = cleanup_timed_out_child(&mut self.child, &result);
        }
    }
}

fn bounded_capture(path: &Path) -> Result<String, String> {
    if fs::metadata(path).map_err(failure)?.len() > 1024 * 1024 {
        return Err(failure("runtime output exceeded capture bound"));
    }
    fs::read_to_string(path).map_err(failure)
}

fn archive_table_name(version: Option<u64>) -> Result<&'static str, String> {
    match version {
        Some(1) => Ok("ff_archive_records_v1"),
        Some(2) => Ok("ff_archive_records_v2"),
        _ => Err(failure("archive store version is missing or unsupported")),
    }
}

fn active_archive_table(transaction: &redb::ReadTransaction) -> Result<&'static str, String> {
    let metadata = transaction
        .open_table(TableDefinition::<&str, u64>::new("ff_archive_meta_u64_v1"))
        .map_err(failure)?;
    archive_table_name(
        metadata
            .get("store_version")
            .map_err(failure)?
            .map(|value| value.value()),
    )
}

fn archive_record(stage: &Path) -> Result<ArchiveRecord, String> {
    let database = ReadOnlyDatabase::open(stage.join("state/archive.redb")).map_err(failure)?;
    let transaction = database.begin_read().map_err(failure)?;
    let table = transaction
        .open_table(TableDefinition::<&str, &[u8]>::new(active_archive_table(
            &transaction,
        )?))
        .map_err(failure)?;
    let mut rows = table.iter().map_err(failure)?;
    let (key, record) = rows
        .next()
        .ok_or_else(|| failure("archive contains no committed row"))?
        .map_err(failure)?;
    let record = ArchiveRecord::from_wire_bytes(record.value(), ArchiveLimits::default())
        .map_err(failure)?;
    if key.value()
        != record
            .key
            .canonical_key(ArchiveLimits::default())
            .map_err(failure)?
    {
        return Err(failure(
            "archive physical key differs from decoded record identity",
        ));
    }
    if rows.next().is_some() {
        return Err(failure("isolated single-job archive contains extra rows"));
    }
    Ok(record)
}

fn archive_is_empty(stage: &Path) -> Result<bool, String> {
    let path = stage.join("state/archive.redb");
    match fs::metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(failure(error)),
        Ok(_) => {}
    }
    let database = ReadOnlyDatabase::open(path).map_err(failure)?;
    let transaction = database.begin_read().map_err(failure)?;
    active_archive_table(&transaction)?;
    for name in ["ff_archive_records_v1", "ff_archive_records_v2"] {
        let table = transaction
            .open_table(TableDefinition::<&str, &[u8]>::new(name))
            .map_err(failure)?;
        if !table.is_empty().map_err(failure)? {
            return Ok(false);
        }
    }
    let leases = transaction
        .open_table(TableDefinition::<&str, &[u8]>::new("ff_archive_leases_v1"))
        .map_err(failure)?;
    leases.is_empty().map_err(failure)
}

fn same_output_target(expected: &Path, recorded: &str) -> Result<bool, String> {
    let recorded = Path::new(recorded);
    if !recorded.is_absolute() {
        return Ok(false);
    }
    // Compare existing targets; this is not a path-confinement authorization check.
    Ok(recorded.canonicalize().map_err(failure)? == expected.canonicalize().map_err(failure)?)
}

fn success(stage: &Path, observation: &(i32, String, String), proof: &Proof) -> Result<(), String> {
    if observation.0 != 0 {
        return Err(failure(format!(
            "expected successful exit, got {}; stderr={}",
            observation.0, observation.2
        )));
    }
    let value: Value = serde_json::from_str(observation.1.trim()).map_err(failure)?;
    let output = stage.join("media.wav");
    let record = archive_record(stage)?;
    if value.get("status").and_then(Value::as_str) != Some("success")
        || value.get("sha256").and_then(Value::as_str) != Some(proof.fixture.sha256.as_str())
        || value.get("bytes").and_then(Value::as_u64) != Some(proof.fixture.bytes)
        || sha256_file(&output)? != proof.fixture.sha256
        || fs::metadata(&output).map_err(failure)?.len() != proof.fixture.bytes
        || record.output.artifact_digest != proof.fixture.sha256
        || record.output.artifact_size_bytes != proof.fixture.bytes
        || record.commit_sequence != 1
        || !same_output_target(&output, &record.output.final_output_identity)?
        || value.get("archive_sequence").and_then(Value::as_u64) != Some(record.commit_sequence)
        || value.get("job_id").and_then(Value::as_str) != Some(record.provenance.job_id.as_str())
    {
        return Err(failure(
            "actual media bytes, independently decoded archive and result identity disagree",
        ));
    }
    let journal = bounded_capture(&journal_path(stage)?)?;
    let mut previous = "0".repeat(64);
    let mut last = None;
    for (index, line) in journal.lines().enumerate() {
        let value: Value = serde_json::from_str(line).map_err(failure)?;
        let body = value
            .get("body")
            .ok_or_else(|| failure("actual journal body missing"))?;
        let checksum = value
            .get("checksum")
            .and_then(Value::as_str)
            .ok_or_else(|| failure("actual journal checksum missing"))?;
        let decoded: JournalBodyOracle = serde_json::from_value(body.clone()).map_err(failure)?;
        if decoded.sequence != index as u64 + 1
            || decoded.previous != previous
            || journal_checksum(body)? != checksum
        {
            return Err(failure("actual journal sequence/checksum mismatch"));
        }
        previous = checksum.into();
        last = Some(decoded);
    }
    let last = last.ok_or_else(|| failure("successful job omitted durable journal"))?;
    let commit = last
        .commit
        .ok_or_else(|| failure("successful archived journal omitted commit"))?;
    commit.validate(ArchiveLimits::default()).map_err(failure)?;
    if last.phase != "archived"
        || commit.output != record.output
        || commit.provenance != record.provenance
        || commit.lease.key != record.key
        || commit.success_event != record.success_event
        || commit.lease.token != record.claim_lease_token
        || commit.lease.generation != record.claim_lease_generation
    {
        return Err(failure(
            "actual journal and independently decoded archive disagree",
        ));
    }
    Ok(())
}

fn execute_case(
    root: &Path,
    stage: &Path,
    case: &str,
    fixture: &[u8],
    proof: &Proof,
) -> Result<(), String> {
    if !cfg!(windows) {
        return Err(failure(
            "Phase 1 storage contract currently supports the approved Windows/NTFS profile only",
        ));
    }
    let watcher = stage.join(executable("fforager-watcher"));
    prepare_watcher_fault(root, stage, case, &watcher)?;
    let slow = matches!(
        case,
        "external-cancellation" | "crash-restart" | "watcher-crash" | "watcher-hung"
    );
    let held = matches!(case, "crash-restart" | "watcher-crash" | "watcher-hung");
    let origin = Origin::start_controlled(
        stage,
        fixture,
        case == "transport-failure",
        slow,
        held,
        proof.timeout_seconds,
    )?;
    let launched = SystemTime::now();
    let invocation = Invocation::start(stage, &origin, proof.timeout_seconds)?;
    match case {
        "external-cancellation" => {
            wait_until(proof.timeout_seconds, || Ok(journal_path(stage).is_ok()))?;
            fs::write(stage.join("cancel"), b"cancel\n").map_err(failure)?;
            let observation = invocation.finish(proof.timeout_seconds)?;
            if observation.0 != 130
                || !observation.2.contains("cancelled")
                || stage.join("media.wav").exists()
                || !archive_is_empty(stage)?
            {
                return Err(failure(
                    "external cancellation did not return typed non-success without final/archive success",
                ));
            }
            return Ok(());
        }
        "crash-restart" => {
            return execute_crash_restart(stage, &origin, invocation, launched, &watcher, proof);
        }
        "watcher-crash" | "watcher-hung" => {
            return execute_watcher_fault(
                stage, case, &watcher, &origin, launched, invocation, proof,
            );
        }
        _ => {}
    }
    let observation = invocation.finish(proof.timeout_seconds)?;
    if case == "transport-failure" {
        if observation.0 == 0
            || !observation.2.contains("transport_failed")
            || stage.join("media.wav").exists()
            || !archive_is_empty(stage)?
        {
            return Err(failure(
                "actual HTTP503 did not produce typed failure without output/archive success",
            ));
        }
        return Ok(());
    }
    success(stage, &observation, proof)?;
    verify_watcher_outcome(stage, case, &observation)?;
    if case == "state-counterfactual" {
        let record = archive_record(stage)?;
        let output = stage.join("media.wav");
        let mut corrupted = fs::read(&output).map_err(failure)?;
        let first = corrupted
            .first_mut()
            .ok_or_else(|| failure("empty mutation target"))?;
        *first ^= 1;
        fs::write(&output, corrupted).map_err(failure)?;
        let observation = Invocation::start(stage, &origin, proof.timeout_seconds)?
            .finish(proof.timeout_seconds)?;
        if observation.0 == 0
            || success(stage, &observation, proof).is_ok()
            || !observation.2.contains("RECOVERY-HASH")
            || archive_record(stage)? != record
        {
            return Err(failure(
                "declaration-preserving physical state corruption was not rejected by the actual restarted product and independent oracle",
            ));
        }
    }
    if case == "durable-prefixes" {
        durable_prefixes(stage, &origin, proof)?;
    }
    Ok(())
}

fn verify_watcher_outcome(
    stage: &Path,
    case: &str,
    observation: &(i32, String, String),
) -> Result<(), String> {
    if case.starts_with("watcher-") && !observation.2.contains("diagnostics_degraded") {
        return Err(failure(
            "watcher fault did not expose independent diagnostic degradation",
        ));
    }
    if case == "watcher-incompatible" {
        let files = walk_files(&stage.join("state/diagnostics"))?;
        let mut observed = false;
        for path in files
            .iter()
            .filter(|path| path.file_name() == Some(OsStr::new("ready.json")))
        {
            let ready: Value = serde_json::from_str(&bounded_capture(path)?).map_err(failure)?;
            observed |= ready.get("schema").and_then(Value::as_str) == Some("ff.watcher-ready@9");
        }
        if !observed || !observation.2.contains("watcher_ready_timeout") {
            return Err(failure(
                "actual incompatible watcher wire was not observed and rejected",
            ));
        }
    }
    if matches!(case, "watcher-missing" | "watcher-corrupt")
        && !observation.2.contains("watcher_missing_or_unavailable")
    {
        return Err(failure(
            "missing/corrupt watcher did not exercise actual executable startup rejection",
        ));
    }
    if case == "watcher-storage-failure" && !observation.2.contains("diagnostic_state_unavailable")
    {
        return Err(failure(
            "watcher storage failure did not exercise diagnostic directory rejection",
        ));
    }
    Ok(())
}

fn prepare_watcher_fault(
    root: &Path,
    stage: &Path,
    case: &str,
    watcher: &Path,
) -> Result<(), String> {
    match case {
        "watcher-missing" => fs::remove_file(watcher).map_err(failure)?,
        "watcher-corrupt" => {
            fs::write(watcher, b"deliberately invalid executable image").map_err(failure)?;
        }
        "watcher-incompatible" => {
            let original = sha256_file(watcher)?;
            let variant = watcher_mutant::build(root, stage)?;
            let variant_hash = sha256_file(&variant)?;
            if variant_hash == original {
                return Err(failure(
                    "source-mutated watcher equals the original artifact",
                ));
            }
            fs::copy(variant, watcher).map_err(failure)?;
            if sha256_file(watcher)? != variant_hash {
                return Err(failure("source-mutated watcher staging identity changed"));
            }
            fs::write(stage.join("watcher-original-artifact.sha256"), original).map_err(failure)?;
        }
        "watcher-storage-failure" => {
            fs::create_dir_all(stage.join("state")).map_err(failure)?;
            fs::write(
                stage.join("state/diagnostics"),
                b"diagnostic directory deliberately unavailable",
            )
            .map_err(failure)?;
        }
        _ => {}
    }
    Ok(())
}

fn execute_crash_restart(
    stage: &Path,
    origin: &Origin,
    mut invocation: Invocation,
    launched: SystemTime,
    watcher: &Path,
    proof: &Proof,
) -> Result<(), String> {
    let (ready, control) = ready_actor(stage, proof.timeout_seconds)?;
    #[cfg(windows)]
    let watcher_actor = {
        let pid = ready
            .get("watcher_pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
            .ok_or_else(|| failure("missing watcher actor identity"))?;
        let actor = windows_pause::OwnedWatcher::open(pid, watcher, launched).map_err(failure)?;
        verify_actor_parent(stage, actor.pid(), invocation.child.id(), watcher)?;
        actor
    };
    wait_until(proof.timeout_seconds, || Ok(journal_path(stage).is_ok()))?;
    let worker = ready
        .get("worker_pid")
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
        .ok_or_else(|| failure("missing worker actor identity"))?;
    kill_worker(stage, &mut invocation, worker, launched)?;
    let observation = invocation.finish(proof.timeout_seconds)?;
    if observation.0 == 0 {
        return Err(failure("uncommanded worker death was reported successful"));
    }
    wait_until(3, || {
        watcher_log_contains(&control, "worker_exit_without_shutdown_receipt")
    })?;
    fs::write(control.join("stop"), b"stop\n").map_err(failure)?;
    #[cfg(windows)]
    watcher_actor
        .wait_exit(Duration::from_secs(2))
        .map_err(failure)?;
    origin.release_body();
    let first =
        Invocation::start(stage, origin, proof.timeout_seconds)?.finish(proof.timeout_seconds)?;
    success(stage, &first, proof)?;
    let record = archive_record(stage)?;
    let second =
        Invocation::start(stage, origin, proof.timeout_seconds)?.finish(proof.timeout_seconds)?;
    success(stage, &second, proof)?;
    if archive_record(stage)? != record {
        return Err(failure("second recovery altered the committed archive"));
    }
    Ok(())
}

fn execute_watcher_fault(
    stage: &Path,
    case: &str,
    watcher: &Path,
    origin: &Origin,
    launched: SystemTime,
    mut invocation: Invocation,
    proof: &Proof,
) -> Result<(), String> {
    let (ready, _) = ready_actor(stage, proof.timeout_seconds)?;
    let pid = ready
        .get("watcher_pid")
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
        .ok_or_else(|| failure("missing watcher actor identity"))?;
    #[cfg(windows)]
    {
        let mut actor =
            windows_pause::OwnedWatcher::open(pid, watcher, launched).map_err(failure)?;
        verify_actor_parent(stage, actor.pid(), invocation.child.id(), watcher)?;
        if invocation.child.try_wait().map_err(failure)?.is_some() {
            return Err(failure(
                "owned launcher exited before watcher fault actuation",
            ));
        }
        if case == "watcher-hung" {
            actor.pause().map_err(failure)?;
            thread::sleep(Duration::from_secs(1));
            actor.resume().map_err(failure)?;
        } else {
            taskkill(stage, actor.pid())?;
            actor.wait_exit(Duration::from_secs(2)).map_err(failure)?;
        }
    }
    origin.release_body();
    let observation = invocation.finish(proof.timeout_seconds)?;
    success(stage, &observation, proof)?;
    let expected = if case == "watcher-hung" {
        "watcher_heartbeat_stale"
    } else {
        "watcher_failed_after_readiness"
    };
    if !observation.2.contains(expected) {
        return Err(failure(format!("actual watcher fault omitted {expected}")));
    }
    Ok(())
}

fn wait_until(
    timeout: u64,
    mut condition: impl FnMut() -> Result<bool, String>,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(timeout);
    loop {
        if condition()? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(failure("runtime actor/state observation deadline exceeded"));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn ready_actor(stage: &Path, timeout: u64) -> Result<(Value, PathBuf), String> {
    let mut result = None;
    wait_until(timeout, || {
        let directory = stage.join("state/diagnostics");
        let Ok(entries) = fs::read_dir(&directory) else {
            return Ok(false);
        };
        for entry in entries.take(64) {
            let path = entry.map_err(failure)?.path();
            let ready = path.join("ready.json");
            let Ok(metadata) = fs::metadata(&ready) else {
                continue;
            };
            if metadata.len() > 4096 {
                return Err(failure("oversized actor readiness"));
            }
            let Ok(value) = serde_json::from_slice::<Value>(&fs::read(&ready).map_err(failure)?)
            else {
                continue;
            };
            if value.get("schema").and_then(Value::as_str) == Some("ff.watcher-ready@1")
                && value.get("ready").and_then(Value::as_bool) == Some(true)
            {
                result = Some((value, path));
                return Ok(true);
            }
        }
        Ok(false)
    })?;
    result.ok_or_else(|| failure("ready actor absent"))
}

fn journal_path(stage: &Path) -> Result<PathBuf, String> {
    for entry in fs::read_dir(stage.join("state")).map_err(failure)?.take(64) {
        let path = entry.map_err(failure)?.path();
        if path.extension() == Some(OsStr::new("journal"))
            && fs::metadata(&path).map_err(failure)?.len() > 0
        {
            return Ok(path);
        }
    }
    Err(failure("no actual durable journal"))
}

fn watcher_log_contains(control: &Path, needle: &str) -> Result<bool, String> {
    for entry in fs::read_dir(control).map_err(failure)?.take(64) {
        let path = entry.map_err(failure)?.path();
        if path.extension() == Some(OsStr::new("jsonl"))
            && bounded_capture(&path)?.lines().any(|line| {
                serde_json::from_str::<Value>(line)
                    .ok()
                    .is_some_and(|value| value.get("event").and_then(Value::as_str) == Some(needle))
            })
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn taskkill(stage: &Path, pid: u32) -> Result<(), String> {
    let system = env::var_os("SystemRoot").ok_or_else(|| failure("SystemRoot unavailable"))?;
    let executable = PathBuf::from(system).join("System32/taskkill.exe");
    command_output(
        stage,
        executable
            .to_str()
            .ok_or_else(|| failure("system tool path encoding"))?,
        &["/PID", &pid.to_string(), "/F"],
    )?;
    Ok(())
}

fn kill_worker(
    stage: &Path,
    invocation: &mut Invocation,
    pid: u32,
    launched: SystemTime,
) -> Result<(), String> {
    if !matches!(invocation.child.try_wait(), Ok(None)) {
        return Err(failure("owned launcher already exited before worker fault"));
    }
    #[cfg(windows)]
    {
        let image = stage.join(executable_name_worker());
        let actor = windows_pause::OwnedWorker::open(pid, &image, launched).map_err(failure)?;
        verify_actor_parent(stage, actor.pid(), invocation.child.id(), &image)?;
        if invocation.child.try_wait().map_err(failure)?.is_some() {
            return Err(failure(
                "owned launcher exited during worker identity verification",
            ));
        }
        actor.verify_live().map_err(failure)?;
        // The retained process handle preserves this PID's process-object identity
        // through the existing actuator and independent exit acknowledgement.
        taskkill(stage, actor.pid())?;
        actor.wait_exit(Duration::from_secs(2)).map_err(failure)
    }
    #[cfg(not(windows))]
    {
        let _ = (stage, pid, launched);
        Err(failure(
            "retained worker identity requires the approved Windows profile",
        ))
    }
}

fn verify_actor_parent(
    stage: &Path,
    pid: u32,
    expected_parent: u32,
    expected_image: &Path,
) -> Result<(), String> {
    let system = env::var_os("SystemRoot").ok_or_else(|| failure("SystemRoot unavailable"))?;
    let executable = PathBuf::from(system).join("System32/wbem/WMIC.exe");
    let bytes = command_output_bytes_with_timeout(
        stage,
        executable
            .to_str()
            .ok_or_else(|| failure("system tool path encoding"))?,
        &[
            "process",
            "where",
            &format!("ProcessId={pid}"),
            "get",
            "ParentProcessId,ExecutablePath",
            "/value",
        ],
        TOOL_COMMAND_TIMEOUT,
    )?;
    fs::write(stage.join(format!("actor-{pid}-metadata.bin")), &bytes).map_err(failure)?;
    let process = decode_actor_metadata(&bytes)?;
    let mut parent = None;
    let mut image = None;
    for line in process.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("ParentProcessId=") {
            parent = value.parse::<u32>().ok();
        }
        if let Some(value) = line.strip_prefix("ExecutablePath=") {
            image = Some(PathBuf::from(value));
        }
    }
    let expected = expected_image.canonicalize().map_err(failure)?;
    if parent != Some(expected_parent)
        || image.as_ref().and_then(|path| path.canonicalize().ok()) != Some(expected)
    {
        return Err(failure(
            "fault target is not the actual owned launcher's staged child",
        ));
    }
    Ok(())
}

fn decode_actor_metadata(bytes: &[u8]) -> Result<String, String> {
    if bytes.len() > 64 * 1024 {
        return Err(failure("actor metadata exceeds bound"));
    }
    if let Some(payload) = bytes.strip_prefix(&[0xff, 0xfe]) {
        if payload.len() % 2 != 0 {
            return Err(failure("truncated UTF-16 actor metadata"));
        }
        let words = payload
            .chunks_exact(2)
            .map(|word| u16::from_le_bytes([word[0], word[1]]))
            .collect::<Vec<_>>();
        String::from_utf16(&words).map_err(failure)
    } else {
        String::from_utf8(
            bytes
                .strip_prefix(&[0xef, 0xbb, 0xbf])
                .unwrap_or(bytes)
                .to_vec(),
        )
        .map_err(failure)
    }
}

fn executable_name_worker() -> String {
    executable("fforager-worker")
}
fn durable_prefixes(stage: &Path, origin: &Origin, proof: &Proof) -> Result<(), String> {
    let journal = journal_path(stage)?;
    let original = fs::read(&journal).map_err(failure)?;
    if original.len() > 1024 * 1024 {
        return Err(failure("journal exceeds inspection bound"));
    }
    let lines = original
        .split_inclusive(|byte| *byte == b'\n')
        .collect::<Vec<_>>();
    let mut phases = BTreeMap::new();
    let mut end = 0;
    let mut commit = None;
    let mut previous = "0".repeat(64);
    for (index, line) in lines.iter().enumerate() {
        end += line.len();
        let value: Value = serde_json::from_slice(line).map_err(failure)?;
        let body = value
            .get("body")
            .ok_or_else(|| failure("journal body missing"))?;
        let checksum = value
            .get("checksum")
            .and_then(Value::as_str)
            .ok_or_else(|| failure("journal checksum missing"))?;
        if body.get("sequence").and_then(Value::as_u64) != Some(index as u64 + 1)
            || body.get("previous").and_then(Value::as_str) != Some(previous.as_str())
            || journal_checksum(body)? != checksum
        {
            return Err(failure(
                "independent durable journal sequence/checksum reconciliation failed",
            ));
        }
        checksum.clone_into(&mut previous);
        let phase = body
            .get("phase")
            .and_then(Value::as_str)
            .ok_or_else(|| failure("journal phase missing"))?;
        phases.insert(phase.to_owned(), end);
        if phase == "validated" {
            commit = body.get("commit").cloned();
        }
    }
    if phases.keys().map(String::as_str).collect::<BTreeSet<_>>()
        != BTreeSet::from(["prepared", "validated", "placed", "archived"])
    {
        return Err(failure(
            "actual completed job did not acknowledge every implemented durable prefix",
        ));
    }
    let commit = commit.ok_or_else(|| failure("validated durable commit missing"))?;
    let baseline_files = snapshot(&stage.join("state"))?;
    let media = fs::read(stage.join("media.wav")).map_err(failure)?;
    let stage_name = format!(
        ".fforager-{}.part",
        journal
            .file_stem()
            .and_then(OsStr::to_str)
            .ok_or_else(|| failure("journal identity encoding"))?
    );
    for phase in ["prepared", "validated", "placed", "archived"] {
        restore_snapshot(stage, &baseline_files)?;
        fs::write(stage.join("media.wav"), &media).map_err(failure)?;
        let cutoff = *phases
            .get(phase)
            .ok_or_else(|| failure("required durable prefix absent"))?;
        fs::write(&journal, &original[..cutoff]).map_err(failure)?;
        if phase != "archived" {
            rollback_archive(&stage.join("state/archive.redb"), &commit)?;
            if matches!(phase, "prepared" | "validated") {
                fs::rename(stage.join("media.wav"), stage.join(&stage_name)).map_err(failure)?;
            }
        }
        let first = Invocation::start(stage, origin, proof.timeout_seconds)?
            .finish(proof.timeout_seconds)?;
        success(stage, &first, proof)?;
        let record = archive_record(stage)?;
        let second = Invocation::start(stage, origin, proof.timeout_seconds)?
            .finish(proof.timeout_seconds)?;
        success(stage, &second, proof)?;
        if archive_record(stage)? != record {
            return Err(failure(format!(
                "second reconciliation of {phase} was not idempotent"
            )));
        }
    }
    // A real physical journal mutation retains a correct checksum but violates
    // the phase/commit relationship; the restarted product must reject it.
    restore_snapshot(stage, &baseline_files)?;
    fs::write(stage.join("media.wav"), &media).map_err(failure)?;
    let mut malformed: Value = serde_json::from_slice(lines[0]).map_err(failure)?;
    malformed["body"]["commit"] = commit;
    malformed["checksum"] = Value::String(journal_checksum(&malformed["body"])?);
    let mut malformed = serde_json::to_vec(&malformed).map_err(failure)?;
    malformed.push(b'\n');
    fs::write(&journal, malformed).map_err(failure)?;
    let observation =
        Invocation::start(stage, origin, proof.timeout_seconds)?.finish(proof.timeout_seconds)?;
    if observation.0 == 0 || !observation.2.contains("JOURNAL-PHASE") {
        return Err(failure(
            "checksum-valid inconsistent journal phase was accepted",
        ));
    }
    Ok(())
}

fn snapshot(root: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>, String> {
    fn visit(
        root: &Path,
        path: &Path,
        depth: usize,
        files: &mut BTreeMap<PathBuf, Vec<u8>>,
    ) -> Result<(), String> {
        if depth > 8 || files.len() > 256 {
            return Err(failure("runtime state tree exceeds snapshot bounds"));
        }
        for entry in fs::read_dir(path).map_err(failure)? {
            let path = entry.map_err(failure)?.path();
            let metadata = fs::symlink_metadata(&path).map_err(failure)?;
            if metadata.file_type().is_symlink() {
                return Err(failure("runtime state contains a link"));
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if metadata.file_attributes() & 0x400 != 0 {
                    return Err(failure("runtime state contains a reparse point"));
                }
            }
            if metadata.is_dir() {
                visit(root, &path, depth + 1, files)?;
            } else {
                if metadata.len() > 64 * 1024 * 1024 {
                    return Err(failure("state snapshot file exceeds bound"));
                }
                files.insert(
                    path.strip_prefix(root).map_err(failure)?.to_path_buf(),
                    fs::read(path).map_err(failure)?,
                );
            }
        }
        Ok(())
    }
    let mut files = BTreeMap::new();
    visit(root, root, 0, &mut files)?;
    Ok(files)
}

fn restore_snapshot(stage: &Path, files: &BTreeMap<PathBuf, Vec<u8>>) -> Result<(), String> {
    let state_directory = stage.join("state");
    let owned_root = stage.canonicalize().map_err(failure)?;
    let restored_directory = state_directory.canonicalize().map_err(failure)?;
    if restored_directory.parent() != Some(owned_root.as_path())
        || restored_directory.file_name() != Some(OsStr::new("state"))
    {
        return Err(failure(
            "refusing state rollback outside the owned scenario directory",
        ));
    }
    snapshot(&state_directory)?; // Reject links/reparse points before a recursive removal.
    fs::remove_dir_all(&state_directory).map_err(failure)?;
    fs::create_dir(&state_directory).map_err(failure)?;
    for (relative, bytes) in files {
        let path = state_directory.join(relative);
        fs::create_dir_all(
            path.parent()
                .ok_or_else(|| failure("snapshot parent absent"))?,
        )
        .map_err(failure)?;
        fs::write(path, bytes).map_err(failure)?;
    }
    Ok(())
}

fn rollback_archive(path: &Path, commit: &Value) -> Result<(), String> {
    let commit: fforager_contracts::ArchiveCommitRequest =
        serde_json::from_value(commit.clone()).map_err(failure)?;
    commit.validate(ArchiveLimits::default()).map_err(failure)?;
    let key = commit
        .lease
        .key
        .canonical_key(ArchiveLimits::default())
        .map_err(failure)?;
    let lease = commit
        .lease
        .to_wire_bytes(ArchiveLimits::default())
        .map_err(failure)?;
    let database = redb::Database::open(path).map_err(failure)?;
    let mut transaction = database.begin_write().map_err(failure)?;
    transaction
        .set_durability(redb::Durability::Immediate)
        .map_err(failure)?;
    transaction.set_two_phase_commit(true);
    transaction.set_quick_repair(true);
    let active = {
        let metadata = transaction
            .open_table(TableDefinition::<&str, u64>::new("ff_archive_meta_u64_v1"))
            .map_err(failure)?;
        archive_table_name(
            metadata
                .get("store_version")
                .map_err(failure)?
                .map(|value| value.value()),
        )?
    };
    let inactive = if active == "ff_archive_records_v1" {
        "ff_archive_records_v2"
    } else {
        "ff_archive_records_v1"
    };
    if !transaction
        .open_table(TableDefinition::<&str, &[u8]>::new(inactive))
        .map_err(failure)?
        .is_empty()
        .map_err(failure)?
    {
        return Err(failure(
            "isolated prefix baseline contains unexpected inactive records",
        ));
    }
    {
        let mut records = transaction
            .open_table(TableDefinition::<&str, &[u8]>::new(active))
            .map_err(failure)?;
        if records.len().map_err(failure)? != 1 {
            return Err(failure(
                "prefix rollback requires the isolated single committed baseline",
            ));
        }
        let record = {
            let row = records
                .get(key.as_str())
                .map_err(failure)?
                .ok_or_else(|| failure("prefix baseline row missing"))?;
            ArchiveRecord::from_wire_bytes(row.value(), ArchiveLimits::default())
                .map_err(failure)?
        };
        if record.commit_sequence != 1
            || record.key != commit.lease.key
            || record.output != commit.output
            || record.provenance != commit.provenance
            || record.claim_lease_token != commit.lease.token
            || record.claim_lease_generation != commit.lease.generation
        {
            return Err(failure(
                "prefix rollback baseline differs from the real isolated commit",
            ));
        }
        records.remove(key.as_str()).map_err(failure)?;
    }
    rollback_archive_sequence(&transaction)?;
    {
        let mut leases = transaction
            .open_table(TableDefinition::<&str, &[u8]>::new("ff_archive_leases_v1"))
            .map_err(failure)?;
        if !leases.is_empty().map_err(failure)? {
            return Err(failure(
                "prefix baseline unexpectedly retains active leases",
            ));
        }
        leases
            .insert(key.as_str(), lease.as_slice())
            .map_err(failure)?;
    }
    transaction.commit().map_err(failure)
}

fn rollback_archive_sequence(transaction: &redb::WriteTransaction) -> Result<(), String> {
    {
        let mut metadata = transaction
            .open_table(TableDefinition::<&str, u64>::new("ff_archive_meta_u64_v1"))
            .map_err(failure)?;
        if metadata
            .get("commit_sequence")
            .map_err(failure)?
            .map(|value| value.value())
            != Some(1)
        {
            return Err(failure(
                "prefix rollback refuses an unexpected archive sequence",
            ));
        }
        metadata.insert("commit_sequence", 0).map_err(failure)?;
    }
    Ok(())
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalBodyOracle {
    schema: u32,
    sequence: u64,
    previous: String,
    identity: String,
    phase: String,
    commit: Option<fforager_contracts::ArchiveCommitRequest>,
}

fn journal_checksum(value: &Value) -> Result<String, String> {
    let body: JournalBodyOracle = serde_json::from_value(value.clone()).map_err(failure)?;
    Ok(sha256_bytes(&serde_json::to_vec(&body).map_err(failure)?))
}

#[cfg(test)]
mod tests {
    use super::super::{
        GateReport, GateReportArtifact, InputState, RUNTIME_GATE, aggregate_executed_proof_class,
        executed_proof_classes, invocation, pass_with_class, repo_root,
        validate_gate_report_artifact, with_isolated_fixture_root,
    };
    use super::*;

    fn component_evidence() -> RuntimeEvidence {
        let empty = sha256_bytes(b"");
        let file = |path: String| EvidenceFile {
            path,
            sha256: empty.clone(),
            bytes: 0,
        };
        RuntimeEvidence {
            schema_id: "ff.staged-runtime-evidence@1".into(),
            originating_gate_args: vec!["runtime-truth-check".into(), "--evidence-from-taskboard".into()],
            source: SourceState { git_commit: "1".repeat(40), dirty: false, dirty_paths: Vec::new(), content_fingerprint: "2".repeat(64) },
            proof: Proof { schema_id: SCHEMA.into(), completion_claim: "operator_usable_runtime".into(), authorization_reference: "component-boundary-only".into(),
                source_commit: "1".repeat(40), artifacts: [Artifact { package: "fforager-launcher".into(), binary: "fforager".into() },
                    Artifact { package: "fforager-worker".into(), binary: "fforager-worker".into() }, Artifact { package: "fforager-watcher".into(), binary: "fforager-watcher".into() }].into(),
                fixture: Fixture { source: "build/fixtures/phase1-direct-http/media.wav".into(), sha256: "3".repeat(64), bytes: 524_332 },
                scenarios: REQUIRED_CASES.iter().map(|name| (*name).into()).collect(), timeout_seconds: 30 },
            artifacts: ["fforager", "fforager-worker", "fforager-watcher"].iter().map(|name| file(format!(".fforager-artifacts/cargo-target/release/{}", executable(name)))).collect(),
            cases: REQUIRED_CASES.iter().map(|name| CaseEvidence { name: (*name).into(), files: ["stdout", "stderr", "status.json"].iter()
                .map(|extension| file(format!(".fforager-artifacts/runtime-proof/phase1-component/{name}/run.{extension}"))).collect() }).collect(),
        }
    }

    fn component_report(evidence: &RuntimeEvidence) -> Value {
        let mut checks = REQUIRED_CASES
            .iter()
            .map(|name| {
                runtime_check(
                    &format!("phase1-{name}"),
                    match *name {
                        "durable-prefixes" => "state_effect",
                        "state-counterfactual" => "counterfactual",
                        _ => "production_runtime",
                    },
                    "component schema shape only",
                    "component evidence, not production execution".into(),
                )
            })
            .collect::<Vec<_>>();
        checks.push(runtime_check(
            "phase1-watcher-boundaries",
            "runtime_fault",
            "component schema shape only",
            "component evidence".into(),
        ));
        checks.push(pass_with_class(
            "phase1-release-build",
            "external_process",
            "component build shape only",
            "component evidence",
        ));
        let classes = executed_proof_classes(&checks, &[]);
        let report = GateReport {
            schema_id: "ff.gate-report@1",
            schema_version: "1.0.0",
            gate_id: RUNTIME_GATE.into(),
            gate_version: 1,
            status: "PASS",
            exit_code: 0,
            source: evidence.source.clone(),
            invocation: invocation(&[
                "runtime-truth-check".into(),
                "--evidence-from-taskboard".into(),
            ]),
            inputs: vec![InputState {
                path: "component-input".into(),
                git_blob: "1".repeat(40),
            }],
            checks,
            rules: Vec::new(),
            fixtures: Vec::new(),
            declared_supported_proof_classes: classes.clone(),
            aggregate_executed_proof_class: aggregate_executed_proof_class(&classes),
            executed_proof_classes: classes,
            proof_limitations: vec!["Component shape control only; no runtime claim.".into()],
            artifacts: Vec::new(),
        };
        let mut value = serde_json::to_value(report).expect("component report serialization");
        value["schema_id"] = "ff.gate-report@2".into();
        value["schema_version"] = "2.0.0".into();
        value["runtime_evidence"] =
            serde_json::to_value(evidence).expect("component evidence serialization");
        value
    }

    #[test]
    fn version_two_report_rejects_missing_mutated_and_promoted_evidence() {
        let evidence = component_evidence();
        let valid = component_report(&evidence);
        let decoded: GateReportArtifact =
            serde_json::from_value(valid.clone()).expect("strict component control");
        validate_gate_report_artifact(&decoded)
            .expect("valid component shape, not a production proof");
        let rejects = |value: Value| {
            if let Ok(report) = serde_json::from_value::<GateReportArtifact>(value) {
                assert!(validate_gate_report_artifact(&report).is_err());
            }
        };
        let mut missing = valid.clone();
        missing
            .as_object_mut()
            .expect("report object")
            .remove("runtime_evidence");
        rejects(missing);
        for mutation in [
            "null",
            "unknown-source",
            "unknown-proof",
            "duplicate-case",
            "missing-case",
            "missing-artifact",
            "duplicate-artifact",
            "missing-status",
            "source-mismatch",
            "skipped-runtime",
            "wrong-class",
            "v1-promotion",
        ] {
            let mut changed = valid.clone();
            match mutation {
                "null" => changed["runtime_evidence"] = Value::Null,
                "unknown-source" => changed["runtime_evidence"]["source"]["unknown"] = true.into(),
                "unknown-proof" => changed["runtime_evidence"]["proof"]["unknown"] = true.into(),
                "duplicate-case" => {
                    changed["runtime_evidence"]["cases"][1] =
                        changed["runtime_evidence"]["cases"][0].clone();
                }
                "missing-case" => {
                    changed["runtime_evidence"]["cases"]
                        .as_array_mut()
                        .expect("case array")
                        .pop();
                }
                "missing-artifact" => {
                    changed["runtime_evidence"]["artifacts"]
                        .as_array_mut()
                        .expect("artifact array")
                        .pop();
                }
                "duplicate-artifact" => {
                    changed["runtime_evidence"]["artifacts"][1] =
                        changed["runtime_evidence"]["artifacts"][0].clone();
                }
                "missing-status" => {
                    changed["runtime_evidence"]["cases"][0]["files"]
                        .as_array_mut()
                        .expect("files")
                        .pop();
                }
                "source-mismatch" => {
                    changed["runtime_evidence"]["source"]["git_commit"] = "4".repeat(40).into();
                }
                "skipped-runtime" => {
                    changed["checks"][0]["skipped_semantic_dependencies"] =
                        serde_json::json!(["production entrypoint skipped"]);
                }
                "wrong-class" => changed["checks"][0]["proof_class"] = "structural".into(),
                "v1-promotion" => {
                    changed["schema_id"] = "ff.gate-report@1".into();
                    changed["schema_version"] = "1.0.0".into();
                    changed
                        .as_object_mut()
                        .expect("report object")
                        .remove("runtime_evidence");
                }
                _ => unreachable!(),
            }
            rejects(changed);
        }
        let mut derived = valid.clone();
        derived["gate_id"] = "FF-GATE-WATCHER-001".into();
        derived["invocation"] =
            serde_json::to_value(invocation(&["watcher-check".into()])).expect("child invocation");
        derived["derived_from"] = valid["invocation"].clone();
        let report: GateReportArtifact =
            serde_json::from_value(derived.clone()).expect("strict derived report");
        validate_gate_report_artifact(&report).expect("actual parent invocation retained");
        let mut missing_origin = derived.clone();
        missing_origin
            .as_object_mut()
            .expect("report object")
            .remove("derived_from");
        rejects(missing_origin);
        derived["derived_from"] = derived["invocation"].clone();
        rejects(derived);
    }

    #[test]
    fn version_two_receipt_rereads_actual_bytes_and_declared_proof() {
        let first = component_evidence().proof;
        let first_value = serde_json::to_value(&first).expect("first candidate");
        let mut next_value = first_value.clone();
        next_value["source_commit"] = "6".repeat(40).into();
        let reused: Proof =
            serde_json::from_value(next_value.clone()).expect("reused reference shape");
        assert!(
            validate_candidate_history(&reused, &next_value, std::slice::from_ref(&first_value))
                .is_err()
        );
        next_value["authorization_reference"] = "component-source-candidate-2".into();
        let next: Proof = serde_json::from_value(next_value.clone()).expect("next candidate shape");
        assert!(
            validate_candidate_history(&next, &next_value, std::slice::from_ref(&first_value))
                .is_err()
        );
        validate_candidate_history(
            &next,
            &next_value,
            &[first_value.clone(), next_value.clone()],
        )
        .expect("explicit source-only successor preserves first contract");
        for field in ["fixture", "scenarios", "artifacts", "timeout_seconds"] {
            let mut changed = next_value.clone();
            match field {
                "fixture" => changed["fixture"]["sha256"] = "7".repeat(64).into(),
                "scenarios" => changed["scenarios"]
                    .as_array_mut()
                    .expect("cases")
                    .reverse(),
                "artifacts" => changed["artifacts"]
                    .as_array_mut()
                    .expect("artifacts")
                    .reverse(),
                "timeout_seconds" => changed["timeout_seconds"] = 60.into(),
                _ => unreachable!(),
            }
            let proof: Proof =
                serde_json::from_value(changed.clone()).expect("changed successor shape");
            assert!(
                validate_candidate_history(
                    &proof,
                    &changed,
                    &[first_value.clone(), changed.clone()]
                )
                .is_err()
            );
        }
        let canonical = repo_root().expect("repository root");
        with_isolated_fixture_root(&canonical, "phase1-runtime-receipt", |root| {
            let mut evidence = component_evidence();
            let packet = root.join(format!(".GOV/work_packets/{PACKET}/packet.json"));
            fs::create_dir_all(packet.parent().ok_or("packet parent")?).map_err(failure)?;
            fs::create_dir_all(root.join(".GOV/taskboard")).map_err(failure)?;
            fs::write(root.join(".GOV/taskboard/taskboard.yaml"), format!("current_focus:\n  active_wp_id: {PACKET}\n  statement: component-boundary-only\n")).map_err(failure)?;
            fs::write(&packet, serde_json::to_vec(&serde_json::json!({"extensions":{"runtime_proof":evidence.proof}})).map_err(failure)?).map_err(failure)?;
            for file in evidence.artifacts.iter().chain(evidence.cases.iter().flat_map(|case| &case.files)) {
                let path = root.join(&file.path); fs::create_dir_all(path.parent().ok_or("file parent")?).map_err(failure)?; fs::write(path, b"").map_err(failure)?;
            }
            for case in &mut evidence.cases {
                for file in case.files.iter_mut().filter(|file| file.path.ends_with(".status.json")) {
                    let receipt = InvocationReceipt { schema: "ff.production-invocation@1".into(), launcher_pid: 1, exit_code: Some(0), elapsed_ms: 1,
                        stdout_sha256: sha256_bytes(b""), stderr_sha256: sha256_bytes(b"") };
                    let path = root.join(&file.path);
                    fs::write(&path, serde_json::to_vec(&receipt).map_err(failure)?).map_err(failure)?;
                    *file = evidence_file(root, &path)?;
                }
            }
            validate_evidence(&evidence, Some(root))?;
            evidence.proof.fixture.sha256 = "5".repeat(64);
            if validate_evidence(&evidence, Some(root)).is_ok() { return Err("changed declaration was accepted".into()); }
            evidence.proof.fixture.sha256 = "3".repeat(64);
            let status_path = root.join(&evidence.cases[0].files[2].path);
            let original_status = fs::read(&status_path).map_err(failure)?;
            for bad in [b"".as_slice(), br#"{"schema":"unknown"}"#.as_slice()] {
                fs::write(&status_path, bad).map_err(failure)?;
                evidence.cases[0].files[2] = evidence_file(root, &status_path)?;
                if validate_evidence(&evidence, Some(root)).is_ok() { return Err("invalid actual invocation status accepted".into()); }
            }
            fs::write(&status_path, original_status).map_err(failure)?;
            evidence.cases[0].files[2] = evidence_file(root, &status_path)?;
            fs::write(root.join(&evidence.cases[0].files[0].path), b"changed actual bytes").map_err(failure)?;
            if validate_evidence(&evidence, Some(root)).is_ok() { return Err("changed captured bytes were accepted".into()); }
            Ok(())
        }).expect("actual byte/declaration rejection controls");
    }

    #[test]
    fn actor_metadata_decodes_utf16_and_rejects_malformed_bytes() {
        let expected =
            "ExecutablePath=C:\\owned\\caf\u{e9}\\fforager-watcher.exe\r\nParentProcessId=42\r\n";
        let mut utf16 = vec![0xff, 0xfe];
        for word in expected.encode_utf16() {
            utf16.extend_from_slice(&word.to_le_bytes());
        }
        assert_eq!(decode_actor_metadata(&utf16).expect("BOM UTF16"), expected);
        assert_eq!(
            decode_actor_metadata(expected.as_bytes()).expect("UTF8"),
            expected
        );
        for invalid in [
            vec![0xff],
            vec![0xff, 0xfe, 0x41],
            vec![0xff, 0xfe, 0x00, 0xd8],
            vec![0xff; 65537],
        ] {
            assert!(decode_actor_metadata(&invalid).is_err());
        }
    }

    #[test]
    fn origin_accepts_delayed_request_and_returns_exact_body() {
        let canonical = repo_root().expect("repository root");
        with_isolated_fixture_root(&canonical, "phase1-delayed-origin", |stage| {
            let origin = Origin::start(stage, b"exact controlled media bytes", false, false)?;
            let mut client = TcpStream::connect(origin.address).map_err(failure)?;
            client
                .set_read_timeout(Some(Duration::from_secs(2)))
                .map_err(failure)?;
            thread::sleep(Duration::from_millis(50));
            client
                .write_all(
                    b"GET /media HTTP/1.1\r\nHost: owned-origin\r\nConnection: close\r\n\r\n",
                )
                .map_err(failure)?;
            let mut response = Vec::new();
            client.read_to_end(&mut response).map_err(failure)?;
            let split = response
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .ok_or("origin response header missing")?
                + 4;
            if &response[split..] != b"exact controlled media bytes" {
                return Err("origin body differs from real controlled source".into());
            }
            drop(origin);
            let payload = vec![17_u8; 16384];
            let origin = Origin::start_controlled(stage, &payload, false, false, true, 2)?;
            let mut client = TcpStream::connect(origin.address).map_err(failure)?;
            client
                .set_read_timeout(Some(Duration::from_millis(100)))
                .map_err(failure)?;
            client
                .write_all(b"GET /media HTTP/1.1\r\nHost: owned-origin\r\n\r\n")
                .map_err(failure)?;
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") && header.len() < 4096 {
                let mut byte = [0];
                client.read_exact(&mut byte).map_err(failure)?;
                header.push(byte[0]);
            }
            let mut body = vec![0; 8192];
            client.read_exact(&mut body).map_err(failure)?;
            let error = client
                .read(&mut [0])
                .expect_err("body remains held before fault acknowledgment");
            if !matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) {
                return Err(failure(error));
            }
            origin.release_body();
            client
                .set_read_timeout(Some(Duration::from_secs(2)))
                .map_err(failure)?;
            client.read_to_end(&mut body).map_err(failure)?;
            if body != payload {
                return Err("released body differs from controlled fixture".into());
            }
            Ok(())
        })
        .expect("delayed real TCP request");
    }

    #[test]
    fn output_identity_requires_the_same_existing_target() {
        let canonical = repo_root().expect("repository root");
        with_isolated_fixture_root(&canonical, "phase1-output-identity", |stage| {
            fs::create_dir_all(stage.join("identity")).map_err(failure)?;
            let output = stage.join("identity/output.wav");
            let other = stage.join("identity/other.wav");
            fs::write(&output, b"same physical content").map_err(failure)?;
            fs::write(&other, b"same physical content").map_err(failure)?;
            let recorded = output.canonicalize().map_err(failure)?;
            let alias = format!("{}/./output.wav", slash(&stage.join("identity")));
            if alias == output.to_string_lossy() || !same_output_target(&output, &alias)? {
                return Err("textually different existing alias was not reconciled".into());
            }
            if !same_output_target(&output, recorded.to_str().ok_or("nonunicode test path")?)?
                || same_output_target(&output, other.to_str().ok_or("nonunicode test path")?)?
                || same_output_target(&output, "identity/output.wav")?
                || same_output_target(&output, &stage.join("absent.wav").to_string_lossy()).is_ok()
            {
                return Err(
                    "output identity accepted a different, relative or missing target".into(),
                );
            }
            Ok(())
        })
        .expect("existing output target identity");
    }

    #[test]
    fn archive_oracle_uses_metadata_and_rejects_invalid_rows_and_versions() {
        let canonical = repo_root().expect("repository root");
        with_isolated_fixture_root(&canonical, "phase1-active-archive", |stage| {
            fs::create_dir_all(stage.join("state")).map_err(failure)?;
            let path = stage.join("state/archive.redb");
            for version in [1_u64, 2, 99] {
                {
                    let database = redb::Database::create(&path).map_err(failure)?;
                    let transaction = database.begin_write().map_err(failure)?;
                    {
                        let mut metadata = transaction
                            .open_table(TableDefinition::<&str, u64>::new("ff_archive_meta_u64_v1"))
                            .map_err(failure)?;
                        metadata.insert("store_version", version).map_err(failure)?;
                    }
                    for name in [
                        "ff_archive_records_v1",
                        "ff_archive_records_v2",
                        "ff_archive_leases_v1",
                    ] {
                        transaction
                            .open_table(TableDefinition::<&str, &[u8]>::new(name))
                            .map_err(failure)?;
                    }
                    transaction.commit().map_err(failure)?;
                }
                if version == 99 {
                    if archive_is_empty(stage).is_ok() || archive_record(stage).is_ok() {
                        return Err("unsupported archive version accepted".into());
                    }
                    continue;
                }
                if !archive_is_empty(stage)? {
                    return Err("new archive was not empty".into());
                }
                {
                    let database = redb::Database::open(&path).map_err(failure)?;
                    let transaction = database.begin_write().map_err(failure)?;
                    transaction
                        .open_table(TableDefinition::<&str, &[u8]>::new(archive_table_name(
                            Some(version),
                        )?))
                        .map_err(failure)?
                        .insert("actual-active-key", b"invalid-record".as_slice())
                        .map_err(failure)?;
                    transaction.commit().map_err(failure)?;
                }
                if archive_is_empty(stage)? {
                    return Err("active record incorrectly reported absent".into());
                }
                let error =
                    archive_record(stage).expect_err("corrupt active record must fail decoding");
                if error.contains("no committed row") {
                    return Err("reader ignored metadata-active row".into());
                }
                fs::remove_file(&path).map_err(failure)?;
            }
            Ok(())
        })
        .expect("active archive schema boundary");
    }
}
